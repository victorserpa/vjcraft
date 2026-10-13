//! GPU compositor: stacks layers bottom to top into an Rgba8Unorm texture.
//! Each layer is one pass that reads the composition so far (a copy) and blends in the
//! shader; transition-only BlendModes render as Normal. Mirrors `vjc_core::blend`.

use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use vjc_core::blend::shader_id;
use vjc_model::{BlendMode, FitMode, LayerId};

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
struct U { opacity: f32, mode: u32, scale: vec2<f32> };
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
    let q = (in.uv - vec2(0.5)) / u.scale + vec2(0.5);
    if (q.x < 0.0 || q.x > 1.0 || q.y < 0.0 || q.y > 1.0) {
        discard;
    }
    let s = textureSample(tex, smp, q);
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
    }
    return vec4(mix(b.rgb, rgb, a), a + b.a * (1.0 - a));
}
"#;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub struct LayerDraw<'a> {
    pub id: LayerId,
    pub opacity: f32,
    pub blend: BlendMode,
    pub fit: FitMode,
    /// `(width, height, rgba, seq)`; `None` = nothing playing (layer skipped).
    pub frame: Option<(u32, u32, &'a [u8], u64)>,
}

struct Slot {
    tex: wgpu::Texture,
    size: (u32, u32),
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    seq: u64,
}

pub struct Compositor {
    pipe: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    base_bgl: wgpu::BindGroupLayout,
    base: wgpu::Texture,
    base_bind: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    slots: HashMap<LayerId, Slot>,
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
            | wgpu::TextureUsages::COPY_SRC,
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
        let (target, target_view) = make_target(device, w, h);
        let (base, base_bind) = make_base(device, &base_bgl, w, h);
        Compositor {
            pipe,
            bgl,
            base_bgl,
            base,
            base_bind,
            sampler,
            slots: HashMap::new(),
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

    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers_bottom_to_top: &[LayerDraw],
    ) {
        let live: Vec<LayerId> = layers_bottom_to_top
            .iter()
            .filter(|l| l.frame.is_some())
            .map(|l| l.id)
            .collect();
        self.slots.retain(|k, _| live.contains(k));
        for l in layers_bottom_to_top {
            let Some((w, h, data, seq)) = l.frame else {
                continue;
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
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("layer_u"),
                    size: 16,
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
                        tex,
                        size: (w, h),
                        uniform,
                        bind,
                        seq: u64::MAX,
                    },
                );
            }
            let slot = self.slots.get_mut(&l.id).unwrap();
            if slot.seq != seq {
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
                bytemuck::bytes_of(&[
                    l.opacity.to_bits(),
                    shader_id(&l.blend),
                    sx.to_bits(),
                    sy.to_bits(),
                ]),
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
        for l in layers_bottom_to_top {
            let Some(slot) = self.slots.get(&l.id) else {
                continue;
            };
            enc.copy_texture_to_texture(
                self.target.as_image_copy(),
                self.base.as_image_copy(),
                extent,
            );
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("compose"),
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
            pass.set_bind_group(0, &slot.bind, &[]);
            pass.set_bind_group(1, &self.base_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(enc.finish()));
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
                    frame: Some((4, 4, &bd, 1)),
                },
                LayerDraw {
                    id: uuid::Uuid::from_u128(2),
                    opacity,
                    blend: mode.clone(),
                    fit: FitMode::Stretch,
                    frame: Some((4, 4, &sd, 1)),
                },
            ];
            comp.render(&device, &queue, &layers);
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
}
