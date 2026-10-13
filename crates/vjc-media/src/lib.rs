//! Video decoding for VJCraft: one decode thread per `Player`, RGBA frames.
//! Two modes: `Player::open` streams paced by the file timestamps and loops at EOF;
//! `Player::open_scrub` serves the frame nearest to a position (0..1) set by the caller,
//! which is what play modes and directions need (see replica/build-log.md, #8).
//! FFmpeg decodes H264, ProRes, MJPEG and HAP.

use ffmpeg_next as ff;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A decoded RGBA8 frame (tightly packed, `width * 4` bytes per row).
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Arc<Vec<u8>>,
    /// Monotonic counter, increments per decoded frame (lets the renderer skip re-uploads).
    pub seq: u64,
}

struct Shared {
    latest: Mutex<Option<Frame>>,
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    /// Playback rate (f64 bits), 1.0 = file speed. Set by `Player::set_rate`.
    rate: AtomicU64,
    /// File duration in seconds (f64 bits); 0 = unknown.
    duration: AtomicU64,
    /// Scrub mode: wanted position 0..1 (f64 bits).
    position: AtomicU64,
}

impl Shared {
    fn new() -> Self {
        Shared {
            latest: Mutex::new(None),
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
            rate: AtomicU64::new(1.0f64.to_bits()),
            duration: AtomicU64::new(0),
            position: AtomicU64::new(0.0f64.to_bits()),
        }
    }

    fn rate(&self) -> f64 {
        f64::from_bits(self.rate.load(Ordering::Relaxed))
    }
}

pub struct Player {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    pub path: PathBuf,
}

impl Player {
    /// Starts decoding `path` in a background thread. Open errors are reported via `error()`.
    pub fn open(path: impl AsRef<Path>) -> Player {
        Self::spawn(path.as_ref(), false)
    }

    /// Like `open`, but the frame shown follows `set_position` instead of the clock.
    /// Clips that fit `CACHE_BYTES` are decoded once into memory; longer ones seek to the
    /// previous keyframe and decode forward on demand.
    pub fn open_scrub(path: impl AsRef<Path>) -> Player {
        Self::spawn(path.as_ref(), true)
    }

    /// Scrub mode: sets the wanted position (0..1, clamped).
    pub fn set_position(&self, pos: f64) {
        let p = if pos.is_finite() {
            pos.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.shared.position.store(p.to_bits(), Ordering::Relaxed);
    }

    fn spawn(path: &Path, scrub: bool) -> Player {
        let path = path.to_path_buf();
        let shared = Arc::new(Shared::new());
        let (s, p) = (shared.clone(), path.clone());
        let thread = std::thread::Builder::new()
            .name("vjc-decode".into())
            .spawn(move || {
                if is_image(&p) {
                    match load_image(&p) {
                        Ok(f) => *s.latest.lock().unwrap() = Some(f),
                        Err(e) => *s.error.lock().unwrap() = Some(e),
                    }
                    return;
                }
                let r = if scrub {
                    run_scrub(&p, &s)
                } else {
                    run(&p, &s, true)
                };
                if let Err(e) = r {
                    *s.error.lock().unwrap() = Some(e.to_string());
                }
            })
            .ok();
        Player {
            shared,
            thread,
            path,
        }
    }

    pub fn latest_frame(&self) -> Option<Frame> {
        self.shared.latest.lock().unwrap().clone()
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Sets the playback rate (1.0 = file speed), clamped to 0.05..=20. Applies from the
    /// next frame without a time jump. Design choice: the decode thread paces itself by
    /// this rate, so the clock stays outside the player (simplest; no seek per tick).
    pub fn set_rate(&self, rate: f64) {
        let r = if rate.is_finite() { rate } else { 1.0 };
        self.shared
            .rate
            .store(r.clamp(0.05, 20.0).to_bits(), Ordering::Relaxed);
    }

    /// Duration of the file in seconds, once known (None for images/unknown).
    pub fn duration(&self) -> Option<f64> {
        let d = f64::from_bits(self.shared.duration.load(Ordering::Relaxed));
        (d > 0.0).then_some(d)
    }

    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp"))
}

/// Decodes a still image into a single RGBA frame.
fn load_image(path: &Path) -> Result<Frame, String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let (width, height) = img.dimensions();
    Ok(Frame {
        width,
        height,
        data: Arc::new(img.into_raw()),
        seq: 1,
    })
}

/// Small pool of frame buffers; a buffer is reused once the consumer dropped its `Arc`.
struct Pool(Vec<Arc<Vec<u8>>>);

impl Pool {
    fn take(&mut self, len: usize) -> &mut Arc<Vec<u8>> {
        let idx = match self
            .0
            .iter()
            .position(|a| Arc::strong_count(a) == 1 && a.len() == len)
        {
            Some(i) => i,
            None => {
                if self.0.len() >= 4 {
                    self.0.retain(|a| Arc::strong_count(a) > 1);
                }
                self.0.push(Arc::new(vec![0u8; len]));
                self.0.len() - 1
            }
        };
        &mut self.0[idx]
    }
}

/// Decodes the file; with `looping` restarts at EOF. Also used by tests with `looping=false`
/// through `decode_n`.
fn run(path: &Path, shared: &Shared, looping: bool) -> Result<(), ff::Error> {
    decode_loop(path, looping, shared, &mut |frame, due| {
        // Pace: wait until the frame's presentation time.
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                return false;
            }
            let now = Instant::now();
            if now >= due {
                break;
            }
            std::thread::sleep((due - now).min(Duration::from_millis(5)));
        }
        *shared.latest.lock().unwrap() = Some(frame);
        true
    })
}

/// Clips whose decoded RGBA frames fit this budget are cached whole for scrubbing.
pub const CACHE_BYTES: f64 = 384.0 * 1024.0 * 1024.0;

/// Sequential decoder with seek, no pacing. Frames come out with their pts in seconds.
struct Scrubber {
    ictx: ff::format::context::Input,
    decoder: ff::decoder::Video,
    stream_idx: usize,
    tb: f64,
    fps: f64,
    duration: f64,
    scaler: Option<ff::software::scaling::Context>,
    eof: bool,
    count: u64,
    /// pts of the last frame handed out (NaN if none) and a copy of it.
    last_t: f64,
    last: Option<Frame>,
}

impl Scrubber {
    fn open(path: &Path) -> Result<Scrubber, ff::Error> {
        ff::init()?;
        let ictx = ff::format::input(&path)?;
        let (stream_idx, tb, fps, params) = {
            let st = ictx
                .streams()
                .best(ff::media::Type::Video)
                .ok_or(ff::Error::StreamNotFound)?;
            let r = st.avg_frame_rate();
            let fps = if r.denominator() != 0 && r.numerator() != 0 {
                r.numerator() as f64 / r.denominator() as f64
            } else {
                30.0
            };
            (st.index(), f64::from(st.time_base()), fps, st.parameters())
        };
        let decoder = ff::codec::context::Context::from_parameters(params)?
            .decoder()
            .video()?;
        let duration = ictx.duration().max(0) as f64 / f64::from(ff::ffi::AV_TIME_BASE);
        Ok(Scrubber {
            ictx,
            decoder,
            stream_idx,
            tb,
            fps,
            duration,
            scaler: None,
            eof: false,
            count: 0,
            last_t: f64::NAN,
            last: None,
        })
    }

    /// Next frame in decode order, or None at end of file.
    fn next(&mut self) -> Result<Option<(f64, Frame)>, ff::Error> {
        let mut decoded = ff::frame::Video::empty();
        loop {
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                if self.scaler.is_none() {
                    self.scaler = Some(ff::software::scaling::Context::get(
                        decoded.format(),
                        decoded.width(),
                        decoded.height(),
                        ff::format::Pixel::RGBA,
                        decoded.width(),
                        decoded.height(),
                        ff::software::scaling::Flags::BILINEAR,
                    )?);
                }
                let mut rgba = ff::frame::Video::empty();
                self.scaler.as_mut().unwrap().run(&decoded, &mut rgba)?;
                let (w, h) = (rgba.width(), rgba.height());
                let (stride, row) = (rgba.stride(0), w as usize * 4);
                let src = rgba.data(0);
                let mut data = Vec::with_capacity(row * h as usize);
                for y in 0..h as usize {
                    data.extend_from_slice(&src[y * stride..y * stride + row]);
                }
                let t = decoded
                    .timestamp()
                    .map(|p| p as f64 * self.tb)
                    .unwrap_or(self.count as f64 / self.fps);
                self.count += 1;
                let frame = Frame {
                    width: w,
                    height: h,
                    data: Arc::new(data),
                    seq: self.count,
                };
                self.last_t = t;
                self.last = Some(frame.clone());
                return Ok(Some((t, frame)));
            }
            if self.eof {
                return Ok(None);
            }
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.ictx) {
                Ok(()) => {
                    if pkt.stream() == self.stream_idx {
                        self.decoder.send_packet(&pkt)?;
                    }
                }
                Err(ff::Error::Eof) => {
                    self.eof = true;
                    self.decoder.send_eof()?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn seek(&mut self, t: f64) -> Result<(), ff::Error> {
        let ts = (t.max(0.0) * f64::from(ff::ffi::AV_TIME_BASE)) as i64;
        self.ictx.seek(ts, ..ts)?;
        self.decoder.flush();
        self.eof = false;
        self.last_t = f64::NAN;
        Ok(())
    }

    /// Frame to show at media time `target`. None when it is the frame already handed out.
    /// Close ahead of the last frame it just decodes forward; anything else seeks to the
    /// previous keyframe and decodes up to the target.
    fn frame_at(&mut self, target: f64) -> Result<Option<Frame>, ff::Error> {
        let half = 0.5 / self.fps;
        if self.last_t.is_finite() {
            if (target - self.last_t).abs() <= half {
                return Ok(None);
            }
            if target < self.last_t || target - self.last_t > 2.0 {
                self.seek(target)?;
            }
        } else {
            self.seek(target)?;
        }
        loop {
            match self.next()? {
                Some((t, f)) if t + half >= target => return Ok(Some(f)),
                Some(_) => {}
                // Past the last frame: hold it.
                None => return Ok(self.last.clone()),
            }
        }
    }
}

/// Index into `frames` (sorted by pts) of the frame nearest to `target`.
fn nearest(frames: &[(f64, Frame)], target: f64) -> usize {
    let i = frames.partition_point(|(t, _)| *t < target);
    if i == 0 {
        0
    } else if i == frames.len() || target - frames[i - 1].0 <= frames[i].0 - target {
        i - 1
    } else {
        i
    }
}

fn run_scrub(path: &Path, shared: &Shared) -> Result<(), ff::Error> {
    let mut sc = Scrubber::open(path)?;
    let mut seq = 0u64;
    let publish = |f: &Frame, seq: &mut u64| {
        *seq += 1;
        *shared.latest.lock().unwrap() = Some(Frame {
            seq: *seq,
            ..f.clone()
        });
    };
    let mut frames: Vec<(f64, Frame)> = vec![];
    let mut cached = false;
    if sc.duration > 0.0 {
        // First frame early; the size is only known after decoding it.
        if let Some((t, f)) = sc.next()? {
            let bytes = f.data.len() as f64 * sc.duration * sc.fps;
            publish(&f, &mut seq);
            if bytes <= CACHE_BYTES {
                frames.push((t, f));
                while let Some(x) = sc.next()? {
                    if shared.stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    frames.push(x);
                }
                cached = true;
            }
        }
        // Publish the duration only once frames can be served, so the
        // playhead does not advance (or finish a Once clip) while loading.
        shared
            .duration
            .store(sc.duration.to_bits(), Ordering::Relaxed);
    } else {
        // Unknown duration: decode to learn it, but never past the cache
        // budget; a longer stream is truncated to what fits.
        let mut bytes = 0.0;
        while let Some(x) = sc.next()? {
            if shared.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            bytes += x.1.data.len() as f64;
            frames.push(x);
            if bytes > CACHE_BYTES {
                break;
            }
        }
        cached = true;
        let end = frames.last().map_or(0.0, |(t, _)| t + 1.0 / sc.fps);
        shared.duration.store(end.to_bits(), Ordering::Relaxed);
        sc.duration = end;
    }
    let mut shown = f64::NAN;
    let mut shown_idx = usize::MAX;
    while !shared.stop.load(Ordering::Relaxed) {
        let pos = f64::from_bits(shared.position.load(Ordering::Relaxed));
        if pos == shown {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        shown = pos;
        let target = pos * sc.duration;
        if cached {
            if frames.is_empty() {
                return Ok(());
            }
            let i = nearest(&frames, target);
            if i != shown_idx {
                shown_idx = i;
                publish(&frames[i].1, &mut seq);
            }
        } else if let Some(f) = sc.frame_at(target)? {
            publish(&f, &mut seq);
        }
    }
    Ok(())
}

fn decode_loop(
    path: &Path,
    looping: bool,
    shared: &Shared,
    sink: &mut dyn FnMut(Frame, Instant) -> bool,
) -> Result<(), ff::Error> {
    ff::init()?;
    let mut ictx = ff::format::input(&path)?;
    let dur = ictx.duration();
    if dur > 0 {
        let secs = dur as f64 / f64::from(ff::ffi::AV_TIME_BASE);
        shared.duration.store(secs.to_bits(), Ordering::Relaxed);
    }
    let (stream_idx, tb, avg_fps, params) = {
        let st = ictx
            .streams()
            .best(ff::media::Type::Video)
            .ok_or(ff::Error::StreamNotFound)?;
        let r = st.avg_frame_rate();
        let fps = if r.denominator() != 0 && r.numerator() != 0 {
            r.numerator() as f64 / r.denominator() as f64
        } else {
            30.0
        };
        (st.index(), f64::from(st.time_base()), fps, st.parameters())
    };
    let mut decoder = ff::codec::context::Context::from_parameters(params)?
        .decoder()
        .video()?;
    let (w, h) = (decoder.width(), decoder.height());
    let mut scaler: Option<ff::software::scaling::Context> = None;
    let mut decoded = ff::frame::Video::empty();
    let mut rgba = ff::frame::Video::empty();
    let mut pool = Pool(vec![]);
    let mut seq = 0u64;
    let mut base = Instant::now();
    let mut first_pts: Option<f64> = None;
    // Pacing anchor: media time `anchor_t` plays at wall time `base`, at `cur_rate`.
    let mut cur_rate = shared.rate();
    let mut anchor_t: Option<f64> = None;
    let mut last_t = f64::NAN;
    let mut count = 0u64;
    let mut emitted_this_pass = 0u64;

    macro_rules! drain {
        () => {
            while decoder.receive_frame(&mut decoded).is_ok() {
                if scaler.is_none() {
                    scaler = Some(ff::software::scaling::Context::get(
                        decoded.format(),
                        decoded.width(),
                        decoded.height(),
                        ff::format::Pixel::RGBA,
                        decoded.width(),
                        decoded.height(),
                        ff::software::scaling::Flags::BILINEAR,
                    )?);
                }
                scaler.as_mut().unwrap().run(&decoded, &mut rgba)?;
                let (fw, fh) = (rgba.width(), rgba.height());
                let stride = rgba.stride(0);
                let src = rgba.data(0);
                let row = fw as usize * 4;
                let slot = pool.take(row * fh as usize);
                let dst = Arc::get_mut(slot).expect("pool slot unique");
                for y in 0..fh as usize {
                    dst[y * row..(y + 1) * row].copy_from_slice(&src[y * stride..y * stride + row]);
                }
                let t = decoded
                    .timestamp()
                    .map(|p| p as f64 * tb)
                    .unwrap_or(count as f64 / avg_fps);
                let first = *first_pts.get_or_insert(t);
                let rate = shared.rate();
                let at = *anchor_t.get_or_insert(first);
                if rate != cur_rate {
                    // Re-anchor at the previous frame so the rate change has no jump.
                    let (advance, anchor) = rerate(last_t, at, cur_rate);
                    base += advance;
                    anchor_t = Some(anchor);
                    cur_rate = rate;
                }
                last_t = t;
                let due = base
                    + Duration::from_secs_f64((t - anchor_t.unwrap_or(first)).max(0.0) / cur_rate);
                count += 1;
                emitted_this_pass += 1;
                seq += 1;
                let f = Frame {
                    width: fw,
                    height: fh,
                    data: slot.clone(),
                    seq,
                };
                if !sink(f, due) {
                    return Ok(());
                }
            }
        };
    }

    let _ = (w, h);
    loop {
        let mut eof = true;
        let mut sent = false;
        for (s, packet) in ictx.packets() {
            if s.index() != stream_idx {
                continue;
            }
            decoder.send_packet(&packet)?;
            sent = true;
            drain!();
        }
        let _ = (&mut eof, sent);
        decoder.send_eof()?;
        drain!();
        if !looping || emitted_this_pass == 0 {
            return Ok(());
        }
        ictx.seek(0, ..)?;
        decoder.flush();
        base = Instant::now();
        first_pts = None;
        anchor_t = None;
        // No previous frame in the new pass.
        last_t = f64::NAN;
        count = 0;
        emitted_this_pass = 0;
    }
}

/// Wall-time advance and new anchor when the rate changes. `last_t` is the
/// previous frame's pts in this pass, or NaN when there is none yet.
fn rerate(last_t: f64, anchor: f64, cur_rate: f64) -> (Duration, f64) {
    if last_t.is_nan() {
        return (Duration::ZERO, anchor);
    }
    (
        Duration::from_secs_f64((last_t - anchor).max(0.0) / cur_rate),
        last_t,
    )
}

/// Decodes up to `n` frames as fast as possible (no pacing, no loop). For tests/benchmarks.
pub fn decode_n(path: &Path, n: usize) -> Result<Vec<Frame>, ff::Error> {
    let mut out = vec![];
    decode_loop(path, false, &Shared::new(), &mut |f, _| {
        out.push(Frame {
            data: Arc::new((*f.data).clone()),
            ..f
        });
        out.len() < n
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rerate_at_pass_start_does_not_jump() {
        // Previous pass ended at 9.5 s; new pass starts with no previous frame.
        let (adv, anchor) = super::rerate(f64::NAN, 0.0, 1.0);
        assert_eq!(adv, std::time::Duration::ZERO);
        assert_eq!(anchor, 0.0);
        let (adv, anchor) = super::rerate(2.0, 1.0, 0.5);
        assert_eq!(adv, std::time::Duration::from_secs(2));
        assert_eq!(anchor, 2.0);
    }

    use super::*;

    fn fake(ts: &[f64]) -> Vec<(f64, Frame)> {
        ts.iter()
            .map(|t| {
                let f = Frame {
                    width: 1,
                    height: 1,
                    data: Arc::new(vec![0; 4]),
                    seq: 0,
                };
                (*t, f)
            })
            .collect()
    }

    #[test]
    fn nearest_picks_closest_frame_and_clamps() {
        let f = fake(&[0.0, 0.5, 1.0]);
        assert_eq!(nearest(&f, -1.0), 0);
        assert_eq!(nearest(&f, 0.2), 0);
        assert_eq!(nearest(&f, 0.3), 1);
        assert_eq!(nearest(&f, 0.9), 2);
        assert_eq!(nearest(&f, 5.0), 2);
    }

    #[test]
    fn scrub_serves_frame_at_position() {
        let dir = std::env::temp_dir().join(format!("vjc-scrub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("s.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=size=64x48:rate=10:duration=2")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&out)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("skip: cannot encode");
            return;
        }
        let mut pl = Player::open_scrub(&out);
        let wait = |pl: &Player, after: u64| {
            let t = Instant::now();
            loop {
                if let Some(f) = pl.latest_frame()
                    && f.seq > after
                {
                    return f;
                }
                assert!(t.elapsed() < Duration::from_secs(5), "{:?}", pl.error());
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let first = wait(&pl, 0);
        pl.set_position(1.0);
        let end = wait(&pl, first.seq);
        assert_ne!(end.data, first.data);
        pl.set_position(0.0);
        let back = wait(&pl, end.seq);
        assert_eq!(back.data, first.data);
        pl.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    const SAMPLE: &str = "/Applications/Resolume Arena/media/AV/Beat 001.mov";

    #[test]
    fn decodes_ten_frames() {
        let p = Path::new(SAMPLE);
        if !p.exists() {
            eprintln!("skip: {SAMPLE} missing");
            return;
        }
        let frames = decode_n(p, 10).unwrap();
        assert_eq!(frames.len(), 10);
        let f = &frames[0];
        assert_eq!(f.data.len(), (f.width * f.height * 4) as usize);
    }

    #[test]
    fn player_delivers_frames() {
        let p = Path::new(SAMPLE);
        if !p.exists() {
            return;
        }
        let mut pl = Player::open(p);
        let t = Instant::now();
        while pl.latest_frame().is_none() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(pl.latest_frame().is_some(), "{:?}", pl.error());
        pl.stop();
    }

    /// Generates tiny clips with the ffmpeg CLI (skips codecs/CLI that are unavailable).
    #[test]
    fn decodes_generated_codecs() {
        let dir = std::env::temp_dir().join(format!("vjc-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cases: [(&str, &[&str]); 4] = [
            ("h264.mp4", &["-c:v", "libx264", "-pix_fmt", "yuv420p"]),
            ("prores.mov", &["-c:v", "prores_ks"]),
            ("mjpeg.avi", &["-c:v", "mjpeg", "-pix_fmt", "yuvj420p"]),
            ("hap.mov", &["-c:v", "hap"]),
        ];
        for (name, args) in cases {
            let out = dir.join(name);
            let ok = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=320x240:rate=25:duration=1",
                ])
                .args(args)
                .arg(&out)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                eprintln!("skip {name}: cannot encode");
                continue;
            }
            let frames = decode_n(&out, 10).unwrap();
            assert_eq!(frames.len(), 10, "{name}");
            eprintln!("{name}: ok {}x{}", frames[0].width, frames[0].height);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_rate_is_clamped_and_sanitized() {
        let pl = Player::open("/nonexistent/x.mov");
        assert_eq!(pl.shared.rate(), 1.0);
        pl.set_rate(2.0);
        assert_eq!(pl.shared.rate(), 2.0);
        pl.set_rate(1000.0);
        assert_eq!(pl.shared.rate(), 20.0);
        pl.set_rate(f64::NAN);
        assert_eq!(pl.shared.rate(), 1.0);
        assert_eq!(pl.duration(), None);
    }
}
