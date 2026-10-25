//! A row (or grid) of tray icons inside some bar or popup: draws them, tracks
//! hover, shows the tooltip, and forwards clicks to the owning apps exactly
//! like explorer's notification area does.

use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, ID2D1Bitmap1};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::model::tray::TrayModel;
use crate::platform::tray_host::{self, TrayInput, with_tray};
use crate::ui::bar::{self, Button, PaintCx};
use crate::ui::gfx::{rect, rounded};
use crate::ui::popup;

pub const TIMER_TIP: usize = 50;
const TIP_DELAY_MS: u32 = 450;
pub const ICON_DIP: f32 = 16.0;

#[derive(Default)]
pub struct TrayStrip {
    hwnd: HWND,
    scale: f32,
    /// Icon id and its slot, in window pixels.
    slots: Vec<(u64, D2D_RECT_F)>,
    hover: Option<u64>,
    /// What the hover produced: our tooltip, or NIN_POPUPOPEN to the app.
    tip_open: Option<u64>,
    pressed: Option<(u64, Button)>,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
}

impl TrayStrip {
    pub fn attach(&mut self, hwnd: HWND, scale: f32) {
        self.hwnd = hwnd;
        self.scale = scale;
    }

    pub fn set_slots(&mut self, slots: Vec<(u64, D2D_RECT_F)>) {
        if self.hover.is_some_and(|h| !slots.iter().any(|(id, _)| *id == h)) {
            self.close_tip();
            self.hover = None;
        }
        self.slots = slots;
    }

    pub fn paint(&mut self, cx: &PaintCx, tray: &TrayModel) {
        let size = ICON_DIP * cx.scale;
        let mut live = Vec::with_capacity(self.slots.len());
        for &(id, slot) in &self.slots {
            let Some(icon) = tray.icon(id) else {
                continue;
            };
            if self.hover == Some(id)
                && let Ok(brush) = cx.gfx.brush(cx.theme.hover) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(slot, 5.0 * cx.scale), &brush) };
                }
            let Some(image) = &icon.image else {
                continue;
            };
            let key = icon.cache_key();
            if !self.bitmaps.contains_key(&key)
                && let Ok(bitmap) = unsafe { cx.dc.CreateBitmapFromWicBitmap(image, None) } {
                    self.bitmaps.insert(key.clone(), bitmap);
                }
            if let Some(bitmap) = self.bitmaps.get(&key) {
                let cx_mid = ((slot.left + slot.right) / 2.0).round();
                let cy_mid = ((slot.top + slot.bottom) / 2.0).round();
                let dest = rect(cx_mid - size / 2.0, cy_mid - size / 2.0, cx_mid + size / 2.0, cy_mid + size / 2.0);
                unsafe {
                    cx.dc.DrawBitmap(bitmap, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
            }
            live.push(key);
        }
        self.bitmaps.retain(|key, _| live.contains(key));
    }

    /// Records where icons were drawn, for Shell_NotifyIconGetRect.
    pub fn publish_rects(&self) {
        let origin = bar::screen_rect(self.hwnd);
        with_tray(|tray| {
            for &(id, slot) in &self.slots {
                tray.set_screen_rect(id, Some(to_screen(origin, slot)));
            }
        });
    }

    fn hit(&self, x: f32, y: f32) -> Option<u64> {
        self.slots
            .iter()
            .find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .map(|(id, _)| *id)
    }

    fn slot_screen(&self, id: u64) -> Option<RECT> {
        let origin = bar::screen_rect(self.hwnd);
        self.slots.iter().find(|(i, _)| *i == id).map(|&(_, slot)| to_screen(origin, slot))
    }

    /// Bottom-centre of the icon: where apps anchor their menus (version 4).
    fn anchor(&self, id: u64) -> POINT {
        self.slot_screen(id)
            .map_or(POINT::default(), |r| POINT { x: (r.left + r.right) / 2, y: r.bottom })
    }

    fn close_tip(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TIP);
        }
        if let Some(id) = self.tip_open.take() {
            popup::hide_tooltip();
            tray_host::send(id, TrayInput::PopupClose, self.anchor(id));
        }
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hover = self.hit(x, y);
        if let Some(id) = hover {
            tray_host::send(id, TrayInput::MouseMove, self.anchor(id));
        }
        if hover == self.hover {
            return false;
        }
        self.close_tip();
        self.hover = hover;
        if hover.is_some() {
            unsafe { SetTimer(Some(self.hwnd), TIMER_TIP, TIP_DELAY_MS, None) };
        }
        true
    }

    pub fn mouse_leave(&mut self) -> bool {
        self.close_tip();
        self.pressed = None;
        self.hover.take().is_some()
    }

    pub fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        let Some(id) = self.hit(x, y) else {
            return false;
        };
        self.close_tip();
        self.pressed = Some((id, button));
        let input = if button == Button::Left { TrayInput::LeftDown } else { TrayInput::RightDown };
        tray_host::send(id, input, self.anchor(id));
        true
    }

    pub fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        let Some((id, pressed_button)) = self.pressed.take() else {
            return false;
        };
        if pressed_button == button && self.hit(x, y) == Some(id) {
            let input = if button == Button::Left { TrayInput::LeftUp } else { TrayInput::RightUp };
            tray_host::send(id, input, self.anchor(id));
        }
        true
    }

    pub fn double_click(&mut self, x: f32, y: f32, button: Button) -> bool {
        let Some(id) = self.hit(x, y) else {
            return false;
        };
        if button == Button::Left {
            tray_host::send(id, TrayInput::LeftDouble, self.anchor(id));
        }
        // Windows sends DBLCLK in place of the second DOWN; its UP still follows.
        self.pressed = Some((id, button));
        false
    }

    /// Hover delay elapsed: our tooltip, or the app's own popup for version-4 icons.
    pub fn timer(&mut self, id: usize) -> bool {
        if id != TIMER_TIP {
            return false;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TIP);
        }
        let Some(hover) = self.hover else {
            return false;
        };
        let tip = with_tray(|tray| tray.icon(hover).map(|i| (i.wants_standard_tip(), i.tip.clone()))).flatten();
        match tip {
            Some((true, text)) if !text.is_empty() => {
                if let Some(anchor) = self.slot_screen(hover) {
                    popup::show_tooltip(&text, anchor, self.scale);
                }
            }
            Some((false, _)) => tray_host::send(hover, TrayInput::PopupOpen, self.anchor(hover)),
            _ => return false,
        }
        self.tip_open = Some(hover);
        false
    }
}

fn to_screen(origin: RECT, slot: D2D_RECT_F) -> RECT {
    RECT {
        left: origin.left + slot.left.round() as i32,
        top: origin.top + slot.top.round() as i32,
        right: origin.left + slot.right.round() as i32,
        bottom: origin.top + slot.bottom.round() as i32,
    }
}
