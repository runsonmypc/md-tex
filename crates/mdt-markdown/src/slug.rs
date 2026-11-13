//! GitHub-style heading anchors: lowercase, punctuation removed, spaces to hyphens, and
//! duplicates numbered `-1`, `-2`, …

use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct Slugger {
    used: HashSet<String>,
    /// For each base slug, the number its next duplicate tries first. Numbers below it
    /// are taken, and stay taken, so the search starts there: counting up from 0 for each
    /// of 3,800 equal headings (64 KB) took 2 s.
    next: HashMap<String, usize>,
}

impl Slugger {
    pub fn slug(&mut self, heading: &str) -> String {
        let base = base_slug(heading);
        let mut n = self.next.get(&base).copied().unwrap_or(0);
        loop {
            let candidate = if n == 0 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            n += 1;
            if self.used.insert(candidate.clone()) {
                self.next.insert(base, n);
                return candidate;
            }
        }
    }
}

pub fn base_slug(heading: &str) -> String {
    heading
        .trim()
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getting_started() {
        assert_eq!(
            Slugger::default().slug("Getting Started"),
            "getting-started"
        );
    }

    #[test]
    fn punctuation_and_unicode() {
        assert_eq!(base_slug("What's new in v1.2?"), "whats-new-in-v12");
        assert_eq!(base_slug("Übersicht & Zürich"), "übersicht--zürich");
        assert_eq!(base_slug("snake_case-name"), "snake_case-name");
    }

    #[test]
    fn duplicates_are_numbered() {
        let mut s = Slugger::default();
        assert_eq!(s.slug("Notes"), "notes");
        assert_eq!(s.slug("Notes"), "notes-1");
        assert_eq!(s.slug("Notes"), "notes-2");
        assert_eq!(s.slug("Notes 1"), "notes-1-1");
        assert_eq!(s.slug("Notes 1"), "notes-1-2");
        assert_eq!(s.slug("Notes"), "notes-3");
    }

    /// Each duplicate starts where the last one of its base stopped: numbering every one
    /// of n equal headings from 0 took time quadratic in n (3,800 took 2 s).
    #[test]
    fn many_duplicates_take_linear_time() {
        let mut s = Slugger::default();
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            s.slug("Same heading");
        }
        assert_eq!(s.slug("Same heading"), "same-heading-100000");
        assert_eq!(s.slug("Same heading 5"), "same-heading-5-1");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }
}
