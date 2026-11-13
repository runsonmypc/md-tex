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
        let blank = text.trim().is_empty();
        if first {
            first = false;
            if text == "---" || text == "+++" {
                state.front_matter = Some(if text == "---" { "---" } else { "+++" });
                continue;
            }
        }
        if let Some(blank_at) = prev_blank_at.take()
            && !blank
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
                && body.trim_start_matches(ch as char).trim().is_empty()
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

fn strip_containers(text: &str) -> &str {
    let mut s = text.trim_start();
    while let Some(rest) = s.strip_prefix('>') {
        s = rest.trim_start();
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            return rest.trim_start();
        }
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits)
        && let Some(rest) = s[digits..]
            .strip_prefix(". ")
            .or_else(|| s[digits..].strip_prefix(") "))
    {
        return rest.trim_start();
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
fn starts_new_top_level_block(text: &str) -> bool {
    if text.starts_with([' ', '\t', '>']) {
        return false;
    }
    let s = text;
    if ["- ", "* ", "+ "].iter().any(|m| s.starts_with(m)) || matches!(s, "-" | "*" | "+") {
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
