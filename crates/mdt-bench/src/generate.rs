//! Deterministic generator for the standard benchmark documents:
//! 100 KB, 1 MB and 10 MB / 100,000-line text, plus the media-heavy corpus with 500 display
//! equations, 50 diagrams and 200 photos of 12 megapixels each.

use std::fmt::Write as _;
use std::io;
use std::path::Path;

/// xorshift64*: small, fast and stable across platforms and Rust versions.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `lo..hi`.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next_u64() % (hi - lo) as u64) as usize
    }

    pub fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.range(0, items.len())]
    }
}

const WORDS: &[&str] = &[
    "render", "glyph", "layout", "parser", "window", "buffer", "scroll", "anchor", "theme", "zoom",
    "math", "diagram", "image", "source", "preview", "cursor", "select", "search", "palette",
    "heading", "table", "list", "quote", "fence", "token", "line", "block", "inline", "frame",
    "budget", "measure", "cache", "the", "a", "of", "and", "to", "in", "is", "with", "for", "on",
    "each", "every", "fast", "small",
];

/// Paragraph lines wrap at this width. Chosen so the 10 MB document lands near 100,000 lines.
const WRAP: usize = 155;

pub enum Target {
    Bytes(usize),
    Lines(usize),
}

fn sentence(rng: &mut Rng, out: &mut String) {
    let n = rng.range(6, 16);
    for i in 0..n {
        let w = rng.pick(WORDS);
        match rng.range(0, 40) {
            0 => write!(out, "**{w}**").unwrap(),
            1 => write!(out, "*{w}*").unwrap(),
            2 => write!(out, "`{w}()`").unwrap(),
            3 => write!(out, "[{w}](https://example.com/{w})").unwrap(),
            4 => write!(out, "$x_{{{}}}^2$", rng.range(0, 9)).unwrap(),
            _ if i == 0 => {
                let mut c = w.chars();
                let first = c.next().unwrap().to_ascii_uppercase();
                write!(out, "{first}{}", c.as_str()).unwrap();
            }
            _ => out.push_str(w),
        }
        out.push(if i + 1 == n { '.' } else { ' ' });
    }
}

/// A paragraph hard-wrapped at `WRAP` columns, `lines` lines long.
fn paragraph(rng: &mut Rng, lines: usize) -> Vec<String> {
    let mut text = String::new();
    let mut result = Vec::new();
    let mut line = String::new();
    while result.len() < lines {
        text.clear();
        sentence(rng, &mut text);
        for word in text.split(' ') {
            if !line.is_empty() && line.len() + 1 + word.len() > WRAP {
                result.push(std::mem::take(&mut line));
                if result.len() == lines {
                    return result;
                }
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    result
}

fn block(rng: &mut Rng, index: usize) -> Vec<String> {
    match rng.range(0, 12) {
        0 => vec![format!(
            "{} Section {index}: {}",
            "#".repeat(rng.range(1, 4)),
            rng.pick(WORDS)
        )],
        1 | 2 => (0..rng.range(3, 8))
            .map(|i| {
                let mut s = format!(
                    "{}- ",
                    "  ".repeat(usize::from(i > 0 && rng.range(0, 3) == 0))
                );
                sentence(rng, &mut s);
                s.push(' ');
                sentence(rng, &mut s);
                s
            })
            .collect(),
        3 => {
            let mut v = vec!["```rust".to_string()];
            for i in 0..rng.range(4, 14) {
                let (var, func, arg) = (rng.pick(WORDS), rng.pick(WORDS), rng.range(0, 1000));
                v.push(format!("    let {var}_{i} = {func}({arg}).{func}_with({var}, {arg} * 2).expect(\"{var} {func}\");"));
            }
            v.push("```".into());
            v
        }
        4 => {
            let mut v = vec![
                "| Name | Value | Notes |".to_string(),
                "|:---|---:|:---:|".to_string(),
            ];
            for _ in 0..rng.range(3, 7) {
                let (name, value, note) = (rng.pick(WORDS), rng.range(0, 10_000), rng.pick(WORDS));
                v.push(format!("| {name} | {value} | {note} |"));
            }
            v
        }
        5 => {
            let n = rng.range(2, 4);
            paragraph(rng, n)
                .into_iter()
                .map(|l| format!("> {l}"))
                .collect()
        }
        _ => {
            let n = rng.range(3, 8);
            paragraph(rng, n)
        }
    }
}

/// A text document of the given size, deterministic for a given seed.
pub fn text_document(seed: u64, target: Target) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::from("# Benchmark document\n");
    let mut lines = 1;
    for index in 1.. {
        let blk = block(&mut rng, index);
        match target {
            Target::Bytes(n) if out.len() >= n => break,
            Target::Lines(n) if lines + 1 + blk.len() > n => {
                // Fill the remainder exactly with one paragraph.
                let remaining = n - lines;
                if remaining >= 2 {
                    out.push('\n');
                    for l in paragraph(&mut rng, remaining - 1) {
                        out.push_str(&l);
                        out.push('\n');
                    }
                } else if remaining == 1 {
                    out.push('\n');
                }
                break;
            }
            _ => {}
        }
        out.push('\n');
        lines += 1;
        for l in blk {
            out.push_str(&l);
            out.push('\n');
            lines += 1;
        }
    }
    out
}

const EQUATIONS: &[&str] = &[
    r"\sum_{i=1}^{n} i^{K} = \frac{n^{K+1}}{K+1} + O(n^{K})",
    r"\int_0^{K} e^{-x^2}\,dx \approx \frac{\sqrt{\pi}}{2}",
    r"\begin{pmatrix} K & 1 \\ 0 & K \end{pmatrix}^{-1} = \frac{1}{K^2}\begin{pmatrix} K & -1 \\ 0 & K \end{pmatrix}",
    r"\lim_{x \to K} \frac{\sin(x - K)}{x - K} = 1",
    r"f(x) = \begin{cases} x^{K} & x \ge 0 \\ -x & x < 0 \end{cases}",
    r"\nabla \cdot \mathbf{E} = \frac{\rho_{K}}{\varepsilon_0}",
];

fn equation(i: usize) -> String {
    EQUATIONS[i % EQUATIONS.len()].replace('K', &(i % 97 + 2).to_string())
}

fn diagram(i: usize) -> String {
    if i.is_multiple_of(2) {
        let mut s = String::from("```mermaid\nflowchart TD\n");
        for n in 0..19 {
            writeln!(s, "  N{i}_{n}[Step {n}] --> N{i}_{}[Step {}]", n + 1, n + 1).unwrap();
        }
        s.push_str("```\n");
        s
    } else {
        format!(
            "```pikchr\nbox \"Diagram {i}\" fit\narrow\nellipse \"next\" fit\narrow\ncylinder \"store\" fit\n```\n"
        )
    }
}

pub const PHOTO_COUNT: usize = 200;
pub const PHOTO_SIZE: (u32, u32) = (4000, 3000);

/// The media-heavy Markdown document. Photos are referenced as `img/photo-NNN.jpg`.
pub fn media_document(seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::from("# Media-heavy benchmark\n");
    let (mut eq, mut dg, mut ph) = (0, 0, 0);
    let mut section = 0;
    while eq < 500 || dg < 50 || ph < PHOTO_COUNT {
        section += 1;
        writeln!(out, "\n## Part {section}\n").unwrap();
        for l in paragraph(&mut rng, 3) {
            out.push_str(&l);
            out.push('\n');
        }
        for _ in 0..10 {
            if eq < 500 {
                writeln!(out, "\n$$\n{}\n$$", equation(eq)).unwrap();
                eq += 1;
            }
        }
        if dg < 50 {
            writeln!(out).unwrap();
            out.push_str(&diagram(dg));
            dg += 1;
        }
        for _ in 0..4 {
            if ph < PHOTO_COUNT {
                writeln!(out, "\n![Photo {ph}](img/photo-{ph:03}.jpg)").unwrap();
                ph += 1;
            }
        }
    }
    out
}

/// A deterministic 12-megapixel photo-like image: smooth gradients plus noise, so JPEG size
/// and decode cost resemble a real photo rather than a flat fill.
pub fn photo(index: usize) -> image::RgbImage {
    let (w, h) = PHOTO_SIZE;
    let mut rng = Rng::new(0x5EED_0000 + index as u64);
    let phase = (index % 7) as f32;
    image::RgbImage::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        let noise = (rng.next_u64() & 0x1F) as f32;
        let r = 120.0 + 100.0 * (fx * 6.0 + phase).sin() + noise;
        let g = 110.0 + 90.0 * (fy * 5.0 - phase).cos() + noise;
        let b = 100.0 + 80.0 * ((fx + fy) * 4.0).sin() + noise;
        image::Rgb([
            r.clamp(0.0, 255.0) as u8,
            g.clamp(0.0, 255.0) as u8,
            b.clamp(0.0, 255.0) as u8,
        ])
    })
}

/// The 1 MB text document.
pub fn text_1m() -> String {
    text_document(2, Target::Bytes(1_000_000))
}

/// The 10 MB, 100,000-line text document.
pub fn text_10m() -> String {
    text_document(3, Target::Lines(100_000))
}

/// Writes all standard documents under `dir`. Existing photos are kept, since they are
/// deterministic and slow to encode.
pub fn write_all(dir: &Path, photos: bool) -> io::Result<()> {
    std::fs::create_dir_all(dir.join("media/img"))?;
    std::fs::write(
        dir.join("text-100k.md"),
        text_document(1, Target::Bytes(100_000)),
    )?;
    std::fs::write(dir.join("text-1m.md"), text_1m())?;
    std::fs::write(dir.join("text-10m.md"), text_10m())?;
    std::fs::write(dir.join("media/media-heavy.md"), media_document(4))?;
    if photos {
        for i in 0..PHOTO_COUNT {
            let path = dir.join(format!("media/img/photo-{i:03}.jpg"));
            if !path.exists() {
                let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
                image::codecs::jpeg::JpegEncoder::new_with_quality(file, 85)
                    .encode_image(&photo(i))
                    .map_err(io::Error::other)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        assert_eq!(
            text_document(7, Target::Bytes(50_000)),
            text_document(7, Target::Bytes(50_000))
        );
        assert_eq!(media_document(4), media_document(4));
    }

    #[test]
    fn byte_targets_are_met() {
        for n in [100_000, 1_000_000] {
            let len = text_document(1, Target::Bytes(n)).len();
            assert!(len >= n && len < n + 4_000, "{n}: {len}");
        }
    }

    #[test]
    fn ten_megabyte_document_has_exactly_100k_lines_and_about_10_mb() {
        let doc = text_document(3, Target::Lines(100_000));
        assert_eq!(doc.lines().count(), 100_000);
        let mb = doc.len() as f64 / 1e6;
        assert!((9.5..=10.5).contains(&mb), "{mb} MB");
    }

    #[test]
    fn media_document_counts() {
        let doc = media_document(4);
        assert_eq!(doc.lines().filter(|l| *l == "$$").count(), 2 * 500);
        assert_eq!(
            doc.matches("```mermaid").count() + doc.matches("```pikchr").count(),
            50
        );
        assert_eq!(doc.matches("](img/photo-").count(), 200);
    }

    #[test]
    #[ignore = "encodes a 12 MP JPEG; run with --release --ignored"]
    fn photo_is_twelve_megapixels_and_encodes() {
        let img = photo(0);
        assert_eq!(img.width() * img.height(), 12_000_000);
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
            .encode_image(&img)
            .unwrap();
        assert!(jpeg.len() > 500_000, "{} bytes", jpeg.len());
        assert_eq!(img, photo(0));
    }
}
