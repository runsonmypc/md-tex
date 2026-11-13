//! The per-window document view.

use gpui::{
    Context, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Window, div, prelude::FluentBuilder, px,
};
use std::cell::RefCell;
use std::rc::Rc;

use mdt_doc::Document;

use crate::editor::Editor;
use crate::reader::view::Reader;

use crate::keymap::*;
use crate::notices::Notices;
use crate::reader::view::Direction;
use crate::settings::ViewMode;
use crate::theme::{self, Theme, ThemeMode};

mod editing;
mod files;
mod find_bar;
mod links;
mod modes;
mod overlay;

pub use editing::Motion;
pub use files::{
    CONFLICT_KEEP, CONFLICT_RELOAD, confirm_quit, document_windows, quit, quit_now, title_text,
};
pub use links::Opener;
pub use modes::{MIN_PANE, divider_x, near_switcher, next_mode, split_fits, switcher_opacity};
pub use overlay::{Overlay, field_keys};

/// The theme mode for all windows. Settings set it; each window resolves it against its
/// own OS appearance on every render.
pub struct CurrentThemeMode(pub ThemeMode);

impl gpui::Global for CurrentThemeMode {}

pub fn theme_for(window: &Window, cx: &gpui::App) -> Theme {
    let mode = cx
        .try_global::<CurrentThemeMode>()
        .map_or(ThemeMode::System, |m| m.0);
    theme::resolve(mode, window.appearance())
}

pub struct DocumentView {
    pub doc: Document,
    pub notices: Notices,
    pub reader: Rc<RefCell<Reader>>,
    pub editor: Rc<RefCell<Editor>>,
    focus: FocusHandle,
    _appearance: gpui::Subscription,
    _activation: gpui::Subscription,
    parse_generation: u64,
    /// The generation the shown model comes from; behind `parse_generation` while a
    /// background parse runs.
    parsed_generation: u64,
    /// A background parse is running.
    parsing: bool,
    /// Background parses started, for tests.
    background_parses: u64,
    mode: ViewMode,
    /// Split divider position as a fraction of the window width.
    split: f32,
    dragging_divider: bool,
    /// In Split, the preview (not the editor) was clicked last: Copy and the movement
    /// keys go to it.
    preview_active: bool,
    blink_task: Option<gpui::Task<()>>,
    /// Soft wrap as last applied from settings.
    soft_wrap: bool,
    /// Wide reader as last applied from settings.
    wide_reader: bool,
    /// Zoom (percent) as last applied from settings.
    zoom: u16,
    /// The find bar, while open.
    pub find: Option<crate::find::Find>,
    find_focus: FocusHandle,
    /// The command palette, while open.
    pub palette: Option<crate::palette::Palette>,
    palette_focus: FocusHandle,
    /// The query of the last closed find bar, for Cmd/Ctrl+G.
    last_query: String,
    /// Counts model replacements, so find knows when to search again.
    model_epoch: u64,
    /// An anchor to go to once the full parse lands (it was not in the parsed prefix).
    pending_anchor: Option<String>,
    /// Positions to go back and forward to (links and heading jumps).
    back: Vec<links::Spot>,
    forward: Vec<links::Spot>,
    /// Tests answer the open-link confirmation with this button index.
    #[doc(hidden)]
    pub test_link_answer: Option<usize>,
    /// Watches the bound file for changes by other programs.
    watcher: Option<mdt_doc::FileWatcher>,
    /// The path a watch was last tried for, whether or not it worked.
    watch_tried: Option<std::path::PathBuf>,
    /// Watches started, for tests.
    watch_attempts: u64,
    /// The title and edited marker last shown.
    shown_title: Option<(String, bool)>,
    /// The close prompt was answered; the next close request goes through.
    close_confirmed: bool,
    /// Tests answer the close prompt with this button index instead of showing it.
    #[doc(hidden)]
    pub test_close_answer: Option<usize>,
    switcher: modes::Switcher,
}

impl DocumentView {
    pub fn new(doc: Document, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut notices = Notices::default();
        if doc.read_only().is_some() {
            notices.push("Editing is disabled: the file is not valid UTF-8.", &[]);
        }
        if let Some(state) = cx.try_global::<crate::app_settings::AppSettings>() {
            for p in &state.problems {
                notices.push(p.clone(), &[]);
            }
        }
        // `system` mode follows OS appearance changes live.
        let _appearance = cx.observe_window_appearance(window, |_, _, cx| cx.notify());
        // The caret hides while the window is inactive.
        let _activation = cx.observe_window_activation(window, |_, _, cx| cx.notify());
        let settings = crate::app_settings::current(cx);
        let zoom = settings.zoom as f32 / 100.0;
        let model = first_model(&doc);
        let mut editor = Editor::new(doc.rope(), zoom, settings.soft_wrap);
        editor.set_model(&model);
        let mut reader = Reader::new(model, zoom);
        reader.wide = settings.wide_reader;
        let reader = Rc::new(RefCell::new(reader));
        let mut this = Self {
            doc,
            notices,
            reader,
            editor: Rc::new(RefCell::new(editor)),
            focus: cx.focus_handle(),
            _appearance,
            _activation,
            parse_generation: 0,
            parsed_generation: 0,
            parsing: false,
            background_parses: 0,
            mode: settings.initial_view_mode,
            split: 0.5,
            dragging_divider: false,
            preview_active: false,
            blink_task: None,
            soft_wrap: settings.soft_wrap,
            wide_reader: settings.wide_reader,
            zoom: settings.zoom,
            find: None,
            find_focus: cx.focus_handle(),
            palette: None,
            palette_focus: cx.focus_handle(),
            last_query: String::new(),
            model_epoch: 0,
            pending_anchor: None,
            back: Vec::new(),
            forward: Vec::new(),
            test_link_answer: None,
            watcher: None,
            watch_tried: None,
            watch_attempts: 0,
            shown_title: None,
            close_confirmed: false,
            test_close_answer: None,
            switcher: modes::Switcher::default(),
        };
        // The window's close button asks about unsaved changes first.
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            view.update(cx, |this, cx| {
                if this.close_confirmed || !this.doc.is_dirty() {
                    return true;
                }
                this.close(window, cx);
                false
            })
            .unwrap_or(true)
        });
        // The first frame uses a parsed prefix; the full model replaces it when ready.
        if this.doc.rope().len_bytes() > PREFIX_BYTES {
            this.reparse(cx);
        }
        this
    }

    /// Parses the current text and swaps the model in: small documents at once, large
    /// ones in the background. At most one background parse runs; edits made meanwhile
    /// are parsed together once it finishes.
    pub fn reparse(&mut self, cx: &mut Context<Self>) {
        self.parse_generation += 1;
        if self.doc.rope().len_bytes() <= PREFIX_BYTES {
            self.parsed_generation = self.parse_generation;
            self.set_model(mdt_markdown::parse(&self.doc.rope().to_string()));
            self.full_model_landed(cx);
            cx.notify();
            return;
        }
        if !self.parsing {
            self.parse_in_background(cx);
        }
    }

    fn parse_in_background(&mut self, cx: &mut Context<Self>) {
        self.parsing = true;
        self.background_parses += 1;
        let generation = self.parse_generation;
        // Cloning the rope is cheap; the copy into one string happens off the UI thread.
        let rope = self.doc.rope().clone();
        let task = cx
            .background_executor()
            .spawn(async move { mdt_markdown::parse(&rope.to_string()) });
        cx.spawn(async move |this, cx| {
            let model = task.await;
            let _ = this.update(cx, |this, cx| {
                this.parsing = false;
                if this.parse_generation != generation {
                    // The text changed while parsing (or a small document was parsed at
                    // once): this model is out of date.
                    if !this.parsed() {
                        this.parse_in_background(cx);
                    }
                    return;
                }
                this.parsed_generation = generation;
                this.set_model(model);
                #[cfg(feature = "bench")]
                crate::bench::full_model();
                // A source search held back for this model runs now. Only a query typed
                // meanwhile moves to its match; otherwise the user was editing, so the
                // selection and the scroll stay.
                match &this.find {
                    Some(f) if f.waiting && f.query_changed() => this.find_changed(cx),
                    Some(f) if f.waiting => {
                        this.refresh_find();
                    }
                    _ => {}
                }
                this.full_model_landed(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// A link's anchor that was not in the parsed prefix is looked for in the full model.
    fn full_model_landed(&mut self, cx: &mut Context<Self>) {
        if let Some(f) = self.pending_anchor.take() {
            self.go_to_anchor(&f, cx);
        }
    }

    /// Whether the shown model is a parse of the whole current text.
    pub fn parsed(&self) -> bool {
        self.parsed_generation == self.parse_generation
    }

    /// Moves on with every change to the text (each starts a parse), so find can tell
    /// that its source ranges are out of date before the new model lands.
    pub(crate) fn text_generation(&self) -> u64 {
        self.parse_generation
    }

    /// Background parses started so far.
    #[doc(hidden)]
    pub fn background_parses(&self) -> u64 {
        self.background_parses
    }

    /// Counts model swaps; a new value means the views show a new parse.
    pub fn model_epoch(&self) -> u64 {
        self.model_epoch
    }

    /// Puts the cursor at the start of the line in the middle of the document.
    #[cfg(feature = "bench")]
    pub fn bench_cursor_mid(&mut self, cx: &mut Context<Self>) {
        let rope = self.doc.rope().clone();
        let line = rope.len_lines() / 2;
        let at = crate::editor::text::line_range(&rope, line).start;
        self.editor.borrow_mut().selection = mdt_doc::Selection::cursor(at);
        self.cursor_moved(cx);
    }

    fn set_model(&mut self, model: mdt_markdown::BlockModel) {
        self.model_epoch += 1;
        self.editor.borrow_mut().set_model(&model);
        self.reader.borrow_mut().set_model(model);
    }

    /// Flips a task checkbox (`[ ]` ↔ `[x]`) as one undoable edit.
    pub fn toggle_task(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        let Some(old) = self
            .doc
            .rope()
            .get_byte_slice(range.clone())
            .map(|s| s.to_string())
        else {
            return;
        };
        let new = match old.as_str() {
            "[ ]" => "[x]",
            "[x]" | "[X]" => "[ ]",
            _ => return,
        };
        let sel = mdt_doc::Selection::cursor(range.start);
        let line = crate::editor::text::line_of(self.doc.rope(), range.start);
        self.doc.seal_undo_group();
        if self.doc.edit(
            [(range, new.to_owned())],
            mdt_doc::EditKind::Other,
            sel,
            sel,
            std::time::Instant::now(),
        ) {
            self.doc.seal_undo_group();
            let rope = self.doc.rope().clone();
            self.editor
                .borrow_mut()
                .lines_replaced(&rope, line, line, line);
            self.reparse(cx);
        }
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some(sel) = self.doc.undo() {
            self.restore(sel, cx);
        }
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        if let Some(sel) = self.doc.redo() {
            self.restore(sel, cx);
        }
    }

    /// Whether the cursor blink has a frame scheduled.
    pub fn blink_scheduled(&self) -> bool {
        self.blink_task.is_some()
    }

    /// Schedules the next blink frame; returns whether the caret shows now. No frames are
    /// scheduled with blinking off, without an editor, or without focus.
    pub fn update_blink(&mut self, focused: bool, blink: bool, cx: &mut Context<Self>) -> bool {
        if !self.editor_shown() || !focused {
            self.blink_task = None;
            return false;
        }
        if !blink {
            self.blink_task = None;
            return true;
        }
        let since = self.editor.borrow().last_input.elapsed();
        let delay = Editor::blink_delay(since);
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
        Editor::blink_visible(since)
    }

    /// Whether keyboard commands go to the editor (Source, or Split with the editor last
    /// clicked).
    fn editor_active(&self) -> bool {
        match self.mode {
            ViewMode::Reader => false,
            ViewMode::Source => true,
            ViewMode::Split => !self.preview_active,
        }
    }

    fn motion(&mut self, m: Motion, select: bool, cx: &mut Context<Self>) {
        if self.editor_active() {
            self.move_cursor(m, select, cx);
            return;
        }
        let mut r = self.reader.borrow_mut();
        let vp = r.viewport_h();
        let line = crate::reader::block::BODY_SIZE * crate::reader::block::LINE_HEIGHT * r.zoom;
        match m {
            Motion::Up => r.scroll_by(-line),
            Motion::Down => r.scroll_by(line),
            Motion::PageUp => r.scroll_by(-vp),
            Motion::PageDown => r.scroll_by(vp),
            Motion::DocStart => r.scroll_by(-f32::MAX),
            Motion::DocEnd => r.scroll_by(f32::MAX),
            _ => return,
        }
        r.last_activity = Some(std::time::Instant::now());
        drop(r);
        if self.mode == ViewMode::Split {
            self.sync_editor();
        }
        cx.notify();
    }

    fn edit_action(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Context<Self>),
    ) {
        if self.editor_shown() {
            self.preview_active = false;
            f(self, cx);
        }
    }

    pub fn title_for(doc: &Document) -> String {
        doc.path().and_then(|p| p.file_name()).map_or_else(
            || "Untitled".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    fn render_notices(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (bg, fg, button) = (
            theme.overlay_background,
            theme.overlay_text,
            theme.overlay_button,
        );
        div()
            .absolute()
            .top(px(12.0))
            .right(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children(self.notices.items().iter().map(|n| {
                let id = n.id;
                div()
                    .id(("notice", id as usize))
                    .max_w(px(360.0))
                    .p(px(10.0))
                    .rounded(px(6.0))
                    .bg(bg)
                    .text_color(fg)
                    .text_sm()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(n.message.clone())
                    .child(div().flex().gap(px(6.0)).children(n.actions.iter().map(
                        |a: &SharedString| {
                            div()
                                .id(SharedString::from(format!("notice-{id}-{a}")))
                                .px(px(8.0))
                                .py(px(2.0))
                                .rounded(px(4.0))
                                .bg(button)
                                .cursor_pointer()
                                .child(a.clone())
                                .on_click({
                                    let a = a.clone();
                                    cx.listener(move |this, _, _, cx| {
                                        this.notice_action(id, &a, cx)
                                    })
                                })
                        },
                    )))
                    .when(n.actions.is_empty(), |d| {
                        d.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.notices.dismiss(id);
                                cx.notify();
                            }))
                    })
            }))
    }
}

impl DocumentView {
    fn extend(&mut self, dir: Direction, cx: &mut Context<Self>) {
        if self.editor_active() {
            let m = match dir {
                Direction::Left => Motion::Left,
                Direction::Right => Motion::Right,
                Direction::Up => Motion::Up,
                Direction::Down => Motion::Down,
            };
            self.move_cursor(m, true, cx);
            return;
        }
        let fonts = crate::reader::view::fonts(cx);
        self.reader
            .borrow_mut()
            .extend(&mut fonts.borrow_mut(), cx, dir);
        cx.notify();
    }
}

/// The Open dialog: each chosen file opens in its own window, or brings its window forward.
pub fn open_dialog(cx: &mut gpui::App) {
    let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: None,
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = rx.await {
            cx.update(|cx| {
                let errors = crate::launch::open_all(&paths, cx);
                crate::launch::report(errors, cx);
            });
        }
    })
    .detach();
}

/// Zooms every window one step in or out, or back to 100% (`None`), and saves the level.
fn zoom(cx: &mut gpui::App, up: Option<bool>) {
    let current = crate::app_settings::current(cx).zoom;
    let z = up.map_or(100, |up| crate::settings::zoom_step(current, up));
    if z != current {
        crate::app_settings::change(cx, |s| s.zoom = z, "zoom");
    }
}

/// Minimum prefix parsed synchronously for the first frame.
const PREFIX_BYTES: usize = 64 * 1024;

fn first_model(doc: &Document) -> mdt_markdown::BlockModel {
    let rope = doc.rope();
    if rope.len_bytes() <= PREFIX_BYTES {
        return mdt_markdown::parse(&rope.to_string());
    }
    // Enough text for the prefix rule plus slack to find a safe break, cut at the start
    // of the character that straddles the limit.
    let cut = rope.char_to_byte(rope.byte_to_char(rope.len_bytes().min(PREFIX_BYTES * 2)));
    let head: String = rope.byte_slice(..cut).to_string();
    let end = mdt_markdown::prefix::safe_prefix_end(&head, PREFIX_BYTES);
    mdt_markdown::parse(&head[..end])
}

impl Focusable for DocumentView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DocumentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "bench")]
        {
            let step = crate::bench::frame_start(self.model_epoch);
            if step != 0.0 {
                self.reader.borrow_mut().scroll_by(step);
            }
        }
        let theme = theme_for(window, cx);
        let settings = crate::app_settings::current(cx);
        let rope = self.doc.rope().clone();
        if settings.soft_wrap != self.soft_wrap {
            self.soft_wrap = settings.soft_wrap;
            self.editor
                .borrow_mut()
                .set_soft_wrap(&rope, settings.soft_wrap);
        }
        if settings.wide_reader != self.wide_reader {
            // The new column width reaches the reader's `resize` on the next paint.
            self.wide_reader = settings.wide_reader;
            self.reader.borrow_mut().wide = settings.wide_reader;
        }
        {
            // Images resolve against the file's folder and follow the remote setting.
            let mut r = self.reader.borrow_mut();
            let dir = self
                .doc
                .path()
                .and_then(|p| p.parent())
                .map(|p| p.to_owned());
            if r.images.doc_dir != dir || r.images.remote_allowed != settings.remote_images {
                r.images.doc_dir = dir;
                r.images.remote_allowed = settings.remote_images;
                r.content_landed();
            }
        }
        if settings.zoom != self.zoom {
            self.zoom = settings.zoom;
            let z = settings.zoom as f32 / 100.0;
            self.reader.borrow_mut().set_zoom(z);
            self.editor.borrow_mut().set_zoom(&rope, z);
        }
        self.sync_watch(cx);
        self.refresh_find();
        let window_w = f32::from(window.viewport_size().width);
        self.fit_mode(window_w);
        self.sync_title(window);
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let show_caret = self.update_blink(focused, settings.cursor_blink, cx);

        let root = div()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                let text = if this.editor_active() {
                    this.editor_selected_text()
                } else {
                    let source = this.doc.rope().to_string();
                    let fonts = crate::reader::view::fonts(cx);
                    this.reader
                        .borrow_mut()
                        .selected_text(&mut fonts.borrow_mut(), cx, &source)
                };
                if !text.is_empty() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
            }))
            .on_action(cx.listener(|this, _: &CopyAsMarkdown, _, cx| {
                let text = if this.editor_active() {
                    this.editor_selected_text()
                } else {
                    let source = this.doc.rope().to_string();
                    this.reader.borrow().selected_markdown(&source)
                };
                if !text.is_empty() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
            }))
            .on_action(cx.listener(|_, _: &Open, _, cx| open_dialog(cx)))
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.open_here(paths.paths(), None, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save(window, cx)))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &SaveAs, window, cx| this.save_as(window, cx)))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.redo(cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                if this.editor_active() {
                    let len = this.doc.rope().len_bytes();
                    this.editor.borrow_mut().selection = mdt_doc::Selection {
                        anchor: 0,
                        head: len,
                    };
                    this.doc.seal_undo_group();
                } else {
                    this.reader.borrow_mut().select_all();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ShowCharacterPalette, window, _| {
                if this.editor_active() {
                    window.show_character_palette();
                }
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| this.extend(Direction::Left, cx)))
            .on_action(
                cx.listener(|this, _: &SelectRight, _, cx| this.extend(Direction::Right, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.extend(Direction::Up, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.extend(Direction::Down, cx)))
            .on_action(
                cx.listener(|this, _: &MoveLeft, _, cx| this.motion(Motion::Left, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MoveRight, _, cx| this.motion(Motion::Right, false, cx)),
            )
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.motion(Motion::Up, false, cx)))
            .on_action(
                cx.listener(|this, _: &MoveDown, _, cx| this.motion(Motion::Down, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MoveWordLeft, _, cx| {
                    this.motion(Motion::WordLeft, false, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &MoveWordRight, _, cx| {
                this.motion(Motion::WordRight, false, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveLineStart, _, cx| {
                this.motion(Motion::LineStart, false, cx)
            }))
            .on_action(
                cx.listener(|this, _: &MoveLineEnd, _, cx| this.motion(Motion::LineEnd, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MoveDocStart, _, cx| {
                    this.motion(Motion::DocStart, false, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &MoveDocEnd, _, cx| this.motion(Motion::DocEnd, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PageUp, _, cx| this.motion(Motion::PageUp, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PageDown, _, cx| this.motion(Motion::PageDown, false, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| {
                this.motion(Motion::WordLeft, true, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| {
                this.motion(Motion::WordRight, true, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLineStart, _, cx| {
                this.motion(Motion::LineStart, true, cx)
            }))
            .on_action(
                cx.listener(|this, _: &SelectLineEnd, _, cx| {
                    this.motion(Motion::LineEnd, true, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectDocStart, _, cx| {
                this.motion(Motion::DocStart, true, cx)
            }))
            .on_action(
                cx.listener(|this, _: &SelectDocEnd, _, cx| this.motion(Motion::DocEnd, true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectPageUp, _, cx| this.motion(Motion::PageUp, true, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| {
                this.motion(Motion::PageDown, true, cx)
            }))
            .on_action(cx.listener(|this, _: &Backspace, _, cx| {
                this.edit_action(cx, |t, cx| t.delete(Motion::Left, cx))
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                this.edit_action(cx, |t, cx| t.delete(Motion::Right, cx))
            }))
            .on_action(cx.listener(|this, _: &DeleteWordLeft, _, cx| {
                this.edit_action(cx, |t, cx| t.delete(Motion::WordLeft, cx))
            }))
            .on_action(cx.listener(|this, _: &DeleteWordRight, _, cx| {
                this.edit_action(cx, |t, cx| t.delete(Motion::WordRight, cx))
            }))
            .on_action(cx.listener(|this, _: &DeleteLineLeft, _, cx| {
                this.edit_action(cx, |t, cx| t.delete(Motion::LineStart, cx))
            }))
            .on_action(
                cx.listener(|this, _: &Newline, _, cx| this.edit_action(cx, |t, cx| t.newline(cx))),
            )
            .on_action(cx.listener(|this, _: &Indent, _, cx| {
                this.edit_action(cx, |t, cx| t.indent(false, cx))
            }))
            .on_action(cx.listener(|this, _: &Outdent, _, cx| {
                this.edit_action(cx, |t, cx| t.indent(true, cx))
            }))
            .on_action(cx.listener(|this, _: &Cut, _, cx| this.edit_action(cx, |t, cx| t.cut(cx))))
            .on_action(
                cx.listener(|this, _: &Paste, _, cx| this.edit_action(cx, |t, cx| t.paste(cx))),
            )
            .on_action(cx.listener(|this, _: &CycleViewMode, window, cx| {
                let w = f32::from(window.viewport_size().width);
                this.cycle_mode(w, cx);
            }))
            .on_action(cx.listener(|this, _: &ViewReader, window, cx| {
                let w = f32::from(window.viewport_size().width);
                this.set_mode(ViewMode::Reader, w, cx);
            }))
            .on_action(cx.listener(|this, _: &ViewSource, window, cx| {
                let w = f32::from(window.viewport_size().width);
                this.set_mode(ViewMode::Source, w, cx);
            }))
            .on_action(cx.listener(|this, _: &ViewSplit, window, cx| {
                let w = f32::from(window.viewport_size().width);
                this.set_mode(ViewMode::Split, w, cx);
            }))
            .on_action(cx.listener(|_, _: &ZoomIn, _, cx| zoom(cx, Some(true))))
            .on_action(cx.listener(|_, _: &ZoomOut, _, cx| zoom(cx, Some(false))))
            .on_action(cx.listener(|_, _: &ZoomReset, _, cx| zoom(cx, None)))
            .on_action(cx.listener(|_, _: &ToggleWideReader, _, cx| {
                crate::app_settings::change(cx, |s| s.wide_reader = !s.wide_reader, "wide_reader")
            }))
            .on_action(
                cx.listener(|this, _: &CommandPalette, window, cx| this.open_palette(window, cx)),
            )
            .on_action(cx.listener(|this, _: &GoBack, _, cx| this.go_back(cx)))
            .on_action(cx.listener(|this, _: &GoForward, _, cx| this.go_forward(cx)))
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(false, window, cx)))
            .on_action(
                cx.listener(|this, _: &Replace, window, cx| this.open_find(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FindNext, window, cx| this.find_step(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FindPrevious, window, cx| this.find_step(false, window, cx)),
            )
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                if this.notices.dismiss_latest() {
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                let w = f32::from(window.viewport_size().width);
                this.pointer_moved((f32::from(e.position.x), f32::from(e.position.y)), w, cx);
            }))
            .on_mouse_exit(cx.listener(|this, _, _, cx| this.pointer_left(cx)))
            .size_full()
            .relative()
            .bg(theme.background)
            .text_color(theme.text);

        let reader = |this: &Self, theme: &Theme, cx: &mut Context<Self>| {
            crate::reader::view::element(
                this.reader.clone(),
                theme.clone(),
                {
                    let view = cx.entity().downgrade();
                    move |_, cx| {
                        let _ = view.update(cx, |_, cx| cx.notify());
                    }
                },
                {
                    let view = cx.entity().downgrade();
                    move |range, _, cx| {
                        let _ = view.update(cx, |this, cx| this.toggle_task(range, cx));
                    }
                },
                {
                    let view = cx.entity().downgrade();
                    move |link, window, cx| {
                        let _ = view.update(cx, |this, cx| this.follow_link(link, window, cx));
                    }
                },
            )
        };
        let content = match self.mode {
            ViewMode::Reader => root.child(reader(self, &theme, cx)),
            ViewMode::Source => root.child(self.editor_element(rope, &theme, show_caret, cx)),
            ViewMode::Split => {
                let left = divider_x(self.split, window_w);
                root.on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                    if this.dragging_divider && e.pressed_button == Some(MouseButton::Left) {
                        let w = f32::from(window.viewport_size().width);
                        this.split = (f32::from(e.position.x) / w).clamp(0.0, 1.0);
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.dragging_divider = false),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .size_full()
                        .child(
                            div()
                                .h_full()
                                .w(px(left))
                                .child(self.editor_element(rope, &theme, show_caret, cx)),
                        )
                        .child(
                            div()
                                .id("divider")
                                .h_full()
                                .w(px(modes::DIVIDER))
                                .flex()
                                .justify_center()
                                .cursor_col_resize()
                                .child(div().h_full().w(px(1.0)).bg(theme.rule))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, _| this.dragging_divider = true),
                                ),
                        )
                        .child(
                            div()
                                .h_full()
                                .flex_1()
                                .min_w(px(MIN_PANE))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _: &MouseDownEvent, _, _| {
                                        this.preview_active = true
                                    }),
                                )
                                .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                                    this.sync_editor();
                                    cx.notify();
                                }))
                                .child(reader(self, &theme, cx)),
                        ),
                )
            }
        };
        let find_focus = self.find_focus.clone();
        let find = self.render_find(&theme, &find_focus, window, cx);
        let palette_focus = self.palette_focus.clone();
        let palette = self.render_palette(&theme, &palette_focus, window, cx);
        let switcher = self.render_switcher(&theme, window_w, window, cx);
        content
            .children(switcher)
            .children(find)
            .children(palette)
            .child(self.render_notices(&theme, cx))
    }
}

impl DocumentView {
    fn editor_element(
        &self,
        rope: ropey::Rope,
        theme: &Theme,
        show_caret: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let view = cx.entity().downgrade();
        let follow_view = view.clone();
        let notify = {
            let view = view.clone();
            move |_: &mut Window, cx: &mut gpui::App| {
                let _ = view.update(cx, |_, cx| cx.notify());
            }
        };
        let scrolled = {
            let view = view.clone();
            move |_: &mut Window, cx: &mut gpui::App| {
                let _ = view.update(cx, |this, cx| {
                    if this.mode == ViewMode::Split {
                        this.sync_preview();
                    }
                    cx.notify();
                });
            }
        };
        let focus = self.focus.clone();
        let selected = {
            let view = view.clone();
            move |window: &mut Window, cx: &mut gpui::App| {
                window.focus(&focus, cx);
                let _ = view.update(cx, |this, cx| {
                    this.preview_active = false;
                    this.cursor_moved(cx);
                });
            }
        };
        let middle_click = move |_b: usize, _: &mut Window, _cx: &mut gpui::App| {
            #[cfg(target_os = "linux")]
            if let Some(text) = _cx.read_from_primary().and_then(|c| c.text()) {
                let _ = view.update(_cx, |this, cx| {
                    this.editor.borrow_mut().selection = mdt_doc::Selection::cursor(_b);
                    this.paste_text(&text, cx);
                });
            }
            #[cfg(not(target_os = "linux"))]
            let _ = &view;
        };
        crate::editor::element::element(
            self.editor.clone(),
            rope,
            theme.clone(),
            show_caret,
            Some((self.focus.clone(), cx.entity())),
            crate::editor::element::Hooks {
                changed: Rc::new(notify),
                scrolled: Rc::new(scrolled),
                selected: Rc::new(selected),
                middle_click: Rc::new(middle_click),
                follow: Rc::new(move |at, window, cx| {
                    follow_view
                        .update(cx, |this, cx| this.follow_link_at(at, window, cx))
                        .unwrap_or(false)
                }),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_model_cuts_large_documents_on_a_char_boundary() {
        // A three-byte character straddles the 128 KiB read for the first frame.
        let mut text = "word ".repeat((2 * PREFIX_BYTES - 1) / 5);
        text.push_str(&"a".repeat(2 * PREFIX_BYTES - 1 - text.len()));
        text.push_str("日本語\n\nmore text\n");
        assert!(!text.is_char_boundary(2 * PREFIX_BYTES));
        let doc = Document::from_bytes(text.as_bytes(), None).unwrap();
        let model = first_model(&doc);
        assert!(!model.units.is_empty());
    }
}
