//! Find and replace (group 14). `harness = false`: GPUI's macOS text system must be
//! created on the main thread.

use std::sync::Arc;

use gpui::{AppContext, HeadlessAppContext, WindowHandle, px, size};
use mdt_app::settings::ViewMode;
use mdt_app::view::DocumentView;

fn cx() -> HeadlessAppContext {
    HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    )
}

fn open(cx: &mut HeadlessAppContext, text: &str) -> WindowHandle<DocumentView> {
    let doc = mdt_doc::Document::from_bytes(text.as_bytes(), None).unwrap();
    let h = cx
        .open_window(size(px(1000.0), px(800.0)), |window, cx| {
            cx.new(|cx| DocumentView::new(doc, window, cx))
        })
        .unwrap();
    draw(cx, h);
    h
}

fn draw(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) {
    cx.run_until_parked();
    cx.capture_screenshot(h.into()).unwrap();
}

fn mode(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, mode: ViewMode) {
    h.update(cx, |v, _, cx| v.set_mode(mode, 1000.0, cx))
        .unwrap();
    draw(cx, h);
}

/// Opens the find bar and types `query` into it, a key at a time.
fn find(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, query: &str, replace: bool) {
    h.update(cx, |v, window, cx| v.open_find(replace, window, cx))
        .unwrap();
    for c in query.chars() {
        h.update(cx, |v, _, cx| v.find_type(&c.to_string(), cx))
            .unwrap();
    }
    draw(cx, h);
}

fn status(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) -> String {
    cx.update(|app| {
        h.read(app)
            .unwrap()
            .find
            .as_ref()
            .map_or_else(String::new, |f| f.status())
    })
}

/// A document with `latency` in sections far apart, so finding one scrolls.
fn long_doc() -> String {
    let mut s = String::new();
    for i in 0..60 {
        s.push_str(&format!(
            "## Section {i}\n\nFiller paragraph number {i}.\n\n"
        ));
        if i % 20 == 10 {
            s.push_str("The latency budget is tight.\n\n");
        }
    }
    s
}

fn incremental_find_scrolls_to_the_nearest_match_and_wraps() {
    let mut cx = cx();
    let h = open(&mut cx, &long_doc());
    find(&mut cx, h, "latency", false);
    assert_eq!(status(&mut cx, h), "1 of 3");
    // The current match is on screen: its unit is between the top unit and the bottom.
    let (anchor, unit, highlighted) = cx.update(|app| {
        let v = h.read(app).unwrap();
        let f = v.find.as_ref().unwrap();
        let r = v.reader.borrow();
        let n: usize = r.found.values().map(Vec::len).sum();
        (r.anchor.unit, f.current_hit().unwrap().unit, n)
    });
    assert_eq!(highlighted, 3, "every match is highlighted");
    assert!(
        anchor > 0 && anchor <= unit,
        "scrolled to it: {anchor} {unit}"
    );
    // Next twice reaches the last; next again wraps to the first.
    for want in ["2 of 3", "3 of 3", "1 of 3"] {
        h.update(&mut cx, |v, window, cx| v.find_step(true, window, cx))
            .unwrap();
        assert_eq!(status(&mut cx, h), want);
    }
    h.update(&mut cx, |v, window, cx| v.find_step(false, window, cx))
        .unwrap();
    assert_eq!(status(&mut cx, h), "3 of 3", "previous wraps to the last");
    // Escape closes the bar and removes the highlights.
    h.update(&mut cx, |v, window, cx| v.close_find(window, cx))
        .unwrap();
    let left = cx.update(|app| h.read(app).unwrap().reader.borrow().found.len());
    assert_eq!(left, 0);
}

fn escape_in_source_keeps_the_match_selected() {
    let mut cx = cx();
    let h = open(&mut cx, "alpha beta\n\ngamma beta\n");
    mode(&mut cx, h, ViewMode::Source);
    find(&mut cx, h, "beta", false);
    h.update(&mut cx, |v, window, cx| v.find_step(true, window, cx))
        .unwrap();
    h.update(&mut cx, |v, window, cx| v.close_find(window, cx))
        .unwrap();
    let (sel, found, open) = cx.update(|app| {
        let v = h.read(app).unwrap();
        let e = v.editor.borrow();
        (e.range(), e.found.len(), v.find.is_some())
    });
    assert!(!open);
    assert_eq!(sel, 18..22, "the second beta stays selected");
    assert_eq!(found, 0, "highlights removed");
}

fn replace_all_is_one_undo_step() {
    let mut cx = cx();
    let mut text = String::new();
    for i in 0..12 {
        text.push_str(&format!("The colour of item {i}.\n\n"));
    }
    let h = open(&mut cx, &text);
    mode(&mut cx, h, ViewMode::Source);
    find(&mut cx, h, "colour", true);
    assert_eq!(status(&mut cx, h), "1 of 12");
    h.update(&mut cx, |v, _, cx| {
        v.find.as_mut().unwrap().in_replace = true;
        v.find_type("color", cx);
        v.replace_all(cx);
    })
    .unwrap();
    let after = cx.update(|app| h.read(app).unwrap().doc.rope().to_string());
    assert_eq!(after.matches("color ").count(), 12);
    assert_eq!(after.matches("colour").count(), 0);
    assert_eq!(status(&mut cx, h), "No results");
    h.update(&mut cx, |v, _, cx| v.undo(cx)).unwrap();
    let undone = cx.update(|app| h.read(app).unwrap().doc.rope().to_string());
    assert_eq!(undone, text, "one undo restores all 12");
}

fn replace_all_in_a_large_document_then_find() {
    // Over the synchronous parse limit, so the model is reparsed in the background and
    // its ranges trail the replaced text for a moment; an é sits in every paragraph.
    let text = "colour é\n\n".repeat(200 * 1024 / 11);
    let paragraphs = text.matches("colour").count();
    let mut cx = cx();
    let h = open(&mut cx, &text);
    mode(&mut cx, h, ViewMode::Source);
    find(&mut cx, h, "colour", true);
    assert_eq!(status(&mut cx, h), format!("1 of {paragraphs}"));
    let hits = |v: &DocumentView| -> Vec<String> {
        let rope = v.doc.rope();
        v.find
            .as_ref()
            .unwrap()
            .hits
            .iter()
            .map(|hit| rope.byte_slice(hit.range.clone()).to_string())
            .collect()
    };
    // Replace, then search for the new word before the parse catches up.
    let early = h
        .update(&mut cx, |v, _, cx| {
            v.find.as_mut().unwrap().in_replace = true;
            v.find_type("color", cx);
            v.replace_all(cx);
            let f = v.find.as_mut().unwrap();
            f.in_replace = false;
            f.query.set("color");
            v.find_changed(cx);
            hits(v)
        })
        .unwrap();
    // The model still describes the old text: no results until the new one lands.
    assert_eq!(early, Vec::<String>::new(), "no results from the old model");
    assert!(cx.update(|app| h.read(app).unwrap().find.as_ref().unwrap().waiting));
    draw(&mut cx, h);
    let after = cx.update(|app| h.read(app).unwrap().doc.rope().to_string());
    assert_eq!(after.matches("color é").count(), paragraphs);
    // The search resumes from the cursor, after the first replacement.
    assert_eq!(status(&mut cx, h), format!("2 of {paragraphs}"));
    let (found, selected) = cx.update(|app| {
        let v = h.read(app).unwrap();
        let sel = v.editor.borrow().range();
        (hits(v), v.doc.rope().byte_slice(sel).to_string())
    });
    assert_eq!(found.len(), paragraphs);
    assert!(found.iter().all(|t| t == "color"));
    assert_eq!(
        selected, "color",
        "the first match is selected once it is found"
    );
}

fn type_then_replace_in_a_large_document() {
    // Over the synchronous parse limit: typing starts a background parse, and until it
    // lands the matches' ranges point into the text before the typing.
    let text = "colour é\n\n".repeat(200 * 1024 / 11);
    let paragraphs = text.matches("colour").count();
    let mut cx = cx();
    let h = open(&mut cx, &text);
    mode(&mut cx, h, ViewMode::Source);
    find(&mut cx, h, "colour", true);
    assert_eq!(status(&mut cx, h), format!("1 of {paragraphs}"));
    let typed = h
        .update(&mut cx, |v, _, cx| {
            v.find.as_mut().unwrap().in_replace = true;
            v.find_type("color", cx);
            v.editor.borrow_mut().selection = mdt_doc::Selection::cursor(0);
            v.insert("abc", mdt_doc::EditKind::Typing, cx);
            v.replace_one(cx);
            v.doc.rope().to_string()
        })
        .unwrap();
    assert_eq!(
        typed,
        format!("abc{text}"),
        "nothing replaced before the parse"
    );
    assert_eq!(status(&mut cx, h), "Searching…");
    draw(&mut cx, h);
    // Once the model lands the matches are found again, at their new offsets, and the
    // match that was current is again, without moving the caret.
    assert_eq!(status(&mut cx, h), format!("1 of {paragraphs}"));
    let caret = cx.update(|app| h.read(app).unwrap().editor.borrow().selection);
    assert_eq!(caret, mdt_doc::Selection::cursor(3));
    h.update(&mut cx, |v, _, cx| v.replace_one(cx)).unwrap();
    let after = cx.update(|app| h.read(app).unwrap().doc.rope().to_string());
    assert!(
        after.starts_with("abccolor é\n\ncolour é"),
        "{:?}",
        after.chars().take(24).collect::<String>()
    );
    assert_eq!(after.matches("colour").count(), paragraphs - 1);
}

fn replace_moves_to_the_next_match() {
    let mut cx = cx();
    let h = open(&mut cx, "a cat, a cat, a cat\n");
    mode(&mut cx, h, ViewMode::Source);
    find(&mut cx, h, "cat", true);
    h.update(&mut cx, |v, _, cx| {
        v.find.as_mut().unwrap().in_replace = true;
        v.find_type("dog", cx);
        v.replace_one(cx);
    })
    .unwrap();
    let after = cx.update(|app| h.read(app).unwrap().doc.rope().to_string());
    assert_eq!(after, "a dog, a cat, a cat\n");
    assert_eq!(status(&mut cx, h), "1 of 2", "the next cat is current");
}

fn reader_find_has_no_replace_field() {
    let mut cx = cx();
    let h = open(&mut cx, "A **fast** path.\n");
    find(&mut cx, h, "fast path", true);
    let (replace, status) = cx.update(|app| {
        let v = h.read(app).unwrap();
        let f = v.find.as_ref().unwrap();
        (f.replace.is_some(), f.status())
    });
    assert!(!replace, "no replace field in Reader");
    assert_eq!(status, "1 of 1", "rendered text matches across the markup");
    // Switching to Source searches the raw text instead.
    mode(&mut cx, h, ViewMode::Source);
    assert_eq!(status_of(&mut cx, h), "No results");
}

fn status_of(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) -> String {
    draw(cx, h);
    status(cx, h)
}

fn palette(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, query: &str) {
    h.update(cx, |v, window, cx| v.open_palette(window, cx))
        .unwrap();
    for c in query.chars() {
        h.update(cx, |v, _, cx| v.palette_type(&c.to_string(), cx))
            .unwrap();
    }
    draw(cx, h);
}

fn choose(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) {
    h.update(cx, |v, window, cx| v.palette_choose(window, cx))
        .unwrap();
    draw(cx, h);
}

fn method_doc() -> String {
    let mut s = String::new();
    for i in 0..40 {
        s.push_str(&format!("## Part {i}\n\nSome text for part {i}.\n\n"));
        if i == 30 {
            s.push_str("## Method\n\nHow it was done.\n\n");
        }
    }
    s
}

fn palette_escape_keeps_scroll_and_cursor() {
    let mut cx = cx();
    let h = open(&mut cx, &method_doc());
    mode(&mut cx, h, ViewMode::Source);
    let state = |cx: &mut HeadlessAppContext| {
        cx.update(|app| {
            let v = h.read(app).unwrap();
            let e = v.editor.borrow();
            (
                e.selection,
                e.anchor,
                v.reader.borrow().anchor,
                v.doc.rope().len_bytes(),
            )
        })
    };
    h.update(&mut cx, |v, _, cx| {
        v.editor.borrow_mut().selection = mdt_doc::Selection::cursor(40);
        v.editor.borrow_mut().scroll_by(300.0);
        cx.notify();
    })
    .unwrap();
    draw(&mut cx, h);
    let before = state(&mut cx);
    palette(&mut cx, h, "meth");
    h.update(&mut cx, |v, _, cx| {
        let p = v.palette.as_mut().unwrap();
        p.move_by(1);
        cx.notify();
    })
    .unwrap();
    h.update(&mut cx, |v, window, cx| v.close_palette(window, cx))
        .unwrap();
    draw(&mut cx, h);
    assert!(cx.update(|app| h.read(app).unwrap().palette.is_none()));
    assert_eq!(state(&mut cx), before);
}

fn heading_jump_in_reader_and_source() {
    let mut cx = cx();
    let h = open(&mut cx, &method_doc());
    palette(&mut cx, h, "#method");
    choose(&mut cx, h);
    let (anchor, unit) = cx.update(|app| {
        let v = h.read(app).unwrap();
        let r = v.reader.borrow();
        let unit = r
            .model
            .headings
            .iter()
            .find(|h| h.text == "Method")
            .unwrap()
            .unit;
        (r.anchor, unit)
    });
    assert_eq!(
        (anchor.unit, anchor.offset),
        (unit, 0.0),
        "Method at the top"
    );

    mode(&mut cx, h, ViewMode::Source);
    h.update(&mut cx, |v, _, cx| {
        v.editor.borrow_mut().selection = mdt_doc::Selection::cursor(0);
        v.editor.borrow_mut().anchor = Default::default();
        cx.notify();
    })
    .unwrap();
    palette(&mut cx, h, "#method");
    choose(&mut cx, h);
    let (sel, top_line, line) = cx.update(|app| {
        let v = h.read(app).unwrap();
        let e = v.editor.borrow();
        let rope = v.doc.rope();
        let at = rope.to_string().find("## Method").unwrap();
        (e.selection, e.anchor.unit, rope.byte_to_line(at))
    });
    assert_eq!(
        sel,
        mdt_doc::Selection::cursor(cx_text_find(&mut cx, h, "## Method"))
    );
    assert_eq!(top_line, line, "the heading line is at the top");
}

fn cx_text_find(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, s: &str) -> usize {
    cx.update(|app| h.read(app).unwrap().doc.rope().to_string().find(s).unwrap())
}

fn command_runs_and_recent_files_open_or_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.toml");
    let other = dir.path().join("other.md");
    let gone = dir.path().join("gone.md");
    std::fs::write(&other, "# Other\n").unwrap();
    mdt_app::recent::add(&state, &gone).unwrap();
    mdt_app::recent::add(&state, &other).unwrap();
    let mut cx = cx();
    cx.update(|app| app.set_global(mdt_app::recent::RecentFiles(state.clone())));
    let h = open(&mut cx, "# Doc\n");

    // A command: View: Split.
    palette(&mut cx, h, ">view split");
    choose(&mut cx, h);
    assert_eq!(
        cx.update(|app| h.read(app).unwrap().mode()),
        ViewMode::Split
    );

    // A recent file that exists opens in its own window.
    palette(&mut cx, h, "other.md");
    choose(&mut cx, h);
    assert_eq!(cx.update(|app| app.windows().len()), 2);

    // One that was deleted is removed, with a notice.
    palette(&mut cx, h, "gone.md");
    choose(&mut cx, h);
    let notices: Vec<String> = cx.update(|app| {
        h.read(app)
            .unwrap()
            .notices
            .items()
            .iter()
            .map(|n| n.message.to_string())
            .collect()
    });
    assert!(
        notices.iter().any(|n| n.contains("gone.md is missing")),
        "{notices:?}"
    );
    // The state file is written on the save thread.
    mdt_app::persist::flush();
    assert_eq!(mdt_app::recent::load(&state), vec![other]);
    assert_eq!(cx.update(|app| app.windows().len()), 2);
}

/// A frame with 100,000 matches in one paragraph draws only those in view: a rectangle
/// for each match, each found by a walk through the whole layout, took 7 s a frame with
/// 1,000,000 of them in release, and 0.26 s with 100,000 after the fix in a debug build.
fn matches_in_one_long_paragraph_draw_in_view() {
    let mut cx = cx();
    let h = open(&mut cx, &"a ".repeat(100_000));
    find(&mut cx, h, "a", false);
    for source in [false, true] {
        if source {
            mode(&mut cx, h, ViewMode::Source);
        }
        assert_eq!(status(&mut cx, h), "1 of 100000");
        let start = std::time::Instant::now();
        h.update(&mut cx, |_, _, cx| cx.notify()).unwrap();
        cx.capture_screenshot(h.into()).unwrap();
        let frame = start.elapsed();
        println!("  frame with 100,000 matches (source {source}): {frame:?}");
        assert!(frame < std::time::Duration::from_secs(2), "{frame:?}");
    }
}

fn main() {
    let tests: [(&str, fn()); 11] = [
        (
            "matches_in_one_long_paragraph_draw_in_view",
            matches_in_one_long_paragraph_draw_in_view,
        ),
        (
            "type_then_replace_in_a_large_document",
            type_then_replace_in_a_large_document,
        ),
        (
            "replace_all_in_a_large_document_then_find",
            replace_all_in_a_large_document_then_find,
        ),
        (
            "palette_escape_keeps_scroll_and_cursor",
            palette_escape_keeps_scroll_and_cursor,
        ),
        (
            "heading_jump_in_reader_and_source",
            heading_jump_in_reader_and_source,
        ),
        (
            "command_runs_and_recent_files_open_or_are_pruned",
            command_runs_and_recent_files_open_or_are_pruned,
        ),
        (
            "incremental_find_scrolls_to_the_nearest_match_and_wraps",
            incremental_find_scrolls_to_the_nearest_match_and_wraps,
        ),
        (
            "escape_in_source_keeps_the_match_selected",
            escape_in_source_keeps_the_match_selected,
        ),
        ("replace_all_is_one_undo_step", replace_all_is_one_undo_step),
        (
            "replace_moves_to_the_next_match",
            replace_moves_to_the_next_match,
        ),
        (
            "reader_find_has_no_replace_field",
            reader_find_has_no_replace_field,
        ),
    ];
    for (name, test) in tests {
        test();
        println!("test {name} ... ok");
    }
}
