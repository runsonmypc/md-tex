//! Paragraph layout with Parley: style runs over the text plus inline boxes for objects
//! (math, images). A line grows to fit its tallest inline box.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};
use std::ops::Range;

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontFamilyName, FontStyle, FontWeight, GenericFamily,
    InlineBox, InlineBoxKind, Layout, LayoutContext, LineHeight, OverflowWrap, StyleProperty,
    VerticalAlign,
};

use crate::fonts::{BODY_FAMILY, FontRegistry, MONO_FAMILY};
use crate::paint::Brush;

/// Marks an inline object in the text. Laid out as U+200B (same UTF-8 length, so byte
/// offsets stay valid), which shapes to nothing and allows a line break.
pub const OBJECT_CHAR: char = '\u{FFFC}';

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BaseStyle {
    pub size: f32,
    /// Multiple of the font size.
    pub line_height: f32,
    pub mono: bool,
    pub bold: bool,
    pub brush: Brush,
}

/// Styling over a byte range of the text. Unset fields keep the base style.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleRun {
    pub range: Range<usize>,
    pub mono: bool,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub brush: Option<Brush>,
    /// Font size multiplier (e.g. superscript footnote numbers).
    pub scale: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InlineBoxSpec {
    pub id: u64,
    /// Byte offset of the box's `OBJECT_CHAR`.
    pub at: usize,
    pub width: f32,
    pub height: f32,
    /// Distance from the box's top to its baseline, which sits on the text baseline.
    pub baseline: f32,
}

/// Most style ranges (runs and emoji clusters) one paragraph gives Parley; the text past
/// them keeps the base style. Each range splits at most two of Parley's style spans, and
/// Parley panics past 65,535 spans: highlighted code of 1 MB, or 100,000 autolinks or
/// emoji in one paragraph, crashed the app. 32,000 runs take about 50 ms to lay out.
pub const MAX_STYLE_RANGES: usize = 32_000;

pub struct Paragraph {
    pub layout: Layout<Brush>,
    pub width: f32,
    pub height: f32,
}

fn family_name(mono: bool) -> &'static str {
    if mono { MONO_FAMILY } else { BODY_FAMILY }
}

fn family(mono: bool) -> FontFamily<'static> {
    FontFamily::named(family_name(mono))
}

pub fn layout_paragraph(
    fonts: &mut FontRegistry,
    lcx: &mut LayoutContext<Brush>,
    text: &str,
    base: BaseStyle,
    runs: &[StyleRun],
    boxes: &[InlineBoxSpec],
    max_width: f32,
) -> Paragraph {
    build(fonts, lcx, text, base, runs, boxes, max_width, false)
}

/// A table cell: laid out on one line, ready to measure (`content_widths`) and to break
/// again at its column's width (`rebreak`). A word longer than the column breaks inside
/// itself, but the min-content width stays the widest whole word.
pub fn layout_cell(
    fonts: &mut FontRegistry,
    lcx: &mut LayoutContext<Brush>,
    text: &str,
    base: BaseStyle,
    runs: &[StyleRun],
    boxes: &[InlineBoxSpec],
) -> Paragraph {
    build(fonts, lcx, text, base, runs, boxes, f32::INFINITY, true)
}

impl Paragraph {
    /// (min-content, max-content) widths: with every soft break taken, and with none.
    pub fn content_widths(&self) -> (f32, f32) {
        let w = self.layout.calculate_content_widths();
        (w.min, w.max)
    }

    /// Breaks the lines again at `max_width`.
    pub fn rebreak(&mut self, max_width: f32) {
        self.layout.break_all_lines(Some(max_width));
        self.layout
            .align(Alignment::Start, AlignmentOptions::default());
        self.width = self.layout.width();
        self.height = self.layout.height();
    }
}

#[allow(clippy::too_many_arguments)]
fn build(
    fonts: &mut FontRegistry,
    lcx: &mut LayoutContext<Brush>,
    text: &str,
    base: BaseStyle,
    runs: &[StyleRun],
    boxes: &[InlineBoxSpec],
    max_width: f32,
    break_words: bool,
) -> Paragraph {
    let shaped = text.replace(OBJECT_CHAR, "\u{200B}");
    let emoji = emoji_styles(fonts, &shaped, base, runs);
    let mut b = lcx.ranged_builder(&mut fonts.fcx, &shaped, 1.0, false);
    b.push_default(StyleProperty::FontFamily(family(base.mono)));
    b.push_default(StyleProperty::FontSize(base.size));
    b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
        base.line_height,
    )));
    b.push_default(StyleProperty::Brush(base.brush));
    if break_words {
        b.push_default(StyleProperty::OverflowWrap(OverflowWrap::BreakWord));
    }
    if base.bold {
        b.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
    }
    // Parley inserts each range's spans into a list, so ranges out of text order took time
    // quadratic in their number (16,000 emoji after as many runs, 1.3 s). Runs come in
    // text order; an emoji cluster goes after the runs that start at or before it, which
    // it overrides as it did when every cluster came last.
    let in_order = runs.is_sorted_by_key(|r| r.range.start);
    let mut emoji = emoji.into_iter().peekable();
    let mut ranges = Vec::with_capacity(runs.len() + emoji.len());
    for r in runs {
        while let Some(e) = emoji.next_if(|e| in_order && e.0.start < r.range.start) {
            ranges.push(Styled::Emoji(e));
        }
        ranges.push(Styled::Run(r));
    }
    ranges.extend(emoji.map(Styled::Emoji));
    for range in ranges.into_iter().take(MAX_STYLE_RANGES) {
        match range {
            Styled::Run(r) => push_run(&mut b, base, r),
            Styled::Emoji((range, mono, spacing)) => {
                let families = vec![
                    FontFamilyName::Generic(GenericFamily::Emoji),
                    FontFamilyName::named(family_name(mono)),
                ];
                b.push(
                    StyleProperty::FontFamily(FontFamily::List(families.into())),
                    range.clone(),
                );
                if spacing > 0.0 {
                    b.push(StyleProperty::LetterSpacing(spacing), range);
                }
            }
        }
    }
    for bx in boxes {
        b.push_inline_box(InlineBox {
            id: bx.id,
            kind: InlineBoxKind::InFlow,
            index: bx.at,
            width: bx.width,
            height: bx.height,
            baseline: Some(bx.baseline),
            vertical_align: VerticalAlign::BASELINE,
        });
    }
    let mut layout = b.build(&shaped);
    layout.break_all_lines(Some(max_width));
    layout.align(Alignment::Start, AlignmentOptions::default());
    let width = layout.width();
    let height = layout.height();
    Paragraph {
        layout,
        width,
        height,
    }
}

enum Styled<'a> {
    Run(&'a StyleRun),
    /// An emoji cluster, whether it is in mono text, and its letter spacing.
    Emoji((Range<usize>, bool, f32)),
}

fn push_run(b: &mut parley::RangedBuilder<'_, Brush>, base: BaseStyle, r: &StyleRun) {
    let range = r.range.clone();
    if r.mono {
        b.push(StyleProperty::FontFamily(family(true)), range.clone());
    }
    if r.bold {
        b.push(StyleProperty::FontWeight(FontWeight::BOLD), range.clone());
    }
    if r.italic {
        b.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
    }
    if r.underline {
        b.push(StyleProperty::Underline(true), range.clone());
    }
    if r.strikethrough {
        b.push(StyleProperty::Strikethrough(true), range.clone());
    }
    if let Some(brush) = r.brush {
        b.push(StyleProperty::Brush(brush), range.clone());
        b.push(StyleProperty::UnderlineBrush(Some(brush)), range.clone());
        b.push(
            StyleProperty::StrikethroughBrush(Some(brush)),
            range.clone(),
        );
    }
    if let Some(scale) = r.scale {
        b.push(StyleProperty::FontSize(base.size * scale), range);
    }
}

/// Emoji clusters, which ask for the emoji font before the text font (which may have a
/// plain glyph for the same character) and get the platform's advance
/// (`FontRegistry::emoji_spacing`): each with whether it is in mono text and its letter
/// spacing, from the runs around its start (the last of them that scales sets the size).
/// One sweep over runs and clusters by position: testing every run for every cluster was
/// quadratic.
fn emoji_styles(
    fonts: &FontRegistry,
    text: &str,
    base: BaseStyle,
    runs: &[StyleRun],
) -> Vec<(Range<usize>, bool, f32)> {
    let ranges = crate::emoji::emoji_ranges(text);
    if ranges.is_empty() {
        return Vec::new();
    }
    let mut by_start: Vec<usize> = (0..runs.len()).collect();
    by_start.sort_by_key(|&i| runs[i].range.start);
    let mut next = by_start.into_iter().peekable();
    // Runs started so far that may still hold the position, by end; of those, how many
    // are mono and which scale.
    let mut open = BinaryHeap::new();
    let (mut mono, mut scaled) = (0usize, BTreeSet::new());
    ranges
        .into_iter()
        .map(|range| {
            let at = range.start;
            while let Some(i) = next.next_if(|&i| runs[i].range.start <= at) {
                open.push(Reverse((runs[i].range.end, i)));
                mono += usize::from(runs[i].mono);
                if runs[i].scale.is_some() {
                    scaled.insert(i);
                }
            }
            while let Some(&Reverse((end, i))) = open.peek()
                && end <= at
            {
                open.pop();
                mono -= usize::from(runs[i].mono);
                scaled.remove(&i);
            }
            let scale = scaled.last().and_then(|&i| runs[i].scale).unwrap_or(1.0);
            let spacing = fonts.emoji_spacing(base.size * scale);
            (range, base.mono || mono > 0, spacing)
        })
        .collect()
}
