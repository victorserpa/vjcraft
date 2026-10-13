//! Output recording: RGBA frames in, `.mp4` out. Encoding runs on its own thread behind a
//! bounded queue; when the queue is full the frame is dropped and counted, so the render
//! thread never blocks. H.264 (libx264) when available, otherwise mpeg4.

use ffmpeg_next as ff;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Default queue depth, in frames.
pub const QUEUE_FRAMES: usize = 6;

struct Job {
    /// Presentation index in frame periods: gaps left by dropped frames keep timing right.
    pts: i64,
    data: Vec<u8>,
}

/// Bounded, never-blocking frame queue with a drop counter.
pub struct FrameQueue {
    tx: SyncSender<Job>,
    dropped: AtomicU64,
}

impl FrameQueue {
    fn new(cap: usize) -> (Self, Receiver<Job>) {
        let (tx, rx) = sync_channel(cap.max(1));
        (
            Self {
                tx,
                dropped: AtomicU64::new(0),
            },
            rx,
        )
    }

    /// Queues the frame; returns false (and counts a drop) when the queue is full or closed.
    fn push(&self, pts: i64, data: Vec<u8>) -> bool {
        match self.tx.try_send(Job { pts, data }) {
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
    handle: Option<JoinHandle<Result<u64, String>>>,
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
}

impl Enc {
    fn open(path: &Path, w: u32, h: u32, fps: u32) -> Result<(Self, String), ff::Error> {
        ff::init()?;
        let (src_w, src_h) = (w, h);
        let (w, h) = (even(w), even(h));
        let mut octx = ff::format::output(&path)?;
        let global = octx
            .format()
            .flags()
            .contains(ff::format::Flags::GLOBAL_HEADER);
        let tb = ff::Rational(1, fps as i32);
        let mut last = ff::Error::EncoderNotFound;
        for (name, codec) in [
            ("libx264", ff::encoder::find_by_name("libx264")),
            ("mpeg4", ff::encoder::find(ff::codec::Id::MPEG4)),
        ] {
            let Some(codec) = codec else { continue };
            let mut v = ff::codec::context::Context::new_with_codec(codec)
                .encoder()
                .video()?;
            v.set_width(w);
            v.set_height(h);
            v.set_format(ff::format::Pixel::YUV420P);
            v.set_time_base(tb);
            v.set_frame_rate(Some(ff::Rational(fps as i32, 1)));
            if global {
                v.set_flags(ff::codec::Flags::GLOBAL_HEADER);
            }
            let mut opts = ff::Dictionary::new();
            if name == "libx264" {
                opts.set("preset", "veryfast");
                opts.set("crf", "20");
            } else {
                v.set_bit_rate(8_000_000);
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
            octx.write_header()?;
            let out_tb = octx.stream(0).map_or(tb, |s| s.time_base());
            let scaler = ff::software::scaling::Context::get(
                ff::format::Pixel::RGBA,
                src_w,
                src_h,
                ff::format::Pixel::YUV420P,
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
        let mut yuv = ff::frame::Video::new(ff::format::Pixel::YUV420P, self.w, self.h);
        self.scaler.run(&rgba, &mut yuv)?;
        yuv.set_pts(Some(job.pts));
        self.enc.send_frame(&yuv)?;
        self.encoded += 1;
        self.drain()
    }

    fn finish(&mut self) -> Result<(), ff::Error> {
        self.enc.send_eof()?;
        self.drain()?;
        self.octx.write_trailer()
    }
}

fn run(
    rx: Receiver<Job>,
    path: &Path,
    w: u32,
    h: u32,
    fps: u32,
    codec: &Mutex<String>,
) -> Result<u64, String> {
    let (mut e, name) = Enc::open(path, w, h, fps).map_err(|e| e.to_string())?;
    *codec.lock().unwrap() = name;
    while let Ok(job) = rx.recv() {
        e.encode(&job).map_err(|e| e.to_string())?;
    }
    e.finish().map_err(|e| e.to_string())?;
    Ok(e.encoded)
}

impl Recorder {
    /// Starts recording `width x height` RGBA frames at `fps` into `path` (.mp4).
    /// Odd sizes are rescaled to the next lower even size (yuv420p needs even dimensions).
    pub fn start(path: &Path, width: u32, height: u32, fps: u32) -> Result<Self, String> {
        Self::start_with_queue(path, width, height, fps, QUEUE_FRAMES)
    }

    pub fn start_with_queue(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        queue: usize,
    ) -> Result<Self, String> {
        if width < 2 || height < 2 || fps == 0 {
            return Err("invalid recording size or fps".into());
        }
        let p = path.to_path_buf();
        Self::spawn(path, width, height, queue, move |rx, c| {
            run(rx, &p, width, height, fps, c)
        })
    }

    /// Starts the worker thread running `body` over the frame queue.
    fn spawn<F>(path: &Path, width: u32, height: u32, queue: usize, body: F) -> Result<Self, String>
    where
        F: FnOnce(Receiver<Job>, &Mutex<String>) -> Result<u64, String> + Send + 'static,
    {
        let (q, rx) = FrameQueue::new(queue);
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
        self.queue.push(pts, rgba)
    }

    /// Offers a frame at an explicit presentation index (frame periods since the start,
    /// from real elapsed time). Indices must increase: a stale one is dropped and counted.
    pub fn push_at(&mut self, pts: i64, rgba: Vec<u8>) -> bool {
        if rgba.len() != self.width as usize * self.height as usize * 4 || pts <= self.last_pts {
            self.queue.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.last_pts = pts;
        self.queue.push(pts, rgba)
    }

    /// Counts a frame that could not even be captured. Only adds a drop: it must not move
    /// `last_pts`, or older frames still in flight would be rejected as stale.
    pub fn skip(&mut self) {
        self.queue.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Like `finish` but waits at most `timeout` for the encoder to flush (app shutdown).
    pub fn finish_timeout(self, timeout: std::time::Duration) -> Result<Summary, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("vjc-record-finish".into())
            .spawn(move || {
                let _ = tx.send(self.finish());
            })
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(timeout)
            .map_err(|_| "recording did not finish in time".to_string())?
    }

    /// Flushes the queue, finalizes the file and joins the thread.
    pub fn finish(mut self) -> Result<Summary, String> {
        let dropped = self.queue.dropped();
        let handle = self.handle.take().ok_or("already finished")?;
        let path = self.path.clone();
        let codec = self.codec.clone();
        // Close the channel so the thread writes the trailer, then join it.
        self.close_channel();
        let encoded = handle.join().map_err(|_| "encoder thread panicked")??;
        let codec = codec.lock().unwrap().clone();
        Ok(Summary {
            path,
            encoded,
            dropped,
            codec,
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
            Ok(0)
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
            Ok(v.len() as u64)
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
            Ok(v.len() as u64)
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
}
