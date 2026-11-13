//! Non-blocking notices shown over the document's top-right corner. A notice stays until
//! the user dismisses it or picks one of its actions.

use gpui::SharedString;

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub id: u64,
    pub message: SharedString,
    /// Button labels, e.g. "Reload" and "Keep Mine". Empty for plain information.
    pub actions: Vec<SharedString>,
}

#[derive(Debug, Default)]
pub struct Notices {
    items: Vec<Notice>,
    next_id: u64,
}

impl Notices {
    pub fn push(&mut self, message: impl Into<SharedString>, actions: &[&'static str]) -> u64 {
        self.next_id += 1;
        let message = message.into();
        // The same message twice would only stack up; keep the newest.
        self.items.retain(|n| n.message != message);
        self.items.push(Notice {
            id: self.next_id,
            message,
            actions: actions.iter().map(|&a| a.into()).collect(),
        });
        self.next_id
    }

    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|n| n.id != id);
    }

    /// Dismisses the newest notice (Escape).
    pub fn dismiss_latest(&mut self) -> bool {
        self.items.pop().is_some()
    }

    pub fn items(&self) -> &[Notice] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dismiss_and_dedupe() {
        let mut n = Notices::default();
        let a = n.push("File changed on disk", &["Reload", "Keep Mine"]);
        let b = n.push("Anchor not found", &[]);
        n.push("Anchor not found", &[]);
        assert_eq!(n.items().len(), 2);
        n.dismiss(a);
        assert_eq!(n.items()[0].message, "Anchor not found");
        assert!(n.items()[0].id > b);
        assert!(n.dismiss_latest());
        assert!(!n.dismiss_latest());
    }
}
