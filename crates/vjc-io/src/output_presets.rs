//! Output presets: a whole output setup (screens, slices, masks, soft edge, routing) saved
//! as one file in a user folder, apart from any project, so the same LED wall layout can
//! be loaded per venue. Files are JSON, written atomically; anything read from disk is
//! checked and clamped before it can reach the project.

use crate::write_atomic;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use vjc_core::mapping;
use vjc_model::{OutputDevice, OutputSetup, Screen, SliceSource};

/// File extension of a preset.
pub const EXT: &str = "vjcout";
pub const FORMAT_VERSION: u32 = 1;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_NAME: usize = 80;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// Empty, too long or containing control characters.
    BadName,
    NotFound,
    /// A preset with that name already exists.
    Exists,
    TooBig,
    /// Written by a newer VJCraft.
    FutureVersion(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Json(e) => write!(f, "not a valid output preset: {e}"),
            Error::BadName => write!(f, "invalid preset name"),
            Error::NotFound => write!(f, "preset not found"),
            Error::Exists => write!(f, "a preset with that name already exists"),
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
pub struct OutputPreset {
    pub format_version: u32,
    pub name: String,
    pub setup: OutputSetup,
}

/// What the current machine and project offer, used to repair a preset on load.
#[derive(Debug, Clone, Copy)]
pub struct Env<'a> {
    /// Ids of the connected monitors, as stored in `OutputDevice::Monitor`.
    pub monitors: &'a [String],
    /// Layers and groups of the open project (a routed slice needs them).
    pub layers: &'a [Uuid],
    pub groups: &'a [Uuid],
}

/// Trimmed name, or `None` when it cannot name a preset.
pub fn clean_name(name: &str) -> Option<String> {
    let n = name.trim();
    (!n.is_empty() && n.chars().count() <= MAX_NAME && !n.chars().any(char::is_control))
        .then(|| n.to_string())
}

/// File stem of a preset name: letters, digits, space, `-`, `_` and `.` survive; anything
/// else (path separators included) becomes `_`; never starts with a dot.
pub(crate) fn stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.trim_start_matches('.').to_string()
}

/// Path of the preset called `name` in `dir`.
pub fn path_of(dir: &Path, name: &str) -> Result<PathBuf, Error> {
    let n = clean_name(name).ok_or(Error::BadName)?;
    let s = stem(&n);
    if s.is_empty() {
        return Err(Error::BadName);
    }
    Ok(dir.join(format!("{s}.{EXT}")))
}

/// Names of the presets in `dir`, sorted; unreadable files are skipped.
pub fn list(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut names: Vec<String> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == EXT))
        .filter_map(|p| read(&p).ok())
        .map(|p| p.name)
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// Repairs a setup read from disk so it is safe to hand to the project.
fn clamp(setup: &mut OutputSetup) {
    for s in &mut setup.screens {
        s.width = s.width.clamp(1, 16384);
        s.height = s.height.clamp(1, 16384);
    }
    mapping::normalize_output(setup);
    mapping::sanitize_slice_effects(setup);
    for sl in setup.screens.iter_mut().flat_map(|s| &mut s.slices) {
        if sl.input.len() < 3
            || sl
                .input
                .iter()
                .any(|p| !p.0.is_finite() || !p.1.is_finite())
        {
            sl.input = mapping::full_input();
        }
    }
}

/// Reads and checks a preset file (also used for imports).
pub fn read(path: &Path) -> Result<OutputPreset, Error> {
    if std::fs::metadata(path)?.len() > MAX_BYTES {
        return Err(Error::TooBig);
    }
    let mut p: OutputPreset = serde_json::from_slice(&std::fs::read(path)?)?;
    if p.format_version > FORMAT_VERSION {
        return Err(Error::FutureVersion(p.format_version));
    }
    p.name = clean_name(&p.name).ok_or(Error::BadName)?;
    clamp(&mut p.setup);
    Ok(p)
}

fn write(path: &Path, preset: &OutputPreset) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(path, &serde_json::to_vec_pretty(preset)?)?;
    Ok(())
}

/// Saves `setup` as the preset `name` in `dir` (replacing a preset of the same name).
pub fn save(dir: &Path, name: &str, setup: &OutputSetup) -> Result<PathBuf, Error> {
    let path = path_of(dir, name)?;
    let preset = OutputPreset {
        format_version: FORMAT_VERSION,
        name: clean_name(name).ok_or(Error::BadName)?,
        setup: setup.clone(),
    };
    write(&path, &preset)?;
    Ok(path)
}

pub fn load(dir: &Path, name: &str) -> Result<OutputPreset, Error> {
    let path = path_of(dir, name)?;
    if !path.exists() {
        return Err(Error::NotFound);
    }
    read(&path)
}

pub fn delete(dir: &Path, name: &str) -> Result<(), Error> {
    let path = path_of(dir, name)?;
    if !path.exists() {
        return Err(Error::NotFound);
    }
    std::fs::remove_file(path)?;
    Ok(())
}

/// Renames a preset. Fails with `Exists` when `new` is taken by another preset.
pub fn rename(dir: &Path, old: &str, new: &str) -> Result<(), Error> {
    let (from, to) = (path_of(dir, old)?, path_of(dir, new)?);
    let mut preset = load(dir, old)?;
    if from != to && to.exists() {
        return Err(Error::Exists);
    }
    preset.name = clean_name(new).ok_or(Error::BadName)?;
    write(&to, &preset)?;
    if from != to {
        std::fs::remove_file(from)?;
    }
    Ok(())
}

/// Writes the preset `name` to `dest` (any folder), to share it.
pub fn export(dir: &Path, name: &str, dest: &Path) -> Result<(), Error> {
    write(dest, &load(dir, name)?)
}

/// Copies the preset file `src` into `dir` under its own name (` 2`, ` 3`... when taken).
/// Returns the name it got.
pub fn import(dir: &Path, src: &Path) -> Result<String, Error> {
    let mut preset = read(src)?;
    let base = preset.name.clone();
    let mut n = 1;
    while path_of(dir, &preset.name)?.exists() {
        n += 1;
        preset.name = format!("{base} {n}");
    }
    write(&path_of(dir, &preset.name)?, &preset)?;
    Ok(preset.name)
}

/// Screens ready to install: new ids (nothing aliases the previous setup), a screen on a
/// missing display falls back to a window, a second screen on one monitor too, a slice
/// routed to a layer / group the project lacks shows the composition. Returns the warnings.
pub fn instantiate(preset: &OutputPreset, env: Env) -> (Vec<Screen>, Vec<String>) {
    let mut warnings = vec![];
    let mut used = std::collections::HashSet::new();
    let screens = preset
        .setup
        .screens
        .iter()
        .map(|s| {
            let mut s = s.clone();
            s.id = Uuid::new_v4();
            if let OutputDevice::Monitor { name } = &s.device {
                if !env.monitors.contains(name) {
                    warnings.push(format!(
                        "Screen \"{}\": its display is not connected, using a window",
                        s.name
                    ));
                    s.device = OutputDevice::Window;
                } else if !used.insert(name.clone()) {
                    warnings.push(format!(
                        "Screen \"{}\": display already used by another screen, using a window",
                        s.name
                    ));
                    s.device = OutputDevice::Window;
                }
            }
            for sl in &mut s.slices {
                sl.id = Uuid::new_v4();
                let missing = match sl.source {
                    SliceSource::Layer(id) => !env.layers.contains(&id),
                    SliceSource::Group(id) => !env.groups.contains(&id),
                    _ => false,
                };
                if missing {
                    warnings.push(format!(
                        "Screen \"{}\": a slice routes a layer or group this project lacks, showing the composition",
                        s.name
                    ));
                    sl.source = SliceSource::Composition;
                }
            }
            s
        })
        .collect();
    (screens, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-presets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn wall() -> OutputSetup {
        let mut a = mapping::new_screen("Wall", 3840, 1080);
        a.device = OutputDevice::Monitor { name: "7".into() };
        let mut sl = mapping::new_slice(3840, 1080);
        sl.soft_edge = Some(vjc_model::SoftEdge {
            left: 0.1,
            right: 0.0,
            top: 0.0,
            bottom: 0.0,
            gamma: 1.0,
        });
        sl.mask = Some(vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]);
        sl.source = SliceSource::Layer(Uuid::from_u128(5));
        a.slices.push(sl);
        let mut b = mapping::new_screen("Side", 640, 360);
        b.device = OutputDevice::Syphon;
        OutputSetup {
            screens: vec![a, b],
        }
    }

    const ENV_ALL: Env<'static> = Env {
        monitors: &[],
        layers: &[],
        groups: &[],
    };

    #[test]
    fn save_list_load_roundtrip_keeps_everything() {
        let d = dir("roundtrip");
        assert!(list(&d).is_empty(), "missing folder lists nothing");
        let setup = wall();
        save(&d, "Venue A", &setup).unwrap();
        save(&d, "club b", &setup).unwrap();
        assert_eq!(list(&d), vec!["club b", "Venue A"]);
        let p = load(&d, "Venue A").unwrap();
        assert_eq!(
            p.setup, setup,
            "screens, slices, mask, soft edge and routing"
        );
        assert_eq!(p.name, "Venue A");
        // No temp file is left behind by the atomic write.
        let leftovers = std::fs::read_dir(&d)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x != EXT)
            })
            .count();
        assert_eq!(leftovers, 0);
        assert!(matches!(load(&d, "nope"), Err(Error::NotFound)));
    }

    #[test]
    fn names_cannot_escape_the_folder() {
        let d = dir("names");
        for bad in ["", "   ", "a\nb", &"x".repeat(81)] {
            assert!(
                matches!(save(&d, bad, &wall()), Err(Error::BadName)),
                "{bad:?}"
            );
        }
        let p = save(&d, "../../evil", &wall()).unwrap();
        assert_eq!(p.parent().unwrap(), d, "stays inside the preset folder");
        assert!(!p.file_name().unwrap().to_string_lossy().starts_with('.'));
        assert_eq!(list(&d), vec!["../../evil"]);
        assert!(load(&d, "../../evil").is_ok());
    }

    #[test]
    fn rename_delete_export_import() {
        let d = dir("ops");
        let setup = wall();
        save(&d, "One", &setup).unwrap();
        save(&d, "Two", &setup).unwrap();
        assert!(matches!(rename(&d, "One", "Two"), Err(Error::Exists)));
        rename(&d, "One", "Uno").unwrap();
        assert_eq!(list(&d), vec!["Two", "Uno"]);
        assert_eq!(load(&d, "Uno").unwrap().name, "Uno");
        // Case-only / same-name rename is fine.
        rename(&d, "Uno", "Uno").unwrap();
        let share = dir("share");
        std::fs::create_dir_all(&share).unwrap();
        let file = share.join("wall.vjcout");
        export(&d, "Uno", &file).unwrap();
        delete(&d, "Uno").unwrap();
        assert!(matches!(delete(&d, "Uno"), Err(Error::NotFound)));
        assert_eq!(import(&d, &file).unwrap(), "Uno");
        assert_eq!(import(&d, &file).unwrap(), "Uno 2", "never overwrites");
        assert_eq!(list(&d), vec!["Two", "Uno", "Uno 2"]);
        assert_eq!(load(&d, "Uno 2").unwrap().setup, setup);
    }

    #[test]
    fn corrupt_future_and_oversized_files_are_rejected_or_repaired() {
        let d = dir("bad");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("x.vjcout");
        std::fs::write(&f, b"{ not json").unwrap();
        assert!(matches!(read(&f), Err(Error::Json(_))));
        assert!(
            list(&d).is_empty(),
            "a corrupt file does not break the list"
        );
        let mut p = OutputPreset {
            format_version: FORMAT_VERSION + 1,
            name: "n".into(),
            setup: wall(),
        };
        std::fs::write(&f, serde_json::to_vec(&p).unwrap()).unwrap();
        assert!(matches!(read(&f), Err(Error::FutureVersion(_))));
        // Out-of-range values are clamped / dropped, not trusted.
        p.format_version = FORMAT_VERSION;
        p.setup.screens[0].width = 0;
        p.setup.screens[0].height = 99999;
        p.setup.screens[0].slices[0].output_mesh.pop();
        p.setup.screens[0].slices[0].mask = Some(vec![(0.0, 0.0)]);
        p.setup.screens[0].slices[0]
            .soft_edge
            .as_mut()
            .unwrap()
            .left = 9.0;
        std::fs::write(&f, serde_json::to_vec(&p).unwrap()).unwrap();
        let r = read(&f).unwrap();
        let s = &r.setup.screens[0];
        assert_eq!((s.width, s.height), (1, 16384));
        assert!(mapping::screens_valid(&r.setup.screens));
        assert_eq!(s.slices[0].mask, None);
        assert_eq!(
            s.slices[0].soft_edge.as_ref().unwrap().left,
            mapping::MAX_SOFT_WIDTH
        );
        std::fs::write(&f, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(matches!(read(&f), Err(Error::TooBig)));
    }

    #[test]
    fn instantiate_falls_back_to_window_with_a_warning_and_new_ids() {
        let d = dir("inst");
        save(&d, "W", &wall()).unwrap();
        let preset = load(&d, "W").unwrap();
        // Display 7 missing, layer 5 missing.
        let (screens, warnings) = instantiate(&preset, ENV_ALL);
        assert_eq!(screens[0].device, OutputDevice::Window);
        assert_eq!(
            screens[1].device,
            OutputDevice::Syphon,
            "other devices untouched"
        );
        assert_eq!(screens[0].slices[0].source, SliceSource::Composition);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("Wall") && warnings[0].contains("not connected"));
        assert_ne!(screens[0].id, preset.setup.screens[0].id);
        assert_ne!(
            screens[0].slices[0].id,
            preset.setup.screens[0].slices[0].id
        );
        assert!(mapping::screens_valid(&screens));
        // Display and layer present: kept, no warnings.
        let env = Env {
            monitors: &["7".to_string()],
            layers: &[Uuid::from_u128(5)],
            groups: &[],
        };
        let (screens, warnings) = instantiate(&preset, env);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            screens[0].device,
            OutputDevice::Monitor { name: "7".into() }
        );
        assert_eq!(
            screens[0].slices[0].source,
            SliceSource::Layer(Uuid::from_u128(5))
        );
        // Two screens on one display: the second becomes a window.
        let mut twin = preset.clone();
        twin.setup.screens[1].device = OutputDevice::Monitor { name: "7".into() };
        let (screens, warnings) = instantiate(&twin, env);
        assert_eq!(screens[1].device, OutputDevice::Window);
        assert_eq!(warnings.len(), 1);
    }
}
