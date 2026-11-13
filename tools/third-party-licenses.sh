#!/bin/sh
# Writes THIRD-PARTY-LICENSES.md (or the path given): the licenses of every Rust crate linked
# into md-tex on the release platforms, from cargo-about, followed by the bundled fonts' licenses.
# Needs cargo-about: cargo install cargo-about --locked --features cli
set -eu
cd "$(dirname "$0")/.."
out=${1:-THIRD-PARTY-LICENSES.md}
cargo about generate --locked --fail -c tools/licenses/about.toml -m crates/mdt-app/Cargo.toml \
    tools/licenses/about.hbs > "$out"
for f in crates/mdt-text/fonts/*LICENSE*.txt; do
    printf '\n## Font: %s\n\n```text\n' "$(basename "$f" .txt)" >> "$out"
    cat "$f" >> "$out"
    printf '\n```\n' >> "$out"
done
echo "wrote $out"
