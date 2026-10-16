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
