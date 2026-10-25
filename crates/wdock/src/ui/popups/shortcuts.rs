//! Shortcut cheat sheet: a small glass card listing wdock's shortcuts next
//! to the Windows ones people forget. It opens in the top-right corner (or
//! where it was last dragged to; drag it anywhere), never takes focus, so it
//! can stay up while you work, and closes with Win+Alt+/, the top bar's
//! keyboard button or its own close button.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, IDWriteTextFormat,
};

use crate::model::config::Backdrop;
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{Gfx, rect, rounded};
use crate::ui::text;

pub struct Group {
    pub title: &'static str,
    /// Keys as "Win+Alt+Space" (split on '+' into keycaps), then what they do.
    pub rows: Vec<(String, &'static str)>,
}

const PADDING: f32 = 16.0;
const TITLE: f32 = 26.0;
const HEADER: f32 = 30.0;
const ROW: f32 = 25.0;
const COLUMN: f32 = 330.0;
const COLUMN_GAP: f32 = 18.0;
const CAP_HEIGHT: f32 = 19.0;
const CAP_PAD: f32 = 6.0;
const CAP_GAP: f32 = 3.0;
const KEY_COLUMN: f32 = 168.0;
const MARGIN: f32 = 12.0;
const CLOSE: f32 = 22.0;
const GLYPH_CLOSE: &str = "\u{E711}";

pub struct ShortcutsView {
    hwnd: HWND,
    groups: Vec<Group>,
    frame: RECT,
    scale: f32,
    close_hot: bool,
    /// While dragging: cursor minus window origin, screen pixels.
    grab: Option<POINT>,
}

fn position_path() -> std::path::PathBuf {
    crate::paths::local_dir().join("shortcuts.txt")
}

/// The top-left corner the card was last dragged to.
pub fn saved_position() -> Option<POINT> {
    let text = std::fs::read_to_string(position_path()).ok()?;
    let mut parts = text.split_whitespace().map(str::parse::<i32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(x)), Some(Ok(y))) => Some(POINT { x, y }),
        _ => None,
    }
}

fn save_position(origin: POINT) {
    let path = position_path();
    let text = format!("{} {}\n", origin.x, origin.y);
    if let Err(err) = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, text)) {
        log::debug!("could not remember the shortcut card's position: {err}");
    }
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    let _ = unsafe { GetCursorPos(&mut p) };
    p
}

impl ShortcutsView {
    /// `monitor` is the monitor rectangle, `top` the y below its top bar.
    pub fn new(_gfx: &Gfx, groups: Vec<Group>, monitor: RECT, top: i32, scale: f32) -> Self {
        let s = scale;
        let rows = groups.iter().map(|g| g.rows.len()).max().unwrap_or(0) as f32;
        let columns = groups.len().max(1) as f32;
        let width = ((PADDING * 2.0 + columns * COLUMN + (columns - 1.0) * COLUMN_GAP) * s).round() as i32;
        let height = ((PADDING * 2.0 + TITLE + HEADER + rows * ROW) * s).round() as i32;
        let margin = (MARGIN * s).round() as i32;
        let left = (monitor.right - width - margin).max(monitor.left);
        let top = top + margin;
        Self {
            hwnd: HWND::default(),
            groups,
            frame: RECT { left, top, right: left + width, bottom: top + height },
            scale,
            close_hot: false,
            grab: None,
        }
    }

    /// The same card with its top-left corner at `origin`, kept on screen.
    pub fn moved_to(mut self, origin: POINT) -> Self {
        self.place(origin);
        self
    }

    /// Moves the frame, keeping it inside the virtual screen (all monitors).
    fn place(&mut self, origin: POINT) {
        let (w, h) = (self.frame.right - self.frame.left, self.frame.bottom - self.frame.top);
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let left = origin.x.clamp(vx, (vx + vw - w).max(vx));
        let top = origin.y.clamp(vy, (vy + vh - h).max(vy));
        self.frame = RECT { left, top, right: left + w, bottom: top + h };
    }

    fn close_rect(&self) -> D2D_RECT_F {
        let s = self.scale;
        let width = (self.frame.right - self.frame.left) as f32;
        let top = (PADDING + (TITLE - CLOSE) / 2.0) * s;
        rect(width - (PADDING + CLOSE) * s, top, width - PADDING * s, top + CLOSE * s)
    }

    fn over_close(&self, x: f32, y: f32) -> bool {
        let r = self.close_rect();
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    }

    /// Draws "Win+Alt+Space" as keycaps from `left`, vertically centred on `mid`.
    fn keycaps(&self, cx: &PaintCx, keys: &str, format: &IDWriteTextFormat, left: f32, mid: f32) {
        let s = cx.scale;
        let mut x = left;
        for key in keys.split('+') {
            let (w, _) = text::measure(&cx.gfx.dwrite, key, format, f32::MAX);
            let cap = rect(x, mid - CAP_HEIGHT * s / 2.0, x + (w + CAP_PAD * 2.0 * s).max(CAP_HEIGHT * s), mid + CAP_HEIGHT * s / 2.0);
            if let Ok(brush) = cx.gfx.brush(cx.theme.control) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(cap, 4.0 * s), &brush) };
            }
            if let Ok(brush) = cx.gfx.brush(cx.theme.border) {
                unsafe { cx.dc.DrawRoundedRectangle(&rounded(cap, 4.0 * s), &brush, 1.0, None) };
            }
            text::draw(cx, key, format, cap, cx.theme.text);
            x = cap.right + CAP_GAP * s;
        }
    }
}

impl View for ShortcutsView {
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
        let (Ok(title), Ok(header), Ok(label), Ok(cap), Ok(icon)) = (
            text::format(cx, text::UI_FONT, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
            text::format(cx, text::UI_FONT, 11.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
            text::format(cx, text::UI_FONT, 12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            text::format(cx, text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER),
            text::format(cx, text::ICON_FONT, 9.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) else {
            return;
        };
        let pad = PADDING * s;
        text::draw(cx, "Keyboard shortcuts", &title, rect(pad, pad, cx.width - pad, pad + TITLE * s), theme.text);
        let close = self.close_rect();
        if self.close_hot
            && let Ok(brush) = cx.gfx.brush(theme.hover) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(close, CLOSE * s / 2.0), &brush) };
            }
        text::draw(cx, GLYPH_CLOSE, &icon, close, theme.text_dim);

        for (column, group) in self.groups.iter().enumerate() {
            let left = pad + column as f32 * (COLUMN + COLUMN_GAP) * s;
            let right = left + COLUMN * s;
            let top = pad + TITLE * s;
            let mut accent = theme.accent;
            accent.a = 0.95;
            text::draw(cx, group.title, &header, rect(left, top, right, top + HEADER * s), accent);
            for (i, (keys, what)) in group.rows.iter().enumerate() {
                let row_top = top + (HEADER + i as f32 * ROW) * s;
                let mid = row_top + ROW * s / 2.0;
                self.keycaps(cx, keys, &cap, left, mid);
                let text_left = left + KEY_COLUMN * s;
                text::draw(cx, what, &label, rect(text_left, row_top, right, row_top + ROW * s), theme.text_dim);
            }
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if let Some(grab) = self.grab {
            // Screen coordinates: the window moves under the pointer.
            let p = cursor();
            let before = self.frame;
            self.place(POINT { x: p.x - grab.x, y: p.y - grab.y });
            return self.frame != before;
        }
        let hot = self.over_close(x, y);
        std::mem::replace(&mut self.close_hot, hot) != hot
    }

    fn mouse_leave(&mut self) -> bool {
        std::mem::take(&mut self.close_hot)
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button == Button::Left && !self.over_close(x, y) {
            let p = cursor();
            self.grab = Some(POINT { x: p.x - self.frame.left, y: p.y - self.frame.top });
            unsafe { SetCapture(self.hwnd) };
        }
        false
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        if self.grab.take().is_some() {
            let _ = unsafe { ReleaseCapture() };
            save_position(POINT { x: self.frame.left, y: self.frame.top });
            return false;
        }
        if self.over_close(x, y) {
            bar::close(self.hwnd);
            // The top bar's keyboard button stops highlighting.
            crate::app::notify_model();
        }
        false
    }
}
