//! Control-centre panel: network, volume and brightness sliders, battery, and
//! the session buttons. Sign out, restart and shut down ask for a second click.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::model::config::Backdrop;
use crate::platform::launch;
use crate::platform::session::{self, SessionAction};
use crate::services::audio::{Output, OutputKind};
use crate::services::{self, NetState, SystemStatus};
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::glyphs;
use crate::ui::popups::wifi_panel::{Outcome, WifiHit, WifiPanel};
use crate::ui::text;

const WIDTH: f32 = 300.0;
const PADDING: f32 = 14.0;
const TILE: f32 = 52.0;
const LABEL: f32 = 22.0;
const SLIDER_ROW: f32 = 32.0;
const ICON_BUTTON: f32 = 30.0;
const POWER_BUTTON: f32 = 38.0;
const INFO_LINE: f32 = 22.0;
const OUTPUT_ROW: f32 = 30.0;
const GAP: f32 = 10.0;
const TIMER_CONFIRM: usize = 1;
const CONFIRM_MS: u32 = 3000;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Network,
    Wifi(WifiHit),
    Mute,
    Volume,
    OutputToggle,
    Output(usize),
    Brightness,
    Battery,
    Session(SessionAction),
    Settings,
}

const POWER_ROW: [(Hit, &str, &str); 6] = [
    (Hit::Session(SessionAction::Lock), glyphs::LOCK, "Lock"),
    (Hit::Session(SessionAction::Sleep), glyphs::SLEEP, "Sleep"),
    (Hit::Session(SessionAction::SignOut), glyphs::SIGN_OUT, "Sign out"),
    (Hit::Session(SessionAction::Restart), glyphs::RESTART, "Restart"),
    (Hit::Session(SessionAction::ShutDown), glyphs::POWER, "Shut down"),
    (Hit::Settings, glyphs::SETTINGS, "Settings"),
];

struct Drag {
    slider: Hit,
    value: f32,
}

pub struct QuickSettingsView {
    hwnd: HWND,
    anchor: RECT,
    scale: f32,
    status: SystemStatus,
    /// Hit targets and the two slider tracks, in window pixels.
    targets: Vec<(Hit, D2D_RECT_F)>,
    tracks: Vec<(Hit, D2D_RECT_F)>,
    height: f32,
    hot: Option<Hit>,
    drag: Option<Drag>,
    confirm: Option<SessionAction>,
    outputs: Vec<Output>,
    outputs_open: bool,
    /// The Wi-Fi list; `Some` (and scanning) only while expanded.
    wifi: Option<WifiPanel>,
}

impl QuickSettingsView {
    pub fn new(anchor: RECT, scale: f32) -> Self {
        let mut view = Self {
            hwnd: HWND::default(),
            anchor,
            scale,
            status: services::status(),
            targets: Vec::new(),
            tracks: Vec::new(),
            height: 0.0,
            hot: None,
            drag: None,
            confirm: None,
            outputs: services::audio_outputs(),
            outputs_open: false,
            wifi: None,
        };
        view.layout();
        view
    }

    fn layout(&mut self) {
        let s = self.scale;
        let (left, right) = (PADDING * s, (WIDTH - PADDING) * s);
        let mut y = PADDING * s;
        self.targets.clear();
        self.tracks.clear();

        self.targets.push((Hit::Network, rect(left, y, right, y + TILE * s)));
        y += (TILE + GAP) * s;
        if let Some(wifi) = &mut self.wifi {
            y = wifi.layout(left, right, y - GAP * s / 2.0, s) + GAP * s;
        }

        let slider = |hit: Hit, y: &mut f32, targets: &mut Vec<_>, tracks: &mut Vec<_>| {
            *y += LABEL * s;
            let button = rect(left, *y + (SLIDER_ROW - ICON_BUTTON) / 2.0 * s, left + ICON_BUTTON * s, *y + (SLIDER_ROW + ICON_BUTTON) / 2.0 * s);
            if hit == Hit::Volume {
                targets.push((Hit::Mute, button));
                targets.push((Hit::OutputToggle, rect(left + 70.0 * s, *y - LABEL * s, right, *y)));
            }
            let track = rect(button.right + 10.0 * s, *y, right - 40.0 * s, *y + SLIDER_ROW * s);
            targets.push((hit, track));
            tracks.push((hit, track));
            *y += (SLIDER_ROW + GAP) * s;
        };
        if self.status.volume.is_some() {
            slider(Hit::Volume, &mut y, &mut self.targets, &mut self.tracks);
            if self.outputs_open {
                for i in 0..self.outputs.len() {
                    self.targets.push((Hit::Output(i), rect(left, y, right, y + OUTPUT_ROW * s)));
                    y += OUTPUT_ROW * s;
                }
                y += GAP * s;
            }
        }
        if self.status.brightness.is_some() {
            slider(Hit::Brightness, &mut y, &mut self.targets, &mut self.tracks);
        }
        if self.status.battery.is_some() {
            self.targets.push((Hit::Battery, rect(left, y, right, y + 36.0 * s)));
            y += (36.0 + GAP) * s;
        }

        y += 4.0 * s;
        let count = POWER_ROW.len() as f32;
        let spacing = (right - left - POWER_BUTTON * s * count) / (count - 1.0);
        for (i, (hit, _, _)) in POWER_ROW.iter().enumerate() {
            let x = left + i as f32 * (POWER_BUTTON * s + spacing);
            self.targets.push((*hit, rect(x, y, x + POWER_BUTTON * s, y + POWER_BUTTON * s)));
        }
        y += (POWER_BUTTON + INFO_LINE) * s;
        self.height = y + PADDING * s - 6.0 * s;
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if let Some(hit) = self.wifi.as_ref().and_then(|w| w.hit(x, y)) {
            return Some(Hit::Wifi(hit));
        }
        self.targets
            .iter()
            .find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .map(|(hit, _)| *hit)
    }

    fn slider_value(&self, slider: Hit) -> f32 {
        if let Some(drag) = self.drag.as_ref().filter(|d| d.slider == slider) {
            return drag.value;
        }
        match slider {
            Hit::Volume => self.status.volume.map_or(0.0, |v| if v.muted { 0.0 } else { v.level }),
            Hit::Brightness => self.status.brightness.map_or(0.0, |b| f32::from(b) / 100.0),
            _ => 0.0,
        }
    }

    fn drag_to(&mut self, x: f32) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        let Some((_, track)) = self.tracks.iter().find(|(hit, _)| *hit == drag.slider) else {
            return;
        };
        let inset = 7.0 * self.scale;
        drag.value = ((x - track.left - inset) / (track.right - track.left - inset * 2.0)).clamp(0.0, 1.0);
        match drag.slider {
            Hit::Volume => services::set_volume(drag.value),
            Hit::Brightness => services::set_brightness((drag.value * 100.0).round() as u8),
            _ => {}
        }
    }

    fn info_text(&self) -> &'static str {
        match self.confirm {
            Some(SessionAction::SignOut) => "Click again to sign out",
            Some(SessionAction::Restart) => "Click again to restart",
            Some(SessionAction::ShutDown) => "Click again to shut down",
            _ => POWER_ROW.iter().find(|(hit, _, _)| Some(*hit) == self.hot).map_or("", |(_, _, label)| label),
        }
    }

    fn close(&self) {
        bar::close(self.hwnd);
    }
}

impl View for QuickSettingsView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }

    /// Right-aligned under the status icons.
    fn frame(&self) -> Option<RECT> {
        let width = (WIDTH * self.scale).round() as i32;
        let left = self.anchor.right - width;
        let top = self.anchor.bottom + (6.0 * self.scale).round() as i32;
        Some(RECT { left, top, right: left + width, bottom: top + self.height.round() as i32 })
    }

    fn model_changed(&mut self) -> bool {
        let wifi_changed = self.wifi.as_mut().is_some_and(WifiPanel::refresh);
        let status = services::status();
        if status == self.status && !wifi_changed {
            return false;
        }
        if status.audio_devices != self.status.audio_devices {
            self.outputs = services::audio_outputs();
        }
        self.status = status;
        self.layout();
        true
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let theme = cx.theme;
        if theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(theme.solid_fill) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(0.0, 0.0, cx.width, cx.height), DWM_CORNER_RADIUS * s), &brush) };
            }
        let format = |family, size: f32, weight, align| text::format_px(&cx.gfx.dwrite, family, size * s, weight, align, false);
        let (Ok(bold), Ok(normal), Ok(small), Ok(right_aligned), Ok(icon), Ok(big_icon)) = (
            format(text::UI_FONT, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 11.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            format(text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING),
            format(text::ICON_FONT, 14.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            format(text::ICON_FONT, 16.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) else {
            return;
        };
        let fill = |r: D2D_RECT_F, radius: f32, color| {
            if let Ok(brush) = cx.gfx.brush(color) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, radius), &brush) };
            }
        };
        let plate_color = |hit: Hit| if self.hot == Some(hit) { theme.hover } else { theme.control };

        for &(hit, r) in &self.targets {
            match hit {
                Hit::Network => {
                    fill(r, 10.0 * s, plate_color(hit));
                    let (glyph, title, detail) = match &self.status.network {
                        NetState::Wifi { ssid, signal } => (glyphs::WIFI, "Wi-Fi", format!("{ssid} · {signal}%")),
                        NetState::Wired => (glyphs::ETHERNET, "Ethernet", "Connected".to_owned()),
                        NetState::Offline => (glyphs::WIFI, "Network", "Not connected".to_owned()),
                    };
                    let badge = rect(r.left + 10.0 * s, r.top + 10.0 * s, r.left + 42.0 * s, r.bottom - 10.0 * s);
                    let online = self.status.network != NetState::Offline;
                    fill(badge, 16.0 * s, if online { theme.accent } else { theme.control });
                    text::draw(cx, glyph, &icon, badge, if online { theme.on_accent } else { theme.text_dim });
                    let text_left = badge.right + 12.0 * s;
                    let mid = (r.top + r.bottom) / 2.0;
                    text::draw(cx, title, &bold, rect(text_left, r.top + 6.0 * s, r.right - 10.0 * s, mid), theme.text);
                    text::draw(cx, &detail, &normal, rect(text_left, mid, r.right - 30.0 * s, r.bottom - 6.0 * s), theme.text_dim);
                    let chevron = if self.wifi.is_some() { "\u{E70E}" } else { "\u{E70D}" };
                    text::draw(cx, chevron, &icon, rect(r.right - 30.0 * s, r.top, r.right - 6.0 * s, r.bottom), theme.text_dim);
                }
                Hit::Wifi(_) => {}
                Hit::Mute => {
                    let muted = self.status.volume.is_some_and(|v| v.muted);
                    fill(r, 15.0 * s, plate_color(hit));
                    let level = self.slider_value(Hit::Volume);
                    text::draw(cx, glyphs::volume(level, muted), &icon, r, theme.text);
                }
                Hit::OutputToggle => {
                    if let Some(current) = self.outputs.iter().find(|o| o.default) {
                        let color = if self.hot == Some(hit) { theme.text } else { theme.text_dim };
                        text::draw(cx, &current.name, &right_aligned, rect(r.left, r.top, r.right - 22.0 * s, r.bottom), color);
                        let chevron = if self.outputs_open { glyphs::CHEVRON_UP } else { glyphs::CHEVRON_DOWN };
                        text::draw(cx, chevron, &icon, rect(r.right - 20.0 * s, r.top, r.right, r.bottom), color);
                    }
                }
                Hit::Output(i) => {
                    let Some(output) = self.outputs.get(i) else {
                        continue;
                    };
                    if self.hot == Some(hit) {
                        fill(r, 8.0 * s, theme.hover);
                    }
                    let glyph = match output.kind {
                        OutputKind::Headphones => glyphs::HEADPHONES,
                        OutputKind::Bluetooth => glyphs::BLUETOOTH,
                        OutputKind::Display => glyphs::DISPLAY,
                        OutputKind::Speakers | OutputKind::Other => glyphs::SPEAKER,
                    };
                    text::draw(cx, glyph, &icon, rect(r.left, r.top, r.left + 30.0 * s, r.bottom), theme.text);
                    let name_color = if output.default { theme.accent } else { theme.text };
                    text::draw(cx, &output.name, &normal, rect(r.left + 36.0 * s, r.top, r.right - 26.0 * s, r.bottom), name_color);
                    if output.default {
                        text::draw(cx, glyphs::CHECK, &icon, rect(r.right - 26.0 * s, r.top, r.right, r.bottom), theme.accent);
                    }
                }
                Hit::Volume | Hit::Brightness => {
                    let label = if hit == Hit::Volume { "Sound" } else { "Display" };
                    let label_rect = rect(r.left - (ICON_BUTTON + 10.0) * s, r.top - LABEL * s, r.right, r.top);
                    text::draw(cx, label, &bold, label_rect, theme.text);
                    if hit == Hit::Brightness {
                        let button = rect(label_rect.left, r.top + 1.0 * s, label_rect.left + ICON_BUTTON * s, r.bottom - 1.0 * s);
                        text::draw(cx, glyphs::BRIGHTNESS, &icon, button, theme.text);
                    }
                    let value = self.slider_value(hit);
                    let inset = 7.0 * s;
                    let mid = (r.top + r.bottom) / 2.0;
                    let track = rect(r.left + inset, mid - 2.0 * s, r.right - inset, mid + 2.0 * s);
                    fill(track, 2.0 * s, theme.control);
                    let knob_x = track.left + (track.right - track.left) * value;
                    fill(rect(track.left, track.top, knob_x, track.bottom), 2.0 * s, theme.accent);
                    let knob = rect(knob_x - inset, mid - inset, knob_x + inset, mid + inset);
                    fill(knob, inset, theme.on_accent);
                    let percent = format!("{}%", (value * 100.0).round() as u32);
                    text::draw(cx, &percent, &right_aligned, rect(r.right, r.top, r.right + 40.0 * s, r.bottom), theme.text_dim);
                }
                Hit::Battery => {
                    fill(r, 10.0 * s, plate_color(hit));
                    if let Some(battery) = self.status.battery {
                        let state = match (battery.charging, battery.plugged) {
                            (true, _) => "Charging",
                            (false, true) => "Plugged in",
                            (false, false) => "On battery",
                        };
                        let pad = 12.0 * s;
                        text::draw(cx, "Battery", &bold, rect(r.left + pad, r.top, r.right - pad, r.bottom), theme.text);
                        let detail = format!("{}% · {state}", battery.percent);
                        text::draw(cx, &detail, &right_aligned, rect(r.left + pad, r.top, r.right - pad, r.bottom), theme.text_dim);
                    }
                }
                Hit::Session(_) | Hit::Settings => {
                    let armed = matches!(hit, Hit::Session(a) if self.confirm == Some(a));
                    fill(r, (POWER_BUTTON / 2.0) * s, if armed { theme.accent } else { plate_color(hit) });
                    let glyph = POWER_ROW.iter().find(|(h, _, _)| *h == hit).map_or("", |(_, g, _)| g);
                    text::draw(cx, glyph, &big_icon, r, if armed { theme.on_accent } else { theme.text });
                }
            }
        }

        if let Some(wifi) = &self.wifi {
            wifi.paint(cx, if let Some(Hit::Wifi(hit)) = self.hot { Some(hit) } else { None });
        }

        let info_top = self.targets.last().map_or(0.0, |(_, r)| r.bottom);
        let info = rect(0.0, info_top, cx.width, info_top + INFO_LINE * s);
        let color = if self.confirm.is_some() { theme.accent } else { theme.text_dim };
        text::draw(cx, self.info_text(), &small, info, color);
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if self.drag.is_some() {
            self.drag_to(x);
            return true;
        }
        let hot = self.hit(x, y);
        std::mem::replace(&mut self.hot, hot) != hot
    }

    fn mouse_leave(&mut self) -> bool {
        self.drag.is_none() && self.hot.take().is_some()
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        let Some(slider @ (Hit::Volume | Hit::Brightness)) = self.hit(x, y) else {
            return false;
        };
        unsafe { SetCapture(self.hwnd) };
        self.drag = Some(Drag { slider, value: self.slider_value(slider) });
        self.drag_to(x);
        true
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if self.drag.take().is_some() {
            let _ = unsafe { ReleaseCapture() };
            return true;
        }
        if button != Button::Left {
            return false;
        }
        match self.hit(x, y) {
            Some(Hit::Network) => {
                self.wifi = match self.wifi.take() {
                    Some(_) => None,
                    None => Some(WifiPanel::open(self.hwnd)),
                };
                self.layout();
            }
            Some(Hit::Wifi(hit)) => {
                let outcome = self.wifi.as_mut().map_or(Outcome::Nothing, |w| w.click(hit));
                self.layout();
                match outcome {
                    Outcome::Close => {
                        self.close();
                        return true;
                    }
                    Outcome::Repaint => {}
                    Outcome::Nothing => return false,
                }
            }
            Some(Hit::OutputToggle) => {
                self.outputs = services::audio_outputs();
                self.outputs_open = !self.outputs_open;
                self.layout();
            }
            Some(Hit::Output(i)) => {
                if let Some(output) = self.outputs.get(i).filter(|o| !o.default) {
                    services::set_default_output(&output.id);
                }
                self.outputs_open = false;
                self.layout();
            }
            Some(Hit::Mute) => {
                services::set_muted(!self.status.volume.is_some_and(|v| v.muted));
            }
            Some(Hit::Battery) => {
                launch::open("ms-settings:batterysaver");
                self.close();
            }
            Some(Hit::Settings) => {
                launch::open("ms-settings:");
                self.close();
            }
            Some(Hit::Session(action)) => {
                if action.needs_confirmation() && self.confirm != Some(action) {
                    self.confirm = Some(action);
                    unsafe { SetTimer(Some(self.hwnd), TIMER_CONFIRM, CONFIRM_MS, None) };
                    return true;
                }
                self.close();
                session::perform(action);
            }
            _ => return false,
        }
        true
    }

    fn wheel(&mut self, x: f32, y: f32, delta: i16) -> bool {
        let step = if delta > 0 { 0.05 } else { -0.05 };
        if let Some(wifi) = self.wifi.as_mut().filter(|w| w.contains(x, y)) {
            let changed = wifi.wheel(delta);
            if changed {
                self.layout();
            }
            return changed;
        }
        match self.hit(x, y) {
            Some(Hit::Volume | Hit::Mute) => services::nudge_volume(step),
            Some(Hit::Brightness) => {
                let current = self.slider_value(Hit::Brightness);
                services::set_brightness(((current + step).clamp(0.0, 1.0) * 100.0).round() as u8);
            }
            _ => return false,
        }
        false
    }

    fn timer(&mut self, id: usize) -> bool {
        if id != TIMER_CONFIRM {
            return false;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CONFIRM);
        }
        self.confirm.take().is_some()
    }

    fn deactivated(&mut self) -> bool {
        self.drag.is_none()
    }
}
