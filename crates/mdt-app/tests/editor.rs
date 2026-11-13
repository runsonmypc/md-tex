//! Source editor and view modes (groups 11 and 12). `harness = false`: GPUI's macOS text
//! system must be created on the main thread.

use std::sync::Arc;

use gpui::{
    AppContext, EntityInputHandler, HeadlessAppContext, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PlatformInput, WindowHandle, point, px, size,
};
use mdt_app::reader::heights::Anchor;
use mdt_app::settings::ViewMode;
use mdt_app::view::{DocumentView, Motion};

fn cx() -> HeadlessAppContext {
    HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    )
}

fn open(cx: &mut HeadlessAppContext, text: &str, width: f32) -> WindowHandle<DocumentView> {
    let doc = mdt_doc::Document::from_bytes(text.as_bytes(), None).unwrap();
    let h = cx
        .open_window(size(px(width), px(800.0)), |window, cx| {
            cx.new(|cx| DocumentView::new(doc, window, cx))
        })
        .unwrap();
    draw(cx, h);
    h
}

/// Renders a frame, so layout runs as it does on screen.
fn draw(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) {
    cx.run_until_parked();
    cx.capture_screenshot(h.into()).unwrap();
}

fn text(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) -> String {
    cx.update(|app| h.read(app).unwrap().doc.rope().to_string())
}

fn set_mode(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, mode: ViewMode, w: f32) {
    h.update(cx, |v, _, cx| v.set_mode(mode, w, cx)).unwrap();
    draw(cx, h);
}

fn typing(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, s: &str) {
    for c in s.chars() {
        h.update(cx, |v, window, cx| {
            v.replace_text_in_range(None, &c.to_string(), window, cx)
        })
        .unwrap();
    }
}

fn mode_cycle_and_reader_is_not_typed_into() {
    let mut cx = cx();
    let h = open(&mut cx, "# Hi\n", 1000.0);
    let cycle = |cx: &mut HeadlessAppContext, w: f32| {
        h.update(cx, |v, _, cx| {
            v.cycle_mode(w, cx);
            v.mode()
        })
        .unwrap()
    };
    assert_eq!(
        cx.update(|app| h.read(app).unwrap().mode()),
        ViewMode::Reader
    );
    assert_eq!(cycle(&mut cx, 1000.0), ViewMode::Source);
    assert_eq!(cycle(&mut cx, 1000.0), ViewMode::Split);
    assert_eq!(cycle(&mut cx, 1000.0), ViewMode::Reader);
    typing(&mut cx, h, "abc");
    assert_eq!(
        text(&mut cx, h),
        "# Hi\n",
        "typing in Reader changes nothing"
    );
    // A narrow window skips Split; choosing it directly leaves the mode and says why.
    assert_eq!(cycle(&mut cx, 400.0), ViewMode::Source);
    assert_eq!(cycle(&mut cx, 400.0), ViewMode::Reader);
    h.update(&mut cx, |v, _, cx| v.set_mode(ViewMode::Split, 400.0, cx))
        .unwrap();
    let (mode, notice) = cx.update(|app| {
        let v = h.read(app).unwrap();
        (
            v.mode(),
            v.notices.items().last().map(|n| n.message.to_string()),
        )
    });
    assert_eq!(mode, ViewMode::Reader);
    assert!(notice.unwrap().contains("wider window"));
}

fn input(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, e: PlatformInput) {
    cx.update_window(h.into(), |_, window, cx| {
        window.dispatch_event(e, cx);
    })
    .unwrap();
    draw(cx, h);
}

fn mode_switcher_shows_near_the_top_and_switches_mode() {
    let mut cx = cx();
    let h = open(&mut cx, "# Hi\n\nSome text.\n", 1000.0);
    let buttons =
        |cx: &mut HeadlessAppContext| cx.update(|app| h.read(app).unwrap().switcher_buttons());
    let pointer = |cx: &mut HeadlessAppContext, x: f32, y: f32| {
        input(
            cx,
            h,
            PlatformInput::MouseMove(MouseMoveEvent {
                position: point(px(x), px(y)),
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
        )
    };
    assert!(
        buttons(&mut cx).is_none(),
        "hidden until the pointer comes near"
    );
    pointer(&mut cx, 500.0, 400.0);
    assert!(
        buttons(&mut cx).is_none(),
        "the middle of the page does not show it"
    );
    pointer(&mut cx, 520.0, 30.0);
    let b = buttons(&mut cx).expect("shown near the top centre");
    let modes: Vec<ViewMode> = b.iter().map(|(m, _)| *m).collect();
    assert_eq!(modes, [ViewMode::Reader, ViewMode::Source, ViewMode::Split]);
    let source = b[1].1;
    assert!(source.size.width > px(0.0), "{source:?}");
    let at = source.center();
    for e in [
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
        }),
    ] {
        input(&mut cx, h, e);
    }
    assert_eq!(
        cx.update(|app| h.read(app).unwrap().mode()),
        ViewMode::Source
    );
    assert_eq!(text(&mut cx, h), "# Hi\n\nSome text.\n");
}

fn prompt_enter_picks_the_default_and_escape_cancels() {
    let mut cx = cx();
    cx.update(mdt_app::prompt::install);
    let h = open(&mut cx, "# Hi\n", 1000.0);
    for (key, want) in [("enter", 0), ("escape", 2)] {
        let answer = cx
            .update_window(h.into(), |_, window, cx| {
                window.prompt(
                    gpui::PromptLevel::Warning,
                    "Do you want to save the changes you made to notes.md?",
                    Some("Your changes will be lost if you don't save them."),
                    &["Save", "Don't Save", "Cancel"],
                    cx,
                )
            })
            .unwrap();
        draw(&mut cx, h);
        cx.update_window(h.into(), |_, window, cx| {
            window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), cx)
        })
        .unwrap();
        draw(&mut cx, h);
        let got = cx.foreground_executor().block_on(answer).expect("answered");
        assert_eq!(got, want, "{key}");
    }
}

fn split_window_shrunk_switches_to_source() {
    let mut cx = cx();
    let h = open(&mut cx, "# Hi\n", 1000.0);
    set_mode(&mut cx, h, ViewMode::Split, 1000.0);
    // Render runs this with the window's width on every frame.
    h.update(&mut cx, |v, _, _| v.fit_mode(400.0)).unwrap();
    let (mode, notice) = cx.update(|app| {
        let v = h.read(app).unwrap();
        (
            v.mode(),
            v.notices.items().last().map(|n| n.message.to_string()),
        )
    });
    assert_eq!(mode, ViewMode::Source);
    assert!(notice.unwrap().contains("wider window"));
}

fn divider_keeps_both_panes_at_least_240_px() {
    use mdt_app::view::divider_x;
    assert_eq!(divider_x(0.5, 1006.0), 500.0, "starts centered");
    assert_eq!(divider_x(0.05, 1006.0), 240.0);
    assert_eq!(divider_x(0.95, 1006.0), 1006.0 - 6.0 - 240.0);
}

fn long_doc() -> String {
    let mut s = String::new();
    for i in 0..60 {
        s.push_str(&format!(
            "Paragraph {i} has enough words in it to wrap across a couple of lines in a narrow pane, which makes proportional scrolling matter.\n\n"
        ));
        if i == 30 {
            s.push_str("# Results\n\n```mermaid\ngraph TD\n  A --> B\n```\n\n");
        }
    }
    s
}

fn unit_starting(
    cx: &mut HeadlessAppContext,
    h: WindowHandle<DocumentView>,
    prefix: &str,
) -> (usize, usize) {
    cx.update(|app| {
        let v = h.read(app).unwrap();
        let source = v.doc.rope().to_string();
        let r = v.reader.borrow();
        let u = r
            .model
            .units
            .iter()
            .position(|u| source[u.range.clone()].starts_with(prefix))
            .unwrap();
        (u, v.doc.rope().byte_to_line(r.model.units[u].range.start))
    })
}

fn top_line(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) -> String {
    cx.update(|app| {
        let v = h.read(app).unwrap();
        let e = v.editor.borrow();
        let (line, offset) = e.heights.find(e.scroll_y());
        assert!(offset < 1.0, "a line starts at the top");
        mdt_app::editor::text::line_text(v.doc.rope(), line)
    })
}

fn reader_to_source_keeps_the_top_block_and_the_cursor() {
    let mut cx = cx();
    let h = open(&mut cx, &long_doc(), 900.0);
    let (results, _) = unit_starting(&mut cx, h, "# Results");
    h.update(&mut cx, |v, _, _| {
        v.reader.borrow_mut().anchor = Anchor {
            unit: results,
            offset: 0.0,
        };
    })
    .unwrap();
    draw(&mut cx, h);
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    assert_eq!(top_line(&mut cx, h), "# Results");

    // Place the cursor, go to Reader, scroll, come back: the cursor stays, and the view
    // shows the block that was at the top of Reader.
    h.update(&mut cx, |v, _, cx| {
        v.editor.borrow_mut().selection = mdt_doc::Selection::cursor(10);
        v.cursor_moved(cx);
    })
    .unwrap();
    set_mode(&mut cx, h, ViewMode::Reader, 900.0);
    let (p50, _) = unit_starting(&mut cx, h, "Paragraph 50 ");
    h.update(&mut cx, |v, _, _| {
        v.reader.borrow_mut().anchor = Anchor {
            unit: p50,
            offset: 0.0,
        };
    })
    .unwrap();
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    assert!(top_line(&mut cx, h).starts_with("Paragraph 50 "));
    let sel = cx.update(|app| h.read(app).unwrap().editor.borrow().selection);
    assert_eq!(sel, mdt_doc::Selection::cursor(10), "cursor kept");
}

fn split_scroll_sync_follows_either_pane() {
    let mut cx = cx();
    let h = open(&mut cx, &long_doc(), 1000.0);
    set_mode(&mut cx, h, ViewMode::Split, 1000.0);
    // Editor → preview: a paragraph's first line at the top of the editor puts the
    // rendered paragraph at the top of the preview.
    let (p40, line) = unit_starting(&mut cx, h, "Paragraph 40 ");
    h.update(&mut cx, |v, _, _| {
        v.editor.borrow_mut().anchor = Anchor {
            unit: line,
            offset: 0.0,
        };
        v.sync_preview();
    })
    .unwrap();
    draw(&mut cx, h);
    let anchor = cx.update(|app| h.read(app).unwrap().reader.borrow().anchor);
    assert_eq!((anchor.unit, anchor.offset), (p40, 0.0));
    // Half-way through the paragraph's source is half-way through its rendering.
    h.update(&mut cx, |v, _, _| {
        let mut e = v.editor.borrow_mut();
        let half = e.heights.get(line) as f64 / 2.0;
        e.anchor = Anchor {
            unit: line,
            offset: half,
        };
        drop(e);
        v.sync_preview();
    })
    .unwrap();
    let (offset, height) = cx.update(|app| {
        let r = h.read(app).unwrap().reader.borrow();
        (r.anchor.offset, r.heights.get(p40) as f64)
    });
    assert!((offset / height - 0.5).abs() < 0.01, "{offset} of {height}");

    // Preview → editor: scrolling the preview to the diagram puts its fence at the top.
    let (diagram, _) = unit_starting(&mut cx, h, "```mermaid");
    h.update(&mut cx, |v, _, _| {
        v.reader.borrow_mut().anchor = Anchor {
            unit: diagram,
            offset: 0.0,
        };
        v.sync_editor();
    })
    .unwrap();
    assert_eq!(top_line(&mut cx, h), "```mermaid");
}

fn typing_groups_into_words_and_undo_survives_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.md");
    std::fs::write(&path, "").unwrap();
    let mut cx = cx();
    let doc = mdt_doc::Document::open(&path).unwrap();
    let h = cx
        .open_window(size(px(900.0), px(800.0)), |window, cx| {
            cx.new(|cx| DocumentView::new(doc, window, cx))
        })
        .unwrap();
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    typing(&mut cx, h, "hello world");
    assert_eq!(text(&mut cx, h), "hello world");
    h.update(&mut cx, |v, _, cx| v.undo(cx)).unwrap();
    assert_eq!(text(&mut cx, h), "hello ");
    let sel = cx.update(|app| h.read(app).unwrap().editor.borrow().selection);
    assert_eq!(sel, mdt_doc::Selection::cursor(6), "selection restored");
    h.update(&mut cx, |v, _, cx| v.redo(cx)).unwrap();
    // Edit, save, undo: the edit goes and the document is dirty again.
    h.update(&mut cx, |v, _, _| v.doc.save().unwrap()).unwrap();
    typing(&mut cx, h, "!");
    h.update(&mut cx, |v, _, _| v.doc.save().unwrap()).unwrap();
    h.update(&mut cx, |v, _, cx| v.undo(cx)).unwrap();
    let dirty = cx.update(|app| h.read(app).unwrap().doc.is_dirty());
    assert_eq!(text(&mut cx, h), "hello world");
    assert!(dirty);
}

fn paste_converts_line_breaks_and_is_one_step() {
    let mut cx = cx();
    let h = open(&mut cx, "a\r\nb\r\n", 900.0);
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    h.update(&mut cx, |v, _, cx| {
        v.editor.borrow_mut().selection = mdt_doc::Selection::cursor(3);
        v.paste_text("x\ny\n", cx);
    })
    .unwrap();
    assert_eq!(text(&mut cx, h), "a\r\nx\r\ny\r\nb\r\n");
    h.update(&mut cx, |v, _, cx| v.undo(cx)).unwrap();
    assert_eq!(text(&mut cx, h), "a\r\nb\r\n");
    // Cut puts plain text on the clipboard and removes it.
    h.update(&mut cx, |v, _, cx| {
        v.editor.borrow_mut().selection = mdt_doc::Selection { anchor: 0, head: 1 };
        v.cut(cx);
    })
    .unwrap();
    assert_eq!(text(&mut cx, h), "\r\nb\r\n");
}

fn tab_indents_selected_lines() {
    let mut cx = cx();
    let h = open(&mut cx, "  a\n  b\n  c\nd\n", 900.0);
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    h.update(&mut cx, |v, _, cx| {
        v.editor.borrow_mut().selection = mdt_doc::Selection {
            anchor: 2,
            head: 11,
        };
        v.indent(false, cx);
    })
    .unwrap();
    assert_eq!(text(&mut cx, h), "    a\n    b\n    c\nd\n");
    h.update(&mut cx, |v, _, cx| v.indent(true, cx)).unwrap();
    assert_eq!(text(&mut cx, h), "  a\n  b\n  c\nd\n");
    // No selection: one unit at the cursor.
    h.update(&mut cx, |v, _, cx| {
        v.editor.borrow_mut().selection = mdt_doc::Selection::cursor(12);
        v.indent(false, cx);
    })
    .unwrap();
    assert_eq!(text(&mut cx, h), "  a\n  b\n  c\n  d\n");
}

fn movement_keys() {
    let mut cx = cx();
    let family = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
    let h = open(
        &mut cx,
        &format!("{family}x hello world\nsecond line\n"),
        900.0,
    );
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    let step = |cx: &mut HeadlessAppContext, m: Motion| {
        h.update(cx, |v, _, cx| {
            v.move_cursor(m, false, cx);
            v.editor.borrow().selection.head
        })
        .unwrap()
    };
    assert_eq!(
        step(&mut cx, Motion::Right),
        family.len(),
        "emoji in one step"
    );
    let x = family.len() + 1;
    assert_eq!(
        step(&mut cx, Motion::WordRight),
        x,
        "end of the current word"
    );
    assert_eq!(
        step(&mut cx, Motion::WordRight),
        x + 6,
        "end of the next word"
    );
    assert_eq!(step(&mut cx, Motion::LineEnd), x + 12);
    assert_eq!(
        step(&mut cx, Motion::Down),
        x + 12 + 1 + 11,
        "goal x clamps to the shorter line"
    );
    assert_eq!(step(&mut cx, Motion::LineStart), x + 13);
    assert_eq!(step(&mut cx, Motion::Up), 0);
    assert_eq!(step(&mut cx, Motion::DocEnd), x + 25);
}

fn blink_off_schedules_no_frames() {
    let mut cx = cx();
    let h = open(&mut cx, "text\n", 900.0);
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    let scheduled = |cx: &mut HeadlessAppContext, focused: bool, blink: bool| {
        h.update(cx, |v, _, cx| {
            // Input just now, so a slow test run can't land in the cursor's off phase.
            v.editor.borrow_mut().last_input = std::time::Instant::now();
            let shown = v.update_blink(focused, blink, cx);
            (shown, v.blink_scheduled())
        })
        .unwrap()
    };
    assert_eq!(
        scheduled(&mut cx, true, false),
        (true, false),
        "solid, no frames"
    );
    assert_eq!(
        scheduled(&mut cx, true, true),
        (true, true),
        "solid after input, then blinks"
    );
    assert_eq!(
        scheduled(&mut cx, false, true),
        (false, false),
        "unfocused: no cursor"
    );
}

fn soft_wrap_changes_layout_not_text() {
    let mut cx = cx();
    let long = "word ".repeat(60);
    let h = open(&mut cx, &format!("{long}\nshort\n"), 900.0);
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    let visual_lines = |cx: &mut HeadlessAppContext, wrap: bool| {
        h.update(cx, |v, _, cx| {
            let rope = v.doc.rope().clone();
            v.editor.borrow_mut().set_soft_wrap(&rope, wrap);
            let fonts = mdt_app::reader::view::fonts(cx);
            let l = v
                .editor
                .borrow_mut()
                .line(0, &rope, &mut fonts.borrow_mut(), cx);
            let ranges: Vec<_> = l.paragraph.layout.lines().map(|x| x.text_range()).collect();
            ranges
        })
        .unwrap()
    };
    let wrapped = visual_lines(&mut cx, true);
    let unwrapped = visual_lines(&mut cx, false);
    assert!(wrapped.len() > 1, "wraps at the editor width");
    assert_eq!(unwrapped.len(), 1, "one visual line without wrap");
    assert_eq!(
        text(&mut cx, h),
        format!("{long}\nshort\n"),
        "text unchanged"
    );
    insta::assert_debug_snapshot!("editor_soft_wrap_lines", (wrapped, unwrapped));
}

/// Input latency, mode switch and preview update on the 1 MB document (performance
/// budgets). Handler plus layout time per keystroke stands in for event-to-frame-submit;
/// hard limits apply to release builds.
fn budgets_on_1_mb_document() {
    use std::time::{Duration, Instant};
    let mut cx = cx();
    let h = open(&mut cx, &mdt_bench::generate::text_1m(), 1000.0);
    let layout = |cx: &mut HeadlessAppContext| {
        h.update(cx, |v, _, cx| {
            let rope = v.doc.rope().clone();
            let fonts = mdt_app::reader::view::fonts(cx);
            v.editor
                .borrow_mut()
                .prepare(&rope, &mut fonts.borrow_mut(), cx);
        })
        .unwrap()
    };
    let t = Instant::now();
    h.update(&mut cx, |v, _, cx| v.set_mode(ViewMode::Source, 1000.0, cx))
        .unwrap();
    layout(&mut cx);
    let switch = t.elapsed();
    draw(&mut cx, h);
    // Type into the middle of the document.
    h.update(&mut cx, |v, _, cx| {
        let mid = v.doc.rope().len_bytes() / 2;
        let line = v.doc.rope().byte_to_line(mid);
        v.editor.borrow_mut().selection =
            mdt_doc::Selection::cursor(v.doc.rope().line_to_byte(line));
        v.cursor_moved(cx);
    })
    .unwrap();
    let mut times = Vec::new();
    for i in 0..1000 {
        let c = if i % 6 == 5 { " " } else { "x" };
        let t = Instant::now();
        h.update(&mut cx, |v, window, cx| {
            v.replace_text_in_range(None, c, window, cx)
        })
        .unwrap();
        layout(&mut cx);
        times.push(t.elapsed());
    }
    times.sort();
    let (p50, p99) = (times[500], times[990]);
    // Split: keystroke until the preview model holds it.
    set_mode(&mut cx, h, ViewMode::Split, 1000.0);
    let t = Instant::now();
    h.update(&mut cx, |v, window, cx| {
        v.replace_text_in_range(None, "Q", window, cx)
    })
    .unwrap();
    cx.run_until_parked();
    let landed = cx.update(|app| {
        let v = h.read(app).unwrap();
        let r = v.reader.borrow();
        let head = v.editor.borrow().selection.head;
        r.model.units.iter().any(|u| u.range.contains(&(head - 1)))
    });
    let preview = t.elapsed();
    assert!(landed, "preview has the edit");
    println!("  mode switch {switch:?}; keystroke p50 {p50:?}, p99 {p99:?}; preview {preview:?}");
    if !cfg!(debug_assertions) {
        assert!(
            switch <= Duration::from_millis(16),
            "mode switch {switch:?}"
        );
        assert!(p50 <= Duration::from_millis(2), "keystroke p50 {p50:?}");
        assert!(p99 <= Duration::from_millis(4), "keystroke p99 {p99:?}");
        assert!(preview <= Duration::from_millis(50), "preview {preview:?}");
    }
}

fn save_marks_title_and_reports_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.md");
    std::fs::write(&path, "a\n").unwrap();
    let mut cx = cx();
    let doc = mdt_doc::Document::open(&path).unwrap();
    let h = cx
        .open_window(size(px(900.0), px(800.0)), |window, cx| {
            cx.new(|cx| DocumentView::new(doc, window, cx))
        })
        .unwrap();
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    typing(&mut cx, h, "b");
    draw(&mut cx, h);
    let state = |cx: &mut HeadlessAppContext| {
        cx.update(|app| {
            let v = h.read(app).unwrap();
            (v.window_title(), v.doc.is_dirty())
        })
    };
    let linux = cfg!(target_os = "linux");
    assert_eq!(
        state(&mut cx),
        (mdt_app::view::title_text("note.md", true, linux), true)
    );
    // An injected failure: the target directory does not exist.
    let bad = dir.path().join("missing").join("note.md");
    let ok = h
        .update(&mut cx, |v, window, cx| v.write(Some(bad), window, cx))
        .unwrap();
    assert!(!ok);
    let notice = cx.update(|app| {
        h.read(app)
            .unwrap()
            .notices
            .items()
            .last()
            .map(|n| n.message.to_string())
    });
    assert!(notice.unwrap().starts_with("Could not save"));
    assert!(state(&mut cx).1, "still unsaved");
    assert!(
        h.update(&mut cx, |v, window, cx| v.write(None, window, cx))
            .unwrap()
    );
    assert_eq!(state(&mut cx), ("note.md".to_owned(), false));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ba\n");
}

fn read_only_save_is_refused_with_a_notice() {
    // Invalid UTF-8 opens read-only with a lossy decode, which must never be written.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("latin1.md");
    std::fs::write(&path, b"caf\xE9\n").unwrap();
    let mut cx = cx();
    let doc = mdt_doc::Document::open(&path).unwrap();
    let h = cx
        .open_window(size(px(900.0), px(800.0)), |window, cx| {
            cx.new(|cx| DocumentView::new(doc, window, cx))
        })
        .unwrap();
    draw(&mut cx, h);
    let notices = |cx: &mut HeadlessAppContext| -> Vec<String> {
        cx.update(|app| {
            h.read(app)
                .unwrap()
                .notices
                .items()
                .iter()
                .map(|n| n.message.to_string())
                .collect()
        })
    };
    let before = notices(&mut cx).len();
    h.update(&mut cx, |v, window, cx| v.save(window, cx))
        .unwrap();
    draw(&mut cx, h);
    let after = notices(&mut cx);
    assert_eq!(after.len(), before + 1, "{after:?}");
    assert!(
        after.last().unwrap().contains("cannot be saved"),
        "{after:?}"
    );
    h.update(&mut cx, |v, _, cx| {
        v.notices.dismiss_latest();
        cx.notify();
    })
    .unwrap();
    h.update(&mut cx, |v, window, cx| v.save_as(window, cx))
        .unwrap();
    draw(&mut cx, h);
    let after = notices(&mut cx);
    assert!(
        after.last().unwrap().contains("cannot be saved"),
        "{after:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"caf\xE9\n");
}

fn large_document_keeps_one_parse_in_flight() {
    let mut text = String::new();
    while text.len() < 200 * 1024 {
        text.push_str("A paragraph of filler text for the background parse.\n\n");
    }
    let mut cx = cx();
    let h = open(&mut cx, &text, 900.0);
    set_mode(&mut cx, h, ViewMode::Source, 900.0);
    let started = cx.update(|app| h.read(app).unwrap().background_parses());
    typing(&mut cx, h, "Typed. ");
    draw(&mut cx, h);
    let (parses, parsed, first) = cx.update(|app| {
        let v = h.read(app).unwrap();
        let r = v.reader.borrow();
        let first = match &r.model.units[0].block.kind {
            mdt_markdown::BlockKind::Paragraph { content, .. } => content.text.clone(),
            _ => String::new(),
        };
        (v.background_parses() - started, v.parsed(), first)
    });
    assert!(parsed);
    assert!(first.starts_with("Typed. A paragraph"), "{first:?}");
    assert!(parses <= 2, "{parses} parses for 7 keystrokes");
}

/// Runs a task to completion on the app and returns its result.
fn finish<T: 'static>(cx: &mut HeadlessAppContext, task: gpui::Task<T>) -> T {
    let out = std::rc::Rc::new(std::cell::RefCell::new(None));
    let o = out.clone();
    cx.update(|app| {
        app.spawn(async move |_| {
            *o.borrow_mut() = Some(task.await);
        })
        .detach()
    });
    cx.run_until_parked();
    out.borrow_mut().take().expect("task finished")
}

fn close_and_quit_ask_about_unsaved_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = cx();
    let mut windows = Vec::new();
    for name in ["a.md", "b.md"] {
        let path = dir.path().join(name);
        std::fs::write(&path, "x\n").unwrap();
        let doc = mdt_doc::Document::open(&path).unwrap();
        let h = cx
            .open_window(size(px(900.0), px(800.0)), |window, cx| {
                cx.new(|cx| DocumentView::new(doc, window, cx))
            })
            .unwrap();
        set_mode(&mut cx, h, ViewMode::Source, 900.0);
        typing(&mut cx, h, "y");
        windows.push(h);
    }
    let (a, b) = (windows[0], windows[1]);
    let answer = |cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, n: usize| {
        h.update(cx, |v, _, _| v.test_close_answer = Some(n))
            .unwrap();
    };
    let confirm = |cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>| {
        let task = h
            .update(cx, |v, window, cx| v.confirm_close(window, cx))
            .unwrap();
        finish(cx, task)
    };
    let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();

    answer(&mut cx, a, 2);
    assert!(!confirm(&mut cx, a), "Cancel keeps the window");
    answer(&mut cx, a, 1);
    assert!(confirm(&mut cx, a), "Don't Save closes");
    assert_eq!(read("a.md"), "x\n", "without saving");
    answer(&mut cx, a, 0);
    assert!(confirm(&mut cx, a), "Save closes");
    assert_eq!(read("a.md"), "yx\n", "after saving");

    // Closing through the window: Cancel keeps it open, Don't Save removes it.
    answer(&mut cx, b, 2);
    b.update(&mut cx, |v, window, cx| v.close(window, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|app| app.windows().len()), 2);

    // Quit asks about each unsaved window; Cancel on any of them stops the quit.
    typing(&mut cx, a, "z");
    answer(&mut cx, a, 0);
    answer(&mut cx, b, 2);
    let quit = |cx: &mut HeadlessAppContext| {
        let task = cx.update(|app| {
            app.spawn(async move |cx| mdt_app::view::confirm_quit(vec![a, b], cx).await)
        });
        finish(cx, task)
    };
    assert!(
        !quit(&mut cx),
        "cancelling the second prompt cancels the quit"
    );
    assert_eq!(read("a.md"), "yzx\n", "the first window was saved");
    answer(&mut cx, b, 1);
    assert!(quit(&mut cx));
    assert_eq!(read("b.md"), "x\n");

    b.update(&mut cx, |v, window, cx| v.close(window, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.update(|app| app.windows().len()),
        1,
        "Don't Save closed it"
    );
}

/// An `Editor` on `text` in a column `width` wide, with its visible lines laid out.
fn editor_on(
    app: &mut gpui::App,
    text: &str,
    width: f32,
    wrap: bool,
) -> (mdt_app::editor::Editor, ropey::Rope) {
    let rope = ropey::Rope::from_str(text);
    let mut e = mdt_app::editor::Editor::new(&rope, 1.0, wrap);
    e.resize(&rope, width, 800.0);
    let fonts = mdt_app::reader::view::fonts(app);
    e.prepare(&rope, &mut fonts.borrow_mut(), app);
    (e, rope)
}

/// End on a line wrapped inside a word (CJK has no spaces) goes past its last character
/// and leaves the caret on that line; End again stays, Home goes back to its start.
fn end_on_a_line_broken_inside_a_word() {
    let mut cx = cx();
    cx.update(|app| {
        let (mut e, rope) = editor_on(app, &"日本語の文章です".repeat(20), 200.0, true);
        let fonts = mdt_app::reader::view::fonts(app);
        let mut fonts = fonts.borrow_mut();
        let l = e.line(0, &rope, &mut fonts, app);
        let first = l.paragraph.layout.lines().next().unwrap().text_range();
        assert!(first.end < rope.len_bytes(), "the line wraps");
        let end = e.visual_line_edge(&rope, &mut fonts, app, 0, true);
        assert_eq!(end, first.end, "End stopped short");
        let (_, y, _) = e.caret(&rope, &mut fonts, app, end);
        assert_eq!(y, 0.0, "the caret moved to the next line");
        assert_eq!(e.visual_line_edge(&rope, &mut fonts, app, end, true), end);
        assert_eq!(e.visual_line_edge(&rope, &mut fonts, app, end, false), 0);
        // Wrapped at a space, End stays before it.
        drop(fonts);
        let (mut e, rope) = editor_on(app, &"word ".repeat(60), 200.0, true);
        let fonts = mdt_app::reader::view::fonts(app);
        let end = e.visual_line_edge(&rope, &mut fonts.borrow_mut(), app, 0, true);
        assert_eq!(&rope.to_string()[end - 1..=end], "d ");
    });
}

/// With soft wrap off, the sideways scroll limit follows the lines around the view: it
/// shrinks once the long line is gone.
fn sideways_scroll_limit_shrinks() {
    let mut cx = cx();
    cx.update(|app| {
        let (mut e, rope) = editor_on(app, &format!("{}\nshort\n", "x".repeat(500)), 400.0, false);
        assert!(e.scroll_horizontal(1e6));
        assert!(e.hscroll > 3000.0, "{}", e.hscroll);
        let short = ropey::Rope::from_str("short\nshort\n");
        e.lines_replaced(&short, 0, 0, 0);
        let fonts = mdt_app::reader::view::fonts(app);
        e.prepare(&short, &mut fonts.borrow_mut(), app);
        assert_eq!(e.hscroll, 0.0, "scrolled past the text");
        assert!(!e.scroll_horizontal(100.0), "the limit kept the long line");
        drop(rope);
    });
}

/// Lines wrap within the column at any width in a width bucket, not at the first width
/// the bucket saw.
fn wrapped_lines_fit_the_column() {
    let mut cx = cx();
    cx.update(|app| {
        let text = "日".repeat(200);
        let rope = ropey::Rope::from_str(&text);
        let fonts = mdt_app::reader::view::fonts(app);
        let mut fonts = fonts.borrow_mut();
        for bucket in (160..=240).step_by(8) {
            let column = bucket as f32;
            for k in 1..16 {
                let mut e = mdt_app::editor::Editor::new(&rope, 1.0, true);
                e.resize(&rope, column + k as f32 * 0.5, 800.0);
                e.resize(&rope, column, 800.0);
                let l = e.line(0, &rope, &mut fonts, app);
                for line in l.paragraph.layout.lines() {
                    let w = line.metrics().advance;
                    assert!(w <= column + 0.01, "{w} wide in a {column} px column");
                }
            }
        }
    });
}

/// A layout is reused while its line is unchanged, and laid out again after an edit of
/// the same length (a task toggled in Split) replaced the line.
fn line_layouts_follow_the_text() {
    let mut cx = cx();
    cx.update(|app| {
        let (mut e, rope) = editor_on(app, "- [ ] task\n", 400.0, true);
        let fonts = mdt_app::reader::view::fonts(app);
        let mut fonts = fonts.borrow_mut();
        let a = e.line(0, &rope, &mut fonts, app);
        assert!(std::rc::Rc::ptr_eq(&a, &e.line(0, &rope, &mut fonts, app)));
        let toggled = ropey::Rope::from_str("- [x] task\n");
        e.lines_replaced(&toggled, 0, 0, 0);
        assert!(!std::rc::Rc::ptr_eq(
            &a,
            &e.line(0, &toggled, &mut fonts, app)
        ));
    });
}

fn main() {
    // `cargo test --test editor -- <part of a name>` runs only the matching tests.
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let tests: [(&str, fn()); 22] = [
        (
            "end_on_a_line_broken_inside_a_word",
            end_on_a_line_broken_inside_a_word,
        ),
        (
            "sideways_scroll_limit_shrinks",
            sideways_scroll_limit_shrinks,
        ),
        ("wrapped_lines_fit_the_column", wrapped_lines_fit_the_column),
        ("line_layouts_follow_the_text", line_layouts_follow_the_text),
        (
            "read_only_save_is_refused_with_a_notice",
            read_only_save_is_refused_with_a_notice,
        ),
        (
            "large_document_keeps_one_parse_in_flight",
            large_document_keeps_one_parse_in_flight,
        ),
        (
            "close_and_quit_ask_about_unsaved_changes",
            close_and_quit_ask_about_unsaved_changes,
        ),
        (
            "save_marks_title_and_reports_failure",
            save_marks_title_and_reports_failure,
        ),
        ("budgets_on_1_mb_document", budgets_on_1_mb_document),
        (
            "mode_cycle_and_reader_is_not_typed_into",
            mode_cycle_and_reader_is_not_typed_into,
        ),
        (
            "mode_switcher_shows_near_the_top_and_switches_mode",
            mode_switcher_shows_near_the_top_and_switches_mode,
        ),
        (
            "prompt_enter_picks_the_default_and_escape_cancels",
            prompt_enter_picks_the_default_and_escape_cancels,
        ),
        (
            "split_window_shrunk_switches_to_source",
            split_window_shrunk_switches_to_source,
        ),
        (
            "divider_keeps_both_panes_at_least_240_px",
            divider_keeps_both_panes_at_least_240_px,
        ),
        (
            "reader_to_source_keeps_the_top_block_and_the_cursor",
            reader_to_source_keeps_the_top_block_and_the_cursor,
        ),
        (
            "split_scroll_sync_follows_either_pane",
            split_scroll_sync_follows_either_pane,
        ),
        (
            "typing_groups_into_words_and_undo_survives_save",
            typing_groups_into_words_and_undo_survives_save,
        ),
        (
            "paste_converts_line_breaks_and_is_one_step",
            paste_converts_line_breaks_and_is_one_step,
        ),
        ("tab_indents_selected_lines", tab_indents_selected_lines),
        ("movement_keys", movement_keys),
        (
            "blink_off_schedules_no_frames",
            blink_off_schedules_no_frames,
        ),
        (
            "soft_wrap_changes_layout_not_text",
            soft_wrap_changes_layout_not_text,
        ),
    ];
    for (name, test) in tests {
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        test();
        println!("test {name} ... ok");
    }
}
