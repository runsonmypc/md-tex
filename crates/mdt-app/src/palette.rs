//! The command palette: headings, recent files and commands in one list,
//! ranked with nucleo-matcher. `#` limits the list to headings and `>` to commands.

use std::path::PathBuf;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::field::Field;
use crate::keymap::{BINDINGS, Binding};
use crate::settings::{Settings, ViewMode};
use crate::theme::ThemeMode;

/// What a command does when chosen.
#[derive(Clone, Copy)]
pub enum Run {
    Action(fn() -> Box<dyn gpui::Action>),
    /// Changes a setting and saves its key.
    Setting(fn(&mut Settings), &'static str),
}

#[derive(Clone)]
pub struct Command {
    pub name: &'static str,
    /// The shortcut as shown, e.g. `Cmd+E`.
    pub shortcut: Option<String>,
    pub run: Run,
}

/// Bindings that are text-editing keys rather than commands.
fn is_command(b: &Binding) -> bool {
    const KEYS: [&str; 7] = [
        "Move: ",
        "Select: ",
        "Edit: New Line",
        "Edit: Indent",
        "Edit: Outdent",
        "Edit: Delete",
        "Close Overlay",
    ];
    !KEYS.iter().any(|k| b.name.starts_with(k))
        && b.name != "Palette: Open"
        && (cfg!(target_os = "macos") || b.name != "Edit: Emoji & Symbols")
}

/// Every app command: the registry's, then theme and setting commands.
pub fn commands() -> Vec<Command> {
    let mut out: Vec<Command> = BINDINGS
        .iter()
        .filter(|b| is_command(b))
        .map(|b| Command {
            name: b.name,
            shortcut: crate::keymap::shortcut(b).map(crate::keymap::display),
            run: Run::Action(b.action),
        })
        .collect();
    let setting = |name, f, key| Command {
        name,
        shortcut: None,
        run: Run::Setting(f, key),
    };
    out.extend([
        setting(
            "Theme: Light",
            |s: &mut Settings| s.theme = ThemeMode::Light,
            "theme",
        ),
        setting("Theme: Dark", |s| s.theme = ThemeMode::Dark, "theme"),
        setting(
            "Theme: Follow System",
            |s| s.theme = ThemeMode::System,
            "theme",
        ),
        setting(
            "Settings: Toggle Soft Wrap",
            |s| s.soft_wrap = !s.soft_wrap,
            "soft_wrap",
        ),
        setting(
            "Settings: Toggle Cursor Blink",
            |s| s.cursor_blink = !s.cursor_blink,
            "cursor_blink",
        ),
        setting(
            "Settings: Toggle Remote Images",
            |s| s.remote_images = !s.remote_images,
            "remote_images",
        ),
        setting(
            "Settings: Start in Reader",
            |s| s.initial_view_mode = ViewMode::Reader,
            "initial_view_mode",
        ),
        setting(
            "Settings: Start in Source",
            |s| s.initial_view_mode = ViewMode::Source,
            "initial_view_mode",
        ),
        setting(
            "Settings: Start in Split",
            |s| s.initial_view_mode = ViewMode::Split,
            "initial_view_mode",
        ),
    ]);
    out
}

#[derive(Clone)]
pub enum Entry {
    /// Index into the model's headings.
    Heading {
        index: usize,
        level: u8,
        text: String,
    },
    Recent(PathBuf),
    Command(Command),
}

impl Entry {
    /// The text matched and shown.
    pub fn label(&self) -> String {
        match self {
            Entry::Heading { text, .. } => text.clone(),
            Entry::Recent(p) => p.display().to_string(),
            Entry::Command(c) => c.name.to_owned(),
        }
    }
}

/// A result: the entry and the byte ranges of its label that matched.
#[derive(Clone)]
pub struct Row {
    pub entry: usize,
    pub matched: Vec<std::ops::Range<usize>>,
}

pub struct Palette {
    pub query: Field,
    pub entries: Vec<Entry>,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// First row shown; the list scrolls to keep the selection visible.
    pub top: usize,
    matcher: Matcher,
    /// The query `rows` were computed for.
    shown: Option<String>,
}

/// Rows shown at once.
pub const VISIBLE: usize = 12;

impl Palette {
    pub fn new(headings: &[mdt_markdown::HeadingEntry], recent: Vec<PathBuf>, query: &str) -> Self {
        let mut entries: Vec<Entry> = headings
            .iter()
            .enumerate()
            .map(|(index, h)| Entry::Heading {
                index,
                level: h.level,
                text: h.text.clone(),
            })
            .collect();
        entries.extend(recent.into_iter().map(Entry::Recent));
        entries.extend(commands().into_iter().map(Entry::Command));
        let mut p = Self {
            query: Field::new(query),
            entries,
            rows: Vec::new(),
            selected: 0,
            top: 0,
            matcher: Matcher::new(Config::DEFAULT),
            shown: None,
        };
        p.update();
        p
    }

    /// Re-ranks for the current query. An empty query lists everything in entry order
    /// (headings in document order, recent files newest first, then commands).
    pub fn update(&mut self) {
        if self.shown.as_deref() == Some(self.query.text.as_str()) {
            return;
        }
        let q = self.query.text.as_str();
        let (only, needle) = match q.chars().next() {
            Some('#') => (Some(0), &q[1..]),
            Some('>') => (Some(2), &q[1..]),
            _ => (None, q),
        };
        let kind = |e: &Entry| match e {
            Entry::Heading { .. } => 0,
            Entry::Recent(_) => 1,
            Entry::Command(_) => 2,
        };
        let pattern = Pattern::new(
            needle.trim(),
            CaseMatching::Smart,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let mut buf = Vec::new();
        let mut indices = Vec::new();
        let mut scored: Vec<(u32, Row)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if only.is_some_and(|k| k != kind(e)) {
                continue;
            }
            let label = e.label();
            indices.clear();
            let Some(score) = pattern.indices(
                Utf32Str::new(&label, &mut buf),
                &mut self.matcher,
                &mut indices,
            ) else {
                continue;
            };
            indices.sort_unstable();
            indices.dedup();
            scored.push((
                score,
                Row {
                    entry: i,
                    matched: char_ranges(&label, &indices),
                },
            ));
        }
        // Stable: equal scores (and an empty query) keep entry order.
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        self.rows = scored.into_iter().map(|(_, r)| r).collect();
        self.selected = 0;
        self.top = 0;
        self.shown = Some(self.query.text.clone());
    }

    /// Up (`-1`) or Down (`1`), stopping at the ends.
    pub fn move_by(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(d)
            .min(self.rows.len() - 1);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + VISIBLE {
            self.top = self.selected + 1 - VISIBLE;
        }
    }

    pub fn chosen(&self) -> Option<&Entry> {
        self.rows.get(self.selected).map(|r| &self.entries[r.entry])
    }
}

/// Byte ranges of `text` covering the characters at `indices` (sorted), merged when
/// adjacent.
fn char_ranges(text: &str, indices: &[u32]) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut want = indices.iter().peekable();
    for (n, (at, ch)) in text.char_indices().enumerate() {
        if want.peek().is_some_and(|&&i| i as usize == n) {
            want.next();
            let r = at..at + ch.len_utf8();
            match out.last_mut() {
                Some(last) if last.end == r.start => last.end = r.end,
                _ => out.push(r),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headings(names: &[&str]) -> Vec<mdt_markdown::HeadingEntry> {
        let src: String = names
            .iter()
            .map(|n| format!("## {n}\n\ntext\n\n"))
            .collect();
        mdt_markdown::parse(&src).headings
    }

    fn labels(p: &Palette) -> Vec<String> {
        p.rows.iter().map(|r| p.entries[r.entry].label()).collect()
    }

    fn query(p: &mut Palette, q: &str) {
        p.query.set(q);
        p.update();
    }

    #[test]
    fn hash_lists_only_headings() {
        let mut p = Palette::new(
            &headings(&["Introduction", "Results", "Discussion", "Resources"]),
            vec![PathBuf::from("/notes/results.md")],
            "",
        );
        query(&mut p, "#res");
        let l = labels(&p);
        assert!(l.contains(&"Results".to_owned()));
        assert!(l.contains(&"Resources".to_owned()));
        assert!(
            p.rows
                .iter()
                .all(|r| matches!(p.entries[r.entry], Entry::Heading { .. })),
            "{l:?}"
        );
    }

    #[test]
    fn gt_lists_commands_with_shortcuts() {
        let mut p = Palette::new(&headings(&["Split results"]), vec![], "");
        query(&mut p, ">split");
        let first = &p.entries[p.rows[0].entry];
        let Entry::Command(c) = first else {
            panic!("a command first")
        };
        assert_eq!(c.name, "View: Split");
        assert!(
            p.rows
                .iter()
                .all(|r| matches!(p.entries[r.entry], Entry::Command(_)))
        );
        // A command with a shortcut shows it.
        query(&mut p, ">cycle mode");
        let Entry::Command(c) = &p.entries[p.rows[0].entry] else {
            panic!()
        };
        let want = if cfg!(target_os = "macos") {
            "Cmd+E"
        } else {
            "Ctrl+E"
        };
        assert_eq!(c.shortcut.as_deref(), Some(want));
    }

    #[test]
    fn out_of_order_characters_are_highlighted() {
        let mut p = Palette::new(&headings(&["Method", "Results"]), vec![], "");
        query(&mut p, "rsl");
        assert_eq!(labels(&p)[0], "Results");
        let label = "Results";
        let hl: String = p.rows[0]
            .matched
            .iter()
            .map(|r| &label[r.clone()])
            .collect();
        assert_eq!(hl.to_lowercase(), "rsl");
    }

    #[test]
    fn empty_query_orders_headings_recent_then_commands() {
        let p = Palette::new(
            &headings(&["Zeta", "Alpha"]),
            vec![PathBuf::from("/b.md"), PathBuf::from("/a.md")],
            "",
        );
        let l = labels(&p);
        assert_eq!(&l[..4], ["Zeta", "Alpha", "/b.md", "/a.md"]);
        assert!(matches!(p.entries[p.rows[4].entry], Entry::Command(_)));
    }

    #[test]
    fn every_command_is_listed_under_gt() {
        let mut p = Palette::new(&[], vec![], "");
        query(&mut p, ">");
        let l = labels(&p);
        for c in commands() {
            assert!(l.contains(&c.name.to_owned()), "{}", c.name);
        }
        for want in [
            "View: Reader",
            "View: Source",
            "View: Split",
            "Theme: Light",
            "Theme: Dark",
            "Theme: Follow System",
            "View: Zoom In",
            "View: Zoom Out",
            "View: Reset Zoom",
            "Settings: Toggle Soft Wrap",
            "Settings: Toggle Cursor Blink",
            "Settings: Toggle Remote Images",
            "Settings: Start in Reader",
            "Settings: Start in Source",
            "Settings: Start in Split",
            "View: Toggle Wide Reader",
            "App: About md-tex",
            "Edit: Copy as Markdown",
        ] {
            assert!(l.contains(&want.to_owned()), "{want}");
        }
        assert!(
            !l.iter().any(|n| n.starts_with("Move: ")),
            "editing keys left out"
        );
    }

    /// Every key in the settings file has a palette command that changes it.
    #[test]
    fn every_setting_has_a_command() {
        let keys: Vec<&str> = commands()
            .iter()
            .filter_map(|c| match c.run {
                Run::Setting(_, key) => Some(key),
                Run::Action(_) => None,
            })
            .collect();
        for key in crate::settings::KEYS {
            // Zoom and wide reader are registry commands with shortcuts.
            if key == "zoom" || key == "wide_reader" {
                continue;
            }
            assert!(keys.contains(&key), "no command sets `{key}`");
        }
        let mut s = crate::settings::DEFAULTS;
        for c in commands() {
            if let (Run::Setting(f, _), "Settings: Start in Split") = (c.run, c.name) {
                f(&mut s);
            }
        }
        assert_eq!(s.initial_view_mode, ViewMode::Split);
    }

    #[test]
    fn selection_scrolls_and_stops_at_the_ends() {
        let names: Vec<String> = (0..30).map(|i| format!("Heading {i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut p = Palette::new(&headings(&names), vec![], "#");
        p.move_by(-1);
        assert_eq!(p.selected, 0);
        for _ in 0..20 {
            p.move_by(1);
        }
        assert_eq!(p.selected, 20);
        assert_eq!(p.top, 20 + 1 - VISIBLE);
        for _ in 0..50 {
            p.move_by(1);
        }
        assert_eq!(p.selected, 29);
    }
}
