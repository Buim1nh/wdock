//! Opt-in, bounded clipboard text held only for this shell session.

use std::cell::RefCell;

pub const MAX_ENTRIES: usize = 100;
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_ENTRY_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: u64,
    pub text: String,
    pub pinned: bool,
}

#[derive(Default)]
struct History {
    entries: Vec<Entry>,
    bytes: usize,
    next_id: u64,
    enabled: bool,
    paused: bool,
}

impl History {
    fn insert(&mut self, text: String) -> bool {
        if !self.enabled || self.paused || text.is_empty() || text.len() > MAX_ENTRY_BYTES {
            return false;
        }
        if let Some(index) = self.entries.iter().position(|entry| entry.text == text) {
            if index == 0 {
                return false;
            }
            let entry = self.entries.remove(index);
            self.entries.insert(0, entry);
            return true;
        }
        let pinned_bytes: usize = self.entries.iter().filter(|entry| entry.pinned).map(|entry| entry.text.len()).sum();
        if self.entries.iter().filter(|entry| entry.pinned).count() == MAX_ENTRIES
            || pinned_bytes + text.len() > MAX_BYTES
        {
            return false;
        }
        while self.entries.len() >= MAX_ENTRIES || self.bytes + text.len() > MAX_BYTES {
            let Some(index) = self.entries.iter().rposition(|entry| !entry.pinned) else {
                return false;
            };
            self.bytes -= self.entries.remove(index).text.len();
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.bytes += text.len();
        self.entries.insert(0, Entry { id: self.next_id, text, pinned: false });
        true
    }

    fn remove(&mut self, id: u64) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        self.bytes -= self.entries.remove(index).text.len();
        true
    }

    fn clear_unpinned(&mut self) {
        self.entries.retain(|entry| entry.pinned);
        self.bytes = self.entries.iter().map(|entry| entry.text.len()).sum();
    }
}

thread_local! {
    static HISTORY: RefCell<History> = RefCell::default();
}

pub fn list() -> Vec<Entry> {
    HISTORY.with_borrow(|history| history.entries.clone())
}

pub fn enabled() -> bool {
    HISTORY.with_borrow(|history| history.enabled)
}

pub fn paused() -> bool {
    HISTORY.with_borrow(|history| history.paused)
}

pub fn set_paused(paused: bool) {
    HISTORY.with_borrow_mut(|history| history.paused = history.enabled && paused);
    // Invalidates a read already waiting on another app's delayed rendering.
    crate::platform::clipboard::skip_current();
}

pub fn toggle_pin(id: u64) -> bool {
    HISTORY.with_borrow_mut(|history| {
        let Some(entry) = history.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.pinned = !entry.pinned;
        true
    })
}

pub fn remove(id: u64) -> bool {
    HISTORY.with_borrow_mut(|history| history.remove(id))
}

pub fn clear_unpinned() {
    crate::platform::clipboard::skip_current();
    HISTORY.with_borrow_mut(History::clear_unpinned);
}

pub fn clear_all() {
    crate::platform::clipboard::skip_current();
    HISTORY.with_borrow_mut(|history| {
        history.entries.clear();
        history.bytes = 0;
    });
}

pub(crate) fn configure(enabled: bool) -> bool {
    let changed = HISTORY.with_borrow_mut(|history| {
        let changed = history.enabled != enabled;
        history.enabled = enabled;
        if !enabled {
            history.paused = false;
        }
        changed
    });
    if !enabled && changed {
        clear_all();
    }
    changed
}

pub(crate) fn insert(text: String) -> bool {
    HISTORY.with_borrow_mut(|history| history.insert(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history() -> History {
        History { enabled: true, ..Default::default() }
    }

    #[test]
    fn exact_duplicates_move_front_and_keep_pin_and_id() {
        let mut history = history();
        assert!(history.insert("Tiếng Việt 🦀".into()));
        let id = history.entries[0].id;
        history.entries[0].pinned = true;
        assert!(history.insert("other".into()));
        assert!(history.insert("Tiếng Việt 🦀".into()));
        assert_eq!(history.entries[0], Entry { id, text: "Tiếng Việt 🦀".into(), pinned: true });
        assert_eq!(history.bytes, "Tiếng Việt 🦀other".len());
        assert!(!history.insert("Tiếng Việt 🦀".into()));
        assert!(history.insert("tiếng Việt 🦀".into()));
    }

    #[test]
    fn entry_limit_is_utf8_bytes_and_preserves_whitespace() {
        let mut history = history();
        assert!(!history.insert(String::new()));
        assert!(history.insert(" \n\t".into()));
        assert!(history.insert("🦀".repeat(MAX_ENTRY_BYTES / 4)));
        assert!(!history.insert("🦀".repeat(MAX_ENTRY_BYTES / 4 + 1)));
        assert!(!history.insert("x".repeat(MAX_ENTRY_BYTES + 1)));
        assert_eq!(history.entries.len(), 2);
    }

    #[test]
    fn count_eviction_preserves_old_pins() {
        let mut history = history();
        history.insert("keep".into());
        let pinned = history.entries[0].id;
        history.entries[0].pinned = true;
        for index in 0..MAX_ENTRIES + 5 {
            history.insert(index.to_string());
        }
        assert_eq!(history.entries.len(), MAX_ENTRIES);
        assert!(history.entries.iter().any(|entry| entry.id == pinned));
        assert!(!history.entries.iter().any(|entry| entry.text == "0"));
        assert_eq!(history.entries[0].text, (MAX_ENTRIES + 4).to_string());
    }

    #[test]
    fn byte_limit_includes_pins_and_rejects_without_partial_eviction() {
        let mut history = history();
        for index in 0..MAX_BYTES / MAX_ENTRY_BYTES {
            let text = format!("{index:04}{}", "x".repeat(MAX_ENTRY_BYTES - 4));
            assert!(history.insert(text));
            history.entries[0].pinned = true;
        }
        assert_eq!(history.bytes, MAX_BYTES);
        assert!(!history.insert("extra".into()));
        assert_eq!(history.bytes, MAX_BYTES);
        history.entries[0].pinned = false;
        assert!(history.insert("extra".into()));
        assert_eq!(history.bytes, MAX_BYTES - MAX_ENTRY_BYTES + 5);
        assert_eq!(history.entries.len(), MAX_BYTES / MAX_ENTRY_BYTES);
    }

    #[test]
    fn all_pinned_count_rejects_new_but_accepts_existing() {
        let mut history = history();
        for index in 0..MAX_ENTRIES {
            history.insert(index.to_string());
            history.entries[0].pinned = true;
        }
        assert!(!history.insert("extra".into()));
        assert!(history.insert("0".into()));
        assert!(history.entries[0].pinned);
        assert_eq!(history.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn rejected_entry_does_not_evict_smaller_unpinned_items() {
        let mut history = history();
        for index in 0..MAX_BYTES / MAX_ENTRY_BYTES - 1 {
            history.insert(format!("{index:04}{}", "x".repeat(MAX_ENTRY_BYTES - 4)));
            history.entries[0].pinned = true;
        }
        history.insert("y".repeat(MAX_ENTRY_BYTES - 8));
        history.entries[0].pinned = true;
        history.insert("12345678".into());
        assert_eq!(history.bytes, MAX_BYTES);
        assert!(!history.insert("123456789".into()));
        assert_eq!(history.entries[0].text, "12345678");
        assert_eq!(history.bytes, MAX_BYTES);
    }

    #[test]
    fn clear_and_remove_reclaim_bytes_and_pause_prevents_capture() {
        let mut history = history();
        history.insert("pin".into());
        history.entries[0].pinned = true;
        let id = history.entries[0].id;
        history.insert("unpin".into());
        history.clear_unpinned();
        assert_eq!(history.bytes, 3);
        assert!(history.remove(id));
        assert!(!history.remove(id));
        assert_eq!(history.bytes, 0);
        history.paused = true;
        assert!(!history.insert("paused".into()));
        history.paused = false;
        history.enabled = false;
        assert!(!history.insert("disabled".into()));
    }
}
