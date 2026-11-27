//! The sleep timer and playback speed: their choices, and the small picker
//! strip drawn across the bottom of box 2 (`z` / `x`).

use std::time::{Duration, Instant};

/// How long the volume eases down before the sleep timer pauses.
pub const SLEEP_FADE: f64 = 10.0;

pub const SPEEDS: [f64; 7] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

/// Sleep timer choices: off, minutes, or the end of the current song.
pub const SLEEP_CHOICES: [Sleep; 7] = [
    Sleep::Off,
    Sleep::Minutes(15),
    Sleep::Minutes(30),
    Sleep::Minutes(45),
    Sleep::Minutes(60),
    Sleep::Minutes(90),
    Sleep::EndOfSong,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sleep {
    Off,
    Minutes(u64),
    EndOfSong,
}

impl Sleep {
    pub fn label(self) -> String {
        match self {
            Sleep::Off => "off".into(),
            Sleep::Minutes(m) => format!("{m}m"),
            Sleep::EndOfSong => "end of song".into(),
        }
    }
}

/// A running sleep timer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SleepTimer {
    At(Instant),
    EndOfSong,
}

impl SleepTimer {
    pub fn start(choice: Sleep, now: Instant) -> Option<SleepTimer> {
        match choice {
            Sleep::Off => None,
            Sleep::Minutes(m) => Some(SleepTimer::At(now + Duration::from_secs(m * 60))),
            Sleep::EndOfSong => Some(SleepTimer::EndOfSong),
        }
    }

    /// Seconds of listening left. For "end of song", what's left of the
    /// song in real time (`speed` makes it pass faster).
    pub fn remaining(self, now: Instant, time: f64, duration: f64, speed: f64) -> Option<f64> {
        match self {
            SleepTimer::At(t) => Some(t.saturating_duration_since(now).as_secs_f64()),
            SleepTimer::EndOfSong if duration > 0.0 => Some((duration - time).max(0.0) / speed.max(0.1)),
            SleepTimer::EndOfSong => None,
        }
    }

    /// For the status line: "☾ 23m", "☾ 40s", "☾ end of song".
    pub fn badge(self, now: Instant) -> String {
        match self {
            SleepTimer::EndOfSong => "☾ end of song".into(),
            SleepTimer::At(t) => {
                let s = t.saturating_duration_since(now).as_secs();
                if s >= 60 {
                    format!("☾ {}m", s.div_ceil(60))
                } else {
                    format!("☾ {s}s")
                }
            }
        }
    }
}

/// Volume factor while the sleep timer fades out: 1 until the last
/// [`SLEEP_FADE`] seconds, then down to 0.
pub fn fade_gain(remaining: f64) -> f64 {
    (remaining / SLEEP_FADE).clamp(0.0, 1.0)
}

pub fn speed_label(speed: f64) -> String {
    let s = format!("{speed:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    format!("{s}×")
}

/// The index in [`SPEEDS`] nearest to `speed`.
pub fn nearest_speed(speed: f64) -> usize {
    SPEEDS
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - speed).abs().total_cmp(&(b.1 - speed).abs()))
        .map(|(i, _)| i)
        .unwrap_or(2)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    Sleep,
    Speed,
}

/// The open picker strip in box 2.
#[derive(Clone, Debug, PartialEq)]
pub struct Picker {
    pub kind: PickerKind,
    /// Highlighted choice.
    pub idx: usize,
    /// Box 2 was hidden and is shown only while the picker is open.
    pub showed_player: bool,
}

impl Picker {
    pub fn title(&self) -> &'static str {
        match self.kind {
            PickerKind::Sleep => "sleep timer",
            PickerKind::Speed => "playback speed",
        }
    }

    pub fn labels(&self) -> Vec<String> {
        match self.kind {
            PickerKind::Sleep => SLEEP_CHOICES.iter().map(|c| c.label()).collect(),
            PickerKind::Speed => SPEEDS.iter().map(|s| speed_label(*s)).collect(),
        }
    }

    pub fn len(&self) -> usize {
        match self.kind {
            PickerKind::Sleep => SLEEP_CHOICES.len(),
            PickerKind::Speed => SPEEDS.len(),
        }
    }

    pub fn step(&mut self, forward: bool) {
        let n = self.len();
        self.idx = if forward {
            (self.idx + 1) % n
        } else {
            (self.idx + n - 1) % n
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_read_naturally() {
        let l: Vec<String> = SPEEDS.iter().map(|s| speed_label(*s)).collect();
        assert_eq!(l, ["0.5×", "0.75×", "1×", "1.25×", "1.5×", "1.75×", "2×"]);
        assert_eq!(Sleep::Minutes(15).label(), "15m");
        assert_eq!(nearest_speed(1.4), 4);
        assert_eq!(nearest_speed(9.0), 6);
    }

    #[test]
    fn sleep_timer_counts_down_and_fades_at_the_end() {
        let now = Instant::now();
        let t = SleepTimer::start(Sleep::Minutes(15), now).unwrap();
        assert_eq!(t.remaining(now, 0.0, 0.0, 1.0), Some(900.0));
        assert_eq!(t.badge(now), "☾ 15m");
        assert_eq!(t.badge(now + Duration::from_secs(890)), "☾ 10s");
        assert_eq!(t.remaining(now + Duration::from_secs(1000), 0.0, 0.0, 1.0), Some(0.0));
        assert_eq!(SleepTimer::start(Sleep::Off, now), None);
        assert_eq!(fade_gain(60.0), 1.0);
        assert_eq!(fade_gain(5.0), 0.5);
        assert_eq!(fade_gain(-1.0), 0.0);
    }

    #[test]
    fn end_of_song_follows_the_song_and_speed() {
        let now = Instant::now();
        let t = SleepTimer::EndOfSong;
        assert_eq!(t.remaining(now, 200.0, 240.0, 1.0), Some(40.0));
        assert_eq!(t.remaining(now, 200.0, 240.0, 2.0), Some(20.0));
        // Length not known yet: no countdown.
        assert_eq!(t.remaining(now, 0.0, 0.0, 1.0), None);
    }

    #[test]
    fn picker_wraps_around() {
        let mut p = Picker {
            kind: PickerKind::Speed,
            idx: 6,
            showed_player: false,
        };
        p.step(true);
        assert_eq!(p.idx, 0);
        p.step(false);
        assert_eq!(p.idx, 6);
        assert_eq!(p.labels().len(), p.len());
    }
}
