//! Settings as app state: loaded at startup, reloaded live when the file changes, applied
//! to every window, with problems shown as notices.

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use gpui::{App, Global};

use crate::settings::{self, DEFAULTS, Settings};
use crate::view::{CurrentThemeMode, DocumentView};

pub struct AppSettings {
    pub settings: Settings,
    pub path: Option<PathBuf>,
    /// Problems from the last load, shown in windows opened later too.
    pub problems: Vec<String>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Global for AppSettings {}

pub fn init(cx: &mut App) {
    let path = settings::settings_path();
    let (s, problems) = match &path {
        Some(p) => settings::load(p, DEFAULTS, DEFAULTS),
        None => (DEFAULTS, Vec::new()),
    };
    // The watcher wakes the app only when the file may have changed; nothing polls.
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
    let watcher = path.as_deref().and_then(|p| {
        settings::watch(p, move || {
            let _ = tx.unbounded_send(());
        })
        .ok()
    });
    cx.set_global(CurrentThemeMode(s.theme));
    cx.set_global(AppSettings {
        settings: s,
        path,
        problems,
        _watcher: watcher,
    });
    cx.spawn(async move |cx| {
        while rx.next().await.is_some() {
            // One reload for a burst of events.
            while rx.try_recv().is_ok() {}
            // The app's own saves are still being written; reading the file now could
            // apply an older value over a newer one. Their events follow, so wait.
            while crate::persist::pending() {
                cx.background_executor()
                    .timer(Duration::from_millis(20))
                    .await;
            }
            cx.update(reload);
        }
    })
    .detach();
    // Saves still queued land before the app exits.
    cx.on_app_quit(|_| {
        crate::persist::flush();
        async {}
    })
    .detach();
}

fn reload(cx: &mut App) {
    let Some(state) = cx.try_global::<AppSettings>() else {
        return;
    };
    let Some(path) = state.path.clone() else {
        return;
    };
    let (s, problems) = settings::load(&path, state.settings, DEFAULTS);
    if s == state.settings && problems == state.problems {
        return;
    }
    apply(s, problems, cx);
}

/// Changes one setting (from the palette): applies it everywhere and saves it in the
/// background.
pub fn change(cx: &mut App, f: impl FnOnce(&mut Settings), key: &'static str) {
    if cx.try_global::<AppSettings>().is_none() {
        // Without `init` (offscreen use), changes apply but are not saved.
        cx.set_global(AppSettings {
            settings: DEFAULTS,
            path: None,
            problems: Vec::new(),
            _watcher: None,
        });
    }
    let state = cx.global::<AppSettings>();
    let mut s = state.settings;
    f(&mut s);
    if let (Some(path), Some(v)) = (state.path.clone(), settings::file_value(&s, key)) {
        let (tx, rx) = futures::channel::oneshot::channel();
        crate::persist::run(move || {
            let _ = tx.send(settings::write_key(&path, key, v).err());
        });
        cx.spawn(async move |cx| {
            if let Ok(Some(e)) = rx.await {
                cx.update(|cx| notify(&[format!("Cannot save setting `{key}`: {e}")], cx));
            }
        })
        .detach();
    }
    // The file's other problems still stand.
    let problems = cx.global::<AppSettings>().problems.clone();
    apply(s, problems, cx);
}

/// Makes `s` current. Problems are shown again only when they differ from the last ones,
/// so changing one setting does not repeat the notices about another.
fn apply(s: Settings, problems: Vec<String>, cx: &mut App) {
    cx.set_global(CurrentThemeMode(s.theme));
    let new_problems = {
        let state = cx.global_mut::<AppSettings>();
        state.settings = s;
        let new = state.problems != problems;
        state.problems = problems.clone();
        new
    };
    notify(if new_problems { &problems } else { &[] }, cx);
}

/// Redraws every window, with `problems` as new notices.
fn notify(problems: &[String], cx: &mut App) {
    for window in cx.windows() {
        if let Some(view) = window.downcast::<DocumentView>() {
            let _ = view.update(cx, |view, _, cx| {
                for p in problems {
                    view.notices.push(p.clone(), &[]);
                }
                cx.notify();
            });
        }
    }
    cx.refresh_windows();
}

pub fn current(cx: &App) -> Settings {
    cx.try_global::<AppSettings>()
        .map_or(DEFAULTS, |s| s.settings)
}
