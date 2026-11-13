//! `mdt-bench generate <dir> [--no-photos]` writes the standard benchmark documents.
//! `mdt-bench check <results.json>` compares measured values with the budgets and exits
//! non-zero when any budget is exceeded.
//! `mdt-bench budgets` prints the budget table.
//! `mdt-bench parse` prints parse throughput for the 1 MB and 10 MB documents.

use std::path::Path;
use std::process::ExitCode;

use mdt_bench::budget::{self, BUDGETS};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("generate") if args.len() >= 2 => {
            let photos = !args.iter().any(|a| a == "--no-photos");
            if let Err(e) = mdt_bench::generate::write_all(Path::new(&args[1]), photos) {
                eprintln!("generate failed: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("check") if args.len() == 2 => {
            let results = match std::fs::read_to_string(&args[1])
                .map_err(|e| e.to_string())
                .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("cannot read results {}: {e}", args[1]);
                    return ExitCode::FAILURE;
                }
            };
            let report = budget::check(&results);
            print!("{}", report.text);
            ExitCode::from(report.exit_code() as u8)
        }
        Some("parse") => {
            for r in mdt_bench::parse::run() {
                println!(
                    "{:10} {:>10} bytes {:>7} units {:>9.2} ms {:>8.1} MB/s",
                    r.name,
                    r.bytes,
                    r.units,
                    r.median.as_secs_f64() * 1e3,
                    r.mb_per_s()
                );
            }
            ExitCode::SUCCESS
        }
        Some("budgets") => {
            for b in BUDGETS {
                println!(
                    "{:32} macOS {:>7} {:<14} Linux {:>7} {}",
                    b.metric, b.macos, b.unit, b.linux, b.unit
                );
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "usage: mdt-bench generate <dir> [--no-photos] | check <results.json> | budgets | parse"
            );
            ExitCode::from(2)
        }
    }
}
