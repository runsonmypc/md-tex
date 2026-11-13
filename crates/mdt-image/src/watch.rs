//! Watches referenced local image files. Each file's directory is watched, which also
//! catches tools that write a new file and rename it over the old one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::{RecursiveMode, Watcher as _};

pub struct Watcher {
    inner: notify::RecommendedWatcher,
    /// Watched file (as given) by its canonical path, which is what events carry.
    files: Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    dirs: HashSet<PathBuf>,
}

impl Watcher {
    /// `on_change` runs on the watcher's thread with the changed path, as given to `watch`.
    pub fn new(on_change: impl Fn(PathBuf) + Send + 'static) -> notify::Result<Self> {
        let files: Arc<Mutex<HashMap<PathBuf, PathBuf>>> = Default::default();
        let map = files.clone();
        let inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if event.kind.is_access() {
                return;
            }
            let hits: Vec<PathBuf> = {
                let map = map.lock().unwrap();
                event
                    .paths
                    .iter()
                    .filter_map(|p| map.get(p).cloned())
                    .collect()
            };
            for p in hits {
                on_change(p);
            }
        })?;
        Ok(Self {
            inner,
            files,
            dirs: HashSet::new(),
        })
    }

    /// Starts watching `path` (once). Paths in directories that do not exist are skipped.
    pub fn watch(&mut self, path: &Path) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let Ok(canonical_dir) = std::fs::canonicalize(if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        }) else {
            return;
        };
        self.files
            .lock()
            .unwrap()
            .insert(canonical_dir.join(name), path.to_owned());
        if self.dirs.insert(canonical_dir.clone()) {
            let _ = self
                .inner
                .watch(&canonical_dir, RecursiveMode::NonRecursive);
        }
    }
}
