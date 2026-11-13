//! Rendered-text selection across blocks: hit-testing, keyboard extension, and copying as
//! rendered text or as Markdown source.

use std::ops::Range;

use gpui::App;
use mdt_markdown::{Block, BlockKind};
use mdt_text::FontRegistry;
use parley::{Affinity, Cursor};

use super::Reader;
use crate::reader::block::{
    Item, SelectText, TextItem, UnitLayout, fence_object_select, fence_select, source_base,
    table_rows_shown,
};

/// A position in rendered text: a selectable text of a unit, and a byte offset in its
/// text. Ordered in reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub unit: usize,
    /// The selectable text's ordinal in the unit (as `Found::text`), not its index among
    /// the layout's items: images and math that land later add and remove items.
    pub item: usize,
    pub offset: usize,
}

impl Pos {
    pub fn unit_start(unit: usize) -> Self {
        Pos {
            unit,
            item: 0,
            offset: 0,
        }
    }

    pub fn unit_end(unit: usize) -> Self {
        Pos {
            unit,
            item: usize::MAX,
            offset: usize::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Pos,
    pub focus: Pos,
}

impl Selection {
    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// The selected byte range of selectable text `item` of unit `unit`, whose text is
    /// `text`.
    pub fn item_range(&self, unit: usize, item: usize, text: &str) -> Option<Range<usize>> {
        let (s, e) = self.ordered();
        if (unit, item) < (s.unit, s.item) || (unit, item) > (e.unit, e.item) {
            return None;
        }
        let start = if (unit, item) == (s.unit, s.item) {
            text.floor_char_boundary(s.offset)
        } else {
            0
        };
        let end = if (unit, item) == (e.unit, e.item) {
            text.floor_char_boundary(e.offset)
        } else {
            text.len()
        };
        (start < end).then_some(start..end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The selectable texts of a layout: (ordinal, index in `items`, item, text).
fn text_items(items: &[Item]) -> impl Iterator<Item = (usize, usize, &TextItem, &SelectText)> {
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            Item::Text(t) => t.select.as_ref().map(|s| (i, &**t, s)),
            _ => None,
        })
        .enumerate()
        .map(|(k, (i, t, s))| (k, i, t, s))
}

/// The selectable texts of a block in layout order, without laying it out.
pub(crate) fn block_texts(block: &Block, out: &mut Vec<SelectText>) {
    let plain = |text: &str| SelectText {
        text: text.to_owned(),
        objects: Vec::new(),
    };
    match &block.kind {
        BlockKind::Heading { content, .. } | BlockKind::Paragraph { content, .. } => {
            out.push(SelectText::from_inline(content))
        }
        BlockKind::Code { lang, .. }
            if lang
                .as_deref()
                .and_then(mdt_diagram::Engine::from_lang)
                .is_some() =>
        {
            out.push(fence_object_select(block.range.clone()))
        }
        BlockKind::Code { text, .. } | BlockKind::FrontMatter { text, .. } => {
            out.push(plain(text.strip_suffix('\n').unwrap_or(text)))
        }
        BlockKind::Math { latex } => out.push(fence_select(latex, block.range.clone())),
        BlockKind::Html { text } => out.push(plain(text.trim_end_matches('\n'))),
        BlockKind::Rule => {}
        BlockKind::BlockQuote { children, .. } => children.iter().for_each(|b| block_texts(b, out)),
        BlockKind::List { items, .. } => items
            .iter()
            .flat_map(|i| &i.children)
            .for_each(|b| block_texts(b, out)),
        BlockKind::Table { rows, .. } => rows[..table_rows_shown(rows)]
            .iter()
            .flatten()
            .for_each(|c| out.push(SelectText::from_inline(c))),
        BlockKind::Footnotes { notes } => notes
            .iter()
            .flat_map(|n| &n.children)
            .for_each(|b| block_texts(b, out)),
    }
}

impl Reader {
    pub fn select_all(&mut self) {
        let n = self.model.units.len();
        self.selection = (n > 0).then(|| Selection {
            anchor: Pos::unit_start(0),
            focus: Pos::unit_end(n - 1),
        });
    }

    /// The position under window point `at`.
    pub fn hit_test(&mut self, fonts: &mut FontRegistry, cx: &App, at: (f32, f32)) -> Option<Pos> {
        let y = self.anchor.scroll_y(&self.heights) + (at.1 - self.origin.1) as f64;
        self.hit_test_doc(fonts, cx, at.0 - self.origin.0, y)
    }

    /// The position at column x and document y.
    pub fn hit_test_doc(
        &mut self,
        fonts: &mut FontRegistry,
        cx: &App,
        x: f32,
        y: f64,
    ) -> Option<Pos> {
        if self.heights.is_empty() {
            return None;
        }
        let (unit, within) = self.heights.find(y.max(0.0));
        let l = self.layout(unit, fonts, cx);
        // Below the document: the bottom of the last unit, so the nearest item is the last
        // one rather than whichever is nearest sideways.
        let within = if y > self.heights.total() {
            l.height
        } else {
            within as f32
        };
        let shift = |i: usize| self.item_shift(&l, unit, i);
        // Prefer an item whose lines span y; otherwise the vertically nearest.
        let best = text_items(&l.items).min_by(|a, b| {
            let d = |t: &TextItem, i: usize| {
                let dy = if within < t.y {
                    t.y - within
                } else if within > t.y + t.paragraph.height {
                    within - t.y - t.paragraph.height
                } else {
                    0.0
                };
                let x = x + shift(i);
                let dx = if x < t.x {
                    t.x - x
                } else {
                    (x - t.x - t.paragraph.width).max(0.0)
                };
                (dy, dx)
            };
            d(a.2, a.1)
                .partial_cmp(&d(b.2, b.1))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let Some((item, i, t, _)) = best else {
            return Some(Pos::unit_start(unit));
        };
        let cursor = Cursor::from_point(&t.paragraph.layout, x + shift(i) - t.x, within - t.y);
        Some(Pos {
            unit,
            item,
            offset: cursor.index(),
        })
    }

    /// How far item `i` of unit `unit`'s layout `l` is scrolled sideways.
    fn item_shift(&self, l: &UnitLayout, unit: usize, i: usize) -> f32 {
        l.regions
            .iter()
            .position(|r| r.items.contains(&i))
            .map_or(0.0, |n| {
                self.hscroll.get(&(unit, n)).copied().unwrap_or(0.0)
            })
    }

    /// Moves the selection focus one character or line, extending the selection.
    pub fn extend(&mut self, fonts: &mut FontRegistry, cx: &App, dir: Direction) {
        let Some(sel) = self.selection else { return };
        let Some(focus) = self.normalize(fonts, cx, sel.focus) else {
            return;
        };
        let l = self.layout(focus.unit, fonts, cx);
        let forward = matches!(dir, Direction::Right | Direction::Down);
        let Some((_, i, t, s)) = text_items(&l.items).find(|(k, ..)| *k == focus.item) else {
            // A unit without text (a rule): on to the neighbouring text.
            if let (Some(next), Some(sel)) = (
                self.neighbour(fonts, cx, focus, forward),
                self.selection.as_mut(),
            ) {
                sel.focus = next;
            }
            return;
        };
        let layout = &t.paragraph.layout;
        let cursor = Cursor::from_byte_index(layout, focus.offset, Affinity::Downstream);
        let next = match dir {
            Direction::Left if focus.offset > 0 => Some(Pos {
                offset: cursor.previous_visual(layout).index(),
                ..focus
            }),
            Direction::Right if focus.offset < s.text.len() => Some(Pos {
                offset: cursor.next_visual(layout).index(),
                ..focus
            }),
            Direction::Left => self.neighbour(fonts, cx, focus, false),
            Direction::Right => self.neighbour(fonts, cx, focus, true),
            Direction::Up | Direction::Down => {
                let g = cursor.geometry(layout, 1.0);
                let top = self.heights.top(focus.unit) + (t.y + g.y0 as f32) as f64;
                let y = if dir == Direction::Up {
                    top - 1.0
                } else {
                    top + (g.y1 - g.y0) + 1.0
                };
                // In column coordinates: `hit_test_doc` adds the sideways scroll back.
                let x = t.x + g.x0 as f32 - self.item_shift(&l, focus.unit, i);
                let y = self.skip_gap(fonts, cx, y, dir == Direction::Down);
                if y < 0.0 {
                    Some(Pos::unit_start(0))
                } else {
                    self.hit_test_doc(fonts, cx, x, y)
                }
            }
        };
        if let (Some(focus), Some(sel)) = (next, self.selection.as_mut()) {
            sel.focus = focus;
        }
    }

    /// Moves document y out of the gap below a unit's text: down to the next unit's top, or
    /// up to the unit's last text line.
    fn skip_gap(&mut self, fonts: &mut FontRegistry, cx: &App, y: f64, down: bool) -> f64 {
        if y < 0.0 || y >= self.heights.total() {
            return y;
        }
        let (unit, within) = self.heights.find(y);
        let l = self.layout(unit, fonts, cx);
        let bottom = text_items(&l.items)
            .map(|(_, _, t, _)| t.y + t.paragraph.height)
            .fold(0.0, f32::max) as f64;
        if within < bottom {
            y
        } else if down {
            self.heights.top(unit + 1) + 1.0
        } else {
            self.heights.top(unit) + bottom - 1.0
        }
    }

    /// A position clamped onto a selectable item (unit ends become their last item's end).
    fn normalize(&mut self, fonts: &mut FontRegistry, cx: &App, p: Pos) -> Option<Pos> {
        let l = self.layout(p.unit, fonts, cx);
        let items: Vec<(usize, &str)> = text_items(&l.items)
            .map(|(k, _, _, s)| (k, s.text.as_str()))
            .collect();
        if let Some(&(k, text)) = items.iter().find(|(k, _)| *k == p.item) {
            return Some(Pos {
                item: k,
                offset: text.floor_char_boundary(p.offset),
                ..p
            });
        }
        match items
            .iter()
            .rev()
            .find(|(k, _)| *k < p.item)
            .or(items.first())
        {
            Some(&(k, text)) => Some(Pos {
                unit: p.unit,
                item: k,
                offset: if k < p.item { text.len() } else { 0 },
            }),
            None => Some(p),
        }
    }

    /// The end of the previous selectable item or the start of the next one.
    fn neighbour(
        &mut self,
        fonts: &mut FontRegistry,
        cx: &App,
        p: Pos,
        forward: bool,
    ) -> Option<Pos> {
        let mut unit = p.unit;
        let mut after = Some(p.item);
        loop {
            let l = self.layout(unit, fonts, cx);
            let items: Vec<(usize, usize)> = text_items(&l.items)
                .map(|(k, _, _, s)| (k, s.text.len()))
                .collect();
            let found = if forward {
                items
                    .iter()
                    .find(|(i, _)| after.is_none_or(|a| *i > a))
                    .map(|&(i, _)| Pos {
                        unit,
                        item: i,
                        offset: 0,
                    })
            } else {
                items
                    .iter()
                    .rev()
                    .find(|(i, _)| after.is_none_or(|a| *i < a))
                    .map(|&(i, len)| Pos {
                        unit,
                        item: i,
                        offset: len,
                    })
            };
            if found.is_some() {
                return found;
            }
            if forward && unit + 1 < self.model.units.len() {
                unit += 1;
            } else if !forward && unit > 0 {
                unit -= 1;
            } else {
                return None;
            }
            after = None;
        }
    }

    /// The selection as rendered plain text: blocks separated by blank lines, math as its
    /// source with delimiters.
    pub fn selected_text(&mut self, fonts: &mut FontRegistry, cx: &App, source: &str) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        let (s, e) = sel.ordered();
        let mut blocks = Vec::new();
        for unit in s.unit..=e.unit.min(self.model.units.len().saturating_sub(1)) {
            let whole = s <= Pos::unit_start(unit) && e >= Pos::unit_end(unit);
            let mut parts = Vec::new();
            if whole {
                let mut texts = Vec::new();
                block_texts(&self.model.units[unit].block, &mut texts);
                parts.extend(texts.iter().map(|t| t.copy(0..t.text.len(), source)));
            } else {
                // The layout's source ranges are relative to the unit.
                let unit_source = source.get(source_base(&self.model.units[unit])..);
                let l = self.layout(unit, fonts, cx);
                for (k, _, _, t) in text_items(&l.items) {
                    if let Some(r) = sel.item_range(unit, k, &t.text) {
                        parts.push(t.copy(r, unit_source.unwrap_or("")));
                    }
                }
            }
            if !parts.is_empty() {
                blocks.push(parts.join("\n"));
            }
        }
        blocks.join("\n\n")
    }

    /// The source of every block the selection touches.
    pub fn selected_markdown(&self, source: &str) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        let (s, e) = sel.ordered();
        let ranges = self
            .model
            .units
            .get(s.unit..=e.unit.min(self.model.units.len().saturating_sub(1)));
        let Some(ranges) = ranges else {
            return String::new();
        };
        let ranges: Vec<_> = ranges
            .iter()
            .map(|u| u.range.clone())
            .filter(|r| !r.is_empty())
            .collect();
        match (ranges.first(), ranges.last()) {
            (Some(a), Some(b)) => source
                .get(a.start..b.end)
                .unwrap_or("")
                .trim_end_matches('\n')
                .to_owned(),
            _ => String::new(),
        }
    }
}
