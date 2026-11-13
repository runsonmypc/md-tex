#![cfg_attr(
    not(any(feature = "pikchr", feature = "mermaid", feature = "svg")),
    allow(dead_code)
)]

use super::*;

/// Rasterizes at 1x with the bundled body font.
fn raster(svg: &str) -> mdt_image::Raster {
    let font: &'static [u8] = include_bytes!("../../mdt-text/fonts/Inter-Regular.ttf");
    let fonts = mdt_image::svg::Fonts::new(&[font], "Inter", "Inter");
    mdt_image::svg::rasterize(svg.as_bytes(), 1.0, &fonts).unwrap()
}

fn ink(r: &mdt_image::Raster) -> usize {
    r.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 0)
        .count()
}

#[cfg(feature = "pikchr")]
#[test]
fn pikchr_valid_and_invalid() {
    let svg = to_svg(Engine::Pikchr, r#"box "A"; arrow; box "B""#, false).unwrap();
    assert_eq!(svg.matches("<text").count(), 2, "two labels");
    assert!(svg.contains(">A</text>") && svg.contains(">B</text>"));
    assert!(svg.contains("<polygon"), "an arrowhead");
    let r = raster(&svg);
    assert!(r.width > 100 && ink(&r) > 0);

    let err = to_svg(Engine::Pikchr, "box \"A\"; arrow; frobnicate", false).unwrap_err();
    assert!(
        !err.message.contains('^'),
        "only the message: {}",
        err.message
    );
    assert!(err.message.contains("syntax error"), "{}", err.message);
}

#[cfg(feature = "pikchr")]
#[test]
fn pikchr_dark_mode_and_input_cap() {
    let light = to_svg(Engine::Pikchr, "box", false).unwrap();
    let dark = to_svg(Engine::Pikchr, "box", true).unwrap();
    assert!(light.contains("rgb(0,0,0)"), "{light}");
    assert_ne!(light, dark, "dark mode changes the colours");
    let big = "box\n".repeat(PIKCHR_MAX_INPUT / 4 + 1);
    let err = to_svg(Engine::Pikchr, &big, false).unwrap_err();
    assert!(err.message.contains("64 KiB"), "{}", err.message);
}

#[cfg(feature = "mermaid")]
#[test]
fn mermaid_types_render() {
    set_mermaid_font(include_bytes!("../../mdt-text/fonts/Inter-Regular.ttf"));
    let cases = [
        ("flowchart", "flowchart LR; A-->B"),
        (
            "sequence",
            "sequenceDiagram\n  Alice->>Bob: Hi\n  Bob-->>Alice: Hello",
        ),
        (
            "class",
            "classDiagram\n  Animal <|-- Duck\n  class Duck {\n    +swim()\n  }",
        ),
        (
            "state",
            "stateDiagram-v2\n  [*] --> Still\n  Still --> Moving\n  Moving --> [*]",
        ),
        ("er", "erDiagram\n  CUSTOMER ||--o{ ORDER : places"),
        (
            "gantt",
            "gantt\n  title Plan\n  dateFormat YYYY-MM-DD\n  section A\n  Task :a1, 2026-01-01, 3d",
        ),
        ("pie", "pie title Pets\n  \"Dogs\" : 386\n  \"Cats\" : 85"),
        ("mindmap", "mindmap\n  root((Root))\n    A\n    B"),
    ];
    for (name, source) in cases {
        for dark in [false, true] {
            let svg = to_svg(Engine::Mermaid, source, dark)
                .unwrap_or_else(|e| panic!("{name}: {}", e.message));
            assert!(svg.contains("<svg"), "{name}");
            assert!(ink(&raster(&svg)) > 0, "{name} draws something");
        }
    }
    let flow = to_svg(Engine::Mermaid, "flowchart LR; A-->B", false).unwrap();
    assert!(
        flow.contains(">A<") && flow.contains(">B<"),
        "nodes A and B"
    );
}

#[cfg(feature = "mermaid")]
#[test]
fn unsupported_mermaid_type_is_named() {
    let err = to_svg(Engine::Mermaid, "zenumlDiagram\n  A->B", false).unwrap_err();
    assert_eq!(
        err.message,
        "unsupported Mermaid diagram type: zenumlDiagram"
    );
}

/// Layout time grows steeply with edges: 30 nodes and 100 random edges took 3 s, 200 nodes
/// and 400 edges 21 s; 5,000 nested parentheses took 386 MB. Each is refused before layout.
#[cfg(feature = "mermaid")]
#[test]
fn oversized_mermaid_is_refused_quickly() {
    let mut edges = String::from("flowchart TD\n");
    let mut x = 88_172_645_463_325_252u64;
    for _ in 0..MERMAID_MAX_EDGES + 1 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        edges.push_str(&format!("  N{} --> N{}\n", x % 30, (x >> 32) % 30));
    }
    let chain: String = (0..MERMAID_MAX_EDGES * 2)
        .map(|i| format!("  C{i} --> C{}\n", i + 1))
        .collect();
    let nodes: String = (0..MERMAID_MAX_NODES + 1)
        .map(|i| format!("  N{i}\n"))
        .collect();
    let cases = [
        (edges, "too large"),
        (format!("flowchart TD\n{chain}"), "too large"),
        (format!("flowchart TD\n{nodes}"), "too large"),
        (
            format!("flowchart TD\n  a{}{}", "(".repeat(5000), ")".repeat(5000)),
            "nest",
        ),
        (
            format!("flowchart TD\n{}", "  a --> b\n".repeat(8000)),
            "larger than",
        ),
    ];
    for (source, why) in cases {
        let start = std::time::Instant::now();
        let err = to_svg(Engine::Mermaid, &source, false).unwrap_err();
        assert!(err.message.contains(why), "{}", err.message);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
    // Sequence diagrams lay out quickly, so they may have more messages than edges.
    let messages: String = (0..100).map(|i| format!("  A->>B: m{i}\n")).collect();
    assert!(
        to_svg(
            Engine::Mermaid,
            &format!("sequenceDiagram\n{messages}"),
            false
        )
        .is_ok()
    );
}

/// Fences render on background workers with 512 KiB stacks, where an overflow aborts the
/// app: 150 nested C4 boundaries (3 KB) overflowed one in the Mermaid renderer. Engines
/// run on a stack of their own; the deepest nesting that fits in the input cap renders.
#[cfg(feature = "mermaid")]
#[test]
fn deeply_nested_mermaid_does_not_overflow_a_worker() {
    let nested = |open: &dyn Fn(usize) -> String, inner: &str, close: &str| {
        let mut source = String::new();
        let mut n = 0;
        while source.len() + inner.len() + (n + 1) * (open(n).len() + close.len())
            < MERMAID_MAX_INPUT
        {
            n += 1;
            source.push_str(&open(n));
        }
        source + inner + &close.repeat(n)
    };
    let cases = [
        nested(
            &|i| format!("Boundary(b{i}, \"B\") {{\n"),
            "Person(p, \"P\")\n",
            "}\n",
        ),
        nested(&|i| format!("subgraph s{i}\n"), "a-->b\n", "end\n"),
        nested(&|i| format!("state S{i} {{\n"), "[*] --> x\n", "}\n"),
        nested(&|i| format!("namespace N{i} {{\n"), "class A\n", "}\n"),
    ];
    for (header, body) in [
        "C4Context",
        "flowchart TD",
        "stateDiagram-v2",
        "classDiagram",
    ]
    .into_iter()
    .zip(cases)
    {
        let source = format!("{header}\n{body}");
        let result = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || to_svg(Engine::Mermaid, &source, false))
            .unwrap()
            .join()
            .unwrap();
        assert!(result.is_ok(), "{header}: {:?}", result.err());
    }
}

/// A git graph's commits and branches count as nodes: 3,000 branches of a commit each
/// took 1.6 s to lay out and 18 s and 1.2 GB to draw.
#[cfg(feature = "mermaid")]
#[test]
fn git_graph_commits_and_branches_count_as_nodes() {
    let branches: String = (0..300)
        .map(|i| format!("  branch b{i}\n  commit\n"))
        .collect();
    let err = to_svg(Engine::Mermaid, &format!("gitGraph\n{branches}"), false).unwrap_err();
    assert!(err.message.contains("too large"), "{}", err.message);
    let commits = "  commit\n".repeat(50);
    assert!(
        to_svg(
            Engine::Mermaid,
            &format!("gitGraph\n{commits}  branch b\n  commit\n"),
            false
        )
        .is_ok()
    );
}

/// Only the layered layout (flowchart, class, state, ER, requirement) is slow with many
/// edges. A mindmap of 200 nodes (199 edges) laid out in 3 ms in release, and a class
/// diagram with as many edges is still refused.
#[cfg(feature = "mermaid")]
#[test]
fn edge_limit_applies_to_layered_layouts_only() {
    let n = MERMAID_MAX_EDGES + 50;
    let mut mindmap = String::from("mindmap\n  root\n");
    for i in 1..n {
        mindmap.push_str(&format!("{}n{i}\n", "  ".repeat(2 + i % 3)));
    }
    assert!(to_svg(Engine::Mermaid, &mindmap, false).is_ok());
    let class: String = (0..n).map(|i| format!("  C{i} --> C{}\n", i + 1)).collect();
    let err = to_svg(Engine::Mermaid, &format!("classDiagram\n{class}"), false).unwrap_err();
    assert!(err.message.contains("edges"), "{}", err.message);
    let nodes: String = (0..MERMAID_MAX_NODES + 1)
        .map(|i| format!("  n{i}\n"))
        .collect();
    let err = to_svg(Engine::Mermaid, &format!("mindmap\n  root\n{nodes}"), false).unwrap_err();
    assert!(err.message.contains("the limit is"), "{}", err.message);
}

#[cfg(feature = "svg")]
#[test]
fn svg_fence_keeps_its_colours() {
    let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><circle cx="10" cy="10" r="9" fill="red"/></svg>"#;
    for dark in [false, true] {
        let r = raster(&to_svg(Engine::Svg, source, dark).unwrap());
        let center = ((10 * 20 + 10) * 4) as usize;
        assert_eq!(&r.rgba[center..center + 4], &[255, 0, 0, 255]);
    }
}

#[test]
fn engine_panic_becomes_an_error() {
    let err = guarded::<String>(|| panic!("boom")).unwrap_err();
    assert!(err.message.contains("boom"), "{}", err.message);
}

#[test]
fn engines_follow_their_features() {
    assert_eq!(
        Engine::from_lang("pikchr").is_some(),
        cfg!(feature = "pikchr")
    );
    assert_eq!(
        Engine::from_lang("mermaid").is_some(),
        cfg!(feature = "mermaid")
    );
    assert_eq!(Engine::from_lang("svg").is_some(), cfg!(feature = "svg"));
    assert_eq!(Engine::from_lang("rust"), None);
}
