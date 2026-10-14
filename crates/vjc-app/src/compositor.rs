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
    };
    (sw * k / dw, sh * k / dh)
}

const SHADER: &str = r#"
// scale = fraction of the composition frame covered by the video (1,1 = whole frame).
// mode = index into vjc_core::blend::COMPOSITION_MODES.
// tkind/prog/out: layer transition (see vjc_core::transition::Kind::shader_id); out = 1 for
// the outgoing clip, which is drawn first. tkind 0 = none.
// m/t: inverse transform, output px (centred on the frame, y down) -> picture px:
// c = M * p + t.xy, with t.zw = frame size in px. t.z == 0 means no transform.
struct U { opacity: f32, mode: u32, scale: vec2<f32>, prog: f32, tkind: u32, out: u32, pad: u32, m: vec4<f32>, t: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
// Copy of the composition below this layer.
@group(1) @binding(0) var base_tex: texture_2d<f32>;

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

// Output is the final composited pixel (no hardware blending): mix(base, blended, alpha).
@fragment
fn fs(in: VO) -> @location(0) vec4<f32> {
    // Map frame uv to video uv; outside the video is letterbox (discard keeps the base).
    var q = (in.uv - vec2(0.5)) / u.scale + vec2(0.5);
    if (u.t.z > 0.0) {
        let p0 = (in.uv - vec2(0.5)) * u.t.zw;
        let c = vec2(u.m.x * p0.x + u.m.y * p0.y, u.m.z * p0.x + u.m.w * p0.y) + u.t.xy;
        q = c / (u.t.zw * u.scale) + vec2(0.5);
    }
    var tmix = 1.0;
    var tblack = 0.0;
    let p = u.prog;
    if (u.tkind == 1u) {
        if (u.out == 0u) { tmix = p; }
    } else if (u.tkind == 2u) {
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
    }
    if (q.x < 0.0 || q.x > 1.0 || q.y < 0.0 || q.y > 1.0) {
        discard;
    }
    var s = textureSample(tex, smp, q);
    // pad = channel mask: bit 0..3 set = R, G, B, A off (off alpha means opaque).
    if ((u.pad & 1u) != 0u) { s.r = 0.0; }
    if ((u.pad & 2u) != 0u) { s.g = 0.0; }
    if ((u.pad & 4u) != 0u) { s.b = 0.0; }
    if ((u.pad & 8u) != 0u) { s.a = 1.0; }
    s = vec4(mix(s.rgb, vec3(0.0), tblack), s.a * tmix);
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

/// The 16 uniform words of a layer pass: the 8 base words, then the inverse transform
/// (`m`, then `t.xy` and the frame size). An identity transform leaves the last four zero,
/// which the shader reads as "no transform" (the exact pre-transform mapping).
fn uniform_words(base: [u32; 8], xf: &Affine, size: (u32, u32)) -> [u32; 16] {
    let mut w = [0u32; 16];
    w[..8].copy_from_slice(&base);
    if !xf.is_identity() {
        let f = |v: f64| (v as f32).to_bits();
        w[8..12].copy_from_slice(&xf.m.map(f));
        w[12] = f(xf.t[0]);
        w[13] = f(xf.t[1]);
        w[14] = (size.0.max(1) as f32).to_bits();
        w[15] = (size.1.max(1) as f32).to_bits();
    }
    w
}

/// Vertex stage shared by all effect fragment shaders. `uv` is ISF-style (origin bottom-left).
const FX_VS: &str = r#"
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

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One active (non-bypassed) effect with its uniform block for this frame.
#[derive(Clone)]
pub struct EffectDraw {
    pub kind: String,
    pub uniforms: vjc_fx::Uniforms,
    /// 1 = effect only, 0 = input only (effect skipped), between = mix(in, out, dry_wet).
    pub dry_wet: f32,
    /// Set for ML effects: the effect reads a mask produced by an ML worker and is skipped
    /// until the first one arrives.
    pub ml: Option<MlDraw>,
    /// Instance id of a persistent effect (trails, echo): the effect gets its own previous
    /// output as `PREV_PIXEL`. None for ordinary effects.
    pub persistent: Option<uuid::Uuid>,
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

fn mix_plan(dry_wet: f32) -> MixPlan {
    if dry_wet.is_nan() || dry_wet <= 0.0 {
        MixPlan::Skip
    } else if dry_wet >= 1.0 {
        MixPlan::Wet
    } else {
        MixPlan::Blend(dry_wet)
    }
}

/// Mixes the effect input (`a`, dry) with its output (`b`, wet) by `t.x`.
const MIX_FS: &str = r#"
struct U { t: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var dry: texture_2d<f32>;
@group(0) @binding(2) var wet: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@fragment
fn main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    // `uv` is ISF-style (y up); textures are stored top row first.
    let p = vec2(uv.x, 1.0 - uv.y);
    return mix(textureSample(dry, samp, p), textureSample(wet, samp, p), u.t.x);
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
    /// Previous output of each persistent effect instance.
    persist: PersistStore<(wgpu::Texture, wgpu::TextureView)>,
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
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fx_bg"),
        layout: bgl,
        entries: &[
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
        ],
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
        draw_pass(
            device, enc, bgl, sampler, pipe, buf, blank, dst, blank, blank,
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
            let plan = mix_plan(e.dry_wet);
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
            let buf = &self.bufs[self.next];
            self.next += 1;
            queue.write_buffer(buf, 0, bytemuck::cast_slice(&e.uniforms));
            let out = (0..3).find(|&i| Some(i) != cur).unwrap_or(0);
            let src = match cur {
                None => input,
                Some(i) => &scratch[i].1,
            };
            // Persistent effect: its previous output is bound as PREV_PIXEL, and this
            // frame's result is copied back after the pass.
            let hist = e.persistent.map(|id| {
                self.persist
                    .acquire(id, size, || make_history(device, size))
                    .0
            });
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
                queue.write_buffer(mbuf, 0, bytemuck::cast_slice(&[t, 0.0, 0.0, 0.0]));
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
    sampler: wgpu::Sampler,
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
) -> (wgpu::Texture, wgpu::BindGroup) {
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
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("composition_base_bg"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&v),
        }],
    });
    (t, bind)
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
        let fx_bgl = {
            let mut e: Vec<wgpu::BindGroupLayoutEntry> = vec![];
            for binding in 0..5u32 {
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
        };
        let base_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compositor_base_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
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
            size: 64,
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
            persist: PersistStore::default(),
        };
        let (target, target_view) = make_target(device, w, h);
        let (base, base_bind) = make_base(device, &base_bgl, w, h);
        Compositor {
            pipe,
            bgl,
            fx_bgl,
            base_bgl,
            base,
            base_bind,
            sampler,
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
    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32) -> bool {
        if self.size == (w, h) {
            return false;
        }
        (self.target, self.target_view) = make_target(device, w.max(1), h.max(1));
        (self.base, self.base_bind) = make_base(device, &self.base_bgl, w.max(1), h.max(1));
        self.size = (w, h);
        true
    }

    /// Installs a translated effect shader (WGSL from `vjc_fx::translate`). Returns the
    /// validation error instead of panicking, so a bad shader only disables its effect.
    pub fn add_effect(
        &mut self,
        device: &wgpu::Device,
        kind: &str,
        wgsl: &str,
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
        to_group: bool,
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
        let (dst_tex, dst_view, base_tex, base_bind) = match &self.group {
            Some(g) if to_group => (&g.tex, &g.view, &g.base, &g.base_bind),
            _ => (&self.target, &self.target_view, &self.base, &self.base_bind),
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

    /// Composes `run` into the group buffer (transparent start), applies the group chain and
    /// blends the result into the composition. False when nothing in the group is drawn.
    #[allow(clippy::too_many_arguments)]
    fn compose_group(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        run: &[LayerDraw],
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
            let (base, base_bind) = make_base(device, &self.base_bgl, size.0, size.1);
            self.group = Some(GroupBuf {
                tex,
                view,
                base,
                base_bind,
                size,
                uniforms: vec![],
            });
        }
        let gb = self.group.as_mut().unwrap();
        while gb.uniforms.len() <= n {
            gb.uniforms
                .push(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("group_u"),
                    size: 64,
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
        for l in run {
            self.draw_layer(device, queue, enc, l, true, extent);
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
        (layers_bottom_to_top, groups): (&[LayerDraw], &[GroupDraw]),
        (master, master_opacity): (&[EffectDraw], f32),
        routes: &[SliceSource],
    ) {
        self.fx.next = 0;
        self.fx.mix_next = 0;
        self.fx.scratch.begin_frame();
        let live: Vec<LayerId> = layers_bottom_to_top
            .iter()
            .filter(|l| l.frame.is_some() || l.generator.is_some())
            .map(|l| l.id)
            .collect();
        self.slots.retain(|k, _| live.contains(k));
        for l in layers_bottom_to_top {
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
                    size: 64,
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
        let mut i = 0;
        let mut group_no = 0;
        while i < layers_bottom_to_top.len() {
            let group = groups
                .iter()
                .find(|g| g.start == i && g.len > 0 && i + g.len <= layers_bottom_to_top.len());
            if let Some(g) = group {
                let run = &layers_bottom_to_top[i..i + g.len];
                if self.compose_group(device, queue, &mut enc, run, g, group_no, extent) {
                    group_no += 1;
                }
                i += g.len;
            } else {
                self.draw_layer(
                    device,
                    queue,
                    &mut enc,
                    &layers_bottom_to_top[i],
                    false,
                    extent,
                );
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
                (layers_bottom_to_top, groups),
                (extent, fade > 0.0, &mut group_no),
            );
        }
        self.fx.scratch.sweep();
        self.fx.persist.sweep();
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
                size: 64,
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
        match content {
            SliceSource::Layer(id) => {
                if let Some(l) = layers.iter().find(|l| l.id == id) {
                    self.draw_layer(device, queue, enc, l, false, extent);
                }
            }
            SliceSource::Group(id) => {
                let found = groups
                    .iter()
                    .find(|g| g.id == Some(id) && g.len > 0 && g.start + g.len <= layers.len());
                if let Some(g) = found {
                    let run = &layers[g.start..g.start + g.len];
                    if self.compose_group(device, queue, enc, run, g, *group_no, extent) {
                        *group_no += 1;
                    }
                }
            }
            SliceSource::Composition | SliceSource::Preview => {}
        }
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
            kind: "invert".into(),
            uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
            dry_wet: 1.0,
            ml: None,
            persistent: None,
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
                kind: "solid_color".into(),
                uniforms: isf.pack(&params, 0.0, (1, 1)),
                dry_wet: 1.0,
                ml: None,
                persistent: None,
            }),
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
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
                kind: "solid_color".into(),
                uniforms: isf.pack(&params, 0.0, (w, h)),
                dry_wet: 1.0,
                ml: None,
                persistent: None,
            }),
            transition: None,
            transform: Affine::IDENTITY,
            channels: Default::default(),
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
                if let Err(e) = comp.add_effect(&device, id, &wgsl) {
                    bad.push(format!("{id}: GPU compile: {e}"));
                    continue;
                }
                let draw = EffectDraw {
                    kind: id.to_string(),
                    uniforms: isf.pack(&isf.default_params(), 0.1, (w, h)),
                    dry_wet: 1.0,
                    ml: None,
                    persistent: None,
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
            }
        }
        assert_eq!(count, vjc_fx::BUILTIN.len() + vjc_fx::GENERATORS.len());
        assert!(bad.is_empty(), "failed:\n{}", bad.join("\n"));
    }

    #[test]
    fn mix_plan_cases() {
        assert_eq!(mix_plan(0.0), MixPlan::Skip);
        assert_eq!(mix_plan(-1.0), MixPlan::Skip);
        assert_eq!(mix_plan(f32::NAN), MixPlan::Skip);
        assert_eq!(mix_plan(1.0), MixPlan::Wet);
        assert_eq!(mix_plan(2.0), MixPlan::Wet);
        assert_eq!(mix_plan(0.5), MixPlan::Blend(0.5));
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
            kind: "invert".into(),
            uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
            dry_wet,
            ml: None,
            persistent: None,
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
                kind: "trails".into(),
                uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
                dry_wet: 1.0,
                ml: None,
                persistent: instance_id,
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
                },
                LayerDraw {
                    id: uuid::Uuid::from_u128(1),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects: vec![EffectDraw {
                        kind: "remove_bg".into(),
                        // Default params: Transparent mode, threshold 0.5.
                        uniforms: isf.pack(&isf.default_params(), 0.0, (2, 1)),
                        dry_wet: 1.0,
                        ml: Some(ml.clone()),
                        persistent: None,
                    }],
                    frame: Some((2, 1, &red, 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
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
                    kind: "invert".into(),
                    uniforms: isf.pack(&isf.default_params(), 0.0, (1, 4)),
                    dry_wet,
                    ml: None,
                    persistent: None,
                }],
                frame: Some((1, 4, &img, 1)),
                generator: None,
                transition: None,
                transform: Affine::IDENTITY,
                channels: Default::default(),
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
                },
                LayerDraw {
                    id: uuid::Uuid::from_u128(1),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    fit: FitMode::Stretch,
                    effects: vec![EffectDraw {
                        kind: "remove_bg".into(),
                        uniforms: isf.pack(&isf.default_params(), 0.0, (1, 4)),
                        dry_wet: 1.0,
                        ml: Some(ml.clone()),
                        persistent: None,
                    }],
                    frame: Some((1, 4, &red, 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
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
            kind: "invert".into(),
            uniforms: isf.pack(&isf.default_params(), 0.0, (1, 1)),
            dry_wet: 1.0,
            ml: None,
            persistent: None,
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

    /// Measures ms/frame of N 1080p layers with one effect each (CPU submit + GPU wait):
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
        let mut stats = crate::frame_stats::FrameStats::new();
        for i in 0..130 {
            let layers: Vec<LayerDraw> = (0..n)
                .map(|k| LayerDraw {
                    id: uuid::Uuid::from_u128(k as u128 + 1),
                    opacity: 0.8,
                    blend: BlendMode::Normal,
                    fit: FitMode::Fit,
                    effects: vec![],
                    frame: Some((w, h, &data, i as u64 + 1)),
                    generator: None,
                    transition: None,
                    transform: Affine::IDENTITY,
                    channels: Default::default(),
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
