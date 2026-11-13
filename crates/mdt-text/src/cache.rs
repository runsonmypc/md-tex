//! Layout cache keyed by (unit hash, width bucket, zoom, font-set version).
//! Entries remember their unit index so the cache can drop units far from the viewport,
//! and layouts a unit no longer uses.

use std::collections::HashMap;
use std::ops::Range;

/// Widths within the same 8 px bucket share a layout.
pub const WIDTH_BUCKET: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub unit_hash: u64,
    pub width_bucket: u32,
    /// Zoom in percent.
    pub zoom: u32,
    pub font_version: u64,
}

impl CacheKey {
    pub fn new(unit_hash: u64, width: f32, zoom: f32, font_version: u64) -> Self {
        Self {
            unit_hash,
            width_bucket: (width.max(0.0) / WIDTH_BUCKET) as u32,
            zoom: (zoom * 100.0).round() as u32,
            font_version,
        }
    }

    /// The width a layout for this key is made at: the bucket's lower edge, so every
    /// width in the bucket fits it.
    pub fn layout_width(&self) -> f32 {
        self.width_bucket as f32 * WIDTH_BUCKET
    }
}

pub struct LayoutCache<T> {
    /// Each entry's unit index and when it was last inserted or touched.
    entries: HashMap<CacheKey, (usize, u64, T)>,
    tick: u64,
}

impl<T> Default for LayoutCache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            tick: 0,
        }
    }
}

impl<T> LayoutCache<T> {
    pub fn get(&self, key: &CacheKey) -> Option<&T> {
        self.entries.get(key).map(|(_, _, v)| v)
    }

    /// `unit` is the unit's current index in the document. Its earlier layouts are dropped
    /// by `retain_units`, not here: until the frame's lookups have moved the other
    /// entries to their new indices (`touch`), an entry at `unit` may be the layout of
    /// a unit that a new one above it pushed down.
    pub fn insert(&mut self, key: CacheKey, unit: usize, value: T) {
        self.tick += 1;
        self.entries.insert(key, (unit, self.tick, value));
    }

    /// Records that a cached layout now belongs to a different unit index (after edits
    /// above it shift units).
    pub fn touch(&mut self, key: &CacheKey, unit: usize) {
        if let Some(e) = self.entries.get_mut(key) {
            self.tick += 1;
            e.0 = unit;
            e.1 = self.tick;
        }
    }

    /// Drops every entry whose unit is outside `keep` (the units within ±3 viewports) or
    /// that was made for another width bucket, zoom or font set than `current` (whose
    /// unit hash is ignored), and of several entries for one unit all but the newest
    /// (the others are its old contents). Without the width rule a resize, zoom or font
    /// change would leave the old layouts of every unit near the viewport behind.
    pub fn retain_units(&mut self, keep: Range<usize>, current: &CacheKey) {
        self.entries.retain(|k, (unit, _, _)| {
            keep.contains(unit)
                && (k.width_bucket, k.zoom, k.font_version)
                    == (current.width_bucket, current.zoom, current.font_version)
        });
        let mut newest: HashMap<usize, u64> = HashMap::new();
        for &(unit, tick, _) in self.entries.values() {
            let t = newest.entry(unit).or_insert(tick);
            *t = (*t).max(tick);
        }
        self.entries
            .retain(|_, (unit, tick, _)| newest.get(unit) == Some(tick));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_on_same_key_and_same_bucket() {
        let mut c = LayoutCache::default();
        c.insert(CacheKey::new(1, 640.0, 1.0, 1), 0, "a");
        assert_eq!(c.get(&CacheKey::new(1, 640.0, 1.0, 1)), Some(&"a"));
        assert_eq!(
            c.get(&CacheKey::new(1, 647.9, 1.0, 1)),
            Some(&"a"),
            "same 8 px bucket"
        );
    }

    #[test]
    fn miss_on_width_zoom_hash_or_font_change() {
        let mut c = LayoutCache::default();
        c.insert(CacheKey::new(1, 640.0, 1.0, 1), 0, "a");
        assert_eq!(
            c.get(&CacheKey::new(1, 648.0, 1.0, 1)),
            None,
            "next width bucket"
        );
        assert_eq!(c.get(&CacheKey::new(1, 640.0, 1.1, 1)), None, "zoom");
        assert_eq!(c.get(&CacheKey::new(2, 640.0, 1.0, 1)), None, "unit hash");
        assert_eq!(c.get(&CacheKey::new(1, 640.0, 1.0, 2)), None, "font set");
    }

    #[test]
    fn layout_width_fits_every_width_in_bucket() {
        let k = CacheKey::new(1, 645.0, 1.0, 1);
        assert_eq!(k.layout_width(), 640.0);
    }

    #[test]
    fn eviction_keeps_units_within_three_viewports() {
        // 100 units of 100 px each; viewport 200 px at y = 5000 (unit 50): 3 viewports
        // above (4400) through 4 below the top (5800).
        let keep = 43..58;
        let mut c = LayoutCache::default();
        for u in 0..100 {
            c.insert(CacheKey::new(u as u64, 640.0, 1.0, 1), u, u);
        }
        c.retain_units(keep, &CacheKey::new(0, 640.0, 1.0, 1));
        assert_eq!(c.len(), 15);
        assert!(c.get(&CacheKey::new(50, 640.0, 1.0, 1)).is_some());
        assert!(c.get(&CacheKey::new(42, 640.0, 1.0, 1)).is_none());
        assert!(c.get(&CacheKey::new(58, 640.0, 1.0, 1)).is_none());
    }

    #[test]
    fn stale_layouts_do_not_accumulate() {
        let mut c = LayoutCache::default();
        // A unit edited, resized, zoomed and re-laid out for new fonts: one entry each
        // frame.
        for v in 0..50u64 {
            let width = 600.0 + (v % 7) as f32 * WIDTH_BUCKET;
            let zoom = 1.0 + (v % 3) as f32 * 0.1;
            c.insert(CacheKey::new(v, width, zoom, v), 3, "unit 3");
            c.insert(CacheKey::new(1000, width, zoom, v), 4, "unit 4");
            c.retain_units(0..10, &CacheKey::new(0, width, zoom, v));
            assert_eq!(c.len(), 2);
        }
        // An edited unit, still within the same width, zoom and fonts.
        for v in 0..50u64 {
            c.insert(CacheKey::new(2000 + v, 600.0, 1.0, 49), 3, "unit 3");
            c.retain_units(0..10, &CacheKey::new(0, 600.0, 1.0, 49));
            assert!(c.len() <= 2, "{} entries", c.len());
        }
        // Two units with the same content share an entry, whichever laid it out.
        let mut c = LayoutCache::default();
        let shared = CacheKey::new(7, 640.0, 1.0, 1);
        c.insert(shared, 0, "same");
        c.touch(&shared, 2);
        c.insert(CacheKey::new(8, 640.0, 1.0, 1), 1, "other");
        assert_eq!(c.len(), 2);
        // Keys for an old width, zoom or font set are dropped; other contents are kept.
        let mut c = LayoutCache::default();
        c.insert(CacheKey::new(1, 640.0, 1.0, 1), 0, "old width");
        c.insert(CacheKey::new(2, 648.0, 1.0, 1), 1, "current");
        c.insert(CacheKey::new(3, 648.0, 1.0, 1), 2, "current");
        c.insert(CacheKey::new(4, 648.0, 1.25, 1), 3, "old zoom");
        c.insert(CacheKey::new(5, 648.0, 1.0, 0), 4, "old fonts");
        c.retain_units(0..10, &CacheKey::new(0, 650.0, 1.0, 1));
        assert_eq!(c.len(), 2);
    }

    /// A unit inserted near the top shifts every unit below it by one. Lookups find
    /// their layouts by content and move them to the new indices; only the new unit is
    /// laid out.
    #[test]
    fn inserting_a_unit_lays_out_only_that_unit() {
        let key = |hash: u64| CacheKey::new(hash, 640.0, 1.0, 1);
        // One frame as the reader runs it: look up each visible unit (or lay it out),
        // then prune. Returns the units laid out.
        let frame = |c: &mut LayoutCache<u64>, units: &[u64]| {
            let mut misses = 0;
            for (i, &h) in units.iter().enumerate() {
                if c.get(&key(h)).is_some() {
                    c.touch(&key(h), i);
                } else {
                    c.insert(key(h), i, h);
                    misses += 1;
                }
            }
            c.retain_units(0..units.len(), &key(0));
            misses
        };
        let mut units: Vec<u64> = (0..40).collect();
        let mut c = LayoutCache::default();
        assert_eq!(frame(&mut c, &units), 40);
        units.insert(5, 1000);
        assert_eq!(frame(&mut c, &units), 1);
        assert_eq!(frame(&mut c, &units), 0);
        assert_eq!(c.len(), 41);
    }
}
