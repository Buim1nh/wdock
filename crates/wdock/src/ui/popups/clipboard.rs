//! Text clipboard history. Search and selection stay local to this popup.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_END, VK_HOME, VK_NEXT, VK_PRIOR, VK_UP,
};

use crate::model::{clipboard, config::Backdrop};
use crate::platform::{clipboard as system_clipboard, monitors};
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{self, Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::{popup, text};

const WIDTH: f32 = 460.0;
const PAD: f32 = 12.0;
const LIST_TOP: f32 = 148.0;
const ROW: f32 = 50.0;
const FOOTER: f32 = 40.0;
const MAX_ROWS: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Search,
    Copy,
    Pin,
    Remove,
    Pause,
    Clear,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Search,
    Copy(u64),
    Pin(u64),
    Remove(u64),
    Pause,
    Clear,
}

pub struct ClipboardView {
    hwnd: HWND,
    frame: RECT,
    scale: f32,
    width: f32,
    rows: usize,
    entries: Vec<clipboard::Entry>,
    query: String,
    selected: Option<u64>,
    scroll: usize,
    focus: Focus,
    hot: Option<Hit>,
    enabled: bool,
    paused: bool,
    clearable: bool,
    capture_error: Option<&'static str>,
    error: Option<&'static str>,
}

fn filtered(entries: Vec<clipboard::Entry>, query: &str) -> Vec<clipboard::Entry> {
    let query = query.to_lowercase();
    entries
        .into_iter()
        .filter(|entry| entry.text.to_lowercase().contains(&query))
        .collect()
}

fn retained_selection(
    entries: &[clipboard::Entry],
    selected: Option<u64>,
    old_index: usize,
) -> Option<u64> {
    selected
        .filter(|id| entries.iter().any(|entry| entry.id == *id))
        .or_else(|| {
            entries
                .get(old_index.min(entries.len().saturating_sub(1)))
                .map(|entry| entry.id)
        })
}

fn layout(anchor: RECT, monitor: Option<RECT>, scale: f32) -> (RECT, f32, usize) {
    let width = monitor.map_or(WIDTH, |m| WIDTH.min((m.right - m.left) as f32 / scale));
    let available = monitor.map_or(600.0, |m| (m.bottom - m.top) as f32 / scale - 12.0);
    let rows = (((available - LIST_TOP - FOOTER) / ROW).floor() as usize).clamp(1, MAX_ROWS);
    let width_px = (width * scale).round() as i32;
    let height_px = ((LIST_TOP + rows as f32 * ROW + FOOTER) * scale).round() as i32;
    let mut left = anchor.right - width_px;
    let mut top = anchor.bottom + (6.0 * scale).round() as i32;
    if let Some(m) = monitor {
        left = left.clamp(m.left, (m.right - width_px).max(m.left));
        top = top.clamp(m.top, (m.bottom - height_px).max(m.top));
    }
    (
        RECT {
            left,
            top,
            right: left + width_px,
            bottom: top + height_px,
        },
        width,
        rows,
    )
}

impl ClipboardView {
    pub fn new(anchor: RECT, scale: f32) -> Self {
        let monitors = monitors::enumerate();
        let cx = (anchor.left + anchor.right) / 2;
        let cy = (anchor.top + anchor.bottom) / 2;
        let monitor = monitors
            .iter()
            .find(|m| {
                cx >= m.rect.left && cx < m.rect.right && cy >= m.rect.top && cy < m.rect.bottom
            })
            .or_else(|| monitors.iter().find(|m| m.primary))
            .map(|m| m.rect);
        let (frame, width, rows) = layout(anchor, monitor, scale);
        let mut view = Self {
            hwnd: HWND::default(),
            frame,
            scale,
            width,
            rows,
            entries: Vec::new(),
            query: String::new(),
            selected: None,
            scroll: 0,
            focus: Focus::Search,
            hot: None,
            enabled: false,
            paused: false,
            clearable: false,
            capture_error: None,
            error: None,
        };
        view.refresh();
        view
    }

    fn selected_index(&self) -> usize {
        self.selected
            .and_then(|id| self.entries.iter().position(|entry| entry.id == id))
            .unwrap_or(0)
    }

    fn refresh(&mut self) {
        let old_index = self.selected_index();
        self.enabled = clipboard::enabled();
        self.paused = clipboard::paused();
        self.capture_error = system_clipboard::error();
        let entries = if self.enabled {
            clipboard::list()
        } else {
            Vec::new()
        };
        self.clearable = entries.iter().any(|entry| !entry.pinned);
        self.entries = filtered(entries, &self.query);
        if self.entries.is_empty() && matches!(self.focus, Focus::Copy | Focus::Pin | Focus::Remove)
        {
            self.focus = Focus::Search;
        }
        if !self.clearable && self.focus == Focus::Clear {
            self.focus = Focus::Search;
        }
        self.selected = retained_selection(&self.entries, self.selected, old_index);
        self.reveal();
        self.hot = None;
    }

    fn reveal(&mut self) {
        let index = self.selected_index();
        self.scroll = self
            .scroll
            .min(self.entries.len().saturating_sub(self.rows));
        if index < self.scroll {
            self.scroll = index;
        }
        if index >= self.scroll + self.rows {
            self.scroll = index + 1 - self.rows;
        }
    }

    fn row_rect(&self, visible: usize) -> D2D_RECT_F {
        let top = LIST_TOP + visible as f32 * ROW;
        rect(PAD, top, self.width - PAD, top + ROW - 4.0)
    }

    fn pin_rect(&self, visible: usize) -> D2D_RECT_F {
        let row = self.row_rect(visible);
        rect(
            row.right - 122.0,
            row.top + 8.0,
            row.right - 68.0,
            row.bottom - 8.0,
        )
    }

    fn remove_rect(&self, visible: usize) -> D2D_RECT_F {
        let row = self.row_rect(visible);
        rect(
            row.right - 64.0,
            row.top + 8.0,
            row.right - 4.0,
            row.bottom - 8.0,
        )
    }

    fn search_rect(&self) -> D2D_RECT_F {
        rect(PAD, 64.0, self.width - PAD, 104.0)
    }
    fn pause_rect(&self) -> D2D_RECT_F {
        rect(PAD, 110.0, 138.0, 140.0)
    }
    fn clear_rect(&self) -> D2D_RECT_F {
        rect(146.0, 110.0, self.width - PAD, 140.0)
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let (x, y) = (x / self.scale, y / self.scale);
        let contains = |r: D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if !self.enabled {
            return None;
        }
        if contains(self.search_rect()) {
            return Some(Hit::Search);
        }
        if contains(self.pause_rect()) {
            return Some(Hit::Pause);
        }
        if self.clearable && contains(self.clear_rect()) {
            return Some(Hit::Clear);
        }
        for (visible, entry) in self
            .entries
            .iter()
            .skip(self.scroll)
            .take(self.rows)
            .enumerate()
        {
            if contains(self.pin_rect(visible)) {
                return Some(Hit::Pin(entry.id));
            }
            if contains(self.remove_rect(visible)) {
                return Some(Hit::Remove(entry.id));
            }
            if contains(self.row_rect(visible)) {
                return Some(Hit::Copy(entry.id));
            }
        }
        None
    }

    fn activate(&mut self, hit: Hit) {
        self.error = None;
        match hit {
            Hit::Search => {
                self.focus = Focus::Search;
                return;
            }
            Hit::Copy(id) => {
                self.selected = Some(id);
                self.focus = Focus::Copy;
                let Some(entry) = self.entries.iter().find(|entry| entry.id == id) else {
                    return;
                };
                if system_clipboard::write_text(self.hwnd, &entry.text).is_ok() {
                    bar::close(self.hwnd);
                } else {
                    self.error = Some("Clipboard is busy. Press Enter to try again.");
                }
                return;
            }
            Hit::Pin(id) => {
                self.selected = Some(id);
                if !clipboard::toggle_pin(id) {
                    self.error = Some("Could not pin this entry. Select another entry.");
                }
            }
            Hit::Remove(id) => {
                self.selected = Some(id);
                clipboard::remove(id);
            }
            Hit::Pause => {
                clipboard::set_paused(!self.paused);
            }
            Hit::Clear => {
                if self.clearable {
                    clipboard::clear_unpinned();
                }
            }
        }
        self.refresh();
        popup::clipboard_changed();
    }

    fn focused_hit(&self) -> Option<Hit> {
        match self.focus {
            Focus::Search | Focus::Copy => self.selected.map(Hit::Copy),
            Focus::Pin => self.selected.map(Hit::Pin),
            Focus::Remove => self.selected.map(Hit::Remove),
            Focus::Pause => Some(Hit::Pause),
            Focus::Clear => Some(Hit::Clear),
        }
    }

    fn advance_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Search if !self.entries.is_empty() => Focus::Copy,
            Focus::Copy => Focus::Pin,
            Focus::Pin => Focus::Remove,
            Focus::Search | Focus::Remove => Focus::Pause,
            Focus::Pause if self.clearable => Focus::Clear,
            Focus::Pause => Focus::Search,
            Focus::Clear => Focus::Search,
        };
    }
}

impl View for ClipboardView {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }
    fn frame(&self) -> Option<RECT> {
        Some(self.frame)
    }
    fn model_changed(&mut self) -> bool {
        self.refresh();
        true
    }
    fn deactivated(&mut self) -> bool {
        true
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        let theme = cx.theme;
        let px = |r: D2D_RECT_F| rect(r.left * s, r.top * s, r.right * s, r.bottom * s);
        let fill = |r, radius, color| {
            if let Ok(brush) = cx.gfx.brush(color) {
                unsafe {
                    cx.dc
                        .FillRoundedRectangle(&rounded(px(r), radius * s), &brush)
                };
            }
        };
        if theme.backdrop == Backdrop::Solid {
            fill(
                rect(0.0, 0.0, self.width, cx.height / s),
                DWM_CORNER_RADIUS,
                theme.solid_fill,
            );
        }
        let (Ok(title), Ok(body), Ok(small), Ok(center)) = (
            text::format(
                cx,
                text::UI_FONT,
                16.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            ),
            text::format(
                cx,
                text::UI_FONT,
                13.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            ),
            text::format(
                cx,
                text::UI_FONT,
                11.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            ),
            text::format(
                cx,
                text::UI_FONT,
                11.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            ),
        ) else {
            return;
        };
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            ..Default::default()
        };
        unsafe {
            if let Ok(ellipsis) = cx.gfx.dwrite.CreateEllipsisTrimmingSign(&body) {
                let _ = body.SetTrimming(&trimming, &ellipsis);
            }
        }
        let draw = |value: &str, format, r, color| text::draw(cx, value, format, px(r), color);
        draw(
            "Clipboard history",
            &title,
            rect(PAD, 10.0, self.width - PAD, 38.0),
            theme.text,
        );
        let status = if !self.enabled {
            "History is off"
        } else if self.capture_error.is_some() {
            "History unavailable · collection could not start"
        } else if self.paused {
            "Paused · new copies are not saved"
        } else {
            "Text only · kept in memory until wdock exits"
        };
        draw(
            status,
            &small,
            rect(PAD, 38.0, self.width - PAD, 58.0),
            theme.text_dim,
        );
        if !self.enabled {
            draw(
                "Enable Clipboard history in Settings.",
                &body,
                rect(PAD, 100.0, self.width - PAD, 138.0),
                theme.text,
            );
            draw(
                "wdock settings > Clipboard history (RAM)",
                &small,
                rect(PAD, 138.0, self.width - PAD, 166.0),
                theme.text_dim,
            );
            return;
        }
        fill(self.search_rect(), 7.0, theme.control);
        let search = self.search_rect();
        if self.focus == Focus::Search {
            fill(
                rect(
                    search.left,
                    search.bottom - 2.0,
                    search.right,
                    search.bottom,
                ),
                1.0,
                theme.accent,
            );
        }
        draw(
            if self.query.is_empty() {
                "Type to search copied text"
            } else {
                &self.query
            },
            &body,
            rect(
                search.left + 10.0,
                search.top,
                search.right - 10.0,
                search.bottom,
            ),
            if self.query.is_empty() {
                theme.text_dim
            } else {
                theme.text
            },
        );

        let focused = self.focused_hit();
        let button = |label, r, hit, enabled| {
            let active = enabled
                && (self.hot == Some(hit) || (self.focus != Focus::Search && focused == Some(hit)));
            fill(r, 5.0, if active { theme.accent } else { theme.control });
            draw(
                label,
                &center,
                r,
                if active {
                    theme.on_accent
                } else if enabled {
                    theme.text
                } else {
                    theme.text_dim
                },
            );
        };
        button(
            if self.paused {
                "Resume history"
            } else {
                "Pause history"
            },
            self.pause_rect(),
            Hit::Pause,
            true,
        );
        button(
            "Clear unpinned",
            self.clear_rect(),
            Hit::Clear,
            self.clearable,
        );
        for (visible, entry) in self
            .entries
            .iter()
            .skip(self.scroll)
            .take(self.rows)
            .enumerate()
        {
            let row = self.row_rect(visible);
            let selected = self.selected == Some(entry.id);
            fill(
                row,
                7.0,
                if selected || self.hot == Some(Hit::Copy(entry.id)) {
                    theme.hover
                } else {
                    theme.control
                },
            );
            if selected {
                fill(
                    rect(row.left, row.top + 8.0, row.left + 3.0, row.bottom - 8.0),
                    1.0,
                    theme.accent,
                );
            }
            let preview: String = entry
                .text
                .chars()
                .take(256)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            let right = self.pin_rect(visible).left - 8.0;
            draw(
                &preview,
                &body,
                rect(row.left + 10.0, row.top + 3.0, right, row.top + 27.0),
                theme.text,
            );
            draw(
                if entry.pinned {
                    "Pinned · Enter to copy"
                } else {
                    "Enter to copy"
                },
                &small,
                rect(row.left + 10.0, row.top + 25.0, right, row.bottom - 2.0),
                theme.text_dim,
            );
            button(
                if entry.pinned { "Unpin" } else { "Pin" },
                self.pin_rect(visible),
                Hit::Pin(entry.id),
                true,
            );
            button(
                "Remove",
                self.remove_rect(visible),
                Hit::Remove(entry.id),
                true,
            );
        }
        if self.entries.is_empty() {
            let (heading, detail) = if self.capture_error.is_some() {
                (
                    "Clipboard history unavailable",
                    "Turn history off and on in wdock settings to retry.",
                )
            } else if !self.query.is_empty() {
                ("No matching text", "Try a different search.")
            } else if self.paused {
                (
                    "History is paused",
                    "Resume history, then copy text to collect it here.",
                )
            } else {
                (
                    "No copied text yet",
                    "Copy text in any app to collect it here.",
                )
            };
            draw(
                heading,
                &body,
                rect(PAD, LIST_TOP + 30.0, self.width - PAD, LIST_TOP + 62.0),
                theme.text,
            );
            draw(
                detail,
                &small,
                rect(PAD, LIST_TOP + 62.0, self.width - PAD, LIST_TOP + 90.0),
                theme.text_dim,
            );
        }
        if self.entries.len() > self.rows {
            let top =
                LIST_TOP + self.scroll as f32 / self.entries.len() as f32 * self.rows as f32 * ROW;
            let height = self.rows as f32 / self.entries.len() as f32 * self.rows as f32 * ROW;
            fill(
                rect(self.width - 6.0, top, self.width - 3.0, top + height),
                1.5,
                theme.text_dim,
            );
        }
        let bottom = LIST_TOP + self.rows as f32 * ROW;
        let hint = self
            .error
            .or(self.capture_error)
            .unwrap_or(match self.focus {
                Focus::Search | Focus::Copy => "↑ ↓ select · Enter copy · Tab actions · Esc close",
                Focus::Pin => "Enter pin / unpin · Tab next action · Esc close",
                Focus::Remove => "Enter remove selected · Tab next action · Esc close",
                Focus::Pause => "Enter pause / resume · Tab next action · Esc close",
                Focus::Clear => "Enter clear unpinned · Tab search · Esc close",
            });
        draw(
            hint,
            &small,
            rect(PAD, bottom + 6.0, self.width - PAD, bottom + 32.0),
            theme.text_dim,
        );
    }

    fn char_input(&mut self, c: char) -> bool {
        match c {
            '\u{1b}' => {
                bar::close(self.hwnd);
                return false;
            }
            _ if !self.enabled => return false,
            '\r' => {
                if let Some(hit) = self.focused_hit() {
                    self.activate(hit);
                }
                return true;
            }
            '\t' => {
                self.advance_focus();
                return true;
            }
            '\u{8}' => {
                self.query.pop();
            }
            '\u{7f}' => {
                self.query.clear();
            }
            c if c.is_control() => return false,
            c if self.query.len() < 1024 => {
                self.query.push(c);
            }
            _ => return false,
        }
        self.error = None;
        self.focus = Focus::Search;
        self.refresh();
        true
    }

    fn key_down(&mut self, vk: u16) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let index = self.selected_index();
        let last = self.entries.len() - 1;
        let next = match vk {
            v if v == VK_DOWN.0 => (index + 1).min(last),
            v if v == VK_UP.0 => index.saturating_sub(1),
            v if v == VK_NEXT.0 => (index + self.rows).min(last),
            v if v == VK_PRIOR.0 => index.saturating_sub(self.rows),
            v if v == VK_HOME.0 => 0,
            v if v == VK_END.0 => last,
            _ => return false,
        };
        self.selected = Some(self.entries[next].id);
        self.focus = Focus::Copy;
        self.error = None;
        self.reveal();
        true
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hit = self.hit(x, y);
        std::mem::replace(&mut self.hot, hit) != hit
    }
    fn mouse_leave(&mut self) -> bool {
        self.hot.take().is_some()
    }
    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        let Some(hit) = self.hit(x, y) else {
            return false;
        };
        self.activate(hit);
        true
    }
    fn wheel(&mut self, _x: f32, _y: f32, delta: i16) -> bool {
        if delta == 0 {
            return false;
        }
        self.key_down(if delta > 0 { VK_UP.0 } else { VK_DOWN.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, text: &str) -> clipboard::Entry {
        clipboard::Entry {
            id,
            text: text.into(),
            pinned: false,
        }
    }

    #[test]
    fn search_matches_unicode_and_multiline_text() {
        let entries = filtered(vec![entry(1, "Café\n日本語"), entry(2, "Other")], "CAFÉ");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 1);
        assert_eq!(filtered(entries, "日本語").len(), 1);
    }

    #[test]
    fn selection_survives_insertions_and_falls_back_near_removed_row() {
        let entries = vec![entry(3, "new"), entry(1, "first"), entry(2, "second")];
        assert_eq!(retained_selection(&entries, Some(2), 1), Some(2));
        assert_eq!(retained_selection(&entries[..2], Some(2), 2), Some(1));
        assert_eq!(retained_selection(&[], Some(2), 2), None);
    }

    #[test]
    fn fractional_dpi_layout_stays_inside_each_monitor() {
        let monitors = [
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            RECT {
                left: -1920,
                top: -120,
                right: 0,
                bottom: 960,
            },
        ];
        for scale in [1.25, 1.5] {
            for monitor in monitors {
                for (x, y) in [
                    (monitor.left + 40, monitor.top + 31),
                    (monitor.right - 2, monitor.bottom - 2),
                ] {
                    let anchor = RECT {
                        left: x - 24,
                        top: y - 28,
                        right: x,
                        bottom: y,
                    };
                    let (frame, width, rows) = layout(anchor, Some(monitor), scale);
                    assert!(frame.left >= monitor.left && frame.right <= monitor.right);
                    assert!(frame.top >= monitor.top && frame.bottom <= monitor.bottom);
                    assert_eq!(rows, MAX_ROWS);
                    assert_eq!(frame.right - frame.left, (width * scale).round() as i32);
                    assert_eq!(
                        frame.bottom - frame.top,
                        ((LIST_TOP + rows as f32 * ROW + FOOTER) * scale).round() as i32
                    );
                }
            }
        }
    }

    #[test]
    fn short_fractional_dpi_monitor_reduces_rows_and_keeps_footer_visible() {
        let monitor = RECT {
            left: -1280,
            top: 0,
            right: 0,
            bottom: 600,
        };
        let anchor = RECT {
            left: -90,
            top: 0,
            right: -40,
            bottom: 38,
        };
        let (frame, _, rows) = layout(anchor, Some(monitor), 1.5);
        assert_eq!(rows, 4);
        assert!(frame.top >= monitor.top && frame.bottom <= monitor.bottom);
        assert!(frame.right <= monitor.right && frame.left >= monitor.left);
        assert_eq!(frame.right, anchor.right);
    }
}
