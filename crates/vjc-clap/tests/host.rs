use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vjc_clap::*;

const HELPER: &str = env!("CARGO_BIN_EXE_vjc-clap-helper");
const RATE: u32 = 48_000;

/// Builds the fixture plugin once with rustc and returns the path of the .clap file.
fn plugin() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("vjc-clap-test-{}", std::process::id()));
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

/// Same plugin built with a mono-only port.
fn mono_plugin() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("vjc-clap-mono-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gain_plugin.rs");
        let out = dir.join("mono.clap");
        let st = std::process::Command::new("rustc")
            .args(["--edition", "2024", "--crate-type", "cdylib", "--cfg", "mono", "-O", "-o"])
            .arg(&out)
            .arg(src)
            .status()
            .unwrap();
        assert!(st.success(), "mono fixture build failed");
        out.to_string_lossy().to_string()
    })
}

const ID: &str = "org.vjcraft.test.gain";

fn effect(budget_ms: u64) -> ClapEffect {
    ClapEffect::new(
        plugin(),
        ID,
        Path::new(HELPER),
        Duration::from_millis(budget_ms),
        RATE,
    )
}

/// Interleaved stereo sine, amplitude 0.5, 480 frames.
fn sine() -> Vec<f32> {
    (0..480)
        .flat_map(|i| {
            let v = 0.5 * (i as f32 * 0.1).sin();
            [v, v]
        })
        .collect()
}

fn peak(b: &[f32]) -> f32 {
    b.iter().fold(0.0, |m, v| m.max(v.abs()))
}

#[test]
fn processes_a_sine_with_the_plugin_gain() {
    let mut e = effect(5000);
    assert_eq!(e.status, Status::Running);
    let dry = sine();
    let mut b = dry.clone();
    assert!(e.process(&mut b));
    for (a, d) in b.iter().zip(&dry) {
        assert!((a - d).abs() < 1e-6, "gain 1.0 is identity");
    }
    assert!(e.set_param(0, 0.5));
    let mut b = dry.clone();
    assert!(e.process(&mut b));
    assert!((peak(&b) - peak(&dry) * 0.5).abs() < 1e-5);
    assert!(e.set_param(0, 2.0));
    let mut b = dry.clone();
    assert!(e.process(&mut b));
    assert!((peak(&b) - peak(&dry) * 2.0).abs() < 1e-5);
    assert_eq!(e.get_param(0), Some(2.0));
}

#[test]
fn parameters_come_from_the_plugin() {
    let e = effect(5000);
    let p = e.params();
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].name, "Gain");
    assert_eq!((p[0].min, p[0].max, p[0].default), (0.0, 4.0, 1.0));
}

#[test]
fn over_budget_plugin_is_bypassed_and_audio_continues() {
    let mut e = effect(300);
    assert!(e.set_param(2, 1.0)); // plugin sleeps 30 s inside process
    let dry = sine();
    let mut b = dry.clone();
    let t = Instant::now();
    assert!(!e.process(&mut b));
    assert!(t.elapsed() < Duration::from_secs(3), "watchdog fired");
    assert_eq!(b, dry, "bypass leaves the audio untouched");
    assert_eq!(e.status, Status::Stopped(HostError::Timeout));
    // later blocks pass straight through without waiting
    let t = Instant::now();
    let mut b = dry.clone();
    assert!(!e.process(&mut b));
    assert!(t.elapsed() < Duration::from_millis(100));
    assert_eq!(b, dry);
    // fix the parameter and restart: plugin runs again
    assert!(e.set_param(2, 0.0));
    e.enable();
    assert_eq!(e.status, Status::Running);
    assert!(e.set_param(0, 0.5));
    let mut b = dry.clone();
    assert!(e.process(&mut b));
    assert!((peak(&b) - peak(&dry) * 0.5).abs() < 1e-5);
}

#[test]
fn crash_mid_block_passes_audio_through() {
    let mut e = effect(5000);
    assert!(e.set_param(1, 1.0)); // plugin aborts while processing
    let dry = sine();
    let mut b = dry.clone();
    assert!(!e.process(&mut b));
    assert_eq!(b, dry);
    assert!(matches!(e.status, Status::Stopped(_)));
    let mut b = dry.clone();
    assert!(!e.process(&mut b), "keeps going while stopped");
    assert_eq!(b, dry);
}

#[test]
fn bad_input_is_rejected_locally() {
    let mut e = effect(5000);
    assert!(!e.set_param(9, 1.0), "index out of range");
    assert!(!e.set_param(0, f64::NAN));
    assert!(!e.set_param(0, f64::INFINITY));
    assert!(e.set_param(0, 100.0));
    assert_eq!(e.get_param(0), Some(4.0), "clamped to the plugin range");
    let mut odd = vec![0.25f32; 7]; // not whole stereo frames
    assert!(!e.process(&mut odd));
    assert_eq!(odd, vec![0.25f32; 7]);
    assert_eq!(
        e.status,
        Status::Running,
        "bad input is not the plugin's fault"
    );
}

#[test]
fn missing_plugin_or_id_stays_stopped_and_passes_through() {
    let mut e = ClapEffect::new(
        "/no/such.clap",
        ID,
        Path::new(HELPER),
        Duration::from_secs(2),
        RATE,
    );
    assert!(matches!(e.status, Status::Stopped(_)));
    let mut b = sine();
    assert!(!e.process(&mut b));
    let e = ClapEffect::new(
        plugin(),
        "no.such.id",
        Path::new(HELPER),
        Duration::from_secs(2),
        RATE,
    );
    assert!(matches!(e.status, Status::Stopped(HostError::Plugin(_))));
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vjc-clap-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn scan_lists_the_plugin_and_caches_by_mtime() {
    let dir = tmp("scan");
    std::fs::copy(plugin(), dir.join("gain.clap")).unwrap();
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    let cache = dir.join("cache.json");
    let (found, loads) = scan(std::slice::from_ref(&dir), &cache, Path::new(HELPER));
    assert_eq!((found.len(), loads), (1, 1));
    assert_eq!(found[0].id, ID);
    assert_eq!(found[0].name, "VJC Test Gain");
    assert_eq!(found[0].params.len(), 3);
    let (again, loads) = scan(&[dir], &cache, Path::new(HELPER));
    assert_eq!((again, loads), (found, 0), "second scan reads the cache");
}

#[test]
fn scan_skips_a_file_that_is_not_a_plugin() {
    let dir = tmp("junk");
    std::fs::write(dir.join("junk.clap"), "not a library").unwrap();
    std::fs::copy(plugin(), dir.join("gain.clap")).unwrap();
    let (found, _) = scan(
        std::slice::from_ref(&dir),
        &dir.join("c.json"),
        Path::new(HELPER),
    );
    assert_eq!(found.len(), 1);
}

#[test]
fn default_scan_dirs_follow_the_platform() {
    let d: Vec<String> = default_scan_dirs()
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    if cfg!(target_os = "macos") {
        assert!(d.iter().any(|p| p.ends_with("Library/Audio/Plug-Ins/CLAP")));
        assert!(d.contains(&"/Library/Audio/Plug-Ins/CLAP".to_string()));
    } else if cfg!(windows) {
        assert!(
            d.iter()
                .any(|p| p.ends_with("/CLAP") || p.ends_with("CLAP"))
        );
    } else {
        assert!(d.iter().any(|p| p.ends_with("/.clap")));
        assert!(d.contains(&"/usr/lib/clap".to_string()));
    }
}

#[test]
fn macos_bundle_is_resolved_to_its_executable() {
    let dir = tmp("bundle");
    let b = dir.join("x.clap");
    std::fs::create_dir_all(b.join("Contents/MacOS")).unwrap();
    std::fs::write(b.join("Contents/MacOS/inner"), "").unwrap();
    assert_eq!(resolve_binary(&b), b.join("Contents/MacOS/inner"));
    let f = dir.join("plain.clap");
    std::fs::write(&f, "").unwrap();
    assert_eq!(resolve_binary(&f), f);
}

#[test]
fn plugin_without_a_stereo_port_is_rejected_with_a_clear_message() {
    let e = ClapEffect::new(
        mono_plugin(),
        ID,
        Path::new(HELPER),
        Duration::from_millis(500),
        RATE,
    );
    match &e.status {
        Status::Stopped(HostError::Plugin(m)) => {
            assert!(m.contains("stereo"), "message names the problem: {m}")
        }
        s => panic!("expected a rejection, got {s:?}"),
    }
    let dir = tmp("mono");
    std::fs::copy(mono_plugin(), dir.join("mono.clap")).unwrap();
    std::fs::copy(plugin(), dir.join("gain.clap")).unwrap();
    let (found, _) = scan(
        std::slice::from_ref(&dir),
        &dir.join("c.json"),
        Path::new(HELPER),
    );
    assert_eq!(found.len(), 2);
    let stereo: Vec<bool> = found.iter().map(|f| f.stereo).collect();
    assert_eq!(stereo, vec![true, false], "sorted by file name: gain, mono");
}

#[test]
fn scan_descends_into_subfolders() {
    let dir = tmp("deep");
    let sub = dir.join("Vendor").join("Pack");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::copy(plugin(), sub.join("gain.clap")).unwrap();
    let (found, _) = scan(
        std::slice::from_ref(&dir),
        &dir.join("c.json"),
        Path::new(HELPER),
    );
    assert_eq!(found.len(), 1);
}
