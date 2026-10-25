//! Alt+Tab replacement: live previews of the windows on visible workspaces,
//! most recent first. Tab / Shift+Tab (hotkeys) step, releasing Alt switches,
//! Esc cancels. Alt is polled while open, so the release is caught even if
//! focus never reached us; nothing runs once the switcher closes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_ESCAPE, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT,
};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::model::config::Backdrop;
use crate::model::windows::WindowModel;
use crate::platform::monitors::Monitor;
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{Gfx, rect, rounded};
use crate::ui::popups::tiles::{self, Emphasis};
use crate::ui::thumbs::Thumbnail;

pub const CMD_NEXT: usize = 1;
pub const CMD_PREV: usize = 2;
const TIMER_ALT: usize = 1;
const ALT_POLL_MS: u32 = 30;
const TILE_WIDTH: f32 = 220.0;
const TILE_HEIGHT: f32 = 132.0 + tiles::TITLE_HEIGHT;
const PADDING: f32 = 14.0;
const GAP: f32 = 8.0;

pub struct SwitcherView {
    hwnd: HWND,
    model: Rc<RefCell<WindowModel>>,
    windows: Vec<HWND>,
    thumbs: Vec<Option<Thumbnail>>,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
    selected: usize,
    hot: Option<usize>,
    columns: usize,
    frame: RECT,
    scale: f32,
    done: bool,
    alt_seen: bool,
}

impl SwitcherView {
    pub fn new(_gfx: &Gfx, model: Rc<RefCell<WindowModel>>, mut windows: Vec<HWND>, step: i32, monitor: &Monitor) -> Self {
        let s = monitor.scale;
        let m = monitor.rect;
        let fits = |available: i32, tile: f32| {
            (((available as f32 * 0.9 / s) - PADDING * 2.0 + GAP) / (tile + GAP)).floor().max(1.0) as usize
        };
        let max_columns = fits(m.right - m.left, TILE_WIDTH);
        let max_rows = fits(m.bottom - m.top, TILE_HEIGHT);
        windows.truncate(max_columns * max_rows);
        let count = windows.len();
        let columns = count.min(max_columns).max(1);
        let rows = count.div_ceil(columns).max(1);

        let width = ((PADDING * 2.0 + columns as f32 * TILE_WIDTH + (columns - 1) as f32 * GAP) * s).round() as i32;
        let height = ((PADDING * 2.0 + rows as f32 * TILE_HEIGHT + (rows - 1) as f32 * GAP) * s).round() as i32;
        let left = m.left + (m.right - m.left - width) / 2;
        let top = m.top + (m.bottom - m.top - height) / 2;
        // Tab from the current window goes to the previous one; Shift+Tab to the oldest.
        let selected = match (count, step) {
            (1, _) => 0,
            (_, s) if s < 0 => count - 1,
            _ => 1,
        };
        Self {
            hwnd: HWND::default(),
            model,
            windows,
            thumbs: Vec::new(),
            bitmaps: HashMap::new(),
            selected,
            hot: None,
            columns,
            frame: RECT { left, top, right: left + width, bottom: top + height },
            scale: s,
            done: false,
            alt_seen: false,
        }
    }

    fn tile_rect(&self, index: usize) -> D2D_RECT_F {
        let s = self.scale;
        let (column, row) = (index % self.columns, index / self.columns);
        let left = (PADDING + column as f32 * (TILE_WIDTH + GAP)) * s;
        let top = (PADDING + row as f32 * (TILE_HEIGHT + GAP)) * s;
        rect(left, top, left + TILE_WIDTH * s, top + TILE_HEIGHT * s)
    }

    fn tile_at(&self, x: f32, y: f32) -> Option<usize> {
        (0..self.windows.len()).find(|&i| {
            let r = self.tile_rect(i);
            x >= r.left && x < r.right && y >= r.top && y < r.bottom
        })
    }

    /// Closes and activates `target` (the selection, or nothing on cancel).
    fn finish(&mut self, target: Option<usize>) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ALT);
        }
        bar::close(self.hwnd);
        // On cancel, hand focus back to the window that had it.
        let hwnd = self.windows[target.unwrap_or(0)];
        self.model.borrow_mut().focus(hwnd);
    }

    fn step(&mut self, forward: bool) -> bool {
        let count = self.windows.len();
        self.selected = if forward { (self.selected + 1) % count } else { (self.selected + count - 1) % count };
        true
    }
}

impl View for SwitcherView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.thumbs = tiles::register(hwnd, &self.windows);
        unsafe { SetTimer(Some(hwnd), TIMER_ALT, ALT_POLL_MS, None) };
    }

    fn frame(&self) -> Option<RECT> {
        Some(self.frame)
    }

    fn paint(&mut self, cx: &PaintCx) {
        if cx.theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(cx.theme.solid_fill) {
                let all = rounded(rect(0.0, 0.0, cx.width, cx.height), DWM_CORNER_RADIUS * cx.scale);
                unsafe { cx.dc.FillRoundedRectangle(&all, &brush) };
            }
        let model = self.model.borrow();
        for (index, &hwnd) in self.windows.iter().enumerate() {
            let emphasis = if index == self.selected {
                Emphasis::Selected
            } else if self.hot == Some(index) {
                Emphasis::Hot
            } else {
                Emphasis::None
            };
            let thumb = self.thumbs.get(index).and_then(Option::as_ref);
            let tile = self.tile_rect(index);
            tiles::paint(cx, &model, &mut self.bitmaps, thumb, hwnd, tile, emphasis);
        }
    }

    fn command(&mut self, id: usize) -> bool {
        match id {
            CMD_NEXT => self.step(true),
            CMD_PREV => self.step(false),
            _ => false,
        }
    }

    fn timer(&mut self, id: usize) -> bool {
        if id != TIMER_ALT {
            return false;
        }
        let alt_down = unsafe { GetAsyncKeyState(VK_MENU.0.into()) } as u16 & 0x8000 != 0;
        self.alt_seen |= alt_down;
        // Debug builds with WDOCK_TEST_INPUT stay open for posted test input
        // until Alt has actually been pressed.
        let testing = cfg!(debug_assertions) && std::env::var_os("WDOCK_TEST_INPUT").is_some();
        if !alt_down && (self.alt_seen || !testing) {
            self.finish(Some(self.selected));
        }
        false
    }

    fn key_down(&mut self, vk: u16) -> bool {
        match vk {
            v if v == VK_ESCAPE.0 => {
                self.finish(None);
                false
            }
            v if v == VK_RETURN.0 => {
                self.finish(Some(self.selected));
                false
            }
            v if v == VK_RIGHT.0 => self.step(true),
            v if v == VK_LEFT.0 => self.step(false),
            _ => false,
        }
    }

    fn key_up(&mut self, vk: u16) -> bool {
        if vk == VK_MENU.0 {
            self.finish(Some(self.selected));
        }
        false
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hot = self.tile_at(x, y);
        std::mem::replace(&mut self.hot, hot) != hot
    }

    fn mouse_leave(&mut self) -> bool {
        self.hot.take().is_some()
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button == Button::Left
            && let Some(index) = self.tile_at(x, y) {
                self.finish(Some(index));
            }
        false
    }

    fn model_changed(&mut self) -> bool {
        // Windows that closed while open drop out; the rest keep their slots.
        let model = self.model.borrow();
        let before = self.windows.len();
        let mut keep = self.windows.iter().map(|&h| model.placement(h).is_some()).collect::<Vec<_>>().into_iter();
        drop(model);
        let mut keep_thumbs = keep.clone();
        self.windows.retain(|_| keep.next().unwrap_or(false));
        self.thumbs.retain(|_| keep_thumbs.next().unwrap_or(false));
        if self.windows.is_empty() {
            self.done = true;
            bar::close(self.hwnd);
            return false;
        }
        self.selected = self.selected.min(self.windows.len() - 1);
        self.windows.len() != before
    }

    fn deactivated(&mut self) -> bool {
        self.done = true;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ALT);
        }
        true
    }
}
