//! Images in the reader. Layout asks for each image's display size: the file
//! header is probed in the background, so space is reserved before decoding. Painting asks
//! for a raster at the device pixel size; rasters decode in the background into a cache
//! bounded by bytes, off-screen images evicted first.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use gpui::RenderImage;
use mdt_image::{ImageError, RasterCache, Source};

/// Raster cache bound: an internal tuning constant, not a setting.
pub const RASTER_BYTES: usize = 64 * 1024 * 1024;
/// Remote image bodies kept for the session, in bytes; an evicted one is fetched again.
const REMOTE_BYTES: usize = 32 * 1024 * 1024;
/// Directories watched for image changes at most: on Linux each is an inotify watch, a
/// per-user limit. Past it new directories are not watched.
const MAX_WATCHED_DIRS: usize = 256;

/// SVG images and diagrams see only the bundled fonts.
pub static SVG_FONTS: LazyLock<mdt_image::svg::Fonts> = LazyLock::new(|| {
    let faces: Vec<&'static [u8]> = mdt_text::fonts::BUNDLED.iter().map(|f| f.bytes).collect();
    mdt_image::svg::Fonts::new(&faces, "Inter", "Meslo LG M")
});

/// A raster's identity: the image and its pixel size.
pub type RasterKey = (Source, u32, u32);

#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    /// Display size in logical pixels; the raster, once decoded, fills it.
    Frame { w: f32, h: f32, source: Source },
    /// A placeholder with this message; `load` is the URL a click fetches.
    Placeholder {
        message: String,
        load: Option<String>,
    },
    /// The header is still being read.
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Job {
    Probe(Source),
    Decode(RasterKey),
}

type Body = Result<Arc<Vec<u8>>, ImageError>;

struct Shared {
    probes: HashMap<Source, Result<(u32, u32), ImageError>>,
    /// Remote bodies (and fetch errors), in memory for the session only, bounded by bytes.
    #[cfg_attr(not(feature = "remote-images"), allow(dead_code))]
    remote: RasterCache<String, Body>,
    /// Sources on screen in the last frame, for eviction.
    visible: HashSet<Source>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            probes: HashMap::new(),
            remote: RasterCache::new(REMOTE_BYTES),
            visible: HashSet::new(),
        }
    }
}

pub struct Images {
    pub doc_dir: Option<PathBuf>,
    /// The remote-images setting.
    pub remote_allowed: bool,
    /// Remote images loaded one at a time through their placeholder's action.
    allowed: HashSet<String>,
    shared: Arc<Mutex<Shared>>,
    rasters: Arc<Mutex<RasterCache<RasterKey, RenderImage>>>,
    pending: Vec<Job>,
    inflight: Arc<Mutex<HashSet<Job>>>,
    watcher: Option<mdt_image::Watcher>,
    /// Watched directories.
    watched: HashSet<PathBuf>,
    /// Probed files already considered for watching (those past the cap too).
    seen: HashSet<PathBuf>,
    /// Receives changed image paths from the watcher.
    changes: Option<futures::channel::mpsc::UnboundedReceiver<PathBuf>>,
    changes_tx: futures::channel::mpsc::UnboundedSender<PathBuf>,
}

impl Default for Images {
    fn default() -> Self {
        let (changes_tx, changes) = futures::channel::mpsc::unbounded();
        Self {
            doc_dir: None,
            remote_allowed: false,
            allowed: HashSet::new(),
            shared: Default::default(),
            rasters: Arc::new(Mutex::new(RasterCache::new(RASTER_BYTES))),
            pending: Vec::new(),
            inflight: Default::default(),
            watcher: None,
            watched: HashSet::new(),
            seen: HashSet::new(),
            changes: Some(changes),
            changes_tx,
        }
    }
}

/// A unit of background work; `run` it off the UI thread.
pub struct Work {
    job: Job,
    shared: Arc<Mutex<Shared>>,
    rasters: Arc<Mutex<RasterCache<RasterKey, RenderImage>>>,
    inflight: Arc<Mutex<HashSet<Job>>>,
}

impl Images {
    /// Layout's question: how to show image `src` with `<img>` sizes `attr` at `zoom` in a
    /// column `max_w` wide.
    pub fn resolve(
        &mut self,
        src: &str,
        attr: (Option<u32>, Option<u32>),
        zoom: f32,
        max_w: f32,
    ) -> Shown {
        let source = mdt_image::resolve(src, self.doc_dir.as_deref());
        if let Source::Unsupported(s) = &source {
            let scheme = s.split(':').next().unwrap_or("");
            return Shown::Placeholder {
                message: format!("unsupported image address ({scheme}:)"),
                load: None,
            };
        }
        if let Source::Remote(url) = &source
            && !self.remote_allowed
            && !self.allowed.contains(url)
        {
            return Shown::Placeholder {
                message: "Remote image. Click to load.".into(),
                load: Some(url.clone()),
            };
        }
        let probe = self.shared.lock().unwrap().probes.get(&source).cloned();
        match probe {
            Some(Ok(size)) => {
                let (w, h) = mdt_image::display_size(size, attr, zoom, max_w);
                Shown::Frame { w, h, source }
            }
            Some(Err(e)) => Shown::Placeholder {
                message: e.to_string(),
                load: None,
            },
            None => {
                self.queue(Job::Probe(source));
                Shown::Pending
            }
        }
    }

    /// Paint's question: the raster for `source` at `w`×`h` device pixels, if decoded.
    /// A missing one is queued.
    pub fn raster(&mut self, source: &Source, w: u32, h: u32) -> Option<Arc<RenderImage>> {
        let key = (source.clone(), w.max(1), h.max(1));
        let r = self.rasters.lock().unwrap().get(&key);
        // A file that failed to decode is not tried again: it shows its placeholder once
        // laid out again (or until it changes on disk).
        let failed = || matches!(self.shared.lock().unwrap().probes.get(source), Some(Err(_)));
        if r.is_none() && !failed() {
            self.queue(Job::Decode(key));
        }
        r
    }

    fn queue(&mut self, job: Job) {
        if self.inflight.lock().unwrap().insert(job.clone()) {
            self.pending.push(job);
        }
    }

    /// Sets the sources on screen, which eviction keeps longest.
    pub fn set_visible(&mut self, visible: HashSet<Source>) {
        self.shared.lock().unwrap().visible = visible;
    }

    /// Lets one remote image load (its placeholder's action).
    pub fn allow(&mut self, url: &str) {
        self.allowed.insert(url.to_owned());
    }

    /// Work waiting to run; probes first, since layout waits for them.
    pub fn take_pending(&mut self) -> Vec<Work> {
        let mut jobs = std::mem::take(&mut self.pending);
        jobs.sort_by_key(|j| !matches!(j, Job::Probe(_)));
        jobs.into_iter()
            .map(|job| Work {
                job,
                shared: self.shared.clone(),
                rasters: self.rasters.clone(),
                inflight: self.inflight.clone(),
            })
            .collect()
    }

    /// Whether any job is queued or running.
    pub fn busy(&self) -> bool {
        !self.inflight.lock().unwrap().is_empty()
    }

    /// Watches the local files of probed images (their directories, at most
    /// `MAX_WATCHED_DIRS`); changes arrive on `take_changes`.
    pub fn watch_probed(&mut self) {
        let paths: Vec<PathBuf> = {
            let shared = self.shared.lock().unwrap();
            shared
                .probes
                .keys()
                .filter_map(|s| match s {
                    Source::Local(p) if !self.seen.contains(p) => Some(p.clone()),
                    _ => None,
                })
                .collect()
        };
        if paths.is_empty() {
            return;
        }
        if self.watcher.is_none() {
            self.watcher = self.new_watcher();
        }
        for p in paths {
            self.seen.insert(p.clone());
            let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
            // The first directories stay watched: a watcher cannot stop watching one, so
            // making room would mean rebuilding it on the UI thread for every new one.
            if !self.watched.contains(&dir) {
                if self.watched.len() >= MAX_WATCHED_DIRS {
                    continue;
                }
                self.watched.insert(dir);
            }
            if let Some(w) = &mut self.watcher {
                w.watch(&p);
            }
        }
    }

    fn new_watcher(&self) -> Option<mdt_image::Watcher> {
        let tx = self.changes_tx.clone();
        mdt_image::Watcher::new(move |p| {
            let _ = tx.unbounded_send(p);
        })
        .ok()
    }

    /// The stream of changed image paths (taken once by the owner).
    pub fn take_changes(&mut self) -> Option<futures::channel::mpsc::UnboundedReceiver<PathBuf>> {
        self.changes.take()
    }

    /// Forgets everything known about a changed file, so it is probed and decoded again.
    pub fn invalidate(&mut self, path: &Path) {
        let source = Source::Local(path.to_owned());
        self.shared.lock().unwrap().probes.remove(&source);
        self.rasters
            .lock()
            .unwrap()
            .retain(|(s, _, _)| *s != source);
    }
}

impl Work {
    /// Runs the work here; returns whether layout changed (a header read, or a decode
    /// that failed and turns the frame into a placeholder).
    pub fn run(self) -> bool {
        futures::executor::block_on(self.run_async())
    }

    /// `run`, waiting for a remote image's body without holding the thread: fetches run
    /// on threads of their own.
    pub async fn run_async(self) -> bool {
        let (Job::Probe(source) | Job::Decode((source, _, _))) = &self.job;
        let body = match source {
            Source::Remote(url) => self.remote(url).await,
            _ => Err(ImageError::NotFound),
        };
        let layout = match &self.job {
            Job::Probe(source) => {
                let result = match source {
                    Source::Local(path) => mdt_image::probe(path),
                    Source::Remote(url) => body.and_then(|bytes| {
                        if mdt_image::is_svg(Path::new(url), &bytes) {
                            mdt_image::svg::size(&bytes).map_err(ImageError::Decode)
                        } else {
                            mdt_image::probe_bytes(&bytes)
                        }
                    }),
                    Source::Unsupported(_) => Err(ImageError::NotFound),
                };
                self.shared
                    .lock()
                    .unwrap()
                    .probes
                    .insert(source.clone(), result);
                true
            }
            Job::Decode(key) => {
                let (source, w, h) = key;
                let result = match source {
                    Source::Local(path) => mdt_image::load(path, (*w, *h), &SVG_FONTS),
                    Source::Remote(url) => body.and_then(|bytes| {
                        mdt_image::decode_any(Path::new(url), &bytes, (*w, *h), &SVG_FONTS)
                    }),
                    Source::Unsupported(_) => Err(ImageError::NotFound),
                };
                match result {
                    Ok(raster) => {
                        let bytes = raster.bytes();
                        let image = Arc::new(render_image(raster));
                        let visible = self.shared.lock().unwrap().visible.clone();
                        self.rasters.lock().unwrap().insert(
                            key.clone(),
                            image,
                            bytes,
                            |(s, _, _)| visible.contains(s),
                        );
                        false
                    }
                    // A file whose header reads but whose data does not decodes to a
                    // placeholder; layout sees it through the probe.
                    Err(e) => {
                        self.shared
                            .lock()
                            .unwrap()
                            .probes
                            .insert(source.clone(), Err(e));
                        true
                    }
                }
            }
        };
        self.inflight.lock().unwrap().remove(&self.job);
        layout
    }

    /// Whether this work reads a header (and so changes layout), rather than decoding
    /// pixels.
    pub fn changes_layout(&self) -> bool {
        matches!(self.job, Job::Probe(_))
    }

    /// The image this work is for (bench timing).
    #[cfg(feature = "bench")]
    pub fn bench_key(&self) -> String {
        match &self.job {
            Job::Probe(s) | Job::Decode((s, _, _)) => format!("{s:?}"),
        }
    }

    #[cfg(feature = "remote-images")]
    async fn remote(&self, url: &str) -> Body {
        if let Some(r) = self.shared.lock().unwrap().remote.get(&url.to_owned()) {
            return (*r).clone();
        }
        let r = fetch(url.to_owned()).await;
        let bytes = r.as_ref().map_or(0, |b| b.len()) + url.len();
        self.shared.lock().unwrap().remote.insert(
            url.to_owned(),
            Arc::new(r.clone()),
            bytes,
            |_| false,
        );
        r
    }

    #[cfg(not(feature = "remote-images"))]
    async fn remote(&self, _url: &str) -> Body {
        Err(ImageError::Fetch("built without remote images".into()))
    }
}

/// How many remote fetches run at once, each on a thread of its own. They ran on GPUI's
/// background threads (a fixed pool on Linux), where a few slow or silent servers held
/// every thread for minutes while math, diagrams and local images waited.
#[cfg(feature = "remote-images")]
const FETCHERS: usize = 8;

#[cfg(feature = "remote-images")]
type FetchJob = (
    Box<dyn FnOnce() -> Body + Send>,
    futures::channel::oneshot::Sender<Body>,
);

/// Fetches `url` on a fetch thread.
#[cfg(feature = "remote-images")]
async fn fetch(url: String) -> Body {
    off_thread(move || mdt_image::remote::fetch(&url).map(Arc::new)).await
}

/// Runs `fetch` on one of the `FETCHERS` threads (after those queued before it).
#[cfg(feature = "remote-images")]
async fn off_thread(fetch: impl FnOnce() -> Body + Send + 'static) -> Body {
    static QUEUE: std::sync::OnceLock<std::sync::mpsc::Sender<FetchJob>> =
        std::sync::OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<FetchJob>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..FETCHERS {
            let rx = rx.clone();
            // Without the thread, sends fail (or wait for the others) below.
            let _ = std::thread::Builder::new()
                .name(format!("md-tex-fetch-{i}"))
                .spawn(move || {
                    loop {
                        let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        let Ok((fetch, done)) = job else { return };
                        let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(fetch))
                            .unwrap_or_else(|_| Err(ImageError::Fetch("the fetch failed".into())));
                        let _ = done.send(body);
                    }
                });
        }
        tx
    });
    let (done, body) = futures::channel::oneshot::channel();
    let lost = || ImageError::Fetch("no fetch thread".into());
    if queue.send((Box::new(fetch), done)).is_err() {
        return Err(lost());
    }
    body.await.unwrap_or_else(|_| Err(lost()))
}

/// GPUI wants BGRA with straight alpha. The image gets a transparent border of `PAD`
/// pixels: GPUI packs images into one texture with nothing between them, so an image drawn
/// larger than its raster blended in a row of its neighbour at its edges. `paint_raster`
/// draws only the inside.
pub fn render_image(raster: mdt_image::Raster) -> RenderImage {
    let (w, h) = (raster.width as usize, raster.height as usize);
    let row = (w + 2 * PAD) * 4;
    let mut bgra = vec![0u8; row * (h + 2 * PAD)];
    for (y, src) in raster.rgba.chunks_exact(w * 4).enumerate() {
        let start = (y + PAD) * row + PAD * 4;
        let dst = &mut bgra[start..start + w * 4];
        dst.copy_from_slice(src);
        for px in dst.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
    let buffer = image::RgbaImage::from_raw(
        raster.width + 2 * PAD as u32,
        raster.height + 2 * PAD as u32,
        bgra,
    )
    .expect("raster size matches its pixels");
    RenderImage::new(vec![image::Frame::new(buffer)])
}

/// Transparent pixels around each image made by `render_image`.
pub const PAD: usize = 1;

/// Paints an image made by `render_image` so its inside fills `bounds`.
pub fn paint_raster(
    bounds: gpui::Bounds<gpui::Pixels>,
    image: Arc<RenderImage>,
    window: &mut gpui::Window,
) {
    let size = image.size(0);
    let inner = |outer: i32| (outer as f32 - 2.0 * PAD as f32).max(1.0);
    let pad = gpui::point(
        bounds.size.width * (PAD as f32 / inner(size.width.0)),
        bounds.size.height * (PAD as f32 / inner(size.height.0)),
    );
    let whole = gpui::Bounds::new(
        bounds.origin - pad,
        gpui::size(
            bounds.size.width + pad.x * 2.0,
            bounds.size.height + pad.y * 2.0,
        ),
    );
    let _ = window.paint_image(bounds, whole, gpui::Corners::default(), image, 0, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×1 raster becomes 4×3: BGRA inside a transparent border.
    #[test]
    fn rasters_get_a_transparent_border() {
        let raster = mdt_image::Raster {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 128],
        };
        let image = render_image(raster);
        assert_eq!(
            image.size(0),
            gpui::size(gpui::DevicePixels(4), gpui::DevicePixels(3))
        );
        let bytes = image.as_bytes(0).unwrap();
        let px = |x: usize, y: usize| &bytes[(y * 4 + x) * 4..][..4];
        assert_eq!(px(1, 1), [3, 2, 1, 255]);
        assert_eq!(px(2, 1), [6, 5, 4, 128]);
        for (x, y) in [(0, 0), (0, 1), (3, 1), (1, 0), (2, 2), (3, 2)] {
            assert_eq!(px(x, y), [0, 0, 0, 0], "({x}, {y})");
        }
    }

    #[test]
    fn remote_bodies_are_bounded_by_bytes() {
        let mut shared = Shared::default();
        for i in 0..20 {
            let body: Body = Ok(Arc::new(vec![0; 5 * 1024 * 1024]));
            shared.remote.insert(
                format!("https://example.com/{i}.png"),
                Arc::new(body),
                5 << 20,
                |_| false,
            );
        }
        assert!(shared.remote.used() <= REMOTE_BYTES);
        assert!(shared.remote.len() >= 5, "the most recent are kept");
    }

    /// Remote fetches ran on the GPUI background thread that asked for them: a few silent
    /// servers held every thread (a fixed pool on Linux) for minutes. They wait on fetch
    /// threads instead, more of them than there are fetch threads.
    #[cfg(feature = "remote-images")]
    #[test]
    fn slow_fetches_wait_off_the_calling_thread() {
        use std::future::Future;
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let mut fetches: Vec<_> = (0..FETCHERS * 2)
            .map(|i| {
                let released = released.clone();
                Box::pin(off_thread(move || {
                    let _ = released.lock().unwrap().recv();
                    Ok(Arc::new(vec![i as u8]))
                }))
            })
            .collect();
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        let start = std::time::Instant::now();
        for f in &mut fetches {
            assert!(f.as_mut().poll(&mut cx).is_pending());
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        for _ in 0..fetches.len() {
            release.send(()).unwrap();
        }
        for (i, f) in fetches.into_iter().enumerate() {
            assert_eq!(*futures::executor::block_on(f).unwrap(), vec![i as u8]);
        }
        // A panicking fetch is an error, and its thread fetches on.
        let failed = futures::executor::block_on(off_thread(|| panic!("boom")));
        assert!(matches!(failed, Err(ImageError::Fetch(_))));
        for _ in 0..FETCHERS {
            let ok = futures::executor::block_on(off_thread(|| Ok(Arc::new(vec![7]))));
            assert_eq!(*ok.unwrap(), vec![7]);
        }
    }

    /// Thousands of images in different directories must not exhaust the watches.
    #[test]
    fn watched_directories_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let mut images = Images::default();
        let n = MAX_WATCHED_DIRS + 44;
        {
            let mut shared = images.shared.lock().unwrap();
            for i in 0..n {
                let sub = dir.path().join(format!("d{i}"));
                std::fs::create_dir(&sub).unwrap();
                shared
                    .probes
                    .insert(Source::Local(sub.join("a.png")), Ok((1, 1)));
                // A second file in a directory already watched adds no directory.
                if i % 10 == 0 {
                    shared
                        .probes
                        .insert(Source::Local(sub.join("b.png")), Ok((1, 1)));
                }
            }
        }
        images.watch_probed();
        assert_eq!(images.watched.len(), MAX_WATCHED_DIRS);
        assert_eq!(images.seen.len(), n + n.div_ceil(10));
        // Nothing new: nothing changes.
        images.watch_probed();
        assert_eq!(images.watched.len(), MAX_WATCHED_DIRS);
        // New directories past the cap are left out; the first ones stay watched.
        let before = images.watched.clone();
        {
            let mut shared = images.shared.lock().unwrap();
            for i in 0..20 {
                let sub = dir.path().join(format!("late{i}"));
                std::fs::create_dir(&sub).unwrap();
                shared
                    .probes
                    .insert(Source::Local(sub.join("a.png")), Ok((1, 1)));
            }
        }
        images.watch_probed();
        assert_eq!(images.watched, before);
        assert_eq!(images.seen.len(), n + n.div_ceil(10) + 20);
    }
}
