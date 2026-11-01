//! Read Explorer's notification-area preferences without changing them.
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_NOTIFY, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW, RegNotifyChangeKeyValue, REG_NOTIFY_CHANGE_NAME, REG_NOTIFY_CHANGE_LAST_SET,
    RRF_RT_REG_DWORD, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};
use crate::model::tray_hidden::exe_name;

pub const WM_TRAY_PREFERENCES: u32 = WM_APP + 32;

pub struct Watcher {
    stop: HANDLE,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub fn start(target: HWND) -> Option<Self> {
        let mut key = HKEY::default();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Control Panel\NotifyIconSettings"), None, KEY_NOTIFY, &mut key) }.is_err() { return None; }
        let event = match unsafe { CreateEventW(None, false, false, None) } {
            Ok(event) => event,
            Err(_) => { unsafe { let _ = RegCloseKey(key); } return None; }
        };
        let stop = match unsafe { CreateEventW(None, true, false, None) } {
            Ok(stop) => stop,
            Err(_) => { unsafe { let _ = RegCloseKey(key); let _ = CloseHandle(event); } return None; }
        };
        let (raw_key, raw_event, raw_stop, raw_target) = (key.0 as isize, event.0 as isize, stop.0 as isize, target.0 as isize);
        let thread = std::thread::spawn(move || {
            let key = HKEY(raw_key as *mut _);
            let event = HANDLE(raw_event as *mut _);
            let handles = [HANDLE(raw_stop as *mut _), event];
            loop {
                if unsafe { RegNotifyChangeKeyValue(key, true, REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET, Some(event), true) }.is_err() { break; }
                let result = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                if result.0 != WAIT_OBJECT_0.0 + 1 { break; }
                let _ = unsafe { PostMessageW(Some(HWND(raw_target as *mut _)), WM_TRAY_PREFERENCES, WPARAM(0), LPARAM(0)) };
            }
            unsafe { let _ = RegCloseKey(key); let _ = CloseHandle(event); }
        });
        Some(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = unsafe { SetEvent(self.stop) };
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
        unsafe { let _ = CloseHandle(self.stop); }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preference {
    pub exe: String,
    pub guid: Option<[u8; 16]>,
    pub uid: Option<u32>,
    pub promoted: bool,
}

impl Preference {
    pub fn matches(&self, exe: &str, guid: Option<[u8; 16]>, uid: u32) -> bool {
        match (self.guid, guid) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.uid == Some(uid) && self.exe.eq_ignore_ascii_case(exe_name(exe)),
            _ => false,
        }
    }
}

fn string(key: HKEY, name: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 4096];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let ok = unsafe {
        RegGetValueW(key, PCWSTR::null(), name, RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    }.is_ok();
    if !ok { return None; }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn dword(key: HKEY, name: PCWSTR) -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of_val(&value) as u32;
    unsafe {
        RegGetValueW(key, PCWSTR::null(), name, RRF_RT_REG_DWORD, None,
            Some((&mut value as *mut u32).cast()), Some(&mut size))
    }.is_ok().then_some(value)
}

/// NOTIFYICONDATA stores the first three GUID fields in little-endian.
fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let s = s.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<_> = s.split('-').collect();
    if parts.len() != 5 || parts.iter().map(|p| p.len()).collect::<Vec<_>>() != [8, 4, 4, 4, 12] {
        return None;
    }
    let mut bytes = [0u8; 16];
    bytes[..4].copy_from_slice(&u32::from_str_radix(parts[0], 16).ok()?.to_le_bytes());
    bytes[4..6].copy_from_slice(&u16::from_str_radix(parts[1], 16).ok()?.to_le_bytes());
    bytes[6..8].copy_from_slice(&u16::from_str_radix(parts[2], 16).ok()?.to_le_bytes());
    let tail = format!("{}{}", parts[3], parts[4]);
    for i in 0..8 { bytes[8 + i] = u8::from_str_radix(&tail[i*2..i*2+2], 16).ok()?; }
    Some(bytes)
}

pub fn load() -> Vec<Preference> {
    let mut root = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Control Panel\NotifyIconSettings"), None, KEY_READ, &mut root) }.is_err() {
        return Vec::new();
    }
    let mut entries = Vec::new();
    for index in 0.. {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        if unsafe { RegEnumKeyExW(root, index, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) }.is_err() { break; }
        let mut key = HKEY::default();
        if unsafe { RegOpenKeyExW(root, &HSTRING::from(String::from_utf16_lossy(&name[..len as usize])), None, KEY_READ, &mut key) }.is_err() { continue; }
        let path = string(key, w!("ExecutablePath")).unwrap_or_default();
        let guid = string(key, w!("IconGuid")).and_then(|s| parse_guid(&s));
        let uid = dword(key, w!("UID"));
        let promoted = dword(key, w!("IsPromoted")).unwrap_or(0) != 0;
        unsafe { let _ = RegCloseKey(key); }
        if !path.is_empty() && (guid.is_some() || uid.is_some()) {
            entries.push(Preference { exe: exe_name(&path).to_lowercase(), guid, uid, promoted });
        }
    }
    unsafe { let _ = RegCloseKey(root); }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_windows_guid_layout() {
        assert_eq!(parse_guid("{00112233-4455-6677-8899-AABBCCDDEEFF}"),
            Some([0x33,0x22,0x11,0x00,0x55,0x44,0x77,0x66,0x88,0x99,0xaa,0xbb,0xcc,0xdd,0xee,0xff]));
        assert!(parse_guid("bad").is_none());
    }

    #[test]
    #[ignore = "reads this user's Explorer tray settings"]
    fn reads_live_preferences() {
        let prefs = load();
        assert!(!prefs.is_empty(), "no Explorer tray preferences found");
        eprintln!("Read {} notification-area preferences", prefs.len());
    }
}
