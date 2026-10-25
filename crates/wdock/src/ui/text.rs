use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::core::{PCWSTR, Result, w};

use crate::ui::bar::PaintCx;

pub const UI_FONT: PCWSTR = w!("Segoe UI Variable Text");
pub const ICON_FONT: PCWSTR = w!("Segoe Fluent Icons");

/// A single-line, vertically centred text format sized in DIPs.
pub fn format(
    cx: &PaintCx,
    family: PCWSTR,
    size_dip: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    format_px(&cx.gfx.dwrite, family, size_dip * cx.scale, weight, align, false)
}

/// Single-line formats are vertically centred; wrapping ones flow from the top.
pub fn format_px(
    dwrite: &IDWriteFactory,
    family: PCWSTR,
    size_px: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
    wrap: bool,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = dwrite.CreateTextFormat(
            family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size_px,
            w!(""),
        )?;
        format.SetTextAlignment(align)?;
        if wrap {
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        } else {
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        }
        Ok(format)
    }
}

/// Laid-out size of `text` in pixels, wrapping at `max_width` if the format wraps.
pub fn measure(dwrite: &IDWriteFactory, text: &str, format: &IDWriteTextFormat, max_width: f32) -> (f32, f32) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    // Centred or trailing text in a huge box is offset by half its width;
    // at f32::MAX that loses all precision and the measured width collapses.
    let max_width = max_width.min(100_000.0);
    let metrics = unsafe {
        dwrite.CreateTextLayout(&wide, format, max_width, 100_000.0).and_then(|layout| {
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics).map(|()| metrics)
        })
    };
    metrics.map_or((0.0, 0.0), |m| (m.widthIncludingTrailingWhitespace, m.height))
}

pub fn draw(cx: &PaintCx, text: &str, format: &IDWriteTextFormat, rect: D2D_RECT_F, color: D2D1_COLOR_F) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let Ok(brush) = cx.gfx.brush(color) else {
        return;
    };
    unsafe {
        cx.dc.DrawText(
            &wide,
            format,
            &rect,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}
