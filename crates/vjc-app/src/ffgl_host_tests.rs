//! FFGL plugins in the app (docs/specs/ffgl-host.md, stage 2a item 4): scan, browser entry,
//! layer effect through the compositor, and pass-through when the helper dies.
//! Needs the `vjc-ffgl-helper` binary next to the test binary (`cargo build -p vjc-ffgl`).
use crate::compositor::{Compositor, LayerDraw};
use crate::ffgl_host::FfglHost;
use eframe::egui_wgpu::wgpu;
use std::path::{Path, PathBuf};
use vjc_model::{BlendMode, FitMode};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vjc-ffgl-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds vjc-ffgl's GL fixture (an inverting plugin) into `dir`.
fn build_fixture(dir: &Path) -> PathBuf {
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(windows) {
        "dll"
    } else {
        "so"
    };
    let out = dir.join(format!("invert.{ext}"));
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vjc-ffgl/tests/fixtures/gl_plugin.rs");
    let st = std::process::Command::new("rustc")
        .args(["--edition", "2024", "--crate-type", "cdylib"])
        .args(["--crate-name", "gl_plugin", "-O", "-o"])
        .arg(&out)
        .arg(src)
        .status()
        .unwrap();
    assert!(st.success(), "fixture build failed");
    out
}

fn scanned(name: &str) -> (FfglHost, PathBuf) {
    let dir = temp(name);
    build_fixture(&dir);
    assert!(
        vjc_ffgl::helper_path().exists(),
        "build the helper first: cargo build -p vjc-ffgl"
    );
    let host = FfglHost::default();
    assert!(host.rescan(&[dir.display().to_string()], Some(dir.join("cache.json")), false));
    let t = std::time::Instant::now();
    while host.scanning() && t.elapsed() < std::time::Duration::from_secs(60) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!host.scanning(), "scan did not finish");
    (host, dir)
}

#[test]
fn scan_lists_the_fixture_and_the_browser_shows_it_in_its_own_group() {
    let (host, _dir) = scanned("scan");
    let list = host.effect_entries();
    assert_eq!(list.len(), 1, "{list:?}");
    let id = vjc_ffgl::effect_id(&list[0]);
    assert!(id.starts_with("ffgl:"));
    let all = crate::browser_fx_with(&host);
    let hit = all
        .iter()
        .find(|e| matches!(&e.item, crate::browser::Item::Effect(i) if *i == id))
        .expect("plugin in the Effects browser");
    assert_eq!(hit.category.as_deref(), Some(crate::ffgl_host::GROUP));
    // built-ins stay, and an empty host adds nothing
    assert!(all.len() > crate::browser_fx_with(&FfglHost::default()).len());
    let e = host.new_effect(&id).expect("addable like any effect");
    assert_eq!(e.kind, id);
    assert!(host.new_effect("ffgl:nope").is_none());
}

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

const N: u32 = 64;

fn source() -> Vec<u8> {
    let mut px = Vec::new();
    for i in 0..N * N {
        px.extend([(i % 251) as u8, (i % N * 4) as u8, (i / N * 4) as u8, 255]);
    }
    px
}

/// One frame through the compositor with the fixture on the layer.
fn render(host: &FfglHost, kill_first: bool) -> Option<(Vec<u8>, vjc_model::Effect)> {
    let (device, queue) = gpu().or_else(|| {
        eprintln!("no wgpu adapter: skipped");
        None
    })?;
    let id = vjc_ffgl::effect_id(&host.effect_entries()[0]);
    let effect = host.new_effect(&id).unwrap();
    let mut lib = crate::fx::FxLib::new();
    lib.ffgl = host.clone();
    let mut ml = crate::ml_fx::MlHub::new();
    let ctx = vjc_core::anim::AnimCtx::default();
    let draws = crate::draw_chain(&mut lib, &mut ml, None, &[effect.clone()], 0.0, (N, N), &ctx);
    assert_eq!(draws.len(), 1, "the ffgl effect is drawn");
    if let Some(fx) = host.shared(&effect) {
        let mut fx = fx.lock().unwrap();
        if let vjc_ffgl::Status::Stopped(vjc_ffgl::HostError::Plugin(m)) = &fx.status
            && m.contains("OpenGL context")
        {
            eprintln!("no GL context: skipped");
            return None;
        }
        if kill_first {
            fx.kill_helper();
        }
    }
    let src = source();
    let mut comp = Compositor::new(&device, N, N);
    let layer = LayerDraw {
        id: uuid::Uuid::from_u128(1),
        opacity: 1.0,
        blend: BlendMode::Normal,
        fit: FitMode::Stretch,
        effects: draws,
        frame: Some((N, N, &src, 1)),
        generator: None,
        transition: None,
        transform: vjc_core::transform::Affine::IDENTITY,
        channels: Default::default(),
        mask: Default::default(),
        orient: Default::default(),
    };
    comp.render(&device, &queue, &[layer], &[], 1.0);
    let (_, _, out) = comp.read_rgba(&device, &queue)?;
    Some((out, effect))
}

#[test]
fn a_layer_with_the_ffgl_effect_comes_out_inverted() {
    let (host, _dir) = scanned("invert");
    let Some((out, _)) = render(&host, false) else { return };
    for (i, (a, b)) in source().chunks(4).zip(out.chunks(4)).enumerate() {
        for c in 0..3 {
            let want = 255 - a[c] as i32;
            assert!((want - b[c] as i32).abs() <= 1, "pixel {i} ch {c}: {a:?} -> {b:?}");
        }
    }
}

#[test]
fn killing_the_helper_passes_the_frame_through_and_marks_the_plugin_stopped() {
    let (host, _dir) = scanned("kill");
    let Some((out, effect)) = render(&host, true) else { return };
    let src = source();
    for (i, (a, b)) in src.chunks(4).zip(out.chunks(4)).enumerate() {
        for c in 0..3 {
            assert!((a[c] as i32 - b[c] as i32).abs() <= 1, "pixel {i}: {a:?} vs {b:?}");
        }
    }
    assert_eq!(host.badge(&effect).as_deref(), Some("Plugin stopped"));
}

#[test]
fn saving_prefs_with_ffgl_folders_starts_a_scan_of_those_folders_only() {
    let dir = temp("prefs");
    let mut st = crate::State::headless(Default::default());
    let st = &mut st;
    st.fx.ffgl.isolate(dir.join("cache.json"));
    assert!(!st.fx.ffgl.scanning(), "app() starts no scan");
    st.prefs.video.ffgl_dirs.push(dir.display().to_string());
    st.save_prefs();
    assert_eq!(st.ffgl_dirs_scanned, st.prefs.video.ffgl_dirs);
    let t = std::time::Instant::now();
    while st.fx.ffgl.scanning() && t.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(dir.join("cache.json").exists(), "the scan ran on the isolated cache");
}
