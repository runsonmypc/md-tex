//! Renders `docs/visual/reader-check.md` offscreen in Source mode (soft wrap on and off)
//! and Split mode, light and dark, writing `docs/visual/editor-*.png`:
//! `cargo run -p mdt-app --example editor_check`.

use std::sync::Arc;

use gpui::{AppContext, HeadlessAppContext, px, size};
use mdt_app::settings::ViewMode;
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
        for (name, view_mode, wrap) in [
            ("source", ViewMode::Source, true),
            ("source-nowrap", ViewMode::Source, false),
            ("split", ViewMode::Split, true),
        ] {
            let doc = mdt_doc::Document::open(&dir.join("reader-check.md"))?;
            let width = 1400.0;
            let window = cx.open_window(size(px(width), px(1000.0)), |window, cx| {
                cx.new(|cx| {
                    let mut v = DocumentView::new(doc, window, cx);
                    v.set_mode(view_mode, width, cx);
                    let rope = v.doc.rope().clone();
                    let mut e = v.editor.borrow_mut();
                    e.set_soft_wrap(&rope, wrap);
                    // A selection across lines to show its highlight.
                    e.selection = mdt_doc::Selection {
                        anchor: 40,
                        head: 120,
                    };
                    drop(e);
                    v
                })
            })?;
            cx.run_until_parked();
            let image = cx.capture_screenshot(window.into())?;
            let path = dir.join(format!("editor-{name}-{theme}.png"));
            image.save(&path)?;
            println!(
                "wrote {} ({}x{})",
                path.display(),
                image.width(),
                image.height()
            );
        }
    }
    Ok(())
}
