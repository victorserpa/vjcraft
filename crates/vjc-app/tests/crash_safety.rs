//! Process-level tests of the panic hook and the supervisor (hidden flags of the real
//! binary). Each run gets its own VJC_DATA_DIR: the user's data is never touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn data_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vjc-crash-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vjcraft"))
        .args(args)
        .env("VJC_DATA_DIR", dir)
        .env_remove("VJC_NO_SUPERVISOR")
        .output()
        .unwrap()
}

fn launches(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("launches.txt"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn panic_hook_writes_a_crash_report() {
    let dir = data_dir("hook");
    let out = run(&dir, &["--panic-test"]);
    assert!(!out.status.success());
    // The default hook output is kept.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("panic-test: deliberate panic"), "{err}");
    let reports: Vec<_> = std::fs::read_dir(dir.join("crashes"))
        .expect("crashes dir")
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let t = std::fs::read_to_string(&reports[0]).unwrap();
    for needle in [
        "message: panic-test: deliberate panic",
        "location: ",
        "backtrace:",
        "VJCraft 0.1.0",
        std::env::consts::OS,
        "recovery file:",
    ] {
        assert!(t.contains(needle), "missing {needle:?} in:\n{t}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn supervisor_restarts_once_after_a_crash_then_ends_on_quit() {
    let dir = data_dir("once");
    let out = run(&dir, &["--fake-child=crash-once"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(launches(&dir), ["fresh", "recover"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn supervisor_stops_at_three_restarts_and_names_the_report_path() {
    let dir = data_dir("limit");
    let out = run(&dir, &["--fake-child=crash"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        launches(&dir),
        ["fresh", "recover", "recover", "recover"],
        "first launch + 3 restarts"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not restarting again"), "{err}");
    assert!(err.contains("Crash report:"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn opt_out_runs_the_app_path_directly() {
    let dir = data_dir("optout");
    // With the opt-out the fake child flag is ignored by the supervisor path: the binary
    // would start the real app, so use --panic-test, which never reaches the supervisor.
    let out = Command::new(env!("CARGO_BIN_EXE_vjcraft"))
        .arg("--panic-test")
        .env("VJC_DATA_DIR", &dir)
        .env("VJC_NO_SUPERVISOR", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(101));
    assert!(launches(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn supervisor_relays_sigterm_to_the_child() {
    let dir = data_dir("term");
    let mut sup = Command::new(env!("CARGO_BIN_EXE_vjcraft"))
        .arg("--fake-child=sleep")
        .env("VJC_DATA_DIR", &dir)
        .env_remove("VJC_NO_SUPERVISOR")
        .spawn()
        .unwrap();
    let pid_file = dir.join("child.pid");
    let mut tries = 0;
    while !pid_file.exists()
        || std::fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .is_empty()
    {
        tries += 1;
        assert!(tries < 200, "child never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let child: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    unsafe { libc::kill(sup.id() as i32, libc::SIGTERM) };
    sup.wait().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let alive = unsafe { libc::kill(child, 0) } == 0;
    if alive {
        unsafe { libc::kill(child, libc::SIGKILL) };
    }
    assert!(!alive, "child outlived a SIGTERM to the supervisor");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Waits for `child.pid` to hold a pid and returns it.
#[cfg(unix)]
fn wait_child_pid(dir: &Path) -> i32 {
    let pid_file = dir.join("child.pid");
    for _ in 0..200 {
        if let Ok(t) = std::fs::read_to_string(&pid_file)
            && let Ok(p) = t.parse()
        {
            return p;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("child never started");
}

#[cfg(unix)]
fn spawn_supervisor(dir: &Path, mode: &str) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_vjcraft"))
        .arg(format!("--fake-child={mode}"))
        .env("VJC_DATA_DIR", dir)
        .env_remove("VJC_NO_SUPERVISOR")
        .spawn()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn sigterm_to_the_supervisor_quits_even_if_the_child_crashes_on_it() {
    let dir = data_dir("term-crash");
    let mut sup = spawn_supervisor(&dir, "sleep-crash-on-term");
    let child = wait_child_pid(&dir);
    std::thread::sleep(std::time::Duration::from_millis(300));
    unsafe { libc::kill(sup.id() as i32, libc::SIGTERM) };
    // Give a (buggy) relaunch time to show up before looking.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let exited = sup.try_wait().unwrap().is_some();
    if !exited {
        let _ = sup.kill();
    }
    let n = launches(&dir).len();
    let _ = sup.wait();
    unsafe { libc::kill(child, libc::SIGKILL) };
    if n > 1 {
        // A relaunched child may exist too.
        let p: i32 = std::fs::read_to_string(dir.join("child.pid"))
            .unwrap()
            .parse()
            .unwrap();
        unsafe { libc::kill(p, libc::SIGKILL) };
    }
    assert_eq!(n, 1, "supervisor relaunched after a SIGTERM");
    assert!(exited, "supervisor did not exit");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn child_exits_when_the_supervisor_is_sigkilled() {
    let dir = data_dir("orphan");
    let mut sup = spawn_supervisor(&dir, "sleep");
    let child = wait_child_pid(&dir);
    std::thread::sleep(std::time::Duration::from_millis(800));
    unsafe { libc::kill(sup.id() as i32, libc::SIGKILL) };
    let _ = sup.wait();
    std::thread::sleep(std::time::Duration::from_millis(3000));
    // The child is reparented, so reap-less liveness: kill(0) also succeeds on a zombie,
    // but an orphan is reaped by init, so this reflects real life.
    let alive = unsafe { libc::kill(child, 0) } == 0;
    if alive {
        unsafe { libc::kill(child, libc::SIGKILL) };
    }
    assert!(
        !alive,
        "orphaned child kept running after its supervisor died"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
