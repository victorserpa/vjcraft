//! The lane is what the audio callback talks to: it must never block, and it must keep the
//! audio going (dry) when the plugin crashes or goes over its budget.
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use vjc_clap::*;

const HELPER: &str = env!("CARGO_BIN_EXE_vjc-clap-helper");
const ID: &str = "org.vjcraft.test.gain";
const RATE: u32 = 48_000;

fn plugin() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("vjc-clap-lane-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gain_plugin.rs");
        let out = dir.join("gain.clap");
        let st = std::process::Command::new("rustc")
            .args(["--edition", "2024", "--crate-type", "cdylib", "-O", "-o"])
            .arg(&out)
            .arg(src)
            .status()
            .unwrap();
        assert!(st.success(), "fixture plugin build failed");
        out.to_string_lossy().to_string()
    })
}

fn lane(budget_ms: u64) -> (Arc<ClapShared>, ClapLane) {
    let shared = ClapShared::new(plugin(), ID, &[1.0, 0.0, 0.0]);
    let l = ClapLane::spawn(
        shared.clone(),
        Path::new(HELPER),
        Duration::from_millis(budget_ms),
        RATE,
    );
    (shared, l)
}

/// One callback: 256 stereo frames of DC 0.5. Returns the output and how long the call took.
fn block(l: &mut ClapLane) -> (Vec<f32>, Duration) {
    let mut b = vec![0.5f32; 512];
    let t = Instant::now();
    l.process(&mut b);
    (b, t.elapsed())
}

fn is_wet(b: &[f32], gain: f32) -> bool {
    b.iter().all(|v| (v - 0.5 * gain).abs() < 1e-4)
}

/// Feeds blocks at about real time until `until` is true for a block, or `secs` pass.
fn feed(l: &mut ClapLane, secs: u64, mut until: impl FnMut(&[f32]) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        let (b, took) = block(l);
        assert!(took < Duration::from_millis(5), "callback blocked {took:?}");
        if until(&b) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn lane_applies_the_plugin_gain_without_blocking() {
    let (shared, mut l) = lane(2000);
    shared.set_param(0, 0.5);
    assert!(feed(&mut l, 10, |b| is_wet(b, 0.5)), "wet audio arrives");
    assert_eq!(shared.status(), LaneStatus::Running);
    shared.set_param(0, 2.0);
    assert!(
        feed(&mut l, 10, |b| is_wet(b, 2.0)),
        "new value reaches the plugin"
    );
}

#[test]
fn crash_passes_dry_audio_and_flags_bypass() {
    let (shared, mut l) = lane(2000);
    assert!(feed(&mut l, 10, |b| is_wet(b, 1.0)) || shared.status() == LaneStatus::Running);
    shared.set_param(1, 1.0); // plugin aborts while processing
    let ok = feed(&mut l, 10, |_| {
        matches!(shared.status(), LaneStatus::Bypassed(_))
    });
    assert!(ok, "crash flagged");
    // keeps going: every later block is the dry input
    for _ in 0..20 {
        let (b, took) = block(&mut l);
        assert!(took < Duration::from_millis(5));
        assert!(is_wet(&b, 1.0), "dry audio continues");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn budget_miss_passes_dry_audio_and_flags_bypass() {
    let (shared, mut l) = lane(50);
    assert!(feed(&mut l, 10, |_| shared.status() == LaneStatus::Running));
    shared.set_param(2, 1.0); // plugin sleeps 30 s
    assert!(feed(&mut l, 10, |_| matches!(
        shared.status(),
        LaneStatus::Bypassed(_)
    )));
    let (b, _) = block(&mut l);
    assert!(is_wet(&b, 1.0));
}

#[test]
fn restart_brings_a_bypassed_plugin_back() {
    let (shared, mut l) = lane(2000);
    shared.set_param(1, 1.0);
    assert!(feed(&mut l, 10, |_| matches!(
        shared.status(),
        LaneStatus::Bypassed(_)
    )));
    shared.set_param(1, 0.0);
    shared.set_param(0, 0.5);
    shared.restart();
    assert!(
        feed(&mut l, 10, |b| is_wet(b, 0.5)),
        "wet again after restart"
    );
    assert_eq!(shared.status(), LaneStatus::Running);
}

#[test]
fn missing_plugin_is_bypassed_from_the_start() {
    let shared = ClapShared::new("/nonexistent/x.clap", ID, &[]);
    let mut l = ClapLane::spawn(
        shared.clone(),
        Path::new(HELPER),
        Duration::from_millis(100),
        RATE,
    );
    assert!(feed(&mut l, 10, |_| matches!(
        shared.status(),
        LaneStatus::Bypassed(_)
    )));
    let (b, _) = block(&mut l);
    assert!(is_wet(&b, 1.0));
}

#[test]
fn dry_wet_mix_uses_the_dry_block_the_wet_audio_came_from() {
    // gain 1: the wet block equals its own dry block, so a correct mix returns exactly an
    // earlier input value. Mixing against the current block would land between two values.
    let (shared, mut l) = lane(2000);
    assert!(feed(&mut l, 10, |_| shared.status() == LaneStatus::Running));
    let mut wet_blocks = 0;
    for k in 1..400 {
        let v = k as f32 * 0.01;
        let mut b = vec![v; 512];
        l.process_mix(&mut b, 0.25);
        let r = (b[0] / 0.01).round();
        assert!(
            (b[0] - r * 0.01).abs() < 1e-4 && b.iter().all(|x| (x - b[0]).abs() < 1e-6),
            "block {k} came out as a blend of different blocks: {}",
            b[0]
        );
        if r < k as f32 {
            wet_blocks += 1;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(wet_blocks > 5, "wet audio did arrive ({wet_blocks})");
}

#[test]
fn a_stale_report_does_not_overwrite_a_newer_restart() {
    let shared = ClapShared::new("/x.clap", ID, &[]);
    let g = shared.restart_gen();
    shared.restart(); // asked for while the worker was still loading
    shared.mark_running_at(g);
    assert_eq!(shared.status(), LaneStatus::Starting);
    shared.mark_bypassed_at(Some(g), "old failure");
    assert_eq!(shared.status(), LaneStatus::Starting);
    shared.mark_running_at(shared.restart_gen());
    assert_eq!(shared.status(), LaneStatus::Running);
}

#[test]
fn a_heavy_plugin_shows_starting_while_it_loads_and_never_blocks_the_callback() {
    let shared = ClapShared::new(&slow_load_plugin(), ID, &[1.0, 0.0, 0.0]);
    let mut l = ClapLane::spawn(
        shared.clone(),
        Path::new(HELPER),
        Duration::from_secs(2),
        RATE,
    );
    feed(&mut l, 1, |_| false);
    assert_eq!(shared.status(), LaneStatus::Starting);
    assert!(feed(&mut l, 20, |_| shared.status() == LaneStatus::Running));
}

fn slow_load_plugin() -> String {
    let dir = std::env::temp_dir().join(format!("vjc-clap-lane-slowload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gain_plugin.rs");
    let out = dir.join("slow.clap");
    let st = std::process::Command::new("rustc")
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "cdylib",
            "--cfg",
            "slowload",
            "-O",
            "-o",
        ])
        .arg(&out)
        .arg(src)
        .status()
        .unwrap();
    assert!(st.success());
    out.to_string_lossy().to_string()
}
