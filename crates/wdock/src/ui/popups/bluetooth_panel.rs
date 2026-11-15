//! The popup owns discovery only while expanded. Windows handles pairing.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER};

use crate::platform::launch;
use crate::services::bluetooth::{Kind, Session, Snapshot, SETTINGS_PAGE};
use crate::ui::{bar::PaintCx, gfx::{rect, rounded}, glyphs, text};
use super::wifi_panel::Outcome;

const HEADER: f32 = 32.0;
const ROW: f32 = 36.0;
const FOOTER: f32 = 30.0;
const MAX_ROWS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BluetoothHit { Toggle, Settings, Row(usize) }

pub struct BluetoothPanel {
    session: Option<Session>,
    snapshot: Snapshot,
    scroll: usize,
    targets: Vec<(BluetoothHit, D2D_RECT_F)>,
    header: D2D_RECT_F,
    message_area: D2D_RECT_F,
    bounds: D2D_RECT_F,
}

impl BluetoothPanel {
    pub fn open(notify: HWND, snapshot: Snapshot) -> Self {
        let mut panel = Self::preview(snapshot);
        panel.session = Some(Session::open(notify));
        panel
    }

    fn preview(snapshot: Snapshot) -> Self {
        Self { session: None, snapshot, scroll: 0, targets: Vec::new(), header: rect(0.0, 0.0, 0.0, 0.0), message_area: rect(0.0, 0.0, 0.0, 0.0), bounds: rect(0.0, 0.0, 0.0, 0.0) }
    }

    pub fn snapshot(&self) -> &Snapshot { &self.snapshot }

    pub fn suspend(&mut self) { self.session = None; }

    pub fn refresh(&mut self) -> bool {
        let Some(session) = &self.session else { return false; };
        let snapshot = session.snapshot();
        if !snapshot.ready || snapshot == self.snapshot { return false; }
        self.snapshot = snapshot;
        self.scroll = self.scroll.min(self.snapshot.devices.len().saturating_sub(MAX_ROWS));
        true
    }

    fn message(&self) -> Option<&'static str> {
        if !self.snapshot.ready { Some("Loading…") }
        else if !self.snapshot.available { Some("Bluetooth is not available on this PC") }
        else if !self.snapshot.on { Some("Bluetooth is off") }
        else if self.snapshot.devices.is_empty() { Some("No paired devices") }
        else { None }
    }

    pub fn layout(&mut self, left: f32, right: f32, top: f32, s: f32) -> f32 {
        self.targets.clear();
        self.header = rect(left + 12.0 * s, top, right - 50.0 * s, top + HEADER * s);
        if self.snapshot.available {
            self.targets.push((BluetoothHit::Toggle, rect(right - 44.0 * s, top + 6.0 * s, right - 8.0 * s, top + 26.0 * s)));
        }
        let mut y = top + HEADER * s;
        if self.message().is_some() {
            self.message_area = rect(left, y, right, y + ROW * s);
            y += ROW * s;
        } else {
            for index in self.scroll..(self.scroll + MAX_ROWS).min(self.snapshot.devices.len()) {
                self.targets.push((BluetoothHit::Row(index), rect(left, y, right, y + ROW * s)));
                y += ROW * s;
            }
        }
        y += 6.0 * s;
        self.targets.push((BluetoothHit::Settings, rect(left, y, right, y + FOOTER * s)));
        y += FOOTER * s;
        self.bounds = rect(left, top, right, y);
        y
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<BluetoothHit> {
        self.targets.iter().find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(hit, _)| *hit)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        let r = self.bounds;
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    }

    pub fn wheel(&mut self, delta: i16) -> bool {
        let max = self.snapshot.devices.len().saturating_sub(MAX_ROWS);
        let next = if delta > 0 { self.scroll.saturating_sub(1) } else { (self.scroll + 1).min(max) };
        std::mem::replace(&mut self.scroll, next) != next
    }

    pub fn click(&mut self, hit: BluetoothHit) -> Outcome {
        match hit {
            BluetoothHit::Toggle => {
                if let Some(session) = &self.session { session.set_radio(!self.snapshot.on); }
                Outcome::Nothing
            }
            BluetoothHit::Settings => { launch::open(SETTINGS_PAGE); Outcome::Close }
            BluetoothHit::Row(index) => {
                let Some(device) = self.snapshot.devices.get(index) else { return Outcome::Nothing; };
                if device.kind == Kind::Audio {
                    if let Some(session) = &self.session { session.activate(&device.id); }
                    Outcome::Nothing
                } else { launch::open(SETTINGS_PAGE); Outcome::Close }
            }
        }
    }

    pub fn paint(&self, cx: &PaintCx, hot: Option<BluetoothHit>) {
        let s = cx.scale;
        let theme = cx.theme;
        let format = |family, size: f32, weight, align| text::format_px(&cx.gfx.dwrite, family, size * s, weight, align, false);
        let (Ok(bold), Ok(normal), Ok(small), Ok(centred), Ok(icon)) = (
            format(text::UI_FONT, 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            format(text::ICON_FONT, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) else { return; };
        for format in [&bold, &normal] {
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            unsafe {
                let sign = cx.gfx.dwrite.CreateEllipsisTrimmingSign(format).ok();
                let _ = format.SetTrimming(&trimming, sign.as_ref());
            }
        }
        let fill = |r: D2D_RECT_F, radius, color| {
            if let Ok(brush) = cx.gfx.brush(color) { unsafe { cx.dc.FillRoundedRectangle(&rounded(r, radius), &brush); } }
        };
        text::draw(cx, "Bluetooth", &bold, self.header, theme.text);
        for &(hit, r) in &self.targets {
            match hit {
                BluetoothHit::Toggle => {
                    let on = self.snapshot.on;
                    fill(r, 10.0 * s, if on { theme.accent } else { theme.control });
                    let size = r.bottom - r.top - 6.0 * s;
                    let x = if on { r.right - 3.0 * s - size } else { r.left + 3.0 * s };
                    fill(rect(x, r.top + 3.0 * s, x + size, r.bottom - 3.0 * s), size / 2.0, if on { theme.on_accent } else { theme.text_dim });
                }
                BluetoothHit::Settings => {
                    fill(r, 12.0 * s, if hot == Some(hit) { theme.hover } else { theme.control });
                    text::draw(cx, "Bluetooth settings", &centred, r, theme.text);
                }
                BluetoothHit::Row(index) => {
                    let Some(device) = self.snapshot.devices.get(index) else { continue; };
                    if hot == Some(hit) { fill(r, 12.0 * s, theme.hover); }
                    let glyph = if device.kind == Kind::Audio { glyphs::HEADPHONES } else { glyphs::BLUETOOTH };
                    text::draw(cx, glyph, &icon, rect(r.left + 8.0 * s, r.top, r.left + 32.0 * s, r.bottom), theme.text);
                    let left = r.left + 40.0 * s;
                    let right = r.right - 8.0 * s;
                    let mid = (r.top + r.bottom) / 2.0;
                    text::draw(cx, &device.name, if device.connected { &bold } else { &normal }, rect(left, r.top + 2.0 * s, right, mid + 2.0 * s), theme.text);
                    let action = if device.kind == Kind::Audio { if device.connected { "Connected · Disconnect" } else { "Connect" } } else if device.connected { "Connected" } else { "Paired" };
                    let detail = device.battery.map_or_else(|| action.to_owned(), |p| format!("{action} · {p}%"));
                    text::draw(cx, &detail, &small, rect(left, mid, right, r.bottom - 2.0 * s), if device.connected { theme.accent } else { theme.text_dim });
                }
            }
        }
        if let Some(message) = self.message() { text::draw(cx, message, &centred, self.message_area, theme.text_dim); }
    }

    #[cfg(test)]
    pub(super) fn fixture(snapshot: Snapshot) -> Self { Self::preview(snapshot) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::bluetooth::Device;

    #[test]
    fn states_and_scrolling_do_not_need_hardware() {
        let mut panel = BluetoothPanel::fixture(Snapshot { ready: true, ..Default::default() });
        assert_eq!(panel.message(), Some("Bluetooth is not available on this PC"));
        panel.snapshot.available = true;
        assert_eq!(panel.message(), Some("Bluetooth is off"));
        panel.snapshot.on = true;
        assert_eq!(panel.message(), Some("No paired devices"));
        panel.snapshot.devices = (0..8).map(|i| Device { id: i.to_string(), name: "Long device name".repeat(8), connected: false, kind: Kind::Other, battery: None, container: None }).collect();
        for _ in 0..12 { panel.wheel(-120); }
        panel.layout(14.0, 286.0, 0.0, 1.0);
        assert_eq!(panel.scroll, 3);
        assert_eq!(panel.hit(20.0, 33.0), Some(BluetoothHit::Row(3)));
        assert_eq!(panel.targets.iter().filter(|(h, _)| matches!(h, BluetoothHit::Row(_))).count(), MAX_ROWS);
    }
}
