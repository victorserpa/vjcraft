//! OpenGL host path: a fixture plugin that draws with GL is loaded through the helper.
use std::path::{Path, PathBuf};
use std::time::Duration;
use vjc_ffgl::*;

const HELPER: &str = env!("CARGO_BIN_EXE_vjc-ffgl-helper");

fn gl_plugin() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vjc-ffgl-gl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(windows) {
        "dll"
    } else {
        "so"
    };
    let out = dir.join(format!("invert.{ext}"));
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gl_plugin.rs");
    let st = std::process::Command::new("rustc")
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "cdylib",
            "--crate-name",
            "gl_plugin",
            "-O",
            "-o",
        ])
        .arg(&out)
        .arg(src)
        .status()
        .unwrap();
    assert!(st.success(), "gl fixture build failed");
    out
}

#[test]
fn gl_plugin_inverts_a_64x64_frame() {
    let path = gl_plugin();
    let mut e = FfglEffect::new(
        &path.to_string_lossy(),
        Path::new(HELPER),
        Duration::from_secs(20),
    );
    assert_eq!(e.status, Status::Running);
    let mut rgba = Vec::new();
    for i in 0..64 * 64u32 {
        rgba.extend([(i % 251) as u8, (i % 64 * 4) as u8, (i / 64 * 4) as u8, 200]);
    }
    let f = Frame { w: 64, h: 64, rgba };
    assert_eq!(e.process(&f).rgba, f.rgba, "frame 1 passes through");
    let t = std::time::Instant::now();
    while e.status == Status::Running && t.elapsed() < Duration::from_secs(20) {
        e.settle();
        if e.process(&f).rgba != f.rgba {
            break;
        }
    }
    let out = e.process(&f); // the previous frame, inverted
    if let Status::Stopped(HostError::Plugin(m)) = &e.status
        && m.contains("OpenGL context")
    {
        eprintln!("skipping: no GL context available ({m})");
        return;
    }
    assert_eq!(e.status, Status::Running, "{:?}", e.status);
    assert_eq!(out.rgba.len(), f.rgba.len());
    for (i, (a, b)) in f.rgba.chunks(4).zip(out.rgba.chunks(4)).enumerate() {
        assert_eq!(
            [255 - a[0], 255 - a[1], 255 - a[2], a[3]],
            [b[0], b[1], b[2], b[3]],
            "pixel {i}"
        );
    }
    // second frame, same size: the same path keeps working
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(e.process(&f).rgba, out.rgba);
}
