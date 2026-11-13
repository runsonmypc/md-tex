//! Detecting a document's main line ending and indentation unit, and converting pasted text
//! (document-files "Byte-preserving round trip", source-editing "Indentation").

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }

    /// The most common line ending in `text`. Ties and text without line breaks give LF.
    pub fn detect(text: &str) -> Self {
        let bytes = text.as_bytes();
        let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
        let mut i = 0;
        while let Some(off) = memchr::memchr2(b'\n', b'\r', &bytes[i..]) {
            let at = i + off;
            if bytes[at] == b'\r' {
                if bytes.get(at + 1) == Some(&b'\n') {
                    crlf += 1;
                    i = at + 2;
                    continue;
                }
                cr += 1;
            } else {
                lf += 1;
            }
            i = at + 1;
        }
        if crlf > lf && crlf >= cr {
            Self::CrLf
        } else if cr > lf && cr > crlf {
            Self::Cr
        } else {
            Self::Lf
        }
    }

    /// Rewrites every line break in `text` (LF, CRLF or CR) as `self`.
    pub fn convert(self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + text.len() / 32);
        let mut rest = text;
        while let Some(at) = rest.find(['\r', '\n']) {
            out.push_str(&rest[..at]);
            out.push_str(self.as_str());
            let skip = if rest[at..].starts_with("\r\n") { 2 } else { 1 };
            rest = &rest[at + skip..];
        }
        out.push_str(rest);
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentUnit {
    Tab,
    Spaces(u8),
}

impl Default for IndentUnit {
    fn default() -> Self {
        Self::Spaces(4)
    }
}

impl IndentUnit {
    pub fn as_string(self) -> String {
        match self {
            Self::Tab => "\t".into(),
            Self::Spaces(n) => " ".repeat(n as usize),
        }
    }

    /// Detects the indentation unit from how indentation changes between consecutive
    /// non-blank lines. Falls back to four spaces when nothing is indented.
    pub fn detect(text: &str) -> Self {
        let (mut tab_lines, mut space_lines) = (0usize, 0usize);
        // Votes for space-indent steps of 1..=8.
        let mut steps = [0usize; 9];
        let mut prev_spaces = 0usize;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with('\t') {
                tab_lines += 1;
                continue;
            }
            let spaces = line.bytes().take_while(|&b| b == b' ').count();
            if spaces > 0 {
                space_lines += 1;
            }
            let delta = spaces.abs_diff(prev_spaces);
            if (1..=8).contains(&delta) {
                steps[delta] += 1;
            }
            prev_spaces = spaces;
        }
        if tab_lines > space_lines {
            return Self::Tab;
        }
        // Most frequent step; ties prefer the larger, so 2+4 mixtures with equal votes give 4.
        match (2..=8).chain([1]).max_by_key(|&s| (steps[s], s)) {
            Some(step) if steps[step] > 0 => Self::Spaces(step as u8),
            _ => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_crlf_and_converts_pasted_lf() {
        let doc = "a\r\nb\r\nc\n";
        assert_eq!(LineEnding::detect(doc), LineEnding::CrLf);
        assert_eq!(
            LineEnding::detect(doc).convert("one\ntwo\n"),
            "one\r\ntwo\r\n"
        );
    }

    #[test]
    fn convert_handles_mixed_input() {
        assert_eq!(LineEnding::Lf.convert("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn no_line_breaks_gives_lf() {
        assert_eq!(LineEnding::detect("one line"), LineEnding::Lf);
    }

    #[test]
    fn two_space_document_gives_two_space_unit() {
        let doc = "- a\n  - b\n    - c\n  - d\n- e\n";
        assert_eq!(IndentUnit::detect(doc), IndentUnit::Spaces(2));
        assert_eq!(IndentUnit::detect(doc).as_string(), "  ");
    }

    #[test]
    fn four_space_code_gives_four() {
        assert_eq!(
            IndentUnit::detect("fn a() {\n    b();\n    if c {\n        d();\n    }\n}\n"),
            IndentUnit::Spaces(4)
        );
    }

    #[test]
    fn tab_document_gives_tab() {
        assert_eq!(IndentUnit::detect("a\n\tb\n\t\tc\n"), IndentUnit::Tab);
    }

    #[test]
    fn undetectable_gives_four_spaces() {
        assert_eq!(
            IndentUnit::detect("# Title\n\nParagraph.\n"),
            IndentUnit::Spaces(4)
        );
        assert_eq!(IndentUnit::detect(""), IndentUnit::Spaces(4));
    }
}
