//! Layout snapshots of the reader-check document at 1,400 and 500 px windows. GPUI is
//! faked (font ids are irrelevant to positions), so they run on every platform; CJK text
//! is left out because fallback fonts differ by system.

use std::borrow::Cow;
use std::cell::Cell;
use std::fmt::Write;

use mdt_text::{FontRegistry, GpuiFonts};

use super::block::{Ctx, Item, layout_unit};
use super::diagrams::Diagrams;
use super::images::Images;
use super::math::MathView;
use super::view::Reader;

#[derive(Default)]
struct FakeGpui(Cell<usize>);

impl GpuiFonts for FakeGpui {
    fn add_fonts(&self, _: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        Ok(())
    }
    fn exact_font_id(&self, _: &gpui::Font) -> Option<gpui::FontId> {
        self.0.set(self.0.get() + 1);
        Some(gpui::FontId(self.0.get()))
    }
}

fn document() -> String {
    let doc = include_str!("../../../../docs/visual/reader-check.md");
    doc.lines()
        .filter(|l| !l.contains("日本語"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn snapshot(window_w: f32) -> String {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let model = mdt_markdown::parse(&document());
    let mut reader = Reader::new(model.clone(), 1.0);
    let col = reader.column(&mut fonts, window_w);
    let mut out = format!(
        "window {window_w} column x {:.1} width {:.1}\n",
        col.x, col.width
    );
    let mut lcx = parley::LayoutContext::new();
    let mut math = MathView::default();
    let highlights = mdt_highlight::Cache::default();
    let mut images = Images::default();
    images.doc_dir =
        Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/visual"));
    let mut diagrams = Diagrams::default();
    // Typeset every equation first, so the snapshot shows the real math metrics.
    let mut ctx = Ctx::new(
        &mut fonts,
        &mut lcx,
        1.0,
        &highlights,
        &mut math,
        &mut images,
        &mut diagrams,
    );
    ctx.page = Some((col.page_x, col.page_w));
    for (i, unit) in model.units.iter().enumerate() {
        layout_unit(&mut ctx, i, unit, col.width);
    }
    let pending = math.take_pending();
    math.cache.compute_batch(&pending);
    // Read image headers and render diagrams, as the background would.
    images.take_pending().into_iter().for_each(|w| {
        w.run();
    });
    diagrams.take_pending().into_iter().for_each(|w| {
        w.run();
    });
    // Past the error delay, so failed equations show their error.
    math.now += super::math::ERROR_DELAY;
    let mut ctx = Ctx::new(
        &mut fonts,
        &mut lcx,
        1.0,
        &highlights,
        &mut math,
        &mut images,
        &mut diagrams,
    );
    ctx.page = Some((col.page_x, col.page_w));
    for (i, unit) in model.units.iter().enumerate() {
        let l = layout_unit(&mut ctx, i, unit, col.width);
        writeln!(out, "unit {:?} height {:.1}", unit.kind, l.height).unwrap();
        for item in &l.items {
            match item {
                Item::Text(t) => {
                    // Byte ranges of each line show where the text wrapped.
                    let lines: Vec<_> = t
                        .paragraph
                        .layout
                        .lines()
                        .map(|line| line.text_range())
                        .collect();
                    writeln!(
                        out,
                        "  text ({:.1},{:.1}) {:.1}x{:.1}{} {:?}",
                        t.x,
                        t.y,
                        t.paragraph.width,
                        t.paragraph.height,
                        if t.scrolls { " scrolls" } else { "" },
                        lines
                    )
                    .unwrap();
                }
                Item::Fill { x, y, w, h, brush } => writeln!(
                    out,
                    "  fill ({x:.1},{y:.1}) {w:.1}x{h:.1} brush {}",
                    brush.0
                )
                .unwrap(),
                Item::Checkbox {
                    x,
                    y,
                    size,
                    checked,
                    ..
                } => writeln!(
                    out,
                    "  checkbox ({x:.1},{y:.1}) {size:.1} checked {checked}"
                )
                .unwrap(),
                Item::Math {
                    x, y, scale, scene, ..
                } => writeln!(
                    out,
                    "  math ({x:.1},{y:.1}) {:.1}x{:.1}",
                    scene.width * scale,
                    scene.height * scale
                )
                .unwrap(),
                Item::Diagram { x, y, w, h, .. } => {
                    writeln!(out, "  diagram ({x:.1},{y:.1}) {w:.1}x{h:.1}").unwrap()
                }
                Item::Image { x, y, w, h, .. } => {
                    writeln!(out, "  image ({x:.1},{y:.1}) {w:.1}x{h:.1}").unwrap()
                }
                Item::Placeholder { x, y, w, h, text } => {
                    writeln!(out, "  placeholder ({x:.1},{y:.1}) {w:.1}x{h:.1} {text:?}").unwrap()
                }
            }
        }
        for r in &l.regions {
            writeln!(
                out,
                "  region items {:?} {:.1} wide, content {:.1}",
                r.items, r.w, r.content_w
            )
            .unwrap();
        }
        for h in &l.hovers {
            writeln!(out, "  hover ({:.1},{:.1}) {}", h.x, h.y, h.message).unwrap();
        }
    }
    out
}

#[test]
fn reader_check_at_1400() {
    insta::assert_snapshot!(snapshot(1400.0));
}

#[test]
fn reader_check_at_500() {
    insta::assert_snapshot!(snapshot(500.0));
}

#[test]
fn column_is_72_chars_max_and_centered() {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut reader = Reader::new(mdt_markdown::parse(""), 1.0);
    let wide = reader.column(&mut fonts, 1400.0);
    let wider = reader.column(&mut fonts, 2000.0);
    assert_eq!(wide.width, wider.width, "column stops growing at 72 chars");
    assert!(
        (wide.x - (1400.0 - wide.width) / 2.0).abs() <= 1.0,
        "centered"
    );
    let narrow = reader.column(&mut fonts, 500.0);
    assert_eq!(narrow.x, 16.0, "narrow windows keep 16 px padding");
    assert_eq!(narrow.width, 468.0);
}

#[test]
fn wide_column_is_half_as_wide_again() {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let mut reader = Reader::new(mdt_markdown::parse(""), 1.0);
    let normal = reader.column(&mut fonts, 2000.0).width;
    reader.wide = true;
    let wide = reader.column(&mut fonts, 2000.0);
    assert!(
        (wide.width / normal - 1.5).abs() < 0.01,
        "{normal} -> {}",
        wide.width
    );
    assert!(
        (wide.x - (2000.0 - wide.width) / 2.0).abs() <= 1.0,
        "centered"
    );
    assert_eq!(
        reader.column(&mut fonts, 500.0).width,
        468.0,
        "narrow windows unchanged"
    );
}

/// Find paints a rendered-text hit on the text item it belongs to by counting selectable
/// text items in layout order, which must line up with `block_texts`.
#[test]
fn selectable_texts_line_up_with_layout_items() {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let source = document();
    let model = mdt_markdown::parse(&source);
    let mut lcx = parley::LayoutContext::new();
    let mut math = MathView::default();
    let highlights = mdt_highlight::Cache::default();
    let mut images = Images::default();
    let mut diagrams = Diagrams::default();
    let mut ctx = Ctx::new(
        &mut fonts,
        &mut lcx,
        1.0,
        &highlights,
        &mut math,
        &mut images,
        &mut diagrams,
    );
    for (i, unit) in model.units.iter().enumerate() {
        let mut texts = Vec::new();
        super::view::block_texts(&unit.block, &mut texts);
        let l = layout_unit(&mut ctx, i, unit, 600.0);
        let laid: Vec<&str> = l
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Text(t) => t.select.as_ref().map(|s| s.text.as_str()),
                _ => None,
            })
            .collect();
        let want: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(laid, want, "unit {i} {:?}", unit.kind);
    }
}

/// Lays out `doc`'s units in a window `window_w` wide, with the page set as the view sets
/// it, and returns the column and the last unit's layout.
fn table_layout(doc: &str, window_w: f32) -> (super::view::Column, super::block::UnitLayout) {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let model = mdt_markdown::parse(doc);
    let mut reader = Reader::new(model.clone(), 1.0);
    let col = reader.column(&mut fonts, window_w);
    let mut lcx = parley::LayoutContext::new();
    let mut math = MathView::default();
    let highlights = mdt_highlight::Cache::default();
    let mut images = Images::default();
    let mut diagrams = Diagrams::default();
    let mut ctx = Ctx::new(
        &mut fonts,
        &mut lcx,
        1.0,
        &highlights,
        &mut math,
        &mut images,
        &mut diagrams,
    );
    ctx.page = Some((col.page_x, col.page_w));
    let last = model.units.len() - 1;
    let l = layout_unit(&mut ctx, last, &model.units[last], col.width);
    (col, l)
}

fn cell_texts(l: &super::block::UnitLayout) -> Vec<&super::block::TextItem> {
    l.items
        .iter()
        .filter_map(|it| match it {
            Item::Text(t) => Some(&**t),
            _ => None,
        })
        .collect()
}

fn lines(t: &super::block::TextItem) -> usize {
    t.paragraph.layout.lines().count()
}

/// Left and right edges of the table's cell texts and rules, relative to the column.
fn extent(l: &super::block::UnitLayout) -> (f32, f32) {
    l.items
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), it| match it {
            Item::Text(t) => (lo.min(t.x), hi.max(t.x + t.paragraph.width)),
            Item::Fill { x, w, .. } => (lo.min(*x), hi.max(x + w)),
            _ => (lo, hi),
        })
}

fn table(cols: usize, header: impl Fn(usize) -> String, rows: &[Vec<String>]) -> String {
    let mut doc = String::from("Intro.\n\n|");
    for c in 0..cols {
        doc += &format!(" {} |", header(c));
    }
    doc += "\n|";
    doc += &"---|".repeat(cols);
    doc += "\n";
    for row in rows {
        doc += "|";
        for cell in row {
            doc += &format!(" {cell} |");
        }
        doc += "\n";
    }
    doc
}

/// Table layout: a table that fits.
#[test]
fn table_that_fits_keeps_its_natural_width_in_the_column() {
    let doc = table(
        3,
        |c| format!("H{c}"),
        &[vec!["a".into(), "b".into(), "c".into()]],
    );
    let (col, l) = table_layout(&doc, 1400.0);
    let (lo, hi) = extent(&l);
    assert_eq!(lo, 0.0, "aligned with the column's left edge");
    assert!(hi < col.width / 2.0, "natural width, not stretched: {hi}");
    assert!(cell_texts(&l).iter().all(|t| lines(t) == 1));
    let r = &l.regions[0];
    assert_eq!((r.x, r.w), (0.0, col.width));
    assert_eq!(r.max_scroll(), 0.0);
}

/// Table layout: long cell text.
#[test]
fn long_cell_text_wraps_without_scrolling() {
    let long = "lorem ipsum dolor sit amet ".repeat(15);
    let long = long.trim().to_owned();
    assert!(long.len() >= 400);
    let doc = table(
        3,
        |c| format!("Column {c}"),
        &[vec!["short".into(), long.clone(), "12.50".into()]],
    );
    let (col, l) = table_layout(&doc, 1400.0);
    let texts = cell_texts(&l);
    let cell = texts
        .iter()
        .find(|t| t.paragraph.layout.lines().count() > 1);
    assert!(cell.is_some(), "the long cell wraps");
    for t in &texts {
        assert!(
            t.paragraph.width <= col.width + 0.5,
            "column wider than the text column"
        );
    }
    let r = &l.regions[0];
    assert_eq!(r.max_scroll(), 0.0, "the table scrolls");
    let (lo, hi) = extent(&l);
    assert!(
        lo >= col.page_x && hi <= col.page_x + col.page_w,
        "past the page: {lo}..{hi}"
    );
}

/// Table layout: many short columns.
#[test]
fn many_short_columns_widen_past_the_column_centered_on_the_window() {
    let window = 1400.0;
    let row: Vec<String> = (0..20).map(|c| format!("v{c}.00")).collect();
    let doc = table(20, |c| format!("cell {c}"), &[row]);
    let (col, l) = table_layout(&doc, window);
    let (lo, hi) = extent(&l);
    let natural = hi - lo;
    assert!(
        natural > col.width * 1.2 && natural < col.page_w,
        "natural width {natural}, column {}, page {}",
        col.width,
        col.page_w
    );
    assert!(
        cell_texts(&l).iter().all(|t| lines(t) == 1),
        "a cell wrapped"
    );
    // Centered on the window: the margins left and right of the table match.
    let left = col.x + lo;
    let right = window - (col.x + hi);
    assert!((left - right).abs() <= 2.0, "margins {left} and {right}");
    assert_eq!(l.regions[0].max_scroll(), 0.0, "the table scrolls");
}

/// Table layout: a long URL in a cell.
#[test]
fn long_url_breaks_inside_its_cell() {
    let url = format!("https://example.com/{}", "a1b2c3d4e5".repeat(10));
    assert!(url.len() >= 120);
    let doc = table(
        3,
        |c| format!("Column {c}"),
        &[vec!["link".into(), url.clone(), "x".into()]],
    );
    for window in [1400.0, 500.0] {
        let (_, l) = table_layout(&doc, window);
        let texts = cell_texts(&l);
        let cell = texts
            .iter()
            .find(|t| t.paragraph.layout.lines().count() > 1);
        assert!(cell.is_some(), "the URL does not break at {window} px");
        assert_eq!(
            l.regions[0].max_scroll(),
            0.0,
            "the table scrolls at {window} px"
        );
    }
}

/// Table layout: too many columns.
#[test]
fn too_many_columns_scroll_inside_the_page() {
    let row: Vec<String> = (0..40).map(|c| format!("value{c}")).collect();
    let doc = table(40, |c| format!("column{c}"), &[row]);
    let (col, l) = table_layout(&doc, 500.0);
    let r = &l.regions[0];
    assert!(r.max_scroll() > 0.0, "the table does not scroll");
    assert_eq!((r.x, r.w), (col.page_x, col.page_w));
    assert!(
        cell_texts(&l).iter().all(|t| lines(t) == 1),
        "a cell wrapped"
    );
}

/// Table layout: a nested table.
#[test]
fn nested_table_stays_within_its_list_item() {
    let row: Vec<String> = (0..20).map(|c| format!("v{c}.00")).collect();
    let inner = table(20, |c| format!("cell {c}"), &[row]);
    let inner = inner.trim_start_matches("Intro.\n\n");
    let doc = format!(
        "Intro.\n\n- item\n\n{}",
        inner
            .lines()
            .map(|l| format!("  {l}\n"))
            .collect::<String>()
    );
    let (col, l) = table_layout(&doc, 1400.0);
    let r = l.regions.last().expect("table region");
    assert!(r.x > 0.0, "the table left the list item: x {}", r.x);
    assert!(r.x + r.w <= col.width + 0.5);
    let texts = cell_texts(&l);
    let (lo, _) = extent(&l);
    assert!(lo >= 0.0);
    // Its cells wrap or the table scrolls, but it does not reach into the margins.
    assert!(texts.len() > 20);
}

/// Links and footnote references in table cells can be hovered and clicked, wrapped or not.
#[test]
fn links_in_table_cells_are_clickable() {
    use super::block::Link;
    let long = "words ".repeat(80);
    let doc = format!(
        "Intro.\n\n| Name | Notes |\n|---|---|\n| [docs](https://example.com/docs) | {long}[more](#more) |\n"
    );
    let (_, l) = table_layout(&doc, 1400.0);
    let texts = cell_texts(&l);
    for (dest, text) in [("https://example.com/docs", "docs"), ("#more", "more")] {
        let h = l
            .hovers
            .iter()
            .find(|h| h.link == Some(Link::Url(dest.into())))
            .unwrap_or_else(|| panic!("no hover for {dest}"));
        let cell = texts
            .iter()
            .find(|t| t.select.as_ref().is_some_and(|s| s.text.ends_with(text)))
            .unwrap();
        assert!(
            h.x >= cell.x
                && h.x + h.w <= cell.x + cell.paragraph.width + 0.5
                && h.y >= cell.y
                && h.y + h.h <= cell.y + cell.paragraph.height + 0.5,
            "{dest} hover outside its cell"
        );
    }
}

/// Block nesting is capped by the builder (32 levels); laying out the deepest document must
/// fit a small thread stack, with room to spare on the main thread.
#[test]
fn deeply_nested_blocks_lay_out_on_a_small_stack() {
    let prefix: String = (0..40)
        .map(|d| if d % 2 == 0 { "> " } else { "- " })
        .collect();
    let doc = format!(
        "{prefix}Text with *emphasis*, `code`, $x^2$ and [a link](#x).\n{prefix}\n{prefix}```rust\n{prefix}fn main() {{}}\n{prefix}```\n"
    );
    let model = mdt_markdown::parse(&doc);
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
            let mut lcx = parley::LayoutContext::new();
            let mut math = MathView::default();
            let highlights = mdt_highlight::Cache::default();
            let mut images = Images::default();
            let mut diagrams = Diagrams::default();
            let mut ctx = Ctx::new(
                &mut fonts,
                &mut lcx,
                1.0,
                &highlights,
                &mut math,
                &mut images,
                &mut diagrams,
            );
            for (i, unit) in model.units.iter().enumerate() {
                let l = layout_unit(&mut ctx, i, unit, 600.0);
                assert!(l.height > 0.0);
            }
        })
        .unwrap()
        .join()
        .expect("layout overflowed a 512 KiB stack");
}

/// A huge table shows its first `MAX_TABLE_CELLS` cells and a note; its selectable texts
/// still line up with `block_texts`.
#[test]
fn huge_tables_show_their_first_rows_and_a_note() {
    use super::block::{MAX_TABLE_CELLS, TextItem};
    let rows: Vec<Vec<String>> = (0..3000)
        .map(|r| vec![format!("r{r}"), "x".into(), "y".into()])
        .collect();
    let doc = table(3, |c| format!("H{c}"), &rows);
    let (_, l) = table_layout(&doc, 1400.0);
    let texts: Vec<&TextItem> = cell_texts(&l);
    let cells = texts.iter().filter(|t| t.select.is_some()).count();
    assert_eq!(cells, MAX_TABLE_CELLS);
    let note = texts.last().unwrap();
    assert!(note.select.is_none(), "the note is not table text");
    let model = mdt_markdown::parse(&doc);
    let mut want = Vec::new();
    super::view::block_texts(&model.units.last().unwrap().block, &mut want);
    assert_eq!(want.len(), MAX_TABLE_CELLS);
    assert_eq!(
        l.regions[0].items.end,
        l.items.len() - 1,
        "the note is outside the table's region"
    );
}

/// Lays out every unit of `doc` in a window `window_w` wide and returns the layouts.
fn layout_all(doc: &str, window_w: f32) -> Vec<super::block::UnitLayout> {
    let mut fonts = FontRegistry::new(&FakeGpui::default()).unwrap();
    let model = mdt_markdown::parse(doc);
    let mut reader = Reader::new(model.clone(), 1.0);
    let col = reader.column(&mut fonts, window_w);
    let mut lcx = parley::LayoutContext::new();
    let mut math = MathView::default();
    let highlights = mdt_highlight::Cache::default();
    let mut images = Images::default();
    let mut diagrams = Diagrams::default();
    let mut ctx = Ctx::new(
        &mut fonts,
        &mut lcx,
        1.0,
        &highlights,
        &mut math,
        &mut images,
        &mut diagrams,
    );
    ctx.page = Some((col.page_x, col.page_w));
    (model.units.iter().enumerate())
        .map(|(i, unit)| layout_unit(&mut ctx, i, unit, col.width))
        .collect()
}

/// Adjacent links with no break opportunity between them are one long line; finding each
/// link's hover rectangles separately walked that line once per link (16,000 links took
/// 6 s on the UI thread).
#[test]
fn many_adjacent_links_lay_out_in_linear_time() {
    use super::block::Link;
    for (doc, link) in [
        ("[a](b)".repeat(16_000), Link::Url("b".into())),
        (
            "[^a]".repeat(16_000) + "x\n\n[^a]: Note.\n",
            Link::Footnote("a".into()),
        ),
    ] {
        let start = std::time::Instant::now();
        let layouts = layout_all(&doc, 1400.0);
        let elapsed = start.elapsed();
        let hovers = layouts[0]
            .hovers
            .iter()
            .filter(|h| h.link == Some(link.clone()));
        eprintln!("{link:?}: {elapsed:?}, {} hovers", hovers.clone().count());
        assert!(hovers.count() >= 16_000);
        assert!(elapsed.as_secs_f32() < 3.0, "{link:?}: took {elapsed:?}");
    }
}
