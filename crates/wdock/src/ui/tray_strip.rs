//! A row (or grid) of tray icons inside some bar or popup: draws them, tracks
//! hover, shows the tooltip, and forwards clicks to the owning apps exactly
//! like explorer's notification area does.

use std::collections::HashMap;
use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, ID2D1Bitmap1};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, KillTimer, SetTimer};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};

use crate::model::tray::TrayModel;
use crate::model::tray_hidden;
use crate::platform::tray_host::{self, TrayInput, with_tray};
use crate::ui::bar::{self, Button, PaintCx};
use crate::ui::gfx::{rect, rounded};
use crate::ui::popup;

pub const TIMER_TIP: usize = 50;
const TIP_DELAY_MS: u32 = 450;
const DRAG_THRESHOLD: f32 = 5.0;
pub const ICON_DIP: f32 = 16.0;

thread_local! {
    static DROP_TARGETS: Cell<Option<(RECT, RECT)>> = const { Cell::new(None) };
    static SURFACES: RefCell<[Option<Surface>; 2]> = const { RefCell::new([None, None]) };
    static MARKER: Cell<Option<Marker>> = const { Cell::new(None) };
}

struct Surface { hwnd: HWND, bounds: RECT, slots: Vec<(u64, RECT)> }
#[derive(Clone, Copy, PartialEq)]
struct Marker { hwnd: HWND, rect: RECT, target: Option<u64>, after: bool, overflow: bool }

fn repaint(hwnd: HWND) { let _ = unsafe { InvalidateRect(Some(hwnd), None, false) }; }

fn set_marker(marker: Option<Marker>) {
    let old = MARKER.replace(marker);
    if old == marker { return; }
    if let Some(old) = old { repaint(old.hwnd); }
    if let Some(marker) = marker { repaint(marker.hwnd); }
}

fn marker_at(x: i32, y: i32) -> Option<Marker> {
    SURFACES.with_borrow(|surfaces| {
        // The popup overlaps screen space near the bar, so prefer it.
        for overflow in [true, false] {
            let Some(surface) = &surfaces[usize::from(overflow)] else { continue; };
            if !unsafe { IsWindow(Some(surface.hwnd)).as_bool() } { continue; }
            if !inside(surface.bounds, x, y) { continue; }
            let nearest = surface.slots.iter().min_by_key(|(_, r)| {
                let dy = if y < r.top { r.top - y } else if y >= r.bottom { y - r.bottom + 1 } else { 0 };
                let dx = if x < r.left { r.left - x } else if x >= r.right { x - r.right + 1 } else { 0 };
                dy * 10000 + dx
            });
            if let Some(&(target, slot)) = nearest {
                // On the bar, ignore areas away from the tray icons.
                if !overflow && (x < slot.left - 12 || x > slot.right + 12) { continue; }
                let after = x >= (slot.left + slot.right) / 2;
                let line = if after { slot.right } else { slot.left };
                return Some(Marker { hwnd: surface.hwnd, rect: RECT { left: line - 1, right: line + 1, top: slot.top + 4, bottom: slot.bottom - 4 }, target: Some(target), after, overflow });
            } else if overflow {
                let line = surface.bounds.left + 8;
                return Some(Marker { hwnd: surface.hwnd, rect: RECT { left: line, right: line + 2, top: surface.bounds.top + 8, bottom: surface.bounds.bottom - 8 }, target: None, after: false, overflow });
            }
        }
        None
    })
}

fn in_overflow_surface(x: i32, y: i32) -> bool {
    SURFACES.with_borrow(|surfaces| surfaces[1].as_ref().is_some_and(|surface|
        unsafe { IsWindow(Some(surface.hwnd)).as_bool() } && inside(surface.bounds, x, y)))
}

pub fn set_drop_targets(bar: RECT, chevron: RECT) {
    DROP_TARGETS.set(Some((bar, chevron)));
}

fn inside(rect: RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

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
    drag_origin: Option<(f32, f32)>,
    dragging: bool,
    in_overflow: bool,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
}

impl TrayStrip {
    pub fn set_overflow(&mut self, in_overflow: bool) {
        self.in_overflow = in_overflow;
    }
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
        let mut live = Vec::with_capacity(self.slots.len());
        for &(id, slot) in &self.slots {
            let size = (ICON_DIP * cx.scale).min((slot.bottom - slot.top).max(0.0));
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
                let cy_mid = (slot.top + slot.bottom) / 2.0;
                let dest = rect(cx_mid - size / 2.0, cy_mid - size / 2.0, cx_mid + size / 2.0, cy_mid + size / 2.0);
                unsafe {
                    cx.dc.DrawBitmap(bitmap, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
            }
            live.push(key);
        }
        self.bitmaps.retain(|key, _| live.contains(key));
        if let Some(marker) = MARKER.get().filter(|marker| marker.hwnd == self.hwnd) {
            let origin = bar::screen_rect(self.hwnd);
            let r = marker.rect;
            if let Ok(brush) = cx.gfx.brush(cx.theme.accent) {
                let line = rect((r.left-origin.left) as f32, (r.top-origin.top) as f32, (r.right-origin.left) as f32, (r.bottom-origin.top) as f32);
                unsafe { cx.dc.FillRoundedRectangle(&rounded(line, cx.scale), &brush) };
            }
        }
    }

    /// Records where icons were drawn, for Shell_NotifyIconGetRect.
    pub fn publish_rects(&self) {
        let origin = bar::screen_rect(self.hwnd);
        SURFACES.with_borrow_mut(|surfaces| surfaces[usize::from(self.in_overflow)] = Some(Surface {
            hwnd: self.hwnd, bounds: origin,
            slots: self.slots.iter().map(|&(id, slot)| (id, to_screen(origin, slot))).collect(),
        }));
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
        if let Some((sx, sy)) = self.drag_origin {
            if (x - sx).abs() > DRAG_THRESHOLD * self.scale || (y - sy).abs() > DRAG_THRESHOLD * self.scale {
                self.dragging = true;
                self.close_tip();
            }
            if self.dragging {
                let origin = bar::screen_rect(self.hwnd);
                let (sx, sy) = (origin.left + x.round() as i32, origin.top + y.round() as i32);
                if !self.in_overflow
                    && let Some((_, chevron)) = DROP_TARGETS.get()
                        && inside(chevron, sx, sy) { popup::open_overflow_for_drag(chevron, self.scale); }
                set_marker(marker_at(sx, sy));
                return true;
            }
        }
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
        if self.drag_origin.is_none() { self.pressed = None; }
        self.hover.take().is_some()
    }

    pub fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        let Some(id) = self.hit(x, y) else {
            return false;
        };
        self.close_tip();
        self.pressed = Some((id, button));
        if button == Button::Left {
            self.drag_origin = Some((x, y));
            self.dragging = false;
            unsafe { SetCapture(self.hwnd); }
        } else {
            tray_host::send(id, TrayInput::RightDown, self.anchor(id));
        }
        true
    }

    pub fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        let Some((id, pressed_button)) = self.pressed.take() else {
            return false;
        };
        let ordinary_click = self.drag_origin.is_some();
        if pressed_button == Button::Left {
            self.drag_origin = None;
            let _ = unsafe { ReleaseCapture() };
            if std::mem::take(&mut self.dragging) {
                let origin = bar::screen_rect(self.hwnd);
                let (sx, sy) = (origin.left + x.round() as i32, origin.top + y.round() as i32);
                let marker = MARKER.get();
                set_marker(None);
                if let Some(marker) = marker {
                    set_on_top_bar(id, !marker.overflow);
                    if let Some(target) = marker.target {
                        with_tray(|tray| tray.move_icon(id, target, marker.after));
                    }
                    crate::app::notify_model();
                } else if in_overflow_surface(sx, sy) {
                    set_on_top_bar(id, false);
                } else if let Some((bar, chevron)) = DROP_TARGETS.get() {
                    if inside(chevron, sx, sy) {
                        set_on_top_bar(id, false);
                    } else if self.in_overflow && inside(bar, sx, sy) {
                        set_on_top_bar(id, true);
                    }
                }
                return true;
            }
        }
        if pressed_button == button && self.hit(x, y) == Some(id) {
            if button == Button::Left {
                if ordinary_click {
                    tray_host::send(id, TrayInput::LeftDown, self.anchor(id));
                }
                tray_host::send(id, TrayInput::LeftUp, self.anchor(id));
            } else {
                tray_host::send(id, TrayInput::RightUp, self.anchor(id));
            }
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

/// Puts an icon on the top bar or back there, and remembers the choice.
fn set_on_top_bar(id: u64, show: bool) {
    if let Some(Some((hidden, shown))) = with_tray(|tray| tray.set_icon_hidden(id, !show)) {
        tray_hidden::save(&hidden);
        tray_hidden::save_shown(&shown);
    }
    crate::app::notify_model();
}

fn to_screen(origin: RECT, slot: D2D_RECT_F) -> RECT {
    RECT {
        left: origin.left + slot.left.round() as i32,
        top: origin.top + slot.top.round() as i32,
        right: origin.left + slot.right.round() as i32,
        bottom: origin.top + slot.bottom.round() as i32,
    }
}
