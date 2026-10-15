//! Snapshot PNG: encodes an RGBA8 frame into a file in a user-chosen folder. Pure CPU and
//! GPU-free, so the encoder path tests headless.

use std::path::{Path, PathBuf};

/// File-name stem made safe: letters, digits, `-` and `_` stay, the rest becomes `_`.
fn safe_stem(stem: &str) -> String {
    let s: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.trim_matches('_').is_empty() {
        "snapshot".into()
    } else {
        s
    }
}

/// Saves `rgba` (`w * h * 4` bytes, straight alpha) as `<dir>/<stem>-<unix secs>[-n].png`
/// and returns the path. Never overwrites an existing file. Creates `dir` when missing.
pub fn save_png(dir: &Path, stem: &str, w: u32, h: u32, rgba: &[u8]) -> Result<PathBuf, String> {
    if w == 0 || h == 0 || rgba.len() != (w as usize) * (h as usize) * 4 {
        return Err("snapshot: frame size and data do not match".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("snapshot: {e}"))?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let base = format!("{}-{secs}", safe_stem(stem));
    let mut path = dir.join(format!("{base}.png"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{base}-{n}.png"));
        n += 1;
    }
    image::save_buffer(&path, rgba, w, h, image::ColorType::Rgba8)
        .map_err(|e| format!("snapshot: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_png_that_decodes_to_the_same_pixels() {
        let dir = std::env::temp_dir().join(format!("vjc-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let px: Vec<u8> = (0..2 * 3 * 4).map(|i| (i * 10) as u8).collect();
        let p = save_png(&dir, "my clip/1", 2, 3, &px).unwrap();
        assert_eq!(p.extension().unwrap(), "png");
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("my_clip_1-")
        );
        let img = image::open(&p).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (2, 3));
        assert_eq!(img.into_raw(), px);
        // A second snapshot in the same second gets its own file.
        let q = save_png(&dir, "my clip/1", 2, 3, &px).unwrap();
        assert_ne!(p, q);
        assert!(p.exists() && q.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_bad_sizes() {
        let dir = std::env::temp_dir().join("vjc-snap-bad");
        assert!(save_png(&dir, "x", 2, 2, &[0; 4]).is_err());
        assert!(save_png(&dir, "x", 0, 2, &[]).is_err());
    }
}
