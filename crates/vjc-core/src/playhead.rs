//! Playhead: pure mapping from elapsed wall time to a normalised clip position (0..1)
//! for each play mode and direction. No I/O and no clock: the caller passes `dt`.
//! The media player just serves the frame nearest to `position`.

use vjc_model::{Direction, PlayMode};

/// In Random direction the position jumps to a new random spot every this many
/// seconds of clip time (so it scales with speed).
pub const RANDOM_HOLD: f64 = 0.25;

/// Result of a `Playhead::tick`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tick {
    /// Normalised position in 0..=1.
    pub position: f64,
    /// Once only: the clip reached its end (the caller clears the layer).
    pub finished: bool,
}

/// Position for a clip driven by an external clock: `elapsed` seconds since the clip's
/// start (negative holds the start), at `speed`, over `duration` seconds. Same modes and
/// directions as `Playhead::tick`, but absolute instead of incremental. Random and Pause
/// directions follow Forward / hold the start.
pub fn position_at(
    elapsed: f64,
    mode: PlayMode,
    direction: Direction,
    speed: f64,
    duration: f64,
) -> Tick {
    let travel =
        if elapsed.is_finite() && speed.is_finite() && duration.is_finite() && duration > 0.0 {
            elapsed.max(0.0) * speed.max(0.0) / duration
        } else {
            0.0
        };
    let backward = direction == Direction::Backward;
    let x = if direction == Direction::Pause {
        0.0
    } else {
        travel
    };
    let u = if backward { 1.0 - x } else { x };
    let (position, ended) = match mode {
        PlayMode::Bounce => {
            let w = u.rem_euclid(2.0);
            (if w > 1.0 { 2.0 - w } else { w }, false)
        }
        PlayMode::Once | PlayMode::OnceHold => (u.clamp(0.0, 1.0), x >= 1.0),
        _ => (u.rem_euclid(1.0), false),
    };
    Tick {
        position,
        finished: ended && mode == PlayMode::Once,
    }
}

#[derive(Debug, Clone)]
pub struct Playhead {
    /// Unfolded travel, in clip lengths. Loop keeps it in [0,1), Bounce in [0,2),
    /// Once clamps it to [0,1]; Random stores the last jump target.
    x: f64,
    /// Clip time accumulated since the last random jump.
    hold: f64,
    rng: u64,
}

impl Playhead {
    /// Starts at the clip start (end when playing backward).
    pub fn new(direction: Direction, seed: u64) -> Self {
        Playhead {
            x: if direction == Direction::Backward {
                1.0
            } else {
                0.0
            },
            hold: 0.0,
            rng: seed,
        }
    }

    /// Jumps to a normalised position (cue point / beat loop).
    pub fn seek(&mut self, pos: f64) {
        self.x = pos.clamp(0.0, 1.0);
    }

    fn next_unit(&mut self) -> f64 {
        // splitmix64
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Advances by `dt` wall seconds at `speed` (1 = file speed) over a clip of
    /// `duration` seconds. Direction and mode may change between ticks without a jump.
    pub fn tick(
        &mut self,
        dt: f64,
        mode: PlayMode,
        direction: Direction,
        speed: f64,
        duration: f64,
    ) -> Tick {
        let clip_dt = if dt.is_finite() && speed.is_finite() {
            dt.max(0.0) * speed.max(0.0)
        } else {
            0.0
        };
        let len = if duration.is_finite() && duration > 0.0 {
            clip_dt / duration
        } else {
            0.0
        };
        let mut finished = false;
        match direction {
            Direction::Random => {
                self.hold += clip_dt;
                if self.hold >= RANDOM_HOLD {
                    self.hold %= RANDOM_HOLD;
                    self.x = self.next_unit();
                }
                return Tick {
                    position: self.x.clamp(0.0, 1.0),
                    finished: false,
                };
            }
            Direction::Pause => {}
            Direction::Forward => self.x += len,
            Direction::Backward => self.x -= len,
        }
        let position = match mode {
            PlayMode::Bounce => {
                self.x = self.x.rem_euclid(2.0);
                if self.x > 1.0 { 2.0 - self.x } else { self.x }
            }
            PlayMode::Once | PlayMode::OnceHold => {
                finished = (self.x >= 1.0 && direction == Direction::Forward)
                    || (self.x <= 0.0 && direction == Direction::Backward);
                self.x = self.x.clamp(0.0, 1.0);
                self.x
            }
            // Loop (and Unknown, treated as Loop).
            _ => {
                self.x = self.x.rem_euclid(1.0);
                self.x
            }
        };
        Tick {
            position,
            finished: finished && mode == PlayMode::Once,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mode: PlayMode, dir: Direction, speed: f64, steps: usize) -> Vec<f64> {
        let mut h = Playhead::new(dir, 1);
        // 4 s clip, 1 s per tick.
        (0..steps)
            .map(|_| h.tick(1.0, mode, dir, speed, 4.0).position)
            .collect()
    }

    fn close(a: &[f64], b: &[f64]) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() < 1e-9, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn loop_forward_and_backward_wrap() {
        close(
            &run(PlayMode::Loop, Direction::Forward, 1.0, 6),
            &[0.25, 0.5, 0.75, 0.0, 0.25, 0.5],
        );
        close(
            &run(PlayMode::Loop, Direction::Backward, 1.0, 5),
            &[0.75, 0.5, 0.25, 0.0, 0.75],
        );
    }

    #[test]
    fn speed_scales_and_zero_freezes() {
        close(
            &run(PlayMode::Loop, Direction::Forward, 2.0, 3),
            &[0.5, 0.0, 0.5],
        );
        close(
            &run(PlayMode::Loop, Direction::Forward, 0.0, 3),
            &[0.0, 0.0, 0.0],
        );
    }

    #[test]
    fn bounce_goes_and_returns() {
        close(
            &run(PlayMode::Bounce, Direction::Forward, 1.0, 10),
            &[0.25, 0.5, 0.75, 1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5],
        );
        close(
            &run(PlayMode::Bounce, Direction::Backward, 1.0, 6),
            &[0.75, 0.5, 0.25, 0.0, 0.25, 0.5],
        );
    }

    #[test]
    fn bounce_reflects_overshoot() {
        let mut h = Playhead::new(Direction::Forward, 1);
        let t = h.tick(5.0, PlayMode::Bounce, Direction::Forward, 1.0, 4.0);
        assert!((t.position - 0.75).abs() < 1e-9);
    }

    #[test]
    fn once_stops_at_end_and_finishes() {
        let mut h = Playhead::new(Direction::Forward, 1);
        for i in 0..3 {
            let t = h.tick(1.0, PlayMode::Once, Direction::Forward, 1.0, 4.0);
            assert!(!t.finished, "step {i}");
        }
        for _ in 0..2 {
            let t = h.tick(1.0, PlayMode::Once, Direction::Forward, 1.0, 4.0);
            assert_eq!((t.position, t.finished), (1.0, true));
        }
    }

    #[test]
    fn once_backward_ends_at_zero() {
        let mut h = Playhead::new(Direction::Backward, 1);
        let t = h.tick(10.0, PlayMode::Once, Direction::Backward, 1.0, 4.0);
        assert_eq!((t.position, t.finished), (0.0, true));
    }

    #[test]
    fn once_hold_stays_on_last_frame_without_finishing() {
        let mut h = Playhead::new(Direction::Forward, 1);
        for _ in 0..8 {
            let t = h.tick(1.0, PlayMode::OnceHold, Direction::Forward, 1.0, 4.0);
            assert!(!t.finished);
            assert!(t.position <= 1.0);
        }
        let t = h.tick(1.0, PlayMode::OnceHold, Direction::Forward, 1.0, 4.0);
        assert_eq!(t.position, 1.0);
    }

    #[test]
    fn pause_holds_position_and_resumes_without_jump() {
        let mut h = Playhead::new(Direction::Forward, 1);
        h.tick(1.0, PlayMode::Loop, Direction::Forward, 1.0, 4.0);
        for _ in 0..3 {
            let t = h.tick(1.0, PlayMode::Loop, Direction::Pause, 1.0, 4.0);
            assert_eq!(t.position, 0.25);
        }
        // Reverse from the paused spot.
        let t = h.tick(1.0, PlayMode::Loop, Direction::Backward, 1.0, 4.0);
        assert_eq!(t.position, 0.0);
    }

    #[test]
    fn random_is_deterministic_per_seed_and_in_range() {
        let seq = |seed| {
            let mut h = Playhead::new(Direction::Random, seed);
            (0..20)
                .map(|_| {
                    h.tick(RANDOM_HOLD, PlayMode::Loop, Direction::Random, 1.0, 4.0)
                        .position
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(seq(7), seq(7));
        assert_ne!(seq(7), seq(8));
        assert!(seq(7).iter().all(|p| (0.0..=1.0).contains(p)));
    }

    #[test]
    fn random_holds_between_jumps_and_never_finishes() {
        let mut h = Playhead::new(Direction::Random, 3);
        let a = h.tick(0.3, PlayMode::Once, Direction::Random, 1.0, 4.0);
        let b = h.tick(0.1, PlayMode::Once, Direction::Random, 1.0, 4.0);
        assert_eq!(a.position, b.position);
        assert!(!a.finished && !b.finished);
    }

    #[test]
    fn bad_input_does_not_move() {
        let mut h = Playhead::new(Direction::Forward, 1);
        let t = h.tick(f64::NAN, PlayMode::Loop, Direction::Forward, 1.0, 4.0);
        assert_eq!(t.position, 0.0);
        let t = h.tick(1.0, PlayMode::Loop, Direction::Forward, 1.0, 0.0);
        assert_eq!(t.position, 0.0);
    }

    #[test]
    fn position_at_follows_modes() {
        let at = |e, m, d| position_at(e, m, d, 1.0, 4.0);
        let (l, f, b) = (PlayMode::Loop, Direction::Forward, Direction::Backward);
        assert_eq!(at(1.0, l, f).position, 0.25);
        assert_eq!(at(5.0, l, f).position, 0.25);
        assert_eq!(at(-3.0, l, f).position, 0.0);
        assert_eq!(at(1.0, l, b).position, 0.75);
        assert_eq!(at(5.0, PlayMode::Bounce, f).position, 0.75);
        assert_eq!(at(6.0, PlayMode::Bounce, f).position, 0.5);
        let end = at(9.0, PlayMode::Once, f);
        assert_eq!((end.position, end.finished), (1.0, true));
        let hold = at(9.0, PlayMode::OnceHold, f);
        assert_eq!((hold.position, hold.finished), (1.0, false));
        assert_eq!(at(9.0, PlayMode::Once, b).position, 0.0);
        assert_eq!(position_at(2.0, l, f, 2.0, 4.0).position, 0.0);
        assert_eq!(position_at(f64::NAN, l, f, 1.0, 4.0).position, 0.0);
    }
}
