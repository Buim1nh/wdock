//! Month calendar under the clock. Names, month titles and the first day of
//! the week follow the user's Windows locale. Wheel or chevrons change month.

use windows::Win32::Foundation::{HWND, RECT, SYSTEMTIME};
use windows::Win32::Globalization::{
    DATE_LONGDATE, ENUM_DATE_FORMATS_FLAGS, GetDateFormatEx, GetLocaleInfoEx, LOCALE_IFIRSTDAYOFWEEK, LOCALE_SSHORTESTDAYNAME1,
};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::{PCWSTR, w};

use crate::model::calendar::{Date, add_months, month_grid};
use crate::model::config::Backdrop;
use crate::ui::backdrop::DWM_CORNER_RADIUS;
use crate::ui::bar::{Button, PaintCx, View};
use crate::ui::gfx::{rect, rounded};
use crate::ui::text;

const PADDING: f32 = 14.0;
const CELL: f32 = 34.0;
const ROW: f32 = 30.0;
const HEADER: f32 = 20.0;
const MONTH_ROW: f32 = 30.0;
const WEEKDAY_ROW: f32 = 22.0;
const NAV: f32 = 26.0;
const GLYPH_PREV: &str = "\u{E76B}";
const GLYPH_NEXT: &str = "\u{E76C}";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hot {
    Prev,
    Next,
    Today,
}

pub struct CalendarView {
    anchor: RECT,
    scale: f32,
    today: Date,
    year: i32,
    month: u32,
    first_weekday: u32,
    /// Weekday names indexed 0 = Sunday.
    weekday_names: [String; 7],
    long_date: String,
    hot: Option<Hot>,
}

fn locale_string(kind: u32) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetLocaleInfoEx(PCWSTR::null(), kind, Some(&mut buf)) };
    String::from_utf16_lossy(&buf[..(len.max(1) - 1) as usize])
}

fn format_date(date: Date, picture: PCWSTR, flags: ENUM_DATE_FORMATS_FLAGS) -> String {
    let st = SYSTEMTIME { wYear: date.year as u16, wMonth: date.month as u16, wDay: date.day as u16, ..Default::default() };
    let mut buf = [0u16; 128];
    let len = unsafe { GetDateFormatEx(PCWSTR::null(), flags, Some(&st), picture, Some(&mut buf), PCWSTR::null()) };
    String::from_utf16_lossy(&buf[..(len.max(1) - 1) as usize])
}

impl CalendarView {
    pub fn new(anchor: RECT, scale: f32) -> Self {
        let now = unsafe { GetLocalTime() };
        let today = Date::new(now.wYear.into(), now.wMonth.into(), now.wDay.into());
        // LOCALE_IFIRSTDAYOFWEEK: 0 = Monday ... 6 = Sunday.
        let first_weekday = locale_string(LOCALE_IFIRSTDAYOFWEEK).parse::<u32>().map_or(1, |d| (d + 1) % 7);
        // LOCALE_SSHORTESTDAYNAME1..7 are Monday..Sunday.
        let weekday_names = std::array::from_fn(|weekday| {
            let index = if weekday == 0 { 6 } else { weekday as u32 - 1 };
            locale_string(LOCALE_SSHORTESTDAYNAME1 + index)
        });
        Self {
            anchor,
            scale,
            today,
            year: today.year,
            month: today.month,
            first_weekday,
            weekday_names,
            long_date: format_date(today, PCWSTR::null(), DATE_LONGDATE),
            hot: None,
        }
    }

    fn width(&self) -> f32 {
        PADDING * 2.0 + CELL * 7.0
    }

    fn height(&self) -> f32 {
        PADDING * 2.0 + HEADER + 6.0 + MONTH_ROW + WEEKDAY_ROW + ROW * 6.0
    }

    fn nav_rects(&self) -> (D2D_RECT_F, D2D_RECT_F, D2D_RECT_F) {
        let s = self.scale;
        let top = (PADDING + HEADER + 6.0) * s;
        let bottom = top + MONTH_ROW * s;
        let right = (self.width() - PADDING) * s;
        let next = rect(right - NAV * s, top, right, bottom);
        let prev = rect(right - NAV * 2.0 * s, top, right - NAV * s, bottom);
        let title = rect(PADDING * s, top, prev.left, bottom);
        (title, prev, next)
    }

    fn hot_at(&self, x: f32, y: f32) -> Option<Hot> {
        let inside = |r: D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        let (_, prev, next) = self.nav_rects();
        let header = rect(0.0, 0.0, self.width() * self.scale, (PADDING + HEADER) * self.scale);
        if inside(prev) {
            Some(Hot::Prev)
        } else if inside(next) {
            Some(Hot::Next)
        } else if inside(header) {
            Some(Hot::Today)
        } else {
            None
        }
    }

    fn shift(&mut self, delta: i32) {
        (self.year, self.month) = add_months(self.year, self.month, delta);
    }
}

impl View for CalendarView {
    fn attach(&mut self, _hwnd: HWND) {}

    /// Right-aligned under the clock.
    fn frame(&self) -> Option<RECT> {
        let s = self.scale;
        let width = (self.width() * s).round() as i32;
        let height = (self.height() * s).round() as i32;
        let left = self.anchor.right - width;
        let top = self.anchor.bottom + (6.0 * s).round() as i32;
        Some(RECT { left, top, right: left + width, bottom: top + height })
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        if cx.theme.backdrop == Backdrop::Solid
            && let Ok(brush) = cx.gfx.brush(cx.theme.solid_fill) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(0.0, 0.0, cx.width, cx.height), DWM_CORNER_RADIUS * s), &brush) };
            }
        let format = |family, size: f32, weight, align| text::format_px(&cx.gfx.dwrite, family, size * s, weight, align, false);
        let (Ok(header), Ok(title), Ok(day), Ok(dim), Ok(icon)) = (
            format(text::UI_FONT, 12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
            format(text::UI_FONT, 13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            format(text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            format(text::ICON_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) else {
            return;
        };
        let plate = |r: D2D_RECT_F, color| {
            if let Ok(brush) = cx.gfx.brush(color) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 6.0 * s), &brush) };
            }
        };

        let pad = PADDING * s;
        let header_rect = rect(pad, pad, cx.width - pad, pad + HEADER * s);
        let header_color = if self.hot == Some(Hot::Today) { cx.theme.accent } else { cx.theme.text_dim };
        text::draw(cx, &self.long_date, &header, header_rect, header_color);

        let (title_rect, prev, next) = self.nav_rects();
        let month_title = format_date(Date::new(self.year, self.month, 1), w!("MMMM yyyy"), ENUM_DATE_FORMATS_FLAGS(0));
        text::draw(cx, &month_title, &title, title_rect, cx.theme.text);
        for (hot, r, glyph) in [(Hot::Prev, prev, GLYPH_PREV), (Hot::Next, next, GLYPH_NEXT)] {
            if self.hot == Some(hot) {
                plate(r, cx.theme.hover);
            }
            text::draw(cx, glyph, &icon, r, cx.theme.text);
        }

        let grid_top = title_rect.bottom;
        for column in 0..7u32 {
            let weekday = (self.first_weekday + column) % 7;
            let left = pad + column as f32 * CELL * s;
            let r = rect(left, grid_top, left + CELL * s, grid_top + WEEKDAY_ROW * s);
            text::draw(cx, &self.weekday_names[weekday as usize], &dim, r, cx.theme.text_dim);
        }

        let rows_top = grid_top + WEEKDAY_ROW * s;
        for (i, (date, in_month)) in month_grid(self.year, self.month, self.first_weekday).into_iter().enumerate() {
            let (column, row) = ((i % 7) as f32, (i / 7) as f32);
            let left = pad + column * CELL * s;
            let top = rows_top + row * ROW * s;
            let cell = rect(left, top, left + CELL * s, top + ROW * s);
            let color = if date == self.today {
                let size = (ROW - 4.0).min(CELL - 4.0) * s;
                let (cx_mid, cy_mid) = ((cell.left + cell.right) / 2.0, (cell.top + cell.bottom) / 2.0);
                let circle = rect(cx_mid - size / 2.0, cy_mid - size / 2.0, cx_mid + size / 2.0, cy_mid + size / 2.0);
                if let Ok(brush) = cx.gfx.brush(cx.theme.accent) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(circle, size / 2.0), &brush) };
                }
                cx.theme.on_accent
            } else if in_month {
                cx.theme.text
            } else {
                cx.theme.text_dim
            };
            text::draw(cx, &date.day.to_string(), &day, cell, color);
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hot = self.hot_at(x, y);
        std::mem::replace(&mut self.hot, hot) != hot
    }

    fn mouse_leave(&mut self) -> bool {
        self.hot.take().is_some()
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        match self.hot_at(x, y) {
            Some(Hot::Prev) => self.shift(-1),
            Some(Hot::Next) => self.shift(1),
            Some(Hot::Today) => (self.year, self.month) = (self.today.year, self.today.month),
            None => return false,
        }
        true
    }

    fn wheel(&mut self, _x: f32, _y: f32, delta: i16) -> bool {
        self.shift(if delta > 0 { -1 } else { 1 });
        true
    }

    fn deactivated(&mut self) -> bool {
        true
    }
}
