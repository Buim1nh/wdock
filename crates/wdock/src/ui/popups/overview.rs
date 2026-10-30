//! Overview (top bar button): every window of the current workspace on one monitor,
//! laid out as live thumbnails, under a strip of that monitor's workspaces.
//! Click a window to switch to it, a workspace to show it, drag a window onto
//! a workspace to move it there. Esc, the button again or a click on empty space closes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};

use crate::model::windows::WindowModel;
use crate::platform::monitors::Monitor;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rgba, rounded};
use crate::ui::popups::tiles::{self, Emphasis};
use crate::ui::text;
use crate::ui::thumbs::Thumbnail;

const STRIP_TOP: f32 = 28.0;
const STRIP_TILE_W: f32 = 132.0;
const STRIP_TILE_H: f32 = 76.0;
const STRIP_GAP: f32 = 14.0;
const GRID_MARGIN_X: f32 = 64.0;
const GRID_GAP: f32 = 24.0;
const DRAG_THRESHOLD: f32 = 6.0;
/// Width of the thumbnail that follows the pointer while dragging.
const DRAG_WIDTH: f32 = 220.0;

#[derive(Clone, Copy, PartialEq)]
enum Hot {
    Window(usize),
    Workspace(u8),
}

struct Drag {
    index: usize,
    origin: (f32, f32),
    at: (f32, f32),
    active: bool,
}

pub struct OverviewView {
    hwnd: HWND,
    model: Rc<RefCell<WindowModel>>,
    monitor: Monitor,
    workspace: u8,
    windows: Vec<HWND>,
    thumbs: Vec<Option<Thumbnail>>,
    cells: Vec<D2D_RECT_F>,
    strip: Vec<D2D_RECT_F>,
    occupancy: Vec<usize>,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
    hot: Option<Hot>,
    drag: Option<Drag>,
}

impl OverviewView {
    pub fn new(model: Rc<RefCell<WindowModel>>, monitor: Monitor) -> Self {
        let mut view = Self {
            hwnd: HWND::default(),
            model,
            monitor,
            workspace: 0,
            windows: Vec::new(),
            thumbs: Vec::new(),
            cells: Vec::new(),
            strip: Vec::new(),
            occupancy: Vec::new(),
            bitmaps: HashMap::new(),
            hot: None,
            drag: None,
        };
        view.reload();
        view
    }

    /// Re-reads the workspace from the model and lays everything out.
    fn reload(&mut self) {
        let model = self.model.borrow();
        self.workspace = model.active_workspace(self.monitor.handle);
        self.occupancy = model.occupancy(self.monitor.handle);
        let windows = model.workspace_windows(self.monitor.handle, self.workspace);
        drop(model);
        if windows != self.windows {
            self.windows = windows;
            self.thumbs = if self.hwnd.is_invalid() { Vec::new() } else { tiles::register(self.hwnd, &self.windows) };
        }
        self.layout();
    }

    fn layout(&mut self) {
        let s = self.monitor.scale;
        let width = (self.monitor.rect.right - self.monitor.rect.left) as f32;
        let height = (self.monitor.rect.bottom - self.monitor.rect.top) as f32;

        let count = self.occupancy.len() as f32;
        let strip_width = (count * STRIP_TILE_W + (count - 1.0).max(0.0) * STRIP_GAP) * s;
        let mut x = (width - strip_width) / 2.0;
        self.strip = (0..self.occupancy.len())
            .map(|_| {
                let r = rect(x, STRIP_TOP * s, x + STRIP_TILE_W * s, (STRIP_TOP + STRIP_TILE_H) * s);
                x += (STRIP_TILE_W + STRIP_GAP) * s;
                r
            })
            .collect();

        // Pick the column count giving the largest 16:10 tiles.
        let area = rect(GRID_MARGIN_X * s, (STRIP_TOP + STRIP_TILE_H + 40.0) * s, width - GRID_MARGIN_X * s, height - 48.0 * s);
        let n = self.windows.len();
        let gap = GRID_GAP * s;
        let (aw, ah) = (area.right - area.left, area.bottom - area.top);
        let mut best = (1, 0.0f32);
        for columns in 1..=n.max(1) {
            let rows = n.div_ceil(columns).max(1);
            let cw = (aw - gap * (columns - 1) as f32) / columns as f32;
            let ch = (ah - gap * (rows - 1) as f32) / rows as f32 - tiles::TITLE_HEIGHT * s;
            let size = cw.min(ch * 1.6);
            if size > best.1 {
                best = (columns, size);
            }
        }
        let columns = best.0;
        let rows = n.div_ceil(columns).max(1);
        let cell_w = ((aw - gap * (columns - 1) as f32) / columns as f32).min(520.0 * s);
        let cell_h = ((ah - gap * (rows - 1) as f32) / rows as f32).min(cell_w / 1.6 + tiles::TITLE_HEIGHT * s);
        let grid_h = rows as f32 * cell_h + (rows - 1) as f32 * gap;
        let top = area.top + (ah - grid_h) / 2.0;
        self.cells = (0..n)
            .map(|i| {
                let (row, column) = (i / columns, i % columns);
                // Centre each row, including a short last one.
                let in_row = if row == rows - 1 { n - row * columns } else { columns };
                let row_w = in_row as f32 * cell_w + (in_row - 1) as f32 * gap;
                let left = area.left + (aw - row_w) / 2.0 + column as f32 * (cell_w + gap);
                let y = top + row as f32 * (cell_h + gap);
                rect(left, y, left + cell_w, y + cell_h)
            })
            .collect();
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hot> {
        let inside = |r: &D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if let Some(i) = self.strip.iter().position(inside) {
            return Some(Hot::Workspace(i as u8));
        }
        self.cells.iter().position(inside).map(Hot::Window)
    }

    fn switch_to(&mut self, workspace: u8) {
        // The overview keeps focus; picking a window afterwards activates it.
        let _ = self.model.borrow_mut().switch_workspace(self.monitor.handle, workspace);
        crate::app::notify_model();
        self.reload();
    }
}

impl View for OverviewView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.thumbs = tiles::register(hwnd, &self.windows);
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let theme = cx.theme;
        // Dim the blurred desktop so thumbnails stand out in either theme.
        if let Ok(brush) = cx.gfx.brush(rgba(0, 0, 0, if theme.dark { 0.35 } else { 0.18 })) {
            unsafe { cx.dc.FillRectangle(&rect(0.0, 0.0, cx.width, cx.height), &brush) };
        }

        let (Ok(number), Ok(detail), Ok(empty)) = (
            text::format(cx, text::UI_FONT, 20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER),
            text::format(cx, text::UI_FONT, 11.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            text::format(cx, text::UI_FONT, 15.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) else {
            return;
        };
        let drop_target = match (&self.drag, self.hot) {
            (Some(d), Some(Hot::Workspace(w))) if d.active => Some(w),
            _ => None,
        };
        for (i, r) in self.strip.iter().enumerate() {
            let ws = i as u8;
            let color = if ws == self.workspace {
                let mut c = theme.accent;
                c.a = 0.55;
                c
            } else if drop_target == Some(ws) || self.hot == Some(Hot::Workspace(ws)) {
                theme.hover
            } else {
                theme.control
            };
            if let Ok(brush) = cx.gfx.brush(color) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(*r, 10.0 * s), &brush) };
            }
            if drop_target == Some(ws)
                && let Ok(brush) = cx.gfx.brush(theme.accent) {
                    unsafe { cx.dc.DrawRoundedRectangle(&rounded(*r, 10.0 * s), &brush, 2.0 * s, None) };
                }
            let mid = r.top + (r.bottom - r.top) * 0.45;
            text::draw(cx, &(i + 1).to_string(), &number, rect(r.left, r.top, r.right, mid + 6.0 * s), theme.text);
            let count = self.occupancy.get(i).copied().unwrap_or(0);
            let label = match count {
                0 => "Empty".to_owned(),
                1 => "1 window".to_owned(),
                n => format!("{n} windows"),
            };
            text::draw(cx, &label, &detail, rect(r.left, mid + 4.0 * s, r.right, r.bottom - 6.0 * s), theme.text_dim);
        }

        if self.windows.is_empty() {
            let area = rect(0.0, (STRIP_TOP + STRIP_TILE_H) * s, cx.width, cx.height);
            text::draw(cx, "No windows on this workspace", &empty, area, theme.text_dim);
            return;
        }
        let model = self.model.borrow();
        for (index, &hwnd) in self.windows.iter().enumerate() {
            let thumb = self.thumbs.get(index).and_then(Option::as_ref);
            match &self.drag {
                // The dragged window's thumbnail follows the pointer instead.
                Some(d) if d.active && d.index == index => {
                    if let Some(t) = thumb {
                        let (w, h) = (DRAG_WIDTH * s, DRAG_WIDTH * s / 1.6);
                        let follow = rect(d.at.0 - w / 2.0, d.at.1 - h / 2.0, d.at.0 + w / 2.0, d.at.1 + h / 2.0);
                        if t.place(follow, 220).is_none() {
                            t.hide();
                        }
                    }
                }
                _ => {
                    let emphasis = if self.hot == Some(Hot::Window(index)) { Emphasis::Hot } else { Emphasis::None };
                    tiles::paint(cx, &model, &mut self.bitmaps, thumb, hwnd, self.cells[index], emphasis);
                }
            }
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let threshold = DRAG_THRESHOLD * self.monitor.scale;
        if let Some(drag) = &mut self.drag {
            drag.at = (x, y);
            if !drag.active && ((x - drag.origin.0).abs() > threshold || (y - drag.origin.1).abs() > threshold) {
                drag.active = true;
                unsafe { SetCapture(self.hwnd) };
            }
        }
        let hot = self.hit(x, y);
        let dragging = self.drag.as_ref().is_some_and(|d| d.active);
        std::mem::replace(&mut self.hot, hot) != hot || dragging
    }

    fn mouse_leave(&mut self) -> bool {
        self.hot.take().is_some()
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button == Button::Left
            && let Some(Hot::Window(index)) = self.hit(x, y) {
                self.drag = Some(Drag { index, origin: (x, y), at: (x, y), active: false });
            }
        false
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        let drag = self.drag.take();
        if drag.as_ref().is_some_and(|d| d.active) {
            let _ = unsafe { ReleaseCapture() };
        }
        match (drag, self.hit(x, y)) {
            (Some(d), Some(Hot::Workspace(ws))) if d.active => {
                let hwnd = self.windows[d.index];
                if self.model.borrow_mut().move_window(hwnd, ws) {
                    crate::app::notify_model();
                }
                self.reload();
                true
            }
            (Some(d), _) if d.active => true,
            (_, Some(Hot::Window(index))) => {
                let hwnd = self.windows[index];
                bar::close(self.hwnd);
                self.model.borrow_mut().focus(hwnd);
                false
            }
            (_, Some(Hot::Workspace(ws))) => {
                self.switch_to(ws);
                true
            }
            (_, None) => {
                bar::close(self.hwnd);
                false
            }
        }
    }

    fn key_down(&mut self, vk: u16) -> bool {
        if vk == VK_ESCAPE.0 {
            bar::close(self.hwnd);
            return false;
        }
        // 1-9 show that workspace.
        if (u16::from(b'1')..=u16::from(b'9')).contains(&vk) {
            let ws = (vk - u16::from(b'1')) as u8;
            if usize::from(ws) < self.occupancy.len() {
                self.switch_to(ws);
                return true;
            }
        }
        false
    }

    fn model_changed(&mut self) -> bool {
        self.reload();
        true
    }

    fn deactivated(&mut self) -> bool {
        true
    }
}
