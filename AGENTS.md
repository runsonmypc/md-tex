# AGENTS.md

Notes for anyone (human or agent) working on this repository.

## Layout

- `crates/mdt-app`: the app (binary `md-tex`): windows, keymap, settings, reader view.
- `crates/mdt-doc`: rope buffer, file I/O, undo history, source search.
- `crates/mdt-markdown`: pulldown-cmark to the block model (layout units, source maps).
- `crates/mdt-text`: fonts, Parley paragraph layout, layout cache, GPUI glyph painting.
- `crates/mdt-highlight`: tree-sitter highlighting, one feature per grammar.
- `crates/mdt-math`: LaTeX through mitex and an in-memory Typst world to `MathScene`s, plus the scene cache. The Typst part is the `typeset` feature (the app's `math` feature).
  - `typst-scope/` is mitex's Typst package (0.2.7) with local fixes: `\argmax`/`\argmin`, dashed arrows, `Vmatrix`, colour commands that keep only their body, and `\hspace`/`\vspace`/`\raisebox` lengths parsed by `tex-length` instead of run with `eval` (a formula must never run Typst code). After editing it, regenerate the command spec: `cargo run -p mdt-math --example gen_spec`.
  - `vendor/mitex` is mitex 0.2.4 with fixes, applied through `[patch.crates-io]` and listed at the top of `src/converter.rs`: it formats with the caller's command spec (upstream ignored it); `cases` columns are a `\quad` apart, as in amsmath; `\iftypst` blocks and `\includegraphics` are errors and `\label` writes nothing, since upstream passed their raw text to Typst as code; and missing or unclosed arguments are errors, not panics. `mdt-math` also refuses converted text with any `#` call other than the converter's own. Its upstream tests, updated for these changes, run on their own: `cd vendor/mitex && cargo test` (the dev-dependencies need a network fetch once).
  - `vendor/mitex-lexer` is mitex-lexer 0.2.4 with a budget on `\newcommand` expansion (`MAX_EXPANSION` in `src/macro_engine.rs`, changes marked "md-tex"); upstream expanded without bound, so a recursive macro looped forever.
  - `vendor/rowan` is rowan 0.15.19 (mitex's syntax trees) with each cached node's hash kept beside it (`src/green/node_cache.rs`, marked "md-tex"); upstream rehashed every cached subtree when the cache grew, so building a deep tree took the square of its depth. The vendored mitex refuses trees deeper than `MAX_TREE_DEPTH` before converting.
  - `tests/corpus.txt` is the math corpus; every line must typeset in both modes.
- `vendor/xim-ctext` is xim-ctext 0.3.0 (GPUI's X11 input-method client uses it) with a full COMPOUND_TEXT decoder. Upstream decoded only UTF-8 and JIS X 0208, and GPUI panics on a decode error, so an IBus dead key (Latin-1) or simplified Chinese (GB 2312) crashed the app. Test it on its own: `cd vendor/xim-ctext && cargo test`.
- `crates/mdt-image`: image paths, header probe, decode with EXIF orientation, sizing (downscaling with `fast_image_resize`), the byte-bounded raster cache, the file watcher, remote fetch (`remote` feature), and the safe SVG rasterizer (`svg.rs`) that diagrams share.
- `crates/mdt-diagram`: Pikchr, Mermaid and SVG fences to SVG, each engine behind its feature and `catch_unwind`. `vendor/mermaid-rs-renderer` is 0.3.1 with `set_measure_font` added (upstream measured text with system fonts and wrote font files to `~/.cache/mmdr`), and a faster `segment_hits_node_shape_interior` (same output, a third of the routing time). Check the engine features one at a time with `cargo test -p mdt-diagram --no-default-features --features pikchr,svg` (and the other pairs).
- `crates/mdt-bench`: benchmark documents and budget checks.
- `fuzz/`: cargo-fuzz targets for Pikchr, Mermaid and LaTeX math (mitex plus the Typst compile), outside the workspace. Run one with nightly: `cd fuzz && cargo +nightly fuzz run mitex -- -max_total_time=600`. Add any crashing input to the owning crate's regression tests.

## Build and test

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI (`.github/workflows/ci.yml`, macOS and Linux) runs the same checks with `--locked`, plus clippy with `--features bench`. Releases: `RELEASING.md`.

- GPUI tests on macOS must create the platform on the main thread, so they are integration tests with `harness = false` (`crates/mdt-app/tests/launch.rs`, `reader.rs`, `editor.rs`, `search.rs`, `links.rs`, `crates/mdt-text/tests/gpui_fonts.rs`). Each has a hand-written `main` listing its tests.
- Layout snapshots use insta (`src/**/snapshots/`). Review changes with `INSTA_UPDATE=always cargo test` and read the diff. GPUI is faked in snapshot tests, so they match across platforms.
- `cargo test -p mdt-app --test settings -- --ignored` is the release check: it fails while any built-in default carries the `OWNER_DEFAULT_PENDING` marker.

## Features of `mdt-app`

All on by default: `runtime-shaders` (macOS; compiles Metal shaders at launch so the Command Line Tools suffice), `math`, `pikchr`, `mermaid`, `svg`, `remote-images`, and `hl-<lang>` per grammar (`rust`, `python`, `javascript`, `typescript`, `json`, `bash`, `toml`, `yaml`, `c`, `go`). With full Xcode, `--no-default-features` plus the wanted features precompiles shaders at build time. `bench` adds measurement hooks and is never on in release builds.

## Generated files

- `docs/shortcuts.md` comes from the keybinding registry: `cargo run -p mdt-app --bin gen-shortcuts`. A test fails if it is stale.
- `docs/visual/reader-*.png`: `cargo run -p mdt-app --example reader_check` renders `docs/visual/reader-check.md` offscreen in light and dark at 1,400 and 500 px (and with the wide reader at 1,400 px, `reader-*-1400-wide.png`), plus its math section at 300% zoom (`math-*-300.png`) and its images-and-diagrams section (`media-*.png`; the sample images are in `docs/visual/img/`). `cargo run -p mdt-text --example visual_check` renders the text-layout check. `cargo run -p mdt-app --example editor_check` renders the same document in Source (wrap on and off) and Split modes to `docs/visual/editor-*.png`.

## macOS app bundle

`tools/bundle-macos.sh` builds `target/release/md-tex.app` from `assets/macos/` (`Info.plist` with the Markdown document types, and the icon `md-tex.icns` rendered from `icon.svg`). It signs the bundle ad hoc. `--install` copies it to `~/Applications`, registers it with Launch Services, and writes an `md-tex` launcher to `~/.local/bin` that runs the bundled binary. To regenerate the icon after editing `icon.svg` (keep its background transparent; `qlmanage` thumbnails add a white one): `cargo run -p mdt-image --example render_icon -- assets/macos/icon.svg <dir>/md-tex.iconset`, then `iconutil -c icns <dir>/md-tex.iconset -o assets/macos/md-tex.icns`, and copy the SVG to `assets/linux/md-tex.svg`.

## Linux packages

`tools/package-linux.sh` builds `target/linux/md-tex-<version>-<arch>.tar.gz` (the binary, `assets/linux/md-tex.desktop`, the icon and `install.sh`, which installs into `~/.local` or `$PREFIX`) and an AppImage of the same binary (it downloads `appimagetool` once into `~/.cache/md-tex-packaging`).

To install from a source checkout without building packages, run `cargo build --release -p mdt-app --locked`, then `assets/linux/install.sh target/release/md-tex`. The window app ID must match `md-tex.desktop`; X11 also gets an embedded icon from `assets/linux/md-tex.svg`.

## Benchmarks

`mdt-bench` generates the benchmark documents and checks results against the budgets in `crates/mdt-bench/src/budget.rs`.

```sh
cargo run --release -p mdt-bench -- generate bench-docs            # all documents, including 200 12-MP photos (about 1 min, 650 MB)
cargo run --release -p mdt-bench -- generate bench-docs --no-photos
cargo run -p mdt-bench -- budgets                                  # print the budget table
cargo run -p mdt-bench -- check results.json                       # exits non-zero if any budget is exceeded
cargo run --release -p mdt-bench -- parse                          # parse throughput on the 1 MB and 10 MB documents
```

`results.json` looks like `{"platform": "macos", "metrics": {"startup_first_text_frame_ms": 92.0}}`.

The harness measures every budget with real windows (startup, scrolling, idle, typing, mode switch, preview, find, equations, diagrams, images, memory, size); run it on an otherwise idle machine with the screen unlocked (a hidden window draws no frames). It writes `bench-docs/results.json` and exits with `mdt-bench check`'s status. `--size` adds the per-feature size report (six more release builds).

```sh
CARGO_TARGET_DIR=target/bench cargo build --release -p mdt-app --features bench
python3 tools/bench.py target/bench/release/md-tex bench-docs [--size]
```

The `bench` feature's hooks are in `crates/mdt-app/src/bench.rs` (one `MDT_BENCH` mode per measurement).

Single-stage timings: `cargo run --release -p mdt-image --example decode_timing -- <photo.jpg>` (decode and resize) and `cargo run --release -p mdt-diagram --example diagram_timing` (the benchmark Mermaid and Pikchr diagrams). After changing the vendored Mermaid renderer, compare its output with `--example svg_digest` over `vendor/mermaid-rs-renderer/tests/fixtures/**/*.mmd` before and after.

Offline test (Linux, X11, needs `strace` and `xdotool`): `python3 tools/offline_check.py target/release/md-tex bench-docs` opens, pages, edits and saves the media corpus with remote image references and fails on any internet socket.

Find and palette timing on a document (the 10 MB one for the budgets): `cargo run --release -p mdt-app --example search_timing -- bench-docs/text-10m.md`.
