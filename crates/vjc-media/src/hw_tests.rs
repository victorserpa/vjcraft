//! Hardware decode (docs/specs/hw-decode.md). Clips are generated with the `ffmpeg` CLI
//! (skipped with a message when it is missing). Only macOS (VideoToolbox) can assert that
//! the hardware path was really taken.

use super::*;
use std::process::Command;
use std::sync::Mutex;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vjc-hw-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// H.264 test clip of `secs` seconds at `size` (e.g. "1280x720"), keyframe every 10 frames.
fn make_clip(dir: &Path, size: &str, secs: u32) -> Option<PathBuf> {
    let out = dir.join(format!("clip-{size}.mp4"));
    let src = format!("testsrc2=size={size}:rate=30");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(src)
        .args([
            "-t",
            &secs.to_string(),
            "-c:v",
            "libx264",
            "-g",
            "10",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipped: no ffmpeg CLI with libx264");
        return None;
    }
    Some(out)
}

fn max_err(a: &Frame, b: &Frame) -> u8 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.data
        .iter()
        .zip(b.data.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

fn decode(path: &Path, hw: Hw, n: usize) -> (Vec<Frame>, bool) {
    let mut sc = Scrubber::open_with_hw(path, false, hw).unwrap();
    let mut v = vec![];
    while v.len() < n {
        match sc.next() {
            Some((_, f)) => v.push(f),
            None => break,
        }
    }
    (v, sc.is_hw())
}

fn wait_frame(p: &Player) {
    let t = Instant::now();
    while p.latest_frame().is_none() && t.elapsed().as_secs() < 10 {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn hw_matches_sw_within_4() {
    let dir = tmp("match");
    let Some(clip) = make_clip(&dir, "1280x720", 2) else {
        return;
    };
    let (sw, sw_hw) = decode(&clip, Hw::Off, 30);
    let (hw, used) = decode(&clip, Hw::On, 30);
    assert!(!sw_hw);
    assert_eq!((sw.len(), hw.len()), (30, 30));
    if cfg!(target_os = "macos") {
        assert!(used, "VideoToolbox should decode this H.264 clip");
    }
    for (i, (a, b)) in sw.iter().zip(&hw).enumerate() {
        let e = max_err(a, b);
        assert!(e <= 4, "frame {i}: channel error {e}");
    }
}

#[test]
fn failed_device_falls_back_to_software() {
    let dir = tmp("fail");
    let Some(clip) = make_clip(&dir, "640x360", 1) else {
        return;
    };
    let (sw, _) = decode(&clip, Hw::Off, 20);
    let (fb, used) = decode(&clip, Hw::ForceFail, 20);
    assert!(!used, "a failed device must report software");
    assert_eq!(fb.len(), 20);
    for (a, b) in sw.iter().zip(&fb) {
        assert!(max_err(a, b) <= 4);
    }
}

#[test]
fn seek_and_reverse_land_on_the_same_frame() {
    let dir = tmp("seek");
    let Some(clip) = make_clip(&dir, "640x360", 3) else {
        return;
    };
    let mut sw = Scrubber::open_with_hw(&clip, false, Hw::Off).unwrap();
    let mut hw = Scrubber::open_with_hw(&clip, false, Hw::On).unwrap();
    // Forward jump, backward (reverse playback seeks), then a step to the next frame.
    for t in [2.0, 0.5, 1.0, 1.0 + 1.0 / 30.0, 0.0] {
        let a = sw.frame_at(t).unwrap().expect("sw frame");
        let b = hw.frame_at(t).unwrap().expect("hw frame");
        assert!((sw.last_t - hw.last_t).abs() < 1e-6, "t={t}: pts differ");
        assert!(max_err(&a, &b) <= 4, "t={t}");
    }
}

#[test]
fn session_cap_sends_extra_clips_to_software() {
    let slots = HwSlots::default();
    let a = slots.try_acquire(2).expect("first");
    let _b = slots.try_acquire(2).expect("second");
    assert!(slots.try_acquire(2).is_none(), "cap reached");
    assert_eq!(slots.active(), 2);
    drop(a);
    assert_eq!(slots.active(), 1);
    assert!(slots.try_acquire(2).is_some(), "slot freed on drop");
    assert!(slots.try_acquire(0).is_none(), "cap 0 = no hardware");
}

#[test]
fn player_reports_hw_or_sw_status() {
    let _serial = WARM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmp("status");
    let Some(clip) = make_clip(&dir, "640x360", 1) else {
        return;
    };
    for hw in [Hw::Off, Hw::ForceFail] {
        let p = Player::open_scrub_hw(&clip, hw);
        wait_frame(&p);
        assert_eq!(p.decode_status(), Some("sw"));
    }
    if cfg!(target_os = "macos") {
        let p = Player::open_scrub_hw(&clip, Hw::On);
        wait_frame(&p);
        assert_eq!(p.decode_status(), Some("hw"));
    }
}

#[test]
fn dropping_a_hw_player_does_not_block() {
    let _serial = WARM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmp("drop");
    let Some(clip) = make_clip(&dir, "640x360", 1) else {
        return;
    };
    let p = Player::open_scrub_hw(&clip, Hw::On);
    wait_frame(&p);
    let t = Instant::now();
    drop(p);
    assert!(
        t.elapsed() < Duration::from_millis(100),
        "drop blocked the caller"
    );
}

/// Process CPU time (all threads) in seconds.
#[cfg(unix)]
fn cpu_secs() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Process CPU time (all threads) in seconds. MSVC `clock()` ticks in milliseconds.
#[cfg(windows)]
fn cpu_secs() -> f64 {
    unsafe extern "C" {
        fn clock() -> std::os::raw::c_long;
    }
    unsafe { clock() as f64 / 1000.0 }
}

fn bench(clip: &Path, hw: Hw, label: &str) {
    let mut sc = Scrubber::open_with_hw(clip, false, hw).unwrap();
    let is_hw = sc.is_hw();
    let (c0, w0) = (cpu_secs(), Instant::now());
    let mut n = 0u32;
    while n < 60 && sc.next().is_some() {
        n += 1;
    }
    let (cpu, wall) = (cpu_secs() - c0, w0.elapsed().as_secs_f64());
    eprintln!(
        "bench {label} {}: {n} frames, cpu {:.2} ms/frame, wall {:.2} ms/frame",
        if is_hw { "hw" } else { "sw" },
        cpu * 1000.0 / n.max(1) as f64,
        wall * 1000.0 / n.max(1) as f64
    );
}

/// `cargo test -p vjc-media --release hw_benchmark -- --ignored --nocapture`
#[test]
#[ignore]
fn hw_benchmark() {
    let dir = tmp("bench");
    for (size, label) in [("1920x1080", "1080p"), ("3840x2160", "4K")] {
        let Some(clip) = make_clip(&dir, size, 3) else {
            return;
        };
        bench(&clip, Hw::Off, label);
        bench(&clip, Hw::On, label);
    }
}

#[test]
fn fallback_does_not_re_emit_earlier_frames() {
    let dir = tmp("reemit");
    let Some(clip) = make_clip(&dir, "640x360", 2) else {
        return;
    };
    let mut sc = Scrubber::open_with_hw(&clip, false, Hw::Off).unwrap();
    let mut last = f64::NEG_INFINITY;
    for _ in 0..15 {
        last = sc.next().expect("frame").0;
    }
    sc.fall_back_to_software("test").unwrap();
    for _ in 0..10 {
        let (t, _) = sc.next().expect("frame after fallback");
        assert!(t > last, "re-emitted pts {t} after {last}");
        last = t;
    }
}

#[test]
fn format_choice_prefers_hw_then_first_software_format() {
    use ff::ffi::AVPixelFormat as P;
    let other_hw = if hw::HW_FORMAT == P::AV_PIX_FMT_VAAPI {
        P::AV_PIX_FMT_CUDA
    } else {
        P::AV_PIX_FMT_VAAPI
    };
    let list = [
        other_hw,
        P::AV_PIX_FMT_YUV420P,
        P::AV_PIX_FMT_NV12,
        P::AV_PIX_FMT_NONE,
    ];
    let got = unsafe { hw::select_format(list.as_ptr(), hw::HW_FORMAT) };
    assert_eq!(
        got,
        P::AV_PIX_FMT_YUV420P,
        "must skip other hwaccel formats"
    );
    let list = [
        other_hw,
        P::AV_PIX_FMT_YUV420P,
        hw::HW_FORMAT,
        P::AV_PIX_FMT_NONE,
    ];
    assert_eq!(
        unsafe { hw::select_format(list.as_ptr(), hw::HW_FORMAT) },
        hw::HW_FORMAT
    );
    let only_hw = [other_hw, P::AV_PIX_FMT_NONE];
    assert_eq!(
        unsafe { hw::select_format(only_hw.as_ptr(), hw::HW_FORMAT) },
        P::AV_PIX_FMT_NONE
    );
}

#[test]
fn warm_gate_makes_waiters_wait_for_the_warm_up() {
    let g = Arc::new(hw::WarmGate::new());
    assert!(g.wait(Duration::from_millis(1)), "nothing in flight");
    g.start();
    assert!(
        !g.wait(Duration::from_millis(50)),
        "times out while running"
    );
    let g2 = g.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        g2.finish();
    });
    let t0 = Instant::now();
    assert!(g.wait(Duration::from_secs(5)));
    assert!(t0.elapsed() >= Duration::from_millis(80));
    t.join().unwrap();
}

/// Tests that touch the process-wide warm gate or open `Hw::On` players take this lock.
static WARM_TESTS: Mutex<()> = Mutex::new(());

/// Marks a warm-up as running and always finishes it again, even if the test panics.
struct WarmRunning;

impl WarmRunning {
    fn start() -> Self {
        hw::WARM.start();
        WarmRunning
    }
}

impl Drop for WarmRunning {
    fn drop(&mut self) {
        hw::WARM.finish();
    }
}

#[test]
fn open_scrub_waits_for_a_warm_up_in_flight() {
    let _serial = WARM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmp("waitwarm");
    let Some(clip) = make_clip(&dir, "320x180", 1) else {
        return;
    };
    let warm = WarmRunning::start();
    let p = Player::open_scrub_hw(&clip, Hw::On);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        p.latest_frame().is_none(),
        "must not start a second cold session"
    );
    drop(warm);
    wait_frame(&p);
    assert!(p.latest_frame().is_some());
}

#[test]
fn warm_guard_finishes_the_gate_when_the_test_panics() {
    let _serial = WARM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let r = std::panic::catch_unwind(|| {
        let _w = WarmRunning::start();
        panic!("boom");
    });
    assert!(r.is_err());
    assert!(hw::WARM.wait(Duration::from_millis(1)), "gate left running");
}

/// A raw Annex B H.264 stream: no container, so the frames carry no pts.
fn make_raw_h264(dir: &Path) -> Option<PathBuf> {
    let out = dir.join("raw.h264");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg("testsrc2=size=64x64:rate=25")
        .args([
            "-t", "2", "-c:v", "libx264", "-g", "10", "-bf", "0", "-pix_fmt", "yuv420p",
        ])
        .args(["-f", "h264"])
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipped: no ffmpeg CLI with libx264");
        return None;
    }
    Some(out)
}

#[test]
fn frames_without_pts_are_not_all_dropped_below_the_floor() {
    let dir = tmp("nopts");
    let Some(clip) = make_raw_h264(&dir) else {
        return;
    };
    let mut sc = Scrubber::open_with_hw(&clip, false, Hw::Off).unwrap();
    // The state right after a swap that had already emitted 5 frames' worth of time.
    sc.floor = 5.0 / sc.fps;
    let (t, _) = sc.next().expect("every frame was dropped below the floor");
    assert!(t > 5.0 / sc.fps, "t {t}");
}

#[test]
fn acquire_without_a_free_slot_leaves_the_decoder_alone() {
    static NONE_FREE: hw::HwSlots = hw::HwSlots::new();
    let dir = tmp("noslot");
    let Some(clip) = make_clip(&dir, "320x180", 1) else {
        return;
    };
    let mut sc = Scrubber::open_with_hw(&clip, false, Hw::Off).unwrap();
    sc.hw_mode = Hw::On;
    for _ in 0..5 {
        sc.next().expect("frame");
    }
    assert!(!sc.acquire_hw_from(&NONE_FREE, 0));
    assert!(sc.floor.is_nan(), "decoder was swapped without a slot");
    assert_eq!(NONE_FREE.active(), 0);
}

#[test]
fn failed_swap_keeps_the_old_decoder_and_its_slot() {
    static LOCAL: hw::HwSlots = hw::HwSlots::new();
    let dir = tmp("swapfail");
    let Some(clip) = make_clip(&dir, "320x180", 1) else {
        return;
    };
    let mut sc = Scrubber::open_with_hw(&clip, false, Hw::Off).unwrap();
    for _ in 0..5 {
        sc.next().expect("frame");
    }
    sc.slot = LOCAL.try_acquire(1);
    let r = sc.swap_decoder_with(None, |_, _, _| Err(ff::Error::DecoderNotFound));
    assert!(r.is_err());
    assert!(sc.slot.is_some(), "slot dropped by a failed swap");
    assert_eq!(LOCAL.active(), 1);
    assert!(sc.next().is_some(), "old decoder no longer works");
}
