//! Pomodoro timer state. Pure logic: callers pass a monotonic clock in
//! milliseconds, so the state machine needs no timers of its own and is
//! testable. The island schedules one one-shot timer for `next_wake`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Work,
    Break,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run {
    /// Not started, or reset: shows the full phase length.
    Idle,
    Running { ends: u64 },
    Paused { left: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pomodoro {
    work_ms: u64,
    break_ms: u64,
    phase: Phase,
    run: Run,
}

impl Pomodoro {
    /// Lengths in minutes; zero is raised to one.
    pub fn new(work_minutes: u32, break_minutes: u32) -> Self {
        let ms = |m: u32| u64::from(m.max(1)) * 60_000;
        Self { work_ms: ms(work_minutes), break_ms: ms(break_minutes), phase: Phase::Work, run: Run::Idle }
    }

    /// Lengths in minutes: (focus, break).
    pub fn lengths(&self) -> (u32, u32) {
        ((self.work_ms / 60_000) as u32, (self.break_ms / 60_000) as u32)
    }

    /// While idle, chooses which phase is shown, edited and started next.
    pub fn select(&mut self, phase: Phase) {
        if self.run == Run::Idle {
            self.phase = phase;
        }
    }

    /// Steps the selected phase's length by one notch; only while idle.
    /// Returns whether the length changed.
    pub fn step(&mut self, up: bool) -> bool {
        if self.run != Run::Idle {
            return false;
        }
        let before = self.lengths();
        let (work, rest) = before;
        let (work, rest) = match self.phase {
            Phase::Work => (step_minutes(work, up, WORK_RANGE, WORK_STEP), rest),
            Phase::Break => (work, step_minutes(rest, up, BREAK_RANGE, BREAK_STEP)),
        };
        *self = Self { work_ms: u64::from(work) * 60_000, break_ms: u64::from(rest) * 60_000, ..self.clone() };
        (work, rest) != before
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn is_running(&self) -> bool {
        matches!(self.run, Run::Running { .. })
    }

    /// Running or paused part-way: the pill keeps showing the timer.
    pub fn is_active(&self) -> bool {
        self.run != Run::Idle
    }

    fn length(&self) -> u64 {
        match self.phase {
            Phase::Work => self.work_ms,
            Phase::Break => self.break_ms,
        }
    }

    pub fn remaining_ms(&self, now: u64) -> u64 {
        match self.run {
            Run::Idle => self.length(),
            Run::Running { ends } => ends.saturating_sub(now),
            Run::Paused { left } => left,
        }
    }

    /// Starts an idle timer or resumes a paused one.
    pub fn start(&mut self, now: u64) {
        match self.run {
            Run::Idle => self.run = Run::Running { ends: now + self.length() },
            Run::Paused { left } => self.run = Run::Running { ends: now + left },
            Run::Running { .. } => {}
        }
    }

    pub fn pause(&mut self, now: u64) {
        if let Run::Running { ends } = self.run {
            self.run = Run::Paused { left: ends.saturating_sub(now) };
        }
    }

    pub fn toggle(&mut self, now: u64) {
        if self.is_running() {
            self.pause(now);
        } else {
            self.start(now);
        }
    }

    /// Back to a full, idle focus session.
    pub fn reset(&mut self) {
        self.phase = Phase::Work;
        self.run = Run::Idle;
    }

    /// When the running phase ends, for the next one-shot timer.
    pub fn next_wake(&self) -> Option<u64> {
        match self.run {
            Run::Running { ends } => Some(ends),
            _ => None,
        }
    }

    /// Moves on if the running phase is over and returns the phase that just
    /// ended. A finished focus session starts its break right away; a finished
    /// break waits idle for the next focus session.
    pub fn tick(&mut self, now: u64) -> Option<Phase> {
        let Run::Running { ends } = self.run else {
            return None;
        };
        if now < ends {
            return None;
        }
        let finished = self.phase;
        match finished {
            Phase::Work => {
                self.phase = Phase::Break;
                self.run = Run::Running { ends: now + self.break_ms };
            }
            Phase::Break => {
                self.phase = Phase::Work;
                self.run = Run::Idle;
            }
        }
        Some(finished)
    }
}

pub const WORK_RANGE: (u32, u32) = (5, 90);
pub const WORK_STEP: u32 = 5;
pub const BREAK_RANGE: (u32, u32) = (1, 30);
pub const BREAK_STEP: u32 = 1;

/// Moves `value` to the next multiple of `step` in the given direction,
/// kept inside `range`.
fn step_minutes(value: u32, up: bool, range: (u32, u32), step: u32) -> u32 {
    let next = if up { (value / step + 1) * step } else { value.div_ceil(step).saturating_sub(1) * step };
    next.clamp(range.0, range.1)
}

/// Parses the saved lengths (`work=25` / `break=5` lines), clamped to the
/// allowed ranges. Missing or garbled values fall back to `default`.
pub fn parse_saved(text: &str, default: (u32, u32)) -> (u32, u32) {
    let mut out = default;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Ok(value) = value.trim().parse::<u32>() else {
            continue;
        };
        match key.trim() {
            "work" => out.0 = value.clamp(WORK_RANGE.0, WORK_RANGE.1),
            "break" => out.1 = value.clamp(BREAK_RANGE.0, BREAK_RANGE.1),
            _ => {}
        }
    }
    out
}

pub fn format_saved(lengths: (u32, u32)) -> String {
    format!("work={}
break={}
", lengths.0, lengths.1)
}

/// `m:ss` for the expanded island, e.g. `24:05`.
pub fn clock(ms: u64) -> String {
    let secs = ms.div_ceil(1000);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Milliseconds until the compact pill's minute label changes.
pub fn tick_delay(remaining_ms: u64) -> u32 {
    let into_minute = remaining_ms % 60_000;
    (if into_minute == 0 { 60_000 } else { into_minute } + 20) as u32
}

/// Whole minutes rounded up for the compact pill, e.g. `25m`.
pub fn minutes(ms: u64) -> String {
    format!("{}m", ms.div_ceil(60_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_shows_full_length() {
        let p = Pomodoro::new(25, 5);
        assert_eq!(p.remaining_ms(123), 25 * 60_000);
        assert!(!p.is_active());
        assert_eq!(p.next_wake(), None);
    }

    #[test]
    fn pause_and_resume_keep_remaining() {
        let mut p = Pomodoro::new(25, 5);
        p.start(1_000);
        assert_eq!(p.remaining_ms(61_000), 24 * 60_000);
        p.pause(61_000);
        assert_eq!(p.remaining_ms(500_000), 24 * 60_000);
        p.start(600_000);
        assert_eq!(p.next_wake(), Some(600_000 + 24 * 60_000));
    }

    #[test]
    fn work_ends_into_running_break_then_idle() {
        let mut p = Pomodoro::new(1, 1);
        p.start(0);
        assert_eq!(p.tick(59_999), None);
        assert_eq!(p.tick(60_000), Some(Phase::Work));
        assert_eq!(p.phase(), Phase::Break);
        assert!(p.is_running());
        assert_eq!(p.tick(120_000), Some(Phase::Break));
        assert_eq!(p.phase(), Phase::Work);
        assert!(!p.is_active());
    }

    #[test]
    fn reset_returns_to_idle_work() {
        let mut p = Pomodoro::new(2, 1);
        p.start(0);
        p.tick(120_000);
        p.reset();
        assert_eq!(p.phase(), Phase::Work);
        assert_eq!(p.remaining_ms(0), 120_000);
    }

    #[test]
    fn zero_minutes_is_raised() {
        assert_eq!(Pomodoro::new(0, 0).remaining_ms(0), 60_000);
    }

    #[test]
    fn tick_delay_hits_minute_boundary() {
        assert_eq!(tick_delay(25 * 60_000), 60_020);
        assert_eq!(tick_delay(24 * 60_000 + 5_000), 5_020);
    }

    #[test]
    fn stepping_respects_steps_and_limits() {
        let mut p = Pomodoro::new(25, 5);
        assert!(p.step(true));
        assert_eq!(p.lengths(), (30, 5));
        assert!(p.step(false) && p.step(false));
        assert_eq!(p.lengths(), (20, 5));
        // Off-grid values snap to the grid.
        let mut odd = Pomodoro::new(7, 5);
        odd.step(true);
        assert_eq!(odd.lengths().0, 10);
        let mut odd = Pomodoro::new(7, 5);
        odd.step(false);
        assert_eq!(odd.lengths().0, 5);
        // Limits.
        let mut max = Pomodoro::new(90, 5);
        assert!(!max.step(true));
        let mut min = Pomodoro::new(5, 5);
        assert!(!min.step(false));
        // Break uses single minutes, 1..=30.
        let mut b = Pomodoro::new(25, 5);
        b.select(Phase::Break);
        b.step(true);
        assert_eq!(b.lengths(), (25, 6));
        let mut top = Pomodoro::new(25, 30);
        top.select(Phase::Break);
        assert!(!top.step(true));
        let mut one = Pomodoro::new(25, 1);
        one.select(Phase::Break);
        assert!(!one.step(false));
    }

    #[test]
    fn stepping_only_while_idle() {
        let mut p = Pomodoro::new(25, 5);
        p.start(0);
        assert!(!p.step(true));
        p.select(Phase::Break);
        assert_eq!(p.phase(), Phase::Work);
        p.pause(1_000);
        assert!(!p.step(true));
        assert_eq!(p.lengths(), (25, 5));
        p.reset();
        p.select(Phase::Break);
        assert_eq!(p.phase(), Phase::Break);
        assert_eq!(p.remaining_ms(0), 5 * 60_000);
    }

    #[test]
    fn saved_lengths_roundtrip_and_clamp() {
        assert_eq!(parse_saved(&format_saved((40, 10)), (25, 5)), (40, 10));
        assert_eq!(parse_saved("work=500
break=0
", (25, 5)), (90, 1));
        assert_eq!(parse_saved("garbage
work=x
", (25, 5)), (25, 5));
        assert_eq!(parse_saved("break=12", (25, 5)), (25, 12));
    }

    #[test]
    fn formats() {
        assert_eq!(clock(24 * 60_000 + 5_000), "24:05");
        assert_eq!(clock(59_001), "1:00");
        assert_eq!(minutes(59_000), "1m");
        assert_eq!(minutes(25 * 60_000), "25m");
    }
}
