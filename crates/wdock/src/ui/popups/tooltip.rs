use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER};

use crate::model::config::Backdrop;
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{PaintCx, View};
use crate::ui::gfx::{Gfx, rect, rounded};
use crate::ui::text;

const FONT: f32 = 12.0;
const PAD_X: f32 = 9.0;
const PAD_Y: f32 = 5.0;
const MAX_WIDTH: f32 = 360.0;

pub struct TooltipView {
    text: String,
    size: (i32, i32),
}

impl TooltipView {
    pub fn new(gfx: &Gfx, text: &str, scale: f32) -> Self {
        // Tray tips may be multi-line; keep at most what fits a compact tooltip.
        let text = text.lines().take(4).collect::<Vec<_>>().join("\n");
        let (w, h) = text::format_px(&gfx.dwrite, text::UI_FONT, FONT * scale, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, true)
            .map(|format| text::measure(&gfx.dwrite, &text, &format, MAX_WIDTH * scale))
            .unwrap_or((100.0, 16.0));
        let size = ((w + PAD_X * 2.0 * scale).ceil() as i32, (h + PAD_Y * 2.0 * scale).ceil() as i32);
        Self { text, size }
    }

    pub fn size_px(&self) -> (i32, i32) {
        self.size
    }
}

impl View for TooltipView {
    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let bounds = rect(0.0, 0.0, cx.width, cx.height);
        if cx.theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(cx.theme.solid_fill) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(bounds, DWM_CORNER_RADIUS * s), &brush) };
            }
        let Ok(format) =
            text::format_px(&cx.gfx.dwrite, text::UI_FONT, FONT * s, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, true)
        else {
            return;
        };
        let inner = rect(PAD_X * s, PAD_Y * s, cx.width - PAD_X * s, cx.height - PAD_Y * s);
        text::draw(cx, &self.text, &format, inner, cx.theme.text);
    }
}
