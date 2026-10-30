//! Tray icons the user hid from the top bar ("Hide from top bar" on an icon's
//! right-click menu). They wait in the overflow popup instead. Stored one per
//! line in `%LOCALAPPDATA%\wdock\tray-hidden.txt`: the owning executable's
//! file name, then a tab and the icon's GUID in hex when the app gave one.

use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenApp {
    /// Lower-cased file name of the owning executable, e.g. `steam.exe`.
    pub exe: String,
    /// Only the icon with this GUID. Without one, every icon of the app.
    pub guid: Option<[u8; 16]>,
}

impl HiddenApp {
    /// The entry that hides the icon of `exe_path` (full path) with `guid`.
    pub fn for_icon(exe_path: &str, guid: Option<[u8; 16]>) -> Self {
        Self { exe: exe_name(exe_path).to_lowercase(), guid }
    }

    /// Whether this entry covers an icon of `exe_path` with `guid`.
    pub fn matches(&self, exe_path: &str, guid: Option<[u8; 16]>) -> bool {
        self.exe.eq_ignore_ascii_case(exe_name(exe_path)) && self.guid.is_none_or(|g| guid == Some(g))
    }
}

/// File name of a path, either separator.
pub fn exe_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

pub fn path() -> PathBuf {
    crate::paths::local_dir().join("tray-hidden.txt")
}

pub fn load() -> Vec<HiddenApp> {
    fs::read_to_string(path()).map(|text| parse(&text)).unwrap_or_default()
}

pub fn save(apps: &[HiddenApp]) {
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(err) = fs::write(&path, format(apps)) {
        log::warn!("could not save {}: {err}", path.display());
    }
}

/// Lines that do not parse are skipped.
pub fn parse(text: &str) -> Vec<HiddenApp> {
    text.lines()
        .filter_map(|line| {
            let (exe, guid) = match line.split_once('\t') {
                Some((exe, hex)) => (exe, Some(from_hex(hex.trim())?)),
                None => (line, None),
            };
            let exe = exe.trim();
            (!exe.is_empty()).then(|| HiddenApp { exe: exe.to_lowercase(), guid })
        })
        .collect()
}

pub fn format(apps: &[HiddenApp]) -> String {
    apps.iter()
        .map(|app| match app.guid {
            Some(guid) => format!("{}\t{}\n", app.exe, to_hex(&guid)),
            None => format!("{}\n", app.exe),
        })
        .collect()
}

fn to_hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID: [u8; 16] = [0xAB; 16];

    #[test]
    fn exe_only_entry_hides_every_icon_of_the_app() {
        let hidden = HiddenApp::for_icon(r"C:\Apps\Steam\steam.exe", None);
        assert!(hidden.matches(r"c:\apps\steam\steam.exe", None));
        assert!(hidden.matches(r"d:\elsewhere\STEAM.exe", Some(GUID)));
        assert!(!hidden.matches(r"c:\apps\other.exe", None));
    }

    #[test]
    fn guid_entry_hides_only_that_icon() {
        let hidden = HiddenApp::for_icon("x/app.exe", Some(GUID));
        assert!(hidden.matches("app.exe", Some(GUID)));
        assert!(!hidden.matches("app.exe", None));
        assert!(!hidden.matches("app.exe", Some([1; 16])));
    }

    #[test]
    fn file_round_trips_and_skips_bad_lines() {
        let apps = vec![HiddenApp::for_icon("a/Steam.exe", None), HiddenApp::for_icon("b/x.exe", Some(GUID))];
        assert_eq!(parse(&format(&apps)), apps);
        assert_eq!(
            parse("ok.exe\r\nbad.exe\tzz\n\n"),
            vec![HiddenApp { exe: "ok.exe".into(), guid: None }]
        );
    }
}
