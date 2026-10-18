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
