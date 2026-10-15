//! Image sequences: a folder of numbered stills (`shot_0001.png`, `shot_0002.png`, ...)
//! played as one clip at a fixed frame rate.
//!
//! Rule for "is this folder a sequence": every visible file is a png/jpg/jpeg whose stem is
//! `prefix + digits`, all with the same prefix and extension, at least two of them, and
//! the digit runs either all have the same width or none has a leading zero. Anything else
//! (mixed names, other media, subfolders ignored) is not a sequence.

use std::path::{Path, PathBuf};

/// Default playback rate of a new sequence clip.
pub const DEFAULT_FPS: f32 = 30.0;

/// A scanned sequence: the pattern that names it and its frames in numeric order.
#[derive(Debug, Clone, PartialEq)]
pub struct Sequence {
    /// `dir/prefix%04d.ext` (`%d` when the numbers are not padded).
    pub pattern: PathBuf,
    pub frames: Vec<PathBuf>,
}

/// (prefix, number, digit count, lowercase extension) of a frame file name.
fn parse_name(name: &str) -> Option<(String, u64, usize, String)> {
    let (stem, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
        return None;
    }
    let digits = stem.chars().rev().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 18 {
        return None;
    }
    let (prefix, num) = stem.split_at(stem.len() - digits);
    Some((prefix.to_string(), num.parse().ok()?, digits, ext))
}

fn build(
    dir: &Path,
    mut items: Vec<(u64, usize, PathBuf)>,
    prefix: &str,
    ext: &str,
) -> Option<Sequence> {
    if items.len() < 2 {
        return None;
    }
    items.sort_by_key(|i| i.0);
    if items.windows(2).any(|w| w[0].0 == w[1].0) {
        return None;
    }
    let width = items[0].1;
    let same = items.iter().all(|i| i.1 == width);
    // Unpadded numbers (1, 2, 10) have varying widths but no leading zero.
    let unpadded = items.iter().all(|i| {
        i.2.file_name().is_some_and(|n| {
            let s = n.to_string_lossy();
            let stem = s.rsplit_once('.').map_or("", |p| p.0);
            let d = &stem[prefix.len()..];
            !d.starts_with('0') || d == "0"
        })
    });
    if !same && !unpadded {
        return None;
    }
    let lead0 = items.iter().any(|i| {
        let n =
            i.2.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
        n[prefix.len()..].starts_with('0')
    });
    let spec = if same && lead0 {
        format!("%0{width}d")
    } else {
        "%d".to_string()
    };
    Some(Sequence {
        pattern: dir.join(format!("{prefix}{spec}.{ext}")),
        frames: items.into_iter().map(|i| i.2).collect(),
    })
}

/// `Some` when `dir` is a pure numbered image sequence (see the module rule).
pub fn scan_dir(dir: &Path) -> Option<Sequence> {
    let mut key: Option<(String, String)> = None;
    let mut items = Vec::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !p.is_file() {
            continue;
        }
        let (prefix, num, w, ext) = parse_name(&name)?;
        match &key {
            None => key = Some((prefix, ext)),
            Some((pf, x)) if *pf == prefix && *x == ext => {}
            Some(_) => return None,
        }
        items.push((num, w, p));
    }
    let (prefix, ext) = key?;
    build(dir, items, &prefix, &ext)
}

/// Frames named by a `dir/prefix%04d.ext` / `%d` pattern, in numeric order (empty if none).
pub fn resolve_pattern(pattern: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (pattern.parent(), pattern.file_name()) else {
        return Vec::new();
    };
    let name = name.to_string_lossy();
    let Some((head, tail)) = name.split_once('%') else {
        return Vec::new();
    };
    let Some((spec, ext)) = tail.split_once('d') else {
        return Vec::new();
    };
    let width: usize = spec.trim_start_matches('0').parse().unwrap_or(0);
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    let mut out: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let (prefix, num, w, x) = parse_name(&n)?;
            (prefix == head && x == ext && (width == 0 || w == width)).then(|| (num, e.path()))
        })
        .collect();
    out.sort_by_key(|o| o.0);
    out.into_iter().map(|o| o.1).collect()
}

/// Marker the player path carries for a sequence: `pattern@fps`.
pub fn label(pattern: &Path, fps: f32) -> PathBuf {
    PathBuf::from(format!("{}@{}", pattern.display(), fps))
}

/// Inverse of `label`; `None` for any ordinary path.
pub fn parse_label(path: &Path) -> Option<(PathBuf, f32)> {
    let s = path.to_str()?;
    let (pat, fps) = s.rsplit_once('@')?;
    if !pat.contains('%') {
        return None;
    }
    let fps: f32 = fps.parse().ok()?;
    Some((
        PathBuf::from(pat),
        if fps.is_finite() && fps > 0.0 {
            fps.min(240.0)
        } else {
            DEFAULT_FPS
        },
    ))
}

/// Frame index shown at `pos` (0..1) of `n` frames.
pub fn frame_index(pos: f64, n: usize) -> usize {
    if n == 0 || !pos.is_finite() {
        return 0;
    }
    ((pos.clamp(0.0, 1.0) * n as f64) as usize).min(n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(names: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-seq-{}", uuid_like()));
        std::fs::create_dir_all(&d).unwrap();
        for n in names {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        d
    }

    fn uuid_like() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn pure_sequence_is_detected_in_numeric_order() {
        let d = dir_with(&["a_0010.png", "a_0002.png", "a_0001.PNG", ".DS_Store"]);
        // Mixed-case extension counts as the same extension.
        let s = scan_dir(&d).unwrap();
        let names: Vec<_> = s
            .frames
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a_0001.PNG", "a_0002.png", "a_0010.png"]);
        assert!(s.pattern.ends_with("a_%04d.png"));
        // The pattern finds the same frames again (lowercase ext only matches by name).
        assert_eq!(resolve_pattern(&s.pattern).len(), 3);
    }

    #[test]
    fn unpadded_numbers_sort_numerically() {
        let d = dir_with(&["f2.jpg", "f10.jpg", "f1.jpg"]);
        let s = scan_dir(&d).unwrap();
        assert!(s.pattern.ends_with("f%d.jpg"));
        assert_eq!(s.frames.len(), 3);
        assert!(s.frames[2].ends_with("f10.jpg"));
    }

    #[test]
    fn mixed_folders_are_not_sequences() {
        assert!(scan_dir(&dir_with(&["a1.png", "b2.png"])).is_none());
        assert!(scan_dir(&dir_with(&["a1.png", "a2.png", "clip.mp4"])).is_none());
        assert!(scan_dir(&dir_with(&["a1.png", "a2.jpg"])).is_none());
        // One frame is a still, not a sequence.
        assert!(scan_dir(&dir_with(&["a1.png"])).is_none());
        // Padded and unpadded widths mixed.
        assert!(scan_dir(&dir_with(&["a01.png", "a002.png"])).is_none());
    }

    #[test]
    fn player_plays_frames_in_order_at_fps_and_loops() {
        let d = dir_with(&[]);
        for (i, c) in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255]].iter().enumerate() {
            image::RgbaImage::from_pixel(2, 2, image::Rgba([c[0], c[1], c[2], 255]))
                .save(d.join(format!("f_{:03}.png", i + 1)))
                .unwrap();
        }
        let s = scan_dir(&d).unwrap();
        let mut p = crate::Player::open_scrub(label(&s.pattern, 6.0));
        let wait = |p: &crate::Player| {
            for _ in 0..500 {
                if p.duration().is_some() && p.latest_frame().is_some() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("sequence did not load");
        };
        wait(&p);
        // 3 frames at 6 fps = 0.5 s.
        assert!((p.duration().unwrap() - 0.5).abs() < 1e-9);
        let shows = |p: &crate::Player, pos: f64, want: [u8; 3]| {
            p.set_position(pos);
            for _ in 0..500 {
                let f = p.latest_frame().unwrap();
                if f.data[..3] == want {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("position {pos} never showed {want:?}");
        };
        shows(&p, 0.5, [0, 255, 0]);
        shows(&p, 0.99, [0, 0, 255]);
        // Loop: the playhead wraps to 0 and the first frame returns.
        shows(&p, 0.0, [255, 0, 0]);
        p.stop();
    }

    #[test]
    fn label_round_trips_and_ordinary_paths_do_not_parse() {
        let l = label(Path::new("/x/a_%04d.png"), 24.0);
        assert_eq!(
            parse_label(&l),
            Some((PathBuf::from("/x/a_%04d.png"), 24.0))
        );
        assert_eq!(parse_label(Path::new("/x/me@home.png")), None);
    }

    #[test]
    fn frame_index_covers_the_loop() {
        // 30 frames at 30 fps = 1 s; position steps through every frame, then wraps to 0.
        assert_eq!(frame_index(0.0, 30), 0);
        assert_eq!(frame_index(0.5, 30), 15);
        assert_eq!(frame_index(0.999, 30), 29);
        assert_eq!(frame_index(1.0, 30), 29);
        assert_eq!(frame_index(f64::NAN, 30), 0);
    }
}
