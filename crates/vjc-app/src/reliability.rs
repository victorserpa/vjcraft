//! Crash reports, the supervisor and the stall watchdog. See docs/dev/reliability.md.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Restarts allowed inside `RESTART_WINDOW` before the supervisor gives up.
pub const MAX_RESTARTS: usize = 3;
pub const RESTART_WINDOW: Duration = Duration::from_secs(60);
/// A main loop that does not tick for this long is reported as stalled.
pub const STALL_AFTER: Duration = Duration::from_secs(2);

pub fn crash_dir() -> Option<PathBuf> {
    Some(crate::data_dir()?.join("crashes"))
}

// ---------------------------------------------------------------- crash reports

/// `YYYYMMDD-HHMMSS` (UTC) of a unix time, without a date crate.
pub fn utc_stamp(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days as i64 + 719_468;
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Creates `<dir>/<prefix><stamp>.txt` (or `-2`, `-3`... if taken) and writes `body`.
pub fn write_unique(dir: &Path, prefix: &str, body: &str) -> Option<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(dir).ok()?;
    let stamp = utc_stamp(now_secs());
    for n in 1..100 {
        let name = if n == 1 {
            format!("{prefix}{stamp}.txt")
        } else {
            format!("{prefix}{stamp}-{n}.txt")
        };
        let path = dir.join(name);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            let _ = f.write_all(body.as_bytes());
            let _ = f.sync_all();
            return Some(path);
        }
    }
    None
}

fn header() -> String {
    format!(
        "VJCraft {} on {} {}\nunix time: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        now_secs()
    )
}

fn recovery_note() -> String {
    match crate::recovery_path().and_then(|p| std::fs::metadata(&p).ok().map(|m| (p, m))) {
        Some((p, m)) => format!(
            "recovery file: {} ({} bytes, last autosave; restored by --recover)\n",
            p.display(),
            m.len()
        ),
        None => "recovery file: none\n".into(),
    }
}

/// Text of a crash report for a panic.
pub fn panic_report(info: &std::panic::PanicHookInfo<'_>) -> String {
    let msg = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into());
    let loc = info
        .location()
        .map_or("unknown".to_string(), |l| l.to_string());
    let thread = std::thread::current();
    format!(
        "{}thread: {}\nmessage: {msg}\nlocation: {loc}\n{}\nbacktrace:\n{}\n",
        header(),
        thread.name().unwrap_or("<unnamed>"),
        recovery_note(),
        std::backtrace::Backtrace::force_capture()
    )
}

/// Installs the panic hook: the default hook output stays; then a report goes to
/// `<data_dir>/crashes/`. The hook itself never panics (it is wrapped in `catch_unwind`).
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(dir) = crash_dir() {
                let path = write_unique(&dir, "", &panic_report(info));
                if let Some(p) = path {
                    eprintln!("VJCraft crash report: {}", p.display());
                }
            }
        }));
    }));
}

/// A crash report: `<stamp>[-n].txt`, not a `stall-*` report or a `*.sample.txt` dump.
fn is_crash_report(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".txt") && !name.starts_with("stall-") && !name.ends_with(".sample.txt")
}

/// Newest crash report in `dir` modified at or after `since`.
pub fn newest_report(dir: &Path, since: SystemTime) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| is_crash_report(&e.path()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(t, _)| *t >= since)
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

// ------------------------------------------------------------------- supervisor

/// How a child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildEnd {
    Code(i32),
    Signal(i32),
    /// The child could not be started.
    SpawnFailed,
}

impl ChildEnd {
    /// True for a user quit: exit code 0, or a termination request (SIGHUP/SIGINT/SIGTERM,
    /// e.g. Ctrl-C under `cargo run`), or SIGKILL (Force Quit / `kill -9` is intentional).
    pub fn is_clean(self) -> bool {
        matches!(self, ChildEnd::Code(0) | ChildEnd::Signal(1 | 2 | 9 | 15))
    }
}

/// Sliding-window restart budget.
pub struct RestartLimit {
    times: VecDeque<Instant>,
}

impl RestartLimit {
    pub fn new() -> Self {
        Self {
            times: VecDeque::new(),
        }
    }

    /// Records a restart at `now`; false if the budget is spent.
    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .times
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > RESTART_WINDOW)
        {
            self.times.pop_front();
        }
        if self.times.len() >= MAX_RESTARTS {
            return false;
        }
        self.times.push_back(now);
        true
    }
}

/// Final state of a supervised run.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Quit,
    GaveUp { launches: usize, last: ChildEnd },
}

/// Runs `launch(recover)` until the child quits cleanly or the restart budget is spent.
pub fn supervise(
    mut launch: impl FnMut(bool) -> ChildEnd,
    mut now: impl FnMut() -> Instant,
    mut pause: impl FnMut(),
    stopping: impl Fn() -> bool,
) -> Outcome {
    let mut limit = RestartLimit::new();
    let (mut launches, mut recover) = (0, false);
    loop {
        if stopping() {
            return Outcome::Quit;
        }
        launches += 1;
        let end = launch(recover);
        // A stop request (relayed SIGTERM/SIGHUP) wins however the child ended.
        if end.is_clean() || stopping() {
            return Outcome::Quit;
        }
        if !limit.allow(now()) {
            return Outcome::GaveUp {
                launches,
                last: end,
            };
        }
        recover = true;
        pause();
        if stopping() {
            return Outcome::Quit;
        }
    }
}

#[cfg(unix)]
fn end_of(s: std::process::ExitStatus) -> ChildEnd {
    use std::os::unix::process::ExitStatusExt;
    match (s.code(), s.signal()) {
        (Some(c), _) => ChildEnd::Code(c),
        (None, Some(sig)) => ChildEnd::Signal(sig),
        _ => ChildEnd::Code(1),
    }
}

#[cfg(not(unix))]
fn end_of(s: std::process::ExitStatus) -> ChildEnd {
    ChildEnd::Code(s.code().unwrap_or(1))
}

/// Whether `main` should run the supervisor instead of the app.
pub fn should_supervise(args: &[String], no_supervisor: Option<&str>) -> bool {
    !cfg!(test) && wants_supervisor(args, no_supervisor)
}

/// The rule itself, without the `cfg!(test)` guard (so tests can exercise it).
fn wants_supervisor(args: &[String], no_supervisor: Option<&str>) -> bool {
    no_supervisor != Some("1")
        && !args
            .iter()
            .any(|a| matches!(a.as_str(), "--child" | "--mcp" | "--panic-test"))
}

/// The supervisor: spawns this binary as `--child` (plus `--recover` after a crash) and
/// returns the process exit code.
/// `None` when it cannot supervise (no own executable): the caller runs the app in-process.
pub fn run_supervisor(args: &[String]) -> Option<i32> {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("VJCraft: cannot find own executable; running unsupervised");
        return None;
    };
    #[cfg(unix)]
    install_stop_handlers();
    let started = SystemTime::now();
    let fake = args.iter().any(|a| a.starts_with("--fake-child"));
    let outcome = supervise(
        |recover| {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args(&args[1..]).arg("--child");
            if recover {
                cmd.arg("--recover");
            }
            match spawn_and_wait(&mut cmd) {
                Ok(s) => {
                    let end = end_of(s);
                    if !end.is_clean() {
                        eprintln!("VJCraft: child ended abnormally ({end:?})");
                    }
                    end
                }
                Err(e) => {
                    eprintln!("VJCraft: cannot start child: {e}");
                    ChildEnd::SpawnFailed
                }
            }
        },
        Instant::now,
        || std::thread::sleep(Duration::from_millis(if fake { 50 } else { 500 })),
        is_stopping,
    );
    Some(match outcome {
        Outcome::Quit => 0,
        Outcome::GaveUp { launches, last } => {
            let report = crash_dir()
                .and_then(|d| newest_report(&d, started))
                .map_or("(no crash report was written)".into(), |p| {
                    p.display().to_string()
                });
            let msg = format!(
                "VJCraft stopped after {launches} launches (last end: {last:?}); \
                 not restarting again. Crash report: {report}"
            );
            eprintln!("{msg}");
            if !fake && !is_stopping() {
                alert(&msg);
            }
            1
        }
    })
}

/// Pid of the running child, for the SIGTERM relay (0 = none).
#[cfg(unix)]
static CHILD_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Set by a SIGTERM/SIGHUP sent to the supervisor: the user wants out, never relaunch.
static STOPPING: AtomicBool = AtomicBool::new(false);

fn is_stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

#[cfg(unix)]
extern "C" fn relay_term(sig: libc::c_int) {
    STOPPING.store(true, Ordering::SeqCst);
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
}

/// Installed before the first child is spawned, so no signal falls in a gap.
#[cfg(unix)]
fn install_stop_handlers() {
    // SAFETY: the handler only stores an atomic and calls an async-signal-safe function.
    unsafe {
        libc::signal(libc::SIGTERM, relay_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, relay_term as *const () as libc::sighandler_t);
    }
}

/// Like `Command::status`, but a SIGTERM/SIGHUP sent to the supervisor (`kill`, logout) is
/// relayed to the child, so the app can quit cleanly instead of being orphaned.
fn spawn_and_wait(cmd: &mut std::process::Command) -> std::io::Result<std::process::ExitStatus> {
    if is_stopping() {
        return Ok(end_status_stopped());
    }
    let mut child = cmd.spawn()?;
    #[cfg(unix)]
    {
        CHILD_PID.store(child.id() as i32, Ordering::SeqCst);
        // A signal that landed before the pid was published was not relayed.
        if is_stopping() {
            // SAFETY: the pid is our live, un-waited child.
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        }
    }
    let r = child.wait();
    #[cfg(unix)]
    CHILD_PID.store(0, Ordering::SeqCst);
    r
}

#[cfg(unix)]
fn end_status_stopped() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

#[cfg(not(unix))]
fn end_status_stopped() -> std::process::ExitStatus {
    unreachable!("no stop handlers off unix")
}

/// Set when the supervisor (our parent) is gone.
static PARENT_GONE: AtomicBool = AtomicBool::new(false);
/// Set by the UI when it starts the orphan quit, so the watcher lets it finish.
pub static ORPHAN_QUITTING: AtomicBool = AtomicBool::new(false);

/// True once the supervisor died (SIGKILL, crash): the app must save and quit.
pub fn parent_gone() -> bool {
    PARENT_GONE.load(Ordering::SeqCst)
}

/// Child mode: watches the supervisor every 500 ms. When it dies the app would otherwise
/// keep playing unattended forever, so this flags `parent_gone` (the UI loop then saves
/// recovery and exits). If the loop has not started that quit within 10 s (window hidden,
/// UI stuck) the process exits; once the quit has started it gets up to 30 s to finish
/// writing, so a large project's recovery file is not cut short.
#[cfg(unix)]
pub fn start_parent_watch() {
    // SAFETY: getppid has no preconditions.
    let parent = unsafe { libc::getppid() };
    let _ = std::thread::Builder::new()
        .name("vjc-parent-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                // SAFETY: as above.
                if unsafe { libc::getppid() } != parent {
                    PARENT_GONE.store(true, Ordering::SeqCst);
                    let t = std::time::Instant::now();
                    while t.elapsed() < Duration::from_secs(10) {
                        if ORPHAN_QUITTING.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_secs(30));
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    std::process::exit(0);
                }
            }
        });
}

#[cfg(not(unix))]
pub fn start_parent_watch() {}

/// A visible message when the supervisor gives up (macOS: dialog; elsewhere stderr only).
fn alert(msg: &str) {
    #[cfg(target_os = "macos")]
    {
        let esc = msg.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = std::process::Command::new("/usr/bin/osascript")
            .args([
                "-e",
                &format!("display alert \"VJCraft crashed repeatedly\" message \"{esc}\""),
            ])
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = msg;
}

/// Test hook for the hidden `--fake-child=<mode>` flag: logs the launch to
/// `<data_dir>/launches.txt` and exits (`crash`: always 101; `crash-once`: 101 unless
/// relaunched with `--recover`). Never returns when the flag is present.
pub fn fake_child(args: &[String]) {
    use std::io::Write;
    let Some(mode) = args.iter().find_map(|a| a.strip_prefix("--fake-child=")) else {
        return;
    };
    let recovered = args.iter().any(|a| a == "--recover");
    if let Some(d) = crate::data_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("child.pid"), std::process::id().to_string());
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(d.join("launches.txt"))
        {
            let _ = writeln!(f, "{}", if recovered { "recover" } else { "fresh" });
        }
    }
    if mode == "sleep-crash-on-term" {
        extern "C" fn die(_: libc::c_int) {
            // SAFETY: _exit is async-signal-safe.
            unsafe { libc::_exit(1) };
        }
        // SAFETY: the handler only calls _exit.
        unsafe { libc::signal(libc::SIGTERM, die as *const () as libc::sighandler_t) };
        std::thread::sleep(Duration::from_secs(20));
        std::process::exit(0);
    }
    if mode == "sleep" {
        // Waits for a relayed SIGTERM (bounded, so a failing test leaks nothing for long),
        // and quits like the UI loop does when the parent watch flags the supervisor gone.
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(20) {
            if parent_gone() {
                ORPHAN_QUITTING.store(true, Ordering::SeqCst);
                std::process::exit(0);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        std::process::exit(0);
    }
    let crash = match mode {
        "crash" => true,
        "crash-once" => !recovered,
        _ => false,
    };
    std::process::exit(if crash { 101 } else { 0 });
}

// --------------------------------------------------------------------- watchdog

static BEAT_MS: AtomicU64 = AtomicU64::new(0);
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn mono_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// Called once per main-loop frame: one atomic store, no allocation.
pub fn beat() {
    BEAT_MS.store(mono_ms(), Ordering::Relaxed);
}

static OUTPUT_LIVE: AtomicBool = AtomicBool::new(false);

/// Set every frame: whether an output window is live (so the loop must keep ticking).
pub fn set_output_live(live: bool) {
    OUTPUT_LIVE.store(live, Ordering::Relaxed);
}

/// Minimum gap between `sample` thread dumps.
pub const SAMPLE_GAP: Duration = Duration::from_secs(600);

/// Pure rule: a thread dump is allowed if none ran in the last `SAMPLE_GAP`.
pub fn sample_allowed(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= SAMPLE_GAP)
}

/// Pure stall rule: `Some(stalled_ms)` when a report is due (once per stall). An idle UI
/// (reactive repaint, minimized or occluded window) is not a stall: only `expecting`
/// frames (an output window is live and must keep rendering) can stall.
pub fn stall_due(
    last_beat_ms: u64,
    now_ms: u64,
    expecting: bool,
    reported: &mut bool,
) -> Option<u64> {
    if !expecting {
        *reported = false;
        return None;
    }
    if last_beat_ms == 0 {
        return None;
    }
    let stale = now_ms.saturating_sub(last_beat_ms);
    if stale < STALL_AFTER.as_millis() as u64 {
        *reported = false;
        return None;
    }
    if *reported {
        return None;
    }
    *reported = true;
    Some(stale)
}

/// Most report files kept in the crash dir; older ones are deleted so it never grows unbounded.
pub const MAX_REPORTS: usize = 50;

/// Deletes the oldest files in `dir` until at most `keep` remain.
pub fn prune_reports(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    let extra = files.len() - keep;
    for (_, p) in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(p);
    }
}

/// Writes `stall-<stamp>.txt` into `dir`. On macOS adds a thread dump from `sample`.
pub fn write_stall(dir: &Path, stalled_ms: u64, dump: bool) -> Option<PathBuf> {
    prune_reports(dir, MAX_REPORTS.saturating_sub(2));
    let mut body = format!(
        "{}main loop did not tick for {stalled_ms} ms (possible deadlock); not killed\n{}",
        header(),
        recovery_note()
    );
    let path = write_unique(dir, "stall-", &body)?;
    #[cfg(target_os = "macos")]
    if dump {
        let dump_path = path.with_extension("sample.txt");
        let ok = std::process::Command::new("/usr/bin/sample")
            .arg(std::process::id().to_string())
            .args(["1", "-file"])
            .arg(&dump_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            body = format!("thread dump: {}\n", dump_path.display());
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
                let _ = f.write_all(body.as_bytes());
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (dump, body);
    Some(path)
}

/// Starts the watchdog thread (checks twice a second; reports, never kills).
pub fn start_watchdog() {
    let Some(dir) = crash_dir() else { return };
    let _ = std::thread::Builder::new()
        .name("vjc-watchdog".into())
        .spawn(move || {
            let mut reported = false;
            let mut last_sample = None;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let expecting = OUTPUT_LIVE.load(Ordering::Relaxed);
                if let Some(ms) = stall_due(
                    BEAT_MS.load(Ordering::Relaxed),
                    mono_ms(),
                    expecting,
                    &mut reported,
                ) {
                    let now = Instant::now();
                    let dump = sample_allowed(last_sample, now);
                    if dump {
                        last_sample = Some(now);
                    }
                    let p = write_stall(&dir, ms, dump);
                    eprintln!("VJCraft: main loop stalled {ms} ms; report: {p:?}");
                }
            }
        });
}

/// `image::open` that turns a decoder panic into an error (UI-thread callers).
pub fn image_open(path: impl AsRef<Path>) -> Result<image::DynamicImage, String> {
    let path = path.as_ref();
    std::panic::catch_unwind(|| image::open(path).map_err(|e| e.to_string()))
        .unwrap_or_else(|_| Err("image decoder crashed".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_is_utc_civil_time() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1_781_000_000), "20260609-101320");
    }

    #[test]
    fn supervisor_restarts_with_recover_then_stops_at_the_limit() {
        let t0 = Instant::now();
        let mut seen = vec![];
        let out = supervise(
            |r| {
                seen.push(r);
                ChildEnd::Code(101)
            },
            || t0,
            || {},
            || false,
        );
        // 1 first launch + 3 restarts, all after the first with recovery.
        assert_eq!(seen, [false, true, true, true]);
        assert_eq!(
            out,
            Outcome::GaveUp {
                launches: 4,
                last: ChildEnd::Code(101)
            }
        );
    }

    #[test]
    fn crash_then_clean_quit_restarts_once() {
        let mut n = 0;
        let mut seen = vec![];
        let out = supervise(
            |r| {
                seen.push(r);
                n += 1;
                if n == 1 {
                    ChildEnd::Signal(11)
                } else {
                    ChildEnd::Code(0)
                }
            },
            Instant::now,
            || {},
            || false,
        );
        assert_eq!(out, Outcome::Quit);
        assert_eq!(seen, [false, true]);
    }

    #[test]
    fn user_quit_and_term_signals_do_not_restart() {
        for end in [
            ChildEnd::Code(0),
            ChildEnd::Signal(2),
            ChildEnd::Signal(9),
            ChildEnd::Signal(15),
        ] {
            let mut n = 0;
            let out = supervise(
                |_| {
                    n += 1;
                    end
                },
                Instant::now,
                || {},
                || false,
            );
            assert_eq!((out, n), (Outcome::Quit, 1));
        }
        assert!(!ChildEnd::Code(1).is_clean() && !ChildEnd::Signal(11).is_clean());
    }

    #[test]
    fn stop_request_quits_instead_of_relaunching() {
        use std::cell::Cell;
        let stop = Cell::new(false);
        let mut n = 0;
        // The child crashes on the relayed signal: still a quit.
        let out = supervise(
            |_| {
                n += 1;
                stop.set(true);
                ChildEnd::Code(1)
            },
            Instant::now,
            || {},
            || stop.get(),
        );
        assert_eq!((out, n), (Outcome::Quit, 1));
        // A signal during the restart backoff stops before the next launch.
        let stop = Cell::new(false);
        let mut n = 0;
        let out = supervise(
            |_| {
                n += 1;
                ChildEnd::Code(101)
            },
            Instant::now,
            || stop.set(true),
            || stop.get(),
        );
        assert_eq!((out, n), (Outcome::Quit, 1));
    }

    #[test]
    fn restart_budget_slides() {
        let t0 = Instant::now();
        let mut l = RestartLimit::new();
        assert!((0..3).all(|_| l.allow(t0)));
        assert!(!l.allow(t0 + Duration::from_secs(59)));
        assert!(l.allow(t0 + Duration::from_secs(61)));
    }

    #[test]
    fn supervisor_is_skipped_for_child_mcp_and_opt_out() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // `cfg!(test)` is true here, so the rule always says no under unit tests.
        assert!(!should_supervise(&a(&["x"]), None));
        assert!(!should_supervise(&a(&["x", "--child"]), None));
        // The real rule, not masked by `cfg!(test)`.
        assert!(wants_supervisor(&a(&["x"]), None));
        assert!(!wants_supervisor(&a(&["x"]), Some("1")));
        assert!(wants_supervisor(&a(&["x"]), Some("0")));
        assert!(!wants_supervisor(&a(&["x", "--child"]), None));
        assert!(!wants_supervisor(&a(&["x", "--mcp"]), None));
    }

    #[test]
    fn stall_is_reported_once_and_rearms() {
        let mut rep = false;
        assert_eq!(stall_due(0, 9_999, true, &mut rep), None, "no beat yet");
        assert_eq!(stall_due(1_000, 2_900, true, &mut rep), None);
        assert_eq!(stall_due(1_000, 3_100, true, &mut rep), Some(2_100));
        assert_eq!(stall_due(1_000, 4_000, true, &mut rep), None, "once");
        assert_eq!(stall_due(4_000, 4_100, true, &mut rep), None);
        assert_eq!(
            stall_due(4_000, 6_100, true, &mut rep),
            Some(2_100),
            "re-armed"
        );
    }

    #[test]
    fn idle_ui_is_not_a_stall() {
        let mut rep = false;
        assert_eq!(stall_due(1_000, 60_000, false, &mut rep), None);
        assert_eq!(stall_due(1_000, 60_000, true, &mut rep), Some(59_000));
    }

    #[test]
    fn sample_runs_at_most_once_per_ten_minutes() {
        let t0 = Instant::now();
        assert!(sample_allowed(None, t0));
        assert!(!sample_allowed(Some(t0), t0 + Duration::from_secs(599)));
        assert!(sample_allowed(Some(t0), t0 + Duration::from_secs(600)));
    }

    #[test]
    fn newest_report_ignores_stall_and_sample_files() {
        let dir = std::env::temp_dir().join(format!("vjc-newest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let since = SystemTime::now() - Duration::from_secs(5);
        std::fs::write(dir.join("20260101-000000.txt"), "c").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("stall-20260101-000001.txt"), "s").unwrap();
        std::fs::write(dir.join("stall-20260101-000001.sample.txt"), "d").unwrap();
        let p = newest_report(&dir, since).unwrap();
        assert_eq!(p.file_name().unwrap(), "20260101-000000.txt");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stall_report_lands_in_the_crash_dir() {
        let dir = std::env::temp_dir().join(format!("vjc-stall-{}", std::process::id()));
        let p = write_stall(&dir.join("crashes"), 2500, false).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("did not tick for 2500 ms") && t.contains("VJCraft"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn crash_dir_is_pruned_to_the_cap() {
        let dir = std::env::temp_dir().join(format!("vjc-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..60 {
            std::fs::write(dir.join(format!("stall-{i:03}.txt")), b"x").unwrap();
        }
        prune_reports(&dir, 10);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 10);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
