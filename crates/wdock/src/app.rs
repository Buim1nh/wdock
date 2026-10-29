//! Shell process: owns the UI thread, its message loop, the window model and
//! every bar.
//!
//! wdock runs on top of explorer (see `explorer`): explorer stays the real
//! shell with its taskbar hidden, wdock provides the top bar, dock, tray and
//! its own shortcuts, all registered with `RegisterHotKey` on combinations
//! explorer does not use.
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
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
    VK_LEFT, VK_OEM_2, VK_OEM_3, VK_Q, VK_RIGHT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DeregisterShellHookWindow, DestroyWindow, DispatchMessageW,
    GetForegroundWindow, GetMessageW, KillTimer, MSG, PBT_APMPOWERSTATUSCHANGE, PBT_POWERSETTINGCHANGE,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterShellHookWindow, RegisterWindowMessageW, SetTimer,
    TranslateMessage, WM_APP, WM_DESTROY, WM_HOTKEY, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_TIMER, WNDCLASSW,
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
use crate::services::apps_index::{self, WM_APP_INDEX};
use crate::services::media::{self, WM_APP_MEDIA};
use crate::services::{self, WM_APP_STATUS};
use crate::ui::backdrop::Corners;
use crate::ui::bar::{self, BarSpec};
use crate::ui::dock;
use crate::ui::gfx::Gfx;
use crate::ui::island;
use crate::ui::popup::{self, PopupContext};
use crate::ui::popups::shortcuts::Group;
use crate::ui::theme::Theme;
use crate::ui::topbar::TopBar;

/// Ctrl+Alt+Shift+Q always quits wdock, even when the UI is broken.
const HOTKEY_EMERGENCY_EXIT: i32 = 1;
const HOTKEY_LAUNCHER: i32 = 2;
const HOTKEY_LAUNCHER_ALT: i32 = 3;
const HOTKEY_SWITCHER: i32 = 4;
const HOTKEY_SWITCHER_BACK: i32 = 5;
const HOTKEY_OVERVIEW: i32 = 6;
const HOTKEY_WORKSPACE_PREV: i32 = 7;
const HOTKEY_WORKSPACE_NEXT: i32 = 8;
const HOTKEY_MOVE_PREV: i32 = 9;
const HOTKEY_MOVE_NEXT: i32 = 10;
const HOTKEY_SHORTCUTS: i32 = 11;
/// + n for workspace n (1-9).
const HOTKEY_WORKSPACE_BASE: i32 = 110;
const HOTKEY_MOVE_BASE: i32 = 120;

const WM_APP_REBUILD: u32 = WM_APP + 1;
/// The window model changed outside a WinEvent batch (workspace switch, pinning).
const WM_APP_NOTIFY: u32 = WM_APP + 4;
/// An AppBar notification says a full-screen app came or went.
const WM_APP_FULLSCREEN: u32 = WM_APP + 5;
const TIMER_REBUILD: usize = 1;
const TIMER_FOCUS: usize = 2;
const TIMER_RECLAIM: usize = 3;
const TIMER_POMODORO: usize = 4;
const TIMER_APPBAR: usize = 5;
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

/// Shows or hides the shortcut card, as Win+Alt+/ does (top bar button).
pub fn toggle_shortcuts() {
    let _ = unsafe { PostMessageW(Some(main_hwnd()), WM_HOTKEY, WPARAM(HOTKEY_SHORTCUTS as usize), LPARAM(0)) };
}

/// Checks the bars' reserved strips shortly (explorer applies them asynchronously).
fn check_appbars_later() {
    unsafe { SetTimer(Some(main_hwnd()), TIMER_APPBAR, APPBAR_CHECK_MS, None) };
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
    pending_device_lost: bool,
    /// The monitor whose bars are hidden under a full-screen app (game mode).
    fullscreen: Option<HMONITOR>,
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
                        appbar::register(hwnd, Edge::Top, RECT { bottom: m.top + height, ..m });
                    }
                    Err(err) => log::error!("failed to create top bar: {err}"),
                }
            }
            if self.config.island.enabled && in_scope(self.config.topbar.monitors, monitor) {
                match island::create(&gfx, &theme, &self.config.island, monitor) {
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
        // Follow the foreground app's size changes, except in game mode: there
        // the location hook would wake us for every move inside the game, and
        // leaving full screen is announced by the shell hook and the AppBar
        // notification instead.
        winevents::watch_process(if monitor.is_some() { 0 } else { wininfo::process_id(foreground) });
        if monitor == self.fullscreen {
            return;
        }
        match monitor {
            Some(_) => log::info!("full-screen app \"{}\": hiding bars on its monitor", wininfo::title(foreground)),
            None => log::info!("full-screen app gone: showing bars"),
        }
        for &(hwnd, on) in &self.bars {
            let suspended = Some(on) == monitor;
            if suspended != (Some(on) == self.fullscreen) {
                bar::suspend(hwnd, suspended);
            }
        }
        self.fullscreen = monitor;
        popup::set_game_mode(monitor.is_some());
        if monitor.is_none() {
            // Explorer gives the strip back once the full-screen app is gone.
            check_appbars_later();
        }
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
        let view = TopBar::new(&self.config.topbar, tray, self.model.clone(), monitor.handle, monitor.scale);
        bar::create(gfx.clone(), theme.clone(), spec, Box::new(view))
    }

    fn destroy_bars(&mut self) {
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

    /// Workspace shortcuts act on the monitor of the active window, else the cursor's.
    fn target_monitor(&self) -> HMONITOR {
        let model = self.model.borrow();
        match model.foreground().and_then(|fg| model.placement(fg)) {
            Some((monitor, _)) => monitor,
            None => monitors::under_cursor(),
        }
    }

    fn switch_workspace(&self, target: WorkspaceTarget) {
        let monitor = self.target_monitor();
        let mut model = self.model.borrow_mut();
        let workspace = target.resolve(model.active_workspace(monitor), model.workspace_count());
        let focus = model.switch_workspace(monitor, workspace);
        drop(model);
        if let Some(hwnd) = focus {
            focus_later(hwnd);
        }
        self.notify_bars();
    }

    /// Moves the active window to another workspace and follows it there.
    fn move_foreground(&self, target: WorkspaceTarget) {
        let mut model = self.model.borrow_mut();
        let Some(hwnd) = model.foreground() else {
            return;
        };
        let Some((monitor, current)) = model.placement(hwnd) else {
            return;
        };
        let workspace = target.resolve(current, model.workspace_count());
        if model.move_window(hwnd, workspace) {
            model.switch_workspace(monitor, workspace);
            drop(model);
            focus_later(hwnd);
            self.notify_bars();
        }
    }

    fn hotkey(&mut self, id: i32) {
        match id {
            HOTKEY_SWITCHER => popup::switcher(1),
            HOTKEY_SWITCHER_BACK => popup::switcher(-1),
            HOTKEY_OVERVIEW => popup::toggle_overview(),
            HOTKEY_SHORTCUTS => {
                popup::toggle_shortcuts(shortcut_groups(&self.config));
                self.notify_bars();
            }
            HOTKEY_WORKSPACE_PREV => self.switch_workspace(WorkspaceTarget::Previous),
            HOTKEY_WORKSPACE_NEXT => self.switch_workspace(WorkspaceTarget::Next),
            HOTKEY_MOVE_PREV => self.move_foreground(WorkspaceTarget::Previous),
            HOTKEY_MOVE_NEXT => self.move_foreground(WorkspaceTarget::Next),
            _ if id > HOTKEY_MOVE_BASE => self.move_foreground(WorkspaceTarget::Index((id - HOTKEY_MOVE_BASE - 1) as u8)),
            _ if id > HOTKEY_WORKSPACE_BASE => {
                self.switch_workspace(WorkspaceTarget::Index((id - HOTKEY_WORKSPACE_BASE - 1) as u8))
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
enum WorkspaceTarget {
    Index(u8),
    Previous,
    Next,
}

impl WorkspaceTarget {
    /// Previous/next stop at the ends rather than wrapping, like macOS.
    fn resolve(self, current: u8, count: u8) -> u8 {
        match self {
            Self::Index(n) => n.min(count - 1),
            Self::Previous => current.saturating_sub(1),
            Self::Next => (current + 1).min(count - 1),
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
    register_hotkeys(hwnd, &config)?;
    window_model::restore_orphans();
    let max_scale = monitors::enumerate().iter().map(|m| m.scale).fold(1.0, f32::max);
    let pinned = pins::load().unwrap_or_else(|| config.dock.pinned.clone());
    // Magnified icons are drawn from the same bitmap, so load it at full size.
    let icon_scale = max_scale * config.dock.magnification.max(1.0);
    let model = Rc::new(RefCell::new(WindowModel::new(
        &pinned,
        (config.dock.icon_size * icon_scale).round() as u32,
        config.workspaces.count,
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
    let tray = TrayHost::create(hwnd).inspect_err(|err| log::error!("tray host failed: {err}")).ok();
    services::start(hwnd);
    apps_index::start(hwnd);
    if config.island.enabled {
        media::start(hwnd);
    }
    model.borrow_mut().scan();

    SHELL.with_borrow_mut(|shell| {
        shell
            .insert(Shell { config, model, gfx: None, bars: Vec::new(), pending_device_lost: false, fullscreen: None })
            .rebuild();
    });
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
    Ok(())
}

/// One shortcut: id, modifiers, virtual key, name for the log.
type Hotkey = (i32, HOT_KEY_MODIFIERS, u32, String);

/// wdock's shortcuts, chosen not to collide with explorer's (Alt+Tab,
/// Win+Tab, Ctrl+Win+arrows, Win+Alt+n... stay Windows'). See default_config.toml.
fn hotkeys(config: &Config) -> Vec<Hotkey> {
    let once = MOD_NOREPEAT;
    let mut keys: Vec<Hotkey> = vec![
        (HOTKEY_LAUNCHER, MOD_WIN | MOD_ALT | once, VK_SPACE.0.into(), "Win+Alt+Space".into()),
        (HOTKEY_LAUNCHER_ALT, MOD_CONTROL | MOD_ALT | once, VK_SPACE.0.into(), "Ctrl+Alt+Space".into()),
        (HOTKEY_OVERVIEW, MOD_WIN | once, VK_OEM_3.0.into(), "Win+`".into()),
        (HOTKEY_SHORTCUTS, MOD_WIN | MOD_ALT | once, VK_OEM_2.0.into(), "Win+Alt+/".into()),
    ];
    if config.switcher.enabled {
        // No MOD_NOREPEAT: holding the key keeps stepping.
        keys.push((HOTKEY_SWITCHER, MOD_ALT, VK_OEM_3.0.into(), "Alt+`".into()));
        keys.push((HOTKEY_SWITCHER_BACK, MOD_ALT | MOD_SHIFT, VK_OEM_3.0.into(), "Alt+Shift+`".into()));
    }
    if config.workspaces.count > 1 {
        let switch = MOD_WIN | MOD_CONTROL | MOD_ALT | once;
        let carry = MOD_WIN | MOD_ALT | MOD_SHIFT | once;
        keys.push((HOTKEY_WORKSPACE_PREV, switch, VK_LEFT.0.into(), "Win+Ctrl+Alt+Left".into()));
        keys.push((HOTKEY_WORKSPACE_NEXT, switch, VK_RIGHT.0.into(), "Win+Ctrl+Alt+Right".into()));
        keys.push((HOTKEY_MOVE_PREV, carry, VK_LEFT.0.into(), "Win+Alt+Shift+Left".into()));
        keys.push((HOTKEY_MOVE_NEXT, carry, VK_RIGHT.0.into(), "Win+Alt+Shift+Right".into()));
        for n in 1..=config.workspaces.count.min(9) as i32 {
            let digit = '0' as u32 + n as u32;
            keys.push((HOTKEY_WORKSPACE_BASE + n, switch, digit, format!("Win+Ctrl+Alt+{n}")));
            keys.push((HOTKEY_MOVE_BASE + n, carry, digit, format!("Win+Alt+Shift+{n}")));
        }
    }
    keys
}

/// The cheat sheet's contents: wdock's shortcuts as configured, then the
/// Windows ones that still work underneath.
fn shortcut_groups(config: &Config) -> Vec<Group> {
    let mut ours = vec![("Win+Alt+Space".to_owned(), "Launcher")];
    if config.switcher.enabled {
        ours.push(("Alt+`".to_owned(), "Switch windows"));
    }
    ours.push(("Win+`".to_owned(), "Overview"));
    if config.workspaces.count > 1 {
        let n = config.workspaces.count.min(9);
        ours.push(("Win+Ctrl+Alt+←/→".to_owned(), "Previous / next workspace"));
        ours.push((format!("Win+Ctrl+Alt+1-{n}"), "Go to workspace"));
        ours.push(("Win+Alt+Shift+←/→".to_owned(), "Move window left / right"));
        ours.push((format!("Win+Alt+Shift+1-{n}"), "Move window to workspace"));
    }
    ours.push(("Win+Alt+/".to_owned(), "Show / hide this card"));
    ours.push(("Ctrl+Alt+Shift+Q".to_owned(), "Quit wdock"));
    let windows = [
        ("Win", "Start"),
        ("Alt+Tab", "Windows switcher"),
        ("Win+Tab", "Task view"),
        ("Win+Shift+S", "Screenshot"),
        ("Win+E", "File Explorer"),
        ("Win+R", "Run"),
        ("Win+V", "Clipboard history"),
        ("Win+.", "Emoji"),
        ("Win+D", "Show desktop"),
        ("Win+L", "Lock"),
        ("Ctrl+Shift+Esc", "Task Manager"),
    ];
    vec![
        Group { title: "WDOCK", rows: ours },
        Group { title: "WINDOWS", rows: windows.into_iter().map(|(k, v)| (k.to_owned(), v)).collect() },
    ]
}

fn register_hotkeys(hwnd: HWND, config: &Config) -> Result<()> {
    unsafe {
        RegisterHotKey(Some(hwnd), HOTKEY_EMERGENCY_EXIT, MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, VK_Q.0.into())?;
    }
    let taken: Vec<String> = hotkeys(config)
        .into_iter()
        .filter(|(id, modifiers, vk, _)| unsafe { RegisterHotKey(Some(hwnd), *id, *modifiers, *vk).is_err() })
        .map(|(_, _, _, name)| name)
        .collect();
    if !taken.is_empty() {
        log::warn!("shortcuts taken by another app: {}", taken.join(", "));
    }
    Ok(())
}

fn unregister_hotkeys(hwnd: HWND) {
    let ids = (HOTKEY_EMERGENCY_EXIT..=HOTKEY_SHORTCUTS)
        .chain((1..=9).flat_map(|n| [HOTKEY_WORKSPACE_BASE + n, HOTKEY_MOVE_BASE + n]));
    for id in ids {
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
        WM_APP_WINEVENTS => {
            let events = winevents::drain();
            let moved = !events.is_empty();
            enforce_hybrid();
            with_shell(|shell| {
                let windows_changed = shell.model.borrow_mut().apply(events);
                shell.update_fullscreen();
                let tray_changed = tray_host::with_tray(|tray| tray.prune()).unwrap_or(false);
                // Any window event can change whether a window overlaps the
                // dock (smart auto-hide), so bars always hear about it.
                if windows_changed || tray_changed || moved {
                    shell.notify_bars();
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
            if (wparam.0 as u32 == PBT_POWERSETTINGCHANGE || wparam.0 as u32 == PBT_APMPOWERSTATUSCHANGE)
                && services::refresh_power()
            {
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
                HOTKEY_LAUNCHER | HOTKEY_LAUNCHER_ALT => open_launcher(hwnd),
                id => with_shell(|shell| shell.hotkey(id)),
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
