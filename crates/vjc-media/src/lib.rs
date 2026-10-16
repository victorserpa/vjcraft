//! Video decoding for VJCraft: one decode thread per `Player`, RGBA frames.
//! The frame shown follows a position (0..1) set by the caller (play modes and
//! directions live in the app, #8). Two serving modes,
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
use std::time::{Duration, Instant};

pub mod analysis;
pub mod audio;
pub mod audio_fx;
pub mod capture;
pub mod ltc_in;
pub mod sequence;
pub mod snapshot;

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

/// Locks ignoring poison: a panicked decoder thread must not turn the UI thread's next
/// read of the slot into a second panic.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Text of a panic payload.
pub fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Runs a player thread body; a panic marks the player failed (`error()`) instead of
/// leaving a silent dead thread, and never reaches the rest of the app.
fn guarded(shared: &Shared, what: &str, body: impl FnOnce()) {
    if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        *lock(&shared.error) = Some(format!("{what} crashed: {}", panic_text(&*p)));
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
                guarded(&s.clone(), "decoder", move || {
                    if let Some((pattern, fps)) = sequence::parse_label(&p) {
                        run_sequence(&pattern, fps, &s);
                        return;
                    }
                    if is_image(&p) {
                        match load_image(&p) {
                            Ok(f) => *lock(&s.latest) = Some(f),
                            Err(e) => *lock(&s.error) = Some(e),
                        }
                        return;
                    }
                    if let Err(e) = run_scrub(&p, &s) {
                        *lock(&s.error) = Some(e.to_string());
                    }
                })
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
                guarded(&s.clone(), "capture", || {
                    if let Err(e) = capture::run_capture(&d, &s) {
                        *lock(&s.error) = Some(e);
                    }
                })
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
                guarded(&s.clone(), "source", || {
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
                        *lock(&s.latest) = Some(Frame {
                            width,
                            height,
                            data: Arc::new(data),
                            seq,
                        });
                        true
                    };
                    if let Err(e) = run(&mut sink) {
                        *lock(&s.error) = Some(e);
                    }
                })
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
        if self.audio.is_none() && !is_image(&self.path) && !is_live_label(&self.path) {
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

    /// Per-frame audio effects: the clip's chain and the layer bus (1-based, 0 = none) it
    /// sums into.
    pub fn set_audio_fx(&self, bus: u32, specs: &[audio_fx::FxSpec], rate: u32, bpm: f32) {
        if let Some(a) = &self.audio {
            a.voice.set_bus(bus);
            a.voice.fx.set(specs, rate, bpm);
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
        lock(&self.shared.latest).clone()
    }

    pub fn error(&self) -> Option<String> {
        lock(&self.shared.error).clone()
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

/// Camera and network players carry a `camera:`/`net:` label as path, not a file.
fn is_live_label(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("camera:") || s.starts_with("net:") || sequence::parse_label(path).is_some()
}

/// Plays an image sequence: shows the frame for the wanted position (0..1 over n/fps
/// seconds). Decoded frames are kept up to a byte budget so loops do not re-decode.
fn run_sequence(pattern: &Path, fps: f32, shared: &Shared) {
    let files = sequence::resolve_pattern(pattern);
    if files.is_empty() {
        *lock(&shared.error) = Some("no frames match the sequence".into());
        return;
    }
    let n = files.len();
    let mut cache: Vec<Option<Frame>> = vec![None; n];
    let (mut used, budget) = (0u64, 256u64 * 1024 * 1024);
    let (mut seq, mut shown) = (0u64, usize::MAX);
    let mut announced = false;
    while !shared.stop.load(Ordering::Relaxed) {
        let pos = f64::from_bits(shared.position.load(Ordering::Relaxed));
        let i = sequence::frame_index(pos, n);
        if i != shown {
            let f = match &cache[i] {
                Some(f) => Some(f.clone()),
                None => match load_image(&files[i]) {
                    Ok(f) => {
                        if used + f.data.len() as u64 <= budget {
                            used += f.data.len() as u64;
                            cache[i] = Some(f.clone());
                        }
                        Some(f)
                    }
                    Err(e) => {
                        *lock(&shared.error) = Some(e);
                        None
                    }
                },
            };
            if let Some(f) = f {
                seq += 1;
                *lock(&shared.latest) = Some(Frame { seq, ..f });
                shown = i;
                if !announced {
                    announced = true;
                    let d = n as f64 / f64::from(fps);
                    shared.duration.store(d.to_bits(), Ordering::Relaxed);
                }
            } else {
                shown = i;
            }
        }
        std::thread::sleep(Duration::from_millis(4));
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

/// What a video file is, read from its container without decoding a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceInfo {
    /// Codec short name as FFmpeg knows it ("h264", "hap", "prores"...).
    pub codec: String,
    /// Frame size in px; 0 x 0 when the decoder could not be opened.
    pub width: u32,
    pub height: u32,
    /// Average frame rate; 0 when the container does not say.
    pub fps: f64,
    /// Length of the file in seconds; 0 when unknown.
    pub duration: f64,
}

/// Reads codec, size, frame rate and length of a video file. Blocks on file I/O: call it
/// off the UI thread. None when the file cannot be opened or has no video stream.
pub fn probe(path: impl AsRef<Path>) -> Option<SourceInfo> {
    ff::init().ok()?;
    let ictx = ff::format::input(&path).ok()?;
    let st = ictx.streams().best(ff::media::Type::Video)?;
    let r = st.avg_frame_rate();
    let fps = if r.denominator() != 0 && r.numerator() != 0 {
        r.numerator() as f64 / r.denominator() as f64
    } else {
        0.0
    };
    let params = st.parameters();
    let codec = params.id().name().to_string();
    let (width, height) = ff::codec::context::Context::from_parameters(params)
        .ok()
        .and_then(|c| c.decoder().video().ok())
        .map_or((0, 0), |d| (d.width(), d.height()));
    let duration = ictx.duration().max(0) as f64 / f64::from(ff::ffi::AV_TIME_BASE);
    Some(SourceInfo {
        codec,
        width,
        height,
        fps,
        duration,
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
    /// Bytes of packets (all streams) read from the container so far.
    bytes_in: u64,
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
            bytes_in: 0,
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
                    self.bytes_in += pkt.size() as u64;
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

/// Duration of a clip without container duration: first pts to last pts plus one frame.
fn unknown_duration(start: f64, last: f64, fps: f64) -> f64 {
    (last + 1.0 / fps - start).max(0.0)
}

/// Result of `learn_duration`: duration, whether it is exact (false = estimate to refine
/// with `measure_full`), and the frames when the whole clip fit in the cache budget.
struct Learned {
    duration: f64,
    exact: bool,
    cache: Option<(Vec<(f64, Frame)>, u64)>,
}

/// Longest time spent measuring a long clip before an estimated duration is published.
const LEARN_LIMIT: Duration = Duration::from_secs(2);

/// Duration guess from how much of the file (`bytes_in` of `size`) covered `span` seconds.
fn estimate_duration(span: f64, bytes_in: u64, size: u64) -> f64 {
    if bytes_in == 0 || size == 0 {
        return span;
    }
    let frac = (bytes_in as f64 / size as f64).clamp(0.001, 1.0);
    span / frac
}

/// Learns the duration of a clip that has none in its container by decoding it. Short
/// clips (under `cap` bytes) come back cached; longer ones are measured without keeping
/// frames, for at most `limit`, after which an estimate from the file size is returned
/// (`exact` false). None if the player was stopped meanwhile. The scrubber is left so
/// the next `frame_at` seeks from scratch.
fn learn_duration(
    sc: &mut Scrubber,
    start: f64,
    first: Frame,
    shared: &Shared,
    cap: u64,
    limit: Duration,
    file_size: u64,
) -> Option<Learned> {
    let t0 = Instant::now();
    let mut bytes = first.data.len() as u64;
    let mut frames = vec![(start, first)];
    let mut capped = false;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return None;
        }
        if bytes >= cap {
            capped = true;
            break;
        }
        match sc.next() {
            Some(x) => {
                bytes += x.1.data.len() as u64;
                frames.push(x);
            }
            None => break,
        }
    }
    if !capped {
        let last = frames.last().map_or(start, |(t, _)| *t);
        sc.last_t = f64::NAN;
        return Some(Learned {
            duration: unknown_duration(start, last, sc.fps),
            exact: true,
            cache: Some((frames, bytes)),
        });
    }
    let mut last = frames.last().map_or(start, |(t, _)| *t);
    drop(frames);
    let mut exact = true;
    while let Some((t, _)) = sc.next() {
        if shared.stop.load(Ordering::Relaxed) {
            return None;
        }
        last = last.max(t);
        if t0.elapsed() >= limit {
            exact = false;
            break;
        }
    }
    let mut duration = unknown_duration(start, last, sc.fps);
    if !exact {
        duration = estimate_duration(duration, sc.bytes_in, file_size).max(duration);
    }
    sc.last_t = f64::NAN;
    Some(Learned {
        duration,
        exact,
        cache: None,
    })
}

/// Full measure of a clip without a container duration, on its own decoder. None if stopped.
fn measure_full(path: &Path, shared: &Shared) -> Option<f64> {
    let mut sc = Scrubber::open(path).ok()?;
    let (start, _) = sc.next()?;
    let mut last = start;
    while let Some((t, _)) = sc.next() {
        if shared.stop.load(Ordering::Relaxed) {
            return None;
        }
        last = last.max(t);
    }
    Some(unknown_duration(start, last, sc.fps))
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
        *lock(&shared.latest) = Some(Frame {
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
    // Long clips publish an estimate after LEARN_LIMIT and refine it on a side thread.
    let mut exact = true;
    if sc.duration <= 0.0 {
        let size = std::fs::metadata(path).map_or(0, |m| m.len());
        match learn_duration(
            &mut sc,
            start,
            first,
            shared,
            UNKNOWN_DURATION_CAP,
            LEARN_LIMIT,
            size,
        ) {
            None => return Ok(()),
            Some(l) => {
                sc.duration = l.duration;
                exact = l.exact;
                if let Some((frames, bytes)) = l.cache {
                    cache = Some(Cache {
                        frames,
                        _res: Reservation::forced(bytes),
                    });
                }
            }
        }
    }
    // Publish the duration only once frames can be served, so the
    // playhead does not advance (or finish a Once clip) while loading.
    shared
        .duration
        .store(sc.duration.to_bits(), Ordering::Relaxed);
    let refined = AtomicU64::new(f64::NAN.to_bits());
    std::thread::scope(|scope| {
        if !exact {
            scope.spawn(|| {
                if let Some(d) = measure_full(path, shared) {
                    refined.store(d.to_bits(), Ordering::Relaxed);
                }
            });
        }
        scrub_loop(
            &mut sc,
            start,
            frame_bytes,
            cache,
            shared,
            &refined,
            &mut seq,
            &publish,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn scrub_loop(
    sc: &mut Scrubber,
    start: f64,
    frame_bytes: u64,
    mut cache: Option<Cache>,
    shared: &Shared,
    refined: &AtomicU64,
    seq: &mut u64,
    publish: &dyn Fn(&Frame, &mut u64),
) -> Result<(), ff::Error> {
    let unknown = cache.is_some();
    let mut need = (frame_bytes as f64 * sc.duration * sc.fps) as u64;
    let mut shown = f64::NAN;
    let mut shown_idx = usize::MAX;
    let mut was_sequential = true;
    while !shared.stop.load(Ordering::Relaxed) {
        let r = f64::from_bits(refined.load(Ordering::Relaxed));
        if r.is_finite() && r > 0.0 && r != sc.duration {
            // The side thread measured the real length: replace the estimate.
            sc.duration = r;
            need = (frame_bytes as f64 * r * sc.fps) as u64;
            shared.duration.store(r.to_bits(), Ordering::Relaxed);
            shown = f64::NAN;
        }
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
            match load_all(sc, start, shared) {
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
                    publish(&c.frames[i].1, seq);
                }
            }
            _ => match sc.frame_at(target) {
                Ok(Some(f)) => publish(&f, seq),
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
    // Corrupt media can trip a panic in a decoder: report it as a failed thumbnail.
    std::panic::catch_unwind(|| thumbnail_inner(path, max_side))
        .unwrap_or_else(|p| Err(format!("thumbnail crashed: {}", panic_text(&*p))))
}

fn thumbnail_inner(path: &Path, max_side: u32) -> Result<Frame, String> {
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

    #[test]
    fn a_panicking_source_marks_that_player_failed_and_others_keep_running() {
        let bad = Player::open_external("bad", |_sink| panic!("boom in decoder"));
        let good = Player::open_external("good", |sink| {
            sink(2, 2, vec![255; 16]);
            Ok(())
        });
        let t = Instant::now();
        while (bad.error().is_none() || good.latest_frame().is_none())
            && t.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let e = bad.error().expect("panic must surface as an error");
        assert!(e.contains("boom in decoder"), "{e}");
        assert!(good.error().is_none());
        assert!(good.latest_frame().is_some());
        // The slots stay usable after the panic (no poison).
        assert!(bad.latest_frame().is_none());
    }

    #[test]
    fn live_labels_are_not_files() {
        assert!(is_live_label(Path::new("camera:0")));
        assert!(is_live_label(Path::new("net:syphon")));
        assert!(!is_live_label(Path::new("/tmp/camera:0.mp4")));
        assert!(!Path::new("camera:0").starts_with("camera:")); // the old guard never matched
    }
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
    fn unknown_duration_beyond_cap_is_not_truncated() {
        let dir = std::env::temp_dir().join(format!("vjc-unk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("u.mp4");
        if !encode(&out, &[]) {
            eprintln!("skip: cannot encode");
            return;
        }
        let shared = Shared::new();
        let mut sc = Scrubber::open(&out).unwrap();
        let (start, first) = sc.next().unwrap();
        // Cap of a few frames: the clip (20 frames, 2 s) must still be measured whole.
        let cap = first.data.len() as u64 * 3;
        let big = Duration::from_secs(60);
        let l = learn_duration(&mut sc, start, first.clone(), &shared, cap, big, 0).unwrap();
        assert!(l.cache.is_none() && l.exact);
        assert!((l.duration - 2.0).abs() < 0.15, "duration {}", l.duration);
        // After learning, the scrubber must serve the start again (rewind via seek).
        let f = sc.frame_at(start).unwrap().unwrap();
        assert_eq!(f.data, first.data);
        // A time limit stops the measure early and flags an estimate, never below the span.
        let mut sc = Scrubber::open(&out).unwrap();
        let (start, first) = sc.next().unwrap();
        let size = std::fs::metadata(&out).unwrap().len();
        let l = learn_duration(&mut sc, start, first, &shared, cap, Duration::ZERO, size).unwrap();
        assert!(!l.exact && l.cache.is_none());
        assert!(l.duration >= 0.3, "estimate {}", l.duration);
        assert_eq!(estimate_duration(1.0, 10, 100), 10.0);
        assert_eq!(estimate_duration(1.0, 0, 100), 1.0);
        std::fs::remove_dir_all(&dir).ok();
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
