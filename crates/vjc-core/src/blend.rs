//! Per-pixel reference of the compositor's blend modes (straight-alpha RGBA, f32 0..1).
//! The WGSL shader in vjc-app mirrors these formulas (W3C compositing, separable blend
//! modes); tests compare both. Transition modes are not handled here: they render as Normal.

use vjc_model::BlendMode;

/// Composition modes offered in the UI, with their display names (shader id = index).
pub const COMPOSITION_MODES: &[(BlendMode, &str)] = &[
    (BlendMode::Normal, "Normal"),
    (BlendMode::Alpha, "Alpha"),
    (BlendMode::Add, "Add"),
    (BlendMode::Subtract, "Subtract"),
    (BlendMode::Multiply, "Multiply"),
    (BlendMode::Screen, "Screen"),
    (BlendMode::Overlay, "Overlay"),
    (BlendMode::SoftLight, "Soft Light"),
    (BlendMode::HardLight, "Hard Light"),
    (BlendMode::Lighten, "Lighten"),
    (BlendMode::Darken, "Darken"),
    (BlendMode::Difference, "Difference"),
    (BlendMode::Exclusion, "Exclusion"),
    (BlendMode::Dodge, "Color Dodge"),
    (BlendMode::Burn, "Color Burn"),
    (BlendMode::LumaKey, "Luma Key"),
    (BlendMode::LumaIsAlpha, "Luma Is Alpha"),
    (BlendMode::Rgb, "RGB"),
    (BlendMode::BlackWhite, "Black & White"),
];

/// Mode id used by the shader; modes without a composition formula map to 0 (Normal).
pub fn shader_id(mode: &BlendMode) -> u32 {
    COMPOSITION_MODES
        .iter()
        .position(|(m, _)| m == mode)
        .unwrap_or(0) as u32
}

pub fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb * 2.0 * cs
    } else {
        let s = 2.0 * cs - 1.0;
        cb + s - cb * s
    }
}

fn channel(id: u32, cb: f32, cs: f32) -> f32 {
    match id {
        2 => (cb + cs).min(1.0),
        3 => (cb - cs).max(0.0),
        4 => cb * cs,
        5 => cb + cs - cb * cs,
        6 => hard_light(cs, cb),
        7 => soft_light(cb, cs),
        8 => hard_light(cb, cs),
        9 => cb.max(cs),
        10 => cb.min(cs),
        11 => (cb - cs).abs(),
        12 => cb + cs - 2.0 * cb * cs,
        13 => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        14 => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        _ => cs,
    }
}

/// Blends source `s` over base `b`. `opacity` scales the source alpha, so the result is
/// `mix(base, blended, s.a * opacity)`. Output alpha is the union of both alphas.
pub fn blend_pixel(mode: &BlendMode, b: [f32; 4], s: [f32; 4], opacity: f32) -> [f32; 4] {
    let id = shader_id(mode);
    let mut a = (s[3] * opacity).clamp(0.0, 1.0);
    let l = luma([s[0], s[1], s[2]]);
    let bl = luma([b[0], b[1], b[2]]);
    let mut rgb = [0.0; 3];
    match id {
        // Alpha: the source alpha masks the base (rgb of the source is ignored).
        1 => {
            let k = 1.0 + (s[3] - 1.0) * opacity.clamp(0.0, 1.0);
            return [b[0] * k, b[1] * k, b[2] * k, b[3]];
        }
        // Luma Key: dark pixels of the source become transparent.
        15 => {
            a *= l;
            rgb = [s[0], s[1], s[2]];
        }
        // Luma Is Alpha: source luma is the alpha and the color is white-weighted by it.
        16 => {
            a = (l * opacity).clamp(0.0, 1.0);
            rgb = [1.0; 3];
        }
        // RGB: base luminance tinted per channel by the source.
        17 => {
            for i in 0..3 {
                rgb[i] = bl * s[i];
            }
        }
        // Black & White: grayscale source, normal blend.
        18 => rgb = [l; 3],
        _ => {
            for i in 0..3 {
                rgb[i] = channel(id, b[i], s[i]);
            }
        }
    }
    [
        b[0] + (rgb[0] - b[0]) * a,
        b[1] + (rgb[1] - b[1]) * a,
        b[2] + (rgb[2] - b[2]) * a,
        a + b[3] * (1.0 - a),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: [f32; 4] = [0.8, 0.5, 0.2, 1.0];
    const S: [f32; 4] = [0.4, 0.5, 0.9, 1.0];

    fn run(m: BlendMode, b: [f32; 4], s: [f32; 4], op: f32) -> [f32; 4] {
        blend_pixel(&m, b, s, op)
    }

    fn eq(got: [f32; 4], want: [f32; 3]) {
        for i in 0..3 {
            assert!((got[i] - want[i]).abs() < 1e-5, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn simple_modes() {
        eq(run(BlendMode::Normal, B, S, 1.0), [0.4, 0.5, 0.9]);
        eq(run(BlendMode::Add, B, S, 1.0), [1.0, 1.0, 1.0]);
        eq(run(BlendMode::Subtract, B, S, 1.0), [0.4, 0.0, 0.0]);
        eq(run(BlendMode::Multiply, B, S, 1.0), [0.32, 0.25, 0.18]);
        eq(run(BlendMode::Screen, B, S, 1.0), [0.88, 0.75, 0.92]);
        eq(run(BlendMode::Lighten, B, S, 1.0), [0.8, 0.5, 0.9]);
        eq(run(BlendMode::Darken, B, S, 1.0), [0.4, 0.5, 0.2]);
        eq(run(BlendMode::Difference, B, S, 1.0), [0.4, 0.0, 0.7]);
        eq(run(BlendMode::Exclusion, B, S, 1.0), [0.56, 0.5, 0.74]);
    }

    #[test]
    fn contrast_modes() {
        // Overlay = HardLight with layers swapped; swapped roles.
        eq(run(BlendMode::Overlay, B, S, 1.0), [0.76, 0.5, 0.36]);
        // HardLight: cs=0.4 multiplies 2cs; cs=0.5 multiplies 1.0; cs=0.9 screens 0.8.
        eq(
            run(BlendMode::HardLight, B, S, 1.0),
            [0.64, 0.5, 0.2 + 0.8 - 0.16],
        );
        // Dodge: 0.8/0.6 clamps to 1; 0.5/0.5 = 1; 0.2/0.1 clamps to 1.
        eq(run(BlendMode::Dodge, B, S, 1.0), [1.0, 1.0, 1.0]);
        // Burn: 1-(0.2/0.4)=0.5; 1-(0.5/0.5)=0; 1-(0.8/0.9)
        eq(run(BlendMode::Burn, B, S, 1.0), [0.5, 0.0, 1.0 - 0.8 / 0.9]);
        // Soft light: cs=0.5 leaves the base untouched; cs=0.4 darkens a bit.
        let r = run(BlendMode::SoftLight, B, S, 1.0);
        assert!((r[1] - 0.5).abs() < 1e-5);
        assert!((r[0] - (0.8 - 0.2 * 0.8 * 0.2)).abs() < 1e-5);
        assert!(r[2] > 0.2);
    }

    #[test]
    fn luma_and_color_modes() {
        let white = [1.0; 4];
        let black = [0.0, 0.0, 0.0, 1.0];
        // Luma key: black source is transparent, white source is opaque.
        eq(run(BlendMode::LumaKey, B, black, 1.0), [0.8, 0.5, 0.2]);
        eq(run(BlendMode::LumaKey, B, white, 1.0), [1.0, 1.0, 1.0]);
        // Luma is alpha: luma 0.5 gray over black base gives white at half alpha.
        let r = run(
            BlendMode::LumaIsAlpha,
            [0.0, 0.0, 0.0, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            1.0,
        );
        eq(r, [0.5, 0.5, 0.5]);
        eq(
            run(BlendMode::BlackWhite, B, [1.0, 0.0, 0.0, 1.0], 1.0),
            [0.299; 3],
        );
        let bl = luma([0.8, 0.5, 0.2]);
        eq(
            run(BlendMode::Rgb, B, [1.0, 0.0, 0.5, 1.0], 1.0),
            [bl, 0.0, bl * 0.5],
        );
        // Alpha masks the base.
        eq(
            run(BlendMode::Alpha, B, [0.0, 0.0, 0.0, 0.5], 1.0),
            [0.4, 0.25, 0.1],
        );
    }

    #[test]
    fn opacity_and_alpha_mix() {
        eq(run(BlendMode::Multiply, B, S, 0.5), [0.56, 0.375, 0.19]);
        eq(run(BlendMode::Normal, B, S, 0.0), [0.8, 0.5, 0.2]);
        eq(
            run(BlendMode::Normal, B, [0.0, 0.0, 0.0, 0.5], 1.0),
            [0.4, 0.25, 0.1],
        );
    }

    #[test]
    fn transition_modes_fall_back_to_normal() {
        assert_eq!(shader_id(&BlendMode::PushUp), 0);
        assert_eq!(
            run(BlendMode::Cut, B, S, 1.0),
            run(BlendMode::Normal, B, S, 1.0)
        );
        assert_eq!(COMPOSITION_MODES.len(), 19);
    }
}
