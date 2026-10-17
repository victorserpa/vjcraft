use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use vjc_ffgl::proto::*;
use vjc_ffgl::*;

const HELPER: &str = env!("CARGO_BIN_EXE_vjc-ffgl-helper");

/// Builds the fixture plugin once with rustc and returns its directory.
fn plugin_dir() -> &'static Path {
    static D: OnceLock<PathBuf> = OnceLock::new();
    D.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("vjc-ffgl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ext = if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(windows) {
            "dll"
        } else {
            "so"
        };
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gain_plugin.rs");
        let st = std::process::Command::new("rustc")
            .args([
                "--edition",
                "2024",
                "--crate-type",
                "cdylib",
                "--crate-name",
                "gain_plugin",
                "-O",
                "-o",
            ])
            .arg(dir.join(format!("gain.{ext}")))
            .arg(src)
            .status()
            .unwrap();
        assert!(st.success(), "fixture plugin build failed");
        dir
    })
}

fn plugin() -> String {
    std::fs::read_dir(plugin_dir())
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path()
        .to_string_lossy()
        .to_string()
}

fn frame(v: u8) -> Frame {
    let mut rgba = Vec::new();
    for _ in 0..16 {
        rgba.extend([v, v / 2, 10, 200]);
    }
    Frame { w: 4, h: 4, rgba }
}

fn effect(timeout_ms: u64) -> FfglEffect {
    FfglEffect::new(
        &plugin(),
        Path::new(HELPER),
        Duration::from_millis(timeout_ms),
    )
}

#[test]
fn processes_a_frame_like_the_plugin_documents() {
    let mut e = effect(5000);
    assert_eq!(e.status, Status::Running);
    e.set_param(0, 0.5); // gain 0.5 -> x2*0.5 = identity
    let out = e.process(&frame(100));
    assert_eq!(out.rgba[..4], [100, 50, 10, 200]);
    e.set_param(0, 1.0); // x2, alpha untouched, clamped
    let out = e.process(&frame(100));
    assert_eq!(out.rgba[..4], [200, 100, 20, 200]);
}

#[test]
fn crash_mid_frame_passes_input_through_and_restarts_on_enable() {
    let mut e = effect(5000);
    e.set_param(1, 1.0); // plugin aborts while processing
    let f = frame(80);
    let out = e.process(&f);
    assert_eq!(out.rgba, f.rgba, "pass-through");
    assert!(matches!(e.status, Status::Stopped(_)));
    // keeps rendering without panic while stopped
    assert_eq!(e.process(&f).rgba, f.rgba);
    // restart on enable; the crash param is restored from local state, so clear it first
    e.set_param(1, 0.0);
    e.enable();
    assert_eq!(e.status, Status::Running);
    e.set_param(0, 1.0);
    assert_eq!(e.process(&frame(100)).rgba[0], 200);
}

#[test]
fn hang_times_out_and_passes_through() {
    let mut e = effect(400);
    e.set_param(2, 1.0);
    let f = frame(80);
    let t = std::time::Instant::now();
    let out = e.process(&f);
    assert!(t.elapsed() < Duration::from_secs(3));
    assert_eq!(out.rgba, f.rgba);
    assert_eq!(e.status, Status::Stopped(HostError::Timeout));
}

#[test]
fn parameter_mapping_roundtrip() {
    let mut e = effect(5000);
    for v in [0.0, 0.25, 0.8, 1.0] {
        e.set_param(0, v);
        assert_eq!(e.get_param(0), Some(v));
    }
    let (entries, _) = scan(
        &[plugin_dir().into()],
        &std::env::temp_dir().join("vjc-ffgl-map.json"),
        Path::new(HELPER),
    );
    let ins = inputs(&entries[0].info);
    assert_eq!(ins.len(), 3);
    assert_eq!(ins[0].name, "Gain");
    assert_eq!(
        ins[0].kind,
        vjc_fx::InputKind::Float {
            min: 0.0,
            max: 1.0,
            default: 0.5
        }
    );
}

#[test]
fn scan_lists_effect_and_caches_by_mtime() {
    let cache = std::env::temp_dir().join(format!("vjc-ffgl-cache-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&cache);
    let dirs = vec![plugin_dir().to_path_buf()];
    let (a, loads) = scan(&dirs, &cache, Path::new(HELPER));
    assert_eq!(loads, 1);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].info.name, "VJC Test Gain");
    assert_eq!(a[0].info.kind, Kind::Effect);
    assert_eq!(effect_id(&a[0]), "ffgl:TSTG");
    let (b, loads) = scan(&dirs, &cache, Path::new(HELPER));
    assert_eq!(loads, 0, "second scan comes from the cache");
    assert_eq!(a, b);
    // touching the file invalidates the entry
    let p = Path::new(&a[0].path);
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    let (_, loads) = scan(&dirs, &cache, Path::new(HELPER));
    assert_eq!(loads, 1);
}

#[test]
fn scan_skips_a_file_that_is_not_a_plugin() {
    let d = std::env::temp_dir().join(format!("vjc-ffgl-bad-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("junk.dylib"), b"not a library").unwrap();
    let (e, _) = scan(
        &[d],
        &std::env::temp_dir().join("vjc-ffgl-bad.json"),
        Path::new(HELPER),
    );
    assert!(e.is_empty());
}
