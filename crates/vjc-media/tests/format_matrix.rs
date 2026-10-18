//! Opens every sample of the format matrix (`scripts/gen-format-matrix.sh <dir>`) and checks
//! probe, open, first frame, seek to the middle, play to the end and loop. Ignored unless
//! `VJC_FORMAT_DIR` is set, so CI stays fast. `VJC_FORMAT_HW=auto|off` limits the hardware
//! decode modes (default: both). `VJC_FORMAT_TIMEOUT` is the per-file budget in seconds (30).
//! Each sample has `<file>.expect` = "WxH alpha|opaque ok|error|any" (size after rotation).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use vjc_media::{Frame, Player, configure_hw_decode, probe};

struct Expect {
    w: u32,
    h: u32,
    alpha: bool,
    result: String,
}

fn parse_expect(p: &Path) -> Option<Expect> {
    let s = std::fs::read_to_string(p).ok()?;
    let mut it = s.split_whitespace();
    let (w, h) = it.next()?.split_once('x')?;
    Some(Expect {
        w: w.parse().ok()?,
        h: h.parse().ok()?,
        alpha: it.next()? == "alpha",
        result: it.next()?.to_string(),
    })
}

fn samples(dir: &Path) -> Vec<(String, PathBuf, Expect)> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "expect") {
            let media = p.with_extension("");
            if let Some(x) = parse_expect(&p) {
                let name = media.file_name().unwrap().to_string_lossy().into_owned();
                v.push((name, media, x));
            }
        }
    }
    // Image sequence: played through its label (pattern@fps).
    let seq = dir.join("seq");
    if seq.join("frame_0001.png").is_file() {
        let label = vjc_media::sequence::label(&seq.join("frame_%04d.png"), 25.0);
        v.push((
            "seq/frame_%04d.png".into(),
            label,
            Expect {
                w: 320,
                h: 180,
                alpha: false,
                result: "ok".into(),
            },
        ));
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn has_alpha(f: &Frame) -> bool {
    f.data.as_chunks::<4>().0.iter().any(|p| p[3] < 250)
}

/// Waits for a frame newer than `after`; Err on error text or timeout.
fn wait_frame(pl: &Player, after: u64, secs: u64) -> Result<Frame, String> {
    let t = Instant::now();
    loop {
        if let Some(f) = pl.latest_frame()
            && f.seq > after
        {
            return Ok(f);
        }
        if let Some(e) = pl.error() {
            return Err(format!("error: {e}"));
        }
        if t.elapsed() > Duration::from_secs(secs) {
            return Err("no frame in time".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn check(path: &Path, x: &Expect) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();
    // probe must not panic or hang; for good files it must agree on size and alpha.
    let info = probe(path);
    if x.result == "ok"
        && let Some(i) = &info
        && path.to_string_lossy().find('@').is_none()
    {
        if (i.width, i.height) != (x.w, x.h) {
            problems.push(format!(
                "probe size {}x{} want {}x{}",
                i.width, i.height, x.w, x.h
            ));
        }
        if i.alpha != x.alpha {
            problems.push(format!("probe alpha {} want {}", i.alpha, x.alpha));
        }
    }
    let mut pl = Player::open_scrub(path);
    let first = match wait_frame(&pl, 0, 10) {
        Ok(f) => f,
        Err(e) => {
            pl.stop();
            if x.result == "error" {
                eprintln!("    message for {}: {e}", path.display());
            }
            return match x.result.as_str() {
                "error" if e.starts_with("error:") && !e.contains("panic") => Ok(()),
                "any" if e.starts_with("error:") && !e.contains("panic") => Ok(()),
                _ => Err(format!("first frame: {e}")),
            };
        }
    };
    if x.result == "error" {
        pl.stop();
        return Err("opened a file that must be an error".into());
    }
    if x.result == "any" {
        // Damaged file that still opens: playing it must not panic or hang.
        for p in [0.5, 1.0, 0.0, 0.7] {
            pl.set_position(p);
            std::thread::sleep(Duration::from_millis(100));
        }
        let e = pl.error();
        pl.stop();
        return match e {
            Some(e) if e.contains("panic") => Err(e),
            _ => Ok(()),
        };
    }
    if (first.width, first.height) != (x.w, x.h) {
        problems.push(format!(
            "frame {}x{} want {}x{}",
            first.width, first.height, x.w, x.h
        ));
    }
    if x.alpha && !has_alpha(&first) {
        problems.push("alpha lost (all opaque)".into());
    }
    // Seek to the middle.
    pl.set_position(0.5);
    match wait_frame(&pl, first.seq, 5) {
        Ok(m) if m.data == first.data => problems.push("middle frame equals first".into()),
        Ok(_) => {}
        Err(e) => problems.push(format!("seek middle: {e}")),
    }
    // Play to the end, then loop back.
    let mut seq = pl.latest_frame().map_or(0, |f| f.seq);
    for i in 1..=10 {
        pl.set_position(f64::from(i) / 10.0);
        match wait_frame(&pl, seq, 5) {
            Ok(f) => seq = f.seq,
            Err(e) => {
                problems.push(format!("play at {}%: {e}", i * 10));
                break;
            }
        }
    }
    pl.set_position(0.0);
    match wait_frame(&pl, seq, 5) {
        Ok(f) if f.data != first.data => problems.push("loop: frame 0 differs".into()),
        Ok(_) => {}
        Err(e) => problems.push(format!("loop: {e}")),
    }
    if let Some(e) = pl.error() {
        problems.push(format!("error during play: {e}"));
    }
    pl.stop();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

#[test]
#[ignore = "needs VJC_FORMAT_DIR (scripts/gen-format-matrix.sh)"]
fn format_matrix() {
    let Some(dir) = std::env::var_os("VJC_FORMAT_DIR").map(PathBuf::from) else {
        eprintln!("VJC_FORMAT_DIR not set: skipped");
        return;
    };
    let secs: u64 = std::env::var("VJC_FORMAT_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let want = std::env::var("VJC_FORMAT_HW").unwrap_or_default();
    let modes: Vec<(&str, bool)> = [("auto", true), ("off", false)]
        .into_iter()
        .filter(|(n, _)| want.is_empty() || want == *n)
        .collect();
    let mut failed = 0;
    for (mode, on) in modes {
        configure_hw_decode(on, 8);
        for (name, path, x) in samples(&dir) {
            let (tx, rx) = mpsc::channel();
            let (p, t0) = (path.clone(), Instant::now());
            let xe = Expect {
                w: x.w,
                h: x.h,
                alpha: x.alpha,
                result: x.result.clone(),
            };
            std::thread::spawn(move || {
                let r = std::panic::catch_unwind(|| check(&p, &xe))
                    .unwrap_or_else(|_| Err("PANIC in test thread".into()));
                let _ = tx.send(r);
            });
            let r = rx
                .recv_timeout(Duration::from_secs(secs))
                .unwrap_or_else(|_| Err(format!("TIMEOUT after {secs}s")));
            let ms = t0.elapsed().as_millis();
            match r {
                Ok(()) => eprintln!("[{mode}] PASS {name} ({ms} ms)"),
                Err(e) => {
                    failed += 1;
                    eprintln!("[{mode}] FAIL {name} ({ms} ms): {e}");
                }
            }
        }
    }
    assert_eq!(failed, 0, "{failed} sample(s) failed");
}

/// Plays `file` on `n` players in real time for `secs`, layers offset by a quarter of the
/// clip. Returns delivered fps per layer, process CPU (% of one core), decode status of
/// layer 0 and the clip duration.
#[cfg(unix)]
fn run_layers(file: &Path, n: usize, secs: u64) -> (Vec<f64>, f64, Option<&'static str>, f64) {
    let cpu = || {
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
        let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
        t(ru.ru_utime) + t(ru.ru_stime)
    };
    let mut players: Vec<Player> = (0..n).map(|_| Player::open_scrub(file)).collect();
    for p in &players {
        p.set_sequential(true);
    }
    let t0 = Instant::now();
    while players.iter().any(|p| p.duration().is_none()) {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "players did not open: {:?}",
            players[0].error()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let dur = players[0].duration().unwrap();
    let (c0, w0) = (cpu(), Instant::now());
    let mut seen = vec![0u64; n];
    let mut last = vec![0u64; n];
    while w0.elapsed() < Duration::from_secs(secs) {
        let pos = (w0.elapsed().as_secs_f64() % dur) / dur;
        for (i, p) in players.iter().enumerate() {
            p.set_position((pos + i as f64 * 0.25) % 1.0);
            if let Some(f) = p.latest_frame()
                && f.seq != last[i]
            {
                last[i] = f.seq;
                seen[i] += 1;
            }
            assert!(p.error().is_none(), "{:?}", p.error());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let wall = w0.elapsed().as_secs_f64();
    let pct = (cpu() - c0) / wall * 100.0;
    let status = players[0].decode_status();
    for p in &mut players {
        p.stop();
    }
    (
        seen.iter()
            .map(|&k| (k as f64 / wall * 10.0).round() / 10.0)
            .collect(),
        pct,
        status,
        dur,
    )
}

/// Real user files (`VJC_REAL_FILES`, ';' separated, read in place): duration agrees with
/// the container, first frame, seek/loop latency, decode path, delivered fps with 1 and
/// `VJC_REAL_LAYERS` (4) layers. `VJC_REAL_SECS` per run (default 20).
#[test]
#[ignore = "needs VJC_REAL_FILES"]
#[cfg(unix)]
fn real_files() {
    let Some(list) = std::env::var("VJC_REAL_FILES").ok() else {
        return;
    };
    let secs: u64 = std::env::var("VJC_REAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let layers: usize = std::env::var("VJC_REAL_LAYERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    for (mode, on) in [("auto", true), ("off", false)] {
        if !std::env::var("VJC_FORMAT_HW")
            .unwrap_or_default()
            .is_empty()
            && std::env::var("VJC_FORMAT_HW").unwrap() != mode
        {
            continue;
        }
        configure_hw_decode(on, 8);
        for f in list.split(';').filter(|f| !f.is_empty()) {
            let path = PathBuf::from(f);
            let info = probe(&path);
            let t = Instant::now();
            let mut pl = Player::open_scrub(&path);
            let first = wait_frame(&pl, 0, 30);
            let t_first = t.elapsed().as_millis();
            let dur = pl.duration();
            let mut lat = Vec::new();
            for p in [0.5, 0.99, 0.0, 0.25] {
                let seq = pl.latest_frame().map_or(0, |f| f.seq);
                let t = Instant::now();
                pl.set_position(p);
                let r = wait_frame(&pl, seq, 10);
                lat.push(format!(
                    "{p}->{}",
                    r.map_or_else(|e| e, |_| format!("{} ms", t.elapsed().as_millis()))
                ));
            }
            eprintln!(
                "REAL [{mode}] {f}\n   probe {:?}\n   first frame {} ms ({}), player duration {:?}, decode {:?}\n   seek {}",
                info,
                t_first,
                first.map_or_else(|e| e, |f| format!("{}x{}", f.width, f.height)),
                dur,
                pl.decode_status(),
                lat.join(", ")
            );
            pl.stop();
            for n in [1, layers] {
                let (fps, cpu, st, d) = run_layers(&path, n, secs);
                eprintln!(
                    "   {n} layer(s) {secs}s: fps {fps:?}, CPU {cpu:.0}% of a core, decode {st:?}, duration {d:.2}"
                );
            }
        }
    }
}

/// Load test: `VJC_HEAVY_FILE` played by 4 players at once in real time for
/// `VJC_HEAVY_SECS` (default 120). Prints fps per player and process CPU.
#[test]
#[ignore = "needs VJC_HEAVY_FILE"]
#[cfg(unix)]
fn four_layers_heavy() {
    let Some(file) = std::env::var_os("VJC_HEAVY_FILE").map(PathBuf::from) else {
        return;
    };
    let secs: u64 = std::env::var("VJC_HEAVY_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    configure_hw_decode(
        std::env::var("VJC_FORMAT_HW").unwrap_or_default() != "off",
        8,
    );
    let (fps, cpu, st, _) = run_layers(&file, 4, secs);
    eprintln!(
        "HEAVY {}: {secs} s, fps per layer {fps:?}, process CPU {cpu:.0}% of one core, decode {st:?}",
        file.display()
    );
    assert!(fps.iter().all(|&f| f > 0.0), "a layer got no frames");
}
