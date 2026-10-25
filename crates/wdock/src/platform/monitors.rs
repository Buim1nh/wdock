use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::BOOL;

#[derive(Debug, Clone, Copy)]
pub struct Monitor {
    pub handle: HMONITOR,
    /// Full monitor rectangle in physical pixels (virtual-screen coordinates).
    pub rect: RECT,
    pub primary: bool,
    /// DPI / 96.
    pub scale: f32,
}

pub fn enumerate() -> Vec<Monitor> {
    let mut monitors = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM((&raw mut monitors) as isize));
    }
    monitors
}

unsafe extern "system" fn collect(handle: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let monitors = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(handle, &mut info) }.as_bool() {
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        monitors.push(Monitor {
            handle,
            rect: info.rcMonitor,
            primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            scale: dpi_x as f32 / 96.0,
        });
    }
    true.into()
}

/// The monitor under the mouse pointer (the primary one if that fails).
pub fn under_cursor() -> HMONITOR {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
        MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY)
    }
}

/// Details of one monitor handle, as `enumerate` reports them.
pub fn describe(handle: HMONITOR) -> Option<Monitor> {
    enumerate().into_iter().find(|m| m.handle == handle)
}
