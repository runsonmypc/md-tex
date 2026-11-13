//! What each equation shows while it is typeset in the background: its scene, the last
//! good scene while a new source is pending or has only just failed, or its error once the
//! source has gone `ERROR_DELAY` without changes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mdt_math::{Key, MathScene};

pub const ERROR_DELAY: Duration = Duration::from_millis(500);

/// Fonts for typesetting, in the order scenes refer to them.
pub static FONTS: &[&[u8]] = &[mdt_text::fonts::MATH_FONT];

#[derive(Debug, Clone)]
pub enum Shown {
    Scene(Arc<MathScene>),
    Error(String),
    /// Not typeset yet and nothing to show in the meantime.
    Pending,
}

/// An equation's place in the document: (unit index, math ordinal within the unit). It
/// stays put while the equation's source is edited, so the last good scene can be kept, and
/// moves with its unit (`follow_units`).
pub type Slot = (usize, usize);

struct SlotState {
    key: Key,
    /// When the source last changed.
    since: Instant,
    good: Option<Arc<MathScene>>,
}

pub struct MathView {
    pub cache: mdt_math::Cache,
    slots: HashMap<Slot, SlotState>,
    pending: Vec<(String, bool)>,
    inflight: HashSet<Key>,
    /// The time layout runs at. The owner sets it before laying out; tests drive it.
    pub now: Instant,
    /// When the earliest held-back error is due; the owner lays out again then.
    pub due: Option<Instant>,
}

impl Default for MathView {
    fn default() -> Self {
        Self {
            cache: mdt_math::Cache::new(FONTS),
            slots: HashMap::new(),
            pending: Vec::new(),
            inflight: HashSet::new(),
            now: Instant::now(),
            due: None,
        }
    }
}

impl MathView {
    pub fn resolve(&mut self, slot: Slot, latex: &str, display: bool) -> Shown {
        let key = mdt_math::key(latex, display);
        let now = self.now;
        let s = self.slots.entry(slot).or_insert(SlotState {
            key,
            since: now,
            good: None,
        });
        if s.key != key {
            s.key = key;
            s.since = now;
        }
        let held = || s.good.clone().map_or(Shown::Pending, Shown::Scene);
        match self.cache.get(latex, display) {
            Some(Ok(scene)) => {
                s.good = Some(scene.clone());
                Shown::Scene(scene)
            }
            Some(Err(e)) => {
                let due = s.since + ERROR_DELAY;
                if now < due {
                    self.due = Some(self.due.map_or(due, |d| d.min(due)));
                    held()
                } else {
                    Shown::Error(e.message)
                }
            }
            None => {
                let shown = held();
                if self.inflight.insert(key) {
                    self.pending.push((latex.to_owned(), display));
                }
                shown
            }
        }
    }

    /// Moves the slots with their units after a reparse: `follow` maps an old unit index
    /// to the new one (`None` when the unit is gone), so a slot never shows the last good
    /// scene of another unit's equation.
    pub fn follow_units(&mut self, follow: &dyn Fn(usize) -> Option<usize>) {
        self.slots = std::mem::take(&mut self.slots)
            .into_iter()
            .filter_map(|((unit, n), v)| Some(((follow(unit)?, n), v)))
            .collect();
    }

    /// Equations waiting to be typeset; the caller runs `Cache::compute_batch` in the
    /// background, then calls `landed` with their keys and `Reader::content_landed`.
    pub fn take_pending(&mut self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.pending)
    }

    /// A batch is in the cache: its equations may be requested again, which they are if
    /// the cache (bounded by bytes) evicts them later.
    pub fn landed(&mut self, keys: &[Key]) {
        for k in keys {
            self.inflight.remove(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_scene(s: &Shown) -> bool {
        matches!(s, Shown::Scene(_))
    }

    #[test]
    fn error_shows_only_after_500_ms_without_edits() {
        let mut m = MathView::default();
        let t0 = Instant::now();
        m.now = t0;
        assert!(matches!(m.resolve((0, 0), "x", true), Shown::Pending));
        for (latex, display) in m.take_pending() {
            let _ = m.cache.compute(&latex, display);
        }
        assert!(is_scene(&m.resolve((0, 0), "x", true)));

        // The source becomes invalid: the last good scene stays while it is fresh.
        m.now = t0 + Duration::from_millis(1000);
        let bad = r"\foo{x}";
        assert!(is_scene(&m.resolve((0, 0), bad, true)), "pending keeps x");
        for (latex, display) in m.take_pending() {
            let _ = m.cache.compute(&latex, display);
        }
        m.now = t0 + Duration::from_millis(1400);
        assert!(is_scene(&m.resolve((0, 0), bad, true)), "error held back");
        assert_eq!(m.due, Some(t0 + Duration::from_millis(1500)));
        m.now = t0 + Duration::from_millis(1500);
        match m.resolve((0, 0), bad, true) {
            Shown::Error(e) => assert!(e.contains(r"\foo"), "{e}"),
            other => panic!("expected the error, got {other:?}"),
        }

        // Another edit restarts the delay; the last good scene is still x.
        m.now = t0 + Duration::from_millis(1600);
        let bad2 = r"\foo{y}";
        let _ = m.cache.compute(bad2, true);
        assert!(is_scene(&m.resolve((0, 0), bad2, true)));
        // A different equation is unaffected.
        let _ = m.cache.compute("y", false);
        assert!(is_scene(&m.resolve((1, 0), "y", false)));
    }

    #[test]
    fn evicted_equations_are_typeset_again() {
        let mut m = MathView {
            cache: mdt_math::Cache::with_capacity(FONTS, 1),
            ..Default::default()
        };
        let typeset = |m: &mut MathView| {
            let batch = m.take_pending();
            m.cache.compute_batch(&batch);
            let keys: Vec<Key> = batch.iter().map(|(l, d)| mdt_math::key(l, *d)).collect();
            m.landed(&keys);
            batch.len()
        };
        assert!(matches!(m.resolve((0, 0), "x", false), Shown::Pending));
        assert_eq!(typeset(&mut m), 1);
        assert!(is_scene(&m.resolve((0, 0), "x", false)));
        // Typesetting another equation evicts x from the one-byte cache.
        m.resolve((1, 0), "y", false);
        assert_eq!(typeset(&mut m), 1);
        assert!(m.cache.get("x", false).is_none(), "x evicted");
        m.resolve((0, 0), "x", false);
        assert_eq!(typeset(&mut m), 1, "x requested again");
    }

    #[test]
    fn slots_follow_their_unit() {
        let mut m = MathView::default();
        let _ = m.cache.compute("x", true);
        let _ = m.cache.compute(r"\foo", true);
        assert!(is_scene(&m.resolve((0, 0), "x", true)));
        // A failing equation was pasted above: it does not show x while its error waits.
        m.follow_units(&|u| Some(u + 1));
        assert!(matches!(m.resolve((0, 0), r"\foo", true), Shown::Pending));
        assert!(is_scene(&m.resolve((1, 0), "x", true)));
    }

    #[test]
    fn a_new_equation_that_fails_waits_too() {
        let mut m = MathView::default();
        let t0 = Instant::now();
        m.now = t0;
        let _ = m.cache.compute(r"\frac{a}{", true);
        let _ = m.cache.compute(r"\foo", true);
        assert!(matches!(m.resolve((0, 0), r"\foo", true), Shown::Pending));
        m.now = t0 + ERROR_DELAY;
        assert!(matches!(m.resolve((0, 0), r"\foo", true), Shown::Error(_)));
    }
}
