//! The virtualized Reader view: only units near the viewport are laid out (within a
//! per-frame budget), heights live in the Fenwick index, and the scroll position is an
//! anchor so height changes above it never move the visible content.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ContentMask, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, Pixels, ScrollWheelEvent, Styled, Window, canvas, point, px,
    size,
};
use mdt_markdown::{BlockKind, BlockModel, LayoutUnit};
use mdt_text::{BaseStyle, Brush, CacheKey, FontRegistry, LayoutCache, layout_paragraph};
use parley::LayoutContext;

use super::block::{self, Ctx, Item, UnitLayout, brush};
use super::diagrams::Diagrams;
use super::heights::{Anchor, Heights};
use super::images::Images;
use super::math::MathView;
use crate::theme::Theme;

/// Layout work allowed per frame beyond the visible units.
pub const FRAME_BUDGET: Duration = Duration::from_millis(2);
/// Character count of the column's maximum width.
pub const COLUMN_CHARS: usize = 72;
/// The same with the `wide_reader` setting on: half as wide again.
pub const WIDE_COLUMN_CHARS: usize = 108;
const SIDE_PAD: f32 = 32.0;
const NARROW_PAD: f32 = 16.0;
const TOP_PAD: f32 = 28.0;

mod selection;
pub(crate) use selection::block_texts;
pub use selection::{Direction, Pos, Selection};

pub struct Fonts(pub Rc<RefCell<FontRegistry>>);

impl gpui::Global for Fonts {}

/// Shared fonts for all windows; registers the bundled fonts with GPUI once.
pub fn fonts(cx: &mut App) -> Rc<RefCell<FontRegistry>> {
    if cx.try_global::<Fonts>().is_none() {
        let reg = FontRegistry::new(cx.text_system()).expect("bundled fonts");
        cx.set_global(Fonts(Rc::new(RefCell::new(reg))));
    }
    cx.global::<Fonts>().0.clone()
}

pub struct Reader {
    pub model: BlockModel,
    pub heights: Heights,
    pub anchor: Anchor,
    pub zoom: f32,
    cache: LayoutCache<Rc<UnitLayout>>,
    /// Which units hold a measured (not estimated) height for the current width/zoom.
    measured: Vec<bool>,
    width: f32,
    viewport_h: f32,
    lcx: LayoutContext<Brush>,
    /// The column's maximum width for (zoom, characters).
    column_cache: Option<(f32, usize, f32)>,
    /// The `wide_reader` setting: the column is `WIDE_COLUMN_CHARS` wide.
    pub wide: bool,
    /// The page top-level tables may widen into, from `set_page`.
    page: Option<(f32, f32)>,
    /// Horizontal scroll offsets of wide blocks, by (unit, region).
    pub hscroll: HashMap<(usize, usize), f32>,
    pub selection: Option<Selection>,
    /// Find matches to highlight, by unit: (selectable text index, byte range, current).
    pub found: HashMap<usize, Vec<Found>>,
    /// Last scroll or right-edge hover, for the scroll indicator.
    pub last_activity: Option<Instant>,
    last_pointer: Option<(f32, f32)>,
    last_version: Option<u64>,
    pub highlights: mdt_highlight::Cache,
    pending_highlights: Vec<(String, String)>,
    highlights_inflight: std::collections::HashSet<mdt_highlight::CacheKey>,
    highlight_epoch: u64,
    pub math: MathView,
    pub images: Images,
    pub diagrams: Diagrams,
    /// Remote-image placeholders of the last frame in window coordinates, with their URLs.
    loads: Vec<(Bounds<Pixels>, String)>,
    /// Math error and link areas of the last frame in window coordinates, with their
    /// messages and links.
    hovers: Vec<(Bounds<Pixels>, String, Option<block::Link>)>,
    hovered: Option<usize>,
    /// Window position of the first unit's top-left corner when scrolled to the top.
    pub origin: (f32, f32),
}

/// Where each unit of `old` is in `new` (`None` when it is gone): units before and after the
/// edited span keep their place, shifted by the units added or removed. An edited span of
/// the same length maps one to one (an equation being typed in); otherwise it maps to
/// nothing.
fn unit_lineage(old: &[LayoutUnit], new: &[LayoutUnit]) -> impl Fn(usize) -> Option<usize> + use<> {
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(a, b)| a.hash == b.hash)
        .count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a.hash == b.hash)
        .count();
    let (old_end, new_end, len) = (old.len() - suffix, new.len() - suffix, old.len());
    let in_place = old_end == new_end;
    move |i| {
        if i < prefix || (in_place && i < old_end) {
            Some(i)
        } else if (old_end..len).contains(&i) {
            Some(i - old_end + new_end)
        } else {
            None
        }
    }
}

/// Where the column sits in a window of a given width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    pub x: f32,
    pub width: f32,
    /// The page a top-level table may widen into (the window less side padding):
    /// its x relative to the column (zero or less) and its width.
    pub page_x: f32,
    pub page_w: f32,
}

impl Reader {
    pub fn new(model: BlockModel, zoom: f32) -> Self {
        let n = model.units.len();
        Self {
            model,
            heights: Heights::new(vec![0.0; n]),
            anchor: Anchor::default(),
            zoom,
            cache: LayoutCache::default(),
            measured: vec![false; n],
            width: 0.0,
            viewport_h: 0.0,
            lcx: LayoutContext::new(),
            column_cache: None,
            wide: false,
            page: None,
            hscroll: HashMap::new(),
            selection: None,
            found: HashMap::new(),
            last_activity: None,
            last_pointer: None,
            last_version: None,
            highlights: mdt_highlight::Cache::default(),
            pending_highlights: Vec::new(),
            highlights_inflight: Default::default(),
            highlight_epoch: 0,
            math: MathView::default(),
            images: Images::default(),
            diagrams: Diagrams::default(),
            loads: Vec::new(),
            hovers: Vec::new(),
            hovered: None,
            origin: (0.0, 0.0),
        }
    }

    /// Replaces the model (reparse, reload), keeping the anchor on the same unit index.
    pub fn set_model(&mut self, model: BlockModel) {
        let follow = unit_lineage(&self.model.units, &model.units);
        self.math.follow_units(&follow);
        self.diagrams.follow_units(&follow);
        self.diagrams.retain_model(&model);
        self.model = model;
        self.hscroll.clear();
        self.selection = None;
        self.estimate_all();
    }

    /// All heights back to estimates for the current width and zoom, nothing measured;
    /// the anchor keeps the view in place.
    fn estimate_all(&mut self) {
        let est: Vec<f32> = self
            .model
            .units
            .iter()
            .map(|u| block::estimate_height(u, self.width, self.zoom))
            .collect();
        self.heights = Heights::new(est);
        self.measured = vec![false; self.model.units.len()];
        self.anchor.clamp(&self.heights);
    }

    /// The column for a window width: at most `COLUMN_CHARS` (or `WIDE_COLUMN_CHARS`)
    /// characters of body text wide, centered, with padding that shrinks at narrow widths.
    pub fn column(&mut self, fonts: &mut FontRegistry, window_w: f32) -> Column {
        let chars = if self.wide {
            WIDE_COLUMN_CHARS
        } else {
            COLUMN_CHARS
        };
        let max = match self.column_cache {
            Some((zoom, c, max)) if zoom == self.zoom && c == chars => max,
            _ => {
                let base = BaseStyle {
                    size: block::BODY_SIZE * self.zoom,
                    line_height: block::LINE_HEIGHT,
                    mono: false,
                    bold: false,
                    brush: brush::TEXT,
                };
                let sample = "0".repeat(chars);
                let max =
                    layout_paragraph(fonts, &mut self.lcx, &sample, base, &[], &[], f32::INFINITY)
                        .width;
                self.column_cache = Some((self.zoom, chars, max));
                max
            }
        };
        let pad = if window_w >= max + 2.0 * SIDE_PAD * self.zoom {
            SIDE_PAD
        } else {
            NARROW_PAD
        } * self.zoom;
        let width = (window_w - 2.0 * pad).min(max).max(1.0);
        let x = ((window_w - width) / 2.0).floor();
        Column {
            x,
            width,
            page_x: (pad - x).min(0.0),
            page_w: (window_w - 2.0 * pad).max(width),
        }
    }

    fn key(&self, i: usize, fonts: &FontRegistry) -> CacheKey {
        self.key_at(i, self.version(fonts))
    }

    /// A top-level table's layout also depends on the page (it may widen into the
    /// margins), and the footnotes section's on where its notes are (its source ranges are
    /// absolute).
    fn key_at(&self, i: usize, version: u64) -> CacheKey {
        let unit = &self.model.units[i];
        let mut hash = unit.hash;
        let mix = |hash: &mut u64, v: u64| {
            *hash ^= v.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            *hash = hash.rotate_left(17);
        };
        match (&unit.block.kind, self.page) {
            (BlockKind::Table { .. }, Some((x, w))) => mix(
                &mut hash,
                ((w as u64) << 32) ^ (x as i64 as u64 & 0xffff_ffff),
            ),
            (BlockKind::Footnotes { notes }, _) => notes
                .iter()
                .for_each(|n| mix(&mut hash, n.range.start as u64)),
            _ => {}
        }
        CacheKey::new(hash, self.width, self.zoom, version)
    }

    /// Changes the zoom, keeping the top of the view on the same place in the same unit.
    pub fn set_zoom(&mut self, zoom: f32) {
        if zoom == self.zoom {
            return;
        }
        self.anchor.offset *= (zoom / self.zoom) as f64;
        self.zoom = zoom;
        self.hscroll.clear();
        self.estimate_all();
    }

    /// Sets the column width and viewport height; on a width or zoom change all heights
    /// go back to estimates (the anchor keeps the view in place) and sideways scrolls
    /// reset, since a block may now fit or scroll less.
    pub fn resize(&mut self, width: f32, viewport_h: f32) {
        self.viewport_h = viewport_h;
        if (width / mdt_text::cache::WIDTH_BUCKET).floor()
            != (self.width / mdt_text::cache::WIDTH_BUCKET).floor()
            || self.heights.len() != self.model.units.len()
        {
            self.width = width;
            self.hscroll.clear();
            self.estimate_all();
        }
    }

    /// Sets the page a top-level table may widen into (`Column::page_x`, `page_w`),
    /// snapped to the layout cache's width buckets. A change lays out tables again and
    /// resets their sideways scrolls.
    pub fn set_page(&mut self, x: f32, w: f32) {
        let bucket = mdt_text::cache::WIDTH_BUCKET;
        let snapped_w = (w / bucket).floor() * bucket;
        let page = Some(((x + (w - snapped_w) / 2.0).round(), snapped_w));
        if page == self.page {
            return;
        }
        self.page = page;
        for (i, u) in self.model.units.iter().enumerate() {
            if matches!(u.block.kind, BlockKind::Table { .. }) {
                if let Some(m) = self.measured.get_mut(i) {
                    *m = false;
                }
                self.hscroll.retain(|&(unit, _), _| unit != i);
            }
        }
    }

    fn layout_once(&mut self, i: usize, fonts: &mut FontRegistry, width: f32) -> UnitLayout {
        self.math.now = Instant::now();
        let mut ctx = Ctx::new(
            fonts,
            &mut self.lcx,
            self.zoom,
            &self.highlights,
            &mut self.math,
            &mut self.images,
            &mut self.diagrams,
        );
        ctx.page = self.page;
        let l = block::layout_unit(&mut ctx, i, &self.model.units[i], width);
        for m in ctx.missing {
            if self
                .highlights_inflight
                .insert(mdt_highlight::Cache::key(&m.0, &m.1))
            {
                self.pending_highlights.push(m);
            }
        }
        l
    }

    fn layout(&mut self, i: usize, fonts: &mut FontRegistry, cx: &App) -> Rc<UnitLayout> {
        let key = self.key(i, fonts);
        if let Some(l) = self.cache.get(&key).cloned() {
            self.cache.touch(&key, i);
            return l;
        }
        let mut l = self.layout_once(i, fonts, key.layout_width());
        // Characters outside the bundled fonts pull in system fonts on first use.
        let before = fonts.version();
        for item in &l.items {
            if let Item::Text(t) = item {
                let _ = fonts.ensure_layout_fonts(&t.paragraph.layout, cx.text_system());
            }
        }
        if fonts.version() != before {
            // Paint ops were built before the fallback fonts had GPUI ids; build them again.
            l = self.layout_once(i, fonts, key.layout_width());
        }
        let l = Rc::new(l);
        self.cache.insert(self.key(i, fonts), i, l.clone());
        self.last_version = Some(self.version(fonts));
        l
    }

    /// The number of unit layouts held.
    pub fn cached_layouts(&self) -> usize {
        self.cache.len()
    }

    /// Cache version: font set and highlight results that have landed since.
    fn version(&self, fonts: &FontRegistry) -> u64 {
        fonts.version() | (self.highlight_epoch << 32)
    }

    /// Code blocks waiting for highlighting; the caller computes them in the background
    /// and calls `highlight_landed`.
    pub fn take_pending_highlights(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending_highlights)
    }

    /// A code block's highlighting is in the cache. It may be requested again, which it is
    /// if the cache (bounded by bytes) evicts it later.
    pub fn highlight_landed(&mut self, key: &mdt_highlight::CacheKey) {
        self.highlights_inflight.remove(key);
        self.content_landed();
    }

    /// Background results (highlighting, math, diagrams, images) have landed: lay units
    /// out again as they come into view. Heights that change above the anchor do not move
    /// the visible content.
    pub fn content_landed(&mut self) {
        self.highlight_epoch += 1;
        self.measured.iter_mut().for_each(|m| *m = false);
    }

    /// Runs queued image and diagram work on this thread until none is left, then lays out
    /// again (tests and offscreen renders).
    pub fn run_images_blocking(&mut self) {
        loop {
            let work = self.images.take_pending();
            let diagrams = self.diagrams.take_pending();
            if work.is_empty() && diagrams.is_empty() {
                break;
            }
            for w in work {
                w.run();
            }
            for w in diagrams {
                w.run();
            }
        }
        self.images.watch_probed();
        self.content_landed();
    }

    /// The remote-image URL whose placeholder is under window point `at`.
    pub fn load_at(&self, at: gpui::Point<Pixels>) -> Option<String> {
        self.loads
            .iter()
            .find(|(b, _)| b.contains(&at))
            .map(|(_, url)| url.clone())
    }

    /// Lays out the visible units (always) and the overscan (within the frame budget),
    /// recording measured heights. Returns the visible units with their layouts.
    pub fn prepare(&mut self, fonts: &mut FontRegistry, cx: &App) -> Vec<(usize, Rc<UnitLayout>)> {
        if self.model.units.is_empty() {
            return Vec::new();
        }
        let start = Instant::now();
        // Visible units first. Measuring one can change heights below it, so re-query the
        // range until it is stable.
        let mut visible;
        loop {
            let top = self.anchor.scroll_y(&self.heights);
            visible = self.heights.range(top, top + self.viewport_h as f64);
            let mut changed = false;
            for i in visible.clone() {
                if !self.measured[i] {
                    let l = self.layout(i, fonts, cx);
                    self.heights.set(i, l.height);
                    self.measured[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Overscan: one viewport below, then above, within the budget.
        let top = self.anchor.scroll_y(&self.heights);
        let below = self.heights.range(
            top + self.viewport_h as f64,
            top + 2.0 * self.viewport_h as f64,
        );
        let above = self
            .heights
            .range((top - self.viewport_h as f64).max(0.0), top);
        for i in below.chain(above.rev()) {
            if start.elapsed() >= FRAME_BUDGET {
                break;
            }
            if !self.measured[i] {
                let l = self.layout(i, fonts, cx);
                self.heights.set(i, l.height);
                self.measured[i] = true;
            }
        }
        // Keep the layouts of units within three viewports above and below the screen, made
        // for the current width, zoom and fonts.
        let vh = self.viewport_h as f64;
        let keep = self
            .heights
            .range((top - 3.0 * vh).max(0.0), top + 4.0 * vh);
        let current = CacheKey::new(0, self.width, self.zoom, self.version(fonts));
        self.cache.retain_units(keep, &current);
        visible.map(|i| (i, self.layout(i, fonts, cx))).collect()
    }

    /// Layouts of the measured units within one viewport above and below the screen, whose
    /// images and diagrams are rendered ahead of scrolling.
    pub fn overscan_layouts(&mut self, fonts: &mut FontRegistry, cx: &App) -> Vec<Rc<UnitLayout>> {
        let top = self.anchor.scroll_y(&self.heights);
        let vp = self.viewport_h as f64;
        let below = self.heights.range(top + vp, top + 2.0 * vp);
        let above = self.heights.range((top - vp).max(0.0), top);
        let units: Vec<usize> = below
            .chain(above.rev())
            .filter(|&i| self.measured[i])
            .collect();
        units
            .into_iter()
            .map(|i| self.layout(i, fonts, cx))
            .collect()
    }

    /// Whether some overscan is still unmeasured (another frame should continue it).
    pub fn overscan_pending(&self) -> bool {
        let top = self.anchor.scroll_y(&self.heights);
        let r = self.heights.range(
            (top - self.viewport_h as f64).max(0.0),
            top + 2.0 * self.viewport_h as f64,
        );
        r.into_iter().any(|i| !self.measured[i])
    }

    /// Scrolls the wide block under window point `at` sideways. Returns false (and the
    /// caller scrolls the page) when no scrollable block is there.
    pub fn scroll_horizontal(&mut self, at: (f32, f32), dx: f32) -> bool {
        let y = self.anchor.scroll_y(&self.heights) + (at.1 - self.origin.1) as f64;
        if self.heights.is_empty() || y < 0.0 {
            return false;
        }
        let (unit, within) = self.heights.find(y);
        let Some(fonts_version) = self.last_version else {
            return false;
        };
        let key = self.key_at(unit, fonts_version);
        let Some(l) = self.cache.get(&key) else {
            return false;
        };
        let x = at.0 - self.origin.0;
        let Some((n, region)) = l
            .regions
            .iter()
            .enumerate()
            .find(|(_, r)| r.max_scroll() > 0.0 && r.contains(x, within as f32))
        else {
            return false;
        };
        let max = region.max_scroll();
        let offset = self.hscroll.entry((unit, n)).or_insert(0.0);
        *offset = (*offset + dx).clamp(0.0, max);
        true
    }

    /// The source range of the task checkbox under window point `at`, if any.
    pub fn checkbox_at(&self, at: (f32, f32)) -> Option<std::ops::Range<usize>> {
        let y = self.anchor.scroll_y(&self.heights) + (at.1 - self.origin.1) as f64;
        if self.heights.is_empty() || y < 0.0 {
            return None;
        }
        let (unit, within) = self.heights.find(y);
        let key = self.key_at(unit, self.last_version?);
        let l = self.cache.get(&key)?;
        let (x, y) = (at.0 - self.origin.0, within as f32);
        let start = block::source_base(&self.model.units[unit]);
        // A little slack around the box makes it easier to hit.
        l.items.iter().find_map(|item| match item {
            Item::Checkbox {
                x: bx,
                y: by,
                size,
                source,
                ..
            } if x >= bx - 3.0 && x <= bx + size + 3.0 && y >= by - 3.0 && y <= by + size + 3.0 => {
                Some(start.wrapping_add(source.start)..start.wrapping_add(source.end))
            }
            _ => None,
        })
    }

    /// Where a link was drawn in the last frame, in window coordinates.
    pub fn link_bounds(&self, link: &block::Link) -> Option<Bounds<Pixels>> {
        self.hovers
            .iter()
            .find(|(_, _, l)| l.as_ref() == Some(link))
            .map(|(b, _, _)| *b)
    }

    /// The message shown for the hovered area (a link's target, a math error).
    pub fn hover_message(&self) -> Option<&str> {
        self.hovers.get(self.hovered?).map(|(_, m, _)| m.as_str())
    }

    /// The link under a window position in the last frame.
    pub fn link_at(&self, at: gpui::Point<Pixels>) -> Option<block::Link> {
        self.hovers
            .iter()
            .find(|(b, _, l)| l.is_some() && b.contains(&at))
            .and_then(|(_, _, l)| l.clone())
    }

    pub fn viewport_h(&self) -> f32 {
        self.viewport_h
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.anchor
            .scroll_by(dy as f64, &self.heights, self.viewport_h as f64);
    }

    /// Scrolls `unit` to the top of the view, offset by `dy` within it.
    pub fn scroll_to_unit(&mut self, unit: usize, dy: f32) {
        if unit >= self.heights.len() {
            return;
        }
        self.anchor = Anchor { unit, offset: 0.0 };
        self.scroll_by(dy);
        self.last_activity = Some(Instant::now());
    }

    /// Scrolls a unit's `text`-th selectable text to the top of the view.
    pub fn scroll_to_text(&mut self, unit: usize, text: usize, fonts: &mut FontRegistry, cx: &App) {
        if unit >= self.model.units.len() {
            return;
        }
        let l = self.layout(unit, fonts, cx);
        let y = l
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text(t) if t.select.is_some() => Some(t.y),
                _ => None,
            })
            .nth(text)
            .unwrap_or(0.0);
        self.scroll_to_unit(unit, y);
    }

    /// Scrolls a find match into view (a third of the way down) unless it is visible.
    pub fn reveal_found(
        &mut self,
        unit: usize,
        text: usize,
        range: std::ops::Range<usize>,
        fonts: &mut FontRegistry,
        cx: &App,
    ) {
        if unit >= self.model.units.len() {
            return;
        }
        let l = self.layout(unit, fonts, cx);
        let y = l
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text(t) if t.select.is_some() => Some(t),
                _ => None,
            })
            .nth(text)
            .and_then(|t| {
                mdt_text::selection_rects(&t.paragraph.layout, range)
                    .first()
                    .map(|r| t.y + r.y)
            })
            .unwrap_or(0.0);
        let top = self.heights.top(unit) + y as f64;
        let scroll = self.anchor.scroll_y(&self.heights);
        let line = (block::BODY_SIZE * block::LINE_HEIGHT * self.zoom) as f64;
        if top < scroll || top + line > scroll + self.viewport_h as f64 {
            self.scroll_to_unit(unit, y - self.viewport_h / 3.0);
        }
    }
}

const INDICATOR_W: f32 = 6.0;
/// Hovering this close to the window's right edge shows the indicator.
const HOVER_EDGE: f32 = 16.0;
const INDICATOR_HOLD: Duration = Duration::from_millis(600);
const INDICATOR_FADE: Duration = Duration::from_millis(400);

/// Indicator opacity `elapsed` after the last activity: full, then fading to zero by 1 s.
pub fn indicator_opacity(elapsed: Duration) -> f32 {
    if elapsed <= INDICATOR_HOLD {
        1.0
    } else {
        let t = (elapsed - INDICATOR_HOLD).as_secs_f32() / INDICATOR_FADE.as_secs_f32();
        (1.0 - t).max(0.0)
    }
}

/// Indicator thumb (top, height) in a track as tall as the viewport; zero height when the
/// document fits.
pub fn thumb(scroll_y: f32, total: f32, viewport: f32) -> (f32, f32) {
    if total <= viewport || viewport <= 0.0 {
        return (0.0, 0.0);
    }
    let h = (viewport * viewport / total).max(24.0).min(viewport);
    let y = scroll_y / (total - viewport) * (viewport - h);
    (y.clamp(0.0, viewport - h), h)
}

pub fn brush_color(theme: &Theme, b: Brush) -> Hsla {
    match b {
        brush::MUTED => theme.muted,
        brush::LINK => theme.link,
        brush::CODE => theme.code_text,
        brush::CODE_BG => theme.code_background,
        brush::RULE => theme.rule,
        Brush(n @ 10..=14) => theme.alerts[(n - 10) as usize],
        Brush(n @ brush::SYNTAX_BASE..) => theme
            .syntax
            .get((n - brush::SYNTAX_BASE) as usize)
            .copied()
            .unwrap_or(theme.code_text),
        _ => theme.text,
    }
}

fn rect(origin: gpui::Point<Pixels>, x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(origin + point(px(x), px(y)), size(px(w), px(h)))
}

/// A find match on a unit's `text`-th selectable text.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub text: usize,
    pub range: std::ops::Range<usize>,
    pub current: bool,
}

/// Finds the raster for an image or diagram item, by its frame's logical size.
pub type Rasters<'a> = &'a dyn Fn(&Item) -> Option<Arc<gpui::RenderImage>>;

pub fn paint_unit(
    l: &UnitLayout,
    origin: gpui::Point<Pixels>,
    hscroll: &dyn Fn(usize) -> f32,
    (selection, found): (Option<(usize, &Selection)>, &[Found]),
    theme: &Theme,
    rasters: Rasters,
    window: &mut Window,
) {
    // Selectable text index of each item, for matching find results to items.
    let mut n = 0;
    let text_index: Vec<Option<usize>> = l
        .items
        .iter()
        .map(|item| match item {
            Item::Text(t) if t.select.is_some() => {
                n += 1;
                Some(n - 1)
            }
            _ => None,
        })
        .collect();
    // Matches come by text, in order. Only those in view get rectangles: a paragraph of
    // 1,000,000 matches took 7 s a frame.
    let marks = |i: usize,
                 item: &Item,
                 origin: gpui::Point<Pixels>,
                 mask: Bounds<Pixels>|
     -> Vec<(mdt_text::Rect, gpui::Hsla)> {
        let (Item::Text(t), Some(k)) = (item, text_index[i]) else {
            return Vec::new();
        };
        let from = found.partition_point(|f| f.text < k);
        let to = from + found[from..].partition_point(|f| f.text == k);
        if from == to {
            return Vec::new();
        }
        let at = origin + point(px(t.x), px(t.y));
        let visible = mdt_text::visible_bytes(
            &t.paragraph.layout,
            &mdt_text::Rect {
                x: f32::from(mask.origin.x - at.x),
                y: f32::from(mask.origin.y - at.y),
                w: f32::from(mask.size.width),
                h: f32::from(mask.size.height),
            },
        );
        let shown: Vec<&Found> = found[from..to]
            .iter()
            .filter(|f| f.range.start < visible.end && f.range.end > visible.start)
            .collect();
        let ranges: Vec<_> = shown.iter().map(|f| f.range.clone()).collect();
        mdt_text::ranges_rects(&t.paragraph.layout, &ranges)
            .into_iter()
            .zip(shown)
            .flat_map(|(rects, f)| {
                let color = if f.current {
                    theme.match_current
                } else {
                    theme.match_highlight
                };
                rects.into_iter().map(move |r| (r, color))
            })
            .collect()
    };
    let sel = |i: usize, item: &Item| -> Vec<mdt_text::Rect> {
        match (selection, item, text_index[i]) {
            (Some((unit, s)), Item::Text(t), Some(k)) => match &t.select {
                Some(st) => s
                    .item_range(unit, k, &st.text)
                    .map(|r| mdt_text::selection_rects(&t.paragraph.layout, r))
                    .unwrap_or_default(),
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    };
    let mut i = 0;
    while i < l.items.len() {
        match l.regions.iter().position(|r| r.items.start == i) {
            Some(n) => {
                let r = &l.regions[n];
                let mask = ContentMask {
                    bounds: rect(origin, r.x, r.y, r.w, r.h),
                };
                let shifted = origin - point(px(hscroll(n)), px(0.0));
                window.with_content_mask(Some(mask), |window| {
                    let mask = window.content_mask().bounds;
                    for k in r.items.clone() {
                        paint_item(
                            &l.items[k],
                            &sel(k, &l.items[k]),
                            &marks(k, &l.items[k], shifted, mask),
                            shifted,
                            theme,
                            rasters,
                            window,
                        );
                    }
                });
                i = r.items.end.max(i + 1);
            }
            None => {
                let mask = window.content_mask().bounds;
                paint_item(
                    &l.items[i],
                    &sel(i, &l.items[i]),
                    &marks(i, &l.items[i], origin, mask),
                    origin,
                    theme,
                    rasters,
                    window,
                );
                i += 1;
            }
        }
    }
}

fn paint_item(
    item: &Item,
    selection: &[mdt_text::Rect],
    marks: &[(mdt_text::Rect, gpui::Hsla)],
    origin: gpui::Point<Pixels>,
    theme: &Theme,
    rasters: Rasters,
    window: &mut Window,
) {
    let color = |b: Brush| brush_color(theme, b);
    match item {
        Item::Fill { x, y, w, h, brush } => {
            window.paint_quad(gpui::fill(rect(origin, *x, *y, *w, *h), color(*brush)))
        }
        Item::Checkbox {
            x,
            y,
            size: s,
            checked,
            ..
        } => {
            let b = rect(origin, *x, *y, *s, *s);
            if *checked {
                window.paint_quad(gpui::fill(b, theme.link).corner_radii(px(3.0)));
                let inner = rect(origin, x + s * 0.3, y + s * 0.3, s * 0.4, s * 0.4);
                window.paint_quad(gpui::fill(inner, theme.background).corner_radii(px(1.0)));
            } else {
                window.paint_quad(
                    gpui::outline(b, theme.muted, gpui::BorderStyle::Solid).corner_radii(px(3.0)),
                );
            }
        }
        Item::Text(t) => {
            let at = origin + point(px(t.x), px(t.y));
            for (r, color) in marks {
                let b = Bounds::new(at + point(px(r.x), px(r.y)), size(px(r.w), px(r.h)));
                window.paint_quad(gpui::fill(b, *color).corner_radii(px(2.0)));
            }
            mdt_text::paint(&t.ops, selection, theme.selection, at, color, window);
        }
        Item::Math {
            x,
            y,
            scale,
            font,
            scene,
        } => paint_math(
            scene,
            origin + point(px(*x), px(*y)),
            *scale,
            *font,
            theme.text,
            window,
        ),
        Item::Image { x, y, w, h, .. } | Item::Diagram { x, y, w, h, .. } => {
            let b = rect(origin, *x, *y, *w, *h);
            if let Some(image) = rasters(item) {
                super::images::paint_raster(b, image, window);
            }
        }
        Item::Placeholder { x, y, w, h, .. } => {
            let b = rect(origin, *x, *y, *w, *h);
            window.paint_quad(gpui::fill(b, theme.code_background).corner_radii(px(4.0)));
            window.paint_quad(
                gpui::outline(b, theme.rule, gpui::BorderStyle::Solid).corner_radii(px(4.0)),
            );
            // The broken-image mark: a frame with a diagonal tear.
            let s = (*h - 16.0).clamp(8.0, block::PLACEHOLDER_ICON);
            let pad = (*h - s).min(16.0) / 2.0;
            let icon = rect(origin, x + pad, y + pad, s, s);
            window.paint_quad(
                gpui::outline(icon, theme.muted, gpui::BorderStyle::Solid).corner_radii(px(2.0)),
            );
            let mut tear = gpui::PathBuilder::stroke(px(1.5));
            tear.move_to(icon.origin + point(px(s * 0.15), px(s * 0.8)));
            tear.line_to(icon.origin + point(px(s * 0.45), px(s * 0.45)));
            tear.line_to(icon.origin + point(px(s * 0.6), px(s * 0.6)));
            tear.line_to(icon.origin + point(px(s * 0.85), px(s * 0.2)));
            if let Ok(path) = tear.build() {
                window.paint_path(path, theme.muted);
            }
        }
    }
}

const TOOLTIP_PAD: f32 = 6.0;

/// An area's x and width as shown, from its (x, y, w, h) in the unit: inside a
/// sideways-scrolling block it moves with the block's scroll and is clipped to the block;
/// `None` when scrolled out of view.
fn scrolled_span(
    l: &UnitLayout,
    hscroll: &HashMap<(usize, usize), f32>,
    unit: usize,
    (x, y, w, h): (f32, f32, f32, f32),
) -> Option<(f32, f32)> {
    let mid_y = y + h / 2.0;
    let Some((n, r)) = l
        .regions
        .iter()
        .enumerate()
        .find(|(_, r)| mid_y >= r.y && mid_y < r.y + r.h && x >= r.x && x < r.x + r.content_w)
    else {
        return Some((x, w));
    };
    let x = x - hscroll.get(&(unit, n)).copied().unwrap_or(0.0);
    let (lo, hi) = (x.max(r.x), (x + w).min(r.x + r.w));
    (hi > lo).then_some((lo, hi - lo))
}

impl Reader {
    /// Records this frame's hover areas (math errors, links) and remote-image placeholders
    /// in window coordinates, for units placed at `origin` plus their y. Inside a
    /// sideways-scrolling block they move with its scroll.
    pub fn record_areas(
        &mut self,
        placed: &[(usize, f32, Rc<UnitLayout>)],
        origin: gpui::Point<Pixels>,
    ) {
        let hscroll = &self.hscroll;
        let area = |i: usize, y: f32, l: &UnitLayout, a: (f32, f32, f32, f32)| {
            let (x, w) = scrolled_span(l, hscroll, i, a)?;
            Some(rect(origin, x, y + a.1, w, a.3))
        };
        self.hovers = placed
            .iter()
            .flat_map(|(i, y, l)| {
                l.hovers.iter().filter_map(move |h| {
                    let b = area(*i, *y, l, (h.x, h.y, h.w, h.h))?;
                    Some((b, h.message.clone(), h.link.clone()))
                })
            })
            .collect();
        self.loads = placed
            .iter()
            .flat_map(|(i, y, l)| {
                l.loads.iter().filter_map(move |d| {
                    Some((area(*i, *y, l, (d.x, d.y, d.w, d.h))?, d.url.clone()))
                })
            })
            .collect();
        let pointer = self.last_pointer.map(|(x, y)| point(px(x), px(y)));
        self.hovered =
            pointer.and_then(|p| self.hovers.iter().position(|(b, _, _)| b.contains(&p)));
    }

    /// Lays out the message of the hovered area (a math error, a link's target) below it.
    fn hover_tooltip(
        &mut self,
        fonts: &mut FontRegistry,
    ) -> Option<(Bounds<Pixels>, Vec<mdt_text::PaintOp>)> {
        let (area, message, _) = self.hovers.get(self.hovered?)?;
        let base = BaseStyle {
            size: 13.0 * self.zoom,
            line_height: 1.4,
            mono: false,
            bold: false,
            brush: brush::TEXT,
        };
        let p = layout_paragraph(
            fonts,
            &mut self.lcx,
            message,
            base,
            &[],
            &[],
            420.0 * self.zoom,
        );
        let ops = mdt_text::paint_ops(&p.layout, fonts);
        let b = Bounds::new(
            area.bottom_left() + point(px(0.0), px(4.0)),
            size(
                px(p.width + 2.0 * TOOLTIP_PAD),
                px(p.height + 2.0 * TOOLTIP_PAD),
            ),
        );
        Some((b, ops))
    }
}

/// Device pixels for a frame of `w`×`h` logical pixels.
pub fn device_size(w: f32, h: f32, scale: f32) -> (u32, u32) {
    (
        ((w * scale).round() as u32).max(1),
        ((h * scale).round() as u32).max(1),
    )
}

/// Paints a math scene with its top-left at `origin`, `scale` pixels per point.
fn paint_math(
    scene: &mdt_math::MathScene,
    origin: gpui::Point<Pixels>,
    k: f32,
    font: gpui::FontId,
    color: Hsla,
    window: &mut Window,
) {
    use mdt_math::PathCmd;
    let at = |x: f32, y: f32| origin + point(px(x * k), px(y * k));
    for g in &scene.glyphs {
        // A glyph that cannot be rasterized is skipped rather than failing the frame.
        let _ = window.paint_glyph(
            at(g.x, g.y),
            font,
            gpui::GlyphId(g.id as u32),
            px(g.size * k),
            color,
        );
    }
    for r in &scene.rules {
        window.paint_quad(gpui::fill(
            Bounds::new(at(r.x, r.y), size(px(r.w * k), px(r.h * k))),
            color,
        ));
    }
    for p in &scene.paths {
        let mut b = match p.stroke {
            Some(w) => gpui::PathBuilder::stroke(px(w * k)),
            None => gpui::PathBuilder::fill(),
        };
        for c in &p.cmds {
            match *c {
                PathCmd::Move(x, y) => b.move_to(at(x, y)),
                PathCmd::Line(x, y) => b.line_to(at(x, y)),
                PathCmd::Cubic(ax, ay, bx, by, x, y) => {
                    b.cubic_bezier_to(at(x, y), at(ax, ay), at(bx, by))
                }
                PathCmd::Close => b.close(),
            }
        }
        if let Ok(path) = b.build() {
            window.paint_path(path, color);
        }
    }
}

/// Rasters made for the frame: images by (source, device size), diagrams by (key, device
/// size).
type FrameRasters = (
    HashMap<super::images::RasterKey, Arc<gpui::RenderImage>>,
    HashMap<(super::diagrams::DiagramKey, u32, u32), Arc<gpui::RenderImage>>,
);

impl Reader {
    /// Asks for the rasters of the images and diagrams on screen at device pixels (`scale`
    /// per logical pixel), then of those in the overscan so they are ready on arrival.
    /// Returns the rasters already made for the screen.
    fn frame_rasters(
        &mut self,
        placed: &[(usize, f32, Rc<UnitLayout>)],
        (fonts, cx): (&mut FontRegistry, &App),
        dark: bool,
        scale: f32,
        screen_h: f32,
    ) -> FrameRasters {
        let mut rasters = HashMap::new();
        let mut diagram_rasters = HashMap::new();
        let mut visible = std::collections::HashSet::new();
        // On screen and still without a raster (bench timing).
        #[cfg(feature = "bench")]
        let mut waiting = Vec::new();
        #[cfg(feature = "bench")]
        let on_screen = |y: f32, h: f32| y + h > 0.0 && y < screen_h;
        #[cfg(not(feature = "bench"))]
        let _ = screen_h;
        for (_, _y, l) in placed {
            for item in &l.items {
                match item {
                    Item::Image { w, h, source, .. } => {
                        let (pw, ph) = device_size(*w, *h, scale);
                        match self.images.raster(source, pw, ph) {
                            Some(image) => {
                                rasters.insert((source.clone(), pw, ph), image);
                            }
                            #[cfg(feature = "bench")]
                            None if on_screen(_y + item_y(item), *h) => {
                                waiting.push((crate::bench::Kind::Image, format!("{source:?}")))
                            }
                            None => {}
                        }
                        visible.insert(source.clone());
                    }
                    Item::Diagram { w, h, key, .. } => {
                        let (pw, ph) = device_size(*w, *h, scale);
                        match self.diagrams.raster(*key, dark, pw, ph) {
                            Some(image) => {
                                diagram_rasters.insert((*key, pw, ph), image);
                            }
                            #[cfg(feature = "bench")]
                            None if on_screen(_y + item_y(item), *h) => waiting
                                .push((crate::bench::Kind::Diagram, format!("{key:?}{dark}"))),
                            None => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        for l in self.overscan_layouts(fonts, cx) {
            for item in &l.items {
                match item {
                    Item::Image { w, h, source, .. } => {
                        let (pw, ph) = device_size(*w, *h, scale);
                        let _ = self.images.raster(source, pw, ph);
                    }
                    Item::Diagram { w, h, key, .. } => {
                        let (pw, ph) = device_size(*w, *h, scale);
                        let _ = self.diagrams.raster(*key, dark, pw, ph);
                    }
                    _ => {}
                }
            }
        }
        // Images on screen are evicted last.
        self.images.set_visible(visible);
        #[cfg(feature = "bench")]
        crate::bench::waiting(waiting);
        (rasters, diagram_rasters)
    }
}

/// An image's or diagram's y within its unit (bench timing).
#[cfg(feature = "bench")]
fn item_y(item: &Item) -> f32 {
    match item {
        Item::Image { y, .. } | Item::Diagram { y, .. } => *y,
        _ => 0.0,
    }
}

/// Runs `task` in the background; when it is done, `landed` updates the reader and the
/// view draws again.
fn on_landed<T: 'static>(
    reader: &Rc<RefCell<Reader>>,
    notify: &Rc<impl Fn(&mut Window, &mut App) + 'static>,
    window: &Window,
    cx: &App,
    task: impl Future<Output = T> + 'static,
    landed: impl FnOnce(&mut Reader, T) + 'static,
) {
    let (reader, notify) = (reader.clone(), notify.clone());
    window
        .spawn(cx, async move |cx| {
            let out = task.await;
            landed(&mut reader.borrow_mut(), out);
            let _ = cx.update(|window, cx| notify(window, cx));
        })
        .detach();
}

/// Starts the background work the frame asked for: highlighting, typesetting, image and
/// diagram work, the file watcher's changes and held-back math errors.
fn spawn_work(
    r: &mut Reader,
    reader: &Rc<RefCell<Reader>>,
    notify: &Rc<impl Fn(&mut Window, &mut App) + 'static>,
    window: &Window,
    cx: &App,
) {
    let background = cx.background_executor();
    for (info, code) in r.take_pending_highlights() {
        let cache = r.highlights.clone();
        let key = mdt_highlight::Cache::key(&info, &code);
        let task = background.spawn(async move {
            cache.compute(&info, &code);
        });
        on_landed(reader, notify, window, cx, task, move |r, ()| {
            r.highlight_landed(&key)
        });
    }
    let batch = r.math.take_pending();
    if !batch.is_empty() {
        let cache = r.math.cache.clone();
        let keys: Vec<mdt_math::Key> = batch.iter().map(|(l, d)| mdt_math::key(l, *d)).collect();
        #[cfg(feature = "bench")]
        let t0 = crate::bench::equations_begin();
        let task = background.spawn(async move {
            #[cfg(feature = "bench")]
            for (latex, display) in &batch {
                // Only cache misses count as typesetting.
                let (s, misses) = (Instant::now(), cache.compiles());
                let _ = cache.compute(latex, *display);
                if cache.compiles() > misses {
                    crate::bench::equation_typeset(s.elapsed());
                }
            }
            cache.compute_batch(&batch)
        });
        on_landed(reader, notify, window, cx, task, move |r, ()| {
            #[cfg(feature = "bench")]
            crate::bench::equations_landed(t0);
            r.math.landed(&keys);
            r.content_landed();
        });
    }
    // A math error held back while its source is fresh shows when it is due.
    if let Some(due) = r.math.due.take() {
        let timer = background.timer(due.saturating_duration_since(Instant::now()));
        on_landed(reader, notify, window, cx, timer, |r, ()| {
            r.content_landed()
        });
    }
    for work in r.diagrams.take_pending() {
        #[cfg(feature = "bench")]
        let (token, last) = (crate::bench::begin(work.bench_key()), work.is_raster());
        let task = background.spawn(async move { work.run() });
        on_landed(reader, notify, window, cx, task, move |r, layout| {
            #[cfg(feature = "bench")]
            crate::bench::landed(crate::bench::Kind::Diagram, token, last);
            if layout {
                r.content_landed();
            }
        });
    }
    for work in r.images.take_pending() {
        #[cfg(feature = "bench")]
        let (token, last) = (
            crate::bench::begin(work.bench_key()),
            !work.changes_layout(),
        );
        let task = background.spawn(work.run_async());
        on_landed(reader, notify, window, cx, task, move |r, layout| {
            #[cfg(feature = "bench")]
            crate::bench::landed(crate::bench::Kind::Image, token, last);
            if layout {
                r.images.watch_probed();
                r.content_landed();
            }
        });
    }
    // Image files that change on disk are probed and decoded again.
    if let Some(mut changes) = r.images.take_changes() {
        let (reader, notify) = (reader.clone(), notify.clone());
        window
            .spawn(cx, async move |cx| {
                use futures::StreamExt;
                while let Some(path) = changes.next().await {
                    {
                        let mut r = reader.borrow_mut();
                        r.images.invalidate(&path);
                        r.content_landed();
                    }
                    let _ = cx.update(|window, cx| notify(window, cx));
                }
            })
            .detach();
    }
}

/// How far a wheel event scrolls (right, down) in pixels: a line is a body line at the
/// zoom. Shift turns a vertical wheel into horizontal scrolling.
pub fn wheel_delta(e: &ScrollWheelEvent, zoom: f32) -> (f32, f32) {
    let d = e
        .delta
        .pixel_delta(px(block::BODY_SIZE * block::LINE_HEIGHT * zoom));
    let (dx, dy) = (-f32::from(d.x), -f32::from(d.y));
    if e.modifiers.shift && dx == 0.0 {
        (dy, 0.0)
    } else {
        (dx, dy)
    }
}

/// The Reader element: paints the visible units and scrolls on wheel events.
pub fn element(
    reader: Rc<RefCell<Reader>>,
    theme: Theme,
    on_change: impl Fn(&mut Window, &mut App) + 'static,
    on_toggle_task: impl Fn(std::ops::Range<usize>, &mut Window, &mut App) + 'static,
    on_link: impl Fn(block::Link, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let for_scroll = reader.clone();
    let on_change = Rc::new(on_change);
    let notify = on_change.clone();
    #[cfg(feature = "bench")]
    let bench_notify = on_change.clone();
    let paint = canvas(
        move |bounds, window, cx| {
            let fonts = fonts(cx);
            let mut fonts = fonts.borrow_mut();
            let mut r = reader.borrow_mut();
            let col = r.column(&mut fonts, f32::from(bounds.size.width));
            let viewport = f32::from(bounds.size.height) - TOP_PAD * r.zoom;
            r.resize(col.width, viewport.max(1.0));
            r.set_page(col.page_x, col.page_w);
            r.origin = (
                f32::from(bounds.origin.x) + col.x,
                f32::from(bounds.origin.y) + TOP_PAD * r.zoom,
            );
            let units = r.prepare(&mut fonts, cx);
            if r.overscan_pending() {
                let n = notify.clone();
                window.on_next_frame(move |window, cx| n(window, cx));
            }
            let scroll_y = r.anchor.scroll_y(&r.heights);
            let placed: Vec<(usize, f32, Rc<UnitLayout>)> = units
                .into_iter()
                .map(|(i, l)| {
                    (
                        i,
                        (r.heights.top(i) - scroll_y) as f32 + TOP_PAD * r.zoom,
                        l,
                    )
                })
                .collect();
            let hscroll = r.hscroll.clone();
            let rasters = r.frame_rasters(
                &placed,
                (&mut fonts, cx),
                theme.dark,
                window.scale_factor(),
                f32::from(bounds.size.height),
            );
            spawn_work(&mut r, &reader, &notify, window, cx);
            // The scroll indicator: shown while scrolling or hovering the right edge.
            let opacity = r
                .last_activity
                .map_or(0.0, |t| indicator_opacity(t.elapsed()));
            if opacity > 0.0 {
                let n = notify.clone();
                window.on_next_frame(move |window, cx| n(window, cx));
            }
            let (thumb_y, thumb_h) = thumb(
                r.anchor.scroll_y(&r.heights) as f32,
                // The top padding scrolls with the content.
                r.heights.total() as f32 + TOP_PAD * r.zoom,
                f32::from(bounds.size.height),
            );
            let indicator = (opacity > 0.0 && thumb_h > 0.0).then_some((thumb_y, thumb_h, opacity));
            r.record_areas(&placed, bounds.origin + point(px(col.x), px(0.0)));
            let tooltip = r.hover_tooltip(&mut fonts);
            let found: Vec<Vec<Found>> = placed
                .iter()
                .map(|(i, _, _)| r.found.get(i).cloned().unwrap_or_default())
                .collect();
            (
                col,
                placed,
                hscroll,
                r.selection,
                found,
                indicator,
                tooltip,
                rasters,
            )
        },
        move |bounds,
              (col, placed, hscroll, selection, found, indicator, tooltip, (rasters, diagrams)),
              window,
              _cx| {
            let scale = window.scale_factor();
            let lookup = |item: &Item| match item {
                Item::Image { w, h, source, .. } => {
                    let (pw, ph) = device_size(*w, *h, scale);
                    rasters.get(&(source.clone(), pw, ph)).cloned()
                }
                Item::Diagram { w, h, key, .. } => {
                    let (pw, ph) = device_size(*w, *h, scale);
                    diagrams.get(&(*key, pw, ph)).cloned()
                }
                _ => None,
            };
            window.paint_quad(gpui::fill(bounds, theme.background));
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for ((i, y, l), found) in placed.iter().zip(&found) {
                    let origin = bounds.origin + point(px(col.x), px(*y));
                    let offset = |n: usize| hscroll.get(&(*i, n)).copied().unwrap_or(0.0);
                    let sel = selection.as_ref().map(|s| (*i, s));
                    paint_unit(l, origin, &offset, (sel, found), &theme, &lookup, window);
                }
                if let Some((y, h, opacity)) = indicator {
                    let b = Bounds::new(
                        point(bounds.right() - px(INDICATOR_W + 3.0), bounds.top() + px(y)),
                        size(px(INDICATOR_W), px(h)),
                    );
                    let color = theme.muted.opacity(0.6 * opacity);
                    window.paint_quad(gpui::fill(b, color).corner_radii(px(INDICATOR_W / 2.0)));
                }
                if let Some((b, ops)) = tooltip {
                    window
                        .paint_quad(gpui::fill(b, theme.overlay_background).corner_radii(px(4.0)));
                    let at = b.origin + point(px(TOOLTIP_PAD), px(TOOLTIP_PAD));
                    mdt_text::paint(
                        &ops,
                        &[],
                        theme.selection,
                        at,
                        |_| theme.overlay_text,
                        window,
                    );
                }
            });
            // Keep frames coming while the bench scrolls, as wheel events would.
            #[cfg(feature = "bench")]
            if crate::bench::frame_end(!placed.is_empty(), _cx) {
                let n = bench_notify.clone();
                window.on_next_frame(move |window, cx| n(window, cx));
            }
        },
    )
    .size_full();
    let for_down = for_scroll.clone();
    let for_move = for_scroll.clone();
    let on_down_change = on_change.clone();
    let on_move_change = on_change.clone();
    gpui::div()
        .size_full()
        .child(paint)
        .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, cx| {
            let fonts = fonts(cx);
            let mut fonts = fonts.borrow_mut();
            let mut r = for_down.borrow_mut();
            if let Some(link) = r.link_at(e.position) {
                drop(r);
                drop(fonts);
                on_link(link, window, cx);
                return;
            }
            if let Some(url) = r.load_at(e.position) {
                r.images.allow(&url);
                r.content_landed();
                drop(r);
                drop(fonts);
                on_down_change(window, cx);
                return;
            }
            let at = (f32::from(e.position.x), f32::from(e.position.y));
            if let Some(range) = r.checkbox_at(at) {
                drop(r);
                drop(fonts);
                on_toggle_task(range, window, cx);
                return;
            }
            if let Some(pos) = r.hit_test(&mut fonts, cx, at) {
                r.selection = Some(match r.selection {
                    Some(s) if e.modifiers.shift => Selection { focus: pos, ..s },
                    _ => Selection {
                        anchor: pos,
                        focus: pos,
                    },
                });
            }
            drop(r);
            on_down_change(window, cx);
        })
        .on_mouse_move(move |e: &MouseMoveEvent, window, cx| {
            if e.pressed_button != Some(MouseButton::Left) {
                // GPUI repeats the last mouse position after frames; only real movement
                // counts, or the indicator would keep itself alive.
                let at = (f32::from(e.position.x), f32::from(e.position.y));
                let edge = window.viewport_size().width - e.position.x;
                let mut r = for_move.borrow_mut();
                let moved = r.last_pointer.replace(at) != Some(at);
                let hovered = r
                    .hovers
                    .iter()
                    .position(|(b, _, _)| b.contains(&e.position));
                let hover_changed = hovered != r.hovered;
                r.hovered = hovered;
                if moved && edge < px(HOVER_EDGE) {
                    r.last_activity = Some(Instant::now());
                }
                if hover_changed || (moved && edge < px(HOVER_EDGE)) {
                    drop(r);
                    on_move_change(window, cx);
                }
                return;
            }
            let fonts = fonts(cx);
            let mut fonts = fonts.borrow_mut();
            let mut r = for_move.borrow_mut();
            let at = (f32::from(e.position.x), f32::from(e.position.y));
            if let (Some(pos), Some(_)) = (r.hit_test(&mut fonts, cx, at), r.selection) {
                r.selection.as_mut().unwrap().focus = pos;
                drop(r);
                on_move_change(window, cx);
            }
        })
        .on_scroll_wheel(move |e: &ScrollWheelEvent, window, cx| {
            let mut r = for_scroll.borrow_mut();
            let (dx, dy) = wheel_delta(e, r.zoom);
            let at = (f32::from(e.position.x), f32::from(e.position.y));
            let horizontal = dx.abs() > dy.abs() && r.scroll_horizontal(at, dx);
            if !horizontal {
                r.scroll_by(dy);
                r.last_activity = Some(Instant::now());
            }
            drop(r);
            on_change(window, cx);
        })
}

#[cfg(test)]
mod indicator_tests {
    use super::*;

    #[test]
    fn indicator_holds_then_fades_out_within_one_second() {
        assert_eq!(indicator_opacity(Duration::ZERO), 1.0);
        assert_eq!(indicator_opacity(Duration::from_millis(600)), 1.0);
        let mid = indicator_opacity(Duration::from_millis(800));
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(indicator_opacity(Duration::from_millis(1000)), 0.0);
        assert_eq!(indicator_opacity(Duration::from_secs(5)), 0.0);
    }

    #[test]
    fn wheel_lines_scale_with_zoom() {
        let wheel = |lines: f32, shift: bool| ScrollWheelEvent {
            delta: gpui::ScrollDelta::Lines(point(0.0, -lines)),
            modifiers: gpui::Modifiers {
                shift,
                ..Default::default()
            },
            ..Default::default()
        };
        let line = block::BODY_SIZE * block::LINE_HEIGHT;
        assert_eq!(wheel_delta(&wheel(3.0, false), 1.0), (0.0, 3.0 * line));
        assert_eq!(wheel_delta(&wheel(3.0, false), 2.0), (0.0, 6.0 * line));
        assert_eq!(wheel_delta(&wheel(1.0, true), 1.5), (1.5 * line, 0.0));
        let pixels = ScrollWheelEvent {
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-40.0))),
            ..Default::default()
        };
        assert_eq!(wheel_delta(&pixels, 2.0), (0.0, 40.0), "pixels as given");
    }

    #[test]
    fn thumb_tracks_position_and_hides_when_document_fits() {
        assert_eq!(thumb(0.0, 500.0, 800.0), (0.0, 0.0));
        let (y0, h) = thumb(0.0, 8000.0, 800.0);
        assert_eq!((y0, h), (0.0, 80.0));
        let (y1, _) = thumb(7200.0, 8000.0, 800.0);
        assert_eq!(
            y1, 720.0,
            "bottom of the document puts the thumb at the bottom"
        );
        let (_, tiny) = thumb(0.0, 10_000_000.0, 800.0);
        assert_eq!(tiny, 24.0, "minimum thumb size");
    }
}

#[cfg(test)]
mod hover_tests {
    use std::collections::HashMap;

    use super::block::{Region, UnitLayout};
    use super::scrolled_span;

    #[test]
    fn hovers_in_a_scrolled_block_move_with_it_and_clip() {
        let l = UnitLayout {
            height: 100.0,
            items: Vec::new(),
            regions: vec![Region {
                items: 0..0,
                x: -50.0,
                y: 0.0,
                w: 300.0,
                h: 100.0,
                content_w: 900.0,
            }],
            hovers: Vec::new(),
            loads: Vec::new(),
        };
        let hover = |x: f32, y: f32| (x, y, 40.0, 20.0);
        let mut hscroll = HashMap::new();
        // Unscrolled: as laid out.
        assert_eq!(
            scrolled_span(&l, &hscroll, 0, hover(100.0, 10.0)),
            Some((100.0, 40.0))
        );
        hscroll.insert((0, 0), 120.0);
        assert_eq!(
            scrolled_span(&l, &hscroll, 0, hover(100.0, 10.0)),
            Some((-20.0, 40.0))
        );
        // Partly scrolled out: clipped at the block's left edge.
        assert_eq!(
            scrolled_span(&l, &hscroll, 0, hover(50.0, 10.0)),
            Some((-50.0, 20.0))
        );
        // Wholly out of view.
        assert_eq!(scrolled_span(&l, &hscroll, 0, hover(0.0, 10.0)), None);
        // Past the right edge until scrolled in.
        assert_eq!(scrolled_span(&l, &hscroll, 0, hover(500.0, 10.0)), None);
        // Below the block: not shifted.
        assert_eq!(
            scrolled_span(&l, &hscroll, 0, hover(100.0, 150.0)),
            Some((100.0, 40.0))
        );
    }
}
