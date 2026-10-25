//! Top bar: launcher button, workspace pills and the active app's name on the left; tray
//! icons, the overflow chevron, status icons (network, volume, battery) and
//! the clock on the right. Tray and status appear on the primary monitor's bar
//! only. Status icons open quick settings; the clock opens the calendar.

use std::cell::RefCell;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_FORMAT_FLAGS};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, IDWriteTextFormat,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::SetTimer;
use windows::core::PCWSTR;

use crate::model::config::{Backdrop, TopBarConfig, TrayConfig};
use crate::model::windows::WindowModel;
use crate::platform::tray_host::with_tray;
use crate::services::{self, NetState, SystemStatus};
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::glyphs;
use crate::ui::popup;
use crate::ui::text;
use crate::ui::tray_strip::{self, TrayStrip};

const TIMER_CLOCK: usize = 1;
const PADDING_X: f32 = 12.0;
const LAUNCHER_WIDTH: f32 = 36.0;
const TRAY_SLOT: f32 = 26.0;
const CHEVRON_WIDTH: f32 = 22.0;
const CLOCK_GAP: f32 = 10.0;
const STATUS_SLOT: f32 = 26.0;
const BATTERY_SLOT: f32 = 58.0;
const VOLUME_STEP: f32 = 0.02;
const GLYPH_MENU: &str = "\u{E700}";
const GLYPH_KEYBOARD: &str = "\u{E765}";
const SHORTCUTS_WIDTH: f32 = 32.0;
const GLYPH_CHEVRON: &str = "\u{E70D}";
const PILL_WIDTH: f32 = 22.0;
const PILL_GAP: f32 = 3.0;

struct Formats {
    scale: f32,
    icon: IDWriteTextFormat,
    small_icon: IDWriteTextFormat,
    title: IDWriteTextFormat,
    clock: IDWriteTextFormat,
    small_text: IDWriteTextFormat,
    small_number: IDWriteTextFormat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hot {
    Launcher,
    Shortcuts,
    Chevron,
    Status,
    Clock,
    Workspace(u8),
}

/// Status icon slots, laid out during paint.
#[derive(Default)]
struct StatusLayout {
    area: Option<D2D_RECT_F>,
    volume: Option<D2D_RECT_F>,
    clock: Option<D2D_RECT_F>,
}

pub struct TopBar {
    hwnd: HWND,
    /// Bar height in pixels, as last painted.
    height: f32,
    /// The shortcut card was open at the last paint (button highlight).
    shortcuts_shown: bool,
    scale: f32,
    model: Rc<RefCell<WindowModel>>,
    title: String,
    date_format: Vec<u16>,
    time_format: Vec<u16>,
    clock: String,
    formats: Option<Formats>,
    hot: Option<Hot>,
    /// Tray only on the primary monitor's bar.
    tray_config: Option<TrayConfig>,
    tray: TrayStrip,
    tray_revision: u64,
    chevron: Option<D2D_RECT_F>,
    status: SystemStatus,
    layout: StatusLayout,
    monitor: HMONITOR,
    /// Active workspace and per-workspace window counts on this monitor.
    workspaces: (u8, Vec<usize>),
    pills: Vec<D2D_RECT_F>,
}

impl TopBar {
    pub fn new(
        config: &TopBarConfig,
        tray: Option<TrayConfig>,
        model: Rc<RefCell<WindowModel>>,
        monitor: HMONITOR,
        scale: f32,
    ) -> Self {
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let mut bar = Self {
            hwnd: HWND::default(),
            height: 0.0,
            shortcuts_shown: false,
            scale,
            model,
            title: String::new(),
            date_format: wide(&config.date_format),
            time_format: wide(&config.time_format),
            clock: String::new(),
            formats: None,
            hot: None,
            tray_config: tray,
            tray: TrayStrip::default(),
            tray_revision: u64::MAX,
            chevron: None,
            status: services::status(),
            layout: StatusLayout::default(),
            monitor,
            workspaces: (0, Vec::new()),
            pills: Vec::new(),
        };
        bar.workspaces = bar.current_workspaces();
        bar.clock = bar.format_now();
        bar.title = bar.current_title();
        bar
    }

    fn current_workspaces(&self) -> (u8, Vec<usize>) {
        let model = self.model.borrow();
        if model.workspace_count() < 2 {
            return (0, Vec::new());
        }
        (model.active_workspace(self.monitor), model.occupancy(self.monitor))
    }

    fn current_title(&self) -> String {
        self.model.borrow().foreground_name().unwrap_or("Desktop").to_owned()
    }

    fn format_now(&self) -> String {
        let mut date = [0u16; 64];
        let mut time = [0u16; 32];
        let (date_len, time_len) = unsafe {
            (
                GetDateFormatEx(
                    PCWSTR::null(),
                    Default::default(),
                    None,
                    PCWSTR(self.date_format.as_ptr()),
                    Some(&mut date),
                    PCWSTR::null(),
                ),
                GetTimeFormatEx(PCWSTR::null(), TIME_FORMAT_FLAGS(0), None, PCWSTR(self.time_format.as_ptr()), Some(&mut time)),
            )
        };
        let part = |buf: &[u16], len: i32| String::from_utf16_lossy(&buf[..(len.max(1) - 1) as usize]);
        format!("{}   {}", part(&date, date_len), part(&time, time_len))
    }

    /// Wakes exactly at the next minute boundary: one timer tick per minute.
    fn schedule_clock(&self) {
        let now = unsafe { GetLocalTime() };
        let elapsed = u32::from(now.wSecond) * 1000 + u32::from(now.wMilliseconds);
        let delay = 60_000 - elapsed + 20;
        unsafe { SetTimer(Some(self.hwnd), TIMER_CLOCK, delay, None) };
    }

    /// The keyboard button that shows the shortcut card, right of the launcher.
    fn shortcuts_rect(&self, height: f32) -> D2D_RECT_F {
        let s = self.scale;
        let left = (PADDING_X / 2.0 + LAUNCHER_WIDTH + 2.0) * s;
        rect(left, 3.0 * s, left + SHORTCUTS_WIDTH * s, height - 3.0 * s)
    }

    fn launcher_rect(&self, height: f32) -> D2D_RECT_F {
        let s = self.scale;
        rect(PADDING_X / 2.0 * s, 3.0 * s, (PADDING_X / 2.0 + LAUNCHER_WIDTH) * s, height - 3.0 * s)
    }

    fn hot_at(&self, x: f32, y: f32) -> Option<Hot> {
        let inside = |r: D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if x < (PADDING_X / 2.0 + LAUNCHER_WIDTH) * self.scale {
            Some(Hot::Launcher)
        } else if inside(self.shortcuts_rect(self.height)) {
            Some(Hot::Shortcuts)
        } else if let Some(i) = self.pills.iter().position(|&r| inside(r)) {
            Some(Hot::Workspace(i as u8))
        } else if self.chevron.is_some_and(inside) {
            Some(Hot::Chevron)
        } else if self.layout.area.is_some_and(inside) {
            Some(Hot::Status)
        } else if self.layout.clock.is_some_and(inside) {
            Some(Hot::Clock)
        } else {
            None
        }
    }

    /// `r` in screen pixels, extended down to the bar's bottom edge (popup anchor).
    fn anchor(&self, r: D2D_RECT_F) -> RECT {
        let origin = bar::screen_rect(self.hwnd);
        RECT {
            left: origin.left + r.left as i32,
            top: origin.top + r.top as i32,
            right: origin.left + r.right as i32,
            bottom: origin.bottom,
        }
    }

    /// Network, volume and battery icons, right-to-left from `right`.
    /// Returns the left edge used.
    fn paint_status(&mut self, cx: &PaintCx, formats: &Formats, right: f32) -> f32 {
        let s = cx.scale;
        let (top, bottom) = (3.0 * s, cx.height - 3.0 * s);
        let theme = cx.theme;
        let mut x = right;
        let mut take = |width: f32| {
            let slot = rect(x - width * s, top, x, bottom);
            x -= width * s;
            slot
        };

        let battery = self.status.battery.map(|b| (b, take(BATTERY_SLOT)));
        let volume = self.status.volume.map(|v| (v, take(STATUS_SLOT)));
        let network = take(STATUS_SLOT);
        let area = rect(x, top, right, bottom);
        self.layout.area = Some(area);
        self.layout.volume = volume.map(|(_, r)| r);

        if self.hot == Some(Hot::Status)
            && let Ok(brush) = cx.gfx.brush(theme.hover) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(area, 6.0 * s), &brush) };
            }

        let (glyph, color) = match self.status.network {
            NetState::Wifi { .. } => (glyphs::WIFI, theme.text),
            NetState::Wired => (glyphs::ETHERNET, theme.text),
            NetState::Offline => (glyphs::WIFI, theme.text_dim),
        };
        text::draw(cx, glyph, &formats.icon, network, color);
        if let Some((v, slot)) = volume {
            text::draw(cx, glyphs::volume(v.level, v.muted), &formats.icon, slot, theme.text);
        }
        if let Some((b, slot)) = battery {
            // Percentage, then a drawn battery: body, cap and a level fill.
            let (bw, bh) = (22.0 * s, 11.0 * s);
            let mid = (slot.top + slot.bottom) / 2.0;
            let body = rect(slot.right - 4.0 * s - bw, mid - bh / 2.0, slot.right - 4.0 * s, mid + bh / 2.0);
            let percent = rect(slot.left, slot.top, body.left - 4.0 * s, slot.bottom);
            text::draw(cx, &format!("{}%", b.percent), &formats.small_text, percent, theme.text);
            if let Ok(brush) = cx.gfx.brush(theme.text_dim) {
                let outline = rect(body.left + 0.5, body.top + 0.5, body.right - 0.5, body.bottom - 0.5);
                unsafe {
                    cx.dc.DrawRoundedRectangle(&rounded(outline, 3.0 * s), &brush, s.max(1.0), None);
                    let cap = rect(body.right + 1.0 * s, mid - 2.5 * s, body.right + 2.5 * s, mid + 2.5 * s);
                    cx.dc.FillRoundedRectangle(&rounded(cap, 1.0 * s), &brush);
                }
            }
            let low = b.percent <= 20 && !b.plugged;
            let fill_color = if low { crate::ui::gfx::rgba(255, 69, 58, 1.0) } else { theme.text };
            let inner = rect(body.left + 2.0 * s, body.top + 2.0 * s, body.right - 2.0 * s, body.bottom - 2.0 * s);
            let level = rect(inner.left, inner.top, inner.left + (inner.right - inner.left) * f32::from(b.percent) / 100.0, inner.bottom);
            if let Ok(brush) = cx.gfx.brush(fill_color) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(level, 1.5 * s), &brush) };
            }
            if b.charging || b.plugged {
                text::draw(cx, glyphs::CHARGING, &formats.small_icon, body, theme.accent);
            }
        }
        x
    }

    fn ensure_formats(&mut self, cx: &PaintCx) {
        if self.formats.as_ref().is_some_and(|f| f.scale == cx.scale) {
            return;
        }
        let created = (|| {
            Ok::<_, windows::core::Error>(Formats {
                scale: cx.scale,
                icon: text::format(cx, text::ICON_FONT, 14.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                small_icon: text::format(cx, text::ICON_FONT, 10.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                title: text::format(cx, text::UI_FONT, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING)?,
                clock: text::format(cx, text::UI_FONT, 13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
                small_text: text::format(cx, text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
                small_number: text::format(cx, text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            })
        })();
        match created {
            Ok(formats) => self.formats = Some(formats),
            Err(err) => log::error!("text formats: {err}"),
        }
    }

    /// Lays out tray slots right-to-left from `right`; returns the left edge used.
    fn layout_tray(&mut self, right: f32, height: f32) -> f32 {
        let Some(config) = &self.tray_config else {
            return right;
        };
        let s = self.scale;
        let split = with_tray(|tray| {
            let (bar, extra) = tray.split(config.max_visible, &config.overflow);
            (tray.revision(), bar.iter().map(|i| i.id).collect::<Vec<_>>(), !extra.is_empty())
        });
        let Some((revision, ids, has_overflow)) = split else {
            return right;
        };
        self.tray_revision = revision;

        let (top, bottom) = (3.0 * s, height - 3.0 * s);
        let mut x = right;
        self.chevron = has_overflow.then(|| {
            let chevron = rect(x - CHEVRON_WIDTH * s, top, x, bottom);
            x -= (CHEVRON_WIDTH + 2.0) * s;
            chevron
        });
        let mut slots: Vec<(u64, D2D_RECT_F)> = ids
            .iter()
            .rev()
            .map(|&id| {
                let slot = rect(x - TRAY_SLOT * s, top, x, bottom);
                x -= TRAY_SLOT * s;
                (id, slot)
            })
            .collect();
        slots.reverse();
        self.tray.set_slots(slots);
        x
    }
}

impl View for TopBar {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.tray.attach(hwnd, self.scale);
        self.schedule_clock();
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let (w, h) = (cx.width, cx.height);
        self.height = h;

        if cx.theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(cx.theme.solid_fill) {
                unsafe { cx.dc.FillRectangle(&rect(0.0, 0.0, w, h), &brush) };
            }
        if let Ok(brush) = cx.gfx.brush(cx.theme.border) {
            unsafe { cx.dc.FillRectangle(&rect(0.0, h - s.max(1.0), w, h), &brush) };
        }

        self.ensure_formats(cx);
        let Some(formats) = self.formats.take() else {
            return;
        };
        let hover_plate = |r: D2D_RECT_F| {
            if let Ok(brush) = cx.gfx.brush(cx.theme.hover) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 6.0 * s), &brush) };
            }
        };

        let launcher = self.launcher_rect(h);
        if self.hot == Some(Hot::Launcher) {
            hover_plate(launcher);
        }
        text::draw(cx, GLYPH_MENU, &formats.icon, launcher, cx.theme.text);
        let shortcuts = self.shortcuts_rect(h);
        self.shortcuts_shown = popup::shortcuts_open();
        if self.hot == Some(Hot::Shortcuts) || self.shortcuts_shown {
            hover_plate(shortcuts);
        }
        text::draw(cx, GLYPH_KEYBOARD, &formats.icon, shortcuts, cx.theme.text);

        let clock_right = w - PADDING_X * s;
        let (clock_width, _) = text::measure(&cx.gfx.dwrite, &self.clock, &formats.clock, w);
        let clock_left = clock_right - clock_width;
        let clock_plate = rect(clock_left - 6.0 * s, 3.0 * s, clock_right + 6.0 * s, h - 3.0 * s);
        self.layout.clock = Some(clock_plate);
        if self.hot == Some(Hot::Clock) {
            hover_plate(clock_plate);
        }
        text::draw(cx, &self.clock, &formats.clock, rect(clock_left - 4.0, 0.0, clock_right, h), cx.theme.text);

        let status_left = if self.tray_config.is_some() {
            self.paint_status(cx, &formats, clock_plate.left - 4.0 * s)
        } else {
            clock_left
        };
        let tray_left = self.layout_tray(status_left - CLOCK_GAP * s, h);
        if let Some(chevron) = self.chevron {
            if self.hot == Some(Hot::Chevron) {
                hover_plate(chevron);
            }
            text::draw(cx, GLYPH_CHEVRON, &formats.small_icon, chevron, cx.theme.text);
        }
        if self.tray_config.is_some() {
            let strip = &mut self.tray;
            with_tray(|tray| strip.paint(cx, tray));
            self.tray.publish_rects();
        }

        // Workspace pills: the active one filled, occupied ones with a dot.
        let mut pill_x = shortcuts.right + 4.0 * s;
        self.pills.clear();
        for (i, &count) in self.workspaces.1.iter().enumerate() {
            let pill = rect(pill_x, 6.0 * s, pill_x + PILL_WIDTH * s, h - 6.0 * s);
            pill_x = pill.right + PILL_GAP * s;
            let active = i as u8 == self.workspaces.0;
            let fill = if active {
                Some(cx.theme.accent)
            } else if self.hot == Some(Hot::Workspace(i as u8)) {
                Some(cx.theme.hover)
            } else {
                None
            };
            if let Some(color) = fill
                && let Ok(brush) = cx.gfx.brush(color) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(pill, 5.0 * s), &brush) };
                }
            let color = if active {
                cx.theme.on_accent
            } else if count > 0 {
                cx.theme.text
            } else {
                cx.theme.text_dim
            };
            text::draw(cx, &(i + 1).to_string(), &formats.small_number, pill, color);
            self.pills.push(pill);
        }

        let title_left = if self.pills.is_empty() { shortcuts.right + 6.0 * s } else { pill_x + 8.0 * s };
        let title_right = (tray_left - 12.0 * s).max(title_left);
        text::draw(cx, &self.title, &formats.title, rect(title_left, 0.0, title_right, h), cx.theme.text);
        self.formats = Some(formats);
    }

    fn model_changed(&mut self) -> bool {
        let title = self.current_title();
        let title_changed = std::mem::replace(&mut self.title, title) != self.title;
        let workspaces = self.current_workspaces();
        let workspaces_changed = std::mem::replace(&mut self.workspaces, workspaces) != self.workspaces;
        let tray_changed = self.tray_config.is_some()
            && with_tray(|tray| tray.prune() || tray.revision() != self.tray_revision).unwrap_or(false);
        let status = services::status();
        let status_changed = self.tray_config.is_some() && std::mem::replace(&mut self.status, status) != self.status;
        let shortcuts_changed = popup::shortcuts_open() != self.shortcuts_shown;
        title_changed || workspaces_changed || tray_changed || status_changed || shortcuts_changed
    }

    fn wheel(&mut self, x: f32, y: f32, delta: i16) -> bool {
        let over_volume = self.layout.volume.is_some_and(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom);
        if over_volume {
            services::nudge_volume(if delta > 0 { VOLUME_STEP } else { -VOLUME_STEP });
        }
        false
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hot = self.hot_at(x, y);
        let hot_changed = std::mem::replace(&mut self.hot, hot) != hot;
        self.tray.mouse_move(x, y) | hot_changed
    }

    fn mouse_leave(&mut self) -> bool {
        self.tray.mouse_leave() | self.hot.take().is_some()
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        self.tray.mouse_down(x, y, button)
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if self.tray.mouse_up(x, y, button) {
            return true;
        }
        match (self.hot_at(x, y), button) {
            (Some(Hot::Launcher), Button::Left) => popup::toggle_launcher(),
            (Some(Hot::Shortcuts), Button::Left) => crate::app::toggle_shortcuts(),
            // The active pill opens the overview; another one switches to it.
            (Some(Hot::Workspace(ws)), Button::Left) if ws == self.workspaces.0 => popup::toggle_overview(),
            (Some(Hot::Workspace(ws)), Button::Left) => {
                let focus = self.model.borrow_mut().switch_workspace(self.monitor, ws);
                if let Some(hwnd) = focus {
                    crate::app::focus_later(hwnd);
                }
                crate::app::notify_model();
            }
            (Some(Hot::Chevron), Button::Left) => {
                if let Some(chevron) = self.chevron {
                    popup::toggle_overflow(self.anchor(chevron), self.scale);
                }
            }
            (Some(Hot::Status), Button::Left) => {
                if let Some(area) = self.layout.area {
                    popup::toggle_quick_settings(self.anchor(area), self.scale);
                }
            }
            (Some(Hot::Clock), Button::Left) => {
                if let Some(clock) = self.layout.clock {
                    popup::toggle_calendar(self.anchor(clock), self.scale);
                }
            }
            _ => {}
        }
        false
    }

    fn double_click(&mut self, x: f32, y: f32, button: Button) -> bool {
        self.tray.double_click(x, y, button)
    }

    fn timer(&mut self, id: usize) -> bool {
        match id {
            TIMER_CLOCK => {
                self.schedule_clock();
                let now = self.format_now();
                std::mem::replace(&mut self.clock, now) != self.clock
            }
            tray_strip::TIMER_TIP => self.tray.timer(id),
            _ => false,
        }
    }
}
