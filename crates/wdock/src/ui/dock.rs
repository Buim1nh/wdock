//! Dock: pinned apps, then a separator and other running apps. A dot marks
//! running apps (wider for the active one, orange when asking for attention).
//!
//! The dock is two windows. The shelf is the acrylic plate; above it sits a
//! transparent overlay that draws the icons and takes input. The overlay grows
//! upwards while the pointer is over it so hovered icons can magnify past the
//! shelf, and shrinks back afterwards: nothing animates (and no timer runs)
//! unless the pointer is on the dock or it is sliding in or out.
//!
//! Click launches, focuses, minimizes or cycles (see `activate_entry`); right
//! click opens a menu; hovering a running app previews its windows; dragging
//! reorders pinned apps, pins running ones, or unpins when dropped off the dock.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::f32::consts::PI;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, KillTimer, SW_HIDE, SW_SHOWNOACTIVATE, SetTimer, ShowWindow,
};
use windows::core::Result;

use crate::model::config::{AutoHide, Backdrop, DockConfig, DockWindows};
use crate::model::windows::{Entry, EntrySource, WindowModel};
use crate::platform::monitors::Monitor;
use crate::platform::{launch, wininfo};
use crate::ui::backdrop::{Corners, DWM_CORNER_RADIUS};
use crate::ui::bar::{self, BarSpec, Button, PaintCx, View};
use crate::services::icons;
use crate::ui::gfx::{Gfx, rect, rgba, rounded};
use crate::ui::popup;
use crate::ui::popups::menu::MenuItem;
use crate::ui::text;
use crate::ui::theme::Theme;

const PADDING: f32 = 6.0;
const GAP: f32 = 4.0;
/// Extra room around each icon for the hover plate.
const SLOT_INSET: f32 = 4.0;
/// Extra space between pinned and running apps, holding the divider.
const SEPARATOR: f32 = 10.0;
const MARGIN_BOTTOM: f32 = 8.0;
const DOT: f32 = 4.0;
/// Cache-key prefix of resting-size icon bitmaps.
const REST: &str = "rest:";
const ACTIVE_DOT_WIDTH: f32 = 12.0;
/// Room above a hovered icon for its name.
const LABEL: f32 = 30.0;
/// How many slots to either side of the pointer magnification reaches.
const MAGNIFY_RANGE: f32 = 2.6;
const DRAG_THRESHOLD: f32 = 6.0;
/// Dropping a pinned icon this many slots above the shelf unpins it.
const REMOVE_DISTANCE: f32 = 1.2;
/// Height of the invisible strip that brings a hidden dock back.
const TRIGGER_PX: i32 = 2;

const TIMER_ANIMATE: usize = 1;
const TIMER_PREVIEW: usize = 2;
const TIMER_HIDE: usize = 3;
const FRAME_MS: u32 = 15;
const PREVIEW_DELAY_MS: u32 = 500;
const HIDE_DELAY_MS: u32 = 600;
/// Fraction of the remaining distance covered per animation frame.
const EASE: f32 = 0.3;

/// Where the shelf should be, shared from the overlay that lays it out.
#[derive(Clone, Copy, Default)]
struct ShelfFrame {
    rect: RECT,
    visible: bool,
}

/// The acrylic plate under the icons. It never takes input: the overlay
/// above it covers it entirely.
struct Shelf {
    hwnd: HWND,
    frame: Rc<Cell<ShelfFrame>>,
    shown: bool,
    suspended: bool,
}

impl View for Shelf {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }

    fn frame(&self) -> Option<RECT> {
        Some(self.frame.get().rect)
    }

    fn suspend(&mut self, suspended: bool) -> bool {
        self.suspended = suspended;
        self.model_changed();
        true
    }

    fn model_changed(&mut self) -> bool {
        let visible = self.frame.get().visible && !self.suspended;
        if visible != self.shown {
            self.shown = visible;
            unsafe {
                let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
        }
        visible
    }

    fn paint(&mut self, cx: &PaintCx) {
        let radius = DWM_CORNER_RADIUS * cx.scale;
        if cx.theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(cx.theme.solid_fill) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(0.0, 0.0, cx.width, cx.height), radius), &brush) };
            }
        if let Ok(brush) = cx.gfx.brush(cx.theme.border) {
            let inset = rect(0.5, 0.5, cx.width - 0.5, cx.height - 0.5);
            unsafe { cx.dc.DrawRoundedRectangle(&rounded(inset, radius), &brush, 1.0, None) };
        }
    }
}

/// Screen-bottom space (DIPs) the dock keeps free of maximized windows, or 0
/// when it auto-hides.
pub fn reserved_height(config: &DockConfig) -> f32 {
    if config.auto_hide != AutoHide::Off {
        return 0.0;
    }
    PADDING * 2.0 + config.icon_size + SLOT_INSET * 2.0 + MARGIN_BOTTOM
}

/// Creates the shelf and the dock overlay for one monitor. Destroy the
/// returned windows in order (overlay first).
pub fn create(gfx: &Rc<Gfx>, theme: &Rc<Theme>, config: &DockConfig, model: Rc<RefCell<WindowModel>>, monitor: &Monitor) -> Result<Vec<HWND>> {
    let shared = Rc::new(Cell::new(ShelfFrame::default()));
    let mut dock = Dock::new(config, model, monitor, shared.clone());
    let shelf_rect = shared.get().rect;
    let shelf = bar::create(
        gfx.clone(),
        theme.clone(),
        BarSpec { rect: shelf_rect, scale: monitor.scale, corners: Corners::Round, activate: false },
        Box::new(Shelf { hwnd: HWND::default(), frame: shared.clone(), shown: true, suspended: false }),
    )?;
    bar::refresh_now(shelf);
    dock.shelf = shelf;
    let spec = BarSpec {
        rect: dock.frame().expect("dock always has a frame"),
        scale: monitor.scale,
        corners: Corners::None,
        activate: false,
    };
    match bar::create(gfx.clone(), theme.clone(), spec, Box::new(dock)) {
        Ok(overlay) => Ok(vec![overlay, shelf]),
        Err(err) => {
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(shelf) };
            Err(err)
        }
    }
}

struct Drag {
    index: usize,
    /// Pointer position at mouse-down, screen pixels.
    origin: POINT,
    active: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum DropTarget {
    /// Insert before the n-th pinned item (pinned items other than the dragged one).
    Pin(usize),
    Remove,
    None,
}

/// Geometry of the current frame, in overlay client pixels unless noted.
#[derive(Default)]
struct Layout {
    /// Overlay window rectangle, screen pixels.
    frame: RECT,
    /// Each icon's slot (the hover plate), bottom-aligned on the shelf.
    slots: Vec<D2D_RECT_F>,
    /// Each slot's magnification factor.
    factors: Vec<f32>,
    separator: Option<f32>,
    /// Top of the shelf inside the overlay.
    shelf_top: f32,
    /// Slot centres at rest, screen x; magnification is computed from these.
    rest_centres: Vec<f32>,
    /// The shelf, screen pixels, and how far the tallest icon rises above it.
    shelf: RECT,
    grown: f32,
    rest_top: i32,
}

pub struct Dock {
    hwnd: HWND,
    shelf: HWND,
    shelf_frame: Rc<Cell<ShelfFrame>>,
    model: Rc<RefCell<WindowModel>>,
    /// Only windows on this monitor, when configured per monitor.
    filter: Option<HMONITOR>,
    monitor: Monitor,
    icon_size: f32,
    magnification: f32,
    auto_hide: AutoHide,
    entries: Vec<Entry>,
    pins_revision: u64,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
    /// Resamples icons to their resting size, so they draw 1:1 and stay sharp.
    wic: Option<IWICImagingFactory>,
    layout: Layout,

    /// Pointer in screen pixels while it is over the dock.
    pointer: Option<POINT>,
    hover: Option<usize>,
    drag: Option<Drag>,
    pending_preview: Option<usize>,
    /// Magnification and visibility, each animated from 0 to 1.
    zoom: f32,
    reveal: f32,
    animating: bool,
    overlapped: bool,
}

impl Dock {
    fn new(config: &DockConfig, model: Rc<RefCell<WindowModel>>, monitor: &Monitor, shelf_frame: Rc<Cell<ShelfFrame>>) -> Self {
        let mut dock = Self {
            hwnd: HWND::default(),
            shelf: HWND::default(),
            shelf_frame,
            model,
            filter: (config.windows == DockWindows::ThisMonitor).then_some(monitor.handle),
            monitor: *monitor,
            icon_size: config.icon_size,
            magnification: config.magnification.clamp(1.0, 3.0),
            auto_hide: config.auto_hide,
            entries: Vec::new(),
            pins_revision: 0,
            bitmaps: HashMap::new(),
            wic: unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok(),
            layout: Layout::default(),
            pointer: None,
            hover: None,
            drag: None,
            pending_preview: None,
            zoom: 0.0,
            reveal: if config.auto_hide == AutoHide::On { 0.0 } else { 1.0 },
            animating: false,
            overlapped: false,
        };
        dock.refresh();
        dock.overlapped = dock.check_overlap();
        dock.reveal = if dock.wants_hidden() { 0.0 } else { 1.0 };
        dock.relayout();
        dock
    }

    fn s(&self) -> f32 {
        self.monitor.scale
    }

    /// Slot edge at rest, in pixels.
    fn slot(&self) -> f32 {
        (self.icon_size + SLOT_INSET * 2.0) * self.s()
    }

    fn shelf_height(&self) -> f32 {
        PADDING * 2.0 * self.s() + self.slot()
    }

    /// Re-reads entries from the model. Returns whether they changed.
    fn refresh(&mut self) -> bool {
        let (entries, revision) = {
            let model = self.model.borrow();
            (model.entries(self.filter), model.pins_revision())
        };
        if entries == self.entries && revision == self.pins_revision && !self.layout.slots.is_empty() {
            return false;
        }
        self.entries = entries;
        self.pins_revision = revision;
        let model = self.model.borrow();
        let live: Vec<String> = self.entries.iter().filter_map(|e| model.entry_icon(e).map(|(k, _)| k)).collect();
        drop(model);
        self.bitmaps.retain(|key, _| live.contains(key) || key.strip_prefix(REST).is_some_and(|k| live.iter().any(|l| l == k)));
        if self.hover.is_some_and(|h| h >= self.entries.len()) {
            self.hover = None;
        }
        if self.drag.as_ref().is_some_and(|d| d.index >= self.entries.len()) {
            self.end_drag();
        }
        true
    }

    fn has_separator_before(&self, i: usize) -> bool {
        i > 0 && self.entries[i - 1].is_pinned() && !self.entries[i].is_pinned()
    }

    fn dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.active)
    }

    /// Magnification of each slot for the pointer position.
    fn factors(&self, rest_centres: &[f32]) -> Vec<f32> {
        let amount = (self.magnification - 1.0) * self.zoom;
        match self.pointer {
            Some(p) if amount > 0.0 && !self.dragging() => rest_centres
                .iter()
                .map(|&c| {
                    let d = (p.x as f32 - c).abs() / self.slot();
                    let falloff = if d < MAGNIFY_RANGE { ((PI * d / MAGNIFY_RANGE).cos() + 1.0) / 2.0 } else { 0.0 };
                    1.0 + amount * falloff
                })
                .collect(),
            _ => vec![1.0; rest_centres.len()],
        }
    }

    /// Recomputes every rectangle from entries, pointer, zoom and reveal.
    fn relayout(&mut self) {
        let s = self.s();
        let (pad, gap, sep) = (PADDING * s, GAP * s, SEPARATOR * s);
        let slot = self.slot();
        let m = self.monitor.rect;
        let centre = (m.left + m.right) as f32 / 2.0;

        // Rest positions (screen) decide magnification, so icons do not chase the pointer.
        let n = self.entries.len();
        let mut rest_width = pad * 2.0 + if n == 0 { slot } else { n as f32 * slot + (n - 1) as f32 * gap };
        if (0..n).any(|i| self.has_separator_before(i)) {
            rest_width += sep;
        }
        let mut x = centre - rest_width / 2.0 + pad;
        let mut rest_centres = Vec::with_capacity(n);
        for i in 0..n {
            if i > 0 {
                x += gap;
                if self.has_separator_before(i) {
                    x += sep;
                }
            }
            rest_centres.push(x + slot / 2.0);
            x += slot;
        }

        let factors = self.factors(&rest_centres);
        let content: f32 = factors.iter().map(|f| f * slot).sum::<f32>()
            + gap * n.saturating_sub(1) as f32
            + if (0..n).any(|i| self.has_separator_before(i)) { sep } else { 0.0 };
        let width = (pad * 2.0 + if n == 0 { slot } else { content }).round();

        // While the pointer is on the dock the overlay keeps one size, big
        // enough for any magnification and the name label: resizing a window
        // on every mouse move blanks its swap chain and makes it flicker.
        let grown = factors.iter().map(|f| (f - 1.0) * slot).fold(0.0, f32::max);
        let max_grow = (self.magnification - 1.0) * slot;
        let expanded = self.pointer.is_some() || self.zoom > 0.0 || self.drag.is_some();
        let headroom = if self.dragging() {
            slot * (REMOVE_DISTANCE + 1.0)
        } else if expanded {
            max_grow + LABEL * s
        } else {
            0.0
        }
        .ceil();
        let frame_width = if expanded { (rest_width + max_grow * (MAGNIFY_RANGE + 1.0)).max(width).ceil() } else { width };

        let shelf_height = self.shelf_height().round();
        let rest_bottom = m.bottom - (MARGIN_BOTTOM * s).round() as i32;
        let hide_shift = ((1.0 - self.reveal) * (shelf_height + MARGIN_BOTTOM * s)).round() as i32;
        let bottom = rest_bottom + hide_shift;
        let left = (centre - width / 2.0).round() as i32;
        let shelf = RECT { left, top: bottom - shelf_height as i32, right: left + width as i32, bottom };

        let hidden = self.reveal <= 0.0 && self.auto_hide != AutoHide::Off;
        self.layout.frame = if hidden {
            // Fully hidden: only a thin trigger strip along the screen edge remains.
            let rest_left = (centre - rest_width / 2.0).round() as i32;
            RECT { left: rest_left, top: m.bottom - TRIGGER_PX, right: rest_left + rest_width as i32, bottom: m.bottom }
        } else {
            let frame_left = (centre - frame_width / 2.0).round() as i32;
            RECT {
                left: frame_left,
                top: (shelf.top - headroom as i32).min(m.bottom - TRIGGER_PX),
                right: frame_left + frame_width as i32,
                // Down to the screen edge, margin included: a pointer resting
                // on the very bottom row must stay on the dock, or the dock
                // hides, re-triggers and shows again in a loop.
                bottom: m.bottom,
            }
        };
        self.layout.shelf = shelf;
        self.layout.grown = grown;
        self.layout.rest_top = rest_bottom - shelf_height as i32;
        self.shelf_frame.set(ShelfFrame { rect: shelf, visible: !hidden });

        // Slots inside the overlay, bottom-aligned on the shelf's inner edge.
        let origin_x = self.layout.frame.left as f32;
        let shelf_top = (shelf.top - self.layout.frame.top) as f32;
        let slot_bottom = shelf_top + pad + slot;
        let mut x = left as f32 - origin_x + pad;
        self.layout.slots.clear();
        self.layout.separator = None;
        for (i, factor) in factors.iter().enumerate().take(n) {
            if i > 0 {
                x += gap;
                if self.has_separator_before(i) {
                    self.layout.separator = Some(x + sep / 2.0 - gap / 2.0);
                    x += sep;
                }
            }
            let size = slot * factor;
            self.layout.slots.push(rect(x, slot_bottom - size, x + size, slot_bottom));
            x += size;
        }
        self.layout.factors = factors;
        self.layout.shelf_top = shelf_top;
        self.layout.rest_centres = rest_centres;
    }

    /// Moves the shelf to the frame computed by `relayout`.
    fn sync_shelf(&self) {
        if !self.shelf.is_invalid() {
            bar::refresh_now(self.shelf);
        }
    }

    fn cursor() -> POINT {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        p
    }

    /// The pointer for a mouse message. The real cursor is used because the
    /// overlay moves under it while magnifying; debug builds with
    /// `WDOCK_TEST_INPUT` set trust the message instead, for posted test input.
    fn test_input() -> bool {
        cfg!(debug_assertions) && std::env::var_os("WDOCK_TEST_INPUT").is_some()
    }

    fn leave(&mut self) -> bool {
        if self.dragging() {
            return false; // captured; the drop decides
        }
        self.pointer = None;
        self.hover = None;
        self.drag = None;
        self.pending_preview = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
        }
        popup::preview_leave();
        self.schedule_hide();
        self.relayout();
        self.sync_shelf();
        self.animate();
        true
    }

    fn pointer_at(&self, x: f32, y: f32) -> POINT {
        if Self::test_input() {
            return POINT { x: self.layout.frame.left + x as i32, y: self.layout.frame.top + y as i32 };
        }
        Self::cursor()
    }

    /// Pointer in overlay client pixels.
    fn local(&self, p: POINT) -> (f32, f32) {
        ((p.x - self.layout.frame.left) as f32, (p.y - self.layout.frame.top) as f32)
    }

    fn hit(&self, p: POINT) -> Option<usize> {
        let (x, y) = self.local(p);
        let bottom = self.layout.shelf_top + self.shelf_height();
        self.layout.slots.iter().position(|r| x >= r.left && x < r.right && y >= r.top.min(self.layout.shelf_top) && y < bottom)
    }

    /// Slot rectangle in screen pixels (popup anchors).
    fn slot_screen(&self, index: usize) -> RECT {
        let r = self.layout.slots[index];
        let (ox, oy) = (self.layout.frame.left, self.layout.frame.top);
        RECT { left: ox + r.left as i32, top: oy + r.top as i32, right: ox + r.right as i32, bottom: oy + r.bottom as i32 }
    }

    // ---- animation ----

    fn zoom_target(&self) -> f32 {
        if self.pointer.is_some() && !self.dragging() && self.magnification > 1.0 { 1.0 } else { 0.0 }
    }

    fn wants_hidden(&self) -> bool {
        let mode_hides = match self.auto_hide {
            AutoHide::Off => false,
            AutoHide::On => true,
            AutoHide::Smart => self.overlapped,
        };
        mode_hides && self.pointer.is_none() && self.drag.is_none() && !popup::menu_open() && !popup::preview_open()
    }

    fn reveal_target(&self) -> f32 {
        if self.wants_hidden() { 0.0 } else { 1.0 }
    }

    /// Starts the frame timer if zoom or reveal are not at their targets.
    fn animate(&mut self) {
        let settled = self.zoom == self.zoom_target() && self.reveal == self.reveal_target();
        if !settled && !self.animating && !self.hwnd.is_invalid() {
            self.animating = true;
            unsafe { SetTimer(Some(self.hwnd), TIMER_ANIMATE, FRAME_MS, None) };
        }
    }

    fn step_animation(&mut self) -> bool {
        let approach = |value: &mut f32, target: f32| {
            *value += (target - *value) * EASE;
            if (target - *value).abs() < 0.01 {
                *value = target;
            }
        };
        let (zoom, reveal) = (self.zoom_target(), self.reveal_target());
        approach(&mut self.zoom, zoom);
        // Reveal slides in faster than it slides out.
        if reveal > self.reveal {
            approach(&mut self.reveal, reveal);
            approach(&mut self.reveal, reveal);
        } else {
            approach(&mut self.reveal, reveal);
        }
        if self.zoom == zoom && self.reveal == reveal {
            self.animating = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIMATE);
            }
        }
        self.relayout();
        self.sync_shelf();
        true
    }

    // ---- auto-hide ----

    /// Smart auto-hide: a visible window on this monitor intersects the dock at rest.
    fn check_overlap(&self) -> bool {
        if self.auto_hide != AutoHide::Smart {
            return false;
        }
        let s = self.s();
        let m = self.monitor.rect;
        let bottom = m.bottom - (MARGIN_BOTTOM * s).round() as i32;
        let top = bottom - self.shelf_height().round() as i32;
        let half = self.layout.rest_centres.last().zip(self.layout.rest_centres.first()).map_or(self.slot(), |(l, f)| (l - f) / 2.0 + self.slot());
        let centre = (m.left + m.right) / 2;
        let dock = RECT { left: centre - half as i32, top, right: centre + half as i32, bottom };
        let model = self.model.borrow();
        let workspace = model.active_workspace(self.monitor.handle);
        model.workspace_windows(self.monitor.handle, workspace).into_iter().any(|hwnd| {
            if wininfo::is_minimized(hwnd) || !wininfo::is_visible(hwnd) {
                return false;
            }
            let r = wininfo::rect(hwnd);
            r.left < dock.right && r.right > dock.left && r.top < dock.bottom && r.bottom > dock.top
        })
    }

    fn schedule_hide(&self) {
        if self.auto_hide != AutoHide::Off {
            unsafe { SetTimer(Some(self.hwnd), TIMER_HIDE, HIDE_DELAY_MS, None) };
        }
    }

    // ---- previews, menus, drag and drop ----

    fn hover_changed(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
        }
        self.pending_preview = None;
        let Some(index) = self.hover.filter(|_| !popup::menu_open()) else {
            return;
        };
        if self.entries[index].windows.is_empty() {
            popup::hide_preview();
        } else if popup::preview_open() {
            self.show_preview(index);
        } else {
            self.pending_preview = Some(index);
            unsafe { SetTimer(Some(self.hwnd), TIMER_PREVIEW, PREVIEW_DELAY_MS, None) };
        }
    }

    fn show_preview(&self, index: usize) {
        let windows = self.entries[index].windows.clone();
        popup::show_preview(windows, self.slot_screen(index), self.s());
    }

    fn open_menu(&mut self, index: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
        }
        self.pending_preview = None;
        popup::hide_preview();
        let entry = self.entries[index].clone();
        let model = self.model.borrow();
        let mut items = vec![MenuItem::Header(model.entry_name(&entry).to_owned())];
        for &hwnd in entry.windows.iter().take(10) {
            let mut label = wininfo::title(hwnd);
            if label.is_empty() {
                label = model.entry_name(&entry).to_owned();
            }
            if model.is_hidden(hwnd)
                && let Some((_, ws)) = model.placement(hwnd) {
                    label = format!("{label}  ·  Workspace {}", ws + 1);
                }
            let target = self.model.clone();
            items.push(MenuItem::action(label, move || target.borrow_mut().focus(hwnd)));
        }
        if !entry.windows.is_empty() {
            items.push(MenuItem::Separator);
        }
        if let Some(launch_target) = model.entry_launch(&entry).map(str::to_owned) {
            let label = if entry.windows.is_empty() { "Open" } else { "New window" };
            items.push(MenuItem::action(label, move || launch::open(&launch_target)));
        }
        let target = self.model.clone();
        match entry.source {
            EntrySource::Pinned(i) => items.push(MenuItem::action("Remove from Dock", move || {
                if target.borrow_mut().unpin(i) {
                    crate::app::notify_model();
                }
            })),
            EntrySource::Running(_) => {
                let pinned = entry.clone();
                items.push(MenuItem::action("Keep in Dock", move || {
                    if target.borrow_mut().pin(&pinned, usize::MAX) {
                        crate::app::notify_model();
                    }
                }));
            }
        }
        if !entry.windows.is_empty() {
            items.push(MenuItem::Separator);
            let label = if entry.windows.len() == 1 { "Close window".to_owned() } else { format!("Close {} windows", entry.windows.len()) };
            let windows = entry.windows.clone();
            items.push(MenuItem::action(label, move || windows.iter().for_each(|&w| wininfo::close(w))));
        }
        drop(model);
        let slot = self.slot_screen(index);
        let shelf_top = self.layout.frame.top + self.layout.shelf_top as i32;
        popup::show_menu(items, POINT { x: (slot.left + slot.right) / 2, y: shelf_top.min(slot.top) }, self.s());
    }

    /// Where a drop at `p` would go.
    fn drop_target(&self, p: POINT) -> DropTarget {
        let Some(drag) = &self.drag else {
            return DropTarget::None;
        };
        let dragged_pinned = self.entries[drag.index].is_pinned();
        let shelf_top = self.layout.frame.top as f32 + self.layout.shelf_top;
        if (p.y as f32) < shelf_top - self.slot() * REMOVE_DISTANCE {
            return if dragged_pinned { DropTarget::Remove } else { DropTarget::None };
        }
        let pinned: Vec<usize> = (0..self.entries.len()).filter(|&i| self.entries[i].is_pinned() && i != drag.index).collect();
        let x = p.x as f32;
        if !dragged_pinned {
            // Running apps only pin when dropped over the pinned section.
            let pinned_end = pinned.last().map_or(f32::MIN, |&i| self.layout.rest_centres[i] + self.slot());
            if x > pinned_end {
                return DropTarget::None;
            }
        }
        DropTarget::Pin(pinned.iter().filter(|&&i| self.layout.rest_centres[i] < x).count())
    }

    fn end_drag(&mut self) {
        if self.drag.take().is_some_and(|d| d.active) {
            let _ = unsafe { ReleaseCapture() };
        }
    }

    fn finish_drag(&mut self, p: POINT) {
        let target = self.drop_target(p);
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        let entry = self.entries[drag.index].clone();
        self.end_drag();
        let mut model = self.model.borrow_mut();
        let changed = match (entry.source.clone(), target) {
            (EntrySource::Pinned(i), DropTarget::Remove) => model.unpin(i),
            (EntrySource::Pinned(i), DropTarget::Pin(at)) => model.move_pinned(i, at),
            (EntrySource::Running(_), DropTarget::Pin(at)) => model.pin(&entry, at),
            _ => false,
        };
        drop(model);
        if changed {
            crate::app::notify_model();
        }
    }

    fn paint_label(&self, cx: &PaintCx, index: usize) {
        let s = cx.scale;
        let name = self.model.borrow().entry_name(&self.entries[index]).to_owned();
        if name.is_empty() {
            return;
        }
        let Ok(format) = text::format(cx, text::UI_FONT, 12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER) else {
            return;
        };
        let (w, _) = text::measure(&cx.gfx.dwrite, &name, &format, f32::MAX);
        let slot = self.layout.slots[index];
        let centre = (slot.left + slot.right) / 2.0;
        let (pw, ph) = (w + 20.0 * s, 24.0 * s);
        let left = (centre - pw / 2.0).clamp(0.0, (cx.width - pw).max(0.0));
        let top = (slot.top - ph - 4.0 * s).max(0.0);
        let pill = rect(left, top, left + pw, top + ph);
        let fill = if cx.theme.dark { rgba(40, 40, 44, 0.94) } else { rgba(250, 250, 252, 0.96) };
        if let Ok(brush) = cx.gfx.brush(fill) {
            unsafe { cx.dc.FillRoundedRectangle(&rounded(pill, 7.0 * s), &brush) };
        }
        if let Ok(brush) = cx.gfx.brush(cx.theme.border) {
            unsafe { cx.dc.DrawRoundedRectangle(&rounded(pill, 7.0 * s), &brush, 1.0, None) };
        }
        text::draw(cx, &name, &format, pill, cx.theme.text);
    }

    /// Resting icon size in whole pixels.
    fn rest_px(&self) -> u32 {
        (self.icon_size * self.s()).round() as u32
    }

    /// The icon at full (magnified) resolution, or with `rest`, resampled to
    /// exactly `rest_px` for 1:1 drawing.
    fn icon_bitmap(&mut self, cx: &PaintCx, index: usize, rest: bool) -> Option<ID2D1Bitmap1> {
        let model = self.model.borrow();
        let (key, image) = model.entry_icon(&self.entries[index])?;
        let key = if rest { format!("{REST}{key}") } else { key };
        if let Some(bitmap) = self.bitmaps.get(&key) {
            return Some(bitmap.clone());
        }
        let image = match (&self.wic, rest) {
            (Some(wic), true) => icons::resize(wic, image, self.rest_px()).ok()?,
            _ => image.clone(),
        };
        match unsafe { cx.dc.CreateBitmapFromWicBitmap(&image, None) } {
            Ok(bitmap) => {
                self.bitmaps.insert(key, bitmap.clone());
                Some(bitmap)
            }
            Err(err) => {
                log::warn!("icon bitmap for {}: {err}", model.entry_name(&self.entries[index]));
                None
            }
        }
    }
}

impl View for Dock {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }

    fn frame(&self) -> Option<RECT> {
        Some(self.layout.frame)
    }

    fn model_changed(&mut self) -> bool {
        let entries_changed = self.refresh();
        let overlapped = self.check_overlap();
        let overlap_changed = std::mem::replace(&mut self.overlapped, overlapped) != overlapped;
        if entries_changed || overlap_changed {
            self.relayout();
            self.sync_shelf();
            self.animate();
        }
        // Entries carry the dots and the active state, so nothing else repaints.
        entries_changed
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        if self.reveal <= 0.0 && self.auto_hide != AutoHide::Off {
            return; // only the trigger strip is showing
        }
        let inset = SLOT_INSET * s;
        let shelf_bottom = self.layout.shelf_top + self.shelf_height();

        if let Some(x) = self.layout.separator
            && let Ok(brush) = cx.gfx.brush(cx.theme.border) {
                let x = x.round() + 0.5;
                let top = self.layout.shelf_top + (PADDING + 8.0) * s;
                let line = rect(x - 0.5, top, x + 0.5, shelf_bottom - (PADDING + 8.0) * s);
                unsafe { cx.dc.FillRectangle(&line, &brush) };
            }

        let dragged = self.drag.as_ref().filter(|d| d.active).map(|d| d.index);
        for index in 0..self.entries.len() {
            let slot = self.layout.slots[index];
            let factor = self.layout.factors[index];
            if self.hover == Some(index) && self.magnification <= 1.0 && dragged.is_none()
                && let Ok(brush) = cx.gfx.brush(cx.theme.hover) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(slot, 10.0 * s), &brush) };
                }
            let resting = factor < 1.005;
            if let Some(bitmap) = self.icon_bitmap(cx, index, resting) {
                let pad = inset * factor;
                let dest = if resting {
                    // Whole pixels at the bitmap's exact size: no resampling blur.
                    let size = self.rest_px() as f32;
                    let (left, top) = ((slot.left + pad).round(), (slot.bottom - pad - size).round());
                    rect(left, top, left + size, top + size)
                } else {
                    rect(slot.left + pad, slot.top + pad, slot.right - pad, slot.bottom - pad)
                };
                let opacity = if dragged == Some(index) { 0.3 } else { 1.0 };
                unsafe {
                    cx.dc.DrawBitmap(&bitmap, Some(&dest), opacity, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
            }

            let entry = &self.entries[index];
            if !entry.windows.is_empty() {
                let color = if entry.flashing {
                    rgba(255, 159, 10, 1.0)
                } else {
                    let mut c = cx.theme.text;
                    c.a = if entry.active { 0.95 } else { 0.55 };
                    c
                };
                let width = if entry.active { ACTIVE_DOT_WIDTH } else { DOT } * s;
                let centre = (slot.left + slot.right) / 2.0;
                let bottom = shelf_bottom - 2.0 * s;
                let dot = rect(centre - width / 2.0, bottom - DOT * s, centre + width / 2.0, bottom);
                if let Ok(brush) = cx.gfx.brush(color) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(dot, DOT * s / 2.0), &brush) };
                }
            }
        }

        if let Some(index) = dragged {
            let p = self.pointer.unwrap_or_else(Self::cursor);
            // Insertion marker between pinned icons.
            if let DropTarget::Pin(at) = self.drop_target(p) {
                let pinned: Vec<usize> = (0..self.entries.len()).filter(|&i| self.entries[i].is_pinned() && i != index).collect();
                let x = match (at.checked_sub(1).and_then(|i| pinned.get(i)), pinned.get(at)) {
                    (Some(&before), _) => self.layout.slots[before].right + GAP * s / 2.0,
                    (None, Some(&after)) => self.layout.slots[after].left - GAP * s / 2.0,
                    (None, None) => self.layout.slots[0].left,
                };
                let top = self.layout.shelf_top + PADDING * s;
                if let Ok(brush) = cx.gfx.brush(cx.theme.accent) {
                    let bar = rect(x - 1.5 * s, top, x + 1.5 * s, shelf_bottom - PADDING * s);
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(bar, 1.5 * s), &brush) };
                }
            }
            // The dragged icon follows the pointer, faded when it would be removed.
            if let Some(bitmap) = self.icon_bitmap(cx, index, false) {
                let (x, y) = self.local(p);
                let half = (self.slot() - inset * 2.0) / 2.0;
                let (x, y) = (x.clamp(half, cx.width - half), y.clamp(half, cx.height - half));
                let dest = rect(x - half, y - half, x + half, y + half);
                let opacity = if self.drop_target(p) == DropTarget::Remove { 0.45 } else { 0.9 };
                unsafe {
                    cx.dc.DrawBitmap(&bitmap, Some(&dest), opacity, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
            }
        } else if let Some(index) = self.hover.filter(|_| !popup::preview_open()) {
            self.paint_label(cx, index);
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let p = self.pointer_at(x, y);
        // Windows repeats WM_MOUSEMOVE when a window changes under a still pointer.
        if self.pointer == Some(p) {
            return false;
        }
        // The expanded overlay is mostly transparent; only the shelf and the
        // icons rising from it count as being on the dock.
        let shelf = self.layout.shelf;
        // Measured from where the shelf rests, so a dock still sliding in
        // (or hidden below the edge) counts too.
        let on_dock = p.x >= shelf.left
            && p.x < shelf.right
            && p.y as f32 >= self.layout.rest_top as f32 - self.layout.grown
            && p.y < shelf.bottom.max(self.layout.frame.bottom);
        if !on_dock && self.drag.is_none() {
            return if self.pointer.is_some() { self.leave() } else { false };
        }
        let f = self.layout.frame;
        if Self::test_input() && !self.dragging() && (p.x < f.left || p.x >= f.right || p.y < f.top || p.y >= f.bottom) {
            return self.leave();
        }
        self.pointer = Some(p);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
        let threshold = (DRAG_THRESHOLD * self.s()) as i32;
        if let Some(drag) = &mut self.drag
            && !drag.active && ((p.x - drag.origin.x).abs() > threshold || (p.y - drag.origin.y).abs() > threshold) {
                drag.active = true;
                unsafe { SetCapture(self.hwnd) };
                popup::hide_preview();
            }
        if self.dragging() {
            self.relayout();
            self.sync_shelf();
            self.animate();
            return true;
        }
        let hover = self.hit(p);
        if std::mem::replace(&mut self.hover, hover) != hover {
            self.hover_changed();
        }
        self.relayout();
        self.sync_shelf();
        self.animate();
        true
    }

    fn mouse_leave(&mut self) -> bool {
        // Posted test input: the real cursor is elsewhere, so the system's
        // leave notification is meaningless; a move outside the frame leaves.
        if Self::test_input() {
            return false;
        }
        self.leave()
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        let p = self.pointer_at(x, y);
        if button == Button::Left {
            self.drag = self.hit(p).map(|index| Drag { index, origin: p, active: false });
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
            }
            popup::hide_preview();
        }
        false
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        let p = self.pointer_at(x, y);
        match button {
            Button::Left if self.dragging() => {
                self.finish_drag(p);
                self.pointer = self.hit(p).map(|_| p);
                self.relayout();
                self.sync_shelf();
                self.animate();
                if self.pointer.is_none() {
                    self.schedule_hide();
                }
            }
            Button::Left => {
                let pressed = self.drag.take().map(|d| d.index);
                if let Some(index) = pressed.filter(|&i| self.hit(p) == Some(i)) {
                    let entry = self.entries[index].clone();
                    self.model.borrow_mut().activate_entry(&entry);
                }
            }
            Button::Right => {
                if let Some(index) = self.hit(p) {
                    self.open_menu(index);
                }
            }
        }
        true
    }

    fn timer(&mut self, id: usize) -> bool {
        match id {
            TIMER_ANIMATE => self.step_animation(),
            TIMER_PREVIEW => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
                }
                let wanted = self.pending_preview.take().filter(|&i| self.hover == Some(i) && !popup::menu_open());
                if let Some(index) = wanted {
                    self.show_preview(index);
                    return true; // the label gives way to the preview
                }
                false
            }
            TIMER_HIDE => {
                // Wait while a menu or preview of ours is still open.
                if popup::menu_open() || popup::preview_open() {
                    return false;
                }
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
                }
                self.animate();
                false
            }
            _ => false,
        }
    }
}
