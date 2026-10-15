//! Output recording on the app side: asynchronous GPU readback of the composition
//! texture feeding `vjc_media::record::Recorder`. Nothing here blocks the frame: the
//! copy is queued, the buffer map completes on a later poll, and a bounded number of
//! readbacks are in flight (extra frames are counted as dropped).

use eframe::egui_wgpu::wgpu;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};
use vjc_media::record::{Recorder, Summary};

pub(crate) const MAX_INFLIGHT: usize = 3;

pub(crate) const PENDING: u8 = 0;
pub(crate) const OK: u8 = 1;
const ERR: u8 = 2;

/// Shared result of one buffer map: pending, ok or error.
pub(crate) type MapState = Arc<AtomicU8>;

/// Called from the `map_async` callback: every outcome leaves `PENDING`.
pub(crate) fn settle(state: &AtomicU8, ok: bool) {
    state.store(if ok { OK } else { ERR }, Ordering::Release);
}

/// In-order queue of readbacks in flight. GPU-free so it can be tested.
pub(crate) struct Inflight<B> {
    q: VecDeque<(B, MapState)>,
}

impl<B> Inflight<B> {
    pub(crate) fn new() -> Self {
        Self { q: VecDeque::new() }
    }

    pub(crate) fn len(&self) -> usize {
        self.q.len()
    }

    pub(crate) fn push(&mut self, buf: B, state: MapState) {
        self.q.push_back((buf, state));
    }

    /// Pops every settled entry at the front, in order, with whether its map succeeded.
    /// Failed maps are removed too, so they never block the frames behind them.
    pub(crate) fn take_settled(&mut self) -> Vec<(B, bool)> {
        let mut out = Vec::new();
        while self
            .q
            .front()
            .is_some_and(|(_, s)| s.load(Ordering::Acquire) != PENDING)
        {
            let (b, s) = self.q.pop_front().unwrap();
            out.push((b, s.load(Ordering::Acquire) == OK));
        }
        out
    }
}

pub struct Capture {
    rec: Recorder,
    inflight: Inflight<(wgpu::Buffer, i64)>,
    fps: f64,
    t0: Instant,
    /// Last presentation index handed out (-1 = none): pts follow real elapsed time, so a
    /// render slower than the recording fps leaves gaps instead of speeding the video up.
    last_pts: i64,
    /// Kept from `poll` so `shutdown` can settle the readbacks still in flight.
    device: Option<wgpu::Device>,
    /// Last file size read for the status text, and when (see `SizePoll`).
    size_poll: std::cell::Cell<Option<(Instant, u64)>>,
}

/// Longest the status text reuses a file size before asking the disk again.
const SIZE_POLL: Duration = Duration::from_millis(500);

/// File size through the cache `slot`: `probe` runs at most once per `SIZE_POLL`.
fn polled_size(
    slot: &std::cell::Cell<Option<(Instant, u64)>>,
    now: Instant,
    probe: impl FnOnce() -> u64,
) -> u64 {
    match slot.get() {
        Some((at, bytes)) if now.saturating_duration_since(at) < SIZE_POLL => bytes,
        _ => {
            let bytes = probe();
            slot.set(Some((now, bytes)));
            bytes
        }
    }
}

/// How long the app waits at exit for the encoder: a base, plus time for each frame still
/// queued (a long queue of big frames takes more than a fixed 3 s), capped.
pub(crate) fn exit_timeout(pending: usize) -> Duration {
    (Duration::from_secs(3) + Duration::from_millis(150) * pending as u32)
        .min(Duration::from_secs(30))
}

/// Presentation index for a frame taken `elapsed` after the start, or None when that slot
/// was already used (the render is faster than `fps`).
pub(crate) fn next_pts(elapsed: Duration, fps: f64, last: i64) -> Option<i64> {
    let pts = (elapsed.as_secs_f64() * fps).round() as i64;
    (pts > last).then_some(pts)
}

/// Row stride of the readback buffer (wgpu needs a multiple of 256).
pub(crate) fn padded_row(w: u32) -> u32 {
    (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

/// Queues a copy of `tex` (Rgba8Unorm, `size`) into a fresh MAP_READ buffer and starts its
/// asynchronous map. The returned state settles when the map completes (see `settle`).
pub(crate) fn start_readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    (w, h): (u32, u32),
    label: &str,
) -> (wgpu::Buffer, MapState) {
    let buf = readback_buffer(device, (w, h), label);
    let state = start_readback_into(device, queue, tex, (w, h), &buf);
    (buf, state)
}

/// A MAP_READ buffer sized for a `size` readback (padded rows).
pub(crate) fn readback_buffer(
    device: &wgpu::Device,
    (w, h): (u32, u32),
    label: &str,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: u64::from(padded_row(w)) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

/// Like `start_readback` but into an existing (unmapped) buffer from `readback_buffer`,
/// so callers can reuse buffers instead of allocating per frame.
pub(crate) fn start_readback_into(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    (w, h): (u32, u32),
    buf: &wgpu::Buffer,
) -> MapState {
    let stride = padded_row(w);
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(enc.finish()));
    map_read(buf)
}

/// Starts the asynchronous read map of `buf` (never waits); the state settles on completion.
pub(crate) fn map_read(buf: &wgpu::Buffer) -> MapState {
    let state: MapState = Arc::new(AtomicU8::new(PENDING));
    let flag = state.clone();
    buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        settle(&flag, r.is_ok());
    });
    state
}

/// Strips row padding from a mapped readback.
pub(crate) fn unpad(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    unpad_into(src, w, h, &mut out);
    out
}

/// `unpad` into a reusable buffer (cleared first).
pub(crate) fn unpad_into(src: &[u8], w: u32, h: u32, out: &mut Vec<u8>) {
    let (row, stride) = (w as usize * 4, padded_row(w) as usize);
    out.clear();
    out.reserve(row * h as usize);
    for y in 0..h as usize {
        out.extend_from_slice(&src[y * stride..y * stride + row]);
    }
}

impl Capture {
    pub fn start(
        path: PathBuf,
        size: (u32, u32),
        fps: f32,
        audio: Option<vjc_media::record::AudioFeed>,
    ) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let fps = fps.round().clamp(1.0, 240.0) as u32;
        let rec = match audio {
            Some(feed) => Recorder::start_with_audio(&path, size.0, size.1, fps, feed)?,
            None => Recorder::start(&path, size.0, size.1, fps)?,
        };
        Ok(Self {
            rec,
            inflight: Inflight::new(),
            fps: f64::from(fps),
            t0: Instant::now(),
            last_pts: -1,
            device: None,
            size_poll: std::cell::Cell::new(None),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.rec.size()
    }

    /// Time and file size so far, for the master bar. The size is what the muxer has
    /// written to disk, which trails the encoder by a few frames.
    pub fn info(&self) -> String {
        let bytes = polled_size(&self.size_poll, Instant::now(), || {
            std::fs::metadata(self.rec.path()).map_or(0, |m| m.len())
        });
        rec_info(self.t0.elapsed(), bytes)
    }

    pub fn failed(&self) -> bool {
        self.rec.failed()
    }

    pub fn dropped(&self) -> u64 {
        self.rec.dropped()
    }

    pub fn path(&self) -> &std::path::Path {
        self.rec.path()
    }

    /// Hands finished readbacks (in order) to the encoder. Never waits.
    pub fn poll(&mut self, device: &wgpu::Device) {
        let _ = device.poll(wgpu::PollType::Poll);
        if self.device.is_none() {
            self.device = Some(device.clone());
        }
        let (w, h) = self.rec.size();
        for ((buf, pts), ok) in self.inflight.take_settled() {
            let data = if ok {
                buf.slice(..)
                    .get_mapped_range()
                    .map(|m| unpad(&m, w, h))
                    .ok()
            } else {
                None
            };
            if ok {
                buf.unmap();
            }
            match data {
                Some(d) => {
                    self.rec.push_at(pts, d);
                }
                None => self.rec.skip(),
            }
        }
    }

    /// Queues a copy of `tex` (Rgba8Unorm, the recorder's size) when a frame is due.
    pub fn capture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) {
        let Some(pts) = next_pts(self.t0.elapsed(), self.fps, self.last_pts) else {
            return;
        };
        self.last_pts = pts;
        if self.inflight.len() >= MAX_INFLIGHT {
            self.rec.skip();
            return;
        }
        let (w, h) = self.rec.size();
        let stride = padded_row(w);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("record readback"),
            size: u64::from(stride) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        let state: MapState = Arc::new(AtomicU8::new(PENDING));
        let flag = state.clone();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            settle(&flag, r.is_ok());
        });
        self.inflight.push((buf, pts), state);
    }

    /// App shutdown: settles the readbacks in flight (bounded wait) so the last frames
    /// are kept, then finalizes the file (trailer written). The wait grows with the
    /// frames still queued; an error means the file may be unplayable.
    pub fn shutdown(mut self) -> Result<Summary, String> {
        if let Some(d) = self.device.clone() {
            let _ = d.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(2)),
            });
            self.poll(&d);
        }
        let t = exit_timeout(self.rec.pending());
        self.rec.finish_timeout(t)
    }

    /// Finalizes the file. Waits for the encoder to flush (called from a button press).
    pub fn stop(mut self, device: &wgpu::Device) -> Result<Summary, String> {
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        self.poll(device);
        self.rec.finish()
    }
}

/// `<folder>/<UTC timestamp>.mp4`, the folder being the preference or `recordings_dir`.
pub fn default_path(pref: Option<&std::path::Path>) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    unique_path(&recording_folder(pref), &timestamp(secs))
}

/// A typed recording folder as an absolute path: `~/` and relative paths resolve against
/// the home folder (the app's working directory is not meaningful); blank is `None`.
pub fn absolute_folder(text: &str) -> Option<PathBuf> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let p = PathBuf::from(t);
    if p.is_absolute() {
        return Some(p);
    }
    let home = directories::UserDirs::new()
        .map_or_else(std::env::temp_dir, |d| d.home_dir().to_path_buf());
    Some(home.join(p.strip_prefix("~").unwrap_or(&p)))
}

/// The chosen recording folder, or the default one when none is set (blank counts as none).
pub fn recording_folder(pref: Option<&std::path::Path>) -> PathBuf {
    match pref {
        Some(p) => absolute_folder(&p.to_string_lossy()).unwrap_or_else(recordings_dir),
        _ => recordings_dir(),
    }
}

/// "75 s" -> "01:15"; an hour or more -> "1:02:03".
pub(crate) fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Byte count in decimal units: "512 B", "1.5 kB", "12.3 MB", "1.20 GB".
pub(crate) fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes < 1000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{:.1} kB", b / 1e3)
    } else if bytes < 1_000_000_000 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.2} GB", b / 1e9)
    }
}

/// Status text while recording: "01:15  12.3 MB".
pub(crate) fn rec_info(elapsed: Duration, bytes: u64) -> String {
    format!("{}  {}", format_elapsed(elapsed), format_size(bytes))
}

/// Folder the app records into (`~/Movies/VJCraft`); the browser's Record tab lists it.
pub fn recordings_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|d| d.video_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(std::env::temp_dir)
        .join("VJCraft")
}

/// `<dir>/<stem>.mp4`, or `<stem>-2.mp4`, `-3`... when the name is taken (1 s resolution).
pub(crate) fn unique_path(dir: &std::path::Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.mp4"));
    if !first.exists() {
        return first;
    }
    (2u32..)
        .map(|n| dir.join(format!("{stem}-{n}.mp4")))
        .find(|p| !p.exists())
        .unwrap()
}

/// `YYYYMMDD-HHMMSS` (UTC) from unix seconds.
fn timestamp(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_poll_hits_the_disk_at_most_every_500_ms() {
        let slot = std::cell::Cell::new(None);
        let calls = std::cell::Cell::new(0u64);
        let probe = || {
            calls.set(calls.get() + 1);
            calls.get() * 10
        };
        let t0 = Instant::now();
        assert_eq!(polled_size(&slot, t0, probe), 10);
        assert_eq!(
            polled_size(&slot, t0 + Duration::from_millis(400), probe),
            10
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(
            polled_size(&slot, t0 + Duration::from_millis(500), probe),
            20
        );
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn relative_record_folders_become_absolute_under_home() {
        assert_eq!(absolute_folder("  "), None);
        let abs = absolute_folder("/tmp/rec").unwrap();
        assert_eq!(abs, PathBuf::from("/tmp/rec"));
        let rel = absolute_folder("clips/out").unwrap();
        assert!(rel.is_absolute());
        assert!(rel.ends_with("clips/out"));
        let tilde = absolute_folder("~/Movies/x").unwrap();
        assert!(tilde.is_absolute() && !tilde.to_string_lossy().contains('~'));
    }

    #[test]
    fn failed_map_does_not_block_later_frames() {
        let mut q: Inflight<u32> = Inflight::new();
        let states: Vec<MapState> = (0..MAX_INFLIGHT)
            .map(|i| {
                let s: MapState = Arc::new(AtomicU8::new(PENDING));
                q.push(i as u32, s.clone());
                s
            })
            .collect();
        settle(&states[0], false); // map error on the oldest
        settle(&states[1], true);
        assert_eq!(q.take_settled(), vec![(0, false), (1, true)]);
        assert_eq!(q.len(), 1);
        // Later frames keep flowing once slots are free.
        let s = Arc::new(AtomicU8::new(PENDING));
        q.push(10, s.clone());
        settle(&states[2], true);
        settle(&s, true);
        assert_eq!(q.take_settled(), vec![(2, true), (10, true)]);
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn exit_timeout_grows_with_the_queue_and_is_capped() {
        assert_eq!(exit_timeout(0), Duration::from_secs(3));
        assert!(exit_timeout(6) > Duration::from_secs(3));
        assert_eq!(exit_timeout(10_000), Duration::from_secs(30));
    }

    #[test]
    fn shutdown_leaves_a_playable_file() {
        let path = std::env::temp_dir().join(format!("vjc-sd-{}.mp4", std::process::id()));
        let mut cap = Capture::start(path.clone(), (64, 48), 30.0, None).unwrap();
        for pts in 0..5 {
            assert!(cap.rec.push_at(pts, vec![90; 64 * 48 * 4]));
        }
        let s = cap.shutdown().unwrap();
        assert_eq!(s.encoded, 5);
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output();
        let _ = std::fs::remove_file(&path);
        match ok {
            Ok(o) => assert!(o.status.success() && o.stderr.is_empty(), "{o:?}"),
            Err(_) => eprintln!("ffmpeg CLI missing: reopen check skipped"),
        }
    }

    #[test]
    fn pts_follows_elapsed_time_with_gaps() {
        let ms = Duration::from_millis;
        assert_eq!(next_pts(ms(0), 30.0, -1), Some(0));
        assert_eq!(next_pts(ms(5), 30.0, 0), None); // same slot
        // A 100 ms stall at 30 fps: pts jumps by 3, not 1.
        assert_eq!(next_pts(ms(100), 30.0, 0), Some(3));
    }

    #[test]
    fn unique_path_adds_suffix() {
        let d = std::env::temp_dir().join(format!("vjc-up-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(unique_path(&d, "a"), d.join("a.mp4"));
        std::fs::write(d.join("a.mp4"), b"").unwrap();
        assert_eq!(unique_path(&d, "a"), d.join("a-2.mp4"));
        std::fs::write(d.join("a-2.mp4"), b"").unwrap();
        assert_eq!(unique_path(&d, "a"), d.join("a-3.mp4"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn timestamp_formats_utc() {
        assert_eq!(timestamp(0), "19700101-000000");
        assert_eq!(timestamp(1_700_000_000), "20231114-221320");
    }

    #[test]
    fn unpad_strips_row_padding() {
        let (w, h) = (3u32, 2u32);
        let stride = padded_row(w) as usize;
        let mut src = vec![0u8; stride * 2];
        src[..12].fill(1);
        src[stride..stride + 12].fill(2);
        let out = unpad(&src, w, h);
        assert_eq!(out.len(), 24);
        assert_eq!((out[0], out[12]), (1, 2));
    }

    #[test]
    fn recording_folder_prefers_the_pref_and_ignores_blank() {
        let d = std::path::Path::new("/somewhere/else");
        assert_eq!(recording_folder(Some(d)), d);
        assert_eq!(recording_folder(None), recordings_dir());
        assert_eq!(
            recording_folder(Some(std::path::Path::new("  "))),
            recordings_dir()
        );
        let dir = std::env::temp_dir().join(format!("vjc-recdir-{}", std::process::id()));
        let p = default_path(Some(&dir));
        assert_eq!(p.parent().unwrap(), dir);
        assert_eq!(p.extension().unwrap(), "mp4");
        assert_ne!(default_path(None).parent().unwrap(), dir);
    }

    #[test]
    fn elapsed_and_size_format() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "00:00");
        assert_eq!(format_elapsed(Duration::from_millis(75_900)), "01:15");
        assert_eq!(format_elapsed(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1500), "1.5 kB");
        assert_eq!(format_size(12_345_678), "12.3 MB");
        assert_eq!(format_size(1_200_000_000), "1.20 GB");
        assert_eq!(rec_info(Duration::from_secs(5), 2_000_000), "00:05  2.0 MB");
    }
}

/// Global black-out for a network frame: colour goes to zero, alpha stays opaque. Senders keep
/// transmitting (a DMX processor would hold the last frame if they stopped).
pub fn blacken(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        *px = [0, 0, 0, 255];
    }
}

#[cfg(test)]
mod blacken_tests {
    use super::blacken;

    #[test]
    fn zeroes_colour_keeps_alpha() {
        let mut v = vec![10, 20, 30, 40, 200, 100, 50, 255];
        blacken(&mut v);
        assert_eq!(v, [0, 0, 0, 255, 0, 0, 0, 255]);
    }
}
