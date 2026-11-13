//! Find and replace. Source mode searches the raw text
//! of each layout unit; Reader mode searches each unit's rendered texts, so markup is not
//! matched, and a match inside an equation's or diagram's source selects the whole object.
//! Matches never cross a block boundary.

use std::ops::Range;

use mdt_doc::search::{find_in_blocks, find_in_str};
use mdt_markdown::{BlockKind, BlockModel, LayoutUnit, OBJECT_CHAR, UnitKind};
use ropey::Rope;

use crate::field::Field;
use crate::reader::view::block_texts;

/// A match. In source search `range` is a byte range of the document and `text` is 0; in
/// rendered search it is a byte range of the unit's `text`-th selectable text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub unit: usize,
    pub text: usize,
    pub range: Range<usize>,
}

/// The source ranges searched for a unit: its own, or for the footnotes section (which
/// sits at the end with an empty range) each footnote's.
fn unit_ranges(unit: &LayoutUnit) -> Vec<Range<usize>> {
    match (&unit.kind, &unit.block.kind) {
        (UnitKind::Footnotes, BlockKind::Footnotes { notes }) => {
            notes.iter().map(|n| n.range.clone()).collect()
        }
        _ => vec![unit.range.clone()],
    }
}

pub fn source_hits(rope: &Rope, model: &BlockModel, query: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    for (i, unit) in model.units.iter().enumerate() {
        out.extend(
            find_in_blocks(rope, query, &unit_ranges(unit))
                .into_iter()
                .map(|range| Hit {
                    unit: i,
                    text: 0,
                    range,
                }),
        );
    }
    out
}

pub fn rendered_hits(source: &str, model: &BlockModel, query: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let object = OBJECT_CHAR.len_utf8();
    let mut texts = Vec::new();
    for (i, unit) in model.units.iter().enumerate() {
        texts.clear();
        block_texts(&unit.block, &mut texts);
        for (k, t) in texts.iter().enumerate() {
            let mut hits: Vec<Range<usize>> = find_in_str(&t.text, query);
            for (at, src) in &t.objects {
                if source
                    .get(src.clone())
                    .is_some_and(|s| !find_in_str(s, query).is_empty())
                {
                    hits.push(*at..*at + object);
                }
            }
            hits.sort_by_key(|r| r.start);
            out.extend(hits.into_iter().map(|range| Hit {
                unit: i,
                text: k,
                range,
            }));
        }
    }
    out
}

pub struct Find {
    pub query: Field,
    /// The replace field, shown in Source and Split only.
    pub replace: Option<Field>,
    /// The caret is in the replace field.
    pub in_replace: bool,
    /// Whether `hits` are rendered-text matches.
    pub rendered: bool,
    pub hits: Vec<Hit>,
    pub current: Option<usize>,
    /// Where the search started: a source offset, or a unit for rendered search. The first
    /// match at or after it becomes current as the query changes.
    pub origin: usize,
    /// (query, rendered, model epoch, text generation) the hits were computed for. The
    /// text generation changes with every edit, so typing clears source hits even while
    /// the model (and its epoch) trails the text.
    key: Option<(String, bool, u64, u64)>,
    /// A source search is waiting for a model of the current text.
    pub waiting: bool,
    /// While waiting: the start of the match that was current and the document's length
    /// then, so the same match (shifted by the edit) becomes current again.
    resume: Option<(usize, usize)>,
}

impl Find {
    pub fn new(query: &str, replace: bool, rendered: bool, origin: usize) -> Self {
        Self {
            query: Field::new(query),
            replace: replace.then(Field::default),
            in_replace: false,
            rendered,
            hits: Vec::new(),
            current: None,
            origin,
            key: None,
            waiting: false,
            resume: None,
        }
    }

    /// Whether the query or the kind of search differs from the one the hits are for.
    pub fn query_changed(&self) -> bool {
        self.key
            .as_ref()
            .is_none_or(|k| k.0 != self.query.text || k.1 != self.rendered)
    }

    /// Recomputes the hits when the query, the kind of search, the model or the text
    /// changed; returns whether it did. A new query makes the first match at or after
    /// `origin` current; after a wait for the model, the match that was current (or the
    /// first after `caret`); otherwise the current index is kept in range.
    pub fn refresh(
        &mut self,
        rope: &Rope,
        model: &BlockModel,
        epoch: u64,
        text: u64,
        caret: usize,
    ) -> bool {
        let key = (self.query.text.clone(), self.rendered, epoch, text);
        if self.key.as_ref() == Some(&key) {
            return false;
        }
        let new_query = self.query_changed();
        let waited = std::mem::take(&mut self.waiting);
        let resume = self.resume.take();
        self.hits = if self.rendered {
            rendered_hits(&rope.to_string(), model, &self.query.text)
        } else {
            source_hits(rope, model, &self.query.text)
        };
        self.key = Some(key);
        let pos = |h: &Hit| if self.rendered { h.unit } else { h.range.start };
        let first_from = |at: usize| {
            let first = self.hits.iter().position(|h| pos(h) >= at);
            first.or((!self.hits.is_empty()).then_some(0))
        };
        if new_query {
            self.current = first_from(self.origin);
        } else if waited {
            // Edits shift the old match by the change in length when they were before
            // it, which the caret (just after the edit) tells.
            let at = resume.map_or(caret, |(at, len)| {
                let moved = (at + rope.len_bytes()).saturating_sub(len);
                if caret <= moved { moved } else { at }
            });
            self.current = first_from(at);
        } else {
            self.current = self
                .current
                .filter(|_| !self.hits.is_empty())
                .map(|c| c.min(self.hits.len() - 1));
        }
        true
    }

    /// `refresh` for a source search while the model trails the text (a background parse
    /// is running): its ranges point into the old text, so when a search is due the hits
    /// are cleared and it waits (`waiting`) for the new model, remembering the current
    /// match. The key is kept, so the search runs then.
    pub fn wait_for_model(&mut self, epoch: u64, text: u64, len: usize) -> bool {
        let key = (self.query.text.clone(), self.rendered, epoch, text);
        if self.key.as_ref() == Some(&key) {
            return false;
        }
        if let Some(h) = self.current_hit() {
            self.resume = Some((h.range.start, len));
        }
        self.hits.clear();
        self.current = None;
        self.waiting = true;
        true
    }

    /// Searches again from `origin`, as for a new query.
    pub fn restart(&mut self, origin: usize) {
        self.origin = origin;
        self.key = None;
    }

    /// Moves to the next (or previous) match, wrapping at the ends.
    pub fn step(&mut self, forward: bool) {
        let n = self.hits.len();
        if n == 0 {
            return;
        }
        self.current = Some(match (self.current, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(c), true) => (c + 1) % n,
            (Some(c), false) => (c + n - 1) % n,
        });
    }

    pub fn current_hit(&self) -> Option<&Hit> {
        self.current.and_then(|c| self.hits.get(c))
    }

    /// Whether a source hit still covers whole characters of `rope` that match the query.
    /// Replacing checks this before editing: a stale range would cut a character in two
    /// (ropey panics) or replace the wrong bytes.
    pub fn still_matches(&self, rope: &Rope, hit: &Hit) -> bool {
        rope.get_byte_slice(hit.range.clone()).is_some_and(|s| {
            let s = s.to_string();
            matches!(&find_in_str(&s, &self.query.text)[..], [r] if *r == (0..s.len()))
        })
    }

    /// "3 of 12", or "No results" for a query that matches nothing.
    pub fn status(&self) -> String {
        match (self.current, self.hits.len()) {
            (_, 0) if self.query.text.is_empty() => String::new(),
            // Not "No results": the matches are not known until the parse lands.
            (_, 0) if self.waiting => "Searching…".to_owned(),
            (_, 0) => "No results".to_owned(),
            (Some(c), n) => format!("{} of {n}", c + 1),
            (None, n) => format!("{n} matches"),
        }
    }

    /// The field the caret is in.
    pub fn field(&mut self) -> &mut Field {
        match &mut self.replace {
            Some(r) if self.in_replace => r,
            _ => &mut self.query,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(src: &str, q: &str) -> Vec<String> {
        let model = mdt_markdown::parse(src);
        let mut texts = Vec::new();
        rendered_hits(src, &model, q)
            .into_iter()
            .map(|h| {
                texts.clear();
                block_texts(&model.units[h.unit].block, &mut texts);
                texts[h.text].text[h.range].to_owned()
            })
            .collect()
    }

    fn source(src: &str, q: &str) -> Vec<String> {
        let model = mdt_markdown::parse(src);
        source_hits(&Rope::from_str(src), &model, q)
            .into_iter()
            .map(|h| src[h.range].to_owned())
            .collect()
    }

    #[test]
    fn reader_ignores_markup() {
        let src = "A **fast** path and a slow path.\n";
        assert_eq!(rendered(src, "fast path"), ["fast path"]);
        assert!(rendered(src, "**fast**").is_empty());
    }

    #[test]
    fn source_matches_markup() {
        let src = "A **fast** path.\n";
        assert_eq!(source(src, "**fast**"), ["**fast**"]);
        assert!(source(src, "fast path").is_empty());
    }

    #[test]
    fn match_inside_math_selects_the_whole_equation() {
        let src =
            "Let $\\alpha + \\beta$ be given.\n\n$$\n\\alpha^2\n$$\n\n```math\n\\alpha\n```\n";
        let obj = OBJECT_CHAR.to_string();
        assert_eq!(rendered(src, "\\alpha"), [obj.clone(), obj.clone(), obj]);
        assert!(rendered(src, "\\gamma").is_empty());
    }

    #[test]
    fn match_inside_diagram_source_selects_the_diagram() {
        let src = "```pikchr\nbox \"Reader\"\n```\n";
        assert_eq!(rendered(src, "reader"), [OBJECT_CHAR.to_string()]);
    }

    #[test]
    fn smart_case_and_block_boundaries() {
        let src = "GPU, Gpu and gpu.\n\nend of one\n\nstart\n";
        assert_eq!(rendered(src, "gpu").len(), 3);
        assert_eq!(rendered(src, "GPU"), ["GPU"]);
        assert!(rendered(src, "one start").is_empty());
        assert!(source(src, "one\n\nstart").is_empty());
    }

    #[test]
    fn footnotes_and_tables_are_searched() {
        let src =
            "Text.[^1]\n\n| a | latency |\n|---|---|\n| 1 | 2 |\n\n[^1]: Some latency note.\n";
        assert_eq!(rendered(src, "latency").len(), 2);
        assert_eq!(source(src, "latency").len(), 2);
    }

    #[test]
    fn source_search_waits_for_a_model_of_the_current_text() {
        let rope = Rope::from_str("x one x\n\ntwo x\n");
        let mut f = Find::new("x", false, false, 0);
        assert!(f.wait_for_model(0, 1, rope.len_bytes()));
        assert!(f.waiting && f.hits.is_empty() && f.current.is_none());
        let model = mdt_markdown::parse(&rope.to_string());
        assert!(f.refresh(&rope, &model, 1, 1, 0));
        assert!(!f.waiting);
        assert_eq!(f.hits.len(), 3);
        assert_eq!(f.current, Some(0));
    }

    #[test]
    fn an_edit_before_the_new_model_clears_the_hits() {
        let src = "colour é\n\ncolour é\n";
        let rope = Rope::from_str(src);
        let model = mdt_markdown::parse(src);
        let mut f = Find::new("colour", true, false, 0);
        assert!(f.refresh(&rope, &model, 1, 1, 0));
        assert_eq!(f.hits.len(), 2);
        // Typing moves the text generation on; the model (and its epoch) stays behind.
        assert!(f.wait_for_model(1, 2, rope.len_bytes()));
        assert!(f.waiting && f.hits.is_empty() && f.current_hit().is_none());
        assert_eq!(f.status(), "Searching…", "not \"No results\"");
    }

    #[test]
    fn the_current_match_survives_a_wait_for_the_model() {
        let src = "x one\n\ntwo x\n\nthree x\n";
        let mut rope = Rope::from_str(src);
        let mut f = Find::new("x", false, false, 0);
        assert!(f.refresh(&rope, &mdt_markdown::parse(src), 0, 0, 0));
        f.step(true);
        assert_eq!(f.status(), "2 of 3");
        // Typing before the match, with the caret after the typed text.
        f.wait_for_model(0, 1, rope.len_bytes());
        rope.insert(0, "ab");
        let model = mdt_markdown::parse(&rope.to_string());
        assert!(f.refresh(&rope, &model, 1, 1, 2));
        assert_eq!(f.status(), "2 of 3", "the same match, shifted");
        // Typing after it leaves it where it was.
        f.wait_for_model(1, 2, rope.len_bytes());
        rope.insert(rope.len_bytes(), "yz");
        let model = mdt_markdown::parse(&rope.to_string());
        assert!(f.refresh(&rope, &model, 2, 2, rope.len_bytes()));
        assert_eq!(f.status(), "2 of 3");
    }

    #[test]
    fn stale_ranges_no_longer_match() {
        let mut rope = Rope::from_str("colour é\n");
        let f = Find::new("colour", true, false, 0);
        let hit = |range| Hit {
            unit: 0,
            text: 0,
            range,
        };
        assert!(f.still_matches(&rope, &hit(0..6)));
        rope.insert(0, "abc");
        assert!(!f.still_matches(&rope, &hit(0..6)), "now \"abccol\"");
        assert!(!f.still_matches(&rope, &hit(9..11)), "splits the é");
        assert!(!f.still_matches(&rope, &hit(9..40)), "past the end");
        assert!(f.still_matches(&rope, &hit(3..9)));
    }

    #[test]
    fn query_changes_pick_the_first_match_after_the_origin_and_steps_wrap() {
        let src = "x one\n\ntwo x\n\nthree x\n";
        let model = mdt_markdown::parse(src);
        let rope = Rope::from_str(src);
        let mut f = Find::new("x", false, false, 3);
        assert!(f.refresh(&rope, &model, 0, 0, 0));
        assert_eq!(f.current, Some(1), "first at or after the origin");
        assert_eq!(f.status(), "2 of 3");
        assert!(!f.refresh(&rope, &model, 0, 0, 0), "unchanged");
        f.step(true);
        f.step(true);
        assert_eq!(f.current, Some(0), "wrapped to the first");
        f.step(false);
        assert_eq!(f.current, Some(2), "wrapped to the last");
        f.query.insert("yz");
        f.refresh(&rope, &model, 0, 0, 0);
        assert_eq!(f.status(), "No results");
    }
}
