//! Parameter animation: a pure evaluator. `Param.value` stays the saved base; the effective
//! value is computed per frame from beat phase, time, FFT bands and dashboard dials, so
//! animating never dirties the project nor enters the history.

use vjc_model::{
    AnimDirection, Animation, Curve, DialScope, FftBand, LoopMode, Motion, Param, Value,
};

/// Dials per composition.
pub const DIALS: usize = 8;

/// Inputs of one frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnimCtx<'a> {
    /// Master clock phase, in beats.
    pub beat_phase: f64,
    pub time_secs: f64,
    /// Audio levels 0..1: low, low-mid, mid, high, overall level, then raw bins. Empty when
    /// there is no audio input.
    pub fft_bands: &'a [f32],
    /// Holds attack/release state of audio animations; `None` skips the smoothing.
    pub smoother: Option<&'a AudioSmoother>,
    /// Dial values 0..1 (missing dials read 0).
    pub dashboard: &'a [f64],
    /// Dials of the layer the evaluated param belongs to (`DialScope::Layer`); `None` for
    /// params outside any layer, whose Layer-scoped animations fall back to the base value.
    pub layer_dials: Option<&'a [f64]>,
    /// Dials of the clip the evaluated param belongs to (`DialScope::Clip`); `None` as above.
    pub clip_dials: Option<&'a [f64]>,
}

impl AnimCtx<'_> {
    /// This frame's context for params owned by a layer and, when given, its clip.
    pub fn scoped<'b>(&self, layer: &'b [f64], clip: &'b [f64]) -> AnimCtx<'b>
    where
        Self: 'b,
    {
        AnimCtx {
            layer_dials: Some(layer),
            clip_dials: Some(clip),
            ..*self
        }
    }
}

/// Current dial values (0..=1) of a dials list, padded with zeros to `DIALS`.
pub fn dial_values(dials: &[vjc_model::Param]) -> Vec<f64> {
    (0..DIALS)
        .map(|i| match dials.get(i).map(|p| &p.value) {
            Some(Value::Number(n)) => *n,
            _ => 0.0,
        })
        .collect()
}

/// One step of attack/release smoothing: `prev` moves toward `target` with time constant
/// `attack` (rising) or `release` (falling), both in seconds; 0 means instant.
pub fn smooth_step(prev: f64, target: f64, dt: f64, attack: f64, release: f64) -> f64 {
    let tau = if target > prev { attack } else { release };
    if tau <= 0.0 || !tau.is_finite() || dt.is_nan() {
        return target;
    }
    prev + (target - prev) * (1.0 - (-dt.max(0.0) / tau).exp())
}

type SmoothKey = [u32; 6];

/// Entry count above which unused smoothing states are pruned.
const PRUNE_ABOVE: usize = 64;
/// A state not touched for this many seconds is unused.
const STALE_SECS: f64 = 5.0;

/// Smoothing state of audio animations. Two params with identical audio settings read the
/// same signal, so they share one state, keyed by the settings; the state moves once per
/// distinct `time_secs`, so evaluating the same frame many times is idempotent.
#[derive(Debug, Default)]
pub struct AudioSmoother {
    state: std::sync::Mutex<std::collections::HashMap<SmoothKey, (f64, f64)>>,
}

impl AudioSmoother {
    fn apply(&self, key: SmoothKey, target: f64, time: f64, attack: f64, release: f64) -> f64 {
        let mut m = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let e = m.entry(key).or_insert((time, target));
        if time > e.0 {
            e.1 = smooth_step(e.1, target, time - e.0, attack, release);
            e.0 = time;
        } else if time < e.0 {
            *e = (time, target);
        }
        let out = e.1;
        // Dragging a setting makes a new key per value: drop the ones not read lately.
        if m.len() > PRUNE_ABOVE {
            m.retain(|_, v| (time - v.0).abs() <= STALE_SECS);
        }
        out
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state.lock().unwrap().len()
    }
}

fn hash01(n: i64) -> f64 {
    let mut x = (n as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// Shape of `curve` at `cycles` (cycle count, fractional part is the position), 0..=1.
/// Linear is a triangle, Saw a rising ramp, Random holds one value per cycle.
pub fn shape(curve: &Curve, cycles: f64) -> f64 {
    let f = cycles.rem_euclid(1.0);
    match curve {
        Curve::Linear => 1.0 - (2.0 * f - 1.0).abs(),
        Curve::Sine => 0.5 - 0.5 * (std::f64::consts::TAU * f).cos(),
        Curve::Saw => f,
        Curve::Square => f64::from(u8::from(f < 0.5)),
        Curve::Random => hash01(cycles.floor() as i64),
    }
}

fn envelope(points: &[(f64, f64)], x: f64) -> f64 {
    let mut pts: Vec<(f64, f64)> = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let x = x.rem_euclid(1.0);
    match pts.as_slice() {
        [] => 0.0,
        [(_, y)] => *y,
        [first, .., last] if x <= first.0 => {
            let _ = last;
            first.1
        }
        [.., last] if x >= last.0 => last.1,
        _ => pts
            .windows(2)
            .find(|w| x >= w[0].0 && x <= w[1].0)
            .map_or(0.0, |w| {
                let span = w[1].0 - w[0].0;
                if span <= 0.0 {
                    w[1].1
                } else {
                    w[0].1 + (w[1].1 - w[0].1) * (x - w[0].0) / span
                }
            }),
    }
}

fn band(b: &FftBand, bands: &[f32]) -> f64 {
    let i = match b {
        FftBand::Low => 0,
        FftBand::LowMid => 1,
        FftBand::Mid => 2,
        FftBand::High => 3,
        FftBand::Level => 4,
        FftBand::Bin(n) => 5usize.saturating_add(*n as usize),
    };
    bands.get(i).copied().map_or(0.0, f64::from)
}

fn safe_div(a: f64, b: f64) -> f64 {
    if b.is_finite() && b > 0.0 { a / b } else { 0.0 }
}

/// Ready-made envelope shapes. `points` gives ordinary envelope points (x = position in
/// the cycle, y = level, both 0..=1), so a preset is just a starting point to edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvPreset {
    RampUp,
    RampDown,
    Sine,
    Saw,
    Square,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl EnvPreset {
    pub const ALL: [EnvPreset; 8] = [
        EnvPreset::RampUp,
        EnvPreset::RampDown,
        EnvPreset::Sine,
        EnvPreset::Saw,
        EnvPreset::Square,
        EnvPreset::EaseIn,
        EnvPreset::EaseOut,
        EnvPreset::EaseInOut,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EnvPreset::RampUp => "Ramp up",
            EnvPreset::RampDown => "Ramp down",
            EnvPreset::Sine => "Sine",
            EnvPreset::Saw => "Saw",
            EnvPreset::Square => "Square",
            EnvPreset::EaseIn => "Ease in",
            EnvPreset::EaseOut => "Ease out",
            EnvPreset::EaseInOut => "Ease in/out",
        }
    }

    /// Envelope points of the shape, sorted by x, first at 0 and last at 1.
    pub fn points(self) -> Vec<(f64, f64)> {
        // Curves are sampled; the envelope interpolates linearly between the samples.
        let sampled = |n: usize, f: &dyn Fn(f64) -> f64| -> Vec<(f64, f64)> {
            (0..=n)
                .map(|i| {
                    let x = i as f64 / n as f64;
                    (x, f(x).clamp(0.0, 1.0))
                })
                .collect()
        };
        // A jump is a pair of points a hair apart.
        const JUMP: f64 = 0.001;
        match self {
            EnvPreset::RampUp => vec![(0.0, 0.0), (1.0, 1.0)],
            EnvPreset::RampDown => vec![(0.0, 1.0), (1.0, 0.0)],
            EnvPreset::Sine => sampled(16, &|x| 0.5 - 0.5 * (std::f64::consts::TAU * x).cos()),
            EnvPreset::Saw => vec![(0.0, 0.0), (1.0 - JUMP, 1.0), (1.0, 0.0)],
            EnvPreset::Square => vec![(0.0, 1.0), (0.5 - JUMP, 1.0), (0.5, 0.0), (1.0, 0.0)],
            EnvPreset::EaseIn => sampled(8, &|x| x * x * x),
            EnvPreset::EaseOut => sampled(8, &|x| 1.0 - (1.0 - x).powi(3)),
            EnvPreset::EaseInOut => sampled(8, &|x| x * x * (3.0 - 2.0 * x)),
        }
    }
}

/// One cycle of a Timeline, Beats or Envelope animation as `n` samples in 0..=1 (sample `i`
/// is at `i / n` of the cycle), for drawing the curve. Other kinds have no cycle: empty.
pub fn preview(anim: &Animation, n: usize) -> Vec<f64> {
    let cycle = match anim {
        Animation::Timeline { duration_secs, .. } => *duration_secs,
        Animation::Beats { beats, .. } | Animation::Envelope { beats, .. } => *beats,
        _ => return Vec::new(),
    };
    (0..n)
        .map(|i| {
            let at = cycle * i as f64 / n as f64;
            let ctx = match anim {
                Animation::Timeline { .. } => AnimCtx {
                    time_secs: at,
                    ..Default::default()
                },
                _ => AnimCtx {
                    beat_phase: at,
                    ..Default::default()
                },
            };
            unit(anim, &ctx)
        })
        .collect()
}

/// Index of the envelope point closest to (x, y) within `radius` (in the same 0..1 units,
/// x and y weighted equally), or `None` when every point is farther.
pub fn env_nearest(points: &[(f64, f64)], x: f64, y: f64, radius: f64) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| (i, (p.0 - x).hypot(p.1 - y)))
        .filter(|(_, d)| *d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Moves point `i` to (x, y) clamped to 0..=1, and in x also between its neighbours so the
/// points stay sorted by x. False when `i` is out of range or the position is not finite
/// (nothing changes).
pub fn env_move(points: &mut [(f64, f64)], i: usize, x: f64, y: f64) -> bool {
    if i >= points.len() || !x.is_finite() || !y.is_finite() {
        return false;
    }
    let lo = if i > 0 { points[i - 1].0 } else { 0.0 };
    let hi = points.get(i + 1).map_or(1.0, |p| p.0);
    points[i] = (x.clamp(lo.min(hi), hi.max(lo)), y.clamp(0.0, 1.0));
    true
}

/// Inserts a point at (x, y) clamped to 0..=1, keeping the points sorted by x. Returns its
/// index, or `None` when the position is not finite.
pub fn env_insert(points: &mut Vec<(f64, f64)>, x: f64, y: f64) -> Option<usize> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
    let at = points.partition_point(|p| p.0 <= x);
    points.insert(at, (x, y));
    Some(at)
}

/// Applies loop mode and direction to a position in cycles (`x` grows with the clock).
/// Loop keeps the continuous position (backward = negated); PingPong folds it into 0..1.
pub fn motion_phase(m: Motion, x: f64) -> f64 {
    let back = m.direction == AnimDirection::Backward;
    let p = match m.mode {
        LoopMode::Loop => return if back { -x } else { x },
        LoopMode::PingPong => 1.0 - ((x.rem_euclid(2.0)) - 1.0).abs(),
    };
    let p = p.min(1.0 - 1e-9);
    if back { 1.0 - p } else { p }
}

/// Normalised 0..=1 output of the animation.
fn unit(anim: &Animation, ctx: &AnimCtx) -> f64 {
    let t = match anim {
        Animation::Timeline {
            duration_secs,
            curve,
            motion,
        } => shape(
            curve,
            motion_phase(*motion, safe_div(ctx.time_secs, *duration_secs)),
        ),
        Animation::Beats {
            beats,
            curve,
            motion,
        } => shape(
            curve,
            motion_phase(*motion, safe_div(ctx.beat_phase, *beats)),
        ),
        Animation::Audio {
            band: b,
            gain,
            threshold,
            attack,
            release,
            min,
            max,
        } => {
            let v = band(b, ctx.fft_bands);
            let raw = if v < f64::from(*threshold) {
                0.0
            } else {
                (v * f64::from(*gain)).clamp(0.0, 1.0)
            };
            let level = match ctx.smoother {
                Some(sm) if raw.is_finite() => {
                    let key = [
                        match b {
                            FftBand::Low => 0,
                            FftBand::LowMid => 1,
                            FftBand::Mid => 2,
                            FftBand::High => 3,
                            FftBand::Level => 4,
                            FftBand::Bin(n) => 5u32.saturating_add(*n),
                        },
                        gain.to_bits(),
                        threshold.to_bits(),
                        attack.to_bits(),
                        release.to_bits(),
                        0,
                    ];
                    sm.apply(
                        key,
                        raw,
                        ctx.time_secs,
                        f64::from(*attack),
                        f64::from(*release),
                    )
                }
                _ => raw,
            };
            f64::from(*min) + level * f64::from(*max - *min)
        }
        Animation::Envelope {
            points,
            beats,
            motion,
        } => envelope(
            points,
            motion_phase(*motion, safe_div(ctx.beat_phase, *beats)),
        ),
        Animation::Dashboard { dial, scope } => {
            let dials = match scope {
                DialScope::Comp => ctx.dashboard,
                DialScope::Layer => ctx.layer_dials.unwrap_or(&[]),
                DialScope::Clip => ctx.clip_dials.unwrap_or(&[]),
            };
            dials.get(*dial as usize).copied().unwrap_or(0.0)
        }
    };
    if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Animated value of a numeric param, always inside `param.range` (0..=1 if none).
pub fn evaluate(param: &Param, anim: &Animation, ctx: &AnimCtx) -> f64 {
    let (lo, hi) = param.range.unwrap_or((0.0, 1.0));
    if out_of_scope(anim, ctx) {
        let base = if let Value::Number(n) = param.value {
            n
        } else {
            lo
        };
        return base.clamp(lo.min(hi), lo.max(hi));
    }
    (lo + unit(anim, ctx) * (hi - lo)).clamp(lo.min(hi), lo.max(hi))
}

/// A dial animation whose scope has no owner in this context (a Layer/Clip dial read by a
/// composition-level param).
fn out_of_scope(anim: &Animation, ctx: &AnimCtx) -> bool {
    matches!(anim, Animation::Dashboard { scope: DialScope::Layer, .. } if ctx.layer_dials.is_none())
        || matches!(anim, Animation::Dashboard { scope: DialScope::Clip, .. } if ctx.clip_dials.is_none())
}

/// The value used for rendering and display: the animation if any, else the base value.
pub fn effective(param: &Param, ctx: &AnimCtx) -> Value {
    match (&param.value, &param.animation) {
        (Value::Number(_), Some(a)) => Value::Number(evaluate(param, a, ctx)),
        (v, _) => v.clone(),
    }
}

/// Effective number of a numeric param (non numeric reads 0).
pub fn effective_number(param: &Param, ctx: &AnimCtx) -> f64 {
    match effective(param, ctx) {
        Value::Number(n) => n,
        _ => 0.0,
    }
}

/// Params with animations resolved (a plain copy when none is animated).
pub fn resolve(params: &[Param], ctx: &AnimCtx) -> Vec<Param> {
    params
        .iter()
        .map(|p| {
            let mut q = p.clone();
            q.value = effective(p, ctx);
            q
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(range: (f64, f64)) -> Param {
        Param::number("x", range.0, Some(range))
    }
    fn ctx<'a>(beat: f64, time: f64, fft: &'a [f32], dash: &'a [f64]) -> AnimCtx<'a> {
        AnimCtx {
            beat_phase: beat,
            time_secs: time,
            fft_bands: fft,
            smoother: None,
            dashboard: dash,
            ..Default::default()
        }
    }
    fn beats(c: Curve) -> Animation {
        Animation::Beats {
            motion: Default::default(),
            beats: 4.0,
            curve: c,
        }
    }
    fn at(c: Curve, beat: f64) -> f64 {
        evaluate(&p((0.0, 10.0)), &beats(c), &ctx(beat, 0.0, &[], &[]))
    }

    #[test]
    fn smoother_prunes_unused_keys() {
        let sm = AudioSmoother::default();
        for i in 0..1000u32 {
            let key = [i, 0, 0, 0, 0, 0];
            sm.apply(key, 0.5, f64::from(i) * 0.1, 0.1, 0.1);
        }
        assert!(sm.len() <= 64 + 51, "{}", sm.len());
        // A key in use survives pruning.
        let live = [9999, 0, 0, 0, 0, 0];
        for i in 0..1000u32 {
            let t = 200.0 + f64::from(i) * 0.1;
            sm.apply(live, 1.0, t, 0.1, 0.1);
            sm.apply([i, 1, 0, 0, 0, 0], 0.0, t, 0.1, 0.1);
        }
        assert!(sm.state.lock().unwrap().contains_key(&live));
        assert!(sm.len() <= 64 + 51);
    }

    #[test]
    fn bin_u32_max_does_not_overflow() {
        let a = Animation::Audio {
            band: FftBand::Bin(u32::MAX),
            gain: 1.0,
            threshold: 0.0,
            attack: 0.5,
            release: 0.5,
            min: 0.0,
            max: 1.0,
        };
        let sm = AudioSmoother::default();
        let mut c = ctx(0.0, 1.0, &[0.5; 8], &[]);
        c.smoother = Some(&sm);
        assert_eq!(evaluate(&p((0.0, 1.0)), &a, &c), 0.0);
    }

    #[test]
    fn curves() {
        assert_eq!(at(Curve::Saw, 1.0), 2.5);
        assert_eq!(at(Curve::Linear, 1.0), 5.0);
        assert_eq!(at(Curve::Linear, 2.0), 10.0);
        assert!((at(Curve::Sine, 2.0) - 10.0).abs() < 1e-9);
        assert!(at(Curve::Sine, 0.0).abs() < 1e-9);
        assert_eq!(at(Curve::Square, 1.0), 10.0);
        assert_eq!(at(Curve::Square, 3.0), 0.0);
    }

    #[test]
    fn random_is_deterministic_and_holds_per_cycle() {
        let a = at(Curve::Random, 5.0);
        assert_eq!(a, at(Curve::Random, 5.0));
        assert_eq!(a, at(Curve::Random, 6.5));
        assert_ne!(a, at(Curve::Random, 9.0));
        assert!((0.0..=10.0).contains(&a));
    }

    #[test]
    fn preview_samples_one_cycle_of_each_curve() {
        let tl = |curve| Animation::Timeline {
            motion: Default::default(),
            duration_secs: 3.0,
            curve,
        };
        let saw = preview(&tl(Curve::Saw), 4);
        assert_eq!(saw, vec![0.0, 0.25, 0.5, 0.75]);
        let sq = preview(&tl(Curve::Square), 4);
        assert_eq!(sq, vec![1.0, 1.0, 0.0, 0.0]);
        let b = Animation::Beats {
            motion: Default::default(),
            beats: 4.0,
            curve: Curve::Saw,
        };
        assert_eq!(
            preview(&b, 4),
            saw,
            "cycle length does not change the shape"
        );
        let env = Animation::Envelope {
            motion: Default::default(),
            points: vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)],
            beats: 8.0,
        };
        assert_eq!(preview(&env, 4), vec![0.0, 0.5, 1.0, 0.5]);
        // Negative controls: no cycle, no samples.
        assert!(
            preview(
                &Animation::Dashboard {
                    dial: 0,
                    scope: Default::default()
                },
                4
            )
            .is_empty()
        );
        assert!(preview(&tl(Curve::Saw), 0).is_empty());
    }

    #[test]
    fn envelope_points_pick_and_drag() {
        let mut pts = vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)];
        assert_eq!(env_nearest(&pts, 0.52, 0.9, 0.2), Some(1));
        // Negative control: nothing within the radius.
        assert_eq!(env_nearest(&pts, 0.25, 0.5, 0.1), None);
        assert_eq!(env_nearest(&[], 0.5, 0.5, 1.0), None);
        assert!(env_move(&mut pts, 1, 0.75, 0.25));
        assert_eq!(pts[1], (0.75, 0.25));
        // Clamped to the unit square.
        assert!(env_move(&mut pts, 1, 3.0, -2.0));
        assert_eq!(pts[1], (1.0, 0.0));
        // Rejected moves leave the points alone.
        let before = pts.clone();
        assert!(!env_move(&mut pts, 9, 0.5, 0.5));
        assert!(!env_move(&mut pts, 0, f64::NAN, 0.5));
        assert_eq!(pts, before);
        // The dragged envelope changes the evaluated value.
        let mut a = Animation::Envelope {
            motion: Default::default(),
            points: vec![(0.0, 0.0), (1.0, 1.0)],
            beats: 2.0,
        };
        let v = |a: &Animation| evaluate(&p((0.0, 1.0)), a, &ctx(1.0, 0.0, &[], &[]));
        assert_eq!(v(&a), 0.5);
        if let Animation::Envelope { points, .. } = &mut a {
            env_move(points, 1, 1.0, 0.0);
        }
        assert_eq!(v(&a), 0.0);
    }

    #[test]
    fn envelope_edits_keep_points_sorted() {
        let mut pts = vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)];
        // Dragging past a neighbour stops at it.
        assert!(env_move(&mut pts, 1, 0.95, 0.5));
        assert_eq!(pts[1], (0.95, 0.5));
        assert!(env_move(&mut pts, 1, 1.0, 0.5));
        assert!(env_move(&mut pts, 2, 0.2, 0.0));
        assert_eq!(pts[2].0, 1.0, "last point cannot cross the previous one");
        assert!(env_move(&mut pts, 0, 0.9, 0.0));
        assert_eq!(pts[0].0, 0.9);
        assert!(pts.windows(2).all(|w| w[0].0 <= w[1].0));
        // Inserting in the middle lands in order, not at the end.
        let mut pts = vec![(0.0, 0.0), (1.0, 1.0)];
        assert_eq!(env_insert(&mut pts, 0.4, 0.7), Some(1));
        assert_eq!(pts, [(0.0, 0.0), (0.4, 0.7), (1.0, 1.0)]);
        assert_eq!(env_insert(&mut pts, f64::NAN, 0.0), None);
        assert!(pts.windows(2).all(|w| w[0].0 <= w[1].0));
    }

    #[test]
    fn timeline_uses_seconds() {
        let a = Animation::Timeline {
            motion: Default::default(),
            duration_secs: 2.0,
            curve: Curve::Saw,
        };
        assert_eq!(evaluate(&p((0.0, 1.0)), &a, &ctx(9.0, 0.5, &[], &[])), 0.25);
    }

    #[test]
    fn envelope_interpolates() {
        let a = Animation::Envelope {
            motion: Default::default(),
            points: vec![(1.0, 0.0), (0.0, 1.0)],
            beats: 2.0,
        };
        let v = |b| evaluate(&p((0.0, 1.0)), &a, &ctx(b, 0.0, &[], &[]));
        assert_eq!(v(0.0), 1.0);
        assert_eq!(v(1.0), 0.5);
        assert_eq!(v(0.5), 0.75);
    }

    #[test]
    fn audio_and_dashboard() {
        let au = Animation::Audio {
            band: FftBand::Mid,
            gain: 2.0,
            threshold: 0.1,
            attack: 0.0,
            release: 0.0,
            min: 0.0,
            max: 1.0,
        };
        let pr = p((0.0, 1.0));
        assert_eq!(
            evaluate(&pr, &au, &ctx(0.0, 0.0, &[0.0, 0.0, 0.25, 0.0], &[])),
            0.5
        );
        assert_eq!(
            evaluate(&pr, &au, &ctx(0.0, 0.0, &[0.0, 0.0, 0.05, 0.0], &[])),
            0.0
        );
        assert_eq!(evaluate(&pr, &au, &ctx(0.0, 0.0, &[], &[])), 0.0);
        let d = Animation::Dashboard {
            dial: 1,
            scope: Default::default(),
        };
        assert_eq!(evaluate(&pr, &d, &ctx(0.0, 0.0, &[], &[0.0, 0.7])), 0.7);
        assert_eq!(evaluate(&pr, &d, &ctx(0.0, 0.0, &[], &[])), 0.0);
    }

    #[test]
    fn clamps_to_range_and_keeps_base() {
        let mut pr = p((2.0, 3.0));
        pr.animation = Some(Animation::Audio {
            band: FftBand::Low,
            gain: 50.0,
            threshold: 0.0,
            attack: 0.0,
            release: 0.0,
            min: 0.0,
            max: 1.0,
        });
        let c = ctx(0.0, 0.0, &[1.0], &[]);
        assert_eq!(effective_number(&pr, &c), 3.0);
        assert_eq!(pr.value, Value::Number(2.0));
        let bad = Animation::Beats {
            motion: Default::default(),
            beats: 0.0,
            curve: Curve::Saw,
        };
        assert_eq!(evaluate(&pr, &bad, &c), 2.0);
    }

    fn audio(attack: f32, release: f32, min: f32, max: f32) -> Animation {
        Animation::Audio {
            band: FftBand::Low,
            gain: 1.0,
            threshold: 0.0,
            attack,
            release,
            min,
            max,
        }
    }

    #[test]
    fn audio_attack_release_smooth_and_idempotent_per_frame() {
        let sm = AudioSmoother::default();
        let a = audio(0.5, 1.0, 0.0, 1.0);
        let pr = p((0.0, 1.0));
        let at = |t: f64, lvl: f32| {
            let f = [lvl];
            let c = AnimCtx {
                time_secs: t,
                fft_bands: &f,
                smoother: Some(&sm),
                ..AnimCtx::default()
            };
            evaluate(&pr, &a, &c)
        };
        assert_eq!(at(0.0, 0.0), 0.0);
        let up = at(0.1, 1.0);
        assert!(up > 0.0 && up < 1.0, "attack is gradual: {up}");
        assert_eq!(at(0.1, 1.0), up, "same frame, same value");
        let top = at(5.0, 1.0);
        assert!(top > 0.99);
        let down = at(5.1, 0.0);
        assert!(
            down < top && down > 0.5,
            "release is slower than attack: {down}"
        );
        // negative control: no smoother means the raw value
        let raw = AnimCtx {
            fft_bands: &[1.0],
            ..AnimCtx::default()
        };
        assert_eq!(evaluate(&pr, &a, &raw), 1.0);
    }

    #[test]
    fn audio_min_max_window() {
        let a = audio(0.0, 0.0, 0.25, 0.75);
        let pr = p((0.0, 1.0));
        let c = |l: &'static [f32]| AnimCtx {
            fft_bands: l,
            ..AnimCtx::default()
        };
        assert_eq!(evaluate(&pr, &a, &c(&[0.0])), 0.25);
        assert_eq!(evaluate(&pr, &a, &c(&[1.0])), 0.75);
    }

    #[test]
    fn smooth_step_instant_when_zero() {
        assert_eq!(smooth_step(0.0, 1.0, 0.016, 0.0, 0.0), 1.0);
        assert!(smooth_step(0.0, 1.0, 0.016, 1.0, 0.0) < 0.05);
    }

    #[test]
    fn envelope_presets_shape_the_cycle() {
        let at = |pre: EnvPreset, x: f64| {
            let a = Animation::Envelope {
                motion: Default::default(),
                points: pre.points(),
                beats: 4.0,
            };
            evaluate(&p((0.0, 1.0)), &a, &ctx(x * 4.0, 0.0, &[], &[]))
        };
        assert_eq!(at(EnvPreset::RampUp, 0.25), 0.25);
        assert_eq!(at(EnvPreset::RampDown, 0.25), 0.75);
        assert!((at(EnvPreset::Sine, 0.25) - 0.5).abs() < 1e-9);
        assert!((at(EnvPreset::Sine, 0.5) - 1.0).abs() < 1e-9);
        assert!((at(EnvPreset::Saw, 0.5) - 0.5).abs() < 0.01);
        assert!(at(EnvPreset::Saw, 0.9995) > 0.4);
        assert_eq!(at(EnvPreset::Square, 0.25), 1.0);
        assert_eq!(at(EnvPreset::Square, 0.75), 0.0);
        // Ease in starts slow and ease out starts fast; in/out is symmetric.
        assert!(at(EnvPreset::EaseIn, 0.25) < 0.1);
        assert!(at(EnvPreset::EaseOut, 0.25) > 0.5);
        assert!((at(EnvPreset::EaseInOut, 0.5) - 0.5).abs() < 1e-9);
        assert!(
            (at(EnvPreset::EaseInOut, 0.25) + at(EnvPreset::EaseInOut, 0.75) - 1.0).abs() < 1e-9
        );
    }

    #[test]
    fn envelope_presets_are_well_formed_and_distinct() {
        for pre in EnvPreset::ALL {
            let pts = pre.points();
            assert_eq!(pts.first().unwrap().0, 0.0, "{}", pre.label());
            assert_eq!(pts.last().unwrap().0, 1.0, "{}", pre.label());
            assert!(pts.windows(2).all(|w| w[0].0 < w[1].0), "{}", pre.label());
            assert!(
                pts.iter()
                    .all(|(x, y)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
            );
            // Every preset moves: a flat envelope would be a bug.
            assert!(pts.iter().any(|q| q.1 != pts[0].1), "{}", pre.label());
        }
        let all: Vec<_> = EnvPreset::ALL.iter().map(|p| p.points()).collect();
        for i in 0..all.len() {
            for j in i + 1..all.len() {
                assert_ne!(all[i], all[j]);
            }
        }
    }

    #[test]
    fn dashboard_scope_picks_the_dial_list() {
        let (comp, layer, clip) = ([0.1], [0.2], [0.3]);
        let base = ctx(0.0, 0.0, &[], &comp);
        let c = base.scoped(&layer, &clip);
        let read = |scope| {
            let a = Animation::Dashboard { dial: 0, scope };
            evaluate(&p((0.0, 1.0)), &a, &c)
        };
        assert_eq!(read(DialScope::Comp), 0.1);
        assert_eq!(read(DialScope::Layer), 0.2);
        assert_eq!(read(DialScope::Clip), 0.3);
        // A scope with no dials (param outside any layer/clip) reads 0, not the comp's.
        let none = base.scoped(&[], &[]);
        let a = Animation::Dashboard {
            dial: 0,
            scope: DialScope::Layer,
        };
        assert_eq!(evaluate(&p((0.0, 1.0)), &a, &none), 0.0);
        // An unscoped context (composition-level param) reads the base value.
        let mut q = p((0.0, 1.0));
        q.value = Value::Number(0.7);
        assert_eq!(evaluate(&q, &a, &base), 0.7);
    }

    #[test]
    fn dial_values_pads_to_eight() {
        let mut d = Param::number("Dial 1", 0.5, Some((0.0, 1.0)));
        d.set_number(0.5);
        let v = dial_values(&[d]);
        assert_eq!(v.len(), DIALS);
        assert_eq!(v[0], 0.5);
        assert!(v[1..].iter().all(|x| *x == 0.0));
    }

    #[test]
    fn motion_modes_and_direction() {
        let at = |mode, direction, b: f64| {
            let a = Animation::Beats {
                beats: 4.0,
                curve: Curve::Saw,
                motion: Motion { mode, direction },
            };
            evaluate(&p((0.0, 1.0)), &a, &ctx(b, 1.0, &[], &[]))
        };
        use AnimDirection::{Backward, Forward};
        use LoopMode::{Loop, PingPong};
        // Loop wraps; backward runs the ramp down.
        assert!((at(Loop, Forward, 5.0) - 0.25).abs() < 1e-9);
        assert!((at(Loop, Backward, 1.0) - 0.75).abs() < 1e-9);
        // PingPong goes up then down.
        assert!((at(PingPong, Forward, 1.0) - 0.25).abs() < 1e-9);
        assert!((at(PingPong, Forward, 6.0) - 0.5).abs() < 1e-9);
        assert!((at(PingPong, Backward, 1.0) - 0.75).abs() < 1e-9);
    }
}
