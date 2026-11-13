//! Diagram fences in the reader. The SVG is made in the background; layout uses
//! its natural size (the same in both themes), and painting asks for a raster at device
//! pixels for the current theme. While an edited diagram re-renders, its slot keeps showing
//! the previous rendering.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use gpui::RenderImage;
use mdt_diagram::{DiagramError, Engine};
use mdt_image::RasterCache;
use mdt_markdown::{Block, BlockKind, BlockModel};

use super::images::{RASTER_BYTES, SVG_FONTS, render_image};

/// (engine, hash of the source).
pub type DiagramKey = (Engine, u64);
/// (diagram, dark theme, width and height in device pixels).
type RasterKey = (DiagramKey, bool, u32, u32);

pub fn key(engine: Engine, source: &str) -> DiagramKey {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    (engine, h.finish())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    /// Natural size in SVG units (logical pixels at zoom 1).
    Diagram {
        key: DiagramKey,
        w: f32,
        h: f32,
    },
    Error(String),
    Pending,
}

/// An SVG and its natural size.
struct Svg {
    bytes: Arc<Vec<u8>>,
    w: f32,
    h: f32,
}

type Render = fn(Engine, &str, bool) -> Result<String, DiagramError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Job {
    Svg(DiagramKey, bool),
    Raster(RasterKey),
}

#[derive(Default)]
struct Shared {
    svgs: HashMap<(DiagramKey, bool), Result<Svg, String>>,
    sources: HashMap<DiagramKey, String>,
    /// Rasters that could not be made; they are not tried again.
    failed: HashSet<RasterKey>,
}

pub struct Diagrams {
    shared: Arc<Mutex<Shared>>,
    rasters: Arc<Mutex<RasterCache<RasterKey, RenderImage>>>,
    /// The last rendering each slot (unit, diagram ordinal) showed; `follow_units` keeps
    /// it with its unit when units are added or removed above.
    slots: HashMap<(usize, usize), (DiagramKey, Shown)>,
    /// The light rendering each slot last asked for. While it is still being made, a newer
    /// edit of the fence waits for it, so typing in a large diagram never queues several
    /// multi-second layouts; when it lands, layout asks again for the newest source.
    requested: HashMap<(usize, usize), DiagramKey>,
    pending: Vec<Job>,
    inflight: Arc<Mutex<HashSet<Job>>>,
    /// The engine entry point; tests inject faulty ones.
    pub render: Render,
}

impl Default for Diagrams {
    fn default() -> Self {
        mdt_diagram::set_mermaid_font(mdt_text::fonts::BUNDLED[0].bytes);
        Self {
            shared: Default::default(),
            rasters: Arc::new(Mutex::new(RasterCache::new(RASTER_BYTES))),
            slots: HashMap::new(),
            requested: HashMap::new(),
            pending: Vec::new(),
            inflight: Default::default(),
            render: mdt_diagram::to_svg,
        }
    }
}

pub struct Work {
    job: Job,
    render: Render,
    shared: Arc<Mutex<Shared>>,
    rasters: Arc<Mutex<RasterCache<RasterKey, RenderImage>>>,
    inflight: Arc<Mutex<HashSet<Job>>>,
}

impl Diagrams {
    /// Layout's question for the diagram in `slot`. Sizes come from the light rendering.
    pub fn resolve(&mut self, slot: (usize, usize), engine: Engine, source: &str) -> Shown {
        let k = key(engine, source);
        let found = {
            let mut shared = self.shared.lock().unwrap();
            shared.sources.entry(k).or_insert_with(|| source.to_owned());
            shared.svgs.get(&(k, false)).map(|r| match r {
                Ok(svg) => Shown::Diagram {
                    key: k,
                    w: svg.w,
                    h: svg.h,
                },
                Err(e) => Shown::Error(e.clone()),
            })
        };
        match found {
            Some(shown) => {
                self.slots.insert(slot, (k, shown.clone()));
                shown
            }
            None => {
                let busy = self.requested.get(&slot).is_some_and(|r| {
                    *r != k && self.inflight.lock().unwrap().contains(&Job::Svg(*r, false))
                });
                if !busy {
                    self.requested.insert(slot, k);
                    self.queue(Job::Svg(k, false));
                }
                // Keep the previous rendering while the edited source renders.
                match self.slots.get(&slot) {
                    Some((_, prev @ Shown::Diagram { .. })) => prev.clone(),
                    _ => Shown::Pending,
                }
            }
        }
    }

    /// Paint's question: the raster of `key` for the theme at `w`×`h` device pixels.
    pub fn raster(
        &mut self,
        key: DiagramKey,
        dark: bool,
        w: u32,
        h: u32,
    ) -> Option<Arc<RenderImage>> {
        let (rk, job) = {
            let shared = self.shared.lock().unwrap();
            // A dark rendering that failed falls back to the light one.
            let dark =
                dark && key.0.themed() && !matches!(shared.svgs.get(&(key, true)), Some(Err(_)));
            let rk = (key, dark, w.max(1), h.max(1));
            let job = match shared.svgs.get(&(key, dark)) {
                None => Some(Job::Svg(key, dark)),
                Some(Ok(_)) if !shared.failed.contains(&rk) => Some(Job::Raster(rk)),
                // Failures are not tried again (each try would repaint, and fail again).
                _ => None,
            };
            (rk, job)
        };
        if let Some(r) = self.rasters.lock().unwrap().get(&rk) {
            return Some(r);
        }
        if let Some(job) = job {
            self.queue(job);
        }
        None
    }

    fn queue(&mut self, job: Job) {
        if self.inflight.lock().unwrap().insert(job.clone()) {
            self.pending.push(job);
        }
    }

    /// Moves the slots' previous renderings with their units after a reparse: `follow`
    /// maps an old unit index to the new one (`None` when the unit is gone).
    pub fn follow_units(&mut self, follow: &dyn Fn(usize) -> Option<usize>) {
        self.slots = std::mem::take(&mut self.slots)
            .into_iter()
            .filter_map(|((unit, n), v)| Some(((follow(unit)?, n), v)))
            .collect();
        self.requested = std::mem::take(&mut self.requested)
            .into_iter()
            .filter_map(|((unit, n), k)| Some(((follow(unit)?, n), k)))
            .collect();
    }

    /// Forgets the SVGs, sources and rasters of diagrams that are neither in `model` nor
    /// shown by a slot (each edit of a fence makes a new diagram).
    pub fn retain_model(&mut self, model: &BlockModel) {
        let mut live: HashSet<DiagramKey> = self.slots.values().map(|(k, _)| *k).collect();
        for unit in &model.units {
            fence_keys(&unit.block, &mut live);
        }
        let mut shared = self.shared.lock().unwrap();
        shared.svgs.retain(|(k, _), _| live.contains(k));
        shared.sources.retain(|k, _| live.contains(k));
        shared.failed.retain(|(k, ..)| live.contains(k));
        drop(shared);
        self.rasters
            .lock()
            .unwrap()
            .retain(|(k, ..)| live.contains(k));
    }

    pub fn take_pending(&mut self) -> Vec<Work> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|job| Work {
                job,
                render: self.render,
                shared: self.shared.clone(),
                rasters: self.rasters.clone(),
                inflight: self.inflight.clone(),
            })
            .collect()
    }
}

/// The keys of the diagram fences in `block`.
fn fence_keys(block: &Block, out: &mut HashSet<DiagramKey>) {
    match &block.kind {
        BlockKind::Code { lang, text, .. } => {
            if let Some(engine) = lang.as_deref().and_then(Engine::from_lang) {
                out.insert(key(engine, text));
            }
        }
        BlockKind::BlockQuote { children, .. } => children.iter().for_each(|b| fence_keys(b, out)),
        BlockKind::List { items, .. } => items
            .iter()
            .flat_map(|i| &i.children)
            .for_each(|b| fence_keys(b, out)),
        BlockKind::Footnotes { notes } => notes
            .iter()
            .flat_map(|n| &n.children)
            .for_each(|b| fence_keys(b, out)),
        _ => {}
    }
}

impl Work {
    /// Runs the work; returns whether layout may have changed (a light-theme SVG gives
    /// the diagram's size, or its error).
    pub fn run(self) -> bool {
        match &self.job {
            Job::Svg(key, dark) => {
                // `retain_model` drops the source of a fence edited away while this work
                // waited in the queue: skip it rather than lay out a diagram nobody shows.
                // Layout still runs, so the slot that waited on it asks for its newest source.
                let Some(source) = self.shared.lock().unwrap().sources.get(key).cloned() else {
                    self.inflight.lock().unwrap().remove(&self.job);
                    return true;
                };
                let render = self.render;
                let result = mdt_diagram::guarded(|| render(key.0, &source, *dark))
                    .map_err(|e| e.message)
                    .and_then(|svg| {
                        let bytes = svg.into_bytes();
                        let (w, h) = mdt_image::svg::size(&bytes)?;
                        Ok(Svg {
                            bytes: Arc::new(bytes),
                            w: w as f32,
                            h: h as f32,
                        })
                    });
                self.shared
                    .lock()
                    .unwrap()
                    .svgs
                    .insert((*key, *dark), result);
            }
            Job::Raster(rk) => {
                let (key, dark, w, _) = *rk;
                let svg = self
                    .shared
                    .lock()
                    .unwrap()
                    .svgs
                    .get(&(key, dark))
                    .and_then(|r| r.as_ref().ok().map(|s| (s.bytes.clone(), s.w)));
                if let Some((bytes, natural_w)) = svg {
                    let scale = w as f32 / natural_w.max(1.0);
                    let raster = mdt_diagram::guarded(|| {
                        // Too costly at this size (a high zoom): drawn smaller and scaled.
                        mdt_image::svg::rasterize_fitting(&bytes, scale, &SVG_FONTS)
                            .map(|(raster, _)| raster)
                            .map_err(|message| DiagramError { message })
                    });
                    match raster {
                        Ok(raster) => {
                            let bytes = raster.bytes();
                            self.rasters.lock().unwrap().insert(
                                *rk,
                                Arc::new(render_image(raster)),
                                bytes,
                                |_| false,
                            );
                        }
                        Err(_) => {
                            self.shared.lock().unwrap().failed.insert(*rk);
                        }
                    }
                }
            }
        }
        self.inflight.lock().unwrap().remove(&self.job);
        self.changes_layout()
    }

    /// The diagram this work is for, and whether it makes the raster (bench timing).
    #[cfg(feature = "bench")]
    pub fn bench_key(&self) -> String {
        match &self.job {
            Job::Svg(key, dark) | Job::Raster((key, dark, _, _)) => format!("{key:?}{dark}"),
        }
    }

    #[cfg(feature = "bench")]
    pub fn is_raster(&self) -> bool {
        matches!(self.job, Job::Raster(_))
    }

    /// Whether finishing this work can change layout (a light-theme SVG gives the size).
    pub fn changes_layout(&self) -> bool {
        matches!(self.job, Job::Svg(_, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(d: &mut Diagrams) {
        loop {
            let work = d.take_pending();
            if work.is_empty() {
                break;
            }
            for w in work {
                w.run();
            }
        }
    }

    const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="red"/></svg>"#;

    #[test]
    fn panicking_engine_shows_the_fallback_and_others_still_render() {
        fn faulty(engine: Engine, source: &str, dark: bool) -> Result<String, DiagramError> {
            if source.contains("boom") {
                panic!("injected fault");
            }
            mdt_diagram::to_svg(engine, source, dark)
        }
        let mut d = Diagrams {
            render: faulty,
            ..Default::default()
        };
        assert_eq!(d.resolve((0, 0), Engine::Svg, "boom"), Shown::Pending);
        assert_eq!(d.resolve((1, 0), Engine::Svg, SVG), Shown::Pending);
        run(&mut d);
        match d.resolve((0, 0), Engine::Svg, "boom") {
            Shown::Error(e) => assert!(e.contains("injected fault"), "{e}"),
            other => panic!("expected the fallback, got {other:?}"),
        }
        assert!(matches!(
            d.resolve((1, 0), Engine::Svg, SVG),
            Shown::Diagram {
                w: 40.0,
                h: 20.0,
                ..
            }
        ));
    }

    #[test]
    fn edited_diagram_keeps_its_previous_rendering_until_the_new_one_lands() {
        let mut d = Diagrams::default();
        d.resolve((0, 0), Engine::Svg, SVG);
        run(&mut d);
        let first = d.resolve((0, 0), Engine::Svg, SVG);
        let edited = SVG.replace("width=\"40\"", "width=\"80\"");
        assert_eq!(d.resolve((0, 0), Engine::Svg, &edited), first, "held");
        run(&mut d);
        assert!(matches!(
            d.resolve((0, 0), Engine::Svg, &edited),
            Shown::Diagram { w: 80.0, .. }
        ));
    }

    /// Each frame asks for the raster again: a failure must not queue work every frame.
    #[test]
    fn failed_renderings_are_not_retried_every_frame() {
        fn light_only(engine: Engine, source: &str, dark: bool) -> Result<String, DiagramError> {
            if dark {
                return Err(DiagramError {
                    message: "no dark theme".into(),
                });
            }
            mdt_diagram::to_svg(engine, source, dark)
        }
        let mut d = Diagrams {
            render: light_only,
            ..Default::default()
        };
        let k = key(Engine::Pikchr, "box");
        d.resolve((0, 0), Engine::Pikchr, "box");
        run(&mut d);
        // The dark SVG fails once, then the light one is used.
        let mut jobs = 0;
        let mut shown = false;
        for _ in 0..10 {
            shown |= d.raster(k, true, 100, 50).is_some();
            let work = d.take_pending();
            jobs += work.len();
            for w in work {
                w.run();
            }
        }
        assert!(shown, "falls back to the light rendering");
        assert_eq!(jobs, 2, "the dark SVG, then the light raster");

        // An SVG that cannot be rasterized is tried once.
        let bad = key(Engine::Svg, "bad");
        d.shared.lock().unwrap().svgs.insert(
            (bad, false),
            Ok(Svg {
                bytes: Arc::new(b"not svg".to_vec()),
                w: 10.0,
                h: 10.0,
            }),
        );
        let mut jobs = 0;
        for _ in 0..10 {
            assert!(d.raster(bad, false, 10, 10).is_none());
            let work = d.take_pending();
            jobs += work.len();
            for w in work {
                w.run();
            }
        }
        assert_eq!(jobs, 1);
    }

    /// Typing in a fence: while one rendering is being made, newer sources wait, and the
    /// newest renders once it lands. A queued rendering of a source edited away is skipped.
    #[test]
    fn a_slot_has_one_rendering_in_flight() {
        let mut d = Diagrams::default();
        let source = |w: u32| SVG.replace("width=\"40\"", &format!("width=\"{w}\""));
        d.resolve((0, 0), Engine::Svg, &source(41));
        for w in 42..50 {
            assert_eq!(d.resolve((0, 0), Engine::Svg, &source(w)), Shown::Pending);
        }
        let work = d.take_pending();
        assert_eq!(work.len(), 1, "only the first edit is queued");
        // The fence changed again before the work started: its source is gone.
        let model = mdt_markdown::parse(&format!("```svg\n{}\n```\n", source(49)));
        d.retain_model(&model);
        assert!(
            work.into_iter().all(Work::run),
            "a skipped rendering still relays out"
        );
        d.resolve((0, 0), Engine::Svg, &source(49));
        let work = d.take_pending();
        assert_eq!(work.len(), 1, "then the newest source");
        work.into_iter().for_each(|w| {
            w.run();
        });
        assert!(matches!(
            d.resolve((0, 0), Engine::Svg, &source(49)),
            Shown::Diagram { w: 49.0, .. }
        ));
        let first = key(Engine::Svg, &source(41));
        assert!(!d.shared.lock().unwrap().svgs.contains_key(&(first, false)));
    }

    /// Pasting a new diagram above another: the new one is pending, not showing the other.
    #[test]
    fn previous_renderings_follow_their_unit() {
        let mut d = Diagrams::default();
        let first = d.resolve((0, 0), Engine::Svg, SVG);
        run(&mut d);
        let first_done = d.resolve((0, 0), Engine::Svg, SVG);
        assert_eq!(first, Shown::Pending);
        // A unit was inserted above: unit 0 is now unit 1.
        d.follow_units(&|u| Some(u + 1));
        let pasted = SVG.replace("width=\"40\"", "width=\"60\"");
        assert_eq!(d.resolve((0, 0), Engine::Svg, &pasted), Shown::Pending);
        assert_eq!(d.resolve((1, 0), Engine::Svg, SVG), first_done);
        // A removed unit's slot goes.
        d.follow_units(&|u| (u != 1).then_some(u));
        assert!(!d.slots.contains_key(&(1, 0)));
    }

    /// Each keystroke in a fence makes a new diagram; only those in the document (and the
    /// renderings slots still show) are kept.
    #[test]
    fn diagrams_no_longer_in_the_document_are_dropped() {
        let mut d = Diagrams::default();
        let fence = |w: u32| format!("```svg\n{}\n```\n", SVG.replace("40", &w.to_string()));
        for w in 41..61 {
            let model = mdt_markdown::parse(&fence(w));
            let BlockKind::Code { text, .. } = &model.units[0].block.kind else {
                unreachable!()
            };
            d.resolve((0, 0), Engine::Svg, text);
            run(&mut d);
            d.resolve((0, 0), Engine::Svg, text);
            d.retain_model(&model);
        }
        let shared = d.shared.lock().unwrap();
        assert_eq!(shared.svgs.len(), 1, "{} SVGs kept", shared.svgs.len());
        assert_eq!(shared.sources.len(), 1);
    }

    #[test]
    fn rasters_are_keyed_by_theme_and_pixel_size() {
        let mut d = Diagrams::default();
        let k = key(Engine::Pikchr, "box");
        d.resolve((0, 0), Engine::Pikchr, "box");
        run(&mut d);
        // Each frame asks again; the SVG for a theme renders first, then the raster.
        let raster = |d: &mut Diagrams, dark: bool, w: u32, h: u32| {
            (0..3).any(|_| {
                let r = d.raster(k, dark, w, h).is_some();
                run(d);
                r
            })
        };
        let before = d.rasters.lock().unwrap().len();
        assert!(raster(&mut d, false, 100, 50));
        assert!(raster(&mut d, true, 100, 50), "dark renders its own SVG");
        assert!(raster(&mut d, false, 200, 100), "2x density or zoom");
        assert_eq!(d.rasters.lock().unwrap().len(), before + 3);
        // Asking again for a key that exists renders nothing new.
        assert!(d.raster(k, false, 200, 100).is_some());
        assert!(d.take_pending().is_empty());
        // SVG fences keep their authored colours: dark and light share a raster.
        let s = key(Engine::Svg, SVG);
        d.resolve((1, 0), Engine::Svg, SVG);
        run(&mut d);
        let svg_raster = |d: &mut Diagrams| {
            (0..3).any(|_| {
                let r = d.raster(s, false, 40, 20).is_some();
                run(d);
                r
            })
        };
        assert!(svg_raster(&mut d));
        assert!(d.raster(s, true, 40, 20).is_some(), "same raster for dark");
    }
}
