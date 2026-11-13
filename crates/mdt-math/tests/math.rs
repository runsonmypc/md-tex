use mdt_math::{Cache, MathWorld};

static FONTS: &[&[u8]] = &[include_bytes!("../../mdt-text/fonts/NewCMMath-Regular.otf")];

fn typeset(latex: &str, display: bool) -> mdt_math::MathScene {
    MathWorld::new(FONTS)
        .typeset(latex, display)
        .unwrap_or_else(|e| panic!("{latex}: {}", e.message))
}

#[test]
fn world_compiles_without_file_access() {
    let s = typeset("x^2", false);
    assert_eq!(s.glyphs.len(), 2);
    assert!(s.width > 0.0 && s.baseline > 0.0 && s.baseline < s.height);
    // Only the scope and the equation exist; nothing reaches the file system.
    let world = MathWorld::with_main(FONTS, "#read(\"/etc/hosts\")\n");
    let err = world.compile().expect_err("reading a real file must fail");
    assert!(err.message.contains("not found"), "{}", err.message);
}

#[test]
fn scene_fraction() {
    insta::assert_snapshot!(typeset(r"\frac{a}{b}", true).describe());
}

#[test]
fn scene_matrix() {
    insta::assert_snapshot!(
        typeset(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}", true).describe()
    );
}

#[test]
fn tall_bracket_pieces_stack_over_the_whole_matrix() {
    // A three-row bracket is a glyph assembly: one vertical text run whose pieces advance
    // in y. Ignoring `y_advance` drew every piece at the bottom row.
    let s = typeset(
        r"\begin{bmatrix} 1 & 0 & 0 \\ 0 & 1 & 0 \\ 0 & 0 & 1 \end{bmatrix}",
        true,
    );
    let left = s.glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
    let ys: Vec<f32> = s
        .glyphs
        .iter()
        .filter(|g| g.x < left + 0.5)
        .map(|g| g.y)
        .collect();
    let span =
        ys.iter().cloned().fold(f32::MIN, f32::max) - ys.iter().cloned().fold(f32::MAX, f32::min);
    assert!(ys.len() >= 2, "bracket pieces: {ys:?}");
    assert!(
        span > 0.5 * s.height,
        "pieces span {span} of height {}",
        s.height
    );
}

#[test]
fn cases_columns_are_a_quad_apart() {
    // As in amsmath: without the gap, `-x & x < 0` ran together as "-xx < 0".
    let split = typeset(r"\begin{cases} -x & y \end{cases}", true);
    let joined = typeset(r"\begin{cases} -x y \end{cases}", true);
    let em = split.glyphs[0].size;
    let gap = split.width - joined.width;
    assert!(gap > 0.9 * em && gap < 1.1 * em, "gap {gap}, em {em}");
}

#[test]
fn scene_inline() {
    insta::assert_snapshot!(typeset(r"\pi r^2", false).describe());
}

#[test]
fn corpus_typesets() {
    let world = MathWorld::new(FONTS);
    let corpus: Vec<&str> = include_str!("corpus.txt")
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert!(corpus.len() >= 200, "{} expressions", corpus.len());
    let mut failures = Vec::new();
    for latex in &corpus {
        for display in [false, true] {
            match world.typeset(latex, display) {
                Ok(s) if s.glyphs.is_empty() && s.rules.is_empty() => {
                    failures.push(format!("{latex}: empty"))
                }
                Ok(s) if s.glyphs.iter().any(|g| g.id == 0) => {
                    failures.push(format!("{latex}: missing glyph"))
                }
                Ok(_) => {}
                Err(e) => failures.push(format!("{latex}: {}", e.message)),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn same_block_macro_is_blackboard_bold() {
    let defined = typeset(r"\newcommand{\R}{\mathbb{R}} x \in \R", true);
    let direct = typeset(r"x \in \mathbb{R}", true);
    assert_eq!(defined.glyphs, direct.glyphs);
}

#[test]
fn unknown_command_is_an_error_naming_it() {
    let err = MathWorld::new(FONTS).typeset(r"\foo{x}", true).unwrap_err();
    assert!(err.message.contains(r"\foo"), "{}", err.message);
}

#[test]
fn repeated_equation_compiles_once() {
    let cache = Cache::new(FONTS);
    let batch: Vec<(String, bool)> = (0..50).map(|_| (r"\sum_i x_i".into(), true)).collect();
    cache.compute_batch(&batch);
    assert_eq!(cache.compiles(), 1);
    assert!(cache.get(r"\sum_i x_i", true).unwrap().is_ok());
    // The inline form is a different result.
    assert!(cache.get(r"\sum_i x_i", false).is_none());
}

/// Fuzzing found a stack overflow in mitex on long runs of nesting. Up to the length limit,
/// every shape of deep nesting gives a result or an error on a test thread's 2 MB stack.
#[test]
fn deep_nesting_does_not_overflow_the_stack() {
    let crash = format!("?\r\r\r{}\r\r\r_____", "_".repeat(1100));
    let n = mdt_math::MAX_SOURCE;
    let inputs = [
        crash,
        "_".repeat(n),
        "{".repeat(n),
        format!("{}x{}", "{".repeat(n / 2 - 1), "}".repeat(n / 2 - 1)),
        "\\frac".repeat(n / 5),
        "\\sqrt".repeat(n / 5),
        "\\left(".repeat(n / 6),
        "x^".repeat(n / 2),
    ];
    let cache = Cache::new(FONTS);
    for latex in &inputs {
        let _ = cache.compute(latex, true);
    }
    let long = "x".repeat(n + 1);
    let err = cache.compute(&long, false).unwrap_err();
    assert!(err.message.contains("too long"), "{}", err.message);
}

/// Typesets on another thread; fails instead of hanging if it takes over `secs` seconds.
fn typeset_within(latex: &str, secs: u64) -> Result<mdt_math::MathScene, mdt_math::MathError> {
    let (tx, rx) = std::sync::mpsc::channel();
    let latex = latex.to_owned();
    std::thread::spawn(move || {
        let _ = tx.send(MathWorld::new(FONTS).typeset(&latex, true));
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs))
        .expect("typesetting did not finish")
}

/// mitex expanded `\newcommand` macros without bound: `\newcommand{\a}{\a\a}\a` (23 bytes)
/// ran until killed, at 3.3 GB after 3.5 minutes. Expansion stops at a budget.
#[test]
fn recursive_macros_stop() {
    let nested_arguments = format!(
        r"\newcommand{{\a}}[1]{{#1#1}}{}x{}",
        r"\a{".repeat(25),
        "}".repeat(25)
    );
    for latex in [
        r"\newcommand{\a}{\a\a}\a",
        r"\newcommand{\a}{\a}\a",
        r"\newenvironment{e}{\begin{e}}{\end{e}}\begin{e}x\end{e}",
        &nested_arguments,
    ] {
        let err = typeset_within(latex, 10).unwrap_err();
        assert!(
            err.message.contains("expand too far"),
            "{latex}: {}",
            err.message
        );
    }
    // A macro used a thousand times is fine.
    let a = r"\newcommand{\a}{\frac{x}{y}+}";
    let b = format!(r"\newcommand{{\b}}{{{}}}", r"\a".repeat(10));
    let c = format!(r"\newcommand{{\c}}{{{}}}", r"\b".repeat(10));
    let many = format!(r"{a}{b}{c}{}", r"\c".repeat(10));
    assert!(typeset_within(&many, 30).is_ok());
}

/// rowan, which builds mitex's syntax trees, rehashed each node's whole subtree, so building
/// took the square of the depth: 15,000 primes on one base (15 KB) took 7.8 s, and macros
/// could make as many from less. The vendored rowan builds in linear time, and trees deeper
/// than mitex's `MAX_TREE_DEPTH` are refused before converting.
#[test]
fn deep_syntax_trees_are_refused_quickly() {
    // `body` a thousand times `n` times, within the lexer's expansion budget.
    let chain = |name: &str, body: &str, n: usize| {
        let a = format!(r"\newcommand{{\{name}a}}{{{}}}", body.repeat(10));
        let b = format!(
            r"\newcommand{{\{name}b}}{{{}}}",
            format!(r"\{name}a").repeat(10)
        );
        let c = format!(
            r"\newcommand{{\{name}c}}{{{}}}",
            format!(r"\{name}b").repeat(10)
        );
        format!(r"{a}{b}{c}{} x", format!(r"\{name}c ").repeat(n))
    };
    let deep = [
        format!("a{}", "'".repeat(15_000)),
        "a^".repeat(8_000),
        // Primes through macros.
        chain("p", "'", 12),
        // A command's `[` argument.
        format!("{}x{}", r"\sqrt[".repeat(1100), "]{2}".repeat(1100)),
        // Commands whose argument is the next command.
        chain("s", r"\sqrt", 12),
        r"\frac{x}".repeat(1500) + "x",
        // A stray `]` does not close a brace group, nor `\end` or `\right` one.
        format!("{}{}", "{(a,b] ".repeat(1100), "}".repeat(1100)),
        r"{\end{a}".repeat(1100),
        // `\right` does not close an environment.
        chain("e", r"\begin{a}\right", 5),
    ];
    for latex in &deep {
        let start = std::time::Instant::now();
        let err = typeset_within(latex, 10).unwrap_err();
        assert!(err.message.contains("nested too deeply"), "{}", err.message);
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(2), "{took:?}");
    }
    // Primes and scripts on many bases do not nest, nor `[` and `(` that are not a
    // command's argument.
    let flat = "f'(x) + g''_1^2 + ".repeat(200);
    let intervals = format!("{}x", r"[0,1) \cup ".repeat(1200));
    for latex in [&flat, &intervals] {
        if let Err(e) = typeset_within(latex, 30) {
            panic!("{}", e.message);
        }
    }
}

/// mitex wrote `\iftypst` blocks, `\includegraphics` options and `\label` text into the
/// Typst source as they were, so a document could run Typst code: a panic, or a
/// gigabyte-sized string. None of them reach Typst now.
#[test]
fn formulas_cannot_run_typst_code() {
    let world = MathWorld::new(FONTS);
    for latex in [
        r#"\iftypst #panic("x") \fi"#,
        r#"\iftypst #("x"*100000000).len() \fi"#,
        r#"\includegraphics[width=#panic("x")]{a}"#,
        r#"\includegraphics[width=("x"*100000000).len()*1pt]{a}"#,
        r#"\text{\label{x> #panic("x") <y}}"#,
    ] {
        // The label is dropped, leaving an empty formula; the others are errors.
        if let Err(e) = world.typeset(latex, true) {
            assert!(!e.message.contains("panicked"), "{latex}: {}", e.message);
        }
    }
}

/// mitex's scope ran the argument of `\hspace`, `\vspace` and `\raisebox` with `eval`, so
/// its letters ran as Typst code: a panic, ten billion loop iterations in 60 bytes, or a
/// string that exhausts memory. The argument is parsed as a length now.
#[test]
fn length_arguments_are_parsed_not_run() {
    let world = MathWorld::new(FONTS);
    let start = std::time::Instant::now();
    for latex in [
        r#"\hspace{panic("PWNED")}"#,
        r#"\hspace{0pt*range(100000).map(i=>range(100000).len()).sum()}"#,
        r#"\vspace{("x"*4000000000).len()*1pt}"#,
        r#"\raisebox{panic("PWNED")*1pt}{x}"#,
    ] {
        if let Err(e) = world.typeset(latex, true) {
            assert!(!e.message.contains("panicked"), "{latex}: {}", e.message);
        }
    }
    assert!(start.elapsed().as_secs() < 5, "took {:?}", start.elapsed());
}

#[test]
fn length_arguments_still_space_and_raise() {
    let em = typeset("x", false).glyphs[0].size;
    let plain = typeset("ab", false).width;
    let wide = typeset(r"a\hspace{1em}b", false).width - plain;
    assert!((wide - em).abs() < 0.05 * em, "1em gave {wide}, em {em}");
    let half = typeset(r"a\hspace{.5em}b", false).width - plain;
    assert!((half - em / 2.0).abs() < 0.05 * em, ".5em gave {half}");
    let back = typeset(r"a\hspace{-1em}b", false).width - plain;
    assert!((back + em).abs() < 0.05 * em, "-1em gave {back}");
    let mm = typeset(r"a\hspace{3mm}b", false).width - plain;
    assert!(
        (mm - 3.0 * 72.0 / 25.4 * em / 10.0).abs() < 0.05 * em,
        "3mm gave {mm}"
    );
    typeset(r"a\vspace{2pt}b", true);
    let low = typeset("x", false).glyphs[0].y;
    let high = typeset(r"\raisebox{1pt}{x}", false).glyphs[0].y;
    assert!(
        (low - high - em / 10.0).abs() < 0.01 * em,
        "raised {}",
        low - high
    );
}

/// `\hspace*{1em}` took the `*` as its argument (no length) and printed "1em"; TeX also
/// reads units in any case, `1.em`, and glue as its natural length.
#[test]
fn starred_spaces_and_tex_length_forms() {
    let em = typeset("x", false).glyphs[0].size;
    let plain = typeset("ab", false).width;
    for latex in [
        r"a\hspace*{1em}b",
        r"a\hspace{1EM}b",
        r"a\hspace{1.em}b",
        r"a\hspace{1em plus 1fil minus 2pt}b",
    ] {
        let scene = typeset(latex, false);
        let wide = scene.width - plain;
        assert!(
            (wide - em).abs() < 0.05 * em,
            "{latex} gave {wide}, em {em}"
        );
        assert_eq!(scene.glyphs.len(), 2, "{latex} printed its argument");
    }
    let scene = typeset(r"a\vspace*{2pt}b", true);
    assert_eq!(scene.glyphs.len(), 2, "vspace* printed its argument");
}

/// These panicked in mitex (`expect` and unchecked slicing); found once the fuzz target
/// used md-tex's command spec.
#[test]
fn malformed_commands_are_errors_not_panics() {
    let world = MathWorld::new(FONTS);
    for latex in [
        r"\label",
        r"\label{",
        r"\label x",
        r"\includegraphics",
        r"\includegraphics{",
        r"\text{\begin{tabular}\end{tabular}}",
        r"\text{\begin{tabular}{l\end{tabular}}",
        r"\text{\begin{figure}\caption\end{figure}}",
    ] {
        assert!(world.typeset(latex, true).is_err(), "{latex}");
    }
}

#[test]
fn cache_keeps_the_most_recent_results_within_its_bytes() {
    let cache = Cache::with_capacity(FONTS, 8 * 1024);
    for i in 0..60 {
        let _ = cache.compute(&format!("x^{{{i}}} + y_{{{i}}}"), true);
        assert!(cache.bytes() <= 8 * 1024, "{} bytes", cache.bytes());
    }
    assert!(cache.get("x^{59} + y_{59}", true).is_some());
    assert!(
        cache.get("x^{0} + y_{0}", true).is_none(),
        "the oldest went"
    );
}
