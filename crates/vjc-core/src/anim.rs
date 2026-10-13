//! Parameter animation: a pure evaluator. `Param.value` stays the saved base; the effective
//! value is computed per frame from beat phase, time, FFT bands and dashboard dials, so
//! animating never dirties the project nor enters the history.

use vjc_model::{Animation, Curve, FftBand, Param, Value};

/// Dials per composition.
pub const DIALS: usize = 8;

/// Inputs of one frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnimCtx<'a> {
    /// Master clock phase, in beats.
    pub beat_phase: f64,
    pub time_secs: f64,
    /// FFT levels 0..1: low, low-mid, mid, high, then raw bins. TODO(#24): no audio input
    /// yet (cpal/rustfft are not dependencies), the app passes an empty slice.
    pub fft_bands: &'a [f32],
    /// Dial values 0..1 (missing dials read 0).
    pub dashboard: &'a [f64],
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
        FftBand::Bin(n) => 4 + *n as usize,
    };
    bands.get(i).copied().map_or(0.0, f64::from)
}

fn safe_div(a: f64, b: f64) -> f64 {
    if b.is_finite() && b > 0.0 { a / b } else { 0.0 }
}

/// Normalised 0..=1 output of the animation.
fn unit(anim: &Animation, ctx: &AnimCtx) -> f64 {
    let t = match anim {
        Animation::Timeline {
            duration_secs,
            curve,
        } => shape(curve, safe_div(ctx.time_secs, *duration_secs)),
        Animation::Beats { beats, curve } => shape(curve, safe_div(ctx.beat_phase, *beats)),
        Animation::Audio {
            band: b,
            gain,
            threshold,
        } => {
            let v = band(b, ctx.fft_bands);
            if v < f64::from(*threshold) {
                0.0
            } else {
                v * f64::from(*gain)
            }
        }
        Animation::Envelope { points, beats } => envelope(points, safe_div(ctx.beat_phase, *beats)),
        Animation::Dashboard { dial } => ctx.dashboard.get(*dial as usize).copied().unwrap_or(0.0),
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
    (lo + unit(anim, ctx) * (hi - lo)).clamp(lo.min(hi), lo.max(hi))
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
            dashboard: dash,
        }
    }
    fn beats(c: Curve) -> Animation {
        Animation::Beats {
            beats: 4.0,
            curve: c,
        }
    }
    fn at(c: Curve, beat: f64) -> f64 {
        evaluate(&p((0.0, 10.0)), &beats(c), &ctx(beat, 0.0, &[], &[]))
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
    fn timeline_uses_seconds() {
        let a = Animation::Timeline {
            duration_secs: 2.0,
            curve: Curve::Saw,
        };
        assert_eq!(evaluate(&p((0.0, 1.0)), &a, &ctx(9.0, 0.5, &[], &[])), 0.25);
    }

    #[test]
    fn envelope_interpolates() {
        let a = Animation::Envelope {
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
        let d = Animation::Dashboard { dial: 1 };
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
        });
        let c = ctx(0.0, 0.0, &[1.0], &[]);
        assert_eq!(effective_number(&pr, &c), 3.0);
        assert_eq!(pr.value, Value::Number(2.0));
        let bad = Animation::Beats {
            beats: 0.0,
            curve: Curve::Saw,
        };
        assert_eq!(evaluate(&pr, &bad, &c), 2.0);
    }
}
