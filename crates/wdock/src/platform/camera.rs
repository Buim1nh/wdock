//! On-demand native preview. The driver renders frames; the shell never redraws them.
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{Error, HRESULT, Interface, Result, w};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Media::DirectShow::{IBaseFilter, IBasicVideo, ICaptureGraphBuilder2, ICreateDevEnum, IGraphBuilder, IMediaControl, IVideoWindow};
use windows::Win32::Media::MediaFoundation::{CLSID_CaptureGraphBuilder2, CLSID_FilterGraph, CLSID_SystemDeviceEnum, CLSID_VideoInputDeviceCategory, MEDIATYPE_Video, PIN_CATEGORY_PREVIEW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IMoniker};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

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
                let top = (40.0 * scale(hwnd)).round() as i32;
                let available = (r.bottom - top).max(1);
                let width = r.right.min((available as f32 * aspect) as i32).max(1);
                let height = (width as f32 / aspect) as i32;
                let _ = video.SetWindowPosition((r.right - width) / 2, top + (available - height) / 2, width, height);
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
    PREVIEW.set(Some(preview));
    unsafe { let _ = PostMessageW(Some(hwnd), SYNC_PREVIEW, WPARAM(0), LPARAM(0)); }
    if let Err(err) = result {
        log::warn!("camera preview unavailable: {err}");
        unsafe { MessageBoxW(Some(hwnd), w!("Cannot preview this camera. It may be disconnected, in use, or blocked. Open Camera settings to check access, or try the next camera."), w!("wdock camera"), MB_OK | MB_ICONINFORMATION); }
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
    unsafe {
        for (id, x, width) in [(1, 8.0, 130.0), (2, 146.0, 150.0)] {
            if let Ok(button) = GetDlgItem(Some(hwnd), id) {
                let _ = MoveWindow(button, (x * s).round() as i32, (6.0 * s).round() as i32, (width * s).round() as i32, (28.0 * s).round() as i32, true);
            }
        }
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

fn run() -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?; }
    let result = run_window();
    drop(PREVIEW.take());
    unsafe { CoUninitialize(); }
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
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: w!("wdock-camera"), hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default() };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE::default(), w!("wdock-camera"), w!("wdock — Camera preview"), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, 680, 480, None, None, Some(instance.into()), None)?;
        CreateWindowExW(WINDOW_EX_STYLE::default(), w!("BUTTON"), w!("Next camera"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 8, 6, 130, 28, Some(hwnd), Some(HMENU(std::ptr::without_provenance_mut(1))), Some(instance.into()), None)?;
        CreateWindowExW(WINDOW_EX_STYLE::default(), w!("BUTTON"), w!("Camera settings"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 146, 6, 150, 28, Some(hwnd), Some(HMENU(2usize as *mut _)), Some(instance.into()), None)?;
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
                info.ptMinTrackSize.x = (360.0 * scale(hwnd)).round() as i32;
                info.ptMinTrackSize.y = (240.0 * scale(hwnd)).round() as i32;
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
