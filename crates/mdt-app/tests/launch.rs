//! Launch scenarios. Runs on the main thread (`harness = false`): GPUI's macOS
//! text system must be created there.

use std::path::PathBuf;

use gpui::HeadlessAppContext;
use mdt_app::launch::open_all;
use mdt_app::view::DocumentView;

fn cx() -> HeadlessAppContext {
    HeadlessAppContext::new(gpui_platform::current_platform(true).text_system())
}

/// (title, document text, bound path, dirty) for every open window.
fn windows(cx: &mut HeadlessAppContext) -> Vec<(String, String, Option<PathBuf>, bool)> {
    let handles = cx.update(|app| app.windows());
    handles
        .into_iter()
        .map(|h| {
            let h = h.downcast::<DocumentView>().expect("document window");
            cx.update(|app| {
                let view = h.read(app).unwrap();
                let doc = &view.doc;
                (
                    DocumentView::title_for(doc),
                    doc.rope().to_string(),
                    doc.path().map(PathBuf::from),
                    doc.is_dirty(),
                )
            })
        })
        .collect()
}

fn no_path_opens_one_untitled_window() {
    let mut cx = cx();
    let errors = cx.update(|app| open_all(&[], app));
    assert!(errors.is_empty());
    assert_eq!(
        windows(&mut cx),
        [("Untitled".to_owned(), String::new(), None, false)]
    );
}

fn existing_paths_open_one_window_each() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.md");
    std::fs::write(&a, "# A\n").unwrap();
    std::fs::write(&b, "# B\n").unwrap();
    let mut cx = cx();
    let errors = cx.update(|app| open_all(&[a.clone(), b.clone()], app));
    assert!(errors.is_empty());
    let mut w = windows(&mut cx);
    w.sort();
    assert_eq!(
        w,
        [
            ("a.md".to_owned(), "# A\n".to_owned(), Some(a), false),
            ("b.md".to_owned(), "# B\n".to_owned(), Some(b), false)
        ]
    );
}

fn missing_path_opens_empty_document_bound_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.md");
    let mut cx = cx();
    let errors = cx.update(|app| open_all(std::slice::from_ref(&path), app));
    assert!(errors.is_empty());
    assert_eq!(
        windows(&mut cx),
        [(
            "new.md".to_owned(),
            String::new(),
            Some(path.clone()),
            false
        )]
    );
    assert!(!path.exists(), "opening must not create the file");
}

fn unopenable_path_reports_error_and_others_still_open() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("x.bin");
    let ok = dir.path().join("ok.md");
    std::fs::write(&bin, b"\0\0binary").unwrap();
    std::fs::write(&ok, "ok").unwrap();
    let mut cx = cx();
    let errors = cx.update(|app| open_all(&[bin, ok], app));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("binary"), "{errors:?}");
    assert_eq!(windows(&mut cx).len(), 1);
}

fn settings_reload_live_into_every_window() {
    use mdt_app::app_settings;
    use mdt_app::theme::ThemeMode;
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded test binary; nothing else reads the environment concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
    }
    let path = mdt_app::settings::settings_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "theme = \"light\"\n").unwrap();

    let mut cx = cx();
    cx.update(app_settings::init);
    cx.update(|app| open_all(&[], app));
    cx.update(|app| open_all(&[], app));
    assert_eq!(
        cx.update(|app| app_settings::current(app).theme),
        ThemeMode::Light
    );

    std::fs::write(&path, "theme = \"dark\"\nzoom = 900\n").unwrap();
    // The watcher wakes the app from its own thread; nothing polls.
    wait_until(&mut cx, |app| {
        app_settings::current(app).theme == ThemeMode::Dark
    });
    let notices = all_notices(&mut cx);
    assert_eq!(notices.len(), 2);
    for n in &notices {
        assert_eq!(
            n.iter().filter(|m| m.contains("`zoom`")).count(),
            1,
            "{n:?}"
        );
    }

    // Changing another setting does not repeat the notice about `zoom`, in the windows
    // that dismissed it or anywhere else.
    let first = cx.update(|app| app.windows()[0].downcast::<DocumentView>().unwrap());
    let _ = first.update(&mut cx, |v, _, cx| {
        v.notices.dismiss_latest();
        cx.notify();
    });
    for _ in 0..2 {
        let window = cx.update(|app| app.windows()[1]);
        let _ = window.update(&mut cx, |_, window, cx| {
            window.dispatch_action(Box::new(mdt_app::keymap::ToggleWideReader), cx)
        });
        mdt_app::persist::flush();
        settle(&mut cx);
    }
    let notices = all_notices(&mut cx);
    assert!(notices[0].is_empty(), "{notices:?}");
    assert_eq!(notices[1].len(), 1, "{notices:?}");
    // A new problem is shown.
    std::fs::write(&path, "theme = \"dark\"\nzoom = 900\ncolour = 1\n").unwrap();
    wait_until(&mut cx, |app| {
        app.global::<app_settings::AppSettings>().problems.len() == 2
    });
    let notices = all_notices(&mut cx);
    assert!(
        notices[0].iter().any(|m| m.contains("`colour`")),
        "{notices:?}"
    );
}

/// Every window's notices, in window order.
fn all_notices(cx: &mut HeadlessAppContext) -> Vec<Vec<String>> {
    cx.update(|app| {
        app.windows()
            .into_iter()
            .map(|w| {
                let v = w.downcast::<DocumentView>().unwrap();
                v.read(app)
                    .unwrap()
                    .notices
                    .items()
                    .iter()
                    .map(|n| n.message.to_string())
                    .collect()
            })
            .collect()
    })
}

/// Lets file watcher threads and the app run for a moment.
fn settle(cx: &mut HeadlessAppContext) {
    std::thread::sleep(std::time::Duration::from_millis(300));
    cx.advance_clock(std::time::Duration::from_millis(100));
    cx.run_until_parked();
}

/// Waits up to 3 s for `done`.
fn wait_until(cx: &mut HeadlessAppContext, done: impl Fn(&gpui::App) -> bool) {
    let start = std::time::Instant::now();
    while !cx.update(|app| done(app)) {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        cx.advance_clock(std::time::Duration::from_millis(20));
        cx.run_until_parked();
    }
}

fn task_toggle_is_an_undoable_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.md");
    std::fs::write(&path, "- [ ] a\n- [x] b\n").unwrap();
    let mut cx = cx();
    cx.update(|app| open_all(std::slice::from_ref(&path), app));
    let h = cx.update(|app| app.windows()[0].downcast::<DocumentView>().unwrap());
    let state = |cx: &mut HeadlessAppContext| {
        cx.update(|app| {
            let v = h.read(app).unwrap();
            let checked: Vec<bool> = v
                .reader
                .borrow()
                .model
                .units
                .iter()
                .flat_map(|u| match &u.block.kind {
                    mdt_markdown::BlockKind::List { items, .. } => items
                        .iter()
                        .map(|i| i.task.as_ref().unwrap().checked)
                        .collect(),
                    _ => vec![],
                })
                .collect();
            (v.doc.rope().to_string(), v.doc.is_dirty(), checked)
        })
    };
    assert_eq!(
        state(&mut cx),
        ("- [ ] a\n- [x] b\n".into(), false, vec![false, true])
    );
    let _ = h.update(&mut cx, |v, _, cx| v.toggle_task(2..5, cx));
    assert_eq!(
        state(&mut cx),
        ("- [x] a\n- [x] b\n".into(), true, vec![true, true])
    );
    let _ = h.update(&mut cx, |v, _, cx| v.toggle_task(10..13, cx));
    assert_eq!(
        state(&mut cx),
        ("- [x] a\n- [ ] b\n".into(), true, vec![true, false])
    );
    let _ = h.update(&mut cx, |v, _, cx| v.undo(cx));
    assert_eq!(
        state(&mut cx),
        ("- [x] a\n- [x] b\n".into(), true, vec![true, true])
    );
    let _ = h.update(&mut cx, |v, _, cx| v.undo(cx));
    assert_eq!(
        state(&mut cx),
        ("- [ ] a\n- [x] b\n".into(), false, vec![false, true])
    );
    let _ = h.update(&mut cx, |v, _, cx| v.redo(cx));
    assert_eq!(state(&mut cx).0, "- [x] a\n- [x] b\n");
}

/// A task toggled in the reader lays out the source pane's line again, though its length
/// is unchanged (here a line over 4 KiB).
fn task_toggle_refreshes_the_source_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.md");
    std::fs::write(&path, format!("- [ ] {}\n", "word ".repeat(1000))).unwrap();
    let mut cx = cx();
    cx.update(|app| open_all(std::slice::from_ref(&path), app));
    let h = cx.update(|app| app.windows()[0].downcast::<DocumentView>().unwrap());
    let line = |cx: &mut HeadlessAppContext| {
        cx.update(|app| {
            let fonts = mdt_app::reader::view::fonts(app);
            let v = h.read(app).unwrap();
            let rope = v.doc.rope().clone();
            v.editor
                .borrow_mut()
                .line(0, &rope, &mut fonts.borrow_mut(), app)
        })
    };
    let before = line(&mut cx);
    let _ = h.update(&mut cx, |v, _, cx| v.toggle_task(2..5, cx));
    assert!(cx.update(|app| {
        h.read(app)
            .unwrap()
            .doc
            .rope()
            .to_string()
            .starts_with("- [x] ")
    }));
    assert!(
        !std::rc::Rc::ptr_eq(&before, &line(&mut cx)),
        "the source pane kept the unchecked line"
    );
}

fn zoom_in_twice_is_125_and_persists_across_relaunch() {
    use mdt_app::app_settings;
    use mdt_app::keymap::ZoomIn;
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded test binary; nothing else reads the environment concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
    }
    let zooms = |cx: &mut HeadlessAppContext| -> Vec<f32> {
        let handles = cx.update(|app| app.windows());
        handles
            .into_iter()
            .map(|w| {
                let v = w.downcast::<DocumentView>().unwrap();
                cx.update(|app| v.read(app).unwrap().reader.borrow().zoom)
            })
            .collect()
    };
    let mut cx = self::cx();
    cx.update(app_settings::init);
    cx.update(|app| open_all(&[], app));
    let window = cx.update(|app| app.windows()[0]);
    for _ in 0..2 {
        let _ = window.update(&mut cx, |_, window, cx| {
            window.dispatch_action(Box::new(ZoomIn), cx)
        });
        cx.run_until_parked();
    }
    let _ = window.update(&mut cx, |_, window, _| window.refresh());
    let _ = cx.capture_screenshot(window);
    assert_eq!(cx.update(|app| app_settings::current(app).zoom), 125);
    assert_eq!(zooms(&mut cx), vec![1.25]);
    mdt_app::persist::flush();
    let path = mdt_app::settings::settings_path().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "zoom = 125\n");

    // A new launch reads the saved level.
    let mut cx2 = self::cx();
    cx2.update(app_settings::init);
    cx2.update(|app| open_all(&[], app));
    assert_eq!(zooms(&mut cx2), vec![1.25]);
}

fn external_changes_reload_ask_or_report_deletion() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.md");
    std::fs::write(&path, "one\n").unwrap();
    let mut cx = self::cx();
    cx.update(|app| open_all(std::slice::from_ref(&path), app));
    let window = cx
        .update(|app| app.windows()[0])
        .downcast::<DocumentView>()
        .unwrap();
    let _ = cx.capture_screenshot(window.into());
    let settle = |cx: &mut HeadlessAppContext| {
        std::thread::sleep(Duration::from_millis(400));
        cx.advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    };
    let state = |cx: &mut HeadlessAppContext| {
        cx.update(|app| {
            let v = window.read(app).unwrap();
            let notices: Vec<String> = v
                .notices
                .items()
                .iter()
                .map(|n| n.message.to_string())
                .collect();
            (v.doc.rope().to_string(), notices)
        })
    };
    settle(&mut cx);
    // Waits for `want` for up to 3 s and reports how long it took. In the real app a
    // reload lands 130 to 220 ms after the write (measured 2026-10-20); the headless test
    // platform delivers the watcher's wake-up about a second later.
    type Seen = (String, Vec<String>);
    let wait_for = |cx: &mut HeadlessAppContext, want: &dyn Fn(&Seen) -> bool| {
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
            cx.advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            if want(&state(cx)) {
                return start.elapsed();
            }
        }
        panic!("timed out; state {:?}", state(cx));
    };

    // No local edits: reload.
    std::fs::write(&path, "two\n").unwrap();
    let took = wait_for(&mut cx, &|s| s.0 == "two\n");
    println!("  reload seen after {took:?}");
    assert_eq!(state(&mut cx), ("two\n".into(), vec![]));

    // Our own save: nothing happens.
    let _ = window.update(&mut cx, |v, window, cx| v.save(window, cx));
    settle(&mut cx);
    assert_eq!(state(&mut cx), ("two\n".into(), vec![]));

    // Local edits: a notice, and the edits stay until the user picks.
    let _ = window.update(&mut cx, |v, _, cx| v.paste_text("mine ", cx));
    std::fs::write(&path, "three\n").unwrap();
    wait_for(&mut cx, &|s| !s.1.is_empty());
    let (text, notices) = state(&mut cx);
    assert_eq!(text, "mine two\n");
    assert_eq!(notices, vec!["doc.md changed on disk.".to_owned()]);
    let id = cx.update(|app| window.read(app).unwrap().notices.items()[0].id);
    let _ = window.update(&mut cx, |v, _, cx| {
        v.notice_action(id, mdt_app::view::CONFLICT_RELOAD, cx)
    });
    assert_eq!(state(&mut cx), ("three\n".into(), vec![]));

    // Deleted: the text stays, with a notice; saving recreates the file.
    std::fs::remove_file(&path).unwrap();
    wait_for(&mut cx, &|s| !s.1.is_empty());
    let (text, notices) = state(&mut cx);
    assert_eq!(text, "three\n");
    assert_eq!(
        notices,
        vec!["doc.md was deleted on disk. Saving recreates it.".to_owned()]
    );
    let _ = window.update(&mut cx, |v, window, cx| v.save(window, cx));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\n");
}

fn file_already_open_focuses_its_window() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    std::fs::write(&a, "# A\n").unwrap();
    let mut cx = self::cx();
    cx.update(|app| open_all(std::slice::from_ref(&a), app));
    // Again, by another spelling of the same path, and twice in one request.
    let dotted = dir.path().join(".").join("a.md");
    cx.update(|app| open_all(&[dotted.clone(), dotted], app));
    assert_eq!(windows(&mut cx).len(), 1);
    // A forwarded launch without paths opens a new untitled window.
    cx.update(|app| open_all(&[], app));
    assert_eq!(windows(&mut cx).len(), 2);
}

fn spellings_of_one_file_in_one_request_open_one_window() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    std::fs::write(&a, "# A\n").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let up = dir.path().join("sub").join("..").join("a.md");
    let link = dir.path().join("link.md");
    std::os::unix::fs::symlink(&a, &link).unwrap();
    let mut cx = self::cx();
    let errors = cx.update(|app| open_all(&[a, up, link], app));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(windows(&mut cx).len(), 1);
}

fn opened_files_are_recent_after_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.toml");
    let a = dir.path().join("a.md");
    std::fs::write(&a, "# A\n").unwrap();
    {
        let mut cx = self::cx();
        cx.update(|app| app.set_global(mdt_app::recent::RecentFiles(state.clone())));
        cx.update(|app| open_all(std::slice::from_ref(&a), app));
    }
    mdt_app::persist::flush();
    // A new app with the same state file: the palette lists the file.
    let mut cx = self::cx();
    cx.update(|app| app.set_global(mdt_app::recent::RecentFiles(state.clone())));
    let recent = cx.update(|app| mdt_app::recent::list(app));
    assert_eq!(recent, vec![a.clone()]);
    let p = mdt_app::palette::Palette::new(&[], recent, "a.md");
    let first = p.chosen().map(|e| e.label());
    assert_eq!(first, Some(a.display().to_string()));
}

fn wide_reader_shortcut_widens_the_column_and_persists() {
    use mdt_app::app_settings;
    use mdt_app::keymap::ToggleWideReader;
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded test binary; nothing else reads the environment concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
    }
    let mut cx = self::cx();
    cx.update(app_settings::init);
    cx.update(|app| open_all(&[], app));
    let window = cx.update(|app| app.windows()[0]);
    let view = window.downcast::<DocumentView>().unwrap();
    // The column's left edge, from a painted frame.
    let column_x = |cx: &mut HeadlessAppContext| {
        let _ = window.update(cx, |_, window, _| window.refresh());
        let _ = cx.capture_screenshot(window);
        cx.update(|app| view.read(app).unwrap().reader.borrow().origin.0)
    };
    let narrow = column_x(&mut cx);
    let _ = window.update(&mut cx, |_, window, cx| {
        window.dispatch_action(Box::new(ToggleWideReader), cx)
    });
    cx.run_until_parked();
    let wide = column_x(&mut cx);
    assert!(cx.update(|app| app_settings::current(app).wide_reader));
    assert!(wide < narrow, "column moved from x {narrow} to {wide}");
    mdt_app::persist::flush();
    let path = mdt_app::settings::settings_path().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "wide_reader = true\n"
    );
}

fn failed_watch_is_not_retried_every_frame() {
    let dir = tempfile::tempdir().unwrap();
    // Its folder does not exist, so the file cannot be watched yet.
    let path = dir.path().join("later").join("new.md");
    let mut cx = self::cx();
    cx.update(|app| open_all(std::slice::from_ref(&path), app));
    let window = cx.update(|app| app.windows()[0]);
    let view = window.downcast::<DocumentView>().unwrap();
    for _ in 0..5 {
        let _ = window.update(&mut cx, |_, window, _| window.refresh());
        let _ = cx.capture_screenshot(window);
    }
    assert_eq!(cx.update(|app| view.read(app).unwrap().watch_attempts()), 1);
    // A save (after the folder appears) tries again.
    std::fs::create_dir(path.parent().unwrap()).unwrap();
    let _ = view.update(&mut cx, |v, window, cx| {
        v.paste_text("x", cx);
        v.save(window, cx)
    });
    let _ = window.update(&mut cx, |_, window, _| window.refresh());
    let _ = cx.capture_screenshot(window);
    assert_eq!(cx.update(|app| view.read(app).unwrap().watch_attempts()), 2);
}

fn open_failures_show_in_a_window() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("x.bin");
    std::fs::write(&bin, b"\0\0binary").unwrap();
    let mut cx = self::cx();
    // No window open (a Finder launch whose only file fails): an untitled one says why.
    let errors = cx.update(|app| open_all(std::slice::from_ref(&bin), app));
    cx.update(|app| mdt_app::launch::report(errors, app));
    let notices = all_notices(&mut cx);
    assert_eq!(notices.len(), 1);
    assert!(notices[0][0].contains("binary"), "{notices:?}");
    assert_eq!(windows(&mut cx)[0].0, "Untitled");
    // With a window open, the notice goes there.
    let errors = cx.update(|app| open_all(std::slice::from_ref(&dir.path().to_path_buf()), app));
    cx.update(|app| mdt_app::launch::report(errors, app));
    let notices = all_notices(&mut cx);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].len(), 2, "{notices:?}");
}

/// Dock > Quit and logout reach GPUI's app delegate, which gets
/// `applicationShouldTerminate:` from the app.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 macros test `feature = "cargo-clippy"`.
fn quit_from_outside_asks_the_app_first() {
    use objc::runtime::Class;
    use objc::{sel, sel_impl};
    let _cx = cx();
    let class = Class::get("GPUIApplicationDelegate").expect("GPUI's delegate class");
    let method = || class.instance_method(sel!(applicationShouldTerminate:));
    assert!(method().is_none(), "GPUI does not implement it");
    assert!(mdt_app::menu::terminate::install());
    assert!(method().is_some());
}

#[cfg(not(target_os = "macos"))]
fn quit_from_outside_asks_the_app_first() {}

fn main() {
    let tests: [(&str, fn()); 16] = [
        (
            "spellings_of_one_file_in_one_request_open_one_window",
            spellings_of_one_file_in_one_request_open_one_window,
        ),
        (
            "quit_from_outside_asks_the_app_first",
            quit_from_outside_asks_the_app_first,
        ),
        (
            "failed_watch_is_not_retried_every_frame",
            failed_watch_is_not_retried_every_frame,
        ),
        (
            "open_failures_show_in_a_window",
            open_failures_show_in_a_window,
        ),
        (
            "wide_reader_shortcut_widens_the_column_and_persists",
            wide_reader_shortcut_widens_the_column_and_persists,
        ),
        (
            "opened_files_are_recent_after_relaunch",
            opened_files_are_recent_after_relaunch,
        ),
        (
            "file_already_open_focuses_its_window",
            file_already_open_focuses_its_window,
        ),
        (
            "external_changes_reload_ask_or_report_deletion",
            external_changes_reload_ask_or_report_deletion,
        ),
        (
            "zoom_in_twice_is_125_and_persists_across_relaunch",
            zoom_in_twice_is_125_and_persists_across_relaunch,
        ),
        (
            "task_toggle_is_an_undoable_edit",
            task_toggle_is_an_undoable_edit,
        ),
        (
            "task_toggle_refreshes_the_source_line",
            task_toggle_refreshes_the_source_line,
        ),
        (
            "settings_reload_live_into_every_window",
            settings_reload_live_into_every_window,
        ),
        (
            "no_path_opens_one_untitled_window",
            no_path_opens_one_untitled_window,
        ),
        (
            "existing_paths_open_one_window_each",
            existing_paths_open_one_window_each,
        ),
        (
            "missing_path_opens_empty_document_bound_to_it",
            missing_path_opens_empty_document_bound_to_it,
        ),
        (
            "unopenable_path_reports_error_and_others_still_open",
            unopenable_path_reports_error_and_others_still_open,
        ),
    ];
    for (name, test) in tests {
        test();
        println!("test {name} ... ok");
    }
}
