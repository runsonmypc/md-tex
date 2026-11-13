//! A laid-out paragraph as a flat list of paint operations in logical pixels, relative to
//! the paragraph's top-left. GPUI painting (and the theme colour lookup for each brush)
//! happens in one place on top of this list, which keeps it testable without a window.

use gpui::FontId;
use parley::{Layout, PositionedLayoutItem};

use crate::fonts::FontRegistry;

/// A palette slot, resolved to a theme colour at paint time so layouts survive theme
/// switches: layouts do not depend on the theme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Brush(pub u16);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaintOp {
    /// `origin` is the glyph's baseline origin.
    Glyph {
        font_id: FontId,
        glyph: u32,
        x: f32,
        y: f32,
        size: f32,
        brush: Brush,
    },
    /// A glyph from a colour font (emoji), painted in its own colours.
    Emoji {
        font_id: FontId,
        glyph: u32,
        x: f32,
        y: f32,
        size: f32,
    },
    /// A glyph no font has: drawn as an outlined box the size of its advance.
    MissingGlyph {
        rect: Rect,
        brush: Brush,
    },
    /// Space reserved for an inline object (math, image), painted by its owner.
    InlineBox {
        id: u64,
        rect: Rect,
    },
    Underline {
        rect: Rect,
        brush: Brush,
    },
    Strikethrough {
        rect: Rect,
        brush: Brush,
    },
}

pub fn paint_ops(layout: &Layout<Brush>, fonts: &FontRegistry) -> Vec<PaintOp> {
    let mut ops = Vec::new();
    for line in layout.lines() {
        for item in line.items() {
            match item {
                PositionedLayoutItem::GlyphRun(run) => {
                    let style = run.style();
                    let brush = style.brush;
                    let r = run.run();
                    let size = r.font_size();
                    let metrics = r.font_metrics();
                    let font_id = fonts.font_id(r.font());
                    let colour = fonts.is_colour(r.font());
                    let baseline = run.baseline();
                    for g in run.positioned_glyphs() {
                        match font_id {
                            Some(font_id) if g.id != 0 && colour => ops.push(PaintOp::Emoji {
                                font_id,
                                glyph: g.id,
                                x: g.x,
                                y: g.y,
                                size,
                            }),
                            Some(font_id) if g.id != 0 => ops.push(PaintOp::Glyph {
                                font_id,
                                glyph: g.id,
                                x: g.x,
                                y: g.y,
                                size,
                                brush,
                            }),
                            // Glyph 0 is .notdef: no font covers the character. An unmapped
                            // font means registration failed; show the same box.
                            _ => {
                                let h = metrics.ascent * 0.8;
                                let inset = (g.advance * 0.1).min(1.5);
                                ops.push(PaintOp::MissingGlyph {
                                    rect: Rect {
                                        x: g.x + inset,
                                        y: g.y - h,
                                        w: (g.advance - 2.0 * inset).max(1.0),
                                        h,
                                    },
                                    brush,
                                })
                            }
                        }
                    }
                    let x = run.offset();
                    let w = run.advance();
                    if let Some(u) = &style.underline {
                        let offset = u.offset.unwrap_or(metrics.underline_offset);
                        let size = u.size.unwrap_or(metrics.underline_size);
                        ops.push(PaintOp::Underline {
                            rect: Rect {
                                x,
                                y: baseline - offset,
                                w,
                                h: size,
                            },
                            brush: u.brush,
                        });
                    }
                    if let Some(s) = &style.strikethrough {
                        let offset = s.offset.unwrap_or(metrics.strikethrough_offset);
                        let size = s.size.unwrap_or(metrics.strikethrough_size);
                        ops.push(PaintOp::Strikethrough {
                            rect: Rect {
                                x,
                                y: baseline - offset,
                                w,
                                h: size,
                            },
                            brush: s.brush,
                        });
                    }
                }
                PositionedLayoutItem::InlineBox(b) => ops.push(PaintOp::InlineBox {
                    id: b.id,
                    rect: Rect {
                        x: b.x,
                        y: b.y,
                        w: b.width,
                        h: b.height,
                    },
                }),
            }
        }
    }
    ops
}

/// Selection highlight rectangles for a byte range of the paragraph.
pub fn selection_rects(layout: &Layout<Brush>, range: std::ops::Range<usize>) -> Vec<Rect> {
    use parley::{Affinity, Selection};
    let anchor = Selection::from_byte_index(layout, range.start, Affinity::Downstream);
    let focus = Selection::from_byte_index(layout, range.end, Affinity::Upstream).focus();
    anchor
        .extend(focus)
        .geometry(layout)
        .into_iter()
        .map(|(b, _)| Rect {
            x: b.x0 as f32,
            y: b.y0 as f32,
            w: (b.x1 - b.x0) as f32,
            h: (b.y1 - b.y0) as f32,
        })
        .collect()
}

/// `selection_rects` for many byte ranges at once (a paragraph's links). Each of those
/// calls walks every cluster of the lines it touches, so a line of thousands of links took
/// quadratic time; this measures each cluster once and finds a range's clusters by binary
/// search. Visually adjacent clusters make one rectangle; an inline box counts with the
/// cluster after it, as in a selection.
pub fn ranges_rects(layout: &Layout<Brush>, ranges: &[std::ops::Range<usize>]) -> Vec<Vec<Rect>> {
    struct Line {
        text: std::ops::Range<usize>,
        top: f32,
        bottom: f32,
        /// Each cluster's left and right edges, in visual order.
        x: Vec<(f32, f32)>,
        /// (byte offset, visual index) of each cluster, by offset.
        by_byte: Vec<(usize, usize)>,
    }
    if ranges.is_empty() {
        return Vec::new();
    }
    // Only the lines a range reaches are measured: a few matches in view of a 2 MB
    // paragraph should not measure all of it.
    let reach = ranges.iter().map(|r| r.start).min().unwrap_or(0)
        ..ranges.iter().map(|r| r.end).max().unwrap_or(0);
    let lines: Vec<Line> = layout
        .lines()
        .map(|line| {
            let m = line.metrics();
            let (mut x, mut by_byte) = (Vec::new(), Vec::new());
            let text = line.text_range();
            if text.end <= reach.start || text.start >= reach.end {
                return Line {
                    text,
                    top: 0.0,
                    bottom: 0.0,
                    x,
                    by_byte,
                };
            }
            let mut last_run = None;
            let mut box_start = None;
            for item in line.items() {
                match item {
                    PositionedLayoutItem::GlyphRun(g) => {
                        // A run split by style comes as several glyph runs.
                        let run = g.run();
                        if last_run.as_ref() == Some(&run.text_range()) {
                            continue;
                        }
                        last_run = Some(run.text_range());
                        let mut right = g.offset();
                        for c in run.visual_clusters() {
                            let left = box_start.take().unwrap_or(right);
                            right += c.advance();
                            by_byte.push((c.text_range().start, x.len()));
                            x.push((left, right));
                        }
                    }
                    // As in a selection, a box that starts the line is left out.
                    PositionedLayoutItem::InlineBox(b) if last_run.is_some() => {
                        box_start.get_or_insert(b.x);
                    }
                    PositionedLayoutItem::InlineBox(_) => {}
                }
            }
            by_byte.sort_unstable();
            Line {
                text: line.text_range(),
                top: m.block_min_coord.min(m.content_block_min_coord),
                bottom: m.block_max_coord.max(m.content_block_max_coord),
                x,
                by_byte,
            }
        })
        .collect();
    ranges
        .iter()
        .map(|range| {
            let mut rects = Vec::new();
            let first = lines.partition_point(|l| l.text.end <= range.start);
            for line in lines[first..]
                .iter()
                .take_while(|l| l.text.start < range.end)
            {
                let from = line.by_byte.partition_point(|&(b, _)| b < range.start);
                let to = line.by_byte.partition_point(|&(b, _)| b < range.end);
                let mut visual: Vec<usize> =
                    line.by_byte[from..to].iter().map(|&(_, i)| i).collect();
                visual.sort_unstable();
                for group in visual.chunk_by(|a, b| b == &(a + 1)) {
                    let (left, right) = (line.x[group[0]].0, line.x[group[group.len() - 1]].1);
                    if right > left {
                        rects.push(Rect {
                            x: left,
                            y: line.top,
                            w: right - left,
                            h: line.bottom - line.top,
                        });
                    }
                }
            }
            rects
        })
        .collect()
}

/// The bytes of `layout` that may show inside `visible` (paragraph coordinates): from the
/// first to the last cluster that reaches into it, on the lines it crosses. A range outside
/// them has no part in view.
pub fn visible_bytes(layout: &Layout<Brush>, visible: &Rect) -> std::ops::Range<usize> {
    let (mut first, mut last) = (usize::MAX, 0);
    let (left, right) = (visible.x, visible.x + visible.w);
    for line in layout.lines() {
        let m = line.metrics();
        let (top, bottom) = (
            m.block_min_coord.min(m.content_block_min_coord),
            m.block_max_coord.max(m.content_block_max_coord),
        );
        if bottom < visible.y || top > visible.y + visible.h {
            continue;
        }
        let text = line.text_range();
        if m.offset >= left && m.offset + m.advance <= right {
            (first, last) = (first.min(text.start), last.max(text.end));
            continue;
        }
        let mut last_run = None;
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(g) = item else {
                continue;
            };
            let run = g.run();
            if last_run.as_ref() == Some(&run.text_range()) {
                continue;
            }
            last_run = Some(run.text_range());
            let mut x = g.offset();
            if x > right || x + run.advance() < left {
                continue;
            }
            for c in run.visual_clusters() {
                let next = x + c.advance();
                if next >= left && x <= right {
                    let t = c.text_range();
                    (first, last) = (first.min(t.start), last.max(t.end));
                }
                x = next;
            }
        }
    }
    if first > last { 0..0 } else { first..last }
}

fn bounds(origin: gpui::Point<gpui::Pixels>, r: &Rect) -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        origin + gpui::point(gpui::px(r.x), gpui::px(r.y)),
        gpui::size(gpui::px(r.w), gpui::px(r.h)),
    )
}

/// Paints selection rectangles, then the ops, at `origin`. `color` resolves brushes to
/// theme colours. Inline boxes are left to their owners. Ops outside the content mask are
/// skipped: painting every glyph of a 1 MB paragraph took 50 ms a frame.
pub fn paint(
    ops: &[PaintOp],
    selection: &[Rect],
    selection_color: gpui::Hsla,
    origin: gpui::Point<gpui::Pixels>,
    color: impl Fn(Brush) -> gpui::Hsla,
    window: &mut gpui::Window,
) {
    use gpui::{GlyphId, point, px};
    let mask = window.content_mask().bounds;
    let visible = Rect {
        x: f32::from(mask.origin.x - origin.x),
        y: f32::from(mask.origin.y - origin.y),
        w: f32::from(mask.size.width),
        h: f32::from(mask.size.height),
    };
    for r in selection {
        window.paint_quad(gpui::fill(bounds(origin, r), selection_color));
    }
    for op in ops.iter().filter(|op| may_show(op, &visible)) {
        match op {
            PaintOp::Glyph {
                font_id,
                glyph,
                x,
                y,
                size,
                brush,
            } => {
                // A glyph that cannot be rasterized is skipped rather than failing the frame.
                let _ = window.paint_glyph(
                    origin + point(px(*x), px(*y)),
                    *font_id,
                    GlyphId(*glyph),
                    px(*size),
                    color(*brush),
                );
            }
            PaintOp::Emoji {
                font_id,
                glyph,
                x,
                y,
                size,
            } => {
                let _ = window.paint_emoji(
                    origin + point(px(*x), px(*y)),
                    *font_id,
                    GlyphId(*glyph),
                    px(*size),
                );
            }
            PaintOp::MissingGlyph { rect, brush } => {
                window.paint_quad(gpui::outline(
                    bounds(origin, rect),
                    color(*brush),
                    gpui::BorderStyle::Solid,
                ));
            }
            PaintOp::Underline { rect, brush } | PaintOp::Strikethrough { rect, brush } => {
                window.paint_quad(gpui::fill(bounds(origin, rect), color(*brush)));
            }
            PaintOp::InlineBox { .. } => {}
        }
    }
}

/// Whether `op` may draw inside `visible` (paragraph coordinates). A glyph's ink is taken
/// to reach 3 em above its baseline origin and 2 em below, 2 em before it and 16 em after
/// it (wide ligatures); rectangles are exact.
pub(crate) fn may_show(op: &PaintOp, visible: &Rect) -> bool {
    let overlaps = |r: &Rect| {
        r.x <= visible.x + visible.w
            && r.x + r.w >= visible.x
            && r.y <= visible.y + visible.h
            && r.y + r.h >= visible.y
    };
    match op {
        PaintOp::Glyph { x, y, size, .. } | PaintOp::Emoji { x, y, size, .. } => overlaps(&Rect {
            x: x - 2.0 * size,
            y: y - 3.0 * size,
            w: 18.0 * size,
            h: 5.0 * size,
        }),
        PaintOp::MissingGlyph { rect, .. }
        | PaintOp::Underline { rect, .. }
        | PaintOp::Strikethrough { rect, .. } => overlaps(rect),
        PaintOp::InlineBox { .. } => true,
    }
}
