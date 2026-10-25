//! What the launcher can open: entries of the shell's Apps folder (Start
//! Menu shortcuts and packaged apps) plus built-in settings and commands.
//!
//! Listing the Apps folder takes a few hundred milliseconds, so it runs on a
//! worker thread that sends back plain strings. Launch counts are kept in
//! `%LOCALAPPDATA%\wdock\usage.txt` ("count<TAB>key" per line) for ranking.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICBitmapSource, IWICImagingFactory};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, SHCreateItemFromParsingName, SIGDN, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Result, w};

use crate::model::search::{self, Candidate, Target};
use crate::paths;
use crate::services::icons;

/// Posted to the main window when a fresh listing is ready.
pub const WM_APP_INDEX: u32 = WM_APP + 21;

/// Re-list at most this often; app installs are rare.
const REFRESH_AFTER: Duration = Duration::from_secs(120);

/// Documentation and uninstaller shortcuts that clutter the Start Menu.
const SKIPPED_EXTENSIONS: &[&str] = &[".url", ".chm", ".txt", ".pdf", ".htm", ".html", ".rtf", ".ini"];

struct Index {
    apps: Vec<Candidate>,
    builtins: Vec<Candidate>,
    usage: HashMap<String, u32>,
    listed_at: Option<Instant>,
    pending: Option<Receiver<Vec<Candidate>>>,
    wic: Option<IWICImagingFactory>,
    icons: HashMap<String, Option<IWICBitmapSource>>,
}

thread_local! {
    static INDEX: RefCell<Option<Index>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Index) -> R) -> Option<R> {
    INDEX.with(|i| i.try_borrow_mut().ok()?.as_mut().map(f))
}

fn usage_path() -> PathBuf {
    paths::local_dir().join("usage.txt")
}

fn load_usage() -> HashMap<String, u32> {
    let Ok(text) = fs::read_to_string(usage_path()) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let (count, key) = line.split_once('\t')?;
            Some((key.to_owned(), count.parse().ok()?))
        })
        .collect()
}

fn save_usage(usage: &HashMap<String, u32>) {
    let text: String = usage.iter().map(|(key, count)| format!("{count}\t{key}\n")).collect();
    if let Err(err) = fs::create_dir_all(paths::local_dir()).and_then(|()| fs::write(usage_path(), text)) {
        log::warn!("could not save launcher usage: {err}");
    }
}

/// Loads usage stats and starts the first listing; `target` gets `WM_APP_INDEX`.
pub fn start(target: HWND) {
    let wic = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok();
    INDEX.set(Some(Index {
        apps: Vec::new(),
        builtins: search::builtins(),
        usage: load_usage(),
        listed_at: None,
        pending: None,
        wic,
        icons: HashMap::new(),
    }));
    refresh(target);
}

pub fn stop() {
    INDEX.set(None);
}

/// Starts a background listing unless one is running or the last is recent.
pub fn refresh(target: HWND) {
    with(|index| {
        let fresh = index.listed_at.is_some_and(|t| t.elapsed() < REFRESH_AFTER);
        if fresh || index.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let target = target.0 as isize;
        let spawned = thread::Builder::new().name("apps-index".into()).spawn(move || {
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            let started = Instant::now();
            match list_apps() {
                Ok(apps) => {
                    log::info!("indexed {} apps in {:?}", apps.len(), started.elapsed());
                    let _ = tx.send(apps);
                }
                Err(err) => log::error!("listing the Apps folder failed: {err}"),
            }
            let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), WM_APP_INDEX, WPARAM(0), LPARAM(0)) };
        });
        match spawned {
            Ok(_) => index.pending = Some(rx),
            Err(err) => log::error!("apps index thread: {err}"),
        }
    });
}

/// Handles `WM_APP_INDEX`: takes the worker's result.
pub fn receive() {
    with(|index| {
        let Some(rx) = index.pending.take() else {
            return;
        };
        if let Ok(apps) = rx.try_recv() {
            index.apps = apps;
            index.listed_at = Some(Instant::now());
        }
    });
}

fn display_name(item: &IShellItem, kind: SIGDN) -> Option<String> {
    unsafe {
        let name = item.GetDisplayName(kind).ok()?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        text
    }
}

fn list_apps() -> Result<Vec<Candidate>> {
    let folder: IShellItem = unsafe { SHCreateItemFromParsingName(w!("shell:AppsFolder"), None)? };
    let entries: IEnumShellItems = unsafe { folder.BindToHandler(None, &BHID_EnumItems)? };
    let mut apps = Vec::new();
    loop {
        let mut batch = [None];
        let mut fetched = 0;
        if unsafe { entries.Next(&mut batch, Some(&mut fetched)) }.is_err() || fetched == 0 {
            break;
        }
        let Some(item) = batch[0].take() else {
            break;
        };
        let (Some(name), Some(id)) = (display_name(&item, SIGDN_NORMALDISPLAY), display_name(&item, SIGDN_PARENTRELATIVEPARSING))
        else {
            continue;
        };
        let lower_id = id.to_lowercase();
        if name.to_lowercase().contains("uninstall") || SKIPPED_EXTENSIONS.iter().any(|ext| lower_id.ends_with(ext)) {
            continue;
        }
        apps.push(Candidate { name, target: Target::App(format!(r"shell:AppsFolder\{id}")) });
    }
    apps.sort_by_key(|c| c.name.to_lowercase());
    apps.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name) && a.target == b.target);
    Ok(apps)
}

/// Ranked matches for `query` across apps and built-ins.
pub fn find(query: &str, limit: usize) -> Vec<Candidate> {
    with(|index| {
        let all: Vec<Candidate> = index.apps.iter().chain(index.builtins.iter()).cloned().collect();
        search::search(query, &all, &index.usage, limit).into_iter().cloned().collect()
    })
    .unwrap_or_default()
}

/// Icon for an app result, cached after the first load.
pub fn icon(target: &str, size_px: u32) -> Option<IWICBitmapSource> {
    with(|index| {
        let key = format!("{size_px}:{target}");
        if let Some(cached) = index.icons.get(&key) {
            return cached.clone();
        }
        let loaded = index.wic.as_ref().and_then(|wic| icons::load(wic, target, size_px).ok()).map(|i| i.image);
        index.icons.insert(key, loaded.clone());
        loaded
    })
    .flatten()
}

pub fn record_launch(candidate: &Candidate) {
    with(|index| {
        *index.usage.entry(candidate.usage_key()).or_insert(0) += 1;
        save_usage(&index.usage);
    });
}
