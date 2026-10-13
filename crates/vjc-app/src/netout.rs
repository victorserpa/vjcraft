//! Syphon / NDI output: screens whose device is Syphon or NDI publish their texture.
//!
//! The frame never waits on the network: the texture is copied into a readback buffer
//! (same asynchronous scheme as recording, see `capture`), and once the map completes the
//! pixels go to a worker thread that owns the sender. At most `MAX_INFLIGHT` readbacks are
//! pending; extra frames are dropped. Syphon with no connected client skips the readback.
//! (A zero-copy path through the wgpu-hal MTLTexture is not implemented.)

use crate::capture::{Inflight, MAX_INFLIGHT, start_readback, unpad};
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
    rgba: Vec<u8>,
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
    next_due: Instant,
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
                let mut sender = match Sender::open(w.protocol, &w.name) {
                    Ok(s) => s,
                    Err(e) => {
                        *e2.lock().unwrap() = Some(format!("{} output: {e}", w.protocol.label()));
                        return;
                    }
                };
                loop {
                    match rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(f) => {
                            if let Err(e) = sender.send(f.w, f.h, f.w * 4, &f.rgba) {
                                *e2.lock().unwrap() =
                                    Some(format!("{} output: {e}", w.protocol.label()));
                                return;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                    w2.store(sender.wants_frames(), Ordering::Relaxed);
                }
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
            next_due: Instant::now(),
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
            let _ = w.join();
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
                    let data = buf.slice(..).get_mapped_range().map(|m| unpad(&m, w, h));
                    buf.unmap();
                    if let (Ok(rgba), Some(tx)) = (data, &p.tx)
                        && tx.try_send(Frame { w, h, rgba }).is_ok()
                    {
                        p.sent += 1;
                    }
                }
            }
            if !p.reported
                && let Some(e) = p.err.lock().unwrap().clone()
            {
                p.reported = true;
                errs.push(e);
            }
        }
        errs
    }

    /// Queues a readback of `tex` (Rgba8Unorm, `size`) for the publisher of `id` when due.
    pub fn feed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: Uuid,
        tex: &wgpu::Texture,
        size: (u32, u32),
        fps: f32,
    ) {
        let Some(p) = self.pubs.get_mut(&id) else {
            return;
        };
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
