//! VJCraft project data model (format_version 1).
//! Serialized as JSON (`.vjc`). Clean-room design; see replica/architecture.md.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub type LayerId = Uuid;
pub type GroupId = Uuid;
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

fn yes() -> bool {
    true
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
    /// Layer groups, ordered top to bottom like `layers`. A group's layers are adjacent
    /// and point back through `Layer::group`; groups never nest.
    #[serde(default)]
    pub groups: Vec<LayerGroup>,
    /// Multiplies the speed of every clip, 0..=4.
    #[serde(default = "one")]
    pub master_speed: f64,
    pub crossfader: Option<Crossfader>,
    pub dashboard: Vec<Param>,
    /// Timecode cues, fired when the external timecode crosses them.
    #[serde(default)]
    pub timecode_cues: Vec<TimecodeCue>,
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
    #[serde(default)]
    pub autopilot: Autopilot,
    pub crossfader_bus: Option<Bus>,
    pub group: Option<Uuid>,
    /// Audio of the layer's clips is heard (off = muted, independent of volume).
    #[serde(default = "yes")]
    pub audio: bool,
    /// The layer's picture is drawn (off = hidden, audio unaffected).
    #[serde(default = "yes")]
    pub video: bool,
}

/// Adjacent layers composed together first, then blended into the stack as one layer.
/// `track.effects` run over the composed result; `speed` multiplies the clips' speed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerGroup {
    pub id: GroupId,
    pub name: String,
    pub track: Track,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub bypass: bool,
    /// Multiplies the speed of every clip in the group, 0..=4.
    #[serde(default = "one")]
    pub speed: f64,
}

impl LayerGroup {
    pub fn new(name: &str) -> Self {
        LayerGroup {
            id: Uuid::new_v4(),
            name: name.into(),
            track: Track::new(BlendMode::Normal),
            solo: false,
            bypass: false,
            speed: 1.0,
        }
    }
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
    /// Colour channels of the picture that are drawn.
    #[serde(default)]
    pub channels: Channels,
}

/// R/G/B/A visibility of a clip's picture. A channel that is off is zeroed (R, G, B) or
/// ignored (A: the picture becomes opaque).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channels {
    pub r: bool,
    pub g: bool,
    pub b: bool,
    pub a: bool,
}

impl Default for Channels {
    fn default() -> Self {
        Channels {
            r: true,
            g: true,
            b: true,
            a: true,
        }
    }
}

impl Channels {
    /// Shader mask: bit 0..3 set = that channel (R, G, B, A) is OFF.
    pub fn off_mask(self) -> u32 {
        u32::from(!self.r)
            | u32::from(!self.g) << 1
            | u32::from(!self.b) << 2
            | u32::from(!self.a) << 3
    }
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

fn forward() -> Direction {
    Direction::Forward
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transport {
    pub play_mode: PlayMode,
    pub trigger_mode: TriggerMode,
    pub direction: Direction,
    /// Direction to return to when a paused clip resumes (never `Pause`). Projects saved
    /// before it existed open with Forward.
    #[serde(default = "forward")]
    pub resume_direction: Direction,
    pub speed: f64,
    /// When set, clip length is stretched to this many beats.
    pub beat_sync: Option<f64>,
    pub cue_points: Vec<f64>,
    pub in_point: f64,
    pub out_point: f64,
    /// When set, the clip follows the external timecode: position = timecode - this start.
    #[serde(default)]
    pub timecode_start: Option<Timecode>,
}

/// Timecode frame rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TcFps {
    F24,
    F25,
    /// 29.97 fps drop-frame.
    F2997Df,
    F30,
}

impl TcFps {
    pub const ALL: [TcFps; 4] = [TcFps::F24, TcFps::F25, TcFps::F2997Df, TcFps::F30];

    /// Frame numbers per second on the display (30 for 29.97).
    pub fn nominal(self) -> u32 {
        match self {
            TcFps::F24 => 24,
            TcFps::F25 => 25,
            TcFps::F2997Df | TcFps::F30 => 30,
        }
    }

    /// Real frames per second.
    pub fn rate(self) -> f64 {
        match self {
            TcFps::F2997Df => 30000.0 / 1001.0,
            f => f64::from(f.nominal()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TcFps::F24 => "24",
            TcFps::F25 => "25",
            TcFps::F2997Df => "29.97 DF",
            TcFps::F30 => "30",
        }
    }
}

/// A SMPTE timecode value (hours:minutes:seconds:frames at a frame rate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timecode {
    pub h: u8,
    pub m: u8,
    pub s: u8,
    pub f: u8,
    pub fps: TcFps,
}

/// Fires a column (or one layer's clip of it) when the timecode crosses `time`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimecodeCue {
    pub time: Timecode,
    pub column: ColumnIdx,
    /// None = the whole column, Some = just that layer.
    #[serde(default)]
    pub layer: Option<LayerId>,
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
    /// Reframing of the picture; projects saved before it existed open with identity.
    #[serde(default)]
    pub transform: Transform,
}

/// Position, scale, rotation and anchor of a picture, all animatable params.
/// Pixels are composition pixels; (0, 0) is the frame centre, y grows downwards.
/// Effective scale on each axis is `scale * scale_w` (x) and `scale * scale_h` (y).
/// `anchor` is the pivot of scale and rotation, relative to the picture centre; it does
/// not move the picture by itself. `rotation` is in degrees, clockwise on screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub x: Param,
    pub y: Param,
    pub scale: Param,
    pub scale_w: Param,
    pub scale_h: Param,
    pub rotation: Param,
    pub anchor_x: Param,
    pub anchor_y: Param,
}

/// Largest position / anchor offset in px (the composition size limit).
pub const TRANSFORM_PX_MAX: f64 = 16384.0;
/// Largest scale factor.
pub const TRANSFORM_SCALE_MAX: f64 = 10.0;
/// Rotation limit in degrees (both signs).
pub const TRANSFORM_ROT_MAX: f64 = 360.0;

impl Default for Transform {
    fn default() -> Self {
        let px = Some((-TRANSFORM_PX_MAX, TRANSFORM_PX_MAX));
        let sc = Some((0.0, TRANSFORM_SCALE_MAX));
        Transform {
            x: Param::number("Position X", 0.0, px),
            y: Param::number("Position Y", 0.0, px),
            scale: Param::number("Scale", 1.0, sc),
            scale_w: Param::number("Scale W", 1.0, sc),
            scale_h: Param::number("Scale H", 1.0, sc),
            rotation: Param::number(
                "Rotation",
                0.0,
                Some((-TRANSFORM_ROT_MAX, TRANSFORM_ROT_MAX)),
            ),
            anchor_x: Param::number("Anchor X", 0.0, px),
            anchor_y: Param::number("Anchor Y", 0.0, px),
        }
    }
}

/// One field of a `Transform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransformField {
    X,
    Y,
    Scale,
    ScaleW,
    ScaleH,
    Rotation,
    AnchorX,
    AnchorY,
}

impl TransformField {
    pub const ALL: [TransformField; 8] = [
        TransformField::X,
        TransformField::Y,
        TransformField::Scale,
        TransformField::ScaleW,
        TransformField::ScaleH,
        TransformField::Rotation,
        TransformField::AnchorX,
        TransformField::AnchorY,
    ];

    /// Last segment of the `/comp/.../transform/<name>` address.
    pub fn address(self) -> &'static str {
        match self {
            TransformField::X => "x",
            TransformField::Y => "y",
            TransformField::Scale => "scale",
            TransformField::ScaleW => "scale_w",
            TransformField::ScaleH => "scale_h",
            TransformField::Rotation => "rotation",
            TransformField::AnchorX => "anchor_x",
            TransformField::AnchorY => "anchor_y",
        }
    }

    pub fn from_address(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.address() == s)
    }
}

impl Transform {
    pub fn field(&self, f: TransformField) -> &Param {
        match f {
            TransformField::X => &self.x,
            TransformField::Y => &self.y,
            TransformField::Scale => &self.scale,
            TransformField::ScaleW => &self.scale_w,
            TransformField::ScaleH => &self.scale_h,
            TransformField::Rotation => &self.rotation,
            TransformField::AnchorX => &self.anchor_x,
            TransformField::AnchorY => &self.anchor_y,
        }
    }

    pub fn field_mut(&mut self, f: TransformField) -> &mut Param {
        match f {
            TransformField::X => &mut self.x,
            TransformField::Y => &mut self.y,
            TransformField::Scale => &mut self.scale,
            TransformField::ScaleW => &mut self.scale_w,
            TransformField::ScaleH => &mut self.scale_h,
            TransformField::Rotation => &mut self.rotation,
            TransformField::AnchorX => &mut self.anchor_x,
            TransformField::AnchorY => &mut self.anchor_y,
        }
    }
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

/// What a layer plays when its clip ends (Once) or after `every_loops` loops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Autopilot {
    pub mode: AutopilotMode,
    /// Loop-based modes: switch after this many completed loops (min 1).
    pub every_loops: u32,
}

impl Default for Autopilot {
    fn default() -> Self {
        Autopilot {
            mode: AutopilotMode::Off,
            every_loops: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutopilotMode {
    Off,
    Next,
    Random,
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
    /// Fade = gains follow the curve; Cut = hard switch at the middle (ignores the curve).
    #[serde(default)]
    pub behaviour: CrossfadeBehaviour,
    /// Blend mode used by the layers on a bus instead of their own; `None` = their own.
    #[serde(default)]
    pub blend: Option<BlendMode>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrossfadeBehaviour {
    #[default]
    Fade,
    Cut,
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
    /// Slices that are not scaled sample with nearest (no filtering) and the output
    /// window presents the canvas 1:1 in physical pixels.
    #[serde(default)]
    pub pixel_perfect: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OutputDevice {
    Monitor {
        name: String,
    },
    Window,
    Syphon,
    Ndi,
    /// Art-Net: ArtDmx sent to `ip` ("host" or "host:port", default port 6454), the pixels
    /// of the screen sampled on the fixture grid, from `universe` upward.
    ArtNet {
        ip: String,
        #[serde(default)]
        universe: u16,
        #[serde(default)]
        fixture: Fixture,
    },
}

/// Channels of one fixture pixel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    #[default]
    Rgb,
    Rgbw,
    /// Single white channel (luminance).
    W,
}

/// Order of the colour channels in a pixel (RGBW keeps W last).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelOrder {
    #[default]
    Rgb,
    Rbg,
    Grb,
    Gbr,
    Brg,
    Bgr,
}

/// A DMX pixel fixture: a `width` x `height` grid, row-major from the top-left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fixture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub order: ChannelOrder,
    /// Output = input^gamma on 0..1 (1.0 = off, 2.2 typical for LEDs).
    pub gamma: f32,
    /// Odd rows run right to left (zig-zag wired matrices).
    pub serpentine: bool,
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture {
            name: "Strip 1x16 RGB".into(),
            width: 16,
            height: 1,
            format: PixelFormat::Rgb,
            order: ChannelOrder::Rgb,
            gamma: 1.0,
            serpentine: false,
        }
    }
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
    /// What the input rectangle samples. Projects saved before routing existed have no
    /// field and load as the whole composition.
    #[serde(default)]
    pub source: SliceSource,
}

/// Where a slice takes its picture from (before the master chain).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SliceSource {
    /// The finished composition.
    #[default]
    Composition,
    /// One layer on its own (its opacity and chain, nothing below or above it).
    Layer(LayerId),
    /// One layer group on its own (its layers, group chain and opacity).
    Group(GroupId),
    /// The preview monitor: the selected layer on its own; the composition when nothing
    /// is selected.
    Preview,
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
    /// Stable id of the effect an effect address points at; the positional `target` is
    /// recomputed from it when the chain is reordered, and the shortcut is orphaned when
    /// the effect goes away.
    #[serde(default)]
    pub effect: Option<EffectId>,
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
            transform: Transform::default(),
        }
    }
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            play_mode: PlayMode::Loop,
            trigger_mode: TriggerMode::Restart,
            direction: Direction::Forward,
            resume_direction: Direction::Forward,
            speed: 1.0,
            beat_sync: None,
            cue_points: vec![],
            in_point: 0.0,
            out_point: 1.0,
            timecode_start: None,
        }
    }
}

impl Transport {
    /// Direction a clip takes when it leaves Pause.
    pub fn resumed_direction(&self) -> Direction {
        match self.resume_direction {
            Direction::Pause => Direction::Forward,
            d => d,
        }
    }

    /// Sets `direction`, remembering the one left when entering Pause so a resume restores it.
    pub fn set_direction(&mut self, direction: Direction) {
        if direction != Direction::Pause {
            self.resume_direction = direction;
        } else if self.direction != Direction::Pause {
            self.resume_direction = self.direction;
        }
        self.direction = direction;
    }
}

impl Project {
    /// Pause is a live state, not project data: every paused clip goes back to the direction
    /// it had before. Returns how many clips changed.
    pub fn resume_paused(&mut self) -> usize {
        let mut n = 0;
        for k in self
            .composition
            .decks
            .iter_mut()
            .flat_map(|d| d.clips.iter_mut())
        {
            let t = &mut k.clip.transport;
            if t.direction == Direction::Pause {
                t.direction = t.resumed_direction();
                n += 1;
            }
        }
        n
    }

    /// Enforces one monitor per screen: a screen that names a monitor an earlier screen
    /// already uses falls back to a window. Returns how many screens were changed.
    pub fn normalize_screens(&mut self) -> usize {
        let mut seen = std::collections::HashSet::new();
        let mut changed = 0;
        for s in &mut self.output.screens {
            if let OutputDevice::Monitor { name } = &s.device
                && !seen.insert(name.clone())
            {
                s.device = OutputDevice::Window;
                changed += 1;
            }
        }
        changed
    }

    /// Repairs groups from a file: a layer naming a group that does not exist, or a group
    /// whose layers are not adjacent (effects would apply twice), is ungrouped. Returns how
    /// many layers were changed.
    pub fn normalize_groups(&mut self) -> usize {
        let c = &mut self.composition;
        let mut changed = 0;
        // Groups already empty in the file keep their speed/opacity; only groups emptied
        // by the repair below are dropped.
        let empty_in_file: std::collections::HashSet<GroupId> = c
            .groups
            .iter()
            .map(|g| g.id)
            .filter(|id| !c.layers.iter().any(|l| l.group == Some(*id)))
            .collect();
        for l in &mut c.layers {
            if l.group.is_some_and(|g| !c.groups.iter().any(|x| x.id == g)) {
                l.group = None;
                changed += 1;
            }
        }
        for g in &c.groups {
            let idx: Vec<usize> = (0..c.layers.len())
                .filter(|&i| c.layers[i].group == Some(g.id))
                .collect();
            if idx
                .last()
                .zip(idx.first())
                .is_some_and(|(l, f)| l - f + 1 != idx.len())
            {
                for i in idx {
                    c.layers[i].group = None;
                    changed += 1;
                }
            }
        }
        // A group left without members by the repair above is dropped.
        let used: std::collections::HashSet<GroupId> =
            c.layers.iter().filter_map(|l| l.group).collect();
        c.groups
            .retain(|g| used.contains(&g.id) || empty_in_file.contains(&g.id));
        changed
    }

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
                groups: vec![],
                master_speed: 1.0,
                crossfader: None,
                dashboard: vec![],
                timecode_cues: vec![],
            },
            output: OutputSetup { screens: vec![] },
            shortcuts: vec![],
        }
    }
}

impl Composition {
    /// True when the layer is not rendered: bypassed, or another layer is soloed.
    pub fn layer_off(&self, l: &Layer) -> bool {
        let g = l.group.and_then(|id| self.group(id));
        l.bypass
            || g.is_some_and(|g| g.bypass)
            || (!l.solo && !g.is_some_and(|g| g.solo) && self.any_solo())
    }

    pub fn group(&self, id: GroupId) -> Option<&LayerGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    fn any_solo(&self) -> bool {
        self.layers.iter().any(|o| o.solo) || self.groups.iter().any(|g| g.solo)
    }

    /// True when the whole group is not rendered: bypassed, or something else is soloed
    /// (a soloed layer inside the group keeps it on).
    pub fn group_off(&self, g: &LayerGroup) -> bool {
        g.bypass
            || (!g.solo
                && self.any_solo()
                && !self.layers.iter().any(|l| l.solo && l.group == Some(g.id)))
    }

    /// Speed multiplier a layer's clips get from its group (1 when ungrouped).
    pub fn group_speed(&self, l: &Layer) -> f64 {
        l.group
            .and_then(|id| self.group(id))
            .map_or(1.0, |g| g.speed)
    }
}

impl Timecode {
    pub fn new(h: u8, m: u8, s: u8, f: u8, fps: TcFps) -> Timecode {
        Timecode { h, m, s, f, fps }
    }

    /// Frames in one day at this rate (the wrap point).
    fn day_frames(fps: TcFps) -> u64 {
        match fps {
            // 30 x 86400 minus 2 per minute except every tenth.
            TcFps::F2997Df => 2_589_408,
            f => u64::from(f.nominal()) * 86_400,
        }
    }

    /// Frames since 00:00:00:00 (drop-frame numbers skipped).
    pub fn frames(&self) -> u64 {
        let nom = u64::from(self.fps.nominal());
        let (h, m, s, f) = (
            u64::from(self.h),
            u64::from(self.m),
            u64::from(self.s),
            u64::from(self.f),
        );
        let n = nom * (3600 * h + 60 * m + s) + f;
        if self.fps == TcFps::F2997Df {
            let tm = 60 * h + m;
            n.saturating_sub(2 * (tm - tm / 10))
        } else {
            n
        }
    }

    /// Timecode `n` frames after 00:00:00:00, wrapping at 24 h.
    pub fn from_frames(fps: TcFps, n: u64) -> Timecode {
        let mut n = n % Self::day_frames(fps);
        if fps == TcFps::F2997Df {
            let (d, r) = (n / 17_982, n % 17_982);
            n += 18 * d + if r < 2 { 0 } else { 2 * ((r - 2) / 1798) };
        }
        let nom = u64::from(fps.nominal());
        Timecode {
            f: (n % nom) as u8,
            s: (n / nom % 60) as u8,
            m: (n / (nom * 60) % 60) as u8,
            h: (n / (nom * 3600) % 24) as u8,
            fps,
        }
    }

    /// Adds (or subtracts) frames, wrapping at 24 h.
    pub fn add_frames(&self, delta: i64) -> Timecode {
        let day = Self::day_frames(self.fps) as i64;
        let n = (self.frames() as i64 + delta).rem_euclid(day);
        Timecode::from_frames(self.fps, n as u64)
    }

    /// Wall-clock seconds since 00:00:00:00.
    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.fps.rate()
    }

    /// Nearest frame to `secs` (wraps at 24 h; negative clamps to 0).
    pub fn from_seconds(fps: TcFps, secs: f64) -> Timecode {
        let n = if secs.is_finite() {
            (secs.max(0.0) * fps.rate()).round() as u64
        } else {
            0
        };
        Timecode::from_frames(fps, n)
    }

    /// Fields in range; drop-frame skips frames 0 and 1 at minutes not divisible by 10.
    pub fn is_valid(&self) -> bool {
        self.h < 24
            && self.m < 60
            && self.s < 60
            && u32::from(self.f) < self.fps.nominal()
            && !(self.fps == TcFps::F2997Df
                && self.s == 0
                && self.f < 2
                && !self.m.is_multiple_of(10))
    }
}

impl std::fmt::Display for Timecode {
    fn fmt(&self, w: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sep = if self.fps == TcFps::F2997Df { ';' } else { ':' };
        write!(
            w,
            "{:02}:{:02}:{:02}{sep}{:02}",
            self.h, self.m, self.s, self.f
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_turns_other_layers_off() {
        let mut p = Project::new("t");
        let mk = |solo, bypass| Layer {
            id: Uuid::new_v4(),
            name: "L".into(),
            track: Track::new(BlendMode::Normal),
            solo,
            bypass,
            crossfader_bus: None,
            audio: true,
            video: true,
            transition: Transition {
                kind: "cut".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            autopilot: Default::default(),
            group: None,
        };
        p.composition.layers = vec![mk(false, false), mk(false, false)];
        let c = &p.composition;
        assert!(!c.layer_off(&c.layers[0]) && !c.layer_off(&c.layers[1]));
        p.composition.layers[1].solo = true;
        let c = &p.composition;
        assert!(c.layer_off(&c.layers[0]) && !c.layer_off(&c.layers[1]));
    }

    #[test]
    fn text_param_serde_roundtrip() {
        let prm = Param {
            name: "message".into(),
            value: Value::Text("OLÁ \"VJ\"".into()),
            default: Value::Text("A".into()),
            range: None,
            animation: None,
        };
        let json = serde_json::to_string(&prm).unwrap();
        assert_eq!(serde_json::from_str::<Param>(&json).unwrap(), prm);
        // Negative control: a Text is not read back as another variant.
        let num: Value =
            serde_json::from_str(&serde_json::to_string(&Value::Number(1.0)).unwrap()).unwrap();
        assert_ne!(num, Value::Text("1".into()));
    }

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
            autopilot: Default::default(),
            crossfader_bus: None,
            audio: true,
            video: true,
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
                channels: Default::default(),
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

    #[test]
    fn old_projects_without_timecode_fields_load() {
        let mut v = serde_json::to_value(Project::new("x")).unwrap();
        v["composition"]
            .as_object_mut()
            .unwrap()
            .remove("timecode_cues");
        let p: Project = serde_json::from_value(v).unwrap();
        assert!(p.composition.timecode_cues.is_empty());
        let mut t = serde_json::to_value(Transport::default()).unwrap();
        t.as_object_mut().unwrap().remove("timecode_start");
        let t: Transport = serde_json::from_value(t).unwrap();
        assert_eq!(t.timecode_start, None);
    }
}
