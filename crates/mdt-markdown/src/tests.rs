use crate::*;

fn kinds(m: &BlockModel) -> Vec<UnitKind> {
    m.units.iter().map(|u| u.kind).collect()
}

fn paragraph(m: &BlockModel, unit: usize) -> &Inline {
    match &m.units[unit].block.kind {
        BlockKind::Paragraph { content, .. } => content,
        other => panic!("not a paragraph: {other:?}"),
    }
}

fn styles(inline: &Inline) -> Vec<(&str, &SpanStyle)> {
    inline
        .spans
        .iter()
        .map(|s| (&inline.text[s.range.clone()], &s.style))
        .collect()
}

// 4.1 Layout units

#[test]
fn unit_kinds_and_byte_ranges() {
    let src = "# Title\n\nPara one\nstill one.\n\n- a\n- b\n\n> quote\n\n```rust\nfn x() {}\n```\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n$$\nx\n$$\n";
    let m = parse(src);
    assert_eq!(
        kinds(&m),
        [
            UnitKind::Heading,
            UnitKind::Paragraph,
            UnitKind::List,
            UnitKind::BlockQuote,
            UnitKind::Code,
            UnitKind::Rule,
            UnitKind::Table,
            UnitKind::Paragraph,
        ]
    );
    let slices: Vec<&str> = m.units.iter().map(|u| &src[u.range.clone()]).collect();
    assert_eq!(slices[0], "# Title");
    assert_eq!(slices[1], "Para one\nstill one.");
    assert_eq!(slices[2], "- a\n- b");
    assert_eq!(slices[4], "```rust\nfn x() {}\n```");
    assert_eq!(slices[7], "$$\nx\n$$");
}

#[test]
fn hashes_follow_content() {
    let a = parse("# A\n\npara\n");
    let b = parse("# A\n\nother para\n\n# A\n");
    assert_eq!(a.units[0].hash, b.units[0].hash);
    assert_ne!(a.units[1].hash, b.units[1].hash);
    assert_eq!(a.units[0].hash, b.units[2].hash);
}

#[test]
fn ten_thousand_item_list_splits_into_one_unit_per_item() {
    let src: String = (0..10_000)
        .map(|i| format!("{}. item {i}\n", i + 1))
        .collect();
    let m = parse(&src);
    assert_eq!(m.units.len(), 10_000);
    assert!(m.units.iter().all(|u| u.kind == UnitKind::ListItem));
    let BlockKind::List { start, items, .. } = &m.units[1234].block.kind else {
        panic!()
    };
    assert_eq!(*start, Some(1235));
    assert_eq!(&src[items[0].range.clone()], "1235. item 1234");
    assert_eq!(&src[m.units[1234].range.clone()], "1235. item 1234");
}

#[test]
fn small_list_stays_one_unit() {
    let src: String = (0..SPLIT_THRESHOLD).map(|i| format!("- {i}\n")).collect();
    assert_eq!(kinds(&parse(&src)), [UnitKind::List]);
}

#[test]
fn large_blockquote_splits() {
    let src: String = (0..40).map(|i| format!("> para {i}\n>\n")).collect();
    let m = parse(&src);
    assert_eq!(m.units.len(), 40);
    assert!(m.units.iter().all(|u| u.kind == UnitKind::BlockQuoteChild));
}

#[test]
fn tight_and_loose_lists() {
    let BlockKind::List { tight, items, .. } = &parse("- a\n- b\n").units[0].block.kind else {
        panic!()
    };
    assert!(*tight);
    assert!(matches!(
        items[0].children[0].kind,
        BlockKind::Paragraph { implicit: true, .. }
    ));
    let BlockKind::List { tight, .. } = &parse("- a\n\n- b\n").units[0].block.kind else {
        panic!()
    };
    assert!(!*tight);
}

// 4.2 Inline spans and source maps

fn round_trip(src: &str, rendered_word: &str, source_word: &str) {
    let m = parse(src);
    let inline = paragraph(&m, 0);
    let r = inline
        .text
        .find(rendered_word)
        .unwrap_or_else(|| panic!("{rendered_word:?} not in {:?}", inline.text));
    let s = src.find(source_word).unwrap();
    for i in 0..rendered_word.len() {
        assert_eq!(
            inline.source_offset(r + i),
            Some(s + i),
            "rendered→source at {i} in {src:?}"
        );
        assert_eq!(
            inline.rendered_offset(s + i),
            r + i,
            "source→rendered at {i} in {src:?}"
        );
    }
}

#[test]
fn emphasis_strong_code_link_strikethrough_round_trip() {
    round_trip("say *hello* now", "hello", "hello");
    round_trip("say **bold** now", "bold", "bold");
    round_trip("call `run()` here", "run()", "run()");
    round_trip("see [the docs](https://x.y) here", "the docs", "the docs");
    round_trip("old ~~gone~~ new", "gone", "gone");
    round_trip("after **bold** text", "text", "text");
}

#[test]
fn styles_are_recorded() {
    let m = parse("a *em* **st** `co` ~~del~~ [ln](u)");
    let inline = paragraph(&m, 0);
    assert_eq!(inline.text, "a em st co del ln");
    let s = styles(inline);
    assert!(s.contains(&("em", &SpanStyle::Emphasis)));
    assert!(s.contains(&("st", &SpanStyle::Strong)));
    assert!(s.contains(&("co", &SpanStyle::Code)));
    assert!(s.contains(&("del", &SpanStyle::Strikethrough)));
    assert!(s.contains(&(
        "ln",
        &SpanStyle::Link {
            dest: "u".into(),
            title: "".into()
        }
    )));
}

#[test]
fn inline_math_round_trip() {
    let src = r"the area is $\pi r^2$ units";
    let m = parse(src);
    let inline = paragraph(&m, 0);
    assert_eq!(inline.text, "the area is \u{FFFC} units");
    let obj = &inline.objects[0];
    assert_eq!(
        obj.kind,
        ObjectKind::Math {
            latex: r"\pi r^2".into(),
            display: false
        }
    );
    assert_eq!(&src[obj.source.clone()], r"$\pi r^2$");
    assert_eq!(inline.source_offset(obj.at), Some(obj.source.start));
    assert_eq!(inline.rendered_offset(obj.source.start), obj.at);
    assert_eq!(
        inline.rendered_offset(obj.source.start + 3),
        obj.at,
        "inside math snaps to the object"
    );
    let units_at = inline.text.find("units").unwrap();
    assert_eq!(
        inline.source_offset(units_at),
        Some(src.find("units").unwrap())
    );
}

/// `source_offset` binary-searches the segment map; it answers as a scan from the start
/// (the containing segment, else the one ending there) would, at every rendered offset.
#[test]
fn source_offset_matches_a_linear_scan() {
    let src = "a &amp; \\* *em* `co` $x$ [l **b**](u) ~~d~~ e&copy;";
    let m = parse(src);
    let inline = paragraph(&m, 0);
    assert!(inline.map.len() > 8, "{:?}", inline.map);
    let linear = |r: usize| {
        let seg = inline
            .map
            .iter()
            .find(|s| s.rendered.contains(&r))
            .or_else(|| inline.map.iter().rev().find(|s| s.rendered.end == r))?;
        Some(if seg.rendered.len() == seg.source.len() {
            seg.source.start + (r - seg.rendered.start)
        } else if r == seg.rendered.start {
            seg.source.start
        } else {
            seg.source.end
        })
    };
    for r in 0..=inline.text.len() + 1 {
        assert_eq!(inline.source_offset(r), linear(r), "at {r}");
    }
}

#[test]
fn markup_offsets_snap_to_the_next_rendered_character() {
    let src = "a **b** c";
    let m = parse(src);
    let inline = paragraph(&m, 0);
    assert_eq!(inline.rendered_offset(2), inline.text.find('b').unwrap());
}

// 4.4 GFM extensions

#[test]
fn table_alignment() {
    let m = parse("| l | c | r |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n");
    let BlockKind::Table { alignments, rows } = &m.units[0].block.kind else {
        panic!()
    };
    assert_eq!(
        alignments,
        &[Alignment::Left, Alignment::Center, Alignment::Right]
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1][2].text, "3");
}

#[test]
fn task_items_with_checkbox_range() {
    let src = "- [ ] todo\n- [x] done\n";
    let m = parse(src);
    let BlockKind::List { items, .. } = &m.units[0].block.kind else {
        panic!()
    };
    let t0 = items[0].task.clone().unwrap();
    let t1 = items[1].task.clone().unwrap();
    assert!(!t0.checked && t1.checked);
    assert_eq!(&src[t0.range], "[ ]");
    assert_eq!(&src[t1.range], "[x]");
}

#[test]
fn strikethrough() {
    let m = parse("~~gone~~");
    assert_eq!(
        styles(paragraph(&m, 0)),
        [("gone", &SpanStyle::Strikethrough)]
    );
}

#[test]
fn footnotes_number_by_first_reference_and_gather_at_end() {
    let src = "[^b]: second\n\nText[^a] and[^b].\n\n[^a]: first note\n\n# After\n";
    let m = parse(src);
    assert_eq!(
        kinds(&m),
        [UnitKind::Paragraph, UnitKind::Heading, UnitKind::Footnotes]
    );
    let inline = paragraph(&m, 0);
    assert_eq!(inline.text, "Text1 and2.");
    assert!(styles(inline).contains(&("1", &SpanStyle::FootnoteRef { label: "a".into() })));
    let BlockKind::Footnotes { notes } = &m.units[2].block.kind else {
        panic!()
    };
    assert_eq!(
        notes
            .iter()
            .map(|n| (n.label.as_str(), n.number))
            .collect::<Vec<_>>(),
        [("a", 1), ("b", 2)]
    );
    assert_eq!(m.units[2].range, src.len()..src.len());
}

#[test]
fn five_alert_types() {
    for (tag, kind) in [
        ("NOTE", AlertKind::Note),
        ("TIP", AlertKind::Tip),
        ("IMPORTANT", AlertKind::Important),
        ("WARNING", AlertKind::Warning),
        ("CAUTION", AlertKind::Caution),
    ] {
        let m = parse(&format!("> [!{tag}]\n> Body text.\n"));
        let BlockKind::BlockQuote { alert, children } = &m.units[0].block.kind else {
            panic!()
        };
        assert_eq!(*alert, Some(kind), "{tag}");
        let BlockKind::Paragraph { content, .. } = &children[0].kind else {
            panic!()
        };
        assert_eq!(content.text, "Body text.");
    }
}

#[test]
fn yaml_and_toml_front_matter() {
    let m = parse("---\ntitle: Hi\n---\n\n# Heading\n");
    assert_eq!(kinds(&m), [UnitKind::FrontMatter, UnitKind::Heading]);
    let BlockKind::FrontMatter { kind, text } = &m.units[0].block.kind else {
        panic!()
    };
    assert_eq!(
        (*kind, text.as_str()),
        (MetadataBlockKind::YamlStyle, "title: Hi\n")
    );
    let m = parse("+++\ntitle = \"Hi\"\n+++\n\ntext\n");
    assert_eq!(kinds(&m), [UnitKind::FrontMatter, UnitKind::Paragraph]);
}

// 4.5 Math

fn objects(src: &str) -> Vec<ObjectKind> {
    let m = parse(src);
    paragraph(&m, 0)
        .objects
        .iter()
        .map(|o| o.kind.clone())
        .collect()
}

#[test]
fn math_delimiters() {
    assert_eq!(
        objects(r"the area is $\pi r^2$ units"),
        [ObjectKind::Math {
            latex: r"\pi r^2".into(),
            display: false
        }]
    );
    assert_eq!(
        objects(r"$$\int_0^1 x^2\,dx$$"),
        [ObjectKind::Math {
            latex: r"\int_0^1 x^2\,dx".into(),
            display: true
        }]
    );
}

#[test]
fn formulas_past_the_limit_stay_source() {
    use crate::build::MAX_MATH_PER_UNIT;
    let src = "$x$ ".repeat(MAX_MATH_PER_UNIT + 3);
    let m = parse(&format!("{src}\n\n$y$ and $$z$$\n"));
    let p = paragraph(&m, 0);
    assert_eq!(p.objects.len(), MAX_MATH_PER_UNIT);
    assert!(
        p.text.ends_with("$x$ $x$ $x$"),
        "{}",
        &p.text[p.text.len() - 20..]
    );
    // The count starts again in the next unit.
    assert_eq!(paragraph(&m, 1).objects.len(), 2);
}

#[test]
fn not_math() {
    for src in ["$ y=x$", "$1$2$3"] {
        let m = parse(src);
        let inline = paragraph(&m, 0);
        assert!(inline.objects.is_empty(), "{src}: {:?}", inline.objects);
        assert_eq!(inline.text, src);
    }
}

#[test]
fn closing_dollar_before_whitespace_is_literal() {
    let m = parse("cost $5 and $x $ here");
    assert!(paragraph(&m, 0).objects.is_empty());
}

#[test]
fn valid_math_next_to_rejected_math() {
    let m = parse("$1$2 and $x$.");
    let inline = paragraph(&m, 0);
    assert_eq!(inline.text, "$1$2 and \u{FFFC}.");
    assert_eq!(
        inline.objects[0].kind,
        ObjectKind::Math {
            latex: "x".into(),
            display: false
        }
    );
}

#[test]
fn escaped_dollar() {
    let m = parse(r"costs \$5");
    assert_eq!(paragraph(&m, 0).text, "costs $5");
}

#[test]
fn math_fence() {
    let m = parse("```math\na^2 + b^2\n```\n");
    assert_eq!(
        m.units[0].block.kind,
        BlockKind::Math {
            latex: "a^2 + b^2".into()
        }
    );
}

#[test]
fn math_in_table_cell_and_link_text() {
    let m = parse("| f |\n|---|\n| $x^2$ |\n");
    let BlockKind::Table { rows, .. } = &m.units[0].block.kind else {
        panic!()
    };
    assert_eq!(
        rows[1][0].objects[0].kind,
        ObjectKind::Math {
            latex: "x^2".into(),
            display: false
        }
    );
    let m = parse("[see $x$ here](u)");
    let inline = paragraph(&m, 0);
    assert_eq!(
        inline.objects[0].kind,
        ObjectKind::Math {
            latex: "x".into(),
            display: false
        }
    );
    let link = inline
        .spans
        .iter()
        .find(|s| matches!(s.style, SpanStyle::Link { .. }))
        .unwrap();
    assert!(link.range.contains(&inline.objects[0].at));
}

// 4.6 HTML

#[test]
fn img_tag_with_width() {
    let m = parse(r#"<img src="a.png" width="200">"#);
    let obj = &paragraph(&m, 0).objects[0];
    let ObjectKind::Image { src, width, .. } = &obj.kind else {
        panic!()
    };
    assert_eq!((src.as_str(), *width), ("a.png", Some(200)));
    let m = parse(r#"Inline <img src="b.png" width="20"> image and line<br>break"#);
    let inline = paragraph(&m, 0);
    assert!(matches!(
        inline.objects[0].kind,
        ObjectKind::Image {
            width: Some(20),
            ..
        }
    ));
    assert!(matches!(
        inline.objects[1].kind,
        ObjectKind::HtmlBreak { .. }
    ));
}

#[test]
fn other_html_is_literal() {
    let m = parse("<div style=\"color:red\">text</div>\n");
    assert_eq!(
        m.units[0].block.kind,
        BlockKind::Html {
            text: "<div style=\"color:red\">text</div>\n".into()
        }
    );
    let m = parse("a <span style=\"x\">b</span> c");
    let inline = paragraph(&m, 0);
    assert_eq!(inline.text, "a <span style=\"x\">b</span> c");
    assert_eq!(
        styles(inline),
        [
            ("<span style=\"x\">", &SpanStyle::Html),
            ("</span>", &SpanStyle::Html)
        ]
    );
}

// 4.7 Slugs

#[test]
fn heading_slugs_and_duplicates() {
    let m = parse("# Getting Started\n\n## Notes\n\n## Notes\n\n### Notes\n");
    let slugs: Vec<&str> = m.headings.iter().map(|h| h.slug.as_str()).collect();
    assert_eq!(slugs, ["getting-started", "notes", "notes-1", "notes-2"]);
    assert_eq!(m.headings[0].text, "Getting Started");
    assert_eq!(m.headings[2].unit, 2);
}

#[test]
fn heading_text_includes_math_source_and_code() {
    let m = parse("# Area is $\\pi r^2$ in `code`\n");
    assert_eq!(m.headings[0].text, "Area is \\pi r^2 in code");
}

// 4.8 Safe prefix

#[test]
fn prefix_never_ends_inside_a_fence_html_or_math() {
    let filler = "word ".repeat(30);
    let src = format!(
        "{filler}\n\n```\ncode\n\nmore code after blank\n```\n\n<!--\ncomment\n\nstill comment\n-->\n\n$$\nx\n$$\n\n<div>\nhtml\n\nafter\n\n{filler}\n\n# Tail\n"
    );
    let fence_open = src.find("```").unwrap();
    let fence_close = src.rfind("```").unwrap();
    for min in 0..src.len() {
        let end = prefix::safe_prefix_end(&src, min);
        assert!(end >= min.min(src.len()));
        assert!(
            !(fence_open < end && end <= fence_close),
            "min {min}: cut {end} inside fence"
        );
        let comment = src.find("<!--").unwrap()..src.find("-->").unwrap();
        assert!(
            !(comment.start < end && end <= comment.end),
            "min {min}: cut {end} inside comment"
        );
        let div = src.find("<div>").unwrap();
        let after = src.find("after").unwrap();
        assert!(
            !(div < end && end <= after),
            "min {min}: cut {end} inside html block"
        );
    }
}

#[test]
fn prefix_units_match_full_units() {
    let mut src = String::from("---\ntitle: x\n---\n\n");
    for i in 0..800 {
        src.push_str(&format!("## Section {i}\n\n[l{i}]: /u{i}\n\n[^n{i}]: Note {i}.\n\nText with *em* and $x_{i}$, a note[^n{i}] and [a link][l{i}].\n\n- item\n- item\n\n  continued\n\n```\na\n\nb\n```\n\n> quote\n>\n> more\n\n"));
    }
    // Definitions come before their uses: the prefix parse sees them too.
    let full = parse(&src);
    // The footnotes section sits at the end of whatever was parsed.
    let body = |m: &BlockModel, end: usize| -> Vec<LayoutUnit> {
        m.units
            .iter()
            .filter(|u| u.kind != UnitKind::Footnotes && u.range.end <= end)
            .cloned()
            .collect()
    };
    for min in [0, 100, 5_000, 64 * 1024] {
        let end = prefix::safe_prefix_end(&src, min);
        assert!(end >= min && end < src.len(), "min {min} end {end}");
        let prefix = parse(&src[..end]);
        assert_eq!(
            body(&prefix, end),
            body(&full, end),
            "min {min}, cut at {end}"
        );
    }
}

#[test]
fn prefix_without_a_later_definition_hashes_differently() {
    let mut src = String::from("See [x] here.\n\n");
    while src.len() < 70_000 {
        src.push_str("Filler paragraph text.\n\n");
    }
    src.push_str("[x]: https://example.com\n");
    let end = prefix::safe_prefix_end(&src, 64 * 1024);
    assert!(end < src.len());
    let (full, prefix) = (parse(&src), parse(&src[..end]));
    assert_eq!(prefix.units[0].range, full.units[0].range);
    assert_ne!(
        prefix.units[0].hash, full.units[0].hash,
        "the link only resolves in the full parse"
    );
    assert_eq!(prefix.units[1].hash, full.units[1].hash);
}

#[test]
fn hashes_cover_footnote_numbers() {
    let a = parse("First[^a].\n\nSecond[^b].\n\n[^a]: a\n\n[^b]: b\n");
    let b = parse("First.\n\nSecond[^b].\n\n[^b]: b\n");
    assert_eq!(paragraph(&a, 1).text, "Second2.");
    assert_eq!(paragraph(&b, 1).text, "Second1.");
    assert_ne!(a.units[1].hash, b.units[1].hash);
}

#[test]
fn hashes_cover_link_reference_definitions() {
    let a = parse("See [x].\n\n[x]: https://a.example\n");
    let b = parse("See [x].\n\n[x]: https://b.example\n");
    let undefined = parse("See [x].\n");
    assert_ne!(a.units[0].hash, b.units[0].hash);
    assert_ne!(a.units[0].hash, undefined.units[0].hash);
}

#[test]
fn split_list_items_hash_their_numbers() {
    let n = SPLIT_THRESHOLD + 8;
    let m = parse(&"1. same\n".repeat(n));
    assert_eq!(m.units.len(), n);
    let mut hashes: Vec<u64> = m.units.iter().map(|u| u.hash).collect();
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(hashes.len(), n, "each item shows its own number");
}

fn nesting_depth(block: &Block) -> usize {
    let children: Vec<&Block> = match &block.kind {
        BlockKind::List { items, .. } => items.iter().flat_map(|i| &i.children).collect(),
        BlockKind::BlockQuote { children, .. } => children.iter().collect(),
        _ => Vec::new(),
    };
    1 + children.into_iter().map(nesting_depth).max().unwrap_or(0)
}

fn paragraph_texts(block: &Block, out: &mut Vec<String>) {
    match &block.kind {
        BlockKind::Paragraph { content, .. } => out.push(content.text.clone()),
        BlockKind::List { items, .. } => items
            .iter()
            .flat_map(|i| &i.children)
            .for_each(|c| paragraph_texts(c, out)),
        BlockKind::BlockQuote { children, .. } => {
            children.iter().for_each(|c| paragraph_texts(c, out))
        }
        _ => {}
    }
}

#[test]
fn deep_nesting_is_flattened_and_fits_a_small_stack() {
    let docs = [
        format!("{} deep\n", ">".repeat(100_000)),
        format!("{}deep\n", "- ".repeat(100_000)),
        format!("{}deep\n", "> 1. ".repeat(30_000)),
    ];
    for src in docs {
        // Background parses run on threads with small stacks (512 KiB on macOS).
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let m = parse(&src);
                assert_eq!(m.units.len(), 1);
                let block = &m.units[0].block;
                let depth = nesting_depth(block);
                assert!(depth <= crate::build::MAX_DEPTH + 1, "depth {depth}");
                let mut texts = Vec::new();
                paragraph_texts(block, &mut texts);
                assert_eq!(texts, ["deep"]);
                assert_eq!(m.clone(), m);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

/// Hostile inline nesting: emphasis, strong, strikethrough, links and images 50,000 deep.
fn deep_inline_documents() -> Vec<String> {
    let n = 50_000;
    let wrap = |open: &str, close: &str| format!("{}x{}\n", open.repeat(n), close.repeat(n));
    vec![
        wrap("*a ", " a*"),
        wrap("**a ", " a**"),
        wrap("~~a ", " a~~"),
        wrap("[a ", " a](u)"),
        wrap("![a ", " a](u)"),
        wrap("*[~~a ", " a~~](u)*"),
    ]
}

#[test]
fn deep_inline_nesting_stays_flat_and_fits_a_small_stack() {
    for src in deep_inline_documents() {
        // Spans are a flat list with ranges, so nesting depth costs no stack anywhere.
        let spans = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let m = parse(&src);
                let BlockKind::Paragraph { content, .. } = &m.units[0].block.kind else {
                    panic!("one paragraph")
                };
                let spans = content.spans.len() + content.objects.len();
                assert_eq!(m.clone(), m);
                spans
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(spans >= 1);
    }
    let emphasis = parse(&deep_inline_documents()[0]);
    let BlockKind::Paragraph { content, .. } = &emphasis.units[0].block.kind else {
        panic!()
    };
    assert_eq!(content.spans.len(), 50_000, "every level reaches the model");
}

#[test]
fn rule_inside_a_tight_list_item_is_kept() {
    let m = parse("- a\n  ***\n- b\n");
    let BlockKind::List { items, .. } = &m.units[0].block.kind else {
        panic!()
    };
    let rules: Vec<bool> = items[0]
        .children
        .iter()
        .map(|c| c.kind == BlockKind::Rule)
        .collect();
    assert_eq!(rules, [false, true]);
}

#[test]
fn html_block_tags_map_to_their_source() {
    for src in [
        "> <img src=\"a.png\">\n> <img src=\"b.png\">\n",
        "  <img src=\"a.png\">",
        "<img src=\"a.png\">\n<br>\n<img src=\"a.png\">\n",
    ] {
        let m = parse(src);
        let content = match &m.units[0].block.kind {
            BlockKind::BlockQuote { children, .. } => match &children[0].kind {
                BlockKind::Paragraph { content, .. } => content.clone(),
                other => panic!("{other:?}"),
            },
            _ => paragraph(&m, 0).clone(),
        };
        let mut last = 0;
        for obj in &content.objects {
            let html = match &obj.kind {
                ObjectKind::Image { html, .. } => html.clone().unwrap(),
                ObjectKind::HtmlBreak { html } => html.clone(),
                other => panic!("{other:?}"),
            };
            assert_eq!(src.get(obj.source.clone()), Some(html.as_str()), "{src:?}");
            assert!(obj.source.start >= last, "in order in {src:?}");
            last = obj.source.end;
        }
    }
}

#[test]
fn heading_text_and_slug_skip_footnote_numbers() {
    let m = parse("# Intro[^1]\n\n[^1]: note\n");
    assert_eq!(m.headings[0].text, "Intro");
    assert_eq!(m.headings[0].slug, "intro");
}

#[test]
fn duplicate_footnote_definitions_keep_the_first() {
    let m = parse("Text[^a].\n\n[^a]: one\n\n[^a]: two\n");
    let BlockKind::Footnotes { notes } = &m.units.last().unwrap().block.kind else {
        panic!()
    };
    assert_eq!(notes.len(), 1);
    let BlockKind::Paragraph { content, .. } = &notes[0].children[0].kind else {
        panic!()
    };
    assert_eq!(content.text, "one");
}

#[test]
fn prefix_does_not_split_a_loose_list() {
    let mut src = "x\n\n".to_string();
    src.push_str(&"- item\n\n".repeat(50));
    src.push_str("end\n");
    let end = prefix::safe_prefix_end(&src, 10);
    assert!(end == 3 || end == src.len() - 4, "cut at {end}");
}

#[test]
fn footnote_scenario() {
    let m = parse("Text[^1].\n\n[^1]: note\n");
    let inline = paragraph(&m, 0);
    assert_eq!(
        styles(inline),
        [("1", &SpanStyle::FootnoteRef { label: "1".into() })]
    );
    let Some(LayoutUnit {
        block: Block {
            kind: BlockKind::Footnotes { notes },
            ..
        },
        ..
    }) = m.units.last()
    else {
        panic!()
    };
    let BlockKind::Paragraph { content, .. } = &notes[0].children[0].kind else {
        panic!()
    };
    assert_eq!(content.text, "note");
}
