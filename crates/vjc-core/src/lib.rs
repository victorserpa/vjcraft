//! Command bus: every state change is a `Command` applied to a `Project` by the pure
//! function `apply`. `History` keeps inverses for undo/redo. No UI, no players: the app
//! reacts to the returned `Event`s.

pub mod anim;
pub mod blend;
pub mod clock;
pub mod columns;
pub mod comp_autopilot;
pub mod crossfader;
pub mod dmx;
pub mod effects;
pub mod groups;
pub mod ltc;
pub mod mapping;
pub mod midi;
pub mod osc;
pub mod playback;
pub mod playhead;
pub mod rehearsal;
pub mod shortcuts;
pub mod slice_geom;
pub mod timecode;
pub mod transform;
pub mod transition;
pub mod transition_fx;
#[cfg(test)]
mod wave_j_tests;
#[cfg(test)]
mod wave_m_tests;
#[cfg(test)]
mod wave_o_tests;

pub use effects::EffectTarget;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;
use vjc_model::{
    BlendMode, CellClip, Clip, Column, ColumnIdx, Deck, DeckId, Direction, Effect, EffectId,
    FitMode, GroupId, Layer, LayerId, Param, PlayMode, Project, Source, Timecode, TimecodeCue,
    Track, TransformField, Transport, Value,
};

/// Continuous commands on the same target within this window merge into one undo step.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(500);

/// What a palette colour is set on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorTarget {
    Layer(LayerId),
    Column(uuid::Uuid),
    Clip {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
}

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
    /// Moves the clip of cell `from` to cell `to` of the same deck. An occupied target
    /// swaps the two clips. With `copy` the source stays and a copy (fresh ids) replaces
    /// whatever is in the target. A playing source is followed by the app (`ClipsMoved`).
    MoveClip {
        deck: DeckId,
        from: (LayerId, ColumnIdx),
        to: (LayerId, ColumnIdx),
        copy: bool,
    },
    /// Plays the cell's clip on its layer. Runtime only: not undoable.
    TriggerClip {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
    /// Plays the clip of cell (`layer`, `column`) on layer `onto` (clip target Active or
    /// Free). The event keeps `onto` as the layer; the app remembers where the clip
    /// lives. Runtime only: not undoable.
    TriggerClipOn {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        onto: LayerId,
    },
    /// Stops the layer. Runtime only: not undoable.
    ClearLayer {
        layer: LayerId,
    },
    /// Empties every cell of the layer in the deck; the layer stays. Refused on a locked layer.
    ClearLayerClips {
        deck: DeckId,
        layer: LayerId,
    },
    SetLayerLock {
        layer: LayerId,
        locked: bool,
    },
    SetLayerFaderStart {
        layer: LayerId,
        enabled: bool,
    },
    SetLayerMaskMode {
        layer: LayerId,
        mode: vjc_model::MaskMode,
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
    /// Points several file clips at new media paths in one step (relink, collect). Cells
    /// that hold no file clip fail the whole command, so the project never half-changes.
    SetMediaPaths {
        changes: Vec<(DeckId, LayerId, ColumnIdx, PathBuf)>,
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
    /// Seeks the playing clip to a normalised file position 0..=1 (scrub bar). Runtime only.
    SeekClip {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        pos: f64,
    },
    /// Sets the played region of the clip, normalised file positions with
    /// `0 <= in_point` and `in_point + playback::MIN_REGION <= out_point <= 1`. Coalesces:
    /// dragging a marker is one undo step.
    SetInOut {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        in_point: f64,
        out_point: f64,
    },
    /// How the clip is driven: free (both None), beat-synced (`beat_sync`) or following the
    /// external timecode (`timecode_start`). Sets both at once so a mode switch is one undo
    /// step; both set at once is `InvalidValue`.
    SetClipSync {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        beat_sync: Option<f64>,
        timecode_start: Option<Timecode>,
    },
    /// Plays the in..out region in `secs` seconds (speed derived from it); None = free
    /// speed. Beat sync wins when both are set. Undoable.
    SetClipDuration {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        secs: Option<f64>,
    },
    /// Layer master level and sizing (auto-size, width, height). Undoable.
    SetLayerExtras {
        layer: LayerId,
        extras: vjc_model::LayerExtras,
    },
    /// Decodes the clip's thumbnail again. Runtime only.
    RefreshClipThumb {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
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
    /// Sets one field of a track's transform (px / scale / degrees, clamped to its range).
    /// Coalesces: slider drags are one undo step.
    SetTransform {
        target: EffectTarget,
        field: TransformField,
        value: f64,
    },
    /// Sets several transform fields at once (the preview widget's drag). A drag over the
    /// same field set coalesces into one undo step; all-or-nothing on invalid values.
    SetTransformMulti {
        target: EffectTarget,
        values: Vec<(TransformField, f64)>,
    },
    /// Dashboard dial `dial` (0..8), value 0..=1 (coalesces).
    SetDashboardDial {
        dial: u32,
        value: f64,
    },
    /// Dial `dial` (0..8) of a layer's own dashboard, value 0..=1 (coalesces).
    SetLayerDial {
        layer: LayerId,
        dial: u32,
        value: f64,
    },
    /// Dial `dial` (0..8) of a clip's own dashboard, value 0..=1 (coalesces).
    SetClipDial {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        dial: u32,
        value: f64,
    },
    /// Renames a dial and sets its range (undoable, coalesces). The dial keeps its position
    /// in 0..=1 whatever the range.
    SetDialInfo {
        owner: DialRef,
        dial: u32,
        name: String,
        range: (f64, f64),
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
    /// Crossfader behaviour: fade along the curve or cut at the middle. Undoable.
    SetCrossfaderBehaviour {
        behaviour: vjc_model::CrossfadeBehaviour,
    },
    /// Moves the crossfader to bus A (0) or B (1). `pressed = false` is the release of the
    /// button: with the Cut & return behaviour it goes back to the opposite bus, otherwise
    /// it does nothing. Undoable.
    GoCrossfader {
        bus: vjc_model::Bus,
        pressed: bool,
    },
    /// Assigns a whole group to crossfader bus A or B (None = unaffected). Undoable.
    SetGroupBus {
        group: GroupId,
        bus: Option<vjc_model::Bus>,
    },
    /// Blend mode of the layers on a crossfader bus (`None` = their own). Undoable.
    SetCrossfaderBlend {
        blend: Option<BlendMode>,
    },
    /// Global transport of the tempo clock. Stop also ejects every clip (`LayerCleared` for
    /// each layer that is not locked) and parks the clock on beat 0, which rewinds the
    /// animations that follow it. Not undoable: it is a performance action.
    SetTransport {
        state: vjc_model::PlayState,
    },
    /// Global reverse: every clip runs against its own direction. Undoable.
    SetMasterReverse {
        reverse: bool,
    },
    /// Metronome click on/off (saved with the project). Undoable.
    SetMetronome {
        on: bool,
    },
    /// Layer audio / video enable toggles. Undoable.
    SetLayerAudio {
        layer: LayerId,
        enabled: bool,
    },
    SetLayerVideo {
        layer: LayerId,
        enabled: bool,
    },
    /// R/G/B/A channel toggles of a clip's picture. Undoable.
    SetClipChannels {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        channels: vjc_model::Channels,
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
    /// Pixel-perfect output of a screen (nearest sampling of unscaled slices, 1:1 window).
    /// Undoable.
    SetScreenPixelPerfect {
        screen: Uuid,
        pixel_perfect: bool,
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
    /// Replaces the whole output setup (screens, slices, masks, soft edge, routing), e.g.
    /// when a saved output preset is loaded. Undoable.
    SetOutputSetup {
        screens: Vec<vjc_model::Screen>,
    },
    /// Source of a slice: composition, one layer, one group or the preview. Undoable.
    SetSliceSource {
        screen: Uuid,
        slice: Uuid,
        source: vjc_model::SliceSource,
    },
    /// Several commands as one undo step (e.g. an edit of every selected slice). All or
    /// nothing: if one is rejected the project is left untouched. An empty batch is invalid.
    Batch {
        commands: Vec<Command>,
    },
    /// Moves a slice to another screen, inserted at `index` (clamped). The mesh keeps its
    /// pixel positions. Undoable.
    MoveSlice {
        from: Uuid,
        to: Uuid,
        slice: Uuid,
        index: usize,
    },
    /// Mask polygon in normalized screen coordinates (3..=32 points); `None` clears it.
    /// Coalesces.
    SetSliceMask {
        screen: Uuid,
        slice: Uuid,
        mask: Option<Vec<(f32, f32)>>,
    },
    /// Invert the slice mask (show only outside it). Coalesces.
    SetSliceMaskInvert {
        screen: Uuid,
        slice: Uuid,
        invert: bool,
    },
    /// Is Key and Black BG of a slice.
    SetSliceKey {
        screen: Uuid,
        slice: Uuid,
        is_key: bool,
        black_bg: bool,
    },
    /// Screen output delay in ms (clamped to 0..=100). Coalesces.
    /// Additive blend of overlapping slices, alpha 0 outside them (soft-edge blending).
    SetScreenAdditive {
        screen: Uuid,
        on: bool,
    },
    SetScreenDelay {
        screen: Uuid,
        delay_ms: u32,
    },
    /// Soft edge (widths clamped to 0..=0.5, gamma to 0.1..=5); `None` clears it. Coalesces.
    SetSliceSoftEdge {
        screen: Uuid,
        slice: Uuid,
        soft_edge: Option<vjc_model::SoftEdge>,
    },
    /// Brightness and RGB gains (each clamped to 0..=2). Coalesces.
    SetSliceColor {
        screen: Uuid,
        slice: Uuid,
        color: vjc_model::SliceColor,
    },
    /// Screen on/off. Off keeps sending, in black. Coalesces.
    SetScreenEnabled {
        screen: Uuid,
        enabled: bool,
    },
    /// Screen opacity (0..=1), brightness, contrast and RGB (0..=2). Coalesces.
    SetScreenColor {
        screen: Uuid,
        color: vjc_model::ScreenColor,
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
    /// Where the clip starts when triggered (`Layer` = the layer's default). Undoable.
    SetTriggerMode {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        mode: vjc_model::TriggerMode,
    },
    /// Default trigger mode of the layer's clips; `Layer` is stored as Restart. Undoable.
    SetLayerTriggerMode {
        layer: LayerId,
        mode: vjc_model::TriggerMode,
    },
    /// Trigger style of the composition (clips with `Composition` follow it). Undoable.
    /// Composition autopilot (which column plays next, and when). Undoable.
    SetCompAutopilot {
        autopilot: vjc_model::CompAutopilot,
    },
    /// Autopilot override of one column. Undoable.
    SetColumnAutopilot {
        column: uuid::Uuid,
        autopilot: vjc_model::ColumnAutopilot,
    },
    SetCompTriggerStyle {
        style: vjc_model::TriggerStyle,
    },
    /// Beat snap of the composition (None = the app preference). Undoable.
    SetCompBeatSnap {
        snap: Option<vjc_model::BeatSnap>,
    },
    /// Beat snap override of one column (None = the composition's). Undoable.
    SetColumnBeatSnap {
        column: uuid::Uuid,
        snap: Option<vjc_model::BeatSnap>,
    },
    /// Beat snap override of one clip (None = the column's). Undoable.
    SetClipBeatSnap {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        snap: Option<vjc_model::BeatSnap>,
    },
    /// Free-text description of the composition. Undoable.
    SetCompDescription {
        text: String,
    },
    /// Bit depth (8 or 16) of the composition's render textures. Undoable.
    SetCompBitDepth {
        depth: u8,
    },
    /// Palette colour (1..=PALETTE_LEN, None = none) of a layer, column or clip. Undoable.
    SetColor {
        target: ColorTarget,
        color: Option<u8>,
    },
    /// Per-clip extras: colour, ignore-column / fader-start overrides, strip toggles. Undoable.
    SetClipExtras {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        extras: vjc_model::ClipExtras,
    },
    /// Replaces a whole effect chain (paste effects). Undoable.
    ReplaceEffects {
        target: effects::EffectTarget,
        effects: Vec<vjc_model::Effect>,
    },
    /// Blend mode of one effect's mix over its input. Undoable.
    SetEffectBlend {
        target: effects::EffectTarget,
        effect: EffectId,
        blend: vjc_model::BlendMode,
    },
    /// Collapses a layer in the grid. Undoable.
    SetLayerFolded {
        layer: LayerId,
        folded: bool,
    },
    /// Collapses a layer group in the grid. Undoable.
    SetGroupFolded {
        group: GroupId,
        folded: bool,
    },
    /// Clip target of the composition. Undoable.
    SetCompClipTarget {
        target: vjc_model::ClipTarget,
    },
    /// Clip target override of one clip (`Composition` = follow). Undoable.
    SetClipTarget {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        target: vjc_model::ClipTarget,
    },
    /// Column triggers skip this layer. Undoable.
    SetLayerIgnoreColumn {
        layer: LayerId,
        ignore: bool,
    },
    /// Trigger style override of one clip (`Composition` = follow). Undoable.
    SetClipTriggerStyle {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        style: vjc_model::TriggerStyle,
    },
    /// Blend mode a clip uses while it plays (None = the layer's). Undoable.
    SetBlendOverride {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        blend: Option<BlendMode>,
    },
    /// Transition a clip uses when triggered (None = the layer's). Undoable.
    SetClipTransition {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        transition: Option<vjc_model::Transition>,
    },
    /// Groups adjacent, ungrouped layers under `group` (any order; at least one).
    CreateGroup {
        group: vjc_model::LayerGroup,
        layers: Vec<LayerId>,
    },
    /// Inverse of `Ungroup` on a group that has no layers (only a file can hold one):
    /// puts it back at `index` in `Composition::groups`.
    RestoreEmptyGroup {
        group: vjc_model::LayerGroup,
        index: usize,
    },
    /// Dissolves the group; its layers stay where they are.
    Ungroup {
        group: GroupId,
    },
    RenameGroup {
        group: GroupId,
        name: String,
    },
    /// Group opacity 0..=1 (coalesces).
    SetGroupOpacity {
        group: GroupId,
        value: f64,
    },
    SetGroupBlend {
        group: GroupId,
        blend: BlendMode,
    },
    SetGroupBypass {
        group: GroupId,
        bypass: bool,
    },
    SetGroupSolo {
        group: GroupId,
        solo: bool,
    },
    /// Group-level mask mode (the composed group masks the layers below it).
    SetGroupMaskMode {
        group: GroupId,
        mode: vjc_model::MaskMode,
    },
    /// A column trigger skips every layer of the group.
    SetGroupIgnoreColumn {
        group: GroupId,
        ignore: bool,
    },
    /// Locks the content of every layer of the group.
    SetGroupLock {
        group: GroupId,
        locked: bool,
    },
    /// Group speed 0..=4, multiplies the speed of its clips (coalesces).
    SetGroupSpeed {
        group: GroupId,
        value: f64,
    },
    /// Puts an ungrouped layer directly above or below the group into it.
    MoveLayerToGroup {
        layer: LayerId,
        group: GroupId,
    },
    /// Takes the topmost or bottommost layer out of its group (never the last one).
    MoveLayerOutOfGroup {
        layer: LayerId,
    },
    /// Moves an ungrouped layer so it ends at `to_index` (0 = top, clamped). It never lands
    /// inside a group: it goes above the group it hit.
    MoveLayer {
        layer: LayerId,
        to_index: usize,
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
    /// Removes a layer together with its clips in every deck. Undo restores the layer, the
    /// clips (effects included) and their place in each deck.
    RemoveLayerWithClips {
        layer: LayerId,
    },
    /// Inverse of `RemoveLayerWithClips`: the layer at `index` and its clips as
    /// (deck, position in the deck's clip list, clip).
    RestoreLayer {
        layer: Layer,
        index: usize,
        clips: Vec<(DeckId, usize, CellClip)>,
        /// (screen, slice, source) of slices that routed from the layer; put back on undo.
        slices: Vec<(Uuid, Uuid, vjc_model::SliceSource)>,
        /// (position, cue) of timecode cues aimed at the layer.
        cues: Vec<(usize, vjc_model::TimecodeCue)>,
        /// (position, shortcut) of shortcuts aimed at the layer's number.
        shortcuts: Vec<(usize, vjc_model::Shortcut)>,
    },
    AddColumn {
        column: Column,
    },
    RenameColumn {
        column: Uuid,
        name: String,
    },
    /// Removes the last column, which must be empty.
    RemoveColumn {
        column: Uuid,
    },
    /// Moves the columns (kept in their grid order) as a block so that the first lands at
    /// index `to` among the columns left behind (clamped). Clips follow. Undoable.
    MoveColumns {
        columns: Vec<Uuid>,
        to: usize,
    },
    /// Puts the columns in exactly this order (a permutation of all ids). Undoable.
    ReorderColumns {
        order: Vec<Uuid>,
    },
    /// Inserts a blank column at `index` (clamped); later columns and their clips shift.
    InsertColumn {
        index: usize,
        column: vjc_model::Column,
    },
    /// Copies the columns with their clips (new ids) as a block after the last of them.
    DuplicateColumns {
        columns: Vec<Uuid>,
    },
    /// Removes the columns wherever they are, with their clips; fails (`InUse`) while a
    /// timecode cue targets one of them. Undoable.
    RemoveColumns {
        columns: Vec<Uuid>,
    },
    /// Puts removed columns and their clips back at their old positions (the undo of
    /// `RemoveColumns`).
    RestoreColumns {
        items: Vec<columns::ColumnItem>,
    },
    SetBpm {
        bpm: f64,
    },
    /// Turns Ableton Link on/off (undoable like other tempo settings).
    SetLink {
        enabled: bool,
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
    /// Clip follows the external timecode from `start` (None = off).
    SetTimecodeStart {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        start: Option<Timecode>,
    },
    /// Replaces the whole timecode cue list of the composition.
    SetTimecodeCues {
        cues: Vec<TimecodeCue>,
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
    /// Pauses (direction Pause) or resumes (Forward) the transport of each clip. Clips already
    /// in the requested state are untouched; none changed is `InvalidValue`. Undoable.
    SetPaused {
        clips: Vec<(DeckId, LayerId, ColumnIdx)>,
        paused: bool,
    },
    /// Sets each clip's direction at once (the inverse of `SetPaused`).
    RestoreDirections {
        items: Vec<(DeckId, LayerId, ColumnIdx, Direction)>,
    },
    /// Sets a track's volume (0..=1) or pan (-1..=1) on a layer or a clip (coalesces).
    SetAudio {
        target: AudioTarget,
        param: AudioParam,
        value: f64,
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
    /// Dry/wet of the effect, 0..=1 (coalesces: slider drags are one undo step).
    SetEffectDryWet {
        target: EffectTarget,
        effect: EffectId,
        dry_wet: f32,
    },
    /// Sets param number `param` of the effect (coalesces: slider drags are one undo step).
    SetEffectParam {
        target: EffectTarget,
        effect: EffectId,
        param: usize,
        value: Value,
    },
    /// Loads saved values onto the effect: each given param replaces the effect's param of
    /// the same name (kind must match; numbers are clamped, a choice keeps its options).
    /// Others stay. One undo step; the inverse restores every param exactly.
    SetEffectParams {
        target: EffectTarget,
        effect: EffectId,
        params: Vec<vjc_model::Param>,
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
    /// Replaces the shortcut with the same id (inline edit, re-learn).
    UpdateShortcut {
        shortcut: vjc_model::Shortcut,
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
    Transform {
        target: EffectTarget,
        field: TransformField,
    },
}

/// Whose track `SetAudio` edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioTarget {
    /// The composition master.
    Master,
    Layer(LayerId),
    Clip(DeckId, LayerId, ColumnIdx),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioParam {
    Volume,
    Pan,
}

/// Whose dashboard a dial belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialRef {
    Comp,
    Layer(LayerId),
    Clip(DeckId, LayerId, ColumnIdx),
}

/// What a coalescible command targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    DialInfo(DialRef, u32),
    Opacity(LayerId),
    Audio(AudioTarget, AudioParam),
    Speed(DeckId, LayerId, ColumnIdx),
    Bpm,
    TimecodeCues,
    TimecodeStart(DeckId, LayerId, ColumnIdx),
    InOut(DeckId, LayerId, ColumnIdx),
    MasterOpacity,
    MasterSpeed,
    GroupOpacity(GroupId),
    GroupSpeed(GroupId),
    Crossfader,
    EffectParam(EffectId, usize),
    EffectDryWet(EffectId),
    GeneratorParam(DeckId, LayerId, ColumnIdx, usize),
    Dial(u32),
    LayerDial(LayerId, u32),
    ClipDial(DeckId, LayerId, ColumnIdx, u32),
    Transform(EffectTarget, TransformField),
    /// Target and bitmask of the fields set by `SetTransformMulti`.
    TransformMulti(EffectTarget, u16),
    Anim(AnimTarget),
    SliceMesh(Uuid),
    /// Slice, point count (0 = cleared) and dragged point (first point that differs from the
    /// current mask; 0 when there is none or the count changes): create, add point, clear and drags of
    /// different points are separate steps.
    SliceMask(Uuid, usize, usize),
    SliceSoftEdge(Uuid),
    SliceMaskInvert(Uuid),
    SliceKey(Uuid),
    ScreenDelay(Uuid),
    SliceColor(Uuid),
    ScreenEnabled(Uuid),
    ScreenColor(Uuid),
    SliceInput(Uuid, Uuid),
    ScreenSize(Uuid),
    /// Screen and device kind: switching type is its own undo step, editing the IP is not.
    ScreenDevice(Uuid, std::mem::Discriminant<vjc_model::OutputDevice>),
}

impl Command {
    fn target(&self) -> Option<Target> {
        match self {
            Command::SetOpacity { layer, .. } => Some(Target::Opacity(*layer)),
            Command::SetAudio { target, param, .. } => Some(Target::Audio(*target, *param)),
            Command::SetBpm { .. } => Some(Target::Bpm),
            // Dragging a timecode field emits one command per frame.
            Command::SetTimecodeCues { .. } => Some(Target::TimecodeCues),
            Command::SetTimecodeStart {
                deck,
                layer,
                column,
                ..
            } => Some(Target::TimecodeStart(*deck, *layer, *column)),
            Command::SetInOut {
                deck,
                layer,
                column,
                ..
            } => Some(Target::InOut(*deck, *layer, *column)),
            Command::SetScreenSize { screen, .. } => Some(Target::ScreenSize(*screen)),
            // Typing / dragging in the Art-Net setup emits one command per change.
            Command::SetScreenDevice { screen, device } => Some(Target::ScreenDevice(
                *screen,
                std::mem::discriminant(device),
            )),
            Command::SetSliceMesh { slice, .. } => Some(Target::SliceMesh(*slice)),
            Command::SetSliceMask { slice, mask, .. } => Some(Target::SliceMask(
                *slice,
                mask.as_ref().map_or(0, Vec::len),
                0,
            )),
            Command::SetSliceSoftEdge { slice, .. } => Some(Target::SliceSoftEdge(*slice)),
            Command::SetSliceMaskInvert { slice, .. } => Some(Target::SliceMaskInvert(*slice)),
            Command::SetSliceKey { slice, .. } => Some(Target::SliceKey(*slice)),
            Command::SetScreenDelay { screen, .. } => Some(Target::ScreenDelay(*screen)),
            Command::SetSliceColor { slice, .. } => Some(Target::SliceColor(*slice)),
            Command::SetScreenEnabled { screen, .. } => Some(Target::ScreenEnabled(*screen)),
            Command::SetScreenColor { screen, .. } => Some(Target::ScreenColor(*screen)),
            Command::SetSliceInput { screen, slice, .. } => {
                Some(Target::SliceInput(*screen, *slice))
            }
            Command::SetDialInfo { owner, dial, .. } => Some(Target::DialInfo(*owner, *dial)),
            Command::SetDashboardDial { dial, .. } => Some(Target::Dial(*dial)),
            Command::SetLayerDial { layer, dial, .. } => Some(Target::LayerDial(*layer, *dial)),
            Command::SetClipDial {
                deck,
                layer,
                column,
                dial,
                ..
            } => Some(Target::ClipDial(*deck, *layer, *column, *dial)),
            Command::SetTransform { target, field, .. } => Some(Target::Transform(*target, *field)),
            Command::SetTransformMulti { target, values } => Some(Target::TransformMulti(
                *target,
                values.iter().fold(0u16, |m, (f, _)| {
                    m | 1 << TransformField::ALL.iter().position(|a| a == f).unwrap_or(0)
                }),
            )),
            // Dragging an animation field emits one command per frame.
            Command::SetAnimation { target, .. } => Some(Target::Anim(*target)),
            Command::SetMasterOpacity { .. } => Some(Target::MasterOpacity),
            Command::SetMasterSpeed { .. } => Some(Target::MasterSpeed),
            Command::SetGroupOpacity { group, .. } => Some(Target::GroupOpacity(*group)),
            Command::SetGroupSpeed { group, .. } => Some(Target::GroupSpeed(*group)),
            Command::SetCrossfader { .. } => Some(Target::Crossfader),
            Command::SetEffectDryWet { effect, .. } => Some(Target::EffectDryWet(*effect)),
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
    /// A network clip (Syphon/NDI) started on the layer; `name` is the server/source label.
    NetworkStarted {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
        protocol: vjc_model::NetProtocol,
        name: String,
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
    /// The clip's thumbnail must be decoded again.
    ThumbRefresh {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
    /// The layer's fader went from 0 up and it has fader start: restart its playing clip.
    FaderStart {
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
    /// Clips changed cells (a move or a swap): playback of a moved clip follows it. All
    /// moves of one event happen at once.
    ClipsMoved {
        deck: DeckId,
        moves: Vec<((LayerId, ColumnIdx), (LayerId, ColumnIdx))>,
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
    NoGroup,
    /// Group rule broken: layers not adjacent, already grouped, or group would be empty.
    BadGroup,
    NoShortcut,
    NoScreen,
    NoSlice,
    /// Clip has no file source to play.
    NotPlayable,
    /// Layer/column still holds clips, or is not the last column.
    InUse,
    /// Value outside the accepted range (e.g. BPM not finite or not positive).
    InvalidValue,
    /// Another screen already outputs to that monitor.
    MonitorInUse,
    /// The layer's content is locked.
    Locked,
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
            Error::NoGroup => "group not found",
            Error::BadGroup => "group layers must be adjacent, ungrouped, and a group never empty",
            Error::NoShortcut => "shortcut not found",
            Error::NotPlayable => "clip has no playable file source",
            Error::InUse => "layer or column is not empty",
            Error::InvalidValue => "value out of range",
            Error::Locked => "layer content is locked",
            Error::MonitorInUse => "that monitor already shows another screen",
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
        } => effects::fx_mut(p, target)?
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
        AnimTarget::Transform { target, field } => {
            Ok(effects::track_mut(p, target)?.transform.field_mut(field))
        }
    }
}

fn set_crossfader(p: &mut Project, value: f64) -> Result<Applied, Error> {
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

/// Where a freshly triggered clip must start, or None to start at the beginning as usual
/// (Restart). Resolves the clip's trigger mode against its layer's default.
/// `own_last`: file position the clip stopped at; `prev_local`: fraction of its region the
/// layer's previous clip had reached.
pub fn trigger_start(
    p: &Project,
    deck: DeckId,
    layer: LayerId,
    column: ColumnIdx,
    own_last: Option<f64>,
    prev_local: Option<f64>,
) -> Option<f64> {
    let k = clip_in(p, deck, layer, column)?;
    let default = p
        .composition
        .layers
        .iter()
        .find(|l| l.id == layer)
        .map_or(vjc_model::TriggerMode::Restart, |l| l.trigger_mode);
    let mode = k.transport.trigger_mode.resolve(default);
    (mode != vjc_model::TriggerMode::Restart).then(|| {
        playback::start_position(mode, playback::region(&k.transport), own_last, prev_local)
    })
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

/// Grows a dashboard to `anim::DIALS` named dials.
fn fill_dials(dials: &mut Vec<Param>) {
    while dials.len() < anim::DIALS {
        let n = dials.len() + 1;
        dials.push(Param::number(&format!("Dial {n}"), 0.0, Some((0.0, 1.0))));
    }
}

/// Sets dial `dial` of a dashboard (growing it to `anim::DIALS` named dials first) to
/// `value`, a position in 0..=1 over the dial's range, and returns the previous position.
fn set_dial(dials: &mut Vec<Param>, dial: u32, value: f64) -> Result<f64, Error> {
    if dial as usize >= anim::DIALS || !value.is_finite() {
        return Err(Error::InvalidValue);
    }
    fill_dials(dials);
    let prm = &mut dials[dial as usize];
    let prev = anim::dial_norm(prm);
    let (lo, hi) = prm.range.unwrap_or((0.0, 1.0));
    prm.set_number(lo + value.clamp(0.0, 1.0) * (hi - lo));
    Ok(prev)
}

/// Renames dial `dial` and sets its range; returns the old name and range.
fn set_dial_info(
    dials: &mut Vec<Param>,
    dial: u32,
    name: &str,
    range: (f64, f64),
) -> Result<(String, (f64, f64)), Error> {
    let name = name.trim();
    if dial as usize >= anim::DIALS
        || name.is_empty()
        || !range.0.is_finite()
        || !range.1.is_finite()
        || range.0 >= range.1
    {
        return Err(Error::InvalidValue);
    }
    fill_dials(dials);
    let prm = &mut dials[dial as usize];
    let norm = anim::dial_norm(prm);
    let old = (
        std::mem::replace(&mut prm.name, name.to_string()),
        prm.range.unwrap_or((0.0, 1.0)),
    );
    prm.range = Some(range);
    prm.default = Value::Number(range.0);
    prm.set_number(range.0 + norm * (range.1 - range.0));
    Ok(old)
}

fn dials_of(p: &mut Project, owner: DialRef) -> Result<&mut Vec<Param>, Error> {
    match owner {
        DialRef::Comp => Ok(&mut p.composition.dashboard),
        DialRef::Layer(layer) => {
            let i = layer_idx(p, layer)?;
            Ok(&mut p.composition.layers[i].track.dials)
        }
        DialRef::Clip(deck, layer, column) => {
            check_cell(p, layer, column)?;
            deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .map(|k| &mut k.clip.track.dials)
                .ok_or(Error::NoClip)
        }
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

/// A clip that plays the numbered images named by `pattern` at `fps`.
pub fn clip_from_sequence(pattern: PathBuf, fps: f32) -> Clip {
    let mut c = clip_from_path(pattern.clone());
    c.name = pattern
        .file_name()
        .map(|n| n.to_string_lossy())
        .and_then(|n| {
            n.split('%')
                .next()
                .map(|h| h.trim_end_matches(['_', '-', '.', ' ']).to_string())
        })
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "sequence".into());
    c.source = Source::Sequence { pattern, fps };
    c
}

pub fn clip_from_path(path: PathBuf) -> Clip {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Clip {
        extras: Default::default(),
        id: Uuid::new_v4(),
        name,
        source: Source::File { path },
        transport: Transport::default(),
        track: Track::new(BlendMode::Normal),
        blend_override: None,
        transition: None,
        trigger_style: Default::default(),
        target: Default::default(),
        beat_snap: None,
        channels: Default::default(),
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

/// A copy of `clip` that shares no id with it (clip and effects).
pub fn clone_with_fresh_ids(clip: &Clip) -> Clip {
    let mut c = clip.clone();
    c.id = Uuid::new_v4();
    for e in &mut c.track.effects {
        e.id = Uuid::new_v4();
    }
    c
}

/// A copy of the deck under a new id and name, sharing no clip or effect id with it.
pub fn duplicate_deck(p: &Project, deck: DeckId) -> Option<Deck> {
    let d = p.composition.decks.iter().find(|d| d.id == deck)?;
    let mut c = d.clone();
    c.id = Uuid::new_v4();
    c.name = format!("{} copy", d.name);
    for cc in &mut c.clips {
        cc.clip = clone_with_fresh_ids(&cc.clip);
    }
    Some(c)
}

fn move_clip(
    p: &mut Project,
    deck: DeckId,
    from: (LayerId, ColumnIdx),
    to: (LayerId, ColumnIdx),
    copy: bool,
) -> Result<Applied, Error> {
    check_cell(p, from.0, from.1)?;
    check_cell(p, to.0, to.1)?;
    let d = deck_mut(p, deck)?;
    let deck_id = d.id;
    let src = d.clip_at(from.0, from.1).cloned().ok_or(Error::NoClip)?;
    if from == to {
        return Ok(Applied {
            inverse: None,
            events: vec![],
        });
    }
    let dst = d.clip_at(to.0, to.1).cloned();
    let changed = |(layer, column)| Event::CellChanged {
        deck: deck_id,
        layer,
        column,
    };
    if copy {
        d.set_clip(to.0, to.1, clone_with_fresh_ids(&src));
        let inverse = match dst {
            Some(clip) => Command::SetClip {
                deck: deck_id,
                layer: to.0,
                column: to.1,
                clip,
            },
            None => Command::ClearCell {
                deck: deck_id,
                layer: to.0,
                column: to.1,
            },
        };
        return undoable(inverse, vec![changed(to)]);
    }
    for c in &mut d.clips {
        if (c.layer, c.column) == from {
            (c.layer, c.column) = to;
        } else if (c.layer, c.column) == to {
            (c.layer, c.column) = from;
        }
    }
    let mut moves = vec![(from, to)];
    if dst.is_some() {
        moves.push((to, from));
    }
    // The undo moves it back (or swaps back: the origin is occupied again by then).
    undoable(
        Command::MoveClip {
            deck: deck_id,
            from: to,
            to: from,
            copy: false,
        },
        vec![Event::ClipsMoved {
            deck: deck_id,
            moves,
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
        // The player recognises `pattern@fps` as an image sequence.
        Source::Sequence { pattern, fps } => Ok(Event::ClipStarted {
            deck,
            layer,
            column,
            path: PathBuf::from(format!("{}@{}", pattern.display(), fps)),
        }),
        Source::Capture { device } => Ok(Event::CaptureStarted {
            deck,
            layer,
            column,
            device: device.clone(),
        }),
        // Spout is Windows-only: such clips (from imports) stay placeholders.
        Source::Network { protocol, name } if *protocol != vjc_model::NetProtocol::Spout => {
            Ok(Event::NetworkStarted {
                deck,
                layer,
                column,
                protocol: protocol.clone(),
                name: name.clone(),
            })
        }
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

/// Same event, aimed at another layer.
fn with_layer(ev: Event, to: LayerId) -> Event {
    match ev {
        Event::ClipStarted {
            deck, column, path, ..
        } => Event::ClipStarted {
            deck,
            layer: to,
            column,
            path,
        },
        Event::CaptureStarted {
            deck,
            column,
            device,
            ..
        } => Event::CaptureStarted {
            deck,
            layer: to,
            column,
            device,
        },
        Event::NetworkStarted {
            deck,
            column,
            protocol,
            name,
            ..
        } => Event::NetworkStarted {
            deck,
            layer: to,
            column,
            protocol,
            name,
        },
        Event::GeneratorStarted { deck, column, .. } => Event::GeneratorStarted {
            deck,
            layer: to,
            column,
        },
        other => other,
    }
}

/// What is live right now: runtime state the pure project does not hold.
#[derive(Debug, Clone, Default)]
pub struct TriggerCtx {
    /// The selected layer (target "Active layer").
    pub selected: Option<LayerId>,
    /// Layers with a clip in the air: (layer it plays on, deck, layer the clip lives in,
    /// column). The two layers differ when the clip was redirected.
    pub playing: Vec<(LayerId, DeckId, LayerId, ColumnIdx)>,
}

/// The layer a clip of `own` plays on under target `style`.
pub fn resolve_target(
    p: &Project,
    ctx: &TriggerCtx,
    own: LayerId,
    style: vjc_model::ClipTarget,
) -> LayerId {
    let layers = &p.composition.layers;
    match style {
        vjc_model::ClipTarget::Active => ctx
            .selected
            .filter(|s| layers.iter().any(|l| l.id == *s))
            .unwrap_or(own),
        // Layers are ordered top to bottom: the first empty one from the bottom.
        vjc_model::ClipTarget::Free => layers
            .iter()
            .rev()
            .map(|l| l.id)
            .find(|id| !ctx.playing.iter().any(|x| x.0 == *id))
            .unwrap_or(own),
        _ => own,
    }
}

/// Effective trigger style of the clip in a cell (Normal when the cell is empty).
pub fn trigger_style_of(
    p: &Project,
    deck: DeckId,
    layer: LayerId,
    column: ColumnIdx,
) -> vjc_model::TriggerStyle {
    clip_in(p, deck, layer, column).map_or(vjc_model::TriggerStyle::Normal, |k| {
        k.trigger_style.resolve(p.composition.trigger_style)
    })
}

/// Beat snap that governs `cmd`: a clip trigger uses the clip's override, then its
/// column's, then the composition's (`pref` stands in when the composition has none); a
/// column trigger uses the column's, then the composition's. Other commands: the
/// composition's.
pub fn effective_beat_snap(
    p: &Project,
    pref: vjc_model::BeatSnap,
    cmd: &Command,
) -> vjc_model::BeatSnap {
    let comp = p.composition.beat_snap.unwrap_or(pref);
    let col = |i: ColumnIdx| {
        p.composition
            .columns
            .get(i as usize)
            .and_then(|c| c.beat_snap)
    };
    match *cmd {
        Command::TriggerClip {
            deck,
            layer,
            column,
        }
        | Command::TriggerClipOn {
            deck,
            layer,
            column,
            ..
        } => vjc_model::BeatSnap::effective(
            clip_in(p, deck, layer, column).and_then(|k| k.beat_snap),
            col(column),
            comp,
        ),
        Command::TriggerColumn { column, .. } => {
            vjc_model::BeatSnap::effective(None, col(column), comp)
        }
        _ => comp,
    }
}

/// Turns a user's `TriggerClip` into what must really run: an eject (toggle on the clip
/// that is playing), a redirect to the target layer, or the command unchanged. Other
/// commands pass through. The eject is immediate: it does not wait for the beat snap.
pub fn resolve_trigger(p: &Project, ctx: &TriggerCtx, cmd: Command) -> Command {
    let Command::TriggerClip {
        deck,
        layer,
        column,
    } = cmd
    else {
        return cmd;
    };
    let Some(k) = clip_in(p, deck, layer, column) else {
        return cmd;
    };
    if trigger_style_of(p, deck, layer, column) == vjc_model::TriggerStyle::Toggle
        && let Some(x) = ctx
            .playing
            .iter()
            .find(|x| x.1 == deck && x.2 == layer && x.3 == column)
    {
        return Command::ClearLayer { layer: x.0 };
    }
    let onto = resolve_target(p, ctx, layer, k.target.resolve(p.composition.clip_target));
    if onto == layer {
        cmd
    } else {
        Command::TriggerClipOn {
            deck,
            layer,
            column,
            onto,
        }
    }
}

/// A Hold clip the button or key put in the air.
#[derive(Debug, Clone, PartialEq)]
pub struct HoldFired {
    pub deck: DeckId,
    /// Layer of the cell the clip lives in.
    pub layer: LayerId,
    pub column: ColumnIdx,
    pub clip: vjc_model::ClipId,
    /// Layer it plays on.
    pub onto: LayerId,
}

/// The eject that ends a Hold on release: only while the clip the Hold fired is still the
/// one in the air (same origin cell, same clip id). Anything else that took the layer
/// since is left alone.
pub fn hold_release(
    p: &Project,
    playing: &[(LayerId, DeckId, LayerId, ColumnIdx)],
    h: &HoldFired,
) -> Option<Command> {
    let same_air = playing.contains(&(h.onto, h.deck, h.layer, h.column));
    let same_clip = clip_in(p, h.deck, h.layer, h.column).is_some_and(|k| k.id == h.clip);
    (same_air && same_clip).then_some(Command::ClearLayer { layer: h.onto })
}

fn clip_mut(
    p: &mut Project,
    deck: DeckId,
    layer: LayerId,
    column: ColumnIdx,
) -> Result<&mut Clip, Error> {
    check_cell(p, layer, column)?;
    deck_mut(p, deck)?
        .clips
        .iter_mut()
        .find(|k| k.layer == layer && k.column == column)
        .map(|k| &mut k.clip)
        .ok_or(Error::NoClip)
}

fn trigger_column(p: &Project, deck: DeckId, column: ColumnIdx) -> Result<Applied, Error> {
    if (column as usize) >= p.composition.columns.len() {
        return Err(Error::NoColumn);
    }
    deck_idx(p, deck)?;
    let mut events = vec![];
    for l in p.composition.layers.iter().filter(|l| {
        let own = clip_in(p, deck, l.id, column).and_then(|k| k.extras.ignore_column_trigger);
        !own.unwrap_or(p.composition.ignores_column(l)) && !p.composition.locked(l)
    }) {
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

/// Applies a command to the project. Pure: no I/O, no UI, no players. Effect shortcuts are
/// re-pointed afterwards so they keep following their effect (see `shortcuts::rebind`).
pub fn apply(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
    let cmd = resolve_layer_name(p, cmd);
    // Empty names are rejected for every user edit; undo/redo go through `restore` because
    // a name that was already empty must be restorable.
    if let Command::RenameLayer { name, .. }
    | Command::RenameClip { name, .. }
    | Command::RenameDeck { name, .. }
    | Command::RenameColumn { name, .. }
    | Command::RenameGroup { name, .. } = &cmd
        && name.trim().is_empty()
    {
        return Err(Error::InvalidValue);
    }
    // A locked layer ignores trigger, eject, drop and clear from the user. Undo/redo go
    // through `restore` and are not gated.
    let mut touched = vec![];
    touched_layers(&cmd, &mut touched);
    if touched.iter().any(|l| is_locked(p, *l)) {
        return Err(Error::Locked);
    }
    // Fader start fires only for the user's own opacity edit, never for undo/redo.
    let rise = match &cmd {
        Command::SetOpacity { layer, value } => p
            .composition
            .layers
            .iter()
            .find(|l| l.id == *layer)
            .filter(|l| {
                !p.composition.locked(l)
                    && (l.fader_start
                        || p.composition.decks.iter().any(|d| {
                            d.clips
                                .iter()
                                .any(|c| c.layer == l.id && c.clip.extras.fader_start == Some(true))
                        }))
            })
            .is_some_and(|l| number(&l.track.opacity.value) <= 0.0 && *value > 0.0)
            .then_some(*layer),
        _ => None,
    };
    let mut applied = restore(p, cmd)?;
    if let Some(layer) = rise {
        applied.events.push(Event::FaderStart { layer });
    }
    Ok(applied)
}

/// Layers whose content a command would change or play (looks inside `Batch`).
fn touched_layers(cmd: &Command, out: &mut Vec<LayerId>) {
    match cmd {
        Command::ImportMedia { layer, .. }
        | Command::SetClip { layer, .. }
        | Command::SetGenerator { layer, .. }
        | Command::ClearCell { layer, .. }
        | Command::TriggerClip { layer, .. }
        | Command::StepClip { layer, .. }
        | Command::ClearLayer { layer }
        | Command::ClearLayerClips { layer, .. }
        | Command::RemoveLayerWithClips { layer } => out.push(*layer),
        Command::MoveClip { from, to, copy, .. } => {
            if !*copy {
                out.push(from.0);
            }
            out.push(to.0);
        }
        Command::TriggerClipOn { layer, onto, .. } => out.extend([*layer, *onto]),
        Command::Batch { commands } => commands.iter().for_each(|c| touched_layers(c, out)),
        _ => {}
    }
}

/// A layer rename with '#' becomes the final name ('#' = the layer's number, bottom = 1),
/// so the stored command (and its undo) holds the name as it was set.
pub fn resolve_layer_name(p: &Project, cmd: Command) -> Command {
    match cmd {
        Command::RenameLayer { layer, name } if name.contains('#') => {
            let name = match p.composition.layers.iter().position(|l| l.id == layer) {
                Some(i) => name.replace('#', &(p.composition.layers.len() - i).to_string()),
                None => name,
            };
            Command::RenameLayer { layer, name }
        }
        c => c,
    }
}

/// Group a layer inserted at `index` (0 = top) must join: the neighbours' group when both
/// share one, so the group stays contiguous.
pub fn insert_group_at(p: &Project, index: usize) -> Option<GroupId> {
    let ls = &p.composition.layers;
    let above = index.checked_sub(1).and_then(|i| ls.get(i))?.group?;
    (ls.get(index)?.group == Some(above)).then_some(above)
}

/// True when the layer exists and has its content locked.
pub fn is_locked(p: &Project, layer: LayerId) -> bool {
    p.composition
        .layers
        .iter()
        .any(|l| l.id == layer && p.composition.locked(l))
}

/// Layer number as shown to the user: the bottom layer is 1.
pub fn layer_number(p: &Project, layer: LayerId) -> Option<usize> {
    let i = p.composition.layers.iter().position(|l| l.id == layer)?;
    Some(p.composition.layers.len() - i)
}

/// Command that puts a copy of the layer (clips, effects, params; fresh ids) right above
/// it. Undo removes the copy with its clips. None when the layer does not exist.
pub fn duplicate_layer_command(p: &Project, layer: LayerId) -> Option<Command> {
    let i = p.composition.layers.iter().position(|l| l.id == layer)?;
    let mut copy = p.composition.layers[i].clone();
    copy.id = Uuid::new_v4();
    copy.name = format!("{} copy", copy.name);
    for e in &mut copy.track.effects {
        e.id = Uuid::new_v4();
    }
    let mut clips = vec![];
    for d in &p.composition.decks {
        let own = d.clips.iter().filter(|c| c.layer == layer);
        for (at, cc) in (d.clips.len()..).zip(own) {
            let mut n = cc.clone();
            n.layer = copy.id;
            n.clip = clone_with_fresh_ids(&cc.clip);
            clips.push((d.id, at, n));
        }
    }
    Some(Command::RestoreLayer {
        layer: copy,
        index: i,
        clips,
        slices: vec![],
        cues: vec![],
        shortcuts: vec![],
    })
}

/// `apply` without the user-input checks: for undo/redo replays.
fn restore(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
    let r = apply_raw(p, cmd)?;
    shortcuts::rebind(p);
    Ok(r)
}

fn apply_raw(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
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
        Command::MoveClip {
            deck,
            from,
            to,
            copy,
        } => move_clip(p, deck, from, to, copy),
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
        Command::TriggerClipOn {
            deck,
            layer,
            column,
            onto,
        } => {
            check_cell(p, layer, column)?;
            layer_idx(p, onto)?;
            let ev = trigger_event(p, deck, layer, column)?;
            Ok(Applied {
                inverse: None,
                events: vec![with_layer(ev, onto)],
            })
        }
        Command::TriggerColumn { deck, column } => trigger_column(p, deck, column),
        Command::StepColumn { deck, from, delta } => {
            let n = p.composition.columns.len() as i64;
            if n == 0 {
                return Err(Error::NoColumn);
            }
            // Empty columns (no clip in any layer of the deck) are passed over. With nothing
            // filled in that direction the step stays where it is (clamped, no wrap).
            let filled = |c: i64| {
                deck_idx(p, deck).is_ok_and(|d| {
                    p.composition.decks[d]
                        .clips
                        .iter()
                        .any(|k| i64::from(k.column) == c)
                })
            };
            let to = match from {
                None => (0..n).find(|c| filled(*c)).unwrap_or(0),
                Some(f) => {
                    let f = i64::from(f).clamp(0, n - 1);
                    let sign = if delta < 0 { -1 } else { 1 };
                    let mut c = f;
                    // Delta 0 retriggers the current column.
                    let mut left = delta.unsigned_abs();
                    let mut at = f;
                    while left > 0 {
                        c += sign;
                        if !(0..n).contains(&c) {
                            break;
                        }
                        if filled(c) {
                            at = c;
                            left -= 1;
                        }
                    }
                    at
                }
            };
            trigger_column(p, deck, to as ColumnIdx)
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
        Command::SetMediaPaths { changes } => {
            // Validate first so a bad entry leaves the project untouched.
            for (deck, layer, column, _) in &changes {
                let d = p
                    .composition
                    .decks
                    .iter()
                    .find(|d| d.id == *deck)
                    .ok_or(Error::NoDeck)?;
                match d.clip_at(*layer, *column).map(|c| &c.source) {
                    Some(vjc_model::Source::File { .. }) => {}
                    _ => return Err(Error::NoClip),
                }
            }
            let mut prev = Vec::with_capacity(changes.len());
            for (deck, layer, column, path) in changes {
                if let Some(d) = p.composition.decks.iter_mut().find(|d| d.id == deck)
                    && let Some(c) = d
                        .clips
                        .iter_mut()
                        .find(|c| c.layer == layer && c.column == column)
                    && let vjc_model::Source::File { path: old } = &mut c.clip.source
                {
                    prev.push((deck, layer, column, std::mem::replace(old, path)));
                }
            }
            // Undo in reverse so a cell listed twice ends on its first old path.
            prev.reverse();
            undoable(Command::SetMediaPaths { changes: prev }, vec![])
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
        Command::SeekClip {
            deck,
            layer,
            column,
            pos,
        } => {
            transport_mut(p, deck, layer, column)?;
            if !pos.is_finite() {
                return Err(Error::InvalidValue);
            }
            Ok(Applied {
                inverse: None,
                events: vec![Event::Seek {
                    deck,
                    layer,
                    column,
                    pos: pos.clamp(0.0, 1.0),
                }],
            })
        }
        Command::SetInOut {
            deck,
            layer,
            column,
            in_point,
            out_point,
        } => {
            if !(in_point.is_finite()
                && out_point.is_finite()
                && in_point >= 0.0
                && out_point <= 1.0
                && out_point - in_point >= playback::MIN_REGION)
            {
                return Err(Error::InvalidValue);
            }
            let t = transport_mut(p, deck, layer, column)?;
            let prev = (t.in_point, t.out_point);
            (t.in_point, t.out_point) = (in_point, out_point);
            undoable(
                Command::SetInOut {
                    deck,
                    layer,
                    column,
                    in_point: prev.0,
                    out_point: prev.1,
                },
                vec![],
            )
        }
        Command::SetClipSync {
            deck,
            layer,
            column,
            beat_sync,
            timecode_start,
        } => {
            if beat_sync.is_some() && timecode_start.is_some()
                || beat_sync.is_some_and(|b| !b.is_finite() || b <= 0.0)
                || timecode_start.is_some_and(|t| !timecode::is_valid(&t))
            {
                return Err(Error::InvalidValue);
            }
            let t = transport_mut(p, deck, layer, column)?;
            let prev = (
                std::mem::replace(&mut t.beat_sync, beat_sync),
                std::mem::replace(&mut t.timecode_start, timecode_start),
            );
            undoable(
                Command::SetClipSync {
                    deck,
                    layer,
                    column,
                    beat_sync: prev.0,
                    timecode_start: prev.1,
                },
                vec![],
            )
        }
        Command::ClearAll => Ok(Applied {
            inverse: None,
            events: p
                .composition
                .layers
                .iter()
                .filter(|l| !p.composition.locked(l))
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
        Command::SetTransport { state } => {
            p.composition.tempo.play_state = state;
            let events = if state == vjc_model::PlayState::Stop {
                p.composition
                    .layers
                    .iter()
                    .filter(|l| !p.composition.locked(l))
                    .map(|l| Event::LayerCleared { layer: l.id })
                    .collect()
            } else {
                vec![]
            };
            Ok(Applied {
                inverse: None,
                events,
            })
        }
        Command::SetMasterReverse { reverse } => {
            let prev = std::mem::replace(&mut p.composition.master_reverse, reverse);
            undoable(Command::SetMasterReverse { reverse: prev }, vec![])
        }
        Command::SetMetronome { on } => {
            let prev = std::mem::replace(&mut p.composition.tempo.metronome, on);
            undoable(Command::SetMetronome { on: prev }, vec![])
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
        Command::ClearLayerClips { deck, layer } => {
            layer_idx(p, layer)?;
            let d = deck_mut(p, deck)?;
            let deck_id = d.id;
            let removed: Vec<CellClip> = d
                .clips
                .iter()
                .filter(|c| c.layer == layer)
                .cloned()
                .collect();
            if removed.is_empty() {
                return Ok(Applied {
                    inverse: None,
                    events: vec![],
                });
            }
            d.clips.retain(|c| c.layer != layer);
            let events = removed
                .iter()
                .map(|c| Event::CellChanged {
                    deck: deck_id,
                    layer,
                    column: c.column,
                })
                .collect();
            let commands = removed
                .into_iter()
                .map(|c| Command::SetClip {
                    deck: deck_id,
                    layer,
                    column: c.column,
                    clip: c.clip,
                })
                .collect();
            undoable(Command::Batch { commands }, events)
        }
        Command::SetLayerLock { layer, locked } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].lock_content, locked);
            undoable(
                Command::SetLayerLock {
                    layer,
                    locked: prev,
                },
                vec![],
            )
        }
        Command::SetLayerFaderStart { layer, enabled } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].fader_start, enabled);
            undoable(
                Command::SetLayerFaderStart {
                    layer,
                    enabled: prev,
                },
                vec![],
            )
        }
        Command::SetLayerMaskMode { layer, mode } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].mask_mode, mode);
            undoable(Command::SetLayerMaskMode { layer, mode: prev }, vec![])
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
        Command::SetCrossfader { value } => set_crossfader(p, value),
        Command::GoCrossfader { bus, pressed } => {
            let to_b = matches!(bus, vjc_model::Bus::B);
            if pressed {
                return set_crossfader(p, if to_b { 1.0 } else { 0.0 });
            }
            let returns = p.composition.crossfader.as_ref().map(|c| c.behaviour)
                == Some(vjc_model::CrossfadeBehaviour::CutReturn);
            if !returns {
                return Ok(Applied {
                    inverse: None,
                    events: vec![],
                });
            }
            set_crossfader(p, if to_b { 0.0 } else { 1.0 })
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
        Command::SetCrossfaderBehaviour { behaviour } => {
            let cf = p
                .composition
                .crossfader
                .get_or_insert_with(crossfader::new_crossfader);
            let prev = std::mem::replace(&mut cf.behaviour, behaviour);
            undoable(Command::SetCrossfaderBehaviour { behaviour: prev }, vec![])
        }
        Command::SetCrossfaderBlend { blend } => {
            let cf = p
                .composition
                .crossfader
                .get_or_insert_with(crossfader::new_crossfader);
            let prev = std::mem::replace(&mut cf.blend, blend);
            undoable(Command::SetCrossfaderBlend { blend: prev }, vec![])
        }
        Command::SetLayerAudio { layer, enabled } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].audio, enabled);
            undoable(
                Command::SetLayerAudio {
                    layer,
                    enabled: prev,
                },
                vec![],
            )
        }
        Command::SetLayerVideo { layer, enabled } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].video, enabled);
            undoable(
                Command::SetLayerVideo {
                    layer,
                    enabled: prev,
                },
                vec![],
            )
        }
        Command::SetClipChannels {
            deck,
            layer,
            column,
            channels,
        } => {
            check_cell(p, layer, column)?;
            let k = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = std::mem::replace(&mut k.clip.channels, channels);
            undoable(
                Command::SetClipChannels {
                    deck,
                    layer,
                    column,
                    channels: prev,
                },
                vec![],
            )
        }
        Command::AddScreen { screen, index } => {
            let id = screen.id;
            let at = index.min(p.output.screens.len());
            p.output.screens.insert(at, screen);
            // One monitor drives one screen only (also when undoing a RemoveScreen after
            // the monitor was given to another screen): the newcomer falls back to a window.
            if let vjc_model::OutputDevice::Monitor { name } = &p.output.screens[at].device
                && p.output.screens.iter().enumerate().any(|(i, o)| {
                    i != at
                        && matches!(&o.device, vjc_model::OutputDevice::Monitor { name: n } if n == name)
                })
            {
                p.output.screens[at].device = vjc_model::OutputDevice::Window;
            }
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
            // One monitor drives one screen only.
            if let vjc_model::OutputDevice::Monitor { name } = &device
                && p.output.screens.iter().any(|o| {
                    o.id != screen
                        && matches!(&o.device, vjc_model::OutputDevice::Monitor { name: n } if n == name)
                })
            {
                return Err(Error::MonitorInUse);
            }
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
        Command::SetScreenPixelPerfect {
            screen,
            pixel_perfect,
        } => {
            let s = screen_mut(p, screen)?;
            let prev = std::mem::replace(&mut s.pixel_perfect, pixel_perfect);
            undoable(
                Command::SetScreenPixelPerfect {
                    screen,
                    pixel_perfect: prev,
                },
                vec![],
            )
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
        Command::SetOutputSetup { screens } => {
            if !mapping::screens_valid(&screens) {
                return Err(Error::InvalidValue);
            }
            let mut monitors = std::collections::HashSet::new();
            for s in &screens {
                if let vjc_model::OutputDevice::Monitor { name } = &s.device
                    && !monitors.insert(name.clone())
                {
                    return Err(Error::MonitorInUse);
                }
            }
            let prev = std::mem::replace(&mut p.output.screens, screens);
            undoable(Command::SetOutputSetup { screens: prev }, vec![])
        }
        Command::SetSliceSource {
            screen,
            slice,
            source,
        } => {
            let s = slice_mut(p, screen, slice)?;
            let prev = std::mem::replace(&mut s.source, source);
            undoable(
                Command::SetSliceSource {
                    screen,
                    slice,
                    source: prev,
                },
                vec![],
            )
        }
        Command::Batch { commands } => {
            if commands.is_empty() {
                return Err(Error::InvalidValue);
            }
            let before = p.clone();
            let (mut inverses, mut events) = (vec![], vec![]);
            for c in commands {
                match apply(p, c) {
                    Ok(a) => {
                        inverses.extend(a.inverse);
                        events.extend(a.events);
                    }
                    Err(e) => {
                        *p = before;
                        return Err(e);
                    }
                }
            }
            inverses.reverse();
            Ok(Applied {
                inverse: (!inverses.is_empty()).then_some(Command::Batch { commands: inverses }),
                events,
            })
        }
        Command::MoveSlice {
            from,
            to,
            slice,
            index,
        } => {
            if from == to {
                return Err(Error::InvalidValue);
            }
            screen_idx(p, to)?;
            let src = screen_mut(p, from)?;
            let i = src
                .slices
                .iter()
                .position(|x| x.id == slice)
                .ok_or(Error::NoSlice)?;
            let sl = src.slices.remove(i);
            let dst = screen_mut(p, to)?;
            dst.slices.insert(index.min(dst.slices.len()), sl);
            undoable(
                Command::MoveSlice {
                    from: to,
                    to: from,
                    slice,
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
        Command::SetSliceMask {
            screen,
            slice,
            mask,
        } => {
            if mask.as_ref().is_some_and(|m| !mapping::mask_valid(m)) {
                return Err(Error::InvalidValue);
            }
            let mask = mask.map(|m| {
                m.into_iter()
                    .map(|p| (p.0.clamp(0.0, 1.0), p.1.clamp(0.0, 1.0)))
                    .collect()
            });
            let s = slice_mut(p, screen, slice)?;
            let prev = std::mem::replace(&mut s.mask, mask);
            undoable(
                Command::SetSliceMask {
                    screen,
                    slice,
                    mask: prev,
                },
                vec![],
            )
        }
        Command::SetSliceMaskInvert {
            screen,
            slice,
            invert,
        } => {
            let s = slice_mut(p, screen, slice)?;
            let prev = std::mem::replace(&mut s.mask_invert, invert);
            undoable(
                Command::SetSliceMaskInvert {
                    screen,
                    slice,
                    invert: prev,
                },
                vec![],
            )
        }
        Command::SetSliceKey {
            screen,
            slice,
            is_key,
            black_bg,
        } => {
            let s = slice_mut(p, screen, slice)?;
            let (pk, pb) = (
                std::mem::replace(&mut s.is_key, is_key),
                std::mem::replace(&mut s.black_bg, black_bg),
            );
            undoable(
                Command::SetSliceKey {
                    screen,
                    slice,
                    is_key: pk,
                    black_bg: pb,
                },
                vec![],
            )
        }
        Command::SetScreenAdditive { screen, on } => {
            let s = screen_mut(p, screen)?;
            let prev = std::mem::replace(&mut s.additive_overlap, on);
            undoable(Command::SetScreenAdditive { screen, on: prev }, vec![])
        }
        Command::SetScreenDelay { screen, delay_ms } => {
            let s = screen_mut(p, screen)?;
            let prev = std::mem::replace(&mut s.delay_ms, delay_ms.min(mapping::MAX_DELAY_MS));
            undoable(
                Command::SetScreenDelay {
                    screen,
                    delay_ms: prev,
                },
                vec![],
            )
        }
        Command::SetSliceSoftEdge {
            screen,
            slice,
            soft_edge,
        } => {
            let soft_edge = match soft_edge {
                Some(e) => Some(mapping::clamp_soft_edge(&e).ok_or(Error::InvalidValue)?),
                None => None,
            };
            let s = slice_mut(p, screen, slice)?;
            let prev = std::mem::replace(&mut s.soft_edge, soft_edge);
            undoable(
                Command::SetSliceSoftEdge {
                    screen,
                    slice,
                    soft_edge: prev,
                },
                vec![],
            )
        }
        Command::SetSliceColor {
            screen,
            slice,
            color,
        } => {
            let color = mapping::clamp_color(&color).ok_or(Error::InvalidValue)?;
            let s = slice_mut(p, screen, slice)?;
            let prev = std::mem::replace(&mut s.color, color);
            undoable(
                Command::SetSliceColor {
                    screen,
                    slice,
                    color: prev,
                },
                vec![],
            )
        }
        Command::SetScreenEnabled { screen, enabled } => {
            let s = screen_mut(p, screen)?;
            let prev = std::mem::replace(&mut s.enabled, enabled);
            undoable(
                Command::SetScreenEnabled {
                    screen,
                    enabled: prev,
                },
                vec![],
            )
        }
        Command::SetScreenColor { screen, color } => {
            let color = mapping::clamp_screen_color(&color).ok_or(Error::InvalidValue)?;
            let s = screen_mut(p, screen)?;
            let prev = std::mem::replace(&mut s.color, color);
            undoable(
                Command::SetScreenColor {
                    screen,
                    color: prev,
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
        Command::SetTriggerMode {
            deck,
            layer,
            column,
            mode,
        } => {
            let prev = std::mem::replace(
                &mut transport_mut(p, deck, layer, column)?.trigger_mode,
                mode,
            );
            undoable(
                Command::SetTriggerMode {
                    deck,
                    layer,
                    column,
                    mode: prev,
                },
                vec![],
            )
        }
        Command::SetLayerTriggerMode { layer, mode } => {
            let i = layer_idx(p, layer)?;
            let mode = mode.resolve(vjc_model::TriggerMode::Restart);
            let prev = std::mem::replace(&mut p.composition.layers[i].trigger_mode, mode);
            undoable(Command::SetLayerTriggerMode { layer, mode: prev }, vec![])
        }
        Command::SetCompAutopilot { autopilot } => {
            let prev = std::mem::replace(&mut p.composition.autopilot, autopilot);
            undoable(Command::SetCompAutopilot { autopilot: prev }, vec![])
        }
        Command::SetColumnAutopilot { column, autopilot } => {
            let c = p
                .composition
                .columns
                .iter_mut()
                .find(|c| c.id == column)
                .ok_or(Error::NoColumn)?;
            let prev = std::mem::replace(&mut c.autopilot, autopilot);
            undoable(
                Command::SetColumnAutopilot {
                    column,
                    autopilot: prev,
                },
                vec![],
            )
        }
        Command::SetCompBeatSnap { snap } => {
            let prev = std::mem::replace(&mut p.composition.beat_snap, snap);
            undoable(Command::SetCompBeatSnap { snap: prev }, vec![])
        }
        Command::SetColumnBeatSnap { column, snap } => {
            let c = p
                .composition
                .columns
                .iter_mut()
                .find(|c| c.id == column)
                .ok_or(Error::NoColumn)?;
            let prev = std::mem::replace(&mut c.beat_snap, snap);
            undoable(Command::SetColumnBeatSnap { column, snap: prev }, vec![])
        }
        Command::SetClipBeatSnap {
            deck,
            layer,
            column,
            snap,
        } => {
            let k = clip_mut(p, deck, layer, column)?;
            let prev = std::mem::replace(&mut k.beat_snap, snap);
            undoable(
                Command::SetClipBeatSnap {
                    deck,
                    layer,
                    column,
                    snap: prev,
                },
                vec![],
            )
        }
        Command::SetCompTriggerStyle { style } => {
            let prev = std::mem::replace(&mut p.composition.trigger_style, style);
            undoable(Command::SetCompTriggerStyle { style: prev }, vec![])
        }
        Command::SetCompDescription { text } => {
            let prev = std::mem::replace(&mut p.composition.description, text);
            undoable(Command::SetCompDescription { text: prev }, vec![])
        }
        Command::SetCompBitDepth { depth } => {
            if !vjc_model::BIT_DEPTHS.contains(&depth) {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(&mut p.composition.bit_depth, depth);
            undoable(Command::SetCompBitDepth { depth: prev }, vec![])
        }
        Command::SetColor { target, color } => {
            if color.is_some_and(|c| c == 0 || c > vjc_model::PALETTE_LEN) {
                return Err(Error::InvalidValue);
            }
            let prev = match target {
                ColorTarget::Layer(l) => {
                    let i = layer_idx(p, l)?;
                    std::mem::replace(&mut p.composition.layers[i].color, color)
                }
                ColorTarget::Column(id) => {
                    let c = p
                        .composition
                        .columns
                        .iter_mut()
                        .find(|c| c.id == id)
                        .ok_or(Error::NoColumn)?;
                    std::mem::replace(&mut c.color, color)
                }
                ColorTarget::Clip {
                    deck,
                    layer,
                    column,
                } => {
                    let k = clip_mut(p, deck, layer, column)?;
                    std::mem::replace(&mut k.extras.color, color)
                }
            };
            undoable(
                Command::SetColor {
                    target,
                    color: prev,
                },
                vec![],
            )
        }
        Command::SetClipExtras {
            deck,
            layer,
            column,
            extras,
        } => {
            if extras
                .color
                .is_some_and(|c| c == 0 || c > vjc_model::PALETTE_LEN)
            {
                return Err(Error::InvalidValue);
            }
            let k = clip_mut(p, deck, layer, column)?;
            let prev = std::mem::replace(&mut k.extras, extras);
            undoable(
                Command::SetClipExtras {
                    deck,
                    layer,
                    column,
                    extras: prev,
                },
                vec![],
            )
        }
        Command::SetLayerFolded { layer, folded } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].folded, folded);
            undoable(
                Command::SetLayerFolded {
                    layer,
                    folded: prev,
                },
                vec![],
            )
        }
        Command::SetGroupFolded { group, folded } => {
            let g = p
                .composition
                .groups
                .iter_mut()
                .find(|g| g.id == group)
                .ok_or(Error::NoGroup)?;
            let prev = std::mem::replace(&mut g.folded, folded);
            undoable(
                Command::SetGroupFolded {
                    group,
                    folded: prev,
                },
                vec![],
            )
        }
        Command::SetCompClipTarget { target } => {
            let prev = std::mem::replace(&mut p.composition.clip_target, target);
            undoable(Command::SetCompClipTarget { target: prev }, vec![])
        }
        Command::SetClipTarget {
            deck,
            layer,
            column,
            target,
        } => {
            let k = clip_mut(p, deck, layer, column)?;
            let prev = std::mem::replace(&mut k.target, target);
            undoable(
                Command::SetClipTarget {
                    deck,
                    layer,
                    column,
                    target: prev,
                },
                vec![],
            )
        }
        Command::SetLayerIgnoreColumn { layer, ignore } => {
            let i = layer_idx(p, layer)?;
            let prev =
                std::mem::replace(&mut p.composition.layers[i].ignore_column_trigger, ignore);
            undoable(
                Command::SetLayerIgnoreColumn {
                    layer,
                    ignore: prev,
                },
                vec![],
            )
        }
        Command::SetClipTriggerStyle {
            deck,
            layer,
            column,
            style,
        } => {
            let k = clip_mut(p, deck, layer, column)?;
            let prev = std::mem::replace(&mut k.trigger_style, style);
            undoable(
                Command::SetClipTriggerStyle {
                    deck,
                    layer,
                    column,
                    style: prev,
                },
                vec![],
            )
        }
        Command::SetBlendOverride {
            deck,
            layer,
            column,
            blend,
        } => {
            check_cell(p, layer, column)?;
            let k = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = std::mem::replace(&mut k.clip.blend_override, blend);
            undoable(
                Command::SetBlendOverride {
                    deck,
                    layer,
                    column,
                    blend: prev,
                },
                vec![],
            )
        }
        Command::SetClipTransition {
            deck,
            layer,
            column,
            transition,
        } => {
            if transition.as_ref().is_some_and(|t| {
                !t.duration_secs.is_finite()
                    || t.duration_beats.is_some_and(|b| !b.is_finite() || b < 0.0)
            }) {
                return Err(Error::InvalidValue);
            }
            check_cell(p, layer, column)?;
            let k = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = std::mem::replace(&mut k.clip.transition, transition);
            undoable(
                Command::SetClipTransition {
                    deck,
                    layer,
                    column,
                    transition: prev,
                },
                vec![],
            )
        }
        Command::AddLayer { layer, index } => {
            let id = layer.id;
            let ls = &mut p.composition.layers;
            let mut index = index.min(ls.len());
            // Never split a group: an ungrouped layer lands above the group it hit.
            if layer.group.is_none()
                && index > 0
                && index < ls.len()
                && ls[index].group.is_some()
                && ls[index - 1].group == ls[index].group
            {
                let g = ls[index].group;
                while index > 0 && ls[index - 1].group == g {
                    index -= 1;
                }
            }
            ls.insert(index, layer);
            undoable(Command::RemoveLayer { layer: id }, vec![])
        }
        Command::MoveLayer { layer, to_index } => {
            let from = layer_idx(p, layer)?;
            if p.composition.layers[from].group.is_some() {
                return Err(Error::BadGroup);
            }
            let ls = &mut p.composition.layers;
            let l = ls.remove(from);
            let mut to = to_index.min(ls.len());
            if to > 0 && to < ls.len() && ls[to].group.is_some() && ls[to - 1].group == ls[to].group
            {
                let g = ls[to].group;
                while to > 0 && ls[to - 1].group == g {
                    to -= 1;
                }
            }
            ls.insert(to, l);
            Ok(Applied {
                inverse: (to != from).then_some(Command::MoveLayer {
                    layer,
                    to_index: from,
                }),
                events: vec![],
            })
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
            if let Some(g) = p.composition.layers[i].group
                && groups::members(p, g).len() < 2
            {
                return Err(Error::BadGroup);
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
        Command::RemoveLayerWithClips { layer } => {
            let i = layer_idx(p, layer)?;
            if p.composition.layers.len() <= 1 {
                return Err(Error::InUse);
            }
            // A router clip on another layer would point at nothing.
            if p.composition.decks.iter().any(|d| {
                d.clips.iter().any(|c| {
                    c.layer != layer
                        && matches!(c.clip.source, Source::LayerRouter { layer: l } if l == layer)
                })
            }) {
                return Err(Error::InUse);
            }
            if let Some(g) = p.composition.layers[i].group
                && groups::members(p, g).len() < 2
            {
                return Err(Error::BadGroup);
            }
            let mut clips = vec![];
            for d in &mut p.composition.decks {
                let mut at = 0;
                while at < d.clips.len() {
                    if d.clips[at].layer == layer {
                        clips.push((d.id, at, d.clips.remove(at)));
                    } else {
                        at += 1;
                    }
                }
            }
            let mut slices = vec![];
            for sc in &mut p.output.screens {
                for sl in &mut sc.slices {
                    if sl.source == vjc_model::SliceSource::Layer(layer) {
                        slices.push((sc.id, sl.id, sl.source));
                        sl.source = vjc_model::SliceSource::Composition;
                    }
                }
            }
            let mut cues = vec![];
            let mut at = 0;
            let mut k = 0;
            while at < p.composition.timecode_cues.len() {
                if p.composition.timecode_cues[at].layer == Some(layer) {
                    cues.push((k, p.composition.timecode_cues.remove(at)));
                } else {
                    at += 1;
                }
                k += 1;
            }
            // Shortcuts address layers by number: drop the ones aimed at this layer and
            // renumber the layers above it.
            let n = p.composition.layers.len() - i;
            let mut shortcuts = vec![];
            let mut at = 0;
            let mut k = 0;
            while at < p.shortcuts.len() {
                match shortcuts::layer_number(&p.shortcuts[at]) {
                    Some(m) if m == n => {
                        shortcuts.push((k, p.shortcuts.remove(at)));
                    }
                    Some(m) if m > n => {
                        shortcuts::set_layer_number(&mut p.shortcuts[at], m - 1);
                        at += 1;
                    }
                    _ => at += 1,
                }
                k += 1;
            }
            let removed = p.composition.layers.remove(i);
            undoable(
                Command::RestoreLayer {
                    layer: removed,
                    index: i,
                    clips,
                    slices,
                    cues,
                    shortcuts,
                },
                vec![Event::LayerCleared { layer }],
            )
        }
        Command::RestoreLayer {
            layer,
            index,
            clips,
            slices,
            cues,
            shortcuts: removed_shortcuts,
        } => {
            let id = layer.id;
            // Check everything first so a failure leaves the project untouched.
            if clips
                .iter()
                .any(|(deck, _, _)| !p.composition.decks.iter().any(|d| d.id == *deck))
            {
                return Err(Error::NoDeck);
            }
            let ls = &mut p.composition.layers;
            let at = index.min(ls.len());
            ls.insert(at, layer);
            let n = ls.len() - at;
            for s in &mut p.shortcuts {
                if let Some(m) = shortcuts::layer_number(s)
                    && m >= n
                {
                    shortcuts::set_layer_number(s, m + 1);
                }
            }
            for (at, s) in removed_shortcuts {
                p.shortcuts.insert(at.min(p.shortcuts.len()), s);
            }
            // Ascending positions per deck rebuild the original order.
            for (deck, at, clip) in clips {
                if let Some(d) = p.composition.decks.iter_mut().find(|d| d.id == deck) {
                    d.clips.insert(at.min(d.clips.len()), clip);
                }
            }
            for (sc, sl, source) in slices {
                if let Ok(s) = slice_mut(p, sc, sl) {
                    s.source = source;
                }
            }
            for (at, cue) in cues {
                let cs = &mut p.composition.timecode_cues;
                cs.insert(at.min(cs.len()), cue);
            }
            undoable(Command::RemoveLayerWithClips { layer: id }, vec![])
        }
        Command::AddColumn { column } => {
            let id = column.id;
            p.composition.columns.push(column);
            undoable(Command::RemoveColumn { column: id }, vec![])
        }
        Command::RenameColumn { column, name } => {
            let c = p
                .composition
                .columns
                .iter_mut()
                .find(|c| c.id == column)
                .ok_or(Error::NoColumn)?;
            let prev = std::mem::replace(&mut c.name, name);
            undoable(Command::RenameColumn { column, name: prev }, vec![])
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
        Command::MoveColumns { .. }
        | Command::ReorderColumns { .. }
        | Command::InsertColumn { .. }
        | Command::DuplicateColumns { .. }
        | Command::RemoveColumns { .. }
        | Command::RestoreColumns { .. } => columns::apply(p, cmd),
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
        Command::SetClipDuration {
            deck,
            layer,
            column,
            secs,
        } => {
            if secs.is_some_and(|s| !s.is_finite() || !(0.001..=86_400.0).contains(&s)) {
                return Err(Error::InvalidValue);
            }
            let t = transport_mut(p, deck, layer, column)?;
            let prev = std::mem::replace(&mut t.duration_secs, secs);
            undoable(
                Command::SetClipDuration {
                    deck,
                    layer,
                    column,
                    secs: prev,
                },
                vec![],
            )
        }
        Command::SetLayerExtras { layer, extras } => {
            if !extras.master.is_finite()
                || !(0.0..=1.0).contains(&extras.master)
                || extras.width > vjc_model::MAX_LAYER_SIDE
                || extras.height > vjc_model::MAX_LAYER_SIDE
            {
                return Err(Error::InvalidValue);
            }
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].extras, extras);
            undoable(
                Command::SetLayerExtras {
                    layer,
                    extras: prev,
                },
                vec![],
            )
        }
        Command::RefreshClipThumb {
            deck,
            layer,
            column,
        } => {
            clip_mut(p, deck, layer, column)?;
            Ok(Applied {
                inverse: None,
                events: vec![Event::ThumbRefresh {
                    deck,
                    layer,
                    column,
                }],
            })
        }
        Command::SetTimecodeStart {
            deck,
            layer,
            column,
            start,
        } => {
            if start.is_some_and(|t| !timecode::is_valid(&t)) {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(
                &mut transport_mut(p, deck, layer, column)?.timecode_start,
                start,
            );
            undoable(
                Command::SetTimecodeStart {
                    deck,
                    layer,
                    column,
                    start: prev,
                },
                vec![],
            )
        }
        Command::SetTimecodeCues { cues } => {
            if cues.iter().any(|c| !timecode::is_valid(&c.time)) {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(&mut p.composition.timecode_cues, cues);
            undoable(Command::SetTimecodeCues { cues: prev }, vec![])
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
            let prev = t.direction;
            t.set_direction(direction);
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
        Command::SetAudio {
            target,
            param,
            value,
        } => {
            if !value.is_finite() {
                return Err(Error::InvalidValue);
            }
            let track = match target {
                AudioTarget::Master => &mut p.composition.master,
                AudioTarget::Layer(l) => {
                    let i = layer_idx(p, l)?;
                    &mut p.composition.layers[i].track
                }
                AudioTarget::Clip(deck, layer, column) => {
                    check_cell(p, layer, column)?;
                    &mut deck_mut(p, deck)?
                        .clips
                        .iter_mut()
                        .find(|k| k.layer == layer && k.column == column)
                        .ok_or(Error::NoClip)?
                        .clip
                        .track
                }
            };
            let slot = match param {
                AudioParam::Volume => &mut track.volume,
                AudioParam::Pan => &mut track.pan,
            };
            let prev = number(&slot.value);
            slot.set_number(value);
            undoable(
                Command::SetAudio {
                    target,
                    param,
                    value: prev,
                },
                vec![],
            )
        }
        Command::SetPaused { clips, paused } => {
            // Validate every target first so a missing clip changes nothing.
            for &(deck, layer, column) in &clips {
                transport_mut(p, deck, layer, column)?;
            }
            let mut prev = vec![];
            for (deck, layer, column) in clips {
                let t = transport_mut(p, deck, layer, column)?;
                // Pausing stops any direction; a resume only leaves Pause, back to the saved one.
                let hit = if paused {
                    t.direction != Direction::Pause
                } else {
                    t.direction == Direction::Pause
                };
                if hit {
                    prev.push((deck, layer, column, t.direction));
                    if paused {
                        t.set_direction(Direction::Pause);
                    } else {
                        t.set_direction(t.resumed_direction());
                    }
                }
            }
            if prev.is_empty() {
                return Err(Error::InvalidValue);
            }
            undoable(Command::RestoreDirections { items: prev }, vec![])
        }
        Command::RestoreDirections { items } => {
            for &(deck, layer, column, _) in &items {
                transport_mut(p, deck, layer, column)?;
            }
            let mut prev = vec![];
            for (deck, layer, column, direction) in items {
                let t = transport_mut(p, deck, layer, column)?;
                prev.push((deck, layer, column, {
                    let before = t.direction;
                    t.set_direction(direction);
                    before
                }));
            }
            undoable(Command::RestoreDirections { items: prev }, vec![])
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
        Command::SetTransform {
            target,
            field,
            value,
        } => {
            if !value.is_finite() {
                return Err(Error::InvalidValue);
            }
            let slot = effects::track_mut(p, target)?.transform.field_mut(field);
            let prev = number(&slot.value);
            slot.set_number(value);
            undoable(
                Command::SetTransform {
                    target,
                    field,
                    value: prev,
                },
                vec![],
            )
        }
        Command::SetTransformMulti { target, values } => {
            if values.is_empty() || values.iter().any(|(_, v)| !v.is_finite()) {
                return Err(Error::InvalidValue);
            }
            let tr = &mut effects::track_mut(p, target)?.transform;
            let mut prev = Vec::with_capacity(values.len());
            for (field, value) in values {
                let slot = tr.field_mut(field);
                prev.push((field, number(&slot.value)));
                slot.set_number(value);
            }
            prev.reverse();
            undoable(
                Command::SetTransformMulti {
                    target,
                    values: prev,
                },
                vec![],
            )
        }
        Command::SetDialInfo {
            owner,
            dial,
            name,
            range,
        } => {
            let (name, range) = set_dial_info(dials_of(p, owner)?, dial, &name, range)?;
            undoable(
                Command::SetDialInfo {
                    owner,
                    dial,
                    name,
                    range,
                },
                vec![],
            )
        }
        Command::SetDashboardDial { dial, value } => {
            let prev = set_dial(&mut p.composition.dashboard, dial, value)?;
            undoable(Command::SetDashboardDial { dial, value: prev }, vec![])
        }
        Command::SetLayerDial { layer, dial, value } => {
            let i = layer_idx(p, layer)?;
            let prev = set_dial(&mut p.composition.layers[i].track.dials, dial, value)?;
            undoable(
                Command::SetLayerDial {
                    layer,
                    dial,
                    value: prev,
                },
                vec![],
            )
        }
        Command::SetClipDial {
            deck,
            layer,
            column,
            dial,
            value,
        } => {
            check_cell(p, layer, column)?;
            let k = deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = set_dial(&mut k.clip.track.dials, dial, value)?;
            undoable(
                Command::SetClipDial {
                    deck,
                    layer,
                    column,
                    dial,
                    value: prev,
                },
                vec![],
            )
        }
        Command::AddEffect { .. }
        | Command::RemoveEffect { .. }
        | Command::MoveEffect { .. }
        | Command::SetEffectBypass { .. }
        | Command::SetEffectDryWet { .. }
        | Command::SetEffectBlend { .. }
        | Command::ReplaceEffects { .. }
        | Command::SetEffectParam { .. }
        | Command::SetEffectParams { .. } => effects::apply(p, cmd),
        Command::CreateGroup { .. }
        | Command::RestoreEmptyGroup { .. }
        | Command::Ungroup { .. }
        | Command::RenameGroup { .. }
        | Command::SetGroupOpacity { .. }
        | Command::SetGroupBlend { .. }
        | Command::SetGroupBypass { .. }
        | Command::SetGroupSolo { .. }
        | Command::SetGroupMaskMode { .. }
        | Command::SetGroupIgnoreColumn { .. }
        | Command::SetGroupLock { .. }
        | Command::SetGroupBus { .. }
        | Command::SetGroupSpeed { .. }
        | Command::MoveLayerToGroup { .. }
        | Command::MoveLayerOutOfGroup { .. } => groups::apply(p, cmd),
        Command::AddShortcut { .. }
        | Command::RemoveShortcut { .. }
        | Command::UpdateShortcut { .. } => shortcuts::apply(p, cmd),
        Command::SetBpm { bpm } => {
            if !bpm.is_finite() || bpm <= 0.0 {
                return Err(Error::InvalidValue);
            }
            let bpm = bpm.clamp(clock::MIN_BPM, clock::MAX_BPM);
            let prev = std::mem::replace(&mut p.composition.tempo.bpm, bpm);
            undoable(Command::SetBpm { bpm: prev }, vec![])
        }
        Command::SetLink { enabled } => {
            let prev = std::mem::replace(&mut p.composition.tempo.link_enabled, enabled);
            undoable(Command::SetLink { enabled: prev }, vec![])
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
        let cmd = resolve_layer_name(p, cmd);
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
        let mut target = cmd.target();
        if let (
            Some(Target::SliceMask(sl, n, _)),
            Command::SetSliceMask {
                mask: Some(new), ..
            },
        ) = (target, &cmd)
        {
            let cur = p
                .output
                .screens
                .iter()
                .flat_map(|s| &s.slices)
                .find(|s| s.id == sl)
                .and_then(|s| s.mask.as_ref());
            let idx = match cur {
                Some(old) if old.len() == new.len() => {
                    old.iter().zip(new).position(|(a, b)| a != b).unwrap_or(0)
                }
                // Creating the mask or changing its point count is no drag of point 0.
                _ => usize::MAX,
            };
            target = Some(Target::SliceMask(sl, n, idx));
        }
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
        let r = restore(p, e.undo.clone());
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
        let r = restore(p, e.redo.clone());
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
    fn set_media_paths_is_one_undo_step_and_all_or_nothing() {
        let (mut p, l) = setup();
        let mut h = History::default();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let src = |p: &Project| match &clip_in(p, A, l, 0).unwrap().source {
            vjc_model::Source::File { path } => path.clone(),
            _ => panic!("not a file"),
        };
        let old = src(&p);
        let new = PathBuf::from("/media/new.mp4");
        h.exec(
            &mut p,
            Command::SetMediaPaths {
                changes: vec![(A, l, 0, new.clone())],
            },
            t,
        )
        .unwrap();
        assert_eq!(src(&p), new);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(src(&p), old);
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(src(&p), new);
        // Negative control: an empty cell in the batch rejects it and changes nothing.
        let before = p.clone();
        let r = h.exec(
            &mut p,
            Command::SetMediaPaths {
                changes: vec![(A, l, 0, old.clone()), (A, l, 9, old)],
            },
            t,
        );
        assert!(r.is_err());
        assert_eq!(p, before);
    }

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
                    ..Default::default()
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

    #[test]
    fn bpm_is_clamped() {
        let mut p = vjc_model::Project::new("t");
        super::apply(&mut p, super::Command::SetBpm { bpm: 5000.0 }).unwrap();
        assert_eq!(p.composition.tempo.bpm, clock::MAX_BPM);
        super::apply(&mut p, super::Command::SetBpm { bpm: 1.0 }).unwrap();
        assert_eq!(p.composition.tempo.bpm, clock::MIN_BPM);
    }

    use super::*;

    const A: DeckId = Uuid::from_u128(1);
    use vjc_model::{ShortcutMode, Transition};

    pub(crate) fn layer(n: usize) -> Layer {
        Layer {
            color: None,
            folded: false,
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
            audio: true,
            video: true,
            trigger_mode: vjc_model::TriggerMode::Restart,
            ignore_column_trigger: false,
            lock_content: false,
            fader_start: false,
            extras: Default::default(),
            mask_mode: Default::default(),
            group: None,
        }
    }

    pub(super) fn setup() -> (Project, LayerId) {
        let mut p = Project::new("t");
        p.composition.decks[0].id = A;
        p.composition.current_deck = A;
        let l = layer(1);
        let id = l.id;
        p.composition.layers.push(l);
        p.composition.columns.push(Column {
            color: None,
            autopilot: Default::default(),
            beat_snap: None,
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
    pub(crate) fn roundtrip(p: &mut Project, cmd: Command) {
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
    fn empty_names_rejected() {
        let mut p = Project::new("t");
        let mut h = History::new();
        let d = p.composition.decks[0].id;
        let r = h.exec(
            &mut p,
            Command::RenameDeck {
                deck: d,
                name: "  ".into(),
            },
            Instant::now(),
        );
        assert_eq!(r, Err(Error::InvalidValue));
        assert_eq!(p.composition.decks[0].name, "Deck");
    }

    #[test]
    fn rename_column_is_undoable_and_rejects_empty() {
        let mut p = Project::new("t");
        let mut h = History::new();
        let id = Uuid::new_v4();
        p.composition.columns.push(Column {
            color: None,
            autopilot: Default::default(),
            beat_snap: None,
            id,
            name: "1".into(),
        });
        let rename = |name: &str| Command::RenameColumn {
            column: id,
            name: name.into(),
        };
        h.exec(&mut p, rename("Drop"), Instant::now()).unwrap();
        assert_eq!(p.composition.columns.last().unwrap().name, "Drop");
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.columns.last().unwrap().name, "1");
        // negative control: blank and unknown ids change nothing
        assert_eq!(
            h.exec(&mut p, rename("  "), Instant::now()),
            Err(Error::InvalidValue)
        );
        let bad = Command::RenameColumn {
            column: Uuid::new_v4(),
            name: "x".into(),
        };
        assert_eq!(apply(&mut p, bad), Err(Error::NoColumn));
        assert_eq!(p.composition.columns.last().unwrap().name, "1");
    }

    #[test]
    fn empty_names_rejected_by_apply_too() {
        let mut p = Project::new("t");
        let d = p.composition.decks[0].id;
        let c = Command::RenameDeck {
            deck: d,
            name: " ".into(),
        };
        assert_eq!(apply(&mut p, c), Err(Error::InvalidValue));
        assert_eq!(p.composition.decks[0].name, "Deck");
    }

    #[test]
    fn empty_clip_name_undo_redo_roundtrip() {
        let (mut p, l) = setup();
        let mut h = History::new();
        h.exec(
            &mut p,
            Command::ImportMedia {
                deck: A,
                layer: l,
                column: 0,
                path: "/".into(),
            },
            Instant::now(),
        )
        .unwrap();
        assert_eq!(clip(&p, l).unwrap().name, "");
        let rename = |name: &str| Command::RenameClip {
            deck: A,
            layer: l,
            column: 0,
            name: name.into(),
        };
        let t = Instant::now() + std::time::Duration::from_secs(10);
        h.exec(&mut p, rename("a"), t).unwrap();
        assert!(h.undo(&mut p).unwrap().is_ok());
        assert_eq!(clip(&p, l).unwrap().name, "");
        assert!(h.redo(&mut p).unwrap().is_ok());
        assert_eq!(clip(&p, l).unwrap().name, "a");
        assert!(h.exec(&mut p, rename(" "), t).is_err());
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
    fn trigger_network_source_starts_it() {
        let (mut p, l) = setup();
        let mut c = clip_from_path("/x/a.mov".into());
        c.source = Source::Network {
            protocol: vjc_model::NetProtocol::Ndi,
            name: "feed".into(),
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
        assert!(matches!(&a.events[0], Event::NetworkStarted { name, .. } if name == "feed"));
    }

    #[test]
    fn trigger_non_file_source_fails() {
        let (mut p, l) = setup();
        let mut c = clip_from_path("/x/a.mov".into());
        c.source = Source::Network {
            protocol: vjc_model::NetProtocol::Spout,
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
    fn trigger_start_follows_clip_mode_then_layer_default() {
        use vjc_model::TriggerMode as T;
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let go = |p: &Project| trigger_start(p, A, l, 0, Some(0.7), Some(0.25));
        // Clip defers, layer default is Restart: start at the beginning (None).
        assert_eq!(go(&p), None);
        p.composition.layers[0].trigger_mode = T::Continue;
        assert_eq!(go(&p), Some(0.7));
        p.composition.layers[0].trigger_mode = T::Relative;
        assert_eq!(go(&p), Some(0.25));
        // Negative control: the clip's own mode beats the layer default.
        p.composition.decks[0].clips[0].clip.transport.trigger_mode = T::Restart;
        assert_eq!(go(&p), None);
        // Region applies to Relative: 0.5 of 0.0..0.5 is 0.25.
        let t = &mut p.composition.decks[0].clips[0].clip.transport;
        t.trigger_mode = T::Relative;
        t.out_point = 0.5;
        assert_eq!(trigger_start(&p, A, l, 0, None, Some(0.5)), Some(0.25));
        assert_eq!(trigger_start(&p, A, l, 3, None, Some(0.5)), None);
    }

    #[test]
    fn trigger_mode_and_blend_override_roundtrip() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let clip = |p: &Project| p.composition.decks[0].clips[0].clip.clone();
        assert_eq!(
            clip(&p).transport.trigger_mode,
            vjc_model::TriggerMode::Layer
        );
        roundtrip(
            &mut p,
            Command::SetTriggerMode {
                deck: A,
                layer: l,
                column: 0,
                mode: vjc_model::TriggerMode::Continue,
            },
        );
        assert_eq!(
            clip(&p).transport.trigger_mode,
            vjc_model::TriggerMode::Continue
        );
        roundtrip(
            &mut p,
            Command::SetBlendOverride {
                deck: A,
                layer: l,
                column: 0,
                blend: Some(BlendMode::Add),
            },
        );
        assert_eq!(clip(&p).blend_override, Some(BlendMode::Add));
        roundtrip(
            &mut p,
            Command::SetLayerTriggerMode {
                layer: l,
                mode: vjc_model::TriggerMode::Relative,
            },
        );
        assert_eq!(
            p.composition.layers[0].trigger_mode,
            vjc_model::TriggerMode::Relative
        );
        // Negative control: a missing clip is an error, not a silent no-op.
        assert!(
            apply(
                &mut p,
                Command::SetBlendOverride {
                    deck: A,
                    layer: l,
                    column: 5,
                    blend: None,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn clip_transition_command_roundtrips_and_rejects_bad_input() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let t = vjc_model::Transition {
            kind: "Cube".into(),
            duration_beats: None,
            duration_secs: 0.5,
        };
        roundtrip(
            &mut p,
            Command::SetClipTransition {
                deck: A,
                layer: l,
                column: 0,
                transition: Some(t.clone()),
            },
        );
        assert_eq!(p.composition.decks[0].clips[0].clip.transition, Some(t));
        let bad = |deck_col: u32, secs: f64| {
            let mut q = p.clone();
            apply(
                &mut q,
                Command::SetClipTransition {
                    deck: A,
                    layer: l,
                    column: deck_col,
                    transition: Some(vjc_model::Transition {
                        kind: "Cube".into(),
                        duration_beats: None,
                        duration_secs: secs,
                    }),
                },
            )
            .is_err()
        };
        assert!(bad(5, 1.0), "missing clip");
        assert!(bad(0, f64::NAN), "non-finite duration");
        assert!(!bad(0, 1.0), "control: valid input passes");
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
        // Pause last: leaving Pause by hand overwrites the remembered direction.
        for direction in [Direction::Backward, Direction::Random, Direction::Pause] {
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
    fn audio_sets_clamps_coalesces_and_undoes() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        for target in [
            AudioTarget::Master,
            AudioTarget::Layer(l),
            AudioTarget::Clip(A, l, 0),
        ] {
            let set = |param, value| Command::SetAudio {
                target,
                param,
                value,
            };
            roundtrip(&mut p, set(AudioParam::Volume, 0.25));
            roundtrip(&mut p, set(AudioParam::Pan, -0.5));
            assert_eq!(
                apply(&mut p, set(AudioParam::Pan, f64::NAN)),
                Err(Error::InvalidValue)
            );
            apply(&mut p, set(AudioParam::Volume, 9.0)).unwrap();
            let vol = |p: &Project| match target {
                AudioTarget::Master => p.composition.master.volume.clone(),
                AudioTarget::Layer(_) => p.composition.layers[0].track.volume.clone(),
                AudioTarget::Clip(..) => clip(p, l).unwrap().track.volume,
            };
            assert_eq!(vol(&p).value, Value::Number(1.0));
        }
        let mut h = History::new();
        let set = |v| Command::SetAudio {
            target: AudioTarget::Layer(l),
            param: AudioParam::Volume,
            value: v,
        };
        h.exec(&mut p, set(0.5), t).unwrap();
        h.exec(&mut p, set(0.2), t).unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(
            p.composition.layers[0].track.volume.value,
            Value::Number(1.0)
        );
        let bad = Command::SetAudio {
            target: AudioTarget::Clip(A, l, 3),
            param: AudioParam::Pan,
            value: 0.0,
        };
        assert!(apply(&mut p, bad).is_err());
    }

    #[test]
    fn mixer_commands_set_and_undo() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        // The first crossfader command creates the crossfader (it stays after an undo).
        apply(&mut p, Command::SetCrossfader { value: 0.5 }).unwrap();
        roundtrip(
            &mut p,
            Command::SetCrossfaderBehaviour {
                behaviour: vjc_model::CrossfadeBehaviour::Cut,
            },
        );
        assert_eq!(
            p.composition.crossfader.as_ref().unwrap().behaviour,
            vjc_model::CrossfadeBehaviour::Cut
        );
        roundtrip(
            &mut p,
            Command::SetCrossfaderBlend {
                blend: Some(BlendMode::Add),
            },
        );
        roundtrip(
            &mut p,
            Command::SetLayerAudio {
                layer: l,
                enabled: false,
            },
        );
        assert!(!p.composition.layers[0].audio);
        roundtrip(
            &mut p,
            Command::SetLayerVideo {
                layer: l,
                enabled: false,
            },
        );
        assert!(!p.composition.layers[0].video);
        let channels = vjc_model::Channels {
            g: false,
            ..Default::default()
        };
        roundtrip(
            &mut p,
            Command::SetClipChannels {
                deck: A,
                layer: l,
                column: 0,
                channels,
            },
        );
        assert_eq!(clip(&p, l).unwrap().channels, channels);
        let missing = Command::SetClipChannels {
            deck: A,
            layer: l,
            column: 3,
            channels,
        };
        assert!(apply(&mut p, missing).is_err());
    }

    #[test]
    fn transform_sets_clamps_coalesces_animates_and_undoes() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let targets = [
            EffectTarget::Master,
            EffectTarget::Layer(l),
            EffectTarget::Clip {
                deck: A,
                layer: l,
                column: 0,
            },
        ];
        let read = |p: &mut Project, target, field| {
            let tr = effects::track_mut(p, target).unwrap();
            number(&tr.transform.field(field).value)
        };
        for target in targets {
            let set = |field, value| Command::SetTransform {
                target,
                field,
                value,
            };
            roundtrip(&mut p, set(TransformField::X, 120.0));
            roundtrip(&mut p, set(TransformField::Rotation, 45.0));
            assert_eq!(
                apply(&mut p, set(TransformField::Y, f64::NAN)),
                Err(Error::InvalidValue)
            );
            apply(&mut p, set(TransformField::Scale, 99.0)).unwrap();
            assert_eq!(
                read(&mut p, target, TransformField::Scale),
                vjc_model::TRANSFORM_SCALE_MAX
            );
            apply(
                &mut p,
                Command::SetTransformMulti {
                    target,
                    values: vec![(TransformField::Scale, 99.0), (TransformField::X, 5.0)],
                },
            )
            .unwrap();
            assert_eq!(
                read(&mut p, target, TransformField::Scale),
                vjc_model::TRANSFORM_SCALE_MAX,
                "multi clamps like SetTransform"
            );
            assert_eq!(read(&mut p, target, TransformField::X), 5.0);
            let anim = |a| Command::SetAnimation {
                target: AnimTarget::Transform {
                    target,
                    field: TransformField::X,
                },
                animation: a,
            };
            roundtrip(
                &mut p,
                anim(Some(vjc_model::Animation::Beats {
                    motion: Default::default(),
                    beats: 4.0,
                    curve: vjc_model::Curve::Sine,
                })),
            );
        }
        let bad = Command::SetTransform {
            target: EffectTarget::Clip {
                deck: A,
                layer: l,
                column: 3,
            },
            field: TransformField::X,
            value: 0.0,
        };
        assert!(apply(&mut p, bad).is_err());
        // A drag is one undo step.
        let set = |v| Command::SetTransform {
            target: EffectTarget::Layer(l),
            field: TransformField::Y,
            value: v,
        };
        let mut h = History::new();
        h.exec(&mut p, set(10.0), t).unwrap();
        h.exec(&mut p, set(30.0), t).unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(read(&mut p, EffectTarget::Layer(l), TransformField::Y), 0.0);
    }

    #[test]
    fn in_out_validates_coalesces_and_undoes() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let before = p.clone();
        let set = |a, b| Command::SetInOut {
            deck: A,
            layer: l,
            column: 0,
            in_point: a,
            out_point: b,
        };
        let region = |p: &Project| {
            let t = clip(p, l).unwrap().transport;
            (t.in_point, t.out_point)
        };
        for (a, b) in [
            (0.6, 0.2),
            (0.5, 0.5),
            (-0.1, 0.5),
            (0.1, 1.2),
            (f64::NAN, 0.5),
            (0.1, f64::INFINITY),
        ] {
            assert_eq!(
                apply(&mut p, set(a, b)),
                Err(Error::InvalidValue),
                "{a} {b}"
            );
        }
        assert_eq!(region(&p), (0.0, 1.0));
        // A marker drag is one undo step.
        h.exec(&mut p, set(0.1, 1.0), t).unwrap();
        h.exec(&mut p, set(0.2, 1.0), t).unwrap();
        h.exec(&mut p, set(0.2, 0.7), t).unwrap();
        assert_eq!(region(&p), (0.2, 0.7));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(region(&p), (0.2, 0.7));
        let missing = Command::SetInOut {
            deck: A,
            layer: l,
            column: 3,
            in_point: 0.1,
            out_point: 0.5,
        };
        assert!(apply(&mut p, missing).is_err());
    }

    #[test]
    fn clip_sync_switches_mode_in_one_undo_step() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let before = p.clone();
        let sync = |beat_sync, timecode_start| Command::SetClipSync {
            deck: A,
            layer: l,
            column: 0,
            beat_sync,
            timecode_start,
        };
        let tc = Timecode::new(0, 1, 0, 0, vjc_model::TcFps::F25);
        h.exec(&mut p, sync(Some(4.0), None), t).unwrap();
        assert_eq!(clip(&p, l).unwrap().transport.beat_sync, Some(4.0));
        h.exec(&mut p, sync(None, Some(tc)), t).unwrap();
        let tr = clip(&p, l).unwrap().transport;
        assert_eq!((tr.beat_sync, tr.timecode_start), (None, Some(tc)));
        h.undo(&mut p).unwrap().unwrap();
        let tr = clip(&p, l).unwrap().transport;
        assert_eq!((tr.beat_sync, tr.timecode_start), (Some(4.0), None));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        // Both at once, a non positive beat count and a bad timecode are refused.
        assert_eq!(
            apply(&mut p, sync(Some(4.0), Some(tc))),
            Err(Error::InvalidValue)
        );
        assert_eq!(
            apply(&mut p, sync(Some(0.0), None)),
            Err(Error::InvalidValue)
        );
        let bad = Timecode::new(0, 99, 0, 0, vjc_model::TcFps::F25);
        assert_eq!(
            apply(&mut p, sync(None, Some(bad))),
            Err(Error::InvalidValue)
        );
    }

    #[test]
    fn seek_clip_is_a_runtime_event() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let seek = |pos| Command::SeekClip {
            deck: A,
            layer: l,
            column: 0,
            pos,
        };
        let before = p.clone();
        let a = apply(&mut p, seek(1.7)).unwrap();
        assert_eq!(a.inverse, None);
        assert!(matches!(a.events[..], [Event::Seek { pos, .. }] if pos == 1.0));
        assert_eq!(apply(&mut p, seek(f64::NAN)), Err(Error::InvalidValue));
        assert_eq!(p, before);
    }

    #[test]
    fn pause_toggles_direction_and_undoes() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let dir = |p: &Project| clip(p, l).unwrap().transport.direction;
        let cmd = |paused| Command::SetPaused {
            clips: vec![(A, l, 0)],
            paused,
        };
        assert_eq!(apply(&mut p, cmd(false)), Err(Error::InvalidValue));
        h.exec(&mut p, cmd(true), t).unwrap();
        assert_eq!(dir(&p), Direction::Pause);
        assert_eq!(apply(&mut p, cmd(true)), Err(Error::InvalidValue));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(dir(&p), Direction::Forward);
        h.redo(&mut p).unwrap().unwrap();
        h.exec(&mut p, cmd(false), t).unwrap();
        assert_eq!(dir(&p), Direction::Forward);
        assert!(
            apply(
                &mut p,
                Command::SetPaused {
                    clips: vec![(A, l, 5)],
                    paused: true
                }
            )
            .is_err()
        );
    }

    #[test]
    fn resume_restores_the_direction_before_the_pause() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let dir = |p: &Project| clip(p, l).unwrap().transport.direction;
        for d in [Direction::Backward, Direction::Random] {
            let set = Command::SetDirection {
                deck: A,
                layer: l,
                column: 0,
                direction: d,
            };
            h.exec(&mut p, set, t).unwrap();
            let cmd = |paused| Command::SetPaused {
                clips: vec![(A, l, 0)],
                paused,
            };
            h.exec(&mut p, cmd(true), t).unwrap();
            assert_eq!(dir(&p), Direction::Pause);
            h.exec(&mut p, cmd(false), t).unwrap();
            assert_eq!(dir(&p), d);
        }
    }

    #[test]
    fn pause_with_a_missing_clip_changes_nothing() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t = Instant::now();
        import(&mut p, &mut h, l, t);
        let r = apply(
            &mut p,
            Command::SetPaused {
                clips: vec![(A, l, 0), (A, l, 5)],
                paused: true,
            },
        );
        assert!(r.is_err());
        assert_eq!(clip(&p, l).unwrap().transport.direction, Direction::Forward);
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
    fn timecode_start_and_cues_roundtrip_and_validate() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let tc = Timecode::new(1, 0, 0, 0, vjc_model::TcFps::F25);
        let set = |start| Command::SetTimecodeStart {
            deck: A,
            layer: l,
            column: 0,
            start,
        };
        roundtrip(&mut p, set(Some(tc)));
        let bad = Timecode::new(0, 0, 0, 25, vjc_model::TcFps::F25);
        assert_eq!(apply(&mut p, set(Some(bad))), Err(Error::InvalidValue));
        let cue = TimecodeCue {
            time: tc,
            column: 1,
            layer: Some(l),
        };
        let cues = |c: &TimecodeCue| Command::SetTimecodeCues {
            cues: vec![c.clone()],
        };
        roundtrip(&mut p, cues(&cue));
        let mut wrong = cue.clone();
        wrong.time = bad;
        assert_eq!(apply(&mut p, cues(&wrong)), Err(Error::InvalidValue));
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
                    color: None,
                    autopilot: Default::default(),
                    beat_snap: None,
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
    fn remove_layer_with_clips_undo_restores_layer_clips_and_effects_in_all_decks() {
        let (mut p, l) = setup();
        p.composition.layers.push(layer(9));
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let b = Deck {
            id: Uuid::new_v4(),
            name: "B".into(),
            clips: vec![],
        };
        let b_id = b.id;
        apply(
            &mut p,
            Command::AddDeck {
                deck: b,
                index: 1,
                select: None,
            },
        )
        .unwrap();
        let mut k = clip_from_path("/x/b.mov".into());
        k.track.effects.push(fx("blur"));
        p.composition
            .decks
            .iter_mut()
            .find(|d| d.id == b_id)
            .unwrap()
            .clips
            .push(CellClip {
                layer: l,
                column: 0,
                clip: k,
            });
        let before = p.clone();
        let mut h = History::new();
        h.exec(
            &mut p,
            Command::RemoveLayerWithClips { layer: l },
            Instant::now(),
        )
        .unwrap();
        assert!(p.composition.layers.iter().all(|x| x.id != l));
        assert!(
            p.composition
                .decks
                .iter()
                .all(|d| d.clips.iter().all(|c| c.layer != l))
        );
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before, "layer, clips and effects restored in every deck");
        h.redo(&mut p).unwrap().unwrap();
        assert!(p.composition.layers.iter().all(|x| x.id != l));
    }

    fn two_layer_project() -> (Project, LayerId, LayerId) {
        let (mut p, l) = setup();
        let other = layer(7);
        let oid = other.id;
        p.composition.layers.push(other);
        (p, l, oid)
    }

    #[test]
    fn removing_the_last_layer_is_refused() {
        let (mut p, l) = setup();
        assert_eq!(p.composition.layers.len(), 1);
        assert_eq!(
            apply(&mut p, Command::RemoveLayerWithClips { layer: l }),
            Err(Error::InUse)
        );
        assert_eq!(p.composition.layers.len(), 1);
        // Negative control: with a second layer the same command works.
        p.composition.layers.push(layer(2));
        assert!(apply(&mut p, Command::RemoveLayerWithClips { layer: l }).is_ok());
    }

    #[test]
    fn remove_layer_drops_its_shortcuts_renumbers_others_and_undo_restores() {
        use vjc_model::{ControlSource, Shortcut, ShortcutMode};
        let (mut p, l, other) = two_layer_project();
        let sc = |t: &str| Shortcut {
            id: Uuid::new_v4(),
            source: ControlSource::Key { code: "A".into() },
            target: t.into(),
            mode: ShortcutMode::Toggle,
            range: (0.0, 1.0),
            invert: false,
            effect: None,
        };
        let _ = other;
        let la = shortcuts::layer_address(&p, l, "opacity").unwrap();
        let oa = shortcuts::layer_address(&p, other, "opacity").unwrap();
        p.shortcuts = vec![sc(&la), sc(&oa)];
        let before = p.clone();
        let mut h = History::new();
        h.exec(
            &mut p,
            Command::RemoveLayerWithClips { layer: l },
            Instant::now(),
        )
        .unwrap();
        assert_eq!(
            p.shortcuts.len(),
            1,
            "shortcut of the removed layer is gone"
        );
        assert_eq!(
            p.shortcuts[0].target,
            shortcuts::layer_address(&p, other, "opacity").unwrap(),
            "the other layer's shortcut follows its layer"
        );
        assert!(h.undo(&mut p).is_some());
        assert_eq!(p.shortcuts, before.shortcuts);
    }

    #[test]
    fn remove_layer_resets_slices_and_drops_cues_and_undo_brings_them_back() {
        use vjc_model::SliceSource;
        let (mut p, l, other) = two_layer_project();
        let mut scr = mapping::new_screen("S", 1920, 1080);
        let (a, b) = (mapping::new_slice(100, 100), mapping::new_slice(100, 100));
        scr.slices = vec![a, b];
        scr.slices[0].source = SliceSource::Layer(l);
        scr.slices[1].source = SliceSource::Layer(other);
        p.output.screens.push(scr);
        let cue = |layer| TimecodeCue {
            time: Timecode::from_frames(vjc_model::TcFps::F30, 0),
            column: 0,
            layer,
        };
        p.composition.timecode_cues = vec![cue(Some(l)), cue(None), cue(Some(other))];
        let before = p.clone();
        let mut h = History::new();
        h.exec(
            &mut p,
            Command::RemoveLayerWithClips { layer: l },
            Instant::now(),
        )
        .unwrap();
        let sl = &p.output.screens[0].slices;
        assert_eq!(sl[0].source, SliceSource::Composition);
        assert_eq!(
            sl[1].source,
            SliceSource::Layer(other),
            "other layer untouched"
        );
        assert_eq!(
            p.composition.timecode_cues,
            vec![cue(None), cue(Some(other))]
        );
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn remove_layer_refused_while_a_router_clip_reads_it() {
        let (mut p, l, other) = two_layer_project();
        let mut k = clip_from_path("/x/r.mov".into());
        k.source = Source::LayerRouter { layer: l };
        p.composition.decks[0].clips.push(CellClip {
            layer: other,
            column: 0,
            clip: k,
        });
        assert_eq!(
            apply(&mut p, Command::RemoveLayerWithClips { layer: l }),
            Err(Error::InUse)
        );
        // Negative control: nothing routes from `other` to itself, so it can go.
        p.composition.decks[0].clips.clear();
        assert!(apply(&mut p, Command::RemoveLayerWithClips { layer: l }).is_ok());
    }

    #[test]
    fn restore_layer_with_a_missing_deck_changes_nothing() {
        let (mut p, _, other) = two_layer_project();
        let lay = layer(8);
        let before = p.clone();
        let clip = CellClip {
            layer: lay.id,
            column: 0,
            clip: clip_from_path("/x/a.mov".into()),
        };
        let cmd = |deck| Command::RestoreLayer {
            layer: lay.clone(),
            index: 0,
            clips: vec![(deck, 0, clip.clone())],
            slices: vec![],
            cues: vec![],
            shortcuts: vec![],
        };
        assert_eq!(apply(&mut p, cmd(Uuid::new_v4())), Err(Error::NoDeck));
        assert_eq!(p, before);
        // Negative control: with a real deck it applies.
        let d = p.composition.decks[0].id;
        assert!(apply(&mut p, cmd(d)).is_ok());
        let _ = other;
    }

    #[test]
    fn set_link() {
        let (mut p, _) = setup();
        roundtrip(&mut p, Command::SetLink { enabled: true });
        apply(&mut p, Command::SetLink { enabled: true }).unwrap();
        assert!(p.composition.tempo.link_enabled);
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
            blend: Default::default(),
            id: Uuid::new_v4(),
            kind: kind.into(),
            bypass: false,
            dry_wet: 1.0,
            params: vec![vjc_model::Param::number("amount", 1.0, Some((0.0, 1.0)))],
        }
    }

    #[test]
    fn audio_chains_are_separate_ordered_undoable_and_persist() {
        fn strip(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(m) => {
                    m.remove("audio_effects");
                    m.values_mut().for_each(strip);
                }
                serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
                _ => {}
            }
        }
        let (mut p, l) = setup();
        for t in targets(&mut p, l) {
            let at = match t {
                EffectTarget::Master => EffectTarget::MasterAudio,
                EffectTarget::Layer(l) => EffectTarget::LayerAudio(l),
                EffectTarget::Clip {
                    deck,
                    layer,
                    column,
                } => EffectTarget::ClipAudio {
                    deck,
                    layer,
                    column,
                },
                _ => unreachable!(),
            };
            let (a, b) = (fx("audio.gain"), fx("audio.delay"));
            let (ia, ib) = (a.id, b.id);
            let mut h = History::new();
            let now = Instant::now();
            for (e, index) in [(a, 0), (b, 0)] {
                h.exec(
                    &mut p,
                    Command::AddEffect {
                        target: at,
                        effect: e,
                        index,
                    },
                    now,
                )
                .unwrap();
            }
            let order = |p: &Project| -> Vec<EffectId> {
                effects::chain(p, at)
                    .unwrap()
                    .iter()
                    .map(|e| e.id)
                    .collect()
            };
            assert_eq!(order(&p), [ib, ia]);
            assert!(
                effects::chain(&p, t).unwrap().is_empty(),
                "video chain untouched"
            );
            h.exec(
                &mut p,
                Command::MoveEffect {
                    target: at,
                    effect: ib,
                    index: 1,
                },
                now,
            )
            .unwrap();
            assert_eq!(order(&p), [ia, ib]);
            h.exec(
                &mut p,
                Command::SetEffectParam {
                    target: at,
                    effect: ia,
                    param: 0,
                    value: Value::Number(0.25),
                },
                now,
            )
            .unwrap();
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
            let mut old: serde_json::Value = serde_json::from_str(&json).unwrap();
            strip(&mut old);
            let old: Project = serde_json::from_value(old).unwrap();
            assert!(old.composition.master.audio_effects.is_empty());
            h.exec(
                &mut p,
                Command::RemoveEffect {
                    target: at,
                    effect: ia,
                },
                now,
            )
            .unwrap();
            assert_eq!(order(&p), [ib]);
            h.undo(&mut p).unwrap().unwrap();
            assert_eq!(order(&p), [ia, ib]);
            for _ in 0..4 {
                h.undo(&mut p).unwrap().unwrap();
            }
            assert!(effects::chain(&p, at).unwrap().is_empty());
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
                Command::SetEffectDryWet {
                    target: t,
                    effect: ia,
                    dry_wet: 0.25,
                },
            );
            assert_eq!(
                apply(
                    &mut p,
                    Command::SetEffectDryWet {
                        target: t,
                        effect: ia,
                        dry_wet: f32::NAN,
                    }
                )
                .err(),
                Some(Error::InvalidValue)
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
    fn set_effect_params_merges_by_name_clamps_and_undoes() {
        let (mut p, l) = setup();
        let t = EffectTarget::Layer(l);
        let mut e = fx("a");
        e.params
            .push(vjc_model::Param::number("other", 0.5, Some((0.0, 1.0))));
        let id = e.id;
        let mut h = History::new();
        let now = Instant::now();
        h.exec(
            &mut p,
            Command::AddEffect {
                target: t,
                effect: e,
                index: 0,
            },
            now,
        )
        .unwrap();
        let before = p.clone();
        let mut loud = vjc_model::Param::number("amount", 9.0, Some((0.0, 100.0)));
        loud.animation = Some(vjc_model::Animation::Dashboard {
            min: 0.0,
            max: 1.0,
            dial: 1,
            scope: Default::default(),
        });
        let stray = vjc_model::Param::number("nope", 1.0, None);
        let wrong = vjc_model::Param {
            value: Value::Text("x".into()),
            ..vjc_model::Param::number("other", 0.0, None)
        };
        h.exec(
            &mut p,
            Command::SetEffectParams {
                target: t,
                effect: id,
                params: vec![loud, stray, wrong],
            },
            now,
        )
        .unwrap();
        let ps = &effects::chain(&p, t).unwrap()[0].params;
        assert_eq!(
            ps[0].value,
            Value::Number(1.0),
            "clamped to the effect range"
        );
        assert!(ps[0].animation.is_some());
        assert_eq!(ps[1].value, Value::Number(0.5), "wrong kind ignored");
        assert_eq!(ps.len(), 2);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn text_param_sets_undoes_redoes_and_rejects_wrong_type() {
        let (mut p, l) = setup();
        let t = EffectTarget::Layer(l);
        let mut e = fx("text");
        e.params = vec![vjc_model::Param {
            name: "message".into(),
            value: Value::Text("A".into()),
            default: Value::Text("A".into()),
            range: None,
            animation: None,
        }];
        let id = e.id;
        let mut h = History::new();
        let t0 = Instant::now();
        let get = |p: &Project| effects::chain(p, t).unwrap()[0].params[0].value.clone();
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
        h.exec(&mut p, set(Value::Text("HELLO".into())), t0)
            .unwrap();
        assert_eq!(get(&p), Value::Text("HELLO".into()));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(get(&p), Value::Text("A".into()));
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(get(&p), Value::Text("HELLO".into()));
        // Negative controls: wrong type is refused; absurd length is truncated.
        assert_eq!(
            h.exec(&mut p, set(Value::Number(1.0)), t0),
            Err(Error::InvalidValue)
        );
        h.exec(&mut p, set(Value::Text("x".repeat(10_000))), t0)
            .unwrap();
        assert_eq!(get(&p), Value::Text("x".repeat(effects::MAX_TEXT_CHARS)));
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
    pub(crate) const A_DECK: DeckId = A;

    pub(crate) fn grid() -> (Project, LayerId, LayerId) {
        let (mut p, l1) = setup();
        let l2 = layer(2);
        let l2id = l2.id;
        p.composition.layers.push(l2);
        for n in 2..=3 {
            p.composition.columns.push(Column {
                color: None,
                autopilot: Default::default(),
                beat_snap: None,
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
        assert_eq!(
            col_of(&mut p, Some(0), 0),
            0,
            "delta 0 retriggers the current column"
        );
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
        let a = vjc_model::Animation::Dashboard {
            min: 0.0,
            max: 1.0,
            dial: 2,
            scope: Default::default(),
        };
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
    fn envelope_edits_are_undoable_and_dial_info_round_trips() {
        let (mut p, l, _) = grid();
        let mut h = History::new();
        let t = Instant::now();
        let target = AnimTarget::LayerOpacity(l);
        let env = |points: Vec<(f64, f64)>, curves| vjc_model::Animation::Envelope {
            points,
            beats: 4.0,
            motion: Default::default(),
            curves,
            unit: Default::default(),
        };
        let get = |p: &Project| p.composition.layers[0].track.opacity.animation.clone();
        let (mut pts, mut cur) = (vec![(0.0, 0.0), (1.0, 1.0)], vec![]);
        let mut seq = vec![];
        // add, move, delete
        let i = anim::env_insert_curve(&mut pts, &mut cur, 0.5, 0.5).unwrap();
        seq.push(env(pts.clone(), cur.clone()));
        anim::env_move(&mut pts, i, 0.6, 0.9);
        seq.push(env(pts.clone(), cur.clone()));
        anim::env_remove(&mut pts, &mut cur, i);
        seq.push(env(pts.clone(), cur.clone()));
        let mut prev = None;
        for (k, a) in seq.iter().enumerate() {
            h.exec(
                &mut p,
                Command::SetAnimation {
                    target,
                    animation: Some(a.clone()),
                },
                t + COALESCE_WINDOW * 4 * (k as u32 + 1),
            )
            .unwrap();
            assert_eq!(get(&p).as_ref(), Some(a));
            prev = Some(a.clone());
        }
        let _ = prev;
        for k in (0..seq.len() - 1).rev() {
            h.undo(&mut p).unwrap().unwrap();
            assert_eq!(get(&p).as_ref(), Some(&seq[k]));
        }
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(get(&p), None);

        // Dial name and range: value keeps its position; undo restores both.
        h.exec(
            &mut p,
            Command::SetDashboardDial {
                dial: 1,
                value: 0.5,
            },
            t + Duration::from_secs(60),
        )
        .unwrap();
        h.exec(
            &mut p,
            Command::SetDialInfo {
                owner: DialRef::Comp,
                dial: 1,
                name: "Speed".into(),
                range: (10.0, 20.0),
            },
            t + Duration::from_secs(120),
        )
        .unwrap();
        let d = &p.composition.dashboard[1];
        assert_eq!((d.name.as_str(), d.range), ("Speed", Some((10.0, 20.0))));
        assert_eq!(d.value, Value::Number(15.0));
        assert_eq!(anim::dial_norm(d), 0.5);
        h.undo(&mut p).unwrap().unwrap();
        let d = &p.composition.dashboard[1];
        assert_eq!((d.name.as_str(), d.range), ("Dial 2", Some((0.0, 1.0))));
        assert_eq!(d.value, Value::Number(0.5));
        let bad = Command::SetDialInfo {
            owner: DialRef::Comp,
            dial: 1,
            name: " ".into(),
            range: (0.0, 1.0),
        };
        assert_eq!(apply(&mut p, bad).unwrap_err(), Error::InvalidValue);
    }

    #[test]
    fn renamed_dial_with_range_stays_mappable_by_normalised_value() {
        let (mut p, _, _) = grid();
        apply(
            &mut p,
            Command::SetDialInfo {
                owner: DialRef::Comp,
                dial: 2,
                name: "Zoom".into(),
                range: (10.0, 20.0),
            },
        )
        .unwrap();
        // MIDI/OSC send 0..1: the dial lands at the matching point of its range.
        let cmd = shortcuts::resolve(&p, "/comp/dashboard/3", 0.5).unwrap();
        apply(&mut p, cmd).unwrap();
        assert_eq!(p.composition.dashboard[2].value, Value::Number(15.0));
        let ctx = shortcuts::Ctx::new(None, false);
        assert_eq!(shortcuts::current(&p, &ctx, "/comp/dashboard/3"), Some(0.5));
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
                    animation: Some(vjc_model::Animation::Dashboard {
                        min: 0.0,
                        max: 1.0,
                        dial: d,
                        scope: Default::default(),
                    }),
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

    fn fx_project() -> (Project, EffectId, EffectId, EffectId) {
        let (mut p, l1, l2) = grid();
        let mk = |kind: &str| Effect {
            blend: Default::default(),
            id: Uuid::new_v4(),
            kind: kind.into(),
            bypass: false,
            dry_wet: 1.0,
            params: vec![
                vjc_model::Param::number("amt", 5.0, Some((0.0, 10.0))),
                vjc_model::Param {
                    name: "on".into(),
                    value: Value::Bool(false),
                    default: Value::Bool(false),
                    range: None,
                    animation: None,
                },
                vjc_model::Param {
                    name: "mode".into(),
                    value: Value::Choice {
                        selected: 0,
                        options: vec!["a".into(), "b".into(), "c".into()],
                    },
                    default: Value::Choice {
                        selected: 0,
                        options: vec!["a".into(), "b".into(), "c".into()],
                    },
                    range: None,
                    animation: None,
                },
            ],
        };
        let (m, l, c) = (mk("m"), mk("l"), mk("c"));
        let ids = (m.id, l.id, c.id);
        p.composition.master.effects.push(m);
        // Layer 1 (bottom) is l2.
        let li = p
            .composition
            .layers
            .iter()
            .position(|x| x.id == l2)
            .unwrap();
        p.composition.layers[li].track.effects.push(l);
        let _ = l1;
        let d = &mut p.composition.decks[0];
        d.clips
            .iter_mut()
            .find(|k| k.layer == l2 && k.column == 0)
            .unwrap()
            .clip
            .track
            .effects
            .push(c);
        (p, ids.0, ids.1, ids.2)
    }

    #[test]
    fn deck_addresses_select_and_step() {
        let (mut p, _, _, _) = fx_project();
        let first = p.composition.decks[0].id;
        for n in ["B", "C"] {
            p.composition.decks.push(Deck {
                id: Uuid::new_v4(),
                name: n.into(),
                clips: vec![],
            });
        }
        let ids: Vec<_> = p.composition.decks.iter().map(|d| d.id).collect();
        let sel = |id| Some(Command::SelectDeck { deck: id });
        assert_eq!(sc::resolve(&p, "/comp/deck/2/select", 1.0), sel(ids[1]));
        // Negative controls: released button, out of range, zero.
        assert_eq!(sc::resolve(&p, "/comp/deck/2/select", 0.0), None);
        assert_eq!(sc::resolve(&p, "/comp/deck/4/select", 1.0), None);
        assert_eq!(sc::resolve(&p, "/comp/deck/0/select", 1.0), None);
        assert_eq!(p.composition.current_deck, first);
        assert_eq!(sc::resolve(&p, "/comp/deck/next", 1.0), sel(ids[1]));
        assert_eq!(sc::resolve(&p, "/comp/deck/prev", 1.0), None);
        p.composition.current_deck = ids[2];
        assert_eq!(sc::resolve(&p, "/comp/deck/next", 1.0), None);
        assert_eq!(sc::resolve(&p, "/comp/deck/prev", 1.0), sel(ids[1]));
    }

    #[test]
    fn duplicate_deck_has_fresh_ids_and_same_content() {
        let (p, _, _, _) = fx_project();
        let src = p.composition.decks[0].clone();
        assert!(!src.clips.is_empty());
        let d = duplicate_deck(&p, src.id).unwrap();
        assert_ne!(d.id, src.id);
        assert_eq!(d.clips.len(), src.clips.len());
        for (a, b) in d.clips.iter().zip(&src.clips) {
            assert_ne!(a.clip.id, b.clip.id);
            assert_eq!((a.layer, a.column), (b.layer, b.column));
            assert_eq!(a.clip.source, b.clip.source);
        }
        assert!(duplicate_deck(&p, Uuid::new_v4()).is_none());
    }

    #[test]
    fn pause_addresses_toggle_active_clip() {
        let (mut p, _, _, _) = fx_project();
        let l2 = p.composition.layers.last().unwrap().id;
        let ctx = Ctx::new(None, false).with_active(vec![(l2, A, 0)]);
        let mut h = History::new();
        for addr in ["/comp/layer/1/pause", "/comp/pause"] {
            let cmd = sc::resolve_with(&p, &ctx, addr, 1.0).unwrap();
            assert!(
                matches!(&cmd, Command::SetPaused { paused: true, .. }),
                "{addr}"
            );
            h.exec(&mut p, cmd, Instant::now()).unwrap();
            let cmd = sc::resolve_with(&p, &ctx, addr, 1.0).unwrap();
            assert!(
                matches!(&cmd, Command::SetPaused { paused: false, .. }),
                "{addr}"
            );
            h.exec(&mut p, cmd, Instant::now()).unwrap();
            assert_eq!(sc::resolve_with(&p, &ctx, addr, 0.0), None);
        }
        // Nothing playing: no command.
        assert_eq!(sc::resolve(&p, "/comp/pause", 1.0), None);
        assert_eq!(sc::resolve_with(&p, &ctx, "/comp/layer/9/pause", 1.0), None);
    }

    #[test]
    fn effect_addresses_resolve_and_apply() {
        let (mut p, m, l, c) = fx_project();
        let l2 = p.composition.layers.last().unwrap().id;
        let ctx = Ctx::new(None, false).with_active(vec![(l2, A, 0)]);
        // Each scope: param (number, bool, choice), drywet, bypass.
        for (base, id) in [
            ("/comp/effect/1", m),
            ("/comp/layer/1/effect/1", l),
            ("/comp/layer/1/clip/effect/1", c),
        ] {
            let cmd = sc::resolve_with(&p, &ctx, &format!("{base}/param/amt"), 0.25).unwrap();
            assert!(matches!(&cmd, Command::SetEffectParam { effect, param: 0,
                value: Value::Number(v), .. } if *effect == id && (*v - 2.5).abs() < 1e-9));
            let cmd = sc::resolve_with(&p, &ctx, &format!("{base}/param/on"), 1.0).unwrap();
            assert!(matches!(
                cmd,
                Command::SetEffectParam {
                    value: Value::Bool(true),
                    ..
                }
            ));
            let cmd = sc::resolve_with(&p, &ctx, &format!("{base}/param/mode"), 1.0).unwrap();
            assert!(matches!(
                cmd,
                Command::SetEffectParam {
                    value: Value::Choice { selected: 2, .. },
                    ..
                }
            ));
            let cmd = sc::resolve_with(&p, &ctx, &format!("{base}/drywet"), 2.0).unwrap();
            assert!(matches!(cmd, Command::SetEffectDryWet { dry_wet, .. } if dry_wet == 1.0));
            let cmd = sc::resolve_with(&p, &ctx, &format!("{base}/bypass"), 1.0).unwrap();
            assert!(matches!(cmd, Command::SetEffectBypass { bypass: true, .. }));
            assert_eq!(
                sc::current(&p, &ctx, &format!("{base}/param/amt")),
                Some(0.5)
            );
            // Out of range / unknown: None, never a panic.
            for bad in [
                format!("{base}/../x"),
                format!("{base}/param/nope"),
                format!("{base}/param/amt/x"),
                format!("{base}/nope"),
            ] {
                assert!(sc::resolve_with(&p, &ctx, &bad, 1.0).is_none(), "{bad}");
            }
        }
        for bad in [
            "/comp/effect/0/drywet",
            "/comp/effect/2/drywet",
            "/comp/effect/x/drywet",
            "/comp/effect/99999999999999999999/bypass",
            "/comp/layer/0/effect/1/drywet",
            "/comp/layer/3/effect/1/drywet",
            "/comp/layer/1/effect/2/bypass",
            "/comp/layer/1/clip/effect/2/bypass",
            "/comp/layer/2/clip/effect/1/bypass",
            "/comp/layer/9/clip/effect/1/bypass",
        ] {
            assert!(sc::resolve_with(&p, &ctx, bad, 1.0).is_none(), "{bad}");
            assert_eq!(sc::current(&p, &ctx, bad), None, "{bad}");
        }
        // Clip form needs an active clip.
        assert!(sc::resolve(&p, "/comp/layer/1/clip/effect/1/bypass", 1.0).is_none());
        // Existing clip connect form still works.
        assert!(sc::resolve(&p, "/comp/layer/1/clip/3/connect", 1.0).is_some());
        // Apply through history and read back.
        let mut h = History::default();
        let cmd = sc::resolve_with(&p, &ctx, "/comp/layer/1/effect/1/param/amt", 1.0).unwrap();
        h.exec(&mut p, cmd, Instant::now()).unwrap();
        let e = &effects::chain(&p, EffectTarget::Layer(l2)).unwrap()[0];
        assert_eq!(number(&e.params[0].value), 10.0);
    }

    #[test]
    fn osc_and_key_shortcut_drive_effect_param() {
        use crate::osc;
        let (mut p, m, _, _) = fx_project();
        let ctx = ctx(None, false);
        let mut h = History::default();
        let ev = osc::Event {
            address: "/comp/effect/1/param/amt".into(),
            value: Some(0.8),
        };
        let cmd = osc::direct(&p, &ctx, &ev).unwrap();
        h.exec(&mut p, cmd, Instant::now()).unwrap();
        assert!((number(&p.composition.master.effects[0].params[0].value) - 8.0).abs() < 1e-9);
        // Learned key (toggle 0..1) on drywet flips it.
        let s = sc::key_shortcut(
            "D",
            "/comp/effect/1/drywet",
            vjc_model::ShortcutMode::Toggle,
            (0.0, 1.0),
        );
        let cmd = sc::activate(&p, &ctx, &s).unwrap();
        h.exec(&mut p, cmd, Instant::now()).unwrap();
        assert_eq!(p.composition.master.effects[0].dry_wet, 0.0);
        assert_eq!(p.composition.master.effects[0].id, m);
        // A learned CC on a param is absolute over 0..1.
        assert!(sc::is_continuous("/comp/effect/1/param/amt"));
        assert!(sc::is_continuous("/comp/layer/1/effect/2/drywet"));
        assert!(!sc::is_continuous("/comp/effect/1/bypass"));
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
    fn one_monitor_per_screen_in_add_and_undo_remove() {
        let mut p = Project::new("t");
        let on = |name: &str, mon: &str| {
            let mut s = mapping::new_screen(name, 1920, 1080);
            s.device = vjc_model::OutputDevice::Monitor { name: mon.into() };
            s
        };
        let (a, b) = (on("A", "2"), on("B", "3"));
        let (aid, bid) = (a.id, b.id);
        apply(
            &mut p,
            Command::AddScreen {
                screen: a,
                index: 0,
            },
        )
        .unwrap();
        apply(
            &mut p,
            Command::AddScreen {
                screen: b,
                index: 1,
            },
        )
        .unwrap();
        // Adding a third screen on monitor 2 does not duplicate it.
        let c = on("C", "2");
        let cid = c.id;
        apply(
            &mut p,
            Command::AddScreen {
                screen: c,
                index: 2,
            },
        )
        .unwrap();
        let dev = |p: &Project, id| {
            p.output
                .screens
                .iter()
                .find(|s| s.id == id)
                .unwrap()
                .device
                .clone()
        };
        assert_eq!(dev(&p, cid), vjc_model::OutputDevice::Window);
        // Remove A, give its monitor to B-less C, then undo the removal: still one owner.
        let undo = apply(&mut p, Command::RemoveScreen { screen: aid })
            .unwrap()
            .inverse
            .unwrap();
        let mon2 = vjc_model::OutputDevice::Monitor { name: "2".into() };
        apply(
            &mut p,
            Command::SetScreenDevice {
                screen: cid,
                device: mon2.clone(),
            },
        )
        .unwrap();
        apply(&mut p, undo).unwrap();
        let owners = p.output.screens.iter().filter(|s| s.device == mon2).count();
        assert_eq!(owners, 1);
        assert_eq!(
            dev(&p, bid),
            vjc_model::OutputDevice::Monitor { name: "3".into() }
        );
    }

    #[test]
    fn duplicate_monitor_is_rejected_with_its_own_error() {
        let mut p = Project::new("t");
        let mon = vjc_model::OutputDevice::Monitor { name: "1".into() };
        let mut ids = vec![];
        for n in ["A", "B"] {
            let s = mapping::new_screen(n, 100, 100);
            ids.push(s.id);
            p.output.screens.push(s);
        }
        let set = |screen| Command::SetScreenDevice {
            screen,
            device: mon.clone(),
        };
        apply(&mut p, set(ids[0])).unwrap();
        assert_eq!(apply(&mut p, set(ids[1])).unwrap_err(), Error::MonitorInUse);
    }

    #[test]
    fn load_normalizes_shared_monitor() {
        let mut p = Project::new("t");
        for n in ["A", "B"] {
            let mut s = mapping::new_screen(n, 100, 100);
            s.device = vjc_model::OutputDevice::Monitor { name: "1".into() };
            p.output.screens.push(s);
        }
        assert_eq!(p.normalize_screens(), 1);
        assert_eq!(p.output.screens[1].device, vjc_model::OutputDevice::Window);
        assert_eq!(p.normalize_screens(), 0);
    }

    #[test]
    fn set_output_setup_replaces_everything_and_undoes() {
        let (mut p, _) = setup();
        let mut a = mapping::new_screen("A", 1920, 1080);
        a.slices.push(mapping::new_slice(1920, 1080));
        p.output.screens = vec![a];
        let before = p.clone();
        let mut b = mapping::new_screen("B", 800, 600);
        b.slices.push(mapping::new_slice(800, 600));
        b.slices[0].mask = Some(vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]);
        let new = vec![b.clone()];
        let mut h = History::new();
        let now = Instant::now();
        let set = |screens| Command::SetOutputSetup { screens };
        h.exec(&mut p, set(new.clone()), now).unwrap();
        assert_eq!(p.output.screens, new);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(p.output.screens, new);
        // Invalid setups are rejected and leave the project alone.
        let kept = p.clone();
        let mut bad_mesh = b.clone();
        bad_mesh.slices[0].output_mesh.pop();
        let mut bad_size = b.clone();
        bad_size.width = 0;
        let mut dup_slice = b.clone();
        dup_slice.id = Uuid::new_v4();
        dup_slice.slices.push(dup_slice.slices[0].clone());
        let (mut m1, mut m2) = (
            mapping::new_screen("M1", 10, 10),
            mapping::new_screen("M2", 10, 10),
        );
        m1.device = vjc_model::OutputDevice::Monitor { name: "3".into() };
        m2.device = vjc_model::OutputDevice::Monitor { name: "3".into() };
        for (screens, err) in [
            (vec![bad_mesh], Error::InvalidValue),
            (vec![bad_size], Error::InvalidValue),
            (vec![dup_slice], Error::InvalidValue),
            (vec![b.clone(), b.clone()], Error::InvalidValue),
            (vec![m1, m2], Error::MonitorInUse),
        ] {
            assert_eq!(h.exec(&mut p, set(screens), now), Err(err));
        }
        assert_eq!(p, kept);
    }

    #[test]
    fn slice_source_undo_and_routed_sources() {
        use vjc_model::SliceSource;
        let (mut p, _) = setup();
        let mut scr = mapping::new_screen("S", 1920, 1080);
        let (a, b) = (mapping::new_slice(100, 100), mapping::new_slice(100, 100));
        let (sid, aid, bid) = (scr.id, a.id, b.id);
        scr.slices = vec![a, b];
        p.output.screens.push(scr);
        assert_eq!(
            p.output.screens[0].slices[0].source,
            SliceSource::Composition
        );
        assert!(mapping::routed_sources(&p.output.screens).is_empty());
        let (l, g) = (Uuid::new_v4(), Uuid::new_v4());
        let set = |slice, source| Command::SetSliceSource {
            screen: sid,
            slice,
            source,
        };
        roundtrip(&mut p.clone(), set(aid, SliceSource::Layer(l)));
        let mut h = History::new();
        let now = Instant::now();
        h.exec(&mut p, set(aid, SliceSource::Layer(l)), now)
            .unwrap();
        h.exec(&mut p, set(bid, SliceSource::Group(g)), now)
            .unwrap();
        assert_eq!(
            mapping::routed_sources(&p.output.screens),
            vec![SliceSource::Layer(l), SliceSource::Group(g)]
        );
        // Preview and composition need no extra render; the same layer twice counts once.
        h.exec(&mut p, set(bid, SliceSource::Layer(l)), now)
            .unwrap();
        assert_eq!(
            mapping::routed_sources(&p.output.screens),
            vec![SliceSource::Layer(l)]
        );
        h.exec(&mut p, set(bid, SliceSource::Preview), now).unwrap();
        assert_eq!(mapping::routed_sources(&p.output.screens).len(), 1);
        while h.undo(&mut p).is_some() {}
        assert!(mapping::routed_sources(&p.output.screens).is_empty());
        let bad = Command::SetSliceSource {
            screen: sid,
            slice: Uuid::new_v4(),
            source: SliceSource::Preview,
        };
        assert!(h.exec(&mut p, bad, now).is_err());
    }

    #[test]
    fn batch_edits_many_slices_as_one_undo_step_and_is_all_or_nothing() {
        let (mut p, _) = setup();
        let mut scr = mapping::new_screen("A", 1920, 1080);
        let (s1, s2) = (
            mapping::new_slice(1920, 1080),
            mapping::new_slice(1920, 1080),
        );
        let (id1, id2) = (s1.id, s2.id);
        scr.slices = vec![s1, s2];
        let sid = scr.id;
        p.output.screens = vec![scr];
        let before = p.clone();
        let shift = |slice, dx: f32| Command::SetSliceMesh {
            screen: sid,
            slice,
            mesh: before.output.screens[0].slices[0]
                .output_mesh
                .iter()
                .map(|q| (q.0 + dx, q.1))
                .collect(),
            cols: before.output.screens[0].slices[0].mesh_cols,
        };
        let batch = Command::Batch {
            commands: vec![shift(id1, 10.0), shift(id2, 20.0)],
        };
        let mut h = History::new();
        h.exec(&mut p, batch, Instant::now()).unwrap();
        let x = |p: &Project, i: usize| p.output.screens[0].slices[i].output_mesh[0].0;
        assert_eq!((x(&p, 0), x(&p, 1)), (10.0, 20.0));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before, "one undo reverts every slice");
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(x(&p, 1), 20.0);
        // A rejected member leaves the project untouched; an empty batch is invalid.
        let mut q = before.clone();
        let bad = Command::Batch {
            commands: vec![shift(id1, 10.0), shift(Uuid::new_v4(), 5.0)],
        };
        assert!(apply(&mut q, bad).is_err());
        assert_eq!(q, before);
        assert_eq!(
            apply(&mut q, Command::Batch { commands: vec![] }),
            Err(Error::InvalidValue)
        );
    }

    #[test]
    fn move_slice_between_screens_undoes_in_place() {
        let (mut p, _) = setup();
        let (a, b) = (
            mapping::new_screen("A", 1920, 1080),
            mapping::new_screen("B", 640, 480),
        );
        let (aid, bid) = (a.id, b.id);
        p.output.screens = vec![a, b];
        let (s1, s2) = (
            mapping::new_slice(1920, 1080),
            mapping::new_slice(1920, 1080),
        );
        let (id1, id2) = (s1.id, s2.id);
        p.output.screens[0].slices = vec![s1, s2];
        let mv = Command::MoveSlice {
            from: aid,
            to: bid,
            slice: id1,
            index: 0,
        };
        roundtrip(&mut p.clone(), mv.clone());
        let mut h = History::new();
        h.exec(&mut p, mv, Instant::now()).unwrap();
        assert_eq!(p.output.screens[0].slices.len(), 1);
        assert_eq!(p.output.screens[1].slices[0].id, id1);
        // The mesh keeps its pixel positions (still 1920 wide on a 640 screen).
        assert_eq!(p.output.screens[1].slices[0].output_mesh[1].0, 1920.0);
        h.undo(&mut p).unwrap().unwrap();
        let ids: Vec<_> = p.output.screens[0].slices.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![id1, id2], "back at its original index");
        assert!(p.output.screens[1].slices.is_empty());
        // Same screen, unknown screen or unknown slice are rejected without changes.
        let before = p.clone();
        for (from, to, slice) in [
            (aid, aid, id1),
            (aid, Uuid::new_v4(), id1),
            (aid, bid, Uuid::new_v4()),
        ] {
            let c = Command::MoveSlice {
                from,
                to,
                slice,
                index: 0,
            };
            assert!(h.exec(&mut p, c, Instant::now()).is_err());
        }
        assert_eq!(p, before);
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
        {
            let mut q = p.clone();
            let b = Uuid::new_v4();
            let mut s2 = q.output.screens[0].clone();
            s2.id = b;
            q.output.screens.push(s2);
            let r = h.exec(
                &mut q,
                Command::SetScreenDevice {
                    screen: b,
                    device: vjc_model::OutputDevice::Monitor { name: "3".into() },
                },
                now,
            );
            // first screen is not on monitor 3 yet in the clone: assign it, then dup must fail
            assert!(r.is_ok());
            let a = q.output.screens[0].id;
            let r = h.exec(
                &mut q,
                Command::SetScreenDevice {
                    screen: a,
                    device: vjc_model::OutputDevice::Monitor { name: "3".into() },
                },
                now,
            );
            assert!(r.is_err());
        }
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
    fn slice_key_invert_and_screen_delay_undo() {
        let (mut p, sid, lid) = one_screen_slice();
        let before = p.clone();
        let mut h = History::new();
        let now = Instant::now();
        h.exec(
            &mut p,
            Command::SetSliceKey {
                screen: sid,
                slice: lid,
                is_key: true,
                black_bg: true,
            },
            now,
        )
        .unwrap();
        h.exec(
            &mut p,
            Command::SetSliceMaskInvert {
                screen: sid,
                slice: lid,
                invert: true,
            },
            now,
        )
        .unwrap();
        h.exec(
            &mut p,
            Command::SetScreenDelay {
                screen: sid,
                delay_ms: 500,
            },
            now,
        )
        .unwrap();
        let s = &p.output.screens[0];
        assert_eq!(s.delay_ms, 100, "clamped");
        assert!(s.slices[0].is_key && s.slices[0].black_bg && s.slices[0].mask_invert);
        for _ in 0..3 {
            h.undo(&mut p).unwrap().unwrap();
        }
        assert_eq!(p, before);
    }

    #[test]
    fn screen_additive_overlap_sets_undoes_and_rejects_unknown_screens() {
        let (mut p, sid, _) = one_screen_slice();
        let before = p.clone();
        let mut h = History::new();
        let now = Instant::now();
        assert!(!p.output.screens[0].additive_overlap);
        let on = Command::SetScreenAdditive {
            screen: sid,
            on: true,
        };
        h.exec(&mut p, on, now).unwrap();
        assert!(p.output.screens[0].additive_overlap);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        let ghost = Command::SetScreenAdditive {
            screen: uuid::Uuid::new_v4(),
            on: true,
        };
        assert!(h.exec(&mut p, ghost, now).is_err());
    }

    #[test]
    fn slice_color_clamps_validates_and_undoes() {
        let (mut p, sid, lid) = one_screen_slice();
        let before = p.clone();
        let mut h = History::new();
        let now = Instant::now();
        let cmd = |brightness, r| Command::SetSliceColor {
            screen: sid,
            slice: lid,
            color: vjc_model::SliceColor {
                brightness,
                r,
                g: 1.0,
                b: 1.0,
                contrast: 1.0,
            },
        };
        assert!(h.exec(&mut p, cmd(f32::NAN, 1.0), now).is_err());
        assert_eq!(p, before, "rejected edit changes nothing");
        h.exec(&mut p, cmd(5.0, -1.0), now).unwrap();
        let c = p.output.screens[0].slices[0].color;
        assert_eq!((c.brightness, c.r), (2.0, 0.0));
        h.exec(&mut p, cmd(0.5, 0.9), now).unwrap();
        // A drag coalesces into one undo step.
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn slice_mask_and_soft_edge_undo_and_validate() {
        let (mut p, sid, lid) = one_screen_slice();
        let before = p.clone();
        let mut h = History::new();
        let now = Instant::now();
        let tri = vec![(0.0, 0.0), (1.0, 0.0), (0.0, 2.0)];
        let mask = |m| Command::SetSliceMask {
            screen: sid,
            slice: lid,
            mask: m,
        };
        assert!(
            h.exec(&mut p, mask(Some(vec![(0.0, 0.0), (1.0, 1.0)])), now)
                .is_err()
        );
        let nan = vec![(0.0, 0.0), (1.0, f32::NAN), (0.0, 1.0)];
        assert!(h.exec(&mut p, mask(Some(nan)), now).is_err());
        assert_eq!(p, before);
        h.exec(&mut p, mask(Some(tri)), now).unwrap();
        // Out-of-range points are clamped.
        assert_eq!(
            p.output.screens[0].slices[0].mask.as_ref().unwrap()[2],
            (0.0, 1.0)
        );
        let edge = |l, g| Command::SetSliceSoftEdge {
            screen: sid,
            slice: lid,
            soft_edge: Some(vjc_model::SoftEdge {
                left: l,
                right: 0.0,
                top: 0.0,
                bottom: 0.0,
                gamma: g,
            }),
        };
        assert!(h.exec(&mut p, edge(f32::INFINITY, 1.0), now).is_err());
        h.exec(&mut p, edge(0.9, 0.0), now).unwrap();
        let e = p.output.screens[0].slices[0].soft_edge.clone().unwrap();
        assert_eq!((e.left, e.gamma), (0.5, 0.1));
        // Mask and edge are different targets: two undo steps back to the start.
        h.undo(&mut p).unwrap().unwrap();
        assert!(p.output.screens[0].slices[0].soft_edge.is_none());
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap().unwrap();
        assert!(p.output.screens[0].slices[0].mask.is_some());
        // Dragging a slider coalesces into one undo step; None clears.
        let mut h2 = History::new();
        let mid = p.clone();
        for k in 1..=5 {
            h2.exec(&mut p, edge(k as f32 / 20.0, 1.0), now).unwrap();
        }
        h2.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, mid);
        h2.exec(&mut p, mask(None), now).unwrap();
        assert!(p.output.screens[0].slices[0].mask.is_none());
    }

    #[test]
    fn mask_steps_do_not_coalesce_across_shape_changes() {
        let (mut p, sid, lid) = one_screen_slice();
        let mut h = History::new();
        let now = Instant::now();
        let mask = |m| Command::SetSliceMask {
            screen: sid,
            slice: lid,
            mask: m,
        };
        let tri = |x: f32| vec![(x, 0.0), (1.0, 0.0), (0.0, 1.0)];
        let mut quad = tri(0.2);
        quad.push((1.0, 1.0));
        let state = |p: &Project| p.output.screens[0].slices[0].mask.clone();
        h.exec(&mut p, mask(Some(tri(0.0))), now).unwrap(); // create
        h.exec(&mut p, mask(Some(tri(0.1))), now).unwrap(); // drag point 0: own step
        h.exec(&mut p, mask(Some(tri(0.15))), now).unwrap(); // same drag
        h.exec(&mut p, mask(Some(quad.clone())), now).unwrap(); // add point
        h.exec(&mut p, mask(None), now).unwrap(); // clear
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(state(&p), Some(quad));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(state(&p), Some(tri(0.15)));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(
            state(&p),
            Some(tri(0.0)),
            "the drag did not swallow the creation"
        );
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(state(&p), None);
    }

    #[test]
    fn mask_drags_of_different_points_are_separate_undo_steps() {
        let (mut p, sid, lid) = one_screen_slice();
        let mut h = History::new();
        let now = Instant::now();
        let mask = |m: Vec<(f32, f32)>| Command::SetSliceMask {
            screen: sid,
            slice: lid,
            mask: Some(m),
        };
        let state = |p: &Project| p.output.screens[0].slices[0].mask.clone().unwrap();
        let base = vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)];
        h.exec(&mut p, mask(base.clone()), now).unwrap();
        // Two frames dragging point 1, then two dragging point 2.
        let mut m = base.clone();
        m[1] = (0.9, 0.0);
        h.exec(&mut p, mask(m.clone()), now).unwrap();
        m[1] = (0.8, 0.0);
        h.exec(&mut p, mask(m.clone()), now).unwrap();
        let after1 = m.clone();
        m[2] = (0.0, 0.9);
        h.exec(&mut p, mask(m.clone()), now).unwrap();
        m[2] = (0.0, 0.8);
        h.exec(&mut p, mask(m), now).unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(state(&p), after1);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(state(&p), base);
    }

    #[test]
    fn device_type_switch_and_ip_edit_are_separate_undo_steps() {
        use vjc_model::OutputDevice as D;
        let (mut p, sid, _) = one_screen_slice();
        let mut h = History::new();
        let now = Instant::now();
        let art = |ip: &str| D::ArtNet {
            ip: ip.into(),
            universe: 0,
            fixtures: vec![Default::default()],
            fps: 0.0,
            delay_ms: 0,
        };
        let set = |d| Command::SetScreenDevice {
            screen: sid,
            device: d,
        };
        let before = p.output.screens[0].device.clone();
        h.exec(&mut p, set(art("10.0.0.1")), now).unwrap();
        h.exec(&mut p, set(art("10.0.0.2")), now).unwrap(); // same kind: one step
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.output.screens[0].device, before);
        h.exec(&mut p, set(art("10.0.0.1")), now).unwrap();
        h.exec(&mut p, set(D::Syphon), now).unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.output.screens[0].device, art("10.0.0.1"));
    }

    #[test]
    fn screen_pixel_perfect_toggles_with_undo() {
        let (mut p, sid, _) = one_screen_slice();
        let before = p.clone();
        let mut h = History::new();
        let c = Command::SetScreenPixelPerfect {
            screen: sid,
            pixel_perfect: true,
        };
        h.exec(&mut p, c, Instant::now()).unwrap();
        assert!(p.output.screens[0].pixel_perfect);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap().unwrap();
        assert!(p.output.screens[0].pixel_perfect);
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

    /// Two layers x three columns, clips "a" at (l1,0) and "b" at (l2,1).
    fn two_clips() -> (Project, LayerId, LayerId) {
        let (mut p, l1) = setup();
        for n in 2..=3 {
            p.composition.columns.push(Column {
                color: None,
                autopilot: Default::default(),
                beat_snap: None,
                id: Uuid::new_v4(),
                name: n.to_string(),
            });
        }
        let l2 = layer(2);
        let l2id = l2.id;
        p.composition.layers.push(l2);
        let t = Instant::now();
        let mut h = History::default();
        for (l, col, f) in [(l1, 0, "/x/a.mov"), (l2id, 1, "/x/b.mov")] {
            h.exec(
                &mut p,
                Command::ImportMedia {
                    deck: A,
                    layer: l,
                    column: col,
                    path: f.into(),
                },
                t,
            )
            .unwrap();
        }
        (p, l1, l2id)
    }

    fn name_at(p: &Project, l: LayerId, c: u32) -> Option<String> {
        clip_in(p, A, l, c).map(|k| k.name.clone())
    }

    #[test]
    fn move_clip_to_empty_cell_moves_and_undoes() {
        let (mut p, l1, l2) = two_clips();
        let before = p.clone();
        let mut h = History::default();
        let ev = h
            .exec(
                &mut p,
                Command::MoveClip {
                    deck: A,
                    from: (l1, 0),
                    to: (l2, 2),
                    copy: false,
                },
                Instant::now(),
            )
            .unwrap();
        assert_eq!(name_at(&p, l1, 0), None, "source vacated");
        assert_eq!(name_at(&p, l2, 2).as_deref(), Some("a"));
        assert!(matches!(
            &ev[..],
            [Event::ClipsMoved { moves, .. }] if moves == &vec![((l1, 0), (l2, 2))]
        ));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        // Redo moves it again.
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(name_at(&p, l2, 2).as_deref(), Some("a"));
    }

    #[test]
    fn move_clip_onto_occupied_cell_swaps_and_undoes() {
        let (mut p, l1, l2) = two_clips();
        let before = p.clone();
        let mut h = History::default();
        let ev = h
            .exec(
                &mut p,
                Command::MoveClip {
                    deck: A,
                    from: (l1, 0),
                    to: (l2, 1),
                    copy: false,
                },
                Instant::now(),
            )
            .unwrap();
        assert_eq!(name_at(&p, l1, 0).as_deref(), Some("b"));
        assert_eq!(name_at(&p, l2, 1).as_deref(), Some("a"));
        assert!(matches!(
            &ev[..],
            [Event::ClipsMoved { moves, .. }] if moves.len() == 2
        ));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn copy_clip_keeps_source_uses_fresh_ids_and_undoes() {
        let (mut p, l1, l2) = two_clips();
        let before = p.clone();
        let src_id = clip_in(&p, A, l1, 0).unwrap().id;
        let mut h = History::default();
        h.exec(
            &mut p,
            Command::MoveClip {
                deck: A,
                from: (l1, 0),
                to: (l2, 2),
                copy: true,
            },
            Instant::now(),
        )
        .unwrap();
        assert_eq!(name_at(&p, l1, 0).as_deref(), Some("a"), "source stays");
        let copy = clip_in(&p, A, l2, 2).unwrap();
        assert_eq!(copy.name, "a");
        assert_ne!(copy.id, src_id, "the copy has its own id");
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        // Copy over an occupied cell replaces it; undo brings it back.
        h.exec(
            &mut p,
            Command::MoveClip {
                deck: A,
                from: (l1, 0),
                to: (l2, 1),
                copy: true,
            },
            Instant::now(),
        )
        .unwrap();
        assert_eq!(name_at(&p, l2, 1).as_deref(), Some("a"));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn move_clip_rejects_empty_source_and_ignores_same_cell() {
        let (mut p, l1, l2) = two_clips();
        let before = p.clone();
        let mv = |from, to| Command::MoveClip {
            deck: A,
            from,
            to,
            copy: false,
        };
        assert!(matches!(
            apply(&mut p, mv((l1, 2), (l2, 2))),
            Err(Error::NoClip)
        ));
        assert!(matches!(
            apply(&mut p, mv((l1, 0), (l2, 9))),
            Err(Error::NoColumn)
        ));
        let r = apply(&mut p, mv((l1, 0), (l1, 0))).unwrap();
        assert!(r.inverse.is_none() && r.events.is_empty(), "no-op");
        assert_eq!(p, before);
    }

    fn layer_order(p: &Project) -> Vec<LayerId> {
        p.composition.layers.iter().map(|l| l.id).collect()
    }

    #[test]
    fn move_layer_reorders_with_undo_and_clamps() {
        let (mut p, a) = setup();
        let b = layer(2);
        let c = layer(3);
        let (bid, cid) = (b.id, c.id);
        p.composition.layers.extend([b, c]);
        let before = p.clone();
        let mut h = History::default();
        let t = Instant::now();
        h.exec(
            &mut p,
            Command::MoveLayer {
                layer: a,
                to_index: 2,
            },
            t,
        )
        .unwrap();
        assert_eq!(layer_order(&p), vec![bid, cid, a]);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        // Past the end clamps to the bottom.
        h.exec(
            &mut p,
            Command::MoveLayer {
                layer: a,
                to_index: 99,
            },
            t,
        )
        .unwrap();
        assert_eq!(layer_order(&p), vec![bid, cid, a]);
        h.undo(&mut p).unwrap().unwrap();
        // Moving to the current place is not an undo step.
        let r = apply(
            &mut p,
            Command::MoveLayer {
                layer: bid,
                to_index: 1,
            },
        )
        .unwrap();
        assert!(r.inverse.is_none());
        assert_eq!(p, before);
    }

    #[test]
    fn move_layer_never_splits_a_group_nor_moves_a_grouped_layer() {
        let (mut p, a) = setup();
        let (b, c) = (layer(2), layer(3));
        let (bid, cid) = (b.id, c.id);
        p.composition.layers.extend([b, c]);
        let group = vjc_model::LayerGroup::new("g");
        let gid = group.id;
        apply(
            &mut p,
            Command::CreateGroup {
                group,
                layers: vec![bid, cid],
            },
        )
        .unwrap();
        // a is on top, the group below it: landing between b and c goes above the group.
        let r = apply(
            &mut p,
            Command::MoveLayer {
                layer: a,
                to_index: 1,
            },
        );
        assert!(r.is_ok());
        assert_eq!(layer_order(&p), vec![a, bid, cid]);
        let r = apply(
            &mut p,
            Command::MoveLayer {
                layer: a,
                to_index: 2,
            },
        )
        .unwrap();
        assert!(r.inverse.is_some());
        assert_eq!(layer_order(&p), vec![bid, cid, a]);
        assert_eq!(p.composition.layers[0].group, Some(gid));
        assert!(matches!(
            apply(
                &mut p,
                Command::MoveLayer {
                    layer: bid,
                    to_index: 2
                }
            ),
            Err(Error::BadGroup)
        ));
    }

    #[test]
    fn cue_address_jumps_the_playing_clip() {
        let (mut p, l1, _) = grid();
        let mut h = History::new();
        let t = Instant::now();
        for pos in [0.2, 0.7] {
            h.exec(
                &mut p,
                Command::AddCue {
                    deck: A,
                    layer: l1,
                    column: 0,
                    pos,
                },
                t,
            )
            .unwrap();
        }
        // l1 is the second layer from the bottom: "Layer 2".
        let addr = shortcuts::cue_address(&p, l1, 1).unwrap();
        assert_eq!(addr, "/comp/layer/2/clip/cue/2");
        let ctx = shortcuts::Ctx::new(None, false).with_active(vec![(l1, A, 0)]);
        let cmd = shortcuts::resolve_with(&p, &ctx, &addr, 1.0).unwrap();
        assert_eq!(
            cmd,
            Command::JumpToCue {
                deck: A,
                layer: l1,
                column: 0,
                index: 1
            }
        );
        let ev = apply(&mut p, cmd).unwrap().events;
        assert!(matches!(ev[..], [Event::Seek { pos, .. }] if pos == 0.7));
        // Negative controls: release, nothing playing, missing cue, bad numbers.
        assert!(shortcuts::resolve_with(&p, &ctx, &addr, 0.0).is_none());
        let idle = shortcuts::Ctx::new(None, false);
        assert!(shortcuts::resolve_with(&p, &idle, &addr, 1.0).is_none());
        for bad in ["clip/cue/3", "clip/cue/0", "clip/cue/x"] {
            let a = format!("/comp/layer/2/{bad}");
            assert!(shortcuts::resolve_with(&p, &ctx, &a, 1.0).is_none(), "{a}");
        }
        assert!(shortcuts::resolve_with(&p, &ctx, "/comp/layer/1/clip/cue/1", 1.0).is_none());
        // Triggers have no current value, so a shortcut on it fires.
        assert_eq!(shortcuts::current(&p, &ctx, &addr), None);
    }

    #[test]
    fn layer_and_clip_dials_by_address_undo_and_stay_apart() {
        let (mut p, l1, l2) = grid();
        let mut h = History::new();
        let t = Instant::now();
        let ctx = shortcuts::Ctx::new(None, false).with_active(vec![(l1, A, 0)]);
        // l1 is "Layer 2", l2 is "Layer 1".
        let a = shortcuts::dial_address(&p, l1, None, None, 2).unwrap();
        assert_eq!(a, "/comp/layer/2/dashboard/3");
        let cmd = shortcuts::resolve_with(&p, &ctx, &a, 0.75).unwrap();
        assert_eq!(
            cmd,
            Command::SetLayerDial {
                layer: l1,
                dial: 2,
                value: 0.75
            }
        );
        h.exec(&mut p, cmd, t).unwrap();
        let ix = |p: &Project, l| p.composition.layers.iter().position(|x| x.id == l).unwrap();
        assert_eq!(
            p.composition.layers[ix(&p, l1)].track.dials.len(),
            anim::DIALS
        );
        assert_eq!(shortcuts::current(&p, &ctx, &a), Some(0.75));
        // Another layer, the composition and the clip are untouched.
        assert_eq!(
            shortcuts::current(&p, &ctx, "/comp/layer/1/dashboard/3"),
            Some(0.0)
        );
        assert!(p.composition.dashboard.is_empty());
        let ca = "/comp/layer/2/clip/dashboard/3";
        assert_eq!(shortcuts::current(&p, &ctx, ca), Some(0.0));
        let cmd = shortcuts::resolve_with(&p, &ctx, ca, 2.0).unwrap();
        assert!(matches!(cmd, Command::SetClipDial { dial: 2, value, .. } if value == 1.0));
        h.exec(&mut p, cmd, t + Duration::from_secs(1)).unwrap();
        assert_eq!(shortcuts::current(&p, &ctx, ca), Some(1.0));
        assert_eq!(shortcuts::current(&p, &ctx, &a), Some(0.75));
        assert_eq!(
            clip_in(&p, A, l1, 0).unwrap().track.dials.len(),
            anim::DIALS
        );
        // Undo walks back one by one.
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(shortcuts::current(&p, &ctx, ca), Some(0.0));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(shortcuts::current(&p, &ctx, &a), Some(0.0));
        // Negative controls: out of range dial, no playing clip, unknown layer, NaN.
        for bad in [
            "/comp/layer/2/dashboard/9",
            "/comp/layer/2/dashboard/0",
            "/comp/layer/9/dashboard/1",
        ] {
            assert!(
                shortcuts::resolve_with(&p, &ctx, bad, 1.0).is_none(),
                "{bad}"
            );
        }
        let idle = shortcuts::Ctx::new(None, false);
        assert!(shortcuts::resolve_with(&p, &idle, ca, 1.0).is_none());
        assert!(shortcuts::dial_address(&p, l1, Some((A, 0)), None, 0).is_none());
        assert_eq!(
            apply(
                &mut p,
                Command::SetLayerDial {
                    layer: l2,
                    dial: 0,
                    value: f64::NAN
                }
            )
            .unwrap_err(),
            Error::InvalidValue
        );
        assert_eq!(
            apply(
                &mut p,
                Command::SetClipDial {
                    deck: A,
                    layer: l2,
                    column: 1,
                    dial: 0,
                    value: 0.5
                }
            )
            .unwrap_err(),
            Error::NoClip
        );
    }

    #[test]
    fn layer_dial_drag_is_one_undo_step() {
        let (mut p, l1, _) = grid();
        let mut h = History::new();
        let t = Instant::now();
        for (i, v) in [0.1, 0.2, 0.3].into_iter().enumerate() {
            h.exec(
                &mut p,
                Command::SetLayerDial {
                    layer: l1,
                    dial: 0,
                    value: v,
                },
                t + Duration::from_millis(16 * i as u64),
            )
            .unwrap();
        }
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.layers[0].track.dials[0].value), 0.0);
    }

    fn gen_cell(p: &mut Project, layer: LayerId, column: u32) {
        apply(
            p,
            Command::SetGenerator {
                deck: A,
                layer,
                column,
                kind: "solid_color".into(),
                name: "Solid".into(),
                params: vec![],
            },
        )
        .unwrap();
    }

    fn three_layers() -> (Project, [LayerId; 3]) {
        let (mut p, l1) = setup();
        let (b, c) = (layer(2), layer(3));
        let ids = [l1, b.id, c.id];
        p.composition.layers.extend([b, c]);
        (p, ids)
    }

    fn trig(layer: LayerId) -> Command {
        Command::TriggerClip {
            deck: A,
            layer,
            column: 0,
        }
    }

    #[test]
    fn toggle_second_trigger_ejects_and_normal_does_not() {
        let (mut p, [l1, ..]) = three_layers();
        gen_cell(&mut p, l1, 0);
        p.composition.trigger_style = vjc_model::TriggerStyle::Toggle;
        let mut ctx = TriggerCtx::default();
        // Nothing playing: the first trigger plays.
        assert_eq!(resolve_trigger(&p, &ctx, trig(l1)), trig(l1));
        ctx.playing.push((l1, A, l1, 0));
        assert_eq!(
            resolve_trigger(&p, &ctx, trig(l1)),
            Command::ClearLayer { layer: l1 }
        );
        // Control: a Normal clip (own override beats the composition) plays again.
        p.composition.decks[0].clips[0].clip.trigger_style = vjc_model::TriggerStyle::Normal;
        assert_eq!(resolve_trigger(&p, &ctx, trig(l1)), trig(l1));
    }

    #[test]
    fn clip_target_own_active_and_free() {
        let (mut p, [l1, l2, l3]) = three_layers();
        gen_cell(&mut p, l1, 0);
        let on = |l: LayerId| Command::TriggerClipOn {
            deck: A,
            layer: l1,
            column: 0,
            onto: l,
        };
        let mut ctx = TriggerCtx {
            selected: Some(l2),
            ..Default::default()
        };
        // Own (the default): the command is untouched, whatever is selected.
        assert_eq!(resolve_trigger(&p, &ctx, trig(l1)), trig(l1));
        // Active: plays on the selected layer.
        p.composition.clip_target = vjc_model::ClipTarget::Active;
        assert_eq!(resolve_trigger(&p, &ctx, trig(l1)), on(l2));
        // Control: nothing selected falls back to the clip's own layer.
        ctx.selected = None;
        assert_eq!(resolve_trigger(&p, &ctx, trig(l1)), trig(l1));
        // Free: the lowest layer with nothing in the air.
        p.composition.clip_target = vjc_model::ClipTarget::Free;
        assert_eq!(resolve_trigger(&p, &ctx, trig(l1)), on(l3));
        ctx.playing.push((l3, A, l3, 7));
        assert_eq!(resolve_trigger(&p, &ctx, trig(l1)), on(l2));
        // Control: the clip's own override beats the composition.
        p.composition.decks[0].clips[0].clip.target = vjc_model::ClipTarget::Own;
        assert_eq!(resolve_trigger(&p, &ctx, trig(l1)), trig(l1));
    }

    #[test]
    fn trigger_on_aims_the_event_at_the_target_layer() {
        let (mut p, [l1, l2, _]) = three_layers();
        gen_cell(&mut p, l1, 0);
        let ev = apply(
            &mut p,
            Command::TriggerClipOn {
                deck: A,
                layer: l1,
                column: 0,
                onto: l2,
            },
        )
        .unwrap()
        .events;
        assert!(
            matches!(ev.as_slice(), [Event::GeneratorStarted { layer, column: 0, .. }] if *layer == l2)
        );
        // Control: a plain trigger stays on its own layer.
        let ev = apply(&mut p, trig(l1)).unwrap().events;
        assert!(matches!(ev.as_slice(), [Event::GeneratorStarted { layer, .. }] if *layer == l1));
    }

    #[test]
    fn hold_release_ejects_only_the_clip_it_played() {
        let (mut p, [l1, l2, _]) = three_layers();
        gen_cell(&mut p, l1, 0);
        gen_cell(&mut p, l2, 0);
        let clip = clip_in(&p, A, l1, 0).unwrap().id;
        let h = HoldFired {
            deck: A,
            layer: l1,
            column: 0,
            clip,
            onto: l2,
        };
        // The clip it played is in the air on the target layer: eject that layer.
        assert_eq!(
            hold_release(&p, &[(l2, A, l1, 0)], &h),
            Some(Command::ClearLayer { layer: l2 })
        );
        // Another clip took the layer since: left alone.
        assert_eq!(hold_release(&p, &[(l2, A, l2, 0)], &h), None);
        // Nothing in the air (it was ejected by someone else): nothing to do.
        assert_eq!(hold_release(&p, &[], &h), None);
        // The cell now holds a different clip (same place, new content): left alone.
        p.composition.decks[0]
            .clips
            .iter_mut()
            .find(|k| k.layer == l1 && k.column == 0)
            .unwrap()
            .clip
            .id = uuid::Uuid::new_v4();
        assert_eq!(hold_release(&p, &[(l2, A, l1, 0)], &h), None);
    }

    #[test]
    fn resolving_a_trigger_never_touches_the_project() {
        let (mut p, [l1, ..]) = three_layers();
        gen_cell(&mut p, l1, 0);
        let before = p.clone();
        let _ = resolve_trigger(&p, &TriggerCtx::default(), trig(l1));
        assert_eq!(p, before);
    }

    #[test]
    fn ignore_column_trigger_skips_the_layer() {
        let (mut p, [l1, l2, _]) = three_layers();
        gen_cell(&mut p, l1, 0);
        gen_cell(&mut p, l2, 0);
        p.composition.layers[1].ignore_column_trigger = true;
        let ev = apply(&mut p, Command::TriggerColumn { deck: A, column: 0 })
            .unwrap()
            .events;
        let touches = |l| {
            ev.iter().any(|e| {
                matches!(e, Event::GeneratorStarted { layer, .. }
                    | Event::LayerCleared { layer }
                    | Event::Skipped { layer, .. } if *layer == l)
            })
        };
        assert!(touches(l1));
        assert!(!touches(l2));
        // Control: without the flag the layer is triggered.
        p.composition.layers[1].ignore_column_trigger = false;
        let ev2 = apply(&mut p, Command::TriggerColumn { deck: A, column: 0 })
            .unwrap()
            .events;
        assert!(
            ev2.iter()
                .any(|e| matches!(e, Event::GeneratorStarted { layer, .. } if *layer == l2))
        );
    }

    #[test]
    fn group_ignore_lock_and_mask_commands_undo_and_apply_to_members() {
        let (mut p, [l1, l2, _]) = three_layers();
        gen_cell(&mut p, l1, 0);
        gen_cell(&mut p, l2, 0);
        let g = vjc_model::LayerGroup::new("G");
        let gid = g.id;
        p.composition.groups.push(g);
        p.composition.layers[1].group = Some(gid);
        let before = p.clone();
        let mut h = History::new();
        let t = Instant::now();
        for c in [
            Command::SetGroupMaskMode {
                group: gid,
                mode: vjc_model::MaskMode::AllBelow,
            },
            Command::SetGroupIgnoreColumn {
                group: gid,
                ignore: true,
            },
            Command::SetGroupLock {
                group: gid,
                locked: true,
            },
        ] {
            h.exec(&mut p, c, t).unwrap();
        }
        let g = &p.composition.groups[0];
        assert!(g.ignore_column_trigger && g.lock_content);
        assert_eq!(g.mask_mode, vjc_model::MaskMode::AllBelow);
        // The member follows its group, the layer outside does not.
        assert!(is_locked(&p, l2) && !is_locked(&p, l1));
        let ev = apply(&mut p, Command::TriggerColumn { deck: A, column: 0 })
            .unwrap()
            .events;
        let on = |l| {
            ev.iter()
                .any(|e| matches!(e, Event::GeneratorStarted { layer, .. } if *layer == l))
        };
        assert!(on(l1) && !on(l2));
        for _ in 0..3 {
            h.undo(&mut p).unwrap().unwrap();
        }
        assert_eq!(p.composition.groups, before.composition.groups);
        assert!(!is_locked(&p, l2));
        let ev = apply(&mut p, Command::TriggerColumn { deck: A, column: 0 })
            .unwrap()
            .events;
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::GeneratorStarted { layer, .. } if *layer == l2))
        );
        let bad = Command::SetGroupLock {
            group: uuid::Uuid::new_v4(),
            locked: true,
        };
        assert!(h.exec(&mut p, bad, t).is_err());
    }

    #[test]
    fn trigger_settings_commands_undo() {
        let (mut p, [l1, ..]) = three_layers();
        gen_cell(&mut p, l1, 0);
        let mut h = History::new();
        let t = Instant::now();
        let cmds = [
            Command::SetCompTriggerStyle {
                style: vjc_model::TriggerStyle::Toggle,
            },
            Command::SetLayerIgnoreColumn {
                layer: l1,
                ignore: true,
            },
            Command::SetClipTriggerStyle {
                deck: A,
                layer: l1,
                column: 0,
                style: vjc_model::TriggerStyle::Toggle,
            },
        ];
        let before = p.clone();
        for (i, c) in cmds.into_iter().enumerate() {
            h.exec(&mut p, c, t + Duration::from_secs(i as u64 + 1))
                .unwrap();
        }
        assert_ne!(p, before);
        for _ in 0..3 {
            h.undo(&mut p).unwrap().unwrap();
        }
        assert_eq!(p, before);
    }

    #[test]
    fn duplicate_layer_copies_clips_effects_with_fresh_ids_and_undoes() {
        let (mut p, [l1, l2, _]) = three_layers();
        gen_cell(&mut p, l2, 0);
        let fx = vjc_model::Effect {
            blend: Default::default(),
            id: Uuid::new_v4(),
            kind: "Invert".into(),
            bypass: false,
            dry_wet: 1.0,
            params: vec![],
        };
        p.composition.layers[1].track.effects.push(fx);
        let before = p.clone();
        let cmd = duplicate_layer_command(&p, l2).unwrap();
        let mut h = History::new();
        h.exec(&mut p, cmd, Instant::now()).unwrap();
        let ls = &p.composition.layers;
        assert_eq!(ls.len(), 4);
        // Inserted right above the original.
        assert_eq!(ls[2].id, l2);
        let copy = &ls[1];
        assert_ne!(copy.id, l2);
        assert_eq!(copy.track.effects.len(), 1);
        assert_ne!(copy.track.effects[0].id, ls[2].track.effects[0].id);
        let a = clip_in(&p, A, l2, 0).unwrap();
        let b = clip_in(&p, A, copy.id, 0).unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(a.source, b.source);
        // Control: the other layers got no clips.
        assert!(clip_in(&p, A, l1, 0).is_none());
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.layers.len(), 4);
        assert!(duplicate_layer_command(&p, Uuid::new_v4()).is_none());
    }

    #[test]
    fn locked_layer_ignores_trigger_eject_drop_clear_and_column() {
        let (mut p, [l1, l2, _]) = three_layers();
        gen_cell(&mut p, l1, 0);
        gen_cell(&mut p, l2, 0);
        let trig = |l| Command::TriggerClip {
            deck: A,
            layer: l,
            column: 0,
        };
        // Control: unlocked, all of it works.
        assert!(apply(&mut p, trig(l2)).is_ok());
        apply(
            &mut p,
            Command::SetLayerLock {
                layer: l2,
                locked: true,
            },
        )
        .unwrap();
        assert_eq!(apply(&mut p, trig(l2)).unwrap_err(), Error::Locked);
        assert_eq!(
            apply(&mut p, Command::ClearLayer { layer: l2 }).unwrap_err(),
            Error::Locked
        );
        assert_eq!(
            apply(
                &mut p,
                Command::ClearCell {
                    deck: A,
                    layer: l2,
                    column: 0
                }
            )
            .unwrap_err(),
            Error::Locked
        );
        assert_eq!(
            apply(&mut p, Command::ClearLayerClips { deck: A, layer: l2 }).unwrap_err(),
            Error::Locked
        );
        let c = clip_in(&p, A, l1, 0).unwrap().clone();
        assert_eq!(
            apply(
                &mut p,
                Command::SetClip {
                    deck: A,
                    layer: l2,
                    column: 0,
                    clip: c.clone()
                }
            )
            .unwrap_err(),
            Error::Locked
        );
        // Moving a clip into, or out of, the locked layer is refused; a copy out is fine.
        let mv = |from, to, copy| Command::MoveClip {
            deck: A,
            from,
            to,
            copy,
        };
        assert_eq!(
            apply(&mut p, mv((l1, 0), (l2, 0), false)).unwrap_err(),
            Error::Locked
        );
        assert_eq!(
            apply(&mut p, mv((l2, 0), (l1, 0), false)).unwrap_err(),
            Error::Locked
        );
        assert!(apply(&mut p, mv((l2, 0), (l1, 0), true)).is_ok());
        // Column trigger and clear-all leave the locked layer alone.
        let touches = |ev: &[Event], l| {
            ev.iter().any(|e| {
                matches!(e, Event::GeneratorStarted { layer, .. }
                    | Event::LayerCleared { layer } if *layer == l)
            })
        };
        let ev = apply(&mut p, Command::TriggerColumn { deck: A, column: 0 })
            .unwrap()
            .events;
        assert!(touches(&ev, l1) && !touches(&ev, l2));
        let ev = apply(&mut p, Command::ClearAll).unwrap().events;
        assert!(touches(&ev, l1) && !touches(&ev, l2));
        // Unlocking restores everything; the lock itself undoes.
        apply(
            &mut p,
            Command::SetLayerLock {
                layer: l2,
                locked: false,
            },
        )
        .unwrap();
        assert!(apply(&mut p, trig(l2)).is_ok());
        roundtrip(
            &mut p,
            Command::SetLayerLock {
                layer: l2,
                locked: true,
            },
        );
    }

    #[test]
    fn fader_start_fires_only_on_rise_from_zero_with_flag_and_no_lock() {
        let (mut p, [_, l2, _]) = three_layers();
        let set = |p: &mut Project, v| {
            apply(
                p,
                Command::SetOpacity {
                    layer: l2,
                    value: v,
                },
            )
            .unwrap()
            .events
        };
        let fired = |ev: &[Event]| ev.iter().any(|e| matches!(e, Event::FaderStart { .. }));
        set(&mut p, 0.0);
        // Control: flag off, no event.
        assert!(!fired(&set(&mut p, 0.5)));
        set(&mut p, 0.0);
        apply(
            &mut p,
            Command::SetLayerFaderStart {
                layer: l2,
                enabled: true,
            },
        )
        .unwrap();
        assert!(fired(&set(&mut p, 0.4)));
        // Moving a fader that is already up does not retrigger.
        assert!(!fired(&set(&mut p, 0.8)));
        // Dropping to zero does not either.
        assert!(!fired(&set(&mut p, 0.0)));
        // Locked layers ignore the trigger.
        apply(
            &mut p,
            Command::SetLayerLock {
                layer: l2,
                locked: true,
            },
        )
        .unwrap();
        assert!(!fired(&set(&mut p, 0.6)));
        roundtrip(
            &mut p,
            Command::SetLayerFaderStart {
                layer: l2,
                enabled: false,
            },
        );
    }

    #[test]
    fn clear_layer_clips_keeps_layer_and_undoes() {
        let (mut p, [l1, l2, _]) = three_layers();
        gen_cell(&mut p, l1, 0);
        gen_cell(&mut p, l2, 0);
        let before = p.clone();
        let mut h = History::new();
        let ev = h
            .exec(
                &mut p,
                Command::ClearLayerClips { deck: A, layer: l2 },
                Instant::now(),
            )
            .unwrap();
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::CellChanged { layer, .. } if *layer == l2))
        );
        assert!(clip_in(&p, A, l2, 0).is_none());
        assert_eq!(p.composition.layers.len(), 3);
        // Control: the neighbour keeps its clip.
        assert!(clip_in(&p, A, l1, 0).is_some());
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn rename_layer_hash_becomes_number() {
        let (mut p, [l1, _, l3]) = three_layers();
        let n = p.composition.layers.len();
        let top = p.composition.layers[0].id;
        apply(
            &mut p,
            Command::RenameLayer {
                layer: top,
                name: "Fx #".into(),
            },
        )
        .unwrap();
        assert_eq!(p.composition.layers[0].name, format!("Fx {n}"));
        // Control: no '#', name untouched.
        apply(
            &mut p,
            Command::RenameLayer {
                layer: l3,
                name: "Plain".into(),
            },
        )
        .unwrap();
        assert_eq!(layer_number(&p, top), Some(n));
        let _ = l1;
        roundtrip(
            &mut p,
            Command::RenameLayer {
                layer: top,
                name: "#-#".into(),
            },
        );
    }

    #[test]
    fn mask_mode_command_undoes() {
        let (mut p, [l1, ..]) = three_layers();
        roundtrip(
            &mut p,
            Command::SetLayerMaskMode {
                layer: l1,
                mode: vjc_model::MaskMode::OneBelow,
            },
        );
        apply(
            &mut p,
            Command::SetLayerMaskMode {
                layer: l1,
                mode: vjc_model::MaskMode::AllBelow,
            },
        )
        .unwrap();
        assert_eq!(
            p.composition
                .layers
                .iter()
                .find(|l| l.id == l1)
                .unwrap()
                .mask_mode,
            vjc_model::MaskMode::AllBelow
        );
    }

    #[test]
    fn fader_start_fires_inside_a_batch() {
        let (mut p, [_, l2, _]) = three_layers();
        for c in [
            Command::SetLayerFaderStart {
                layer: l2,
                enabled: true,
            },
            Command::SetOpacity {
                layer: l2,
                value: 0.0,
            },
        ] {
            apply(&mut p, c).unwrap();
        }
        let ev = apply(
            &mut p,
            Command::Batch {
                commands: vec![Command::SetOpacity {
                    layer: l2,
                    value: 0.7,
                }],
            },
        )
        .unwrap()
        .events;
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::FaderStart { layer } if *layer == l2))
        );
    }

    #[test]
    fn fader_start_not_fired_by_undo_redo() {
        let (mut p, [_, l2, _]) = three_layers();
        let mut h = History::new();
        let now = Instant::now();
        apply(
            &mut p,
            Command::SetLayerFaderStart {
                layer: l2,
                enabled: true,
            },
        )
        .unwrap();
        let fired = |ev: &[Event]| ev.iter().any(|e| matches!(e, Event::FaderStart { .. }));
        let set = |h: &mut History, p: &mut Project, v, t| {
            h.exec(
                p,
                Command::SetOpacity {
                    layer: l2,
                    value: v,
                },
                t,
            )
            .unwrap()
        };
        set(&mut h, &mut p, 0.0, now);
        // Direct edit fires.
        assert!(fired(&set(
            &mut h,
            &mut p,
            0.7,
            now + Duration::from_secs(5)
        )));
        // Undo to 0 and redo back up to 0.7: neither restarts the clip.
        assert!(!fired(&h.undo(&mut p).unwrap().unwrap()));
        assert!(!fired(&h.redo(&mut p).unwrap().unwrap()));
    }

    #[test]
    fn hash_rename_stores_final_name_and_undo_keeps_literal_hash() {
        let (mut p, [l1, ..]) = three_layers();
        p.composition
            .layers
            .iter_mut()
            .find(|l| l.id == l1)
            .unwrap()
            .name = "Old #".into();
        let mut h = History::new();
        h.exec(
            &mut p,
            Command::RenameLayer {
                layer: l1,
                name: "New #".into(),
            },
            Instant::now(),
        )
        .unwrap();
        let name = |p: &Project| {
            p.composition
                .layers
                .iter()
                .find(|l| l.id == l1)
                .unwrap()
                .name
                .clone()
        };
        assert_eq!(name(&p), "New 3");
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(name(&p), "Old #", "literal '#' survives undo");
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(name(&p), "New 3");
    }

    #[test]
    fn batch_cannot_get_around_the_lock() {
        let (mut p, [l1, l2, _]) = three_layers();
        gen_cell(&mut p, l1, 0);
        gen_cell(&mut p, l2, 0);
        apply(
            &mut p,
            Command::SetLayerLock {
                layer: l2,
                locked: true,
            },
        )
        .unwrap();
        let trig = |l| Command::TriggerClip {
            deck: A,
            layer: l,
            column: 0,
        };
        let before = p.clone();
        let b = Command::Batch {
            commands: vec![
                trig(l1),
                Command::Batch {
                    commands: vec![trig(l2)],
                },
            ],
        };
        assert_eq!(apply(&mut p, b).unwrap_err(), Error::Locked);
        assert_eq!(p, before);
        // Control: the same batch on the unlocked layer passes.
        assert!(
            apply(
                &mut p,
                Command::Batch {
                    commands: vec![trig(l1)]
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn insert_group_at_joins_only_between_same_group_layers() {
        let (mut p, [l1, l2, l3]) = three_layers();
        let g = Uuid::new_v4();
        for l in &mut p.composition.layers {
            if l.id == l1 || l.id == l2 {
                l.group = Some(g);
            }
        }
        let _ = l3;
        // Between the two grouped layers: joins. Edges and next to ungrouped: no.
        assert_eq!(insert_group_at(&p, 1), Some(g));
        assert_eq!(insert_group_at(&p, 0), None);
        assert_eq!(insert_group_at(&p, 2), None);
        assert_eq!(insert_group_at(&p, 3), None);
    }
}

#[cfg(test)]
mod screen_color_tests {
    use super::*;

    #[test]
    fn screen_enabled_and_color_undo_and_clamp() {
        let mut p = Project::new("t");
        let scr = mapping::new_screen("s", 8, 8);
        let id = scr.id;
        apply(
            &mut p,
            Command::AddScreen {
                screen: scr,
                index: 0,
            },
        )
        .unwrap();
        let off = apply(
            &mut p,
            Command::SetScreenEnabled {
                screen: id,
                enabled: false,
            },
        )
        .unwrap();
        assert!(!p.output.screens[0].enabled);
        apply(&mut p, off.inverse.unwrap()).unwrap();
        assert!(p.output.screens[0].enabled);
        let mut c = vjc_model::ScreenColor {
            opacity: 7.0,
            color: vjc_model::SliceColor {
                contrast: 9.0,
                ..Default::default()
            },
        };
        let u = apply(
            &mut p,
            Command::SetScreenColor {
                screen: id,
                color: c,
            },
        )
        .unwrap();
        assert_eq!(p.output.screens[0].color.opacity, 1.0);
        assert_eq!(p.output.screens[0].color.color.contrast, 2.0);
        apply(&mut p, u.inverse.unwrap()).unwrap();
        assert_eq!(p.output.screens[0].color, vjc_model::ScreenColor::default());
        c.opacity = f32::NAN;
        assert!(
            apply(
                &mut p,
                Command::SetScreenColor {
                    screen: id,
                    color: c
                }
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod beat_snap_tests {
    use super::tests::setup;
    use super::*;
    use vjc_model::BeatSnap as S;

    fn project() -> (Project, Command, uuid::Uuid) {
        let (p, layer) = setup();
        let deck = p.composition.current_deck;
        let col = p.composition.columns[0].id;
        (
            p,
            Command::TriggerClip {
                deck,
                layer,
                column: 0,
            },
            col,
        )
    }

    #[test]
    fn effective_snap_is_clip_then_column_then_composition() {
        let (mut p, trig, col) = project();
        let Command::TriggerClip { deck, layer, .. } = trig else {
            unreachable!()
        };
        let pref = S::Off;
        // Nothing set: the app preference.
        assert_eq!(effective_beat_snap(&p, S::Bars4, &trig), S::Bars4);
        // Composition beats the preference.
        apply(&mut p, Command::SetCompBeatSnap { snap: Some(S::Bar) }).unwrap();
        assert_eq!(effective_beat_snap(&p, pref, &trig), S::Bar);
        // Column beats the composition; a column None-division is an override too.
        apply(
            &mut p,
            Command::SetColumnBeatSnap {
                column: col,
                snap: Some(S::Half),
            },
        )
        .unwrap();
        assert_eq!(effective_beat_snap(&p, pref, &trig), S::Half);
        apply(
            &mut p,
            Command::SetColumnBeatSnap {
                column: col,
                snap: Some(S::Off),
            },
        )
        .unwrap();
        assert_eq!(effective_beat_snap(&p, pref, &trig), S::Off);
        // Clip beats the column.
        let k = Command::SetClipBeatSnap {
            deck,
            layer,
            column: 0,
            snap: Some(S::Bars2),
        };
        // Negative control: with no clip in the cell the override cannot be set.
        assert!(apply(&mut p, k.clone()).is_err());
        assert_eq!(effective_beat_snap(&p, pref, &trig), S::Off);
        let cmd = Command::ImportMedia {
            deck,
            layer,
            column: 0,
            path: "/x/a.mov".into(),
        };
        apply(&mut p, cmd).unwrap();
        apply(&mut p, k).unwrap();
        assert_eq!(effective_beat_snap(&p, pref, &trig), S::Bars2);
        // A column trigger ignores the clip override.
        let ct = Command::TriggerColumn { deck, column: 0 };
        assert_eq!(effective_beat_snap(&p, pref, &ct), S::Off);
        // Clearing the clip override falls back to the column.
        apply(
            &mut p,
            Command::SetClipBeatSnap {
                deck,
                layer,
                column: 0,
                snap: None,
            },
        )
        .unwrap();
        assert_eq!(effective_beat_snap(&p, pref, &trig), S::Off);
    }
}
