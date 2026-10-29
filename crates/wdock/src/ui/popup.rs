//! Transient windows above the bars: tooltip, tray overflow, balloon,
//! quick settings, calendar, the launcher, the Alt+Tab switcher, the
//! overview, dock context menus and dock window previews.
//! At most one of each exists. The shell installs the graphics context on
//! every rebuild, which also closes whatever was open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use crate::model::config::TrayConfig;
use crate::model::tray::Balloon;
use crate::model::windows::WindowModel;
use crate::platform::monitors::{self, Monitor};
use crate::platform::tray_host::{self, TrayInput};
use crate::ui::backdrop::Corners;
use crate::ui::bar::{self, BarSpec, View};
use crate::ui::gfx::Gfx;
use crate::ui::popups::{
    balloon::BalloonView, calendar::CalendarView, launcher::LauncherView, menu::MenuItem, menu::MenuView,
    overflow::OverflowView, overview::OverviewView, preview::PreviewView, quick_settings::QuickSettingsView,
    switcher::SwitcherView, tooltip::TooltipView,
};
use crate::ui::popups::{preview, switcher};
use crate::ui::theme::Theme;

pub struct PopupContext {
    pub gfx: Rc<Gfx>,
    pub theme: Rc<Theme>,
    /// Balloons appear on the primary monitor, under its top bar.
    pub primary: Monitor,
    pub topbar_height: f32,
    pub tray: TrayConfig,
    pub model: Rc<RefCell<WindowModel>>,
}

thread_local! {
    static CTX: RefCell<Option<Rc<PopupContext>>> = const { RefCell::new(None) };
    static TOOLTIP: Cell<Option<HWND>> = const { Cell::new(None) };
    static OVERFLOW: Cell<Option<HWND>> = const { Cell::new(None) };
    static QUICK: Cell<Option<HWND>> = const { Cell::new(None) };
    static CALENDAR: Cell<Option<HWND>> = const { Cell::new(None) };
    static LAUNCHER: Cell<Option<HWND>> = const { Cell::new(None) };
    static BALLOON: Cell<Option<(HWND, u64)>> = const { Cell::new(None) };
    static SWITCHER: Cell<Option<HWND>> = const { Cell::new(None) };
    static OVERVIEW: Cell<Option<HWND>> = const { Cell::new(None) };
    static MENU: Cell<Option<HWND>> = const { Cell::new(None) };
    static PREVIEW: Cell<Option<HWND>> = const { Cell::new(None) };
    /// Windows shown by the open preview, so hovering the same icon keeps it.
    static PREVIEW_OF: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    static GAME_MODE: Cell<bool> = const { Cell::new(false) };
    static SHORTCUTS: Cell<Option<HWND>> = const { Cell::new(None) };
}

/// Slots closed whenever another panel opens.
fn panels() -> [&'static std::thread::LocalKey<Cell<Option<HWND>>>; 8] {
    [&OVERFLOW, &QUICK, &CALENDAR, &LAUNCHER, &SWITCHER, &OVERVIEW, &MENU, &PREVIEW]
}

fn context() -> Option<Rc<PopupContext>> {
    CTX.with_borrow(Clone::clone)
}

fn alive(hwnd: HWND) -> bool {
    unsafe { IsWindow(Some(hwnd)).as_bool() }
}

fn destroy(slot: &'static std::thread::LocalKey<Cell<Option<HWND>>>) {
    if let Some(hwnd) = slot.take()
        && alive(hwnd) {
            bar::close(hwnd);
        }
}

pub fn set_context(ctx: Option<PopupContext>) {
    destroy(&TOOLTIP);
    destroy(&OVERFLOW);
    destroy(&QUICK);
    destroy(&CALENDAR);
    destroy(&LAUNCHER);
    destroy(&SWITCHER);
    destroy(&OVERVIEW);
    destroy(&MENU);
    destroy(&PREVIEW);
    destroy(&SHORTCUTS);
    hide_balloon();
    CTX.set(ctx.map(Rc::new));
}

fn open(spec: BarSpec, view: Box<dyn View>) -> Option<HWND> {
    let ctx = context()?;
    bar::create(ctx.gfx.clone(), ctx.theme.clone(), spec, view)
        .inspect_err(|err| log::error!("popup creation failed: {err}"))
        .ok()
}

/// Forwards a model/tray change to open popups.
pub fn notify() {
    let open = [OVERFLOW.get(), QUICK.get(), BALLOON.get().map(|(h, _)| h), SWITCHER.get(), OVERVIEW.get(), PREVIEW.get()];
    for hwnd in open.into_iter().flatten() {
        if alive(hwnd) {
            bar::notify_model_changed(hwnd);
        }
    }
}

/// Keeps a popup of `size` inside the monitor containing `anchor`.
fn clamp(left: i32, top: i32, width: i32, height: i32, within: RECT) -> RECT {
    let left = left.clamp(within.left, (within.right - width).max(within.left));
    let top = top.clamp(within.top, (within.bottom - height).max(within.top));
    RECT { left, top, right: left + width, bottom: top + height }
}

fn monitor_rect(anchor: RECT) -> RECT {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect};
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
        let _ = GetMonitorInfoW(monitor, &mut info);
    }
    info.rcMonitor
}

/// Shows a tooltip centred under `anchor` (screen pixels).
pub fn show_tooltip(text: &str, anchor: RECT, scale: f32) {
    destroy(&TOOLTIP);
    let Some(ctx) = context() else {
        return;
    };
    let view = TooltipView::new(&ctx.gfx, text, scale);
    let (width, height) = view.size_px();
    let left = (anchor.left + anchor.right) / 2 - width / 2;
    let top = anchor.bottom + (6.0 * scale).round() as i32;
    let spec = BarSpec {
        rect: clamp(left, top, width, height, monitor_rect(anchor)),
        scale,
        corners: Corners::Round,
        activate: false,
    };
    TOOLTIP.set(open(spec, Box::new(view)));
}

pub fn hide_tooltip() {
    destroy(&TOOLTIP);
}

/// Opens the overflow grid under the chevron, or closes it if open.
pub fn toggle_overflow(anchor: RECT, scale: f32) {
    if OVERFLOW.get().is_some_and(alive) {
        destroy(&OVERFLOW);
        return;
    }
    let Some(ctx) = context() else {
        return;
    };
    hide_tooltip();
    let view = OverflowView::new(ctx.tray.clone(), anchor, monitor_rect(anchor), scale);
    let Some(rect) = view.frame() else {
        return;
    };
    let spec = BarSpec { rect, scale, corners: Corners::Round, activate: true };
    OVERFLOW.set(open(spec, Box::new(view)));
}

/// Opens an activatable panel built by make in slot, or closes it if open.
fn toggle_panel(
    slot: &'static std::thread::LocalKey<Cell<Option<HWND>>>,
    scale: f32,
    make: impl FnOnce() -> Box<dyn View>,
) {
    if slot.get().is_some_and(alive) {
        destroy(slot);
        return;
    }
    close_panels();
    let view = make();
    let Some(rect) = view.frame() else {
        return;
    };
    let spec = BarSpec { rect, scale, corners: Corners::Round, activate: true };
    slot.set(open(spec, view));
}

fn close_panels() {
    hide_tooltip();
    for slot in panels() {
        destroy(slot);
    }
}

/// Control-centre panel under the status icons.
pub fn toggle_quick_settings(anchor: RECT, scale: f32) {
    toggle_panel(&QUICK, scale, || Box::new(QuickSettingsView::new(anchor, scale)));
}

/// Month calendar under the clock.
pub fn toggle_calendar(anchor: RECT, scale: f32) {
    toggle_panel(&CALENDAR, scale, || Box::new(CalendarView::new(anchor, scale)));
}

/// The launcher, on the monitor under the cursor. Opening it again closes it.
pub fn toggle_launcher() {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut cursor = POINT::default();
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let (mut dpi_x, mut dpi_y) = (96, 96);
    unsafe {
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let _ = GetMonitorInfoW(monitor, &mut info);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    }
    let scale = dpi_x as f32 / 96.0;
    toggle_panel(&LAUNCHER, scale, || Box::new(LauncherView::new(info.rcMonitor, scale)));
}

/// Shows a notification card under the primary top bar, replacing any other.
/// The shortcut cheat sheet on the cursor's monitor, or closes it if open.
/// It is a widget rather than a panel: other popups leave it alone.
pub fn toggle_shortcuts(groups: Vec<crate::ui::popups::shortcuts::Group>) {
    if SHORTCUTS.get().is_some_and(alive) {
        destroy(&SHORTCUTS);
        return;
    }
    let Some(ctx) = context() else {
        return;
    };
    let monitor = cursor_monitor(&ctx);
    let top = monitor.rect.top + (ctx.topbar_height * monitor.scale).round() as i32;
    let view = crate::ui::popups::shortcuts::ShortcutsView::new(&ctx.gfx, groups, monitor.rect, top, monitor.scale);
    // Where the user last dragged it, if that is still on a screen.
    let view = match crate::ui::popups::shortcuts::saved_position() {
        Some(origin) => view.moved_to(origin),
        None => view,
    };
    let Some(rect) = view.frame() else {
        return;
    };
    let spec = BarSpec { rect, scale: monitor.scale, corners: Corners::Round, activate: false };
    SHORTCUTS.set(open(spec, Box::new(view)));
}

pub fn shortcuts_open() -> bool {
    SHORTCUTS.get().is_some_and(alive)
}

/// While a full-screen app is in front, balloons are dropped instead of shown.
pub fn set_game_mode(on: bool) {
    GAME_MODE.set(on);
    if on {
        hide_balloon();
    }
}

pub fn game_mode() -> bool {
    GAME_MODE.get()
}

pub fn show_balloon(balloon: Balloon) {
    hide_balloon();
    if GAME_MODE.get() {
        return;
    }
    let Some(ctx) = context() else {
        return;
    };
    let scale = ctx.primary.scale;
    let view = BalloonView::new(&ctx.gfx, balloon.clone(), scale);
    let (width, height) = view.size_px();
    let m = ctx.primary.rect;
    let margin = (10.0 * scale).round() as i32;
    let top = m.top + (ctx.topbar_height * scale).round() as i32 + margin;
    let spec = BarSpec {
        rect: clamp(m.right - width - margin, top, width, height, m),
        scale,
        corners: Corners::Round,
        activate: false,
    };
    if let Some(hwnd) = open(spec, Box::new(view)) {
        BALLOON.set(Some((hwnd, balloon.icon_id)));
    }
}

/// Dismisses the balloon (telling its app) — e.g. replaced or icon deleted.
pub fn hide_balloon() {
    if let Some((hwnd, icon_id)) = BALLOON.take()
        && alive(hwnd) {
            tray_host::send(icon_id, TrayInput::BalloonHide, Default::default());
            bar::close(hwnd);
        }
}

pub fn hide_balloon_for(icon_id: u64) {
    if BALLOON.get().is_some_and(|(_, id)| id == icon_id) {
        hide_balloon();
    }
}

/// Called by the balloon itself when it closes on timeout or click.
pub fn balloon_closed(hwnd: HWND) {
    if BALLOON.get().is_some_and(|(h, _)| h == hwnd) {
        BALLOON.set(None);
    }
}

/// The monitor under the cursor, falling back to the primary one.
fn cursor_monitor(ctx: &PopupContext) -> Monitor {
    monitors::describe(monitors::under_cursor()).unwrap_or(ctx.primary)
}

/// Alt+Tab: opens the switcher one step from the current window, or steps
/// the open one. `step` is +1 for Tab and -1 for Shift+Tab.
pub fn switcher(step: i32) {
    if let Some(hwnd) = SWITCHER.get().filter(|&h| alive(h)) {
        bar::command(hwnd, if step > 0 { switcher::CMD_NEXT } else { switcher::CMD_PREV });
        return;
    }
    let Some(ctx) = context() else {
        return;
    };
    let windows = ctx.model.borrow().switcher_windows();
    if windows.is_empty() {
        return;
    }
    close_panels();
    let monitor = cursor_monitor(&ctx);
    let view = SwitcherView::new(&ctx.gfx, ctx.model.clone(), windows, step, &monitor);
    let Some(rect) = view.frame() else {
        return;
    };
    let spec = BarSpec { rect, scale: monitor.scale, corners: Corners::Round, activate: true };
    SWITCHER.set(open(spec, Box::new(view)));
}

/// Win+Tab: every window of the current workspace on the cursor's monitor,
/// plus the workspace strip. Opening it again closes it.
pub fn toggle_overview() {
    if OVERVIEW.get().is_some_and(alive) {
        destroy(&OVERVIEW);
        return;
    }
    let Some(ctx) = context() else {
        return;
    };
    close_panels();
    let monitor = cursor_monitor(&ctx);
    let view = OverviewView::new(ctx.model.clone(), monitor);
    let spec = BarSpec { rect: monitor.rect, scale: monitor.scale, corners: Corners::Square, activate: true };
    OVERVIEW.set(open(spec, Box::new(view)));
}

/// A context menu whose bottom edge is centred on `anchor` (screen pixels).
pub fn show_menu(items: Vec<MenuItem>, anchor: POINT, scale: f32) {
    let Some(ctx) = context() else {
        return;
    };
    close_panels();
    let within = monitor_rect(RECT { left: anchor.x, top: anchor.y, right: anchor.x + 1, bottom: anchor.y + 1 });
    let view = MenuView::new(&ctx.gfx, items, anchor, within, scale);
    let Some(rect) = view.frame() else {
        return;
    };
    let spec = BarSpec { rect, scale, corners: Corners::Round, activate: true };
    MENU.set(open(spec, Box::new(view)));
}

pub fn menu_open() -> bool {
    MENU.get().is_some_and(alive)
}

/// Live previews of `windows` above `anchor` (a dock icon, screen pixels).
/// Showing the same windows again just keeps the open preview.
pub fn show_preview(windows: Vec<HWND>, anchor: RECT, scale: f32) {
    if let Some(hwnd) = PREVIEW.get().filter(|&h| alive(h))
        && PREVIEW_OF.with_borrow(|of| *of == windows) {
            bar::command(hwnd, preview::CMD_KEEP);
            return;
        }
    let Some(ctx) = context() else {
        return;
    };
    destroy(&PREVIEW);
    hide_tooltip();
    let view = PreviewView::new(ctx.model.clone(), windows.clone(), anchor, monitor_rect(anchor), scale);
    let Some(rect) = view.frame() else {
        return;
    };
    let spec = BarSpec { rect, scale, corners: Corners::Round, activate: false };
    PREVIEW.set(open(spec, Box::new(view)));
    PREVIEW_OF.set(windows);
}

pub fn hide_preview() {
    destroy(&PREVIEW);
    PREVIEW_OF.take();
}

/// The pointer left the dock: the preview closes unless the pointer enters it.
pub fn preview_leave() {
    if let Some(hwnd) = PREVIEW.get().filter(|&h| alive(h)) {
        bar::command(hwnd, preview::CMD_LEAVE);
    }
}

pub fn preview_open() -> bool {
    PREVIEW.get().is_some_and(alive)
}
