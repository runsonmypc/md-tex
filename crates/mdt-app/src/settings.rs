//! The settings file: TOML at the platform path, read per key so one bad value does not
//! discard the rest, written through `toml_edit` so the user's other lines survive, and
//! watched for edits made by other programs.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, value};

use crate::theme::ThemeMode;

pub const APP_DIR: &str = "md-tex";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Reader,
    Source,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemeMode,
    /// Percent, 50–300.
    pub zoom: u16,
    pub remote_images: bool,
    pub soft_wrap: bool,
    pub cursor_blink: bool,
    pub initial_view_mode: ViewMode,
    /// The reader's column is half as wide again (108 characters instead of 72).
    pub wide_reader: bool,
}

pub const ZOOM_RANGE: std::ops::RangeInclusive<u16> = 50..=300;

/// Zoom levels that Zoom In and Zoom Out step through, in percent.
pub const ZOOM_STEPS: [u16; 13] = [50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// The next zoom level up (`up`) or down from `current`, which need not be a step.
pub fn zoom_step(current: u16, up: bool) -> u16 {
    if up {
        ZOOM_STEPS.iter().copied().find(|&z| z > current)
    } else {
        ZOOM_STEPS.iter().rev().copied().find(|&z| z < current)
    }
    .unwrap_or(current)
}

/// The settings file's keys, in documentation order.
pub const KEYS: [&str; 7] = [
    "theme",
    "zoom",
    "remote_images",
    "soft_wrap",
    "cursor_blink",
    "initial_view_mode",
    "wide_reader",
];

/// `~/Library/Application Support/md-tex/settings.toml` on macOS;
/// `$XDG_CONFIG_HOME/md-tex/settings.toml` (or `~/.config/...`) on Linux.
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home?.join("Library/Application Support")
    } else {
        match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(x) => PathBuf::from(x),
            None => home?.join(".config"),
        }
    };
    Some(base.join(APP_DIR).join("settings.toml"))
}

/// Reads the file's text. Keys absent from the file take `defaults`. Each key is checked
/// on its own: an invalid value keeps the key's `previous` value and adds a problem naming
/// the key. A file that is not TOML keeps `previous` entirely and reports the error's line.
pub fn apply(text: &str, previous: Settings, defaults: Settings) -> (Settings, Vec<String>) {
    let doc: DocumentMut = match text.parse() {
        Ok(d) => d,
        Err(e) => {
            let line = e
                .span()
                .map(|s| text[..s.start.min(text.len())].lines().count().max(1));
            let msg = match line {
                Some(l) => format!(
                    "Settings file has a syntax error on line {l}; keeping the previous settings."
                ),
                None => {
                    "Settings file has a syntax error; keeping the previous settings.".to_owned()
                }
            };
            return (previous, vec![msg]);
        }
    };
    let mut s = defaults;
    let mut problems = Vec::new();
    for (key, item) in doc.iter() {
        if let Err(why) = set_key(&mut s, key, item.as_value()) {
            problems.push(format!("Setting `{key}` {why}; it was ignored."));
            // Keep the previous value, by setting it again from its file form.
            if let Some(old) = file_value(&previous, key) {
                let _ = set_key(&mut s, key, Some(&old));
            }
        }
    }
    (s, problems)
}

/// Sets one key from its value in the file, or says why the value is not valid.
fn set_key(s: &mut Settings, key: &str, v: Option<&toml_edit::Value>) -> Result<(), &'static str> {
    match key {
        "theme" => match v.and_then(|v| v.as_str()) {
            Some("light") => set(&mut s.theme, ThemeMode::Light),
            Some("dark") => set(&mut s.theme, ThemeMode::Dark),
            Some("system") => set(&mut s.theme, ThemeMode::System),
            _ => Err("must be \"light\", \"dark\" or \"system\""),
        },
        "zoom" => match v.and_then(|v| v.as_integer()) {
            Some(z) if (*ZOOM_RANGE.start() as i64..=*ZOOM_RANGE.end() as i64).contains(&z) => {
                set(&mut s.zoom, z as u16)
            }
            _ => Err("must be a whole number from 50 to 300"),
        },
        "remote_images" => match v.and_then(|v| v.as_str()) {
            Some("allow") => set(&mut s.remote_images, true),
            Some("block") => set(&mut s.remote_images, false),
            _ => Err("must be \"allow\" or \"block\""),
        },
        "soft_wrap" => v
            .and_then(|v| v.as_bool())
            .map_or(Err("must be true or false"), |b| set(&mut s.soft_wrap, b)),
        "cursor_blink" => v
            .and_then(|v| v.as_bool())
            .map_or(Err("must be true or false"), |b| {
                set(&mut s.cursor_blink, b)
            }),
        "initial_view_mode" => match v.and_then(|v| v.as_str()) {
            Some("reader") => set(&mut s.initial_view_mode, ViewMode::Reader),
            Some("source") => set(&mut s.initial_view_mode, ViewMode::Source),
            Some("split") => set(&mut s.initial_view_mode, ViewMode::Split),
            _ => Err("must be \"reader\", \"source\" or \"split\""),
        },
        "wide_reader" => v
            .and_then(|v| v.as_bool())
            .map_or(Err("must be true or false"), |b| set(&mut s.wide_reader, b)),
        _ => Err("is not a setting"),
    }
}

fn set<T>(slot: &mut T, v: T) -> Result<(), &'static str> {
    *slot = v;
    Ok(())
}

/// The file representation of one setting.
pub fn file_value(settings: &Settings, key: &str) -> Option<toml_edit::Value> {
    Some(match key {
        "theme" => match settings.theme {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
            ThemeMode::System => "system",
        }
        .into(),
        "zoom" => (settings.zoom as i64).into(),
        "remote_images" => (if settings.remote_images {
            "allow"
        } else {
            "block"
        })
        .into(),
        "soft_wrap" => settings.soft_wrap.into(),
        "cursor_blink" => settings.cursor_blink.into(),
        "initial_view_mode" => match settings.initial_view_mode {
            ViewMode::Reader => "reader",
            ViewMode::Source => "source",
            ViewMode::Split => "split",
        }
        .into(),
        "wide_reader" => settings.wide_reader.into(),
        _ => return None,
    })
}

/// Writes one key into the file, creating the file (and its directory) if needed and
/// keeping every other line, comment and format as it was.
pub fn write_key(path: &Path, key: &str, new: toml_edit::Value) -> anyhow::Result<()> {
    let text = match read_text(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: DocumentMut = text.parse()?;
    match doc.get_mut(key) {
        Some(Item::Value(old)) => {
            // Keep the comment and spacing around the old value.
            let decor = old.decor().clone();
            *old = new;
            *old.decor_mut() = decor;
        }
        _ => doc[key] = value(new),
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    mdt_doc::save::save_atomic(path, doc.to_string().as_bytes())?;
    Ok(())
}

/// Reads a settings or state file as documents are read: only a regular file is opened,
/// since a FIFO in its place blocked the UI thread for good (at launch, and on every
/// reload) and a link to `/dev/zero` filled memory.
pub fn read_text(path: &Path) -> std::io::Result<String> {
    String::from_utf8(mdt_doc::document::read_file(path)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Reads the settings file. A missing file means built-in defaults.
pub fn load(path: &Path, previous: Settings, defaults: Settings) -> (Settings, Vec<String>) {
    match read_text(path) {
        Ok(text) => apply(&text, previous, defaults),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (defaults, Vec::new()),
        Err(e) => (
            previous,
            vec![format!("Cannot read the settings file: {e}")],
        ),
    }
}

/// Built-in defaults. The pending marker anywhere in this block fails
/// `release_check_no_pending_defaults`.
pub const DEFAULTS: Settings = Settings {
    theme: ThemeMode::System,
    zoom: 100,
    remote_images: false,
    soft_wrap: true,
    cursor_blink: true,
    initial_view_mode: ViewMode::Reader,
    wide_reader: false,
};

/// Lines of the built-in defaults that still carry the pending marker, anywhere in the
/// `DEFAULTS` block (a key's line, or a comment of its own).
pub fn pending_defaults() -> Vec<String> {
    pending_in(include_str!("settings.rs"))
}

fn pending_in(source: &str) -> Vec<String> {
    const MARKER: &str = concat!("OWNER_DEFAULT", "_PENDING");
    let start = source.find("pub const DEFAULTS").unwrap();
    let block = &source[start..start + source[start..].find("};").unwrap()];
    block
        .lines()
        .filter(|l| l.contains(MARKER))
        .map(|l| l.trim().to_owned())
        .collect()
}

/// Watches the settings file's directory (which catches editors that save by renaming a
/// new file over the old one) and calls `changed` (on the watcher's thread) when the file
/// may have changed. Creates the directory if it is missing, so the watch never covers
/// more than that one directory.
pub fn watch(
    path: &Path,
    changed: impl Fn() + Send + 'static,
) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let name = path.file_name().map(|n| n.to_owned());
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        // Reading the file is an access event on Linux (inotify IN_OPEN); reacting to it
        // would make every reload trigger the next one.
        if let Ok(event) = event
            && !event.kind.is_access()
            && event
                .paths
                .iter()
                .any(|p| p.file_name() == name.as_deref() || p.is_dir())
        {
            changed();
        }
    })?;
    // A recursive watch on an ancestor such as ~/.config would add an inotify watch for
    // every directory under it on Linux (over a second at startup on a lived-in home).
    // If the directory can't be created, the nearest existing ancestor still sees it appear.
    let mut dir = path.parent().unwrap_or(Path::new("."));
    let _ = std::fs::create_dir_all(dir);
    while !dir.exists() {
        dir = dir.parent().unwrap_or(Path::new("/"));
    }
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    /// A FIFO at the settings path blocked the UI thread in `load` (at launch and on every
    /// reload), and a link to `/dev/zero` read until memory ran out.
    #[cfg(unix)]
    #[test]
    fn special_files_at_the_settings_path_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("settings.toml");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let zero = dir.path().join("zero.toml");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        for path in [fifo, zero] {
            let (tx, rx) = std::sync::mpsc::channel();
            let p = path.clone();
            std::thread::spawn(move || {
                let _ = tx.send(super::load(&p, super::DEFAULTS, super::DEFAULTS));
            });
            let (s, problems) = rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("{path:?} blocked"));
            assert_eq!(s, super::DEFAULTS);
            assert!(
                problems
                    .iter()
                    .any(|p| p.contains("Cannot read the settings file")),
                "{problems:?}"
            );
            let saved = super::write_key(&path, "zoom", 120.into());
            assert!(saved.is_err(), "{path:?}");
        }
    }

    #[test]
    fn pending_marker_is_found_anywhere_in_the_defaults_block() {
        let m = concat!("OWNER_DEFAULT", "_PENDING");
        let clean = "pub const DEFAULTS: Settings = Settings {\n    zoom: 100,\n};\n";
        assert!(super::pending_in(clean).is_empty());
        let same_line = clean.replace("100,", &format!("100, // {m}"));
        assert_eq!(
            super::pending_in(&same_line),
            [format!("zoom: 100, // {m}")]
        );
        let own_line = clean.replace("    zoom", &format!("    // {m}: decide\n    zoom"));
        assert_eq!(super::pending_in(&own_line), [format!("// {m}: decide")]);
        let after = format!("{clean}// {m}\n");
        assert!(super::pending_in(&after).is_empty(), "outside the block");
    }
}
