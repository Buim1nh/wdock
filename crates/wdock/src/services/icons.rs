//! Sharp icons for paths and `shell:AppsFolder` items, via the shell's image
//! factory (the same source explorer uses), converted to premultiplied BGRA
//! so Direct2D can draw them.
//!
//! The shell returns its nearest frame at or above the requested size (often
//! 48 or 256 px). Scaling that down at draw time softens edges, so icons are
//! resampled here, once, to exactly the pixels they will cover, and then
//! drawn 1:1.

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{DeleteObject, HPALETTE};
use windows::Win32::Graphics::Imaging::{
    GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory, WICBitmapCacheOnLoad,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeMedianCut,
    WICBitmapUsePremultipliedAlpha,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY,
    SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::core::{HSTRING, Interface, Result};

pub struct ShellIcon {
    pub name: String,
    pub image: IWICBitmapSource,
}

pub fn load(wic: &IWICImagingFactory, target: &str, size_px: u32) -> Result<ShellIcon> {
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&HSTRING::from(target), None)? };
    load_item(wic, &item, size_px)
}

pub fn load_item(wic: &IWICImagingFactory, item: &IShellItem, size_px: u32) -> Result<ShellIcon> {
    unsafe {
        let display = item.GetDisplayName(SIGDN_NORMALDISPLAY)?;
        let name = display.to_string().unwrap_or_default();
        CoTaskMemFree(Some(display.0 as *const _));

        let factory: IShellItemImageFactory = item.cast()?;
        let size = SIZE { cx: size_px as i32, cy: size_px as i32 };
        let hbitmap = factory.GetImage(size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)?;
        let bitmap = wic.CreateBitmapFromHBITMAP(hbitmap, HPALETTE::default(), WICBitmapUsePremultipliedAlpha);
        let _ = DeleteObject(hbitmap.into());
        let bitmap = bitmap?;

        let image = resize(wic, &bitmap.cast()?, size_px)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &image,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;
        // Materialise the pixels so no COM chain back to the shell stays alive.
        let image = wic.CreateBitmapFromSource(&converter, WICBitmapCacheOnLoad)?.cast()?;
        Ok(ShellIcon { name, image })
    }
}

/// `source` resampled to `size_px` square (Fant filter: a proper area average
/// when shrinking), or unchanged if it already has that size.
pub fn resize(wic: &IWICImagingFactory, source: &IWICBitmapSource, size_px: u32) -> Result<IWICBitmapSource> {
    let size_px = size_px.max(1);
    unsafe {
        let (mut w, mut h) = (0, 0);
        source.GetSize(&mut w, &mut h)?;
        if w == size_px && h == size_px {
            return Ok(source.clone());
        }
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(source, size_px, size_px, WICBitmapInterpolationModeFant)?;
        wic.CreateBitmapFromSource(&scaler, WICBitmapCacheOnLoad)?.cast()
    }
}
