//! The Source editor: one Parley layout per logical line, laid out only when
//! visible, line heights in the Fenwick index, and the scroll position as a line anchor.
//! The cursor and selection are byte offsets into the document's rope.

pub mod element;
pub mod style;
pub mod text;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::App;
use mdt_doc::Selection;
use mdt_markdown::{BlockKind, BlockModel};
use mdt_text::{BaseStyle, Brush, FontRegistry, PaintOp, Paragraph, layout_paragraph, paint_ops};
use parley::{Affinity, Cursor, LayoutContext};
use ropey::Rope;

use crate::reader::block::brush;
use crate::reader::heights::{Anchor, Heights};
use crate::reader::view::Column;
use text::{line_of, line_range};

pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 1.5;
/// Character count of the column's maximum width.
pub const COLUMN_CHARS: usize = 80;
const SIDE_PAD: f32 = 32.0;
const NARROW_PAD: f32 = 16.0;
pub const TOP_PAD: f32 = 28.0;
/// The cursor stays solid this long after input, then blinks with this period.
pub const BLINK_HOLD: Duration = Duration::from_millis(500);
pub const BLINK_PERIOD: Duration = Duration::from_millis(500);

pub struct LineLayout {
    pub paragraph: Paragraph,
    pub ops: Vec<PaintOp>,
    key: u64,
}

pub struct Editor {
    pub selection: Selection,
    /// IME composition in progress.
    pub marked: Option<Range<usize>>,
    /// Line heights; the anchor's `unit` is a line index.
    pub heights: Heights,
    pub anchor: Anchor,
    measured: Vec<bool>,
    cache: HashMap<usize, Rc<LineLayout>>,
    pub zoom: f32,
    pub soft_wrap: bool,
    width: f32,
    pub viewport_h: f32,
    char_w: Option<(f32, f32)>,
    lcx: LayoutContext<Brush>,
    /// Horizontal scroll when soft wrap is off.
    pub hscroll: f32,
    /// Widest line laid out around the view (horizontal scroll limit).
    widest: f32,
    /// End put the caret here, at the end of a visual line broken inside a word (CJK, a
    /// long URL). The next visual line starts at the same offset; the caret is drawn at
    /// the end of this one.
    line_end: Option<usize>,
    /// The x that vertical movement tries to keep.
    pub goal_x: Option<f32>,
    /// Window position of the text's top-left corner when scrolled to the top.
    pub origin: (f32, f32),
    pub last_input: Instant,
    /// Last scroll, for the scroll indicator.
    pub last_activity: Option<Instant>,
    /// Whether the user has placed the cursor or typed yet.
    pub used: bool,
    /// A mouse selection is in progress.
    pub dragging: bool,
    /// Find matches to highlight, sorted, and whether each is the current one.
    pub found: Vec<(Range<usize>, bool)>,
    /// Byte ranges of code-like blocks (fenced code, math, HTML, front matter).
    code: Vec<Range<usize>>,
}

impl Editor {
    pub fn new(rope: &Rope, zoom: f32, soft_wrap: bool) -> Self {
        let mut e = Self {
            selection: Selection::default(),
            marked: None,
            heights: Heights::default(),
            anchor: Anchor::default(),
            measured: Vec::new(),
            cache: HashMap::new(),
            zoom,
            soft_wrap,
            width: 600.0,
            viewport_h: 800.0,
            char_w: None,
            lcx: LayoutContext::new(),
            hscroll: 0.0,
            widest: 0.0,
            line_end: None,
            goal_x: None,
            origin: (0.0, 0.0),
            last_input: Instant::now(),
            last_activity: None,
            used: false,
            dragging: false,
            found: Vec::new(),
            code: Vec::new(),
        };
        e.reset_lines(rope);
        e
    }

    pub fn line_h(&self) -> f32 {
        FONT_SIZE * self.zoom * LINE_HEIGHT
    }

    fn base(&self) -> BaseStyle {
        BaseStyle {
            size: FONT_SIZE * self.zoom,
            line_height: LINE_HEIGHT,
            mono: true,
            bold: false,
            brush: brush::TEXT,
        }
    }

    fn char_width(&mut self, fonts: &mut FontRegistry) -> f32 {
        match self.char_w {
            Some((zoom, w)) if zoom == self.zoom => w,
            _ => {
                let base = self.base();
                let p = layout_paragraph(
                    fonts,
                    &mut self.lcx,
                    &"0".repeat(100),
                    base,
                    &[],
                    &[],
                    f32::INFINITY,
                );
                let w = p.width / 100.0;
                self.char_w = Some((self.zoom, w));
                w
            }
        }
    }

    /// The text column for a pane width: at most `COLUMN_CHARS` characters wide, centered.
    pub fn column(&mut self, fonts: &mut FontRegistry, pane_w: f32) -> Column {
        let max = COLUMN_CHARS as f32 * self.char_width(fonts);
        let pad = if pane_w >= max + 2.0 * SIDE_PAD * self.zoom {
            SIDE_PAD
        } else {
            NARROW_PAD
        } * self.zoom;
        let width = (pane_w - 2.0 * pad).min(max).max(1.0);
        // The editor has no tables, so its page is the column.
        Column {
            x: ((pane_w - width) / 2.0).floor(),
            width,
            page_x: 0.0,
            page_w: width,
        }
    }

    fn estimate(&self, rope: &Rope, i: usize) -> f32 {
        let lh = self.line_h();
        if !self.soft_wrap {
            return lh;
        }
        let cols = self
            .char_w
            .map_or(80.0, |(_, w)| (self.width / w).floor().max(1.0));
        let chars = rope.line(i).len_chars() as f32;
        (chars / cols).ceil().max(1.0) * lh
    }

    /// All line heights back to estimates (width, zoom or wrap change; undo).
    pub fn reset_lines(&mut self, rope: &Rope) {
        let n = rope.len_lines();
        self.heights = Heights::new((0..n).map(|i| self.estimate(rope, i)).collect());
        self.measured = vec![false; n];
        self.cache.clear();
        self.line_end = None;
        self.anchor.clamp(&self.heights);
        self.clamp_selection(rope);
    }

    pub fn clamp_selection(&mut self, rope: &Rope) {
        let len = rope.len_bytes();
        self.selection.anchor = self.selection.anchor.min(len);
        self.selection.head = self.selection.head.min(len);
    }

    pub fn set_soft_wrap(&mut self, rope: &Rope, on: bool) {
        if self.soft_wrap != on {
            self.soft_wrap = on;
            self.hscroll = 0.0;
            self.reset_lines(rope);
        }
    }

    pub fn set_zoom(&mut self, rope: &Rope, zoom: f32) {
        if self.zoom != zoom {
            self.zoom = zoom;
            self.reset_lines(rope);
        }
    }

    /// Sets the text width and viewport height; a width change re-estimates wrapped lines.
    pub fn resize(&mut self, rope: &Rope, width: f32, viewport_h: f32) {
        self.viewport_h = viewport_h;
        let bucket = |w: f32| (w / mdt_text::cache::WIDTH_BUCKET).floor();
        if bucket(width) != bucket(self.width) {
            self.width = width;
            if self.soft_wrap {
                self.reset_lines(rope);
            }
        }
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    /// Takes the code-like block ranges from a new block model.
    pub fn set_model(&mut self, model: &BlockModel) {
        self.code = model
            .units
            .iter()
            .filter(|u| {
                matches!(
                    u.block.kind,
                    BlockKind::Code { .. }
                        | BlockKind::Math { .. }
                        | BlockKind::Html { .. }
                        | BlockKind::FrontMatter { .. }
                )
            })
            .map(|u| u.range.clone())
            .collect();
    }

    fn in_code(&self, b: usize) -> bool {
        let i = self.code.partition_point(|r| r.end <= b);
        self.code.get(i).is_some_and(|r| r.start <= b)
    }

    /// The cache key of line `i`'s layout: its style and length. Every edit drops the
    /// cached lines it touched (`lines_replaced`, `reset_lines`), so hits need not read
    /// the text.
    fn line_key(&self, r: &Range<usize>, in_code: bool, width: f32, fonts: u64) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (
            in_code,
            width.to_bits(),
            self.zoom.to_bits(),
            fonts,
            r.len(),
        )
            .hash(&mut h);
        h.finish()
    }

    /// The layout of line `i`, from the cache when its text and style are unchanged.
    pub fn line(
        &mut self,
        i: usize,
        rope: &Rope,
        fonts: &mut FontRegistry,
        cx: &App,
    ) -> Rc<LineLayout> {
        let r = line_range(rope, i);
        let in_code = self.in_code(r.start);
        // Wrapped at the width bucket's lower edge, which fits every width in the bucket
        // (`resize` keeps the first width it sees in a bucket).
        let width = if self.soft_wrap {
            ((self.width / mdt_text::cache::WIDTH_BUCKET).floor() * mdt_text::cache::WIDTH_BUCKET)
                .max(1.0)
        } else {
            f32::INFINITY
        };
        let key = self.line_key(&r, in_code, width, fonts.version());
        if let Some(l) = self.cache.get(&i)
            && l.key == key
        {
            return l.clone();
        }
        let text = rope.byte_slice(r.clone()).to_string();
        // Tabs are laid out as spaces (same byte length, so offsets stay valid).
        let shaped = text.replace('\t', " ");
        let runs = style::line_runs(&text, in_code);
        let base = self.base();
        let mut p = layout_paragraph(fonts, &mut self.lcx, &shaped, base, &runs, &[], width);
        let before = fonts.version();
        let _ = fonts.ensure_layout_fonts(&p.layout, cx.text_system());
        let key = if fonts.version() != before {
            // Fallback fonts were added: their glyph ids exist only now.
            p = layout_paragraph(fonts, &mut self.lcx, &shaped, base, &runs, &[], width);
            self.line_key(&r, in_code, width, fonts.version())
        } else {
            key
        };
        let l = Rc::new(LineLayout {
            ops: paint_ops(&p.layout, fonts),
            paragraph: p,
            key,
        });
        self.cache.insert(i, l.clone());
        l
    }

    fn measure(
        &mut self,
        i: usize,
        rope: &Rope,
        fonts: &mut FontRegistry,
        cx: &App,
    ) -> Rc<LineLayout> {
        let l = self.line(i, rope, fonts, cx);
        if !self.measured[i] {
            self.heights.set(i, l.paragraph.height.max(self.line_h()));
            self.measured[i] = true;
        }
        l
    }

    /// Lays out the visible lines, recording measured heights.
    pub fn prepare(
        &mut self,
        rope: &Rope,
        fonts: &mut FontRegistry,
        cx: &App,
    ) -> Vec<(usize, Rc<LineLayout>)> {
        let mut visible;
        loop {
            let top = self.anchor.scroll_y(&self.heights);
            visible = self.heights.range(top, top + self.viewport_h as f64);
            let pending: Vec<usize> = visible.clone().filter(|&i| !self.measured[i]).collect();
            if pending.is_empty() {
                break;
            }
            for i in pending {
                self.measure(i, rope, fonts, cx);
            }
        }
        // Keep layouts for a few screens around the viewport.
        let keep = visible.start.saturating_sub(200)..visible.end + 200;
        self.cache.retain(|i, _| keep.contains(i));
        let visible = visible
            .map(|i| (i, self.line(i, rope, fonts, cx)))
            .collect();
        // The sideways scroll limit follows the lines around the view, so it shrinks when
        // the widest line is edited or scrolled far away.
        self.widest = self
            .cache
            .values()
            .map(|l| l.paragraph.width)
            .fold(0.0, f32::max);
        self.hscroll = self.hscroll.min(self.max_hscroll());
        visible
    }

    /// Updates line heights after an edit replaced lines `first..=old_last` of the old text
    /// with lines `first..=new_last` of `rope`.
    pub fn lines_replaced(&mut self, rope: &Rope, first: usize, old_last: usize, new_last: usize) {
        let est: Vec<f32> = (first..=new_last).map(|i| self.estimate(rope, i)).collect();
        let old = first..(old_last + 1).min(self.heights.len());
        self.heights.splice(old.clone(), est);
        self.measured.splice(old, (first..=new_last).map(|_| false));
        let delta = new_last as isize - old_last as isize;
        self.cache = std::mem::take(&mut self.cache)
            .into_iter()
            .filter_map(|(i, l)| {
                if i < first {
                    Some((i, l))
                } else if i > old_last {
                    Some(((i as isize + delta) as usize, l))
                } else {
                    None
                }
            })
            .collect();
        if self.anchor.unit > old_last {
            self.anchor.unit = (self.anchor.unit as isize + delta) as usize;
        }
        self.anchor.clamp(&self.heights);
        self.line_end = None;
    }

    pub fn scroll_y(&self) -> f64 {
        self.anchor.scroll_y(&self.heights)
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.anchor
            .scroll_by(dy as f64, &self.heights, self.viewport_h as f64);
    }

    /// How far the text can scroll sideways: past the widest line by two characters.
    fn max_hscroll(&self) -> f32 {
        if self.soft_wrap {
            return 0.0;
        }
        (self.widest - self.width + 2.0 * self.char_w.map_or(8.0, |c| c.1)).max(0.0)
    }

    /// Scrolls sideways (soft wrap off). Returns whether it moved.
    pub fn scroll_horizontal(&mut self, dx: f32) -> bool {
        if self.soft_wrap {
            return false;
        }
        let before = self.hscroll;
        self.hscroll = (self.hscroll + dx).clamp(0.0, self.max_hscroll());
        self.hscroll != before
    }

    /// The byte offset under window point `at`.
    pub fn hit_test(
        &mut self,
        rope: &Rope,
        fonts: &mut FontRegistry,
        cx: &App,
        at: (f32, f32),
    ) -> usize {
        let y = self.scroll_y() + (at.1 - self.origin.1) as f64;
        if y < 0.0 {
            return 0;
        }
        if y >= self.heights.total() {
            return rope.len_bytes();
        }
        let x = at.0 - self.origin.0 + self.hscroll;
        self.offset_at(rope, fonts, cx, x, y)
    }

    /// The byte offset at document position (`x`, `y`).
    fn offset_at(
        &mut self,
        rope: &Rope,
        fonts: &mut FontRegistry,
        cx: &App,
        x: f32,
        y: f64,
    ) -> usize {
        let (line, _) = self.heights.find(y);
        self.measure(line, rope, fonts, cx);
        // Measuring may have changed the line's height; look it up again.
        let (line, within) = self.heights.find(y);
        let l = self.measure(line, rope, fonts, cx);
        let r = line_range(rope, line);
        let c = Cursor::from_point(&l.paragraph.layout, x, within as f32);
        r.start + c.index().min(r.len())
    }

    /// The caret at byte `b`: document x, document top y and height.
    pub fn caret(
        &mut self,
        rope: &Rope,
        fonts: &mut FontRegistry,
        cx: &App,
        b: usize,
    ) -> (f32, f64, f32) {
        let line = line_of(rope, b);
        let l = self.measure(line, rope, fonts, cx);
        let r = line_range(rope, line);
        let top = self.heights.top(line);
        let affinity = if self.line_end == Some(b) {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        let g = Cursor::from_byte_index(
            &l.paragraph.layout,
            b.saturating_sub(r.start).min(r.len()),
            affinity,
        )
        .geometry(&l.paragraph.layout, 1.0);
        if g.y1 - g.y0 <= 0.0 {
            return (0.0, top, self.line_h());
        }
        (g.x0 as f32, top + g.y0, (g.y1 - g.y0) as f32)
    }

    /// The offset one visual line (or `page` viewports) up or down from `b`, keeping the
    /// goal x.
    pub fn vertical(
        &mut self,
        rope: &Rope,
        fonts: &mut FontRegistry,
        cx: &App,
        b: usize,
        down: bool,
        page: bool,
    ) -> usize {
        let (x, y, h) = self.caret(rope, fonts, cx, b);
        let gx = *self.goal_x.get_or_insert(x);
        let vp = self.viewport_h as f64;
        let target = match (down, page) {
            (true, false) => y + h as f64 + 1.0,
            (true, true) => y + vp,
            (false, false) => y - 1.0,
            (false, true) => y - vp,
        };
        if target < 0.0 {
            return 0;
        }
        if target >= self.heights.total() {
            return rope.len_bytes();
        }
        self.offset_at(rope, fonts, cx, gx, target)
    }

    /// Start or end of the visual (wrapped) line holding `b`.
    pub fn visual_line_edge(
        &mut self,
        rope: &Rope,
        fonts: &mut FontRegistry,
        cx: &App,
        b: usize,
        end: bool,
    ) -> usize {
        let line = line_of(rope, b);
        let l = self.measure(line, rope, fonts, cx);
        let r = line_range(rope, line);
        let at = b - r.start;
        // The caret End left at a line's end is on that line, not the next one.
        let upstream = self.line_end.take() == Some(b);
        let lines: Vec<Range<usize>> = l.paragraph.layout.lines().map(|v| v.text_range()).collect();
        let n = lines.len();
        let Some(k) = lines
            .iter()
            .position(|v| at < v.end || (upstream && at == v.end) || v.end >= r.len())
        else {
            return if end { r.end } else { r.start };
        };
        let v = &lines[k];
        if !end {
            r.start + v.start
        } else if k + 1 == n {
            r.end
        } else {
            // Before the space the line wrapped at, so the caret stays on this line. A line
            // broken inside a word ends after its last character, drawn on this line.
            let edge = r.start + v.end;
            let before = text::prev_grapheme(rope, edge);
            if rope
                .byte_slice(before..edge)
                .chars()
                .all(char::is_whitespace)
            {
                before
            } else {
                self.line_end = Some(edge);
                edge
            }
        }
    }

    /// Scrolls so the caret at the selection head is visible.
    pub fn reveal_head(&mut self, rope: &Rope, fonts: &mut FontRegistry, cx: &App) {
        let (x, y, h) = self.caret(rope, fonts, cx, self.selection.head);
        let top = self.scroll_y();
        let vp = self.viewport_h as f64;
        let margin = self.line_h() as f64;
        if y < top {
            self.anchor
                .scroll_to((y - margin).max(0.0), &self.heights, vp);
        } else if y + h as f64 > top + vp {
            self.anchor
                .scroll_to(y + h as f64 - vp + margin, &self.heights, vp);
        }
        if !self.soft_wrap {
            let m = 4.0 * self.char_w.map_or(8.0, |c| c.1);
            if x < self.hscroll {
                self.hscroll = (x - m).max(0.0);
            } else if x > self.hscroll + self.width - m {
                self.hscroll = x - self.width + m;
            }
        }
    }

    /// The selection as an ordered byte range.
    pub fn range(&self) -> Range<usize> {
        let s = self.selection;
        s.anchor.min(s.head)..s.anchor.max(s.head)
    }

    /// Whether the cursor shows `since` the last input, with blinking on.
    pub fn blink_visible(since: Duration) -> bool {
        since < BLINK_HOLD || ((since - BLINK_HOLD).as_millis() / BLINK_PERIOD.as_millis()) % 2 == 1
    }

    /// Time until the blinking cursor next changes state.
    pub fn blink_delay(since: Duration) -> Duration {
        if since < BLINK_HOLD {
            BLINK_HOLD - since
        } else {
            let p = BLINK_PERIOD.as_millis();
            Duration::from_millis((p - (since - BLINK_HOLD).as_millis() % p) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blink_is_solid_after_input_then_alternates() {
        let ms = Duration::from_millis;
        assert!(Editor::blink_visible(ms(0)));
        assert!(Editor::blink_visible(ms(499)));
        assert!(!Editor::blink_visible(ms(600)));
        assert!(Editor::blink_visible(ms(1100)));
        assert!(!Editor::blink_visible(ms(1600)));
        assert_eq!(Editor::blink_delay(ms(100)), ms(400));
        assert_eq!(Editor::blink_delay(ms(700)), ms(300));
    }
}
