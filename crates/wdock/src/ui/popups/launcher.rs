//! Spotlight-style launcher: a search field and up to eight results, centred
//! near the top of the monitor under the cursor. Type to search, arrows to
//! move, Enter to open, Esc to close. Sign out, restart and shut down need a
//! second Enter.

use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_NEXT, VK_PRIOR, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::model::config::Backdrop;
use crate::model::search::{Candidate, Target};
use crate::platform::launch;
use crate::platform::session::{self, SessionAction};
use crate::services::apps_index;
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::glyphs;
use crate::ui::text;

const WIDTH: f32 = 620.0;
const PADDING: f32 = 8.0;
const FIELD: f32 = 52.0;
const ROW: f32 = 46.0;
/// A size icons ship as natively (16/24/32/48), so they stay crisp.
const ICON: f32 = 24.0;
const MAX_RESULTS: usize = 8;
const TIMER_CARET: usize = 1;
const CARET_MS: u32 = 530;

pub struct LauncherView {
    hwnd: HWND,
    monitor: RECT,
    scale: f32,
    query: String,
    results: Vec<Candidate>,
    selected: usize,
    hot: Option<usize>,
    caret: bool,
    armed: Option<SessionAction>,
    bitmaps: HashMap<String, ID2D1Bitmap1>,
}

impl LauncherView {
    pub fn new(monitor: RECT, scale: f32) -> Self {
        let mut view = Self {
            hwnd: HWND::default(),
            monitor,
            scale,
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            hot: None,
            caret: true,
            armed: None,
            bitmaps: HashMap::new(),
        };
        view.update_results();
        view
    }

    fn update_results(&mut self) {
        self.results = apps_index::find(&self.query, MAX_RESULTS);
        self.selected = 0;
        self.hot = None;
        self.armed = None;
    }

    fn row_rect(&self, index: usize) -> D2D_RECT_F {
        let s = self.scale;
        let top = (PADDING + FIELD + 6.0 + index as f32 * ROW) * s;
        rect(PADDING * s, top, (WIDTH - PADDING) * s, top + ROW * s)
    }

    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        (0..self.results.len()).find(|&i| {
            let r = self.row_rect(i);
            x >= r.left && x < r.right && y >= r.top && y < r.bottom
        })
    }

    fn activate(&mut self, index: usize) {
        let Some(candidate) = self.results.get(index).cloned() else {
            return;
        };
        if let Target::Session(action) = candidate.target
            && action.needs_confirmation() && self.armed != Some(action) {
                self.armed = Some(action);
                self.selected = index;
                return;
            }
        apps_index::record_launch(&candidate);
        bar::close(self.hwnd);
        match &candidate.target {
            Target::App(target) | Target::Setting(target) => launch::open(target),
            Target::Session(action) => session::perform(*action),
        }
    }

    fn subtitle(&self, candidate: &Candidate) -> &'static str {
        match candidate.target {
            Target::App(_) => "App",
            Target::Setting(_) => "Settings",
            Target::Session(action) if self.armed == Some(action) => "Press Enter again to confirm",
            Target::Session(_) => "System",
        }
    }

    fn restart_caret(&mut self) {
        self.caret = true;
        unsafe { SetTimer(Some(self.hwnd), TIMER_CARET, CARET_MS, None) };
    }
}

fn session_glyph(action: SessionAction) -> &'static str {
    match action {
        SessionAction::Lock => glyphs::LOCK,
        SessionAction::Sleep => glyphs::SLEEP,
        SessionAction::SignOut => glyphs::SIGN_OUT,
        SessionAction::Restart => glyphs::RESTART,
        SessionAction::ShutDown => glyphs::POWER,
    }
}

impl View for LauncherView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.restart_caret();
    }

    /// Centred horizontally, a fifth of the way down the monitor.
    fn frame(&self) -> Option<RECT> {
        let s = self.scale;
        let rows = self.results.len() as f32;
        let list = if rows > 0.0 { 6.0 + rows * ROW } else { 0.0 };
        let width = (WIDTH * s).round() as i32;
        let height = ((PADDING * 2.0 + FIELD + list) * s).round() as i32;
        let m = self.monitor;
        let left = m.left + (m.right - m.left - width) / 2;
        let top = m.top + (m.bottom - m.top) / 5;
        Some(RECT { left, top, right: left + width, bottom: top + height })
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let theme = cx.theme;
        if theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(theme.solid_fill) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(0.0, 0.0, cx.width, cx.height), DWM_CORNER_RADIUS * s), &brush) };
            }
        let format = |family, size: f32, weight, align| text::format_px(&cx.gfx.dwrite, family, size * s, weight, align, false);
        let (Ok(field), Ok(name), Ok(detail), Ok(icon), Ok(search_icon)) = (
            format(text::UI_FONT, 19.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 13.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 11.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::ICON_FONT, 16.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            format(text::ICON_FONT, 18.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) else {
            return;
        };
        let fill = |r: D2D_RECT_F, radius: f32, color| {
            if let Ok(brush) = cx.gfx.brush(color) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, radius), &brush) };
            }
        };

        // Search field.
        let pad = PADDING * s;
        let field_rect = rect(pad, pad, cx.width - pad, pad + FIELD * s);
        let glyph_rect = rect(field_rect.left, field_rect.top, field_rect.left + 44.0 * s, field_rect.bottom);
        text::draw(cx, glyphs::SEARCH, &search_icon, glyph_rect, theme.text_dim);
        let text_rect = rect(glyph_rect.right, field_rect.top, field_rect.right - 12.0 * s, field_rect.bottom);
        if self.query.is_empty() {
            text::draw(cx, "Search apps and settings", &field, text_rect, theme.text_dim);
        } else {
            text::draw(cx, &self.query, &field, text_rect, theme.text);
        }
        if self.caret {
            let (typed, _) = text::measure(&cx.gfx.dwrite, &self.query, &field, f32::MAX);
            let x = (text_rect.left + typed + 1.0 * s).round();
            let caret = rect(x, field_rect.top + 15.0 * s, x + s.max(1.0), field_rect.bottom - 15.0 * s);
            fill(caret, 0.0, theme.accent);
        }
        if !self.results.is_empty()
            && let Ok(brush) = cx.gfx.brush(theme.border) {
                let y = field_rect.bottom + 2.0 * s;
                unsafe { cx.dc.FillRectangle(&rect(pad, y, cx.width - pad, y + s.max(1.0)), &brush) };
            }

        // Results.
        let icon_px = (ICON * s).round() as u32;
        for (index, candidate) in self.results.iter().enumerate() {
            let row = self.row_rect(index);
            let selected = index == self.selected;
            if selected {
                fill(row, 8.0 * s, theme.accent);
            } else if self.hot == Some(index) {
                fill(row, 8.0 * s, theme.hover);
            }
            let (fg, dim) = if selected { (theme.on_accent, theme.on_accent) } else { (theme.text, theme.text_dim) };

            let mid = (row.top + row.bottom) / 2.0;
            // Whole pixels, exactly the bitmap's size: drawn 1:1, no resampling.
            let (left, top, size) = ((row.left + 10.0 * s).round(), (mid - icon_px as f32 / 2.0).round(), icon_px as f32);
            let icon_rect = rect(left, top, left + size, top + size);
            match &candidate.target {
                Target::App(target) => {
                    if !self.bitmaps.contains_key(target)
                        && let Some(image) = apps_index::icon(target, icon_px)
                            && let Ok(bitmap) = unsafe { cx.dc.CreateBitmapFromWicBitmap(&image, None) } {
                                self.bitmaps.insert(target.clone(), bitmap);
                            }
                    if let Some(bitmap) = self.bitmaps.get(target) {
                        unsafe {
                            cx.dc.DrawBitmap(bitmap, Some(&icon_rect), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                        }
                    }
                }
                Target::Setting(_) => text::draw(cx, glyphs::SETTINGS, &icon, icon_rect, fg),
                Target::Session(action) => text::draw(cx, session_glyph(*action), &icon, icon_rect, fg),
            }

            let text_left = icon_rect.right + 12.0 * s;
            let text_right = row.right - 12.0 * s;
            text::draw(cx, &candidate.name, &name, rect(text_left, row.top + 4.0 * s, text_right, mid + 1.0 * s), fg);
            text::draw(cx, self.subtitle(candidate), &detail, rect(text_left, mid, text_right, row.bottom - 5.0 * s), dim);
        }
    }

    fn char_input(&mut self, c: char) -> bool {
        match c {
            '\u{1b}' => {
                bar::close(self.hwnd);
                return false;
            }
            '\r' => {
                self.activate(self.selected);
                return true;
            }
            '\u{8}' => {
                self.query.pop();
            }
            // Ctrl+Backspace: delete the last word.
            '\u{7f}' => {
                let trimmed = self.query.trim_end().len();
                let cut = self.query[..trimmed].rfind(' ').map_or(0, |i| i + 1);
                self.query.truncate(cut);
            }
            '\t' => {
                if !self.results.is_empty() {
                    self.selected = (self.selected + 1) % self.results.len();
                }
                return true;
            }
            c if c.is_control() => return false,
            c => self.query.push(c),
        }
        self.update_results();
        self.restart_caret();
        true
    }

    fn key_down(&mut self, vk: u16) -> bool {
        let count = self.results.len();
        if count == 0 {
            return false;
        }
        self.selected = match vk {
            v if v == VK_DOWN.0 => (self.selected + 1) % count,
            v if v == VK_UP.0 => (self.selected + count - 1) % count,
            v if v == VK_NEXT.0 => count - 1,
            v if v == VK_PRIOR.0 => 0,
            _ => return false,
        };
        self.armed = None;
        true
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hot = self.row_at(x, y);
        std::mem::replace(&mut self.hot, hot) != hot
    }

    fn mouse_leave(&mut self) -> bool {
        self.hot.take().is_some()
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        match self.row_at(x, y) {
            Some(index) => {
                self.activate(index);
                true
            }
            None => false,
        }
    }

    fn wheel(&mut self, _x: f32, _y: f32, delta: i16) -> bool {
        let vk = if delta > 0 { VK_UP.0 } else { VK_DOWN.0 };
        self.key_down(vk)
    }

    fn timer(&mut self, id: usize) -> bool {
        if id != TIMER_CARET {
            return false;
        }
        self.caret = !self.caret;
        true
    }

    fn deactivated(&mut self) -> bool {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CARET);
        }
        true
    }
}
