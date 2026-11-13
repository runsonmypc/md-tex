//! Image probing, decoding, orientation, downscaling and remote fetching, plus the safe SVG
//! rasterizer that diagrams share.

mod cache;
#[cfg(feature = "remote")]
pub mod remote;
pub mod svg;
mod watch;

use std::any::Any;
use std::io::{Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, ImageDecoder, ImageReader};

pub use cache::RasterCache;
pub use watch::Watcher;

/// Where an image reference points.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    Local(PathBuf),
    /// An `http` or `https` URL.
    Remote(String),
    /// Any other scheme (`data:`, `ftp:` …); never loaded.
    Unsupported(String),
}

/// Resolves `src` against the document's directory: a relative path, an absolute path or
/// a `file://` URL. `doc_dir` is `None` for an unsaved document.
pub fn resolve(src: &str, doc_dir: Option<&Path>) -> Source {
    let src = src.trim();
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Source::Remote(src.to_owned());
    }
    if lower.starts_with("file://") {
        // file:///abs/path, or file://localhost/abs/path.
        let rest = &src["file://".len()..];
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        return Source::Local(PathBuf::from(percent_decode(rest)));
    }
    // A scheme of two or more letters (one letter would be a Windows drive).
    if let Some(colon) = src.find(':')
        && colon > 1
        && src[..colon]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return Source::Unsupported(src.to_owned());
    }
    let path = PathBuf::from(percent_decode(src));
    if path.is_absolute() {
        return Source::Local(path);
    }
    match doc_dir {
        Some(dir) => Source::Local(dir.join(path)),
        None => Source::Local(path),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    NotFound,
    /// The file exists but is not a supported image, or its data is broken.
    Decode(String),
    /// Over a size limit: a file or remote response's bytes, or an image's pixel
    /// dimensions.
    TooLarge,
    /// A remote fetch failed: network error, bad status or a refused redirect.
    Fetch(String),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::NotFound => f.write_str("image not found"),
            ImageError::Decode(e) => write!(f, "could not decode the image: {e}"),
            ImageError::TooLarge => f.write_str("image too large"),
            ImageError::Fetch(e) => write!(f, "could not fetch the image: {e}"),
        }
    }
}

/// Decoded pixels: RGBA, 8 bits per channel, straight alpha.
#[derive(Clone, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Raster({}x{})", self.width, self.height)
    }
}

impl Raster {
    pub fn bytes(&self) -> usize {
        self.rgba.len()
    }
}

/// Bytes read to probe a header; EXIF blocks sit near the start of a file.
const PROBE_BYTES: u64 = 256 * 1024;

/// Largest local image file read, in bytes (a 12-MP JPEG is about 5 MB).
pub const MAX_FILE_BYTES: u64 = 64 << 20;

/// Largest image side and pixel count decoded. A 1.6 MB PNG can declare 20000×20000
/// pixels, 1.6 GB decoded. 64 MP of RGBA is 256 MiB; the benchmark photos are 12 MP.
pub const MAX_SIDE: u32 = 32_768;
pub const MAX_PIXELS: u64 = 64 << 20;

/// Most memory a decoder may allocate.
const MAX_DECODE_BYTES: u64 = 256 << 20;

/// Decoded bytes in flight across all threads. Each reference to an image, and each size
/// it is drawn at, decodes on its own background thread, and a decode peaks at about
/// twice its pixel bytes: ten 64-MP images (33 KB of WebP each) decoding at once peaked at
/// 5.4 GB. Two of the largest fit at once, and a dozen 12-MP photos.
const DECODE_BUDGET: u64 = 2 * MAX_DECODE_BYTES;

static DECODING: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
static DECODED: std::sync::Condvar = std::sync::Condvar::new();

/// A share of `DECODE_BUDGET`, given back when dropped (also when the decoder panics).
struct Permit(u64);

/// Waits until `bytes` (at most the whole budget, so any image can decode alone) fit in
/// the budget, and takes them.
fn permit(bytes: u64) -> Permit {
    let cost = bytes.min(DECODE_BUDGET);
    let mut used = DECODING.lock().unwrap_or_else(|e| e.into_inner());
    while *used + cost > DECODE_BUDGET {
        used = DECODED.wait(used).unwrap_or_else(|e| e.into_inner());
    }
    *used += cost;
    Permit(cost)
}

impl Drop for Permit {
    fn drop(&mut self) {
        *DECODING.lock().unwrap_or_else(|e| e.into_inner()) -= self.0;
        DECODED.notify_all();
    }
}

/// Runs `f`, turning a panic into an error made by `error`: decoding runs on background
/// threads, where a panic would abort the app.
pub(crate) fn guarded_with<T, E>(
    error: impl FnOnce(String) -> E,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|panic| {
        Err(error(format!(
            "the decoder failed: {}",
            panic_message(panic.as_ref())
        )))
    })
}

fn guarded<T>(f: impl FnOnce() -> Result<T, ImageError>) -> Result<T, ImageError> {
    guarded_with(ImageError::Decode, f)
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown fault".into())
}

/// Opens a regular file and gives its size. Opening anything else has effects a document
/// must not cause: a FIFO blocks the opener (or releases a writer waiting for a reader),
/// and a device's driver runs (a serial port raises DTR, which resets many boards;
/// `/dev/zero` never ends). So the path is checked before it is opened, and since it
/// could be swapped in between, it is opened without blocking and checked again.
fn open_regular(path: &Path) -> Result<(std::fs::File, u64), ImageError> {
    let not_regular = || ImageError::Decode("not a regular file".into());
    if !std::fs::metadata(path).map_err(io_error)?.is_file() {
        return Err(not_regular());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOCTTY,
    );
    let file = options.open(path).map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() {
        return Err(not_regular());
    }
    Ok((file, meta.len()))
}

/// Reads up to `cap` bytes of a regular file.
fn read_file(path: &Path, cap: u64) -> Result<Vec<u8>, ImageError> {
    let (file, len) = open_regular(path)?;
    if len > cap {
        return Err(ImageError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    // The file may have grown since.
    if bytes.len() as u64 > cap {
        return Err(ImageError::TooLarge);
    }
    Ok(bytes)
}

/// Refuses dimensions over `MAX_SIDE` or `MAX_PIXELS` before any pixels are decoded.
fn check_dimensions(w: u32, h: u32) -> Result<(), ImageError> {
    if w > MAX_SIDE || h > MAX_SIDE || w as u64 * h as u64 > MAX_PIXELS {
        return Err(ImageError::TooLarge);
    }
    Ok(())
}

pub fn is_svg(path: &Path, bytes: &[u8]) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
        || bytes.trim_ascii_start().starts_with(b"<svg")
        || (bytes.trim_ascii_start().starts_with(b"<?xml")
            && bytes.windows(4).take(4096).any(|w| w == b"<svg"))
}

/// Display dimensions (after EXIF orientation) from the start of a file, without decoding
/// it. SVG gives its own size.
pub fn probe(path: &Path) -> Result<(u32, u32), ImageError> {
    guarded(|| {
        let mut head = Vec::new();
        open_regular(path)?
            .0
            .take(PROBE_BYTES)
            .read_to_end(&mut head)
            .map_err(io_error)?;
        if is_svg(path, &head) {
            let bytes = read_file(path, svg::MAX_BYTES as u64)?;
            return svg::size(&bytes).map_err(ImageError::Decode);
        }
        probe_bytes(&head)
    })
}

/// Like `probe`, for bytes already in memory (a remote image, or a file's start). Images
/// over the size limits are `TooLarge`.
pub fn probe_bytes(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    guarded(|| {
        let mut decoder = reader(bytes)?.into_decoder().map_err(decode_error)?;
        let (w, h) = decoder.dimensions();
        check_dimensions(w, h)?;
        let orientation = decoder
            .orientation()
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        Ok(if swaps(orientation) { (h, w) } else { (w, h) })
    })
}

fn swaps(o: image::metadata::Orientation) -> bool {
    use image::metadata::Orientation::*;
    matches!(o, Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH)
}

fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, ImageError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(io_error)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    Ok(reader)
}

fn io_error(e: std::io::Error) -> ImageError {
    if e.kind() == std::io::ErrorKind::NotFound {
        ImageError::NotFound
    } else {
        ImageError::Decode(e.to_string())
    }
}

fn decode_error(e: image::ImageError) -> ImageError {
    match e {
        image::ImageError::Limits(_) => ImageError::TooLarge,
        e => ImageError::Decode(e.to_string()),
    }
}

/// Decodes PNG, JPEG, GIF (first frame) or WebP, applies EXIF orientation, and downscales
/// to fit within `max` (width, height) pixels, keeping the aspect ratio. Never upscales.
pub fn decode(bytes: &[u8], max: (u32, u32)) -> Result<Raster, ImageError> {
    guarded(|| decode_unguarded(bytes, max))
}

fn decode_unguarded(bytes: &[u8], max: (u32, u32)) -> Result<Raster, ImageError> {
    let mut decoder = reader(bytes)?.into_decoder().map_err(decode_error)?;
    let (w, h) = decoder.dimensions();
    check_dimensions(w, h)?;
    let _permit = permit(decoder.total_bytes());
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    img.apply_orientation(orientation);
    let (w, h) = (img.width(), img.height());
    let (tw, th) = fit((w, h), max);
    if (tw, th) == (w, h) {
        let rgba = img.into_rgba8();
        return Ok(Raster {
            width: w,
            height: h,
            rgba: rgba.into_raw(),
        });
    }
    // Opaque RGB (every JPEG) shrinks as three channels and gains alpha afterwards, at the
    // small size; anything else shrinks as RGBA, which the resizer premultiplies.
    let (pixels, kind) = match img {
        DynamicImage::ImageRgb8(rgb) => (rgb.into_raw(), PixelType::U8x3),
        other => (other.into_rgba8().into_raw(), PixelType::U8x4),
    };
    let src =
        Image::from_vec_u8(w, h, pixels, kind).map_err(|e| ImageError::Decode(e.to_string()))?;
    let mut dst = Image::new(tw, th, kind);
    Resizer::new()
        .resize(
            &src,
            &mut dst,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .map_err(|e| ImageError::Decode(e.to_string()))?;
    let small = dst.into_vec();
    let rgba = if kind == PixelType::U8x3 {
        let mut out = Vec::with_capacity(small.len() / 3 * 4);
        for &[r, g, b] in small.as_chunks::<3>().0 {
            out.extend_from_slice(&[r, g, b, 255]);
        }
        out
    } else {
        small
    };
    Ok(Raster {
        width: tw,
        height: th,
        rgba,
    })
}

/// Reads and decodes a local image; SVG goes through the safe rasterizer.
pub fn load(path: &Path, max: (u32, u32), svg_fonts: &svg::Fonts) -> Result<Raster, ImageError> {
    let bytes = read_file(path, MAX_FILE_BYTES)?;
    decode_any(path, &bytes, max, svg_fonts)
}

pub fn decode_any(
    path: &Path,
    bytes: &[u8],
    max: (u32, u32),
    svg_fonts: &svg::Fonts,
) -> Result<Raster, ImageError> {
    if is_svg(path, bytes) {
        let (w, h) = svg::size(bytes).map_err(ImageError::Decode)?;
        let (tw, _) = fit((w, h), max);
        // Too costly at this size (a high zoom): drawn smaller and scaled.
        return svg::rasterize_fitting(bytes, tw as f32 / w.max(1) as f32, svg_fonts)
            .map(|(raster, _)| raster)
            .map_err(ImageError::Decode);
    }
    decode(bytes, max)
}

/// The largest size within `max` with the aspect ratio of `size`, never larger than `size`.
pub fn fit(size: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (w, h) = (size.0.max(1) as f64, size.1.max(1) as f64);
    let k = (max.0 as f64 / w).min(max.1 as f64 / h).min(1.0);
    (
        ((w * k).round() as u32).max(1),
        ((h * k).round() as u32).max(1),
    )
}

/// Display size in logical pixels: one logical pixel per image pixel (`<img>` width and
/// height override it; one of them alone keeps the aspect ratio), scaled by `zoom`, then
/// capped at `column` keeping the aspect ratio.
pub fn display_size(
    intrinsic: (u32, u32),
    attr: (Option<u32>, Option<u32>),
    zoom: f32,
    column: f32,
) -> (f32, f32) {
    let (iw, ih) = (intrinsic.0.max(1) as f32, intrinsic.1.max(1) as f32);
    let (w, h) = match attr {
        (Some(w), Some(h)) => (w as f32, h as f32),
        (Some(w), None) => (w as f32, w as f32 * ih / iw),
        (None, Some(h)) => (h as f32 * iw / ih, h as f32),
        (None, None) => (iw, ih),
    };
    let (w, h) = (w * zoom, h * zoom);
    if w > column && column > 0.0 {
        (column, h * column / w)
    } else {
        (w, h)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod budget_tests {
    use super::*;

    /// Every image reference decoded at once: a document showing one 64-MP image at ten
    /// sizes used over 5 GB. Decodes wait while the budget is spent.
    #[test]
    fn decodes_wait_while_the_budget_is_spent() {
        let mut png = Vec::new();
        image::RgbaImage::new(4, 4)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let spent = permit(u64::MAX);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(decode(&png, (4, 4)).map(|r| r.width));
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "decoded past the budget"
        );
        drop(spent);
        let decoded = rx.recv_timeout(std::time::Duration::from_secs(10));
        assert_eq!(decoded.unwrap(), Ok(4));
    }
}

#[cfg(all(test, unix))]
mod open_tests {
    use super::*;

    /// `![](/dev/cu.usbmodem1)` opened the device (its driver raises DTR, which resets many
    /// boards) and `![](pipe)` a FIFO (releasing a writer waiting for a reader) as soon as
    /// the document was shown, although the file was then refused.
    #[test]
    fn special_files_are_never_opened() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.png");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let writer_path = c_path.clone();
        std::thread::spawn(move || {
            // SAFETY: a valid C string; the descriptor is closed below.
            let _ = tx.send(unsafe { libc::open(writer_path.as_ptr(), libc::O_WRONLY) });
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        let fonts = svg::Fonts::empty();
        assert!(matches!(probe(&fifo), Err(ImageError::Decode(_))));
        assert!(matches!(
            load(&fifo, (10, 10), &fonts),
            Err(ImageError::Decode(_))
        ));
        let released = rx
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_ok();
        if !released {
            // Release the writer, so the test ends.
            // SAFETY: a valid C string and descriptors this test owns.
            unsafe {
                let reader = libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK);
                libc::close(rx.recv().unwrap());
                libc::close(reader);
            }
        }
        assert!(!released, "the FIFO was opened");
    }

    /// The path was checked, then opened: swapping it for a FIFO in between blocked the
    /// caller in `open`. Probes a path that flips between an image and a FIFO.
    #[test]
    fn a_path_swapped_for_a_fifo_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let image = dir.path().join("a.png");
        std::fs::write(&image, b"not really a png").unwrap();
        let link = dir.path().join("link.png");
        std::os::unix::fs::symlink(&image, &link).unwrap();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let swapper = {
            let (dir, done) = (dir.path().to_path_buf(), done.clone());
            let (link, fifo, image) = (link.clone(), fifo.clone(), image.clone());
            std::thread::spawn(move || {
                let tmp = dir.join("tmp");
                for target in [&fifo, &image].into_iter().cycle() {
                    if done.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    std::os::unix::fs::symlink(target, &tmp).unwrap();
                    std::fs::rename(&tmp, &link).unwrap();
                }
            })
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            while start.elapsed().as_secs_f32() < 2.0 {
                let _ = probe(&link);
            }
            let _ = tx.send(());
        });
        let finished = rx.recv_timeout(std::time::Duration::from_secs(10));
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        swapper.join().unwrap();
        assert!(finished.is_ok(), "probe blocked opening the FIFO");
    }
}
