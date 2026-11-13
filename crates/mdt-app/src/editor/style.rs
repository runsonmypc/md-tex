//! Source colouring: a per-line scan for Markdown syntax, so typed text is coloured in the
//! same frame. Fenced code, math, HTML and front matter come from the block model (they
//! span lines), so they follow the next parse.

use mdt_highlight::Class;
use mdt_text::{Brush, StyleRun};

use crate::reader::block::brush;

pub const MATH: Brush = Brush(brush::SYNTAX_BASE + Class::Number as u16);
pub const MARKER: Brush = brush::MUTED;

fn run(range: std::ops::Range<usize>, brush: Brush) -> StyleRun {
    StyleRun {
        range,
        brush: Some(brush),
        ..Default::default()
    }
}

/// Style runs for one source line. `in_code` marks lines inside a code-like block.
pub fn line_runs(line: &str, in_code: bool) -> Vec<StyleRun> {
    if line.is_empty() {
        return Vec::new();
    }
    if in_code {
        return vec![run(0..line.len(), brush::CODE)];
    }
    let b = line.as_bytes();
    let indent = b.iter().take_while(|&&c| c == b' ').count();
    let hashes = b[indent.min(b.len())..]
        .iter()
        .take_while(|&&c| c == b'#')
        .count();
    if indent < 4
        && (1..=6).contains(&hashes)
        && matches!(b.get(indent + hashes), None | Some(b' ' | b'\t'))
    {
        return vec![
            StyleRun {
                range: 0..line.len(),
                bold: true,
                ..Default::default()
            },
            run(indent..indent + hashes, MARKER),
        ];
    }
    let mut runs = Vec::new();
    // Block markers: quotes, then a list marker.
    let mut i = indent;
    while i < b.len() && (b[i] == b'>' || (b[i] == b' ' && i > indent)) {
        if b[i] == b'>' {
            runs.push(run(i..i + 1, MARKER));
        }
        i += 1;
    }
    let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let marker = match b.get(i) {
        Some(b'-' | b'*' | b'+') => 1,
        _ if (1..=9).contains(&digits) && matches!(b.get(i + digits), Some(b'.' | b')')) => {
            digits + 1
        }
        _ => 0,
    };
    if marker > 0 && matches!(b.get(i + marker), None | Some(b' ' | b'\t')) {
        let mut end = i + marker;
        // A task box belongs to the marker.
        if let Some(rest) = line.get(end..)
            && (rest.starts_with(" [ ]") || rest.starts_with(" [x]") || rest.starts_with(" [X]"))
        {
            end += 4;
        }
        runs.push(run(i..end, MARKER));
        i = end;
    }
    // Searches that `i` only moves past. A long line of `](` without `)`, or of ever
    // shorter backtick runs, searched the rest of the line at each one: quadratic.
    let mut paren = Lookahead::default();
    let mut ticks: Option<Vec<(usize, usize)>> = None;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'`' => {
                let n = b[i..].iter().take_while(|&&c| c == b'`').count();
                let fence = &line[i..i + n];
                let longest = ticks.get_or_insert_with(|| longest_tick_runs(b));
                let after = longest.partition_point(|&(start, _)| start < i + n);
                let close = longest
                    .get(after)
                    .filter(|&&(_, len)| len >= n)
                    .and_then(|_| line[i + n..].find(fence));
                match close {
                    Some(close) => {
                        let end = i + n + close + n;
                        runs.push(StyleRun {
                            mono: true,
                            ..run(i..end, brush::CODE)
                        });
                        i = end;
                    }
                    None => i += n,
                }
            }
            b'$' => {
                let n = if b.get(i + 1) == Some(&b'$') { 2 } else { 1 };
                let delim = &line[i..i + n];
                match line[i + n..].find(delim) {
                    Some(close) if close > 0 => {
                        let end = i + n + close + n;
                        runs.push(run(i..end, MATH));
                        i = end;
                    }
                    _ => i += n,
                }
            }
            b'[' | b']' | b'!' if b[i] != b'!' || b.get(i + 1) == Some(&b'[') => {
                runs.push(run(i..i + 1, MARKER));
                if b[i] == b']'
                    && b.get(i + 1) == Some(&b'(')
                    && let Some(close) = paren.next(b, i + 1, b')')
                {
                    let end = close + 1;
                    runs.push(run(i + 1..end, brush::LINK));
                    i = end;
                } else {
                    i += 1;
                }
            }
            c @ (b'*' | b'_' | b'~') => {
                let n = b[i..].iter().take_while(|&&x| x == c).count();
                let before = i.checked_sub(1).map(|j| b[j]);
                let after = b.get(i + n).copied();
                let word = |x: Option<u8>| x.is_some_and(|x| x.is_ascii_alphanumeric());
                let space = |x: Option<u8>| x.is_none_or(|x| x.is_ascii_whitespace());
                // Delimiters touch text on one side; `snake_case` underscores do not count.
                let intraword = c == b'_' && word(before) && word(after);
                if !intraword && (c != b'~' || n >= 2) && !(space(before) && space(after)) {
                    runs.push(run(i..i + n, MARKER));
                }
                i += n;
            }
            _ => i += 1,
        }
    }
    runs
}

/// The first of a byte at or after a position that only grows, found once per occurrence.
#[derive(Default)]
struct Lookahead {
    /// The last search: from where, and what it found.
    last: Option<(usize, Option<usize>)>,
}

impl Lookahead {
    fn next(&mut self, b: &[u8], from: usize, byte: u8) -> Option<usize> {
        match self.last {
            Some((at, found)) if at <= from && found.is_none_or(|f| f >= from) => found,
            _ => {
                let found = b[from..].iter().position(|&c| c == byte).map(|k| from + k);
                self.last = Some((from, found));
                found
            }
        }
    }
}

/// Each backtick run's start, with the longest run starting there or later.
fn longest_tick_runs(b: &[u8]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while let Some(k) = b[i..].iter().position(|&c| c == b'`') {
        let start = i + k;
        let n = b[start..].iter().take_while(|&&c| c == b'`').count();
        runs.push((start, n));
        i = start + n;
    }
    let mut longest = 0;
    for run in runs.iter_mut().rev() {
        longest = longest.max(run.1);
        run.1 = longest;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brushes(line: &str) -> Vec<(&str, Brush)> {
        line_runs(line, false)
            .into_iter()
            .filter_map(|r| Some((&line[r.range], r.brush?)))
            .collect()
    }

    #[test]
    fn heading_marker_colours_the_line_at_once() {
        let runs = line_runs("## Results", false);
        assert!(runs[0].bold && runs[0].range == (0..10), "the line is bold");
        assert_eq!((runs[1].range.clone(), runs[1].brush), (0..2, Some(MARKER)));
        assert_eq!(line_runs("## ", false).len(), 2, "styled as soon as typed");
        assert!(line_runs("#hashtag", false).is_empty());
    }

    #[test]
    fn inline_syntax() {
        assert_eq!(
            brushes("- [x] a **b** `c` $x^2$ [l](u) snake_case"),
            [
                ("- [x]", MARKER),
                ("**", MARKER),
                ("**", MARKER),
                ("`c`", brush::CODE),
                ("$x^2$", MATH),
                ("[", MARKER),
                ("]", MARKER),
                ("(u)", brush::LINK),
            ]
        );
        assert_eq!(brushes("> 1. quote"), [(">", MARKER), ("1.", MARKER)]);
    }

    /// Unclosed `](` and ever shorter backtick runs searched the rest of the line each
    /// time: 150,000 `](` (300 KB) took 0.6 s to colour.
    #[test]
    fn long_lines_colour_in_linear_time() {
        let parens = "](".repeat(150_000);
        let ticks: String = (1..1400).rev().map(|k| "`".repeat(k) + "a").collect();
        let start = std::time::Instant::now();
        let runs = line_runs(&parens, false);
        assert_eq!(runs.len(), 150_000, "a marker each, no links");
        assert!(line_runs(&ticks, false).is_empty());
        // 3.2 s before, in a debug build; 0.05 s after.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
        // Closed ones still colour.
        assert_eq!(
            brushes("``a`` `b` ](x) ](y)"),
            [
                ("``a``", brush::CODE),
                ("`b`", brush::CODE),
                ("]", MARKER),
                ("(x)", brush::LINK),
                ("]", MARKER),
                ("(y)", brush::LINK),
            ]
        );
    }

    #[test]
    fn code_lines_are_code() {
        let runs = line_runs("# not a heading", true);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].brush, Some(brush::CODE));
    }
}
