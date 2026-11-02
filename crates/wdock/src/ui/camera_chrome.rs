//! Camera chrome shares wdock's theme and DirectWrite font resolution.
use windows::core::Result;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_FOCUS, ODS_SELECTED, ODS_DISABLED};
use crate::ui::{gfx::{rect, rounded}, text, theme::Theme};

pub struct Chrome {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    target: Option<ID2D1HwndRenderTarget>,
    theme: Theme,
    pub detail: String,
    pub error: bool,
}

impl Chrome {
    pub fn new() -> Result<Self> {
        let config = crate::model::config::load();
        text::set_ui_font(&config.appearance.font);
        unsafe {
            Ok(Self { factory: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?, target: None,
                theme: Theme::resolve(&config.appearance), detail: "Opening camera…".into(), error: false })
        }
    }

    pub fn decorate(&self, hwnd: HWND) {
        crate::ui::backdrop::apply(hwnd, &self.theme, crate::ui::backdrop::Corners::Round);
    }

    fn label(&self, rt: &ID2D1RenderTarget, value: &str, r: D2D_RECT_F, size: f32, style: [bool; 3]) {
        let [bold, centered, dim] = style;
        let format = text::format_px(&self.dwrite, text::UI_FONT, size,
            if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
            if centered { DWRITE_TEXT_ALIGNMENT_CENTER } else { DWRITE_TEXT_ALIGNMENT_LEADING }, false);
        if let Ok(format) = format {
            unsafe {
                let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
                let sign = self.dwrite.CreateEllipsisTrimmingSign(&format).ok();
                let _ = format.SetTrimming(&trimming, sign.as_ref());
                if let Ok(brush) = rt.CreateSolidColorBrush(if dim { &self.theme.text_dim } else { &self.theme.text }, None) {
                    rt.DrawText(&value.encode_utf16().collect::<Vec<_>>(), &format, &r, &brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
        }
    }

    pub fn paint(&mut self, hwnd: HWND, width: u32, height: u32, scale: f32) {
        unsafe {
            if self.target.is_none() {
                self.target = self.factory.CreateHwndRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES::default(), &D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd, pixelSize: D2D_SIZE_U { width, height }, ..Default::default() }).ok();
            }
            let Some(rt) = &self.target else { return; };
            if rt.Resize(&D2D_SIZE_U { width: width.max(1), height: height.max(1) }).is_err() { self.target = None; return; }
            rt.SetDpi(96.0, 96.0);
            rt.BeginDraw();
            let mut background = self.theme.solid_fill; background.a = 1.0;
            rt.Clear(Some(&background));
            let (w, h, s) = (width as f32, height as f32, scale);
            self.label(rt, "Camera preview", rect(20.0*s, 9.0*s, w-100.0*s, 35.0*s), 16.0*s, [true, false, false]);
            self.label(rt, &self.detail, rect(20.0*s, 35.0*s, w-65.0*s, 59.0*s), 12.0*s, [false, false, true]);
            let viewport = rect(16.0*s, 72.0*s, w-16.0*s, h-84.0*s);
            let well = if self.error && !self.theme.dark { crate::ui::gfx::rgba(232, 232, 236, 1.0) } else { crate::ui::gfx::rgba(8, 8, 10, 1.0) };
            if let Ok(brush) = rt.CreateSolidColorBrush(&well, None) { rt.FillRoundedRectangle(&rounded(viewport, 12.0*s), &brush); }
            if self.error {
                let mid = (viewport.top + viewport.bottom) / 2.0;
                self.label(rt, "Camera unavailable", rect(viewport.left+12.0*s, mid-30.0*s, viewport.right-12.0*s, mid), 16.0*s, [true, true, false]);
                self.label(rt, "Check access in Settings or try another camera.", rect(viewport.left+12.0*s, mid, viewport.right-12.0*s, mid+30.0*s), 12.0*s, [false, true, false]);
            }
            self.label(rt, "Preview only · nothing is recorded", rect(20.0*s, h-79.0*s, w-20.0*s, h-58.0*s), 11.5*s, [false, false, true]);
            if rt.EndDraw(None, None).is_err() { self.target = None; }
        }
    }

    pub fn button(&self, item: &DRAWITEMSTRUCT, scale: f32) {
        unsafe {
            let properties = D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE }, ..Default::default() };
            let Ok(rt) = self.factory.CreateDCRenderTarget(&properties) else { return; };
            if rt.BindDC(item.hDC, &item.rcItem).is_err() { return; }
            rt.SetDpi(96.0, 96.0);
            rt.BeginDraw();
            let mut bg = self.theme.solid_fill; bg.a = 1.0; rt.Clear(Some(&bg));
            let (w,h) = ((item.rcItem.right-item.rcItem.left) as f32, (item.rcItem.bottom-item.rcItem.top) as f32);
            let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
            let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
            let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
            let tint = if pressed { self.theme.hover } else { self.theme.control };
            // GDI-backed buttons are opaque; composite translucent tokens ourselves.
            let color = D2D1_COLOR_F { r: bg.r*(1.0-tint.a)+tint.r*tint.a, g: bg.g*(1.0-tint.a)+tint.g*tint.a, b: bg.b*(1.0-tint.a)+tint.b*tint.a, a: 1.0 };
            let r = rounded(rect(1.0,1.0,w-1.0,h-1.0), 10.0*scale);
            if let Ok(brush) = rt.CreateSolidColorBrush(&color,None) { rt.FillRoundedRectangle(&r,&brush); }
            if focused && let Ok(brush) = rt.CreateSolidColorBrush(&self.theme.accent,None) { rt.DrawRoundedRectangle(&r,&brush,1.5*scale,None); }
            let label = match item.CtlID { 1 => "Next camera", 2 => "Camera settings", _ => "×" };
            self.label(&rt, label, rect(4.0,0.0,w-4.0,h), if item.CtlID == 3 { 23.0*scale } else { 12.5*scale }, [false, true, disabled]);
            let _ = rt.EndDraw(None,None);
        }
    }
}

pub fn video_bounds(width: i32, height: i32, scale: f32, aspect: f32) -> RECT {
    let pad = (16.0*scale).round() as i32;
    let top = (72.0*scale).round() as i32;
    let available_w = (width-2*pad).max(1);
    let available_h = (height-top-(84.0*scale).round() as i32).max(1);
    let w = available_w.min((available_h as f32*aspect) as i32).max(1);
    let h = (w as f32/aspect).round() as i32;
    let left = pad+(available_w-w)/2;
    let y = top+(available_h-h)/2;
    RECT { left, top: y, right: left+w, bottom: y+h }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_keeps_aspect_and_chrome_clear_at_fractional_dpi() {
        for s in [1.0,1.25,1.5,2.0] { for aspect in [4.0/3.0,16.0/9.0,9.0/16.0] {
            let (w,h) = ((720.0*s) as i32,(560.0*s) as i32);
            let r = video_bounds(w,h,s,aspect);
            assert!(r.left >= (16.0*s) as i32 && r.right <= w-(16.0*s) as i32);
            assert!(r.top >= (72.0*s) as i32 && r.bottom <= h-(84.0*s) as i32);
            assert!(((r.right-r.left) as f32/(r.bottom-r.top) as f32-aspect).abs()<0.02);
        } }
    }
}
