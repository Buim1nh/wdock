//! A small context menu (dock icons). Items run their action after the menu
//! closes; Esc or clicking elsewhere dismisses it.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP};

use crate::model::config::Backdrop;
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{Gfx, rect, rounded};
use crate::ui::text;

const PADDING: f32 = 5.0;
const ROW: f32 = 28.0;
const SEPARATOR: f32 = 9.0;
const TEXT_INSET: f32 = 12.0;
const MIN_WIDTH: f32 = 180.0;
const MAX_WIDTH: f32 = 340.0;
const FONT: f32 = 13.0;
/// Gap between the anchor point and the menu's bottom edge.
const ANCHOR_GAP: f32 = 10.0;

pub enum MenuItem {
    Action { label: String, run: Option<Box<dyn FnOnce()>> },
    /// Dim, unclickable text such as the app name.
    Header(String),
    Separator,
}

impl MenuItem {
    pub fn action(label: impl Into<String>, run: impl FnOnce() + 'static) -> Self {
        Self::Action { label: label.into(), run: Some(Box::new(run)) }
    }

    fn height(&self) -> f32 {
        match self {
            Self::Separator => SEPARATOR,
            _ => ROW,
        }
    }

    fn selectable(&self) -> bool {
        matches!(self, Self::Action { .. })
    }
}

pub struct MenuView {
    hwnd: HWND,
    items: Vec<MenuItem>,
    frame: RECT,
    scale: f32,
    selected: Option<usize>,
}

impl MenuView {
    pub fn new(gfx: &Gfx, items: Vec<MenuItem>, anchor: POINT, within: RECT, scale: f32) -> Self {
        let s = scale;
        let widest = text::format_px(&gfx.dwrite, text::UI_FONT, FONT * s, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, false)
            .map(|format| {
                items
                    .iter()
                    .filter_map(|item| match item {
                        MenuItem::Action { label, .. } | MenuItem::Header(label) => {
                            Some(text::measure(&gfx.dwrite, label, &format, f32::MAX).0)
                        }
                        MenuItem::Separator => None,
                    })
                    .fold(0.0, f32::max)
            })
            .unwrap_or(0.0);
        let width_dip = (widest / s + TEXT_INSET * 2.0 + PADDING * 2.0).clamp(MIN_WIDTH, MAX_WIDTH);
        let height_dip = PADDING * 2.0 + items.iter().map(MenuItem::height).sum::<f32>();
        let (width, height) = ((width_dip * s).round() as i32, (height_dip * s).round() as i32);
        let left = (anchor.x - width / 2).clamp(within.left, (within.right - width).max(within.left));
        let bottom = anchor.y - (ANCHOR_GAP * s).round() as i32;
        let top = (bottom - height).clamp(within.top, (within.bottom - height).max(within.top));
        Self { hwnd: HWND::default(), items, frame: RECT { left, top, right: left + width, bottom: top + height }, scale, selected: None }
    }

    fn row_rects(&self) -> Vec<D2D_RECT_F> {
        let s = self.scale;
        let width = (self.frame.right - self.frame.left) as f32;
        let mut y = PADDING * s;
        self.items
            .iter()
            .map(|item| {
                let r = rect(PADDING * s, y, width - PADDING * s, y + item.height() * s);
                y = r.bottom;
                r
            })
            .collect()
    }

    fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        self.row_rects()
            .iter()
            .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .filter(|&i| self.items[i].selectable())
    }

    fn run(&mut self, index: usize) {
        if let Some(MenuItem::Action { run, .. }) = self.items.get_mut(index)
            && let Some(run) = run.take() {
                bar::close(self.hwnd);
                run();
            }
    }

    fn step(&mut self, forward: bool) -> bool {
        let selectable: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].selectable()).collect();
        if selectable.is_empty() {
            return false;
        }
        let position = self.selected.and_then(|s| selectable.iter().position(|&i| i == s));
        let next = match (position, forward) {
            (None, true) => 0,
            (None, false) => selectable.len() - 1,
            (Some(p), true) => (p + 1) % selectable.len(),
            (Some(p), false) => (p + selectable.len() - 1) % selectable.len(),
        };
        self.selected = Some(selectable[next]);
        true
    }
}

impl View for MenuView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }

    fn frame(&self) -> Option<RECT> {
        Some(self.frame)
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let theme = cx.theme;
        if theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(theme.solid_fill) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(0.0, 0.0, cx.width, cx.height), DWM_CORNER_RADIUS * s), &brush) };
            }
        let (Ok(normal), Ok(header)) = (
            text::format(cx, text::UI_FONT, FONT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            text::format(cx, text::UI_FONT, FONT, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
        ) else {
            return;
        };
        for (index, (item, row)) in self.items.iter().zip(self.row_rects()).enumerate() {
            let label_rect = rect(row.left + TEXT_INSET * s, row.top, row.right - TEXT_INSET * s, row.bottom);
            match item {
                MenuItem::Separator => {
                    if let Ok(brush) = cx.gfx.brush(theme.border) {
                        let y = ((row.top + row.bottom) / 2.0).round();
                        let line = rect(row.left + 6.0 * s, y, row.right - 6.0 * s, y + s.max(1.0));
                        unsafe { cx.dc.FillRectangle(&line, &brush) };
                    }
                }
                MenuItem::Header(label) => text::draw(cx, label, &header, label_rect, theme.text_dim),
                MenuItem::Action { label, .. } => {
                    let selected = self.selected == Some(index);
                    if selected
                        && let Ok(brush) = cx.gfx.brush(theme.accent) {
                            unsafe { cx.dc.FillRoundedRectangle(&rounded(row, 5.0 * s), &brush) };
                        }
                    text::draw(cx, label, &normal, label_rect, if selected { theme.on_accent } else { theme.text });
                }
            }
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let selected = self.item_at(x, y);
        std::mem::replace(&mut self.selected, selected) != selected
    }

    fn mouse_leave(&mut self) -> bool {
        self.selected.take().is_some()
    }

    fn mouse_up(&mut self, x: f32, y: f32, _button: Button) -> bool {
        if let Some(index) = self.item_at(x, y) {
            self.run(index);
        }
        false
    }

    fn key_down(&mut self, vk: u16) -> bool {
        match vk {
            v if v == VK_ESCAPE.0 => {
                bar::close(self.hwnd);
                false
            }
            v if v == VK_RETURN.0 => {
                if let Some(index) = self.selected {
                    self.run(index);
                }
                false
            }
            v if v == VK_DOWN.0 => self.step(true),
            v if v == VK_UP.0 => self.step(false),
            _ => false,
        }
    }

    fn deactivated(&mut self) -> bool {
        true
    }
}
