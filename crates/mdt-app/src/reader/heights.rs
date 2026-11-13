//! Unit heights in a Fenwick tree, and a scroll position stored as an anchor (unit index,
//! offset within it) so height changes above the anchor never move what is on screen.

/// Heights of all layout units: measured for units laid out, estimated for the rest.
#[derive(Debug, Clone, Default)]
pub struct Heights {
    values: Vec<f32>,
    /// 1-based Fenwick tree over `values`, in f64 so long documents keep precision.
    tree: Vec<f64>,
}

impl Heights {
    pub fn new(values: Vec<f32>) -> Self {
        let n = values.len();
        let mut tree = vec![0.0; n + 1];
        for (i, &v) in values.iter().enumerate() {
            tree[i + 1] += v as f64;
            let parent = (i + 1) + (i + 1).isolate_lowest_one();
            if parent <= n {
                tree[parent] += tree[i + 1];
            }
        }
        Self { values, tree }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Replaces the entries in `range` with `values` (lines inserted or removed) and
    /// rebuilds the index.
    pub fn splice(&mut self, range: std::ops::Range<usize>, values: impl IntoIterator<Item = f32>) {
        let mut v = std::mem::take(&mut self.values);
        v.splice(range, values);
        *self = Self::new(v);
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, i: usize) -> f32 {
        self.values[i]
    }

    pub fn set(&mut self, i: usize, h: f32) {
        let delta = (h - self.values[i]) as f64;
        if delta == 0.0 {
            return;
        }
        self.values[i] = h;
        let mut j = i + 1;
        while j < self.tree.len() {
            self.tree[j] += delta;
            j += j.isolate_lowest_one();
        }
    }

    /// Sum of heights of units `0..i` = the top of unit `i`.
    pub fn top(&self, i: usize) -> f64 {
        let mut sum = 0.0;
        let mut j = i.min(self.values.len());
        while j > 0 {
            sum += self.tree[j];
            j &= j - 1;
        }
        sum
    }

    pub fn total(&self) -> f64 {
        self.top(self.values.len())
    }

    /// The unit containing document y, and y's offset within it. Past the end gives the
    /// last unit.
    pub fn find(&self, y: f64) -> (usize, f64) {
        let n = self.values.len();
        if n == 0 {
            return (0, 0.0);
        }
        // Largest prefix whose sum is <= y.
        let mut pos = 0;
        let mut rest = y.max(0.0);
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n && self.tree[next] <= rest {
                pos = next;
                rest -= self.tree[next];
            }
            step >>= 1;
        }
        if pos >= n {
            (n - 1, rest + self.values[n - 1] as f64)
        } else {
            (pos, rest)
        }
    }

    /// Units overlapping document y range `top..bottom`.
    pub fn range(&self, top: f64, bottom: f64) -> std::ops::Range<usize> {
        if self.values.is_empty() {
            return 0..0;
        }
        let (first, _) = self.find(top);
        let (last, _) = self.find((bottom - 0.001).max(top));
        first..last + 1
    }
}

/// The scroll position: the unit at the top of the viewport and how far into it the
/// viewport starts. The pixel offset is derived from the heights.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Anchor {
    pub unit: usize,
    pub offset: f64,
}

impl Anchor {
    pub fn scroll_y(&self, h: &Heights) -> f64 {
        h.top(self.unit) + self.offset
    }

    /// Scrolls by `dy`, clamped to the document (the last viewport ends at the bottom).
    pub fn scroll_by(&mut self, dy: f64, h: &Heights, viewport: f64) {
        self.scroll_to(self.scroll_y(h) + dy, h, viewport);
    }

    pub fn scroll_to(&mut self, y: f64, h: &Heights, viewport: f64) {
        let max = (h.total() - viewport).max(0.0);
        let (unit, offset) = h.find(y.clamp(0.0, max));
        *self = Anchor { unit, offset };
    }

    /// Keeps the anchor valid after its own unit changes height or units are replaced.
    pub fn clamp(&mut self, h: &Heights) {
        if h.is_empty() {
            *self = Anchor::default();
            return;
        }
        self.unit = self.unit.min(h.len() - 1);
        self.offset = self.offset.clamp(0.0, h.get(self.unit) as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_top(v: &[f32], i: usize) -> f64 {
        v[..i].iter().map(|&x| x as f64).sum()
    }

    #[test]
    fn prefix_sums_match_naive_after_updates() {
        let mut v: Vec<f32> = (0..1000).map(|i| (i % 37) as f32 + 1.0).collect();
        let mut h = Heights::new(v.clone());
        for k in 0..500 {
            let i = (k * 7919) % v.len();
            let nh = ((k * 31) % 97) as f32 + 0.5;
            v[i] = nh;
            h.set(i, nh);
        }
        for i in [0, 1, 2, 63, 64, 500, 999, 1000] {
            assert!((h.top(i) - naive_top(&v, i)).abs() < 1e-6, "top({i})");
        }
    }

    #[test]
    fn find_returns_unit_and_offset() {
        let h = Heights::new(vec![10.0, 20.0, 30.0]);
        assert_eq!(h.find(0.0), (0, 0.0));
        assert_eq!(h.find(9.5), (0, 9.5));
        assert_eq!(h.find(10.0), (1, 0.0));
        assert_eq!(h.find(35.0), (2, 5.0));
        assert_eq!(h.find(1000.0).0, 2);
        assert_eq!(h.range(5.0, 31.0), 0..3);
        assert_eq!(h.range(10.0, 30.0), 1..2);
    }

    #[test]
    fn find_on_large_index() {
        let h = Heights::new(vec![24.0; 100_000]);
        assert_eq!(h.find(24.0 * 54_321.0 + 3.0), (54_321, 3.0));
        assert_eq!(h.total(), 2_400_000.0);
    }

    #[test]
    fn zero_height_units_are_skipped_by_find() {
        let h = Heights::new(vec![10.0, 0.0, 0.0, 10.0]);
        assert_eq!(h.find(10.0), (3, 0.0));
    }

    #[test]
    fn height_changes_above_anchor_keep_its_screen_position() {
        let mut h = Heights::new(vec![100.0; 50]);
        let mut a = Anchor::default();
        a.scroll_to(2_550.0, &h, 600.0);
        assert_eq!(
            a,
            Anchor {
                unit: 25,
                offset: 50.0
            }
        );
        // The anchor's screen position is (top of anchor unit - scroll_y) = -offset.
        let screen = |a: &Anchor, h: &Heights| h.top(a.unit) - a.scroll_y(h);
        let before = screen(&a, &h);
        h.set(3, 400.0); // estimate corrected upward
        h.set(10, 20.0); // async content smaller than its placeholder
        h.set(24, 0.0);
        assert_eq!(screen(&a, &h), before);
        assert_eq!(a.scroll_y(&h), h.top(25) + 50.0);
        // Changes below the anchor change nothing either.
        h.set(40, 1000.0);
        assert_eq!(screen(&a, &h), before);
    }

    #[test]
    fn anchor_clamps_when_its_unit_shrinks_or_disappears() {
        let mut a = Anchor {
            unit: 9,
            offset: 80.0,
        };
        a.clamp(&Heights::new(vec![50.0; 5]));
        assert_eq!(
            a,
            Anchor {
                unit: 4,
                offset: 50.0
            }
        );
    }

    #[test]
    fn scrolling_is_clamped_to_the_document() {
        let h = Heights::new(vec![100.0; 10]);
        let mut a = Anchor::default();
        a.scroll_by(-50.0, &h, 300.0);
        assert_eq!(a.scroll_y(&h), 0.0);
        a.scroll_by(5_000.0, &h, 300.0);
        assert_eq!(a.scroll_y(&h), 700.0);
    }
}
