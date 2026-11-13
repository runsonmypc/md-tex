//! Writes `docs/shortcuts.md` from the keybinding registry.

fn main() -> std::io::Result<()> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/shortcuts.md");
    std::fs::write(&path, mdt_app::keymap::shortcuts_markdown())?;
    println!("wrote {}", path.display());
    Ok(())
}
