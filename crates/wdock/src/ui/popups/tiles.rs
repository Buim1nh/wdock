//! Window tiles shared by the overview and dock previews: a
//! live DWM thumbnail (or the app's icon when there is nothing to show) above
//! an icon-and-title row.

use std::collections::HashMap;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING};

use crate::model::windows::WindowModel;
use crate::platform::wininfo;
use crate::ui::bar::PaintCx;
use crate::ui::gfx::{rect, rounded};
use crate::ui::text;
use crate::ui::thumbs::Thumbnail;

pub const TITLE_HEIGHT: f32 = 30.0;
const ICON: f32 = 18.0;
const INSET: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    None,
    Hot,
}

/// Draws an icon bitmap, creating and caching it on first use.
pub fn draw_icon(
    cx: &PaintCx,
    model: &WindowModel,
    bitmaps: &mut HashMap<String, ID2D1Bitmap1>,
    hwnd: HWND,
    dest: D2D_RECT_F,
    opacity: f32,
) {
    let Some((key, image)) = model.window_icon(hwnd) else {
        return;
    };
    if !bitmaps.contains_key(&key) {
        match unsafe { cx.dc.CreateBitmapFromWicBitmap(image, None) } {
            Ok(bitmap) => {
                bitmaps.insert(key.clone(), bitmap);
            }
            Err(err) => log::debug!("tile icon: {err}"),
        }
    }
    if let Some(bitmap) = bitmaps.get(&key) {
        unsafe { cx.dc.DrawBitmap(bitmap, Some(&dest), opacity, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) };
    }
}

/// Paints a tile filling `tile` (pixels) and places its thumbnail.
/// Hidden or empty windows show their app icon instead.
pub fn paint(
    cx: &PaintCx,
    model: &WindowModel,
    bitmaps: &mut HashMap<String, ID2D1Bitmap1>,
    thumb: Option<&Thumbnail>,
    hwnd: HWND,
    tile: D2D_RECT_F,
    emphasis: Emphasis,
) {
    let s = cx.scale;
    let theme = cx.theme;
    let plate = match emphasis {
        Emphasis::Hot => Some(theme.hover),
        Emphasis::None => None,
    };
    if let Some(color) = plate
        && let Ok(brush) = cx.gfx.brush(color) {
            unsafe { cx.dc.FillRoundedRectangle(&rounded(tile, 10.0 * s), &brush) };
        }

    let inset = INSET * s;
    let title_top = tile.bottom - TITLE_HEIGHT * s;
    let preview = rect(tile.left + inset, tile.top + inset, tile.right - inset, title_top - 2.0 * s);
    let shown = (!model.is_hidden(hwnd)).then_some(thumb).flatten().and_then(|t| t.place(preview, 255));
    if shown.is_none() {
        if let Some(t) = thumb {
            t.hide();
        }
        let size = ((preview.right - preview.left).min(preview.bottom - preview.top) * 0.45).min(64.0 * s);
        let (cx_, cy) = ((preview.left + preview.right) / 2.0, (preview.top + preview.bottom) / 2.0);
        let dest = rect(cx_ - size / 2.0, cy - size / 2.0, cx_ + size / 2.0, cy + size / 2.0);
        draw_icon(cx, model, bitmaps, hwnd, dest, if model.is_hidden(hwnd) { 0.7 } else { 1.0 });
    }

    let icon = ICON * s;
    let mid = (title_top + tile.bottom) / 2.0;
    let icon_rect = rect(tile.left + inset, mid - icon / 2.0, tile.left + inset + icon, mid + icon / 2.0);
    draw_icon(cx, model, bitmaps, hwnd, icon_rect, 1.0);
    if let Ok(format) = text::format(cx, text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING) {
        let mut title = wininfo::title(hwnd);
        if title.is_empty() {
            title = model.window_app_name(hwnd).to_owned();
        }
        let _ = unsafe { format.SetTrimming(&ellipsis(), None) };
        let text_rect = rect(icon_rect.right + 6.0 * s, title_top, tile.right - inset, tile.bottom);
        text::draw(cx, &title, &format, text_rect, theme.text);
    }
}

fn ellipsis() -> windows::Win32::Graphics::DirectWrite::DWRITE_TRIMMING {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER};
    DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }
}

/// Registers a thumbnail per window into `dest`.
pub fn register(dest: HWND, windows: &[HWND]) -> Vec<Option<Thumbnail>> {
    windows.iter().map(|&w| Thumbnail::register(dest, w)).collect()
}
