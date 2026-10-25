//! Grid of tray icons that did not fit on the top bar. Takes focus so it can
//! close itself when the user clicks elsewhere (or an app's menu opens).

use windows::Win32::Foundation::{HWND, RECT};

use crate::model::config::{Backdrop, TrayConfig};
use crate::platform::tray_host::with_tray;
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::tray_strip::TrayStrip;

const CELL: f32 = 32.0;
const PADDING: f32 = 6.0;
const COLUMNS: usize = 5;

pub struct OverflowView {
    config: TrayConfig,
    anchor: RECT,
    monitor: RECT,
    scale: f32,
    ids: Vec<u64>,
    revision: u64,
    strip: TrayStrip,
}

impl OverflowView {
    pub fn new(config: TrayConfig, anchor: RECT, monitor: RECT, scale: f32) -> Self {
        let mut view = Self { config, anchor, monitor, scale, ids: Vec::new(), revision: u64::MAX, strip: TrayStrip::default() };
        view.refresh();
        view
    }

    /// Re-reads overflow icons; returns whether anything changed.
    fn refresh(&mut self) -> bool {
        let Some((revision, ids)) = with_tray(|tray| {
            let (_, extra) = tray.split(self.config.max_visible, &self.config.overflow);
            (tray.revision(), extra.iter().map(|i| i.id).collect::<Vec<_>>())
        }) else {
            return false;
        };
        if revision == self.revision {
            return false;
        }
        self.revision = revision;
        self.ids = ids;
        let s = self.scale;
        let slots = self
            .ids
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let (col, row) = ((i % COLUMNS) as f32, (i / COLUMNS) as f32);
                let left = (PADDING + col * CELL) * s;
                let top = (PADDING + row * CELL) * s;
                (id, rect(left, top, left + CELL * s, top + CELL * s))
            })
            .collect();
        self.strip.set_slots(slots);
        true
    }

    fn grid(&self) -> (usize, usize) {
        let n = self.ids.len().max(1);
        (n.min(COLUMNS), n.div_ceil(COLUMNS))
    }
}

impl View for OverflowView {
    fn attach(&mut self, hwnd: HWND) {
        self.strip.attach(hwnd, self.scale);
    }

    /// Right-aligned under the chevron.
    fn frame(&self) -> Option<RECT> {
        let (cols, rows) = self.grid();
        let s = self.scale;
        let width = ((PADDING * 2.0 + cols as f32 * CELL) * s).round() as i32;
        let height = ((PADDING * 2.0 + rows as f32 * CELL) * s).round() as i32;
        let left = (self.anchor.right - width).max(self.monitor.left);
        let top = self.anchor.bottom + (6.0 * s).round() as i32;
        Some(RECT { left, top, right: left + width, bottom: top + height })
    }

    fn model_changed(&mut self) -> bool {
        self.refresh()
    }

    fn paint(&mut self, cx: &PaintCx) {
        if cx.theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(cx.theme.solid_fill) {
                let bounds = rect(0.0, 0.0, cx.width, cx.height);
                unsafe { cx.dc.FillRoundedRectangle(&rounded(bounds, DWM_CORNER_RADIUS * cx.scale), &brush) };
            }
        let strip = &mut self.strip;
        with_tray(|tray| strip.paint(cx, tray));
        self.strip.publish_rects();
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.strip.mouse_move(x, y)
    }

    fn mouse_leave(&mut self) -> bool {
        self.strip.mouse_leave()
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        self.strip.mouse_down(x, y, button)
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        self.strip.mouse_up(x, y, button)
    }

    fn double_click(&mut self, x: f32, y: f32, button: Button) -> bool {
        self.strip.double_click(x, y, button)
    }

    fn timer(&mut self, id: usize) -> bool {
        self.strip.timer(id)
    }

    fn deactivated(&mut self) -> bool {
        self.strip.mouse_leave();
        true
    }
}
