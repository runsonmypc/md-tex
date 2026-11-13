//! Editing commands and the IME input handler for the Source editor.

use std::ops::Range;
use std::time::Instant;

use gpui::{
    Bounds, ClipboardItem, Context, EntityInputHandler, Pixels, UTF16Selection, Window, point, px,
    size,
};
use mdt_doc::{EditKind, Selection};

use super::DocumentView;
use crate::editor::text::{
    self, byte_to_utf16, indent_edits, line_of, outdent_edits, utf16_to_byte,
};
use crate::reader::view::fonts;

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    PageUp,
    PageDown,
}

impl DocumentView {
    /// Where `motion` takes the selection head.
    fn target(&mut self, motion: Motion, cx: &mut Context<Self>) -> usize {
        let rope = self.doc.rope().clone();
        let fonts = fonts(cx);
        let mut fonts = fonts.borrow_mut();
        let mut e = self.editor.borrow_mut();
        let head = e.selection.head;
        let r = e.range();
        match motion {
            // With a selection, Left and Right collapse it to its edge.
            Motion::Left if !r.is_empty() => r.start,
            Motion::Right if !r.is_empty() => r.end,
            Motion::Left => text::prev_grapheme(&rope, head),
            Motion::Right => text::next_grapheme(&rope, head),
            Motion::WordLeft => text::prev_word(&rope, head),
            Motion::WordRight => text::next_word(&rope, head),
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                let down = matches!(motion, Motion::Down | Motion::PageDown);
                let page = matches!(motion, Motion::PageUp | Motion::PageDown);
                let goal = e.goal_x;
                let b = e.vertical(&rope, &mut fonts, cx, head, down, page);
                if page {
                    let dy = if down { e.viewport_h } else { -e.viewport_h };
                    e.scroll_by(dy);
                }
                // Keep the goal x across a run of vertical moves.
                e.goal_x = e.goal_x.or(goal);
                b
            }
            Motion::LineStart => e.visual_line_edge(&rope, &mut fonts, cx, head, false),
            Motion::LineEnd => e.visual_line_edge(&rope, &mut fonts, cx, head, true),
            Motion::DocStart => 0,
            Motion::DocEnd => rope.len_bytes(),
        }
    }

    /// Moves the cursor (or extends the selection with `select`).
    pub fn move_cursor(&mut self, motion: Motion, select: bool, cx: &mut Context<Self>) {
        let vertical = matches!(
            motion,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
        );
        if !vertical {
            self.editor.borrow_mut().goal_x = None;
        }
        let b = self.target(motion, cx);
        {
            let mut e = self.editor.borrow_mut();
            e.selection = if select {
                Selection {
                    anchor: e.selection.anchor,
                    head: b,
                }
            } else {
                Selection::cursor(b)
            };
            e.marked = None;
        }
        self.cursor_moved(cx);
    }

    /// After the cursor or selection changed without an edit.
    pub fn cursor_moved(&mut self, cx: &mut Context<Self>) {
        self.doc.seal_undo_group();
        {
            let rope = self.doc.rope().clone();
            let fonts = fonts(cx);
            let mut e = self.editor.borrow_mut();
            e.used = true;
            e.last_input = Instant::now();
            e.reveal_head(&rope, &mut fonts.borrow_mut(), cx);
        }
        self.reveal_cursor_in_preview();
        #[cfg(target_os = "linux")]
        {
            let text = self.editor_selected_text();
            if !text.is_empty() {
                cx.write_to_primary(ClipboardItem::new_string(text));
            }
        }
        cx.notify();
    }

    pub fn editor_selected_text(&self) -> String {
        let r = self.editor.borrow().range();
        self.doc.rope().byte_slice(r).to_string()
    }

    /// Applies `edits` (in application order) as one undo step and places the cursor.
    pub fn apply_edits(
        &mut self,
        edits: Vec<(Range<usize>, String)>,
        kind: EditKind,
        after: Selection,
        cx: &mut Context<Self>,
    ) {
        if edits.is_empty() {
            return;
        }
        let before = self.editor.borrow().selection;
        let (lines_before, first, old_last) = {
            let rope = self.doc.rope();
            let first = edits
                .iter()
                .map(|(r, _)| line_of(rope, r.start))
                .min()
                .unwrap_or(0);
            let last = edits
                .iter()
                .map(|(r, _)| line_of(rope, r.end))
                .max()
                .unwrap_or(0);
            (rope.len_lines(), first, last)
        };
        if kind != EditKind::Typing && kind != EditKind::Deleting {
            self.doc.seal_undo_group();
        }
        if !self.doc.edit(edits, kind, before, after, Instant::now()) {
            return;
        }
        if kind == EditKind::Other {
            self.doc.seal_undo_group();
        }
        let rope = self.doc.rope().clone();
        let new_last = (old_last + rope.len_lines()).saturating_sub(lines_before);
        {
            let mut e = self.editor.borrow_mut();
            e.lines_replaced(&rope, first, old_last, new_last);
            e.selection = after;
            e.goal_x = None;
        }
        self.edited(cx);
    }

    /// After any change to the text: reparse, keep the cursor and its block in view.
    pub fn edited(&mut self, cx: &mut Context<Self>) {
        let rope = self.doc.rope().clone();
        {
            let fonts = fonts(cx);
            let mut e = self.editor.borrow_mut();
            e.clamp_selection(&rope);
            e.used = true;
            e.last_input = Instant::now();
            e.reveal_head(&rope, &mut fonts.borrow_mut(), cx);
        }
        self.reparse(cx);
        self.reveal_cursor_in_preview();
        cx.notify();
    }

    /// Replaces the selection with `text`.
    pub fn insert(&mut self, text: &str, kind: EditKind, cx: &mut Context<Self>) {
        let r = self.editor.borrow().range();
        let end = r.start + text.len();
        self.apply_edits(vec![(r, text.to_owned())], kind, Selection::cursor(end), cx);
    }

    pub fn newline(&mut self, cx: &mut Context<Self>) {
        let nl = self.doc.line_ending().as_str();
        self.insert(nl, EditKind::Typing, cx);
    }

    /// Deletes the selection, or from the cursor to where `motion` would take it.
    pub fn delete(&mut self, motion: Motion, cx: &mut Context<Self>) {
        let r = self.editor.borrow().range();
        let (range, kind) = if r.is_empty() {
            let b = self.target(motion, cx);
            (b.min(r.start)..b.max(r.start), EditKind::Deleting)
        } else {
            (r, EditKind::Other)
        };
        if range.is_empty() {
            return;
        }
        let at = range.start;
        self.apply_edits(
            vec![(range, String::new())],
            kind,
            Selection::cursor(at),
            cx,
        );
    }

    /// Tab: indents the selected lines, or inserts one unit at the cursor.
    pub fn indent(&mut self, outdent: bool, cx: &mut Context<Self>) {
        let unit = self.doc.indent_unit().as_string();
        let (sel, lines) = {
            let rope = self.doc.rope();
            let sel = self.editor.borrow().selection;
            let r = self.editor.borrow().range();
            let first = line_of(rope, r.start);
            let mut last = line_of(rope, r.end);
            // A selection ending at a line start does not include that line.
            if last > first && rope.line_to_byte(last) == r.end {
                last -= 1;
            }
            (sel, first..last + 1)
        };
        if !outdent && lines.len() == 1 && sel.anchor == sel.head {
            self.insert(&unit, EditKind::Other, cx);
            return;
        }
        let rope = self.doc.rope().clone();
        let edits = if outdent {
            outdent_edits(&rope, lines, &unit)
        } else {
            indent_edits(&rope, lines, &unit)
        };
        // Each edit moves the offsets after it.
        let shift = |p: usize| -> usize {
            edits.iter().fold(p as isize, |acc, (r, new)| {
                if outdent {
                    let n = r.len() as isize;
                    if p >= r.end {
                        acc - n
                    } else if p > r.start {
                        acc - (p - r.start) as isize
                    } else {
                        acc
                    }
                } else if r.start <= p {
                    acc + new.len() as isize
                } else {
                    acc
                }
            }) as usize
        };
        let after = Selection {
            anchor: shift(sel.anchor),
            head: shift(sel.head),
        };
        self.apply_edits(edits, EditKind::Other, after, cx);
    }

    pub fn cut(&mut self, cx: &mut Context<Self>) {
        let text = self.editor_selected_text();
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.delete(Motion::Left, cx);
    }

    /// Pastes plain text as one undo step, converting line breaks to the document's style.
    pub fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = self.doc.line_ending().convert(text);
        self.insert(&text, EditKind::Other, cx);
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.paste_text(&text, cx);
        }
    }

    /// Undo or redo in the editor: restores the selection.
    pub fn restore(&mut self, sel: Selection, cx: &mut Context<Self>) {
        let rope = self.doc.rope().clone();
        {
            let mut e = self.editor.borrow_mut();
            e.reset_lines(&rope);
            e.selection = sel;
            e.marked = None;
        }
        self.edited(cx);
    }

    fn utf16_range(&self, r: &Range<usize>) -> Range<usize> {
        let rope = self.doc.rope();
        byte_to_utf16(rope, r.start)..byte_to_utf16(rope, r.end)
    }

    fn byte_range(&self, r: &Range<usize>) -> Range<usize> {
        let rope = self.doc.rope();
        utf16_to_byte(rope, r.start)..utf16_to_byte(rope, r.end)
    }

    fn input_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|r| self.byte_range(&r))
            .or_else(|| self.editor.borrow().marked.clone())
            .unwrap_or_else(|| self.editor.borrow().range())
    }
}

impl EntityInputHandler for DocumentView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let r = self.byte_range(&range_utf16);
        *adjusted = Some(self.utf16_range(&r));
        Some(self.doc.rope().byte_slice(r).to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let s = self.editor.borrow().selection;
        let r = self.editor.borrow().range();
        Some(UTF16Selection {
            range: self.utf16_range(&r),
            reversed: s.head < s.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let m = self.editor.borrow().marked.clone()?;
        Some(self.utf16_range(&m))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.editor.borrow_mut().marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editor_shown() {
            return;
        }
        let r = self.input_range(range_utf16);
        self.editor.borrow_mut().marked = None;
        let end = r.start + text.len();
        self.apply_edits(
            vec![(r, text.to_owned())],
            EditKind::Typing,
            Selection::cursor(end),
            cx,
        );
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editor_shown() {
            return;
        }
        let r = self.input_range(range_utf16);
        let start = r.start;
        // The selection inside the composition, in UTF-16 units of `new_text`.
        let to_byte = |u: usize| {
            new_text
                .char_indices()
                .scan(0, |n, (i, c)| {
                    let at = *n;
                    *n += c.len_utf16();
                    Some((at, i))
                })
                .find(|&(at, _)| at >= u)
                .map_or(new_text.len(), |(_, i)| i)
        };
        let sel = new_selected.map_or(new_text.len()..new_text.len(), |s| {
            to_byte(s.start)..to_byte(s.end)
        });
        let after = Selection {
            anchor: start + sel.start,
            head: start + sel.end,
        };
        self.apply_edits(vec![(r, new_text.to_owned())], EditKind::Typing, after, cx);
        self.editor.borrow_mut().marked =
            (!new_text.is_empty()).then(|| start..start + new_text.len());
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let b = self.byte_range(&range_utf16).start;
        let rope = self.doc.rope().clone();
        let fonts = fonts(cx);
        let mut e = self.editor.borrow_mut();
        let (x, y, h) = e.caret(&rope, &mut fonts.borrow_mut(), cx, b);
        let wy = (y - e.scroll_y()) as f32 + e.origin.1;
        let wx = x - e.hscroll + e.origin.0;
        Some(Bounds::new(point(px(wx), px(wy)), size(px(2.0), px(h))))
    }

    fn character_index_for_point(
        &mut self,
        p: gpui::Point<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let rope = self.doc.rope().clone();
        let fonts = fonts(cx);
        let b = self.editor.borrow_mut().hit_test(
            &rope,
            &mut fonts.borrow_mut(),
            cx,
            (f32::from(p.x), f32::from(p.y)),
        );
        Some(byte_to_utf16(&rope, b))
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.editor_shown() && self.doc.read_only().is_none()
    }
}
