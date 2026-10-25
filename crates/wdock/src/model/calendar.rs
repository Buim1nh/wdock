//! Gregorian month grids for the calendar popup.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Date {
    pub const fn new(year: i32, month: u32, day: u32) -> Self {
        Self { year, month, day }
    }

    /// 0 = Sunday ... 6 = Saturday (Sakamoto's method).
    pub fn weekday(self) -> u32 {
        const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let year = if self.month < 3 { self.year - 1 } else { self.year };
        let day = year + year.div_euclid(4) - year.div_euclid(100) + year.div_euclid(400)
            + OFFSETS[self.month as usize - 1]
            + self.day as i32;
        day.rem_euclid(7) as u32
    }
}

pub fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `(year, month)` shifted by `delta` months.
pub fn add_months(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let index = year * 12 + month as i32 - 1 + delta;
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

/// Six weeks of dates covering `month`, starting on `first_weekday`
/// (0 = Sunday ... 6 = Saturday), with the flag `true` for days in `month`.
pub fn month_grid(year: i32, month: u32, first_weekday: u32) -> Vec<(Date, bool)> {
    let first = Date::new(year, month, 1);
    let lead = (first.weekday() + 7 - first_weekday % 7) % 7;
    let (prev_year, prev_month) = add_months(year, month, -1);
    let (next_year, next_month) = add_months(year, month, 1);
    let prev_days = days_in_month(prev_year, prev_month);
    let days = days_in_month(year, month);

    (0..42)
        .map(|cell| {
            let offset = cell as i64 - lead as i64;
            if offset < 0 {
                (Date::new(prev_year, prev_month, (prev_days as i64 + offset + 1) as u32), false)
            } else if offset < days as i64 {
                (Date::new(year, month, offset as u32 + 1), true)
            } else {
                (Date::new(next_year, next_month, (offset - days as i64) as u32 + 1), false)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekdays() {
        assert_eq!(Date::new(2026, 10, 25).weekday(), 0, "a Sunday");
        assert_eq!(Date::new(2000, 1, 1).weekday(), 6, "a Saturday");
        assert_eq!(Date::new(2024, 2, 29).weekday(), 4, "a Thursday");
    }

    #[test]
    fn month_lengths_and_leap_years() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2026, 4), 30);
    }

    #[test]
    fn add_months_wraps_years() {
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 12, 1), (2027, 1));
        assert_eq!(add_months(2026, 10, -22), (2024, 12));
    }

    #[test]
    fn grid_starts_on_requested_weekday() {
        // October 2026 starts on a Thursday.
        let monday_first = month_grid(2026, 10, 1);
        assert_eq!(monday_first.len(), 42);
        assert_eq!(monday_first[0], (Date::new(2026, 9, 28), false));
        assert_eq!(monday_first[3], (Date::new(2026, 10, 1), true));
        assert_eq!(monday_first[33], (Date::new(2026, 10, 31), true));
        assert_eq!(monday_first[34], (Date::new(2026, 11, 1), false));

        let sunday_first = month_grid(2026, 10, 0);
        assert_eq!(sunday_first[4], (Date::new(2026, 10, 1), true));
        assert!(sunday_first.iter().all(|(d, _)| d.day >= 1));
    }
}
