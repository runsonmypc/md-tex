//! Times decoding one image at a display size: `cargo run --release -p mdt-image --example
//! decode_timing -- <image> [max_w max_h]`.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = &args[0];
    let max = (
        args.get(1).and_then(|a| a.parse().ok()).unwrap_or(1400),
        args.get(2).and_then(|a| a.parse().ok()).unwrap_or(1400),
    );
    let bytes = std::fs::read(path).unwrap();
    for _ in 0..3 {
        let t = Instant::now();
        let r = mdt_image::decode(&bytes, max).unwrap();
        println!("decode to {}x{}: {:?}", r.width, r.height, t.elapsed());
    }
    let t = Instant::now();
    let img = image::load_from_memory(&bytes).unwrap();
    let full = t.elapsed();
    let t = Instant::now();
    let small = img.thumbnail_exact(max.0, max.0 * img.height() / img.width());
    println!("full decode {full:?}, thumbnail {:?}", t.elapsed());
    let t = Instant::now();
    let _ = small.into_rgba8();
    println!("to rgba {:?}", t.elapsed());
}
