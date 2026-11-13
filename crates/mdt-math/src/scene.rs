//! Typst frames to resolution-independent geometry.

#[cfg(feature = "typeset")]
use typst::layout::{Abs, Frame, FrameItem, Point, Transform};
#[cfg(feature = "typeset")]
use typst::text::Font;
#[cfg(feature = "typeset")]
use typst::visualize::{CurveItem, Geometry};

/// Math geometry in points at `REFERENCE_SIZE`. Y grows downward from the top.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MathScene {
    pub width: f32,
    pub height: f32,
    /// Distance from the top to the baseline.
    pub baseline: f32,
    pub glyphs: Vec<SceneGlyph>,
    pub rules: Vec<SceneRule>,
    pub paths: Vec<ScenePath>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneGlyph {
    /// Index into the world's font list.
    pub font: usize,
    pub size: f32,
    pub id: u16,
    /// Baseline origin of the glyph.
    pub x: f32,
    pub y: f32,
}

/// Axis-aligned filled rectangle (fraction bars, radical overbars, straight lines).
#[derive(Debug, Clone, PartialEq)]
pub struct SceneRule {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathCmd {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScenePath {
    pub cmds: Vec<PathCmd>,
    /// Some(width) for a stroked path, None for a filled one.
    pub stroke: Option<f32>,
}

impl MathScene {
    /// A compact, rounded listing for snapshot tests.
    pub fn describe(&self) -> String {
        use std::fmt::Write;
        let r = |v: f32| (v * 100.0).round() / 100.0;
        let mut s = format!(
            "size {} x {}, baseline {}\n",
            r(self.width),
            r(self.height),
            r(self.baseline)
        );
        for g in &self.glyphs {
            let _ = writeln!(
                s,
                "glyph {} font {} size {} at ({}, {})",
                g.id,
                g.font,
                r(g.size),
                r(g.x),
                r(g.y)
            );
        }
        for q in &self.rules {
            let _ = writeln!(s, "rule ({}, {}) {} x {}", r(q.x), r(q.y), r(q.w), r(q.h));
        }
        for p in &self.paths {
            let _ = writeln!(s, "path {} commands, stroke {:?}", p.cmds.len(), p.stroke);
        }
        s
    }
}

#[cfg(feature = "typeset")]
pub(crate) fn from_frame(frame: &Frame, fonts: &[Font]) -> MathScene {
    let mut scene = MathScene {
        width: frame.width().to_pt() as f32,
        height: frame.height().to_pt() as f32,
        baseline: find_baseline(frame, 0.0).unwrap_or(frame.height().to_pt()) as f32,
        ..Default::default()
    };
    walk(frame, Transform::identity(), fonts, &mut scene);
    scene
}

#[cfg(feature = "typeset")]
/// The page frame has no baseline of its own; the box frame nested inside it does.
fn find_baseline(frame: &Frame, y: f64) -> Option<f64> {
    if frame.has_baseline() {
        return Some(y + frame.baseline().to_pt());
    }
    frame.items().find_map(|(pos, item)| match item {
        FrameItem::Group(g) => find_baseline(&g.frame, y + pos.y.to_pt() + g.transform.ty.to_pt()),
        _ => None,
    })
}

#[cfg(feature = "typeset")]
fn apply(ts: Transform, p: Point) -> (f32, f32) {
    let p = p.transform(ts);
    (p.x.to_pt() as f32, p.y.to_pt() as f32)
}

#[cfg(feature = "typeset")]
fn walk(frame: &Frame, ts: Transform, fonts: &[Font], scene: &mut MathScene) {
    for (pos, item) in frame.items() {
        let local = ts.pre_concat(Transform::translate(pos.x, pos.y));
        match item {
            FrameItem::Group(group) => walk(
                &group.frame,
                local.pre_concat(group.transform),
                fonts,
                scene,
            ),
            FrameItem::Text(text) => {
                let font = fonts
                    .iter()
                    .position(|f| f == text.font.font())
                    .unwrap_or(0);
                let size = text.size.to_pt();
                // Advances and offsets are Y-up. Tall delimiters are vertical runs: each
                // piece of the assembly advances in y, not x.
                let (mut x, mut y) = (0.0_f64, 0.0_f64);
                for g in &text.glyphs {
                    let gx = x + g.x_offset.get() * size;
                    let gy = -(y + g.y_offset.get() * size);
                    let (px, py) = apply(local, Point::new(Abs::pt(gx), Abs::pt(gy)));
                    scene.glyphs.push(SceneGlyph {
                        font,
                        size: (size * local.sy.get()) as f32,
                        id: g.id,
                        x: px,
                        y: py,
                    });
                    x += g.x_advance.get() * size;
                    y += g.y_advance.get() * size;
                }
            }
            FrameItem::Shape(shape, _) => {
                let stroke = shape.stroke.as_ref().map(|s| s.thickness.to_pt() as f32);
                match &shape.geometry {
                    Geometry::Rect(size) if shape.fill.is_some() => {
                        let (x, y) = apply(local, Point::zero());
                        scene.rules.push(SceneRule {
                            x,
                            y,
                            w: size.x.to_pt() as f32,
                            h: size.y.to_pt() as f32,
                        });
                    }
                    Geometry::Line(to) => {
                        let (x0, y0) = apply(local, Point::zero());
                        let (x1, y1) = apply(local, *to);
                        let t = stroke.unwrap_or(0.0);
                        if (y0 - y1).abs() < 1e-4 {
                            scene.rules.push(SceneRule {
                                x: x0.min(x1),
                                y: y0 - t / 2.0,
                                w: (x1 - x0).abs(),
                                h: t,
                            });
                        } else {
                            scene.paths.push(ScenePath {
                                cmds: vec![PathCmd::Move(x0, y0), PathCmd::Line(x1, y1)],
                                stroke,
                            });
                        }
                    }
                    Geometry::Rect(size) => {
                        let (x, y) = apply(local, Point::zero());
                        let (w, h) = (size.x.to_pt() as f32, size.y.to_pt() as f32);
                        scene.paths.push(ScenePath {
                            cmds: vec![
                                PathCmd::Move(x, y),
                                PathCmd::Line(x + w, y),
                                PathCmd::Line(x + w, y + h),
                                PathCmd::Line(x, y + h),
                                PathCmd::Close,
                            ],
                            stroke,
                        });
                    }
                    Geometry::Curve(curve) => {
                        let at = |p: &Point| apply(local, *p);
                        let cmds = curve
                            .0
                            .iter()
                            .map(|c| match c {
                                CurveItem::Move(p) => {
                                    let (x, y) = at(p);
                                    PathCmd::Move(x, y)
                                }
                                CurveItem::Line(p) => {
                                    let (x, y) = at(p);
                                    PathCmd::Line(x, y)
                                }
                                CurveItem::Cubic(a, b, p) => {
                                    let ((ax, ay), (bx, by), (x, y)) = (at(a), at(b), at(p));
                                    PathCmd::Cubic(ax, ay, bx, by, x, y)
                                }
                                CurveItem::Close => PathCmd::Close,
                            })
                            .collect();
                        scene.paths.push(ScenePath {
                            cmds,
                            stroke: if shape.fill.is_some() { None } else { stroke },
                        });
                    }
                }
            }
            FrameItem::Image(..) | FrameItem::Link(..) | FrameItem::Tag(..) => {}
        }
    }
}
