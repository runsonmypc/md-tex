//! Times find on a document, a keystroke at a time, and the palette on 10,000 headings:
//! `cargo run --release -p mdt-app --example search_timing -- bench-docs/text-10m.md`.
use std::time::{Duration, Instant};

fn main() {
    let path = std::env::args().nth(1).expect("path to a document");
    let src = std::fs::read_to_string(path).unwrap();
    let rope = ropey::Rope::from_str(&src);
    let model = mdt_markdown::parse(&src);
    for rendered in [false, true] {
        let mut f = mdt_app::find::Find::new("", false, rendered, 0);
        let mut worst = Duration::ZERO;
        for c in "latency".chars() {
            f.query.insert(&c.to_string());
            let t = Instant::now();
            f.refresh(&rope, &model, 0, 0, 0);
            worst = worst.max(t.elapsed());
        }
        let kind = if rendered { "rendered" } else { "source" };
        println!("find ({kind}): worst keystroke {worst:?}");
    }

    let headings: String = (0..10_000)
        .map(|i| format!("## Section {i} on latency budgets and results\n\n"))
        .collect();
    let headings = mdt_markdown::parse(&headings).headings;
    let mut p = mdt_app::palette::Palette::new(&headings, Vec::new(), "");
    let mut worst = Duration::ZERO;
    for c in "#results 77".chars() {
        p.query.insert(&c.to_string());
        let t = Instant::now();
        p.update();
        worst = worst.max(t.elapsed());
    }
    println!(
        "palette (10,000 headings): worst keystroke {worst:?}, {} rows",
        p.rows.len()
    );
}
