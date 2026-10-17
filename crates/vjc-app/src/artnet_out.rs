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
//! GPU reduction (`artnet_reduce`): a compute pass averages each fixture's input rectangle
//! onto its pixel grid and only that (sum of fixture pixels x 4 bytes) is read back. The
//! whole-screen readback above stays as the fallback (no compute, too many or invalid
//! fixtures, texture not sampleable).
//!
//! A sender is recreated only when the socket target (`ip`) changes; universe and fixture
//! changes go to the running worker over a channel.

use crate::artnet_reduce::{self, Reducer};
use crate::capture::{
    Inflight, MAX_INFLIGHT, blacken, padded_row, readback_buffer, start_readback_into, unpad_into,
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
    /// Mapped buffer of GPU-reduced fixture grids (tight RGBA, `artnet_reduce::Plan` layout),
    /// with the fixtures it was reduced for.
    Reduced(wgpu::Buffer, Vec<Fixture>),
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
    inflight: Inflight<(wgpu::Buffer, Vec<Fixture>)>,
    size: (u32, u32),
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
                            match &f.src {
                                Src::Gpu(b) => match b.slice(..).get_mapped_range() {
                                    Ok(m) => unpad_into(&m, f.w, f.h, &mut scratch),
                                    Err(_) => ready = false,
                                },
                                Src::Reduced(b, _) => match b.slice(..).get_mapped_range() {
                                    Ok(m) => {
                                        scratch.clear();
                                        scratch.extend_from_slice(&m);
                                    }
                                    Err(_) => ready = false,
                                },
                                Src::Cpu(_) => {}
                            }
                            if f.black {
                                match &f.src {
                                    Src::Cpu(v) => {
                                        scratch.clear();
                                        scratch.extend_from_slice(v);
                                    }
                                    Src::Gpu(_) | Src::Reduced(..) => {}
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
                                    let packets = match &f.src {
                                        Src::Reduced(_, fx) => artnet_reduce::packets(
                                            fx,
                                            set.universe,
                                            rgba,
                                            seq,
                                            f.black,
                                        ),
                                        _ => dmx::frame_packets(
                                            &set.fixtures,
                                            set.universe,
                                            rgba,
                                            (f.w, f.h),
                                            seq,
                                            f.black,
                                        ),
                                    };
                                    // Black goes out at once and drops what was still waiting.
                                    let delay = if f.black { 0 } else { set.delay_ms };
                                    if f.black {
                                        held.clear();
                                    }
                                    let due = now + Duration::from_millis(u64::from(delay));
                                    held.push_back((due, packets));
                                }
                            }
                            if let Src::Gpu(b) | Src::Reduced(b, _) = f.src {
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
    /// GPU reducer, created on the first feed; `None` inside = no compute support.
    reducer: Option<Option<Reducer>>,
    /// Never reduce on the GPU (tests of the fallback).
    #[cfg(test)]
    cpu_only: bool,
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
            for ((buf, fixtures), ok) in p.inflight.take_settled() {
                if !ok {
                    p.pool.push(buf);
                    continue;
                }
                let src = if fixtures.is_empty() {
                    Src::Gpu(buf)
                } else {
                    Src::Reduced(buf, fixtures)
                };
                let sent = match &p.tx {
                    Some(tx) => tx.try_send(Frame {
                        w,
                        h,
                        src,
                        black: p.black,
                    }),
                    None => continue,
                };
                // Worker busy: the frame is dropped, the buffer goes back to the pool.
                if let Err(mpsc::TrySendError::Full(f) | mpsc::TrySendError::Disconnected(f)) = sent
                    && let Src::Gpu(b) | Src::Reduced(b, _) = f.src
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
            p.pool.clear(); // wrong size for the new screen
            p.size = size;
        }
        #[cfg(test)]
        let allow = !self.cpu_only;
        #[cfg(not(test))]
        let allow = true;
        let reducer = if allow {
            self.reducer
                .get_or_insert_with(|| Reducer::new(device))
                .as_mut()
        } else {
            None
        };
        let plan = reducer
            .as_ref()
            .filter(|_| tex.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING))
            .and_then(|_| artnet_reduce::plan(&p.want.fixtures))
            .filter(|pl| artnet_reduce::gpu_ok(&p.want.fixtures, pl, size, tex.format()));
        let need = plan
            .as_ref()
            .map_or(u64::from(padded_row(size.0)) * u64::from(size.1), |pl| {
                pl.bytes()
            });
        // Buffers still with the worker during a resize come back with another size.
        let buf = match take_matching(&mut p.pool, wgpu::Buffer::size, Some(need)) {
            Some(b) => b,
            None => match &plan {
                Some(_) => device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("art-net reduced readback"),
                    size: need,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                None => readback_buffer(device, size, "art-net readback"),
            },
        };
        match (reducer, plan) {
            (Some(r), Some(pl)) => {
                let fx = p.want.fixtures.clone();
                let state = r.reduce_into(device, queue, tex, size, &fx, &pl, &buf);
                p.inflight.push((buf, fx), state);
            }
            _ => {
                let state = start_readback_into(device, queue, tex, size, &buf);
                p.inflight.push((buf, vec![]), state);
            }
        }
    }

    /// Bytes the next readback of `fixtures` moves from a `size` screen, and what the whole
    /// screen would move. Used to report the saving.
    #[cfg(test)]
    fn readback_bytes(fixtures: &[Fixture], size: (u32, u32)) -> (u64, u64) {
        let full = artnet_reduce::full_bytes(size);
        (
            artnet_reduce::plan(fixtures).map_or(full, |p| p.bytes()),
            full,
        )
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

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no wgpu adapter: skipped");
            return None;
        };
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    /// Known pattern, not smooth, so a wrong cell or flip shows.
    fn pattern(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x * 7 + y * 13) % 256) as u8,
                    255,
                ]);
            }
        }
        v
    }

    fn texture(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32) -> wgpu::Texture {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        queue.write_texture(
            tex.as_image_copy(),
            &pattern(w, h),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        tex
    }

    fn fixtures_for_compare() -> Vec<Fixture> {
        let mut a = fixture(8, 6, PixelFormat::Rgb);
        a.flip_h = true;
        a.serpentine = true;
        a.gamma = 2.2;
        let mut b = fixture(5, 4, PixelFormat::Rgbw);
        b.start_channel = 200;
        b.input.x = 0.4;
        b.input.y = 0.55;
        b.input.w = 0.5;
        b.input.h = 0.6;
        b.input.rotation = 30.0;
        let mut c = fixture(7, 3, PixelFormat::W);
        c.start_channel = 300;
        c.color.brightness = 0.8;
        vec![a, b, c]
    }

    #[test]
    fn gpu_gate_sends_huge_cells_and_other_formats_to_the_cpu() {
        use wgpu::TextureFormat::{Bgra8Unorm, Rgba8Unorm, Rgba8UnormSrgb};
        let par = [fixture(1, 1, PixelFormat::Rgb)];
        let p = artnet_reduce::plan(&par).unwrap();
        // A 1x1 par on a 4K screen would average 8.3M texels on one thread.
        assert!(!artnet_reduce::gpu_ok(&par, &p, (3840, 2160), Rgba8Unorm));
        let wall = [fixture(168, 168, PixelFormat::Rgb)];
        let p = artnet_reduce::plan(&wall).unwrap();
        assert!(artnet_reduce::gpu_ok(&wall, &p, (1920, 1080), Rgba8Unorm));
        assert!(!artnet_reduce::gpu_ok(&wall, &p, (1920, 1080), Bgra8Unorm));
        assert!(!artnet_reduce::gpu_ok(
            &wall,
            &p,
            (1920, 1080),
            Rgba8UnormSrgb
        ));
    }

    #[test]
    fn gpu_reduce_matches_cpu_within_tolerance() {
        let Some((device, queue)) = gpu() else {
            return;
        };
        let (w, h) = (97, 61); // awkward size: cells of different widths
        let tex = texture(&device, &queue, w, h);
        let fx = fixtures_for_compare();
        let plan = artnet_reduce::plan(&fx).unwrap();
        let mut red = Reducer::new(&device).expect("compute");
        let dst = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: plan.bytes(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        red.reduce_into(&device, &queue, &tex, (w, h), &fx, &plan, &dst);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let bytes = dst.slice(..).get_mapped_range().unwrap().to_vec();
        let gpu_pk = artnet_reduce::packets(&fx, 0, &bytes, 1, false);
        let cpu_pk = dmx::frame_packets(&fx, 0, &pattern(w, h), (w, h), 1, false);
        assert_eq!(gpu_pk.len(), cpu_pk.len());
        let mut worst = 0u8;
        for (g, c) in gpu_pk.iter().zip(&cpu_pk) {
            let (Some(Packet::Dmx(g)), Some(Packet::Dmx(c))) = (decode(g), decode(c)) else {
                panic!("not ArtDmx");
            };
            assert_eq!((g.universe, g.data.len()), (c.universe, c.data.len()));
            for (a, b) in g.data.iter().zip(&c.data) {
                worst = worst.max(a.abs_diff(*b));
            }
        }
        // Box-averaged fixtures are exact up to rounding; the rotated one samples points
        // (float rounding at a pixel edge can pick a neighbour).
        assert!(worst <= 40, "worst diff {worst}");
        // Negative control: the check does see differences.
        let wrong = artnet_reduce::packets(&fx, 0, &vec![0u8; bytes.len()], 1, false);
        assert_ne!(wrong, cpu_pk);
        // Plain fixture alone: exact.
        let one = &fx[2..];
        let p1 = artnet_reduce::plan(one).unwrap();
        let d1 = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: p1.bytes(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        red.reduce_into(&device, &queue, &tex, (w, h), one, &p1, &d1);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let b1 = d1.slice(..).get_mapped_range().unwrap().to_vec();
        assert_eq!(
            artnet_reduce::packets(one, 0, &b1, 1, false),
            dmx::frame_packets(one, 0, &pattern(w, h), (w, h), 1, false)
        );
    }

    /// Feeds a real texture through `ArtNetOut` (GPU path unless `cpu_only`) until a packet
    /// arrives.
    fn run_feed(cpu_only: bool, black: bool) -> dmx::ArtDmx {
        let (device, queue) = gpu().expect("gpu");
        let (rx, addr) = local();
        let tex = texture(&device, &queue, 64, 48);
        let mut out = ArtNetOut {
            cpu_only,
            ..Default::default()
        };
        let mut w = want(addr, fixture(4, 3, PixelFormat::Rgb), 0);
        w.fixtures[0].order = ChannelOrder::Rgb;
        let id = w.id;
        out.sync(&[w]);
        rx.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let mut b = [0u8; 1024];
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            out.feed(&device, &queue, id, &tex, (64, 48), black);
            out.poll(&device);
            if let Ok(n) = rx.recv(&mut b)
                && let Some(Packet::Dmx(d)) = decode(&b[..n])
            {
                return d;
            }
        }
        panic!("no packet");
    }

    #[test]
    fn end_to_end_gpu_and_cpu_paths_agree() {
        if gpu().is_none() {
            return;
        }
        let g = run_feed(false, false);
        let c = run_feed(true, false);
        assert_eq!(g.data, c.data);
        assert!(g.data.iter().any(|&v| v != 0));
    }

    #[test]
    fn blackout_on_the_gpu_path_is_all_zeros() {
        if gpu().is_none() {
            return;
        }
        let d = run_feed(false, true);
        assert_eq!(d.data.len(), 36);
        assert!(d.data.iter().all(|&v| v == 0));
        // Control: the same texture without black-out is not zero.
        assert!(run_feed(false, false).data.iter().any(|&v| v != 0));
    }

    #[test]
    fn readback_bytes_before_and_after() {
        // 1920x1080 screen, two 168x168 fixtures: before = whole screen, after = grids.
        let fx = vec![
            fixture(168, 168, PixelFormat::Rgb),
            fixture(168, 168, PixelFormat::Rgb),
        ];
        let (after, before) = ArtNetOut::readback_bytes(&fx, (1920, 1080));
        assert_eq!(before, 1920 * 4 * 1080); // 8_294_400
        assert_eq!(after, 2 * 168 * 168 * 4); // 225_792
        assert!(after * 36 < before);
    }
}
