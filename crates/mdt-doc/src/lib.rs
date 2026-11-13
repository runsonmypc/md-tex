//! Rope buffer, file I/O, encoding, line endings, undo history and source search.

pub mod decode;
pub mod document;
pub mod history;
pub mod line_style;
pub mod save;
pub mod search;
pub mod watch;

pub use document::{ContentHash, DiskChange, Document, OpenError, content_hash};
pub use history::{EditKind, Selection};
pub use watch::FileWatcher;
