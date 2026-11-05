//! Control-centre panel: network, volume and brightness sliders, battery, and
//! the session buttons. Sign out, restart and shut down ask for a second click.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::model::config::Backdrop;
use crate::platform::{launch, monitors};
use crate::platform::session::{self, SessionAction};
use crate::services::audio::{Output, OutputKind};
use crate::services::bluetooth::{Probe, Snapshot};
use crate::services::{self, NetState, SystemStatus};
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::glyphs;
use crate::ui::popup;
use crate::ui::popups::wifi_panel::{Outcome, WifiHit, WifiPanel};
use crate::ui::popups::bluetooth_panel::{BluetoothHit, BluetoothPanel};
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
const TIMER_PANEL_ANIMATE: usize = 2;
const PANEL_FRAME_MS: u32 = 15;
const PANEL_RADIUS: f32 = 16.0;
const CONFIRM_MS: u32 = 3000;

fn animate_panel(progress: f32, open: bool) -> (f32, bool) {
    let target = if open { 1.0 } else { 0.0 };
    let next = progress + (target - progress) * 0.28;
    if (target - next).abs() < 0.01 { (target, true) } else { (next, false) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel { Wifi, Bluetooth }

fn accordion(current: Option<Panel>, clicked: Panel) -> Option<Panel> {
    if current == Some(clicked) { None } else { Some(clicked) }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Network,
    Wifi(WifiHit),
    Bluetooth,
    BluetoothPanel(BluetoothHit),
    Mute,
    MicMute,
    Microphone,
    InputToggle,
    Input(usize),
    Camera,
    CameraSettings,
    Volume,
    OutputToggle,
    Output(usize),
    /// The slider for `status.displays[i]`.
    Brightness(usize),
    Battery,
    Session(SessionAction),
    Settings,
    /// wdock's own settings, next to Windows' Settings button.
    WdockSettings,
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
    monitor: Option<RECT>,
    scale: f32,
    status: SystemStatus,
    /// Hit targets and the two slider tracks, in window pixels.
    targets: Vec<(Hit, D2D_RECT_F)>,
    tracks: Vec<(Hit, D2D_RECT_F)>,
    height: f32,
    content_height: f32,
    scroll: f32,
    hot: Option<Hit>,
    drag: Option<Drag>,
    confirm: Option<SessionAction>,
    outputs: Vec<Output>,
    outputs_open: bool,
    inputs: Vec<Output>,
    inputs_open: bool,
    /// The Wi-Fi list; `Some` (and scanning) only while expanded.
    wifi: Option<WifiPanel>,
    expanded: Option<Panel>,
    wifi_progress: f32,
    wifi_clip: D2D_RECT_F,
    bluetooth_probe: Option<Probe>,
    bluetooth_status: Snapshot,
    bluetooth: Option<BluetoothPanel>,
    bluetooth_progress: f32,
    bluetooth_clip: D2D_RECT_F,
}

impl QuickSettingsView {
    pub fn new(anchor: RECT, scale: f32) -> Self {
        let x = anchor.left + (anchor.right - anchor.left) / 2;
        let y = anchor.top + (anchor.bottom - anchor.top) / 2;
        let monitors = monitors::enumerate();
        let monitor = monitors.iter().find(|m| x >= m.rect.left && x < m.rect.right && y >= m.rect.top && y < m.rect.bottom)
            .or_else(|| monitors.iter().find(|m| m.primary)).map(|m| m.rect);
        Self::with_monitor(anchor, scale, monitor)
    }

    fn with_monitor(anchor: RECT, scale: f32, monitor: Option<RECT>) -> Self {
        let mut view = Self {
            hwnd: HWND::default(),
            anchor,
            monitor,
            scale,
            status: services::status(),
            targets: Vec::new(),
            tracks: Vec::new(),
            height: 0.0,
            content_height: 0.0,
            scroll: 0.0,
            hot: None,
            drag: None,
            confirm: None,
            outputs: services::audio_outputs(),
            outputs_open: false,
            inputs: services::microphone_inputs(),
            inputs_open: false,
            wifi: None,
            expanded: None,
            wifi_progress: 0.0,
            wifi_clip: rect(0.0, 0.0, 0.0, 0.0),
            bluetooth_probe: None,
            bluetooth_status: Snapshot::default(),
            bluetooth: None,
            bluetooth_progress: 0.0,
            bluetooth_clip: rect(0.0, 0.0, 0.0, 0.0),
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
            let top = y - GAP * s / 2.0;
            let bottom = wifi.layout(left, right, top, s);
            let visible = (bottom - top) * self.wifi_progress;
            self.wifi_clip = rect(left, top, right, top + visible);
            y += (bottom - top + GAP * s / 2.0) * self.wifi_progress;
        }

        if self.bluetooth_status.available || self.bluetooth.is_some() {
            self.targets.push((Hit::Bluetooth, rect(left, y, right, y + TILE * s)));
            y += (TILE + GAP) * s;
        }
        if let Some(bluetooth) = &mut self.bluetooth {
            let top = y - GAP * s / 2.0;
            let bottom = bluetooth.layout(left, right, top, s);
            let visible = (bottom - top) * self.bluetooth_progress;
            self.bluetooth_clip = rect(left, top, right, top + visible);
            y += (bottom - top + GAP * s / 2.0) * self.bluetooth_progress;
        }

        let slider = |hit: Hit, y: &mut f32, targets: &mut Vec<_>, tracks: &mut Vec<_>| {
            *y += LABEL * s;
            let button = rect(left, *y + (SLIDER_ROW - ICON_BUTTON) / 2.0 * s, left + ICON_BUTTON * s, *y + (SLIDER_ROW + ICON_BUTTON) / 2.0 * s);
            if hit == Hit::Volume {
                targets.push((Hit::Mute, button));
                targets.push((Hit::OutputToggle, rect(left + 70.0 * s, *y - LABEL * s, right, *y)));
            }
            if hit == Hit::Microphone {
                targets.push((Hit::MicMute, button));
                targets.push((Hit::InputToggle, rect(left + 94.0 * s, *y - LABEL * s, right, *y)));
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
        if self.status.microphone.is_some() {
            slider(Hit::Microphone, &mut y, &mut self.targets, &mut self.tracks);
            if self.inputs_open {
                for i in 0..self.inputs.len() {
                    self.targets.push((Hit::Input(i), rect(left, y, right, y + OUTPUT_ROW * s)));
                    y += OUTPUT_ROW * s;
                }
                y += GAP * s;
            }
        }
        self.targets.push((Hit::Camera, rect(left, y, right - 38.0 * s, y + 34.0 * s)));
        self.targets.push((Hit::CameraSettings, rect(right - 34.0 * s, y, right, y + 34.0 * s)));
        y += 44.0 * s;
        for i in 0..self.status.displays.len() {
            slider(Hit::Brightness(i), &mut y, &mut self.targets, &mut self.tracks);
        }
        if self.status.battery.is_some() {
            self.targets.push((Hit::Battery, rect(left, y, right, y + 36.0 * s)));
            y += (36.0 + GAP) * s;
        }

        self.targets.push((Hit::WdockSettings, rect(left, y, right, y + 30.0 * s)));
        y += (30.0 + GAP) * s;

        y += 4.0 * s;
        let count = POWER_ROW.len() as f32;
        let spacing = (right - left - POWER_BUTTON * s * count) / (count - 1.0);
        for (i, (hit, _, _)) in POWER_ROW.iter().enumerate() {
            let x = left + i as f32 * (POWER_BUTTON * s + spacing);
            self.targets.push((*hit, rect(x, y, x + POWER_BUTTON * s, y + POWER_BUTTON * s)));
        }
        y += (POWER_BUTTON + INFO_LINE) * s;
        self.content_height = y + PADDING * s - 6.0 * s;
        self.height = self.monitor.map_or(self.content_height, |m| {
            let top = self.popup_top().max(m.top);
            self.content_height.min((m.bottom - top).max(1) as f32 - 6.0 * s).max(1.0)
        });
        self.scroll = self.scroll.min((self.content_height - self.height).max(0.0));
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if y < 0.0 || y >= self.height { return None; }
        let y = y + self.scroll;
        if self.expanded == Some(Panel::Wifi) && self.wifi_progress >= 1.0
            && let Some(hit) = self.wifi.as_ref().and_then(|w| w.hit(x, y)) {
            return Some(Hit::Wifi(hit));
        }
        if self.expanded == Some(Panel::Bluetooth) && self.bluetooth_progress >= 1.0
            && let Some(hit) = self.bluetooth.as_ref().and_then(|b| b.hit(x, y)) {
            return Some(Hit::BluetoothPanel(hit));
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
            Hit::Microphone => self.status.microphone.map_or(0.0, |v| v.level),
            Hit::Brightness(i) => self.status.displays.get(i).map_or(0.0, |d| f32::from(d.percent) / 100.0),
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
        let (slider, value) = (drag.slider, drag.value);
        match slider {
            Hit::Volume => services::set_volume(value),
            Hit::Microphone => services::set_microphone_level(value),
            Hit::Brightness(i) => self.write_brightness(i, value, false),
            _ => {}
        }
    }

    fn write_brightness(&self, index: usize, value: f32, last: bool) {
        if let Some(display) = self.status.displays.get(index) {
            services::set_brightness(display.id, (value * 100.0).round() as u8, last);
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

    fn popup_top(&self) -> i32 {
        let top = self.anchor.bottom + (6.0 * self.scale).round() as i32;
        self.monitor.map_or(top, |m| top.clamp(m.top, (m.bottom - (80.0 * self.scale) as i32).max(m.top)))
    }

    fn scroll_by(&mut self, delta: i16) -> bool {
        let next = (self.scroll + if delta > 0 { -36.0 } else { 36.0 } * self.scale).clamp(0.0, (self.content_height - self.height).max(0.0));
        let changed = std::mem::replace(&mut self.scroll, next) != next;
        if changed { self.hot = None; }
        changed
    }

    fn toggle_panel(&mut self, panel: Panel) {
        self.expanded = accordion(self.expanded, panel);
        if self.expanded == Some(Panel::Wifi) && self.wifi.is_none() {
            self.wifi = Some(WifiPanel::open(self.hwnd));
        }
        if self.expanded == Some(Panel::Bluetooth) {
            self.bluetooth = Some(BluetoothPanel::open(self.hwnd, self.bluetooth_status.clone()));
        } else if let Some(bluetooth) = &mut self.bluetooth {
            // Keep only the painted snapshot during collapse, never its watchers.
            bluetooth.suspend();
        }
        unsafe { SetTimer(Some(self.hwnd), TIMER_PANEL_ANIMATE, PANEL_FRAME_MS, None); }
        self.hot = None;
        self.layout();
    }
}

impl View for QuickSettingsView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.bluetooth_probe = Some(Probe::open(hwnd));
    }

    fn window_corner_radius(&self) -> Option<f32> { Some(PANEL_RADIUS) }

    /// Right-aligned under the status icons.
    fn frame(&self) -> Option<RECT> {
        let width = (WIDTH * self.scale).round() as i32;
        let left = self.anchor.right - width;
        let left = self.monitor.map_or(left, |m| left.clamp(m.left, (m.right - width).max(m.left)));
        let top = self.popup_top();
        Some(RECT { left, top, right: left + width, bottom: top + self.height.round() as i32 })
    }

    fn model_changed(&mut self) -> bool {
        let wifi_changed = self.wifi.as_mut().is_some_and(WifiPanel::refresh);
        let before_bluetooth = self.bluetooth_status.clone();
        if let Some(probe) = &self.bluetooth_probe {
            let snapshot = probe.snapshot();
            if snapshot.ready {
                self.bluetooth_status = snapshot;
                self.bluetooth_probe = None;
            }
        }
        let bluetooth_changed = self.bluetooth.as_mut().is_some_and(BluetoothPanel::refresh);
        if let Some(bluetooth) = &self.bluetooth {
            self.bluetooth_status = bluetooth.snapshot().clone();
        }
        let status = services::status();
        if status == self.status && !wifi_changed && !bluetooth_changed && self.bluetooth_status == before_bluetooth {
            return false;
        }
        if status.audio_devices != self.status.audio_devices {
            self.outputs = services::audio_outputs();
            self.inputs = services::microphone_inputs();
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
                unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(0.0, 0.0, cx.width, cx.height), PANEL_RADIUS * s), &brush) };
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
        for format in [&normal, &right_aligned] {
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            unsafe {
                let sign = cx.gfx.dwrite.CreateEllipsisTrimmingSign(format).ok();
                let _ = format.SetTrimming(&trimming, sign.as_ref());
            }
        }
        let fill = |r: D2D_RECT_F, radius: f32, color| {
            if let Ok(brush) = cx.gfx.brush(color) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, radius), &brush) };
            }
        };
        let plate_color = |hit: Hit| if self.hot == Some(hit) { theme.hover } else { theme.control };

        let mut transform = windows_numerics::Matrix3x2::identity();
        unsafe {
            cx.dc.GetTransform(&mut transform);
            cx.dc.PushAxisAlignedClip(&rect(0.0, 0.0, cx.width, cx.height), windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
            cx.dc.SetTransform(&(windows_numerics::Matrix3x2::translation(0.0, -self.scroll) * transform));
        }

        for &(hit, r) in &self.targets {
            match hit {
                Hit::Network => {
                    fill(r, 16.0 * s, plate_color(hit));
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
                    let chevron_rect = rect(r.right - 30.0 * s, r.top, r.right - 6.0 * s, r.bottom);
                    let mut down = theme.text_dim;
                    down.a *= 1.0 - self.wifi_progress;
                    let mut up = theme.text_dim;
                    up.a *= self.wifi_progress;
                    text::draw(cx, glyphs::CHEVRON_DOWN, &icon, chevron_rect, down);
                    text::draw(cx, glyphs::CHEVRON_UP, &icon, chevron_rect, up);
                }
                Hit::Bluetooth => {
                    fill(r, 16.0 * s, plate_color(hit));
                    let badge = rect(r.left + 10.0 * s, r.top + 10.0 * s, r.left + 42.0 * s, r.bottom - 10.0 * s);
                    let on = self.bluetooth_status.on;
                    fill(badge, 16.0 * s, if on { theme.accent } else { theme.control });
                    text::draw(cx, glyphs::BLUETOOTH, &icon, badge, if on { theme.on_accent } else { theme.text_dim });
                    let left = badge.right + 12.0 * s;
                    let mid = (r.top + r.bottom) / 2.0;
                    text::draw(cx, "Bluetooth", &bold, rect(left, r.top + 6.0 * s, r.right - 30.0 * s, mid), theme.text);
                    text::draw(cx, &self.bluetooth_status.subtitle(), &normal, rect(left, mid, r.right - 30.0 * s, r.bottom - 6.0 * s), theme.text_dim);
                    let chevron = rect(r.right - 30.0 * s, r.top, r.right - 6.0 * s, r.bottom);
                    let mut down = theme.text_dim;
                    down.a *= 1.0 - self.bluetooth_progress;
                    let mut up = theme.text_dim;
                    up.a *= self.bluetooth_progress;
                    text::draw(cx, glyphs::CHEVRON_DOWN, &icon, chevron, down);
                    text::draw(cx, glyphs::CHEVRON_UP, &icon, chevron, up);
                }
                Hit::Wifi(_) | Hit::BluetoothPanel(_) => {}
                Hit::Mute => {
                    let muted = self.status.volume.is_some_and(|v| v.muted);
                    fill(r, 15.0 * s, plate_color(hit));
                    let level = self.slider_value(Hit::Volume);
                    text::draw(cx, glyphs::volume(level, muted), &icon, r, theme.text);
                }
                Hit::MicMute => {
                    fill(r, 15.0 * s, plate_color(hit));
                    let muted = self.status.microphone.is_some_and(|v| v.muted);
                    text::draw(cx, if muted { glyphs::MIC_MUTED } else { glyphs::MIC }, &icon, r, if muted { theme.text_dim } else { theme.text });
                }
                Hit::Camera | Hit::CameraSettings => {
                    fill(r, 12.0 * s, plate_color(hit));
                    text::draw(cx, if hit == Hit::Camera { glyphs::CAMERA } else { glyphs::SETTINGS }, &icon, rect(r.left, r.top, r.left + 34.0 * s, r.bottom), theme.text);
                    if hit == Hit::Camera { text::draw(cx, "Camera preview", &normal, rect(r.left + 36.0 * s, r.top, r.right, r.bottom), theme.text); }
                }
                Hit::InputToggle | Hit::OutputToggle => {
                    if let Some(current) = (if hit == Hit::InputToggle { &self.inputs } else { &self.outputs }).iter().find(|o| o.default) {
                        let color = if self.hot == Some(hit) { theme.text } else { theme.text_dim };
                        text::draw(cx, &current.name, &right_aligned, rect(r.left, r.top, r.right - 22.0 * s, r.bottom), color);
                        let chevron = if if hit == Hit::InputToggle { self.inputs_open } else { self.outputs_open } { glyphs::CHEVRON_UP } else { glyphs::CHEVRON_DOWN };
                        text::draw(cx, chevron, &icon, rect(r.right - 20.0 * s, r.top, r.right, r.bottom), color);
                    }
                }
                Hit::Input(i) | Hit::Output(i) => {
                    let Some(output) = (if matches!(hit, Hit::Input(_)) { &self.inputs } else { &self.outputs }).get(i) else {
                        continue;
                    };
                    if self.hot == Some(hit) {
                        fill(r, 8.0 * s, theme.hover);
                    }
                    let glyph = if matches!(hit, Hit::Input(_)) { glyphs::MIC } else { match output.kind {
                        OutputKind::Headphones => glyphs::HEADPHONES,
                        OutputKind::Bluetooth => glyphs::BLUETOOTH,
                        OutputKind::Display => glyphs::DISPLAY,
                        OutputKind::Speakers | OutputKind::Other => glyphs::SPEAKER,
                    }};
                    text::draw(cx, glyph, &icon, rect(r.left, r.top, r.left + 30.0 * s, r.bottom), theme.text);
                    let name_color = if output.default { theme.accent } else { theme.text };
                    text::draw(cx, &output.name, &normal, rect(r.left + 36.0 * s, r.top, r.right - 26.0 * s, r.bottom), name_color);
                    if output.default {
                        text::draw(cx, glyphs::CHECK, &icon, rect(r.right - 26.0 * s, r.top, r.right, r.bottom), theme.accent);
                    }
                }
                Hit::Volume | Hit::Microphone | Hit::Brightness(_) => {
                    let label = match hit {
                        Hit::Brightness(i) => self.status.displays.get(i).map_or("Display", |d| d.name.as_str()),
                        Hit::Microphone => "Microphone",
                        _ => "Sound",
                    };
                    let label_rect = rect(r.left - (ICON_BUTTON + 10.0) * s, r.top - LABEL * s, r.right, r.top);
                    text::draw(cx, label, &bold, label_rect, theme.text);
                    if matches!(hit, Hit::Brightness(_)) {
                        let button = rect(label_rect.left, r.top + 1.0 * s, label_rect.left + ICON_BUTTON * s, r.bottom - 1.0 * s);
                        text::draw(cx, glyphs::BRIGHTNESS, &icon, button, theme.text);
                    }
                    let value = self.slider_value(hit);
                    let inset = 7.0 * s;
                    let mid = (r.top + r.bottom) / 2.0;
                    let track = rect(r.left + inset, mid - 5.0 * s, r.right - inset, mid + 5.0 * s);
                    fill(track, 5.0 * s, theme.control);
                    let knob_x = track.left + (track.right - track.left) * value;
                    if knob_x > track.left {
                        fill(rect(track.left, track.top, knob_x, track.bottom), 5.0 * s, theme.accent);
                    }
                    let knob = rect(knob_x - inset, mid - inset, knob_x + inset, mid + inset);
                    fill(knob, inset, theme.on_accent);
                    let percent = format!("{}%", (value * 100.0).round() as u32);
                    text::draw(cx, &percent, &right_aligned, rect(r.right, r.top, r.right + 40.0 * s, r.bottom), theme.text_dim);
                }
                Hit::Battery => {
                    fill(r, 14.0 * s, plate_color(hit));
                    if let Some(battery) = self.status.battery {
                        let state = match (battery.charging, battery.plugged) {
                            (true, _) => "Charging",
                            (false, true) => "Plugged in",
                            (false, false) => "On battery",
                        };
                        let pad = 12.0 * s;
                        let mid = (r.top + r.bottom) / 2.0;
                        let body = rect(r.left + pad, mid - 5.0 * s, r.left + pad + 19.0 * s, mid + 5.0 * s);
                        if let Ok(brush) = cx.gfx.brush(theme.text_dim) {
                            unsafe {
                                cx.dc.DrawRoundedRectangle(&rounded(body, 3.0 * s), &brush, s.max(1.0), None);
                                cx.dc.FillRoundedRectangle(&rounded(rect(body.right + s, mid - 2.0 * s, body.right + 2.5 * s, mid + 2.0 * s), s), &brush);
                            }
                        }
                        let inner = rect(body.left + 2.0 * s, body.top + 2.0 * s, body.right - 2.0 * s, body.bottom - 2.0 * s);
                        let level = rect(inner.left, inner.top, inner.left + (inner.right - inner.left) * f32::from(battery.percent) / 100.0, inner.bottom);
                        if battery.percent > 0 {
                            fill(level, 2.0 * s, crate::ui::battery::level_color(battery, theme.text));
                        }
                        crate::ui::battery::mark(cx, body, battery, &icon);
                        text::draw(cx, "Battery", &bold, rect(body.right + 9.0 * s, r.top, r.right - pad, r.bottom), theme.text);
                        let detail = format!("{}% · {state}", battery.percent);
                        text::draw(cx, &detail, &right_aligned, rect(r.left + pad, r.top, r.right - pad, r.bottom), theme.text_dim);
                    }
                }
                Hit::WdockSettings => {
                    fill(r, 12.0 * s, plate_color(hit));
                    text::draw(cx, glyphs::SETTINGS, &icon, rect(r.left + 8.0 * s, r.top, r.left + 34.0 * s, r.bottom), theme.text);
                    text::draw(cx, "wdock settings", &normal, rect(r.left + 34.0 * s, r.top, r.right - 8.0 * s, r.bottom), theme.text);
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
            unsafe { cx.dc.PushAxisAlignedClip(&self.wifi_clip, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED); }
            wifi.paint(cx, if let Some(Hit::Wifi(hit)) = self.hot { Some(hit) } else { None });
            unsafe { cx.dc.PopAxisAlignedClip(); }
        }
        if let Some(bluetooth) = &self.bluetooth {
            unsafe { cx.dc.PushAxisAlignedClip(&self.bluetooth_clip, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED); }
            bluetooth.paint(cx, if let Some(Hit::BluetoothPanel(hit)) = self.hot { Some(hit) } else { None });
            unsafe { cx.dc.PopAxisAlignedClip(); }
        }

        let info_top = self.targets.last().map_or(0.0, |(_, r)| r.bottom);
        let info = rect(0.0, info_top, cx.width, info_top + INFO_LINE * s);
        let color = if self.confirm.is_some() { theme.accent } else { theme.text_dim };
        text::draw(cx, self.info_text(), &small, info, color);
        unsafe { cx.dc.SetTransform(&transform); cx.dc.PopAxisAlignedClip(); }
        if self.content_height > self.height {
            let track = self.height - 16.0 * s;
            let thumb = (track * self.height / self.content_height).max(20.0 * s);
            let top = 8.0 * s + (track - thumb) * self.scroll / (self.content_height - self.height);
            fill(rect(cx.width - 6.0 * s, top, cx.width - 3.0 * s, top + thumb), 1.5 * s, theme.text_dim);
        }
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
        let Some(slider @ (Hit::Volume | Hit::Microphone | Hit::Brightness(_))) = self.hit(x, y) else {
            return false;
        };
        unsafe { SetCapture(self.hwnd) };
        self.drag = Some(Drag { slider, value: self.slider_value(slider) });
        self.drag_to(x);
        true
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if let Some(drag) = self.drag.take() {
            let _ = unsafe { ReleaseCapture() };
            if let Hit::Brightness(i) = drag.slider {
                self.write_brightness(i, drag.value, true);
            }
            return true;
        }
        if button != Button::Left {
            return false;
        }
        match self.hit(x, y) {
            Some(Hit::Network) => {
                self.toggle_panel(Panel::Wifi);
            }
            Some(Hit::Bluetooth) => {
                self.toggle_panel(Panel::Bluetooth);
            }
            Some(Hit::BluetoothPanel(hit)) => {
                let outcome = self.bluetooth.as_mut().map_or(Outcome::Nothing, |b| b.click(hit));
                self.layout();
                match outcome {
                    Outcome::Close => { self.close(); return true; }
                    Outcome::Repaint => {}
                    Outcome::Nothing => return false,
                }
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
            Some(Hit::Camera) => { crate::platform::camera::show(); self.close(); }
            Some(Hit::CameraSettings) => { launch::open("ms-settings:camera"); self.close(); }
            Some(Hit::MicMute) => services::set_microphone_muted(!self.status.microphone.is_some_and(|v| v.muted)),
            Some(Hit::InputToggle) => {
                self.inputs = services::microphone_inputs();
                self.inputs_open = !self.inputs_open;
                self.outputs_open = false;
                self.layout();
            }
            Some(Hit::Input(i)) => {
                if let Some(input) = self.inputs.get(i).filter(|o| !o.default) { services::set_default_microphone(&input.id); }
                self.inputs_open = false;
                self.layout();
            }
            Some(Hit::OutputToggle) => {
                self.outputs = services::audio_outputs();
                self.outputs_open = !self.outputs_open;
                self.inputs_open = false;
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
            Some(Hit::WdockSettings) => popup::toggle_settings(self.anchor, self.scale),
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
        let content_y = y + self.scroll;
        if let Some(wifi) = self.wifi.as_mut().filter(|w| self.expanded == Some(Panel::Wifi) && self.wifi_progress >= 1.0 && w.contains(x, content_y)) {
            let changed = wifi.wheel(delta);
            if changed {
                self.layout();
            }
            if changed { return true; }
        }
        if let Some(bluetooth) = self.bluetooth.as_mut().filter(|b| self.expanded == Some(Panel::Bluetooth) && self.bluetooth_progress >= 1.0 && b.contains(x, content_y)) {
            let changed = bluetooth.wheel(delta);
            if changed { self.layout(); }
            if changed { return true; }
        }
        match self.hit(x, y) {
            Some(Hit::Volume | Hit::Mute) => services::nudge_volume(step),
            Some(Hit::Microphone | Hit::MicMute) => services::set_microphone_level((self.slider_value(Hit::Microphone) + step).clamp(0.0, 1.0)),
            Some(Hit::Brightness(i)) => {
                let current = self.slider_value(Hit::Brightness(i));
                self.write_brightness(i, (current + step).clamp(0.0, 1.0), true);
            }
            _ => return self.scroll_by(delta),
        }
        false
    }

    fn timer(&mut self, id: usize) -> bool {
        if id == TIMER_PANEL_ANIMATE {
            let (next, wifi_done) = animate_panel(self.wifi_progress, self.expanded == Some(Panel::Wifi));
            self.wifi_progress = next;
            let (next, bluetooth_done) = animate_panel(self.bluetooth_progress, self.expanded == Some(Panel::Bluetooth));
            self.bluetooth_progress = next;
            if wifi_done && self.expanded != Some(Panel::Wifi) { self.wifi = None; }
            if bluetooth_done && self.expanded != Some(Panel::Bluetooth) { self.bluetooth = None; }
            if wifi_done && bluetooth_done {
                unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_PANEL_ANIMATE); }
            }
            self.layout();
            return true;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_controls_do_not_overlap_at_fractional_dpi() {
        for scale in [1.0, 1.25, 1.5] {
            let mut view = QuickSettingsView::with_monitor(RECT::default(), scale, None);
            view.status.volume = Some(services::Volume { level: 0.5, muted: false });
            view.status.microphone = Some(services::Volume { level: 0.7, muted: true });
            view.inputs = vec![Output { id: "mic".into(), name: "Microphone".into(), kind: OutputKind::Other, default: true }];
            view.inputs_open = true;
            view.layout();
            assert_eq!(view.slider_value(Hit::Microphone), 0.7);
            for (i, (_, a)) in view.targets.iter().enumerate() {
                assert!(a.left >= 0.0 && a.right <= WIDTH * scale && a.bottom <= view.height);
                for (_, b) in &view.targets[i + 1..] {
                    assert!(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top);
                }
            }
            assert!(view.targets.iter().any(|(hit, _)| *hit == Hit::MicMute));
            assert!(view.targets.iter().any(|(hit, _)| *hit == Hit::Camera));
        }
    }

    #[test]
    fn panel_animation_converges_and_can_reverse() {
        let (partial, done) = animate_panel(0.0, true);
        assert!(partial > 0.0 && partial < 1.0 && !done);
        let (reversed, _) = animate_panel(partial, false);
        assert!(reversed < partial);
        for open in [true, false] {
            let mut progress = if open { 0.0 } else { 1.0 };
            for _ in 0..100 {
                let (next, finished) = animate_panel(progress, open);
                progress = next;
                if finished { break; }
            }
            assert_eq!(progress, if open { 1.0 } else { 0.0 });
        }
    }

    #[test]
    fn accordion_switches_and_reverses_during_animation() {
        let mut expanded = accordion(None, Panel::Wifi);
        let (wifi, _) = animate_panel(0.0, expanded == Some(Panel::Wifi));
        expanded = accordion(expanded, Panel::Bluetooth);
        assert_eq!(expanded, Some(Panel::Bluetooth));
        let (closing, _) = animate_panel(wifi, expanded == Some(Panel::Wifi));
        let (opening, _) = animate_panel(0.0, expanded == Some(Panel::Bluetooth));
        assert!(closing < wifi && opening > 0.0);
        expanded = accordion(expanded, Panel::Wifi);
        assert_eq!(expanded, Some(Panel::Wifi));
        assert!(animate_panel(closing, true).0 > closing);
        assert_eq!(accordion(expanded, Panel::Wifi), None);
    }

    #[test]
    fn bluetooth_layout_and_clipped_hits_hold_at_fractional_dpi() {
        use crate::services::bluetooth::{Device, Kind};

        for scale in [1.0, 1.25, 1.5] {
            let mut view = QuickSettingsView::with_monitor(RECT::default(), scale, None);
            view.layout();
            assert!(!view.targets.iter().any(|(h, _)| *h == Hit::Bluetooth));
            view.bluetooth_status = Snapshot { ready: true, available: true, on: true, devices: (0..8).map(|i| Device {
                id: i.to_string(), name: "A very long Bluetooth device name".repeat(4), connected: i == 0,
                kind: Kind::Audio, battery: Some(75), container: None,
            }).collect() };
            view.bluetooth = Some(BluetoothPanel::fixture(view.bluetooth_status.clone()));
            view.expanded = Some(Panel::Bluetooth);
            for progress in [0.0, 0.28, 0.75, 1.0] {
                view.bluetooth_progress = progress;
                view.layout();
                for (i, (_, a)) in view.targets.iter().enumerate() {
                    assert!(a.left >= 0.0 && a.right <= WIDTH * scale && a.bottom <= view.height);
                    for (_, b) in &view.targets[i + 1..] {
                        assert!(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top);
                    }
                }
                let network = view.targets.iter().find(|(h, _)| *h == Hit::Network).map(|(_, r)| *r).expect("network row");
                let bluetooth = view.targets.iter().find(|(h, _)| *h == Hit::Bluetooth).map(|(_, r)| *r).expect("Bluetooth row");
                assert!((bluetooth.top - network.bottom - GAP * scale).abs() < 0.001);
                let x = view.bluetooth_clip.left + 10.0 * scale;
                let y = view.bluetooth_clip.top + 33.0 * scale;
                let expected = Some(Hit::BluetoothPanel(BluetoothHit::Row(0)));
                if progress == 1.0 { assert_eq!(view.hit(x, y), expected); }
                else { assert_ne!(view.hit(x, y), expected); }
                assert!(view.bluetooth_clip.bottom <= view.height);
            }
            view.expanded = None;
            assert!(!matches!(view.hit(30.0 * scale, view.bluetooth_clip.top + 33.0 * scale), Some(Hit::BluetoothPanel(_))));
        }
    }

    #[test]
    fn long_panel_can_scroll_to_footer_on_two_fractional_dpi_monitors() {
        use crate::services::bluetooth::{Device, Kind};

        for scale in [1.25, 1.5] {
            for monitor in [RECT { left: 0, top: 0, right: 1366, bottom: 768 }, RECT { left: -1920, top: -1080, right: 0, bottom: 0 }] {
                let anchor = RECT { left: monitor.right - 100, top: monitor.top, right: monitor.right - 10, bottom: monitor.top + 40 };
                let mut view = QuickSettingsView::with_monitor(anchor, scale, Some(monitor));
                view.status.volume = Some(services::Volume { level: 0.5, muted: false });
                view.status.microphone = view.status.volume;
                view.bluetooth_status = Snapshot { ready: true, available: true, on: true, devices: (0..8).map(|i| Device {
                    id: i.to_string(), name: "Headphones".into(), connected: i == 0, kind: Kind::Audio, battery: None, container: None,
                }).collect() };
                view.bluetooth = Some(BluetoothPanel::fixture(view.bluetooth_status.clone()));
                view.expanded = Some(Panel::Bluetooth);
                view.bluetooth_progress = 1.0;
                view.layout();
                let frame = view.frame().expect("popup frame");
                assert!(frame.left >= monitor.left && frame.right <= monitor.right);
                assert!(frame.top >= monitor.top && frame.bottom <= monitor.bottom);
                for _ in 0..100 { view.scroll_by(-120); }
                assert!((view.scroll - (view.content_height - view.height).max(0.0)).abs() < 0.001);
                let settings = view.targets.iter().find(|(h, _)| *h == Hit::Settings).map(|(_, r)| *r).expect("settings footer");
                let y = (settings.top + settings.bottom) / 2.0 - view.scroll;
                assert!(y > 0.0 && y < view.height);
                assert_eq!(view.hit((settings.left + settings.right) / 2.0, y), Some(Hit::Settings));
                assert_eq!(view.hit(20.0, view.height + 1.0), None);
                view.bluetooth = None;
                view.layout();
                assert!(view.scroll <= (view.content_height - view.height).max(0.0));
            }
        }
    }
}
