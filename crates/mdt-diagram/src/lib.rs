//! Pikchr, Mermaid and SVG fences to SVG. Rasterizing goes through
//! `mdt_image::svg`, the safe rasterizer. Every engine call is guarded with `catch_unwind`.

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Pikchr input larger than this is refused before it reaches the C parser.
pub const PIKCHR_MAX_INPUT: usize = 64 * 1024;

/// Mermaid limits. Layout runs in the background, so a large diagram only takes longer to
/// appear, but its time grows with edges far faster than with nodes (measured in release on
/// an Apple M4): random flowcharts took 0.4–1.1 s at 64 edges, 0.9–3.1 s at 100 and 6 s at
/// 50 nodes and 100 edges; 3,000 unconnected nodes took 54 ms. The caps admit real
/// architecture diagrams and refuse documents built to stall the renderer. The edge cap
/// is for the layered kinds (flowchart, class, state, ER, requirement); the others place
/// edges directly (a 500-node mindmap took 3 ms, a sankey with 5,454 links 14 ms, C4 with
/// 64 KiB of relations 113 ms). Sequence diagrams lay out messages in rows: 400 took about
/// 0.5 s.
pub const MERMAID_MAX_INPUT: usize = 64 * 1024;
pub const MERMAID_MAX_NODES: usize = 500;
pub const MERMAID_MAX_EDGES: usize = 150;
pub const MERMAID_MAX_MESSAGES: usize = 400;
/// Deepest bracket nesting on one line. The parser's memory grows with the square of it:
/// a node with 5,000 nested parentheses took 386 MB.
pub const MERMAID_MAX_NESTING: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    Pikchr,
    Mermaid,
    Svg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagramError {
    pub message: String,
}

impl DiagramError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Engine {
    /// The engine for a fence's language, if it is built in. Without one, the fence is an
    /// ordinary code block.
    pub fn from_lang(lang: &str) -> Option<Self> {
        match lang {
            #[cfg(feature = "pikchr")]
            "pikchr" => Some(Engine::Pikchr),
            #[cfg(feature = "mermaid")]
            "mermaid" => Some(Engine::Mermaid),
            #[cfg(feature = "svg")]
            "svg" => Some(Engine::Svg),
            _ => None,
        }
    }

    /// Whether the theme changes the output (SVG fences keep their authored colours).
    pub fn themed(self) -> bool {
        self != Engine::Svg
    }
}

/// Runs `f`, turning a panic into an error, so a fault in an engine affects only its
/// block.
pub fn guarded<T>(f: impl FnOnce() -> Result<T, DiagramError>) -> Result<T, DiagramError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|panic| {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown fault".into());
        Err(DiagramError::new(format!(
            "the diagram engine failed: {what}"
        )))
    })
}

/// Stack for one engine call; pages are committed only as they are touched. Fences render
/// on background workers with 512 KiB stacks, where an overflow aborts the app, and the
/// Mermaid renderer recurses once per nesting level of some blocks (150 nested C4
/// boundaries, 3 KB, overflowed a worker).
const STACK: usize = 64 << 20;

/// Converts a fence to SVG for the light or dark theme.
pub fn to_svg(engine: Engine, source: &str, dark: bool) -> Result<String, DiagramError> {
    stacker::grow(STACK, || {
        guarded(|| match engine {
            Engine::Pikchr => pikchr(source, dark),
            Engine::Mermaid => mermaid(source, dark),
            Engine::Svg => Ok(source.to_owned()),
        })
    })
}

#[cfg(feature = "pikchr")]
fn pikchr(source: &str, dark: bool) -> Result<String, DiagramError> {
    if source.len() > PIKCHR_MAX_INPUT {
        return Err(DiagramError::new(format!(
            "Pikchr input is larger than {} KiB",
            PIKCHR_MAX_INPUT / 1024
        )));
    }
    if source.contains('\0') {
        return Err(DiagramError::new("Pikchr input contains a NUL character"));
    }
    let mut flags = pikchr::PikchrFlags::default();
    flags.generate_plain_errors();
    if dark {
        flags.use_dark_mode();
    }
    match pikchr::Pikchr::render(source, None, flags) {
        Ok(p) if p.rendered().contains("<svg") => Ok(p.rendered().to_owned()),
        Ok(_) => Err(DiagramError::new("empty Pikchr diagram")),
        Err(text) => Err(DiagramError::new(pikchr_error(&text))),
    }
}

/// Pikchr's plain-text errors quote the source with a caret line, then `ERROR: …`; the
/// source is already on screen, so only the message lines are kept.
#[cfg(feature = "pikchr")]
fn pikchr_error(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("ERROR:"))
        .map(str::trim)
        .collect();
    if lines.is_empty() {
        text.trim().to_owned()
    } else {
        lines.join("; ")
    }
}

#[cfg(not(feature = "pikchr"))]
fn pikchr(_: &str, _: bool) -> Result<String, DiagramError> {
    Err(DiagramError::new("built without Pikchr"))
}

/// Mermaid diagram types that render.
pub const MERMAID_TYPES: &[&str] = &[
    "flowchart",
    "graph",
    "sequenceDiagram",
    "classDiagram",
    "stateDiagram",
    "stateDiagram-v2",
    "erDiagram",
    "gantt",
    "pie",
    "mindmap",
    "journey",
    "timeline",
    "gitGraph",
    "quadrantChart",
    "xychart",
    "xychart-beta",
    "requirementDiagram",
    "sankey",
    "sankey-beta",
    "C4Context",
    "C4Container",
    "C4Component",
    "C4Dynamic",
    "C4Deployment",
];

/// Uses this font file to measure Mermaid text, the font its SVG text is drawn with.
pub fn set_mermaid_font(_data: &'static [u8]) {
    #[cfg(feature = "mermaid")]
    mermaid_rs_renderer::set_measure_font(_data);
}

/// The deepest nesting of brackets on any line (unbalanced closers count as zero).
#[cfg(feature = "mermaid")]
fn bracket_depth(source: &str) -> usize {
    source
        .lines()
        .map(|line| {
            let (mut depth, mut max) = (0usize, 0);
            for b in line.bytes() {
                match b {
                    b'(' | b'[' | b'{' => {
                        depth += 1;
                        max = max.max(depth);
                    }
                    b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            max
        })
        .max()
        .unwrap_or(0)
}

#[cfg(feature = "mermaid")]
fn mermaid(source: &str, dark: bool) -> Result<String, DiagramError> {
    if source.len() > MERMAID_MAX_INPUT {
        return Err(DiagramError::new(format!(
            "Mermaid input is larger than {} KiB",
            MERMAID_MAX_INPUT / 1024
        )));
    }
    if bracket_depth(source) > MERMAID_MAX_NESTING {
        return Err(DiagramError::new(format!(
            "Mermaid brackets nest deeper than {MERMAID_MAX_NESTING} levels"
        )));
    }
    // The first line that is not blank, a comment or front matter names the type.
    let mut in_front_matter = false;
    let header = source.lines().map(str::trim).find(|l| {
        if *l == "---" {
            in_front_matter = !in_front_matter;
            return false;
        }
        !(in_front_matter || l.is_empty() || l.starts_with("%%"))
    });
    let kind = header
        .and_then(|h| h.split(|c: char| c.is_whitespace() || c == ';').next())
        .unwrap_or("");
    if !MERMAID_TYPES.iter().any(|t| t.eq_ignore_ascii_case(kind)) {
        return Err(DiagramError::new(if kind.is_empty() {
            "empty Mermaid diagram".to_owned()
        } else {
            format!("unsupported Mermaid diagram type: {kind}")
        }));
    }
    let mut theme = if dark {
        mermaid_rs_renderer::Theme::dark()
    } else {
        mermaid_rs_renderer::Theme::modern()
    };
    theme.font_family = "Inter, sans-serif".into();
    // The reader's page colours, so the diagram has no box of its own. They repeat
    // `background` of the two themes in `crates/mdt-app/src/theme.rs`; keep them in step.
    theme.background = if dark { "#0d1117" } else { "#ffffff" }.into();
    let layout = mermaid_rs_renderer::LayoutConfig::default();
    let parsed = mermaid_rs_renderer::parse_mermaid_strict(source)
        .map_err(|e| DiagramError::new(e.to_string()))?;
    let graph = &parsed.graph;
    use mermaid_rs_renderer::DiagramKind as K;
    // Only the layered layout (ranks, crossing reduction and edge routing) slows down
    // with edges; the other kinds place their edges directly.
    let edge_limit = match graph.kind {
        K::Sequence => Some((MERMAID_MAX_MESSAGES, "messages")),
        K::Flowchart | K::Class | K::State | K::Er | K::Requirement | K::Packet => {
            Some((MERMAID_MAX_EDGES, "edges"))
        }
        _ => None,
    };
    // A git graph's commits and branches are not nodes, and its layout grows with both:
    // 3,000 branches of a commit each took 1.6 s and drew in 18 s.
    let git = graph.gitgraph.commits.len() + graph.gitgraph.branches.len();
    let (nodes, edges) = (graph.nodes.len() + git, graph.edges.len());
    let too_many_edges = edge_limit.is_some_and(|(max, _)| edges > max);
    if nodes > MERMAID_MAX_NODES || too_many_edges {
        return Err(DiagramError::new(match edge_limit {
            Some((max, what)) => format!(
                "Mermaid diagram too large to draw ({nodes} nodes and {edges} {what}; the \
                 limits are {MERMAID_MAX_NODES} and {max})"
            ),
            None => format!(
                "Mermaid diagram too large to draw ({nodes} nodes; the limit is \
                 {MERMAID_MAX_NODES})"
            ),
        }));
    }
    let laid_out = mermaid_rs_renderer::compute_layout(graph, &theme, &layout);
    Ok(mermaid_rs_renderer::render_svg(&laid_out, &theme, &layout))
}

#[cfg(not(feature = "mermaid"))]
fn mermaid(_: &str, _: bool) -> Result<String, DiagramError> {
    Err(DiagramError::new("built without Mermaid"))
}

#[cfg(test)]
mod tests;
