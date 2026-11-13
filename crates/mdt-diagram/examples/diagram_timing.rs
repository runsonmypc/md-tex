//! Times the benchmark diagrams (a 20-node Mermaid flowchart and a Pikchr diagram):
//! `cargo run --release -p mdt-diagram --example diagram_timing`.

use std::time::Instant;

fn main() {
    let mut mermaid = String::from("flowchart TD\n");
    for n in 0..19 {
        mermaid.push_str(&format!(
            "  N_{n}[Step {n}] --> N_{}[Step {}]\n",
            n + 1,
            n + 1
        ));
    }
    let pikchr =
        "box \"Diagram\" fit\narrow\nellipse \"next\" fit\narrow\ncylinder \"store\" fit\n";
    mdt_diagram::set_mermaid_font(include_bytes!("../../mdt-text/fonts/Inter-Regular.ttf"));
    for (lang, src) in [("mermaid", mermaid.as_str()), ("pikchr", pikchr)] {
        let kind = mdt_diagram::Engine::from_lang(lang).unwrap();
        let runs = std::env::var("RUNS")
            .ok()
            .and_then(|r| r.parse().ok())
            .unwrap_or(4);
        for i in 0..runs {
            let t = Instant::now();
            let svg = mdt_diagram::to_svg(kind, src, false).unwrap();
            println!("{lang} run {i}: {:?} ({} bytes)", t.elapsed(), svg.len());
        }
    }
}
