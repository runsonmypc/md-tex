//! The keybinding registry: every app shortcut in one table, bound into GPUI and rendered
//! into `docs/shortcuts.md`, so the two cannot drift apart.

use gpui::{App, KeyBinding, actions};

actions!(
    md_tex,
    [
        Open,
        Save,
        SaveAs,
        CloseWindow,
        Quit,
        Undo,
        Redo,
        Find,
        FindNext,
        FindPrevious,
        Replace,
        CommandPalette,
        CycleViewMode,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        ToggleWideReader,
        Dismiss,
        Copy,
        CopyAsMarkdown,
        SelectAll,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        MoveWordLeft,
        MoveWordRight,
        MoveLineStart,
        MoveLineEnd,
        MoveDocStart,
        MoveDocEnd,
        PageUp,
        PageDown,
        SelectWordLeft,
        SelectWordRight,
        SelectLineStart,
        SelectLineEnd,
        SelectDocStart,
        SelectDocEnd,
        SelectPageUp,
        SelectPageDown,
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteLineLeft,
        Newline,
        Indent,
        Outdent,
        Cut,
        Paste,
        ShowCharacterPalette,
        ViewReader,
        ViewSource,
        ViewSplit,
        GoBack,
        GoForward,
        About
    ]
);

pub struct Binding {
    /// Name shown in the palette and the shortcuts doc.
    pub name: &'static str,
    /// GPUI keystrokes; each entry is an alternative.
    pub mac: &'static [&'static str],
    pub linux: &'static [&'static str],
    pub action: fn() -> Box<dyn gpui::Action>,
    key_binding: fn(&str) -> KeyBinding,
}

macro_rules! binding {
    ($name:literal, [$($mac:literal),*], [$($linux:literal),*], $action:ident) => {
        Binding {
            name: $name,
            mac: &[$($mac),*],
            linux: &[$($linux),*],
            action: || Box::new($action),
            key_binding: |k| KeyBinding::new(k, $action, None),
        }
    };
}

pub static BINDINGS: &[Binding] = &[
    binding!("File: Open", ["cmd-o"], ["ctrl-o"], Open),
    binding!("File: Save", ["cmd-s"], ["ctrl-s"], Save),
    binding!("File: Save As", ["cmd-shift-s"], ["ctrl-shift-s"], SaveAs),
    binding!("Window: Close", ["cmd-w"], ["ctrl-w"], CloseWindow),
    binding!("App: Quit", ["cmd-q"], ["ctrl-q"], Quit),
    binding!("App: About md-tex", [], [], About),
    binding!("Edit: Undo", ["cmd-z"], ["ctrl-z"], Undo),
    binding!(
        "Edit: Redo",
        ["cmd-shift-z"],
        ["ctrl-shift-z", "ctrl-y"],
        Redo
    ),
    binding!("Edit: Copy", ["cmd-c"], ["ctrl-c"], Copy),
    binding!(
        "Edit: Copy as Markdown",
        ["cmd-shift-c"],
        ["ctrl-shift-c"],
        CopyAsMarkdown
    ),
    binding!("Edit: Select All", ["cmd-a"], ["ctrl-a"], SelectAll),
    binding!("Edit: Cut", ["cmd-x"], ["ctrl-x"], Cut),
    binding!("Edit: Paste", ["cmd-v"], ["ctrl-v"], Paste),
    // macOS only: GPUI has no character palette on Linux.
    binding!(
        "Edit: Emoji & Symbols",
        ["ctrl-cmd-space"],
        [],
        ShowCharacterPalette
    ),
    binding!("Edit: New Line", ["enter"], ["enter"], Newline),
    binding!("Edit: Indent", ["tab"], ["tab"], Indent),
    binding!("Edit: Outdent", ["shift-tab"], ["shift-tab"], Outdent),
    binding!(
        "Edit: Delete Left",
        ["backspace", "shift-backspace"],
        ["backspace", "shift-backspace"],
        Backspace
    ),
    binding!("Edit: Delete Right", ["delete"], ["delete"], Delete),
    binding!(
        "Edit: Delete Word Left",
        ["alt-backspace"],
        ["ctrl-backspace"],
        DeleteWordLeft
    ),
    binding!(
        "Edit: Delete Word Right",
        ["alt-delete"],
        ["ctrl-delete"],
        DeleteWordRight
    ),
    binding!(
        "Edit: Delete to Line Start",
        ["cmd-backspace"],
        [],
        DeleteLineLeft
    ),
    binding!("Move: Left", ["left"], ["left"], MoveLeft),
    binding!("Move: Right", ["right"], ["right"], MoveRight),
    binding!("Move: Up", ["up"], ["up"], MoveUp),
    binding!("Move: Down", ["down"], ["down"], MoveDown),
    binding!("Move: Word Left", ["alt-left"], ["ctrl-left"], MoveWordLeft),
    binding!(
        "Move: Word Right",
        ["alt-right"],
        ["ctrl-right"],
        MoveWordRight
    ),
    binding!(
        "Move: Line Start",
        ["cmd-left", "home"],
        ["home"],
        MoveLineStart
    ),
    binding!("Move: Line End", ["cmd-right", "end"], ["end"], MoveLineEnd),
    binding!(
        "Move: Document Start",
        ["cmd-up"],
        ["ctrl-home"],
        MoveDocStart
    ),
    binding!("Move: Document End", ["cmd-down"], ["ctrl-end"], MoveDocEnd),
    binding!("Move: Page Up", ["pageup"], ["pageup"], PageUp),
    binding!("Move: Page Down", ["pagedown"], ["pagedown"], PageDown),
    binding!("Select: Left", ["shift-left"], ["shift-left"], SelectLeft),
    binding!(
        "Select: Right",
        ["shift-right"],
        ["shift-right"],
        SelectRight
    ),
    binding!("Select: Up", ["shift-up"], ["shift-up"], SelectUp),
    binding!("Select: Down", ["shift-down"], ["shift-down"], SelectDown),
    binding!(
        "Select: Word Left",
        ["alt-shift-left"],
        ["ctrl-shift-left"],
        SelectWordLeft
    ),
    binding!(
        "Select: Word Right",
        ["alt-shift-right"],
        ["ctrl-shift-right"],
        SelectWordRight
    ),
    binding!(
        "Select: To Line Start",
        ["cmd-shift-left", "shift-home"],
        ["shift-home"],
        SelectLineStart
    ),
    binding!(
        "Select: To Line End",
        ["cmd-shift-right", "shift-end"],
        ["shift-end"],
        SelectLineEnd
    ),
    binding!(
        "Select: To Document Start",
        ["cmd-shift-up"],
        ["ctrl-shift-home"],
        SelectDocStart
    ),
    binding!(
        "Select: To Document End",
        ["cmd-shift-down"],
        ["ctrl-shift-end"],
        SelectDocEnd
    ),
    binding!(
        "Select: Page Up",
        ["shift-pageup"],
        ["shift-pageup"],
        SelectPageUp
    ),
    binding!(
        "Select: Page Down",
        ["shift-pagedown"],
        ["shift-pagedown"],
        SelectPageDown
    ),
    binding!("Find: Find", ["cmd-f"], ["ctrl-f"], Find),
    binding!("Find: Next Match", ["cmd-g"], ["ctrl-g"], FindNext),
    binding!(
        "Find: Previous Match",
        ["cmd-shift-g"],
        ["ctrl-shift-g"],
        FindPrevious
    ),
    binding!("Find: Replace", ["cmd-alt-f"], ["ctrl-h"], Replace),
    binding!("Palette: Open", ["cmd-p"], ["ctrl-p"], CommandPalette),
    binding!("Navigate: Back", ["cmd-["], ["alt-left"], GoBack),
    binding!("Navigate: Forward", ["cmd-]"], ["alt-right"], GoForward),
    binding!("View: Cycle Mode", ["cmd-e"], ["ctrl-e"], CycleViewMode),
    binding!("View: Reader", [], [], ViewReader),
    binding!("View: Source", [], [], ViewSource),
    binding!("View: Split", [], [], ViewSplit),
    binding!(
        "View: Zoom In",
        ["cmd-=", "cmd-+"],
        ["ctrl-=", "ctrl-+"],
        ZoomIn
    ),
    binding!("View: Zoom Out", ["cmd--"], ["ctrl--"], ZoomOut),
    binding!("View: Reset Zoom", ["cmd-0"], ["ctrl-0"], ZoomReset),
    binding!(
        "View: Toggle Wide Reader",
        ["cmd-shift-l"],
        ["ctrl-shift-l"],
        ToggleWideReader
    ),
    binding!("Close Overlay", ["escape"], ["escape"], Dismiss),
];

fn keys(b: &Binding) -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        b.mac
    } else {
        b.linux
    }
}

/// The first shortcut for this platform, if any.
pub fn shortcut(b: &Binding) -> Option<&'static str> {
    keys(b).first().copied()
}

pub fn bind(cx: &mut App) {
    cx.bind_keys(
        BINDINGS
            .iter()
            .flat_map(|b| keys(b).iter().map(move |k| (b.key_binding)(k))),
    );
}

/// `cmd-shift-s` → `Cmd+Shift+S`.
pub fn display(keystroke: &str) -> String {
    // The key is everything after the last modifier separator; `ctrl--` is Ctrl and `-`.
    let (mods, key) = match keystroke.strip_suffix("--") {
        Some(mods) => (mods, "-"),
        None => keystroke.rsplit_once('-').unwrap_or(("", keystroke)),
    };
    let mut parts: Vec<String> = mods
        .split('-')
        .filter(|m| !m.is_empty())
        .map(|m| match m {
            "cmd" => "Cmd".to_owned(),
            "ctrl" => "Ctrl".to_owned(),
            "alt" => "Alt".to_owned(),
            "shift" => "Shift".to_owned(),
            other => other.to_owned(),
        })
        .collect();
    parts.push(match key {
        "escape" => "Escape".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "delete" => "Delete".to_owned(),
        "enter" => "Enter".to_owned(),
        "tab" => "Tab".to_owned(),
        "space" => "Space".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "pageup" => "Page Up".to_owned(),
        "pagedown" => "Page Down".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        k => k.to_uppercase(),
    });
    parts.join("+")
}

pub fn shortcuts_markdown() -> String {
    let mut out = String::from(
        "# Keyboard shortcuts\n\nGenerated from `crates/mdt-app/src/keymap.rs` by `cargo run -p mdt-app --bin gen-shortcuts`. Do not edit by hand.\n\n| Command | macOS | Linux |\n|---|---|---|\n",
    );
    for b in BINDINGS {
        let fmt = |ks: &[&str]| {
            if ks.is_empty() {
                return "—".to_owned();
            }
            ks.iter()
                .map(|k| format!("`{}`", display(k)))
                .collect::<Vec<_>>()
                .join(" or ")
        };
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            b.name,
            fmt(b.mac),
            fmt(b.linux)
        ));
    }
    out
}
