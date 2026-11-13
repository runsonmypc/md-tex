//! Renders the text visual-check document offscreen and writes `docs/visual/text-1x.png`
//! and `text-2x.png`: `cargo run -p mdt-text --example visual_check`.
//!
//! The headless window has a fixed scale factor of 2. GPUI multiplies every position and
//! font size by the scale factor, so a layout at zoom 0.5 in that window produces exactly
//! the device pixels of zoom 1 at scale factor 1; that is the 1x capture.

use std::sync::Arc;

use gpui::{
    AppContext, HeadlessAppContext, Hsla, IntoElement, ParentElement, Render, Styled, canvas, div,
    point, px, rgb, size,
};
use parley::LayoutContext;

use mdt_text::{
    BaseStyle, Brush, FontRegistry, InlineBoxSpec, OBJECT_CHAR, PaintOp, Rect, StyleRun,
    layout_paragraph, paint, paint_ops, selection_rects,
};

const WIDTH: f32 = 560.0;
const PAD: f32 = 24.0;

struct Block {
    y: f32,
    ops: Vec<PaintOp>,
    selection: Vec<Rect>,
}

struct Visual {
    blocks: Vec<Block>,
}

fn color(b: Brush) -> Hsla {
    match b.0 {
        1 => rgb(0x0b5cad).into(), // link
        2 => rgb(0x8a3b12).into(), // code
        _ => rgb(0x1a1a1a).into(),
    }
}

impl Render for Visual {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let blocks: Vec<(f32, Vec<PaintOp>, Vec<Rect>)> = self
            .blocks
            .iter()
            .map(|b| (b.y, b.ops.clone(), b.selection.clone()))
            .collect();
        div().size_full().bg(rgb(0xffffff)).child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    for (y, ops, sel) in &blocks {
                        let origin = bounds.origin + point(px(0.0), px(*y));
                        paint(ops, sel, rgb(0xb4d5fe).into(), origin, color, window);
                        for op in ops {
                            if let PaintOp::InlineBox { rect, .. } = op {
                                let b = gpui::Bounds::new(
                                    origin + point(px(rect.x), px(rect.y)),
                                    size(px(rect.w), px(rect.h)),
                                );
                                window.paint_quad(gpui::fill(b, rgb(0x3b82f6)));
                            }
                        }
                    }
                },
            )
            .size_full(),
        )
    }
}

fn build(fonts: &mut FontRegistry, ts: &Arc<gpui::TextSystem>, zoom: f32) -> Vec<Block> {
    let mut lcx = LayoutContext::new();
    let z = |v: f32| v * zoom;
    let body = BaseStyle {
        size: z(16.0),
        line_height: 1.5,
        mono: false,
        bold: false,
        brush: Brush(0),
    };
    let mut blocks = Vec::new();
    let mut y = z(PAD);
    let mut add = |fonts: &mut FontRegistry,
                   text: &str,
                   base: BaseStyle,
                   runs: &[StyleRun],
                   boxes: &[InlineBoxSpec],
                   sel: Option<std::ops::Range<usize>>| {
        let p = layout_paragraph(fonts, &mut lcx, text, base, runs, boxes, z(WIDTH));
        fonts
            .ensure_layout_fonts(&p.layout, ts)
            .expect("fallback fonts");
        let ops = paint_ops(&p.layout, fonts);
        let selection = sel
            .map(|r| selection_rects(&p.layout, r))
            .unwrap_or_default();
        let mut ops = ops;
        for op in &mut ops {
            shift(op, z(PAD));
        }
        let selection = selection
            .into_iter()
            .map(|r| Rect {
                x: r.x + z(PAD),
                ..r
            })
            .collect();
        blocks.push(Block { y, ops, selection });
        y += p.height + z(12.0);
    };

    add(
        fonts,
        "Visual check: text",
        BaseStyle {
            size: z(28.0),
            bold: true,
            ..body
        },
        &[],
        &[],
        None,
    );

    let para = "Body text in the bundled font with bold, italic, a link, inline code, struck words, and a selection that starts on the first line and ends on the second.";
    let at = |w: &str| para.find(w).unwrap();
    let runs = [
        StyleRun {
            range: at("bold")..at("bold") + 4,
            bold: true,
            ..Default::default()
        },
        StyleRun {
            range: at("italic")..at("italic") + 6,
            italic: true,
            ..Default::default()
        },
        StyleRun {
            range: at("a link")..at("a link") + 6,
            underline: true,
            brush: Some(Brush(1)),
            ..Default::default()
        },
        StyleRun {
            range: at("inline code")..at("inline code") + 11,
            mono: true,
            brush: Some(Brush(2)),
            ..Default::default()
        },
        StyleRun {
            range: at("struck words")..at("struck words") + 12,
            strikethrough: true,
            ..Default::default()
        },
    ];
    add(
        fonts,
        para,
        body,
        &runs,
        &[],
        Some(at("code")..at("selection") + 9),
    );

    let math =
        "Inline objects sit on the baseline: \u{FFFC} and a tall one \u{FFFC} grows its line.";
    let mut objs = math.match_indices(OBJECT_CHAR).map(|(i, _)| i);
    let boxes = [
        InlineBoxSpec {
            id: 1,
            at: objs.next().unwrap(),
            width: z(36.0),
            height: z(14.0),
            baseline: z(12.0),
        },
        InlineBoxSpec {
            id: 2,
            at: objs.next().unwrap(),
            width: z(28.0),
            height: z(44.0),
            baseline: z(30.0),
        },
    ];
    add(fonts, math, body, &[], &boxes, None);

    add(
        fonts,
        "let x = a != b && c >= d; // monospace: Meslo LG M 0O1lI",
        BaseStyle {
            mono: true,
            size: z(14.0),
            ..body
        },
        &[],
        &[],
        None,
    );

    add(
        fonts,
        "Fallback: 日本語のテキスト. Missing glyph: \u{0378}.",
        body,
        &[],
        &[],
        None,
    );
    blocks
}

fn shift(op: &mut PaintOp, dx: f32) {
    match op {
        PaintOp::Glyph { x, .. } | PaintOp::Emoji { x, .. } => *x += dx,
        PaintOp::MissingGlyph { rect, .. }
        | PaintOp::InlineBox { rect, .. }
        | PaintOp::Underline { rect, .. }
        | PaintOp::Strikethrough { rect, .. } => rect.x += dx,
    }
}

fn main() -> anyhow::Result<()> {
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/visual");
    std::fs::create_dir_all(&out)?;
    for (zoom, name) in [(0.5, "text-1x.png"), (1.0, "text-2x.png")] {
        let text_system = cx.text_system().clone();
        let mut fonts = FontRegistry::new(&text_system)?;
        let blocks = build(&mut fonts, &text_system, zoom);
        let height = blocks.last().map_or(0.0, |b| b.y) + 80.0 * zoom;
        let window = cx
            .open_window(size(px((WIDTH + 2.0 * PAD) * zoom), px(height)), |_, cx| {
                cx.new(|_| Visual { blocks })
            })?;
        cx.run_until_parked();
        let image = cx.capture_screenshot(window.into())?;
        image.save(out.join(name))?;
        println!(
            "wrote {} ({}x{})",
            out.join(name).display(),
            image.width(),
            image.height()
        );
    }
    Ok(())
}
