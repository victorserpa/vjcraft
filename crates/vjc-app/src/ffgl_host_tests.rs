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
    assert!(host.rescan(
        &[dir.display().to_string()],
        Some(dir.join("cache.json")),
        false
    ));
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

#[derive(Clone, Copy, PartialEq)]
enum Place {
    Layer,
    Master,
    Group,
}

struct Opts {
    kill: bool,
    dry_wet: f32,
    fmt: wgpu::TextureFormat,
    place: Place,
    /// Size of the source frame (the compositor is always N x N).
    frame: u32,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            kill: false,
            dry_wet: 1.0,
            fmt: wgpu::TextureFormat::Rgba8Unorm,
            place: Place::Layer,
            frame: N,
        }
    }
}

/// One frame through the compositor with the fixture on the layer, a group or the master.
fn render(host: &FfglHost, kill_first: bool) -> Option<(Vec<u8>, vjc_model::Effect)> {
    render_with(
        host,
        Opts {
            kill: kill_first,
            ..Default::default()
        },
    )
}

fn render_with(host: &FfglHost, o: Opts) -> Option<(Vec<u8>, vjc_model::Effect)> {
    let (device, queue) = gpu().or_else(|| {
        eprintln!("no wgpu adapter: skipped");
        None
    })?;
    let id = vjc_ffgl::effect_id(&host.effect_entries()[0]);
    let mut effect = host.new_effect(&id).unwrap();
    effect.dry_wet = o.dry_wet;
    let mut lib = crate::fx::FxLib::new();
    lib.ffgl = host.clone();
    let mut ml = crate::ml_fx::MlHub::new();
    let ctx = vjc_core::anim::AnimCtx::default();
    let draws = crate::draw_chain(
        &mut lib,
        &mut ml,
        None,
        std::slice::from_ref(&effect),
        0.0,
        (N, N),
        &ctx,
    );
    assert_eq!(draws.len(), 1, "the ffgl effect is drawn");
    if let Some(fx) = host.shared(&effect) {
        let mut fx = fx.lock().unwrap();
        if let vjc_ffgl::Status::Stopped(vjc_ffgl::HostError::Plugin(m)) = &fx.status
            && m.contains("OpenGL context")
        {
            eprintln!("no GL context: skipped");
            return None;
        }
        if o.kill {
            fx.kill_helper();
        }
    }
    let f = o.frame;
    let src: Vec<u8> = source()
        .chunks(4)
        .take((f * f) as usize)
        .flatten()
        .copied()
        .collect();
    let mut comp = Compositor::with_format(&device, N, N, o.fmt);
    let (on_layer, on_master, groups) = match o.place {
        Place::Layer => (draws, vec![], vec![]),
        Place::Master => (vec![], draws, vec![]),
        Place::Group => (
            vec![],
            vec![],
            vec![crate::compositor::GroupDraw {
                id: None,
                start: 0,
                len: 1,
                opacity: 1.0,
                blend: BlendMode::Normal,
                effects: draws,
                mask: Default::default(),
            }],
        ),
    };
    let layer = LayerDraw {
        id: uuid::Uuid::from_u128(1),
        opacity: 1.0,
        blend: BlendMode::Normal,
        fit: FitMode::Stretch,
        effects: on_layer,
        frame: Some((f, f, &src, 1)),
        generator: None,
        transition: None,
        transform: vjc_core::transform::Affine::IDENTITY,
        channels: Default::default(),
        mask: Default::default(),
        orient: Default::default(),
    };
    comp.render_groups(&device, &queue, &[layer], &groups, &on_master, 1.0);
    let (_, _, out) = comp.read_rgba(&device, &queue)?;
    Some((out, effect))
}

fn assert_inverted(out: &[u8]) {
    for (i, (a, b)) in source().chunks(4).zip(out.chunks(4)).enumerate() {
        for c in 0..3 {
            let want = 255 - a[c] as i32;
            assert!(
                (want - b[c] as i32).abs() <= 2,
                "pixel {i} ch {c}: {a:?} -> {b:?}"
            );
        }
    }
}

#[test]
fn half_dry_wet_mixes_the_plugin_with_its_input() {
    let (host, _dir) = scanned("drywet");
    let Some((out, _)) = render_with(
        &host,
        Opts {
            dry_wet: 0.5,
            ..Default::default()
        },
    ) else {
        return;
    };
    for (i, b) in out.chunks(4).enumerate() {
        for c in 0..3 {
            assert!((b[c] as i32 - 128).abs() <= 2, "pixel {i} ch {c}: {b:?}");
        }
    }
}

#[test]
fn a_16_bit_composition_still_runs_the_plugin() {
    let (host, _dir) = scanned("fmt16");
    let Some((out, _)) = render_with(
        &host,
        Opts {
            fmt: wgpu::TextureFormat::Rgba16Float,
            ..Default::default()
        },
    ) else {
        return;
    };
    assert_inverted(&out);
}

#[test]
fn the_plugin_runs_on_the_master_and_on_a_group() {
    let (host, _dir) = scanned("places");
    for place in [Place::Master, Place::Group] {
        let Some((out, _)) = render_with(
            &host,
            Opts {
                place,
                ..Default::default()
            },
        ) else {
            return;
        };
        assert_inverted(&out);
    }
}

#[test]
fn an_input_of_another_size_shows_a_note() {
    let (host, _dir) = scanned("size");
    let Some((_, effect)) = render_with(
        &host,
        Opts {
            frame: 32,
            ..Default::default()
        },
    ) else {
        return;
    };
    assert!(
        host.badge(&effect).is_some_and(|b| b.contains("size")),
        "{:?}",
        host.badge(&effect)
    );
}

#[test]
fn params_follow_the_plugin_index_when_an_input_is_not_mapped() {
    use vjc_fx::{Input, InputKind};
    let inputs = vec![
        Input {
            name: "tint".into(),
            group: None,
            kind: InputKind::Color { default: [0.0; 4] },
        },
        Input {
            name: "amount".into(),
            group: None,
            kind: InputKind::Float {
                min: 0.0,
                max: 1.0,
                default: 0.5,
            },
        },
        Input {
            name: "on".into(),
            group: None,
            kind: InputKind::Bool { default: true },
        },
    ];
    let (params, index) = crate::ffgl_host::params_for(&inputs);
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name, "amount");
    assert_eq!(index, vec![1, 2], "effect param 0 is plugin parameter 1");
}

#[test]
fn temp_cache_of_a_default_scan_is_removed_when_the_host_is_dropped() {
    let dir = temp("tmpcache");
    let host = FfglHost::default();
    assert!(host.rescan(&[dir.display().to_string()], None, false));
    let t = std::time::Instant::now();
    while host.scanning() && t.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let cache = host.temp_cache().expect("fallback cache in use");
    assert!(cache.exists());
    drop(host);
    assert!(!cache.exists());
}

#[test]
fn a_layer_with_the_ffgl_effect_comes_out_inverted() {
    let (host, _dir) = scanned("invert");
    let Some((out, _)) = render(&host, false) else {
        return;
    };
    for (i, (a, b)) in source().chunks(4).zip(out.chunks(4)).enumerate() {
        for c in 0..3 {
            let want = 255 - a[c] as i32;
            assert!(
                (want - b[c] as i32).abs() <= 1,
                "pixel {i} ch {c}: {a:?} -> {b:?}"
            );
        }
    }
}

#[test]
fn killing_the_helper_passes_the_frame_through_and_marks_the_plugin_stopped() {
    let (host, _dir) = scanned("kill");
    let Some((out, effect)) = render(&host, true) else {
        return;
    };
    let src = source();
    for (i, (a, b)) in src.chunks(4).zip(out.chunks(4)).enumerate() {
        for c in 0..3 {
            assert!(
                (a[c] as i32 - b[c] as i32).abs() <= 1,
                "pixel {i}: {a:?} vs {b:?}"
            );
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
    assert!(
        dir.join("cache.json").exists(),
        "the scan ran on the isolated cache"
    );
}

/// Cost of one CPU round trip at 1080p (GPU readback + helper + upload). Run by hand:
/// `cargo test -p vjc-app --bin vjcraft readback_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn readback_cost_1080p() {
    let (host, _dir) = scanned("cost");
    let Some((device, queue)) = gpu() else { return };
    let id = vjc_ffgl::effect_id(&host.effect_entries()[0]);
    let effect = host.new_effect(&id).unwrap();
    let mut lib = crate::fx::FxLib::new();
    lib.ffgl = host.clone();
    let mut ml = crate::ml_fx::MlHub::new();
    let ctx = vjc_core::anim::AnimCtx::default();
    let (w, h) = (1920u32, 1080u32);
    let src = vec![90u8; (w * h * 4) as usize];
    let mut comp = Compositor::new(&device, w, h);
    let mut times = vec![];
    for _ in 0..12 {
        let draws = crate::draw_chain(
            &mut lib,
            &mut ml,
            None,
            std::slice::from_ref(&effect),
            0.0,
            (w, h),
            &ctx,
        );
        let layer = LayerDraw {
            id: uuid::Uuid::from_u128(1),
            opacity: 1.0,
            blend: BlendMode::Normal,
            fit: FitMode::Stretch,
            effects: draws,
            frame: Some((w, h, &src, 1)),
            generator: None,
            transition: None,
            transform: vjc_core::transform::Affine::IDENTITY,
            channels: Default::default(),
            mask: Default::default(),
            orient: Default::default(),
        };
        let t = std::time::Instant::now();
        comp.render(&device, &queue, &[layer], &[], 1.0);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        times.push(t.elapsed());
    }
    eprintln!(
        "1080p frame with ffgl effect, ms: {:?}",
        times
            .iter()
            .map(|t| t.as_secs_f64() * 1e3)
            .collect::<Vec<_>>()
    );
}
