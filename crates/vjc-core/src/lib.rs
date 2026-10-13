//! Command bus: every state change is a `Command` applied to a `Project` by the pure
//! function `apply`. `History` keeps inverses for undo/redo. No UI, no players: the app
//! reacts to the returned `Event`s. See replica/architecture.md section 3.

pub mod anim;
pub mod blend;
pub mod clock;
pub mod crossfader;
pub mod effects;
pub mod mapping;
pub mod midi;
pub mod osc;
pub mod playback;
pub mod playhead;
pub mod shortcuts;
pub mod transition;

pub use effects::EffectTarget;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;
use vjc_model::{
    BlendMode, Clip, Column, ColumnIdx, Deck, DeckId, Direction, Effect, EffectId, FitMode, Layer,
    LayerId, Param, PlayMode, Project, Source, Track, Transport, Value,
};

/// Continuous commands on the same target within this window merge into one undo step.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Creates a clip from a media file and puts it in the cell.
    ImportMedia {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        path: PathBuf,
    },
    /// Puts a ready-made clip in the cell (replacing any).
    SetClip {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        clip: Clip,
    },
    /// Puts a generator clip (no file; rendered at composition size) in the cell.
    SetGenerator {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        kind: String,
        name: String,
        params: Vec<Param>,
    },
    /// Sets param number `param` of the cell's generator (coalesces like effect params).
    SetGeneratorParam {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        param: usize,
        value: Value,
    },
    ClearCell {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
    /// Plays the cell's clip on its layer. Runtime only: not undoable.
    TriggerClip {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
    /// Stops the layer. Runtime only: not undoable.
    ClearLayer {
        layer: LayerId,
    },
    /// Triggers every clip of the column in each layer. Layers with no clip in the column
    /// are cleared (the column replaces the whole stage, like Resolume). Placeholder or
    /// otherwise unplayable clips are skipped (`Event::Skipped`), the rest still plays.
    /// Runtime only: not undoable.
    TriggerColumn {
        deck: DeckId,
        column: ColumnIdx,
    },
    /// Triggers the column `delta` away from `from` (None = nothing playing: starts at the
    /// first column). Clamped at the ends, no wrap. Runtime only.
    StepColumn {
        deck: DeckId,
        from: Option<ColumnIdx>,
        delta: i32,
    },
    /// Triggers the nearest clip of the layer `delta` columns away from `from` (empty cells
    /// and placeholders are passed over; at the ends nothing changes and `NoClip` is
    /// returned). Runtime only.
    StepClip {
        deck: DeckId,
        layer: LayerId,
        from: Option<ColumnIdx>,
        delta: i32,
    },
    /// Layer transition between clips (kind = a `BlendMode` name, see `transition::Kind`).
    SetLayerTransition {
        layer: LayerId,
        transition: vjc_model::Transition,
    },
    SetLayerAutopilot {
        layer: LayerId,
        autopilot: vjc_model::Autopilot,
    },
    /// Adds a cue point (normalised clip position 0..=1; kept sorted, no duplicates).
    AddCue {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        pos: f64,
    },
    RemoveCue {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        pos: f64,
    },
    /// Seeks the playing clip to cue `index`. Runtime only.
    JumpToCue {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        index: usize,
    },
    /// Stops every layer. Runtime only: not undoable.
    ClearAll,
    /// Starts recording the composition output to a file. Runtime only: not undoable,
    /// not in the project.
    StartRecording,
    /// Stops the recording and finalizes the file. Runtime only.
    StopRecording,
    /// Master bypass: black output while set. Runtime only: not in the project.
    SetMasterBypass {
        bypass: bool,
    },
    /// Master opacity 0..=1 (coalesces).
    SetMasterOpacity {
        value: f64,
    },
    /// Master speed 0..=4, multiplies every clip speed (coalesces).
    SetMasterSpeed {
        value: f64,
    },
    SetOpacity {
        layer: LayerId,
        value: f64,
    },
    /// Sets or clears the animation of a numeric param (undoable, coalesces per target).
    SetAnimation {
        target: AnimTarget,
        animation: Option<vjc_model::Animation>,
    },
    /// Dashboard dial `dial` (0..8), value 0..=1 (coalesces).
    SetDashboardDial {
        dial: u32,
        value: f64,
    },
    SetBlend {
        layer: LayerId,
        blend: BlendMode,
    },
    /// Assigns a layer to crossfader bus A or B (None = unaffected). Undoable.
    SetLayerBus {
        layer: LayerId,
        bus: Option<vjc_model::Bus>,
    },
    /// Crossfader position 0 (A) ..= 1 (B); creates the crossfader on first use (coalesces).
    SetCrossfader {
        value: f64,
    },
    /// Crossfader fade law. Undoable.
    SetCrossfaderCurve {
        curve: crossfader::CrossfadeCurve,
    },
    /// Output screens and slices (all undoable; `SetSliceMesh` coalesces).
    AddScreen {
        screen: vjc_model::Screen,
        index: usize,
    },
    RemoveScreen {
        screen: Uuid,
    },
    SetScreenDevice {
        screen: Uuid,
        device: vjc_model::OutputDevice,
    },
    SetScreenSize {
        screen: Uuid,
        width: u32,
        height: u32,
    },
    AddSlice {
        screen: Uuid,
        slice: vjc_model::Slice,
        index: usize,
    },
    RemoveSlice {
        screen: Uuid,
        slice: Uuid,
    },
    SetSliceInput {
        screen: Uuid,
        slice: Uuid,
        input: Vec<(f32, f32)>,
    },
    /// Whole mesh plus its column count (rows = len / cols).
    SetSliceMesh {
        screen: Uuid,
        slice: Uuid,
        mesh: Vec<(f32, f32)>,
        cols: u32,
    },
    /// Inverse of `SetScreenSize`: exact size and slice meshes, in slice order.
    RestoreScreen {
        screen: Uuid,
        width: u32,
        height: u32,
        meshes: Vec<Vec<(f32, f32)>>,
    },
    /// New grid size 2..=9 each; resamples the mesh keeping its shape.
    SetSliceMeshSize {
        screen: Uuid,
        slice: Uuid,
        cols: u32,
        rows: u32,
    },
    SetCompositionName {
        name: String,
    },
    /// Composition frame size in pixels (1..=16384) and frame rate (> 0).
    SetCompositionSize {
        width: u32,
        height: u32,
        fps: f32,
    },
    RenameLayer {
        layer: LayerId,
        name: String,
    },
    SetLayerSolo {
        layer: LayerId,
        solo: bool,
    },
    SetLayerBypass {
        layer: LayerId,
        bypass: bool,
    },
    RenameClip {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        name: String,
    },
    SetFit {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        fit: FitMode,
    },
    /// Inserts a layer at `index` (0 = top).
    AddLayer {
        layer: Layer,
        index: usize,
    },
    /// Removes an empty layer (no clips).
    RemoveLayer {
        layer: LayerId,
    },
    AddColumn {
        column: Column,
    },
    /// Removes the last column, which must be empty.
    RemoveColumn {
        column: Uuid,
    },
    SetBpm {
        bpm: f64,
    },
    /// Inserts a deck at `index`. Does not change the current deck unless `select` is set.
    AddDeck {
        deck: Deck,
        index: usize,
        /// Selection to restore after inserting (set by the undo of a `RemoveDeck`).
        select: Option<DeckId>,
    },
    /// Removes a deck (never the last one). Clips go with it; undo restores them.
    RemoveDeck {
        deck: DeckId,
        /// Selection to restore after removing (set by the undo of an `AddDeck`).
        select: Option<DeckId>,
    },
    RenameDeck {
        deck: DeckId,
        name: String,
    },
    /// Moves the deck to `index` in the tab order.
    MoveDeck {
        deck: DeckId,
        index: usize,
    },
    /// Switches the grid shown. Runtime only: not undoable, playing clips keep playing.
    SelectDeck {
        deck: DeckId,
    },
    /// Stretches the clip to `beats` beats (None = off). The app derives the player rate.
    SetBeatSync {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        beats: Option<f64>,
    },
    SetPlayMode {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        mode: PlayMode,
    },
    SetDirection {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        direction: Direction,
    },
    /// Clip speed multiplier, 0..=4 (coalesces: slider drags are one undo step).
    SetSpeed {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        speed: f64,
    },
    /// Inserts an effect at `index` of the target's chain (clamped to the end).
    AddEffect {
        target: EffectTarget,
        effect: Effect,
        index: usize,
    },
    RemoveEffect {
        target: EffectTarget,
        effect: EffectId,
    },
    /// Moves the effect to `index` in the chain.
    MoveEffect {
        target: EffectTarget,
        effect: EffectId,
        index: usize,
    },
    SetEffectBypass {
        target: EffectTarget,
        effect: EffectId,
        bypass: bool,
    },
    /// Sets param number `param` of the effect (coalesces: slider drags are one undo step).
    SetEffectParam {
        target: EffectTarget,
        effect: EffectId,
        param: usize,
        value: Value,
    },
    /// Clock input (not undoable, not in history): the app forwards these to its `Clock`.
    Tap {
        at: Instant,
    },
    /// -1, 0, +1: temporary slower/normal/faster.
    Nudge {
        dir: i8,
        at: Instant,
    },
    Resync {
        at: Instant,
    },
    /// Inserts a shortcut at `index` (clamped).
    AddShortcut {
        shortcut: vjc_model::Shortcut,
        index: usize,
    },
    RemoveShortcut {
        id: Uuid,
    },
}

/// A param that can be animated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimTarget {
    LayerOpacity(LayerId),
    MasterOpacity,
    Crossfader,
    Effect {
        target: EffectTarget,
        effect: EffectId,
        param: usize,
    },
    Generator {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        param: usize,
    },
}

/// What a coalescible command targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Opacity(LayerId),
    Speed(DeckId, LayerId, ColumnIdx),
    Bpm,
    MasterOpacity,
    MasterSpeed,
    Crossfader,
    EffectParam(EffectId, usize),
    GeneratorParam(DeckId, LayerId, ColumnIdx, usize),
    Dial(u32),
    Anim(AnimTarget),
    SliceMesh(Uuid),
    SliceInput(Uuid, Uuid),
    ScreenSize(Uuid),
}

impl Command {
    fn target(&self) -> Option<Target> {
        match self {
            Command::SetOpacity { layer, .. } => Some(Target::Opacity(*layer)),
            Command::SetBpm { .. } => Some(Target::Bpm),
            Command::SetScreenSize { screen, .. } => Some(Target::ScreenSize(*screen)),
            Command::SetSliceMesh { slice, .. } => Some(Target::SliceMesh(*slice)),
            Command::SetSliceInput { screen, slice, .. } => {
                Some(Target::SliceInput(*screen, *slice))
            }
            Command::SetDashboardDial { dial, .. } => Some(Target::Dial(*dial)),
            // Dragging an animation field emits one command per frame.
            Command::SetAnimation { target, .. } => Some(Target::Anim(*target)),
            Command::SetMasterOpacity { .. } => Some(Target::MasterOpacity),
            Command::SetMasterSpeed { .. } => Some(Target::MasterSpeed),
            Command::SetCrossfader { .. } => Some(Target::Crossfader),
            Command::SetEffectParam { effect, param, .. } => {
                Some(Target::EffectParam(*effect, *param))
            }
            Command::SetGeneratorParam {
                deck,
                layer,
                column,
                param,
                ..
            } => Some(Target::GeneratorParam(*deck, *layer, *column, *param)),
            Command::SetSpeed {
                deck,
                layer,
                column,
                ..
            } => Some(Target::Speed(*deck, *layer, *column)),
            _ => None,
        }
    }
}

/// Things the app must react to (start/stop players).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    ClipStarted {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        path: PathBuf,
    },
    /// A live capture clip (camera) started on the layer; `device` is the avfoundation
    /// index or name.
    CaptureStarted {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        device: String,
    },
    /// A generator clip started on the layer (no player; the app renders it).
    GeneratorStarted {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
    LayerCleared {
        layer: LayerId,
    },
    /// Jump the playing clip (if it is this cell) to a normalised position.
    Seek {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        pos: f64,
    },
    /// A column trigger skipped this clip (placeholder / not playable).
    Skipped {
        layer: LayerId,
        column: ColumnIdx,
    },
    MasterBypass {
        bypass: bool,
    },
    /// Output recording was requested to start.
    RecordingStarted,
    /// Output recording was requested to stop.
    RecordingStopped,
    /// The clip in this cell was replaced or removed.
    CellChanged {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
    /// A deck left the project: clips playing from it must stop.
    DeckRemoved {
        deck: DeckId,
    },
    Tap {
        at: Instant,
    },
    Nudge {
        dir: i8,
        at: Instant,
    },
    Resync {
        at: Instant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NoLayer,
    NoColumn,
    NoDeck,
    NoClip,
    NoEffect,
    NoShortcut,
    NoScreen,
    NoSlice,
    /// Clip has no file source to play.
    NotPlayable,
    /// Layer/column still holds clips, or is not the last column.
    InUse,
    /// Value outside the accepted range (e.g. BPM not finite or not positive).
    InvalidValue,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::NoLayer => "layer not found",
            Error::NoScreen => "screen not found",
            Error::NoSlice => "slice not found",
            Error::NoColumn => "column not found",
            Error::NoDeck => "current deck not found",
            Error::NoClip => "no clip in cell",
            Error::NoEffect => "effect not found",
            Error::NoShortcut => "shortcut not found",
            Error::NotPlayable => "clip has no playable file source",
            Error::InUse => "layer or column is not empty",
            Error::InvalidValue => "value out of range",
        })
    }
}

impl std::error::Error for Error {}

/// Result of `apply`: the command that undoes it (None if not undoable) and events.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub inverse: Option<Command>,
    pub events: Vec<Event>,
}

fn anim_param_mut(p: &mut Project, t: AnimTarget) -> Result<&mut Param, Error> {
    match t {
        AnimTarget::MasterOpacity => Ok(&mut p.composition.master.opacity),
        AnimTarget::Crossfader => Ok(&mut p
            .composition
            .crossfader
            .get_or_insert_with(crossfader::new_crossfader)
            .position),
        AnimTarget::LayerOpacity(l) => {
            let i = layer_idx(p, l)?;
            Ok(&mut p.composition.layers[i].track.opacity)
        }
        AnimTarget::Effect {
            target,
            effect,
            param,
        } => effects::track_mut(p, target)?
            .effects
            .iter_mut()
            .find(|e| e.id == effect)
            .ok_or(Error::NoEffect)?
            .params
            .get_mut(param)
            .ok_or(Error::InvalidValue),
        AnimTarget::Generator {
            deck,
            layer,
            column,
            param,
        } => {
            check_cell(p, layer, column)?;
            let clip = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .map(|k| &mut k.clip)
                .ok_or(Error::NoClip)?;
            let Source::Generator { params, .. } = &mut clip.source else {
                return Err(Error::NotPlayable);
            };
            params.get_mut(param).ok_or(Error::InvalidValue)
        }
    }
}

fn undoable(inverse: Command, events: Vec<Event>) -> Result<Applied, Error> {
    Ok(Applied {
        inverse: Some(inverse),
        events,
    })
}

fn screen_idx(p: &Project, id: Uuid) -> Result<usize, Error> {
    p.output
        .screens
        .iter()
        .position(|s| s.id == id)
        .ok_or(Error::NoScreen)
}

fn screen_mut(p: &mut Project, id: Uuid) -> Result<&mut vjc_model::Screen, Error> {
    let i = screen_idx(p, id)?;
    Ok(&mut p.output.screens[i])
}

fn slice_mut(p: &mut Project, screen: Uuid, slice: Uuid) -> Result<&mut vjc_model::Slice, Error> {
    screen_mut(p, screen)?
        .slices
        .iter_mut()
        .find(|s| s.id == slice)
        .ok_or(Error::NoSlice)
}

fn deck_mut(p: &mut Project, id: DeckId) -> Result<&mut vjc_model::Deck, Error> {
    p.composition
        .decks
        .iter_mut()
        .find(|d| d.id == id)
        .ok_or(Error::NoDeck)
}

/// The clip at (layer, column) of a specific deck. Playing clips are looked up with the
/// deck they were triggered from, never with `current_deck`.
pub fn clip_in(p: &Project, deck: DeckId, layer: LayerId, column: ColumnIdx) -> Option<&Clip> {
    p.composition
        .decks
        .iter()
        .find(|d| d.id == deck)?
        .clip_at(layer, column)
}

fn deck_idx(p: &Project, id: DeckId) -> Result<usize, Error> {
    p.composition
        .decks
        .iter()
        .position(|d| d.id == id)
        .ok_or(Error::NoDeck)
}

fn layer_idx(p: &Project, id: LayerId) -> Result<usize, Error> {
    p.composition
        .layers
        .iter()
        .position(|l| l.id == id)
        .ok_or(Error::NoLayer)
}

fn check_cell(p: &Project, layer: LayerId, column: ColumnIdx) -> Result<(), Error> {
    layer_idx(p, layer)?;
    if (column as usize) < p.composition.columns.len() {
        Ok(())
    } else {
        Err(Error::NoColumn)
    }
}

fn transport_mut(
    p: &mut Project,
    deck: DeckId,
    layer: LayerId,
    column: ColumnIdx,
) -> Result<&mut Transport, Error> {
    check_cell(p, layer, column)?;
    deck_mut(p, deck)?
        .clips
        .iter_mut()
        .find(|k| k.layer == layer && k.column == column)
        .map(|k| &mut k.clip.transport)
        .ok_or(Error::NoClip)
}

fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => *n,
        _ => 1.0,
    }
}

pub fn clip_from_path(path: PathBuf) -> Clip {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Clip {
        id: Uuid::new_v4(),
        name,
        source: Source::File { path },
        transport: Transport::default(),
        track: Track::new(BlendMode::Normal),
        blend_override: None,
        fit: FitMode::default(),
    }
}

fn set_clip(
    p: &mut Project,
    deck: DeckId,
    layer: LayerId,
    column: ColumnIdx,
    clip: Clip,
) -> Result<Applied, Error> {
    check_cell(p, layer, column)?;
    let deck = deck_mut(p, deck)?;
    let deck_id = deck.id;
    let prev = deck.clip_at(layer, column).cloned();
    deck.set_clip(layer, column, clip);
    let inverse = match prev {
        Some(clip) => Command::SetClip {
            deck: deck_id,
            layer,
            column,
            clip,
        },
        None => Command::ClearCell {
            deck: deck_id,
            layer,
            column,
        },
    };
    undoable(
        inverse,
        vec![Event::CellChanged {
            deck: deck_id,
            layer,
            column,
        }],
    )
}

/// Event that starts the clip in the cell, or `NoClip` / `NotPlayable`.
fn trigger_event(
    p: &Project,
    deck: DeckId,
    layer: LayerId,
    column: ColumnIdx,
) -> Result<Event, Error> {
    deck_idx(p, deck)?;
    let clip = clip_in(p, deck, layer, column).ok_or(Error::NoClip)?;
    match &clip.source {
        Source::File { path } => Ok(Event::ClipStarted {
            deck,
            layer,
            column,
            path: path.clone(),
        }),
        Source::Capture { device } => Ok(Event::CaptureStarted {
            deck,
            layer,
            column,
            device: device.clone(),
        }),
        // Imported generators with no bundled equivalent are placeholders.
        Source::Generator { kind, .. } if vjc_fx::GENERATORS.iter().any(|(id, _)| id == kind) => {
            Ok(Event::GeneratorStarted {
                deck,
                layer,
                column,
            })
        }
        _ => Err(Error::NotPlayable),
    }
}

fn trigger_column(p: &Project, deck: DeckId, column: ColumnIdx) -> Result<Applied, Error> {
    if (column as usize) >= p.composition.columns.len() {
        return Err(Error::NoColumn);
    }
    deck_idx(p, deck)?;
    let mut events = vec![];
    for l in &p.composition.layers {
        match trigger_event(p, deck, l.id, column) {
            Ok(e) => events.push(e),
            Err(Error::NoClip) => events.push(Event::LayerCleared { layer: l.id }),
            Err(_) => events.push(Event::Skipped {
                layer: l.id,
                column,
            }),
        }
    }
    Ok(Applied {
        inverse: None,
        events,
    })
}

/// Playback rate: clip speed x beat-sync factor x master speed.
pub fn playback_rate(clip_speed: f64, sync: f64, master_speed: f64) -> f64 {
    clip_speed * sync * master_speed
}

/// Applies a command to the project. Pure: no I/O, no UI, no players.
pub fn apply(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
    match cmd {
        Command::ImportMedia {
            deck,
            layer,
            column,
            path,
        } => set_clip(p, deck, layer, column, clip_from_path(path)),
        Command::SetClip {
            deck,
            layer,
            column,
            clip,
        } => set_clip(p, deck, layer, column, clip),
        Command::ClearCell {
            deck,
            layer,
            column,
        } => {
            check_cell(p, layer, column)?;
            let deck = deck_mut(p, deck)?;
            let deck_id = deck.id;
            let clip = deck.clip_at(layer, column).cloned().ok_or(Error::NoClip)?;
            deck.clips
                .retain(|c| !(c.layer == layer && c.column == column));
            undoable(
                Command::SetClip {
                    deck: deck_id,
                    layer,
                    column,
                    clip,
                },
                vec![Event::CellChanged {
                    deck: deck_id,
                    layer,
                    column,
                }],
            )
        }
        Command::TriggerClip {
            deck,
            layer,
            column,
        } => {
            check_cell(p, layer, column)?;
            let ev = trigger_event(p, deck, layer, column)?;
            Ok(Applied {
                inverse: None,
                events: vec![ev],
            })
        }
        Command::TriggerColumn { deck, column } => trigger_column(p, deck, column),
        Command::StepColumn { deck, from, delta } => {
            let n = p.composition.columns.len() as i64;
            if n == 0 {
                return Err(Error::NoColumn);
            }
            let to = from.map_or(0, |c| i64::from(c) + i64::from(delta));
            trigger_column(p, deck, to.clamp(0, n - 1) as ColumnIdx)
        }
        Command::StepClip {
            deck,
            layer,
            from,
            delta,
        } => {
            layer_idx(p, layer)?;
            let n = p.composition.columns.len() as i64;
            let sign = if delta < 0 { -1 } else { 1 };
            let mut c = match from {
                Some(f) => i64::from(f) + sign,
                None if sign > 0 => 0,
                None => n - 1,
            };
            while (0..n).contains(&c) {
                let col = c as ColumnIdx;
                // Empty cells and placeholders are passed over.
                match trigger_event(p, deck, layer, col) {
                    Ok(ev) => {
                        return Ok(Applied {
                            inverse: None,
                            events: vec![ev],
                        });
                    }
                    Err(Error::NoClip | Error::NotPlayable) => {}
                    Err(e) => return Err(e),
                }
                c += sign;
            }
            Err(Error::NoClip)
        }
        Command::SetLayerTransition { layer, transition } => {
            let i = layer_idx(p, layer)?;
            if !transition.duration_secs.is_finite()
                || transition
                    .duration_beats
                    .is_some_and(|b| !b.is_finite() || b < 0.0)
            {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(&mut p.composition.layers[i].transition, transition);
            undoable(
                Command::SetLayerTransition {
                    layer,
                    transition: prev,
                },
                vec![],
            )
        }
        Command::SetLayerAutopilot { layer, autopilot } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].autopilot, autopilot);
            undoable(
                Command::SetLayerAutopilot {
                    layer,
                    autopilot: prev,
                },
                vec![],
            )
        }
        Command::AddCue {
            deck,
            layer,
            column,
            pos,
        } => {
            if !pos.is_finite() || !(0.0..=1.0).contains(&pos) {
                return Err(Error::InvalidValue);
            }
            let cues = &mut transport_mut(p, deck, layer, column)?.cue_points;
            if cues.contains(&pos) {
                return Err(Error::InvalidValue);
            }
            cues.push(pos);
            cues.sort_by(f64::total_cmp);
            undoable(
                Command::RemoveCue {
                    deck,
                    layer,
                    column,
                    pos,
                },
                vec![],
            )
        }
        Command::RemoveCue {
            deck,
            layer,
            column,
            pos,
        } => {
            let cues = &mut transport_mut(p, deck, layer, column)?.cue_points;
            let i = cues
                .iter()
                .position(|c| *c == pos)
                .ok_or(Error::InvalidValue)?;
            cues.remove(i);
            undoable(
                Command::AddCue {
                    deck,
                    layer,
                    column,
                    pos,
                },
                vec![],
            )
        }
        Command::JumpToCue {
            deck,
            layer,
            column,
            index,
        } => {
            let pos = *transport_mut(p, deck, layer, column)?
                .cue_points
                .get(index)
                .ok_or(Error::InvalidValue)?;
            Ok(Applied {
                inverse: None,
                events: vec![Event::Seek {
                    deck,
                    layer,
                    column,
                    pos,
                }],
            })
        }
        Command::ClearAll => Ok(Applied {
            inverse: None,
            events: p
                .composition
                .layers
                .iter()
                .map(|l| Event::LayerCleared { layer: l.id })
                .collect(),
        }),
        Command::StartRecording => Ok(Applied {
            inverse: None,
            events: vec![Event::RecordingStarted],
        }),
        Command::StopRecording => Ok(Applied {
            inverse: None,
            events: vec![Event::RecordingStopped],
        }),
        Command::SetMasterBypass { bypass } => Ok(Applied {
            inverse: None,
            events: vec![Event::MasterBypass { bypass }],
        }),
        Command::SetMasterOpacity { value } => {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(Error::InvalidValue);
            }
            let op = &mut p.composition.master.opacity;
            let prev = number(&op.value);
            op.set_number(value);
            undoable(Command::SetMasterOpacity { value: prev }, vec![])
        }
        Command::SetMasterSpeed { value } => {
            if !value.is_finite() || !(0.0..=4.0).contains(&value) {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(&mut p.composition.master_speed, value);
            undoable(Command::SetMasterSpeed { value: prev }, vec![])
        }
        Command::SetGenerator {
            deck,
            layer,
            column,
            kind,
            name,
            params,
        } => {
            let mut clip = clip_from_path(PathBuf::new());
            clip.name = name;
            clip.source = Source::Generator { kind, params };
            set_clip(p, deck, layer, column, clip)
        }
        Command::SetGeneratorParam {
            deck,
            layer,
            column,
            param,
            value,
        } => {
            check_cell(p, layer, column)?;
            let clip = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .map(|k| &mut k.clip)
                .ok_or(Error::NoClip)?;
            let Source::Generator { params, .. } = &mut clip.source else {
                return Err(Error::NotPlayable);
            };
            let prm = params.get_mut(param).ok_or(Error::InvalidValue)?;
            if std::mem::discriminant(&prm.value) != std::mem::discriminant(&value) {
                return Err(Error::InvalidValue);
            }
            let prev = prm.value.clone();
            match value {
                Value::Number(n) if n.is_finite() => prm.set_number(n),
                Value::Number(_) => return Err(Error::InvalidValue),
                Value::Choice { selected, .. } => match &mut prm.value {
                    Value::Choice {
                        selected: s,
                        options,
                    } if (selected as usize) < options.len() => *s = selected,
                    _ => return Err(Error::InvalidValue),
                },
                v => prm.value = v,
            }
            undoable(
                Command::SetGeneratorParam {
                    deck,
                    layer,
                    column,
                    param,
                    value: prev,
                },
                vec![],
            )
        }
        Command::ClearLayer { layer } => {
            layer_idx(p, layer)?;
            Ok(Applied {
                inverse: None,
                events: vec![Event::LayerCleared { layer }],
            })
        }
        Command::SetOpacity { layer, value } => {
            let i = layer_idx(p, layer)?;
            let op = &mut p.composition.layers[i].track.opacity;
            let prev = number(&op.value);
            op.set_number(value);
            undoable(Command::SetOpacity { layer, value: prev }, vec![])
        }
        Command::SetBlend { layer, blend } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].track.blend, blend);
            undoable(Command::SetBlend { layer, blend: prev }, vec![])
        }
        Command::SetLayerBus { layer, bus } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].crossfader_bus, bus);
            undoable(Command::SetLayerBus { layer, bus: prev }, vec![])
        }
        Command::SetCrossfader { value } => {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(Error::InvalidValue);
            }
            let cf = p
                .composition
                .crossfader
                .get_or_insert_with(crossfader::new_crossfader);
            let prev = number(&cf.position.value);
            cf.position.set_number(value);
            undoable(Command::SetCrossfader { value: prev }, vec![])
        }
        Command::SetCrossfaderCurve { curve } => {
            let cf = p
                .composition
                .crossfader
                .get_or_insert_with(crossfader::new_crossfader);
            let prev = crossfader::CrossfadeCurve::from_name(&cf.curve);
            cf.curve = curve.name().to_string();
            undoable(Command::SetCrossfaderCurve { curve: prev }, vec![])
        }
        Command::AddScreen { screen, index } => {
            let id = screen.id;
            let at = index.min(p.output.screens.len());
            p.output.screens.insert(at, screen);
            undoable(Command::RemoveScreen { screen: id }, vec![])
        }
        Command::RemoveScreen { screen } => {
            let i = screen_idx(p, screen)?;
            let s = p.output.screens.remove(i);
            undoable(
                Command::AddScreen {
                    screen: s,
                    index: i,
                },
                vec![],
            )
        }
        Command::SetScreenDevice { screen, device } => {
            let s = screen_mut(p, screen)?;
            let prev = std::mem::replace(&mut s.device, device);
            undoable(
                Command::SetScreenDevice {
                    screen,
                    device: prev,
                },
                vec![],
            )
        }
        Command::SetScreenSize {
            screen,
            width,
            height,
        } => {
            if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
                return Err(Error::InvalidValue);
            }
            let s = screen_mut(p, screen)?;
            let inverse = Command::RestoreScreen {
                screen,
                width: s.width,
                height: s.height,
                meshes: s.slices.iter().map(|x| x.output_mesh.clone()).collect(),
            };
            let (kx, ky) = (
                width as f32 / s.width as f32,
                height as f32 / s.height as f32,
            );
            for sl in &mut s.slices {
                for q in &mut sl.output_mesh {
                    *q = (q.0 * kx, q.1 * ky);
                }
            }
            (s.width, s.height) = (width, height);
            undoable(inverse, vec![])
        }
        Command::RestoreScreen {
            screen,
            width,
            height,
            meshes,
        } => {
            let s = screen_mut(p, screen)?;
            let inverse = Command::SetScreenSize {
                screen,
                width: s.width,
                height: s.height,
            };
            (s.width, s.height) = (width, height);
            for (sl, m) in s.slices.iter_mut().zip(meshes) {
                sl.output_mesh = m;
            }
            undoable(inverse, vec![])
        }
        Command::AddSlice {
            screen,
            slice,
            index,
        } => {
            let s = screen_mut(p, screen)?;
            let id = slice.id;
            let at = index.min(s.slices.len());
            s.slices.insert(at, slice);
            undoable(Command::RemoveSlice { screen, slice: id }, vec![])
        }
        Command::RemoveSlice { screen, slice } => {
            let s = screen_mut(p, screen)?;
            let i = s
                .slices
                .iter()
                .position(|x| x.id == slice)
                .ok_or(Error::NoSlice)?;
            let sl = s.slices.remove(i);
            undoable(
                Command::AddSlice {
                    screen,
                    slice: sl,
                    index: i,
                },
                vec![],
            )
        }
        Command::SetSliceInput {
            screen,
            slice,
            input,
        } => {
            if input.len() < 3 || input.iter().any(|q| !q.0.is_finite() || !q.1.is_finite()) {
                return Err(Error::InvalidValue);
            }
            let s = slice_mut(p, screen, slice)?;
            let prev = std::mem::replace(&mut s.input, input);
            undoable(
                Command::SetSliceInput {
                    screen,
                    slice,
                    input: prev,
                },
                vec![],
            )
        }
        Command::SetSliceMesh {
            screen,
            slice,
            mesh,
            cols,
        } => {
            if !mapping::mesh_valid(&mesh, cols) {
                return Err(Error::InvalidValue);
            }
            let s = slice_mut(p, screen, slice)?;
            let inverse = Command::SetSliceMesh {
                screen,
                slice,
                mesh: std::mem::replace(&mut s.output_mesh, mesh),
                cols: s.mesh_cols,
            };
            s.mesh_cols = cols;
            undoable(inverse, vec![])
        }
        Command::SetSliceMeshSize {
            screen,
            slice,
            cols,
            rows,
        } => {
            let r = mapping::MIN_GRID..=mapping::MAX_GRID;
            if !r.contains(&cols) || !r.contains(&rows) {
                return Err(Error::InvalidValue);
            }
            let s = slice_mut(p, screen, slice)?;
            if !mapping::mesh_valid(&s.output_mesh, s.mesh_cols) {
                return Err(Error::InvalidValue);
            }
            let new = mapping::resample_mesh(&s.output_mesh, s.mesh_cols, cols, rows);
            let inverse = Command::SetSliceMesh {
                screen,
                slice,
                mesh: std::mem::replace(&mut s.output_mesh, new),
                cols: s.mesh_cols,
            };
            s.mesh_cols = cols;
            undoable(inverse, vec![])
        }
        Command::SetCompositionName { name } => {
            let prev = std::mem::replace(&mut p.composition.name, name);
            undoable(Command::SetCompositionName { name: prev }, vec![])
        }
        Command::SetCompositionSize { width, height, fps } => {
            let ok = (1..=16384).contains(&width) && (1..=16384).contains(&height);
            if !ok || !fps.is_finite() || fps <= 0.0 {
                return Err(Error::InvalidValue);
            }
            let c = &mut p.composition;
            let inverse = Command::SetCompositionSize {
                width: c.width,
                height: c.height,
                fps: c.fps,
            };
            (c.width, c.height, c.fps) = (width, height, fps);
            undoable(inverse, vec![])
        }
        Command::RenameLayer { layer, name } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].name, name);
            undoable(Command::RenameLayer { layer, name: prev }, vec![])
        }
        Command::SetLayerSolo { layer, solo } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].solo, solo);
            undoable(Command::SetLayerSolo { layer, solo: prev }, vec![])
        }
        Command::SetLayerBypass { layer, bypass } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].bypass, bypass);
            undoable(
                Command::SetLayerBypass {
                    layer,
                    bypass: prev,
                },
                vec![],
            )
        }
        Command::RenameClip {
            deck,
            layer,
            column,
            name,
        } => {
            check_cell(p, layer, column)?;
            let k = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = std::mem::replace(&mut k.clip.name, name);
            undoable(
                Command::RenameClip {
                    deck,
                    layer,
                    column,
                    name: prev,
                },
                vec![],
            )
        }
        Command::SetFit {
            deck,
            layer,
            column,
            fit,
        } => {
            check_cell(p, layer, column)?;
            let k = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = std::mem::replace(&mut k.clip.fit, fit);
            undoable(
                Command::SetFit {
                    deck,
                    layer,
                    column,
                    fit: prev,
                },
                vec![],
            )
        }
        Command::AddLayer { layer, index } => {
            let id = layer.id;
            let ls = &mut p.composition.layers;
            ls.insert(index.min(ls.len()), layer);
            undoable(Command::RemoveLayer { layer: id }, vec![])
        }
        Command::RemoveLayer { layer } => {
            let i = layer_idx(p, layer)?;
            if p.composition
                .decks
                .iter()
                .any(|d| d.clips.iter().any(|c| c.layer == layer))
            {
                return Err(Error::InUse);
            }
            let removed = p.composition.layers.remove(i);
            undoable(
                Command::AddLayer {
                    layer: removed,
                    index: i,
                },
                vec![Event::LayerCleared { layer }],
            )
        }
        Command::AddColumn { column } => {
            let id = column.id;
            p.composition.columns.push(column);
            undoable(Command::RemoveColumn { column: id }, vec![])
        }
        Command::RemoveColumn { column } => {
            let i = p
                .composition
                .columns
                .iter()
                .position(|c| c.id == column)
                .ok_or(Error::NoColumn)?;
            let last = p.composition.columns.len() - 1;
            if i != last
                || p.composition
                    .decks
                    .iter()
                    .any(|d| d.clips.iter().any(|c| c.column as usize == i))
            {
                return Err(Error::InUse);
            }
            let removed = p.composition.columns.remove(i);
            undoable(Command::AddColumn { column: removed }, vec![])
        }
        Command::SetBeatSync {
            deck,
            layer,
            column,
            beats,
        } => {
            check_cell(p, layer, column)?;
            if beats.is_some_and(|b| !b.is_finite() || b <= 0.0) {
                return Err(Error::InvalidValue);
            }
            let k = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = std::mem::replace(&mut k.clip.transport.beat_sync, beats);
            undoable(
                Command::SetBeatSync {
                    deck,
                    layer,
                    column,
                    beats: prev,
                },
                vec![],
            )
        }
        Command::SetPlayMode {
            deck,
            layer,
            column,
            mode,
        } => {
            let prev =
                std::mem::replace(&mut transport_mut(p, deck, layer, column)?.play_mode, mode);
            undoable(
                Command::SetPlayMode {
                    deck,
                    layer,
                    column,
                    mode: prev,
                },
                vec![],
            )
        }
        Command::SetDirection {
            deck,
            layer,
            column,
            direction,
        } => {
            let t = transport_mut(p, deck, layer, column)?;
            let prev = std::mem::replace(&mut t.direction, direction);
            undoable(
                Command::SetDirection {
                    deck,
                    layer,
                    column,
                    direction: prev,
                },
                vec![],
            )
        }
        Command::SetSpeed {
            deck,
            layer,
            column,
            speed,
        } => {
            if !speed.is_finite() || !(0.0..=4.0).contains(&speed) {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(&mut transport_mut(p, deck, layer, column)?.speed, speed);
            undoable(
                Command::SetSpeed {
                    deck,
                    layer,
                    column,
                    speed: prev,
                },
                vec![],
            )
        }
        Command::Tap { at } => Ok(Applied {
            inverse: None,
            events: vec![Event::Tap { at }],
        }),
        Command::Nudge { dir, at } => Ok(Applied {
            inverse: None,
            events: vec![Event::Nudge { dir, at }],
        }),
        Command::Resync { at } => Ok(Applied {
            inverse: None,
            events: vec![Event::Resync { at }],
        }),
        Command::AddDeck {
            deck,
            index,
            select,
        } => {
            let id = deck.id;
            // Validate before mutating so an error leaves the project unchanged.
            if let Some(s) = select
                && s != id
            {
                deck_idx(p, s)?;
            }
            let ds = &mut p.composition.decks;
            ds.insert(index.min(ds.len()), deck);
            let prev = p.composition.current_deck;
            if let Some(s) = select {
                p.composition.current_deck = s;
            }
            undoable(
                Command::RemoveDeck {
                    deck: id,
                    select: Some(prev),
                },
                vec![],
            )
        }
        Command::RemoveDeck { deck, select } => {
            let i = deck_idx(p, deck)?;
            if p.composition.decks.len() == 1 {
                return Err(Error::InUse);
            }
            let removed = p.composition.decks.remove(i);
            let c = &mut p.composition;
            let was_current = c.current_deck == deck;
            if was_current {
                c.current_deck = c.decks[i.min(c.decks.len() - 1)].id;
            }
            if let Some(s) = select
                && c.decks.iter().any(|d| d.id == s)
            {
                c.current_deck = s;
            }
            undoable(
                Command::AddDeck {
                    deck: removed,
                    index: i,
                    select: was_current.then_some(deck),
                },
                vec![Event::DeckRemoved { deck }],
            )
        }
        Command::RenameDeck { deck, name } => {
            let i = deck_idx(p, deck)?;
            let prev = std::mem::replace(&mut p.composition.decks[i].name, name);
            undoable(Command::RenameDeck { deck, name: prev }, vec![])
        }
        Command::MoveDeck { deck, index } => {
            let i = deck_idx(p, deck)?;
            let ds = &mut p.composition.decks;
            let d = ds.remove(i);
            let to = index.min(ds.len());
            ds.insert(to, d);
            undoable(Command::MoveDeck { deck, index: i }, vec![])
        }
        Command::SelectDeck { deck } => {
            deck_idx(p, deck)?;
            p.composition.current_deck = deck;
            Ok(Applied {
                inverse: None,
                events: vec![],
            })
        }
        Command::SetAnimation { target, animation } => {
            let prm = anim_param_mut(p, target)?;
            if !matches!(prm.value, Value::Number(_)) {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(&mut prm.animation, animation);
            undoable(
                Command::SetAnimation {
                    target,
                    animation: prev,
                },
                vec![],
            )
        }
        Command::SetDashboardDial { dial, value } => {
            if dial as usize >= anim::DIALS || !value.is_finite() {
                return Err(Error::InvalidValue);
            }
            let d = &mut p.composition.dashboard;
            while d.len() < anim::DIALS {
                let n = d.len() + 1;
                d.push(Param::number(&format!("Dial {n}"), 0.0, Some((0.0, 1.0))));
            }
            let prm = &mut d[dial as usize];
            let prev = number(&prm.value);
            prm.set_number(value);
            undoable(Command::SetDashboardDial { dial, value: prev }, vec![])
        }
        Command::AddEffect { .. }
        | Command::RemoveEffect { .. }
        | Command::MoveEffect { .. }
        | Command::SetEffectBypass { .. }
        | Command::SetEffectParam { .. } => effects::apply(p, cmd),
        Command::AddShortcut { .. } | Command::RemoveShortcut { .. } => shortcuts::apply(p, cmd),
        Command::SetBpm { bpm } => {
            if !bpm.is_finite() || bpm <= 0.0 {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(&mut p.composition.tempo.bpm, bpm);
            undoable(Command::SetBpm { bpm: prev }, vec![])
        }
    }
}

struct Entry {
    undo: Command,
    redo: Command,
    target: Option<Target>,
    at: Instant,
    /// Set after undo/redo so later commands never merge into this entry.
    sealed: bool,
}

#[derive(Default)]
pub struct History {
    done: Vec<Entry>,
    undone: Vec<Entry>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }

    /// Applies `cmd` and records it. `now` is injected so tests control time.
    pub fn exec(
        &mut self,
        p: &mut Project,
        cmd: Command,
        now: Instant,
    ) -> Result<Vec<Event>, Error> {
        // Resolve the clip id now so redo recreates the very same clip.
        let cmd = match cmd {
            Command::ImportMedia {
                deck,
                layer,
                column,
                path,
            } => Command::SetClip {
                deck,
                layer,
                column,
                clip: clip_from_path(path),
            },
            c => c,
        };
        let target = cmd.target();
        let Applied { inverse, events } = apply(p, cmd.clone())?;
        let Some(undo) = inverse else {
            return Ok(events);
        };
        self.undone.clear();
        if let (Some(t), Some(last)) = (target, self.done.last_mut())
            && last.target == Some(t)
            && !last.sealed
            && now.saturating_duration_since(last.at) <= COALESCE_WINDOW
        {
            last.redo = cmd;
            last.at = now;
            return Ok(events);
        }
        self.done.push(Entry {
            undo,
            redo: cmd,
            target,
            at: now,
            sealed: false,
        });
        Ok(events)
    }

    /// Returns None when there is nothing to undo.
    pub fn undo(&mut self, p: &mut Project) -> Option<Result<Vec<Event>, Error>> {
        let mut e = self.done.pop()?;
        let r = apply(p, e.undo.clone());
        if r.is_ok() {
            e.sealed = true;
            self.undone.push(e);
        } else {
            // Keep the entry so a failed undo does not drop history.
            self.done.push(e);
        }
        Some(r.map(|a| a.events))
    }

    /// Returns None when there is nothing to redo.
    pub fn redo(&mut self, p: &mut Project) -> Option<Result<Vec<Event>, Error>> {
        let mut e = self.undone.pop()?;
        let r = apply(p, e.redo.clone());
        if r.is_ok() {
            e.sealed = true;
            self.done.push(e);
        } else {
            self.undone.push(e);
        }
        Some(r.map(|a| a.events))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn transition_autopilot_cues_undo_and_jump_is_runtime() {
        let (mut p, l) = setup();
        let mut h = History::default();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let before = p.clone();
        let cmds = [
            Command::SetLayerTransition {
                layer: l,
                transition: vjc_model::Transition {
                    kind: "Dissolve".into(),
                    duration_beats: Some(2.0),
                    duration_secs: 0.5,
                },
            },
            Command::SetLayerAutopilot {
                layer: l,
                autopilot: vjc_model::Autopilot {
                    mode: vjc_model::AutopilotMode::Random,
                    every_loops: 3,
                },
            },
            Command::AddCue {
                deck: A,
                layer: l,
                column: 0,
                pos: 0.7,
            },
            Command::AddCue {
                deck: A,
                layer: l,
                column: 0,
                pos: 0.2,
            },
        ];
        for c in cmds {
            h.exec(&mut p, c, t).unwrap();
        }
        let cues = clip_in(&p, A, l, 0).unwrap().transport.cue_points.clone();
        assert_eq!(cues, vec![0.2, 0.7]);
        let ev = h
            .exec(
                &mut p,
                Command::JumpToCue {
                    deck: A,
                    layer: l,
                    column: 0,
                    index: 1,
                },
                t,
            )
            .unwrap();
        assert!(matches!(ev[..], [Event::Seek { pos, .. }] if pos == 0.7));
        h.exec(
            &mut p,
            Command::RemoveCue {
                deck: A,
                layer: l,
                column: 0,
                pos: 0.2,
            },
            t,
        )
        .unwrap();
        for _ in 0..5 {
            h.undo(&mut p).unwrap().unwrap();
        }
        assert_eq!(p, before);
    }

    #[test]
    fn panel_commands_undo_redo() {
        let (mut p, l) = setup();
        let mut h = History::default();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let cmds = [
            Command::SetCompositionName {
                name: "Show".into(),
            },
            Command::SetCompositionSize {
                width: 1280,
                height: 720,
                fps: 25.0,
            },
            Command::RenameLayer {
                layer: l,
                name: "Bg".into(),
            },
            Command::SetLayerSolo {
                layer: l,
                solo: true,
            },
            Command::SetLayerBypass {
                layer: l,
                bypass: true,
            },
            Command::RenameClip {
                deck: A,
                layer: l,
                column: 0,
                name: "Intro".into(),
            },
        ];
        let before = p.clone();
        for c in cmds {
            h.exec(&mut p, c, t).unwrap();
        }
        let after = p.clone();
        assert_eq!(
            (after.composition.width, after.composition.fps),
            (1280, 25.0)
        );
        assert_eq!(clip(&after, l).unwrap().name, "Intro");
        for _ in 0..6 {
            h.undo(&mut p).unwrap().unwrap();
        }
        assert_eq!(p, before);
        for _ in 0..6 {
            h.redo(&mut p).unwrap().unwrap();
        }
        assert_eq!(p, after);
    }

    #[test]
    fn composition_size_rejects_invalid_and_clip_needs_clip() {
        let (mut p, l) = setup();
        for (w, h, f) in [
            (0, 10, 30.0),
            (10, 20000, 30.0),
            (10, 10, 0.0),
            (10, 10, f32::NAN),
        ] {
            let c = Command::SetCompositionSize {
                width: w,
                height: h,
                fps: f,
            };
            assert_eq!(apply(&mut p, c).unwrap_err(), Error::InvalidValue);
        }
        let c = Command::RenameClip {
            deck: A,
            layer: l,
            column: 0,
            name: "x".into(),
        };
        assert_eq!(apply(&mut p, c).unwrap_err(), Error::NoClip);
    }

    #[test]
    fn bpm_rejects_invalid() {
        let mut p = vjc_model::Project::new("t");
        for bad in [f64::NAN, 0.0, -1.0, f64::INFINITY] {
            assert!(super::apply(&mut p, super::Command::SetBpm { bpm: bad }).is_err());
        }
    }

    use super::*;

    const A: DeckId = Uuid::from_u128(1);
    use vjc_model::{ShortcutMode, Transition};

    fn layer(n: usize) -> Layer {
        Layer {
            id: Uuid::new_v4(),
            name: format!("L{n}"),
            track: Track::new(BlendMode::Normal),
            solo: false,
            bypass: false,
            transition: Transition {
                kind: "Cut".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            autopilot: Default::default(),
            crossfader_bus: None,
            group: None,
        }
    }

    fn setup() -> (Project, LayerId) {
        let mut p = Project::new("t");
        p.composition.decks[0].id = A;
        p.composition.current_deck = A;
        let l = layer(1);
        let id = l.id;
        p.composition.layers.push(l);
        p.composition.columns.push(Column {
            id: Uuid::new_v4(),
            name: "1".into(),
        });
        (p, id)
    }

    fn import(p: &mut Project, h: &mut History, l: LayerId, t: Instant) {
        h.exec(
            p,
            Command::ImportMedia {
                deck: A,
                layer: l,
                column: 0,
                path: "/x/a.mov".into(),
            },
            t,
        )
        .unwrap();
    }

    fn clip(p: &Project, l: LayerId) -> Option<Clip> {
        let c = &p.composition;
        c.decks
            .iter()
            .find(|d| d.id == c.current_deck)?
            .clip_at(l, 0)
            .cloned()
    }

    /// Runs `cmd`, then checks undo restores the exact project and redo reapplies.
    fn roundtrip(p: &mut Project, cmd: Command) {
        let before = p.clone();
        let mut h = History::new();
        h.exec(p, cmd, Instant::now()).unwrap();
        let after = p.clone();
        assert_ne!(before, after);
        h.undo(p).unwrap().unwrap();
        assert_eq!(*p, before);
        h.redo(p).unwrap().unwrap();
        assert_eq!(*p, after);
    }

    #[test]
    fn failed_undo_keeps_history() {
        let (mut p, l) = setup();
        let mut h = History::new();
        h.exec(
            &mut p,
            Command::ImportMedia {
                deck: A,
                layer: l,
                column: 0,
                path: "/x/a.mov".into(),
            },
            Instant::now(),
        )
        .unwrap();
        // Break the state so the inverse cannot apply.
        p.composition.layers.retain(|x| x.id != l);
        assert!(h.undo(&mut p).unwrap().is_err());
        assert!(h.can_undo());
    }

    #[test]
    fn import_and_clear_cell() {
        let (mut p, l) = setup();
        roundtrip(
            &mut p,
            Command::ImportMedia {
                deck: A,
                layer: l,
                column: 0,
                path: "/x/a.mov".into(),
            },
        );
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        assert_eq!(clip(&p, l).unwrap().name, "a");
        roundtrip(
            &mut p,
            Command::ClearCell {
                deck: A,
                layer: l,
                column: 0,
            },
        );
    }

    #[test]
    fn set_clip_replaces_and_undo_restores_previous() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        roundtrip(
            &mut p,
            Command::SetClip {
                deck: A,
                layer: l,
                column: 0,
                clip: clip_from_path("/x/b.mov".into()),
            },
        );
        assert_eq!(clip(&p, l).unwrap().name, "b");
    }

    #[test]
    fn trigger_and_clear_layer_emit_events_not_history() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let ev = h
            .exec(
                &mut p,
                Command::TriggerClip {
                    deck: A,
                    layer: l,
                    column: 0,
                },
                Instant::now(),
            )
            .unwrap();
        assert_eq!(
            ev,
            vec![Event::ClipStarted {
                deck: p.composition.current_deck,
                layer: l,
                column: 0,
                path: "/x/a.mov".into()
            }]
        );
        let ev = h
            .exec(&mut p, Command::ClearLayer { layer: l }, Instant::now())
            .unwrap();
        assert_eq!(ev, vec![Event::LayerCleared { layer: l }]);
        // Only the import is in history.
        h.undo(&mut p).unwrap().unwrap();
        assert!(!h.can_undo());
        assert_eq!(
            apply(
                &mut p,
                Command::TriggerClip {
                    deck: A,
                    layer: l,
                    column: 0
                }
            ),
            Err(Error::NoClip)
        );
    }

    #[test]
    fn trigger_capture_source_starts_capture() {
        let (mut p, l) = setup();
        let mut c = clip_from_path("/x/a.mov".into());
        c.source = Source::Capture {
            device: "cam".into(),
        };
        let set = Command::SetClip {
            deck: A,
            layer: l,
            column: 0,
            clip: c,
        };
        apply(&mut p, set).unwrap();
        let a = apply(
            &mut p,
            Command::TriggerClip {
                deck: A,
                layer: l,
                column: 0,
            },
        )
        .unwrap();
        assert!(matches!(&a.events[0], Event::CaptureStarted { device, .. } if device == "cam"));
    }

    #[test]
    fn trigger_non_file_source_fails() {
        let (mut p, l) = setup();
        let mut c = clip_from_path("/x/a.mov".into());
        c.source = Source::Network {
            protocol: vjc_model::NetProtocol::Ndi,
            name: "feed".into(),
        };
        apply(
            &mut p,
            Command::SetClip {
                deck: A,
                layer: l,
                column: 0,
                clip: c,
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut p,
                Command::TriggerClip {
                    deck: A,
                    layer: l,
                    column: 0
                }
            ),
            Err(Error::NotPlayable)
        );
    }

    #[test]
    fn opacity_blend_fit() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        roundtrip(
            &mut p,
            Command::SetOpacity {
                layer: l,
                value: 0.25,
            },
        );
        roundtrip(
            &mut p,
            Command::SetBlend {
                layer: l,
                blend: BlendMode::Add,
            },
        );
        roundtrip(
            &mut p,
            Command::SetFit {
                deck: A,
                layer: l,
                column: 0,
                fit: FitMode::Fill,
            },
        );
    }

    #[test]
    fn play_mode_and_direction_roundtrip() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        for mode in [PlayMode::Bounce, PlayMode::Once, PlayMode::OnceHold] {
            roundtrip(
                &mut p,
                Command::SetPlayMode {
                    deck: A,
                    layer: l,
                    column: 0,
                    mode,
                },
            );
        }
        for direction in [Direction::Backward, Direction::Pause, Direction::Random] {
            roundtrip(
                &mut p,
                Command::SetDirection {
                    deck: A,
                    layer: l,
                    column: 0,
                    direction,
                },
            );
        }
        let missing = Command::SetDirection {
            deck: A,
            layer: l,
            column: 1,
            direction: Direction::Pause,
        };
        assert!(apply(&mut p, missing).is_err());
    }

    #[test]
    fn speed_validates_and_coalesces() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let set = |speed| Command::SetSpeed {
            deck: A,
            layer: l,
            column: 0,
            speed,
        };
        for bad in [-0.1, 4.1, f64::NAN, f64::INFINITY] {
            assert_eq!(apply(&mut p, set(bad)), Err(Error::InvalidValue));
        }
        roundtrip(&mut p, set(2.5));
        for (i, v) in [0.5, 1.5, 3.0].into_iter().enumerate() {
            h.exec(&mut p, set(v), t + Duration::from_millis(100 * i as u64))
                .unwrap();
        }
        assert_eq!(clip(&p, l).unwrap().transport.speed, 3.0);
        // One undo step undoes the whole drag; the import is the next one.
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(clip(&p, l).unwrap().transport.speed, 2.5);
        // Outside the window a new step starts.
        let t2 = t + Duration::from_secs(5);
        h.exec(&mut p, set(2.0), t2).unwrap();
        h.exec(&mut p, set(2.0 + 1.0), t2 + COALESCE_WINDOW * 2)
            .unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(clip(&p, l).unwrap().transport.speed, 2.0);
    }

    #[test]
    fn beat_sync_roundtrip_and_validation() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let set = |beats| Command::SetBeatSync {
            deck: A,
            layer: l,
            column: 0,
            beats,
        };
        roundtrip(&mut p, set(Some(4.0)));
        assert_eq!(apply(&mut p, set(Some(0.0))), Err(Error::InvalidValue));
    }

    #[test]
    fn clock_inputs_are_events_not_history() {
        let (mut p, _) = setup();
        let mut h = History::new();
        let at = Instant::now();
        let ev = h.exec(&mut p, Command::Tap { at }, at).unwrap();
        assert_eq!(ev, vec![Event::Tap { at }]);
        h.exec(&mut p, Command::Resync { at }, at).unwrap();
        h.exec(&mut p, Command::Nudge { dir: 1, at }, at).unwrap();
        assert!(!h.can_undo());
    }

    #[test]
    fn opacity_clamps() {
        let (mut p, l) = setup();
        apply(
            &mut p,
            Command::SetOpacity {
                layer: l,
                value: 7.0,
            },
        )
        .unwrap();
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 1.0);
    }

    #[test]
    fn add_remove_layer_and_column() {
        let (mut p, l) = setup();
        roundtrip(
            &mut p,
            Command::AddLayer {
                layer: layer(2),
                index: 0,
            },
        );
        roundtrip(
            &mut p,
            Command::AddColumn {
                column: Column {
                    id: Uuid::new_v4(),
                    name: "2".into(),
                },
            },
        );
        let nl = layer(3);
        let nid = nl.id;
        apply(
            &mut p,
            Command::AddLayer {
                layer: nl,
                index: 1,
            },
        )
        .unwrap();
        roundtrip(&mut p, Command::RemoveLayer { layer: nid });
        // Non-empty layer and non-last column cannot be removed.
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        assert_eq!(
            apply(&mut p, Command::RemoveLayer { layer: l }),
            Err(Error::InUse)
        );
        let first = p.composition.columns[0].id;
        assert_eq!(
            apply(&mut p, Command::RemoveColumn { column: first }),
            Err(Error::InUse)
        );
    }

    #[test]
    fn set_bpm() {
        let (mut p, _) = setup();
        roundtrip(&mut p, Command::SetBpm { bpm: 99.0 });
    }

    #[test]
    fn unknown_targets_error() {
        let (mut p, l) = setup();
        let ghost = Uuid::new_v4();
        assert_eq!(
            apply(
                &mut p,
                Command::SetOpacity {
                    layer: ghost,
                    value: 0.5
                }
            ),
            Err(Error::NoLayer)
        );
        assert_eq!(
            apply(
                &mut p,
                Command::ClearCell {
                    deck: A,
                    layer: l,
                    column: 9
                }
            ),
            Err(Error::NoColumn)
        );
        assert_eq!(
            apply(
                &mut p,
                Command::ClearCell {
                    deck: A,
                    layer: l,
                    column: 0
                }
            ),
            Err(Error::NoClip)
        );
    }

    #[test]
    fn slider_drag_coalesces_into_one_step() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t0 = Instant::now();
        // Each event within 500 ms of the previous one, total drag > 500 ms.
        for (i, v) in [0.9, 0.8, 0.7, 0.6, 0.5].into_iter().enumerate() {
            let t = t0 + Duration::from_millis(i as u64 * 300);
            h.exec(&mut p, Command::SetOpacity { layer: l, value: v }, t)
                .unwrap();
        }
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 0.5);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 1.0);
        assert!(!h.can_undo());
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 0.5);
    }

    #[test]
    fn no_coalesce_after_gap_other_target_or_other_command() {
        let (mut p, l) = setup();
        let l2 = layer(2);
        let l2id = l2.id;
        p.composition.layers.push(l2);
        let mut h = History::new();
        let t0 = Instant::now();
        let op = |layer, value| Command::SetOpacity { layer, value };
        h.exec(&mut p, op(l, 0.9), t0).unwrap();
        h.exec(&mut p, op(l, 0.8), t0 + Duration::from_millis(600))
            .unwrap(); // gap
        h.exec(&mut p, op(l2id, 0.5), t0 + Duration::from_millis(700))
            .unwrap(); // other layer
        h.exec(
            &mut p,
            Command::SetBpm { bpm: 90.0 },
            t0 + Duration::from_millis(710),
        )
        .unwrap();
        h.exec(&mut p, op(l, 0.1), t0 + Duration::from_millis(720))
            .unwrap();
        let mut n = 0;
        while h.undo(&mut p).is_some() {
            n += 1;
        }
        assert_eq!(n, 5);
    }

    #[test]
    fn undo_seals_and_new_command_clears_redo() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t0 = Instant::now();
        let op = |value| Command::SetOpacity { layer: l, value };
        h.exec(&mut p, op(0.5), t0).unwrap();
        h.exec(&mut p, op(0.4), t0 + Duration::from_millis(10))
            .unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert!(h.can_redo());
        h.redo(&mut p).unwrap().unwrap();
        // Right after redo, a new drag must be a separate step.
        h.exec(&mut p, op(0.3), t0 + Duration::from_millis(20))
            .unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 0.4);
        h.exec(&mut p, op(0.2), t0 + Duration::from_secs(5))
            .unwrap();
        assert!(!h.can_redo());
    }

    fn new_deck(name: &str) -> Deck {
        Deck {
            id: Uuid::new_v4(),
            name: name.into(),
            clips: vec![],
        }
    }

    #[test]
    fn deck_commands_undo_redo() {
        let (mut p, _) = setup();
        let first = p.composition.decks[0].id;
        roundtrip(
            &mut p,
            Command::AddDeck {
                deck: new_deck("B"),
                index: 1,
                select: None,
            },
        );
        let mut h = History::new();
        h.exec(
            &mut p,
            Command::AddDeck {
                deck: new_deck("B"),
                index: 1,
                select: None,
            },
            Instant::now(),
        )
        .unwrap();
        let b = p.composition.decks[1].id;
        roundtrip(
            &mut p,
            Command::RenameDeck {
                deck: b,
                name: "Intro".into(),
            },
        );
        roundtrip(&mut p, Command::MoveDeck { deck: b, index: 0 });
        roundtrip(
            &mut p,
            Command::RemoveDeck {
                deck: b,
                select: None,
            },
        );
        assert_ne!(first, b);
    }

    #[test]
    fn remove_deck_keeps_clips_on_undo_and_never_last() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let a = p.composition.decks[0].id;
        assert_eq!(
            apply(
                &mut p,
                Command::RemoveDeck {
                    deck: a,
                    select: None
                }
            )
            .unwrap_err(),
            Error::InUse
        );
        h.exec(
            &mut p,
            Command::AddDeck {
                deck: new_deck("B"),
                index: 1,
                select: None,
            },
            Instant::now(),
        )
        .unwrap();
        let ev = h
            .exec(
                &mut p,
                Command::RemoveDeck {
                    deck: a,
                    select: None,
                },
                Instant::now(),
            )
            .unwrap();
        assert_eq!(ev, vec![Event::DeckRemoved { deck: a }]);
        assert_ne!(p.composition.current_deck, a);
        h.undo(&mut p).unwrap().unwrap();
        assert!(clip_in(&p, a, l, 0).is_some());
    }

    #[test]
    fn select_deck_is_runtime_and_playing_lookup_ignores_it() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let a = p.composition.current_deck;
        h.exec(
            &mut p,
            Command::AddDeck {
                deck: new_deck("B"),
                index: 1,
                select: None,
            },
            Instant::now(),
        )
        .unwrap();
        let b = p.composition.decks[1].id;
        h.exec(&mut p, Command::SelectDeck { deck: b }, Instant::now())
            .unwrap();
        assert_eq!(p.composition.current_deck, b);
        // Playing clip is still found in deck A; the current deck has no clip there.
        assert_eq!(clip_in(&p, a, l, 0).unwrap().name, "a");
        assert!(clip_in(&p, b, l, 0).is_none());
        // Undo goes to AddDeck, not to a deck switch.
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.decks.len(), 1);
        assert_eq!(
            apply(&mut p, Command::SelectDeck { deck: b }).unwrap_err(),
            Error::NoDeck
        );
    }

    #[test]
    fn undo_redo_target_origin_deck_not_current() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let now = Instant::now();
        let a = p.composition.current_deck;
        import(&mut p, &mut h, l, now);
        h.exec(
            &mut p,
            Command::AddDeck {
                deck: new_deck("B"),
                index: 1,
                select: None,
            },
            now,
        )
        .unwrap();
        let b = p.composition.decks[1].id;
        h.exec(&mut p, Command::SelectDeck { deck: b }, now)
            .unwrap();
        h.exec(
            &mut p,
            Command::ImportMedia {
                deck: A,
                layer: l,
                column: 0,
                path: "/x/b.mov".into(),
            },
            now,
        )
        .unwrap();
        h.exec(&mut p, Command::SelectDeck { deck: a }, now)
            .unwrap();
        h.exec(
            &mut p,
            Command::SetFit {
                deck: A,
                layer: l,
                column: 0,
                fit: FitMode::Fill,
            },
            now,
        )
        .unwrap();
        let a_fit = clip_in(&p, a, l, 0).unwrap().fit;
        let a_before = FitMode::default();
        assert_ne!(a_fit, a_before);
        h.exec(&mut p, Command::SelectDeck { deck: b }, now)
            .unwrap();
        let b_before = p.composition.decks[1].clone();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(clip_in(&p, a, l, 0).unwrap().fit, a_before);
        assert_eq!(p.composition.decks[1], b_before);
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(clip_in(&p, a, l, 0).unwrap().fit, a_fit);
        assert_eq!(p.composition.decks[1], b_before);
    }

    #[test]
    fn speed_coalescing_is_per_deck() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let now = Instant::now();
        import(&mut p, &mut h, l, now);
        let b = new_deck("B");
        let bid = b.id;
        h.exec(
            &mut p,
            Command::AddDeck {
                deck: b,
                index: 1,
                select: None,
            },
            now,
        )
        .unwrap();
        for deck in [A, bid] {
            if deck == bid {
                h.exec(
                    &mut p,
                    Command::SetClip {
                        deck,
                        layer: l,
                        column: 0,
                        clip: clip_from_path("/x/b.mov".into()),
                    },
                    now,
                )
                .unwrap();
            }
            h.exec(
                &mut p,
                Command::SetSpeed {
                    deck,
                    layer: l,
                    column: 0,
                    speed: 2.0,
                },
                now,
            )
            .unwrap();
        }
        // Two decks, two steps: undoing the last only touches deck B.
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(clip_in(&p, bid, l, 0).unwrap().transport.speed, 1.0);
        assert_eq!(clip_in(&p, A, l, 0).unwrap().transport.speed, 2.0);
    }

    #[test]
    fn undo_deck_ops_restore_selection() {
        let (mut p, _) = setup();
        let mut h = History::new();
        let now = Instant::now();
        let b = new_deck("B");
        let bid = b.id;
        h.exec(
            &mut p,
            Command::AddDeck {
                deck: b,
                index: 1,
                select: None,
            },
            now,
        )
        .unwrap();
        h.exec(&mut p, Command::SelectDeck { deck: bid }, now)
            .unwrap();
        // Undo AddDeck while B is selected: back to A.
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.current_deck, A);
        h.redo(&mut p).unwrap().unwrap();
        h.exec(&mut p, Command::SelectDeck { deck: bid }, now)
            .unwrap();
        // Remove the current deck, undo: it is selected again.
        h.exec(
            &mut p,
            Command::RemoveDeck {
                deck: bid,
                select: None,
            },
            now,
        )
        .unwrap();
        assert_eq!(p.composition.current_deck, A);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.current_deck, bid);
        // Removing a non-current deck leaves selection alone on undo.
        h.redo(&mut p).unwrap().unwrap();
        h.exec(&mut p, Command::SelectDeck { deck: A }, now)
            .unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.current_deck, bid);
    }

    fn fx(kind: &str) -> Effect {
        Effect {
            id: Uuid::new_v4(),
            kind: kind.into(),
            bypass: false,
            dry_wet: 1.0,
            params: vec![vjc_model::Param::number("amount", 1.0, Some((0.0, 1.0)))],
        }
    }

    fn targets(p: &mut Project, l: LayerId) -> Vec<EffectTarget> {
        let mut h = History::new();
        import(p, &mut h, l, Instant::now());
        vec![
            EffectTarget::Master,
            EffectTarget::Layer(l),
            EffectTarget::Clip {
                deck: A,
                layer: l,
                column: 0,
            },
        ]
    }

    #[test]
    fn effect_chain_commands_undo_redo_on_every_target() {
        let (mut p, l) = setup();
        for t in targets(&mut p, l) {
            let (a, b) = (fx("a"), fx("b"));
            let (ia, ib) = (a.id, b.id);
            roundtrip(
                &mut p,
                Command::AddEffect {
                    target: t,
                    effect: a,
                    index: 0,
                },
            );
            let mut h = History::new();
            let now = Instant::now();
            h.exec(
                &mut p,
                Command::AddEffect {
                    target: t,
                    effect: fx("z"),
                    index: 0,
                },
                now,
            )
            .unwrap();
            h.undo(&mut p).unwrap().unwrap();
            h.exec(
                &mut p,
                Command::AddEffect {
                    target: t,
                    effect: b,
                    index: 9,
                },
                now,
            )
            .unwrap();
            let order = |p: &Project| -> Vec<EffectId> {
                effects::chain(p, t).unwrap().iter().map(|e| e.id).collect()
            };
            assert_eq!(order(&p), [ia, ib]);
            roundtrip(
                &mut p,
                Command::MoveEffect {
                    target: t,
                    effect: ib,
                    index: 0,
                },
            );
            roundtrip(
                &mut p,
                Command::SetEffectBypass {
                    target: t,
                    effect: ia,
                    bypass: true,
                },
            );
            roundtrip(
                &mut p,
                Command::SetEffectParam {
                    target: t,
                    effect: ia,
                    param: 0,
                    value: Value::Number(0.25),
                },
            );
            roundtrip(
                &mut p,
                Command::RemoveEffect {
                    target: t,
                    effect: ia,
                },
            );
        }
    }

    #[test]
    fn effect_param_validates_clamps_and_coalesces() {
        let (mut p, l) = setup();
        let t = EffectTarget::Layer(l);
        let e = fx("a");
        let id = e.id;
        let mut h = History::new();
        let t0 = Instant::now();
        h.exec(
            &mut p,
            Command::AddEffect {
                target: t,
                effect: e,
                index: 0,
            },
            t0,
        )
        .unwrap();
        let set = |value| Command::SetEffectParam {
            target: t,
            effect: id,
            param: 0,
            value,
        };
        assert_eq!(
            h.exec(&mut p, set(Value::Bool(true)), t0),
            Err(Error::InvalidValue)
        );
        assert_eq!(
            h.exec(
                &mut p,
                Command::SetEffectParam {
                    target: t,
                    effect: id,
                    param: 5,
                    value: Value::Number(0.0)
                },
                t0
            ),
            Err(Error::InvalidValue)
        );
        assert_eq!(
            h.exec(
                &mut p,
                Command::RemoveEffect {
                    target: t,
                    effect: Uuid::new_v4()
                },
                t0
            ),
            Err(Error::NoEffect)
        );
        for (i, v) in [0.9, 0.5, 5.0].into_iter().enumerate() {
            let at = t0 + Duration::from_millis(100 * (i as u64 + 1));
            h.exec(&mut p, set(Value::Number(v)), at).unwrap();
        }
        // Clamped to the range, and the whole drag is a single undo step.
        assert_eq!(
            effects::chain(&p, t).unwrap()[0].params[0].value,
            Value::Number(1.0)
        );
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(
            effects::chain(&p, t).unwrap()[0].params[0].value,
            Value::Number(1.0)
        );
        h.undo(&mut p).unwrap().unwrap();
        assert!(effects::chain(&p, t).unwrap().is_empty());
    }

    #[test]
    fn placeholder_generator_is_not_playable() {
        let (mut p, l) = setup();
        apply(
            &mut p,
            Command::SetGenerator {
                deck: A,
                layer: l,
                column: 0,
                kind: "plasma_unbundled".into(),
                name: "Plasma".into(),
                params: vec![],
            },
        )
        .unwrap();
        let r = apply(
            &mut p,
            Command::TriggerClip {
                deck: A,
                layer: l,
                column: 0,
            },
        );
        assert!(matches!(r, Err(Error::NotPlayable)));
    }

    #[test]
    fn generator_commands_undo_redo_and_coalesce() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t0 = Instant::now();
        let at = |c, o: u64| (c, t0 + Duration::from_millis(o));
        let set = Command::SetGenerator {
            deck: A,
            layer: l,
            column: 0,
            kind: "solid_color".into(),
            name: "Solid Color".into(),
            params: vec![Param::number("level", 0.5, Some((0.0, 1.0)))],
        };
        let (c, t) = at(set, 0);
        h.exec(&mut p, c, t).unwrap();
        let get = |p: &Project| match &clip_in(p, A, l, 0).unwrap().source {
            Source::Generator { params, .. } => params[0].value.clone(),
            _ => panic!("not a generator"),
        };
        assert_eq!(get(&p), Value::Number(0.5));
        let ev = apply(
            &mut p,
            Command::TriggerClip {
                deck: A,
                layer: l,
                column: 0,
            },
        )
        .unwrap()
        .events;
        assert_eq!(
            ev,
            vec![Event::GeneratorStarted {
                deck: A,
                layer: l,
                column: 0
            }]
        );
        let param = |v| Command::SetGeneratorParam {
            deck: A,
            layer: l,
            column: 0,
            param: 0,
            value: v,
        };
        for (i, v) in [0.6, 0.7, 9.0].into_iter().enumerate() {
            let (c, t) = at(param(Value::Number(v)), 100 * (i as u64 + 1));
            h.exec(&mut p, c, t).unwrap();
        }
        assert_eq!(get(&p), Value::Number(1.0));
        assert_eq!(
            apply(&mut p, param(Value::Bool(true))),
            Err(Error::InvalidValue)
        );
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(get(&p), Value::Number(0.5));
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(get(&p), Value::Number(1.0));
        h.undo(&mut p).unwrap().unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert!(clip_in(&p, A, l, 0).is_none());
        h.redo(&mut p).unwrap().unwrap();
        assert!(clip_in(&p, A, l, 0).is_some());
    }

    /// Two layers, three columns: layer 1 has clips in columns 0 and 2 (2 is a placeholder
    /// in layer 2), layer 2 has a clip in column 0 and a placeholder in column 2.
    fn grid() -> (Project, LayerId, LayerId) {
        let (mut p, l1) = setup();
        let l2 = layer(2);
        let l2id = l2.id;
        p.composition.layers.push(l2);
        for n in 2..=3 {
            p.composition.columns.push(Column {
                id: Uuid::new_v4(),
                name: n.to_string(),
            });
        }
        let d = &mut p.composition.decks[0];
        d.set_clip(l1, 0, clip_from_path("/x/a.mov".into()));
        d.set_clip(l1, 2, clip_from_path("/x/c.mov".into()));
        d.set_clip(l2id, 0, clip_from_path("/x/d.mov".into()));
        let mut ph = clip_from_path(PathBuf::new());
        ph.source = Source::Generator {
            kind: "nope".into(),
            params: vec![],
        };
        d.set_clip(l2id, 2, ph);
        (p, l1, l2id)
    }

    #[test]
    fn trigger_column_plays_clears_and_skips() {
        let (mut p, l1, l2) = grid();
        let ev = apply(&mut p, Command::TriggerColumn { deck: A, column: 0 })
            .unwrap()
            .events;
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[0], Event::ClipStarted { layer, .. } if *layer == l1));
        assert!(matches!(&ev[1], Event::ClipStarted { layer, .. } if *layer == l2));
        // Column 1 is empty: every layer is cleared.
        let ev = apply(&mut p, Command::TriggerColumn { deck: A, column: 1 })
            .unwrap()
            .events;
        assert_eq!(
            ev,
            vec![
                Event::LayerCleared { layer: l1 },
                Event::LayerCleared { layer: l2 }
            ]
        );
        // Column 2: placeholder skipped, the other layer still plays.
        let a = apply(&mut p, Command::TriggerColumn { deck: A, column: 2 }).unwrap();
        assert!(a.inverse.is_none());
        assert!(matches!(&a.events[0], Event::ClipStarted { .. }));
        assert_eq!(
            a.events[1],
            Event::Skipped {
                layer: l2,
                column: 2
            }
        );
        assert_eq!(
            apply(&mut p, Command::TriggerColumn { deck: A, column: 9 }),
            Err(Error::NoColumn)
        );
    }

    #[test]
    fn step_column_and_clip_respect_limits() {
        let (mut p, l1, _) = grid();
        let col_of = |p: &mut Project, from, delta| {
            let ev = apply(
                p,
                Command::StepColumn {
                    deck: A,
                    from,
                    delta,
                },
            )
            .unwrap()
            .events;
            // Column 1 clears; others start something.
            match &ev[0] {
                Event::ClipStarted { column, .. } | Event::Skipped { column, .. } => *column,
                _ => 1,
            }
        };
        assert_eq!(col_of(&mut p, None, 1), 0);
        assert_eq!(col_of(&mut p, Some(0), -1), 0);
        assert_eq!(col_of(&mut p, Some(2), 1), 2);
        let clip = |p: &mut Project, from, delta| {
            apply(
                p,
                Command::StepClip {
                    deck: A,
                    layer: l1,
                    from,
                    delta,
                },
            )
        };
        let col = |a: Applied| match &a.events[0] {
            Event::ClipStarted { column, .. } => *column,
            _ => unreachable!(),
        };
        assert_eq!(col(clip(&mut p, Some(0), 1).unwrap()), 2); // skips empty column 1
        assert_eq!(col(clip(&mut p, Some(2), -1).unwrap()), 0);
        assert_eq!(clip(&mut p, Some(2), 1), Err(Error::NoClip));
        assert_eq!(clip(&mut p, Some(0), -1), Err(Error::NoClip));
        assert_eq!(col(clip(&mut p, None, 1).unwrap()), 0);
    }

    #[test]
    fn set_animation_undo_redo() {
        let (mut p, l, _) = grid();
        let mut h = History::new();
        let t = Instant::now();
        let target = AnimTarget::LayerOpacity(l);
        let a = vjc_model::Animation::Dashboard { dial: 2 };
        let get = |p: &Project| p.composition.layers[0].track.opacity.animation.clone();
        h.exec(
            &mut p,
            Command::SetAnimation {
                target,
                animation: Some(a.clone()),
            },
            t,
        )
        .unwrap();
        assert_eq!(get(&p), Some(a.clone()));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(get(&p), None);
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(get(&p), Some(a));
        let bad = Command::SetAnimation {
            target: AnimTarget::Effect {
                target: EffectTarget::Master,
                effect: Uuid::new_v4(),
                param: 0,
            },
            animation: None,
        };
        assert_eq!(apply(&mut p, bad).unwrap_err(), Error::NoEffect);
    }

    #[test]
    fn set_animation_drag_is_one_undo_step() {
        let (mut p, l, _) = grid();
        let mut h = History::new();
        let t = Instant::now();
        let target = AnimTarget::LayerOpacity(l);
        for (i, d) in [1u32, 2, 3].into_iter().enumerate() {
            h.exec(
                &mut p,
                Command::SetAnimation {
                    target,
                    animation: Some(vjc_model::Animation::Dashboard { dial: d }),
                },
                t + Duration::from_millis(16 * i as u64),
            )
            .unwrap();
        }
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.layers[0].track.opacity.animation, None);
    }

    #[test]
    fn dashboard_dial_by_address_undoes() {
        let (mut p, _, _) = grid();
        let cmd = shortcuts::resolve(&p, "/comp/dashboard/3", 0.75).unwrap();
        assert_eq!(
            cmd,
            Command::SetDashboardDial {
                dial: 2,
                value: 0.75
            }
        );
        let mut h = History::new();
        h.exec(&mut p, cmd, Instant::now()).unwrap();
        assert_eq!(p.composition.dashboard.len(), anim::DIALS);
        let ctx = shortcuts::Ctx::new(None, false);
        assert_eq!(
            shortcuts::current(&p, &ctx, "/comp/dashboard/3"),
            Some(0.75)
        );
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(shortcuts::current(&p, &ctx, "/comp/dashboard/3"), Some(0.0));
        assert!(shortcuts::resolve(&p, "/comp/dashboard/9", 1.0).is_none());
        assert!(shortcuts::resolve(&p, "/comp/dashboard/0", 1.0).is_none());
    }

    #[test]
    fn crossfader_address_resolves() {
        let (p, _, _) = grid();
        let ctx = shortcuts::Ctx::new(None, false);
        assert_eq!(shortcuts::current(&p, &ctx, "/comp/crossfader"), Some(0.5));
        assert_eq!(
            shortcuts::resolve(&p, "/comp/crossfader", 2.0),
            Some(Command::SetCrossfader { value: 1.0 })
        );
        let mut q = p.clone();
        let t = AnimTarget::Crossfader;
        apply(
            &mut q,
            Command::SetAnimation {
                target: t,
                animation: None,
            },
        )
        .unwrap();
        assert!(q.composition.crossfader.is_some());
    }

    #[test]
    fn recording_commands_are_runtime() {
        let (mut p, _, _) = grid();
        let mut h = History::new();
        let ev = h
            .exec(&mut p, Command::StartRecording, Instant::now())
            .unwrap();
        assert_eq!(ev, vec![Event::RecordingStarted]);
        let ev = h
            .exec(&mut p, Command::StopRecording, Instant::now())
            .unwrap();
        assert_eq!(ev, vec![Event::RecordingStopped]);
        assert!(!h.can_undo());
    }

    #[test]
    fn clear_all_and_bypass_are_runtime() {
        let (mut p, _, _) = grid();
        let mut h = History::new();
        let ev = h.exec(&mut p, Command::ClearAll, Instant::now()).unwrap();
        assert_eq!(ev.len(), 2);
        let ev = h
            .exec(
                &mut p,
                Command::SetMasterBypass { bypass: true },
                Instant::now(),
            )
            .unwrap();
        assert_eq!(ev, vec![Event::MasterBypass { bypass: true }]);
        assert!(!h.can_undo());
    }

    #[test]
    fn crossfader_commands_undo_and_coalesce() {
        use crate::crossfader::CrossfadeCurve;
        let (mut p, l, _) = grid();
        let mut h = History::new();
        let t = Instant::now();
        h.exec(
            &mut p,
            Command::SetLayerBus {
                layer: l,
                bus: Some(vjc_model::Bus::B),
            },
            t,
        )
        .unwrap();
        for v in [0.1, 0.7, 0.9] {
            h.exec(&mut p, Command::SetCrossfader { value: v }, t)
                .unwrap();
        }
        h.exec(
            &mut p,
            Command::SetCrossfaderCurve {
                curve: CrossfadeCurve::Cut,
            },
            t,
        )
        .unwrap();
        assert!(
            apply(&mut p, Command::SetCrossfader { value: 1.5 }).is_err(),
            "out of range"
        );
        let cf = |p: &Project| p.composition.crossfader.clone().unwrap();
        assert_eq!(number(&cf(&p).position.value), 0.9);
        assert_eq!(cf(&p).curve, "Cut");
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(cf(&p).curve, "ConstantPower");
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(number(&cf(&p).position.value), 0.5, "3 moves = 1 undo");
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.layers[0].crossfader_bus, None);
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(
            p.composition.layers[0].crossfader_bus,
            Some(vjc_model::Bus::B)
        );
    }

    #[test]
    fn master_opacity_and_speed_undo_and_coalesce() {
        let (mut p, _, _) = grid();
        let mut h = History::new();
        let t = Instant::now();
        for v in [0.9, 0.5, 0.2] {
            h.exec(&mut p, Command::SetMasterOpacity { value: v }, t)
                .unwrap();
        }
        for v in [2.0, 3.0] {
            h.exec(&mut p, Command::SetMasterSpeed { value: v }, t)
                .unwrap();
        }
        assert_eq!(p.composition.master_speed, 3.0);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.master_speed, 1.0);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.master.opacity.value), 1.0);
        assert!(!h.can_undo());
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.master.opacity.value), 0.2);
        for bad in [-0.1, 1.1, f64::NAN] {
            assert!(apply(&mut p, Command::SetMasterOpacity { value: bad }).is_err());
        }
        assert!(apply(&mut p, Command::SetMasterSpeed { value: 4.5 }).is_err());
    }

    #[test]
    fn master_speed_scales_playback_rate() {
        assert_eq!(playback_rate(1.0, 1.0, 1.0), 1.0);
        assert_eq!(playback_rate(2.0, 0.5, 2.0), 2.0);
        assert_eq!(playback_rate(1.5, 1.0, 0.0), 0.0);
    }

    use crate::shortcuts::{self as sc, Ctx};

    fn ctx(sel: Option<u32>, bypass: bool) -> Ctx {
        Ctx::new(sel, bypass)
    }

    #[test]
    fn resolve_every_address() {
        let (p, l1, l2) = grid();
        // Layers count from the bottom: layer 1 is the last listed (l2).
        assert!(matches!(
            sc::resolve(&p, "/comp/layer/1/clip/3/connect", 1.0),
            Some(Command::TriggerClip { deck: A, layer, column: 2 }) if layer == l2
        ));
        assert!(matches!(
            sc::resolve(&p, "/comp/layer/2/clip/1/connect", 1.0),
            Some(Command::TriggerClip { layer, column: 0, .. }) if layer == l1
        ));
        assert_eq!(sc::resolve(&p, "/comp/layer/3/clip/1/connect", 1.0), None);
        assert_eq!(sc::resolve(&p, "/comp/layer/1/clip/0/connect", 1.0), None);
        assert_eq!(sc::resolve(&p, "/comp/layer/1/clip/1/connect", 0.0), None);
        assert_eq!(
            sc::resolve(&p, "/comp/column/2/connect", 1.0),
            Some(Command::TriggerColumn { deck: A, column: 1 })
        );
        assert_eq!(
            sc::resolve(&p, "/comp/layer/2/opacity", 3.0),
            Some(Command::SetOpacity {
                layer: l1,
                value: 1.0
            })
        );
        assert_eq!(
            sc::resolve(&p, "/comp/layer/1/clear", 1.0),
            Some(Command::ClearLayer { layer: l2 })
        );
        assert!(matches!(
            sc::resolve(&p, "/comp/tempo/tap", 1.0),
            Some(Command::Tap { .. })
        ));
        assert_eq!(
            sc::resolve(&p, "/comp/tempo/bpm", 9999.0),
            Some(Command::SetBpm {
                bpm: clock::MAX_BPM
            })
        );
        assert_eq!(
            sc::resolve(&p, "/comp/master/bypass", 1.0),
            Some(Command::SetMasterBypass { bypass: true })
        );
        assert_eq!(
            sc::resolve(&p, "/comp/master/opacity", 0.5),
            Some(Command::SetMasterOpacity { value: 0.5 })
        );
        assert_eq!(sc::resolve(&p, "/comp/clear", 1.0), Some(Command::ClearAll));
        assert_eq!(sc::resolve(&p, "/comp/nope", 1.0), None);
        let c = ctx(Some(1), false);
        assert_eq!(
            sc::resolve_with(&p, &c, "/comp/column/next", 1.0),
            Some(Command::StepColumn {
                deck: A,
                from: Some(1),
                delta: 1
            })
        );
        assert_eq!(
            sc::resolve_with(&p, &c, "/comp/column/selected/connect", 1.0),
            Some(Command::TriggerColumn { deck: A, column: 1 })
        );
    }

    #[test]
    fn activate_honors_modes() {
        let (mut p, _, _) = grid();
        let c = ctx(None, false);
        let mk = |target: &str, mode, range, invert| {
            let mut s = sc::key_shortcut("Q", target, mode, range);
            s.invert = invert;
            s
        };
        let op = "/comp/layer/1/opacity";
        // Absolute: fixed value (range.1, or range.0 inverted).
        assert_eq!(
            sc::activate(&p, &c, &mk(op, ShortcutMode::Absolute, (0.0, 0.25), false)),
            Some(Command::SetOpacity {
                layer: p.composition.layers[1].id,
                value: 0.25
            })
        );
        assert!(matches!(
            sc::activate(&p, &c, &mk(op, ShortcutMode::Absolute, (0.1, 0.25), true)),
            Some(Command::SetOpacity { value, .. }) if value == 0.1
        ));
        // Toggle: opacity starts at 1 -> 0, and bypass flips with the runtime state.
        assert!(matches!(
            sc::activate(&p, &c, &mk(op, ShortcutMode::Toggle, (0.0, 1.0), false)),
            Some(Command::SetOpacity { value, .. }) if value == 0.0
        ));
        let by = mk(
            "/comp/master/bypass",
            ShortcutMode::Toggle,
            (0.0, 1.0),
            false,
        );
        assert_eq!(
            sc::activate(&p, &c, &by),
            Some(Command::SetMasterBypass { bypass: true })
        );
        assert_eq!(
            sc::activate(&p, &ctx(None, true), &by),
            Some(Command::SetMasterBypass { bypass: false })
        );
        // Relative: increments the current value (clamped), negative when inverted.
        p.composition.tempo.bpm = 100.0;
        let bpm = |inv| mk("/comp/tempo/bpm", ShortcutMode::Relative, (0.0, 5.0), inv);
        assert_eq!(
            sc::activate(&p, &c, &bpm(false)),
            Some(Command::SetBpm { bpm: 105.0 })
        );
        assert_eq!(
            sc::activate(&p, &c, &bpm(true)),
            Some(Command::SetBpm { bpm: 95.0 })
        );
        // Trigger fires the address.
        let t = mk(
            "/comp/column/1/connect",
            ShortcutMode::Trigger,
            (0.0, 1.0),
            false,
        );
        assert_eq!(
            sc::activate(&p, &c, &t),
            Some(Command::TriggerColumn { deck: A, column: 0 })
        );
    }

    #[test]
    fn learn_creates_replaces_removes_and_undoes() {
        let (mut p, _, _) = grid();
        let mut h = History::new();
        let now = Instant::now();
        let run = |p: &mut Project, h: &mut History, cmds: Vec<Command>| {
            for c in cmds {
                h.exec(p, c, now).unwrap();
            }
        };
        let t1 = "/comp/column/1/connect";
        let t2 = "/comp/column/2/connect";
        let cmds = sc::learn(&p, t1, "A");
        run(&mut p, &mut h, cmds);
        assert_eq!(sc::key_for(&p, t1), Some("A"));
        // Same target, new key: replaces.
        let cmds = sc::learn(&p, t1, "B");
        run(&mut p, &mut h, cmds);
        assert_eq!(p.shortcuts.len(), 1);
        assert_eq!(sc::key_for(&p, t1), Some("B"));
        // Same key on another target: the key moves.
        let cmds = sc::learn(&p, t2, "B");
        run(&mut p, &mut h, cmds);
        assert_eq!(p.shortcuts.len(), 1);
        assert_eq!(sc::key_for(&p, t1), None);
        assert_eq!(sc::key_for(&p, t2), Some("B"));
        // Delete removes; unmapped target is a no-op.
        assert!(sc::unlearn(&p, t1).is_empty());
        let cmds = sc::unlearn(&p, t2);
        run(&mut p, &mut h, cmds);
        assert!(p.shortcuts.is_empty());
        // Undo brings it back at the same place.
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(sc::key_for(&p, t2), Some("B"));
        h.redo(&mut p).unwrap().unwrap();
        assert!(p.shortcuts.is_empty());
        assert_eq!(
            apply(&mut p, Command::RemoveShortcut { id: Uuid::new_v4() }),
            Err(Error::NoShortcut)
        );
    }

    #[test]
    fn default_preset_resolves() {
        let (p, _, _) = grid();
        let d = sc::default_shortcuts();
        let keys: Vec<_> = d.iter().filter_map(|s| sc::key_of(s)).collect();
        assert_eq!(keys, ["ArrowLeft", "ArrowRight", "Space"]);
        let c = ctx(Some(1), false);
        for s in &d {
            assert!(sc::activate(&p, &c, s).is_some(), "{}", s.target);
        }
    }

    #[test]
    fn step_clip_skips_placeholder() {
        // Layer 2 (l2) has clips at 0 and a placeholder at 2: stepping from 0 finds nothing.
        let (mut p, l1, l2) = grid();
        let step = |p: &mut Project, layer, from| {
            apply(
                p,
                Command::StepClip {
                    deck: A,
                    layer,
                    from,
                    delta: 1,
                },
            )
        };
        assert_eq!(step(&mut p, l2, Some(0)), Err(Error::NoClip));
        // Backwards from the end also passes over the placeholder.
        let a = apply(
            &mut p,
            Command::StepClip {
                deck: A,
                layer: l2,
                from: None,
                delta: -1,
            },
        )
        .unwrap();
        assert!(matches!(&a.events[0], Event::ClipStarted { column: 0, .. }));
        // A playable clip after the gap is still reached.
        let a = step(&mut p, l1, Some(0)).unwrap();
        assert!(matches!(&a.events[0], Event::ClipStarted { column: 2, .. }));
    }

    #[test]
    fn screens_and_slices_undo_redo() {
        let (mut p, _) = setup();
        let scr = mapping::new_screen("S1", 1920, 1080);
        let sid = scr.id;
        roundtrip(
            &mut p,
            Command::AddScreen {
                screen: scr,
                index: 0,
            },
        );
        let mut h = History::new();
        let now = Instant::now();
        let sc = mapping::new_screen("S1", 1920, 1080);
        let sid = if p.output.screens.is_empty() {
            h.exec(
                &mut p,
                Command::AddScreen {
                    screen: sc.clone(),
                    index: 0,
                },
                now,
            )
            .unwrap();
            sc.id
        } else {
            sid
        };
        let sl = mapping::new_slice(1920, 1080);
        let lid = sl.id;
        for c in [
            Command::AddSlice {
                screen: sid,
                slice: sl,
                index: 0,
            },
            Command::SetScreenSize {
                screen: sid,
                width: 1280,
                height: 720,
            },
            Command::SetScreenDevice {
                screen: sid,
                device: vjc_model::OutputDevice::Monitor { name: "3".into() },
            },
            Command::SetSliceInput {
                screen: sid,
                slice: lid,
                input: vec![(0.1, 0.1), (0.5, 0.1), (0.5, 0.5), (0.1, 0.5)],
            },
            Command::SetSliceMeshSize {
                screen: sid,
                slice: lid,
                cols: 4,
                rows: 3,
            },
        ] {
            roundtrip(&mut p, c.clone());
            h.exec(&mut p, c, now).unwrap();
        }
        let sl = &p.output.screens[0].slices[0];
        assert_eq!((sl.mesh_cols, sl.output_mesh.len()), (4, 12));
        // Bad size and unknown ids are rejected.
        let bad = Command::SetSliceMeshSize {
            screen: sid,
            slice: lid,
            cols: 10,
            rows: 2,
        };
        assert!(h.exec(&mut p, bad, now).is_err());
        let gone = Command::RemoveScreen {
            screen: Uuid::new_v4(),
        };
        assert!(h.exec(&mut p, gone, now).is_err());
        // Dragging a point coalesces into one undo step.
        let mut mesh = p.output.screens[0].slices[0].output_mesh.clone();
        let before = p.clone();
        let mut h2 = History::new();
        for k in 1..=5 {
            mesh[0].0 = k as f32;
            let c = Command::SetSliceMesh {
                screen: sid,
                slice: lid,
                mesh: mesh.clone(),
                cols: 4,
            };
            h2.exec(&mut p, c, now).unwrap();
        }
        assert_eq!(p.output.screens[0].slices[0].output_mesh[0].0, 5.0);
        h2.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        roundtrip(
            &mut p,
            Command::RemoveSlice {
                screen: sid,
                slice: lid,
            },
        );
        roundtrip(&mut p, Command::RemoveScreen { screen: sid });
    }

    fn one_screen_slice() -> (Project, Uuid, Uuid) {
        let (mut p, _) = setup();
        let mut scr = mapping::new_screen("S", 1000, 500);
        let sl = mapping::new_slice(1000, 500);
        let (sid, lid) = (scr.id, sl.id);
        scr.slices.push(sl);
        p.output.screens.push(scr);
        (p, sid, lid)
    }

    #[test]
    fn invalid_mesh_does_not_panic() {
        let (mut p, sid, lid) = one_screen_slice();
        for (cols, len) in [(0u32, 4usize), (1, 4), (3, 7)] {
            let sl = &mut p.output.screens[0].slices[0];
            sl.mesh_cols = cols;
            sl.output_mesh = vec![(0.0, 0.0); len];
            let c = Command::SetSliceMeshSize {
                screen: sid,
                slice: lid,
                cols: 3,
                rows: 3,
            };
            assert!(History::new().exec(&mut p, c, Instant::now()).is_err());
        }
        let mut out = p.output.clone();
        mapping::normalize_output(&mut out);
        let sl = &out.screens[0].slices[0];
        assert_eq!((sl.mesh_cols, sl.output_mesh.len()), (2, 4));
        assert_eq!(sl.output_mesh[3], (1000.0, 500.0));
    }

    #[test]
    fn slice_input_coalesces() {
        let (mut p, sid, lid) = one_screen_slice();
        let before = p.clone();
        let mut h = History::new();
        let now = Instant::now();
        for k in 1..=5 {
            let x = k as f32 / 10.0;
            let c = Command::SetSliceInput {
                screen: sid,
                slice: lid,
                input: vec![(x, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            };
            h.exec(&mut p, c, now).unwrap();
        }
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn screen_size_rescales_meshes() {
        let (mut p, sid, _) = one_screen_slice();
        let before = p.clone();
        let c = Command::SetScreenSize {
            screen: sid,
            width: 500,
            height: 1000,
        };
        let mut h = History::new();
        h.exec(&mut p, c, Instant::now()).unwrap();
        let m = &p.output.screens[0].slices[0].output_mesh;
        assert_eq!(m[3], (500.0, 1000.0));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
    }
}
