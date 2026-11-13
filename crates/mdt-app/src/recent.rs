//! Recent files: the 20 most recently opened paths, newest first, kept in the state file
//! (separate from settings).

use std::path::{Path, PathBuf};

pub const MAX: usize = 20;

/// `~/Library/Application Support/md-tex/state.toml` on macOS;
/// `$XDG_STATE_HOME/md-tex/state.toml` (or `~/.local/state/...`) on Linux.
pub fn state_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home?.join("Library/Application Support")
    } else {
        match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
            Some(x) => PathBuf::from(x),
            None => home?.join(".local/state"),
        }
    };
    Some(base.join(crate::settings::APP_DIR).join("state.toml"))
}

/// The list in the state file; a missing or broken file gives an empty list.
pub fn load(state: &Path) -> Vec<PathBuf> {
    let Ok(text) = crate::settings::read_text(state) else {
        return Vec::new();
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    doc.get("recent")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(PathBuf::from))
                .take(MAX)
                .collect()
        })
        .unwrap_or_default()
}

fn store(state: &Path, list: &[PathBuf]) -> std::io::Result<()> {
    let mut doc = crate::settings::read_text(state)
        .ok()
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
        .unwrap_or_default();
    let mut array = toml_edit::Array::new();
    // TOML strings are UTF-8. A lossy copy of a non-UTF-8 path would name another file
    // (opening empty, and saving would create it), so such paths are not recorded.
    for p in list.iter().filter_map(|p| p.to_str()) {
        array.push(p);
    }
    doc["recent"] = toml_edit::value(array);
    if let Some(dir) = state.parent() {
        std::fs::create_dir_all(dir)?;
    }
    mdt_doc::save::save_atomic(state, doc.to_string().as_bytes()).map(|_| ())
}

/// Moves `path` to the front, keeping at most `MAX` entries.
pub fn add(state: &Path, path: &Path) -> std::io::Result<()> {
    let mut list = load(state);
    list.retain(|p| p != path);
    list.insert(0, path.to_owned());
    list.truncate(MAX);
    store(state, &list)
}

pub fn remove(state: &Path, path: &Path) -> std::io::Result<()> {
    let mut list = load(state);
    list.retain(|p| p != path);
    store(state, &list)
}

/// Where the running app keeps its recent files. Absent (nothing recorded) until `init`, so
/// tests and offscreen renders leave the user's state alone.
pub struct RecentFiles(pub PathBuf);

impl gpui::Global for RecentFiles {}

pub fn init(cx: &mut gpui::App) {
    if let Some(p) = state_path() {
        cx.set_global(RecentFiles(p));
    }
}

/// Records an opened file (saved in the background).
pub fn note(cx: &gpui::App, path: &Path) {
    if let Some(RecentFiles(state)) = cx.try_global::<RecentFiles>() {
        let state = state.clone();
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        crate::persist::run(move || {
            let _ = add(&state, &path);
        });
    }
}

/// Drops a file that no longer exists (saved in the background, after earlier notes).
pub fn forget(cx: &gpui::App, path: &Path) {
    if let Some(RecentFiles(state)) = cx.try_global::<RecentFiles>() {
        let (state, path) = (state.clone(), path.to_owned());
        crate::persist::run(move || {
            let _ = remove(&state, &path);
        });
    }
}

/// The recent files, newest first.
pub fn list(cx: &gpui::App) -> Vec<PathBuf> {
    cx.try_global::<RecentFiles>()
        .map(|RecentFiles(state)| load(state))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_deduplicated_and_capped_at_20() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("md-tex/state.toml");
        assert!(load(&state).is_empty(), "no file yet");
        for i in 0..25 {
            add(&state, Path::new(&format!("/notes/{i}.md"))).unwrap();
        }
        add(&state, Path::new("/notes/10.md")).unwrap();
        let list = load(&state);
        assert_eq!(list.len(), MAX);
        assert_eq!(list[0], PathBuf::from("/notes/10.md"));
        assert_eq!(list[1], PathBuf::from("/notes/24.md"));
        assert_eq!(list.iter().filter(|p| p.ends_with("10.md")).count(), 1);
        assert!(
            !list.contains(&PathBuf::from("/notes/0.md")),
            "oldest dropped"
        );
        remove(&state, Path::new("/notes/10.md")).unwrap();
        assert_eq!(load(&state)[0], PathBuf::from("/notes/24.md"));
    }

    #[test]
    fn non_utf8_paths_are_not_recorded() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state.toml");
        add(&state, Path::new("/notes/a.md")).unwrap();
        let odd = Path::new(std::ffi::OsStr::from_bytes(b"/notes/\xff.md"));
        add(&state, odd).unwrap();
        assert_eq!(load(&state), vec![PathBuf::from("/notes/a.md")]);
    }

    #[test]
    fn broken_state_file_gives_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state.toml");
        std::fs::write(&state, "recent = [unclosed").unwrap();
        assert!(load(&state).is_empty());
        add(&state, Path::new("/a.md")).unwrap();
        assert_eq!(load(&state), vec![PathBuf::from("/a.md")]);
    }
}
