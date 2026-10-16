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

    /// The in..out region changed from `old` to `new` while playing: keep the place in the
    /// file instead of the place in the region, so dragging a marker never jumps the picture.
    pub fn retarget(&mut self, old: (f64, f64), new: (f64, f64)) {
        use crate::playback::{to_actual, to_local};
        let remap = |u: f64| to_local(to_actual(u, old), new);
        // Bounce stores the way back as 1..2 (mirrored).
        self.x = if self.x > 1.0 {
            2.0 - remap(2.0 - self.x)
        } else {
            remap(self.x)
        };
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
            // Random position: plays on like Loop, but each wrap lands somewhere random.
            PlayMode::Random => {
                if self.x >= 1.0 || self.x < 0.0 {
                    self.x = self.next_unit();
                }
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
    /// What the app does each frame for a beat-synced clip: rate = speed x sync x master.
    fn synced(bpm: f64, beats: f64, dur: f64, wall: f64) -> f64 {
        let mut h = Playhead::new(Direction::Forward, 1);
        let rate = crate::playback_rate(1.0, crate::clock::beat_sync_rate(dur, beats, bpm), 1.0);
        let mut pos = 0.0;
        for _ in 0..(wall * 600.0).round() as usize {
            pos = h
                .tick(1.0 / 600.0, PlayMode::Loop, Direction::Forward, rate, dur)
                .position;
        }
        pos
    }

    #[test]
    fn retarget_keeps_the_file_position_when_the_region_changes() {
        use crate::playback::to_actual;
        let (old, new) = ((0.0, 1.0), (0.4, 1.0));
        let mut h = Playhead::new(Direction::Forward, 1);
        h.seek(0.5);
        h.retarget(old, new);
        let t = h.tick(0.0, PlayMode::Loop, Direction::Forward, 1.0, 1.0);
        assert!((to_actual(t.position, new) - 0.5).abs() < 1e-9, "{t:?}");
        // Marker moved past the head: it clamps to the new in point.
        h.retarget(new, (0.8, 1.0));
        let t = h.tick(0.0, PlayMode::Loop, Direction::Forward, 1.0, 1.0);
        assert_eq!(t.position, 0.0);
        // Bounce on its way back keeps the place too.
        let mut b = Playhead::new(Direction::Forward, 1);
        b.seek(1.0);
        b.x = 1.5; // back half, local 0.5
        b.retarget((0.0, 1.0), (0.0, 0.5));
        assert!((b.x - 1.0).abs() < 1e-9, "{}", b.x);
    }

    #[test]
    fn random_mode_jumps_at_the_end_and_stays_in_range() {
        let mut h = Playhead::new(Direction::Forward, 9);
        let mut jumps = 0;
        let mut prev = 0.0;
        for _ in 0..400 {
            let t = h.tick(0.1, PlayMode::Random, Direction::Forward, 1.0, 4.0);
            assert!((0.0..=1.0).contains(&t.position) && !t.finished);
            if t.position < prev - 0.001 {
                jumps += 1;
            }
            prev = t.position;
        }
        assert!(jumps >= 5, "{jumps} jumps");
        // Negative control: Loop wraps to the start, never to a random spot.
        let mut l = Playhead::new(Direction::Forward, 9);
        for _ in 0..39 {
            l.tick(0.1, PlayMode::Loop, Direction::Forward, 1.0, 4.0);
        }
        let wrap = l
            .tick(0.2, PlayMode::Loop, Direction::Forward, 1.0, 4.0)
            .position;
        assert!(wrap < 0.06, "{wrap}");
        // Same seed, same sequence.
        let run = |seed| {
            let mut h = Playhead::new(Direction::Forward, seed);
            (0..60)
                .map(|_| {
                    h.tick(0.2, PlayMode::Random, Direction::Forward, 1.0, 1.0)
                        .position
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(3), run(3));
        assert_ne!(run(3), run(4));
    }

    #[test]
    fn retarget_to_an_empty_region_never_produces_nan() {
        for new in [(0.5, 0.5), (0.7, 0.2)] {
            let mut h = Playhead::new(Direction::Forward, 1);
            h.seek(0.5);
            h.retarget((0.0, 1.0), new);
            assert!(h.x.is_finite() && (0.0..=1.0).contains(&h.x), "{}", h.x);
            h.retarget(new, (0.0, 1.0));
            assert!(h.x.is_finite(), "{}", h.x);
            let mut b = Playhead::new(Direction::Forward, 1);
            b.x = 1.5;
            b.retarget((0.0, 1.0), new);
            assert!(b.x.is_finite(), "{}", b.x);
        }
    }

    #[test]
    fn beat_sync_paces_the_playhead_to_the_beats() {
        // 4 s clip over 2 beats at 120 bpm = 1 s: half way after 0.5 s of wall time.
        assert!((synced(120.0, 2.0, 4.0, 0.5) - 0.5).abs() < 1e-3);
        // Same clip at 60 bpm lasts 2 s: a quarter after 0.5 s.
        assert!((synced(60.0, 2.0, 4.0, 0.5) - 0.25).abs() < 1e-3);
        // A clip already as long as its beats plays at file speed.
        assert!((synced(120.0, 4.0, 2.0, 1.0) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn beat_sync_rate_change_mid_play_does_not_jump() {
        let mut h = Playhead::new(Direction::Forward, 1);
        let r = |bpm| crate::playback_rate(1.0, crate::clock::beat_sync_rate(4.0, 2.0, bpm), 1.0);
        let a = h
            .tick(0.25, PlayMode::Loop, Direction::Forward, r(120.0), 4.0)
            .position;
        let b = h
            .tick(0.0, PlayMode::Loop, Direction::Forward, r(60.0), 4.0)
            .position;
        assert_eq!(a, b, "new tempo only changes the slope");
        let c = h
            .tick(0.5, PlayMode::Loop, Direction::Forward, r(60.0), 4.0)
            .position;
        assert!((c - (a + 0.25)).abs() < 1e-6);
    }

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
