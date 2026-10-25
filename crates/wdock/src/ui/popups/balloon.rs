//! Notification card for tray balloons (NIF_INFO). Click activates the app
//! (NIN_BALLOONUSERCLICK); otherwise it times out (NIN_BALLOONTIMEOUT).

use std::collections::HashMap;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING,
};
use windows::Win32::UI::WindowsAndMessaging::SetTimer;

use crate::model::config::Backdrop;
use crate::model::tray::Balloon;
use crate::platform::tray_host::{self, TrayInput, with_tray};
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{Gfx, rect, rounded};
use crate::ui::popup;
use crate::ui::text;

const WIDTH: f32 = 340.0;
const PADDING: f32 = 12.0;
const ICON: f32 = 32.0;
const GAP: f32 = 12.0;
const TITLE_FONT: f32 = 13.0;
const BODY_FONT: f32 = 12.5;
const TIMER_DISMISS: usize = 1;
const SHOW_MS: u32 = 7000;

pub struct BalloonView {
    hwnd: HWND,
    balloon: Balloon,
    size: (i32, i32),
    title_height: f32,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
}

impl BalloonView {
    pub fn new(gfx: &Gfx, balloon: Balloon, scale: f32) -> Self {
        let text_width = (WIDTH - PADDING * 2.0 - ICON - GAP) * scale;
        let measure = |s: &str, size: f32, weight| {
            if s.is_empty() {
                return 0.0;
            }
            text::format_px(&gfx.dwrite, text::UI_FONT, size * scale, weight, DWRITE_TEXT_ALIGNMENT_LEADING, true)
                .map(|f| text::measure(&gfx.dwrite, s, &f, text_width).1)
                .unwrap_or(0.0)
        };
        let title_height = measure(&balloon.title, TITLE_FONT, DWRITE_FONT_WEIGHT_SEMI_BOLD);
        let body_height = measure(&balloon.text, BODY_FONT, DWRITE_FONT_WEIGHT_NORMAL).min(BODY_FONT * 1.4 * 5.0 * scale);
        let content = (title_height + 3.0 * scale + body_height).max(ICON * scale);
        let size = ((WIDTH * scale).round() as i32, (content + PADDING * 2.0 * scale).ceil() as i32);
        Self { hwnd: HWND::default(), balloon, size, title_height, bitmaps: HashMap::new() }
    }

    pub fn size_px(&self) -> (i32, i32) {
        self.size
    }

    fn close(&self) {
        popup::balloon_closed(self.hwnd);
        bar::close(self.hwnd);
    }
}

impl View for BalloonView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        unsafe { SetTimer(Some(hwnd), TIMER_DISMISS, SHOW_MS, None) };
        tray_host::send(self.balloon.icon_id, TrayInput::BalloonShow, Default::default());
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        if cx.theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(cx.theme.solid_fill) {
                let bounds = rect(0.0, 0.0, cx.width, cx.height);
                unsafe { cx.dc.FillRoundedRectangle(&rounded(bounds, DWM_CORNER_RADIUS * s), &brush) };
            }

        let (pad, icon) = (PADDING * s, ICON * s);
        let bitmaps = &mut self.bitmaps;
        with_tray(|tray| {
            let Some(tray_icon) = tray.icon(self.balloon.icon_id) else {
                return;
            };
            let Some(image) = &tray_icon.image else {
                return;
            };
            let key = tray_icon.cache_key();
            if !bitmaps.contains_key(&key)
                && let Ok(bitmap) = unsafe { cx.dc.CreateBitmapFromWicBitmap(image, None) } {
                    bitmaps.insert(key.clone(), bitmap);
                }
            if let Some(bitmap) = bitmaps.get(&key) {
                let dest = rect(pad, pad, pad + icon, pad + icon);
                unsafe { cx.dc.DrawBitmap(bitmap, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) };
            }
        });

        let left = pad + icon + GAP * s;
        let right = cx.width - pad;
        let fmt = |size: f32, weight| {
            text::format_px(&cx.gfx.dwrite, text::UI_FONT, size * s, weight, DWRITE_TEXT_ALIGNMENT_LEADING, true)
        };
        if let Ok(title) = fmt(TITLE_FONT, DWRITE_FONT_WEIGHT_SEMI_BOLD) {
            text::draw(cx, &self.balloon.title, &title, rect(left, pad, right, pad + self.title_height), cx.theme.text);
        }
        if let Ok(body) = fmt(BODY_FONT, DWRITE_FONT_WEIGHT_NORMAL) {
            let top = pad + self.title_height + if self.title_height > 0.0 { 3.0 * s } else { 0.0 };
            let mut color = cx.theme.text;
            color.a *= 0.8;
            text::draw(cx, &self.balloon.text, &body, rect(left, top, right, cx.height - pad), color);
        }
    }

    fn mouse_up(&mut self, _x: f32, _y: f32, button: Button) -> bool {
        if button == Button::Left {
            tray_host::send(self.balloon.icon_id, TrayInput::BalloonClick, Default::default());
        }
        self.close();
        false
    }

    fn timer(&mut self, id: usize) -> bool {
        if id == TIMER_DISMISS {
            tray_host::send(self.balloon.icon_id, TrayInput::BalloonTimeout, Default::default());
            self.close();
        }
        false
    }
}
