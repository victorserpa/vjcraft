//! FFGL plugins in the app (docs/specs/ffgl-host.md, stage 2a): the scanned catalog, the
//! effects it offers, and one running plugin instance per layer effect. Plugins run in the
//! `vjc-ffgl-helper` process, so a crashing plugin shows "plugin stopped" and the layer
//! passes through.

use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vjc_ffgl::{Entry, FfglEffect, Status};
use vjc_model::{Effect, EffectId, Param, Value};

/// Browser group of the FFGL effects.
pub const GROUP: &str = "FFGL plugins";

/// Effect tooltip: frames are pipelined to keep the render thread free.
pub const LATENCY_TIP: &str = "FFGL plugin: runs in a helper process, so its output has at least two frames of latency (the GPU readback is not waited for); more when the plugin is slower than the frame rate.";

/// How long one request to the helper may take before the plugin counts as stopped.
const TIMEOUT: Duration = Duration::from_secs(3);
/// An instance that no frame asked for during this long is shut down.
const IDLE: Duration = Duration::from_secs(5);

/// Whether an effect kind names an FFGL plugin (`ffgl:<id>`).
pub fn is_kind(kind: &str) -> bool {
    kind.starts_with("ffgl:")
}

/// One running plugin plus the staging buffers of its GPU readback. Both live and die with the
/// effect's slot in the rack.
pub struct Handle {
    fx: Mutex<FfglEffect>,
    /// Locked by the compositor only.
    pub rb: Mutex<Staging>,
}

impl Handle {
    pub fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, FfglEffect>> {
        self.fx.lock()
    }
}

pub type Shared = Arc<Handle>;

/// Ring of 2 staging buffers: frame N copies its input into slot N%2 and starts the mapping
/// without waiting; a later frame picks the bytes up once the GPU has finished.
#[derive(Default)]
pub struct Staging {
    /// (width, height, bytes per pixel) the buffers were made for.
    shape: (u32, u32, u32),
    bufs: Vec<wgpu::Buffer>,
    /// Per slot: the mapping in flight (0 pending, 1 ready, 2 failed).
    pending: [Option<Arc<AtomicU8>>; 2],
    next: usize,
    /// Last plugin output (width, height, texture bytes), shown again while no readback is ready.
    pub last_out: Option<(u32, u32, Vec<u8>)>,
}

impl Staging {
    /// Test hook: forgets every readback in flight, as if none had finished yet.
    #[cfg(test)]
    pub fn forget_pending(&mut self) {
        self.pending = [None, None];
    }

    fn row(&self) -> u32 {
        (self.shape.0 * self.shape.2).div_ceil(256) * 256
    }

    /// Makes the buffers fit `(w, h, bpp)`; a change drops the old ones (and what they held).
    fn ensure(&mut self, device: &wgpu::Device, shape: (u32, u32, u32)) {
        if self.shape == shape && self.bufs.len() == 2 {
            return;
        }
        self.shape = shape;
        self.pending = [None, None];
        self.next = 0;
        let size = u64::from(self.row()) * u64::from(shape.1);
        self.bufs = (0..2)
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ffgl_staging"),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                })
            })
            .collect();
    }

    /// Polls the device without waiting and returns the tight bytes of the newest finished
    /// readback, if any.
    pub fn harvest(&mut self, device: &wgpu::Device, shape: (u32, u32, u32)) -> Option<Vec<u8>> {
        self.ensure(device, shape);
        let _ = device.poll(wgpu::PollType::Poll);
        let (w, h, bpp) = shape;
        let row = self.row() as usize;
        let mut out = None;
        // Older slot first, so the newer finished one wins.
        for k in [self.next, 1 - self.next] {
            let Some(flag) = self.pending[k].as_ref() else {
                continue;
            };
            match flag.load(Ordering::Acquire) {
                0 => continue,
                1 => {
                    let mut v = Vec::with_capacity((w * h * bpp) as usize);
                    if let Ok(data) = self.bufs[k].slice(..).get_mapped_range() {
                        for y in 0..h as usize {
                            v.extend_from_slice(&data[y * row..][..(w * bpp) as usize]);
                        }
                        out = Some(v);
                    }
                    self.bufs[k].unmap();
                }
                _ => {}
            }
            self.pending[k] = None;
        }
        out
    }

    /// Records the copy of `tex` into the free slot; None when both are still busy. Call
    /// `start` with the slot after submitting `enc`.
    pub fn copy_in(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        tex: &wgpu::Texture,
    ) -> Option<usize> {
        let k = self.next;
        if self.pending[k].is_some() {
            return None;
        }
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.bufs[k],
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.row()),
                    rows_per_image: Some(self.shape.1),
                },
            },
            wgpu::Extent3d {
                width: self.shape.0,
                height: self.shape.1,
                depth_or_array_layers: 1,
            },
        );
        self.next = 1 - k;
        Some(k)
    }

    /// Starts the mapping of a slot whose copy was submitted.
    pub fn start(&mut self, k: usize) {
        let flag = Arc::new(AtomicU8::new(0));
        let f = flag.clone();
        self.bufs[k]
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                f.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release);
            });
        self.pending[k] = Some(flag);
    }
}

#[derive(Default)]
struct Catalog {
    entries: Vec<Entry>,
    scanning: bool,
}

struct Slot {
    fx: Shared,
    used: Instant,
}

#[derive(Default)]
struct Inner {
    catalog: Mutex<Catalog>,
    rack: Mutex<HashMap<EffectId, Slot>>,
    /// Tests: always use this cache file.
    #[cfg(test)]
    cache: Mutex<Option<PathBuf>>,
    /// Cache file in the temp folder (no data folder): removed with the host.
    temp_cache: Mutex<Option<PathBuf>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(c) = self.temp_cache.get_mut()
            && let Some(p) = c.take()
        {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Cheap to clone: all clones share one catalog and rack.
#[derive(Clone, Default)]
pub struct FfglHost {
    inner: Arc<Inner>,
}

impl FfglHost {
    /// Every plugin found (effects and sources).
    pub fn entries(&self) -> Vec<Entry> {
        self.inner
            .catalog
            .lock()
            .map(|c| c.entries.clone())
            .unwrap_or_default()
    }

    /// The effect plugins; sources and mixers are not offered in stage 2a.
    pub fn effect_entries(&self) -> Vec<Entry> {
        let mut v = self.entries();
        v.retain(|e| e.info.kind == vjc_ffgl::proto::Kind::Effect);
        v
    }

    pub fn scanning(&self) -> bool {
        self.inner.catalog.lock().is_ok_and(|c| c.scanning)
    }

    /// The temp cache file in use, if any.
    #[cfg(test)]
    pub fn temp_cache(&self) -> Option<PathBuf> {
        self.inner.temp_cache.lock().ok()?.clone()
    }

    #[cfg(test)]
    pub fn isolate(&self, cache: PathBuf) {
        *self.inner.cache.lock().unwrap() = Some(cache);
    }

    /// Scans `dirs` (only these, never the system folders) in a background thread. Results are
    /// cached by path and mtime. Returns whether a scan was started (false while one runs).
    pub fn rescan(&self, dirs: &[String], cache: Option<PathBuf>, force: bool) -> bool {
        {
            let Ok(mut c) = self.inner.catalog.lock() else {
                return false;
            };
            if c.scanning {
                return false;
            }
            c.scanning = true;
        }
        let dirs: Vec<PathBuf> = dirs
            .iter()
            .filter(|d| !d.trim().is_empty())
            .map(PathBuf::from)
            .collect();
        #[allow(unused_mut)]
        let mut cache = cache;
        #[cfg(test)]
        if let Some(c) = self.inner.cache.lock().unwrap().clone() {
            cache = Some(c);
        }
        let cache = cache.unwrap_or_else(|| {
            // One file per host, so dropping one host never deletes another's cache.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::env::temp_dir()
                .join(format!("vjc-ffgl-cache-{}-{n}.json", std::process::id()));
            if let Ok(mut t) = self.inner.temp_cache.lock() {
                *t = Some(p.clone());
            }
            p
        });
        let inner = self.inner.clone();
        let spawned = std::thread::Builder::new().name("ffgl-scan".into()).spawn({
            let inner = inner.clone();
            move || {
                let helper = vjc_ffgl::helper_path();
                let (found, _) = if force {
                    vjc_ffgl::scan_forced(&dirs, &cache, &helper)
                } else {
                    vjc_ffgl::scan(&dirs, &cache, &helper)
                };
                if let Ok(mut c) = inner.catalog.lock() {
                    c.entries = found;
                    c.scanning = false;
                }
            }
        });
        if spawned.is_err()
            && let Ok(mut c) = inner.catalog.lock()
        {
            c.scanning = false;
            return false;
        }
        true
    }

    fn entry_of(&self, kind: &str) -> Option<Entry> {
        self.effect_entries()
            .into_iter()
            .find(|e| vjc_ffgl::effect_id(e) == kind)
    }

    /// Plugin name for the effect list; None for an unknown kind.
    pub fn name_of(&self, kind: &str) -> Option<String> {
        self.entry_of(kind).map(|e| e.info.name)
    }

    /// The plugin's parameters as effect inputs (the same shape the ISF effects use).
    pub fn inputs(&self, kind: &str) -> Option<Vec<vjc_fx::Input>> {
        self.entry_of(kind).map(|e| vjc_ffgl::inputs(&e.info))
    }

    /// A new effect for the plugin `kind` (`ffgl:<id>`), with one param per plugin parameter.
    pub fn new_effect(&self, kind: &str) -> Option<Effect> {
        let inputs = self.inputs(kind)?;
        let (params, _) = params_for(&inputs);
        Some(Effect {
            blend: Default::default(),
            id: uuid::Uuid::new_v4(),
            kind: kind.to_string(),
            bypass: false,
            dry_wet: 1.0,
            params,
        })
    }

    /// The running plugin of an effect, started on first use. None when the plugin is not in
    /// the catalog (folder removed): the effect is skipped and the layer passes through.
    pub fn shared(&self, e: &Effect) -> Option<Shared> {
        let mut rack = self.inner.rack.lock().ok()?;
        if let Some(s) = rack.get_mut(&e.id) {
            s.used = Instant::now();
            return Some(s.fx.clone());
        }
        let entry = self.entry_of(&e.kind)?;
        let fx = Arc::new(Handle {
            fx: Mutex::new(FfglEffect::new(
                &entry.path,
                &vjc_ffgl::helper_path(),
                TIMEOUT,
            )),
            rb: Mutex::default(),
        });
        rack.insert(
            e.id,
            Slot {
                fx: fx.clone(),
                used: Instant::now(),
            },
        );
        Some(fx)
    }

    /// Plugin parameter index of each param of an effect of this kind.
    pub fn index_map(&self, kind: &str) -> Vec<usize> {
        self.inputs(kind)
            .map(|i| params_for(&i).1)
            .unwrap_or_default()
    }

    /// Hands this frame's param values to the plugin; they travel with the next frame.
    pub fn apply_params(fx: &Shared, params: &[Param], index: &[usize]) {
        let Ok(mut f) = fx.lock() else { return };
        for (p, &i) in params.iter().zip(index) {
            let v = match p.value {
                Value::Number(n) => n as f32,
                Value::Bool(b) => f32::from(u8::from(b)),
                _ => continue,
            };
            f.set_param(i, v);
        }
    }

    /// "Plugin stopped" when the helper of this effect crashed or hung; else a host remark
    /// (such as the input being resized).
    pub fn badge(&self, e: &Effect) -> Option<String> {
        let rack = self.inner.rack.lock().ok()?;
        let fx = rack.get(&e.id)?.fx.lock().ok()?;
        if matches!(fx.status, Status::Stopped(_)) {
            return Some("Plugin stopped".to_string());
        }
        fx.note.clone()
    }

    /// Why the plugin stopped, for a tooltip.
    pub fn stop_reason(&self, e: &Effect) -> Option<String> {
        let rack = self.inner.rack.lock().ok()?;
        let fx = rack.get(&e.id)?.fx.lock().ok()?;
        match &fx.status {
            Status::Stopped(why) => Some(format!("{why:?}")),
            Status::Running => None,
        }
    }

    /// Starts the helper of a stopped effect again (the Restart button).
    pub fn restart(&self, e: &Effect) {
        if let Some(fx) = self
            .inner
            .rack
            .lock()
            .ok()
            .and_then(|r| r.get(&e.id).map(|s| s.fx.clone()))
            && let Ok(mut f) = fx.lock()
        {
            f.enable();
        }
    }

    /// Shuts down the instances no frame has asked for lately (effect removed or bypassed).
    pub fn sweep(&self) {
        if let Ok(mut r) = self.inner.rack.lock() {
            r.retain(|_, s| s.used.elapsed() < IDLE);
        }
    }
}

/// Effect params for the plugin inputs, and the plugin parameter index of each one. Inputs
/// that are not a number or a switch get no param; the index list keeps the others aimed at
/// their real plugin parameter.
pub fn params_for(inputs: &[vjc_fx::Input]) -> (Vec<Param>, Vec<usize>) {
    let mut params = Vec::new();
    let mut index = Vec::new();
    for (k, i) in inputs.iter().enumerate() {
        let (value, range) = match &i.kind {
            vjc_fx::InputKind::Float { min, max, default } => {
                (Value::Number(*default), Some((*min, *max)))
            }
            vjc_fx::InputKind::Bool { default } => (Value::Bool(*default), None),
            _ => continue,
        };
        params.push(Param {
            name: i.name.clone(),
            default: value.clone(),
            value,
            range,
            animation: None,
        });
        index.push(k);
    }
    (params, index)
}
