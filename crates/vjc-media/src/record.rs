//! Output recording: RGBA frames in, a video file (or a PNG sequence) out. Encoding runs on
//! its own thread behind a bounded queue; when the queue is full the frame is dropped and
//! counted, so the render thread never blocks. The codec is a choice (`RecCodec`); the
//! list of what this build can encode is checked at runtime (`available`). H.264 falls back
//! to mpeg4 when no H.264 encoder exists, as before.

use ffmpeg_next as ff;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

/// What to record with. Ids are stored in the preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecCodec {
    H264,
    Hevc,
    ProRes,
    /// One PNG per frame in a folder (lossless, no audio).
    PngSeq,
}

impl RecCodec {
    pub const ALL: [RecCodec; 4] = [
        RecCodec::H264,
        RecCodec::Hevc,
        RecCodec::ProRes,
        RecCodec::PngSeq,
    ];

    pub fn id(self) -> &'static str {
        match self {
            RecCodec::H264 => "h264",
            RecCodec::Hevc => "hevc",
            RecCodec::ProRes => "prores",
            RecCodec::PngSeq => "png",
        }
    }

    pub fn from_id(id: &str) -> Option<RecCodec> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            RecCodec::H264 => "H.264 (.mp4)",
            RecCodec::Hevc => "HEVC / H.265 (.mp4)",
            RecCodec::ProRes => "Apple ProRes 422 (.mov)",
            RecCodec::PngSeq => "PNG sequence (folder)",
        }
    }

    /// File extension of the output; empty for the PNG sequence, which is a folder.
    pub fn extension(self) -> &'static str {
        match self {
            RecCodec::H264 | RecCodec::Hevc => "mp4",
            RecCodec::ProRes => "mov",
            RecCodec::PngSeq => "",
        }
    }

    /// Encoders that produce this codec, best first. `legacy` adds the MPEG-4 stand-in
    /// that H.264 recordings have always used when no H.264 encoder exists.
    fn encoders(self, legacy: bool) -> Vec<&'static str> {
        let mut v = match self {
            RecCodec::H264 => vec!["libx264", "h264_videotoolbox"],
            RecCodec::Hevc => vec!["libx265", "hevc_videotoolbox"],
            RecCodec::ProRes => vec!["prores_ks", "prores_videotoolbox", "prores"],
            RecCodec::PngSeq => vec!["png"],
        };
        if legacy && self == RecCodec::H264 {
            v.push("mpeg4");
        }
        v
    }

    fn pixel(self) -> ff::format::Pixel {
        match self {
            RecCodec::H264 | RecCodec::Hevc => ff::format::Pixel::YUV420P,
            RecCodec::ProRes => ff::format::Pixel::YUV422P10LE,
            RecCodec::PngSeq => ff::format::Pixel::RGB24,
        }
    }
}

/// The codecs `has` (an encoder-name lookup) can encode.
pub fn available_with(has: impl Fn(&str) -> bool) -> Vec<RecCodec> {
    RecCodec::ALL
        .into_iter()
        .filter(|c| c.encoders(false).into_iter().any(&has))
        .collect()
}

/// The codecs this ffmpeg build can encode, checked now.
pub fn available() -> Vec<RecCodec> {
    if ff::init().is_err() {
        return vec![];
    }
    available_with(|n| ff::encoder::find_by_name(n).is_some())
}

/// Codec probe run on its own thread: `get` never blocks the caller (the UI thread) and
/// is empty until the probe finishes.
#[derive(Clone, Default)]
pub struct Probe(Arc<OnceLock<Vec<RecCodec>>>);

impl Probe {
    /// Runs `f` on a new thread.
    pub fn start(f: impl FnOnce() -> Vec<RecCodec> + Send + 'static) -> Self {
        let cell = Arc::new(OnceLock::new());
        let c = cell.clone();
        std::thread::spawn(move || {
            let _ = c.set(f());
        });
        Self(cell)
    }

    /// The codecs found so far (none while still probing).
    pub fn get(&self) -> &[RecCodec] {
        self.0.get().map_or(&[], Vec::as_slice)
    }
}

/// Codec and quality of a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecSettings {
    pub codec: RecCodec,
    /// 1 (small) to 100 (best); the PNG sequence ignores it.
    pub quality: u8,
}

impl Default for RecSettings {
    fn default() -> Self {
        RecSettings {
            codec: RecCodec::H264,
            quality: 70,
        }
    }
}

impl RecSettings {
    /// From stored preferences; an unknown or unavailable codec becomes H.264.
    pub fn from_prefs(codec: &str, quality: u8, available: &[RecCodec]) -> Self {
        let codec = RecCodec::from_id(codec)
            .filter(|c| available.contains(c))
            .unwrap_or(RecCodec::H264);
        RecSettings {
            codec,
            quality: quality.clamp(1, 100),
        }
    }
}

/// Encoder options for `enc_name` at `quality`: (name, value) pairs, plus a bit rate for
/// the encoders that take one instead of a constant-quality factor.
pub fn encoder_options(
    enc_name: &str,
    quality: u8,
    (w, h, fps): (u32, u32, u32),
) -> (Vec<(&'static str, String)>, Option<usize>) {
    let q = u32::from(quality.clamp(1, 100));
    // x264 CRF: 70 -> 20 (the old fixed value), 100 -> 13, 1 -> 35.
    let crf = 35 - q * 22 / 100;
    let bits_per_pixel = 0.02 + f64::from(q) / 100.0 * 0.18;
    let rate = (f64::from(w) * f64::from(h) * f64::from(fps) * bits_per_pixel) as usize;
    match enc_name {
        "libx264" => (
            vec![("preset", "veryfast".into()), ("crf", crf.to_string())],
            None,
        ),
        "libx265" => (
            vec![
                ("preset", "veryfast".into()),
                ("crf", (crf + 4).to_string()),
                ("x265-params", "log-level=error".into()),
            ],
            None,
        ),
        "h264_videotoolbox" | "hevc_videotoolbox" => (vec![], Some(rate)),
        "prores_ks" => {
            let profile = match q {
                0..=24 => 0,
                25..=49 => 1,
                50..=79 => 2,
                _ => 3,
            };
            (
                vec![("profile", profile.to_string()), ("vendor", "apl0".into())],
                None,
            )
        }
        "mpeg4" => (vec![], Some(8_000_000)),
        _ => (vec![], None),
    }
}

/// Master audio to record next to the video: the mixer tap and the device sample rate it
/// is mixed at.
#[derive(Clone)]
pub struct AudioFeed {
    pub tap: Arc<crate::audio::Tap>,
    pub rate: u32,
}

/// Stops the tap when the worker ends, however it ends.
struct StopTap(Option<(Arc<crate::audio::Tap>, u64)>);

impl Drop for StopTap {
    fn drop(&mut self) {
        if let Some((t, rec_id)) = &self.0 {
            t.stop_rec(*rec_id);
        }
    }
}

/// AAC stream of the master audio: drains the tap, cuts it into encoder-sized frames.
struct AudioEnc {
    enc: ff::encoder::Audio,
    tap: Arc<crate::audio::Tap>,
    rec_id: u64,
    rate: u32,
    in_tb: ff::Rational,
    out_tb: ff::Rational,
    stream: usize,
    left: Vec<f32>,
    right: Vec<f32>,
    next_pts: i64,
    frame: usize,
}

const AUDIO_SAMPLE: ff::format::Sample = ff::format::Sample::F32(ff::format::sample::Type::Planar);

impl AudioEnc {
    /// Opens the AAC encoder and adds its stream to `octx` (before the header is written).
    fn open(
        octx: &mut ff::format::context::Output,
        feed: &AudioFeed,
        rec_id: u64,
        global: bool,
    ) -> Result<Self, ff::Error> {
        let codec = ff::encoder::find(ff::codec::Id::AAC).ok_or(ff::Error::EncoderNotFound)?;
        let tb = ff::Rational(1, feed.rate as i32);
        let mut a = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()?;
        a.set_rate(feed.rate as i32);
        a.set_channel_layout(ff::ChannelLayout::STEREO);
        a.set_format(AUDIO_SAMPLE);
        a.set_bit_rate(160_000);
        a.set_time_base(tb);
        if global {
            a.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        }
        let enc = a.open_as(codec)?;
        let mut stream = octx.add_stream(codec)?;
        stream.set_parameters(&enc);
        stream.set_time_base(tb);
        let frame = match enc.frame_size() {
            0 => 1024,
            n => n as usize,
        };
        Ok(Self {
            stream: stream.index(),
            enc,
            tap: feed.tap.clone(),
            rec_id,
            rate: feed.rate,
            in_tb: tb,
            out_tb: tb,
            left: vec![],
            right: vec![],
            next_pts: 0,
            frame,
        })
    }

    fn drain(&mut self, octx: &mut ff::format::context::Output) -> Result<(), ff::Error> {
        let mut pkt = ff::Packet::empty();
        while self.enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(self.stream);
            pkt.rescale_ts(self.in_tb, self.out_tb);
            pkt.write_interleaved(octx)?;
        }
        Ok(())
    }

    fn send(&mut self, octx: &mut ff::format::context::Output, n: usize) -> Result<(), ff::Error> {
        let mut f = ff::frame::Audio::new(AUDIO_SAMPLE, n, ff::ChannelLayout::STEREO);
        f.set_rate(self.rate);
        f.set_pts(Some(self.next_pts));
        f.plane_mut::<f32>(0).copy_from_slice(&self.left[..n]);
        f.plane_mut::<f32>(1).copy_from_slice(&self.right[..n]);
        self.left.drain(..n);
        self.right.drain(..n);
        self.next_pts += n as i64;
        self.enc.send_frame(&f)?;
        self.drain(octx)
    }

    /// Moves what the tap has collected into the encoder. `last` also flushes the partial
    /// frame (padded with silence) and closes the stream.
    fn pump(
        &mut self,
        octx: &mut ff::format::context::Output,
        last: bool,
    ) -> Result<(), ff::Error> {
        while let Some([l, r]) = self.tap.pop_frame_rec(self.rec_id) {
            self.left.push(l);
            self.right.push(r);
        }
        while self.left.len() >= self.frame {
            self.send(octx, self.frame)?;
        }
        if last {
            if !self.left.is_empty() {
                self.left.resize(self.frame, 0.0);
                self.right.resize(self.frame, 0.0);
                self.send(octx, self.frame)?;
            }
            self.enc.send_eof()?;
            self.drain(octx)?;
        }
        Ok(())
    }
}

/// Default queue depth, in frames.
pub const QUEUE_FRAMES: usize = 6;

/// Extra queue depth while the encoder is opening or just opened (hardware encoders start
/// slowly), and how long after the start it lasts. Bounded by `WARM_BYTES`.
pub const WARM_FRAMES: usize = 30;
pub const WARM_BYTES: usize = 256 << 20;
const WARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

struct Job {
    /// Presentation index in frame periods: gaps left by dropped frames keep timing right.
    pts: i64,
    data: Vec<u8>,
    /// Counts the job as queued until the encoder is done with it (dropped).
    _pending: Pending,
}

/// Frames the encoder must finish before the larger warm-up queue ends.
const WARM_DONE: u64 = 3;

struct Pending(Arc<AtomicUsize>, Arc<AtomicU64>);

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
        self.1.fetch_add(1, Ordering::Relaxed);
    }
}

/// Bounded, never-blocking frame queue with a drop counter.
pub struct FrameQueue {
    tx: SyncSender<Job>,
    cap: usize,
    /// Extra frames accepted while warming up.
    extra: usize,
    dropped: AtomicU64,
    /// Jobs queued or being encoded.
    pending: Arc<AtomicUsize>,
    /// Jobs the encoder has finished with.
    done: Arc<AtomicU64>,
}

impl FrameQueue {
    fn new(cap: usize) -> (Self, Receiver<Job>) {
        Self::with_extra(cap, 0)
    }

    fn with_extra(cap: usize, extra: usize) -> (Self, Receiver<Job>) {
        let (tx, rx) = sync_channel(cap.max(1) + extra);
        (
            Self {
                tx,
                cap: cap.max(1),
                extra,
                dropped: AtomicU64::new(0),
                pending: Arc::new(AtomicUsize::new(0)),
                done: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    /// Queues the frame; returns false (and counts a drop) when the queue is full or closed.
    #[cfg(test)]
    fn push(&self, pts: i64, data: Vec<u8>) -> bool {
        self.push_warm(pts, data, false)
    }

    /// Like `push`; `warm` allows the extra warm-up depth.
    fn push_warm(&self, pts: i64, data: Vec<u8>, warm: bool) -> bool {
        // Steady state: `cap` queued plus the one the encoder holds. Warm-up adds `extra`.
        let limit = self.cap + 1 + if warm { self.extra } else { 0 };
        if self.pending.load(Ordering::Relaxed) >= limit {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.pending.fetch_add(1, Ordering::Relaxed);
        let job = Job {
            pts,
            data,
            _pending: Pending(self.pending.clone(), self.done.clone()),
        };
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Result of a finished recording.
#[derive(Debug, Clone)]
pub struct Summary {
    pub path: PathBuf,
    pub encoded: u64,
    pub dropped: u64,
    pub codec: String,
    /// True when the file carries the master audio track.
    pub audio: bool,
}

pub struct Recorder {
    queue: FrameQueue,
    /// Last presentation index handed to the queue (-1 = none). One meaning only: `push`
    /// continues from it and `push_at` rejects anything not strictly after it.
    last_pts: i64,
    width: u32,
    height: u32,
    path: PathBuf,
    codec: Arc<Mutex<String>>,
    started: std::time::Instant,
    /// The larger warm-up queue lasts at most this long from the spawn.
    warm_window: std::time::Duration,
    handle: Option<JoinHandle<Result<(u64, bool), String>>>,
}

fn even(v: u32) -> u32 {
    (v & !1).max(2)
}

struct Enc {
    octx: ff::format::context::Output,
    enc: ff::encoder::Video,
    scaler: ff::software::scaling::Context,
    in_tb: ff::Rational,
    out_tb: ff::Rational,
    w: u32,
    h: u32,
    src_w: u32,
    encoded: u64,
    audio: Option<AudioEnc>,
    pix: ff::format::Pixel,
}

impl Enc {
    fn open(
        path: &Path,
        (w, h, fps): (u32, u32, u32),
        feed: Option<(&AudioFeed, u64)>,
        st: RecSettings,
    ) -> Result<(Self, String), ff::Error> {
        ff::init()?;
        let (src_w, src_h) = (w, h);
        let (w, h) = (even(w), even(h));
        let pix = st.codec.pixel();
        let mut octx = if st.codec == RecCodec::PngSeq {
            std::fs::create_dir_all(path).map_err(|_| ff::Error::Unknown)?;
            // The image2 muxer is chosen by the .png name and writes one file per frame.
            ff::format::output(&path.join("frame_%06d.png"))?
        } else {
            ff::format::output(&path)?
        };
        let global = octx
            .format()
            .flags()
            .contains(ff::format::Flags::GLOBAL_HEADER);
        let tb = ff::Rational(1, fps as i32);
        let mut last = ff::Error::EncoderNotFound;
        for name in st.codec.encoders(true) {
            let Some(codec) = ff::encoder::find_by_name(name) else {
                continue;
            };
            let mut v = ff::codec::context::Context::new_with_codec(codec)
                .encoder()
                .video()?;
            v.set_width(w);
            v.set_height(h);
            v.set_format(pix);
            v.set_time_base(tb);
            v.set_frame_rate(Some(ff::Rational(fps as i32, 1)));
            if global {
                v.set_flags(ff::codec::Flags::GLOBAL_HEADER);
            }
            if st.codec == RecCodec::Hevc {
                // QuickTime and Safari only play HEVC in mp4 tagged hvc1.
                // SAFETY: plain field write on the encoder context we own, before open.
                unsafe {
                    (*v.as_mut_ptr()).codec_tag = u32::from_le_bytes(*b"hvc1");
                }
            }
            let (pairs, bit_rate) = encoder_options(name, st.quality, (w, h, fps));
            let mut opts = ff::Dictionary::new();
            for (k, val) in pairs {
                opts.set(k, &val);
            }
            if let Some(b) = bit_rate {
                v.set_bit_rate(b);
            }
            let enc = match v.open_as_with(codec, opts) {
                Ok(e) => e,
                Err(e) => {
                    last = e;
                    continue;
                }
            };
            let mut stream = octx.add_stream(codec)?;
            stream.set_parameters(&enc);
            stream.set_time_base(tb);
            // No AAC encoder (or it refuses the rate): record video only.
            let mut audio = feed
                .filter(|_| st.codec != RecCodec::PngSeq)
                .and_then(|(f, rec_id)| AudioEnc::open(&mut octx, f, rec_id, global).ok());
            octx.write_header()?;
            let out_tb = octx.stream(0).map_or(tb, |s| s.time_base());
            if let Some(a) = audio.as_mut() {
                a.out_tb = octx.stream(a.stream).map_or(a.in_tb, |s| s.time_base());
            }
            let scaler = ff::software::scaling::Context::get(
                ff::format::Pixel::RGBA,
                src_w,
                src_h,
                pix,
                w,
                h,
                ff::software::scaling::Flags::BILINEAR,
            )?;
            return Ok((
                Self {
                    octx,
                    enc,
                    scaler,
                    in_tb: tb,
                    out_tb,
                    w,
                    h,
                    src_w,
                    encoded: 0,
                    audio,
                    pix,
                },
                name.to_string(),
            ));
        }
        Err(last)
    }

    fn drain(&mut self) -> Result<(), ff::Error> {
        let mut pkt = ff::Packet::empty();
        while self.enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            pkt.rescale_ts(self.in_tb, self.out_tb);
            pkt.write_interleaved(&mut self.octx)?;
        }
        Ok(())
    }

    fn encode(&mut self, job: &Job) -> Result<(), ff::Error> {
        let src_h = (job.data.len() / (self.src_w as usize * 4)) as u32;
        let mut rgba = ff::frame::Video::new(ff::format::Pixel::RGBA, self.src_w, src_h);
        let stride = rgba.stride(0);
        let row = self.src_w as usize * 4;
        let dst = rgba.data_mut(0);
        for y in 0..src_h as usize {
            dst[y * stride..y * stride + row].copy_from_slice(&job.data[y * row..(y + 1) * row]);
        }
        let mut yuv = ff::frame::Video::new(self.pix, self.w, self.h);
        self.scaler.run(&rgba, &mut yuv)?;
        yuv.set_pts(Some(job.pts));
        self.enc.send_frame(&yuv)?;
        self.encoded += 1;
        self.drain()
    }

    /// Feeds the audio encoder with what the tap collected since the last call.
    fn pump_audio(&mut self, last: bool) -> Result<(), ff::Error> {
        match self.audio.as_mut() {
            Some(a) => a.pump(&mut self.octx, last),
            None => Ok(()),
        }
    }

    fn finish(&mut self) -> Result<(), ff::Error> {
        let audio = self.pump_audio(true);
        self.finish_video(audio)
    }

    /// Flushes the video and writes the trailer even when the audio flush failed (so the
    /// file stays playable), then reports the audio error if there was one.
    fn finish_video(&mut self, audio: Result<(), ff::Error>) -> Result<(), ff::Error> {
        let video = self
            .enc
            .send_eof()
            .and_then(|()| self.drain())
            .and_then(|()| self.octx.write_trailer());
        video.and(audio)
    }
}

fn run(
    rx: Receiver<Job>,
    path: &Path,
    size: (u32, u32, u32),
    codec: &Mutex<String>,
    feed: Option<(AudioFeed, u64)>,
    st: RecSettings,
) -> Result<(u64, bool), String> {
    let _stop = StopTap(feed.as_ref().map(|(f, g)| (f.tap.clone(), *g)));
    let (mut e, name) = Enc::open(path, size, feed.as_ref().map(|(f, g)| (f, *g)), st)
        .map_err(|e| e.to_string())?;
    *codec.lock().unwrap() = name;
    let audio = e.audio.is_some();
    loop {
        // With audio the loop also wakes up on its own to drain the tap.
        let job = if audio {
            match rx.recv_timeout(std::time::Duration::from_millis(5)) {
                Ok(j) => Some(j),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(j) => Some(j),
                Err(_) => break,
            }
        };
        if let Some(job) = job {
            e.encode(&job).map_err(|e| e.to_string())?;
        }
        e.pump_audio(false).map_err(|e| e.to_string())?;
    }
    e.finish().map_err(|e| e.to_string())?;
    let encoded = e.encoded;
    drop(e);
    if st.codec == RecCodec::PngSeq {
        // ffmpeg opens the literal pattern name once; the frames are the numbered files.
        let _ = std::fs::remove_file(path.join("frame_%06d.png"));
    }
    Ok((encoded, audio))
}

impl Recorder {
    /// Starts recording `width x height` RGBA frames at `fps` into `path` (.mp4).
    /// Odd sizes are rescaled to the next lower even size (yuv420p needs even dimensions).
    pub fn start(path: &Path, width: u32, height: u32, fps: u32) -> Result<Self, String> {
        Self::start_with_queue(path, width, height, fps, QUEUE_FRAMES)
    }

    /// Like `start`, with the master audio of `feed` as an AAC track. The tap starts
    /// collecting now, so audio and video begin together.
    pub fn start_with_audio(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        feed: AudioFeed,
    ) -> Result<Self, String> {
        Self::start_inner(
            path,
            width,
            height,
            fps,
            QUEUE_FRAMES,
            Some(feed),
            RecSettings::default(),
        )
    }

    /// Like `start_with_audio` with a chosen codec and quality; `feed` is optional. The PNG
    /// sequence writes `path` as a folder and has no audio.
    pub fn start_settings(
        path: &Path,
        (width, height, fps): (u32, u32, u32),
        feed: Option<AudioFeed>,
        settings: RecSettings,
    ) -> Result<Self, String> {
        let feed = feed.filter(|_| settings.codec != RecCodec::PngSeq);
        Self::start_inner(path, width, height, fps, QUEUE_FRAMES, feed, settings)
    }

    pub fn start_with_queue(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        queue: usize,
    ) -> Result<Self, String> {
        Self::start_inner(
            path,
            width,
            height,
            fps,
            queue,
            None,
            RecSettings::default(),
        )
    }

    fn start_inner(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        queue: usize,
        feed: Option<AudioFeed>,
        settings: RecSettings,
    ) -> Result<Self, String> {
        if width < 2 || height < 2 || fps == 0 {
            return Err("invalid recording size or fps".into());
        }
        let p = path.to_path_buf();
        let feed = feed.map(|f| {
            let rec_id = f.tap.start();
            (f, rec_id)
        });
        Self::spawn(path, width, height, queue, move |rx, c| {
            run(rx, &p, (width, height, fps), c, feed, settings)
        })
    }

    /// Starts the worker thread running `body` over the frame queue.
    fn spawn<F>(path: &Path, width: u32, height: u32, queue: usize, body: F) -> Result<Self, String>
    where
        F: FnOnce(Receiver<Job>, &Mutex<String>) -> Result<(u64, bool), String> + Send + 'static,
    {
        let frame = width as usize * height as usize * 4;
        let extra = WARM_FRAMES.min(WARM_BYTES / frame.max(1));
        let (q, rx) = FrameQueue::with_extra(queue, extra);
        let codec = Arc::new(Mutex::new(String::new()));
        let c = codec.clone();
        let handle = std::thread::Builder::new()
            .name("vjc-record".into())
            .spawn(move || body(rx, &c))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            queue: q,
            last_pts: -1,
            width,
            height,
            path: path.to_path_buf(),
            codec,
            started: std::time::Instant::now(),
            warm_window: WARM_WINDOW,
            handle: Some(handle),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dropped(&self) -> u64 {
        self.queue.dropped()
    }

    /// Frames still waiting for (or in) the encoder.
    pub fn pending(&self) -> usize {
        self.queue.pending.load(Ordering::Relaxed)
    }

    /// True once the encoder thread died (open or encode error).
    pub fn failed(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| h.is_finished())
    }

    /// Offers one tightly packed RGBA frame. Never blocks: returns false when dropped.
    pub fn push(&mut self, rgba: Vec<u8>) -> bool {
        if rgba.len() != self.width as usize * self.height as usize * 4 {
            return false;
        }
        let pts = self.last_pts + 1;
        self.last_pts = pts;
        self.queue.push_warm(pts, rgba, self.warming())
    }

    /// Offers a frame at an explicit presentation index (frame periods since the start,
    /// from real elapsed time). Indices must increase: a stale one is dropped and counted.
    pub fn push_at(&mut self, pts: i64, rgba: Vec<u8>) -> bool {
        if rgba.len() != self.width as usize * self.height as usize * 4 || pts <= self.last_pts {
            self.queue.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.last_pts = pts;
        self.queue.push_warm(pts, rgba, self.warming())
    }

    /// True while the encoder is opening or has not yet finished its first frames (the first
    /// frames after the open are slow too), for at most `warm_window` from the spawn.
    fn warming(&self) -> bool {
        self.started.elapsed() < self.warm_window
            && (self.codec.lock().unwrap().is_empty()
                || self.queue.done.load(Ordering::Relaxed) < WARM_DONE)
    }

    /// Counts a frame that could not even be captured. Only adds a drop: it must not move
    /// `last_pts`, or older frames still in flight would be rejected as stale.
    pub fn skip(&mut self) {
        self.queue.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Like `finish` but waits at most `timeout` for the encoder to flush (app shutdown).
    pub fn finish_timeout(self, timeout: std::time::Duration) -> Result<Summary, String> {
        let path = self.path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("vjc-record-finish".into())
            .spawn(move || {
                let _ = tx.send(self.finish());
            })
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(timeout).map_err(|_| {
            format!(
                "recording did not finish in time; {} may be missing its end (unplayable)",
                path.display()
            )
        })?
    }

    /// Flushes the queue, finalizes the file and joins the thread.
    pub fn finish(mut self) -> Result<Summary, String> {
        let dropped = self.queue.dropped();
        let handle = self.handle.take().ok_or("already finished")?;
        let path = self.path.clone();
        let codec = self.codec.clone();
        // Close the channel so the thread writes the trailer, then join it.
        self.close_channel();
        let (encoded, audio) = handle.join().map_err(|_| "encoder thread panicked")??;
        let codec = codec.lock().unwrap().clone();
        Ok(Summary {
            path,
            encoded,
            dropped,
            codec,
            audio,
        })
    }
}

impl Recorder {
    /// Closes the frame channel (the thread then writes the trailer and exits).
    fn close_channel(&mut self) {
        let (dead, _rx) = FrameQueue::new(1);
        drop(std::mem::replace(&mut self.queue, dead));
    }
}

impl Drop for Recorder {
    /// Dropping mid-recording still finalizes the file (trailer written), but the join
    /// happens on a background thread so the caller (UI) never waits for the queue.
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            self.close_channel();
            let _ = std::thread::Builder::new()
                .name("vjc-record-join".into())
                .spawn(move || {
                    let _ = h.join();
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vjc-rec-{}-{name}", std::process::id()))
    }

    #[test]
    fn listing_only_offers_codecs_with_an_encoder() {
        let only = |names: &'static [&'static str]| available_with(move |n| names.contains(&n));
        assert_eq!(only(&[]), vec![]);
        assert_eq!(only(&["libx264"]), vec![RecCodec::H264]);
        assert_eq!(
            only(&["hevc_videotoolbox", "prores_ks"]),
            vec![RecCodec::Hevc, RecCodec::ProRes]
        );
        assert_eq!(only(&["mpeg4"]), vec![], "the mpeg4 stand-in is not H.264");
        assert_eq!(only(&["png"]), vec![RecCodec::PngSeq]);
        for c in RecCodec::ALL {
            assert_eq!(RecCodec::from_id(c.id()), Some(c), "ids roundtrip");
        }
        // Whatever this build lists really has an encoder.
        for c in available() {
            assert!(
                c.encoders(false)
                    .iter()
                    .any(|n| ff::encoder::find_by_name(n).is_some())
            );
        }
        // Stored ids that cannot be used fall back to H.264.
        let have = [RecCodec::ProRes];
        assert_eq!(
            RecSettings::from_prefs("prores", 80, &have).codec,
            RecCodec::ProRes
        );
        assert_eq!(
            RecSettings::from_prefs("hevc", 80, &have).codec,
            RecCodec::H264
        );
        assert_eq!(RecSettings::from_prefs("zzz", 0, &have).quality, 1);
    }

    #[test]
    fn quality_maps_to_encoder_options() {
        let opt = |n, q| encoder_options(n, q, (1920, 1080, 30));
        let crf = |q| {
            opt("libx264", q)
                .0
                .iter()
                .find(|(k, _)| *k == "crf")
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(crf(70), "20", "default quality keeps the old CRF");
        assert!(crf(100).parse::<u32>().unwrap() < crf(70).parse::<u32>().unwrap());
        assert!(crf(1).parse::<u32>().unwrap() > crf(70).parse::<u32>().unwrap());
        let rate = |q| opt("hevc_videotoolbox", q).1.unwrap();
        assert!(rate(90) > rate(30));
        let prof = |q| {
            opt("prores_ks", q)
                .0
                .iter()
                .find(|(k, _)| *k == "profile")
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(
            (prof(10).as_str(), prof(60).as_str(), prof(100).as_str()),
            ("0", "2", "3")
        );
        assert_eq!(opt("png", 50), (vec![], None));
    }

    /// Each codec this build lists records a file that ffmpeg reads back with that codec.
    #[test]
    fn records_in_every_available_codec_and_plays_back() {
        let avail = available();
        assert!(!avail.is_empty(), "at least one codec on this build");
        for codec in avail {
            let path = tmp(&format!(
                "codec-{}{}",
                codec.id(),
                if codec.extension().is_empty() {
                    String::new()
                } else {
                    format!(".{}", codec.extension())
                }
            ));
            let _ = std::fs::remove_dir_all(&path);
            let _ = std::fs::remove_file(&path);
            let st = RecSettings { codec, quality: 60 };
            let mut r = Recorder::start_settings(&path, (64, 48, 10), None, st).unwrap();
            for i in 0..6u8 {
                assert!(r.push(vec![i * 30; 64 * 48 * 4]));
            }
            let sum = r.finish().unwrap_or_else(|e| panic!("{}: {e}", codec.id()));
            assert_eq!(sum.encoded, 6, "{}", codec.id());
            if codec == RecCodec::PngSeq {
                let n = std::fs::read_dir(&path)
                    .unwrap()
                    .flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
                    .count();
                assert_eq!(n, 6);
                let _ = std::fs::remove_dir_all(&path);
                continue;
            }
            let ictx = ff::format::input(&path).unwrap();
            let v = ictx.streams().best(ff::media::Type::Video).unwrap();
            let name = v.parameters().id().name().to_string();
            let want = match codec {
                RecCodec::H264 => "h264",
                RecCodec::Hevc => "hevc",
                _ => "prores",
            };
            assert_eq!(name, want, "{}", codec.id());
            if codec == RecCodec::Hevc {
                // SAFETY: reads a plain field of the stream's codec parameters.
                let tag = unsafe { (*v.parameters().as_ptr()).codec_tag };
                assert_eq!(tag, u32::from_le_bytes(*b"hvc1"), "QuickTime needs hvc1");
            }
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Decodes the file: (video streams, audio streams, audio codec, audio seconds, audio RMS).
    fn probe(path: &Path) -> (usize, usize, Option<ff::codec::Id>, f64, f64) {
        let mut ictx = ff::format::input(&path).unwrap();
        let (mut v, mut a) = (0, 0);
        let mut audio_idx = None;
        for s in ictx.streams() {
            match s.parameters().medium() {
                ff::media::Type::Video => v += 1,
                ff::media::Type::Audio => {
                    a += 1;
                    audio_idx = Some(s.index());
                }
                _ => {}
            }
        }
        let Some(idx) = audio_idx else {
            return (v, a, None, 0.0, 0.0);
        };
        let params = ictx.stream(idx).unwrap().parameters();
        let id = params.id();
        let mut dec = ff::codec::context::Context::from_parameters(params)
            .unwrap()
            .decoder()
            .audio()
            .unwrap();
        let (mut n, mut sum) = (0usize, 0f64);
        let mut frame = ff::frame::Audio::empty();
        let mut eat = |dec: &mut ff::decoder::Audio| {
            while dec.receive_frame(&mut frame).is_ok() {
                let p = frame.plane::<f32>(0);
                n += p.len();
                sum += p.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
            }
        };
        for (s, pkt) in ictx.packets() {
            if s.index() == idx {
                dec.send_packet(&pkt).unwrap();
                eat(&mut dec);
            }
        }
        dec.send_eof().unwrap();
        eat(&mut dec);
        let secs = n as f64 / f64::from(dec.rate());
        (v, a, Some(id), secs, (sum / n.max(1) as f64).sqrt())
    }

    /// Records 1 s of a 440 Hz tone at -6 dB next to 10 video frames.
    fn record_tone(path: &Path, with_audio: bool) -> Summary {
        const RATE: u32 = 48_000;
        let tap = Arc::new(crate::audio::Tap::new());
        let mut r = if with_audio {
            let feed = AudioFeed {
                tap: tap.clone(),
                rate: RATE,
            };
            Recorder::start_with_audio(path, 64, 48, 10, feed).unwrap()
        } else {
            Recorder::start(path, 64, 48, 10).unwrap()
        };
        for i in 0..10 {
            assert!(r.push(vec![(i * 20) as u8; 64 * 48 * 4]));
            // 0.1 s of audio per frame, fed like the output callback would.
            let chunk: Vec<f32> = (0..RATE as usize / 10)
                .flat_map(|k| {
                    let n = i * (RATE as usize / 10) + k;
                    let v = 0.5 * (std::f32::consts::TAU * 440.0 * n as f32 / RATE as f32).sin();
                    [v, v]
                })
                .collect();
            tap.feed(&chunk);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        r.finish().unwrap()
    }

    #[test]
    fn master_audio_lands_in_the_file_as_an_aac_track() {
        let path = tmp("audio.mp4");
        let s = record_tone(&path, true);
        assert!(s.audio, "{s:?}");
        let (v, a, id, secs, rms) = probe(&path);
        assert_eq!((v, a), (1, 1));
        assert_eq!(id, Some(ff::codec::Id::AAC));
        assert!((secs - 1.0).abs() < 0.15, "{secs} s of audio");
        // A -6 dB sine has an RMS of ~0.35; silence would be ~0.
        assert!((0.25..0.45).contains(&rms), "rms {rms}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn without_a_feed_there_is_no_audio_track() {
        let path = tmp("silent.mp4");
        let s = record_tone(&path, false);
        assert!(!s.audio);
        let (v, a, id, _, _) = probe(&path);
        assert_eq!((v, a, id), (1, 0, None));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tap_stops_when_the_recording_ends() {
        let path = tmp("tapstop.mp4");
        let tap = Arc::new(crate::audio::Tap::new());
        let feed = AudioFeed {
            tap: tap.clone(),
            rate: 48_000,
        };
        let mut r = Recorder::start_with_audio(&path, 16, 16, 10, feed).unwrap();
        assert!(tap.is_on(), "collecting while recording");
        r.push(vec![0; 16 * 16 * 4]);
        r.finish().unwrap();
        assert!(!tap.is_on(), "no audio is buffered after the recording");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn old_recording_cannot_stop_a_newer_one() {
        let (p1, p2) = (tmp("gen1.mp4"), tmp("gen2.mp4"));
        let tap = Arc::new(crate::audio::Tap::new());
        let feed = AudioFeed {
            tap: tap.clone(),
            rate: 48_000,
        };
        let mut r1 = Recorder::start_with_audio(&p1, 16, 16, 10, feed.clone()).unwrap();
        r1.push(vec![0; 16 * 16 * 4]);
        // A new recording starts while the old worker is still alive.
        let mut r2 = Recorder::start_with_audio(&p2, 16, 16, 10, feed).unwrap();
        r1.finish().unwrap();
        assert!(tap.is_on(), "old worker must not stop the new recording");
        r2.push(vec![0; 16 * 16 * 4]);
        r2.finish().unwrap();
        assert!(!tap.is_on());
        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&p2);
    }

    #[test]
    fn audio_flush_error_still_writes_the_trailer() {
        let path = tmp("audfail.mp4");
        let (mut e, _) = Enc::open(&path, (16, 16, 10), None, RecSettings::default()).unwrap();
        let job = Job {
            pts: 0,
            data: vec![0; 16 * 16 * 4],
            _pending: Pending(Arc::new(AtomicUsize::new(1)), Arc::new(AtomicU64::new(0))),
        };
        e.encode(&job).unwrap();
        let r = e.finish_video(Err(ff::Error::Bug));
        assert!(r.is_err(), "audio error is reported");
        drop(e);
        let (v, _, _, _, _) = probe(&path);
        assert_eq!(v, 1, "file with trailer is readable");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pending_counts_queued_frames_until_encoded() {
        let path = tmp("pend.mp4");
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let mut r = Recorder::spawn(&path, 4, 4, 8, move |rx, _| {
            let _ = gate_rx.recv(); // hold the worker until released
            let mut n = 0;
            while rx.recv().is_ok() {
                n += 1;
            }
            Ok((n, false))
        })
        .unwrap();
        for _ in 0..3 {
            assert!(r.push(vec![0; 4 * 4 * 4]));
        }
        assert_eq!(r.pending(), 3);
        gate_tx.send(()).unwrap();
        assert_eq!(r.finish().unwrap().encoded, 3);
    }

    #[test]
    fn finish_timeout_reports_the_file_when_the_encoder_hangs() {
        let path = tmp("hang.mp4");
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let r = Recorder::spawn(&path, 4, 4, 8, move |_rx, _| {
            let _ = gate_rx.recv();
            Ok((0, false))
        })
        .unwrap();
        let e = r
            .finish_timeout(std::time::Duration::from_millis(50))
            .unwrap_err();
        assert!(e.contains("hang.mp4") && e.contains("unplayable"), "{e}");
        let _ = gate_tx.send(());
    }

    #[test]
    fn records_thirty_frames_and_reopens() {
        let path = tmp("a.mp4");
        let (w, h) = (64u32, 48u32);
        let mut r = Recorder::start_with_queue(&path, w, h, 30, 64).unwrap();
        for i in 0..30u32 {
            let mut d = vec![255u8; (w * h * 4) as usize];
            for px in d.chunks_mut(4) {
                px[0] = (i * 8) as u8;
                px[1] = 128;
                px[2] = 255 - (i * 8) as u8;
            }
            assert!(r.push(d));
        }
        let s = r.finish().unwrap();
        assert_eq!((s.encoded, s.dropped), (30, 0));
        // Count video packets (one per frame) and check the stream size.
        let mut ictx = ff::format::input(&path).unwrap();
        let st = ictx.streams().best(ff::media::Type::Video).unwrap();
        let idx = st.index();
        let dec = ff::codec::context::Context::from_parameters(st.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        assert_eq!((dec.width(), dec.height()), (w, h));
        let n = ictx.packets().filter(|(s, _)| s.index() == idx).count();
        assert_eq!(n, 30);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn probe_reads_codec_size_fps_and_length() {
        let path = tmp("probe.mp4");
        let (w, h) = (64u32, 48u32);
        let mut r = Recorder::start_with_queue(&path, w, h, 30, 64).unwrap();
        for _ in 0..30 {
            assert!(r.push(vec![200u8; (w * h * 4) as usize]));
        }
        r.finish().unwrap();
        let info = crate::probe(&path).expect("probe");
        assert_eq!((info.width, info.height), (w, h));
        assert!((info.fps - 30.0).abs() < 3.0, "{}", info.fps);
        assert!(!info.codec.is_empty());
        assert!(
            info.duration > 0.5 && info.duration < 2.0,
            "{}",
            info.duration
        );
        let _ = std::fs::remove_file(&path);
        assert_eq!(crate::probe(&path), None);
    }

    #[test]
    fn dropping_recorder_finalizes_file() {
        let path = tmp("drop.mp4");
        let (w, h) = (64u32, 48u32);
        let mut r = Recorder::start_with_queue(&path, w, h, 30, 64).unwrap();
        for i in 0..5u8 {
            assert!(r.push(vec![i * 40; (w * h * 4) as usize]));
        }
        drop(r);
        // The join runs in the background now: wait for the trailer.
        let mut ictx = None;
        for _ in 0..100 {
            if let Ok(i) = ff::format::input(&path) {
                ictx = Some(i);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut ictx = ictx.expect("mp4 must reopen after drop");
        let idx = ictx.streams().best(ff::media::Type::Video).unwrap().index();
        assert_eq!(ictx.packets().filter(|(s, _)| s.index() == idx).count(), 5);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn drop_does_not_block_on_slow_encoder() {
        let mut r = Recorder::spawn(&tmp("slow.mp4"), 2, 2, 4, |rx, _| {
            while rx.recv().is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
            Ok((0, false))
        })
        .unwrap();
        for _ in 0..4 {
            r.push(vec![0; 16]);
        }
        let t = std::time::Instant::now();
        drop(r);
        assert!(
            t.elapsed() < std::time::Duration::from_millis(300),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn push_at_keeps_real_time_gaps() {
        let mut r = Recorder::spawn(&tmp("gap.mp4"), 2, 2, 8, |rx, _| {
            let v: Vec<i64> = rx.iter().map(|j| j.pts).collect();
            assert_eq!(v, vec![0, 3, 4]);
            Ok((v.len() as u64, false))
        })
        .unwrap();
        assert!(r.push_at(0, vec![0; 16]));
        assert!(r.push_at(3, vec![0; 16]));
        assert!(!r.push_at(3, vec![0; 16])); // stale
        assert!(r.push_at(4, vec![0; 16]));
        assert_eq!(r.finish().unwrap().encoded, 3);
    }

    #[test]
    fn skip_does_not_stale_inflight_frames() {
        let mut r = Recorder::spawn(&tmp("skip.mp4"), 2, 2, 8, |rx, _| {
            let v: Vec<i64> = rx.iter().map(|j| j.pts).collect();
            assert_eq!(v, vec![0, 1, 2]);
            Ok((v.len() as u64, false))
        })
        .unwrap();
        // Three frames in flight (pts 0..=2), the fourth capture is skipped, then the
        // in-flight ones are delivered: all accepted, only the skip is a drop.
        r.skip();
        for pts in 0..3 {
            assert!(r.push_at(pts, vec![0; 16]), "pts {pts} wrongly stale");
        }
        assert!(!r.push_at(2, vec![0; 16])); // truly stale
        assert_eq!(r.dropped(), 2);
        assert_eq!(r.finish().unwrap().encoded, 3);
    }

    #[test]
    fn finish_timeout_finalizes_file() {
        let path = tmp("ft.mp4");
        let mut r = Recorder::start_with_queue(&path, 64, 48, 30, 64).unwrap();
        for i in 0..5u8 {
            assert!(r.push(vec![i * 40; 64 * 48 * 4]));
        }
        let s = r.finish_timeout(std::time::Duration::from_secs(3)).unwrap();
        assert_eq!(s.encoded, 5);
        let mut ictx = ff::format::input(&path).unwrap();
        let idx = ictx.streams().best(ff::media::Type::Video).unwrap().index();
        assert_eq!(ictx.packets().filter(|(s, _)| s.index() == idx).count(), 5);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn full_queue_drops_and_counts() {
        let (q, rx) = FrameQueue::new(2);
        assert!(q.push(0, vec![0]));
        assert!(q.push(1, vec![0]));
        assert!(!q.push(2, vec![0]));
        assert!(!q.push(3, vec![0]));
        assert_eq!(q.dropped(), 2);
        assert_eq!(rx.try_iter().count(), 2);
        assert!(q.push(4, vec![0]));
    }

    #[test]
    fn slow_encoder_start_keeps_early_frames() {
        // The encoder is still opening (videotoolbox): the first frames must wait, not drop.
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let mut r = Recorder::spawn(&tmp("warm.mp4"), 2, 2, 4, move |rx, _| {
            let _ = gate_rx.recv();
            Ok((rx.iter().count() as u64, false))
        })
        .unwrap();
        for i in 0..12 {
            assert!(
                r.push(vec![0; 16]),
                "frame {i} dropped while the encoder starts"
            );
        }
        assert_eq!(r.dropped(), 0);
        gate_tx.send(()).unwrap();
        assert_eq!(r.finish().unwrap().encoded, 12);
    }

    /// Worker that takes one frame (the in-flight one), then holds until released.
    fn holding(
        c: Option<&'static str>,
        gate_rx: std::sync::mpsc::Receiver<()>,
    ) -> impl FnOnce(Receiver<Job>, &Mutex<String>) -> Result<(u64, bool), String> + Send + 'static
    {
        move |rx, cell| {
            if let Some(c) = c {
                *cell.lock().unwrap() = c.into();
            }
            let first = rx.recv();
            let _ = gate_rx.recv();
            drop(first);
            Ok((rx.iter().count() as u64 + 1, false))
        }
    }

    fn steady_pushes(r: &mut Recorder) -> usize {
        // let the worker take its in-flight frame first
        assert!(r.push(vec![0; 16]));
        std::thread::sleep(std::time::Duration::from_millis(100));
        (0..12).filter(|_| r.push(vec![0; 16])).count() + 1
    }

    #[test]
    fn steady_state_depth_is_cap_queued_plus_one_in_flight() {
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let mut r =
            Recorder::spawn(&tmp("warm2.mp4"), 2, 2, 4, holding(Some("x"), gate_rx)).unwrap();
        r.warm_window = std::time::Duration::ZERO;
        let ok = steady_pushes(&mut r);
        assert_eq!(ok, 5, "4 queued + 1 in flight");
        gate_tx.send(()).unwrap();
        let _ = r.finish();
    }

    #[test]
    fn warm_up_lasts_past_the_codec_open_until_first_frames_are_encoded() {
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let mut r =
            Recorder::spawn(&tmp("warm4.mp4"), 2, 2, 4, holding(Some("x"), gate_rx)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!r.codec.lock().unwrap().is_empty(), "codec open");
        assert!(r.warming(), "codec open but no frame encoded yet");
        gate_tx.send(()).unwrap();
        let _ = r.finish();
    }

    #[test]
    fn warm_up_ends_after_the_window_even_if_the_codec_never_opens() {
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let mut r = Recorder::spawn(&tmp("warm3.mp4"), 2, 2, 4, holding(None, gate_rx)).unwrap();
        r.warm_window = std::time::Duration::from_millis(50);
        assert!(r.warming());
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(!r.warming(), "codec never written, window over");
        let ok = steady_pushes(&mut r);
        assert_eq!(ok, 5);
        gate_tx.send(()).unwrap();
        let _ = r.finish();
    }

    #[test]
    fn probe_does_not_block_the_caller() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let p = Probe::start(move || {
            let _ = rx.recv();
            vec![RecCodec::H264]
        });
        assert!(p.get().is_empty(), "get must not wait for the probe");
        tx.send(()).unwrap();
        for _ in 0..200 {
            if !p.get().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(p.get(), &[RecCodec::H264]);
    }
}
