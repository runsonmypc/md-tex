//! Prompts (unsaved changes, opening a link in another app) drawn in the window in the
//! app's theme. Linux has no native alert, and GPUI's fallback is unstyled; macOS keeps
//! its native alerts.

use gpui::{
    App, AppContext, Context, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, PromptButton, PromptHandle, PromptLevel,
    PromptResponse, Render, RenderablePromptHandle, SharedString, StatefulInteractiveElement,
    Styled, Window, div, hsla, prelude::FluentBuilder, px,
};

/// Draws every prompt with [`Prompt`].
pub fn install(cx: &mut App) {
    cx.set_prompt_builder(build);
}

fn build(
    _level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    actions: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let view = cx.new(|cx| Prompt {
        message: message.to_owned().into(),
        detail: detail.map(|d| d.to_owned().into()),
        actions: actions.to_vec(),
        focus: cx.focus_handle(),
    });
    handle.with_view(view, window, cx)
}

/// A dialog card over a dimmed window. The first action is the default (Enter, drawn as
/// the primary button on the right); Escape picks Cancel.
pub struct Prompt {
    message: SharedString,
    detail: Option<SharedString>,
    actions: Vec<PromptButton>,
    focus: FocusHandle,
}

impl Prompt {
    /// The action Escape picks: Cancel if there is one, else the last.
    fn cancel(&self) -> usize {
        self.actions
            .iter()
            .position(PromptButton::is_cancel)
            .unwrap_or(self.actions.len().saturating_sub(1))
    }
}

impl EventEmitter<PromptResponse> for Prompt {}

impl Focusable for Prompt {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Prompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::view::theme_for(window, cx);
        let cancel = self.cancel();
        let button = |ix: usize, cx: &mut Context<Self>| {
            let primary = ix == 0;
            div()
                .id(ix)
                .px(px(14.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .text_sm()
                .cursor_pointer()
                .border_1()
                .when(primary, |d| {
                    d.bg(theme.link)
                        .border_color(theme.link)
                        .text_color(gpui::white())
                        .hover(|s| s.opacity(0.9))
                })
                .when(!primary, |d| {
                    d.border_color(theme.rule)
                        .text_color(theme.text)
                        .hover(|s| s.bg(theme.code_background))
                })
                .child(self.actions[ix].label().clone())
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(PromptResponse(ix));
                    cx.stop_propagation();
                }))
        };
        // Other actions (Don't Save) on the left; Cancel, then the default, on the right.
        let others: Vec<_> = (1..self.actions.len())
            .filter(|&i| i != cancel)
            .map(|i| button(i, cx))
            .collect();
        let mut right = Vec::new();
        if cancel != 0 {
            right.push(button(cancel, cx));
        }
        right.push(button(0, cx));
        let scrim = if theme.dark { 0.55 } else { 0.3 };
        div()
            .size_full()
            .absolute()
            .top_0()
            .left_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0.0, 0.0, 0.0, scrim))
            .occlude()
            .child(
                div()
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(move |_, e: &KeyDownEvent, _, cx| {
                        let ix = match e.keystroke.key.as_str() {
                            "enter" => 0,
                            "escape" => cancel,
                            _ => return,
                        };
                        cx.emit(PromptResponse(ix));
                        cx.stop_propagation();
                    }))
                    .w(px(420.0))
                    .max_w(gpui::relative(0.9))
                    .p(px(20.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.rule)
                    .bg(theme.surface)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .cursor_default()
                    .child(
                        div()
                            .text_color(theme.text)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.message.clone()),
                    )
                    .children(
                        self.detail
                            .clone()
                            .map(|d| div().text_sm().text_color(theme.muted).child(d)),
                    )
                    .child(
                        div()
                            .mt(px(14.0))
                            .flex()
                            .justify_between()
                            .gap(px(8.0))
                            .child(div().flex().gap(px(8.0)).children(others))
                            .child(div().flex().gap(px(8.0)).children(right)),
                    ),
            )
    }
}
