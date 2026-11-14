//! Where to cut the document for the synchronous first-frame parse.
//!
//! The prefix runs at least `min_bytes` and ends at a blank line where no fence, HTML block
//! or display-math block is open, and where the next line cannot continue a container
//! from before the blank line (no indentation, list marker or `>`). The top-level blocks
//! before the cut therefore parse the same as in the full document.

/// Byte length of the prefix to parse first. Returns `source.len()` when no safe boundary
/// exists after `min_bytes`.
pub fn safe_prefix_end(source: &str, min_bytes: usize) -> usize {
    if source.len() <= min_bytes {
        return source.len();
    }
    let mut state = State::default();
    let mut pos = 0;
    let mut prev_blank_at: Option<usize> = None;
    let mut first = true;
    for line in source.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let text = line.trim_end_matches(['\n', '\r']);
        let blank = is_blank(text);
        if first {
            first = false;
            if text == "---" || text == "+++" {
                state.front_matter = Some(if text == "---" { "---" } else { "+++" });
                continue;
            }
        }
        if let Some(blank_at) = prev_blank_at.take()
            && !may_be_blank(text)
            && state.is_outside()
            && blank_at >= min_bytes
            && starts_new_top_level_block(text)
        {
            return start;
        }
        state.feed(text, blank);
        if blank && state.is_outside() {
            prev_blank_at = Some(start);
        }
    }
    source.len()
}

/// Whether a line of `source` could open or close front matter: `---`, `+++` or `...`,
/// alone or after quote and list markers. pulldown-cmark reads front matter anywhere in a
/// document, not only at its start, and looks for the closing line all the way to the end.
pub(crate) fn has_metadata_delimiter(source: &str) -> bool {
    // A lone `\r` ends a line too.
    source.split(['\n', '\r']).any(|line| {
        let line = line.trim_end();
        ["---", "+++", "..."].iter().any(|d| {
            line.strip_suffix(d).is_some_and(|before| {
                before.chars().all(|c| {
                    matches!(c, ' ' | '\t' | '>' | '-' | '*' | '+' | '.' | ')')
                        || c.is_ascii_digit()
                })
            })
        })
    })
}

#[derive(Default)]
struct State {
    /// Open fence: its character and length.
    fence: Option<(u8, usize)>,
    /// Open HTML block that ends at a marker rather than a blank line.
    html_end: Option<&'static str>,
    /// Inside an HTML block that ends at the next blank line.
    html_until_blank: bool,
    display_math: bool,
    front_matter: Option<&'static str>,
}

impl State {
    fn is_outside(&self) -> bool {
        self.fence.is_none()
            && self.html_end.is_none()
            && !self.html_until_blank
            && !self.display_math
            && self.front_matter.is_none()
    }

    fn feed(&mut self, text: &str, blank: bool) {
        if let Some(close) = self.front_matter {
            if text == close || (close == "---" && text == "...") {
                self.front_matter = None;
            }
            return;
        }
        // Container markers (`>` and list indentation) do not change fence state.
        let body = strip_containers(text);
        if let Some((ch, len)) = self.fence {
            if let Some((c, l)) = fence_marker(body)
                && c == ch
                && l >= len
                && is_blank(body.trim_start_matches(ch as char))
            {
                self.fence = None;
            }
            return;
        }
        if let Some(end) = self.html_end {
            if text.to_ascii_lowercase().contains(end) {
                self.html_end = None;
            }
            return;
        }
        if self.html_until_blank {
            if blank {
                self.html_until_blank = false;
            }
            return;
        }
        if let Some(marker) = fence_marker(body) {
            self.fence = Some(marker);
            return;
        }
        if let Some(end) = html_block_start(body) {
            match end {
                // The block stays open unless its end marker is on the same line.
                HtmlEnd::Marker(m) if !body.to_ascii_lowercase()[1..].contains(m) => {
                    self.html_end = Some(m)
                }
                HtmlEnd::Marker(_) => {}
                HtmlEnd::Blank => self.html_until_blank = true,
            }
            return;
        }
        if body.matches("$$").count() % 2 == 1 {
            self.display_math = !self.display_math;
        }
        if blank {
            // A blank line ends a paragraph, and with it any unterminated `$$`.
            self.display_math = false;
        }
    }
}

/// Whether a line (without its line ending) is blank. Markdown whitespace is ASCII:
/// `str::trim` also strips Unicode spaces, so a line holding only a no-break space would
/// pass for blank. Spaces and tabs only, which can miss a blank line pulldown-cmark sees
/// (a vertical tab or form feed) but never takes text for one.
pub(crate) fn is_blank(text: &str) -> bool {
    text.bytes().all(|b| b == b' ' || b == b'\t')
}

/// Whether pulldown-cmark could read a line (without its line ending) as blank: it also
/// counts vertical tabs and form feeds as blank. A cut must start on a line that is not.
pub(crate) fn may_be_blank(text: &str) -> bool {
    text.bytes()
        .all(|b| matches!(b, b' ' | b'\t' | 0x0b | 0x0c))
}

/// Strips the spaces and tabs that indent a line, and no other whitespace.
fn trim_indent(s: &str) -> &str {
    s.trim_start_matches([' ', '\t'])
}

fn strip_containers(text: &str) -> &str {
    let mut s = trim_indent(text);
    while let Some(rest) = s.strip_prefix('>') {
        s = trim_indent(rest);
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            return trim_indent(rest);
        }
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits)
        && let Some(rest) = s[digits..]
            .strip_prefix(". ")
            .or_else(|| s[digits..].strip_prefix(") "))
    {
        return trim_indent(rest);
    }
    s
}

fn fence_marker(body: &str) -> Option<(u8, usize)> {
    let ch = *body.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = body.bytes().take_while(|&b| b == ch).count();
    (len >= 3).then_some((ch, len))
}

enum HtmlEnd {
    Marker(&'static str),
    Blank,
}

/// CommonMark HTML block start conditions 1–7 (simplified: any tag-like start that is not a
/// recognized end-marker type ends at a blank line).
fn html_block_start(body: &str) -> Option<HtmlEnd> {
    let lower = body.to_ascii_lowercase();
    if !lower.starts_with('<') {
        return None;
    }
    for (start, end) in [
        ("<script", "</script>"),
        ("<pre", "</pre>"),
        ("<style", "</style>"),
        ("<textarea", "</textarea>"),
    ] {
        if lower.starts_with(start)
            && (lower.len() == start.len() || lower[start.len()..].starts_with([' ', '>', '\t']))
        {
            return Some(HtmlEnd::Marker(end));
        }
    }
    for (start, end) in [("<!--", "-->"), ("<![cdata[", "]]>"), ("<?", "?>")] {
        if lower.starts_with(start) {
            return Some(HtmlEnd::Marker(end));
        }
    }
    if lower.starts_with("<!") && lower.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic) {
        return Some(HtmlEnd::Marker(">"));
    }
    let after = lower.trim_start_matches('<').trim_start_matches('/');
    after
        .starts_with(|c: char| c.is_ascii_alphabetic())
        .then_some(HtmlEnd::Blank)
}

/// A line that, after a blank line, cannot continue a list, blockquote or indented code.
pub(crate) fn starts_new_top_level_block(text: &str) -> bool {
    if text.starts_with([' ', '\t', '>']) {
        return false;
    }
    let s = text;
    if s.strip_prefix(['-', '*', '+'])
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
    {
        return false;
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits) && (s[digits..].starts_with(['.', ')'])) {
        return false;
    }
    // A footnote definition's continuation or a lazy paragraph line cannot follow a blank
    // line, so any other unindented line starts a new block.
    true
}
