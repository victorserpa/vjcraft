//! GPU side of HAP playback: block-compressed frames are uploaded as BC textures and a
//! small pass writes them (YCoCg converted when needed) into the layer's Rgba8 texture,
//! so the rest of the compositor does not know the clip was HAP.

use std::collections::HashMap;

use eframe::egui_wgpu::wgpu;
use vjc_media::hap::{HapFormat, HapFrame};
use vjc_model::LayerId;

const SHADER: &str = r#"
struct U { mode: u32, _a: u32, _b: u32, _c: u32 }
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var main_tex: texture_2d<f32>;
@group(0) @binding(2) var alpha_tex: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let c = textureLoad(main_tex, p, 0);
    if (u.mode == 0u) { return c; }
    if (u.mode == 1u) { return vec4<f32>(c.rgb, 1.0); }
    if (u.mode == 4u) { return vec4<f32>(1.0, 1.0, 1.0, c.r); }
    // Scaled YCoCg (Hap Q): R = Co, G = Cg, B = scale, A = Y.
    let scale = (c.b * (255.0 / 8.0)) + 1.0;
    let co = (c.r - 0.50196078431373) / scale;
    let cg = (c.g - 0.50196078431373) / scale;
    let y = c.a;
    let rgb = clamp(vec3<f32>(y + co - cg, y + cg, y - co - cg), vec3<f32>(0.0), vec3<f32>(1.0));
    var a = 1.0;
    if (u.mode == 3u) { a = textureLoad(alpha_tex, p, 0).r; }
    return vec4<f32>(rgb, a);
}
"#;

struct Slot {
    key: (HapFormat, u32, u32),
    color: wgpu::Texture,
    alpha: Option<wgpu::Texture>,
    bind: wgpu::BindGroup,
}

pub struct HapGpu {
    pipe: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    /// 4x4 stand-in bound where a format has no alpha plane.
    dummy: wgpu::Texture,
    uniform: wgpu::Buffer,
    slots: HashMap<LayerId, Slot>,
}

/// True when the device can sample BC textures.
pub fn supported(device: &wgpu::Device) -> bool {
    device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
}

fn bc_format(f: HapFormat, plane: usize) -> wgpu::TextureFormat {
    match (f, plane) {
        (_, 1) => wgpu::TextureFormat::Bc4RUnorm,
        (HapFormat::Dxt1, _) => wgpu::TextureFormat::Bc1RgbaUnorm,
        (HapFormat::AlphaOnly, _) => wgpu::TextureFormat::Bc4RUnorm,
        (HapFormat::Bc7, _) => wgpu::TextureFormat::Bc7RgbaUnorm,
        _ => wgpu::TextureFormat::Bc3RgbaUnorm,
    }
}

fn mode(f: HapFormat) -> u32 {
    match f {
        HapFormat::Dxt5 | HapFormat::Bc7 => 0,
        HapFormat::Dxt1 => 1,
        HapFormat::YcoCg => 2,
        HapFormat::YcoCgAlpha => 3,
        HapFormat::AlphaOnly => 4,
    }
}

fn plane_texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    w: u32,
    h: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hap"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn upload(queue: &wgpu::Queue, tex: &wgpu::Texture, data: &[u8], (w, h): (u32, u32), block: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w / 4 * block),
            rows_per_image: Some(h / 4),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

impl HapGpu {
    /// `None` when the device cannot sample BC textures.
    pub fn new(device: &wgpu::Device) -> Option<HapGpu> {
        if !supported(device) {
            return None;
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hap"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
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
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hap_bgl"),
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
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hap_pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hap"),
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
                    format: crate::compositor::FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let dummy = plane_texture(device, wgpu::TextureFormat::Bc4RUnorm, 4, 4);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hap_u"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(HapGpu {
            pipe,
            bgl,
            dummy,
            uniform,
            slots: HashMap::new(),
        })
    }

    /// Drops the textures of layers that are no longer playing HAP.
    pub fn retain(&mut self, live: &[LayerId]) {
        self.slots.retain(|k, _| live.contains(k));
    }

    /// Uploads `frame` and renders it (cropped to the picture size) into `target`, a
    /// `frame.width` x `frame.height` view of the layer texture.
    pub fn decode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: LayerId,
        frame: &HapFrame,
        target: &wgpu::TextureView,
    ) {
        let key = (frame.format, frame.tex_w, frame.tex_h);
        if self.slots.get(&id).is_none_or(|s| s.key != key) {
            let color = plane_texture(device, bc_format(frame.format, 0), frame.tex_w, frame.tex_h);
            let alpha = frame.alpha.is_some().then(|| {
                plane_texture(device, bc_format(frame.format, 1), frame.tex_w, frame.tex_h)
            });
            let cv = color.create_view(&Default::default());
            let av = alpha
                .as_ref()
                .unwrap_or(&self.dummy)
                .create_view(&Default::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("hap_bg"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&cv),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&av),
                    },
                ],
            });
            self.slots.insert(
                id,
                Slot {
                    key,
                    color,
                    alpha,
                    bind,
                },
            );
        }
        let slot = &self.slots[&id];
        let block = frame.format.block_bytes() as u32;
        upload(
            queue,
            &slot.color,
            &frame.color,
            (frame.tex_w, frame.tex_h),
            block,
        );
        if let (Some(tex), Some(data)) = (&slot.alpha, &frame.alpha) {
            upload(queue, tex, data, (frame.tex_w, frame.tex_h), 8);
        }
        // One uniform shared by all layers: written and consumed in submission order, so
        // each layer submits its own pass right after its write.
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::cast_slice(&[mode(frame.format), 0, 0, 0]),
        );
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hap_decode"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hap_decode"),
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
            pass.set_bind_group(0, &slot.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([enc.finish()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_media::hap::testgen::{Kind, write_clip};

    static FLAG: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Reports a skipped GPU test loudly (`cargo test -- --nocapture` shows it). With
    /// `VJC_REQUIRE_GPU=1` (CI with a GPU) a missing capability fails instead.
    fn skipped(why: &str) {
        assert!(
            std::env::var_os("VJC_REQUIRE_GPU").is_none(),
            "GPU test cannot run: {why} (VJC_REQUIRE_GPU is set)"
        );
        eprintln!("SKIPPED (not passed): GPU test did not run: {why}");
    }

    fn device(bc: bool) -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            skipped("no wgpu adapter");
            return None;
        };
        let want = if bc {
            adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC
        } else {
            wgpu::Features::empty()
        };
        if bc && want.is_empty() {
            skipped("adapter has no BC texture support");
            return None;
        }
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: want,
            ..Default::default()
        }))
        .ok()
    }

    fn read(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (tex.width(), tex.height());
        let row = (w * 4).div_ceil(256) * 256;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * h),
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
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = buf.slice(..).get_mapped_range().unwrap();
        let mut out = vec![];
        for y in 0..h as usize {
            out.extend_from_slice(&data[y * row as usize..][..(w * 4) as usize]);
        }
        out
    }

    fn first_hap_frame(path: &std::path::Path) -> vjc_media::Frame {
        vjc_media::hap::set_gpu_supported(true);
        let pl = vjc_media::Player::open_scrub(path);
        let t = std::time::Instant::now();
        let f = loop {
            if let Some(f) = pl.latest_frame() {
                break f;
            }
            assert!(t.elapsed().as_secs() < 10, "{:?}", pl.error());
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        vjc_media::hap::set_gpu_supported(false);
        f
    }

    /// Max channel error of the GPU path against FFmpeg's RGBA decode of the same file.
    fn max_error(kind: Kind, chunks: usize) -> Option<u8> {
        let (device, queue) = device(true)?;
        let dir = std::env::temp_dir().join(format!("vjc-hapgpu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("t{}_{chunks}.mov", kind as u8));
        // 62x34: not multiples of 4, so padding and cropping are exercised.
        write_clip(&path, kind, (62, 34), 4, chunks).unwrap();
        let reference = vjc_media::decode_n(&path, 1).unwrap().remove(0);
        let f = first_hap_frame(&path);
        let hap = f.hap.clone().expect("compressed frame");
        assert_eq!((hap.width, hap.height), (62, 34));
        assert_eq!((hap.tex_w, hap.tex_h), (64, 36));
        let mut gpu = HapGpu::new(&device).unwrap();
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 62,
                height: 34,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::compositor::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        gpu.decode(&device, &queue, LayerId::nil(), &hap, &view);
        let got = read(&device, &queue, &target);
        assert_eq!(got.len(), reference.data.len());
        Some(
            got.iter()
                .zip(reference.data.iter())
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap(),
        )
    }

    #[test]
    fn gpu_decode_matches_ffmpeg_rgba() {
        let _g = FLAG.lock().unwrap_or_else(|e| e.into_inner());
        for (kind, limit) in [(Kind::Hap, 8), (Kind::HapAlpha, 8), (Kind::HapQ, 12)] {
            for chunks in [1, 3] {
                let Some(err) = max_error(kind, chunks) else {
                    return;
                };
                eprintln!("kind {} chunks {chunks}: max error {err}", kind as u8);
                assert!(
                    err <= limit,
                    "kind {} chunks {chunks}: error {err}",
                    kind as u8
                );
            }
        }
    }

    /// The HAP frame goes through the compositor's real layer slot (same texture, format
    /// and usage the app uses), with validation errors captured, for an 8-bit and a 16-bit
    /// composition.
    #[test]
    fn hap_frame_renders_into_the_real_compositor_layer_slot() {
        use crate::compositor::{Compositor, LayerDraw};
        let _g = FLAG.lock().unwrap_or_else(|e| e.into_inner());
        let Some((device, queue)) = device(true) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("vjc-hapgpu-slot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slot.mov");
        write_clip(&path, Kind::Hap, (62, 34), 3, 2).unwrap();
        let reference = vjc_media::decode_n(&path, 1).unwrap().remove(0);
        let hap = first_hap_frame(&path)
            .hap
            .clone()
            .expect("compressed frame");
        let id = LayerId::from_u128(1);
        for fmt in [crate::compositor::FORMAT, wgpu::TextureFormat::Rgba16Float] {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut comp = Compositor::with_format(&device, 62, 34, fmt);
            comp.hap_in.insert(id, hap.clone());
            // The slot is made from this placeholder; the HAP pass overwrites it.
            let blank = vec![0u8; 62 * 34 * 4];
            let layer = [LayerDraw {
                id,
                opacity: 1.0,
                blend: Default::default(),
                fit: Default::default(),
                effects: vec![],
                frame: Some((62, 34, &blank, 1)),
                generator: None,
                transition: None,
                transform: crate::compositor::Affine::IDENTITY,
                channels: Default::default(),
                mask: Default::default(),
                orient: Default::default(),
            }];
            comp.render(&device, &queue, &layer, &[], 1.0);
            let (w, h, got) = comp.read_rgba(&device, &queue).unwrap();
            let err = pollster::block_on(scope.pop());
            assert!(err.is_none(), "{fmt:?}: validation error {err:?}");
            assert_eq!((w, h), (62, 34));
            let max = got
                .iter()
                .zip(reference.data.iter())
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(max <= 16, "{fmt:?}: pixels differ by {max}");
        }
    }

    /// A HAP layer with an orientation plays turned: the compressed frame is decoded on the
    /// GPU and turned in the compositor, like any other picture.
    #[test]
    fn hap_frame_is_turned_on_the_gpu() {
        use crate::compositor::{Compositor, LayerDraw};
        let _g = FLAG.lock().unwrap_or_else(|e| e.into_inner());
        let Some((device, queue)) = device(true) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("vjc-hapgpu-turn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("turn.mov");
        write_clip(&path, Kind::Hap, (62, 34), 3, 2).unwrap();
        let reference = vjc_media::decode_n(&path, 1).unwrap().remove(0);
        let hap = first_hap_frame(&path)
            .hap
            .clone()
            .expect("compressed frame");
        let id = LayerId::from_u128(1);
        for code in 1..8 {
            let orient = vjc_media::Orient::from_code(code);
            let want = vjc_media::Frame {
                orient,
                ..reference.clone()
            };
            let (lw, lh) = want.display_size();
            let want = want.oriented();
            let mut comp = Compositor::new(&device, lw, lh);
            comp.hap_in.insert(id, hap.clone());
            let blank = vec![0u8; 62 * 34 * 4];
            let layer = [LayerDraw {
                id,
                opacity: 1.0,
                blend: Default::default(),
                fit: vjc_model::FitMode::Stretch,
                effects: vec![],
                frame: Some((62, 34, &blank, 1)),
                generator: None,
                transition: None,
                transform: crate::compositor::Affine::IDENTITY,
                channels: Default::default(),
                mask: Default::default(),
                orient,
            }];
            comp.render(&device, &queue, &layer, &[], 1.0);
            let (w, h, got) = comp.read_rgba(&device, &queue).unwrap();
            assert_eq!((w, h), (lw, lh), "orientation {code}");
            let max = got
                .iter()
                .zip(want.data.iter())
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(max <= 16, "orientation {code}: pixels differ by {max}");
        }
    }

    #[test]
    fn without_bc_feature_there_is_no_gpu_path_and_frames_still_arrive() {
        let _g = FLAG.lock().unwrap_or_else(|e| e.into_inner());
        let Some((device, _)) = device(false) else {
            return;
        };
        assert!(!supported(&device));
        assert!(HapGpu::new(&device).is_none());
        let dir = std::env::temp_dir().join(format!("vjc-hapgpu-fb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fb.mov");
        write_clip(&path, Kind::HapAlpha, (62, 34), 3, 2).unwrap();
        vjc_media::hap::set_gpu_supported(supported(&device));
        let pl = vjc_media::Player::open_scrub(&path);
        let t = std::time::Instant::now();
        let f = loop {
            if let Some(f) = pl.latest_frame() {
                break f;
            }
            assert!(t.elapsed().as_secs() < 10, "{:?}", pl.error());
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(f.hap.is_none());
        assert_eq!(f.data.len(), 62 * 34 * 4);
    }
}
