//! Shell process: owns the UI thread, its message loop, the window model and
//! every bar.
//!
//! wdock runs on top of explorer (see `explorer`): explorer stays the real
//! shell with its taskbar hidden, wdock provides the top bar, dock and tray.
//! Its only shortcuts are the launcher and the emergency quit, registered with
//! `RegisterHotKey` on combinations explorer does not use.
//!
//! Bars are cheap to recreate, so any change to monitors, DPI, theme or the
//! GPU device simply rebuilds all of them (debounced) from the current config.
//! The window model outlives rebuilds; bars only hold a reference to it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
    VK_Q, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DeregisterShellHookWindow, DestroyWindow, DispatchMessageW,
    GetForegroundWindow, GetMessageW, KillTimer, MSG, PBT_APMPOWERSTATUSCHANGE, PBT_APMRESUMEAUTOMATIC, PBT_POWERSETTINGCHANGE,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterShellHookWindow, RegisterWindowMessageW, SetTimer,
    TranslateMessage, EVENT_SYSTEM_FOREGROUND, WM_APP, WM_DESTROY, WM_HOTKEY, WM_POWERBROADCAST,
    WM_QUERYENDSESSION, WM_TIMER, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{Result, w};

use crate::Mode;
use crate::explorer;
use crate::instance::SingleInstance;
use crate::model::config::{self, AutoHide, Config, MonitorScope};
use crate::model::pins;
use crate::model::windows::{self as window_model, WindowModel};
use crate::platform::appbar::{self, Edge};
use crate::platform::monitors::{self, Monitor};
use crate::platform::tray_host::{self, TrayEvent, TrayHost, WM_APP_TRAY};
use crate::platform::winevents::{self, WM_APP_WINEVENTS};
use crate::platform::wininfo;
use crate::platform::virtual_desktops;
use crate::services::apps_index::{self, WM_APP_INDEX};
use crate::services::media::{self, WM_APP_MEDIA};
use crate::services::privacy::{self, WM_APP_PRIVACY};
use crate::services::{self, WM_APP_STATUS};
use crate::ui::backdrop::Corners;
use crate::ui::bar::{self, BarSpec};
use crate::ui::dock;
use crate::ui::gfx::Gfx;
use crate::ui::island;
use crate::ui::popup::{self, PopupContext};
use crate::ui::theme::Theme;
use crate::ui::topbar::TopBar;

/// Ctrl+Alt+Shift+Q always quits wdock, even when the UI is broken.
const HOTKEY_EMERGENCY_EXIT: i32 = 1;
const HOTKEY_LAUNCHER: i32 = 2;

const WM_APP_REBUILD: u32 = WM_APP + 1;
/// The window model changed outside a WinEvent batch (workspace switch, pinning).
const WM_APP_NOTIFY: u32 = WM_APP + 4;
/// An AppBar notification says a full-screen app came or went.
const WM_APP_FULLSCREEN: u32 = WM_APP + 5;
/// The settings panel saved a key to config.toml: reload it and rebuild.
const WM_APP_CONFIG: u32 = WM_APP + 24;
const WM_APP_ISLAND_APPEARANCE: u32 = WM_APP + 25;
const WM_APP_LYRICS: u32 = WM_APP + 26;
const TIMER_REBUILD: usize = 1;
const TIMER_FOCUS: usize = 2;
const TIMER_RECLAIM: usize = 3;
const TIMER_POMODORO: usize = 4;
const TIMER_APPBAR: usize = 5;
const TIMER_FULLSCREEN_CHECK: usize = 6;
/// Explorer applies a reservation asynchronously; check it this long after.
const APPBAR_CHECK_MS: u32 = 1500;
/// Some apps (Steam) ignore a TaskbarCreated that arrives soon after another
/// one, e.g. ours at start right after explorer's or a previous wdock's.
/// One more broadcast a little later catches them; duplicate adds are refused.
const RECLAIM_DELAY_MS: u32 = 5000;
/// Windows shown by `ShowWindowAsync` need a moment before they can be activated.
const FOCUS_DELAY_MS: u32 = 60;
const REBUILD_DEBOUNCE_MS: u32 = 250;
const HSHELL_WINDOWACTIVATED: usize = 4;
const HSHELL_RUDEAPPACTIVATED: usize = 0x8004;
const HSHELL_FLASH: usize = 0x8006;

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static SHELLHOOK_MSG: AtomicU32 = AtomicU32::new(0);
static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static PENDING_FOCUS: Cell<Option<HWND>> = const { Cell::new(None) };
    /// Explorer's taskbar we last saw; a new one means explorer restarted.
    static EXPLORER_TASKBAR: Cell<Option<HWND>> = const { Cell::new(None) };
}

fn main_hwnd() -> HWND {
    HWND(MAIN_HWND.load(Ordering::Relaxed) as *mut _)
}

fn post_main(msg: u32) {
    let _ = unsafe { PostMessageW(Some(main_hwnd()), msg, WPARAM(0), LPARAM(0)) };
}

/// Repaints every bar and popup after a model change made outside WinEvents.
pub fn notify_model() {
    post_main(WM_APP_NOTIFY);
}

/// Repaint island views when externally supplied lyrics or background change.
pub fn notify_lyrics() {
    post_main(WM_APP_LYRICS);
}


/// Checks the bars' reserved strips shortly (explorer applies them asynchronously).
fn check_appbars_later() {
    unsafe { SetTimer(Some(main_hwnd()), TIMER_APPBAR, APPBAR_CHECK_MS, None) };
}

/// Reloads config.toml and rebuilds the bars (called after the settings panel saves).
pub fn reload_config() {
    post_main(WM_APP_CONFIG);
}

/// Applies island appearance and tab order without rebuilding the AppBars.
pub fn reload_island_appearance() {
    post_main(WM_APP_ISLAND_APPEARANCE);
}

/// Re-evaluates game mode soon (from an AppBar notification).
pub fn recheck_fullscreen() {
    post_main(WM_APP_FULLSCREEN);
}

/// Arms (or with `None` cancels) the one-shot timer for the Pomodoro phase end.
pub fn schedule_pomodoro(delay_ms: Option<u32>) {
    let hwnd = main_hwnd();
    if hwnd.is_invalid() {
        return;
    }
    match delay_ms {
        Some(ms) => unsafe {
            SetTimer(Some(hwnd), TIMER_POMODORO, ms, None);
        },
        None => unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_POMODORO);
        },
    }
}

/// Activates `hwnd` shortly, once an asynchronous show has completed.
pub fn focus_later(hwnd: HWND) {
    PENDING_FOCUS.set(Some(hwnd));
    unsafe { SetTimer(Some(main_hwnd()), TIMER_FOCUS, FOCUS_DELAY_MS, None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildReason {
    Display = 0,
    Theme = 1,
    DeviceLost = 2,
}

/// Asks the main window to rebuild all bars shortly. Safe to call from any wndproc.
pub fn request_rebuild(reason: RebuildReason) {
    let hwnd = main_hwnd();
    if !hwnd.is_invalid() {
        let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_REBUILD, WPARAM(reason as usize), LPARAM(0)) };
    }
}

struct Shell {
    config: Config,
    model: Rc<RefCell<WindowModel>>,
    gfx: Option<Rc<Gfx>>,
    /// Every bar window with the monitor it sits on.
    bars: Vec<(HWND, HMONITOR)>,
    /// Top bars and their interactive screen strips.
    topbars: Vec<(HWND, HMONITOR, RECT)>,
    pending_device_lost: bool,
    /// The monitor whose bars are hidden under a full-screen app (game mode).
    fullscreen: Option<HMONITOR>,
    /// Top bars temporarily hidden because the active app's visible frame covers them.
    covered_topbars: Vec<HWND>,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

fn with_shell(f: impl FnOnce(&mut Shell)) {
    SHELL.with_borrow_mut(|shell| {
        if let Some(shell) = shell {
            f(shell);
        }
    });
}

impl Shell {
    fn rebuild(&mut self) {
        self.destroy_bars();
        island::configure(&self.config.island);
        if std::mem::take(&mut self.pending_device_lost) {
            self.gfx = None;
        }
        let gfx = match &self.gfx {
            Some(gfx) => gfx.clone(),
            None => match Gfx::new(self.config.performance.renderer) {
                Ok(gfx) => self.gfx.insert(Rc::new(gfx)).clone(),
                Err(err) => {
                    log::error!("graphics init failed: {err}");
                    return;
                }
            },
        };
        let theme = Rc::new(Theme::resolve(&self.config.appearance));

        let monitors = monitors::enumerate();
        let max_scale = monitors.iter().map(|m| m.scale).fold(1.0, f32::max);
        self.model.borrow_mut().set_icon_size(self.config.dock.icon_source_size(max_scale));
        if let Some(primary) = monitors.iter().find(|m| m.primary).or(monitors.first()) {
            popup::set_context(Some(PopupContext {
                gfx: gfx.clone(),
                theme: theme.clone(),
                primary: *primary,
                topbar_height: self.config.topbar.height,
                tray: self.config.tray.clone(),
                model: self.model.clone(),
            }));
        }
        log::info!("rebuilding bars for {} monitor(s), dark={}", monitors.len(), theme.dark);
        for monitor in &monitors {
            let m = monitor.rect;
            if in_scope(self.config.topbar.monitors, monitor) {
                match self.create_topbar(&gfx, &theme, monitor) {
                    Ok(hwnd) => {
                        self.bars.push((hwnd, monitor.handle));
                        // Explorer keeps maximized windows below the top bar.
                        let height = (self.config.topbar.height * monitor.scale).round() as i32;
                        let reserved = RECT { bottom: m.top + height, ..m };
                        self.topbars.push((hwnd, monitor.handle, reserved));
                        appbar::register(hwnd, Edge::Top, reserved);
                    }
                    Err(err) => log::error!("failed to create top bar: {err}"),
                }
            }
            if self.config.island.enabled && in_scope(self.config.topbar.monitors, monitor) {
                match island::create(&gfx, &theme, &self.config.island, monitor, self.config.topbar.height) {
                    Ok(hwnd) => self.bars.push((hwnd, monitor.handle)),
                    Err(err) => log::error!("failed to create island: {err}"),
                }
            }
            if in_scope(self.config.dock.monitors, monitor) {
                match dock::create(&gfx, &theme, &self.config.dock, self.model.clone(), monitor) {
                    Ok(windows) => {
                        // A dock that never hides keeps maximized windows above it.
                        if let (Some(&overlay), AutoHide::Off) = (windows.first(), self.config.dock.auto_hide) {
                            let height = (dock::reserved_height(&self.config.dock) * monitor.scale).round() as i32;
                            appbar::register(overlay, Edge::Bottom, RECT { top: m.bottom - height, ..m });
                        }
                        self.bars.extend(windows.into_iter().map(|hwnd| (hwnd, monitor.handle)));
                    }
                    Err(err) => log::error!("failed to create dock: {err}"),
                }
            }
        }
        // New bars start visible; hide them again if a game is still in front.
        self.fullscreen = None;
        self.update_fullscreen();
        check_appbars_later();
    }

    /// Game mode: while a full-screen app is in front, the bars on its monitor
    /// are hidden (no topmost window over the game, nothing repaints) and
    /// notification balloons are held back.
    fn update_fullscreen(&mut self) {
        let foreground = unsafe { GetForegroundWindow() };
        let monitor = wininfo::is_fullscreen_app(foreground).then(|| wininfo::monitor(foreground));
        let covered_topbars = self.covered_topbars(foreground);
        // Follow the foreground app's size changes, except in game mode: there
        // the location hook would wake us for every move inside the game, and
        // leaving full screen is announced by the shell hook and the AppBar
        // notification instead.
        winevents::watch_process(if monitor.is_some() { 0 } else { wininfo::process_id(foreground) });
        if monitor == self.fullscreen && covered_topbars == self.covered_topbars {
            if monitor.is_none() { unsafe { let _ = KillTimer(Some(main_hwnd()), TIMER_FULLSCREEN_CHECK); } }
            return;
        }
        match monitor {
            Some(_) => log::info!("full-screen app \"{}\": hiding bars on its monitor", wininfo::title(foreground)),
            None => log::info!("full-screen app gone: showing bars"),
        }
        for &(hwnd, on) in &self.bars {
            let was_suspended = Some(on) == self.fullscreen || self.covered_topbars.contains(&hwnd);
            let suspended = Some(on) == monitor || covered_topbars.contains(&hwnd);
            if suspended != was_suspended {
                bar::suspend(hwnd, suspended);
            }
        }
        self.covered_topbars = covered_topbars;
        let fullscreen_changed = monitor != self.fullscreen;
        self.fullscreen = monitor;
        if fullscreen_changed {
            unsafe {
                if monitor.is_some() { SetTimer(Some(main_hwnd()), TIMER_FULLSCREEN_CHECK, 1000, None); }
                else { let _ = KillTimer(Some(main_hwnd()), TIMER_FULLSCREEN_CHECK); }
            }
            popup::set_game_mode(monitor.is_some());
            if monitor.is_none() {
                // Explorer gives the strip back once the full-screen app is gone.
                check_appbars_later();
            }
        }
    }

    fn covered_topbars(&self, foreground: HWND) -> Vec<HWND> {
        if !wininfo::is_app_window(foreground) {
            return Vec::new();
        }
        let frame = wininfo::visible_rect(foreground);
        self.topbars.iter().filter_map(|&(hwnd, _, strip)| {
            let overlaps = frame.left < strip.right && frame.right > strip.left
                && frame.top < strip.bottom && frame.bottom > strip.top;
            overlaps.then_some(hwnd)
        }).collect()
    }

    fn create_topbar(&self, gfx: &Rc<Gfx>, theme: &Rc<Theme>, monitor: &Monitor) -> Result<HWND> {
        let m = monitor.rect;
        let height = (self.config.topbar.height * monitor.scale).round() as i32;
        let spec = BarSpec {
            rect: RECT { left: m.left, top: m.top, right: m.right, bottom: m.top + height },
            scale: monitor.scale,
            corners: Corners::Square,
            activate: false,
        };
        let tray = monitor.primary.then(|| self.config.tray.clone());
        let view = TopBar::new(&self.config.topbar, tray, self.model.clone(), monitor.scale);
        bar::create(gfx.clone(), theme.clone(), spec, Box::new(view))
    }

    fn destroy_bars(&mut self) {
        self.topbars.clear();
        self.covered_topbars.clear();
        for (hwnd, _) in self.bars.drain(..) {
            appbar::unregister(hwnd);
            let _ = unsafe { DestroyWindow(hwnd) };
        }
    }

    fn notify_bars(&self) {
        for &(hwnd, _) in &self.bars {
            bar::notify_model_changed(hwnd);
        }
        popup::notify();
    }

    fn notify_foreground_bars(&self) {
        for &(hwnd, _) in &self.bars {
            bar::notify_foreground_changed(hwnd);
        }
        popup::notify_foreground_changed();
    }

    /// Applies the saved config live. The settings panel is rebuilt with the
    /// bars (its popups are closed by `rebuild`), so it is reopened if it was open.
    fn reload_config(&mut self, hwnd: HWND) {
        let reopen = popup::settings_reopen();
        self.config = config::load();
        self.rebuild();
        if self.config.island.enabled {
            media::start(hwnd);
        } else {
            media::stop();
        }
        if let Some((anchor, scale)) = reopen {
            popup::toggle_settings(anchor, scale);
        }
    }
}

fn in_scope(scope: MonitorScope, monitor: &Monitor) -> bool {
    scope == MonitorScope::All || monitor.primary
}

/// Keeps explorer's taskbar out of sight; it re-shows itself now and then.
/// A taskbar we have not seen means explorer restarted: take the tray back
/// (apps re-register with the topmost Shell_TrayWnd) and re-register appbars.
fn enforce_hybrid() {
    if explorer::hide_taskbar() {
        tray_host::reclaim();
    }
    let current = explorer::taskbar();
    if current.is_some() && EXPLORER_TASKBAR.replace(current) != current {
        log::info!("explorer's taskbar (re)appeared: reclaiming the tray");
        tray_host::reclaim();
        request_rebuild(RebuildReason::Display);
    }
}

pub fn run(mode: Mode) -> Result<()> {
    let Some(_instance) = SingleInstance::acquire()? else {
        log::warn!("another wdock shell is already running; exiting");
        return Ok(());
    };

    if let Err(err) = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
        log::warn!("could not set per-monitor DPI awareness: {err}");
    }
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()? };
    unsafe { windows::Win32::System::Ole::OleInitialize(None)? };

    let hwnd = create_main_window()?;
    MAIN_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    unsafe {
        SHELLHOOK_MSG.store(RegisterWindowMessageW(w!("SHELLHOOK")), Ordering::Relaxed);
        TASKBAR_CREATED_MSG.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        if !RegisterShellHookWindow(hwnd).as_bool() {
            log::warn!("RegisterShellHookWindow failed; attention flashes will not show");
        }
    }

    let config = config::load();
    register_hotkeys(hwnd)?;
    window_model::restore_orphans();
    let max_scale = monitors::enumerate().iter().map(|m| m.scale).fold(1.0, f32::max);
    let pinned = pins::load_or_import(&config.dock.pinned);
    // Magnified icons are drawn from the same bitmap, so load it at full size.
    let model = Rc::new(RefCell::new(WindowModel::new(
        &pinned,
        config.dock.icon_source_size(max_scale),
        1,
    )));
    // Install hooks before scanning so no window slips through in between.
    let _hooks = winevents::install(hwnd);
    // Known before our tray's own TaskbarCreated broadcast comes back to us.
    EXPLORER_TASKBAR.set(explorer::taskbar());
    if EXPLORER_TASKBAR.get().is_none() {
        log::warn!("explorer is not running: packaged apps, screenshots and Settings need it");
    }
    // Before the tray exists: switching on auto-hide re-orders explorer's
    // taskbar to the top, and our tray must end up above it.
    explorer::hide_taskbar();
    services::start(hwnd);
    apps_index::start(hwnd);
    if config.island.enabled {
        media::start(hwnd);
        privacy::start(hwnd);
    }
    model.borrow_mut().scan();
    virtual_desktops::start(hwnd);

    SHELL.with_borrow_mut(|shell| {
        shell
            .insert(Shell { config, model, gfx: None, bars: Vec::new(), topbars: Vec::new(), pending_device_lost: false, fullscreen: None, covered_topbars: Vec::new() })
            .rebuild();
    });
    // Register appbars while Explorer's real Shell_TrayWnd is still the
    // shell API's target. Forwarding appbar IPC through our tray window can
    // lose the shared-memory reply used for position negotiation.
    let tray = TrayHost::create(hwnd).inspect_err(|err| log::error!("tray host failed: {err}")).ok();
    unsafe { SetTimer(Some(hwnd), TIMER_RECLAIM, RECLAIM_DELAY_MS, None) };
    log::info!("shell ready ({mode:?}); Ctrl+Alt+Shift+Q exits");

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    popup::set_context(None);
    services::stop();
    media::stop();
    privacy::stop();
    apps_index::stop();
    SHELL.with_borrow_mut(|shell| {
        if let Some(mut shell) = shell.take() {
            shell.destroy_bars();
            island::shutdown();
            shell.model.borrow_mut().show_all_hidden();
        }
    });
    explorer::restore_taskbar();
    // Hand tray icons back to explorer's taskbar.
    drop(tray);
    unsafe { windows::Win32::System::Ole::OleUninitialize(); }
    Ok(())
}

/// wdock's only shortcut besides the emergency quit. Win+Alt+Space does not
/// collide with explorer's; everything else is done with the mouse.
fn register_hotkeys(hwnd: HWND) -> Result<()> {
    unsafe {
        RegisterHotKey(Some(hwnd), HOTKEY_EMERGENCY_EXIT, MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, VK_Q.0.into())?;
        if RegisterHotKey(Some(hwnd), HOTKEY_LAUNCHER, MOD_WIN | MOD_ALT | MOD_NOREPEAT, VK_SPACE.0.into()).is_err() {
            log::warn!("Win+Alt+Space is taken by another app; the launcher stays on the top bar button");
        }
    }
    Ok(())
}

fn unregister_hotkeys(hwnd: HWND) {
    for id in [HOTKEY_EMERGENCY_EXIT, HOTKEY_LAUNCHER] {
        let _ = unsafe { UnregisterHotKey(Some(hwnd), id) };
    }
}

/// Hidden top-level window: unlike a message-only window it receives
/// broadcasts and can register as a shell hook window.
fn create_main_window() -> Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: w!("wdock.Main"),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(windows::core::Error::from_thread());
    }
    unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("wdock.Main"),
            w!("wdock"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

fn open_launcher(hwnd: HWND) {
    apps_index::refresh(hwnd);
    popup::toggle_launcher();
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg != 0 && msg == SHELLHOOK_MSG.load(Ordering::Relaxed) {
        match wparam.0 {
            HSHELL_FLASH => {
                let target = HWND(lparam.0 as *mut _);
                with_shell(|shell| {
                    if shell.model.borrow_mut().set_flashing(target, true) {
                        shell.notify_bars();
                    }
                });
            }
            // User32 tells the shell when a "rude" (full-screen) window
            // activates or stops being full screen; react at once.
            HSHELL_WINDOWACTIVATED | HSHELL_RUDEAPPACTIVATED => with_shell(Shell::update_fullscreen),
            _ => {}
        }
        return LRESULT(0);
    }
    if msg != 0 && msg == TASKBAR_CREATED_MSG.load(Ordering::Relaxed) {
        // Our own tray broadcasts this too; only a new explorer taskbar matters.
        enforce_hybrid();
        return LRESULT(0);
    }

    match msg {
        crate::platform::tray_preferences::WM_TRAY_PREFERENCES => {
            let prefs = crate::platform::tray_preferences::load();
            tray_host::with_tray(|tray| tray.set_windows_preferences(prefs));
            with_shell(|shell| shell.notify_bars());
            LRESULT(0)
        }
        virtual_desktops::WM_DESKTOP_CHANGED => {
            virtual_desktops::refresh();
            with_shell(|shell| shell.notify_bars());
            LRESULT(0)
        }
        WM_APP_WINEVENTS => {
            let events = winevents::drain();
            let moved = !events.is_empty();
            let foreground_only = moved && events.iter().all(|(event, _)| *event == EVENT_SYSTEM_FOREGROUND);
            // A rapid Alt+Tab sequence is only a foreground change. Shell
            // activation notifications already update full-screen state, so
            // skip taskbar enumeration and the duplicate fullscreen scan here.
            if moved && !foreground_only {
                enforce_hybrid();
            }
            with_shell(|shell| {
                let windows_changed = shell.model.borrow_mut().apply(events);
                if moved && !foreground_only {
                    shell.update_fullscreen();
                }
                let tray_changed = if foreground_only {
                    false
                } else {
                    tray_host::with_tray(|tray| tray.prune()).unwrap_or(false)
                };
                // Any window event can change whether a window overlaps the
                // dock (smart auto-hide), so bars always hear about it.
                if windows_changed || tray_changed || moved {
                    if foreground_only {
                        shell.notify_foreground_bars();
                    } else {
                        shell.notify_bars();
                    }
                }
            });
            LRESULT(0)
        }
        WM_APP_TRAY => {
            for event in tray_host::take_events() {
                match event {
                    TrayEvent::Balloon(balloon) => popup::show_balloon(balloon),
                    TrayEvent::Removed(id) => popup::hide_balloon_for(id),
                }
            }
            with_shell(|shell| shell.notify_bars());
            LRESULT(0)
        }
        WM_APP_MEDIA => {
            island::media_changed();
            LRESULT(0)
        }
        WM_APP_LYRICS => {
            island::lyrics_changed();
            LRESULT(0)
        }
        WM_APP_PRIVACY => {
            island::media_changed();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_POMODORO => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_POMODORO) };
            island::pomodoro_due();
            LRESULT(0)
        }
        WM_APP_INDEX => {
            apps_index::receive();
            LRESULT(0)
        }
        WM_APP_STATUS => {
            if services::handle_message(wparam, lparam) {
                with_shell(|shell| shell.notify_bars());
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC {
                services::refresh_displays();
            }
            let saver_changed = wparam.0 as u32 == PBT_POWERSETTINGCHANGE && services::power::power_setting_changed(lparam);
            if saver_changed || ((wparam.0 as u32 == PBT_POWERSETTINGCHANGE || wparam.0 as u32 == PBT_APMPOWERSTATUSCHANGE)
                && services::refresh_power()) {
                with_shell(|shell| shell.notify_bars());
            }
            LRESULT(1)
        }
        WM_HOTKEY => {
            match wparam.0 as i32 {
                HOTKEY_EMERGENCY_EXIT => {
                    log::info!("emergency exit hotkey pressed");
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                HOTKEY_LAUNCHER => open_launcher(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        // Sign-out or shutdown: show workspace-hidden windows so apps asking to
        // save their documents are visible, and put explorer's taskbar setting
        // back so it is saved unchanged. A cancelled sign-out re-hides it.
        WM_QUERYENDSESSION => {
            with_shell(|shell| shell.model.borrow_mut().show_all_hidden());
            explorer::restore_taskbar();
            LRESULT(1)
        }
        WM_APP_NOTIFY => {
            with_shell(|shell| shell.notify_bars());
            LRESULT(0)
        }
        WM_APP_FULLSCREEN => {
            with_shell(Shell::update_fullscreen);
            LRESULT(0)
        }
        WM_APP_CONFIG => {
            with_shell(|shell| shell.reload_config(hwnd));
            LRESULT(0)
        }
        WM_APP_ISLAND_APPEARANCE => {
            with_shell(|shell| {
                let updated = config::load().island;
                shell.config.island.widget_order = updated.widget_order;
                shell.config.island.style = updated.style;
                shell.config.island.notch_width = updated.notch_width;
                island::refresh_appearance();
            });
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_FULLSCREEN_CHECK => {
            with_shell(Shell::update_fullscreen);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_APPBAR => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_APPBAR) };
            // Explorer drops reservations for full-screen apps on purpose.
            let mut idle = false;
            with_shell(|shell| idle = shell.fullscreen.is_none());
            if idle && appbar::verify() {
                check_appbars_later();
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_RECLAIM => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_RECLAIM) };
            tray_host::reclaim();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_FOCUS => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_FOCUS) };
            if let Some(target) = PENDING_FOCUS.take() {
                wininfo::activate(target);
            }
            LRESULT(0)
        }
        WM_APP_REBUILD => {
            if wparam.0 == RebuildReason::DeviceLost as usize {
                with_shell(|shell| shell.pending_device_lost = true);
            }
            if wparam.0 == RebuildReason::Display as usize {
                services::refresh_displays();
            }
            unsafe { SetTimer(Some(hwnd), TIMER_REBUILD, REBUILD_DEBOUNCE_MS, None) };
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_REBUILD => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_REBUILD) };
            with_shell(Shell::rebuild);
            LRESULT(0)
        }
        WM_DESTROY => {
            unregister_hotkeys(hwnd);
            let _ = unsafe { DeregisterShellHookWindow(hwnd) };
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
