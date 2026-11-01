//! Windows virtual desktops. The underlying shell interfaces are undocumented.
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RegGetValueW, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::core::w;

pub const WM_DESKTOP_CHANGED: u32 = WM_APP + 31;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub current: u8,
    pub count: u8,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub fn state() -> Option<State> {
    STATE.with_borrow(Clone::clone)
}

pub fn refresh() {
    let result = (|| {
        let count = winvd::get_desktop_count().ok()?.min(255) as u8;
        let current = winvd::get_current_desktop().ok()?.get_index().ok()?;
        if count == 0 || current >= u32::from(count) { return None; }
        Some(State { current: current as u8, count })
    })();
    STATE.set(result);
}

// winvd 0.0.49 targets the shell interface introduced in Windows 11
// 24H2 build 26100.2605. Do not call a potentially incompatible COM vtable.
fn supported_windows() -> bool {
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let mut build = [0u16; 32];
    let mut size = std::mem::size_of_val(&build) as u32;
    let build_ok = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("CurrentBuildNumber"), RRF_RT_REG_SZ, None,
            Some(build.as_mut_ptr().cast()), Some(&mut size))
    }.is_ok();
    let mut ubr = 0u32;
    let mut ubr_size = std::mem::size_of_val(&ubr) as u32;
    let ubr_ok = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("UBR"), RRF_RT_REG_DWORD, None,
            Some((&mut ubr as *mut u32).cast()), Some(&mut ubr_size))
    }.is_ok();
    let end = build.iter().position(|&c| c == 0).unwrap_or(build.len());
    let number = String::from_utf16_lossy(&build[..end]).parse::<u32>().unwrap_or(0);
    build_ok && ubr_ok && supports_build(number, ubr)
}

fn supports_build(build: u32, revision: u32) -> bool {
    (build == 26100 && revision >= 2605) || build == 26200
}

pub fn start(target: HWND) {
    if !supported_windows() {
        log::warn!("Windows version not supported for virtual desktop integration");
        return;
    }
    refresh();
    if state().is_none() {
        log::warn!("Windows virtual desktops unavailable");
        return;
    }
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("wdock-desktops".into()).spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel::<winvd::DesktopEvent>();
        let Ok(_listener) = winvd::listen_desktop_events(tx) else {
            log::warn!("Could not listen for virtual desktop changes");
            return;
        };
        for _ in rx {
            let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), WM_DESKTOP_CHANGED, WPARAM(0), LPARAM(0)) };
        }
    });
}

pub fn switch(index: u8) {
    if state().is_some_and(|state| index < state.count)
        && let Err(err) = winvd::switch_desktop(u32::from(index)) {
            log::warn!("Could not switch Windows desktop: {err:?}");
        }
}

pub fn pin_window(hwnd: HWND) {
    if state().is_some() {
        let handle = windows058::Win32::Foundation::HWND(hwnd.0);
        if let Err(err) = winvd::pin_window(handle) {
            log::warn!("Could not show wdock window on every desktop: {err:?}");
        }
    }
}

pub fn move_window(hwnd: HWND, index: u8) -> bool {
    if !state().is_some_and(|state| index < state.count) {
        return false;
    }
    let handle = windows058::Win32::Foundation::HWND(hwnd.0);
    match winvd::move_window_to_desktop(u32::from(index), &handle) {
        Ok(()) => true,
        Err(err) => {
            log::warn!("Could not move window to Windows desktop: {err:?}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_unverified_shell_versions() {
        assert!(!super::supports_build(26100, 2604));
        assert!(super::supports_build(26100, 2605));
        assert!(super::supports_build(26200, 1));
        assert!(!super::supports_build(22631, 9999));
        assert!(!super::supports_build(26300, 0));
    }

    #[test]
    #[ignore = "requires a supported interactive Windows desktop"]
    fn reads_live_desktop() {
        assert!(super::supported_windows());
        super::refresh();
        let state = super::state().expect("desktop state available");
        assert!(state.count > 0 && state.current < state.count);
        eprintln!("Windows desktops: {state:?}");
    }
}
