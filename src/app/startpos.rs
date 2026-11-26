//! `--position X,Y`: moves the window there after start.
//!
//! A move requested before the window manager has placed the window can
//! be ignored (seen with WSLg), so it is repeated until the window is there.

/// First try this long after start, seconds.
const FIRST_TRY: f64 = 0.2;
/// Wait between tries, seconds.
const RETRY_AFTER: f64 = 0.5;
const MAX_TRIES: u32 = 5;
/// Window managers place the frame or the client area there (WSLg: off
/// by the title bar), so "close" is good enough.
const TOLERANCE: i32 = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Nothing to do before this time (seconds).
    WaitUntil(f64),
    /// Ask the window manager to move the window to the target.
    Move { x: i32, y: i32, attempt: u32 },
    /// The window is at the target.
    Done,
    /// It did not move; no more tries.
    GaveUp,
}

#[derive(Debug)]
pub struct StartPosition {
    target: (i32, i32),
    next_try: f64,
    tries: u32,
}

impl StartPosition {
    pub fn new(x: i32, y: i32) -> Self {
        StartPosition {
            target: (x, y),
            next_try: FIRST_TRY,
            tries: 0,
        }
    }

    /// `now`: seconds since start; `current`: the window's outer position
    /// in pixels, if known.
    pub fn step(&mut self, now: f64, current: Option<(i32, i32)>) -> Step {
        if now < self.next_try {
            return Step::WaitUntil(self.next_try);
        }
        let (x, y) = self.target;
        let there = current
            .is_some_and(|(cx, cy)| (cx - x).abs() <= TOLERANCE && (cy - y).abs() <= TOLERANCE);
        if there && self.tries > 0 {
            return Step::Done;
        }
        if self.tries >= MAX_TRIES {
            return Step::GaveUp;
        }
        self.tries += 1;
        self.next_try = now + RETRY_AFTER;
        Step::Move {
            x,
            y,
            attempt: self.tries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_then_moves_until_the_window_is_there() {
        let mut pos = StartPosition::new(100, 50);
        assert_eq!(pos.step(0.0, Some((900, 900))), Step::WaitUntil(FIRST_TRY));
        let first = Step::Move {
            x: 100,
            y: 50,
            attempt: 1,
        };
        assert_eq!(pos.step(0.3, Some((900, 900))), first);
        assert_eq!(pos.step(0.4, Some((900, 900))), Step::WaitUntil(0.8));
        // Ignored: tried again.
        assert!(matches!(
            pos.step(0.8, Some((900, 900))),
            Step::Move { attempt: 2, .. }
        ));
        assert_eq!(pos.step(1.3, Some((102, 48))), Step::Done);
    }

    #[test]
    fn moves_at_least_once_and_gives_up_eventually() {
        // Already there before any move: still asks once (cheap, harmless).
        let mut pos = StartPosition::new(0, 0);
        assert!(matches!(pos.step(1.0, Some((0, 0))), Step::Move { .. }));
        assert_eq!(pos.step(2.0, Some((0, 0))), Step::Done);

        let mut pos = StartPosition::new(0, 0);
        let mut now = 1.0;
        for _ in 0..MAX_TRIES {
            assert!(matches!(pos.step(now, None), Step::Move { .. }));
            now += RETRY_AFTER;
        }
        assert_eq!(pos.step(now, None), Step::GaveUp);
    }
}
