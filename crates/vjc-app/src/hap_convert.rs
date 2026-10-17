//! "Convert to HAP": runs the ffmpeg CLI in the background and never fills the disk.
//!
//! Safety rules: the output size is estimated before starting and the job is refused when
//! it would eat into the reserve; free space is polled while ffmpeg runs and the job is
//! killed (and its partial file deleted) when the reserve is reached; output goes to
//! `<name>.hap.mov.part` and is renamed only on success; the source is never touched.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use vjc_model::{ClipId, ColumnIdx, DeckId, LayerId};

const GB: u64 = 1_000_000_000;
/// Free space is checked this often while ffmpeg runs.
const POLL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Hap,
    HapAlpha,
    HapQ,
}

impl Variant {
    pub const ALL: [Variant; 3] = [Variant::Hap, Variant::HapAlpha, Variant::HapQ];

    pub fn label(self) -> &'static str {
        match self {
            Variant::Hap => "HAP",
            Variant::HapAlpha => "HAP Alpha",
            Variant::HapQ => "HAP Q",
        }
    }

    fn ffmpeg_format(self) -> &'static str {
        match self {
            Variant::Hap => "hap",
            Variant::HapAlpha => "hap_alpha",
            Variant::HapQ => "hap_q",
        }
    }

    /// Typical output bytes per pixel (before snappy gains).
    pub fn bytes_per_px(self) -> f64 {
        match self {
            Variant::Hap => 0.5,
            Variant::HapAlpha | Variant::HapQ => 1.0,
        }
    }
}

/// Estimated output size: pixels x frames x bytes per pixel, plus 20% margin.
pub fn estimate_bytes(w: u32, h: u32, fps: f64, duration: f64, v: Variant) -> u64 {
    let px = f64::from(w) * f64::from(h) * fps.max(0.0) * duration.max(0.0);
    (px * v.bytes_per_px() * 1.2) as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Space {
    pub free: u64,
    pub total: u64,
}

/// Free and total bytes of the volume holding `path` (or its nearest existing parent).
pub fn free_space(path: &Path) -> Option<Space> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    // GetDiskFreeSpaceExW wants a directory: for a file use its parent.
    if cfg!(windows) && p.is_file() {
        p = p.parent()?;
    }
    volume_space(p)
}

#[cfg(unix)]
fn volume_space(p: &Path) -> Option<Space> {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a properly sized out struct.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frag = st.f_frsize as u64;
    Some(Space {
        free: (st.f_bavail as u64).saturating_mul(frag),
        total: (st.f_blocks as u64).saturating_mul(frag),
    })
}

#[cfg(windows)]
fn volume_space(p: &Path) -> Option<Space> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            avail: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and the three out pointers are valid u64s.
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
        return None;
    }
    // `avail` honours the per-user quota, like f_bavail.
    Some(Space { free: avail, total })
}

#[cfg(not(any(unix, windows)))]
fn volume_space(_: &Path) -> Option<Space> {
    None
}

/// Free space a conversion must leave: the pref in GB, else max(10 GB, 10% of the volume).
pub fn reserve_bytes(total: u64, pref_gb: Option<u32>) -> u64 {
    match pref_gb {
        Some(g) if g > 0 => u64::from(g) * GB,
        _ => (10 * GB).max(total / 10),
    }
}

/// Refuses when the estimate plus the reserve does not fit. An estimate of 0 means the
/// duration or fps is unknown: then twice the reserve must be free.
pub fn check_space(estimate: u64, space: Space, reserve: u64) -> Result<(), String> {
    if estimate == 0 && space.free < reserve.saturating_mul(2) {
        return Err(format!(
            "Cannot estimate the size of this HAP file (unknown duration or frame rate) and \
             only {} are free; {} must be free to try. Pick another folder \
             (Preferences > General) or free some space.",
            human(space.free),
            human(reserve.saturating_mul(2))
        ));
    }
    if estimate.saturating_add(reserve) > space.free {
        return Err(format!(
            "Not enough disk space: the HAP file needs about {}, {} are free and {} must stay free. \
             Pick another folder (Preferences > General) or free some space.",
            human(estimate),
            human(space.free),
            human(reserve)
        ));
    }
    Ok(())
}

pub fn human(b: u64) -> String {
    let g = b as f64 / GB as f64;
    if g >= 1.0 {
        format!("{g:.1} GB")
    } else {
        format!("{:.0} MB", b as f64 / 1e6)
    }
}

/// `<dir or source dir>/<stem>.hap.mov`, numbered when it exists (never overwrites).
pub fn output_path(src: &Path, dir: Option<&Path>) -> PathBuf {
    let stem = src
        .file_stem()
        .map_or_else(|| "clip".to_string(), |s| s.to_string_lossy().into_owned());
    let dir = dir
        .map(Path::to_path_buf)
        .or_else(|| src.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem}.hap.mov")
        } else {
            format!("{stem}.hap-{n}.mov")
        };
        let p = dir.join(name);
        if !p.exists() && !part_of(&p).exists() {
            return p;
        }
        n += 1;
    }
}

pub fn part_of(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// Tracks the `.part` files we created, so the next start can delete leftovers of a crash.
pub struct Registry(pub PathBuf);

impl Registry {
    fn list(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(&self.0)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect()
    }

    fn write(&self, v: &[PathBuf]) {
        if v.is_empty() {
            let _ = std::fs::remove_file(&self.0);
        } else {
            let text: Vec<String> = v.iter().map(|p| p.display().to_string()).collect();
            let _ = std::fs::write(&self.0, text.join("\n"));
        }
    }

    fn add(&self, p: &Path) {
        let mut v = self.list();
        v.push(p.to_path_buf());
        self.write(&v);
    }

    fn remove(&self, p: &Path) {
        let v: Vec<PathBuf> = self.list().into_iter().filter(|x| x != p).collect();
        self.write(&v);
    }

    /// Deletes every listed `.part` (only ones ending in `.part`, never anything else).
    pub fn clean_stale(&self) -> usize {
        let v = self.list();
        let mut n = 0;
        for p in &v {
            if p.extension().is_some_and(|e| e == "part") && std::fs::remove_file(p).is_ok() {
                n += 1;
            }
        }
        self.write(&[]);
        n
    }
}

pub fn ffmpeg_args(src: &Path, part: &Path, v: Variant) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = ["-hide_banner", "-nostdin", "-v", "error", "-n"]
        .map(Into::into)
        .into();
    a.extend(["-progress", "pipe:1", "-nostats"].map(Into::into));
    a.push("-i".into());
    a.push(src.into());
    a.extend(["-map", "0:v:0", "-map", "0:a?"].map(Into::into));
    a.extend(["-c:v", "hap", "-format", v.ffmpeg_format()].map(Into::into));
    a.extend(["-chunks", "4", "-c:a", "pcm_s16le", "-f", "mov"].map(Into::into));
    a.push(part.into());
    a
}

/// Whether the ffmpeg on PATH can write HAP (its encoder needs libsnappy).
pub fn ffmpeg_has_hap() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(" hap "))
        .unwrap_or(false)
}

#[derive(Debug, PartialEq)]
pub enum Update {
    Progress(f32),
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

type FreeFn = Arc<dyn Fn(&Path) -> Option<Space> + Send + Sync>;

#[allow(clippy::too_many_arguments)]
/// Runs `cmd` (ffmpeg writing `part`) and moves `part` to `out` on success. Blocks; used
/// on a worker thread. `duration` scales the progress lines of ffmpeg.
pub fn run_job(
    mut cmd: Command,
    (part, out): (&Path, &Path),
    duration: f64,
    reserve: u64,
    cancel: &AtomicBool,
    free: &FreeFn,
    reg: Option<&Registry>,
    tx: &Sender<Update>,
) {
    let fail = |msg: String| {
        let _ = std::fs::remove_file(part);
        if let Some(r) = reg {
            r.remove(part);
        }
        let _ = tx.send(Update::Failed(msg));
    };
    if let Some(r) = reg {
        r.add(part);
    }
    let mut child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot start ffmpeg: {e}")),
    };
    let stdout = child.stdout.take();
    let tx2 = tx.clone();
    let reader = std::thread::spawn(move || {
        let Some(out) = stdout else { return };
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(us) = line.strip_prefix("out_time_us=")
                && let Ok(us) = us.trim().parse::<i64>()
                && duration > 0.0
            {
                let _ = tx2.send(Update::Progress(
                    (us as f64 / 1e6 / duration).clamp(0.0, 1.0) as f32,
                ));
            }
        }
    });
    let dir = part.parent().unwrap_or(Path::new("."));
    let mut last_poll = Instant::now();
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break Err(None);
        }
        match child.try_wait() {
            Ok(Some(st)) => {
                break if st.success() {
                    Ok(())
                } else {
                    Err(Some(format!("ffmpeg failed ({st})")))
                };
            }
            Ok(None) => {}
            Err(e) => break Err(Some(format!("ffmpeg: {e}"))),
        }
        if last_poll.elapsed() >= POLL {
            last_poll = Instant::now();
            if let Some(sp) = free(dir)
                && sp.free < reserve
            {
                let _ = child.kill();
                let _ = child.wait();
                break Err(Some(format!(
                    "Stopped: free space fell below the {} reserve; the partial file was deleted",
                    human(reserve)
                )));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // Not joined: a grandchild holding the pipe must not stall the cleanup.
    drop(reader);
    match result {
        Ok(()) => {
            if out.exists() {
                return fail("the output file appeared meanwhile; not overwriting it".into());
            }
            match std::fs::rename(part, out) {
                Ok(()) => {
                    if let Some(r) = reg {
                        r.remove(part);
                    }
                    let _ = tx.send(Update::Done(out.to_path_buf()));
                }
                Err(e) => fail(format!("cannot finish {}: {e}", out.display())),
            }
        }
        Err(None) => {
            let _ = std::fs::remove_file(part);
            if let Some(r) = reg {
                r.remove(part);
            }
            let _ = tx.send(Update::Cancelled);
        }
        Err(Some(m)) => fail(m),
    }
}

/// A conversion asked for, waiting for the user's confirmation or its turn.
#[derive(Clone, Debug)]
pub struct Request {
    pub src: PathBuf,
    pub variant: Variant,
    /// Clip to offer to swap when it finishes.
    pub cell: Option<(DeckId, LayerId, ColumnIdx)>,
    /// Id of the clip in `cell` when the question was asked.
    pub clip: Option<ClipId>,
    pub out_dir: Option<PathBuf>,
    pub reserve_gb: Option<u32>,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
}

impl Request {
    pub fn estimate(&self) -> u64 {
        estimate_bytes(
            self.width,
            self.height,
            self.fps,
            self.duration,
            self.variant,
        )
    }
}

/// Whether the swap offer still applies: the cell must hold the same clip, still playing
/// the same source file. `found` is the clip in the cell now: `(id, source path)`.
pub fn swap_check(found: Option<(ClipId, Option<&Path>)>, req: &Request) -> Result<(), String> {
    match found {
        Some((id, Some(path))) if Some(id) == req.clip && path == req.src => Ok(()),
        _ => Err("The clip changed while converting; not swapped".into()),
    }
}

struct Running {
    handle: Option<std::thread::JoinHandle<()>>,
    req: Request,
    rx: Receiver<Update>,
    cancel: Arc<AtomicBool>,
    progress: f32,
}

/// Result handed to the app when a job ends.
pub enum Finished {
    Done { req: Request, out: PathBuf },
    Failed(String),
    Cancelled,
}

/// Queue of conversions; one runs at a time.
#[derive(Default)]
pub struct Converter {
    queue: VecDeque<Request>,
    current: Option<Running>,
    pub registry: Option<PathBuf>,
}

impl Converter {
    pub fn busy(&self) -> bool {
        self.current.is_some() || !self.queue.is_empty()
    }

    /// Checks the space and queues the request.
    pub fn submit(&mut self, req: Request) -> Result<(), String> {
        let space = Self::out_dir(&req)
            .as_deref()
            .and_then(free_space)
            .ok_or("Cannot read the free space of the output volume")?;
        self.submit_with(req, space)
    }

    fn out_dir(req: &Request) -> Option<PathBuf> {
        req.out_dir
            .clone()
            .or_else(|| req.src.parent().map(Path::to_path_buf))
    }

    /// `submit` with the free space of the output volume given. Jobs already queued or
    /// running into the same folder will still write their estimate: it counts against it.
    fn submit_with(&mut self, req: Request, mut space: Space) -> Result<(), String> {
        let dir = Self::out_dir(&req);
        let pending: u64 = self
            .queue
            .iter()
            .chain(self.current.iter().map(|r| &r.req))
            .filter(|r| Self::out_dir(r) == dir)
            .map(Request::estimate)
            .fold(0, u64::saturating_add);
        space.free = space.free.saturating_sub(pending);
        check_space(
            req.estimate(),
            space,
            reserve_bytes(space.total, req.reserve_gb),
        )?;
        self.queue.push_back(req);
        Ok(())
    }

    /// App quit: stops the running ffmpeg, deletes its .part file and drops the queue.
    /// Waits a bounded time for the worker to finish the cleanup.
    pub fn shutdown(&mut self) {
        self.cancel_current();
        if let Some(mut r) = self.current.take()
            && let Some(h) = r.handle.take()
        {
            let t = Instant::now();
            while !h.is_finished() && t.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }

    pub fn cancel_current(&mut self) {
        if let Some(r) = &self.current {
            r.cancel.store(true, Ordering::Relaxed);
        }
        self.queue.clear();
    }

    /// `(source name, progress 0..1, queued behind it)` of the running job.
    pub fn status(&self) -> Option<(String, f32, usize)> {
        self.current.as_ref().map(|r| {
            (
                r.req
                    .src
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                r.progress,
                self.queue.len(),
            )
        })
    }

    /// Drives the queue; call every frame. Returns a job that just ended.
    pub fn poll(&mut self) -> Option<Finished> {
        if let Some(r) = &mut self.current {
            let mut end = None;
            loop {
                match r.rx.try_recv() {
                    Ok(Update::Progress(p)) => r.progress = p,
                    Ok(Update::Done(out)) => end = Some(Ok(out)),
                    Ok(Update::Failed(m)) => end = Some(Err(Some(m))),
                    Ok(Update::Cancelled) => end = Some(Err(None)),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        end.get_or_insert(Err(Some("the conversion stopped unexpectedly".into())));
                        break;
                    }
                }
            }
            if let Some(e) = end {
                let r = self.current.take()?;
                return Some(match e {
                    Ok(out) => Finished::Done { req: r.req, out },
                    Err(Some(msg)) => Finished::Failed(msg),
                    Err(None) => Finished::Cancelled,
                });
            }
            return None;
        }
        let req = self.queue.pop_front()?;
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (c2, r2) = (cancel.clone(), req.clone());
        let reg = self.registry.clone();
        let handle = std::thread::spawn(move || {
            if !ffmpeg_has_hap() {
                let _ = tx.send(Update::Failed(
                    "This ffmpeg cannot write HAP (it needs the snappy library: brew reinstall ffmpeg with snappy)".into(),
                ));
                return;
            }
            let out = output_path(&r2.src, r2.out_dir.as_deref());
            let part = part_of(&out);
            if let Some(d) = out.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let total = free_space(&part).map_or(0, |s| s.total);
            let mut cmd = Command::new("ffmpeg");
            cmd.args(ffmpeg_args(&r2.src, &part, r2.variant));
            let free: FreeFn = Arc::new(free_space);
            run_job(
                cmd,
                (&part, &out),
                r2.duration,
                reserve_bytes(total, r2.reserve_gb),
                &c2,
                &free,
                reg.map(Registry).as_ref(),
                &tx,
            );
        });
        self.current = Some(Running {
            handle: Some(handle),
            req,
            rx,
            cancel,
            progress: 0.0,
        });
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-hapconv-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn estimate_for_known_clip_is_in_range() {
        // 10 s of 1080p30: 622 M px. HAP 0.5 B/px, +20%: about 373 MB.
        let hap = estimate_bytes(1920, 1080, 30.0, 10.0, Variant::Hap);
        assert!((350_000_000..400_000_000).contains(&hap), "{hap}");
        let q = estimate_bytes(1920, 1080, 30.0, 10.0, Variant::HapQ);
        assert!((q as f64 / hap as f64 - 2.0).abs() < 0.01);
        // A real 4:00 4K60 clip needs a lot: the check must see it coming.
        let big = estimate_bytes(3840, 2160, 60.0, 240.0, Variant::HapAlpha);
        assert!(big > 100 * GB);
    }

    #[test]
    fn reserve_defaults_to_the_larger_of_10gb_and_10_percent() {
        assert_eq!(reserve_bytes(500 * GB, None), 50 * GB);
        assert_eq!(reserve_bytes(50 * GB, None), 10 * GB);
        assert_eq!(reserve_bytes(500 * GB, Some(3)), 3 * GB);
    }

    #[test]
    fn refuses_when_free_space_cannot_hold_estimate_and_reserve() {
        let space = Space {
            free: 20 * GB,
            total: 100 * GB,
        };
        assert!(check_space(5 * GB, space, 10 * GB).is_ok());
        let e = check_space(11 * GB, space, 10 * GB).unwrap_err();
        assert!(e.contains("Not enough disk space"), "{e}");
    }

    #[test]
    fn submit_refuses_with_an_injected_small_volume() {
        // Real volume, but a reserve far above any disk: the same rule as a full disk.
        let d = tmp("refuse");
        let mut c = Converter::default();
        let req = Request {
            src: d.join("a.mp4"),
            variant: Variant::Hap,
            cell: None,
            clip: None,
            out_dir: None,
            reserve_gb: Some(100_000),
            width: 1920,
            height: 1080,
            fps: 30.0,
            duration: 10.0,
        };
        assert!(c.submit(req.clone()).is_err());
        assert!(!c.busy());
        // Control: a zero-sized job with a small reserve is accepted.
        let ok = Request {
            reserve_gb: Some(1),
            duration: 0.0,
            ..req
        };
        assert!(c.submit(ok).is_ok());
    }

    fn req(dur: f64) -> Request {
        Request {
            src: PathBuf::from("/v/a.mp4"),
            variant: Variant::Hap,
            cell: None,
            clip: None,
            out_dir: Some(PathBuf::from("/out")),
            reserve_gb: Some(1),
            width: 1000,
            height: 1000,
            fps: 1.0,
            duration: dur,
        }
    }

    #[test]
    fn submit_counts_queued_and_running_estimates() {
        // 100 s of 1000x1000x1 fps HAP = 60 MB x 1.0 ... sized against a 10 GB volume.
        let space = Space {
            free: 10 * GB,
            total: 100 * GB,
        };
        let one = req(1.0).estimate();
        // Each job takes a bit over a third of the room above the 1 GB reserve.
        let secs = (3.5 * GB as f64 / one as f64).ceil();
        let mut c = Converter::default();
        assert!(c.submit_with(req(secs), space).is_ok());
        assert!(c.submit_with(req(secs), space).is_ok());
        let e = c.submit_with(req(secs), space).unwrap_err();
        assert!(e.contains("Not enough disk space"), "{e}");
        assert_eq!(c.queue.len(), 2);
        // Another folder is another budget.
        let mut other = req(secs);
        other.out_dir = Some(PathBuf::from("/elsewhere"));
        assert!(c.submit_with(other, space).is_ok());
    }

    #[test]
    fn unknown_estimate_needs_twice_the_reserve() {
        let space = |free| Space {
            free,
            total: 100 * GB,
        };
        let e = check_space(0, space(15 * GB), 10 * GB).unwrap_err();
        assert!(e.contains("Cannot estimate"), "{e}");
        assert!(check_space(0, space(20 * GB), 10 * GB).is_ok());
        // Control: a known estimate keeps the ordinary rule.
        assert!(check_space(1, space(15 * GB), 10 * GB).is_ok());
    }

    #[test]
    fn swap_only_when_the_cell_still_holds_the_same_clip_and_source() {
        let id = ClipId::from_u128(1);
        let mut r = req(1.0);
        r.clip = Some(id);
        let src = r.src.clone();
        assert!(swap_check(Some((id, Some(&src))), &r).is_ok());
        // Another clip moved into the cell.
        let e = swap_check(Some((ClipId::from_u128(2), Some(&src))), &r).unwrap_err();
        assert!(e.contains("not swapped"), "{e}");
        // Same clip, source relinked elsewhere.
        assert!(swap_check(Some((id, Some(Path::new("/v/b.mp4")))), &r).is_err());
        // Source no longer a file, or the cell emptied.
        assert!(swap_check(Some((id, None)), &r).is_err());
        assert!(swap_check(None, &r).is_err());
    }

    #[test]
    fn shutdown_kills_the_child_and_deletes_the_part() {
        let d = tmp("quit");
        let out = d.join("q.hap.mov");
        let part = part_of(&out);
        let pidf = d.join("pid");
        let cmd = fake(&format!(
            "echo data > '{}'; echo $$ > '{}'; exec sleep 60",
            part.display(),
            pidf.display()
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (c2, p2, o2) = (cancel.clone(), part.clone(), out.clone());
        let free: FreeFn = Arc::new(|_| None);
        let handle = std::thread::spawn(move || {
            run_job(cmd, (&p2, &o2), 1.0, 0, &c2, &free, None, &tx);
        });
        let t = Instant::now();
        while !pidf.exists() || std::fs::read_to_string(&pidf).unwrap().trim().is_empty() {
            assert!(t.elapsed().as_secs() < 5);
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid = std::fs::read_to_string(&pidf).unwrap().trim().to_string();
        let mut c = Converter::default();
        c.queue.push_back(req(1.0));
        c.current = Some(Running {
            handle: Some(handle),
            req: req(1.0),
            rx,
            cancel,
            progress: 0.0,
        });
        c.shutdown();
        assert!(!c.busy());
        assert!(!part.exists(), "the .part file must be deleted");
        let alive = Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "ffmpeg child must be dead");
    }

    #[test]
    fn real_free_space_is_readable() {
        let s = free_space(&std::env::temp_dir()).unwrap();
        assert!(s.total > 0 && s.free <= s.total);
    }

    #[test]
    fn output_never_overwrites() {
        let d = tmp("names");
        let src = d.join("clip.mp4");
        let a = output_path(&src, None);
        assert_eq!(a, d.join("clip.hap.mov"));
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(output_path(&src, None), d.join("clip.hap-2.mov"));
        let other = tmp("other");
        assert_eq!(output_path(&src, Some(&other)), other.join("clip.hap.mov"));
    }

    fn fake(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn cancel_removes_the_part_file() {
        let d = tmp("cancel");
        let out = d.join("c.hap.mov");
        let part = part_of(&out);
        let cmd = fake(&format!("echo data > '{}'; sleep 30", part.display()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let free: FreeFn = Arc::new(|_| None);
        let (c2, p2, o2) = (cancel.clone(), part.clone(), out.clone());
        let h = std::thread::spawn(move || {
            run_job(cmd, (&p2, &o2), 1.0, 0, &c2, &free, None, &tx);
        });
        let t = Instant::now();
        while !part.exists() {
            assert!(t.elapsed().as_secs() < 5);
            std::thread::sleep(Duration::from_millis(20));
        }
        cancel.store(true, Ordering::Relaxed);
        h.join().unwrap();
        assert_eq!(rx.try_iter().last(), Some(Update::Cancelled));
        assert!(!part.exists(), "partial file must be deleted");
        assert!(!out.exists());
    }

    #[test]
    fn success_renames_and_low_space_kills_and_deletes() {
        let d = tmp("run");
        let (out, part) = (d.join("ok.hap.mov"), part_of(&d.join("ok.hap.mov")));
        let (tx, rx) = channel();
        let free: FreeFn = Arc::new(|_| None);
        let no = AtomicBool::new(false);
        run_job(
            fake(&format!("echo data > '{}'", part.display())),
            (&part, &out),
            1.0,
            0,
            &no,
            &free,
            None,
            &tx,
        );
        assert_eq!(rx.try_iter().last(), Some(Update::Done(out.clone())));
        assert!(out.exists() && !part.exists());
        // Low space: the poll sees less than the reserve and stops the job.
        let (out2, part2) = (d.join("low.hap.mov"), part_of(&d.join("low.hap.mov")));
        let low: FreeFn = Arc::new(|_| {
            Some(Space {
                free: GB,
                total: 100 * GB,
            })
        });
        let t = Instant::now();
        run_job(
            fake(&format!("echo data > '{}'; sleep 30", part2.display())),
            (&part2, &out2),
            1.0,
            10 * GB,
            &no,
            &low,
            None,
            &tx,
        );
        assert!(t.elapsed() < Duration::from_secs(10), "job was not killed");
        assert!(matches!(rx.try_iter().last(), Some(Update::Failed(_))));
        assert!(!part2.exists() && !out2.exists());
    }

    #[test]
    fn stale_parts_are_cleaned_on_start() {
        let d = tmp("stale");
        let part = d.join("x.hap.mov.part");
        let keep = d.join("keep.mov");
        std::fs::write(&part, b"x").unwrap();
        std::fs::write(&keep, b"x").unwrap();
        let reg = Registry(d.join("parts.txt"));
        reg.add(&part);
        reg.add(&keep);
        assert_eq!(reg.clean_stale(), 1);
        assert!(!part.exists());
        assert!(keep.exists(), "only .part files are deleted");
    }

    #[test]
    fn free_space_reads_the_volume_of_an_existing_or_missing_path() {
        let d = tmp("space");
        let a = free_space(&d).expect("temp volume");
        assert!(a.total > 0 && a.free <= a.total);
        let b = free_space(&d.join("not").join("yet")).expect("walks up to the parent");
        assert_eq!(a.total, b.total);
    }

    #[cfg(windows)]
    #[test]
    fn free_space_of_a_file_uses_its_parent() {
        let d = tmp("space_file");
        let f = d.join("a.bin");
        std::fs::write(&f, b"x").unwrap();
        let a = free_space(&f).expect("file volume");
        assert_eq!(a.total, free_space(&d).unwrap().total);
    }
}
