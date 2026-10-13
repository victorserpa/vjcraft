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
        _ => {
            let mut value = value;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("format_version".into(), Value::from(CURRENT_VERSION));
            }
            Ok(value)
        }
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

/// Clamps size, fps and tempo to what the UI allows (a hand-edited or damaged file could
/// hold zero or absurd values and crash texture allocation). Returns how many were changed.
fn clamp_composition(p: &mut Project) -> usize {
    use vjc_core::clock::{MAX_BPM, MIN_BPM};
    let c = &mut p.composition;
    let mut n = 0;
    for v in [&mut c.width, &mut c.height] {
        let w = (*v).clamp(1, 16384);
        n += usize::from(w != *v);
        *v = w;
    }
    let fps = if c.fps.is_finite() && c.fps > 0.0 {
        c.fps.clamp(1.0, 240.0)
    } else {
        30.0
    };
    n += usize::from(fps != c.fps);
    c.fps = fps;
    let bpm = if c.tempo.bpm.is_finite() {
        c.tempo.bpm.clamp(MIN_BPM, MAX_BPM)
    } else {
        120.0
    };
    n += usize::from(bpm != c.tempo.bpm);
    c.tempo.bpm = bpm;
    n
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
    let value = migrate(value, version)?;
    let mut project: Project = serde_json::from_value(value)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut warnings = vec![];
    let n = project.normalize_screens();
    if n > 0 {
        warnings.push(format!(
            "{n} screen(s) shared a monitor with another screen and were set to a window"
        ));
    }
    let n = project.normalize_groups();
    if n > 0 {
        warnings.push(format!(
            "{n} layer(s) had an invalid group and were ungrouped"
        ));
    }
    let n = clamp_composition(&mut project);
    if n > 0 {
        warnings.push(format!(
            "{n} composition value(s) (size, fps, tempo) were out of range and were clamped"
        ));
    }
    vjc_core::shortcuts::bind_legacy(&mut project);
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

    #[test]
    fn open_binds_legacy_positional_effect_shortcuts() {
        let d = tmp("legacyfx");
        let mut p = Project::new("T");
        let e = vjc_model::Effect {
            id: Default::default(),
            kind: "x".into(),
            bypass: false,
            dry_wet: 1.0,
            params: vec![],
        };
        p.composition.master.effects.push(e.clone());
        p.shortcuts.push(vjc_core::shortcuts::key_shortcut(
            "Q",
            "/comp/effect/1/drywet",
            vjc_model::ShortcutMode::Toggle,
            (0.0, 1.0),
        ));
        let f = d.join("p.vjc");
        save(&p, &f).unwrap();
        let (q, _) = open(&f).unwrap();
        assert_eq!(q.shortcuts[0].effect, Some(e.id));
    }

    #[test]
    fn open_normalizes_screens_sharing_a_monitor() {
        let d = tmp("dupmon");
        let mut p = Project::new("T");
        for n in ["A", "B"] {
            let mut s = vjc_core::mapping::new_screen(n, 100, 100);
            s.device = vjc_model::OutputDevice::Monitor { name: "1".into() };
            p.output.screens.push(s);
        }
        let f = d.join("p.vjc");
        save(&p, &f).unwrap();
        let (q, warns) = open(&f).unwrap();
        assert_eq!(q.output.screens[1].device, vjc_model::OutputDevice::Window);
        assert!(warns.iter().any(|w| w.contains("share")), "{warns:?}");
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
    fn stale_tmp_blocking_the_temp_name_keeps_the_old_file() {
        // A directory squats on the temp name: an atomic writer must fail without touching
        // the target. A direct `fs::write(path)` would succeed and overwrite it.
        let d = tmp("staletmp");
        let f = d.join("p.vjc");
        save(&Project::new("one"), &f).unwrap();
        std::fs::create_dir(d.join("p.vjc.tmp")).unwrap();
        assert!(save(&Project::new("two"), &f).is_err());
        assert_eq!(open(&f).unwrap().0.composition.name, "one");
    }

    #[test]
    fn write_atomic_replaces_content_and_leaves_no_tmp() {
        let d = tmp("replace");
        let f = d.join("x.json");
        write_atomic(&f, b"old").unwrap();
        write_atomic(&f, b"new").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"new");
        assert!(!d.join("x.json.tmp").exists());
    }

    #[test]
    fn truncated_project_is_an_error_not_a_panic() {
        let d = tmp("trunc");
        let f = d.join("p.vjc");
        save(&Project::new("one"), &f).unwrap();
        let raw = std::fs::read(&f).unwrap();
        std::fs::write(&f, &raw[..raw.len() / 2]).unwrap();
        assert!(matches!(open(&f), Err(Error::Json(_))));
        std::fs::write(&f, b"").unwrap();
        assert!(open(&f).is_err());
    }

    #[test]
    fn open_clamps_out_of_range_composition_values_with_a_warning() {
        let d = tmp("range");
        let f = d.join("p.vjc");
        let mut p = Project::new("r");
        p.composition.width = 0;
        p.composition.height = 4_000_000;
        p.composition.fps = 0.0;
        p.composition.tempo.bpm = 0.0;
        save(&p, &f).unwrap();
        let (back, w) = open(&f).unwrap();
        let c = &back.composition;
        assert_eq!((c.width, c.height), (1, 16384));
        assert!(c.fps > 0.0 && c.fps <= 240.0, "{}", c.fps);
        assert!(c.tempo.bpm >= vjc_core::clock::MIN_BPM, "{}", c.tempo.bpm);
        assert!(w.iter().any(|m| m.contains("out of range")), "{w:?}");
        // A sane project produces no such warning.
        save(&Project::new("ok"), &f).unwrap();
        assert!(open(&f).unwrap().1.is_empty());
    }

    #[test]
    fn migrate_versions() {
        let v = serde_json::json!({"format_version": 1});
        assert_eq!(migrate(v.clone(), 1).unwrap(), v);
        // Whatever the file said, the migrated value carries the current version.
        let old = serde_json::json!({"format_version": 1.0});
        assert_eq!(migrate(old, 1).unwrap()["format_version"], CURRENT_VERSION);
        let e = migrate(v, 99).unwrap_err().to_string();
        assert!(e.contains("newer"), "{e}");
    }
}
