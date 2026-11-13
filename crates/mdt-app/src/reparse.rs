//! What an incremental reparse needs from the document: the span that changed since the
//! last parse, and line access to both versions of the text.

use std::borrow::Cow;
use std::ops::Range;

use mdt_markdown::incremental::{Change, Text};
use ropey::Rope;

/// A rope as the incremental parser's text.
pub struct RopeText<'a>(pub &'a Rope);

impl Text for RopeText<'_> {
    fn len_bytes(&self) -> usize {
        self.0.len_bytes()
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        self.0.byte_slice(range).into()
    }

    fn line(&self, at: usize) -> Range<usize> {
        // Lines end at `\n` only, as the parser's line rules expect: ropey also breaks at
        // `\r` and Unicode separators.
        let mut back = self.0.bytes_at(at);
        let mut start = at;
        while back.prev().is_some_and(|b| b != b'\n') {
            start -= 1;
        }
        let mut end = at;
        for b in self.0.bytes_at(at) {
            end += 1;
            if b == b'\n' {
                break;
            }
        }
        start..end
    }
}

/// The span that differs between `old` and `new`, or `None` when they are equal.
pub fn changed_range(old: &Rope, new: &Rope) -> Option<Change> {
    let prefix = common_prefix(old, new);
    if prefix == old.len_bytes() && prefix == new.len_bytes() {
        return None;
    }
    let max_suffix = old.len_bytes().min(new.len_bytes()) - prefix;
    let suffix = common_suffix(old, new, max_suffix);
    Some(Change {
        start: prefix,
        old_end: old.len_bytes() - suffix,
        new_end: new.len_bytes() - suffix,
    })
}

/// Bytes the two ropes share at the start. Chunks an edit did not touch are shared between
/// the ropes, so they compare by address.
fn common_prefix(a: &Rope, b: &Rope) -> usize {
    let (mut ca, mut cb) = (a.chunks(), b.chunks());
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut n = 0;
    loop {
        if x.is_empty() {
            match ca.next() {
                Some(c) => x = c.as_bytes(),
                None => return n,
            }
        }
        if y.is_empty() {
            match cb.next() {
                Some(c) => y = c.as_bytes(),
                None => return n,
            }
        }
        let k = x.len().min(y.len());
        if !std::ptr::eq(x.as_ptr(), y.as_ptr()) && x[..k] != y[..k] {
            return n + x.iter().zip(y).take_while(|(p, q)| p == q).count();
        }
        n += k;
        (x, y) = (&x[k..], &y[k..]);
    }
}

/// Bytes the two ropes share at the end, at most `max`.
fn common_suffix(a: &Rope, b: &Rope, max: usize) -> usize {
    let mut ca = a.chunks_at_byte(a.len_bytes()).0.reversed();
    let mut cb = b.chunks_at_byte(b.len_bytes()).0.reversed();
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut n = 0;
    while n < max {
        if x.is_empty() {
            match ca.next() {
                Some(c) => x = c.as_bytes(),
                None => break,
            }
        }
        if y.is_empty() {
            match cb.next() {
                Some(c) => y = c.as_bytes(),
                None => break,
            }
        }
        let k = x.len().min(y.len());
        let (tx, ty) = (&x[x.len() - k..], &y[y.len() - k..]);
        if !std::ptr::eq(tx.as_ptr(), ty.as_ptr()) && tx != ty {
            n += tx
                .iter()
                .rev()
                .zip(ty.iter().rev())
                .take_while(|(p, q)| p == q)
                .count();
            break;
        }
        n += k;
        (x, y) = (&x[..x.len() - k], &y[..y.len() - k]);
    }
    n.min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(old: &str, new: &str) -> Option<(usize, usize, usize)> {
        changed_range(&Rope::from_str(old), &Rope::from_str(new))
            .map(|c| (c.start, c.old_end, c.new_end))
    }

    #[test]
    fn finds_the_changed_span() {
        assert_eq!(change("abc", "abc"), None);
        assert_eq!(change("abc", "abXc"), Some((2, 2, 3)));
        assert_eq!(change("abc", "ac"), Some((1, 2, 1)));
        // The shared prefix and suffix may not overlap: "aa" to "aaa" inserts at the end.
        assert_eq!(change("aa", "aaa"), Some((2, 2, 3)));
        assert_eq!(change("", "x"), Some((0, 0, 1)));
    }

    #[test]
    fn compares_across_chunk_boundaries() {
        let text = "line of text\n".repeat(20_000);
        let old = Rope::from_str(&text);
        let mut new = old.clone();
        let at = new.byte_to_char(150_000);
        new.insert(at, "inserted");
        let c = changed_range(&old, &new).unwrap();
        assert_eq!((c.start, c.old_end, c.new_end), (150_000, 150_000, 150_008));
        // A rope built separately has different chunks.
        let mut other = text.clone();
        other.replace_range(150_000..150_004, "");
        let c = changed_range(&old, &Rope::from_str(&other)).unwrap();
        assert_eq!((c.start, c.old_end, c.new_end), (150_000, 150_004, 150_000));
    }

    #[test]
    fn lines_end_at_newlines_only() {
        let rope = Rope::from_str("ab\ncd\r\ne\u{2028}f\ng");
        let t = RopeText(&rope);
        assert_eq!(t.line(0), 0..3);
        assert_eq!(t.line(2), 0..3);
        assert_eq!(t.line(4), 3..7);
        // `e`, U+2028 (three bytes), `f`, `\n`.
        assert_eq!(t.line(8), 7..13);
        assert_eq!(t.line(13), 13..14);
    }
}
