use mdt_app::settings::*;
use mdt_app::theme::ThemeMode;

fn custom() -> Settings {
    Settings {
        theme: ThemeMode::Light,
        zoom: 125,
        remote_images: true,
        soft_wrap: false,
        cursor_blink: false,
        initial_view_mode: ViewMode::Split,
        wide_reader: true,
    }
}

#[test]
fn first_launch_uses_defaults_and_creates_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("md-tex/settings.toml");
    let (s, problems) = load(&path, DEFAULTS, DEFAULTS);
    assert_eq!(s, DEFAULTS);
    assert!(problems.is_empty());
    assert!(!path.exists() && !path.parent().unwrap().exists());
}

#[test]
fn change_creates_file_then_keeps_other_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("md-tex/settings.toml");
    write_key(&path, "theme", "dark".into()).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theme = \"dark\"\n"
    );

    let original = "# My settings\nzoom = 125   # a bit bigger\n\n# theme below\ntheme = \"dark\" # night owl\nunknown = 1\n";
    std::fs::write(&path, original).unwrap();
    write_key(&path, "theme", "light".into()).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# My settings\nzoom = 125   # a bit bigger\n\n# theme below\ntheme = \"light\" # night owl\nunknown = 1\n"
    );
    write_key(&path, "soft_wrap", false.into()).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# My settings\nzoom = 125   # a bit bigger\n"));
    assert!(text.ends_with("soft_wrap = false\n"), "{text}");
}

#[test]
fn every_setting_round_trips_through_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let want = custom();
    for key in KEYS {
        write_key(&path, key, file_value(&want, key).unwrap()).unwrap();
    }
    assert_eq!(load(&path, DEFAULTS, DEFAULTS), (want, vec![]));
}

#[test]
fn out_of_range_zoom_is_ignored_with_notice_and_others_apply() {
    let previous = Settings {
        zoom: 150,
        ..DEFAULTS
    };
    let (s, problems) = apply("zoom = 900\ntheme = \"dark\"\n", previous, DEFAULTS);
    assert_eq!(s.zoom, 150, "zoom keeps its previous value");
    assert_eq!(s.theme, ThemeMode::Dark, "other settings apply");
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].contains("`zoom`") && problems[0].contains("50 to 300"),
        "{problems:?}"
    );
}

#[test]
fn unknown_key_and_wrong_type_are_ignored() {
    let (s, problems) = apply(
        "colour = \"red\"\nsoft_wrap = \"yes\"\ncursor_blink = false\n",
        DEFAULTS,
        DEFAULTS,
    );
    assert_eq!(s.soft_wrap, DEFAULTS.soft_wrap);
    assert!(!s.cursor_blink);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].contains("`colour`") && problems[1].contains("`soft_wrap`"));
}

#[test]
fn broken_file_keeps_last_good_settings_and_reports_line() {
    let previous = custom();
    let (s, problems) = apply(
        "theme = \"dark\"\nzoom = 120\nsoft_wrap = = true\n",
        previous,
        DEFAULTS,
    );
    assert_eq!(s, previous);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("line 3"), "{problems:?}");
}

#[test]
fn removed_key_returns_to_default() {
    let (s, _) = apply("zoom = 125\n", custom(), DEFAULTS);
    assert_eq!(
        s,
        Settings {
            zoom: 125,
            ..DEFAULTS
        }
    );
}

#[test]
fn settings_path_is_per_platform() {
    let p = settings_path().unwrap();
    assert!(p.ends_with("md-tex/settings.toml"));
    if cfg!(target_os = "macos") {
        assert!(p.to_string_lossy().contains("Library/Application Support"));
    }
}

/// Release check: run with `cargo test -p mdt-app --test settings -- --ignored`.
#[test]
#[ignore = "release check: fails while any default is pending"]
fn release_check_no_pending_defaults() {
    let pending = pending_defaults();
    assert!(
        pending.is_empty(),
        "defaults still pending a decision: {}",
        pending.join(", ")
    );
}

#[test]
fn watcher_sees_edits_from_another_program_within_500_ms() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, "theme = \"light\"\n").unwrap();
    let changed = Arc::new(AtomicBool::new(false));
    let flag = changed.clone();
    let _w = watch(&path, move || flag.store(true, Ordering::SeqCst)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    changed.store(false, Ordering::SeqCst);

    // Save by rename, as many editors do.
    let tmp = dir.path().join(".settings.toml.tmp");
    std::fs::write(&tmp, "theme = \"dark\"\n").unwrap();
    let start = Instant::now();
    std::fs::rename(&tmp, &path).unwrap();
    while !changed.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "no change seen within 500 ms"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let (s, _) = load(&path, DEFAULTS, DEFAULTS);
    assert_eq!(s.theme, ThemeMode::Dark);
}

#[test]
fn watcher_creates_a_missing_settings_directory_and_sees_the_first_file() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("md-tex").join("settings.toml");
    let changed = Arc::new(AtomicBool::new(false));
    let flag = changed.clone();
    let _w = watch(&path, move || flag.store(true, Ordering::SeqCst)).unwrap();
    assert!(path.parent().unwrap().is_dir());
    std::thread::sleep(Duration::from_millis(100));
    changed.store(false, Ordering::SeqCst);
    let start = Instant::now();
    std::fs::write(&path, "theme = \"dark\"\n").unwrap();
    while !changed.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "no change seen within 500 ms"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn reading_the_settings_file_is_not_a_change() {
    // On Linux, opening a watched file is an inotify event. If it counted, each reload
    // would trigger the next and the app would spin at full CPU while idle.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, "theme = \"light\"\n").unwrap();
    let changed = Arc::new(AtomicBool::new(false));
    let flag = changed.clone();
    let _w = watch(&path, move || flag.store(true, Ordering::SeqCst)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    changed.store(false, Ordering::SeqCst);
    for _ in 0..5 {
        let (s, _) = load(&path, DEFAULTS, DEFAULTS);
        assert_eq!(s.theme, ThemeMode::Light);
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !changed.load(Ordering::SeqCst),
        "reading the file counted as a change"
    );
}

#[test]
fn settings_doc_lists_exactly_the_v1_settings() {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/settings.md"
    ))
    .unwrap();
    let headings: Vec<&str> = doc
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .filter(|h| *h != "Example")
        .collect();
    assert_eq!(headings, KEYS);
    // The example must be a valid file that sets every key.
    let example = doc
        .split("```toml\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    assert_eq!(apply(example, DEFAULTS, DEFAULTS).1, Vec::<String>::new());
}

#[test]
fn settings_doc_defaults_match_the_built_in_ones() {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/settings.md"
    ))
    .unwrap();
    for section in doc.split("\n## ").skip(1) {
        let key = section.lines().next().unwrap();
        if key == "Example" {
            continue;
        }
        let documented = section
            .split("Default: `")
            .nth(1)
            .and_then(|r| r.split('`').next())
            .unwrap_or_else(|| panic!("`{key}` has no Default line"));
        let built_in = file_value(&DEFAULTS, key).unwrap().to_string();
        assert_eq!(documented, built_in.trim(), "default of `{key}`");
    }
}

#[test]
fn zoom_steps_stay_in_range() {
    assert_eq!(zoom_step(100, true), 110);
    assert_eq!(zoom_step(zoom_step(100, true), true), 125);
    assert_eq!(zoom_step(100, false), 90);
    assert_eq!(zoom_step(300, true), 300);
    assert_eq!(zoom_step(50, false), 50);
    assert_eq!(zoom_step(113, true), 125, "from a level set in the file");
    assert!(ZOOM_STEPS.iter().all(|z| ZOOM_RANGE.contains(z)));
}
