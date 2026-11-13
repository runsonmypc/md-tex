//! Renders `docs/visual/reader-check.md` offscreen at 1,400 and 500 px window widths in
//! light and dark themes, writing `docs/visual/reader-*.png` (and the wide reader at
//! 1,400 px, `reader-*-1400-wide.png`), and its math section at 300%
//! zoom (`docs/visual/math-*-300.png`) and its images-and-diagrams section
//! (`docs/visual/media-*.png`): `cargo run -p mdt-app --example reader_check`.

use std::sync::Arc;

use gpui::{AppContext, HeadlessAppContext, px, size};
use mdt_app::theme::ThemeMode;
use mdt_app::view::{CurrentThemeMode, DocumentView};

fn main() -> anyhow::Result<()> {
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/visual");
    for (mode, theme) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        cx.update(|app| app.set_global(CurrentThemeMode(mode)));
        for (width, wide) in [(1400.0, false), (500.0, false), (1400.0, true)] {
            let doc = mdt_doc::Document::open(&dir.join("reader-check.md"))?;
            let window = cx.open_window(size(px(width), px(1300.0)), |window, cx| {
                cx.new(|cx| {
                    let view = DocumentView::new(doc, window, cx);
                    view.reader.borrow_mut().wide = wide;
                    view
                })
            })?;
            settle(&mut cx, window.into())?;
            let suffix = if wide { "-wide" } else { "" };
            save(
                &mut cx,
                window.into(),
                &dir.join(format!("reader-{theme}-{width}{suffix}.png")),
            )?;
        }
        let text = std::fs::read_to_string(dir.join("reader-check.md"))?;
        // The images and diagrams section alone, resolved against the check's folder.
        let start = text.find("## Images").expect("images section");
        let end = text.find("## Math").expect("math section");
        let media = dir.join("media-check.md");
        std::fs::write(&media, &text[start..end])?;
        let doc = mdt_doc::Document::open(&media)?;
        let window = cx.open_window(size(px(1400.0), px(1300.0)), |window, cx| {
            cx.new(|cx| DocumentView::new(doc, window, cx))
        })?;
        settle(&mut cx, window.into())?;
        save(
            &mut cx,
            window.into(),
            &dir.join(format!("media-{theme}.png")),
        )?;
        std::fs::remove_file(&media)?;
        // The math section alone at 300%, to compare edges with body text.
        let math = &text[text.find("## Math").expect("math section")..];
        let doc = mdt_doc::Document::from_bytes(math.as_bytes(), None)?;
        let window = cx.open_window(size(px(1400.0), px(1300.0)), |window, cx| {
            cx.new(|cx| {
                let view = DocumentView::new(doc, window, cx);
                view.reader.borrow_mut().zoom = 3.0;
                view
            })
        })?;
        settle(&mut cx, window.into())?;
        save(
            &mut cx,
            window.into(),
            &dir.join(format!("math-{theme}-300.png")),
        )?;
    }
    Ok(())
}

/// Draws until background results (highlighting, math, images, diagrams) have landed.
fn settle(cx: &mut HeadlessAppContext, window: gpui::AnyWindowHandle) -> anyhow::Result<()> {
    for _ in 0..5 {
        cx.capture_screenshot(window)?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        cx.run_until_parked();
    }
    Ok(())
}

fn save(
    cx: &mut HeadlessAppContext,
    window: gpui::AnyWindowHandle,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    let image = cx.capture_screenshot(window)?;
    image.save(path)?;
    println!(
        "wrote {} ({}x{})",
        path.display(),
        image.width(),
        image.height()
    );
    Ok(())
}
