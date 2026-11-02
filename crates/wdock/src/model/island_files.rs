//! Bounded local file/folder references shared by Bookmarks and the Island shelf.
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

pub const MAX_FILES: usize = 32;
const MAX_GROUPS: usize = 16;
const MAX_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bookmark {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Data {
    version: u32,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    items: Vec<Bookmark>,
}
impl Default for Data {
    fn default() -> Self { Self { version: 1, groups: Vec::new(), items: Vec::new() } }
}

#[derive(Default)]
struct Shelf {
    data: Data,
    pending: Vec<PathBuf>,
    error: Option<String>,
    file: Option<PathBuf>,
}
thread_local! { static SHELF: RefCell<Option<Shelf>> = const { RefCell::new(None) }; }
fn path() -> PathBuf { crate::paths::local_dir().join("bookmarks.toml") }
fn key(path: &Path) -> String { path.to_string_lossy().to_lowercase() }
fn valid_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    path.is_absolute() && value.len() <= 32768 && !value.contains('"') && !value.chars().any(char::is_control)
}
fn add_unique(data: &mut Data, path: PathBuf) -> bool {
    if !valid_path(&path) || data.items.len() >= MAX_FILES || data.items.iter().any(|i| key(&i.path) == key(&path)) { return false; }
    data.items.push(Bookmark { path, group: None, pinned: false });
    true
}
fn parse_legacy(text: &str) -> Data {
    let mut data = Data::default();
    for line in text.lines().filter(|s| !s.is_empty()) { add_unique(&mut data, PathBuf::from(line)); }
    data
}
fn valid_group(name: &str) -> bool { !name.trim().is_empty() && name.chars().count() <= 48 && !name.chars().any(char::is_control) }
fn decode(text: &str) -> Result<Data, String> {
    let data: Data = toml::from_str(text).map_err(|e| format!("Cannot read bookmarks: {e}"))?;
    if data.version != 1 || data.items.len() > MAX_FILES || data.groups.len() > MAX_GROUPS { return Err("Unsupported or oversized bookmarks file; original preserved.".into()); }
    let mut seen = Vec::new();
    for name in &data.groups {
        if !valid_group(name) || seen.contains(&name.to_lowercase()) { return Err("Invalid bookmark groups; original preserved.".into()); }
        seen.push(name.to_lowercase());
    }
    let mut seen = Vec::new();
    for item in &data.items {
        if !valid_path(&item.path) || seen.contains(&key(&item.path)) || item.group.as_ref().is_some_and(|g| !data.groups.contains(g)) { return Err("Invalid bookmarks; original preserved.".into()); }
        seen.push(key(&item.path));
    }
    Ok(data)
}
fn read_bounded(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_BYTES { return Err(std::io::Error::other("bookmarks file exceeds 256 KB")); }
    let mut text = String::new();
    file.take(MAX_BYTES + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_BYTES { return Err(std::io::Error::other("bookmarks file exceeds 256 KB")); }
    Ok(text)
}
fn load(file: &Path, legacy: &Path) -> Result<Data, String> {
    match read_bounded(file) {
        Ok(text) => decode(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let data = match read_bounded(legacy) {
                Ok(text) => parse_legacy(&text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
                Err(e) => return Err(format!("Cannot read old file shelf: {e}")),
            };
            // Leave the legacy file intact as a recovery copy.
            persist(file, &data)?;
            Ok(data)
        }
        Err(e) => Err(format!("Cannot read bookmarks: {e}")),
    }
}
fn persist(file: &Path, data: &Data) -> Result<(), String> {
    commit(Some(file), data)
}
fn commit(file: Option<&Path>, data: &Data) -> Result<(), String> {
    let text = toml::to_string(data).map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_BYTES { return Err("Bookmarks exceed the 256 KB storage limit. Shorten paths or remove a reference.".into()); }
    if let Some(file) = file { crate::model::island_storage::write(file, &text).map_err(|e| format!("Cannot save bookmarks: {e}"))?; }
    Ok(())
}
pub fn init() {
    SHELF.with_borrow_mut(|slot| {
        if slot.is_some() { return; }
        *slot = Some(match load(&path(), &crate::paths::local_dir().join("island-files.txt")) {
            Ok(data) => Shelf { data, file: Some(path()), ..Default::default() },
            Err(error) => { log::warn!("{error}"); Shelf { error: Some(error), ..Default::default() } }
        });
    });
}
#[cfg(test)]
pub fn init_empty() { SHELF.set(Some(Shelf::default())); }
pub fn error() -> Option<String> { SHELF.with_borrow(|s| s.as_ref().and_then(|s| s.error.clone())) }
pub fn groups() -> Vec<String> { SHELF.with_borrow(|s| s.as_ref().map(|s| s.data.groups.clone()).unwrap_or_default()) }
pub fn entries(query: &str) -> Vec<Bookmark> {
    let query = query.to_lowercase();
    let mut items = SHELF.with_borrow(|s| s.as_ref().map(|s| s.data.items.iter().filter(|i| {
        query.split_whitespace().all(|q| i.path.to_string_lossy().to_lowercase().contains(q) || i.group.as_ref().is_some_and(|g| g.to_lowercase().contains(q)))
    }).cloned().collect::<Vec<_>>()).unwrap_or_default());
    items.sort_by_key(|i| !i.pinned);
    items
}
pub fn list() -> Vec<PathBuf> { entries("").into_iter().map(|i| i.path).collect() }
fn edit(change: impl FnOnce(&mut Data) -> Result<(), String>) -> Result<(), String> {
    SHELF.with_borrow_mut(|slot| {
        let s = slot.as_mut().ok_or("Bookmarks are not initialized")?;
        if let Some(e) = &s.error { return Err(e.clone()); }
        let mut data = s.data.clone();
        change(&mut data)?;
        commit(s.file.as_deref(), &data)?;
        s.data = data;
        Ok(())
    })
}
pub fn add(path: PathBuf) -> Result<(), String> { edit(|d| if add_unique(d, path) { Ok(()) } else { Err("Use a unique absolute path (32 bookmarks maximum).".into()) }) }
pub fn add_group(name: &str) -> Result<(), String> {
    let name = name.trim();
    edit(|d| {
        if !valid_group(name) || d.groups.len() >= MAX_GROUPS || d.groups.iter().any(|g| g.to_lowercase() == name.to_lowercase()) { return Err("Choose a unique group name (1–48 characters; 16 groups maximum).".into()); }
        d.groups.push(name.into()); Ok(())
    })
}
pub fn rename_group(old: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    edit(|d| {
        if !valid_group(name) || d.groups.iter().any(|g| g != old && g.to_lowercase() == name.to_lowercase()) { return Err("Choose a unique group name (1–48 characters).".into()); }
        let group = d.groups.iter_mut().find(|g| *g == old).ok_or("Group no longer exists")?;
        *group = name.into();
        for item in &mut d.items { if item.group.as_deref() == Some(old) { item.group = Some(name.into()); } }
        Ok(())
    })
}
pub fn delete_group(name: &str) -> Result<(), String> { edit(|d| { delete_group_from(d, name); Ok(()) }) }
fn delete_group_from(data: &mut Data, name: &str) {
    data.groups.retain(|g| g != name);
    for item in &mut data.items { if item.group.as_deref() == Some(name) { item.group = None; } }
}
pub fn move_to(path: &Path, group: Option<&str>) -> Result<(), String> {
    edit(|d| {
        if group.is_some_and(|g| !d.groups.iter().any(|n| n == g)) { return Err("Group no longer exists".into()); }
        let item = d.items.iter_mut().find(|i| i.path == path).ok_or("Bookmark no longer exists")?;
        item.group = group.map(str::to_owned); Ok(())
    })
}
pub fn toggle_pin(path: &Path) -> Result<(), String> {
    edit(|d| { let i = d.items.iter_mut().find(|i| i.path == path).ok_or("Bookmark no longer exists")?; i.pinned = !i.pinned; Ok(()) })
}
pub fn remove_path(path: &Path) -> Result<(), String> { edit(|d| { d.items.retain(|i| i.path != path); Ok(()) }) }
pub fn remove(index: usize) -> bool { list().get(index).is_some_and(|p| remove_path(p).is_ok()) }
pub fn queue(paths: Vec<PathBuf>) -> usize {
    SHELF.with_borrow_mut(|slot| {
        let Some(s) = slot else { return 0; };
        if s.error.is_some() { return 0; }
        let mut combined = s.data.clone();
        for p in &s.pending { add_unique(&mut combined, p.clone()); }
        let mut accepted = 0;
        for p in paths { if add_unique(&mut combined, p.clone()) { s.pending.push(p); accepted += 1; } }
        accepted
    })
}
pub fn accept_pending() -> bool {
    SHELF.with_borrow_mut(|slot| {
        let Some(s) = slot else { return false; };
        if s.error.is_some() || s.pending.is_empty() { return false; }
        let mut data = s.data.clone();
        for p in &s.pending { add_unique(&mut data, p.clone()); }
        match commit(s.file.as_deref(), &data) {
            Ok(()) => { s.pending.clear(); s.data = data; true }
            Err(e) => { log::warn!("{e}"); false }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_deduplicates_and_bounds_without_reading_targets() {
        let mut text = "C:\\Work\\File.txt\nC:\\work\\file.txt\nrelative.txt\nD:\\Other\n".to_owned();
        for i in 0..40 { text.push_str(&format!("C:\\f{i}\n")); }
        let data = parse_legacy(&text);
        assert_eq!(data.items.len(), MAX_FILES);
        assert_eq!(data.items[0].path, PathBuf::from(r"C:\Work\File.txt"));
        assert!(data.items.iter().all(|i| !i.pinned && i.group.is_none()));
        assert_eq!(decode(&toml::to_string(&data).unwrap()).unwrap().items, data.items);
    }
    #[test]
    fn removing_group_keeps_references_ungrouped() {
        let mut data = parse_legacy("C:\\file.txt\nD:\\folder");
        data.groups.push("Work".into());
        data.items[0].group = Some("Work".into());
        delete_group_from(&mut data, "Work");
        assert!(data.groups.is_empty());
        assert_eq!(data.items.len(), 2);
        assert!(data.items.iter().all(|i| i.group.is_none()));
    }
    #[test]
    fn rejects_corrupt_and_unsupported_data() {
        assert!(decode("broken = [").is_err());
        assert!(decode("version = 2").is_err());
        assert!(decode("version = 1\ngroups = ['Work', 'work']").is_err());
        let mut data = parse_legacy("C:\\file"); data.items[0].group = Some("missing".into());
        assert!(decode(&toml::to_string(&data).unwrap()).is_err());
    }
    #[test]
    fn groups_move_rename_pin_search_and_reference_removal() {
        init_empty();
        add(PathBuf::from(r"C:\Work\one.txt")).unwrap();
        add(PathBuf::from(r"D:\two")).unwrap();
        add_group("Work").unwrap();
        assert!(add_group("work").is_err());
        move_to(Path::new(r"C:\Work\one.txt"), Some("Work")).unwrap();
        rename_group("Work", "Projects").unwrap();
        assert_eq!(entries("projects one").len(), 1);
        assert_eq!(entries("projects missing").len(), 0);
        toggle_pin(Path::new(r"D:\two")).unwrap();
        assert_eq!(list()[0], PathBuf::from(r"D:\two"));
        delete_group("Projects").unwrap();
        assert!(entries("").iter().all(|i| i.group.is_none()));
        assert_eq!(list().len(), 2);
        assert!(remove(0));
        assert_eq!(list(), vec![PathBuf::from(r"C:\Work\one.txt")]);
    }
    #[test]
    fn oversized_transaction_rolls_back_and_unicode_groups_round_trip() {
        init_empty();
        add_group("Nhóm").unwrap();
        assert!(add_group("NHÓM").is_err());
        for i in 0..MAX_FILES {
            let before = list();
            let result = add(PathBuf::from(format!(r"C:\{i}{}", "a".repeat(32000))));
            if result.is_err() { assert_eq!(before, list()); assert!(!before.is_empty()); return; }
        }
        panic!("byte limit must reject oversized serialization");
    }
    #[test]
    fn failed_write_keeps_the_previous_references() {
        init_empty();
        add(PathBuf::from(r"C:\kept")).unwrap();
        let blocker=std::env::temp_dir().join(format!("wdock-bookmark-blocker-{}",std::process::id()));
        std::fs::write(&blocker,"occupied").unwrap();
        SHELF.with_borrow_mut(|s| s.as_mut().unwrap().file=Some(blocker.join("bookmarks.toml")));
        assert!(add(PathBuf::from(r"D:\new")).is_err());
        assert_eq!(list(),vec![PathBuf::from(r"C:\kept")]);
        assert_eq!(std::fs::read_to_string(&blocker).unwrap(),"occupied");
        std::fs::remove_file(blocker).unwrap();
    }
    #[test]
    fn corrupt_new_file_is_never_replaced_by_legacy() {
        let dir = std::env::temp_dir().join(format!("wdock-bookmarks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bookmarks.toml"); let old = dir.join("island-files.txt");
        std::fs::write(&file, "broken = [").unwrap(); std::fs::write(&old, "C:\\file").unwrap();
        assert!(load(&file, &old).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "broken = [");
        std::fs::remove_file(&file).unwrap();
        assert_eq!(load(&file, &old).unwrap().items.len(), 1);
        assert!(old.exists());
        std::fs::remove_file(&file).unwrap(); std::fs::remove_file(&old).unwrap(); std::fs::remove_dir(&dir).unwrap();
    }
}
