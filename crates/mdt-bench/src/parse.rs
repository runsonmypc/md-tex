//! Parse throughput: full block-model builds of the 1 MB and 10 MB documents.

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

pub fn run() -> Vec<ParseResult> {
    vec![
        measure("parse_1m", &generate::text_1m(), 11),
        measure("parse_10m", &generate::text_10m(), 5),
    ]
}
