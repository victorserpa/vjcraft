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

/// The path itself or its nearest existing parent. None when that parent is only a mount
/// root (`/`, `/Volumes`, `/media`, `/mnt`): the volume the path names (an unmounted drive)
/// is unknown, and must not be mistaken for the volume holding the root.
fn nearest_existing(path: &Path) -> Option<&Path> {
    let mut p = path;
    let mut skipped = false;
    while !p.exists() {
        p = p.parent()?;
        skipped = true;
    }
    let mount_root = ["/", "/Volumes", "/media", "/mnt"].iter().any(|r| p == Path::new(r));
    (!(skipped && mount_root)).then_some(p)
}

/// Free and total bytes of the volume holding `path` (or its nearest existing parent).
pub fn free_space(path: &Path) -> Option<Space> {
    let p = nearest_existing(path)?;
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

/// Device id (`st_dev`) of the volume holding `path` (or its nearest existing parent).
pub fn volume_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let p = nearest_existing(path)?;
    std::fs::metadata(p).ok().map(|m| m.dev())
}

/// True when both folders live on the same volume (so they share one free-space budget).
/// When a volume cannot be read, falls back to comparing the paths.
pub fn same_volume(a: &Option<PathBuf>, b: &Option<PathBuf>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => match (volume_of(x), volume_of(y)) {
            (Some(vx), Some(vy)) => vx == vy,
            _ => x == y,
        },
        _ => a == b,
    }
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
    /// `(owner pid, path)`; a line without a pid (older format) has no live owner.
    fn list(&self) -> Vec<(u32, PathBuf)> {
        std::fs::read_to_string(&self.0)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| match l.split_once('\t') {
                Some((pid, p)) => (parse_pid(pid), PathBuf::from(p)),
                None => (0, PathBuf::from(l)),
            })
            .collect()
    }

    fn write(&self, v: &[(u32, PathBuf)]) {
        if v.is_empty() {
            let _ = std::fs::remove_file(&self.0);
        } else {
            let text: Vec<String> = v
                .iter()
                .map(|(pid, p)| format!("{pid}\t{}", p.display()))
                .collect();
            let _ = std::fs::write(&self.0, text.join("\n"));
        }
    }

    fn add(&self, p: &Path) {
        self.add_as(p, std::process::id());
    }

    /// Exclusive lock for a read-modify-write of the file (released on drop).
    fn lock(&self) -> Option<std::fs::File> {
        use std::os::fd::AsRawFd;
        let mut name = self.0.as_os_str().to_os_string();
        name.push(".lock");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(name)
            .ok()?;
        // SAFETY: flock on a descriptor we own; blocks until the other writer is done.
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        Some(f)
    }

    fn add_as(&self, p: &Path, pid: u32) {
        let _lock = self.lock();
        let mut v = self.list();
        v.push((pid, p.to_path_buf()));
        self.write(&v);
    }

    fn remove(&self, p: &Path) {
        let _lock = self.lock();
        let v: Vec<_> = self.list().into_iter().filter(|(_, x)| x != p).collect();
        self.write(&v);
    }

    /// Deletes the listed `.part` files of dead instances (only ones ending in `.part`,
    /// never anything else). A part owned by another running VJCraft is left alone.
    pub fn clean_stale(&self) -> usize {
        let _lock = self.lock();
        let mut n = 0;
        let mut keep = vec![];
        for (pid, p) in self.list() {
            if pid != 0 && pid != std::process::id() && pid_alive(pid) {
                keep.push((pid, p));
            } else if p.extension().is_some_and(|e| e == "part") && std::fs::remove_file(&p).is_ok()
            {
                n += 1;
            }
        }
        self.write(&keep);
        n
    }
}

/// A pid that fits `pid_t` and is positive; anything else is 0 (no live owner).
fn parse_pid(s: &str) -> u32 {
    s.trim()
        .parse::<i32>()
        .ok()
        .filter(|p| *p > 0)
        .map_or(0, |p| p as u32)
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let ok = unsafe { libc::kill(pid as i32, 0) } == 0;
    ok || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
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

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The ffmpeg CLI to run: the first candidate that is an executable file with the HAP
/// encoder. Candidates: the bundle (`Contents/Resources/ffmpeg` next to `Contents/MacOS/<exe>`),
/// `ffmpeg` beside the exe, `<repo>/target/ffmpeg/bin/ffmpeg` for a dev exe in
/// `target/{debug,release}`. Falls back to `ffmpeg` on PATH.
pub fn resolve_ffmpeg(exe: &Path) -> PathBuf {
    let Some(dir) = exe.parent() else {
        return PathBuf::from("ffmpeg");
    };
    let bundled = dir.parent().map(|c| c.join("Resources/ffmpeg"));
    let dev = dir
        .file_name()
        .is_some_and(|n| n == "debug" || n == "release")
        .then(|| dir.parent().map(|t| t.join("ffmpeg/bin/ffmpeg")))
        .flatten();
    bundled
        .into_iter()
        .chain([dir.join("ffmpeg")])
        .chain(dev)
        .find(|p| is_executable_file(p) && ffmpeg_has_hap_at(p))
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

/// `resolve_ffmpeg` for the running executable.
pub fn ffmpeg_program() -> PathBuf {
    std::env::current_exe().map_or_else(|_| PathBuf::from("ffmpeg"), |e| resolve_ffmpeg(&e))
}

/// Whether this ffmpeg can write HAP (its encoder needs libsnappy).
pub fn ffmpeg_has_hap_at(program: &Path) -> bool {
    Command::new(program)
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
        let queued: u64 = self
            .queue
            .iter()
            .filter(|r| same_volume(&Self::out_dir(r), &dir))
            .map(Request::estimate)
            .fold(0, u64::saturating_add);
        // The running job has already written part of its estimate: `space.free` shows that
        // part as used, so only the rest is still owed.
        let running = self
            .current
            .iter()
            .filter(|r| same_volume(&Self::out_dir(&r.req), &dir))
            .map(|r| (r.req.estimate() as f64 * f64::from(1.0 - r.progress.clamp(0.0, 1.0))) as u64)
            .fold(0, u64::saturating_add);
        let pending = queued.saturating_add(running);
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
            let program = ffmpeg_program();
            if !ffmpeg_has_hap_at(&program) {
                let _ = tx.send(Update::Failed(
                    "This ffmpeg cannot write HAP (it needs the snappy library). The app bundle ships one; for a source build run scripts/build-ffmpeg.sh".into(),
                ));
                return;
            }
            let out = output_path(&r2.src, r2.out_dir.as_deref());
            let part = part_of(&out);
            if let Some(d) = out.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let total = free_space(&part).map_or(0, |s| s.total);
            let mut cmd = Command::new(&program);
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
        // Another volume is another budget.
        if volume_of(Path::new("/dev")) == volume_of(Path::new("/")) {
            eprintln!("/dev shares the root volume here: other-volume case SKIPPED");
            return;
        }
        let mut other = req(secs);
        other.out_dir = Some(PathBuf::from("/dev"));
        assert!(c.submit_with(other, space).is_ok());
    }

    #[test]
    fn a_missing_drive_is_its_own_unknown_volume_not_the_root() {
        let gone = Path::new("/Volumes/vjc-no-such-drive/out");
        assert_eq!(volume_of(gone), None);
        assert!(free_space(gone).is_none());
        let a = Some(gone.to_path_buf());
        let b = Some(PathBuf::from("/Volumes/vjc-other-gone-drive/out"));
        let root = Some(PathBuf::from("/"));
        assert!(!same_volume(&a, &root), "must not share the root budget");
        assert!(!same_volume(&a, &b));
    }

    #[test]
    fn different_folders_on_one_volume_share_the_budget() {
        let space = Space {
            free: 10 * GB,
            total: 100 * GB,
        };
        let secs = (3.5 * GB as f64 / req(1.0).estimate() as f64).ceil();
        let mut c = Converter::default();
        let mut a = req(secs);
        a.out_dir = Some(std::env::temp_dir());
        let mut b = req(secs);
        b.out_dir = Some(std::env::temp_dir().join("vjc-other-folder-same-volume"));
        let mut d = req(secs);
        d.out_dir = Some(std::env::temp_dir().join("vjc-third-folder"));
        assert!(c.submit_with(a, space).is_ok());
        assert!(c.submit_with(b, space).is_ok());
        let e = c.submit_with(d, space).unwrap_err();
        assert!(e.contains("Not enough disk space"), "{e}");
    }

    #[test]
    fn running_job_counts_only_its_unwritten_remainder() {
        let one = req(1.0).estimate();
        let secs = (3.0 * GB as f64 / one as f64).ceil();
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut c = Converter {
            current: Some(Running {
                handle: None,
                req: req(secs),
                rx,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: 1.0,
            }),
            ..Converter::default()
        };
        // free space already shows the finished job's bytes as used
        let space = Space {
            free: 5 * GB,
            total: 100 * GB,
        };
        assert!(
            c.submit_with(req(secs), space).is_ok(),
            "done job owes nothing"
        );
        // control: a job that has written nothing still owes its whole estimate
        c.current.as_mut().unwrap().progress = 0.0;
        assert!(c.submit_with(req(secs), space).is_err());
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
    fn stale_clean_spares_a_part_of_a_running_instance() {
        let d = tmp("stale-live");
        let (live, dead) = (d.join("a.hap.mov.part"), d.join("b.hap.mov.part"));
        std::fs::write(&live, b"x").unwrap();
        std::fs::write(&dead, b"x").unwrap();
        let mut other = Command::new("sleep").arg("30").spawn().unwrap();
        let mut gone = Command::new("true").spawn().unwrap();
        gone.wait().unwrap();
        let reg = Registry(d.join("parts.txt"));
        reg.add_as(&live, other.id());
        reg.add_as(&dead, gone.id());
        let n = reg.clean_stale();
        let _ = other.kill();
        let _ = other.wait();
        assert_eq!(n, 1);
        assert!(live.exists(), "another instance's part survives");
        assert!(!dead.exists(), "control: a dead owner's part is deleted");
    }

    #[test]
    fn registry_pid_parse_rejects_out_of_range() {
        assert_eq!(parse_pid("123"), 123);
        for bad in ["0", "-5", "2147483648", "4294967295", "x", ""] {
            assert_eq!(parse_pid(bad), 0, "{bad}");
        }
    }

    #[test]
    fn registry_concurrent_adds_lose_nothing() {
        let d = tmp("reg-conc");
        let path = d.join("parts.txt");
        let hs: Vec<_> = (0..8)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let reg = Registry(path);
                    for i in 0..25 {
                        reg.add(Path::new(&format!("/x/{t}-{i}.part")));
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(Registry(path).list().len(), 200);
    }

    fn fake_ffmpeg(path: &Path, tag: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!("#!/bin/sh\nif [ \"$2\" = -encoders ]; then echo ' V..... hap {tag}'; fi\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn bundled_ffmpeg_beats_path() {
        let d = tmp("bundled");
        let (macos, res) = (d.join("Contents/MacOS"), d.join("Contents/Resources"));
        std::fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("vjcraft");
        // Nothing bundled: falls back to PATH.
        assert_eq!(resolve_ffmpeg(&exe), PathBuf::from("ffmpeg"));
        // A fake on PATH-like place does not matter; the bundle wins.
        fake_ffmpeg(&res.join("ffmpeg"), "bundled");
        assert_eq!(resolve_ffmpeg(&exe), res.join("ffmpeg"));
        assert!(ffmpeg_has_hap_at(&resolve_ffmpeg(&exe)));
        // Next to the exe (dev builds, portable layouts) is found too.
        let d2 = tmp("beside");
        fake_ffmpeg(&d2.join("ffmpeg"), "beside");
        assert_eq!(resolve_ffmpeg(&d2.join("vjcraft")), d2.join("ffmpeg"));
        // Control: a bundled binary that cannot write HAP is skipped (falls to PATH).
        let d3 = tmp("nohap");
        std::fs::create_dir_all(d3.join("Contents/MacOS")).unwrap();
        let p = d3.join("Contents/Resources/ffmpeg");
        fake_ffmpeg(&p, "x");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        assert_eq!(
            resolve_ffmpeg(&d3.join("Contents/MacOS/vjcraft")),
            PathBuf::from("ffmpeg")
        );
    }

    #[test]
    fn resolve_skips_non_executable_and_non_hap_then_next_candidate() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("skip");
        let (macos, res) = (d.join("Contents/MacOS"), d.join("Contents/Resources"));
        std::fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("vjcraft");
        // Bundled: not executable. Beside exe: executable but no HAP. Neither is picked.
        fake_ffmpeg(&res.join("ffmpeg"), "bundled");
        std::fs::set_permissions(res.join("ffmpeg"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        fake_ffmpeg(&macos.join("ffmpeg"), "x");
        std::fs::write(macos.join("ffmpeg"), "#!/bin/sh\n").unwrap();
        assert_eq!(resolve_ffmpeg(&exe), PathBuf::from("ffmpeg"));
        // A good later candidate wins over the bad earlier ones.
        let t = tmp("dev");
        let dbg = t.join("target/debug");
        std::fs::create_dir_all(&dbg).unwrap();
        fake_ffmpeg(&t.join("target/ffmpeg/bin/ffmpeg"), "dev");
        assert_eq!(
            resolve_ffmpeg(&dbg.join("vjcraft")),
            t.join("target/ffmpeg/bin/ffmpeg")
        );
    }
}
