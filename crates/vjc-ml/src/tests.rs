use super::*;

#[test]
fn resize_identity_and_constant() {
    let img: Vec<u8> = (0..4 * 3 * 4).map(|i| (i * 5) as u8).collect();
    assert_eq!(resize_rgba(&img, (4, 3), (4, 3)), img);
    let flat = [10u8, 20, 30, 255].repeat(7 * 5);
    let out = resize_rgba(&flat, (7, 5), (3, 2));
    assert_eq!(out.len(), 3 * 2 * 4);
    assert!(out.chunks(4).all(|p| p == [10, 20, 30, 255]));
}

#[test]
fn resize_downscale_averages_halves() {
    // Left half black, right half white, 4x1 -> 2x1: pure black and pure white.
    let mut img = vec![];
    for x in 0..4 {
        let v = if x < 2 { 0 } else { 255 };
        img.extend_from_slice(&[v, v, v, 255]);
    }
    let out = resize_rgba(&img, (4, 1), (2, 1));
    assert_eq!(out[0], 0);
    assert_eq!(out[4], 255);
}

#[test]
fn chw_layout_and_normalisation() {
    // 2x1 image: pixel 0 = (255, 0, 0), pixel 1 = (0, 255, 0).
    let img = [255, 0, 0, 255, 0, 255, 0, 255];
    let plain = to_chw(&img, (2, 1), false);
    assert_eq!(plain, vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let n = to_chw(&img, (2, 1), true);
    assert!((n[0] - (1.0 - 0.485) / 0.229).abs() < 1e-5);
    assert!((n[1] - (0.0 - 0.485) / 0.229).abs() < 1e-5);
    assert!((n[2] - (0.0 - 0.456) / 0.224).abs() < 1e-5);
    assert!((n[3] - (1.0 - 0.456) / 0.224).abs() < 1e-5);
}

#[test]
fn normalize_range_and_flat() {
    assert_eq!(normalize_u8(&[1.0, 2.0, 3.0], 1.0, 3.0), vec![0, 128, 255]);
    assert_eq!(normalize_u8(&[5.0, 5.0], 5.0, 5.0), vec![0, 0]);
    assert_eq!(normalize_u8(&[f32::NAN, 9.0], 0.0, 1.0), vec![0, 255]);
}

#[test]
fn missing_model_reports_where_to_put_it() {
    let dir = std::env::temp_dir().join("vjc-ml-no-such-dir");
    let w = Worker::spawn(ModelKind::Depth, &dir);
    match w.status() {
        Status::NotFound(m) => {
            assert!(
                m.contains("model-small.onnx") && m.contains("docs/ml.md"),
                "{m}"
            );
        }
        s => panic!("{s:?}"),
    }
    w.submit(vec![0; 4], (1, 1));
    assert!(w.take().is_none());
}

// ---- tests needing the real model files (skipped when absent, e.g. in CI)

fn models_dir() -> Option<PathBuf> {
    let d = std::env::var_os("VJC_MODELS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join("Library/Application Support/vjcraft-models-dl"))
        })?;
    d.is_dir().then_some(d)
}

fn model(kind: ModelKind) -> Option<PathBuf> {
    let p = kind.path_in(&models_dir()?);
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("SKIPPED: {} not found", kind.file_name());
        None
    }
}

/// White rectangle on black, `w` x `h`.
fn rect_image(w: u32, h: u32) -> Vec<u8> {
    let mut img = vec![];
    for y in 0..h {
        for x in 0..w {
            let inside = x > w / 3 && x < 2 * w / 3 && y > h / 4 && y < 3 * h / 4;
            let v = if inside { 255 } else { 0 };
            img.extend_from_slice(&[v, v, v, 255]);
        }
    }
    img
}

/// A smooth colourful synthetic scene (gradient + blobs), more natural than the rectangle.
fn scene(w: u32, h: u32) -> Vec<u8> {
    let mut img = vec![];
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let d = ((u - 0.5).powi(2) + (v - 0.5).powi(2)).sqrt();
            img.push((255.0 * u) as u8);
            img.push((255.0 * (1.0 - d * 1.5).clamp(0.0, 1.0)) as u8);
            img.push((255.0 * v) as u8);
            img.push(255);
        }
    }
    img
}

#[test]
fn depth_model_shape_and_range() {
    let Some(p) = model(ModelKind::Depth) else {
        return;
    };
    let mut m = Depth::load(&p).unwrap();
    let (w, h) = ModelKind::Depth.input_size();
    let out = m.infer(&scene(w, h), (w, h)).unwrap();
    assert_eq!((out.w, out.h), (256, 256));
    assert_eq!(out.data.len(), 256 * 256);
    let (lo, hi) = (
        *out.data.iter().min().unwrap(),
        *out.data.iter().max().unwrap(),
    );
    assert!(hi > lo, "depth is flat");
    assert_eq!((lo, hi), (0, 255), "first frame uses the whole range");
    // Other input size goes through the resize.
    let out2 = m.infer(&scene(320, 180), (320, 180)).unwrap();
    assert_eq!(out2.data.len(), 256 * 256);
    eprintln!("depth ok, range {lo}..{hi}");
}

#[test]
fn matting_model_shape_range_and_state() {
    let Some(p) = model(ModelKind::Matting) else {
        return;
    };
    let mut m = Matting::load(&p).unwrap();
    let (w, h) = ModelKind::Matting.input_size();
    let img = rect_image(w, h);
    let mut last = None;
    for _ in 0..4 {
        last = Some(m.infer(&img, (w, h)).unwrap());
    }
    let out = last.unwrap();
    assert_eq!((out.w, out.h), (w, h));
    assert_eq!(out.data.len(), (w * h) as usize);
    let mean = |inside: bool| {
        let (mut s, mut n) = (0u64, 0u64);
        for y in 0..h {
            for x in 0..w {
                let i = x > w / 3 && x < 2 * w / 3 && y > h / 4 && y < 3 * h / 4;
                if i == inside {
                    s += u64::from(out.data[(y * w + x) as usize]);
                    n += 1;
                }
            }
        }
        s as f64 / n as f64
    };
    let (inside, outside) = (mean(true), mean(false));
    // The network only mattes people; a white rectangle is not one, so it may answer 0
    // everywhere. Separation is only required when the model reacts at all.
    eprintln!("RVM pha mean inside {inside:.1}, outside {outside:.1}");
    assert!(inside >= outside, "inside {inside} outside {outside}");
    if out.data.iter().any(|&v| v > 0) {
        assert!(inside > outside);
    }
}

/// One frame of ffmpeg's `testsrc2` as RGBA at `size` (None without ffmpeg).
fn testsrc2_frame((w, h): (u32, u32)) -> Option<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={w}x{h}:rate=1"))
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .ok()?;
    (out.status.success() && out.stdout.len() == (w * h * 4) as usize).then_some(out.stdout)
}

#[test]
fn matting_on_testsrc2_frame_has_valid_shape_and_range() {
    let Some(p) = model(ModelKind::Matting) else {
        return;
    };
    let size = ModelKind::Matting.input_size();
    let Some(img) = testsrc2_frame(size) else {
        eprintln!("SKIPPED: ffmpeg not available");
        return;
    };
    let mut m = Matting::load(&p).unwrap();
    let pha = m.infer_f32(&img, size).unwrap();
    assert_eq!(pha.len(), (size.0 * size.1) as usize);
    assert!(
        pha.iter()
            .all(|v| v.is_finite() && *v >= -1e-4 && *v <= 1.0 + 1e-4)
    );
}

#[test]
fn matting_reset_restores_first_frame_output() {
    let Some(p) = model(ModelKind::Matting) else {
        return;
    };
    let mut m = Matting::load(&p).unwrap();
    let (w, h) = ModelKind::Matting.input_size();
    let img = rect_image(w, h);
    let first = m.infer(&img, (w, h)).unwrap();
    m.infer(&img, (w, h)).unwrap();
    m.reset();
    assert_eq!(m.infer(&img, (w, h)).unwrap(), first);
}

#[test]
fn worker_delivers_mask_and_replaces_stale_frames() {
    let Some(_) = model(ModelKind::Depth) else {
        return;
    };
    let dir = models_dir().unwrap();
    let wk = Worker::spawn(ModelKind::Depth, &dir);
    let (w, h) = ModelKind::Depth.input_size();
    // Submitting many frames never blocks; a mask shows up eventually.
    let t0 = Instant::now();
    let mut got = None;
    while got.is_none() && t0.elapsed().as_secs() < 60 {
        wk.submit(scene(w, h), (w, h));
        std::thread::sleep(std::time::Duration::from_millis(20));
        got = wk.take();
    }
    assert_eq!(wk.status(), Status::Ready);
    assert_eq!(got.expect("mask").data.len(), 256 * 256);
}

/// Timing, run with `cargo test --release -p vjc-ml -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_inference_ms() {
    for kind in [ModelKind::Depth, ModelKind::Matting] {
        let Some(p) = model(kind) else { continue };
        let (w, h) = kind.input_size();
        let img = scene(w, h);
        let t0 = Instant::now();
        let mut run: Box<dyn FnMut()> = match kind {
            ModelKind::Depth => {
                let mut m = Depth::load(&p).unwrap();
                Box::new(move || {
                    m.infer(&img, (w, h)).unwrap();
                })
            }
            ModelKind::Matting => {
                let mut m = Matting::load(&p).unwrap();
                Box::new(move || {
                    m.infer(&img, (w, h)).unwrap();
                })
            }
        };
        eprintln!(
            "{}: load {:.0} ms",
            kind.label(),
            t0.elapsed().as_secs_f32() * 1e3
        );
        run();
        let n = 20;
        let t = Instant::now();
        for _ in 0..n {
            run();
        }
        eprintln!(
            "{} {}x{}: {:.1} ms/inference",
            kind.label(),
            w,
            h,
            t.elapsed().as_secs_f32() * 1e3 / n as f32
        );
    }
}

#[test]
fn frame_without_finite_values_keeps_the_depth_range() {
    let first = smooth_range(None, &[1.0, 5.0]).unwrap();
    assert_eq!(first, (1.0, 5.0));
    assert_eq!(smooth_range(Some(first), &[f32::NAN, f32::INFINITY]), None);
    // The caller keeps `first`, so the next good frame eases from it, not from (0, 0).
    let next = smooth_range(Some(first), &[1.0, 5.0]).unwrap();
    assert_eq!(next, (1.0, 5.0));
    // Negative control: easing from a crushed (0, 0) range would give 0.7 here.
    assert_ne!(smooth_range(Some((0.0, 0.0)), &[1.0, 5.0]).unwrap(), next);
}
