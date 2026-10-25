//! Who a window belongs to, and how pinned entries are recognised.
//!
//! A window's identity is its AppUserModelID when it has one (explicit on the
//! window, or assigned by its package), plus its executable path. Pinned items
//! match a window by either, so `shell:AppsFolder\MSEdge` and a plain
//! `msedge.exe` path both pick up Edge's windows.

use std::path::Path;

use windows::Win32::Foundation::{HWND, PROPERTYKEY};
use windows::Win32::Graphics::Imaging::{IWICBitmapSource, IWICImagingFactory};
use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_Link_TargetParsingPath};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};
use windows::core::{HSTRING, Interface};

use crate::paths;
use crate::platform::wininfo;
use crate::services::icons;

const APPS_FOLDER: &str = r"shell:appsfolder\";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppKey {
    Aumid(String),
    Exe(String),
}

/// Lower-cased so comparisons are case-insensitive, like the shell's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub aumid: Option<String>,
    pub exe: String,
}

impl Identity {
    pub fn key(&self) -> AppKey {
        match &self.aumid {
            Some(aumid) => AppKey::Aumid(aumid.clone()),
            None => AppKey::Exe(self.exe.clone()),
        }
    }

    pub fn of_window(hwnd: HWND) -> Self {
        let process = wininfo::process_info(wininfo::content_process_id(hwnd));
        let aumid = wininfo::explicit_aumid(hwnd).or_else(|| process.as_ref().and_then(|p| p.package_aumid.clone()));
        let exe = match process {
            Some(process) => process.exe,
            // e.g. a process we may not query: keep the window on its own entry.
            None => format!("hwnd:{:x}", hwnd.0 as usize),
        };
        Self { aumid: aumid.map(|a| a.to_lowercase()), exe: exe.to_lowercase() }
    }
}

pub struct Pinned {
    pub target: String,
    pub name: String,
    pub aumid: Option<String>,
    pub exe: Option<String>,
    pub icon: Option<IWICBitmapSource>,
}

impl Pinned {
    pub fn matches(&self, identity: &Identity) -> bool {
        self.aumid.as_ref().is_some_and(|a| identity.aumid.as_ref() == Some(a))
            || self.exe.as_ref().is_some_and(|e| *e == identity.exe)
    }

    pub fn resolve(wic: &IWICImagingFactory, target: &str, icon_px: u32) -> Option<Self> {
        let item: IShellItem2 = match unsafe { SHCreateItemFromParsingName(&HSTRING::from(target), None) } {
            Ok(item) => item,
            Err(err) => {
                log::warn!("skipping pinned item {target}: {err}");
                return None;
            }
        };
        let icon = icons::load_item(wic, &item.cast().ok()?, icon_px);
        if let Err(err) = &icon {
            log::warn!("no icon for pinned item {target}: {err}");
        }

        let lower = target.to_lowercase();
        let aumid = lower
            .strip_prefix(APPS_FOLDER)
            .map(str::to_owned)
            .or_else(|| item_string(&item, &PKEY_AppUserModel_ID).map(|a| a.to_lowercase()));
        let exe = item_string(&item, &PKEY_Link_TargetParsingPath)
            .map(|p| p.to_lowercase())
            .filter(|p| p.ends_with(".exe"))
            .or_else(|| lower.ends_with(".exe").then(|| lower.clone()))
            .or_else(|| {
                // File Explorer's AppsFolder entry has no link target, but its windows run in explorer.exe.
                (aumid.as_deref() == Some("microsoft.windows.explorer"))
                    .then(|| paths::explorer_exe().to_string_lossy().to_lowercase())
            });

        let (name, icon) = match icon {
            Ok(icon) => (icon.name, Some(icon.image)),
            Err(_) => (target.to_owned(), None),
        };
        log::debug!("pinned {name}: aumid={aumid:?} exe={exe:?}");
        Some(Self { target: target.to_owned(), name, aumid, exe, icon })
    }
}

/// Name, icon and launch target for an app that is running but not pinned.
pub struct AppInfo {
    pub name: String,
    pub icon: Option<IWICBitmapSource>,
    pub launch: String,
    pub first_seen: u64,
}

impl AppInfo {
    pub fn resolve(wic: Option<&IWICImagingFactory>, identity: &Identity, icon_px: u32, first_seen: u64) -> Self {
        let fallback_name = Path::new(&identity.exe)
            .file_stem()
            .map_or_else(|| identity.exe.clone(), |s| s.to_string_lossy().into_owned());
        let Some(wic) = wic else {
            return Self { name: fallback_name, icon: None, launch: identity.exe.clone(), first_seen };
        };

        if let Some(aumid) = &identity.aumid {
            let target = format!(r"shell:AppsFolder\{aumid}");
            if let Ok(icon) = icons::load(wic, &target, icon_px) {
                return Self { name: icon.name, icon: Some(icon.image), launch: target, first_seen };
            }
        }
        let icon = icons::load(wic, &identity.exe, icon_px).ok().map(|i| i.image);
        let name = wininfo::file_description(&identity.exe).unwrap_or(fallback_name);
        Self { name, icon, launch: identity.exe.clone(), first_seen }
    }
}

fn item_string(item: &IShellItem2, key: &PROPERTYKEY) -> Option<String> {
    unsafe {
        let value = item.GetString(key).ok()?;
        let text = value.to_string().ok();
        CoTaskMemFree(Some(value.0 as *const _));
        text.filter(|t| !t.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned(aumid: Option<&str>, exe: Option<&str>) -> Pinned {
        Pinned {
            target: String::new(),
            name: String::new(),
            aumid: aumid.map(str::to_owned),
            exe: exe.map(str::to_owned),
            icon: None,
        }
    }

    #[test]
    fn pinned_matches_by_aumid_or_exe() {
        let edge = Identity { aumid: Some("msedge".into()), exe: r"c:\edge\msedge.exe".into() };
        assert!(pinned(Some("msedge"), None).matches(&edge));
        assert!(pinned(None, Some(r"c:\edge\msedge.exe")).matches(&edge));
        assert!(!pinned(Some("chrome"), Some(r"c:\chrome.exe")).matches(&edge));
    }

    #[test]
    fn key_prefers_aumid() {
        let packaged = Identity { aumid: Some("app!id".into()), exe: "x.exe".into() };
        let plain = Identity { aumid: None, exe: "x.exe".into() };
        assert_eq!(packaged.key(), AppKey::Aumid("app!id".into()));
        assert_eq!(plain.key(), AppKey::Exe("x.exe".into()));
    }
}
