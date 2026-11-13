//! CommonMark 0.31.2 conformance. A test-only
//! serializer turns the block model into HTML, which is compared with each example's
//! reference HTML after normalization.

use std::fmt::Write as _;

use mdt_markdown::*;
use pulldown_cmark_escape::{FmtWriter, escape_href, escape_html};

fn esc(s: &str) -> String {
    let mut out = String::new();
    escape_html(FmtWriter(&mut out), s).unwrap();
    out
}

fn href(s: &str) -> String {
    let mut out = String::new();
    escape_href(FmtWriter(&mut out), s).unwrap();
    out
}

fn blocks_html(blocks: &[Block], out: &mut String) {
    for b in blocks {
        block_html(b, out);
    }
}

fn block_html(block: &Block, out: &mut String) {
    match &block.kind {
        BlockKind::Heading { level, content, .. } => {
            writeln!(out, "<h{level}>{}</h{level}>", inline_html(content)).unwrap()
        }
        BlockKind::Paragraph {
            content,
            implicit: true,
        } => out.push_str(&inline_html(content)),
        BlockKind::Paragraph {
            content,
            implicit: false,
        } => writeln!(out, "<p>{}</p>", inline_html(content)).unwrap(),
        BlockKind::Code { lang, text, .. } => {
            match lang {
                Some(l) => write!(out, "<pre><code class=\"language-{}\">", esc(l)).unwrap(),
                None => out.push_str("<pre><code>"),
            }
            writeln!(out, "{}</code></pre>", esc(text)).unwrap();
        }
        BlockKind::Math { latex } => write!(
            out,
            "<pre><code class=\"language-math\">{}\n</code></pre>\n",
            esc(latex)
        )
        .unwrap(),
        BlockKind::List { start, items, .. } => {
            let tag = match start {
                Some(1) => {
                    out.push_str("<ol>\n");
                    "ol"
                }
                Some(n) => {
                    writeln!(out, "<ol start=\"{n}\">").unwrap();
                    "ol"
                }
                None => {
                    out.push_str("<ul>\n");
                    "ul"
                }
            };
            for item in items {
                out.push_str("<li>");
                blocks_html(&item.children, out);
                out.push_str("</li>\n");
            }
            writeln!(out, "</{tag}>").unwrap();
        }
        BlockKind::BlockQuote { children, .. } => {
            out.push_str("<blockquote>\n");
            blocks_html(children, out);
            out.push_str("</blockquote>\n");
        }
        BlockKind::Table { .. } | BlockKind::FrontMatter { .. } | BlockKind::Footnotes { .. } => {}
        BlockKind::Rule => out.push_str("<hr />\n"),
        BlockKind::Html { text } => out.push_str(text),
    }
}

fn inline_html(inline: &Inline) -> String {
    // Boundaries where spans open or close.
    let mut cuts: Vec<usize> = inline
        .spans
        .iter()
        .flat_map(|s| [s.range.start, s.range.end])
        .collect();
    cuts.extend(
        inline
            .objects
            .iter()
            .flat_map(|o| [o.at, o.at + OBJECT_CHAR.len_utf8()]),
    );
    cuts.extend([0, inline.text.len()]);
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = String::new();
    let mut open: Vec<&Span> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        while let Some(s) = open.last()
            && s.range.end <= a
        {
            out.push_str(close_tag(&s.style));
            open.pop();
        }
        for s in inline
            .spans
            .iter()
            .filter(|s| s.range.is_empty() && s.range.start == a)
        {
            out.push_str(&open_tag(&s.style));
            out.push_str(close_tag(&s.style));
        }
        for s in inline
            .spans
            .iter()
            .filter(|s| s.range.start == a && !s.range.is_empty())
        {
            out.push_str(&open_tag(&s.style));
            open.push(s);
        }
        let text = &inline.text[a..b];
        let atomic = open.iter().rev().find(|s| {
            matches!(
                s.style,
                SpanStyle::Html | SpanStyle::SoftBreak | SpanStyle::HardBreak
            )
        });
        if let Some(obj) = inline.objects.iter().find(|o| o.at == a) {
            out.push_str(&object_html(&obj.kind));
        } else {
            match atomic.map(|s| &s.style) {
                Some(SpanStyle::Html) => out.push_str(text),
                Some(SpanStyle::SoftBreak) => out.push('\n'),
                Some(SpanStyle::HardBreak) => out.push_str("<br />\n"),
                _ => out.push_str(&esc(text)),
            }
        }
    }
    while let Some(s) = open.pop() {
        out.push_str(close_tag(&s.style));
    }
    let end = inline.text.len();
    for s in inline
        .spans
        .iter()
        .filter(|s| s.range.is_empty() && s.range.start == end)
    {
        out.push_str(&open_tag(&s.style));
        out.push_str(close_tag(&s.style));
    }
    out
}

fn open_tag(style: &SpanStyle) -> String {
    match style {
        SpanStyle::Emphasis => "<em>".into(),
        SpanStyle::Strong => "<strong>".into(),
        SpanStyle::Strikethrough => "<del>".into(),
        SpanStyle::Code => "<code>".into(),
        SpanStyle::Link { dest, title } if title.is_empty() => {
            format!("<a href=\"{}\">", href(dest))
        }
        SpanStyle::Link { dest, title } => {
            format!("<a href=\"{}\" title=\"{}\">", href(dest), esc(title))
        }
        SpanStyle::FootnoteRef { .. } => "<sup>".into(),
        SpanStyle::Html | SpanStyle::SoftBreak | SpanStyle::HardBreak => String::new(),
    }
}

fn close_tag(style: &SpanStyle) -> &'static str {
    match style {
        SpanStyle::Emphasis => "</em>",
        SpanStyle::Strong => "</strong>",
        SpanStyle::Strikethrough => "</del>",
        SpanStyle::Code => "</code>",
        SpanStyle::Link { .. } => "</a>",
        SpanStyle::FootnoteRef { .. } => "</sup>",
        SpanStyle::Html | SpanStyle::SoftBreak | SpanStyle::HardBreak => "",
    }
}

fn object_html(kind: &ObjectKind) -> String {
    match kind {
        ObjectKind::Math {
            latex,
            display: false,
        } => format!("${}$", esc(latex)),
        ObjectKind::Math {
            latex,
            display: true,
        } => format!("$${}$$", esc(latex)),
        ObjectKind::Image {
            html: Some(raw), ..
        }
        | ObjectKind::HtmlBreak { html: raw } => raw.clone(),
        ObjectKind::Image {
            src,
            alt,
            title,
            html: None,
            ..
        } => {
            let title = if title.is_empty() {
                String::new()
            } else {
                format!(" title=\"{}\"", esc(title))
            };
            format!("<img src=\"{}\" alt=\"{}\"{title} />", href(src), esc(alt))
        }
    }
}

pub fn to_html(model: &BlockModel) -> String {
    let mut out = String::new();
    for unit in &model.units {
        block_html(&unit.block, &mut out);
    }
    out
}

// Normalization, modelled on the CommonMark test suite's normalize.py: whitespace runs
// collapse outside <pre>, whitespace next to block-level tags is dropped, tag names are
// lowercased, and character references are decoded.

const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "legend",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "ul",
];

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                _ if name.starts_with("#x") || name.starts_with("#X") => {
                    char::from_u32(u32::from_str_radix(&name[2..], 16).ok()?)?
                }
                _ if name.starts_with('#') => char::from_u32(name[1..].parse().ok()?)?,
                _ => return None,
            };
            Some((c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Debug, PartialEq)]
enum Tok {
    Tag { name: String, raw: String },
    Text(String),
}

fn tokenize(html: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let is_tag = rest.starts_with('<')
            && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '/');
        match rest.find('>').filter(|_| is_tag) {
            Some(end) => {
                let raw = &rest[..=end];
                let name: String = raw[1..]
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                let closing = raw.starts_with("</");
                let raw_norm = format!(
                    "<{}{}{}",
                    if closing { "/" } else { "" },
                    name,
                    &raw[1 + usize::from(closing) + name.len()..]
                );
                toks.push(Tok::Tag {
                    name,
                    raw: decode_entities(&raw_norm),
                });
                rest = &rest[end + 1..];
            }
            None => {
                let first = rest.chars().next().unwrap().len_utf8();
                let next = rest[first..].find('<').map_or(rest.len(), |i| i + first);
                toks.push(Tok::Text(rest[..next].to_owned()));
                rest = &rest[next..];
            }
        }
    }
    toks
}

fn normalize(html: &str) -> String {
    let toks = tokenize(html);
    let mut out = String::new();
    let mut in_pre = false;
    for (i, tok) in toks.iter().enumerate() {
        match tok {
            Tok::Tag { name, raw } => {
                if name == "pre" {
                    in_pre = !raw.starts_with("</");
                }
                out.push_str(raw);
            }
            Tok::Text(text) => {
                let text = decode_entities(text);
                if in_pre {
                    out.push_str(&text);
                    continue;
                }
                let mut t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let is_block = |t: Option<&Tok>| matches!(t, Some(Tok::Tag { name, .. }) if BLOCK_TAGS.contains(&name.as_str()));
                let lead = text.starts_with(char::is_whitespace)
                    && !(i == 0 || is_block(toks.get(i.wrapping_sub(1))));
                let trail = text.ends_with(char::is_whitespace)
                    && !(i + 1 == toks.len() || is_block(toks.get(i + 1)));
                if t.is_empty() {
                    if lead || trail {
                        out.push(' ');
                    }
                    continue;
                }
                if lead {
                    t.insert(0, ' ');
                }
                if trail {
                    t.push(' ');
                }
                out.push_str(&t);
            }
        }
    }
    out
}

/// The parser's options minus front matter. YAML front matter is the one extension that
/// changes CommonMark's own results: a document starting with `---` opens a metadata
/// block instead of a thematic break (example 96). Front matter has its own tests.
fn conformance_options() -> pulldown_cmark::Options {
    options()
        - pulldown_cmark::Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        - pulldown_cmark::Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
}

#[test]
fn every_other_d4_option_is_on_in_conformance() {
    let full = options();
    let diff = full - conformance_options();
    assert_eq!(
        diff,
        pulldown_cmark::Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
            | pulldown_cmark::Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
    );
}

#[derive(serde::Deserialize)]
struct Example {
    markdown: String,
    html: String,
    example: u32,
    section: String,
}

#[test]
fn commonmark_0_31_2() {
    let examples: Vec<Example> =
        serde_json::from_str(include_str!("data/commonmark-0.31.2.json")).unwrap();
    let mut failures = Vec::new();
    for ex in &examples {
        let got = to_html(&parse_with(&ex.markdown, conformance_options()));
        if normalize(&got) != normalize(&ex.html) {
            failures.push(format!(
                "example {} ({}):\n  markdown: {:?}\n  expected: {:?}\n  got:      {:?}",
                ex.example,
                ex.section,
                ex.markdown,
                normalize(&ex.html),
                normalize(&got)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} examples fail:\n{}",
        failures.len(),
        examples.len(),
        failures.join("\n")
    );
}

#[test]
fn normalizer_sanity() {
    assert_eq!(
        normalize("<p>a\n  b</p>\n<ul>\n<li>x</li>\n</ul>\n"),
        "<p>a b</p><ul><li>x</li></ul>"
    );
    assert_eq!(
        normalize("<pre><code>a\n  b\n</code></pre>"),
        "<pre><code>a\n  b\n</code></pre>"
    );
    assert_eq!(normalize("<p>&amp; &#35; &quot;</p>"), "<p>& # \"</p>");
}
