//! Reader frame time while background results land: scrolls a document in an offscreen
//! reader, and between frames finishes all the work the frame asked for (highlighting,
//! math, diagrams, images), as if it landed then. Prints the time `prepare` (layout of the
//! visible units) takes per frame.
//! `cargo run --release -p mdt-app --example landing_timing -- bench-docs/media/media-heavy.md`

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::HeadlessAppContext;
use mdt_app::reader::view::{Reader, fonts};

fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("a Markdown file"));
    let frames: usize = std::env::args().nth(2).map_or(Ok(600), |s| s.parse())?;
    let text = std::fs::read_to_string(&path)?;
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let mut reader = Reader::new(mdt_markdown::parse(&text), 1.0);
    reader.images.doc_dir = path.parent().map(|p| p.to_owned());
    reader.resize(700.0, 900.0);
    let mut times = Vec::with_capacity(frames);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        for _ in 0..frames {
            reader.scroll_by(60.0);
            let t = Instant::now();
            std::hint::black_box(reader.prepare(&mut fonts, app));
            times.push(t.elapsed());
            for (info, code) in reader.take_pending_highlights() {
                reader.highlights.compute(&info, &code);
                reader.highlight_landed(&mdt_highlight::Cache::key(&info, &code));
            }
            let batch = reader.math.take_pending();
            let keys: Vec<_> = batch.iter().map(|(l, d)| mdt_math::key(l, *d)).collect();
            reader.math.cache.compute_batch(&batch);
            reader.math.landed(&keys);
            // Runs image and diagram work and reports that results landed.
            reader.run_images_blocking();
        }
    });
    times.sort_unstable();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let total: Duration = times.iter().sum();
    println!(
        "{} frames: total {:.1} ms, median {:.3} ms, p99 {:.3} ms, max {:.3} ms",
        times.len(),
        ms(total),
        ms(times[times.len() / 2]),
        ms(times[times.len() * 99 / 100]),
        ms(times[times.len() - 1]),
    );
    Ok(())
}
