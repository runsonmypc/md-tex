#![no_main]

use libfuzzer_sys::fuzz_target;
use mdt_math::MathWorld;

static FONTS: &[&[u8]] = &[include_bytes!(
    "../../crates/mdt-text/fonts/NewCMMath-Regular.otf"
)];

thread_local! {
    static WORLD: MathWorld = MathWorld::new(FONTS);
}

// What `mdt_math::Cache::compute` runs, without its panic guard so that panics surface:
// mitex with md-tex's command spec, the check for Typst code, then the Typst compile, with
// the length limit and the large stack of `MathWorld::typeset`.
fuzz_target!(|latex: &str| {
    WORLD.with(|world| {
        let _ = world.typeset(latex, false);
    });
    // Typst's memoized results, dropped as the app does after each batch.
    comemo::evict(10);
});
