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
    f.data.chunks_exact(4).any(|p| p[3] < 250)
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
