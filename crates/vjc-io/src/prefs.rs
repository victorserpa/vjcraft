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
    /// Folder holding the ML model files (`.onnx`); `None` = `<app data dir>/models`.
    pub models_folder: Option<PathBuf>,
    /// Recently opened or saved `.vjc` projects, newest first (at most `MAX_RECENT`).
    pub recent_projects: Vec<PathBuf>,
    /// Browser favourites: `dir:<path>` for folders, `fx:<name>` for effects / generators.
    pub favorites: Vec<String>,
    /// Browser shows thumbnails in a grid instead of a list.
    pub browser_grid: bool,
    /// Folder the output recorder writes into; `None` = `~/Movies/VJCraft`.
    pub record_folder: Option<PathBuf>,
    /// Folder snapshot PNGs are saved into; `None` = ask on the first snapshot.
    pub snapshot_folder: Option<PathBuf>,
    /// Folder "Convert to HAP" writes into; `None` = next to the source file.
    pub hap_folder: Option<PathBuf>,
    /// Free space (GB) a conversion must leave on the volume; `None` = the larger of 10 GB
    /// and 10% of the volume.
    pub hap_reserve_gb: Option<u32>,
    /// Ask before quitting even when the project has no unsaved changes.
    pub confirm_quit: bool,
    /// Blend mode of layers added from the toolbar.
    pub default_blend: vjc_model::BlendMode,
    /// Milliseconds into a clip that a trigger starts at (0 = at the in-point).
    pub clip_start_offset_ms: u32,
    /// Scale of the whole interface (1.0 = normal).
    pub ui_scale: f32,
}

/// Longest recent-projects list kept.
pub const MAX_RECENT: usize = 12;

impl General {
    /// Moves `path` to the front of the recent projects (no duplicates, capped).
    pub fn push_recent(&mut self, path: &Path) {
        self.recent_projects.retain(|p| p != path);
        self.recent_projects.insert(0, path.to_path_buf());
        self.recent_projects.truncate(MAX_RECENT);
    }

    pub fn is_favorite(&self, key: &str) -> bool {
        self.favorites.iter().any(|f| f == key)
    }

    /// Adds the favourite, or removes it when present. Returns the new state.
    pub fn toggle_favorite(&mut self, key: &str) -> bool {
        if self.is_favorite(key) {
            self.favorites.retain(|f| f != key);
            false
        } else {
            self.favorites.push(key.to_string());
            true
        }
    }

    fn sanitize(&mut self) {
        self.clip_start_offset_ms = self.clip_start_offset_ms.min(600_000);
        self.ui_scale = if self.ui_scale.is_finite() {
            self.ui_scale.clamp(0.75, 2.0)
        } else {
            1.0
        };
        let mut seen = std::collections::HashSet::new();
        self.recent_projects.retain(|p| seen.insert(p.clone()));
        self.recent_projects.truncate(MAX_RECENT);
        // Old effect keys were `fx:<Category> / <Name>`; keep only the name.
        for f in &mut self.favorites {
            if let Some(rest) = f.strip_prefix("fx:")
                && let Some((_, name)) = rest.rsplit_once(" / ")
            {
                *f = format!("fx:{name}");
            }
        }
        let mut seen = std::collections::HashSet::new();
        self.favorites
            .retain(|f| !f.is_empty() && seen.insert(f.clone()));
    }
}

impl Default for General {
    fn default() -> Self {
        General {
            autosave: true,
            autosave_secs: 30,
            open_last_project: false,
            last_project: None,
            browser_folder: None,
            models_folder: None,
            recent_projects: vec![],
            favorites: vec![],
            browser_grid: false,
            record_folder: None,
            snapshot_folder: None,
            hap_folder: None,
            hap_reserve_gb: None,
            confirm_quit: false,
            default_blend: vjc_model::BlendMode::Normal,
            clip_start_offset_ms: 0,
            ui_scale: 1.0,
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
    /// Render rate ceiling in fps; 0 = unlimited (the composition fps and the display decide).
    pub max_fps: u32,
    /// A dropped folder that is a pure numbered image sequence becomes one clip.
    pub folder_sequence: bool,
    /// Ask for a 10-bit (Rgb10a2) output surface; read at startup, falls back to 8-bit when
    /// the adapter or surface cannot do it.
    pub output_10bit: bool,
}

impl Default for Video {
    fn default() -> Self {
        Video {
            width: 1920,
            height: 1080,
            fps: 60.0,
            fit: FitMode::Fit,
            vsync: true,
            max_fps: 0,
            folder_sequence: true,
            output_10bit: false,
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
    /// Quantizes clip triggers and beat-loop starts to the tempo clock.
    pub beat_snap: vjc_model::BeatSnap,
    /// Tempo nudge: speed change while the nudge buttons are held, in percent (1..=50).
    pub nudge_percent: u32,
}

impl Default for Control {
    fn default() -> Self {
        Control {
            default_shortcuts: true,
            offer_shortcuts: true,
            beat_snap: vjc_model::BeatSnap::Off,
            nudge_percent: 10,
        }
    }
}

/// MIDI ports the user switched off (by port name). Everything is enabled by default, so
/// a new device works at once.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Midi {
    pub disabled_inputs: Vec<String>,
    pub disabled_outputs: Vec<String>,
    /// Which octave number note 60 gets in note names.
    pub middle_c: MiddleC,
}

/// Naming of MIDI notes: note 60 is "C3" (Ableton, Arena) or "C4" (scientific pitch).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MiddleC {
    #[default]
    C3,
    C4,
}

impl MiddleC {
    pub fn label(self) -> &'static str {
        match self {
            MiddleC::C3 => "C3",
            MiddleC::C4 => "C4",
        }
    }

    /// "C#3" style name of a MIDI note under this convention.
    pub fn note_name(self, note: u8) -> String {
        const NAMES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        let base = match self {
            MiddleC::C3 => 2,
            MiddleC::C4 => 1,
        };
        format!(
            "{}{}",
            NAMES[usize::from(note % 12)],
            i32::from(note / 12) - base
        )
    }
}

impl Midi {
    pub fn input_enabled(&self, name: &str) -> bool {
        !self.disabled_inputs.iter().any(|n| n == name)
    }

    pub fn output_enabled(&self, name: &str) -> bool {
        !self.disabled_outputs.iter().any(|n| n == name)
    }

    pub fn set_input(&mut self, name: &str, on: bool) {
        Self::set(&mut self.disabled_inputs, name, on);
    }

    pub fn set_output(&mut self, name: &str, on: bool) {
        Self::set(&mut self.disabled_outputs, name, on);
    }

    fn set(list: &mut Vec<String>, name: &str, on: bool) {
        list.retain(|n| n != name);
        if !on {
            list.push(name.to_owned());
        }
    }
}

/// FFT sizes the analysis accepts (powers of two).
pub const FFT_SIZES: [usize; 5] = [256, 512, 1024, 2048, 4096];

/// Audio devices and engine settings. `None` means "what the device offers by default".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Chosen output device name (None = system default).
    pub output_device: Option<String>,
    /// Input device for audio-reactive animation and the FFT (None = system default).
    pub input_device: Option<String>,
    /// 0-based channel of the input device that feeds the analysis.
    pub input_channel: u16,
    /// Output sample rate in Hz, if the device supports it.
    pub sample_rate: Option<u32>,
    /// Output buffer size in frames, clamped to what the device supports.
    pub buffer_size: Option<u32>,
    /// Output channel count (1 = mono mix; more than 2 leaves the extra channels silent).
    pub channels: Option<u16>,
    /// Samples per FFT block of the input analysis; one of `FFT_SIZES`.
    pub fft_size: usize,
}

impl Default for Audio {
    fn default() -> Self {
        Audio {
            output_device: None,
            input_device: None,
            input_channel: 0,
            sample_rate: None,
            buffer_size: None,
            channels: None,
            fft_size: 1024,
        }
    }
}

impl Audio {
    /// Drops values no device could use, so a hand-edited file cannot wedge the engine.
    fn sanitize(&mut self) {
        self.sample_rate = self.sample_rate.filter(|r| (8_000..=384_000).contains(r));
        self.buffer_size = self.buffer_size.filter(|b| (16..=16_384).contains(b));
        self.channels = self.channels.filter(|c| (1..=32).contains(c));
        self.input_channel = self.input_channel.min(63);
        if !FFT_SIZES.contains(&self.fft_size) {
            self.fft_size = Audio::default().fft_size;
        }
        for d in [&mut self.output_device, &mut self.input_device] {
            if d.as_deref().is_some_and(|n| n.trim().is_empty()) {
                *d = None;
            }
        }
    }
}

/// Output recording: codec id (see `vjc_media::record::RecCodec::id`) and quality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    /// "h264", "hevc", "prores" or "png"; an id the build cannot encode falls back to h264.
    pub codec: String,
    /// 1 (small) to 100 (best); ignored by the lossless PNG sequence.
    pub quality: u8,
}

impl Default for Record {
    fn default() -> Self {
        Record {
            codec: "h264".into(),
            quality: 70,
        }
    }
}

/// What a new clip starts with (applied when a file is loaded into a cell).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub play_mode: vjc_model::PlayMode,
    pub direction: vjc_model::Direction,
    /// Where a trigger starts the clip (`Layer` = the layer's setting).
    pub trigger_mode: vjc_model::TriggerMode,
}

impl Default for Defaults {
    fn default() -> Self {
        let t = vjc_model::Transport::default();
        Defaults {
            play_mode: t.play_mode,
            direction: t.direction,
            trigger_mode: t.trigger_mode,
        }
    }
}

impl Defaults {
    /// Sets the transport of a freshly created clip.
    pub fn apply(&self, clip: &mut vjc_model::Clip) {
        let t = &mut clip.transport;
        t.play_mode = self.play_mode;
        t.direction = self.direction;
        t.resume_direction = match self.direction {
            vjc_model::Direction::Pause => vjc_model::Direction::Forward,
            d => d,
        };
        t.trigger_mode = self.trigger_mode;
    }
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
    /// "Web remote PIN": when not empty, mutating calls and the WebSocket must carry it.
    pub web_pin: String,
}

impl Default for Api {
    fn default() -> Self {
        Api {
            enabled: false,
            port: 8090,
            expose_network: false,
            web_pin: String::new(),
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

/// Panel visibility and sizes (points) of the main window, saved under a name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub name: String,
    pub browser: bool,
    pub browser_w: f32,
    pub monitors: bool,
    pub monitors_h: f32,
    pub params: bool,
    pub params_h: f32,
    pub grid: bool,
    /// Speed sliders of the master, groups and clips (hidden = less clutter, speeds stay).
    pub speed_sliders: bool,
    /// Interface parts hidden by the user (keys of `VIEW_TOGGLES`).
    pub hidden: Vec<String>,
}

/// Parts of the interface the View menu can show or hide: (key, menu label).
pub const VIEW_TOGGLES: [(&str, &str); 9] = [
    ("crossfader", "Crossfader"),
    ("dashboard", "Dashboard"),
    ("autopilot", "Autopilot sections"),
    ("cue-points", "Cue points"),
    ("transition", "Transition controls"),
    ("layer-transport", "Layer transport controls"),
    ("undo", "Undo toolbar"),
    ("column-triggers", "Column triggers"),
    ("audio", "Audio controls"),
];

impl Default for Layout {
    fn default() -> Self {
        Layout {
            name: "Default".into(),
            browser: true,
            browser_w: 260.0,
            monitors: true,
            monitors_h: 220.0,
            params: true,
            params_h: 400.0,
            grid: true,
            speed_sliders: true,
            hidden: vec![],
        }
    }
}

impl Layout {
    /// Whether the interface part `key` (see `VIEW_TOGGLES`) is hidden.
    pub fn is_hidden(&self, key: &str) -> bool {
        self.hidden.iter().any(|h| h == key)
    }

    /// Hides or shows the interface part `key`.
    pub fn set_hidden(&mut self, key: &str, hide: bool) {
        self.hidden.retain(|h| h != key);
        if hide {
            self.hidden.push(key.to_owned());
        }
    }

    /// The built-in layouts, in menu order.
    pub fn presets() -> Vec<Layout> {
        let d = Layout::default();
        vec![
            d.clone(),
            Layout {
                name: "Performance".into(),
                browser: false,
                params: false,
                monitors_h: 180.0,
                ..d.clone()
            },
            Layout {
                name: "Mapping".into(),
                monitors_h: 420.0,
                browser: false,
                params_h: 140.0,
                ..d
            },
        ]
    }

    pub fn is_preset(name: &str) -> bool {
        Layout::presets().iter().any(|l| l.name == name)
    }

    fn sanitize(&mut self) {
        self.hidden
            .retain(|k| VIEW_TOGGLES.iter().any(|(key, _)| key == k));
        self.hidden.sort();
        self.hidden.dedup();
        self.name = self.name.trim().to_owned();
        for v in [
            &mut self.browser_w,
            &mut self.monitors_h,
            &mut self.params_h,
        ] {
            *v = if v.is_finite() {
                v.clamp(40.0, 4000.0)
            } else {
                220.0
            };
        }
    }
}

/// The layout in use (restored on start), the user's named layouts and the help toggle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layouts {
    pub current: Layout,
    pub custom: Vec<Layout>,
    pub help: bool,
}

impl Default for Layouts {
    fn default() -> Self {
        Layouts {
            current: Layout::default(),
            custom: vec![],
            help: true,
        }
    }
}

impl Layouts {
    fn sanitize(&mut self) {
        self.current.sanitize();
        for l in &mut self.custom {
            l.sanitize();
        }
        self.custom
            .retain(|l| !l.name.is_empty() && !Layout::is_preset(&l.name));
        let mut seen = std::collections::HashSet::new();
        self.custom.retain(|l| seen.insert(l.name.clone()));
    }

    /// Preset or custom layout by name.
    pub fn find(&self, name: &str) -> Option<Layout> {
        Layout::presets()
            .into_iter()
            .chain(self.custom.iter().cloned())
            .find(|l| l.name == name)
    }

    /// Saves `layout` under its name (replacing a custom one); false for empty or preset names.
    pub fn save(&mut self, layout: Layout) -> bool {
        let name = layout.name.trim().to_owned();
        if name.is_empty() || Layout::is_preset(&name) {
            return false;
        }
        let l = Layout { name, ..layout };
        match self.custom.iter_mut().find(|c| c.name == l.name) {
            Some(c) => *c = l,
            None => self.custom.push(l),
        }
        true
    }

    pub fn delete(&mut self, name: &str) -> bool {
        let n = self.custom.len();
        self.custom.retain(|l| l.name != name);
        self.custom.len() != n
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub version: u32,
    pub general: General,
    pub video: Video,
    pub control: Control,
    pub audio: Audio,
    pub record: Record,
    pub defaults: Defaults,
    pub midi: Midi,
    pub osc: Osc,
    pub dmx: Dmx,
    pub api: Api,
    pub timecode: Timecode,
    pub layouts: Layouts,
    /// Parameter-animation and dashboard presets of this user.
    pub presets: crate::param_presets::Presets,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            version: PREFS_VERSION,
            general: General::default(),
            video: Video::default(),
            control: Control::default(),
            audio: Audio::default(),
            record: Record::default(),
            defaults: Defaults::default(),
            midi: Midi::default(),
            osc: Osc::default(),
            dmx: Dmx::default(),
            api: Api::default(),
            timecode: Timecode::default(),
            layouts: Layouts::default(),
            presets: Default::default(),
        }
    }
}

impl Prefs {
    /// Clamps values to sane ranges.
    pub fn sanitize(&mut self) {
        self.general.sanitize();
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
        self.layouts.sanitize();
        self.audio.sanitize();
        self.record.quality = self.record.quality.clamp(1, 100);
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
            merge_old::<Prefs>(&mut new, &old);
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

/// Keeps what this build does not know from `old`. The known shape is `old` as `T` would
/// re-serialize it (not `T::default()`, whose maps are empty and whose `None` structs have no
/// fields, which would make every map key / inner field look unknown and resurrect it).
fn merge_old<T>(new: &mut serde_json::Value, old: &serde_json::Value)
where
    T: Serialize + serde::de::DeserializeOwned + Default,
{
    let known = serde_json::from_value::<T>(old.clone())
        .ok()
        .and_then(|t| serde_json::to_value(t).ok())
        .or_else(|| serde_json::to_value(T::default()).ok());
    if let Some(known) = known {
        merge_unknown(new, old, &known);
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
    fn hidden_view_parts_roundtrip_and_unknown_keys_are_dropped() {
        let mut p = super::Prefs::default();
        p.layouts.current.set_hidden("crossfader", true);
        p.layouts.current.set_hidden("undo", true);
        p.layouts.current.set_hidden("undo", false);
        let (back, _) = super::Prefs::from_json(&serde_json::to_string(&p).unwrap());
        assert!(back.layouts.current.is_hidden("crossfader"));
        assert!(!back.layouts.current.is_hidden("undo"));
        let (old, _) =
            super::Prefs::from_json(r#"{"layouts":{"current":{"hidden":["x","audio"]}}}"#);
        assert_eq!(old.layouts.current.hidden, vec!["audio".to_owned()]);
    }

    #[test]
    fn new_prefs_fields_roundtrip_and_default_when_missing() {
        let mut p = super::Prefs::default();
        p.general.clip_start_offset_ms = 1500;
        p.general.ui_scale = 1.25;
        p.defaults.play_mode = vjc_model::PlayMode::Bounce;
        p.defaults.direction = vjc_model::Direction::Backward;
        p.defaults.trigger_mode = vjc_model::TriggerMode::Continue;
        let (back, warn) = super::Prefs::from_json(&serde_json::to_string(&p).unwrap());
        assert!(warn.is_none());
        assert_eq!(back.general.clip_start_offset_ms, 1500);
        assert_eq!(back.general.ui_scale, 1.25);
        assert_eq!(back.defaults, p.defaults);
        // An old file without the fields loads with the defaults.
        let (old, warn) = super::Prefs::from_json(r#"{"general":{"autosave":false}}"#);
        assert!(warn.is_none());
        assert_eq!(old.defaults, super::Defaults::default());
        assert_eq!(old.general.ui_scale, 1.0);
        // Nonsense is clamped.
        let (bad, _) = super::Prefs::from_json(r#"{"general":{"ui_scale":99.0}}"#);
        assert_eq!(bad.general.ui_scale, 2.0);
    }

    #[derive(Default, serde::Serialize, serde::Deserialize)]
    #[serde(default)]
    struct Inner {
        #[serde(skip_serializing_if = "Option::is_none")]
        a: Option<u32>,
        b: u32,
    }

    #[derive(Default, serde::Serialize, serde::Deserialize)]
    #[serde(default)]
    struct Future {
        map: std::collections::BTreeMap<String, u32>,
        inner: Option<Inner>,
    }

    #[test]
    fn merge_does_not_resurrect_map_keys_or_inner_fields() {
        let old = serde_json::json!({"map":{"a":1,"b":2},"inner":{"a":1,"b":2},"future":5});
        let mut new = serde_json::json!({"map":{"a":1},"inner":{"b":2}});
        super::merge_old::<Future>(&mut new, &old);
        assert_eq!(new["map"], serde_json::json!({"a":1}));
        assert_eq!(new["inner"], serde_json::json!({"b":2}));
        assert_eq!(new["future"], 5);
    }

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
    fn autosave_is_on_by_default_and_at_most_every_30_seconds() {
        let g = Prefs::default().general;
        assert!(g.autosave && g.autosave_secs <= 30, "{g:?}");
        // A prefs file that never mentioned autosave gets the same defaults.
        let g = Prefs::from_json("{}").0.general;
        assert!(g.autosave && g.autosave_secs <= 30);
        // Negative control: an explicit choice is kept.
        let g = Prefs::from_json(r#"{"general":{"autosave":false,"autosave_secs":120}}"#)
            .0
            .general;
        assert!(!g.autosave && g.autosave_secs == 120);
    }

    #[test]
    fn audio_and_record_prefs_roundtrip_and_old_files_load() {
        // A file from before these fields existed.
        let (p, w) = Prefs::from_json(r#"{"audio":{"output_device":"Built-in"},"general":{}}"#);
        assert!(w.is_none());
        assert_eq!(p.audio.output_device.as_deref(), Some("Built-in"));
        assert_eq!(p.audio.fft_size, 1024);
        assert_eq!(
            (p.audio.sample_rate, p.audio.buffer_size, p.audio.channels),
            (None, None, None)
        );
        assert_eq!((p.record.codec.as_str(), p.record.quality), ("h264", 70));
        let mut q = p.clone();
        q.audio = Audio {
            output_device: Some("USB".into()),
            input_device: Some("Mic".into()),
            input_channel: 1,
            sample_rate: Some(96_000),
            buffer_size: Some(256),
            channels: Some(2),
            fft_size: 2048,
        };
        q.record = Record {
            codec: "prores".into(),
            quality: 90,
        };
        let (back, w) = Prefs::from_json(&serde_json::to_string(&q).unwrap());
        assert!(w.is_none());
        assert_eq!(back.audio, q.audio);
        assert_eq!(back.record, q.record);
    }

    #[test]
    fn nonsense_audio_values_are_dropped_on_load() {
        let (p, _) = Prefs::from_json(
            r#"{"audio":{"sample_rate":5,"buffer_size":1,"channels":0,"fft_size":1000,"input_device":" "},"record":{"quality":0}}"#,
        );
        assert_eq!(
            (p.audio.sample_rate, p.audio.buffer_size, p.audio.channels),
            (None, None, None)
        );
        assert_eq!((p.audio.fft_size, p.audio.input_device), (1024, None));
        assert_eq!(p.record.quality, 1);
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

    #[test]
    fn recent_projects_move_to_front_dedupe_and_cap() {
        let mut g = General::default();
        for n in 0..(MAX_RECENT + 3) {
            g.push_recent(Path::new(&format!("/p/{n}.vjc")));
        }
        assert_eq!(g.recent_projects.len(), MAX_RECENT);
        g.push_recent(Path::new("/p/5.vjc"));
        assert_eq!(g.recent_projects[0], Path::new("/p/5.vjc"));
        assert_eq!(
            g.recent_projects
                .iter()
                .filter(|p| *p == Path::new("/p/5.vjc"))
                .count(),
            1
        );
    }

    #[test]
    fn old_category_favorite_keys_migrate_on_load() {
        let mut g = General {
            favorites: vec!["fx:Color / Blur".into(), "fx:Blur".into(), "dir:/a".into()],
            ..General::default()
        };
        g.sanitize();
        assert_eq!(g.favorites, ["fx:Blur", "dir:/a"]);
    }

    #[test]
    fn favorites_toggle_roundtrip_through_the_prefs_file() {
        let mut p = Prefs::default();
        assert!(p.general.toggle_favorite("dir:/a"));
        assert!(p.general.toggle_favorite("fx:Blur"));
        assert!(!p.general.toggle_favorite("dir:/a"));
        p.general.browser_grid = true;
        let f = std::env::temp_dir().join(format!("vjc-fav-{}.json", std::process::id()));
        p.save(&f).unwrap();
        let (back, _) = Prefs::load(&f);
        std::fs::remove_file(&f).ok();
        assert_eq!(back.general.favorites, ["fx:Blur"]);
        assert!(back.general.browser_grid);
    }

    #[test]
    fn nudge_snapshot_and_middle_c_default_and_roundtrip() {
        use super::{MiddleC, Prefs};
        let (old, _) = Prefs::from_json(r#"{"control":{"default_shortcuts":false}}"#);
        assert_eq!(old.control.nudge_percent, 10);
        assert_eq!(old.midi.middle_c, MiddleC::C3);
        assert_eq!(old.general.snapshot_folder, None);
        let mut p = Prefs::default();
        p.control.nudge_percent = 25;
        p.midi.middle_c = MiddleC::C4;
        p.general.snapshot_folder = Some("/tmp/x".into());
        let back = Prefs::from_json(&serde_json::to_string(&p).unwrap()).0;
        assert_eq!(back.control.nudge_percent, 25);
        assert_eq!(back.midi.middle_c, MiddleC::C4);
        assert_eq!(back.general.snapshot_folder, Some("/tmp/x".into()));
    }

    #[test]
    fn middle_c_names_notes_in_both_conventions() {
        use super::MiddleC;
        assert_eq!(MiddleC::C3.note_name(60), "C3");
        assert_eq!(MiddleC::C4.note_name(60), "C4");
        assert_eq!(MiddleC::C3.note_name(61), "C#3");
        assert_eq!(MiddleC::C3.note_name(0), "C-2");
        assert_eq!(MiddleC::C4.note_name(127), "G9");
    }

    #[test]
    fn beat_snap_defaults_off_and_roundtrips() {
        use super::Prefs;
        let (old, _) = Prefs::from_json(r#"{"control":{"default_shortcuts":false}}"#);
        assert_eq!(old.control.beat_snap, vjc_model::BeatSnap::Off);
        let mut p = Prefs::default();
        p.control.beat_snap = vjc_model::BeatSnap::Bar;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(
            Prefs::from_json(&json).0.control.beat_snap,
            vjc_model::BeatSnap::Bar
        );
    }

    #[test]
    fn max_fps_persists_and_defaults_to_unlimited() {
        let v: Video = serde_json::from_str("{}").unwrap();
        assert_eq!(v.max_fps, 0);
        let v = Video {
            max_fps: 30,
            ..Video::default()
        };
        let back: Video = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(back.max_fps, 30);
    }
}

#[cfg(test)]
mod general_extra_tests {
    use super::*;

    #[test]
    fn confirm_quit_and_default_blend_roundtrip_and_default() {
        let g = General::default();
        assert!(!g.confirm_quit);
        assert_eq!(g.default_blend, vjc_model::BlendMode::Normal);
        let mut p = Prefs::default();
        p.general.confirm_quit = true;
        p.general.default_blend = vjc_model::BlendMode::Screen;
        let s = serde_json::to_string(&p).unwrap();
        let q: Prefs = serde_json::from_str(&s).unwrap();
        assert!(q.general.confirm_quit);
        assert_eq!(q.general.default_blend, vjc_model::BlendMode::Screen);
        // Old files without the keys still load.
        let old: Prefs = serde_json::from_str(r#"{"general":{"autosave":false}}"#).unwrap();
        assert!(!old.general.confirm_quit);
    }
}
