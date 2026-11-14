//! Reparsing only the blocks an edit touched.
//!
//! The new text is parsed from a cut before the edit to a cut after it. A cut is the start
//! of a top-level unit that follows a blank line and cannot continue a container (the rule
//! `prefix` uses), so the blocks on either side parse the same alone as in the whole
//! document. Units outside the window are kept and shifted to their new offsets, and
//! heading slugs are numbered again over the whole document. Link reference definitions
//! and footnotes tie blocks together across the document, and pulldown-cmark reads front
//! matter wherever a `---` line has a closing line after it. With any of them in play
//! `reparse` returns `None` and the caller parses everything, as it does when no cut is
//! near enough.

use std::borrow::Cow;
use std::ops::Range;

use crate::build::{collect_headings, parse};
use crate::model::*;
use crate::prefix::{has_metadata_delimiter, starts_new_top_level_block};
use crate::slug::Slugger;

/// The document text before or after an edit.
pub trait Text {
    fn len_bytes(&self) -> usize;
    /// The text in `range`, which lies on char boundaries.
    fn slice(&self, range: Range<usize>) -> Cow<'_, str>;
    /// The line holding byte `at` (`at < len_bytes`), with its line break.
    fn line(&self, at: usize) -> Range<usize>;
}

impl Text for str {
    fn len_bytes(&self) -> usize {
        self.len()
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        Cow::Borrowed(&self[range])
    }

    fn line(&self, at: usize) -> Range<usize> {
        let bytes = self.as_bytes();
        let start = bytes[..at]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let end = bytes[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| at + i + 1);
        start..end
    }
}

/// An edit: bytes `start..old_end` of the old text became `start..new_end` of the new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

/// New units for a run of old ones; apply it with [`BlockModel::apply`].
#[derive(Debug)]
pub struct Patch {
    /// The old units the new ones replace.
    pub replaced: Range<usize>,
    pub units: Vec<LayoutUnit>,
    /// What the edit added to the offsets of the units after `replaced`.
    pub delta: isize,
}

/// Where each old unit is after a patch (see [`Lineage::get`]).
#[derive(Debug, Clone, Copy)]
pub struct Lineage {
    /// Units before this kept their index: the units before the window and the ones at its
    /// start whose hash did not change.
    kept_before: usize,
    /// Old units from here on moved to `i - old_tail + new_tail`.
    old_tail: usize,
    new_tail: usize,
    /// The window holds as many units as before, so they map one to one (an equation
    /// being typed in).
    in_place: bool,
}

impl Lineage {
    /// The index in the patched model of old unit `i`, or `None` when it was replaced.
    pub fn get(&self, i: usize) -> Option<usize> {
        if i < self.kept_before || (self.in_place && i < self.old_tail) {
            Some(i)
        } else if i >= self.old_tail {
            Some(i - self.old_tail + self.new_tail)
        } else {
            None
        }
    }

    /// The units the patch changed: their index range in the old model, then in the new
    /// one. Every other unit has the same content as before, at the index `get` gives.
    pub fn changed(&self) -> (Range<usize>, Range<usize>) {
        (
            self.kept_before..self.old_tail,
            self.kept_before..self.new_tail,
        )
    }
}

/// Parses the part of `new` that `change` touched, or returns `None` when the whole text
/// must be parsed: the document has cross-references, an edit opened a block that runs past
/// the window (a fence), or the window would be longer than `max_window` bytes. `model` is
/// a full parse of `old`.
pub fn reparse<O, N>(
    model: &BlockModel,
    old: &O,
    new: &N,
    change: Change,
    max_window: usize,
) -> Option<Patch>
where
    O: Text + ?Sized,
    N: Text + ?Sized,
{
    if model.cross_references {
        return None;
    }
    let units = &model.units;
    let delta = change.new_end as isize - change.old_end as isize;
    // The window starts at the last cut at or before the edit. The text before it is the
    // same in both versions.
    let mut i = units.partition_point(|u| u.range.start <= change.start);
    let (first, start) = loop {
        if i == 0 {
            break (0, 0);
        }
        i -= 1;
        let at = units[i].range.start;
        if change.start - at > max_window {
            return None;
        }
        if top_level(units[i].kind) && is_cut(new, at) {
            break (i, at);
        }
    };
    // It ends at the first unit after the edit that starts at a cut in both versions: the
    // old unit's parse did not depend on what came before it, and neither will the new one.
    let mut j = units.partition_point(|u| u.range.start < change.old_end);
    let (last, old_end, end) = loop {
        let Some(u) = units.get(j) else {
            break (units.len(), old.len_bytes(), new.len_bytes());
        };
        let at = u.range.start;
        if at - change.old_end > max_window {
            return None;
        }
        let new_at = at.checked_add_signed(delta)?;
        if top_level(u.kind) && is_cut(old, at) && is_cut(new, new_at) {
            break (j, at, new_at);
        }
        j += 1;
    };
    if end - start > max_window {
        return None;
    }
    let mut text = new.slice(start..end).into_owned();
    // pulldown-cmark looks for the line that closes front matter in all the text after it,
    // even past the end of the quote or list holding it, so a delimiter line in the window,
    // before or after the edit, can change blocks on either side.
    if has_metadata_delimiter(&text) || has_metadata_delimiter(&old.slice(start..old_end)) {
        return None;
    }
    // A paragraph stands in for the block at the cut. If the parse does not end with it
    // as a unit of its own, a block before it is still open (a fence the edit opened, or
    // one whose closing line it deleted) and would run on past the cut.
    let stand_in = (last < units.len()).then_some(text.len());
    if stand_in.is_some() {
        text.push_str("x\n");
    }
    let mut window = parse(&text);
    if window.cross_references {
        return None;
    }
    if let Some(at) = stand_in {
        match window.units.pop() {
            Some(u) if u.kind == UnitKind::Paragraph && u.range.start == at => {}
            _ => return None,
        }
    }
    for u in &mut window.units {
        u.shift(start as isize);
    }
    Some(Patch {
        replaced: first..last,
        units: window.units,
        delta,
    })
}

/// Units that are a whole top-level block (not part of a split list or blockquote, nor the
/// footnotes gathered at the end).
fn top_level(kind: UnitKind) -> bool {
    !matches!(
        kind,
        UnitKind::ListItem | UnitKind::BlockQuoteChild | UnitKind::Footnotes
    )
}

/// Whether a block starting at `at` parses the same alone as after the text before it: it
/// starts a non-blank line after a blank line, and the line cannot continue a list,
/// blockquote or indented code. A `---` or `+++` line is not a cut, since it would open front matter at
/// the start of the window.
fn is_cut<T: Text + ?Sized>(t: &T, at: usize) -> bool {
    if at == 0 {
        return true;
    }
    if at >= t.len_bytes() {
        return false;
    }
    let prev = t.line(at - 1);
    if prev.end != at {
        return false;
    }
    let (prev, line) = (t.slice(prev), t.slice(t.line(at)));
    let text = line.trim_end_matches(['\n', '\r']);
    // pulldown-cmark also ends a line at a lone `\r`; lines here end at `\n`.
    let lone_cr = |s: &str| {
        s.trim_end_matches('\n')
            .trim_end_matches('\r')
            .contains('\r')
    };
    prev.trim().is_empty()
        && !lone_cr(&prev)
        && !lone_cr(&line)
        && !text.trim().is_empty()
        && starts_new_top_level_block(text)
        && !matches!(text.trim_end(), "---" | "+++")
}

impl BlockModel {
    /// Replaces the patched units, moves the ones after them and numbers the heading slugs
    /// again. Returns where each old unit went.
    pub fn apply(&mut self, patch: Patch) -> Lineage {
        let Patch {
            replaced,
            units,
            delta,
        } = patch;
        let old = &self.units[replaced.clone()];
        let same_start = old
            .iter()
            .zip(&units)
            .take_while(|(a, b)| a.hash == b.hash)
            .count();
        let same_end = old[same_start..]
            .iter()
            .rev()
            .zip(units[same_start..].iter().rev())
            .take_while(|(a, b)| a.hash == b.hash)
            .count();
        let new_end = replaced.start + units.len();
        let lineage = Lineage {
            kept_before: replaced.start + same_start,
            old_tail: replaced.end - same_end,
            new_tail: new_end - same_end,
            in_place: units.len() == replaced.len(),
        };
        let moved = new_end as isize - replaced.end as isize;
        for u in &mut self.units[replaced.end..] {
            u.shift(delta);
        }
        self.units.splice(replaced.clone(), units);

        let mut headings = Vec::with_capacity(self.headings.len());
        let mut old = std::mem::take(&mut self.headings).into_iter().peekable();
        while let Some(h) = old.next_if(|h| h.unit < replaced.start) {
            headings.push(h);
        }
        for (i, u) in self.units[replaced.start..new_end].iter().enumerate() {
            collect_headings(&u.block, replaced.start + i, &mut headings);
        }
        headings.extend(old.skip_while(|h| h.unit < replaced.end).map(|mut h| {
            h.unit = h.unit.wrapping_add_signed(moved);
            shift(&mut h.range, delta);
            h
        }));
        let mut slugger = Slugger::default();
        for h in &mut headings {
            let slug = slugger.slug(&h.text);
            if slug != h.slug {
                set_slug(&mut self.units[h.unit].block, &h.range, &slug);
                h.slug = slug;
            }
        }
        self.headings = headings;
        lineage
    }
}

/// Sets the slug of the heading at `range` in `block`'s tree.
fn set_slug(block: &mut Block, range: &Range<usize>, new: &str) -> bool {
    match &mut block.kind {
        BlockKind::Heading { slug, .. } if block.range == *range => {
            new.clone_into(slug);
            true
        }
        BlockKind::BlockQuote { children, .. } => {
            children.iter_mut().any(|c| set_slug(c, range, new))
        }
        BlockKind::List { items, .. } => items
            .iter_mut()
            .flat_map(|i| &mut i.children)
            .any(|c| set_slug(c, range, new)),
        _ => false,
    }
}

fn shift(r: &mut Range<usize>, by: isize) {
    r.start = r.start.wrapping_add_signed(by);
    r.end = r.end.wrapping_add_signed(by);
}

impl LayoutUnit {
    /// Moves every source offset by `by` bytes.
    fn shift(&mut self, by: isize) {
        if by != 0 {
            shift(&mut self.range, by);
            self.block.shift(by);
        }
    }
}

impl Block {
    fn shift(&mut self, by: isize) {
        shift(&mut self.range, by);
        match &mut self.kind {
            BlockKind::Heading { content, .. } | BlockKind::Paragraph { content, .. } => {
                content.shift(by)
            }
            BlockKind::List { items, .. } => {
                for item in items {
                    shift(&mut item.range, by);
                    if let Some(task) = &mut item.task {
                        shift(&mut task.range, by);
                    }
                    item.children.iter_mut().for_each(|c| c.shift(by));
                }
            }
            BlockKind::BlockQuote { children, .. } => children.iter_mut().for_each(|c| c.shift(by)),
            BlockKind::Table { rows, .. } => rows.iter_mut().flatten().for_each(|c| c.shift(by)),
            BlockKind::Footnotes { notes } => {
                for n in notes {
                    shift(&mut n.range, by);
                    n.children.iter_mut().for_each(|c| c.shift(by));
                }
            }
            BlockKind::Code { .. }
            | BlockKind::Math { .. }
            | BlockKind::Rule
            | BlockKind::Html { .. }
            | BlockKind::FrontMatter { .. } => {}
        }
    }
}

impl Inline {
    /// Moves the source side of the map and of each object; rendered offsets stay.
    fn shift(&mut self, by: isize) {
        self.map.iter_mut().for_each(|s| shift(&mut s.source, by));
        self.objects
            .iter_mut()
            .for_each(|o| shift(&mut o.source, by));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind of block, including ones a cut must not split or start: a list long
    /// enough to be split into units, fences, HTML blocks, indented code, setext headings,
    /// `---` rules, tables, display math and duplicate headings.
    fn document() -> String {
        let mut s =
            String::from("---\ntitle: t\n---\n\n# Title\n\nIntro with $x^2$ and **bold**.\n\n");
        for i in 0..40 {
            s.push_str(&format!("- item {i}\n"));
        }
        s.push_str("\nAfter the list.\n\n> quote\n> more\n\n```rust\nfn main() {}\n\n# not a heading\n```\n\n");
        s.push_str("<div>\nhtml\n</div>\n\n<!-- a\n\ncomment -->\n\n    indented\n    code\n\n");
        s.push_str("Setext\n---\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n$$\ny = mx\n$$\n\n");
        for i in 0..12 {
            s.push_str(&format!(
                "## Same\n\nParagraph {i} with `code` and a [link](u).\n\n"
            ));
        }
        s.push_str("1. one\n2. two\n\n   still two\n\nTail.\n");
        s
    }

    const SNIPPETS: &[&str] = &[
        "x",
        "\n",
        "\n\n",
        "```\n",
        "~~~",
        "# ",
        "## Same\n",
        "- ",
        "1. ",
        "> ",
        "    ",
        "$$",
        "$",
        "---\n",
        "+++\n",
        "<div>\n",
        "<!--",
        "-->",
        "|",
        "| c |\n|---|\n",
        "**",
        "\r\n",
        "é",
        "[^1]",
        "[r]: /u\n",
        "[^n]: note\n",
        "Setext\n===\n",
        "\n- a\n- b\n",
        "<script>\n",
        "</script>",
        "  - nested\n",
        "\t",
        "***\n",
        "> - ",
        "```math\n",
        "<pre>",
        "- [ ] ",
        "\\",
        "_",
        "`",
        "  \n",
        "\n\n    ",
        "<?",
        "?>",
        "~~~\n",
        "\n\n- ",
        "\n\n> ",
        "| a |\n",
        "...\n",
        "````\n",
        "\n\n<div>",
    ];

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }

        fn boundary(&mut self, s: &str) -> usize {
            let mut at = self.below(s.len() + 1);
            while !s.is_char_boundary(at) {
                at -= 1;
            }
            at
        }
    }

    #[test]
    fn matches_a_full_parse_after_random_edits() {
        let seeds: u64 = std::env::var("MDT_REPARSE_SEEDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        for seed in 0..seeds {
            random_edits(0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0xa076_1d64_78bd_642f));
        }
    }

    fn random_edits(seed: u64) {
        let mut rng = Rng(seed);
        let mut text = document();
        let mut model = parse(&text);
        let (mut patched, mut edits) = (0, 0);
        for _ in 0..4000 {
            let start = rng.boundary(&text);
            let mut end = start;
            if rng.below(3) == 0 {
                end = (start + rng.below(40)).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
            }
            let insert = if rng.below(4) == 0 && end > start {
                ""
            } else {
                SNIPPETS[rng.below(SNIPPETS.len())]
            };
            let mut new = text.clone();
            new.replace_range(start..end, insert);
            let change = Change {
                start,
                old_end: end,
                new_end: start + insert.len(),
            };
            let full = parse(&new);
            if let Some(patch) = reparse(&model, text.as_str(), new.as_str(), change, 4096) {
                let replaced = patch.replaced.clone();
                model.apply(patch);
                if model != full {
                    let i = (0..model.units.len().max(full.units.len()))
                        .find(|&i| model.units.get(i) != full.units.get(i));
                    panic!(
                        "after replacing {start}..{end} of {:?} with {insert:?} (units \
                         {replaced:?} reparsed): unit {i:?} is\n{:#?}\nnot\n{:#?}\n\
                         headings equal: {}",
                        &text[start.saturating_sub(120)..(end + 80).min(text.len())],
                        i.and_then(|i| model.units.get(i)),
                        i.and_then(|i| full.units.get(i)),
                        model.headings == full.headings,
                    );
                }
                patched += 1;
            }
            edits += 1;
            model = full;
            text = new;
            // Cross-references turn the incremental path off; drop them now and then so
            // most edits test it.
            if model.cross_references && rng.below(8) == 0 {
                text = document();
                model = parse(&text);
            }
        }
        assert!(
            patched * 3 > edits,
            "only {patched} of {edits} edits patched"
        );
    }

    #[test]
    fn lineage_follows_units_around_the_window() {
        let text = "# A\n\none\n\ntwo\n\nthree\n";
        let mut model = parse(text);
        let new = "# A\n\none\n\ntwo\n\nx\n\nthree\n";
        let change = Change {
            start: 15,
            old_end: 15,
            new_end: 18,
        };
        let lineage = model.apply(reparse(&model, text, new, change, 1024).expect("patched"));
        assert_eq!(model, parse(new));
        // "three" moved down one place; the units before it kept theirs.
        assert_eq!(
            (0..4).map(|i| lineage.get(i)).collect::<Vec<_>>(),
            [Some(0), Some(1), Some(2), Some(4)]
        );
    }
}
