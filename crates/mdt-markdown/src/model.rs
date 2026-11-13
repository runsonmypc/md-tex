//! The block model: a flat list of layout units, each holding a small block tree whose
//! inline content keeps maps between rendered-text offsets and source offsets.

use std::ops::Range;

pub use pulldown_cmark::{Alignment, BlockQuoteKind as AlertKind, MetadataBlockKind};

/// Stands in for an inline object (math, image) in rendered text.
pub const OBJECT_CHAR: char = '\u{FFFC}';

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockModel {
    pub units: Vec<LayoutUnit>,
    /// Every heading in document order, for the palette and anchor links.
    pub headings: Vec<HeadingEntry>,
}

/// One top-level block, or one child of a top-level list or blockquote that was split
/// because it had more than `SPLIT_THRESHOLD` children. Units are what the reader
/// virtualizes, lays out and caches.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutUnit {
    pub kind: UnitKind,
    /// Source byte range. The footnotes section, gathered at the end, has an empty range at
    /// the end of the document; its blocks keep their own ranges.
    pub range: Range<usize>,
    /// Hash of the unit's kind, source text and rendered content (footnote numbers,
    /// resolved links, list numbers), the key for layout and derived caches.
    pub hash: u64,
    pub block: Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Heading,
    Paragraph,
    List,
    /// A single item of a split list.
    ListItem,
    BlockQuote,
    /// A single child of a split blockquote.
    BlockQuoteChild,
    Code,
    Math,
    Table,
    Rule,
    Html,
    FrontMatter,
    Footnotes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub range: Range<usize>,
    pub kind: BlockKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockKind {
    Heading {
        level: u8,
        content: Inline,
        slug: String,
    },
    /// `implicit` marks the text of a tight list item, which has no paragraph of its own.
    Paragraph {
        content: Inline,
        implicit: bool,
    },
    Code {
        /// First word of the info string, if any.
        lang: Option<String>,
        info: String,
        text: String,
        fenced: bool,
    },
    /// A `math` fence. (`$$…$$` is an inline object, so it can sit inside a paragraph.)
    Math {
        latex: String,
    },
    List {
        /// Start number for ordered lists.
        start: Option<u64>,
        tight: bool,
        items: Vec<ListItem>,
    },
    BlockQuote {
        alert: Option<AlertKind>,
        children: Vec<Block>,
    },
    Table {
        alignments: Vec<Alignment>,
        /// The first row is the header.
        rows: Vec<Vec<Inline>>,
    },
    Rule,
    /// Raw HTML shown as literal, dimmed source.
    Html {
        text: String,
    },
    FrontMatter {
        kind: MetadataBlockKind,
        text: String,
    },
    /// The footnotes section: definitions in order of first reference.
    Footnotes {
        notes: Vec<Footnote>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub range: Range<usize>,
    pub task: Option<Task>,
    pub children: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub checked: bool,
    /// Source range of `[ ]` / `[x]`, the bytes a toggle rewrites.
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Footnote {
    pub label: String,
    pub number: usize,
    pub range: Range<usize>,
    pub children: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingEntry {
    pub level: u8,
    pub text: String,
    pub slug: String,
    pub unit: usize,
    pub range: Range<usize>,
}

/// Inline content: rendered text with style spans, inline objects at `OBJECT_CHAR`
/// positions, and a map from rendered offsets to source offsets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    pub spans: Vec<Span>,
    pub objects: Vec<InlineObject>,
    pub map: Vec<Segment>,
}

/// A style applied to a range of rendered text. Spans nest (a link inside strong text).
#[derive(Debug, Clone, PartialEq, Hash)]
pub struct Span {
    pub range: Range<usize>,
    pub style: SpanStyle,
}

#[derive(Debug, Clone, PartialEq, Hash)]
pub enum SpanStyle {
    Emphasis,
    Strong,
    Strikethrough,
    Code,
    Link {
        dest: String,
        title: String,
    },
    /// A footnote reference, rendered as its superscript number.
    FootnoteRef {
        label: String,
    },
    /// Raw inline HTML shown as literal source.
    Html,
    /// A soft line break, rendered as one space.
    SoftBreak,
    /// A hard line break, rendered as `\n`.
    HardBreak,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InlineObject {
    /// Byte offset of its `OBJECT_CHAR` in the rendered text.
    pub at: usize,
    pub source: Range<usize>,
    pub kind: ObjectKind,
}

#[derive(Debug, Clone, PartialEq, Hash)]
pub enum ObjectKind {
    Math {
        latex: String,
        display: bool,
    },
    Image {
        src: String,
        alt: String,
        title: String,
        width: Option<u32>,
        height: Option<u32>,
        /// The `<img>` tag as written, for `<img>` from HTML.
        html: Option<String>,
    },
    /// `<br>` from HTML; renders as a line break.
    HtmlBreak {
        html: String,
    },
}

/// Maps a rendered byte range to the source byte range it came from. When both have the
/// same length the mapping is exact per byte; otherwise (escapes, entities) the whole
/// rendered range maps to the whole source range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub rendered: Range<usize>,
    pub source: Range<usize>,
}

impl Inline {
    /// Source offset for a rendered offset: the segment containing it, or else the one
    /// ending at it.
    pub fn source_offset(&self, rendered: usize) -> Option<usize> {
        // Segments are non-empty and in rendered order, so a binary search finds both
        // candidates; a paragraph can hold many thousands of them.
        let i = self.map.partition_point(|s| s.rendered.end <= rendered);
        let seg = self
            .map
            .get(i)
            .filter(|s| s.rendered.contains(&rendered))
            .or_else(|| {
                let prev = self.map.get(i.checked_sub(1)?)?;
                (prev.rendered.end == rendered).then_some(prev)
            })?;
        Some(if seg.rendered.len() == seg.source.len() {
            seg.source.start + (rendered - seg.rendered.start)
        } else if rendered == seg.rendered.start {
            seg.source.start
        } else {
            seg.source.end
        })
    }

    /// Rendered offset for a source offset. Offsets inside markup (`**`, backticks, link
    /// targets) snap to the nearest rendered boundary that follows them.
    #[cfg(test)]
    pub(crate) fn rendered_offset(&self, source: usize) -> usize {
        for seg in &self.map {
            if seg.source.contains(&source) {
                return if seg.rendered.len() == seg.source.len() {
                    seg.rendered.start + (source - seg.source.start)
                } else {
                    seg.rendered.start
                };
            }
            if source < seg.source.start {
                return seg.rendered.start;
            }
        }
        self.text.len()
    }
}
