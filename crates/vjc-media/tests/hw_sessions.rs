//! Hardware session policy (own process: it sets the global session cap and needs a quiet
//! device). An idle clip keeps its session; one is reclaimed only on demand.
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vjc_media::Player;

static SERIAL: Mutex<()> = Mutex::new(());

fn clip(name: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("vjc-sess-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(format!("{name}.mp4"));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=1280x720:rate=30:duration=4")
        .args(["-c:v", "libx264", "-g", "30", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skip: cannot encode");
        return None;
    }
    Some(out)
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(secs), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn open_with_frame(path: &PathBuf) -> Player {
    let p = Player::open_scrub(path);
    wait_until("first frame", 10, || p.latest_frame().is_some());
    p
}

/// Time from `set_position(pos)` to the next published frame.
fn time_to_frame(p: &Player, pos: f64) -> Duration {
    let before = p.latest_frame().map_or(0, |f| f.seq);
    let t = Instant::now();
    p.set_position(pos);
    wait_until("frame after set_position", 10, || {
        p.latest_frame().is_some_and(|f| f.seq > before)
    });
    t.elapsed()
}

fn setup(cap: usize) {
    vjc_media::configure_hw_decode(true, cap);
    if let Some(h) = vjc_media::warm_up_hw_decode() {
        h.join().unwrap();
    }
    wait_until("no sessions left from a previous test", 10, || {
        vjc_media::active_sessions() == 0
    });
}

#[test]
fn idle_clip_keeps_its_session_and_first_frame_is_not_late() {
    let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = clip("lat") else { return };
    setup(8);
    let fresh = open_with_frame(&path);
    assert_eq!(fresh.decode_status(), Some("hw"));
    // Never idle: moves right after opening.
    let t_fresh = time_to_frame(&fresh, 0.5);

    let idle = open_with_frame(&path);
    assert_eq!(idle.decode_status(), Some("hw"));
    std::thread::sleep(Duration::from_millis(2500)); // well past the idle threshold
    assert_eq!(
        idle.decode_status(),
        Some("hw"),
        "idle clip lost its session"
    );
    assert_eq!(vjc_media::active_sessions(), 2);
    let t_idle = time_to_frame(&idle, 0.5);
    assert_eq!(idle.decode_status(), Some("hw"));
    eprintln!("time to first frame: never-idle {t_fresh:?}, idle>2.5s {t_idle:?}");
    // The session checks above are the real guard. Wall-clock timing is noisy on a
    // loaded machine, so the latency bound only catches a gross regression.
    assert!(
        t_idle <= t_fresh * 2 + Duration::from_millis(250),
        "idle {t_idle:?} vs never-idle {t_fresh:?}"
    );
}

#[test]
fn on_air_clip_reclaims_the_session_of_the_idle_holder() {
    let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = clip("evict") else { return };
    setup(1);
    let a = open_with_frame(&path);
    assert_eq!(a.decode_status(), Some("hw"));
    std::thread::sleep(Duration::from_millis(1500)); // A is idle
    assert_eq!(a.decode_status(), Some("hw"), "free of demand, A keeps it");
    let b = open_with_frame(&path);
    assert_eq!(b.decode_status(), Some("sw"), "cap 1 is taken by A");
    b.set_position(0.3);
    wait_until("B gets hw", 5, || b.decode_status() == Some("hw"));
    wait_until("A goes sw", 5, || a.decode_status() == Some("sw"));
    assert_eq!(vjc_media::active_sessions(), 1);
}
