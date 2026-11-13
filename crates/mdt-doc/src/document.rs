//! A document: the source text as a rope plus its file binding, detected styles, undo
//! history and save state.

use std::io::{self, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ropey::Rope;

use crate::decode::{self, DecodeError, ReadOnlyReason};
use crate::history::{Edit, EditKind, History, Selection};
use crate::line_style::{IndentUnit, LineEnding};
use crate::save;

/// Hash of file content, used to tell the app's own writes from external changes.
pub type ContentHash = u128;

pub fn content_hash(bytes: &[u8]) -> ContentHash {
    xxhash_rust::xxh3::xxh3_128(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskChange {
    /// The file holds exactly what was last loaded or saved: our own write, or no change.
    OwnWrite,
    /// Someone else changed the file.
    External,
}

#[derive(Debug)]
pub enum OpenError {
    Binary(PathBuf),
    Io(PathBuf, io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binary(p) => write!(f, "{} appears to be a binary file.", p.display()),
            Self::Io(p, e) => write!(f, "Could not open {}: {e}", p.display()),
        }
    }
}

impl std::error::Error for OpenError {}

/// Larger files are refused (the 10 MB benchmark document is far below this): the text
/// is held several times over, and opening runs on the UI thread.
pub const MAX_FILE_BYTES: u64 = 256 << 20;

/// Reads a regular file of at most `MAX_FILE_BYTES`. A link can name a FIFO (opening it
/// blocks, or releases a writer waiting for a reader) or a device (opening it runs its
/// driver: a serial port raises DTR, which resets many boards; reading `/dev/zero` never
/// ends). So the path is checked before it is opened, and since it could be swapped in
/// between, it is opened without blocking and the opened file is checked again.
pub fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    let not_regular = || io::Error::new(io::ErrorKind::InvalidInput, "not a regular file");
    if !std::fs::metadata(path)?.is_file() {
        return Err(not_regular());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOCTTY,
    );
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(not_regular());
    }
    let too_large = || {
        io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("the file is larger than {} MB", MAX_FILE_BYTES >> 20),
        )
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // The file may grow while it is read.
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

#[derive(Debug)]
pub struct Document {
    rope: Rope,
    bom: bool,
    line_ending: LineEnding,
    indent: IndentUnit,
    read_only: Option<ReadOnlyReason>,
    path: Option<PathBuf>,
    history: History,
    saved_version: u64,
    /// Hash of the bytes last read from or written to disk; `None` when the file does not
    /// exist on disk yet.
    disk_hash: Option<ContentHash>,
}

impl Document {
    /// An empty untitled document.
    pub fn untitled() -> Self {
        Self::from_text("", false, None, None)
    }

    /// Opens `path`. A missing file gives an empty document bound to the path.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        match read_file(path) {
            Ok(bytes) => Self::from_bytes(&bytes, Some(path.to_path_buf())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok(Self::from_text("", false, None, Some(path.to_path_buf())))
            }
            Err(e) => Err(OpenError::Io(path.to_path_buf(), e)),
        }
    }

    pub fn from_bytes(bytes: &[u8], path: Option<PathBuf>) -> Result<Self, OpenError> {
        let decoded = decode::decode(bytes)
            .map_err(|DecodeError::Binary| OpenError::Binary(path.clone().unwrap_or_default()))?;
        let mut doc = Self::from_text(&decoded.text, decoded.bom, decoded.read_only, path);
        doc.disk_hash = Some(content_hash(bytes));
        Ok(doc)
    }

    fn from_text(
        text: &str,
        bom: bool,
        read_only: Option<ReadOnlyReason>,
        path: Option<PathBuf>,
    ) -> Self {
        Self {
            line_ending: LineEnding::detect(text),
            indent: IndentUnit::detect(text),
            rope: Rope::from_str(text),
            bom,
            read_only,
            path,
            history: History::default(),
            saved_version: 0,
            disk_hash: None,
        }
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn indent_unit(&self) -> IndentUnit {
        self.indent
    }

    pub fn read_only(&self) -> Option<ReadOnlyReason> {
        self.read_only
    }

    pub fn is_dirty(&self) -> bool {
        self.history.version() != self.saved_version
    }

    #[cfg(test)]
    fn history(&self) -> &History {
        &self.history
    }

    /// Ends the current undo group (cursor moved, focus changed).
    pub fn seal_undo_group(&mut self) {
        self.history.seal();
    }

    /// Applies `edits` in order (each range refers to the text after the previous edits) as
    /// one change and records it for undo. Returns false for read-only documents.
    pub fn edit(
        &mut self,
        edits: impl IntoIterator<Item = (Range<usize>, String)>,
        kind: EditKind,
        before: Selection,
        after: Selection,
        now: Instant,
    ) -> bool {
        if self.read_only.is_some() {
            return false;
        }
        let applied: Vec<Edit> = edits
            .into_iter()
            .map(|(range, new)| {
                let old = self.rope.byte_slice(range.clone()).to_string();
                splice(&mut self.rope, range.clone(), &new);
                Edit { range, old, new }
            })
            .collect();
        if !applied.is_empty() {
            self.history.record(applied, kind, before, after, now);
        }
        true
    }

    /// Inserts pasted text at `range`, converting its line breaks to the document's style.
    pub fn paste(
        &mut self,
        range: Range<usize>,
        text: &str,
        before: Selection,
        now: Instant,
    ) -> bool {
        let text = self.line_ending.convert(text);
        let end = range.start + text.len();
        self.edit(
            [(range, text)],
            EditKind::Other,
            before,
            Selection::cursor(end),
            now,
        )
    }

    /// Reverts the last change; returns the selection to restore.
    pub fn undo(&mut self) -> Option<Selection> {
        // The transaction is borrowed, not cloned: a large paste would copy its text.
        let t = self.history.undo()?;
        for e in t.edits.iter().rev() {
            splice(
                &mut self.rope,
                e.range.start..e.range.start + e.new.len(),
                &e.old,
            );
        }
        Some(t.before)
    }

    /// Reapplies the last undone change; returns the selection to restore.
    pub fn redo(&mut self) -> Option<Selection> {
        let t = self.history.redo()?;
        for e in &t.edits {
            splice(&mut self.rope, e.range.clone(), &e.new);
        }
        Some(t.after)
    }

    /// The exact bytes a save writes: BOM if the file had one, then the text verbatim.
    pub fn bytes_for_save(&self) -> Vec<u8> {
        decode::encode(self.rope.chunks().map(str::as_bytes), self.bom)
    }

    /// Saves to the bound path. Untitled documents need `save_as`.
    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "untitled document"))?;
        self.save_as(&path)
    }

    /// Saves atomically to `path` and binds the document to it. On failure nothing changes.
    /// A read-only document is refused: its text is a lossy decode of the file.
    pub fn save_as(&mut self, path: &Path) -> io::Result<()> {
        if let Some(reason) = self.read_only {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                reason.to_string(),
            ));
        }
        let bytes = self.bytes_for_save();
        save::save_atomic(path, &bytes)?;
        self.path = Some(path.to_path_buf());
        self.saved_version = self.history.version();
        self.disk_hash = Some(content_hash(&bytes));
        self.history.seal();
        Ok(())
    }

    /// The file is gone from disk: anything that appears there later is someone else's.
    pub fn mark_missing(&mut self) {
        self.disk_hash = None;
    }

    /// Whether the document was last known to exist on disk.
    pub fn on_disk(&self) -> bool {
        self.disk_hash.is_some()
    }

    /// Classifies file content seen on disk against what was last loaded or saved.
    pub fn classify_disk_content(&self, bytes: &[u8]) -> DiskChange {
        if self.disk_hash == Some(content_hash(bytes)) {
            DiskChange::OwnWrite
        } else {
            DiskChange::External
        }
    }
}

/// Replaces a byte range of `rope` with `new`.
fn splice(rope: &mut Rope, range: Range<usize>, new: &str) {
    let start = rope.byte_to_char(range.start);
    let end = rope.byte_to_char(range.end);
    debug_assert_eq!(
        rope.char_to_byte(start),
        range.start,
        "edit must start on a char boundary"
    );
    debug_assert_eq!(
        rope.char_to_byte(end),
        range.end,
        "edit must end on a char boundary"
    );
    rope.remove(start..end);
    rope.insert(start, new);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use proptest::prelude::*;

    use super::*;

    fn text(doc: &Document) -> String {
        doc.rope().to_string()
    }

    /// Types `s` one character at a time at the end of the document.
    fn type_str(doc: &mut Document, s: &str, now: Instant) {
        for ch in s.chars() {
            let at = doc.rope().len_bytes();
            let end = at + ch.len_utf8();
            doc.edit(
                [(at..at, ch.to_string())],
                EditKind::Typing,
                Selection::cursor(at),
                Selection::cursor(end),
                now,
            );
        }
    }

    #[test]
    fn grouped_typing_undoes_word_by_word() {
        let mut doc = Document::untitled();
        type_str(&mut doc, "hello world", Instant::now());
        assert_eq!(doc.undo(), Some(Selection::cursor(6)));
        assert_eq!(text(&doc), "hello ");
        doc.undo();
        assert_eq!(text(&doc), "");
        doc.redo();
        doc.redo();
        assert_eq!(text(&doc), "hello world");
    }

    #[test]
    fn idle_second_splits_typing() {
        let mut doc = Document::untitled();
        let t0 = Instant::now();
        type_str(&mut doc, "ab", t0);
        type_str(&mut doc, "cd", t0 + Duration::from_millis(1500));
        doc.undo();
        assert_eq!(text(&doc), "ab");
    }

    #[test]
    fn backspace_run_is_one_step() {
        let mut doc = Document::untitled();
        let now = Instant::now();
        doc.edit(
            [(0..0, "abcdef".into())],
            EditKind::Other,
            Selection::cursor(0),
            Selection::cursor(6),
            now,
        );
        for end in (3..=6).rev() {
            doc.edit(
                [(end - 1..end, String::new())],
                EditKind::Deleting,
                Selection::cursor(end),
                Selection::cursor(end - 1),
                now,
            );
        }
        assert_eq!(text(&doc), "ab");
        assert_eq!(doc.undo(), Some(Selection::cursor(6)));
        assert_eq!(text(&doc), "abcdef");
    }

    #[test]
    fn undo_after_save_marks_dirty_and_undoing_back_to_save_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, "start").unwrap();
        let mut doc = Document::open(&path).unwrap();
        assert!(!doc.is_dirty());
        type_str(&mut doc, "!", Instant::now());
        assert!(doc.is_dirty());
        doc.save().unwrap();
        assert!(!doc.is_dirty());
        doc.undo();
        assert_eq!(text(&doc), "start");
        assert!(doc.is_dirty());
        doc.redo();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn typing_after_save_does_not_merge_into_the_saved_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        let mut doc = Document::open(&path).unwrap();
        let now = Instant::now();
        type_str(&mut doc, "ab", now);
        doc.save().unwrap();
        type_str(&mut doc, "cd", now);
        doc.undo();
        assert_eq!(text(&doc), "ab");
        assert!(!doc.is_dirty());
    }

    #[test]
    fn paste_into_crlf_document_saves_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, "a\r\nb\r\n").unwrap();
        let mut doc = Document::open(&path).unwrap();
        doc.paste(6..6, "c\nd\n", Selection::cursor(6), Instant::now());
        doc.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"a\r\nb\r\nc\r\nd\r\n");
    }

    #[test]
    fn capacity_holds_ten_thousand_steps() {
        let mut doc = Document::untitled();
        let now = Instant::now();
        for i in 0..10_050 {
            doc.edit(
                [(i..i, "x".into())],
                EditKind::Other,
                Selection::cursor(i),
                Selection::cursor(i + 1),
                now,
            );
        }
        assert_eq!(doc.history().undo_len(), 10_000);
        let mut n = 0;
        while doc.undo().is_some() {
            n += 1;
        }
        assert_eq!(n, 10_000);
        assert_eq!(doc.rope().len_bytes(), 50);
    }

    #[test]
    fn read_only_document_refuses_edits() {
        let mut doc = Document::from_bytes(b"bad \xFF", None).unwrap();
        assert!(!doc.edit(
            [(0..0, "x".into())],
            EditKind::Typing,
            Selection::default(),
            Selection::default(),
            Instant::now()
        ));
        assert_eq!(text(&doc), "bad \u{FFFD}");
    }

    #[test]
    fn read_only_document_is_never_saved() {
        // Saving the lossy decode would replace the invalid byte with U+FFFD on disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, b"caf\xE9").unwrap();
        let mut doc = Document::open(&path).unwrap();
        assert!(doc.read_only().is_some());
        assert!(doc.save().is_err());
        let other = dir.path().join("b.md");
        assert!(doc.save_as(&other).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"caf\xE9");
        assert!(!other.exists());
        assert_eq!(doc.path(), Some(path.as_path()));
    }

    #[test]
    fn typing_after_an_undo_starts_a_new_step() {
        let mut doc = Document::untitled();
        let now = Instant::now();
        type_str(&mut doc, "ab", now);
        doc.edit(
            [(1..2, String::new())],
            EditKind::Deleting,
            Selection::cursor(2),
            Selection::cursor(1),
            now,
        );
        doc.undo();
        assert_eq!(text(&doc), "ab");
        type_str(&mut doc, "c", now);
        doc.undo();
        assert_eq!(text(&doc), "ab", "the c is its own step");
        doc.redo();
        type_str(&mut doc, "d", now);
        doc.undo();
        assert_eq!(text(&doc), "abc", "typing after a redo is its own step too");
    }

    #[test]
    fn missing_path_opens_empty_document_bound_to_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.md");
        let mut doc = Document::open(&path).unwrap();
        assert_eq!(text(&doc), "");
        assert_eq!(doc.path(), Some(path.as_path()));
        type_str(&mut doc, "hi", Instant::now());
        doc.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hi");
    }

    /// Opens `path` on another thread, so a regression fails after a few seconds instead
    /// of hanging the test run.
    #[cfg(unix)]
    fn open_soon(path: &Path) -> Result<Document, OpenError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(Document::open(&path));
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("opening blocked")
    }

    /// A link to a FIFO blocked the UI thread in `open`; one to `/dev/zero` read forever.
    #[cfg(unix)]
    #[test]
    fn only_regular_files_open() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.md");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let zero = dir.path().join("zero.md");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        for path in [&fifo, &zero] {
            let err = open_soon(path).unwrap_err().to_string();
            assert!(err.ends_with("not a regular file"), "{err}");
        }
        // A symlink to a regular file still opens.
        let real = dir.path().join("real.md");
        std::fs::write(&real, "hi").unwrap();
        let link = dir.path().join("link.md");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(text(&open_soon(&link).unwrap()), "hi");
    }

    /// Opening a FIFO released a writer blocked waiting for a reader (and opening a device
    /// runs its driver), although the file was then refused: only regular files are opened.
    #[cfg(unix)]
    #[test]
    fn special_files_are_never_opened() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.md");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let writer_path = c_path.clone();
        std::thread::spawn(move || {
            // SAFETY: a valid C string; the descriptor is closed below.
            let _ = tx.send(unsafe { libc::open(writer_path.as_ptr(), libc::O_WRONLY) });
        });
        std::thread::sleep(Duration::from_millis(200));
        let err = open_soon(&fifo).unwrap_err().to_string();
        assert!(err.ends_with("not a regular file"), "{err}");
        let released = rx.recv_timeout(Duration::from_millis(300)).is_ok();
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

    #[cfg(unix)]
    #[test]
    fn oversized_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.md");
        // Sparse: no disk space is used.
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(super::MAX_FILE_BYTES + 1).unwrap();
        let err = open_soon(&path).unwrap_err().to_string();
        assert!(err.ends_with("larger than 256 MB"), "{err}");
    }

    #[test]
    fn binary_file_is_refused_with_its_name() {
        let err = Document::from_bytes(b"a\0b", Some("pic.png".into())).unwrap_err();
        assert_eq!(err.to_string(), "pic.png appears to be a binary file.");
    }

    #[test]
    fn own_write_versus_external_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, "one").unwrap();
        let mut doc = Document::open(&path).unwrap();
        assert_eq!(doc.classify_disk_content(b"one"), DiskChange::OwnWrite);
        type_str(&mut doc, " two", Instant::now());
        doc.save().unwrap();
        assert_eq!(
            doc.classify_disk_content(&std::fs::read(&path).unwrap()),
            DiskChange::OwnWrite
        );
        assert_eq!(
            doc.classify_disk_content(b"someone else"),
            DiskChange::External
        );
    }

    /// A random edit against a document of length `len`, on char boundaries of ASCII-or-
    /// multibyte text: (start fraction, end fraction, replacement, kind).
    fn edit_strategy() -> impl Strategy<Value = (f64, f64, String, u8)> {
        (0.0..1.0f64, 0.0..1.0f64, "[a-zé日 \n]{0,6}", 0u8..3)
    }

    proptest! {
        #[test]
        fn applying_n_edits_then_undoing_all_restores_original_bytes(
            original in "[a-zé日 \r\n]{0,40}",
            edits in prop::collection::vec(edit_strategy(), 1..40),
        ) {
            let mut doc = Document::from_bytes(original.as_bytes(), None).unwrap();
            let t0 = Instant::now();
            for (i, (a, b, new, kind)) in edits.into_iter().enumerate() {
                let s = doc.rope().to_string();
                let bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
                let pick = |f: f64| bounds[((bounds.len() - 1) as f64 * f) as usize];
                let (start, end) = (pick(a.min(b)), pick(a.max(b)));
                let kind = [EditKind::Typing, EditKind::Deleting, EditKind::Other][kind as usize];
                let now = t0 + Duration::from_millis(i as u64 * 300);
                doc.edit([(start..end, new.clone())], kind, Selection::cursor(start), Selection::cursor(start + new.len()), now);
            }
            while doc.undo().is_some() {}
            prop_assert_eq!(doc.bytes_for_save(), original.as_bytes());
            prop_assert!(!doc.is_dirty());
        }
    }
}
