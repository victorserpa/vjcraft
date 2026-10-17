//! End to end: the FFmpeg built by `scripts/build-ffmpeg.sh` writes Hap, Hap Alpha and
//! Hap Q; the native HAP path reads each file and must show the same picture as the
//! FFmpeg decoder. Skipped (with a message) when the binary was not built; set
//! `VJC_REQUIRE_HAP_FFMPEG=1` to make a missing binary a failure.

use super::*;
use std::process::Command;

const W: usize = 1920;
const H: usize = 1080;

fn built_ffmpeg() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let p = std::env::var_os("VJC_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/ffmpeg/bin/ffmpeg"));
    if p.is_file() {
        return Some(p);
    }
    assert!(
        std::env::var_os("VJC_REQUIRE_HAP_FFMPEG").is_none(),
        "built ffmpeg missing at {}",
        p.display()
    );
    eprintln!("skipped: no built ffmpeg at {}", p.display());
    None
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vjc-hapff-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Smooth 1080p RGBA picture (block compression keeps it close), alpha varies.
fn raw_frames(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(W * H * 4 * n);
    for t in 0..n {
        for y in 0..H {
            for x in 0..W {
                v.extend([
                    ((x * 255) / W) as u8,
                    ((y * 255) / H) as u8,
                    (((x + y) * 255) / (W + H)) as u8 / 2 + (t as u8) * 20,
                    (255 - (x * 200) / W) as u8,
                ]);
            }
        }
    }
    v
}

fn convert(bin: &Path, raw: &Path, out: &Path, format: &str) {
    let st = Command::new(bin)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-y"])
        .args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            "1920x1080",
            "-r",
            "25",
            "-i",
        ])
        .arg(raw)
        .args([
            "-c:v", "hap", "-format", format, "-chunks", "4", "-f", "mov",
        ])
        .arg(out)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg failed for {format}");
}

fn rgb565(c: u16) -> [i32; 3] {
    [
        i32::from((c >> 11) & 31) * 255 / 31,
        i32::from((c >> 5) & 63) * 255 / 63,
        i32::from(c & 31) * 255 / 31,
    ]
}

/// One BC1 colour block into 16 RGB texels. `four` forces the 4-colour mode (BC3).
fn bc1_block(b: &[u8], four: bool) -> [[i32; 3]; 16] {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let (a, z) = (rgb565(c0), rgb565(c1));
    let mix = |wa: i32, wb: i32, d: i32| [0, 1, 2].map(|i| (a[i] * wa + z[i] * wb) / d);
    let pal = if c0 > c1 || four {
        [a, z, mix(2, 1, 3), mix(1, 2, 3)]
    } else {
        [a, z, mix(1, 1, 2), [0, 0, 0]]
    };
    let idx = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    std::array::from_fn(|i| pal[((idx >> (2 * i)) & 3) as usize])
}

/// One BC4/BC3-alpha block into 16 values.
fn bc4_block(b: &[u8]) -> [i32; 16] {
    let (a0, a1) = (i32::from(b[0]), i32::from(b[1]));
    let pal: [i32; 8] = if a0 > a1 {
        std::array::from_fn(|i| match i {
            0 => a0,
            1 => a1,
            k => ((8 - k as i32) * a0 + (k as i32 - 1) * a1) / 7,
        })
    } else {
        std::array::from_fn(|i| match i {
            0 => a0,
            1 => a1,
            6 => 0,
            7 => 255,
            k => ((6 - k as i32) * a0 + (k as i32 - 1) * a1) / 5,
        })
    };
    let bits = b[2..8]
        .iter()
        .enumerate()
        .fold(0u64, |s, (i, &x)| s | u64::from(x) << (8 * i));
    std::array::from_fn(|i| pal[((bits >> (3 * i)) & 7) as usize])
}

/// CPU reference decode of a native HAP frame to RGBA (picture size).
fn decode_native(f: &hap::HapFrame) -> Vec<u8> {
    use hap::HapFormat::*;
    let (tw, th) = (f.tex_w as usize, f.tex_h as usize);
    let mut out = vec![255u8; f.width as usize * f.height as usize * 4];
    let bb = f.format.block_bytes();
    for by in 0..th / 4 {
        for bx in 0..tw / 4 {
            let blk = &f.color[(by * (tw / 4) + bx) * bb..][..bb];
            let alpha = if bb == 16 {
                Some(bc4_block(&blk[..8]))
            } else {
                None
            };
            let rgb = bc1_block(&blk[bb - 8..], bb == 16);
            for i in 0..16 {
                let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                if x >= f.width as usize || y >= f.height as usize {
                    continue;
                }
                let mut px = rgb[i].map(|c| c as f32);
                let mut a = alpha.map_or(255, |a| a[i]);
                if f.format == YcoCg {
                    // BC3 colour block carries Co, Cg, scale; the alpha block carries Y.
                    let (co, cg, s) = (
                        px[0] - 128.0,
                        px[1] - 128.0,
                        px[2] / 255.0 * 255.0 / 8.0 + 1.0,
                    );
                    let (co, cg, y) = (co / s, cg / s, a as f32);
                    px = [y + co - cg, y + cg, y - co - cg];
                    a = 255;
                }
                let o = (y * f.width as usize + x) * 4;
                for c in 0..3 {
                    out[o + c] = px[c].clamp(0.0, 255.0) as u8;
                }
                out[o + 3] = a as u8;
            }
        }
    }
    out
}

fn mean_abs_diff(a: &[u8], b: &[u8], channels: usize) -> f64 {
    assert_eq!(a.len(), b.len());
    let (mut s, mut n) = (0u64, 0u64);
    for (p, q) in a.chunks(4).zip(b.chunks(4)) {
        for c in 0..channels {
            s += u64::from(p[c].abs_diff(q[c]));
            n += 1;
        }
    }
    s as f64 / n as f64
}

#[test]
fn built_ffmpeg_writes_hap_variants_the_native_path_reads_like_ffmpeg() {
    let Some(bin) = built_ffmpeg() else { return };
    let d = tmp("variants");
    let raw = d.join("in.rgba");
    std::fs::write(&raw, raw_frames(3)).unwrap();
    let cases = [
        ("hap", hap::HapFormat::Dxt1, 3),
        ("hap_alpha", hap::HapFormat::Dxt5, 4),
        ("hap_q", hap::HapFormat::YcoCg, 3),
    ];
    for (name, fmt, channels) in cases {
        let out = d.join(format!("{name}.mov"));
        convert(&bin, &raw, &out, name);
        assert!(hap_probe(&out), "{name}: native path rejects the file");
        let mut sc = Scrubber::open_with(&out, true).unwrap();
        let mut n = 0;
        let mut first = None;
        while let Some((_, f)) = sc.next() {
            n += 1;
            first.get_or_insert(f);
        }
        assert_eq!(n, 3, "{name}: frame count");
        let hf = first.unwrap().hap.expect("block-compressed frame");
        assert_eq!(
            (hf.width, hf.height, hf.format),
            (W as u32, H as u32, fmt),
            "{name}"
        );
        let native = decode_native(&hf);
        let ff = decode_n(&out, 1).unwrap().remove(0);
        assert_eq!((ff.width, ff.height), (W as u32, H as u32));
        let diff = mean_abs_diff(&native, &ff.data, channels);
        eprintln!("{name}: mean diff {diff:.3}");
        assert!(diff < 2.5, "{name}: native vs FFmpeg mean diff {diff}");
    }
}
