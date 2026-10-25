//! System-wide window events through out-of-context WinEvent hooks: the OS
//! posts them to our thread, so no code is loaded into other processes
//! (important for games with anti-cheat).
//!
//! The callback only queues events and posts one wake-up message; the model
//! applies them in a batch from the main window procedure, which also keeps
//! the callback from ever re-entering shell state.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_UNCLOAKED,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
    EVENT_SYSTEM_MOVESIZEEND, OBJID_WINDOW, PostMessageW, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};

use crate::platform::wininfo;

pub const WM_APP_WINEVENTS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// Event ranges we subscribe to. Location changes are deliberately absent:
/// they fire for every cursor move system-wide.
const RANGES: &[(u32, u32)] = &[
    (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
    (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
    (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
    (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
    (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
    (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
];

thread_local! {
    static TARGET: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static QUEUE: RefCell<Vec<(u32, HWND)>> = const { RefCell::new(Vec::new()) };
    static WAKE_POSTED: Cell<bool> = const { Cell::new(false) };
    /// Location hook on the foreground app's process, and that process.
    static LOCATION: Cell<Option<(HWINEVENTHOOK, u32)>> = const { Cell::new(None) };
}

/// Follows size/position changes of the foreground app only. Location events
/// are far too frequent system-wide, but within one process they are what
/// reveals F11-style full screen and maximizing, which send nothing else.
pub fn watch_process(pid: u32) {
    if LOCATION.get().is_some_and(|(_, p)| p == pid) {
        return;
    }
    unwatch();
    if pid == 0 || pid == unsafe { windows::Win32::System::Threading::GetCurrentProcessId() } {
        return;
    }
    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(callback),
            pid,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if !hook.is_invalid() {
        LOCATION.set(Some((hook, pid)));
    }
}

fn unwatch() {
    if let Some((hook, _)) = LOCATION.take() {
        let _ = unsafe { UnhookWinEvent(hook) };
    }
}

pub struct Hooks(Vec<HWINEVENTHOOK>);

impl Drop for Hooks {
    fn drop(&mut self) {
        unwatch();
        for hook in self.0.drain(..) {
            let _ = unsafe { UnhookWinEvent(hook) };
        }
    }
}

/// Installs the hooks on the calling (UI) thread; `target` receives `WM_APP_WINEVENTS`.
pub fn install(target: HWND) -> Hooks {
    TARGET.set(target);
    let hooks = RANGES
        .iter()
        .filter_map(|&(min, max)| {
            let hook = unsafe {
                SetWinEventHook(min, max, None, Some(callback), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
            };
            if hook.is_invalid() {
                log::error!("SetWinEventHook({min:#x}..{max:#x}) failed");
                None
            } else {
                Some(hook)
            }
        })
        .collect();
    Hooks(hooks)
}

/// Takes all queued `(event, hwnd)` pairs.
pub fn drain() -> Vec<(u32, HWND)> {
    WAKE_POSTED.set(false);
    QUEUE.take()
}

unsafe extern "system" fn callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.is_invalid() || object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
        return;
    }
    // Most create/show/hide events are for child controls; drop those early.
    // Destroyed windows can no longer be inspected, so those always pass.
    if event != EVENT_OBJECT_DESTROY && !wininfo::is_top_level(hwnd) {
        return;
    }
    QUEUE.with_borrow_mut(|queue| queue.push((event, hwnd)));
    if !WAKE_POSTED.replace(true) {
        let _ = unsafe { PostMessageW(Some(TARGET.get()), WM_APP_WINEVENTS, WPARAM(0), LPARAM(0)) };
    }
}
