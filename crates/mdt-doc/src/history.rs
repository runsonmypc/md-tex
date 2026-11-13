//! Undo history.
//!
//! Each transaction stores its edits (range, old text, new text) plus the selection before
//! and after. Typing coalesces into word-sized steps and splits after 1 s of idle time.
//! Every transaction carries a version id, so "unsaved changes" is simply "the current
//! version differs from the saved one", which also makes undoing back to a save clean.

use std::collections::VecDeque;
use std::ops::Range;
use std::time::{Duration, Instant};

/// Typing pauses longer than this start a new undo step.
pub const IDLE_SPLIT: Duration = Duration::from_secs(1);
pub const DEFAULT_CAPACITY: usize = 10_000;

/// A selection as byte offsets. `anchor == head` is a cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn cursor(at: usize) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }
}

/// Replaces `range` (in the text as it is when this edit applies) of `old` with `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Typed text; coalesces with adjacent typing.
    Typing,
    /// Backspace or forward delete; coalesces with adjacent deletes.
    Deleting,
    /// Paste, cut, replace, task toggle and anything else: always its own step.
    Other,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    /// Applied in order; each range refers to the text after the previous edits.
    pub edits: Vec<Edit>,
    pub before: Selection,
    pub after: Selection,
    kind: EditKind,
    time: Instant,
    version: u64,
    sealed: bool,
}

#[derive(Debug)]
pub struct History {
    undo: VecDeque<Transaction>,
    redo: Vec<Transaction>,
    capacity: usize,
    next_version: u64,
}

impl Default for History {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl History {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            capacity: capacity.max(1),
            next_version: 1,
        }
    }

    /// The version of the current text: 0 for the original, otherwise the id of the
    /// transaction on top of the undo stack.
    pub fn version(&self) -> u64 {
        self.undo.back().map_or(0, |t| t.version)
    }

    #[cfg(test)]
    pub(crate) fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Ends the current group so the next change starts a new undo step (cursor moved,
    /// document saved, focus changed).
    pub fn seal(&mut self) {
        if let Some(top) = self.undo.back_mut() {
            top.sealed = true;
        }
    }

    /// Records an already-applied change. Clears the redo stack.
    pub fn record(
        &mut self,
        edits: Vec<Edit>,
        kind: EditKind,
        before: Selection,
        after: Selection,
        now: Instant,
    ) {
        self.redo.clear();
        let version = self.next_version;
        self.next_version += 1;
        if let (Some(top), [edit]) = (self.undo.back_mut(), edits.as_slice())
            && coalesce(top, edit, kind, now)
        {
            top.after = after;
            top.time = now;
            top.version = version;
            return;
        }
        self.undo.push_back(Transaction {
            edits,
            before,
            after,
            kind,
            time: now,
            version,
            sealed: false,
        });
        if self.undo.len() > self.capacity {
            self.undo.pop_front();
        }
    }

    /// Pops the most recent transaction; the caller reverts its edits in reverse order.
    /// Typing afterwards starts a new step rather than extending the one now on top.
    pub fn undo(&mut self) -> Option<&Transaction> {
        let mut t = self.undo.pop_back()?;
        t.sealed = true;
        self.seal();
        self.redo.push(t);
        self.redo.last()
    }

    /// Pops the most recently undone transaction; the caller reapplies its edits in order.
    pub fn redo(&mut self) -> Option<&Transaction> {
        let t = self.redo.pop()?;
        self.undo.push_back(t);
        self.undo.back()
    }
}

/// Merges `edit` into `top` when it continues the same typing or deleting run.
fn coalesce(top: &mut Transaction, edit: &Edit, kind: EditKind, now: Instant) -> bool {
    if top.sealed
        || top.kind != kind
        || kind == EditKind::Other
        || now.duration_since(top.time) >= IDLE_SPLIT
    {
        return false;
    }
    let [prev] = top.edits.as_mut_slice() else {
        return false;
    };
    match kind {
        EditKind::Typing => {
            let contiguous =
                edit.old.is_empty() && edit.range.start == prev.range.start + prev.new.len();
            // A word starts a new step: non-whitespace typed right after whitespace.
            let starts_word = prev.new.ends_with(char::is_whitespace)
                && !edit.new.starts_with(char::is_whitespace);
            if !contiguous || starts_word {
                return false;
            }
            prev.new.push_str(&edit.new);
        }
        EditKind::Deleting => {
            if !edit.new.is_empty() || !prev.new.is_empty() {
                return false;
            }
            if edit.range.end == prev.range.start {
                // Backspace run.
                prev.old.insert_str(0, &edit.old);
                prev.range = edit.range.start..prev.range.end;
            } else if edit.range.start == prev.range.start {
                // Forward-delete run.
                prev.old.push_str(&edit.old);
                prev.range = prev.range.start..prev.range.end + edit.old.len();
            } else {
                return false;
            }
        }
        EditKind::Other => return false,
    }
    true
}
