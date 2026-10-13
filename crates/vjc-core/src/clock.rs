//! Master BPM clock. Pure: time is always an injected `Instant`, never the wall clock.
//! Phase is measured in beats; beat 0 is the downbeat set by `resync`.
//!
//! Nudge is a temporary speed change (+/-`NUDGE_RATE`) while held, so phase stays
//! monotonic and the clock drifts back into place when released.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 300.0;
/// Taps further apart than this start a new measurement.
pub const TAP_RESET: Duration = Duration::from_secs(2);
/// Intervals averaged by `tap` (so up to 5 taps are remembered).
const TAP_INTERVALS: usize = 4;
/// Speed change while nudging (10 %).
pub const NUDGE_RATE: f64 = 0.1;

#[derive(Debug, Clone)]
pub struct Clock {
    bpm: f64,
    /// Phase (beats) at `anchor`; every phase change re-anchors.
    anchor_phase: f64,
    anchor: Instant,
    nudge: i8,
    taps: VecDeque<Instant>,
    /// Last beat index already reported by `tick`.
    last_beat: i64,
}

impl Clock {
    pub fn new(bpm: f64, now: Instant) -> Self {
        Clock {
            bpm: bpm.clamp(MIN_BPM, MAX_BPM),
            anchor_phase: 0.0,
            anchor: now,
            nudge: 0,
            taps: VecDeque::new(),
            last_beat: -1,
        }
    }

    pub fn bpm(&self) -> f64 {
        self.bpm
    }

    fn beats_per_sec(&self) -> f64 {
        self.bpm / 60.0 * (1.0 + NUDGE_RATE * f64::from(self.nudge))
    }

    /// Phase in beats at `now` (never before the anchor).
    pub fn phase(&self, now: Instant) -> f64 {
        let dt = now.saturating_duration_since(self.anchor).as_secs_f64();
        self.anchor_phase + dt * self.beats_per_sec()
    }

    fn reanchor(&mut self, now: Instant) {
        self.anchor_phase = self.phase(now);
        self.anchor = now;
    }

    /// Changes tempo keeping the phase continuous.
    pub fn set_bpm(&mut self, bpm: f64, now: Instant) {
        self.reanchor(now);
        self.bpm = bpm.clamp(MIN_BPM, MAX_BPM);
    }

    pub fn halve(&mut self, now: Instant) -> f64 {
        self.set_bpm(self.bpm / 2.0, now);
        self.bpm
    }

    pub fn double(&mut self, now: Instant) -> f64 {
        self.set_bpm(self.bpm * 2.0, now);
        self.bpm
    }

    /// Registers a tap. Returns the new BPM once there are at least 2 taps
    /// (mean of the last 4 intervals). Does not change the clock itself.
    pub fn tap(&mut self, now: Instant) -> Option<f64> {
        if let Some(&last) = self.taps.back()
            && now.saturating_duration_since(last) > TAP_RESET
        {
            self.taps.clear();
        }
        self.taps.push_back(now);
        while self.taps.len() > TAP_INTERVALS + 1 {
            self.taps.pop_front();
        }
        if self.taps.len() < 2 {
            return None;
        }
        let span = now.saturating_duration_since(self.taps[0]).as_secs_f64();
        let mean = span / (self.taps.len() - 1) as f64;
        (mean > 0.0).then(|| (60.0 / mean).clamp(MIN_BPM, MAX_BPM))
    }

    /// `dir` is -1, 0 or +1: runs slower/normal/faster until changed.
    pub fn nudge(&mut self, dir: i8, now: Instant) {
        self.reanchor(now);
        self.nudge = dir.signum();
    }

    /// Puts `now` on beat 0; the next `tick` reports beat 0.
    pub fn resync(&mut self, now: Instant) {
        self.anchor_phase = 0.0;
        self.anchor = now;
        self.last_beat = -1;
    }

    /// Call once per frame. Returns the phase and, if a whole beat was crossed since
    /// the previous tick, the index of the latest one.
    pub fn tick(&mut self, now: Instant) -> Tick {
        let phase = self.phase(now);
        let beat = phase.floor() as i64;
        let crossed = (beat > self.last_beat).then_some(beat);
        self.last_beat = self.last_beat.max(beat);
        Tick {
            phase,
            beat: crossed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tick {
    pub phase: f64,
    pub beat: Option<i64>,
}

/// Playback rate that makes a clip of `duration` seconds last exactly `beats` beats
/// at `bpm`. Rate 1.0 = original speed; clamped to a sane range.
pub fn beat_sync_rate(duration: f64, beats: f64, bpm: f64) -> f64 {
    if !(duration > 0.0 && beats > 0.0 && bpm > 0.0) {
        return 1.0;
    }
    (duration * bpm / (60.0 * beats)).clamp(0.05, 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn tap_500ms_is_120_bpm() {
        let t0 = Instant::now();
        let mut c = Clock::new(90.0, t0);
        assert_eq!(c.tap(t0), None);
        for i in 1..=6 {
            let bpm = c.tap(ms(t0, 500 * i)).unwrap();
            assert!((bpm - 120.0).abs() < 1e-9);
        }
    }

    #[test]
    fn tap_averages_last_four_intervals() {
        let t0 = Instant::now();
        let mut c = Clock::new(90.0, t0);
        c.tap(t0);
        c.tap(ms(t0, 1000)); // old, slow interval
        for i in 1..=4 {
            c.tap(ms(t0, 1000 + 500 * i));
        }
        let bpm = c.tap(ms(t0, 3500)).unwrap();
        assert!((bpm - 120.0).abs() < 1e-9);
    }

    #[test]
    fn tap_resets_after_two_seconds() {
        let t0 = Instant::now();
        let mut c = Clock::new(90.0, t0);
        c.tap(t0);
        assert!(c.tap(ms(t0, 500)).is_some());
        assert_eq!(c.tap(ms(t0, 3000)), None); // 2.5 s gap: starts over
        let bpm = c.tap(ms(t0, 3600)).unwrap();
        assert!((bpm - 100.0).abs() < 1e-9);
    }

    #[test]
    fn phase_advances_with_bpm() {
        let t0 = Instant::now();
        let c = Clock::new(120.0, t0);
        assert!((c.phase(ms(t0, 1000)) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn set_bpm_keeps_phase_continuous() {
        let t0 = Instant::now();
        let mut c = Clock::new(120.0, t0);
        c.set_bpm(60.0, ms(t0, 1000));
        assert!((c.phase(ms(t0, 1000)) - 2.0).abs() < 1e-9);
        assert!((c.phase(ms(t0, 2000)) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn resync_zeroes_phase_and_reports_beat_zero() {
        let t0 = Instant::now();
        let mut c = Clock::new(120.0, t0);
        c.tick(ms(t0, 1700));
        c.resync(ms(t0, 1750));
        assert_eq!(c.phase(ms(t0, 1750)), 0.0);
        let t = c.tick(ms(t0, 1760));
        assert_eq!(t.beat, Some(0));
        assert!((c.phase(ms(t0, 2250)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn beat_crossing_fires_once_per_beat() {
        let t0 = Instant::now();
        let mut c = Clock::new(120.0, t0); // beat every 500 ms
        assert_eq!(c.tick(ms(t0, 10)).beat, Some(0));
        assert_eq!(c.tick(ms(t0, 400)).beat, None);
        assert_eq!(c.tick(ms(t0, 510)).beat, Some(1));
        assert_eq!(c.tick(ms(t0, 520)).beat, None);
        // A long frame skipping beats reports only the latest.
        assert_eq!(c.tick(ms(t0, 2100)).beat, Some(4));
    }

    #[test]
    fn halve_and_double() {
        let t0 = Instant::now();
        let mut c = Clock::new(120.0, t0);
        assert_eq!(c.halve(t0), 60.0);
        assert_eq!(c.double(t0), 120.0);
        assert_eq!(c.double(t0), 240.0);
        assert_eq!(c.double(t0), MAX_BPM);
        let mut c = Clock::new(30.0, t0);
        assert_eq!(c.halve(t0), MIN_BPM);
    }

    #[test]
    fn nudge_is_temporary_speed_change() {
        let t0 = Instant::now();
        let mut c = Clock::new(120.0, t0);
        c.nudge(1, t0);
        assert!((c.phase(ms(t0, 1000)) - 2.2).abs() < 1e-9);
        c.nudge(0, ms(t0, 1000));
        assert!((c.phase(ms(t0, 2000)) - 4.2).abs() < 1e-9);
        c.nudge(-1, ms(t0, 2000));
        assert!((c.phase(ms(t0, 3000)) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn beat_sync_rate_fits_clip_to_beats() {
        // 4 s clip, 4 beats at 120 BPM (2 s): twice as fast.
        assert!((beat_sync_rate(4.0, 4.0, 120.0) - 2.0).abs() < 1e-9);
        // 2 s clip, 4 beats at 120 BPM (2 s): original speed.
        assert!((beat_sync_rate(2.0, 4.0, 120.0) - 1.0).abs() < 1e-9);
        // 1 s clip over 2 beats at 60 BPM (2 s): half speed.
        assert!((beat_sync_rate(1.0, 2.0, 60.0) - 0.5).abs() < 1e-9);
        assert_eq!(beat_sync_rate(0.0, 4.0, 120.0), 1.0);
    }
}
