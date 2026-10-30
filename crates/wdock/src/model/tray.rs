//! Notification-area icons registered with our `Shell_TrayWnd`, following
//! explorer's semantics: icons are identified by GUID when given, otherwise by
//! (owner window, uID); NIM_ADD of an existing icon and NIM_MODIFY of an
//! unknown one fail so the app falls back to the other call.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Imaging::IWICBitmapSource;

use crate::model::tray_hidden::HiddenApp;
use crate::platform::tray_proto::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_STATE, NIF_TIP, NIS_HIDDEN, NotifyIconData,
    TrayCommand, TrayMessage,
};

/// Host-side services, injected so the model stays testable.
pub struct TrayEnv {
    /// Converts an app's HICON into a bitmap (and a hash of its pixels) while the app still owns it.
    pub load_icon: fn(isize) -> Option<(IWICBitmapSource, u64)>,
    /// Executable of the owner window's process, lower-cased.
    pub exe_of: fn(isize) -> String,
    pub is_window: fn(isize) -> bool,
}

pub struct TrayIcon {
    pub id: u64,
    pub hwnd: isize,
    pub uid: u32,
    pub guid: Option<[u8; 16]>,
    pub callback: u32,
    pub version: u32,
    pub image: Option<IWICBitmapSource>,
    /// Pixel hash, so re-sent identical icons cost nothing.
    image_hash: u64,
    /// The app's handle `image` was converted from.
    hicon: isize,
    /// Bumped whenever `image` changes, for render caches.
    pub image_rev: u64,
    pub tip: String,
    pub show_tip: bool,
    pub hidden: bool,
    pub exe: String,
    /// Where the icon was last drawn, in screen pixels (for Shell_NotifyIconGetRect).
    pub screen_rect: Option<RECT>,
}

impl TrayIcon {
    pub fn cache_key(&self) -> String {
        format!("tray:{}:{}", self.id, self.image_rev)
    }

    /// Version 4 apps without NIF_SHOWTIP draw their own popup (NIN_POPUPOPEN).
    pub fn wants_standard_tip(&self) -> bool {
        self.version < 4 || self.show_tip
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Balloon {
    pub icon_id: u64,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Return value for Shell_NotifyIcon.
    pub ok: bool,
    pub changed: bool,
    pub balloon: Option<Balloon>,
    pub removed: Option<u64>,
}

pub struct TrayModel {
    icons: Vec<TrayIcon>,
    next_id: u64,
    revision: u64,
    env: TrayEnv,
    /// Apps the user hid from the top bar (see `tray_hidden`).
    hidden: Vec<HiddenApp>,
}

impl TrayModel {
    pub fn new(env: TrayEnv) -> Self {
        Self { icons: Vec::new(), next_id: 1, revision: 0, env, hidden: Vec::new() }
    }

    /// Changes whenever anything visible about the tray changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn icon(&self, id: u64) -> Option<&TrayIcon> {
        self.icons.iter().find(|i| i.id == id)
    }

    fn find(&self, data: &NotifyIconData) -> Option<usize> {
        match data.guid {
            Some(guid) => self.icons.iter().position(|i| i.guid == Some(guid)),
            None => self.icons.iter().position(|i| i.guid.is_none() && i.hwnd == data.hwnd && i.uid == data.uid),
        }
    }

    pub fn find_by(&self, hwnd: isize, uid: u32, guid: Option<[u8; 16]>) -> Option<&TrayIcon> {
        let data = NotifyIconData { hwnd, uid, guid, ..Default::default() };
        self.find(&data).map(|i| &self.icons[i])
    }

    pub fn apply(&mut self, message: TrayMessage) -> Applied {
        let data = message.data;
        let index = self.find(&data);
        let mut applied = Applied::default();
        match (message.command, index) {
            (TrayCommand::Add, None) => {
                let mut icon = TrayIcon {
                    id: self.next_id,
                    hwnd: data.hwnd,
                    uid: data.uid,
                    guid: data.guid,
                    callback: 0,
                    version: 0,
                    image: None,
                    image_hash: 0,
                    hicon: 0,
                    image_rev: 0,
                    tip: String::new(),
                    show_tip: false,
                    hidden: false,
                    exe: (self.env.exe_of)(data.hwnd),
                    screen_rect: None,
                };
                self.next_id += 1;
                applied.balloon = self.update(&mut icon, &data).1;
                self.icons.push(icon);
                applied.ok = true;
                applied.changed = true;
            }
            (TrayCommand::Modify, Some(i)) => {
                let mut icon = self.icons.remove(i);
                (applied.changed, applied.balloon) = self.update(&mut icon, &data);
                self.icons.insert(i, icon);
                applied.ok = true;
            }
            (TrayCommand::Delete, Some(i)) => {
                applied.removed = Some(self.icons.remove(i).id);
                applied.ok = true;
                applied.changed = true;
            }
            (TrayCommand::SetVersion, Some(i)) => {
                self.icons[i].version = data.timeout_or_version;
                applied.ok = data.timeout_or_version <= 4;
            }
            (TrayCommand::SetFocus, Some(_)) => applied.ok = true,
            (TrayCommand::Add, Some(_)) | (_, None) => {}
        }
        if applied.changed {
            self.revision += 1;
        }
        applied
    }

    /// Applies the fields selected by `flags`. Returns whether anything visible
    /// changed (apps such as G HUB re-send the same icon every second) and a
    /// balloon to show, if any.
    fn update(&self, icon: &mut TrayIcon, data: &NotifyIconData) -> (bool, Option<Balloon>) {
        let before = (icon.callback, icon.image_hash, icon.tip.clone(), icon.show_tip, icon.hidden);
        if data.flags & NIF_MESSAGE != 0 {
            icon.callback = data.callback_message;
        }
        // An HICON's pixels never change, so the same handle means the same image
        // and the conversion can be skipped entirely.
        let same_handle = data.hicon != 0 && data.hicon == icon.hicon && icon.image.is_some();
        if data.flags & NIF_ICON != 0 && !same_handle {
            icon.hicon = data.hicon;
            let loaded = if data.hicon == 0 { None } else { (self.env.load_icon)(data.hicon) };
            let hash = loaded.as_ref().map_or(0, |(_, hash)| *hash);
            if hash != icon.image_hash || hash == 0 {
                icon.image = loaded.map(|(image, _)| image);
                icon.image_hash = hash;
                icon.image_rev += 1;
            }
        }
        if data.flags & NIF_TIP != 0 {
            icon.tip = data.tip.clone();
        }
        icon.show_tip = data.flags & NIF_SHOWTIP != 0;
        if data.flags & NIF_STATE != 0 && data.state_mask & NIS_HIDDEN != 0 {
            icon.hidden = data.state & NIS_HIDDEN != 0;
        }
        let changed = before != (icon.callback, icon.image_hash, icon.tip.clone(), icon.show_tip, icon.hidden)
            || (data.flags & NIF_ICON != 0 && !same_handle && icon.image_hash == 0);
        let balloon = (data.flags & NIF_INFO != 0 && !data.info.is_empty()).then(|| Balloon {
            icon_id: icon.id,
            title: data.info_title.clone(),
            text: data.info.clone(),
        });
        (changed, balloon)
    }

    /// Drops icons whose owner window died without NIM_DELETE (crashed apps).
    pub fn prune(&mut self) -> bool {
        let before = self.icons.len();
        let is_window = self.env.is_window;
        self.icons.retain(|i| is_window(i.hwnd));
        let changed = self.icons.len() != before;
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Visible icons split into the bar and the overflow popup. Icons the user
    /// hid and icons matching `overflow` (case-insensitive executable-name
    /// fragments) always go to the popup.
    pub fn split(&self, max_visible: usize, overflow: &[String]) -> (Vec<&TrayIcon>, Vec<&TrayIcon>) {
        let (forced, mut bar): (Vec<&TrayIcon>, Vec<&TrayIcon>) = self
            .icons
            .iter()
            .filter(|i| !i.hidden)
            .partition(|i| {
                self.hidden_by_user(i) || overflow.iter().any(|p| !p.is_empty() && i.exe.contains(&p.to_lowercase()))
            });
        let mut extra = if bar.len() > max_visible { bar.split_off(max_visible) } else { Vec::new() };
        extra.extend(forced);
        (bar, extra)
    }

    pub fn hidden_by_user(&self, icon: &TrayIcon) -> bool {
        self.hidden.iter().any(|h| h.matches(&icon.exe, icon.guid))
    }

    /// Replaces the list of user-hidden apps (loaded at start).
    pub fn set_hidden(&mut self, apps: Vec<HiddenApp>) {
        self.hidden = apps;
        self.revision += 1;
    }

    /// Moves an icon between the bar and the overflow popup. Returns the new
    /// list to persist, or `None` for an unknown icon.
    pub fn toggle_hidden(&mut self, id: u64) -> Option<Vec<HiddenApp>> {
        let icon = self.icon(id)?;
        let (exe, guid) = (icon.exe.clone(), icon.guid);
        if self.hidden_by_user(icon) {
            self.hidden.retain(|h| !h.matches(&exe, guid));
        } else {
            self.hidden.push(HiddenApp::for_icon(&exe, guid));
        }
        self.revision += 1;
        Some(self.hidden.clone())
    }

    pub fn set_screen_rect(&mut self, id: u64, rect: Option<RECT>) {
        if let Some(icon) = self.icons.iter_mut().find(|i| i.id == id) {
            icon.screen_rect = rect;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::tray_proto::NIF_GUID;

    fn env() -> TrayEnv {
        TrayEnv { load_icon: |_| None, exe_of: |hwnd| format!("app{hwnd}.exe"), is_window: |hwnd| hwnd != 99 }
    }

    fn msg(command: TrayCommand, hwnd: isize, uid: u32, flags: u32) -> TrayMessage {
        TrayMessage { command, data: NotifyIconData { hwnd, uid, flags, ..Default::default() } }
    }

    #[test]
    fn add_modify_delete_follow_explorer_rules() {
        let mut tray = TrayModel::new(env());
        assert!(!tray.apply(msg(TrayCommand::Modify, 1, 1, 0)).ok, "modify of unknown icon fails");
        assert!(tray.apply(msg(TrayCommand::Add, 1, 1, NIF_TIP)).ok);
        assert!(!tray.apply(msg(TrayCommand::Add, 1, 1, 0)).ok, "duplicate add fails");
        assert!(tray.apply(msg(TrayCommand::Add, 1, 2, 0)).ok, "same window, other uid is a new icon");

        let mut modify = msg(TrayCommand::Modify, 1, 1, NIF_TIP | NIF_MESSAGE);
        modify.data.tip = "Hello".into();
        modify.data.callback_message = 0x8010;
        assert!(tray.apply(modify).ok);
        let icon = tray.find_by(1, 1, None).unwrap();
        assert_eq!((icon.tip.as_str(), icon.callback), ("Hello", 0x8010));

        let id = icon.id;
        assert_eq!(tray.apply(msg(TrayCommand::Delete, 1, 1, 0)).removed, Some(id));
        assert!(tray.find_by(1, 1, None).is_none());
    }

    #[test]
    fn identical_modify_is_not_a_change() {
        let mut tray = TrayModel::new(env());
        let mut add = msg(TrayCommand::Add, 1, 1, NIF_TIP);
        add.data.tip = "same".into();
        tray.apply(add.clone());
        let revision = tray.revision();
        let mut modify = add;
        modify.command = TrayCommand::Modify;
        let applied = tray.apply(modify);
        assert!(applied.ok && !applied.changed);
        assert_eq!(tray.revision(), revision);
    }

    #[test]
    fn guid_identity_ignores_hwnd() {
        let mut tray = TrayModel::new(env());
        let mut add = msg(TrayCommand::Add, 1, 1, NIF_GUID);
        add.data.guid = Some([7; 16]);
        tray.apply(add);
        let mut modify = msg(TrayCommand::Modify, 2, 9, NIF_GUID | NIF_TIP);
        modify.data.guid = Some([7; 16]);
        modify.data.tip = "moved".into();
        assert!(tray.apply(modify).ok);
        assert_eq!(tray.find_by(0, 0, Some([7; 16])).unwrap().tip, "moved");
    }

    #[test]
    fn state_hidden_and_version() {
        let mut tray = TrayModel::new(env());
        tray.apply(msg(TrayCommand::Add, 1, 1, 0));
        let mut hide = msg(TrayCommand::Modify, 1, 1, NIF_STATE);
        hide.data.state = NIS_HIDDEN;
        hide.data.state_mask = NIS_HIDDEN;
        tray.apply(hide);
        assert!(tray.split(10, &[]).0.is_empty());

        let mut version = msg(TrayCommand::SetVersion, 1, 1, 0);
        version.data.timeout_or_version = 4;
        assert!(tray.apply(version).ok);
        let icon = tray.find_by(1, 1, None).unwrap();
        assert!(!icon.wants_standard_tip(), "v4 without NIF_SHOWTIP draws its own popup");
    }

    #[test]
    fn balloon_only_with_text() {
        let mut tray = TrayModel::new(env());
        let mut add = msg(TrayCommand::Add, 1, 1, NIF_INFO);
        add.data.info = "Download finished".into();
        add.data.info_title = "Steam".into();
        let balloon = tray.apply(add).balloon.unwrap();
        assert_eq!((balloon.title.as_str(), balloon.text.as_str()), ("Steam", "Download finished"));
        assert!(tray.apply(msg(TrayCommand::Modify, 1, 1, NIF_INFO)).balloon.is_none());
    }

    #[test]
    fn split_respects_limit_and_overflow_list() {
        let mut tray = TrayModel::new(env());
        for hwnd in 1..=4 {
            tray.apply(msg(TrayCommand::Add, hwnd, 1, 0));
        }
        let (bar, extra) = tray.split(2, &["APP2".into()]);
        let exes = |v: &[&TrayIcon]| v.iter().map(|i| i.exe.clone()).collect::<Vec<_>>();
        assert_eq!(exes(&bar), ["app1.exe", "app3.exe"]);
        assert_eq!(exes(&extra), ["app4.exe", "app2.exe"]);
    }

    #[test]
    fn prune_removes_dead_owners() {
        let mut tray = TrayModel::new(env());
        tray.apply(msg(TrayCommand::Add, 99, 1, 0));
        tray.apply(msg(TrayCommand::Add, 5, 1, 0));
        assert!(tray.prune());
        assert!(tray.find_by(99, 1, None).is_none());
        assert!(!tray.prune());
    }
}
