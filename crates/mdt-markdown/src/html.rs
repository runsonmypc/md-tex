//! A small scanner for the two HTML tags the renderer honours: `<br>` and `<img>` with
//! `src`, `alt`, `width` and `height`.
//! Everything else stays literal text; nothing is executed or styled.

use crate::model::ObjectKind;

/// Recognizes a single `<br>` or `<img …>` tag (surrounding whitespace allowed).
pub fn recognize(text: &str) -> Option<ObjectKind> {
    let (tag, rest) = parse_tag(text.trim())?;
    rest.trim().is_empty().then_some(tag)
}

/// When `text` consists only of `<br>` / `<img>` tags and whitespace, returns each tag's
/// byte offset, length and object.
pub fn only_known_tags(text: &str) -> Option<Vec<(usize, usize, ObjectKind)>> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let skipped = text[pos..].len() - text[pos..].trim_start().len();
        pos += skipped;
        if pos == text.len() {
            return Some(out);
        }
        let (kind, rest) = parse_tag(&text[pos..])?;
        let len = text[pos..].len() - rest.len();
        out.push((pos, len, kind));
        pos += len;
    }
}

/// Parses one tag at the start of `s`; returns it and the remaining text.
fn parse_tag(s: &str) -> Option<(ObjectKind, &str)> {
    let body = s.strip_prefix('<')?;
    let end = body.find('>')?;
    let (inner, rest) = (&body[..end], &body[end + 1..]);
    let inner = inner.strip_suffix('/').unwrap_or(inner).trim_end();
    let name_len = inner
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(inner.len());
    let (name, attrs) = inner.split_at(name_len);
    let raw = s[..s.len() - rest.len()].to_owned();
    match name.to_ascii_lowercase().as_str() {
        "br" if attrs.trim().is_empty() => Some((ObjectKind::HtmlBreak { html: raw }, rest)),
        "img" => {
            let attrs = parse_attrs(attrs)?;
            let get = |k: &str| {
                attrs
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(k))
                    .map(|(_, v)| v.clone())
            };
            let px =
                |k: &str| get(k).and_then(|v| v.trim().trim_end_matches("px").parse::<u32>().ok());
            Some((
                ObjectKind::Image {
                    src: get("src")?,
                    alt: get("alt").unwrap_or_default(),
                    title: get("title").unwrap_or_default(),
                    width: px("width"),
                    height: px("height"),
                    html: Some(raw),
                },
                rest,
            ))
        }
        _ => None,
    }
}

/// `name="value"`, `name='value'`, `name=value` and bare `name`.
fn parse_attrs(mut s: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start();
        if s.is_empty() {
            return Some(out);
        }
        let name_len = s
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .unwrap_or(s.len());
        if name_len == 0 {
            return None;
        }
        let name = s[..name_len].to_owned();
        s = s[name_len..].trim_start();
        let value = if let Some(after) = s.strip_prefix('=') {
            let after = after.trim_start();
            let (value, rest) = match after.chars().next()? {
                q @ ('"' | '\'') => {
                    let close = after[1..].find(q)? + 1;
                    (&after[1..close], &after[close + 1..])
                }
                _ => {
                    let len = after
                        .find(|c: char| c.is_ascii_whitespace())
                        .unwrap_or(after.len());
                    (&after[..len], &after[len..])
                }
            };
            s = rest;
            value.to_owned()
        } else {
            String::new()
        };
        out.push((name, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn img_with_width() {
        let Some(ObjectKind::Image {
            src,
            width,
            height,
            alt,
            ..
        }) = recognize(r#"<img src="a.png" width="200">"#)
        else {
            panic!()
        };
        assert_eq!(
            (src.as_str(), width, height, alt.as_str()),
            ("a.png", Some(200), None, "")
        );
    }

    #[test]
    fn img_single_quotes_unquoted_and_self_closing() {
        let Some(ObjectKind::Image {
            src, alt, height, ..
        }) = recognize("<IMG src=b.jpg alt='a cat' height=50 />")
        else {
            panic!()
        };
        assert_eq!(
            (src.as_str(), alt.as_str(), height),
            ("b.jpg", "a cat", Some(50))
        );
    }

    #[test]
    fn br_variants() {
        for t in ["<br>", "<br/>", "<br />", "<BR>"] {
            assert!(
                matches!(recognize(t), Some(ObjectKind::HtmlBreak { .. })),
                "{t}"
            );
        }
    }

    #[test]
    fn other_html_is_not_recognized() {
        assert!(recognize(r#"<div style="color:red">"#).is_none());
        assert!(recognize("<img>").is_none(), "img needs src");
        assert!(recognize("<br> text").is_none());
        assert!(recognize("<script>alert(1)</script>").is_none());
    }

    #[test]
    fn block_of_known_tags() {
        let tags = only_known_tags("<img src=\"a.png\">\n<br>\n").unwrap();
        assert_eq!(
            tags.iter().map(|t| (t.0, t.1)).collect::<Vec<_>>(),
            [(0, 17), (18, 4)]
        );
        assert!(only_known_tags("<img src=a.png><div>").is_none());
    }
}
