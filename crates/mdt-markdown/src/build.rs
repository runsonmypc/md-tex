//! pulldown-cmark events → `BlockModel`.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::html;
use crate::model::*;
use crate::slug::Slugger;

/// A top-level list or blockquote with more children than this becomes one unit per child.
pub const SPLIT_THRESHOLD: usize = 32;

/// Lists and blockquotes nest at most this deep. Deeper ones are flattened into the
/// deepest allowed container (their blocks keep their own source ranges), so every
/// recursion over the block tree, from heading collection to layout and drop, stays
/// shallow even on a background thread's small stack.
pub(crate) const MAX_DEPTH: usize = 32;

/// Formulas typeset per layout unit (and in all footnotes together); later ones show as
/// their source. A unit's formulas must all be in the math cache for it to settle: 100,000
/// inline formulas in one paragraph (about 25 MB of scenes, over the cache's 16 MiB) were
/// evicted and typeset again without end.
pub const MAX_MATH_PER_UNIT: usize = 4000;

pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
}

pub fn parse(source: &str) -> BlockModel {
    parse_with(source, options())
}

pub fn parse_with(source: &str, options: Options) -> BlockModel {
    let mut b = Builder {
        source,
        ..Default::default()
    };
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        b.event(event, range);
    }
    b.finish()
}

/// Builds inline content: rendered text, spans, objects and the source map.
#[derive(Default)]
struct InlineBuilder {
    inline: Inline,
    /// Open spans: start offset, open order, style.
    open: Vec<(usize, usize, SpanStyle)>,
    /// Open order of each finished span in `inline.spans`, so equal ranges nest as written.
    order: Vec<usize>,
    opened: usize,
    /// Set while inside `![alt](src)`: alt text collects here instead of the rendered text.
    image: Option<PendingImage>,
    /// Images nested in an image's alt text, whose own text joins the outer alt.
    nested_images: usize,
}

struct PendingImage {
    src: String,
    title: String,
    alt: String,
    source: Range<usize>,
}

impl InlineBuilder {
    fn push_mapped(&mut self, text: &str, source: Range<usize>) {
        if text.is_empty() {
            return;
        }
        if let Some(img) = &mut self.image {
            img.alt.push_str(text);
            return;
        }
        let start = self.inline.text.len();
        self.inline.text.push_str(text);
        self.inline.map.push(Segment {
            rendered: start..self.inline.text.len(),
            source,
        });
    }

    /// Text whose source is exactly `text` somewhere inside `range` when possible.
    fn push_text(&mut self, text: &str, range: Range<usize>, src: &str) {
        let slice = &src[range.clone()];
        let source = match slice.find(text) {
            Some(off) if !text.is_empty() => range.start + off..range.start + off + text.len(),
            _ => range,
        };
        self.push_mapped(text, source);
    }

    fn push_styled(&mut self, text: &str, source: Range<usize>, style: SpanStyle) {
        let start = self.inline.text.len();
        self.push_mapped(text, source);
        if self.image.is_none() {
            self.inline.spans.push(Span {
                range: start..self.inline.text.len(),
                style,
            });
            self.order.push(self.opened);
            self.opened += 1;
        }
    }

    fn push_object(&mut self, kind: ObjectKind, source: Range<usize>) {
        if let Some(img) = &mut self.image {
            // Math inside alt text: keep its source as alt.
            if let ObjectKind::Math { latex, .. } = &kind {
                img.alt.push_str(latex);
            }
            return;
        }
        let at = self.inline.text.len();
        self.inline.text.push(OBJECT_CHAR);
        self.inline.map.push(Segment {
            rendered: at..self.inline.text.len(),
            source: source.clone(),
        });
        self.inline.objects.push(InlineObject { at, source, kind });
    }

    fn open_span(&mut self, style: SpanStyle) {
        if self.image.is_some() {
            return;
        }
        self.open.push((self.inline.text.len(), self.opened, style));
        self.opened += 1;
    }

    fn close_span(&mut self) {
        if self.image.is_some() {
            return;
        }
        if let Some((start, order, style)) = self.open.pop() {
            self.inline.spans.push(Span {
                range: start..self.inline.text.len(),
                style,
            });
            self.order.push(order);
        }
    }

    fn finish(mut self) -> Inline {
        while !self.open.is_empty() {
            self.close_span();
        }
        let mut spans: Vec<(usize, Span)> = self.order.into_iter().zip(self.inline.spans).collect();
        spans.sort_by_key(|(order, s)| (s.range.start, std::cmp::Reverse(s.range.end), *order));
        self.inline.spans = spans.into_iter().map(|(_, s)| s).collect();
        self.inline
    }
}

enum Frame {
    Paragraph {
        start: usize,
        inline: InlineBuilder,
        implicit: bool,
    },
    Heading {
        start: usize,
        level: u8,
        inline: InlineBuilder,
    },
    Code {
        start: usize,
        info: String,
        fenced: bool,
        text: String,
    },
    Html {
        start: usize,
        text: String,
    },
    Meta {
        start: usize,
        kind: MetadataBlockKind,
        text: String,
    },
    List {
        start: usize,
        number: Option<u64>,
        items: Vec<ListItem>,
    },
    Item {
        start: usize,
        task: Option<Task>,
        children: Vec<Block>,
    },
    Quote {
        start: usize,
        alert: Option<AlertKind>,
        children: Vec<Block>,
    },
    Table {
        start: usize,
        alignments: Vec<Alignment>,
        rows: Vec<Vec<Inline>>,
    },
    Row {
        cells: Vec<Inline>,
    },
    Cell {
        inline: InlineBuilder,
    },
    Footnote {
        start: usize,
        label: String,
        children: Vec<Block>,
    },
}

#[derive(Default)]
struct Builder<'a> {
    source: &'a str,
    stack: Vec<Frame>,
    units: Vec<LayoutUnit>,
    footnote_defs: Vec<(String, Range<usize>, Vec<Block>)>,
    footnote_numbers: HashMap<String, usize>,
    slugger: Slugger,
    /// Open lists, items and blockquotes past `MAX_DEPTH`, which have no frame.
    flattened: usize,
    /// Formulas since the last unit was pushed, and in footnote definitions.
    unit_math: usize,
    footnote_math: usize,
}

impl Builder<'_> {
    fn inline_target(&mut self) -> Option<&mut InlineBuilder> {
        match self.stack.last_mut()? {
            Frame::Paragraph { inline, .. }
            | Frame::Heading { inline, .. }
            | Frame::Cell { inline } => Some(inline),
            _ => None,
        }
    }

    /// Inline content directly inside a list item (a tight list) gets an implicit paragraph.
    fn ensure_inline(&mut self, at: usize) -> &mut InlineBuilder {
        if self.inline_target().is_none() {
            self.stack.push(Frame::Paragraph {
                start: at,
                inline: InlineBuilder::default(),
                implicit: true,
            });
        }
        self.inline_target().unwrap()
    }

    /// Open lists and blockquotes.
    fn depth(&self) -> usize {
        self.stack
            .iter()
            .filter(|f| matches!(f, Frame::List { .. } | Frame::Quote { .. }))
            .count()
    }

    /// Closes an implicit paragraph before a block starts or its item ends.
    fn close_implicit(&mut self, end: usize) {
        if matches!(
            self.stack.last(),
            Some(Frame::Paragraph { implicit: true, .. })
        ) {
            let Some(Frame::Paragraph { start, inline, .. }) = self.stack.pop() else {
                unreachable!()
            };
            let end = trim_end(self.source, start, end);
            self.emit(Block {
                range: start..end,
                kind: BlockKind::Paragraph {
                    content: inline.finish(),
                    implicit: true,
                },
            });
        }
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        let src = self.source;
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(tag) => self.end(tag, range),
            Event::Text(text) => match self.stack.last_mut() {
                Some(
                    Frame::Code { text: buf, .. }
                    | Frame::Meta { text: buf, .. }
                    | Frame::Html { text: buf, .. },
                ) => buf.push_str(&text),
                _ => self.ensure_inline(range.start).push_text(&text, range, src),
            },
            Event::Code(text) => {
                let inner = code_inner(src, range.clone(), &text);
                self.ensure_inline(range.start)
                    .push_styled(&text, inner, SpanStyle::Code);
            }
            Event::InlineMath(_) | Event::DisplayMath(_) if !self.take_math() => {
                // Past the limit: literal text, delimiters included.
                let text = &src[range.clone()];
                self.ensure_inline(range.start).push_mapped(text, range);
            }
            Event::InlineMath(latex) => {
                if inline_math_delimiters_valid(src, &range) {
                    let kind = ObjectKind::Math {
                        latex: latex.to_string(),
                        display: false,
                    };
                    self.ensure_inline(range.start).push_object(kind, range);
                } else {
                    // Literal text, dollar signs included.
                    let text = &src[range.clone()];
                    self.ensure_inline(range.start).push_mapped(text, range);
                }
            }
            Event::DisplayMath(latex) => {
                let kind = ObjectKind::Math {
                    latex: latex.to_string(),
                    display: true,
                };
                self.ensure_inline(range.start).push_object(kind, range);
            }
            Event::Html(text) => match self.stack.last_mut() {
                Some(Frame::Html { text: buf, .. }) => buf.push_str(&text),
                _ => self.inline_html(&text, range),
            },
            Event::InlineHtml(text) => self.inline_html(&text, range),
            Event::FootnoteReference(label) => {
                let next = self.footnote_numbers.len() + 1;
                let number = *self
                    .footnote_numbers
                    .entry(label.to_string())
                    .or_insert(next);
                let style = SpanStyle::FootnoteRef {
                    label: label.to_string(),
                };
                self.ensure_inline(range.start)
                    .push_styled(&number.to_string(), range, style);
            }
            Event::SoftBreak => {
                self.ensure_inline(range.start)
                    .push_styled(" ", range, SpanStyle::SoftBreak)
            }
            Event::HardBreak => {
                self.ensure_inline(range.start)
                    .push_styled("\n", range, SpanStyle::HardBreak)
            }
            Event::Rule => {
                // A rule ends the text of a tight list item.
                self.close_implicit(range.start);
                self.emit(Block {
                    range,
                    kind: BlockKind::Rule,
                });
            }
            // A flattened item has no frame to show its checkbox.
            Event::TaskListMarker(_) if self.flattened > 0 => {}
            Event::TaskListMarker(checked) => {
                let task = Task {
                    checked,
                    range: task_marker_range(src, range),
                };
                if let Some(Frame::Item { task: slot, .. }) = self
                    .stack
                    .iter_mut()
                    .rev()
                    .find(|f| matches!(f, Frame::Item { .. }))
                {
                    *slot = Some(task);
                }
            }
        }
    }

    fn inline_html(&mut self, text: &str, range: Range<usize>) {
        let b = self.ensure_inline(range.start);
        match html::recognize(text) {
            Some(kind) => b.push_object(kind, range),
            None => b.push_styled(text, range, SpanStyle::Html),
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        let at = range.start;
        let inline_style = match &tag {
            Tag::Emphasis => Some(SpanStyle::Emphasis),
            Tag::Strong => Some(SpanStyle::Strong),
            Tag::Strikethrough => Some(SpanStyle::Strikethrough),
            Tag::Link {
                link_type: LinkType::Email,
                dest_url,
                title,
                ..
            } => Some(SpanStyle::Link {
                dest: format!("mailto:{dest_url}"),
                title: title.to_string(),
            }),
            Tag::Link {
                dest_url, title, ..
            } => Some(SpanStyle::Link {
                dest: dest_url.to_string(),
                title: title.to_string(),
            }),
            _ => None,
        };
        if let Some(style) = inline_style {
            self.ensure_inline(at).open_span(style);
            return;
        }
        match tag {
            Tag::Image {
                dest_url, title, ..
            } => {
                let b = self.ensure_inline(at);
                if b.image.is_some() {
                    b.nested_images += 1;
                    return;
                }
                b.image = Some(PendingImage {
                    src: dest_url.to_string(),
                    title: title.to_string(),
                    alt: String::new(),
                    source: range,
                });
                return;
            }
            Tag::Superscript | Tag::Subscript => return,
            // Too deep: the container's blocks go to the deepest open one. An item is
            // flattened with its list.
            Tag::List(_) | Tag::BlockQuote(_)
                if self.flattened > 0 || self.depth() >= MAX_DEPTH =>
            {
                self.close_implicit(at);
                self.flattened += 1;
                return;
            }
            Tag::Item if self.flattened > 0 => {
                self.close_implicit(at);
                self.flattened += 1;
                return;
            }
            _ => {}
        }
        self.close_implicit(at);
        let frame = match tag {
            Tag::Paragraph => Frame::Paragraph {
                start: at,
                inline: InlineBuilder::default(),
                implicit: false,
            },
            Tag::Heading { level, .. } => Frame::Heading {
                start: at,
                level: level as u8,
                inline: InlineBuilder::default(),
            },
            Tag::CodeBlock(kind) => {
                let (info, fenced) = match kind {
                    CodeBlockKind::Fenced(info) => (info.to_string(), true),
                    CodeBlockKind::Indented => (String::new(), false),
                };
                Frame::Code {
                    start: at,
                    info,
                    fenced,
                    text: String::new(),
                }
            }
            Tag::HtmlBlock => Frame::Html {
                start: at,
                text: String::new(),
            },
            Tag::MetadataBlock(kind) => Frame::Meta {
                start: at,
                kind,
                text: String::new(),
            },
            Tag::List(number) => Frame::List {
                start: at,
                number,
                items: Vec::new(),
            },
            Tag::Item => Frame::Item {
                start: at,
                task: None,
                children: Vec::new(),
            },
            Tag::BlockQuote(alert) => Frame::Quote {
                start: at,
                alert,
                children: Vec::new(),
            },
            Tag::Table(alignments) => Frame::Table {
                start: at,
                alignments,
                rows: Vec::new(),
            },
            Tag::TableHead | Tag::TableRow => Frame::Row { cells: Vec::new() },
            Tag::TableCell => Frame::Cell {
                inline: InlineBuilder::default(),
            },
            Tag::FootnoteDefinition(label) => Frame::Footnote {
                start: at,
                label: label.to_string(),
                children: Vec::new(),
            },
            // Not enabled.
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {
                Frame::Quote {
                    start: at,
                    alert: None,
                    children: Vec::new(),
                }
            }
            _ => unreachable!("inline tags handled above"),
        };
        self.stack.push(frame);
    }

    fn end(&mut self, tag: TagEnd, range: Range<usize>) {
        match tag {
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                if let Some(b) = self.inline_target() {
                    b.close_span();
                }
                return;
            }
            TagEnd::Image => {
                if let Some(b) = self.inline_target()
                    && b.nested_images > 0
                {
                    b.nested_images -= 1;
                } else if let Some(b) = self.inline_target()
                    && let Some(img) = b.image.take()
                {
                    let kind = ObjectKind::Image {
                        src: img.src,
                        alt: img.alt,
                        title: img.title,
                        width: None,
                        height: None,
                        html: None,
                    };
                    b.push_object(kind, img.source);
                }
                return;
            }
            TagEnd::Superscript | TagEnd::Subscript => return,
            TagEnd::List(_) | TagEnd::Item | TagEnd::BlockQuote(_) if self.flattened > 0 => {
                self.close_implicit(range.end);
                self.flattened -= 1;
                return;
            }
            TagEnd::Item => self.close_implicit(range.end),
            _ => {}
        }
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let end = trim_end(self.source, range.start, range.end);
        let block = match frame {
            Frame::Paragraph {
                start,
                inline,
                implicit,
            } => Block {
                range: start..end,
                kind: BlockKind::Paragraph {
                    content: inline.finish(),
                    implicit,
                },
            },
            Frame::Heading {
                start,
                level,
                inline,
            } => {
                let content = inline.finish();
                let slug = self.slugger.slug(&heading_text(&content));
                Block {
                    range: start..end,
                    kind: BlockKind::Heading {
                        level,
                        content,
                        slug,
                    },
                }
            }
            Frame::Code {
                start,
                info,
                fenced,
                text,
            } => {
                let lang = info.split_whitespace().next().map(str::to_owned);
                let kind = if fenced && lang.as_deref() == Some("math") {
                    BlockKind::Math {
                        latex: text.trim_end_matches('\n').to_owned(),
                    }
                } else {
                    BlockKind::Code {
                        lang,
                        info,
                        text,
                        fenced,
                    }
                };
                Block {
                    range: start..end,
                    kind,
                }
            }
            Frame::Html { start, text } => html_block(self.source, start..end, text),
            Frame::Meta { start, kind, text } => Block {
                range: start..end,
                kind: BlockKind::FrontMatter { kind, text },
            },
            Frame::List {
                start,
                number,
                items,
            } => {
                let tight = !items.iter().any(|i| {
                    i.children.iter().any(|c| {
                        matches!(
                            c.kind,
                            BlockKind::Paragraph {
                                implicit: false,
                                ..
                            }
                        )
                    })
                });
                Block {
                    range: start..end,
                    kind: BlockKind::List {
                        start: number,
                        tight,
                        items,
                    },
                }
            }
            Frame::Item {
                start,
                task,
                children,
            } => {
                if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
                    items.push(ListItem {
                        range: start..end,
                        task,
                        children,
                    });
                }
                return;
            }
            Frame::Quote {
                start,
                alert,
                children,
            } => Block {
                range: start..end,
                kind: BlockKind::BlockQuote { alert, children },
            },
            Frame::Table {
                start,
                alignments,
                rows,
            } => Block {
                range: start..end,
                kind: BlockKind::Table { alignments, rows },
            },
            Frame::Row { cells } => {
                if let Some(Frame::Table { rows, .. }) = self.stack.last_mut() {
                    rows.push(cells);
                }
                return;
            }
            Frame::Cell { inline } => {
                if let Some(Frame::Row { cells }) = self.stack.last_mut() {
                    cells.push(inline.finish());
                }
                return;
            }
            Frame::Footnote {
                start,
                label,
                children,
            } => {
                self.footnote_defs.push((label, start..end, children));
                return;
            }
        };
        self.emit(block);
    }

    /// Adds a finished block to its parent, or makes it a unit at the top level.
    fn emit(&mut self, block: Block) {
        match self.stack.last_mut() {
            Some(
                Frame::Item { children, .. }
                | Frame::Quote { children, .. }
                | Frame::Footnote { children, .. },
            ) => children.push(block),
            Some(_) => {}
            None => self.push_units(block),
        }
    }

    fn push_units(&mut self, block: Block) {
        match block.kind {
            BlockKind::List {
                start,
                tight,
                items,
            } if items.len() > SPLIT_THRESHOLD => {
                for (i, item) in items.into_iter().enumerate() {
                    let range = item.range.clone();
                    let kind = BlockKind::List {
                        start: start.map(|s| s + i as u64),
                        tight,
                        items: vec![item],
                    };
                    self.push_unit(UnitKind::ListItem, Block { range, kind });
                }
            }
            BlockKind::BlockQuote { alert, children } if children.len() > SPLIT_THRESHOLD => {
                for child in children {
                    let range = child.range.clone();
                    let kind = BlockKind::BlockQuote {
                        alert,
                        children: vec![child],
                    };
                    self.push_unit(UnitKind::BlockQuoteChild, Block { range, kind });
                }
            }
            kind => {
                let unit_kind = match &kind {
                    BlockKind::Heading { .. } => UnitKind::Heading,
                    BlockKind::Paragraph { .. } => UnitKind::Paragraph,
                    BlockKind::Code { .. } => UnitKind::Code,
                    BlockKind::Math { .. } => UnitKind::Math,
                    BlockKind::List { .. } => UnitKind::List,
                    BlockKind::BlockQuote { .. } => UnitKind::BlockQuote,
                    BlockKind::Table { .. } => UnitKind::Table,
                    BlockKind::Rule => UnitKind::Rule,
                    BlockKind::Html { .. } => UnitKind::Html,
                    BlockKind::FrontMatter { .. } => UnitKind::FrontMatter,
                    BlockKind::Footnotes { .. } => UnitKind::Footnotes,
                };
                self.push_unit(
                    unit_kind,
                    Block {
                        range: block.range,
                        kind,
                    },
                );
            }
        }
    }

    /// Counts a formula against `MAX_MATH_PER_UNIT`; false once the limit is reached.
    fn take_math(&mut self) -> bool {
        let in_footnote = self
            .stack
            .iter()
            .any(|f| matches!(f, Frame::Footnote { .. }));
        let n = if in_footnote {
            &mut self.footnote_math
        } else {
            &mut self.unit_math
        };
        *n += 1;
        *n <= MAX_MATH_PER_UNIT
    }

    fn push_unit(&mut self, kind: UnitKind, block: Block) {
        self.unit_math = 0;
        let range = block.range.clone();
        let hash = unit_hash(kind, [&self.source[range.clone()]], &block);
        self.units.push(LayoutUnit {
            kind,
            range,
            hash,
            block,
        });
    }

    fn finish(mut self) -> BlockModel {
        while !self.stack.is_empty() {
            let len = self.source.len();
            self.end(TagEnd::Paragraph, len..len);
        }
        // A label defined twice keeps its first definition (GFM).
        let mut defined = HashSet::new();
        let mut notes: Vec<Footnote> = std::mem::take(&mut self.footnote_defs)
            .into_iter()
            .filter(|(label, ..)| defined.insert(label.clone()))
            .filter_map(|(label, range, children)| {
                let number = *self.footnote_numbers.get(&label)?;
                Some(Footnote {
                    label,
                    number,
                    range,
                    children,
                })
            })
            .collect();
        if !notes.is_empty() {
            notes.sort_by_key(|n| n.number);
            let end = self.source.len();
            let block = Block {
                range: end..end,
                kind: BlockKind::Footnotes { notes },
            };
            let BlockKind::Footnotes { notes } = &block.kind else {
                unreachable!()
            };
            let sources = notes.iter().map(|n| &self.source[n.range.clone()]);
            let hash = unit_hash(UnitKind::Footnotes, sources, &block);
            self.units.push(LayoutUnit {
                kind: UnitKind::Footnotes,
                range: end..end,
                hash,
                block,
            });
        }
        let mut headings = Vec::new();
        for (unit, u) in self.units.iter().enumerate() {
            collect_headings(&u.block, unit, &mut headings);
        }
        BlockModel {
            units: self.units,
            headings,
        }
    }
}

fn collect_headings(block: &Block, unit: usize, out: &mut Vec<HeadingEntry>) {
    match &block.kind {
        BlockKind::Heading {
            level,
            content,
            slug,
        } => out.push(HeadingEntry {
            level: *level,
            text: heading_text(content),
            slug: slug.clone(),
            unit,
            range: block.range.clone(),
        }),
        BlockKind::BlockQuote { children, .. } => {
            children.iter().for_each(|c| collect_headings(c, unit, out))
        }
        BlockKind::List { items, .. } => items
            .iter()
            .flat_map(|i| &i.children)
            .for_each(|c| collect_headings(c, unit, out)),
        _ => {}
    }
}

/// Rendered text with inline objects replaced by their text form (LaTeX or alt text).
pub fn plain_text(inline: &Inline) -> String {
    let mut out = String::with_capacity(inline.text.len());
    let mut objects = inline.objects.iter();
    for ch in inline.text.chars() {
        if ch == OBJECT_CHAR
            && let Some(obj) = objects.next()
        {
            match &obj.kind {
                ObjectKind::Math { latex, .. } => out.push_str(latex),
                ObjectKind::Image { alt, .. } => out.push_str(alt),
                ObjectKind::HtmlBreak { .. } => out.push('\n'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// A heading's text for its slug and the heading list: `plain_text` without footnote
/// reference numbers.
fn heading_text(inline: &Inline) -> String {
    let mut notes: Vec<&Range<usize>> = inline
        .spans
        .iter()
        .filter(|s| matches!(s.style, SpanStyle::FootnoteRef { .. }))
        .map(|s| &s.range)
        .collect();
    if notes.is_empty() {
        return plain_text(inline);
    }
    // One pass over the text and the sorted references: testing every character against
    // every reference took half a second for a long heading, before the first frame.
    notes.sort_by_key(|r| r.start);
    let mut notes = notes.into_iter().peekable();
    let mut kept = inline.clone();
    kept.text = inline
        .text
        .char_indices()
        .filter(|&(at, _)| {
            while notes.next_if(|r| r.end <= at).is_some() {}
            !notes.peek().is_some_and(|r| r.contains(&at))
        })
        .map(|(_, ch)| ch)
        .collect();
    plain_text(&kept)
}

/// The key for layout and derived caches: the unit's kind, source text and everything it
/// renders. The rendered content also depends on the rest of the document (footnote
/// numbers, link reference definitions, a split list item's number), so source text alone
/// would let a stale layout be reused. Source offsets are left out, so a unit that only
/// moved keeps its key.
fn unit_hash<'a>(kind: UnitKind, sources: impl IntoIterator<Item = &'a str>, block: &Block) -> u64 {
    let mut h = xxhash_rust::xxh3::Xxh3::with_seed(kind as u64);
    for source in sources {
        source.hash(&mut h);
    }
    hash_block(block, &mut h);
    h.finish()
}

fn hash_block(block: &Block, h: &mut impl Hasher) {
    std::mem::discriminant(&block.kind).hash(h);
    match &block.kind {
        // The slug is not drawn, and differs between equal headings.
        BlockKind::Heading { level, content, .. } => {
            level.hash(h);
            hash_inline(content, h);
        }
        BlockKind::Paragraph { content, implicit } => {
            implicit.hash(h);
            hash_inline(content, h);
        }
        BlockKind::Code {
            lang,
            info,
            text,
            fenced,
        } => (lang, info, text, fenced).hash(h),
        BlockKind::Math { latex } => latex.hash(h),
        BlockKind::List {
            start,
            tight,
            items,
        } => {
            (start, tight, items.len()).hash(h);
            for item in items {
                item.task.as_ref().map(|t| t.checked).hash(h);
                hash_blocks(&item.children, h);
            }
        }
        BlockKind::BlockQuote { alert, children } => {
            alert.hash(h);
            hash_blocks(children, h);
        }
        BlockKind::Table { alignments, rows } => {
            for a in alignments {
                (*a as u8).hash(h);
            }
            rows.len().hash(h);
            for row in rows {
                row.len().hash(h);
                row.iter().for_each(|cell| hash_inline(cell, h));
            }
        }
        BlockKind::Rule => {}
        BlockKind::Html { text } => text.hash(h),
        BlockKind::FrontMatter { kind, text } => (kind, text).hash(h),
        BlockKind::Footnotes { notes } => {
            notes.len().hash(h);
            for n in notes {
                (&n.label, n.number).hash(h);
                hash_blocks(&n.children, h);
            }
        }
    }
}

fn hash_blocks(blocks: &[Block], h: &mut impl Hasher) {
    blocks.len().hash(h);
    blocks.iter().for_each(|b| hash_block(b, h));
}

fn hash_inline(inline: &Inline, h: &mut impl Hasher) {
    inline.text.hash(h);
    inline.spans.hash(h);
    inline.objects.len().hash(h);
    for o in &inline.objects {
        (o.at, &o.kind).hash(h);
    }
}

/// Inline math delimiter rules, stricter than pulldown-cmark's:
/// the opening `$` must not be followed by whitespace, and the closing `$` must not be
/// preceded by whitespace or followed by a digit.
fn inline_math_delimiters_valid(src: &str, range: &Range<usize>) -> bool {
    let inner = &src[range.start + 1..range.end - 1];
    let after = src[range.end..].chars().next();
    !inner.starts_with(char::is_whitespace)
        && !inner.ends_with(char::is_whitespace)
        && !after.is_some_and(|c| c.is_ascii_digit())
}

/// pulldown-cmark block ranges include the trailing newline; units end before it.
fn trim_end(src: &str, start: usize, end: usize) -> usize {
    let mut end = end.min(src.len());
    while end > start && matches!(src.as_bytes()[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    end
}

/// The source range of a code span's content inside its backticks.
fn code_inner(src: &str, range: Range<usize>, text: &str) -> Range<usize> {
    let slice = &src[range.clone()];
    let ticks = slice.bytes().take_while(|&b| b == b'`').count();
    let inner = range.start + ticks..range.end.saturating_sub(ticks).max(range.start + ticks);
    match src[inner.clone()].find(text) {
        Some(off) if !text.is_empty() => inner.start + off..inner.start + off + text.len(),
        _ => inner,
    }
}

/// Narrows a task marker's range to the `[ ]` / `[x]` brackets.
fn task_marker_range(src: &str, range: Range<usize>) -> Range<usize> {
    match src[range.clone()].find('[') {
        Some(off) if src[range.start + off..].len() >= 3 => {
            range.start + off..range.start + off + 3
        }
        _ => range,
    }
}

/// An HTML block made only of `<img>` / `<br>` tags becomes a paragraph with those
/// objects; anything else stays literal.
fn html_block(src: &str, range: Range<usize>, text: String) -> Block {
    match html::only_known_tags(&text) {
        Some(tags) if !tags.is_empty() => {
            let mut b = InlineBuilder::default();
            // `text` lacks the indentation and container markers (`> `) of the source, so
            // each tag is found in the source, after the previous one.
            let mut pos = range.start;
            for (offset, len, kind) in tags {
                let tag = &text[offset..offset + len];
                let source = match src[pos..range.end].find(tag) {
                    Some(at) => pos + at..pos + at + len,
                    // A tag broken across container lines: the rest of the block.
                    None => pos..range.end,
                };
                pos = source.end;
                b.push_object(kind, source);
            }
            Block {
                range,
                kind: BlockKind::Paragraph {
                    content: b.finish(),
                    implicit: false,
                },
            }
        }
        _ => Block {
            range,
            kind: BlockKind::Html { text },
        },
    }
}

#[cfg(test)]
mod tests {
    /// Leaving footnote references out of a heading's text tested each character against
    /// each reference: 0.6 s for a 120 KB heading in a release build.
    #[test]
    fn heading_text_is_linear_in_footnote_references() {
        let doc = format!("# x{}y\n\n[^a]: Note.\n", "[^a]".repeat(30_000));
        let start = std::time::Instant::now();
        let model = crate::parse(&doc);
        let elapsed = start.elapsed();
        assert_eq!(model.headings[0].text, "xy");
        assert!(elapsed.as_secs_f32() < 1.0, "took {elapsed:?}");
    }
}
