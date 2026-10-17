//! Syphon / NDI output: screens whose device is Syphon or NDI publish their texture.
//!
//! The frame never waits on the network: the texture is copied into a readback buffer
//! (same asynchronous scheme as recording, see `capture`), and once the map completes the
//! pixels go to a worker thread that owns the sender. At most `MAX_INFLIGHT` readbacks are
//! pending; extra frames are dropped. Syphon with no connected client skips the readback.
//! (A zero-copy path through the wgpu-hal MTLTexture is not implemented.)

use crate::capture::{Inflight, MAX_INFLIGHT, blacken, start_readback, unpad};
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;
use vjc_net::{Protocol, Sender};

/// What a screen wants to publish.
#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    pub id: Uuid,
    pub protocol: Protocol,
    pub name: String,
}

struct Frame {
    w: u32,
    h: u32,
    fps: f32,
    rgba: Vec<u8>,
}

/// Seconds between attempts to reopen a failed output (the LTC output uses the same).
const RETRY: Duration = Duration::from_secs(2);

/// What the worker needs from a backend (a fake in tests).
trait Out {
    fn wants_frames(&self) -> bool;
    fn send(&mut self, f: &Frame) -> Result<(), String>;
}

impl Out for Sender {
    fn wants_frames(&self) -> bool {
        Sender::wants_frames(self)
    }
    fn send(&mut self, f: &Frame) -> Result<(), String> {
        Sender::send(self, f.w, f.h, f.w * 4, &f.rgba, f.fps)
    }
}

/// Publishes frames from `rx` until it disconnects. A failed open or send records the
/// error (the UI shows it once), drops the sender and tries to open again every `retry`;
/// a successful reopen clears the error. Frames that arrive while down are discarded.
fn run_worker<S: Out>(
    rx: &mpsc::Receiver<Frame>,
    wanted: &AtomicBool,
    err: &Mutex<Option<String>>,
    label: &str,
    retry: Duration,
    mut open: impl FnMut() -> Result<S, String>,
) {
    let mut sender: Option<S> = None;
    let mut next_try = Instant::now();
    loop {
        if sender.is_none() && Instant::now() >= next_try {
            next_try = Instant::now() + retry;
            match open() {
                Ok(s) => {
                    *err.lock().unwrap() = None;
                    sender = Some(s);
                }
                Err(e) => *err.lock().unwrap() = Some(format!("{label} output: {e}")),
            }
        }
        match rx.recv_timeout(Duration::from_millis(250).min(retry)) {
            Ok(f) => {
                if let Some(s) = sender.as_mut()
                    && let Err(e) = s.send(&f)
                {
                    *err.lock().unwrap() = Some(format!("{label} output: {e}"));
                    sender = None;
                    next_try = Instant::now() + retry;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        wanted.store(
            sender.as_ref().is_some_and(|s| s.wants_frames()),
            Ordering::Relaxed,
        );
    }
}

struct Pub {
    want: Want,
    tx: Option<SyncSender<Frame>>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Set by the worker: someone is listening.
    wanted: Arc<AtomicBool>,
    err: Arc<Mutex<Option<String>>>,
    reported: bool,
    inflight: Inflight<wgpu::Buffer>,
    size: (u32, u32),
    fps: f32,
    next_due: Instant,
    /// Global black-out: frames go out as black, still at full rate.
    black: bool,
    pub sent: u64,
}

impl Pub {
    fn start(want: Want) -> Pub {
        let (tx, rx) = mpsc::sync_channel::<Frame>(2);
        let wanted = Arc::new(AtomicBool::new(true));
        let err = Arc::new(Mutex::new(None));
        let (w2, e2, w) = (wanted.clone(), err.clone(), want.clone());
        let worker = std::thread::Builder::new()
            .name("vjc-netout".into())
            .spawn(move || {
                let label = w.protocol.label();
                run_worker(&rx, &w2, &e2, label, RETRY, || {
                    Sender::open(w.protocol, &w.name)
                });
            })
            .ok();
        Pub {
            want,
            tx: Some(tx),
            worker,
            wanted,
            err,
            reported: false,
            inflight: Inflight::new(),
            size: (0, 0),
            fps: 60.0,
            next_due: Instant::now(),
            black: false,
            sent: 0,
        }
    }

    fn failed(&self) -> bool {
        self.err.lock().unwrap().is_some()
    }
}

impl Drop for Pub {
    fn drop(&mut self) {
        self.tx = None; // worker sees Disconnected and drops the sender
        if let Some(w) = self.worker.take() {
            vjc_media::reap(w);
        }
    }
}

#[derive(Default)]
pub struct NetOut {
    pubs: HashMap<Uuid, Pub>,
}

impl NetOut {
    /// Starts publishers for new wants, stops the ones no longer wanted or renamed.
    pub fn sync(&mut self, wants: &[Want]) {
        self.pubs
            .retain(|id, p| wants.iter().any(|w| w.id == *id && *w == p.want));
        for w in wants {
            self.pubs
                .entry(w.id)
                .or_insert_with(|| Pub::start(w.clone()));
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.pubs.is_empty()
    }

    /// Hands settled readbacks to the workers. Returns new error messages (once each).
    pub fn poll(&mut self, device: &wgpu::Device) -> Vec<String> {
        let mut errs = vec![];
        if self.pubs.is_empty() {
            return errs;
        }
        let _ = device.poll(wgpu::PollType::Poll);
        for p in self.pubs.values_mut() {
            let (w, h) = p.size;
            for (buf, ok) in p.inflight.take_settled() {
                if ok {
                    let mut data = buf.slice(..).get_mapped_range().map(|m| unpad(&m, w, h));
                    buf.unmap();
                    if p.black
                        && let Ok(d) = &mut data
                    {
                        blacken(d);
                    }
                    if let (Ok(rgba), Some(tx)) = (data, &p.tx)
                        && tx
                            .try_send(Frame {
                                w,
                                h,
                                fps: p.fps,
                                rgba,
                            })
                            .is_ok()
                    {
                        p.sent += 1;
                    }
                }
            }
            let e = p.err.lock().unwrap().clone();
            match e {
                Some(e) if !p.reported => {
                    p.reported = true;
                    errs.push(e);
                }
                None => p.reported = false, // recovered: a later failure reports again
                _ => {}
            }
        }
        errs
    }

    /// Queues a readback of `tex` (Rgba8Unorm, `size`) for the publisher of `id` when due.
    #[allow(clippy::too_many_arguments)]
    pub fn feed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: Uuid,
        tex: &wgpu::Texture,
        size: (u32, u32),
        fps: f32,
        black: bool,
    ) {
        let Some(p) = self.pubs.get_mut(&id) else {
            return;
        };
        p.black = black;
        if p.failed() || !p.wanted.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if now < p.next_due {
            return;
        }
        let period = Duration::from_secs_f64(1.0 / f64::from(fps.clamp(1.0, 240.0)));
        p.next_due = (p.next_due + period).max(now);
        if p.inflight.len() >= MAX_INFLIGHT || (p.size != size && p.inflight.len() > 0) {
            return;
        }
        p.size = size;
        p.fps = fps;
        let (buf, state) = start_readback(device, queue, tex, size, "net out readback");
        p.inflight.push(buf, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(id: Uuid, name: &str) -> Want {
        Want {
            id,
            protocol: Protocol::Ndi,
            name: name.into(),
        }
    }

    #[test]
    fn sync_adds_renames_and_removes() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut n = NetOut::default();
        n.sync(&[want(a, "x"), want(b, "y")]);
        assert_eq!(n.pubs.len(), 2);
        n.sync(&[want(a, "x2")]);
        assert_eq!(n.pubs.len(), 1);
        assert_eq!(n.pubs[&a].want.name, "x2");
        n.sync(&[]);
        assert!(n.is_empty());
    }

    struct Fake {
        log: Arc<Mutex<Vec<u32>>>,
        fail_next: Arc<AtomicBool>,
    }

    impl Out for Fake {
        fn wants_frames(&self) -> bool {
            true
        }
        fn send(&mut self, f: &Frame) -> Result<(), String> {
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err("boom".into());
            }
            self.log.lock().unwrap().push(f.w);
            Ok(())
        }
    }

    fn frame(w: u32) -> Frame {
        Frame {
            w,
            h: 1,
            fps: 30.0,
            rgba: vec![0; w as usize * 4],
        }
    }

    fn wait(mut c: impl FnMut() -> bool) -> bool {
        for _ in 0..300 {
            if c() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn worker_retries_open_and_resend_after_failures() {
        let (tx, rx) = mpsc::sync_channel::<Frame>(2);
        let wanted = Arc::new(AtomicBool::new(true));
        let err = Arc::new(Mutex::new(None));
        let log = Arc::new(Mutex::new(vec![]));
        let fail_next = Arc::new(AtomicBool::new(false));
        let (w2, e2, l2, f2) = (wanted.clone(), err.clone(), log.clone(), fail_next.clone());
        let mut opens = 0;
        let h = std::thread::spawn(move || {
            run_worker(&rx, &w2, &e2, "Fake", Duration::from_millis(20), || {
                opens += 1;
                if opens <= 2 {
                    Err("no server".to_string())
                } else {
                    Ok(Fake {
                        log: l2.clone(),
                        fail_next: f2.clone(),
                    })
                }
            })
        });
        // Two failed opens, then the third works: the error clears by itself.
        assert!(wait(|| err.lock().unwrap().is_some()));
        assert!(wait(|| err.lock().unwrap().is_none()));
        tx.send(frame(1)).unwrap();
        assert!(wait(|| log.lock().unwrap().len() == 1));
        // A failed send reports, drops the sender and reopens.
        fail_next.store(true, Ordering::SeqCst);
        tx.send(frame(2)).unwrap();
        assert!(wait(|| err.lock().unwrap().is_some()));
        assert!(wait(|| err.lock().unwrap().is_none()));
        tx.send(frame(3)).unwrap();
        assert!(wait(|| log.lock().unwrap().as_slice() == [1, 3]));
        drop(tx);
        h.join().unwrap();
    }

    #[cfg(feature = "ndi")]
    #[test]
    fn missing_runtime_reports_once_and_never_crashes() {
        // Only meaningful where libndi is absent (CI, most dev machines).
        if vjc_net::available(Protocol::Ndi).is_ok() {
            return;
        }
        let id = Uuid::new_v4();
        let mut p = Pub::start(want(id, "t"));
        for _ in 0..100 {
            if p.failed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(p.failed());
        assert!(p.err.lock().unwrap().as_ref().unwrap().contains("NDI"));
        p.reported = true;
    }
}
