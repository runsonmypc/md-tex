//! The macOS menu bar, the About text, and (macOS) quitting from outside the app. The menu
//! lists only commands that work today; shortcuts come from the keybinding registry. Linux
//! has no global menu bar, so setting it is a no-op there.

use gpui::{App, Menu, MenuItem, OsAction};

use crate::keymap::{
    About, CloseWindow, CommandPalette, Copy, CopyAsMarkdown, Cut, CycleViewMode, Find, FindNext,
    FindPrevious, GoBack, GoForward, Open, Paste, Quit, Redo, Replace, Save, SaveAs, SelectAll,
    ShowCharacterPalette, ToggleWideReader, Undo, ViewReader, ViewSource, ViewSplit,
};

pub fn menus() -> Vec<Menu> {
    vec![
        Menu::new("md-tex").items([
            MenuItem::action("About md-tex", About),
            MenuItem::separator(),
            MenuItem::action("Quit md-tex", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("Open…", Open),
            MenuItem::separator(),
            MenuItem::action("Save", Save),
            MenuItem::action("Save As…", SaveAs),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::action("Copy as Markdown", CopyAsMarkdown),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::action("Emoji & Symbols", ShowCharacterPalette),
            MenuItem::separator(),
            MenuItem::action("Find…", Find),
            MenuItem::action("Find Next", FindNext),
            MenuItem::action("Find Previous", FindPrevious),
            MenuItem::action("Replace…", Replace),
        ]),
        Menu::new("View").items([
            MenuItem::action("Reader", ViewReader),
            MenuItem::action("Source", ViewSource),
            MenuItem::action("Split", ViewSplit),
            MenuItem::separator(),
            MenuItem::action("Next Mode", CycleViewMode),
            MenuItem::separator(),
            MenuItem::action("Wide Reader", ToggleWideReader),
            MenuItem::separator(),
            MenuItem::action("Command Palette…", CommandPalette),
            MenuItem::separator(),
            MenuItem::action("Back", GoBack),
            MenuItem::action("Forward", GoForward),
        ]),
    ]
}

pub fn set(cx: &mut App) {
    #[cfg(target_os = "macos")]
    disable_automatic_items();
    cx.set_menus(menus());
}

pub const COPYRIGHT: &str = "Copyright (C) 2026 Alex";
pub const LICENSE: &str = "md-tex is free software: you can redistribute it and/or modify it \
under the terms of the GNU General Public License, version 3. It comes with no warranty.";

/// The About text: a title (name and version) and the copyright and licence.
pub fn about_text() -> (String, String) {
    (
        crate::launch::version_line(),
        format!("{COPYRIGHT}\n\n{LICENSE}"),
    )
}

/// About md-tex (app menu and palette): the standard About panel on macOS, a prompt in the
/// active window on Linux.
pub fn about(cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        about_panel();
    }
    // Deferred: the palette dispatches this from inside its window's update, and a window
    // can't be updated again while it is (the prompt was silently never shown).
    #[cfg(not(target_os = "macos"))]
    cx.defer(|cx| {
        let Some(w) = cx.active_window() else { return };
        let (title, detail) = about_text();
        let _ = w.update(cx, |_, window, cx| {
            // Only OK; nothing waits for the answer, so the receiver is dropped.
            drop(window.prompt(gpui::PromptLevel::Info, &title, Some(&detail), &["OK"], cx));
        });
    });
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 macros test `feature = "cargo-clippy"`.
fn about_panel() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    let ns = |s: &str| -> *mut Object {
        let c = std::ffi::CString::new(s).unwrap_or_default();
        unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
    };
    // An empty `Version` drops the build number AppKit would show in parentheses.
    let options = [
        ("ApplicationName", "md-tex".to_owned()),
        ("ApplicationVersion", env!("CARGO_PKG_VERSION").to_owned()),
        ("Version", String::new()),
        ("Copyright", format!("{COPYRIGHT}\n{LICENSE}")),
    ];
    unsafe {
        let dict: *mut Object = msg_send![class!(NSMutableDictionary), dictionary];
        for (k, v) in options {
            let _: () = msg_send![dict, setObject: ns(&v) forKey: ns(k)];
        }
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, orderFrontStandardAboutPanelWithOptions: dict];
    }
}

/// GPUI's app delegate has no `applicationShouldTerminate:`, so AppKit quits at once when
/// asked from outside the app (Dock > Quit, Cmd+Tab then Q, logging out) and unsaved
/// changes are lost. This adds it: with nothing unsaved the quit goes ahead; otherwise
/// AppKit waits (`NSTerminateLater`, which also holds a logout) while the app asks about
/// each unsaved window, then is told whether to quit.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 macros test `feature = "cargo-clippy"`.
pub mod terminate {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures::StreamExt;
    use futures::channel::mpsc::UnboundedSender;
    use objc::runtime::{BOOL, Class, NO, Object, Sel, YES};
    use objc::{class, msg_send, sel, sel_impl};

    /// `NSApplicationTerminateReply` values.
    pub const CANCEL: usize = 0;
    pub const NOW: usize = 1;
    pub const LATER: usize = 2;

    /// Set once the app's own quit has been confirmed.
    static ALLOWED: AtomicBool = AtomicBool::new(false);
    static REQUESTS: OnceLock<UnboundedSender<()>> = OnceLock::new();

    /// Adds the method to GPUI's delegate class. Call it before the app runs: AppKit notes
    /// which methods a delegate has when the delegate is set. Returns whether it was added.
    pub fn install() -> bool {
        let Some(class) = Class::get("GPUIApplicationDelegate") else {
            return false;
        };
        type Method = extern "C" fn(&Object, Sel, *mut Object) -> usize;
        unsafe {
            objc::runtime::class_addMethod(
                class as *const Class as *mut Class,
                sel!(applicationShouldTerminate:),
                std::mem::transmute::<Method, objc::runtime::Imp>(should_terminate),
                c"Q@:@".as_ptr(),
            ) != NO
        }
    }

    /// Answers each quit the delegate held back: asks about unsaved windows, then tells
    /// AppKit to quit or not.
    pub fn listen(cx: &mut gpui::App) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        if REQUESTS.set(tx).is_err() {
            return;
        }
        cx.spawn(async move |cx| {
            while rx.next().await.is_some() {
                let windows = cx.update(crate::view::document_windows);
                let quit = crate::view::confirm_quit(windows, cx).await;
                if quit {
                    allow();
                }
                unsafe {
                    let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
                    let _: () =
                        msg_send![app, replyToApplicationShouldTerminate: if quit { YES } else { NO }];
                }
            }
        })
        .detach();
    }

    /// Lets the next quit through: the app's own quit, already confirmed.
    pub fn allow() {
        ALLOWED.store(true, Ordering::Release);
    }

    extern "C" fn should_terminate(_: &Object, _: Sel, _: *mut Object) -> usize {
        reply(any_window_edited())
    }

    /// Whether a window shows the unsaved-changes marker (`set_window_edited`, kept in
    /// step with the document by every frame).
    fn any_window_edited() -> bool {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let windows: *mut Object = msg_send![app, windows];
            let count: usize = msg_send![windows, count];
            (0..count).any(|i| {
                let w: *mut Object = msg_send![windows, objectAtIndex: i];
                let edited: BOOL = msg_send![w, isDocumentEdited];
                edited != NO
            })
        }
    }

    fn reply(unsaved: bool) -> usize {
        // Cancelling instead (and quitting later by ourselves) would abort a logout or
        // restart even with nothing to save.
        if ALLOWED.load(Ordering::Acquire) || !unsaved {
            return NOW;
        }
        // Without a listener (before the app runs) nothing could ask; quit as before.
        match REQUESTS.get() {
            Some(tx) if tx.unbounded_send(()).is_ok() => LATER,
            _ => NOW,
        }
    }

    /// What the method answers. That it is added to GPUI's class is checked in
    /// `tests/launch.rs`, where the platform (which defines the class) exists; the AppKit
    /// side (Dock > Quit or a logout reaching the delegate) needs a GUI session.
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn outside_quit_waits_for_the_answer_only_with_unsaved_changes() {
            assert_eq!(reply(true), NOW, "no listener yet");
            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            REQUESTS.set(tx).unwrap();
            assert_eq!(reply(false), NOW, "nothing to save: a logout goes on");
            assert!(rx.try_recv().is_err(), "without asking");
            assert_eq!(reply(true), LATER);
            assert_eq!(rx.try_recv().ok(), Some(()), "the app is asked");
            allow();
            assert_eq!(reply(true), NOW, "the confirmed quit goes through");
            assert!(rx.try_recv().is_err(), "and asks nothing more");
        }
    }
}

/// AppKit adds "Start Dictation…" and "Emoji & Symbols" to any menu named Edit. Dictation
/// does nothing in GPUI, and our own Emoji & Symbols item takes its shortcut from the
/// registry, which AppKit's would duplicate.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 macros test `feature = "cargo-clippy"`.
fn disable_automatic_items() {
    use objc::runtime::{Object, YES};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let defaults: *mut Object = msg_send![class!(NSUserDefaults), standardUserDefaults];
        for key in [
            c"NSDisabledDictationMenuItem",
            c"NSDisabledCharacterPaletteMenuItem",
        ] {
            let key: *mut Object = msg_send![class!(NSString), stringWithUTF8String: key.as_ptr()];
            let _: () = msg_send![defaults, setBool: YES forKey: key];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_names_the_version_copyright_and_licence() {
        let (title, detail) = about_text();
        assert_eq!(title, crate::launch::version_line());
        assert!(detail.starts_with("Copyright (C) 2026 Alex"), "{detail}");
        assert!(
            detail.contains("GNU General Public License, version 3")
                && detail.contains("no warranty"),
            "{detail}"
        );
    }

    /// Every menu command has a registry binding, so the menu shows its shortcut.
    #[test]
    fn menu_commands_are_in_the_registry() {
        let registry: Vec<&str> = crate::keymap::BINDINGS
            .iter()
            .map(|b| (b.action)().name())
            .collect();
        for menu in menus() {
            for item in menu.items {
                if let MenuItem::Action { action, name, .. } = item {
                    assert!(registry.contains(&action.name()), "{name} has no binding");
                }
            }
        }
    }
}
