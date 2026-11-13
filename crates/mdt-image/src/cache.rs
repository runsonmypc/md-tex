//! Rasters bounded by total bytes. When a new raster does not fit, the least recently used
//! off-screen entries go first, then visible ones.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

pub struct RasterCache<K, V> {
    cap: usize,
    used: usize,
    tick: u64,
    entries: HashMap<K, (Arc<V>, usize, u64)>,
}

impl<K: Hash + Eq + Clone, V> RasterCache<K, V> {
    /// `cap`: the byte bound.
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            used: 0,
            tick: 0,
            entries: HashMap::new(),
        }
    }

    pub fn get(&mut self, key: &K) -> Option<Arc<V>> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|e| {
            e.2 = tick;
            e.0.clone()
        })
    }

    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Inserts a value of `bytes` bytes, evicting until it fits. `visible` says which keys
    /// are on screen. A value larger than the whole cap is still kept, alone.
    pub fn insert(&mut self, key: K, value: Arc<V>, bytes: usize, visible: impl Fn(&K) -> bool) {
        self.remove(&key);
        while self.used + bytes > self.cap && !self.entries.is_empty() {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(k, e)| (visible(k), e.2))
                .map(|(k, _)| k.clone())
                .expect("not empty");
            self.remove(&victim);
        }
        self.tick += 1;
        self.used += bytes;
        self.entries.insert(key, (value, bytes, self.tick));
    }

    pub fn remove(&mut self, key: &K) {
        if let Some((_, bytes, _)) = self.entries.remove(key) {
            self.used -= bytes;
        }
    }

    /// Keeps only the entries whose key `keep` accepts.
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        let gone: Vec<K> = self.entries.keys().filter(|k| !keep(k)).cloned().collect();
        for k in gone {
            self.remove(&k);
        }
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
