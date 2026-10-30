//! A borderless, non-activating, always-on-top window drawn through a
//! composition swap chain. The top bar and the dock are both `Bar`s with
//! different `View`s.

use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, SW_HIDE, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, MA_ACTIVATE, SW_SHOW, SetForegroundWindow,
    WA_INACTIVE, WM_ACTIVATE, WM_CLOSE, WM_LBUTTONDBLCLK, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN,
    WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_MOUSEWHEEL, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, SendMessageW, SWP_NOZORDER, WM_RBUTTONUP, CreateWindowExW, DefWindowProcW, GWLP_USERDATA,
    GetClientRect, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IDC_ARROW, LoadCursorW,
    MA_NOACTIVATE, PostMessageW, RegisterClassW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCCREATE,
    WM_NCDESTROY, WM_PAINT, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, Result, w};

use crate::app::{RebuildReason, request_rebuild};
use crate::platform::appbar;
use crate::ui::backdrop::{self, Corners};
use crate::ui::gfx::{Gfx, Surface};
use crate::ui::theme::Theme;

const CLASS_NAME: PCWSTR = w!("wdock.Bar");
const WM_MOUSELEAVE: u32 = 0x02A3;

pub struct PaintCx<'a> {
    pub dc: &'a ID2D1DeviceContext,
    pub gfx: &'a Gfx,
    pub theme: &'a Theme,
    pub width: f32,
    pub height: f32,
    /// Monitor DPI / 96. Views lay out in DIPs and multiply by this.
    pub scale: f32,
    /// The window's root composition visual, for views that add animated layers.
    pub root: &'a windows::Win32::Graphics::DirectComposition::IDCompositionVisual,
}

/// Content of a bar. Event handlers return `true` when the bar must repaint.
pub trait View {
    fn attach(&mut self, _hwnd: HWND) {}
    fn paint(&mut self, cx: &PaintCx);
    fn mouse_move(&mut self, _x: f32, _y: f32) -> bool {
        false
    }
    fn mouse_leave(&mut self) -> bool {
        false
    }
    fn mouse_down(&mut self, _x: f32, _y: f32, _button: Button) -> bool {
        false
    }
    fn mouse_up(&mut self, _x: f32, _y: f32, _button: Button) -> bool {
        false
    }
    fn double_click(&mut self, _x: f32, _y: f32, _button: Button) -> bool {
        false
    }
    /// Mouse wheel over the bar; `delta` is positive when rolled away from the user.
    fn wheel(&mut self, _x: f32, _y: f32, _delta: i16) -> bool {
        false
    }
    /// Key press in an activatable popup (virtual-key code).
    fn key_down(&mut self, _vk: u16) -> bool {
        false
    }
    /// Key release, including Alt (`WM_SYSKEYUP`).
    fn key_up(&mut self, _vk: u16) -> bool {
        false
    }
    /// Translated character, including control characters such as backspace.
    fn char_input(&mut self, _c: char) -> bool {
        false
    }
    /// An activatable popup lost focus. Return `true` to close it.
    fn deactivated(&mut self) -> bool {
        false
    }
    fn timer(&mut self, _id: usize) -> bool {
        false
    }
    /// The window model changed (see `notify_model_changed`).
    fn model_changed(&mut self) -> bool {
        false
    }
    /// The shell hides bars over a full-screen app (game mode) and shows them
    /// again afterwards. Return `true` if the view managed its own visibility.
    fn suspend(&mut self, _suspended: bool) -> bool {
        false
    }
    /// A view-specific request sent with `command`.
    fn command(&mut self, _id: usize) -> bool {
        false
    }
    /// Where the window should be, for views whose size depends on content.
    fn frame(&self) -> Option<RECT> {
        None
    }
}

/// Posted to every bar after the window model changed.
pub const WM_APP_MODEL: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 10;

pub fn notify_model_changed(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_MODEL, WPARAM(0), LPARAM(0));
    }
}

/// Like `notify_model_changed`, but handled before returning (same thread only).
pub fn refresh_now(hwnd: HWND) {
    unsafe {
        SendMessageW(hwnd, WM_APP_MODEL, Some(WPARAM(0)), Some(LPARAM(0)));
    }
}

const WM_APP_COMMAND: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 11;
const WM_APP_SUSPEND: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 12;

/// Hides (or restores) a bar for game mode; see `View::suspend`.
pub fn suspend(hwnd: HWND, suspended: bool) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_SUSPEND, WPARAM(suspended as usize), LPARAM(0));
    }
}

/// Delivers `View::command(id)` synchronously.
pub fn command(hwnd: HWND, id: usize) {
    unsafe {
        SendMessageW(hwnd, WM_APP_COMMAND, Some(WPARAM(id)), Some(LPARAM(0)));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

pub struct BarSpec {
    /// Window rectangle in physical pixels.
    pub rect: RECT,
    pub scale: f32,
    pub corners: Corners,
    /// Popups that take focus (and close when they lose it). Bars never do.
    pub activate: bool,
}

struct BarState {
    gfx: Rc<Gfx>,
    theme: Rc<Theme>,
    view: Box<dyn View>,
    surface: Option<Surface>,
    scale: f32,
    activate: bool,
    tracking_mouse: bool,
}

pub fn create(gfx: Rc<Gfx>, theme: Rc<Theme>, spec: BarSpec, view: Box<dyn View>) -> Result<HWND> {
    register_class()?;
    let state = Box::new(BarState {
        gfx,
        theme,
        view,
        surface: None,
        scale: spec.scale,
        activate: spec.activate,
        tracking_mouse: false,
    });
    let width = spec.rect.right - spec.rect.left;
    let height = spec.rect.bottom - spec.rect.top;

    let mut ex_style = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP;
    if !spec.activate {
        ex_style |= WS_EX_NOACTIVATE;
    }
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            CLASS_NAME,
            w!("wdock"),
            WS_POPUP,
            spec.rect.left,
            spec.rect.top,
            width,
            height,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            Some(Box::into_raw(state) as *const c_void),
        )?
    };

    // WM_NCCREATE stored the state; from here WM_NCDESTROY owns freeing it.
    let state = unsafe { state_mut(hwnd) }.expect("bar state set in WM_NCCREATE");
    backdrop::apply(hwnd, &state.theme, spec.corners);
    state.surface = Some(Surface::new(&state.gfx, hwnd, width as u32, height as u32)?);
    state.view.attach(hwnd);
    unsafe {
        if spec.activate {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    Ok(hwnd)
}

/// Closes a bar or popup after the current message.
pub fn close(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// Window rectangle in screen pixels.
pub fn screen_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    rect
}

pub fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn register_class() -> Result<()> {
    static REGISTER: Once = Once::new();
    let mut result = Ok(());
    REGISTER.call_once(|| unsafe {
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: match GetModuleHandleW(None) {
                Ok(module) => module.into(),
                Err(err) => {
                    result = Err(err);
                    return;
                }
            },
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            result = Err(windows::core::Error::from_thread());
        }
    });
    result
}

unsafe fn state_mut<'a>(hwnd: HWND) -> Option<&'a mut BarState> {
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut BarState).as_mut() }
}

fn button_of(msg: u32) -> Button {
    match msg {
        WM_RBUTTONDOWN | WM_RBUTTONUP | WM_RBUTTONDBLCLK => Button::Right,
        _ => Button::Left,
    }
}

fn mouse_pos(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCREATE => {
                let create = &*(lparam.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            WM_NCDESTROY => {
                let state = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut BarState;
                if !state.is_null() {
                    drop(Box::from_raw(state));
                }
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            // Display topology, DPI and theme changes rebuild every bar (debounced).
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                request_rebuild(RebuildReason::Display);
                return LRESULT(0);
            }
            WM_SETTINGCHANGE if is_theme_change(lparam) => {
                request_rebuild(RebuildReason::Theme);
                return LRESULT(0);
            }
            _ => {}
        }

        let Some(state) = state_mut(hwnd) else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };

        let repaint = match msg {
            WM_PAINT => {
                paint(hwnd, state);
                let _ = ValidateRect(Some(hwnd), None);
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                if !state.tracking_mouse {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    state.tracking_mouse = TrackMouseEvent(&mut track).is_ok();
                }
                let (x, y) = mouse_pos(lparam);
                state.view.mouse_move(x, y)
            }
            WM_MOUSELEAVE => {
                state.tracking_mouse = false;
                state.view.mouse_leave()
            }
            WM_MOUSEACTIVATE => {
                return LRESULT(if state.activate { MA_ACTIVATE } else { MA_NOACTIVATE } as isize);
            }
            WM_ACTIVATE => {
                if wparam.0 & 0xFFFF == WA_INACTIVE as usize && state.view.deactivated() {
                    close(hwnd);
                }
                return LRESULT(0);
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                let (x, y) = mouse_pos(lparam);
                state.view.mouse_down(x, y, button_of(msg))
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                let (x, y) = mouse_pos(lparam);
                state.view.mouse_up(x, y, button_of(msg))
            }
            WM_MOUSEWHEEL => {
                // Wheel coordinates are in screen space.
                let mut point = POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut point);
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
                state.view.wheel(point.x as f32, point.y as f32, delta)
            }
            WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK => {
                let (x, y) = mouse_pos(lparam);
                state.view.double_click(x, y, button_of(msg))
            }
            WM_TIMER => state.view.timer(wparam.0),
            WM_APP_MODEL => state.view.model_changed(),
            WM_APP_COMMAND => state.view.command(wparam.0),
            appbar::WM_APP_APPBAR => {
                match wparam.0 {
                    appbar::ABN_POSCHANGED => appbar::position_changed(hwnd),
                    appbar::ABN_FULLSCREENAPP => crate::app::recheck_fullscreen(),
                    _ => {}
                }
                return LRESULT(0);
            }
            WM_APP_SUSPEND => {
                let suspended = wparam.0 != 0;
                if !state.view.suspend(suspended) {
                    let _ = ShowWindow(hwnd, if suspended { SW_HIDE } else { SW_SHOWNOACTIVATE });
                }
                !suspended
            }
            WM_KEYDOWN => state.view.key_down(wparam.0 as u16),
            // Keys pressed while Alt is held (the switcher) arrive as system keys.
            WM_SYSKEYDOWN => {
                if !state.view.key_down(wparam.0 as u16) {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                true
            }
            WM_KEYUP | WM_SYSKEYUP => state.view.key_up(wparam.0 as u16),
            WM_SYSCHAR if state.activate => return LRESULT(0),
            WM_CHAR => match char::from_u32(wparam.0 as u32) {
                Some(c) => state.view.char_input(c),
                None => false,
            },
            _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
        };
        if repaint {
            // Content-sized views (dock, launcher) may need a new frame first.
            // A resized swap chain holds nothing until drawn, so draw right
            // away instead of waiting for WM_PAINT (no blank frame).
            if apply_frame(hwnd, state) {
                paint(hwnd, state);
                let _ = ValidateRect(Some(hwnd), None);
            } else {
                invalidate(hwnd);
            }
        }
        LRESULT(0)
    }
}

/// Moves/resizes the window to the view's requested frame, swap chain first.
fn apply_frame(hwnd: HWND, state: &mut BarState) -> bool {
    let Some(frame) = state.view.frame() else {
        return false;
    };
    let mut current = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut current);
    }
    if current == frame {
        return false;
    }
    let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
    let resized = width != current.right - current.left || height != current.bottom - current.top;
    if let Some(surface) = state.surface.as_ref().filter(|_| resized)
        && let Err(err) = surface.resize(width as u32, height as u32) {
            log::error!("swap chain resize failed: {err}");
            request_rebuild(RebuildReason::DeviceLost);
            return false;
        }
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), frame.left, frame.top, width, height, SWP_NOACTIVATE | SWP_NOZORDER);
    }
    // Only a resized swap chain is blank and needs drawing right away.
    resized
}

fn paint(hwnd: HWND, state: &mut BarState) {
    let BarState { gfx, theme, view, surface, scale, .. } = state;
    let Some(surface) = surface else {
        return;
    };
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let root = surface.root();
    let drawn = surface.draw(gfx, |dc| {
        view.paint(&PaintCx {
            dc,
            gfx,
            theme,
            width: (client.right - client.left) as f32,
            height: (client.bottom - client.top) as f32,
            scale: *scale,
            root,
        });
    });
    if let Err(err) = drawn {
        // Typically D2DERR_RECREATE_TARGET / device removed: rebuild the GPU objects.
        log::error!("render failed: {err}");
        request_rebuild(RebuildReason::DeviceLost);
    }
}

fn is_theme_change(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    let area = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
    area.is_ok_and(|area| area == "ImmersiveColorSet")
}
