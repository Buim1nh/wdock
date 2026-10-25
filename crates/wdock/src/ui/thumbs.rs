//! Live window previews through DWM thumbnails. DWM composes the source
//! window's content straight into our window, so nothing is copied or drawn
//! by us and the preview stays live at no CPU cost to wdock.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY,
    DWM_TNP_VISIBLE, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties,
};

use crate::ui::gfx::rect;

pub struct Thumbnail {
    id: isize,
}

impl Thumbnail {
    /// Registers `source` for drawing into `dest`, initially hidden.
    pub fn register(dest: HWND, source: HWND) -> Option<Self> {
        match unsafe { DwmRegisterThumbnail(dest, source) } {
            Ok(id) => Some(Self { id }),
            Err(err) => {
                log::debug!("thumbnail for {:?}: {err}", source.0);
                None
            }
        }
    }

    /// Source size in pixels; zero when DWM has nothing to show.
    pub fn source_size(&self) -> (f32, f32) {
        match unsafe { DwmQueryThumbnailSourceSize(self.id) } {
            Ok(size) => (size.cx as f32, size.cy as f32),
            Err(_) => (0.0, 0.0),
        }
    }

    /// Shows the preview fitted (aspect preserved, never enlarged) and centred in `bounds`.
    /// Returns the rectangle it occupies, or `None` if there is nothing to show.
    pub fn place(&self, bounds: D2D_RECT_F, opacity: u8) -> Option<D2D_RECT_F> {
        let fitted = fit(self.source_size(), bounds)?;
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: RECT {
                left: fitted.left.round() as i32,
                top: fitted.top.round() as i32,
                right: fitted.right.round() as i32,
                bottom: fitted.bottom.round() as i32,
            },
            opacity,
            fVisible: true.into(),
            fSourceClientAreaOnly: false.into(),
            ..Default::default()
        };
        unsafe { DwmUpdateThumbnailProperties(self.id, &props) }.ok()?;
        Some(fitted)
    }

    pub fn hide(&self) {
        let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_VISIBLE, fVisible: false.into(), ..Default::default() };
        let _ = unsafe { DwmUpdateThumbnailProperties(self.id, &props) };
    }
}

impl Drop for Thumbnail {
    fn drop(&mut self) {
        let _ = unsafe { DwmUnregisterThumbnail(self.id) };
    }
}

/// Largest rectangle of the source's aspect ratio inside `bounds`, centred.
pub fn fit((w, h): (f32, f32), bounds: D2D_RECT_F) -> Option<D2D_RECT_F> {
    let (bw, bh) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
    if w < 1.0 || h < 1.0 || bw < 1.0 || bh < 1.0 {
        return None;
    }
    let k = (bw / w).min(bh / h).min(1.0);
    let (fw, fh) = (w * k, h * k);
    let left = bounds.left + (bw - fw) / 2.0;
    let top = bounds.top + (bh - fh) / 2.0;
    Some(rect(left, top, left + fw, top + fh))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_centres() {
        let r = fit((1600.0, 900.0), rect(0.0, 0.0, 320.0, 320.0)).unwrap();
        assert_eq!((r.left, r.right), (0.0, 320.0));
        assert_eq!((r.top, r.bottom), (70.0, 250.0));
        let small = fit((100.0, 50.0), rect(0.0, 0.0, 400.0, 400.0)).unwrap();
        assert_eq!(small.right - small.left, 100.0, "never enlarged");
        assert!(fit((0.0, 0.0), rect(0.0, 0.0, 10.0, 10.0)).is_none());
    }
}
