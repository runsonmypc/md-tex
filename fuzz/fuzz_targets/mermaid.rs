#![no_main]

use libfuzzer_sys::fuzz_target;

// Mermaid through md-tex's wrapper: its size, nesting, node and edge limits, then parse,
// layout and SVG. The wrapper turns panics into errors, as in the app, so this finds what
// it cannot catch: hangs (run with `-timeout`), memory blowups and stack overflows.
fuzz_target!(|source: &str| {
    let _ = mdt_diagram::to_svg(mdt_diagram::Engine::Mermaid, source, false);
});
