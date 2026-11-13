//! The safe SVG rasterizer: resvg with a font database holding only the bundled fonts, an
//! image-href resolver that refuses every external reference, and no resources directory.
//! usvg never runs scripts or animations, so SVG renders as static graphics.
//!
//! SVG comes from documents, and this runs on background threads where a panic or a stack
//! overflow aborts the app, so an SVG is checked before it is converted or drawn: its
//! markup before parsing (nesting, attributes), then reference cycles, element and text
//! counts after `use`, `tref`, marker and filter expansion, stylesheet work, image pixels,
//! the drawing work of its layers and shapes, and (at the size it is drawn) the memory of
//! its layers. Parsing and drawing run on their own large stack.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use resvg::{tiny_skia, usvg};

use crate::{Raster, guarded_with};

/// Largest raster side in pixels; bigger requests are scaled down to fit.
const MAX_SIDE: f32 = 8192.0;

/// Largest SVG text accepted, in bytes.
pub const MAX_BYTES: usize = 16 << 20;

/// Deepest element nesting accepted, counting a `use` reference as a level, as usvg follows
/// it. usvg and resvg recurse once per level: about 400 nested groups overflowed a 512 KiB
/// worker stack.
pub const MAX_DEPTH: usize = 128;

/// Most elements accepted once `use` references and markers are expanded. Chained `use`
/// elements multiply (ten levels of ten is ten billion), and usvg copies a marker's content
/// to every vertex it marks.
pub const MAX_ELEMENTS: u64 = 250_000;

/// Most drawing work accepted beyond the canvas itself, in canvas areas, at any size (blurs
/// priced as box blurs; `MAX_LAYER_DRAW` bounds the work for the size drawn at). Each group
/// with opacity, a filter, a mask or a clip path is drawn on its own layer and composited,
/// and filter primitives work over the layer again (twelve nested blurred groups in 631
/// bytes took a second and 150 MB at 2000 px; sixty, over a minute and 1.6 GB).
pub const MAX_LAYER_WORK: f64 = 64.0;

/// Most attributes on one element, counting the declarations of its `style`. roxmltree
/// and usvg compare each with the element's others: 100,000 on one element took 16 s.
pub const MAX_ATTRIBUTES: usize = 256;

/// Most characters of text in all, counting `use` and `tref` copies (10,000 copies of
/// 1,000 characters took 6.9 s and 2.8 GB; 475,000 characters, 3 s in release), and in one
/// `text` element, whose layout grows with the square of its length (30,000 words took
/// 29 s; 20,000 characters 0.14 s).
pub const MAX_TEXT: u64 = 150_000;
pub const MAX_TEXT_PER_ELEMENT: u64 = 20_000;

/// Most `text` elements and the elements in them (each `tspan` a chunk), counting `use`
/// copies, and most elements in one text: usvg lays out each separately, and a text's
/// chunks against each other. 140,000 one-character texts took 3.7 s and 787 MB (and
/// 1,000 of them used 149 times, 6 s and 833 MB), 20,000 texts of 7 `tspan`s 7.1 s, and
/// one text of 2,800 `tspan`s 21.7 s.
pub const MAX_TEXTS: usize = 20_000;
pub const MAX_TEXT_PARTS: usize = 256;

/// Most stylesheet rules times elements: each rule is matched against every element
/// (20,000 of each took 12 s; 2,048 rules for 8,000 elements 0.33 s).
/// The same bound applies to compound selectors matched, to declarations copied to the
/// elements rules may match (12,000 declarations for 20,000 elements took 3.8 GB), and to
/// the ancestors a descendant combinator retries.
pub const MAX_CSS_WORK: u64 = 1 << 24;

/// Most compound selectors in one selector (one of 30,000 took over 40 s).
pub const MAX_COMPOUNDS: usize = 32;

/// Largest stylesheet text, in bytes, not counting `@font-face` blocks and `url(data:...)`
/// values (embedded fonts), which the parser skips without errors. It finds each error's
/// line and column from the start (and ends every declaration with one), so its time grows
/// with the square of the size: 400 KB took 2.4 s. With fonts, the counted size times the
/// whole is bounded by the square of this.
pub const MAX_STYLESHEET: usize = 128 << 10;

/// Most entities an SVG declares, and most text their references add: Illustrator
/// declares a dozen short namespace URLs.
const MAX_ENTITIES: usize = 64;
const MAX_ENTITY_TEXT: usize = 1 << 20;

/// Most raster pixels over all image instances: each is decoded where it is drawn (a
/// 64-megapixel image took 0.2 s a copy).
pub const MAX_IMAGE_PIXELS: u64 = 1 << 28;

/// Stack for parsing and drawing one SVG; pages are committed only as they are touched.
const STACK: usize = 64 << 20;

/// The fonts SVG text may use, with the generic families mapped onto them.
#[derive(Clone)]
pub struct Fonts {
    db: Arc<usvg::fontdb::Database>,
    family: String,
}

impl Fonts {
    /// `faces`: font files; `sans` and `mono`: family names among them for the generic
    /// families (serif, cursive and fantasy fall back to `sans`).
    pub fn new(faces: &[&'static [u8]], sans: &str, mono: &str) -> Self {
        let mut db = usvg::fontdb::Database::new();
        for face in faces {
            db.load_font_data(face.to_vec());
        }
        db.set_sans_serif_family(sans);
        db.set_serif_family(sans);
        db.set_cursive_family(sans);
        db.set_fantasy_family(sans);
        db.set_monospace_family(mono);
        Self {
            db: Arc::new(db),
            family: sans.to_owned(),
        }
    }

    /// No fonts: SVG text is skipped.
    pub fn empty() -> Self {
        Self {
            db: Arc::new(usvg::fontdb::Database::new()),
            family: String::new(),
        }
    }
}

/// An SVG that passed the checks that hold at any size, with the painting work that
/// `fits` weighs against the size it is drawn at.
struct Checked {
    tree: usvg::Tree,
    paint: f64,
}

/// Parses and checks SVG; `refused` counts the external references the resolver turned
/// down. The drawing work and memory also depend on the size drawn at: `fits` checks
/// those. Runs on the caller's stack: callers grow it first.
fn tree(bytes: &[u8], fonts: &Fonts, refused: &AtomicUsize) -> Result<Checked, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("SVG larger than {} MiB", MAX_BYTES >> 20));
    }
    // A compressed SVG would expand unchecked.
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Err("compressed SVG is not supported".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "SVG is not UTF-8".to_owned())?;
    check_markup(text)?;
    let doc = usvg::roxmltree::Document::parse_with_options(
        text,
        usvg::roxmltree::ParsingOptions {
            allow_dtd: true,
            nodes_limit: 4_000_000,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    check_structure(&doc)?;
    let pixels = AtomicU64::new(0);
    let options = usvg::Options {
        resources_dir: None,
        font_family: fonts.family.clone(),
        fontdb: fonts.db.clone(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: data_resolver(&pixels),
            resolve_string: Box::new(|_href, _opts| {
                refused.fetch_add(1, Ordering::Relaxed);
                None
            }),
        },
        ..Default::default()
    };
    let t = usvg::Tree::from_xmltree(&doc, &options).map_err(|e| e.to_string())?;
    if pixels.load(Ordering::Relaxed) > MAX_IMAGE_PIXELS {
        return Err(format!(
            "SVG has too many image pixels (more than {} megapixels)",
            MAX_IMAGE_PIXELS >> 20
        ));
    }
    let size = t.size();
    let canvas = (size.width() as f64, size.height() as f64);
    let layers = layer_work(t.root(), canvas, None);
    if layers.work > MAX_LAYER_WORK {
        return Err(format!(
            "SVG too costly to draw ({:.0} canvas areas of layers; the limit is {MAX_LAYER_WORK})",
            layers.work
        ));
    }
    let (w, h) = (canvas.0.max(1.0), canvas.1.max(1.0));
    let mut draw = DrawWork {
        canvas: w * h,
        height: h,
        area: 0.0,
        inside: 0,
        layers: 0,
        nested: 0,
        clip: tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32).expect("finite"),
        max_layer: tiny_skia::Rect::from_xywh(
            -2.0 * w as f32,
            -2.0 * h as f32,
            5.0 * w as f32,
            5.0 * h as f32,
        )
        .expect("finite"),
        paint: 0.0,
        dashes: 0.0,
        crossings: 0.0,
    };
    draw.group(t.root(), tiny_skia::Transform::identity())?;
    Ok(Checked {
        tree: t,
        paint: draw.paint,
    })
}

/// Whether a checked SVG may be drawn at `scale`: its painting, the layers alive at once
/// and its layers' work (blurs priced for the size drawn at), for the pixels drawn.
fn fits(c: &Checked, scale: f32) -> Result<(), String> {
    let size = c.tree.size();
    let (pw, ph) = pixel_size(size, scale);
    let canvas = (size.width() as f64, size.height() as f64);
    let layers = layer_work(c.tree.root(), canvas, Some(fit_scale(size, scale)));
    let pixels = pw as f64 * ph as f64;
    // Small canvases still draw each shape's edges.
    if c.paint * pixels.max(MIN_DRAW_PIXELS) > MAX_DRAW_PIXELS {
        return Err(format!(
            "SVG too costly to draw ({:.0} canvas areas of painting at {pw}×{ph})",
            c.paint
        ));
    }
    if layers.live * pixels > MAX_LAYER_PIXELS {
        return Err(format!(
            "SVG needs too much memory to draw ({:.0} canvas-sized layers at once at {pw}×{ph})",
            layers.live
        ));
    }
    if layers.work * pixels > MAX_LAYER_DRAW {
        return Err(format!(
            "SVG too costly to draw ({:.0} canvas areas of layers at {pw}×{ph})",
            layers.work
        ));
    }
    Ok(())
}

/// Most painting work when drawn, in pixels painted with blending (a translucent fill of
/// the whole canvas is one per pixel): 20,000 opaque rectangles over a 2800×8192 canvas
/// (1 KB through `use`) took 97 s, and one rectangle with a gradient of 20,000 stops 50 s.
/// About 3.5 s at most.
const MAX_DRAW_PIXELS: f64 = (1u64 << 30) as f64;
/// The fewest pixels the painting work is counted at.
const MIN_DRAW_PIXELS: f64 = (1u64 << 20) as f64;

/// Most layer work when drawn, in pixels copied (a canvas-sized layer is one per pixel, a
/// blur 8, or 45 below two pixels of deviation): about 2.3 s.
const MAX_LAYER_DRAW: f64 = (1u64 << 28) as f64;

/// Most layer pixels alive at once when drawn: each isolated group draws on a layer of its
/// own while the groups around it keep theirs, and a filter keeps its source and every
/// result. One filter on a region of eight canvases took 2.2 GB at 2800×8192, and fifteen
/// nested translucent groups 1.5 GB. 512 MiB of layers.
const MAX_LAYER_PIXELS: f64 = (1u64 << 27) as f64;

/// The scale `render` draws at for a requested `scale`: no side over `MAX_SIDE`.
fn fit_scale(size: usvg::Size, scale: f32) -> f32 {
    scale
        .min(MAX_SIDE / size.width().max(1.0))
        .min(MAX_SIDE / size.height().max(1.0))
        .max(0.0)
}

/// The raster size `render` makes for a requested `scale`.
fn pixel_size(size: usvg::Size, scale: f32) -> (u32, u32) {
    let scale = fit_scale(size, scale);
    (
        ((size.width() * scale).ceil() as u32).max(1),
        ((size.height() * scale).ceil() as u32).max(1),
    )
}

/// Most canvas areas one pattern tile may take, and all tiles together: resvg draws a
/// tile for each shape the pattern paints, at the shape's scale, so a 1×1 tile scaled by
/// 10,000 in a pattern scaled by 10,000 asked for 40 PB.
const MAX_PATTERN_TILE: f64 = 4.0;
const MAX_PATTERN_AREA: f64 = 64.0;
/// The shapes, tiles and layers drawn, walked as resvg draws them (pattern content once
/// per shape it paints). (The content's elements count towards `MAX_ELEMENTS` in
/// `check_structure`.)
struct DrawWork {
    /// The canvas's area and height, in SVG units.
    canvas: f64,
    height: f64,
    /// Pattern tiles drawn, in canvas areas.
    area: f64,
    /// How many pattern tiles the walk is in.
    inside: u32,
    /// Layers drawn in tiles, and how many enclose the walk: 20 nested filtered groups in
    /// a pattern took over 40 s, as each layer grows past the one inside it.
    layers: u64,
    nested: u32,
    /// Where drawing is clipped to (the canvas, a layer or a tile), and the most a layer
    /// may take there.
    clip: tiny_skia::Rect,
    max_layer: tiny_skia::Rect,
    /// Painting work, in canvas areas painted with blending.
    paint: f64,
    /// Dashes made for strokes.
    dashes: f64,
    /// The square of each shape's edges, in canvas heights: how often they may cross.
    crossings: f64,
}

/// Most crossings of edges within shapes, in canvas heights squared. tiny-skia keeps the
/// edges on a pixel row sorted, and each crossing reorders them: one path of 110,000
/// random lines (37,000 canvas heights of edges) took 17 s, and 20,000 lines 0.6 s.
const MAX_CROSSINGS: f64 = (1u64 << 27) as f64;

/// Most dashes made for dashed strokes, about 2 s of work (each took about 0.5 µs).
const MAX_DASHES: f64 = (1u64 << 22) as f64;

/// Most layers drawn in pattern tiles (counting each use), and most nested in one.
const MAX_PATTERN_LAYERS: u64 = 256;
const MAX_PATTERN_NESTED_LAYERS: u32 = 4;

impl DrawWork {
    fn group(&mut self, g: &usvg::Group, ts: tiny_skia::Transform) -> Result<(), String> {
        let isolated = g.should_isolate();
        let layer = self.inside > 0 && isolated;
        if layer {
            self.layers += 1;
            self.nested += 1;
            if self.layers > MAX_PATTERN_LAYERS || self.nested > MAX_PATTERN_NESTED_LAYERS {
                return Err("SVG too costly to draw (too many layers in patterns)".into());
            }
        }
        // A layer is drawn as large as its content, up to `max_layer`.
        let clip = self.clip;
        if isolated {
            self.clip = (g.layer_bounding_box().to_rect())
                .transform(ts.pre_concat(g.transform()))
                .and_then(|r| r.intersect(&self.max_layer))
                .unwrap_or(clip);
        }
        let result = self.group_content(g, ts);
        self.clip = clip;
        if layer {
            self.nested -= 1;
        }
        result
    }

    fn group_content(&mut self, g: &usvg::Group, ts: tiny_skia::Transform) -> Result<(), String> {
        let ts = ts.pre_concat(g.transform());
        let masks = g.mask().map(|m| m.root()).into_iter();
        let clips = g.clip_path().map(|c| c.root()).into_iter();
        let images = (g.filters().iter())
            .flat_map(|f| f.primitives())
            .filter_map(|p| match p.kind() {
                usvg::filter::Kind::Image(image) => Some(image.root()),
                _ => None,
            });
        for root in masks.chain(clips).chain(images) {
            self.group(root, ts)?;
        }
        for child in g.children() {
            match child {
                usvg::Node::Group(c) => self.group(c, ts)?,
                usvg::Node::Text(t) => self.group(t.flattened(), ts)?,
                usvg::Node::Path(p) => {
                    if !p.is_visible() {
                        continue;
                    }
                    let rows = edge_rows(p.data(), ts, self.clip) / self.height;
                    if let Some(fill) = p.fill() {
                        let weight = paint_weight(fill.paint(), fill.opacity().get());
                        self.shape(p.bounding_box(), None, weight, rows, ts);
                        if let usvg::Paint::Pattern(pattern) = fill.paint() {
                            self.pattern(pattern, ts)?;
                        }
                    }
                    if let Some(stroke) = p.stroke() {
                        let weight = paint_weight(stroke.paint(), stroke.opacity().get());
                        let length = path_length(p.data());
                        let outline = length * stroke.width().get() as f64;
                        // The outline runs along both sides, with a join at each vertex.
                        let joins = p.data().len() as f64
                            * stroke.width().get() as f64
                            * ts.get_scale().1 as f64
                            / self.height;
                        let edges = 2.0 * rows + joins;
                        self.shape(p.stroke_bounding_box(), Some(outline), weight, edges, ts);
                        self.dashes(stroke.dasharray(), length)?;
                        if let usvg::Paint::Pattern(pattern) = stroke.paint() {
                            self.pattern(pattern, ts)?;
                        }
                    }
                }
                usvg::Node::Image(i) => {
                    if i.is_visible() {
                        let edges =
                            2.0 * i.bounding_box().height() as f64 * ts.get_scale().1 as f64
                                / self.height;
                        self.shape(i.bounding_box(), None, 2.0, edges, ts);
                    }
                }
            }
        }
        if self.crossings > MAX_CROSSINGS {
            return Err("SVG too costly to draw (shapes with too many crossing edges)".into());
        }
        if self.paint > MAX_PAINT {
            return Err(format!(
                "SVG too costly to draw (more than {MAX_PAINT} canvas areas of painting)"
            ));
        }
        Ok(())
    }

    /// One shape painted with `weight` per pixel over its bounding box `bbox` (in the
    /// coordinates `ts` maps to the canvas), or over `outline` square units if that is
    /// less (a stroke), whose edges cross `edges` canvas heights of pixel rows.
    fn shape(
        &mut self,
        bbox: tiny_skia::Rect,
        outline: Option<f64>,
        weight: f64,
        edges: f64,
        ts: tiny_skia::Transform,
    ) {
        self.paint += edges * EDGE_COST;
        self.crossings += edges * edges;
        let Some(r) = bbox.transform(ts).and_then(|r| r.intersect(&self.clip)) else {
            return;
        };
        let mut area = r.width() as f64 * r.height() as f64;
        if let Some(outline) = outline {
            let (sx, sy) = ts.get_scale();
            area = area.min(outline * sx as f64 * sy as f64);
        }
        self.paint += area / self.canvas * weight;
    }

    /// The dashes tiny-skia makes for a stroke of `length` (in the path's units) with
    /// `dasharray`: it gives up past a million on one path, but 100 such paths took 51 s.
    fn dashes(&mut self, dasharray: Option<&[f32]>, length: f64) -> Result<(), String> {
        let Some(dashes) = dasharray else {
            return Ok(());
        };
        let period: f64 = dashes.iter().map(|&d| d as f64).sum();
        if period > 0.0 {
            let per_period = (dashes.len() / 2).max(1) as f64;
            self.dashes += (length / period * per_period).min(1e6);
        }
        if self.dashes > MAX_DASHES {
            return Err(format!(
                "SVG too costly to draw (more than {MAX_DASHES} dashes)"
            ));
        }
        Ok(())
    }

    /// One use of `pattern` by a shape drawn with `ts`: its tile and its content again.
    fn pattern(&mut self, pattern: &usvg::Pattern, ts: tiny_skia::Transform) -> Result<(), String> {
        let (sx, sy) = ts.pre_concat(pattern.transform()).get_scale();
        let rect = pattern.rect();
        let (tw, th) = (rect.width() * sx, rect.height() * sy);
        let tile = tw as f64 * th as f64 / self.canvas;
        self.area += tile;
        if tile > MAX_PATTERN_TILE || self.area > MAX_PATTERN_AREA {
            return Err("SVG too costly to draw (pattern tiles too large)".into());
        }
        self.inside += 1;
        let (clip, max_layer) = (self.clip, self.max_layer);
        // A tile is drawn on a pixmap of its own size.
        if let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, tw, th) {
            (self.clip, self.max_layer) = (r, r);
        }
        let content = self.group(pattern.root(), tiny_skia::Transform::from_scale(sx, sy));
        (self.clip, self.max_layer) = (clip, max_layer);
        self.inside -= 1;
        content
    }
}

/// Most painting work accepted at any size, in canvas areas painted with blending (each
/// about 80 ms at 2800×8192); `MAX_DRAW_PIXELS` bounds it by the size drawn at.
const MAX_PAINT: f64 = 4096.0;

/// Work of an edge crossing the canvas's height, relative to painting the canvas with
/// blending: tiny-skia steps each edge through every pixel row it crosses (2,000 thin
/// diagonal lines across a 2800×8192 canvas took 7.7 s, and one path of 200,000 random
/// lines over a minute).
const EDGE_COST: f64 = 0.025;

/// The pixel rows inside `clip` that the edges of `path` cross once mapped by `ts`, in its
/// units (curves by their control polygons, which enclose them).
fn edge_rows(path: &tiny_skia::Path, ts: tiny_skia::Transform, clip: tiny_skia::Rect) -> f64 {
    let (top, bottom) = (clip.top() as f64, clip.bottom() as f64);
    let mut rows = 0.0f64;
    let (mut start, mut at) = (tiny_skia::Point::zero(), tiny_skia::Point::zero());
    let mut to = |mut p: tiny_skia::Point, at: &mut tiny_skia::Point| {
        ts.map_point(&mut p);
        let (y0, y1) = (at.y.min(p.y) as f64, at.y.max(p.y) as f64);
        rows += (y1.min(bottom) - y0.max(top)).max(0.0);
        *at = p;
    };
    for segment in path.segments() {
        match segment {
            tiny_skia::PathSegment::MoveTo(p) => {
                start = p;
                at = p;
                ts.map_point(&mut at);
            }
            tiny_skia::PathSegment::LineTo(p) => to(p, &mut at),
            tiny_skia::PathSegment::QuadTo(a, p) => {
                to(a, &mut at);
                to(p, &mut at);
            }
            tiny_skia::PathSegment::CubicTo(a, b, p) => {
                to(a, &mut at);
                to(b, &mut at);
                to(p, &mut at);
            }
            tiny_skia::PathSegment::Close => to(start, &mut at),
        }
    }
    rows
}

/// Work per pixel of a paint, relative to a translucent colour: tiny-skia fills opaque
/// colour spans directly (about 1/19 the time), and finds each pixel's gradient stop by
/// walking the stops (a 2800×8192 rectangle took 0.23 s with 2 stops, 3.3 s with 1,000).
fn paint_weight(paint: &usvg::Paint, opacity: f32) -> f64 {
    match paint {
        usvg::Paint::Color(_) if opacity >= 1.0 => 1.0 / 16.0,
        usvg::Paint::Color(_) => 1.0,
        usvg::Paint::LinearGradient(g) => 3.0 + g.stops().len() as f64 / 32.0,
        usvg::Paint::RadialGradient(g) => 3.0 + g.stops().len() as f64 / 32.0,
        usvg::Paint::Pattern(_) => 2.0,
    }
}

/// The length of a path's control polygon (at least its length), in its own units.
fn path_length(path: &tiny_skia::Path) -> f64 {
    let (mut len, mut start, mut at) = (0.0f64, None, tiny_skia::Point::zero());
    let mut to = |p: tiny_skia::Point, at: &mut tiny_skia::Point| {
        len += at.distance(p) as f64;
        *at = p;
    };
    for segment in path.segments() {
        match segment {
            tiny_skia::PathSegment::MoveTo(p) => {
                at = p;
                start = Some(p);
            }
            tiny_skia::PathSegment::LineTo(p) => to(p, &mut at),
            tiny_skia::PathSegment::QuadTo(a, p) => {
                to(a, &mut at);
                to(p, &mut at);
            }
            tiny_skia::PathSegment::CubicTo(a, b, p) => {
                to(a, &mut at);
                to(b, &mut at);
                to(p, &mut at);
            }
            tiny_skia::PathSegment::Close => {
                if let Some(p) = start {
                    to(p, &mut at);
                }
            }
        }
    }
    len
}

/// Inline `data:` images stay allowed, as they are part of the SVG, but only rasters whose
/// header passes the decoder's size limits (resvg decodes them itself); `pixels` adds up
/// those of every instance. A nested SVG would skip this file's checks, so it is not
/// drawn.
pub(crate) fn data_resolver(pixels: &AtomicU64) -> usvg::ImageHrefDataResolverFn<'_> {
    let default = usvg::ImageHrefResolver::default_data_resolver();
    Box::new(move |mime, data, opts| {
        let (w, h) = crate::probe_bytes(&data).ok()?;
        pixels.fetch_add(u64::from(w) * u64::from(h), Ordering::Relaxed);
        match default(mime, data, opts)? {
            usvg::ImageKind::SVG(_) => None,
            raster => Some(raster),
        }
    })
}

/// Checks the markup before roxmltree parses it. Its parser recurses once per nesting
/// level, so 100,000 nested groups overflowed even the 64 MiB stack, and it compares each
/// attribute with the element's others. Entities whose values hold markup could nest
/// below this check, so they are refused.
fn check_markup(text: &str) -> Result<(), String> {
    let b = text.as_bytes();
    // The index just past `end`, searching from `from` (or the end of the text).
    let past = |from: usize, end: &str| {
        text[from..]
            .find(end)
            .map_or(b.len(), |k| from + k + end.len())
    };
    let (mut i, mut depth) = (0, 0usize);
    while let Some(k) = text[i..].find('<') {
        i += k;
        let rest = &text[i..];
        if rest.starts_with("<!--") {
            i = past(i + 4, "-->");
        } else if rest.starts_with("<![CDATA[") {
            i = past(i + 9, "]]>");
        } else if rest.starts_with("<?") {
            i = past(i + 2, "?>");
        } else if rest.starts_with("<!") {
            i = skip_declaration(text, i)?;
        } else if rest.starts_with("</") {
            depth = depth.saturating_sub(1);
            i = past(i + 2, ">");
        } else {
            // A start tag: count its attributes outside quoted values.
            let (mut j, mut quote, mut attributes) = (i + 1, None, 0);
            while j < b.len() {
                match (quote, b[j]) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, q @ (b'"' | b'\'')) => quote = Some(q),
                    (None, b'=') => attributes += 1,
                    (None, b'>') => break,
                    _ => {}
                }
                j += 1;
            }
            if attributes > MAX_ATTRIBUTES {
                return Err(too_many_attributes());
            }
            if b.get(j.wrapping_sub(1)) != Some(&b'/') {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(too_deep());
                }
            }
            i = j + 1;
        }
        if i >= b.len() {
            break;
        }
    }
    Ok(())
}

/// The index past a `<!...>` declaration at `at`. A DOCTYPE's internal subset is read
/// with roxmltree's grammar (comments, processing instructions, declarations), as a `<!--`
/// in a processing instruction desynced a looser scan and hid entities with markup.
/// Entities may only hold short text, and how much text their references add is bounded.
fn skip_declaration(text: &str, at: usize) -> Result<usize, String> {
    let b = text.as_bytes();
    let past = |from: usize, end: &str| {
        text[from..]
            .find(end)
            .map_or(b.len(), |k| from + k + end.len())
    };
    if !text[at..].starts_with("<!DOCTYPE") {
        return Ok(past(at + 2, ">"));
    }
    // The name and external ID, to the subset or the end.
    let (mut j, mut quote) = (at + 9, None);
    loop {
        match (quote, b.get(j)) {
            (_, None) => return Ok(b.len()),
            (Some(q), Some(&c)) if c == q => quote = None,
            (Some(_), _) => {}
            (None, Some(&q @ (b'"' | b'\''))) => quote = Some(q),
            (None, Some(b'>')) => return Ok(j + 1),
            (None, Some(b'[')) => break,
            _ => {}
        }
        j += 1;
    }
    j += 1;
    let mut entities: Vec<(&str, usize)> = Vec::new();
    let end = loop {
        while b.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        let rest = &text[j..];
        if rest.is_empty() {
            break b.len();
        } else if rest.starts_with("<!--") {
            j = past(j + 4, "-->");
        } else if rest.starts_with("<?") {
            j = past(j + 2, "?>");
        } else if rest.starts_with(']') {
            break past(j + 1, ">");
        } else if rest.starts_with("<!ENTITY") {
            let (entity, next) = entity_declaration(text, j + 8)?;
            entities.push(entity);
            if entities.len() > MAX_ENTITIES {
                return Err(format!("SVG declares more than {MAX_ENTITIES} entities"));
            }
            j = next;
        } else if ["<!ELEMENT", "<!ATTLIST", "<!NOTATION"]
            .iter()
            .any(|d| rest.starts_with(d))
        {
            j = past(j, ">");
        } else {
            return Err("SVG has a DTD that is not understood".into());
        }
    };
    // Each reference after the DOCTYPE copies its entity's text (20,000 references to
    // 100,000 characters took 8 s and 3.6 GB).
    let mut added = 0usize;
    for (name, len) in entities {
        let references = text[end..].matches(&format!("&{name};")).count();
        added = added.saturating_add(references.saturating_mul(len));
    }
    if added > MAX_ENTITY_TEXT {
        return Err(format!(
            "SVG entities expand to more than {} KiB",
            MAX_ENTITY_TEXT >> 10
        ));
    }
    Ok(end)
}

/// The name and length of the entity declared from `at` (past `<!ENTITY`), and the index
/// past its declaration. Only general entities with a quoted value of plain text.
fn entity_declaration(text: &str, at: usize) -> Result<((&str, usize), usize), String> {
    let unsupported = || {
        Err("SVG entities with markup, references or external values are not supported".to_owned())
    };
    let rest = text[at..].trim_start();
    let name_len = rest
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(rest.len());
    let (name, rest) = rest.split_at(name_len);
    let rest = rest.trim_start();
    let Some(quote) = rest.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
        return unsupported();
    };
    let Some(len) = rest[1..].find(quote) else {
        return unsupported();
    };
    let value = &rest[1..1 + len];
    if name.starts_with('%') || value.contains(['<', '&', '%']) {
        return unsupported();
    }
    let after = text.len() - rest.len() + 1 + len + 1;
    let next = text[after..]
        .find('>')
        .map_or(text.len(), |k| after + k + 1);
    Ok(((name, value.len()), next))
}

fn too_deep() -> String {
    format!("SVG nesting is deeper than {MAX_DEPTH} levels")
}

fn too_many() -> String {
    format!("SVG has too many elements once expanded (more than {MAX_ELEMENTS})")
}

fn too_many_texts() -> String {
    format!("SVG has too much text (more than {MAX_TEXTS} text elements and spans once expanded)")
}

fn too_many_attributes() -> String {
    format!("SVG has an element with more than {MAX_ATTRIBUTES} attributes")
}

type Node<'a, 'input> = usvg::roxmltree::Node<'a, 'input>;

/// Properties whose `url(#id)` refers to another element, markers last.
const REFERENCES: [&str; 9] = [
    "mask",
    "clip-path",
    "filter",
    "fill",
    "stroke",
    "marker",
    "marker-start",
    "marker-mid",
    "marker-end",
];

/// The references a cycle is refused through: usvg skips a marker inside itself (its
/// `parent_markers`, kept through masks, clip paths and filters), so any cycle through a
/// marker ends there.
const CYCLE_REFERENCES: &[&str] = REFERENCES.split_at(5).0;

/// Elements whose `href` usvg follows.
const HREFS: [&str; 8] = [
    "use",
    "feImage",
    "pattern",
    "linearGradient",
    "radialGradient",
    "filter",
    "textPath",
    "tref",
];

/// Checks the tree the way usvg will build it: attributes per element, stylesheet work,
/// reference cycles, nesting depth, and the elements and text once `use`, `tref`,
/// markers and `feImage` are expanded.
fn check_structure(doc: &usvg::roxmltree::Document) -> Result<(), String> {
    let elements: Vec<Node> = doc.descendants().filter(|n| n.is_element()).collect();
    for n in &elements {
        let declarations = n.attribute("style").map_or(0, |s| s.split(';').count());
        if n.attributes().len() + declarations > MAX_ATTRIBUTES {
            return Err(too_many_attributes());
        }
    }
    let texts: Vec<&Node> = elements.iter().filter(|n| n.has_tag_name("text")).collect();
    if texts.len() > MAX_TEXTS {
        return Err(too_many_texts());
    }
    if texts
        .iter()
        .any(|t| t.descendants().filter(|d| d.is_element()).count() > MAX_TEXT_PARTS + 1)
    {
        return Err(format!(
            "SVG has a text element of more than {MAX_TEXT_PARTS} parts"
        ));
    }
    // Stylesheets, as usvg reads them; their rules apply to every element.
    let styles: Vec<&str> = (elements.iter())
        .filter(|n| n.has_tag_name("style") && n.attribute("type").is_none_or(|t| t == "text/css"))
        .filter_map(|n| n.text())
        .collect();
    // simplecss ends each declaration with an error whose line it counts from the start,
    // so skipped fonts still cost their length once per declaration after them.
    let counted: usize = styles.iter().map(|t| stylesheet_len(t)).sum();
    let total: usize = styles.iter().map(|t| t.len()).sum();
    if counted > MAX_STYLESHEET || counted.saturating_mul(total) > MAX_STYLESHEET * MAX_STYLESHEET {
        return Err(format!(
            "SVG stylesheet larger than {} KiB",
            MAX_STYLESHEET >> 10
        ));
    }
    let mut sheet = simplecss::StyleSheet::new();
    for text in styles {
        sheet.parse_more(text);
    }
    if (sheet.rules.len() as u64).saturating_mul(elements.len() as u64) > MAX_CSS_WORK {
        return Err(format!(
            "SVG stylesheet too costly to apply ({} rules for {} elements)",
            sheet.rules.len(),
            elements.len()
        ));
    }
    css_work(&sheet, &elements)?;
    let css = css_references(&sheet, &elements);
    let ids: HashMap<&str, Node> = elements
        .iter()
        .filter_map(|n| Some((n.attribute("id")?, *n)))
        .collect();
    let mut walk = Walk {
        ids,
        css,
        memo: HashMap::new(),
        text: HashMap::new(),
        visiting: HashSet::new(),
    };
    walk.refuse_cycles(&elements)?;
    let size = walk.expanded(doc.root_element(), 1, NO_MARKERS)?;
    if size.elements > MAX_ELEMENTS {
        return Err(too_many());
    }
    if size.texts > MAX_TEXTS as u64 {
        return Err(too_many_texts());
    }
    if size.chars > MAX_TEXT {
        return Err(format!(
            "SVG has too much text (more than {MAX_TEXT} characters)"
        ));
    }
    Ok(())
}

/// The stylesheet's `url(...)` references by element: the value that wins for each
/// property (rules are in priority order). Only rules with such a value are matched, so a
/// later rule without one does not clear it; that errs towards more references.
fn css_references<'a, 'input>(
    sheet: &simplecss::StyleSheet<'a>,
    elements: &[Node<'_, 'input>],
) -> HashMap<usvg::roxmltree::NodeId, Vec<(&'a str, &'a str)>> {
    let mut css: HashMap<usvg::roxmltree::NodeId, Vec<(&str, &str)>> = HashMap::new();
    for rule in &sheet.rules {
        // `marker` sets all three, as usvg reads it.
        let refs: Vec<(&str, &str)> = (rule.declarations.iter())
            .filter(|d| REFERENCES.contains(&d.name) && d.value.contains("url("))
            .flat_map(|d| match d.name {
                "marker" => ["marker-start", "marker-mid", "marker-end"]
                    .map(|n| (n, d.value))
                    .to_vec(),
                name => vec![(name, d.value)],
            })
            .collect();
        if refs.is_empty() {
            continue;
        }
        for n in elements {
            if rule.selector.matches(&CssNode(*n)) {
                let entry = css.entry(n.id()).or_default();
                for &(name, value) in &refs {
                    match entry.iter_mut().find(|(k, _)| *k == name) {
                        Some(slot) => slot.1 = value,
                        None => entry.push((name, value)),
                    }
                }
            }
        }
    }
    css
}

/// Refuses a stylesheet whose matching or copying would be too costly. simplecss matches a
/// descendant combinator by trying every ancestor in turn, so an element that matches a
/// selector's last compound costs up to its depth to the power of those combinators.
fn css_work(sheet: &simplecss::StyleSheet, elements: &[Node]) -> Result<(), String> {
    let n = elements.len() as u64;
    let too_costly = || Err("SVG stylesheet too costly to apply".to_owned());
    let (mut matching, mut copying) = (0u64, 0u64);
    for rule in &sheet.rules {
        copying = copying.saturating_add(rule.declarations.len() as u64 * n);
        // Compounds are separated by spaces, with `>` and `+` for the other combinators
        // (a quoted attribute value with a space overcounts, erring towards refusing).
        let text = rule.selector.to_string();
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let combinator = |t: &&str| matches!(*t, ">" | "+");
        let compounds = tokens.iter().filter(|t| !combinator(t)).count();
        if compounds > MAX_COMPOUNDS {
            return Err(format!(
                "SVG stylesheet has a selector of more than {MAX_COMPOUNDS} parts"
            ));
        }
        matching = matching.saturating_add(compounds as u64 * n);
        let descendants = (tokens.windows(2))
            .filter(|w| !combinator(&w[0]) && !combinator(&w[1]))
            .count() as u32;
        if descendants > 0 {
            let last = tokens.last().and_then(|t| simplecss::Selector::parse(t));
            for e in elements {
                if last.as_ref().is_none_or(|s| s.matches(&CssNode(*e))) {
                    let depth = e.ancestors().count() as u64;
                    matching = matching.saturating_add(depth.saturating_pow(descendants));
                }
            }
        }
        if matching > MAX_CSS_WORK || copying > MAX_CSS_WORK {
            return too_costly();
        }
    }
    Ok(())
}

/// `text`'s length without top-level `@font-face` blocks and `url(data:...)` values, which
/// simplecss skips (to the matching brace, or to the `)`) without errors.
fn stylesheet_len(text: &str) -> usize {
    let (b, mut i, mut depth, mut skipped) = (text.as_bytes(), 0, 0usize, 0);
    while i < b.len() {
        let from = i;
        if text[i..].starts_with("/*") {
            i = text[i + 2..].find("*/").map_or(b.len(), |k| i + 2 + k + 2);
            continue;
        } else if depth == 0 && text[i..].starts_with("@font-face") {
            // To the `;` or the block's matching brace.
            i += "@font-face".len();
            while i < b.len() && !matches!(b[i], b';' | b'{') {
                i += 1;
            }
            let mut braces = 0;
            while i < b.len() {
                match b[i] {
                    b'{' => braces += 1,
                    b'}' => braces -= 1,
                    b';' if braces == 0 => break,
                    _ => {}
                }
                i += 1;
                if braces == 0 {
                    break;
                }
            }
            skipped += i - from;
            continue;
        } else if depth > 0 && text[i..].starts_with("url(data:") {
            i = text[i..].find(')').map_or(b.len(), |k| i + k);
            skipped += i - from;
            continue;
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    b.len() - skipped
}

/// A roxmltree element for stylesheet selectors.
#[derive(Clone, Copy)]
struct CssNode<'a, 'input>(Node<'a, 'input>);

impl simplecss::Element for CssNode<'_, '_> {
    fn parent_element(&self) -> Option<Self> {
        self.0.parent_element().map(CssNode)
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.0.prev_sibling_element().map(CssNode)
    }

    fn has_local_name(&self, name: &str) -> bool {
        self.0.tag_name().name() == name
    }

    fn attribute_matches(&self, name: &str, operator: simplecss::AttributeOperator) -> bool {
        self.0.attribute(name).is_some_and(|v| operator.matches(v))
    }

    fn pseudo_class_matches(&self, class: simplecss::PseudoClass) -> bool {
        matches!(class, simplecss::PseudoClass::FirstChild) && self.prev_sibling_element().is_none()
    }
}

/// The `id` in `url(#id)`.
fn url_id(value: &str) -> Option<&str> {
    let inner = value.trim().strip_prefix("url(")?.split(')').next()?;
    inner.trim().trim_matches(['\'', '"']).strip_prefix('#')
}

/// The markers an element's start, mid and end vertices get, and the patterns it is filled
/// and stroked with (all inherited).
type Markers<'a, 'input> = [Option<Node<'a, 'input>>; 5];
const NO_MARKERS: Markers = [None; 5];

/// How many numbers `s` holds (`1.5e-3` is one, `.5.5` two).
fn numbers(s: &str) -> u64 {
    let b = s.as_bytes();
    let digits = |i: &mut usize| {
        let from = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > from
    };
    let (mut i, mut count) = (0, 0);
    while i < b.len() {
        let from = i;
        if matches!(b[i], b'+' | b'-') {
            i += 1;
        }
        let mut number = digits(&mut i);
        if b.get(i) == Some(&b'.') {
            i += 1;
            number |= digits(&mut i);
        }
        if !number {
            i = from + 1;
            continue;
        }
        count += 1;
        if matches!(b.get(i), Some(b'e' | b'E')) {
            let mark = i;
            i += 1;
            if matches!(b.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if !digits(&mut i) {
                i = mark;
            }
        }
    }
    count
}

/// Vertices usvg may put a mid marker on in path data: each command's numbers over the
/// numbers one segment takes. An arc becomes up to four curves.
fn path_vertices(d: &str) -> u64 {
    let is_command = |c: char| c.is_ascii_alphabetic() && !matches!(c, 'e' | 'E');
    let starts: Vec<usize> = d.match_indices(is_command).map(|(i, _)| i).collect();
    let mut total = 1u64;
    for (k, &at) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(d.len());
        let n = numbers(&d[at + 1..end]);
        total = total.saturating_add(match d.as_bytes()[at].to_ascii_lowercase() {
            b'h' | b'v' => n,
            b'c' => n.div_ceil(6),
            b's' | b'q' => n.div_ceil(4),
            b'a' => 4 * n.div_ceil(7),
            b'z' => 1,
            _ => n.div_ceil(2),
        });
    }
    total
}

/// A subtree once expanded: its elements, height, characters of text, and text elements
/// with the elements in them.
#[derive(Clone, Copy)]
struct Size {
    elements: u64,
    height: usize,
    chars: u64,
    texts: u64,
}

/// Elements usvg lays out as text, or as a chunk of one.
const TEXT_PARTS: [&str; 4] = ["text", "tspan", "tref", "textPath"];

struct Walk<'a, 'input> {
    ids: HashMap<&'a str, Node<'a, 'input>>,
    /// Stylesheet declarations that set references, by element.
    css: HashMap<usvg::roxmltree::NodeId, Vec<(&'a str, &'a str)>>,
    /// Each subtree visited, by the markers it inherits.
    memo: HashMap<(usvg::roxmltree::NodeId, Markers<'a, 'input>), Size>,
    /// Characters of each element's text, `tref` copies included.
    text: HashMap<usvg::roxmltree::NodeId, u64>,
    visiting: HashSet<usvg::roxmltree::NodeId>,
}

impl<'a, 'input> Walk<'a, 'input> {
    /// `node`'s properties in increasing priority: attributes, stylesheet, `style`.
    fn properties(&self, node: Node<'a, 'input>) -> Vec<(&'a str, &'a str)> {
        let mut out: Vec<(&str, &str)> = node.attributes().map(|a| (a.name(), a.value())).collect();
        out.extend(self.css.get(&node.id()).into_iter().flatten().copied());
        // Tokenized as usvg does: splitting on `;` and `:` missed `/**/mask:url(#m)`.
        let style = simplecss::DeclarationTokenizer::from(node.attribute("style").unwrap_or(""));
        out.extend(style.map(|d| (d.name, d.value)));
        out
    }

    /// The element `node` refers to with `href` (or `xlink:href`), on the elements that
    /// usvg follows it for.
    fn href(&self, node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
        if !HREFS.contains(&node.tag_name().name()) {
            return None;
        }
        let href = node
            .attribute("href")
            .or_else(|| node.attribute(("http://www.w3.org/1999/xlink", "href")))?;
        self.ids.get(href.strip_prefix('#')?).copied()
    }

    /// Refuses a cycle of references: usvg breaks only a reference to itself or a pair,
    /// and recursed through longer ones (three masks, each masking the next) until the
    /// stack overflowed. An element reaches its children and everything it refers to.
    fn refuse_cycles(&self, elements: &[Node<'a, 'input>]) -> Result<(), String> {
        let next = |n: Node<'a, 'input>| -> Vec<Node<'a, 'input>> {
            let refs = (self.properties(n).into_iter())
                .filter(|(name, _)| CYCLE_REFERENCES.contains(name))
                .filter_map(|(_, value)| self.ids.get(url_id(value)?).copied());
            n.children()
                .filter(|c| c.is_element())
                .chain(refs)
                .chain(self.href(n))
                .collect()
        };
        // 1: on the current path; 2: done.
        let mut state: HashMap<usvg::roxmltree::NodeId, u8> = HashMap::new();
        for &start in elements {
            if state.contains_key(&start.id()) {
                continue;
            }
            state.insert(start.id(), 1);
            let mut stack = vec![(start, next(start), 0)];
            while let Some((n, succ, i)) = stack.last_mut() {
                let Some(&m) = succ.get(*i) else {
                    state.insert(n.id(), 2);
                    stack.pop();
                    continue;
                };
                *i += 1;
                match state.get(&m.id()) {
                    Some(1) => return Err("SVG has a reference cycle".into()),
                    Some(_) => {}
                    None => {
                        state.insert(m.id(), 1);
                        stack.push((m, next(m), 0));
                    }
                }
            }
        }
        Ok(())
    }

    /// `node`'s subtree at `depth`, with `use`, `feImage` and filter references expanded
    /// in place and markers copied to the vertices they mark. Recursion is bounded by
    /// `MAX_DEPTH`.
    fn expanded(
        &mut self,
        node: Node<'a, 'input>,
        depth: usize,
        inherited: Markers<'a, 'input>,
    ) -> Result<Size, String> {
        if depth > MAX_DEPTH {
            return Err(too_deep());
        }
        if let Some(&size) = self.memo.get(&(node.id(), inherited)) {
            return if depth + size.height - 1 > MAX_DEPTH {
                Err(too_deep())
            } else {
                Ok(size)
            };
        }
        // A reference cycle (refused before, so only through a marker's own content).
        if !self.visiting.insert(node.id()) {
            return Ok(Size {
                elements: 0,
                height: 0,
                chars: 0,
                texts: 0,
            });
        }
        let properties = self.properties(node);
        let markers = self.markers(&properties, inherited);
        let mut size = Size {
            elements: 1u64.saturating_add(self.marker_copies(node, markers, depth)?),
            height: 1,
            chars: if node.has_tag_name("text") {
                self.text_chars(node, depth)?
            } else {
                0
            },
            texts: TEXT_PARTS.contains(&node.tag_name().name()).into(),
        };
        if size.chars > MAX_TEXT_PER_ELEMENT {
            return Err(format!(
                "SVG has a text element longer than {MAX_TEXT_PER_ELEMENT} characters"
            ));
        }
        // usvg copies a `use` target and an `feImage` source in place, and draws a filter
        // (with its `feImage` sources) for each element that has it.
        let filter = (properties.iter().rfind(|(name, _)| *name == "filter"))
            .and_then(|(_, value)| self.ids.get(url_id(value)?).copied());
        let copied = self
            .href(node)
            .filter(|_| node.has_tag_name("use") || node.has_tag_name("feImage"));
        for child in node
            .children()
            .filter(|c| c.is_element())
            .chain(copied)
            .chain(filter)
        {
            let c = self.expanded(child, depth + 1, markers)?;
            size.elements = size.elements.saturating_add(c.elements);
            size.chars = size.chars.saturating_add(c.chars);
            size.texts = size.texts.saturating_add(c.texts);
            size.height = size.height.max(c.height + 1);
            if size.elements > MAX_ELEMENTS {
                return Err(too_many());
            }
            if size.texts > MAX_TEXTS as u64 {
                return Err(too_many_texts());
            }
        }
        self.visiting.remove(&node.id());
        self.memo.insert((node.id(), inherited), size);
        Ok(size)
    }

    /// Characters of text under `node`, with the text `tref` elements copy.
    fn text_chars(&mut self, node: Node<'a, 'input>, depth: usize) -> Result<u64, String> {
        if depth > MAX_DEPTH {
            return Err(too_deep());
        }
        if let Some(&n) = self.text.get(&node.id()) {
            return Ok(n);
        }
        let mut chars = 0u64;
        for d in node.descendants() {
            if let Some(t) = d.text().filter(|_| d.is_text()) {
                chars += t.chars().count() as u64;
            } else if d.has_tag_name("tref")
                && let Some(target) = self.href(d)
            {
                chars = chars.saturating_add(self.text_chars(target, depth + 1)?);
            }
        }
        self.text.insert(node.id(), chars);
        Ok(chars)
    }

    /// `inherited` with the marker and pattern properties among `properties`.
    fn markers(
        &self,
        properties: &[(&str, &str)],
        mut markers: Markers<'a, 'input>,
    ) -> Markers<'a, 'input> {
        for &(name, value) in properties {
            let which = match name {
                "marker" => 0..3,
                "marker-start" => 0..1,
                "marker-mid" => 1..2,
                "marker-end" => 2..3,
                "fill" => 3..4,
                "stroke" => 4..5,
                _ => continue,
            };
            if value.trim() == "inherit" {
                continue;
            }
            // `none`, or anything but a reference to a marker (or pattern), means none.
            let tag = if which.start < 3 { "marker" } else { "pattern" };
            let target = url_id(value)
                .and_then(|id| self.ids.get(id).copied())
                .filter(|n| n.has_tag_name(tag));
            markers[which].fill(target);
        }
        markers
    }

    /// The markers `node` inherits from its ancestors.
    fn inherited(&self, node: Node<'a, 'input>) -> Markers<'a, 'input> {
        let ancestors: Vec<_> = node
            .ancestors()
            .skip(1)
            .filter(|a| a.is_element())
            .collect();
        (ancestors.into_iter().rev()).fold(NO_MARKERS, |m, a| self.markers(&self.properties(a), m))
    }

    /// Elements usvg adds to a shape for its markers: a copy of the start and end markers,
    /// and of the mid marker at each vertex. And the elements resvg draws for its fill and
    /// stroke patterns: a tile with their content for each shape (1,000 shapes filled with
    /// a pattern of 50,000 markers took 1.7 s to convert and 47.8 s to draw).
    fn marker_copies(
        &mut self,
        node: Node<'a, 'input>,
        [start, mid, end, fill, stroke]: Markers<'a, 'input>,
        depth: usize,
    ) -> Result<u64, String> {
        let attr = |name| node.attribute(name).unwrap_or("");
        let (start, mid, end, vertices) = match node.tag_name().name() {
            "path" => (start, mid, end, path_vertices(attr("d"))),
            "polyline" | "polygon" => (start, mid, end, numbers(attr("points")) / 2 + 1),
            "line" => (start, mid, end, 2),
            "rect" | "circle" | "ellipse" | "text" => (None, None, None, 0),
            _ => return Ok(0),
        };
        // A marker's content inherits from the marker's ancestors, not from the shape.
        let mut size = |m: Option<Node<'a, 'input>>| match m {
            Some(m) => {
                let inherited = self.inherited(m);
                self.expanded(m, depth + 1, inherited).map(|s| s.elements)
            }
            None => Ok(0),
        };
        let (start, mid, end) = (size(start)?, size(mid)?, size(end)?);
        let patterns = size(fill)?.saturating_add(size(stroke)?);
        Ok(start
            .saturating_add(end)
            .saturating_add(mid.saturating_mul(vertices))
            .saturating_add(patterns))
    }
}

/// The layers under a group: their drawing work and the most canvas areas of them alive
/// at once.
struct Layers {
    work: f64,
    live: f64,
}

/// Layer drawing work under `g`, in canvas areas: each isolated group's layer (as much of
/// it as resvg allocates) times one plus its filter primitives' cost, plus its mask and
/// clip path. `scale` is the pixels per SVG unit drawn at, which picks the blur resvg uses;
/// without one, every blur is priced as a box blur.
fn layer_work(g: &usvg::Group, canvas: (f64, f64), scale: Option<f32>) -> Layers {
    let (cw, ch) = (canvas.0.max(1.0), canvas.1.max(1.0));
    let mut work = 0.0;
    // The most alive below this group, and in it while its filters run.
    let (mut below, mut filtering) = (0.0f64, 0.0f64);
    for child in g.children() {
        if let usvg::Node::Group(child) = child {
            let c = layer_work(child, canvas, scale);
            work += c.work;
            below = below.max(c.live);
        }
    }
    if !g.should_isolate() {
        return Layers { work, live: below };
    }
    // resvg clips layers to two canvas sizes around the canvas.
    let r = g.abs_layer_bounding_box();
    let span = |a: f32, len: f32, c: f64| {
        let (lo, hi) = (a as f64, a as f64 + len as f64);
        (hi.min(3.0 * c) - lo.max(-2.0 * c)).max(0.0)
    };
    let area = span(r.x(), r.width(), cw) * span(r.y(), r.height(), ch) / (cw * ch);
    let (sx, sy) = g.abs_transform().get_scale();
    let mut weight = 1.0;
    for filter in g.filters() {
        // A filter keeps its source and each primitive's result.
        filtering = filtering.max(area * (1 + filter.primitives().len()) as f64);
        for p in filter.primitives() {
            weight += primitive_cost(p.kind(), scale.map(|s| (sx * s, sy * s)));
            // `feImage` draws its source on a layer of its own each time the filter
            // runs, so chains of them multiply (four per level, four levels deep took
            // 30 s and 700 MB).
            if let usvg::filter::Kind::Image(image) = p.kind() {
                let source = layer_work(image.root(), canvas, scale);
                work += 1.0 + source.work;
                below = below.max(source.live);
            }
        }
    }
    if let Some(mask) = g.mask() {
        weight += 1.0;
        let m = layer_work(mask.root(), canvas, scale);
        work += m.work;
        // The mask is drawn on a layer of its own, with its content's.
        below = below.max(area + m.live);
    }
    if let Some(clip) = g.clip_path() {
        weight += 1.0;
        let c = layer_work(clip.root(), canvas, scale);
        work += c.work;
        below = below.max(area + c.live);
    }
    work += area * weight;
    Layers {
        work,
        live: area + below.max(filtering),
    }
}

/// Work of a filter primitive per layer pixel, relative to a copy, at `scale` pixels per
/// unit of its coordinates. Measured over a 2800×8192 region, where a copy took about
/// 0.2 s: a blur took 1.7 s, or 9 s below a deviation of two pixels (resvg's IIR blur);
/// turbulence 2.7 s; lighting 1.2 s; blending, compositing, displacement, tiling and
/// component transfer about 1 s; a merge 0.25 s per input (200 inputs took 52 s).
fn primitive_cost(kind: &usvg::filter::Kind, scale: Option<(f32, f32)>) -> f64 {
    use usvg::filter::Kind;
    let blur = |dx: f32, dy: f32| match scale {
        Some((sx, sy)) if dx * sx < 2.0 && dy * sy < 2.0 => 45.0,
        _ => 8.0,
    };
    match kind {
        Kind::Morphology(m) => {
            (2.0 * m.radius_x().get() as f64 + 1.0) * (2.0 * m.radius_y().get() as f64 + 1.0)
        }
        Kind::ConvolveMatrix(c) => (c.matrix().columns() * c.matrix().rows()) as f64,
        Kind::Turbulence(t) => 13.0 + t.num_octaves() as f64,
        Kind::GaussianBlur(b) => blur(b.std_dev_x().get(), b.std_dev_y().get()),
        Kind::DropShadow(d) => 2.0 + blur(d.std_dev_x().get(), d.std_dev_y().get()),
        Kind::DiffuseLighting(_) | Kind::SpecularLighting(_) => 6.0,
        Kind::Merge(m) => m.inputs().len().max(1) as f64,
        Kind::Blend(_)
        | Kind::Composite(_)
        | Kind::DisplacementMap(_)
        | Kind::Tile(_)
        | Kind::ComponentTransfer(_) => 4.0,
        Kind::ColorMatrix(_) => 2.0,
        _ => 1.0,
    }
}

/// The SVG's own size in pixels (at least 1×1).
pub fn size(bytes: &[u8]) -> Result<(u32, u32), String> {
    guarded_with(
        |e| e,
        || {
            let c = stacker::grow(STACK, || tree(bytes, &Fonts::empty(), &AtomicUsize::new(0)))?;
            let s = c.tree.size();
            Ok((
                (s.width().ceil() as u32).max(1),
                (s.height().ceil() as u32).max(1),
            ))
        },
    )
}

/// Rasterizes at `scale` pixels per SVG unit.
pub fn rasterize(bytes: &[u8], scale: f32, fonts: &Fonts) -> Result<Raster, String> {
    rasterize_counting(bytes, scale, fonts).map(|(r, _)| r)
}

/// Like `rasterize`, also returning how many external references were refused.
pub fn rasterize_counting(
    bytes: &[u8],
    scale: f32,
    fonts: &Fonts,
) -> Result<(Raster, usize), String> {
    rasterize_within(bytes, scale, fonts, 0)
}

/// Like `rasterize_counting`, but an SVG too costly to draw at `scale` is drawn at half the
/// scale instead, down to a sixteenth, so a diagram that draws at one zoom still shows
/// (drawn larger, a little soft) at a higher one.
pub fn rasterize_fitting(
    bytes: &[u8],
    scale: f32,
    fonts: &Fonts,
) -> Result<(Raster, usize), String> {
    rasterize_within(bytes, scale, fonts, 4)
}

/// Rasterizes at `scale`, or at up to `halvings` halvings of it where it does not `fit`.
fn rasterize_within(
    bytes: &[u8],
    scale: f32,
    fonts: &Fonts,
    halvings: u32,
) -> Result<(Raster, usize), String> {
    guarded_with(
        |e| e,
        || {
            let refused = AtomicUsize::new(0);
            let raster = stacker::grow(STACK, || {
                let c = tree(bytes, fonts, &refused)?;
                let mut s = scale;
                for _ in 0..halvings {
                    if fits(&c, s).is_ok() {
                        break;
                    }
                    s /= 2.0;
                }
                fits(&c, s).map(|()| render(&c.tree, s))
            })?;
            Ok((raster, refused.into_inner()))
        },
    )
}

fn render(t: &usvg::Tree, scale: f32) -> Raster {
    let (w, h) = pixel_size(t.size(), scale);
    let scale = fit_scale(t.size(), scale);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("non-zero size");
    resvg::render(
        t,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied alpha; rasters are straight.
    let mut rgba = pixmap.take();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
            }
        }
    }
    Raster {
        width: w,
        height: h,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_path_vertices() {
        assert_eq!(numbers("1.5.5-2e3,4 - ."), 4);
        assert_eq!(numbers("12.25,30.5"), 2, "a decimal is one number");
        // Start, one line, three horizontal steps, an arc (up to four curves), close.
        assert_eq!(path_vertices("M0 0 L1 1 H1 2 3 a1 1 0 0 1 2 2 z"), 11);
        assert_eq!(path_vertices("M1e2-3.5 C1 2 3 4 5 6 7 8 9 10 11 12"), 4);
    }

    /// A `<!--` in a processing instruction hid the rest of the subset from the old scan, so
    /// an entity of 100,000 nested groups crashed roxmltree (SIGBUS); they are refused
    /// before it parses. So are entities that expand to much text, and nested ones.
    #[test]
    fn dtd_subsets_are_scanned_like_roxmltree() {
        let svg = |subset: &str, body: &str| {
            format!(
                r#"<!DOCTYPE svg [{subset}]><svg xmlns="http://www.w3.org/2000/svg">{body}</svg>"#
            )
        };
        let nested = "<g>".repeat(100_000) + &"</g>".repeat(100_000);
        let hidden = svg(
            &format!(r#"<?x <!-- ?><!ENTITY e "{nested}"><?y --> ?>"#),
            "&e;",
        );
        let err = check_markup(&hidden).unwrap_err();
        assert!(err.contains("entities"), "{err}");
        let many = svg(
            &format!(r#"<!ENTITY e "{}">"#, "A".repeat(100_000)),
            &format!("<desc>{}</desc>", "&e;".repeat(20_000)),
        );
        assert!(check_markup(&many).unwrap_err().contains("expand"));
        let inner = svg(r#"<!ENTITY a "AAAA"><!ENTITY b "&a;&a;">"#, "&b;");
        assert!(check_markup(&inner).unwrap_err().contains("entities"));
        // Illustrator's namespace entities, used a few times, are fine.
        let illustrator = svg(
            r#"<!ENTITY ns_svg "http://www.w3.org/2000/svg"><!-- c --><?pi x?><!ELEMENT x ANY>"#,
            r#"<desc>&ns_svg; &ns_svg;</desc>"#,
        );
        assert_eq!(check_markup(&illustrator), Ok(()));
    }

    /// `svg` is refused quickly with an error mentioning `why`, on a worker's small stack:
    /// when sized, or when drawn at `scale` if one is given.
    fn refused(svg: String, scale: Option<f32>, why: &str) {
        let start = std::time::Instant::now();
        let err = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || match scale {
                None => size(svg.as_bytes()).map(|_| ()),
                Some(scale) => rasterize(svg.as_bytes(), scale, &Fonts::empty()).map(|_| ()),
            })
            .unwrap()
            .join()
            .unwrap()
            .unwrap_err();
        assert!(err.contains(why), "{err}");
        // The inputs took 6–97 s before they were refused (release); a debug build
        // sharing the machine with the rest of the test run needs more slack than 2 s.
        let limit = if cfg!(debug_assertions) { 10 } else { 2 };
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(limit), "{took:?}");
    }

    /// A 2800×8192 SVG, the size of a tall diagram in a Retina column.
    fn tall(body: &str) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="2800" height="8192">{body}</svg>"#
        )
    }

    /// The text caps counted `text` elements in the document only: 1,000 one-character
    /// texts used 149 times took 6 s and 833 MB, and a text of 256 `tspan`s used 580 times
    /// 5.6 s. Text elements and their parts count once per copy.
    #[test]
    fn text_parts_count_use_copies() {
        let svg = |defs: &str, uses: usize| {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="1000"><defs><g id="g">{defs}</g></defs>{}</svg>"##,
                r##"<use href="#g"/>"##.repeat(uses)
            )
        };
        refused(
            svg(&"<text>a</text>".repeat(1000), 149),
            None,
            "too much text",
        );
        let spans = r#"<tspan x="1">a</tspan>"#.repeat(256);
        refused(
            svg(&format!("<text>{spans}</text>"), 580),
            None,
            "too much text",
        );
        // Directly too: 20,000 texts of seven `tspan`s took 7.1 s.
        let spans = r#"<tspan x="1">a</tspan>"#.repeat(7);
        refused(
            svg(&format!("<text>{spans}</text>").repeat(20_000), 1),
            None,
            "too much text",
        );
        // A few hundred labels used a few times are fine.
        assert!(size(svg(&"<text>label</text>".repeat(500), 20).as_bytes()).is_ok());
    }

    /// Painting had no bound beyond the element count: 20,000 opaque rectangles over a
    /// 2800×8192 canvas (1 KB through `use`) took 97 s, 200 translucent ones 16 s, one with
    /// a gradient of 20,000 stops 50 s, 100 lines of a million dashes each 51 s, and one
    /// path of 110,000 random lines 17 s. Each is refused before drawing.
    #[test]
    fn costly_painting_is_refused() {
        let rect = r#"<rect id="l0" width="2800" height="8192" fill="red"/>"#;
        let mut defs = String::from(rect);
        for level in 1..4 {
            let uses = format!(r##"<use href="#l{}"/>"##, level - 1).repeat(10);
            defs.push_str(&format!(r#"<g id="l{level}">{uses}</g>"#));
        }
        let uses = r##"<use href="#l3"/>"##.repeat(20);
        let overdraw = tall(&format!("<defs>{defs}</defs>{uses}"));
        assert!(overdraw.len() < 1100);
        refused(overdraw, Some(1.0), "costly");
        let translucent = r#"<rect width="2800" height="8192" fill="red" fill-opacity="0.5"/>"#;
        refused(tall(&translucent.repeat(200)), Some(1.0), "costly");
        let stops: String = (0..20_000)
            .map(|i| {
                format!(
                    r#"<stop offset="{}" stop-color="red"/>"#,
                    i as f32 / 20_000.0
                )
            })
            .collect();
        let gradient = format!(
            r#"<linearGradient id="g">{stops}</linearGradient><rect width="2800" height="8192" fill="url(#g)"/>"#
        );
        refused(tall(&gradient), Some(1.0), "costly");
        let dashed = r#"<path d="M0 0 L1000 1000" stroke="red" stroke-dasharray="0.001"/>"#;
        refused(tall(&dashed.repeat(100)), None, "dashes");
        let mut x = 88_172_645_463_325_252u64;
        let mut random = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % 1000
        };
        let lines: String = (0..110_000)
            .map(|_| format!("L{} {} ", random(), random()))
            .collect();
        let crossing = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="1000"><path d="M0 0 {lines}" fill="red"/></svg>"#
        );
        refused(crossing, None, "crossing");
        // The same drawn at a third of the size takes a ninth of the time, and is fine.
        let some = tall(&translucent.repeat(4));
        assert!(rasterize(some.as_bytes(), 1.0 / 3.0, &Fonts::empty()).is_ok());
        assert!(
            rasterize(
                tall(&translucent.repeat(100)).as_bytes(),
                0.05,
                &Fonts::empty()
            )
            .is_ok()
        );
    }

    /// Filter primitives were priced as a copy of the layer or two, but a merge of 200
    /// inputs took 52 s over a 2800×8192 canvas, a blur below two pixels of deviation 9 s
    /// (resvg's IIR blur), and turbulence 2.7 s.
    #[test]
    fn filter_primitives_cost_what_they_take() {
        let filtered = |primitive: &str| {
            tall(&format!(
                r#"<filter id="f" x="0" y="0" width="1" height="1">{primitive}</filter><rect width="2800" height="8192" fill="red" filter="url(#f)"/>"#
            ))
        };
        let merge = format!(
            "<feMerge>{}</feMerge>",
            r#"<feMergeNode in="SourceGraphic"/>"#.repeat(200)
        );
        refused(filtered(&merge), None, "costly");
        // The rest depend on the pixels drawn: sizing accepts them, drawing them this large
        // does not.
        let blur = r#"<feGaussianBlur stdDeviation="1.5"/>"#;
        let turbulence = r#"<feTurbulence baseFrequency="0.05"/>"#;
        for svg in [filtered(blur), filtered(&turbulence.repeat(2))] {
            assert!(size(svg.as_bytes()).is_ok());
            refused(svg, Some(1.0), "costly");
        }
        // A blur of two pixels or more is a box blur, and costs a few copies.
        let checked = |svg: &str| tree(svg.as_bytes(), &Fonts::empty(), &AtomicUsize::new(0));
        let wide_blur = filtered(r#"<feGaussianBlur stdDeviation="3"/>"#);
        assert_eq!(fits(&checked(&wide_blur).unwrap(), 1.0), Ok(()));
        // Over a small canvas, even the slow blur is quick.
        let small = filtered(blur).replace(
            r#"width="2800" height="8192""#,
            r#"width="500" height="500""#,
        );
        assert!(rasterize(small.as_bytes(), 1.0, &Fonts::empty()).is_ok());
    }

    /// A drop shadow priced as resvg's slow blur against a limit that ignored the size drawn
    /// refused ordinary cards and icons that draw in milliseconds.
    #[test]
    fn soft_shadows_draw() {
        let shadow = |w: u32, h: u32, filter: &str| {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}"><filter id="s">{filter}</filter><g filter="url(#s)"><rect x="4" y="4" width="{}" height="{}" fill="#36c"/></g></svg>"##,
                w - 20,
                h - 20
            )
        };
        let merged = r#"<feGaussianBlur in="SourceAlpha" stdDeviation="1.5"/><feOffset dx="2" dy="2" result="o"/><feMerge><feMergeNode in="o"/><feMergeNode in="SourceGraphic"/></feMerge>"#;
        let drop = r#"<feDropShadow dx="1" dy="1" stdDeviation="1"/>"#;
        let soft = r#"<feGaussianBlur stdDeviation="0.5"/>"#;
        for svg in [
            shadow(400, 300, merged),
            shadow(600, 400, drop),
            shadow(24, 24, soft),
            shadow(800, 500, r#"<feGaussianBlur stdDeviation="1"/>"#),
        ] {
            assert!(size(svg.as_bytes()).is_ok(), "{svg}");
            for scale in [1.0, 2.0, 3.0] {
                assert!(
                    rasterize(svg.as_bytes(), scale, &Fonts::empty()).is_ok(),
                    "{scale}: {svg}"
                );
            }
        }
    }

    /// An SVG too costly at the size asked for is drawn at a smaller scale, so a diagram
    /// that shows at one zoom does not turn blank at a higher one.
    #[test]
    fn too_costly_at_a_size_draws_smaller() {
        let translucent = r#"<rect width="2800" height="8192" fill="red" fill-opacity="0.5"/>"#;
        let svg = tall(&translucent.repeat(100));
        assert!(rasterize(svg.as_bytes(), 1.0, &Fonts::empty()).is_err());
        let (raster, _) = rasterize_fitting(svg.as_bytes(), 1.0, &Fonts::empty()).unwrap();
        assert!(
            raster.width < 2800 && raster.width >= 2800 / 16,
            "{}",
            raster.width
        );
        // Past a sixteenth, it stays refused.
        let svg = tall(&translucent.repeat(200_000 / 100));
        assert!(rasterize_fitting(svg.as_bytes(), 1.0, &Fonts::empty()).is_err());
    }

    /// Layers stay allocated while the layers inside them draw, and a filter keeps its
    /// source and every result: one filter over a region of eight canvases took 2.2 GB at
    /// 2800×8192, and fifteen nested translucent groups 1.5 GB.
    #[test]
    fn layer_memory_is_bounded_where_drawn() {
        let wide = tall(
            r#"<filter id="f" x="-1.5" y="-0.5" width="4" height="2"><feOffset dx="1"/></filter><rect width="2800" height="8192" filter="url(#f)"/>"#,
        );
        let nested = tall(
            &(r#"<g opacity="0.9">"#.repeat(15)
                + r#"<rect width="2800" height="8192"/>"#
                + &"</g>".repeat(15)),
        );
        for svg in [wide, nested] {
            // Sizing draws nothing.
            assert!(size(svg.as_bytes()).is_ok());
            refused(svg.clone(), Some(1.0), "memory");
            // At a quarter of the size, a sixteenth of the memory.
            assert!(rasterize(svg.as_bytes(), 0.25, &Fonts::empty()).is_ok());
        }
    }

    /// Every rule matching every element used to copy each declaration to it: 2,048
    /// colour rules over 8,190 elements took 2.3 s and 604 MB. Only `url(` values are kept,
    /// one per property and element.
    #[test]
    fn stylesheet_references_keep_one_url_per_property() {
        let css = |rule: &dyn Fn(usize) -> String| {
            let rules: String = (0..2048).map(rule).collect();
            let rects = r#"<rect width="1" height="1"/>"#.repeat(8190);
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg"><style>{rules}</style>{rects}</svg>"#
            )
        };
        let entries = |text: &str| {
            let doc = usvg::roxmltree::Document::parse(text).unwrap();
            let elements: Vec<Node> = doc.descendants().filter(|n| n.is_element()).collect();
            let mut sheet = simplecss::StyleSheet::new();
            sheet.parse_more(
                doc.descendants()
                    .find(|n| n.has_tag_name("style"))
                    .unwrap()
                    .text()
                    .unwrap(),
            );
            let map = css_references(&sheet, &elements);
            (
                map.len(),
                map.values().map(Vec::len).sum::<usize>(),
                map.values().next().map(|v| format!("{v:?}")),
            )
        };
        let colours = css(&|i| format!("* {{ fill: #{i:06x} }}"));
        assert_eq!(entries(&colours), (0, 0, None));
        assert!(check_structure(&usvg::roxmltree::Document::parse(&colours).unwrap()).is_ok());
        // `url(` values: the last rule wins, and `marker` sets all three.
        let urls = css(&|i| format!("* {{ fill: url(#g{i}); marker: url(#m) }}"));
        let (elements, total, first) = entries(&urls);
        assert_eq!((elements, total), (8192, 8192 * 4));
        let first = first.unwrap();
        assert!(first.contains(r#"("fill", "url(#g2047)")"#), "{first}");
        assert!(first.contains(r#"("marker-end", "url(#m)")"#), "{first}");
    }
}
