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
    // Appended after the original 19: shader ids are list positions, so order is stable.
    (BlendMode::Divide, "Divide"),
    (BlendMode::Hue, "Hue"),
    (BlendMode::Saturation, "Saturation"),
    (BlendMode::Color, "Color"),
    (BlendMode::Luminosity, "Luminosity"),
    (BlendMode::Displace, "Displace"),
    (BlendMode::LoRez, "LoRez"),
    (BlendMode::Add50, "50 Add"),
    (BlendMode::Burn50, "50 Burn"),
    (BlendMode::Difference50, "50 Difference"),
    (BlendMode::Distance50, "50 Distance"),
    (BlendMode::Dodge50, "50 Dodge"),
    (BlendMode::Lighten50, "50 Lighten"),
    (BlendMode::Mask50, "50 Mask"),
    (BlendMode::Multiply50, "50 Multiply"),
    (BlendMode::Overlay50, "50 Overlay"),
    (BlendMode::Subtract50, "50 Subtract"),
    (BlendMode::DifferenceInverse, "Inverse Difference"),
    (BlendMode::LumaKeyInverse, "Inverse Luma Key"),
    (BlendMode::Jitter, "Jitter"),
    (BlendMode::Bumper, "Luma Reveal"),
];

/// Reach in px of the Jitter blend (offset in -JITTER_PX..JITTER_PX on each axis).
pub const JITTER_PX: i32 = 4;

/// Per-pixel source offset of the Jitter blend (the shader mirrors this hash).
pub fn jitter_offset(x: u32, y: u32) -> [i32; 2] {
    let mut h = x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    [
        (h & 7) as i32 - JITTER_PX,
        ((h >> 8) & 7) as i32 - JITTER_PX,
    ]
}

/// Share of the source shown by the Bumper blend at `progress` for a pixel of luma `l`:
/// pixels appear from dark to bright as the progress goes 0 -> 1 (soft edge of 0.1).
pub fn bumper_gate(l: f32, progress: f32) -> f32 {
    ((progress * 1.1 - l) / 0.1).clamp(0.0, 1.0)
}

// Not offered as composition blends (they render as Normal):
// - Cube: a two-picture transition (needs outgoing and incoming faces); it lives in
//   `transition::Kind::Cube`.
// - Depth, JitterBug, Meta Mix, Multi Task, Noise...: need depth maps, time or several
//   inputs that a single source-over-base pass does not have.

/// Reach in px of the Displace blend: red and green of the source push the base sample
/// (0.5 = no shift).
pub const DISPLACE_PX: f32 = 16.0;
pub fn displace_offset(s: [f32; 4]) -> [i32; 2] {
    [
        ((s[0] - 0.5) * 2.0 * DISPLACE_PX).round() as i32,
        ((s[1] - 0.5) * 2.0 * DISPLACE_PX).round() as i32,
    ]
}

/// Block size in source texels of the LoRez blend (source sampled at each block's first texel).
pub const LOREZ_BLOCK: usize = 8;

/// Mode id used by the shader; modes without a composition formula map to 0 (Normal).
/// Blend a clip is drawn with: its own override when set, else the layer's.
pub fn effective_blend(layer: &BlendMode, clip_override: Option<&BlendMode>) -> BlendMode {
    clip_override.unwrap_or(layer).clone()
}

/// Blend modes an effect can mix its output with over its input (index = shader id; 0 =
/// Normal, a plain crossfade).
pub const FX_MIX_MODES: [BlendMode; 13] = [
    BlendMode::Normal,
    BlendMode::Add,
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::Difference,
    BlendMode::Darken,
    BlendMode::Lighten,
    BlendMode::Subtract,
    BlendMode::Exclusion,
    BlendMode::HardLight,
    BlendMode::Dodge,
    BlendMode::Burn,
];

/// Shader id of an effect's mix blend; modes outside `FX_MIX_MODES` act as Normal.
pub fn fx_mix_id(mode: &BlendMode) -> u32 {
    FX_MIX_MODES.iter().position(|m| m == mode).unwrap_or(0) as u32
}

/// CPU twin of the effect mix shader for one pixel: `dry` is the effect input, `wet` its
/// output, `opacity` the dry/wet amount.
pub fn fx_mix_pixel(mode: &BlendMode, dry: [f32; 4], wet: [f32; 4], opacity: f32) -> [f32; 4] {
    let id = fx_mix_id(mode);
    let t = opacity.clamp(0.0, 1.0);
    let blended = if id == 0 {
        [wet[0], wet[1], wet[2]]
    } else {
        // Same formulas as `color_mode`, which the layer blend uses for these modes.
        color_mode(
            shader_id(mode),
            [dry[0], dry[1], dry[2]],
            [wet[0], wet[1], wet[2]],
        )
    };
    let mut o = [0.0; 4];
    for i in 0..3 {
        o[i] = dry[i] + (blended[i] - dry[i]) * t;
    }
    o[3] = dry[3] + (wet[3] - dry[3]) * t;
    o
}

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

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut o = c;
    if n < 0.0 {
        for v in &mut o {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut o {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    o
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if x <= n {
        return [0.0; 3];
    }
    c.map(|v| (v - n) * s / (x - n))
}

/// Full-colour result of a mode before the alpha mix (ids 0..=14, 19..=23 and 26..=35).
fn color_mode(id: u32, b: [f32; 3], s: [f32; 3]) -> [f32; 3] {
    let per = |id: u32| [0, 1, 2].map(|i| channel(id, b[i], s[i]));
    match id {
        19 => [0, 1, 2].map(|i| {
            if s[i] <= 0.0 {
                1.0
            } else {
                (b[i] / s[i]).min(1.0)
            }
        }),
        20 => set_lum(set_sat(s, sat(b)), lum(b)),
        21 => set_lum(set_sat(b, sat(s)), lum(b)),
        22 => set_lum(s, lum(b)),
        23 => set_lum(b, lum(s)),
        26..=35 => {
            // 50%: the mode's result mixed with the layer below at half strength.
            let full = match id {
                29 => {
                    // Distance: grey level from the colour distance between the two layers.
                    let d = ((b[0] - s[0]).powi(2) + (b[1] - s[1]).powi(2) + (b[2] - s[2]).powi(2))
                        .sqrt()
                        / 3f32.sqrt();
                    [d; 3]
                }
                32 => {
                    // Mask: the base kept where the source is bright.
                    let m = luma(s);
                    b.map(|v| v * m)
                }
                _ => per(match id {
                    26 => 2,
                    27 => 14,
                    28 => 11,
                    30 => 13,
                    31 => 9,
                    33 => 4,
                    34 => 6,
                    _ => 3,
                }),
            };
            [0, 1, 2].map(|i| b[i] + (full[i] - b[i]) * 0.5)
        }
        36 => [0, 1, 2].map(|i| 1.0 - (b[i] - s[i]).abs()),
        _ => per(id),
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
        // Inverse Luma Key: bright pixels of the source become transparent.
        37 => {
            a *= 1.0 - l;
            rgb = [s[0], s[1], s[2]];
        }
        // Jitter: the caller passes the source already read at `jitter_offset`.
        38 => rgb = [s[0], s[1], s[2]],
        // Bumper: the opacity is the wipe progress, not a plain alpha scale.
        39 => {
            a = (s[3] * bumper_gate(l, opacity.clamp(0.0, 1.0))).clamp(0.0, 1.0);
            rgb = [s[0], s[1], s[2]];
        }
        // Displace and LoRez need neighbouring pixels (see `displace_offset` and
        // `LOREZ_BLOCK`): per pixel the caller passes the displaced base / snapped source.
        24 => rgb = [b[0], b[1], b[2]],
        25 => rgb = [s[0], s[1], s[2]],
        _ => rgb = color_mode(id, [b[0], b[1], b[2]], [s[0], s[1], s[2]]),
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

    #[test]
    fn new_blends_cpu_reference() {
        let b = [0.8, 0.5, 0.2, 1.0];
        let s = [0.4, 0.5, 0.9, 1.0];
        let d = blend_pixel(&BlendMode::DifferenceInverse, b, s, 1.0);
        assert!((d[0] - 0.6).abs() < 1e-6 && (d[1] - 1.0).abs() < 1e-6);
        // Inverse Luma Key: a white source vanishes, a black one stays.
        let w = blend_pixel(&BlendMode::LumaKeyInverse, b, [1.0, 1.0, 1.0, 1.0], 1.0);
        assert_eq!(w[..3], b[..3]);
        let k = blend_pixel(&BlendMode::LumaKeyInverse, b, [0.0, 0.0, 0.0, 1.0], 1.0);
        assert_eq!(k[..3], [0.0; 3]);
        // Bumper: nothing at progress 0, all at progress 1, dark pixels first in between.
        let grey = [0.5, 0.5, 0.5, 1.0];
        assert_eq!(blend_pixel(&BlendMode::Bumper, b, grey, 0.0)[..3], b[..3]);
        assert_eq!(
            blend_pixel(&BlendMode::Bumper, b, grey, 1.0)[..3],
            grey[..3]
        );
        let w = [1.0, 1.0, 1.0, 1.0];
        assert_eq!(blend_pixel(&BlendMode::Bumper, b, w, 1.0)[..3], w[..3]);
        assert!(bumper_gate(0.2, 0.4) > bumper_gate(0.8, 0.4));
    }

    #[test]
    fn jitter_offsets_are_bounded_and_vary() {
        let mut seen = std::collections::HashSet::new();
        for y in 0..16 {
            for x in 0..16 {
                let o = jitter_offset(x, y);
                assert!(o.iter().all(|v| (-JITTER_PX..JITTER_PX).contains(v)));
                seen.insert(o);
            }
        }
        assert!(seen.len() > 20);
    }

    const B: [f32; 4] = [0.8, 0.5, 0.2, 1.0];
    const S: [f32; 4] = [0.4, 0.5, 0.9, 1.0];

    #[test]
    fn fx_mix_normal_is_a_crossfade_and_multiply_differs() {
        let (d, w) = ([0.8, 0.5, 0.2, 1.0], [0.4, 0.5, 0.9, 1.0]);
        assert_eq!(fx_mix_pixel(&BlendMode::Normal, d, w, 1.0), w);
        assert_eq!(fx_mix_pixel(&BlendMode::Normal, d, w, 0.0), d);
        let half = fx_mix_pixel(&BlendMode::Normal, d, w, 0.5);
        assert!((half[0] - 0.6).abs() < 1e-6);
        let m = fx_mix_pixel(&BlendMode::Multiply, d, w, 1.0);
        assert!((m[0] - 0.32).abs() < 1e-5 && (m[2] - 0.18).abs() < 1e-5);
        // Every listed mode is distinct from Normal's id.
        assert!(FX_MIX_MODES[1..].iter().all(|m| fx_mix_id(m) != 0));
    }

    #[test]
    fn clip_override_wins_over_layer_blend() {
        assert_eq!(
            effective_blend(&BlendMode::Multiply, Some(&BlendMode::Add)),
            BlendMode::Add
        );
        // Negative control: no override keeps the layer's blend.
        assert_eq!(
            effective_blend(&BlendMode::Multiply, None),
            BlendMode::Multiply
        );
    }

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
        assert_eq!(COMPOSITION_MODES.len(), 40);
    }

    /// Old projects keep their blends: the first 19 keep their shader ids and the serialized
    /// names of every old variant are unchanged.
    #[test]
    fn original_blends_keep_ids_and_names() {
        let old = [
            "Normal",
            "Alpha",
            "Add",
            "Subtract",
            "Multiply",
            "Screen",
            "Overlay",
            "SoftLight",
            "HardLight",
            "Lighten",
            "Darken",
            "Difference",
            "Exclusion",
            "Dodge",
            "Burn",
            "LumaKey",
            "LumaIsAlpha",
            "Rgb",
            "BlackWhite",
        ];
        for (i, name) in old.iter().enumerate() {
            let m: BlendMode = serde_json::from_str(&format!("\"{name}\"")).unwrap();
            assert_eq!(COMPOSITION_MODES[i].0, m, "{name}");
            assert_eq!(shader_id(&m), i as u32);
            assert_eq!(serde_json::to_string(&m).unwrap(), format!("\"{name}\""));
        }
        // Control: a name that does not exist maps to Unknown, not to a shifted mode.
        let u: BlendMode = serde_json::from_str("\"Nope\"").unwrap();
        assert_eq!(u, BlendMode::Unknown);
    }
}
