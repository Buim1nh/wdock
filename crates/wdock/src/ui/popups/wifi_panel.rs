//! The expandable Wi-Fi list inside quick settings. It owns a `wifi::Session`
//! only while expanded; collapsing or closing the popover drops it, which
//! stops the notifications. Networks without a saved profile are handed to
//! Windows' own Wi-Fi settings: wdock never asks for a password.

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
};
use windows::Win32::Foundation::HWND;

use crate::platform::launch;
use crate::services::wifi::{self, AccessPoint, Session};
use crate::ui::bar::PaintCx;
use crate::ui::gfx::{rect, rounded};
use crate::ui::glyphs;
use crate::ui::text;

const HEADER: f32 = 32.0;
const ROW: f32 = 36.0;
const FOOTER: f32 = 30.0;
const MAX_ROWS: usize = 5;
const SETTINGS_PAGE: &str = "ms-settings:network-wifi";
const REFRESH: &str = "\u{E72C}";
const SSID_CHARS: usize = 26;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WifiHit {
    Toggle,
    Refresh,
    Disconnect,
    Settings,
    /// Index into the full list, not the visible window.
    Row(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    Nothing,
    Repaint,
    /// Close the popover (Windows' own UI took over).
    Close,
}

pub struct WifiPanel {
    session: Option<Session>,
    networks: Vec<AccessPoint>,
    radio: Option<bool>,
    scroll: usize,
    searching: bool,
    targets: Vec<(WifiHit, D2D_RECT_F)>,
    message_area: D2D_RECT_F,
    header: D2D_RECT_F,
}

fn shorten(ssid: &str) -> String {
    if ssid.chars().count() <= SSID_CHARS {
        return ssid.to_owned();
    }
    ssid.chars().take(SSID_CHARS - 1).chain(['…']).collect()
}

impl WifiPanel {
    /// Opens the WLAN session and scans once. Without Wi-Fi hardware the panel
    /// still opens and says so.
    pub fn open(notify: HWND) -> Self {
        let session = Session::open(notify);
        let mut panel = Self { session, networks: Vec::new(), radio: None, scroll: 0, searching: true, targets: Vec::new(), message_area: rect(0.0, 0.0, 0.0, 0.0), header: rect(0.0, 0.0, 0.0, 0.0) };
        panel.reload();
        panel
    }

    fn reload(&mut self) {
        if let Some(session) = &self.session {
            self.radio = session.radio_on();
            self.networks = if self.radio == Some(false) { Vec::new() } else { session.networks() };
        }
        self.scroll = self.scroll.min(self.networks.len().saturating_sub(MAX_ROWS));
    }

    /// A WLAN notification arrived. Returns whether the view changed.
    pub fn refresh(&mut self) -> bool {
        let (before_networks, before_radio) = (self.networks.clone(), self.radio);
        self.reload();
        let was_searching = std::mem::replace(&mut self.searching, false);
        self.networks != before_networks || self.radio != before_radio || was_searching
    }

    fn message(&self) -> Option<&'static str> {
        if self.session.is_none() {
            Some("Wi-Fi is not available on this PC")
        } else if self.radio == Some(false) {
            Some("Wi-Fi is off")
        } else if self.networks.is_empty() {
            Some(if self.searching { "Searching…" } else { "No networks found" })
        } else {
            None
        }
    }

    fn connected(&self) -> bool {
        self.networks.iter().any(|n| n.connected)
    }

    /// Lays the panel out from `top`; returns its bottom edge.
    pub fn layout(&mut self, left: f32, right: f32, top: f32, s: f32) -> f32 {
        self.targets.clear();
        let mut y = top;
        let button = 24.0 * s;
        self.header = rect(left + 12.0 * s, y, right, y + HEADER * s);
        if self.session.is_some() {
            let toggle = rect(right - 36.0 * s - 8.0 * s, y + (HEADER * s - 20.0 * s) / 2.0, right - 8.0 * s, y + (HEADER * s + 20.0 * s) / 2.0);
            let refresh = rect(toggle.left - 8.0 * s - button, y + (HEADER * s - button) / 2.0, toggle.left - 8.0 * s, y + (HEADER * s + button) / 2.0);
            self.targets.push((WifiHit::Toggle, toggle));
            self.targets.push((WifiHit::Refresh, refresh));
        }
        y += HEADER * s;
        if self.message().is_some() {
            self.message_area = rect(left, y, right, y + ROW * s);
            y += ROW * s;
        } else {
            let end = (self.scroll + MAX_ROWS).min(self.networks.len());
            for index in self.scroll..end {
                self.targets.push((WifiHit::Row(index), rect(left, y, right, y + ROW * s)));
                y += ROW * s;
            }
        }
        if self.session.is_some() {
            let half = (right - left) / 2.0;
            if self.connected() {
                self.targets.push((WifiHit::Disconnect, rect(left, y, left + half, y + FOOTER * s)));
            }
            self.targets.push((WifiHit::Settings, rect(left + half, y, right, y + FOOTER * s)));
            y += FOOTER * s;
        } else {
            self.targets.push((WifiHit::Settings, rect(left, y, right, y + FOOTER * s)));
            y += FOOTER * s;
        }
        y
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<WifiHit> {
        self.targets.iter().find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(hit, _)| *hit)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.hit(x, y).is_some()
    }

    pub fn wheel(&mut self, delta: i16) -> bool {
        let max = self.networks.len().saturating_sub(MAX_ROWS);
        let next = if delta > 0 { self.scroll.saturating_sub(1) } else { (self.scroll + 1).min(max) };
        std::mem::replace(&mut self.scroll, next) != next
    }

    pub fn click(&mut self, hit: WifiHit) -> Outcome {
        match hit {
            WifiHit::Refresh => {
                if let Some(session) = &self.session {
                    self.searching = true;
                    session.scan();
                }
                Outcome::Repaint
            }
            WifiHit::Toggle => {
                let on = self.radio.unwrap_or(true);
                if self.session.as_ref().is_some_and(|s| s.set_radio(!on)) {
                    self.radio = Some(!on);
                    self.reload();
                    Outcome::Repaint
                } else {
                    // Needs rights wdock does not have: let Windows do it.
                    launch::open(SETTINGS_PAGE);
                    Outcome::Close
                }
            }
            WifiHit::Disconnect => {
                if let Some(session) = &self.session {
                    session.disconnect();
                }
                Outcome::Nothing
            }
            WifiHit::Settings => {
                launch::open(SETTINGS_PAGE);
                Outcome::Close
            }
            WifiHit::Row(index) => {
                let Some(network) = self.networks.get(index) else {
                    return Outcome::Nothing;
                };
                if network.connected {
                    return Outcome::Nothing;
                }
                match (&network.profile, &self.session) {
                    (Some(profile), Some(session)) if session.connect(profile) => Outcome::Nothing,
                    _ => {
                        launch::open(SETTINGS_PAGE);
                        Outcome::Close
                    }
                }
            }
        }
    }

    pub fn paint(&self, cx: &PaintCx, hot: Option<WifiHit>) {
        let s = cx.scale;
        let theme = cx.theme;
        let format = |family, size: f32, weight, align| text::format_px(&cx.gfx.dwrite, family, size * s, weight, align, false);
        let (Ok(bold), Ok(normal), Ok(small), Ok(centred), Ok(icon)) = (
            format(text::UI_FONT, 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            format(text::ICON_FONT, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) else {
            return;
        };
        let fill = |r: D2D_RECT_F, radius: f32, color| {
            if let Ok(brush) = cx.gfx.brush(color) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, radius), &brush) };
            }
        };
        let plate = |hit: WifiHit| if hot == Some(hit) { theme.hover } else { theme.control };

        text::draw(cx, "Wi-Fi", &bold, self.header, theme.text);

        for &(hit, r) in &self.targets {
            match hit {
                WifiHit::Toggle => {
                    let on = self.radio.unwrap_or(false);
                    fill(r, 10.0 * s, if on { theme.accent } else { theme.control });
                    let size = r.bottom - r.top - 6.0 * s;
                    let x = if on { r.right - 3.0 * s - size } else { r.left + 3.0 * s };
                    fill(rect(x, r.top + 3.0 * s, x + size, r.bottom - 3.0 * s), size / 2.0, if on { theme.on_accent } else { theme.text_dim });
                }
                WifiHit::Refresh => {
                    fill(r, 12.0 * s, plate(hit));
                    text::draw(cx, REFRESH, &icon, r, theme.text);
                }
                WifiHit::Disconnect | WifiHit::Settings => {
                    fill(r, 8.0 * s, plate(hit));
                    let label = if hit == WifiHit::Disconnect { "Disconnect" } else { "Wi-Fi settings" };
                    text::draw(cx, label, &centred, r, theme.text);
                }
                WifiHit::Row(index) => {
                    let Some(network) = self.networks.get(index) else {
                        continue;
                    };
                    if hot == Some(hit) {
                        fill(r, 8.0 * s, theme.hover);
                    }
                    // Signal bars.
                    let bars = wifi::bars(network.signal);
                    let base = (r.top + r.bottom) / 2.0 + 7.0 * s;
                    for i in 0..4u8 {
                        let x = r.left + 12.0 * s + f32::from(i) * 4.5 * s;
                        let color = if i < bars { theme.text } else { theme.text_dim };
                        fill(rect(x, base - (4.0 + f32::from(i) * 3.0) * s, x + 3.0 * s, base), 1.0 * s, color);
                    }
                    let text_left = r.left + 40.0 * s;
                    let text_right = r.right - 32.0 * s;
                    let name = shorten(&network.ssid);
                    if network.connected {
                        let mid = (r.top + r.bottom) / 2.0;
                        text::draw(cx, &name, &bold, rect(text_left, r.top + 2.0 * s, text_right, mid + 2.0 * s), theme.text);
                        text::draw(cx, "Connected", &small, rect(text_left, mid, text_right, r.bottom - 2.0 * s), theme.accent);
                    } else {
                        text::draw(cx, &name, &normal, rect(text_left, r.top, text_right, r.bottom), theme.text);
                    }
                    if network.secured {
                        text::draw(cx, glyphs::LOCK, &icon, rect(r.right - 30.0 * s, r.top, r.right - 8.0 * s, r.bottom), theme.text_dim);
                    }
                }
            }
        }

        if let Some(message) = self.message() {
            text::draw(cx, message, &centred, self.message_area, theme.text_dim);
        }
    }
}
