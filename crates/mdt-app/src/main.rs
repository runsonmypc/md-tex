use std::path::PathBuf;

use futures::StreamExt;

fn main() {
    use mdt_app::launch::{Cli, USAGE, parse_args, version_line};
    // Options are answered before anything else, also while another instance runs.
    let mut paths = match parse_args(std::env::args_os().skip(1)) {
        Cli::Open(paths) => paths,
        Cli::Version => return println!("{}", version_line()),
        Cli::Help => return print!("{USAGE}"),
        Cli::Unknown(option) => {
            eprint!("md-tex: unknown option {option}\n{USAGE}");
            std::process::exit(2);
        }
    };
    // glibc raises its mmap threshold (up to 32 MiB) each time a large block is freed, after
    // which multi-megabyte decode and resize buffers come from per-thread heaps that keep
    // the memory: the media corpus peaked at twice its working set (802 MB against 408 MB).
    // A fixed 1 MiB threshold hands such buffers back to the system when they are freed.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 1 << 20);
    }
    // A second launch hands its paths to the running instance and exits. The bench
    // harness launches the app repeatedly and measures each launch, so it skips this.
    #[cfg(not(feature = "bench"))]
    let listener = match mdt_app::ipc::socket_path().and_then(|s| mdt_app::ipc::claim(&s, &paths)) {
        Ok(mdt_app::ipc::Claim::Forwarded) => return,
        Ok(mdt_app::ipc::Claim::Primary(l)) => Some(l),
        Err(e) => {
            eprintln!("md-tex: single-instance socket unavailable: {e}");
            None
        }
    };
    // Finder and `open` deliver files as open-URL events. Files opened at launch arrive
    // before `run` starts; later ones arrive while the app is running.
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
    #[cfg(not(feature = "bench"))]
    if let Some(listener) = listener {
        let tx = tx.clone();
        mdt_app::ipc::serve(listener, move |paths| {
            let _ = tx.unbounded_send(paths);
        });
    }
    let app = gpui_platform::application();
    // Before `run`, which sets GPUI's delegate: Dock > Quit and logout then ask first.
    #[cfg(target_os = "macos")]
    mdt_app::menu::terminate::install();
    app.on_open_urls(move |urls| {
        let paths = urls
            .iter()
            .filter_map(|u| u.strip_prefix("file://"))
            .map(|p| PathBuf::from(mdt_app::links::percent_decode(p)))
            .collect();
        let _ = tx.unbounded_send(paths);
    });
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            mdt_app::launch::open_all(&[], cx);
        }
    });
    app.run(move |cx| {
        mdt_app::app_settings::init(cx);
        mdt_app::recent::init(cx);
        mdt_app::keymap::bind(cx);
        mdt_app::menu::set(cx);
        // macOS keeps its native alerts.
        #[cfg(not(target_os = "macos"))]
        mdt_app::prompt::install(cx);
        cx.on_action(|_: &mdt_app::keymap::Quit, cx| mdt_app::view::quit(cx));
        cx.on_action(|_: &mdt_app::keymap::About, cx| mdt_app::menu::about(cx));
        #[cfg(target_os = "macos")]
        mdt_app::menu::terminate::listen(cx);
        // Linux has no app without windows: closing the last one quits (GPUI's default
        // there, stated so it cannot change under us). macOS apps keep running.
        #[cfg(not(target_os = "macos"))]
        cx.set_quit_mode(gpui::QuitMode::LastWindowClosed);
        while let Ok(more) = rx.try_recv() {
            paths.extend(more);
        }
        // Failures show in a window (a new untitled one if none opened), never only on
        // stderr.
        let errors = mdt_app::launch::open_all(&paths, cx);
        mdt_app::launch::report(errors, cx);
        if cx.windows().is_empty() {
            mdt_app::view::quit_now(cx);
        }
        cx.spawn(async move |cx| {
            // Later Finder opens and forwarded launches (an empty list opens a new window).
            while let Some(paths) = rx.next().await {
                cx.update(|cx| {
                    let errors = mdt_app::launch::open_all(&paths, cx);
                    mdt_app::launch::report(errors, cx);
                    cx.activate(true);
                });
            }
        })
        .detach();
        #[cfg(feature = "bench")]
        mdt_app::bench::start(cx);
        cx.activate(true);
    });
}
