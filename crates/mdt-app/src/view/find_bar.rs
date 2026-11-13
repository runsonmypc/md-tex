//! The find bar over the document: query and replace fields, "N of M", and the actions
//! that move between matches and replace them.

use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use mdt_doc::{EditKind, Selection};

use super::DocumentView;
use crate::find::Find;
use crate::keymap::*;
use crate::reader::view::Found;
use crate::theme::Theme;

impl DocumentView {
    /// Where a new search starts: the cursor, or the top visible unit when rendered.
    fn find_origin(&self, rendered: bool) -> usize {
        if rendered {
            self.reader.borrow().anchor.unit
        } else {
            self.editor.borrow().range().start
        }
    }

    /// Cmd/Ctrl+F, or the replace shortcut (`replace`): opens the bar, or moves the caret
    /// to it. The replace field only appears where the source is edited.
    pub fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let rendered = !self.editor_active();
        let replace = replace && !rendered;
        match &mut self.find {
            Some(f) => {
                if replace && f.replace.is_none() {
                    f.replace = Some(Default::default());
                }
                f.in_replace = false;
            }
            None => {
                let origin = self.find_origin(rendered);
                // The editor's selection seeds the query, as in most editors.
                let seed = if rendered {
                    String::new()
                } else {
                    let s = self.editor_selected_text();
                    if s.contains('\n') { String::new() } else { s }
                };
                let query = if seed.is_empty() {
                    self.last_query.clone()
                } else {
                    seed
                };
                self.find = Some(Find::new(&query, replace, rendered, origin));
            }
        }
        window.focus(&self.find_focus, cx);
        self.find_changed(cx);
    }

    /// Escape: closes the bar and its highlights. In the editor the current match stays
    /// selected.
    pub fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.find.take() {
            self.last_query = f.query.text;
        }
        self.push_found();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Next or previous match (Enter, Cmd/Ctrl+G and their Shift forms). With the bar
    /// closed, reopens it with the last query.
    pub fn find_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            if self.last_query.is_empty() {
                return;
            }
            self.open_find(false, window, cx);
        }
        self.refresh_find();
        if let Some(f) = &mut self.find {
            f.step(forward);
        }
        self.goto_match(cx);
    }

    /// The query changed: search again and show the first match from the origin.
    pub fn find_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_find();
        self.goto_match(cx);
    }

    /// Recomputes matches when the query, the searched text or the mode changed.
    pub fn refresh_find(&mut self) -> bool {
        let rendered = !self.editor_active();
        let origin = self.find_origin(rendered);
        let parsed = self.parsed();
        let (epoch, text) = (self.model_epoch, self.text_generation());
        let Some(f) = &mut self.find else {
            return false;
        };
        if f.rendered != rendered {
            f.rendered = rendered;
            f.origin = origin;
            if rendered {
                f.replace = None;
                f.in_replace = false;
            }
        }
        let reader = self.reader.borrow();
        // Source ranges from a model older than the text would point into the old text.
        let changed = if parsed || f.rendered {
            f.refresh(self.doc.rope(), &reader.model, epoch, text, origin)
        } else {
            f.wait_for_model(epoch, text, self.doc.rope().len_bytes())
        };
        drop(reader);
        if changed {
            self.push_found();
        }
        changed
    }

    /// Hands the matches to the reader and the editor for painting.
    fn push_found(&mut self) {
        let mut reader = self.reader.borrow_mut();
        let mut editor = self.editor.borrow_mut();
        reader.found.clear();
        editor.found.clear();
        let Some(f) = &self.find else { return };
        for (i, h) in f.hits.iter().enumerate() {
            let current = f.current == Some(i);
            if f.rendered {
                reader.found.entry(h.unit).or_default().push(Found {
                    text: h.text,
                    range: h.range.clone(),
                    current,
                });
            } else {
                editor.found.push((h.range.clone(), current));
            }
        }
    }

    /// Scrolls the current match into view; in the editor it also becomes the selection.
    fn goto_match(&mut self, cx: &mut Context<Self>) {
        self.push_found();
        let Some((hit, rendered)) = self
            .find
            .as_ref()
            .and_then(|f| f.current_hit().map(|h| (h.clone(), f.rendered)))
        else {
            cx.notify();
            return;
        };
        let fonts = crate::reader::view::fonts(cx);
        if rendered {
            self.reader.borrow_mut().reveal_found(
                hit.unit,
                hit.text,
                hit.range,
                &mut fonts.borrow_mut(),
                cx,
            );
        } else {
            let rope = self.doc.rope().clone();
            let mut e = self.editor.borrow_mut();
            e.selection = Selection {
                anchor: hit.range.start,
                head: hit.range.end,
            };
            e.reveal_head(&rope, &mut fonts.borrow_mut(), cx);
            drop(e);
            self.doc.seal_undo_group();
            self.reveal_cursor_in_preview();
        }
        cx.notify();
    }

    /// Replaces the current match and moves to the next one.
    pub fn replace_one(&mut self, cx: &mut Context<Self>) {
        self.refresh_find();
        let Some(f) = &self.find else { return };
        let (Some(with), false) = (f.replace.as_ref().map(|r| r.text.clone()), f.rendered) else {
            return;
        };
        let Some(hit) = f.current_hit().cloned() else {
            return;
        };
        if !f.still_matches(self.doc.rope(), &hit) {
            return;
        }
        let before = self.editor.borrow().selection;
        let end = hit.range.start + with.len();
        self.doc.seal_undo_group();
        if !self.doc.edit(
            [(hit.range.clone(), with)],
            EditKind::Other,
            before,
            Selection::cursor(end),
            std::time::Instant::now(),
        ) {
            return;
        }
        self.doc.seal_undo_group();
        self.editor.borrow_mut().selection = Selection::cursor(end);
        self.after_replace(end, cx);
    }

    /// Replaces every match as one undo step.
    pub fn replace_all(&mut self, cx: &mut Context<Self>) {
        self.refresh_find();
        let Some(f) = &self.find else { return };
        let (Some(with), false) = (f.replace.as_ref().map(|r| r.text.clone()), f.rendered) else {
            return;
        };
        // Back to front, so each range still refers to the original text. Hits that no
        // longer match (the text changed under them) are left alone.
        let rope = self.doc.rope();
        let edits: Vec<_> = f
            .hits
            .iter()
            .rev()
            .filter(|h| f.still_matches(rope, h))
            .map(|h| (h.range.clone(), with.clone()))
            .collect();
        if edits.is_empty() {
            return;
        }
        let before = self.editor.borrow().selection;
        let cursor = self
            .editor
            .borrow()
            .selection
            .head
            .min(self.doc.rope().len_bytes());
        self.doc.seal_undo_group();
        if !self.doc.edit(
            edits,
            EditKind::Other,
            before,
            Selection::cursor(cursor),
            std::time::Instant::now(),
        ) {
            return;
        }
        self.doc.seal_undo_group();
        let len = self.doc.rope().len_bytes();
        self.editor.borrow_mut().selection = Selection::cursor(cursor.min(len));
        self.after_replace(cursor, cx);
    }

    fn after_replace(&mut self, origin: usize, cx: &mut Context<Self>) {
        let rope = self.doc.rope().clone();
        self.editor.borrow_mut().reset_lines(&rope);
        self.edited(cx);
        if let Some(f) = &mut self.find {
            // Search again from just after the replacement.
            f.restart(origin);
        }
        self.find_changed(cx);
    }

    pub fn find_type(&mut self, text: &str, cx: &mut Context<Self>) {
        self.field_edit(super::Overlay::Find, cx, |f| f.insert(text));
    }

    pub fn render_find(
        &self,
        theme: &Theme,
        focus: &FocusHandle,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let f = self.find.as_ref()?;
        let focused = focus.is_focused(window);
        let (fg, muted) = (theme.overlay_text, theme.muted);
        let field = |field: &crate::field::Field, placeholder: &str, active: bool| {
            div()
                .flex_1()
                .min_w(px(120.0))
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .bg(theme.overlay_button)
                .overflow_hidden()
                .child(field.render(placeholder, focused && active, fg, muted))
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .bg(theme.overlay_button)
                .cursor_pointer()
                .child(label)
        };
        let in_replace = f.in_replace && f.replace.is_some();
        let bar = super::field_keys(div().id("find-bar"), super::Overlay::Find, cx)
            .track_focus(focus)
            .absolute()
            .top(px(12.0))
            .right(px(12.0))
            .w(px(420.0))
            .max_w_full()
            .p(px(8.0))
            .rounded(px(6.0))
            .bg(theme.overlay_background)
            .text_color(fg)
            .text_sm()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if k.key == "enter" && k.modifiers.shift && !k.modifiers.platform {
                    this.find_step(false, window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &Newline, window, cx| {
                let in_replace = this
                    .find
                    .as_ref()
                    .is_some_and(|f| f.in_replace && f.replace.is_some());
                if in_replace {
                    this.replace_one(cx);
                } else {
                    this.find_step(true, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Indent, _, cx| {
                if let Some(f) = &mut this.find
                    && f.replace.is_some()
                {
                    f.in_replace = !f.in_replace;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Outdent, _, cx| {
                if let Some(f) = &mut this.find
                    && f.replace.is_some()
                {
                    f.in_replace = !f.in_replace;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.close_find(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(field(&f.query, "Find", !in_replace))
                    .child(div().min_w(px(72.0)).text_color(muted).child(f.status()))
                    .child(button("find-prev", "↑").on_click(
                        cx.listener(|this, _, window, cx| this.find_step(false, window, cx)),
                    ))
                    .child(button("find-next", "↓").on_click(
                        cx.listener(|this, _, window, cx| this.find_step(true, window, cx)),
                    )),
            )
            .when_some(f.replace.as_ref(), |bar, r| {
                bar.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(field(r, "Replace", in_replace))
                        .child(
                            button("replace-one", "Replace")
                                .on_click(cx.listener(|this, _, _, cx| this.replace_one(cx))),
                        )
                        .child(
                            button("replace-all", "All")
                                .on_click(cx.listener(|this, _, _, cx| this.replace_all(cx))),
                        ),
                )
            });
        Some(bar)
    }
}
