//! LaTeX math through mitex and an in-memory Typst world, producing `MathScene`s.

mod scene;
#[cfg(feature = "typeset")]
mod world;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub use scene::{MathScene, PathCmd, SceneGlyph, ScenePath, SceneRule};
#[cfg(feature = "typeset")]
pub use world::MathWorld;

/// Longest LaTeX source typeset, in bytes; longer formulas show their error fallback.
/// Pathological nesting typesets in quadratic time (about 1 s at this length in release).
pub const MAX_SOURCE: usize = 16 * 1024;

/// Bytes of typeset results the cache keeps (estimated); beyond it the least recently used
/// go. Every edit of a formula is a new entry; a display formula is about 1 KB.
pub const CACHE_BYTES: usize = 16 << 20;

/// Scenes are measured in points at this font size; painting scales them.
pub const REFERENCE_SIZE: f32 = 10.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathError {
    pub message: String,
}

pub type MathResult = Result<Arc<MathScene>, MathError>;

/// (hash of the LaTeX source, display).
pub type Key = (u64, bool);

pub fn key(latex: &str, display: bool) -> Key {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    latex.hash(&mut h);
    (h.finish(), display)
}

/// Typeset results by (source, mode), shared between the UI and background workers. Each
/// worker thread keeps its own long-lived `MathWorld`.
#[derive(Clone)]
pub struct Cache {
    #[cfg_attr(not(feature = "typeset"), allow(dead_code))]
    fonts: &'static [&'static [u8]],
    results: Arc<Mutex<Results>>,
    compiles: Arc<AtomicUsize>,
}

/// Results bounded by estimated bytes, least recently used out first.
struct Results {
    cap: usize,
    used: usize,
    tick: u64,
    /// (result, estimated bytes, last use).
    entries: HashMap<Key, (MathResult, usize, u64)>,
    /// Keys by last use, so the oldest is found without a scan.
    by_use: std::collections::BTreeMap<u64, Key>,
}

impl Results {
    fn get(&mut self, key: &Key) -> Option<MathResult> {
        self.tick += 1;
        let e = self.entries.get_mut(key)?;
        self.by_use.remove(&e.2);
        e.2 = self.tick;
        self.by_use.insert(self.tick, *key);
        Some(e.0.clone())
    }

    fn insert(&mut self, key: Key, result: MathResult) {
        let bytes = result_bytes(&result);
        if let Some((_, old, used)) = self.entries.remove(&key) {
            self.used -= old;
            self.by_use.remove(&used);
        }
        while self.used + bytes > self.cap
            && let Some((_, oldest)) = self.by_use.pop_first()
        {
            let (_, gone, _) = self.entries.remove(&oldest).expect("present");
            self.used -= gone;
        }
        self.tick += 1;
        self.used += bytes;
        self.entries.insert(key, (result, bytes, self.tick));
        self.by_use.insert(self.tick, key);
    }
}

/// Heap and inline size of a result, roughly.
fn result_bytes(result: &MathResult) -> usize {
    use std::mem::size_of;
    let entry = size_of::<(Key, (MathResult, usize, u64))>();
    entry
        + match result {
            Ok(s) => {
                size_of::<MathScene>()
                    + s.glyphs.len() * size_of::<SceneGlyph>()
                    + s.rules.len() * size_of::<SceneRule>()
                    + s.paths
                        .iter()
                        .map(|p| size_of::<ScenePath>() + p.cmds.len() * size_of::<PathCmd>())
                        .sum::<usize>()
            }
            Err(e) => e.message.len(),
        }
}

/// Runs `f`, turning a panic into an error: typesetting runs on background threads, where
/// a panic would abort the app.
#[cfg_attr(not(feature = "typeset"), allow(dead_code))]
fn guarded(f: impl FnOnce() -> MathResult) -> MathResult {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|panic| {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown fault".into());
        Err(MathError {
            message: format!("the typesetter failed: {what}"),
        })
    })
}

#[cfg(feature = "typeset")]
use std::cell::OnceCell;

#[cfg(feature = "typeset")]
thread_local! {
    static WORLD: OnceCell<MathWorld> = const { OnceCell::new() };
}

impl Cache {
    /// `fonts`: the math font first, then any fallbacks. The same list must be registered
    /// for painting, since glyphs refer to fonts by index.
    pub fn new(fonts: &'static [&'static [u8]]) -> Self {
        Self::with_capacity(fonts, CACHE_BYTES)
    }

    /// A cache keeping about `bytes` of results.
    pub fn with_capacity(fonts: &'static [&'static [u8]], bytes: usize) -> Self {
        Self {
            fonts,
            results: Arc::new(Mutex::new(Results {
                cap: bytes,
                used: 0,
                tick: 0,
                entries: HashMap::new(),
                by_use: Default::default(),
            })),
            compiles: Default::default(),
        }
    }

    pub fn get(&self, latex: &str, display: bool) -> Option<MathResult> {
        self.results.lock().unwrap().get(&key(latex, display))
    }

    /// Typesets (on the calling thread) unless cached, and caches the result.
    pub fn compute(&self, latex: &str, display: bool) -> MathResult {
        if let Some(r) = self.get(latex, display) {
            return r;
        }
        self.compiles.fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "typeset")]
        let result = guarded(|| {
            WORLD.with(|w| {
                w.get_or_init(|| MathWorld::new(self.fonts))
                    .typeset(latex, display)
                    .map(Arc::new)
            })
        });
        #[cfg(not(feature = "typeset"))]
        let result = Err(MathError {
            message: "built without math support".into(),
        });
        self.results
            .lock()
            .unwrap()
            .insert(key(latex, display), result.clone());
        result
    }

    /// Typesets a batch, then drops Typst's memoized intermediates that went unused.
    pub fn compute_batch(&self, items: &[(String, bool)]) {
        for (latex, display) in items {
            let _ = self.compute(latex, *display);
        }
        #[cfg(feature = "typeset")]
        comemo::evict(10);
    }

    /// How many equations were typeset (cache misses), for tests.
    pub fn compiles(&self) -> usize {
        self.compiles.load(Ordering::Relaxed)
    }

    /// Estimated bytes of results held, for tests.
    pub fn bytes(&self) -> usize {
        self.results.lock().unwrap().used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_recently_used_result_goes_first() {
        let scene = |w: f32| -> MathResult {
            Ok(Arc::new(MathScene {
                width: w,
                ..Default::default()
            }))
        };
        let one = result_bytes(&scene(1.0));
        let mut r = Results {
            cap: 3 * one,
            used: 0,
            tick: 0,
            entries: HashMap::new(),
            by_use: Default::default(),
        };
        let k = |n: u64| (n, false);
        for n in 0..3 {
            r.insert(k(n), scene(n as f32));
        }
        assert!(r.get(&k(0)).is_some());
        r.insert(k(3), scene(3.0));
        assert!(r.get(&k(1)).is_none(), "the oldest unused result stays");
        for n in [0, 2, 3] {
            assert!(r.get(&k(n)).is_some(), "{n}");
        }
        // Inserting a key again replaces it without leaving a stale use behind.
        r.insert(k(0), scene(9.0));
        assert_eq!(r.entries.len(), r.by_use.len());
        assert_eq!(r.used, 3 * one);
    }

    #[test]
    fn a_panic_becomes_an_error() {
        let err = guarded(|| panic!("boom")).unwrap_err();
        assert_eq!(err.message, "the typesetter failed: boom");
    }
}
