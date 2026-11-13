#[test]
fn shortcuts_doc_matches_registry() {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/shortcuts.md"
    ))
    .unwrap();
    assert_eq!(
        doc,
        mdt_app::keymap::shortcuts_markdown(),
        "docs/shortcuts.md is stale: run `cargo run -p mdt-app --bin gen-shortcuts`"
    );
}

#[test]
fn display_names() {
    use mdt_app::keymap::display;
    assert_eq!(display("cmd-shift-s"), "Cmd+Shift+S");
    assert_eq!(display("ctrl--"), "Ctrl+-");
    assert_eq!(display("cmd-="), "Cmd+=");
    assert_eq!(display("escape"), "Escape");
    assert_eq!(display("ctrl-cmd-space"), "Ctrl+Cmd+Space");
}
