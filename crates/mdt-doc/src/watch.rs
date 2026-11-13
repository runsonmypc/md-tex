//! Watches the open file for changes by other programs. The parent directory
//! is watched, which also catches editors that save by renaming a new file over the old
//! one, and events are debounced by `DEBOUNCE`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

pub const DEBOUNCE: Duration = Duration::from_millis(50);

pub struct FileWatcher {
    _inner: notify::RecommendedWatcher,
    path: PathBuf,
    on_change: Callback,
}

type Callback = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

impl FileWatcher {
    /// Calls `on_change` (on a watcher thread) once events for `path` have been quiet for
    /// `DEBOUNCE`. The caller reads the file to see what happened.
    pub fn new(path: &Path, on_change: impl Fn() + Send + 'static) -> notify::Result<Self> {
        // Events name the real path: resolve a symlinked document to its target, and a
        // wrong-case path (case-insensitive macOS) to the case on disk. A file that does
        // not exist yet is watched in its canonical folder under the name given.
        let target = match std::fs::canonicalize(path) {
            Ok(real) => real,
            Err(_) => {
                let dir = match path.parent() {
                    Some(d) if !d.as_os_str().is_empty() => d.to_owned(),
                    _ => PathBuf::from("."),
                };
                std::fs::canonicalize(&dir)?.join(path.file_name().unwrap_or_default())
            }
        };
        let dir = target
            .parent()
            .map_or_else(|| PathBuf::from("/"), Path::to_owned);
        let (tx, rx) = mpsc::channel::<()>();
        let mut inner =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                if event.kind.is_access() {
                    return;
                }
                if event.paths.contains(&target) {
                    let _ = tx.send(());
                }
            })?;
        inner.watch(&dir, RecursiveMode::NonRecursive)?;
        let on_change: Callback = Arc::new(Mutex::new(Some(Box::new(on_change))));
        let thread_change = on_change.clone();
        std::thread::spawn(move || {
            // Ends when the watcher (and with it the sender) is dropped.
            while rx.recv().is_ok() {
                while rx.recv_timeout(DEBOUNCE).is_ok() {}
                if let Some(f) = thread_change.lock().unwrap().as_ref() {
                    f();
                }
            }
        });
        Ok(Self {
            _inner: inner,
            path: path.to_owned(),
            on_change,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileWatcher {
    /// Drops the callback here rather than on the watcher thread, so whatever it holds (a
    /// channel sender that wakes a UI task) is released on the owner's thread. GPUI's test
    /// scheduler deadlocks when a foreign thread wakes a task during app shutdown.
    fn drop(&mut self) {
        let f = self.on_change.lock().unwrap().take();
        drop(f);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::{DiskChange, Document};

    fn watch(path: &Path) -> (FileWatcher, mpsc::Receiver<Instant>) {
        let (tx, rx) = mpsc::channel();
        let w = FileWatcher::new(path, move || {
            let _ = tx.send(Instant::now());
        })
        .unwrap();
        // Let FSEvents start, and drop anything from the file's creation.
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        (w, rx)
    }

    fn changed(rx: &mpsc::Receiver<Instant>) -> bool {
        rx.recv_timeout(Duration::from_millis(500)).is_ok()
    }

    #[test]
    fn write_rename_over_and_delete_are_seen_and_own_saves_classify_as_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "one\n").unwrap();
        let mut doc = Document::open(&path).unwrap();
        let (_w, rx) = watch(&path);

        // Another program writes.
        std::fs::write(&path, "two\n").unwrap();
        assert!(changed(&rx), "write seen");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(doc.classify_disk_content(&bytes), DiskChange::External);

        // Another editor saves by renaming a temporary file over it; watching continues.
        let tmp = dir.path().join(".doc.md.tmp");
        std::fs::write(&tmp, "three\n").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        assert!(changed(&rx), "rename-over seen");
        std::fs::write(&path, "four\n").unwrap();
        assert!(changed(&rx), "still watching after the replace");

        // Our own save is seen, but its content is what we wrote.
        doc = Document::open(&path).unwrap();
        while rx.try_recv().is_ok() {}
        doc.save().unwrap();
        assert!(changed(&rx));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(doc.classify_disk_content(&bytes), DiskChange::OwnWrite);

        std::fs::remove_file(&path).unwrap();
        assert!(changed(&rx), "deletion seen");
        assert!(!path.exists());
    }

    #[test]
    fn bursts_are_debounced_into_one_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "x").unwrap();
        let (_w, rx) = watch(&path);
        for i in 0..10 {
            std::fs::write(&path, format!("{i}")).unwrap();
        }
        assert!(changed(&rx));
        std::thread::sleep(Duration::from_millis(200));
        assert!(rx.try_recv().is_err(), "one call for the burst");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_document_sees_changes_to_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        let real = real_dir.join("doc.md");
        let link = dir.path().join("link.md");
        std::fs::write(&real, "x").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (_w, rx) = watch(&link);
        std::fs::write(&real, "y").unwrap();
        assert!(changed(&rx), "a write to the target is seen");
    }

    #[test]
    fn other_files_in_the_folder_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "x").unwrap();
        let (_w, rx) = watch(&path);
        std::fs::write(dir.path().join("other.md"), "y").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
