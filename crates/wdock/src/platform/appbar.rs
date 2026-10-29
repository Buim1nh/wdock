//! Bars registered with explorer's AppBar manager, so maximized windows stop
//! at them: explorer shrinks the work area on that monitor edge for us and
//! tells us when an app goes full screen.
//!
//! Explorer's AppBar manager can get stuck refusing every reservation
//! (ABM_QUERYPOS hands back an empty rectangle, seen after explorer's taskbar
//! was hidden and restored many times in a row); the top bar then covers
//! maximized windows until explorer restarts. `verify` notices that,
//! registers again, and as a last resort sets the monitor's work area itself
//! (put back when the bar unregisters).

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::{SPI_SETWORKAREA, SPIF_SENDCHANGE, SystemParametersInfoW, WM_APP};

/// Callback message explorer sends a registered bar.
pub const WM_APP_APPBAR: u32 = WM_APP + 13;
pub const ABN_POSCHANGED: usize = 1;
pub const ABN_FULLSCREENAPP: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

/// Re-registrations tried before setting the work area ourselves.
const RETRIES: u8 = 2;

struct Bar {
    hwnd: HWND,
    edge: Edge,
    rect: RECT,
    /// Times explorer failed to honour the reservation since it last did.
    failures: u8,
    /// The work area we replaced, while our own one is in force.
    replaced: Option<RECT>,
}

thread_local! {
    /// Registered bars and the strip each reserves.
    static BARS: RefCell<Vec<Bar>> = const { RefCell::new(Vec::new()) };
}

fn data(hwnd: HWND, edge: Edge, rect: RECT) -> APPBARDATA {
    APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: WM_APP_APPBAR,
        uEdge: match edge {
            Edge::Top => ABE_TOP,
            Edge::Bottom => ABE_BOTTOM,
        },
        rc: rect,
        lParam: LPARAM(0),
    }
}

fn set_pos(hwnd: HWND, edge: Edge, rect: RECT) {
    let mut d = data(hwnd, edge, rect);
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &mut d);
        if d.rc.right <= d.rc.left {
            // Explorer refused the position; `verify` deals with it.
            log::warn!("explorer refused the AppBar position for {:?}", hwnd.0);
            d.rc = rect;
        }
        // Keep our thickness even if explorer moved the strip (another bar).
        let thickness = rect.bottom - rect.top;
        match edge {
            Edge::Top => d.rc.bottom = d.rc.top + thickness,
            Edge::Bottom => d.rc.top = d.rc.bottom - thickness,
        }
        SHAppBarMessage(ABM_SETPOS, &mut d);
    }
}

/// Reserves `rect` (screen pixels, along `edge` of its monitor) for `hwnd`.
pub fn register(hwnd: HWND, edge: Edge, rect: RECT) {
    let mut d = data(hwnd, edge, rect);
    if unsafe { SHAppBarMessage(ABM_NEW, &mut d) } == 0 {
        log::warn!("AppBar registration refused (is explorer running?)");
        return;
    }
    BARS.with_borrow_mut(|bars| bars.push(Bar { hwnd, edge, rect, failures: 0, replaced: None }));
    set_pos(hwnd, edge, rect);
}

/// Explorer re-arranged appbars (another bar, a display change): claim again.
pub fn position_changed(hwnd: HWND) {
    if let Some((edge, rect)) = BARS.with_borrow(|bars| bars.iter().find(|b| b.hwnd == hwnd).map(|b| (b.edge, b.rect))) {
        set_pos(hwnd, edge, rect);
    }
}

/// Releases `hwnd`'s reservation, if it has one.
pub fn unregister(hwnd: HWND) {
    let found = BARS.with_borrow_mut(|bars| {
        let index = bars.iter().position(|b| b.hwnd == hwnd)?;
        Some(bars.remove(index))
    });
    if let Some(bar) = found {
        let mut d = data(hwnd, bar.edge, bar.rect);
        unsafe { SHAppBarMessage(ABM_REMOVE, &mut d) };
        if let Some(original) = bar.replaced {
            set_work_area(original);
        }
    }
}

fn work_area(hwnd: HWND) -> Option<RECT> {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() }
        .then_some(info.rcWork)
}

fn set_work_area(mut area: RECT) {
    let ptr = Some(&mut area as *mut RECT as *mut core::ffi::c_void);
    if let Err(err) = unsafe { SystemParametersInfoW(SPI_SETWORKAREA, 0, ptr, SPIF_SENDCHANGE) } {
        log::warn!("could not set the work area: {err}");
    }
}

/// Whether the work area currently leaves `rect` free on `edge`.
fn honoured(edge: Edge, rect: RECT, work: RECT) -> bool {
    match edge {
        Edge::Top => work.top >= rect.bottom,
        Edge::Bottom => work.bottom <= rect.top,
    }
}

/// The work area with `rect`'s strip taken off `edge`.
fn without_strip(edge: Edge, rect: RECT, work: RECT) -> RECT {
    match edge {
        Edge::Top => RECT { top: work.top.max(rect.bottom), ..work },
        Edge::Bottom => RECT { bottom: work.bottom.min(rect.top), ..work },
    }
}

/// Checks every reservation against the real work area; call it while no
/// app is full screen, since explorer drops reservations for those on
/// purpose. Returns true when it acted and the bars should be checked again.
pub fn verify() -> bool {
    let bars: Vec<(HWND, Edge, RECT, u8, bool)> =
        BARS.with_borrow(|bars| bars.iter().map(|b| (b.hwnd, b.edge, b.rect, b.failures, b.replaced.is_some())).collect());
    let mut recheck = false;
    for (hwnd, edge, rect, failures, replaced) in bars {
        let Some(work) = work_area(hwnd) else {
            continue;
        };
        if honoured(edge, rect, work) {
            update(hwnd, |bar| bar.failures = 0);
            continue;
        }
        recheck = true;
        if replaced {
            // Our own work area was overwritten (a display change, or explorer
            // laying out again): take the strip once more.
            set_work_area(without_strip(edge, rect, work));
        } else if failures < RETRIES {
            log::warn!("explorer is not reserving a bar's strip (work area {work:?}); registering again");
            let mut d = data(hwnd, edge, rect);
            unsafe {
                SHAppBarMessage(ABM_REMOVE, &mut d);
                SHAppBarMessage(ABM_NEW, &mut d);
            }
            set_pos(hwnd, edge, rect);
            update(hwnd, |bar| bar.failures = failures + 1);
        } else {
            log::warn!("explorer keeps ignoring a bar's strip; setting the work area to keep maximized windows clear");
            set_work_area(without_strip(edge, rect, work));
            update(hwnd, |bar| bar.replaced = Some(work));
        }
    }
    recheck
}

fn update(hwnd: HWND, change: impl FnOnce(&mut Bar)) {
    BARS.with_borrow_mut(|bars| {
        if let Some(bar) = bars.iter_mut().find(|b| b.hwnd == hwnd) {
            change(bar);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };

    #[test]
    fn top_strip_is_honoured_only_below_the_bar() {
        let bar = RECT { left: 0, top: 0, right: 1920, bottom: 30 };
        assert!(!honoured(Edge::Top, bar, WORK));
        assert!(honoured(Edge::Top, bar, RECT { top: 30, ..WORK }));
        assert_eq!(without_strip(Edge::Top, bar, WORK), RECT { top: 30, ..WORK });
    }

    #[test]
    fn bottom_strip_keeps_a_smaller_work_area() {
        let bar = RECT { left: 0, top: 1020, right: 1920, bottom: 1080 };
        assert!(!honoured(Edge::Bottom, bar, WORK));
        assert!(honoured(Edge::Bottom, bar, RECT { bottom: 1000, ..WORK }));
        assert_eq!(without_strip(Edge::Bottom, bar, WORK), RECT { bottom: 1020, ..WORK });
    }
}
