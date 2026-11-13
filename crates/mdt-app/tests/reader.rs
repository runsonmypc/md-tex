//! Reader virtualization on the 10 MB document (`harness = false`: GPUI's macOS text
//! system must be created on the main thread).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::HeadlessAppContext;
use mdt_app::reader::view::{Reader, fonts};

fn scroll_through_10_mb_document() {
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let model = mdt_markdown::parse(&mdt_bench::generate::text_10m());
    let units = model.units.len();
    let mut reader = Reader::new(model, 1.0);
    let viewport = 1000.0;
    reader.resize(720.0, viewport);
    let mut frames = Vec::new();
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        // Scroll 300 viewports, a quarter viewport per frame, like a fast fling.
        for _ in 0..1200 {
            reader.scroll_by(viewport / 4.0);
            let start = Instant::now();
            let visible = reader.prepare(&mut fonts, app);
            frames.push(start.elapsed());
            assert!(!visible.is_empty());
            // The visible units cover the viewport (or reach the document end).
            let top = reader.anchor.scroll_y(&reader.heights);
            let last = visible.last().unwrap().0;
            assert!(
                reader.heights.top(last + 1) >= top + viewport as f64
                    || last + 1 == reader.heights.len(),
                "visible units do not cover the viewport"
            );
        }
    });
    frames.sort();
    let p50 = frames[frames.len() / 2];
    let p95 = frames[frames.len() * 95 / 100];
    let max = *frames.last().unwrap();
    println!("  {units} units; frame layout p50 {p50:?}, p95 {p95:?}, max {max:?}");
    // Budget: visible layout plus 2 ms of overscan. Debug builds are several times slower,
    // so the hard limit applies to release builds (the harness measures the real numbers).
    if !cfg!(debug_assertions) {
        assert!(p95 < Duration::from_millis(8), "p95 frame layout {p95:?}");
    }
}

fn wide_table_scrolls_inside_its_block() {
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let header: String = (0..30)
        .map(|i| format!("| column {i} "))
        .collect::<String>()
        + "|\n";
    let rule: String = "|---".repeat(30) + "|\n";
    let row: String = (0..30).map(|i| format!("| value {i} ")).collect::<String>() + "|\n";
    let doc = format!("Intro paragraph.\n\n{header}{rule}{row}{row}\nAfter the table.\n");
    let mut reader = Reader::new(mdt_markdown::parse(&doc), 1.0);
    reader.resize(600.0, 800.0);
    reader.origin = (100.0, 50.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
    });
    let table_y = reader.origin.1 + reader.heights.top(1) as f32 + 20.0;
    let anchor = reader.anchor;
    assert!(
        reader.scroll_horizontal((300.0, table_y), 120.0),
        "table did not take the scroll"
    );
    assert_eq!(reader.hscroll[&(1, 0)], 120.0);
    assert_eq!(reader.anchor, anchor, "page offset changed");
    // Clamped at the table's right edge.
    assert!(reader.scroll_horizontal((300.0, table_y), 1e6));
    let max = reader.hscroll[&(1, 0)];
    assert!(max > 1000.0 && max < 1e6, "max {max}");
    assert!(reader.scroll_horizontal((300.0, table_y), -1e6));
    assert_eq!(reader.hscroll[&(1, 0)], 0.0);
    // A paragraph does not scroll sideways.
    assert!(!reader.scroll_horizontal((300.0, reader.origin.1 + 5.0), 50.0));
    assert_eq!(reader.anchor, anchor);
    // A new column width (window resize, wide reader) resets the sideways scroll, which
    // could otherwise leave a table that now fits stuck shifted left.
    assert!(reader.scroll_horizontal((300.0, table_y), 120.0));
    reader.resize(900.0, 800.0);
    assert!(reader.hscroll.is_empty());
}

/// A top-level table that cannot fit scrolls in a region as wide as the page, which
/// reaches past the column into the margin; a page change resets its sideways scroll.
fn wide_table_scrolls_across_the_page() {
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let header: String = (0..40).map(|i| format!("| column{i} ")).collect::<String>() + "|\n";
    let rule: String = "|---".repeat(40) + "|\n";
    let row: String = (0..40).map(|i| format!("| value{i} ")).collect::<String>() + "|\n";
    let doc = format!("Intro paragraph.\n\n{header}{rule}{row}\nAfter the table.\n");
    let mut reader = Reader::new(mdt_markdown::parse(&doc), 1.0);
    reader.resize(600.0, 800.0);
    reader.set_page(-200.0, 1000.0);
    reader.origin = (300.0, 50.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
    });
    let table_y = reader.origin.1 + reader.heights.top(1) as f32 + 20.0;
    // 150 px left of the column, inside the page.
    let in_margin = (reader.origin.0 - 150.0, table_y);
    assert!(
        reader.scroll_horizontal(in_margin, 80.0),
        "margin did not scroll the table"
    );
    assert!(
        !reader.scroll_horizontal((reader.origin.0 - 250.0, table_y), 80.0),
        "outside the page scrolled the table"
    );
    reader.set_page(-150.0, 900.0);
    assert!(
        reader.hscroll.is_empty(),
        "page change kept the sideways scroll"
    );
}

fn code_blocks_are_highlighted_after_background_work() {
    use mdt_app::reader::block::{Item, brush};
    use mdt_text::PaintOp;
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let doc = "```rust\nfn main() { let s = \"hi\"; } // c\n```\n\n```klingon\nqapla'\n```\n";
    let mut reader = Reader::new(mdt_markdown::parse(doc), 1.0);
    reader.resize(600.0, 800.0);
    let syntax_glyphs = |units: &[(usize, std::rc::Rc<mdt_app::reader::block::UnitLayout>)]| {
        units[0]
            .1
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Text(t) => Some(t),
                _ => None,
            })
            .flat_map(|t| t.ops.iter())
            .filter(
                |op| matches!(op, PaintOp::Glyph { brush, .. } if brush.0 >= brush::SYNTAX_BASE),
            )
            .count()
    };
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        let units = reader.prepare(&mut fonts, app);
        assert_eq!(syntax_glyphs(&units), 0, "plain until highlighting lands");
        let pending = reader.take_pending_highlights();
        assert_eq!(
            pending.len(),
            1,
            "only the known language is queued: {pending:?}"
        );
        for (info, code) in pending {
            reader.highlights.compute(&info, &code);
        }
        reader.content_landed();
        let units = reader.prepare(&mut fonts, app);
        assert!(syntax_glyphs(&units) > 10, "highlight colours applied");
        assert!(
            reader.take_pending_highlights().is_empty(),
            "not queued twice"
        );
    });
}

fn selection_copies_rendered_text_and_markdown() {
    use mdt_app::reader::view::{Direction, Pos, Selection};
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let source = "Some **bold** text\n\nSecond $x^2$ para\n\n- one\n- two\n";
    let mut reader = Reader::new(mdt_markdown::parse(source), 1.0);
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);

        // Select the first paragraph with the mouse: start of its line to past its end.
        let a = reader.hit_test_doc(&mut fonts, app, -10.0, 2.0).unwrap();
        let b = reader.hit_test_doc(&mut fonts, app, 5000.0, 2.0).unwrap();
        assert_eq!((a.unit, a.offset), (0, 0));
        reader.selection = Some(Selection {
            anchor: a,
            focus: b,
        });
        assert_eq!(
            reader.selected_text(&mut fonts, app, source),
            "Some bold text"
        );
        assert_eq!(reader.selected_markdown(source), "Some **bold** text");

        // Across blocks, with math copied as its source including delimiters.
        reader.selection = Some(Selection {
            anchor: Pos { offset: 5, ..a },
            focus: Pos::unit_end(1),
        });
        assert_eq!(
            reader.selected_text(&mut fonts, app, source),
            "bold text\n\nSecond $x^2$ para"
        );

        // Select all, including list items (markers are not text).
        reader.select_all();
        assert_eq!(
            reader.selected_text(&mut fonts, app, source),
            "Some bold text\n\nSecond $x^2$ para\n\none\ntwo"
        );
        assert_eq!(reader.selected_markdown(source), source.trim_end());

        // Keyboard: extend four characters right, then a line down into the next block.
        reader.selection = Some(Selection {
            anchor: a,
            focus: a,
        });
        for _ in 0..4 {
            reader.extend(&mut fonts, app, Direction::Right);
        }
        assert_eq!(reader.selected_text(&mut fonts, app, source), "Some");
        reader.extend(&mut fonts, app, Direction::Down);
        assert_eq!(reader.selection.unwrap().focus.unit, 1);
        // Right at the end of a block continues into the next one.
        reader.selection = Some(Selection {
            anchor: b,
            focus: b,
        });
        reader.extend(&mut fonts, app, Direction::Right);
        assert_eq!(reader.selection.unwrap().focus.unit, 1);
        reader.extend(&mut fonts, app, Direction::Left);
        reader.extend(&mut fonts, app, Direction::Left);
        assert_eq!(reader.selected_text(&mut fonts, app, source), "t");
    });
}

fn async_content_above_viewport_does_not_move_visible_content() {
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    // a.png is much taller than the nothing reserved while its header is read; b.png
    // reserves 300x200 from its tag but is not an image, so it becomes a placeholder.
    let dir = tempfile::tempdir().unwrap();
    image::RgbaImage::new(300, 400)
        .save(dir.path().join("a.png"))
        .unwrap();
    std::fs::write(dir.path().join("b.png"), "not an image").unwrap();
    let mut doc =
        String::from("![a](a.png)\n\n<img src=\"b.png\" width=\"300\" height=\"200\">\n\n");
    for i in 0..200 {
        doc.push_str(&format!(
            "Paragraph {i} with some words to fill a line or two.\n\n"
        ));
    }
    let mut reader = Reader::new(mdt_markdown::parse(&doc), 1.0);
    reader.images.doc_dir = Some(dir.path().to_owned());
    let viewport = 400.0;
    reader.resize(600.0, viewport);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
        // Scroll so both images are above the viewport (but inside the overscan).
        reader.scroll_by(300.0);
        let visible = reader.prepare(&mut fonts, app);
        let first_visible = visible[0].0;
        assert!(first_visible >= 2, "images still visible");
        let screen = |r: &Reader, u: usize| r.heights.top(u) - r.anchor.scroll_y(&r.heights);
        let before = screen(&reader, first_visible + 1);
        let (h0, h1) = (reader.heights.get(0), reader.heights.get(1));

        // The headers arrive: one image much taller, the other shorter than reserved.
        reader.run_images_blocking();
        // Overscan continues over later frames within the per-frame budget.
        reader.prepare(&mut fonts, app);
        while reader.overscan_pending() {
            reader.prepare(&mut fonts, app);
        }
        assert!(reader.heights.get(0) > h0 + 300.0, "a.png height applied");
        assert!(reader.heights.get(1) < h1, "b.png placeholder applied");
        assert_eq!(
            screen(&reader, first_visible + 1),
            before,
            "visible content moved"
        );
    });
}

fn images_reserve_space_and_show_placeholders() {
    use mdt_app::reader::block::Item;
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("img")).unwrap();
    image::RgbaImage::from_pixel(64, 32, image::Rgba([0, 128, 255, 255]))
        .save(dir.path().join("img/plot.png"))
        .unwrap();
    let doc =
        "![plot](img/plot.png)\n\n![gone](img/missing.png)\n\n![net](https://example.com/a.png)\n";
    let mut reader = Reader::new(mdt_markdown::parse(doc), 1.0);
    reader.images.doc_dir = Some(dir.path().to_owned());
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
        reader.run_images_blocking();
        let units = reader.prepare(&mut fonts, app);
        let texts = |u: usize| -> String {
            units[u]
                .1
                .items
                .iter()
                .filter_map(|i| match i {
                    Item::Placeholder { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("|")
        };
        let frame = units[0].1.items.iter().find_map(|i| match i {
            Item::Image { w, h, source, .. } => Some((*w, *h, source.clone())),
            _ => None,
        });
        let (w, h, source) = frame.expect("plot.png has its frame");
        assert_eq!((w, h), (64.0, 32.0), "one logical pixel per image pixel");
        let height_before = units[0].1.height;

        // Missing file: a placeholder with the alt text, the reference and the reason.
        assert!(
            units[1]
                .1
                .items
                .iter()
                .any(|i| matches!(i, Item::Placeholder { .. }))
        );
        let t = texts(1);
        for part in ["gone", "img/missing.png", "image not found"] {
            assert!(t.contains(part), "{part} in {t}");
        }
        // Remote, not allowed: a placeholder with a load action, and nothing fetched.
        assert_eq!(units[2].1.loads[0].url, "https://example.com/a.png");
        assert!(texts(2).contains("https://example.com/a.png"));
        assert!(!reader.images.busy(), "no remote request was queued");

        // Decoding fills the reserved frame without changing layout.
        assert!(
            reader.images.raster(&source, 128, 64).is_none(),
            "not decoded yet"
        );
        for work in reader.images.take_pending() {
            assert!(!work.changes_layout());
            work.run();
        }
        let raster = reader.images.raster(&source, 128, 64).expect("decoded");
        // The raster keeps the image's own size, plus the transparent border every
        // raster gets.
        let pad = 2 * mdt_app::reader::images::PAD as i32;
        assert_eq!(
            raster.size(0),
            gpui::size(gpui::DevicePixels(64 + pad), gpui::DevicePixels(32 + pad)),
            "never upscaled"
        );
        assert_eq!(reader.prepare(&mut fonts, app)[0].1.height, height_before);
    });
}

fn math_copies_as_source_with_delimiters() {
    use mdt_app::reader::view::{Pos, Selection};
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let source = "Area $\\pi r^2$ here\n\n$$\\int_0^1 x\\,dx$$\n\n```math\na^2 + b^2\n```\n";
    let mut reader = Reader::new(mdt_markdown::parse(source), 1.0);
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
        let expected = "Area $\\pi r^2$ here\n\n$$\\int_0^1 x\\,dx$$\n\n```math\na^2 + b^2\n```";
        reader.select_all();
        assert_eq!(reader.selected_text(&mut fonts, app, source), expected);
        // A partial selection goes through the laid-out items and copies the same way.
        reader.selection = Some(Selection {
            anchor: Pos {
                offset: 1,
                ..Pos::unit_start(0)
            },
            focus: Pos::unit_end(2),
        });
        assert_eq!(
            reader.selected_text(&mut fonts, app, source),
            &expected[1..]
        );
    });
}

fn theme_switch_and_repeats_typeset_once() {
    use gpui::{AppContext, px, size};
    use mdt_app::theme::ThemeMode;
    use mdt_app::view::{CurrentThemeMode, DocumentView};
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let text = "$$\\sum_i x_i$$\n\n".repeat(50) + "Inline $y^2$.\n";
    let doc = mdt_doc::Document::from_bytes(text.as_bytes(), None).unwrap();
    cx.update(|app| app.set_global(CurrentThemeMode(ThemeMode::Light)));
    let window = cx
        .open_window(size(px(800.0), px(4000.0)), |window, cx| {
            cx.new(|cx| DocumentView::new(doc, window, cx))
        })
        .unwrap();
    let settle = |cx: &mut HeadlessAppContext| {
        for _ in 0..3 {
            cx.capture_screenshot(window.into()).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            cx.run_until_parked();
        }
    };
    settle(&mut cx);
    let compiles = |cx: &mut HeadlessAppContext| {
        cx.update(|app| {
            window
                .read(app)
                .unwrap()
                .reader
                .borrow()
                .math
                .cache
                .compiles()
        })
    };
    assert_eq!(compiles(&mut cx), 2, "50 repeats plus one inline equation");
    cx.update(|app| app.set_global(CurrentThemeMode(ThemeMode::Dark)));
    let _ = window.update(&mut cx, |_, window, _| window.refresh());
    settle(&mut cx);
    assert_eq!(
        compiles(&mut cx),
        2,
        "a theme switch typesets nothing again"
    );
}

fn zoom_keeps_the_top_block_in_place() {
    let ts = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        ts,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    let mut doc = String::new();
    for i in 0..300 {
        doc.push_str(&format!(
            "Paragraph {i} with enough words to wrap across a line or two in the column.\n\n"
        ));
    }
    let mut reader = Reader::new(mdt_markdown::parse(&doc), 1.0);
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
        reader.scroll_by(5000.0);
        let top = reader.prepare(&mut fonts, app)[0].0;
        let offset = reader.anchor.offset;
        for zoom in [1.1, 1.25, 3.0, 0.5, 1.0] {
            reader.set_zoom(zoom);
            let visible = reader.prepare(&mut fonts, app);
            assert_eq!(visible[0].0, top, "top block at zoom {zoom}");
            assert_eq!(reader.anchor.unit, top);
            assert!((reader.anchor.offset - offset * zoom as f64).abs() < 1e-6);
        }
    });
}

fn headless() -> HeadlessAppContext {
    HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    )
}

/// The window point at the middle of unit `unit`'s first checkbox (origin at 0, 0).
fn checkbox_point(
    reader: &mut Reader,
    unit: usize,
    fonts: &mut mdt_text::FontRegistry,
    app: &gpui::App,
) -> (f32, f32) {
    use mdt_app::reader::block::Item;
    let visible = reader.prepare(fonts, app);
    let l = &visible.iter().find(|(i, _)| *i == unit).unwrap().1;
    let (x, y, size) = l
        .items
        .iter()
        .find_map(|i| match i {
            Item::Checkbox { x, y, size, .. } => Some((*x, *y, *size)),
            _ => None,
        })
        .unwrap();
    let top = reader.heights.top(unit) - reader.anchor.scroll_y(&reader.heights);
    (x + size / 2.0, top as f32 + y + size / 2.0)
}

/// Cached layouts are shared by units with the same content, wherever they are, so their
/// source ranges follow the unit: after an edit above it, and for a repeated block.
fn cached_layouts_point_at_their_own_unit_source() {
    use mdt_app::reader::view::{Pos, Selection};
    let mut cx = headless();
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        let before = "Intro\n\n- [ ] task\n\nArea $x^2$ here\n";
        let after = "Intro, longer now\n\n- [ ] task\n\nArea $x^2$ here\n";
        let mut reader = Reader::new(mdt_markdown::parse(before), 1.0);
        reader.resize(600.0, 800.0);
        reader.prepare(&mut fonts, app);
        reader.set_model(mdt_markdown::parse(after));
        let at = checkbox_point(&mut reader, 1, &mut fonts, app);
        let range = reader.checkbox_at(at).expect("checkbox hit");
        assert_eq!(&after[range.clone()], "[ ]", "{range:?}");
        assert_eq!(range.start, after.find("[ ]").unwrap());
        // A partial copy of the paragraph below the edit copies its own math source.
        reader.selection = Some(Selection {
            anchor: Pos {
                offset: 1,
                ..Pos::unit_start(2)
            },
            focus: Pos::unit_end(2),
        });
        assert_eq!(
            reader.selected_text(&mut fonts, app, after),
            "rea $x^2$ here"
        );

        let doc = "- [ ] same\n\npara\n\n- [ ] same\n";
        let mut reader = Reader::new(mdt_markdown::parse(doc), 1.0);
        reader.resize(600.0, 800.0);
        let at = checkbox_point(&mut reader, 2, &mut fonts, app);
        let range = reader.checkbox_at(at).expect("checkbox hit");
        assert_eq!(
            range.start,
            doc.rfind("[ ]").unwrap(),
            "toggles the first box"
        );
    });
}

/// A selection made before an image's header is read still copies the same text after
/// the image lands and adds layout items before it.
fn selection_survives_async_content() {
    let mut cx = headless();
    let dir = tempfile::tempdir().unwrap();
    image::RgbaImage::new(20, 10)
        .save(dir.path().join("p.png"))
        .unwrap();
    let doc = "| a | b | c |\n|---|---|---|\n| x | ![p](p.png)日本 | abcdef |\n";
    let mut reader = Reader::new(mdt_markdown::parse(doc), 1.0);
    reader.images.doc_dir = Some(dir.path().to_owned());
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        use mdt_app::reader::block::Item;
        use mdt_app::reader::view::{Pos, Selection};
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        let visible = reader.prepare(&mut fonts, app);
        let cell = visible[0]
            .1
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Text(t) if t.select.is_some() => Some((t.x, t.y)),
                _ => None,
            })
            .nth(5)
            .unwrap();
        let at = reader
            .hit_test_doc(&mut fonts, app, cell.0 + 1.0, cell.1 as f64 + 2.0)
            .unwrap();
        reader.selection = Some(Selection {
            anchor: Pos { offset: 1, ..at },
            focus: Pos { offset: 2, ..at },
        });
        assert_eq!(reader.selected_text(&mut fonts, app, doc), "b");
        reader.run_images_blocking();
        let visible = reader.prepare(&mut fonts, app);
        assert!(
            visible[0]
                .1
                .items
                .iter()
                .any(|i| matches!(i, Item::Image { .. })),
            "the image landed"
        );
        assert_eq!(reader.selected_text(&mut fonts, app, doc), "b");
    });
}

/// Clicking below the document's end goes to its last text, not to whichever item is
/// nearest sideways.
fn hit_test_below_the_end_picks_the_last_text() {
    let mut cx = headless();
    let doc = "- a much longer first item here\n- two\n- three\n";
    let mut reader = Reader::new(mdt_markdown::parse(doc), 1.0);
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
        let y = reader.heights.total() + 50.0;
        for x in [0.0, 200.0, 2000.0] {
            let p = reader.hit_test_doc(&mut fonts, app, x, y).unwrap();
            assert_eq!(p.item, 2, "x {x}");
        }
        let p = reader.hit_test_doc(&mut fonts, app, 2000.0, y).unwrap();
        assert_eq!(p.offset, "three".len());
    });
}

/// Keyboard selection inside a sideways-scrolled code block, and across a block with no
/// text.
fn keyboard_selection_in_scrolled_code_and_across_rules() {
    use mdt_app::reader::view::{Direction, Pos, Selection};
    let mut cx = headless();
    let line = "0123456789".repeat(10);
    let doc = format!("```\n{line}\n{line}\n```\n\nPara one\n\n---\n\nPara two\n");
    let mut reader = Reader::new(mdt_markdown::parse(&doc), 1.0);
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
        reader.hscroll.insert((0, 0), 300.0);
        let at = Pos {
            offset: 20,
            ..Pos::unit_start(0)
        };
        reader.selection = Some(Selection {
            anchor: at,
            focus: at,
        });
        reader.extend(&mut fonts, app, Direction::Down);
        assert_eq!(reader.selection.unwrap().focus.offset, line.len() + 1 + 20);
        reader.extend(&mut fonts, app, Direction::Up);
        assert_eq!(reader.selection.unwrap().focus.offset, 20);

        // From the rule (unit 2), Shift+arrows go on to the paragraphs around it.
        for (dir, unit) in [
            (Direction::Right, 3),
            (Direction::Down, 3),
            (Direction::Left, 1),
            (Direction::Up, 1),
        ] {
            let rule = Pos::unit_start(2);
            reader.selection = Some(Selection {
                anchor: rule,
                focus: rule,
            });
            reader.extend(&mut fonts, app, dir);
            assert_eq!(reader.selection.unwrap().focus.unit, unit, "{dir:?}");
        }
    });
}

/// An image whose header reads but whose data does not decode becomes a placeholder; it is
/// not decoded again on every frame.
fn undecodable_image_is_decoded_once() {
    use mdt_app::reader::block::Item;
    let mut cx = headless();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.png");
    image::RgbaImage::from_pixel(64, 32, image::Rgba([9, 99, 199, 255]))
        .save(&path)
        .unwrap();
    // Keep the signature and IHDR (33 bytes), garble the image data.
    let mut bytes = std::fs::read(&path).unwrap();
    for b in &mut bytes[41..] {
        *b ^= 0x5a;
    }
    std::fs::write(&path, bytes).unwrap();
    let mut reader = Reader::new(mdt_markdown::parse("![bad](bad.png)\n"), 1.0);
    reader.images.doc_dir = Some(dir.path().to_owned());
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        let mut decodes = 0;
        // Frames as the element runs them: lay out, ask for rasters, run the work.
        for _ in 0..6 {
            for (_, l) in reader.prepare(&mut fonts, app) {
                for item in &l.items {
                    if let Item::Image { source, .. } = item {
                        let _ = reader.images.raster(source, 64, 32);
                    }
                }
            }
            for work in reader.images.take_pending() {
                decodes += usize::from(!work.changes_layout());
                if work.run() {
                    reader.content_landed();
                }
            }
        }
        assert_eq!(decodes, 1, "decoded again after failing");
        let visible = reader.prepare(&mut fonts, app);
        assert!(
            visible[0]
                .1
                .items
                .iter()
                .any(|i| matches!(i, Item::Placeholder { .. })),
            "a placeholder after the failed decode"
        );
    });
}

/// Results that land, resizes and zooms make new layouts of the units on screen; the old
/// ones are dropped rather than kept while the units stay near the viewport.
fn layout_cache_stays_bounded() {
    let mut cx = headless();
    let doc: String = (0..30).map(|i| format!("Paragraph {i}.\n\n")).collect();
    let mut reader = Reader::new(mdt_markdown::parse(&doc), 1.0);
    reader.resize(600.0, 4000.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        for i in 0..20 {
            reader.content_landed();
            reader.resize(600.0 + (i % 5) as f32 * 40.0, 4000.0);
            reader.set_zoom(if i % 2 == 0 { 1.0 } else { 1.1 });
            reader.prepare(&mut fonts, app);
            assert!(
                reader.cached_layouts() <= 30,
                "{} layouts for 30 units",
                reader.cached_layouts()
            );
        }
    });
}

/// The highlight cache is bounded: a code block whose highlighting was evicted is
/// highlighted again.
fn evicted_highlighting_is_requested_again() {
    let mut cx = headless();
    let doc = "```rust\nfn a() {}\n```\n\n```rust\nfn b() {}\n```\n";
    let mut reader = Reader::new(mdt_markdown::parse(doc), 1.0);
    reader.highlights = mdt_highlight::Cache::with_capacity(1);
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
        let pending = reader.take_pending_highlights();
        assert_eq!(pending.len(), 2);
        // The second result evicts the first from the one-byte cache.
        for (info, code) in &pending {
            reader.highlights.compute(info, code);
            reader.highlight_landed(&mdt_highlight::Cache::key(info, code));
        }
        reader.prepare(&mut fonts, app);
        let again = reader.take_pending_highlights();
        assert_eq!(again, pending[..1], "the evicted block is queued again");
    });
}

/// A diagram fence pasted above another shows as pending while it renders, not as the
/// diagram that used to be in its place.
fn pasted_diagram_does_not_show_its_neighbour() {
    use mdt_app::reader::block::Item;
    let mut cx = headless();
    let svg = |w: u32| {
        format!(
            "```svg\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"20\"><rect width=\"{w}\" height=\"20\"/></svg>\n```\n"
        )
    };
    let before = format!("Intro\n\n{}", svg(40));
    let after = format!("Intro\n\n{}\n{}", svg(80), svg(40));
    let mut reader = Reader::new(mdt_markdown::parse(&before), 1.0);
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        reader.prepare(&mut fonts, app);
        reader.run_images_blocking();
        let has_diagram = |l: &mdt_app::reader::block::UnitLayout| {
            l.items.iter().any(|i| matches!(i, Item::Diagram { .. }))
        };
        assert!(has_diagram(&reader.prepare(&mut fonts, app)[1].1));
        reader.set_model(mdt_markdown::parse(&after));
        let visible = reader.prepare(&mut fonts, app);
        assert!(
            !has_diagram(&visible[1].1),
            "the pasted fence shows its neighbour"
        );
        assert!(
            has_diagram(&visible[2].1),
            "the old fence lost its rendering"
        );
    });
}

/// A remote-image placeholder in a sideways-scrolled table is clicked where it is shown.
fn remote_image_placeholder_moves_with_its_table() {
    use gpui::{point, px};
    let mut cx = headless();
    let mut doc = String::from("|");
    for c in 0..40 {
        doc += &format!(" column{c} |");
    }
    doc += "\n|";
    doc += &"---|".repeat(40);
    doc += "\n|";
    for c in 0..39 {
        doc += &format!(" value{c} |");
    }
    doc += " ![net](https://example.com/a.png) |\n";
    let mut reader = Reader::new(mdt_markdown::parse(&doc), 1.0);
    reader.resize(600.0, 800.0);
    cx.update(|app| {
        let fonts = fonts(app);
        let mut fonts = fonts.borrow_mut();
        let placed: Vec<_> = reader
            .prepare(&mut fonts, app)
            .into_iter()
            .map(|(i, l)| (i, reader.heights.top(i) as f32, l))
            .collect();
        let load = placed[0].2.loads[0].clone();
        assert!(load.x > 1000.0, "the image is in the last column");
        let origin = point(px(0.0), px(0.0));
        let at = |x: f32| point(px(x + 5.0), px(load.y + 5.0));
        let scroll = load.x - 100.0;
        reader.hscroll.insert((0, 0), scroll);
        reader.record_areas(&placed, origin);
        assert_eq!(
            reader.load_at(at(100.0)).as_deref(),
            Some(load.url.as_str())
        );
        assert_eq!(
            reader.load_at(at(load.x)),
            None,
            "clicked where it was laid out"
        );
    });
}

fn main() {
    // `cargo test --test reader -- <part of a name>` runs only the matching tests.
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let tests: [(&str, fn()); 19] = [
        (
            "pasted_diagram_does_not_show_its_neighbour",
            pasted_diagram_does_not_show_its_neighbour,
        ),
        (
            "remote_image_placeholder_moves_with_its_table",
            remote_image_placeholder_moves_with_its_table,
        ),
        ("layout_cache_stays_bounded", layout_cache_stays_bounded),
        (
            "evicted_highlighting_is_requested_again",
            evicted_highlighting_is_requested_again,
        ),
        (
            "undecodable_image_is_decoded_once",
            undecodable_image_is_decoded_once,
        ),
        (
            "cached_layouts_point_at_their_own_unit_source",
            cached_layouts_point_at_their_own_unit_source,
        ),
        (
            "selection_survives_async_content",
            selection_survives_async_content,
        ),
        (
            "hit_test_below_the_end_picks_the_last_text",
            hit_test_below_the_end_picks_the_last_text,
        ),
        (
            "keyboard_selection_in_scrolled_code_and_across_rules",
            keyboard_selection_in_scrolled_code_and_across_rules,
        ),
        (
            "zoom_keeps_the_top_block_in_place",
            zoom_keeps_the_top_block_in_place,
        ),
        (
            "images_reserve_space_and_show_placeholders",
            images_reserve_space_and_show_placeholders,
        ),
        (
            "math_copies_as_source_with_delimiters",
            math_copies_as_source_with_delimiters,
        ),
        (
            "theme_switch_and_repeats_typeset_once",
            theme_switch_and_repeats_typeset_once,
        ),
        (
            "async_content_above_viewport_does_not_move_visible_content",
            async_content_above_viewport_does_not_move_visible_content,
        ),
        (
            "selection_copies_rendered_text_and_markdown",
            selection_copies_rendered_text_and_markdown,
        ),
        (
            "code_blocks_are_highlighted_after_background_work",
            code_blocks_are_highlighted_after_background_work,
        ),
        (
            "wide_table_scrolls_inside_its_block",
            wide_table_scrolls_inside_its_block,
        ),
        (
            "wide_table_scrolls_across_the_page",
            wide_table_scrolls_across_the_page,
        ),
        (
            "scroll_through_10_mb_document",
            scroll_through_10_mb_document,
        ),
    ];
    for (name, test) in tests {
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        test();
        println!("test {name} ... ok");
    }
}
