//! Shared GPU objects plus one composition swap chain per bar window.
//!
//! All bars share a single D2D device context, so bitmaps (icons) created once
//! can be drawn into any window. Everything is drawn in physical pixels.
//! Nothing renders unless a window is invalidated: there is no frame loop.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_ROUNDED_RECT, D2D1_UNIT_MODE_PIXELS, D2D1CreateFactory, ID2D1DeviceContext,
    ID2D1Factory1, ID2D1Image, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
};
use windows::core::{Interface, Result};

use crate::model::config::Renderer;

pub struct Gfx {
    pub d3d: ID3D11Device,
    pub dxgi_factory: IDXGIFactory2,
    pub dc: ID2D1DeviceContext,
    pub dcomp: IDCompositionDevice,
    pub dwrite: IDWriteFactory,
}

impl Gfx {
    pub fn new(renderer: Renderer) -> Result<Self> {
        let d3d = create_d3d_device(renderer)?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        unsafe {
            let adapter = dxgi.GetAdapter()?;
            let dxgi_factory: IDXGIFactory2 = adapter.GetParent()?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(
                D2D1_FACTORY_TYPE_SINGLE_THREADED,
                Some(&D2D1_FACTORY_OPTIONS::default()),
            )?;
            let dc = factory.CreateDevice(&dxgi)?.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            dc.SetUnitMode(D2D1_UNIT_MODE_PIXELS);
            Ok(Self {
                dxgi_factory,
                dcomp: DCompositionCreateDevice(&dxgi)?,
                dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
                d3d,
                dc,
            })
        }
    }

    pub fn brush(&self, color: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        unsafe { self.dc.CreateSolidColorBrush(&color, None) }
    }
}

/// WARP rasterises on the CPU. Our surfaces are small and only redrawn on
/// change, so it is fast enough and avoids loading a GPU driver (30-60 MB)
/// or keeping a laptop's discrete GPU awake.
fn create_d3d_device(renderer: Renderer) -> Result<ID3D11Device> {
    let mut last_err = None;
    let drivers: &[_] = match renderer {
        Renderer::Software => &[D3D_DRIVER_TYPE_WARP],
        Renderer::Gpu => &[D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP],
    };
    for &driver in drivers {
        let mut device = None;
        let created = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        };
        match (created, device) {
            (Ok(()), Some(device)) => return Ok(device),
            (Err(err), _) => {
                log::warn!("D3D11 device ({driver:?}) unavailable: {err}");
                last_err = Some(err);
            }
            (Ok(()), None) => {}
        }
    }
    Err(last_err.unwrap_or_else(windows::core::Error::empty))
}

/// A window's composition swap chain.
pub struct Surface {
    swapchain: IDXGISwapChain1,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual,
}

impl Surface {
    pub fn new(gfx: &Gfx, hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        unsafe {
            let swapchain = gfx.dxgi_factory.CreateSwapChainForComposition(&gfx.d3d, &desc, None)?;
            let target = gfx.dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = gfx.dcomp.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            target.SetRoot(&visual)?;
            gfx.dcomp.Commit()?;
            Ok(Self { swapchain, _target: target, visual })
        }
    }

    /// The window's root visual (its content is the swap chain); views can
    /// add child visuals that DWM composes on top.
    pub fn root(&self) -> &IDCompositionVisual {
        &self.visual
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        unsafe {
            self.swapchain.ResizeBuffers(
                2,
                width.max(1),
                height.max(1),
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        }
    }

    /// Clears to transparent, runs `paint`, then presents.
    pub fn draw(&self, gfx: &Gfx, paint: impl FnOnce(&ID2D1DeviceContext)) -> Result<()> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };
        unsafe {
            let buffer: IDXGISurface = self.swapchain.GetBuffer(0)?;
            let target = gfx.dc.CreateBitmapFromDxgiSurface(&buffer, Some(&props))?;
            gfx.dc.SetTarget(&target);
            gfx.dc.BeginDraw();
            gfx.dc.Clear(Some(&D2D1_COLOR_F::default()));
            paint(&gfx.dc);
            let ended = gfx.dc.EndDraw(None, None);
            gfx.dc.SetTarget(None::<&ID2D1Image>);
            ended?;
            self.swapchain.Present(1, DXGI_PRESENT(0)).ok()
        }
    }
}

pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

pub fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}

pub fn rounded(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}
