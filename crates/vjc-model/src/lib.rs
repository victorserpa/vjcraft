//! VJCraft project data model (format_version 1).
//! Serialized as JSON (`.vjc`). Clean-room design; see replica/architecture.md.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub type LayerId = Uuid;
pub type ClipId = Uuid;
pub type EffectId = Uuid;
pub type DeckId = Uuid;
pub type ColumnIdx = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub format_version: u32,
    pub composition: Composition,
    pub output: OutputSetup,
    pub shortcuts: Vec<Shortcut>,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    pub id: Uuid,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub tempo: Tempo,
    /// Ordered top to bottom.
    pub layers: Vec<Layer>,
    pub columns: Vec<Column>,
    pub decks: Vec<Deck>,
    pub current_deck: DeckId,
    pub master: Track,
    /// Multiplies the speed of every clip, 0..=4.
    #[serde(default = "one")]
    pub master_speed: f64,
    pub crossfader: Option<Crossfader>,
    pub dashboard: Vec<Param>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tempo {
    pub bpm: f64,
    pub link_enabled: bool,
    pub metronome: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub track: Track,
    pub solo: bool,
    pub bypass: bool,
    pub transition: Transition,
    pub crossfader_bus: Option<Bus>,
    pub group: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deck {
    pub id: DeckId,
    pub name: String,
    /// One clip per (layer, column) cell at most (invariant, see `Deck::clip_at`).
    /// A `Vec<CellClip>` replaces `BTreeMap<(LayerId, ColumnIdx), Clip>` because JSON
    /// object keys must be strings, so a tuple-keyed map cannot be serialized.
    pub clips: Vec<CellClip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    pub source: Source,
    pub transport: Transport,
    pub track: Track,
    /// Overrides the layer blend when set.
    pub blend_override: Option<BlendMode>,
    /// How the video is fitted to the composition frame.
    #[serde(default)]
    pub fit: FitMode,
}

/// Aspect-ratio handling of a clip inside the composition frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FitMode {
    /// Whole video visible, aspect kept; bars are transparent (lower layers show through).
    #[default]
    Fit,
    /// Frame fully covered, aspect kept, overflow cropped.
    Fill,
    /// Frame fully covered, aspect ignored.
    Stretch,
}

impl FitMode {
    pub const ALL: [FitMode; 3] = [FitMode::Fit, FitMode::Fill, FitMode::Stretch];
    pub fn label(self) -> &'static str {
        match self {
            FitMode::Fit => "Fit",
            FitMode::Fill => "Fill",
            FitMode::Stretch => "Stretch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Source {
    File { path: PathBuf },
    Generator { kind: String, params: Vec<Param> },
    Capture { device: String },
    Network { protocol: NetProtocol, name: String },
    LayerRouter { layer: LayerId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NetProtocol {
    Syphon,
    Spout,
    Ndi,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transport {
    pub play_mode: PlayMode,
    pub trigger_mode: TriggerMode,
    pub direction: Direction,
    pub speed: f64,
    /// When set, clip length is stretched to this many beats.
    pub beat_sync: Option<f64>,
    pub cue_points: Vec<f64>,
    pub in_point: f64,
    pub out_point: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayMode {
    Loop,
    Bounce,
    Once,
    OnceHold,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TriggerMode {
    Restart,
    Continue,
    Relative,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Forward,
    Backward,
    Pause,
    Random,
}

/// Shared by composition, layer and clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub opacity: Param,
    pub blend: BlendMode,
    pub effects: Vec<Effect>,
    pub volume: Param,
    pub pan: Param,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BlendMode {
    // Generic compositing and transition modes; see docs/03-catalogo.md.
    Normal,
    Add,
    Burn,
    Darken,
    Difference,
    Dodge,
    Exclusion,
    HardLight,
    Lighten,
    Multiply,
    Overlay,
    Screen,
    SoftLight,
    Subtract,
    Dissolve,
    Cut,
    LumaKey,
    LumaIsAlpha,
    ToBlack,
    ToWhite,
    ToColor,
    PushUp,
    PushDown,
    PushLeft,
    PushRight,
    ZoomIn,
    ZoomOut,
    WipeEllipse,
    RotateX,
    RotateY,
    SideBySide,
    PictureInPicture,
    Rgb,
    BlackWhite,
    Alpha,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub id: EffectId,
    /// ISF shader id or built-in id.
    pub kind: String,
    pub bypass: bool,
    pub dry_wet: f32,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: String,
    pub duration_beats: Option<f64>,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Bus {
    A,
    B,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crossfader {
    pub position: Param,
    pub curve: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub value: Value,
    pub default: Value,
    /// Invariant: min <= value <= max (clamped by the setter).
    pub range: Option<(f64, f64)>,
    pub animation: Option<Animation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Bool(bool),
    Text(String),
    Number(f64),
    Choice { selected: u32, options: Vec<String> },
    Color([f32; 4]),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Animation {
    Timeline {
        duration_secs: f64,
        curve: Curve,
    },
    Beats {
        beats: f64,
        curve: Curve,
    },
    Audio {
        band: FftBand,
        gain: f32,
        threshold: f32,
    },
    Envelope {
        points: Vec<(f64, f64)>,
        beats: f64,
    },
    Dashboard {
        dial: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Curve {
    Linear,
    Sine,
    Saw,
    Square,
    Random,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FftBand {
    Low,
    LowMid,
    Mid,
    High,
    Bin(u32),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputSetup {
    pub screens: Vec<Screen>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    pub id: Uuid,
    pub name: String,
    pub device: OutputDevice,
    pub width: u32,
    pub height: u32,
    pub slices: Vec<Slice>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OutputDevice {
    Monitor { name: String },
    Window,
    Syphon,
    Ndi,
    ArtNet { ip: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slice {
    pub id: Uuid,
    /// Normalized 0..1 rectangle or polygon on the composition.
    pub input: Vec<(f32, f32)>,
    /// Warp mesh in output pixels, row-major grid.
    pub output_mesh: Vec<(f32, f32)>,
    pub mesh_cols: u32,
    pub mask: Option<Vec<(f32, f32)>>,
    pub soft_edge: Option<SoftEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoftEdge {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
    pub gamma: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    pub id: Uuid,
    pub source: ControlSource,
    /// Address in VJCraft's own space, e.g. "/comp/layer/1/opacity".
    pub target: String,
    pub mode: ShortcutMode,
    pub range: (f64, f64),
    pub invert: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlSource {
    Key {
        code: String,
    },
    MidiNote {
        device: String,
        channel: u8,
        note: u8,
    },
    MidiCc {
        device: String,
        channel: u8,
        cc: u8,
    },
    Osc {
        address: String,
    },
    Dmx {
        universe: u16,
        channel: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ShortcutMode {
    Trigger,
    Toggle,
    Absolute,
    Relative,
}

/// A clip placed in a grid cell. Replaces the tuple-keyed map entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellClip {
    pub layer: LayerId,
    pub column: ColumnIdx,
    pub clip: Clip,
}

impl Deck {
    pub fn clip_at(&self, layer: LayerId, column: ColumnIdx) -> Option<&Clip> {
        self.clips
            .iter()
            .find(|c| c.layer == layer && c.column == column)
            .map(|c| &c.clip)
    }

    /// Inserts or replaces the clip in a cell, keeping the one-clip-per-cell invariant.
    pub fn set_clip(&mut self, layer: LayerId, column: ColumnIdx, clip: Clip) {
        self.clips
            .retain(|c| !(c.layer == layer && c.column == column));
        self.clips.push(CellClip {
            layer,
            column,
            clip,
        });
    }
}

impl Param {
    pub fn number(name: &str, value: f64, range: Option<(f64, f64)>) -> Self {
        Param {
            name: name.into(),
            value: Value::Number(value),
            default: Value::Number(value),
            range,
            animation: None,
        }
    }

    /// Sets a numeric value, clamping to `range` (invariant min <= value <= max).
    pub fn set_number(&mut self, v: f64) {
        let v = match self.range {
            Some((lo, hi)) => v.clamp(lo, hi),
            None => v,
        };
        self.value = Value::Number(v);
    }
}

impl Track {
    pub fn new(blend: BlendMode) -> Self {
        Track {
            opacity: Param::number("Opacity", 1.0, Some((0.0, 1.0))),
            blend,
            effects: vec![],
            volume: Param::number("Volume", 1.0, Some((0.0, 1.0))),
            pan: Param::number("Pan", 0.0, Some((-1.0, 1.0))),
        }
    }
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            play_mode: PlayMode::Loop,
            trigger_mode: TriggerMode::Restart,
            direction: Direction::Forward,
            speed: 1.0,
            beat_sync: None,
            cue_points: vec![],
            in_point: 0.0,
            out_point: 1.0,
        }
    }
}

impl Project {
    /// Minimal empty project with one deck.
    pub fn new(name: &str) -> Self {
        let deck = Deck {
            id: Uuid::new_v4(),
            name: "Deck".into(),
            clips: vec![],
        };
        Project {
            format_version: 1,
            composition: Composition {
                id: Uuid::new_v4(),
                name: name.into(),
                width: 1920,
                height: 1080,
                fps: 60.0,
                tempo: Tempo {
                    bpm: 120.0,
                    link_enabled: false,
                    metronome: false,
                },
                layers: vec![],
                columns: vec![],
                current_deck: deck.id,
                decks: vec![deck],
                master: Track::new(BlendMode::Normal),
                master_speed: 1.0,
                crossfader: None,
                dashboard: vec![],
            },
            output: OutputSetup { screens: vec![] },
            shortcuts: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_json_roundtrip() {
        let mut p = Project::new("t");
        let lid = Uuid::new_v4();
        p.composition.layers.push(Layer {
            id: lid,
            name: "L1".into(),
            track: Track::new(BlendMode::Add),
            solo: false,
            bypass: false,
            transition: Transition {
                kind: "Alpha".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            crossfader_bus: None,
            group: None,
        });
        p.composition.decks[0].set_clip(
            lid,
            2,
            Clip {
                id: Uuid::new_v4(),
                name: "c".into(),
                source: Source::File {
                    path: "a.mov".into(),
                },
                transport: Transport::default(),
                track: Track::new(BlendMode::Normal),
                fit: FitMode::default(),
                blend_override: None,
            },
        );
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        assert!(back.composition.decks[0].clip_at(lid, 2).is_some());
    }

    #[test]
    fn unknown_blend_tolerated() {
        let b: BlendMode = serde_json::from_str("\"Future\"").unwrap();
        assert_eq!(b, BlendMode::Unknown);
    }

    #[test]
    fn param_clamps() {
        let mut p = Param::number("x", 0.5, Some((0.0, 1.0)));
        p.set_number(3.0);
        assert_eq!(p.value, Value::Number(1.0));
    }
}
