//! The set of app windows, kept current from WinEvent notifications, and the
//! dock entries derived from it (pinned apps first, then other running apps).
//!
//! Each monitor has its own set of workspaces. Windows of inactive workspaces
//! are hidden with `SW_HIDE` (no injection, works for every app); their handles
//! are written to `hidden.txt` so a crash never leaves them stranded: the next
//! start (or the guard) shows them again.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICBitmapSource, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_DESTROY, EVENT_SYSTEM_FOREGROUND, EnumWindows, GetForegroundWindow,
};
use windows::core::BOOL;

use crate::model::apps::{AppInfo, AppKey, Identity, Pinned};
use crate::platform::{launch, wininfo};

pub struct TrackedWindow {
    pub hwnd: HWND,
    pub identity: Identity,
    pub monitor: HMONITOR,
    pub last_active: u64,
    pub flashing: bool,
    /// 0-based workspace on `monitor`.
    pub workspace: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntrySource {
    Pinned(usize),
    Running(AppKey),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub source: EntrySource,
    /// In creation order, which keeps click-to-cycle stable.
    pub windows: Vec<HWND>,
    /// Most recently activated window.
    pub recent: Option<HWND>,
    /// Owns the foreground window.
    pub active: bool,
    pub flashing: bool,
}

impl Entry {
    pub fn is_pinned(&self) -> bool {
        matches!(self.source, EntrySource::Pinned(_))
    }
}

pub struct WindowModel {
    pinned: Vec<Pinned>,
    windows: Vec<TrackedWindow>,
    apps: HashMap<AppKey, AppInfo>,
    foreground: Option<HWND>,
    clock: u64,
    wic: Option<IWICImagingFactory>,
    icon_px: u32,
    workspaces: u8,
    /// Active workspace per monitor handle; absent means the first.
    active: HashMap<isize, u8>,
    /// Windows we hid for workspaces, with the tick they were hidden at.
    hidden: HashMap<isize, u64>,
    /// Bumped whenever the pinned list changes, since entries refer to it by index.
    pins_revision: u64,
}

/// Visibility events for a window we just hid can still report it visible
/// while `ShowWindowAsync` is in flight; within this window they are ignored.
const HIDE_GRACE_MS: u64 = 1500;

fn mon(monitor: HMONITOR) -> isize {
    monitor.0 as isize
}

fn now_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

fn hidden_path() -> PathBuf {
    crate::paths::local_dir().join("hidden.txt")
}

/// Shows windows a previous run hid for workspaces and did not restore.
pub fn restore_orphans() {
    let path = hidden_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let mut restored = 0;
    for line in text.lines() {
        if let Ok(value) = line.trim().parse::<isize>() {
            let hwnd = HWND(value as *mut _);
            if wininfo::exists(hwnd) && !wininfo::is_visible(hwnd) {
                wininfo::show(hwnd);
                restored += 1;
            }
        }
    }
    let _ = fs::remove_file(&path);
    if restored > 0 {
        log::warn!("restored {restored} window(s) hidden by a previous run");
    }
}

impl WindowModel {
    pub fn new(pinned_targets: &[String], icon_px: u32, workspaces: u8) -> Self {
        let wic: Option<IWICImagingFactory> =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .inspect_err(|err| log::error!("WIC unavailable, icons disabled: {err}"))
                .ok();
        let pinned = match &wic {
            Some(wic) => pinned_targets.iter().filter_map(|t| Pinned::resolve(wic, t, icon_px)).collect(),
            None => Vec::new(),
        };
        let mut model = Self::empty(pinned);
        model.wic = wic;
        model.icon_px = icon_px;
        model.workspaces = workspaces.clamp(1, 9);
        model
    }

    fn empty(pinned: Vec<Pinned>) -> Self {
        Self {
            pinned,
            windows: Vec::new(),
            apps: HashMap::new(),
            foreground: None,
            clock: 0,
            wic: None,
            icon_px: 0,
            workspaces: 1,
            active: HashMap::new(),
            hidden: HashMap::new(),
            pins_revision: 0,
        }
    }

    /// Picks up windows that existed before the hooks were installed.
    pub fn scan(&mut self) {
        let mut found: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), windows::Win32::Foundation::LPARAM((&raw mut found) as isize));
        }
        // EnumWindows is top-to-bottom in z-order; insert bottom-up so the
        // topmost window ends up most recent.
        for hwnd in found.into_iter().rev() {
            if self.refresh(hwnd) {
                self.touch(hwnd);
            }
        }
        let foreground = unsafe { GetForegroundWindow() };
        self.set_foreground(foreground);
        log::info!("tracking {} window(s) in {} running app(s)", self.windows.len(), self.apps.len());
    }

    /// Applies queued WinEvents. Returns whether anything visible changed.
    pub fn apply(&mut self, events: Vec<(u32, HWND)>) -> bool {
        let mut changed = false;
        for (event, hwnd) in events {
            changed |= match event {
                EVENT_OBJECT_DESTROY => self.remove(hwnd),
                EVENT_SYSTEM_FOREGROUND => self.refresh(hwnd) | self.set_foreground(hwnd),
                _ => self.refresh(hwnd),
            };
        }
        changed
    }

    pub fn set_flashing(&mut self, hwnd: HWND, flashing: bool) -> bool {
        let Some(window) = self.windows.iter_mut().find(|w| w.hwnd == hwnd) else {
            return false;
        };
        std::mem::replace(&mut window.flashing, flashing) != flashing
    }

    fn index(&self, hwnd: HWND) -> Option<usize> {
        self.windows.iter().position(|w| w.hwnd == hwnd)
    }

    /// Re-evaluates one window: starts or stops tracking it, or notes a monitor change.
    fn refresh(&mut self, hwnd: HWND) -> bool {
        let mut changed = false;
        if let Some(&hidden_at) = self.hidden.get(&(hwnd.0 as isize)) {
            if !wininfo::exists(hwnd) {
                return self.remove(hwnd);
            }
            if !wininfo::is_visible(hwnd) || now_ms() < hidden_at + HIDE_GRACE_MS {
                return false;
            }
            // Shown by someone else, usually the app itself: it joins the
            // workspace now visible on its monitor.
            self.hidden.remove(&(hwnd.0 as isize));
            self.save_hidden();
            if let Some(i) = self.index(hwnd) {
                let monitor = wininfo::monitor(hwnd);
                self.windows[i].monitor = monitor;
                self.windows[i].workspace = self.active_workspace(monitor);
            }
            changed = true;
        }
        let wanted = wininfo::is_app_window(hwnd);
        match (self.index(hwnd), wanted) {
            (Some(i), true) => {
                let monitor = wininfo::monitor(hwnd);
                if std::mem::replace(&mut self.windows[i].monitor, monitor) == monitor {
                    return changed;
                }
                // Dragged to another monitor: it lands on that monitor's current workspace.
                self.windows[i].workspace = self.active_workspace(monitor);
                true
            }
            (Some(_), false) => self.remove(hwnd),
            (None, true) => {
                let identity = Identity::of_window(hwnd);
                log::debug!("track {:?} \"{}\" {:?}", hwnd.0, wininfo::title(hwnd), identity);
                let monitor = wininfo::monitor(hwnd);
                self.insert(TrackedWindow {
                    hwnd,
                    identity,
                    monitor,
                    last_active: 0,
                    flashing: false,
                    workspace: self.active_workspace(monitor),
                });
                true
            }
            (None, false) => changed,
        }
    }

    fn insert(&mut self, window: TrackedWindow) {
        let key = window.identity.key();
        let pinned = self.pinned.iter().any(|p| p.matches(&window.identity));
        if !pinned && !self.apps.contains_key(&key) {
            self.clock += 1;
            let info = AppInfo::resolve(self.wic.as_ref(), &window.identity, self.icon_px, self.clock);
            self.apps.insert(key, info);
        }
        self.windows.push(window);
    }

    fn remove(&mut self, hwnd: HWND) -> bool {
        let Some(i) = self.index(hwnd) else {
            return false;
        };
        let window = self.windows.remove(i);
        if self.hidden.remove(&(hwnd.0 as isize)).is_some() {
            self.save_hidden();
        }
        let key = window.identity.key();
        if !self.windows.iter().any(|w| w.identity.key() == key) {
            // Forget it so it reappears at the end next time, like macOS.
            self.apps.remove(&key);
        }
        if self.foreground == Some(hwnd) {
            self.foreground = None;
        }
        true
    }

    fn touch(&mut self, hwnd: HWND) {
        self.clock += 1;
        let clock = self.clock;
        if let Some(window) = self.windows.iter_mut().find(|w| w.hwnd == hwnd) {
            window.last_active = clock;
            window.flashing = false;
        }
    }

    /// Dialogs and other owned windows count as their main window's app.
    fn set_foreground(&mut self, hwnd: HWND) -> bool {
        let tracked = if self.index(hwnd).is_some() {
            Some(hwnd)
        } else {
            let owner = wininfo::root_owner(hwnd);
            self.index(owner).map(|_| owner)
        };
        if let Some(hwnd) = tracked {
            self.touch(hwnd);
        }
        std::mem::replace(&mut self.foreground, tracked) != tracked
    }

    /// Dock entries, optionally limited to windows on one monitor. Pinned apps
    /// are always present; other apps follow in the order they first appeared.
    pub fn entries(&self, monitor: Option<HMONITOR>) -> Vec<Entry> {
        let empty = |source| Entry { source, windows: Vec::new(), recent: None, active: false, flashing: false };
        let mut pinned: Vec<Entry> = (0..self.pinned.len()).map(|i| empty(EntrySource::Pinned(i))).collect();
        let mut running: Vec<Entry> = Vec::new();

        let mut recency: HashMap<isize, u64> = HashMap::new();
        for window in &self.windows {
            if monitor.is_some_and(|m| m != window.monitor) {
                continue;
            }
            recency.insert(window.hwnd.0 as isize, window.last_active);
            let entry = match self.pinned.iter().position(|p| p.matches(&window.identity)) {
                Some(i) => &mut pinned[i],
                None => {
                    let key = window.identity.key();
                    let source = EntrySource::Running(key);
                    match running.iter().position(|e| e.source == source) {
                        Some(i) => &mut running[i],
                        None => {
                            running.push(empty(source));
                            running.last_mut().expect("just pushed")
                        }
                    }
                }
            };
            entry.windows.push(window.hwnd);
            entry.flashing |= window.flashing;
            entry.active |= self.foreground == Some(window.hwnd);
        }

        running.sort_by_key(|e| match &e.source {
            EntrySource::Running(key) => self.apps.get(key).map_or(u64::MAX, |a| a.first_seen),
            EntrySource::Pinned(_) => 0,
        });
        pinned.extend(running);
        for entry in &mut pinned {
            entry.recent = entry.windows.iter().copied().max_by_key(|h| recency.get(&(h.0 as isize)).copied().unwrap_or(0));
        }
        pinned
    }

    pub fn entry_name(&self, entry: &Entry) -> &str {
        match &entry.source {
            EntrySource::Pinned(i) => &self.pinned[*i].name,
            EntrySource::Running(key) => self.apps.get(key).map_or("", |a| a.name.as_str()),
        }
    }

    /// Stable cache key plus the icon to draw.
    pub fn entry_icon(&self, entry: &Entry) -> Option<(String, &IWICBitmapSource)> {
        match &entry.source {
            EntrySource::Pinned(i) => self.pinned[*i].icon.as_ref().map(|icon| (format!("pin:{}", self.pinned[*i].target), icon)),
            EntrySource::Running(key) => {
                self.apps.get(key)?.icon.as_ref().map(|icon| (format!("app:{key:?}"), icon))
            }
        }
    }

    pub fn entry_launch(&self, entry: &Entry) -> Option<&str> {
        match &entry.source {
            EntrySource::Pinned(i) => Some(&self.pinned[*i].target),
            EntrySource::Running(key) => self.apps.get(key).map(|a| a.launch.as_str()),
        }
    }

    /// Name of the app owning the foreground window, for the top bar.
    pub fn foreground_name(&self) -> Option<&str> {
        let window = self.windows.iter().find(|w| Some(w.hwnd) == self.foreground)?;
        match self.pinned.iter().find(|p| p.matches(&window.identity)) {
            Some(pinned) => Some(&pinned.name),
            None => self.apps.get(&window.identity.key()).map(|a| a.name.as_str()),
        }
    }

    /// Dock click / Win+number: launch when nothing is open, minimize the only
    /// active window, cycle when several are open, otherwise bring it forward.
    pub fn activate_entry(&mut self, entry: &Entry) {
        let Some(&first) = entry.windows.first() else {
            if let Some(target) = self.entry_launch(entry) {
                launch::open(target);
            }
            return;
        };
        match self.foreground.filter(|fg| entry.active && entry.windows.contains(fg)) {
            Some(fg) if entry.windows.len() == 1 => wininfo::minimize(fg),
            Some(fg) => {
                let i = entry.windows.iter().position(|&h| h == fg).unwrap_or(0);
                self.focus(entry.windows[(i + 1) % entry.windows.len()]);
            }
            None => {
                // Prefer a window on a workspace that is already showing.
                let visible = entry
                    .windows
                    .iter()
                    .copied()
                    .filter(|h| !self.is_hidden(*h))
                    .max_by_key(|h| self.last_active(*h));
                self.focus(visible.or(entry.recent).unwrap_or(first));
            }
        }
    }

    fn last_active(&self, hwnd: HWND) -> u64 {
        self.windows.iter().find(|w| w.hwnd == hwnd).map_or(0, |w| w.last_active)
    }

    /// Brings a window forward, first switching to its workspace if hidden.
    pub fn focus(&mut self, hwnd: HWND) {
        let Some(window) = self.windows.iter().find(|w| w.hwnd == hwnd) else {
            wininfo::activate(hwnd);
            return;
        };
        if self.is_hidden(hwnd) {
            let (monitor, workspace) = (window.monitor, window.workspace);
            self.switch_workspace(monitor, workspace);
            // The show is asynchronous; activate once the window is visible.
            crate::app::focus_later(hwnd);
            crate::app::notify_model();
        } else {
            wininfo::activate(hwnd);
        }
    }

    // ---- workspaces ----

    pub fn workspace_count(&self) -> u8 {
        self.workspaces
    }

    pub fn active_workspace(&self, monitor: HMONITOR) -> u8 {
        self.active.get(&mon(monitor)).copied().unwrap_or(0).min(self.workspaces - 1)
    }

    pub fn is_hidden(&self, hwnd: HWND) -> bool {
        self.hidden.contains_key(&(hwnd.0 as isize))
    }

    pub fn foreground(&self) -> Option<HWND> {
        self.foreground
    }

    /// Monitor and workspace of a tracked window.
    pub fn placement(&self, hwnd: HWND) -> Option<(HMONITOR, u8)> {
        self.windows.iter().find(|w| w.hwnd == hwnd).map(|w| (w.monitor, w.workspace))
    }

    /// Window count per workspace on `monitor`.
    pub fn occupancy(&self, monitor: HMONITOR) -> Vec<usize> {
        let mut counts = vec![0; self.workspaces as usize];
        for w in self.windows.iter().filter(|w| w.monitor == monitor) {
            if let Some(count) = counts.get_mut(w.workspace as usize) {
                *count += 1;
            }
        }
        counts
    }

    /// Windows of one workspace, most recently active first.
    pub fn workspace_windows(&self, monitor: HMONITOR, workspace: u8) -> Vec<HWND> {
        let mut windows: Vec<&TrackedWindow> =
            self.windows.iter().filter(|w| w.monitor == monitor && w.workspace == workspace).collect();
        windows.sort_by_key(|w| std::cmp::Reverse(w.last_active));
        windows.into_iter().map(|w| w.hwnd).collect()
    }

    /// Every window on a visible workspace, most recently active first (Alt+Tab).
    pub fn switcher_windows(&self) -> Vec<HWND> {
        let mut windows: Vec<&TrackedWindow> = self.windows.iter().filter(|w| !self.is_hidden(w.hwnd)).collect();
        windows.sort_by_key(|w| std::cmp::Reverse(w.last_active));
        windows.into_iter().map(|w| w.hwnd).collect()
    }

    /// Shows `workspace` on `monitor`, hiding the one shown before. Full-screen
    /// windows (games) are never hidden; they follow to the new workspace.
    /// Returns the window that should receive focus.
    pub fn switch_workspace(&mut self, monitor: HMONITOR, workspace: u8) -> Option<HWND> {
        let workspace = workspace.min(self.workspaces - 1);
        let current = self.active_workspace(monitor);
        if current == workspace {
            return None;
        }
        log::debug!("monitor {:?}: workspace {} -> {}", monitor.0, current + 1, workspace + 1);
        self.active.insert(mon(monitor), workspace);
        let now = now_ms();
        let mut focus: Option<(u64, HWND)> = None;
        for w in self.windows.iter_mut().filter(|w| w.monitor == monitor) {
            let id = w.hwnd.0 as isize;
            if w.workspace == current && !self.hidden.contains_key(&id) {
                if wininfo::is_fullscreen(w.hwnd) {
                    w.workspace = workspace;
                } else {
                    self.hidden.insert(id, now);
                    wininfo::hide(w.hwnd);
                }
            } else if w.workspace == workspace && self.hidden.remove(&id).is_some() {
                wininfo::show(w.hwnd);
            }
            let newer = focus.is_none_or(|(t, _)| w.last_active > t);
            if w.workspace == workspace && !wininfo::is_minimized(w.hwnd) && newer {
                focus = Some((w.last_active, w.hwnd));
            }
        }
        self.save_hidden();
        focus.map(|(_, hwnd)| hwnd)
    }

    /// Moves a window to another workspace of its monitor, hiding it if that
    /// workspace is not the one showing.
    pub fn move_window(&mut self, hwnd: HWND, workspace: u8) -> bool {
        let workspace = workspace.min(self.workspaces - 1);
        let Some(i) = self.index(hwnd) else {
            return false;
        };
        if self.windows[i].workspace == workspace {
            return false;
        }
        self.windows[i].workspace = workspace;
        let id = hwnd.0 as isize;
        if workspace == self.active_workspace(self.windows[i].monitor) {
            if self.hidden.remove(&id).is_some() {
                wininfo::show(hwnd);
            }
        } else if let std::collections::hash_map::Entry::Vacant(slot) = self.hidden.entry(id) {
            slot.insert(now_ms());
            wininfo::hide(hwnd);
        }
        self.save_hidden();
        true
    }

    /// Shows every window hidden for workspaces (shutdown).
    pub fn show_all_hidden(&mut self) {
        for &id in self.hidden.keys() {
            wininfo::show(HWND(id as *mut _));
        }
        self.hidden.clear();
        self.active.clear();
        let _ = fs::remove_file(hidden_path());
    }

    fn save_hidden(&self) {
        if cfg!(test) {
            return;
        }
        let path = hidden_path();
        if self.hidden.is_empty() {
            let _ = fs::remove_file(&path);
            return;
        }
        let text: String = self.hidden.keys().map(|id| format!("{id}\n")).collect();
        let written = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path, text));
        if let Err(err) = written {
            log::warn!("could not record hidden windows: {err}");
        }
    }

    // ---- per-window lookups (switcher, overview, previews) ----

    pub fn window_app_name(&self, hwnd: HWND) -> &str {
        let Some(window) = self.windows.iter().find(|w| w.hwnd == hwnd) else {
            return "";
        };
        match self.pinned.iter().find(|p| p.matches(&window.identity)) {
            Some(pinned) => &pinned.name,
            None => self.apps.get(&window.identity.key()).map_or("", |a| a.name.as_str()),
        }
    }

    pub fn window_icon(&self, hwnd: HWND) -> Option<(String, &IWICBitmapSource)> {
        let window = self.windows.iter().find(|w| w.hwnd == hwnd)?;
        match self.pinned.iter().position(|p| p.matches(&window.identity)) {
            Some(i) => self.pinned[i].icon.as_ref().map(|icon| (format!("pin:{}", self.pinned[i].target), icon)),
            None => {
                let key = window.identity.key();
                self.apps.get(&key)?.icon.as_ref().map(|icon| (format!("app:{key:?}"), icon))
            }
        }
    }

    // ---- pinning ----

    /// Pins a running app at `at` (clamped). Returns whether the dock changed.
    pub fn pin(&mut self, entry: &Entry, at: usize) -> bool {
        let EntrySource::Running(key) = &entry.source else {
            return false;
        };
        let (Some(wic), Some(app)) = (self.wic.as_ref(), self.apps.get(key)) else {
            return false;
        };
        let Some(mut pinned) = Pinned::resolve(wic, &app.launch, self.icon_px) else {
            return false;
        };
        // The running windows define the identity even when the launch target
        // resolves to something else (a launcher stub, say).
        if let Some(window) = entry.windows.first().and_then(|h| self.windows.iter().find(|w| w.hwnd == *h))
            && !pinned.matches(&window.identity) {
                pinned.aumid = window.identity.aumid.clone().or(pinned.aumid);
                pinned.exe = Some(window.identity.exe.clone());
            }
        let at = at.min(self.pinned.len());
        self.pinned.insert(at, pinned);
        self.save_pins();
        true
    }

    pub fn unpin(&mut self, index: usize) -> bool {
        if index >= self.pinned.len() {
            return false;
        }
        self.pinned.remove(index);
        // Its open windows become an ordinary running entry.
        for i in 0..self.windows.len() {
            let identity = self.windows[i].identity.clone();
            let key = identity.key();
            if !self.pinned.iter().any(|p| p.matches(&identity)) && !self.apps.contains_key(&key) {
                self.clock += 1;
                let info = AppInfo::resolve(self.wic.as_ref(), &identity, self.icon_px, self.clock);
                self.apps.insert(key, info);
            }
        }
        self.save_pins();
        true
    }

    pub fn move_pinned(&mut self, from: usize, to: usize) -> bool {
        if from >= self.pinned.len() {
            return false;
        }
        let to = to.min(self.pinned.len() - 1);
        if from == to {
            return false;
        }
        let item = self.pinned.remove(from);
        self.pinned.insert(to, item);
        self.save_pins();
        true
    }

    pub fn pins_revision(&self) -> u64 {
        self.pins_revision
    }

    fn save_pins(&mut self) {
        self.pins_revision += 1;
        let targets: Vec<String> = self.pinned.iter().map(|p| p.target.clone()).collect();
        crate::model::pins::save(&targets);
    }
}

unsafe extern "system" fn collect(hwnd: HWND, data: windows::Win32::Foundation::LPARAM) -> BOOL {
    unsafe { (*(data.0 as *mut Vec<HWND>)).push(hwnd) };
    true.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hwnd(n: usize) -> HWND {
        HWND(n as *mut _)
    }

    fn identity(aumid: Option<&str>, exe: &str) -> Identity {
        Identity { aumid: aumid.map(str::to_owned), exe: exe.to_owned() }
    }

    fn model_with(pinned: Vec<Pinned>, windows: &[(usize, Identity, u64)]) -> WindowModel {
        let mut model = WindowModel::empty(pinned);
        for (n, id, last_active) in windows {
            model.clock += 1;
            model.apps.entry(id.key()).or_insert(AppInfo {
                name: id.exe.clone(),
                icon: None,
                launch: id.exe.clone(),
                first_seen: model.clock,
            });
            model.windows.push(TrackedWindow {
                hwnd: hwnd(*n),
                identity: id.clone(),
                monitor: HMONITOR::default(),
                last_active: *last_active,
                flashing: false,
                workspace: 0,
            });
        }
        model
    }

    fn pin(exe: &str) -> Pinned {
        Pinned { target: exe.into(), name: exe.into(), aumid: None, exe: Some(exe.into()), icon: None }
    }

    #[test]
    fn pinned_first_then_running_in_first_seen_order() {
        let model = model_with(
            vec![pin("a.exe"), pin("b.exe")],
            &[
                (1, identity(None, "c.exe"), 1),
                (2, identity(None, "a.exe"), 2),
                (3, identity(Some("pkg!app"), "host.exe"), 3),
                (4, identity(None, "c.exe"), 4),
            ],
        );
        let entries = model.entries(None);
        let sources: Vec<_> = entries.iter().map(|e| e.source.clone()).collect();
        assert_eq!(
            sources,
            vec![
                EntrySource::Pinned(0),
                EntrySource::Pinned(1),
                EntrySource::Running(AppKey::Exe("c.exe".into())),
                EntrySource::Running(AppKey::Aumid("pkg!app".into())),
            ]
        );
        assert_eq!(entries[0].windows, vec![hwnd(2)]);
        assert!(entries[1].windows.is_empty());
        assert_eq!(entries[2].windows, vec![hwnd(1), hwnd(4)]);
        assert_eq!(entries[2].recent, Some(hwnd(4)));
    }

    #[test]
    fn foreground_marks_entry_active() {
        let mut model = model_with(vec![], &[(1, identity(None, "x.exe"), 1), (2, identity(None, "y.exe"), 2)]);
        model.foreground = Some(hwnd(2));
        let entries = model.entries(None);
        assert!(!entries[0].active);
        assert!(entries[1].active);
    }

    fn workspace_model() -> WindowModel {
        let mut model = model_with(
            vec![],
            &[(1, identity(None, "a.exe"), 1), (2, identity(None, "b.exe"), 2), (3, identity(None, "c.exe"), 3)],
        );
        model.workspaces = 3;
        model
    }

    #[test]
    fn switching_hides_the_old_workspace_and_shows_the_new() {
        let mut model = workspace_model();
        let m = HMONITOR::default();
        model.windows[2].workspace = 1;
        assert_eq!(model.occupancy(m), vec![2, 1, 0]);

        let focus = model.switch_workspace(m, 1);
        assert_eq!(focus, Some(hwnd(3)));
        assert_eq!(model.active_workspace(m), 1);
        assert!(model.is_hidden(hwnd(1)) && model.is_hidden(hwnd(2)) && !model.is_hidden(hwnd(3)));
        assert_eq!(model.switcher_windows(), vec![hwnd(3)]);

        model.switch_workspace(m, 0);
        assert!(!model.is_hidden(hwnd(1)) && model.is_hidden(hwnd(3)));
        assert_eq!(model.switcher_windows(), vec![hwnd(2), hwnd(1)], "most recent first");
        assert_eq!(model.switch_workspace(m, 0), None, "already showing");
    }

    #[test]
    fn moving_a_window_hides_it_until_its_workspace_shows() {
        let mut model = workspace_model();
        let m = HMONITOR::default();
        assert!(model.move_window(hwnd(1), 2));
        assert!(model.is_hidden(hwnd(1)));
        assert!(!model.move_window(hwnd(1), 2), "no-op");
        assert_eq!(model.workspace_windows(m, 2), vec![hwnd(1)]);
        model.switch_workspace(m, 2);
        assert!(!model.is_hidden(hwnd(1)));
        assert!(model.move_window(hwnd(2), 2), "hidden window joins the shown workspace");
        assert!(!model.is_hidden(hwnd(2)));
        model.show_all_hidden();
        assert!(model.hidden.is_empty());
    }

    #[test]
    fn monitor_filter_hides_other_monitors() {
        let mut model = model_with(vec![pin("a.exe")], &[(1, identity(None, "a.exe"), 1), (2, identity(None, "z.exe"), 2)]);
        model.windows[1].monitor = HMONITOR(7 as *mut _);
        let entries = model.entries(Some(HMONITOR::default()));
        assert_eq!(entries.len(), 1, "pinned stays, z.exe is on another monitor");
        assert_eq!(entries[0].windows, vec![hwnd(1)]);
    }
}
