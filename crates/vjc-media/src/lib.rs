//! Video decoding for VJCraft: one decode thread per `Player`, RGBA frames.
//! The frame shown follows a position (0..1) set by the caller (play modes and
//! directions live in the app, see replica/build-log.md, #8). Two serving modes,
//! switchable at any time from `Player::set_sequential`:
//! - streaming (forward playback): decodes sequentially, keeping only the last frame;
//! - random access (backward, bounce, scrub): the whole clip is cached in RAM when the
//!   global budget `CACHE_BUDGET_BYTES` (summed over all players) admits it, otherwise
//!   it seeks to the previous keyframe and decodes up to the target.
//!
//! FFmpeg decodes H264, ProRes, MJPEG and HAP. Live cameras are in `capture`, clip audio
//! in `audio`.

pub mod record;

use ffmpeg_next as ff;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub mod audio;
pub mod capture;

/// A decoded RGBA8 frame (tightly packed, `width * 4` bytes per row).
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Arc<Vec<u8>>,
    /// Monotonic counter, increments per decoded frame (lets the renderer skip re-uploads).
    pub seq: u64,
}

/// Global budget for fully cached clips, summed over every player.
pub const CACHE_BUDGET_BYTES: u64 = 1536 * 1024 * 1024;
/// Largest single clip cached when its duration is unknown (decoded to learn it).
const UNKNOWN_DURATION_CAP: u64 = 384 * 1024 * 1024;

static CACHE_USED: AtomicU64 = AtomicU64::new(0);

/// Pure admission rule: a clip of `need` bytes fits if `used + need <= budget`.
pub fn admit(used: u64, need: u64, budget: u64) -> bool {
    used.checked_add(need).is_some_and(|t| t <= budget)
}

/// Bytes reserved in the global cache budget; released on drop.
struct Reservation(u64);

impl Reservation {
    fn try_new(need: u64) -> Option<Reservation> {
        CACHE_USED
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                admit(used, need, CACHE_BUDGET_BYTES).then(|| used + need)
            })
            .ok()
            .map(|_| Reservation(need))
    }

    /// Accounts bytes that were already decoded (unknown-duration clips).
    fn forced(bytes: u64) -> Reservation {
        CACHE_USED.fetch_add(bytes, Ordering::SeqCst);
        Reservation(bytes)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        CACHE_USED.fetch_sub(self.0, Ordering::SeqCst);
    }
}

/// Position (0..1) to media pts, for a clip whose first frame is at `start` seconds.
pub fn pos_to_pts(start: f64, duration: f64, pos: f64) -> f64 {
    start + pos * duration
}

/// Media pts to position (0..1, clamped); inverse of `pos_to_pts`.
pub fn pts_to_pos(start: f64, duration: f64, pts: f64) -> f64 {
    if duration > 0.0 {
        ((pts - start) / duration).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub(crate) struct Shared {
    latest: Mutex<Option<Frame>>,
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    /// File duration in seconds (f64 bits); 0 = unknown.
    duration: AtomicU64,
    /// Wanted position 0..1 (f64 bits).
    position: AtomicU64,
    /// True while playback only moves forward (streaming is enough).
    sequential: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Shared {
            latest: Mutex::new(None),
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
            duration: AtomicU64::new(0),
            position: AtomicU64::new(0.0f64.to_bits()),
            sequential: AtomicBool::new(true),
        }
    }
}

pub struct Player {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    pub path: PathBuf,
    audio: Option<audio::AudioPart>,
}

impl Player {
    /// Starts decoding `path` in a background thread. Open errors are reported via `error()`.
    /// The frame shown follows `set_position`.
    pub fn open_scrub(path: impl AsRef<Path>) -> Player {
        let path = path.as_ref().to_path_buf();
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
                if let Err(e) = run_scrub(&p, &s) {
                    *s.error.lock().unwrap() = Some(e.to_string());
                }
            })
            .ok();
        Player {
            shared,
            thread,
            path,
            audio: None,
        }
    }

    /// Opens a live capture device (macOS: avfoundation index or name). Open failures and
    /// disconnects are reported via `error()`; the last frame stays available.
    pub fn open_capture(device: &str) -> Player {
        let shared = Arc::new(Shared::new());
        let (s, d) = (shared.clone(), device.to_string());
        let thread = std::thread::Builder::new()
            .name("vjc-capture".into())
            .spawn(move || {
                if let Err(e) = capture::run_capture(&d, &s) {
                    *s.error.lock().unwrap() = Some(e);
                }
            })
            .ok();
        Player {
            shared,
            thread,
            path: PathBuf::from(format!("camera:{device}")),
            audio: None,
        }
    }

    /// Runs a live source supplied by the caller (network feeds) on its own thread. `run`
    /// gets a sink: `sink(width, height, rgba)` publishes a frame (an empty frame publishes
    /// nothing) and returns false once the player was stopped, at which point `run` must return. `run` returning `Err` is
    /// reported through `error()`. `label` is shown as the player path.
    pub fn open_external<F>(label: &str, run: F) -> Player
    where
        F: FnOnce(&mut dyn FnMut(u32, u32, Vec<u8>) -> bool) -> Result<(), String> + Send + 'static,
    {
        let shared = Arc::new(Shared::new());
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("vjc-external".into())
            .spawn(move || {
                let mut seq = 0u64;
                let mut sink = |width: u32, height: u32, data: Vec<u8>| {
                    if s.stop.load(Ordering::Relaxed) {
                        return false;
                    }
                    // An empty frame is an idle probe: only reports whether to go on.
                    if width == 0 || height == 0 || data.is_empty() {
                        return true;
                    }
                    seq += 1;
                    *s.latest.lock().unwrap() = Some(Frame {
                        width,
                        height,
                        data: Arc::new(data),
                        seq,
                    });
                    true
                };
                if let Err(e) = run(&mut sink) {
                    *s.error.lock().unwrap() = Some(e);
                }
            })
            .ok();
        Player {
            shared,
            thread,
            path: PathBuf::from(label),
            audio: None,
        }
    }

    /// Starts decoding the file's audio stream into `engine`. No-op for images, captures and
    /// files without audio. Audio follows this player's position (see `audio` module docs).
    pub fn attach_audio(&mut self, engine: &audio::AudioEngine) {
        if self.audio.is_none()
            && !is_image(&self.path)
            && !self.path.starts_with("camera:")
            && !self.path.starts_with("net:")
        {
            self.audio = audio::AudioPart::start(engine, &self.path, self.shared.clone());
        }
    }

    /// Per-frame audio control: linear volume, pan (-1..1) and whether the clip plays
    /// forward at 1x (otherwise it is muted).
    pub fn set_audio(&self, volume: f32, pan: f32, audible: bool) {
        if let Some(a) = &self.audio {
            a.voice.set(volume, pan, audible);
        }
    }

    /// Sets the wanted position (0..1, clamped).
    pub fn set_position(&self, pos: f64) {
        let p = if pos.is_finite() {
            pos.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.shared.position.store(p.to_bits(), Ordering::Relaxed);
    }

    /// Access hint: true when the position only advances (streaming, no cache), false when
    /// it jumps around (cache if the global budget admits it, else keyframe seeks).
    /// May change at any time; the decode thread switches without blocking the caller.
    pub fn set_sequential(&self, sequential: bool) {
        self.shared.sequential.store(sequential, Ordering::Relaxed);
    }

    pub fn latest_frame(&self) -> Option<Frame> {
        self.shared.latest.lock().unwrap().clone()
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Duration of the file in seconds, once known (None for images/unknown).
    pub fn duration(&self) -> Option<f64> {
        let d = f64::from_bits(self.shared.duration.load(Ordering::Relaxed));
        (d > 0.0).then_some(d)
    }

    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(a) = self.audio.take() {
            a.stop();
        }
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

/// Converts a decoded frame to a tightly packed RGBA `Frame`, creating the scaler on first use.
pub(crate) fn frame_to_rgba(
    scaler: &mut Option<ff::software::scaling::Context>,
    decoded: &ff::frame::Video,
    seq: u64,
) -> Result<Frame, ff::Error> {
    if scaler.is_none() {
        *scaler = Some(ff::software::scaling::Context::get(
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
    scaler.as_mut().unwrap().run(decoded, &mut rgba)?;
    let (w, h) = (rgba.width(), rgba.height());
    let (stride, row) = (rgba.stride(0), w as usize * 4);
    let src = rgba.data(0);
    let mut data = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        data.extend_from_slice(&src[y * stride..y * stride + row]);
    }
    Ok(Frame {
        width: w,
        height: h,
        data: Arc::new(data),
        seq,
    })
}

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

    /// Converts a decoded frame to RGBA.
    fn convert(&mut self, decoded: &ff::frame::Video) -> Result<Frame, ff::Error> {
        self.count += 1;
        frame_to_rgba(&mut self.scaler, decoded, self.count)
    }

    /// Next frame in decode order, or None at end of file. Decode errors on a packet or
    /// frame skip it with a warning; they never end the stream.
    fn next(&mut self) -> Option<(f64, Frame)> {
        let mut decoded = ff::frame::Video::empty();
        loop {
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                let t = decoded
                    .timestamp()
                    .map(|p| p as f64 * self.tb)
                    .unwrap_or(self.count as f64 / self.fps);
                match self.convert(&decoded) {
                    Ok(frame) => {
                        self.last_t = t;
                        self.last = Some(frame.clone());
                        return Some((t, frame));
                    }
                    Err(e) => {
                        eprintln!("vjc-media: frame at {t:.3}s skipped: {e}");
                        continue;
                    }
                }
            }
            if self.eof {
                return None;
            }
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.ictx) {
                Ok(()) => {
                    if pkt.stream() == self.stream_idx
                        && let Err(e) = self.decoder.send_packet(&pkt)
                    {
                        eprintln!("vjc-media: packet skipped: {e}");
                    }
                }
                Err(e) => {
                    if !matches!(e, ff::Error::Eof) {
                        eprintln!("vjc-media: read error, treating as end of file: {e}");
                    }
                    self.eof = true;
                    let _ = self.decoder.send_eof();
                }
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
    /// Close ahead of the last frame it just decodes forward (streaming); anything else
    /// seeks to the previous keyframe and decodes up to the target.
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
            match self.next() {
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

/// Whole clip in RAM, plus its share of the global budget.
struct Cache {
    frames: Vec<(f64, Frame)>,
    _res: Reservation,
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
    // First frame: shown early, gives the first pts (start_time may not be 0) and the size.
    let Some((start, first)) = sc.next() else {
        return Err(ff::Error::StreamNotFound);
    };
    publish(&first, &mut seq);
    let frame_bytes = first.data.len() as u64;
    let mut cache: Option<Cache> = None;
    // Unknown duration: decode once to learn it (bounded), then it is always cached.
    if sc.duration <= 0.0 {
        let mut frames = vec![(start, first)];
        let mut bytes = frame_bytes;
        while bytes < UNKNOWN_DURATION_CAP {
            if shared.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            match sc.next() {
                Some(x) => {
                    bytes += x.1.data.len() as u64;
                    frames.push(x);
                }
                None => break,
            }
        }
        sc.duration = frames.last().map_or(0.0, |(t, _)| t + 1.0 / sc.fps - start);
        cache = Some(Cache {
            frames,
            _res: Reservation::forced(bytes),
        });
    }
    // Publish the duration only once frames can be served, so the
    // playhead does not advance (or finish a Once clip) while loading.
    shared
        .duration
        .store(sc.duration.to_bits(), Ordering::Relaxed);
    let unknown = cache.is_some();
    let need = (frame_bytes as f64 * sc.duration * sc.fps) as u64;
    let mut shown = f64::NAN;
    let mut shown_idx = usize::MAX;
    let mut was_sequential = true;
    while !shared.stop.load(Ordering::Relaxed) {
        let sequential = shared.sequential.load(Ordering::Relaxed);
        if sequential != was_sequential {
            // Mode switch: streaming drops the cache and frees its budget.
            was_sequential = sequential;
            shown = f64::NAN;
            shown_idx = usize::MAX;
            if sequential && !unknown {
                cache = None;
                // Resume decoding from the keyframe of the current position.
                sc.last_t = f64::NAN;
            }
        }
        if !sequential
            && !unknown
            && cache.is_none()
            && let Some(res) = Reservation::try_new(need)
        {
            match load_all(&mut sc, start, shared) {
                Some(frames) => {
                    cache = Some(Cache { frames, _res: res });
                    shown = f64::NAN;
                }
                None => return Ok(()),
            }
        }
        let pos = f64::from_bits(shared.position.load(Ordering::Relaxed));
        if pos == shown {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        shown = pos;
        let target = pos_to_pts(start, sc.duration, pos);
        // Streaming uses the cache only if one is left over from unknown duration.
        match &cache {
            Some(c) if !c.frames.is_empty() => {
                let i = nearest(&c.frames, target);
                if i != shown_idx {
                    shown_idx = i;
                    publish(&c.frames[i].1, &mut seq);
                }
            }
            _ => match sc.frame_at(target) {
                Ok(Some(f)) => publish(&f, &mut seq),
                Ok(None) => {}
                Err(e) => eprintln!("vjc-media: seek to {target:.3}s failed: {e}"),
            },
        }
    }
    Ok(())
}

/// Decodes the whole clip from the start. None if the player was stopped meanwhile.
fn load_all(sc: &mut Scrubber, start: f64, shared: &Shared) -> Option<Vec<(f64, Frame)>> {
    if let Err(e) = sc.seek(start) {
        eprintln!("vjc-media: rewind failed: {e}");
    }
    let mut frames = vec![];
    while let Some(x) = sc.next() {
        if shared.stop.load(Ordering::Relaxed) {
            return None;
        }
        frames.push(x);
    }
    sc.last_t = f64::NAN;
    Some(frames)
}

/// First frame of a video or the image itself, shrunk so the longest side is at most
/// `max_side` (box-free nearest sampling; enough for browser thumbnails).
pub fn thumbnail(path: &Path, max_side: u32) -> Result<Frame, String> {
    let frame = if is_image(path) {
        load_image(path)?
    } else {
        decode_n(path, 1)
            .map_err(|e| e.to_string())?
            .into_iter()
            .next()
            .ok_or("no frames")?
    };
    Ok(downscale(&frame, max_side))
}

fn downscale(f: &Frame, max_side: u32) -> Frame {
    let long = f.width.max(f.height);
    if long <= max_side || f.width == 0 || f.height == 0 {
        return f.clone();
    }
    let w = (f.width as u64 * max_side as u64 / long as u64).max(1) as u32;
    let h = (f.height as u64 * max_side as u64 / long as u64).max(1) as u32;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let sy = (y as u64 * f.height as u64 / h as u64) as usize;
        for x in 0..w {
            let sx = (x as u64 * f.width as u64 / w as u64) as usize;
            let i = (sy * f.width as usize + sx) * 4;
            out.extend_from_slice(&f.data[i..i + 4]);
        }
    }
    Frame {
        width: w,
        height: h,
        data: Arc::new(out),
        seq: 1,
    }
}

/// Decodes up to `n` frames as fast as possible (no pacing). For thumbnails and tests.
pub fn decode_n(path: &Path, n: usize) -> Result<Vec<Frame>, ff::Error> {
    let mut sc = Scrubber::open(path)?;
    let mut out = vec![];
    while out.len() < n {
        match sc.next() {
            Some((_, f)) => out.push(f),
            None => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn thumbnail_of_generated_png_is_shrunk() {
        let dir = std::env::temp_dir().join(format!("vjc-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        let img = image::RgbaImage::from_pixel(320, 160, image::Rgba([255, 0, 0, 255]));
        if img.save(&p).is_err() {
            return; // cannot write here: skip
        }
        let t = thumbnail(&p, 160).unwrap();
        assert_eq!((t.width, t.height), (160, 80));
        assert_eq!(&t.data[..4], &[255, 0, 0, 255]);
        std::fs::remove_dir_all(&dir).ok();
    }

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
        let mut pl = Player::open_scrub(p);
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
    fn set_position_and_sequential_are_sanitized() {
        let pl = Player::open_scrub("/nonexistent/x.mov");
        pl.set_position(f64::NAN);
        assert_eq!(
            f64::from_bits(pl.shared.position.load(Ordering::Relaxed)),
            0.0
        );
        pl.set_position(7.0);
        assert_eq!(
            f64::from_bits(pl.shared.position.load(Ordering::Relaxed)),
            1.0
        );
        pl.set_sequential(false);
        assert!(!pl.shared.sequential.load(Ordering::Relaxed));
        assert_eq!(pl.duration(), None);
    }

    #[test]
    fn admit_respects_global_budget() {
        assert!(admit(0, 100, 100));
        assert!(admit(40, 60, 100));
        assert!(!admit(41, 60, 100));
        assert!(!admit(u64::MAX, 1, 100));
        assert!(!admit(0, u64::MAX, 100));
    }

    #[test]
    fn pos_pts_mapping_with_start_offset() {
        assert_eq!(pos_to_pts(5.0, 2.0, 0.0), 5.0);
        assert_eq!(pos_to_pts(5.0, 2.0, 0.5), 6.0);
        assert_eq!(pts_to_pos(5.0, 2.0, 6.0), 0.5);
        assert_eq!(pts_to_pos(5.0, 2.0, 4.0), 0.0);
        assert_eq!(pts_to_pos(5.0, 2.0, 9.0), 1.0);
        assert_eq!(pts_to_pos(5.0, 0.0, 9.0), 0.0);
    }

    fn encode(out: &Path, extra: &[&str]) -> bool {
        std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=size=64x48:rate=10:duration=2")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args(extra)
            .arg(out)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn start_time_offset_maps_to_first_pts() {
        let dir = std::env::temp_dir().join(format!("vjc-off-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("o.mp4");
        if !encode(&out, &["-output_ts_offset", "5"]) {
            eprintln!("skip: cannot encode");
            return;
        }
        let mut sc = Scrubber::open(&out).unwrap();
        let (start, first) = sc.next().unwrap();
        assert!(start >= 4.9, "first pts {start}");
        // Position 0 and 1 are the first and last frames, not frames before the offset.
        let mut pl = Player::open_scrub(&out);
        let t = Instant::now();
        while pl.duration().is_none() {
            assert!(t.elapsed() < Duration::from_secs(5), "{:?}", pl.error());
            std::thread::sleep(Duration::from_millis(5));
        }
        pl.set_position(0.0);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(pl.latest_frame().unwrap().data, first.data);
        pl.set_position(1.0);
        let t = Instant::now();
        while pl.latest_frame().unwrap().data == first.data {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        pl.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn streaming_decodes_forward_without_cache_and_mode_switch_works() {
        let dir = std::env::temp_dir().join(format!("vjc-str-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("s.mp4");
        if !encode(&out, &[]) {
            eprintln!("skip: cannot encode");
            return;
        }
        assert_eq!(decode_n(&out, 100).unwrap().len(), 20);
        let mut pl = Player::open_scrub(&out);
        pl.set_sequential(true);
        let mut last = 0;
        for i in 0..=10 {
            pl.set_position(i as f64 / 10.0);
            std::thread::sleep(Duration::from_millis(30));
            last = pl.latest_frame().map_or(0, |f| f.seq);
        }
        assert!(last > 1, "{:?}", pl.error());
        pl.set_sequential(false); // switch to random access mid-play
        pl.set_position(0.0);
        std::thread::sleep(Duration::from_millis(300));
        assert!(pl.latest_frame().unwrap().seq > last);
        pl.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_file_does_not_kill_the_thread() {
        let dir = std::env::temp_dir().join(format!("vjc-trunc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.mjpeg.avi");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=size=64x48:rate=10:duration=2")
            .args(["-c:v", "mjpeg", "-pix_fmt", "yuvj420p"])
            .arg(&out)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("skip: cannot encode");
            return;
        }
        let mut bytes = std::fs::read(&out).unwrap();
        // Corrupt the middle (keeps the header/index readable) and cut the tail.
        let mid = bytes.len() / 2;
        for b in &mut bytes[mid..mid + 200] {
            *b = 0xA5;
        }
        bytes.truncate(bytes.len() - 300);
        std::fs::write(&out, &bytes).unwrap();
        let mut pl = Player::open_scrub(&out);
        let t = Instant::now();
        while pl.latest_frame().is_none() {
            assert!(t.elapsed() < Duration::from_secs(5), "{:?}", pl.error());
            std::thread::sleep(Duration::from_millis(5));
        }
        for p in [1.0, 0.0, 0.7] {
            pl.set_position(p);
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(pl.error().is_none(), "{:?}", pl.error());
        assert!(!pl.thread.as_ref().unwrap().is_finished());
        pl.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
