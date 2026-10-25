//! Pinned dock items changed from the dock itself, one target per line in
//! `%LOCALAPPDATA%\wdock\pinned.txt`. When present it overrides the config's
//! list, so the user's comments in config.toml are never rewritten.

use std::fs;
use std::path::PathBuf;

fn path() -> PathBuf {
    crate::paths::local_dir().join("pinned.txt")
}

pub fn load() -> Option<Vec<String>> {
    let text = fs::read_to_string(path()).ok()?;
    Some(text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect())
}

pub fn save(targets: &[String]) {
    if cfg!(test) {
        return;
    }
    let path = path();
    let mut text = targets.join("\n");
    text.push('\n');
    let written = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path, text));
    if let Err(err) = written {
        log::warn!("could not save pinned items to {}: {err}", path.display());
    }
}
