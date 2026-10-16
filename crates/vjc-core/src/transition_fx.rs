//! CPU twin of the newer transition shaders (the `tkind` 17..=41 branches in the
//! compositor). `coverage` is what one layer contributes at a pixel: the alpha the shader
//! gives the picture there. GPU tests render flat colours and compare against it, so keep
//! both sides in sync.

use crate::transition::Kind;
use std::f32::consts::PI;

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Soft sweep edge: 0 before the front (d > p), 1 behind it.
fn edge(d: f32, p: f32) -> f32 {
    1.0 - smoothstep(p * 1.05 - 0.05, p * 1.05, d)
}

/// Integer hash in 0..1 (same ops as `thash` in the shader; u32 wrapping on both sides).
pub fn hash2(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(73_856_093) ^ b.wrapping_mul(19_349_663);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65536.0
}

fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// Rotates `(q - 0.5) / s` by `-ang` around the frame centre (spin kinds).
fn spin(q: (f32, f32), s: f32, ang: f32) -> (f32, f32) {
    let (c, sn) = (ang.cos(), ang.sin());
    let r = ((q.0 - 0.5) / s, (q.1 - 0.5) / s);
    (c * r.0 + sn * r.1 + 0.5, -sn * r.0 + c * r.1 + 0.5)
}

/// Alpha of the outgoing (`out`) or incoming picture at frame position `uv` (0..1, y
/// down) for a flat full-frame picture. Meant for kinds with shader id >= 17; `ToWhite`'s
/// colour dip is not alpha, so only its on/off halves show here.
pub fn coverage(kind: Kind, p: f32, out: bool, uv: (f32, f32)) -> f32 {
    let (x, y) = uv;
    let mut q = uv;
    let mut mix = 1.0;
    let id = kind.shader_id();
    match kind {
        Kind::ToWhite => {
            if out == (p >= 0.5) {
                return 0.0;
            }
        }
        _ if (17..=30).contains(&id) => {
            if out {
                return 1.0;
            }
            let d = match kind {
                Kind::WipeUpLeft => ((1.0 - x) + (1.0 - y)) * 0.5,
                Kind::WipeUpRight => (x + (1.0 - y)) * 0.5,
                Kind::WipeDownLeft => ((1.0 - x) + y) * 0.5,
                Kind::WipeDownRight => (x + y) * 0.5,
                Kind::IrisOut => {
                    1.0 - ((x - 0.5) * 2.0).hypot((y - 0.5) * 2.0) / std::f32::consts::SQRT_2
                }
                Kind::Clock => fract((x - 0.5).atan2(0.5 - y) / (2.0 * PI)),
                Kind::ClockReverse => 1.0 - fract((x - 0.5).atan2(0.5 - y) / (2.0 * PI)),
                Kind::BlindsH => fract(y * 8.0),
                Kind::BlindsV => fract(x * 8.0),
                Kind::Checker => {
                    let lx = (fract(x * 8.0) - 0.5).abs() * 2.0;
                    let ly = (fract(y * 8.0) - 0.5).abs() * 2.0;
                    let cell = ((x * 8.0).floor() as u32) + ((y * 8.0).floor() as u32);
                    0.5 * lx.max(ly) + 0.5 * (cell & 1) as f32
                }
                Kind::BarnDoorH => (x - 0.5).abs() * 2.0,
                Kind::BarnDoorV => (y - 0.5).abs() * 2.0,
                Kind::Diamond => (x - 0.5).abs() + (y - 0.5).abs(),
                _ => hash2((x * 32.0).floor() as u32, (y * 32.0).floor() as u32),
            };
            mix = edge(d, p);
        }
        Kind::SlideLeft | Kind::SlideRight | Kind::SlideUp | Kind::SlideDown => {
            if out {
                return 1.0;
            }
            match kind {
                Kind::SlideLeft => q.0 += p - 1.0,
                Kind::SlideRight => q.0 += 1.0 - p,
                Kind::SlideUp => q.1 += p - 1.0,
                _ => q.1 += 1.0 - p,
            }
        }
        Kind::SpinIn => {
            if !out {
                q = spin(q, p.max(0.001), (1.0 - p) * 2.0 * PI);
            }
        }
        Kind::SpinOut => {
            if out {
                q = spin(q, (1.0 - p).max(0.001), p * 2.0 * PI);
                mix = 1.0 - p;
            } else {
                mix = p;
            }
        }
        Kind::FlipH | Kind::FlipV => {
            if out == (p >= 0.5) {
                return 0.0;
            }
            let k = if out { 1.0 - 2.0 * p } else { 2.0 * p - 1.0 }.max(0.001);
            if kind == Kind::FlipH {
                q.0 = (q.0 - 0.5) / k + 0.5;
            } else {
                q.1 = (q.1 - 0.5) / k + 0.5;
            }
        }
        Kind::Glitch => {
            let band = (y * 16.0).floor() as u32;
            let step = (p * 12.0).floor() as u32;
            let off = (hash2(band, step + 100) - 0.5) * 0.3 * (p * PI).sin();
            q.0 += off;
            if out == (hash2(band, 7) < p) {
                return 0.0;
            }
        }
        Kind::Ripple => {
            let (cx, cy) = (x - 0.5, y - 0.5);
            let r = cx.hypot(cy);
            let amp = 0.03 * (p * PI).sin() * (r * 30.0 - p * 20.0).sin();
            q.0 += cx / r.max(1e-5) * amp;
            q.1 += cy / r.max(1e-5) * amp;
            mix = if out { 1.0 - p } else { p };
        }
        _ => {}
    }
    if !(0.0..=1.0).contains(&q.0) || !(0.0..=1.0).contains(&q.1) {
        return 0.0;
    }
    mix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> impl Iterator<Item = (f32, f32)> {
        (0..24)
            .flat_map(|j| (0..24).map(move |i| ((i as f32 + 0.5) / 24.0, (j as f32 + 0.5) / 24.0)))
    }

    fn new_kinds() -> impl Iterator<Item = Kind> {
        Kind::ALL
            .into_iter()
            .filter(|k| k.shader_id() >= 17 && *k != Kind::Random && *k != Kind::ToWhite)
    }

    #[test]
    fn every_new_kind_starts_old_and_ends_new() {
        for k in new_kinds() {
            for uv in grid() {
                let (o0, n0) = (coverage(k, 0.0, true, uv), coverage(k, 0.0, false, uv));
                // The new picture is drawn over the old one, which may stay underneath.
                let n1 = coverage(k, 1.0, false, uv);
                assert!(o0 > 0.99 && n0 < 0.01, "{k:?} p=0 {uv:?}: {o0} {n0}");
                assert!(n1 > 0.99, "{k:?} p=1 {uv:?}: {n1}");
            }
        }
    }

    #[test]
    fn midway_is_neither_old_nor_new() {
        let all = (24 * 24) as f32;
        for k in new_kinds() {
            let n: f32 = grid().map(|uv| coverage(k, 0.5, false, uv)).sum();
            assert!(n < all * 0.999, "{k:?} already fully new ({n})");
        }
    }

    #[test]
    fn reveal_grows_monotonically() {
        // A reveal kind never un-reveals a pixel.
        for k in [
            Kind::Clock,
            Kind::Checker,
            Kind::Diamond,
            Kind::BlindsH,
            Kind::WipeDownRight,
        ] {
            for uv in grid() {
                let mut prev = 0.0;
                for s in 0..=10 {
                    let c = coverage(k, s as f32 / 10.0, false, uv);
                    assert!(c + 1e-5 >= prev, "{k:?} {uv:?}");
                    prev = c;
                }
            }
        }
    }

    #[test]
    fn diagonal_wipes_start_in_their_corner() {
        // Down-right starts top-left (like WipeRight starts left); up-left starts bottom-right.
        assert!(coverage(Kind::WipeDownRight, 0.3, false, (0.05, 0.05)) > 0.9);
        assert!(coverage(Kind::WipeDownRight, 0.3, false, (0.95, 0.95)) < 0.1);
        assert!(coverage(Kind::WipeUpLeft, 0.3, false, (0.95, 0.95)) > 0.9);
    }
}
