//! Code-block highlighting with tree-sitter. Each grammar is a feature; an
//! unknown or disabled language yields no spans and no error. Results are cached by
//! (language, content hash) and the work is meant to run on a background thread.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::{ControlFlow, Range};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use tree_sitter::{
    Language, ParseOptions, Parser, Query, QueryCursor, QueryCursorOptions, StreamingIterator,
};

/// Highlighting gives up after this long plus `TIME_PER_MB` per megabyte of code, and the
/// block shows without colours. Ordinary blocks take well under a millisecond and large
/// ones about 0.25 s per MB (3 MB of JSON took 0.4 s), but tree-sitter's error recovery
/// is quadratic on unclosed nesting: 20 KB of `{` in a Rust fence took 0.6 s.
pub const TIME_LIMIT: Duration = Duration::from_millis(500);
pub const TIME_PER_MB: Duration = Duration::from_secs(1);

/// A timed-out block is tried once more when it is next asked for after this long (the
/// thread pool may have been busy); a second time-out is final.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Deepest bracket nesting highlighted. Matching the highlight query tracks every open
/// level, so deep nesting ran far past the time limit: 100,000 balanced braces took 44 s,
/// as many `$(` in Bash 42 s.
pub const MAX_NESTING: usize = 256;

/// Deepest indentation highlighted, in columns (a tab is 8), for the grammars whose
/// external scanner keeps a stack of indents (YAML, Python). Past about 250 levels the
/// stack overflows tree-sitter's 1,024 bytes of scanner state, and its assertion aborts
/// the app: 255 YAML `- ` on one line did. Each level takes at least one column; YAML's
/// `- `, `? ` and `: ` before content count as columns too.
pub const MAX_INDENT: usize = 120;

/// Bytes of spans the cache keeps (estimated); beyond it the least recently used go. Every
/// edit of a code block is a new entry.
pub const CACHE_BYTES: usize = 16 << 20;

/// The highlight classes the theme colours. Capture names map to the first class whose
/// name is a prefix of the capture (`keyword.control` → `Keyword`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    Keyword,
    String,
    Comment,
    Number,
    Function,
    Type,
    Constant,
    Property,
    Operator,
    Punctuation,
    Variable,
    Attribute,
    Tag,
}

const CAPTURES: [(&str, Class); 22] = [
    ("keyword", Class::Keyword),
    ("include", Class::Keyword),
    ("conditional", Class::Keyword),
    ("repeat", Class::Keyword),
    ("string", Class::String),
    ("escape", Class::String),
    ("character", Class::String),
    ("comment", Class::Comment),
    ("number", Class::Number),
    ("float", Class::Number),
    ("boolean", Class::Constant),
    ("constant", Class::Constant),
    ("function", Class::Function),
    ("method", Class::Function),
    ("constructor", Class::Type),
    ("type", Class::Type),
    ("property", Class::Property),
    ("operator", Class::Operator),
    ("punctuation", Class::Punctuation),
    ("variable", Class::Variable),
    ("attribute", Class::Attribute),
    ("tag", Class::Tag),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub class: Class,
}

/// Canonical language name for a fence info string's first word, if a grammar is built in.
pub fn language(info: &str) -> Option<&'static str> {
    let word = info.split_whitespace().next()?.to_ascii_lowercase();
    let name = match word.as_str() {
        "rust" | "rs" => "rust",
        "python" | "py" => "python",
        "javascript" | "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "typescript" | "ts" | "tsx" => "typescript",
        "json" | "jsonc" => "json",
        "bash" | "sh" | "shell" | "zsh" => "bash",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "c" | "h" => "c",
        "go" | "golang" => "go",
        _ => return None,
    };
    configs().contains_key(name).then_some(name)
}

/// A grammar, its highlight query, and the class of each of the query's captures.
struct Config {
    language: Language,
    query: Query,
    classes: Vec<Option<Class>>,
}

/// The class for a capture name: of the `CAPTURES` names that are one of its dot-separated
/// parts, the first (`function.method` is `Function`, `string.escape` is `String`).
fn class_of(capture: &str) -> Option<Class> {
    CAPTURES
        .iter()
        .find(|(name, _)| capture.split('.').any(|part| part == *name))
        .map(|&(_, class)| class)
}

fn configs() -> &'static HashMap<&'static str, Config> {
    static CONFIGS: OnceLock<HashMap<&'static str, Config>> = OnceLock::new();
    CONFIGS.get_or_init(|| {
        let mut m = HashMap::new();
        let mut add =
            |name: &'static str, lang: tree_sitter_language::LanguageFn, highlights: &str| {
                let language = Language::from(lang);
                let query = Query::new(&language, highlights)
                    .unwrap_or_else(|e| panic!("{name} highlight query: {e}"));
                let classes = query.capture_names().iter().map(|c| class_of(c)).collect();
                m.insert(
                    name,
                    Config {
                        language,
                        query,
                        classes,
                    },
                );
            };
        #[cfg(feature = "rust")]
        add(
            "rust",
            tree_sitter_rust::LANGUAGE,
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        );
        #[cfg(feature = "python")]
        add(
            "python",
            tree_sitter_python::LANGUAGE,
            tree_sitter_python::HIGHLIGHTS_QUERY,
        );
        #[cfg(feature = "javascript")]
        add(
            "javascript",
            tree_sitter_javascript::LANGUAGE,
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        );
        #[cfg(feature = "typescript")]
        add(
            "typescript",
            tree_sitter_typescript::LANGUAGE_TSX,
            &format!(
                "{}\n{}",
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                tree_sitter_javascript::HIGHLIGHT_QUERY
            ),
        );
        #[cfg(feature = "json")]
        add(
            "json",
            tree_sitter_json::LANGUAGE,
            tree_sitter_json::HIGHLIGHTS_QUERY,
        );
        #[cfg(feature = "bash")]
        add(
            "bash",
            tree_sitter_bash::LANGUAGE,
            tree_sitter_bash::HIGHLIGHT_QUERY,
        );
        #[cfg(feature = "toml")]
        add(
            "toml",
            tree_sitter_toml_ng::LANGUAGE,
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        );
        #[cfg(feature = "yaml")]
        add(
            "yaml",
            tree_sitter_yaml::LANGUAGE,
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
        );
        #[cfg(feature = "c")]
        add("c", tree_sitter_c::LANGUAGE, tree_sitter_c::HIGHLIGHT_QUERY);
        #[cfg(feature = "go")]
        add(
            "go",
            tree_sitter_go::LANGUAGE,
            tree_sitter_go::HIGHLIGHTS_QUERY,
        );
        let _ = &mut add;
        m
    })
}

/// The time limit for `code`: `TIME_LIMIT` plus `TIME_PER_MB` per megabyte.
pub fn time_limit(code: &str) -> Duration {
    TIME_LIMIT + TIME_PER_MB.mul_f64(code.len() as f64 / (1 << 20) as f64)
}

/// Highlights `code` in the language named by a fence info string. Unknown languages,
/// disabled grammars, grammar errors, panics, nesting too deep and running past
/// `time_limit` all give no spans.
pub fn highlight(info: &str, code: &str) -> Vec<Span> {
    highlight_within(info, code, time_limit(code)).unwrap_or_default()
}

/// `highlight` with a time limit of `limit`; `None` if it ran out of time.
pub fn highlight_within(info: &str, code: &str, limit: Duration) -> Option<Vec<Span>> {
    let Some(name) = language(info) else {
        return Some(Vec::new());
    };
    if too_nested(name, code) {
        return Some(Vec::new());
    }
    let cancel = deadline(limit);
    // On a background thread, a panic would abort the app.
    catch_unwind(AssertUnwindSafe(|| spans(&configs()[name], code, &cancel)))
        .unwrap_or(Some(Vec::new()))
}

/// Whether `code` nests deeper than `MAX_NESTING` brackets or, in a grammar with an
/// indent-stack scanner, is indented past `MAX_INDENT`.
fn too_nested(name: &str, code: &str) -> bool {
    let (mut depth, mut max) = (0usize, 0);
    for b in code.bytes() {
        match b {
            b'(' | b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if max > MAX_NESTING {
        return true;
    }
    let yaml = name == "yaml";
    if !(yaml || name == "python") {
        return false;
    }
    // YAML ends a line at a lone `\r` as well; Python's scanner starts counting again
    // after `\r` or a form feed, and goes on counting past a `\` that ends a line: 600
    // levels of indentation made of lines of 100 spaces and a `\` aborted the app.
    let (mut cols, mut rest, mut leading) = (0, code.as_bytes(), true);
    while let [b, r @ ..] = rest {
        rest = r;
        if matches!(b, b'\n' | b'\r') {
            (cols, leading) = (0, true);
            continue;
        }
        if !leading {
            continue;
        }
        match (b, rest) {
            (b' ', _) => cols += 1,
            (b'\t', _) => cols += 8,
            (b'\x0c', _) => cols = 0,
            (b'-' | b'?' | b':', [b' ' | b'\t', ..]) if yaml => cols += 1,
            (b'\\', [b'\n', r @ ..] | [b'\r', b'\n', r @ ..]) if !yaml => rest = r,
            _ => leading = false,
        }
        if cols > MAX_INDENT {
            return true;
        }
    }
    false
}

/// A cancel flag that one shared watchdog thread raises once `limit` has passed.
fn deadline(limit: Duration) -> Arc<AtomicUsize> {
    type Deadline = (Instant, Arc<AtomicUsize>);
    static WATCHDOG: OnceLock<mpsc::Sender<Deadline>> = OnceLock::new();
    let watchdog = WATCHDOG.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Deadline>();
        std::thread::Builder::new()
            .name("highlight-watchdog".into())
            .spawn(move || {
                let mut due: Vec<Deadline> = Vec::new();
                loop {
                    let now = Instant::now();
                    // Raise the flags past their deadline; forget finished calls.
                    due.retain(|(at, flag)| {
                        if *at <= now {
                            flag.store(1, Ordering::Relaxed);
                        }
                        *at > now && Arc::strong_count(flag) > 1
                    });
                    let next = match due.iter().map(|(at, _)| *at).min() {
                        Some(at) => rx.recv_timeout(at - now),
                        None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                    };
                    match next {
                        Ok(d) => due.push(d),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("spawn the highlight watchdog");
        tx
    });
    let flag = Arc::new(AtomicUsize::new(0));
    let _ = watchdog.send((Instant::now() + limit, flag.clone()));
    flag
}

/// tree-sitter checks `cancel` while parsing and while matching the query; `None` if it
/// was raised. The query has to be cancellable: on a deep tree (100,000 chained
/// `else if`s) matching it ran a minute past the time limit.
///
/// The spans are those `tree-sitter-highlight` gave: captures come in document order, and
/// of several captures of one node the last pattern's wins (the others' matches are
/// dropped, with their later captures). A capture's class lasts until its node ends.
fn spans(config: &Config, code: &str, cancel: &AtomicUsize) -> Option<Vec<Span>> {
    let halted = Cell::new(false);
    let check = || {
        if cancel.load(Ordering::Relaxed) != 0 {
            halted.set(true);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut parser = Parser::new();
    if parser.set_language(&config.language).is_err() {
        return Some(Vec::new());
    }
    let bytes = code.as_bytes();
    let mut parse_check = |_: &tree_sitter::ParseState| check();
    let Some(tree) = parser.parse_with_options(
        &mut |i, _| bytes.get(i..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut parse_check)),
    ) else {
        return (!halted.get()).then(Vec::new);
    };
    let mut query_check = |_: &tree_sitter::QueryCursorState| check();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures_with_options(
        &config.query,
        tree.root_node(),
        bytes,
        QueryCursorOptions::new().progress_callback(&mut query_check),
    );
    let mut out = Spans::default();
    let mut dropped = HashSet::new();
    // The capture waiting for any later patterns' captures of the same node.
    let mut last: Option<(tree_sitter::Node, Option<Class>, u32)> = None;
    while let Some((m, i)) = captures.next() {
        if dropped.contains(&m.id()) {
            continue;
        }
        let capture = m.captures()[*i];
        let class = config.classes[capture.index as usize];
        match last {
            Some((node, _, id)) if node == capture.node => {
                dropped.insert(id);
            }
            Some((node, class, _)) => out.capture(node.byte_range(), class),
            None => {}
        }
        last = Some((capture.node, class, m.id()));
    }
    if halted.get() {
        return None;
    }
    if let Some((node, class, _)) = last {
        out.capture(node.byte_range(), class);
    }
    Some(out.finish())
}

/// Spans built from captures in document order.
#[derive(Default)]
struct Spans {
    spans: Vec<Span>,
    /// Text before this has its spans.
    at: usize,
    /// The classes in effect, innermost last, with where each ends.
    open: Vec<(usize, Class)>,
}

impl Spans {
    fn capture(&mut self, range: Range<usize>, class: Option<Class>) {
        while let Some(&(end, _)) = self.open.last()
            && end <= range.start
        {
            self.advance(end);
            self.open.pop();
        }
        if let Some(class) = class {
            self.advance(range.start);
            self.open.push((range.end, class));
        }
    }

    /// Gives the text up to `to` the innermost class.
    fn advance(&mut self, to: usize) {
        if to <= self.at {
            return;
        }
        if let Some(&(_, class)) = self.open.last() {
            match self.spans.last_mut() {
                Some(Span { range, class: c }) if *c == class && range.end == self.at => {
                    range.end = to
                }
                _ => self.spans.push(Span {
                    range: self.at..to,
                    class,
                }),
            }
        }
        self.at = to;
    }

    fn finish(mut self) -> Vec<Span> {
        while let Some(&(end, _)) = self.open.last() {
            self.advance(end);
            self.open.pop();
        }
        self.spans
    }
}

/// Results by (language, content hash), shared across threads and windows, bounded by
/// `CACHE_BYTES`.
#[derive(Clone)]
pub struct Cache(Arc<Mutex<Results>>);

/// (canonical language, content hash).
pub type CacheKey = (String, u64);

/// (spans, estimated bytes, last use, when a timed-out result expires).
type Entry = (Arc<Vec<Span>>, usize, u64, Option<Instant>);

struct Results {
    cap: usize,
    used: usize,
    tick: u64,
    entries: HashMap<CacheKey, Entry>,
    /// Blocks that timed out and were tried again: a second time-out stays.
    retried: HashSet<CacheKey>,
    /// `time_limit` and `RETRY_AFTER`; tests shorten them.
    limit: fn(&str) -> Duration,
    retry_after: Duration,
}

impl Default for Cache {
    fn default() -> Self {
        Self::with_capacity(CACHE_BYTES)
    }
}

impl Cache {
    /// A cache keeping about `bytes` of results.
    pub fn with_capacity(bytes: usize) -> Self {
        Self(Arc::new(Mutex::new(Results {
            cap: bytes,
            used: 0,
            tick: 0,
            entries: HashMap::new(),
            retried: HashSet::new(),
            limit: time_limit,
            retry_after: RETRY_AFTER,
        })))
    }

    /// Estimated bytes of results held.
    pub fn bytes(&self) -> usize {
        self.0.lock().unwrap().used
    }

    pub fn key(info: &str, code: &str) -> CacheKey {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        code.hash(&mut h);
        (language(info).unwrap_or("").to_owned(), h.finish())
    }

    /// The cached result. A timed-out one past `RETRY_AFTER` is dropped instead, so the
    /// block is highlighted again.
    pub fn get(&self, info: &str, code: &str) -> Option<Arc<Vec<Span>>> {
        let key = Self::key(info, code);
        let mut r = self.0.lock().unwrap();
        r.tick += 1;
        let tick = r.tick;
        let e = r.entries.get_mut(&key)?;
        if e.3.is_some_and(|at| at <= Instant::now()) {
            let (_, bytes, _, _) = r.entries.remove(&key).expect("present");
            r.used -= bytes;
            r.retried.insert(key);
            return None;
        }
        e.2 = tick;
        Some(e.0.clone())
    }

    /// Highlights (on the calling thread) unless cached, and caches the result.
    pub fn compute(&self, info: &str, code: &str) -> Arc<Vec<Span>> {
        if let Some(s) = self.get(info, code) {
            return s;
        }
        let limit = self.0.lock().unwrap().limit;
        let result = highlight_within(info, code, limit(code));
        let timed_out = result.is_none();
        let spans = Arc::new(result.unwrap_or_default());
        let key = Self::key(info, code);
        let bytes = 64 + key.0.len() + spans.len() * std::mem::size_of::<Span>();
        let mut r = self.0.lock().unwrap();
        // A time-out may come from a busy machine rather than the code: it is kept for a
        // while (not retried every frame), then tried once more.
        let expires =
            (timed_out && !r.retried.contains(&key)).then(|| Instant::now() + r.retry_after);
        if let Some((_, old, _, _)) = r.entries.remove(&key) {
            r.used -= old;
        }
        while r.used + bytes > r.cap && !r.entries.is_empty() {
            let oldest = r
                .entries
                .iter()
                .min_by_key(|(_, e)| e.2)
                .map(|(k, _)| k.clone())
                .expect("not empty");
            let (_, gone, _, _) = r.entries.remove(&oldest).expect("present");
            r.used -= gone;
        }
        r.tick += 1;
        let tick = r.tick;
        r.used += bytes;
        r.entries.insert(key, (spans.clone(), bytes, tick, expires));
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(info: &str, code: &str) -> Vec<(String, Class)> {
        highlight(info, code)
            .into_iter()
            .map(|s| (code[s.range].to_owned(), s.class))
            .collect()
    }

    #[test]
    #[cfg(feature = "rust")]
    fn rust_fence_yields_keyword_string_and_comment() {
        let got = classes("rust", "fn main() {\n    let s = \"hi\"; // greet\n}\n");
        assert!(got.contains(&("fn".into(), Class::Keyword)), "{got:?}");
        assert!(got.contains(&("let".into(), Class::Keyword)), "{got:?}");
        assert!(
            got.iter()
                .any(|(t, c)| t.contains("\"hi\"") && *c == Class::String),
            "{got:?}"
        );
        assert!(
            got.contains(&("// greet".into(), Class::Comment)),
            "{got:?}"
        );
    }

    #[test]
    fn unknown_language_yields_nothing() {
        assert!(highlight("klingon", "qapla'").is_empty());
        assert!(highlight("", "plain").is_empty());
        assert_eq!(language("brainfuck"), None);
    }

    #[test]
    fn every_enabled_grammar_loads_and_highlights() {
        let samples = [
            ("python", "def f(x):\n    return 'a'  # c\n"),
            ("js", "const x = 'a'; // c\n"),
            ("ts", "const x: number = 1; // c\n"),
            ("json", "{\"a\": 1}"),
            ("sh", "echo \"a\" # c\n"),
            ("toml", "a = \"b\" # c\n"),
            ("yaml", "a: b # c\n"),
            ("c", "int main() { return 0; } // c\n"),
            ("go", "func main() { x := \"a\" } // c\n"),
            ("rs", "fn f() {}"),
        ];
        for (info, code) in samples {
            if language(info).is_some() {
                assert!(
                    !highlight(info, code).is_empty(),
                    "{info} produced no spans"
                );
            }
        }
    }

    /// Ordinary Rust, `n` bytes or a little more.
    fn rust_code(n: usize) -> String {
        let mut code = String::new();
        for i in 0.. {
            if code.len() >= n {
                break;
            }
            code.push_str(&format!("fn f{i}(x: u32) -> u32 {{ x + {i} }} // c\n"));
        }
        code
    }

    /// Code past the time limit gives up: tree-sitter checks the flag while parsing and
    /// between events. Every call shares one watchdog thread, and calls that finish in
    /// time are unaffected.
    #[test]
    #[cfg(feature = "rust")]
    fn highlighting_gives_up_at_the_time_limit() {
        let code = rust_code(200_000);
        let start = Instant::now();
        assert_eq!(highlight_within("rust", &code, Duration::ZERO), None);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    let ok = highlight_within("rust", "fn a() {}", Duration::from_secs(30));
                    assert!(!ok.unwrap().is_empty());
                });
            }
        });
    }

    /// Big fences get more time: 3 MB of JSON took 0.4 s and 1 MB of Rust 0.24 s in
    /// release, past the flat 500 ms on a slower machine or a busy pool.
    #[test]
    fn the_time_limit_grows_with_the_code() {
        assert_eq!(time_limit("fn a() {}").as_millis(), 500);
        let three_mb = " ".repeat(3 << 20);
        assert_eq!(time_limit(&three_mb), Duration::from_millis(3500));
    }

    /// Deep nesting is not highlighted: query matching tracks every open level and cannot
    /// be cancelled within a step (100,000 balanced braces ran 44 s past a 500 ms limit),
    /// and YAML's indent stack overflowed tree-sitter's scanner state, which aborts. These
    /// are refused before tree-sitter sees them.
    #[test]
    fn deep_nesting_is_refused_before_tree_sitter() {
        let n = 100_000;
        let deep = [
            ("rust", format!("fn f() {}{}", "{".repeat(n), "}".repeat(n))),
            (
                "rust",
                format!("fn f() {{ m!{}{} }}", "(".repeat(n), ")".repeat(n)),
            ),
            (
                "json",
                format!("{}1{}", "{\"a\": ".repeat(50_000), "}".repeat(50_000)),
            ),
            (
                "bash",
                format!("echo {}x{}", "$(".repeat(50_000), ")".repeat(50_000)),
            ),
            ("yaml", format!("a: {}{}", "[".repeat(n), "]".repeat(n))),
            ("yaml", format!("{}x\n", "- ".repeat(255))),
            (
                "yaml",
                (0..255)
                    .map(|i| format!("{}k{i}:\n", " ".repeat(i)))
                    .collect(),
            ),
            (
                "python",
                (0..300)
                    .map(|i| format!("{}if x:\n", " ".repeat(i)))
                    .collect(),
            ),
            // Lines that end at a lone `\r` (this one aborted the app), and indentation
            // after a form feed.
            (
                "yaml",
                (0..255)
                    .map(|i| format!("{}k{i}:\r", " ".repeat(i)))
                    .collect(),
            ),
            (
                "python",
                (0..300)
                    .map(|i| format!("\x0c{}if x:\n", " ".repeat(i)))
                    .collect(),
            ),
            // Indentation carried on past lines that end in `\`, with a string at the
            // deepest level (this one aborted the app).
            ("python", python_continued(600)),
        ];
        for (name, code) in &deep {
            assert!(too_nested(name, code), "{name}: {}", &code[..40]);
            if language(name).is_some() {
                let start = Instant::now();
                assert!(highlight(name, code).is_empty());
                assert!(start.elapsed() < Duration::from_secs(1));
            }
        }
        // Ordinary nesting and indentation are highlighted.
        let fine = [
            (
                "rust",
                format!("fn f() {}{}", "{".repeat(100), "}".repeat(100)),
            ),
            ("yaml", format!("{}x\n", "- ".repeat(40))),
            ("python", "def f():\n\tif x:\n\t\treturn 1\n".to_owned()),
            ("json", format!("{}1{}", "[".repeat(200), "]".repeat(200))),
            ("python", python_continued(100)),
            ("python", "x = 1 + \\\n    2 + \\\r\n        3\n".to_owned()),
        ];
        for (name, code) in &fine {
            assert!(!too_nested(name, code), "{name}");
        }
    }

    /// `levels` nested `if`s, indented one column more each, each indent written as lines
    /// of at most 100 spaces ending in `\`; a string at the deepest level.
    fn python_continued(levels: usize) -> String {
        let mut code = String::from("if x:\n");
        for i in 1..levels {
            code.push_str(&format!("{} \\\n", " ".repeat(99)).repeat(i / 100));
            code.push_str(&" ".repeat(i % 100));
            code.push_str(if i + 1 < levels { "if x:\n" } else { "'a'\n" });
        }
        code
    }

    /// Matching the query is cancelled too. Past 65,535 levels of syntax tree (here
    /// 40,000 chained `else if`s, which pass the nesting check) it ran far past the limit:
    /// 14 s for Rust, 10 s for C and 36 s for JavaScript against 200 ms.
    #[test]
    fn a_deep_tree_gives_up_at_the_time_limit() {
        let n = 40_000;
        let chains = [
            (
                "rust",
                format!("fn f() {{ {}{{ }} }}", "if a { } else ".repeat(n)),
            ),
            (
                "c",
                format!("void f() {{ {}y; }}", "if (a) x; else ".repeat(n)),
            ),
            ("js", format!("{}y;", "if (a) x; else ".repeat(n))),
        ];
        for (info, code) in &chains {
            if language(info).is_none() {
                continue;
            }
            assert!(!too_nested(language(info).unwrap(), code), "{info}");
            let start = Instant::now();
            assert_eq!(
                highlight_within(info, code, Duration::from_millis(200)),
                None,
                "{info}"
            );
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "{info}: {:?}",
                start.elapsed()
            );
        }
    }

    /// A time-out is cached for a while, so a block is not highlighted again every frame,
    /// then tried once more; a second time-out stays.
    #[test]
    #[cfg(feature = "rust")]
    fn a_timed_out_block_is_retried_once() {
        let code = rust_code(200_000);
        let cache = Cache::default();
        cache.0.lock().unwrap().limit = |_| Duration::ZERO;
        assert!(cache.compute("rust", &code).is_empty());
        assert!(
            cache.get("rust", &code).is_some(),
            "kept, not retried at once"
        );
        cache.0.lock().unwrap().retry_after = Duration::ZERO;
        assert!(
            cache.get("rust", &code).is_some(),
            "the first expiry still holds"
        );
        let cache = Cache::default();
        {
            let mut r = cache.0.lock().unwrap();
            r.limit = |_| Duration::ZERO;
            r.retry_after = Duration::ZERO;
        }
        assert!(cache.compute("rust", &code).is_empty());
        assert!(cache.get("rust", &code).is_none(), "expired: tried again");
        assert!(cache.compute("rust", &code).is_empty());
        assert!(
            cache.get("rust", &code).is_some(),
            "the second time-out stays"
        );
        // With time to finish, the retry highlights it.
        let cache = Cache::default();
        {
            let mut r = cache.0.lock().unwrap();
            r.limit = |_| Duration::ZERO;
            r.retry_after = Duration::ZERO;
        }
        cache.compute("rust", &code);
        assert!(cache.get("rust", &code).is_none());
        cache.0.lock().unwrap().limit = time_limit;
        assert!(!cache.compute("rust", &code).is_empty());
    }

    #[test]
    #[cfg(feature = "rust")]
    fn cache_keeps_the_most_recent_results_within_its_bytes() {
        let cache = Cache::with_capacity(4096);
        for i in 0..100 {
            cache.compute("rust", &format!("fn f{i}() {{ let x = {i}; }}"));
            assert!(cache.bytes() <= 4096, "{} bytes", cache.bytes());
        }
        assert!(cache.get("rust", "fn f99() { let x = 99; }").is_some());
        assert!(
            cache.get("rust", "fn f0() { let x = 0; }").is_none(),
            "the oldest went"
        );
    }

    #[test]
    fn cache_returns_same_result_by_language_and_hash() {
        let cache = Cache::default();
        assert!(cache.get("rust", "fn a() {}").is_none());
        let a = cache.compute("rust", "fn a() {}");
        let b = cache.compute("rs", "fn a() {}");
        assert!(Arc::ptr_eq(&a, &b), "alias hits the same entry");
    }
}
