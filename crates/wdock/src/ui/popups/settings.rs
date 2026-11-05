//! wdock's own settings: the options the setup offers, plus the top bar's
//! choices. Each click saves one key to `config.toml`; the shell then reloads
//! the config and rebuilds its bars, and this panel reopens with the new
//! values. Opened from the quick-settings panel and closes when it loses focus.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};

use crate::app;
use crate::model::config::{self, AutoHide, Backdrop, Config, DockHoverEffect, IslandStyle, MonitorScope, ThemeMode};
use crate::model::config_edit;
use crate::platform::{launch, sign_in};
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::text;
use crate::ui::popup;
use std::cell::Cell;

const WIDTH: f32 = 300.0;
const PADDING: f32 = 14.0;
const HEADER: f32 = 26.0;
const ROW: f32 = 32.0;
const GAP: f32 = 4.0;
thread_local! { static SETTINGS_SCROLL: Cell<f32> = const { Cell::new(0.0) }; }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    Theme,
    Backdrop,
    Font,
    WindowsFont,
    WindowsFontRestore,
    TopBars,
    AutoHide,
    DockHover,
    DockSize,
    DockHoverSize,
    Island,
    IslandStyle,
    NotchWidth,
    Equalizer,
    LyricsMotion,
    IslandFiles,
    ClipboardHistory,
    IslandOrder,
    SignIn,
    OpenFile,
    Reload,
}

const ROWS: [(Setting, &str); 21] = [
    (Setting::Theme, "Theme"),
    (Setting::Backdrop, "Backdrop"),
    (Setting::Font, "Font"),
    (Setting::WindowsFont, "Use font in Windows"),
    (Setting::WindowsFontRestore, "Windows default font"),
    (Setting::TopBars, "Top bar on"),
    (Setting::AutoHide, "Dock auto-hide"),
    (Setting::DockHover, "Dock hover"),
    (Setting::DockSize, "Icon size"),
    (Setting::DockHoverSize, "Hover size"),
    (Setting::Island, "Dynamic Island"),
    (Setting::IslandStyle, "Island style"),
    (Setting::NotchWidth, "Notch width"),
    (Setting::Equalizer, "Animated equalizer"),
    (Setting::LyricsMotion, "Lyrics background motion"),
    (Setting::IslandFiles, "Island file shelf"),
    (Setting::ClipboardHistory, "Clipboard history (RAM)"),
    (Setting::IslandOrder, "Order (M/F)"),
    (Setting::SignIn, "Start at sign-in"),
    (Setting::OpenFile, "Open config file"),
    (Setting::Reload, "Reload wdock"),
];

const WINDOWS_FONT_SCRIPT: &str = include_str!("../../../../../scripts/windows-font.ps1");

/// Folder `windows-font.ps1` installs missing fonts from (install.ps1 fills it).
pub fn font_dir() -> std::path::PathBuf {
    crate::paths::local_dir().join("fonts")
}

fn windows_font_arguments(config: &Config, restore: bool) -> String {
    if restore {
        return "-Restore".into();
    }
    let family = config.appearance.font.trim();
    let family = if family.is_empty() { "auto" } else { family };
    format!("-Family {} -FontDir {}", launch::powershell_quote(family),
        launch::powershell_quote(&font_dir().display().to_string()))
}

/// The next installed family in the settings cycle after `current`.
fn next_font(current: &str) -> &'static str {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory};
    let choices = text::FONT_CHOICES;
    let installed = |name: &str| {
        name == "auto"
            || unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) }
                .is_ok_and(|dwrite| text::font_installed(&dwrite, name))
    };
    let start = choices.iter().position(|c| c.eq_ignore_ascii_case(current.trim())).map_or(0, |i| i + 1);
    (0..choices.len()).map(|i| choices[(start + i) % choices.len()]).find(|c| installed(c)).unwrap_or("auto")
}

impl Setting {
    /// What the row shows for the current values.
    fn value(self, config: &Config, sign_in: bool) -> String {
        match self {
            Self::Theme => match config.appearance.theme {
                ThemeMode::Auto => "Auto",
                ThemeMode::Dark => "Dark",
                ThemeMode::Light => "Light",
            },
            Self::Backdrop => match config.appearance.backdrop {
                Backdrop::Acrylic => "Acrylic",
                Backdrop::Solid => "Solid",
            },
            Self::Font => return if config.appearance.font.trim().eq_ignore_ascii_case("auto") {
                "Auto".to_owned()
            } else {
                config.appearance.font.trim().to_owned()
            },
            Self::WindowsFont => "Apply (admin)",
            Self::WindowsFontRestore => "Restore (admin)",
            Self::TopBars => match config.topbar.monitors {
                MonitorScope::All => "All monitors",
                MonitorScope::Primary => "Primary only",
            },
            Self::AutoHide => match config.dock.auto_hide {
                AutoHide::Smart => "Smart",
                AutoHide::On => "Hidden",
                AutoHide::Off => "Shown",
            },
            Self::DockHover => match config.dock.hover_effect {
                DockHoverEffect::Magnify => "Magnify",
                DockHoverEffect::Glass => "Glass",
            },
            Self::DockSize => return format!("−  {:.0} px  +", config.dock.base_icon_size()),
            Self::DockHoverSize => return format!("−  {:.0} px  +", config.dock.hover_icon_size()),
            Self::Island => on_off(config.island.enabled),
            Self::IslandStyle => match config.island.style { IslandStyle::Island => "Island", IslandStyle::Notch => "Notch" },
            Self::NotchWidth => return format!("{:.0}", config.island.notch_width()),
            Self::Equalizer => on_off(config.island.animate_equalizer),
            Self::LyricsMotion => on_off(config.island.lyrics_background_motion),
            Self::IslandFiles => on_off(config.island.file_shelf),
            Self::ClipboardHistory => on_off(config.island.clipboard_history),
            Self::IslandOrder => return widget_order(&config.island.widget_order)
                .iter().map(|s| if *s == "media" { "M" } else { "F" })
                .collect::<Vec<_>>().join(" · "),
            Self::SignIn => on_off(sign_in),
            Self::OpenFile | Self::Reload => "",
        }.to_owned()
    }

    /// The change a click makes, given the current values.
    fn change(self, config: &Config, sign_in: bool, delta: i32) -> Change {
        let key = |section, key, value: &str| Change::Key(section, key, value.to_owned());
        match self {
            Self::Theme => {
                let next = match config.appearance.theme {
                    ThemeMode::Auto => "dark",
                    ThemeMode::Dark => "light",
                    ThemeMode::Light => "auto",
                };
                key("appearance", "theme", &format!("\"{next}\""))
            }
            Self::Backdrop => {
                let next = match config.appearance.backdrop {
                    Backdrop::Acrylic => "solid",
                    Backdrop::Solid => "acrylic",
                };
                key("appearance", "backdrop", &format!("\"{next}\""))
            }
            Self::Font => key("appearance", "font", &format!("\"{}\"", next_font(&config.appearance.font))),
            Self::TopBars => {
                let next = match config.topbar.monitors {
                    MonitorScope::All => "primary",
                    MonitorScope::Primary => "all",
                };
                key("topbar", "monitors", &format!("\"{next}\""))
            }
            Self::AutoHide => {
                let next = match config.dock.auto_hide {
                    AutoHide::Smart => "on",
                    AutoHide::On => "off",
                    AutoHide::Off => "smart",
                };
                key("dock", "auto_hide", &format!("\"{next}\""))
            }
            Self::DockHover => {
                let next = match config.dock.hover_effect {
                    DockHoverEffect::Magnify => "glass",
                    DockHoverEffect::Glass => "magnify",
                };
                key("dock", "hover_effect", &format!("\"{next}\""))
            }
            Self::DockSize => {
                let next = (config.dock.base_icon_size() + delta as f32 * 4.0).clamp(16.0, 96.0);
                key("dock", "icon_size", &format!("{next:.0}"))
            }
            Self::DockHoverSize => {
                let base = config.dock.base_icon_size();
                let next = (config.dock.hover_icon_size() + delta as f32 * 4.0).clamp(base, (base * 3.0).min(192.0));
                key("dock", "hover_size", &format!("{next:.0}"))
            }
            Self::Island => key("island", "enabled", &(!config.island.enabled).to_string()),
            Self::IslandStyle => key("island", "style", match config.island.style { IslandStyle::Island => "\"notch\"", IslandStyle::Notch => "\"island\"" }),
            Self::NotchWidth => key("island", "notch_width", &format!("{:.0}", config.island.notch_width())),
            Self::Equalizer => key("island", "animate_equalizer", &(!config.island.animate_equalizer).to_string()),
            Self::LyricsMotion => key("island", "lyrics_background_motion", &(!config.island.lyrics_background_motion).to_string()),
            Self::IslandFiles => key("island", "file_shelf", &(!config.island.file_shelf).to_string()),
            Self::ClipboardHistory => key("island", "clipboard_history", &(!config.island.clipboard_history).to_string()),
            Self::IslandOrder => {
                let current = widget_order(&config.island.widget_order);
                let i = ORDERS.iter().position(|o| *o == current).unwrap_or(0);
                let next = ORDERS[(i + 1) % ORDERS.len()];
                key("island", "widget_order", &format!("[\"{}\", \"{}\"]", next[0], next[1]))
            }
            Self::SignIn => Change::SignIn(!sign_in),
            Self::OpenFile => Change::OpenFile,
            Self::Reload => Change::Reload,
            Self::WindowsFont => Change::WindowsFont { restore: false },
            Self::WindowsFontRestore => Change::WindowsFont { restore: true },
        }
    }
}

const ORDERS: [[&str; 2]; 2] = [["media", "files"], ["files", "media"]];

fn widget_order(order: &[String]) -> [&str; 2] {
    if order.iter().find(|s| *s == "media" || *s == "files").is_some_and(|s| s == "files") {
        ORDERS[1]
    } else { ORDERS[0] }
}

fn on_off(on: bool) -> &'static str {
    if on { "On" } else { "Off" }
}

fn slider_width(x: f32, left: f32, right: f32) -> f32 {
    (160.0 + ((x - left) / (right - left)).clamp(0.0, 1.0) * 240.0).round()
}

enum Change {
    Key(&'static str, &'static str, String),
    SignIn(bool),
    OpenFile,
    Reload,
    WindowsFont { restore: bool },
}

pub struct SettingsView {
    hwnd: HWND,
    anchor: RECT,
    scale: f32,
    config: Config,
    sign_in: bool,
    rows: Vec<(Setting, D2D_RECT_F)>,
    hot: Option<usize>,
    back_hot: bool,
    height: f32,
    scroll: f32,
    dragging_width: bool,
}

impl SettingsView {
    pub fn new(anchor: RECT, scale: f32) -> Self {
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect};
        let full_height = PADDING * 2.0 + HEADER + ROWS.len() as f32 * (ROW + GAP);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let ok = unsafe {
            let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
            GetMonitorInfoW(monitor, &mut info).as_bool()
        };
        let available = if ok { (info.rcMonitor.bottom - anchor.bottom) as f32 / scale - 6.0 } else { full_height };
        let height = full_height.min(available.max(HEADER + PADDING * 2.0 + ROW * 2.0));
        let mut view = Self {
            hwnd: HWND::default(),
            anchor,
            scale,
            config: config::load(),
            sign_in: sign_in::enabled(),
            rows: Vec::new(),
            hot: None,
            back_hot: false,
            height,
            scroll: SETTINGS_SCROLL.get().min((full_height - height).max(0.0)),
            dragging_width: false,
        };
        view.layout();
        view
    }

    fn layout(&mut self) {
        let s = self.scale;
        let (left, right) = (PADDING * s, (WIDTH - PADDING) * s);
        let mut y = (PADDING + HEADER + GAP - self.scroll) * s;
        self.rows.clear();
        for (setting, _) in ROWS {
            self.rows.push((setting, rect(left, y, right, y + ROW * s)));
            y += (ROW + GAP) * s;
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if y < (PADDING + HEADER) * self.scale || y >= (self.height - PADDING) * self.scale { return None; }
        self.rows.iter().position(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
    }

    fn set_width_from_x(&mut self, x: f32) {
        let Some((_, row)) = self.rows.iter().find(|(setting, _)| *setting == Setting::NotchWidth) else { return; };
        let left = row.left + 104.0 * self.scale;
        let right = row.right - 48.0 * self.scale;
        self.config.island.notch_width = slider_width(x, left, right);
    }

    fn back_hit(&self, x: f32, y: f32) -> bool {
        x >= PADDING * self.scale && x < (PADDING + HEADER) * self.scale
            && y >= PADDING * self.scale && y < (PADDING + HEADER) * self.scale
    }
}

impl View for SettingsView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }

    /// Right-aligned under the status icons, like the quick-settings panel.
    fn frame(&self) -> Option<RECT> {
        let width = (WIDTH * self.scale).round() as i32;
        let left = self.anchor.right - width;
        let top = self.anchor.bottom + (6.0 * self.scale).round() as i32;
        Some(RECT { left, top, right: left + width, bottom: top + (self.height * self.scale).round() as i32 })
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let theme = cx.theme;
        if theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(theme.solid_fill)
        {
            unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(0.0, 0.0, cx.width, cx.height), DWM_CORNER_RADIUS * s), &brush) };
        }
        let format = |size: f32, weight, align| text::format_px(&cx.gfx.dwrite, text::UI_FONT, size * s, weight, align, false);
        let (Ok(title), Ok(normal), Ok(value_format)) = (
            format(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING),
        ) else {
            return;
        };

        let pad = PADDING * s;
        let back = rect(pad, PADDING * s, pad + HEADER * s, (PADDING + HEADER) * s);
        if self.back_hot && let Ok(brush) = cx.gfx.brush(theme.hover) {
            unsafe { cx.dc.FillRoundedRectangle(&rounded(back, 8.0 * s), &brush) };
        }
        if let Ok(icon) = text::format_px(&cx.gfx.dwrite, text::ICON_FONT, 13.0 * s, DWRITE_FONT_WEIGHT_NORMAL,
            windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_CENTER, false) {
            text::draw(cx, "\u{E72B}", &icon, back, theme.text);
        }
        text::draw(cx, "wdock settings", &title, rect(back.right + 8.0 * s, PADDING * s, cx.width - pad, (PADDING + HEADER) * s), theme.text);
        let full = PADDING * 2.0 + HEADER + ROWS.len() as f32 * (ROW + GAP);
        if full > self.height {
            text::draw(cx, "Scroll", &value_format, rect(pad, PADDING * s, cx.width - pad, (PADDING + HEADER) * s), theme.text_dim);
        }

        let clip = rect(0.0, (PADDING + HEADER) * s, cx.width, cx.height - PADDING * s);
        unsafe { cx.dc.PushAxisAlignedClip(&clip, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED); }
        for (i, &(setting, r)) in self.rows.iter().enumerate() {
            let hot = self.hot == Some(i);
            if let Ok(brush) = cx.gfx.brush(if hot { theme.hover } else { theme.control }) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 8.0 * s), &brush) };
            }
            let label = ROWS[i].1;
            let label_rect = rect(r.left + 12.0 * s, r.top, r.right - 12.0 * s, r.bottom);
            text::draw(cx, label, &normal, label_rect, theme.text);
            if setting == Setting::NotchWidth {
                let left = r.left + 104.0 * s;
                let right = r.right - 48.0 * s;
                let mid = (r.top + r.bottom) / 2.0;
                let x = left + (right - left) * ((self.config.island.notch_width() - 160.0) / 240.0);
                if let Ok(track) = cx.gfx.brush(theme.text_dim) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(left, mid - s, right, mid + s), s), &track) };
                }
                if let Ok(brush) = cx.gfx.brush(theme.accent) {
                    unsafe { cx.dc.FillEllipse(&windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: x, Y: mid }, radiusX: 5.0 * s, radiusY: 5.0 * s }, &brush) };
                }
            }
            let value = setting.value(&self.config, self.sign_in);
            let color = if value == "On" { theme.accent } else { theme.text_dim };
            text::draw(cx, &value, &value_format, label_rect, color);
        }
        unsafe { cx.dc.PopAxisAlignedClip(); }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if self.dragging_width { self.set_width_from_x(x); return true; }
        let back_hot = self.back_hit(x, y);
        let back_changed = std::mem::replace(&mut self.back_hot, back_hot) != back_hot;
        let hot = self.hit(x, y);
        (std::mem::replace(&mut self.hot, hot) != hot) || back_changed
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button == Button::Left && let Some(index) = self.hit(x, y)
            && self.rows[index].0 == Setting::NotchWidth
            && x >= self.rows[index].1.left + 94.0 * self.scale {
            self.dragging_width = true;
            unsafe { SetCapture(self.hwnd); }
            self.set_width_from_x(x);
            return true;
        }
        false
    }

    fn mouse_leave(&mut self) -> bool {
        let back = std::mem::replace(&mut self.back_hot, false);
        self.hot.take().is_some() || back
    }

    fn wheel(&mut self, _x: f32, _y: f32, delta: i16) -> bool {
        let full = PADDING * 2.0 + HEADER + ROWS.len() as f32 * (ROW + GAP);
        let max = (full - self.height).max(0.0);
        let next = (self.scroll + if delta > 0 { -(ROW + GAP) } else { ROW + GAP }).clamp(0.0, max);
        if (next - self.scroll).abs() < f32::EPSILON { return false; }
        self.scroll = next;
        SETTINGS_SCROLL.set(next);
        self.hot = None;
        self.layout();
        true
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        if self.dragging_width {
            self.dragging_width = false;
            unsafe { let _ = ReleaseCapture(); }
            self.set_width_from_x(x);
            if let Err(err) = config_edit::save_key("island", "notch_width", &format!("{:.0}", self.config.island.notch_width())) {
                log::error!("could not save island.notch_width: {err}");
                self.config = config::load();
            } else {
                app::reload_island_appearance();
            }
            return true;
        }
        if self.back_hit(x, y) {
            popup::toggle_quick_settings(self.anchor, self.scale);
            return false;
        }
        let Some(index) = self.hit(x, y) else {
            return false;
        };
        let setting = self.rows[index].0;
        if setting == Setting::NotchWidth { return false; }
        let row = self.rows[index].1;
        let delta = if matches!(setting, Setting::DockSize | Setting::DockHoverSize) {
            if x < row.right - 105.0 * self.scale { return false; }
            if x < row.right - 55.0 * self.scale { -1 } else { 1 }
        } else { 1 };
        match setting.change(&self.config, self.sign_in, delta) {
            Change::Key(section, key, value) => {
                if let Err(err) = config_edit::save_key(section, key, &value) {
                    log::error!("could not save {section}.{key}: {err}");
                    return false;
                }
                if matches!(setting, Setting::IslandOrder | Setting::IslandStyle) {
                    self.config = config::load();
                    app::reload_island_appearance();
                    true
                } else {
                    // The shell reloads the config and reopens this panel.
                    app::reload_config();
                    false
                }
            }
            Change::SignIn(on) => {
                sign_in::set(on);
                self.sign_in = on;
                true
            }
            Change::OpenFile => {
                launch::open(&config::path().display().to_string());
                bar::close(self.hwnd);
                false
            }
            Change::Reload => {
                app::reload();
                false
            }
            Change::WindowsFont { restore } => {
                launch::powershell_elevated(WINDOWS_FONT_SCRIPT, &windows_font_arguments(&self.config, restore));
                bar::close(self.hwnd);
                false
            }
        }
    }

    /// Closes when the user clicks elsewhere.
    fn deactivated(&mut self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_font_rows_pass_the_configured_family() {
        let mut config = Config::default();
        assert!(windows_font_arguments(&config, false).starts_with("-Family 'auto' -FontDir '"));
        config.appearance.font = "SF Pro".into();
        assert!(windows_font_arguments(&config, false).starts_with("-Family 'SF Pro' "));
        assert_eq!(windows_font_arguments(&config, true), "-Restore");
        assert!(matches!(Setting::WindowsFontRestore.change(&config, false, 1), Change::WindowsFont { restore: true }));
    }

    #[test]
    fn font_row_cycles_installed_families() {
        let mut config = Config::default();
        assert_eq!(Setting::Font.value(&config, false), "Auto");
        assert!(matches!(Setting::Font.change(&config, false, 1),
            Change::Key("appearance", "font", ref v) if v == "\"Segoe UI Variable Text\""));
        config.appearance.font = "Arial".into();
        assert_eq!(super::next_font("Arial"), "auto");
        assert_eq!(Setting::Font.value(&config, false), "Arial");
    }

    #[test]
    fn dock_settings_change_effect_and_sizes() {
        let config = Config::default();
        assert!(matches!(Setting::DockHover.change(&config, false, 1),
            Change::Key("dock", "hover_effect", value) if value == "\"glass\""));
        assert!(matches!(Setting::DockSize.change(&config, false, -1),
            Change::Key("dock", "icon_size", value) if value == "28"));
        assert!(matches!(Setting::DockHoverSize.change(&config, false, 1),
            Change::Key("dock", "hover_size", value) if value == "68"));
    }

    #[test]
    fn island_controls_toggle_widgets_and_cycle_order() {
        let config = Config::default();
        assert!(matches!(Setting::IslandFiles.change(&config, false, 1),
            Change::Key("island", "file_shelf", value) if value == "false"));
        assert!(matches!(Setting::IslandOrder.change(&config, false, 1),
            Change::Key("island", "widget_order", value) if value == "[\"files\", \"media\"]"));
    }

    #[test]
    fn notch_controls_switch_style_and_bound_slider() {
        let mut config = Config::default();
        assert!(matches!(Setting::IslandStyle.change(&config, false, 1),
            Change::Key("island", "style", value) if value == "\"notch\""));
        config.island.style = IslandStyle::Notch;
        assert!(matches!(Setting::IslandStyle.change(&config, false, 1),
            Change::Key("island", "style", value) if value == "\"island\""));
        assert_eq!(slider_width(0.0, 10.0, 110.0), 160.0);
        assert_eq!(slider_width(60.0, 10.0, 110.0), 280.0);
        assert_eq!(slider_width(200.0, 10.0, 110.0), 400.0);
    }
}
