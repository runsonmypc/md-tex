//! Link navigation (group 16). `harness = false`: GPUI's macOS text system must be
//! created on the main thread.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AppContext, Bounds, HeadlessAppContext, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, PlatformInput, WindowHandle, point, px, size,
};
use mdt_app::reader::block::Link;
use mdt_app::settings::ViewMode;
use mdt_app::view::{DocumentView, Opener};

fn cx() -> HeadlessAppContext {
    HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    )
}

fn open_doc(cx: &mut HeadlessAppContext, doc: mdt_doc::Document) -> WindowHandle<DocumentView> {
    let h = cx
        .open_window(size(px(1000.0), px(800.0)), |window, cx| {
            cx.new(|cx| DocumentView::new(doc, window, cx))
        })
        .unwrap();
    draw(cx, h);
    h
}

fn open(cx: &mut HeadlessAppContext, text: &str) -> WindowHandle<DocumentView> {
    open_doc(
        cx,
        mdt_doc::Document::from_bytes(text.as_bytes(), None).unwrap(),
    )
}

fn draw(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) {
    cx.run_until_parked();
    cx.capture_screenshot(h.into()).unwrap();
}

/// Records what would have gone to the system handler and the file manager.
#[derive(Default)]
struct Calls {
    urls: Vec<String>,
    revealed: Vec<PathBuf>,
}

fn fake_opener(cx: &mut HeadlessAppContext) -> Rc<RefCell<Calls>> {
    let calls = Rc::new(RefCell::new(Calls::default()));
    let (a, b) = (calls.clone(), calls.clone());
    cx.update(|app| {
        app.set_global(Opener {
            open_url: Rc::new(move |url, _| a.borrow_mut().urls.push(url.to_owned())),
            reveal: Rc::new(move |p: &Path, _| b.borrow_mut().revealed.push(p.to_owned())),
        })
    });
    calls
}

fn center(b: Bounds<Pixels>) -> gpui::Point<Pixels> {
    point(
        b.origin.x + b.size.width / 2.0,
        b.origin.y + b.size.height / 2.0,
    )
}

fn mouse_down(
    cx: &mut HeadlessAppContext,
    h: WindowHandle<DocumentView>,
    at: gpui::Point<Pixels>,
    modifiers: Modifiers,
) {
    cx.update_window(h.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
    })
    .unwrap();
    draw(cx, h);
}

/// Clicks a link in the reader where it was last drawn.
fn click(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, link: Link) {
    let b = cx
        .update(|app| h.read(app).unwrap().reader.borrow().link_bounds(&link))
        .unwrap_or_else(|| panic!("{link:?} not on screen"));
    mouse_down(cx, h, center(b), Modifiers::default());
}

fn top_unit(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) -> usize {
    cx.update(|app| h.read(app).unwrap().reader.borrow().anchor.unit)
}

/// Whether the top of `unit` is on screen in the reader.
fn visible(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, unit: usize) -> bool {
    cx.update(|app| {
        let v = h.read(app).unwrap();
        let r = v.reader.borrow();
        let top = r.heights.top(unit);
        let y = r.anchor.scroll_y(&r.heights);
        top >= y && top < y + r.viewport_h() as f64
    })
}

fn notices(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>) -> Vec<String> {
    cx.update(|app| {
        h.read(app)
            .unwrap()
            .notices
            .items()
            .iter()
            .map(|n| n.message.to_string())
            .collect()
    })
}

fn unit_of(cx: &mut HeadlessAppContext, h: WindowHandle<DocumentView>, heading: &str) -> usize {
    cx.update(|app| {
        let v = h.read(app).unwrap();
        let r = v.reader.borrow();
        r.model
            .headings
            .iter()
            .find(|x| x.text == heading)
            .unwrap()
            .unit
    })
}

fn anchors_back_and_missing_anchor() {
    let mut text =
        String::from("Jump to [the start](#getting-started) or [nowhere](#nowhere).\n\n");
    for i in 0..40 {
        text.push_str(&format!("Filler paragraph {i}.\n\n"));
    }
    text.push_str("## Getting Started\n\nHere.\n\n");
    for i in 0..40 {
        text.push_str(&format!("More text {i}.\n\n"));
    }
    let mut cx = cx();
    let h = open(&mut cx, &text);
    // Hovering a link shows its target.
    let b = cx
        .update(|app| {
            h.read(app)
                .unwrap()
                .reader
                .borrow()
                .link_bounds(&Link::Url("#getting-started".into()))
        })
        .unwrap();
    cx.update_window(h.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position: center(b),
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
    })
    .unwrap();
    draw(&mut cx, h);
    let hover = cx.update(|app| {
        h.read(app)
            .unwrap()
            .reader
            .borrow()
            .hover_message()
            .map(str::to_owned)
    });
    assert_eq!(hover.as_deref(), Some("#getting-started"));

    // The missing anchor leaves the view and says so.
    click(&mut cx, h, Link::Url("#nowhere".into()));
    assert_eq!(top_unit(&mut cx, h), 0);
    assert!(
        notices(&mut cx, h).iter().any(|n| n.contains("#nowhere")),
        "{:?}",
        notices(&mut cx, h)
    );

    // The heading anchor scrolls the heading to the top; back returns.
    click(&mut cx, h, Link::Url("#getting-started".into()));
    let target = unit_of(&mut cx, h, "Getting Started");
    let anchor = cx.update(|app| h.read(app).unwrap().reader.borrow().anchor);
    assert_eq!(
        (anchor.unit, anchor.offset),
        (target, 0.0),
        "heading at the top"
    );
    h.update(&mut cx, |v, _, cx| v.go_back(cx)).unwrap();
    draw(&mut cx, h);
    assert_eq!(top_unit(&mut cx, h), 0, "back where it was");
    h.update(&mut cx, |v, _, cx| v.go_forward(cx)).unwrap();
    draw(&mut cx, h);
    assert_ne!(top_unit(&mut cx, h), 0, "forward again");
}

fn footnote_and_back_reference() {
    let mut text = String::from("A claim.[^src]\n\n");
    for i in 0..40 {
        text.push_str(&format!("Filler paragraph {i}.\n\n"));
    }
    text.push_str("[^src]: The source.\n");
    let mut cx = cx();
    let h = open(&mut cx, &text);
    click(&mut cx, h, Link::Footnote("src".into()));
    let notes = cx.update(|app| h.read(app).unwrap().reader.borrow().model.units.len() - 1);
    assert!(visible(&mut cx, h, notes), "at the footnotes");
    click(&mut cx, h, Link::FootnoteBack("src".into()));
    assert_eq!(top_unit(&mut cx, h), 0, "back at the reference");
}

fn plain_click_in_source_places_the_cursor_and_cmd_click_follows() {
    let text = "Go [nowhere](#nowhere) now.\n";
    let mut cx = cx();
    let h = open(&mut cx, text);
    h.update(&mut cx, |v, _, cx| v.set_mode(ViewMode::Source, 1000.0, cx))
        .unwrap();
    draw(&mut cx, h);
    let at = text.find("nowhere").unwrap() + 2;
    let pos = cx.update(|app| {
        let fonts = mdt_app::reader::view::fonts(app);
        let v = h.read(app).unwrap();
        let mut e = v.editor.borrow_mut();
        let (x, y, line_h) = e.caret(v.doc.rope(), &mut fonts.borrow_mut(), app, at);
        point(
            px(e.origin.0 + x - e.hscroll),
            px(e.origin.1 + (y - e.scroll_y()) as f32 + line_h / 2.0),
        )
    });
    mouse_down(&mut cx, h, pos, Modifiers::default());
    let (sel, n) = cx.update(|app| {
        let v = h.read(app).unwrap();
        (v.editor.borrow().selection, v.notices.items().len())
    });
    assert_eq!(sel, mdt_doc::Selection::cursor(at), "cursor placed");
    assert_eq!(n, 0, "not followed");
    let follow = if cfg!(target_os = "macos") {
        Modifiers::command()
    } else {
        Modifiers::control()
    };
    mouse_down(&mut cx, h, pos, follow);
    assert!(
        notices(&mut cx, h).iter().any(|n| n.contains("#nowhere")),
        "followed"
    );
}

fn local_files_open_markdown_and_reveal_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes");
    let docs = dir.path().join("docs");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::create_dir_all(&docs).unwrap();
    let mut setup = String::new();
    for i in 0..40 {
        setup.push_str(&format!("Para {i}.\n\n"));
    }
    setup.push_str("## Install\n\nSteps.\n\n");
    for i in 0..40 {
        setup.push_str(&format!("More {i}.\n\n"));
    }
    std::fs::write(docs.join("setup.md"), setup).unwrap();
    std::fs::write(notes.join("run.sh"), "#!/bin/sh\ntouch ran\n").unwrap();
    let page = notes.join("index.md");
    std::fs::write(
        &page,
        "[setup](../docs/setup.md#install) [run](./run.sh) [gone](./gone.md) [pipe](./pipe) \
         [null](/dev/null) [docs](../docs)\n",
    )
    .unwrap();
    let mut cx = cx();
    let calls = fake_opener(&mut cx);
    let h = open_doc(&mut cx, mdt_doc::Document::open(&page).unwrap());

    click(&mut cx, h, Link::Url("../docs/setup.md#install".into()));
    let windows = cx.update(|app| app.windows().len());
    assert_eq!(windows, 2, "setup.md opened in its own window");
    let w = cx
        .update(|app| mdt_app::launch::window_for(&docs.join("setup.md"), app))
        .unwrap();
    draw(&mut cx, w);
    let install = unit_of(&mut cx, w, "Install");
    let top = top_unit(&mut cx, w);
    assert!(
        top > 30 && top <= install,
        "scrolled to Install: {top} {install}"
    );

    // Following it again focuses the same window.
    click(&mut cx, h, Link::Url("../docs/setup.md#install".into()));
    assert_eq!(cx.update(|app| app.windows().len()), 2);

    click(&mut cx, h, Link::Url("./run.sh".into()));
    assert_eq!(calls.borrow().revealed, vec![notes.join("./run.sh")]);
    assert!(calls.borrow().urls.is_empty(), "not opened");
    assert!(!notes.join("ran").exists(), "not run");

    click(&mut cx, h, Link::Url("./gone.md".into()));
    assert!(
        notices(&mut cx, h)
            .iter()
            .any(|n| n.contains("gone.md does not exist"))
    );

    // GPUI's Linux reveal opens the path, which blocks on a FIFO and runs a device's
    // driver: neither is revealed.
    let fifo = notes.join("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    for link in ["./pipe", "/dev/null"] {
        click(&mut cx, h, Link::Url(link.into()));
        let name = link.trim_start_matches("./");
        assert!(
            notices(&mut cx, h)
                .iter()
                .any(|n| n.contains(name) && n.contains("is not a regular file")),
            "{link}"
        );
    }
    assert_eq!(calls.borrow().revealed, vec![notes.join("./run.sh")]);
    // A folder is still revealed.
    click(&mut cx, h, Link::Url("../docs".into()));
    assert_eq!(calls.borrow().revealed.last(), Some(&notes.join("../docs")));
}

/// Filler paragraphs, a heading, then more filler.
fn with_heading(before: usize, heading: &str, after: usize) -> String {
    let mut text = String::new();
    for i in 0..before {
        text.push_str(&format!("Para {i}.\n\n"));
    }
    text.push_str(&format!("## {heading}\n\nSteps.\n\n"));
    for i in 0..after {
        text.push_str(&format!("More {i}.\n\n"));
    }
    text
}

fn own_file_stays_in_its_window() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("index.md");
    let text = format!(
        "[install](./index.md#install) [me](./index.md) [cmd](FILE://{}/run.command)\n\n{}",
        dir.path().display(),
        with_heading(40, "Install", 40)
    );
    std::fs::write(&page, text).unwrap();
    std::fs::write(dir.path().join("run.command"), "#!/bin/sh\ntouch ran\n").unwrap();
    let state = dir.path().join("state.toml");
    mdt_app::recent::add(&state, &page).unwrap();
    let mut cx = cx();
    let calls = fake_opener(&mut cx);
    cx.update(|app| app.set_global(mdt_app::recent::RecentFiles(state.clone())));
    let h = open_doc(&mut cx, mdt_doc::Document::open(&page).unwrap());
    let windows = |cx: &mut HeadlessAppContext| cx.update(|app| app.windows().len());

    // A link to a heading in its own file scrolls there.
    click(&mut cx, h, Link::Url("./index.md#install".into()));
    assert_eq!(windows(&mut cx), 1, "no second window");
    let install = unit_of(&mut cx, h, "Install");
    assert!(visible(&mut cx, h, install) && top_unit(&mut cx, h) > 30);
    h.update(&mut cx, |v, _, cx| v.go_back(cx)).unwrap();
    draw(&mut cx, h);

    // Its own file without a fragment, from a link, the palette's recent files or a drop.
    click(&mut cx, h, Link::Url("./index.md".into()));
    h.update(&mut cx, |v, window, cx| {
        v.open_palette(window, cx);
        v.palette_type("index.md", cx);
        v.palette_choose(window, cx);
    })
    .unwrap();
    h.update(&mut cx, |v, window, cx| {
        v.open_here(std::slice::from_ref(&page), None, window, cx)
    })
    .unwrap();
    draw(&mut cx, h);
    assert_eq!(windows(&mut cx), 1, "no second window");
    assert_eq!(top_unit(&mut cx, h), 0, "stays where it was");
    assert!(notices(&mut cx, h).is_empty(), "{:?}", notices(&mut cx, h));

    // A `file:` URL in any case is revealed, never opened (it could run a program).
    let cmd = format!("FILE://{}/run.command", dir.path().display());
    click(&mut cx, h, Link::Url(cmd));
    assert_eq!(
        calls.borrow().revealed,
        vec![dir.path().join("run.command")]
    );
    assert!(calls.borrow().urls.is_empty(), "not opened");
}

fn anchor_in_a_large_document_waits_for_the_full_parse() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.md");
    // The heading is far past the prefix parsed for the first frame.
    let text = with_heading(30_000, "Late Heading", 20);
    assert!(text.len() > 256 * 1024);
    std::fs::write(&big, text).unwrap();
    let page = dir.path().join("index.md");
    std::fs::write(&page, "[late](big.md#late-heading)\n").unwrap();
    let mut cx = cx();
    let h = open_doc(&mut cx, mdt_doc::Document::open(&page).unwrap());
    click(&mut cx, h, Link::Url("big.md#late-heading".into()));
    let w = cx
        .update(|app| mdt_app::launch::window_for(&big, app))
        .unwrap();
    draw(&mut cx, w);
    assert!(cx.update(|app| w.read(app).unwrap().parsed()));
    assert!(notices(&mut cx, w).is_empty(), "{:?}", notices(&mut cx, w));
    let late = unit_of(&mut cx, w, "Late Heading");
    assert!(visible(&mut cx, w, late), "scrolled to the heading");
}

fn web_links_open_and_other_schemes_ask_first() {
    let mut cx = cx();
    let calls = fake_opener(&mut cx);
    let h = open(
        &mut cx,
        "[web](https://example.com) and [code](vscode://file/a)\n",
    );
    click(&mut cx, h, Link::Url("https://example.com".into()));
    assert_eq!(calls.borrow().urls, vec!["https://example.com"]);

    h.update(&mut cx, |v, _, _| v.test_link_answer = Some(1))
        .unwrap();
    click(&mut cx, h, Link::Url("vscode://file/a".into()));
    assert_eq!(calls.borrow().urls.len(), 1, "cancelled: nothing opens");
    h.update(&mut cx, |v, _, _| v.test_link_answer = Some(0))
        .unwrap();
    click(&mut cx, h, Link::Url("vscode://file/a".into()));
    assert_eq!(
        calls.borrow().urls,
        vec!["https://example.com", "vscode://file/a"],
        "confirmed"
    );
}

fn main() {
    let tests: [(&str, fn()); 7] = [
        ("own_file_stays_in_its_window", own_file_stays_in_its_window),
        (
            "anchor_in_a_large_document_waits_for_the_full_parse",
            anchor_in_a_large_document_waits_for_the_full_parse,
        ),
        (
            "anchors_back_and_missing_anchor",
            anchors_back_and_missing_anchor,
        ),
        ("footnote_and_back_reference", footnote_and_back_reference),
        (
            "plain_click_in_source_places_the_cursor_and_cmd_click_follows",
            plain_click_in_source_places_the_cursor_and_cmd_click_follows,
        ),
        (
            "local_files_open_markdown_and_reveal_the_rest",
            local_files_open_markdown_and_reveal_the_rest,
        ),
        (
            "web_links_open_and_other_schemes_ask_first",
            web_links_open_and_other_schemes_ask_first,
        ),
    ];
    for (name, test) in tests {
        test();
        println!("test {name} ... ok");
    }
}
