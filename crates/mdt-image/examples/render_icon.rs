//! Renders the app icon SVG into a macOS iconset, keeping the transparent background.
//!
//! `cargo run -p mdt-image --example render_icon -- assets/macos/icon.svg <dir>.iconset`,
//! then `iconutil -c icns <dir>.iconset -o assets/macos/md-tex.icns`.

use resvg::{tiny_skia, usvg};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [svg, out] = args.as_slice() else {
        eprintln!("usage: render_icon <icon.svg> <out.iconset>");
        std::process::exit(2);
    };
    let data = std::fs::read(svg).expect("read the SVG");
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("parse the SVG");
    std::fs::create_dir_all(out).expect("create the iconset directory");
    for size in [16u32, 32, 128, 256, 512] {
        for scale in [1, 2] {
            let px = size * scale;
            let mut pixmap = tiny_skia::Pixmap::new(px, px).expect("non-zero size");
            let s = px as f32 / tree.size().width();
            resvg::render(
                &tree,
                tiny_skia::Transform::from_scale(s, s),
                &mut pixmap.as_mut(),
            );
            let name = if scale == 1 {
                format!("icon_{size}x{size}.png")
            } else {
                format!("icon_{size}x{size}@2x.png")
            };
            pixmap
                .save_png(std::path::Path::new(out).join(name))
                .expect("write the PNG");
        }
    }
}
