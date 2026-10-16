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
        AutopilotMode::Previous => (1..=n)
            .map(|k| (current as usize + n * k - k) % n)
            .find(|&i| ok(i))
            .map(|i| i as u32),
        AutopilotMode::FirstLast => {
            let first = (0..n).find(|&i| ok(i))?;
            let last = (0..n).rev().find(|&i| ok(i))?;
            Some(if current as usize == first {
                last
            } else {
                first
            } as u32)
        }
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

/// Like `next_clip`, with the layer autopilot's random kind and loop switch. Random reuses
/// the composition autopilot's `pick` over the playable columns; `bag` is the layer's shuffle
/// state. Next/Previous stop at the ends when `loop` is off.
pub fn next_clip_ap(
    ap: &vjc_model::Autopilot,
    current: u32,
    playable: &[bool],
    rand: u64,
    bag: &mut Vec<u32>,
) -> Option<u32> {
    match ap.mode {
        AutopilotMode::Random => {
            let cols: Vec<u32> = (0..playable.len() as u32)
                .filter(|&i| playable[i as usize])
                .collect();
            let cur = cols
                .iter()
                .position(|&c| c == current)
                .unwrap_or(cols.len()) as u32;
            // Bag holds compact indices; keep it consistent with the playable list.
            crate::comp_autopilot::pick(
                vjc_model::ColumnTarget::Random,
                ap.random,
                true,
                cur,
                cols.len() as u32,
                bag,
                rand,
            )
            .and_then(|i| cols.get(i as usize).copied())
        }
        AutopilotMode::Next | AutopilotMode::Previous if !ap.looping => {
            let r = next_clip(ap.mode, current, playable, rand)?;
            let wrapped = if ap.mode == AutopilotMode::Next {
                r <= current
            } else {
                r >= current
            };
            (!wrapped).then_some(r)
        }
        mode => next_clip(mode, current, playable, rand),
    }
}

/// Time a clip has been running, for the beat/second autopilot units.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Elapsed {
    pub secs: f64,
    pub beats: f64,
}

impl Elapsed {
    /// Adds `dt` seconds at `bpm` (a bad bpm adds no beats).
    pub fn advance(&mut self, dt: f64, bpm: f64) {
        let dt = dt.max(0.0);
        self.secs += dt;
        if bpm.is_finite() && bpm > 0.0 {
            self.beats += dt * bpm / 60.0;
        }
    }
}

/// Elapsed time of a layer's timed autopilot plus the setting it was counted under, so a
/// change of unit or interval restarts the count.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedClock {
    unit: vjc_model::AutopilotUnit,
    every: f64,
    pub elapsed: Elapsed,
}

impl TimedClock {
    pub fn new(ap: &vjc_model::Autopilot) -> Self {
        Self {
            unit: ap.unit,
            every: ap.every,
            elapsed: Elapsed::default(),
        }
    }

    /// Advances by `dt` only while the clip `running` (not paused or stopped); resets when
    /// the unit or interval differ from the ones counted so far. Returns true when due.
    pub fn tick(&mut self, ap: &vjc_model::Autopilot, running: bool, dt: f64, bpm: f64) -> bool {
        if self.unit != ap.unit || self.every != ap.every {
            *self = Self::new(ap);
        }
        if running {
            self.elapsed.advance(dt, bpm);
        }
        timed_due(ap, &self.elapsed)
    }
}

/// True when the timed autopilot (`Beats` / `Seconds`) is due. `Loops` is never due here.
pub fn timed_due(ap: &vjc_model::Autopilot, e: &Elapsed) -> bool {
    use vjc_model::AutopilotUnit;
    if ap.mode == AutopilotMode::Off || !(ap.every.is_finite() && ap.every > 0.0) {
        return false;
    }
    match ap.unit {
        AutopilotUnit::Loops => false,
        AutopilotUnit::Beats => e.beats >= ap.every,
        AutopilotUnit::Seconds => e.secs >= ap.every,
    }
}

/// Playback rate that makes a region of `region_secs` seconds last `target` seconds.
/// 1.0 when either is unusable; clamped to 0.01..100 like the beat sync.
pub fn duration_rate(region_secs: f64, target: Option<f64>) -> f64 {
    match target {
        Some(t) if t.is_finite() && t > 0.0 && region_secs.is_finite() && region_secs > 0.0 => {
            (region_secs / t).clamp(0.01, 100.0)
        }
        _ => 1.0,
    }
}

/// Freeze / stop-motion: should the picture take a fresh frame now? `since` is the time
/// since the picture last refreshed. Frozen never refreshes; stop-motion at `fps` refreshes
/// every `1/fps`; otherwise every frame.
pub fn picture_refresh(freeze: bool, stop_motion_fps: f64, since: f64) -> bool {
    if freeze {
        return false;
    }
    if stop_motion_fps.is_finite() && stop_motion_fps > 0.0 {
        return since >= 1.0 / stop_motion_fps;
    }
    true
}

/// Autopilot that rules a layer playing `clip`: the clip's own when set, else the layer's.
pub fn autopilot_of<'a>(
    layer: &'a vjc_model::Layer,
    clip: Option<&'a vjc_model::Clip>,
) -> &'a vjc_model::Autopilot {
    clip.and_then(|k| k.extras.autopilot.as_ref())
        .unwrap_or(&layer.autopilot)
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
/// Direction a clip really runs in: the global reverse flips Forward and Backward; Pause
/// and Random stay as they are.
pub fn global_direction(d: vjc_model::Direction, reverse: bool) -> vjc_model::Direction {
    use vjc_model::Direction::{Backward, Forward};
    match (d, reverse) {
        (Forward, true) => Backward,
        (Backward, true) => Forward,
        _ => d,
    }
}

pub fn region(t: &vjc_model::Transport) -> (f64, f64) {
    let (a, b) = (t.in_point, t.out_point);
    if a.is_finite() && b.is_finite() && a >= 0.0 && b <= 1.0 && b - a >= MIN_REGION {
        (a, b)
    } else {
        (0.0, 1.0)
    }
}

/// Closeness to the out-point (normalised) that counts as having ended (about one frame).
const END_EPS: f64 = 1e-3;

/// Normalised file position a clip starts at when triggered.
/// `own_last`: where this clip stopped (file position); `prev_local`: how far the layer's
/// previous clip had got inside its region (0..=1). Missing data falls back to the in-point.
pub fn start_position(
    mode: vjc_model::TriggerMode,
    region: (f64, f64),
    own_last: Option<f64>,
    prev_local: Option<f64>,
) -> f64 {
    use vjc_model::TriggerMode as T;
    match mode {
        T::Continue => own_last.filter(|p| p.is_finite()).map_or(region.0, |p| {
            // A clip that ended sits at the out-point: resuming there would end it
            // again at once, so start over.
            if p >= region.1 - END_EPS {
                region.0
            } else {
                p.clamp(region.0, region.1)
            }
        }),
        T::Relative => prev_local
            .filter(|p| p.is_finite())
            .map_or(region.0, |f| to_actual(f, region)),
        _ => region.0,
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
    #[test]
    fn start_position_per_trigger_mode() {
        use vjc_model::TriggerMode as T;
        let r = (0.2, 0.6);
        // Restart ignores everything else.
        assert_eq!(
            super::start_position(T::Restart, r, Some(0.5), Some(0.9)),
            0.2
        );
        // Continue resumes, clamped into the region.
        assert_eq!(
            super::start_position(T::Continue, r, Some(0.5), Some(0.9)),
            0.5
        );
        assert_eq!(super::start_position(T::Continue, r, Some(0.9), None), 0.2);
        // A clip that ended at the out-point restarts from the in-point.
        assert_eq!(super::start_position(T::Continue, r, Some(0.6), None), 0.2);
        assert_eq!(super::start_position(T::Continue, r, Some(0.5), None), 0.5);
        assert_eq!(super::start_position(T::Continue, r, None, Some(0.9)), 0.2);
        // Relative maps the previous fraction onto this region (0.5 of 0.2..0.6 = 0.4).
        let rel = super::start_position(T::Relative, r, Some(0.5), Some(0.5));
        assert!((rel - 0.4).abs() < 1e-9);
        assert_eq!(super::start_position(T::Relative, r, Some(0.5), None), 0.2);
        assert_eq!(
            super::start_position(T::Relative, r, None, Some(f64::NAN)),
            0.2
        );
    }

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
    fn previous_wraps_backwards_and_skips_empty() {
        let p = [true, false, true, false];
        assert_eq!(next_clip(AutopilotMode::Previous, 2, &p, 0), Some(0));
        assert_eq!(next_clip(AutopilotMode::Previous, 0, &p, 0), Some(2));
        assert_eq!(next_clip(AutopilotMode::Previous, 3, &p, 0), Some(2));
        assert_eq!(next_clip(AutopilotMode::Previous, 0, &[], 0), None);
        // Negative control: Previous is not Next.
        let three = [true; 3];
        assert_eq!(next_clip(AutopilotMode::Previous, 1, &three, 0), Some(0));
        assert_eq!(next_clip(AutopilotMode::Next, 1, &three, 0), Some(2));
    }

    #[test]
    fn first_last_alternates_between_the_ends_of_the_playable_clips() {
        let p = [false, true, true, true, false];
        assert_eq!(next_clip(AutopilotMode::FirstLast, 1, &p, 0), Some(3));
        assert_eq!(next_clip(AutopilotMode::FirstLast, 3, &p, 0), Some(1));
        assert_eq!(next_clip(AutopilotMode::FirstLast, 2, &p, 0), Some(1));
        assert_eq!(next_clip(AutopilotMode::FirstLast, 0, &[false], 0), None);
        assert_eq!(next_clip(AutopilotMode::FirstLast, 0, &[true], 0), Some(0));
    }

    #[test]
    fn timed_autopilot_is_due_by_seconds_or_beats_only_for_its_unit() {
        use vjc_model::{Autopilot, AutopilotUnit};
        let ap = |unit, every| Autopilot {
            looping: true,
            random: vjc_model::RandomKind::Other,
            mode: AutopilotMode::Next,
            every_loops: 1,
            unit,
            every,
        };
        let mut e = Elapsed::default();
        // 120 bpm: 0.5 s per beat. 3.9 s = 7.8 beats.
        e.advance(3.9, 120.0);
        assert!(!timed_due(&ap(AutopilotUnit::Seconds, 4.0), &e));
        assert!(!timed_due(&ap(AutopilotUnit::Beats, 8.0), &e));
        e.advance(0.1, 120.0);
        assert!(timed_due(&ap(AutopilotUnit::Seconds, 4.0), &e));
        assert!(timed_due(&ap(AutopilotUnit::Beats, 8.0), &e));
        // Negative controls: loops unit, Off mode, a bad interval and a bad bpm never fire.
        assert!(!timed_due(&ap(AutopilotUnit::Loops, 1.0), &e));
        let off = Autopilot {
            mode: AutopilotMode::Off,
            ..ap(AutopilotUnit::Seconds, 1.0)
        };
        assert!(!timed_due(&off, &e));
        assert!(!timed_due(&ap(AutopilotUnit::Seconds, 0.0), &e));
        assert!(!timed_due(&ap(AutopilotUnit::Seconds, f64::NAN), &e));
        let mut z = Elapsed::default();
        z.advance(100.0, 0.0);
        assert!(!timed_due(&ap(AutopilotUnit::Beats, 1.0), &z));
    }

    #[test]
    fn timed_clock_pauses_and_resets_on_change() {
        use vjc_model::{Autopilot, AutopilotUnit};
        let ap = Autopilot {
            mode: AutopilotMode::Next,
            unit: AutopilotUnit::Seconds,
            every: 4.0,
            ..Autopilot::default()
        };
        let mut c = TimedClock::new(&ap);
        assert!(!c.tick(&ap, true, 3.0, 120.0));
        // Paused for a long while: no progress, so it is not due.
        assert!(!c.tick(&ap, false, 100.0, 120.0));
        assert_eq!(c.elapsed.secs, 3.0);
        assert!(c.tick(&ap, true, 1.0, 120.0));
        // Changing the interval restarts the count.
        let ap2 = Autopilot {
            every: 5.0,
            ..ap.clone()
        };
        assert!(!c.tick(&ap2, true, 1.0, 120.0));
        assert_eq!(c.elapsed.secs, 1.0);
        let ap3 = Autopilot {
            unit: AutopilotUnit::Beats,
            every: 5.0,
            ..ap2
        };
        c.tick(&ap3, true, 0.5, 120.0);
        assert_eq!(c.elapsed.secs, 0.5);
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

#[cfg(test)]
mod ap_tests {
    use super::*;
    use vjc_model::{Autopilot, RandomKind};

    fn ap(mode: AutopilotMode) -> Autopilot {
        Autopilot {
            mode,
            ..Autopilot::default()
        }
    }

    #[test]
    fn loop_off_stops_at_the_ends() {
        let mut a = ap(AutopilotMode::Next);
        a.looping = false;
        let pl = [true, true, true];
        let mut bag = vec![];
        assert_eq!(next_clip_ap(&a, 1, &pl, 0, &mut bag), Some(2));
        assert_eq!(next_clip_ap(&a, 2, &pl, 0, &mut bag), None);
        a.looping = true;
        assert_eq!(next_clip_ap(&a, 2, &pl, 0, &mut bag), Some(0));
        let mut b = ap(AutopilotMode::Previous);
        b.looping = false;
        assert_eq!(next_clip_ap(&b, 0, &pl, 0, &mut bag), None);
    }

    #[test]
    fn random_bag_plays_every_playable_clip_before_repeating() {
        let mut a = ap(AutopilotMode::Random);
        a.random = RandomKind::Bag;
        let pl = [true, false, true, true];
        let mut bag = vec![];
        let mut cur = 0;
        let mut seen = vec![];
        for r in 0..2u64 {
            cur = next_clip_ap(&a, cur, &pl, r + 7, &mut bag).unwrap();
            assert!(pl[cur as usize]);
            seen.push(cur);
        }
        seen.sort();
        assert_eq!(seen, vec![2, 3]);
    }

    #[test]
    fn random_any_may_repeat_and_other_never_does() {
        let pl = [true, true];
        let mut bag = vec![];
        let mut a = ap(AutopilotMode::Random);
        a.random = RandomKind::Other;
        assert_eq!(next_clip_ap(&a, 0, &pl, 5, &mut bag), Some(1));
        a.random = RandomKind::Any;
        assert_eq!(next_clip_ap(&a, 0, &pl, 4, &mut bag), Some(0));
    }
}

#[cfg(test)]
mod global_transport_tests {
    use super::global_direction;
    use crate::{Command, Event, History, apply};
    use std::time::{Duration, Instant};
    use vjc_model::{Direction::*, PlayState, Project};

    #[test]
    fn global_reverse_flips_only_forward_and_backward() {
        assert_eq!(global_direction(Forward, true), Backward);
        assert_eq!(global_direction(Backward, true), Forward);
        assert_eq!(global_direction(Pause, true), Pause);
        assert_eq!(global_direction(Random, true), Random);
        assert_eq!(global_direction(Forward, false), Forward);
    }

    #[test]
    fn stop_ejects_every_unlocked_layer_and_parks_the_clock_on_beat_zero() {
        let (mut p, _) = crate::tests::setup();
        p.composition.layers.push(crate::tests::layer(2));
        p.composition.layers[1].lock_content = true;
        let r = apply(
            &mut p,
            Command::SetTransport {
                state: PlayState::Stop,
            },
        )
        .unwrap();
        assert_eq!(p.composition.tempo.play_state, PlayState::Stop);
        assert!(r.inverse.is_none(), "a performance action, not an edit");
        let cleared: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| match e {
                Event::LayerCleared { layer } => Some(*layer),
                _ => None,
            })
            .collect();
        assert_eq!(
            cleared,
            vec![p.composition.layers[0].id],
            "locked layer stays"
        );
        // The app drives the clock from the saved state: the phase goes back to 0.
        let t0 = Instant::now();
        let mut clock = crate::clock::Clock::new(120.0, t0);
        let t1 = t0 + Duration::from_secs(3);
        assert!(clock.phase(t1) > 5.0);
        clock.stop(t1);
        assert_eq!(clock.phase(t1 + Duration::from_secs(1)), 0.0);
        // Pause and play eject nothing.
        let r = apply(
            &mut p,
            Command::SetTransport {
                state: PlayState::Pause,
            },
        )
        .unwrap();
        assert!(r.events.is_empty());
    }

    #[test]
    fn reverse_and_metronome_undo_and_old_files_load() {
        let (mut p, _) = crate::tests::setup();
        let mut h = History::new();
        let t = Instant::now();
        h.exec(&mut p, Command::SetMasterReverse { reverse: true }, t)
            .unwrap();
        h.exec(&mut p, Command::SetMetronome { on: true }, t)
            .unwrap();
        assert!(p.composition.master_reverse && p.composition.tempo.metronome);
        h.undo(&mut p).unwrap().unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert!(!p.composition.master_reverse && !p.composition.tempo.metronome);
        let mut v = serde_json::to_value(Project::new("t")).unwrap();
        v["composition"]
            .as_object_mut()
            .unwrap()
            .remove("master_reverse");
        let old: Project = serde_json::from_value(v).unwrap();
        assert!(!old.composition.master_reverse);
    }

    #[test]
    fn transport_addresses_resolve() {
        let p = Project::new("t");
        let r = |a: &str, v: f64| crate::shortcuts::resolve(&p, a, v);
        assert_eq!(
            r("/comp/transport/stop", 1.0),
            Some(Command::SetTransport {
                state: PlayState::Stop
            })
        );
        assert_eq!(r("/comp/transport/stop", 0.0), None, "release does nothing");
        assert_eq!(
            r("/comp/transport/reverse", 1.0),
            Some(Command::SetMasterReverse { reverse: true })
        );
        assert_eq!(
            r("/comp/tempo/metronome", 0.0),
            Some(Command::SetMetronome { on: false })
        );
    }

    #[test]
    fn duration_rate_fits_the_region_to_the_seconds() {
        assert_eq!(super::duration_rate(10.0, Some(5.0)), 2.0);
        assert_eq!(super::duration_rate(10.0, Some(20.0)), 0.5);
        // Negative controls: unset or unusable input leaves the speed alone.
        assert_eq!(super::duration_rate(10.0, None), 1.0);
        assert_eq!(super::duration_rate(10.0, Some(0.0)), 1.0);
        assert_eq!(super::duration_rate(0.0, Some(3.0)), 1.0);
        assert_eq!(super::duration_rate(10.0, Some(f64::NAN)), 1.0);
        assert_eq!(super::duration_rate(1000.0, Some(1.0)), 100.0);
    }

    #[test]
    fn freeze_and_stop_motion_gate_the_refresh() {
        assert!(!super::picture_refresh(true, 0.0, 99.0));
        assert!(!super::picture_refresh(true, 12.0, 99.0));
        assert!(!super::picture_refresh(false, 10.0, 0.05));
        assert!(super::picture_refresh(false, 10.0, 0.1));
        assert!(super::picture_refresh(false, 0.0, 0.0));
        assert!(super::picture_refresh(false, f64::NAN, 0.0));
    }
}
