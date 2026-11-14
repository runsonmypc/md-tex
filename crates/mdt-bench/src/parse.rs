//! Parse throughput: full block-model builds of the 1 MB and 10 MB documents, and an
//! incremental reparse of each after a keystroke.

use std::time::{Duration, Instant};

use crate::generate;

pub struct ParseResult {
    pub name: &'static str,
    pub bytes: usize,
    pub units: usize,
    pub median: Duration,
}

impl ParseResult {
    pub fn mb_per_s(&self) -> f64 {
        self.bytes as f64 / 1e6 / self.median.as_secs_f64()
    }
}

/// Median of `runs` parses after one warm-up parse.
pub fn measure(name: &'static str, source: &str, runs: usize) -> ParseResult {
    let units = mdt_markdown::parse(source).units.len();
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(mdt_markdown::parse(std::hint::black_box(source)));
            t.elapsed()
        })
        .collect();
    times.sort_unstable();
    ParseResult {
        name,
        bytes: source.len(),
        units,
        median: times[times.len() / 2],
    }
}

/// Median of `runs` incremental reparses after typing one character at the start of the
/// line in the middle of `source`, applied to the model. `units` is the number reparsed.
pub fn measure_reparse(name: &'static str, source: &str, runs: usize) -> ParseResult {
    use mdt_markdown::incremental::{Change, reparse};
    let model = mdt_markdown::parse(source);
    // On bytes: the middle may fall inside a character, and a newline never does.
    let at = source.as_bytes()[..source.len() / 2]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let mut edited = source.to_owned();
    edited.insert(at, 'x');
    let change = Change {
        start: at,
        old_end: at,
        new_end: at + 1,
    };
    let mut units = 0;
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let mut m = model.clone();
            let t = Instant::now();
            let patch = reparse(&m, source, edited.as_str(), change, 64 * 1024)
                .expect("the edit reparses on its own");
            units = patch.units.len();
            std::hint::black_box(m.apply(patch));
            t.elapsed()
        })
        .collect();
    times.sort_unstable();
    ParseResult {
        name,
        bytes: source.len(),
        units,
        median: times[times.len() / 2],
    }
}

pub fn run() -> Vec<ParseResult> {
    let (text_1m, text_10m) = (generate::text_1m(), generate::text_10m());
    vec![
        measure("parse_1m", &text_1m, 11),
        measure("parse_10m", &text_10m, 5),
        measure_reparse("reparse_1m", &text_1m, 11),
        measure_reparse("reparse_10m", &text_10m, 5),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn reparse_measures_text_whose_middle_falls_inside_a_character() {
        // 11 bytes: the middle, byte 5, is inside the first `é`.
        let r = super::measure_reparse("t", "a\n\nx\u{e9}\u{e9}\u{e9}\n", 1);
        assert_eq!(r.bytes, 11);
    }
}
