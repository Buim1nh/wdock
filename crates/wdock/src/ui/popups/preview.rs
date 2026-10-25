//! Live previews of an app's windows above its dock icon. Click a preview to
//! switch to that window, its close button to close it. The preview lingers
//! briefly after the pointer leaves the dock so it can be reached.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::model::config::Backdrop;
use crate::model::windows::WindowModel;
use crate::platform::wininfo;
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::popups::tiles::{self, Emphasis};
use crate::ui::text;
use crate::ui::thumbs::Thumbnail;

/// The pointer left the dock; close unless it reaches the preview soon.
pub const CMD_LEAVE: usize = 1;
/// The pointer is back on the icon this preview belongs to.
pub const CMD_KEEP: usize = 2;
const TIMER_HIDE: usize = 1;
const HIDE_DELAY_MS: u32 = 350;
const MAX_WINDOWS: usize = 6;
const TILE_WIDTH: f32 = 210.0;
const TILE_HEIGHT: f32 = 124.0 + tiles::TITLE_HEIGHT;
const PADDING: f32 = 8.0;
const GAP: f32 = 6.0;
const CLOSE: f32 = 20.0;
const ANCHOR_GAP: f32 = 10.0;
const GLYPH_CLOSE: &str = "\u{E711}";

#[derive(Clone, Copy, PartialEq)]
enum Hot {
    Tile(usize),
    Close(usize),
}

pub struct PreviewView {
    hwnd: HWND,
    model: Rc<RefCell<WindowModel>>,
    windows: Vec<HWND>,
    thumbs: Vec<Option<Thumbnail>>,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
    anchor: RECT,
    within: RECT,
    scale: f32,
    hot: Option<Hot>,
}

impl PreviewView {
    pub fn new(model: Rc<RefCell<WindowModel>>, mut windows: Vec<HWND>, anchor: RECT, within: RECT, scale: f32) -> Self {
        windows.truncate(MAX_WINDOWS);
        Self {
            hwnd: HWND::default(),
            model,
            windows,
            thumbs: Vec::new(),
            bitmaps: HashMap::new(),
            anchor,
            within,
            scale,
            hot: None,
        }
    }

    fn tile_rect(&self, index: usize) -> D2D_RECT_F {
        let s = self.scale;
        let left = (PADDING + index as f32 * (TILE_WIDTH + GAP)) * s;
        rect(left, PADDING * s, left + TILE_WIDTH * s, (PADDING + TILE_HEIGHT) * s)
    }

    fn close_rect(&self, index: usize) -> D2D_RECT_F {
        let s = self.scale;
        let tile = self.tile_rect(index);
        let mid = tile.bottom - tiles::TITLE_HEIGHT * s / 2.0;
        rect(tile.right - (CLOSE + 6.0) * s, mid - CLOSE * s / 2.0, tile.right - 6.0 * s, mid + CLOSE * s / 2.0)
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hot> {
        let inside = |r: D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        (0..self.windows.len()).find_map(|i| {
            if inside(self.close_rect(i)) {
                Some(Hot::Close(i))
            } else if inside(self.tile_rect(i)) {
                Some(Hot::Tile(i))
            } else {
                None
            }
        })
    }

    fn schedule_hide(&self) {
        unsafe { SetTimer(Some(self.hwnd), TIMER_HIDE, HIDE_DELAY_MS, None) };
    }

    fn cancel_hide(&self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
    }
}

impl View for PreviewView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.thumbs = tiles::register(hwnd, &self.windows);
    }

    /// Centred above the icon, inside its monitor.
    fn frame(&self) -> Option<RECT> {
        if self.windows.is_empty() {
            return None;
        }
        let s = self.scale;
        let n = self.windows.len() as f32;
        let width = ((PADDING * 2.0 + n * TILE_WIDTH + (n - 1.0) * GAP) * s).round() as i32;
        let height = ((PADDING * 2.0 + TILE_HEIGHT) * s).round() as i32;
        let m = self.within;
        let centre = (self.anchor.left + self.anchor.right) / 2;
        let left = (centre - width / 2).clamp(m.left, (m.right - width).max(m.left));
        let bottom = self.anchor.top - (ANCHOR_GAP * s).round() as i32;
        let top = (bottom - height).max(m.top);
        Some(RECT { left, top, right: left + width, bottom: top + height })
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let theme = cx.theme;
        if theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(theme.solid_fill) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(0.0, 0.0, cx.width, cx.height), DWM_CORNER_RADIUS * s), &brush) };
            }
        let model = self.model.borrow();
        for (index, &hwnd) in self.windows.iter().enumerate() {
            let hot_tile = matches!(self.hot, Some(Hot::Tile(i) | Hot::Close(i)) if i == index);
            let emphasis = if hot_tile { Emphasis::Hot } else { Emphasis::None };
            let thumb = self.thumbs.get(index).and_then(Option::as_ref);
            let tile = self.tile_rect(index);
            tiles::paint(cx, &model, &mut self.bitmaps, thumb, hwnd, tile, emphasis);
            if hot_tile {
                // The close button covers the end of the title.
                let close = self.close_rect(index);
                let fill = if self.hot == Some(Hot::Close(index)) { crate::ui::gfx::rgba(232, 17, 35, 1.0) } else { theme.control };
                if let Ok(brush) = cx.gfx.brush(fill) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(close, CLOSE * s / 2.0), &brush) };
                }
                if let Ok(format) = text::format(cx, text::ICON_FONT, 9.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER) {
                    text::draw(cx, GLYPH_CLOSE, &format, close, theme.text);
                }
            }
        }
    }

    fn command(&mut self, id: usize) -> bool {
        match id {
            CMD_LEAVE => self.schedule_hide(),
            CMD_KEEP => self.cancel_hide(),
            _ => {}
        }
        false
    }

    fn timer(&mut self, id: usize) -> bool {
        if id == TIMER_HIDE {
            self.cancel_hide();
            bar::close(self.hwnd);
        }
        false
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.cancel_hide();
        let hot = self.hit(x, y);
        std::mem::replace(&mut self.hot, hot) != hot
    }

    fn mouse_leave(&mut self) -> bool {
        self.schedule_hide();
        self.hot.take().is_some()
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        match self.hit(x, y) {
            Some(Hot::Close(index)) => {
                wininfo::close(self.windows[index]);
                false
            }
            Some(Hot::Tile(index)) => {
                let hwnd = self.windows[index];
                bar::close(self.hwnd);
                self.model.borrow_mut().focus(hwnd);
                false
            }
            None => false,
        }
    }

    fn model_changed(&mut self) -> bool {
        let model = self.model.borrow();
        let keep: Vec<bool> = self.windows.iter().map(|&h| model.placement(h).is_some()).collect();
        drop(model);
        if keep.iter().all(|&k| k) {
            return true; // titles may have changed
        }
        let mut it = keep.iter();
        self.windows.retain(|_| *it.next().unwrap_or(&false));
        let mut it = keep.iter();
        self.thumbs.retain(|_| *it.next().unwrap_or(&false));
        self.hot = None;
        if self.windows.is_empty() {
            bar::close(self.hwnd);
            return false;
        }
        true
    }
}
