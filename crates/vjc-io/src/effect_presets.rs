//! Effect presets: the params of one effect instance saved under a name, per effect kind,
//! and loadable onto any instance of that kind. Files are JSON in `<root>/<kind>/`, written
//! atomically. What is read back is size-checked; the undoable load (`SetEffectParams`)
//! clamps and type-checks it against the target effect.

use crate::output_presets::{clean_name, stem};
use crate::write_atomic;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vjc_model::Param;

pub const EXT: &str = "vjcfx";
pub const FORMAT_VERSION: u32 = 1;
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PARAMS: usize = 256;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    BadName,
    NotFound,
    TooBig,
    FutureVersion(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Json(e) => write!(f, "not a valid effect preset: {e}"),
            Error::BadName => write!(f, "invalid preset name"),
            Error::NotFound => write!(f, "preset not found"),
            Error::TooBig => write!(f, "preset file is too large"),
            Error::FutureVersion(v) => write!(f, "preset written by a newer VJCraft (format {v})"),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectPreset {
    pub format_version: u32,
    pub kind: String,
    pub name: String,
    pub params: Vec<Param>,
}

fn kind_dir(root: &Path, kind: &str) -> Result<PathBuf, Error> {
    let s = stem(kind);
    if kind.is_empty() || s.is_empty() {
        return Err(Error::BadName);
    }
    Ok(root.join(s))
}

fn path_of(root: &Path, kind: &str, name: &str) -> Result<PathBuf, Error> {
    let n = clean_name(name).ok_or(Error::BadName)?;
    let s = stem(&n);
    if s.is_empty() {
        return Err(Error::BadName);
    }
    Ok(kind_dir(root, kind)?.join(format!("{s}.{EXT}")))
}

fn read(path: &Path) -> Result<EffectPreset, Error> {
    if std::fs::metadata(path)?.len() > MAX_BYTES {
        return Err(Error::TooBig);
    }
    let mut p: EffectPreset = serde_json::from_slice(&std::fs::read(path)?)?;
    if p.format_version > FORMAT_VERSION {
        return Err(Error::FutureVersion(p.format_version));
    }
    p.name = clean_name(&p.name).ok_or(Error::BadName)?;
    p.params.truncate(MAX_PARAMS);
    Ok(p)
}

/// Saves `params` as preset `name` of effect `kind` (replacing one of the same name).
pub fn save(root: &Path, kind: &str, name: &str, params: &[Param]) -> Result<PathBuf, Error> {
    let path = path_of(root, kind, name)?;
    let preset = EffectPreset {
        format_version: FORMAT_VERSION,
        kind: kind.to_owned(),
        name: clean_name(name).ok_or(Error::BadName)?,
        params: params.to_vec(),
    };
    std::fs::create_dir_all(path.parent().ok_or(Error::BadName)?)?;
    write_atomic(&path, &serde_json::to_vec_pretty(&preset)?)?;
    Ok(path)
}

pub fn load(root: &Path, kind: &str, name: &str) -> Result<EffectPreset, Error> {
    let path = path_of(root, kind, name)?;
    if !path.exists() {
        return Err(Error::NotFound);
    }
    let p = read(&path)?;
    if p.kind != kind {
        return Err(Error::NotFound);
    }
    Ok(p)
}

/// Preset names of `kind`, sorted; unreadable files are skipped.
pub fn list(root: &Path, kind: &str) -> Vec<String> {
    let Ok(dir) = kind_dir(root, kind) else {
        return vec![];
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut names: Vec<String> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == EXT))
        .filter_map(|p| read(&p).ok())
        .filter(|p| p.kind == kind)
        .map(|p| p.name)
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

pub fn delete(root: &Path, kind: &str, name: &str) -> Result<(), Error> {
    let path = path_of(root, kind, name)?;
    if !path.exists() {
        return Err(Error::NotFound);
    }
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-fxp-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_list_load_delete_per_kind() {
        let root = tmp("a");
        let ps = vec![Param::number("amount", 0.3, Some((0.0, 1.0)))];
        save(&root, "blur", "Soft", &ps).unwrap();
        save(&root, "blur", "alpha", &ps).unwrap();
        save(&root, "glow", "Soft", &ps).unwrap();
        assert_eq!(list(&root, "blur"), vec!["alpha", "Soft"]);
        assert_eq!(list(&root, "glow"), vec!["Soft"]);
        assert!(list(&root, "none").is_empty());
        assert_eq!(load(&root, "blur", "Soft").unwrap().params, ps);
        delete(&root, "blur", "Soft").unwrap();
        assert!(matches!(load(&root, "blur", "Soft"), Err(Error::NotFound)));
        assert_eq!(list(&root, "glow"), vec!["Soft"]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn names_cannot_escape_and_save_leaves_no_temp() {
        let root = tmp("b");
        assert!(save(&root, "blur", "  ", &[]).is_err());
        assert!(save(&root, "", "x", &[]).is_err());
        let p = save(&root, "../evil", "../x", &[]).unwrap();
        assert!(p.starts_with(&root), "{p:?}");
        let names: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn corrupt_and_future_files_are_skipped_or_refused() {
        let root = tmp("c");
        save(&root, "blur", "ok", &[]).unwrap();
        let dir = root.join("blur");
        std::fs::write(dir.join("bad.vjcfx"), b"{nope").unwrap();
        assert_eq!(list(&root, "blur"), vec!["ok"]);
        std::fs::write(
            dir.join("new.vjcfx"),
            br#"{"format_version":99,"kind":"blur","name":"new","params":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            load(&root, "blur", "new"),
            Err(Error::FutureVersion(99))
        ));
        std::fs::remove_dir_all(root).ok();
    }
}
