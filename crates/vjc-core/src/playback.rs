//! Autopilot (next clip) and beat loop (position window). Pure: no players, no clock.

use vjc_model::{AutopilotMode, Direction};

/// Next column to play on a layer. `playable[i]` = column i has a playable clip.
/// Next wraps around; Random avoids repeating `current` when there is another choice.
/// `rand` is any random u64.
pub fn next_clip(mode: AutopilotMode, current: u32, playable: &[bool], rand: u64) -> Option<u32> {
    let n = playable.len();
    let ok = |i: usize| playable.get(i).copied().unwrap_or(false);
    match mode {
        AutopilotMode::Off => None,
        AutopilotMode::Next => (1..=n)
            .map(|k| (current as usize + k) % n)
            .find(|&i| ok(i))
            .map(|i| i as u32),
        AutopilotMode::Random => {
            let others: Vec<usize> = (0..n).filter(|&i| ok(i) && i != current as usize).collect();
            if others.is_empty() {
                ok(current as usize).then_some(current)
            } else {
                Some(others[(rand % others.len() as u64) as usize] as u32)
            }
        }
    }
}

/// Counts completed loops from successive playhead positions.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoopCounter {
    last: Option<f64>,
    pub count: u32,
}

impl LoopCounter {
    /// Feed the new position; true when it wrapped (a loop just completed).
    pub fn observe(&mut self, pos: f64, dir: Direction) -> bool {
        let wrapped = match (self.last, dir) {
            (Some(l), Direction::Forward) => pos < l - 0.5,
            (Some(l), Direction::Backward) => pos > l + 0.5,
            _ => false,
        };
        self.last = Some(pos);
        if wrapped {
            self.count += 1;
        }
        wrapped
    }

    pub fn reset(&mut self) {
        *self = LoopCounter::default();
    }
}

/// True once `count` loops reached the autopilot threshold (min 1).
pub fn loops_reached(count: u32, every: u32) -> bool {
    count >= every.max(1)
}

/// Beat loop window (normalised start, end) of `beats` beats from `pos`, for a clip of
/// `duration` seconds at `bpm`. At the clip end the window slides back to fit.
pub fn beat_window(pos: f64, beats: f64, bpm: f64, duration: f64) -> Option<(f64, f64)> {
    if !(beats > 0.0 && bpm > 0.0 && duration > 0.0) {
        return None;
    }
    let len = (beats * 60.0 / bpm / duration).min(1.0);
    let start = pos.clamp(0.0, 1.0 - len);
    Some((start, start + len))
}

/// Where to jump when `pos` left the window: past the end (forward) wraps to
/// the start, below the start (backward / bounce return) wraps to the end.
pub fn loop_wrap(pos: f64, window: (f64, f64)) -> Option<f64> {
    if pos >= window.1 {
        Some(window.0)
    } else if pos < window.0 - 1e-9 {
        Some(window.1 - 1e-6)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_wrap_both_directions() {
        let w = (0.2, 0.4);
        assert_eq!(loop_wrap(0.41, w), Some(0.2));
        let back = loop_wrap(0.19, w).unwrap();
        assert!(back > 0.39 && back < 0.4);
        assert_eq!(loop_wrap(0.3, w), None);
    }

    #[test]
    fn next_wraps_and_skips_empty() {
        let p = [true, false, true, false];
        assert_eq!(next_clip(AutopilotMode::Next, 0, &p, 0), Some(2));
        assert_eq!(next_clip(AutopilotMode::Next, 2, &p, 0), Some(0));
        assert_eq!(next_clip(AutopilotMode::Off, 0, &p, 0), None);
        assert_eq!(next_clip(AutopilotMode::Next, 0, &[true], 0), Some(0));
        assert_eq!(next_clip(AutopilotMode::Next, 0, &[false], 0), None);
    }

    #[test]
    fn random_avoids_current() {
        let p = [true, true, true];
        for r in 0..20 {
            assert_ne!(next_clip(AutopilotMode::Random, 1, &p, r), Some(1));
        }
        assert_eq!(
            next_clip(AutopilotMode::Random, 0, &[true, false], 5),
            Some(0)
        );
    }

    #[test]
    fn loop_counter_detects_wrap() {
        let mut c = LoopCounter::default();
        assert!(!c.observe(0.1, Direction::Forward));
        assert!(!c.observe(0.9, Direction::Forward));
        assert!(c.observe(0.05, Direction::Forward));
        assert_eq!(c.count, 1);
        assert!(loops_reached(1, 1) && !loops_reached(1, 2));
    }

    #[test]
    fn beat_window_and_wrap() {
        // 4 beats at 120 bpm = 2 s of a 10 s clip.
        let (s, e) = beat_window(0.5, 4.0, 120.0, 10.0).unwrap();
        assert!((s - 0.5).abs() < 1e-9 && (e - 0.7).abs() < 1e-9);
        // Near the end the window slides back.
        let (s, e) = beat_window(0.95, 4.0, 120.0, 10.0).unwrap();
        assert!((s - 0.8).abs() < 1e-9 && (e - 1.0).abs() < 1e-9);
        assert_eq!(beat_window(0.0, 4.0, 0.0, 10.0), None);
        assert_eq!(loop_wrap(0.69, (0.5, 0.7)), None);
        assert_eq!(loop_wrap(0.7, (0.5, 0.7)), Some(0.5));
    }
}
