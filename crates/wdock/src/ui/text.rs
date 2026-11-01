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

fn ui_family(dwrite: &IDWriteFactory) -> PCWSTR {
    // Cache an index rather than a PCWSTR, which is not Send or Sync.
    static FAMILY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let families = [w!("SF Pro Text"), w!("SF Pro"), w!("SF Pro Display"), UI_FONT];
    let index = *FAMILY.get_or_init(|| unsafe {
        let mut collection = None;
        if dwrite.GetSystemFontCollection(&mut collection, false).is_ok() {
            if let Some(collection) = collection {
                for (index, family) in families[..3].iter().enumerate() {
                    let mut family_index = 0;
                    let mut exists = windows::core::BOOL::default();
                    if collection.FindFamilyName(*family, &mut family_index, &mut exists).is_ok() && exists.as_bool() {
                        return index;
                    }
                }
            }
        }
        3
    });
    families[index]
}

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
    let family = if family == UI_FONT { ui_family(dwrite) } else { family };
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

/// Keep shaping and wrapping intact while lighting the sung Unicode characters.
/// DirectWrite ranges are UTF-16 offsets, whereas the bridge counts Unicode scalars.
pub fn draw_karaoke(cx: &PaintCx, text: &str, format: &IDWriteTextFormat, rect: D2D_RECT_F, progress: f32, alpha: f32) {
    use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE;
    let wide: Vec<u16> = text.encode_utf16().collect();
    let whole = progress.max(0.0).floor() as usize;
    let sung = text.chars().take(whole).map(char::len_utf16).sum::<usize>() as u32;
    let current = text.chars().nth(whole).map_or(0, char::len_utf16) as u32;
    let color = |opacity| D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: alpha * opacity };
    let (Ok(dim), Ok(bright), Ok(edge)) = (cx.gfx.brush(color(0.38)), cx.gfx.brush(color(0.98)), cx.gfx.brush(color(0.38 + 0.60 * progress.fract()))) else { return; };
    unsafe {
        let Ok(layout) = cx.gfx.dwrite.CreateTextLayout(&wide, format, rect.right - rect.left, (rect.bottom - rect.top).max(1.0)) else { return; };
        if sung > 0 { let _ = layout.SetDrawingEffect(&bright, DWRITE_TEXT_RANGE { startPosition: 0, length: sung.min(wide.len() as u32) }); }
        if current > 0 { let _ = layout.SetDrawingEffect(&edge, DWRITE_TEXT_RANGE { startPosition: sung, length: current }); }
        cx.dc.DrawTextLayout(windows_numerics::Vector2 { X: rect.left, Y: rect.top }, &layout, &dim, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING};

    #[test]
    fn lyrics_wrap_at_fractional_dpi_without_losing_height() {
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.unwrap();
        for scale in [1.0, 1.25, 1.5] {
            let format = format_px(&factory, UI_FONT, 20.0 * scale, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, true).unwrap();
            let line = "Một câu hát dài với dấu tiếng Việt và những từ tiếp theo";
            let single = measure(&factory, line, &format, 1500.0 * scale);
            let wrapped = measure(&factory, line, &format, 200.0 * scale);
            assert!(wrapped.1 > single.1 * 1.5);
            assert!(wrapped.0 <= 201.0 * scale);
        }
    }
}
