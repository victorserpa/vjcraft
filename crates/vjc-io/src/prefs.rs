//! User preferences (not project data): one JSON file in the per-user config dir.
//! Every field has a default, so older or partial files load fine; a corrupt
//! file yields defaults plus a warning, never a panic.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vjc_model::FitMode;

pub const PREFS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub autosave: bool,
    pub autosave_secs: u32,
    pub open_last_project: bool,
    pub last_project: Option<PathBuf>,
    /// Browser start folder; `None` = project folder, else ~/Movies.
    pub browser_folder: Option<PathBuf>,
}

impl Default for General {
    fn default() -> Self {
        General {
            autosave: true,
            autosave_secs: 60,
            open_last_project: false,
            last_project: None,
            browser_folder: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Video {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub fit: FitMode,
    /// Read at startup only.
    pub vsync: bool,
}

impl Default for Video {
    fn default() -> Self {
        Video {
            width: 1920,
            height: 1080,
            fps: 60.0,
            fit: FitMode::Fit,
            vsync: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Control {
    /// New projects start with the default shortcut preset.
    pub default_shortcuts: bool,
    /// Offer the preset when opening a project that has no shortcuts.
    pub offer_shortcuts: bool,
}

impl Default for Control {
    fn default() -> Self {
        Control {
            default_shortcuts: true,
            offer_shortcuts: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Chosen output device name (placeholder: not used by the engine yet).
    pub output_device: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub version: u32,
    pub general: General,
    pub video: Video,
    pub control: Control,
    pub audio: Audio,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            version: PREFS_VERSION,
            general: General::default(),
            video: Video::default(),
            control: Control::default(),
            audio: Audio::default(),
        }
    }
}

impl Prefs {
    /// Clamps values to sane ranges.
    pub fn sanitize(&mut self) {
        self.general.autosave_secs = self.general.autosave_secs.clamp(5, 3600);
        self.video.width = self.video.width.clamp(1, 16384);
        self.video.height = self.video.height.clamp(1, 16384);
        if !(self.video.fps.is_finite() && self.video.fps > 0.0) {
            self.video.fps = Video::default().fps;
        }
        self.video.fps = self.video.fps.min(240.0);
    }

    pub fn from_json(text: &str) -> (Prefs, Option<String>) {
        match serde_json::from_str::<Prefs>(text) {
            Ok(mut p) => {
                p.sanitize();
                p.version = PREFS_VERSION;
                (p, None)
            }
            Err(e) => (
                Prefs::default(),
                Some(format!("Preferences unreadable, using defaults: {e}")),
            ),
        }
    }

    /// Loads from `path`. Missing file = defaults, no warning.
    pub fn load(path: &Path) -> (Prefs, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(t) => {
                let (p, warn) = Prefs::from_json(&t);
                if warn.is_some() {
                    // Keep the unreadable file so the next save does not destroy it.
                    let _ = std::fs::copy(path, path.with_extension("json.bad"));
                }
                (p, warn)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Prefs::default(), None),
            Err(e) => (
                Prefs::default(),
                Some(format!("Preferences unreadable, using defaults: {e}")),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::write_atomic(path, &json)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn corrupt_file_is_backed_up() {
        let d = std::env::temp_dir().join(format!("vjc-prefs-bak-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("prefs.json");
        std::fs::write(&f, b"{ not json").unwrap();
        let (_, w) = super::Prefs::load(&f);
        assert!(w.is_some());
        assert_eq!(
            std::fs::read(f.with_extension("json.bad")).unwrap(),
            b"{ not json"
        );
    }

    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-prefs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("prefs.json")
    }

    #[test]
    fn roundtrip() {
        let p = tmp("rt");
        let mut a = Prefs::default();
        a.general.autosave_secs = 30;
        a.general.last_project = Some("/x/a.vjc".into());
        a.video.fit = FitMode::Fill;
        a.audio.output_device = Some("Built-in".into());
        a.save(&p).unwrap();
        let (b, w) = Prefs::load(&p);
        assert_eq!(a, b);
        assert!(w.is_none());
    }

    #[test]
    fn missing_fields_default() {
        let (p, w) = Prefs::from_json(r#"{"video":{"fps":30.0},"future":1}"#);
        assert!(w.is_none());
        assert_eq!(p.video.fps, 30.0);
        assert_eq!(p.video.width, 1920);
        assert_eq!(p.general, General::default());
    }

    #[test]
    fn corrupt_file_gives_defaults_and_warning() {
        let p = tmp("bad");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        let (d, w) = Prefs::load(&p);
        assert_eq!(d, Prefs::default());
        assert!(w.is_some());
        let (d, w) = Prefs::from_json(r#"{"video":{"fps":"x"}}"#);
        assert_eq!(d, Prefs::default());
        assert!(w.is_some());
    }

    #[test]
    fn missing_file_is_silent_and_values_clamped() {
        let (d, w) = Prefs::load(&tmp("none"));
        assert!(d == Prefs::default() && w.is_none());
        let (p, _) =
            Prefs::from_json(r#"{"general":{"autosave_secs":0},"video":{"width":0,"fps":-1}}"#);
        assert_eq!(p.general.autosave_secs, 5);
        assert_eq!(p.video.width, 1);
        assert_eq!(p.video.fps, 60.0);
    }
}
