//! Read-only questions about other processes' windows, plus focus/minimize.
//! Nothing here injects code or sends blocking messages to other processes.

use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegGetValueW, RRF_RT_REG_DWORD};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GA_ROOT, GA_ROOTOWNER, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW,
    GetShellWindow, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, SW_MINIMIZE, SW_RESTORE, SetForegroundWindow, ShowWindowAsync,
    SwitchToThisWindow, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::{BSTR, HSTRING, PCWSTR, PWSTR, w};

/// Shell and system surfaces that look like app windows but never belong in a dock.
const IGNORED_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
];

/// Roughly the Alt+Tab rule: visible, top-level, unowned (or explicitly an app
/// window), not a tool window, not cloaked, has a title, and not ours.
pub fn is_app_window(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetAncestor(hwnd, GA_ROOT) != hwnd {
            return false;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_APPWINDOW.0 == 0 {
            if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
                return false;
            }
            if GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
                return false;
            }
        }
        if is_cloaked(hwnd) || GetWindowTextLengthW(hwnd) == 0 || process_id(hwnd) == GetCurrentProcessId() {
            return false;
        }
    }
    !IGNORED_CLASSES.contains(&class_name(hwnd).as_str())
}

pub fn is_top_level(hwnd: HWND) -> bool {
    unsafe { GetAncestor(hwnd, GA_ROOT) == hwnd }
}

/// The owner at the top of the chain, e.g. the main window of a dialog.
pub fn root_owner(hwnd: HWND) -> HWND {
    unsafe { GetAncestor(hwnd, GA_ROOTOWNER) }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    let ok = unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&raw mut cloaked).cast::<c_void>(), size_of::<u32>() as u32)
    };
    ok.is_ok() && cloaked != 0
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

pub fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

pub fn monitor(hwnd: HWND) -> HMONITOR {
    unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
}

pub fn process_id(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The process that actually renders a window. UWP apps live in a frame owned by
/// ApplicationFrameHost; the app itself owns the CoreWindow child.
pub fn content_process_id(hwnd: HWND) -> u32 {
    if class_name(hwnd) == "ApplicationFrameWindow"
        && let Ok(core) = unsafe { FindWindowExW(Some(hwnd), None, w!("Windows.UI.Core.CoreWindow"), None) } {
            return process_id(core);
        }
    process_id(hwnd)
}

/// Mirrors Windows' taskbar End task toggle; a missing setting is disabled.
pub fn end_task_enabled() -> bool {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER,
        w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced\TaskbarDeveloperSettings"),
        w!("TaskbarEndTask"), RRF_RT_REG_DWORD, None, Some((&raw mut value).cast()), Some(&mut size)).is_ok() && value != 0 }
}

fn explorer_image(path: &str) -> bool {
    path.rsplit(['\\', '/']).next().is_some_and(|name| name.eq_ignore_ascii_case("explorer.exe"))
}

/// Close the most recently used Explorer window, and terminate other captured processes.
/// Never terminate an unidentified process: it could be the Windows shell.
pub fn end_tasks(targets: &[(HWND, u32)]) {
    let mut handled = Vec::new();
    let mut explorer_closed = false;
    let shell_pid = process_id(unsafe { GetShellWindow() });
    for &(hwnd, pid) in targets {
        if pid == 0 || pid == unsafe { GetCurrentProcessId() } || content_process_id(hwnd) != pid { continue; }
        if pid == shell_pid || matches!(class_name(hwnd).as_str(), "CabinetWClass" | "ExploreWClass") {
            if !explorer_closed { close(hwnd); explorer_closed = true; }
            continue;
        }
        if handled.contains(&pid) { continue; }
        match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid) } {
            Ok(handle) => {
                let mut path = [0u16; 1024];
                let mut len = path.len() as u32;
                let image = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(path.as_mut_ptr()), &mut len) };
                if image.is_err() {
                    log::warn!("could not identify task {pid}; skipping termination");
                } else if explorer_image(&String::from_utf16_lossy(&path[..len as usize])) {
                    // A single Explorer process can own multiple windows and the taskbar.
                    if !explorer_closed { close(hwnd); explorer_closed = true; }
                } else if content_process_id(hwnd) == pid {
                    handled.push(pid);
                    if let Err(error) = unsafe { TerminateProcess(handle, 1) } { log::warn!("could not end task {pid}: {error}"); }
                }
                let _ = unsafe { CloseHandle(handle) };
            }
            Err(error) => log::warn!("could not open task {pid} for termination: {error}"),
        }
    }
}

#[cfg(test)]
mod end_task_tests {
    use super::explorer_image;

    #[test]
    fn explorer_is_identified_case_insensitively_for_any_path() {
        assert!(explorer_image(r"C:\Windows\explorer.exe"));
        assert!(explorer_image(r"c:\WINDOWS\EXPLORER.EXE"));
        assert!(explorer_image("C:/Windows/explorer.exe"));
        assert!(!explorer_image(r"C:\Apps\my-explorer.exe"));
        assert!(!explorer_image(r"C:\Apps\explorer.exe.backup"));
    }
}

pub struct ProcessInfo {
    pub exe: String,
    /// Package-assigned AppUserModelID for packaged (Store/MSIX) apps.
    pub package_aumid: Option<String>,
}

pub fn process_info(pid: u32) -> Option<ProcessInfo> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let exe = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
            .ok()
            .map(|()| String::from_utf16_lossy(&buf[..len as usize]));

        let mut aumid = [0u16; 512];
        let mut aumid_len = aumid.len() as u32;
        let package_aumid = GetApplicationUserModelId(process, &mut aumid_len, Some(PWSTR(aumid.as_mut_ptr())))
            .is_ok()
            .then(|| String::from_utf16_lossy(&aumid[..aumid_len.saturating_sub(1) as usize]));

        let _ = CloseHandle(process);
        Some(ProcessInfo { exe: exe?, package_aumid })
    }
}

/// AppUserModelID set explicitly on the window (browsers' PWAs, multi-app hosts).
pub fn explicit_aumid(hwnd: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let value = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let text = BSTR::try_from(&value).ok()?.to_string();
        (!text.is_empty()).then_some(text)
    }
}

fn version_string(text: &[u16]) -> Option<String> {
    let end = text.iter().position(|&ch| ch == 0).unwrap_or(text.len());
    let text = String::from_utf16_lossy(&text[..end]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// The version resource's FileDescription, e.g. "Google Chrome" for chrome.exe.
pub fn file_description(exe: &str) -> Option<String> {
    let path = HSTRING::from(exe);
    unsafe {
        let size = GetFileVersionInfoSizeW(&path, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&path, None, size, data.as_mut_ptr().cast()).ok()?;

        let mut ptr = std::ptr::null_mut::<c_void>();
        let mut len = 0u32;
        let translation = VerQueryValueW(data.as_ptr().cast(), w!(r"\VarFileInfo\Translation"), &mut ptr, &mut len);
        let lang_cp = if translation.as_bool() && len >= 4 {
            let pair = std::slice::from_raw_parts(ptr as *const u16, 2);
            format!("{:04x}{:04x}", pair[0], pair[1])
        } else {
            "040904b0".to_owned()
        };
        let query = HSTRING::from(format!(r"\StringFileInfo\{lang_cp}\FileDescription"));
        if !VerQueryValueW(data.as_ptr().cast(), PCWSTR(query.as_ptr()), &mut ptr, &mut len).as_bool() || len == 0 {
            return None;
        }
        // Some version resources report a length that includes the next field.
        // Keep the read inside the version buffer and stop at the first NUL.
        let start = data.as_ptr() as usize;
        let end = start.checked_add(data.len())?;
        let value = ptr as usize;
        if value < start || value >= end || !value.is_multiple_of(std::mem::align_of::<u16>()) { return None; }
        let available = (end - value) / size_of::<u16>();
        let text = std::slice::from_raw_parts(ptr as *const u16, (len as usize).min(available));
        version_string(text)
    }
}

#[cfg(test)]
mod version_tests {
    use super::version_string;

    #[test]
    fn description_stops_at_first_null_even_when_resource_length_is_too_long() {
        let value: Vec<u16> = "Spotify\0\u{0038}\u{0001}\u{0002}FileV".encode_utf16().collect();
        assert_eq!(version_string(&value).as_deref(), Some("Spotify"));
        assert_eq!(version_string(&[0, 65]), None);
        assert_eq!(version_string(&"  Example  ".encode_utf16().collect::<Vec<_>>()).as_deref(), Some("Example"));
    }

    #[test]
    #[ignore = "reads the installed Spotify executable"]
    fn installed_spotify_description_has_no_version_resource_suffix() {
        let path = std::path::PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA")).join(r"Spotify\Spotify.exe");
        assert_eq!(super::file_description(&path.to_string_lossy()).as_deref(), Some("Spotify"));
    }
}

/// Restores if minimized and brings the window to the front. Our process just
/// handled user input (click or hotkey), so the foreground lock allows this.
pub fn activate(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            SwitchToThisWindow(hwnd, true);
        }
    }
}

pub fn is_visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

pub fn exists(hwnd: HWND) -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd)).as_bool() }
}

pub fn is_minimized(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd).as_bool() }
}

pub fn rect(hwnd: HWND) -> windows::Win32::Foundation::RECT {
    let mut rect = Default::default();
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) };
    rect
}

/// Covers its whole monitor without a caption: games and full-screen video.
pub fn is_fullscreen(hwnd: HWND) -> bool {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    use windows::Win32::UI::WindowsAndMessaging::{GWL_STYLE, WS_CAPTION};
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if !exists(hwnd) || !IsWindowVisible(hwnd).as_bool() || is_cloaked(hwnd)
            || style & WS_CAPTION.0 == WS_CAPTION.0 || is_minimized(hwnd) {
            return false;
        }
        if !GetMonitorInfoW(monitor(hwnd), &mut info).as_bool() {
            return false;
        }
    }
    let r = rect(hwnd);
    let m = info.rcMonitor;
    let covers = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
    // Maximized windows of apps that draw their own frame (Chromium,
    // Electron) have no caption and can cover the monitor while overhanging
    // it by their invisible borders; they are not full screen. A maximized
    // window that fits the monitor exactly is: Chromium keeps the maximized
    // state for full-screen video, and borderless games maximize themselves.
    let zoomed = unsafe { windows::Win32::UI::WindowsAndMessaging::IsZoomed(hwnd).as_bool() };
    covers && (!zoomed || r == m)
}

/// The foreground app covers its monitor: a game or full-screen video, not
/// the desktop and not one of our own windows (the overview is full-screen too).
pub fn is_fullscreen_app(hwnd: HWND) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if process_id(root) == unsafe { GetCurrentProcessId() } {
        return false;
    }
    !matches!(class_name(root).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") && is_fullscreen(root)
}

/// Hides without activating anything; async so a hung app cannot block us.
pub fn hide(hwnd: HWND) {
    let _ = unsafe { ShowWindowAsync(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_HIDE) };
}

/// Shows in its previous state (minimized stays minimized) without activating.
pub fn show(hwnd: HWND) {
    let _ = unsafe { ShowWindowAsync(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNA) };
}

/// Asks the window to close, as its title bar's close button would.
pub fn close(hwnd: HWND) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SC_CLOSE, WM_SYSCOMMAND};
    let _ = unsafe { PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0)) };
}

pub fn minimize(hwnd: HWND) {
    // Async so a hung target cannot freeze the shell.
    let _ = unsafe { ShowWindowAsync(hwnd, SW_MINIMIZE) };
}
