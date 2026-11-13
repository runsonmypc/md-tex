//! Following links from the document, and the per-window back and forward history.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{App, Context, PromptLevel, Window};
use mdt_doc::Selection;

use super::DocumentView;
use crate::links::{Place, Target, classify, footnote_definition, footnote_reference, heading_for};
use crate::reader::block::Link;
use crate::reader::heights::Anchor;
use crate::settings::ViewMode;

type OpenUrl = Rc<dyn Fn(&str, &mut App)>;
type Reveal = Rc<dyn Fn(&Path, &mut App)>;

/// How links leave the app: the system handler for URLs, the file manager for other
/// files. Tests replace it with a fake that records the calls.
#[derive(Clone)]
pub struct Opener {
    pub open_url: OpenUrl,
    pub reveal: Reveal,
}

impl gpui::Global for Opener {}

fn opener(cx: &App) -> Opener {
    cx.try_global::<Opener>()
        .cloned()
        .unwrap_or_else(|| Opener {
            open_url: Rc::new(|url, cx| cx.open_url(url)),
            reveal: Rc::new(|path, cx| cx.reveal_path(path)),
        })
}

/// A position to return to: both views' scroll and the cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    reader: Anchor,
    editor: Anchor,
    selection: Selection,
}

impl DocumentView {
    fn spot(&self) -> Spot {
        let e = self.editor.borrow();
        Spot {
            reader: self.reader.borrow().anchor,
            editor: e.anchor,
            selection: e.selection,
        }
    }

    fn restore_spot(&mut self, s: Spot, cx: &mut Context<Self>) {
        self.reader.borrow_mut().anchor = s.reader;
        let len = self.doc.rope().len_bytes();
        let mut e = self.editor.borrow_mut();
        e.anchor = s.editor;
        e.selection = Selection {
            anchor: s.selection.anchor.min(len),
            head: s.selection.head.min(len),
        };
        drop(e);
        cx.notify();
    }

    /// Remembers where the view is before a jump; a new jump clears the forward list.
    pub fn push_history(&mut self) {
        let s = self.spot();
        if self.back.last() != Some(&s) {
            self.back.push(s);
        }
        self.forward.clear();
    }

    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.back.pop() {
            self.forward.push(self.spot());
            self.restore_spot(s, cx);
        }
    }

    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.forward.pop() {
            self.back.push(self.spot());
            self.restore_spot(s, cx);
        }
    }

    /// Shows a place: the editor's cursor moves there with its line at the top, and the
    /// reader scrolls its text to the top.
    pub fn jump_to(&mut self, place: Place, cx: &mut Context<Self>) {
        if self.editor_shown() {
            let rope = self.doc.rope().clone();
            let at = place.source.min(rope.len_bytes());
            let line = rope.byte_to_line(at);
            let mut e = self.editor.borrow_mut();
            e.selection = Selection::cursor(at);
            let (top, vp) = (e.heights.top(line), e.viewport_h as f64);
            let heights = e.heights.clone();
            e.anchor.scroll_to(top, &heights, vp);
            e.used = true;
            drop(e);
            self.doc.seal_undo_group();
            if self.mode == ViewMode::Split {
                self.sync_preview();
            }
        } else {
            let fonts = crate::reader::view::fonts(cx);
            self.reader.borrow_mut().scroll_to_text(
                place.unit,
                place.text,
                &mut fonts.borrow_mut(),
                cx,
            );
        }
        cx.notify();
    }

    /// Scrolls to the heading with slug `fragment`, or says it is not there. While only a
    /// prefix of a large document is parsed, a heading not in it is looked for again when
    /// the full parse lands.
    pub fn go_to_anchor(&mut self, fragment: &str, cx: &mut Context<Self>) {
        let place = {
            let r = self.reader.borrow();
            heading_for(&r.model, fragment).map(|i| {
                let h = &r.model.headings[i];
                Place {
                    unit: h.unit,
                    text: 0,
                    source: h.range.start,
                }
            })
        };
        match place {
            Some(p) => {
                self.push_history();
                self.jump_to(p, cx);
            }
            None if !self.parsed() => self.pending_anchor = Some(fragment.to_owned()),
            None => {
                self.notices
                    .push(format!("No heading matches the anchor #{fragment}."), &[]);
                cx.notify();
            }
        }
    }

    /// Opens files from this window (a link, a recent file, a drop). This window's own
    /// file stays here, scrolled to `fragment` if given: GPUI cannot read the window being
    /// updated, so `launch::open_all` would miss it and open a second window.
    pub fn open_here(
        &mut self,
        paths: &[PathBuf],
        fragment: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let own = self.doc.path().map(Path::to_path_buf);
        let (mine, others): (Vec<PathBuf>, Vec<PathBuf>) = paths.iter().cloned().partition(|p| {
            own.as_deref()
                .is_some_and(|o| crate::launch::same_file(o, p))
        });
        if !others.is_empty() {
            for e in crate::launch::open_all(&others, cx) {
                self.notices.push(e, &[]);
            }
            if let Some(f) = fragment {
                for p in &others {
                    if let Some(w) = crate::launch::window_for(p, cx) {
                        let _ = w.update(cx, |v, _, cx| v.go_to_anchor(f, cx));
                    }
                }
            }
        }
        if let Some(p) = mine.first() {
            crate::recent::note(cx, p);
            match fragment {
                Some(f) => self.go_to_anchor(f, cx),
                None => window.activate_window(),
            }
        }
        cx.notify();
    }

    /// Follows a clicked link.
    pub fn follow_link(&mut self, link: Link, window: &mut Window, cx: &mut Context<Self>) {
        let footnote = |this: &Self, label: &str, back: bool| {
            let r = this.reader.borrow();
            if back {
                footnote_reference(&r.model, label)
            } else {
                footnote_definition(&r.model, label)
            }
        };
        let dest = match link {
            Link::Footnote(label) => {
                if let Some(p) = footnote(self, &label, false) {
                    self.push_history();
                    self.jump_to(p, cx);
                }
                return;
            }
            Link::FootnoteBack(label) => {
                if let Some(p) = footnote(self, &label, true) {
                    self.push_history();
                    self.jump_to(p, cx);
                }
                return;
            }
            Link::Url(dest) => dest,
        };
        let dir = self
            .doc
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        match classify(&dest, &dir) {
            Target::Anchor(f) => self.go_to_anchor(&f, cx),
            Target::Web(url) => (opener(cx).open_url)(&url, cx),
            Target::OtherScheme(url) => self.confirm_open(url, window, cx),
            Target::Local { path, fragment } => match std::fs::metadata(&path) {
                Err(_) => {
                    self.notices
                        .push(format!("{} does not exist.", path.display()), &[]);
                    cx.notify();
                }
                Ok(_) if crate::links::is_markdown(&path) => {
                    self.open_here(&[path], fragment.as_deref(), window, cx);
                }
                // Revealed, never opened: the file might be a program. Only files and
                // folders: GPUI's Linux reveal opens the path (blocking) to hand it to the
                // file manager, and opening a FIFO waits for a writer while opening a
                // device runs its driver.
                Ok(meta) if meta.is_file() || meta.is_dir() => (opener(cx).reveal)(&path, cx),
                Ok(_) => {
                    self.notices
                        .push(format!("{} is not a regular file.", path.display()), &[]);
                    cx.notify();
                }
            },
        }
    }

    /// A link with an unfamiliar scheme opens only after the user confirms it.
    fn confirm_open(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let answer = match self.test_link_answer {
            Some(a) => {
                let (tx, rx) = futures::channel::oneshot::channel();
                let _ = tx.send(a);
                rx
            }
            None => window.prompt(
                PromptLevel::Warning,
                "Open this link in another app?",
                Some(&url),
                &["Open", "Cancel"],
                cx,
            ),
        };
        cx.spawn(async move |_, cx| {
            if let Ok(0) = answer.await {
                cx.update(|cx| (opener(cx).open_url)(&url, cx));
            }
        })
        .detach();
    }

    /// Cmd-click (macOS) or Ctrl-click (Linux) in the editor at source offset `at`;
    /// returns whether there was a link to follow.
    pub fn follow_link_at(
        &mut self,
        at: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let link = crate::links::link_at_source(&self.reader.borrow().model, at);
        match link {
            Some(l) => {
                self.follow_link(l, window, cx);
                true
            }
            None => false,
        }
    }
}
