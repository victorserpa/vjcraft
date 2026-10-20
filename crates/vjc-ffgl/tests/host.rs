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

#[test]
fn out_of_range_index_and_non_finite_values_are_rejected_locally() {
    let mut e = effect(5000);
    e.set_param(7, 0.5); // fixture would crash on an index it does not have
    e.set_param(usize::MAX, 0.5);
    assert_eq!(e.status, Status::Running);
    assert_eq!(e.get_param(7), None);
    assert_eq!(e.status, Status::Running);
    e.set_param(0, f32::NAN);
    e.set_param(0, f32::INFINITY);
    assert_eq!(e.get_param(0), Some(0.5), "plugin never saw the bad values");
}

#[test]
fn enable_stays_stopped_when_restoring_a_parameter_fails() {
    let mut e = effect(5000);
    e.set_param(1, 0.75); // fixture aborts while setting this value
    assert!(matches!(e.status, Status::Stopped(_)));
    e.enable(); // restore replays the same Set, which crashes again
    assert!(matches!(e.status, Status::Stopped(_)), "{:?}", e.status);
    let f = frame(80);
    assert_eq!(e.process(&f).rgba, f.rgba, "pass-through");
    e.set_param(1, 0.0);
    e.enable();
    assert_eq!(e.status, Status::Running);
}

#[test]
fn lifecycle_follows_the_ffgl_order() {
    let dir = std::env::temp_dir().join(format!("vjc-ffgl-life-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join(Path::new(&plugin()).file_name().unwrap());
    std::fs::copy(plugin(), &lib).unwrap();
    let log = dir.join(format!(
        "{}.log",
        lib.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&log, "").unwrap();
    let path = lib.to_string_lossy().to_string();
    let mut h = Helper::spawn(Path::new(HELPER)).unwrap();
    let t = Duration::from_secs(5);
    let load = Request::Load { path: path.clone() };
    assert!(matches!(h.request(&load, t), Ok(Response::Info { .. })));
    // per-instance calls carry the instance id: the fixture fails them otherwise
    assert_eq!(
        h.request(
            &Request::Set {
                index: 0,
                value: 0.25
            },
            t
        ),
        Ok(Response::Done)
    );
    assert_eq!(
        h.request(&Request::Get { index: 0 }, t),
        Ok(Response::Value { value: 0.25 })
    );
    assert!(matches!(h.request(&load, t), Ok(Response::Info { .. }))); // replace
    let _ = h.request(&Request::Quit, t);
    let codes: Vec<u32> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| l.parse().unwrap())
        .collect();
    let seq: Vec<u32> = codes
        .into_iter()
        .filter(|c| matches!(c, 34 | 2 | 18 | 19))
        .collect();
    // INITIALISE, INSTANTIATE, then DEINSTANTIATE + DEINITIALISE on replace and on quit
    assert_eq!(seq, [34, 18, 19, 2, 34, 18, 19, 2]);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_bundle_is_resolved_to_its_executable() {
    let d = std::env::temp_dir().join(format!("vjc-ffgl-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let macos = d.join("Gain.bundle/Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::copy(plugin(), macos.join("GainExe")).unwrap();
    std::fs::write(
        d.join("Gain.bundle/Contents/Info.plist"),
        "<plist><dict><key>CFBundleExecutable</key><string>GainExe</string></dict></plist>",
    )
    .unwrap();
    let cache = d.join("cache.json");
    let (e, loads) = scan(std::slice::from_ref(&d), &cache, Path::new(HELPER));
    assert_eq!((e.len(), loads), (1, 1));
    assert_eq!(e[0].info.name, "VJC Test Gain");
    assert_eq!(
        scan(std::slice::from_ref(&d), &cache, Path::new(HELPER)).1,
        0
    );
    // the cache follows the executable's mtime, not the directory's
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(macos.join("GainExe"))
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        scan(std::slice::from_ref(&d), &cache, Path::new(HELPER)).1,
        1
    );
    // and the effect loads the bundle too
    let mut fx = FfglEffect::new(&e[0].path, Path::new(HELPER), Duration::from_secs(5));
    assert_eq!(fx.status, Status::Running);
    fx.set_param(0, 1.0);
    assert_eq!(fx.process(&frame(100)).rgba[0], 200);
}

#[test]
fn failed_plugins_are_cached_by_mtime() {
    let d = std::env::temp_dir().join(format!("vjc-ffgl-failcache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let junk = d.join("junk.dylib");
    std::fs::write(&junk, b"not a library").unwrap();
    let cache = d.join("cache.json");
    assert_eq!(
        scan(std::slice::from_ref(&d), &cache, Path::new(HELPER)).1,
        1
    );
    assert_eq!(
        scan(std::slice::from_ref(&d), &cache, Path::new(HELPER)).1,
        0,
        "not reloaded"
    );
    let f = std::fs::OpenOptions::new().write(true).open(&junk).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        scan(std::slice::from_ref(&d), &cache, Path::new(HELPER)).1,
        1,
        "changed file is retried"
    );
}

#[test]
fn enable_carries_on_when_restoring_a_parameter_returns_ff_fail() {
    let mut e = effect(5000);
    e.set_param(1, 0.25); // fixture answers FF_FAIL for this value
    e.enable();
    assert_eq!(e.status, Status::Running, "{:?}", e.status);
    e.set_param(0, 1.0);
    assert_eq!(e.process(&frame(100)).rgba[0], 200);
}

fn slow_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vjc-ffgl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let src = PathBuf::from(plugin());
    let dst = d.join(src.file_name().unwrap());
    std::fs::copy(&src, &dst).unwrap();
    let mut slow = dst.clone().into_os_string();
    slow.push(".slow");
    std::fs::write(slow, b"").unwrap();
    d
}

#[test]
fn scan_timeout_is_retried_with_the_long_deadline() {
    let d = slow_dir("slowretry");
    let cache = d.join("cache.json");
    let (quick, long) = (Duration::from_millis(300), Duration::from_secs(10));
    let dirs = std::slice::from_ref(&d);
    let (f, loads) = scan_with(dirs, &cache, Path::new(HELPER), false, quick, long);
    assert_eq!((f.len(), loads), (0, 1), "first try times out");
    let (f, loads) = scan_with(dirs, &cache, Path::new(HELPER), false, quick, long);
    assert_eq!(
        (f.len(), loads),
        (1, 1),
        "slow plugin retried with the long deadline"
    );
}

#[test]
fn forced_scan_clears_a_cached_timeout() {
    let d = slow_dir("slowforce");
    let cache = d.join("cache.json");
    let dirs = std::slice::from_ref(&d);
    let (quick, short) = (Duration::from_millis(300), Duration::from_millis(300));
    // both deadlines too short: ends up in failed
    scan_with(dirs, &cache, Path::new(HELPER), false, quick, short);
    scan_with(dirs, &cache, Path::new(HELPER), false, quick, short);
    let (_, loads) = scan_with(dirs, &cache, Path::new(HELPER), false, quick, short);
    assert_eq!(loads, 0, "failed is remembered");
    let (f, loads) = scan_with(
        dirs,
        &cache,
        Path::new(HELPER),
        true,
        quick,
        Duration::from_secs(10),
    );
    assert_eq!((f.len(), loads), (1, 1), "force retries it");
}

#[test]
fn param_changes_are_coalesced_and_sent_with_the_frame() {
    let mut e = effect(5000);
    // 0.75 on param 1 makes the fixture abort when it is set; only the latest value may go out
    e.set_param(1, 0.75);
    e.set_param(1, 0.0);
    e.set_param(0, 0.5);
    let out = e.process(&frame(100));
    assert_eq!(e.status, Status::Running, "{:?}", e.status);
    assert_eq!(
        out.rgba[..4],
        [100, 50, 10, 200],
        "gain 0.5 applied with the frame"
    );
}

#[test]
fn setting_a_param_never_talks_to_the_helper_before_the_frame() {
    let mut e = effect(5000);
    e.kill_helper();
    let t = std::time::Instant::now();
    for i in 0..50 {
        e.set_param(0, i as f32 / 100.0);
    }
    assert!(t.elapsed() < Duration::from_millis(200));
    assert_eq!(
        e.status,
        Status::Running,
        "only the frame notices the dead helper"
    );
    let f = frame(80);
    assert_eq!(e.process(&f).rgba, f.rgba);
    assert!(matches!(e.status, Status::Stopped(_)));
}
