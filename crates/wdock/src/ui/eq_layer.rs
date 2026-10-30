//! The island's moving equalizer, animated by DirectComposition.
//!
//! Each bar is a composition visual showing one tiny pre-drawn surface, with
//! a scale transform whose Y factor follows an endlessly repeating
//! animation curve. DWM plays the curves itself, so once the layer is
//! committed wdock does no work per frame: no timer, no repaint, no wake-up.
//! It costs only DWM composing a few pixels while the island is visible
//! (hidden windows, as in game mode, are not composed at all).

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, ID2D1DeviceContext, ID2D1Image,
};
use windows::Win32::Graphics::DirectComposition::{
    IDCompositionAnimation, IDCompositionScaleTransform, IDCompositionSurface, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::core::Result;

use crate::ui::gfx::{Gfx, rect, rounded};

/// Per bar: (period in seconds, lowest and highest height as a fraction).
/// Unequal periods keep the bars from ever moving in step.
const CURVES: [(f64, f32, f32); 4] = [(0.62, 0.30, 1.0), (0.47, 0.25, 0.85), (0.71, 0.35, 0.95), (0.55, 0.20, 0.80)];

pub struct EqLayer {
    root: IDCompositionVisual,
    container: IDCompositionVisual,
    attached: bool,
    at: (f32, f32),
    // Kept alive for as long as the visuals show them.
    _surface: IDCompositionSurface,
    _bars: Vec<(IDCompositionVisual, IDCompositionScaleTransform, IDCompositionAnimation)>,
}

impl EqLayer {
    /// Builds four `bar_w` x `max_h` bars `gap` apart (window pixels) in `color`,
    /// as children of the window's root visual, detached until `place`.
    pub fn new(gfx: &Gfx, root: &IDCompositionVisual, bar_w: f32, gap: f32, max_h: f32, color: D2D1_COLOR_F) -> Result<Self> {
        let (w, h) = (bar_w.ceil().max(1.0) as u32, max_h.ceil().max(1.0) as u32);
        unsafe {
            let surface = gfx.dcomp.CreateSurface(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_PREMULTIPLIED)?;
            draw_bar(gfx, &surface, w as f32, h as f32, color)?;
            let container = gfx.dcomp.CreateVisual()?;
            let mut bars = Vec::with_capacity(CURVES.len());
            for (i, &(period, low, high)) in CURVES.iter().enumerate() {
                let visual = gfx.dcomp.CreateVisual()?;
                visual.SetContent(&surface)?;
                visual.SetOffsetX2(i as f32 * (bar_w + gap))?;
                let transform = gfx.dcomp.CreateScaleTransform()?;
                // Shrink towards the middle, as the still bars do.
                transform.SetCenterY2(h as f32 / 2.0)?;
                let animation = gfx.dcomp.CreateAnimation()?;
                bounce(&animation, period / 2.0, low, high)?;
                transform.SetScaleX2(1.0)?;
                transform.SetScaleY(&animation)?;
                visual.SetTransform(&transform)?;
                container.AddVisual(&visual, true, None)?;
                bars.push((visual, transform, animation));
            }
            Ok(Self { root: root.clone(), container, attached: false, at: (f32::NAN, f32::NAN), _surface: surface, _bars: bars })
        }
    }

    /// Shows the bars with their left edge at `x` and centred on `mid`.
    pub fn place(&mut self, gfx: &Gfx, x: f32, mid: f32, max_h: f32) {
        let top = (mid - max_h / 2.0).round();
        let x = x.round();
        if self.attached && self.at == (x, top) {
            return;
        }
        let result = unsafe {
            self.container.SetOffsetX2(x).and_then(|()| self.container.SetOffsetY2(top)).and_then(|()| {
                if self.attached { Ok(()) } else { self.root.AddVisual(&self.container, true, None) }
            })
        };
        match result {
            Ok(()) => {
                self.attached = true;
                self.at = (x, top);
                let _ = unsafe { gfx.dcomp.Commit() };
            }
            Err(err) => log::warn!("equalizer layer: {err}"),
        }
    }

    /// Takes the bars off the window; DWM stops animating them.
    pub fn detach(&mut self, gfx: &Gfx) {
        if !std::mem::take(&mut self.attached) {
            return;
        }
        unsafe {
            let _ = self.root.RemoveVisual(&self.container);
            let _ = gfx.dcomp.Commit();
        }
    }
}

impl Drop for EqLayer {
    fn drop(&mut self) {
        if self.attached {
            let _ = unsafe { self.root.RemoveVisual(&self.container) };
        }
    }
}

/// low -> high -> low with ease in and out (cubic Hermite), `half` seconds
/// each way, repeated for ever.
fn bounce(animation: &IDCompositionAnimation, half: f64, low: f32, high: f32) -> Result<()> {
    let t = half as f32;
    let up = high - low;
    unsafe {
        animation.AddCubic(0.0, low, 0.0, 3.0 * up / (t * t), -2.0 * up / (t * t * t))?;
        animation.AddCubic(half, high, 0.0, -3.0 * up / (t * t), 2.0 * up / (t * t * t))?;
        animation.AddRepeat(2.0 * half, 2.0 * half)
    }
}

/// Fills the composition surface with one rounded bar. Uses its own device
/// context: the window's shared one may be mid-draw when this runs.
fn draw_bar(gfx: &Gfx, surface: &IDCompositionSurface, w: f32, h: f32, color: D2D1_COLOR_F) -> Result<()> {
    unsafe {
        let device = gfx.dc.GetDevice()?;
        let dc: ID2D1DeviceContext = device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        let mut offset = POINT::default();
        let dxgi: IDXGISurface = surface.BeginDraw(None, &mut offset)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };
        let drawn = (|| -> Result<()> {
            let target = dc.CreateBitmapFromDxgiSurface(&dxgi, Some(&props))?;
            dc.SetTarget(&target);
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F::default()));
            let (x, y) = (offset.x as f32, offset.y as f32);
            let brush = dc.CreateSolidColorBrush(&color, None)?;
            dc.FillRoundedRectangle(&rounded(rect(x, y, x + w, y + h), w / 2.0), &brush);
            let ended = dc.EndDraw(None, None);
            dc.SetTarget(None::<&ID2D1Image>);
            ended
        })();
        surface.EndDraw()?;
        drawn
    }
}
