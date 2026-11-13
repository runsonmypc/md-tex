//! Literal smart-case search over the source.
//! A lowercase-only query matches case-insensitively; any uppercase makes it exact.
//! Matches are confined to the given block ranges, so none crosses a block boundary.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::OnceLock;

use memchr::memmem;
use ropey::Rope;

pub fn is_case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// All non-overlapping matches of `query` inside `blocks` (byte ranges into `rope`, in
/// document order), as byte ranges. Ranges past the end or off a character boundary (from
/// a model parsed before the latest edit) are clamped and snapped to whole characters.
pub fn find_in_blocks(rope: &Rope, query: &str, blocks: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let Some(matcher) = Matcher::new(query) else {
        return out;
    };
    let snap = |at: usize| rope.char_to_byte(rope.byte_to_char(at.min(rope.len_bytes())));
    for block in blocks {
        let (start, end) = (snap(block.start), snap(block.end));
        if start >= end {
            continue;
        }
        let slice = rope.byte_slice(start..end);
        let text: Cow<'_, str> = match slice.as_str() {
            Some(s) => Cow::Borrowed(s),
            None => Cow::Owned(slice.to_string()),
        };
        matcher.find(&text, start, &mut out);
    }
    out
}

/// All non-overlapping matches of `query` in `text`, as byte ranges.
pub fn find_in_str(text: &str, query: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if let Some(matcher) = Matcher::new(query) {
        matcher.find(text, 0, &mut out);
    }
    out
}

enum Matcher {
    /// The query's bytes; `fold_ascii` matches a lowercase ASCII query against
    /// ASCII-lowercased text.
    Bytes {
        // Boxed: on x86_64 the finder carries its vector searchers (about 290 bytes).
        finder: Box<memmem::Finder<'static>>,
        fold_ascii: bool,
    },
    /// A lowercase non-ASCII query, compared by lowercased characters. `leads` are the first
    /// UTF-8 bytes of every character whose lowercase form starts like the query's.
    Unicode { needle: Vec<char>, leads: Vec<u8> },
}

impl Matcher {
    fn new(query: &str) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        if is_case_sensitive(query) || query.is_ascii() {
            let fold_ascii = !is_case_sensitive(query);
            return Some(Self::Bytes {
                finder: Box::new(memmem::Finder::new(query.as_bytes()).into_owned()),
                fold_ascii,
            });
        }
        let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        let first = needle[0];
        let mut leads: Vec<u8> = std::iter::once(first)
            .chain(
                lowercase_changes()
                    .iter()
                    .filter(|&&(lower, _)| lower == first)
                    .map(|&(_, c)| c),
            )
            .map(|c| c.encode_utf8(&mut [0; 4]).as_bytes()[0])
            .collect();
        leads.sort_unstable();
        leads.dedup();
        Some(Self::Unicode { needle, leads })
    }

    fn find(&self, text: &str, base: usize, out: &mut Vec<Range<usize>>) {
        match self {
            Self::Bytes { finder, fold_ascii } => {
                let len = finder.needle().len();
                // ASCII lowercasing keeps byte offsets, since it never touches UTF-8
                // continuation bytes.
                let folded;
                let haystack = if *fold_ascii {
                    folded = text.to_ascii_lowercase();
                    folded.as_bytes()
                } else {
                    text.as_bytes()
                };
                out.extend(
                    finder
                        .find_iter(haystack)
                        .map(|at| base + at..base + at + len),
                );
            }
            Self::Unicode { needle, leads } => {
                let bytes = text.as_bytes();
                let mut pos = 0;
                while let Some(at) = next_lead(&bytes[pos..], leads).map(|i| pos + i) {
                    // `at` holds a lead byte, so it is a character boundary.
                    match match_len(&text[at..], needle) {
                        Some(len) => {
                            out.push(base + at..base + at + len);
                            pos = at + len;
                        }
                        None => pos = at + 1,
                    }
                }
            }
        }
    }
}

/// Offset of the first of `leads` in `bytes`.
fn next_lead(bytes: &[u8], leads: &[u8]) -> Option<usize> {
    match *leads {
        [a] => memchr::memchr(a, bytes),
        [a, b] => memchr::memchr2(a, b, bytes),
        [a, b, c] => memchr::memchr3(a, b, c, bytes),
        _ => bytes.iter().position(|b| leads.contains(b)),
    }
}

/// Byte length of the match of `needle` (lowercase characters) at the start of `text`. A
/// match ends at a character boundary, never halfway through a character's lowercase form.
fn match_len(text: &str, needle: &[char]) -> Option<usize> {
    let mut want = needle.iter();
    for (offset, ch) in text.char_indices() {
        for lower in ch.to_lowercase() {
            if want.next() != Some(&lower) {
                return None;
            }
        }
        if want.len() == 0 {
            return Some(offset + ch.len_utf8());
        }
    }
    None
}

/// Every character that lowercasing changes, with the first character of its lowercase
/// form, so a search can find each character that folds to a given one. Characters above
/// plane 1 have no case.
fn lowercase_changes() -> &'static [(char, char)] {
    static TABLE: OnceLock<Vec<(char, char)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..0x20000)
            .filter_map(char::from_u32)
            .filter_map(|c| {
                let lower = c.to_lowercase().next()?;
                (lower != c).then_some((lower, c))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(text: &'a str, query: &str) -> Vec<&'a str> {
        let rope = Rope::from_str(text);
        find_in_blocks(&rope, query, std::slice::from_ref(&(0..text.len())))
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    #[test]
    fn lowercase_query_matches_any_case() {
        assert_eq!(find("GPU, Gpu and gpu.", "gpu"), ["GPU", "Gpu", "gpu"]);
    }

    #[test]
    fn uppercase_query_is_exact() {
        assert_eq!(find("GPU, Gpu and gpu.", "GPU"), ["GPU"]);
    }

    #[test]
    fn no_match_crosses_a_block_boundary() {
        let text = "end of one\n\nstart of two";
        let rope = Rope::from_str(text);
        let blocks = [0..10, 12..text.len()];
        assert!(find_in_blocks(&rope, "one\n\nstart", &blocks).is_empty());
        assert!(find_in_blocks(&rope, "onestart", &blocks).is_empty());
        let r = find_in_blocks(&rope, "of", &blocks);
        assert_eq!(r, vec![4..6, 18..20]);
    }

    #[test]
    fn unicode_case_insensitive() {
        assert_eq!(find("Ärger, ärger", "ärger"), ["Ärger", "ärger"]);
    }

    #[test]
    fn unicode_case_folding() {
        // İ lowercases to two characters, i and a combining dot above.
        assert_eq!(
            find("İstanbul, i\u{307}stanbul, istanbul", "i\u{307}stanbul"),
            ["İstanbul", "i\u{307}stanbul"]
        );
        // A match cannot end halfway through a character's lowercase form.
        assert!(find("éİ", "éi").is_empty());
        // The Kelvin sign lowercases to an ASCII k.
        assert_eq!(find("\u{212A}é and Ké", "ké"), ["\u{212A}é", "Ké"]);
        assert_eq!(find("CAFÉ café Café", "café"), ["CAFÉ", "café", "Café"]);
        assert_eq!(find("éé", "é"), ["é", "é"]);
    }

    #[test]
    fn ranges_off_char_boundaries_do_not_panic() {
        // A model parsed before an edit can hold ranges that now split a character.
        let text = "colour é colour";
        let rope = Rope::from_str(text);
        let blocks = [0..8, 8..text.len() + 5];
        assert_eq!(find_in_blocks(&rope, "colour", &blocks), vec![0..6, 10..16]);
    }

    #[test]
    fn works_across_rope_chunks() {
        let text = format!("{}needle{}", "x".repeat(5000), "y".repeat(5000));
        let rope = Rope::from_str(&text);
        assert_eq!(
            find_in_blocks(&rope, "needle", std::slice::from_ref(&(0..text.len()))),
            vec![5000..5006]
        );
    }

    #[test]
    fn plain_strings() {
        assert_eq!(find_in_str("a Fast fast", "fast"), vec![2..6, 7..11]);
        assert!(find_in_str("abc", "").is_empty());
    }

    #[test]
    fn empty_query_finds_nothing() {
        assert!(find("abc", "").is_empty());
    }
}
