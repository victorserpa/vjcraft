//! GPU compositor: stacks layers bottom to top into an Rgba8Unorm texture.
//! Each layer is one pass that reads the composition so far (a copy) and blends in the
//! shader; transition-only BlendModes render as Normal. Mirrors `vjc_core::blend`.

use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use vjc_core::blend::shader_id;
pub use vjc_core::transform::Affine;
use vjc_model::{BlendMode, FitMode, GroupId, LayerId, SliceSource};

use crate::ml_gpu::MlStore;
pub use crate::ml_gpu::{MlDraw, MlFrame};
use crate::persist::PersistStore;
use crate::pool::Pool;

/// Fraction of the destination frame (x, y) covered by a `src` video for the given mode.
pub fn fit_scale(src: (u32, u32), dst: (u32, u32), mode: FitMode) -> (f32, f32) {
    let (sw, sh, dw, dh) = (
        src.0.max(1) as f32,
        src.1.max(1) as f32,
        dst.0.max(1) as f32,
        dst.1.max(1) as f32,
    );
    let (rw, rh) = (dw / sw, dh / sh);
    let k = match mode {
        FitMode::Stretch => return (1.0, 1.0),
        FitMode::Fit => rw.min(rh),
        FitMode::Fill => rw.max(rh),
        FitMode::Original => 1.0,
    };
    (sw * k / dw, sh * k / dh)
}

/// Extra picture-space scale for a layer with its own width / height: the picture fitted
/// into the `area` rect (centred) instead of the whole frame. `None` when the layer uses
/// the composition's size or the result is the same as the frame fit.
pub fn area_affine(
    extras: &vjc_model::LayerExtras,
    src: (u32, u32),
    frame: (u32, u32),
    fit: FitMode,
) -> Option<vjc_core::transform::Affine> {
    if extras.width == 0 && extras.height == 0 {
        return None;
    }
    let area = (
        if extras.width == 0 {
            frame.0
        } else {
            extras.width
        },
        if extras.height == 0 {
            frame.1
        } else {
            extras.height
        },
    );
    let (fa, ff) = (fit_scale(src, area, fit), fit_scale(src, frame, fit));
    let gx = f64::from(fa.0 * area.0 as f32 / frame.0.max(1) as f32 / ff.0.max(1e-6));
    let gy = f64::from(fa.1 * area.1 as f32 / frame.1.max(1) as f32 / ff.1.max(1e-6));
    if (gx - 1.0).abs() < 1e-6 && (gy - 1.0).abs() < 1e-6 {
        return None;
    }
    Some(vjc_core::transform::Affine {
        m: [1.0 / gx.max(1e-6), 0.0, 0.0, 1.0 / gy.max(1e-6)],
        t: [0.0, 0.0],
        p: [0.0, 0.0, 1.0],
    })
}

/// Centred rect (x, y, w, h) in destination px that `src` occupies under `mode`.
#[cfg(test)]
fn fit_rect(src: (u32, u32), dst: (u32, u32), mode: FitMode) -> (f32, f32, f32, f32) {
    let (sx, sy) = fit_scale(src, dst, mode);
    let (dw, dh) = (dst.0.max(1) as f32, dst.1.max(1) as f32);
    let (w, h) = (sx * dw, sy * dh);
    ((dw - w) / 2.0, (dh - h) / 2.0, w, h)
}

const SHADER: &str = r#"
// scale = fraction of the composition frame covered by the video (1,1 = whole frame).
// mode = index into vjc_core::blend::COMPOSITION_MODES.
// tkind/prog/out: layer transition (see vjc_core::transition::Kind::shader_id); out = 1 for
// the outgoing clip, which is drawn first. tkind 0 = none.
// m/t: inverse transform, output px (centred on the frame, y down) -> picture px:
// c = M * p + t.xy, with t.zw = frame size in px. t.z == 0 means no transform.
// p.w != 0: perspective, c is divided by p.xy . p0 + p.z and the pixel is dropped where
// that is not positive (behind the camera / edge on).
struct U { opacity: f32, mode: u32, scale: vec2<f32>, prog: f32, tkind: u32, out: u32, pad: u32, m: vec4<f32>, t: vec4<f32>, p: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
// Copy of the composition below this layer.
@group(1) @binding(0) var base_tex: texture_2d<f32>;
// Mask picture (a mask layer drawn on black) for the layers it masks. A 1x1 texture means
// "no mask": the source alpha is left alone.
@group(1) @binding(1) var mask_tex: texture_2d<f32>;

struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VO {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: VO;
    o.pos = vec4(p[i], 0.0, 1.0);
    o.uv = vec2((p[i].x + 1.0) * 0.5, 1.0 - (p[i].y + 1.0) * 0.5);
    return o;
}

fn luma(c: vec3<f32>) -> f32 { return dot(c, vec3(0.299, 0.587, 0.114)); }

fn soft_light1(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
    }
    var d: f32;
    if (cb <= 0.25) {
        d = ((16.0 * cb - 12.0) * cb + 4.0) * cb;
    } else {
        d = sqrt(cb);
    }
    return cb + (2.0 * cs - 1.0) * (d - cb);
}

fn hard_light1(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return cb * 2.0 * cs;
    }
    let s = 2.0 * cs - 1.0;
    return cb + s - cb * s;
}

fn dodge1(cb: f32, cs: f32) -> f32 {
    if (cb <= 0.0) { return 0.0; }
    if (cs >= 1.0) { return 1.0; }
    return min(cb / (1.0 - cs), 1.0);
}

fn burn1(cb: f32, cs: f32) -> f32 {
    if (cb >= 1.0) { return 1.0; }
    if (cs <= 0.0) { return 0.0; }
    return 1.0 - min((1.0 - cb) / cs, 1.0);
}

fn channel(id: u32, cb: f32, cs: f32) -> f32 {
    switch id {
        case 2u: { return min(cb + cs, 1.0); }
        case 3u: { return max(cb - cs, 0.0); }
        case 4u: { return cb * cs; }
        case 5u: { return cb + cs - cb * cs; }
        case 6u: { return hard_light1(cs, cb); }
        case 7u: { return soft_light1(cb, cs); }
        case 8u: { return hard_light1(cb, cs); }
        case 9u: { return max(cb, cs); }
        case 10u: { return min(cb, cs); }
        case 11u: { return abs(cb - cs); }
        case 12u: { return cb + cs - 2.0 * cb * cs; }
        case 13u: { return dodge1(cb, cs); }
        case 14u: { return burn1(cb, cs); }
        default: { return cs; }
    }
}

fn lum3(c: vec3<f32>) -> f32 { return dot(c, vec3(0.3, 0.59, 0.11)); }

fn clip_color(c: vec3<f32>) -> vec3<f32> {
    let l = lum3(c);
    let n = min(c.r, min(c.g, c.b));
    let x = max(c.r, max(c.g, c.b));
    var o = c;
    if (n < 0.0) { o = vec3(l) + (o - vec3(l)) * l / (l - n); }
    if (x > 1.0) { o = vec3(l) + (o - vec3(l)) * (1.0 - l) / (x - l); }
    return o;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> { return clip_color(c + vec3(l - lum3(c))); }

fn sat3(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
}

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let n = min(c.r, min(c.g, c.b));
    let x = max(c.r, max(c.g, c.b));
    if (x <= n) { return vec3(0.0); }
    return (c - vec3(n)) * s / (x - n);
}

// Transition helpers (CPU twin: vjc_core::transition_fx).
fn redge(d: f32, p: f32) -> f32 {
    return 1.0 - smoothstep(p * 1.05 - 0.05, p * 1.05, d);
}

fn thash(a: u32, b: u32) -> f32 {
    var h = (a * 73856093u) ^ (b * 19349663u);
    h = h ^ (h >> 13u);
    h = h * 0x5bd1e995u;
    h = h ^ (h >> 15u);
    return f32(h & 0xffffu) / 65536.0;
}

fn tspin(q: vec2<f32>, s: f32, ang: f32) -> vec2<f32> {
    let c = cos(ang);
    let sn = sin(ang);
    let r = (q - vec2(0.5)) / s;
    return vec2(c * r.x + sn * r.y, -sn * r.x + c * r.y) + vec2(0.5);
}

fn per_channel(id: u32, b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return vec3(channel(id, b.r, s.r), channel(id, b.g, s.g), channel(id, b.b, s.b));
}

// Full-colour modes appended after the original 19 (mirrors blend.rs `color_mode`).
fn color_mode(id: u32, b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    if (id == 36u) { return vec3(1.0) - abs(b - s); }
    if (id == 19u) {
        return select(min(b / s, vec3(1.0)), vec3(1.0), s <= vec3(0.0));
    }
    if (id == 20u) { return set_lum(set_sat(s, sat3(b)), lum3(b)); }
    if (id == 21u) { return set_lum(set_sat(b, sat3(s)), lum3(b)); }
    if (id == 22u) { return set_lum(s, lum3(b)); }
    if (id == 23u) { return set_lum(b, lum3(s)); }
    // 26..35: the mode mixed 50% with the layer below.
    var full: vec3<f32>;
    switch id {
        case 26u: { full = per_channel(2u, b, s); }
        case 27u: { full = per_channel(14u, b, s); }
        case 28u: { full = per_channel(11u, b, s); }
        case 29u: { full = vec3(length(b - s) / 1.7320508); }
        case 30u: { full = per_channel(13u, b, s); }
        case 31u: { full = per_channel(9u, b, s); }
        case 32u: { full = b * luma(s); }
        case 33u: { full = per_channel(4u, b, s); }
        case 34u: { full = per_channel(6u, b, s); }
        default: { full = per_channel(3u, b, s); }
    }
    return b + (full - b) * 0.5;
}

// Output is the final composited pixel (no hardware blending): mix(base, blended, alpha).
@fragment
fn fs(in: VO) -> @location(0) vec4<f32> {
    // Map frame uv to video uv; outside the video is letterbox (discard keeps the base).
    var q = (in.uv - vec2(0.5)) / u.scale + vec2(0.5);
    if (u.t.z > 0.0) {
        let p0 = (in.uv - vec2(0.5)) * u.t.zw;
        let c = vec2(u.m.x * p0.x + u.m.y * p0.y, u.m.z * p0.x + u.m.w * p0.y) + u.t.xy;
        var cc = c;
        if (u.p.w != 0.0) {
            let den = u.p.x * p0.x + u.p.y * p0.y + u.p.z;
            if (den <= 1e-6) { discard; }
            cc = c / den;
        }
        q = cc / (u.t.zw * u.scale) + vec2(0.5);
    }
    var tmix = 1.0;
    var tblack = 0.0;
    var tcol = vec3(0.0);
    let p = u.prog;
    if (u.tkind == 1u) {
        if (u.out == 0u) { tmix = p; }
    } else if (u.tkind == 2u || u.tkind == 41u) {
        if (u.tkind == 41u) { tcol = vec3(1.0); }
        if (u.out == 1u) {
            if (p >= 0.5) { discard; }
            tblack = min(2.0 * p, 1.0);
        } else {
            if (p < 0.5) { discard; }
            tblack = 1.0 - (2.0 * p - 1.0);
        }
    } else if (u.tkind == 3u || u.tkind == 4u) {
        let dir = select(-1.0, 1.0, u.tkind == 3u);
        let shift = select(p - 1.0, p, u.out == 1u);
        q.x = q.x + dir * shift;
    } else if (u.tkind == 5u) {
        if (u.out == 1u) {
            q = (q - vec2(0.5)) / (1.0 + p) + vec2(0.5);
            tmix = 1.0 - p;
        } else {
            tmix = p;
        }
    } else if (u.tkind == 6u) {
        if (u.out == 0u) {
            let e = length((in.uv - vec2(0.5)) * 2.0) / 1.41421356;
            tmix = 1.0 - smoothstep(p * 1.1 - 0.1, p * 1.1, e);
        }
    } else if (u.tkind >= 7u && u.tkind <= 10u) {
        // Wipes: a soft edge sweeps the frame; d = 0 at the side it starts from.
        // 7 left (edge moves toward the left), 8 right, 9 up, 10 down.
        if (u.out == 0u) {
            var d = in.uv.x;
            if (u.tkind == 7u) { d = 1.0 - in.uv.x; }
            if (u.tkind == 9u) { d = 1.0 - in.uv.y; }
            if (u.tkind == 10u) { d = in.uv.y; }
            tmix = 1.0 - smoothstep(p * 1.05 - 0.05, p * 1.05, d);
        }
    } else if (u.tkind == 11u || u.tkind == 12u) {
        let dir = select(-1.0, 1.0, u.tkind == 11u);
        let shift = select(p - 1.0, p, u.out == 1u);
        q.y = q.y + dir * shift;
    } else if (u.tkind == 13u) {
        // Zoom out: the old picture shrinks away while fading, the new one fades in.
        if (u.out == 1u) {
            q = (q - vec2(0.5)) * (1.0 + p) + vec2(0.5);
            tmix = 1.0 - p;
        } else {
            tmix = p;
        }
    } else if (u.tkind == 14u) {
        // Cube: the edge shared by both faces runs from the right side to the left one;
        // each face is squeezed into its part of the frame and shrinks towards its far
        // edge (cheap perspective).
        let x = in.uv.x * 2.0 - 1.0;
        let e = 1.0 - 2.0 * p;
        var fu: f32;
        var depth: f32;
        if (u.out == 1u) {
            if (x > e) { discard; }
            fu = (x + 1.0) / max(e + 1.0, 0.0001);
            depth = p * (1.0 - fu);
        } else {
            if (x < e) { discard; }
            fu = (x - e) / max(1.0 - e, 0.0001);
            depth = (1.0 - p) * fu;
        }
        let k = 1.0 - 0.3 * depth;
        q = vec2(fu, (in.uv.y - 0.5) / k + 0.5);
    } else if (u.tkind == 15u) {
        // Pixelate: blocks grow then shrink while the new picture fades in.
        let sn = sin(p * 3.14159265);
        let cells = 8.0 + pow(1.0 - sn, 4.0) * 2000.0;
        q = (floor(q * cells) + vec2(0.5)) / cells;
        if (u.out == 0u) { tmix = p; }
    } else if (u.tkind == 16u) {
        // Luma wipe: a procedural luma pattern (gradient plus blocky noise) decides the
        // order in which the new picture appears.
        if (u.out == 0u) {
            let g = floor(in.uv * 14.0);
            let n = fract(sin(dot(g, vec2(12.9898, 78.233))) * 43758.5453);
            let l = 0.6 * in.uv.x + 0.4 * n;
            tmix = smoothstep(0.0, 0.2, p * 1.2 - l);
        }
    } else if (u.tkind >= 17u && u.tkind <= 30u) {
        // Reveal masks: the new picture appears where the front d has passed.
        if (u.out == 0u) {
            let x = in.uv.x;
            let y = in.uv.y;
            var d = 0.0;
            switch u.tkind {
                case 17u: { d = ((1.0 - x) + (1.0 - y)) * 0.5; }
                case 18u: { d = (x + (1.0 - y)) * 0.5; }
                case 19u: { d = ((1.0 - x) + y) * 0.5; }
                case 20u: { d = (x + y) * 0.5; }
                case 21u: { d = 1.0 - length((in.uv - vec2(0.5)) * 2.0) / 1.41421356; }
                case 22u: { d = fract(atan2(x - 0.5, 0.5 - y) / 6.28318531); }
                case 23u: { d = 1.0 - fract(atan2(x - 0.5, 0.5 - y) / 6.28318531); }
                case 24u: { d = fract(y * 8.0); }
                case 25u: { d = fract(x * 8.0); }
                case 26u: {
                    let lx = abs(fract(x * 8.0) - 0.5) * 2.0;
                    let ly = abs(fract(y * 8.0) - 0.5) * 2.0;
                    let cell = u32(floor(x * 8.0)) + u32(floor(y * 8.0));
                    d = 0.5 * max(lx, ly) + 0.5 * f32(cell & 1u);
                }
                case 27u: { d = abs(x - 0.5) * 2.0; }
                case 28u: { d = abs(y - 0.5) * 2.0; }
                case 29u: { d = abs(x - 0.5) + abs(y - 0.5); }
                default: { d = thash(u32(floor(x * 32.0)), u32(floor(y * 32.0))); }
            }
            tmix = redge(d, p);
        }
    } else if (u.tkind >= 31u && u.tkind <= 34u) {
        // Slide: the new picture slides over the old one, which stays put.
        if (u.out == 0u) {
            switch u.tkind {
                case 31u: { q.x = q.x + p - 1.0; }
                case 32u: { q.x = q.x + 1.0 - p; }
                case 33u: { q.y = q.y + p - 1.0; }
                default: { q.y = q.y + 1.0 - p; }
            }
        }
    } else if (u.tkind == 35u) {
        if (u.out == 0u) { q = tspin(q, max(p, 0.001), (1.0 - p) * 6.28318531); }
    } else if (u.tkind == 36u) {
        if (u.out == 1u) {
            q = tspin(q, max(1.0 - p, 0.001), p * 6.28318531);
            tmix = 1.0 - p;
        } else {
            tmix = p;
        }
    } else if (u.tkind == 37u || u.tkind == 38u) {
        // Card flip: the old picture squeezes to a line, then the new one opens.
        if ((u.out == 1u) == (p >= 0.5)) { discard; }
        let k = max(select(2.0 * p - 1.0, 1.0 - 2.0 * p, u.out == 1u), 0.001);
        if (u.tkind == 37u) { q.x = (q.x - 0.5) / k + 0.5; } else { q.y = (q.y - 0.5) / k + 0.5; }
    } else if (u.tkind == 39u) {
        // Glitch: horizontal bands jump sideways and switch to the new picture at random.
        let band = u32(floor(in.uv.y * 16.0));
        let st = u32(floor(p * 12.0));
        q.x = q.x + (thash(band, st + 100u) - 0.5) * 0.3 * sin(p * 3.14159265);
        if ((u.out == 1u) == (thash(band, 7u) < p)) { discard; }
    } else if (u.tkind == 40u) {
        // Ripple: rings push both pictures radially while they cross-fade.
        let c = in.uv - vec2(0.5);
        let r = length(c);
        q = q + c / max(r, 1e-5) * (0.03 * sin(p * 3.14159265) * sin(r * 30.0 - p * 20.0));
        tmix = select(p, 1.0 - p, u.out == 1u);
    }
    if (q.x < 0.0 || q.x > 1.0 || q.y < 0.0 || q.y > 1.0) {
        discard;
    }
    if (u.mode == 25u) {
        // LoRez: the source is read at the first texel of each 8x8 block.
        let td = vec2<f32>(textureDimensions(tex));
        q = (floor(q * td / 8.0) * 8.0 + vec2(0.5)) / td;
    }
    if (u.mode == 38u) {
        // Jitter: the source is read at a per-pixel hashed offset (mirrors blend.rs).
        let td = vec2<f32>(textureDimensions(tex));
        let pxy = vec2<u32>(in.pos.xy);
        var h = (pxy.x * 73856093u) ^ (pxy.y * 19349663u);
        h = h ^ (h >> 13u);
        h = h * 0x5bd1e995u;
        h = h ^ (h >> 15u);
        let o = vec2<f32>(f32(i32(h & 7u) - 4), f32(i32((h >> 8u) & 7u) - 4));
        q = clamp(q + o / td, vec2(0.5) / td, vec2(1.0) - vec2(0.5) / td);
    }
    var s = textureSample(tex, smp, q);
    // pad bit 4: the source is premultiplied; blending wants straight colour.
    if ((u.pad & 16u) != 0u && s.a > 0.0) { s = vec4(min(s.rgb / s.a, vec3(1.0)), s.a); }
    // pad = channel mask: bit 0..3 set = R, G, B, A off (off alpha means opaque).
    if ((u.pad & 1u) != 0u) { s.r = 0.0; }
    if ((u.pad & 2u) != 0u) { s.g = 0.0; }
    if ((u.pad & 4u) != 0u) { s.b = 0.0; }
    if ((u.pad & 8u) != 0u) { s.a = 1.0; }
    s = vec4(mix(s.rgb, tcol, tblack), s.a * tmix);
    let md = textureDimensions(mask_tex);
    if (md.x > 1u) {
        s.a = s.a * luma(textureLoad(mask_tex, vec2<i32>(in.pos.xy), 0).rgb);
    }
    let b = textureLoad(base_tex, vec2<i32>(in.pos.xy), 0);
    let id = u.mode;
    var a = clamp(s.a * u.opacity, 0.0, 1.0);
    let l = luma(s.rgb);
    var rgb: vec3<f32>;
    if (id == 1u) {
        let k = 1.0 + (s.a - 1.0) * clamp(u.opacity, 0.0, 1.0);
        return vec4(b.rgb * k, b.a);
    } else if (id == 15u) {
        a = a * l;
        rgb = s.rgb;
    } else if (id == 16u) {
        a = clamp(l * u.opacity, 0.0, 1.0);
        rgb = vec3(1.0);
    } else if (id == 17u) {
        rgb = luma(b.rgb) * s.rgb;
    } else if (id == 18u) {
        rgb = vec3(l);
    } else if (id == 37u) {
        a = a * (1.0 - l);
        rgb = s.rgb;
    } else if (id == 38u) {
        rgb = s.rgb;
    } else if (id == 39u) {
        a = clamp(s.a * clamp((clamp(u.opacity, 0.0, 1.0) * 1.1 - l) / 0.1, 0.0, 1.0), 0.0, 1.0);
        rgb = s.rgb;
    } else if (id == 24u) {
        // Displace: the source's red/green push where the base is read from.
        let bs = vec2<i32>(textureDimensions(base_tex)) - vec2<i32>(1);
        let off = vec2<i32>(round((s.rg - vec2(0.5)) * 32.0));
        let bp = clamp(vec2<i32>(in.pos.xy) + off, vec2<i32>(0), bs);
        rgb = textureLoad(base_tex, bp, 0).rgb;
    } else if (id == 25u) {
        rgb = s.rgb;
    } else if (id >= 19u) {
        rgb = color_mode(id, b.rgb, s.rgb);
        rgb = mix(s.rgb, rgb, b.a);
    } else {
        rgb = vec3(channel(id, b.r, s.r), channel(id, b.g, s.g), channel(id, b.b, s.b));
        // Over a see-through base (a group's own buffer) the blend only applies where the
        // base has coverage: Cs' = (1 - ab) Cs + ab B(Cb, Cs).
        rgb = mix(s.rgb, rgb, b.a);
    }
    // The composition and group buffers hold straight (non-premultiplied) colour.
    if (b.a >= 1.0) {
        return vec4(mix(b.rgb, rgb, a), 1.0);
    }
    let oa = a + b.a * (1.0 - a);
    if (oa <= 0.0) { return vec4(0.0); }
    return vec4((rgb * a + b.rgb * b.a * (1.0 - a)) / oa, oa);
}
"#;

/// The 20 uniform words of a layer pass: the 8 base words, then the inverse transform
/// (`m`, then `t.xy` and the frame size), then the perspective row. An identity transform
/// leaves the last twelve zero, which the shader reads as "no transform" (the exact
/// pre-transform mapping); without X/Y rotation the perspective words stay zero too.
fn uniform_words(base: [u32; 8], xf: &Affine, size: (u32, u32)) -> [u32; 20] {
    let mut w = [0u32; 20];
    w[..8].copy_from_slice(&base);
    if !xf.is_identity() {
        let f = |v: f64| (v as f32).to_bits();
        w[8..12].copy_from_slice(&xf.m.map(f));
        w[12] = f(xf.t[0]);
        w[13] = f(xf.t[1]);
        w[14] = (size.0.max(1) as f32).to_bits();
        w[15] = (size.1.max(1) as f32).to_bits();
        if xf.has_perspective() {
            w[16..19].copy_from_slice(&xf.p.map(f));
            w[19] = 1.0f32.to_bits();
        }
    }
    w
}

/// Vertex stage shared by all effect fragment shaders. `uv` is ISF-style (origin bottom-left).
pub(crate) const FX_VS: &str = r#"
struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex
fn vs(@builtin(vertex_index) i: u32) -> VO {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: VO;
    o.pos = vec4(p[i], 0.0, 1.0);
    o.uv = vec2((p[i].x + 1.0) * 0.5, (p[i].y + 1.0) * 0.5);
    return o;
}
"#;

pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One active (non-bypassed) effect with its uniform block for this frame.
#[derive(Clone)]
pub struct EffectDraw {
    pub kind: String,
    pub uniforms: vjc_fx::Uniforms,
    /// 1 = effect only, 0 = input only (effect skipped), between = mix(in, out, dry_wet).
    pub dry_wet: f32,
    /// Blend of the effect output over its input (`vjc_core::blend::fx_mix_id`; 0 = Normal).
    pub blend: u32,
    /// LUT of a `lut3d` effect (the effect is skipped without one).
    pub lut: Option<std::sync::Arc<vjc_fx::lut::Lut>>,
    /// Set for ML effects: the effect reads a mask produced by an ML worker and is skipped
    /// until the first one arrives.
    pub ml: Option<MlDraw>,
    /// Instance id of a persistent effect (trails, echo): the effect gets its own previous
    /// output as `PREV_PIXEL`. None for ordinary effects.
    pub persistent: Option<uuid::Uuid>,
    /// The effect declares a tick input (freeze): only then does the runtime write the
    /// `NEW_TICK` flag into slot 0. Every other effect's uniforms are left as packed.
    pub ticking: bool,
}

/// Writes the `NEW_TICK` flag into slot 0 of a ticking effect (rate in, flag out) and
/// returns the tick to remember. Effects that do not tick are not touched.
pub(crate) fn apply_tick(
    ticking: bool,
    uniforms: &mut vjc_fx::Uniforms,
    last: Option<i64>,
) -> Option<i64> {
    if !ticking || uniforms[0][3] <= 0.0 {
        return None;
    }
    let (flag, tick) = vjc_fx::tick_flag(uniforms[0][0], uniforms[0][3], last);
    uniforms[0][3] = flag;
    Some(tick)
}

/// What an effect with this dry/wet costs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MixPlan {
    /// Dry only: the effect is not run.
    Skip,
    /// Wet only: the effect output is used as is (no extra pass).
    Wet,
    /// Run the effect, then one mix pass with this amount.
    Blend(f32),
}

fn mix_plan(dry_wet: f32, blend: u32) -> MixPlan {
    if dry_wet.is_nan() || dry_wet <= 0.0 {
        MixPlan::Skip
    } else if dry_wet >= 1.0 && blend == 0 {
        MixPlan::Wet
    } else {
        MixPlan::Blend(dry_wet.min(1.0))
    }
}

/// Mixes the effect input (`a`, dry) with its output (`b`, wet) by `t.x`.
const MIX_FS: &str = r#"
struct U { t: vec4<f32> };
// Blend of base `b` (effect input) and source `s` (effect output), ids as
// `vjc_core::blend::FX_MIX_MODES`.
fn ch(m: u32, b: f32, s: f32) -> f32 {
    switch m {
        case 1u: { return min(b + s, 1.0); }
        case 2u: { return b * s; }
        case 3u: { return 1.0 - (1.0 - b) * (1.0 - s); }
        case 4u: { if (b < 0.5) { return 2.0 * b * s; } return 1.0 - 2.0 * (1.0 - b) * (1.0 - s); }
        case 5u: { return abs(b - s); }
        case 6u: { return min(b, s); }
        case 7u: { return max(b, s); }
        case 8u: { return max(b - s, 0.0); }
        case 9u: { return b + s - 2.0 * b * s; }
        case 10u: { if (s < 0.5) { return 2.0 * b * s; } return 1.0 - 2.0 * (1.0 - b) * (1.0 - s); }
        case 11u: { if (s >= 1.0) { return 1.0; } return min(b / (1.0 - s), 1.0); }
        case 12u: { if (s <= 0.0) { return 0.0; } return 1.0 - min((1.0 - b) / s, 1.0); }
        default: { return s; }
    }
}
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var dry: texture_2d<f32>;
@group(0) @binding(2) var wet: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@fragment
fn main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    // `uv` is ISF-style (y up); textures are stored top row first.
    let p = vec2(uv.x, 1.0 - uv.y);
    let d = textureSample(dry, samp, p);
    let w = textureSample(wet, samp, p);
    let m = u32(u.t.y + 0.5);
    var c = w.rgb;
    if (m != 0u) {
        c = vec3(ch(m, d.r, w.r), ch(m, d.g, w.g), ch(m, d.b, w.b));
    }
    return vec4(mix(d.rgb, c, u.t.x), mix(d.a, w.a, u.t.x));
}
"#;

/// Two effect-sized textures to ping-pong plus a third for the mix result.
type Scratch = [(wgpu::Texture, wgpu::TextureView); 3];

/// Effect pipelines and the ping-pong textures the chains render through.
struct Fx {
    vs: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    pipes: HashMap<String, wgpu::RenderPipeline>,
    bufs: Vec<wgpu::Buffer>,
    next: usize,
    scratch: Pool<(u32, u32), Scratch>,
    mix_bgl: wgpu::BindGroupLayout,
    mix_pipe: wgpu::RenderPipeline,
    mix_bufs: Vec<wgpu::Buffer>,
    mix_next: usize,
    ml: MlStore,
    /// Uploaded LUT strips by LUT identity (the `Arc` keeps the address unique).
    luts: HashMap<usize, LutTex>,
    /// Previous output of each persistent effect instance.
    persist: PersistStore<(wgpu::Texture, wgpu::TextureView)>,
    /// Last tick each ticking persistent instance was drawn at (see `vjc_fx::tick_flag`).
    ticks: HashMap<uuid::Uuid, i64>,
    /// Mixed into the persist key: 0 for the composition, one value per routed output, so
    /// an effect drawn in several outputs in one frame keeps one history for each.
    persist_scope: u128,
    /// `PASSES` of each multipass effect kind (empty/absent = ordinary effect).
    passes: HashMap<String, Vec<vjc_fx::Pass>>,
    /// Persistent `TARGET` buffers, one per (instance, output, target name).
    targets: PersistStore<(wgpu::Texture, wgpu::TextureView)>,
    /// Non-persistent `TARGET` buffers, shared by size and target index (always written
    /// before they are read).
    pass_targets: Pool<(u32, u32, usize), (wgpu::Texture, wgpu::TextureView)>,
    /// Where a targeted pass renders before the result is copied to its buffer.
    pass_tmp: Pool<(u32, u32), (wgpu::Texture, wgpu::TextureView)>,
}

/// A LUT uploaded as a strip texture (see `vjc_fx::lut`).
struct LutTex {
    _lut: std::sync::Arc<vjc_fx::lut::Lut>,
    _tex: wgpu::Texture,
    view: wgpu::TextureView,
}

/// View of the strip texture of `lut`, uploaded on first use.
fn lut_view<'a>(
    luts: &'a mut HashMap<usize, LutTex>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    lut: &std::sync::Arc<vjc_fx::lut::Lut>,
) -> &'a wgpu::TextureView {
    if luts.len() > 16 {
        luts.clear();
    }
    let key = std::sync::Arc::as_ptr(lut) as usize;
    &luts
        .entry(key)
        .or_insert_with(|| {
            let (w, h, px) = lut.strip_rgba8();
            let size = wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            };
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lut_strip"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                tex.as_image_copy(),
                &px,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                size,
            );
            let view = tex.create_view(&Default::default());
            LutTex {
                _lut: lut.clone(),
                _tex: tex,
                view,
            }
        })
        .view
}

/// Key part that separates the persistent histories of the outputs (see `persist_scope`).
fn persist_scope(route: SliceSource) -> u128 {
    const MIX: u128 = 0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835;
    match route {
        SliceSource::Composition => 0,
        SliceSource::Layer(id) => id.as_u128().wrapping_mul(MIX) | 1,
        SliceSource::Group(id) => id.as_u128().wrapping_mul(MIX).rotate_left(7) | 2,
        SliceSource::Preview => MIX,
    }
}

/// Key of one effect instance's history in one output: a hash of both parts, so two
/// (instance, output) pairs only share a key by a 128-bit collision (an XOR collides
/// whenever `a ^ b == c ^ d`).
fn persist_key(id: uuid::Uuid, scope: u128) -> uuid::Uuid {
    use std::hash::{Hash, Hasher};
    let half = |seed: u8| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        seed.hash(&mut h);
        id.as_u128().hash(&mut h);
        scope.hash(&mut h);
        u128::from(h.finish())
    };
    uuid::Uuid::from_u128(half(0) << 64 | half(1))
}

fn make_scratch(device: &wgpu::Device, (w, h): (u32, u32)) -> Scratch {
    let mk = || {
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fx_scratch"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let v = t.create_view(&Default::default());
        (t, v)
    };
    [mk(), mk(), mk()]
}

/// Texture holding a persistent effect's previous output (zeroed by wgpu on creation).
fn make_history(device: &wgpu::Device, (w, h): (u32, u32)) -> (wgpu::Texture, wgpu::TextureView) {
    let t = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fx_history"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let v = t.create_view(&Default::default());
    (t, v)
}

/// One fullscreen fragment pass of an effect or generator shader: `src` is bound as
/// `inputImage`, the result replaces `dst`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_pass(
    device: &wgpu::Device,
    enc: &mut wgpu::CommandEncoder,
    bgl: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    pipe: &wgpu::RenderPipeline,
    buf: &wgpu::Buffer,
    src: &wgpu::TextureView,
    dst: &wgpu::TextureView,
    mask: &wgpu::TextureView,
    prev: &wgpu::TextureView,
) {
    draw_pass_targets(
        device,
        enc,
        bgl,
        sampler,
        pipe,
        buf,
        (src, dst, mask, prev),
        &[],
        None,
    );
}

/// `draw_pass` plus the named `PASSES` targets (binding `TARGET_BINDING + k`); slots
/// without a target are bound to `prev` so the layout is always complete.
#[allow(clippy::too_many_arguments)]
fn draw_pass_targets(
    device: &wgpu::Device,
    enc: &mut wgpu::CommandEncoder,
    bgl: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    pipe: &wgpu::RenderPipeline,
    buf: &wgpu::Buffer,
    (src, dst, mask, prev): (
        &wgpu::TextureView,
        &wgpu::TextureView,
        &wgpu::TextureView,
        &wgpu::TextureView,
    ),
    targets: &[&wgpu::TextureView],
    audio: Option<[&wgpu::TextureView; 2]>,
) {
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: 0,
            resource: buf.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::TextureView(src),
        },
        wgpu::BindGroupEntry {
            binding: 2,
            resource: wgpu::BindingResource::Sampler(sampler),
        },
        wgpu::BindGroupEntry {
            binding: 3,
            resource: wgpu::BindingResource::TextureView(mask),
        },
        wgpu::BindGroupEntry {
            binding: 4,
            resource: wgpu::BindingResource::TextureView(prev),
        },
    ];
    for k in 0..vjc_fx::MAX_TARGETS {
        entries.push(wgpu::BindGroupEntry {
            binding: vjc_fx::TARGET_BINDING + k as u32,
            resource: wgpu::BindingResource::TextureView(targets.get(k).copied().unwrap_or(prev)),
        });
    }
    // Spectrum and waveform; shaders without audio inputs never read them.
    for k in 0..2 {
        entries.push(wgpu::BindGroupEntry {
            binding: vjc_fx::AUDIO_BINDING + k as u32,
            resource: wgpu::BindingResource::TextureView(audio.map_or(prev, |a| a[k])),
        });
    }
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fx_bg"),
        layout: bgl,
        entries: &entries,
    });
    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("fx"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: dst,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipe);
    pass.set_bind_group(0, &bind, &[]);
    pass.draw(0..3, 0..1);
}

impl Fx {
    /// Renders a generator shader into `dst`; `blank` stands in for the unused input image.
    /// Skipped while the shader is not compiled.
    #[allow(clippy::too_many_arguments)]
    fn generate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        blank: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        g: &EffectDraw,
        audio: [&wgpu::TextureView; 2],
    ) {
        let Some(pipe) = self.pipes.get(&g.kind) else {
            // Not compiled or failed: clear, so the layer never shows the previous clip.
            drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("gen_clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            }));
            return;
        };
        if self.bufs.len() <= self.next {
            self.bufs
                .push(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("fx_u"),
                    size: std::mem::size_of::<vjc_fx::Uniforms>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
        }
        let buf = &self.bufs[self.next];
        self.next += 1;
        queue.write_buffer(buf, 0, bytemuck::cast_slice(&g.uniforms));
        draw_pass_targets(
            device,
            enc,
            bgl,
            sampler,
            pipe,
            buf,
            (blank, dst, blank, blank),
            &[],
            Some(audio),
        );
    }

    /// Runs `effects` in order, ping-ponging between two scratch textures of `size`.
    /// Returns the scratch index holding the result, or None if no effect ran (effects
    /// whose shader is not compiled yet are skipped).
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        input: &wgpu::TextureView,
        size: (u32, u32),
        effects: &[EffectDraw],
    ) -> Option<usize> {
        let size = (size.0.max(1), size.1.max(1));
        let mut cur: Option<usize> = None;
        for e in effects {
            let plan = mix_plan(e.dry_wet, e.blend);
            if plan == MixPlan::Skip {
                continue;
            }
            let Some(pipe) = self.pipes.get(&e.kind) else {
                continue;
            };
            let in_view = match cur {
                None => input,
                Some(i) => match self.scratch.get(&size) {
                    Some(sc) => &sc[i].1,
                    None => input,
                },
            };
            let mask = match &e.ml {
                None => None,
                Some(m) => match self.ml.prepare(device, enc, bgl, sampler, in_view, m) {
                    Some(v) => Some(v),
                    None => continue,
                },
            };
            let mask = match (&e.lut, mask) {
                (Some(l), None) => Some(lut_view(&mut self.luts, device, queue, l)),
                (None, m) if e.kind == "lut3d" => {
                    let _ = m;
                    continue;
                }
                (_, m) => m,
            };
            let scratch = self.scratch.acquire(size, || make_scratch(device, size));
            if self.bufs.len() <= self.next {
                self.bufs
                    .push(device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("fx_u"),
                        size: std::mem::size_of::<vjc_fx::Uniforms>() as u64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
            }
            let out = (0..3).find(|&i| Some(i) != cur).unwrap_or(0);
            let src = match cur {
                None => input,
                Some(i) => &scratch[i].1,
            };
            if let Some(passes) = self.passes.get(&e.kind) {
                let base = e.persistent.map(|id| persist_key(id, self.persist_scope));
                let names = vjc_fx::targets_of(passes);
                // Buffer of each target name: persistent when any pass says so (and the
                // draw has an instance id), else pooled.
                let tsize: Vec<(u32, u32)> = names
                    .iter()
                    .map(|n| {
                        passes
                            .iter()
                            .find(|p| p.target.as_deref() == Some(*n))
                            .map_or(size, |p| p.size(size))
                    })
                    .collect();
                let keys: Vec<Option<uuid::Uuid>> = names
                    .iter()
                    .map(|n| {
                        let pers = passes
                            .iter()
                            .any(|p| p.persistent && p.target.as_deref() == Some(*n));
                        base.filter(|_| pers).map(|b| {
                            use std::hash::{Hash, Hasher};
                            let mut h = std::collections::hash_map::DefaultHasher::new();
                            n.hash(&mut h);
                            persist_key(b, u128::from(h.finish()))
                        })
                    })
                    .collect();
                for (k, key) in keys.iter().enumerate() {
                    let sz = tsize[k];
                    match key {
                        Some(key) => {
                            self.targets
                                .acquire(*key, sz, || make_target(device, sz.0, sz.1));
                        }
                        None => {
                            self.pass_targets
                                .acquire((sz.0, sz.1, k), || make_target(device, sz.0, sz.1));
                        }
                    }
                }
                for p in passes.iter().filter(|p| p.target.is_some()) {
                    let sz = p.size(size);
                    self.pass_tmp
                        .acquire(sz, || make_target(device, sz.0, sz.1));
                }
                let tex = |k: usize| -> &(wgpu::Texture, wgpu::TextureView) {
                    match &keys[k] {
                        Some(key) => self.targets.get(key).expect("target acquired"),
                        None => &self.pass_targets[&(tsize[k].0, tsize[k].1, k)],
                    }
                };
                for (pi, pass) in passes.iter().enumerate() {
                    let psize = if pass.target.is_some() {
                        pass.size(size)
                    } else {
                        size
                    };
                    if self.bufs.len() <= self.next {
                        self.bufs
                            .push(device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some("fx_u"),
                                size: std::mem::size_of::<vjc_fx::Uniforms>() as u64,
                                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                                mapped_at_creation: false,
                            }));
                    }
                    let buf = &self.bufs[self.next];
                    self.next += 1;
                    // Slot 0: TIME, this pass's RENDERSIZE, PASSINDEX.
                    let mut u = e.uniforms;
                    u[0][1] = psize.0 as f32;
                    u[0][2] = psize.1 as f32;
                    u[0][3] = pi as f32;
                    queue.write_buffer(buf, 0, bytemuck::cast_slice(&u));
                    let views: Vec<&wgpu::TextureView> =
                        (0..names.len()).map(|k| &tex(k).1).collect();
                    let dst = match pass.target {
                        Some(_) => &self.pass_tmp[&psize].1,
                        None => &scratch[out].1,
                    };
                    draw_pass_targets(
                        device,
                        enc,
                        bgl,
                        sampler,
                        pipe,
                        buf,
                        (src, dst, mask.unwrap_or(src), src),
                        &views,
                        None,
                    );
                    if let Some(t) = &pass.target {
                        let k = names.iter().position(|n| n == t).unwrap_or(0);
                        enc.copy_texture_to_texture(
                            self.pass_tmp[&psize].0.as_image_copy(),
                            tex(k).0.as_image_copy(),
                            wgpu::Extent3d {
                                width: psize.0,
                                height: psize.1,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                }
            } else {
                let buf = &self.bufs[self.next];
                self.next += 1;
                // Persistent effect: its previous output is bound as PREV_PIXEL, and this
                // frame's result is copied back after the pass.
                let hist = e.persistent.map(|id| {
                    let key = persist_key(id, self.persist_scope);
                    let (tex, how) = self
                        .persist
                        .acquire(key, size, || make_history(device, size));
                    (key, tex, how)
                });
                let mut uniforms = e.uniforms;
                // A ticking effect (freeze) learns per instance whether this draw starts a new
                // tick: slot 0 carries its rate in, and the flag goes out in its place.
                if let Some((key, _, how)) = &hist {
                    let last = (*how == crate::persist::Acquired::Kept)
                        .then(|| self.ticks.get(key).copied())
                        .flatten();
                    if let Some(tick) = apply_tick(e.ticking, &mut uniforms, last) {
                        self.ticks.insert(*key, tick);
                    }
                }
                queue.write_buffer(buf, 0, bytemuck::cast_slice(&uniforms));
                let hist = hist.map(|h| h.1);
                draw_pass(
                    device,
                    enc,
                    bgl,
                    sampler,
                    pipe,
                    buf,
                    src,
                    &scratch[out].1,
                    mask.unwrap_or(src),
                    hist.map_or(src, |h| &h.1),
                );
                if let Some(h) = hist {
                    enc.copy_texture_to_texture(
                        scratch[out].0.as_image_copy(),
                        h.0.as_image_copy(),
                        wgpu::Extent3d {
                            width: size.0,
                            height: size.1,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
            if let MixPlan::Blend(t) = plan {
                if self.mix_bufs.len() <= self.mix_next {
                    self.mix_bufs
                        .push(device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("mix_u"),
                            size: 16,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        }));
                }
                let mbuf = &self.mix_bufs[self.mix_next];
                self.mix_next += 1;
                queue.write_buffer(
                    mbuf,
                    0,
                    bytemuck::cast_slice(&[t, e.blend as f32, 0.0, 0.0]),
                );
                let dst = (0..3).find(|&i| Some(i) != cur && i != out).unwrap_or(0);
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mix_bg"),
                    layout: &self.mix_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: mbuf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(src),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&scratch[out].1),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                });
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fx_mix"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &scratch[dst].1,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.mix_pipe);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
                cur = Some(dst);
            } else {
                cur = Some(out);
            }
        }
        cur
    }
}

/// Id of the `LayerDraw` that carries a clip shown only in the Preview monitor. It goes last
/// in the list and is never part of the composition (see `render_routed`).
pub const PREVIEW_CLIP_ID: LayerId = uuid::Uuid::from_u128(u128::MAX);

pub struct LayerDraw<'a> {
    pub id: LayerId,
    pub opacity: f32,
    pub blend: BlendMode,
    pub fit: FitMode,
    /// Clip chain followed by the layer chain, applied to the source before blending.
    pub effects: Vec<EffectDraw>,
    /// `(width, height, rgba, seq)`; `None` = nothing playing (layer skipped).
    pub frame: Option<(u32, u32, &'a [u8], u64)>,
    /// A generator clip: rendered at composition size instead of uploading a frame.
    /// `frame` must be `None` when this is set.
    pub generator: Option<EffectDraw>,
    /// Layer transition: `(shader id, progress 0..1, outgoing)`. The outgoing source is a
    /// second `LayerDraw` (own `id`) placed right below the incoming one.
    pub transition: Option<(u32, f32, bool)>,
    /// Clip then layer transform (output px -> picture px), applied when mapping the picture
    /// into the frame, after the effects of the layer.
    pub transform: Affine,
    /// Channels of the picture that are drawn (an off channel is zeroed, off alpha = opaque).
    pub channels: vjc_model::Channels,
    /// What this layer masks. A layer with a mask mode is not drawn: its luminance (drawn on
    /// black with its own fit, effects and opacity) multiplies the layer(s) below it.
    pub mask: vjc_model::MaskMode,
}

/// Where `draw_layer` puts its pass.
#[derive(Clone, Copy, PartialEq)]
enum Dest {
    Composition,
    Group,
    /// The mask buffer (a mask layer drawn on black).
    Mask,
}

/// Which mask layer (index in the bottom-to-top list) multiplies each layer, and which
/// layers are mask sources (not drawn). A source without a picture masks nothing. The
/// nearest mask above wins; "One below" also covers the outgoing half of a transition.
fn mask_plan(layers: &[LayerDraw]) -> (Vec<Option<usize>>, Vec<bool>) {
    use vjc_model::MaskMode;
    let n = layers.len();
    let is_src: Vec<bool> = layers
        .iter()
        .map(|l| l.mask != MaskMode::Off && (l.frame.is_some() || l.generator.is_some()))
        .collect();
    let outgoing = |j: usize| layers[j].transition.is_some_and(|t| t.2);
    let mut plan = vec![None; n];
    // Top down, so the nearest mask above a layer wins. The outgoing half of a transition
    // is the same layer as the incoming half above it: it is never a mask picture itself
    // and never "the layer below".
    for m in (0..n).rev() {
        if !is_src[m] || outgoing(m) {
            continue;
        }
        match layers[m].mask {
            MaskMode::Off => {}
            MaskMode::AllBelow => {
                for j in (0..m).filter(|j| !is_src[*j]) {
                    plan[j] = Some(m);
                }
            }
            MaskMode::OneBelow => {
                let mut j = m;
                if j > 0 && outgoing(j - 1) {
                    j -= 1;
                }
                let Some(mut j) = j.checked_sub(1) else {
                    continue;
                };
                if !is_src[j] {
                    plan[j] = Some(m);
                    while j > 0 && outgoing(j - 1) {
                        j -= 1;
                        plan[j] = Some(m);
                    }
                }
            }
        }
    }
    (plan, is_src)
}

/// Mask data of one frame, for `draw_layer` calls that may need a mask picture.
struct MaskCtx<'a> {
    layers: &'a [LayerDraw<'a>],
    plan: &'a [Option<usize>],
    is_src: &'a [bool],
    /// Index of the first layer of the run being drawn.
    base: usize,
}

/// A layer group: `len` consecutive entries of the layer list (bottom to top) from `start`
/// are composed into an own buffer, `effects` run over it, then it is blended into the
/// stack like a layer with `opacity` and `blend`.
pub struct GroupDraw {
    /// The model group, so a slice can route it on its own.
    pub id: Option<GroupId>,
    pub start: usize,
    pub len: usize,
    pub opacity: f32,
    pub blend: BlendMode,
    pub effects: Vec<EffectDraw>,
}

/// Intermediate buffer the layers of a group are composed into, one at a time.
struct GroupBuf {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    base: wgpu::Texture,
    base_bind: wgpu::BindGroup,
    base_bind_masked: wgpu::BindGroup,
    size: (u32, u32),
    /// One uniform per group drawn in a frame (writes land before the submit).
    uniforms: Vec<wgpu::Buffer>,
}

struct Slot {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    seq: u64,
}

pub struct Compositor {
    pipe: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    /// Layout of effect passes (`bgl` plus the ML mask).
    fx_bgl: wgpu::BindGroupLayout,
    base_bgl: wgpu::BindGroupLayout,
    base: wgpu::Texture,
    base_bind: wgpu::BindGroup,
    /// Like `base_bind`, but the shader also multiplies the layer by the mask picture.
    base_bind_masked: wgpu::BindGroup,
    /// Mask layer drawn on black (composition size), and the 1x1 "no mask" texture.
    mask: (wgpu::Texture, wgpu::TextureView),
    white: (wgpu::Texture, wgpu::TextureView),
    white_ready: bool,
    /// Index (in the frame's layer list) of the mask layer now held in `mask`.
    mask_cur: Option<usize>,
    sampler: wgpu::Sampler,
    /// Spectrum and waveform rows read by generators with ISF audio inputs.
    audio: [(wgpu::Texture, wgpu::TextureView); 2],
    /// 1x1 texture bound as the (unused) input image of generators.
    blank: wgpu::TextureView,
    /// Opaque black 1x1 texture, its uniform and bind group: draws the master fade.
    fade: (wgpu::Texture, wgpu::Buffer, wgpu::BindGroup),
    slots: HashMap<LayerId, Slot>,
    group: Option<GroupBuf>,
    /// Layers / groups rendered on their own because a slice routes them (only the ones
    /// in use are kept).
    routed: HashMap<SliceSource, (wgpu::Texture, wgpu::TextureView)>,
    /// Layer / group the preview monitor shows (`None` = no preview render). Set by the
    /// caller before each render; the result is stored under `SliceSource::Preview`.
    pub preview: Option<SliceSource>,
    fx: Fx,
    /// Copy of the composition and its uniform / bind group, for the composition transform.
    xf: Option<(wgpu::Texture, wgpu::Buffer, wgpu::BindGroup)>,
    /// Composition transform, applied to the finished stack before the master effects.
    /// Set by the caller before each render.
    pub master_transform: Affine,
    pub target: wgpu::Texture,
    pub target_view: wgpu::TextureView,
    pub size: (u32, u32),
}

fn make_target(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let t = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("composition"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let v = t.create_view(&Default::default());
    (t, v)
}

/// Copy of the composition used as the blend base (a pass cannot read what it writes).
fn make_base(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    w: u32,
    h: u32,
    (white, mask): (&wgpu::TextureView, &wgpu::TextureView),
) -> (wgpu::Texture, wgpu::BindGroup, wgpu::BindGroup) {
    let t = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("composition_base"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let v = t.create_view(&Default::default());
    let make = |m: &wgpu::TextureView| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composition_base_bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&v),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(m),
                },
            ],
        })
    };
    // Plain (1x1 white = no mask) and masked (the mask layer's picture).
    (t, make(white), make(mask))
}

/// Bind group layout of every effect / generator pass (also used by the browser thumbnails).
pub(crate) fn fx_bind_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let mut e: Vec<wgpu::BindGroupLayoutEntry> = vec![];
    for binding in 0..vjc_fx::AUDIO_BINDING + 2 {
        e.push(wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: match binding {
                0 => wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                2 => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                _ => wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            },
            count: None,
        });
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("fx_bgl"),
        entries: &e,
    })
}

impl Compositor {
    pub fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compositor"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compositor_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // Effect passes: the layer bindings plus the ML mask texture.
        let fx_bgl = fx_bind_layout(device);
        let base_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compositor_base_bgl"),
            entries: &[0, 1].map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compositor_pl"),
            bind_group_layouts: &[Some(&bgl), Some(&base_bgl)],
            immediate_size: 0,
        });
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("compositor_pipe"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let fade_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fade"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let fade_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fade_u"),
            size: 80,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fade_view = fade_tex.create_view(&Default::default());
        let fade_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fade_bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: fade_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&fade_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let fx_vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx_vs"),
            source: wgpu::ShaderSource::Wgsl(FX_VS.into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let mix_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mix_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let mix_fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mix_fs"),
            source: wgpu::ShaderSource::Wgsl(MIX_FS.into()),
        });
        let mix_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mix_pl"),
            bind_group_layouts: &[Some(&mix_bgl)],
            immediate_size: 0,
        });
        let mix_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mix"),
            layout: Some(&mix_pl),
            vertex: wgpu::VertexState {
                module: &fx_vs,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &mix_fs,
                entry_point: Some("main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let fx_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx_pl"),
            bind_group_layouts: &[Some(&fx_bgl)],
            immediate_size: 0,
        });
        let ml = MlStore::new(device, &fx_vs, &fx_layout);
        let fx = Fx {
            vs: fx_vs,
            layout: fx_layout,
            ml,
            pipes: HashMap::new(),
            bufs: vec![],
            next: 0,
            scratch: Pool::new(),
            mix_bgl,
            mix_pipe,
            mix_bufs: vec![],
            mix_next: 0,
            luts: HashMap::new(),
            persist: PersistStore::default(),
            ticks: HashMap::new(),
            persist_scope: 0,
            passes: HashMap::new(),
            targets: PersistStore::default(),
            pass_targets: Pool::new(),
            pass_tmp: Pool::new(),
        };
        let (target, target_view) = make_target(device, w, h);
        let white = make_target(device, 1, 1);
        // At least 2x2: a 1x1 mask texture means "no mask" to the shader.
        let mask = make_target(device, w.max(2), h.max(2));
        let (base, base_bind, base_bind_masked) =
            make_base(device, &base_bgl, w, h, (&white.1, &mask.1));
        Compositor {
            pipe,
            bgl,
            fx_bgl,
            base_bgl,
            base,
            base_bind,
            base_bind_masked,
            mask,
            white,
            white_ready: false,
            mask_cur: None,
            sampler,
            audio: [0, 1].map(|_| {
                let t = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("fx_audio"),
                    size: wgpu::Extent3d {
                        width: vjc_fx::AUDIO_TEX_W as u32,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let v = t.create_view(&Default::default());
                (t, v)
            }),
            blank,
            fade: (fade_tex, fade_buf, fade_bind),
            slots: HashMap::new(),
            group: None,
            routed: HashMap::new(),
            preview: None,
            fx,
            xf: None,
            master_transform: Affine::IDENTITY,
            target,
            target_view,
            size: (w, h),
        }
    }

    /// Recreates the target if the composition resolution changed. Returns true when recreated
    /// (the caller must re-register the egui texture).
    /// Estimated bytes of live textures (thumbnails are the caller's).
    pub fn vram(&self) -> crate::vram::Usage {
        use crate::vram::gpu_bytes as b;
        let mut fixed = b(&self.base) + b(&self.target) + b(&self.fade.0) + b(&self.mask.0);
        fixed += self.xf.as_ref().map_or(0, |x| b(&x.0));
        fixed += self.group.as_ref().map_or(0, |g| b(&g.tex) + b(&g.base));
        fixed += self.routed.values().map(|r| b(&r.0)).sum::<u64>();
        crate::vram::Usage {
            fixed,
            players: self.slots.values().map(|s| b(&s.tex)).sum(),
            pool: self
                .fx
                .scratch
                .values()
                .flat_map(|s| s.iter())
                .map(|t| b(&t.0))
                .sum(),
            persist: self
                .fx
                .persist
                .values()
                .chain(self.fx.targets.values())
                .map(|t| b(&t.0))
                .sum(),
            thumbs: 0,
        }
    }

    /// Drops the effect scratch pool; it is rebuilt on demand.
    #[cfg(test)]
    pub fn evict_scratch(&mut self) -> usize {
        self.fx.scratch.clear()
    }

    /// Blocking readback of the composition output as tight RGBA8 (a one-off, for snapshots).
    pub fn read_rgba(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let (w, h) = (self.target.width(), self.target.height());
        let row = (w * 4).div_ceil(256) * 256;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("snapshot"),
            size: u64::from(row) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        {
            let data = buf.slice(..).get_mapped_range().ok()?;
            for y in 0..h as usize {
                out.extend_from_slice(&data[y * row as usize..][..(w * 4) as usize]);
            }
        }
        buf.unmap();
        Some((w, h, out))
    }

    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32) -> bool {
        if self.size == (w, h) {
            return false;
        }
        (self.target, self.target_view) = make_target(device, w.max(1), h.max(1));
        self.mask = make_target(device, w.max(2), h.max(2));
        (self.base, self.base_bind, self.base_bind_masked) = make_base(
            device,
            &self.base_bgl,
            w.max(1),
            h.max(1),
            (&self.white.1, &self.mask.1),
        );
        self.size = (w, h);
        true
    }

    /// Installs a translated effect shader (WGSL from `vjc_fx::translate`). Returns the
    /// validation error instead of panicking, so a bad shader only disables its effect.
    #[cfg(test)]
    pub fn add_effect(
        &mut self,
        device: &wgpu::Device,
        kind: &str,
        wgsl: &str,
    ) -> Result<(), String> {
        self.add_effect_passes(device, kind, wgsl, &[])
    }

    /// `add_effect` for an ISF with `PASSES` (empty = ordinary single-pass effect).
    pub fn add_effect_passes(
        &mut self,
        device: &wgpu::Device,
        kind: &str,
        wgsl: &str,
        passes: &[vjc_fx::Pass],
    ) -> Result<(), String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(kind),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(kind),
            layout: Some(&self.fx.layout),
            vertex: wgpu::VertexState {
                module: &self.fx.vs,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: Some("main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        match pollster::block_on(scope.pop()) {
            Some(e) => Err(e.to_string()),
            None => {
                self.fx.pipes.insert(kind.to_string(), pipe);
                if passes.is_empty() {
                    self.fx.passes.remove(kind);
                } else {
                    self.fx.passes.insert(kind.to_string(), passes.to_vec());
                }
                Ok(())
            }
        }
    }

    /// Draws one layer onto the composition (or the group buffer): effects, then one blend
    /// pass reading a copy of what is below.
    #[allow(clippy::too_many_arguments)]
    fn draw_layer(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        l: &LayerDraw,
        (dest, masked): (Dest, bool),
        extent: wgpu::Extent3d,
    ) {
        let Some(slot) = self.slots.get(&l.id) else {
            return;
        };
        if let Some(g) = &l.generator {
            self.fx.generate(
                device,
                queue,
                enc,
                &self.fx_bgl,
                &self.sampler,
                &self.blank,
                &slot.view,
                g,
                [&self.audio[0].1, &self.audio[1].1],
            );
        }
        let out = self.fx.run(
            device,
            queue,
            enc,
            &self.fx_bgl,
            &self.sampler,
            &slot.view,
            slot.size,
            &l.effects,
        );
        let (dst_tex, dst_view, base_tex, base_bind) = match (&self.group, dest) {
            (Some(g), Dest::Group) => (
                &g.tex,
                &g.view,
                &g.base,
                if masked {
                    &g.base_bind_masked
                } else {
                    &g.base_bind
                },
            ),
            (_, Dest::Mask) => (&self.mask.0, &self.mask.1, &self.base, &self.base_bind),
            _ => (
                &self.target,
                &self.target_view,
                &self.base,
                if masked {
                    &self.base_bind_masked
                } else {
                    &self.base_bind
                },
            ),
        };
        let fx_bind;
        let bind = match out {
            None => &slot.bind,
            Some(i) => {
                fx_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("layer_fx_bg"),
                    layout: &self.bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: slot.uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(
                                &self.fx.scratch[&slot.size][i].1,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
                &fx_bind
            }
        };
        enc.copy_texture_to_texture(dst_tex.as_image_copy(), base_tex.as_image_copy(), extent);
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("compose"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipe);
        pass.set_bind_group(0, bind, &[]);
        pass.set_bind_group(1, base_bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Draws the mask layer `m` on black into the mask buffer (once per frame and layer).
    fn render_mask(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        layers: &[LayerDraw],
        m: usize,
        extent: wgpu::Extent3d,
    ) {
        if self.mask_cur == Some(m) {
            return;
        }
        drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mask_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.mask.1,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        self.draw_layer(device, queue, enc, &layers[m], (Dest::Mask, false), extent);
        self.mask_cur = Some(m);
    }

    /// Draws `layers[i]` into the composition or the group buffer, masked when the plan
    /// says so. Mask sources are not drawn.
    fn draw_planned(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        mc: &MaskCtx,
        (i, dest): (usize, Dest),
        extent: wgpu::Extent3d,
    ) {
        if mc.is_src[i] {
            return;
        }
        let masked = mc.plan[i];
        if let Some(m) = masked {
            self.render_mask(device, queue, enc, mc.layers, m, extent);
        }
        self.draw_layer(
            device,
            queue,
            enc,
            &mc.layers[i],
            (dest, masked.is_some()),
            extent,
        );
    }

    /// Composes `run` into the group buffer (transparent start), applies the group chain and
    /// blends the result into the composition. False when nothing in the group is drawn.
    #[allow(clippy::too_many_arguments)]
    fn compose_group(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        (run, mc): (&[LayerDraw], Option<&MaskCtx>),
        g: &GroupDraw,
        n: usize,
        extent: wgpu::Extent3d,
    ) -> bool {
        if !run.iter().any(|l| self.slots.contains_key(&l.id)) {
            return false;
        }
        let size = (self.size.0.max(1), self.size.1.max(1));
        if self.group.as_ref().is_none_or(|b| b.size != size) {
            let (tex, view) = make_target(device, size.0, size.1);
            let (base, base_bind, base_bind_masked) = make_base(
                device,
                &self.base_bgl,
                size.0,
                size.1,
                (&self.white.1, &self.mask.1),
            );
            self.group = Some(GroupBuf {
                tex,
                view,
                base,
                base_bind,
                base_bind_masked,
                size,
                uniforms: vec![],
            });
        }
        let gb = self.group.as_mut().unwrap();
        while gb.uniforms.len() <= n {
            gb.uniforms
                .push(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("group_u"),
                    size: 80,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
        }
        drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("group_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gb.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        for (k, l) in run.iter().enumerate() {
            match mc {
                Some(mc) => {
                    self.draw_planned(device, queue, enc, mc, (mc.base + k, Dest::Group), extent)
                }
                None => self.draw_layer(device, queue, enc, l, (Dest::Group, false), extent),
            }
        }
        let gb = self.group.as_ref().unwrap();
        let out = self.fx.run(
            device,
            queue,
            enc,
            &self.fx_bgl,
            &self.sampler,
            &gb.view,
            size,
            &g.effects,
        );
        queue.write_buffer(
            &gb.uniforms[n],
            0,
            bytemuck::bytes_of(&[
                g.opacity.to_bits(),
                shader_id(&g.blend),
                1.0f32.to_bits(),
                1.0f32.to_bits(),
                0.0f32.to_bits(),
                0u32,
                0u32,
                0u32,
            ]),
        );
        let src = match out {
            None => &gb.view,
            Some(i) => &self.fx.scratch[&size][i].1,
        };
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("group_bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: gb.uniforms[n].as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        enc.copy_texture_to_texture(
            self.target.as_image_copy(),
            self.base.as_image_copy(),
            extent,
        );
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("group_blend"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipe);
        pass.set_bind_group(0, &bind, &[]);
        pass.set_bind_group(1, &self.base_bind, &[]);
        pass.draw(0..3, 0..1);
        true
    }

    /// Uploads this frame's audio analysis for generators with ISF `audio` / `audioFFT`
    /// inputs: `spectrum` and `wave` in 0..1 (wave 0.5 = silence); without `active` audio
    /// the textures read zero and the shaders fall back to their synthetic picture.
    pub fn set_audio(&self, queue: &wgpu::Queue, spectrum: &[f32], wave: &[f32], active: bool) {
        for (tex, data) in self.audio.iter().zip([spectrum, wave]) {
            queue.write_texture(
                tex.0.as_image_copy(),
                &vjc_fx::audio_texels(data, active),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(vjc_fx::AUDIO_TEX_W as u32 * 4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: vjc_fx::AUDIO_TEX_W as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// `master` runs over the finished composition, then the master fade: black drawn with
    /// alpha `1 - master_opacity` (0 = black output, used by bypass too).
    #[cfg(test)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers_bottom_to_top: &[LayerDraw],
        master: &[EffectDraw],
        master_opacity: f32,
    ) {
        self.render_groups(
            device,
            queue,
            layers_bottom_to_top,
            &[],
            master,
            master_opacity,
        );
    }

    /// Like `render`, with layer groups: each `GroupDraw` covers a run of `layers_bottom_to_top`.
    #[cfg(test)]
    pub fn render_groups(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers_bottom_to_top: &[LayerDraw],
        groups: &[GroupDraw],
        master: &[EffectDraw],
        master_opacity: f32,
    ) {
        self.render_routed(
            device,
            queue,
            (layers_bottom_to_top, groups),
            (master, master_opacity),
            &[],
        );
    }

    /// The view of a routed layer / group rendered by the last `render_routed`.
    pub fn routed_view(&self, source: SliceSource) -> Option<&wgpu::TextureView> {
        self.routed.get(&source).map(|t| &t.1)
    }

    /// Composes the stack, and renders each layer / group in `routes` on its own (same size,
    /// black background, master fade applied, master effects not) for slices that route it.
    pub fn render_routed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        (all_layers, groups): (&[LayerDraw], &[GroupDraw]),
        (master, master_opacity): (&[EffectDraw], f32),
        routes: &[SliceSource],
    ) {
        // A preview-only clip sits last: it gets a slot and can be routed to the Preview,
        // but the composition below is built without it.
        let layers_bottom_to_top = match all_layers.split_last() {
            Some((l, rest)) if l.id == PREVIEW_CLIP_ID => rest,
            _ => all_layers,
        };
        self.fx.next = 0;
        self.fx.mix_next = 0;
        self.fx.scratch.begin_frame();
        self.fx.pass_targets.begin_frame();
        self.fx.pass_tmp.begin_frame();
        let live: Vec<LayerId> = all_layers
            .iter()
            .filter(|l| l.frame.is_some() || l.generator.is_some())
            .map(|l| l.id)
            .collect();
        self.slots.retain(|k, _| live.contains(k));
        for l in all_layers {
            let (w, h, data, seq) = match (&l.frame, &l.generator) {
                (Some((w, h, data, seq)), _) => (*w, *h, Some(*data), *seq),
                (None, Some(_)) => (self.size.0.max(1), self.size.1.max(1), None, u64::MAX),
                (None, None) => continue,
            };
            let recreate = self.slots.get(&l.id).is_none_or(|s| s.size != (w, h));
            if recreate {
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("layer"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("layer_u"),
                    size: 80,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let view = tex.create_view(&Default::default());
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("layer_bg"),
                    layout: &self.bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
                self.slots.insert(
                    l.id,
                    Slot {
                        view,
                        tex,
                        size: (w, h),
                        uniform,
                        bind,
                        seq: u64::MAX,
                    },
                );
            }
            let slot = self.slots.get_mut(&l.id).unwrap();
            if let Some(data) = data
                && slot.seq != seq
            {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &slot.tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * w),
                        rows_per_image: Some(h),
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                slot.seq = seq;
            }
            let (sx, sy) = fit_scale((w, h), self.size, l.fit);
            queue.write_buffer(
                &slot.uniform,
                0,
                bytemuck::bytes_of(&uniform_words(
                    [
                        l.opacity.to_bits(),
                        shader_id(&l.blend),
                        sx.to_bits(),
                        sy.to_bits(),
                        l.transition.map_or(0.0f32, |t| t.1).to_bits(),
                        l.transition.map_or(0, |t| t.0),
                        u32::from(l.transition.is_some_and(|t| t.2)),
                        l.channels.off_mask(),
                    ],
                    &l.transform,
                    self.size,
                )),
            );
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("compose"),
        });
        // Clear, then one pass per layer reading a copy of the composition below it.
        drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("compose_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        let extent = wgpu::Extent3d {
            width: self.size.0.max(1),
            height: self.size.1.max(1),
            depth_or_array_layers: 1,
        };
        let (plan, is_src) = mask_plan(layers_bottom_to_top);
        self.mask_cur = None;
        if !self.white_ready {
            drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("white_init"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.white.1,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            }));
            self.white_ready = true;
        }
        let mut i = 0;
        let mut group_no = 0;
        while i < layers_bottom_to_top.len() {
            let group = groups
                .iter()
                .find(|g| g.start == i && g.len > 0 && i + g.len <= layers_bottom_to_top.len());
            if let Some(g) = group {
                let run = &layers_bottom_to_top[i..i + g.len];
                let mc = MaskCtx {
                    layers: layers_bottom_to_top,
                    plan: &plan,
                    is_src: &is_src,
                    base: i,
                };
                if self.compose_group(
                    device,
                    queue,
                    &mut enc,
                    (run, Some(&mc)),
                    g,
                    group_no,
                    extent,
                ) {
                    group_no += 1;
                }
                i += g.len;
            } else {
                let mc = MaskCtx {
                    layers: layers_bottom_to_top,
                    plan: &plan,
                    is_src: &is_src,
                    base: i,
                };
                self.draw_planned(device, queue, &mut enc, &mc, (i, Dest::Composition), extent);
                i += 1;
            }
        }
        if !self.master_transform.is_identity() {
            self.transform_pass(device, queue, &mut enc, extent);
        }
        if let Some(i) = self.fx.run(
            device,
            queue,
            &mut enc,
            &self.fx_bgl,
            &self.sampler,
            &self.target_view,
            self.size,
            master,
        ) {
            enc.copy_texture_to_texture(
                self.fx.scratch[&self.size][i].0.as_image_copy(),
                self.target.as_image_copy(),
                extent,
            );
        }
        let fade = 1.0 - master_opacity.clamp(0.0, 1.0);
        if fade > 0.0 {
            queue.write_texture(
                self.fade.0.as_image_copy(),
                &[0, 0, 0, 255],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            queue.write_buffer(
                &self.fade.1,
                0,
                bytemuck::bytes_of(&[
                    fade.to_bits(),
                    shader_id(&BlendMode::Normal),
                    1.0f32.to_bits(),
                    1.0f32.to_bits(),
                ]),
            );
            self.fade_pass(&mut enc, extent);
        }
        let preview = self
            .preview
            .filter(|p| matches!(p, SliceSource::Layer(_) | SliceSource::Group(_)));
        self.routed
            .retain(|k, _| routes.contains(k) || (*k == SliceSource::Preview && preview.is_some()));
        let jobs = routes
            .iter()
            .map(|r| (*r, *r))
            .chain(preview.map(|p| (SliceSource::Preview, p)));
        for (key, content) in jobs {
            self.render_route(
                device,
                queue,
                &mut enc,
                (key, content),
                (all_layers, groups),
                (extent, fade > 0.0, &mut group_no),
            );
        }
        self.fx.scratch.sweep();
        self.fx.pass_targets.sweep();
        self.fx.pass_tmp.sweep();
        self.fx.persist.sweep();
        self.fx.targets.sweep();
        let persist = &self.fx.persist;
        self.fx.ticks.retain(|k, _| persist.contains(k));
        queue.submit(Some(enc.finish()));
        self.fx.ml.dispatch(device, queue);
    }

    /// Redraws the finished composition through `master_transform` onto black.
    fn transform_pass(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        extent: wgpu::Extent3d,
    ) {
        let size = (extent.width, extent.height);
        if self
            .xf
            .as_ref()
            .is_none_or(|x| (x.0.width(), x.0.height()) != size)
        {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("composition_xf"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("composition_xf_u"),
                size: 80,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let view = tex.create_view(&Default::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("composition_xf_bg"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.xf = Some((tex, buf, bind));
        }
        let (tex, buf, bind) = self.xf.as_ref().unwrap();
        queue.write_buffer(
            buf,
            0,
            bytemuck::bytes_of(&uniform_words(
                [
                    1.0f32.to_bits(),
                    shader_id(&BlendMode::Normal),
                    1.0f32.to_bits(),
                    1.0f32.to_bits(),
                    0.0f32.to_bits(),
                    0,
                    0,
                    0,
                ],
                &self.master_transform,
                self.size,
            )),
        );
        enc.copy_texture_to_texture(self.target.as_image_copy(), tex.as_image_copy(), extent);
        drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("xf_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        enc.copy_texture_to_texture(
            self.target.as_image_copy(),
            self.base.as_image_copy(),
            extent,
        );
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composition_xf"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipe);
        pass.set_bind_group(0, bind, &[]);
        pass.set_bind_group(1, &self.base_bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Blends the black master fade (set up by `render_routed`) over the current target.
    fn fade_pass(&self, enc: &mut wgpu::CommandEncoder, extent: wgpu::Extent3d) {
        enc.copy_texture_to_texture(
            self.target.as_image_copy(),
            self.base.as_image_copy(),
            extent,
        );
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("master_fade"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipe);
        pass.set_bind_group(0, &self.fade.2, &[]);
        pass.set_bind_group(1, &self.base_bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Renders one routed layer or group alone into its own texture, by swapping it in as
    /// the composition target for the duration of the draw.
    fn render_route(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        (route, content): (SliceSource, SliceSource),
        (layers, groups): (&[LayerDraw], &[GroupDraw]),
        (extent, fade, group_no): (wgpu::Extent3d, bool, &mut usize),
    ) {
        let size = (self.size.0.max(1), self.size.1.max(1));
        let mut rt = match self.routed.remove(&route) {
            Some(t) if (t.0.width(), t.0.height()) == size => t,
            _ => make_target(device, size.0, size.1),
        };
        std::mem::swap(&mut self.target, &mut rt.0);
        std::mem::swap(&mut self.target_view, &mut rt.1);
        drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("route_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        self.fx.persist_scope = persist_scope(route);
        match content {
            SliceSource::Layer(id) => {
                if let Some(l) = layers.iter().find(|l| l.id == id) {
                    self.draw_layer(device, queue, enc, l, (Dest::Composition, false), extent);
                }
            }
            SliceSource::Group(id) => {
                let found = groups
                    .iter()
                    .find(|g| g.id == Some(id) && g.len > 0 && g.start + g.len <= layers.len());
                if let Some(g) = found {
                    let run = &layers[g.start..g.start + g.len];
                    if self.compose_group(device, queue, enc, (run, None), g, *group_no, extent) {
                        *group_no += 1;
                    }
                }
            }
            SliceSource::Composition | SliceSource::Preview => {}
        }
        self.fx.persist_scope = 0;
        if fade {
            self.fade_pass(enc, extent);
        }
        std::mem::swap(&mut self.target, &mut rt.0);
        std::mem::swap(&mut self.target_view, &mut rt.1);
        self.routed.insert(route, rt);
    }

    /// Reduced frames of ML effects whose readback finished (never waits).
    pub fn ml_poll(&mut self, device: &wgpu::Device) -> Vec<MlFrame> {
        self.fx.ml.poll(device)
    }

    /// Installs the newest mask of an ML effect instance (one byte per pixel, top row first).
    pub fn ml_set_mask(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: u128,
        size: (u32, u32),
        data: &[u8],
    ) {
        self.fx.ml.set_mask(device, queue, key, size, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_ticking_effect_gets_the_tick_flag() {
        let mut u = [[0.0; 4]; vjc_fx::MAX_SLOTS];
        u[0] = [0.5, 8.0, 8.0, 3.0];
        // A persistent effect with a positive value in that lane is not a ticker.
        let mut plain = u;
        assert_eq!(apply_tick(false, &mut plain, None), None);
        assert_eq!(plain, u, "uniforms untouched");
        // A ticking one: rate in, flag out (2 = first draw).
        let mut t = u;
        assert!(apply_tick(true, &mut t, None).is_some());
        assert_eq!(t[0][3], 2.0);
    }

    fn near(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5
    }

    #[test]
    fn fit_fill_stretch() {
        // 4:3 video in a 16:9 frame.
        let (src, dst) = ((400, 300), (1920, 1080));
        assert!(near(fit_scale(src, dst, FitMode::Fit), (0.75, 1.0)));
        assert!(near(fit_scale(src, dst, FitMode::Fill), (1.0, 4.0 / 3.0)));
        assert!(near(fit_scale(src, dst, FitMode::Stretch), (1.0, 1.0)));
        // Same aspect: all modes cover the frame.
        assert!(near(fit_scale((1920, 1080), dst, FitMode::Fit), (1.0, 1.0)));
    }

    #[test]
    fn original_rect_is_native_size_and_centred() {
        let (src, dst) = ((400, 300), (1920, 1080));
        let (x, y, w, h) = fit_rect(src, dst, FitMode::Original);
        assert!((w - 400.0).abs() < 1e-2 && (h - 300.0).abs() < 1e-2);
        assert!((x - 760.0).abs() < 1e-2 && (y - 390.0).abs() < 1e-2);
        // Larger than the frame: native size still, overflow cropped by the frame.
        let (_, _, w, _) = fit_rect((3840, 2160), dst, FitMode::Original);
        assert!((w - 3840.0).abs() < 1e-1);
        // Control: Fit does not give the native size.
        let (_, _, w, h) = fit_rect(src, dst, FitMode::Fit);
        assert!((w - 400.0).abs() > 1.0 && (h - 300.0).abs() > 1.0);
    }

    /// Renders every composition mode on the GPU and compares one pixel to the Rust reference.
    /// Skipped when there is no adapter.
    #[test]
    fn gpu_matches_reference() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        let base = [204u8, 128, 51, 255];
        let src = [102u8, 128, 230, 255];
        let opacity = 0.7;
        let px = |c: [u8; 4]| c.repeat(16);
        let (bd, sd) = (px(base), px(src));
        let f = |c: [u8; 4]| c.map(|v| v as f32 / 255.0);
        let mut comp = Compositor::new(&device, 4, 4);
        for (mode, name) in vjc_core::blend::COMPOSITION_MODES {
            let layers = [
                LayerDraw {
                    id: uuid::Uuid::from_u128(1),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects: vec![],
                    frame: Some((4, 4, &bd, 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                },
                LayerDraw {
                    id: uuid::Uuid::from_u128(2),
                    opacity,
                    blend: mode.clone(),
                    fit: FitMode::Stretch,
                    effects: vec![],
                    frame: Some((4, 4, &sd, 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                },
            ];
            comp.render(&device, &queue, &layers, &[], 1.0);
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256 * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = device.create_command_encoder(&Default::default());
            enc.copy_texture_to_buffer(
                comp.target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(4),
                    },
                },
                wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(enc.finish()));
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
            let got: Vec<u8> = buf.slice(..).get_mapped_range().expect("map")[..4].to_vec();
            buf.unmap();
            let want = vjc_core::blend::blend_pixel(mode, f(base), f(src), opacity);
            for i in 0..3 {
                let g = got[i] as f32 / 255.0;
                assert!((g - want[i]).abs() < 0.02, "{name}: {got:?} vs {want:?}");
            }
        }
    }

    /// Dissolve at 50% on one pixel: black outgoing, white incoming -> mid grey.
    /// Skipped when there is no adapter.
    #[test]
    fn gpu_dissolve_half_one_pixel() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        let draw = |n: u128, px: &'static [u8; 4], out: bool| LayerDraw {
            id: uuid::Uuid::from_u128(n),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: vec![],
            frame: Some((1, 1, px, 1)),
            generator: None,
            transition: Some((vjc_core::transition::Kind::Dissolve.shader_id(), 0.5, out)),
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        };
        let layers = [
            draw(1, &[0, 0, 0, 255], true),
            draw(2, &[255, 255, 255, 255], false),
        ];
        let mut comp = Compositor::new(&device, 1, 1);
        comp.render(&device, &queue, &layers, &[], 1.0);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let got: Vec<u8> = buf.slice(..).get_mapped_range().expect("map")[..4].to_vec();
        buf.unmap();
        for g in &got[..3] {
            assert!(g.abs_diff(128) <= 2, "{got:?}");
        }
    }

    /// Renders `old` (outgoing) and `new` (incoming) flat colours of a transition at
    /// `progress` on a `w` x `h` frame; returns the RGBA of every pixel, row by row.
    /// `None` when there is no adapter.
    fn render_transition(
        kind: vjc_core::transition::Kind,
        progress: f32,
        (w, h): (u32, u32),
        old: &'static [u8; 4],
        new: &'static [u8; 4],
    ) -> Option<Vec<[u8; 4]>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let draw = |n: u128, px: &'static [u8; 4], out: bool| LayerDraw {
            id: uuid::Uuid::from_u128(n),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: vec![],
            frame: Some((1, 1, px, 1)),
            generator: None,
            transition: Some((kind.shader_id(), progress, out)),
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        };
        let layers = [draw(1, old, true), draw(2, new, false)];
        let mut comp = Compositor::new(&device, w, h);
        comp.render(&device, &queue, &layers, &[], 1.0);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = buf.slice(..).get_mapped_range().expect("map").to_vec();
        buf.unmap();
        let mut out = vec![];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * 256 + x * 4;
                out.push([data[i], data[i + 1], data[i + 2], data[i + 3]]);
            }
        }
        Some(out)
    }

    /// The texture estimate follows the live layers and persistent buffers, and the scratch
    /// pool can be dropped without touching them. Skipped when there is no adapter.
    #[test]
    fn gpu_vram_estimate_tracks_live_textures_and_evicts_scratch() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            return;
        };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("invert").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 8, 8);
        comp.add_effect(&device, "invert", &wgsl).unwrap();
        let idle = comp.vram();
        assert_eq!(idle.players, 0);
        assert_eq!(idle.pool, 0);
        assert!(idle.fixed >= 2 * 8 * 8 * 4, "target and base: {idle:?}");
        let fx = EffectDraw {
            lut: None,
            blend: Default::default(),
            kind: "invert".into(),
            uniforms: isf.pack(&isf.default_params(), 0.0, (8, 8)),
            dry_wet: 1.0,
            ml: None,
            persistent: Some(uuid::Uuid::from_u128(99)),
            ticking: false,
        };
        let layer = LayerDraw {
            id: uuid::Uuid::from_u128(7),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: vec![fx],
            frame: Some((4, 4, &[9; 64], 1)),
            generator: None,
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        };
        comp.render(&device, &queue, &[layer], &[], 1.0);
        let busy = comp.vram();
        assert_eq!(busy.players, 4 * 4 * 4, "one 4x4 player texture");
        assert_eq!(
            busy.pool,
            3 * 4 * 4 * 4,
            "scratch trio at the layer size: {busy:?}"
        );
        assert_eq!(busy.persist, 4 * 4 * 4, "one history buffer");
        assert_eq!(comp.evict_scratch(), 1);
        let after = comp.vram();
        assert_eq!(after.pool, 0);
        // Negative control: live textures are not caches and stay.
        assert_eq!(after.players, busy.players);
        assert_eq!(after.persist, busy.persist);
        assert_eq!(after.fixed, busy.fixed);
    }

    fn px_near(px: [u8; 4], v: u8) -> bool {
        px[..3].iter().all(|c| c.abs_diff(v) <= 3)
    }

    const PX_BLACK: &[u8; 4] = &[0, 0, 0, 255];
    const PX_WHITE: &[u8; 4] = &[255, 255, 255, 255];

    /// ToBlack dips through black: outgoing darkens, incoming rises from black.
    #[test]
    fn gpu_to_black_dips_through_black() {
        use vjc_core::transition::Kind::ToBlack;
        let at = |p| render_transition(ToBlack, p, (1, 1), PX_WHITE, PX_WHITE).map(|v| v[0]);
        let Some(q) = at(0.25) else { return };
        assert!(px_near(q, 128), "{q:?}");
        assert!(px_near(at(0.5).unwrap(), 0), "midpoint is black");
        assert!(px_near(at(1.0).unwrap(), 255), "ends on the incoming clip");
        // Negative control: a plain cross would not be black at the midpoint.
        let d = render_transition(
            vjc_core::transition::Kind::Dissolve,
            0.5,
            (1, 1),
            PX_WHITE,
            PX_WHITE,
        )
        .unwrap();
        assert!(px_near(d[0], 255), "{d:?}");
    }

    /// Push slides the incoming clip in; left and right are mirror images.
    #[test]
    fn gpu_push_left_and_right_are_mirrored() {
        use vjc_core::transition::Kind::{PushLeft, PushRight};
        let Some(l) = render_transition(PushLeft, 0.5, (4, 1), PX_BLACK, PX_WHITE) else {
            return;
        };
        let r = render_transition(PushRight, 0.5, (4, 1), PX_BLACK, PX_WHITE).unwrap();
        let v = |px: &[[u8; 4]]| px.iter().map(|p| p[0] > 128).collect::<Vec<_>>();
        assert_eq!(v(&l), [false, false, true, true]);
        assert_eq!(v(&r), [true, true, false, false]);
        // Start shows only the old clip, end only the new one.
        let s = render_transition(PushLeft, 0.0, (4, 1), PX_BLACK, PX_WHITE).unwrap();
        assert!(s.iter().all(|p| px_near(*p, 0)), "{s:?}");
        let e = render_transition(PushLeft, 1.0, (4, 1), PX_BLACK, PX_WHITE).unwrap();
        assert!(e.iter().all(|p| px_near(*p, 255)), "{e:?}");
    }

    /// ZoomIn cross-fades like the pure weights say; WipeEllipse opens from the centre.
    #[test]
    fn gpu_zoom_in_and_wipe_ellipse() {
        use vjc_core::transition::Kind::{WipeEllipse, ZoomIn};
        let Some(z) = render_transition(ZoomIn, 0.5, (1, 1), PX_BLACK, PX_WHITE) else {
            return;
        };
        let (_, wn) = vjc_core::transition::weights(ZoomIn, 0.5).unwrap();
        assert!(px_near(z[0], (wn * 255.0) as u8), "{z:?}");
        let w = render_transition(WipeEllipse, 0.5, (5, 5), PX_BLACK, PX_WHITE).unwrap();
        assert!(px_near(w[12], 255), "centre already new: {:?}", w[12]);
        assert!(px_near(w[0], 0), "corner still old: {:?}", w[0]);
        // Negative control: at progress 0 the centre is still the old clip.
        let w0 = render_transition(WipeEllipse, 0.0, (5, 5), PX_BLACK, PX_WHITE).unwrap();
        assert!(px_near(w0[12], 0), "{:?}", w0[12]);
    }

    /// New transition kinds: at progress 0 every pixel is the old clip, at 1 the new one,
    /// and halfway the frame is neither. Old is black and new is white.
    fn check_new_kind(kind: vjc_core::transition::Kind) {
        check_new_kind_sized(kind, (16, 4));
    }

    fn check_new_kind_sized(kind: vjc_core::transition::Kind, size: (u32, u32)) {
        let Some(start) = render_transition(kind, 0.0, size, PX_BLACK, PX_WHITE) else {
            return;
        };
        let end = render_transition(kind, 1.0, size, PX_BLACK, PX_WHITE).unwrap();
        let mid = render_transition(kind, 0.5, size, PX_BLACK, PX_WHITE).unwrap();
        assert!(
            start.iter().all(|p| px_near(*p, 0)),
            "{kind:?} t=0: {start:?}"
        );
        assert!(
            end.iter().all(|p| px_near(*p, 255)),
            "{kind:?} t=1: {end:?}"
        );
        assert_ne!(mid, start, "{kind:?} t=.5 still the old clip");
        assert_ne!(mid, end, "{kind:?} t=.5 already the new clip");
    }

    #[test]
    fn gpu_wipe_left() {
        check_new_kind(vjc_core::transition::Kind::WipeLeft);
    }
    #[test]
    fn gpu_wipe_right() {
        check_new_kind(vjc_core::transition::Kind::WipeRight);
    }
    #[test]
    fn gpu_wipe_up() {
        check_new_kind(vjc_core::transition::Kind::WipeUp);
    }
    #[test]
    fn gpu_wipe_down() {
        check_new_kind(vjc_core::transition::Kind::WipeDown);
    }
    #[test]
    fn gpu_push_up() {
        check_new_kind(vjc_core::transition::Kind::PushUp);
    }
    #[test]
    fn gpu_push_down() {
        check_new_kind(vjc_core::transition::Kind::PushDown);
    }
    #[test]
    fn gpu_zoom_out() {
        check_new_kind(vjc_core::transition::Kind::ZoomOut);
    }
    #[test]
    fn gpu_cube() {
        check_new_kind(vjc_core::transition::Kind::Cube);
    }
    #[test]
    fn gpu_pixelate() {
        check_new_kind(vjc_core::transition::Kind::Pixelate);
    }
    #[test]
    fn gpu_luma_wipe() {
        check_new_kind(vjc_core::transition::Kind::LumaWipe);
    }

    /// Wipes sweep in the named direction: the new clip shows on the side it comes from.
    #[test]
    fn gpu_wipe_directions_are_opposite() {
        use vjc_core::transition::Kind::{WipeLeft, WipeRight};
        let Some(l) = render_transition(WipeLeft, 0.5, (8, 1), PX_BLACK, PX_WHITE) else {
            return;
        };
        let r = render_transition(WipeRight, 0.5, (8, 1), PX_BLACK, PX_WHITE).unwrap();
        assert!(px_near(l[7], 255) && px_near(l[0], 0), "{l:?}");
        assert!(px_near(r[0], 255) && px_near(r[7], 0), "{r:?}");
    }

    /// Every kind added after LumaWipe: GPU pixels vs the CPU twin, for the incoming half
    /// (black old, white new) and the outgoing half (white old, black new). Skipped
    /// without an adapter.
    #[test]
    fn gpu_new_transition_kinds_match_cpu_twin() {
        use vjc_core::transition::Kind;
        use vjc_core::transition_fx::coverage;
        let size = (32u32, 32u32);
        let mut checked = 0;
        for kind in Kind::ALL
            .into_iter()
            .filter(|k| k.shader_id() >= 17 && !matches!(k, Kind::Random | Kind::ToWhite))
        {
            for p in [0.0f32, 0.3, 0.5, 0.7, 1.0] {
                let Some(new_half) = render_transition(kind, p, size, PX_BLACK, PX_WHITE) else {
                    return;
                };
                let old_half = render_transition(kind, p, size, PX_WHITE, PX_BLACK).unwrap();
                for (i, (a, b)) in new_half.iter().zip(&old_half).enumerate() {
                    let uv = (
                        ((i as u32 % size.0) as f32 + 0.5) / size.0 as f32,
                        ((i as u32 / size.0) as f32 + 0.5) / size.1 as f32,
                    );
                    let (o, n) = (coverage(kind, p, true, uv), coverage(kind, p, false, uv));
                    let (want_new, want_old) = (255.0 * n, 255.0 * o * (1.0 - n));
                    assert!(
                        (f32::from(a[0]) - want_new).abs() <= 6.0,
                        "{kind:?} p={p} new half px {i}: gpu {} cpu {want_new}",
                        a[0]
                    );
                    assert!(
                        (f32::from(b[0]) - want_old).abs() <= 6.0,
                        "{kind:?} p={p} old half px {i}: gpu {} cpu {want_old}",
                        b[0]
                    );
                }
                checked += 1;
            }
        }
        assert!(checked >= 24 * 5);
    }

    /// Every new kind is a real transition on the GPU: old at 0, new at 1, neither at .5.
    #[test]
    fn gpu_new_kinds_start_old_end_new() {
        use vjc_core::transition::Kind;
        for kind in Kind::ALL.into_iter().filter(|k| {
            // Flips squeeze the old picture to nothing: black-on-black looks the same
            // as the start there; the CPU-twin test covers them with both colours.
            k.shader_id() >= 17
                && !matches!(k, Kind::Random | Kind::ToWhite | Kind::FlipH | Kind::FlipV)
        }) {
            check_new_kind_sized(kind, (32, 32));
        }
    }

    /// ToWhite dips through white: a black picture reaches full white mid-way.
    #[test]
    fn gpu_to_white_dips_through_white() {
        use vjc_core::transition::Kind::ToWhite;
        let Some(mid) = render_transition(ToWhite, 0.5, (1, 1), PX_BLACK, PX_BLACK) else {
            return;
        };
        assert!(px_near(mid[0], 255), "{mid:?}");
        let q = render_transition(ToWhite, 0.25, (1, 1), PX_BLACK, PX_BLACK).unwrap();
        assert!(q[0][0].abs_diff(128) <= 3, "{q:?}");
        let e = render_transition(ToWhite, 0.75, (1, 1), PX_BLACK, PX_BLACK).unwrap();
        assert!(e[0][0].abs_diff(128) <= 3, "{e:?}");
        let done = render_transition(ToWhite, 1.0, (1, 1), PX_BLACK, PX_BLACK).unwrap();
        assert!(px_near(done[0], 0), "{done:?}");
    }

    /// Invert on one pixel through the full chain (translate, pipeline, ping-pong), for a
    /// layer effect and for a master effect. Skipped when there is no adapter.
    #[test]
    fn gpu_invert_one_pixel() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("invert").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 1, 1);
        comp.add_effect(&device, "invert", &wgsl).unwrap();
        let fx = EffectDraw {
            lut: None,
            blend: Default::default(),
            kind: "invert".into(),
            uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
            dry_wet: 1.0,
            ml: None,
            persistent: None,
            ticking: false,
        };
        let src = [51u8, 102, 204, 255];
        for on_master in [false, true] {
            let layer = [LayerDraw {
                id: uuid::Uuid::from_u128(1),
                opacity: 1.0,
                blend: BlendMode::Normal,
                fit: FitMode::Stretch,
                effects: if on_master { vec![] } else { vec![fx.clone()] },
                frame: Some((1, 1, &src, 1)),
                generator: None,
                transition: None,
                transform: Affine::IDENTITY,
                channels: Default::default(),
                mask: Default::default(),
            }];
            let master = if on_master { vec![fx.clone()] } else { vec![] };
            comp.render(&device, &queue, &layer, &master, 1.0);
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = device.create_command_encoder(&Default::default());
            enc.copy_texture_to_buffer(
                comp.target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(enc.finish()));
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
            let got: Vec<u8> = buf.slice(..).get_mapped_range().expect("map")[..4].to_vec();
            buf.unmap();
            for (g, w) in got.iter().zip([204u8, 153, 51]) {
                assert!(g.abs_diff(w) <= 2, "master={on_master}: {got:?}");
            }
        }
    }

    /// Solid Color generator on one pixel: params -> uniforms -> shader -> composition.
    /// Skipped when there is no adapter.
    #[test]
    fn gpu_solid_color_one_pixel() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("solid_color").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 1, 1);
        comp.add_effect(&device, "solid_color", &wgsl).unwrap();
        let mut params = isf.default_params();
        params[0].value = vjc_model::Value::Color([0.2, 0.4, 0.8, 1.0]);
        let layer = [LayerDraw {
            id: uuid::Uuid::from_u128(1),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: vec![],
            frame: None,
            generator: Some(EffectDraw {
                lut: None,
                blend: Default::default(),
                kind: "solid_color".into(),
                uniforms: isf.pack(&params, 0.0, (1, 1)),
                dry_wet: 1.0,
                ml: None,
                persistent: None,
                ticking: false,
            }),
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        }];
        comp.render(&device, &queue, &layer, &[], 1.0);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let got: Vec<u8> = buf.slice(..).get_mapped_range().expect("map")[..4].to_vec();
        buf.unmap();
        for (g, w) in got.iter().zip([51u8, 102, 204, 255]) {
            assert!(g.abs_diff(w) <= 2, "{got:?}");
        }
    }

    /// Renders `spectrum_bars` on the GPU and returns the RGBA bytes of a 64x36 picture.
    fn render_spectrum(audio: Option<&[f32]>) -> Option<Vec<u8>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let (w, h) = (64u32, 36u32);
        let (isf, wgsl) =
            vjc_fx::compile(vjc_fx::builtin_source("spectrum_bars").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, w, h);
        comp.add_effect(&device, "spectrum_bars", &wgsl).unwrap();
        // A fake FFT: the lower half of the bins is full, the upper half silent.
        match audio {
            Some(spec) => comp.set_audio(&queue, spec, &[0.5; 8], true),
            None => comp.set_audio(&queue, &[], &[], false),
        }
        let layer = [LayerDraw {
            id: uuid::Uuid::from_u128(1),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: vec![],
            frame: None,
            generator: Some(EffectDraw {
                lut: None,
                blend: Default::default(),
                kind: "spectrum_bars".into(),
                uniforms: isf.pack(&isf.default_params(), 0.0, (w, h)),
                dry_wet: 1.0,
                ml: None,
                persistent: None,
                ticking: false,
            }),
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        }];
        comp.render(&device, &queue, &layer, &[], 1.0);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let got = buf.slice(..).get_mapped_range().expect("map").to_vec();
        buf.unmap();
        Some(got)
    }

    /// Pixels of picture column `x` clearly brighter than the dark background, in a 64x36
    /// RGBA readback.
    fn lit_in_column(px: &[u8], x: usize) -> usize {
        (0..36)
            .filter(|y| {
                let p = &px[y * 256 + x * 4..][..3];
                p.iter().map(|c| u32::from(*c)).sum::<u32>() > 150
            })
            .count()
    }

    /// ISF `audioFFT` end to end: a fake spectrum drives the bars, no audio keeps the
    /// synthetic fallback. Skipped when there is no adapter.
    #[test]
    fn gpu_spectrum_bars_follow_the_fake_fft() {
        let mut spec = vec![1.0f32; 64];
        spec[32..].fill(0.0);
        let Some(px) = render_spectrum(Some(&spec)) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        // Bar 8 (x 17) sits in the loud half, bar 24 (x 49) in the silent one.
        assert!(
            lit_in_column(&px, 17) >= 30,
            "loud bar {}",
            lit_in_column(&px, 17)
        );
        assert!(
            lit_in_column(&px, 49) <= 3,
            "silent bar {}",
            lit_in_column(&px, 49)
        );
        // No audio: the synthetic levels light both.
        let px = render_spectrum(None).unwrap();
        assert!(lit_in_column(&px, 17) >= 2 && lit_in_column(&px, 49) >= 2);
    }

    /// Master fade on the GPU: opacity 0.5 halves the colour towards black, bypass (opacity 0)
    /// is pure black. Writes PPMs to `target/ui-snapshots/`. Skipped when there is no adapter.
    #[test]
    fn gpu_master_opacity_and_bypass_fade_to_black() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        // 64 px x 4 bytes = 256: rows are already copy-aligned.
        let (w, h) = (64u32, 36u32);
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("solid_color").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, w, h);
        comp.add_effect(&device, "solid_color", &wgsl).unwrap();
        let mut params = isf.default_params();
        params[0].value = vjc_model::Value::Color([0.2, 0.4, 0.8, 1.0]);
        let layer = [LayerDraw {
            id: uuid::Uuid::from_u128(1),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: vec![],
            frame: None,
            generator: Some(EffectDraw {
                lut: None,
                blend: Default::default(),
                kind: "solid_color".into(),
                uniforms: isf.pack(&params, 0.0, (w, h)),
                dry_wet: 1.0,
                ml: None,
                persistent: None,
                ticking: false,
            }),
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        }];
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        // (name, master opacity as the app passes it, expected pixel)
        let cases: [(&str, f32, [u8; 3]); 3] = [
            ("master-opacity-1", 1.0, [51, 102, 204]),
            ("master-opacity-0.5", 0.5, [26, 51, 102]),
            ("master-bypass", 0.0, [0, 0, 0]),
        ];
        for (name, opacity, want) in cases {
            comp.render(&device, &queue, &layer, &[], opacity);
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (256 * h) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = device.create_command_encoder(&Default::default());
            enc.copy_texture_to_buffer(
                comp.target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(enc.finish()));
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
            let data = buf.slice(..).get_mapped_range().expect("map").to_vec();
            buf.unmap();
            let mid = ((h / 2) * 256 + (w / 2) * 4) as usize;
            for (g, wnt) in data[mid..mid + 3].iter().zip(want) {
                assert!(g.abs_diff(wnt) <= 2, "{name}: {:?}", &data[mid..mid + 4]);
            }
            // Same everywhere: the fade covers the whole frame.
            assert!(
                data.chunks(4).all(|p| p[..3] == data[mid..mid + 3]),
                "{name}"
            );
            // Binary PPM, for eyeballing (no image crate here); the asserts above are the check.
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in data.chunks(4).take((w * h) as usize) {
                ppm.extend_from_slice(&p[..3]);
            }
            std::fs::write(dir.join(format!("{name}.ppm")), ppm).unwrap();
        }
    }

    /// Renders `layer` on `comp` and returns the 64 x 36 RGBA target.
    fn read_target(device: &wgpu::Device, queue: &wgpu::Queue, comp: &Compositor) -> Vec<u8> {
        let (w, h) = (64u32, 36u32);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (256 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = buf.slice(..).get_mapped_range().expect("map").to_vec();
        buf.unmap();
        data
    }

    /// Effects whose output on a flat frame is legitimately partly black.
    const PARTIAL_BLACK_OK: &[&str] = &["vignette", "crt"];

    #[test]
    fn partial_black_ok_lists_only_bundled_effects() {
        for id in PARTIAL_BLACK_OK {
            assert!(
                vjc_fx::BUILTIN.iter().any(|(b, _)| b == id),
                "{id} is not a bundled effect"
            );
        }
    }

    #[test]
    fn persist_key_does_not_collide_where_xor_does() {
        let (a, b) = (uuid::Uuid::from_u128(0b01), uuid::Uuid::from_u128(0b10));
        // a ^ 2 == b ^ 1 == 3: the old key was the same for both pairs.
        assert_ne!(persist_key(a, 2), persist_key(b, 1));
        assert_eq!(persist_key(a, 2), persist_key(a, 2), "stable");
        assert_ne!(persist_key(a, 0), persist_key(a, 1));
    }

    /// Every bundled effect and generator renders with default params to something that is
    /// not entirely black (a NaN/inf in the shader ends up black in the Unorm target).
    /// `MAY_BE_DARK` lists the ones where black is a legitimate result. Skipped without
    /// an adapter.
    #[test]
    fn gpu_every_bundled_effect_and_generator_renders_visible_output() {
        const MAY_BE_DARK: &[&str] = &[];
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        let (w, h) = (64u32, 36u32);
        // Test frame: colour ramps, a bright and a dark block, a green patch (for keying).
        let mut frame = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                let mut rgb = [fx, fy, 1.0 - fx];
                if x < 8 && y < 8 {
                    rgb = [0.02; 3];
                }
                if x >= w - 10 && y < 10 {
                    rgb = [0.95; 3];
                }
                if (24..40).contains(&x) && (14..24).contains(&y) {
                    rgb = [0.1, 0.9, 0.15];
                }
                for c in 0..3 {
                    frame[i + c] = (rgb[c] * 255.0) as u8;
                }
            }
        }
        let mut comp = Compositor::new(&device, w, h);
        let mut bad = vec![];
        let mut count = 0;
        for (generator, list) in [(false, vjc_fx::BUILTIN), (true, vjc_fx::GENERATORS)] {
            for (id, _) in list {
                count += 1;
                let (isf, wgsl) = match vjc_fx::compile(vjc_fx::builtin_source(id).unwrap()) {
                    Ok(v) => v,
                    Err(e) => {
                        bad.push(format!("{id}: translate: {e}"));
                        continue;
                    }
                };
                if let Err(e) = comp.add_effect_passes(&device, id, &wgsl, &isf.passes) {
                    bad.push(format!("{id}: GPU compile: {e}"));
                    continue;
                }
                let draw = EffectDraw {
                    lut: None,
                    blend: Default::default(),
                    kind: id.to_string(),
                    uniforms: isf.pack(&isf.default_params(), 0.1, (w, h)),
                    dry_wet: 1.0,
                    ml: None,
                    persistent: isf.needs_instance().then(|| uuid::Uuid::from_u128(77)),
                    ticking: false,
                };
                let (effects, gen_draw, fr) = if generator {
                    (vec![], Some(draw), None)
                } else {
                    (vec![draw], None, Some((w, h, &frame[..], 1)))
                };
                let layer = [LayerDraw {
                    id: uuid::Uuid::from_u128(2),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects,
                    frame: fr,
                    generator: gen_draw,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                }];
                comp.render(&device, &queue, &layer, &[], 1.0);
                let data = read_target(&device, &queue, &comp);
                let lit = data
                    .chunks(4)
                    .filter(|p| p[..3].iter().any(|v| *v > 2))
                    .count();
                if lit == 0 && !MAY_BE_DARK.contains(id) {
                    bad.push(format!("{id}: output is entirely black"));
                }
                // Partial NaN/inf: a few exactly-black pixels among lit ones. Only checked
                // for effects fed a flat bright frame, where the output should be smooth.
                if !generator && lit > 0 {
                    let flat: Vec<u8> = [150u8, 130, 180, 255].repeat((w * h) as usize);
                    let mut l = layer;
                    l[0].frame = Some((w, h, &flat[..], 2));
                    comp.render(&device, &queue, &l, &[], 1.0);
                    let d = read_target(&device, &queue, &comp);
                    let black = d
                        .chunks(4)
                        .filter(|p| p[..3].iter().all(|v| *v == 0))
                        .count();
                    let total = d.len() / 4;
                    if black > 0 && black * 2 < total && !PARTIAL_BLACK_OK.contains(id) {
                        bad.push(format!("{id}: {black}/{total} pixels black (partial NaN?)"));
                    }
                }
            }
        }
        assert_eq!(count, vjc_fx::BUILTIN.len() + vjc_fx::GENERATORS.len());
        assert!(bad.is_empty(), "failed:\n{}", bad.join("\n"));
    }

    #[test]
    fn mix_plan_cases() {
        assert_eq!(mix_plan(0.0, 0), MixPlan::Skip);
        assert_eq!(mix_plan(-1.0, 0), MixPlan::Skip);
        assert_eq!(mix_plan(f32::NAN, 0), MixPlan::Skip);
        assert_eq!(mix_plan(1.0, 0), MixPlan::Wet);
        assert_eq!(mix_plan(2.0, 0), MixPlan::Wet);
        assert_eq!(mix_plan(0.5, 0), MixPlan::Blend(0.5));
        // A blend other than Normal needs the mix pass even at full strength.
        assert_eq!(mix_plan(1.0, 2), MixPlan::Blend(1.0));
        assert_eq!(mix_plan(0.0, 2), MixPlan::Skip);
    }

    /// Invert on a white pixel at several dry/wet values, in a two-effect chain too.
    /// Skipped when there is no adapter.
    #[test]
    fn gpu_dry_wet_mixes_effect_with_input() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("invert").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 1, 1);
        comp.add_effect(&device, "invert", &wgsl).unwrap();
        let fx = |dry_wet| EffectDraw {
            lut: None,
            blend: Default::default(),
            kind: "invert".into(),
            uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
            dry_wet,
            ml: None,
            persistent: None,
            ticking: false,
        };
        let src = [255u8, 255, 255, 255];
        // (chain, expected red)
        let cases: [(Vec<EffectDraw>, u8); 5] = [
            (vec![fx(0.5)], 128),
            (vec![fx(1.0)], 0),
            (vec![fx(0.0)], 255),
            (vec![fx(0.25)], 191),
            // 0.5 -> 128 grey; second invert at 0.5 -> mix(128, 127, 0.5) = ~128
            (vec![fx(0.5), fx(0.5)], 128),
        ];
        for (effects, want) in cases {
            let layer = [LayerDraw {
                id: uuid::Uuid::from_u128(1),
                opacity: 1.0,
                blend: BlendMode::Normal,
                fit: FitMode::Stretch,
                effects,
                frame: Some((1, 1, &src, 1)),
                generator: None,
                transition: None,
                transform: Affine::IDENTITY,
                channels: Default::default(),
                mask: Default::default(),
            }];
            comp.render(&device, &queue, &layer, &[], 1.0);
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = device.create_command_encoder(&Default::default());
            enc.copy_texture_to_buffer(
                comp.target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(enc.finish()));
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
            let got: Vec<u8> = buf.slice(..).get_mapped_range().expect("map")[..4].to_vec();
            buf.unmap();
            for g in &got[..3] {
                assert!(g.abs_diff(want) <= 3, "want {want}, got {got:?}");
            }
        }
    }

    /// Renders a 1x1 `src` pixel through `effects` and reads it back.
    fn render_pixel(
        comp: &mut Compositor,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        effects: Vec<EffectDraw>,
        src: [u8; 4],
    ) -> Vec<u8> {
        // A new frame version each call, so the source is uploaded again.
        static VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let version = VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let layer = [LayerDraw {
            id: uuid::Uuid::from_u128(1),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects,
            frame: Some((1, 1, &src, version)),
            generator: None,
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        }];
        comp.render(device, queue, &layer, &[], 1.0);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let got = buf.slice(..).get_mapped_range().expect("map")[..4].to_vec();
        buf.unmap();
        got
    }

    /// `read_rgba` returns the tight pixels of the rendered output. Skipped without a GPU.
    #[test]
    fn gpu_read_rgba_returns_the_rendered_pixels() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 3, 2);
        let px: Vec<u8> = (0..3 * 2).flat_map(|i| [i * 40, 100, 200, 255]).collect();
        let layer = [LayerDraw {
            id: uuid::Uuid::from_u128(1),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: vec![],
            frame: Some((3, 2, &px, 1)),
            generator: None,
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        }];
        comp.render(&device, &queue, &layer, &[], 1.0);
        let (w, h, got) = comp.read_rgba(&device, &queue).unwrap();
        assert_eq!((w, h, got.len()), (3, 2, 24));
        for (g, want) in got.iter().zip(&px) {
            assert!(g.abs_diff(*want) <= 2, "{got:?} vs {px:?}");
        }
    }

    /// Invert on mid grey with each effect blend: matches the CPU twin. Skipped without a GPU.
    #[test]
    fn gpu_effect_blend_matches_cpu_twin() {
        let Some((device, queue)) = gpu() else { return };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("invert").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 1, 1);
        comp.add_effect(&device, "invert", &wgsl).unwrap();
        let src = [128u8, 128, 128, 255];
        for mode in vjc_core::blend::FX_MIX_MODES.iter() {
            for dry_wet in [1.0f32, 0.5] {
                let fx = EffectDraw {
                    lut: None,
                    blend: vjc_core::blend::fx_mix_id(mode),
                    kind: "invert".into(),
                    uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
                    dry_wet,
                    ml: None,
                    persistent: None,
                    ticking: false,
                };
                let got = render_pixel(&mut comp, &device, &queue, vec![fx], src);
                let d = 128.0 / 255.0;
                let want = vjc_core::blend::fx_mix_pixel(
                    mode,
                    [d, d, d, 1.0],
                    [1.0 - d, 1.0 - d, 1.0 - d, 1.0],
                    dry_wet,
                )[0] * 255.0;
                assert!(
                    (f32::from(got[0]) - want).abs() <= 3.0,
                    "{mode:?} @{dry_wet}: got {got:?}, want {want}"
                );
            }
        }
    }

    /// An identity LUT leaves colours alone; a channel-swap LUT swaps them; strength 0 is
    /// passthrough. Skipped without a GPU.
    #[test]
    fn gpu_lut3d_identity_is_passthrough_and_swap_swaps() {
        let Some((device, queue)) = gpu() else { return };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("lut3d").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 1, 1);
        comp.add_effect(&device, "lut3d", &wgsl).unwrap();
        let fx = |lut: &std::sync::Arc<vjc_fx::lut::Lut>, strength: f64| {
            let mut params = isf.default_params();
            for p in &mut params {
                if p.name == "strength" {
                    p.value = vjc_model::Value::Number(strength);
                }
            }
            EffectDraw {
                blend: 0,
                lut: Some(lut.clone()),
                kind: "lut3d".into(),
                uniforms: isf.pack(&params, 0.0, (1, 1)),
                dry_wet: 1.0,
                ml: None,
                persistent: None,
                ticking: false,
            }
        };
        let id = std::sync::Arc::new(vjc_fx::lut::Lut::identity(17));
        let mut swap = vjc_fx::lut::Lut::identity(17);
        for c in &mut swap.data {
            c.swap(0, 2);
        }
        let swap = std::sync::Arc::new(swap);
        for src in [[200u8, 100, 30, 255], [0, 255, 128, 255], [17, 17, 17, 255]] {
            let got = render_pixel(&mut comp, &device, &queue, vec![fx(&id, 1.0)], src);
            for k in 0..3 {
                assert!(got[k].abs_diff(src[k]) <= 3, "identity {src:?} -> {got:?}");
            }
            let got = render_pixel(&mut comp, &device, &queue, vec![fx(&swap, 1.0)], src);
            assert!(
                got[0].abs_diff(src[2]) <= 3 && got[2].abs_diff(src[0]) <= 3,
                "swap {src:?} -> {got:?}"
            );
            let got = render_pixel(&mut comp, &device, &queue, vec![fx(&swap, 0.0)], src);
            assert!(
                got[0].abs_diff(src[0]) <= 3,
                "strength 0 {src:?} -> {got:?}"
            );
        }
    }

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return None;
        };
        let Ok(dq) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return None;
        };
        Some(dq)
    }

    /// Trails keep the previous output of the same effect instance: a bright frame
    /// followed by a black one still shows the decayed trail. Negative control: without an
    /// instance id (ordinary effect path) the second frame is black. Skipped without a GPU.
    #[test]
    fn gpu_persistent_effect_keeps_previous_output_per_instance() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("trails").unwrap()).unwrap();
        assert!(isf.persistent);
        let run = |instance_id: Option<uuid::Uuid>| {
            let mut comp = Compositor::new(&device, 1, 1);
            comp.add_effect(&device, "trails", &wgsl).unwrap();
            let fx = EffectDraw {
                lut: None,
                blend: Default::default(),
                kind: "trails".into(),
                uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
                dry_wet: 1.0,
                ml: None,
                persistent: instance_id,
                ticking: false,
            };
            let mut last = [0u8; 4];
            for (serial, px) in [[255u8; 4], [0, 0, 0, 255]].iter().enumerate() {
                let layer = [LayerDraw {
                    id: uuid::Uuid::from_u128(1),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects: vec![fx.clone()],
                    frame: Some((1, 1, px, serial as u64 + 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                }];
                comp.render(&device, &queue, &layer, &[], 1.0);
                last = read_px(&device, &queue, &comp);
            }
            last
        };
        let kept = run(Some(uuid::Uuid::from_u128(7)));
        assert!(kept[0].abs_diff(217) <= 3, "{kept:?}");
        assert_eq!(run(None)[0], 0);
    }

    /// A persistent effect drawn in the composition and again in a routed output keeps one
    /// history per output: both show the same decayed trail, and the buffers are freed
    /// once the routes go away. Skipped without a GPU.
    #[test]
    fn gpu_persistent_effect_has_own_history_per_route_and_frees_all() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return;
        };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("trails").unwrap()).unwrap();
        let lid = uuid::Uuid::from_u128(1);
        let fx = EffectDraw {
            lut: None,
            blend: Default::default(),
            kind: "trails".into(),
            uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
            dry_wet: 1.0,
            ml: None,
            persistent: Some(uuid::Uuid::from_u128(7)),
            ticking: false,
        };
        let mut comp = Compositor::new(&device, 1, 1);
        comp.add_effect(&device, "trails", &wgsl).unwrap();
        let routes = [SliceSource::Layer(lid)];
        let (mut main_px, mut routed_px) = ([0u8; 4], [0u8; 4]);
        for (serial, px) in [[255u8; 4], [0, 0, 0, 255]].iter().enumerate() {
            let layer = [LayerDraw {
                id: lid,
                opacity: 1.0,
                blend: BlendMode::Normal,
                fit: FitMode::Stretch,
                effects: vec![fx.clone()],
                frame: Some((1, 1, px, serial as u64 + 1)),
                generator: None,
                transition: None,
                transform: Affine::IDENTITY,
                channels: Default::default(),
                mask: Default::default(),
            }];
            comp.render_routed(&device, &queue, (&layer, &[]), (&[], 1.0), &routes);
            main_px = read_px(&device, &queue, &comp);
            let v = comp.routed.get(&routes[0]).unwrap();
            let t = std::mem::replace(&mut comp.target, v.0.clone());
            routed_px = read_px(&device, &queue, &comp);
            comp.target = t;
        }
        assert!(main_px[0].abs_diff(217) <= 3, "{main_px:?}");
        assert!(routed_px[0].abs_diff(217) <= 3, "{routed_px:?}");
        assert_eq!(comp.fx.persist.len(), 2);
        comp.render_routed(&device, &queue, (&[], &[]), (&[], 1.0), &[]);
        assert_eq!(comp.fx.persist.len(), 0, "histories freed with the layer");
    }

    fn layer<'a>(
        id: u128,
        opacity: f32,
        blend: BlendMode,
        effects: Vec<EffectDraw>,
        data: &'a [u8],
        serial: u64,
    ) -> LayerDraw<'a> {
        LayerDraw {
            id: uuid::Uuid::from_u128(id),
            opacity,
            blend,
            fit: FitMode::Stretch,
            effects,
            frame: Some((16, 16, data, serial)),
            generator: None,
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        }
    }

    /// Whole 16x16 composition as RGBA bytes.
    fn read16(device: &wgpu::Device, queue: &wgpu::Queue, comp: &Compositor) -> Vec<u8> {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(16),
                },
            },
            wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let raw = buf.slice(..).get_mapped_range().expect("map").to_vec();
        buf.unmap();
        (0..16)
            .flat_map(|y| raw[y * 256..y * 256 + 64].to_vec())
            .collect()
    }

    fn px_at(img: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (y * 16 + x) * 4;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    /// One 16x16 frame through a bundled effect with the given param overrides.
    #[allow(clippy::too_many_arguments)]
    fn run_fx(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        comp: &mut Compositor,
        kind: &str,
        set: &[(&str, vjc_model::Value)],
        time: f32,
        persistent: Option<uuid::Uuid>,
        data: &[u8],
        serial: u64,
    ) -> Vec<u8> {
        let (isf, _) = vjc_fx::compile(vjc_fx::builtin_source(kind).unwrap()).unwrap();
        let mut params = isf.default_params();
        for (n, v) in set {
            params.iter_mut().find(|p| p.name == *n).unwrap().value = v.clone();
        }
        let fx = EffectDraw {
            lut: None,
            blend: Default::default(),
            kind: kind.into(),
            uniforms: isf.pack(&params, time, (16, 16)),
            dry_wet: 1.0,
            ml: None,
            persistent,
            ticking: isf.tick_input.is_some(),
        };
        let l = [layer(1, 1.0, BlendMode::Normal, vec![fx], data, serial)];
        comp.render(device, queue, &l, &[], 1.0);
        read16(device, queue, comp)
    }

    fn fx_comp(device: &wgpu::Device, kind: &str) -> Compositor {
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source(kind).unwrap()).unwrap();
        let mut comp = Compositor::new(device, 16, 16);
        comp.add_effect_passes(device, kind, &wgsl, &isf.passes)
            .unwrap();
        comp
    }

    /// Two-pass separable blur against a CPU reference (9-tap box in x, then in y, taps one
    /// pixel apart at radius 4). Interior pixels only: the edges depend on the sampler's
    /// address mode. Skipped without a GPU.
    #[test]
    fn gpu_two_pass_blur_matches_cpu_reference() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = fx_comp(&device, "blur_separable");
        let f = |x: usize, y: usize, c: usize| ((x * 37 + y * 91 + c * 53 + x * y * 7) % 256) as u8;
        let mut data = vec![255u8; 16 * 16 * 4];
        for y in 0..16 {
            for x in 0..16 {
                for c in 0..3 {
                    data[(y * 16 + x) * 4 + c] = f(x, y, c);
                }
            }
        }
        let img = run_fx(
            &device,
            &queue,
            &mut comp,
            "blur_separable",
            &[("radius", vjc_model::Value::Number(4.0))],
            0.0,
            None,
            &data,
            1,
        );
        let mut worst = 0i32;
        for y in 4..12 {
            for x in 4..12 {
                for c in 0..3 {
                    // Horizontal pass, stored as 8-bit, then vertical.
                    let h = |yy: usize| {
                        let s: f32 = (-4i32..=4)
                            .map(|d| f((x as i32 + d) as usize, yy, c) as f32)
                            .sum();
                        (s / 9.0).round()
                    };
                    let v: f32 = (-4i32..=4).map(|d| h((y as i32 + d) as usize)).sum::<f32>() / 9.0;
                    let got = px_at(&img, x, y)[c] as i32;
                    worst = worst.max((got - v.round() as i32).abs());
                }
            }
        }
        assert!(worst <= 4, "worst deviation {worst}");
        // Negative control: the blur changed the picture.
        assert!((0..3).any(|c| px_at(&img, 8, 8)[c] != f(8, 8, c)));
    }

    /// A persistent PASSES target keeps its content across frames, is separate per effect
    /// instance, and is reset once the instance stops being drawn (retrigger). Skipped
    /// without a GPU.
    #[test]
    fn gpu_persistent_target_keeps_state_and_resets() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = fx_comp(&device, "feedback_trail");
        let (white, black) = (vec![255u8; 16 * 16 * 4], {
            let mut b = vec![0u8; 16 * 16 * 4];
            b.chunks_mut(4).for_each(|p| p[3] = 255);
            b
        });
        let id = Some(uuid::Uuid::from_u128(5));
        let go = |comp: &mut Compositor, id, data: &[u8], n| {
            px_at(
                &run_fx(
                    &device,
                    &queue,
                    comp,
                    "feedback_trail",
                    &[],
                    0.0,
                    id,
                    data,
                    n,
                ),
                8,
                8,
            )[0]
        };
        assert_eq!(go(&mut comp, id, &white, 1), 255);
        // 255 * 0.85 - 1 = 215.75
        let kept = go(&mut comp, id, &black, 2);
        assert!(kept.abs_diff(216) <= 3, "{kept}");
        let kept2 = go(&mut comp, id, &black, 3);
        assert!(kept2 < kept && kept2 > 150, "{kept2}");
        // Another instance does not see that history.
        let other = Some(uuid::Uuid::from_u128(6));
        assert_eq!(go(&mut comp, other, &black, 4), 0);
        // Not drawn for a frame: buffers are freed, same id starts from black again.
        comp.render(&device, &queue, &[], &[], 1.0);
        assert_eq!(comp.fx.targets.len(), 0);
        assert_eq!(go(&mut comp, id, &black, 5), 0);
    }

    /// Every blend added after the original 19 (except Displace/LoRez, tested apart)
    /// against the CPU formula, for two colour pairs. Negative control: the CPU result of
    /// Normal must differ from the GPU one, so a shader that fell back to Normal fails.
    #[test]
    fn gpu_new_blends_match_reference() {
        let Some((device, queue)) = gpu() else { return };
        let pairs = [
            ([204u8, 128, 51, 255], [102u8, 128, 230, 255]),
            ([30u8, 200, 120, 255], [220u8, 60, 90, 255]),
        ];
        let f = |c: [u8; 4]| c.map(|v| v as f32 / 255.0);
        let mut comp = Compositor::new(&device, 16, 16);
        for (mode, name) in &vjc_core::blend::COMPOSITION_MODES[19..] {
            // Jitter reads other texels: tested apart (a flat colour can't tell it from Normal).
            if matches!(
                mode,
                BlendMode::Displace | BlendMode::LoRez | BlendMode::Jitter
            ) {
                continue;
            }
            for (n, (base, src)) in pairs.iter().enumerate() {
                let (bd, sd) = (base.repeat(256), src.repeat(256));
                let ls = [
                    layer(1, 1.0, BlendMode::Normal, vec![], &bd, 1 + n as u64),
                    layer(2, 0.7, mode.clone(), vec![], &sd, 1 + n as u64),
                ];
                comp.render(&device, &queue, &ls, &[], 1.0);
                let got = px_at(&read16(&device, &queue, &comp), 5, 5);
                let want = vjc_core::blend::blend_pixel(mode, f(*base), f(*src), 0.7);
                let normal =
                    vjc_core::blend::blend_pixel(&BlendMode::Normal, f(*base), f(*src), 0.7);
                let mut differs = false;
                for i in 0..3 {
                    let g = got[i] as f32 / 255.0;
                    assert!((g - want[i]).abs() < 0.02, "{name}: {got:?} vs {want:?}");
                    differs |= (g - normal[i]).abs() > 0.02;
                }
                assert!(differs, "{name}: indistinguishable from Normal, weak case");
            }
        }
    }

    /// Displace reads the base shifted by the source; LoRez reads the source per 8x8 block.
    /// Controls: a neutral source (no shift) and plain Normal give the unshifted pixel.
    #[test]
    fn gpu_displace_and_lorez() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 16, 16);
        // Base: left half red, right half blue.
        let base: Vec<u8> = (0..256)
            .flat_map(|i| {
                if i % 16 < 8 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                }
            })
            .collect();
        let shifted = [255u8, 128, 128, 255].repeat(256);
        let neutral = [128u8, 128, 128, 255].repeat(256);
        let draw = |comp: &mut Compositor, mode: BlendMode, src: &[u8], s: u64| {
            let ls = [
                layer(1, 1.0, BlendMode::Normal, vec![], &base, 1),
                layer(2, 1.0, mode, vec![], src, s),
            ];
            comp.render(&device, &queue, &ls, &[], 1.0);
            read16(&device, &queue, comp)
        };
        let off = vjc_core::blend::displace_offset([1.0, 0.5, 0.5, 1.0]);
        assert!(off[0] > 8);
        let img = draw(&mut comp, BlendMode::Displace, &shifted, 1);
        assert_eq!(
            px_at(&img, 3, 8)[2],
            255,
            "red side reads blue base: {img:?}"
        );
        let img = draw(&mut comp, BlendMode::Displace, &neutral, 2);
        assert_eq!(px_at(&img, 3, 8)[0], 255);
        assert_eq!(px_at(&img, 3, 8)[2], 0);
        // LoRez: source red = 16 * x, so block 1 (x 8..16) repeats the value of x = 8.
        let ramp: Vec<u8> = (0..256)
            .flat_map(|i| [(i % 16 * 16) as u8, 0, 0, 255])
            .collect();
        let img = draw(&mut comp, BlendMode::LoRez, &ramp, 3);
        assert_eq!(px_at(&img, 11, 4)[0], 128, "x=11 reads x=8");
        assert_eq!(px_at(&img, 3, 4)[0], 0, "x=3 reads x=0");
        let img = draw(&mut comp, BlendMode::Normal, &ramp, 4);
        assert_eq!(px_at(&img, 11, 4)[0], 176, "control: Normal keeps x=11");
    }

    /// Jitter against its CPU twin on a ramp source. Control: Normal keeps the pixel's own
    /// texel, so a shader that fell back to Normal fails.
    #[test]
    fn gpu_jitter_matches_reference() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 16, 16);
        let base = [0u8, 0, 0, 255].repeat(256);
        let ramp: Vec<u8> = (0..256)
            .flat_map(|i| [(i % 16 * 16) as u8, (i / 16 * 16) as u8, 0, 255])
            .collect();
        let mut draw = |mode: BlendMode, s: u64| {
            let ls = [
                layer(1, 1.0, BlendMode::Normal, vec![], &base, 1),
                layer(2, 1.0, mode, vec![], &ramp, s),
            ];
            comp.render(&device, &queue, &ls, &[], 1.0);
            read16(&device, &queue, &comp)
        };
        let img = draw(BlendMode::Jitter, 1);
        let mut moved = 0;
        for y in 0..16u32 {
            for x in 0..16u32 {
                let o = vjc_core::blend::jitter_offset(x, y);
                let sx = (x as i32 + o[0]).clamp(0, 15) as u8;
                let sy = (y as i32 + o[1]).clamp(0, 15) as u8;
                let got = px_at(&img, x as usize, y as usize);
                assert_eq!(got[..2], [sx * 16, sy * 16], "pixel {x},{y}");
                moved += usize::from(o != [0, 0]);
            }
        }
        assert!(moved > 100);
        let img = draw(BlendMode::Normal, 2);
        assert_eq!(
            px_at(&img, 5, 5)[..2],
            [80, 80],
            "control: Normal is unshifted"
        );
    }

    /// Crop clears everything outside the box. Control: zero crop leaves the picture.
    #[test]
    fn gpu_crop_zeroes_outside_area() {
        use vjc_model::Value::Number;
        let Some((device, queue)) = gpu() else { return };
        let white = [255u8; 4].repeat(256);
        let mut comp = fx_comp(&device, "crop");
        let set = [("left", Number(0.25)), ("top", Number(0.5))];
        let img = run_fx(
            &device, &queue, &mut comp, "crop", &set, 0.0, None, &white, 1,
        );
        assert_eq!(px_at(&img, 1, 12)[..3], [0, 0, 0], "left strip");
        assert_eq!(px_at(&img, 8, 2)[..3], [0, 0, 0], "top half");
        assert_eq!(px_at(&img, 8, 12), [255; 4], "inside stays");
        let img = run_fx(
            &device,
            &queue,
            &mut comp,
            "crop",
            &[],
            0.0,
            None,
            &white,
            2,
        );
        assert_eq!(px_at(&img, 1, 2), [255; 4], "control: no crop");
    }

    /// Keystone with the corners on the pattern's own corners is identity. Control: moving
    /// one corner changes the picture.
    #[test]
    fn gpu_keystone_default_is_identity() {
        use vjc_model::Value::Color;
        let Some((device, queue)) = gpu() else { return };
        let ramp: Vec<u8> = (0..256)
            .flat_map(|i| [(i % 16 * 16) as u8, (i / 16 * 16) as u8, 90, 255])
            .collect();
        let mut comp = fx_comp(&device, "keystone");
        let img = run_fx(
            &device,
            &queue,
            &mut comp,
            "keystone",
            &[],
            0.0,
            None,
            &ramp,
            1,
        );
        for (g, w) in img.iter().zip(&ramp) {
            assert!(g.abs_diff(*w) <= 2, "identity broke: {g} vs {w}");
        }
        let set = [("top_right", Color([0.5, 1.0, 0.0, 1.0]))];
        let img = run_fx(
            &device, &queue, &mut comp, "keystone", &set, 0.0, None, &ramp, 2,
        );
        assert_ne!(px_at(&img, 14, 8), px_at(&ramp, 14, 8));
        assert_eq!(
            px_at(&img, 14, 1)[..3],
            [0, 0, 0],
            "right of the moved edge is empty"
        );
    }

    /// Freeze keeps its frame inside a tick and while held; a new tick without hold grabs
    /// the new one, however many ticks TIME skipped (an even jump left the old parity
    /// check blind). Each case has its control. The kept alpha is the input's own.
    #[test]
    fn gpu_freeze_grabs_on_any_tick_change_and_keeps_alpha() {
        use vjc_model::Value::{Bool, Number};
        let Some((device, queue)) = gpu() else { return };
        let (white, black) = ([255u8; 4].repeat(256), [0, 0, 0, 255].repeat(256));
        let half = [10u8, 20, 30, 128].repeat(256);
        let id = Some(uuid::Uuid::from_u128(5));
        let fps = ("fps", Number(1.0));
        let run =
            |comp: &mut Compositor, set: &[(&str, vjc_model::Value)], t: f32, d: &[u8], s: u64| {
                px_at(
                    &run_fx(&device, &queue, comp, "freeze", set, t, id, d, s),
                    4,
                    4,
                )
            };
        let mut comp = fx_comp(&device, "freeze");
        // First draw grabs.
        assert_eq!(
            run(&mut comp, std::slice::from_ref(&fps), 0.1, &white, 1)[0],
            255
        );
        // Same tick: the black frame is not grabbed (control for the jumps below).
        assert_eq!(
            run(&mut comp, std::slice::from_ref(&fps), 0.8, &black, 2)[0],
            255
        );
        // TIME jumped two ticks (0 -> 2): same parity, still a new tick.
        assert_eq!(
            run(&mut comp, std::slice::from_ref(&fps), 2.3, &black, 3)[0],
            0
        );
        // One tick on: grabs white again.
        assert_eq!(
            run(&mut comp, std::slice::from_ref(&fps), 3.1, &white, 4)[0],
            255
        );
        // Hold: later ticks keep the white frame.
        let hold = [fps.clone(), ("hold", Bool(true))];
        assert_eq!(run(&mut comp, &hold, 6.5, &black, 5)[0], 255);
        // Released: a new tick grabs again.
        assert_eq!(
            run(&mut comp, std::slice::from_ref(&fps), 9.2, &black, 6)[0],
            0
        );
        // The kept frame carries the input's alpha, not a marker.
        let mut comp = fx_comp(&device, "freeze");
        let id2 = Some(uuid::Uuid::from_u128(6));
        let a = |comp: &mut Compositor, t: f32, d: &[u8], s: u64| {
            px_at(
                &run_fx(
                    &device,
                    &queue,
                    comp,
                    "freeze",
                    std::slice::from_ref(&fps),
                    t,
                    id2,
                    d,
                    s,
                ),
                4,
                4,
            )
        };
        let first = a(&mut comp, 0.1, &half, 1);
        let kept = a(&mut comp, 0.5, &white, 2);
        assert_eq!(kept, first, "same tick keeps the frame as it was");
        // Over the opaque backdrop, half alpha halves the colour (10 -> 5); an alpha
        // overwritten by a marker would leave it at 10.
        assert!(
            kept[0].abs_diff(5) <= 1 && kept[2].abs_diff(15) <= 1,
            "{kept:?}"
        );
    }

    fn read_px(device: &wgpu::Device, queue: &wgpu::Queue, comp: &Compositor) -> [u8; 4] {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let got = buf.slice(..).get_mapped_range().expect("map")[..4].to_vec();
        buf.unmap();
        [got[0], got[1], got[2], got[3]]
    }

    fn read_row(device: &wgpu::Device, queue: &wgpu::Queue, comp: &Compositor, w: u32) -> Vec<u8> {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let got = buf.slice(..).get_mapped_range().expect("map")[..w as usize * 4].to_vec();
        buf.unmap();
        got
    }

    /// A pixel with mask 0 is transparent after "Remove Background"; before the first
    /// mask arrives the effect is skipped. Skipped when there is no adapter.
    #[test]
    fn gpu_ml_mask_makes_pixels_transparent() {
        let Some((device, queue)) = gpu() else { return };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("remove_bg").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 2, 1);
        comp.add_effect(&device, "remove_bg", &wgsl).unwrap();
        let ml = MlDraw {
            key: 7,
            tag: 0,
            size: (2, 1),
        };
        let red = [255u8, 0, 0, 255].repeat(2);
        let green = [0u8, 255, 0, 255].repeat(2);
        let draw = |comp: &mut Compositor| {
            let layer = [
                LayerDraw {
                    id: uuid::Uuid::from_u128(2),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects: vec![],
                    frame: Some((2, 1, &green, 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                },
                LayerDraw {
                    id: uuid::Uuid::from_u128(1),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects: vec![EffectDraw {
                        lut: None,
                        blend: Default::default(),
                        kind: "remove_bg".into(),
                        // Default params: Transparent mode, threshold 0.5.
                        uniforms: isf.pack(&isf.default_params(), 0.0, (2, 1)),
                        dry_wet: 1.0,
                        ml: Some(ml.clone()),
                        persistent: None,
                        ticking: false,
                    }],
                    frame: Some((2, 1, &red, 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                },
            ];
            comp.render(&device, &queue, &layer, &[], 1.0);
            read_row(&device, &queue, comp, 2)
        };
        // No mask yet: effect skipped, the image shows as is.
        assert_eq!(draw(&mut comp), [255, 0, 0, 255, 255, 0, 0, 255]);
        // The readback of the reduced frame is requested and completes without waiting.
        let mut frames = vec![];
        for _ in 0..100 {
            frames.extend(comp.ml_poll(&device));
            if !frames.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(frames.len(), 1);
        assert_eq!((frames[0].key, frames[0].size), (7, (2, 1)));
        assert_eq!(
            frames[0].rgba, red,
            "reduced frame keeps orientation and colour"
        );
        comp.ml_set_mask(&device, &queue, 7, (2, 1), &[0, 255]);
        let got = draw(&mut comp);
        // Mask 0: the layer below shows through. Mask 255: the pixel is kept.
        assert_eq!(
            &got[..4],
            [0, 255, 0, 255],
            "mask 0 must be transparent: {got:?}"
        );
        assert_eq!(
            &got[4..],
            [255, 0, 0, 255],
            "mask 255 keeps the pixel: {got:?}"
        );
    }

    /// Red channel of each row of a 1-pixel-wide target, top row first.
    fn read_col(device: &wgpu::Device, queue: &wgpu::Queue, comp: &Compositor, h: u32) -> Vec<u8> {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = buf.slice(..).get_mapped_range().expect("map");
        let out = (0..h as usize).map(|r| data[r * 256]).collect();
        drop(data);
        buf.unmap();
        out
    }

    /// Image whose top is black and bottom white keeps its orientation through the dry/wet
    /// mix (dry/wet 0.25 is not symmetric, so a flipped mix pass shows). Skipped without adapter.
    #[test]
    fn gpu_dry_wet_keeps_vertical_orientation() {
        let Some((device, queue)) = gpu() else { return };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("invert").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 1, 4);
        comp.add_effect(&device, "invert", &wgsl).unwrap();
        let img: Vec<u8> = (0..4u8)
            .flat_map(|r| [r * 85, r * 85, r * 85, 255])
            .collect();
        for (dry_wet, first, last) in [(1.0, 255u8, 0u8), (0.25, 64, 191)] {
            let layer = [LayerDraw {
                id: uuid::Uuid::from_u128(1),
                opacity: 1.0,
                blend: BlendMode::Normal,
                fit: FitMode::Stretch,
                effects: vec![EffectDraw {
                    lut: None,
                    blend: Default::default(),
                    kind: "invert".into(),
                    uniforms: isf.pack(&isf.default_params(), 0.0, (1, 4)),
                    dry_wet,
                    ml: None,
                    persistent: None,
                    ticking: false,
                }],
                frame: Some((1, 4, &img, 1)),
                generator: None,
                transition: None,
                transform: Affine::IDENTITY,
                channels: Default::default(),
                mask: Default::default(),
            }];
            comp.render(&device, &queue, &layer, &[], 1.0);
            let col = read_col(&device, &queue, &comp, 4);
            assert!(
                col[0].abs_diff(first) <= 3 && col[3].abs_diff(last) <= 3,
                "dry_wet {dry_wet}: rows {col:?}, want {first} .. {last}"
            );
        }
    }

    /// Whole composition as RGBA rows (needs `w * 4 <= 256`).
    fn read_all(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        comp: &Compositor,
        (w, h): (u32, u32),
    ) -> Vec<[u8; 4]> {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = buf.slice(..).get_mapped_range().expect("map").to_vec();
        buf.unmap();
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (y, x)))
            .map(|(y, x)| {
                let i = (y * 256 + x * 4) as usize;
                [data[i], data[i + 1], data[i + 2], data[i + 3]]
            })
            .collect()
    }

    fn xf(r: vjc_core::transform::Resolved) -> Affine {
        Affine::inverse_of(&r)
    }

    /// Position and scale move pixels as expected: a white 8x8 frame over black, shifted
    /// 4 px right leaves the left half black; at scale 0.5 only the centre 4x4 stays white;
    /// the composition transform does the same to the finished stack. Skipped without adapter.
    #[test]
    fn gpu_transform_position_and_scale_move_pixels() {
        use vjc_core::transform::Resolved;
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 8, 8);
        let white = [255u8; 4];
        let at = |img: &[[u8; 4]], x: usize, y: usize| img[y * 8 + x];
        let render = |comp: &mut Compositor, layer: Affine, master: Affine| {
            let mut l = solid(1, &white, 1.0);
            l.transform = layer;
            comp.master_transform = master;
            comp.render(&device, &queue, &[l], &[], 1.0);
            read_all(&device, &queue, comp, (8, 8))
        };
        // Identity: everything white.
        let img = render(&mut comp, Affine::IDENTITY, Affine::IDENTITY);
        assert!(img.iter().all(|p| *p == white), "identity changed pixels");
        // Position x = +4: right half white, left half black, every row.
        let moved = Resolved {
            x: 4.0,
            ..Resolved::IDENTITY
        };
        let img = render(&mut comp, xf(moved), Affine::IDENTITY);
        for y in 0..8 {
            assert_eq!(at(&img, 1, y), [0, 0, 0, 255], "left half, row {y}");
            assert_eq!(at(&img, 6, y), white, "right half, row {y}");
        }
        // Position y = +4 (down): bottom half white.
        let down = Resolved {
            y: 4.0,
            ..Resolved::IDENTITY
        };
        let img = render(&mut comp, xf(down), Affine::IDENTITY);
        assert_eq!(at(&img, 3, 1), [0, 0, 0, 255]);
        assert_eq!(at(&img, 3, 6), white);
        // Scale 0.5: centre white, border black.
        let half = Resolved {
            scale: (0.5, 0.5),
            ..Resolved::IDENTITY
        };
        let img = render(&mut comp, xf(half), Affine::IDENTITY);
        assert_eq!(at(&img, 3, 3), white);
        assert_eq!(at(&img, 4, 4), white);
        assert_eq!(at(&img, 0, 0), [0, 0, 0, 255]);
        assert_eq!(at(&img, 7, 3), [0, 0, 0, 255]);
        assert_eq!(at(&img, 1, 4), [0, 0, 0, 255]);
        // Rotation 45 degrees: centre stays, the frame corner leaves the picture.
        let rot = Resolved {
            rotation: 45.0,
            ..Resolved::IDENTITY
        };
        let img = render(&mut comp, xf(rot), Affine::IDENTITY);
        assert_eq!(at(&img, 3, 3), white);
        assert_eq!(at(&img, 0, 0), [0, 0, 0, 255]);
        // The composition transform moves the finished stack the same way.
        let img = render(&mut comp, Affine::IDENTITY, xf(moved));
        assert_eq!(at(&img, 1, 2), [0, 0, 0, 255]);
        assert_eq!(at(&img, 6, 2), white);
        let img = render(&mut comp, Affine::IDENTITY, xf(half));
        assert_eq!(at(&img, 3, 3), white);
        assert_eq!(at(&img, 0, 0), [0, 0, 0, 255]);
    }

    /// Rotation X/Y at 0 is the flat path, pixel for pixel (a gradient, so a shift or a
    /// resample would show); Y at 90 degrees is edge on and the picture has no width;
    /// Y at 45 narrows it but keeps the centre. Skipped without adapter.
    #[test]
    fn gpu_rotation_xy_zero_is_identity_and_y_90_has_no_width() {
        use vjc_core::transform::Resolved;
        let Some((device, queue)) = gpu() else { return };
        let n = 16u32;
        let mut comp = Compositor::new(&device, n, n);
        let gdata = grad_data();
        let render = |comp: &mut Compositor, xf: Affine, grad: bool| {
            let white = [255u8; 4];
            let mut l = solid(1, &white, 1.0);
            if grad {
                l.frame = Some((n, n, &gdata, 1));
            }
            l.transform = xf;
            comp.render(&device, &queue, &[l], &[], 1.0);
            read_all(&device, &queue, comp, (n, n))
        };
        fn grad_data() -> Vec<u8> {
            (0..256u32)
                .flat_map(|i| {
                    [
                        (i % 16 * 16) as u8,
                        (i / 16 * 16) as u8,
                        (i % 251) as u8,
                        255,
                    ]
                })
                .collect()
        }
        let flat = render(&mut comp, Affine::IDENTITY, true);
        // Pivot depth alone, or X/Y at exactly 0, must not change one pixel.
        let zero = Resolved {
            anchor_z: 300.0,
            rotation_x: 0.0,
            rotation_y: 0.0,
            ..Resolved::IDENTITY
        };
        let img = render(
            &mut comp,
            Affine::inverse_of_sized(&zero, (n as f64, n as f64)),
            true,
        );
        assert_eq!(img, flat, "rotation X/Y 0 changed pixels");
        let opaque_white = [255, 255, 255, 255];
        let at = |img: &[[u8; 4]], x: usize, y: usize| img[y * n as usize + x];
        // Edge on: nothing of the white picture is left.
        let edge = Resolved {
            rotation_y: 90.0,
            ..Resolved::IDENTITY
        };
        let img = render(
            &mut comp,
            Affine::inverse_of_sized(&edge, (n as f64, n as f64)),
            false,
        );
        let lit = img.iter().filter(|p| **p == opaque_white).count();
        assert!(lit <= n as usize, "{lit} white pixels at Y 90");
        let edge_x = Resolved {
            rotation_x: 90.0,
            ..Resolved::IDENTITY
        };
        let img = render(
            &mut comp,
            Affine::inverse_of_sized(&edge_x, (n as f64, n as f64)),
            false,
        );
        let lit = img.iter().filter(|p| **p == opaque_white).count();
        assert!(lit <= n as usize, "{lit} white pixels at X 90");
        // 45 degrees: narrower than the frame, centre still lit.
        let half = Resolved {
            rotation_y: 45.0,
            ..Resolved::IDENTITY
        };
        let img = render(
            &mut comp,
            Affine::inverse_of_sized(&half, (n as f64, n as f64)),
            false,
        );
        let lit = img.iter().filter(|p| **p == opaque_white).count();
        assert_eq!(at(&img, 8, 8), opaque_white);
        assert!(lit > 0 && lit < (n * n) as usize, "{lit}");
    }

    /// A mask with the top half 0 and the bottom half 255 clears the top half. Skipped
    /// without adapter.
    #[test]
    fn gpu_ml_mask_keeps_vertical_orientation() {
        let Some((device, queue)) = gpu() else { return };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("remove_bg").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 1, 4);
        comp.add_effect(&device, "remove_bg", &wgsl).unwrap();
        let ml = MlDraw {
            key: 8,
            tag: 0,
            size: (1, 4),
        };
        let red = [255u8, 0, 0, 255].repeat(4);
        let green = [0u8, 255, 0, 255].repeat(4);
        let draw = |comp: &mut Compositor| {
            let layers = [
                LayerDraw {
                    id: uuid::Uuid::from_u128(2),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects: vec![],
                    frame: Some((1, 4, &green, 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                },
                LayerDraw {
                    id: uuid::Uuid::from_u128(1),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects: vec![EffectDraw {
                        lut: None,
                        blend: Default::default(),
                        kind: "remove_bg".into(),
                        uniforms: isf.pack(&isf.default_params(), 0.0, (1, 4)),
                        dry_wet: 1.0,
                        ml: Some(ml.clone()),
                        persistent: None,
                        ticking: false,
                    }],
                    frame: Some((1, 4, &red, 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                },
            ];
            comp.render(&device, &queue, &layers, &[], 1.0);
            read_col(&device, &queue, comp, 4)
        };
        draw(&mut comp); // registers the instance
        comp.ml_set_mask(&device, &queue, 8, (1, 4), &[0, 0, 255, 255]);
        let col = draw(&mut comp);
        // Red channel: green layer shows (0) where the mask is 0, red (255) where kept.
        assert_eq!(
            col,
            [0, 0, 255, 255],
            "top half must be transparent: {col:?}"
        );
    }

    fn solid<'a>(n: u128, px: &'a [u8; 4], opacity: f32) -> LayerDraw<'a> {
        LayerDraw {
            id: uuid::Uuid::from_u128(n),
            opacity,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: vec![],
            frame: Some((1, 1, px, 1)),
            generator: None,
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
        }
    }

    fn group(start: usize, len: usize, opacity: f32, effects: Vec<EffectDraw>) -> GroupDraw {
        GroupDraw {
            id: None,
            start,
            len,
            opacity,
            blend: BlendMode::Normal,
            effects,
        }
    }

    /// Two white layers at 50% in a group: the group alone equals the flat stack, group
    /// opacity scales the composed result (not each layer), over black and over grey.
    /// Skipped when there is no adapter.
    #[test]
    fn gpu_group_opacity_matches_flat_stack() {
        let Some((device, queue)) = gpu() else {
            return;
        };
        let (white, grey) = ([255u8; 4], [102u8, 102, 102, 255]);
        let mut comp = Compositor::new(&device, 1, 1);
        // (with grey bottom layer?, group opacity, expected value)
        let cases = [
            (false, None, 191u8), // flat: 0.5 then 0.5 over it = 0.75
            (false, Some(1.0), 191),
            (false, Some(0.5), 96), // 0.75 * 0.5 = 0.375
            (true, None, 217),      // grey 0.4 -> 0.7 -> 0.85
            (true, Some(1.0), 217),
            (true, Some(0.5), 159), // 0.4 * 0.625 + 0.375 = 0.625
        ];
        for (with_grey, group_opacity, want) in cases {
            let mut layers = vec![];
            if with_grey {
                layers.push(solid(1, &grey, 1.0));
            }
            let start = layers.len();
            layers.push(solid(2, &white, 0.5));
            layers.push(solid(3, &white, 0.5));
            let groups: Vec<GroupDraw> = group_opacity
                .map(|o| group(start, 2, o, vec![]))
                .into_iter()
                .collect();
            comp.render_groups(&device, &queue, &layers, &groups, &[], 1.0);
            let got = read_px(&device, &queue, &comp);
            for c in &got[..3] {
                assert!(
                    c.abs_diff(want) <= 2,
                    "grey={with_grey} group={group_opacity:?}: {got:?} want {want}"
                );
            }
        }
    }

    /// Mask mode: the mask layer's luminance multiplies the layer right below it, or all of
    /// the layers below; the mask layer itself is not drawn. Negative controls: mode Off
    /// leaves the picture alone, a black mask hides it, and a layer above the mask is
    /// untouched. Skipped when there is no adapter.
    #[test]
    fn gpu_mask_mode_one_below_and_all_below() {
        use vjc_model::MaskMode;
        let Some((device, queue)) = gpu() else {
            return;
        };
        let (red, green) = ([255u8, 0, 0, 255], [0u8, 255, 0, 255]);
        let (white, grey, black) = ([255u8; 4], [128u8, 128, 128, 255], [0u8, 0, 0, 255]);
        // A fresh compositor per case: layer textures are cached by (id, seq).
        let run = |layers: Vec<LayerDraw>| {
            let mut comp = Compositor::new(&device, 1, 1);
            comp.render(&device, &queue, &layers, &[], 1.0);
            read_px(&device, &queue, &comp)
        };
        let masked = |n, px, mode| LayerDraw {
            mask: mode,
            ..solid(n, px, 1.0)
        };
        // Control: Off draws the layer (it covers the red one below).
        let got = run(vec![solid(1, &red, 1.0), masked(2, &white, MaskMode::Off)]);
        assert_eq!(&got[..3], &[255, 255, 255], "{got:?}");
        // White mask: the red layer shows in full; the mask layer is not drawn.
        let got = run(vec![
            solid(1, &red, 1.0),
            masked(2, &white, MaskMode::OneBelow),
        ]);
        assert_eq!(&got[..3], &[255, 0, 0], "{got:?}");
        // Grey mask: about half.
        let got = run(vec![
            solid(1, &red, 1.0),
            masked(2, &grey, MaskMode::OneBelow),
        ]);
        assert!(got[0].abs_diff(128) <= 3 && got[1] == 0, "{got:?}");
        // Black mask hides the layer below.
        let got = run(vec![
            solid(1, &red, 1.0),
            masked(2, &black, MaskMode::OneBelow),
        ]);
        assert_eq!(&got[..3], &[0, 0, 0], "{got:?}");
        // One below masks only the layer right below: red (bottom) stays, green is hidden.
        let got = run(vec![
            solid(1, &red, 1.0),
            solid(2, &green, 1.0),
            masked(3, &black, MaskMode::OneBelow),
        ]);
        assert_eq!(&got[..3], &[255, 0, 0], "{got:?}");
        // All below hides both.
        let got = run(vec![
            solid(1, &red, 1.0),
            solid(2, &green, 1.0),
            masked(3, &black, MaskMode::AllBelow),
        ]);
        assert_eq!(&got[..3], &[0, 0, 0], "{got:?}");
        // A layer above the mask is not masked.
        let got = run(vec![
            solid(1, &red, 1.0),
            masked(2, &black, MaskMode::AllBelow),
            solid(3, &green, 1.0),
        ]);
        assert_eq!(&got[..3], &[0, 255, 0], "{got:?}");
        // Masking does not leak into the next frame without a mask.
        let got = run(vec![solid(1, &red, 1.0)]);
        assert_eq!(&got[..3], &[255, 0, 0], "{got:?}");
    }

    /// A mask layer with nothing playing masks nothing.
    #[test]
    fn mask_plan_ignores_empty_mask_layer_and_covers_outgoing_half() {
        use vjc_model::MaskMode;
        let empty = LayerDraw {
            mask: MaskMode::AllBelow,
            frame: None,
            ..solid(9, &[0; 4], 1.0)
        };
        let (plan, src) = mask_plan(&[solid(1, &[1; 4], 1.0), empty]);
        assert_eq!(plan, vec![None, None]);
        assert_eq!(src, vec![false, false]);
        let out = LayerDraw {
            transition: Some((1, 0.5, true)),
            ..solid(2, &[1; 4], 1.0)
        };
        let m = LayerDraw {
            mask: MaskMode::OneBelow,
            ..solid(4, &[1; 4], 1.0)
        };
        let (plan, src) = mask_plan(&[solid(1, &[1; 4], 1.0), out, solid(3, &[1; 4], 1.0), m]);
        assert_eq!(plan, vec![None, Some(3), Some(3), None]);
        assert_eq!(src, vec![false, false, false, true]);
    }

    /// A mask layer in a transition: both halves are sources (neither is drawn), the
    /// incoming half is the mask, and "one below" is the real layer below.
    #[test]
    fn mask_plan_transitioning_mask_layer_and_nearest_all_below() {
        use vjc_model::MaskMode;
        let out = |n| LayerDraw {
            transition: Some((1, 0.5, true)),
            mask: MaskMode::OneBelow,
            ..solid(n, &[1; 4], 1.0)
        };
        let inc = LayerDraw {
            mask: MaskMode::OneBelow,
            transition: Some((1, 0.5, false)),
            ..solid(4, &[1; 4], 1.0)
        };
        let (plan, src) = mask_plan(&[solid(1, &[1; 4], 1.0), solid(2, &[1; 4], 1.0), out(3), inc]);
        assert_eq!(plan, vec![None, Some(3), None, None]);
        assert_eq!(src, vec![false, false, true, true]);
        // Control: the plain layer two below is left alone.
        assert_eq!(plan[0], None);
        // Two all-below masks: the nearest above wins.
        let all = |n| LayerDraw {
            mask: MaskMode::AllBelow,
            ..solid(n, &[1; 4], 1.0)
        };
        let (plan, _) = mask_plan(&[
            solid(1, &[1; 4], 1.0),
            all(2),
            solid(3, &[1; 4], 1.0),
            all(4),
        ]);
        assert_eq!(plan, vec![Some(1), None, Some(3), None]);
    }

    /// The group chain runs over the composed group, not over the layers below it.
    /// Skipped when there is no adapter.
    #[test]
    fn gpu_group_effect_applies_to_group_only() {
        let Some((device, queue)) = gpu() else {
            return;
        };
        let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source("invert").unwrap()).unwrap();
        let mut comp = Compositor::new(&device, 1, 1);
        comp.add_effect(&device, "invert", &wgsl).unwrap();
        let fx = EffectDraw {
            lut: None,
            blend: Default::default(),
            kind: "invert".into(),
            uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
            dry_wet: 1.0,
            ml: None,
            persistent: None,
            ticking: false,
        };
        let (below, src) = ([255u8, 0, 0, 255], [51u8, 102, 204, 255]);
        let layers = [solid(1, &below, 1.0), solid(2, &src, 1.0)];
        comp.render_groups(
            &device,
            &queue,
            &layers,
            &[group(1, 1, 1.0, vec![fx])],
            &[],
            1.0,
        );
        let got = read_px(&device, &queue, &comp);
        for (g, w) in got.iter().zip([204u8, 153, 51]) {
            assert!(g.abs_diff(w) <= 2, "{got:?}");
        }
    }

    /// Copies a `w` x 1 texture (w <= 64) back as RGBA.
    fn read_row_of(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: &wgpu::Texture,
        w: u32,
    ) -> Vec<u8> {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let got = buf.slice(..).get_mapped_range().expect("map")[..(w * 4) as usize].to_vec();
        buf.unmap();
        got
    }

    /// Four one-pixel slices on a 4x1 screen, each with its own source: the composition
    /// (layer 2 inside the 50% group, over red), layer 1 alone (red), layer 2 alone (blue at 50% over black)
    /// and a group holding layer 2 at 50% (blue at 25% over black). Master opacity 0 blacks
    /// every routed slice too. Skipped when there is no adapter.
    #[test]
    fn gpu_routed_layer_and_group_show_only_in_their_slice() {
        use vjc_model::SliceSource;
        let Some((device, queue)) = gpu() else {
            return;
        };
        let (red, blue) = ([255u8, 0, 0, 255], [0u8, 0, 255, 255]);
        let gid = uuid::Uuid::from_u128(99);
        let (l1, l2) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
        let layers = [solid(1, &red, 1.0), solid(2, &blue, 0.5)];
        let groups = [GroupDraw {
            id: Some(gid),
            ..group(1, 1, 0.5, vec![])
        }];
        let sources = [
            SliceSource::Composition,
            SliceSource::Layer(l1),
            SliceSource::Layer(l2),
            SliceSource::Group(gid),
        ];
        let mut screen = vjc_core::mapping::new_screen("t", 4, 1);
        for (i, src) in sources.iter().enumerate() {
            let mut sl = vjc_core::mapping::new_slice(4, 1);
            let x = i as f32;
            sl.output_mesh = vec![(x, 0.0), (x + 1.0, 0.0), (x, 1.0), (x + 1.0, 1.0)];
            sl.source = *src;
            screen.slices.push(sl);
        }
        let routes = vjc_core::mapping::routed_sources(&[screen.clone()]);
        assert_eq!(routes.len(), 3, "composition needs no extra render");
        let mut comp = Compositor::new(&device, 1, 1);
        let renderer = crate::slice_render::SliceRenderer::new(&device);
        let (tex, view) = crate::slice_render::make_screen_target(&device, 4, 1);
        let mut show = |opacity: f32, routes: &[SliceSource]| {
            comp.render_routed(&device, &queue, (&layers, &groups), (&[], opacity), routes);
            let src = |sl: &vjc_model::Slice| {
                let v = comp.routed_view(sl.source).unwrap_or(&comp.target_view);
                (v, comp.size)
            };
            renderer.render(&device, &queue, &screen, &src, &view);
            read_row_of(&device, &queue, &tex, 4)
        };
        let img = show(1.0, &routes);
        let at = |i: usize| [img[i * 4], img[i * 4 + 1], img[i * 4 + 2]];
        let near =
            |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 2);
        assert!(near(at(0), [191, 0, 64]), "composition {:?}", at(0));
        assert!(near(at(1), [255, 0, 0]), "layer 1 alone {:?}", at(1));
        assert!(near(at(2), [0, 0, 127]), "layer 2 alone {:?}", at(2));
        assert!(near(at(3), [0, 0, 64]), "group alone {:?}", at(3));
        // Master opacity 0 (bypass) is black on routed slices as well.
        let img = show(0.0, &routes);
        assert!(img.chunks(4).all(|p| p[..3] == [0, 0, 0]), "{img:?}");
        // Nothing routed: no extra texture is kept.
        show(1.0, &[]);
        assert!(comp.routed_view(SliceSource::Layer(l1)).is_none());
    }

    /// The preview texture shows the selected layer alone (slice 1, source Preview) while
    /// slice 0 shows the composition; with no preview set nothing extra is kept and a
    /// Preview slice falls back to the composition. Skipped when there is no adapter.
    #[test]
    fn gpu_preview_texture_shows_selected_layer_only_when_requested() {
        use vjc_model::SliceSource;
        let Some((device, queue)) = gpu() else {
            return;
        };
        let (red, blue) = ([255u8, 0, 0, 255], [0u8, 0, 255, 255]);
        let l2 = uuid::Uuid::from_u128(2);
        let layers = [solid(1, &red, 1.0), solid(2, &blue, 0.5)];
        let mut screen = vjc_core::mapping::new_screen("t", 2, 1);
        for (i, src) in [SliceSource::Composition, SliceSource::Preview]
            .iter()
            .enumerate()
        {
            let mut sl = vjc_core::mapping::new_slice(2, 1);
            let x = i as f32;
            sl.output_mesh = vec![(x, 0.0), (x + 1.0, 0.0), (x, 1.0), (x + 1.0, 1.0)];
            sl.source = *src;
            screen.slices.push(sl);
        }
        let mut comp = Compositor::new(&device, 1, 1);
        let renderer = crate::slice_render::SliceRenderer::new(&device);
        let (tex, view) = crate::slice_render::make_screen_target(&device, 2, 1);
        let mut show = |preview: Option<SliceSource>| {
            comp.preview = preview;
            comp.render_routed(&device, &queue, (&layers, &[]), (&[], 1.0), &[]);
            let src = |sl: &vjc_model::Slice| {
                let v = comp.routed_view(sl.source).unwrap_or(&comp.target_view);
                (v, comp.size)
            };
            renderer.render(&device, &queue, &screen, &src, &view);
            read_row_of(&device, &queue, &tex, 2)
        };
        let near =
            |got: &[u8], want: [u8; 3]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 2);
        let img = show(Some(SliceSource::Layer(l2)));
        assert!(
            near(&img[0..3], [127, 0, 127]),
            "composition {:?}",
            &img[0..3]
        );
        assert!(near(&img[4..7], [0, 0, 127]), "preview {:?}", &img[4..7]);
        let img = show(None);
        assert!(
            near(&img[4..7], [127, 0, 127]),
            "falls back {:?}",
            &img[4..7]
        );
        assert!(comp.routed_view(SliceSource::Preview).is_none());
    }

    /// A preview-only clip (last in the list, `PREVIEW_CLIP_ID`) reaches the Preview texture
    /// but never the composition. Control: the same draw under another id is composed.
    /// Skipped when there is no adapter.
    #[test]
    fn gpu_preview_clip_stays_out_of_the_composition() {
        use vjc_model::SliceSource;
        let Some((device, queue)) = gpu() else {
            return;
        };
        let (red, green) = ([255u8, 0, 0, 255], [0u8, 255, 0, 255]);
        let mut comp = Compositor::new(&device, 1, 1);
        let mut run = |id: LayerId| {
            let mut clip = solid(9, &green, 1.0);
            clip.id = id;
            let layers = [solid(1, &red, 1.0), clip];
            comp.preview = Some(SliceSource::Layer(PREVIEW_CLIP_ID));
            comp.render_routed(&device, &queue, (&layers, &[]), (&[], 1.0), &[]);
            let target = read_px(&device, &queue, &comp);
            let prev = comp.routed_view(SliceSource::Preview).is_some();
            (target, prev)
        };
        let (target, prev) = run(PREVIEW_CLIP_ID);
        assert!(prev, "preview rendered");
        assert!(
            target[0] > 200 && target[1] < 20,
            "composition stays red {target:?}"
        );
        let (target, _) = run(uuid::Uuid::from_u128(9));
        assert!(
            target[1] > 200,
            "control: a normal layer is composed {target:?}"
        );
    }

    /// Channel toggles: a mid-grey-blue layer with G off loses its green; with A off an
    /// alpha 0.5 picture turns opaque. Skipped when there is no adapter.
    #[test]
    fn gpu_clip_channel_toggles_zero_channels_and_force_opaque_alpha() {
        let Some((device, queue)) = gpu() else {
            return;
        };
        let px = [200u8, 100, 50, 128];
        let mut layers = [solid(1, &px, 1.0)];
        let mut comp = Compositor::new(&device, 1, 1);
        let mut show = |channels: vjc_model::Channels| {
            layers[0].channels = channels;
            comp.render(&device, &queue, &layers, &[], 1.0);
            let (tex, view) = crate::slice_render::make_screen_target(&device, 1, 1);
            let mut screen = vjc_core::mapping::new_screen("t", 1, 1);
            screen.slices.push(vjc_core::mapping::new_slice(1, 1));
            let renderer = crate::slice_render::SliceRenderer::new(&device);
            let src = |_: &vjc_model::Slice| (&comp.target_view, comp.size);
            renderer.render(&device, &queue, &screen, &src, &view);
            read_row_of(&device, &queue, &tex, 1)
        };
        let all = show(vjc_model::Channels::default());
        let no_g = show(vjc_model::Channels {
            g: false,
            ..Default::default()
        });
        assert_eq!(no_g[1], 0, "green off");
        assert!(no_g[0].abs_diff(all[0]) <= 2 && no_g[2].abs_diff(all[2]) <= 2);
        let no_a = show(vjc_model::Channels {
            a: false,
            ..Default::default()
        });
        assert!(
            no_a[0].abs_diff(200) <= 2,
            "opaque alpha keeps colour {no_a:?}"
        );
        assert!(all[0] < 150, "half alpha over black darkens {all:?}");
    }

    /// Alpha type: the same texel is brighter once declared premultiplied (its colour is
    /// divided by alpha before blending). Skipped when there is no adapter.
    #[test]
    fn gpu_premultiplied_source_is_unpremultiplied_before_blending() {
        let Some((device, queue)) = gpu() else {
            return;
        };
        let mut comp = Compositor::new(&device, 1, 1);
        // Own layer id per texel: the compositor keeps a layer's texture while its seq stays.
        let mut show = |id: u128, px: [u8; 4], premultiplied: bool| {
            let mut layers = [solid(id, &px, 1.0)];
            layers[0].channels = vjc_model::Channels {
                premultiplied,
                ..Default::default()
            };
            comp.render(&device, &queue, &layers, &[], 1.0);
            read_px(&device, &queue, &comp)
        };
        let straight = show(1, [100, 50, 25, 128], false);
        let premult = show(1, [100, 50, 25, 128], true);
        // Straight: 100 * 0.5 over black; premultiplied: (100 / 0.5) * 0.5.
        assert!(straight[0].abs_diff(50) <= 3, "{straight:?}");
        assert!(premult[0].abs_diff(100) <= 3, "{premult:?}");
        // An opaque texel is the same either way.
        let a = show(2, [100, 50, 25, 255], false);
        let b = show(2, [100, 50, 25, 255], true);
        assert!(a[0].abs_diff(b[0]) <= 1, "{a:?} {b:?}");
    }

    #[test]
    fn layer_area_scales_the_picture_into_its_own_rect() {
        let ex = |w, h| vjc_model::LayerExtras {
            width: w,
            height: h,
            ..Default::default()
        };
        let frame = (1920, 1080);
        // Composition size: nothing to do.
        assert!(area_affine(&ex(0, 0), (1920, 1080), frame, FitMode::Fit).is_none());
        assert!(area_affine(&ex(1920, 1080), (1920, 1080), frame, FitMode::Fit).is_none());
        // Half-size area, same aspect: the picture covers half the frame on each axis.
        let a = area_affine(&ex(960, 540), (1920, 1080), frame, FitMode::Fit).unwrap();
        assert!(
            (a.m[0] - 2.0).abs() < 1e-4 && (a.m[3] - 2.0).abs() < 1e-4,
            "{a:?}"
        );
        // Stretch into a square area: both axes follow the area.
        let s = area_affine(&ex(540, 540), (1920, 1080), frame, FitMode::Stretch).unwrap();
        assert!((s.m[0] - 1920.0 / 540.0).abs() < 1e-3 && (s.m[3] - 1080.0 / 540.0).abs() < 1e-3);
        // Fit into the same square keeps the aspect: the 16:9 picture fills its width only.
        let f = area_affine(&ex(540, 540), (1920, 1080), frame, FitMode::Fit).unwrap();
        assert!((f.m[0] - f.m[3]).abs() < 1e-4 && f.m[0] > 3.0, "{f:?}");
        // Native size is left alone by the area.
        assert!(area_affine(&ex(540, 540), (1920, 1080), frame, FitMode::Original).is_none());
    }

    /// Measures ms/frame of N 1080p layers with two effects each (CPU submit + GPU wait):
    /// `VJC_BENCH_LAYERS=8 cargo test -p vjc-app --release bench_layers -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_layers() {
        let n: usize = std::env::var("VJC_BENCH_LAYERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            return;
        };
        let (w, h) = (1920u32, 1080u32);
        let data = vec![128u8; (w * h * 4) as usize];
        let mut comp = Compositor::new(&device, w, h);
        // Two effects per layer (one with a blend mix) so the scratch Pool, the uniform
        // buffers and the per-pass bind groups are exercised, like a real show.
        let kinds = ["brightness_contrast", "hue_shift"];
        let fxs: Vec<EffectDraw> = kinds
            .iter()
            .map(|k| {
                let (isf, wgsl) = vjc_fx::compile(vjc_fx::builtin_source(k).unwrap()).unwrap();
                comp.add_effect(&device, k, &wgsl).unwrap();
                EffectDraw {
                    lut: None,
                    blend: Default::default(),
                    kind: (*k).into(),
                    uniforms: isf.pack(&isf.default_params(), 0.0, (w, h)),
                    dry_wet: 0.7,
                    ml: None,
                    persistent: None,
                    ticking: false,
                }
            })
            .collect();
        let mut stats = crate::frame_stats::FrameStats::new();
        for i in 0..130 {
            let layers: Vec<LayerDraw> = (0..n)
                .map(|k| LayerDraw {
                    id: uuid::Uuid::from_u128(k as u128 + 1),
                    opacity: 0.8,
                    blend: BlendMode::Normal,
                    fit: FitMode::Fit,
                    effects: fxs.clone(),
                    frame: Some((w, h, &data, i as u64 + 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
                    mask: Default::default(),
                })
                .collect();
            let t = std::time::Instant::now();
            comp.render(&device, &queue, &layers, &[], 1.0);
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
            if i >= 10 {
                stats.push(t.elapsed().as_secs_f32() * 1000.0);
            }
        }
        eprintln!(
            "bench_layers n={n} {w}x{h}: avg {:.2} ms, p99 {:.2} ms",
            stats.avg(),
            stats.p99()
        );
    }
}
