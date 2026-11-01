//! Small power-state marks shared by the menu bar and Control Centre.
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;

use crate::services::Battery;
use crate::ui::bar::PaintCx;
use crate::ui::gfx::{rect, rgba};
use crate::ui::{glyphs, text};

pub fn level_color(battery: Battery, normal: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    if battery.charging { rgba(48, 209, 88, 1.0) }
    else if battery.percent <= 20 && !battery.plugged { rgba(255, 69, 58, 1.0) }
    else { normal }
}

pub fn mark(cx: &PaintCx, body: D2D_RECT_F, battery: Battery, icon: &IDWriteTextFormat) {
    if !battery.charging && !battery.plugged { return; }
    let s = cx.scale;
    let dark = rgba(0, 0, 0, 1.0);
    let white = rgba(255, 255, 255, 1.0);
    // A dark keyline keeps the bolt visible over either the fill or shell.
    for (x, y) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
        text::draw(cx, glyphs::CHARGING, icon, rect(body.left + x*s, body.top + y*s, body.right + x*s, body.bottom + y*s), dark);
    }
    text::draw(cx, glyphs::CHARGING, icon, body, white);
}
