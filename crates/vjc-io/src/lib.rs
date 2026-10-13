//! Save/open of `.vjc` project files (JSON, `format_version`), with relative
//! media paths, missing-media warnings, migration and atomic writes.

pub mod prefs;

use serde_json::Value;
use std::path::{Path, PathBuf};
use vjc_model::{Project, Source};

pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// File written by a newer VJCraft.
    FutureVersion {
        found: u32,
        supported: u32,
    },
    BadVersion,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Json(e) => write!(f, "invalid project file: {e}"),
            Error::FutureVersion { found, supported } => write!(
                f,
                "project format v{found} is newer than this VJCraft supports (v{supported}); update the app"
            ),
            Error::BadVersion => write!(f, "project file has no valid format_version"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

/// Upgrades raw project JSON from `from_version` to [`CURRENT_VERSION`].
pub fn migrate(value: Value, from_version: u32) -> Result<Value, Error> {
    match from_version {
        0 => Err(Error::BadVersion),
        v if v > CURRENT_VERSION => Err(Error::FutureVersion {
            found: v,
            supported: CURRENT_VERSION,
        }),
        // v1 is current; future steps chain here (1 -> 2 ...).
        _ => Ok(value),
    }
}

fn media_paths(p: &mut Project) -> impl Iterator<Item = &mut PathBuf> {
    p.composition
        .decks
        .iter_mut()
        .flat_map(|d| d.clips.iter_mut())
        .filter_map(|c| match &mut c.clip.source {
            Source::File { path } => Some(path),
            _ => None,
        })
}

/// Writes `bytes` to a temp file next to `path`, then renames over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        sync_parent(path);
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Best effort: flushes the directory entry so the rename survives a power cut.
/// Directories cannot be opened for sync on Windows, where this does nothing.
fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Saves `project` to `path`. Media inside the project folder become relative.
pub fn save(project: &Project, path: &Path) -> Result<(), Error> {
    let mut p = project.clone();
    p.format_version = CURRENT_VERSION;
    if let Some(dir) = path.parent() {
        for m in media_paths(&mut p) {
            if let Ok(rel) = m.strip_prefix(dir) {
                *m = rel.to_path_buf();
            }
        }
    }
    let json = serde_json::to_vec_pretty(&p)?;
    write_atomic(path, &json)?;
    Ok(())
}

fn find_by_name(dir: &Path, name: &std::ffi::OsStr, depth: u32) -> Option<PathBuf> {
    let mut subdirs = vec![];
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.file_name() == Some(name) {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs
        .into_iter()
        .find_map(|d| find_by_name(&d, name, depth - 1))
}

/// `format_version` as an integer; accepts `1.0` (some writers emit floats), not `1.5`.
fn version_number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        let f = v.as_f64()?;
        (f >= 0.0 && f.fract() == 0.0 && f <= u32::MAX as f64).then_some(f as u64)
    })
}

/// Opens a project. Media are resolved as: relative to the project folder,
/// then as stored (absolute), then by file name inside the project folder.
/// Returns the project and warnings for media that could not be found.
pub fn open(path: &Path) -> Result<(Project, Vec<String>), Error> {
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let version = value
        .get("format_version")
        .and_then(version_number)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or(Error::BadVersion)?;
    let mut value = migrate(value, version)?;
    value["format_version"] = Value::from(version);
    let mut project: Project = serde_json::from_value(value)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut warnings = vec![];
    for m in media_paths(&mut project) {
        let joined = dir.join(&*m);
        if joined.exists() {
            *m = joined;
        } else if m.is_absolute() && m.exists() {
            // keep as is
        } else if let Some(found) = m.file_name().and_then(|n| find_by_name(dir, n, 3)) {
            warnings.push(format!(
                "Relinked {} to {} by file name",
                m.display(),
                found.display()
            ));
            *m = found;
        } else {
            warnings.push(format!("Missing media: {}", m.display()));
            *m = joined;
        }
    }
    Ok((project, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_model::{CellClip, Clip};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-io-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn with_media(path: PathBuf) -> Project {
        let mut p = Project::new("T");
        let layer = uuid_layer();
        let mut clip = vjc_core::clip_from_path(path);
        clip.name = "c".into();
        let _: &Clip = &clip;
        p.composition.decks[0].clips.push(CellClip {
            layer,
            column: 0,
            clip,
        });
        p
    }

    fn uuid_layer() -> vjc_model::LayerId {
        vjc_model::LayerId::nil()
    }

    fn first_path(p: &Project) -> PathBuf {
        match &p.composition.decks[0].clips[0].clip.source {
            Source::File { path } => path.clone(),
            _ => panic!(),
        }
    }

    #[test]
    fn roundtrip_and_relative() {
        let d = tmp("rt");
        let media = d.join("a.mov");
        std::fs::write(&media, b"x").unwrap();
        let proj = with_media(media);
        let f = d.join("p.vjc");
        save(&proj, &f).unwrap();
        let raw = std::fs::read_to_string(&f).unwrap();
        assert!(raw.contains("\"a.mov\"") && !raw.contains(d.to_str().unwrap()));
        let (back, w) = open(&f).unwrap();
        assert!(w.is_empty());
        assert_eq!(back, proj);
    }

    #[test]
    fn outside_stays_absolute() {
        let d = tmp("abs");
        let o = tmp("abs-other");
        let media = o.join("b.mov");
        std::fs::write(&media, b"x").unwrap();
        let f = d.join("p.vjc");
        save(&with_media(media.clone()), &f).unwrap();
        let (back, w) = open(&f).unwrap();
        assert!(w.is_empty());
        assert_eq!(first_path(&back), media);
    }

    #[test]
    fn missing_warns_and_name_search_finds() {
        let d = tmp("miss");
        let f = d.join("p.vjc");
        save(&with_media(PathBuf::from("/nope/zz.mov")), &f).unwrap();
        let (_, w) = open(&f).unwrap();
        assert_eq!(w.len(), 1);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/zz.mov"), b"x").unwrap();
        let (back, w) = open(&f).unwrap();
        // Relinking by name is reported so a wrong same-named file is visible.
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("Relinked"));
        assert_eq!(first_path(&back), d.join("sub/zz.mov"));
    }

    #[test]
    fn atomic_write_leaves_no_partial() {
        let d = tmp("atomic");
        let f = d.join("p.vjc");
        save(&Project::new("one"), &f).unwrap();
        // Force failure: the target is a directory, so the rename fails.
        let bad = d.join("dir.vjc");
        std::fs::create_dir(&bad).unwrap();
        assert!(save(&Project::new("two"), &bad).is_err());
        assert!(!d.join("dir.vjc.tmp").exists());
        assert!(!d.join("p.vjc.tmp").exists());
        assert_eq!(open(&f).unwrap().0.composition.name, "one");
    }

    #[test]
    fn float_format_version_is_accepted() {
        let d = tmp("float");
        let f = d.join("p.vjc");
        save(&Project::new("f"), &f).unwrap();
        let raw = std::fs::read_to_string(&f).unwrap();
        let patched = raw.replacen("\"format_version\": 1", "\"format_version\": 1.0", 1);
        assert_ne!(raw, patched);
        std::fs::write(&f, patched).unwrap();
        assert_eq!(open(&f).unwrap().0.composition.name, "f");
        let frac = raw.replacen("\"format_version\": 1", "\"format_version\": 1.5", 1);
        std::fs::write(&f, frac).unwrap();
        assert!(matches!(open(&f), Err(Error::BadVersion)));
    }

    #[test]
    fn project_without_groups_opens_and_groups_roundtrip() {
        let d = tmp("groups");
        let f = d.join("old.vjc");
        // A project written before groups existed has no "groups" key.
        let mut v = serde_json::to_value(Project::new("old")).unwrap();
        assert!(
            v["composition"]
                .as_object_mut()
                .unwrap()
                .remove("groups")
                .is_some()
        );
        std::fs::write(&f, serde_json::to_vec(&v).unwrap()).unwrap();
        let (p, w) = open(&f).unwrap();
        assert!(w.is_empty() && p.composition.groups.is_empty());
        let mut p = p;
        let mut g = vjc_model::LayerGroup::new("G");
        g.speed = 2.0;
        p.composition.groups.push(g);
        let f2 = d.join("new.vjc");
        save(&p, &f2).unwrap();
        assert_eq!(open(&f2).unwrap().0, p);
    }

    #[test]
    fn migrate_versions() {
        let v = serde_json::json!({"format_version": 1});
        assert_eq!(migrate(v.clone(), 1).unwrap(), v);
        let e = migrate(v, 99).unwrap_err().to_string();
        assert!(e.contains("newer"), "{e}");
    }
}
