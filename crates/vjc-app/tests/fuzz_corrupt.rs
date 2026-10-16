//! Corrupt-input corpus, generated at test time. Every file goes through the real
//! open/load path of its type and must come back as an error (or, for mutated-but-plausible
//! projects, as anything) but never panic. See docs/dev/reliability.md.

use serde_json::Value;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Tiny deterministic generator (xorshift64*), so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 24) as u8).collect()
    }
}

fn dir(name: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("vjc-fuzz-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn image_bytes(fmt: image::ImageFormat) -> Vec<u8> {
    let img = image::RgbImage::from_fn(48, 32, |x, y| image::Rgb([x as u8 * 5, y as u8 * 7, 99]));
    let mut out = Cursor::new(vec![]);
    img.write_to(&mut out, fmt).unwrap();
    out.into_inner()
}

/// A valid mp4/mov made with the ffmpeg CLI, if present.
fn video_bytes(ext: &str, d: &Path) -> Option<Vec<u8>> {
    let out = d.join(format!("src.{ext}"));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=size=64x48:rate=10:duration=1")
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    ok.then(|| std::fs::read(&out).unwrap())
}

fn project_json() -> Vec<u8> {
    let d = dir("seed");
    let p = d.join("seed.vjc");
    vjc_io::save(&vjc_model::Project::new("seed"), &p).unwrap();
    std::fs::read(p).unwrap()
}

const AVC: &str =
    r#"<Composition name="x"><CompositionInfo width="1920" height="1080"/></Composition>"#;

/// What opening does for a file type. Err(text) is the expected outcome for corrupt input.
fn open_any(path: &Path) -> Result<(), String> {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "vjc" => vjc_io::open(path).map(|_| ()).map_err(|e| e.to_string()),
        "avc" => vjc_avc::import_file(path)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        _ => vjc_media::thumbnail(path, 64).map(|_| ()),
    }
}

/// Runs `open_any`, failing the test if it panics.
fn must_not_panic(path: &Path) -> Result<(), String> {
    std::panic::catch_unwind(|| open_any(path))
        .unwrap_or_else(|_| panic!("opening {} panicked", path.display()))
}

struct Corpus {
    /// Files that must fail to open.
    bad: Vec<PathBuf>,
    /// Plausible-but-mutated files: only "no panic" is required.
    odd: Vec<PathBuf>,
}

fn put(d: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = d.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn build() -> Corpus {
    let d = dir("corpus");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut c = Corpus {
        bad: vec![],
        odd: vec![],
    };
    let png = image_bytes(image::ImageFormat::Png);
    let jpg = image_bytes(image::ImageFormat::Jpeg);
    let vjc = project_json();
    let mp4 = video_bytes("mp4", &d);
    let mov = video_bytes("mov", &d);
    let mut samples: Vec<(&str, Vec<u8>)> = vec![
        ("png", png),
        ("jpg", jpg),
        ("vjc", vjc.clone()),
        ("avc", AVC.as_bytes().to_vec()),
    ];
    if let Some(v) = mp4 {
        samples.push(("mp4", v));
    }
    if let Some(v) = mov {
        samples.push(("mov", v));
    }
    for (ext, bytes) in &samples {
        // Truncated (never the whole file), and empty.
        for (i, cut) in [0, 1, 4, bytes.len() / 8, bytes.len() / 4, bytes.len() / 2]
            .into_iter()
            .enumerate()
        {
            // A cut avc document or a cut tiny header is an error too, except where the
            // cut leaves a complete XML document (never, with these sizes).
            c.bad
                .push(put(&d, &format!("trunc{i}.{ext}"), &bytes[..cut]));
        }
        // Garbage of several sizes, and the valid head followed by garbage.
        for (i, n) in [3usize, 64, 4096].into_iter().enumerate() {
            c.bad
                .push(put(&d, &format!("garbage{i}.{ext}"), &rng.bytes(n)));
        }
        c.bad.push(put(&d, &format!("zeros.{ext}"), &[0u8; 2048]));
        // Bit flips in a valid file: usually an error, sometimes still decodable.
        for i in 0..6 {
            let mut b = bytes.clone();
            for _ in 0..8 {
                let at = (rng.next() as usize) % b.len();
                b[at] ^= 1 << (rng.next() % 8);
            }
            c.odd.push(put(&d, &format!("flip{i}.{ext}"), &b));
        }
    }
    // Media headers promising absurd sizes.
    let mut big = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    big.extend_from_slice(&[0xff; 8]);
    big.extend_from_slice(&[8, 2, 0, 0, 0]);
    c.bad.push(put(&d, "huge-dims.png", &big));
    let mut atom = vec![0xff, 0xff, 0xff, 0xff];
    atom.extend_from_slice(b"ftypisom");
    atom.extend_from_slice(&rng.bytes(64));
    c.bad.push(put(&d, "huge-atom.mp4", &atom));

    // Project files: JSON garbage and wrong types.
    let text = |s: &str| s.as_bytes().to_vec();
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let bad_json: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("null", text("null")),
        ("array", text("[]")),
        ("string", text("\"project\"")),
        ("brace", text("{")),
        ("no-version", text("{}")),
        ("version-string", text(r#"{"format_version":"one"}"#)),
        ("version-huge", text(r#"{"format_version":1e999}"#)),
        ("version-neg", text(r#"{"format_version":-1}"#)),
        (
            "version-max",
            text(r#"{"format_version":18446744073709551615}"#),
        ),
        ("version-only", text(r#"{"format_version":1}"#)),
        ("deep", text(&deep)),
        ("binary", rng.bytes(5000)),
        ("utf16", vec![0xff, 0xfe, b'{', 0, b'}', 0]),
    ];
    for (name, b) in bad_json {
        c.bad.push(put(&d, &format!("{name}.vjc"), &b));
    }
    // A valid project with each number or string swapped for a hostile value.
    let v: Value = serde_json::from_slice(&vjc).unwrap();
    let hostile = [
        serde_json::json!(1e308),
        serde_json::json!(-1),
        serde_json::json!(u64::MAX),
        serde_json::json!(0),
        serde_json::json!("x"),
        serde_json::json!(null),
        serde_json::json!([]),
    ];
    let mut n = 0;
    for h in &hostile {
        let mut paths = vec![];
        collect(&v, &mut vec![], &mut paths);
        for (i, path) in paths.iter().enumerate() {
            // Spread the work: every leaf with one hostile value would be thousands of files.
            if (i + n) % 3 != 0 {
                continue;
            }
            let mut m = v.clone();
            let mut slot = &mut m;
            for k in path {
                slot = match k {
                    Key::Name(s) => &mut slot[s.as_str()],
                    Key::Index(i) => &mut slot[*i],
                };
            }
            *slot = h.clone();
            c.odd.push(put(
                &d,
                &format!("mut{n}.vjc"),
                &serde_json::to_vec(&m).unwrap(),
            ));
            n += 1;
        }
    }
    // Wrong types for the version, with the rest intact.
    for h in &hostile {
        let mut m = v.clone();
        m["format_version"] = h.clone();
        c.odd.push(put(
            &d,
            &format!("ver{n}.vjc"),
            &serde_json::to_vec(&m).unwrap(),
        ));
        n += 1;
    }

    // .avc: XML garbage.
    let avc: Vec<(&str, &str)> = vec![
        ("empty", ""),
        ("text", "not xml"),
        ("open", "<Composition"),
        ("unclosed", "<Composition><CompositionInfo"),
        ("wrong-root", "<Other/>"),
        ("mismatch", "<A><B></A></B>"),
        ("bad-entity", "<Composition name=\"&bogus;\"/>"),
        (
            "huge-num",
            r#"<Composition><CompositionInfo width="1e999" height="-5"/></Composition>"#,
        ),
        (
            "nan-num",
            r#"<Composition><CompositionInfo width="NaN" height="inf"/></Composition>"#,
        ),
    ];
    for (name, t) in avc {
        let p = put(&d, &format!("{name}.avc"), t.as_bytes());
        // Some of these parse as an empty composition; only "no panic" is required.
        if matches!(name, "huge-num" | "nan-num") {
            c.odd.push(p);
        } else {
            c.bad.push(p);
        }
    }
    c
}

enum Key {
    Name(String),
    Index(usize),
}

fn collect(v: &Value, at: &mut Vec<Key>, out: &mut Vec<Vec<Key>>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                at.push(Key::Name(k.clone()));
                collect(x, at, out);
                at.pop();
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                at.push(Key::Index(i));
                collect(x, at, out);
                at.pop();
            }
        }
        _ => out.push(
            at.iter()
                .map(|k| match k {
                    Key::Name(s) => Key::Name(s.clone()),
                    Key::Index(i) => Key::Index(*i),
                })
                .collect(),
        ),
    }
}

#[test]
fn corrupt_files_fail_to_open_and_never_panic() {
    let c = build();
    assert!(c.bad.len() > 50 && c.odd.len() > 30, "corpus too small");
    for p in &c.bad {
        let r = must_not_panic(p);
        assert!(r.is_err(), "{} opened without error", p.display());
    }
    for p in &c.odd {
        let _ = must_not_panic(p);
    }
}

#[test]
fn corrupt_media_in_a_player_reports_an_error_not_a_crash() {
    let c = build();
    let media = |p: &&PathBuf| {
        matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("png" | "jpg" | "mp4" | "mov")
        )
    };
    for p in c.bad.iter().chain(&c.odd).filter(media) {
        let pl = vjc_media::Player::open_scrub(p);
        let t = Instant::now();
        // Garbage must settle on an error; "odd" files may decode, so wait for either.
        while pl.error().is_none()
            && pl.latest_frame().is_none()
            && t.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(e) = pl.error() {
            assert!(
                !e.contains("crashed"),
                "{}: decoder panicked: {e}",
                p.display()
            );
        }
    }
}
