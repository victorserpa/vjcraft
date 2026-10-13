//! Video decoding for VJCraft: one decode thread per `Player`, RGBA frames,
//! loop at EOF, pacing from the monotonic clock using the file timestamps.
//! FFmpeg decodes H264, ProRes, MJPEG and HAP.

use ffmpeg_next as ff;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
}

pub struct Player {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    pub path: PathBuf,
}

impl Player {
    /// Starts decoding `path` in a background thread. Open errors are reported via `error()`.
    pub fn open(path: impl AsRef<Path>) -> Player {
        let path = path.as_ref().to_path_buf();
        let shared = Arc::new(Shared { latest: Mutex::new(None), stop: AtomicBool::new(false), error: Mutex::new(None) });
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
                if let Err(e) = run(&p, &s, true) {
                    *s.error.lock().unwrap() = Some(e.to_string());
                }
            })
            .ok();
        Player { shared, thread, path }
    }

    pub fn latest_frame(&self) -> Option<Frame> {
        self.shared.latest.lock().unwrap().clone()
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
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
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp"))
}

/// Decodes a still image into a single RGBA frame.
fn load_image(path: &Path) -> Result<Frame, String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let (width, height) = img.dimensions();
    Ok(Frame { width, height, data: Arc::new(img.into_raw()), seq: 1 })
}

/// Small pool of frame buffers; a buffer is reused once the consumer dropped its `Arc`.
struct Pool(Vec<Arc<Vec<u8>>>);

impl Pool {
    fn take(&mut self, len: usize) -> &mut Arc<Vec<u8>> {
        let idx = match self.0.iter().position(|a| Arc::strong_count(a) == 1 && a.len() == len) {
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
    decode_loop(path, looping, &mut |frame, due| {
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

fn decode_loop(path: &Path, looping: bool, sink: &mut dyn FnMut(Frame, Instant) -> bool) -> Result<(), ff::Error> {
    ff::init()?;
    let mut ictx = ff::format::input(&path)?;
    let (stream_idx, tb, avg_fps, params) = {
        let st = ictx.streams().best(ff::media::Type::Video).ok_or(ff::Error::StreamNotFound)?;
        let r = st.avg_frame_rate();
        let fps = if r.denominator() != 0 && r.numerator() != 0 { r.numerator() as f64 / r.denominator() as f64 } else { 30.0 };
        (st.index(), f64::from(st.time_base()), fps, st.parameters())
    };
    let mut decoder = ff::codec::context::Context::from_parameters(params)?.decoder().video()?;
    let (w, h) = (decoder.width(), decoder.height());
    let mut scaler: Option<ff::software::scaling::Context> = None;
    let mut decoded = ff::frame::Video::empty();
    let mut rgba = ff::frame::Video::empty();
    let mut pool = Pool(vec![]);
    let mut seq = 0u64;
    let mut base = Instant::now();
    let mut first_pts: Option<f64> = None;
    let mut count = 0u64;
    let mut emitted_this_pass = 0u64;

    macro_rules! drain {
        () => {
            while decoder.receive_frame(&mut decoded).is_ok() {
                if scaler.is_none() {
                    scaler = Some(ff::software::scaling::Context::get(
                        decoded.format(), decoded.width(), decoded.height(),
                        ff::format::Pixel::RGBA, decoded.width(), decoded.height(),
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
                let t = decoded.timestamp().map(|p| p as f64 * tb).unwrap_or(count as f64 / avg_fps);
                let first = *first_pts.get_or_insert(t);
                let due = base + Duration::from_secs_f64((t - first).max(0.0));
                count += 1;
                emitted_this_pass += 1;
                seq += 1;
                let f = Frame { width: fw, height: fh, data: slot.clone(), seq };
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
        count = 0;
        emitted_this_pass = 0;
    }
}

/// Decodes up to `n` frames as fast as possible (no pacing, no loop). For tests/benchmarks.
pub fn decode_n(path: &Path, n: usize) -> Result<Vec<Frame>, ff::Error> {
    let mut out = vec![];
    decode_loop(path, false, &mut |f, _| {
        out.push(Frame { data: Arc::new((*f.data).clone()), ..f });
        out.len() < n
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=1"])
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
}
