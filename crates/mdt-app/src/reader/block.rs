//! Lays out one layout unit into positioned paragraphs and shapes, in logical pixels at
//! the given zoom. The result does not depend on the theme: colours are brushes that the
//! painter resolves.

use std::sync::Arc;

use mdt_markdown::{
    AlertKind, Block, BlockKind, Inline, InlineObject, LayoutUnit, OBJECT_CHAR, ObjectKind,
    Segment, SpanStyle,
};
use mdt_math::MathScene;
use mdt_text::{
    BaseStyle, Brush, FontRegistry, InlineBoxSpec, PaintOp, Paragraph, StyleRun, layout_cell,
    layout_paragraph, paint_ops,
};
use parley::LayoutContext;

use super::diagrams::{self, DiagramKey, Diagrams};
use super::images::{self, Images};
use super::math::{MathView, Shown};

pub mod brush {
    use mdt_text::Brush;
    pub const TEXT: Brush = Brush(0);
    pub const MUTED: Brush = Brush(1);
    pub const LINK: Brush = Brush(2);
    pub const CODE: Brush = Brush(3);
    pub const CODE_BG: Brush = Brush(4);
    pub const RULE: Brush = Brush(5);
    /// Alerts: note, tip, important, warning, caution.
    pub const ALERT: [Brush; 5] = [Brush(10), Brush(11), Brush(12), Brush(13), Brush(14)];
    /// Code highlight classes start here, in `mdt_highlight::Class` order.
    pub const SYNTAX_BASE: u16 = 20;
}

/// Base sizes at zoom 1, in logical pixels.
pub const BODY_SIZE: f32 = 16.0;
pub const CODE_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 1.5;
const GAP: f32 = 16.0;
const INDENT: f32 = 26.0;
const QUOTE_BAR: f32 = 4.0;
const QUOTE_PAD: f32 = 14.0;
const CODE_PAD: f32 = 12.0;
/// Padding around a table cell's text, each side.
const CELL_PAD: f32 = 8.0;
/// Footnote text size.
const FOOTNOTE_SIZE: f32 = 14.0;
/// Side of the broken-image mark in a placeholder, at zoom 1.
pub const PLACEHOLDER_ICON: f32 = 20.0;
const HEADING_SCALE: [f32; 6] = [2.0, 1.5, 1.25, 1.0, 0.875, 0.85];

pub struct TextItem {
    pub x: f32,
    pub y: f32,
    pub paragraph: Paragraph,
    pub ops: Vec<PaintOp>,
    /// Laid out without wrapping (code); wider content scrolls in its region.
    pub scrolls: bool,
    /// Rendered text the user can select and copy; `None` for list markers and labels.
    pub select: Option<SelectText>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SelectText {
    pub text: String,
    /// Inline objects: (rendered offset of the object character, source range). Copy
    /// replaces the character with the source (math keeps its delimiters). In a unit's
    /// layout the ranges are relative to the unit's start (`layout_unit`).
    pub objects: Vec<(usize, std::ops::Range<usize>)>,
}

impl SelectText {
    pub fn from_inline(inline: &Inline) -> Self {
        Self {
            text: inline.text.clone(),
            objects: inline
                .objects
                .iter()
                .map(|o| (o.at, o.source.clone()))
                .collect(),
        }
    }

    /// The rendered text of `range` for the clipboard.
    pub fn copy(&self, range: std::ops::Range<usize>, source: &str) -> String {
        let t = &self.text;
        let range = t.floor_char_boundary(range.start)..t.floor_char_boundary(range.end);
        let mut out = String::new();
        let mut at = range.start;
        for (pos, src) in &self.objects {
            if (range.start..range.end).contains(pos) {
                out.push_str(&self.text[at..*pos]);
                out.push_str(source.get(src.clone()).unwrap_or(""));
                at = pos + mdt_markdown::OBJECT_CHAR.len_utf8();
            }
        }
        out.push_str(&self.text[at.min(range.end)..range.end]);
        out
    }
}

pub enum Item {
    Text(Box<TextItem>),
    Fill {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        brush: Brush,
    },
    Checkbox {
        x: f32,
        y: f32,
        size: f32,
        checked: bool,
        /// The `[ ]` in the source, relative to the unit's start.
        source: std::ops::Range<usize>,
    },
    /// A typeset equation: its scene (in points at the reference size) scaled by `scale`,
    /// drawn in the theme's text colour.
    Math {
        x: f32,
        y: f32,
        scale: f32,
        font: gpui::FontId,
        scene: Arc<MathScene>,
    },
    /// An image's frame; the raster for `source` fills it once decoded.
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        source: mdt_image::Source,
    },
    /// A rendered diagram's frame; its raster for the theme fills it.
    Diagram {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        key: DiagramKey,
    },
    /// The outline and broken-image mark of an image placeholder; its text (alt text,
    /// reference, reason) is a separate text item.
    Placeholder {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        text: String,
    },
}

pub struct UnitLayout {
    pub height: f32,
    pub items: Vec<Item>,
    /// Blocks that scroll horizontally: code, HTML blocks, display math wider than the
    /// column, and tables (a top-level table's region may reach past the column into the
    /// page).
    pub regions: Vec<Region>,
    /// Areas that show a message on hover: math errors, and links with their targets.
    pub hovers: Vec<Hover>,
    /// Remote-image placeholders: clicking one loads its URL.
    pub loads: Vec<Load>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hover {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub message: String,
    /// Clicking follows this link.
    pub link: Option<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// A link destination as written: `#anchor`, a path, or a URL.
    Url(String),
    /// A footnote reference, by label: goes to the footnote.
    Footnote(String),
    /// A footnote's number in the footnotes section: back to its first reference.
    FootnoteBack(String),
}

/// A horizontally scrollable block: `items` shift left by the region's scroll offset and
/// are clipped to its bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub items: std::ops::Range<usize>,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub content_w: f32,
}

impl Region {
    pub fn max_scroll(&self) -> f32 {
        (self.content_w - self.w).max(0.0)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Size of an equation still being typeset: (width, height, baseline from top).
fn pending_math_size(latex: &str, display: bool, font_size: f32) -> (f32, f32, f32) {
    if display {
        (font_size * 6.0, font_size * 2.5, font_size * 1.6)
    } else {
        (
            (latex.chars().count() as f32 * 0.45 * font_size).max(font_size),
            font_size,
            font_size * 0.8,
        )
    }
}

pub struct Ctx<'a> {
    pub fonts: &'a mut FontRegistry,
    pub lcx: &'a mut LayoutContext<Brush>,
    pub zoom: f32,
    pub highlights: &'a mdt_highlight::Cache,
    pub math: &'a mut MathView,
    pub images: &'a mut Images,
    pub diagrams: &'a mut Diagrams,
    /// Code blocks laid out without highlighting because it is not computed yet.
    pub missing: Vec<(String, String)>,
    /// The page a top-level table may widen into: (x relative to the column, width).
    /// `None` keeps tables inside the column.
    pub page: Option<(f32, f32)>,
    /// The unit being laid out and the number of equations met in it so far.
    unit: usize,
    math_n: usize,
    diagram_n: usize,
}

impl<'a> Ctx<'a> {
    pub fn new(
        fonts: &'a mut FontRegistry,
        lcx: &'a mut LayoutContext<Brush>,
        zoom: f32,
        highlights: &'a mdt_highlight::Cache,
        math: &'a mut MathView,
        images: &'a mut Images,
        diagrams: &'a mut Diagrams,
    ) -> Self {
        Self {
            fonts,
            lcx,
            zoom,
            highlights,
            math,
            images,
            diagrams,
            missing: Vec::new(),
            page: None,
            unit: 0,
            math_n: 0,
            diagram_n: 0,
        }
    }
}

struct Out {
    items: Vec<Item>,
    regions: Vec<Region>,
    hovers: Vec<Hover>,
    loads: Vec<Load>,
}

/// How an inline object is drawn in its box.
enum Shape {
    /// `pad` above the scene inside the box; `w` the scaled scene width (centered).
    Math {
        scene: Arc<MathScene>,
        scale: f32,
        pad: f32,
        w: f32,
    },
    /// The source in error styling, `pad` below the top of the box.
    Error(Box<Paragraph>, String, f32),
    Image(mdt_image::Source),
    Diagram {
        key: DiagramKey,
        w: f32,
        h: f32,
    },
    /// A diagram's source as a code block (`pad` inside its background), with the error
    /// message below it when rendering failed.
    Code(Box<Paragraph>, Option<Box<Paragraph>>, f32),
    /// An image placeholder: its text and layout inset by `pad`, and the URL a click loads.
    Placeholder(Box<Paragraph>, String, f32, Option<String>),
    Blank,
}

struct Obj {
    w: f32,
    h: f32,
    baseline: f32,
    shape: Shape,
}

fn boxes(inline: &Inline, objects: &[Obj]) -> Vec<InlineBoxSpec> {
    inline
        .objects
        .iter()
        .zip(objects)
        .enumerate()
        .map(|(i, (o, b))| InlineBoxSpec {
            id: i as u64,
            at: o.at,
            width: b.w,
            height: b.h,
            baseline: b.baseline,
        })
        .collect()
}

impl Ctx<'_> {
    fn z(&self, v: f32) -> f32 {
        v * self.zoom
    }

    /// Lays out inline content at (x, y), returns its height.
    #[allow(clippy::too_many_arguments)]
    fn inline(
        &mut self,
        out: &mut Out,
        inline: &Inline,
        x: f32,
        y: f32,
        width: f32,
        base: BaseStyle,
        scrolls: bool,
    ) -> f32 {
        let objects = self.objects(inline, base.size, width, width);
        self.inline_with(out, inline, objects, x, y, width, base, scrolls)
    }

    /// `inline` with its objects already resolved.
    #[allow(clippy::too_many_arguments)]
    fn inline_with(
        &mut self,
        out: &mut Out,
        inline: &Inline,
        objects: Vec<Obj>,
        x: f32,
        y: f32,
        width: f32,
        base: BaseStyle,
        scrolls: bool,
    ) -> f32 {
        let wide = !scrolls && objects.iter().any(|o| o.w > width);
        let runs = style_runs(inline);
        let max_width = if scrolls { f32::INFINITY } else { width };
        let paragraph = layout_paragraph(
            self.fonts,
            self.lcx,
            &inline.text,
            base,
            &runs,
            &boxes(inline, &objects),
            max_width,
        );
        let ops = paint_ops(&paragraph.layout, self.fonts);
        let (h, content_w) = (paragraph.height, paragraph.width);
        link_hovers(out, inline, &paragraph, x, y);
        let first = out.items.len();
        self.place(out, &ops, objects, x, y);
        out.items.push(Item::Text(Box::new(TextItem {
            x,
            y,
            paragraph,
            ops,
            scrolls,
            select: Some(SelectText::from_inline(inline)),
        })));
        // Display math wider than the column scrolls with its paragraph.
        if wide {
            out.regions.push(Region {
                items: first..out.items.len(),
                x,
                y,
                w: width,
                h,
                content_w,
            });
        }
        h
    }

    /// Sizes and resolves the inline objects. Display math takes a line of its own: its
    /// box is at least `line_w` wide, with the scene centered in it. Images fit `max_w`.
    fn objects(&mut self, inline: &Inline, size: f32, line_w: f32, max_w: f32) -> Vec<Obj> {
        let mut objects = Vec::new();
        for o in &inline.objects {
            let (latex, display) = match &o.kind {
                ObjectKind::Math { latex, display } => (latex, display),
                ObjectKind::Image {
                    src,
                    alt,
                    width,
                    height,
                    ..
                } => {
                    let obj = self.image(src, alt, (*width, *height), size, max_w);
                    objects.push(obj);
                    continue;
                }
                ObjectKind::HtmlBreak { .. } => {
                    objects.push(Obj {
                        w: 0.0,
                        h: 0.0,
                        baseline: 0.0,
                        shape: Shape::Blank,
                    });
                    continue;
                }
            };
            let display = *display;
            let slot = (self.unit, self.math_n);
            self.math_n += 1;
            let size = if display { self.z(BODY_SIZE) } else { size };
            let line_w = if display { line_w } else { 0.0 };
            let pad = if display { size * 0.5 } else { 0.0 };
            objects.push(match self.math.resolve(slot, latex, display) {
                Shown::Scene(scene) => {
                    let scale = size / mdt_math::REFERENCE_SIZE;
                    let w = scene.width * scale;
                    Obj {
                        w: w.max(line_w),
                        h: scene.height * scale + 2.0 * pad,
                        baseline: scene.baseline * scale + pad,
                        shape: Shape::Math {
                            scene,
                            scale,
                            pad,
                            w,
                        },
                    }
                }
                Shown::Error(message) => {
                    let delim = if display { "$$" } else { "$" };
                    let base = BaseStyle {
                        size: size * 0.9,
                        line_height: LINE_HEIGHT,
                        mono: true,
                        bold: false,
                        brush: brush::ALERT[4],
                    };
                    let text = format!("{delim}{}{delim}", latex.trim());
                    let p = layout_paragraph(
                        self.fonts,
                        self.lcx,
                        &text,
                        base,
                        &[],
                        &[],
                        f32::INFINITY,
                    );
                    let baseline = p
                        .layout
                        .lines()
                        .next()
                        .map_or(p.height, |l| l.metrics().baseline);
                    Obj {
                        w: p.width.max(line_w),
                        h: p.height + 2.0 * pad,
                        baseline: baseline + pad,
                        shape: Shape::Error(Box::new(p), message, pad),
                    }
                }
                Shown::Pending => {
                    let (w, h, baseline) = pending_math_size(latex, display, size);
                    Obj {
                        w: w.max(line_w),
                        h,
                        baseline,
                        shape: Shape::Blank,
                    }
                }
            });
        }
        objects
    }

    /// A diagram fence's box, the column's width: the diagram (scaled down to fit), or its
    /// source as code, in error styling with the message when rendering failed.
    fn diagram(&mut self, engine: mdt_diagram::Engine, text: &str, width: f32) -> Obj {
        let slot = (self.unit, self.diagram_n);
        self.diagram_n += 1;
        let (error, pending) = match self.diagrams.resolve(slot, engine, text) {
            diagrams::Shown::Diagram { key, w, h } => {
                let (dw, dh) = (w * self.zoom, h * self.zoom);
                let (dw, dh) = if dw > width {
                    (width, dh * width / dw)
                } else {
                    (dw, dh)
                };
                return Obj {
                    w: width,
                    h: dh,
                    baseline: dh,
                    shape: Shape::Diagram { key, w: dw, h: dh },
                };
            }
            diagrams::Shown::Error(e) => (Some(e), false),
            diagrams::Shown::Pending => (None, true),
        };
        let pad = self.z(CODE_PAD);
        let code_brush = if pending {
            brush::MUTED
        } else {
            brush::ALERT[4]
        };
        let base = BaseStyle {
            size: self.z(CODE_SIZE),
            line_height: LINE_HEIGHT,
            mono: true,
            bold: false,
            brush: code_brush,
        };
        let code = text.strip_suffix('\n').unwrap_or(text);
        let inner = (width - 2.0 * pad).max(1.0);
        let code = layout_paragraph(self.fonts, self.lcx, code, base, &[], &[], inner);
        let mut h = code.height + 2.0 * pad;
        let message = error.map(|e| {
            let base = BaseStyle {
                size: self.z(14.0),
                mono: false,
                brush: brush::ALERT[4],
                ..base
            };
            let p = layout_paragraph(self.fonts, self.lcx, &e, base, &[], &[], width);
            h += self.z(4.0) + p.height;
            Box::new(p)
        });
        Obj {
            w: width,
            h,
            baseline: h,
            shape: Shape::Code(Box::new(code), message, pad),
        }
    }

    /// An image's box: its frame once the header is read, or a placeholder.
    fn image(
        &mut self,
        src: &str,
        alt: &str,
        attr: (Option<u32>, Option<u32>),
        size: f32,
        max_w: f32,
    ) -> Obj {
        let (message, load) = match self.images.resolve(src, attr, self.zoom, max_w) {
            images::Shown::Frame { w, h, source } => {
                return Obj {
                    w,
                    h,
                    baseline: h,
                    shape: Shape::Image(source),
                };
            }
            images::Shown::Pending => {
                // Nothing to reserve until the header is read, unless the tag gave a size.
                let (w, h) = match attr {
                    (Some(w), Some(h)) => mdt_image::display_size((w, h), attr, self.zoom, max_w),
                    _ => (0.0, 0.0),
                };
                return Obj {
                    w,
                    h,
                    baseline: h,
                    shape: Shape::Blank,
                };
            }
            images::Shown::Placeholder { message, load } => (message, load),
        };
        let pad = self.z(8.0);
        let icon = self.z(PLACEHOLDER_ICON);
        let base = BaseStyle {
            size: size * 0.85,
            line_height: LINE_HEIGHT,
            mono: false,
            bold: false,
            brush: brush::MUTED,
        };
        let alt = if alt.is_empty() { "(no alt text)" } else { alt };
        let text = format!("{alt}\n{src}\n{message}");
        let max_text = (max_w - icon - 3.0 * pad).max(self.z(40.0));
        let p = layout_paragraph(self.fonts, self.lcx, &text, base, &[], &[], max_text);
        let h = p.height.max(icon) + 2.0 * pad;
        Obj {
            w: (p.width + icon + 3.0 * pad).min(max_w.max(icon + 2.0 * pad)),
            h,
            baseline: h,
            shape: Shape::Placeholder(Box::new(p), text, pad, load),
        }
    }

    /// Adds the items that draw objects into their laid-out boxes; the paragraph is at
    /// (x, y).
    fn place(&mut self, out: &mut Out, ops: &[PaintOp], objects: Vec<Obj>, x: f32, y: f32) {
        let mut objects: Vec<Option<Obj>> = objects.into_iter().map(Some).collect();
        for op in ops {
            let PaintOp::InlineBox { id, rect } = op else {
                continue;
            };
            let Some(o) = objects.get_mut(*id as usize).and_then(Option::take) else {
                continue;
            };
            match o.shape {
                Shape::Math {
                    scene,
                    scale,
                    pad,
                    w,
                } => out.items.push(Item::Math {
                    x: x + rect.x + (rect.w - w) / 2.0,
                    y: y + rect.y + pad,
                    scale,
                    font: self.fonts.math_font_id(),
                    scene,
                }),
                Shape::Error(paragraph, message, pad) => {
                    let (ex, ey) = (x + rect.x, y + rect.y + pad);
                    out.hovers.push(Hover {
                        x: ex,
                        y: ey,
                        w: paragraph.width,
                        h: paragraph.height,
                        message,
                        link: None,
                    });
                    let ops = paint_ops(&paragraph.layout, self.fonts);
                    out.items.push(Item::Text(Box::new(TextItem {
                        x: ex,
                        y: ey,
                        paragraph: *paragraph,
                        ops,
                        scrolls: false,
                        select: None,
                    })));
                }
                Shape::Diagram { key, w, h } => out.items.push(Item::Diagram {
                    x: x + rect.x + (rect.w - w) / 2.0,
                    y: y + rect.y,
                    w,
                    h,
                    key,
                }),
                Shape::Code(code, message, pad) => {
                    let (bx, by) = (x + rect.x, y + rect.y);
                    let code_h = code.height + 2.0 * pad;
                    out.items.push(Item::Fill {
                        x: bx,
                        y: by,
                        w: rect.w,
                        h: code_h,
                        brush: brush::CODE_BG,
                    });
                    let ops = paint_ops(&code.layout, self.fonts);
                    out.items.push(Item::Text(Box::new(TextItem {
                        x: bx + pad,
                        y: by + pad,
                        paragraph: *code,
                        ops,
                        scrolls: false,
                        select: None,
                    })));
                    if let Some(m) = message {
                        let ops = paint_ops(&m.layout, self.fonts);
                        out.items.push(Item::Text(Box::new(TextItem {
                            x: bx,
                            y: by + code_h + self.z(4.0),
                            paragraph: *m,
                            ops,
                            scrolls: false,
                            select: None,
                        })));
                    }
                }
                Shape::Image(source) => out.items.push(Item::Image {
                    x: x + rect.x,
                    y: y + rect.y,
                    w: rect.w,
                    h: rect.h,
                    source,
                }),
                Shape::Placeholder(paragraph, text, pad, load) => {
                    let (px, py) = (x + rect.x, y + rect.y);
                    out.items.push(Item::Placeholder {
                        x: px,
                        y: py,
                        w: rect.w,
                        h: rect.h,
                        text,
                    });
                    if let Some(url) = load {
                        out.loads.push(Load {
                            x: px,
                            y: py,
                            w: rect.w,
                            h: rect.h,
                            url,
                        });
                    }
                    let ops = paint_ops(&paragraph.layout, self.fonts);
                    out.items.push(Item::Text(Box::new(TextItem {
                        x: px + 2.0 * pad + self.z(PLACEHOLDER_ICON),
                        y: py + pad,
                        paragraph: *paragraph,
                        ops,
                        scrolls: false,
                        select: None,
                    })));
                }
                Shape::Blank => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn plain(
        &mut self,
        out: &mut Out,
        text: &str,
        x: f32,
        y: f32,
        width: f32,
        base: BaseStyle,
        scrolls: bool,
    ) -> f32 {
        let inline = Inline {
            text: text.to_owned(),
            ..Default::default()
        };
        let h = self.inline(out, &inline, x, y, width, base, scrolls);
        // Markers and labels are decoration, not selectable text.
        if let Some(Item::Text(t)) = out.items.last_mut() {
            t.select = None;
        }
        h
    }

    fn body(&self) -> BaseStyle {
        BaseStyle {
            size: self.z(BODY_SIZE),
            line_height: LINE_HEIGHT,
            mono: false,
            bold: false,
            brush: brush::TEXT,
        }
    }

    /// Lays out blocks top to bottom with gaps between them; returns total height.
    fn blocks(
        &mut self,
        out: &mut Out,
        blocks: &[Block],
        x: f32,
        y: f32,
        width: f32,
        ctx: Nest,
    ) -> f32 {
        let mut cy = y;
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                cy += if ctx.tight { self.z(4.0) } else { self.z(GAP) };
            }
            cy += self.block(out, b, x, cy, width, ctx);
        }
        cy - y
    }

    fn block(
        &mut self,
        out: &mut Out,
        block: &Block,
        x: f32,
        y: f32,
        width: f32,
        nest: Nest,
    ) -> f32 {
        let body = BaseStyle {
            brush: nest.brush,
            ..self.body()
        };
        match &block.kind {
            BlockKind::Heading { level, content, .. } => {
                let lvl = (*level as usize).clamp(1, 6);
                let base = BaseStyle {
                    size: self.z(BODY_SIZE * HEADING_SCALE[lvl - 1]),
                    line_height: 1.25,
                    bold: true,
                    ..body
                };
                let base = if lvl == 6 {
                    BaseStyle {
                        brush: brush::MUTED,
                        ..base
                    }
                } else {
                    base
                };
                let mut h = self.inline(out, content, x, y, width, base, false);
                if lvl <= 2 {
                    h += self.z(6.0);
                    out.items.push(Item::Fill {
                        x,
                        y: y + h,
                        w: width,
                        h: self.z(1.0),
                        brush: brush::RULE,
                    });
                    h += self.z(1.0);
                }
                h
            }
            BlockKind::Paragraph { content, .. } => {
                self.inline(out, content, x, y, width, body, false)
            }
            BlockKind::Code { lang, text, .. }
                if lang
                    .as_deref()
                    .and_then(mdt_diagram::Engine::from_lang)
                    .is_some() =>
            {
                let engine = lang.as_deref().and_then(mdt_diagram::Engine::from_lang);
                let obj = self.diagram(engine.expect("checked"), text, width);
                let inline = fence_object(block.range.clone());
                self.inline_with(out, &inline, vec![obj], x, y, width, body, false)
            }
            BlockKind::Code { info, text, .. } => {
                let text = text.strip_suffix('\n').unwrap_or(text);
                let runs = match self.highlights.get(info, text) {
                    Some(spans) => spans
                        .iter()
                        .map(|s| StyleRun {
                            range: s.range.clone(),
                            brush: Some(Brush(brush::SYNTAX_BASE + s.class as u16)),
                            ..Default::default()
                        })
                        .collect(),
                    None => {
                        if mdt_highlight::language(info).is_some() {
                            self.missing.push((info.clone(), text.to_owned()));
                        }
                        Vec::new()
                    }
                };
                self.code_box(out, text, &runs, x, y, width, brush::CODE)
            }
            BlockKind::Math { latex } => {
                let inline = fence_inline(latex, block.range.clone());
                self.inline(out, &inline, x, y, width, body, false)
            }
            BlockKind::FrontMatter { text, .. } => {
                let text = text.strip_suffix('\n').unwrap_or(text);
                self.code_box(out, text, &[], x, y, width, brush::MUTED)
            }
            BlockKind::Html { text } => {
                let base = BaseStyle {
                    size: self.z(CODE_SIZE),
                    mono: true,
                    brush: brush::MUTED,
                    ..body
                };
                let first = out.items.len();
                let inline = Inline {
                    text: text.trim_end_matches('\n').to_owned(),
                    ..Default::default()
                };
                let h = self.inline(out, &inline, x, y, width, base, true);
                let content_w = match &out.items[first] {
                    Item::Text(t) => t.paragraph.width,
                    _ => 0.0,
                };
                out.regions.push(Region {
                    items: first..first + 1,
                    x,
                    y,
                    w: width,
                    h,
                    content_w,
                });
                h
            }
            BlockKind::Rule => {
                let h = self.z(GAP);
                out.items.push(Item::Fill {
                    x,
                    y: y + h / 2.0 - self.z(1.0),
                    w: width,
                    h: self.z(2.0),
                    brush: brush::RULE,
                });
                h
            }
            BlockKind::BlockQuote { alert, children } => {
                let inner_x = x + self.z(QUOTE_BAR + QUOTE_PAD);
                let inner_w = width - self.z(QUOTE_BAR + QUOTE_PAD);
                let (bar, child_brush) = match alert {
                    Some(k) => (brush::ALERT[alert_index(*k)], nest.brush),
                    None => (brush::RULE, brush::MUTED),
                };
                let mut h = 0.0;
                if let Some(k) = alert {
                    let label = BaseStyle {
                        bold: true,
                        brush: bar,
                        ..body
                    };
                    h += self.plain(out, alert_label(*k), inner_x, y, inner_w, label, false)
                        + self.z(4.0);
                }
                h += self.blocks(
                    out,
                    children,
                    inner_x,
                    y + h,
                    inner_w,
                    Nest {
                        brush: child_brush,
                        top: false,
                        ..nest
                    },
                );
                out.items.push(Item::Fill {
                    x,
                    y,
                    w: self.z(QUOTE_BAR),
                    h,
                    brush: bar,
                });
                h
            }
            BlockKind::List {
                start,
                tight,
                items,
            } => {
                let mut cy = y;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        cy += if *tight { self.z(4.0) } else { self.z(GAP) };
                    }
                    let marker_w = self.z(INDENT);
                    let cx = x + marker_w;
                    let mut h = 0.0;
                    match &item.task {
                        Some(task) => {
                            let size = self.z(13.0);
                            let line = self.z(BODY_SIZE * LINE_HEIGHT);
                            out.items.push(Item::Checkbox {
                                x: x + self.z(4.0),
                                y: cy + (line - size) / 2.0,
                                size,
                                checked: task.checked,
                                source: task.range.clone(),
                            });
                        }
                        None => {
                            let marker = match start {
                                Some(n) => format!("{}.", n + i as u64),
                                None => ["•", "◦", "▪"][nest.depth % 3].to_owned(),
                            };
                            let m = BaseStyle {
                                brush: brush::MUTED,
                                ..body
                            };
                            self.plain(out, &marker, x, cy, marker_w, m, false);
                        }
                    }
                    let child = Nest {
                        depth: nest.depth + 1,
                        tight: *tight,
                        top: false,
                        ..nest
                    };
                    h += self.blocks(out, &item.children, cx, cy, width - marker_w, child);
                    // An empty item still takes a line.
                    cy += h.max(self.z(BODY_SIZE * LINE_HEIGHT));
                }
                cy - y
            }
            BlockKind::Table { alignments, rows } => {
                self.table(out, alignments, rows, x, y, width, body, nest.top)
            }
            BlockKind::Footnotes { notes } => {
                let mut cy = y;
                out.items.push(Item::Fill {
                    x,
                    y: cy,
                    w: width / 3.0,
                    h: self.z(1.0),
                    brush: brush::RULE,
                });
                cy += self.z(GAP);
                let small = BaseStyle {
                    size: self.z(FOOTNOTE_SIZE),
                    ..body
                };
                for n in notes {
                    let marker_w = self.z(INDENT);
                    out.hovers.push(Hover {
                        x,
                        y: cy,
                        w: marker_w,
                        h: self.z(FOOTNOTE_SIZE * LINE_HEIGHT),
                        message: "Back to the reference".to_owned(),
                        link: Some(Link::FootnoteBack(n.label.clone())),
                    });
                    self.plain(
                        out,
                        &format!("{}.", n.number),
                        x,
                        cy,
                        marker_w,
                        BaseStyle {
                            brush: brush::MUTED,
                            ..small
                        },
                        false,
                    );
                    let child = Nest { top: false, ..nest };
                    let h =
                        self.blocks(out, &n.children, x + marker_w, cy, width - marker_w, child);
                    cy += h.max(self.z(FOOTNOTE_SIZE * LINE_HEIGHT)) + self.z(8.0);
                }
                cy - y
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn code_box(
        &mut self,
        out: &mut Out,
        text: &str,
        runs: &[StyleRun],
        x: f32,
        y: f32,
        width: f32,
        text_brush: Brush,
    ) -> f32 {
        let pad = self.z(CODE_PAD);
        let base = BaseStyle {
            size: self.z(CODE_SIZE),
            line_height: LINE_HEIGHT,
            mono: true,
            bold: false,
            brush: text_brush,
        };
        let first = out.items.len();
        let paragraph =
            layout_paragraph(self.fonts, self.lcx, text, base, runs, &[], f32::INFINITY);
        let ops = paint_ops(&paragraph.layout, self.fonts);
        let h = paragraph.height + 2.0 * pad;
        out.items.push(Item::Text(Box::new(TextItem {
            x: x + pad,
            y: y + pad,
            select: Some(SelectText {
                text: text.to_owned(),
                objects: Vec::new(),
            }),
            paragraph,
            ops,
            scrolls: true,
        })));
        out.items.insert(
            first,
            Item::Fill {
                x,
                y,
                w: width,
                h,
                brush: brush::CODE_BG,
            },
        );
        let content_w = match &out.items[first + 1] {
            Item::Text(t) => t.paragraph.width,
            _ => 0.0,
        };
        out.regions.push(Region {
            items: first + 1..out.items.len(),
            x: x + pad,
            y,
            w: width - 2.0 * pad,
            h,
            content_w,
        });
        h
    }

    #[allow(clippy::too_many_arguments)]
    fn table(
        &mut self,
        out: &mut Out,
        alignments: &[mdt_markdown::Alignment],
        rows: &[Vec<Inline>],
        x: f32,
        y: f32,
        width: f32,
        body: BaseStyle,
        top: bool,
    ) -> f32 {
        let shown = table_rows_shown(rows);
        let (rows, hidden) = (&rows[..shown], rows.len() - shown);
        let first = out.items.len();
        let pad = self.z(CELL_PAD);
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        // A word counts at most this wide toward a column's minimum; longer ones break.
        let word_cap = {
            let p = layout_paragraph(
                self.fonts,
                self.lcx,
                &"0".repeat(MAX_WORD_CHARS),
                body,
                &[],
                &[],
                f32::INFINITY,
            );
            p.width
        };
        let mut min_w = vec![0.0f32; cols];
        let mut max_w = vec![0.0f32; cols];
        type Cell = (Paragraph, SelectText, Vec<Obj>);
        let mut laid: Vec<Vec<Cell>> = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            let base = BaseStyle {
                bold: r == 0,
                ..body
            };
            let mut cells = Vec::new();
            for (c, cell) in row.iter().enumerate() {
                let objects = self.objects(cell, base.size, 0.0, width);
                let p = layout_cell(
                    self.fonts,
                    self.lcx,
                    &cell.text,
                    base,
                    &style_runs(cell),
                    &boxes(cell, &objects),
                );
                let (min, max) = p.content_widths();
                let widest_object = objects.iter().map(|o| o.w).fold(0.0, f32::max);
                min_w[c] = min_w[c].max(min.min(word_cap).max(widest_object));
                max_w[c] = max_w[c].max(max.max(p.width));
                cells.push((p, SelectText::from_inline(cell), objects));
            }
            laid.push(cells);
        }
        // Top-level tables may use the page (window less side padding), centered on it.
        let (limit_x, limit_w) = match self.page {
            Some((px, pw)) if top && pw > width => (px, pw),
            _ => (x, width),
        };
        let col_w = column_widths(&min_w, &max_w, width, limit_w - cols as f32 * 2.0 * pad);
        let total_w: f32 = col_w.iter().map(|w| w + 2.0 * pad).sum();
        let tx = if total_w <= width {
            x
        } else {
            (limit_x + (limit_w - total_w) / 2.0).max(limit_x)
        };
        let mut cy = y;
        for (r, (cells, row)) in laid.into_iter().zip(rows).enumerate() {
            let mut cells: Vec<Cell> = cells;
            for (c, (p, ..)) in cells.iter_mut().enumerate() {
                if p.width > col_w[c] + 0.5 {
                    p.rebreak(col_w[c]);
                }
            }
            let row_h = cells.iter().map(|(p, ..)| p.height).fold(0.0, f32::max) + 2.0 * pad;
            let mut cx = tx;
            for (c, (p, select, objects)) in cells.into_iter().enumerate() {
                let slack = (col_w[c] - p.width).max(0.0);
                let dx = match alignments.get(c) {
                    Some(mdt_markdown::Alignment::Center) => slack / 2.0,
                    Some(mdt_markdown::Alignment::Right) => slack,
                    _ => 0.0,
                };
                let ops = paint_ops(&p.layout, self.fonts);
                link_hovers(out, &row[c], &p, cx + pad + dx, cy + pad);
                self.place(out, &ops, objects, cx + pad + dx, cy + pad);
                out.items.push(Item::Text(Box::new(TextItem {
                    x: cx + pad + dx,
                    y: cy + pad,
                    paragraph: p,
                    ops,
                    scrolls: false,
                    select: Some(select),
                })));
                cx += col_w[c] + 2.0 * pad;
            }
            cy += row_h;
            let line_h = if r == 0 { self.z(2.0) } else { self.z(1.0) };
            out.items.push(Item::Fill {
                x: tx,
                y: cy,
                w: total_w,
                h: line_h,
                brush: brush::RULE,
            });
            cy += line_h;
        }
        let (rx, rw) = if total_w <= width {
            (x, width)
        } else {
            (limit_x, limit_w)
        };
        out.regions.push(Region {
            items: first..out.items.len(),
            x: rx,
            y,
            w: rw,
            h: cy - y,
            content_w: tx - rx + total_w,
        });
        if hidden > 0 {
            let note = BaseStyle {
                brush: brush::MUTED,
                ..body
            };
            let text =
                format!("{hidden} more rows not shown. The Source view has the whole table.");
            cy += self.z(4.0);
            cy += self.plain(out, &text, x, cy, width, note, false);
        }
        cy - y
    }
}

/// Table cells laid out at most per table. A table is one layout unit, laid out on the UI
/// thread (release, Apple M4): short cells take about 3.5 µs each, so a full table of them
/// about 20 ms; 600 rows of 10 cells of 100 characters took 167 ms. The rows past it are
/// left out with a note.
pub const MAX_TABLE_CELLS: usize = 6000;

/// How many of a table's rows (the header first) are shown: at most `MAX_TABLE_CELLS`
/// cells, and always the header.
pub fn table_rows_shown(rows: &[Vec<Inline>]) -> usize {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    rows.len().min((MAX_TABLE_CELLS / cols).max(1))
}

/// Hover areas for the links and footnote references in `inline`, laid out as `paragraph`
/// at (x, y): clicking one follows it. All in one pass over the layout: one
/// `selection_rects` per link was quadratic in a long line of adjacent links.
fn link_hovers(out: &mut Out, inline: &Inline, paragraph: &Paragraph, x: f32, y: f32) {
    let (ranges, links): (Vec<_>, Vec<_>) = inline
        .spans
        .iter()
        .filter_map(|span| {
            let link = match &span.style {
                SpanStyle::Link { dest, .. } => (Link::Url(dest.clone()), dest.clone()),
                SpanStyle::FootnoteRef { label } => (
                    Link::Footnote(label.clone()),
                    "Go to the footnote".to_owned(),
                ),
                _ => return None,
            };
            Some((span.range.clone(), link))
        })
        .unzip();
    let rects = mdt_text::ranges_rects(&paragraph.layout, &ranges);
    for ((link, message), rects) in links.into_iter().zip(rects) {
        for r in rects {
            out.hovers.push(Hover {
                x: x + r.x,
                y: y + r.y,
                w: r.w,
                h: r.h,
                message: message.clone(),
                link: Some(link.clone()),
            });
        }
    }
}

/// A word in a table cell counts at most this many body characters toward its column's
/// minimum width; a longer one breaks inside the word.
pub const MAX_WORD_CHARS: usize = 20;

/// Table column content widths (padding excluded) from each column's minimum and natural
/// widths, the text column's width `column` (no column grows past it unless its minimum
/// does) and the content width `limit` available to the table.
pub fn column_widths(min: &[f32], max: &[f32], column: f32, limit: f32) -> Vec<f32> {
    let pref: Vec<f32> = min
        .iter()
        .zip(max)
        .map(|(&lo, &hi)| hi.min(column).max(lo))
        .collect();
    if pref.iter().sum::<f32>() <= limit {
        return pref;
    }
    let floor: f32 = min.iter().sum();
    let give: f32 = pref.iter().zip(min).map(|(p, m)| p - m).sum();
    if floor >= limit || give <= 0.0 {
        return min.to_vec();
    }
    let share = (limit - floor) / give;
    pref.iter()
        .zip(min)
        .map(|(p, m)| m + (p - m) * share)
        .collect()
}

#[derive(Clone, Copy)]
struct Nest {
    depth: usize,
    tight: bool,
    brush: Brush,
    /// Not inside a list, blockquote, alert or footnote: a table may use the page.
    top: bool,
}

fn alert_index(k: AlertKind) -> usize {
    match k {
        AlertKind::Note => 0,
        AlertKind::Tip => 1,
        AlertKind::Important => 2,
        AlertKind::Warning => 3,
        AlertKind::Caution => 4,
    }
}

fn alert_label(k: AlertKind) -> &'static str {
    ["Note", "Tip", "Important", "Warning", "Caution"][alert_index(k)]
}

/// Inline spans → mdt-text style runs, inline objects → inline boxes.
pub fn style_runs(inline: &Inline) -> Vec<StyleRun> {
    inline
        .spans
        .iter()
        .filter_map(|s| {
            let range = s.range.clone();
            Some(match &s.style {
                SpanStyle::Emphasis => StyleRun {
                    range,
                    italic: true,
                    ..Default::default()
                },
                SpanStyle::Strong => StyleRun {
                    range,
                    bold: true,
                    ..Default::default()
                },
                SpanStyle::Strikethrough => StyleRun {
                    range,
                    strikethrough: true,
                    ..Default::default()
                },
                SpanStyle::Code => StyleRun {
                    range,
                    mono: true,
                    brush: Some(brush::CODE),
                    scale: Some(0.9),
                    ..Default::default()
                },
                SpanStyle::Link { .. } => StyleRun {
                    range,
                    underline: true,
                    brush: Some(brush::LINK),
                    ..Default::default()
                },
                SpanStyle::FootnoteRef { .. } => StyleRun {
                    range,
                    brush: Some(brush::LINK),
                    scale: Some(0.75),
                    ..Default::default()
                },
                SpanStyle::Html => StyleRun {
                    range,
                    mono: true,
                    brush: Some(brush::MUTED),
                    scale: Some(0.9),
                    ..Default::default()
                },
                SpanStyle::SoftBreak | SpanStyle::HardBreak => return None,
            })
        })
        .collect()
}

/// A math fence as an inline holding one display equation; copying it gives the fence.
fn fence_inline(latex: &str, source: std::ops::Range<usize>) -> Inline {
    let mut inline = Inline {
        text: OBJECT_CHAR.to_string(),
        ..Default::default()
    };
    inline.map.push(Segment {
        rendered: 0..inline.text.len(),
        source: source.clone(),
    });
    inline.objects.push(InlineObject {
        at: 0,
        source,
        kind: ObjectKind::Math {
            latex: latex.to_owned(),
            display: true,
        },
    });
    inline
}

/// A fence shown as one inline object (a diagram); copying it gives the fence.
fn fence_object(source: std::ops::Range<usize>) -> Inline {
    let mut inline = Inline {
        text: OBJECT_CHAR.to_string(),
        ..Default::default()
    };
    inline.map.push(Segment {
        rendered: 0..inline.text.len(),
        source: source.clone(),
    });
    // The kind is not consulted: the caller resolves the object itself.
    inline.objects.push(InlineObject {
        at: 0,
        source,
        kind: ObjectKind::HtmlBreak {
            html: String::new(),
        },
    });
    inline
}

pub fn fence_object_select(source: std::ops::Range<usize>) -> SelectText {
    SelectText::from_inline(&fence_object(source))
}

pub fn fence_select(latex: &str, source: std::ops::Range<usize>) -> SelectText {
    SelectText::from_inline(&fence_inline(latex, source))
}

/// What a unit layout's source ranges are relative to: the unit's start. The footnotes
/// section has no source of its own (its notes are defined anywhere), so its ranges stay
/// absolute and the reader keys its layout by where the notes are.
pub fn source_base(unit: &LayoutUnit) -> usize {
    match unit.block.kind {
        BlockKind::Footnotes { .. } => 0,
        _ => unit.range.start,
    }
}

/// Lays out a unit at the given column width.
pub fn layout_unit(ctx: &mut Ctx, index: usize, unit: &LayoutUnit, width: f32) -> UnitLayout {
    ctx.unit = index;
    ctx.math_n = 0;
    ctx.diagram_n = 0;
    let mut out = Out {
        items: Vec::new(),
        regions: Vec::new(),
        hovers: Vec::new(),
        loads: Vec::new(),
    };
    let nest = Nest {
        depth: 0,
        tight: false,
        brush: brush::TEXT,
        top: true,
    };
    let h = ctx.block(&mut out, &unit.block, 0.0, 0.0, width, nest);
    // The layout is cached by content and reused wherever the same unit appears (after an
    // edit above it, or for a repeated block), so its source ranges are kept relative to
    // the unit (`source_base`); users add the base back.
    let base = source_base(unit);
    let rel =
        |r: &mut std::ops::Range<usize>| *r = r.start.wrapping_sub(base)..r.end.wrapping_sub(base);
    for item in &mut out.items {
        match item {
            Item::Checkbox { source, .. } => rel(source),
            Item::Text(t) => {
                for (_, r) in t.select.iter_mut().flat_map(|s| &mut s.objects) {
                    rel(r);
                }
            }
            _ => {}
        }
    }
    UnitLayout {
        height: h + ctx.z(GAP),
        items: out.items,
        regions: out.regions,
        hovers: out.hovers,
        loads: out.loads,
    }
}

/// Height guess for a unit not yet laid out.
pub fn estimate_height(unit: &LayoutUnit, width: f32, zoom: f32) -> f32 {
    let line = BODY_SIZE * LINE_HEIGHT * zoom;
    let chars_per_line = (width / (BODY_SIZE * 0.5 * zoom)).max(10.0);
    let len = unit.range.len() as f32;
    let gap = GAP * zoom;
    match &unit.block.kind {
        BlockKind::Code { text, .. } => {
            text.lines().count().max(1) as f32 * CODE_SIZE * LINE_HEIGHT * zoom
                + 2.0 * CODE_PAD * zoom
                + gap
        }
        BlockKind::Math { latex } => latex.lines().count().max(1) as f32 * line * 1.5 + gap,
        BlockKind::Rule => 2.0 * gap,
        BlockKind::Heading { level, .. } => {
            BODY_SIZE * HEADING_SCALE[(*level as usize).clamp(1, 6) - 1] * 1.25 * zoom + gap
        }
        BlockKind::Table { rows, .. } => {
            let rows = &rows[..table_rows_shown(rows)];
            // Cells longer than an even share of the column wrap.
            let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1) as f32;
            let share = (chars_per_line / cols).max(4.0);
            let lines: f32 = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|c| (c.text.chars().count() as f32 / share).ceil())
                        .fold(1.0, f32::max)
                })
                .sum();
            lines * line + rows.len() as f32 * 2.0 * CELL_PAD * zoom + gap
        }
        BlockKind::List { items, .. } => {
            items.len() as f32 * line + (len / chars_per_line).floor() * line + gap
        }
        _ => ((len / chars_per_line).ceil().max(1.0)) * line + gap,
    }
}

#[cfg(test)]
mod tests {
    use super::column_widths;

    #[test]
    fn column_widths_follow_the_table_layout_rules() {
        // Natural widths fit: unchanged.
        assert_eq!(
            column_widths(&[10.0, 20.0], &[30.0, 40.0], 600.0, 100.0),
            [30.0, 40.0]
        );
        // No column grows past the text column, unless its minimum does.
        assert_eq!(
            column_widths(&[10.0, 700.0], &[900.0, 800.0], 600.0, 2000.0),
            [600.0, 700.0]
        );
        // Only the minimums fit: leftover space goes by (preferred − minimum).
        let w = column_widths(&[10.0, 10.0], &[110.0, 410.0], 600.0, 220.0);
        assert!(
            (w[0] - 50.0).abs() < 1e-3 && (w[1] - 170.0).abs() < 1e-3,
            "{w:?}"
        );
        // Not even the minimums fit: minimums (the table scrolls).
        assert_eq!(
            column_widths(&[100.0, 100.0], &[200.0, 200.0], 600.0, 150.0),
            [100.0, 100.0]
        );
    }
}
