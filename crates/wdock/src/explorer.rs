//! Explorer's taskbar while wdock runs on top of it.
//!
//! Explorer stays the real shell: it hosts the machinery packaged (UWP) apps,
//! the screenshot overlay, Settings and desktop icons depend on. wdock only
//! hides its taskbars (primary and secondary) and turns on their auto-hide
//! so explorer stops reserving screen space for them.
//!
//! The original auto-hide state is saved to `taskbar.txt` before it is
//! changed, so a crash never loses it: whoever restores next (wdock, the
//! guard, `--restore-taskbar` from uninstall.ps1) puts it back.

use std::fs;
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, IsWindowVisible, SW_HIDE, SW_SHOWNA, ShowWindow,
};
use windows::core::{PCWSTR, w};

fn state_path() -> PathBuf {
    crate::paths::local_dir().join("taskbar.txt")
}

fn not_ours(hwnd: HWND) -> bool {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid != unsafe { GetCurrentProcessId() }
}

fn find_all(class: PCWSTR) -> Vec<HWND> {
    let mut found = Vec::new();
    let mut after = None;
    while let Ok(hwnd) = unsafe { FindWindowExW(None, after, class, None) } {
        if not_ours(hwnd) {
            found.push(hwnd);
        }
        after = Some(hwnd);
    }
    found
}

/// Explorer's primary taskbar, if explorer is running.
pub fn taskbar() -> Option<HWND> {
    find_all(w!("Shell_TrayWnd")).into_iter().next()
}

/// Every explorer taskbar, one per monitor.
fn taskbars() -> Vec<HWND> {
    let mut all = find_all(w!("Shell_TrayWnd"));
    all.extend(find_all(w!("Shell_SecondaryTrayWnd")));
    all
}

fn appbar_data(taskbar: HWND) -> APPBARDATA {
    APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: taskbar, ..Default::default() }
}

fn auto_hide(taskbar: HWND) -> bool {
    let mut data = appbar_data(taskbar);
    let state = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) } as u32;
    state & ABS_AUTOHIDE != 0
}

fn set_auto_hide(taskbar: HWND, on: bool) {
    let mut data = appbar_data(taskbar);
    data.lParam = LPARAM(if on { ABS_AUTOHIDE as isize } else { 0 });
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut data) };
}

/// Hides explorer's taskbars and makes them auto-hide. Safe to call again
/// whenever explorer may have shown them; only visible ones are touched.
///
/// Returns whether auto-hide was switched on just now. Explorer then moves its
/// taskbar to the top of the z-order, above our tray window, so the caller
/// must reclaim the tray or apps re-adding icons would reach explorer's.
pub fn hide_taskbar() -> bool {
    let Some(primary) = taskbar() else {
        return false;
    };
    let mut switched = false;
    let path = state_path();
    if !path.exists() {
        let original = auto_hide(primary);
        let saved = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| {
            fs::write(&path, if original { "autohide=1\n" } else { "autohide=0\n" })
        });
        if let Err(err) = saved {
            log::warn!("could not save the taskbar state: {err}");
        }
        if !original {
            log::info!("turning on auto-hide for explorer's taskbar (restored on exit)");
            set_auto_hide(primary, true);
            switched = true;
        }
    }
    for taskbar in taskbars() {
        if unsafe { IsWindowVisible(taskbar) }.as_bool() {
            unsafe {
                let _ = ShowWindow(taskbar, SW_HIDE);
            }
        }
    }
    switched
}

/// Shows explorer's taskbars again with their original auto-hide setting.
pub fn restore_taskbar() {
    let path = state_path();
    let original = fs::read_to_string(&path).ok().map(|text| text.trim() == "autohide=1");
    if let Some(primary) = taskbar() {
        if let Some(original) = original {
            set_auto_hide(primary, original);
        }
        for taskbar in taskbars() {
            unsafe {
                let _ = ShowWindow(taskbar, SW_SHOWNA);
            }
        }
        log::info!("explorer's taskbar restored");
    }
    let _ = fs::remove_file(&path);
}
