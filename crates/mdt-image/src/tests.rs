use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use image::{ImageBuffer, ImageEncoder, Rgba};

use super::*;

fn png(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
    let img = ImageBuffer::from_pixel(w, h, Rgba(color));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&img, w, h, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

/// A landscape JPEG carrying EXIF orientation 6 (rotate 90° clockwise to display).
fn jpeg_orientation_6(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30]));
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .write_image(&img, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    // APP1: "Exif\0\0", a big-endian TIFF header, one IFD entry (0x0112 orientation = 6).
    let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01".to_vec();
    exif.extend_from_slice(&[0x01, 0x12, 0x00, 0x03, 0, 0, 0, 1, 0, 6, 0, 0]);
    exif.extend_from_slice(&[0, 0, 0, 0]);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&exif);
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn animated_gif() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            let frame = image::Frame::new(ImageBuffer::from_pixel(8, 6, Rgba(color)));
            enc.encode_frame(frame).unwrap();
        }
    }
    out
}

#[test]
fn resolves_relative_absolute_and_file_urls() {
    let dir = Path::new("/notes");
    assert_eq!(
        resolve("img/plot.png", Some(dir)),
        Source::Local(PathBuf::from("/notes/img/plot.png"))
    );
    assert_eq!(
        resolve("/abs/a.png", Some(dir)),
        Source::Local(PathBuf::from("/abs/a.png"))
    );
    assert_eq!(
        resolve("file:///tmp/my%20plot.png", Some(dir)),
        Source::Local(PathBuf::from("/tmp/my plot.png"))
    );
    assert_eq!(
        resolve("file://localhost/tmp/a.png", None),
        Source::Local(PathBuf::from("/tmp/a.png"))
    );
    assert_eq!(
        resolve("https://example.com/a.png", Some(dir)),
        Source::Remote("https://example.com/a.png".into())
    );
    assert!(matches!(
        resolve("data:image/png;base64,AAAA", Some(dir)),
        Source::Unsupported(_)
    ));
}

#[test]
fn orientation_6_jpeg_is_portrait() {
    let bytes = jpeg_orientation_6(40, 20);
    assert_eq!(probe_bytes(&bytes).unwrap(), (20, 40));
    let r = decode(&bytes, (1000, 1000)).unwrap();
    assert_eq!((r.width, r.height), (20, 40));
}

#[test]
fn animated_gif_yields_its_first_frame() {
    let r = decode(&animated_gif(), (100, 100)).unwrap();
    assert_eq!((r.width, r.height), (8, 6));
    assert_eq!(&r.rgba[..4], &[255, 0, 0, 255], "the first frame is red");
}

#[test]
fn decodes_png_and_webp() {
    let r = decode(&png(5, 3, [1, 2, 3, 255]), (100, 100)).unwrap();
    assert_eq!(
        (r.width, r.height, &r.rgba[..4]),
        (5, 3, &[1u8, 2, 3, 255][..])
    );
    let img = image::RgbaImage::from_pixel(4, 4, Rgba([0, 255, 0, 255]));
    let mut webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
        .write_image(&img, 4, 4, image::ExtendedColorType::Rgba8)
        .unwrap();
    assert_eq!(probe_bytes(&webp).unwrap(), (4, 4));
    assert_eq!(
        decode(&webp, (100, 100)).unwrap().rgba[..4],
        [0, 255, 0, 255]
    );
}

#[test]
fn sizing_caps_at_the_column_and_never_stretches() {
    assert_eq!(
        display_size((4000, 3000), (None, None), 1.0, 700.0),
        (700.0, 525.0)
    );
    assert_eq!(
        display_size((32, 32), (None, None), 1.0, 700.0),
        (32.0, 32.0)
    );
    // <img width> overrides, keeping the aspect ratio; still capped.
    assert_eq!(
        display_size((400, 200), (Some(100), None), 1.0, 700.0),
        (100.0, 50.0)
    );
    assert_eq!(
        display_size((400, 200), (Some(1400), Some(100)), 1.0, 700.0),
        (700.0, 50.0)
    );
    assert_eq!(
        display_size((32, 32), (None, None), 2.0, 700.0),
        (64.0, 64.0)
    );
}

#[test]
fn downscales_to_the_needed_size() {
    let r = decode(&png(400, 300, [9, 9, 9, 255]), (100, 100)).unwrap();
    assert_eq!((r.width, r.height), (100, 75));
    assert_eq!(fit((32, 32), (700, 700)), (32, 32), "never upscales");
}

#[test]
fn cache_evicts_off_screen_entries_first() {
    let mut c: RasterCache<&str, ()> = RasterCache::new(300);
    let visible = |k: &&str| ["a", "c"].contains(k);
    c.insert("a", ().into(), 100, visible);
    c.insert("b", ().into(), 100, visible);
    c.insert("c", ().into(), 100, visible);
    // "a" is least recently used, but on screen; "b" is off screen and goes first.
    c.insert("d", ().into(), 100, visible);
    assert!(c.contains(&"a") && !c.contains(&"b") && c.contains(&"c"));
    assert_eq!(c.used(), 300);
    // With nothing off screen left but "d", the oldest off-screen entry goes, then the
    // least recently used visible one.
    c.get(&"a");
    c.insert("e", ().into(), 200, |k| ["a", "c", "e"].contains(k));
    assert!(!c.contains(&"d") && !c.contains(&"c") && c.contains(&"a"));
    assert!(c.used() <= 300);
}

#[test]
fn missing_and_corrupt_images_give_their_errors() {
    let dir = tempfile::tempdir().unwrap();
    let fonts = svg::Fonts::empty();
    let missing = dir.path().join("img/missing.png");
    assert_eq!(probe(&missing), Err(ImageError::NotFound));
    assert_eq!(
        load(&missing, (100, 100), &fonts),
        Err(ImageError::NotFound)
    );
    let truncated = dir.path().join("cut.png");
    let bytes = png(64, 64, [1, 2, 3, 255]);
    std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
    let err = load(&truncated, (100, 100), &fonts).unwrap_err();
    assert!(matches!(err, ImageError::Decode(_)), "{err:?}");
    assert!(err.to_string().contains("could not decode"));
    let text = dir.path().join("notes.png");
    std::fs::write(&text, "not an image").unwrap();
    assert!(matches!(probe(&text), Err(ImageError::Decode(_))));
}

#[test]
fn svg_never_fetches_external_references() {
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="10">
        <rect width="20" height="10" fill="#ff0000"/>
        <image href="https://example.com/x.png" width="5" height="5"/>
        <image xlink:href="/etc/hosts" width="5" height="5"/>
    </svg>"##;
    let (r, refused) = svg::rasterize_counting(svg, 1.0, &svg::Fonts::empty()).unwrap();
    assert_eq!(refused, 2, "both references go to the refusing resolver");
    assert_eq!((r.width, r.height), (20, 10));
    assert_eq!(&r.rgba[..4], &[255, 0, 0, 255], "the rest still renders");
}

#[test]
fn svg_scripts_are_ignored() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
        <script>document.querySelector('circle').setAttribute('fill', 'blue')</script>
        <circle cx="5" cy="5" r="5" fill="red" onclick="alert(1)"/>
    </svg>"#;
    let r = svg::rasterize(svg, 2.0, &svg::Fonts::empty()).unwrap();
    assert_eq!((r.width, r.height), (20, 20));
    let center = ((10 * 20 + 10) * 4) as usize;
    assert_eq!(&r.rgba[center..center + 4], &[255, 0, 0, 255]);
}

#[test]
fn svg_text_uses_only_the_given_fonts() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="20">
        <text x="0" y="15" font-family="Comic Sans MS" font-size="14">Hi</text></svg>"#;
    let ink = |r: &Raster| {
        r.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0)
            .count()
    };
    let none = svg::rasterize(svg, 1.0, &svg::Fonts::empty()).unwrap();
    assert_eq!(ink(&none), 0, "no system fonts are loaded");
    let font: &'static [u8] = include_bytes!("../../mdt-text/fonts/Inter-Regular.ttf");
    let fonts = svg::Fonts::new(&[font], "Inter", "Inter");
    let some = svg::rasterize(svg, 1.0, &fonts).unwrap();
    assert!(
        ink(&some) > 0,
        "unknown families fall back to the bundled font"
    );
}

#[test]
fn watcher_sees_an_overwritten_image() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plot.png");
    std::fs::write(&path, png(4, 4, [255, 0, 0, 255])).unwrap();
    let (tx, rx) = mpsc::channel();
    let mut w = Watcher::new(move |p| {
        let _ = tx.send(p);
    })
    .unwrap();
    w.watch(&path);
    // Let the watcher settle so the initial write is not reported.
    std::thread::sleep(Duration::from_millis(200));
    while rx.try_recv().is_ok() {}
    let start = std::time::Instant::now();
    std::fs::write(&path, png(4, 4, [0, 0, 255, 255])).unwrap();
    let changed = rx.recv_timeout(Duration::from_millis(500)).unwrap();
    assert_eq!(changed, path);
    let r = load(&changed, (10, 10), &svg::Fonts::empty()).unwrap();
    assert_eq!(&r.rgba[..4], &[0, 0, 255, 255], "the new pixels");
    assert!(start.elapsed() < Duration::from_millis(500));
}

/// A 1×1 PNG whose header declares `w`×`h` pixels.
fn png_header(w: u32, h: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    // Signature, then the IHDR chunk: length, type, width, height, five more bytes, CRC.
    let mut out = png(1, 1, [0, 0, 0, 255]);
    out[16..20].copy_from_slice(&w.to_be_bytes());
    out[20..24].copy_from_slice(&h.to_be_bytes());
    let crc = crc32(&out[12..29]);
    out[29..33].copy_from_slice(&crc.to_be_bytes());
    out
}

/// A 1.6 MB PNG of 20000×20000 pixels decoded to gigabytes; the header is refused first.
#[test]
fn huge_dimensions_are_refused_before_decoding() {
    let huge = png_header(20_000, 20_000);
    assert_eq!(probe_bytes(&huge), Err(ImageError::TooLarge));
    assert_eq!(decode(&huge, (100, 100)), Err(ImageError::TooLarge));
    let wide = png_header(MAX_SIDE + 1, 1);
    assert_eq!(probe_bytes(&wide), Err(ImageError::TooLarge));
    // A 12-MP photo is fine.
    assert_eq!(probe_bytes(&png_header(4000, 3000)), Ok((4000, 3000)));
}

/// A FIFO blocked a worker on open, and a link to /dev/zero read forever.
#[cfg(unix)]
#[test]
fn special_and_oversized_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let fonts = svg::Fonts::empty();
    let fifo = dir.path().join("pipe.png");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let zero = dir.path().join("zero.svg");
    std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
    for path in [&fifo, &zero] {
        assert!(
            matches!(probe(path), Err(ImageError::Decode(_))),
            "{path:?}"
        );
        assert!(
            matches!(load(path, (10, 10), &fonts), Err(ImageError::Decode(_))),
            "{path:?}"
        );
    }
    // Sparse files: their size costs no disk.
    let big = dir.path().join("big.png");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(MAX_FILE_BYTES + 1).unwrap();
    assert_eq!(load(&big, (10, 10), &fonts), Err(ImageError::TooLarge));
    let big_svg = dir.path().join("big.svg");
    let f = std::fs::File::create(&big_svg).unwrap();
    f.set_len(svg::MAX_BYTES as u64 + 1).unwrap();
    assert_eq!(probe(&big_svg), Err(ImageError::TooLarge));
}

#[test]
fn a_decoder_panic_becomes_an_error() {
    let err = guarded(|| -> Result<(), ImageError> { panic!("boom") }).unwrap_err();
    assert_eq!(err, ImageError::Decode("the decoder failed: boom".into()));
}

fn nested_groups(depth: usize, attrs: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="2000" height="2000">{}<rect width="2000" height="2000"/>{}</svg>"#,
        format!("<g {attrs}>").repeat(depth),
        "</g>".repeat(depth)
    )
}

/// Background workers on macOS have 512 KiB stacks; about 400 nested groups overflowed
/// one in usvg and aborted the app.
#[test]
fn deep_svg_nesting_is_refused_without_overflowing_a_small_stack() {
    let on_small_stack = |svg: String| {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || svg::size(svg.as_bytes()))
            .unwrap()
            .join()
            .unwrap()
    };
    let err = on_small_stack(nested_groups(400, "")).unwrap_err();
    assert!(err.contains("deeper"), "{err}");
    assert_eq!(
        on_small_stack(nested_groups(svg::MAX_DEPTH - 2, "")),
        Ok((2000, 2000))
    );
    // `use` references count as levels.
    let mut chain = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect id="u0" width="1" height="1"/>"#,
    );
    for i in 1..200 {
        chain.push_str(&format!(r##"<use id="u{i}" href="#u{}"/>"##, i - 1));
    }
    chain.push_str("</svg>");
    assert!(on_small_stack(chain).unwrap_err().contains("deeper"));
}

#[test]
fn svg_expansion_bombs_are_refused() {
    // Ten levels of ten `use` elements each: ten billion copies.
    let mut bomb = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><defs><rect id="l0" width="1" height="1"/>"#,
    );
    for level in 1..=10 {
        bomb.push_str(&format!(r#"<g id="l{level}">"#));
        for _ in 0..10 {
            bomb.push_str(&format!(r##"<use xlink:href="#l{}"/>"##, level - 1));
        }
        bomb.push_str("</g>");
    }
    bomb.push_str(r##"</defs><use href="#l10"/></svg>"##);
    let start = std::time::Instant::now();
    let err = svg::size(bomb.as_bytes()).unwrap_err();
    assert!(err.contains("too many elements"), "{err}");
    assert!(start.elapsed() < Duration::from_secs(2));
    // A marker with 1,000 elements on a path of 1,000 vertices.
    let marker = r#"<circle r="1"/>"#.repeat(1000);
    let path: String = (0..1000).map(|i| format!("L{i} {i} ")).collect();
    let markers = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><marker id="m">{marker}</marker><path d="M0 0 {path}" marker-mid="url(#m)"/></svg>"##
    );
    let err = svg::size(markers.as_bytes()).unwrap_err();
    assert!(err.contains("too many elements"), "{err}");
    // The same marker set by inheritance, through `use`, from a stylesheet or by a
    // marker's own ancestors on another marker's content.
    let svg = |defs: &str, body: &str| {
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><marker id="m">{marker}</marker>{defs}{body}</svg>"##
        )
    };
    let shape = format!(r#"<path id="p" d="M0 0 {path}"/>"#);
    for bomb in [
        svg("", &format!(r#"<g style="marker: url(#m)">{shape}</g>"#)),
        svg(
            "",
            &format!(r#"<g style="/**/marker: url(#m)">{shape}</g>"#),
        ),
        svg(
            &format!("<defs>{shape}</defs>"),
            r##"<use href="#p" marker-mid="url('#m')"/>"##,
        ),
        svg("<style>path { marker-mid: url(#m) }</style>", &shape),
        svg(
            r##"<g marker-mid="url(#m)"><marker id="n"><path d="M0 0 L1 1 L2 2 L3 3"/></marker></g>"##,
            &format!(
                r#"<polyline points="{}" marker-mid="url(#n)"/>"#,
                "1,1 ".repeat(300)
            ),
        ),
    ] {
        let err = svg::size(bomb.as_bytes()).unwrap_err();
        assert!(err.contains("too many elements"), "{err}");
    }
}

/// An Inkscape drawing: arrowhead markers, one used at the end of an arrow, and a traced
/// path of 30,000 points without markers. Only the arrow's end is copied.
#[test]
fn markers_count_only_where_they_are_used() {
    let arrow = |id: &str| {
        format!(
            r#"<marker style="overflow:visible" id="{id}" refX="0" refY="0" orient="auto"><path transform="scale(0.8)" style="fill-rule:evenodd;stroke:#000000;stroke-width:1pt" d="M 0.0,0.0 L 5.0,-5.0 L -12.5,0.0 L 5.0,5.0 z"/></marker>"#
        )
    };
    let traced: String = (0..30_000)
        .map(|i| format!("{}.{:03},{}.{:03} ", i % 1000, i % 997, i % 991, i % 983))
        .collect();
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="1000"><defs>{}{}{}</defs><path style="fill:none;stroke:#000000;marker-end:url(#Arrow1Lend)" d="m 10.5,10.5 h 200.25"/><path style="fill:#333333" d="M {traced}z"/></svg>"##,
        arrow("Arrow1Lstart"),
        arrow("Arrow1Lend"),
        arrow("Arrow2Lend"),
    );
    assert!(svg.len() > 450_000);
    assert_eq!(svg::size(svg.as_bytes()), Ok((1000, 1000)));
    // Start and end markers are one copy each, however long the path.
    let ends = svg.replace("marker-end:url(#Arrow1Lend)", "").replace(
        "fill:#333333",
        "marker-start:url(#Arrow1Lstart);marker-end:url(#Arrow2Lend)",
    );
    assert_eq!(svg::size(ends.as_bytes()), Ok((1000, 1000)));
    // A mid marker on it is a copy per vertex: 30,000 copies of two elements.
    let mid = svg.replace("fill:#333333", "marker-mid:url(#Arrow2Lend)");
    assert!(svg::size(mid.as_bytes()).is_ok());
    let mid = mid.replace(
        "orient=\"auto\">",
        "orient=\"auto\"><g/><g/><g/><g/><g/><g/><g/>",
    );
    assert!(
        svg::size(mid.as_bytes())
            .unwrap_err()
            .contains("too many elements")
    );
}

/// An SVG with `body` inside the root, 100 px square.
fn svg_with(body: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">{body}</svg>"#
    )
}

/// Checks `svg` fails quickly with an error mentioning `why`, on a worker's small stack.
fn refused_quickly(svg: String, why: &str) {
    let start = std::time::Instant::now();
    let err = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || svg::size(svg.as_bytes()))
        .unwrap()
        .join()
        .unwrap()
        .unwrap_err();
    assert!(err.contains(why), "{err}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

/// roxmltree recursed per nesting level and compared every attribute with the others
/// while parsing, before any check ran: 100,000 nested groups (300 KB) crashed even the
/// 64 MiB stack, and 100,000 attributes took 16 s. The markup is scanned first.
#[test]
fn svg_markup_is_checked_before_parsing() {
    refused_quickly(svg_with(&"<g>".repeat(100_000)), "deeper");
    // Quoted `>`, comments and CDATA do not hide the nesting.
    let hidden = r#"<g a=">"><!-- <g> --><![CDATA[<g>]]>"#.repeat(100_000);
    refused_quickly(svg_with(&hidden), "deeper");
    // Self-closing and closed elements do not nest.
    let flat = "<g/><g></g>".repeat(50_000);
    assert!(svg::size(svg_with(&flat).as_bytes()).is_ok());
    let attributes: String = (0..100_000).map(|i| format!(" a{i}=\"1\"")).collect();
    refused_quickly(svg_with(&format!("<rect{attributes}/>")), "attributes");
    // An entity whose value holds markup would nest unseen.
    let entity = format!(
        r#"<!DOCTYPE svg [<!ENTITY e "{}">]>{}"#,
        "<g>".repeat(10) + &"</g>".repeat(10),
        svg_with("&e;")
    );
    refused_quickly(entity, "entities");
    // A DTD without markup in its entities, as Illustrator writes, is fine.
    let illustrator = format!(
        r#"<?xml version="1.0"?><!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [<!ENTITY ns_svg "http://www.w3.org/2000/svg"><!-- it's fine -->]>{}"#,
        svg_with(r#"<rect width="10" height="10"/>"#)
    );
    assert_eq!(svg::size(illustrator.as_bytes()), Ok((100, 100)));
}

/// usvg breaks only references to themselves and pairs; longer cycles recursed until the
/// stack overflowed (each under 450 bytes).
#[test]
fn svg_reference_cycles_are_refused() {
    let three = |kind: &str, attr: &str| {
        let mut body: String = (1..=3)
            .map(|i| {
                format!(
                    r#"<{kind} id="r{i}"><rect width="9" height="9" {attr}="url(#r{})"/></{kind}>"#,
                    i % 3 + 1
                )
            })
            .collect();
        body.push_str(
            r#"<rect width="9" height="9" mask="url(#r1)" clip-path="url(#r1)" fill="url(#r1)"/>"#,
        );
        svg_with(&body)
    };
    for svg in [
        three("mask", "mask"),
        three("clipPath", "clip-path"),
        three("pattern", "fill"),
        // Two filters, each with an `feImage` of the other's element.
        svg_with(
            r##"<filter id="f1"><feImage href="#r2"/></filter><filter id="f2"><feImage href="#r1"/></filter><rect id="r1" width="9" height="9" filter="url(#f1)"/><rect id="r2" width="9" height="9" filter="url(#f2)"/>"##,
        ),
        // Through a stylesheet.
        svg_with(
            r#"<style>.a { mask: url(#m2) } .b { mask: url(#m1) }</style><mask id="m1"><rect class="a" width="9" height="9"/></mask><mask id="m2"><rect class="b" width="9" height="9"/></mask>"#,
        ),
        // Through `style` with comments, which usvg's tokenizer skips.
        svg_with(
            r#"<mask id="m1"><rect style="/**/mask:url(#m2)" width="9" height="9"/></mask><mask id="m2"><rect style="mask/*x*/:url(#m3)" width="9" height="9"/></mask><mask id="m3"><rect style="fill:red;/**/mask:url(#m1)" width="9" height="9"/></mask><rect mask="url(#m1)" width="9" height="9"/>"#,
        ),
        // A `use` of its own ancestor.
        svg_with(r##"<g id="g"><g><use href="#g"/></g></g>"##),
    ] {
        refused_quickly(svg, "cycle");
    }
    // Ordinary references: gradients inheriting from another, a clip path of a `use`, and
    // a stylesheet setting them (as Illustrator writes).
    let fine = svg_with(
        r##"<style>.st0 { clip-path: url(#c); fill: url(#g2) } .st1 { overflow: visible }</style><defs><linearGradient id="g1"><stop offset="0"/></linearGradient><linearGradient id="g2" xlink:href="#g1"/><rect id="shape" width="50" height="50"/><clipPath id="c"><use xlink:href="#shape" class="st1"/></clipPath></defs><rect class="st0" width="90" height="90"/>"##,
    );
    assert_eq!(svg::size(fine.as_bytes()), Ok((100, 100)));
}

/// usvg skips a marker inside itself, so an arrowhead path under a stylesheet that gives
/// every path the marker, or a marker on its own content, is no cycle.
#[test]
fn svg_markers_on_their_own_content_are_fine() {
    let arrow = r#"<marker id="arrow"><path d="M0 0 L5 5 L0 10 z"/></marker>"#;
    for svg in [
        svg_with(&format!(
            r#"<style>path {{ marker-end: url(#arrow) }}</style><defs>{arrow}</defs><path d="M10 10 L90 90"/>"#
        )),
        svg_with(
            r#"<marker id="m"><path d="M0 0 L5 5" marker-end="url(#m)"/></marker><path d="M10 10 L90 90" marker-end="url(#m)"/>"#,
        ),
    ] {
        assert_eq!(svg::size(svg.as_bytes()), Ok((100, 100)));
        assert!(svg::rasterize(svg.as_bytes(), 1.0, &svg::Fonts::empty()).is_ok());
    }
}

/// draw.io's "embed fonts" writes the font into the stylesheet; simplecss skips it in
/// linear time, so it does not count towards the size limit.
#[test]
fn svg_embedded_fonts_do_not_count_as_stylesheet() {
    let font = "A".repeat(200 << 10);
    let svg = svg_with(&format!(
        r#"<style>@font-face {{ font-family: "Embedded"; src: url(data:font/woff2;base64,{font}) format("woff2"); }} .t {{ fill: red }}</style><rect class="t" width="10" height="10"/>"#
    ));
    assert_eq!(svg::size(svg.as_bytes()), Ok((100, 100)));
    // A font in a rule's value is skipped too, but not ordinary rules around it.
    let value = svg_with(&format!(
        r#"<style>.t {{ src: url(data:font/woff2;base64,{font}) }}</style>"#
    ));
    assert_eq!(svg::size(value.as_bytes()), Ok((100, 100)));
    let rules: String = (0..20_000)
        .map(|i| format!(".c{i} {{ fill: red }}"))
        .collect();
    let mixed = svg_with(&format!(
        r#"<style>@font-face {{ src: url(data:,{font}) }} {rules}</style>"#
    ));
    refused_quickly(mixed, "stylesheet larger");
    // A large font still costs its length for each declaration after it.
    let rules: String = (0..3_000)
        .map(|i| format!(".c{i} {{ fill: red }}"))
        .collect();
    let after = svg_with(&format!(
        r#"<style>@font-face {{ src: url(data:,{}) }} {rules}</style>"#,
        "A".repeat(1 << 20)
    ));
    refused_quickly(after, "stylesheet larger");
}

/// Marker copies count once per `use` of the marked path: 200 uses of a path with 2,000
/// vertices and a 20-circle mid marker took 30 s and 5.4 GB.
#[test]
fn svg_marker_copies_multiply_with_use() {
    let marker = r#"<marker id="m">"#.to_owned() + &r#"<circle r="1"/>"#.repeat(20) + "</marker>";
    let path: String = (0..2000).map(|i| format!("L{i} {i} ")).collect();
    let uses = r##"<use href="#p"/>"##.repeat(200);
    let svg = svg_with(&format!(
        r#"<defs>{marker}<path id="p" d="M0 0 {path}" marker-mid="url(#m)"/></defs>{uses}"#
    ));
    refused_quickly(svg, "too many elements");
}

/// Each passed every other check and took seconds to minutes to convert or draw.
#[test]
fn svg_work_bombs_are_refused() {
    // Four `feImage`s per level of the next level's filtered group, four levels deep.
    let mut chain = String::new();
    for level in 0..4 {
        let next = format!(r##"<feImage href="#g{}"/>"##, level + 1).repeat(4);
        chain.push_str(&format!(
            r#"<filter id="f{level}">{next}<feGaussianBlur stdDeviation="3"/></filter><g id="g{level}" filter="url(#f{level})"><rect width="100" height="100"/></g>"#
        ));
    }
    chain.push_str(r#"<g id="g4"><rect width="100" height="100"/></g>"#);
    let start = std::time::Instant::now();
    let err = svg::rasterize(svg_with(&chain).as_bytes(), 1.0, &svg::Fonts::empty()).unwrap_err();
    assert!(err.contains("costly"), "{err}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    // One text element of 30,000 words, and 10,000 copies of 1,000 characters.
    let words = "word ".repeat(30_000);
    refused_quickly(svg_with(&format!("<text>{words}</text>")), "text element");
    let copies = r##"<text><tref xlink:href="#t"/></text>"##.repeat(10_000);
    let tref = format!(
        r#"<defs><text id="t">{}</text></defs>{copies}"#,
        "x".repeat(1000)
    );
    refused_quickly(svg_with(&tref), "too much text");
    let spans = r#"<tspan x="1">abcdefg</tspan>"#.repeat(2_800);
    refused_quickly(svg_with(&format!("<text>{spans}</text>")), "parts");
    refused_quickly(svg_with(&"<text>x</text>".repeat(140_000)), "text elements");
    // 20,000 stylesheet rules over 20,000 elements; parsing 400 KB of rules alone took
    // 2.4 s, and 1,000 rules for 17,000 elements are too many to match.
    let css = |rules: usize, elements: usize| {
        let rules: String = (0..rules)
            .map(|i| format!(".c{i} {{ fill: red }}"))
            .collect();
        let rects = r#"<rect width="1" height="1"/>"#.repeat(elements);
        svg_with(&format!("<style>{rules}</style>{rects}"))
    };
    refused_quickly(css(20_000, 20_000), "stylesheet larger");
    refused_quickly(css(1_000, 17_000), "stylesheet too costly");
    // Long descendant selectors over deep or many elements, declarations copied to every
    // element, and sibling selectors.
    let nested = |rule: &str, groups: usize| {
        let g = "<g>".repeat(groups) + &"</g>".repeat(groups);
        svg_with(&format!("<style>{rule}</style>{g}"))
    };
    refused_quickly(
        nested(&format!("{} {{ fill: red }}", "g ".repeat(30_000)), 1),
        "parts",
    );
    refused_quickly(
        nested(&format!("x {} {{ fill: red }}", "g ".repeat(5)), 120),
        "too costly",
    );
    let many = format!("g {{ {} }}", "fill: red; ".repeat(3_000));
    refused_quickly(
        svg_with(&format!("<style>{many}</style>{}", "<g/>".repeat(20_000))),
        "too costly",
    );
    let siblings: String = (0..100)
        .map(|_| "rect + rect + rect + rect { fill: red } ")
        .collect();
    let rects = r#"<rect width="1" height="1"/>"#.repeat(100_000);
    refused_quickly(
        svg_with(&format!("<style>{siblings}</style>{rects}")),
        "too costly",
    );
    // A pattern of a 1×1 tile scaled by 10,000 filling one scaled the same way (asked for
    // 40 PB), one scaled by 30,000, and 100 shapes filled with one scaled by 20,000.
    let scaled = |id: &str, scale: u32, fill: &str| {
        format!(
            r#"<pattern id="{id}" width="1" height="1" patternUnits="userSpaceOnUse" patternTransform="scale({scale})"><rect width="1" height="1" fill="{fill}"/></pattern>"#
        )
    };
    let nested = scaled("q", 10_000, "red") + &scaled("p", 10_000, "url(#q)");
    let rect = |fill: &str| format!(r#"<rect width="100" height="100" fill="{fill}"/>"#);
    for svg in [
        svg_with(&(nested + &rect("url(#p)"))),
        svg_with(&(scaled("p", 30_000, "red") + &rect("url(#p)"))),
        svg_with(&(scaled("p", 20_000, "red") + &rect("url(#p)").repeat(100))),
    ] {
        refused_quickly(svg, "pattern tiles too large");
    }
    // Four levels of patterns of 20 shapes filled with the level below (6.2 s).
    let mut levels = String::from(
        r#"<pattern id="l0" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern>"#,
    );
    for level in 1..5 {
        let shapes = format!(
            r#"<rect width="1" height="1" fill="url(#l{})"/>"#,
            level - 1
        )
        .repeat(20);
        levels.push_str(&format!(r#"<pattern id="l{level}" width="10" height="10" patternUnits="userSpaceOnUse">{shapes}</pattern>"#));
    }
    refused_quickly(svg_with(&(levels + &rect("url(#l4)"))), "too many elements");
    // 1,000 shapes filled with a small pattern of 1,000 shapes, refused before usvg
    // converts it (a pattern of 50,000 markers on 1,000 shapes took 1.7 s to convert).
    let busy = r#"<rect width="0.1" height="0.1"/>"#.repeat(1000);
    let busy = format!(
        r#"<pattern id="b" width="1" height="1" patternUnits="userSpaceOnUse">{busy}</pattern>"#
    );
    let inherited = format!(r#"<g fill="url(#b)">{}</g>"#, rect("inherit").repeat(1000));
    for svg in [
        busy.clone() + &rect("url(#b)").repeat(1000),
        busy + &inherited,
    ] {
        refused_quickly(svg_with(&svg), "too many elements");
    }
    // 20 nested filtered groups in a pattern.
    let groups = r#"<g filter="url(#f)">"#.repeat(20)
        + r#"<rect width="1" height="1"/>"#
        + &"</g>".repeat(20);
    let filtered = format!(
        r#"<filter id="f" filterUnits="userSpaceOnUse" x="-5" y="-5" width="20" height="20"><feGaussianBlur stdDeviation="1"/></filter><pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse">{groups}</pattern>"#
    );
    refused_quickly(
        svg_with(&(filtered + &rect("url(#p)"))),
        "layers in patterns",
    );
    // A hatch pattern on many shapes is fine.
    let hatch = r#"<pattern id="h" width="4" height="4" patternUnits="userSpaceOnUse"><path d="M0 0 L4 4" stroke="black"/></pattern>"#;
    let shapes = r#"<rect width="10" height="10" fill="url(#h)"/>"#.repeat(1000);
    let svg = svg_with(&format!("<defs>{hatch}</defs>{shapes}"));
    assert!(svg::rasterize(svg.as_bytes(), 1.0, &svg::Fonts::empty()).is_ok());
    // 1,000 uses of a 64-megapixel image.
    let png = base64(&png_header(8192, 8192));
    let uses = r##"<use href="#i"/>"##.repeat(1000);
    let images = format!(
        r#"<defs><image id="i" width="10" height="10" href="data:image/png;base64,{png}"/></defs>{uses}"#
    );
    let start = std::time::Instant::now();
    let err = svg::size(svg_with(&images).as_bytes()).unwrap_err();
    assert!(err.contains("image pixels"), "{err}");
    assert!(start.elapsed() < Duration::from_secs(2));
}

fn base64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = u32::from(c[0]) << 16
            | u32::from(*c.get(1).unwrap_or(&0)) << 8
            | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            out.push(if k <= c.len() {
                T[(n >> (18 - 6 * k) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Twelve nested blurred, translucent groups (631 bytes) took a second and 150 MB.
#[test]
fn costly_svg_layers_are_refused() {
    let blur = r#"<svg xmlns="http://www.w3.org/2000/svg" width="2000" height="2000"><filter id="b"><feGaussianBlur stdDeviation="5"/></filter>"#;
    let svg = nested_groups(12, r#"opacity="0.9" filter="url(#b)""#).replace(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="2000" height="2000">"#,
        blur,
    );
    let start = std::time::Instant::now();
    let err = svg::rasterize(svg.as_bytes(), 1.0, &svg::Fonts::empty()).unwrap_err();
    assert!(err.contains("costly"), "{err}");
    assert!(start.elapsed() < Duration::from_secs(1));
    // A few translucent groups are fine.
    let some = nested_groups(4, r#"opacity="0.9""#);
    assert!(svg::size(some.as_bytes()).is_ok());
    // So is a huge morphology radius, refused for its work rather than run.
    let morph = r#"<svg xmlns="http://www.w3.org/2000/svg" width="500" height="500"><filter id="m"><feMorphology radius="200"/></filter><rect width="500" height="500" filter="url(#m)"/></svg>"#;
    assert!(svg::size(morph.as_bytes()).unwrap_err().contains("costly"));
}

/// resvg decodes inline rasters itself, without the decoder limits, and a nested SVG
/// would skip the structure checks.
#[test]
fn svg_data_images_pass_the_raster_limits_and_never_nest() {
    use std::sync::Arc;
    let pixels = std::sync::atomic::AtomicU64::new(0);
    let resolve = svg::data_resolver(&pixels);
    let opts = resvg::usvg::Options::default();
    let ok = Arc::new(png(2, 2, [1, 2, 3, 255]));
    assert!(resolve("image/png", ok, &opts).is_some());
    let huge = Arc::new(png_header(20_000, 20_000));
    assert!(resolve("image/png", huge.clone(), &opts).is_none());
    assert!(resolve("text/plain", huge, &opts).is_none());
    let nested = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#;
    assert!(resolve("image/svg+xml", Arc::new(nested.to_vec()), &opts).is_none());
}

#[cfg(feature = "remote")]
mod remote_tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A one-thread HTTP server: `route(path)` gives (status line, extra headers, body).
    /// Returns the base URL and the raw request heads it received.
    fn serve(
        route: impl Fn(&str) -> (String, String, Vec<u8>) + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<String>>> = Default::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                log.lock().unwrap().push(head);
                let (status, headers, body) = route(&path);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n{headers}\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        (base, seen)
    }

    /// A proxy from the environment (a terminal launch) resolves the target itself, so it
    /// would reach this machine for a document: image fetches use none. The fetch runs in a
    /// child process (`fetch_ignores_the_proxy`) with the proxy variables set.
    #[test]
    fn proxy_settings_are_ignored() {
        let (proxy, seen) = serve(|_| ("200 OK".into(), String::new(), png(2, 2, [1, 1, 1, 255])));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::remote_tests::fetch_ignores_the_proxy"])
            .args(["--ignored", "--test-threads=1"])
            .env("ALL_PROXY", &proxy)
            .env("HTTP_PROXY", &proxy)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .status()
            .unwrap();
        assert!(status.success());
        assert!(seen.lock().unwrap().is_empty(), "the proxy was asked");
    }

    #[test]
    #[ignore = "run by proxy_settings_are_ignored"]
    fn fetch_ignores_the_proxy() {
        assert!(std::env::var("ALL_PROXY").is_ok());
        // Not resolvable: only a proxy could fetch it.
        assert!(remote::fetch("http://md-tex-proxy-check.invalid/a.png").is_err());
    }

    #[test]
    fn fetches_with_generic_headers_only() {
        let (base, seen) = serve(|_| ("200 OK".into(), String::new(), png(2, 2, [1, 1, 1, 255])));
        let bytes = remote::fetch_from(&format!("{base}/a.png"), false).unwrap();
        assert_eq!(probe_bytes(&bytes).unwrap(), (2, 2));
        let head = seen.lock().unwrap()[0].to_ascii_lowercase();
        assert!(head.starts_with("get /a.png http/1.1\r\n"), "{head}");
        assert!(head.contains(&format!(
            "user-agent: {}",
            remote::USER_AGENT.to_ascii_lowercase()
        )));
        for header in ["cookie", "referer", "authorization"] {
            assert!(
                !head.contains(&format!("{header}:")),
                "{header} sent: {head}"
            );
        }
        let names: Vec<&str> = head
            .lines()
            .skip(1)
            .filter_map(|l| l.split(':').next())
            .collect();
        assert!(
            names
                .iter()
                .all(|n| ["host", "user-agent", "accept"].contains(n)),
            "{names:?}"
        );
    }

    #[test]
    fn oversized_response_is_abandoned() {
        let big = vec![0u8; remote::MAX_BYTES as usize + 1];
        let (base, _) = serve(move |_| ("200 OK".into(), String::new(), big.clone()));
        assert_eq!(
            remote::fetch_from(&format!("{base}/big.png"), false),
            Err(ImageError::TooLarge)
        );
    }

    #[test]
    fn redirect_to_file_url_is_not_followed() {
        let (base, seen) = serve(|_| {
            (
                "302 Found".into(),
                "location: file:///etc/passwd\r\n".into(),
                Vec::new(),
            )
        });
        let err = remote::fetch_from(&format!("{base}/a.png"), false).unwrap_err();
        assert!(
            matches!(err, ImageError::Fetch(ref m) if m.contains("file")),
            "{err:?}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn follows_at_most_five_redirects() {
        let (base, seen) = serve(|path| {
            let n: usize = path.trim_start_matches('/').parse().unwrap_or(0);
            if n < 10 {
                (
                    "302 Found".into(),
                    format!("location: /{}\r\n", n + 1),
                    Vec::new(),
                )
            } else {
                ("200 OK".into(), String::new(), png(1, 1, [0, 0, 0, 255]))
            }
        });
        assert!(
            remote::fetch_from(&format!("{base}/5"), false).is_ok(),
            "5 redirects"
        );
        seen.lock().unwrap().clear();
        assert!(
            remote::fetch_from(&format!("{base}/4"), false).is_err(),
            "6 redirects"
        );
        assert_eq!(seen.lock().unwrap().len(), 6);
    }

    /// Each request of a redirect chain had the whole timeout to itself: a server
    /// answering each one just in time held the fetch (and its thread) six times as long.
    #[test]
    fn the_timeout_covers_the_whole_redirect_chain() {
        let (base, _) = serve(|path| {
            std::thread::sleep(std::time::Duration::from_millis(700));
            let n: usize = path.trim_start_matches('/').parse().unwrap_or(0);
            if n < 1 {
                let location = format!("location: /{}\r\n", n + 1);
                ("302 Found".into(), location, Vec::new())
            } else {
                ("200 OK".into(), String::new(), png(1, 1, [0, 0, 0, 255]))
            }
        });
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(1);
        let fetched = remote::fetch_within(&format!("{base}/0"), false, timeout);
        assert!(
            matches!(fetched, Err(ImageError::Fetch(ref m)) if m.contains("timeout")),
            "{fetched:?}"
        );
        assert!(start.elapsed() < timeout * 3 / 2, "{:?}", start.elapsed());
        // Within the time, the same chain is fine.
        let long = std::time::Duration::from_secs(10);
        assert!(remote::fetch_within(&format!("{base}/0"), false, long).is_ok());
    }

    /// A document could make the reader request pages on this machine or its network
    /// (a router's admin page) once remote images were allowed.
    #[test]
    fn local_addresses_are_refused() {
        let (base, seen) = serve(|_| ("200 OK".into(), String::new(), png(1, 1, [0; 4])));
        let port = base.rsplit(':').next().unwrap();
        for url in [
            format!("{base}/a.png"),
            format!("http://localhost:{port}/a.png"),
            format!("http://[::ffff:127.0.0.1]:{port}/a.png"),
        ] {
            let err = remote::fetch(&url).unwrap_err();
            assert!(
                matches!(err, ImageError::Fetch(ref m) if m.contains("public")),
                "{url}: {err:?}"
            );
        }
        assert!(seen.lock().unwrap().is_empty(), "no request was sent");
    }

    #[test]
    fn address_classifier() {
        let public = [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700::1111",
            "100.128.0.1",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ];
        let local = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12::1",
            "fec0::1",
            "ff02::1",
            "::ffff:192.168.0.1",
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "2002:c0a8:101::1",
            "2002:7f00:1::",
            "240.0.0.1",
            "198.18.0.1",
            "192.0.0.8",
        ];
        for ip in public {
            assert!(remote::is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in local {
            assert!(!remote::is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn only_http_and_https() {
        assert!(remote::fetch("ftp://example.com/a.png").is_err());
        assert!(remote::fetch("file:///etc/hosts").is_err());
    }
}
