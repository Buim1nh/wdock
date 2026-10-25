//! Bars registered with explorer's AppBar manager, so maximized windows stop
//! at them: explorer shrinks the work area on that monitor edge for us and
//! tells us when an app goes full screen.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

/// Callback message explorer sends a registered bar.
pub const WM_APP_APPBAR: u32 = WM_APP + 13;
pub const ABN_POSCHANGED: usize = 1;
pub const ABN_FULLSCREENAPP: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

thread_local! {
    /// Registered bars and the strip each reserves.
    static BARS: RefCell<Vec<(HWND, Edge, RECT)>> = const { RefCell::new(Vec::new()) };
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
    BARS.with_borrow_mut(|bars| bars.push((hwnd, edge, rect)));
    set_pos(hwnd, edge, rect);
}

/// Explorer re-arranged appbars (another bar, a display change): claim again.
pub fn position_changed(hwnd: HWND) {
    if let Some((_, edge, rect)) = BARS.with_borrow(|bars| bars.iter().copied().find(|b| b.0 == hwnd)) {
        set_pos(hwnd, edge, rect);
    }
}

/// Releases `hwnd`'s reservation, if it has one.
pub fn unregister(hwnd: HWND) {
    let found = BARS.with_borrow_mut(|bars| {
        let index = bars.iter().position(|b| b.0 == hwnd)?;
        Some(bars.remove(index))
    });
    if let Some((_, edge, rect)) = found {
        let mut d = data(hwnd, edge, rect);
        unsafe { SHAppBarMessage(ABM_REMOVE, &mut d) };
    }
}
