//! Thumbnails of the bundled effects and generators for the browser. Each shader is
//! rendered once at 96x54 over a fixed test pattern on a worker thread and kept as an egui
//! texture (LRU). A few are started per frame so startup is never blocked; while pending
//! or when the shader does not compile the browser shows its placeholder.

use crate::compositor::{FORMAT, FX_VS, draw_pass, fx_bind_layout};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};

pub const W: u32 = 96;
pub const H: u32 = 54;
/// Most thumbnails kept at once; past this the least recently shown are dropped.
const MAX_THUMBS: usize = 256;
/// Renders started per UI frame.
pub const PER_FRAME: usize = 2;
/// Renders queued on the worker at once.
const MAX_INFLIGHT: usize = 4;

enum Slot {
    Pending,
    Ready(egui::TextureHandle, u64),
    /// Compile / render error: stays on the placeholder.
    Failed,
}

type Done = (String, Result<Vec<u8>, String>);

pub struct FxThumbs {
    slots: HashMap<String, Slot>,
    tick: u64,
    cap: usize,
    /// Thumbnails shown since this tick (the previous frame on) are not evicted: with more
    /// visible than `cap` they would be rendered again every frame. None until the first
    /// `step`.
    frame_floor: Option<u64>,
    /// `tick` when the current frame's `step` ran.
    frame_start: u64,
    /// Ids seen on screen this frame that have no slot yet.
    wanted: Vec<String>,
    req: Option<Sender<String>>,
    tx: Sender<Done>,
    done: Receiver<Done>,
}

impl FxThumbs {
    pub fn new() -> Self {
        let (tx, done) = channel();
        FxThumbs {
            slots: HashMap::new(),
            tick: 0,
            cap: MAX_THUMBS,
            frame_floor: None,
            frame_start: 0,
            wanted: vec![],
            req: None,
            tx,
            done,
        }
    }

    /// Starts the render worker (once). `ctx` is repainted when a thumbnail finishes.
    pub fn attach(&mut self, device: wgpu::Device, queue: wgpu::Queue, ctx: egui::Context) {
        if self.req.is_some() {
            return;
        }
        let (req, rx) = channel::<String>();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            run_worker(
                rx,
                tx,
                || ctx.request_repaint(),
                || Renderer::new(&device, &queue),
                |r, id| r.render(id),
            )
        });
        self.req = Some(req);
    }

    /// Texture of the shader, or None while pending / failed. Unknown ids are queued.
    pub fn texture(&mut self, id: &str) -> Option<egui::TextureId> {
        self.tick += 1;
        match self.slots.get_mut(id) {
            Some(Slot::Ready(h, used)) => {
                *used = self.tick;
                Some(h.id())
            }
            Some(_) => None,
            None => {
                if !self.wanted.iter().any(|w| w == id) {
                    self.wanted.push(id.to_string());
                }
                None
            }
        }
    }

    /// Marks up to `max` wanted ids as pending and returns them (in the order shown).
    /// Always forgets the rest: visible rows ask again next frame.
    fn take_batch(&mut self, max: usize) -> Vec<String> {
        let inflight = self
            .slots
            .values()
            .filter(|s| matches!(s, Slot::Pending))
            .count();
        let room = MAX_INFLIGHT.saturating_sub(inflight);
        let mut out = vec![];
        for id in std::mem::take(&mut self.wanted) {
            if out.len() >= max.min(room) {
                break;
            }
            if !self.slots.contains_key(&id) {
                self.slots.insert(id.clone(), Slot::Pending);
                out.push(id);
            }
        }
        out
    }

    /// Stores the outcome of a render. Ignored unless the id was pending.
    fn finish(&mut self, ctx: &egui::Context, id: &str, res: Result<Vec<u8>, String>) {
        if !matches!(self.slots.get(id), Some(Slot::Pending)) {
            return;
        }
        match res {
            Ok(rgba) if rgba.len() == (W * H * 4) as usize => {
                let img = egui::ColorImage::from_rgba_unmultiplied([W as usize, H as usize], &rgba);
                let h = ctx.load_texture("fx-thumb", img, egui::TextureOptions::LINEAR);
                self.tick += 1;
                self.slots.insert(id.into(), Slot::Ready(h, self.tick));
                self.evict();
            }
            _ => {
                self.slots.insert(id.into(), Slot::Failed);
            }
        }
    }

    fn evict(&mut self) {
        loop {
            let ready = self
                .slots
                .iter()
                .filter_map(|(k, s)| match s {
                    Slot::Ready(_, used) => Some((k.clone(), *used)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if ready.len() <= self.cap {
                return;
            }
            let floor = self.frame_floor;
            let Some((oldest, _)) = ready
                .into_iter()
                .filter(|(_, u)| floor.is_none_or(|f| *u <= f))
                .min_by_key(|(_, u)| *u)
            else {
                return;
            };
            self.slots.remove(&oldest);
        }
    }

    /// Once per frame: collects finished renders and starts a few new ones.
    pub fn step(&mut self, ctx: &egui::Context) {
        self.frame_floor = Some(self.frame_start);
        self.frame_start = self.tick;
        while let Ok((id, res)) = self.done.try_recv() {
            self.finish(ctx, &id, res);
        }
        let batch = self.take_batch(PER_FRAME);
        if let Some(req) = &self.req {
            for id in batch {
                if req.send(id.clone()).is_err() {
                    self.slots.insert(id, Slot::Failed);
                }
            }
        } else {
            // No GPU yet: forget, ask again later.
            for id in batch {
                self.slots.remove(&id);
            }
        }
    }
}

/// Worker loop. A panic while building the renderer must not kill the thread silently
/// (every slot would stay Pending for ever): each request then answers with an error.
fn run_worker<R>(
    rx: Receiver<String>,
    tx: Sender<Done>,
    repaint: impl Fn(),
    init: impl FnOnce() -> R,
    render: impl Fn(&R, &str) -> Result<Vec<u8>, String>,
) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(init)).ok();
    while let Ok(id) = rx.recv() {
        let out = match &r {
            Some(r) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(r, &id)))
                .unwrap_or_else(|_| Err("thumbnail render panicked".into())),
            None => Err("thumbnail renderer failed to start".into()),
        };
        if tx.send((id, out)).is_err() {
            break;
        }
        repaint();
    }
}

/// Fixed input for effects: colour bars over a grey ramp with a checker strip.
pub fn test_pattern() -> Vec<u8> {
    const BARS: [[u8; 3]; 6] = [
        [235, 235, 235],
        [235, 235, 16],
        [16, 235, 235],
        [16, 235, 16],
        [235, 16, 235],
        [16, 16, 235],
    ];
    let mut d = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let rgb = if y < H * 2 / 3 {
                BARS[(x * 6 / W) as usize]
            } else if y < H * 5 / 6 {
                let g = (x * 255 / (W - 1)) as u8;
                [g, g, g]
            } else if (x / 6 + y / 6) % 2 == 0 {
                [240, 240, 240]
            } else {
                [20, 20, 20]
            };
            d.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    d
}

/// GPU side: one pipeline per render, over a shared test pattern. Lives on the worker.
struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bgl: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    vs: wgpu::ShaderModule,
    sampler: wgpu::Sampler,
    pattern: wgpu::TextureView,
}

impl Renderer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let bgl = fx_bind_layout(device);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx_thumb_pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx_thumb_vs"),
            source: wgpu::ShaderSource::Wgsl(FX_VS.into()),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let tex = make_tex(device, "fx_thumb_pattern");
        queue.write_texture(
            tex.as_image_copy(),
            &test_pattern(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
            wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
        );
        Renderer {
            device: device.clone(),
            queue: queue.clone(),
            bgl,
            layout,
            vs,
            sampler,
            pattern: tex.create_view(&Default::default()),
        }
    }

    /// RGBA8 pixels (W x H, tightly packed) of the shader `id` at default params. Errors of
    /// the whole job (submit and readback included) are caught here instead of reaching
    /// the device's uncaptured handler, which panics.
    fn render(&self, id: &str) -> Result<Vec<u8>, String> {
        self.render_variant(id, Extreme::Default, 1.0)
    }

    /// Like `render`, with every float parameter at its default, minimum or maximum.
    fn render_variant(&self, id: &str, which: Extreme, time: f32) -> Result<Vec<u8>, String> {
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let res = self.render_job(id, which, time);
        // Pop both scopes before returning, so a failed render leaves none on the stack.
        let v = pollster::block_on(validation.pop());
        let o = pollster::block_on(oom.pop());
        if let Some(e) = v.or(o) {
            return Err(e.to_string());
        }
        res
    }

    fn render_job(&self, id: &str, which: Extreme, time: f32) -> Result<Vec<u8>, String> {
        let src = vjc_fx::builtin_source(id).ok_or("unknown effect")?;
        let isf = vjc_fx::parse(src).map_err(|e| e.to_string())?;
        let wgsl = vjc_fx::translate(&isf).map_err(|e| e.to_string())?;
        let d = &self.device;
        let scope = d.push_error_scope(wgpu::ErrorFilter::Validation);
        let fs = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(id),
            source: wgpu::ShaderSource::Wgsl(wgsl.as_str().into()),
        });
        let pipe = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(id),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.vs,
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
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(e.to_string());
        }
        let mut params = isf.default_params();
        for p in &mut params {
            if let (Some((lo, hi)), vjc_model::Value::Number(_)) = (p.range, &p.value) {
                match which {
                    Extreme::Default => {}
                    Extreme::Min => p.value = vjc_model::Value::Number(lo),
                    Extreme::Max => p.value = vjc_model::Value::Number(hi),
                }
            }
        }
        let uniforms = isf.pack(&params, time, (W, H));
        let buf = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_thumb_u"),
            size: std::mem::size_of::<vjc_fx::Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&buf, 0, bytemuck::cast_slice(&uniforms));
        let target = make_tex(d, "fx_thumb_target");
        let view = target.create_view(&Default::default());
        let mut enc = d.create_command_encoder(&Default::default());
        draw_pass(
            d,
            &mut enc,
            &self.bgl,
            &self.sampler,
            &pipe,
            &buf,
            &self.pattern,
            &view,
            &self.pattern,
            &self.pattern,
        );
        self.queue.submit(Some(enc.finish()));
        let rb = crate::capture::readback_buffer(d, (W, H), "fx_thumb_rb");
        let state = crate::capture::start_readback_into(d, &self.queue, &target, (W, H), &rb);
        d.poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        if state.load(std::sync::atomic::Ordering::Acquire) != crate::capture::OK {
            return Err("readback failed".into());
        }
        let px = {
            let m = rb.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
            crate::capture::unpad(&m, W, H)
        };
        rb.unmap();
        Ok(px)
    }
}

/// Which end of its range the smoke renders put each float parameter at.
#[derive(Clone, Copy)]
enum Extreme {
    Default,
    #[cfg_attr(not(test), allow(dead_code))]
    Min,
    #[cfg_attr(not(test), allow(dead_code))]
    Max,
}

fn make_tex(device: &wgpu::Device, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(v: u8) -> Vec<u8> {
        vec![v; (W * H * 4) as usize]
    }

    fn pending(t: &mut FxThumbs, ids: &[&str]) -> Vec<String> {
        for i in ids {
            t.texture(i);
        }
        t.take_batch(PER_FRAME)
    }

    #[test]
    fn pending_then_ready() {
        let ctx = egui::Context::default();
        let mut t = FxThumbs::new();
        assert!(t.texture("invert").is_none());
        assert_eq!(t.take_batch(5), ["invert"]);
        assert!(
            t.texture("invert").is_none(),
            "pending shows the placeholder"
        );
        t.finish(&ctx, "invert", Ok(rgba(9)));
        assert!(t.texture("invert").is_some());
    }

    #[test]
    fn negative_control_unfinished_never_ready() {
        let mut t = FxThumbs::new();
        pending(&mut t, &["invert"]);
        assert!(t.texture("invert").is_none());
    }

    #[test]
    fn error_falls_back_and_is_not_retried() {
        let ctx = egui::Context::default();
        let mut t = FxThumbs::new();
        pending(&mut t, &["bad"]);
        t.finish(&ctx, "bad", Err("boom".into()));
        assert!(t.texture("bad").is_none());
        assert!(
            t.take_batch(5).is_empty(),
            "failed id must not be queued again"
        );
        // A wrong-sized image is also an error, not a texture.
        pending(&mut t, &["short"]);
        t.finish(&ctx, "short", Ok(vec![0; 4]));
        assert!(t.texture("short").is_none());
    }

    #[test]
    fn finish_for_unrequested_id_is_ignored() {
        let ctx = egui::Context::default();
        let mut t = FxThumbs::new();
        t.finish(&ctx, "ghost", Ok(rgba(1)));
        assert!(t.texture("ghost").is_none());
    }

    #[test]
    fn per_frame_work_is_bounded() {
        let mut t = FxThumbs::new();
        let ids: Vec<String> = (0..20).map(|i| format!("fx{i}")).collect();
        for i in &ids {
            t.texture(i);
        }
        assert_eq!(t.take_batch(PER_FRAME).len(), PER_FRAME);
        // Negative control: with a larger budget more would start (until inflight cap).
        let mut u = FxThumbs::new();
        for i in &ids {
            u.texture(i);
        }
        assert_eq!(u.take_batch(100).len(), MAX_INFLIGHT);
    }

    #[test]
    fn inflight_cap_holds_across_frames() {
        let mut t = FxThumbs::new();
        let ids: Vec<String> = (0..20).map(|i| format!("fx{i}")).collect();
        let mut started = 0;
        for _ in 0..10 {
            for i in &ids {
                t.texture(i);
            }
            started += t.take_batch(PER_FRAME).len();
        }
        assert_eq!(started, MAX_INFLIGHT, "nothing finished, so no more room");
    }

    #[test]
    fn lru_evicts_oldest_over_cap() {
        let ctx = egui::Context::default();
        let mut t = FxThumbs::new();
        t.cap = 2;
        for id in ["a", "b", "c"] {
            t.texture(id);
            t.take_batch(1);
            t.finish(&ctx, id, Ok(rgba(1)));
            if id == "b" {
                assert!(t.texture("a").is_some(), "touch a: b is now oldest");
            }
        }
        assert!(t.texture("a").is_some() && t.texture("c").is_some());
        assert!(t.texture("b").is_none(), "oldest dropped");
    }

    fn wait(rx: &Receiver<Done>) -> Done {
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker answered")
    }

    #[test]
    fn worker_that_fails_to_start_answers_every_request_with_an_error() {
        let (req, rx) = channel::<String>();
        let (tx, done) = channel();
        let h = std::thread::spawn(move || {
            run_worker(
                rx,
                tx,
                || {},
                || -> u8 { panic!("no device") },
                |_, _| Ok(vec![]),
            )
        });
        req.send("a".into()).unwrap();
        req.send("b".into()).unwrap();
        for id in ["a", "b"] {
            let (got, res) = wait(&done);
            assert_eq!(got, id);
            assert!(res.is_err(), "Failed, not Pending for ever");
        }
        drop(req);
        h.join().unwrap();
    }

    #[test]
    fn worker_with_a_renderer_returns_its_pixels_and_survives_a_render_panic() {
        let (req, rx) = channel::<String>();
        let (tx, done) = channel();
        let h = std::thread::spawn(move || {
            run_worker(
                rx,
                tx,
                || {},
                || 7u8,
                |r, id| {
                    if id == "boom" {
                        panic!("shader")
                    }
                    Ok(vec![*r])
                },
            )
        });
        req.send("ok".into()).unwrap();
        req.send("boom".into()).unwrap();
        req.send("ok".into()).unwrap();
        assert_eq!(wait(&done).1, Ok(vec![7]));
        assert!(wait(&done).1.is_err());
        assert_eq!(wait(&done).1, Ok(vec![7]), "still serving");
        drop(req);
        h.join().unwrap();
    }

    #[test]
    fn thumbnails_on_screen_are_not_evicted_even_over_cap() {
        let ctx = egui::Context::default();
        let mut t = FxThumbs::new();
        let ids = ["a", "b", "c", "d"];
        for id in ids {
            t.texture(id);
            t.take_batch(1);
            t.finish(&ctx, id, Ok(rgba(1)));
        }
        t.cap = 2;
        t.step(&ctx);
        for id in ids {
            assert!(t.texture(id).is_some());
        }
        t.step(&ctx);
        t.texture("e");
        t.take_batch(1);
        t.finish(&ctx, "e", Ok(rgba(1)));
        assert!(
            ids.iter().all(|i| t.texture(i).is_some()),
            "visible survive"
        );
        // Negative control: unseen for two frames, they are evicted again.
        t.step(&ctx);
        t.step(&ctx);
        t.texture("f");
        t.take_batch(1);
        t.finish(&ctx, "f", Ok(rgba(1)));
        let ready = t
            .slots
            .values()
            .filter(|s| matches!(s, Slot::Ready(..)))
            .count();
        assert!(ready <= 2, "cap holds again: {ready}");
    }

    #[test]
    fn negative_control_under_cap_keeps_all() {
        let ctx = egui::Context::default();
        let mut t = FxThumbs::new();
        t.cap = 3;
        for id in ["a", "b", "c"] {
            t.texture(id);
            t.take_batch(1);
            t.finish(&ctx, id, Ok(rgba(1)));
        }
        assert!(["a", "b", "c"].iter().all(|i| t.texture(i).is_some()));
    }

    #[test]
    fn step_without_gpu_does_not_stick_pending() {
        let ctx = egui::Context::default();
        let mut t = FxThumbs::new();
        t.texture("invert");
        t.step(&ctx);
        t.texture("invert");
        assert_eq!(t.take_batch(5), ["invert"], "asked again once a GPU exists");
    }

    #[test]
    fn test_pattern_is_fixed_and_not_flat() {
        let p = test_pattern();
        assert_eq!(p.len(), (W * H * 4) as usize);
        assert_eq!(p, test_pattern());
        assert!(p.chunks(4).any(|a| a[..3] != p[..3]));
    }

    /// Renders every bundled shader on the GPU: all must produce a non-flat image for
    /// generators and effects alike. Skipped when there is no adapter.
    #[test]
    fn gpu_renders_all_bundled_shaders() {
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
        let r = Renderer::new(&device, &queue);
        let inv = r.render("invert").expect("invert renders");
        assert_eq!(inv.len(), (W * H * 4) as usize);
        let pat = test_pattern();
        // Invert flips the first bar (235,235,235) to about (20,20,20).
        assert!(inv[0] < 40 && pat[0] > 200, "effect applied to the pattern");
        assert!(
            inv.chunks(4).any(|a| a[..3] != inv[..3]),
            "invert of a non-flat pattern is not flat"
        );
        assert!(r.render("nope").is_err(), "unknown shader falls back");
        let ids = vjc_fx::BUILTIN.iter().chain(vjc_fx::GENERATORS);
        let failed: Vec<String> = ids
            .filter_map(|(id, _)| r.render(id).err().map(|e| format!("{id}: {e}")))
            .collect();
        assert!(failed.is_empty(), "shaders failed to render: {failed:?}");
    }

    /// Pixels fully transparent black: what NaN or inf in a shader becomes after the
    /// Rgba8 target clamps it (the test pattern and every bundled effect are opaque).
    fn nan_pixels(px: &[u8]) -> usize {
        px.chunks(4).filter(|a| a[3] == 0).count()
    }

    #[test]
    fn nan_detector_sees_transparent_black_only() {
        let mut px = test_pattern();
        assert_eq!(nan_pixels(&px), 0);
        px[3] = 0;
        assert_eq!(nan_pixels(&px), 1, "negative control: one hole is counted");
    }

    /// Effects that clear alpha on purpose (keyers; remove_bg without a mask).
    const KEYERS: [&str; 3] = ["chroma_key", "luma_key", "remove_bg"];

    /// Every bundled shader at default, minimum and maximum parameters and at several
    /// times: no transparent-black holes (NaN) outside the keyers; at defaults, not a fully
    /// black frame (strobe is black half of its cycle). Skipped
    /// without a GPU.
    #[test]
    fn gpu_no_bundled_shader_outputs_nan_or_black() {
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
        let r = Renderer::new(&device, &queue);
        let mut bad = vec![];
        for (id, _) in vjc_fx::BUILTIN.iter().chain(vjc_fx::GENERATORS) {
            for (which, name) in [
                (Extreme::Default, "default"),
                (Extreme::Min, "min"),
                (Extreme::Max, "max"),
            ] {
                for time in [0.0, 1.7, 1000.0] {
                    let Ok(px) = r.render_variant(id, which, time) else {
                        bad.push(format!("{id} {name} t={time}: render failed"));
                        continue;
                    };
                    let holes = nan_pixels(&px);
                    if holes > 0 && !KEYERS.contains(id) {
                        bad.push(format!("{id} {name} t={time}: {holes} NaN-like pixels"));
                    } else if matches!(which, Extreme::Default)
                        && *id != "strobe"
                        && px.chunks(4).all(|a| a[..3] == [0, 0, 0])
                    {
                        bad.push(format!("{id} {name} t={time}: black frame"));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "bad shader output:\n{}", bad.join("\n"));
    }
}
