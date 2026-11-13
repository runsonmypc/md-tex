//! Renders `docs/visual/showcase.md` offscreen in the dark theme to
//! `docs/visual/showcase-dark.png`, the README's screenshot:
//! `cargo run -p mdt-app --example showcase`.

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
    cx.update(|app| app.set_global(CurrentThemeMode(ThemeMode::Dark)));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/visual");
    let doc = mdt_doc::Document::open(&dir.join("showcase.md"))?;
    let window = cx.open_window(size(px(880.0), px(830.0)), |window, cx| {
        cx.new(|cx| DocumentView::new(doc, window, cx))
    })?;
    // Draw until the math and the diagram have landed.
    for _ in 0..5 {
        cx.capture_screenshot(window.into())?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        cx.run_until_parked();
    }
    let image = cx.capture_screenshot(window.into())?;
    let path = dir.join("showcase-dark.png");
    image.save(&path)?;
    println!(
        "wrote {} ({}x{})",
        path.display(),
        image.width(),
        image.height()
    );
    Ok(())
}
