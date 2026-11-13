//! Following links: where a destination points, the link under a
//! source offset, and where footnotes and their references are.

use std::path::{Path, PathBuf};

use mdt_markdown::{Block, BlockKind, BlockModel, Inline, SpanStyle, UnitKind};

use crate::reader::block::{Link, table_rows_shown};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `#fragment` in this document.
    Anchor(String),
    /// `http`, `https` and `mailto` to a plain address: the system handler opens these.
    Web(String),
    /// Any other scheme, and any other `mailto` (some mail clients honour `?attach=`, also
    /// written `%3F` or `&`): asks first.
    OtherScheme(String),
    /// A local file (also every `file:` URL), with the fragment after `#` if any.
    Local {
        path: PathBuf,
        fragment: Option<String>,
    },
}

/// Classifies a destination as written. Relative paths resolve against `doc_dir`.
pub fn classify(dest: &str, doc_dir: &Path) -> Target {
    if let Some(f) = dest.strip_prefix('#') {
        return Target::Anchor(percent_decode(f));
    }
    let mut rest = dest;
    if let Some((scheme, after)) = dest.split_once(':') {
        // One letter is a Windows drive, not a scheme.
        let is_scheme = scheme.len() > 1
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if is_scheme {
            match scheme.to_ascii_lowercase().as_str() {
                // Schemes are case-insensitive. A `file:` URL is handled like a local path
                // (revealed, never opened), since the system handler would run a
                // `.command` file or launch an app.
                "file" => rest = file_url_path(after),
                "http" | "https" => return Target::Web(dest.to_owned()),
                "mailto" if plain_address(after) => return Target::Web(dest.to_owned()),
                _ => return Target::OtherScheme(dest.to_owned()),
            }
        }
    }
    let (path, fragment) = split_fragment(rest);
    let path = PathBuf::from(percent_decode(path));
    Target::Local {
        path: if path.is_absolute() {
            path
        } else {
            doc_dir.join(path)
        },
        fragment,
    }
}

/// `name@example.com` and nothing else: no query, fragment, escapes or second address.
fn plain_address(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let ok = |part: &str, extra: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    ok(local, ".-_+") && ok(domain, ".-")
}

/// The path part of a `file:` URL after the scheme: `///x`, `//localhost/x` and `/x` are
/// all `/x`. Another host stays as `//host/x`; either way the file is only ever revealed
/// or opened as Markdown.
fn file_url_path(after: &str) -> &str {
    match after.strip_prefix("//") {
        Some(auth) => {
            let host_end = auth.find('/').unwrap_or(auth.len());
            match &auth[..host_end] {
                h if h.is_empty() || h.eq_ignore_ascii_case("localhost") => &auth[host_end..],
                _ => after,
            }
        }
        None => after,
    }
}

fn split_fragment(s: &str) -> (&str, Option<String>) {
    match s.split_once('#') {
        Some((p, f)) => (p, Some(percent_decode(f)).filter(|f| !f.is_empty())),
        None => (s, None),
    }
}

/// Whether a local link opens in the app (Markdown) rather than in the file manager.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// `%20` → space.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(Some(h)), Some(Some(l))) =
                (b.get(i + 1).map(|&c| hex(c)), b.get(i + 2).map(|&c| hex(c)))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The inline content of a block's selectable texts, in the order of `block_texts` (and
/// so of layout); `None` for texts without inline markup (code, math, HTML). A capped
/// table gives only its shown rows.
fn inlines<'a>(block: &'a Block, out: &mut Vec<Option<&'a Inline>>) {
    match &block.kind {
        BlockKind::Heading { content, .. } | BlockKind::Paragraph { content, .. } => {
            out.push(Some(content))
        }
        BlockKind::Code { .. }
        | BlockKind::FrontMatter { .. }
        | BlockKind::Math { .. }
        | BlockKind::Html { .. } => out.push(None),
        BlockKind::Rule => {}
        BlockKind::BlockQuote { children, .. } => children.iter().for_each(|b| inlines(b, out)),
        BlockKind::List { items, .. } => items
            .iter()
            .flat_map(|i| &i.children)
            .for_each(|b| inlines(b, out)),
        BlockKind::Table { rows, .. } => rows[..table_rows_shown(rows)]
            .iter()
            .flatten()
            .for_each(|c| out.push(Some(c))),
        BlockKind::Footnotes { notes } => notes
            .iter()
            .flat_map(|n| &n.children)
            .for_each(|b| inlines(b, out)),
    }
}

/// The link whose text covers source offset `at` (Cmd/Ctrl-click in the editor).
pub fn link_at_source(model: &BlockModel, at: usize) -> Option<Link> {
    let mut list = Vec::new();
    for unit in &model.units {
        // Only the unit holding `at` can have the link; the footnotes section has an empty
        // range of its own, so it is always searched.
        let footnotes = matches!(unit.kind, UnitKind::Footnotes);
        if !footnotes && !(unit.range.start..=unit.range.end).contains(&at) {
            continue;
        }
        list.clear();
        inlines(&unit.block, &mut list);
        for inline in list.iter().flatten() {
            for span in &inline.spans {
                let link = match &span.style {
                    SpanStyle::Link { dest, .. } => Link::Url(dest.clone()),
                    SpanStyle::FootnoteRef { label } => Link::Footnote(label.clone()),
                    _ => continue,
                };
                let (Some(s), Some(e)) = (
                    inline.source_offset(span.range.start),
                    inline.source_offset(span.range.end),
                ) else {
                    continue;
                };
                if (s..e.max(s + 1)).contains(&at) {
                    return Some(link);
                }
            }
        }
    }
    None
}

/// A place in the document: a unit's selectable text, and its source offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub unit: usize,
    pub text: usize,
    pub source: usize,
}

/// The first reference to footnote `label`.
pub fn footnote_reference(model: &BlockModel, label: &str) -> Option<Place> {
    let mut list = Vec::new();
    for (u, unit) in model.units.iter().enumerate() {
        list.clear();
        inlines(&unit.block, &mut list);
        for (k, inline) in list.iter().enumerate() {
            let Some(inline) = inline else { continue };
            for span in &inline.spans {
                if matches!(&span.style, SpanStyle::FootnoteRef { label: l } if l == label) {
                    return Some(Place {
                        unit: u,
                        text: k,
                        source: inline.source_offset(span.range.start).unwrap_or(0),
                    });
                }
            }
        }
    }
    None
}

/// Footnote `label`'s definition in the footnotes section.
pub fn footnote_definition(model: &BlockModel, label: &str) -> Option<Place> {
    for (u, unit) in model.units.iter().enumerate() {
        let BlockKind::Footnotes { notes } = &unit.block.kind else {
            continue;
        };
        let mut text = 0;
        for n in notes {
            let mut list = Vec::new();
            n.children.iter().for_each(|b| inlines(b, &mut list));
            if n.label == label {
                return Some(Place {
                    unit: u,
                    text,
                    source: n.range.start,
                });
            }
            text += list.len();
        }
    }
    None
}

/// The heading whose slug is `fragment` (exactly, then ignoring case).
pub fn heading_for(model: &BlockModel, fragment: &str) -> Option<usize> {
    let h = &model.headings;
    h.iter()
        .position(|h| h.slug == fragment)
        .or_else(|| h.iter().position(|h| h.slug.eq_ignore_ascii_case(fragment)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        let dir = Path::new("/notes/a");
        assert_eq!(
            classify("#getting-started", dir),
            Target::Anchor("getting-started".into())
        );
        assert_eq!(
            classify("https://example.com", dir),
            Target::Web("https://example.com".into())
        );
        assert_eq!(
            classify("mailto:me@example.com", dir),
            Target::Web("mailto:me@example.com".into())
        );
        assert_eq!(
            classify("vscode://file/x", dir),
            Target::OtherScheme("vscode://file/x".into())
        );
        assert_eq!(
            classify("../docs/setup.md#install", dir),
            Target::Local {
                path: PathBuf::from("/notes/a/../docs/setup.md"),
                fragment: Some("install".into())
            }
        );
        assert_eq!(
            classify("./run%20me.sh", dir),
            Target::Local {
                path: PathBuf::from("/notes/a/./run me.sh"),
                fragment: None
            }
        );
        assert_eq!(
            classify("file:///tmp/x.md#top", dir),
            Target::Local {
                path: PathBuf::from("/tmp/x.md"),
                fragment: Some("top".into())
            }
        );
        assert!(is_markdown(Path::new("a/B.MARKDOWN")));
    }

    #[test]
    fn every_file_url_is_local_whatever_its_case_or_form() {
        let dir = Path::new("/notes/a");
        let local = |p: &str| Target::Local {
            path: PathBuf::from(p),
            fragment: None,
        };
        assert_eq!(
            classify("FILE:///x/run.command", dir),
            local("/x/run.command")
        );
        assert_eq!(
            classify("File:/Applications/X.app", dir),
            local("/Applications/X.app")
        );
        assert_eq!(classify("file:/path", dir), local("/path"));
        assert_eq!(
            classify("file://localhost/tmp/a%20b.md", dir),
            local("/tmp/a b.md")
        );
        assert_eq!(classify("file:x.md", dir), local("/notes/a/x.md"));
        assert_eq!(
            classify("file://server/share/x", dir),
            local("//server/share/x")
        );
        assert_eq!(
            classify("HTTPS://example.com", dir),
            Target::Web("HTTPS://example.com".into())
        );
    }

    #[test]
    fn mailto_with_a_query_asks_first() {
        let dir = Path::new("/notes");
        assert_eq!(
            classify("MAILTO:me@example.com", dir),
            Target::Web("MAILTO:me@example.com".into())
        );
        assert_eq!(
            classify("mailto:me@example.com?attach=/etc/passwd", dir),
            Target::OtherScheme("mailto:me@example.com?attach=/etc/passwd".into())
        );
        for dest in [
            "mailto:a@b.c%3Fattach=/etc/passwd",
            "mailto:a@b.c&attach=/etc/passwd",
            "mailto:a@b.c#x",
            "mailto:a@b.c,c@d.e",
            "mailto:",
            "MAILTO:?attach=/etc/passwd",
        ] {
            assert_eq!(
                classify(dest, dir),
                Target::OtherScheme(dest.into()),
                "{dest}"
            );
        }
        assert_eq!(
            classify("mailto:first.last+tag@mail-1.example.com", dir),
            Target::Web("mailto:first.last+tag@mail-1.example.com".into())
        );
        assert!(!is_markdown(Path::new("deploy.command")));
    }

    /// Inline spans are a flat list in the model, so hostile nesting (50,000 levels of
    /// emphasis, links, images and footnote references) costs no stack here either.
    #[test]
    fn deep_inline_nesting_fits_a_small_stack() {
        let n = 50_000;
        let wrap = |open: &str, close: &str| format!("{}x{}\n", open.repeat(n), close.repeat(n));
        let docs = [
            wrap("*[~~a ", " a~~](u)*"),
            wrap("![a ", " a](u)"),
            wrap("> *a [^n] ", " a*") + "\n[^n]: note\n",
        ];
        for src in docs {
            std::thread::Builder::new()
                .stack_size(256 * 1024)
                .spawn(move || {
                    let model = mdt_markdown::parse(&src);
                    let at = src.find('x').unwrap();
                    let _ = link_at_source(&model, at);
                    let _ = footnote_reference(&model, "n");
                    let _ = footnote_definition(&model, "n");
                    let _ = heading_for(&model, "x");
                    drop(model);
                })
                .unwrap()
                .join()
                .unwrap();
        }
    }

    /// A table capped at `MAX_TABLE_CELLS` counts only its shown cells, as the reader's
    /// selectable texts do, so a reference after it in the same unit keeps its ordinal.
    #[test]
    fn capped_table_keeps_footnote_ordinals() {
        use crate::reader::block::MAX_TABLE_CELLS;
        let mut src = String::from("> | a |\n> |---|\n");
        for i in 0..MAX_TABLE_CELLS + 500 {
            src.push_str(&format!("> | {i} |\n"));
        }
        src.push_str(">\n> See the note.[^n]\n\n[^n]: The note.\n");
        let model = mdt_markdown::parse(&src);
        let r = footnote_reference(&model, "n").unwrap();
        assert_eq!((r.unit, r.text), (0, MAX_TABLE_CELLS));
        assert_eq!(&src[r.source..r.source + 4], "[^n]");
    }

    #[test]
    fn links_and_footnotes_in_the_model() {
        let src = "# Getting Started\n\nSee [the guide](#getting-started) and a note.[^n]\n\n| a |\n|---|\n| [x](https://x.io) |\n\n[^n]: The note.\n";
        let model = mdt_markdown::parse(src);
        let at = src.find("the guide").unwrap() + 2;
        assert_eq!(
            link_at_source(&model, at),
            Some(Link::Url("#getting-started".into()))
        );
        assert_eq!(
            link_at_source(&model, 3),
            None,
            "heading text is not a link"
        );
        let cell = src.find("[x]").unwrap() + 1;
        assert_eq!(
            link_at_source(&model, cell),
            Some(Link::Url("https://x.io".into()))
        );
        assert_eq!(heading_for(&model, "getting-started"), Some(0));
        assert_eq!(heading_for(&model, "nowhere"), None);
        let r = footnote_reference(&model, "n").unwrap();
        assert_eq!((r.unit, r.text), (1, 0));
        assert_eq!(&src[r.source..r.source + 4], "[^n]");
        let d = footnote_definition(&model, "n").unwrap();
        assert_eq!(model.units[d.unit].kind, mdt_markdown::UnitKind::Footnotes);
        assert_eq!(d.text, 0);
    }
}
