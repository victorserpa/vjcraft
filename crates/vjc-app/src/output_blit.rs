//! Blit of the composition texture (any filterable or float format, e.g. 16-bit) to an
//! output target of another format, such as an `Rgb10a2Unorm` surface.
// Not wired yet: eframe owns the output windows' surfaces (see docs/specs/ten-bit-output.md).
#![allow(dead_code)]

use eframe::egui_wgpu::wgpu;

const SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy / vec2<f32>(textureDimensions(src));
    return textureSampleLevel(src, smp, uv, 0.0);
}
"#;

/// Draws a texture onto a render target of a fixed format.
pub struct OutputBlit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl OutputBlit {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("output blit"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("output blit"),
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
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("output blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("output blit"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
        }
    }

    /// Records a full-target copy of `src` into `dst` on `enc` (same size: 1:1 texels).
    pub fn blit(
        &self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
    ) {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("output blit"),
            layout: &self.layout,
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
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("output blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return None;
        };
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    fn f16_bits(x: f32) -> u16 {
        if x == 0.0 {
            return 0;
        }
        let b = x.to_bits();
        let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
        assert!(
            (1..31).contains(&e),
            "gradient value out of f16 normal range"
        );
        ((e as u16) << 10) | ((b >> 13) & 0x3ff) as u16
    }

    const W: u32 = 1024;

    /// Blits a W-step 16-bit gradient into `fmt` and returns the raw texels (4 bytes each).
    fn run(device: &wgpu::Device, queue: &wgpu::Queue, fmt: wgpu::TextureFormat) -> Vec<u8> {
        let mk = |format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: W,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let src = mk(
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let dst = mk(
            fmt,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let mut texels = Vec::new();
        for i in 0..W {
            let v = f16_bits(i as f32 / (W - 1) as f32);
            for h in [v, v, v, f16_bits(1.0)] {
                texels.extend_from_slice(&h.to_le_bytes());
            }
        }
        queue.write_texture(
            src.as_image_copy(),
            &texels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 8),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: W,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let blit = OutputBlit::new(device, fmt);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (W * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        blit.blit(
            device,
            &mut enc,
            &src.create_view(&Default::default()),
            &dst.create_view(&Default::default()),
        );
        enc.copy_texture_to_buffer(
            dst.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(W * 4),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: W,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        slice.get_mapped_range().unwrap().to_vec()
    }

    fn distinct_red_10(raw: &[u8]) -> usize {
        let mut s: Vec<u32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) & 0x3ff)
            .collect();
        s.sort_unstable();
        s.dedup();
        s.len()
    }

    /// A 16-bit gradient blitted to Rgb10a2 keeps more than 256 levels (no banding).
    /// Control: the same blit to an 8-bit target cannot exceed 256.
    #[test]
    fn gpu_ten_bit_blit_has_more_than_256_levels() {
        let Some((device, queue)) = gpu() else { return };
        let ten = run(&device, &queue, wgpu::TextureFormat::Rgb10a2Unorm);
        let n10 = distinct_red_10(&ten);
        assert!(n10 > 256, "10-bit gradient has only {n10} levels");
        let eight = run(&device, &queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut r: Vec<u8> = eight.as_chunks::<4>().0.iter().map(|c| c[0]).collect();
        r.sort_unstable();
        r.dedup();
        assert!(r.len() <= 256);
        assert!(
            r.len() > 200,
            "8-bit blit produced no gradient: {}",
            r.len()
        );
    }
}
