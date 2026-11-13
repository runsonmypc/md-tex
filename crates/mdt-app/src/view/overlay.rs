//! The command palette overlay, and the text-field keys it shares with the find bar.

use gpui::{
    Context, FocusHandle, FontWeight, HighlightStyle, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Stateful, StatefulInteractiveElement, Styled, StyledText, Window,
    div, prelude::FluentBuilder, px,
};

use super::DocumentView;
use crate::field::Field;
use crate::keymap::*;
use crate::palette::{Entry, Palette, Run, VISIBLE};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Find,
    Palette,
}

/// Typing and the editing keys for an overlay's text field. Bound keys arrive as actions
/// first; anything else that types a character arrives as a key-down event.
pub fn field_keys(
    el: Stateful<gpui::Div>,
    which: Overlay,
    cx: &mut Context<DocumentView>,
) -> Stateful<gpui::Div> {
    el.on_key_down(cx.listener(move |this, ev: &KeyDownEvent, _, cx| {
        let k = &ev.keystroke;
        if k.modifiers.platform || k.modifiers.control || k.modifiers.function {
            return;
        }
        if let Some(ch) = &k.key_char
            && !ch.chars().any(char::is_control)
        {
            let ch = ch.clone();
            this.field_edit(which, cx, |f| f.insert(&ch));
            cx.stop_propagation();
        }
    }))
    .on_action(
        cx.listener(move |this, _: &Backspace, _, cx| this.field_edit(which, cx, Field::backspace)),
    )
    .on_action(
        cx.listener(move |this, _: &Delete, _, cx| this.field_edit(which, cx, Field::delete)),
    )
    .on_action(cx.listener(move |this, _: &DeleteWordLeft, _, cx| {
        this.field_edit(which, cx, Field::delete_word_left)
    }))
    .on_action(cx.listener(move |this, _: &DeleteLineLeft, _, cx| {
        this.field_edit(which, cx, |f| {
            f.text.replace_range(..f.caret, "");
            f.caret = 0;
        })
    }))
    .on_action(
        cx.listener(move |this, _: &MoveLeft, _, cx| this.field_edit(which, cx, Field::left)),
    )
    .on_action(
        cx.listener(move |this, _: &MoveRight, _, cx| this.field_edit(which, cx, Field::right)),
    )
    .on_action(
        cx.listener(move |this, _: &MoveLineStart, _, cx| this.field_edit(which, cx, Field::home)),
    )
    .on_action(
        cx.listener(move |this, _: &MoveLineEnd, _, cx| this.field_edit(which, cx, Field::end)),
    )
    // Nothing to select in a one-line field; the document's selection stays.
    .on_action(cx.listener(|_, _: &SelectAll, _, _| {}))
    .on_action(cx.listener(move |this, _: &Paste, _, cx| {
        if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
            this.field_edit(which, cx, |f| f.insert(&t));
        }
    }))
}

impl DocumentView {
    /// Applies `edit` to the overlay's active field and reacts to the change.
    pub fn field_edit(
        &mut self,
        which: Overlay,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Field),
    ) {
        match which {
            Overlay::Find => {
                let Some(f) = &mut self.find else { return };
                let in_query = !f.in_replace || f.replace.is_none();
                edit(f.field());
                if in_query {
                    self.find_changed(cx);
                } else {
                    cx.notify();
                }
            }
            Overlay::Palette => {
                let Some(p) = &mut self.palette else { return };
                edit(&mut p.query);
                p.update();
                cx.notify();
            }
        }
    }

    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let headings = self.reader.borrow().model.headings.clone();
        self.palette = Some(Palette::new(&headings, crate::recent::list(cx), ""));
        window.focus(&self.palette_focus, cx);
        cx.notify();
    }

    /// Escape: closes the palette; nothing else changed while it was open.
    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn palette_type(&mut self, text: &str, cx: &mut Context<Self>) {
        self.field_edit(Overlay::Palette, cx, |f| f.insert(text));
    }

    /// Enter: runs the highlighted entry.
    pub fn palette_choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.palette.as_ref().and_then(|p| p.chosen().cloned()) else {
            return;
        };
        self.close_palette(window, cx);
        match entry {
            Entry::Heading { index, .. } => self.jump_to_heading(index, cx),
            Entry::Recent(path) => {
                if path.exists() {
                    self.open_here(&[path], None, window, cx);
                } else {
                    crate::recent::forget(cx, &path);
                    self.notices.push(
                        format!(
                            "{} is missing; it was removed from recent files.",
                            path.display()
                        ),
                        &[],
                    );
                }
            }
            Entry::Command(c) => match c.run {
                Run::Action(action) => {
                    let action = action();
                    window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
                }
                Run::Setting(f, key) => crate::app_settings::change(cx, f, key),
            },
        }
        cx.notify();
    }

    /// Scrolls the heading to the top; in Source and Split the cursor moves to its line.
    pub fn jump_to_heading(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(h) = self.reader.borrow().model.headings.get(index).cloned() else {
            return;
        };
        self.push_history();
        self.jump_to(
            crate::links::Place {
                unit: h.unit,
                text: 0,
                source: h.range.start,
            },
            cx,
        );
    }

    pub fn render_palette(
        &self,
        theme: &Theme,
        focus: &FocusHandle,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let p = self.palette.as_ref()?;
        let (fg, muted) = (theme.overlay_text, theme.muted);
        let focused = focus.is_focused(window);
        let hl = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            color: Some(theme.link),
            ..Default::default()
        };
        let rows = p
            .rows
            .iter()
            .enumerate()
            .skip(p.top)
            .take(VISIBLE)
            .map(|(n, row)| {
                let entry = &p.entries[row.entry];
                let label = StyledText::new(entry.label())
                    .with_highlights(row.matched.iter().map(|r| (r.clone(), hl)));
                let (tag, right) = match entry {
                    Entry::Heading { level, .. } => (format!("H{level}"), None),
                    Entry::Recent(_) => ("Recent".to_owned(), None),
                    Entry::Command(c) => (String::new(), c.shortcut.clone()),
                };
                div()
                    .id(("palette-row", n))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .when(n == p.selected, |d| d.bg(theme.overlay_button))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(p) = &mut this.palette {
                            p.selected = n;
                        }
                        this.palette_choose(window, cx);
                    }))
                    .when(!tag.is_empty(), |d| {
                        // At least as wide as "H6"; "Recent" takes what it needs, unwrapped.
                        d.child(
                            div()
                                .flex_none()
                                .min_w(px(44.0))
                                .whitespace_nowrap()
                                .text_color(muted)
                                .child(tag),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(label),
                    )
                    .children(right.map(|s| div().text_color(muted).child(s)))
            })
            .collect::<Vec<_>>();
        let panel = field_keys(div().id("palette"), Overlay::Palette, cx)
            .track_focus(focus)
            .w(px(560.0))
            .max_w_full()
            .p(px(8.0))
            .rounded(px(8.0))
            .bg(theme.overlay_background)
            .text_color(fg)
            .text_sm()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| {
                if let Some(p) = &mut this.palette {
                    p.move_by(-1);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| {
                if let Some(p) = &mut this.palette {
                    p.move_by(1);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                if let Some(p) = &mut this.palette {
                    p.move_by(-(VISIBLE as isize));
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                if let Some(p) = &mut this.palette {
                    p.move_by(VISIBLE as isize);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Newline, window, cx| this.palette_choose(window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.close_palette(window, cx)))
            .on_action(
                cx.listener(|this, _: &CommandPalette, window, cx| this.close_palette(window, cx)),
            )
            .child(
                div()
                    .px(px(6.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .bg(theme.overlay_button)
                    .child(p.query.render(
                        "Headings, recent files, > commands, # headings",
                        focused,
                        fg,
                        muted,
                    )),
            )
            .children(rows)
            .when(p.rows.is_empty(), |d| {
                d.child(
                    div()
                        .px(px(8.0))
                        .py(px(3.0))
                        .text_color(muted)
                        .child("No matches"),
                )
            });
        Some(
            div()
                .absolute()
                .top(px(48.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .px(px(16.0))
                .child(panel),
        )
    }
}
