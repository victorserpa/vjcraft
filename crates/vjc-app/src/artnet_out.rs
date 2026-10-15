//! Art-Net output: screens whose device is Art-Net sample their texture on the fixture grid
//! and send ArtDmx to one IP, at most `MAX_HZ` times per second.
//!
//! Same scheme as `netout`: the screen texture goes through an asynchronous readback (see
//! `capture`), at most `MAX_INFLIGHT` are pending (extra frames are dropped). The UI thread
//! only queues the copy and hands the mapped buffer over; the worker thread removes the row
//! padding, averages the pixels onto the fixture grid (`vjc_core::dmx::map_pixels`, which
//! reads the full-size buffer directly, so there is no separate downsample pass), and owns
//! the UDP socket. Readback buffers are pooled: the worker unmaps and returns them.
//!
//! Not done: shrinking the texture on the GPU before readback. The copy GPU->CPU still moves
//! the whole screen (async, no stall); a reduce pass needs its own pipeline and was left out.
//!
//! A sender is recreated only when the socket target (`ip`) changes; universe and fixture
//! changes go to the running worker over a channel.

use crate::capture::{
    Inflight, MAX_INFLIGHT, blacken, readback_buffer, start_readback_into, unpad_into,
};
use eframe::egui_wgpu::wgpu;
use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;
use vjc_core::dmx;
use vjc_model::Fixture;

pub const MAX_HZ: f32 = 40.0;
/// Highest rate a device may ask for (the DMX512 refresh limit is about 44 Hz).
pub const TOP_HZ: f32 = 44.0;

/// Time between two sends for a device rate (`fps` <= 0 or not a number = `MAX_HZ`).
pub fn period(fps: f32) -> Duration {
    let hz = if fps.is_finite() && fps > 0.0 {
        fps.clamp(1.0, TOP_HZ)
    } else {
        MAX_HZ
    };
    Duration::from_secs_f64(1.0 / f64::from(hz))
}

/// Lets a frame through at most once per period.
#[derive(Default)]
struct Pacer {
    next: Option<Instant>,
}

impl Pacer {
    fn allow(&mut self, now: Instant, period: Duration) -> bool {
        if self.next.is_some_and(|n| now < n) {
            return false;
        }
        self.next = Some(now + period);
        true
    }
}

/// What a screen wants to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    pub id: Uuid,
    /// "host" or "host:port" (default port 6454).
    pub ip: String,
    pub universe: u16,
    pub fixtures: Vec<Fixture>,
    /// Rate limit in Hz (0 = `MAX_HZ`).
    pub fps: f32,
    /// Output delay in milliseconds.
    pub delay_ms: u32,
}

/// Where a frame's pixels live.
enum Src {
    /// Mapped readback buffer with padded rows; the worker unmaps and recycles it.
    Gpu(wgpu::Buffer),
    /// Tight RGBA (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    Cpu(Vec<u8>),
}

struct Frame {
    w: u32,
    h: u32,
    src: Src,
    black: bool,
}

/// Fixtures, first universe, rate and delay of a running sender.
struct Settings {
    universe: u16,
    fixtures: Vec<Fixture>,
    fps: f32,
    delay_ms: u32,
}

impl Settings {
    fn of(w: &Want) -> Settings {
        Settings {
            universe: w.universe,
            fixtures: w.fixtures.clone(),
            fps: w.fps,
            delay_ms: w.delay_ms,
        }
    }

    fn valid(&self) -> bool {
        !self.fixtures.is_empty() && self.fixtures.iter().all(dmx::valid)
    }
}

/// Resolves "host" / "host:port" to an IPv4 address; the port defaults to 6454.
pub fn parse_target(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    let with_port = if s.contains(':') {
        s.to_owned()
    } else {
        format!("{s}:{}", dmx::PORT)
    };
    with_port.to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)
}

/// Sends one frame (tight RGBA `w` x `h`) to `target`. Returns how many ArtDmx packets went out.
#[cfg(test)]
fn send_frame(
    sock: &UdpSocket,
    target: SocketAddr,
    set: &Settings,
    rgba: &[u8],
    size: (u32, u32),
    seq: u8,
) -> std::io::Result<usize> {
    let packets = dmx::frame_packets(&set.fixtures, set.universe, rgba, size, seq, false);
    for p in &packets {
        sock.send_to(p, target)?;
    }
    Ok(packets.len())
}

/// Stores `msg` unless the same error is already there (a per-frame failure reports once).
/// Returns true if it was new.
fn record_error(slot: &Mutex<Option<String>>, msg: String) -> bool {
    let mut g = slot.lock().unwrap();
    if g.as_deref() == Some(msg.as_str()) {
        return false;
    }
    *g = Some(msg);
    true
}

struct Pub {
    want: Want,
    tx: Option<SyncSender<Frame>>,
    upd: Sender<Settings>,
    /// Buffers coming back from the worker.
    back: Receiver<wgpu::Buffer>,
    pool: Vec<wgpu::Buffer>,
    worker: Option<std::thread::JoinHandle<()>>,
    err: Arc<Mutex<Option<String>>>,
    reported: bool,
    inflight: Inflight<wgpu::Buffer>,
    size: (u32, u32),
    /// Byte size of the pooled buffers for `size`; others are dropped.
    buf_size: Option<u64>,
    next_due: Instant,
    /// Global black-out: zeros keep going out (a processor would hold the last frame).
    black: bool,
}

impl Pub {
    fn start(want: Want) -> Pub {
        let (tx, rx) = mpsc::sync_channel::<Frame>(2);
        let (upd, upd_rx) = mpsc::channel::<Settings>();
        let (ret, back) = mpsc::channel::<wgpu::Buffer>();
        let err = Arc::new(Mutex::new(None));
        let (e2, w) = (err.clone(), want.clone());
        let worker = std::thread::Builder::new()
            .name("vjc-artnet".into())
            .spawn(move || {
                let fail = |m: String| {
                    record_error(&e2, format!("Art-Net output: {m}"));
                };
                let Some(target) = parse_target(&w.ip) else {
                    return fail(format!("cannot resolve '{}'", w.ip));
                };
                if !Settings::of(&w).valid() {
                    return fail("fixture is empty or too large".into());
                }
                let sock = match UdpSocket::bind(("0.0.0.0", 0)) {
                    Ok(s) => s,
                    Err(e) => return fail(e.to_string()),
                };
                // Broadcast only if the user typed a broadcast address.
                let _ = sock.set_broadcast(true);
                let mut set = Settings::of(&w);
                let mut seq = 0u8;
                let mut scratch = Vec::new();
                let mut pacer = Pacer::default();
                let mut was_black = false;
                // Packets waiting for their delay to pass.
                let mut held: VecDeque<(Instant, Vec<Vec<u8>>)> = VecDeque::new();
                loop {
                    let wait = held.front().map_or(Duration::from_millis(250), |(due, _)| {
                        due.saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(250))
                    });
                    let got = rx.recv_timeout(wait);
                    while let Ok(s) = upd_rx.try_recv() {
                        set = s;
                    }
                    match got {
                        Ok(f) => {
                            let mut ready = true;
                            if let Src::Gpu(b) = &f.src {
                                match b.slice(..).get_mapped_range() {
                                    Ok(m) => unpad_into(&m, f.w, f.h, &mut scratch),
                                    Err(_) => ready = false,
                                }
                            }
                            if f.black {
                                match &f.src {
                                    Src::Cpu(v) => {
                                        scratch.clear();
                                        scratch.extend_from_slice(v);
                                    }
                                    Src::Gpu(_) => {}
                                }
                                blacken(&mut scratch);
                            }
                            let rgba: &[u8] = match (&f.src, f.black) {
                                (Src::Cpu(v), false) => v,
                                _ => &scratch,
                            };
                            if !set.valid() {
                                // Reported once; the sender restarts on the next change.
                                fail("fixture is empty or too large".into());
                            } else if ready {
                                let now = Instant::now();
                                // Slightly under one period, so jitter does not eat frames.
                                // A black-out change is never held back by the rate limit.
                                let flip = f.black != was_black;
                                was_black = f.black;
                                // fps 0 (old projects): the app-side throttle already paces, as before.
                                let paced = set.fps <= 0.0
                                    || pacer.allow(now, period(set.fps).mul_f32(0.9));
                                if paced || flip {
                                    seq = dmx::next_seq(seq);
                                    // The black-out zeroes the channels too (a fixture colour
                                    // with contrast < 1 would lift black to grey).
                                    let packets = dmx::frame_packets(
                                        &set.fixtures,
                                        set.universe,
                                        rgba,
                                        (f.w, f.h),
                                        seq,
                                        f.black,
                                    );
                                    // Black goes out at once and drops what was still waiting.
                                    let delay = if f.black { 0 } else { set.delay_ms };
                                    if f.black {
                                        held.clear();
                                    }
                                    let due = now + Duration::from_millis(u64::from(delay));
                                    held.push_back((due, packets));
                                }
                            }
                            if let Src::Gpu(b) = f.src {
                                b.unmap();
                                let _ = ret.send(b);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                    let now = Instant::now();
                    while held.front().is_some_and(|(due, _)| *due <= now) {
                        if let Some((_, packets)) = held.pop_front() {
                            // UDP errors (unreachable host, link down) must not stop the show.
                            for p in &packets {
                                let _ = sock.send_to(p, target);
                            }
                        }
                    }
                }
            })
            .ok();
        Pub {
            want,
            tx: Some(tx),
            upd,
            back,
            pool: vec![],
            worker,
            err,
            reported: false,
            inflight: Inflight::new(),
            size: (0, 0),
            buf_size: None,
            next_due: Instant::now(),
            black: false,
        }
    }

    /// Universe, fixture, rate and delay changes reach the running worker; no new socket.
    fn update(&mut self, want: &Want) {
        if self.want != *want {
            let _ = self.upd.send(Settings::of(want));
            self.want = want.clone();
        }
    }

    fn failed(&self) -> bool {
        self.err.lock().unwrap().is_some()
    }
}

impl Drop for Pub {
    fn drop(&mut self) {
        self.tx = None; // worker sees Disconnected and closes the socket
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[derive(Default)]
pub struct ArtNetOut {
    pubs: HashMap<Uuid, Pub>,
}

impl ArtNetOut {
    /// Starts senders for new wants, stops the ones no longer wanted or whose target changed;
    /// universe and fixture changes are sent to the running sender.
    pub fn sync(&mut self, wants: &[Want]) {
        // A worker that died (bad fixture, bind failure) restarts on any change.
        self.pubs.retain(|id, p| {
            wants
                .iter()
                .any(|w| w.id == *id && w.ip == p.want.ip && !(p.failed() && *w != p.want))
        });
        for w in wants {
            match self.pubs.get_mut(&w.id) {
                Some(p) => p.update(w),
                None => {
                    self.pubs.insert(w.id, Pub::start(w.clone()));
                }
            }
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.pubs.len()
    }

    /// Hands settled readbacks to the workers and collects recycled buffers. Returns new
    /// error messages (once each).
    pub fn poll(&mut self, device: &wgpu::Device) -> Vec<String> {
        let mut errs = vec![];
        if self.pubs.is_empty() {
            return errs;
        }
        let _ = device.poll(wgpu::PollType::Poll);
        for p in self.pubs.values_mut() {
            while let Ok(b) = p.back.try_recv() {
                p.pool.push(b);
            }
            let (w, h) = p.size;
            for (buf, ok) in p.inflight.take_settled() {
                if !ok {
                    p.pool.push(buf);
                    continue;
                }
                let sent = match &p.tx {
                    Some(tx) => tx.try_send(Frame {
                        w,
                        h,
                        src: Src::Gpu(buf),
                        black: p.black,
                    }),
                    None => continue,
                };
                // Worker busy: the frame is dropped, the buffer goes back to the pool.
                if let Err(mpsc::TrySendError::Full(f) | mpsc::TrySendError::Disconnected(f)) = sent
                    && let Src::Gpu(b) = f.src
                {
                    b.unmap();
                    p.pool.push(b);
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

    /// Queues a readback of `tex` (Rgba8Unorm, `size`) for the sender of `id` when due.
    pub fn feed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: Uuid,
        tex: &wgpu::Texture,
        size: (u32, u32),
        black: bool,
    ) {
        let Some(p) = self.pubs.get_mut(&id) else {
            return;
        };
        p.black = black;
        if p.failed() {
            return;
        }
        let now = Instant::now();
        if now < p.next_due {
            return;
        }
        let period = period(p.want.fps);
        p.next_due = (p.next_due + period).max(now);
        if p.inflight.len() >= MAX_INFLIGHT || (p.size != size && p.inflight.len() > 0) {
            return;
        }
        if p.size != size {
            p.buf_size = None;
            p.pool.clear(); // wrong size for the new screen
            p.size = size;
        }
        // Buffers still with the worker during a resize come back with the old size.
        let buf = match take_matching(&mut p.pool, wgpu::Buffer::size, p.buf_size) {
            Some(b) => b,
            None => {
                let b = readback_buffer(device, size, "art-net readback");
                p.buf_size = Some(b.size());
                b
            }
        };
        let state = start_readback_into(device, queue, tex, size, &buf);
        p.inflight.push(buf, state);
    }
}

/// Pops a pooled buffer of byte size `want`, dropping any of another size.
fn take_matching<T>(pool: &mut Vec<T>, size: impl Fn(&T) -> u64, want: Option<u64>) -> Option<T> {
    while let Some(b) = pool.pop() {
        if Some(size(&b)) == want {
            return Some(b);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_core::dmx::{Packet, decode};
    use vjc_model::{ChannelOrder, PixelFormat};

    #[test]
    fn same_error_is_recorded_once() {
        let slot = Mutex::new(None);
        assert!(record_error(&slot, "a".into()));
        assert!(!record_error(&slot, "a".into()));
        assert!(record_error(&slot, "b".into()));
    }

    #[test]
    fn stale_size_buffers_are_never_reused() {
        // A buffer returned after a resize keeps the old size: it must be dropped.
        let mut pool = vec![100u64, 400, 100];
        assert_eq!(take_matching(&mut pool, |b| *b, Some(400)), Some(400));
        assert_eq!(take_matching(&mut pool, |b| *b, Some(400)), None);
        assert!(pool.is_empty());
        let mut pool = vec![100u64];
        assert_eq!(take_matching(&mut pool, |b| *b, None), None);
    }

    fn local() -> (UdpSocket, SocketAddr) {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let a = s.local_addr().unwrap();
        (s, a)
    }

    fn want(addr: SocketAddr, f: Fixture, universe: u16) -> Want {
        Want {
            id: Uuid::new_v4(),
            ip: addr.to_string(),
            universe,
            fixtures: vec![f],
            fps: 0.0,
            delay_ms: 0,
        }
    }

    fn settings(fixture: Fixture, universe: u16) -> Settings {
        Settings {
            universe,
            fixtures: vec![fixture],
            fps: 0.0,
            delay_ms: 0,
        }
    }

    fn fixture(w: u32, h: u32, format: PixelFormat) -> Fixture {
        Fixture {
            name: "t".into(),
            width: w,
            height: h,
            format,
            order: ChannelOrder::Grb,
            gamma: 1.0,
            serpentine: false,
            ..Fixture::default()
        }
    }

    fn recv_dmx(rx: &UdpSocket) -> dmx::ArtDmx {
        let mut b = [0u8; 1024];
        let n = rx.recv(&mut b).expect("packet");
        match decode(&b[..n]) {
            Some(Packet::Dmx(d)) => d,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_target_defaults_port() {
        assert_eq!(
            parse_target("127.0.0.1").unwrap().to_string(),
            "127.0.0.1:6454"
        );
        assert_eq!(
            parse_target(" 127.0.0.1:7000 ").unwrap().to_string(),
            "127.0.0.1:7000"
        );
        assert!(parse_target("").is_none());
        assert!(parse_target("no such host.invalid").is_none());
    }

    #[test]
    fn real_socket_receives_the_mapped_bytes() {
        let (rx, addr) = local();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        // 2x1 image: orange and blue.
        let rgba = [255, 128, 0, 255, 0, 0, 255, 255];
        let w = settings(fixture(2, 1, PixelFormat::Rgb), 5);
        assert_eq!(send_frame(&tx, addr, &w, &rgba, (2, 1), 9).unwrap(), 1);
        let d = recv_dmx(&rx);
        assert_eq!((d.universe, d.sequence), (5, 9));
        // GRB order, 6 channels padded to even length (already even).
        assert_eq!(d.data, [128, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn large_fixture_spans_consecutive_universes() {
        let (rx, addr) = local();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let rgba = [10u8, 20, 30, 255].repeat(200);
        let w = settings(fixture(200, 1, PixelFormat::Rgb), 7);
        assert_eq!(send_frame(&tx, addr, &w, &rgba, (200, 1), 1).unwrap(), 2);
        let (a, b) = (recv_dmx(&rx), recv_dmx(&rx));
        assert_eq!((a.universe, a.data.len()), (7, 510));
        assert_eq!((b.universe, b.data.len()), (8, 90));
    }

    #[test]
    fn blacked_out_worker_keeps_sending_zeros_then_resumes() {
        let (rx, addr) = local();
        let mut out = ArtNetOut::default();
        let w = want(addr, fixture(1, 1, PixelFormat::Rgbw), 0);
        let id = w.id;
        out.sync(&[w]);
        let tx = out.pubs[&id].tx.as_ref().unwrap();
        let send = |black| {
            tx.send(Frame {
                w: 1,
                h: 1,
                src: Src::Cpu(vec![200, 100, 50, 255]),
                black,
            })
            .unwrap();
        };
        send(true);
        assert!(recv_dmx(&rx).data.iter().all(|&b| b == 0));
        send(false);
        assert!(recv_dmx(&rx).data.iter().any(|&b| b != 0));
    }

    #[test]
    fn worker_thread_sends_frames_and_stops_on_drop() {
        let (rx, addr) = local();
        let mut out = ArtNetOut::default();
        let w = want(addr, fixture(1, 1, PixelFormat::Rgbw), 0);
        let id = w.id;
        out.sync(&[w]);
        let pubn = &out.pubs[&id];
        pubn.tx
            .as_ref()
            .unwrap()
            .send(Frame {
                w: 1,
                h: 1,
                src: Src::Cpu(vec![200, 100, 50, 255]),
                black: false,
            })
            .unwrap();
        let d = recv_dmx(&rx);
        // GRB with W last: (r-w, g-w, b-w) = (150, 50, 0) -> G R B W
        assert_eq!(d.data, [50, 150, 0, 50]);
        assert!(!pubn.failed());
        out.sync(&[]);
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn bad_target_or_fixture_reports_an_error_without_crashing() {
        let (_rx, addr) = local();
        let mut w = want(addr, fixture(1, 1, PixelFormat::Rgb), 0);
        w.ip = "no such host.invalid".into();
        let p = Pub::start(w);
        for _ in 0..100 {
            if p.failed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(p.failed());
        let w = want(addr, fixture(0, 0, PixelFormat::Rgb), 0);
        let p = Pub::start(w);
        for _ in 0..100 {
            if p.failed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(p.failed());
    }

    #[test]
    fn sync_restarts_only_when_the_target_changes() {
        let (_rx, addr) = local();
        let mut out = ArtNetOut::default();
        let mut w = want(addr, fixture(1, 1, PixelFormat::Rgb), 0);
        out.sync(&[w.clone()]);
        let thread = out.pubs[&w.id].worker.as_ref().unwrap().thread().id();
        w.universe = 3;
        w.fixtures[0].name = "other".into();
        w.fixtures[0].gamma = 2.0;
        out.sync(&[w.clone()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out.pubs[&w.id].want.universe, 3);
        assert_eq!(
            out.pubs[&w.id].worker.as_ref().unwrap().thread().id(),
            thread
        );
        w.ip = "127.0.0.1:1".into();
        out.sync(&[w.clone()]);
        assert_ne!(
            out.pubs[&w.id].worker.as_ref().unwrap().thread().id(),
            thread
        );
    }

    #[test]
    fn running_worker_uses_the_updated_universe() {
        let (rx, addr) = local();
        let mut out = ArtNetOut::default();
        let mut w = want(addr, fixture(1, 1, PixelFormat::Rgb), 0);
        out.sync(&[w.clone()]);
        w.universe = 4;
        out.sync(&[w.clone()]);
        let frame = || Frame {
            w: 1,
            h: 1,
            src: Src::Cpu(vec![1, 2, 3, 255]),
            black: false,
        };
        out.pubs[&w.id].tx.as_ref().unwrap().send(frame()).unwrap();
        assert_eq!(recv_dmx(&rx).universe, 4);
    }

    #[test]
    fn fixture_made_invalid_at_runtime_is_reported() {
        let (_rx, addr) = local();
        let mut w = want(addr, fixture(1, 1, PixelFormat::Rgb), 0);
        let mut p = Pub::start(w.clone());
        w.fixtures[0].width = 0;
        p.update(&w);
        p.tx.as_ref()
            .unwrap()
            .send(Frame {
                w: 1,
                h: 1,
                src: Src::Cpu(vec![1, 2, 3, 255]),
                black: false,
            })
            .unwrap();
        for _ in 0..100 {
            if p.failed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Negative control: before the fix the frame was skipped without any error.
        assert!(p.failed());
    }

    #[test]
    fn valid_fixture_does_not_report_errors() {
        let (rx, addr) = local();
        let w = want(addr, fixture(1, 1, PixelFormat::Rgb), 0);
        let p = Pub::start(w);
        p.tx.as_ref()
            .unwrap()
            .send(Frame {
                w: 1,
                h: 1,
                src: Src::Cpu(vec![1, 2, 3, 255]),
                black: false,
            })
            .unwrap();
        recv_dmx(&rx);
        assert!(!p.failed());
    }

    fn frame(black: bool) -> Frame {
        Frame {
            w: 1,
            h: 1,
            src: Src::Cpu(vec![1, 2, 3, 255]),
            black,
        }
    }

    /// Packets that reach `rx` within `window`.
    fn count_for(rx: &UdpSocket, window: Duration) -> usize {
        rx.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let end = Instant::now() + window;
        let mut n = 0;
        let mut b = [0u8; 1024];
        while Instant::now() < end {
            if rx.recv(&mut b).is_ok() {
                n += 1;
            }
        }
        n
    }

    #[test]
    fn pacer_lets_one_frame_through_per_period() {
        let t0 = Instant::now();
        let p = period(10.0);
        assert_eq!(p, Duration::from_millis(100));
        let mut pc = Pacer::default();
        let got: Vec<bool> = (0..6)
            .map(|i| pc.allow(t0 + Duration::from_millis(i * 30), p))
            .collect();
        assert_eq!(got, [true, false, false, false, true, false]);
        // Negative control: a 30 ms period lets every one of those frames through.
        let mut pc = Pacer::default();
        assert!((0..6).all(|i| pc.allow(
            t0 + Duration::from_millis(i * 30),
            Duration::from_millis(30)
        )));
        assert_eq!(period(0.0), period(MAX_HZ));
        assert_eq!(period(1000.0), period(TOP_HZ));
    }

    #[test]
    fn worker_framerate_limits_the_sends() {
        let (rx, addr) = local();
        let mut w = want(addr, fixture(1, 1, PixelFormat::Rgb), 0);
        w.fps = 10.0;
        let p = Pub::start(w);
        let tx = p.tx.as_ref().unwrap();
        for _ in 0..5 {
            tx.send(frame(false)).unwrap();
        }
        assert_eq!(count_for(&rx, Duration::from_millis(250)), 1);
        // Control: the same frames spaced beyond the period all go out.
        for _ in 0..3 {
            tx.send(frame(false)).unwrap();
            std::thread::sleep(Duration::from_millis(130));
        }
        assert_eq!(count_for(&rx, Duration::from_millis(100)), 3);
    }

    #[test]
    fn worker_without_fps_forwards_every_frame_the_app_sends() {
        // Old projects (fps 0): frames 17 ms apart (one 60 Hz tick) must all go out.
        let (rx, addr) = local();
        let w = want(addr, fixture(1, 1, PixelFormat::Rgb), 0);
        assert_eq!(w.fps, 0.0);
        let p = Pub::start(w);
        let tx = p.tx.as_ref().unwrap();
        for _ in 0..4 {
            tx.send(frame(false)).unwrap();
            std::thread::sleep(Duration::from_millis(17));
        }
        assert_eq!(count_for(&rx, Duration::from_millis(150)), 4);
    }

    #[test]
    fn worker_delay_holds_the_frame_but_black_is_immediate() {
        let (rx, addr) = local();
        let mut w = want(addr, fixture(1, 1, PixelFormat::Rgb), 0);
        w.delay_ms = 250;
        let p = Pub::start(w);
        let tx = p.tx.as_ref().unwrap();
        let t = Instant::now();
        tx.send(frame(false)).unwrap();
        recv_dmx(&rx);
        assert!(
            t.elapsed() >= Duration::from_millis(240),
            "{:?}",
            t.elapsed()
        );
        // Black-out skips the delay (and the control: no delay configured is fast too).
        let t = Instant::now();
        tx.send(frame(true)).unwrap();
        assert!(recv_dmx(&rx).data.iter().all(|&b| b == 0));
        assert!(
            t.elapsed() < Duration::from_millis(200),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn running_worker_applies_two_fixtures_and_colour_but_black_stays_black() {
        let (rx, addr) = local();
        let mut a = fixture(1, 1, PixelFormat::Rgb);
        a.order = ChannelOrder::Rgb;
        a.color.contrast = 0.5; // lifts black to grey
        let mut b = a.clone();
        b.start_channel = 4;
        let mut w = want(addr, a, 0);
        w.fixtures.push(b);
        let p = Pub::start(w);
        let tx = p.tx.as_ref().unwrap();
        tx.send(Frame {
            w: 1,
            h: 1,
            src: Src::Cpu(vec![0, 0, 0, 255]),
            black: false,
        })
        .unwrap();
        assert!(recv_dmx(&rx).data[..6].iter().all(|&v| v > 0));
        tx.send(Frame {
            w: 1,
            h: 1,
            src: Src::Cpu(vec![0, 0, 0, 255]),
            black: true,
        })
        .unwrap();
        assert!(recv_dmx(&rx).data.iter().all(|&v| v == 0));
    }
}
