//! View modes (Reader, Source, Split), position preservation across switches, Split
//! scroll sync, and the mode switcher shown while the pointer moves.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use super::DocumentView;
use crate::editor::text::line_of;
use crate::reader::heights::Anchor;
use crate::settings::ViewMode;
use crate::theme::Theme;

/// Each Split pane is at least this wide.
pub const MIN_PANE: f32 = 240.0;
pub const DIVIDER: f32 = 6.0;

/// The editor pane's width for a divider at `split` (a fraction of the window width),
/// keeping both panes at least `MIN_PANE` wide.
pub fn divider_x(split: f32, window_w: f32) -> f32 {
    (split * (window_w - DIVIDER)).clamp(MIN_PANE, (window_w - DIVIDER - MIN_PANE).max(MIN_PANE))
}

/// Whether a window this wide can show Split.
pub fn split_fits(window_w: f32) -> bool {
    window_w >= 2.0 * MIN_PANE + DIVIDER
}

/// The next mode in the Cmd/Ctrl+E cycle; Split is skipped when it does not fit.
pub fn next_mode(mode: ViewMode, window_w: f32) -> ViewMode {
    match mode {
        ViewMode::Reader => ViewMode::Source,
        ViewMode::Source if split_fits(window_w) => ViewMode::Split,
        ViewMode::Source | ViewMode::Split => ViewMode::Reader,
    }
}

/// The switcher fades out over this long once the pointer leaves it.
const SWITCHER_FADE: Duration = Duration::from_millis(300);
/// The pointer shows the switcher within this distance of the window's top edge, and
/// this far either side of its centre.
const SWITCHER_ZONE_H: f32 = 64.0;
const SWITCHER_ZONE_HALF_W: f32 = 220.0;

/// Switcher opacity `elapsed` after the pointer left it.
pub fn switcher_opacity(elapsed: Duration) -> f32 {
    (1.0 - elapsed.as_secs_f32() / SWITCHER_FADE.as_secs_f32()).max(0.0)
}

/// Whether a pointer at (`x`, `y`) is near the switcher, in a window `window_w` wide.
pub fn near_switcher(x: f32, y: f32, window_w: f32) -> bool {
    y <= SWITCHER_ZONE_H && (x - window_w / 2.0).abs() <= SWITCHER_ZONE_HALF_W
}

const SWITCHER_MODES: [(ViewMode, &str); 3] = [
    (ViewMode::Reader, "Reader"),
    (ViewMode::Source, "Source"),
    (ViewMode::Split, "Split"),
];

/// The Reader | Source | Split switcher at the top centre of the window, with the Wide
/// reader toggle (the way into both on Linux, which has no menu bar). It shows while the
/// pointer is near it.
#[derive(Default)]
pub struct Switcher {
    near: bool,
    hovered: bool,
    /// When the pointer left the switcher's area, for the fade.
    left: Option<Instant>,
    /// Each mode's button as last drawn, in `SWITCHER_MODES` order.
    buttons: Rc<RefCell<Vec<Bounds<Pixels>>>>,
}

impl Switcher {
    fn opacity(&self) -> f32 {
        if self.near || self.hovered {
            return 1.0;
        }
        self.left.map_or(0.0, |t| switcher_opacity(t.elapsed()))
    }

    fn set_near(&mut self, near: bool) -> bool {
        if near == self.near {
            return false;
        }
        self.near = near;
        if !near {
            self.left = Some(Instant::now());
        }
        true
    }
}

impl DocumentView {
    /// The pointer moved to `at` in a window `window_w` wide.
    pub fn pointer_moved(&mut self, at: (f32, f32), window_w: f32, cx: &mut Context<Self>) {
        if self.switcher.set_near(near_switcher(at.0, at.1, window_w)) {
            cx.notify();
        }
    }

    /// The pointer left the window.
    pub fn pointer_left(&mut self, cx: &mut Context<Self>) {
        if self.switcher.set_near(false) {
            cx.notify();
        }
    }

    /// Whether the switcher is drawn, and where its mode buttons were last drawn.
    pub fn switcher_buttons(&self) -> Option<Vec<(ViewMode, Bounds<Pixels>)>> {
        (self.switcher.opacity() > 0.0).then(|| {
            SWITCHER_MODES
                .iter()
                .map(|(m, _)| *m)
                .zip(self.switcher.buttons.borrow().iter().copied())
                .collect()
        })
    }

    pub(super) fn render_switcher(
        &mut self,
        theme: &Theme,
        window_w: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        // The find bar and the palette sit where the switcher would.
        let opacity = if self.find.is_some() || self.palette.is_some() {
            0.0
        } else {
            self.switcher.opacity()
        };
        if opacity <= 0.0 {
            return None;
        }
        if opacity < 1.0 {
            window.request_animation_frame();
        }
        let buttons = self.switcher.buttons.clone();
        let current = self.mode;
        let wide = self.wide_reader;
        let split_ok = split_fits(window_w);
        let (text, muted, selected) = (theme.text, theme.muted, theme.rule);
        Some(
            div()
                .absolute()
                .top(px(10.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .on_children_prepainted(move |b, _, _| *buttons.borrow_mut() = b)
                        .id("mode-switcher")
                        .occlude()
                        .flex()
                        .p(px(3.0))
                        .gap(px(2.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.rule)
                        .bg(theme.surface)
                        .shadow_md()
                        .text_sm()
                        .opacity(opacity)
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            this.switcher.hovered = *hovered;
                            if !*hovered && !this.switcher.near {
                                this.switcher.left = Some(Instant::now());
                            }
                            cx.notify();
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .children(SWITCHER_MODES.iter().map(|&(mode, label)| {
                            let enabled = mode != ViewMode::Split || split_ok;
                            div()
                                .id(label)
                                .px(px(12.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .text_color(muted)
                                .when(mode == current, |d| d.bg(selected).text_color(text))
                                .when(enabled && mode != current, |d| {
                                    d.cursor_pointer().hover(move |s| s.text_color(text))
                                })
                                .when(!enabled, |d| d.opacity(0.5))
                                .child(label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let w = f32::from(window.viewport_size().width);
                                    this.set_mode(mode, w, cx);
                                }))
                        }))
                        .child(div().w(px(1.0)).my(px(3.0)).mx(px(2.0)).bg(theme.rule))
                        .child(
                            div()
                                .id("wide-reader")
                                .px(px(12.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .text_color(muted)
                                .cursor_pointer()
                                .hover(move |s| s.text_color(text))
                                .when(wide, |d| d.bg(selected).text_color(text))
                                .child("Wide")
                                .on_click(cx.listener(|_, _, _, cx| {
                                    crate::app_settings::change(
                                        cx,
                                        |s| s.wide_reader = !s.wide_reader,
                                        "wide_reader",
                                    )
                                })),
                        ),
                ),
        )
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    pub fn editor_shown(&self) -> bool {
        self.mode != ViewMode::Reader
    }

    /// A Split window resized below the minimum switches to Source.
    pub fn fit_mode(&mut self, window_w: f32) {
        if self.mode == ViewMode::Split && !split_fits(window_w) {
            self.mode = ViewMode::Source;
            self.notices.push("Split needs a wider window.", &[]);
        }
    }

    pub fn cycle_mode(&mut self, window_w: f32, cx: &mut Context<Self>) {
        self.set_mode(next_mode(self.mode, window_w), window_w, cx);
    }

    /// Switches mode, keeping the block at the top of the view at the top.
    pub fn set_mode(&mut self, mode: ViewMode, window_w: f32, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        if mode == ViewMode::Split && !split_fits(window_w) {
            self.notices.push("Split needs a wider window.", &[]);
            cx.notify();
            return;
        }
        match (self.mode, mode) {
            (ViewMode::Reader, _) => self.editor_from_reader(),
            (_, ViewMode::Reader) => self.reader_from_editor(),
            (_, ViewMode::Split) => self.sync_preview(),
            _ => {}
        }
        self.mode = mode;
        cx.notify();
    }

    /// The unit holding byte `b`, or the first unit after it.
    fn unit_for_byte(&self, b: usize) -> Option<usize> {
        let r = self.reader.borrow();
        let units = &r.model.units;
        if units.is_empty() {
            return None;
        }
        Some(
            units
                .partition_point(|u| u.range.end <= b)
                .min(units.len() - 1),
        )
    }

    /// Puts the Reader's top block at the top of the editor; an unused editor also puts
    /// its cursor there.
    fn editor_from_reader(&mut self) {
        let r = self.reader.borrow();
        let Some(unit) = r.model.units.get(r.anchor.unit) else {
            return;
        };
        let b = unit.range.start;
        drop(r);
        let line = line_of(self.doc.rope(), b);
        let mut guard = self.editor.borrow_mut();
        let e = &mut *guard;
        e.anchor = Anchor {
            unit: line,
            offset: 0.0,
        };
        e.anchor.clamp(&e.heights);
        if !e.used {
            e.selection = mdt_doc::Selection::cursor(b);
        }
    }

    /// Puts the block at the top of the editor at the top of the Reader.
    fn reader_from_editor(&mut self) {
        let line = self.editor.borrow().anchor.unit;
        let b = self
            .doc
            .rope()
            .line_to_byte(line.min(self.doc.rope().len_lines() - 1));
        if let Some(u) = self.unit_for_byte(b) {
            let mut guard = self.reader.borrow_mut();
            let r = &mut *guard;
            r.anchor = Anchor {
                unit: u,
                offset: 0.0,
            };
            r.anchor.clamp(&r.heights);
        }
    }

    /// Editor document y range of a unit's source lines.
    fn editor_span(&self, unit: usize) -> (f64, f64) {
        let range = self.reader.borrow().model.units[unit].range.clone();
        let rope = self.doc.rope();
        let first = line_of(rope, range.start);
        let last = line_of(rope, range.end.saturating_sub(1).max(range.start)) + 1;
        let e = self.editor.borrow();
        (e.heights.top(first), e.heights.top(last))
    }

    /// Split: scrolls the preview to match the editor, in proportion within a block.
    pub fn sync_preview(&mut self) {
        let (line, y) = {
            let e = self.editor.borrow();
            (e.anchor.unit, e.scroll_y())
        };
        let rope = self.doc.rope();
        let b = rope.line_to_byte(line.min(rope.len_lines() - 1));
        let Some(u) = self.unit_for_byte(b) else {
            return;
        };
        let (top, bottom) = self.editor_span(u);
        let frac = if bottom > top {
            ((y - top) / (bottom - top)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mut guard = self.reader.borrow_mut();
        let r = &mut *guard;
        let h = r.heights.get(u) as f64;
        r.anchor = Anchor {
            unit: u,
            offset: frac * h,
        };
        r.anchor.clamp(&r.heights);
    }

    /// Split: scrolls the editor to match the preview.
    pub fn sync_editor(&mut self) {
        let (u, frac) = {
            let r = self.reader.borrow();
            if r.heights.is_empty() {
                return;
            }
            let h = r.heights.get(r.anchor.unit) as f64;
            let frac = if h > 0.0 { r.anchor.offset / h } else { 0.0 };
            (r.anchor.unit, frac)
        };
        let (top, bottom) = self.editor_span(u);
        let mut e = self.editor.borrow_mut();
        let (unit, offset) = e.heights.find(top + frac * (bottom - top));
        e.anchor = Anchor { unit, offset };
    }

    /// Split: keeps the block holding the cursor visible in the preview.
    pub fn reveal_cursor_in_preview(&mut self) {
        if self.mode != ViewMode::Split {
            return;
        }
        let head = self.editor.borrow().selection.head;
        let Some(u) = self.unit_for_byte(head) else {
            return;
        };
        let mut r = self.reader.borrow_mut();
        let top = r.anchor.scroll_y(&r.heights);
        let vp = r.viewport_h() as f64;
        let (ut, ub) = (r.heights.top(u), r.heights.top(u + 1));
        if ub <= top || ut >= top + vp {
            let (unit, offset) = r.heights.find((ut - vp / 3.0).max(0.0));
            r.anchor = Anchor { unit, offset };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switcher_fades_out_after_the_pointer_leaves() {
        assert_eq!(switcher_opacity(Duration::ZERO), 1.0);
        let mid = switcher_opacity(Duration::from_millis(150));
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        assert_eq!(switcher_opacity(Duration::from_millis(300)), 0.0);
        assert_eq!(switcher_opacity(Duration::from_secs(5)), 0.0);
    }

    #[test]
    fn only_the_top_centre_shows_the_switcher() {
        assert!(near_switcher(500.0, 20.0, 1000.0));
        assert!(near_switcher(300.0, 60.0, 1000.0));
        assert!(!near_switcher(500.0, 200.0, 1000.0), "lower down");
        assert!(!near_switcher(50.0, 20.0, 1000.0), "top left corner");
        assert!(!near_switcher(950.0, 20.0, 1000.0), "top right corner");
    }
}
