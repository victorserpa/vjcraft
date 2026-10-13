//! ML effects on the app side: one `vjc_ml::Worker` per effect instance, fed with the
//! reduced frames the compositor reads back and feeding it the masks they produce.
//! The render thread never waits: a late worker just means the previous mask stays.

use crate::compositor::{Compositor, MlDraw};
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};
use vjc_ml::{ModelKind, Status, Worker};

/// Pumps an instance may go unused before its worker is dropped.
const IDLE_PUMPS: u64 = 600;

/// The model an effect kind runs, if it is an ML effect.
pub fn model_of(kind: &str) -> Option<ModelKind> {
    match kind {
        "depth" => Some(ModelKind::Depth),
        "remove_bg" => Some(ModelKind::Matting),
        _ => None,
    }
}

/// Models folder: the preference, else `<data dir>/models`.
pub fn models_dir(pref: Option<&PathBuf>, data_dir: Option<PathBuf>) -> PathBuf {
    pref.cloned()
        .or_else(|| data_dir.map(|d| d.join("models")))
        .unwrap_or_else(|| PathBuf::from("models"))
}

/// How often a missing or failed model file is looked at again.
const RECHECK: Duration = Duration::from_secs(2);

/// Identity of a model file on disk: size and modification time. None when it is missing.
type FileSig = Option<(u64, Option<SystemTime>)>;

fn file_sig(model: ModelKind, dir: &std::path::Path) -> FileSig {
    let md = std::fs::metadata(model.path_in(dir)).ok()?;
    md.is_file().then(|| (md.len(), md.modified().ok()))
}

struct Inst {
    worker: Worker,
    /// File seen when the worker was spawned.
    sig: FileSig,
    /// Clock reading of the last look at the file while the worker was stuck.
    checked: Duration,
    /// Source tag of the last frame handed to the worker.
    tag: u64,
    last_used: u64,
}

pub struct MlHub {
    dir: PathBuf,
    inst: HashMap<u128, Inst>,
    pumps: u64,
    clock: Box<dyn Fn() -> Duration + Send>,
}

impl MlHub {
    pub fn new() -> Self {
        Self {
            dir: PathBuf::new(),
            inst: HashMap::new(),
            pumps: 0,
            clock: {
                let t0 = Instant::now();
                Box::new(move || t0.elapsed())
            },
        }
    }

    /// Changes the models folder; running workers are dropped so they reload from it.
    pub fn set_dir(&mut self, dir: PathBuf) {
        if self.dir != dir {
            self.dir = dir;
            self.inst.clear();
        }
    }

    /// Draw info of an ML effect instance, or None when it cannot run (not an ML effect,
    /// model missing or failed): the effect is then skipped.
    pub fn draw(&mut self, kind: &str, id: uuid::Uuid, tag: u64) -> Option<MlDraw> {
        let model = model_of(kind)?;
        let key = id.as_u128();
        let (dir, pumps, now) = (&self.dir, self.pumps, (self.clock)());
        let inst = self.inst.entry(key).or_insert_with(|| Inst {
            worker: Worker::spawn(model, dir),
            sig: file_sig(model, dir),
            checked: now,
            tag,
            last_used: pumps,
        });
        inst.last_used = pumps;
        // A model dropped into the folder later (or a failed one replaced) must work
        // without a restart: look again every RECHECK while stuck.
        if matches!(
            inst.worker.status(),
            Status::NotFound(_) | Status::Failed(_)
        ) && now.saturating_sub(inst.checked) >= RECHECK
        {
            inst.checked = now;
            let sig = file_sig(model, dir);
            if sig.is_some() && sig != inst.sig {
                inst.worker = Worker::spawn(model, dir);
                inst.sig = sig;
                inst.tag = tag;
            }
        }
        match inst.worker.status() {
            Status::NotFound(_) | Status::Failed(_) => None,
            Status::Loading | Status::Ready => Some(MlDraw {
                key,
                tag,
                size: model.input_size(),
            }),
        }
    }

    /// Why an ML effect instance is not running (shown next to it), if so.
    pub fn problem(&self, kind: &str, id: uuid::Uuid) -> Option<String> {
        match self.inst.get(&id.as_u128()) {
            Some(i) => match i.worker.status() {
                Status::NotFound(m) | Status::Failed(m) => Some(m),
                _ => None,
            },
            // Not drawn yet (or not in use): check the file so the message is there early.
            None => model_of(kind)
                .filter(|m| !m.path_in(&self.dir).is_file())
                .map(|m| m.missing_message(&self.dir)),
        }
    }

    /// Once per rendered frame, after the compositor rendered: moves reduced frames to the
    /// workers and finished masks to the compositor.
    pub fn pump(&mut self, comp: &mut Compositor, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.pumps += 1;
        for f in comp.ml_poll(device) {
            if let Some(i) = self.inst.get_mut(&f.key) {
                if f.tag != i.tag {
                    i.worker.reset();
                    i.tag = f.tag;
                }
                i.worker.submit(f.rgba, f.size);
            }
        }
        for (key, i) in &self.inst {
            if let Some(m) = i.worker.take() {
                comp.ml_set_mask(device, queue, *key, (m.w, m.h), &m.data);
            }
        }
        let now = self.pumps;
        self.inst.retain(|_, i| now - i.last_used < IDLE_PUMPS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_effect_kinds_to_models() {
        assert_eq!(model_of("depth"), Some(ModelKind::Depth));
        assert_eq!(model_of("remove_bg"), Some(ModelKind::Matting));
        assert_eq!(model_of("blur"), None);
    }

    #[test]
    fn models_dir_prefers_the_preference() {
        let pref = PathBuf::from("/x/models");
        assert_eq!(models_dir(Some(&pref), Some("/data".into())), pref);
        assert_eq!(
            models_dir(None, Some("/data".into())),
            PathBuf::from("/data/models")
        );
    }

    fn fake_clock(hub: &mut MlHub) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        use std::sync::atomic::{AtomicU64, Ordering};
        let ms = std::sync::Arc::new(AtomicU64::new(0));
        let m = ms.clone();
        hub.clock = Box::new(move || Duration::from_millis(m.load(Ordering::Relaxed)));
        ms
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn model_added_later_is_picked_up() {
        use std::sync::atomic::Ordering;
        let dir = temp_dir("late-model");
        let mut hub = MlHub::new();
        let ms = fake_clock(&mut hub);
        hub.set_dir(dir.clone());
        let id = uuid::Uuid::from_u128(9);
        assert!(hub.draw("depth", id, 0).is_none());
        assert!(matches!(
            hub.inst[&id.as_u128()].worker.status(),
            Status::NotFound(_)
        ));
        std::fs::write(ModelKind::Depth.path_in(&dir), b"not a model").unwrap();
        // Too early: still not looked at again.
        ms.store(1000, Ordering::Relaxed);
        hub.draw("depth", id, 0);
        assert!(matches!(
            hub.inst[&id.as_u128()].worker.status(),
            Status::NotFound(_)
        ));
        ms.store(2500, Ordering::Relaxed);
        hub.draw("depth", id, 0);
        let st = hub.inst[&id.as_u128()].worker.status();
        assert!(
            matches!(st, Status::Loading | Status::Ready | Status::Failed(_)),
            "{st:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_model_reloads_only_when_the_file_changes() {
        use std::sync::atomic::Ordering;
        let dir = temp_dir("failed-model");
        let path = ModelKind::Depth.path_in(&dir);
        std::fs::write(&path, b"broken").unwrap();
        let mut hub = MlHub::new();
        let ms = fake_clock(&mut hub);
        hub.set_dir(dir.clone());
        let id = uuid::Uuid::from_u128(10);
        let key = id.as_u128();
        hub.draw("depth", id, 0);
        for _ in 0..500 {
            if matches!(hub.inst[&key].worker.status(), Status::Failed(_)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(hub.inst[&key].worker.status(), Status::Failed(_)));
        let sig = hub.inst[&key].sig;
        // Unchanged file: no new worker, same signature.
        ms.store(3000, Ordering::Relaxed);
        hub.draw("depth", id, 0);
        assert_eq!(hub.inst[&key].sig, sig);
        assert!(matches!(hub.inst[&key].worker.status(), Status::Failed(_)));
        // Changed file: respawned (signature follows the file).
        std::fs::write(&path, b"broken differently").unwrap();
        ms.store(6000, Ordering::Relaxed);
        hub.draw("depth", id, 0);
        assert_ne!(hub.inst[&key].sig, sig);
        assert_eq!(hub.inst[&key].sig, file_sig(ModelKind::Depth, &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_model_skips_the_effect_and_explains() {
        let mut hub = MlHub::new();
        hub.set_dir(std::env::temp_dir().join("vjc-no-models-here"));
        let id = uuid::Uuid::from_u128(5);
        // Before the first draw the problem is already known from the file check.
        let early = hub.problem("depth", id).unwrap();
        assert!(early.contains("model-small.onnx"), "{early}");
        assert!(hub.draw("depth", id, 0).is_none());
        let why = hub.problem("depth", id).unwrap();
        assert!(
            why.contains("model not found") && why.contains("docs/ml.md"),
            "{why}"
        );
        assert!(hub.problem("blur", uuid::Uuid::from_u128(6)).is_none());
    }
}
