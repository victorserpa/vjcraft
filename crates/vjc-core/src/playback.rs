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

    /// A deliberate jump (cue): continue from `pos` without counting it as a loop.
    pub fn rebase(&mut self, pos: f64) {
        self.last = Some(pos);
    }

    pub fn reset(&mut self) {
        *self = LoopCounter::default();
    }
}

/// True once `count` loops reached the autopilot threshold (min 1).
/// Generators have no end: for autopilot one "loop" is one bar (4 beats). Advances the
/// `(last bar, bars completed)` state to `bar`; the first call only records the bar.
pub fn gen_bars(prev: Option<(i64, u32)>, bar: i64) -> (i64, u32) {
    match prev {
        Some((last, n)) if last != bar => (bar, n + 1),
        Some(p) => p,
        None => (bar, 0),
    }
}

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

/// Smallest in..out span (normalised), so the region never collapses.
pub const MIN_REGION: f64 = 0.001;

/// The (in, out) region a clip plays, normalised. A damaged pair (not finite, outside 0..=1
/// or narrower than `MIN_REGION`) plays the whole clip.
pub fn region(t: &vjc_model::Transport) -> (f64, f64) {
    let (a, b) = (t.in_point, t.out_point);
    if a.is_finite() && b.is_finite() && a >= 0.0 && b <= 1.0 && b - a >= MIN_REGION {
        (a, b)
    } else {
        (0.0, 1.0)
    }
}

/// Position inside the file for a playhead position local to the region (0..=1).
pub fn to_actual(local: f64, region: (f64, f64)) -> f64 {
    region.0 + local.clamp(0.0, 1.0) * (region.1 - region.0)
}

/// Playhead position local to the region for a position inside the file.
pub fn to_local(pos: f64, region: (f64, f64)) -> f64 {
    let w = region.1 - region.0;
    if !(w > 0.0 && w.is_finite()) {
        return 0.0;
    }
    ((pos - region.0) / w).clamp(0.0, 1.0)
}

/// Seconds the in..out region lasts at clip speed `speed` (infinite when paused by speed 0).
pub fn region_secs(file_secs: f64, region: (f64, f64), speed: f64) -> f64 {
    if speed > 0.0 {
        file_secs * (region.1 - region.0) / speed
    } else {
        f64::INFINITY
    }
}

/// Clip speed (0.01..=4, the range `SetSpeed` accepts) that makes the region last
/// `target_secs`; None for a target that is not a positive finite number.
pub fn speed_for_duration(file_secs: f64, region: (f64, f64), target_secs: f64) -> Option<f64> {
    if !(target_secs.is_finite() && target_secs > 0.0 && file_secs > 0.0) {
        return None;
    }
    Some((file_secs * (region.1 - region.0) / target_secs).clamp(0.01, 4.0))
}

/// What a press on the scrub bar grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubHit {
    In,
    Out,
    /// Neither marker: the press moves the playhead.
    Seek,
}

/// Which marker a press at normalised bar position `p` grabs, within `tol` (bar fractions).
/// Overlapping markers split at their midpoint, so both stay reachable.
pub fn scrub_hit(p: f64, region: (f64, f64), tol: f64) -> ScrubHit {
    let (di, d_o) = ((p - region.0).abs(), (p - region.1).abs());
    match (di <= tol, d_o <= tol) {
        (true, true) => {
            if p < (region.0 + region.1) / 2.0 {
                ScrubHit::In
            } else {
                ScrubHit::Out
            }
        }
        (true, false) => ScrubHit::In,
        (false, true) => ScrubHit::Out,
        _ => ScrubHit::Seek,
    }
}

/// The region after dragging `hit` to bar position `p`: the marker follows the pointer but
/// never crosses the other one (keeps `MIN_REGION`) or leaves 0..=1.
pub fn drag_marker(hit: ScrubHit, p: f64, region: (f64, f64)) -> (f64, f64) {
    let p = if p.is_finite() { p } else { 0.0 };
    match hit {
        ScrubHit::In => (p.clamp(0.0, region.1 - MIN_REGION), region.1),
        ScrubHit::Out => (region.0, p.clamp(region.0 + MIN_REGION, 1.0)),
        ScrubHit::Seek => region,
    }
}

/// How a clip's time is driven, the three modes of the transport dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    /// Free running at the clip speed.
    Timeline,
    /// Stretched to a number of beats of the app clock.
    BpmSync,
    /// Follows the external timecode.
    Timecode,
}

/// Beats a clip gets when switched to BPM sync.
pub const DEFAULT_SYNC_BEATS: f64 = 4.0;

impl TransportMode {
    pub const ALL: [TransportMode; 3] = [
        TransportMode::Timeline,
        TransportMode::BpmSync,
        TransportMode::Timecode,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TransportMode::Timeline => "Timeline",
            TransportMode::BpmSync => "BPM sync",
            TransportMode::Timecode => "Timecode",
        }
    }

    /// Timecode wins over BPM sync, as in the player.
    pub fn of(t: &vjc_model::Transport) -> Self {
        if t.timecode_start.is_some() {
            TransportMode::Timecode
        } else if t.beat_sync.is_some() {
            TransportMode::BpmSync
        } else {
            TransportMode::Timeline
        }
    }

    /// `(beat_sync, timecode_start)` after switching `t` to this mode; `seed` is the start
    /// a fresh Timecode mode begins with.
    pub fn sync_for(
        self,
        t: &vjc_model::Transport,
        seed: vjc_model::Timecode,
    ) -> (Option<f64>, Option<vjc_model::Timecode>) {
        match self {
            TransportMode::Timeline => (None, None),
            TransportMode::BpmSync => (Some(t.beat_sync.unwrap_or(DEFAULT_SYNC_BEATS)), None),
            TransportMode::Timecode => (None, Some(t.timecode_start.unwrap_or(seed))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_press_grabs_the_nearest_marker_or_seeks() {
        let r = (0.2, 0.8);
        assert_eq!(scrub_hit(0.21, r, 0.02), ScrubHit::In);
        assert_eq!(scrub_hit(0.79, r, 0.02), ScrubHit::Out);
        assert_eq!(scrub_hit(0.5, r, 0.02), ScrubHit::Seek);
        // Markers on top of each other: left half grabs In, right half Out.
        let narrow = (0.50, 0.51);
        assert_eq!(scrub_hit(0.499, narrow, 0.02), ScrubHit::In);
        assert_eq!(scrub_hit(0.512, narrow, 0.02), ScrubHit::Out);
    }

    #[test]
    fn dragging_a_marker_never_crosses_the_other() {
        let r = (0.2, 0.8);
        assert_eq!(drag_marker(ScrubHit::In, 0.5, r), (0.5, 0.8));
        assert_eq!(drag_marker(ScrubHit::In, -3.0, r), (0.0, 0.8));
        assert_eq!(drag_marker(ScrubHit::In, 0.95, r), (0.8 - MIN_REGION, 0.8));
        assert_eq!(drag_marker(ScrubHit::Out, 0.5, r), (0.2, 0.5));
        assert_eq!(drag_marker(ScrubHit::Out, 7.0, r), (0.2, 1.0));
        assert_eq!(drag_marker(ScrubHit::Out, 0.0, r), (0.2, 0.2 + MIN_REGION));
        assert_eq!(drag_marker(ScrubHit::Seek, 0.5, r), r);
        let (a, b) = drag_marker(ScrubHit::Out, f64::NAN, r);
        assert!(b - a >= MIN_REGION - 1e-12);
    }

    #[test]
    fn transport_mode_follows_the_transport_fields() {
        let tc = vjc_model::Timecode::new(0, 0, 10, 0, vjc_model::TcFps::F25);
        let mut t = vjc_model::Transport::default();
        assert_eq!(TransportMode::of(&t), TransportMode::Timeline);
        assert_eq!(TransportMode::BpmSync.sync_for(&t, tc), (Some(4.0), None));
        t.beat_sync = Some(8.0);
        assert_eq!(TransportMode::of(&t), TransportMode::BpmSync);
        // Keeps the beats it already had; leaving it clears them.
        assert_eq!(TransportMode::BpmSync.sync_for(&t, tc), (Some(8.0), None));
        assert_eq!(TransportMode::Timecode.sync_for(&t, tc), (None, Some(tc)));
        assert_eq!(TransportMode::Timeline.sync_for(&t, tc), (None, None));
        t.timecode_start = Some(tc);
        assert_eq!(TransportMode::of(&t), TransportMode::Timecode);
    }

    #[test]
    fn region_maps_local_and_actual_positions() {
        let r = (0.25, 0.75);
        assert_eq!(to_actual(0.0, r), 0.25);
        assert_eq!(to_actual(1.0, r), 0.75);
        assert_eq!(to_actual(0.5, r), 0.5);
        assert_eq!(to_local(0.5, r), 0.5);
        assert_eq!(to_local(0.1, r), 0.0);
        assert_eq!(to_local(0.9, r), 1.0);
        let mut t = vjc_model::Transport::default();
        assert_eq!(region(&t), (0.0, 1.0));
        t.in_point = 0.2;
        t.out_point = 0.6;
        assert_eq!(region(&t), (0.2, 0.6));
        for (a, b) in [
            (0.6, 0.2),
            (0.5, 0.5),
            (-0.1, 1.0),
            (0.0, 1.5),
            (f64::NAN, 1.0),
        ] {
            t.in_point = a;
            t.out_point = b;
            assert_eq!(region(&t), (0.0, 1.0), "{a} {b}");
        }
    }

    #[test]
    fn duration_halves_and_doubles_through_speed() {
        // 10 s file, whole clip at speed 1 lasts 10 s; 5 s needs speed 2, 20 s speed 0.5.
        assert_eq!(speed_for_duration(10.0, (0.0, 1.0), 5.0), Some(2.0));
        assert_eq!(speed_for_duration(10.0, (0.0, 1.0), 20.0), Some(0.5));
        // Half the file (in/out) at speed 1 lasts 5 s.
        assert_eq!(region_secs(10.0, (0.25, 0.75), 1.0), 5.0);
        assert_eq!(speed_for_duration(10.0, (0.25, 0.75), 5.0), Some(1.0));
        // Clamped to what SetSpeed accepts; bad targets give nothing.
        assert_eq!(speed_for_duration(10.0, (0.0, 1.0), 0.5), Some(4.0));
        assert_eq!(speed_for_duration(10.0, (0.0, 1.0), 1e9), Some(0.01));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(speed_for_duration(10.0, (0.0, 1.0), bad), None);
        }
        assert!(region_secs(10.0, (0.0, 1.0), 0.0).is_infinite());
    }

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
    fn rebase_jump_is_not_a_loop() {
        let mut c = LoopCounter::default();
        c.observe(0.95, Direction::Forward);
        c.rebase(0.1);
        assert!(!c.observe(0.11, Direction::Forward));
        assert_eq!(c.count, 0);
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

#[cfg(test)]
mod gen_bar_tests {
    use super::*;

    #[test]
    fn counts_bar_changes_only() {
        let a = gen_bars(None, 7);
        assert_eq!(a, (7, 0));
        assert_eq!(gen_bars(Some(a), 7), (7, 0));
        assert_eq!(gen_bars(Some(a), 8), (8, 1));
    }
}
