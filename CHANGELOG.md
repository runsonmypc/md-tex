# Changelog

## 1.0.1

- Fixes a crash on some malformed Markdown (a link definition in a quoted list after a setext heading), from pulldown-cmark 0.13.6.
- Typing in large documents updates the reader and the outline at once: only the blocks around an edit are parsed again. The split preview of a 1 MB document updates in about 4 ms instead of 20.
- Opening a document is faster: the system font list loads while the window opens, and large documents parse faster.
- Scrolling stays smoother while equations, diagrams and images finish loading, since only the blocks that were waiting are laid out again.
- The macOS app compiles its Metal shaders at build time instead of at each launch.

## 1.0.0

The first release of md-tex, a native Markdown reader and editor for macOS (Apple silicon) and Linux (x86_64, X11).

- Reader view: headings, lists, task lists with clickable checkboxes, quotes, GitHub alerts, tables that wrap their cells and widen into the margins, footnotes, front matter, and raw `<br>` and `<img>` tags.
- Code blocks highlighted for Rust, Python, JavaScript, TypeScript, JSON, Bash, TOML, YAML, C and Go.
- LaTeX math, inline and display, typeset with Typst.
- Mermaid, Pikchr and SVG diagrams.
- Local images (PNG, JPEG, GIF, WebP and SVG). Remote images load only when you allow them.
- Source editor and a split view with the reader beside it. Find works in every view, and replace in the source.
- Command palette with every command, the document's headings and recent files.
- Light and dark themes that follow the system, zoom, and a wide reading column.
- Settings in a TOML file, applied as soon as it changes.

macOS builds are not notarized; see the README for how to open the app the first time.
