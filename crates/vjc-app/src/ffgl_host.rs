//! FFGL plugins in the app (docs/specs/ffgl-host.md, stage 2a): the scanned catalog, the
//! effects it offers, and one running plugin instance per layer effect. Plugins run in the
//! `vjc-ffgl-helper` process, so a crashing plugin shows "plugin stopped" and the layer
//! passes through.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vjc_ffgl::{Entry, FfglEffect, Status};
use vjc_model::{Effect, EffectId, Param, Value};

/// Browser group of the FFGL effects.
pub const GROUP: &str = "FFGL plugins";

/// How long one request to the helper may take before the plugin counts as stopped.
const TIMEOUT: Duration = Duration::from_secs(3);
/// An instance that no frame asked for during this long is shut down.
const IDLE: Duration = Duration::from_secs(5);

/// Whether an effect kind names an FFGL plugin (`ffgl:<id>`).
pub fn is_kind(kind: &str) -> bool {
    kind.starts_with("ffgl:")
}

pub type Shared = Arc<Mutex<FfglEffect>>;

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
        let inner = self.inner.clone();
        let spawned = std::thread::Builder::new().name("ffgl-scan".into()).spawn({
            let inner = inner.clone();
            move || {
                let cache = cache.unwrap_or_else(|| {
                    std::env::temp_dir().join(format!("vjc-ffgl-cache-{}.json", std::process::id()))
                });
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
        let params = inputs
            .iter()
            .filter_map(|i| {
                let (value, range) = match &i.kind {
                    vjc_fx::InputKind::Float { min, max, default } => {
                        (Value::Number(*default), Some((*min, *max)))
                    }
                    vjc_fx::InputKind::Bool { default } => (Value::Bool(*default), None),
                    _ => return None,
                };
                Some(Param {
                    name: i.name.clone(),
                    default: value.clone(),
                    value,
                    range,
                    animation: None,
                })
            })
            .collect();
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
        let fx = Arc::new(Mutex::new(FfglEffect::new(
            &entry.path,
            &vjc_ffgl::helper_path(),
            TIMEOUT,
        )));
        rack.insert(
            e.id,
            Slot {
                fx: fx.clone(),
                used: Instant::now(),
            },
        );
        Some(fx)
    }

    /// Passes this frame's param values to the plugin (by position).
    pub fn apply_params(fx: &Shared, params: &[Param]) {
        let Ok(mut f) = fx.lock() else { return };
        for (i, p) in params.iter().enumerate() {
            let v = match p.value {
                Value::Number(n) => n as f32,
                Value::Bool(b) => f32::from(u8::from(b)),
                _ => continue,
            };
            f.set_param(i, v);
        }
    }

    /// "Plugin stopped" when the helper of this effect crashed, hung or was never started.
    pub fn badge(&self, e: &Effect) -> Option<String> {
        let rack = self.inner.rack.lock().ok()?;
        let fx = rack.get(&e.id)?.fx.lock().ok()?;
        matches!(fx.status, Status::Stopped(_)).then(|| "Plugin stopped".to_string())
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
