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
    if let Err(err) = write(targets) {
        log::warn!("could not save pinned items to {}: {err}", path().display());
    }
}

fn write(targets: &[String]) -> std::io::Result<()> {
    let path = path();
    let mut text = targets.join("\n");
    text.push('\n');
    path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path, text))
}

/// Copy taskbar shortcuts so unpinning in Explorer does not break wdock pins.
fn own_shortcuts(targets: Vec<String>) -> std::io::Result<Vec<String>> {
    let dir = crate::paths::local_dir().join("imported-taskbar");
    let mut result = Vec::with_capacity(targets.len());
    for (index, target) in targets.into_iter().enumerate() {
        if target.to_lowercase().ends_with(".lnk") {
            let name = std::path::Path::new(&target).file_name()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "shortcut has no filename"))?;
            let entry_dir = dir.join(format!("{index:04}"));
            fs::create_dir_all(&entry_dir)?;
            let copy = entry_dir.join(name);
            fs::copy(&target, &copy)?;
            result.push(copy.to_string_lossy().into_owned());
        } else {
            result.push(target);
        }
    }
    Ok(result)
}

/// Import only when no wdock list exists. Failed imports are retried next run.
pub fn load_or_import(fallback: &[String]) -> Vec<String> {
    if let Some(saved) = load() { return saved; }
    if path().exists() {
        log::warn!("cannot read saved dock pins; keeping the file unchanged");
        return fallback.to_vec();
    }
    match crate::platform::taskbar_pins::read() {
        Ok(pins) => {
            let pins = match own_shortcuts(pins) {
                Ok(pins) => pins,
                Err(err) => {
                    log::warn!("could not copy Windows taskbar shortcuts: {err}; retrying on next start");
                    return fallback.to_vec();
                }
            };
            if let Err(err) = write(&pins) {
                log::warn!("could not save imported taskbar pins: {err}");
            } else {
                log::info!("imported {} taskbar pins", pins.len());
            }
            pins
        }
        Err(err) => {
            log::warn!("could not import Windows taskbar pins: {err}; retrying on next start");
            fallback.to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "imports current Windows taskbar into this user's wdock data"]
    fn imports_live_taskbar_once() {
        use windows::Win32::System::Com::{CoInitializeEx, CoCreateInstance, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap(); }
        assert!(super::load().is_none(), "existing wdock pins must not be overwritten");
        let expected = crate::platform::taskbar_pins::read().unwrap();
        assert!(!expected.is_empty());
        let actual = super::load_or_import(&[]);
        assert_eq!(actual.len(), expected.len());
        assert_eq!(super::load().unwrap(), actual);
        let wic: IWICImagingFactory = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap() };
        for (original, copied) in expected.iter().zip(&actual) {
            let old = crate::model::apps::Pinned::resolve(&wic, original, 32).unwrap();
            let new = crate::model::apps::Pinned::resolve(&wic, copied, 32).unwrap();
            assert_eq!(old.name, new.name);
            assert_eq!(old.aumid, new.aumid);
            assert_eq!(old.exe, new.exe);
            assert!(new.icon.is_some());
        }
        eprintln!("Imported {} taskbar pins in order", actual.len());
    }
}
