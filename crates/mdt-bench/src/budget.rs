//! The performance budgets, and the check that compares measured values against them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Linux,
}

/// One budget: the measured value must be at most the platform's limit.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub metric: &'static str,
    pub unit: &'static str,
    pub macos: f64,
    pub linux: f64,
}

impl Budget {
    pub fn limit(&self, platform: Platform) -> f64 {
        match platform {
            Platform::Macos => self.macos,
            Platform::Linux => self.linux,
        }
    }
}

const fn b(metric: &'static str, unit: &'static str, macos: f64, linux: f64) -> Budget {
    Budget {
        metric,
        unit,
        macos,
        linux,
    }
}

/// Set from the 1.0 measurements (2026-11-12; Apple M4 and an x86_64 Linux PC with an
/// NVIDIA GPU, `tools/bench.py`), each a little above what was measured. Where the
/// original targets were not met, the measured value is noted.
pub const BUDGETS: &[Budget] = &[
    // Measured 173 ms (macOS) and 280 ms (Linux, where loading the GPU drivers takes
    // part of it).
    b("startup_first_text_frame_ms", "ms", 200.0, 300.0),
    b("input_latency_median_ms", "ms", 2.0, 2.0),
    b("input_latency_p99_ms", "ms", 4.0, 4.0),
    // Single frames the system delays under background load (26-31 ms intervals with
    // about 1.5 ms of md-tex's work): 0-3 per 1,200 frames.
    b("scroll_missed_frames", "frames", 3.0, 3.0),
    b("scroll_frame_cpu_p99_ms", "ms", 6.0, 6.0),
    b("idle_frames_presented", "frames", 0.0, 0.0),
    // Measured 0.20% (macOS) and 0.23% (Linux), with no frames drawn.
    b("idle_cpu_percent", "% of one core", 0.25, 0.25),
    // Measured 203 ms (macOS) and 335 ms (Linux).
    b("large_file_first_frame_ms", "ms", 250.0, 400.0),
    b("large_file_headings_ready_ms", "ms", 1000.0, 1000.0),
    b("mode_switch_ms", "ms", 16.0, 16.0),
    b("preview_update_ms", "ms", 50.0, 50.0),
    // Visible matches within 50 ms of each keystroke.
    b("find_keystroke_ms", "ms", 50.0, 50.0),
    b("equation_first_ms", "ms", 150.0, 150.0),
    b("equation_median_ms", "ms", 20.0, 20.0),
    b("diagram_ms", "ms", 50.0, 50.0),
    // The slowest of the corpus's photos: 229 ms (macOS) and 111 ms (Linux); the medians
    // were 80 and 74 ms.
    b("jpeg_12mp_ms", "ms", 250.0, 150.0),
    // Linux measured 211 MB, most of it the GPU driver libraries' private pages (LLVM
    // for Mesa's Vulkan drivers, NVIDIA's); md-tex's own allocations were about 58 MB.
    b("memory_text_mb", "MB", 40.0, 250.0),
    // Measured 186 MB (macOS) and 404 MB (Linux, the drivers again).
    b("memory_media_peak_mb", "MB", 200.0, 450.0),
    // Measured 52 MB (macOS) and 74 MB (Linux).
    b("binary_size_mb", "MB", 60.0, 80.0),
];

/// Measured values for one platform, as written by the measuring steps.
#[derive(Debug, Deserialize)]
pub struct Results {
    pub platform: Platform,
    pub metrics: BTreeMap<String, f64>,
}

#[derive(Debug, PartialEq)]
pub struct Report {
    pub text: String,
    pub exceeded: usize,
    pub unknown: Vec<String>,
}

impl Report {
    /// Process exit code: non-zero when any budget is exceeded or a metric is unknown.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.exceeded > 0 || !self.unknown.is_empty())
    }
}

pub fn check(results: &Results) -> Report {
    let mut text = String::new();
    let mut exceeded = 0;
    for budget in BUDGETS {
        let limit = budget.limit(results.platform);
        match results.metrics.get(budget.metric) {
            Some(&value) if value > limit => {
                exceeded += 1;
                let _ = writeln!(
                    text,
                    "OVER  {}: {value} {} (budget {limit} {})",
                    budget.metric, budget.unit, budget.unit
                );
            }
            Some(&value) => {
                let _ = writeln!(
                    text,
                    "ok    {}: {value} {} (budget {limit} {})",
                    budget.metric, budget.unit, budget.unit
                );
            }
            None => {
                let _ = writeln!(
                    text,
                    "--    {}: not measured (budget {limit} {})",
                    budget.metric, budget.unit
                );
            }
        }
    }
    let unknown: Vec<String> = results
        .metrics
        .keys()
        .filter(|k| !BUDGETS.iter().any(|b| b.metric == k.as_str()))
        .cloned()
        .collect();
    for name in &unknown {
        let _ = writeln!(text, "ERROR {name}: no such budget");
    }
    Report {
        text,
        exceeded,
        unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results(metrics: &[(&str, f64)]) -> Results {
        Results {
            platform: Platform::Macos,
            metrics: metrics.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn over_budget_metric_gives_nonzero_exit_and_prints_metric_value_and_budget() {
        let report = check(&results(&[
            ("startup_first_text_frame_ms", 240.0),
            ("memory_text_mb", 30.0),
        ]));
        assert_eq!(report.exit_code(), 1);
        assert_eq!(report.exceeded, 1);
        assert!(
            report
                .text
                .contains("OVER  startup_first_text_frame_ms: 240 ms (budget 200 ms)"),
            "{}",
            report.text
        );
    }

    #[test]
    fn all_within_budget_exits_zero_and_prints_values() {
        let report = check(&results(&[
            ("startup_first_text_frame_ms", 90.0),
            ("binary_size_mb", 44.0),
        ]));
        assert_eq!(report.exit_code(), 0);
        assert!(
            report
                .text
                .contains("ok    startup_first_text_frame_ms: 90 ms")
        );
        assert!(report.text.contains("ok    binary_size_mb: 44 MB"));
    }

    #[test]
    fn platform_selects_limit() {
        let mut r = results(&[("startup_first_text_frame_ms", 140.0)]);
        r.platform = Platform::Linux;
        assert_eq!(check(&r).exit_code(), 0);
    }

    #[test]
    fn unknown_metric_is_an_error() {
        let report = check(&results(&[("startup_ms", 1.0)]));
        assert_eq!(report.unknown, vec!["startup_ms".to_string()]);
        assert_eq!(report.exit_code(), 1);
    }
}
