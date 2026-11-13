#![no_main]

use libfuzzer_sys::fuzz_target;

// The wrapper (input cap, NUL check, error extraction) around pikchr's C parser.
fuzz_target!(|source: &str| {
    for dark in [false, true] {
        let _ = mdt_diagram::to_svg(mdt_diagram::Engine::Pikchr, source, dark);
    }
});
