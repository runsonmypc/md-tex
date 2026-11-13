//! Turning command-line paths into documents and windows.

use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext, Bounds, Focusable, Global, Pixels, Size, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use mdt_doc::{Document, OpenError};

use crate::view::DocumentView;

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Cli {
    /// Open these files (none: one untitled document).
    Open(Vec<PathBuf>),
    Version,
    Help,
    /// An option md-tex does not have.
    Unknown(String),
}

pub const USAGE: &str = "\
Usage: md-tex [OPTION]... [FILE]...
Opens each Markdown FILE in its own window, or an untitled document without any. A FILE
that does not exist yet opens empty; saving creates it. With md-tex already running, the
files open there.

  -h, --help     show this help and exit
  -V, --version  show the version and exit
  --             treat every argument after it as a FILE
";

/// `md-tex 1.0.0`.
pub fn version_line() -> String {
    format!("md-tex {}", env!("CARGO_PKG_VERSION"))
}

/// Reads the arguments after the program name. Options come before `--`; a file whose
/// name starts with `-` goes after it.
pub fn parse_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Cli {
    let mut paths = Vec::new();
    let mut options = true;
    for arg in args {
        if options {
            match arg.to_str() {
                Some("--") => {
                    options = false;
                    continue;
                }
                Some("-V" | "--version") => return Cli::Version,
                Some("-h" | "--help") => return Cli::Help,
                // Old macOS versions pass a process serial number to apps that Finder
                // launches.
                Some(a) if a.starts_with("-psn_") => continue,
                Some(a) if a.starts_with('-') && a != "-" => return Cli::Unknown(a.to_owned()),
                _ => {}
            }
        }
        paths.push(PathBuf::from(arg));
    }
    Cli::Open(paths)
}

/// One document per path. No paths gives one untitled document; a path that does not
/// exist gives an empty document bound to it (saving creates the file). Paths that cannot
/// be opened come back as errors, so the others still open.
pub fn documents_for_args(paths: &[PathBuf]) -> Vec<Result<Document, OpenError>> {
    if paths.is_empty() {
        return vec![Ok(Document::untitled())];
    }
    paths.iter().map(|p| Document::open(p)).collect()
}

pub fn open_window(doc: Document, cx: &mut App) -> anyhow::Result<WindowHandle<DocumentView>> {
    if let Some(p) = doc.path() {
        crate::recent::note(cx, p);
    }
    let bounds = next_bounds(cx);
    let title = DocumentView::title_for(&doc);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // Match md-tex.desktop so Linux shells find the icon and group our windows.
            app_id: Some("md-tex".into()),
            #[cfg(target_os = "linux")]
            icon: Some(window_icon()),
            // The native title bar shows the file name.
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| DocumentView::new(doc, window, cx));
            let focus = view.focus_handle(cx);
            window.focus(&focus, cx);
            view
        },
    )
}

/// X11 also needs a window icon when launched without an installed desktop entry.
#[cfg(any(target_os = "linux", test))]
fn window_icon() -> std::sync::Arc<image::RgbaImage> {
    use std::sync::{Arc, OnceLock};

    static ICON: OnceLock<Arc<image::RgbaImage>> = OnceLock::new();
    ICON.get_or_init(|| {
        let raster = mdt_image::svg::rasterize(
            include_bytes!("../../../assets/linux/md-tex.svg"),
            0.25,
            &mdt_image::svg::Fonts::empty(),
        )
        .expect("bundled app icon must be valid SVG");
        Arc::new(
            image::RgbaImage::from_raw(raster.width, raster.height, raster.rgba)
                .expect("app icon must have RGBA pixels"),
        )
    })
    .clone()
}

/// The size new windows ask for, before fitting the screen.
const WINDOW_SIZE: Size<Pixels> = size(px(820.0), px(1000.0));
/// How far each new window sits down and right of the previous one.
const CASCADE: f32 = 28.0;

/// Where the last window was placed, so the next one cascades from it.
struct LastPlaced(Bounds<Pixels>);

impl Global for LastPlaced {}

/// The next window's bounds: `WINDOW_SIZE` shrunk to the visible area of the display it
/// goes on, centred for the first window and cascaded from the last one after that.
fn next_bounds(cx: &mut App) -> Bounds<Pixels> {
    let last = (!cx.windows().is_empty())
        .then(|| cx.try_global::<LastPlaced>().map(|l| l.0))
        .flatten();
    let display = last
        .and_then(|l| {
            cx.displays()
                .into_iter()
                .find(|d| d.bounds().contains(&l.origin))
        })
        .or_else(|| cx.primary_display());
    let bounds = match display {
        Some(d) => place(d.visible_bounds(), last, WINDOW_SIZE),
        None => Bounds::new(point(px(0.0), px(0.0)), WINDOW_SIZE),
    };
    cx.set_global(LastPlaced(bounds));
    bounds
}

/// `want` fitted into `visible`: centred, or `CASCADE` down and right of `last`. Each
/// direction starts again at the visible area's edge when the step would leave it (so a
/// window as tall as the screen still steps right).
pub fn place(
    visible: Bounds<Pixels>,
    last: Option<Bounds<Pixels>>,
    want: Size<Pixels>,
) -> Bounds<Pixels> {
    let size = want.min(&visible.size);
    let Some(last) = last else {
        return Bounds::centered_at(visible.center(), size);
    };
    let step = |from: Pixels, len: Pixels, start: Pixels, end: Pixels| {
        let to = from + px(CASCADE);
        if to >= start && to + len <= end {
            to
        } else {
            start
        }
    };
    let origin = point(
        step(last.origin.x, size.width, visible.left(), visible.right()),
        step(last.origin.y, size.height, visible.top(), visible.bottom()),
    );
    Bounds::new(origin, size)
}

/// Whether two paths name the same file (also before it exists).
pub fn same_file(a: &Path, b: &Path) -> bool {
    file_key(a) == file_key(b)
}

fn file_key(p: &Path) -> PathBuf {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_owned()))
}

/// The window already showing `path`, if any. A window that is being updated cannot be
/// read, so callers inside a window check its own file first (`DocumentView::open_here`).
pub fn window_for(path: &Path, cx: &App) -> Option<WindowHandle<DocumentView>> {
    let want = file_key(path);
    cx.windows()
        .into_iter()
        .filter_map(|w| w.downcast::<DocumentView>())
        .find(|w| {
            w.read(cx)
                .ok()
                .and_then(|v| v.doc.path().map(file_key))
                .is_some_and(|p| p == want)
        })
}

/// Shows errors from opening files as notices in the active window (or another one), or,
/// with no window open (a Finder open whose only file failed), in a new untitled window,
/// so they never go only to stderr.
pub fn report(errors: Vec<String>, cx: &mut App) {
    if errors.is_empty() {
        return;
    }
    for e in &errors {
        eprintln!("md-tex: {e}");
    }
    let target = cx
        .active_window()
        .and_then(|w| w.downcast::<DocumentView>())
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|w| w.downcast::<DocumentView>())
        })
        .or_else(|| open_window(Document::untitled(), cx).ok());
    if let Some(w) = target {
        let _ = w.update(cx, |v, _, cx| {
            for e in errors {
                v.notices.push(e, &[]);
            }
            cx.notify();
        });
    }
}

/// Opens a window for each document, or brings forward the window that already shows it
/// (one file per window); reports the paths that failed.
pub fn open_all(paths: &[PathBuf], cx: &mut App) -> Vec<String> {
    let mut errors = Vec::new();
    let mut fresh = Vec::new();
    // By file, not by spelling: `a.md`, `sub/../a.md` and a link to it are one window.
    let mut seen = std::collections::HashSet::new();
    for p in paths {
        match window_for(p, cx) {
            Some(w) => {
                let _ = w.update(cx, |_, window, _| window.activate_window());
                crate::recent::note(cx, p);
            }
            None if seen.insert(file_key(p)) => fresh.push(p.clone()),
            None => {}
        }
    }
    if !paths.is_empty() && fresh.is_empty() {
        return errors;
    }
    for doc in documents_for_args(&fresh) {
        match doc
            .map_err(|e| e.to_string())
            .and_then(|d| open_window(d, cx).map_err(|e| e.to_string()))
        {
            Ok(_) => {}
            Err(e) => errors.push(e),
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, point, px, size};

    use super::{Cli, parse_args};

    fn args(a: &[&str]) -> Cli {
        parse_args(a.iter().map(std::ffi::OsString::from))
    }

    #[test]
    fn command_line_options_and_files() {
        assert_eq!(args(&[]), Cli::Open(vec![]));
        assert_eq!(args(&["--version"]), Cli::Version);
        assert_eq!(args(&["a.md", "-V"]), Cli::Version);
        assert_eq!(args(&["-h"]), Cli::Help);
        assert_eq!(args(&["--help", "a.md"]), Cli::Help);
        assert_eq!(args(&["-x"]), Cli::Unknown("-x".into()));
        assert_eq!(
            args(&["a.md", "--", "--version", "-h"]),
            Cli::Open(vec!["a.md".into(), "--version".into(), "-h".into()])
        );
        assert_eq!(
            args(&["-", "-psn_0_1234", "b.md"]),
            Cli::Open(vec!["-".into(), "b.md".into()])
        );
        assert_eq!(
            super::version_line(),
            concat!("md-tex ", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn windows_fit_the_screen_and_cascade() {
        // A 1366x768 laptop with a 25 px menu bar.
        let visible = Bounds::new(point(px(0.0), px(25.0)), size(px(1366.0), px(743.0)));
        let first = super::place(visible, None, super::WINDOW_SIZE);
        assert_eq!(
            first.size,
            size(px(820.0), px(743.0)),
            "shorter than the screen"
        );
        assert_eq!(first.origin, point(px(273.0), px(25.0)));
        let second = super::place(visible, Some(first), super::WINDOW_SIZE);
        assert_eq!(
            second.origin,
            point(px(301.0), px(25.0)),
            "no room below: right only"
        );
        // At the right edge it starts again on the left.
        let edge = Bounds::new(point(px(540.0), px(25.0)), first.size);
        let next = super::place(visible, Some(edge), super::WINDOW_SIZE);
        assert_eq!(next.origin, point(px(0.0), px(25.0)));
        // A tall screen cascades down and right.
        let tall = Bounds::new(point(px(0.0), px(0.0)), size(px(2560.0), px(1415.0)));
        let a = super::place(tall, None, super::WINDOW_SIZE);
        assert_eq!(a.size, super::WINDOW_SIZE);
        let b = super::place(tall, Some(a), super::WINDOW_SIZE);
        assert_eq!(b.origin, a.origin + point(px(28.0), px(28.0)));
    }

    #[test]
    fn bundled_window_icon_has_visible_pixels_and_transparency() {
        let icon = super::window_icon();
        assert_eq!(icon.dimensions(), (256, 256));
        assert!(icon.pixels().any(|p| p.0[3] == 0));
        assert!(icon.pixels().any(|p| p.0[3] == 255));
    }
}
