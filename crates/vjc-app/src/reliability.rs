//! Crash reports, the supervisor and the stall watchdog. See docs/dev/reliability.md.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Newest `*.txt` in `dir` modified at or after `since`.
pub fn newest_report(dir: &Path, since: SystemTime) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "txt"))
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
    /// e.g. Ctrl-C under `cargo run`).
    pub fn is_clean(self) -> bool {
        matches!(self, ChildEnd::Code(0) | ChildEnd::Signal(1 | 2 | 15))
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
) -> Outcome {
    let mut limit = RestartLimit::new();
    let (mut launches, mut recover) = (0, false);
    loop {
        launches += 1;
        let end = launch(recover);
        if end.is_clean() {
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
    !cfg!(test)
        && no_supervisor != Some("1")
        && !args
            .iter()
            .any(|a| matches!(a.as_str(), "--child" | "--mcp" | "--panic-test"))
}

/// The supervisor: spawns this binary as `--child` (plus `--recover` after a crash) and
/// returns the process exit code.
pub fn run_supervisor(args: &[String]) -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("VJCraft: cannot find own executable; running unsupervised");
        return 1;
    };
    let started = SystemTime::now();
    let fake = args.iter().any(|a| a.starts_with("--fake-child"));
    let outcome = supervise(
        |recover| {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args(&args[1..]).arg("--child");
            if recover {
                cmd.arg("--recover");
            }
            match cmd.status() {
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
    );
    match outcome {
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
            if !fake {
                alert(&msg);
            }
            1
        }
    }
}

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
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(d.join("launches.txt"))
        {
            let _ = writeln!(f, "{}", if recovered { "recover" } else { "fresh" });
        }
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

/// Pure stall rule: `Some(stalled_ms)` when a report is due (once per stall).
pub fn stall_due(last_beat_ms: u64, now_ms: u64, reported: &mut bool) -> Option<u64> {
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

/// Writes `stall-<stamp>.txt` into `dir`. On macOS adds a thread dump from `sample`.
pub fn write_stall(dir: &Path, stalled_ms: u64, dump: bool) -> Option<PathBuf> {
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
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if let Some(ms) =
                    stall_due(BEAT_MS.load(Ordering::Relaxed), mono_ms(), &mut reported)
                {
                    let p = write_stall(&dir, ms, true);
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
        );
        assert_eq!(out, Outcome::Quit);
        assert_eq!(seen, [false, true]);
    }

    #[test]
    fn user_quit_and_term_signals_do_not_restart() {
        for end in [ChildEnd::Code(0), ChildEnd::Signal(2), ChildEnd::Signal(15)] {
            let mut n = 0;
            let out = supervise(
                |_| {
                    n += 1;
                    end
                },
                Instant::now,
                || {},
            );
            assert_eq!((out, n), (Outcome::Quit, 1));
        }
        assert!(!ChildEnd::Code(1).is_clean() && !ChildEnd::Signal(11).is_clean());
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
        assert!(!should_supervise(&a(&["x"]), Some("1")));
    }

    #[test]
    fn stall_is_reported_once_and_rearms() {
        let mut rep = false;
        assert_eq!(stall_due(0, 9_999, &mut rep), None, "no beat yet");
        assert_eq!(stall_due(1_000, 2_900, &mut rep), None);
        assert_eq!(stall_due(1_000, 3_100, &mut rep), Some(2_100));
        assert_eq!(stall_due(1_000, 4_000, &mut rep), None, "once per stall");
        assert_eq!(stall_due(4_000, 4_100, &mut rep), None);
        assert_eq!(stall_due(4_000, 6_100, &mut rep), Some(2_100), "re-armed");
    }

    #[test]
    fn stall_report_lands_in_the_crash_dir() {
        let dir = std::env::temp_dir().join(format!("vjc-stall-{}", std::process::id()));
        let p = write_stall(&dir.join("crashes"), 2500, false).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("did not tick for 2500 ms") && t.contains("VJCraft"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
