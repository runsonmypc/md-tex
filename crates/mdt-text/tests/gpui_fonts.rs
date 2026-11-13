//! Runs on the main thread (`harness = false`): GPUI's macOS platform must be created there.

use std::sync::Arc;

use gpui::{FontRun, PlatformTextSystem, px};
use parley::{
    FontFamily, FontStyle, FontWeight, Layout, LayoutContext, PositionedLayoutItem, StyleProperty,
};

use mdt_text::fonts::*;
use mdt_text::{Brush, FontRegistry, PaintOp, paint_ops};

fn platform_text_system() -> Arc<dyn PlatformTextSystem> {
    gpui_platform::current_platform(true).text_system()
}

fn layout(
    reg: &mut FontRegistry,
    text: &str,
    family: &str,
    bold: bool,
    italic: bool,
) -> Layout<Brush> {
    let mut lcx = LayoutContext::new();
    let mut b = lcx.ranged_builder(&mut reg.fcx, text, 1.0, false);
    b.push_default(StyleProperty::FontFamily(FontFamily::named(family)));
    b.push_default(StyleProperty::FontSize(16.0));
    if bold {
        b.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
    }
    if italic {
        b.push_default(StyleProperty::FontStyle(FontStyle::Italic));
    }
    let mut layout: Layout<Brush> = b.build(text);
    layout.break_all_lines(None);
    layout
}

/// (GPUI font id, glyph ids, text) for every glyph run, in order.
fn parley_runs(
    reg: &FontRegistry,
    layout: &Layout<Brush>,
    text: &str,
) -> Vec<(gpui::FontId, Vec<u32>, String)> {
    let mut out = Vec::new();
    for line in layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(run) = item {
                let font_id = reg
                    .font_id(run.run().font())
                    .expect("run font is not mapped");
                assert!(!run.run().synthesis().embolden(), "synthesized bold");
                let ids = run.positioned_glyphs().map(|g| g.id).collect();
                out.push((font_id, ids, text[run.run().text_range()].to_owned()));
            }
        }
    }
    out
}

fn gpui_glyphs(ts: &dyn PlatformTextSystem, text: &str, font_id: gpui::FontId) -> Vec<u32> {
    let layout = ts.layout_line(
        text,
        px(16.0),
        &[FontRun {
            len: text.len(),
            font_id,
        }],
    );
    layout
        .runs
        .iter()
        .flat_map(|r| r.glyphs.iter().map(|g| g.id.0))
        .collect()
}

fn bundled_faces_give_identical_glyph_ids_in_parley_and_gpui() {
    let ts = platform_text_system();
    let mut reg = FontRegistry::new(&*ts).unwrap();
    let samples = [
        (
            BODY_FAMILY,
            "Hello, glyphs: AVATAR café naïve 0123 — fi fl ffi",
        ),
        (MONO_FAMILY, "let x = a != b && c >= d; // 0O1lI {} => ->"),
        (MATH_FAMILY, "x+y=z (a) [b] 0123"),
    ];
    let mut checked = 0;
    for (family, text) in samples {
        for face in BUNDLED.iter().filter(|f| f.family == family) {
            let l = layout(&mut reg, text, family, face.bold, face.italic);
            let runs = parley_runs(&reg, &l, text);
            let font_id = ts.font_id(&face.gpui_font()).unwrap();
            let what = format!("{family} bold={} italic={}", face.bold, face.italic);
            assert!(
                runs.iter().all(|(id, _, _)| *id == font_id),
                "{what}: wrong face"
            );
            let parley: Vec<u32> = runs.into_iter().flat_map(|(_, ids, _)| ids).collect();
            assert_eq!(parley, gpui_glyphs(&*ts, text, font_id), "{what}");
            checked += 1;
        }
    }
    assert_eq!(checked, BUNDLED.len());
}

fn every_bundled_face_maps_to_a_distinct_font_id() {
    let ts = platform_text_system();
    let _reg = FontRegistry::new(&*ts).unwrap();
    let ids: std::collections::HashSet<_> = BUNDLED
        .iter()
        .map(|f| ts.font_id(&f.gpui_font()).unwrap())
        .collect();
    assert_eq!(ids.len(), BUNDLED.len());
}

/// Japanese text and an unassigned code point in the body family.
const FALLBACK_FIXTURE: &str = "Body text 日本語のテキスト, then U+0378: \u{0378}.";

/// Japanese text falls back to system fonts, which are registered with GPUI on first use
/// and give the same glyph ids there; the unassigned code point becomes a placeholder box.
fn fallback_fonts_and_missing_glyphs() {
    let ts = platform_text_system();
    let mut reg = FontRegistry::new(&*ts).unwrap();
    let v0 = reg.version();
    let l = layout(&mut reg, FALLBACK_FIXTURE, BODY_FAMILY, false, false);
    reg.ensure_layout_fonts(&l, &*ts).unwrap();
    assert!(reg.version() > v0, "no fallback font was registered");
    let runs = parley_runs(&reg, &l, FALLBACK_FIXTURE);
    let body_id = ts.font_id(&BUNDLED[0].gpui_font()).unwrap();
    let fallback_text: String = runs
        .iter()
        .filter(|(id, _, _)| *id != body_id)
        .map(|(_, _, t)| t.as_str())
        .collect();
    assert_eq!(fallback_text, "日本語のテキスト", "fallback runs: {runs:?}");
    for (font_id, ids, text) in &runs {
        // Where Parley found no glyph (0), GPUI's shaper applies its own fallback; those
        // positions are painted as placeholder boxes, not with GPUI's choice.
        let gpui = gpui_glyphs(&*ts, text, *font_id);
        assert_eq!(gpui.len(), ids.len(), "run {text:?}");
        for (i, (p, g)) in ids.iter().zip(&gpui).enumerate() {
            assert!(
                *p == 0 || p == g,
                "run {text:?} glyph {i}: parley {p}, gpui {g}"
            );
        }
    }
    // Registering again changes nothing.
    let v1 = reg.version();
    reg.ensure_layout_fonts(&l, &*ts).unwrap();
    assert_eq!(reg.version(), v1);

    let ops = paint_ops(&l, &reg);
    let missing = ops
        .iter()
        .filter(|op| matches!(op, PaintOp::MissingGlyph { .. }))
        .count();
    assert_eq!(missing, 1, "one placeholder for U+0378");
    let glyphs = ops
        .iter()
        .filter(|op| matches!(op, PaintOp::Glyph { .. }))
        .count();
    assert_eq!(
        glyphs + 1,
        runs.iter().map(|(_, ids, _)| ids.len()).sum::<usize>()
    );
}

/// Lays out like the app, registering fallback fonts and laying out again as it does.
fn paragraph(
    reg: &mut FontRegistry,
    ts: &(dyn PlatformTextSystem + 'static),
    text: &str,
    mono: bool,
) -> Layout<Brush> {
    let base = mdt_text::BaseStyle {
        size: 16.0,
        line_height: 1.5,
        mono,
        bold: false,
        brush: Brush(0),
    };
    let mut lcx = LayoutContext::new();
    let p = mdt_text::layout_paragraph(reg, &mut lcx, text, base, &[], &[], 1000.0);
    reg.ensure_layout_fonts(&p.layout, ts).unwrap();
    mdt_text::layout_paragraph(reg, &mut lcx, text, base, &[], &[], 1000.0).layout
}

/// Emoji fall back to the system colour emoji font and paint as colour glyphs, not as
/// placeholder boxes or single-colour masks.
fn emoji_fall_back_to_a_colour_font() {
    let ts = platform_text_system();
    let mut reg = FontRegistry::new(&*ts).unwrap();
    let l = paragraph(&mut reg, &*ts, "go 🚀 now", true);
    let ops = paint_ops(&l, &reg);
    assert!(
        !ops.iter()
            .any(|op| matches!(op, PaintOp::MissingGlyph { .. })),
        "{ops:?}"
    );
    let emoji: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, PaintOp::Emoji { .. }))
        .collect();
    assert_eq!(emoji.len(), 1, "{ops:?}");
    // Painted at the text size, with the platform's advance: on macOS, CoreText's 21 px at
    // 16 px (the font's own advance is 16), so the image does not run into the next word.
    let PaintOp::Emoji { size, x, .. } = *emoji[0] else {
        unreachable!()
    };
    assert_eq!(size, 16.0);
    let next = ops
        .iter()
        .find_map(|op| match op {
            PaintOp::Glyph { x: gx, .. } if *gx > x => Some(*gx),
            _ => None,
        })
        .unwrap();
    let advance = next - x;
    if cfg!(target_os = "macos") {
        assert!((advance - 21.0).abs() < 0.5, "emoji advance {advance}");
    }
}

/// An emoji presentation selector picks the colour emoji even where the text font has a
/// plain glyph for the character (Meslo and Inter both have U+2764).
fn emoji_selector_picks_the_emoji_font() {
    let ts = platform_text_system();
    let mut reg = FontRegistry::new(&*ts).unwrap();
    let l = paragraph(&mut reg, &*ts, "love ❤️ and ❤", false);
    let ops = paint_ops(&l, &reg);
    let emoji = ops
        .iter()
        .filter(|op| matches!(op, PaintOp::Emoji { .. }))
        .count();
    assert_eq!(emoji, 1, "{ops:?}");
}

fn main() {
    let tests: [(&str, fn()); 5] = [
        (
            "bundled_faces_give_identical_glyph_ids_in_parley_and_gpui",
            bundled_faces_give_identical_glyph_ids_in_parley_and_gpui,
        ),
        (
            "every_bundled_face_maps_to_a_distinct_font_id",
            every_bundled_face_maps_to_a_distinct_font_id,
        ),
        (
            "fallback_fonts_and_missing_glyphs",
            fallback_fonts_and_missing_glyphs,
        ),
        (
            "emoji_fall_back_to_a_colour_font",
            emoji_fall_back_to_a_colour_font,
        ),
        (
            "emoji_selector_picks_the_emoji_font",
            emoji_selector_picks_the_emoji_font,
        ),
    ];
    for (name, test) in tests {
        test();
        println!("test {name} ... ok");
    }
}
