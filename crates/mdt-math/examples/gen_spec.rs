//! Regenerates mitex's compact command spec from the vendored mitex Typst scope, the way
//! `mitex-spec-gen` does with `typst query`, but through our in-memory world. Run after
//! editing `typst-scope/specs`: `cargo run -p mdt-math --example gen_spec`.

use typst::foundations::{Label, Value};
use typst::introspection::Introspector;
use typst::utils::PicoStr;

static FONTS: &[&[u8]] = &[include_bytes!("../../mdt-text/fonts/NewCMMath-Regular.otf")];

fn main() {
    let world = mdt_math::MathWorld::with_main(
        FONTS,
        "#import \"/specs/mod.typ\": packages\n#metadata(packages) <mitex-packages>\n",
    );
    let doc = world.compile().expect("spec scope compiles");
    let label = Label::new(PicoStr::intern("mitex-packages")).unwrap();
    let content = doc.introspector().query_label(label).expect("one metadata");
    let value: Value = content.get_by_name("value").expect("metadata value");
    let json = serde_json::to_value(&value).unwrap();
    let packages: mitex_spec::query::PackagesVec =
        serde_json::from_value(json).expect("spec JSON matches the mitex-spec schema");
    let mut spec = mitex_spec::JsonCommandSpec::default();
    for package in packages.0 {
        spec.commands.extend(package.spec.commands);
    }
    let n = spec.commands.len();
    let compact: mitex_spec::CommandSpec = spec.into();
    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/typst-scope/default.rkyv");
    std::fs::write(out, compact.to_bytes()).unwrap();
    println!("wrote {n} commands to {out}");
}
