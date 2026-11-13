//! Save, Save As, save-error notices and the unsaved marker in the title.

use std::path::{Path, PathBuf};

use gpui::{AsyncApp, Context, PromptLevel, Task, Window, WindowHandle};

use super::DocumentView;

/// The window title: the file name, with a leading `•` on Linux while unsaved (macOS shows
/// its document-edited marker instead).
pub fn title_text(name: &str, dirty: bool, linux: bool) -> String {
    if dirty && linux {
        format!("• {name}")
    } else {
        name.to_owned()
    }
}

impl DocumentView {
    pub fn window_title(&self) -> String {
        title_text(
            &Self::title_for(&self.doc),
            self.doc.is_dirty(),
            cfg!(target_os = "linux"),
        )
    }

    /// Updates the title and the edited marker when they change.
    pub fn sync_title(&mut self, window: &mut Window) {
        let state = (self.window_title(), self.doc.is_dirty());
        if self.shown_title.as_ref() != Some(&state) {
            window.set_window_title(&state.0);
            window.set_window_edited(state.1);
            window.set_document_path(self.doc.path());
            self.shown_title = Some(state);
        }
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_read_only(cx) {
            return;
        }
        if self.doc.path().is_some() {
            self.write(None, window, cx);
        } else {
            self.save_as(window, cx);
        }
    }

    pub fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_read_only(cx) {
            return;
        }
        self.save_as_task(window, cx).detach();
    }

    /// A read-only document holds a lossy decode of its file (invalid bytes became
    /// U+FFFD), so writing it anywhere would lose data. Says so instead.
    fn refuse_read_only(&mut self, cx: &mut Context<Self>) -> bool {
        if self.doc.read_only().is_none() {
            return false;
        }
        self.notices.push(
            "This file is not valid UTF-8, so it opens read-only and cannot be saved.",
            &[],
        );
        cx.notify();
        true
    }

    /// Asks for a path and saves there; resolves to whether it saved.
    fn save_as_task(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let dir = self
            .doc
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let name = self.doc.path().and_then(|p| p.file_name()).map_or_else(
            || "Untitled.md".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return false;
            };
            this.update_in(cx, |this, window, cx| this.write(Some(path), window, cx))
                .unwrap_or(false)
        })
    }

    /// Before closing: with unsaved changes, asks Save / Don't Save / Cancel. Resolves to
    /// whether the window may close (false after Cancel or a failed save).
    pub fn confirm_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if !self.doc.is_dirty() {
            return Task::ready(true);
        }
        let answer = match self.test_close_answer {
            Some(a) => {
                let (tx, rx) = futures::channel::oneshot::channel();
                let _ = tx.send(a);
                rx
            }
            None => {
                window.activate_window();
                let message = format!(
                    "Do you want to save the changes you made to {}?",
                    Self::title_for(&self.doc)
                );
                window.prompt(
                    PromptLevel::Warning,
                    &message,
                    Some("Your changes will be lost if you don't save them."),
                    &["Save", "Don't Save", "Cancel"],
                    cx,
                )
            }
        };
        cx.spawn_in(window, async move |this, cx| match answer.await {
            Ok(0) => {
                let saved = this.update_in(cx, |this, window, cx| {
                    if this.doc.path().is_some() {
                        Task::ready(this.write(None, window, cx))
                    } else {
                        this.save_as_task(window, cx)
                    }
                });
                match saved {
                    Ok(task) => task.await,
                    Err(_) => false,
                }
            }
            Ok(1) => true,
            _ => false,
        })
    }

    /// Closes the window after confirming unsaved changes.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let confirm = self.confirm_close(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                let _ = this.update_in(cx, |this, window, _| {
                    this.close_confirmed = true;
                    window.remove_window();
                });
            }
        })
        .detach();
    }

    /// Writes the document (to `path` for Save As). A failure leaves the document
    /// unchanged and shows a notice.
    pub fn write(
        &mut self,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = match &path {
            Some(p) => self.doc.save_as(p),
            None => self.doc.save(),
        };
        let ok = match result {
            Ok(()) => {
                if self.watcher.is_none() {
                    self.watch_tried = None;
                }
                true
            }
            Err(e) => {
                let target = path
                    .as_deref()
                    .or(self.doc.path())
                    .map_or_else(|| "the file".to_owned(), |p| p.display().to_string());
                self.notices
                    .push(format!("Could not save {target}: {e}"), &[]);
                false
            }
        };
        self.sync_title(window);
        cx.notify();
        ok
    }
}

/// Notice text and actions for a file changed on disk while there are unsaved edits.
pub const CONFLICT_RELOAD: &str = "Reload";
pub const CONFLICT_KEEP: &str = "Keep Mine";

impl DocumentView {
    /// Watches the bound file (again, after Save As binds a new path). Runs on every
    /// frame, so a watch that fails (its folder is missing) is tried again only when the
    /// path changes or after a save, which may have created the folder.
    pub fn sync_watch(&mut self, cx: &mut Context<Self>) {
        let path = self.doc.path().map(Path::to_path_buf);
        if self.watch_tried == path {
            return;
        }
        self.watch_tried = path.clone();
        self.watcher = None;
        let Some(path) = path else { return };
        self.watch_attempts += 1;
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        self.watcher = mdt_doc::FileWatcher::new(&path, move || {
            let _ = tx.unbounded_send(());
        })
        .ok();
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while rx.next().await.is_some() {
                if this.update(cx, |this, cx| this.disk_changed(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Watches started so far, for tests.
    #[doc(hidden)]
    pub fn watch_attempts(&self) -> u64 {
        self.watch_attempts
    }

    /// Another program touched the file: reload, ask, or say it was deleted. The app's
    /// own saves leave the content it wrote, and are ignored.
    pub fn disk_changed(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.doc.path().map(Path::to_path_buf) else {
            return;
        };
        let name = Self::title_for(&self.doc);
        match mdt_doc::document::read_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if self.doc.on_disk() {
                    self.doc.mark_missing();
                    self.notices.push(
                        format!("{name} was deleted on disk. Saving recreates it."),
                        &[],
                    );
                    cx.notify();
                }
            }
            Err(_) => {}
            Ok(bytes) => {
                if self.doc.classify_disk_content(&bytes) == mdt_doc::DiskChange::OwnWrite {
                    return;
                }
                if self.doc.is_dirty() {
                    self.notices.push(
                        format!("{name} changed on disk."),
                        &[CONFLICT_RELOAD, CONFLICT_KEEP],
                    );
                } else {
                    self.reload(&bytes, cx);
                }
                cx.notify();
            }
        }
    }

    /// Replaces the text with `bytes` from disk, keeping the view where it was.
    pub fn reload(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let path = self.doc.path().map(Path::to_path_buf);
        match mdt_doc::Document::from_bytes(bytes, path) {
            Ok(doc) => {
                self.doc = doc;
                let rope = self.doc.rope().clone();
                {
                    let mut e = self.editor.borrow_mut();
                    e.reset_lines(&rope);
                    e.clamp_selection(&rope);
                    e.marked = None;
                }
                self.reparse(cx);
            }
            Err(e) => {
                self.notices.push(e.to_string(), &[]);
            }
        }
    }

    /// A notice button: Reload discards local edits for the file on disk.
    pub fn notice_action(&mut self, id: u64, action: &str, cx: &mut Context<Self>) {
        self.notices.dismiss(id);
        if action == CONFLICT_RELOAD
            && let Some(bytes) = self
                .doc
                .path()
                .and_then(|p| mdt_doc::document::read_file(p).ok())
        {
            self.reload(&bytes, cx);
        }
        cx.notify();
    }
}

/// Asks about each window with unsaved changes in turn; resolves to whether the app may
/// quit (false as soon as one prompt is cancelled or a save fails).
pub async fn confirm_quit(windows: Vec<WindowHandle<DocumentView>>, cx: &mut AsyncApp) -> bool {
    for h in windows {
        let Ok(task) = h.update(cx, |v, window, cx| v.confirm_close(window, cx)) else {
            continue;
        };
        if !task.await {
            return false;
        }
    }
    true
}

/// The document windows, for asking about unsaved changes before quitting.
pub fn document_windows(cx: &mut gpui::App) -> Vec<WindowHandle<DocumentView>> {
    cx.windows()
        .into_iter()
        .filter_map(|w| w.downcast::<DocumentView>())
        .collect()
}

/// Quits after confirming every window's unsaved changes.
pub fn quit(cx: &mut gpui::App) {
    let windows = document_windows(cx);
    cx.spawn(async move |cx| {
        if confirm_quit(windows, cx).await {
            cx.update(quit_now);
        }
    })
    .detach();
}

/// Quits without asking (every window was confirmed, or there is nothing to ask about).
pub fn quit_now(cx: &mut gpui::App) {
    // macOS asks the app delegate again; this quit is already confirmed.
    #[cfg(target_os = "macos")]
    crate::menu::terminate::allow();
    cx.quit();
}

#[cfg(test)]
mod tests {
    use super::title_text;

    #[test]
    fn unsaved_marker() {
        assert_eq!(title_text("a.md", true, true), "• a.md");
        assert_eq!(title_text("a.md", false, true), "a.md");
        assert_eq!(
            title_text("a.md", true, false),
            "a.md",
            "macOS uses its own marker"
        );
    }
}
