//! Microphone and camera "in use" state, from Windows' capability consent
//! store: every app that opens the device writes `LastUsedTimeStart`, and
//! `LastUsedTimeStop` stays 0 while it is still using it. A worker thread
//! sleeps on registry change notifications (no polling), rescans on a change
//! and posts `WM_APP_PRIVACY` only when the answer differs.

use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, RRF_RT_REG_QWORD,
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{HSTRING, PCWSTR, w};

pub const WM_APP_PRIVACY: u32 = WM_APP + 23;

const STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";
const MIC: u8 = 1;
const CAMERA: u8 = 2;

/// Which sensors are in use right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sensors {
    pub mic: bool,
    pub camera: bool,
}

static TARGET: AtomicIsize = AtomicIsize::new(0);
static STATE: AtomicU8 = AtomicU8::new(0);
static STOP: Mutex<Option<isize>> = Mutex::new(None);

pub fn current() -> Sensors {
    let bits = STATE.load(Ordering::Relaxed);
    Sensors { mic: bits & MIC != 0, camera: bits & CAMERA != 0 }
}

/// Starts the watcher; `target` receives `WM_APP_PRIVACY`. Does nothing if running.
pub fn start(target: HWND) {
    let Ok(mut stop) = STOP.lock() else {
        return;
    };
    if stop.is_some() {
        return;
    }
    let Ok(stop_event) = (unsafe { CreateEventW(None, true, false, None) }) else {
        return;
    };
    TARGET.store(target.0 as isize, Ordering::Relaxed);
    let raw = stop_event.0 as isize;
    let spawned = std::thread::Builder::new().name("wdock-privacy".into()).stack_size(256 * 1024).spawn(move || {
        run(HANDLE(raw as *mut _));
    });
    match spawned {
        Ok(_) => *stop = Some(raw),
        Err(err) => log::warn!("could not start the privacy watcher: {err}"),
    }
}

pub fn stop() {
    if let Ok(mut stop) = STOP.lock()
        && let Some(raw) = stop.take()
    {
        let _ = unsafe { SetEvent(HANDLE(raw as *mut _)) };
    }
    STATE.store(0, Ordering::Relaxed);
}

fn open(sub: &str, access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<HKEY> {
    let mut key = HKEY::default();
    let ok = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(sub), None, access, &mut key) }.is_ok();
    ok.then_some(key)
}

fn subkeys(key: HKEY) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0.. {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let rc = unsafe {
            RegEnumKeyExW(key, index, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None)
        };
        if rc.is_err() {
            break;
        }
        names.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    names
}

fn qword(key: HKEY, name: PCWSTR) -> Option<u64> {
    let mut value = 0u64;
    let mut size = 8u32;
    let rc = unsafe {
        RegGetValueW(key, PCWSTR::null(), name, RRF_RT_REG_QWORD, None, Some((&mut value as *mut u64).cast()), Some(&mut size))
    };
    rc.is_ok().then_some(value)
}

/// An app key is "in use" while it has a start time and no stop time.
fn in_use(app: HKEY) -> bool {
    qword(app, w!("LastUsedTimeStart")).is_some_and(|start| start != 0) && qword(app, w!("LastUsedTimeStop")) == Some(0)
}

fn scan(capability: &str) -> bool {
    let Some(root) = open(&format!(r"{STORE}\{capability}"), KEY_READ) else {
        return false;
    };
    let mut used = false;
    let check = |parent: HKEY, skip: &str| {
        subkeys(parent).into_iter().filter(|n| n != skip).any(|name| {
            open_under(parent, &name).is_some_and(|app| {
                let hit = in_use(app);
                unsafe {
                    let _ = RegCloseKey(app);
                }
                hit
            })
        })
    };
    used |= check(root, "NonPackaged");
    if !used && let Some(non_packaged) = open_under(root, "NonPackaged") {
        used = check(non_packaged, "");
        unsafe {
            let _ = RegCloseKey(non_packaged);
        }
    }
    unsafe {
        let _ = RegCloseKey(root);
    }
    used
}

fn open_under(parent: HKEY, name: &str) -> Option<HKEY> {
    let mut key = HKEY::default();
    let ok = unsafe { RegOpenKeyExW(parent, &HSTRING::from(name), None, KEY_READ, &mut key) }.is_ok();
    ok.then_some(key)
}

fn read_all() -> u8 {
    (u8::from(scan("microphone")) * MIC) | (u8::from(scan("webcam")) * CAMERA)
}

fn run(stop: HANDLE) {
    let target = HWND(TARGET.load(Ordering::Relaxed) as *mut _);
    // One event per key, so only the key that fired is re-armed.
    let mut watched: Vec<(HKEY, HANDLE)> = Vec::new();
    for capability in ["microphone", "webcam"] {
        if let Some(key) = open(&format!(r"{STORE}\{capability}"), KEY_NOTIFY)
            && let Ok(event) = unsafe { CreateEventW(None, false, false, None) }
        {
            let _ = unsafe { RegNotifyChangeKeyValue(key, true, REG_NOTIFY_CHANGE_LAST_SET, Some(event), true) };
            watched.push((key, event));
        }
    }
    STATE.store(read_all(), Ordering::Relaxed);
    let _ = unsafe { PostMessageW(Some(target), WM_APP_PRIVACY, WPARAM(0), LPARAM(0)) };
    let mut handles = vec![stop];
    handles.extend(watched.iter().map(|&(_, event)| event));
    loop {
        let rc = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        let fired = rc.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if fired == 0 || fired >= handles.len() {
            break;
        }
        // Writers update start/stop times in quick succession.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let (key, event) = watched[fired - 1];
        let _ = unsafe { RegNotifyChangeKeyValue(key, true, REG_NOTIFY_CHANGE_LAST_SET, Some(event), true) };
        let bits = read_all();
        if STATE.swap(bits, Ordering::Relaxed) != bits {
            let _ = unsafe { PostMessageW(Some(target), WM_APP_PRIVACY, WPARAM(0), LPARAM(0)) };
        }
    }
    for (key, event) in watched {
        unsafe {
            let _ = RegCloseKey(key);
            let _ = windows::Win32::Foundation::CloseHandle(event);
        }
    }
}
