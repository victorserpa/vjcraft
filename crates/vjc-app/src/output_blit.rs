//! Blit of the composition texture (any filterable or float format, e.g. 16-bit) to an
//! output target of another format, such as an `Rgb10a2Unorm` surface.

use eframe::egui_wgpu::wgpu;

/// Draws a texture onto a render target of a fixed format.
pub struct OutputBlit {
    _format: wgpu::TextureFormat,
}

impl OutputBlit {
    pub fn new(_device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self { _format: format }
    }

    /// Records a full-target copy of `src` into `dst` on `enc`.
    pub fn blit(
        &self,
        _device: &wgpu::Device,
        _enc: &mut wgpu::CommandEncoder,
        _src: &wgpu::TextureView,
        _dst: &wgpu::TextureView,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default()))
        else {
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
        assert!((1..31).contains(&e), "gradient value out of f16 normal range");
        ((e as u16) << 10) | ((b >> 13) & 0x3ff) as u16
    }

    const W: u32 = 1024;

    /// Blits a W-step 16-bit gradient into `fmt` and returns the raw texels (4 bytes each).
    fn run(device: &wgpu::Device, queue: &wgpu::Queue, fmt: wgpu::TextureFormat) -> Vec<u8> {
        let mk = |format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: W, height: 1, depth_or_array_layers: 1 },
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
            wgpu::Extent3d { width: W, height: 1, depth_or_array_layers: 1 },
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
            wgpu::Extent3d { width: W, height: 1, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = slice.get_mapped_range().unwrap().to_vec();
        data
    }

    fn distinct_red_10(raw: &[u8]) -> usize {
        let mut s: Vec<u32> = raw
            .chunks_exact(4)
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
        let mut r: Vec<u8> = eight.chunks_exact(4).map(|c| c[0]).collect();
        r.sort_unstable();
        r.dedup();
        assert!(r.len() <= 256);
        assert!(r.len() > 200, "8-bit blit produced no gradient: {}", r.len());
    }
}
