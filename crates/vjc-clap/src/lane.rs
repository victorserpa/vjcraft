//! The audio callback's side of a CLAP effect. A worker thread owns the [`ClapEffect`] (and so
//! the helper process); the callback talks to it through two lock-free sample rings and never
//! waits: when the wet audio is not there in time, or the plugin is starting, crashed or over
//! its time budget, the callback keeps the dry audio. The wet signal is one callback late.
use crate::{ClapEffect, HostError, MAX_FRAMES, Status};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Ring capacity in samples (power of two): about 0.7 s of stereo at 48 kHz.
const RING: usize = 1 << 16;

/// Single producer, single consumer ring of f32 samples. All-or-nothing push and pop.
struct Ring {
    buf: Box<[AtomicU32]>,
    head: AtomicUsize, // next write (producer)
    tail: AtomicUsize, // next read (consumer)
}

impl Ring {
    fn new() -> Ring {
        Ring {
            buf: (0..RING).map(|_| AtomicU32::new(0)).collect(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }
    fn len(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Acquire))
    }
    fn push(&self, s: &[f32]) -> bool {
        let h = self.head.load(Ordering::Relaxed);
        if RING - h.wrapping_sub(self.tail.load(Ordering::Acquire)) < s.len() {
            return false;
        }
        for (i, v) in s.iter().enumerate() {
            self.buf[h.wrapping_add(i) & (RING - 1)].store(v.to_bits(), Ordering::Relaxed);
        }
        self.head.store(h.wrapping_add(s.len()), Ordering::Release);
        true
    }
    fn pop(&self, out: &mut [f32]) -> bool {
        let t = self.tail.load(Ordering::Relaxed);
        if self.head.load(Ordering::Acquire).wrapping_sub(t) < out.len() {
            return false;
        }
        for (i, v) in out.iter_mut().enumerate() {
            *v = f32::from_bits(self.buf[t.wrapping_add(i) & (RING - 1)].load(Ordering::Relaxed));
        }
        self.tail.store(t.wrapping_add(out.len()), Ordering::Release);
        true
    }
    fn skip(&self, n: usize) {
        let t = self.tail.load(Ordering::Relaxed);
        let n = n.min(self.len());
        self.tail.store(t.wrapping_add(n), Ordering::Release);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneStatus {
    /// Loading the plugin (or restarting it); the audio is dry meanwhile.
    Starting,
    Running,
    /// Shown as "bypassed"; the audio passes through. The text says why.
    Bypassed(String),
}

const STARTING: u8 = 0;
const RUNNING: u8 = 1;
const BYPASSED: u8 = 2;

/// State shared by the control side (UI, mapping, animation), and the lane(s) of one effect:
/// which plugin, its parameter values and its status. Lock-free apart from the reason text.
pub struct ClapShared {
    pub path: String,
    pub id: String,
    params: Vec<AtomicU64>,
    status: AtomicU8,
    reason: Mutex<String>,
    epoch: AtomicU32,
}

impl std::fmt::Debug for ClapShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapShared")
            .field("id", &self.id)
            .field("status", &self.status())
            .finish()
    }
}

impl ClapShared {
    /// `defaults` are the starting values of the plugin parameters, in plugin order.
    pub fn new(path: &str, id: &str, defaults: &[f64]) -> Arc<ClapShared> {
        Arc::new(ClapShared {
            path: path.into(),
            id: id.into(),
            params: defaults.iter().map(|v| AtomicU64::new(v.to_bits())).collect(),
            status: AtomicU8::new(STARTING),
            reason: Mutex::new(String::new()),
            epoch: AtomicU32::new(0),
        })
    }
    pub fn param_count(&self) -> usize {
        self.params.len()
    }
    /// Control side: the wanted value (native range); the lane sends it with the next block.
    pub fn set_param(&self, index: usize, value: f64) {
        if let (Some(p), true) = (self.params.get(index), value.is_finite()) {
            p.store(value.to_bits(), Ordering::Relaxed);
        }
    }
    pub fn param(&self, index: usize) -> f64 {
        self.params
            .get(index)
            .map_or(0.0, |p| f64::from_bits(p.load(Ordering::Relaxed)))
    }
    pub fn status(&self) -> LaneStatus {
        match self.status.load(Ordering::Acquire) {
            RUNNING => LaneStatus::Running,
            BYPASSED => LaneStatus::Bypassed(self.reason.lock().map(|r| r.clone()).unwrap_or_default()),
            _ => LaneStatus::Starting,
        }
    }
    pub fn mark_bypassed(&self, why: &str) {
        if let Ok(mut r) = self.reason.lock() {
            *r = why.into();
        }
        self.status.store(BYPASSED, Ordering::Release);
    }
    pub fn mark_running(&self) {
        self.status.store(RUNNING, Ordering::Release);
    }
    /// Asks the lane to start the plugin again (after a bypass).
    pub fn restart(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.status.store(STARTING, Ordering::Release);
    }
    /// Counts the restarts asked for.
    pub fn restart_gen(&self) -> u32 {
        self.epoch.load(Ordering::Acquire)
    }
}

fn why(s: &HostError) -> String {
    match s {
        HostError::Died => "plugin crashed".into(),
        HostError::Timeout => "plugin went over its time budget".into(),
        HostError::Plugin(m) => m.clone(),
        HostError::Spawn(m) => format!("plugin host did not start: {m}"),
    }
}

/// The callback handle: feed it blocks with [`ClapLane::process`]. Dropping it stops the worker
/// without waiting for it (the helper process is killed when the worker ends).
pub struct ClapLane {
    shared: Arc<ClapShared>,
    input: Arc<Ring>,
    output: Arc<Ring>,
    stop: Arc<AtomicBool>,
    seen_gen: u32,
}

impl ClapLane {
    pub fn spawn(shared: Arc<ClapShared>, helper_bin: &Path, budget: Duration, rate: u32) -> ClapLane {
        let lane = ClapLane {
            input: Arc::new(Ring::new()),
            output: Arc::new(Ring::new()),
            stop: Arc::new(AtomicBool::new(false)),
            seen_gen: shared.restart_gen(),
            shared: shared.clone(),
        };
        let (input, output, stop) = (lane.input.clone(), lane.output.clone(), lane.stop.clone());
        let helper: PathBuf = helper_bin.into();
        let spawned = std::thread::Builder::new()
            .name("clap-lane".into())
            .spawn(move || work(shared, input, output, stop, helper, budget, rate));
        if spawned.is_err() {
            lane.shared.mark_bypassed("could not start the plugin thread");
        }
        lane
    }

    /// Audio callback: interleaved stereo in place. Never blocks, never allocates. The block is
    /// handed to the worker; if its earlier wet output is ready it replaces the block, else the
    /// block stays dry.
    pub fn process(&mut self, buf: &mut [f32]) {
        let g = self.shared.restart_gen();
        if g != self.seen_gen {
            self.seen_gen = g;
            self.output.skip(usize::MAX); // audio of the previous run
        }
        if self.shared.status.load(Ordering::Acquire) != RUNNING || buf.is_empty() {
            return;
        }
        self.input.push(buf);
        // stay close to real time: drop wet audio that piled up while the callback was away
        while self.output.len() >= 2 * buf.len() {
            self.output.skip(buf.len());
        }
        self.output.pop(buf);
    }
}

impl Drop for ClapLane {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn work(
    shared: Arc<ClapShared>,
    input: Arc<Ring>,
    output: Arc<Ring>,
    stop: Arc<AtomicBool>,
    helper: PathBuf,
    budget: Duration,
    rate: u32,
) {
    let n = shared.param_count();
    let mut seen = shared.restart_gen();
    let mut fx = ClapEffect::new(&shared.path, &shared.id, &helper, budget, rate);
    let mut last: Vec<u64> = vec![f64::NAN.to_bits(); n];
    let mut buf = vec![0.0f32; MAX_FRAMES * 2];
    let sync = |fx: &mut ClapEffect, last: &mut Vec<u64>| {
        for (i, l) in last.iter_mut().enumerate() {
            let v = shared.param(i);
            if v.to_bits() != *l {
                fx.set_param(i, v);
                *l = v.to_bits();
            }
        }
    };
    let report = |fx: &ClapEffect| match &fx.status {
        Status::Running => shared.mark_running(),
        Status::Stopped(e) => shared.mark_bypassed(&why(e)),
    };
    sync(&mut fx, &mut last);
    report(&fx);
    while !stop.load(Ordering::Acquire) {
        let g = shared.restart_gen();
        if g != seen {
            seen = g;
            sync(&mut fx, &mut last);
            fx.enable();
            let k = input.len();
            input.skip(k);
            report(&fx);
            continue;
        }
        if fx.status != Status::Running {
            report(&fx);
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        sync(&mut fx, &mut last);
        let avail = input.len() & !1;
        if avail == 0 {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let m = avail.min(buf.len());
        let block = &mut buf[..m];
        input.pop(block);
        fx.process(block); // on a failure the block stays dry
        output.push(block);
        if fx.status != Status::Running {
            report(&fx);
        }
    }
}
