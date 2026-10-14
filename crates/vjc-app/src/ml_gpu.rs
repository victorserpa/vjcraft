//! GPU side of the ML effects: per effect instance a small texture the input is reduced
//! into, a pool of readback buffers (see `capture`) and the R8 mask the effect shader reads.
//! Nothing here waits: readbacks complete on later polls and a missing mask just means
//! the effect is skipped until the first one arrives.

use crate::capture::{self, MapState, OK, PENDING};
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

/// Readback buffers per instance: one in flight while the next frame is reduced.
const BUFFERS: usize = 2;
/// Renders an instance may go unused before its GPU resources are freed.
const IDLE_FRAMES: u64 = 300;

/// An effect that needs a mask from an ML worker.
#[derive(Clone, Debug, PartialEq)]
pub struct MlDraw {
    /// Identifies the instance (the effect id).
    pub key: u128,
    /// Changes when the source feeding the effect changes (new clip): resets the model state.
    pub tag: u64,
    /// Network input size the frame is reduced to.
    pub size: (u32, u32),
}

/// A reduced frame ready for the worker.
pub struct MlFrame {
    pub key: u128,
    pub tag: u64,
    pub size: (u32, u32),
    pub rgba: Vec<u8>,
}

/// Reduces the effect input to the network size. `uv` is ISF-style (y up), the result is
/// stored top row first like every image.
const BLIT_FS: &str = r#"
@group(0) @binding(1) var img: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@fragment
fn main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return textureSampleLevel(img, smp, vec2(uv.x, 1.0 - uv.y), 0.0);
}
"#;

struct Buf {
    buf: wgpu::Buffer,
    flight: Option<(MapState, u64)>,
}

struct Mask {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
}

struct Gpu {
    size: (u32, u32),
    small: wgpu::Texture,
    small_view: wgpu::TextureView,
    bufs: Vec<Buf>,
    mask: Option<Mask>,
    last_used: u64,
}

impl Gpu {
    fn new(device: &wgpu::Device, size: (u32, u32)) -> Self {
        let small = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ml_small"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            size,
            small_view: small.create_view(&Default::default()),
            small,
            bufs: (0..BUFFERS)
                .map(|_| Buf {
                    buf: capture::readback_buffer(device, size, "ml_readback"),
                    flight: None,
                })
                .collect(),
            mask: None,
            last_used: 0,
        }
    }
}

pub struct MlStore {
    gpus: HashMap<u128, Gpu>,
    /// Instances whose reduced frame was rendered this frame and wants a readback.
    want: Vec<(u128, u64)>,
    frame: u64,
    blit: wgpu::RenderPipeline,
    blit_buf: wgpu::Buffer,
}

impl MlStore {
    pub fn new(
        device: &wgpu::Device,
        vs: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
    ) -> Self {
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ml_blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_FS.into()),
        });
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ml_blit"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: vs,
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
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let blit_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ml_blit_u"),
            size: 16 * 16,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        Self {
            gpus: HashMap::new(),
            want: vec![],
            frame: 0,
            blit,
            blit_buf,
        }
    }

    /// Called while an ML effect is encoded: reduces `src` for the worker when a readback
    /// buffer is free, and returns the mask view (None until the first mask arrived, then
    /// the effect is skipped).
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        src: &wgpu::TextureView,
        m: &MlDraw,
    ) -> Option<&wgpu::TextureView> {
        let g = self
            .gpus
            .entry(m.key)
            .or_insert_with(|| Gpu::new(device, m.size));
        if g.size != m.size {
            *g = Gpu::new(device, m.size);
        }
        g.last_used = self.frame;
        let free = g.bufs.iter().any(|b| b.flight.is_none());
        if free && !self.want.iter().any(|w| w.0 == m.key) {
            crate::compositor::draw_pass(
                device,
                enc,
                bgl,
                sampler,
                &self.blit,
                &self.blit_buf,
                src,
                &g.small_view,
                src,
                src,
            );
            self.want.push((m.key, m.tag));
        }
        g.mask.as_ref().map(|k| &k.view)
    }

    /// After the frame was submitted: starts the readbacks of the reduced frames.
    pub fn dispatch(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.frame += 1;
        for (key, tag) in std::mem::take(&mut self.want) {
            let Some(g) = self.gpus.get_mut(&key) else {
                continue;
            };
            if let Some(b) = g.bufs.iter_mut().find(|b| b.flight.is_none()) {
                let st = capture::start_readback_into(device, queue, &g.small, g.size, &b.buf);
                b.flight = Some((st, tag));
            }
        }
        let now = self.frame;
        self.gpus.retain(|_, g| {
            now - g.last_used < IDLE_FRAMES || g.bufs.iter().any(|b| b.flight.is_some())
        });
    }

    /// Reduced frames whose readback finished. Never waits.
    pub fn poll(&mut self, device: &wgpu::Device) -> Vec<MlFrame> {
        let _ = device.poll(wgpu::PollType::Poll);
        let mut out = vec![];
        for (key, g) in &mut self.gpus {
            for b in &mut g.bufs {
                let Some((st, tag)) = &b.flight else { continue };
                let s = st.load(Ordering::Acquire);
                if s == PENDING {
                    continue;
                }
                if s == OK {
                    if let Ok(data) = b.buf.slice(..).get_mapped_range() {
                        out.push(MlFrame {
                            key: *key,
                            tag: *tag,
                            size: g.size,
                            rgba: capture::unpad(&data, g.size.0, g.size.1),
                        });
                    }
                    b.buf.unmap();
                }
                b.flight = None;
            }
        }
        out
    }

    /// Uploads a finished mask (one byte per pixel, top row first) for an instance.
    pub fn set_mask(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: u128,
        size: (u32, u32),
        data: &[u8],
    ) {
        let Some(g) = self.gpus.get_mut(&key) else {
            return;
        };
        if data.len() != size.0 as usize * size.1 as usize || size.0 == 0 || size.1 == 0 {
            return;
        }
        if g.mask.as_ref().is_none_or(|m| m.size != size) {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ml_mask"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            g.mask = Some(Mask { tex, view, size });
        }
        if let Some(m) = &g.mask {
            queue.write_texture(
                m.tex.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size.0),
                    rows_per_image: Some(size.1),
                },
                wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}
