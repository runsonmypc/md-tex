//! A one-line text field for the find bar and the palette: text, a caret, and the editing
//! keys. Typed characters arrive as key-down events; there is no IME composition.

use gpui::{Hsla, IntoElement, ParentElement, Styled, div, px};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Field {
    pub text: String,
    /// Byte offset of the caret.
    pub caret: usize,
}

impl Field {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            caret: text.len(),
        }
    }

    pub fn set(&mut self, text: &str) {
        *self = Self::new(text);
    }

    pub fn insert(&mut self, s: &str) {
        // A pasted line break would make a query that matches nothing.
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.caret, &s);
        self.caret += s.len();
    }

    fn prev(&self, at: usize) -> usize {
        self.text[..at]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self, at: usize) -> usize {
        self.text[at..]
            .chars()
            .next()
            .map_or(at, |c| at + c.len_utf8())
    }

    fn word_left(&self) -> usize {
        let t = &self.text[..self.caret];
        let t = t.trim_end_matches(|c: char| !c.is_alphanumeric());
        t.rfind(|c: char| !c.is_alphanumeric())
            .map_or(0, |i| i + t[i..].chars().next().map_or(1, char::len_utf8))
    }

    pub fn backspace(&mut self) {
        let p = self.prev(self.caret);
        self.text.replace_range(p..self.caret, "");
        self.caret = p;
    }

    pub fn delete(&mut self) {
        let n = self.next(self.caret);
        self.text.replace_range(self.caret..n, "");
    }

    pub fn delete_word_left(&mut self) {
        let p = self.word_left();
        self.text.replace_range(p..self.caret, "");
        self.caret = p;
    }

    pub fn left(&mut self) {
        self.caret = self.prev(self.caret);
    }

    pub fn right(&mut self) {
        self.caret = self.next(self.caret);
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.text.len();
    }

    /// The text with a caret, or the placeholder when empty.
    pub fn render(
        &self,
        placeholder: &str,
        caret: bool,
        text: Hsla,
        muted: Hsla,
    ) -> impl IntoElement + use<> {
        let bar = div().w(px(1.5)).h(px(16.0)).bg(text);
        let row = div().flex().flex_row().items_center().h(px(20.0)).min_w_0();
        if self.text.is_empty() {
            return row
                .when_caret(caret, bar)
                .child(div().text_color(muted).child(placeholder.to_owned()));
        }
        row.child(
            div()
                .whitespace_nowrap()
                .child(self.text[..self.caret].to_owned()),
        )
        .when_caret(caret, bar)
        .child(
            div()
                .whitespace_nowrap()
                .child(self.text[self.caret..].to_owned()),
        )
    }
}

trait WhenCaret {
    fn when_caret(self, caret: bool, bar: gpui::Div) -> Self;
}

impl WhenCaret for gpui::Div {
    fn when_caret(self, caret: bool, bar: gpui::Div) -> Self {
        if caret { self.child(bar) } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_keys() {
        let mut f = Field::default();
        f.insert("héllo wörld");
        f.backspace();
        assert_eq!(f.text, "héllo wörl");
        f.left();
        f.left();
        f.insert("\nX");
        assert_eq!(f.text, "héllo wöXrl", "control characters dropped");
        f.home();
        f.right();
        f.right();
        f.delete();
        assert_eq!(f.text, "hélo wöXrl");
        f.end();
        f.delete_word_left();
        assert_eq!(f.text, "hélo ");
        f.delete_word_left();
        assert_eq!(f.text, "");
    }
}
