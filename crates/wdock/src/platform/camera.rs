//! On-demand native preview. The driver renders frames; the shell never redraws them.
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{Error, HRESULT, Interface, Result, w};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Media::DirectShow::{IBaseFilter, IBasicVideo, ICaptureGraphBuilder2, ICreateDevEnum, IGraphBuilder, IMediaControl, IVideoWindow};
use windows::Win32::Media::MediaFoundation::{CLSID_CaptureGraphBuilder2, CLSID_FilterGraph, CLSID_SystemDeviceEnum, CLSID_VideoInputDeviceCategory, MEDIATYPE_Video, PIN_CATEGORY_PREVIEW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IMoniker};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use crate::ui::camera_chrome::{Chrome, video_bounds};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

thread_local! { static CHROME: RefCell<Option<Chrome>> = const { RefCell::new(None) }; }

static OPEN: AtomicBool = AtomicBool::new(false);
thread_local! { static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) }; }
thread_local! { static RUNNING: Cell<bool> = const { Cell::new(false) }; }
const SYNC_PREVIEW: u32 = WM_APP + 1;

struct Graph {
    control: IMediaControl,
    video: IVideoWindow,
    aspect: f32,
}

impl Drop for Graph {
    fn drop(&mut self) {
        unsafe {
            let _ = self.control.Stop();
            let _ = self.video.SetVisible(0);
            let _ = self.video.SetOwner(0);
        }
    }
}

impl Graph {
    fn new(hwnd: HWND, device: &IMoniker) -> Result<Self> {
        unsafe {
            let graph: IGraphBuilder = CoCreateInstance(&CLSID_FilterGraph, None, CLSCTX_INPROC_SERVER)?;
            let builder: ICaptureGraphBuilder2 = CoCreateInstance(&CLSID_CaptureGraphBuilder2, None, CLSCTX_INPROC_SERVER)?;
            builder.SetFiltergraph(&graph)?;
            let source: IBaseFilter = device.BindToObject(None, None)?;
            graph.AddFilter(&source, w!("Camera"))?;
            builder.RenderStream(Some(&PIN_CATEGORY_PREVIEW), &MEDIATYPE_Video, &source, None, None)?;
            let basic: IBasicVideo = graph.cast()?;
            let aspect = basic.VideoWidth()?.max(1) as f32 / basic.VideoHeight()?.max(1) as f32;
            let preview = Self { control: graph.cast()?, video: graph.cast()?, aspect };
            preview.video.SetOwner(hwnd.0 as isize)?;
            preview.video.SetWindowStyle((WS_CHILD | WS_CLIPSIBLINGS).0 as i32)?;
            preview.resize(hwnd);
            preview.video.SetVisible(-1)?;
            Ok(preview)
        }
    }

    fn resize(&self, hwnd: HWND) {
        Self::resize_video(hwnd, &self.video, self.aspect);
    }

    fn resize_video(hwnd: HWND, video: &IVideoWindow, aspect: f32) {
        let mut r = RECT::default();
        unsafe {
            if GetClientRect(hwnd, &mut r).is_ok() {
                let r = video_bounds(r.right, r.bottom, scale(hwnd), aspect);
                let _ = video.SetWindowPosition(r.left, r.top, r.right-r.left, r.bottom-r.top);
            }
        }
    }
}

struct Preview {
    devices: Vec<IMoniker>,
    index: usize,
    graph: Option<Graph>,
}

impl Preview {
    fn start(&mut self, hwnd: HWND) -> Result<()> {
        self.graph = None;
        RUNNING.set(false);
        let device = self.devices.get(self.index).ok_or_else(|| Error::from_hresult(HRESULT(0x80070490u32 as i32)))?;
        self.graph = Some(Graph::new(hwnd, device)?);
        let title = windows::core::HSTRING::from(format!("wdock — Camera {} of {}", self.index + 1, self.devices.len()));
        unsafe { let _ = SetWindowTextW(hwnd, &title); }
        Ok(())
    }
}

fn start_preview(hwnd: HWND, next: bool) {
    // Camera drivers may pump messages while opening. Do not hold a TLS borrow
    // across COM calls: the user can close the window in that interval.
    let Some(mut preview) = PREVIEW.take() else { return; };
    if next { preview.index = (preview.index + 1) % preview.devices.len().max(1); }
    let result = preview.start(hwnd);
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() { return; }
    let count = preview.devices.len();
    let index = preview.index;
    CHROME.with_borrow_mut(|chrome| { if let Some(chrome) = chrome {
        chrome.error = result.is_err();
        chrome.detail = if count == 0 { "No camera connected".into() } else { format!("Camera {} of {}", index+1, count) };
    } });
    PREVIEW.set(Some(preview));
    unsafe {
        if let Ok(next) = GetDlgItem(Some(hwnd), 1) { let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(next, count>1); }
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    unsafe { let _ = PostMessageW(Some(hwnd), SYNC_PREVIEW, WPARAM(0), LPARAM(0)); }
    if let Err(err) = result {
        log::warn!("camera preview unavailable: {err}");

    }
}

fn sync_preview(hwnd: HWND) {
    let snapshot = PREVIEW.with_borrow(|p| p.as_ref().and_then(|p| p.graph.as_ref()).map(|g| (g.control.clone(), g.video.clone(), g.aspect)));
    let Some((control, video, aspect)) = snapshot else { return; };
    // Driver calls can dispatch window messages, including WM_CLOSE. Only COM
    // clones survive across those calls, never a borrow of the window state.
    Graph::resize_video(hwnd, &video, aspect);
    let play = unsafe { IsWindow(Some(hwnd)).as_bool() && !IsIconic(hwnd).as_bool() && GetAncestor(GetForegroundWindow(), GA_ROOTOWNER) == hwnd };
    if RUNNING.replace(play) != play {
        let result = unsafe { if play { control.Run() } else { control.Stop() } };
        if let Err(err) = result { RUNNING.set(false); log::warn!("camera playback state: {err}"); }
    }
}

fn scale(hwnd: HWND) -> f32 { unsafe { GetDpiForWindow(hwnd).max(96) as f32 / 96.0 } }

fn toolbar(hwnd: HWND) {
    let s = scale(hwnd);
    let mut r = RECT::default();
    unsafe {
        if GetClientRect(hwnd, &mut r).is_err() { return; }
        for (id, x, y, width, height) in [
            (1, 16.0*s, r.bottom as f32-50.0*s, 136.0*s, 34.0*s),
            (2, r.right as f32-178.0*s, r.bottom as f32-50.0*s, 162.0*s, 34.0*s),
            (3, r.right as f32-52.0*s, 14.0*s, 34.0*s, 34.0*s),
        ] {
            if let Ok(button) = GetDlgItem(Some(hwnd), id) {
                let _ = MoveWindow(button, x.round() as i32, y.round() as i32, width.round() as i32, height.round() as i32, true);
            }
        }
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub fn show() {
    if OPEN.swap(true, Ordering::AcqRel) {
        unsafe { if let Ok(hwnd) = FindWindowW(w!("wdock-camera"), None) { let _ = SetForegroundWindow(hwnd); } }
        return;
    }
    if let Err(err) = std::thread::Builder::new().name("camera-preview".into()).spawn(|| {
        if let Err(err) = run() { log::warn!("camera preview: {err}"); super::launch::open("ms-settings:camera"); }
        OPEN.store(false, Ordering::Release);
    }) {
        OPEN.store(false, Ordering::Release);
        log::warn!("cannot start camera preview: {err}");
    }
}

pub fn run() -> Result<()> {
    use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?; }
    let previous_dpi = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let result = run_window();
    drop(PREVIEW.take());
    drop(CHROME.take());
    unsafe {
        if !previous_dpi.0.is_null() { SetThreadDpiAwarenessContext(previous_dpi); }
        CoUninitialize();
    }
    result
}

fn run_window() -> Result<()> {
    unsafe {
        let enumerator: ICreateDevEnum = CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_INPROC_SERVER)?;
        let mut list = None;
        enumerator.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, &mut list, 0)?;
        let mut devices = Vec::new();
        if let Some(list) = list {
            loop {
                let mut item = [None];
                if list.Next(&mut item, None) != windows::core::HRESULT(0) { break; }
                if let Some(device) = item[0].take() { devices.push(device); }
            }
        }
        CHROME.set(Some(Chrome::new()?));
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: w!("wdock-camera"), hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default() };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE::default(), w!("wdock-camera"), w!("wdock — Camera preview"), WS_POPUP | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN, (GetSystemMetrics(SM_CXSCREEN)-720)/2, (GetSystemMetrics(SM_CYSCREEN)-560)/2, 720, 560, None, None, Some(instance.into()), None)?;
        CreateWindowExW(WINDOW_EX_STYLE::default(), w!("BUTTON"), w!("Next camera"), WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32), 8, 6, 130, 28, Some(hwnd), Some(HMENU(std::ptr::without_provenance_mut(1))), Some(instance.into()), None)?;
        CreateWindowExW(WINDOW_EX_STYLE::default(), w!("BUTTON"), w!("Camera settings"), WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32), 146, 6, 150, 28, Some(hwnd), Some(HMENU(2usize as *mut _)), Some(instance.into()), None)?;
        CreateWindowExW(WINDOW_EX_STYLE::default(), w!("BUTTON"), w!("Close"), WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32), 0, 0, 34, 34, Some(hwnd), Some(HMENU(3usize as *mut _)), Some(instance.into()), None)?;
        CHROME.with_borrow(|c| { if let Some(c) = c { c.decorate(hwnd); } });
        PREVIEW.set(Some(Preview { devices, index: 0, graph: None }));
        toolbar(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        // Start after window creation, outside its re-entrant creation messages.
        PostMessageW(Some(hwnd), WM_APP, WPARAM(0), LPARAM(0))?;
        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            if result == -1 { return Err(Error::from_thread()); }
            if result == 0 { break; }
            if msg.message == WM_KEYDOWN && msg.wParam.0 == 27 {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                continue;
            }
            if msg.message == WM_KEYDOWN && msg.wParam.0 == 13 {
                let focused = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus();
                let id = GetDlgCtrlID(focused);
                if (1..=3).contains(&id) { let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(id as usize), LPARAM(0)); }
                continue;
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut ps); }
            let mut r = RECT::default();
            unsafe { let _ = GetClientRect(hwnd, &mut r); }
            CHROME.with_borrow_mut(|c| { if let Some(c) = c { c.paint(hwnd,r.right.max(1) as u32,r.bottom.max(1) as u32,scale(hwnd)); } });
            unsafe { let _ = EndPaint(hwnd,&ps); }
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_DRAWITEM => {
            if let Some(item) = unsafe { (lp.0 as *const DRAWITEMSTRUCT).as_ref() } {
                CHROME.with_borrow(|c| { if let Some(c) = c { c.button(item,scale(hwnd)); } });
            }
            return LRESULT(1);
        }
        WM_NCHITTEST => {
            let hit = unsafe { DefWindowProcW(hwnd,msg,wp,lp) };
            if hit.0 != HTCLIENT as isize { return hit; }
            let mut r = RECT::default();
            unsafe { let _ = GetWindowRect(hwnd, &mut r); }
            let x = (lp.0 as u16 as i16) as i32-r.left;
            let y = ((lp.0 >> 16) as u16 as i16) as i32-r.top;
            if y < (64.0*scale(hwnd)) as i32 && x < r.right-r.left-(60.0*scale(hwnd)) as i32 { return LRESULT(HTCAPTION as isize); }
            return hit;
        }
        WM_KEYDOWN if wp.0 == 27 => { unsafe { let _ = PostMessageW(Some(hwnd),WM_CLOSE,WPARAM(0),LPARAM(0)); } }
        WM_COMMAND if wp.0 & 0xffff == 3 => { unsafe { let _ = PostMessageW(Some(hwnd),WM_CLOSE,WPARAM(0),LPARAM(0)); } }
        WM_APP => start_preview(hwnd, false),
        WM_COMMAND if wp.0 & 0xffff == 1 => start_preview(hwnd, true),
        WM_COMMAND if wp.0 & 0xffff == 2 => super::launch::open("ms-settings:camera"),
        WM_SIZE => {
            toolbar(hwnd);
            unsafe { let _ = PostMessageW(Some(hwnd), SYNC_PREVIEW, WPARAM(0), LPARAM(0)); }
        }
        SYNC_PREVIEW => sync_preview(hwnd),
        WM_GETMINMAXINFO => {
            if let Some(info) = unsafe { (lp.0 as *mut MINMAXINFO).as_mut() } {
                info.ptMinTrackSize.x = (400.0 * scale(hwnd)).round() as i32;
                info.ptMinTrackSize.y = (340.0 * scale(hwnd)).round() as i32;
            }
        }
        WM_DPICHANGED => {
            if let Some(r) = unsafe { (lp.0 as *const RECT).as_ref() } {
                unsafe { let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE); }
            }
        }
        WM_ACTIVATEAPP => {
            unsafe { let _ = PostMessageW(Some(hwnd), SYNC_PREVIEW, WPARAM(0), LPARAM(0)); }
        }
        WM_CLOSE => {
            drop(PREVIEW.take());
            RUNNING.set(false);
            unsafe { let _ = DestroyWindow(hwnd); }
        }
        WM_DESTROY => unsafe { PostQuitMessage(0); },
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}
