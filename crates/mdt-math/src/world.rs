//! `MathWorld`: a Typst world with the main source, the vendored mitex scope and the
//! equation text, all in memory. It never touches the file system.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

use crate::{MathError, MathScene, scene};

const SCOPE_FILES: &[(&str, &str)] = &[
    (
        "/specs/mod.typ",
        include_str!("../typst-scope/specs/mod.typ"),
    ),
    (
        "/specs/prelude.typ",
        include_str!("../typst-scope/specs/prelude.typ"),
    ),
    (
        "/specs/latex/standard.typ",
        include_str!("../typst-scope/specs/latex/standard.typ"),
    ),
];

/// The equation is read from `/eq.txt` and evaluated with the mitex scope, so its text
/// needs no escaping. `\text` uses the math font's own text glyphs. The box keeps the
/// equation's frame (and with it the baseline) as a group; display mode is an inline
/// equation in display style, so both modes have one. Edges at the glyph bounds make the
/// frame enclose the ink.
const MAIN: &str = r#"#import "/specs/mod.typ": mitex-scope
#set page(width: auto, height: auto, margin: 0pt, fill: none)
#set text(size: 10pt, font: "New Computer Modern Math", top-edge: "bounds", bottom-edge: "bounds")
#let body = eval(read("/eq.txt"), mode: "math", scope: mitex-scope)
#let body = if read("/mode.txt") == "display" { math.display(body) } else { body }
#box(math.equation(block: false, body))
"#;

/// Command spec regenerated from the vendored mitex 0.2.7 scope (`examples/gen_spec.rs`);
/// the one built into mitex 0.2.4 predates Typst's symbol renames.
static SPEC: LazyLock<mitex_spec::CommandSpec> = LazyLock::new(|| {
    mitex_spec::CommandSpec::from_bytes(include_bytes!("../typst-scope/default.rkyv"))
});

fn file_id(path: &str) -> FileId {
    FileId::new(RootedPath::new(
        VirtualRoot::Project,
        VirtualPath::new(path).unwrap(),
    ))
}

pub struct MathWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main: FileId,
    sources: HashMap<FileId, Source>,
    /// Per-compile inputs: converted equation text and mode.
    inputs: Mutex<(Bytes, Bytes)>,
}

/// Stack for one formula; pages are committed only as they are touched.
const STACK: usize = 256 << 20;

impl MathWorld {
    pub fn new(fonts: &[&'static [u8]]) -> Self {
        Self::with_main(fonts, MAIN)
    }

    /// A world whose main file is `main_text`, with the mitex scope files available.
    pub fn with_main(fonts: &[&'static [u8]], main_text: &str) -> Self {
        let fonts: Vec<Font> = fonts
            .iter()
            .flat_map(|data| Font::iter(Bytes::new(*data)))
            .collect();
        let book = FontBook::from_fonts(&fonts);
        let main = file_id("/main.typ");
        let mut sources = HashMap::new();
        sources.insert(main, Source::new(main, main_text.into()));
        for (path, text) in SCOPE_FILES {
            let id = file_id(path);
            sources.insert(id, Source::new(id, (*text).into()));
        }
        Self {
            library: LazyHash::new(Library::default()),
            book: LazyHash::new(book),
            fonts,
            main,
            sources,
            inputs: Mutex::new((Bytes::new(Vec::new()), Bytes::new(Vec::new()))),
        }
    }

    /// Converts LaTeX with mitex and typesets it. mitex and Typst recurse once per nesting
    /// level, so a long run of nested commands overflows a worker's stack (found by
    /// fuzzing); the work runs on its own `STACK` of reserved memory, and sources longer
    /// than `MAX_SOURCE` are refused.
    pub fn typeset(&self, latex: &str, display: bool) -> Result<MathScene, MathError> {
        if latex.len() > crate::MAX_SOURCE {
            return Err(MathError {
                message: format!(
                    "formula too long ({} KB; the limit is {} KB)",
                    latex.len() / 1024,
                    crate::MAX_SOURCE / 1024
                ),
            });
        }
        stacker::grow(STACK, || self.typeset_unchecked(latex, display))
    }

    fn typeset_unchecked(&self, latex: &str, display: bool) -> Result<MathScene, MathError> {
        let typst_src = mitex::convert_math(latex, Some(SPEC.clone())).map_err(|e| MathError {
            message: e.to_string(),
        })?;
        refuse_code(&typst_src)?;
        *self.inputs() = (
            Bytes::new(typst_src.into_bytes()),
            Bytes::new(if display {
                &b"display"[..]
            } else {
                &b"inline"[..]
            }),
        );
        let doc = self.compile()?;
        let page = doc.pages().first().ok_or_else(|| MathError {
            message: "empty formula".into(),
        })?;
        Ok(scene::from_frame(&page.frame, &self.fonts))
    }

    /// The inputs, even after a panic while they were held (the typesetter's panics are
    /// caught, and the world is reused).
    fn inputs(&self) -> std::sync::MutexGuard<'_, (Bytes, Bytes)> {
        self.inputs.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn compile(&self) -> Result<PagedDocument, MathError> {
        typst::compile::<PagedDocument>(self)
            .output
            .map_err(|diags| MathError {
                message: diags
                    .iter()
                    .map(|d| d.message.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            })
    }
}

/// Functions the converter calls with `#`: its own (`math.equation` for `$` inside text,
/// `strong` and `emph` for `\textbf` and `\textit` in text), the `#` aliases in
/// `typst-scope/specs/latex/standard.typ`, and the text environments there (`quote`,
/// `figure`, `table`).
const CODE_CALLS: &[&str] = &[
    "math.equation",
    "strong",
    "emph",
    "heading",
    "footnote",
    "mitexcite",
    "mitexref",
    "colortext",
    "mitexcolorbox",
    "textmath",
    "textmd",
    "textnormal",
    "textbf",
    "textit",
    "textrm",
    "textup",
    "textsf",
    "texttt",
    "quote",
    "figure",
    "table",
];

/// Refuses converted text that would run Typst code other than the converter's own calls.
/// Every `#` that is not escaped must start one of `CODE_CALLS`; the converter writes the
/// arguments itself, and anything from the source in them goes through this check too.
/// mitex once copied raw source into the output (`\iftypst`, `\includegraphics`,
/// `\label`); this keeps any such path from running code.
fn refuse_code(typst: &str) -> Result<(), MathError> {
    let mut chars = typst.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '#' => {
                let rest = &typst[i + 1..];
                let name = rest
                    .split(|c: char| !(c.is_alphanumeric() || "_-.".contains(c)))
                    .next()
                    .unwrap_or("");
                if !CODE_CALLS.contains(&name) {
                    return Err(MathError {
                        message: format!("the formula would run Typst code (#{name})"),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

impl World for MathWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }
    fn main(&self) -> FileId {
        self.main
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        self.sources
            .get(&id)
            .cloned()
            .ok_or_else(|| FileError::NotFound(id.vpath().get_without_slash().into()))
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let inputs = self.inputs();
        match id.vpath().get_without_slash() {
            "eq.txt" => Ok(inputs.0.clone()),
            "mode.txt" => Ok(inputs.1.clone()),
            other => Err(FileError::NotFound(other.into())),
        }
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }
    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        Datetime::from_ymd(2026, 1, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::refuse_code;

    #[test]
    fn only_the_converters_own_calls_may_run_code() {
        for ok in [
            r"x + \#",
            "#textmath[a #math.equation(block: false, $x$);];",
            "#heading(level: 1)[a];",
            "frac(1, 2)",
        ] {
            assert!(refuse_code(ok).is_ok(), "{ok}");
        }
        for bad in [
            r#"#panic("x")"#,
            "#textmath.with(x)",
            "#textmathx",
            r"\\#eval(x)",
            "x #",
            "#(1)",
        ] {
            assert!(refuse_code(bad).is_err(), "{bad}");
        }
    }
}
