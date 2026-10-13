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
    pts: array<vec4<f32>, 16>,  // two mask points per entry
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
@fragment
fn fs(in: VO) -> @location(0) vec4<f32> {
    let n = u32(u.misc.y);
    if (n >= 3u) {
        let p = in.pos.xy / u.misc.zw;
        var inside = false;
        var j = n - 1u;
        for (var i = 0u; i < n; i = i + 1u) {
            let a = pt(i);
            let b = pt(j);
            if ((a.y > p.y) != (b.y > p.y)
                && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x) {
                inside = !inside;
            }
            j = i;
        }
        if (!inside) {
            discard;
        }
    }
    let lin = ramp(in.st.x, u.edge.x) * ramp(1.0 - in.st.x, u.edge.y)
        * ramp(in.st.y, u.edge.z) * ramp(1.0 - in.st.y, u.edge.w);
    let c = textureSample(src, smp, in.uv);
    return vec4(c.rgb * pow(lin, u.misc.x), c.a);
}
"#;

/// Uniform block size: 2 vec4 + 16 vec4 mask points.
const UNIFORM_FLOATS: usize = 8 + 4 * (mapping::MAX_MASK_POINTS / 2);

/// Packs the per-slice uniform (see `U` in the shader).
fn slice_uniform(screen: &Screen, s: &Slice) -> Vec<f32> {
    let mut u = vec![0.0f32; UNIFORM_FLOATS];
    if let Some(e) = &s.soft_edge {
        u[..4].copy_from_slice(&[e.left, e.right, e.top, e.bottom]);
        u[4] = e.gamma;
    } else {
        u[4] = 1.0;
    }
    if let Some(m) = s.mask.as_ref().filter(|m| mapping::mask_valid(m)) {
        u[5] = m.len() as f32;
        for (i, p) in m.iter().enumerate() {
            u[8 + 2 * i] = p.0;
            u[9 + 2 * i] = p.1;
        }
    }
    u[6] = screen.width.max(1) as f32;
    u[7] = screen.height.max(1) as f32;
    u
}

pub struct SliceRenderer {
    pipe: wgpu::RenderPipeline,
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
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
        let nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        Self {
            pipe,
            bgl,
            ubgl,
            sampler,
            nearest,
        }
    }

    /// Clears `target` to black and draws every slice of `screen` sampling `src` (of size
    /// `src_size`). With `screen.pixel_perfect`, unscaled slices sample nearest.
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        (screen, src, src_size): (&Screen, &wgpu::TextureView, (u32, u32)),
        target: &wgpu::TextureView,
    ) {
        use wgpu::util::DeviceExt;
        let bind_with = |smp: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("slices_bg"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(smp),
                    },
                ],
            })
        };
        let (bind, bind_nearest) = (bind_with(&self.sampler), bind_with(&self.nearest));
        let bufs: Vec<(wgpu::Buffer, u32, wgpu::BindGroup, bool)> = screen
            .slices
            .iter()
            .filter_map(|s| {
                let v = mapping::slice_vertices_st(screen.width, screen.height, s);
                let bytes: Vec<u8> = v.iter().flatten().flat_map(|f| f.to_ne_bytes()).collect();
                (!v.is_empty()).then(|| {
                    let b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("slice_vb"),
                        contents: &bytes,
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                    let ub: Vec<u8> = slice_uniform(screen, s)
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
                        screen.pixel_perfect && mapping::slice_unscaled(src_size, s),
                    )
                })
            })
            .collect();
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("slices"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            });
            pass.set_pipeline(&self.pipe);
            for (b, n, ubind, near) in &bufs {
                pass.set_bind_group(0, if *near { &bind_nearest } else { &bind }, &[]);
                pass.set_bind_group(1, ubind, &[]);
                pass.set_vertex_buffer(0, b.slice(..));
                pass.draw(0..*n, 0..1);
            }
        }
        queue.submit(Some(enc.finish()));
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
        let (src, src_v) = make_screen_target(&device, 1, 1);
        let one = wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        };
        queue.write_texture(
            src.as_image_copy(),
            &src_px,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            one,
        );
        let (w, h) = (screen.width, screen.height);
        let (dst, dst_v) = make_screen_target(&device, w, h);
        SliceRenderer::new(&device).render(&device, &queue, (screen, &src_v, (1, 1)), &dst_v);
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

    /// An identity slice reproduces the composition pixel. Skipped without an adapter.
    #[test]
    fn gpu_identity_slice_one_pixel() {
        let mut screen = mapping::new_screen("t", 1, 1);
        screen.slices.push(mapping::new_slice(1, 1));
        if let Some(got) = render_rgba(&screen, [200, 100, 50, 255]) {
            assert_eq!(got, vec![200, 100, 50, 255]);
        }
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
}
