//! References parked in the Island file shelf. File contents are never copied.
use std::cell::RefCell;
use std::path::PathBuf;

const MAX_FILES: usize = 32;

#[derive(Default)]
struct Shelf {
    paths: Vec<PathBuf>,
    pending: Vec<PathBuf>,
    writable: bool,
}

thread_local! { static SHELF: RefCell<Option<Shelf>> = const { RefCell::new(None) }; }

fn path() -> PathBuf { crate::paths::local_dir().join("island-files.txt") }

fn parse(text: &str) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for line in text.lines().filter(|s| !s.is_empty()) {
        let candidate = PathBuf::from(line);
        add_unique(&mut result, candidate);
    }
    result
}

fn add_unique(paths: &mut Vec<PathBuf>, candidate: PathBuf) -> bool {
    if !candidate.is_absolute() || paths.len() >= MAX_FILES { return false; }
    let key = candidate.to_string_lossy().to_lowercase();
    if paths.iter().any(|p| p.to_string_lossy().to_lowercase() == key) { return false; }
    paths.push(candidate);
    true
}

pub fn init() {
    SHELF.with_borrow_mut(|slot| {
        if slot.is_none() {
            let (paths, writable) = match std::fs::read_to_string(path()) {
                Ok(text) => (parse(&text), true),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Vec::new(), true),
                Err(err) => { log::warn!("cannot read file shelf: {err}"); (Vec::new(), false) }
            };
            *slot = Some(Shelf { paths, pending: Vec::new(), writable });
        }
    });
}

#[cfg(test)]
pub fn init_empty() {
    SHELF.set(Some(Shelf::default()));
}

pub fn list() -> Vec<PathBuf> {
    SHELF.with_borrow(|slot| slot.as_ref().map(|s| s.paths.clone()).unwrap_or_default())
}

pub fn queue(paths: Vec<PathBuf>) -> usize {
    SHELF.with_borrow_mut(|slot| {
        let Some(s) = slot else { return 0; };
        let mut combined = s.paths.clone();
        for path in &s.pending { add_unique(&mut combined, path.clone()); }
        let mut accepted = 0;
        for path in paths {
            if add_unique(&mut combined, path.clone()) {
                s.pending.push(path);
                accepted += 1;
            }
        }
        accepted
    })
}

pub fn accept_pending() -> bool {
    SHELF.with_borrow_mut(|slot| {
        let Some(shelf) = slot else { return false; };
        let mut changed = false;
        for path in std::mem::take(&mut shelf.pending) {
            changed |= add_unique(&mut shelf.paths, path);
        }
        if changed && shelf.writable { save(&shelf.paths); }
        changed
    })
}

pub fn remove(index: usize) -> bool {
    SHELF.with_borrow_mut(|slot| {
        let Some(s) = slot else { return false; };
        if index >= s.paths.len() { return false; }
        s.paths.remove(index);
        if s.writable { save(&s.paths); }
        true
    })
}

fn save(paths: &[PathBuf]) {
    let path = path();
    let text = paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n");
    if let Err(err) = crate::model::island_storage::write(&path, &text) {
        log::warn!("cannot save file shelf: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_unique_absolute_paths_and_limits_entries() {
        let text = "C:\\Work\\File.txt\nC:\\work\\file.txt\nrelative.txt\nD:\\Other\n";
        let paths = parse(text);
        assert_eq!(paths.len(), 2);
        let mut paths = Vec::new();
        for i in 0..40 { add_unique(&mut paths, PathBuf::from(format!(r"C:\f{i}"))); }
        assert_eq!(paths.len(), MAX_FILES);
    }
}
