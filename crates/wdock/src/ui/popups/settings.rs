//! wdock's own settings: the options the setup offers, plus the top bar's
//! choices. Each click saves one key to `config.toml`; the shell then reloads
//! the config and rebuilds its bars, and this panel reopens with the new
//! values. Opened from the quick-settings panel and closes when it loses focus.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
};

use crate::app;
use crate::model::config::{self, AutoHide, Backdrop, Config, MonitorScope, ThemeMode};
use crate::model::config_edit;
use crate::platform::{launch, sign_in};
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::text;

const WIDTH: f32 = 300.0;
const PADDING: f32 = 14.0;
const HEADER: f32 = 26.0;
const ROW: f32 = 32.0;
const GAP: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    Theme,
    Backdrop,
    TopBars,
    AutoHide,
    Island,
    Equalizer,
    SignIn,
    OpenFile,
}

const ROWS: [(Setting, &str); 8] = [
    (Setting::Theme, "Theme"),
    (Setting::Backdrop, "Backdrop"),
    (Setting::TopBars, "Top bar on"),
    (Setting::AutoHide, "Dock auto-hide"),
    (Setting::Island, "Dynamic Island"),
    (Setting::Equalizer, "Animated equalizer"),
    (Setting::SignIn, "Start at sign-in"),
    (Setting::OpenFile, "Open config file"),
];

impl Setting {
    /// What the row shows for the current values.
    fn value(self, config: &Config, sign_in: bool) -> &'static str {
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
            Self::TopBars => match config.topbar.monitors {
                MonitorScope::All => "All monitors",
                MonitorScope::Primary => "Primary only",
            },
            Self::AutoHide => match config.dock.auto_hide {
                AutoHide::Smart => "Smart",
                AutoHide::On => "Hidden",
                AutoHide::Off => "Shown",
            },
            Self::Island => on_off(config.island.enabled),
            Self::Equalizer => on_off(config.island.animate_equalizer),
            Self::SignIn => on_off(sign_in),
            Self::OpenFile => "",
        }
    }

    /// The change a click makes, given the current values.
    fn change(self, config: &Config, sign_in: bool) -> Change {
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
            Self::Island => key("island", "enabled", &(!config.island.enabled).to_string()),
            Self::Equalizer => key("island", "animate_equalizer", &(!config.island.animate_equalizer).to_string()),
            Self::SignIn => Change::SignIn(!sign_in),
            Self::OpenFile => Change::OpenFile,
        }
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "On" } else { "Off" }
}

enum Change {
    Key(&'static str, &'static str, String),
    SignIn(bool),
    OpenFile,
}

pub struct SettingsView {
    hwnd: HWND,
    anchor: RECT,
    scale: f32,
    config: Config,
    sign_in: bool,
    rows: Vec<(Setting, D2D_RECT_F)>,
    hot: Option<usize>,
    height: f32,
}

impl SettingsView {
    pub fn new(anchor: RECT, scale: f32) -> Self {
        let height = PADDING * 2.0 + HEADER + ROWS.len() as f32 * (ROW + GAP);
        let mut view = Self {
            hwnd: HWND::default(),
            anchor,
            scale,
            config: config::load(),
            sign_in: sign_in::enabled(),
            rows: Vec::new(),
            hot: None,
            height,
        };
        view.layout();
        view
    }

    fn layout(&mut self) {
        let s = self.scale;
        let (left, right) = (PADDING * s, (WIDTH - PADDING) * s);
        let mut y = (PADDING + HEADER + GAP) * s;
        self.rows.clear();
        for (setting, _) in ROWS {
            self.rows.push((setting, rect(left, y, right, y + ROW * s)));
            y += (ROW + GAP) * s;
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.rows.iter().position(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
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
        text::draw(cx, "wdock settings", &title, rect(pad, PADDING * s, cx.width - pad, (PADDING + HEADER) * s), theme.text);

        for (i, &(setting, r)) in self.rows.iter().enumerate() {
            let hot = self.hot == Some(i);
            if let Ok(brush) = cx.gfx.brush(if hot { theme.hover } else { theme.control }) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 8.0 * s), &brush) };
            }
            let label = ROWS[i].1;
            let label_rect = rect(r.left + 12.0 * s, r.top, r.right - 12.0 * s, r.bottom);
            text::draw(cx, label, &normal, label_rect, theme.text);
            let value = setting.value(&self.config, self.sign_in);
            let color = if value == "On" { theme.accent } else { theme.text_dim };
            text::draw(cx, value, &value_format, label_rect, color);
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hot = self.hit(x, y);
        std::mem::replace(&mut self.hot, hot) != hot
    }

    fn mouse_leave(&mut self) -> bool {
        self.hot.take().is_some()
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        let Some(index) = self.hit(x, y) else {
            return false;
        };
        let setting = self.rows[index].0;
        match setting.change(&self.config, self.sign_in) {
            Change::Key(section, key, value) => {
                if let Err(err) = config_edit::save_key(section, key, &value) {
                    log::error!("could not save {section}.{key}: {err}");
                    return false;
                }
                // The shell reloads the config and reopens this panel.
                app::reload_config();
                false
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
        }
    }

    /// Closes when the user clicks elsewhere.
    fn deactivated(&mut self) -> bool {
        true
    }
}
