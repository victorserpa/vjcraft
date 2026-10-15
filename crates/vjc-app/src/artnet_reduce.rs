//! GPU reduction for Art-Net output: a compute pass averages each fixture's input rectangle
//! onto its pixel grid (fixture width x height), so only `sum(w*h)*4` bytes are read back
//! instead of the whole screen. Same sampling as `vjc_core::dmx::map_pixels` (box average for
//! the default input, 3x3 points for a moved/scaled/rotated one). Colour, gamma, flips,
//! serpentine and channel order stay on the CPU: the worker runs `map_pixels` on the reduced
//! grid with a full-screen input, where every cell is one pixel.

use crate::capture::{MapState, map_read};
use eframe::egui_wgpu::wgpu;
use vjc_model::{Fixture, FixtureInput};

/// More fixtures than this fall back to reading the whole screen.
pub const MAX_FIXTURES: usize = 32;
/// Reduced output above this many bytes falls back too.
const MAX_BYTES: u64 = 64 << 20;

const SHADER: &str = r"
struct Fx {
    off: u32, w: u32, h: u32, plain: u32,
    ix: f32, iy: f32, iw: f32, ih: f32,
    sin_r: f32, cos_r: f32, pad0: u32, pad1: u32,
};
struct Params { count: u32, total: u32, sw: u32, sh: u32 };
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> fxs: array<Fx>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read_write> out: array<u32>;

fn load8(x: u32, y: u32) -> vec3<u32> {
    let c = textureLoad(tex, vec2<i32>(i32(x), i32(y)), 0);
    return vec3<u32>(round(c.rgb * 255.0));
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.total) { return; }
    var k = 0u;
    for (var j = 0u; j < params.count; j++) {
        if (i >= fxs[j].off) { k = j; }
    }
    let f = fxs[k];
    let local = i - f.off;
    let cx = local % f.w;
    let cy = local / f.w;
    var sum = vec3<u32>(0u);
    var n = 0u;
    if (f.plain != 0u) {
        let x0 = cx * params.sw / f.w;
        let x1 = min(max((cx + 1u) * params.sw / f.w, x0 + 1u), params.sw);
        let y0 = cy * params.sh / f.h;
        let y1 = min(max((cy + 1u) * params.sh / f.h, y0 + 1u), params.sh);
        for (var y = y0; y < y1; y++) {
            for (var x = x0; x < x1; x++) {
                sum += load8(x, y);
                n += 1u;
            }
        }
    } else {
        let fw = f32(params.sw);
        let fh = f32(params.sh);
        for (var j = 0u; j < 3u; j++) {
            for (var q = 0u; q < 3u; q++) {
                let u = (f32(cx) + (f32(q) + 0.5) / 3.0) / f32(f.w) - 0.5;
                let v = (f32(cy) + (f32(j) + 0.5) / 3.0) / f32(f.h) - 0.5;
                let lx = u * f.iw * fw;
                let ly = v * f.ih * fh;
                let px = f.ix * fw + lx * f.cos_r - ly * f.sin_r;
                let py = f.iy * fh + lx * f.sin_r + ly * f.cos_r;
                n += 1u;
                if (px >= 0.0 && py >= 0.0 && px < fw && py < fh) {
                    sum += load8(u32(px), u32(py));
                }
            }
        }
    }
    let a = (sum + vec3<u32>(n / 2u)) / vec3<u32>(max(n, 1u));
    out[i] = a.x | (a.y << 8u) | (a.z << 16u) | (255u << 24u);
}
";

/// Where each fixture's grid sits in the reduced buffer.
#[derive(Debug, PartialEq)]
pub struct Plan {
    /// First pixel of each fixture.
    pub offsets: Vec<u32>,
    /// Pixels in total.
    pub total: u32,
}

impl Plan {
    /// Bytes read back for this plan.
    pub fn bytes(&self) -> u64 {
        u64::from(self.total) * 4
    }
}

/// Layout for `fixtures`, or None when they cannot be reduced on the GPU (none, too many,
/// invalid, or too big): the caller then reads the whole screen.
pub fn plan(fixtures: &[Fixture]) -> Option<Plan> {
    if fixtures.is_empty()
        || fixtures.len() > MAX_FIXTURES
        || !fixtures.iter().all(vjc_core::dmx::valid)
    {
        return None;
    }
    let mut total = 0u32;
    let mut offsets = vec![];
    for f in fixtures {
        offsets.push(total);
        total = total.checked_add(f.width.checked_mul(f.height)?)?;
    }
    let p = Plan { offsets, total };
    (p.bytes() <= MAX_BYTES).then_some(p)
}

/// Most texels one invocation may average: past this a small fixture on a large screen
/// would loop for millions of texels on one GPU thread (and overflow the u32 sums).
pub const MAX_CELL: u64 = 4096;

/// Whether the GPU pass may serve this plan on a `size` screen texture of `format`:
/// every cell stays small, the dispatch fits one dimension, and the texture is the plain
/// RGBA8 the shader reads. Otherwise the caller reads the whole screen on the CPU.
pub fn gpu_ok(
    fixtures: &[Fixture],
    plan: &Plan,
    size: (u32, u32),
    format: wgpu::TextureFormat,
) -> bool {
    let cells_ok = fixtures.iter().all(|f| {
        u64::from(size.0.div_ceil(f.width.max(1))) * u64::from(size.1.div_ceil(f.height.max(1)))
            <= MAX_CELL
    });
    cells_ok && plan.total.div_ceil(64) <= 65535 && format == wgpu::TextureFormat::Rgba8Unorm
}

/// Bytes a whole-screen readback moves (rows padded to 256).
#[cfg(test)]
pub fn full_bytes((w, h): (u32, u32)) -> u64 {
    u64::from(crate::capture::padded_row(w)) * u64::from(h)
}

/// The fixture as seen on its reduced grid: same layout and colour, whole-grid input.
pub fn on_grid(f: &Fixture) -> Fixture {
    Fixture {
        input: FixtureInput::default(),
        ..f.clone()
    }
}

pub struct Reducer {
    pipeline: wgpu::ComputePipeline,
    fx: wgpu::Buffer,
    params: wgpu::Buffer,
    /// Storage buffer the pass writes (shared: queue order keeps frames apart).
    out: Option<wgpu::Buffer>,
}

impl Reducer {
    /// None when the device cannot run compute.
    pub fn new(device: &wgpu::Device) -> Option<Reducer> {
        let l = device.limits();
        if l.max_compute_workgroups_per_dimension == 0
            || l.max_compute_invocations_per_workgroup < 64
            || l.max_storage_buffers_per_shader_stage < 2
        {
            return None;
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("art-net reduce"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("art-net reduce"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let buf = |size: u64, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("art-net reduce"),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        Some(Reducer {
            pipeline,
            fx: buf(
                (MAX_FIXTURES * 48) as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            params: buf(
                16,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            ),
            out: None,
        })
    }

    /// Reduces `tex` (Rgba8Unorm, `size`) into `dst` (MAP_READ|COPY_DST, `plan.bytes()` or
    /// more) and starts its asynchronous map. Never waits.
    #[allow(clippy::too_many_arguments)]
    pub fn reduce_into(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: &wgpu::Texture,
        (sw, sh): (u32, u32),
        fixtures: &[Fixture],
        plan: &Plan,
        dst: &wgpu::Buffer,
    ) -> MapState {
        let mut raw: Vec<u8> = Vec::with_capacity(fixtures.len() * 48);
        for (f, off) in fixtures.iter().zip(&plan.offsets) {
            let (s, c) = f.input.rotation.to_radians().sin_cos();
            let u = |v: u32| v.to_le_bytes();
            let fl = |v: f32| v.to_le_bytes();
            for b in [
                u(*off),
                u(f.width),
                u(f.height),
                u(u32::from(f.input.is_full())),
                fl(f.input.x),
                fl(f.input.y),
                fl(f.input.w),
                fl(f.input.h),
                fl(s),
                fl(c),
                u(0),
                u(0),
            ] {
                raw.extend_from_slice(&b);
            }
        }
        queue.write_buffer(&self.fx, 0, &raw);
        let mut p = Vec::with_capacity(16);
        for v in [fixtures.len() as u32, plan.total, sw, sh] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        queue.write_buffer(&self.params, 0, &p);
        let need = plan.bytes();
        if self.out.as_ref().is_none_or(|b| b.size() < need) {
            self.out = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("art-net reduce out"),
                size: need,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
        }
        let out = self.out.as_ref().unwrap();
        let view = tex.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("art-net reduce"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.fx.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: out.as_entire_binding(),
                },
            ],
        });
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(plan.total.div_ceil(64), 1, 1);
        }
        enc.copy_buffer_to_buffer(out, 0, dst, 0, need);
        queue.submit(Some(enc.finish()));
        map_read(dst)
    }
}

/// Channel bytes of every fixture from the reduced buffer (tight RGBA, `plan` layout).
pub fn packets(
    fixtures: &[Fixture],
    universe: u16,
    reduced: &[u8],
    seq: u8,
    black: bool,
) -> Vec<Vec<u8>> {
    let mut at = 0usize;
    vjc_core::dmx::frame_packets_with(fixtures, universe, seq, black, |f| {
        let n = (f.width * f.height) as usize * 4;
        let part = reduced.get(at..at + n).unwrap_or(&[]);
        at += n;
        vjc_core::dmx::map_pixels(&on_grid(f), part, f.width, f.height)
    })
}
