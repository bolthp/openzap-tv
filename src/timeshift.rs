//! Timeshift: moving around in a channel's archive.
//!
//! Archive streams of the shift scheme run on from the moment asked for and
//! cannot be seeked by the player, so every jump opens the archive again at
//! the new moment. The position is where the opened stream started plus how
//! far it has played.

use crate::epg::Programme;

/// Closer to now than this counts as live (seconds).
pub const LIVE_MARGIN_SECS: i64 = 10;
/// Jumping to "the start of the programme" this soon after it began goes
/// to the previous programme instead (seconds).
pub const RESTART_GRACE_SECS: i64 = 10;
/// A pause longer than this continues from the archive instead of from
/// the player's buffer, which may no longer hold that moment (seconds).
pub const LONG_PAUSE_SECS: f64 = 30.0;

/// Where to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Live,
    /// A moment in the archive (Unix seconds).
    At(i64),
}

/// Turns a wanted moment into a target: near now means live, and the
/// archive cannot go back further than `days`.
pub fn target(wanted: i64, now: i64, days: u32) -> Target {
    if wanted >= now - LIVE_MARGIN_SECS {
        return Target::Live;
    }
    // A minute of slack: the oldest recordings may already be deleted.
    let oldest = now - i64::from(days) * 86_400 + 60;
    Target::At(wanted.max(oldest))
}

/// Position in an archive stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// The moment the stream was opened at (Unix seconds).
    pub origin: i64,
    /// The player's playback time when the stream started; `None` until known.
    base: Option<f64>,
}

impl Position {
    pub fn new(origin: i64) -> Self {
        Position { origin, base: None }
    }

    /// The moment being shown, given the player's playback time (seconds).
    /// The first time known becomes the reference.
    pub fn at(&mut self, playback_time: Option<f64>) -> i64 {
        let Some(t) = playback_time.filter(|t| t.is_finite()) else {
            return self.origin;
        };
        let base = *self.base.get_or_insert(t);
        self.origin + (t - base).max(0.0) as i64
    }
}

/// The programme running at `at` in `list` (sorted by start).
pub fn programme_at<'a>(list: &[&'a Programme], at: i64) -> Option<&'a Programme> {
    list.iter()
        .rev()
        .find(|p| p.start <= at && at < p.stop)
        .copied()
}

/// Where "back to the start" goes from `at`: the start of the programme
/// running, or of the one before if it has only just begun.
pub fn programme_start(list: &[&Programme], at: i64) -> Option<i64> {
    let idx = list.iter().rposition(|p| p.start <= at)?;
    let current = list[idx];
    if at - current.start < RESTART_GRACE_SECS && idx > 0 {
        return Some(list[idx - 1].start);
    }
    Some(current.start)
}

/// The start of the programme after the one running at `at`.
pub fn next_programme(list: &[&Programme], at: i64) -> Option<i64> {
    list.iter().find(|p| p.start > at).map(|p| p.start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(start: i64, stop: i64) -> Programme {
        Programme {
            channel: "c".into(),
            start,
            stop,
            title: format!("{start}"),
            desc: None,
            category: None,
        }
    }

    #[test]
    fn targets_near_now_are_live_and_old_ones_are_clamped() {
        let now = 1_000_000;
        assert_eq!(target(now, now, 3), Target::Live);
        assert_eq!(target(now - 10, now, 3), Target::Live);
        assert_eq!(target(now + 600, now, 3), Target::Live);
        assert_eq!(target(now - 600, now, 3), Target::At(now - 600));
        assert_eq!(
            target(now - 10 * 86_400, now, 3),
            Target::At(now - 3 * 86_400 + 60)
        );
    }

    #[test]
    fn position_follows_playback_time_from_the_first_value() {
        let mut p = Position::new(5000);
        assert_eq!(p.at(None), 5000, "not started yet");
        assert_eq!(p.at(Some(12.0)), 5000, "first value is the reference");
        assert_eq!(p.at(Some(72.5)), 5060);
        assert_eq!(p.at(Some(f64::NAN)), 5000);
        assert_eq!(p.at(Some(5.0)), 5000, "never before the origin");
    }

    #[test]
    fn programme_navigation() {
        let (a, b, c) = (prog(0, 100), prog(100, 200), prog(200, 300));
        let list = [&a, &b, &c];
        assert_eq!(programme_at(&list, 150).map(|p| p.start), Some(100));
        assert_eq!(programme_at(&list, 300), None);
        assert_eq!(programme_start(&list, 150), Some(100));
        assert_eq!(programme_start(&list, 105), Some(0), "just begun: previous");
        assert_eq!(programme_start(&list, 3), Some(0), "nothing before");
        assert_eq!(programme_start(&list, -5), None);
        assert_eq!(next_programme(&list, 150), Some(200));
        assert_eq!(next_programme(&list, 250), None);
    }
}
