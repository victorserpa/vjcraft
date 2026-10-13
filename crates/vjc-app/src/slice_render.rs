//! Draws the slices of one output screen: each slice samples its input region of the
//! composition and paints it on a deformable mesh (two triangles per cell, UVs from
//! `vjc_core::mapping`). Soft edge and masks are not drawn yet.

use eframe::egui_wgpu::wgpu;
use vjc_model::Screen;

pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const SHADER: &str = r#"
struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@vertex
fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VO {
    var o: VO;
    o.pos = vec4(p, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@fragment
fn fs(in: VO) -> @location(0) vec4<f32> {
    return textureSample(src, smp, in.uv);
}
"#;

pub struct SliceRenderer {
    pipe: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("slices_pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("slices_pipe"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 16,
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
        Self { pipe, bgl, sampler }
    }

    /// Clears `target` to black and draws every slice of `screen` sampling `src`.
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &Screen,
        src: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        use wgpu::util::DeviceExt;
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("slices_bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let bufs: Vec<(wgpu::Buffer, u32)> = screen
            .slices
            .iter()
            .filter_map(|s| {
                let v = vjc_core::mapping::slice_vertices(screen.width, screen.height, s);
                let bytes: Vec<u8> = v.iter().flatten().flat_map(|f| f.to_ne_bytes()).collect();
                (!v.is_empty()).then(|| {
                    let b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("slice_vb"),
                        contents: &bytes,
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                    (b, v.len() as u32)
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
            pass.set_bind_group(0, &bind, &[]);
            for (b, n) in &bufs {
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

    /// An identity slice reproduces the composition pixel. Skipped without an adapter.
    #[test]
    fn gpu_identity_slice_one_pixel() {
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
        let (src, src_v) = make_screen_target(&device, 1, 1);
        queue.write_texture(
            src.as_image_copy(),
            &[200, 100, 50, 255],
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
        let mut screen = vjc_core::mapping::new_screen("t", 1, 1);
        screen.slices.push(vjc_core::mapping::new_slice(1, 1));
        let (dst, dst_v) = make_screen_target(&device, 1, 1);
        SliceRenderer::new(&device).render(&device, &queue, &screen, &src_v, &dst_v);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
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
        assert_eq!(got, vec![200, 100, 50, 255]);
    }
}
