//! Rope helpers for the editor: lines, grapheme and word boundaries, UTF-16 offsets and
//! indentation edits. Positions are byte offsets.

use std::ops::Range;

use ropey::Rope;
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// Byte range of line `i` without its line break.
pub fn line_range(rope: &Rope, i: usize) -> Range<usize> {
    let start = rope.line_to_byte(i);
    let line = rope.line(i);
    let mut len = line.len_bytes();
    // ropey breaks lines on CRLF, LF and the other Unicode line separators.
    let mut chars = line.chars_at(line.len_chars());
    match chars.prev() {
        Some('\n') => {
            len -= 1;
            if chars.prev() == Some('\r') {
                len -= 1;
            }
        }
        Some(c @ ('\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}')) => {
            len -= c.len_utf8()
        }
        _ => {}
    }
    start..start + len
}

pub fn line_text(rope: &Rope, i: usize) -> String {
    rope.byte_slice(line_range(rope, i)).to_string()
}

/// The line holding byte `b`.
pub fn line_of(rope: &Rope, b: usize) -> usize {
    rope.byte_to_line(b.min(rope.len_bytes()))
}

/// The next grapheme boundary after `b`; line breaks count as one step.
pub fn next_grapheme(rope: &Rope, b: usize) -> usize {
    let line = line_of(rope, b);
    let r = line_range(rope, line);
    if b >= r.end {
        return if line + 1 < rope.len_lines() {
            rope.line_to_byte(line + 1)
        } else {
            rope.len_bytes()
        };
    }
    let text = line_text(rope, line);
    let mut c = GraphemeCursor::new(b - r.start, text.len(), true);
    r.start
        + c.next_boundary(&text, 0)
            .ok()
            .flatten()
            .unwrap_or(text.len())
}

/// The previous grapheme boundary before `b`; line breaks count as one step.
pub fn prev_grapheme(rope: &Rope, b: usize) -> usize {
    let line = line_of(rope, b);
    let r = line_range(rope, line);
    if b <= r.start {
        return if line > 0 {
            line_range(rope, line - 1).end
        } else {
            0
        };
    }
    let text = line_text(rope, line);
    let at = (b - r.start).min(text.len());
    let mut c = GraphemeCursor::new(at, text.len(), true);
    r.start + c.prev_boundary(&text, 0).ok().flatten().unwrap_or(0)
}

fn is_word(s: &str) -> bool {
    s.chars().any(|c| c.is_alphanumeric() || c == '_')
}

/// End of the current or next word (macOS Option+Right, Linux Ctrl+Right). At a line end
/// it moves to the next line.
pub fn next_word(rope: &Rope, b: usize) -> usize {
    let line = line_of(rope, b);
    let r = line_range(rope, line);
    if b >= r.end {
        return next_grapheme(rope, b);
    }
    let text = line_text(rope, line);
    let at = b - r.start;
    text.split_word_bound_indices()
        .find(|(i, w)| i + w.len() > at && is_word(w))
        .map_or(r.end, |(i, w)| r.start + i + w.len())
}

/// Start of the current or previous word.
pub fn prev_word(rope: &Rope, b: usize) -> usize {
    let line = line_of(rope, b);
    let r = line_range(rope, line);
    if b <= r.start {
        return prev_grapheme(rope, b);
    }
    let text = line_text(rope, line);
    let at = b - r.start;
    text.split_word_bound_indices()
        .rfind(|(i, w)| *i < at && is_word(w))
        .map_or(r.start, |(i, _)| r.start + i)
}

/// The word (or run of other characters) around `b`, for double-click.
pub fn word_at(rope: &Rope, b: usize) -> Range<usize> {
    let line = line_of(rope, b);
    let r = line_range(rope, line);
    let text = line_text(rope, line);
    let at = b.saturating_sub(r.start).min(text.len());
    text.split_word_bound_indices()
        .find(|(i, w)| at >= *i && at < i + w.len())
        .or_else(|| text.split_word_bound_indices().next_back())
        .map_or(b..b, |(i, w)| r.start + i..r.start + i + w.len())
}

/// The whole line around `b` including its break, for triple-click.
pub fn line_with_break(rope: &Rope, b: usize) -> Range<usize> {
    let line = line_of(rope, b);
    let start = rope.line_to_byte(line);
    let end = if line + 1 < rope.len_lines() {
        rope.line_to_byte(line + 1)
    } else {
        rope.len_bytes()
    };
    start..end
}

pub fn byte_to_utf16(rope: &Rope, b: usize) -> usize {
    rope.char_to_utf16_cu(rope.byte_to_char(b.min(rope.len_bytes())))
}

pub fn utf16_to_byte(rope: &Rope, u: usize) -> usize {
    let u = u.min(rope.len_utf16_cu());
    rope.char_to_byte(rope.utf16_cu_to_char(u))
}

/// Edits that add one indentation unit to the start of each line in `lines`. Ranges are
/// in application order (last line first, so earlier offsets stay valid).
pub fn indent_edits(rope: &Rope, lines: Range<usize>, unit: &str) -> Vec<(Range<usize>, String)> {
    lines
        .rev()
        .map(|l| {
            let s = rope.line_to_byte(l);
            (s..s, unit.to_owned())
        })
        .collect()
}

/// Edits that remove up to one indentation unit from each line in `lines`: a tab, or up
/// to the unit's width in leading spaces.
pub fn outdent_edits(rope: &Rope, lines: Range<usize>, unit: &str) -> Vec<(Range<usize>, String)> {
    let width = unit.len().max(1);
    lines
        .rev()
        .filter_map(|l| {
            let s = rope.line_to_byte(l);
            let text = line_text(rope, l);
            let n = if text.starts_with('\t') {
                1
            } else {
                text.bytes().take(width).take_while(|&c| c == b' ').count()
            };
            (n > 0).then(|| (s..s + n, String::new()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_emoji_is_one_step() {
        let family = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
        let rope = Rope::from_str(&format!("a{family}b"));
        assert_eq!(next_grapheme(&rope, 1), 1 + family.len());
        assert_eq!(prev_grapheme(&rope, 1 + family.len()), 1);
    }

    #[test]
    fn line_breaks_are_one_step() {
        let rope = Rope::from_str("ab\r\ncd\nef");
        assert_eq!(line_range(&rope, 0), 0..2);
        assert_eq!(next_grapheme(&rope, 2), 4, "CRLF is skipped in one step");
        assert_eq!(prev_grapheme(&rope, 4), 2);
        assert_eq!(line_range(&rope, 2), 7..9);
    }

    #[test]
    fn word_movement() {
        let rope = Rope::from_str("hello, big world");
        assert_eq!(next_word(&rope, 0), 5, "end of the current word");
        assert_eq!(
            next_word(&rope, 5),
            10,
            "punctuation is skipped to the next word end"
        );
        assert_eq!(next_word(&rope, 12), 16);
        assert_eq!(prev_word(&rope, 16), 11);
        assert_eq!(prev_word(&rope, 11), 7);
        assert_eq!(prev_word(&rope, 3), 0);
        assert_eq!(word_at(&rope, 8), 7..10);
    }

    #[test]
    fn utf16_mapping() {
        let rope = Rope::from_str("aé😀b");
        // a=1 byte/1 unit, é=2 bytes/1 unit, 😀=4 bytes/2 units.
        assert_eq!(byte_to_utf16(&rope, 0), 0);
        assert_eq!(byte_to_utf16(&rope, 3), 2);
        assert_eq!(byte_to_utf16(&rope, 7), 4);
        assert_eq!(utf16_to_byte(&rope, 4), 7);
        assert_eq!(utf16_to_byte(&rope, 5), 8);
        assert_eq!(utf16_to_byte(&rope, 99), 8, "clamped to the end");
    }

    #[test]
    fn indent_three_lines_by_two_spaces() {
        let mut rope = Rope::from_str("  a\n  b\n  c\nd\n");
        for (r, t) in indent_edits(&rope, 0..3, "  ") {
            let s = rope.byte_to_char(r.start);
            rope.insert(s, &t);
        }
        assert_eq!(rope.to_string(), "    a\n    b\n    c\nd\n");
        for (r, _) in outdent_edits(&rope, 0..4, "  ") {
            rope.remove(rope.byte_to_char(r.start)..rope.byte_to_char(r.end));
        }
        assert_eq!(rope.to_string(), "  a\n  b\n  c\nd\n");
    }
}
