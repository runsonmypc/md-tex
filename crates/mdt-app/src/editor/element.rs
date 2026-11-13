//! The editor's GPUI element: paints the visible lines, selection, IME underline, caret
//! and scroll indicator, registers the input handler, and handles the mouse.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, Bounds, ContentMask, ElementInputHandler, Entity, FocusHandle, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, ScrollWheelEvent,
    Styled, Window, canvas, point, px, size,
};
use mdt_doc::Selection;
use mdt_text::Rect;
use parley::{Affinity, Cursor};
use ropey::Rope;

use super::text::{line_range, line_with_break, word_at};
use super::{Editor, LineLayout, TOP_PAD};
use crate::reader::view::{brush_color, fonts, indicator_opacity, thumb};
use crate::theme::Theme;
use crate::view::DocumentView;

type Hook = Rc<dyn Fn(&mut Window, &mut App)>;
type OffsetHook = Rc<dyn Fn(usize, &mut Window, &mut App)>;
type FollowHook = Rc<dyn Fn(usize, &mut Window, &mut App) -> bool>;

/// What the element tells its view.
pub struct Hooks {
    /// Repaint.
    pub changed: Hook,
    /// The editor scrolled vertically.
    pub scrolled: Hook,
    /// The mouse placed the cursor or changed the selection.
    pub selected: Hook,
    /// Middle-click at a byte offset (Linux primary-selection paste).
    pub middle_click: OffsetHook,
    /// Cmd-click (macOS) or Ctrl-click (Linux) at a byte offset; true if it followed a
    /// link, and then the cursor stays where it was.
    pub follow: FollowHook,
}

struct Placed {
    y: f32,
    layout: Rc<LineLayout>,
    selection: Vec<Rect>,
    marked: Vec<Rect>,
    /// Find matches on the line, and whether each is the current one.
    found: Vec<(Rect, bool)>,
}

pub fn element(
    editor: Rc<RefCell<Editor>>,
    rope: Rope,
    theme: Theme,
    show_caret: bool,
    input: Option<(FocusHandle, Entity<DocumentView>)>,
    hooks: Hooks,
) -> impl IntoElement {
    let paint_rope = rope.clone();
    let paint_editor = editor.clone();
    let paint = canvas(
        move |bounds, _window, cx| {
            let rope = &paint_rope;
            let fonts = fonts(cx);
            let mut fonts = fonts.borrow_mut();
            let mut e = paint_editor.borrow_mut();
            let col = e.column(&mut fonts, f32::from(bounds.size.width));
            let top_pad = TOP_PAD * e.zoom;
            let viewport = (f32::from(bounds.size.height) - top_pad).max(1.0);
            e.resize(rope, col.width, viewport);
            e.origin = (
                f32::from(bounds.origin.x) + col.x,
                f32::from(bounds.origin.y) + top_pad,
            );
            let lines = e.prepare(rope, &mut fonts, cx);
            let scroll_y = e.scroll_y();
            let sel = e.range();
            let marked = e.marked.clone();
            let found_all = &e.found;
            let placed: Vec<Placed> = lines
                .into_iter()
                .map(|(i, layout)| {
                    let r = line_range(rope, i);
                    let local = |s: &std::ops::Range<usize>| {
                        s.start.max(r.start).min(r.end) - r.start
                            ..s.end.min(r.end).max(r.start) - r.start
                    };
                    let pl = &layout.paragraph.layout;
                    let mut selection = Vec::new();
                    if sel.start < sel.end && sel.start <= r.end && sel.end >= r.start {
                        let l = local(&sel);
                        if !l.is_empty() {
                            selection = mdt_text::selection_rects(pl, l);
                        }
                        // A selected line break shows as a sliver after the line.
                        if sel.end > r.end {
                            let g = Cursor::from_byte_index(pl, r.len(), Affinity::Upstream)
                                .geometry(pl, 1.0);
                            let h = if g.y1 > g.y0 {
                                (g.y1 - g.y0) as f32
                            } else {
                                e.line_h()
                            };
                            selection.push(Rect {
                                x: g.x0 as f32,
                                y: g.y0 as f32,
                                w: 6.0 * e.zoom,
                                h,
                            });
                        }
                    }
                    let marked = marked
                        .as_ref()
                        .filter(|m| m.start <= r.end && m.end >= r.start)
                        .map(|m| mdt_text::selection_rects(pl, local(m)))
                        .unwrap_or_default();
                    // Only the matches in view get rectangles: a 2 MB line of 1,000,000
                    // matches took 5 s a frame. Matches are sorted and do not overlap, so
                    // their ends are sorted too.
                    let top = e.heights.top(i);
                    let line_first = found_all.partition_point(|(m, _)| m.end <= r.start);
                    let in_line = found_all
                        .get(line_first)
                        .is_some_and(|(m, _)| m.start <= r.end);
                    // Text is drawn `col.x - hscroll` from the left edge, and the whole
                    // width shows (the margins too, when it scrolls sideways).
                    let visible = if in_line {
                        mdt_text::visible_bytes(
                            pl,
                            &Rect {
                                x: e.hscroll - col.x,
                                y: (scroll_y - top) as f32,
                                w: f32::from(bounds.size.width),
                                h: viewport,
                            },
                        )
                    } else {
                        0..0
                    };
                    let (from, to) = (r.start + visible.start, r.start + visible.end);
                    let first = found_all.partition_point(|(m, _)| m.end <= from);
                    let (ranges, current): (Vec<_>, Vec<_>) = found_all[first..]
                        .iter()
                        .take_while(|(m, _)| m.start < to)
                        .filter(|(m, _)| !local(m).is_empty())
                        .map(|(m, cur)| (local(m), *cur))
                        .unzip();
                    let found = mdt_text::ranges_rects(pl, &ranges)
                        .into_iter()
                        .zip(current)
                        .flat_map(|(rects, cur)| rects.into_iter().map(move |r| (r, cur)))
                        .collect();
                    Placed {
                        y: (e.heights.top(i) - scroll_y) as f32 + top_pad,
                        layout,
                        selection,
                        marked,
                        found,
                    }
                })
                .collect();
            let caret = (show_caret && sel.is_empty()).then(|| {
                let head = e.selection.head;
                let (x, y, h) = e.caret(rope, &mut fonts, cx, head);
                (x, (y - scroll_y) as f32 + top_pad, h)
            });
            let opacity = e
                .last_activity
                .map_or(0.0, |t| indicator_opacity(t.elapsed()));
            let (thumb_y, thumb_h) = thumb(
                scroll_y as f32,
                e.heights.total() as f32 + top_pad,
                f32::from(bounds.size.height),
            );
            let indicator = (opacity > 0.0 && thumb_h > 0.0).then_some((thumb_y, thumb_h, opacity));
            (col.x - e.hscroll, placed, caret, indicator, e.zoom)
        },
        move |bounds, (x, placed, caret, indicator, zoom), window, cx| {
            window.paint_quad(gpui::fill(bounds, theme.background));
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for p in &placed {
                    let origin = bounds.origin + point(px(x), px(p.y));
                    for (r, current) in &p.found {
                        let color = if *current {
                            theme.match_current
                        } else {
                            theme.match_highlight
                        };
                        let b =
                            Bounds::new(origin + point(px(r.x), px(r.y)), size(px(r.w), px(r.h)));
                        window.paint_quad(gpui::fill(b, color).corner_radii(px(2.0)));
                    }
                    mdt_text::paint(
                        &p.layout.ops,
                        &p.selection,
                        theme.selection,
                        origin,
                        |b| brush_color(&theme, b),
                        window,
                    );
                    for m in &p.marked {
                        let b = Bounds::new(
                            origin + point(px(m.x), px(m.y + m.h - 2.0)),
                            size(px(m.w), px(1.5)),
                        );
                        window.paint_quad(gpui::fill(b, theme.text));
                    }
                }
                if let Some((cx_, cy, h)) = caret {
                    let b = Bounds::new(
                        bounds.origin + point(px(x + cx_ - 0.5), px(cy)),
                        size(px(2.0 * zoom.max(1.0)), px(h)),
                    );
                    window.paint_quad(gpui::fill(b, theme.link));
                }
                if let Some((y, h, opacity)) = indicator {
                    let b = Bounds::new(
                        point(bounds.right() - px(9.0), bounds.top() + px(y)),
                        size(px(6.0), px(h)),
                    );
                    window.paint_quad(
                        gpui::fill(b, theme.muted.opacity(0.6 * opacity)).corner_radii(px(3.0)),
                    );
                }
            });
            if let Some((focus, view)) = &input {
                window.handle_input(focus, ElementInputHandler::new(bounds, view.clone()), cx);
            }
            if indicator.is_some() {
                window.request_animation_frame();
            }
            #[cfg(feature = "bench")]
            crate::bench::editor_painted();
        },
    )
    .size_full();

    let (down_editor, move_editor, wheel_editor, up_editor, mid_editor) = (
        editor.clone(),
        editor.clone(),
        editor.clone(),
        editor.clone(),
        editor,
    );
    let (down_rope, move_rope, mid_rope) = (rope.clone(), rope.clone(), rope);
    let Hooks {
        changed,
        scrolled,
        selected,
        middle_click,
        follow,
    } = hooks;
    let move_selected = selected.clone();
    gpui::div()
        .size_full()
        .cursor_text()
        .child(paint)
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, cx| {
            let fonts = fonts(cx);
            let mut e = down_editor.borrow_mut();
            let at = (f32::from(ev.position.x), f32::from(ev.position.y));
            let b = e.hit_test(&down_rope, &mut fonts.borrow_mut(), cx, at);
            let follow_mod = if cfg!(target_os = "macos") {
                ev.modifiers.platform
            } else {
                ev.modifiers.control
            };
            if follow_mod && ev.click_count == 1 {
                drop(e);
                if follow(b, window, cx) {
                    return;
                }
                e = down_editor.borrow_mut();
            }
            e.selection = match ev.click_count {
                1 if ev.modifiers.shift => Selection {
                    anchor: e.selection.anchor,
                    head: b,
                },
                1 => Selection::cursor(b),
                2 => {
                    let w = word_at(&down_rope, b);
                    Selection {
                        anchor: w.start,
                        head: w.end,
                    }
                }
                _ => {
                    let l = line_with_break(&down_rope, b);
                    Selection {
                        anchor: l.start,
                        head: l.end,
                    }
                }
            };
            e.goal_x = None;
            e.used = true;
            e.dragging = true;
            e.last_input = Instant::now();
            drop(e);
            selected(window, cx);
        })
        .on_mouse_move(move |ev: &MouseMoveEvent, window, cx| {
            let mut e = move_editor.borrow_mut();
            if !e.dragging || ev.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let fonts = fonts(cx);
            let at = (f32::from(ev.position.x), f32::from(ev.position.y));
            let b = e.hit_test(&move_rope, &mut fonts.borrow_mut(), cx, at);
            if b != e.selection.head {
                e.selection.head = b;
                e.last_input = Instant::now();
                drop(e);
                move_selected(window, cx);
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, _, _| {
            up_editor.borrow_mut().dragging = false;
        })
        .on_mouse_down(
            MouseButton::Middle,
            move |ev: &MouseDownEvent, window, cx| {
                let fonts = fonts(cx);
                let at = (f32::from(ev.position.x), f32::from(ev.position.y));
                let b =
                    mid_editor
                        .borrow_mut()
                        .hit_test(&mid_rope, &mut fonts.borrow_mut(), cx, at);
                middle_click(b, window, cx);
            },
        )
        .on_scroll_wheel(move |ev: &ScrollWheelEvent, window, cx| {
            let mut e = wheel_editor.borrow_mut();
            let d = ev.delta.pixel_delta(px(e.line_h()));
            let (mut dx, mut dy) = (-f32::from(d.x), -f32::from(d.y));
            if ev.modifiers.shift && dx == 0.0 {
                (dx, dy) = (dy, 0.0);
            }
            if dx.abs() > dy.abs() && e.scroll_horizontal(dx) {
                drop(e);
                changed(window, cx);
                return;
            }
            e.scroll_by(dy);
            e.last_activity = Some(Instant::now());
            drop(e);
            scrolled(window, cx);
        })
}
