//! Draws the slices of one output screen: each slice samples its input region of the
//! composition and paints it on a deformable mesh (two triangles per cell, UVs from
//! `vjc_core::mapping`). A slice mask (polygon in normalized screen coordinates, even-odd,
//! tested per fragment) discards pixels outside it; a soft edge darkens the slice borders
//! with `ramp^gamma` (`vjc_core::mapping::edge_ramp` is the CPU twin of the shader).

use eframe::egui_wgpu::wgpu;
use vjc_core::mapping;
use vjc_model::{Screen, Slice};

pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const SHADER: &str = r#"
struct VO {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) st: vec2<f32>,
};
struct U {
    edge: vec4<f32>,            // left, right, top, bottom
    misc: vec4<f32>,            // gamma, mask point count, screen w, screen h
    gain: vec4<f32>,            // slice rgb multiplier
    con: vec4<f32>,             // slice contrast, screen contrast, screen opacity, flags
                                // flags bit 0 invert mask, bit 1 is key, bit 2 black bg
    sgain: vec4<f32>,           // screen rgb multiplier
    ext: vec4<f32>,             // slice shape point count, output mask point count
    tf: vec4<f32>,              // content transform: scale, shift x, shift y, rotation (radians)
    piv: vec4<f32>,             // input area centre x, y, width, height (source UV)
    tfx: vec4<f32>,             // source aspect (w / h), transform active (0 or 1)
    pts: array<vec4<f32>, 96>,  // two points per entry: mask 0.., shape 32.., output mask 64..
};
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(1) @binding(0) var<uniform> u: U;
@vertex
fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) st: vec2<f32>) -> VO {
    var o: VO;
    o.pos = vec4(p, 0.0, 1.0);
    o.uv = uv;
    o.st = st;
    return o;
}
fn pt(i: u32) -> vec2<f32> {
    let v = u.pts[i / 2u];
    return select(v.xy, v.zw, (i & 1u) == 1u);
}
fn ramp(d: f32, w: f32) -> f32 {
    return select(1.0, clamp(d / w, 0.0, 1.0), w > 0.0);
}
fn contrast(c: vec3<f32>, k: f32) -> vec3<f32> {
    // Exactly 1.0 must not touch the pixel (no float round trip).
    return select(clamp((c - vec3(0.5)) * k + vec3(0.5), vec3(0.0), vec3(1.0)), c, k == 1.0);
}
// Where the transformed content comes from: the pixel's source UV goes through the inverse
// of the transform, pivoting on the centre of the input area.
fn xform_uv(uv: vec2<f32>) -> vec2<f32> {
    let asp = vec2(u.tfx.x, 1.0);
    let d = (uv - u.piv.xy - u.tf.yz * u.piv.zw) * asp;
    let c = cos(u.tf.w);
    let s = sin(u.tf.w);
    // Inverse turn (the content turns clockwise on screen, y points down).
    let r = vec2(d.x * c + d.y * s, -d.x * s + d.y * c) / u.tf.x;
    return u.piv.xy + r / asp;
}
fn inside(off: u32, n: u32, p: vec2<f32>) -> bool {
    var res = false;
    var j = n - 1u;
    for (var i = 0u; i < n; i = i + 1u) {
        let a = pt(off + i);
        let b = pt(off + j);
        if ((a.y > p.y) != (b.y > p.y)
            && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x) {
            res = !res;
        }
        j = i;
    }
    return res;
}
@fragment
fn fs(in: VO) -> @location(0) vec4<f32> {
    let p = in.pos.xy / u.misc.zw;
    let n = u32(u.misc.y);
    if (n >= 3u && inside(0u, n, p) == ((u32(u.con.w) & 1u) != 0u)) {
        discard;
    }
    let ns = u32(u.ext.x);
    if (ns >= 3u && !inside(64u, ns, p)) {
        discard;
    }
    let no = u32(u.ext.y);
    if (no >= 3u && !inside(128u, no, p)) {
        discard;
    }
    let lin = ramp(in.st.x, u.edge.x) * ramp(1.0 - in.st.x, u.edge.y)
        * ramp(in.st.y, u.edge.z) * ramp(1.0 - in.st.y, u.edge.w);
    var uv = in.uv;
    if (u.tfx.y > 0.5) {
        uv = xform_uv(uv);
    }
    var c = textureSample(src, smp, uv);
    let half = u.piv.zw * 0.5;
    if (u.tfx.y > 0.5 && any(abs(uv - u.piv.xy) > half)) {
        // Outside the slice's input area: transparent.
        c = vec4(0.0);
    }
    let flags = u32(u.con.w);
    if ((flags & 2u) != 0u) {
        // Is Key: alpha as a grey level, opaque.
        c = vec4(vec3(c.a), 1.0);
    } else if ((flags & 4u) != 0u) {
        // Black BG: composite over black, opaque.
        c = vec4(c.rgb * c.a, 1.0);
    }
    // Slice colour first, then the screen colour on top (the screen's final stage). The soft
    // edge fades last, so contrast below 1 never lifts a blended edge off black.
    let s1 = contrast(c.rgb, u.con.x) * u.gain.rgb;
    let s2 = contrast(s1, u.con.y) * u.sgain.rgb * u.con.z;
    return vec4(s2 * pow(lin, u.misc.x), c.a);
}
"#;

/// Points each polygon region (slice mask, slice shape, output mask) holds in the uniform.
const REGION: usize = 64;
/// Floats before the point regions: 9 vec4.
const HEADER: usize = 36;
/// Uniform block size: the header + 3 regions of 32 vec4.
const UNIFORM_FLOATS: usize = HEADER + 3 * 2 * REGION;

/// The transform as the shader may use it: scale inside its range (1 when not a number),
/// non-finite shift or rotation as 0, so a hand-edited file still renders.
fn sane_transform(t: &vjc_model::SliceTransform) -> vjc_model::SliceTransform {
    let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
    vjc_model::SliceTransform {
        scale: if t.scale.is_nan() {
            1.0
        } else {
            t.scale
                .clamp(mapping::SCALE_RANGE.0, mapping::SCALE_RANGE.1)
        },
        offset: (fin(t.offset.0), fin(t.offset.1)),
        rotation: fin(t.rotation),
    }
}

/// Packs the per-slice uniform (see `U` in the shader).
fn slice_uniform(screen: &Screen, s: &Slice, src_size: (u32, u32)) -> Vec<f32> {
    let mut u = vec![0.0f32; UNIFORM_FLOATS];
    let (mut n_mask, mut n_shape, mut n_out) = (0, 0, 0);
    if let Some(e) = &s.soft_edge {
        u[..4].copy_from_slice(&[e.left, e.right, e.top, e.bottom]);
        u[4] = e.gamma;
    } else {
        u[4] = 1.0;
    }
    let mut put = |region: usize, pts: &[(f32, f32)]| {
        for (i, p) in pts.iter().enumerate() {
            u[HEADER + 2 * (region * REGION + i)] = p.0;
            u[HEADER + 1 + 2 * (region * REGION + i)] = p.1;
        }
    };
    if let Some(m) = s.mask.as_ref().filter(|m| mapping::mask_valid(m)) {
        let m = vjc_core::slice_adv::flatten_mask(m, &s.mask_tangents);
        put(0, &m);
        n_mask = m.len();
    }
    if let vjc_model::SliceShape::Polygon(poly) = &s.shape
        && mapping::mask_valid(poly)
    {
        put(1, poly);
        n_shape = poly.len();
    }
    if let Some(m) = screen.output_mask.as_ref().map(|m| m.points())
        && mapping::mask_valid(m)
    {
        let m = vjc_core::slice_adv::flatten_mask(m, &screen.output_mask_tangents);
        put(2, &m);
        n_out = m.len();
    }
    u[5] = n_mask as f32;
    u[20] = n_shape as f32;
    u[21] = n_out as f32;
    u[6] = screen.width.max(1) as f32;
    u[7] = screen.height.max(1) as f32;
    u[8..11].copy_from_slice(&mapping::slice_gain(&s.color));
    u[11] = 1.0;
    u[12] = s.color.contrast;
    u[13] = screen.color.color.contrast;
    u[14] = screen.color.opacity;
    u[16..19].copy_from_slice(&mapping::slice_gain(&screen.color.color));
    u[19] = 1.0;
    let t = sane_transform(&s.transform);
    u[24..28].copy_from_slice(&[t.scale, t.offset.0, t.offset.1, t.rotation.to_radians()]);
    let xs = s.input.iter().map(|p| p.0);
    let ys = s.input.iter().map(|p| p.1);
    let (x0, x1) = xs.fold((f32::MAX, f32::MIN), |a, v| (a.0.min(v), a.1.max(v)));
    let (y0, y1) = ys.fold((f32::MAX, f32::MIN), |a, v| (a.0.min(v), a.1.max(v)));
    if x1 > x0 && y1 > y0 {
        u[28..32].copy_from_slice(&[(x0 + x1) / 2.0, (y0 + y1) / 2.0, x1 - x0, y1 - y0]);
    } else {
        u[28..32].copy_from_slice(&[0.5, 0.5, 1.0, 1.0]);
    }
    u[32] = src_size.0.max(1) as f32 / src_size.1.max(1) as f32;
    u[33] = (t != vjc_model::SliceTransform::default()) as u32 as f32;
    u[15] = (s.mask_invert as u32 | (s.is_key as u32) << 1 | (s.black_bg as u32) << 2) as f32;
    u
}

/// Texture a slice samples and its size in pixels.
pub type SliceSrc<'a> = (&'a wgpu::TextureView, (u32, u32));

pub struct SliceRenderer {
    pipe: wgpu::RenderPipeline,
    /// Same shader with additive blending, for screens with `additive_overlap`.
    pipe_add: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    ubgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    nearest: wgpu::Sampler,
}

impl SliceRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("slices"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("slices_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let ubgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("slices_ubgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("slices_pl"),
            bind_group_layouts: &[Some(&bgl), Some(&ubgl)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2];
        let make = |blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("slices_pipe"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 24,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attrs,
                    })],
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
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let (pipe, pipe_add) = (
            make(None),
            make(Some(wgpu::BlendState {
                color: add,
                alpha: add,
            })),
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        Self {
            pipe,
            pipe_add,
            bgl,
            ubgl,
            sampler,
            nearest,
        }
    }

    /// Clears `target` to black (transparent with `additive_overlap`) and draws every slice of `screen`, each sampling the texture
    /// `src` returns for it (with its size). With `screen.pixel_perfect`, unscaled slices
    /// sample nearest.
    pub fn render<'a>(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &Screen,
        src: &dyn Fn(&Slice) -> SliceSrc<'a>,
        target: &wgpu::TextureView,
    ) {
        use wgpu::util::DeviceExt;
        let bind_with = |view: &wgpu::TextureView, smp: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("slices_bg"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(smp),
                    },
                ],
            })
        };
        type Drawn = (
            wgpu::Buffer,
            u32,
            wgpu::BindGroup,
            (wgpu::BindGroup, wgpu::BindGroup),
            bool,
        );
        // Off screen: the pass below still clears the target, so it is a black frame that
        // keeps flowing to every output (a frozen last frame on an LED processor is worse).
        let slices: &[Slice] = if screen.enabled { &screen.slices } else { &[] };
        let bufs: Vec<Drawn> = slices
            .iter()
            .filter_map(|s| {
                let v = mapping::slice_vertices_st(screen.width, screen.height, s);
                let bytes: Vec<u8> = v.iter().flatten().flat_map(|f| f.to_ne_bytes()).collect();
                (!v.is_empty()).then(|| {
                    let (view, src_size) = src(s);
                    let b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("slice_vb"),
                        contents: &bytes,
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                    let ub: Vec<u8> = slice_uniform(screen, s, src_size)
                        .iter()
                        .flat_map(|f| f.to_ne_bytes())
                        .collect();
                    let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("slice_ub"),
                        contents: &ub,
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                    let ubind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("slice_ubg"),
                        layout: &self.ubgl,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: ubuf.as_entire_binding(),
                        }],
                    });
                    (
                        b,
                        v.len() as u32,
                        ubind,
                        (
                            bind_with(view, &self.sampler),
                            bind_with(view, &self.nearest),
                        ),
                        screen.pixel_perfect
                            && sane_transform(&s.transform) == vjc_model::SliceTransform::default()
                            && mapping::slice_unscaled(src_size, s),
                    )
                })
            })
            .collect();
        // Additive screens start transparent (alpha 0 outside the slices); a disabled one
        // stays an opaque black frame.
        let additive = screen.additive_overlap && screen.enabled;
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("slices"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(if additive {
                            wgpu::Color::TRANSPARENT
                        } else {
                            wgpu::Color::BLACK
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(if additive { &self.pipe_add } else { &self.pipe });
            for (b, n, ubind, (bind, bind_nearest), near) in &bufs {
                pass.set_bind_group(0, if *near { bind_nearest } else { bind }, &[]);
                pass.set_bind_group(1, ubind, &[]);
                pass.set_vertex_buffer(0, b.slice(..));
                pass.draw(0..*n, 0..1);
            }
        }
        queue.submit(Some(enc.finish()));
    }
}

const STAMP_SHADER: &str = r#"
@group(0) @binding(0) var ov: texture_2d<f32>;
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(ov, vec2<i32>(pos.xy), 0);
}
"#;

/// Alpha-blends a straight-alpha RGBA image over a screen target, on the GPU, so what is
/// stamped (the test card) is part of the frame that Art-Net, Syphon and NDI read back.
pub struct Stamper {
    pipe: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    /// Per screen: size, geometry hash and uploaded card texture.
    cache: std::collections::HashMap<uuid::Uuid, ((u32, u32), u64, wgpu::TextureView)>,
}

impl Stamper {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stamp"),
            source: wgpu::ShaderSource::Wgsl(STAMP_SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stamp_bgl"),
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
            label: Some("stamp_pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stamp_pipe"),
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipe,
            bgl,
            cache: Default::default(),
        }
    }

    /// Drops cached cards of screens that no longer show one.
    pub fn retain_cards(&mut self, keep: impl Fn(&uuid::Uuid) -> bool) {
        self.cache.retain(|id, _| keep(id));
    }

    /// Draws the test card of `screen` over `target`. The card is static, so its texture
    /// is cached per screen and rebuilt (rasterize + upload) only when the size or the
    /// geometry hash changes.
    pub fn stamp_card(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        screen: &vjc_model::Screen,
        size: (u32, u32),
    ) {
        use wgpu::util::DeviceExt;
        let key = crate::output_overlay::card_key(screen);
        if self
            .cache
            .get(&screen.id)
            .is_none_or(|(s, k, _)| *s != size || *k != key)
        {
            let rgba = crate::output_overlay::rasterize(
                &crate::output_overlay::test_card(screen),
                size.0,
                size.1,
            );
            let t = device.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("stamp_tex"),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &rgba,
            );
            self.cache
                .insert(screen.id, (size, key, t.create_view(&Default::default())));
        }
        let Some((_, _, view)) = self.cache.get(&screen.id) else {
            return;
        };
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stamp_bg"),
            layout: &self.bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            }],
        });
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stamp"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(enc.finish()));
    }
}

/// Frame queue of one screen: `push` stores the live frame and writes into `out` the frame
/// from `n` pushes ago. `n == 0` is a plain copy (the app skips the ring then and renders
/// straight into `out`). Until `n` frames are stored, `out` is left as it was.
#[derive(Default)]
pub struct DelayRing {
    frames: std::collections::VecDeque<wgpu::Texture>,
    n: usize,
    size: (u32, u32),
}

impl DelayRing {
    pub fn push(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        live: &wgpu::Texture,
        out: &wgpu::Texture,
        size: (u32, u32),
        n: usize,
    ) {
        if self.n != n || self.size != size {
            self.frames.clear();
            self.n = n;
            self.size = size;
        }
        let ext = wgpu::Extent3d {
            width: size.0.max(1),
            height: size.1.max(1),
            depth_or_array_layers: 1,
        };
        let mut enc = device.create_command_encoder(&Default::default());
        let slot = if self.frames.len() >= n.max(1) {
            let oldest = self.frames.pop_front().expect("non-empty");
            enc.copy_texture_to_texture(oldest.as_image_copy(), out.as_image_copy(), ext);
            oldest
        } else {
            make_screen_target(device, size.0, size.1).0
        };
        enc.copy_texture_to_texture(live.as_image_copy(), slot.as_image_copy(), ext);
        self.frames.push_back(slot);
        queue.submit(Some(enc.finish()));
    }

    /// Drops the stored frames (screen off or delay back to 0).
    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

/// Render target for a screen.
pub fn make_screen_target(
    device: &wgpu::Device,
    w: u32,
    h: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let t = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screen"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders `screen` (w x h) from a 1x1 `src` pixel; None without an adapter.
    fn render_rgba(screen: &Screen, src_px: [u8; 4]) -> Option<Vec<u8>> {
        render_rgba_card(screen, src_px, false)
    }

    /// Same, optionally stamping the test card on the target like the app does.
    fn render_rgba_card(screen: &Screen, src_px: [u8; 4], card: bool) -> Option<Vec<u8>> {
        render_src(screen, &src_px, (1, 1), card)
    }

    /// Same from a `size` source image (RGBA bytes, row-major).
    fn render_src(screen: &Screen, src_px: &[u8], size: (u32, u32), card: bool) -> Option<Vec<u8>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return None;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no wgpu device: skipped");
            return None;
        };
        let (src, src_v) = make_screen_target(&device, size.0, size.1);
        let one = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        queue.write_texture(
            src.as_image_copy(),
            src_px,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * size.0),
                rows_per_image: Some(size.1),
            },
            one,
        );
        let (w, h) = (screen.width, screen.height);
        let (dst, dst_v) = make_screen_target(&device, w, h);
        SliceRenderer::new(&device).render(&device, &queue, screen, &|_| (&src_v, size), &dst_v);
        if card {
            Stamper::new(&device).stamp_card(&device, &queue, &dst_v, screen, (w, h));
        }
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            dst.as_image_copy(),
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
            out.extend_from_slice(&data[y * 256..y * 256 + w as usize * 4]);
        }
        Some(out)
    }

    fn px(img: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * w + x) * 4;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    fn readback(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: &wgpu::Texture,
        h: u32,
    ) -> Vec<u8> {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * h as u64,
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
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: 64,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let d = buf.slice(..).get_mapped_range().expect("map").to_vec();
        buf.unmap();
        d
    }

    /// A second frame with the same screen does not rasterize again and draws the same
    /// pixels as the first; a size change (negative control) invalidates the cache.
    #[test]
    fn card_cache_skips_rasterize_and_matches() {
        let Some((device, queue)) = gpu() else {
            return;
        };
        let calls = || crate::output_overlay::RASTERIZE_CALLS.with(|c| c.get());
        let mut screen = mapping::new_screen("t", 64, 64);
        screen.slices.push(mapping::new_slice(64, 64));
        let mut st = Stamper::new(&device);
        let (t1, v1) = make_screen_target(&device, 64, 64);
        let (t2, v2) = make_screen_target(&device, 64, 64);
        let c0 = calls();
        st.stamp_card(&device, &queue, &v1, &screen, (64, 64));
        assert_eq!(calls(), c0 + 1);
        st.stamp_card(&device, &queue, &v2, &screen, (64, 64));
        assert_eq!(calls(), c0 + 1, "unchanged card: no rasterize");
        let (a, b) = (
            readback(&device, &queue, &t1, 64),
            readback(&device, &queue, &t2, 64),
        );
        assert!(
            a.chunks(4).any(|p| p[3] != 0 && p[..3] != [0, 0, 0]),
            "card drawn"
        );
        assert_eq!(a, b, "cached frame equals fresh frame");
        screen.slices[0].output_mesh[0].0 += 3.0;
        st.stamp_card(&device, &queue, &v1, &screen, (64, 64));
        assert_eq!(calls(), c0 + 2, "geometry change invalidates");
        screen.width = 32;
        let (_t3, v3) = make_screen_target(&device, 32, 64);
        st.stamp_card(&device, &queue, &v3, &screen, (32, 64));
        assert_eq!(calls(), c0 + 3, "size change invalidates");
    }

    /// The frame Art-Net / NDI / Syphon read back carries the test card; without it, an
    /// empty screen is black (negative control); black-out wins over the card.
    #[test]
    fn gpu_test_card_reaches_sampled_frame() {
        let screen = mapping::new_screen("t", 64, 64);
        let (Some(on), Some(off)) = (
            render_rgba_card(&screen, [0, 0, 0, 255], true),
            render_rgba_card(&screen, [0, 0, 0, 255], false),
        ) else {
            return;
        };
        assert!(on.chunks(4).any(|p| p[..3] != [0, 0, 0]), "card is visible");
        assert_eq!(px(&on, 64, 0, 0), [255, 255, 255, 255], "border pixel");
        assert!(
            off.chunks(4).all(|p| p == [0, 0, 0, 255]),
            "no card, no content: black"
        );
        let mut black = on.clone();
        crate::capture::blacken(&mut black);
        assert!(
            black.chunks(4).all(|p| p == [0, 0, 0, 255]),
            "black-out + card: black"
        );
    }

    /// A disabled screen renders black even with the card requested.
    #[test]
    fn gpu_disabled_screen_stays_black_without_card_stamp() {
        let mut screen = mapping::new_screen("t", 8, 8);
        screen.slices.push(mapping::new_slice(8, 8));
        screen.enabled = false;
        if let Some(img) = render_rgba(&screen, [255, 255, 255, 255]) {
            assert!(img.chunks(4).all(|p| p == [0, 0, 0, 255]));
        }
    }

    /// An identity slice reproduces the composition pixel. Skipped without an adapter.
    #[test]
    fn gpu_identity_slice_one_pixel() {
        let mut screen = mapping::new_screen("t", 1, 1);
        screen.slices.push(mapping::new_slice(1, 1));
        if let Some(got) = render_rgba(&screen, [200, 100, 50, 255]) {
            assert_eq!(got, vec![200, 100, 50, 255]);
        }
    }

    /// 8x8 source whose pixel (x, y) is (30x, 30y, 0, 255).
    fn gradient_8() -> Vec<u8> {
        (0..64u32)
            .flat_map(|i| [(i % 8 * 30) as u8, (i / 8 * 30) as u8, 0, 255])
            .collect()
    }

    fn close(a: [u8; 4], b: [u8; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
    }

    /// The identity transform draws exactly the source, as before the transform existed.
    #[test]
    fn gpu_slice_transform_identity_matches_source() {
        let mut screen = mapping::new_screen("t", 8, 8);
        let mut sl = mapping::new_slice(8, 8);
        sl.transform = vjc_model::SliceTransform::default();
        screen.slices.push(sl);
        let src = gradient_8();
        if let Some(got) = render_src(&screen, &src, (8, 8), false) {
            for y in 0..8 {
                for x in 0..8 {
                    assert!(close(px(&got, 8, x, y), px(&src, 8, x, y)), "({x},{y})");
                }
            }
        }
    }

    /// Scale 2 shows the centre quarter of the source; the slice itself stays put.
    #[test]
    fn gpu_slice_transform_scale_two_shows_centre_quarter() {
        let mut screen = mapping::new_screen("t", 4, 4);
        let mut sl = mapping::new_slice(4, 4);
        sl.transform.scale = 2.0;
        screen.slices.push(sl);
        let src = gradient_8();
        if let Some(got) = render_src(&screen, &src, (8, 8), false) {
            for y in 0..4 {
                for x in 0..4 {
                    let want = px(&src, 8, x + 2, y + 2);
                    assert!(
                        close(px(&got, 4, x, y), want),
                        "({x},{y}) {:?}",
                        px(&got, 4, x, y)
                    );
                }
            }
        }
    }

    /// Shifting the content right by half the slice leaves transparent pixels.
    #[test]
    fn gpu_slice_transform_outside_source_is_transparent() {
        let mut screen = mapping::new_screen("t", 4, 4);
        let mut sl = mapping::new_slice(4, 4);
        sl.transform.offset = (0.5, 0.0);
        screen.slices.push(sl);
        let src = gradient_8();
        if let Some(got) = render_src(&screen, &src, (8, 8), false) {
            assert_eq!(
                px(&got, 4, 0, 0),
                [0, 0, 0, 0],
                "left edge is outside the source"
            );
            assert_eq!(
                px(&got, 4, 3, 0)[3],
                255,
                "right half still shows the source"
            );
        }
    }

    /// The cutoff is the slice's input area, not the whole texture: a cropped input never
    /// shows pixels from outside the crop.
    #[test]
    fn gpu_slice_transform_respects_cropped_input() {
        let mut screen = mapping::new_screen("t", 4, 4);
        let mut sl = mapping::new_slice(4, 4);
        sl.input = vec![(0.0, 0.0), (0.5, 0.0), (0.5, 1.0), (0.0, 1.0)];
        sl.transform.scale = 0.5;
        screen.slices.push(sl);
        // Left half grey, right half blue.
        let src: Vec<u8> = (0..64u32)
            .flat_map(|i| {
                if i % 8 < 4 {
                    [90, 90, 90, 255]
                } else {
                    [0, 0, 255, 255]
                }
            })
            .collect();
        if let Some(got) = render_src(&screen, &src, (8, 8), false) {
            // Rows 1 and 2 are inside the zoomed-out content; 0 and 3 are above and below it.
            for y in 1..3 {
                assert_eq!(px(&got, 4, 0, y), [0, 0, 0, 0], "left of the crop");
                assert!(close(px(&got, 4, 1, y), [90, 90, 90, 255]), "inside");
                assert!(close(px(&got, 4, 2, y), [90, 90, 90, 255]), "inside");
                assert_eq!(px(&got, 4, 3, y), [0, 0, 0, 0], "right half must not show");
            }
        }
    }

    /// Rotation 90 turns the content clockwise: the source's top-left corner ends up top-right.
    #[test]
    fn gpu_slice_transform_rotation_is_clockwise() {
        let mut screen = mapping::new_screen("t", 8, 8);
        let mut sl = mapping::new_slice(8, 8);
        sl.transform.rotation = 90.0;
        screen.slices.push(sl);
        let src = gradient_8();
        if let Some(got) = render_src(&screen, &src, (8, 8), false) {
            for y in 0..8 {
                for x in 0..8 {
                    let want = px(&src, 8, y, 7 - x);
                    assert!(close(px(&got, 8, x, y), want), "({x},{y})");
                }
            }
        }
    }

    /// A project file with a wild transform still renders sanely.
    #[test]
    fn transform_is_sanitized_for_the_shader() {
        let screen = mapping::new_screen("t", 4, 4);
        let tf = |scale, offset, rotation| {
            let mut sl = mapping::new_slice(4, 4);
            sl.transform = vjc_model::SliceTransform {
                scale,
                offset,
                rotation,
            };
            slice_uniform(&screen, &sl, (4, 4))[24..28].to_vec()
        };
        assert_eq!(tf(0.0, (0.0, 0.0), 0.0)[0], 0.1);
        assert_eq!(tf(-3.0, (0.0, 0.0), 0.0)[0], 0.1);
        assert_eq!(tf(99.0, (0.0, 0.0), 0.0)[0], 10.0);
        assert_eq!(tf(f32::NAN, (0.0, 0.0), 0.0)[0], 1.0);
        assert_eq!(
            tf(2.0, (f32::INFINITY, f32::NAN), f32::NAN),
            [2.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(tf(2.0, (0.25, -0.5), 0.0), [2.0, 0.25, -0.5, 0.0]);
    }

    /// A triangular mask cuts the pixel outside it.
    #[test]
    fn gpu_triangle_mask_cuts_outside() {
        let mut screen = mapping::new_screen("t", 4, 4);
        let mut sl = mapping::new_slice(4, 4);
        sl.mask = Some(vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]);
        screen.slices.push(sl);
        let Some(img) = render_rgba(&screen, [255, 255, 255, 255]) else {
            return;
        };
        assert_eq!(px(&img, 4, 0, 0), [255, 255, 255, 255]);
        assert_eq!(px(&img, 4, 3, 3), [0, 0, 0, 255]);
        assert_eq!(px(&img, 4, 2, 0)[0], 255, "x+y<1 kept");
        assert_eq!(px(&img, 4, 3, 1)[0], 0, "x+y>1 cut");
    }

    /// Gains scale the channels; the identity keeps them (negative control). Prints when skipped.
    #[test]
    fn gpu_slice_color_scales_channels() {
        let mut screen = mapping::new_screen("t", 1, 1);
        let mut sl = mapping::new_slice(1, 1);
        sl.color = vjc_model::SliceColor {
            brightness: 0.5,
            r: 1.0,
            g: 0.5,
            b: 0.0,
            contrast: 1.0,
        };
        screen.slices.push(sl);
        let Some(img) = render_rgba(&screen, [200, 200, 200, 255]) else {
            eprintln!("gpu_slice_color_scales_channels: SKIPPED (no adapter)");
            return;
        };
        let got = px(&img, 1, 0, 0);
        assert!((got[0] as i32 - 100).abs() <= 1, "{got:?}");
        assert!((got[1] as i32 - 50).abs() <= 1, "{got:?}");
        assert_eq!(got[2], 0);
        screen.slices[0].color = Default::default();
        let id = render_rgba(&screen, [200, 200, 200, 255]).unwrap();
        assert_eq!(px(&id, 1, 0, 0), [200, 200, 200, 255]);
    }

    /// Additive screens add overlapping slices and leave alpha 0 outside them; the default
    /// covers earlier slices over opaque black (negative control: the flag off).
    #[test]
    fn gpu_additive_overlap_adds_slices_and_clears_to_alpha_zero() {
        let src = [100, 50, 20, 255];
        let mut screen = mapping::new_screen("t", 4, 1);
        screen.slices = vec![mapping::new_slice(4, 1), mapping::new_slice(4, 1)];
        let Some(off) = render_rgba(&screen, src) else {
            return;
        };
        assert_eq!(px(&off, 4, 1, 0), src, "second slice covers the first");
        screen.additive_overlap = true;
        let on = render_rgba(&screen, src).unwrap();
        assert_eq!(px(&on, 4, 1, 0), [200, 100, 40, 255], "overlap adds up");
        // One slice over the left half: the right half is black (off) or alpha 0 (on).
        screen.slices.truncate(1);
        screen.slices[0].output_mesh = vec![(0.0, 0.0), (2.0, 0.0), (0.0, 1.0), (2.0, 1.0)];
        screen.slices[0].mesh_cols = 2;
        let on = render_rgba(&screen, src).unwrap();
        assert_eq!(px(&on, 4, 0, 0), src);
        assert_eq!(px(&on, 4, 3, 0), [0, 0, 0, 0], "outside is transparent");
        screen.additive_overlap = false;
        let off = render_rgba(&screen, src).unwrap();
        assert_eq!(px(&off, 4, 3, 0), [0, 0, 0, 255], "outside is black");
        // A disabled additive screen is still an opaque black frame.
        screen.additive_overlap = true;
        screen.enabled = false;
        let dis = render_rgba(&screen, src).unwrap();
        assert_eq!(px(&dis, 4, 0, 0), [0, 0, 0, 255]);
    }

    fn one_slice_screen() -> Screen {
        let mut screen = mapping::new_screen("t", 1, 1);
        screen.slices.push(mapping::new_slice(1, 1));
        screen
    }

    /// Screen brightness 0, opacity 0 and enabled=false all give black; a neutral screen
    /// colour keeps the pixel exactly (negative control: brightness 0.5 does change it).
    #[test]
    fn gpu_screen_color_and_enabled() {
        let src = [200, 120, 30, 255];
        let mut screen = one_slice_screen();
        let Some(base) = render_rgba(&screen, src) else {
            eprintln!("gpu_screen_color_and_enabled: SKIPPED (no adapter)");
            return;
        };
        assert_eq!(px(&base, 1, 0, 0), src, "neutral keeps the pixel");
        screen.color.color.brightness = 0.0;
        let img = render_rgba(&screen, src).unwrap();
        assert_eq!(px(&img, 1, 0, 0), [0, 0, 0, 255], "brightness 0");
        screen.color = Default::default();
        screen.color.opacity = 0.0;
        let img = render_rgba(&screen, src).unwrap();
        assert_eq!(px(&img, 1, 0, 0), [0, 0, 0, 255], "opacity 0");
        screen.color = Default::default();
        screen.enabled = false;
        let img = render_rgba(&screen, src).unwrap();
        assert_eq!(px(&img, 1, 0, 0), [0, 0, 0, 255], "disabled");
        screen.enabled = true;
        screen.color.color.brightness = 0.5;
        let img = render_rgba(&screen, src).unwrap();
        assert!(
            (px(&img, 1, 0, 0)[0] as i32 - 100).abs() <= 1,
            "half bright"
        );
    }

    /// Contrast 1.0 never changes a pixel (slice or screen); 2.0 stretches around grey.
    #[test]
    fn gpu_contrast_neutral_and_active() {
        let src = [200, 120, 30, 255];
        let mut screen = one_slice_screen();
        let Some(_) = render_rgba(&screen, src) else {
            eprintln!("gpu_contrast_neutral_and_active: SKIPPED (no adapter)");
            return;
        };
        screen.slices[0].color.contrast = 1.0;
        screen.color.color.contrast = 1.0;
        let img = render_rgba(&screen, src).unwrap();
        assert_eq!(px(&img, 1, 0, 0), src);
        screen.slices[0].color.contrast = 2.0;
        let img = render_rgba(&screen, src).unwrap();
        let got = px(&img, 1, 0, 0);
        assert!(got[0] > 200 && got[2] < 30, "stretched {got:?}");
        screen.slices[0].color.contrast = 1.0;
        screen.color.color.contrast = 0.0;
        let img = render_rgba(&screen, src).unwrap();
        assert!((px(&img, 1, 0, 0)[0] as i32 - 128).abs() <= 1, "flat grey");
    }

    /// Soft edge left 0.25 darkens the left border only.
    #[test]
    fn gpu_soft_edge_left_darkens_border() {
        let mut screen = mapping::new_screen("t", 8, 2);
        let mut sl = mapping::new_slice(8, 2);
        sl.soft_edge = Some(vjc_model::SoftEdge {
            left: 0.25,
            right: 0.0,
            top: 0.0,
            bottom: 0.0,
            gamma: 1.0,
        });
        screen.slices.push(sl);
        let Some(img) = render_rgba(&screen, [255, 255, 255, 255]) else {
            return;
        };
        // Pixel 0 centre is s = 1/16 -> ramp 0.25; pixel 1 -> 0.75; from pixel 2 on: full.
        let r = |x| px(&img, 8, x, 0)[0] as i32;
        assert!((r(0) - 64).abs() <= 2, "{}", r(0));
        assert!((r(1) - 191).abs() <= 2, "{}", r(1));
        assert_eq!((r(2), r(7)), (255, 255));
    }

    /// f32 to IEEE half bits (normal range, zero and one only; enough for the ramps below).
    fn f16_bits(v: f32) -> u16 {
        if v == 0.0 {
            return 0;
        }
        let b = v.to_bits();
        let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
        ((e as u32) << 10 | (b & 0x7f_ffff) >> 13) as u16
    }

    /// Renders a 1024 x 8 screen from a 1024 x 1 source of grey values `f(x)` (16-bit
    /// source when `deep`, else 8-bit), with `dither`. Returns the red channel of row 3.
    fn dither_row(deep: bool, dither: bool, f: &dyn Fn(usize) -> f32) -> Option<Vec<u8>> {
        let (device, queue) = gpu()?;
        let n = 1024u32;
        let (fmt, bytes): (_, Vec<u8>) = if deep {
            let b = (0..n as usize)
                .flat_map(|x| {
                    let h = f16_bits(f(x)).to_le_bytes();
                    let one = f16_bits(1.0).to_le_bytes();
                    [h, h, h, one].concat()
                })
                .collect();
            (wgpu::TextureFormat::Rgba16Float, b)
        } else {
            let b = (0..n as usize)
                .flat_map(|x| {
                    let v = (f(x) * 255.0).round() as u8;
                    [v, v, v, 255]
                })
                .collect();
            (wgpu::TextureFormat::Rgba8Unorm, b)
        };
        let src = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let px = bytes.len() as u32 / n;
        queue.write_texture(
            src.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(px * n),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let view = src.create_view(&Default::default());
        let mut screen = vjc_core::mapping::new_screen("t", n, 8);
        screen.slices.push(vjc_core::mapping::new_slice(n, 8));
        let (dst, dst_v) = make_screen_target(&device, n, 8);
        SliceRenderer::new(&device).render_dithered(
            &device,
            &queue,
            &screen,
            &|_| (&view, (n, 1)),
            &dst_v,
            dither,
        );
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 4096 * 8,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            dst.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4096),
                    rows_per_image: Some(8),
                },
            },
            wgpu::Extent3d {
                width: n,
                height: 8,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let d = buf.slice(..).get_mapped_range().expect("map").to_vec();
        buf.unmap();
        Some((0..n as usize).map(|x| d[3 * 4096 + x * 4]).collect())
    }

    fn longest_run(row: &[u8]) -> usize {
        let (mut best, mut cur) = (0, 0);
        for i in 0..row.len() {
            cur = if i > 0 && row[i] == row[i - 1] {
                cur + 1
            } else {
                1
            };
            best = best.max(cur);
        }
        best
    }

    fn ramp(x: usize) -> f32 {
        0.40 + 0.02 * (x as f32 + 0.5) / 1024.0
    }

    #[test]
    fn gpu_dither_off_bands_a_deep_ramp() {
        let Some(row) = dither_row(true, false, &ramp) else {
            return;
        };
        let mut distinct = row.clone();
        distinct.sort();
        distinct.dedup();
        assert!(distinct.len() <= 6, "{} distinct", distinct.len());
        assert!(longest_run(&row) > 100, "run {}", longest_run(&row));
    }

    #[test]
    fn gpu_dither_on_keeps_block_means_and_breaks_runs() {
        let Some(row) = dither_row(true, true, &ramp) else {
            return;
        };
        for b in 0..16 {
            let r = b * 64..(b + 1) * 64;
            let got = row[r.clone()].iter().map(|&v| v as f32).sum::<f32>() / 64.0 / 255.0;
            let want = r.map(ramp).sum::<f32>() / 64.0;
            assert!(
                (got - want).abs() <= 0.5 / 255.0,
                "block {b}: {got} vs {want}"
            );
        }
        assert!(longest_run(&row) <= 16, "run {}", longest_run(&row));
    }

    #[test]
    fn gpu_dither_flag_changes_nothing_for_an_8_bit_source() {
        let f = |x: usize| (x % 256) as f32 / 255.0;
        let (Some(a), Some(b)) = (dither_row(false, false, &f), dither_row(false, true, &f)) else {
            return;
        };
        assert_eq!(a, b);
    }

    #[test]
    fn gpu_dither_never_moves_black_or_white() {
        let f = |x: usize| if x < 512 { 0.0 } else { 1.0 };
        let Some(row) = dither_row(true, true, &f) else {
            return;
        };
        assert!(row[..512].iter().all(|&v| v == 0), "black moved");
        assert!(row[512..].iter().all(|&v| v == 255), "white moved");
    }

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    /// Reads the single pixel of a 1x1 screen texture.
    fn read1(device: &wgpu::Device, queue: &wgpu::Queue, t: &wgpu::Texture) -> [u8; 4] {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            t.as_image_copy(),
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
        let d = buf.slice(..).get_mapped_range().expect("map").to_vec();
        buf.unmap();
        [d[0], d[1], d[2], d[3]]
    }

    /// Writes frame `v` (grey) into `live`, runs `app_frame` like the app and returns the
    /// presented pixel: the plain path (n == 0) draws straight into `out`.
    fn feed(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ring: &mut DelayRing,
        live: &wgpu::Texture,
        out: &wgpu::Texture,
        v: u8,
        n: usize,
    ) -> [u8; 4] {
        let one = wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        };
        let layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        };
        let target = if n == 0 { out } else { live };
        queue.write_texture(target.as_image_copy(), &[v, v, v, 255], layout, one);
        if n > 0 {
            ring.push(device, queue, live, out, (1, 1), n);
        }
        read1(device, queue, out)
    }

    /// Delay 0 presents the current frame (as before); delay N presents the frame from N
    /// pushes ago and black until the queue fills (negative control: N=0 never lags).
    #[test]
    fn gpu_delay_zero_is_identity_and_n_lags_n_frames() {
        let Some((device, queue)) = gpu() else {
            eprintln!("gpu_delay: SKIPPED (no adapter)");
            return;
        };
        let (live, _) = make_screen_target(&device, 1, 1);
        let (out0, _) = make_screen_target(&device, 1, 1);
        let mut ring = DelayRing::default();
        for v in [10u8, 20, 30] {
            let got = feed(&device, &queue, &mut ring, &live, &out0, v, 0);
            assert_eq!(got, [v, v, v, 255], "delay 0 = current frame");
        }
        let (out, _) = make_screen_target(&device, 1, 1);
        let mut ring = DelayRing::default();
        let seen: Vec<u8> = (1..=6u8)
            .map(|v| feed(&device, &queue, &mut ring, &live, &out, v * 10, 3)[0])
            .collect();
        // Frames 1..3 fill the queue (output untouched: black); frame 4 shows frame 1.
        assert_eq!(seen, vec![0, 0, 0, 10, 20, 30]);
    }

    /// Black-out blackens what the delayed texture holds, right on the frame it is asked,
    /// without waiting for the queue.
    #[test]
    fn gpu_blackout_ignores_delay() {
        let Some((device, queue)) = gpu() else {
            eprintln!("gpu_blackout_ignores_delay: SKIPPED (no adapter)");
            return;
        };
        let (live, _) = make_screen_target(&device, 1, 1);
        let (out, _) = make_screen_target(&device, 1, 1);
        let mut ring = DelayRing::default();
        let mut last = [0; 4];
        for v in [50u8, 60, 70, 80] {
            last = feed(&device, &queue, &mut ring, &live, &out, v, 2);
        }
        assert_eq!(last[0], 60, "delayed content is on the output");
        crate::capture::blacken(&mut last);
        assert_eq!(&last[..3], &[0, 0, 0], "black-out is immediate");
    }

    /// Is Key paints the alpha as white / black; off keeps the colour (negative control).
    #[test]
    fn gpu_is_key_white_where_content_black_where_transparent() {
        let mut screen = one_slice_screen();
        screen.slices[0].is_key = true;
        let (Some(solid), Some(clear)) = (
            render_rgba(&screen, [200, 100, 50, 255]),
            render_rgba(&screen, [200, 100, 50, 0]),
        ) else {
            eprintln!("gpu_is_key: SKIPPED (no adapter)");
            return;
        };
        assert_eq!(px(&solid, 1, 0, 0), [255, 255, 255, 255]);
        assert_eq!(px(&clear, 1, 0, 0), [0, 0, 0, 255]);
        screen.slices[0].is_key = false;
        let off = render_rgba(&screen, [200, 100, 50, 255]).unwrap();
        assert_eq!(px(&off, 1, 0, 0), [200, 100, 50, 255]);
    }

    /// Black BG turns transparent areas black and opaque; opaque content is unchanged.
    #[test]
    fn gpu_black_bg_under_transparent_content() {
        let mut screen = one_slice_screen();
        let Some(plain) = render_rgba(&screen, [200, 100, 50, 0]) else {
            eprintln!("gpu_black_bg: SKIPPED (no adapter)");
            return;
        };
        assert_eq!(px(&plain, 1, 0, 0), [200, 100, 50, 0], "off: untouched");
        screen.slices[0].black_bg = true;
        let clear = render_rgba(&screen, [200, 100, 50, 0]).unwrap();
        assert_eq!(px(&clear, 1, 0, 0), [0, 0, 0, 255]);
        let solid = render_rgba(&screen, [200, 100, 50, 255]).unwrap();
        assert_eq!(px(&solid, 1, 0, 0), [200, 100, 50, 255]);
    }

    /// An inverted mask shows only outside the polygon; not inverted is the opposite.
    #[test]
    fn gpu_inverted_mask_inverts() {
        let mut screen = mapping::new_screen("t", 4, 4);
        let mut sl = mapping::new_slice(4, 4);
        sl.mask = Some(vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]);
        screen.slices.push(sl);
        let Some(normal) = render_rgba(&screen, [255, 255, 255, 255]) else {
            eprintln!("gpu_inverted_mask: SKIPPED (no adapter)");
            return;
        };
        screen.slices[0].mask_invert = true;
        let inv = render_rgba(&screen, [255, 255, 255, 255]).unwrap();
        assert_eq!(px(&normal, 4, 0, 0)[0], 255);
        assert_eq!(px(&inv, 4, 0, 0)[0], 0, "inside is cut");
        assert_eq!(px(&normal, 4, 3, 3)[0], 0);
        assert_eq!(px(&inv, 4, 3, 3)[0], 255, "outside is shown");
    }

    /// Writes a binary PPM of an RGBA readback to `target/ui-snapshots/` for eyeballing.
    fn snapshot(name: &str, img: &[u8], w: usize) {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        let mut ppm = format!("P6\n{w} {}\n255\n", img.len() / 4 / w).into_bytes();
        for p in img.chunks(4) {
            ppm.extend_from_slice(&p[..3]);
        }
        std::fs::write(dir.join(format!("{name}.ppm")), ppm).unwrap();
    }

    fn square() -> Screen {
        let mut screen = mapping::new_screen("t", 32, 32);
        screen.slices.push(mapping::new_slice(32, 32));
        screen
    }

    /// Rectangle shape: the whole screen is lit (baseline for the shapes below).
    #[test]
    fn gpu_snapshot_rect_shape_fills_the_slice() {
        let screen = square();
        let Some(img) = render_rgba(&screen, [255; 4]) else {
            return;
        };
        snapshot("slice-shape-rect", &img, 32);
        assert_eq!(px(&img, 32, 1, 1)[0], 255);
        assert_eq!(px(&img, 32, 30, 30)[0], 255);
    }

    /// Polygon shape: a triangle (top-left, top-right, bottom-left) lights its half only.
    #[test]
    fn gpu_snapshot_polygon_shape_cuts_outside() {
        let mut screen = square();
        screen.slices[0].shape =
            vjc_model::SliceShape::Polygon(vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]);
        let Some(img) = render_rgba(&screen, [255; 4]) else {
            return;
        };
        snapshot("slice-shape-polygon", &img, 32);
        assert_eq!(px(&img, 32, 4, 4)[0], 255, "inside");
        assert_eq!(px(&img, 32, 28, 28)[0], 0, "outside");
    }

    /// Bezier mesh: handles on the top edge bend it down, so a pixel near the top middle goes
    /// dark. Control: the same handles in Linear mode leave it lit.
    #[test]
    fn gpu_snapshot_bezier_mesh_bends_the_edge() {
        let mut screen = square();
        let sl = &mut screen.slices[0];
        sl.mesh_tangents = vec![
            ((0.0, 10.0), (0.0, 0.0)),
            ((0.0, -10.0), (0.0, 0.0)),
            ((0.0, 0.0), (0.0, 0.0)),
            ((0.0, 0.0), (0.0, 0.0)),
        ];
        let linear = render_rgba(&screen, [255; 4]);
        screen.slices[0].mesh_mode = vjc_model::MeshMode::Bezier;
        let (Some(linear), Some(bez)) = (linear, render_rgba(&screen, [255; 4])) else {
            return;
        };
        snapshot("slice-mesh-bezier", &bez, 32);
        assert_eq!(px(&linear, 32, 16, 3)[0], 255, "linear control");
        assert_eq!(px(&bez, 32, 16, 3)[0], 0, "bent edge");
        assert_eq!(px(&bez, 32, 16, 16)[0], 255, "centre stays lit");
        assert_eq!(px(&bez, 32, 1, 1)[0], 255, "corner stays lit");
    }

    /// Output mask: only the screen part inside it is shown, for every slice; a 63-point
    /// mask (the shared cap) still works.
    #[test]
    fn gpu_snapshot_output_mask_cuts_the_screen() {
        let mut screen = square();
        let left = vec![(0.0, 0.0), (0.5, 0.0), (0.5, 1.0), (0.0, 1.0)];
        screen.output_mask = Some(vjc_model::Mask::Polygon(left));
        let Some(img) = render_rgba(&screen, [255; 4]) else {
            return;
        };
        snapshot("output-mask", &img, 32);
        assert_eq!(px(&img, 32, 4, 16)[0], 255, "inside");
        assert_eq!(px(&img, 32, 28, 16)[0], 0, "outside");
        // 63 points on a circle: the centre is inside, the corner is not.
        let ring: Vec<(f32, f32)> = (0..63)
            .map(|i| {
                let a = i as f32 / 63.0 * std::f32::consts::TAU;
                (0.5 + 0.4 * a.cos(), 0.5 + 0.4 * a.sin())
            })
            .collect();
        screen.output_mask = Some(vjc_model::Mask::Path(ring));
        let img = render_rgba(&screen, [255; 4]).unwrap();
        assert_eq!(px(&img, 32, 16, 16)[0], 255);
        assert_eq!(px(&img, 32, 1, 1)[0], 0);
    }
}
