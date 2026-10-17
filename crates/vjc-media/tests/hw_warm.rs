//! Own process on purpose: the cold VideoToolbox cost must not be paid by another test first.
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

#[test]
fn warm_up_pays_the_first_hardware_session_cost() {
    let dir = std::env::temp_dir().join(format!("vjc-warm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("w.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=size=640x360:rate=10:duration=2")
        .args(["-c:v", "h264_videotoolbox", "-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skip: cannot encode");
        return;
    }
    vjc_media::configure_hw_decode(true, 8);
    let h = vjc_media::warm_up_hw_decode().expect("warm-up thread starts");
    h.join().unwrap();
    let t = Instant::now();
    let mut pl = vjc_media::Player::open_scrub(&clip);
    while pl.latest_frame().is_none() {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "no frame: {:?}",
            pl.error()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let took = t.elapsed();
    eprintln!("first hw frame after warm-up: {took:?}");
    assert_eq!(pl.decode_status(), Some("hw"));
    assert!(took < Duration::from_millis(500), "{took:?}");
    pl.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
