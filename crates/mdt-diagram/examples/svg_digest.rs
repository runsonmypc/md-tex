//! Prints a digest of each Mermaid file's SVG, to compare renderer changes:
//! `cargo run --release -p mdt-diagram --example svg_digest -- <file.mmd>...`.

use std::hash::{Hash, Hasher};

fn main() {
    mdt_diagram::set_mermaid_font(include_bytes!("../../mdt-text/fonts/Inter-Regular.ttf"));
    for path in std::env::args().skip(1) {
        let src = std::fs::read_to_string(&path).unwrap();
        let out =
            mdt_diagram::guarded(|| mdt_diagram::to_svg(mdt_diagram::Engine::Mermaid, &src, false));
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{out:?}").hash(&mut h);
        println!("{:016x} {path}", h.finish());
    }
}
