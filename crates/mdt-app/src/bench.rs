//! Measurement hooks for the budget harness (`bench` feature). Driven by
//! environment variables; each run prints one JSON object on stdout and quits.
//!
//! - `MDT_LAUNCH_NS`: wall-clock epoch nanoseconds taken just before spawn. Reports
//!   `startup_first_text_frame_ms` once the first frame with document text is drawn.
//! - `MDT_BENCH=large`: as startup, then also waits for the full parse and reports
//!   `large_file_headings_ready_ms` (when every heading is in the palette).
//! - `MDT_BENCH=scroll`: after startup and 30 more frames, scrolls one fixed step per frame
//!   for 1,200 frames and reports per-frame CPU time (render to end of paint, p99) and
//!   missed frames.
//! - `MDT_BENCH=idle`: after startup, waits 5 s for background work, then counts frames
//!   and process CPU time for 60 s.
//! - `MDT_BENCH=hold`: after startup, waits 3 s for background work, prints a line and
//!   stays open so the harness can read the process's memory.
//! - `MDT_BENCH=type`, `switch`, `preview`, `find`: scripted input after startup (see
//!   `drive`). Each operation's time is its handler plus the next frame's CPU time (render
//!   to end of paint); `preview` is wall time from the keystroke to the frame that shows
//!   the new parse.
//! - `MDT_BENCH=media`: pages through the document top to bottom and back, then jumps to 30
//!   places far apart, waiting at each stop for its equations, diagrams and images, and
//!   reports their latencies (from the first frame an item is on screen without a raster).
//!   Prints `{"page": n}` lines on the way so the harness can sample memory.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, WindowHandle};

use crate::settings::ViewMode;
use crate::view::DocumentView;

/// 10 s at 120 Hz.
pub const SCROLL_FRAMES: usize = 1200;
pub const SCROLL_STEP: f32 = 40.0;
/// Frames drawn, not scrolled, after the first text frame. On macOS the frame after it
/// comes 60–80 ms later whatever the document (the window settling after launch, with
/// about 1 ms of work), which is startup, not scrolling.
const SCROLL_WARMUP: usize = 30;

/// Operations per scripted run.
const TYPE_KEYS: usize = 1000;
const SWITCHES: usize = 20;
const PREVIEW_KEYS: usize = 50;
const FIND_QUERY: &str = "diagram preview";
const JUMPS: usize = 30;

#[derive(Default)]
pub struct Bench {
    pub mode: Mode,
    launch_ns: Option<u128>,
    startup_ms: Option<f64>,
    headings_ms: Option<f64>,
    frames: usize,
    /// Frames ended since the first text frame.
    frames_since_startup: usize,
    frame_cpu: Vec<Duration>,
    frame_starts: Vec<Instant>,
    current: Option<Instant>,
    idle: Option<(Instant, Duration, usize)>,
    /// Start of the last frame and the end of its last paint, for scripted operations.
    frame_start: Option<Instant>,
    paint_end: Option<Instant>,
    /// The model epoch the current frame renders, and the one the last paint showed.
    frame_epoch: u64,
    painted_epoch: u64,
    done: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    #[default]
    Off,
    Startup,
    Large,
    Scroll,
    Idle,
    Hold,
    Type,
    Switch,
    Preview,
    Find,
    Media,
}

thread_local! {
    static BENCH: RefCell<Bench> = RefCell::new(Bench::from_env());
}

impl Bench {
    fn from_env() -> Self {
        let launch_ns = std::env::var("MDT_LAUNCH_NS")
            .ok()
            .and_then(|v| v.parse().ok());
        let mode = match (launch_ns, std::env::var("MDT_BENCH").as_deref()) {
            (None, _) => Mode::Off,
            (_, Ok("large")) => Mode::Large,
            (_, Ok("scroll")) => Mode::Scroll,
            (_, Ok("idle")) => Mode::Idle,
            (_, Ok("hold")) => Mode::Hold,
            (_, Ok("type")) => Mode::Type,
            (_, Ok("switch")) => Mode::Switch,
            (_, Ok("preview")) => Mode::Preview,
            (_, Ok("find")) => Mode::Find,
            (_, Ok("media")) => Mode::Media,
            _ => Mode::Startup,
        };
        Bench {
            mode,
            launch_ns,
            ..Default::default()
        }
    }
}

fn mode() -> Mode {
    BENCH.with(|b| b.borrow().mode)
}

fn since_launch_ms(b: &Bench) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    (now - b.launch_ns.unwrap_or(now)) as f64 / 1e6
}

/// Called when the document view starts rendering a frame of model `epoch`. Returns the
/// scroll step.
pub fn frame_start(epoch: u64) -> f32 {
    BENCH.with(|b| {
        let mut b = b.borrow_mut();
        let now = Instant::now();
        b.frame_epoch = epoch;
        b.current = Some(now);
        b.frame_start = Some(now);
        b.paint_end = None;
        let scrolling = b.mode == Mode::Scroll
            && b.frames_since_startup >= SCROLL_WARMUP
            && b.frame_starts.len() < SCROLL_FRAMES;
        if scrolling {
            b.frame_starts.push(now);
            SCROLL_STEP
        } else {
            0.0
        }
    })
}

/// Called after the editor painted.
pub fn editor_painted() {
    BENCH.with(|b| {
        let mut b = b.borrow_mut();
        b.paint_end = Some(Instant::now());
        b.painted_epoch = b.frame_epoch;
    });
}

/// Called when a full parse of the document lands (large documents parse in the background).
pub fn full_model() {
    BENCH.with(|b| {
        let mut b = b.borrow_mut();
        if b.mode != Mode::Off && b.headings_ms.is_none() {
            b.headings_ms = Some(since_launch_ms(&b));
        }
    });
}

/// Called after the reader painted a frame; `has_text` when document text was drawn.
/// Returns true when the caller should render another frame (scrolling).
pub fn frame_end(has_text: bool, cx: &mut App) -> bool {
    let report = BENCH.with(|b| {
        let mut b = b.borrow_mut();
        if b.mode == Mode::Off || b.done {
            return None;
        }
        b.paint_end = Some(Instant::now());
        b.painted_epoch = b.frame_epoch;
        b.frames += 1;
        if let Some(start) = b.current.take()
            && b.mode == Mode::Scroll
            && b.frame_cpu.len() < b.frame_starts.len()
        {
            b.frame_cpu.push(start.elapsed());
        }
        if b.startup_ms.is_some() {
            b.frames_since_startup += 1;
        } else if has_text {
            b.startup_ms = Some(since_launch_ms(&b));
        }
        match b.mode {
            Mode::Startup if b.startup_ms.is_some() => Some(report(&mut b)),
            Mode::Scroll if b.frame_cpu.len() >= SCROLL_FRAMES => Some(report(&mut b)),
            _ => None,
        }
    });
    if let Some(json) = report {
        println!("{json}");
        // Quitting from inside a paint is ignored on Linux; quit from a task instead.
        cx.spawn(async move |cx| cx.update(crate::view::quit_now))
            .detach();
        return false;
    }
    BENCH.with(|b| {
        let b = b.borrow();
        b.mode == Mode::Scroll && !b.done
    })
}

/// Starts the measurements that run after startup (call once from `main`).
pub fn start(cx: &mut App) {
    match mode() {
        Mode::Idle => start_idle(cx),
        Mode::Off | Mode::Startup | Mode::Scroll => {}
        _ => {
            cx.spawn(async move |cx| drive(cx).await).detach();
        }
    }
}

fn start_idle(cx: &mut App) {
    cx.spawn(async move |cx| {
        let timer = |ms| cx.background_executor().timer(Duration::from_millis(ms));
        timer(5000).await;
        BENCH.with(|b| {
            let mut b = b.borrow_mut();
            let frames = b.frames;
            b.idle = Some((Instant::now(), cpu_time(), frames));
        });
        timer(60_000).await;
        let json = BENCH.with(|b| report(&mut b.borrow_mut()));
        println!("{json}");
        cx.update(crate::view::quit_now);
    })
    .detach();
}

async fn sleep(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Waits for the next frame to finish painting.
async fn next_frame(cx: &mut AsyncApp) {
    for _ in 0..2000 {
        sleep(cx, 1).await;
        if BENCH.with(|b| b.borrow().paint_end.is_some()) {
            return;
        }
    }
}

/// Runs `op` on the view and returns its handler time plus the next frame's CPU time.
async fn timed(
    cx: &mut AsyncApp,
    w: WindowHandle<DocumentView>,
    op: impl FnOnce(&mut DocumentView, &mut gpui::Window, &mut gpui::Context<DocumentView>),
) -> f64 {
    BENCH.with(|b| b.borrow_mut().paint_end = None);
    let t0 = Instant::now();
    let _ = w.update(cx, |v, window, cx| {
        op(v, window, cx);
        cx.notify();
    });
    let handler = t0.elapsed();
    next_frame(cx).await;
    let frame = BENCH.with(|b| {
        let b = b.borrow();
        match (b.frame_start, b.paint_end) {
            (Some(s), Some(e)) if e >= s => e - s,
            _ => Duration::ZERO,
        }
    });
    (handler + frame).as_secs_f64() * 1e3
}

fn window_w(window: &gpui::Window) -> f32 {
    f32::from(window.viewport_size().width)
}

/// Scripted runs: waits for the first frame, then drives the window and prints the result.
async fn drive(cx: &mut AsyncApp) {
    while BENCH.with(|b| b.borrow().startup_ms.is_none()) {
        sleep(cx, 10).await;
    }
    let mode = mode();
    let Some(w) = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find_map(|w| w.downcast::<DocumentView>())
    }) else {
        return;
    };
    // Large documents first show a parsed prefix; scripts start on the whole document.
    while mode != Mode::Large && !w.update(cx, |v, _, _| v.parsed()).unwrap_or(true) {
        sleep(cx, 5).await;
    }
    let mut fields: Vec<String> = Vec::new();
    match mode {
        Mode::Large => {
            while BENCH.with(|b| b.borrow().headings_ms.is_none()) {
                sleep(cx, 5).await;
            }
            let ms = BENCH.with(|b| b.borrow().headings_ms.unwrap());
            fields.push(format!("\"large_file_headings_ready_ms\": {ms:.2}"));
        }
        Mode::Hold => {
            sleep(cx, 3000).await;
            println!("{{\"ready\": true}}");
            return;
        }
        Mode::Type => {
            let _ = w.update(cx, |v, window, cx| {
                v.set_mode(ViewMode::Source, window_w(window), cx);
                v.bench_cursor_mid(cx);
            });
            next_frame(cx).await;
            let mut times = Vec::new();
            for i in 0..TYPE_KEYS {
                let ch = if i % 6 == 5 { " " } else { "x" };
                times.push(
                    timed(cx, w, |v, _, cx| {
                        v.insert(ch, mdt_doc::EditKind::Typing, cx)
                    })
                    .await,
                );
            }
            fields.push(format!(
                "\"input_latency_median_ms\": {:.3}",
                pct(&times, 50)
            ));
            fields.push(format!("\"input_latency_p99_ms\": {:.3}", pct(&times, 99)));
        }
        Mode::Switch => {
            let mut to_source = Vec::new();
            let mut to_reader = Vec::new();
            for _ in 0..SWITCHES {
                to_source.push(
                    timed(cx, w, |v, window, cx| {
                        v.set_mode(ViewMode::Source, window_w(window), cx)
                    })
                    .await,
                );
                to_reader.push(
                    timed(cx, w, |v, window, cx| {
                        v.set_mode(ViewMode::Reader, window_w(window), cx)
                    })
                    .await,
                );
            }
            fields.push(format!("\"mode_switch_ms\": {:.3}", max(&to_source)));
            fields.push(format!(
                "\"mode_switch_median_ms\": {:.3}",
                pct(&to_source, 50)
            ));
            fields.push(format!(
                "\"mode_switch_to_reader_max_ms\": {:.3}",
                max(&to_reader)
            ));
        }
        Mode::Preview => {
            let _ = w.update(cx, |v, window, cx| {
                v.set_mode(ViewMode::Split, window_w(window), cx);
                v.bench_cursor_mid(cx);
                v.reveal_cursor_in_preview();
            });
            sleep(cx, 1000).await;
            let mut times = Vec::new();
            for _ in 0..PREVIEW_KEYS {
                let t0 = Instant::now();
                let Ok(before) = w.update(cx, |v, _, cx| {
                    let e = v.model_epoch();
                    v.insert("x", mdt_doc::EditKind::Typing, cx);
                    e
                }) else {
                    return;
                };
                // Waits for a painted frame that shows the new parse.
                let end = loop {
                    sleep(cx, 1).await;
                    let (epoch, end) = BENCH.with(|b| {
                        let b = b.borrow();
                        (b.painted_epoch, b.paint_end)
                    });
                    if epoch > before
                        && let Some(end) = end
                    {
                        break end;
                    }
                };
                times.push((end - t0).as_secs_f64() * 1e3);
                sleep(cx, 30).await;
            }
            fields.push(format!("\"preview_update_ms\": {:.3}", max(&times)));
            fields.push(format!(
                "\"preview_update_median_ms\": {:.3}",
                pct(&times, 50)
            ));
        }
        Mode::Find => {
            let _ = w.update(cx, |v, window, cx| v.open_find(false, window, cx));
            next_frame(cx).await;
            let mut times = Vec::new();
            for ch in FIND_QUERY.chars() {
                let s = ch.to_string();
                times.push(timed(cx, w, |v, _, cx| v.find_type(&s, cx)).await);
            }
            fields.push(format!("\"find_keystroke_ms\": {:.3}", max(&times)));
            fields.push(format!(
                "\"find_keystroke_median_ms\": {:.3}",
                pct(&times, 50)
            ));
        }
        Mode::Media => media(cx, w, &mut fields).await,
        _ => {}
    }
    let json = BENCH.with(|b| {
        let mut b = b.borrow_mut();
        let mut s = report(&mut b);
        if b.mode == Mode::Large {
            // `report` already has startup; the large-file budget names it separately.
            s = s.replacen(
                "\"startup_first_text_frame_ms\"",
                "\"large_file_first_frame_ms\"",
                1,
            );
        }
        s
    });
    let extra = fields.join(", ");
    let json = if extra.is_empty() {
        json
    } else {
        format!("{}, {extra}}}", &json[..json.len() - 1])
    };
    println!("{json}");
    cx.update(crate::view::quit_now);
}

/// Pages down to the end and back up, waiting at each page until its content has landed.
async fn media(cx: &mut AsyncApp, w: WindowHandle<DocumentView>, fields: &mut Vec<String>) {
    let mut page = 0;
    for down in [true, false] {
        loop {
            settle(cx, &mut page).await;
            let Ok(moved) = w.update(cx, |v, _, cx| {
                let mut r = v.reader.borrow_mut();
                let before = r.anchor.scroll_y(&r.heights);
                let step = r.viewport_h();
                r.scroll_by(if down { step } else { -step });
                let after = r.anchor.scroll_y(&r.heights);
                cx.notify();
                (after - before).abs() > 0.5
            }) else {
                return;
            };
            if !moved {
                break;
            }
        }
    }
    // Jumps far apart, so nothing there is rendered ahead: images and diagrams start cold.
    for k in 0..JUMPS {
        let _ = w.update(cx, |v, _, cx| {
            let mut r = v.reader.borrow_mut();
            let n = r.heights.len();
            r.scroll_to_unit(n * (k * 7 % JUMPS) / JUMPS, 0.0);
            cx.notify();
        });
        settle(cx, &mut page).await;
    }
    let m = MEDIA.lock().unwrap();
    fields.push(format!(
        "\"equation_first_ms\": {:.3}",
        m.equation_first.unwrap_or(f64::NAN)
    ));
    fields.push(format!(
        "\"equation_median_ms\": {:.3}",
        pct(&m.equations, 50)
    ));
    fields.push(format!("\"equation_max_ms\": {:.3}", max(&m.equations)));
    fields.push(format!("\"equations_rendered\": {}", m.equations.len() + 1));
    fields.push(format!("\"diagram_ms\": {:.3}", max(&m.diagrams)));
    fields.push(format!(
        "\"diagram_median_ms\": {:.3}",
        pct(&m.diagrams, 50)
    ));
    fields.push(format!("\"diagrams_rendered\": {}", m.diagrams.len()));
    let first = |v: &[f64]| v.first().copied().unwrap_or(f64::NAN);
    fields.push(format!("\"diagram_first_ms\": {:.3}", first(&m.diagrams)));
    fields.push(format!("\"jpeg_12mp_first_ms\": {:.3}", first(&m.images)));
    fields.push(format!("\"jpeg_12mp_ms\": {:.3}", max(&m.images)));
    fields.push(format!(
        "\"jpeg_12mp_median_ms\": {:.3}",
        pct(&m.images, 50)
    ));
    fields.push(format!("\"images_rendered\": {}", m.images.len()));
    fields.push(format!("\"media_pages\": {page}"));
}

/// Waits for the current page's work (at most 5 s), then reports the page for memory.
async fn settle(cx: &mut AsyncApp, page: &mut usize) {
    let t0 = Instant::now();
    loop {
        sleep(cx, 20).await;
        let busy = MEDIA.lock().unwrap().outstanding > 0;
        if !busy || t0.elapsed() > Duration::from_secs(5) {
            break;
        }
    }
    sleep(cx, 20).await;
    println!("{{\"page\": {page}}}");
    *page += 1;
}

/// Latencies of background content, recorded from the reader's task sites.
#[derive(Default)]
struct Media {
    outstanding: usize,
    /// Request to landing of the first batch of equations.
    equation_first: Option<f64>,
    /// Typesetting time of each later equation.
    equations: Vec<f64>,
    first_typeset: bool,
    /// First request for a diagram or image to the landing of its raster.
    diagrams: Vec<f64>,
    images: Vec<f64>,
    /// Items on screen without a raster, since when.
    waiting: HashMap<(Kind, String), Instant>,
}

static MEDIA: LazyLock<Mutex<Media>> = LazyLock::new(Default::default);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Diagram,
    Image,
}

/// Background work for `key` was requested.
pub fn begin(key: String) -> String {
    MEDIA.lock().unwrap().outstanding += 1;
    key
}

/// The items on screen without a raster this frame. An item's latency runs from the first
/// frame it waited in to the landing of its raster; items that leave the screen first are
/// not counted.
pub fn waiting(items: Vec<(Kind, String)>) {
    let now = Instant::now();
    let mut m = MEDIA.lock().unwrap();
    m.waiting.retain(|k, _| items.contains(k));
    for k in items {
        m.waiting.entry(k).or_insert(now);
    }
}

/// The work from `begin` landed on the main thread; `last` when it made the raster.
pub fn landed(kind: Kind, key: String, last: bool) {
    let mut m = MEDIA.lock().unwrap();
    m.outstanding = m.outstanding.saturating_sub(1);
    if last && let Some(t0) = m.waiting.remove(&(kind, key)) {
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        match kind {
            Kind::Diagram => m.diagrams.push(ms),
            Kind::Image => m.images.push(ms),
        }
    }
}

pub fn equations_begin() -> Instant {
    MEDIA.lock().unwrap().outstanding += 1;
    Instant::now()
}

/// One equation was typeset in `took` (called on the background thread).
pub fn equation_typeset(took: Duration) {
    let mut m = MEDIA.lock().unwrap();
    if m.first_typeset {
        m.equations.push(took.as_secs_f64() * 1e3);
    }
    m.first_typeset = true;
}

pub fn equations_landed(t0: Instant) {
    let mut m = MEDIA.lock().unwrap();
    m.outstanding = m.outstanding.saturating_sub(1);
    if m.equation_first.is_none() {
        m.equation_first = Some(t0.elapsed().as_secs_f64() * 1e3);
    }
}

fn pct(v: &[f64], p: usize) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get((v.len() * p / 100).min(v.len().saturating_sub(1)))
        .copied()
        .unwrap_or(f64::NAN)
}

fn max(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::NAN, f64::max)
}

fn report(b: &mut Bench) -> String {
    b.done = true;
    let mut fields = vec![format!(
        "\"startup_first_text_frame_ms\": {:.2}",
        b.startup_ms.unwrap_or(f64::NAN)
    )];
    if b.mode == Mode::Scroll {
        let mut cpu: Vec<f64> = b.frame_cpu.iter().map(|d| d.as_secs_f64() * 1e3).collect();
        cpu.sort_by(f64::total_cmp);
        let p99 = cpu.get(cpu.len() * 99 / 100).copied().unwrap_or(f64::NAN);
        // Frame intervals; a frame is missed when its interval exceeds 1.5 display periods.
        let mut intervals: Vec<f64> = b
            .frame_starts
            .windows(2)
            .map(|w| (w[1] - w[0]).as_secs_f64() * 1e3)
            .collect();
        let mut sorted = intervals.clone();
        sorted.sort_by(f64::total_cmp);
        let period = sorted.get(sorted.len() / 2).copied().unwrap_or(f64::NAN);
        intervals.retain(|i| *i > period * 1.5);
        fields.push(format!("\"scroll_frame_cpu_p99_ms\": {p99:.3}"));
        fields.push(format!("\"scroll_missed_frames\": {}", intervals.len()));
        fields.push(format!("\"scroll_frame_period_ms\": {period:.3}"));
    }
    if let (Mode::Idle, Some((start, cpu0, frames0))) = (b.mode, b.idle) {
        let wall = start.elapsed().as_secs_f64();
        let cpu = (cpu_time() - cpu0).as_secs_f64();
        fields.push(format!("\"idle_frames_presented\": {}", b.frames - frames0));
        fields.push(format!("\"idle_cpu_percent\": {:.3}", cpu / wall * 100.0));
    }
    format!("{{{}}}", fields.join(", "))
}

/// User plus system CPU time of this process.
fn cpu_time() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage writes a full rusage struct into the pointer we pass.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(usage.ru_utime) + tv(usage.ru_stime)
}
