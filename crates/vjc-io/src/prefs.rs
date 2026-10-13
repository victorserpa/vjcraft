//! User preferences (not project data): one JSON file in the per-user config dir.
//! Every field has a default, so older or partial files load fine; a corrupt
//! file yields defaults plus a warning, never a panic.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vjc_model::{FitMode, Fixture, TcFps};

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
pub struct Osc {
    pub in_enabled: bool,
    pub in_port: u16,
    /// Bind 0.0.0.0 instead of 127.0.0.1 (anyone on the network can then send commands).
    pub accept_network: bool,
    pub out_enabled: bool,
    pub out_host: String,
    pub out_port: u16,
}

impl Default for Osc {
    fn default() -> Self {
        Osc {
            in_enabled: false,
            in_port: 7000,
            accept_network: false,
            out_enabled: false,
            out_host: "127.0.0.1".into(),
            out_port: 9000,
        }
    }
}

/// DMX / Art-Net input (off by default) and the user's fixture library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dmx {
    pub enabled: bool,
    /// Local IPv4 to listen on: 127.0.0.1 (this computer only), 0.0.0.0 (all interfaces),
    /// or the address of one network interface.
    pub interface: String,
    pub port: u16,
    /// First universe (port-address, 0..=32767) accepted.
    pub universe: u16,
    /// How many consecutive universes are accepted.
    pub universes: u16,
    /// Fixtures saved by the user (the predefined ones live in code).
    pub fixtures: Vec<Fixture>,
}

impl Default for Dmx {
    fn default() -> Self {
        Dmx {
            enabled: false,
            interface: "127.0.0.1".into(),
            port: 6454,
            universe: 0,
            universes: 1,
            fixtures: vec![],
        }
    }
}

/// Remote HTTP/WebSocket API (no authentication: loopback unless the user exposes it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Api {
    pub enabled: bool,
    /// REST port; the WebSocket listens on `port + 1`.
    pub port: u16,
    /// Bind 0.0.0.0 instead of 127.0.0.1 (anyone on the network can then control VJCraft).
    pub expose_network: bool,
}

impl Default for Api {
    fn default() -> Self {
        Api {
            enabled: false,
            port: 8090,
            expose_network: false,
        }
    }
}

/// Where the external timecode comes from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum TcSource {
    #[default]
    Off,
    /// MIDI Timecode from the named input port.
    Mtc { port: String },
    /// LTC audio from an input device (empty name = default) and 0-based channel.
    Ltc { device: String, channel: u16 },
}

/// What a timecode-driven clip does when the signal disappears.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnLoss {
    #[default]
    Freeze,
    FreeRun,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timecode {
    pub source: TcSource,
    /// Added to the incoming timecode, in frames of the incoming rate.
    pub offset_frames: i32,
    pub on_loss: OnLoss,
    /// LTC only: fixed frame rate; None detects it.
    pub ltc_fps: Option<TcFps>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub version: u32,
    pub general: General,
    pub video: Video,
    pub control: Control,
    pub audio: Audio,
    pub osc: Osc,
    pub dmx: Dmx,
    pub api: Api,
    pub timecode: Timecode,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            version: PREFS_VERSION,
            general: General::default(),
            video: Video::default(),
            control: Control::default(),
            audio: Audio::default(),
            osc: Osc::default(),
            dmx: Dmx::default(),
            api: Api::default(),
            timecode: Timecode::default(),
        }
    }
}

impl Prefs {
    /// Clamps values to sane ranges.
    pub fn sanitize(&mut self) {
        self.api.port = self.api.port.clamp(1, 65534);
        self.general.autosave_secs = self.general.autosave_secs.clamp(5, 3600);
        self.video.width = self.video.width.clamp(1, 16384);
        self.video.height = self.video.height.clamp(1, 16384);
        if !(self.video.fps.is_finite() && self.video.fps > 0.0) {
            self.video.fps = Video::default().fps;
        }
        self.video.fps = self.video.fps.min(240.0);
        self.osc.in_port = self.osc.in_port.max(1);
        self.osc.out_port = self.osc.out_port.max(1);
        self.osc.out_host = self.osc.out_host.trim().to_owned();
        self.dmx.port = self.dmx.port.max(1);
        self.dmx.universe = self.dmx.universe.min(0x7FFF);
        self.dmx.universes = self.dmx.universes.clamp(1, 64);
        self.dmx.interface = self.dmx.interface.trim().to_owned();
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
        // Keep fields this build does not know (file written by a newer version).
        let mut new = serde_json::to_value(self).map_err(std::io::Error::other)?;
        if let Some(old) = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        {
            let known = serde_json::to_value(Prefs::default()).map_err(std::io::Error::other)?;
            merge_unknown(&mut new, &old, &known);
            if let Some(v) = old.get("version").and_then(|v| v.as_u64())
                && v > u64::from(PREFS_VERSION)
            {
                new["version"] = v.into();
            }
        }
        let json = serde_json::to_vec_pretty(&new).map_err(std::io::Error::other)?;
        crate::write_atomic(path, &json)
    }
}

/// Copies keys of `old` that the current struct does not know (`known` is a
/// fully populated serialization), recursing into objects. Keys the struct knows
/// but omitted from `new` (removed / None) stay removed.
fn merge_unknown(new: &mut serde_json::Value, old: &serde_json::Value, known: &serde_json::Value) {
    if let (Some(n), Some(o)) = (new.as_object_mut(), old.as_object()) {
        for (k, v) in o {
            match (n.get_mut(k), known.get(k)) {
                (Some(nv), Some(kv)) => merge_unknown(nv, v, kv),
                (None, Some(_)) => {}
                (_, None) => {
                    n.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn save_keeps_unknown_fields_of_newer_file() {
        let d = std::env::temp_dir().join(format!("vjc-prefs-new-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("prefs.json");
        std::fs::write(
            &f,
            r#"{"version":7,"future":{"a":1},"video":{"fps":30.0,"hdr":true}}"#,
        )
        .unwrap();
        let (p, _) = super::Prefs::load(&f);
        p.save(&f).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert_eq!(v["future"]["a"], 1);
        assert_eq!(v["video"]["hdr"], true);
        assert_eq!(v["video"]["fps"], 30.0);
        assert_eq!(v["version"], 7);
    }

    #[test]
    fn save_does_not_resurrect_cleared_known_fields() {
        let d = std::env::temp_dir().join(format!("vjc-prefs-clr-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("prefs.json");
        let mut p = super::Prefs::default();
        p.general.last_project = Some("/tmp/x.vjc".into());
        p.save(&f).unwrap();
        p.general.last_project = None;
        p.save(&f).unwrap();
        p.save(&f).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert!(v["general"]["last_project"].is_null());
        std::fs::write(&f, r#"{"general":{"last_project":"/a","newer":5}}"#).unwrap();
        p.save(&f).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert!(v["general"]["last_project"].is_null());
        assert_eq!(v["general"]["newer"], 5);
    }

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
    fn api_is_off_on_loopback_port_8090_by_default() {
        let mut p = Prefs::default();
        assert!(!p.api.enabled && !p.api.expose_network);
        assert_eq!(p.api.port, 8090);
        p.api.port = 65535;
        p.sanitize();
        assert_eq!(p.api.port, 65534);
    }

    #[test]
    fn dmx_is_off_by_default_and_roundtrips_fixtures() {
        let mut p = Prefs::default();
        assert!(!p.dmx.enabled);
        assert_eq!((p.dmx.port, p.dmx.interface.as_str()), (6454, "127.0.0.1"));
        p.dmx.fixtures.push(Fixture::default());
        p.dmx.universe = 60000;
        p.dmx.universes = 0;
        p.sanitize();
        assert_eq!((p.dmx.universe, p.dmx.universes), (0x7FFF, 1));
        let (back, w) = Prefs::from_json(&serde_json::to_string(&p).unwrap());
        assert!(w.is_none());
        assert_eq!(back.dmx, p.dmx);
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
