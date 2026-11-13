use std::borrow::Cow;
use std::cell::Cell;
use std::fmt::Write as _;

use parley::{LayoutContext, PositionedLayoutItem};

use crate::*;

/// Hands out distinct font ids without a platform, for tests that only lay out.
#[derive(Default)]
struct FakeGpui(Cell<usize>);

impl GpuiFonts for FakeGpui {
    fn add_fonts(&self, _: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        Ok(())
    }
    fn exact_font_id(&self, _: &gpui::Font) -> Option<gpui::FontId> {
        self.0.set(self.0.get() + 1);
        Some(gpui::FontId(self.0.get()))
    }
}

const BASE: BaseStyle = BaseStyle {
    size: 16.0,
    line_height: 1.5,
    mono: false,
    bold: false,
    brush: Brush(0),
};

fn describe(text: &str, runs: &[StyleRun], boxes: &[InlineBoxSpec], width: f32) -> String {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut lcx = LayoutContext::new();
    let p = layout_paragraph(&mut fonts, &mut lcx, text, BASE, runs, boxes, width);
    let mut out = format!("width {:.2} height {:.2}\n", p.width, p.height);
    for line in p.layout.lines() {
        let m = line.metrics();
        let range = line.text_range();
        writeln!(
            out,
            "line top {:.2} baseline {:.2} height {:.2} {:?}",
            m.block_min_coord,
            m.baseline,
            m.block_max_coord - m.block_min_coord,
            text[range].replace(OBJECT_CHAR, "[obj]")
        )
        .unwrap();
        for item in line.items() {
            if let PositionedLayoutItem::InlineBox(b) = item {
                writeln!(out, "  box {} at x {:.2} y {:.2}", b.id, b.x, b.y).unwrap();
            }
        }
    }
    out
}

const PARA: &str = "Markdown paragraphs wrap at the column width. Some words are bold, some are code, and the last line is short.";

#[test]
fn plain_paragraph_wraps() {
    insta::assert_snapshot!(describe(PARA, &[], &[], 320.0));
}

#[test]
fn styled_runs_change_breaks() {
    let bold = PARA.find("bold").unwrap();
    let code = PARA.find("code").unwrap();
    let runs = [
        StyleRun {
            range: bold..bold + 4,
            bold: true,
            ..Default::default()
        },
        StyleRun {
            range: code..code + 4,
            mono: true,
            ..Default::default()
        },
        StyleRun {
            range: 0..8,
            italic: true,
            underline: true,
            ..Default::default()
        },
    ];
    insta::assert_snapshot!(describe(PARA, &runs, &[], 320.0));
}

#[test]
fn tall_inline_box_grows_its_line() {
    let text =
        "Inline math \u{FFFC} sits on the baseline and makes this line taller than the next one.";
    let at = text.find(OBJECT_CHAR).unwrap();
    let boxes = [InlineBoxSpec {
        id: 7,
        at,
        width: 40.0,
        height: 44.0,
        baseline: 30.0,
    }];
    let out = describe(text, &[], &boxes, 320.0);
    insta::assert_snapshot!(out);
}

#[test]
fn tall_box_line_is_taller_and_box_baseline_matches_text() {
    let text =
        "Inline math \u{FFFC} sits on the baseline and makes this line taller than the next one.";
    let at = text.find(OBJECT_CHAR).unwrap();
    let boxes = [InlineBoxSpec {
        id: 7,
        at,
        width: 40.0,
        height: 44.0,
        baseline: 30.0,
    }];
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut lcx = LayoutContext::new();
    let p = layout_paragraph(&mut fonts, &mut lcx, text, BASE, &[], &boxes, 320.0);
    let lines: Vec<_> = p.layout.lines().collect();
    let h = |i: usize| lines[i].metrics().block_max_coord - lines[i].metrics().block_min_coord;
    assert!(lines.len() >= 2);
    assert!(
        h(0) >= 44.0 && h(0) > h(1),
        "first line {} vs {}",
        h(0),
        h(1)
    );
    let bx = lines[0]
        .items()
        .find_map(|i| match i {
            PositionedLayoutItem::InlineBox(b) => Some(b),
            _ => None,
        })
        .unwrap();
    assert!((bx.y + 30.0 - lines[0].metrics().baseline).abs() < 0.01);
}

#[test]
fn object_char_paints_nothing() {
    let text = "a\u{FFFC}b";
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut lcx = LayoutContext::new();
    let boxes = [InlineBoxSpec {
        id: 1,
        at: 1,
        width: 10.0,
        height: 10.0,
        baseline: 8.0,
    }];
    let p = layout_paragraph(&mut fonts, &mut lcx, text, BASE, &[], &boxes, 320.0);
    let ops = paint_ops(&p.layout, &fonts);
    assert!(
        !ops.iter()
            .any(|o| matches!(o, PaintOp::MissingGlyph { .. })),
        "{ops:?}"
    );
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(o, PaintOp::InlineBox { .. }))
            .count(),
        1
    );
}

/// `ranges_rects` gives each range the rectangles `selection_rects` gives it alone, across
/// wrapped lines, style runs and an inline box.
#[test]
fn ranges_rects_match_selection_rects() {
    let text = "Some *styled* words, inline math \u{FFFC} and a long tail that wraps onto the next lines of the paragraph.";
    let at = text.find(OBJECT_CHAR).unwrap();
    let boxes = [InlineBoxSpec {
        id: 1,
        at,
        width: 40.0,
        height: 20.0,
        baseline: 15.0,
    }];
    let runs = [StyleRun {
        range: 5..13,
        bold: true,
        ..Default::default()
    }];
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut lcx = LayoutContext::new();
    let p = layout_paragraph(&mut fonts, &mut lcx, text, BASE, &runs, &boxes, 240.0);
    assert!(p.layout.lines().count() >= 3);
    let word = |w: &str| text.find(w).unwrap()..text.find(w).unwrap() + w.len();
    let ranges = [
        word("Some"),
        word("styled"),
        3..9,
        at..at + OBJECT_CHAR.len_utf8() + 4,
        word("long tail that wraps onto the next"),
        0..text.len(),
        7..7,
    ];
    let all = ranges_rects(&p.layout, &ranges);
    for (range, got) in ranges.iter().zip(&all) {
        let want = selection_rects(&p.layout, range.clone());
        assert_eq!(got.len(), want.len(), "{range:?}: {got:?} vs {want:?}");
        for (g, w) in got.iter().zip(&want) {
            let close = |a: f32, b: f32| (a - b).abs() < 0.01;
            assert!(
                close(g.x, w.x) && close(g.y, w.y) && close(g.w, w.w) && close(g.h, w.h),
                "{range:?}: {got:?} vs {want:?}"
            );
        }
    }
}

/// More style runs than Parley holds (65,535 spans, one or two per run) in one paragraph:
/// highlighted code of 1 MB or 100,000 autolinks panicked. Runs past `MAX_STYLE_RANGES`
/// keep the base style.
#[test]
fn more_runs_than_parley_holds() {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut lcx = LayoutContext::new();
    let n = 70_000;
    let text = "ab".repeat(n);
    let runs: Vec<StyleRun> = (0..n)
        .map(|i| StyleRun {
            range: i * 2..i * 2 + 1,
            brush: Some(Brush(3)),
            ..Default::default()
        })
        .collect();
    let p = layout_paragraph(&mut fonts, &mut lcx, &text, BASE, &runs, &[], 600.0);
    let brush_at =
        |at: usize| parley::Cluster::from_byte_index(&p.layout, at).map(|c| c.style().brush);
    assert_eq!(brush_at(0), Some(Brush(3)));
    assert_eq!(brush_at(1), Some(BASE.brush));
    assert_eq!(brush_at((n - 1) * 2), Some(BASE.brush), "past the cap");
}

/// Emoji clusters go in among the runs in text order: all of them after the runs took
/// time quadratic in their number (16,000 of each, 1.3 s in release).
#[test]
fn emoji_among_many_runs() {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut lcx = LayoutContext::new();
    let n = 16_000;
    let text = "ab 😀 ".repeat(n);
    let runs: Vec<StyleRun> = (0..n)
        .map(|i| StyleRun {
            range: i * 8..i * 8 + 7,
            mono: true,
            ..Default::default()
        })
        .collect();
    let start = std::time::Instant::now();
    let p = layout_paragraph(&mut fonts, &mut lcx, &text, BASE, &runs, &[], 600.0);
    let elapsed = start.elapsed();
    assert!(p.height > 0.0);
    if !cfg!(debug_assertions) {
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "{elapsed:?}"
        );
    }
}

/// Painting skips the ops outside the visible area (painting every glyph of a 1 MB
/// paragraph took 50 ms a frame) and keeps every op that may reach it.
#[test]
fn only_ops_near_the_visible_area_paint() {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut lcx = LayoutContext::new();
    let text = PARA.repeat(500);
    let p = layout_paragraph(&mut fonts, &mut lcx, &text, BASE, &[], &[], 320.0);
    let ops = paint_ops(&p.layout, &fonts);
    let visible = Rect {
        x: 0.0,
        y: 4000.0,
        w: 320.0,
        h: 800.0,
    };
    let shown: Vec<_> = ops
        .iter()
        .filter(|op| crate::paint::may_show(op, &visible))
        .collect();
    assert!(
        shown.len() * 20 < ops.len(),
        "{} of {}",
        shown.len(),
        ops.len()
    );
    for op in &ops {
        if let PaintOp::Glyph { x, y, .. } = op {
            let inside = *x >= visible.x
                && *x <= visible.x + visible.w
                && *y >= visible.y
                && *y <= visible.y + visible.h;
            assert!(!inside || crate::paint::may_show(op, &visible), "{op:?}");
        }
    }
    // A glyph whose origin is just above the area may still reach into it.
    let above = PaintOp::Glyph {
        font_id: gpui::FontId(0),
        glyph: 1,
        x: 10.0,
        y: visible.y - 10.0,
        size: 16.0,
        brush: Brush(0),
    };
    assert!(crate::paint::may_show(&above, &visible));
}
