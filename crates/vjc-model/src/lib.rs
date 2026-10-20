//! VJCraft project data model (format_version 1).
//! Serialized as JSON (`.vjc`). Clean-room design.

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
    /// Plays every clip against its own direction (Forward <-> Backward). Old files: off.
    #[serde(default)]
    pub master_reverse: bool,
    pub crossfader: Option<Crossfader>,
    pub dashboard: Vec<Param>,
    /// Timecode cues, fired when the external timecode crosses them.
    #[serde(default)]
    pub timecode_cues: Vec<TimecodeCue>,
    /// Default trigger style of clips that defer to the composition.
    #[serde(default)]
    pub trigger_style: TriggerStyle,
    /// Plays columns by itself. Old files lack it: Off.
    #[serde(default)]
    pub autopilot: CompAutopilot,
    /// Beat snap of the composition. None (old files): the app preference decides.
    #[serde(default)]
    pub beat_snap: Option<BeatSnap>,
    /// Default destination layer of clips that defer to the composition.
    #[serde(default)]
    pub clip_target: ClipTarget,
    /// Free-text note about the composition.
    #[serde(default)]
    pub description: String,
    /// Bits per channel of the composition's render textures: 8 or 16. Old files: 8.
    #[serde(default = "default_bit_depth")]
    pub bit_depth: u8,
}

/// Bit depths a composition may use.
pub const BIT_DEPTHS: [u8; 2] = [8, 16];

fn default_bit_depth() -> u8 {
    8
}

/// Which column the composition autopilot plays when its time is up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnTarget {
    #[default]
    Off,
    Next,
    Previous,
    Random,
}

/// What the composition autopilot waits for before it plays the next column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnDuration {
    #[default]
    Beats,
    Seconds,
    ShortestClip,
    LongestClip,
    TopClip,
    BottomClip,
}

/// How a random pick is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RandomKind {
    /// Any column, the current one included.
    #[default]
    Any,
    /// Any column but the current one.
    Other,
    /// Shuffled: no column repeats until all have played.
    Bag,
}

fn default_ap_beats() -> f64 {
    4.0
}

fn default_ap_secs() -> f64 {
    10.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompAutopilot {
    pub target: ColumnTarget,
    pub duration: ColumnDuration,
    pub beats: f64,
    pub seconds: f64,
    pub random: RandomKind,
    /// Next/Previous wrap around at the ends when on; off stops there.
    #[serde(rename = "loop")]
    pub looping: bool,
    /// Fire the next column early by the longest layer transition, so the crossing ends
    /// when the hold time does. Old files lack it (off).
    pub compensate_transition: bool,
}

impl Default for CompAutopilot {
    fn default() -> Self {
        CompAutopilot {
            target: ColumnTarget::Off,
            duration: ColumnDuration::Beats,
            beats: default_ap_beats(),
            seconds: default_ap_secs(),
            random: RandomKind::Any,
            looping: true,
            compensate_transition: false,
        }
    }
}

/// Per-column override of the composition autopilot; `None` defers to the composition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColumnAutopilot {
    /// What plays after this column. `Some(Off)` stops here.
    pub target: Option<ColumnTarget>,
    pub duration: Option<ColumnDuration>,
    pub beats: f64,
    pub seconds: f64,
}

impl Default for ColumnAutopilot {
    fn default() -> Self {
        ColumnAutopilot {
            target: None,
            duration: None,
            beats: default_ap_beats(),
            seconds: default_ap_secs(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tempo {
    pub bpm: f64,
    pub link_enabled: bool,
    pub metronome: bool,
    /// Transport of the tempo clock. Old files lack it and play.
    #[serde(default)]
    pub play_state: PlayState,
}

/// Play/Pause/Stop of the tempo clock: Pause freezes the phase, Stop zeroes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayState {
    #[default]
    Play,
    Pause,
    Stop,
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
    /// Trigger mode of clips that defer to the layer (never `Layer` itself).
    #[serde(default = "restart")]
    pub trigger_mode: TriggerMode,
    /// A column trigger skips this layer (its playing clip is left alone).
    #[serde(default)]
    pub ignore_column_trigger: bool,
    /// Content lock: trigger, eject, drop and clear are ignored on this layer.
    #[serde(default)]
    pub lock_content: bool,
    /// Raising the opacity from 0 restarts the playing clip from its start.
    #[serde(default)]
    pub fader_start: bool,
    /// Uses this layer's luminance as a mask for the layers below.
    #[serde(default)]
    pub mask_mode: MaskMode,
    /// Palette index tinting the layer header (None = no colour).
    #[serde(default)]
    pub color: Option<u8>,
    /// Collapsed in the grid UI.
    #[serde(default)]
    pub folded: bool,
    /// Layer master and sizing; old files open with master 1 and the clip's own fit.
    #[serde(default)]
    pub extras: LayerExtras,
}

/// Largest layer width or height, composition px.
pub const MAX_LAYER_SIDE: u32 = 16_384;

/// What the layer adds on top of its clips: a master level separate from the video
/// opacity, and its own picture size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerExtras {
    /// Layer master (M) 0..=1: scales picture and audio together; the video opacity (V) in
    /// `Track::opacity` only fades the picture.
    pub master: f64,
    /// Fit applied to every clip of the layer; `Off` keeps each clip's own fit.
    pub auto_size: AutoSize,
    /// Size of the layer's picture area in composition px; 0 = the composition's.
    pub width: u32,
    pub height: u32,
}

impl Default for LayerExtras {
    fn default() -> Self {
        LayerExtras {
            master: 1.0,
            auto_size: AutoSize::Off,
            width: 0,
            height: 0,
        }
    }
}

/// Layer auto-size: how the layer fits its clips into its picture area.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoSize {
    /// Each clip keeps its own fit mode.
    #[default]
    Off,
    Fill,
    Fit,
    Stretch,
}

impl AutoSize {
    pub const ALL: [AutoSize; 4] = [
        AutoSize::Off,
        AutoSize::Fill,
        AutoSize::Fit,
        AutoSize::Stretch,
    ];

    /// The fit this forces, or `None` for Off.
    pub fn fit(self) -> Option<FitMode> {
        match self {
            AutoSize::Off => None,
            AutoSize::Fill => Some(FitMode::Fill),
            AutoSize::Fit => Some(FitMode::Fit),
            AutoSize::Stretch => Some(FitMode::Stretch),
        }
    }
}

/// What a layer masks: its luminance multiplies the layer(s) below it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskMode {
    #[default]
    Off,
    /// Masks only the layer right below.
    OneBelow,
    /// Masks everything below.
    AllBelow,
}

impl MaskMode {
    pub const ALL: [MaskMode; 3] = [MaskMode::Off, MaskMode::OneBelow, MaskMode::AllBelow];
    pub fn label(self) -> &'static str {
        match self {
            MaskMode::Off => "Off",
            MaskMode::OneBelow => "One below",
            MaskMode::AllBelow => "All below",
        }
    }
}

fn restart() -> TriggerMode {
    TriggerMode::Restart
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
    /// Collapsed in the grid UI (its layers are hidden).
    #[serde(default)]
    pub folded: bool,
    /// Crossfader bus of the group; layers without a bus of their own follow it.
    #[serde(default)]
    pub crossfader_bus: Option<Bus>,
    /// Group-level mask: the composed group is not drawn, its luminance masks the
    /// ungrouped layers below it.
    #[serde(default)]
    pub mask_mode: MaskMode,
    /// A column trigger skips every layer of the group.
    #[serde(default)]
    pub ignore_column_trigger: bool,
    /// Content lock for every layer of the group.
    #[serde(default)]
    pub lock_content: bool,
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
            folded: false,
            crossfader_bus: None,
            mask_mode: MaskMode::Off,
            ignore_column_trigger: false,
            lock_content: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub id: Uuid,
    /// Header label; older projects may lack it.
    #[serde(default)]
    pub name: String,
    /// Autopilot override of this column. Old files lack it: defers to the composition.
    #[serde(default)]
    pub autopilot: ColumnAutopilot,
    /// Beat snap override of this column (None = the composition's).
    #[serde(default)]
    pub beat_snap: Option<BeatSnap>,
    /// Palette index (1..=PALETTE_LEN) tinting the header; None = no colour.
    #[serde(default)]
    pub color: Option<u8>,
}

/// Number of colours in the chip palette (Arena colorid 1-6).
pub const PALETTE_LEN: u8 = 6;

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
    /// Overrides the layer blend when set (None = layer determined).
    #[serde(default)]
    pub blend_override: Option<BlendMode>,
    /// Transition used when this clip is triggered; wins over the layer's (None = layer's).
    #[serde(default)]
    pub transition: Option<Transition>,
    /// How the video is fitted to the composition frame.
    #[serde(default)]
    pub fit: FitMode,
    /// Colour channels of the picture that are drawn.
    #[serde(default)]
    pub channels: Channels,
    /// Normal / toggle / hold; `Composition` defers to the composition's style.
    #[serde(default)]
    pub trigger_style: TriggerStyle,
    /// Beat snap override of this clip (None = the column's, then the composition's).
    #[serde(default)]
    pub beat_snap: Option<BeatSnap>,
    /// Which layer the clip plays on; `Composition` defers to the composition's target.
    #[serde(default)]
    pub target: ClipTarget,
    /// Per-clip colour, trigger/fader overrides and strip toggles.
    #[serde(default)]
    pub extras: ClipExtras,
}

/// Per-clip settings that are rarely touched. `None` overrides defer to the layer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipExtras {
    /// Palette index tinting the cell (None = no colour).
    pub color: Option<u8>,
    /// Column triggers skip this clip's layer when it plays this clip (None = layer's).
    pub ignore_column_trigger: Option<bool>,
    /// Fader start for this clip (None = layer's).
    pub fader_start: Option<bool>,
    /// Audio track removed from playback.
    pub strip_audio: bool,
    /// Video track removed from playback (picture not drawn).
    pub strip_video: bool,
    /// Clip's own autopilot (target and loops); None = the layer's.
    pub autopilot: Option<Autopilot>,
    /// The picture holds its current frame while the transport keeps running.
    pub freeze: bool,
    /// Stop-motion: the picture refreshes at this many fps (0 = off).
    pub stop_motion_fps: f64,
}

/// How a trigger behaves over time (not where the clip starts: see `TriggerMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TriggerStyle {
    /// Use the composition's style (for a composition itself: Normal).
    #[default]
    Composition,
    /// Plays until something else replaces it.
    Normal,
    /// A second trigger of the clip that is playing ejects the layer.
    Toggle,
    /// Plays while the mouse button or key is held; ejects on release.
    Hold,
    #[serde(other)]
    Unknown,
}

impl TriggerStyle {
    pub const CHOICES: [TriggerStyle; 3] = [
        TriggerStyle::Normal,
        TriggerStyle::Toggle,
        TriggerStyle::Hold,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TriggerStyle::Composition => "Composition",
            TriggerStyle::Normal | TriggerStyle::Unknown => "Normal",
            TriggerStyle::Toggle => "Toggle",
            TriggerStyle::Hold => "Hold",
        }
    }

    /// The style that applies: the clip's own, or the composition's when it defers.
    pub fn resolve(self, comp: TriggerStyle) -> TriggerStyle {
        let m = if self == TriggerStyle::Composition {
            comp
        } else {
            self
        };
        match m {
            TriggerStyle::Toggle | TriggerStyle::Hold => m,
            _ => TriggerStyle::Normal,
        }
    }
}

/// Which layer a triggered clip plays on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ClipTarget {
    /// Use the composition's target (for a composition itself: Own).
    #[default]
    Composition,
    /// The layer the clip lives in.
    Own,
    /// The selected layer.
    Active,
    /// The lowest empty layer (the clip's own when none is empty).
    Free,
    #[serde(other)]
    Unknown,
}

impl ClipTarget {
    pub const CHOICES: [ClipTarget; 3] = [ClipTarget::Own, ClipTarget::Active, ClipTarget::Free];

    pub fn label(self) -> &'static str {
        match self {
            ClipTarget::Composition => "Composition",
            ClipTarget::Own | ClipTarget::Unknown => "Own layer",
            ClipTarget::Active => "Active layer",
            ClipTarget::Free => "Free layer",
        }
    }

    pub fn resolve(self, comp: ClipTarget) -> ClipTarget {
        let m = if self == ClipTarget::Composition {
            comp
        } else {
            self
        };
        match m {
            ClipTarget::Active | ClipTarget::Free => m,
            _ => ClipTarget::Own,
        }
    }
}

/// R/G/B/A visibility of a clip's picture. A channel that is off is zeroed (R, G, B) or
/// ignored (A: the picture becomes opaque).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channels {
    pub r: bool,
    pub g: bool,
    pub b: bool,
    pub a: bool,
    /// Alpha type of the source: its colour is already multiplied by alpha (premultiplied)
    /// instead of straight. Old files lack it: straight.
    #[serde(default)]
    pub premultiplied: bool,
}

impl Default for Channels {
    fn default() -> Self {
        Channels {
            r: true,
            g: true,
            b: true,
            a: true,
            premultiplied: false,
        }
    }
}

impl Channels {
    /// Shader mask: bit 0..3 set = that channel (R, G, B, A) is OFF; bit 4 = premultiplied.
    pub fn off_mask(self) -> u32 {
        u32::from(!self.r)
            | u32::from(!self.g) << 1
            | u32::from(!self.b) << 2
            | u32::from(!self.a) << 3
            | u32::from(self.premultiplied) << 4
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
    /// Native size in px, centred (cropped if larger than the frame).
    Original,
}

impl FitMode {
    pub const ALL: [FitMode; 4] = [
        FitMode::Fit,
        FitMode::Fill,
        FitMode::Stretch,
        FitMode::Original,
    ];
    pub fn label(self) -> &'static str {
        match self {
            FitMode::Fit => "Fit",
            FitMode::Fill => "Fill",
            FitMode::Stretch => "Stretch",
            FitMode::Original => "Original",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Source {
    File {
        path: PathBuf,
    },
    Generator {
        kind: String,
        params: Vec<Param>,
    },
    /// Numbered image files played as one clip: `pattern` is `dir/prefix%04d.ext`.
    Sequence {
        pattern: PathBuf,
        fps: f32,
    },
    Capture {
        device: String,
    },
    Network {
        protocol: NetProtocol,
        name: String,
    },
    LayerRouter {
        layer: LayerId,
    },
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
    #[serde(default)]
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
    /// When set (and no beat sync), the in..out region plays in this many seconds, i.e. the
    /// speed is derived from it. Old files lack it: free speed.
    #[serde(default)]
    pub duration_secs: Option<f64>,
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
    /// Plays forward like Loop; each time it reaches the end it jumps to a random position.
    Random,
    #[serde(other)]
    Unknown,
}

/// Where a clip starts when it is triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TriggerMode {
    /// Start from the in-point.
    Restart,
    /// Resume where the clip left off.
    Continue,
    /// Start at the same fraction of the region the layer's previous clip had reached.
    Relative,
    /// Use the layer's default (`Layer::trigger_mode`).
    #[default]
    Layer,
    #[serde(other)]
    Unknown,
}

impl TriggerMode {
    /// Modes a clip can pick (the layer default cannot itself be `Layer`).
    pub const LAYER_CHOICES: [TriggerMode; 3] = [
        TriggerMode::Restart,
        TriggerMode::Continue,
        TriggerMode::Relative,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TriggerMode::Restart => "Restart",
            TriggerMode::Continue => "Continue",
            TriggerMode::Relative => "Relative",
            TriggerMode::Layer => "Layer default",
            TriggerMode::Unknown => "Restart",
        }
    }

    /// The mode that applies: the clip's own, or the layer's when the clip defers.
    /// `Layer` and unknown values fall back to Restart.
    pub fn resolve(self, layer_default: TriggerMode) -> TriggerMode {
        let m = if self == TriggerMode::Layer {
            layer_default
        } else {
            self
        };
        match m {
            TriggerMode::Restart | TriggerMode::Continue | TriggerMode::Relative => m,
            _ => TriggerMode::Restart,
        }
    }
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
    /// Audio effects (`vjc-media` `audio_fx` kinds), in processing order. Old files: none.
    #[serde(default)]
    pub audio_effects: Vec<Effect>,
    pub volume: Param,
    pub pan: Param,
    /// Reframing of the picture; projects saved before it existed open with identity.
    #[serde(default)]
    pub transform: Transform,
    /// Dashboard dials of a layer or clip (empty until first used; the composition keeps
    /// its own in `Composition::dashboard`). 0..=1 numbers.
    #[serde(default)]
    pub dials: Vec<Param>,
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
    /// Degrees about the horizontal axis (3D); old files lack it: 0.
    #[serde(default = "default_rotation_x")]
    pub rotation_x: Param,
    /// Degrees about the vertical axis (3D); old files lack it: 0.
    #[serde(default = "default_rotation_y")]
    pub rotation_y: Param,
    /// Depth of the pivot, px; old files lack it: 0.
    #[serde(default = "default_anchor_z")]
    pub anchor_z: Param,
}

fn default_rotation_x() -> Param {
    Param::number(
        "Rotation X",
        0.0,
        Some((-TRANSFORM_ROT_MAX, TRANSFORM_ROT_MAX)),
    )
}
fn default_rotation_y() -> Param {
    Param::number(
        "Rotation Y",
        0.0,
        Some((-TRANSFORM_ROT_MAX, TRANSFORM_ROT_MAX)),
    )
}
fn default_anchor_z() -> Param {
    Param::number("Anchor Z", 0.0, Some((-TRANSFORM_PX_MAX, TRANSFORM_PX_MAX)))
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
            rotation_x: default_rotation_x(),
            rotation_y: default_rotation_y(),
            anchor_z: default_anchor_z(),
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
    RotationX,
    RotationY,
    AnchorZ,
}

impl TransformField {
    pub const ALL: [TransformField; 11] = [
        TransformField::X,
        TransformField::Y,
        TransformField::Scale,
        TransformField::ScaleW,
        TransformField::ScaleH,
        TransformField::Rotation,
        TransformField::AnchorX,
        TransformField::AnchorY,
        TransformField::RotationX,
        TransformField::RotationY,
        TransformField::AnchorZ,
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
            TransformField::RotationX => "rotation_x",
            TransformField::RotationY => "rotation_y",
            TransformField::AnchorZ => "anchor_z",
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
            TransformField::RotationX => &self.rotation_x,
            TransformField::RotationY => &self.rotation_y,
            TransformField::AnchorZ => &self.anchor_z,
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
            TransformField::RotationX => &mut self.rotation_x,
            TransformField::RotationY => &mut self.rotation_y,
            TransformField::AnchorZ => &mut self.anchor_z,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum BlendMode {
    // Generic compositing and transition modes; see docs/03-catalogo.md.
    #[default]
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
    // Added later (appended; the serialized names of the modes above never change).
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
    Displace,
    LoRez,
    Add50,
    Burn50,
    Difference50,
    Distance50,
    Dodge50,
    Lighten50,
    Mask50,
    Multiply50,
    Overlay50,
    Subtract50,
    /// 1 - |base - source|: the inverted Difference.
    DifferenceInverse,
    /// Luma Key with the key inverted: bright pixels of the source become transparent.
    LumaKeyInverse,
    /// Source read at a per-pixel random offset (a shaky, torn picture).
    Jitter,
    /// Transition-style blend: the opacity is the progress of a luma wipe that reveals the
    /// source from its darkest pixels to its brightest.
    Bumper,
    /// Brighter source pixels weigh more than the base ones (a luma morph).
    MetaMix,
    /// The source tiled 2 x 2, each tile showing the whole picture.
    MultiTask,
    /// Faceted look: cells drawn as cube corners with three shades.
    Cube,
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
    /// How the effect output is mixed over its input (`dry_wet` is the opacity).
    /// Normal (old files) = plain crossfade.
    #[serde(default)]
    pub blend: BlendMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: String,
    pub duration_beats: Option<f64>,
    pub duration_secs: f64,
}

/// What a layer plays when its clip ends (Once) or after `every_loops` loops, or after
/// `every` beats / seconds when `unit` says so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Autopilot {
    pub mode: AutopilotMode,
    /// Loop-based modes: switch after this many completed loops (min 1).
    pub every_loops: u32,
    /// What `every_loops` / `every` count.
    #[serde(default)]
    pub unit: AutopilotUnit,
    /// Beats or seconds between switches when `unit` is not `Loops`.
    #[serde(default = "default_every")]
    pub every: f64,
    /// Random mode: how the pick is drawn (Any / Other / Bag).
    #[serde(default = "random_other")]
    pub random: RandomKind,
    /// Next/Previous wrap around at the ends when on; off stops there.
    #[serde(default = "yes", rename = "loop")]
    pub looping: bool,
}

fn random_other() -> RandomKind {
    RandomKind::Other
}

fn default_every() -> f64 {
    8.0
}

/// When autopilot switches clip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutopilotUnit {
    /// After `every_loops` completed loops (and when a Once clip ends).
    #[default]
    Loops,
    /// After `every` beats of the app clock since the clip started.
    Beats,
    /// After `every` seconds since the clip started.
    Seconds,
}

impl Default for Autopilot {
    fn default() -> Self {
        Autopilot {
            mode: AutopilotMode::Off,
            every_loops: 1,
            unit: AutopilotUnit::Loops,
            every: default_every(),
            random: RandomKind::Other,
            looping: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutopilotMode {
    Off,
    Next,
    Random,
    /// The playable clip before the current one (wraps).
    Previous,
    /// Alternates between the first and the last playable clip.
    FirstLast,
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
    /// Hard switch like Cut, and the Go commands hold the far bus only while pressed.
    CutReturn,
    /// Hard switch with no overlap: the middle already belongs to B.
    Jump,
}

impl CrossfadeBehaviour {
    pub const ALL: [CrossfadeBehaviour; 4] = [Self::Fade, Self::Cut, Self::CutReturn, Self::Jump];

    pub fn label(self) -> &'static str {
        match self {
            Self::Fade => "Fade",
            Self::Cut => "Cut",
            Self::CutReturn => "Cut & return",
            Self::Jump => "Jump",
        }
    }
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

fn one_f32() -> f32 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Animation {
    Timeline {
        duration_secs: f64,
        curve: Curve,
        #[serde(default)]
        motion: Motion,
    },
    Beats {
        beats: f64,
        curve: Curve,
        #[serde(default)]
        motion: Motion,
    },
    Audio {
        band: FftBand,
        gain: f32,
        threshold: f32,
        /// Seconds to rise to a louder level (0 = instant).
        #[serde(default)]
        attack: f32,
        /// Seconds to fall to a quieter level (0 = instant).
        #[serde(default)]
        release: f32,
        /// Output window as a fraction of the param range.
        #[serde(default)]
        min: f32,
        #[serde(default = "one_f32")]
        max: f32,
    },
    Envelope {
        points: Vec<(f64, f64)>,
        /// Cycle length, in beats or seconds according to `unit`.
        beats: f64,
        #[serde(default)]
        motion: Motion,
        /// Curve of the segment that starts at point `i` (missing entries are linear).
        #[serde(default)]
        curves: Vec<EnvCurve>,
        #[serde(default)]
        unit: EnvUnit,
    },
    Dashboard {
        dial: u32,
        /// Whose dial: the composition's, the layer's or the clip's the param lives on.
        #[serde(default)]
        scope: DialScope,
        /// Output window of this link as a fraction of the param range.
        #[serde(default)]
        min: f32,
        #[serde(default = "one_f32")]
        max: f32,
    },
}

/// Shape of one envelope segment, from its point to the next.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum EnvCurve {
    #[default]
    Linear,
    /// Keeps the point's value until the next point.
    Hold,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// Cubic bezier easing with control points (x1, y1) and (x2, y2) in segment units.
    Bezier([f32; 4]),
}

impl EnvCurve {
    pub fn label(self) -> &'static str {
        match self {
            EnvCurve::Linear => "Linear",
            EnvCurve::Hold => "Hold",
            EnvCurve::EaseIn => "Ease in",
            EnvCurve::EaseOut => "Ease out",
            EnvCurve::EaseInOut => "Ease in/out",
            EnvCurve::Bezier(_) => "Bezier",
        }
    }
}

/// Unit of an envelope's cycle length.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvUnit {
    #[default]
    Beats,
    Seconds,
}

/// What an animation does after its first cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopMode {
    /// Repeats forever.
    #[default]
    Loop,
    /// Alternates forward and backward cycles.
    PingPong,
    /// Runs one cycle from the moment its clip is triggered, then holds the end.
    Once,
}

impl LoopMode {
    pub const ALL: [LoopMode; 3] = [LoopMode::Loop, LoopMode::PingPong, LoopMode::Once];

    pub fn label(self) -> &'static str {
        match self {
            LoopMode::Loop => "Loop",
            LoopMode::PingPong => "PingPong",
            LoopMode::Once => "Once",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimDirection {
    #[default]
    Forward,
    Backward,
}

/// Loop mode and direction of a Timeline, Beats or Envelope animation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    pub mode: LoopMode,
    pub direction: AnimDirection,
    /// Time-based animations count from the clip's trigger instead of the global clock.
    #[serde(default)]
    pub follow_clip: bool,
    /// Playhead parked at this position of the cycle (0..1) instead of running.
    #[serde(default)]
    pub scrub: Option<f64>,
}

/// Which dashboard a `Animation::Dashboard` reads. Layer and Clip mean the layer or clip
/// that owns the animated param (its track, effects, transform or generator params).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DialScope {
    #[default]
    Comp,
    Layer,
    Clip,
}

impl DialScope {
    pub const ALL: [DialScope; 3] = [DialScope::Comp, DialScope::Layer, DialScope::Clip];

    pub fn label(self) -> &'static str {
        match self {
            DialScope::Comp => "Comp",
            DialScope::Layer => "Layer",
            DialScope::Clip => "Clip",
        }
    }
}

/// Global quantization of clip triggers and beat-loop starts to the tempo clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BeatSnap {
    #[default]
    Off,
    /// A quarter of a bar (one beat in 4/4): "1/4 bar".
    Beat,
    /// Half a bar ("1/2 bar").
    Half,
    /// One bar.
    Bar,
    Bars2,
    Bars4,
    Bars8,
}

impl BeatSnap {
    /// Every division, coarsest last.
    pub const ALL: [BeatSnap; 7] = [
        BeatSnap::Off,
        BeatSnap::Beat,
        BeatSnap::Half,
        BeatSnap::Bar,
        BeatSnap::Bars2,
        BeatSnap::Bars4,
        BeatSnap::Bars8,
    ];

    /// Grid length in beats (4/4); None when off.
    pub fn beats(self) -> Option<f64> {
        match self {
            BeatSnap::Off => None,
            BeatSnap::Beat => Some(1.0),
            BeatSnap::Half => Some(2.0),
            BeatSnap::Bar => Some(4.0),
            BeatSnap::Bars2 => Some(8.0),
            BeatSnap::Bars4 => Some(16.0),
            BeatSnap::Bars8 => Some(32.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BeatSnap::Off => "None",
            BeatSnap::Beat => "1/4 bar",
            BeatSnap::Half => "1/2 bar",
            BeatSnap::Bar => "1 bar",
            BeatSnap::Bars2 => "2 bars",
            BeatSnap::Bars4 => "4 bars",
            BeatSnap::Bars8 => "8 bars",
        }
    }

    /// Effective snap: the clip's override wins over the column's, which wins over the
    /// composition's (`comp`, already resolved against the app preference).
    pub fn effective(clip: Option<BeatSnap>, column: Option<BeatSnap>, comp: BeatSnap) -> BeatSnap {
        clip.or(column).unwrap_or(comp)
    }
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
    /// Overall level (RMS) of the input.
    Level,
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
    /// Off: the screen keeps its size and keeps sending, but every pixel is black (a frozen
    /// last frame on an LED processor would be worse than black).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Opacity, brightness, contrast and RGB of the whole screen, applied after the slices.
    #[serde(default)]
    pub color: ScreenColor,
    /// Output delay in milliseconds (0..=100), to line screens up. 0 = no queue.
    #[serde(default)]
    pub delay_ms: u32,
    /// Edge blending for projector overlap: slices are added where they overlap and the
    /// area outside every slice has alpha 0 instead of black. Off = later slices cover
    /// earlier ones over black.
    #[serde(default)]
    pub additive_overlap: bool,
    /// Output mask: the screen shows only inside it (black outside).
    #[serde(default)]
    pub output_mask: Option<Mask>,
}

fn default_true() -> bool {
    true
}

fn default_one() -> f32 {
    1.0
}

/// Colour of a whole screen. `opacity` fades the picture to black (outputs have no alpha).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScreenColor {
    #[serde(default = "default_one")]
    pub opacity: f32,
    #[serde(flatten)]
    pub color: SliceColor,
}

impl Default for ScreenColor {
    fn default() -> Self {
        ScreenColor {
            opacity: 1.0,
            color: SliceColor::default(),
        }
    }
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
        /// One or more fixtures. Projects saved with a single `fixture` load as a list of one.
        #[serde(
            default = "default_fixtures",
            alias = "fixture",
            deserialize_with = "one_or_many_fixtures"
        )]
        fixtures: Vec<Fixture>,
        /// Send rate limit in Hz (0 = the app default, 40).
        #[serde(default)]
        fps: f32,
        /// Delay of the whole device, in milliseconds.
        #[serde(default)]
        delay_ms: u32,
    },
}

fn default_fixtures() -> Vec<Fixture> {
    vec![Fixture::default()]
}

fn one_or_many_fixtures<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Fixture>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        Many(Vec<Fixture>),
        One(Fixture),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::Many(v) => v,
        OneOrMany::One(f) => vec![f],
    })
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
    /// First DMX channel (1..=512) inside the device's first universe.
    pub start_channel: u16,
    /// Part of the screen this fixture samples (the whole screen by default).
    pub input: FixtureInput,
    pub flip_h: bool,
    pub flip_v: bool,
    pub color: SliceColor,
}

/// Input rectangle of a fixture, normalized to the screen: centre, size and rotation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FixtureInput {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Degrees, clockwise.
    pub rotation: f32,
}

impl Default for FixtureInput {
    fn default() -> Self {
        FixtureInput {
            x: 0.5,
            y: 0.5,
            w: 1.0,
            h: 1.0,
            rotation: 0.0,
        }
    }
}

impl FixtureInput {
    /// The whole screen, unrotated.
    pub fn is_full(&self) -> bool {
        *self == FixtureInput::default()
    }
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
            start_channel: 1,
            input: FixtureInput::default(),
            flip_h: false,
            flip_v: false,
            color: SliceColor::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slice {
    pub id: Uuid,
    /// Label shown in the Output setup tree and on the canvas; empty = "Slice N".
    #[serde(default)]
    pub name: String,
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
    /// Per-slice brightness and white balance, to match panels of different batches.
    #[serde(default)]
    pub color: SliceColor,
    /// Mask inverted: the slice shows only outside the mask polygon.
    #[serde(default)]
    pub mask_invert: bool,
    /// Is Key: the colour is replaced by the alpha (white = content, black = transparent).
    #[serde(default)]
    pub is_key: bool,
    /// Black BG: a black backdrop under the slice (transparent areas turn black).
    #[serde(default)]
    pub black_bg: bool,
    /// Outline of the slice on the output: a plain rectangle or a free polygon
    /// (normalized to the screen).
    #[serde(default)]
    pub shape: SliceShape,
    /// How the warp mesh is interpolated between its points.
    #[serde(default)]
    pub mesh_mode: MeshMode,
    /// Bezier handles per mesh point (same order as `output_mesh`): the offset of the
    /// handle toward the next column and toward the next row, in output pixels. Empty or
    /// shorter than the mesh = zero handles.
    #[serde(default)]
    pub mesh_tangents: Vec<((f32, f32), (f32, f32))>,
    /// Zoom, shift and turn of the content inside the slice; the outline does not move.
    #[serde(default)]
    pub transform: SliceTransform,
}

/// Placement of the content inside a slice, pivoting on the centre of its input area.
/// The default is the identity, so older projects load unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SliceTransform {
    /// Zoom factor, 1 = as is.
    pub scale: f32,
    /// Shift of the content as a fraction of the input area's width and height.
    pub offset: (f32, f32),
    /// Turn of the content in degrees, clockwise.
    pub rotation: f32,
}

impl Default for SliceTransform {
    fn default() -> Self {
        SliceTransform {
            scale: 1.0,
            offset: (0.0, 0.0),
            rotation: 0.0,
        }
    }
}

/// Outline of a slice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum SliceShape {
    #[default]
    Rect,
    /// Closed polygon, normalized 0..1 on the screen (3..=64 points).
    Polygon(Vec<(f32, f32)>),
}

/// Interpolation of the warp mesh.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeshMode {
    #[default]
    Linear,
    Bezier,
}

/// A shape for a new slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShapePreset {
    Rect,
    Circle,
    Triangle,
    Hexagon,
}

impl ShapePreset {
    pub const ALL: [ShapePreset; 4] = [
        ShapePreset::Rect,
        ShapePreset::Circle,
        ShapePreset::Triangle,
        ShapePreset::Hexagon,
    ];
}

/// A mask on a whole output screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Mask {
    /// Clicked polygon, normalized to the screen.
    Polygon(Vec<(f32, f32)>),
    /// Freehand stroke already simplified to a polygon.
    Path(Vec<(f32, f32)>),
}

impl Mask {
    pub fn points(&self) -> &[(f32, f32)] {
        match self {
            Mask::Polygon(p) | Mask::Path(p) => p,
        }
    }
}

/// Multipliers applied to the slice's RGB after the soft edge: 1.0 = unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SliceColor {
    pub brightness: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    /// Slope around mid grey: 1.0 = unchanged. Files saved before it existed load as 1.0.
    #[serde(default = "default_one")]
    pub contrast: f32,
}

impl Default for SliceColor {
    fn default() -> Self {
        SliceColor {
            brightness: 1.0,
            r: 1.0,
            g: 1.0,
            b: 1.0,
            contrast: 1.0,
        }
    }
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
            audio_effects: vec![],
            volume: Param::number("Volume", 1.0, Some((0.0, 1.0))),
            pan: Param::number("Pan", 0.0, Some((-1.0, 1.0))),
            transform: Transform::default(),
            dials: vec![],
        }
    }
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            play_mode: PlayMode::Loop,
            trigger_mode: TriggerMode::Layer,
            direction: Direction::Forward,
            resume_direction: Direction::Forward,
            speed: 1.0,
            beat_sync: None,
            cue_points: vec![],
            in_point: 0.0,
            out_point: 1.0,
            timecode_start: None,
            duration_secs: None,
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
                description: String::new(),
                bit_depth: 8,
                autopilot: Default::default(),
                beat_snap: None,
                id: Uuid::new_v4(),
                name: name.into(),
                width: 1920,
                height: 1080,
                fps: 60.0,
                tempo: Tempo {
                    bpm: 120.0,
                    link_enabled: false,
                    metronome: false,
                    play_state: PlayState::Play,
                },
                layers: vec![],
                columns: vec![],
                current_deck: deck.id,
                decks: vec![deck],
                master: Track::new(BlendMode::Normal),
                groups: vec![],
                master_speed: 1.0,
                master_reverse: false,
                crossfader: None,
                dashboard: vec![],
                timecode_cues: vec![],
                trigger_style: Default::default(),
                clip_target: Default::default(),
            },
            output: OutputSetup { screens: vec![] },
            shortcuts: vec![],
        }
    }
}

impl Track {
    /// Visits every param of the track: mix, transform and effect params.
    pub fn for_each_param(&self, f: &mut dyn FnMut(&Param)) {
        let tf = &self.transform;
        for p in [&self.opacity, &self.volume, &self.pan].into_iter().chain([
            &tf.x,
            &tf.y,
            &tf.scale,
            &tf.scale_w,
            &tf.scale_h,
            &tf.rotation,
            &tf.anchor_x,
            &tf.anchor_y,
            &tf.rotation_x,
            &tf.rotation_y,
            &tf.anchor_z,
        ]) {
            f(p);
        }
        self.effects
            .iter()
            .for_each(|e| e.params.iter().for_each(&mut *f));
        self.dials.iter().for_each(&mut *f);
    }
}

impl Composition {
    /// Visits every `Param` in the composition (master, layers, groups, clips and their
    /// generator params, crossfader, dashboard). Allocation free. Any new `Param`-holding
    /// field must be added here; `for_each_param_covers_every_location` guards it.
    pub fn for_each_param(&self, f: &mut dyn FnMut(&Param)) {
        self.master.for_each_param(f);
        self.layers.iter().for_each(|l| l.track.for_each_param(f));
        self.groups.iter().for_each(|g| g.track.for_each_param(f));
        for c in self.decks.iter().flat_map(|d| &d.clips) {
            c.clip.track.for_each_param(f);
            if let Source::Generator { params, .. } = &c.clip.source {
                params.iter().for_each(&mut *f);
            }
        }
        if let Some(x) = &self.crossfader {
            f(&x.position);
        }
        self.dashboard.iter().for_each(&mut *f);
    }

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

    fn layer_group(&self, l: &Layer) -> Option<&LayerGroup> {
        l.group.and_then(|g| self.group(g))
    }

    /// Content of the layer is locked, by itself or by its group.
    pub fn locked(&self, l: &Layer) -> bool {
        l.lock_content || self.layer_group(l).is_some_and(|g| g.lock_content)
    }

    /// A column trigger leaves the layer alone (its own flag or its group's).
    pub fn ignores_column(&self, l: &Layer) -> bool {
        l.ignore_column_trigger || self.layer_group(l).is_some_and(|g| g.ignore_column_trigger)
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
    #[test]
    fn artnet_device_loads_the_old_single_fixture_format() {
        let old = r#"{"ArtNet":{"ip":"10.0.0.9","universe":3,"fixture":{"name":"M","width":8,"height":8,"format":"Rgb","order":"Grb","gamma":2.2,"serpentine":true}}}"#;
        let d: OutputDevice = serde_json::from_str(old).unwrap();
        let OutputDevice::ArtNet {
            ip,
            universe,
            fixtures,
            fps,
            delay_ms,
        } = &d
        else {
            panic!()
        };
        assert_eq!((ip.as_str(), *universe, fixtures.len()), ("10.0.0.9", 3, 1));
        assert_eq!((fixtures[0].width, fixtures[0].start_channel), (8, 1));
        assert!(fixtures[0].input.is_full() && !fixtures[0].flip_h);
        assert_eq!((*fps, *delay_ms), (0.0, 0));
        // New format round-trips; a missing field still gives one default fixture.
        let mut d2 = d.clone();
        if let OutputDevice::ArtNet { fixtures, .. } = &mut d2 {
            fixtures.push(Fixture {
                start_channel: 100,
                flip_v: true,
                ..Fixture::default()
            });
        }
        let back: OutputDevice =
            serde_json::from_str(&serde_json::to_string(&d2).unwrap()).unwrap();
        assert_eq!(back, d2);
        let bare: OutputDevice = serde_json::from_str(r#"{"ArtNet":{"ip":"x"}}"#).unwrap();
        assert!(
            matches!(bare, OutputDevice::ArtNet { fixtures, .. } if fixtures == [Fixture::default()])
        );
        // Negative control: without the migration a list-only reader would reject the old form.
        assert!(serde_json::from_str::<Vec<Fixture>>(r#"{"name":"M"}"#).is_err());
    }

    #[test]
    fn old_slice_without_color_loads_as_identity() {
        let s = r#"{"id":"00000000-0000-0000-0000-000000000001","input":[],"output_mesh":[],"mesh_cols":2,"mask":null,"soft_edge":null}"#;
        let sl: Slice = serde_json::from_str(s).unwrap();
        assert_eq!(sl.color, SliceColor::default());
        // Negative control: a stored colour is not overwritten by the default.
        let s2 = s.replace(
            "\"soft_edge\":null",
            "\"soft_edge\":null,\"color\":{\"brightness\":0.5,\"r\":1.0,\"g\":1.0,\"b\":0.9}",
        );
        let sl: Slice = serde_json::from_str(&s2).unwrap();
        assert_eq!(sl.color.brightness, 0.5);
    }

    #[test]
    fn slice_transform_roundtrips_and_old_files_load_as_identity() {
        let s = r#"{"id":"00000000-0000-0000-0000-000000000001","input":[],"output_mesh":[],"mesh_cols":2,"mask":null,"soft_edge":null}"#;
        let sl: Slice = serde_json::from_str(s).unwrap();
        assert_eq!(sl.transform, SliceTransform::default());
        assert_eq!(sl.transform.scale, 1.0);
        assert_eq!(sl.transform.offset, (0.0, 0.0));
        assert_eq!(sl.transform.rotation, 0.0);
        // Negative control: a stored transform survives a save and load.
        let mut set = sl.clone();
        set.transform = SliceTransform {
            scale: 2.5,
            offset: (0.1, -0.2),
            rotation: 30.0,
        };
        let back: Slice = serde_json::from_str(&serde_json::to_string(&set).unwrap()).unwrap();
        assert_eq!(back.transform, set.transform);
        assert_ne!(back.transform, SliceTransform::default());
    }

    #[test]
    fn autopilot_from_an_older_file_defaults_to_loops_and_new_fields_roundtrip() {
        let old: Autopilot = serde_json::from_str(r#"{"mode":"Next","every_loops":3}"#).unwrap();
        assert_eq!(old.unit, AutopilotUnit::Loops);
        assert_eq!(old.every_loops, 3);
        let new = Autopilot {
            looping: true,
            random: RandomKind::Other,
            mode: AutopilotMode::FirstLast,
            every_loops: 1,
            unit: AutopilotUnit::Beats,
            every: 16.0,
        };
        let back: Autopilot = serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }

    #[test]
    fn play_state_defaults_to_play_and_roundtrips() {
        let old: Tempo =
            serde_json::from_str(r#"{"bpm":120.0,"link_enabled":false,"metronome":false}"#)
                .unwrap();
        assert_eq!(old.play_state, PlayState::Play);
        let mut t = old;
        t.play_state = PlayState::Pause;
        let back: Tempo = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn column_name_roundtrips_and_defaults_when_absent() {
        let c = Column {
            color: None,
            autopilot: Default::default(),
            beat_snap: None,
            id: Uuid::new_v4(),
            name: "Drop".into(),
        };
        let back: Column = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        // negative control: an old file without the field still loads
        let old = format!("{{\"id\":\"{}\"}}", c.id);
        assert_eq!(serde_json::from_str::<Column>(&old).unwrap().name, "");
    }

    use super::*;

    #[test]
    fn solo_turns_other_layers_off() {
        let mut p = Project::new("t");
        let mk = |solo, bypass| Layer {
            color: None,
            folded: false,
            id: Uuid::new_v4(),
            name: "L".into(),
            track: Track::new(BlendMode::Normal),
            solo,
            bypass,
            crossfader_bus: None,
            audio: true,
            video: true,
            trigger_mode: TriggerMode::Restart,
            ignore_column_trigger: false,
            lock_content: false,
            fader_start: false,
            extras: Default::default(),
            mask_mode: Default::default(),
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
            color: None,
            folded: false,
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
            trigger_mode: TriggerMode::Restart,
            ignore_column_trigger: false,
            lock_content: false,
            fader_start: false,
            extras: Default::default(),
            mask_mode: Default::default(),
            group: None,
        });
        p.composition.decks[0].set_clip(
            lid,
            2,
            Clip {
                extras: Default::default(),
                id: Uuid::new_v4(),
                name: "c".into(),
                source: Source::File {
                    path: "a.mov".into(),
                },
                transport: Transport::default(),
                track: Track::new(BlendMode::Normal),
                fit: FitMode::default(),
                blend_override: None,
                transition: None,
                trigger_style: Default::default(),
                beat_snap: None,
                target: Default::default(),
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
    fn trigger_mode_resolves_and_old_files_load() {
        use TriggerMode as T;
        assert_eq!(T::Layer.resolve(T::Continue), T::Continue);
        // Negative control: an explicit clip mode beats the layer default.
        assert_eq!(T::Relative.resolve(T::Continue), T::Relative);
        assert_eq!(T::Layer.resolve(T::Layer), T::Restart);
        assert_eq!(T::Unknown.resolve(T::Continue), T::Restart);
        // Files saved before the field existed.
        let mut v = serde_json::to_value(Transport::default()).unwrap();
        v.as_object_mut().unwrap().remove("trigger_mode");
        let t: Transport = serde_json::from_value(v).unwrap();
        assert_eq!(t.trigger_mode, T::Layer);
        let mut v = serde_json::to_value(Transport::default()).unwrap();
        v["trigger_mode"] = "FromTheFuture".into();
        let t: Transport = serde_json::from_value(v).unwrap();
        assert_eq!(t.trigger_mode, T::Unknown);
    }

    #[test]
    fn param_clamps() {
        let mut p = Param::number("x", 0.5, Some((0.0, 1.0)));
        p.set_number(3.0);
        assert_eq!(p.value, Value::Number(1.0));
    }

    #[test]
    fn comp_autopilot_round_trips_and_old_files_are_off() {
        let mut c = fixture();
        c.autopilot.target = ColumnTarget::Random;
        c.autopilot.random = RandomKind::Bag;
        c.autopilot.looping = false;
        c.columns.push(Column {
            color: None,
            id: Uuid::new_v4(),
            name: "c".into(),
            autopilot: ColumnAutopilot {
                duration: Some(ColumnDuration::BottomClip),
                ..Default::default()
            },
            beat_snap: None,
        });
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(serde_json::from_value::<Composition>(v.clone()).unwrap(), c);
        // A project saved before the autopilot existed: no key at all.
        let mut old = v;
        old.as_object_mut().unwrap().remove("autopilot");
        for col in old["columns"].as_array_mut().unwrap() {
            col.as_object_mut().unwrap().remove("autopilot");
        }
        let o: Composition = serde_json::from_value(old).unwrap();
        assert_eq!(o.autopilot.target, ColumnTarget::Off);
        assert!(
            o.columns
                .iter()
                .all(|c| c.autopilot == ColumnAutopilot::default())
        );
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

    #[test]
    fn old_animation_without_motion_loads_as_loop_forward() {
        let a: Animation =
            serde_json::from_str(r#"{"Beats":{"beats":4.0,"curve":"Sine"}}"#).unwrap();
        assert!(matches!(a, Animation::Beats { motion, .. } if motion == Motion::default()));
        let m = Motion {
            follow_clip: false,
            scrub: None,
            mode: LoopMode::PingPong,
            direction: AnimDirection::Backward,
        };
        let b = Animation::Beats {
            beats: 2.0,
            curve: Curve::Saw,
            motion: m,
        };
        let back: Animation = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(back, b);
    }

    fn audio_anim() -> Animation {
        Animation::Audio {
            band: FftBand::Low,
            gain: 1.0,
            threshold: 0.0,
            attack: 0.0,
            release: 0.0,
            min: 0.0,
            max: 1.0,
        }
    }

    fn fixture() -> Composition {
        let mut c = Project::new("t").composition;
        let p = || Param::number("p", 0.0, None);
        let eff = || Effect {
            blend: Default::default(),
            id: Uuid::new_v4(),
            kind: "k".into(),
            bypass: false,
            dry_wet: 1.0,
            params: vec![p()],
        };
        c.master.effects.push(eff());
        c.layers.clear();
        c.layers.push(Layer {
            color: None,
            folded: false,
            id: Uuid::new_v4(),
            name: "L".into(),
            track: Track::new(BlendMode::Normal),
            solo: false,
            bypass: false,
            crossfader_bus: None,
            audio: true,
            video: true,
            trigger_mode: TriggerMode::Restart,
            ignore_column_trigger: false,
            lock_content: false,
            fader_start: false,
            extras: Default::default(),
            mask_mode: Default::default(),
            transition: Transition {
                kind: "cut".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            autopilot: Default::default(),
            group: None,
        });
        c.layers[0].track.effects.push(eff());
        let mut g = LayerGroup::new("g");
        g.track.effects.push(eff());
        c.groups = vec![g];
        let mut deck = c.decks[0].clone();
        let mut clip = Clip {
            extras: Default::default(),
            id: Uuid::new_v4(),
            name: "c".into(),
            source: Source::Generator {
                kind: "g".into(),
                params: vec![p()],
            },
            transport: Transport::default(),
            track: Track::new(BlendMode::Normal),
            blend_override: None,
            transition: None,
            trigger_style: Default::default(),
            beat_snap: None,
            target: Default::default(),
            fit: FitMode::Fit,
            channels: Channels::default(),
        };
        clip.track.effects.push(eff());
        deck.clips = vec![CellClip {
            layer: c.layers[0].id,
            column: Default::default(),
            clip,
        }];
        c.decks = vec![deck];
        c.crossfader = Some(Crossfader {
            position: p(),
            curve: "linear".into(),
            behaviour: Default::default(),
            blend: None,
        });
        c.dashboard = vec![p()];
        c.layers[0].track.dials = vec![p()];
        c.decks[0].clips[0].clip.track.dials = vec![p()];
        c
    }

    #[test]
    fn for_each_param_covers_every_location() {
        fn walk(v: &mut serde_json::Value, i: usize, seen: &mut usize) {
            match v {
                serde_json::Value::Object(m) => {
                    if m.contains_key("animation") {
                        if *seen == i {
                            m.insert(
                                "animation".into(),
                                serde_json::to_value(Some(audio_anim())).unwrap(),
                            );
                        }
                        *seen += 1;
                    }
                    m.values_mut().for_each(|x| walk(x, i, seen));
                }
                serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, i, seen)),
                _ => {}
            }
        }
        let base = fixture();
        let json = serde_json::to_string(&base).unwrap();
        // Every param serializes one "animation" key: the visitor must see all of them.
        let expected = json.matches("\"animation\"").count();
        let mut n = 0;
        base.for_each_param(&mut |_| n += 1);
        assert_eq!(n, expected);
        // And each one individually is detected when it carries an audio animation.
        for i in 0..n {
            let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
            walk(&mut v, i, &mut 0);
            let c: Composition = serde_json::from_value(v).unwrap();
            let mut found = 0;
            c.for_each_param(&mut |p| {
                found += matches!(p.animation, Some(Animation::Audio { .. })) as usize
            });
            assert_eq!(found, 1, "param #{i} not visited");
        }
    }

    #[test]
    fn envelope_and_motion_saved_before_curves_and_scrub_load_with_defaults() {
        let old: Animation = serde_json::from_str(
            r#"{"Envelope":{"points":[[0.0,0.0],[1.0,1.0]],"beats":4.0,
                "motion":{"mode":"PingPong","direction":"Backward"}}}"#,
        )
        .unwrap();
        let Animation::Envelope {
            curves,
            unit,
            motion,
            beats,
            ..
        } = old
        else {
            panic!("not an envelope");
        };
        assert!(curves.is_empty());
        assert_eq!(unit, EnvUnit::default());
        assert_eq!(beats, 4.0);
        assert_eq!(motion.mode, LoopMode::PingPong);
        assert!(!motion.follow_clip && motion.scrub.is_none());
    }

    #[test]
    fn dashboard_animation_without_scope_loads_as_comp() {
        let old: Animation = serde_json::from_str(r#"{"Dashboard":{"dial":2}}"#).unwrap();
        assert_eq!(
            old,
            Animation::Dashboard {
                min: 0.0,
                max: 1.0,
                dial: 2,
                scope: DialScope::Comp
            }
        );
        let new = Animation::Dashboard {
            min: 0.0,
            max: 1.0,
            dial: 1,
            scope: DialScope::Clip,
        };
        let back: Animation = serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
        // A track saved before dials existed opens with none.
        let mut t = serde_json::to_value(Track::new(BlendMode::Normal)).unwrap();
        t.as_object_mut().unwrap().remove("dials");
        let t: Track = serde_json::from_value(t).unwrap();
        assert!(t.dials.is_empty());
    }

    /// A screen saved before `enabled` / `color` / `contrast` existed loads unchanged.
    #[test]
    fn old_screen_json_roundtrips_with_neutral_defaults() {
        let old = r#"{"id":"00000000-0000-0000-0000-000000000001","name":"A",
            "device":"Window","width":4,"height":2,
            "slices":[{"id":"00000000-0000-0000-0000-000000000002","input":[],
            "output_mesh":[],"mesh_cols":2,"mask":null,"soft_edge":null,
            "color":{"brightness":0.5,"r":1.0,"g":1.0,"b":1.0}}]}"#;
        let s: Screen = serde_json::from_str(old).unwrap();
        assert!(s.enabled);
        assert_eq!(s.color, ScreenColor::default());
        assert_eq!(s.slices[0].color.contrast, 1.0);
        assert_eq!(s.slices[0].color.brightness, 0.5);
        let back: Screen = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn clip_transition_roundtrips_and_old_clips_default_to_layer() {
        let mut clip = Clip {
            extras: Default::default(),
            id: Uuid::new_v4(),
            name: "c".into(),
            source: Source::Generator {
                kind: "g".into(),
                params: vec![],
            },
            transport: Transport::default(),
            track: Track::new(BlendMode::Normal),
            blend_override: None,
            transition: Some(Transition {
                kind: "Cube".into(),
                duration_beats: Some(2.0),
                duration_secs: 0.5,
            }),
            trigger_style: Default::default(),
            beat_snap: None,
            target: Default::default(),
            fit: FitMode::Fit,
            channels: Channels::default(),
        };
        let json = serde_json::to_string(&clip).unwrap();
        assert_eq!(serde_json::from_str::<Clip>(&json).unwrap(), clip);
        // An old file has no such key: the layer's transition applies.
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("transition");
        let old: Clip = serde_json::from_value(v).unwrap();
        clip.transition = None;
        assert_eq!(old, clip);
    }

    #[test]
    fn trigger_style_and_target_roundtrip_and_old_projects_default() {
        let mut p = Project::new("t");
        p.composition.trigger_style = TriggerStyle::Hold;
        p.composition.clip_target = ClipTarget::Free;
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.composition.trigger_style, TriggerStyle::Hold);
        assert_eq!(back.composition.clip_target, ClipTarget::Free);
        // An old file has none of the new keys and opens with the old behaviour.
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let c = v["composition"].as_object_mut().unwrap();
        c.remove("trigger_style");
        c.remove("clip_target");
        let old: Project = serde_json::from_value(v).unwrap();
        assert_eq!(old.composition.trigger_style, TriggerStyle::Composition);
        assert_eq!(old.composition.clip_target, ClipTarget::Composition);
        assert_eq!(
            TriggerStyle::Composition.resolve(old.composition.trigger_style),
            TriggerStyle::Normal
        );
        assert_eq!(
            ClipTarget::Composition.resolve(old.composition.clip_target),
            ClipTarget::Own
        );
    }

    #[test]
    fn beat_snap_and_3d_transform_roundtrip_and_old_projects_default() {
        // Values already on disk keep their names.
        for (v, n) in [
            (BeatSnap::Off, "\"Off\""),
            (BeatSnap::Beat, "\"Beat\""),
            (BeatSnap::Bar, "\"Bar\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), n);
        }
        let mut p = Project::new("t");
        if p.composition.columns.is_empty() {
            p.composition.columns.push(Column {
                color: None,
                id: Uuid::new_v4(),
                name: "1".into(),
                autopilot: Default::default(),
                beat_snap: None,
            });
        }
        p.composition.beat_snap = Some(BeatSnap::Bars4);
        p.composition.columns[0].beat_snap = Some(BeatSnap::Half);
        let tf = &mut p.composition.master.transform;
        tf.rotation_x = Param::number("Rotation X", 30.0, None);
        tf.rotation_y.default = Value::Number(-45.0);
        tf.anchor_z.default = Value::Number(120.0);
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        // An old file has none of the new keys and opens as before.
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let comp = v["composition"].as_object_mut().unwrap();
        comp.remove("beat_snap");
        comp["columns"][0]
            .as_object_mut()
            .unwrap()
            .remove("beat_snap");
        let t = comp["master"]["transform"].as_object_mut().unwrap();
        for k in ["rotation_x", "rotation_y", "anchor_z"] {
            assert!(t.remove(k).is_some(), "{k}");
        }
        let old: Project = serde_json::from_value(v).unwrap();
        assert_eq!(old.composition.beat_snap, None);
        assert_eq!(old.composition.columns[0].beat_snap, None);
        assert_eq!(old.composition.master.transform, Transform::default());
    }

    #[test]
    fn beat_snap_resolves_clip_then_column_then_composition() {
        use BeatSnap::*;
        assert_eq!(BeatSnap::effective(Some(Bar), Some(Half), Beat), Bar);
        assert_eq!(BeatSnap::effective(None, Some(Half), Beat), Half);
        assert_eq!(BeatSnap::effective(None, None, Beat), Beat);
        // A None division is an override, not "unset".
        assert_eq!(BeatSnap::effective(Some(Off), Some(Half), Beat), Off);
        assert_eq!(Bars8.beats(), Some(32.0));
        assert_eq!(Off.beats(), None);
    }

    #[test]
    fn layer_lock_fader_mask_default_when_absent_and_roundtrip() {
        let mut l = fixture().layers.remove(0);
        l.lock_content = true;
        l.fader_start = true;
        l.mask_mode = MaskMode::AllBelow;
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(serde_json::from_value::<Layer>(v.clone()).unwrap(), l);
        let mut old = v;
        for k in ["lock_content", "fader_start", "mask_mode"] {
            old.as_object_mut().unwrap().remove(k);
        }
        let o: Layer = serde_json::from_value(old).unwrap();
        assert!(!o.lock_content && !o.fader_start && o.mask_mode == MaskMode::Off);
    }

    #[test]
    fn layer_ignore_column_trigger_roundtrips_and_defaults_when_absent() {
        let mut l = fixture().layers.remove(0);
        l.ignore_column_trigger = true;
        let mut v = serde_json::to_value(&l).unwrap();
        assert!(
            serde_json::from_value::<Layer>(v.clone())
                .unwrap()
                .ignore_column_trigger
        );
        v.as_object_mut().unwrap().remove("ignore_column_trigger");
        assert!(
            !serde_json::from_value::<Layer>(v)
                .unwrap()
                .ignore_column_trigger
        );
    }

    #[test]
    fn old_group_without_mask_ignore_lock_loads_with_defaults() {
        let mut g = LayerGroup::new("G");
        g.mask_mode = MaskMode::OneBelow;
        g.ignore_column_trigger = true;
        g.lock_content = true;
        let mut v = serde_json::to_value(&g).unwrap();
        assert_eq!(serde_json::from_value::<LayerGroup>(v.clone()).unwrap(), g);
        for k in ["mask_mode", "ignore_column_trigger", "lock_content"] {
            v.as_object_mut().unwrap().remove(k);
        }
        let old: LayerGroup = serde_json::from_value(v).unwrap();
        assert_eq!(old.mask_mode, MaskMode::Off);
        assert!(!old.ignore_column_trigger && !old.lock_content);
    }

    #[test]
    fn old_screen_without_additive_overlap_loads_off() {
        let scr = Screen {
            id: Uuid::new_v4(),
            name: "S".into(),
            device: OutputDevice::Window,
            width: 8,
            height: 8,
            slices: vec![],
            pixel_perfect: false,
            enabled: true,
            color: Default::default(),
            delay_ms: 0,
            additive_overlap: true,
            output_mask: None,
        };
        let mut v = serde_json::to_value(&scr).unwrap();
        assert!(
            serde_json::from_value::<Screen>(v.clone())
                .unwrap()
                .additive_overlap
        );
        v.as_object_mut().unwrap().remove("additive_overlap");
        assert!(
            !serde_json::from_value::<Screen>(v)
                .unwrap()
                .additive_overlap
        );
    }
}
