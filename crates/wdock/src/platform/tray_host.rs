//! Our `Shell_TrayWnd`: the window `Shell_NotifyIcon` talks to.
//!
//! On creation we broadcast "TaskbarCreated" so running apps re-add their
//! icons to us; on exit we broadcast again so they re-register with whichever
//! shell comes next. Messages arrive as synchronous WM_COPYDATA, so the tray
//! model lives in its own thread-local (never borrowed across a message pump)
//! and changes are forwarded to the shell with a posted message.
//!
//! Explorer keeps running underneath with its own (hidden) taskbar. Anything
//! sent to "the" Shell_TrayWnd that is not a tray-icon message — AppBar
//! registrations, auto-hide queries, explorer's own internal messages — is
//! forwarded to explorer's taskbar, which still provides those services.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapDitherTypeNone, WICBitmapPaletteTypeMedianCut,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowExW,
    FindWindowW, HICON, KillTimer, SetTimer, WM_TIMER,
    HWND_BROADCAST, HWND_TOPMOST, IsWindow, PostMessageW, RegisterClassW, RegisterWindowMessageW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendNotifyMessageW, SetWindowPos, WM_APP,
    WM_CONTEXTMENU, WM_COPYDATA, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER, WNDCLASSW, InSendMessage, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Interface, Result, w};

use crate::model::tray::{Applied, Balloon, TrayEnv, TrayModel};
use crate::model::tray_hidden;
use crate::platform::tray_proto::{self, COPYDATA_APPBAR, COPYDATA_ICON_RECT, COPYDATA_TRAY};
use crate::platform::wininfo;

/// Posted to the main window after the tray changed; read `take_events`.
pub const WM_APP_TRAY: u32 = WM_APP + 3;

const TIMER_KEEP_ON_TOP: usize = 1;
const KEEP_ON_TOP_MS: u32 = 2000;
const FORWARD_TIMEOUT_MS: u32 = 1000;

const NIN_SELECT: u32 = WM_USER;
const NIN_BALLOONSHOW: u32 = WM_USER + 2;
const NIN_BALLOONHIDE: u32 = WM_USER + 3;
const NIN_BALLOONTIMEOUT: u32 = WM_USER + 4;
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const NIN_POPUPOPEN: u32 = WM_USER + 6;
const NIN_POPUPCLOSE: u32 = WM_USER + 7;

pub enum TrayEvent {
    Balloon(Balloon),
    Removed(u64),
}

thread_local! {
    static TRAY: RefCell<Option<TrayModel>> = const { RefCell::new(None) };
    static EVENTS: RefCell<Vec<TrayEvent>> = const { RefCell::new(Vec::new()) };
    static NOTIFY: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static TRAY_HWND: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static WIC: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

/// Runs `f` on the tray model. Returns `None` if it is unavailable or busy.
pub fn with_tray<R>(f: impl FnOnce(&mut TrayModel) -> R) -> Option<R> {
    TRAY.with(|tray| tray.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn take_events() -> Vec<TrayEvent> {
    EVENTS.take()
}

pub struct TrayHost {
    hwnd: HWND,
}

impl TrayHost {
    /// Creates the tray window; `notify` receives `WM_APP_TRAY`.
    pub fn create(notify: HWND) -> Result<Self> {
        NOTIFY.set(notify);
        WIC.set(
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .inspect_err(|err| log::error!("WIC unavailable for tray icons: {err}"))
                .ok(),
        );
        let mut model = TrayModel::new(TrayEnv { load_icon, exe_of, is_window });
        model.set_hidden(tray_hidden::load());
        TRAY.set(Some(model));

        let instance = unsafe { GetModuleHandleW(None)? };
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("Shell_TrayWnd"),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                w!("Shell_TrayWnd"),
                None,
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?
        };
        TRAY_HWND.set(hwnd);
        keep_on_top(hwnd);
        broadcast_taskbar_created();
        log::info!("tray host ready");
        Ok(Self { hwnd })
    }
}

impl Drop for TrayHost {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.hwnd) };
        TRAY.set(None);
        broadcast_taskbar_created();
    }
}

/// FindWindow walks the z-order, so while another Shell_TrayWnd exists (explorer during
/// development) we keep ours above it. With no rival the check timer stops.
fn keep_on_top(hwnd: HWND) {
    unsafe {
        // Only reorder when another taskbar got in front: a no-op SetWindowPos
        // still sends z-order messages every tick.
        if FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default() != hwnd {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        let first = FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default();
        let rival = first != hwnd || FindWindowExW(None, Some(hwnd), w!("Shell_TrayWnd"), None).is_ok();
        if rival {
            SetTimer(Some(hwnd), TIMER_KEEP_ON_TOP, KEEP_ON_TOP_MS, None);
        } else {
            let _ = KillTimer(Some(hwnd), TIMER_KEEP_ON_TOP);
        }
    }
}

/// Explorer restarted and its new taskbar sits above ours: move back on top
/// and ask apps to re-add their icons, which now reach us again.
pub fn reclaim() {
    let ours = TRAY_HWND.get();
    if !ours.is_invalid() {
        keep_on_top(ours);
    }
    broadcast_taskbar_created();
}

fn broadcast_taskbar_created() {
    unsafe {
        let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        let _ = SendNotifyMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0));
    }
}

fn load_icon(hicon: isize) -> Option<(IWICBitmapSource, u64)> {
    WIC.with_borrow(|wic| {
        let wic = wic.as_ref()?;
        unsafe {
            let bitmap = wic.CreateBitmapFromHICON(HICON(hicon as *mut _)).ok()?;
            let converter = wic.CreateFormatConverter().ok()?;
            converter
                .Initialize(&bitmap, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut)
                .ok()?;
            let image: IWICBitmapSource = wic.CreateBitmapFromSource(&converter, WICBitmapCacheOnLoad).ok()?.cast().ok()?;

            let (mut width, mut height) = (0, 0);
            image.GetSize(&mut width, &mut height).ok()?;
            let stride = width * 4;
            let mut pixels = vec![0u8; (stride * height) as usize];
            image.CopyPixels(std::ptr::null(), stride, &mut pixels).ok()?;
            Some((image, fnv1a(&pixels) | 1))
        }
    })
}

/// Small, dependency-free hash; collisions only cost a skipped repaint.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn exe_of(hwnd: isize) -> String {
    let pid = wininfo::process_id(HWND(hwnd as *mut _));
    wininfo::process_info(pid).map(|p| p.exe.to_lowercase()).unwrap_or_default()
}

fn is_window(hwnd: isize) -> bool {
    unsafe { IsWindow(Some(HWND(hwnd as *mut _))).as_bool() }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TIMER && wparam.0 == TIMER_KEEP_ON_TOP {
        keep_on_top(hwnd);
        return LRESULT(0);
    }
    if msg != WM_COPYDATA || lparam.0 == 0 {
        if msg >= WM_USER {
            return forward(msg, wparam, lparam).unwrap_or(LRESULT(0));
        }
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let data = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
    let bytes: &[u8] = if data.lpData.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) }
    };
    match data.dwData {
        COPYDATA_TRAY => LRESULT(handle_tray(bytes) as isize),
        COPYDATA_ICON_RECT => LRESULT(icon_rect(bytes)),
        // AppBar messages (work-area reservations, auto-hide) and anything
        // else are explorer's business.
        COPYDATA_APPBAR => forward(msg, wparam, lparam).unwrap_or(LRESULT(0)),
        _ => forward(msg, wparam, lparam).unwrap_or(LRESULT(0)),
    }
}

/// Hands a message on to explorer's taskbar: sent messages are sent (with a
/// timeout, so a hung explorer cannot freeze us), posted ones are posted.
fn forward(msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let target = crate::explorer::taskbar()?;
    unsafe {
        if msg != WM_COPYDATA && !InSendMessage().as_bool() {
            let _ = PostMessageW(Some(target), msg, wparam, lparam);
            return Some(LRESULT(0));
        }
        let mut result = 0usize;
        let sent = SendMessageTimeoutW(target, msg, wparam, lparam, SMTO_ABORTIFHUNG, FORWARD_TIMEOUT_MS, Some(&mut result));
        (sent.0 != 0).then_some(LRESULT(result as isize))
    }
}

fn handle_tray(bytes: &[u8]) -> bool {
    let Some(message) = tray_proto::parse_tray(bytes) else {
        log::debug!("ignoring malformed tray message ({} bytes)", bytes.len());
        return false;
    };
    let command = message.command;
    log::debug!("tray {command:?} hwnd={:#x} uid={} flags={:#x} tip={:?}", message.data.hwnd, message.data.uid, message.data.flags, message.data.tip);
    let Some(applied) = with_tray(|tray| tray.apply(message)) else {
        log::warn!("tray busy; dropped {command:?}");
        return false;
    };
    let Applied { ok, changed, balloon, removed } = applied;
    if changed || balloon.is_some() {
        EVENTS.with_borrow_mut(|events| {
            events.extend(balloon.map(TrayEvent::Balloon));
            events.extend(removed.map(TrayEvent::Removed));
        });
        let _ = unsafe { PostMessageW(Some(NOTIFY.get()), WM_APP_TRAY, WPARAM(0), LPARAM(0)) };
    }
    ok
}

/// Shell_NotifyIconGetRect: one corner per call, packed like MAKELPARAM.
fn icon_rect(bytes: &[u8]) -> isize {
    let Some(query) = tray_proto::parse_icon_rect_query(bytes) else {
        return 0;
    };
    with_tray(|tray| {
        let rect = tray.find_by(query.hwnd, query.uid, query.guid)?.screen_rect?;
        let (x, y) = if query.corner == 1 { (rect.left, rect.top) } else { (rect.right, rect.bottom) };
        Some(make_long(x, y))
    })
    .flatten()
    .unwrap_or(0)
}

fn make_long(low: i32, high: i32) -> isize {
    ((low as u16 as u32) | ((high as u16 as u32) << 16)) as i32 as isize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayInput {
    MouseMove,
    LeftDown,
    LeftUp,
    LeftDouble,
    RightDown,
    RightUp,
    PopupOpen,
    PopupClose,
    BalloonShow,
    BalloonHide,
    BalloonTimeout,
    BalloonClick,
}

/// Forwards user input on an icon to its owner, using the message format the
/// app asked for with NIM_SETVERSION. `anchor` is in screen pixels.
pub fn send(icon_id: u64, input: TrayInput, anchor: POINT) {
    let Some(Some((owner, callback, uid, version))) =
        with_tray(|tray| tray.icon(icon_id).map(|i| (i.hwnd, i.callback, i.uid, i.version)))
    else {
        return;
    };
    if callback == 0 {
        return;
    }
    let v4 = version >= 4;
    let messages: &[u32] = match input {
        TrayInput::MouseMove => &[WM_MOUSEMOVE],
        TrayInput::LeftDown => &[WM_LBUTTONDOWN],
        TrayInput::LeftUp if v4 => &[WM_LBUTTONUP, NIN_SELECT],
        TrayInput::LeftUp => &[WM_LBUTTONUP],
        TrayInput::LeftDouble => &[WM_LBUTTONDBLCLK],
        TrayInput::RightDown => &[WM_RBUTTONDOWN],
        TrayInput::RightUp if v4 => &[WM_RBUTTONUP, WM_CONTEXTMENU],
        TrayInput::RightUp => &[WM_RBUTTONUP],
        TrayInput::PopupOpen if v4 => &[NIN_POPUPOPEN],
        TrayInput::PopupClose if v4 => &[NIN_POPUPCLOSE],
        TrayInput::PopupOpen | TrayInput::PopupClose => &[],
        TrayInput::BalloonShow => &[NIN_BALLOONSHOW],
        TrayInput::BalloonHide => &[NIN_BALLOONHIDE],
        TrayInput::BalloonTimeout => &[NIN_BALLOONTIMEOUT],
        TrayInput::BalloonClick => &[NIN_BALLOONUSERCLICK],
    };
    let owner = HWND(owner as *mut _);
    if matches!(input, TrayInput::LeftUp | TrayInput::RightUp | TrayInput::LeftDouble | TrayInput::BalloonClick) {
        // The app usually calls SetForegroundWindow before showing its menu or window;
        // we just received the user's input, so we may grant it that right.
        let _ = unsafe { AllowSetForegroundWindow(wininfo::process_id(owner)) };
    }
    for &message in messages {
        let (wparam, lparam) = if v4 {
            (make_long(anchor.x, anchor.y) as usize, make_long(message as i32, uid as i32))
        } else {
            (uid as usize, message as isize)
        };
        let _ = unsafe { PostMessageW(Some(owner), callback, WPARAM(wparam), LPARAM(lparam)) };
    }
}

#[cfg(test)]
mod tests {
    use super::make_long;

    #[test]
    fn make_long_packs_signed_coordinates() {
        assert_eq!(make_long(10, 20), 10 | (20 << 16));
        assert_eq!(make_long(-1, 0) & 0xFFFF, 0xFFFF);
        assert_eq!((make_long(5, -3) >> 16) as i16, -3);
    }
}
