mod api_ext;
mod api_io;
mod artnet_out;
mod artnet_reduce;
mod artnet_scan;
mod audio_fx_ui;
mod audio_prefs;
mod audio_react;
mod autopilot_ui;
mod browser;
mod capture;
mod clap_host;
mod clip_thumbs;
mod col_ops;
mod color_depth;
mod compositor;
mod dj_io;
mod dmx_io;
mod ffgl_host;
#[cfg(test)]
mod ffgl_host_tests;
mod frame_stats;
mod fx;
mod fx_thumbs;
mod grid_ui;
mod hap_convert;
mod hap_gpu;
mod help;
mod hold;
mod icons;
mod io_monitor;
mod link_io;
mod mcp;
mod midi_io;
mod ml_fx;
mod ml_gpu;
mod monitor;
mod monitors;
mod netout;
mod osc_io;
mod output_artnet;
mod output_canvas;
mod output_extras;
mod output_hints;
mod output_overlay;
mod output_poly;
mod output_presets_ui;
mod output_props;
mod output_tree;
mod output_ui;
mod palette;
mod params;
mod persist;
mod pool;
mod prefs_ui;
mod preload;
mod qr;
mod reliability;
mod shortcut_editor;
mod slice_inspector;
mod slice_render;
mod source_info;
mod stats_bar;
mod theme;
mod timecode_io;
mod ui_stats;
mod vram;
mod wake;
mod wave_ui;
mod web_remote;
mod widgets;
mod xform_widget;

use compositor::{Compositor, EffectDraw, LayerDraw};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use help::help;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;
use vjc_core::clock::{Clock, LinkSync, beat_sync_rate};
use vjc_core::effects::EffectTarget;
use vjc_core::playback::TransportMode;
use vjc_core::playhead::Playhead;
use vjc_core::{AudioParam, AudioTarget, Command, Event, History};
use vjc_io::prefs::{Layout, Prefs};
use vjc_media::audio::AudioEngine;
use vjc_media::{Frame, Player};
use vjc_model::Screen;
use vjc_model::{
    Autopilot, AutopilotMode, AutopilotUnit, BlendMode, Column, Deck, DeckId, Direction, FitMode,
    Layer, LayerId, OutputDevice, PlayMode, Project, Track, TransformField, Transition,
    TriggerMode, Value,
};

/// Seconds a duration -/+ click moves the region length.
const DURATION_STEP: f64 = 0.1;

/// GPU target of one output screen with slices.
/// Per-frame inputs of animations, owned (dials 0..1 and audio levels).
struct AnimIn {
    dials: Vec<f64>,
    /// Dials of the owner of the params being drawn (`None` for composition-level ones): what
    /// scoped dial animations read. The renderer picks each layer's own.
    layer_dials: Option<Vec<f64>>,
    clip_dials: Option<Vec<f64>>,
    bands: [f32; vjc_media::analysis::BANDS],
    smoother: std::sync::Arc<vjc_core::anim::AudioSmoother>,
}

impl std::ops::Deref for AnimIn {
    type Target = [f64];
    fn deref(&self) -> &[f64] {
        &self.dials
    }
}

struct ScreenRt {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    id: egui::TextureId,
    /// Registered with nearest filtering (pixel-perfect screen).
    nearest: bool,
    /// Scratch target the slices draw into while the screen has a delay.
    live: Option<(wgpu::Texture, wgpu::TextureView)>,
    ring: slice_render::DelayRing,
}

struct State {
    /// Largest texture side the GPU supports (adapter limit); 16384 until known.
    tex_cap: u32,
    project: Project,
    history: History,
    warnings: Vec<String>,
    status: String,
    /// Texture-memory warning (see vram.rs), shown in the status bar while it holds.
    vram_warn: Option<String>,
    vram_dog: vram::Watchdog,
    /// Texture-memory limit in bytes (`VJC_VRAM_LIMIT_MB`).
    vram_limit: u64,
    frame_ms: frame_stats::FrameStats,
    /// View > Show FPS & Stats: readout in the status bar (runtime only).
    show_stats: bool,
    /// Last `ui_scale` pref pushed to egui (so the pref is not forced back every frame).
    applied_ui_scale: Option<f32>,
    /// Cmd/Ctrl+K quick search.
    palette: palette::Palette,
    /// Animation presets already handed to the param popups (false: publish them).
    presets_published: bool,
    stats: stats_bar::Sampler,
    /// Estimated texture bytes (refreshed with the VRAM watchdog) and GPU adapter name.
    vram_bytes: u64,
    gpu_name: String,
    ui_stats: ui_stats::UiStats,
    last_ui: Option<std::time::Instant>,
    render_gate: FrameGate,
    /// Status-bar frame-time text, rebuilt every 500 ms so its galley stays cached.
    frame_label: (Option<std::time::Instant>, String),
    /// Decoders opened ahead for the deck the user is about to switch to.
    preload: preload::Preload,
    frames: u64,
    /// Playing clip per layer: (deck it was triggered from, column, player). One clip per
    /// layer. Lookups of the playing clip use this deck, never `current_deck`.
    players: HashMap<LayerId, (DeckId, u32, Player)>,
    /// Audio output, opened on the first clip start (None until then or when unavailable).
    audio: Option<AudioEngine>,
    /// CLAP plugin catalog and per-effect plugin state.
    clap: clap_host::ClapHost,
    /// CLAP folders (from the preferences) the last scan was started with.
    clap_dirs_scanned: Vec<String>,
    /// FFGL folders (from the preferences) the last scan was started with.
    ffgl_dirs_scanned: Vec<String>,
    /// Click output, created the first time a beat lands with Metronome on.
    metro: Option<vjc_media::audio::Metronome>,
    audio_failed: bool,
    /// Output device `audio` was (or will be) opened on.
    audio_cfg: vjc_media::audio::OutConfig,
    /// `audio_failed` came from a real open error (not from the UI-test setup).
    audio_error: bool,
    /// Names of the audio output devices listed in Preferences > Audio.
    audio_outputs: Option<Vec<String>>,
    audio_inputs: Option<Vec<String>>,
    /// Codecs this build can record with, checked on first use.
    rec_codecs: vjc_media::record::Probe,
    /// Writes prefs and the recovery file (false in UI tests).
    persist: bool,
    /// Playing generator clip per layer: (deck, column, own TIME in seconds). The time
    /// follows the clip transport (speed, direction, pause); a layer has a player or a
    /// generator, never both.
    gens: HashMap<LayerId, (DeckId, u32, f64)>,
    /// (seconds, beats) at which each layer's clip was triggered (`Once` animations).
    clip_start: HashMap<LayerId, (f64, f64)>,
    /// Deck tab being renamed, with its edit buffer.
    renaming: Option<(DeckId, String)>,
    /// Tests pin the clock the undo history coalesces with, so a slow machine cannot split
    /// one gesture into two steps.
    #[cfg(test)]
    history_now: Option<std::time::Instant>,
    /// Column header being renamed: (column id, text so far).
    col_renaming: Option<(Uuid, String)>,
    /// Columns marked in the grid header (ctrl/cmd-click, shift-click) for the header menu.
    col_marks: Vec<Uuid>,
    /// Playhead per layer, restarted with each trigger (stale entries are harmless).
    heads: HashMap<LayerId, Playhead>,
    /// Codec / size / fps of file clips, probed on demand.
    src_info: source_info::SourceInfos,
    /// Convert to HAP: queue, the question being asked, and the swap offer after a finish.
    hap: hap_convert::Converter,
    hap_ask: Option<HapAsk>,
    hap_swap: Option<(hap_convert::Request, PathBuf)>,
    last_head_tick: std::time::Instant,
    /// Runtime-only playback flow per layer (transitions, loop counts, beat loops).
    flow: Flow,
    comp: Option<Compositor>,
    fx: fx::FxLib,
    ml: ml_fx::MlHub,
    fx_epoch: std::time::Instant,
    audio_react: audio_react::AudioReact,
    /// Tab of the bottom parameter panel.
    panel_tab: PanelTab,
    comp_tex: Option<egui::TextureId>,
    /// egui id of the preview monitor texture (registered while the monitor is visible).
    prev_tex: Option<egui::TextureId>,
    /// Fit / pan of the Composition and Preview monitors (runtime only).
    mon_views: [monitor::MonitorView; 2],
    /// View > Transform widget: handles on the Preview monitor (runtime only).
    xf_show: bool,
    /// Widget drag in progress: handle, values and pointer (frame px) at the press.
    xf_drag: Option<XfDrag>,
    /// Preview monitor area of the last frame (for hit tests).
    xf_area: egui::Rect,
    /// Set by `monitor_column` when drawn; `compose` renders the preview only then.
    monitor_seen: bool,
    /// Output recording in progress (runtime only).
    capture: Option<capture::Capture>,
    /// Syphon / NDI publishers of the screens set to those devices (runtime only).
    netout: netout::NetOut,
    artnet: artnet_out::ArtNetOut,
    artnet_scan: artnet_scan::Scanner,
    /// Start/stop requested by a command, served in `compose` (it owns the GPU handles).
    rec_request: Option<bool>,
    /// Display id of the monitor the output window is on, while open.
    output: Option<u32>,
    monitors: Vec<monitors::MonitorInfo>,
    monitors_polled: Option<std::time::Instant>,
    /// Cell rects from the last frame, for hit-testing file drops.
    cells: Vec<CellInfo>,
    /// Layer name and master bar rects from the last frame, for browser effect drops.
    layer_rects: Vec<(egui::Rect, LayerId)>,
    /// Screen rect the grid is clipped to (last frame).
    grid_rect: egui::Rect,
    /// Visible (clip) rect of the deck-tab strip, for tests.
    tabs_clip: egui::Rect,
    master_rect: Option<egui::Rect>,
    browser: browser::Browser,
    thumbs: clip_thumbs::ClipThumbs,
    last_pointer: Option<egui::Pos2>,
    /// Cell selected by click; fallback target for drops.
    selected: Option<(LayerId, u32)>,
    /// Deck `selected` was picked in (None: the current deck); Delete targets it.
    selected_deck: Option<DeckId>,
    /// Layer whose name is being dragged to a new place in the stack.
    layer_drag: Option<LayerId>,
    /// Clip copied or cut from a cell, for Paste.
    clip_clipboard: Option<vjc_model::Clip>,
    /// Cell whose thumbnail is being dragged to another cell (move; Alt copies).
    cell_drag: Option<(LayerId, u32)>,
    /// What the Preview monitor shows (see `PreviewTarget`).
    preview_target: PreviewTarget,
    /// Player of a `PreviewTarget::Clip` (runtime only, never in `players`).
    clip_prev: Option<ClipPreview>,
    /// The preview texture holds a fresh render this frame.
    prev_live: bool,
    /// Group shown in the Group tab.
    sel_group: Option<vjc_model::GroupId>,
    /// Column last triggered (by column, cell or step); the "selected" column of shortcuts.
    col_sel: Option<u32>,
    /// Shortcuts > Edit keyboard: clicks pick a target instead of acting.
    learn: bool,
    /// Target (address) waiting for a key while learning.
    learn_target: Option<String>,
    /// Learn waits for a MIDI control (Shortcuts > Edit MIDI) instead of a key; implies `learn`.
    learn_midi: bool,
    /// MIDI devices thread, and the LED state last sent to controllers.
    midi: midi_io::Midi,
    collect_job: Option<CollectJob>,
    /// External timecode (MTC / LTC) state.
    tc: timecode_io::TcRuntime,
    /// MIDI input ports and audio input devices listed in Preferences > Timecode.
    tc_devices: Option<(Vec<String>, Vec<String>)>,
    /// MIDI (input, output) port names for the Preferences tab; `None` = rescan.
    midi_ports: Option<(Vec<String>, Vec<String>)>,
    /// Last incoming MIDI / OSC messages for the monitor windows (Preferences).
    midi_log: io_monitor::Log,
    osc_log: io_monitor::Log,
    midi_monitor_open: bool,
    osc_monitor_open: bool,
    /// Snapshot PNG asked from a clip menu, served after the next composition frame.
    snap_req: Option<(DeckId, LayerId, u32)>,
    /// A HAP clip snapshot decoding on a worker: (clip name, result). Never decoded on the UI
    /// thread; `serve_snapshot` writes it when it arrives.
    snap_job: Option<(String, SnapRx)>,
    /// Shuffle bags of layer autopilots in Random / Bag mode.
    layer_bags: std::cell::RefCell<HashMap<LayerId, Vec<u32>>>,
    /// Opens a file in the file manager; a no-op in tests.
    reveal: Box<dyn wave_ui::Reveal>,
    led_prev: HashMap<vjc_core::midi::Led, bool>,
    /// OSC sockets thread, the config last sent to it, and the output state last sent.
    osc: osc_io::Osc,
    osc_cfg: Option<osc_io::Cfg>,
    osc_prev: HashMap<String, f64>,
    osc_limit: vjc_core::osc::RateLimit,
    /// Art-Net input thread and the config last sent to it.
    /// DJ player listener, what it heard and the follower steering the clock.
    dj: dj_io::Dj,
    dj_cfg: Option<dj_io::Cfg>,
    /// Interface text being typed in Preferences > DJ sync (applied on commit only).
    dj_iface_buf: Option<String>,
    /// The user typed in `dj_iface_buf` (an untouched buffer is not committed on close).
    dj_iface_edited: bool,
    /// Interfaces listed for Preferences > DJ sync, refreshed every couple of seconds.
    dj_ifaces: dj_io::IfaceCache,
    dj_tracker: vjc_core::djlink::Tracker,
    dj_follow: vjc_core::djlink::Follower,
    dmx: dmx_io::Dmx,
    dmx_cfg: Option<dmx_io::Cfg>,
    api: api_io::Api,
    api_cfg: Option<api_io::Cfg>,
    snap_jobs: Vec<api_ext::SnapJob>,
    snap_last: Option<std::time::Instant>,
    /// Mappable widgets drawn this frame, for learn clicks and the key overlay.
    targets: Vec<(egui::Rect, String)>,
    /// Master bypass: black output (runtime only, not saved).
    master_bypass: bool,
    /// Path of the open .vjc, if any.
    file: Option<PathBuf>,
    dirty: bool,
    /// Bumped with every edit that sets `dirty`; the recovery publish encodes on change.
    project_gen: u64,
    published_gen: u64,
    published_bpm: f64,
    last_publish: std::time::Instant,
    publish_count: u32,
    last_autosave: std::time::Instant,
    prefs: Prefs,
    prefs_open: bool,
    sc_ed: shortcut_editor::Editor,
    /// Autosaved project waiting for a Restore/Discard answer.
    recovery_offer: Option<PathBuf>,
    /// Relink dialog is showing.
    relink_open: bool,
    /// Toasts raised outside a frame; queued on the next one.
    toasts: Vec<(widgets::Level, String)>,
    output_setup_open: bool,
    sel_screen: Option<Uuid>,
    sel_slice: Option<Uuid>,
    /// Slices selected together (shift / cmd click); empty means only `sel_slice`.
    sel_slices: Vec<Uuid>,
    /// Working copy of the Output setup while its window is open.
    out_win: Option<output_ui::OutWin>,
    /// Guide images being decoded off the UI thread.
    guide_jobs: Vec<output_canvas::GuideJob>,
    sel_pt_in: Option<usize>,
    sel_pt_out: Option<usize>,
    snap: bool,
    /// Root folder of effect presets; `None` in tests.
    fx_presets_dir: Option<PathBuf>,
    /// Folder of output presets; `None` in tests (nothing touches the user's folder).
    presets_dir: Option<PathBuf>,
    /// Preset names in the folder; `None` = read again.
    preset_names: Option<Vec<String>>,
    preset_sel: Option<String>,
    /// Name typed for Save / Rename.
    preset_text: String,
    preset_confirm_delete: bool,
    /// Why the last loaded preset differs from what was saved.
    preset_warnings: Vec<String>,
    /// Side of the slice the inspector's numeric fields and buttons edit.
    /// Output canvas edits the slice mask polygon instead of the warp mesh.
    edit_mask: bool,
    sel_mask_pt: Option<usize>,
    /// Polygon drawing and point editing on the Output canvas.
    poly: output_poly::PolyUi,
    /// Screens whose output window the user closed (runtime only).
    closed_screens: std::collections::HashSet<Uuid>,
    /// Dev rehearsal (VJC_REHEARSE): output windows open windowed, not fullscreen.
    output_windowed: bool,
    /// Rehearsal session: its scratch screen is never autosaved or recovered.
    rehearsal: bool,
    /// Screens showing the test card on their output (runtime only).
    test_cards: std::collections::HashSet<Uuid>,
    /// Screens held black by the black-out switch (runtime only).
    blackouts: std::collections::HashSet<Uuid>,
    /// Pixel-map grid tile size per screen (runtime only).
    pixel_grids: HashMap<Uuid, (u32, u32)>,
    /// Slices copied in the Output setup (internal clipboard).
    slice_clip: Vec<vjc_model::Slice>,
    /// FPS overlay on every output.
    show_fps: bool,
    /// When "Identify displays" was pressed (shows for `output_overlay::IDENTIFY_FOR`).
    identify_at: Option<std::time::Instant>,
    display_info_open: bool,
    /// Presented-frame rate per output window.
    out_fps: HashMap<Uuid, output_overlay::FpsMeter>,
    slice_r: Option<slice_render::SliceRenderer>,
    stamper: Option<slice_render::Stamper>,
    /// Per-screen render target and its egui texture id.
    screen_rt: HashMap<Uuid, ScreenRt>,
    prefs_tab: PrefsTab,
    prefs_dirty: bool,
    title: String,
    /// Master clock (runtime only; BPM lives in the project, phase here).
    clock: Clock,
    /// Latest beat index and phase from `clock.tick`, for the indicator.
    beat: i64,
    phase: f64,
    /// Triggers and beat loops waiting for their beat or bar (beat snap).
    snap_pending: Vec<(f64, Held)>,
    /// Layer a redirected clip (clip target Active / Free) lives in, per layer it plays on:
    /// (deck, layer of the cell, column). Every lookup of the clip behind a playing source
    /// goes through it (`clip_for`, `cell_of`); a source that plays on its own layer has no
    /// entry.
    origin: HashMap<LayerId, (DeckId, LayerId, u32)>,
    /// (layer it plays on, layer of the cell) of the `TriggerClipOn` being handled.
    redirect: Option<(LayerId, LayerId)>,
    /// Layer chosen by clicking its name: where "Active layer" clips play. Selecting a
    /// cell does not change it.
    active_layer: Option<LayerId>,
    /// Hold clips in the air, one per button or key.
    holds: Vec<hold::Run>,
    /// A mouse press on a Hold cell that has not become a hold yet.
    hold_arm: Option<hold::Arm>,
    hold_token: u64,
    /// Composition autopilot: which column is on air and since when (runtime only).
    comp_ap: vjc_core::comp_autopilot::State,
    nudge_dir: i8,
    link: link_io::Link,
    link_sync: LinkSync,
    /// Set when a layout is applied or panels are toggled: the next frame forces the saved
    /// sizes once (panel ids stay stable, so egui memory does not grow per layout switch).
    force_panels: bool,
    /// Window size last frame: growth re-forces the saved panel sizes.
    win_size: egui::Vec2,
    /// Height of the top bars last frame (reserved when capping the bottom panel).
    topbars_h: f32,
    /// Height of the transport bar last frame (also reserved under the grid).
    transport_h: f32,
    /// Name being typed in the "Save layout as..." window.
    save_layout: Option<String>,
    /// Name of an existing custom layout waiting for overwrite confirmation.
    overwrite_layout: Option<String>,
    /// Playing layer waiting for the Yes/No before it is removed with its clips.
    remove_layer_confirm: Option<LayerId>,
    /// Help text key under the pointer in the previous frame.
    help_shown: Option<&'static str>,
}

/// Layer and column of a grid cell.
type Cell = (LayerId, u32);

/// Index (0 = top) the dragged layer ends at when released at height `y`: the number of
/// other layer rows whose centre is above it. `rows` is top to bottom.
fn layer_drop_index(rows: &[(egui::Rect, LayerId)], dragged: LayerId, y: f32) -> usize {
    rows.iter()
        .filter(|(r, l)| *l != dragged && r.center().y < y)
        .count()
}

/// Entries of the clip cell menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellAct {
    Trigger,
    Preview,
    Cut,
    Copy,
    Paste,
    Clear,
    ConvertHap,
}

/// A "Convert to HAP" question waiting for the user.
struct HapAsk {
    src: PathBuf,
    cell: Option<(DeckId, LayerId, u32)>,
    clip: Option<vjc_model::ClipId>,
    /// Chosen variant; `None` = the default for the source.
    variant: Option<hap_convert::Variant>,
}

/// What the Preview monitor shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PreviewTarget {
    /// One clip on its own player; never reaches the composition or the output.
    Clip(DeckId, LayerId, u32),
    /// The layer alone, as it is mixed in the composition.
    Layer(LayerId),
    /// The whole composition (nothing picked).
    #[default]
    Composition,
}

/// Own player of the clip shown by the Preview monitor (separate from `players`).
struct ClipPreview {
    key: (DeckId, LayerId, u32),
    /// Clip the player was opened for: a new clip in the same cell reopens it.
    clip: Uuid,
    /// `None` for sources that need no decoder (generators).
    player: Option<Player>,
    since: std::time::Instant,
}

/// Caption of the preview monitor. A layer with its video off is not drawn, so the monitor
/// would be a bare black frame; say why.
fn preview_note(layer_selected: bool, video_off: bool) -> &'static str {
    match (layer_selected, video_off) {
        (true, true) => "Selected layer: video is off",
        (true, false) => "Selected layer",
        (false, _) => "Composition (no layer selected)",
    }
}

/// Channel mask of a clip's picture (all on without a clip).
fn channels_of(clip: Option<&vjc_model::Clip>) -> vjc_model::Channels {
    clip.map_or_else(Default::default, |k| k.channels)
}

/// Smallest height the clip grid keeps when panels are enlarged.
const GRID_MIN: f32 = 80.0;

/// Old clip kept alive while a layer transition crosses to the new one (frozen on its
/// last frame: only the incoming clip advances).
struct Outgoing {
    player: Player,
    fit: vjc_model::FitMode,
    state: vjc_core::transition::State,
    /// The outgoing clip, kept so its audio follows layer/master mix and fades out.
    clip: Option<vjc_model::Clip>,
}

/// Whose dashboard a dial row edits.
#[derive(Clone, Copy)]
enum DialOwner {
    Comp,
    Layer(LayerId),
    Clip(DeckId, LayerId, u32),
}

/// An action held back by beat snap until its beat or bar.
enum SnapAct {
    Cmd(Box<Command>),
    BeatLoop(LayerId, f64),
}

/// Columns with a clip on air in any deck: columns are shared by all decks, so removing one
/// deletes its clips in every deck.
fn cols_on_air(players: &HashMap<LayerId, (DeckId, u32, Player)>, gens: &Gens) -> Vec<u32> {
    players
        .values()
        .map(|(_, c, _)| *c)
        .chain(gens.values().map(|(_, c, _)| *c))
        .collect()
}

/// Memory key under which the Preferences scale slider stores its widget id.
pub(crate) fn ui_scale_slider_key() -> egui::Id {
    egui::Id::new("ui-scale-slider")
}

/// Is the scale slider being dragged right now? Read from egui's pointer state (not a flag
/// the slider writes), so it cannot stick when the slider stops being drawn, and it is
/// already false on the frame the button is released.
fn ui_scale_dragging(ctx: &egui::Context) -> bool {
    let Some(id) = ctx.data(|d| d.get_temp::<egui::Id>(ui_scale_slider_key())) else {
        return false;
    };
    ctx.is_being_dragged(id) && ctx.input(|i| i.pointer.any_down() && !i.pointer.any_released())
}

/// The zoom to set when the `ui_scale` pref differs from the last one applied; None while
/// it is unchanged, so a Cmd +/- zoom made meanwhile stays.
/// Not while the scale slider is being dragged: zooming under the pointer moves the slider
/// itself, so the value is applied once the drag ends.
fn scale_to_apply(applied: &mut Option<f32>, pref: f32, dragging: bool) -> Option<f32> {
    if dragging || applied.is_some_and(|a| (a - pref).abs() < 1e-3) {
        return None;
    }
    *applied = Some(pref);
    Some(pref)
}

/// Whether the palette may take arrows and Enter: nothing is focused or its own field is.
fn palette_owns_keys(focused: Option<egui::Id>, query: egui::Id) -> bool {
    focused.is_none_or(|f| f == query)
}

/// The autopilot after the "Every loops" field was set to `n`: the unit stays as it is.
fn every_loops_edit(a: &vjc_model::Autopilot, n: f32) -> vjc_model::Autopilot {
    vjc_model::Autopilot {
        every_loops: n.round().max(1.0) as u32,
        ..a.clone()
    }
}

/// Rewrites the column of a queued command after columns moved; false when its column was
/// removed (the command is dropped).
fn remap_command(cmd: &mut Command, map: &impl Fn(u32) -> Option<u32>) -> bool {
    match cmd {
        Command::ImportMedia { column, .. }
        | Command::SetClip { column, .. }
        | Command::SetGenerator { column, .. }
        | Command::SetGeneratorParam { column, .. }
        | Command::ClearCell { column, .. }
        | Command::TriggerClip { column, .. }
        | Command::TriggerClipOn { column, .. }
        | Command::TriggerColumn { column, .. }
        | Command::AddCue { column, .. }
        | Command::RemoveCue { column, .. }
        | Command::SeekClip { column, .. }
        | Command::SetInOut { column, .. }
        | Command::SetClipSync { column, .. }
        | Command::SetClipDuration { column, .. }
        | Command::RefreshClipThumb { column, .. }
        | Command::JumpToCue { column, .. }
        | Command::SetClipDial { column, .. }
        | Command::SetClipChannels { column, .. }
        | Command::RenameClip { column, .. }
        | Command::SetFit { column, .. }
        | Command::SetTriggerMode { column, .. }
        | Command::SetClipBeatSnap { column, .. }
        | Command::SetClipExtras { column, .. }
        | Command::SetClipTarget { column, .. }
        | Command::SetClipTriggerStyle { column, .. }
        | Command::SetBlendOverride { column, .. }
        | Command::SetClipTransition { column, .. }
        | Command::SetBeatSync { column, .. }
        | Command::SetTimecodeStart { column, .. }
        | Command::SetPlayMode { column, .. }
        | Command::SetDirection { column, .. }
        | Command::SetSpeed { column, .. } => map(*column).is_some_and(|n| {
            *column = n;
            true
        }),
        Command::MoveClip { from, to, .. } => match (map(from.1), map(to.1)) {
            (Some(f), Some(t)) => {
                from.1 = f;
                to.1 = t;
                true
            }
            _ => false,
        },
        Command::StepColumn { from, .. } | Command::StepClip { from, .. } => match *from {
            Some(f) => map(f).is_some_and(|n| {
                *from = Some(n);
                true
            }),
            None => true,
        },
        Command::SetMediaPaths { changes } => {
            changes.retain_mut(|(_, _, c, _)| {
                map(*c).is_some_and(|n| {
                    *c = n;
                    true
                })
            });
            !changes.is_empty()
        }
        Command::SetPaused { clips, .. } => {
            clips.retain_mut(|(_, _, c)| {
                map(*c).is_some_and(|n| {
                    *c = n;
                    true
                })
            });
            !clips.is_empty()
        }
        Command::RestoreDirections { items } => {
            items.retain_mut(|(_, _, c, _)| {
                map(*c).is_some_and(|n| {
                    *c = n;
                    true
                })
            });
            !items.is_empty()
        }
        _ => true,
    }
}

/// A snapped action and the wall time it was queued (a stalled clock must not hold it forever).
struct Held {
    act: SnapAct,
    at: std::time::Instant,
    /// Tag of the Hold that queued it (0 = none): releasing the Hold takes it back.
    token: u64,
}

/// Longest an action waits for its beat or bar, in bars of wall time.
const SNAP_MAX_BARS: f64 = 2.0;

/// Result of a snapshot worker: (width, height, RGBA), None when nothing was decoded.
type SnapRx = std::sync::mpsc::Receiver<Option<(u32, u32, Vec<u8>)>>;

#[derive(Default)]
struct Flow {
    outgoing: HashMap<LayerId, Outgoing>,
    loops: HashMap<LayerId, vjc_core::playback::LoopCounter>,
    /// Beat loop per layer: (beats, normalised window).
    beat_loops: HashMap<LayerId, (f64, (f64, f64))>,
    /// Last playhead position per layer.
    pos: HashMap<LayerId, f64>,
    /// File position each clip had when it was replaced (for the Continue trigger mode).
    /// Keyed by cell and holding the clip id, so a replaced clip does not inherit it.
    resume: HashMap<(DeckId, LayerId, u32), (vjc_model::ClipId, f64)>,
    /// In..out region each layer's playhead last ran over.
    regions: HashMap<LayerId, (f64, f64)>,
    /// Autopilot on generators: (last bar seen, bars completed) per layer.
    gen_bars: HashMap<LayerId, (i64, u32)>,
    /// Time each layer's clip has run, for the beat/second autopilot units.
    auto_time: HashMap<LayerId, vjc_core::playback::TimedClock>,
    /// Frozen / stop-motion picture per layer: (clip, frame shown, when it was taken).
    held: HashMap<LayerId, (vjc_model::ClipId, Frame, std::time::Instant)>,
}

/// Message from the collect-media worker.
enum CollectMsg {
    Progress(usize, usize),
    Done(vjc_io::media::Collect),
}

/// A running Collect media: the worker's channel, a cancel flag and the paths the cells
/// held when it started.
struct CollectJob {
    rx: std::sync::mpsc::Receiver<CollectMsg>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    before: HashMap<(DeckId, LayerId, u32), PathBuf>,
}

impl Drop for CollectJob {
    /// Closing the app or dropping the job stops the worker between files.
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Outgoing draws use this id mask so they get their own compositor slot.
const OUT_MASK: u128 = 0x6f75_745f_736c_6f74;

#[derive(Clone, Copy, PartialEq)]
enum PrefsTab {
    General,
    Recording,
    Defaults,
    Audio,
    Video,
    Control,
    Osc,
    Dmx,
    DjSync,
    Api,
    Timecode,
    Midi,
}

struct XfDrag {
    handle: xform_widget::Handle,
    start: xform_widget::Xf,
    p0: egui::Vec2,
    target: EffectTarget,
    /// Shift at the start: the corner keeps its scale mode for the whole drag, so one drag
    /// writes one set of fields (one undo step).
    shift: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum PanelTab {
    Composition,
    Layer,
    Group,
}

/// Active effects of a chain with this frame's uniforms. Effects still compiling or that
/// failed are skipped.
fn draw_chain(
    lib: &mut fx::FxLib,
    ml: &mut ml_fx::MlHub,
    src_tag: Option<u64>,
    chain: &[vjc_model::Effect],
    time: f32,
    size: (u32, u32),
    ctx: &vjc_core::anim::AnimCtx,
) -> Vec<EffectDraw> {
    let mut out = vec![];
    for e in chain.iter().filter(|e| !e.bypass) {
        // FFGL plugin: runs on the CPU in the compositor; no shader to wait for.
        if ffgl_host::is_kind(&e.kind) {
            if e.dry_wet.is_nan() || e.dry_wet <= 0.0 {
                continue;
            }
            if let Some(fx) = lib.ffgl.shared(e) {
                ffgl_host::FfglHost::apply_params(&fx, &vjc_core::anim::resolve(&e.params, ctx));
                out.push(EffectDraw {
                    kind: e.kind.clone(),
                    uniforms: [[0.0; 4]; vjc_fx::MAX_SLOTS],
                    dry_wet: e.dry_wet,
                    blend: vjc_core::blend::fx_mix_id(&e.blend),
                    lut: None,
                    ml: None,
                    persistent: None,
                    ticking: false,
                    ffgl: Some(fx),
                });
            }
            continue;
        }
        lib.request(&e.kind);
        if lib.status(&e.kind) != Some(&fx::Status::Ready) {
            continue;
        }
        // Fully dry: the effect never runs, so no worker and no model load for it.
        if e.dry_wet.is_nan() || e.dry_wet <= 0.0 {
            continue;
        }
        // ML effects wait for their model: missing or failed ones are skipped.
        let ml_draw = if ml_fx::model_of(&e.kind).is_some() {
            // No tag = a source that cannot own a model (outgoing transition): skipped.
            match src_tag.and_then(|t| ml.draw(&e.kind, e.id, t)) {
                Some(d) => Some(d),
                None => continue,
            }
        } else {
            None
        };
        // A LUT effect without a readable LUT is skipped (passthrough).
        let lut = if e.kind == "lut3d" || e.kind == "image_mask" {
            let path = e.params.iter().find_map(|p| match &p.value {
                vjc_model::Value::Text(t) if p.name == "file" => Some(t.as_str()),
                _ => None,
            });
            let got = match e.kind.as_str() {
                "image_mask" => path.and_then(|p| lib.picture(p)),
                _ => path.and_then(|p| lib.lut(p)),
            };
            match got {
                Some(l) => Some(l),
                None => continue,
            }
        } else {
            None
        };
        if let Some(isf) = lib.isf(&e.kind) {
            out.push(EffectDraw {
                kind: e.kind.clone(),
                uniforms: isf.pack(&vjc_core::anim::resolve(&e.params, ctx), time, size),
                dry_wet: e.dry_wet,
                blend: vjc_core::blend::fx_mix_id(&e.blend),
                lut,
                ml: ml_draw,
                persistent: isf.needs_instance().then_some(e.id),
                ticking: isf.tick_input.is_some(),
                ffgl: None,
            });
        }
    }
    out
}

/// "hue_shift" -> "Hue Shift".
fn fx_label(id: &str) -> String {
    id.split('_')
        .map(|w| {
            if w == "rgb" {
                return "RGB".to_string();
            }
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// "Add effect" submenu listing the bundled effects.
/// Context-menu entries to group, ungroup and move layer `li` (index in the layer list,
/// 0 = top) in or out of groups.
fn group_menu(
    ui: &mut egui::Ui,
    comp: &vjc_model::Composition,
    li: usize,
    cmds: &mut Vec<Command>,
) {
    let me = &comp.layers[li];
    let above = li.checked_sub(1).map(|i| &comp.layers[i]);
    let below = comp.layers.get(li + 1);
    match me.group {
        None => {
            for (label, join, nb) in [
                ("Group with layer above", "Add to group above", above),
                ("Group with layer below", "Add to group below", below),
            ] {
                let Some(nb) = nb else { continue };
                if let Some(group) = nb.group {
                    if widgets::btn(ui, join, widgets::Kind::Secondary).clicked() {
                        cmds.push(Command::MoveLayerToGroup {
                            layer: me.id,
                            group,
                        });
                        ui.close();
                    }
                } else if widgets::btn(ui, label, widgets::Kind::Secondary).clicked() {
                    let group =
                        vjc_model::LayerGroup::new(&format!("Group {}", comp.groups.len() + 1));
                    cmds.push(Command::CreateGroup {
                        group,
                        layers: vec![nb.id, me.id],
                    });
                    ui.close();
                }
            }
        }
        Some(group) => {
            let members = comp
                .layers
                .iter()
                .filter(|l| l.group == Some(group))
                .count();
            let edge = above.is_none_or(|a| a.group != Some(group))
                || below.is_none_or(|b| b.group != Some(group));
            if widgets::btn_if(
                ui,
                members > 1 && edge,
                "Remove from group",
                widgets::Kind::Secondary,
            )
            .clicked()
            {
                cmds.push(Command::MoveLayerOutOfGroup { layer: me.id });
                ui.close();
            }
            if widgets::btn(ui, "Ungroup", widgets::Kind::Secondary).clicked() {
                cmds.push(Command::Ungroup { group });
                ui.close();
            }
        }
    }
}

/// Grid row above the first layer of a group: name, opacity, blend, bypass/solo.
fn group_header(
    ui: &mut egui::Ui,
    project: &vjc_model::Project,
    g: &vjc_model::LayerGroup,
    ncols: u32,
    ui_state: (
        &mut Option<vjc_model::GroupId>,
        &mut PanelTab,
        &mut Vec<(egui::Rect, String)>,
    ),
    cmds: &mut Vec<Command>,
) {
    let (sel_group, panel_tab, targets) = ui_state;
    let gid = g.id;
    ui.vertical(|ui| {
        let name = ui.add(
            egui::Button::new(icons::with_text(
                ui,
                if g.folded {
                    icons::i::CARET_RIGHT
                } else {
                    icons::i::CARET_DOWN
                },
                &g.name,
            ))
            .selected(*sel_group == Some(gid)),
        );
        name.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Button,
                true,
                *sel_group == Some(gid),
                g.name.clone(),
            )
        });
        if name.clicked() || name.secondary_clicked() {
            *sel_group = Some(gid);
            *panel_tab = PanelTab::Group;
        }
        name.context_menu(|ui| {
            if crate::widgets::btn(ui, "Ungroup", crate::widgets::Kind::Secondary).clicked() {
                cmds.push(Command::Ungroup { group: gid });
                ui.close();
            }
            let fold = if g.folded {
                "Unfold group"
            } else {
                "Fold group"
            };
            if crate::widgets::btn(ui, fold, crate::widgets::Kind::Secondary).clicked() {
                cmds.push(Command::SetGroupFolded {
                    group: gid,
                    folded: !g.folded,
                });
                ui.close();
            }
            ui.separator();
            add_effect_menu(ui, EffectTarget::Group(gid), cmds);
        });
        let mut op = match g.track.opacity.value {
            Value::Number(n) => n as f32,
            _ => 1.0,
        };
        let r = ui.add(egui::Slider::new(&mut op, 0.0..=1.0).fixed_decimals(2));
        if let Some(a) = vjc_core::shortcuts::group_address(project, gid, "opacity") {
            targets.push((r.rect, a));
        }
        if r.changed() {
            cmds.push(Command::SetGroupOpacity {
                group: gid,
                value: op as f64,
            });
        }
    });
    let (mut bypass, mut solo) = (g.bypass, g.solo);
    ui.horizontal(|ui| {
        let b = icons::toggle(ui, &mut bypass, icons::i::POWER, "Group bypass");
        if let Some(a) = vjc_core::shortcuts::group_address(project, gid, "bypass") {
            targets.push((b.rect, a));
        }
        if b.changed() {
            cmds.push(Command::SetGroupBypass { group: gid, bypass });
        }
        let s = icons::toggle(ui, &mut solo, icons::i::HEADPHONES, "Group solo");
        if let Some(a) = vjc_core::shortcuts::group_address(project, gid, "solo") {
            targets.push((s.rect, a));
        }
        if s.changed() {
            cmds.push(Command::SetGroupSolo { group: gid, solo });
        }
        if ui
            .small_button(if g.folded { "Unfold" } else { "Fold" })
            .on_hover_text("Collapse or expand the group's layers in the grid")
            .clicked()
        {
            cmds.push(Command::SetGroupFolded {
                group: gid,
                folded: !g.folded,
            });
        }
        if ui
            .small_button("Ungroup")
            .on_hover_text("Dissolve the group, keep its layers")
            .clicked()
        {
            cmds.push(Command::Ungroup { group: gid });
        }
    });
    let label = vjc_core::blend::COMPOSITION_MODES
        .iter()
        .find(|(m, _)| *m == g.track.blend)
        .map_or("Normal", |(_, n)| *n);
    crate::widgets::combo(ui, ("gblend", gid), label, Some(90.0), |ui| {
        for (b, name) in vjc_core::blend::COMPOSITION_MODES {
            if crate::widgets::choice(ui, g.track.blend == *b, *name).clicked() {
                cmds.push(Command::SetGroupBlend {
                    group: gid,
                    blend: b.clone(),
                });
            }
        }
    });
    for _ in 0..ncols.saturating_sub(2) {
        ui.label("");
    }
    ui.end_row();
}

fn add_effect_menu(ui: &mut egui::Ui, target: EffectTarget, cmds: &mut Vec<Command>) {
    ui.menu_button("Add effect", |ui| add_effect_list(ui, target, cmds));
}

/// The bundled effects as menu entries; a click appends the effect to the target's chain.
fn add_effect_list(ui: &mut egui::Ui, target: EffectTarget, cmds: &mut Vec<Command>) {
    for (id, _) in vjc_fx::BUILTIN {
        if crate::widgets::btn(ui, &fx_label(id), crate::widgets::Kind::Secondary).clicked()
            && let Some(effect) = vjc_fx::new_effect(id)
        {
            cmds.push(Command::AddEffect {
                target,
                effect,
                index: usize::MAX,
            });
            ui.close();
        }
    }
}

/// Repaint period while nothing is live.
const IDLE_REPAINT: std::time::Duration = std::time::Duration::from_millis(100);

type Gens = HashMap<LayerId, (DeckId, u32, f64)>;

/// Deck and column of the clip playing on a layer (video or generator).
/// Position a Continue trigger resumes at: only when the cell still holds the clip it was
/// noted for.
fn resume_pos(
    p: &vjc_model::Project,
    resume: &HashMap<(DeckId, LayerId, u32), (vjc_model::ClipId, f64)>,
    key: (DeckId, LayerId, u32),
) -> Option<f64> {
    let (id, pos) = resume.get(&key)?;
    (vjc_core::clip_in(p, key.0, key.1, key.2)?.id == *id).then_some(*pos)
}

/// Drops resume entries of cells that no longer exist or now hold another clip.
fn prune_resume(
    p: &vjc_model::Project,
    resume: &mut HashMap<(DeckId, LayerId, u32), (vjc_model::ClipId, f64)>,
) {
    resume.retain(|k, (id, _)| vjc_core::clip_in(p, k.0, k.1, k.2).is_some_and(|c| c.id == *id));
}

/// Time between repaints of live video: the composition fps is the ceiling (max 240).
fn repaint_interval(fps: impl Into<f64>) -> std::time::Duration {
    std::time::Duration::from_secs_f64(1.0 / repaint_fps(fps))
}

/// Shortest gap between UI frames: the render interval, but never longer than 1/60 s so
/// a low composition fps (1, 24) does not make the controls lag.
fn ui_pace_gap(fps: impl Into<f64>) -> std::time::Duration {
    repaint_interval(fps).min(std::time::Duration::from_secs_f64(1.0 / 60.0))
}

/// How long to wait before starting a UI frame so frames are at least `min_gap` apart.
fn ui_pace_wait(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    min_gap: std::time::Duration,
) -> Option<std::time::Duration> {
    let since = now.checked_duration_since(last?)?;
    min_gap.checked_sub(since).filter(|d| !d.is_zero())
}

/// Repaint rate for live video: the composition fps, kept to 1..=240 (NaN or junk = 60).
fn repaint_fps(fps: impl Into<f64>) -> f64 {
    let f = fps.into();
    if f.is_finite() {
        f.clamp(1.0, 240.0)
    } else {
        60.0
    }
}

/// Render rate: the composition fps, lowered to the "Max frame rate" pref (0 = no cap).
fn effective_fps(comp_fps: f32, max_pref: u32) -> f64 {
    let f = repaint_fps(comp_fps);
    if max_pref == 0 {
        f
    } else {
        f.min(f64::from(max_pref))
    }
}

/// Render limiter: extra vsync ticks (120 Hz monitor, 60 fps comp) reuse the last frame.
/// Frames follow a fixed schedule (`next` advances by one interval), so any vsync rate,
/// integer multiple or not, averages to the composition fps.
#[derive(Default)]
struct FrameGate {
    next: Option<std::time::Instant>,
}

impl FrameGate {
    /// True when a new composition frame is due at `now`. A tick may come up to half a
    /// vsync early (timer jitter); a late or stalled loop does not try to catch up.
    fn due(&mut self, now: std::time::Instant, fps: impl Into<f64>) -> bool {
        let iv = repaint_interval(fps);
        let slack = iv.mul_f64(0.25);
        match self.next {
            Some(n) if now + slack < n => false,
            Some(n) => {
                let next = n + iv;
                // Fell behind by more than one frame: restart the schedule from now.
                self.next = Some(if next + slack < now { now + iv } else { next });
                true
            }
            None => {
                self.next = Some(now + iv);
                true
            }
        }
    }
}

#[cfg(test)]
fn gate_renders(vsync_hz: f64, comp_fps: f64, ticks: u32) -> u32 {
    let t0 = std::time::Instant::now();
    let mut g = FrameGate::default();
    (0..ticks)
        .filter(|i| {
            g.due(
                t0 + std::time::Duration::from_secs_f64(f64::from(*i) / vsync_hz),
                comp_fps,
            )
        })
        .count() as u32
}

#[test]
fn gate_halves_renders_at_120hz_vsync_with_60fps_comp() {
    assert_eq!(gate_renders(120.0, 60.0, 240), 120);
    // Non-integer ratio: 144 Hz vsync for one second still gives about 60 frames.
    let n = gate_renders(144.0, 60.0, 144);
    assert!((59..=61).contains(&n), "144 Hz: {n}");
    // Negative control: a 120 fps comp renders every tick.
    assert_eq!(gate_renders(120.0, 120.0, 240), 240);
    // 60 Hz vsync, 60 fps comp: every tick.
    assert_eq!(gate_renders(60.0, 60.0, 120), 120);
}

#[test]
fn ui_pace_gap_never_exceeds_a_60hz_frame() {
    use std::time::Duration as D;
    let cap = D::from_secs_f64(1.0 / 60.0);
    assert_eq!(
        ui_pace_gap(1.0),
        cap,
        "1 fps composition: the UI still runs at 60"
    );
    assert_eq!(ui_pace_gap(24.0), cap);
    assert_eq!(ui_pace_gap(120.0), repaint_interval(120.0));
}

#[test]
fn ui_pace_waits_only_for_the_rest_of_the_gap() {
    use std::time::{Duration as D, Instant};
    let t = Instant::now();
    let gap = D::from_millis(16);
    assert_eq!(ui_pace_wait(None, t, gap), None);
    assert_eq!(
        ui_pace_wait(Some(t), t + D::from_millis(4), gap),
        Some(D::from_millis(12))
    );
    assert_eq!(ui_pace_wait(Some(t), t + D::from_millis(16), gap), None);
    assert_eq!(ui_pace_wait(Some(t), t + D::from_millis(40), gap), None);
}

#[test]
fn repaint_interval_is_capped_by_composition_fps() {
    let ms = |f: f32| repaint_interval(f).as_secs_f64() * 1000.0;
    // Lower composition fps: slower repaint (the cap is respected).
    assert!((ms(30.0) - 1000.0 / 30.0).abs() < 1e-6);
    assert!(ms(30.0) > ms(60.0));
    // Default stays 60, and nothing repaints faster than 240 fps.
    assert!((ms(60.0) - 1000.0 / 60.0).abs() < 1e-6);
    assert!((ms(120.0) - 1000.0 / 120.0).abs() < 1e-6);
    assert!((ms(1000.0) - 1000.0 / 240.0).abs() < 1e-6);
    // Control: a bad value falls back to 60, never panics.
    assert!((ms(f32::NAN) - 1000.0 / 60.0).abs() < 1e-6);
}

#[test]
fn max_fps_pref_lowers_the_render_rate() {
    assert_eq!(effective_fps(60.0, 0), 60.0);
    assert_eq!(effective_fps(60.0, 30), 30.0);
    // Never raises above the composition fps.
    assert_eq!(effective_fps(30.0, 120), 30.0);
    // 60 Hz vsync with a 30 fps cap renders every other tick.
    assert_eq!(gate_renders(60.0, effective_fps(60.0, 30), 120), 60);
}

#[test]
fn repaint_fps_never_panics_on_bad_project_values() {
    assert_eq!(repaint_fps(f64::NAN), 60.0);
    assert_eq!(repaint_fps(f64::INFINITY), 60.0);
    assert_eq!(repaint_fps(0.0), 1.0);
    assert_eq!(repaint_fps(30.0), 30.0);
}

const MEDIA_EXTS: [&str; 10] = [
    "mov", "mp4", "m4v", "mkv", "avi", "webm", "png", "jpg", "jpeg", "webp",
];

fn is_media(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MEDIA_EXTS.iter().any(|m| m.eq_ignore_ascii_case(e)))
}

/// Most files one drop places (a huge folder must not add hundreds of columns at once).
const MAX_DROP: usize = 128;

/// Expands dropped folders (first level) into their media files; sorts by natural name.
/// Loose files are kept whatever their extension (the decoder decides, as before); inside a
/// folder only known media counts and hidden files (`._x.mp4` sidecars) are skipped.
/// Returns the files and how many entries were left out.
fn expand_drop(paths: Vec<PathBuf>, sequences: bool) -> (Vec<PathBuf>, usize) {
    let mut files = Vec::new();
    let mut ignored = 0;
    for p in paths {
        if p.is_dir() {
            // A pure numbered image folder stays as the folder itself: one sequence clip.
            if sequences && vjc_media::sequence::scan_dir(&p).is_some() {
                files.push(p);
                continue;
            }
            let Ok(rd) = std::fs::read_dir(&p) else {
                continue;
            };
            for e in rd.flatten() {
                let q = e.path();
                let hidden = q
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'));
                if q.is_file() && !hidden {
                    if is_media(&q) {
                        files.push(q);
                    } else {
                        ignored += 1;
                    }
                }
            }
        } else {
            files.push(p);
        }
    }
    files.sort_by_cached_key(|p| natural_key(p));
    if files.len() > MAX_DROP {
        ignored += files.len() - MAX_DROP;
        files.truncate(MAX_DROP);
    }
    (files, ignored)
}

/// Lowercased name with digit runs zero-padded, so "2" sorts before "10".
fn natural_key(p: &std::path::Path) -> String {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut out = String::new();
    let mut digits = String::new();
    for ch in name.chars().chain(std::iter::once('\0')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            if !digits.is_empty() {
                out.push_str(&format!("{:0>12}", digits));
                digits.clear();
            }
            out.push(ch);
        }
    }
    out
}

fn active_of(
    players: &HashMap<LayerId, (DeckId, u32, Player)>,
    gens: &Gens,
    layer: LayerId,
) -> Option<(DeckId, u32)> {
    let video = players.get(&layer).map(|(d, c, _)| (*d, *c));
    video.or_else(|| gens.get(&layer).map(|(d, c, _)| (*d, *c)))
}

/// Layer of the cell behind the source playing on `layer` (deck, column): the origin of a
/// redirected clip, else the layer itself.
fn src_layer(
    origin: &HashMap<LayerId, (DeckId, LayerId, u32)>,
    layer: LayerId,
    deck: DeckId,
    col: u32,
) -> LayerId {
    match origin.get(&layer) {
        Some((d, l, c)) if *d == deck && *c == col => *l,
        _ => layer,
    }
}

/// The clip behind the source playing on `layer` as (deck, col). Every lookup of a
/// playing clip (transport, speed, in/out, cues, effects) goes through here: a redirected
/// clip lives in another layer's cell than the one it plays on.
fn clip_for<'a>(
    p: &'a vjc_model::Project,
    origin: &HashMap<LayerId, (DeckId, LayerId, u32)>,
    layer: LayerId,
    deck: DeckId,
    col: u32,
) -> Option<&'a vjc_model::Clip> {
    vjc_core::clip_in(p, deck, src_layer(origin, layer, deck, col), col)
}

/// Layers the clip of cell (`src`, `col`) plays on (`deck` None = any deck): its own
/// layer when it plays there, and every layer it was redirected to.
fn dests_in(
    players: &HashMap<LayerId, (DeckId, u32, Player)>,
    gens: &Gens,
    origin: &HashMap<LayerId, (DeckId, LayerId, u32)>,
    deck: Option<DeckId>,
    src: LayerId,
    col: u32,
) -> Vec<LayerId> {
    let ok = |d: DeckId| deck.is_none_or(|x| x == d);
    let mut out: Vec<LayerId> = origin
        .iter()
        .filter(|(dest, (d, l, c))| {
            *l == src && *c == col && ok(*d) && active_of(players, gens, **dest) == Some((*d, *c))
        })
        .map(|(dest, _)| *dest)
        .collect();
    if !origin.contains_key(&src)
        && let Some((d, c)) = active_of(players, gens, src)
        && c == col
        && ok(d)
    {
        out.push(src);
    }
    out
}

/// Cell state for LEDs, OSC and the API: every cell on air in `layer`'s row. The layer's own
/// clip counts (unless the layer is only showing a redirected one), and so does each of its
/// clips playing on another layer.
fn cell_active_of(
    players: &HashMap<LayerId, (DeckId, u32, Player)>,
    gens: &Gens,
    origin: &HashMap<LayerId, (DeckId, LayerId, u32)>,
    layer: LayerId,
) -> Vec<(DeckId, u32)> {
    cells_on_air(origin, layer, |l| active_of(players, gens, l))
}

fn cells_on_air(
    origin: &HashMap<LayerId, (DeckId, LayerId, u32)>,
    layer: LayerId,
    on_air: impl Fn(LayerId) -> Option<(DeckId, u32)>,
) -> Vec<(DeckId, u32)> {
    // The layer's own cell comes first (the API reports it as `active_column`); the
    // redirects follow in order.
    let mut out = Vec::new();
    if !origin.contains_key(&layer)
        && let Some(a) = on_air(layer)
    {
        out.push(a);
    }
    let mut redirects = Vec::new();
    for (dest, (d, l, c)) in origin {
        if *l == layer && on_air(*dest) == Some((*d, *c)) && !out.contains(&(*d, *c)) {
            redirects.push((*d, *c));
        }
    }
    redirects.sort();
    redirects.dedup();
    out.extend(redirects);
    out
}

/// "New generator" submenu: creates the clip in the cell and plays it.
fn generator_menu(
    ui: &mut egui::Ui,
    deck: DeckId,
    layer: LayerId,
    column: u32,
    cmds: &mut Vec<Command>,
) {
    ui.menu_button("New generator", |ui| {
        for (id, _) in vjc_fx::GENERATORS {
            if crate::widgets::btn(ui, &fx_label(id), crate::widgets::Kind::Secondary).clicked() {
                cmds.push(Command::SetGenerator {
                    deck,
                    layer,
                    column,
                    kind: id.to_string(),
                    name: fx_label(id),
                    params: vjc_fx::generator_params(id).unwrap_or_default(),
                });
                cmds.push(Command::TriggerClip {
                    deck,
                    layer,
                    column,
                });
                ui.close();
            }
        }
    });
}

/// Inverse map of a track transform for a frame of `frame_px` (perspective distance).
fn xf_inverse(
    t: &vjc_model::Transform,
    ctx: &vjc_core::anim::AnimCtx,
    frame_px: (f64, f64),
) -> vjc_core::transform::Affine {
    vjc_core::transform::Affine::inverse_of_sized(
        &vjc_core::transform::Resolved::of(t, ctx),
        frame_px,
    )
}

/// Slider range and -/+ step of a transform field in the panel (a few frames wide, not the
/// whole model range, so a drag is usable).
fn transform_ui_range(field: TransformField, (w, h): (f64, f64)) -> ((f64, f64), f64) {
    match field {
        TransformField::X => ((-2.0 * w, 2.0 * w), 1.0),
        TransformField::Y => ((-2.0 * h, 2.0 * h), 1.0),
        TransformField::AnchorX => ((-w, w), 1.0),
        TransformField::AnchorY => ((-h, h), 1.0),
        TransformField::Scale | TransformField::ScaleW | TransformField::ScaleH => {
            ((0.0, 4.0), 0.01)
        }
        TransformField::Rotation => ((-360.0, 360.0), 1.0),
        TransformField::RotationX | TransformField::RotationY => ((-180.0, 180.0), 1.0),
        TransformField::AnchorZ => ((-w, w), 1.0),
    }
}

/// Transition types offered in the layer and clip panels: (stored name, label).
fn transition_kinds() -> Vec<(String, String)> {
    vjc_core::transition::Kind::ALL
        .iter()
        .map(|k| (k.name(), k.name()))
        .collect()
}

fn labeled<T: Copy>(v: &[(T, &str)]) -> Vec<(T, String)> {
    v.iter().map(|(a, n)| (*a, n.to_string())).collect()
}

/// Edits one effect param; returns the new value when the user changed it.
fn param_widget(
    ui: &mut egui::Ui,
    p: &vjc_model::Param,
    kind: &vjc_fx::InputKind,
    ctx: &vjc_core::anim::AnimCtx,
    anim_out: &mut Option<Option<vjc_model::Animation>>,
) -> Option<vjc_model::Value> {
    let (name, value) = (p.name.as_str(), &p.value);
    use vjc_fx::InputKind as K;
    use vjc_model::Value as V;
    let mut out = None;
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        ui.label(name);
        match (kind, value) {
            (K::Float { min, max, .. }, V::Number(n)) => {
                let d = match p.default {
                    V::Number(d) => d,
                    _ => *min,
                };
                let shown = vjc_core::anim::effective_number(p, ctx);
                let (v, a) =
                    params::param_slider_anim(ui, name, "", shown, d, (*min, *max), &p.animation);
                out = v.map(V::Number);
                *anim_out = a;
                let _ = n;
            }
            (K::Bool { .. }, V::Bool(b)) => {
                let mut x = *b;
                if crate::widgets::toggle(ui, &mut x, "").changed() {
                    out = Some(V::Bool(x));
                }
            }
            (K::Color { .. }, V::Color(c)) => {
                let mut x = *c;
                if ui.color_edit_button_rgba_unmultiplied(&mut x).changed() {
                    out = Some(V::Color(x));
                }
            }
            (K::Point2D { min, max, .. }, V::Color(c)) => {
                let mut x = *c;
                let a = ui.add(egui::Slider::new(&mut x[0], min[0] as f32..=max[0] as f32));
                let b = ui.add(egui::Slider::new(&mut x[1], min[1] as f32..=max[1] as f32));
                if a.changed() || b.changed() {
                    out = Some(V::Color(x));
                }
            }
            (K::Text { .. }, V::Text(t)) => {
                let id = ui.id().with(("fxtext", name));
                let mut buf: String = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| t.clone());
                let e = ui.add(
                    egui::TextEdit::singleline(&mut buf)
                        .char_limit(vjc_fx::TEXT_CHARS)
                        .desired_width(140.0),
                );
                // Commit on Enter or focus loss: one undo step per edit, not per key.
                if e.lost_focus() {
                    if buf != *t {
                        out = Some(V::Text(buf));
                    }
                    ui.data_mut(|d| d.remove_temp::<String>(id));
                } else if e.has_focus() {
                    ui.data_mut(|d| d.insert_temp(id, buf));
                }
            }
            (K::Long { labels, .. }, V::Choice { selected, options }) => {
                let mut sel = *selected;
                crate::widgets::combo(
                    ui,
                    ("fxlong", name),
                    labels.get(sel as usize).map_or("", String::as_str),
                    None,
                    |ui| {
                        for (i, l) in labels.iter().enumerate() {
                            ui.selectable_value(&mut sel, i as u32, l);
                        }
                    },
                );
                if sel != *selected {
                    out = Some(V::Choice {
                        selected: sel,
                        options: options.clone(),
                    });
                }
            }
            _ => {}
        }
    });
    out
}

/// Grid cell geometry for hit-testing.
#[derive(Clone, Copy)]
struct CellInfo {
    rect: egui::Rect,
    layer: LayerId,
    col: u32,
    empty: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum DropWhere {
    Pointer,
    Selected,
    FirstEmpty,
    FirstCell,
}

/// Picks the cell receiving a drop: cell under pointer, else selected cell,
/// else first empty cell of the bottom layer (last in `cells`), else its first cell.
fn resolve_drop(
    cells: &[CellInfo],
    pointer: Option<egui::Pos2>,
    selected: Option<(LayerId, u32)>,
) -> Option<(LayerId, u32, DropWhere)> {
    if let Some(p) = pointer
        && let Some(c) = cells.iter().find(|c| c.rect.contains(p))
    {
        return Some((c.layer, c.col, DropWhere::Pointer));
    }
    if let Some((l, col)) = selected
        && cells.iter().any(|c| c.layer == l && c.col == col)
    {
        return Some((l, col, DropWhere::Selected));
    }
    let bottom = cells.last()?.layer;
    let mut row = cells.iter().filter(|c| c.layer == bottom);
    if let Some(c) = row.clone().find(|c| c.empty) {
        return Some((c.layer, c.col, DropWhere::FirstEmpty));
    }
    row.next().map(|c| (c.layer, c.col, DropWhere::FirstCell))
}

/// Browser start folder: the pref, else the project's folder, else ~/Movies, else home.
fn browser_start(project: Option<&std::path::Path>, g: &vjc_io::prefs::General) -> PathBuf {
    if let Some(d) = g.browser_folder.as_ref().filter(|d| d.is_dir()) {
        return d.clone();
    }
    if let Some(d) = project.and_then(|p| p.parent()).filter(|d| d.is_dir()) {
        return d.to_path_buf();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let movies = home.join("Movies");
    if movies.is_dir() { movies } else { home }
}

/// Starts receiving a Syphon/NDI feed as a player; failures show up as the player's error.
fn open_network(protocol: vjc_net::Protocol, name: String) -> Player {
    let label = format!("net:{}:{name}", protocol.label());
    Player::open_external(&label, move |sink| vjc_net::receive(protocol, &name, sink))
}

/// Effects and generators offered by the browser. Parsed once: the list is read every
/// frame, so it must not touch the ISF JSON again.
fn browser_fx() -> &'static [browser::FxEntry] {
    static CACHE: std::sync::OnceLock<Vec<browser::FxEntry>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let fx = vjc_fx::BUILTIN.iter().map(|(id, src)| browser::FxEntry {
            name: fx_label(id),
            category: Some(
                vjc_fx::parse(src)
                    .ok()
                    .and_then(|i| i.categories.first().cloned())
                    .unwrap_or_else(|| "Other".into()),
            ),
            item: browser::Item::Effect(id.to_string()),
        });
        // Generators are already their own tab; no category.
        let gens = vjc_fx::GENERATORS.iter().map(|(id, _)| browser::FxEntry {
            name: fx_label(id),
            category: None,
            item: browser::Item::Generator(id.to_string()),
        });
        fx.chain(gens).collect()
    })
}

/// `browser_fx` plus the effect plugins the FFGL host found, in their own group.
fn browser_fx_with(ffgl: &ffgl_host::FfglHost) -> std::borrow::Cow<'static, [browser::FxEntry]> {
    let plugins = ffgl.effect_entries();
    if plugins.is_empty() {
        return std::borrow::Cow::Borrowed(browser_fx());
    }
    let mut v = browser_fx().to_vec();
    v.extend(plugins.iter().map(|e| browser::FxEntry {
        name: e.info.name.clone(),
        category: Some(ffgl_host::GROUP.to_string()),
        item: browser::Item::Effect(vjc_ffgl::effect_id(e)),
    }));
    std::borrow::Cow::Owned(v)
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "VJCraft")
}

/// `VJC_DATA_DIR` (dev/testing) replaces the per-user data and config dir.
fn data_dir_from(over: Option<std::ffi::OsString>) -> Option<PathBuf> {
    match over.filter(|o| !o.is_empty()) {
        Some(o) => Some(PathBuf::from(o)),
        None => Some(project_dirs()?.data_dir().to_path_buf()),
    }
}

/// Per-user data dir (macOS: ~/Library/Application Support/VJCraft), or `VJC_DATA_DIR`.
fn data_dir() -> Option<PathBuf> {
    data_dir_from(std::env::var_os("VJC_DATA_DIR"))
}

/// Per-user data dir + recovery.vjc
fn recovery_path() -> Option<PathBuf> {
    Some(data_dir()?.join("recovery.vjc"))
}

/// Per-user config dir (macOS: ~/Library/Application Support/VJCraft) + prefs.json
fn prefs_path() -> Option<PathBuf> {
    let dir = match std::env::var_os("VJC_DATA_DIR").filter(|o| !o.is_empty()) {
        Some(o) => PathBuf::from(o),
        None => project_dirs()?.config_dir().to_path_buf(),
    };
    Some(dir.join("prefs.json"))
}

/// Removes the recovery file (Don't Save, saved, discarded). Missing file is fine.
fn clear_recovery(path: Option<PathBuf>) {
    // The slot goes first, and the delete runs under the write lock: the watcher never
    // brings back a file the user just discarded.
    reliability::clear_slot_then(|| {
        if let Some(r) = path {
            let _ = std::fs::remove_file(r);
        }
    });
}

/// Keeps an unreadable recovery file aside (`recovery.bad.vjc`, then
/// `recovery.bad-2.vjc`, ...) instead of deleting it. Never overwrites an earlier one.
fn quarantine_recovery(r: &Path) {
    let mut dest = r.with_file_name("recovery.bad.vjc");
    let mut n = 2;
    while dest.exists() {
        dest = r.with_file_name(format!("recovery.bad-{n}.vjc"));
        n += 1;
    }
    let _ = std::fs::rename(r, dest);
}

/// Offers to restore a recovery file newer than the last saved project.
fn offer_recovery(s: &mut State) {
    offer_recovery_from(s, recovery_path());
}

fn offer_recovery_from(s: &mut State, raw: Option<PathBuf>) {
    let Some(r) = s.gate_recovery(raw) else {
        return;
    };
    if !r.exists() {
        return;
    }
    s.recovery_offer = Some(r);
}

/// `--recover` (relaunch after a crash): restores the recovery file with no prompt. The
/// project carries its screens, so the output windows reopen on the same monitors.
fn auto_recover(s: &mut State) {
    auto_recover_from(s, recovery_path());
}

/// Same persist/rehearsal gate as autosave: a rehearsal must not restore the live project.
fn auto_recover_from(s: &mut State, raw: Option<PathBuf>) {
    let Some(r) = s.gate_recovery(raw).filter(|r| r.exists()) else {
        return;
    };
    if s.restore_recovery(r) {
        s.status = "Recovered the autosaved project after a crash".into();
    }
}

impl State {
    /// Loads the recovery file `r` as an unsaved project. An unreadable file is kept aside
    /// (never destroyed). Returns whether it was restored.
    fn restore_recovery(&mut self, r: PathBuf) -> bool {
        if self.load(r.clone()) {
            self.file = None;
            self.dirty = true;
            self.project_gen += 1;
            // The file stays: a second crash before the next autosave must still find it.
            // The next autosave overwrites it; a save or clean quit clears it.
            true
        } else {
            quarantine_recovery(&r);
            false
        }
    }

    /// Restore/Discard dialog for `recovery_offer`.
    fn recovery_window(&mut self, ctx: &egui::Context) {
        let Some(r) = self.recovery_offer.clone() else {
            return;
        };
        let mut restore = None;
        widgets::dialog(ctx, "Recover project", false, |ui| {
            ui.label("VJCraft found an autosaved project from a previous session. Restore it?");
            ui.add_space(theme::SP_4);
            ui.horizontal(|ui| {
                if widgets::btn(ui, "Restore", widgets::Kind::Primary).clicked() {
                    restore = Some(true);
                }
                if widgets::btn(ui, "Discard", widgets::Kind::Danger).clicked() {
                    restore = Some(false);
                }
            });
        });
        let Some(restore) = restore else { return };
        self.recovery_offer = None;
        if restore {
            self.restore_recovery(r);
        } else {
            clear_recovery(Some(r));
        }
        if self.file.is_none()
            && !self.dirty
            && self.prefs.general.open_last_project
            && let Some(p) = self
                .prefs
                .general
                .last_project
                .clone()
                .filter(|p| p.is_file())
        {
            self.load(p);
        }
    }

    /// Relink dialog: explains, then asks for the folder and applies `relink_from`.
    fn relink_window(&mut self, ctx: &egui::Context) {
        if !self.relink_open {
            return;
        }
        let n = vjc_io::media::missing(&self.project).len();
        let mut go = false;
        let mut cancel = false;
        let esc = widgets::dialog(ctx, "Relink missing media", true, |ui| {
            ui.label(format!(
                "{n} file(s) are missing. Pick a folder to search for them by name."
            ));
            ui.add_space(theme::SP_4);
            ui.horizontal(|ui| {
                go = widgets::btn(ui, "Choose folder...", widgets::Kind::Primary).clicked();
                cancel = widgets::btn(ui, "Cancel", widgets::Kind::Secondary).clicked();
            });
        });
        if go || cancel || esc {
            self.relink_open = false;
        }
        if go
            && let Some(root) = rfd::FileDialog::new()
                .set_title("Folder to search for missing media")
                .pick_folder()
        {
            self.relink_from(&root);
        }
    }
}

/// Empty decks go without asking; a deck with clips needs a yes.
fn confirm_remove_deck(name: &str, clips: usize) -> bool {
    clips == 0
        || rfd::MessageDialog::new()
            .set_title("Remove deck")
            .set_description(format!(
                "Remove deck \"{name}\" and its {clips} clip(s)? Undo brings it back."
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
}

/// Layer operations shared by the Layer menu and the layer header's context menu:
/// duplicate, insert above / below, clear clips, content lock, fader start, mask mode.
fn layer_ops_menu(
    ui: &mut egui::Ui,
    project: &Project,
    lid: LayerId,
    deck: DeckId,
    cmds: &mut Vec<Command>,
) {
    let c = &project.composition;
    let Some(i) = c.layers.iter().position(|l| l.id == lid) else {
        return;
    };
    let layer = &c.layers[i];
    if crate::widgets::btn(ui, "Duplicate layer", crate::widgets::Kind::Secondary).clicked() {
        cmds.extend(vjc_core::duplicate_layer_command(project, lid));
        ui.close();
    }
    for (label, at) in [("Insert layer above", i), ("Insert layer below", i + 1)] {
        if crate::widgets::btn(ui, label, crate::widgets::Kind::Secondary).clicked() {
            let mut layer = new_layer(c.layers.len() + 1);
            layer.group = vjc_core::insert_group_at(project, at);
            cmds.push(Command::AddLayer { layer, index: at });
            ui.close();
        }
    }
    if crate::widgets::btn_if(
        ui,
        !layer.lock_content,
        "Clear clips",
        crate::widgets::Kind::Secondary,
    )
    .on_disabled_hover_text("The layer content is locked")
    .clicked()
    {
        cmds.push(Command::ClearLayerClips { deck, layer: lid });
        ui.close();
    }
    let mut lock = layer.lock_content;
    if crate::widgets::toggle(ui, &mut lock, "Lock content").changed() {
        cmds.push(Command::SetLayerLock {
            layer: lid,
            locked: lock,
        });
    }
    let mut fader = layer.fader_start;
    if ui
        .checkbox(&mut fader, "Fader start")
        .on_hover_text("Raising the opacity from 0 restarts the playing clip")
        .changed()
    {
        cmds.push(Command::SetLayerFaderStart {
            layer: lid,
            enabled: fader,
        });
    }
    ui.menu_button("Mask mode", |ui| {
        for mode in vjc_model::MaskMode::ALL {
            if crate::widgets::choice(ui, layer.mask_mode == mode, mode.label()).clicked() {
                cmds.push(Command::SetLayerMaskMode { layer: lid, mode });
                ui.close();
            }
        }
    });
}

/// New layer using the blend mode chosen in the preferences.
fn new_layer_with(n: usize, blend: &BlendMode) -> Layer {
    let mut l = new_layer(n);
    l.track.blend = blend.clone();
    l
}

/// Whether closing the window needs a confirmation of its own (the unsaved-changes
/// dialog already covers a dirty project).
fn quit_needs_confirm(confirm_quit: bool, dirty: bool) -> bool {
    confirm_quit && !dirty
}

fn new_layer(n: usize) -> Layer {
    Layer {
        color: None,
        folded: false,
        id: Uuid::new_v4(),
        name: format!("Layer {n}"),
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
        ignore_column_trigger: false,
        lock_content: false,
        fader_start: false,
        extras: Default::default(),
        mask_mode: Default::default(),
        audio: true,
        video: true,
        trigger_mode: vjc_model::TriggerMode::Restart,
        group: None,
    }
}

/// New project: default grid plus the user's composition and shortcut prefs.
/// Pushes the Video > Hardware decoding prefs to the decoders (clips opened afterwards).
fn apply_hw_decode(prefs: &Prefs) {
    vjc_media::configure_hw_decode(
        prefs.video.hw_decode == vjc_io::prefs::HwDecode::Auto,
        prefs.video.hw_sessions as usize,
    );
}

fn fresh_project(prefs: &Prefs) -> Project {
    let mut project = Project::new("Untitled");
    let c = &mut project.composition;
    c.width = prefs.video.width;
    c.height = prefs.video.height;
    c.fps = prefs.video.fps;
    // Layers are listed top to bottom; the bottom one is "Layer 1".
    for i in (1..=3).rev() {
        c.layers.push(new_layer(i));
    }
    for i in 1..=9 {
        c.columns.push(Column {
            color: None,
            autopilot: Default::default(),
            beat_snap: None,
            id: Uuid::new_v4(),
            name: format!("{i}"),
        });
    }
    if prefs.control.default_shortcuts {
        project.shortcuts = vjc_core::shortcuts::default_shortcuts();
    }
    project
}

impl State {
    fn new(prefs: Prefs) -> Self {
        let s = Self::build(prefs, true);
        // the plugin catalog is ready before the first audio effect is added, whether or not
        // the audio engine has opened yet
        s.start_clap_scan();
        s.start_ffgl_scan();
        s
    }

    /// State for UI tests: no MIDI/OSC threads, no monitors, no audio, and nothing written
    /// to the user's preferences or recovery file.
    #[cfg(test)]
    fn headless(prefs: Prefs) -> Self {
        Self::build(prefs, false)
    }

    /// Recovery file, only for the live app.
    fn recovery_path(&self) -> Option<PathBuf> {
        self.gate_recovery(recovery_path())
    }

    fn gate_recovery(&self, raw: Option<PathBuf>) -> Option<PathBuf> {
        if self.persist && !self.rehearsal {
            raw
        } else {
            None
        }
    }

    fn build(prefs: Prefs, live: bool) -> Self {
        apply_hw_decode(&prefs);
        vjc_media::warm_up_hw_decode();
        let project = fresh_project(&prefs);
        State {
            tex_cap: 16384,
            project,
            history: History::new(),
            warnings: vec![],
            status: "Drop video files on the grid cells.".into(),
            vram_warn: None,
            vram_dog: vram::Watchdog::default(),
            vram_limit: vram::limit_from(std::env::var("VJC_VRAM_LIMIT_MB").ok().as_deref()),
            frame_ms: frame_stats::FrameStats::new(),
            show_stats: false,
            applied_ui_scale: None,
            palette: Default::default(),
            presets_published: false,
            stats: Default::default(),
            vram_bytes: 0,
            gpu_name: String::new(),
            ui_stats: ui_stats::UiStats::new(),
            last_ui: None,
            render_gate: FrameGate::default(),
            frame_label: (None, String::new()),
            preload: preload::Preload::default(),
            frames: 0,
            players: HashMap::new(),
            audio: None,
            clap: Default::default(),
            metro: None,
            gens: HashMap::new(),
            clip_start: HashMap::new(),
            heads: HashMap::new(),
            src_info: Default::default(),
            hap: Default::default(),
            hap_ask: None,
            hap_swap: None,
            flow: Flow::default(),
            last_head_tick: std::time::Instant::now(),
            comp: None,
            fx: fx::FxLib::new(),
            ml: ml_fx::MlHub::new(),
            fx_epoch: std::time::Instant::now(),
            audio_react: {
                let mut r = audio_react::AudioReact::default();
                r.set_config(audio_react::InCfg::from_prefs(&prefs.audio));
                r
            },
            panel_tab: PanelTab::Composition,
            comp_tex: None,
            prev_tex: None,
            mon_views: Default::default(),
            xf_show: true,
            xf_drag: None,
            xf_area: egui::Rect::NOTHING,
            monitor_seen: false,
            capture: None,
            netout: Default::default(),
            artnet: Default::default(),
            artnet_scan: Default::default(),
            rec_request: None,
            output: None,
            monitors: if live { monitors::list() } else { vec![] },
            monitors_polled: (!live).then(std::time::Instant::now),
            persist: live,
            audio_failed: !live,
            clap_dirs_scanned: prefs.audio.clap_dirs.clone(),
            ffgl_dirs_scanned: prefs.video.ffgl_dirs.clone(),
            audio_cfg: audio_prefs::out_config(&prefs.audio),
            audio_error: false,
            audio_outputs: None,
            audio_inputs: None,
            // UI tests get the list at once, so their result does not depend on timing.
            rec_codecs: if live {
                vjc_media::record::Probe::start(vjc_media::record::available)
            } else {
                vjc_media::record::Probe::ready(vjc_media::record::available())
            },
            cells: vec![],
            layer_rects: vec![],
            grid_rect: egui::Rect::NOTHING,
            tabs_clip: egui::Rect::NOTHING,
            master_rect: None,
            browser: browser::Browser::new(browser_start(None, &prefs.general)),
            thumbs: clip_thumbs::ClipThumbs::new(),
            last_pointer: None,
            selected: None,
            selected_deck: None,
            layer_drag: None,
            clip_clipboard: None,
            cell_drag: None,
            preview_target: PreviewTarget::Composition,
            clip_prev: None,
            prev_live: false,
            sel_group: None,
            col_sel: None,
            learn: false,
            learn_target: None,
            learn_midi: false,
            tc: timecode_io::TcRuntime::new(),
            tc_devices: None,
            midi_ports: None,
            midi_log: io_monitor::Log::default(),
            osc_log: io_monitor::Log::default(),
            midi_monitor_open: false,
            osc_monitor_open: false,
            snap_req: None,
            snap_job: None,
            layer_bags: Default::default(),
            reveal: if live {
                Box::new(wave_ui::SystemReveal)
            } else {
                Box::<wave_ui::NoReveal>::default()
            },
            collect_job: None,
            midi: if live {
                midi_io::Midi::spawn()
            } else {
                midi_io::Midi::idle()
            },
            osc: if live {
                osc_io::Osc::spawn()
            } else {
                osc_io::Osc::idle()
            },
            osc_cfg: None,
            osc_prev: HashMap::new(),
            osc_limit: Default::default(),
            dj: if live {
                dj_io::Dj::spawn()
            } else {
                dj_io::Dj::idle()
            },
            dj_cfg: None,
            dj_iface_buf: None,
            dj_iface_edited: false,
            dj_ifaces: Default::default(),
            dj_tracker: Default::default(),
            dj_follow: Default::default(),
            dmx: if live {
                dmx_io::Dmx::spawn()
            } else {
                dmx_io::Dmx::idle()
            },
            dmx_cfg: None,
            api: if live {
                api_io::Api::spawn()
            } else {
                api_io::Api::idle()
            },
            api_cfg: None,
            snap_jobs: Vec::new(),
            snap_last: None,
            led_prev: HashMap::new(),
            targets: vec![],
            master_bypass: false,
            file: None,
            dirty: false,
            project_gen: 0,
            published_gen: 0,
            published_bpm: 0.0,
            last_publish: std::time::Instant::now() - std::time::Duration::from_secs(2),
            publish_count: 0,
            last_autosave: std::time::Instant::now(),
            prefs_open: false,
            sc_ed: Default::default(),
            recovery_offer: None,
            relink_open: false,
            toasts: Vec::new(),
            output_setup_open: false,
            sel_screen: None,
            sel_slice: None,
            sel_slices: vec![],
            out_win: None,
            guide_jobs: vec![],
            sel_pt_in: None,
            sel_pt_out: None,
            snap: false,
            fx_presets_dir: live
                .then(|| data_dir().map(|d| d.join("effect-presets")))
                .flatten(),
            presets_dir: live
                .then(|| data_dir().map(|d| d.join("output-presets")))
                .flatten(),
            preset_names: None,
            preset_sel: None,
            preset_text: String::new(),
            preset_confirm_delete: false,
            preset_warnings: vec![],
            edit_mask: false,
            sel_mask_pt: None,
            poly: Default::default(),
            closed_screens: Default::default(),
            output_windowed: false,
            rehearsal: false,
            test_cards: Default::default(),
            blackouts: Default::default(),
            pixel_grids: Default::default(),
            slice_clip: vec![],
            show_fps: false,
            identify_at: None,
            display_info_open: false,
            out_fps: HashMap::new(),
            slice_r: None,
            stamper: None,
            screen_rt: HashMap::new(),
            prefs_tab: PrefsTab::General,
            prefs_dirty: false,
            prefs,
            title: String::new(),
            clock: Clock::new(120.0, std::time::Instant::now()),
            beat: 0,
            phase: 0.0,
            snap_pending: vec![],
            origin: HashMap::new(),
            redirect: None,
            active_layer: None,
            holds: vec![],
            hold_arm: None,
            hold_token: 0,
            comp_ap: Default::default(),
            nudge_dir: 0,
            link: link_io::Link::new(120.0),
            link_sync: LinkSync::default(),
            force_panels: false,
            win_size: egui::Vec2::ZERO,
            topbars_h: 0.0,
            transport_h: theme::TRANSPORT_H,
            save_layout: None,
            overwrite_layout: None,
            remove_layer_confirm: None,
            help_shown: None,
            renaming: None,
            #[cfg(test)]
            history_now: None,
            col_renaming: None,
            col_marks: vec![],
        }
    }

    /// Starts the background scan for CLAP plugins (cached by file mtime in the data folder).
    fn start_clap_scan(&self) {
        let cache = data_dir().map(|d| d.join("clap-cache.json"));
        self.clap.rescan(&self.prefs.audio.clap_dirs, cache, false);
    }

    /// Audio output, opened lazily; a failure is reported once in the status line.
    fn audio_engine(&mut self) -> Option<AudioEngine> {
        if self.audio.is_none() && !self.audio_failed {
            match AudioEngine::start_with(self.audio_cfg.clone()) {
                Ok(e) => {
                    self.audio = Some(e);
                }
                Err(e) => {
                    self.audio_failed = true;
                    self.audio_error = true;
                    self.status = format!("Audio off: {e}");
                }
            }
        }
        self.audio.clone()
    }

    /// Forgets a layer or cell drag whose layer, column or clip no longer exists.
    fn prune_drags(&mut self) {
        let comp = &self.project.composition;
        if self
            .layer_drag
            .is_some_and(|l| !comp.layers.iter().any(|x| x.id == l))
        {
            self.layer_drag = None;
        }
        if let Some((l, c)) = self.cell_drag
            && vjc_core::clip_in(&self.project, comp.current_deck, l, c).is_none()
        {
            self.cell_drag = None;
        }
    }

    fn reset_players(&mut self) {
        self.flow = Flow::default();
        self.preload.clear();
        self.players.clear();
        self.gens.clear();
        self.history.clear();
        self.snap_pending.clear();
        self.origin.clear();
        self.holds.clear();
        self.hold_arm = None;
        self.active_layer = None;
        self.comp_ap.reset();
        self.selected = None;
        self.selected_deck = None;
        self.layer_drag = None;
        self.cell_drag = None;
        self.preview_target = PreviewTarget::Composition;
        self.clip_prev = None;
    }

    /// Asks what to do with unsaved changes. Returns false to cancel.
    fn confirm_discard(&mut self) -> bool {
        if !self.dirty {
            return true;
        }
        let r = rfd::MessageDialog::new()
            .set_title("Unsaved changes")
            .set_description("Save changes before continuing?")
            .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                "Save".into(),
                "Don't Save".into(),
                "Cancel".into(),
            ))
            .show();
        match r {
            rfd::MessageDialogResult::Custom(l) if l == "Save" => self.save(),
            rfd::MessageDialogResult::Custom(l) if l == "Don't Save" => {
                clear_recovery(self.recovery_path());
                true
            }
            _ => false,
        }
    }

    fn new_project(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        self.project = fresh_project(&self.prefs);
        self.link_sync.note_project_replaced();
        self.file = None;
        self.dirty = false;
        self.warnings.clear();
        self.reset_players();
        self.status = "New project".into();
    }

    /// Makes `l` the current layout; panels take its sizes and visibility.
    fn apply_layout(&mut self, l: Layout) {
        self.prefs.layouts.current = l;
        self.force_panels = true;
        self.prefs_dirty = true;
        self.save_prefs();
    }

    fn apply_layout_named(&mut self, name: &str) {
        if let Some(l) = self.prefs.layouts.find(name) {
            self.apply_layout(l);
            self.status = format!("Layout {name}");
        }
    }

    /// Saves the current panel sizes and visibility under `name` (custom layouts only).
    fn save_layout_as(&mut self, name: &str) {
        let l = Layout {
            name: name.trim().to_owned(),
            ..self.prefs.layouts.current.clone()
        };
        if self.prefs.layouts.save(l.clone()) {
            self.prefs.layouts.current = l.clone();
            self.status = format!("Layout {} saved", l.name);
        } else {
            self.status = "Layout name is empty or belongs to a built-in layout".into();
        }
        self.prefs_dirty = true;
        self.save_prefs();
    }

    fn delete_layout(&mut self, name: &str) {
        if self.prefs.layouts.delete(name) {
            self.status = format!("Layout {name} deleted");
            if self.prefs.layouts.current.name == name {
                self.apply_layout(Layout::default());
            } else {
                self.prefs_dirty = true;
                self.save_prefs();
            }
        }
    }

    fn set_help(&mut self, on: bool) {
        self.prefs.layouts.help = on;
        self.prefs_dirty = true;
        self.save_prefs();
    }

    /// Remembers a panel's live size in the current layout (saved on exit or layout change).
    /// Only while the user drags the panel's handle: a size clamped to the window is not
    /// written back, so the stored layout keeps its full value for a taller window.
    fn track_size(&mut self, f: impl FnOnce(&mut Layout) -> &mut f32, size: f32, dragging: bool) {
        let v = f(&mut self.prefs.layouts.current);
        if dragging && (*v - size).abs() > 0.5 && size >= 40.0 {
            *v = size;
            self.prefs_dirty = true;
        }
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        let cur = self.prefs.layouts.current.name.clone();
        let names: Vec<String> = Layout::presets()
            .into_iter()
            .chain(self.prefs.layouts.custom.iter().cloned())
            .map(|l| l.name)
            .collect();
        for n in &names {
            if crate::widgets::choice(ui, &cur == n, n).clicked() {
                ui.close();
                self.apply_layout_named(n);
            }
        }
        ui.separator();
        if crate::widgets::btn(ui, "Save layout as...", crate::widgets::Kind::Secondary).clicked() {
            ui.close();
            self.save_layout = Some(String::new());
        }
        ui.menu_button("Delete layout", |ui| {
            if self.prefs.layouts.custom.is_empty() {
                ui.label("No saved layouts");
            }
            for n in self
                .prefs
                .layouts
                .custom
                .iter()
                .map(|l| l.name.clone())
                .collect::<Vec<_>>()
            {
                if crate::widgets::btn(ui, &n, crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.delete_layout(&n);
                }
            }
        });
        ui.separator();
        if crate::widgets::btn(ui, "Quick search... Cmd+K", crate::widgets::Kind::Secondary)
            .clicked()
        {
            ui.close();
            self.palette_open();
        }
        crate::widgets::toggle(ui, &mut self.show_stats, "Show FPS & Stats");
        crate::widgets::toggle(ui, &mut self.xf_show, "Transform widget");
        ui.separator();
        let l = self.prefs.layouts.current.clone();
        let mut new = l.clone();
        for (on, label, key) in [
            (&mut new.browser, "Browser", "Cmd+1"),
            (&mut new.monitors, "Monitors", "Cmd+2"),
            (&mut new.params, "Parameters", "Cmd+3"),
            (&mut new.grid, "Clip grid", "Cmd+4"),
            (&mut new.speed_sliders, "Speed sliders", ""),
        ] {
            if ui
                .add(egui::Button::selectable(*on, label).shortcut_text(key))
                .clicked()
            {
                *on = !*on;
            }
        }
        ui.separator();
        for (key, label) in vjc_io::prefs::VIEW_TOGGLES {
            let shown = !new.is_hidden(key);
            if ui.add(egui::Button::selectable(shown, label)).clicked() {
                new.set_hidden(key, shown);
            }
        }
        ui.separator();
        let mut h = self.prefs.layouts.help;
        if ui
            .add(egui::Button::selectable(h, "Help").shortcut_text("Cmd+/"))
            .clicked()
        {
            h = !h;
            self.set_help(h);
        }
        if new != l {
            self.prefs.layouts.current = new;
            self.force_panels = true;
            self.prefs_dirty = true;
        }
    }

    /// Global black-out: every screen goes black, or all come back when any was black.
    fn toggle_blackout(&mut self) {
        let mut ids: Vec<Uuid> = self.project.output.screens.iter().map(|s| s.id).collect();
        if ids.is_empty() {
            ids.push(Uuid::nil()); // the legacy single output
        }
        self.blackouts = output_overlay::toggle_blackout(&self.blackouts, &ids);
        self.status = if self.blackouts.is_empty() {
            "Black-out off".into()
        } else {
            "Black-out ON".into()
        };
    }

    fn layout_keys(&mut self, ctx: &egui::Context) {
        use egui::Key;
        let cmd = egui::Modifiers::COMMAND;
        let l = &mut self.prefs.layouts.current;
        let before = l.clone();
        for (k, on) in [
            (Key::Num1, &mut l.browser),
            (Key::Num2, &mut l.monitors),
            (Key::Num3, &mut l.params),
            (Key::Num4, &mut l.grid),
        ] {
            if ctx.input_mut(|i| i.consume_key(cmd, k)) {
                *on = !*on;
            }
        }
        if *l != before {
            self.force_panels = true;
            self.prefs_dirty = true;
        }
        if ctx.input_mut(|i| i.consume_key(BLACKOUT_KEY.0, BLACKOUT_KEY.1)) {
            self.toggle_blackout();
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::Slash)) {
            self.set_help(!self.prefs.layouts.help);
        }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::K)) {
            if self.palette.open {
                self.palette.close();
            } else {
                self.palette_open();
            }
        }
    }

    /// Opens the quick search with a fresh list of everything it can find.
    fn palette_open(&mut self) {
        use palette::{Act, Cmd, Entry, Kind};
        let mut v = vec![];
        for (id, _) in vjc_fx::BUILTIN {
            v.push(Entry {
                kind: Kind::Effect,
                label: fx_label(id),
                act: Act::AddEffect(id.to_string()),
            });
        }
        for e in self.fx.ffgl.effect_entries() {
            v.push(Entry {
                kind: Kind::Effect,
                label: e.info.name.clone(),
                act: Act::AddEffect(vjc_ffgl::effect_id(&e)),
            });
        }
        for (id, _) in vjc_fx::GENERATORS {
            v.push(Entry {
                kind: Kind::Generator,
                label: fx_label(id),
                act: Act::Generator(id.to_string()),
            });
        }
        let comp = &self.project.composition;
        for l in &comp.layers {
            v.push(Entry {
                kind: Kind::Layer,
                label: l.name.clone(),
                act: Act::SelectLayer(l.id),
            });
        }
        if let Some(d) = comp.decks.iter().find(|d| d.id == comp.current_deck) {
            for cc in d.clips.iter().take(2000) {
                let layer = comp
                    .layers
                    .iter()
                    .find(|l| l.id == cc.layer)
                    .map_or("", |l| l.name.as_str());
                v.push(Entry {
                    kind: Kind::Clip,
                    label: format!("{} ({layer}, column {})", cc.clip.name, cc.column + 1),
                    act: Act::SelectCell(d.id, cc.layer, cc.column),
                });
            }
        }
        for (c, name) in Cmd::ALL {
            v.push(Entry {
                kind: Kind::Command,
                label: name.to_string(),
                act: Act::Command(c),
            });
        }
        self.palette = palette::Palette {
            open: true,
            entries: v,
            focus: true,
            ..Default::default()
        };
    }

    /// Where "add effect" lands now: previewed clip or layer, else the selected cell's
    /// clip, else Master.
    fn palette_target(&self) -> EffectTarget {
        let (pc, pl) = match self.preview_target {
            PreviewTarget::Clip(d, l, c) => (Some((d, l, c)), None),
            PreviewTarget::Layer(l) => (None, Some(l)),
            PreviewTarget::Composition => (None, None),
        };
        let deck = self
            .selected_deck
            .unwrap_or(self.project.composition.current_deck);
        match palette::pick_target(pc, pl, self.selected, deck) {
            palette::Pick::Clip(deck, layer, column) => EffectTarget::Clip {
                deck,
                layer,
                column,
            },
            palette::Pick::Layer(l) => EffectTarget::Layer(l),
            palette::Pick::Master => EffectTarget::Master,
        }
    }

    /// Runs a chosen entry of the quick search. Only selects, adds to a chain, fills an
    /// empty cell or runs a menu command; never triggers a clip.
    fn palette_run(&mut self, act: palette::Act) {
        use palette::{Act, Cmd};
        match act {
            Act::AddEffect(id) => match self.new_fx(&id) {
                Some(effect) => {
                    let target = self.palette_target();
                    self.exec(Command::AddEffect {
                        target,
                        effect,
                        index: usize::MAX,
                    });
                    self.status = format!("Added {}", fx_label(&id));
                }
                None => self.status = format!("Unknown effect {id}"),
            },
            Act::Generator(id) => {
                let deck = self
                    .selected_deck
                    .unwrap_or(self.project.composition.current_deck);
                match self.selected {
                    Some((layer, column))
                        if vjc_core::clip_in(&self.project, deck, layer, column).is_none() =>
                    {
                        self.exec(Command::SetGenerator {
                            deck,
                            layer,
                            column,
                            kind: id.clone(),
                            name: fx_label(&id),
                            params: vjc_fx::generator_params(&id).unwrap_or_default(),
                        });
                    }
                    Some(_) => self.status = "Select an empty cell for the generator".into(),
                    None => self.status = "Select a cell first".into(),
                }
            }
            Act::SelectLayer(l) => self.preview_target = PreviewTarget::Layer(l),
            Act::SelectCell(d, l, c) => {
                self.selected = Some((l, c));
                self.selected_deck = Some(d);
                self.preview_target = PreviewTarget::Clip(d, l, c);
            }
            Act::Command(c) => match c {
                Cmd::Undo => self.undo(),
                Cmd::Redo => self.redo(),
                Cmd::Blackout => self.toggle_blackout(),
                Cmd::ToggleStats => self.show_stats = !self.show_stats,
                Cmd::Preferences => self.prefs_open = true,
                Cmd::MidiMonitor => self.midi_monitor_open = true,
                Cmd::OscMonitor => self.osc_monitor_open = true,
                Cmd::Shortcuts => self.sc_ed.open = true,
            },
        }
    }

    /// The quick search box: type, Up/Down, Enter or click runs, Esc closes.
    fn palette_window(&mut self, ctx: &egui::Context) {
        if !self.palette.open {
            return;
        }
        use egui::Key;
        const SHOWN: usize = 12;
        let hits = palette::rank(&self.palette.entries, &self.palette.query, SHOWN);
        let none = egui::Modifiers::NONE;
        // Arrows and Enter belong to the palette only while nothing else has the focus.
        let query_id = egui::Id::new("quick-search-query");
        let mine = palette_owns_keys(ctx.memory(|m| m.focused()), query_id);
        let (down, up, enter, esc) = ctx.input_mut(|i| {
            (
                mine && i.consume_key(none, Key::ArrowDown),
                mine && i.consume_key(none, Key::ArrowUp),
                mine && i.consume_key(none, Key::Enter),
                i.consume_key(none, Key::Escape),
            )
        });
        if down && !hits.is_empty() {
            self.palette.sel = (self.palette.sel + 1) % hits.len();
        }
        if up && !hits.is_empty() {
            self.palette.sel = (self.palette.sel + hits.len() - 1) % hits.len();
        }
        self.palette.sel = self.palette.sel.min(hits.len().saturating_sub(1));
        let mut run = None;
        if enter {
            run = hits.get(self.palette.sel).copied();
        }
        let target = match self.palette_target() {
            EffectTarget::Clip { .. } => "the selected clip",
            EffectTarget::Layer(_) => "the selected layer",
            _ => "Master",
        };
        let mut changed = false;
        widgets::panel_window(ctx, "Quick search")
            .id(egui::Id::new("quick-search"))
            .title_bar(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 80.0])
            .fixed_size([460.0, 0.0])
            .show(ctx, |ui| {
                let e = ui.add(
                    egui::TextEdit::singleline(&mut self.palette.query)
                        .id(query_id)
                        .hint_text("Search effects, generators, layers, clips, commands")
                        .desired_width(f32::INFINITY),
                );
                changed = e.changed();
                if self.palette.focus {
                    e.request_focus();
                    self.palette.focus = false;
                }
                widgets::help(ui, &format!("Effects are added to {target}."));
                ui.separator();
                if hits.is_empty() {
                    widgets::help(ui, "No match.");
                }
                for (row, i) in hits.iter().enumerate() {
                    let en = &self.palette.entries[*i];
                    let text = format!("{}   {}", en.label, en.kind.label());
                    if ui.selectable_label(row == self.palette.sel, text).clicked() {
                        run = Some(*i);
                    }
                }
            });
        if changed {
            self.palette.sel = 0;
        }
        if let Some(i) = run {
            let act = self.palette.entries[i].act.clone();
            self.palette.close();
            self.palette_run(act);
        } else if esc {
            self.palette.close();
        }
    }

    fn save_layout_window(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.save_layout.take() else {
            return;
        };
        let (mut keep, mut save) = (true, false);
        let esc = widgets::dialog(ctx, "Save layout as", true, |ui| {
            let r = widgets::field_text(ui, &mut name, widgets::Width::Lg, None);
            r.request_focus();
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                if widgets::btn_if(ui, !name.trim().is_empty(), "Save", widgets::Kind::Primary)
                    .clicked()
                    || enter
                {
                    save = true;
                }
                if widgets::btn(ui, "Cancel", widgets::Kind::Secondary).clicked() {
                    keep = false;
                }
            });
        });
        if esc {
            keep = false;
        }
        if save && !name.trim().is_empty() {
            let n = name.trim();
            if self.prefs.layouts.custom.iter().any(|l| l.name == n) {
                // Existing custom layout: ask before replacing it.
                self.overwrite_layout = Some(n.to_owned());
            } else {
                self.save_layout_as(&name);
            }
        } else if keep {
            self.save_layout = Some(name);
        }
    }

    /// Removes `layer` with its clips (undoable). A layer that is playing asks first.
    fn request_remove_layer(&mut self, layer: LayerId) {
        if self.active(layer).is_some() {
            self.remove_layer_confirm = Some(layer);
        } else {
            self.remove_layer_now(layer);
        }
    }

    fn remove_layer_now(&mut self, layer: LayerId) {
        if self.exec_ok(Command::RemoveLayerWithClips { layer }) {
            if self.selected.is_some_and(|s| s.0 == layer) {
                self.selected = None;
                self.selected_deck = None;
            }
            if self.preview_target == PreviewTarget::Layer(layer) {
                self.preview_target = PreviewTarget::Composition;
            }
        }
    }

    /// Convert to HAP: the confirm dialog, the running job's status and the swap offer.
    fn hap_windows(&mut self, ctx: &egui::Context) {
        use hap_convert::{Finished, Request, Variant};
        if self.hap.registry.is_none()
            && let Some(dir) = prefs_path().and_then(|p| p.parent().map(PathBuf::from))
        {
            let reg = dir.join("hap-parts.txt");
            hap_convert::Registry(reg.clone()).clean_stale();
            self.hap.registry = Some(reg);
        }
        if let Some(src) = self.browser.convert_req.take() {
            self.hap_ask = Some(HapAsk {
                src,
                cell: None,
                clip: None,
                variant: None,
            });
        }
        match self.hap.poll() {
            Some(Finished::Done { req, out }) => {
                self.status = format!("HAP ready: {}", out.display());
                if req.cell.is_some() {
                    self.hap_swap = Some((req, out));
                }
            }
            Some(Finished::Failed(msg)) => self.status = format!("Convert to HAP: {msg}"),
            Some(Finished::Cancelled) => self.status = "Convert to HAP cancelled".into(),
            None => {}
        }
        if self.hap.busy()
            && let Some((name, p, queued)) = self.hap.status()
        {
            let more = if queued > 0 {
                format!(" (+{queued} waiting)")
            } else {
                String::new()
            };
            self.status = format!("Converting {name} to HAP: {:.0}%{more}", p * 100.0);
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            let mut stop = false;
            egui::Window::new("Convert to HAP")
                .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -40.0])
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(&name);
                    ui.add(egui::ProgressBar::new(p).show_percentage());
                    stop = widgets::btn(ui, "Cancel", widgets::Kind::Secondary).clicked();
                });
            if stop {
                self.hap.cancel_current();
            }
        }
        if let Some(ask) = &mut self.hap_ask {
            let info = self.src_info.get(&ask.src).map(|i| i.cloned());
            let general = &self.prefs.general;
            let dir = general
                .hap_folder
                .clone()
                .or_else(|| ask.src.parent().map(PathBuf::from))
                .unwrap_or_default();
            let space = hap_convert::free_space(&dir);
            let (mut go, mut no) = (false, false);
            let name = ask
                .src
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let esc = widgets::dialog(ctx, "Convert to HAP", true, |ui| {
                ui.label(format!(
                    "{name}\nWrites a .hap.mov file; the original is kept."
                ));
                match &info {
                    None => {
                        ui.weak("Reading the file...");
                        no = widgets::btn(ui, "Cancel", widgets::Kind::Secondary).clicked();
                    }
                    Some(None) => {
                        ui.colored_label(theme::ERROR_TEXT, "Cannot read this file.");
                        no = widgets::btn(ui, "Close", widgets::Kind::Secondary).clicked();
                    }
                    Some(Some(i)) => {
                        let v = ask.variant.unwrap_or(if i.alpha {
                            Variant::HapAlpha
                        } else {
                            Variant::Hap
                        });
                        ask.variant = Some(v);
                        ui.horizontal(|ui| {
                            for o in Variant::ALL {
                                if ui.selectable_label(v == o, o.label()).clicked() {
                                    ask.variant = Some(o);
                                }
                            }
                        });
                        let est = hap_convert::estimate_bytes(
                            i.width,
                            i.height,
                            i.fps,
                            i.duration,
                            ask.variant.unwrap_or(v),
                        );
                        let free = space.map_or_else(
                            || "free space unknown".to_string(),
                            |s| format!("{} free", hap_convert::human(s.free)),
                        );
                        ui.label(format!("~{}, {free}", hap_convert::human(est)));
                        if !i.alpha && ask.variant == Some(Variant::HapAlpha) {
                            ui.weak("The source has no alpha channel.");
                        }
                        ui.horizontal(|ui| {
                            go = widgets::btn(ui, "Convert", widgets::Kind::Primary).clicked();
                            no = widgets::btn(ui, "Cancel", widgets::Kind::Secondary).clicked();
                        });
                    }
                }
            });
            if go && let Some(Some(i)) = info {
                let req = Request {
                    src: ask.src.clone(),
                    variant: ask.variant.unwrap_or(Variant::Hap),
                    cell: ask.cell,
                    clip: ask.clip,
                    out_dir: general.hap_folder.clone(),
                    reserve_gb: general.hap_reserve_gb,
                    width: i.width,
                    height: i.height,
                    fps: i.fps,
                    duration: i.duration,
                };
                self.hap_ask = None;
                match self.hap.submit(req) {
                    Ok(()) => self.status = "Convert to HAP queued".into(),
                    Err(e) => self.status = e,
                }
            } else if no || esc {
                self.hap_ask = None;
            }
        }
        if let Some((req, out)) = self.hap_swap.clone() {
            let (mut yes, mut no) = (false, false);
            let esc = widgets::dialog(ctx, "HAP file ready", true, |ui| {
                ui.label(format!(
                    "{}\nUse it in this clip instead of the original? You can undo this.",
                    out.display()
                ));
                ui.horizontal(|ui| {
                    yes = widgets::btn(ui, "Swap clip", widgets::Kind::Primary).clicked();
                    no = widgets::btn(ui, "Keep original", widgets::Kind::Secondary).clicked();
                });
            });
            if yes {
                if let Some((deck, layer, column)) = req.cell {
                    let found = vjc_core::clip_in(&self.project, deck, layer, column).map(|c| {
                        let path = match &c.source {
                            vjc_model::Source::File { path } => Some(path.as_path()),
                            _ => None,
                        };
                        (c.id, path)
                    });
                    match hap_convert::swap_check(found, &req) {
                        Ok(()) => self.exec(Command::SetMediaPaths {
                            changes: vec![(deck, layer, column, out)],
                        }),
                        Err(e) => self.status = e,
                    }
                }
                self.hap_swap = None;
            } else if no || esc {
                self.hap_swap = None;
            }
        }
    }

    fn remove_layer_modal(&mut self, ctx: &egui::Context) {
        let Some(layer) = self.remove_layer_confirm else {
            return;
        };
        let name = self
            .project
            .composition
            .layers
            .iter()
            .find(|l| l.id == layer)
            .map(|l| l.name.clone());
        let Some(name) = name else {
            self.remove_layer_confirm = None;
            return;
        };
        let (mut yes, mut no) = (false, false);
        let esc = widgets::dialog(ctx, "Remove layer?", true, |ui| {
            ui.label(format!(
                "\"{name}\" is playing. Remove it with all its clips? You can undo this."
            ));
            ui.horizontal(|ui| {
                yes = widgets::btn(ui, "Yes", widgets::Kind::Danger).clicked();
                no = widgets::btn(ui, "No", widgets::Kind::Secondary).clicked();
            });
        });
        if yes {
            self.remove_layer_confirm = None;
            self.remove_layer_now(layer);
        } else if no || esc {
            self.remove_layer_confirm = None;
        }
    }

    fn overwrite_layout_window(&mut self, ctx: &egui::Context) {
        let Some(name) = self.overwrite_layout.clone() else {
            return;
        };
        let (mut yes, mut no) = (false, false);
        let esc = widgets::dialog(ctx, "Replace layout?", true, |ui| {
            ui.label(format!("Layout \"{name}\" already exists. Replace it?"));
            ui.horizontal(|ui| {
                yes = widgets::btn(ui, "Replace", widgets::Kind::Danger).clicked();
                no = widgets::btn(ui, "Cancel", widgets::Kind::Secondary).clicked();
            });
        });
        no |= esc;
        if yes {
            self.save_layout_as(&name);
        }
        if yes || no {
            self.overwrite_layout = None;
        }
    }

    fn help_bar(&mut self, ui: &mut egui::Ui) {
        let shown = self.help_shown.and_then(help::text).unwrap_or(help::IDLE);
        egui::Panel::bottom("help_bar").show(ui, |ui| {
            ui.label(shown);
        });
    }

    /// Animation presets: answers the param popups' requests (save, delete, export, import)
    /// and keeps the list they show in step with the prefs.
    fn presets_frame(&mut self, ctx: &egui::Context) {
        let mut changed = !self.presets_published;
        for r in params::take_preset_reqs(ctx) {
            match r {
                params::PresetReq::Save(name, a) => {
                    if self.prefs.presets.save_animation(&name, a) {
                        self.status = format!("Saved animation preset {name}");
                        changed = true;
                    } else {
                        self.status = "Animation preset not saved (empty name or list full)".into();
                    }
                }
                params::PresetReq::Delete(name) => {
                    self.prefs.presets.remove_animation(&name);
                    changed = true;
                }
                params::PresetReq::Export => self.export_presets(),
                params::PresetReq::Import => changed |= self.import_presets(),
            }
        }
        if changed {
            self.prefs_dirty = true;
            self.presets_published = true;
            let list = self
                .prefs
                .presets
                .animations
                .iter()
                .map(|p| (p.name.clone(), p.animation.clone()))
                .collect();
            params::publish_presets(ctx, std::sync::Arc::new(list));
        }
    }

    fn export_presets(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("VJCraft presets", &["vjcpresets"])
            .set_file_name("presets.vjcpresets")
            .save_file()
        else {
            return;
        };
        self.status = match std::fs::write(&path, self.prefs.presets.to_json()) {
            Ok(()) => format!("Exported presets to {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    /// True when presets were added.
    fn import_presets(&mut self) -> bool {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("VJCraft presets", &["vjcpresets", "json"])
            .pick_file()
        else {
            return false;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Import failed: {e}");
                return false;
            }
        };
        match self.prefs.presets.import_json(&text) {
            Ok(n) => {
                self.status = format!("Imported {n} presets");
                true
            }
            Err(e) => {
                self.status = format!("Import failed: {e}");
                false
            }
        }
    }

    /// "Presets" menu above a dashboard: save the dials (names, ranges, values) under a
    /// name, or apply a saved set to this dashboard.
    fn dash_presets_menu(
        &mut self,
        ui: &mut egui::Ui,
        dial_ref: vjc_core::DialRef,
        list: &[vjc_model::Param],
        vals: &[f64],
        set_dial: &dyn Fn(usize, f64) -> Command,
        cmds: &mut Vec<Command>,
    ) {
        ui.menu_button("Dashboard presets", |ui| {
            let saved = self.prefs.presets.dashboards.clone();
            if saved.is_empty() {
                ui.weak("No saved dashboards");
            }
            for p in &saved {
                ui.horizontal(|ui| {
                    if crate::widgets::btn(ui, &p.name, crate::widgets::Kind::Secondary).clicked() {
                        for (i, d) in p.dials.iter().enumerate().take(vals.len()) {
                            cmds.push(Command::SetDialInfo {
                                owner: dial_ref,
                                dial: i as u32,
                                name: d.name.clone(),
                                range: d.range,
                            });
                            cmds.push(set_dial(i, d.value));
                        }
                        ui.close();
                    }
                    if crate::icons::button(ui, icons::i::X, "Delete preset").clicked() {
                        self.prefs.presets.remove_dashboard(&p.name);
                        self.prefs_dirty = true;
                    }
                });
            }
            ui.separator();
            let key = egui::Id::new("dash-preset-name");
            let mut buf: String = ui.data(|d| d.get_temp(key)).unwrap_or_default();
            crate::widgets::field_text_hint(ui, &mut buf, 140.0, "Preset name");
            if crate::widgets::btn_if(
                ui,
                !buf.trim().is_empty(),
                "Save dashboard",
                crate::widgets::Kind::Secondary,
            )
            .clicked()
            {
                let dials = vals
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let (name, range) = list.get(i).map_or_else(
                            || (format!("Dial {}", i + 1), (0.0, 1.0)),
                            |p| (p.name.clone(), p.range.unwrap_or((0.0, 1.0))),
                        );
                        vjc_io::param_presets::DialSlot {
                            name,
                            value: *v,
                            range,
                        }
                    })
                    .collect();
                if self.prefs.presets.save_dashboard(&buf, dials) {
                    self.prefs_dirty = true;
                    self.status = format!("Saved dashboard preset {}", buf.trim());
                    buf.clear();
                }
            }
            ui.data_mut(|d| d.insert_temp(key, buf));
        });
    }

    /// New or removed plugin folders: scan again. While a scan runs this does nothing and the
    /// per-frame call retries once it has finished.
    fn sync_clap_dirs(&mut self) {
        if self.prefs.audio.clap_dirs == self.clap_dirs_scanned {
            return;
        }
        let cache = self
            .persist
            .then(|| data_dir().map(|d| d.join("clap-cache.json")))
            .flatten();
        if self.clap.rescan(&self.prefs.audio.clap_dirs, cache, false) {
            self.clap_dirs_scanned = self.prefs.audio.clap_dirs.clone();
        }
    }

    /// Starts the background scan for FFGL plugins in the saved folders (cached by mtime).
    fn start_ffgl_scan(&self) {
        let cache = data_dir().map(|d| d.join("ffgl-cache.json"));
        self.fx
            .ffgl
            .rescan(&self.prefs.video.ffgl_dirs, cache, false);
    }

    /// New or removed FFGL folders: scan again (retried per frame while a scan runs).
    fn sync_ffgl_dirs(&mut self) {
        if self.prefs.video.ffgl_dirs == self.ffgl_dirs_scanned {
            return;
        }
        let cache = self
            .persist
            .then(|| data_dir().map(|d| d.join("ffgl-cache.json")))
            .flatten();
        if self
            .fx
            .ffgl
            .rescan(&self.prefs.video.ffgl_dirs, cache, false)
        {
            self.ffgl_dirs_scanned = self.prefs.video.ffgl_dirs.clone();
        }
    }

    /// A new effect by browser/palette id: a bundled one or an FFGL plugin.
    fn new_fx(&self, id: &str) -> Option<vjc_model::Effect> {
        if ffgl_host::is_kind(id) {
            self.fx.ffgl.new_effect(id)
        } else {
            vjc_fx::new_effect(id)
        }
    }

    fn save_prefs(&mut self) {
        // Out-of-range wishes (rate, buffer, channels) never reach the engine.
        self.prefs.sanitize();
        // Output device, rate, buffer and channels reopen the running output in place;
        // the analysis input restarts with its new device, channel and FFT size.
        let new_out = audio_prefs::out_config(&self.prefs.audio);
        let engine = self.audio.as_ref().map(|e| e as &dyn audio_prefs::Output);
        if audio_prefs::apply(&mut self.audio_cfg, new_out, engine) != audio_prefs::Applied::Same {
            self.metro = None;
            // Audio that failed to open is given another try with the new settings.
            if self.audio.is_none() && self.audio_error {
                self.audio_failed = false;
                self.audio_error = false;
            }
        }
        self.audio_react
            .set_config(audio_react::InCfg::from_prefs(&self.prefs.audio));
        self.sync_clap_dirs();
        self.sync_ffgl_dirs();
        let Some(p) = self.persist.then(prefs_path).flatten() else {
            self.prefs_dirty = false;
            return;
        };
        if let Err(e) = self.prefs.save(&p) {
            self.status = format!("Saving preferences failed: {e}");
        }
        self.prefs_dirty = false;
    }

    /// Old project without shortcuts: offers the default preset (not undoable, marks dirty).
    fn offer_shortcuts(&mut self) {
        if !self.prefs.control.offer_shortcuts || !self.project.shortcuts.is_empty() {
            return;
        }
        let ask = rfd::MessageDialog::new()
            .set_title("Keyboard shortcuts")
            .set_description("This project has no shortcuts. Apply the default preset?")
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if ask == rfd::MessageDialogResult::Yes {
            self.project.shortcuts = vjc_core::shortcuts::default_shortcuts();
            self.dirty = true;
            self.project_gen += 1;
        }
    }

    fn open_project(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("VJCraft project", &["vjc"])
            .pick_file()
        else {
            return;
        };
        self.load(path);
    }

    fn load(&mut self, path: PathBuf) -> bool {
        match vjc_io::open(&path) {
            Ok((mut project, warnings)) => {
                vjc_core::mapping::normalize_output(&mut project.output);
                self.project = project;
                self.link_sync.note_project_replaced();
                // Shortcut migration is shown once, as a toast.
                if let Some(w) = warnings.iter().find(|w| w.starts_with("Shortcuts: ")) {
                    self.toasts.push((widgets::Level::Warn, w.clone()));
                }
                self.warnings = warnings;
                self.status = format!("Opened {}", path.display());
                self.prefs.general.last_project = Some(path.clone());
                self.prefs.general.push_recent(&path);
                self.save_prefs();
                self.file = Some(path);
                self.dirty = false;
                self.reset_players();
                self.offer_shortcuts();
                true
            }
            Err(e) => {
                self.notify(widgets::Level::Error, format!("Open failed: {e}"));
                false
            }
        }
    }

    /// Status bar text plus a toast (drawn next frame).
    fn notify(&mut self, level: widgets::Level, msg: String) {
        self.status.clone_from(&msg);
        self.toasts.push((level, msg));
    }

    /// File > Relink missing media: asks for a folder, then applies `relink_from`.
    fn relink_media(&mut self) {
        if vjc_io::media::missing(&self.project).is_empty() {
            self.status = "No missing media".into();
            return;
        }
        self.relink_open = true;
    }

    /// Relinks every missing file found by name under `root` in one undoable command and
    /// reports what stayed unmatched.
    fn relink_from(&mut self, root: &std::path::Path) {
        let plan = vjc_io::media::plan_relink(&self.project, root, &Default::default());
        let found = plan.changes.len();
        if found > 0 {
            self.exec(Command::SetMediaPaths {
                changes: plan.changes,
            });
        }
        let amb = plan.ambiguous.len();
        self.status = if plan.unmatched.is_empty() && amb == 0 {
            format!("Relinked {found} clip(s)")
        } else if plan.unmatched.is_empty() {
            format!(
                "Relinked {found} clip(s); {amb} file(s) skipped: several candidates share the name"
            )
        } else {
            let names: Vec<String> = plan
                .unmatched
                .iter()
                .map(|p| {
                    p.file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into())
                })
                .collect();
            let more = if amb > 0 {
                format!("; {amb} skipped as ambiguous")
            } else {
                String::new()
            };
            format!(
                "Relinked {found} clip(s); not found: {}{more}",
                names.join(", ")
            )
        };
    }

    /// File > Collect media: copies every referenced file into `<project dir>/media`.
    fn collect_media(&mut self) {
        let Some(dir) = self
            .file
            .as_ref()
            .and_then(|f| f.parent())
            .map(|d| d.to_path_buf())
        else {
            self.status = "Save the project first, then collect its media".into();
            return;
        };
        self.collect_into(&dir);
    }

    /// Starts collecting on a worker thread so a big video never freezes the UI; progress
    /// shows in the status bar and `collect_pump` applies the result as one undoable command.
    fn collect_into(&mut self, dir: &std::path::Path) {
        if self.collect_job.is_some() {
            self.status = "Collect media is already running".into();
            return;
        }
        let snapshot = self.project.clone();
        let before: HashMap<(DeckId, LayerId, u32), PathBuf> = snapshot
            .composition
            .decks
            .iter()
            .flat_map(|d| {
                d.clips.iter().filter_map(move |c| match &c.clip.source {
                    vjc_model::Source::File { path } => {
                        Some(((d.id, c.layer, c.column), path.clone()))
                    }
                    _ => None,
                })
            })
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = cancel.clone();
        let dir = dir.to_path_buf();
        self.status = "Collecting media...".into();
        std::thread::spawn(move || {
            let progress = tx.clone();
            let r = vjc_io::media::collect_with(&snapshot, &dir, &flag, &move |i, t| {
                let _ = progress.send(CollectMsg::Progress(i, t));
                wake::now();
            });
            let _ = tx.send(CollectMsg::Done(r));
            wake::now();
        });
        self.collect_job = Some(CollectJob { rx, cancel, before });
    }

    /// Drains the collect worker's messages (once per frame, never blocking).
    fn collect_pump(&mut self) {
        let Some(job) = &self.collect_job else { return };
        let mut done = None;
        let mut lost = false;
        loop {
            match job.rx.try_recv() {
                Ok(CollectMsg::Progress(i, t)) => {
                    self.status = format!("Collecting media: {i}/{t}");
                }
                Ok(CollectMsg::Done(r)) => done = Some(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    lost = done.is_none();
                    break;
                }
            }
        }
        if lost {
            // The worker died without reporting: drop the job so a new collect can start.
            self.collect_job = None;
            self.status = "Collect media stopped unexpectedly; paths were not changed".into();
            return;
        }
        let Some(r) = done else { return };
        let job = self.collect_job.take().expect("job present");
        let (copied, skipped, failed) = (r.copied, r.skipped, r.failed.len());
        // Cells edited while copying keep what the user did: only those still holding the
        // path that was collected are rewritten.
        let changes: Vec<_> = r
            .changes
            .into_iter()
            .filter(|(d, l, c, _)| {
                let now =
                    vjc_core::clip_in(&self.project, *d, *l, *c).and_then(|k| match &k.source {
                        vjc_model::Source::File { path } => Some(path),
                        _ => None,
                    });
                now.is_some() && now == job.before.get(&(*d, *l, *c))
            })
            .collect();
        if !changes.is_empty() {
            self.exec(Command::SetMediaPaths { changes });
        }
        self.status = if r.cancelled {
            format!("Collect media cancelled: {copied} copied")
        } else {
            format!("Collected media: {copied} copied, {skipped} already there, {failed} failed")
        };
    }

    /// Saves to the current file or asks for one. Returns true when saved.
    fn save(&mut self) -> bool {
        match self.file.clone() {
            Some(p) => self.save_to(p),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> bool {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("VJCraft project", &["vjc"])
            .set_file_name(format!("{}.vjc", self.project.composition.name))
            .save_file()
        else {
            return false;
        };
        if path.extension().is_none() {
            path.set_extension("vjc");
        }
        self.save_to(path)
    }

    /// Runs `f` on the project with the effective tempo (possibly from Link) in it, so
    /// files hold what is heard. The in-memory project is restored afterwards: saving
    /// never changes it without a command and never touches the dirty flag or undo.
    fn with_effective_bpm<R>(&mut self, f: impl FnOnce(&vjc_model::Project) -> R) -> R {
        let prev = self.project.composition.tempo.bpm;
        self.project.composition.tempo.bpm = self.clock.bpm();
        let r = f(&self.project);
        self.project.composition.tempo.bpm = prev;
        r
    }

    fn save_to(&mut self, path: PathBuf) -> bool {
        match self.with_effective_bpm(|p| vjc_io::save(p, &path)) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.prefs.general.last_project = Some(path.clone());
                self.prefs.general.push_recent(&path);
                self.save_prefs();
                self.file = Some(path);
                self.dirty = false;
                clear_recovery(self.recovery_path());
                true
            }
            Err(e) => {
                self.notify(widgets::Level::Error, format!("Save failed: {e}"));
                false
            }
        }
    }

    /// The supervisor died (SIGKILL): do what a clean quit does (recovery file, prefs,
    /// recording trailer, outputs go with the process) and exit instead of playing on.
    fn quit_orphaned(&mut self) -> ! {
        reliability::ORPHAN_QUITTING.store(true, std::sync::atomic::Ordering::SeqCst);
        if self.persist {
            let every = std::time::Duration::from_secs(self.prefs.general.autosave_secs.into());
            let gap = every + std::time::Duration::from_secs(1);
            self.last_autosave = std::time::Instant::now()
                .checked_sub(gap)
                .unwrap_or(self.last_autosave);
            self.autosave();
        }
        eframe::App::on_exit(self);
        std::process::exit(0);
    }

    /// Every `autosave_secs` while dirty (if enabled), writes the recovery file.
    fn autosave(&mut self) {
        let path = self.recovery_path();
        self.autosave_to(path);
    }

    /// `autosave` with an explicit recovery file (`None` = nowhere to write).
    fn autosave_to(&mut self, path: Option<PathBuf>) {
        let every = std::time::Duration::from_secs(self.prefs.general.autosave_secs.into());
        // While the recovery offer is open, the file on disk is the user's only copy.
        if self.rehearsal
            || self.recovery_offer.is_some()
            || !self.prefs.general.autosave
            || !self.dirty
            || self.last_autosave.elapsed() < every
        {
            return;
        }
        self.last_autosave = std::time::Instant::now();
        if let Some(r) = path {
            if let Some(d) = r.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let encoded = self.with_effective_bpm(|p| vjc_io::encode(p, &r));
            match encoded {
                Ok(bytes) => {
                    // Publish before writing: the slot is never older than the file.
                    if self.persist {
                        self.note_published();
                        reliability::slot_set(Some(reliability::Snapshot {
                            path: r.clone(),
                            bytes: bytes.clone(),
                        }));
                    }
                    if let Err(e) = reliability::write_recovery(&r, &bytes) {
                        self.status = format!("Autosave failed: {e}");
                    }
                }
                Err(e) => self.status = format!("Autosave failed: {e}"),
            }
        }
    }

    fn note_published(&mut self) {
        self.published_gen = self.project_gen;
        self.published_bpm = self.clock.bpm();
        self.last_publish = std::time::Instant::now();
    }

    /// Child mode, every frame: keeps the watcher's recovery slot current, so the process
    /// can leave a recovery file even if this thread never runs again. Same gates as
    /// `autosave_to` minus the autosave interval; re-encodes at most once a second and
    /// only when the project changed.
    /// Publishes the recovery snapshot; `Some(wait)` when a change is held back by the
    /// throttle, so the caller repaints in time to publish it even if nothing else does.
    fn publish_recovery(&mut self) -> Option<std::time::Duration> {
        let path = self.recovery_path();
        self.publish_recovery_to(path)
    }

    fn publish_recovery_to(&mut self, path: Option<PathBuf>) -> Option<std::time::Duration> {
        let Some(r) = path.filter(|_| {
            self.persist
                && !self.rehearsal
                && self.recovery_offer.is_none()
                && self.prefs.general.autosave
                && self.dirty
        }) else {
            reliability::slot_set(None);
            // Whatever the gates reopen on must publish again.
            self.published_gen = u64::MAX;
            return None;
        };
        let bpm = self.clock.bpm();
        let changed = self.project_gen != self.published_gen || bpm != self.published_bpm;
        if !changed {
            return None;
        }
        let gap = std::time::Duration::from_secs(1);
        if let Some(wait) = gap.checked_sub(self.last_publish.elapsed()) {
            return Some(wait);
        }
        match self.with_effective_bpm(|p| vjc_io::encode(p, &r)) {
            Ok(bytes) => {
                self.publish_count += 1;
                reliability::slot_set(Some(reliability::Snapshot { path: r, bytes }));
                self.note_published();
                None
            }
            // Retry after the gap rather than marking this change as published.
            Err(_) => {
                self.last_publish = std::time::Instant::now();
                Some(gap)
            }
        }
    }

    fn window_title(&self) -> String {
        let name = match &self.file {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            None => "Untitled".into(),
        };
        format!(
            "{name}{} - VJCraft",
            if self.dirty { " \u{2022}" } else { "" }
        )
    }

    fn import(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Composition", &["avc"])
            .pick_file()
        else {
            return;
        };
        match vjc_avc::import_file(&path) {
            Ok(i) => {
                self.status = format!("Imported {}", path.display());
                self.warnings = i.warnings;
                self.project = i.project;
                self.link_sync.note_project_replaced();
                self.history.clear();
                self.snap_pending.clear();
                self.comp_ap.reset();
                self.flow = Flow::default();
                self.players.clear();
                self.gens.clear();
                self.file = None;
                self.dirty = true;
                self.project_gen += 1;
            }
            Err(e) => self.status = format!("Import failed: {e}"),
        }
    }

    /// Sends a command through the bus and reacts to the resulting events.
    /// Runs a command; a rejection is reported in the status line.
    fn exec(&mut self, cmd: Command) {
        // A file imported by API / MCP / MIDI starts with the Defaults prefs too.
        let cmd = match cmd {
            Command::ImportMedia {
                deck,
                layer,
                column,
                path,
            } => {
                let mut clip = vjc_core::clip_from_path(path);
                self.prefs.defaults.apply(&mut clip);
                Command::SetClip {
                    deck,
                    layer,
                    column,
                    clip,
                }
            }
            c => c,
        };
        let cmd = self.resolve_trigger(cmd);
        if let Some(cmd) = self.snap_defer(cmd) {
            self.exec_ok(cmd);
        }
    }

    /// What is live now, for toggle / target resolution.
    fn trigger_ctx(&self) -> vjc_core::TriggerCtx {
        vjc_core::TriggerCtx {
            selected: self.active_layer,
            playing: self.playing(),
        }
    }

    /// Clips in the air: (layer they play on, deck, layer of their cell, column).
    fn playing(&self) -> Vec<(LayerId, DeckId, LayerId, u32)> {
        let cell = |l: &LayerId, d: DeckId, c: u32| (*l, d, src_layer(&self.origin, *l, d, c), c);
        self.players
            .iter()
            .map(|(l, v)| cell(l, v.0, v.1))
            .chain(self.gens.iter().map(|(l, v)| cell(l, v.0, v.1)))
            .collect()
    }

    /// A user trigger becomes an eject (toggle on the clip in the air) or a redirect (clip
    /// target); anything else passes through.
    fn resolve_trigger(&self, cmd: Command) -> Command {
        vjc_core::resolve_trigger(&self.project, &self.trigger_ctx(), cmd)
    }

    /// With beat snap on, a clip or column trigger that arrives off the grid waits in
    /// `snap_pending` for the next beat or bar; anything else passes through.
    fn snap_defer(&mut self, cmd: Command) -> Option<Command> {
        self.snap_defer_tagged(cmd, 0)
    }

    fn snap_defer_tagged(&mut self, cmd: Command, token: u64) -> Option<Command> {
        if !matches!(
            cmd,
            Command::TriggerClip { .. }
                | Command::TriggerClipOn { .. }
                | Command::TriggerColumn { .. }
        ) {
            return Some(cmd);
        }
        let phase = self.clock.phase(std::time::Instant::now());
        let snap = vjc_core::effective_beat_snap(&self.project, self.prefs.control.beat_snap, &cmd);
        let target = vjc_core::clock::snap_target(phase, snap);
        if target > phase {
            self.snap_pending.push((
                target,
                Held {
                    act: SnapAct::Cmd(Box::new(cmd)),
                    at: std::time::Instant::now(),
                    token,
                },
            ));
            None
        } else {
            Some(cmd)
        }
    }

    /// Drops held actions whose layer or column no longer exists.
    fn prune_snapped(&mut self) {
        let c = &self.project.composition;
        let ncols = c.columns.len() as u32;
        self.snap_pending.retain(|(_, h)| match &h.act {
            SnapAct::Cmd(cmd) => match **cmd {
                Command::TriggerClip { layer, column, .. } => {
                    column < ncols && c.layers.iter().any(|l| l.id == layer)
                }
                Command::TriggerClipOn {
                    layer,
                    column,
                    onto,
                    ..
                } => {
                    column < ncols
                        && c.layers.iter().any(|l| l.id == layer)
                        && c.layers.iter().any(|l| l.id == onto)
                }
                Command::TriggerColumn { column, .. } => column < ncols,
                _ => true,
            },
            SnapAct::BeatLoop(layer, _) => c.layers.iter().any(|l| l.id == *layer),
        });
    }

    /// Runs the snapped actions whose beat or bar has come, or that waited
    /// `SNAP_MAX_BARS` of wall time (the clock may be stalled).
    fn run_snapped(&mut self, phase: f64, now: std::time::Instant) {
        self.prune_snapped();
        let beat_s = 60.0 / self.clock.bpm().max(1.0);
        let bar = vjc_core::clock::BEATS_PER_BAR;
        // Overdue actions get a due target so `take_due` releases them with the rest. The
        // wait allowed covers the snap itself (up to 8 bars) plus one bar, never less than
        // `SNAP_MAX_BARS`.
        for (t, h) in &mut self.snap_pending {
            let beats = (SNAP_MAX_BARS * bar).max((*t - phase).max(0.0) + bar);
            let max = std::time::Duration::from_secs_f64(beats * beat_s);
            if now.saturating_duration_since(h.at) >= max {
                *t = f64::NEG_INFINITY;
            }
        }
        for h in vjc_core::clock::take_due(&mut self.snap_pending, phase) {
            match h.act {
                SnapAct::Cmd(c) => {
                    let ok = self.exec_ok(*c);
                    if h.token != 0
                        && let Some(r) = self.holds.iter_mut().find(|r| r.token == h.token)
                    {
                        r.onair = ok;
                    }
                }
                SnapAct::BeatLoop(layer, b) => self.start_beat_loop(layer, b),
            }
        }
    }

    /// Like `exec`, but says whether the command was accepted.
    fn exec_ok(&mut self, cmd: Command) -> bool {
        // Clock inputs and triggers do not change the saved project.
        let runtime = matches!(
            cmd,
            Command::Tap { .. }
                | Command::Nudge { .. }
                | Command::Resync { .. }
                | Command::TriggerClip { .. }
                | Command::TriggerClipOn { .. }
                | Command::ClearLayer { .. }
                | Command::SelectDeck { .. }
                | Command::TriggerColumn { .. }
                | Command::StepColumn { .. }
                | Command::StepClip { .. }
                | Command::ClearAll
                | Command::StartRecording
                | Command::StopRecording
                | Command::SetMasterBypass { .. }
                | Command::JumpToCue { .. }
        );
        let ncols = self.project.composition.columns.len() as i64;
        let col_after = match &cmd {
            Command::TriggerColumn { column, .. }
            | Command::TriggerClip { column, .. }
            | Command::TriggerClipOn { column, .. } => Some(*column),
            Command::StepColumn { from, delta, .. } => Some(from.map_or(0, |c| {
                (i64::from(c) + i64::from(*delta)).clamp(0, (ncols - 1).max(0)) as u32
            })),
            _ => None,
        };
        let on_air = match &cmd {
            Command::TriggerColumn { column, .. } => Some(*column),
            Command::StepColumn { .. } => col_after,
            _ => None,
        };
        self.redirect = match &cmd {
            Command::TriggerClipOn { layer, onto, .. } => Some((*onto, *layer)),
            _ => None,
        };
        let is_step = matches!(cmd, Command::StepClip { .. });
        let deck_switch = matches!(cmd, Command::SelectDeck { .. });
        let is_set_bpm = matches!(cmd, Command::SetBpm { .. });
        #[cfg(test)]
        let now = self.history_now.unwrap_or_else(std::time::Instant::now);
        #[cfg(not(test))]
        let now = std::time::Instant::now();
        let col_ids = self.column_ids();
        let transport = match &cmd {
            Command::SetTransport { state } => Some(*state),
            _ => None,
        };
        match self.history.exec(&mut self.project, cmd, now) {
            Ok(mut events) => {
                events.extend(self.follow_columns(&col_ids));
                if let Some(st) = transport
                    && st != vjc_model::PlayState::Stop
                {
                    // The clips follow the global transport (a no-op when none is playing).
                    let ctx = self.rt_ctx();
                    if let Some(c) = vjc_core::shortcuts::set_all_paused(
                        &ctx.active,
                        &ctx.origin,
                        st == vjc_model::PlayState::Pause,
                    ) {
                        // Clips already in that state make this fail: nothing to do then.
                        // A performance action, like the transport itself: no undo entry.
                        let _ = vjc_core::apply(&mut self.project, c);
                    }
                }
                if is_set_bpm {
                    self.link_sync.note_bpm_edit();
                }
                if col_after.is_some() {
                    self.col_sel = col_after;
                }
                // Only a column trigger or step puts a column on air.
                if let Some(c) = on_air {
                    self.comp_ap.on_air(c, self.phase);
                }
                if is_step {
                    // The step lands on the first playable clip: follow it.
                    let landed = events.iter().find_map(|e| match e {
                        Event::ClipStarted { column, .. }
                        | Event::CaptureStarted { column, .. }
                        | Event::NetworkStarted { column, .. }
                        | Event::GeneratorStarted { column, .. } => Some(*column),
                        _ => None,
                    });
                    self.col_sel = landed.or(self.col_sel);
                    if let Some(c) = landed {
                        self.comp_ap.on_air(c, self.phase);
                    }
                }
                if deck_switch {
                    self.col_sel = None;
                    self.snap_pending.clear();
                    self.comp_ap.reset();
                    let cur = self.project.composition.current_deck;
                    self.preload_deck(cur);
                }
                self.prune_col_sel();
                if !runtime {
                    self.dirty = true;
                    self.project_gen += 1;
                }
                self.handle(events);
                self.redirect = None;
                true
            }
            Err(e) => {
                self.redirect = None;
                self.status = format!("Command failed: {e}");
                false
            }
        }
    }

    fn undo(&mut self) {
        let before = self.project.composition.tempo.bpm;
        let col_ids = self.column_ids();
        match self.history.undo(&mut self.project) {
            Some(Ok(mut ev)) => {
                ev.extend(self.follow_columns(&col_ids));
                if self.project.composition.tempo.bpm != before {
                    self.link_sync.note_bpm_edit();
                }
                self.dirty = true;
                self.project_gen += 1;
                self.prune_col_sel();
                // Mask points may have been added or removed.
                self.poly.sel_omask = None;
                self.handle(ev);
            }
            Some(Err(e)) => self.status = format!("Undo failed: {e}"),
            None => {}
        }
    }

    fn redo(&mut self) {
        let before = self.project.composition.tempo.bpm;
        let col_ids = self.column_ids();
        match self.history.redo(&mut self.project) {
            Some(Ok(mut ev)) => {
                ev.extend(self.follow_columns(&col_ids));
                if self.project.composition.tempo.bpm != before {
                    self.link_sync.note_bpm_edit();
                }
                self.dirty = true;
                self.project_gen += 1;
                self.prune_col_sel();
                self.poly.sel_omask = None;
                self.handle(ev);
            }
            Some(Err(e)) => self.status = format!("Redo failed: {e}"),
            None => {}
        }
    }

    fn column_ids(&self) -> Vec<Uuid> {
        self.project
            .composition
            .columns
            .iter()
            .map(|c| c.id)
            .collect()
    }

    /// After columns moved, were inserted or removed, what is playing, selected or queued
    /// keeps following its column: every column index kept per cell is rewritten to where
    /// the column is now (nothing on air changes). State of a removed column is dropped.
    /// Returns a `LayerCleared` for every layer whose playing clip was in a removed column,
    /// so the per-layer playback state goes with it.
    fn follow_columns(&mut self, old: &[Uuid]) -> Vec<Event> {
        let new = self.column_ids();
        if *old == new[..] {
            return vec![];
        }
        let mut stopped: Vec<LayerId> = vec![];
        let map = |c: u32| -> Option<u32> {
            old.get(c as usize)
                .and_then(|id| new.iter().position(|n| n == id))
                .map(|n| n as u32)
        };
        self.players.retain(|l, (_, c, _)| match map(*c) {
            Some(n) => {
                *c = n;
                true
            }
            None => {
                stopped.push(*l);
                false
            }
        });
        self.gens.retain(|l, (_, c, _)| match map(*c) {
            Some(n) => {
                *c = n;
                true
            }
            None => {
                stopped.push(*l);
                false
            }
        });
        self.origin.retain(|_, (_, _, c)| match map(*c) {
            Some(n) => {
                *c = n;
                true
            }
            None => false,
        });
        self.col_sel = self.col_sel.and_then(map);
        self.col_marks.retain(|m| new.contains(m));
        self.selected = match self.selected {
            Some((l, c)) => map(c).map(|n| (l, n)),
            None => None,
        };
        if let PreviewTarget::Clip(d, l, c) = self.preview_target {
            self.preview_target = match map(c) {
                Some(n) => PreviewTarget::Clip(d, l, n),
                None => PreviewTarget::Composition,
            };
        }
        self.comp_ap.remap(map);
        self.snap_pending.retain_mut(|(_, h)| match &mut h.act {
            SnapAct::Cmd(cmd) => remap_command(cmd, &map),
            SnapAct::BeatLoop(..) => true,
        });
        self.holds.retain_mut(|r| match map(r.fired.column) {
            Some(n) => {
                r.fired.column = n;
                true
            }
            None => false,
        });
        self.snap_req = match self.snap_req {
            Some((d, l, c)) => map(c).map(|n| (d, l, n)),
            None => None,
        };
        self.cell_drag = match self.cell_drag {
            Some((l, c)) => map(c).map(|n| (l, n)),
            None => None,
        };
        let resume = std::mem::take(&mut self.flow.resume);
        self.flow.resume = resume
            .into_iter()
            .filter_map(|((d, l, c), v)| map(c).map(|n| ((d, l, n), v)))
            .collect();
        for bag in self.layer_bags.borrow_mut().values_mut() {
            *bag = bag.iter().filter_map(|c| map(*c)).collect();
        }
        stopped.sort();
        stopped.dedup();
        stopped
            .into_iter()
            .map(|layer| Event::LayerCleared { layer })
            .collect()
    }

    /// The selected column index must point at a column that still exists.
    pub(crate) fn prune_col_sel(&mut self) {
        let n = self.project.composition.columns.len() as u32;
        if self.col_sel.is_some_and(|c| c >= n) {
            self.col_sel = None;
        }
    }

    /// Drops multi-selected slices that no longer exist on the selected screen (undo,
    /// RemoveScreen, SetOutputSetup); a lone survivor is not a multi-selection.
    pub(crate) fn prune_sel_slices(&mut self) {
        if self.sel_slices.is_empty() {
            return;
        }
        let live: Vec<Uuid> = match self.sel_screen {
            Some(id) => self
                .project
                .output
                .screens
                .iter()
                .filter(|s| s.id == id)
                .flat_map(|s| s.slices.iter().map(|x| x.id))
                .collect(),
            None => self
                .project
                .output
                .screens
                .iter()
                .flat_map(|s| s.slices.iter().map(|x| x.id))
                .collect(),
        };
        self.sel_slices.retain(|i| live.contains(i));
        if self.sel_slices.len() < 2 {
            self.sel_slices.clear();
        }
    }

    fn handle(&mut self, events: Vec<Event>) {
        self.prune_sel_slices();
        prune_resume(&self.project, &mut self.flow.resume);
        // Layer of the cell a started source comes from: its own unless redirected.
        let redirect = self.redirect;
        let src_of = move |layer: LayerId| redirect.filter(|r| r.0 == layer).map_or(layer, |r| r.1);
        for e in events {
            match e {
                Event::ClipStarted {
                    deck,
                    layer,
                    column,
                    path,
                } => {
                    if path.exists() {
                        let src = src_of(layer);
                        let prev_local = self.layer_fraction(layer);
                        self.remember_pos(layer);
                        self.gens.remove(&layer);
                        self.flow.loops.remove(&layer);
                        self.flow.auto_time.remove(&layer);
                        self.flow.beat_loops.remove(&layer);
                        self.cross_from_old(layer, deck, src, column);
                        let mut player = self
                            .preload
                            .take(deck, src, column, &path)
                            .unwrap_or_else(|| Player::open_scrub(&path));
                        // A warmed player was opened off air; it is on air from here.
                        player.set_on_air(true);
                        if let Some(engine) = self.audio_engine() {
                            player.attach_audio(&engine);
                        }
                        self.players.insert(layer, (deck, column, player));
                        self.mark_clip_start(layer);
                        self.set_origin(layer, deck, src, column);
                        let tr = self.transport(deck, src, column);
                        let dir = tr.map_or(Direction::Forward, |t| t.direction);
                        let seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(1, |d| d.as_nanos() as u64);
                        self.heads.insert(layer, Playhead::new(dir, seed));
                        self.flow.regions.remove(&layer);
                        let own_last =
                            resume_pos(&self.project, &self.flow.resume, (deck, src, column));
                        let start = vjc_core::trigger_start(
                            &self.project,
                            deck,
                            src,
                            column,
                            own_last,
                            prev_local,
                        );
                        let offset = f64::from(self.prefs.general.clip_start_offset_ms) / 1000.0;
                        let reg = self.region_of(layer);
                        // The new clip's own length (the player may not know it yet).
                        let dur = self
                            .players
                            .get(&layer)
                            .and_then(|(_, _, p)| p.duration())
                            .or_else(|| {
                                self.transport(deck, src, column)
                                    .and_then(|t| t.duration_secs)
                            });
                        if let Some(to) = vjc_core::playback::first_seek(start, offset, dur, reg) {
                            self.seek(layer, to);
                        }
                    } else {
                        self.notify(
                            widgets::Level::Warn,
                            format!("File not found: {}", path.display()),
                        );
                    }
                }
                Event::CaptureStarted {
                    deck,
                    layer,
                    column,
                    device,
                } => {
                    self.gens.remove(&layer);
                    self.flow.loops.remove(&layer);
                    self.flow.auto_time.remove(&layer);
                    self.flow.beat_loops.remove(&layer);
                    self.cross_from_old(layer, deck, src_of(layer), column);
                    self.flow.regions.remove(&layer);
                    self.flow.pos.remove(&layer);
                    self.players
                        .insert(layer, (deck, column, Player::open_capture(&device)));
                    self.set_origin(layer, deck, src_of(layer), column);
                    self.heads.remove(&layer);
                }
                Event::NetworkStarted {
                    deck,
                    layer,
                    column,
                    protocol,
                    name,
                } => {
                    self.gens.remove(&layer);
                    self.flow.loops.remove(&layer);
                    self.flow.auto_time.remove(&layer);
                    self.flow.beat_loops.remove(&layer);
                    self.cross_from_old(layer, deck, src_of(layer), column);
                    self.flow.regions.remove(&layer);
                    self.flow.pos.remove(&layer);
                    match browser::net_backend(&protocol) {
                        Some(p) => {
                            self.players
                                .insert(layer, (deck, column, open_network(p, name)));
                            self.set_origin(layer, deck, src_of(layer), column);
                            self.heads.remove(&layer);
                        }
                        None => self.status = "Spout is not available on macOS".into(),
                    }
                }
                Event::GeneratorStarted {
                    deck,
                    layer,
                    column,
                } => {
                    self.players.remove(&layer);
                    self.flow.outgoing.remove(&layer);
                    self.flow.loops.remove(&layer);
                    self.flow.auto_time.remove(&layer);
                    self.flow.beat_loops.remove(&layer);
                    self.flow.gen_bars.remove(&layer);
                    self.flow.regions.remove(&layer);
                    self.flow.pos.remove(&layer);
                    self.gens.insert(layer, (deck, column, 0.0));
                    self.mark_clip_start(layer);
                    self.set_origin(layer, deck, src_of(layer), column);
                }
                Event::Seek {
                    deck,
                    layer,
                    column,
                    pos,
                } => {
                    // `layer` is the cell's; the clip may play on another layer.
                    for layer in self.dests_of(Some(deck), layer, column) {
                        // A cue jump ends an active beat loop and is not a completed loop.
                        self.flow.beat_loops.remove(&layer);
                        let local = vjc_core::playback::to_local(pos, self.region_of(layer));
                        if let Some(c) = self.flow.loops.get_mut(&layer) {
                            c.rebase(local);
                        }
                        self.seek(layer, pos);
                    }
                }
                Event::ThumbRefresh {
                    deck,
                    layer,
                    column,
                } => {
                    if let Some(k) = vjc_core::clip_in(&self.project, deck, layer, column) {
                        self.thumbs.refresh(k.id);
                    }
                }
                Event::LayerCleared { layer } => {
                    self.remember_pos(layer);
                    self.flow.outgoing.remove(&layer);
                    self.flow.beat_loops.remove(&layer);
                    self.flow.regions.remove(&layer);
                    self.flow.pos.remove(&layer);
                    self.flow.gen_bars.remove(&layer);
                    self.flow.loops.remove(&layer);
                    self.flow.auto_time.remove(&layer);
                    self.players.remove(&layer);
                    self.gens.remove(&layer);
                    self.clip_start.remove(&layer);
                    self.origin.remove(&layer);
                }
                Event::FaderStart { layer } => {
                    // The playing clip restarts from the start of its region, in place.
                    // The clip's own override wins over the layer's flag.
                    let enabled = self
                        .clip_on(layer)
                        .and_then(|k| k.extras.fader_start)
                        .or_else(|| {
                            self.project
                                .composition
                                .layers
                                .iter()
                                .find(|l| l.id == layer)
                                .map(|l| l.fader_start)
                        });
                    if enabled == Some(true) && self.active(layer).is_some() {
                        self.flow.beat_loops.remove(&layer);
                        let (start, _) = self.region_of(layer);
                        if let Some(c) = self.flow.loops.get_mut(&layer) {
                            c.rebase(0.0);
                        }
                        self.seek(layer, start);
                    }
                }
                Event::Skipped { layer, column } => {
                    let name = self
                        .project
                        .composition
                        .layers
                        .iter()
                        .find(|l| l.id == layer)
                        .map_or("?", |l| l.name.as_str());
                    self.status = format!("Skipped {name}, column {}: not playable", column + 1);
                }
                Event::MasterBypass { bypass } => self.master_bypass = bypass,
                Event::RecordingStarted => self.rec_request = Some(true),
                Event::RecordingStopped => self.rec_request = Some(false),
                Event::Tap { at } => {
                    if let Some(bpm) = self.clock.tap(at) {
                        self.exec(Command::SetBpm { bpm });
                    }
                }
                Event::Nudge { dir, at } => self.link_sync.nudge(dir, &mut self.clock, at),
                Event::Resync { at } => self.link_sync.resync(&mut self.clock, at),
                Event::CellChanged {
                    deck,
                    layer,
                    column,
                } => {
                    for dest in self.dests_of(Some(deck), layer, column) {
                        self.players.remove(&dest);
                        self.gens.remove(&dest);
                        self.origin.remove(&dest);
                    }
                }
                Event::ClipsMoved { deck, moves } => self.follow_moves(deck, &moves),
                Event::DeckRemoved { deck } => {
                    self.flow.outgoing.clear();
                    self.players.retain(|_, (d, _, _)| *d != deck);
                    self.gens.retain(|_, (d, _, _)| *d != deck);
                }
            }
        }
        self.prune_origin();
    }

    /// While a layer name is dragged: the insertion line between the layer rows.
    fn paint_layer_drop_line(&self, ctx: &egui::Context) {
        let Some(dragged) = self.layer_drag else {
            return;
        };
        let Some(p) = ctx.input(|i| i.pointer.latest_pos()) else {
            return;
        };
        let rows: Vec<&(egui::Rect, LayerId)> = self
            .layer_rects
            .iter()
            .filter(|(_, l)| self.project.composition.layers.iter().any(|x| x.id == *l))
            .collect();
        let at = layer_drop_index(&self.layer_rects, dragged, p.y);
        let others: Vec<&&(egui::Rect, LayerId)> =
            rows.iter().filter(|(_, l)| *l != dragged).collect();
        let half = theme::LAYER_ROW_H / 2.0;
        let y = match (others.get(at), others.last()) {
            (Some((r, _)), _) => r.center().y - half,
            (None, Some((r, _))) => r.center().y + half,
            _ => return,
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("layer_drop_line"),
        ));
        let g = self.grid_rect;
        painter.line_segment(
            [egui::pos2(g.left(), y), egui::pos2(g.right(), y)],
            egui::Stroke::new(2.5, theme::CELL_DROP),
        );
    }

    /// A cell menu entry or key on cell (`layer`, `col`) of deck `deck`. Edits become
    /// commands (undoable); Copy and the selection only touch runtime state.
    fn cell_act(
        &mut self,
        deck: DeckId,
        (layer, column): Cell,
        act: CellAct,
        cmds: &mut Vec<Command>,
    ) {
        let clip = vjc_core::clip_in(&self.project, deck, layer, column).cloned();
        self.selected = Some((layer, column));
        self.selected_deck = Some(deck);
        match (act, clip) {
            (CellAct::Trigger, Some(_)) => cmds.push(Command::TriggerClip {
                deck,
                layer,
                column,
            }),
            (CellAct::Preview, Some(_)) => {
                self.preview_target = PreviewTarget::Clip(deck, layer, column);
            }
            (CellAct::Copy, Some(k)) => self.clip_clipboard = Some(k),
            (CellAct::ConvertHap, Some(k)) => {
                if let vjc_model::Source::File { path } = &k.source {
                    self.hap_ask = Some(HapAsk {
                        src: path.clone(),
                        cell: Some((deck, layer, column)),
                        clip: Some(k.id),
                        variant: None,
                    });
                }
            }
            (CellAct::Cut, Some(k)) => {
                self.clip_clipboard = Some(k);
                cmds.push(Command::ClearCell {
                    deck,
                    layer,
                    column,
                });
            }
            (CellAct::Clear, Some(_)) => cmds.push(Command::ClearCell {
                deck,
                layer,
                column,
            }),
            (CellAct::Paste, _) => {
                if let Some(k) = &self.clip_clipboard {
                    cmds.push(Command::SetClip {
                        deck,
                        layer,
                        column,
                        clip: vjc_core::clone_with_fresh_ids(k),
                    });
                }
            }
            _ => {}
        }
    }

    /// Clips changed cells: the selection, the clip preview and any playing source follow
    /// their clip, all at once (a swap exchanges two). A playing clip whose target layer
    /// is busy with a clip that stays put is stopped instead of displacing it.
    fn follow_moves(&mut self, deck: DeckId, moves: &[(Cell, Cell)]) {
        let dest = |cell: (LayerId, u32)| moves.iter().find(|m| m.0 == cell).map(|m| m.1);
        if let Some(sel) = self.selected.and_then(dest) {
            self.selected = Some(sel);
        }
        if let PreviewTarget::Clip(d, l, c) = self.preview_target
            && d == deck
            && let Some((l, c)) = dest((l, c))
        {
            self.preview_target = PreviewTarget::Clip(d, l, c);
        }
        // Clips redirected to another layer stay where they play; only their cell moves.
        let redirected: Vec<(LayerId, (DeckId, LayerId, u32))> = self
            .origin
            .iter()
            .filter(|(_, v)| v.0 == deck)
            .map(|(k, v)| (*k, *v))
            .collect();
        for (onto, (d, l, c)) in redirected {
            if let Some((l2, c2)) = dest((l, c)) {
                if let Some(p) = self.players.get_mut(&onto) {
                    p.1 = c2;
                }
                if let Some(g) = self.gens.get_mut(&onto) {
                    g.1 = c2;
                }
                self.set_origin(onto, d, l2, c2);
            }
        }
        // Only a clip playing on its own layer travels with its cell's flow state.
        let playing =
            |s: &Self, cell: (LayerId, u32)| s.dest_of(Some(deck), cell.0, cell.1) == Some(cell.0);
        /// Per-layer flow state that travels with a clip moved to another layer.
        #[derive(Default)]
        struct Carried {
            loops: Option<vjc_core::playback::LoopCounter>,
            beat_loop: Option<(f64, (f64, f64))>,
            region: Option<(f64, f64)>,
            gen_bar: Option<(i64, u32)>,
            pos: Option<f64>,
        }
        type Taken = (
            (LayerId, u32),
            Option<Player>,
            Option<f64>,
            Option<Playhead>,
            Option<Carried>,
        );
        let mut taken: Vec<Taken> = vec![];
        for &(from, to) in moves {
            if !playing(self, from) {
                continue;
            }
            let busy = to.0 != from.0
                && active_of(&self.players, &self.gens, to.0)
                    .is_some_and(|(d, c)| !(d == deck && dest((to.0, c)).is_some()));
            let player = self.players.remove(&from.0).map(|p| p.2);
            let gen_t = self.gens.remove(&from.0).map(|g| g.2);
            let head = self.heads.remove(&from.0);
            let carried = (to.0 != from.0).then(|| Carried {
                loops: self.flow.loops.remove(&from.0),
                beat_loop: self.flow.beat_loops.remove(&from.0),
                region: self.flow.regions.remove(&from.0),
                gen_bar: self.flow.gen_bars.remove(&from.0),
                pos: self.flow.pos.remove(&from.0),
            });
            if busy {
                continue;
            }
            taken.push((to, player, gen_t, head, carried));
        }
        for (to, player, gen_t, head, carried) in taken {
            if let Some(c) = carried {
                self.flow.outgoing.remove(&to.0);
                self.flow.loops.remove(&to.0);
                self.flow.beat_loops.remove(&to.0);
                self.flow.regions.remove(&to.0);
                self.flow.gen_bars.remove(&to.0);
                self.flow.pos.remove(&to.0);
                if let Some(v) = c.loops {
                    self.flow.loops.insert(to.0, v);
                }
                if let Some(v) = c.beat_loop {
                    self.flow.beat_loops.insert(to.0, v);
                }
                if let Some(v) = c.region {
                    self.flow.regions.insert(to.0, v);
                }
                if let Some(v) = c.gen_bar {
                    self.flow.gen_bars.insert(to.0, v);
                }
                if let Some(v) = c.pos {
                    self.flow.pos.insert(to.0, v);
                }
            }
            if let Some(p) = player {
                self.players.insert(to.0, (deck, to.1, p));
            }
            if let Some(t) = gen_t {
                self.gens.insert(to.0, (deck, to.1, t));
            }
            if let Some(h) = head {
                self.heads.insert(to.0, h);
            }
        }
    }

    /// Opens (in the background) the decoders of the clips `deck` would play first.
    fn preload_deck(&mut self, deck: DeckId) {
        let cells = preload::candidates(&self.project, deck, preload::MAX_PRELOAD, |l, c| {
            self.players
                .get(&l)
                .is_some_and(|(d, col, _)| *d == deck && *col == c)
        });
        if self.preload.is_warm(deck, &cells) {
            return;
        }
        self.preload.warm(deck, cells, |p| Player::open_scrub(p));
    }

    /// How far the layer's playing clip is inside its region (0..=1), if one plays.
    fn layer_fraction(&self, layer: LayerId) -> Option<f64> {
        self.players.contains_key(&layer).then_some(())?;
        let pos = *self.flow.pos.get(&layer)?;
        Some(vjc_core::playback::to_local(pos, self.region_of(layer)))
    }

    /// Notes where the layer's playing clip is, so a Continue trigger can resume it.
    fn remember_pos(&mut self, layer: LayerId) {
        if let (Some((d, src, c)), Some(p)) = (self.cell_of(layer), self.flow.pos.get(&layer))
            && let Some(k) = vjc_core::clip_in(&self.project, d, src, c)
        {
            self.flow.resume.insert((d, src, c), (k.id, *p));
        }
    }

    /// Moves the playing clip of `layer` (if any) into `flow.outgoing` when the layer's
    /// transition asks for a crossing; otherwise drops it. Joins the old decode thread
    /// only in the second case.
    fn cross_from_old(&mut self, layer: LayerId, deck: DeckId, src: LayerId, column: u32) {
        self.flow.outgoing.remove(&layer);
        let old_clip = self.clip_on(layer).cloned();
        let fit = old_clip.as_ref().map(|k| k.fit);
        let Some((_, _, old)) = self.players.remove(&layer) else {
            return;
        };
        let state = self
            .project
            .composition
            .layers
            .iter()
            .find(|l| l.id == layer)
            .map_or(vjc_core::transition::State::Idle, |l| {
                // The incoming clip's own transition, when set, wins over the layer's.
                let own = vjc_core::clip_in(&self.project, deck, src, column)
                    .and_then(|k| k.transition.as_ref());
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64);
                vjc_core::transition::State::start_seeded(
                    vjc_core::transition::effective(&l.transition, own),
                    self.clock.bpm(),
                    seed,
                )
            });
        // A clip that fades out stays on air (and keeps its hw session) until the fade
        // ends and it is dropped; a clip cut straight away is off air now.
        if state == vjc_core::transition::State::Idle {
            old.set_on_air(false);
        }
        if state != vjc_core::transition::State::Idle {
            self.flow.outgoing.insert(
                layer,
                Outgoing {
                    player: old,
                    fit: fit.unwrap_or_default(),
                    state,
                    clip: old_clip,
                },
            );
        }
    }

    /// In..out region of the clip playing on the layer (the whole clip when none).
    fn region_of(&self, layer: LayerId) -> (f64, f64) {
        self.clip_on(layer)
            .map_or((0.0, 1.0), |k| vjc_core::playback::region(&k.transport))
    }

    /// Jumps the layer's playhead and player to a normalised position.
    fn seek(&mut self, layer: LayerId, pos: f64) {
        let reg = self.region_of(layer);
        if let Some(h) = self.heads.get_mut(&layer) {
            h.seek(vjc_core::playback::to_local(pos, reg));
        }
        if let Some((_, _, p)) = self.players.get(&layer) {
            p.set_on_air(true);
            p.set_position(pos);
        }
        self.flow.pos.insert(layer, pos);
    }

    /// Beat loop: repeat `beats` beats from the current position (None clears it).
    fn set_beat_loop(&mut self, layer: LayerId, beats: Option<f64>) {
        let Some(b) = beats else {
            self.snap_pending
                .retain(|(_, a)| !matches!(a.act, SnapAct::BeatLoop(l, _) if l == layer));
            self.flow.beat_loops.remove(&layer);
            return;
        };
        let phase = self.clock.phase(std::time::Instant::now());
        let snap = self
            .project
            .composition
            .beat_snap
            .unwrap_or(self.prefs.control.beat_snap);
        let target = vjc_core::clock::snap_target(phase, snap);
        if target > phase {
            self.snap_pending
                .retain(|(_, a)| !matches!(a.act, SnapAct::BeatLoop(l, _) if l == layer));
            self.snap_pending.push((
                target,
                Held {
                    act: SnapAct::BeatLoop(layer, b),
                    at: std::time::Instant::now(),
                    token: 0,
                },
            ));
        } else {
            self.start_beat_loop(layer, b);
        }
    }

    fn start_beat_loop(&mut self, layer: LayerId, beats: f64) {
        let beats = Some(beats);
        let dur = self.players.get(&layer).and_then(|(_, _, p)| p.duration());
        let win = beats.and_then(|b| {
            let pos = self.flow.pos.get(&layer).copied().unwrap_or(0.0);
            vjc_core::playback::beat_window(pos, b, self.clock.bpm(), dur?).map(|w| (b, w))
        });
        match win {
            Some(w) => self.flow.beat_loops.insert(layer, w),
            None => self.flow.beat_loops.remove(&layer),
        };
    }

    /// Seconds each layer's clip of the column runs (top to bottom); `None` where the layer
    /// has no clip playing from that column or it has no end (generator, live source).
    fn column_clip_secs(&self, col: u32) -> Vec<Option<f64>> {
        let comp = &self.project.composition;
        comp.layers
            .iter()
            .map(|l| {
                let (deck, c, player) = self.players.get(&l.id)?;
                if *c != col {
                    return None;
                }
                let tr = &clip_for(&self.project, &self.origin, l.id, *deck, col)?.transport;
                let reg = vjc_core::playback::region(tr);
                let rate = vjc_core::playback_rate(tr.speed, 1.0, comp.master_speed);
                let secs = player.duration()? * (reg.1 - reg.0) / rate;
                (secs.is_finite() && secs > 0.0).then_some(secs)
            })
            .collect()
    }

    /// Column the layer's autopilot would play next, if any.
    fn autopilot_target(&self, layer: LayerId) -> Option<(DeckId, u32)> {
        let l = self
            .project
            .composition
            .layers
            .iter()
            .find(|l| l.id == layer)?;
        // A redirected clip does not belong to this layer's columns: no autopilot for it.
        if self.origin.contains_key(&layer) {
            return None;
        }
        let (deck, cur) = self.active(layer)?;
        let n = self.project.composition.columns.len();
        let playable: Vec<bool> = (0..n as u32)
            .map(|c| {
                vjc_core::clip_in(&self.project, deck, layer, c)
                    .is_some_and(|k| autopilot_playable(&k.source))
            })
            .collect();
        let rand = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        let mut bags = self.layer_bags.borrow_mut();
        let bag = bags.entry(layer).or_default();
        let clip = vjc_core::clip_in(&self.project, deck, layer, cur);
        let ap = vjc_core::playback::autopilot_of(l, clip);
        vjc_core::playback::next_clip_ap(ap, cur, &playable, rand, bag).map(|c| (deck, c))
    }

    /// Deck and column of the clip playing on the layer (video or generator).
    fn rt_ctx(&self) -> vjc_core::shortcuts::Ctx {
        let active = self
            .project
            .composition
            .layers
            .iter()
            .filter_map(|l| active_of(&self.players, &self.gens, l.id).map(|(d, c)| (l.id, d, c)))
            .collect();
        let origin = self
            .origin
            .iter()
            .map(|(dest, (_, src, _))| (*dest, *src))
            .collect();
        vjc_core::shortcuts::Ctx::new(self.col_sel, self.master_bypass)
            .with_active(active)
            .with_origin(origin)
    }

    /// True while something changes every frame: a playing clip or generator, a fade,
    /// a recording, or any output screen / window.
    fn is_live(&self) -> bool {
        !self.players.is_empty()
            || !self.gens.is_empty()
            || !self.flow.outgoing.is_empty()
            || self.capture.is_some()
            || self.rec_request.is_some()
            || self.output.is_some()
            || !self.project.output.screens.is_empty()
            // Time-driven work (beat clock, Link, timecode cues, metronome) must not wait
            // for the idle repaint.
            || self.project.composition.tempo.metronome
            || self.project.composition.tempo.link_enabled
            || self.prefs.timecode.source != vjc_io::prefs::TcSource::Off
    }

    fn active(&self, layer: LayerId) -> Option<(DeckId, u32)> {
        active_of(&self.players, &self.gens, layer)
    }

    /// Cell (deck, layer, column) of the clip playing on `layer`.
    fn cell_of(&self, layer: LayerId) -> Option<(DeckId, LayerId, u32)> {
        let (d, c) = self.active(layer)?;
        Some((d, src_layer(&self.origin, layer, d, c), c))
    }

    /// The clip playing on `layer`, wherever its cell is.
    fn clip_on(&self, layer: LayerId) -> Option<&vjc_model::Clip> {
        let (d, c) = self.active(layer)?;
        clip_for(&self.project, &self.origin, layer, d, c)
    }

    /// Layers the clip of cell (`src`, `col`) plays on (`deck` None = any deck).
    fn dests_of(&self, deck: Option<DeckId>, src: LayerId, col: u32) -> Vec<LayerId> {
        dests_in(&self.players, &self.gens, &self.origin, deck, src, col)
    }

    fn dest_of(&self, deck: Option<DeckId>, src: LayerId, col: u32) -> Option<LayerId> {
        self.dests_of(deck, src, col).into_iter().next()
    }

    /// A clip target as addresses see it: through the layer the clip plays on (its own
    /// unless redirected), with what is active there. Other targets pass through.
    fn addressed(&self, target: EffectTarget) -> (EffectTarget, Option<(DeckId, u32)>) {
        match target {
            EffectTarget::Clip {
                deck,
                layer,
                column,
            } => match self.dest_of(Some(deck), layer, column) {
                Some(dest) => (
                    EffectTarget::Clip {
                        deck,
                        layer: dest,
                        column,
                    },
                    Some((deck, column)),
                ),
                None => (target, None),
            },
            _ => (target, None),
        }
    }

    /// Notes where the clip now playing on `dest` lives.
    fn set_origin(&mut self, dest: LayerId, deck: DeckId, src: LayerId, col: u32) {
        if src == dest {
            self.origin.remove(&dest);
        } else {
            self.origin.insert(dest, (deck, src, col));
        }
    }

    /// Forgets redirects whose source stopped or whose cell is gone.
    fn prune_origin(&mut self) {
        let (players, gens, project) = (&self.players, &self.gens, &self.project);
        self.origin.retain(|dest, (d, l, c)| {
            active_of(players, gens, *dest) == Some((*d, *c))
                && vjc_core::clip_in(project, *d, *l, *c).is_some()
        });
    }

    fn transport(&self, deck: DeckId, layer: LayerId, column: u32) -> Option<vjc_model::Transport> {
        Some(
            vjc_core::clip_in(&self.project, deck, layer, column)?
                .transport
                .clone(),
        )
    }

    /// Once per frame: follows the project BPM, advances the clock and every playhead.
    /// Effective speed = `speed` x beat-sync factor (clip length = n beats).
    fn tick_clock(&mut self) {
        let now = std::time::Instant::now();
        // Preferences > Control sets how far the nudge buttons change the speed.
        let rate = f64::from(self.prefs.control.nudge_percent) / 100.0;
        let rate = rate.clamp(
            vjc_core::clock::NUDGE_RANGE.0,
            vjc_core::clock::NUDGE_RANGE.1,
        );
        if (self.clock.nudge_rate() - rate).abs() > 1e-9 {
            self.clock.set_nudge_rate(rate, now);
        }
        let on = self.project.composition.tempo.link_enabled;
        self.link_sync.update(
            &mut self.link,
            on,
            self.project.composition.tempo.bpm,
            &mut self.clock,
            now,
        );
        // DJ players (Preferences > DJ sync) steer tempo and bar phase; Link wins if on.
        self.dj_pump(now, !on);
        // Only saved folders are scanned: never half-typed paths while Preferences is open.
        if !self.prefs_open {
            self.sync_clap_dirs();
            self.sync_ffgl_dirs();
        }
        // Transport of the tempo clock follows the saved play state.
        match self.project.composition.tempo.play_state {
            vjc_model::PlayState::Play => self.clock.play(now),
            vjc_model::PlayState::Pause => self.clock.pause(now),
            vjc_model::PlayState::Stop => {
                if !self.clock.is_paused() || self.clock.phase(now) != 0.0 {
                    self.clock.stop(now);
                }
            }
        }
        // Effective tempo (may come from the Link session, runtime only).
        let bpm = self.clock.bpm();
        let t = self.clock.tick(now);
        self.phase = t.phase;
        self.run_snapped(t.phase, now);
        if let Some(b) = t.beat {
            self.beat = b;
        }
        if let Some(accent) =
            vjc_media::audio::beat_click(self.project.composition.tempo.metronome, t.beat)
        {
            if self.metro.is_none()
                && let Some(engine) = self.audio_engine()
            {
                self.metro = Some(vjc_media::audio::Metronome::new(&engine));
            }
            if let Some(m) = &self.metro {
                m.click(accent);
            }
        }
        let dt = now
            .saturating_duration_since(self.last_head_tick)
            .as_secs_f64();
        self.last_head_tick = now;
        let master_speed = self.project.composition.master_speed;
        let reverse = self.project.composition.master_reverse;
        // A group's speed multiplies the clips of its layers, on top of the master speed.
        let comp = &self.project.composition;
        let speed_of = |layer: &LayerId| {
            master_speed
                * comp
                    .layers
                    .iter()
                    .find(|l| l.id == *layer)
                    .map_or(1.0, |l| comp.group_speed(l))
        };
        let mut finished = vec![];
        let mut autopilot = vec![];
        // External timecode (offset applied): None = no signal or source off.
        let tc_on = self.prefs.timecode.source != vjc_io::prefs::TcSource::Off;
        if let Some(err) = self
            .tc
            .sync(&self.prefs.timecode, std::time::Instant::now())
        {
            self.status = err;
        }
        let tc_now = if tc_on { self.tc.current(now) } else { None };
        let tc_free_run = self.prefs.timecode.on_loss == vjc_io::prefs::OnLoss::FreeRun;
        for o in self.flow.outgoing.values_mut() {
            o.state = o.state.advance(dt);
        }
        self.flow
            .outgoing
            .retain(|_, o| o.state != vjc_core::transition::State::Idle);
        // Outgoing audio keeps following the mix and fades out with the transition.
        for (layer, o) in &self.flow.outgoing {
            if let Some(clip) = &o.clip {
                let (vol, pan) = audio_mix(&self.project, *layer, clip);
                let fade = o
                    .state
                    .current()
                    .map_or(0.0, |(_, p)| 1.0 - p.clamp(0.0, 1.0));
                o.player.set_audio(vol * fade, pan, fade > 0.0);
            }
        }
        self.sync_audio_fx();
        for (layer, (deck, col, player)) in &self.players {
            let Some(clip) = clip_for(&self.project, &self.origin, *layer, *deck, *col) else {
                continue;
            };
            let tr = &clip.transport;
            let sync = match (tr.beat_sync, player.duration()) {
                (Some(n), Some(d)) => {
                    let reg = vjc_core::playback::region(tr);
                    beat_sync_rate(d * (reg.1 - reg.0), n, bpm)
                }
                (None, Some(d)) => {
                    let reg = vjc_core::playback::region(tr);
                    vjc_core::playback::duration_rate(d * (reg.1 - reg.0), tr.duration_secs)
                }
                _ => 1.0,
            };
            let rate = vjc_core::playback_rate(tr.speed, sync, speed_of(layer));
            let gdir = vjc_core::playback::global_direction(tr.direction, reverse);
            // Play modes: the playhead picks the position, the player serves the frame.
            if let (Some(file_d), Some(head)) = (player.duration(), self.heads.get_mut(layer)) {
                // The playhead runs over the in..out region only; `position` below is the
                // matching place in the file.
                let reg = vjc_core::playback::region(tr);
                if let Some(old) = self.flow.regions.insert(*layer, reg)
                    && old != reg
                {
                    head.retarget(old, reg);
                }
                let d = file_d * (reg.1 - reg.0);
                // Timecode clips: position = timecode - start (random access, muted); without
                // signal they freeze or run free from where they were, per the prefs.
                let by_tc = tc_on && tr.timecode_start.is_some();
                let t = match (&tr.timecode_start, tc_now) {
                    (Some(start), Some(cur)) if tc_on => {
                        let t = vjc_core::timecode::clip_position(
                            cur,
                            start,
                            tr.play_mode,
                            gdir,
                            rate,
                            d,
                        );
                        head.seek(t.position);
                        t
                    }
                    (Some(_), None) if tc_on && !tc_free_run => {
                        head.tick(0.0, tr.play_mode, gdir, rate, d)
                    }
                    _ => head.tick(dt, tr.play_mode, gdir, rate, d),
                };
                // Forward playback streams; everything else needs random access.
                player.set_sequential(!by_tc && is_sequential(tr.play_mode, gdir, rate));
                let mut position = vjc_core::playback::to_actual(t.position, reg);
                if let Some((_, w)) = self.flow.beat_loops.get(layer)
                    && !t.finished
                    && let Some(np) = vjc_core::playback::loop_wrap(position, *w)
                {
                    head.seek(vjc_core::playback::to_local(np, reg));
                    position = np;
                }
                player.set_on_air(true);
                player.set_position(position);
                // Audio only at forward 1x; any other speed, direction or pause is muted.
                let audible = rate.is_finite()
                    && !by_tc
                    && (rate - 1.0).abs() < 1e-3
                    && is_sequential(tr.play_mode, gdir, rate)
                    && gdir != Direction::Pause
                    && !t.finished;
                let (vol, pan) = audio_mix(&self.project, *layer, clip);
                player.set_audio(vol, pan, audible);
                self.flow.pos.insert(*layer, position);
                if t.finished {
                    finished.push(*layer);
                } else if matches!(tr.play_mode, PlayMode::Loop | PlayMode::Random)
                    && let Some(l) = self
                        .project
                        .composition
                        .layers
                        .iter()
                        .find(|l| l.id == *layer)
                    && vjc_core::playback::autopilot_of(l, Some(clip)).mode
                        != vjc_model::AutopilotMode::Off
                    && vjc_core::playback::autopilot_of(l, Some(clip)).unit
                        == vjc_model::AutopilotUnit::Loops
                {
                    let every = vjc_core::playback::autopilot_of(l, Some(clip)).every_loops;
                    let c = self.flow.loops.entry(*layer).or_default();
                    if c.observe(t.position, tr.direction)
                        && vjc_core::playback::loops_reached(c.count, every)
                    {
                        autopilot.push(*layer);
                    }
                }
            }
        }
        // Autopilot on generators: they never finish, so a bar of the app clock counts as a loop.
        let bar = (self.clock.phase(std::time::Instant::now()) / 4.0).floor() as i64;
        for (layer, (deck, col, _)) in &self.gens {
            let Some(l) = self
                .project
                .composition
                .layers
                .iter()
                .find(|l| l.id == *layer)
            else {
                continue;
            };
            let gclip = clip_for(&self.project, &self.origin, *layer, *deck, *col);
            let ap = vjc_core::playback::autopilot_of(l, gclip);
            if ap.mode == vjc_model::AutopilotMode::Off
                || ap.unit != vjc_model::AutopilotUnit::Loops
            {
                continue;
            }
            let st = vjc_core::playback::gen_bars(self.flow.gen_bars.get(layer).copied(), bar);
            self.flow.gen_bars.insert(*layer, st);
            if vjc_core::playback::loops_reached(st.1, ap.every_loops) {
                autopilot.push(*layer);
                self.flow.gen_bars.remove(layer);
            }
        }
        // Autopilot by beats or seconds: counts from the moment the clip started.
        let running: Vec<(LayerId, DeckId, u32)> = self
            .players
            .iter()
            .map(|(l, v)| (*l, v.0, v.1))
            .chain(self.gens.iter().map(|(l, v)| (*l, v.0, v.1)))
            .collect();
        let live: Vec<LayerId> = running.iter().map(|r| r.0).collect();
        self.flow.auto_time.retain(|k, _| live.contains(k));
        for (layer, deck, col) in running {
            let Some(ap) = self
                .project
                .composition
                .layers
                .iter()
                .find(|l| l.id == layer)
                .map(|l| {
                    let k = clip_for(&self.project, &self.origin, layer, deck, col);
                    vjc_core::playback::autopilot_of(l, k).clone()
                })
                .filter(|a| {
                    a.mode != vjc_model::AutopilotMode::Off
                        && a.unit != vjc_model::AutopilotUnit::Loops
                })
            else {
                self.flow.auto_time.remove(&layer);
                continue;
            };
            // A paused clip does not run the clock.
            let playing = clip_for(&self.project, &self.origin, layer, deck, col)
                .is_some_and(|k| k.transport.direction != Direction::Pause);
            let e = self
                .flow
                .auto_time
                .entry(layer)
                .or_insert_with(|| vjc_core::playback::TimedClock::new(&ap));
            if e.tick(&ap, playing, dt, bpm) {
                self.flow.auto_time.remove(&layer);
                if !autopilot.contains(&layer) {
                    autopilot.push(layer);
                }
            }
        }
        // Generator TIME runs on the app clock, scaled by the clip transport.
        for (layer, (deck, col, t)) in self.gens.iter_mut() {
            if let Some(tr) =
                clip_for(&self.project, &self.origin, *layer, *deck, *col).map(|k| &k.transport)
            {
                let dir = match vjc_core::playback::global_direction(tr.direction, reverse) {
                    Direction::Backward => -1.0,
                    Direction::Pause => 0.0,
                    _ => 1.0,
                };
                let sync = vjc_core::clock::generator_sync_rate(tr.beat_sync, bpm);
                let rate = vjc_core::playback_rate(tr.speed, sync, speed_of(layer));
                match (&tr.timecode_start, tc_now) {
                    (Some(start), Some(cur)) if tc_on => {
                        *t = (cur - start.seconds()).max(0.0) * rate * dir;
                    }
                    (Some(_), None) if tc_on && !tc_free_run => {}
                    _ => *t += dt * rate * dir,
                }
            }
        }
        // Timecode cues: each crossing triggers once.
        match tc_now {
            Some(cur) => {
                let cues = self.project.composition.timecode_cues.clone();
                let deck = self.project.composition.current_deck;
                for i in self.tc.cues.step(&cues, cur) {
                    let c = &cues[i];
                    self.exec_ok(match c.layer {
                        Some(layer) => Command::TriggerClip {
                            deck,
                            layer,
                            column: c.column,
                        },
                        None => Command::TriggerColumn {
                            deck,
                            column: c.column,
                        },
                    });
                }
            }
            None => self.tc.cues.reset(),
        }
        // Once: clear the layer after the last frame.
        for layer in finished {
            let _ = match self.autopilot_target(layer) {
                Some((deck, column)) => self.exec_ok(Command::TriggerClip {
                    deck,
                    layer,
                    column,
                }),
                None => self.exec_ok(Command::ClearLayer { layer }),
            };
        }
        for layer in autopilot {
            if let Some((deck, column)) = self.autopilot_target(layer) {
                self.exec_ok(Command::TriggerClip {
                    deck,
                    layer,
                    column,
                });
            }
        }
        // Composition autopilot: plays the next column when the time of this one is up.
        // `exec` (not `exec_ok`) defers the trigger to the beat grid when beat snap is on.
        if let Some(cur) = self.comp_ap.current() {
            let clip_secs = self.column_clip_secs(cur);
            let rand = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos() as u64);
            let running = !self.clock.is_paused();
            let comp = &self.project.composition;
            // Longest layer transition: the next column fires that much early when asked.
            let bpm = self.project.composition.tempo.bpm;
            let lead_secs = vjc_core::comp_autopilot::lead_secs(&self.project, cur, bpm);
            if let Some(next) = self.comp_ap.tick(
                &comp.autopilot,
                &comp.columns,
                &clip_secs,
                vjc_core::comp_autopilot::Frame {
                    phase: self.phase,
                    dt,
                    running,
                    rand,
                    lead_secs,
                    bpm,
                },
            ) {
                let deck = comp.current_deck;
                self.exec(Command::TriggerColumn { deck, column: next });
            }
        }
    }

    /// Browser drop at `pos`: files and generators go to a cell, effects to a cell,
    /// layer name or the master bar. The deck is always the current one.
    fn browser_drop(&mut self, item: browser::Item, pos: egui::Pos2) {
        let deck = self.project.composition.current_deck;
        let cell = self.cells.iter().find(|c| c.rect.contains(pos)).copied();
        match item {
            browser::Item::File(path) => match cell {
                Some(c) => self.drop_cell(c.layer, c.col, path),
                None => self.status = "Drop the file on a grid cell".into(),
            },
            browser::Item::Generator(id) => match cell {
                Some(c) if c.empty => {
                    self.exec(Command::SetGenerator {
                        deck,
                        layer: c.layer,
                        column: c.col,
                        kind: id.clone(),
                        name: fx_label(&id),
                        params: vjc_fx::generator_params(&id).unwrap_or_default(),
                    });
                    self.exec(Command::TriggerClip {
                        deck,
                        layer: c.layer,
                        column: c.col,
                    });
                }
                _ => self.status = "Drop a generator on an empty cell".into(),
            },
            browser::Item::Network { protocol, name } => match cell {
                Some(c) if c.empty => {
                    let mut clip = vjc_core::clip_from_path(PathBuf::new());
                    clip.name = name.clone();
                    clip.source = vjc_model::Source::Network { protocol, name };
                    self.prefs.defaults.apply(&mut clip);
                    self.exec(Command::SetClip {
                        deck,
                        layer: c.layer,
                        column: c.col,
                        clip,
                    });
                    self.exec(Command::TriggerClip {
                        deck,
                        layer: c.layer,
                        column: c.col,
                    });
                }
                _ => self.status = "Drop a network source on an empty cell".into(),
            },
            browser::Item::Effect(id) => {
                let target = if let Some(c) = cell {
                    Some(EffectTarget::Clip {
                        deck,
                        layer: c.layer,
                        column: c.col,
                    })
                } else if let Some((_, l)) = self.layer_rects.iter().find(|(r, _)| r.contains(pos))
                {
                    Some(EffectTarget::Layer(*l))
                } else if self.master_rect.is_some_and(|r| r.contains(pos)) {
                    Some(EffectTarget::Master)
                } else {
                    None
                };
                match (target, self.new_fx(&id)) {
                    (Some(target), Some(effect)) => self.exec(Command::AddEffect {
                        target,
                        effect,
                        index: usize::MAX,
                    }),
                    _ => self.status = "Drop an effect on a cell, layer name or Master".into(),
                }
            }
        }
    }

    /// A file dropped on a cell: loads and selects it (Preview), never triggers.
    fn drop_cell(&mut self, layer: LayerId, column: u32, path: PathBuf) {
        let deck = self.project.composition.current_deck;
        // A cell already on air must reload, or the wall keeps the old clip.
        let live = self.dest_of(Some(deck), layer, column);
        self.load_file(layer, column, path);
        if let Some(onto) = live {
            // Back where it was playing, whatever the clip target says now.
            self.exec(Command::TriggerClipOn {
                deck,
                layer,
                column,
                onto,
            });
        }
        self.selected = Some((layer, column));
        self.selected_deck = Some(deck);
        self.preview_target = PreviewTarget::Clip(deck, layer, column);
    }

    /// One trigger for every cell of `layer` in `column..column + n` that is on air, each on
    /// the layer it plays on (it may have been redirected).
    fn live_cells(&self, deck: DeckId, layer: LayerId, column: u32, n: u32) -> Vec<Command> {
        (column..column + n)
            .filter_map(|c| {
                let onto = self.dest_of(Some(deck), layer, c)?;
                Some(Command::TriggerClipOn {
                    deck,
                    layer,
                    column: c,
                    onto,
                })
            })
            .collect()
    }

    /// Several files/folders dropped from `column` on: one per column, in name order, adding
    /// the missing columns, as a single undo step. Nothing is triggered unless a cell of the
    /// range is on air (then each live cell reloads, on its own destination). Returns
    /// (placed, ignored).
    fn drop_many(&mut self, layer: LayerId, column: u32, paths: Vec<PathBuf>) -> (usize, usize) {
        let (files, ignored) = expand_drop(paths, self.prefs.video.folder_sequence);
        if files.is_empty() {
            return (0, ignored);
        }
        let deck = self.project.composition.current_deck;
        // The layer's on-air cell, if the drop replaces it: reload it after the batch.
        let n = files.len() as u32;
        let live = self.live_cells(deck, layer, column, n);
        let have = self.project.composition.columns.len() as u32;
        let mut commands = Vec::new();
        for i in have..column + files.len() as u32 {
            commands.push(Command::AddColumn {
                column: vjc_model::Column {
                    color: None,
                    autopilot: Default::default(),
                    beat_snap: None,
                    id: Uuid::new_v4(),
                    name: (i + 1).to_string(),
                },
            });
        }
        for (i, path) in files.iter().enumerate() {
            let mut clip = match vjc_media::sequence::scan_dir(path).filter(|_| path.is_dir()) {
                Some(s) => {
                    vjc_core::clip_from_sequence(s.pattern, vjc_media::sequence::DEFAULT_FPS)
                }
                None => vjc_core::clip_from_path(path.clone()),
            };
            clip.fit = self.prefs.video.fit;
            self.prefs.defaults.apply(&mut clip);
            commands.push(Command::SetClip {
                deck,
                layer,
                column: column + i as u32,
                clip,
            });
        }
        self.exec(Command::Batch { commands });
        for cmd in live {
            self.exec(cmd);
        }
        self.selected = Some((layer, column));
        self.selected_deck = Some(deck);
        self.preview_target = PreviewTarget::Clip(deck, layer, column);
        (files.len(), ignored)
    }

    /// Loads `path` into the cell and plays it.
    fn drop_file(&mut self, layer: LayerId, column: u32, path: PathBuf) {
        self.load_file(layer, column, path);
        let deck = self.project.composition.current_deck;
        self.exec(Command::TriggerClip {
            deck,
            layer,
            column,
        });
    }

    /// Loads `path` into the cell without triggering it: what is on air stays.
    fn load_file(&mut self, layer: LayerId, column: u32, path: PathBuf) {
        let deck = self.project.composition.current_deck;
        // One undo step: the default fit is set on the clip before it is placed.
        let mut clip = vjc_core::clip_from_path(path);
        clip.fit = self.prefs.video.fit;
        self.prefs.defaults.apply(&mut clip);
        self.exec(Command::SetClip {
            deck,
            layer,
            column,
            clip,
        });
    }

    /// Dashboard dials and audio levels of this frame, for composition-level params.
    fn dials(&self) -> AnimIn {
        self.dials_for(None)
    }

    /// Like `dials`, with the scoped dials of `owner`'s own layer and clip (never the UI
    /// selection's).
    fn dials_for(&self, owner: Option<EffectTarget>) -> AnimIn {
        let d = &self.project.composition.dashboard;
        let layer_of = |l: LayerId| {
            self.project
                .composition
                .layers
                .iter()
                .find(|x| x.id == l)
                .map(|l| vjc_core::anim::dial_values(&l.track.dials))
        };
        let (layer_dials, clip_dials) = match owner.map(EffectTarget::base) {
            Some(EffectTarget::Layer(l)) => (layer_of(l), None),
            Some(EffectTarget::Clip {
                deck,
                layer,
                column,
            }) => (
                layer_of(layer),
                vjc_core::clip_in(&self.project, deck, layer, column)
                    .map(|k| vjc_core::anim::dial_values(&k.track.dials)),
            ),
            _ => (None, None),
        };
        AnimIn {
            layer_dials,
            clip_dials,
            dials: (0..vjc_core::anim::DIALS)
                .map(|i| d.get(i).map_or(0.0, vjc_core::anim::dial_norm))
                .collect(),
            bands: self.audio_react.levels,
            smoother: self.audio_react.smoother.clone(),
        }
    }

    /// Records the clock readings of a clip trigger on `layer`.
    fn mark_clip_start(&mut self, layer: LayerId) {
        let secs = self.fx_epoch.elapsed().as_secs_f64();
        let beats = self.clock.phase(std::time::Instant::now());
        self.clip_start.insert(layer, (secs, beats));
    }

    /// Animation inputs of this frame.
    fn anim_ctx<'a>(&self, input: &'a AnimIn) -> vjc_core::anim::AnimCtx<'a> {
        vjc_core::anim::AnimCtx {
            beat_phase: self.clock.phase(std::time::Instant::now()),
            time_secs: self.fx_epoch.elapsed().as_secs_f64(),
            fft_bands: &input.bands,
            smoother: Some(&input.smoother),
            dashboard: &input.dials,
            layer_dials: input.layer_dials.as_deref(),
            clip_dials: input.clip_dials.as_deref(),
            clip_start: None,
        }
    }

    /// Keeps the clip preview player in step with `preview_target`: opened when a file clip
    /// is picked, dropped when something else is. It loops on its own clock, apart from
    /// `players` (which feed the composition).
    fn sync_clip_preview(&mut self) {
        let want = match self.preview_target {
            PreviewTarget::Clip(d, l, c) => vjc_core::clip_in(&self.project, d, l, c)
                .map(|k| ((d, l, c), k.id, k.source.clone())),
            _ => None,
        };
        let Some((key, clip, source)) = want else {
            self.clip_prev = None;
            // The clip was removed or replaced under the preview.
            if matches!(self.preview_target, PreviewTarget::Clip(..)) {
                self.preview_target = PreviewTarget::Composition;
            }
            return;
        };
        if self
            .clip_prev
            .as_ref()
            .is_some_and(|p| p.key == key && p.clip == clip)
        {
            let p = self.clip_prev.as_ref().unwrap();
            if let Some(pl) = &p.player {
                let pos = pl
                    .duration()
                    .map_or(0.0, |d| (p.since.elapsed().as_secs_f64() / d).fract());
                pl.set_position(pos);
            }
            return;
        }
        let player = match source {
            vjc_model::Source::File { path } if path.exists() => {
                let p = Player::open_scrub(path);
                // Looped on its own clock for the preview: never claims a hardware session
                // from a clip that is on a layer.
                p.set_on_air(false);
                Some(p)
            }
            _ => None,
        };
        self.clip_prev = Some(ClipPreview {
            key,
            clip,
            player,
            since: std::time::Instant::now(),
        });
    }

    /// Serves a Snapshot PNG request: the clip's current frame when it is playing, else the
    /// composition output. Saves into the snapshot folder (asked once, then remembered).
    fn serve_snapshot(&mut self, frame: &eframe::Frame) {
        let Some((deck, layer, column)) = self.next_snapshot_req() else {
            return;
        };
        let name = vjc_core::clip_in(&self.project, deck, layer, column)
            .map_or_else(|| "snapshot".to_string(), |k| k.name.clone());
        let playing = self
            .players
            .get(&layer)
            .filter(|(d, c, _)| *d == deck && *c == column)
            .map(|(_, _, p)| p);
        let shot = match playing.and_then(|p| p.latest_frame()) {
            Some(f) if f.hap.is_none() => {
                let f = f.oriented();
                Some((f.width, f.height, f.data.to_vec()))
            }
            // Block-compressed HAP: decode the clip itself on the CPU, on a worker so the
            // UI thread never waits on the decoder; the PNG is written when it is done.
            Some(_) => {
                if let Some((path, pos)) = self.snapshot_source(deck, layer, column) {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let spawned = std::thread::Builder::new()
                        .name("vjc-snapshot".into())
                        .spawn(move || {
                            let shot = vjc_media::snapshot_frame(&path, pos)
                                .map(|f| (f.width, f.height, f.data.to_vec()));
                            let _ = tx.send(shot);
                        });
                    if spawned.is_ok() {
                        self.status = "Snapshot: decoding...".into();
                        self.snap_job = Some((name, rx));
                        return;
                    }
                }
                None
            }
            None => frame
                .wgpu_render_state()
                .zip(self.comp.as_ref())
                .and_then(|(rs, comp)| comp.read_rgba(&rs.device, &rs.queue)),
        };
        self.write_snapshot(&name, shot);
    }

    /// The pending snapshot request, but only when no HAP decode is in flight: the request
    /// waits its turn instead of replacing the running job.
    fn next_snapshot_req(&mut self) -> Option<(DeckId, LayerId, u32)> {
        self.poll_snapshot_job();
        if self.snap_job.is_some() {
            return None;
        }
        self.snap_req.take()
    }

    /// Writes the worker's snapshot once it has finished.
    fn poll_snapshot_job(&mut self) {
        use std::sync::mpsc::TryRecvError;
        let Some((name, rx)) = &self.snap_job else {
            return;
        };
        let shot = match rx.try_recv() {
            Ok(shot) => shot,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        let name = name.clone();
        self.snap_job = None;
        self.write_snapshot(&name, shot);
    }

    /// File and playhead of the clip on air in the cell: the position the app drove into the
    /// player on the latest frame (`flow.pos`), not the first one it was ever given.
    fn snapshot_source(&self, deck: DeckId, layer: LayerId, column: u32) -> Option<(PathBuf, f64)> {
        let (_, _, p) = self
            .players
            .get(&layer)
            .filter(|(d, c, _)| *d == deck && *c == column)?;
        let vjc_model::Source::File { path } =
            &vjc_core::clip_in(&self.project, deck, layer, column)?.source
        else {
            return None;
        };
        let pos = self
            .flow
            .pos
            .get(&layer)
            .copied()
            .unwrap_or_else(|| p.position());
        Some((path.clone(), pos))
    }

    /// Saves `shot` (width, height, RGBA) as a PNG named after `name`.
    fn write_snapshot(&mut self, name: &str, shot: Option<(u32, u32, Vec<u8>)>) {
        let Some((w, h, px)) = shot else {
            self.status = "Snapshot: nothing to capture yet".into();
            return;
        };
        let folder = match self.prefs.general.snapshot_folder.clone() {
            Some(f) => f,
            None => {
                let Some(f) = rfd::FileDialog::new()
                    .set_title("Folder for snapshots")
                    .pick_folder()
                else {
                    self.status = "Snapshot cancelled".into();
                    return;
                };
                self.prefs.general.snapshot_folder = Some(f.clone());
                self.prefs_dirty = true;
                f
            }
        };
        self.status = match vjc_media::snapshot::save_png(&folder, name, w, h, &px) {
            Ok(p) => format!("Snapshot saved: {}", p.display()),
            Err(e) => e,
        };
    }

    fn compose(&mut self, frame: &eframe::Frame) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        self.sync_clip_preview();
        let routes = self.active_routes();
        let want_preview = std::mem::take(&mut self.monitor_seen) || self.preview_routed();
        let preview_src = match self.preview_target {
            PreviewTarget::Layer(l) => Some(vjc_model::SliceSource::Layer(l)),
            PreviewTarget::Clip(..) => {
                Some(vjc_model::SliceSource::Layer(compositor::PREVIEW_CLIP_ID))
            }
            PreviewTarget::Composition => None,
        };
        let prev_frame: Option<Frame> = self
            .clip_prev
            .as_ref()
            .and_then(|p| p.player.as_ref()?.latest_frame());
        let c = &self.project.composition;
        let frame_px = (f64::from(c.width), f64::from(c.height));
        let (w, h) = (c.width.max(1), c.height.max(1));
        let dials = self.dials();
        let actx = self.anim_ctx(&dials);
        let fmt = color_depth::pool_format(self.project.composition.bit_depth);
        if self.comp.as_ref().is_some_and(|c| c.fmt != fmt) {
            // Bit depth changed: every pipeline and texture is built again at the new format.
            self.comp = None;
            if let Some(id) = self.comp_tex.take() {
                rs.renderer.write().free_texture(&id);
            }
        }
        if self.comp.is_none() {
            let comp = Compositor::with_format(&rs.device, w, h, fmt);
            self.comp_tex = Some(rs.renderer.write().register_native_texture(
                &rs.device,
                &comp.target_view,
                wgpu::FilterMode::Linear,
            ));
            self.comp = Some(comp);
        }
        let comp = self.comp.as_mut().unwrap();
        if comp.resize(&rs.device, w, h)
            && let Some(id) = self.comp_tex
        {
            rs.renderer.write().update_egui_texture_from_wgpu_texture(
                &rs.device,
                &comp.target_view,
                wgpu::FilterMode::Linear,
                id,
            );
        }
        // Layers are ordered top to bottom; draw bottom to top.
        let frames: Vec<(&Layer, Option<Frame>)> = c
            .layers
            .iter()
            .rev()
            .map(|l| {
                (
                    l,
                    if c.layer_off(l) {
                        None
                    } else {
                        self.players
                            .get(&l.id)
                            .and_then(|(_, _, p)| p.latest_frame())
                    },
                )
            })
            .collect();
        // Freeze and stop-motion: the layer keeps showing an older frame of its clip.
        let mut frames = frames;
        for (l, f) in frames.iter_mut() {
            let hold = active_of(&self.players, &self.gens, l.id)
                .and_then(|(d, col)| clip_for(&self.project, &self.origin, l.id, d, col))
                .filter(|k| k.extras.freeze || k.extras.stop_motion_fps > 0.0)
                .map(|k| (k.id, k.extras.freeze, k.extras.stop_motion_fps));
            let Some((cid, freeze, fps)) = hold else {
                self.flow.held.remove(&l.id);
                continue;
            };
            let keep = self
                .flow
                .held
                .get(&l.id)
                .filter(|h| h.0 == cid)
                .and_then(|h| {
                    let since = h.2.elapsed().as_secs_f64();
                    (!vjc_core::playback::picture_refresh(freeze, fps, since)).then(|| h.1.clone())
                });
            match (keep, f.clone()) {
                (Some(held), _) => *f = Some(held),
                (None, Some(fresh)) => {
                    self.flow
                        .held
                        .insert(l.id, (cid, fresh, std::time::Instant::now()));
                }
                (None, None) => {}
            }
        }
        let out_frames: HashMap<LayerId, Frame> = self
            .flow
            .outgoing
            .iter()
            .filter_map(|(id, o)| Some((*id, o.player.latest_frame()?)))
            .collect();
        // Fit comes from the deck the clip was triggered from, not the visible one.
        let (players, gens, origin) = (&self.players, &self.gens, &self.origin);
        let fit_of = |l: &Layer| {
            let (deck, col) = active_of(players, gens, l.id)?;
            let fit = clip_for(&self.project, origin, l.id, deck, col)?.fit;
            Some(l.extras.auto_size.fit().unwrap_or(fit))
        };
        // Shaders compile on worker threads; install the ones that finished.
        for done in self.fx.poll() {
            let st = match done.result {
                Ok(wgsl) => match comp.add_effect_passes(
                    &rs.device,
                    &done.kind,
                    &wgsl,
                    self.fx.isf(&done.kind).map_or(&[][..], |i| &i.passes),
                ) {
                    Ok(()) => fx::Status::Ready,
                    Err(e) => fx::Status::Failed(e),
                },
                Err(e) => fx::Status::Failed(e),
            };
            self.fx.set_status(&done.kind, st);
        }
        self.fx.ffgl.sweep();
        let time = self.fx_epoch.elapsed().as_secs_f32();
        self.ml.set_dir(ml_fx::models_dir(
            self.prefs.general.models_folder.as_ref(),
            data_dir(),
        ));
        let cf_pos = c
            .crossfader
            .as_ref()
            .map_or(vjc_core::crossfader::CENTER, |x| {
                vjc_core::anim::effective_number(&x.position, &actx)
            });
        let mut draws: Vec<LayerDraw> = vec![];
        let mut hap_in: HashMap<LayerId, std::sync::Arc<vjc_media::hap::HapFrame>> = HashMap::new();
        // Group of each entry of `draws` (an outgoing transition source shares its layer's).
        let mut draw_groups: Vec<Option<vjc_model::GroupId>> = vec![];
        for (l, f) in &frames {
            // Video toggle off: the layer is not drawn (its clips keep playing).
            if !l.video {
                continue;
            }
            // Clip chain first, then the layer chain.
            let size = f.as_ref().map_or((w, h), |f| f.display_size());
            let clip = active_of(players, gens, l.id)
                .and_then(|(d, col)| clip_for(&self.project, origin, l.id, d, col));
            // Strip video: this clip's picture is not drawn (its audio keeps playing).
            if clip.is_some_and(|k| k.extras.strip_video) {
                continue;
            }
            // Layer and clip dashboards: dial animations on this layer's params read them.
            let (layer_dials, clip_dials) = (
                vjc_core::anim::dial_values(&l.track.dials),
                clip.map_or_else(Vec::new, |k| vjc_core::anim::dial_values(&k.track.dials)),
            );
            let lctx = actx
                .scoped(&layer_dials, &clip_dials)
                .with_clip_start(self.clip_start.get(&l.id).copied());
            let clip_fx = clip.map(|k| k.track.effects.as_slice());
            let generator = match (clip.map(|k| &k.source), gens.get(&l.id)) {
                (Some(vjc_model::Source::Generator { kind, params }), Some((_, _, t)))
                    if !c.layer_off(l) =>
                {
                    self.fx.request(kind);
                    let ready = self.fx.status(kind) == Some(&fx::Status::Ready);
                    self.fx.isf(kind).filter(|_| ready).map(|isf| EffectDraw {
                        lut: None,
                        blend: Default::default(),
                        kind: kind.clone(),
                        uniforms: isf.pack(
                            &vjc_core::anim::resolve(params, &lctx),
                            *t as f32,
                            (w, h),
                        ),
                        dry_wet: 1.0,
                        ml: None,
                        persistent: None,
                        ticking: false,
                        ffgl: None,
                    })
                }
                _ => None,
            };
            // Identifies the clip feeding this layer: ML effects reset their state when it changes.
            let src_tag = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                active_of(players, gens, l.id).hash(&mut h);
                origin.get(&l.id).hash(&mut h);
                h.finish()
            };
            let mut effects = draw_chain(
                &mut self.fx,
                &mut self.ml,
                Some(src_tag),
                clip_fx.unwrap_or_default(),
                time,
                size,
                &lctx,
            );
            effects.extend(draw_chain(
                &mut self.fx,
                &mut self.ml,
                Some(src_tag),
                &l.track.effects,
                time,
                size,
                &lctx,
            ));
            // Clip transform first, then the layer's (see vjc_core::transform).
            let layer_xf = xf_inverse(&l.track.transform, &lctx, frame_px);
            let xf = clip.map_or(layer_xf, |k| {
                layer_xf.through(&xf_inverse(&k.track.transform, &lctx, frame_px))
            });
            let fit_now = fit_of(l).unwrap_or_default();
            let xf = compositor::area_affine(&l.extras, size, (w, h), fit_now)
                .map_or(xf, |a| xf.through(&a));
            let cross = self
                .flow
                .outgoing
                .get(&l.id)
                .filter(|_| !c.layer_off(l))
                .and_then(|o| Some((o, o.state.current()?, out_frames.get(&l.id)?)));
            let opacity = (vjc_core::anim::effective_number(&l.track.opacity, &lctx)
                * l.extras.master
                * vjc_core::crossfader::layer_gain(
                    c.crossfader.as_ref(),
                    vjc_core::crossfader::effective_bus(l, &c.groups),
                    cf_pos,
                )) as f32;
            let blend = vjc_core::blend::effective_blend(
                &vjc_core::crossfader::layer_blend(
                    c.crossfader.as_ref(),
                    vjc_core::crossfader::effective_bus(l, &c.groups),
                    &l.track.blend,
                ),
                clip.and_then(|k| k.blend_override.as_ref()),
            );
            if let Some((o, (kind, prog), of)) = cross {
                let fx = draw_chain(
                    &mut self.fx,
                    &mut self.ml,
                    None,
                    &l.track.effects,
                    time,
                    of.display_size(),
                    &lctx,
                );
                draw_groups.push(l.group);
                if let Some(h) = &of.hap {
                    hap_in.insert(Uuid::from_u128(l.id.as_u128() ^ OUT_MASK), h.clone());
                }
                draws.push(LayerDraw {
                    id: Uuid::from_u128(l.id.as_u128() ^ OUT_MASK),
                    opacity,
                    blend: blend.clone(),
                    fit: l.extras.auto_size.fit().unwrap_or(o.fit),
                    effects: fx,
                    generator: None,
                    transition: Some((kind.shader_id(), prog, true)),
                    transform: compositor::area_affine(
                        &l.extras,
                        of.display_size(),
                        (w, h),
                        l.extras.auto_size.fit().unwrap_or(o.fit),
                    )
                    .map_or(layer_xf, |a| layer_xf.through(&a)),
                    channels: channels_of(o.clip.as_ref()),
                    mask: l.mask_mode,
                    frame: compositor::frame_input(Some(of)).0,
                    orient: compositor::frame_input(Some(of)).1,
                });
            }
            draw_groups.push(l.group);
            if let Some(h) = f.as_ref().and_then(|f| f.hap.clone()) {
                hap_in.insert(l.id, h);
            }
            draws.push(LayerDraw {
                id: l.id,
                transition: cross.map(|(_, (k, p), _)| (k.shader_id(), p, false)),
                opacity,
                blend,
                fit: fit_now,
                effects,
                generator,
                transform: xf,
                channels: channels_of(clip),
                mask: l.mask_mode,
                frame: compositor::frame_input(f.as_ref()).0,
                orient: compositor::frame_input(f.as_ref()).1,
            });
        }
        // The previewed clip, drawn alone with its own effects: last in the list, so the
        // compositor keeps it out of the composition.
        if want_preview
            && let PreviewTarget::Clip(d, l, col) = self.preview_target
            && let Some(k) = vjc_core::clip_in(&self.project, d, l, col)
        {
            let (layer_dials, clip_dials) = (
                c.layers
                    .iter()
                    .find(|x| x.id == l)
                    .map_or_else(Vec::new, |x| vjc_core::anim::dial_values(&x.track.dials)),
                vjc_core::anim::dial_values(&k.track.dials),
            );
            let pctx = actx
                .scoped(&layer_dials, &clip_dials)
                .with_clip_start(self.clip_start.get(&l).copied());
            let t = self
                .clip_prev
                .as_ref()
                .map_or(0.0, |p| p.since.elapsed().as_secs_f32());
            let size = prev_frame.as_ref().map_or((w, h), |f| f.display_size());
            let generator = match &k.source {
                vjc_model::Source::Generator { kind, params } => {
                    self.fx.request(kind);
                    let ready = self.fx.status(kind) == Some(&fx::Status::Ready);
                    self.fx.isf(kind).filter(|_| ready).map(|isf| EffectDraw {
                        lut: None,
                        blend: Default::default(),
                        kind: kind.clone(),
                        uniforms: isf.pack(&vjc_core::anim::resolve(params, &pctx), t, (w, h)),
                        dry_wet: 1.0,
                        ml: None,
                        persistent: None,
                        ticking: false,
                        ffgl: None,
                    })
                }
                _ => None,
            };
            let src_tag = {
                use std::hash::{Hash, Hasher};
                let mut hs = std::collections::hash_map::DefaultHasher::new();
                (d, l, col, "preview").hash(&mut hs);
                hs.finish()
            };
            let effects = draw_chain(
                &mut self.fx,
                &mut self.ml,
                Some(src_tag),
                &k.track.effects,
                time,
                size,
                &pctx,
            );
            let (frame, prev_orient) = compositor::frame_input(prev_frame.as_ref());
            if let Some(h) = prev_frame.as_ref().and_then(|f| f.hap.clone()) {
                hap_in.insert(compositor::PREVIEW_CLIP_ID, h);
            }
            if frame.is_some() || generator.is_some() {
                draw_groups.push(None);
                draws.push(LayerDraw {
                    id: compositor::PREVIEW_CLIP_ID,
                    opacity: 1.0,
                    blend: vjc_model::BlendMode::Normal,
                    fit: k.fit,
                    effects,
                    generator,
                    transition: None,
                    transform: xf_inverse(&k.track.transform, &pctx, frame_px),
                    channels: k.channels,
                    mask: Default::default(),
                    frame,
                    orient: prev_orient,
                });
            }
        }
        let master = draw_chain(
            &mut self.fx,
            &mut self.ml,
            Some(0),
            &c.master.effects,
            time,
            (w, h),
            &actx,
        );
        let master_opacity = if self.master_bypass {
            0.0
        } else {
            vjc_core::anim::effective_number(&c.master.opacity, &actx) as f32
        };
        // Consecutive draws of one group form a run composed in the group's own buffer.
        let mut group_draws: Vec<compositor::GroupDraw> = vec![];
        let mut i = 0;
        while i < draws.len() {
            let Some(gid) = draw_groups[i] else {
                i += 1;
                continue;
            };
            let len = draw_groups[i..]
                .iter()
                .take_while(|g| **g == Some(gid))
                .count();
            if let Some(g) = c.group(gid).filter(|g| !c.group_off(g)) {
                group_draws.push(compositor::GroupDraw {
                    mask: g.mask_mode,
                    id: Some(gid),
                    start: i,
                    len,
                    opacity: vjc_core::anim::effective_number(&g.track.opacity, &actx) as f32,
                    blend: g.track.blend.clone(),
                    effects: draw_chain(
                        &mut self.fx,
                        &mut self.ml,
                        Some(0),
                        &g.track.effects,
                        time,
                        (w, h),
                        &actx,
                    ),
                });
            }
            i += len;
        }
        comp.master_transform = xf_inverse(&c.master.transform, &actx, frame_px);
        comp.preview = if want_preview { preview_src } else { None };
        comp.set_audio(
            &rs.queue,
            &self.audio_react.detail.spectrum,
            &self.audio_react.detail.wave,
            self.audio_react.playing,
        );
        comp.hap_in = hap_in;
        comp.render_routed(
            &rs.device,
            &rs.queue,
            (&draws, &group_draws),
            (&master, master_opacity),
            &routes,
        );
        self.prev_live =
            want_preview && comp.routed_view(vjc_model::SliceSource::Preview).is_some();
        if want_preview && let Some(v) = comp.routed_view(vjc_model::SliceSource::Preview) {
            let mut r = rs.renderer.write();
            match self.prev_tex {
                Some(id) => r.update_egui_texture_from_wgpu_texture(
                    &rs.device,
                    v,
                    wgpu::FilterMode::Linear,
                    id,
                ),
                None => {
                    self.prev_tex =
                        Some(r.register_native_texture(&rs.device, v, wgpu::FilterMode::Linear));
                }
            }
        }
        self.ml.pump(comp, &rs.device, &rs.queue);
        match self.rec_request.take() {
            Some(true) if self.capture.is_none() => {
                let size = comp.size;
                // Master audio goes in when the output device is already open (clips with
                // sound played); recording never opens a device by itself.
                let feed = self
                    .audio
                    .as_ref()
                    .filter(|a| a.sample_rate() > 0)
                    .map(|a| vjc_media::record::AudioFeed {
                        tap: a.tap(),
                        rate: a.sample_rate(),
                    });
                let have = self.rec_codecs.wait();
                let settings = vjc_media::record::RecSettings::from_prefs(
                    &self.prefs.record.codec,
                    self.prefs.record.quality,
                    have,
                );
                match capture::Capture::start(
                    capture::default_path_for(
                        self.prefs.general.record_folder.as_deref(),
                        settings.codec.extension(),
                    ),
                    size,
                    c.fps,
                    feed,
                    settings,
                ) {
                    Ok(cap) => {
                        self.status = format!("Recording to {}", cap.path().display());
                        self.capture = Some(cap);
                    }
                    Err(e) => self.status = format!("Record failed: {e}"),
                }
            }
            Some(false) => {
                if let Some(cap) = self.capture.take() {
                    self.status = match cap.stop(&rs.device) {
                        Ok(s) => format!(
                            "Saved {} ({} frames, {} dropped, {}{})",
                            s.path.display(),
                            s.encoded,
                            s.dropped,
                            s.codec,
                            if s.audio { ", with audio" } else { "" }
                        ),
                        Err(e) => format!("Record failed: {e}"),
                    };
                }
            }
            _ => {}
        }
        if let Some(cap) = self.capture.as_mut() {
            if cap.failed() || cap.size() != comp.size {
                let cap = self.capture.take().unwrap();
                self.status = match cap.stop(&rs.device) {
                    Ok(s) => format!("Recording stopped, saved {}", s.path.display()),
                    Err(e) => format!("Record failed: {e}"),
                };
            } else {
                cap.poll(&rs.device);
                cap.capture(&rs.device, &rs.queue, comp.readable());
            }
        }
        api_ext::snap_service(
            &mut self.snap_jobs,
            &rs.device,
            &rs.queue,
            comp.readable(),
            comp.size,
            self.master_bypass,
        );
        self.render_screens(rs);
    }

    /// Monitor-bound screens: (screen id, monitor index) for every screen whose output
    /// window is open. Screens without slices show the composition as it is.
    fn open_screens(&self) -> Vec<(Uuid, usize)> {
        self.project
            .output
            .screens
            .iter()
            .filter(|s| !self.closed_screens.contains(&s.id))
            .filter_map(|s| match &s.device {
                OutputDevice::Monitor { name } => {
                    let id = name.parse::<u32>().ok()?;
                    Some((s.id, monitors::find(&self.monitors, id)?))
                }
                _ => None,
            })
            .collect()
    }

    /// Screens that are rendered this frame: open monitor windows, Syphon / NDI and Art-Net.
    fn output_screen_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self.open_screens().into_iter().map(|(id, _)| id).collect();
        ids.extend(self.net_wants().iter().map(|w| w.id));
        ids.extend(self.artnet_wants().iter().map(|w| w.id));
        // The Output tab of the setup window shows the live render of the selected screen,
        // even when that screen goes to no display.
        if let Some(id) = self.out_preview_screen()
            && !ids.contains(&id)
        {
            ids.push(id);
        }
        ids
    }

    /// True when a slice of a rendered screen shows the preview monitor.
    fn preview_routed(&self) -> bool {
        let ids = self.output_screen_ids();
        self.project
            .output
            .screens
            .iter()
            .filter(|s| ids.contains(&s.id))
            .flat_map(|s| &s.slices)
            .any(|sl| sl.source == vjc_model::SliceSource::Preview)
    }

    /// Layers and groups that a slice of a rendered screen routes: the only extra renders
    /// the compositor does (a project that routes nothing pays nothing).
    fn active_routes(&self) -> Vec<vjc_model::SliceSource> {
        let ids = self.output_screen_ids();
        vjc_core::mapping::routed_sources(
            self.project
                .output
                .screens
                .iter()
                .filter(|s| ids.contains(&s.id)),
        )
    }

    /// Screens set to Syphon or NDI: what each publishes.
    fn net_wants(&self) -> Vec<netout::Want> {
        self.project
            .output
            .screens
            .iter()
            .filter_map(|s| {
                let protocol = match s.device {
                    OutputDevice::Syphon => vjc_net::Protocol::Syphon,
                    OutputDevice::Ndi => vjc_net::Protocol::Ndi,
                    _ => return None,
                };
                Some(netout::Want {
                    id: s.id,
                    protocol,
                    name: format!("VJCraft {}", s.name),
                })
            })
            .collect()
    }

    /// Screens set to Art-Net: what each sends.
    fn artnet_wants(&self) -> Vec<artnet_out::Want> {
        self.project
            .output
            .screens
            .iter()
            .filter_map(|s| match &s.device {
                OutputDevice::ArtNet {
                    ip,
                    universe,
                    fixtures,
                    fps,
                    delay_ms,
                } => Some(artnet_out::Want {
                    id: s.id,
                    ip: ip.clone(),
                    universe: *universe,
                    fixtures: vjc_core::dmx::resolve_fixtures(
                        fixtures, &s.slices, s.width, s.height,
                    ),
                    fps: *fps,
                    delay_ms: *delay_ms,
                }),
                _ => None,
            })
            .collect()
    }

    /// Renders the warped slices of each open screen into its own texture, and publishes
    /// the Syphon / NDI screens.
    fn render_screens(&mut self, rs: &eframe::egui_wgpu::RenderState) {
        let wants = self.net_wants();
        self.netout.sync(&wants);
        let art = self.artnet_wants();
        self.artnet.sync(&art);
        let open = self.output_screen_ids();
        let gone: Vec<Uuid> = self
            .screen_rt
            .keys()
            .filter(|id| !open.iter().any(|o| o == *id))
            .copied()
            .collect();
        for id in gone {
            if let Some(old) = self.screen_rt.remove(&id) {
                rs.renderer.write().free_texture(&old.id);
            }
        }
        let Some(comp) = self.comp.as_ref() else {
            return;
        };
        // Cards of screens whose card is off (or that were deleted) free their texture.
        if let Some(st) = &mut self.stamper {
            let on = &self.test_cards;
            st.retain_cards(|id| on.contains(id));
        }
        for id in open {
            let Some(scr) = self.project.output.screens.iter().find(|s| s.id == id) else {
                continue;
            };
            // A screen without slices shows the whole composition, still at its own size.
            let plain;
            let scr = if scr.slices.is_empty() {
                let mut s = scr.clone();
                s.slices
                    .push(vjc_core::mapping::new_slice(s.width, s.height));
                plain = s;
                &plain
            } else {
                scr
            };
            let size = (scr.width.max(1), scr.height.max(1));
            let r = self
                .slice_r
                .get_or_insert_with(|| slice_render::SliceRenderer::new(&rs.device));
            let fresh = self
                .screen_rt
                .get(&id)
                .is_none_or(|t| t.size != size || t.nearest != scr.pixel_perfect);
            if fresh {
                let (tex, view) = slice_render::make_screen_target(&rs.device, size.0, size.1);
                let filter = if scr.pixel_perfect {
                    wgpu::FilterMode::Nearest
                } else {
                    wgpu::FilterMode::Linear
                };
                let tid = rs
                    .renderer
                    .write()
                    .register_native_texture(&rs.device, &view, filter);
                if let Some(old) = self.screen_rt.insert(
                    id,
                    ScreenRt {
                        tex,
                        view,
                        size,
                        id: tid,
                        nearest: scr.pixel_perfect,
                        live: None,
                        ring: Default::default(),
                    },
                ) {
                    rs.renderer.write().free_texture(&old.id);
                }
            }
            // Delay: the slices draw into a scratch target and a frame queue feeds the
            // presented texture. A disabled screen skips the queue (black at once).
            let frames = if scr.enabled {
                vjc_core::mapping::delay_frames(scr.delay_ms, self.project.composition.fps)
            } else {
                0
            };
            let t = self.screen_rt.get_mut(&id).expect("just created");
            if frames == 0 {
                t.live = None;
                t.ring.clear();
            } else if t.live.is_none() {
                t.live = Some(slice_render::make_screen_target(&rs.device, size.0, size.1));
            }
            let target_view = t.live.as_ref().map_or(&t.view, |l| &l.1);
            // Each slice samples its own source; a routed layer / group has its own texture.
            let source = |sl: &vjc_model::Slice| {
                let view = comp.routed_view(sl.source).unwrap_or(&comp.target_view);
                (view, comp.size)
            };
            let dither = slice_render::dither_active(
                self.project.output.dither,
                self.project.composition.bit_depth,
            );
            r.render_dithered(&rs.device, &rs.queue, scr, &source, target_view, dither);
            // The test card goes into the screen target itself, so Art-Net, Syphon and NDI
            // see it too. Black-out and a disabled screen stay black.
            if self.test_cards.contains(&id) && !self.blackouts.contains(&id) && scr.enabled {
                self.stamper
                    .get_or_insert_with(|| slice_render::Stamper::new(&rs.device))
                    .stamp_card(&rs.device, &rs.queue, target_view, scr, size);
            }
            if let Some((live, _)) = &t.live {
                t.ring
                    .push(&rs.device, &rs.queue, live, &t.tex, size, frames);
            }
        }
        // Publish: the screen's own target when it has slices, else the composition.
        let fps = self.project.composition.fps;
        for w in &wants {
            let (tex, size) = match self.screen_rt.get(&w.id) {
                Some(t) => (&t.tex, t.size),
                None => (comp.readable(), comp.size),
            };
            self.netout.feed(
                &rs.device,
                &rs.queue,
                w.id,
                tex,
                size,
                fps,
                self.blackouts.contains(&w.id),
            );
        }
        for e in self.netout.poll(&rs.device) {
            self.status = e;
        }
        for w in &art {
            let (tex, size) = match self.screen_rt.get(&w.id) {
                Some(t) => (&t.tex, t.size),
                None => (comp.readable(), comp.size),
            };
            self.artnet.feed(
                &rs.device,
                &rs.queue,
                w.id,
                tex,
                size,
                self.blackouts.contains(&w.id),
            );
        }
        for e in self.artnet.poll(&rs.device) {
            self.status = e;
        }
    }

    /// Master row: bypass, Clear All (opacity and speed live in the Composition tab).
    /// Plays a Hold clip for `src` (a mouse press that stayed put, or a key down). It goes
    /// through the same target and beat snap resolution as any trigger; the clip it put
    /// in the air is remembered so the release can eject exactly that one.
    fn hold_fire(&mut self, src: hold::Src, deck: DeckId, layer: LayerId, column: u32) {
        if self.holds.iter().any(|r| r.src == src) {
            return;
        }
        let Some(clip) = vjc_core::clip_in(&self.project, deck, layer, column).map(|k| k.id) else {
            return;
        };
        let resolved = self.resolve_trigger(Command::TriggerClip {
            deck,
            layer,
            column,
        });
        let onto = match &resolved {
            Command::TriggerClipOn { onto, .. } => *onto,
            _ => layer,
        };
        self.hold_token += 1;
        let mut run = hold::Run {
            src,
            fired: vjc_core::HoldFired {
                deck,
                layer,
                column,
                clip,
                onto,
            },
            token: self.hold_token,
            onair: false,
            key: None,
        };
        // Queued first, so a trigger that runs later (beat snap) finds its Hold.
        if let Some(cmd) = self.snap_defer_tagged(resolved, run.token) {
            run.onair = self.exec_ok(cmd);
        }
        self.holds.push(run);
    }

    /// Runs the command of shortcut `id`; a trigger of a Hold clip holds for as long as that
    /// binding is down (its release calls `hold_release`).
    fn run_shortcut(&mut self, id: Uuid, c: Command) {
        match c {
            Command::TriggerClip {
                deck,
                layer,
                column,
            } if vjc_core::trigger_style_of(&self.project, deck, layer, column)
                == vjc_model::TriggerStyle::Hold =>
            {
                self.hold_fire(hold::Src::Binding(id), deck, layer, column);
            }
            c => self.exec(c),
        }
    }

    /// `run_shortcut` for a key shortcut: the run remembers the key that holds it.
    fn run_key_shortcut(&mut self, id: Uuid, c: Command, key: &str) {
        self.run_shortcut(id, c);
        if let Some(r) = self
            .holds
            .iter_mut()
            .find(|r| r.src == hold::Src::Binding(id) && r.key.is_none())
        {
            r.key = Some(key.to_string());
        }
    }

    /// Key up: lets go of every Hold that key started, even if its shortcut was removed or
    /// rebound while the key was down, and of those of the shortcuts bound to it now.
    fn key_up(&mut self, key: &str) {
        let mut srcs: Vec<hold::Src> = self
            .holds
            .iter()
            .filter(|r| r.key.as_deref() == Some(key))
            .map(|r| r.src)
            .collect();
        srcs.extend(
            self.project
                .shortcuts
                .iter()
                .filter(|s| vjc_core::shortcuts::key_of(s) == Some(key))
                .map(|s| hold::Src::Binding(s.id)),
        );
        for src in srcs {
            self.hold_release(src);
        }
    }

    /// An OSC shortcut fired. A message without an argument has no release to wait for, so
    /// it is a plain trigger, never a Hold run.
    fn osc_shortcut(&mut self, id: Uuid, c: Command, has_arg: bool) {
        if has_arg {
            self.run_shortcut(id, c);
        } else {
            self.exec(c);
        }
    }

    /// The button or key is up. A trigger still waiting for its beat is taken back and
    /// nothing is ejected; otherwise the layer is ejected only if the clip this Hold
    /// played is still the one in the air.
    fn hold_release(&mut self, src: hold::Src) {
        let Some(i) = self.holds.iter().position(|r| r.src == src) else {
            return;
        };
        let run = self.holds.remove(i);
        if !run.onair {
            self.snap_pending.retain(|(_, h)| h.token != run.token);
            return;
        }
        if let Some(cmd) = vjc_core::hold_release(&self.project, &self.playing(), &run.fired) {
            self.exec_ok(cmd);
        }
    }

    /// Mouse side of Hold, once per frame: decides an armed press and releases on button up.
    fn hold_step(
        &mut self,
        now: std::time::Instant,
        down: bool,
        pos: Option<egui::Pos2>,
    ) -> Option<std::time::Duration> {
        if let Some(arm) = self.hold_arm {
            match hold::step(&arm, now, down, pos, self.cell_drag.is_some()) {
                hold::Step::Wait(d) => return Some(d),
                hold::Step::Cancel => self.hold_arm = None,
                hold::Step::Fire => {
                    self.hold_arm = None;
                    self.hold_fire(hold::Src::Mouse, arm.deck, arm.layer, arm.column);
                }
            }
        }
        if !down {
            self.hold_release(hold::Src::Mouse);
        }
        None
    }

    fn hold_tick(&mut self, ctx: &egui::Context) {
        let (down, pos, focused) = ctx.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.latest_pos(),
                i.viewport().focused.unwrap_or(true),
            )
        });
        // Losing focus loses the key-up events: let go of everything.
        if !focused {
            self.hold_arm = None;
            for src in self.holds.iter().map(|r| r.src).collect::<Vec<_>>() {
                self.hold_release(src);
            }
            return;
        }
        if let Some(wait) = self.hold_step(std::time::Instant::now(), down, pos) {
            ctx.request_repaint_after(wait);
        }
    }

    /// Key shortcuts from the project, plus the learn flow. Nothing fires while a widget
    /// has keyboard focus (text field or a focused button), so Space never also presses it.
    fn shortcut_keys(&mut self, ctx: &egui::Context) {
        self.sc_ed.captured = false;
        // Key up lets go of a Hold, whatever has the focus now.
        let up: Vec<egui::Key> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: false,
                        ..
                    } => Some(*key),
                    _ => None,
                })
                .collect()
        });
        for key in up {
            self.key_up(key.name());
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let pressed: Vec<egui::Key> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if !modifiers.command && !modifiers.alt => Some(*key),
                    _ => None,
                })
                .collect()
        });
        if self.sc_armed() == Some(shortcut_editor::Src::Key) {
            for key in pressed {
                if key == egui::Key::Escape {
                    self.sc_ed.learn = None;
                    self.sc_ed.learn_all = false;
                } else if self.sc_capture(vjc_model::ControlSource::Key {
                    code: key.name().into(),
                }) {
                    break;
                }
            }
            return;
        }
        if self.learn {
            for key in pressed {
                if self.learn_midi && matches!(key, egui::Key::Delete | egui::Key::Backspace) {
                    self.unlearn_midi_osc();
                    continue;
                }
                if self.learn_midi && key != egui::Key::Escape {
                    continue;
                }
                self.learn_key(key);
            }
            return;
        }
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        if pressed.contains(&egui::Key::Escape) {
            self.preview_target = PreviewTarget::Composition;
        }
        // Delete / Backspace clear the selected cell (undoable).
        if pressed
            .iter()
            .any(|k| matches!(k, egui::Key::Delete | egui::Key::Backspace))
            && let Some(cell) = self.selected
        {
            let mut cmds = vec![];
            let deck = self
                .selected_deck
                .unwrap_or(self.project.composition.current_deck);
            self.cell_act(deck, cell, CellAct::Clear, &mut cmds);
            for c in cmds {
                self.exec(c);
            }
        }
        let rt = self.rt_ctx();
        for key in pressed {
            let name = key.name();
            let cmds: Vec<(Uuid, Command)> = self
                .project
                .shortcuts
                .iter()
                .filter(|s| vjc_core::shortcuts::key_of(s) == Some(name))
                .filter_map(|s| Some((s.id, vjc_core::shortcuts::activate(&self.project, &rt, s)?)))
                .collect();
            for (id, c) in cmds {
                self.run_key_shortcut(id, c, name);
            }
        }
    }

    /// MIDI, once per frame and never blocking: incoming messages learn or fire shortcuts,
    /// then LEDs of mapped notes are updated where the state changed.
    fn midi_pump(&mut self) {
        use vjc_core::midi as m;
        self.sc_ed.captured = false;
        while let Ok(ev) = self.midi.rx.try_recv() {
            if let midi_io::In::Msg(d, b, _) = &ev
                && self.prefs.midi.input_enabled(d)
            {
                self.midi_log
                    .push(io_monitor::midi_line(d, b, self.prefs.midi.middle_c));
            }
            let (device, bytes, at) = match ev {
                midi_io::In::Msg(d, ..) if !self.prefs.midi.input_enabled(&d) => continue,
                midi_io::In::Connected(d) => {
                    self.led_prev.retain(|(dev, _, _), _| *dev != d);
                    self.status = format!("MIDI device connected: {d}");
                    continue;
                }
                midi_io::In::Disconnected(d, dir) => {
                    self.status =
                        format!("MIDI {} disconnected: {d} (waiting for it)", dir.label());
                    continue;
                }
                midi_io::In::Msg(d, b, at) => (d, b, at),
            };
            if self.tc.feed_midi(&device, &bytes, &self.prefs.timecode, at) {
                continue;
            }
            let Some(msg) = m::parse(&bytes) else {
                continue;
            };
            // Editor learn: the row's source family is captured, never fired.
            if self.sc_armed() == Some(shortcut_editor::Src::Midi) {
                // Only the capture is swallowed: a release still lets go of its Hold.
                let off: Vec<Uuid> = self
                    .project
                    .shortcuts
                    .iter()
                    .filter(|s| m::releases(s, &device, msg))
                    .map(|s| s.id)
                    .collect();
                for id in off {
                    self.hold_release(hold::Src::Binding(id));
                }
                if let Some(src) = m::source_of(&device, msg) {
                    self.sc_capture(src);
                }
                continue;
            }
            if self.learn {
                if self.learn_midi
                    && let Some(target) = self.learn_target.clone()
                    && let Some(src) = m::source_of(&device, msg)
                {
                    let cmds = m::learn(&self.project, &target, src);
                    self.learned(format!("{device} -> {target}"), cmds);
                }
                continue;
            }
            // Note off (or a low CC) lets go of the Hold clips on that control.
            let off: Vec<Uuid> = self
                .project
                .shortcuts
                .iter()
                .filter(|s| m::releases(s, &device, msg))
                .map(|s| s.id)
                .collect();
            for id in off {
                self.hold_release(hold::Src::Binding(id));
            }
            let rt = self.rt_ctx();
            let cmds: Vec<(Uuid, Command)> = self
                .project
                .shortcuts
                .iter()
                .filter_map(|s| Some((s.id, m::command_for(&self.project, &rt, s, &device, msg)?)))
                .collect();
            for (id, c) in cmds {
                self.run_shortcut(id, c);
            }
        }
        let rt = self.rt_ctx();
        let active = |l| cell_active_of(&self.players, &self.gens, &self.origin, l);
        let next = m::led_states(&self.project, &rt, &active);
        let (sends, sent) =
            m::led_sends(&self.led_prev, next, |d| self.prefs.midi.output_enabled(d));
        for ((device, ch, note), on) in sends {
            self.midi.send(&device, m::led_bytes(ch, note, on));
        }
        self.led_prev = sent;
    }

    /// OSC, once per frame and never blocking: events from our own address space resolve
    /// directly, learned external addresses fire their shortcuts (or get learned), then the
    /// changed state goes out, rate limited per address.
    fn osc_pump(&mut self) {
        use vjc_core::osc as o;
        let cfg = osc_io::Cfg::from_prefs(&self.prefs.osc);
        if self.osc_cfg.as_ref() != Some(&cfg) {
            self.osc.configure(cfg.clone());
            self.osc_cfg = Some(cfg);
            self.osc_prev.clear();
        }
        self.sc_ed.captured = false;
        while let Ok(ev) = self.osc.rx.try_recv() {
            self.osc_log.push(io_monitor::osc_line(&ev));
            if self.sc_armed() == Some(shortcut_editor::Src::Osc) {
                let off: Vec<Uuid> = self
                    .project
                    .shortcuts
                    .iter()
                    .filter(|s| o::releases(s, &ev))
                    .map(|s| s.id)
                    .collect();
                for id in off {
                    self.hold_release(hold::Src::Binding(id));
                }
                if o::learnable(&ev) {
                    self.sc_capture(o::source_of(&ev));
                }
                continue;
            }
            if self.learn {
                if self.learn_midi
                    && o::learnable(&ev)
                    && let Some(target) = self.learn_target.clone()
                {
                    let cmds = o::learn(&self.project, &target, o::source_of(&ev));
                    self.learned(format!("{} -> {target}", ev.address), cmds);
                }
                continue;
            }
            // OSC 0 lets go of the Hold clips on that address.
            let off: Vec<Uuid> = self
                .project
                .shortcuts
                .iter()
                .filter(|s| o::releases(s, &ev))
                .map(|s| s.id)
                .collect();
            for id in off {
                self.hold_release(hold::Src::Binding(id));
            }
            let rt = self.rt_ctx();
            let direct: Vec<Command> = o::direct(&self.project, &rt, &ev).into_iter().collect();
            let bound: Vec<(Uuid, Command)> = self
                .project
                .shortcuts
                .iter()
                .filter_map(|s| Some((s.id, o::command_for(&self.project, &rt, s, &ev)?)))
                .collect();
            for c in direct {
                self.exec(c);
            }
            let has_arg = ev.value.is_some();
            for (id, c) in bound {
                self.osc_shortcut(id, c, has_arg);
            }
        }
        if !self.prefs.osc.out_enabled {
            self.osc_prev.clear();
            return;
        }
        let now = std::time::Instant::now();
        let rt = self.rt_ctx();
        let active = |l| cell_active_of(&self.players, &self.gens, &self.origin, l);
        let beat = self.clock.phase(now).floor() as i64;
        let next = o::out_states(&self.project, &rt, &active, beat);
        let due = self
            .osc_limit
            .filter(o::out_diff(&self.osc_prev, &next), now);
        for (addr, v) in due {
            if let Some(bytes) = o::encode(&addr, v) {
                self.osc.send(bytes);
            }
            self.osc_prev.insert(addr, v);
        }
    }

    /// DJ player packets, once per frame and never blocking: feeds the tracker and lets the
    /// follower steer the clock (not while Link is on).
    fn dj_pump(&mut self, now: std::time::Instant, allowed: bool) {
        let cfg = dj_io::Cfg::from_prefs(&self.prefs.dj);
        if self.dj_cfg.as_ref() != Some(&cfg) {
            self.dj.configure(cfg.clone());
            self.dj_cfg = Some(cfg);
        }
        while let Ok((p, at)) = self.dj.rx.try_recv() {
            self.dj_tracker.feed(p, at);
        }
        let stopped = self.project.composition.tempo.play_state == vjc_model::PlayState::Stop;
        self.dj_follow.update(
            &self.dj_tracker,
            self.prefs.dj.enabled && allowed,
            self.prefs.dj.follow_player,
            stopped,
            &mut self.clock,
            now,
        );
        let locked = !stopped
            && matches!(
                self.dj_follow.state,
                vjc_core::djlink::SyncState::Locked { .. }
            );
        self.mirror_dj_tempo(locked);
    }

    /// Copies the followed player's tempo into the project. Live state: the project is not
    /// marked dirty (no "unsaved changes" prompt because a fader moved) and no history entry
    /// is made; the value is simply saved along with the next explicit save.
    fn mirror_dj_tempo(&mut self, locked: bool) {
        if dj_io::mirror_bpm(&mut self.project, self.clock.bpm(), locked) {
            self.project_gen += 1;
        }
    }

    /// Art-Net input, once per frame and never blocking: changed channels learn (first one
    /// only) or fire their shortcuts.
    fn dmx_pump(&mut self) {
        use vjc_core::dmx as d;
        let cfg = dmx_io::Cfg::from_prefs(&self.prefs.dmx);
        if self.dmx_cfg.as_ref() != Some(&cfg) {
            self.dmx.configure(cfg.clone());
            self.dmx_cfg = Some(cfg);
        }
        let mut learned = false;
        self.sc_ed.captured = false;
        while let Ok(ev) = self.dmx.rx.try_recv() {
            if self.sc_armed() == Some(shortcut_editor::Src::Dmx) {
                self.sc_capture(d::source_of(&ev));
                continue;
            }
            if self.learn {
                if self.learn_midi
                    && !learned
                    && let Some(target) = self.learn_target.clone()
                {
                    learned = true;
                    self.status = format!("DMX {}.{} -> {target}", ev.universe, ev.channel);
                    for c in d::learn(&self.project, &target, d::source_of(&ev)) {
                        self.exec(c);
                    }
                }
                continue;
            }
            let rt = self.rt_ctx();
            let cmds: Vec<Command> = self
                .project
                .shortcuts
                .iter()
                .filter_map(|s| d::command_for(&self.project, &rt, s, &ev))
                .collect();
            for c in cmds {
                self.exec(c);
            }
        }
    }

    /// PNG of the thumbnail of the clip at (layer, column), 1-based from the bottom layer,
    /// of the current deck. Starts the decode when it is not cached yet (404 until ready).
    fn web_thumb(&mut self, layer: u32, column: u32) -> api_io::Reply {
        let c = &self.project.composition;
        let li = c.layers.len().checked_sub(layer as usize);
        let clip = li
            .and_then(|i| c.layers.get(i))
            .and_then(|l| {
                let col = column.checked_sub(1)?;
                c.decks
                    .iter()
                    .find(|d| d.id == c.current_deck)?
                    .clips
                    .iter()
                    .find(|k| k.layer == l.id && k.column == col)
            })
            .map(|k| (k.clip.id, k.clip.source.clone()));
        let miss = |m: &str| {
            Err(api_io::ApiErr {
                status: 404,
                msg: m.into(),
            })
        };
        let Some((id, vjc_model::Source::File { path })) = clip else {
            return miss("no thumbnail for this cell");
        };
        match self.thumbs.png(id, &path) {
            Some(png) => Ok(
                serde_json::json!({"png": png.iter().map(|b| char::from(*b)).collect::<String>()}),
            ),
            None => miss("thumbnail not ready"),
        }
    }

    /// Remote API, once per frame: applies the config, then answers the queued requests
    /// (the server threads wait for the reply, the render loop never waits for them).
    fn api_pump(&mut self, ctx: &egui::Context) {
        self.api.set_ctx(ctx);
        let cfg = api_io::Cfg::from_prefs(&self.prefs.api);
        if self.api_cfg.as_ref() != Some(&cfg) {
            self.api.configure(cfg.clone());
            self.api_cfg = Some(cfg);
        }
        if let Some(e) = self.audio.as_ref().and_then(|a| a.take_reopen_error()) {
            self.status = format!("Audio: could not use the new settings ({e}); kept the old ones");
        }
        api_ext::snap_expire(&mut self.snap_jobs);
        for _ in 0..api_io::MAX_PER_FRAME {
            let Ok(msg) = self.api.rx.try_recv() else {
                break;
            };
            self.api_handle(msg.req, msg.reply);
        }
    }

    /// Answers one API request on the UI thread. Snapshots reply later (after the GPU
    /// readback, see `api_ext::snap_service`); everything else replies right away.
    fn api_handle(&mut self, req: api_io::Req, reply: std::sync::mpsc::SyncSender<api_io::Reply>) {
        let rt = self.rt_ctx();
        let active = |l| cell_active_of(&self.players, &self.gens, &self.origin, l);
        match req {
            api_io::Req::Thumb(l, c) => {
                let r = self.web_thumb(l, c);
                let _ = reply.send(r);
            }
            api_io::Req::Ext(op, body) if api_ext::APP_OPS.contains(&op) => {
                // None: the reply comes later (snapshot after the GPU readback).
                if let Some(r) = self.api_app_op(op, &body, &reply) {
                    let _ = reply.send(r);
                }
            }
            req => {
                let (cmd, r) = api_io::answer(&self.project, &rt, &active, req);
                if let Some(c) = cmd {
                    self.exec(c);
                }
                let _ = reply.send(r);
            }
        }
    }

    /// Ops that need files or the GPU. None = the reply is sent later.
    fn api_app_op(
        &mut self,
        op: &str,
        body: &serde_json::Value,
        reply: &std::sync::mpsc::SyncSender<api_io::Reply>,
    ) -> Option<api_io::Reply> {
        use serde_json::json;
        let bad = |status, msg: &str| {
            Some(Err(api_io::ApiErr {
                status,
                msg: msg.into(),
            }))
        };
        let path_of = |k: &str| {
            body[k]
                .as_str()
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        match op {
            "load" => {
                let Some(path) = path_of("path").filter(|p| p.is_file()) else {
                    return bad(422, "path must be an absolute path of an existing file");
                };
                if self.dirty && body["force"] != true {
                    return bad(409, "unsaved changes; pass \"force\": true to discard them");
                }
                if self.load(path) {
                    Some(Ok(json!({"ok": true})))
                } else {
                    bad(422, "could not open the project (see the status bar)")
                }
            }
            "save" => {
                let ok = match path_of("path") {
                    Some(mut p) => {
                        match p.extension() {
                            None => {
                                p.set_extension("vjc");
                            }
                            Some(e) if e.eq_ignore_ascii_case("vjc") => {}
                            Some(_) => return bad(422, "path must end in .vjc"),
                        }
                        if !p.parent().is_some_and(|d| d.is_dir()) {
                            return bad(422, "the parent directory does not exist");
                        }
                        match api_ext::save_target(&p) {
                            Ok(p) => self.save_to(p),
                            Err(m) => return bad(422, m),
                        }
                    }
                    None if body["path"].is_null() && self.file.is_some() => {
                        match api_ext::save_target(&self.file.clone().unwrap_or_default()) {
                            Ok(p) => self.save_to(p),
                            Err(m) => return bad(422, m),
                        }
                    }
                    None => return bad(422, "no file yet: pass an absolute \"path\""),
                };
                if ok {
                    Some(Ok(json!({"ok": true, "path": self.file})))
                } else {
                    bad(500, "save failed (see the status bar)")
                }
            }
            "open_clips" => {
                let c = &self.project.composition;
                let layer = body["layer"]
                    .as_u64()
                    .and_then(|n| c.layers.len().checked_sub(n as usize))
                    .and_then(|i| c.layers.get(i))
                    .map(|l| l.id);
                let col = body["column"]
                    .as_u64()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|n| u32::try_from(n).ok());
                let (Some(layer), Some(col)) = (layer, col) else {
                    return bad(404, "no such layer");
                };
                // Past the last column is fine: the missing columns are added.
                let paths: Vec<PathBuf> = body["paths"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(PathBuf::from))
                    .collect();
                if paths.iter().any(|p| !p.is_absolute() || !p.exists()) {
                    return bad(422, "every path must be absolute and exist");
                }
                if col as usize > c.columns.len() + api_ext::MAX_FILES {
                    return bad(422, "column too far past the last one");
                }
                let (placed, ignored) = self.drop_many(layer, col, paths);
                Some(Ok(
                    json!({"ok": placed > 0, "placed": placed, "ignored": ignored}),
                ))
            }
            "monitor" => {
                let kind = body["kind"].as_str().unwrap_or("");
                if kind == "preview" {
                    return bad(501, "preview snapshots are not available; use composition");
                }
                let now = std::time::Instant::now();
                if self.snap_jobs.len() >= api_ext::SNAP_QUEUE
                    || self
                        .snap_last
                        .is_some_and(|t| now.duration_since(t) < api_ext::SNAP_EVERY)
                {
                    return bad(429, "snapshots are rate limited (4 per second)");
                }
                self.snap_last = Some(now);
                self.snap_jobs.push(api_ext::SnapJob::new(
                    body["format"] == "jpg",
                    reply.clone(),
                ));
                None
            }
            _ => bad(404, "not found"),
        }
    }

    /// Delete in MIDI/OSC learn: unmaps the picked target's MIDI and OSC controls.
    /// Runs the commands of an external-control learn. A controller that keeps streaming
    /// after the mapping exists yields no commands and must not rewrite the status line.
    fn learned(&mut self, label: String, cmds: Vec<Command>) {
        if cmds.is_empty() {
            return;
        }
        self.status = label;
        for c in cmds {
            self.exec(c);
        }
    }

    fn unlearn_midi_osc(&mut self) {
        let Some(target) = self.learn_target.clone() else {
            self.status = "Pick a target first (click a control), then Delete".into();
            return;
        };
        self.status = format!("Unmapped {target}");
        let mut cmds = vjc_core::midi::unlearn(&self.project, &target);
        cmds.extend(vjc_core::osc::unlearn(&self.project, &target));
        cmds.extend(vjc_core::dmx::unlearn(&self.project, &target));
        for c in cmds {
            self.exec(c);
        }
    }

    /// One key press in learn mode: Esc leaves, Delete unmaps the target, any other key
    /// maps to it (replacing its old key and taking the key from any other target).
    fn learn_key(&mut self, key: egui::Key) {
        use egui::Key;
        if key == Key::Escape {
            self.learn = false;
            self.learn_midi = false;
            self.learn_target = None;
            self.status = "Shortcut editing off".into();
            return;
        }
        let Some(target) = self.learn_target.clone() else {
            if matches!(key, Key::Delete | Key::Backspace) {
                self.status = "Pick a target first (click a control), then Delete".into();
            }
            return;
        };
        let cmds = if matches!(key, Key::Delete | Key::Backspace) {
            self.status = format!("Unmapped {target}");
            vjc_core::shortcuts::unlearn(&self.project, &target)
        } else {
            self.status = format!("{} -> {target}", key.name());
            vjc_core::shortcuts::learn(&self.project, &target, key.name())
        };
        for c in cmds {
            self.exec(c);
        }
    }

    /// Learn mode, end of frame: a click on a mapped widget picks it; the overlay shows
    /// each target's key. Commands from the clicks are dropped so nothing else happens.
    fn learn_pass(&mut self, ui: &egui::Ui, cmds: &mut Vec<Command>) {
        if !self.learn {
            return;
        }
        cmds.clear();
        let ctx = ui.ctx();
        if ctx.input(|i| i.pointer.primary_pressed())
            && let Some(pos) = ctx.input(|i| i.pointer.interact_pos())
            && let Some((_, a)) = self.targets.iter().rev().find(|(r, _)| r.contains(pos))
        {
            self.learn_target = Some(a.clone());
            self.status = if self.learn_midi {
                format!("{a}: move a MIDI, OSC or DMX control (Delete unmaps, Esc leaves)")
            } else {
                format!("{a}: press a key (Delete unmaps, Esc leaves)")
            };
        }
        let paint = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("learn_overlay"),
        ));
        let v = ui.visuals();
        let font = egui::TextStyle::Small.resolve(ui.style());
        for (rect, addr) in &self.targets {
            let picked = self.learn_target.as_deref() == Some(addr);
            if picked {
                paint.rect_stroke(*rect, 3.0, v.selection.stroke, egui::StrokeKind::Outside);
            }
            let label = if self.learn_midi {
                vjc_core::midi::label_for(&self.project, addr)
                    .or_else(|| vjc_core::osc::label_for(&self.project, addr))
                    .or_else(|| vjc_core::dmx::label_for(&self.project, addr))
            } else {
                vjc_core::shortcuts::key_for(&self.project, addr).map(str::to_owned)
            };
            if let Some(k) = label {
                let galley = paint.layout_no_wrap(k, font.clone(), v.strong_text_color());
                let r = egui::Rect::from_min_size(rect.min, galley.size() + egui::vec2(6.0, 2.0));
                paint.rect_filled(r, 3.0, v.selection.bg_fill);
                paint.galley(r.min + egui::vec2(3.0, 1.0), galley, v.strong_text_color());
            }
        }
    }

    /// Short DJ sync status for the clock area, and a longer hover text.
    fn dj_status_text(&self, link_on: bool) -> (String, String) {
        use vjc_core::djlink::SyncState;
        if link_on {
            return (
                "DJ paused".into(),
                "Link is on and wins over DJ players".into(),
            );
        }
        if let dj_io::Status::Failed(e) = self.dj.status() {
            return ("DJ error".into(), e);
        }
        match self.dj_follow.state {
            SyncState::Locked { player, bpm } => (
                format!("DJ P{player} {bpm:.1}"),
                format!("Following player {player} at {bpm:.2} BPM"),
            ),
            _ => ("DJ waiting".into(), "No player heard yet".into()),
        }
    }

    /// Tempo row: BPM, tap, nudge (hold), resync, half/double, beat lights, metronome.
    fn time_bar(&mut self, ui: &mut egui::Ui) {
        let now = std::time::Instant::now();
        let mut cmds = vec![];
        let (mut undo, mut redo) = (false, false);
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = theme::SP_2;
            let tbtn = vec2_btn();
            // Link is laid out first, on the right; the rest scrolls in what is left, so a
            // narrow window never overlaps it.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(theme::SP_3);
                let mut link = self.project.composition.tempo.link_enabled;
                let label = if link {
                    format!("Link ({})", self.link_sync.peers)
                } else {
                    "Link".to_owned()
                };
                let text = icons::with_text(ui, icons::i::LINK, &label);
                let lr = ui.add(egui::Button::new(text).selected(link));
                lr.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::Button, true, link, label.clone())
                });
                if help(lr, "link").clicked() {
                    link = !link;
                    cmds.push(Command::SetLink { enabled: link });
                }
                if self.prefs.dj.enabled {
                    let (text, tip) = self.dj_status_text(link);
                    ui.label(egui::RichText::new(text).color(theme::TEXT_MUTED))
                        .on_hover_text(tip);
                }
                egui::ScrollArea::horizontal()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = theme::SP_2;
            if !self.prefs.layouts.current.is_hidden("layer-transport") {
            let play = icons::name(
                ui.add(
                    egui::Button::new(icons::glyph(icons::i::PLAY).color(theme::ON_ACCENT))
                        .fill(theme::ACCENT)
                        .min_size(tbtn),
                ),
                "Play the selected clip",
                None,
            );
            if help(play, "transport-play").clicked()
                && let Some((layer, column)) = self.selected
            {
                cmds.push(Command::TriggerClip {
                    deck: self.project.composition.current_deck,
                    layer,
                    column,
                });
            }
            let pause = icons::name(
                ui.add(egui::Button::new(icons::glyph(icons::i::PAUSE)).min_size(tbtn)),
                "Pause or resume the clip playing on the selected layer (all layers when none is selected)",
                None,
            );
            if help(pause, "transport-pause").clicked() {
                let layer = self.selected.map(|s| s.0);
                let rt = self.rt_ctx();
                let scoped = layer.filter(|l| rt.active.iter().any(|a| a.0 == *l));
                if let Some(c) = vjc_core::shortcuts::toggle_pause(
                    &self.project,
                    &rt.active,
                    &rt.origin,
                    scoped,
                ) {
                    cmds.push(c);
                }
            }
            let stop = icons::name(
                ui.add(egui::Button::new(icons::glyph(icons::i::STOP)).min_size(tbtn)),
                "Stop all layers",
                None,
            );
            if help(stop, "transport-stop").clicked() {
                cmds.push(Command::ClearAll);
            }
            }
            ui.separator();
            ui.add_space(theme::SP_3);
            ui.label(
                egui::RichText::new("BPM")
                    .font(theme::bold_font(ui.ctx(), theme::FS_XS))
                    .color(theme::TEXT_MUTED),
            );
            let mut bpm = self.clock.bpm();
            let bpm_r = ui.add(
                egui::DragValue::new(&mut bpm)
                    .range(vjc_core::clock::MIN_BPM..=vjc_core::clock::MAX_BPM)
                    .fixed_decimals(2)
                    .speed(0.1),
            );
            if help(bpm_r, "bpm").changed() {
                cmds.push(Command::SetBpm { bpm });
            }
            for (txt, tip, delta) in [("\u{2212}", "BPM -1", -1.0), ("+", "BPM +1", 1.0)] {
                let b = icons::name(ui.add(egui::Button::new(txt).min_size(tbtn)), tip, None);
                if help(b, "bpm-step").clicked() {
                    cmds.push(Command::SetBpm {
                        bpm: (self.clock.bpm() + delta)
                            .clamp(vjc_core::clock::MIN_BPM, vjc_core::clock::MAX_BPM),
                    });
                }
            }
            let tap = ui.add(egui::Button::new(caps(ui, "TAP")).min_size(tbtn));
            tap.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Tap"));
            let tap = help(tap, "tap");
            self.targets.push((tap.rect, "/comp/tempo/tap".into()));
            if tap.clicked() {
                #[cfg(test)]
                let now = self.history_now.unwrap_or(now);
                cmds.push(Command::Tap { at: now });
            }
            for (state, icon, tip) in [
                (vjc_model::PlayState::Play, icons::i::PLAY, "Play tempo clock"),
                (vjc_model::PlayState::Pause, icons::i::PAUSE, "Pause tempo clock"),
                (vjc_model::PlayState::Stop, icons::i::STOP, "Stop tempo clock"),
            ] {
                let on = self.project.composition.tempo.play_state == state;
                let b = icons::with(
                    ui,
                    egui::Button::new(icons::glyph(icon))
                        .min_size(tbtn)
                        .selected(on),
                    tip,
                );
                // Transport, not an edit: no undo step (like TAP), but the project is saved.
                // Stop ejects every clip; Pause / Play also pause / resume the clips.
                if b.clicked() && self.project.composition.tempo.play_state != state {
                    cmds.push(Command::SetTransport { state });
                }
            }
            let minus = help(
                icons::button(ui, icons::i::CARET_DOUBLE_LEFT, "Nudge slower (hold)"),
                "nudge-slower",
            );
            let plus = help(
                icons::button(ui, icons::i::CARET_DOUBLE_RIGHT, "Nudge faster (hold)"),
                "nudge-faster",
            );
            let dir = if minus.is_pointer_button_down_on() {
                -1
            } else if plus.is_pointer_button_down_on() {
                1
            } else {
                0
            };
            if dir != self.nudge_dir {
                self.nudge_dir = dir;
                cmds.push(Command::Nudge { dir, at: now });
            }
            let rs = ui.add(egui::Button::new(caps(ui, "RESYNC")).min_size(tbtn));
            rs.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Resync"));
            if help(rs, "resync").clicked() {
                cmds.push(Command::Resync { at: now });
            }
            let bpm = self.clock.bpm();
            let half = ui.add(egui::Button::new("/2").min_size(tbtn));
            half.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "\u{00f7}2")
            });
            if help(half, "half-tempo").clicked() {
                cmds.push(Command::SetBpm {
                    bpm: (bpm / 2.0).max(vjc_core::clock::MIN_BPM),
                });
            }
            if help(ui.add(egui::Button::new("\u{00d7}2").min_size(tbtn)), "double-tempo").clicked() {
                cmds.push(Command::SetBpm {
                    bpm: (bpm * 2.0).min(vjc_core::clock::MAX_BPM),
                });
            }
            // 4 lights: the current beat of the bar glows, fading with the phase.
            let on = self.beat.rem_euclid(4) as usize;
            let fade = (1.0 - self.phase.fract()).clamp(0.0, 1.0) as f32;
            for i in 0..4 {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let col = if i == on {
                    let base = if i == 0 {
                        theme::BEAT_DOWN
                    } else {
                        theme::BEAT_OTHER
                    };
                    base.gamma_multiply(0.35 + 0.65 * fade)
                } else {
                    ui.visuals().widgets.inactive.bg_fill
                };
                ui.painter().circle_filled(rect.center(), 6.0, col);
            }
            let mut metro = self.project.composition.tempo.metronome;
            let mr = help(
                icons::toggle(ui, &mut metro, icons::i::METRONOME, "Metronome"),
                "metronome",
            );
            if mr.changed() {
                cmds.push(Command::SetMetronome { on: metro });
            }
            let mut rev = self.project.composition.master_reverse;
            let rr = icons::toggle(ui, &mut rev, icons::i::ARROWS_LEFT_RIGHT, "Reverse all clips");
            if rr.changed() {
                cmds.push(Command::SetMasterReverse { reverse: rev });
            }
            ui.separator();
            if !self.prefs.layouts.current.is_hidden("undo") {
            let (can_undo, can_redo) = (self.history.can_undo(), self.history.can_redo());
            undo = help(
                icons::button_enabled(ui, can_undo, icons::i::ARROW_COUNTER_CLOCKWISE, "Undo"),
                "transport-undo",
            )
            .clicked();
            redo = help(
                icons::button_enabled(ui, can_redo, icons::i::ARROW_CLOCKWISE, "Redo"),
                "transport-redo",
            )
            .clicked();
            }
            if self.prefs.timecode.source != vjc_io::prefs::TcSource::Off {
                ui.separator();
                let cur = self.tc.current(now);
                let text = match (self.tc.last, cur) {
                    (Some(l), Some(c)) => vjc_model::Timecode::from_seconds(l.fps, c).to_string(),
                    (Some(l), None) => l.to_string(),
                    (None, _) => "--:--:--:--".to_owned(),
                };
                let color = if cur.is_some() {
                    ui.visuals().text_color()
                } else {
                    ui.visuals().weak_text_color()
                };
                let r = ui
                    .label(
                        egui::RichText::new(format!("TC {text}"))
                            .monospace()
                            .color(color),
                    )
                    .on_hover_text(if cur.is_some() {
                        "External timecode"
                    } else {
                        "No timecode signal"
                    });
                help(r, "timecode");
            }
                        });
                    });
            });
        });
        if self.learn {
            cmds.clear();
        }
        for c in cmds {
            self.exec(c);
        }
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        // Only when the pref changes: egui's own Cmd +/- must keep working in between.
        let dragging = ui_scale_dragging(ui.ctx());
        if let Some(scale) = scale_to_apply(
            &mut self.applied_ui_scale,
            self.prefs.general.ui_scale,
            dragging,
        ) {
            ui.ctx().set_zoom_factor(scale);
            ui.ctx().request_repaint();
        }
        params::set_hidden(ui.ctx(), &self.prefs.layouts.current.hidden);
        self.prune_screen_state();
        self.prune_sel_slices();
        self.ui_body(ui);
        self.sync_clip_preview();
        // The help bar is drawn before the widgets, so it shows last frame's hover. If this
        // frame's hover differs (also when the pointer left a control), draw once more.
        let ctx = ui.ctx();
        if help::peek(ctx) != self.help_shown {
            ctx.request_repaint();
        }
    }

    fn ui_body(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.hold_tick(&ctx);
        // Before any panel draws: the panels register their learn targets too.
        self.targets.clear();
        self.help_shown = help::take_hovered(&ctx);
        if self.thumbs.poll(&ctx) || self.thumbs.is_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        for (level, msg) in self.toasts.drain(..) {
            widgets::toast(&ctx, &msg, level);
        }
        widgets::toasts_show(&ctx);
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(egui::Button::new("New").shortcut_text("Cmd+N"))
                    .clicked()
                {
                    ui.close();
                    self.new_project();
                }
                if ui
                    .add(egui::Button::new("Open...").shortcut_text("Cmd+O"))
                    .clicked()
                {
                    ui.close();
                    self.open_project();
                }
                if ui
                    .add(egui::Button::new("Save").shortcut_text("Cmd+S"))
                    .clicked()
                {
                    ui.close();
                    self.save();
                }
                if crate::widgets::btn(ui, "Save As...", crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.save_as();
                }
                ui.separator();
                if crate::widgets::btn(ui, "Relink missing media...", crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.relink_media();
                }
                if crate::widgets::btn(ui, "Collect media...", crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.collect_media();
                }
                ui.separator();
                if crate::widgets::btn(ui, "Import .avc...", crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.import();
                }
                ui.separator();
                if ui
                    .add(egui::Button::new("Preferences...").shortcut_text("Cmd+,"))
                    .clicked()
                {
                    ui.close();
                    self.prefs_open = true;
                }
            });
            ui.menu_button("Edit", |ui| {
                if crate::widgets::btn(ui, "Shortcuts...", crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.sc_ed.open = true;
                }
                if ui
                    .add_enabled(
                        self.history.can_undo(),
                        egui::Button::new("Undo").shortcut_text("Cmd+Z"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.undo();
                }
                if ui
                    .add_enabled(
                        self.history.can_redo(),
                        egui::Button::new("Redo").shortcut_text("Cmd+Shift+Z"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.redo();
                }
            });
            ui.menu_button("Layer", |ui| {
                let sel = self.selected.map(|s| s.0);
                if crate::widgets::btn_if(ui, sel.is_some(), "Remove layer", crate::widgets::Kind::Secondary)
                    .clicked()
                    && let Some(l) = sel
                {
                    ui.close();
                    self.request_remove_layer(l);
                }
                if let Some(l) = sel {
                    ui.separator();
                    let deck = self.project.composition.current_deck;
                    let mut cmds = vec![];
                    layer_ops_menu(ui, &self.project, l, deck, &mut cmds);
                    for c in cmds {
                        self.exec(c);
                    }
                }
            })
            .response
            // The panel tab is also called "Layer".
            .widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Layer menu")
            });
            ui.menu_button("Clip", |ui| {
                let deck = self.project.composition.current_deck;
                let sel = self.selected.and_then(|(l, c)| {
                    let k = vjc_core::clip_in(&self.project, deck, l, c)?;
                    let flags = self
                        .project
                        .composition
                        .layers
                        .iter()
                        .find(|x| x.id == l)
                        .map_or((false, false), |x| (x.ignore_column_trigger, x.fader_start));
                    Some((l, c, k.clone(), flags))
                });
                match sel {
                    Some((l, c, clip, flags)) => {
                        let seed = self.tc.last.unwrap_or(vjc_model::Timecode::new(
                            0,
                            0,
                            0,
                            0,
                            vjc_model::TcFps::F25,
                        ));
                        let mut cmds = vec![];
                        wave_ui::clip_bar_menu(ui, (deck, l, c), &clip, flags, seed, &mut cmds);
                        for cmd in cmds {
                            self.exec(cmd);
                        }
                    }
                    None => {
                        ui.weak("No clip selected");
                    }
                }
            })
            .response
            // The panel tab is also called "Clip".
            .widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Clip menu")
            });
            ui.menu_button("View", |ui| self.view_menu(ui));
            ui.menu_button("Shortcuts", |ui| {
                for (midi, label) in [(false, "Edit keyboard"), (true, "Edit MIDI")] {
                    let on = self.learn && self.learn_midi == midi;
                    if crate::widgets::choice(ui, on, label).clicked() {
                        ui.close();
                        self.learn = !on;
                        self.learn_midi = midi && self.learn;
                        self.learn_target = None;
                        self.status = match (self.learn, midi) {
                            (false, _) => "Shortcut editing off".into(),
                            (true, false) => "Click a cell, column, opacity, Tap or Bypass, then press a key (Esc leaves)".into(),
                            (true, true) => "Click a cell, column, opacity, Tap or Bypass, then move a MIDI, OSC or DMX control (Delete unmaps, Esc leaves)".into(),
                        };
                    }
                }
            });
            ui.menu_button("Output", |ui| {
                if self.monitors.is_empty() {
                    ui.label("No monitors found");
                }
                for m in self.monitors.clone() {
                    let on = self.output == Some(m.id);
                    if crate::widgets::choice(ui, on, format!("Fullscreen on {}", m.label()))
                        .clicked()
                    {
                        ui.close();
                        self.choose_fullscreen(m.id);
                    }
                }
                if crate::widgets::btn(ui, "Setup...", crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.output_setup_open = true;
                }
                if ui
                    .add(
                        egui::Button::new("Black-out all screens")
                            .selected(!self.blackouts.is_empty())
                            .shortcut_text("Cmd+Shift+B"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.toggle_blackout();
                }
                ui.menu_button("Pixel map grid", |ui| {
                    if self.project.output.screens.is_empty() {
                        ui.label("No screens");
                    }
                    for s in self.project.output.screens.clone() {
                        let mut tile = self.pixel_grids.get(&s.id).copied();
                        let mut on = tile.is_some();
                        ui.horizontal(|ui| {
                            crate::widgets::toggle(ui, &mut on, &s.name);
                            let mut t = tile.unwrap_or((128, 128));
                            crate::widgets::field_int(ui, &mut t.0, output_overlay::MIN_TILE..=16384, "W ", "");
                            crate::widgets::field_int(ui, &mut t.1, output_overlay::MIN_TILE..=16384, "H ", "");
                            tile = on.then_some(t);
                        });
                        match tile {
                            Some(t) => self.pixel_grids.insert(s.id, t),
                            None => self.pixel_grids.remove(&s.id),
                        };
                    }
                });
                ui.menu_button("Show test card", |ui| {
                    if self.project.output.screens.is_empty() {
                        ui.label("No screens");
                    }
                    for s in self.project.output.screens.clone() {
                        let mut on = self.test_cards.contains(&s.id);
                        if crate::widgets::toggle(ui, &mut on, &s.name).changed() {
                            if on {
                                self.test_cards.insert(s.id);
                            } else {
                                self.test_cards.remove(&s.id);
                            }
                        }
                    }
                });
                if crate::widgets::btn(ui, "Identify displays", crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.identify_at = Some(std::time::Instant::now());
                    // Repaint once more when it expires, even if nothing else animates.
                    ui.ctx().request_repaint_after(
                        output_overlay::IDENTIFY_FOR + std::time::Duration::from_millis(50),
                    );
                }
                crate::widgets::toggle(ui, &mut self.show_fps, "Show FPS");
                if crate::widgets::btn(ui, "Display info", crate::widgets::Kind::Secondary).clicked() {
                    ui.close();
                    self.display_info_open = true;
                }
                if crate::widgets::btn_if(ui, self.output.is_some() || !self.open_screens().is_empty(), "Close output", crate::widgets::Kind::Secondary)
                    .clicked()
                {
                    ui.close();
                    self.output = None;
                    self.closed_screens
                        .extend(self.project.output.screens.iter().map(|s| s.id));
                }
            });
        });
        let fxs = browser_fx_with(&self.fx.ffgl);
        let fxs = &*fxs;
        let mut opened = None;
        let lay = self.prefs.layouts.current.clone();
        // A panel clipped by a small window keeps its clipped size in egui memory; when the
        // window grows again, bring it back to the layout size.
        let win = ctx.content_rect().size();
        if window_grew(self.win_size, win) {
            self.force_panels = true;
        }
        self.win_size = win;
        let force = std::mem::take(&mut self.force_panels);
        let resizing = |id: &str| ctx.is_being_dragged(egui::Id::new(id).with("__resize"));
        // Status bar: version on the left, status text, local clock on the right.
        egui::Panel::bottom("status_bar")
            .exact_size(theme::STATUS_H)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.weak(format!("VJCraft {}", env!("CARGO_PKG_VERSION")));
                    ui.separator();
                    if !self.blackouts.is_empty() {
                        // Runtime only: never saved with the project.
                        ui.label(
                            egui::RichText::new("BLACK-OUT")
                                .strong()
                                .color(theme::ON_ACCENT)
                                .background_color(theme::DANGER),
                        );
                        ui.separator();
                    }
                    ui.label(&self.status);
                    if let Some(w) = &self.vram_warn {
                        ui.separator();
                        ui.label(egui::RichText::new(w).color(theme::DANGER));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(local_clock())
                                .monospace()
                                .color(theme::CLOCK_TEXT),
                        );
                        if self.show_stats {
                            ui.separator();
                            let txt = self.stats.text(
                                std::time::Instant::now(),
                                stats_bar::Inputs {
                                    p99_ms: self.frame_ms.p99(),
                                    vram_bytes: self.vram_bytes,
                                    gpu: &self.gpu_name,
                                    preload: self.preload.len(),
                                },
                            );
                            ui.weak(egui::RichText::new(txt).monospace())
                                .on_hover_text(
                                    "UI frames per second, frame time p99, UI frames later than 2 periods while playing, process CPU, estimated texture memory, GPU, decoders preloaded",
                                );
                        }
                        if self.frame_ms.len() > 0 {
                            ui.separator();
                            let stale = self.frame_label.0.is_none_or(|t| {
                                t.elapsed() >= std::time::Duration::from_millis(500)
                            });
                            if stale {
                                self.frame_label =
                                    (Some(std::time::Instant::now()), self.frame_ms.label());
                            }
                            ui.weak(egui::RichText::new(&self.frame_label.1).monospace())
                                .on_hover_text("Frame time (CPU, last 240 frames)");
                        }
                    });
                });
            });
        if self.persist {
            // Live app only: keeps the clock ticking while idle (2 Hz with the stats bar).
            let every = if self.show_stats {
                stats_bar::EVERY
            } else {
                std::time::Duration::from_secs(1)
            };
            ctx.request_repaint_after(every);
        }
        let help_shown = self.help_shown.and_then(help::text).unwrap_or(help::IDLE);
        let help_on = self.prefs.layouts.help;
        let browser_below = lay.browser && (lay.params || lay.monitors);
        if lay.browser && !browser_below {
            let mut p = egui::Panel::right("browser").default_size(lay.browser_w);
            if force {
                p = p.exact_size(lay.browser_w);
            }
            let r = p.show(ui, |ui| {
                if help_on {
                    // Contextual help box under the browser.
                    egui::Panel::bottom("help_box")
                        .resizable(false)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("Help")
                                    .font(theme::bold_font(ui.ctx(), theme::FS_SM)),
                            );
                            ui.add(egui::Label::new(help_shown).wrap());
                        });
                }
                self.browser
                    .set_recording(self.capture.as_ref().map(|c| c.path()));
                let (o, changed) = self.browser.show(ui, fxs, &mut self.prefs.general);
                opened = o;
                if changed {
                    self.prefs_dirty = true;
                }
            });
            self.track_size(
                |l| &mut l.browser_w,
                r.response.rect.width(),
                resizing("browser"),
            );
        }
        let mut pcmds = vec![];
        // Only the grid's minimum is guaranteed (80 px + deck tabs + top bars); the rest
        // is up to the user. The real window height bounds what the ui reports.
        let win_h = ctx.content_rect().height();
        if self.prefs.layouts.help && !lay.browser {
            self.help_bar(ui);
        }
        // 40 px for the deck tabs + add buttons, 40 for the status line and separator.
        let lay_n = self.project.composition.layers.len().max(1);
        let room = ui.available_height().min(win_h) - GRID_MIN - 80.0;
        if lay.params || lay.monitors {
            let max = (room - self.topbars_h - self.transport_h).max(40.0);
            let want = if lay.monitors {
                lay.params_h.max(lay.monitors_h)
            } else {
                lay.params_h
            };
            // Grid height fits its layers: the bottom panels take the rest.
            let grid_need = lay_n as f32 * theme::LAYER_ROW_H
                + 2.0 * theme::COLUMN_HEAD_H
                + theme::DECK_TABS_H
                + 56.0;
            let fit = (ui.available_height() - self.transport_h - grid_need).clamp(40.0, max);
            let h = if force && !self.persist {
                want
            } else {
                want.max(fit)
            }
            .min(max);
            let mut p = egui::Panel::bottom("params")
                .default_size(h)
                .resizable(true)
                .size_range(40.0..=max);
            if force {
                p = p.exact_size(h);
            }
            let r = p.show(ui, |ui| {
                if browser_below {
                    let mut bp = egui::Panel::right("browser")
                        .resizable(true)
                        .default_size(lay.browser_w)
                        .size_range(160.0..=(ui.available_width() * 0.4).max(160.0));
                    if force {
                        bp = bp.exact_size(lay.browser_w);
                    }
                    let r = bp.show(ui, |ui| {
                        if help_on {
                            egui::Panel::bottom("help_box")
                                .resizable(false)
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("Help")
                                            .font(theme::bold_font(ui.ctx(), theme::FS_SM)),
                                    );
                                    ui.add(egui::Label::new(help_shown).wrap());
                                });
                        }
                        self.browser
                            .set_recording(self.capture.as_ref().map(|c| c.path()));
                        let (o, changed) = self.browser.show(ui, fxs, &mut self.prefs.general);
                        opened = o;
                        if changed {
                            self.prefs_dirty = true;
                        }
                    });
                    self.track_size(
                        |l| &mut l.browser_w,
                        r.response.rect.width(),
                        resizing("browser"),
                    );
                }
                if lay.monitors {
                    egui::Panel::left("monitor_col")
                        .resizable(true)
                        .default_size(
                            (ui.available_width() * 0.22).clamp(theme::MONITOR_COL_W, 380.0),
                        )
                        .size_range(theme::MONITOR_MIN_W..=ui.available_width() * 0.5)
                        .show(ui, |ui| self.monitor_column(ui));
                }
                if lay.params {
                    egui::Panel::right("clip_panel")
                        .resizable(true)
                        .default_size(theme::CLIP_PANEL_W)
                        .size_range(theme::CLIP_PANEL_MIN_W..=ui.available_width() * 0.7)
                        .show(ui, |ui| self.clip_panel(ui, &mut pcmds));
                    self.params_panel(ui, &mut pcmds);
                }
            });
            let bottom = r.response.rect.height();
            self.track_size(|l| &mut l.params_h, bottom, resizing("params"));
            if lay.monitors {
                // One shared bottom panel: keep both heights in step so a preset with
                // monitors restores what the user dragged to.
                self.track_size(|l| &mut l.monitors_h, bottom, resizing("params"));
            }
        }
        for c in pcmds {
            self.exec(c);
        }
        // Transport bar: under the grid, above the property panels.
        let r = egui::Panel::bottom("transport")
            .exact_size(theme::TRANSPORT_H)
            .frame(
                egui::Frame::new()
                    .fill(theme::HEADER_BG)
                    .inner_margin(egui::Margin::symmetric(6, 4)),
            )
            .show(ui, |ui| self.time_bar(ui));
        self.transport_h = r.response.rect.height();
        match opened {
            Some(browser::Opened::File(path)) => match self.selected {
                Some((l, c)) => self.drop_cell(l, c, path),
                None => self.status = "Select a cell to load the file".into(),
            },
            Some(browser::Opened::Project(path)) if self.confirm_discard() => {
                self.load(path);
            }
            Some(browser::Opened::Project(_)) | None => {}
        }
        if ctx.input(|i| i.pointer.any_released())
            && let Some(item) = egui::DragAndDrop::payload::<browser::Item>(&ctx)
            && let Some(pos) = ctx.pointer_latest_pos()
        {
            self.browser_drop((*item).clone(), pos);
        }
        self.layer_rects.clear();
        self.presets_frame(&ctx);
        self.palette_window(&ctx);
        self.shortcut_keys(&ctx);
        self.layout_keys(&ctx);
        self.collect_pump();
        self.midi_pump();
        self.osc_pump();
        self.dmx_pump();
        self.api_pump(&ctx);

        // Undo/redo shortcuts (Shift variant first: it also matches plain Cmd+Z).
        let redo_key = egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::Z,
        );
        let undo_key = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
        if ctx.input_mut(|i| i.consume_shortcut(&redo_key)) {
            self.redo();
        } else if ctx.input_mut(|i| i.consume_shortcut(&undo_key)) {
            self.undo();
        }

        let key = |k| egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, k);
        if ctx.input_mut(|i| i.consume_shortcut(&key(egui::Key::S))) {
            self.save();
        } else if ctx.input_mut(|i| i.consume_shortcut(&key(egui::Key::O))) {
            self.open_project();
        } else if ctx.input_mut(|i| i.consume_shortcut(&key(egui::Key::N))) {
            self.new_project();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
            self.prefs_open = true;
        }
        self.prefs_window(&ctx);
        if self.midi_monitor_open {
            let hint = format!(
                "Last {} messages. Note 60 is {}.",
                io_monitor::MAX_LINES,
                self.prefs.midi.middle_c.note_name(60)
            );
            self.midi_monitor_open =
                io_monitor::window(&ctx, "MIDI monitor", &mut self.midi_log, &hint);
        }
        if self.osc_monitor_open {
            let hint = format!("Last {} messages.", io_monitor::MAX_LINES);
            self.osc_monitor_open =
                io_monitor::window(&ctx, "OSC monitor", &mut self.osc_log, &hint);
        }
        self.shortcut_editor_window(&ctx);
        self.recovery_window(&ctx);
        self.relink_window(&ctx);
        self.save_layout_window(&ctx);
        self.overwrite_layout_window(&ctx);
        self.remove_layer_modal(&ctx);
        self.hap_windows(&ctx);
        self.output_setup_window(&ctx);
        self.display_info_window(&ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            let stay = if self.dirty {
                !self.confirm_discard()
            } else if quit_needs_confirm(self.prefs.general.confirm_quit, self.dirty) {
                rfd::MessageDialog::new()
                    .set_title("Quit VJCraft")
                    .set_description("Quit VJCraft?")
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    != rfd::MessageDialogResult::Yes
            } else {
                false
            };
            if stay {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
        self.autosave();
        let title = self.window_title();
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        // File drops onto grid cells.
        self.last_pointer = ctx
            .input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()))
            .or(self.last_pointer);
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        // Files dropped on the Output setup window are a guide image, not clips.
        self.poll_guides(&ctx);
        let dropped = self.out_take_drops(&ctx, dropped);
        if !dropped.is_empty() {
            match resolve_drop(&self.cells, self.last_pointer, self.selected) {
                Some((l, c, w)) => {
                    let how = match w {
                        DropWhere::Pointer => "under pointer",
                        DropWhere::Selected => "selected cell (no pointer position)",
                        DropWhere::FirstEmpty => "first empty cell of bottom layer",
                        DropWhere::FirstCell => "first cell of bottom layer",
                    };
                    let (n, ignored) = self.drop_many(l, c, dropped);
                    let mut msg = format!("Dropped {n} file(s) from column {} ({how})", c + 1);
                    if ignored > 0 {
                        msg.push_str(&format!(", {ignored} unsupported ignored"));
                    }
                    self.status = msg;
                }
                None => self.status = "No grid cells to drop on".into(),
            }
        }
        self.prune_drags();
        let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let drag_target = if dragging {
            resolve_drop(&self.cells, self.last_pointer, self.selected).map(|(l, c, _)| (l, c))
        } else if let Some(src) = self.cell_drag {
            let alt = ctx.input(|i| i.modifiers.alt);
            let at = ctx.input(|i| i.pointer.latest_pos());
            if let Some(p) = at {
                let name = self
                    .project
                    .composition
                    .decks
                    .iter()
                    .find(|d| d.id == self.project.composition.current_deck)
                    .and_then(|d| d.clip_at(src.0, src.1))
                    .map_or("", |k| k.name.as_str());
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    egui::Id::new("cell_drag_ghost"),
                ));
                painter.text(
                    p + egui::vec2(12.0, 12.0),
                    egui::Align2::LEFT_TOP,
                    format!("{} {name}", if alt { "Copy" } else { "Move" }),
                    egui::FontId::proportional(theme::FS_SM),
                    theme::CELL_NAME_TEXT,
                );
            }
            at.and_then(|p| self.cells.iter().find(|c| c.rect.contains(p)))
                .map(|c| (c.layer, c.col))
                .filter(|t| *t != src)
        } else {
            None
        };
        let mut pick: Option<(LayerId, u32)> = None;
        let mut acts: Vec<(LayerId, u32, CellAct)> = vec![];
        let mut clip_acts: Vec<(LayerId, u32, wave_ui::ClipAct)> = vec![];
        let mut layer_dropped: Option<LayerId> = None;
        let mut remove_layer_req: Option<LayerId> = None;
        self.paint_layer_drop_line(&ctx);
        let has_clipboard = self.clip_clipboard.is_some();

        if !self.warnings.is_empty() {
            ui.collapsing(format!("{} import warnings", self.warnings.len()), |ui| {
                for w in &self.warnings {
                    ui.label(w);
                }
            });
        }
        let mut convert: Option<(PathBuf, DeckId, LayerId, u32)> = None;
        for (lid, (deck, col, p)) in &self.players {
            if let Some(e) = p.error() {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::ERROR_TEXT, format!("{}: {e}", p.path.display()));
                    // No decoder for the codec: offer a re-encode instead of a dead clip. A
                    // damaged or non-video file gets none (decided once, not per frame).
                    if p.can_convert()
                        && crate::widgets::btn(
                            ui,
                            "Convert to HAP...",
                            crate::widgets::Kind::Secondary,
                        )
                        .clicked()
                    {
                        convert = Some((p.path.clone(), *deck, *lid, *col));
                    }
                });
            }
        }
        if let Some((src, deck, layer, column)) = convert {
            let clip = vjc_core::clip_in(&self.project, deck, layer, column).map(|k| k.id);
            self.hap_ask = Some(HapAsk {
                src,
                cell: Some((deck, layer, column)),
                clip,
                variant: None,
            });
        }
        let dials = self.dials();
        let cf_pos = {
            let actx = self.anim_ctx(&dials);
            self.project
                .composition
                .crossfader
                .as_ref()
                .map_or(vjc_core::crossfader::CENTER, |x| {
                    vjc_core::anim::effective_number(&x.position, &actx)
                })
        };
        let recording = self.capture.is_some();
        let rec_dropped = self.capture.as_ref().map(|c| c.dropped());
        let rec_info = self.capture.as_ref().map(|c| c.info());
        let master_bypass = self.master_bypass;
        let mut cmds: Vec<Command> = vec![];
        let mut cells = vec![];
        let ncols = self.project.composition.columns.len() as u32;
        let cur = self.project.composition.current_deck;
        let names: Vec<(Uuid, String)> = self
            .project
            .composition
            .columns
            .iter()
            .map(|c| (c.id, c.name.clone()))
            .collect();
        let deck_idx = self
            .project
            .composition
            .decks
            .iter()
            .position(|d| d.id == cur);
        if let Some((id, _)) = &self.col_renaming
            && !names.iter().any(|(c, _)| c == id)
        {
            self.col_renaming = None;
        }
        let comp = &self.project.composition;
        if lay.grid {
            // Clip the grid to its own area: nothing of it may paint over the bottom panel.
            let full = ui.available_rect_before_wrap();
            let tabs_h = theme::DECK_TABS_H.min(full.height());
            let area =
                egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, full.max.y - tabs_h));
            let tabs_area = egui::Rect::from_min_max(egui::pos2(full.min.x, area.max.y), full.max);
            self.grid_rect = area;
            let mut gui = ui.new_child(egui::UiBuilder::new().max_rect(area));
            gui.set_clip_rect(area.intersect(ui.clip_rect()));
            let parent_ui = &mut *ui;
            // Registered under the cells: a click that lands on none of them.
            let empty_click = gui
                .interact(area, gui.id().with("grid_empty"), egui::Sense::click())
                .clicked();
            if empty_click {
                self.preview_target = PreviewTarget::Composition;
            }
            let ui = &mut gui;
            let mut ghost_hit = false;
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .max_height(area.height().max(0.0))
                .show(ui, |ui| {
                    let ghost_cols = (((area.width() - theme::LAYER_HEAD_W - 1.0)
                        / (theme::CELL_W + 1.0))
                        .floor() as i64
                        - i64::from(ncols))
                    .clamp(0, 64);
                    egui::Grid::new("grid").spacing([1.0, 1.0]).show(ui, |ui| {
                        ui.scope(|ui| {
                            let (hrect, _) = ui.allocate_exact_size(
                                egui::vec2(theme::LAYER_HEAD_W, 2.0 * theme::COLUMN_HEAD_H),
                                egui::Sense::hover(),
                            );
                            self.master_rect = Some(hrect);
                            grid_ui::composition_header(
                                ui,
                                hrect,
                                grid_ui::MasterState {
                                    bypass: master_bypass,
                                    recording,
                                    dropped: rec_dropped,
                                    rec_info: rec_info.clone(),
                                    layers: comp.layers.len(),
                                    columns: ncols as usize,
                                    blend: self.prefs.general.default_blend.clone(),
                                },
                                &mut cmds,
                                &mut self.targets,
                            );
                        });
                        let all_col_ids: Vec<Uuid> = comp.columns.iter().map(|c| c.id).collect();
                        let cols_on_air = cols_on_air(&self.players, &self.gens);
                        for (ci, (cid, n)) in names.iter().enumerate() {
                            if let Some((rid, buf)) =
                                self.col_renaming.as_mut().filter(|(r, _)| r == cid)
                            {
                                let rid = *rid;
                                let e = ui.add_sized(
                                    egui::vec2(theme::CELL_W, theme::COLUMN_HEAD_H),
                                    egui::TextEdit::singleline(buf),
                                );
                                e.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::TextEdit,
                                        true,
                                        "Column name",
                                    )
                                });
                                if !e.has_focus() && !e.lost_focus() {
                                    e.request_focus();
                                }
                                if e.lost_focus() {
                                    let ok = ui.input(|i| !i.key_pressed(egui::Key::Escape));
                                    if ok && !buf.trim().is_empty() && buf != n {
                                        cmds.push(Command::RenameColumn {
                                            column: rid,
                                            name: buf.trim().to_string(),
                                        });
                                    }
                                    self.col_renaming = None;
                                }
                                continue;
                            }
                            let (r, nr) = ui
                                .scope(|ui| {
                                    let playing_here: Vec<LayerId> = comp
                                        .layers
                                        .iter()
                                        .map(|l| l.id)
                                        .filter(|l| {
                                            active_of(&self.players, &self.gens, *l)
                                                == Some((cur, ci as u32))
                                        })
                                        .collect();
                                    // Two rows: the arrow row triggers, the name row selects
                                    // (double click / context menu rename). Stable ids: a
                                    // double click needs the same widget on both clicks.
                                    let (rect, hover) = ui
                                        .push_id(("colhead", cid), |ui| {
                                            ui.allocate_exact_size(
                                                egui::vec2(
                                                    theme::CELL_W,
                                                    2.0 * theme::COLUMN_HEAD_H,
                                                ),
                                                egui::Sense::hover(),
                                            )
                                        })
                                        .inner;
                                    let top = egui::Rect::from_min_size(
                                        rect.min,
                                        egui::vec2(rect.width(), theme::COLUMN_HEAD_H),
                                    );
                                    let bot = egui::Rect::from_min_max(
                                        egui::pos2(rect.left(), top.bottom()),
                                        rect.max,
                                    );
                                    let triggers =
                                        !self.prefs.layouts.current.is_hidden("column-triggers");
                                    let r = ui.interact(
                                        top,
                                        egui::Id::new(("colhead-trigger", cid)),
                                        if triggers {
                                            egui::Sense::click()
                                        } else {
                                            egui::Sense::hover()
                                        },
                                    );
                                    let nr = ui.interact(
                                        bot,
                                        egui::Id::new(("colhead-name", cid)),
                                        egui::Sense::click(),
                                    );
                                    r.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::Button,
                                            true,
                                            format!("Trigger column {n}"),
                                        )
                                    });
                                    nr.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::Button,
                                            true,
                                            format!("Select column {n}"),
                                        )
                                    });
                                    grid_ui::column_head(
                                        ui,
                                        rect,
                                        n,
                                        self.col_sel == Some(ci as u32)
                                            || self.col_marks.contains(cid),
                                        &hover,
                                        triggers,
                                    );
                                    if let Some(c) = theme::palette(comp.columns[ci].color) {
                                        ui.painter().rect_filled(
                                            egui::Rect::from_min_size(
                                                rect.min,
                                                egui::vec2(rect.width(), 4.0),
                                            ),
                                            0.0,
                                            c,
                                        );
                                    }
                                    let r = help(r, "column");
                                    let nr = help(nr, "column");
                                    let stop_rect = egui::Rect::from_min_size(
                                        rect.right_top() + egui::vec2(-22.0, 2.0),
                                        egui::vec2(20.0, theme::COLUMN_HEAD_H - 4.0),
                                    );
                                    if triggers {
                                        let stop = ui
                                            .scope_builder(
                                                egui::UiBuilder::new().max_rect(stop_rect),
                                                |ui| {
                                                    ui.scope(|ui| {
                                                        if playing_here.is_empty() {
                                                            ui.set_opacity(0.4);
                                                        }
                                                        grid_ui::tool(
                                                            ui,
                                                            stop_rect,
                                                            icons::glyph(icons::i::STOP),
                                                            &format!("Stop column {n}"),
                                                            None,
                                                            None,
                                                        )
                                                    })
                                                    .inner
                                                },
                                            )
                                            .inner;
                                        if help(stop, "column-stop").clicked() {
                                            cmds.extend(
                                                playing_here
                                                    .into_iter()
                                                    .map(|layer| Command::ClearLayer { layer }),
                                            );
                                        }
                                    }
                                    (r, nr)
                                })
                                .inner;
                            // The whole header (arrow row and name row) is the learn target.
                            self.targets.push((
                                r.rect.union(nr.rect),
                                vjc_core::shortcuts::column_address(ci as u32),
                            ));
                            if r.clicked() {
                                cmds.push(Command::TriggerColumn {
                                    deck: cur,
                                    column: ci as u32,
                                });
                            }
                            if nr.double_clicked() {
                                self.col_renaming = Some((*cid, n.clone()));
                            } else if nr.clicked() {
                                let (toggle, extend) =
                                    ui.input(|i| (i.modifiers.command, i.modifiers.shift));
                                col_ops::click_marks(
                                    &mut self.col_marks,
                                    &all_col_ids,
                                    ci,
                                    self.col_sel,
                                    toggle,
                                    extend,
                                );
                                self.col_sel = Some(ci as u32);
                            }
                            let col_ap = comp.columns[ci].autopilot.clone();
                            let col_snap = comp.columns[ci].beat_snap;
                            let col_color = comp.columns[ci].color;
                            nr.context_menu(|ui| {
                                if crate::widgets::btn(
                                    ui,
                                    "Rename column",
                                    crate::widgets::Kind::Secondary,
                                )
                                .clicked()
                                {
                                    self.col_renaming = Some((*cid, n.clone()));
                                    ui.close();
                                }
                                if let Some(color) = wave_ui::color_menu(ui, col_color) {
                                    cmds.push(Command::SetColor {
                                        target: vjc_core::ColorTarget::Column(*cid),
                                        color,
                                    });
                                }
                                ui.separator();
                                let aim = col_ops::targets(&self.col_marks, *cid);
                                for op in col_ops::ColOp::ALL {
                                    let label = if aim.len() > 1 {
                                        format!("{} {} columns", op.label(), aim.len())
                                    } else {
                                        format!("{} column", op.label())
                                    };
                                    if crate::widgets::btn(
                                        ui,
                                        &label,
                                        crate::widgets::Kind::Secondary,
                                    )
                                    .clicked()
                                    {
                                        match col_ops::commands(
                                            &self.project,
                                            &aim,
                                            op,
                                            &cols_on_air,
                                        ) {
                                            Ok(c) => cmds.extend(c),
                                            Err(e) => self.status = e.to_string(),
                                        }
                                        ui.close();
                                    }
                                }
                                ui.separator();
                                autopilot_ui::column_menu(ui, *cid, &col_ap, &mut cmds);
                                autopilot_ui::column_snap_menu(ui, *cid, col_snap, &mut cmds);
                            });
                        }

                        for _ in 0..ghost_cols {
                            let (r, resp) = ui.allocate_exact_size(
                                egui::vec2(theme::CELL_W, 2.0 * theme::COLUMN_HEAD_H),
                                egui::Sense::click(),
                            );
                            ui.painter().rect_filled(
                                r,
                                theme::CELL_ROUNDING,
                                theme::COLUMN_HEAD_BG,
                            );
                            ghost_hit |= resp.clicked();
                            egui::Popup::menu(&resp).show(|ui| {
                                if crate::widgets::btn(
                                    ui,
                                    "Add column",
                                    crate::widgets::Kind::Secondary,
                                )
                                .clicked()
                                {
                                    cmds.push(Command::AddColumn {
                                        column: vjc_model::Column {
                                            color: None,
                                            autopilot: Default::default(),
                                            beat_snap: None,
                                            id: Uuid::new_v4(),
                                            name: (ncols + 1).to_string(),
                                        },
                                    });
                                    ui.close();
                                }
                            });
                        }
                        ui.end_row();
                        for li in 0..comp.layers.len() {
                            let lid = comp.layers[li].id;
                            let group_head = comp.layers[li]
                                .group
                                .filter(|g| li == 0 || comp.layers[li - 1].group != Some(*g))
                                .and_then(|g| comp.group(g));
                            if let Some(g) = group_head {
                                group_header(
                                    ui,
                                    &self.project,
                                    g,
                                    ncols,
                                    (&mut self.sel_group, &mut self.panel_tab, &mut self.targets),
                                    &mut cmds,
                                );
                            }
                            let in_group = comp.layers[li].group.is_some();
                            let group_folded = comp.layers[li]
                                .group
                                .and_then(|g| comp.group(g))
                                .is_some_and(|g| g.folded);
                            // Layers of a folded group have no row; a folded layer is one thin row.
                            if group_folded {
                                continue;
                            }
                            if comp.layers[li].folded {
                                let layer = &comp.layers[li];
                                let (hr, hresp) = ui.allocate_exact_size(
                                    egui::vec2(theme::LAYER_HEAD_W, theme::FOLDED_ROW_H),
                                    egui::Sense::click(),
                                );
                                ui.painter().rect_filled(hr, 0.0, theme::HEADER_BG);
                                if let Some(c) = theme::palette(layer.color) {
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(
                                            hr.min,
                                            egui::vec2(4.0, hr.height()),
                                        ),
                                        0.0,
                                        c,
                                    );
                                }
                                ui.painter().text(
                                    hr.left_center() + egui::vec2(10.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    icons::i::CARET_RIGHT,
                                    theme::icon_font(theme::FS_XS),
                                    theme::TEXT_MUTED,
                                );
                                ui.painter().text(
                                    hr.left_center() + egui::vec2(24.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    &layer.name,
                                    egui::FontId::proportional(theme::FS_XS),
                                    theme::TEXT_MUTED,
                                );
                                hresp.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        true,
                                        format!("Unfold layer {}", layer.name),
                                    )
                                });
                                if hresp.clicked() {
                                    cmds.push(Command::SetLayerFolded {
                                        layer: lid,
                                        folded: false,
                                    });
                                }
                                for _ in 0..i64::from(ncols) + ghost_cols {
                                    ui.allocate_exact_size(
                                        egui::vec2(theme::CELL_W, theme::FOLDED_ROW_H),
                                        egui::Sense::hover(),
                                    );
                                }
                                ui.end_row();
                                continue;
                            }
                            ui.scope(|ui| {
                                ui.spacing_mut().slider_width = theme::LAYER_ROW_H - 16.0;
                                ui.spacing_mut().slider_rail_height = 4.0;
                                let layer = &comp.layers[li];
                                let (hr, _) = ui.allocate_exact_size(
                                    egui::vec2(theme::LAYER_HEAD_W, theme::LAYER_ROW_H),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(hr, 0.0, theme::HEADER_BG);
                                if let Some(c) = theme::palette(layer.color) {
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(
                                            hr.min,
                                            egui::vec2(4.0, hr.height()),
                                        ),
                                        0.0,
                                        c,
                                    );
                                }
                                let at = |x: f32, y: f32, w: f32, h: f32| {
                                    egui::Rect::from_min_size(
                                        hr.min + egui::vec2(x, y),
                                        egui::vec2(w, h),
                                    )
                                };
                                let lctx = ui.ctx().clone();
                                let big = |t: &str| {
                                    egui::RichText::new(t)
                                        .font(theme::bold_font(&lctx, theme::FS_BASE))
                                };
                                let small = |t: &str| {
                                    egui::RichText::new(t)
                                        .font(theme::bold_font(&lctx, theme::FS_XS))
                                };
                                // Name + select
                                let name_rect = at(
                                    theme::lh::X0,
                                    theme::lh::NAME_Y,
                                    theme::lh::NAME_W - if layer.lock_content { 14.0 } else { 0.0 },
                                    theme::lh::BUS_H,
                                );
                                if layer.lock_content {
                                    let lock = egui::Rect::from_min_size(
                                        name_rect.right_top(),
                                        egui::vec2(14.0, theme::lh::BUS_H),
                                    );
                                    ui.painter().text(
                                        lock.center(),
                                        egui::Align2::CENTER_CENTER,
                                        icons::i::LOCK,
                                        theme::icon_font(theme::FS_XS),
                                        theme::TEXT,
                                    );
                                    let r = ui.interact(
                                        lock,
                                        ui.id().with(("lock", lid)),
                                        egui::Sense::hover(),
                                    );
                                    icons::name(r, "Content locked", None);
                                }
                                let name = ui
                                    .scope_builder(
                                        egui::UiBuilder::new().max_rect(name_rect).layout(
                                            egui::Layout::left_to_right(egui::Align::Center),
                                        ),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(if in_group {
                                                        format!("\u{2502} {}", layer.name)
                                                    } else {
                                                        layer.name.clone()
                                                    })
                                                    .size(theme::FS_XS),
                                                )
                                                .truncate()
                                                .sense(egui::Sense::click_and_drag()),
                                            )
                                        },
                                    )
                                    .inner;
                                if name.drag_started() && !in_group {
                                    self.layer_drag = Some(lid);
                                }
                                if name.drag_stopped() && self.layer_drag == Some(lid) {
                                    layer_dropped = Some(lid);
                                }
                                let name = help(name.on_hover_text(&layer.name), "layer-name");
                                self.layer_rects.push((name.rect, lid));
                                if name.clicked() || name.secondary_clicked() {
                                    self.active_layer = Some(lid);
                                    self.selected = Some((lid, self.selected.map_or(0, |s| s.1)));
                                    self.selected_deck = Some(cur);
                                    self.preview_target = PreviewTarget::Layer(lid);
                                }
                                name.context_menu(|ui| {
                                    if crate::widgets::btn(
                                        ui,
                                        "Remove layer",
                                        crate::widgets::Kind::Secondary,
                                    )
                                    .clicked()
                                    {
                                        remove_layer_req = Some(lid);
                                        ui.close();
                                    }
                                    ui.separator();
                                    if let Some(color) = wave_ui::color_menu(ui, layer.color) {
                                        cmds.push(Command::SetColor {
                                            target: vjc_core::ColorTarget::Layer(lid),
                                            color,
                                        });
                                    }
                                    if crate::widgets::btn(
                                        ui,
                                        "Fold layer",
                                        crate::widgets::Kind::Secondary,
                                    )
                                    .clicked()
                                    {
                                        cmds.push(Command::SetLayerFolded {
                                            layer: lid,
                                            folded: true,
                                        });
                                        ui.close();
                                    }
                                    layer_ops_menu(ui, &self.project, lid, cur, &mut cmds);
                                    ui.separator();
                                    let from = active_of(&self.players, &self.gens, lid)
                                        .filter(|(d, _)| *d == cur)
                                        .map(|(_, c)| c);
                                    for (label, delta) in [("Previous clip", -1), ("Next clip", 1)]
                                    {
                                        if crate::widgets::btn(
                                            ui,
                                            label,
                                            crate::widgets::Kind::Secondary,
                                        )
                                        .clicked()
                                        {
                                            cmds.push(Command::StepClip {
                                                deck: cur,
                                                layer: lid,
                                                from,
                                                delta,
                                            });
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    group_menu(ui, comp, li, &mut cmds);
                                    ui.separator();
                                    add_effect_menu(ui, EffectTarget::Layer(lid), &mut cmds)
                                });
                                if help(
                                    grid_ui::tool(
                                        ui,
                                        at(0.0, 0.0, theme::lh::CLEAR_W, theme::LAYER_ROW_H),
                                        icons::glyph(icons::i::X),
                                        "Clear layer",
                                        None,
                                        None,
                                    ),
                                    "layer-clear",
                                )
                                .clicked()
                                {
                                    cmds.push(Command::ClearLayer { layer: lid });
                                }
                                let by = grid_ui::tool(
                                    ui,
                                    at(theme::lh::X0, 6.0, theme::lh::BTN_W, 48.0),
                                    big("B"),
                                    "Bypass layer",
                                    Some(layer.bypass),
                                    None,
                                );
                                if help(by, "layer-bypass").clicked() {
                                    cmds.push(Command::SetLayerBypass {
                                        layer: lid,
                                        bypass: !layer.bypass,
                                    });
                                }
                                let so = grid_ui::tool(
                                    ui,
                                    at(theme::lh::BTN_X2, 6.0, theme::lh::BTN_W, 48.0),
                                    big("S"),
                                    "Solo layer",
                                    Some(layer.solo),
                                    None,
                                );
                                if help(so, "layer-solo").clicked() {
                                    cmds.push(Command::SetLayerSolo {
                                        layer: lid,
                                        solo: !layer.solo,
                                    });
                                }
                                let label = vjc_core::blend::COMPOSITION_MODES
                                    .iter()
                                    .find(|(m, _)| *m == layer.track.blend)
                                    .map_or("Normal", |(_, n)| *n);
                                let blend = ui.put(
                                    at(theme::lh::X0, 58.0, theme::lh::BLEND_W, 18.0),
                                    |ui: &mut egui::Ui| {
                                        egui::ComboBox::from_id_salt(("blend", lid))
                                            .selected_text(
                                                egui::RichText::new(label).size(theme::FS_XS),
                                            )
                                            .width(82.0)
                                            .show_ui(ui, |ui| {
                                                for (b, name) in vjc_core::blend::COMPOSITION_MODES
                                                {
                                                    if crate::widgets::choice(
                                                        ui,
                                                        layer.track.blend == *b,
                                                        *name,
                                                    )
                                                    .clicked()
                                                    {
                                                        cmds.push(Command::SetBlend {
                                                            layer: lid,
                                                            blend: b.clone(),
                                                        });
                                                    }
                                                }
                                            })
                                            .response
                                    },
                                );
                                help(
                                    icons::name(blend, &format!("Blend mode: {label}"), None),
                                    "layer-blend",
                                );
                                // Crossfader bus: A / B, a second click on the lit one frees it.
                                let bus_row = ui.scope(|ui| {
                                    for (txt, bus, x) in [
                                        ("A", vjc_model::Bus::A, theme::lh::BUS_A_X),
                                        ("B", vjc_model::Bus::B, theme::lh::BUS_B_X),
                                    ] {
                                        let on = layer.crossfader_bus == Some(bus.clone());
                                        let r = grid_ui::tool(
                                            ui,
                                            at(
                                                x,
                                                theme::lh::NAME_Y,
                                                theme::lh::BUS_W,
                                                theme::lh::BUS_H,
                                            ),
                                            small(txt),
                                            &format!("Crossfader bus {txt}"),
                                            Some(on),
                                            None,
                                        );
                                        if r.clicked() {
                                            cmds.push(Command::SetLayerBus {
                                                layer: lid,
                                                bus: if on { None } else { Some(bus) },
                                            });
                                        }
                                    }
                                });
                                help(bus_row.response, "layer-bus");
                                // M (effects) / A (audio) / V (video) strip.
                                let fx = ui.put(
                                    at(
                                        theme::lh::FLAG_X,
                                        11.0,
                                        theme::lh::FLAG_W,
                                        theme::lh::FLAG_W,
                                    ),
                                    |ui: &mut egui::Ui| {
                                        ui.menu_button(small("M"), |ui| {
                                            add_effect_list(ui, EffectTarget::Layer(lid), &mut cmds)
                                        })
                                        .response
                                    },
                                );
                                icons::name(fx, "Add effect", None);
                                for (txt, tip, on, video, y, fill) in [
                                    (
                                        "A",
                                        "Layer audio on/off",
                                        layer.audio,
                                        false,
                                        33.0,
                                        theme::LAYER_AUDIO_ON,
                                    ),
                                    (
                                        "V",
                                        "Layer video on/off",
                                        layer.video,
                                        true,
                                        55.0,
                                        theme::LAYER_VIDEO_ON,
                                    ),
                                ] {
                                    let ink = if on {
                                        egui::RichText::new(txt)
                                            .font(theme::bold_font(&lctx, theme::FS_XS))
                                            .color(theme::ON_ACCENT)
                                    } else {
                                        small(txt)
                                    };
                                    let b = grid_ui::tool(
                                        ui,
                                        at(
                                            theme::lh::FLAG_X,
                                            y,
                                            theme::lh::FLAG_W,
                                            theme::lh::FLAG_W,
                                        ),
                                        ink,
                                        tip,
                                        Some(on),
                                        Some(fill),
                                    );
                                    if b.clicked() {
                                        cmds.push(if video {
                                            Command::SetLayerVideo {
                                                layer: lid,
                                                enabled: !on,
                                            }
                                        } else {
                                            Command::SetLayerAudio {
                                                layer: lid,
                                                enabled: !on,
                                            }
                                        });
                                    }
                                }
                                let mut op = match layer.track.opacity.value {
                                    vjc_model::Value::Number(n) => n as f32,
                                    _ => 1.0,
                                };
                                let op_r = ui.put(
                                    at(
                                        theme::lh::OP_X,
                                        8.0,
                                        theme::lh::OP_W,
                                        theme::LAYER_ROW_H - 16.0,
                                    ),
                                    egui::Slider::new(&mut op, 0.0..=1.0)
                                        .vertical()
                                        .show_value(false)
                                        .handle_shape(egui::style::HandleShape::Rect {
                                            aspect_ratio: 0.5,
                                        })
                                        .trailing_fill(true),
                                );
                                let op_r = help(op_r, "layer-opacity");
                                if let Some(a) = vjc_core::shortcuts::layer_address(
                                    &self.project,
                                    lid,
                                    "opacity",
                                ) {
                                    self.targets.push((op_r.rect, a));
                                }
                                if op_r.changed() {
                                    cmds.push(Command::SetOpacity {
                                        layer: lid,
                                        value: op as f64,
                                    });
                                }
                            });
                            for ci in 0..ncols {
                                let clip = deck_idx.and_then(|d| comp.decks[d].clip_at(lid, ci));
                                // Highlight only when playing from the deck being shown.
                                let on = !dests_in(
                                    &self.players,
                                    &self.gens,
                                    &self.origin,
                                    Some(cur),
                                    lid,
                                    ci,
                                )
                                .is_empty();
                                let label = clip
                                    .map(|k| k.name.as_str())
                                    .unwrap_or("(drop / right-click)");
                                let tile = match clip.map(|k| (k.id, &k.source)) {
                                    None => grid_ui::Tile::Empty,
                                    Some((id, vjc_model::Source::File { path })) => {
                                        match self.thumbs.texture(id, path) {
                                            Some((t, a)) => grid_ui::Tile::Thumb(t, a),
                                            None => grid_ui::Tile::Pending,
                                        }
                                    }
                                    Some((_, vjc_model::Source::Generator { kind, .. })) => {
                                        grid_ui::Tile::Color(kind)
                                    }
                                    Some((_, vjc_model::Source::Sequence { .. })) => {
                                        grid_ui::Tile::Color("sequence")
                                    }
                                    Some((_, vjc_model::Source::Capture { .. })) => {
                                        grid_ui::Tile::Color("capture")
                                    }
                                    Some((_, vjc_model::Source::Network { name, .. })) => {
                                        grid_ui::Tile::Color(name)
                                    }
                                    Some((_, vjc_model::Source::LayerRouter { .. })) => {
                                        grid_ui::Tile::Color("router")
                                    }
                                };
                                let cell_id =
                                    egui::Id::new(("cell", lid, comp.columns[ci as usize].id));
                                let cell = grid_ui::cell(
                                    ui,
                                    cell_id,
                                    label,
                                    label,
                                    tile,
                                    grid_ui::CellState {
                                        playing: on,
                                        selected: self.selected == Some((lid, ci)),
                                        failed: self.players.get(&lid).is_some_and(|(d, c, p)| {
                                            *c == ci && *d == cur && p.error().is_some()
                                        }),
                                    },
                                );
                                let r = help(
                                    cell.all(),
                                    if clip.is_some() { "cell" } else { "cell-empty" },
                                );
                                if let Some(c) = theme::palette(clip.and_then(|k| k.extras.color)) {
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(
                                            r.rect.min,
                                            egui::vec2(4.0, r.rect.height()),
                                        ),
                                        0.0,
                                        c,
                                    );
                                }
                                if let Some(k) = clip {
                                    let lay = &comp.layers[li];
                                    r.context_menu(|ui| {
                                        if let Some(a) = wave_ui::clip_menu(
                                            ui,
                                            (cur, lid, ci),
                                            k,
                                            (lay.ignore_column_trigger, lay.fader_start),
                                            &mut cmds,
                                        ) {
                                            clip_acts.push((lid, ci, a));
                                        }
                                        ui.separator();
                                        for (txt, act) in [
                                            ("Trigger", CellAct::Trigger),
                                            ("Preview", CellAct::Preview),
                                            ("Cut", CellAct::Cut),
                                            ("Copy", CellAct::Copy),
                                        ] {
                                            if crate::widgets::btn(
                                                ui,
                                                txt,
                                                crate::widgets::Kind::Secondary,
                                            )
                                            .clicked()
                                            {
                                                acts.push((lid, ci, act));
                                                ui.close();
                                            }
                                        }
                                        ui.add_enabled_ui(has_clipboard, |ui| {
                                            if crate::widgets::btn(
                                                ui,
                                                "Paste",
                                                crate::widgets::Kind::Secondary,
                                            )
                                            .clicked()
                                            {
                                                acts.push((lid, ci, CellAct::Paste));
                                                ui.close();
                                            }
                                        });
                                        if crate::widgets::btn(
                                            ui,
                                            "Clear",
                                            crate::widgets::Kind::Secondary,
                                        )
                                        .clicked()
                                        {
                                            acts.push((lid, ci, CellAct::Clear));
                                            ui.close();
                                        }
                                        if matches!(k.source, vjc_model::Source::File { .. })
                                            && crate::widgets::btn(
                                                ui,
                                                "Convert to HAP...",
                                                crate::widgets::Kind::Secondary,
                                            )
                                            .clicked()
                                        {
                                            acts.push((lid, ci, CellAct::ConvertHap));
                                            ui.close();
                                        }
                                        ui.menu_button("Rename...", |ui| {
                                            let key = ui.id().with(("rename_clip", lid, ci));
                                            let mut buf: String = ui
                                                .data_mut(|d| d.get_temp(key))
                                                .unwrap_or_else(|| label.to_owned());
                                            let e = crate::widgets::field_text_hint(
                                                ui, &mut buf, 140.0, "",
                                            );
                                            e.widget_info(|| {
                                                egui::WidgetInfo::labeled(
                                                    egui::WidgetType::TextEdit,
                                                    true,
                                                    "Clip name",
                                                )
                                            });
                                            if !e.has_focus() && !e.lost_focus() {
                                                e.request_focus();
                                            }
                                            let enter =
                                                ui.input(|i| i.key_pressed(egui::Key::Enter));
                                            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                                ui.data_mut(|d| d.remove_temp::<String>(key));
                                                ui.close();
                                            } else if e.lost_focus()
                                                && enter
                                                && !buf.trim().is_empty()
                                            {
                                                cmds.push(Command::RenameClip {
                                                    deck: cur,
                                                    layer: lid,
                                                    column: ci,
                                                    name: buf.trim().to_string(),
                                                });
                                                ui.data_mut(|d| d.remove_temp::<String>(key));
                                                ui.close();
                                            } else {
                                                ui.data_mut(|d| d.insert_temp(key, buf));
                                            }
                                        });
                                        ui.separator();
                                        let t = EffectTarget::Clip {
                                            deck: cur,
                                            layer: lid,
                                            column: ci,
                                        };
                                        add_effect_menu(ui, t, &mut cmds);
                                    });
                                }
                                if clip.is_none() {
                                    r.context_menu(|ui| {
                                        ui.add_enabled_ui(has_clipboard, |ui| {
                                            if crate::widgets::btn(
                                                ui,
                                                "Paste",
                                                crate::widgets::Kind::Secondary,
                                            )
                                            .clicked()
                                            {
                                                acts.push((lid, ci, CellAct::Paste));
                                                ui.close();
                                            }
                                        });
                                        if crate::widgets::btn(
                                            ui,
                                            "Import file...",
                                            crate::widgets::Kind::Secondary,
                                        )
                                        .clicked()
                                        {
                                            pick = Some((lid, ci));
                                            ui.close();
                                        }
                                        generator_menu(ui, cur, lid, ci, &mut cmds);
                                    });
                                }
                                if let Some(a) =
                                    vjc_core::shortcuts::clip_address(&self.project, lid, ci)
                                {
                                    self.targets.push((r.rect, a));
                                }
                                cells.push(CellInfo {
                                    rect: r.rect,
                                    layer: lid,
                                    col: ci,
                                    empty: clip.is_none(),
                                });
                                if drag_target == Some((lid, ci)) {
                                    ui.painter().rect_stroke(
                                        r.rect,
                                        3.0,
                                        egui::Stroke::new(2.5, theme::CELL_DROP),
                                        egui::StrokeKind::Outside,
                                    );
                                }
                                // The name strip only selects; the picture selects and triggers.
                                let strip_clicked =
                                    cell.strip.as_ref().is_some_and(|s| s.clicked());
                                if cell.pic.clicked() || strip_clicked || r.secondary_clicked() {
                                    self.selected = Some((lid, ci));
                                    self.selected_deck = Some(cur);
                                }
                                // A Hold that already fired owns the press: moving the cell under it would leave
                                // its clip in the air after the release.
                                let holding = self.holds.iter().any(|r| r.src == hold::Src::Mouse);
                                if cell.pic.drag_started() && clip.is_some() && !holding {
                                    self.cell_drag = Some((lid, ci));
                                }
                                if cell.pic.drag_stopped() && self.cell_drag == Some((lid, ci)) {
                                    self.cell_drag = None;
                                    let (at, alt) =
                                        ui.input(|i| (i.pointer.latest_pos(), i.modifiers.alt));
                                    let to = at
                                        .and_then(|p| {
                                            self.cells.iter().find(|c| c.rect.contains(p))
                                        })
                                        .map(|c| (c.layer, c.col))
                                        .filter(|t| *t != (lid, ci));
                                    if let Some(to) = to {
                                        cmds.push(Command::MoveClip {
                                            deck: cur,
                                            from: (lid, ci),
                                            to,
                                            copy: alt,
                                        });
                                    }
                                }
                                if strip_clicked && clip.is_some() {
                                    self.preview_target = PreviewTarget::Clip(cur, lid, ci);
                                } else if cell.pic.clicked() && clip.is_some() {
                                    self.preview_target = PreviewTarget::Layer(lid);
                                }
                                if clip.is_some()
                                    && vjc_core::trigger_style_of(&self.project, cur, lid, ci)
                                        == vjc_model::TriggerStyle::Hold
                                {
                                    // Hold: a press only arms (it may be the start of a drag);
                                    // `hold_tick` plays it once the press has stayed put.
                                    if cell.pic.hovered()
                                        && self.hold_arm.is_none()
                                        && self.cell_drag.is_none()
                                        && ui.input(|i| i.pointer.primary_pressed())
                                    {
                                        self.hold_arm = Some(hold::Arm {
                                            deck: cur,
                                            layer: lid,
                                            column: ci,
                                            at: std::time::Instant::now(),
                                            pos: ui.input(|i| i.pointer.latest_pos()),
                                        });
                                    }
                                } else if cell.pic.clicked() && clip.is_some() {
                                    cmds.push(Command::TriggerClip {
                                        deck: cur,
                                        layer: lid,
                                        column: ci,
                                    });
                                }
                                if clip.is_none() && cell.pic.double_clicked() {
                                    pick = Some((lid, ci));
                                }
                            }

                            for _ in 0..ghost_cols {
                                let (r, resp) = ui.allocate_exact_size(
                                    egui::vec2(theme::CELL_W, theme::THUMB_H + theme::NAME_H),
                                    egui::Sense::click(),
                                );
                                ui.painter().rect_filled(
                                    r,
                                    theme::CELL_ROUNDING,
                                    theme::CELL_EMPTY,
                                );
                                ghost_hit |= resp.clicked();
                                egui::Popup::menu(&resp).show(|ui| {
                                    if crate::widgets::btn(
                                        ui,
                                        "Add column",
                                        crate::widgets::Kind::Secondary,
                                    )
                                    .clicked()
                                    {
                                        cmds.push(Command::AddColumn {
                                            column: vjc_model::Column {
                                                color: None,
                                                autopilot: Default::default(),
                                                beat_snap: None,
                                                id: Uuid::new_v4(),
                                                name: (ncols + 1).to_string(),
                                            },
                                        });
                                        ui.close();
                                    }
                                });
                            }
                            ui.end_row();
                        }
                    });
                });
            if ghost_hit {
                self.preview_target = PreviewTarget::Composition;
            }
            let mut tabs_ui = parent_ui.new_child(egui::UiBuilder::new().max_rect(tabs_area));
            tabs_ui.set_clip_rect(tabs_area.intersect(parent_ui.clip_rect()));
            self.tabs_clip = tabs_ui.clip_rect();
            tabs_ui
                .painter()
                .rect_filled(tabs_area, 0.0, theme::DECK_BAR_BG);
            self.deck_tabs(&mut tabs_ui, &mut cmds, cf_pos);
            self.topbars_h = 0.0;
        } else {
            // Grid hidden by the layout: master controls and the crossfader / deck bar stay
            // reachable in a compact strip.
            self.grid_rect = egui::Rect::NOTHING;
            let strip_h = 2.0 * theme::COLUMN_HEAD_H;
            let (row, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), strip_h),
                egui::Sense::hover(),
            );
            let hrect =
                egui::Rect::from_min_size(row.min, egui::vec2(theme::LAYER_HEAD_W, strip_h));
            self.master_rect = Some(hrect);
            grid_ui::composition_header(
                ui,
                hrect,
                grid_ui::MasterState {
                    bypass: master_bypass,
                    recording,
                    dropped: rec_dropped,
                    rec_info: rec_info.clone(),
                    layers: comp.layers.len(),
                    columns: ncols as usize,
                    blend: self.prefs.general.default_blend.clone(),
                },
                &mut cmds,
                &mut self.targets,
            );
            let tabs_area = egui::Rect::from_min_max(
                egui::pos2(hrect.max.x, row.min.y),
                egui::pos2(row.max.x, row.min.y + theme::DECK_TABS_H),
            );
            let mut tabs_ui = ui.new_child(egui::UiBuilder::new().max_rect(tabs_area));
            tabs_ui.set_clip_rect(tabs_area.intersect(ui.clip_rect()));
            self.tabs_clip = tabs_ui.clip_rect();
            tabs_ui
                .painter()
                .rect_filled(tabs_area, 0.0, theme::DECK_BAR_BG);
            self.deck_tabs(&mut tabs_ui, &mut cmds, cf_pos);
            self.topbars_h = strip_h;
        }
        self.cells = cells;
        if let Some(l) = remove_layer_req {
            self.request_remove_layer(l);
        }
        if let Some(l) = layer_dropped {
            self.layer_drag = None;
            if let Some(p) = ctx.input(|i| i.pointer.latest_pos()) {
                cmds.push(Command::MoveLayer {
                    layer: l,
                    to_index: layer_drop_index(&self.layer_rects, l, p.y),
                });
            }
        }
        for (l, c, act) in acts {
            self.cell_act(cur, (l, c), act, &mut cmds);
        }
        for (l, c, act) in clip_acts {
            self.selected = Some((l, c));
            match act {
                wave_ui::ClipAct::Reveal(path) => self.reveal.reveal(&path),
                wave_ui::ClipAct::Snapshot => {
                    if self.snap_req.is_some() {
                        self.status = "Snapshot busy: wait for the current one".into();
                    } else {
                        self.snap_req = Some((cur, l, c));
                    }
                }
            }
        }
        if let Some((l, c)) = pick
            && let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "Video / image",
                    &[
                        "mov", "mp4", "m4v", "mkv", "avi", "webm", "png", "jpg", "jpeg", "webp",
                    ],
                )
                .pick_file()
        {
            self.drop_cell(l, c, path);
        }
        self.learn_pass(ui, &mut cmds);
        for c in cmds {
            self.exec(c);
        }
        // A click leaves keyboard focus on the widget; free it so shortcut keys keep working.
        if ctx.input(|i| i.pointer.any_released()) && !ctx.egui_wants_keyboard_input() {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
    }

    /// Caption of the Preview monitor.
    fn preview_caption(&self) -> String {
        let comp = &self.project.composition;
        match self.preview_target {
            PreviewTarget::Clip(d, l, c) => {
                let name =
                    vjc_core::clip_in(&self.project, d, l, c).map_or("?", |k| k.name.as_str());
                format!("Clip preview: {name}")
            }
            PreviewTarget::Layer(l) => {
                let off = comp
                    .layers
                    .iter()
                    .find(|x| x.id == l)
                    .is_some_and(|x| !x.video);
                preview_note(true, off).to_owned()
            }
            PreviewTarget::Composition => preview_note(false, false).to_owned(),
        }
    }

    /// Track whose transform the Preview widget edits: the previewed clip or layer, else the
    /// group shown in the Group tab.
    fn xf_target(&self) -> Option<EffectTarget> {
        if !self.xf_show {
            return None;
        }
        let t = match self.preview_target {
            PreviewTarget::Clip(deck, layer, column) => EffectTarget::Clip {
                deck,
                layer,
                column,
            },
            PreviewTarget::Layer(l) => EffectTarget::Layer(l),
            PreviewTarget::Composition => EffectTarget::Group(
                self.sel_group
                    .filter(|_| self.panel_tab == PanelTab::Group)?,
            ),
        };
        vjc_core::effects::track(&self.project, t).map(|_| t)
    }

    fn xf_values(&self) -> Option<(EffectTarget, xform_widget::Xf)> {
        let t = self.xf_target()?;
        let tr = vjc_core::effects::track(&self.project, t)?;
        Some((t, xform_widget::Xf::of(&tr.transform)))
    }

    /// Pointer handling of the Preview transform widget; true while it owns the drag.
    fn xf_interact(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        rect: egui::Rect,
        comp: (u32, u32),
    ) -> bool {
        let Some((target, xf)) = self.xf_values() else {
            self.xf_drag = None;
            return false;
        };
        let pic = monitor::picture_rect(rect, comp, self.mon_views[1]);
        let k = pic.width() / comp.0.max(1) as f32;
        let to_frame = |p: egui::Pos2| (p - pic.center()) / k;
        let alt = ui.input(|i| i.modifiers.alt);
        if resp.drag_started_by(egui::PointerButton::Primary) && !alt {
            let origin = ui.input(|i| i.pointer.press_origin());
            if let Some(o) = origin {
                let p0 = to_frame(o);
                if let Some(handle) = xform_widget::hit(&xf, comp, p0, 8.0 / k) {
                    // A nudge just before must stay its own undo step.
                    self.history.seal();
                    self.xf_drag = Some(XfDrag {
                        handle,
                        start: xf,
                        p0,
                        target,
                        shift: ui.input(|i| i.modifiers.shift),
                    });
                }
            }
        }
        if resp.clicked() || resp.drag_started() {
            resp.request_focus();
        }
        let mut busy = false;
        let was_dragging = self.xf_drag.is_some();
        if let Some(d) = self.xf_drag.take()
            && resp.dragged()
            && d.target == target
            && let Some(ptr) = ui.input(|i| i.pointer.latest_pos())
        {
            let values = xform_widget::drag(d.handle, &d.start, comp, d.p0, to_frame(ptr), d.shift);
            self.exec(Command::SetTransformMulti { target, values });
            self.xf_drag = Some(d);
            busy = true;
        }
        if was_dragging && !busy {
            // The drag ended: a nudge right after must not merge into it.
            self.history.seal();
        }
        if resp.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    resp.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let (mut dx, mut dy, mut shift) = (0, 0, false);
            ui.input_mut(|i| {
                for (key, ax, ay) in [
                    (egui::Key::ArrowLeft, -1, 0),
                    (egui::Key::ArrowRight, 1, 0),
                    (egui::Key::ArrowUp, 0, -1),
                    (egui::Key::ArrowDown, 0, 1),
                ] {
                    for sh in [true, false] {
                        let m = if sh {
                            egui::Modifiers::SHIFT
                        } else {
                            egui::Modifiers::NONE
                        };
                        while i.consume_key(m, key) {
                            dx += ax;
                            dy += ay;
                            shift = sh;
                        }
                    }
                }
            });
            if dx != 0 || dy != 0 {
                let values = xform_widget::nudge(&xf, dx, dy, shift);
                self.exec(Command::SetTransformMulti { target, values });
            }
        }
        busy
    }

    /// Bounding box and handles of the transform widget over the Preview picture.
    fn xf_paint(&self, painter: &egui::Painter, rect: egui::Rect, comp: (u32, u32)) {
        let Some((_, xf)) = self.xf_values() else {
            return;
        };
        let pic = monitor::picture_rect(rect, comp, self.mon_views[1]);
        let k = pic.width() / comp.0.max(1) as f32;
        let s = |v: egui::Vec2| pic.center() + v * k;
        let stroke = egui::Stroke::new(1.5, theme::ACCENT);
        let c = xform_widget::corners(&xf, comp).map(s);
        painter.add(egui::Shape::closed_line(c.to_vec(), stroke));
        for p in c {
            painter.rect_filled(
                egui::Rect::from_center_size(p, egui::vec2(8.0, 8.0)),
                1.0,
                theme::ACCENT,
            );
        }
        let top =
            s((xform_widget::corners(&xf, comp)[0] + xform_widget::corners(&xf, comp)[1]) / 2.0);
        let rot = s(xform_widget::rotate_handle(&xf, comp, f64::from(24.0 / k)));
        painter.line_segment([top, rot], stroke);
        painter.circle_filled(rot, 4.5, theme::ACCENT);
        let a = s(xform_widget::anchor_point(&xf));
        painter.circle_stroke(a, 5.0, egui::Stroke::new(1.5, theme::WARN));
        painter.line_segment(
            [a - egui::vec2(8.0, 0.0), a + egui::vec2(8.0, 0.0)],
            egui::Stroke::new(1.0, theme::WARN),
        );
        painter.line_segment(
            [a - egui::vec2(0.0, 8.0), a + egui::vec2(0.0, 8.0)],
            egui::Stroke::new(1.0, theme::WARN),
        );
    }

    /// Left column of the bottom area: Composition monitor over Preview monitor. Each has a
    /// Fit / Fill / 1:1 dropdown, drag to pan (double click resets) and the composition size.
    /// The Preview shows the selected layer alone (the composition when none is selected).
    fn monitor_column(&mut self, ui: &mut egui::Ui) {
        self.monitor_seen = true;
        let (cw, ch) = (
            self.project.composition.width,
            self.project.composition.height,
        );
        ui.spacing_mut().item_spacing.y = 0.0;
        let head_h = theme::PANEL_HEAD_H;
        let each = ((ui.available_height() - 4.0 * head_h) / 2.0).max(0.0);
        let w = ui.available_width();
        let preview_tex = if self.prev_live {
            match self.preview_target {
                PreviewTarget::Composition => None,
                _ => self.prev_tex,
            }
        } else {
            None
        };
        for (i, name) in ["Composition monitor", "Preview monitor"]
            .into_iter()
            .enumerate()
        {
            let (head, _) = ui.allocate_exact_size(egui::vec2(w, head_h), egui::Sense::hover());
            ui.painter().rect_filled(head, 0.0, theme::HEADER_BG);
            ui.put(
                head,
                egui::Label::new(
                    egui::RichText::new(name).font(theme::bold_font(ui.ctx(), theme::FS_SM)),
                ),
            );
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(w, each), egui::Sense::click_and_drag());
            if i == 1 {
                self.xf_area = rect;
            }
            let xf_busy = i == 1 && self.xf_interact(ui, &resp, rect, (cw, ch));
            if resp.dragged() && !xf_busy {
                self.mon_views[i].pan += resp.drag_delta();
            }
            if resp.double_clicked() {
                self.mon_views[i].pan = egui::Vec2::ZERO;
            }
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, theme::MONITOR_BG);
            let tex = if i == 1 {
                match self.preview_target {
                    PreviewTarget::Clip(..) => preview_tex,
                    _ => preview_tex.or(self.comp_tex),
                }
            } else {
                self.comp_tex
            };
            match tex {
                Some(id) => {
                    painter.image(
                        id,
                        monitor::picture_rect(rect, (cw, ch), self.mon_views[i]),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        crate::theme::IMAGE_TINT,
                    );
                }
                None => {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "No signal",
                        egui::FontId::proportional(theme::FS_SM),
                        ui.visuals().weak_text_color(),
                    );
                }
            }
            if i == 1 {
                self.xf_paint(&painter, rect, (cw, ch));
                let note = self.preview_caption();
                painter.text(
                    rect.left_top() + egui::vec2(4.0, 2.0),
                    egui::Align2::LEFT_TOP,
                    note,
                    egui::FontId::proportional(11.0),
                    theme::SECTION_HEAD,
                );
            }
            let (foot, _) = ui.allocate_exact_size(egui::vec2(w, head_h), egui::Sense::hover());
            ui.painter().rect_filled(foot, 0.0, theme::HEADER_BG);
            let what = if i == 0 {
                "Composition".to_owned()
            } else {
                "Preview".to_owned()
            };
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(foot.shrink2(egui::vec2(theme::SP_3, 0.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{what} \u{00b7} {}",
                            monitor::size_label(cw, ch)
                        ))
                        .size(theme::FS_XS)
                        .color(theme::TEXT_MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt(("monfit", i))
                            .width(54.0)
                            .selected_text(self.mon_views[i].fit.label())
                            .show_ui(ui, |ui| {
                                for f in monitor::MonitorFit::ALL {
                                    ui.selectable_value(&mut self.mon_views[i].fit, f, f.label());
                                }
                            });
                    });
                },
            );
        }
    }

    /// Bottom panel: Composition / Layer / Group parameters of the selection.
    fn params_panel(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            let w = ui.available_width() / 3.0;
            for (t, label, key) in [
                (PanelTab::Composition, "Composition", "tab-composition"),
                (PanelTab::Layer, "Layer", "tab-layer"),
                (PanelTab::Group, "Group", "tab-group"),
            ] {
                let on = self.panel_tab == t;
                let r = ui.add_sized(
                    egui::vec2(w, theme::PANEL_HEAD_H),
                    egui::Button::new(tab_text(ui, label, on))
                        .corner_radius(0.0)
                        .fill(if on { theme::BG } else { theme::HEADER_BG }),
                );
                r.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label)
                });
                if help(r, key).clicked() {
                    self.panel_tab = t;
                }
            }
        });
        ui.add_space(theme::SP_2);
        egui::ScrollArea::vertical().show(ui, |ui| match self.panel_tab {
            PanelTab::Composition => self.composition_tab(ui, cmds),
            PanelTab::Layer => self.layer_tab(ui, cmds),
            PanelTab::Group => self.group_tab(ui, cmds),
        });
    }

    /// Panel of the selected clip, next to the tabbed panel.
    fn clip_panel(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let (head, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), theme::PANEL_HEAD_H),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(head, 0.0, theme::HEADER_BG);
        let r = ui.put(head, egui::Label::new(tab_text(ui, "Clip", true)));
        help(r, "tab-clip");
        ui.add_space(theme::SP_2);
        egui::ScrollArea::vertical().show(ui, |ui| self.clip_tab(ui, cmds));
    }

    fn composition_tab(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let dials = self.dials();
        let actx = self.anim_ctx(&dials);
        let c = &self.project.composition;
        if params::section(ui, "sec-video", "Video") {
            if let Some(name) = params::text_field(ui, "cname", "Name", &c.name) {
                cmds.push(Command::SetCompositionName { name });
            }
            if let Some(text) = params::text_field_free(ui, "cdesc", "Description", &c.description)
            {
                cmds.push(Command::SetCompDescription { text });
            }
            let (mut w, mut h, mut fps) = (c.width, c.height, c.fps);
            let mut size_changed = false;
            let size_row = ui.horizontal(|ui| {
                params::row_label(ui, "Size");
                // speed 0: typed only, so a drag does not resize the output every frame.
                size_changed |= widgets::size_field(ui, &mut w, self.tex_cap, "").changed();
                ui.label("x");
                size_changed |= widgets::size_field(ui, &mut h, self.tex_cap, "").changed();
                ui.label("fps");
                size_changed |= ui
                    .add(
                        egui::DragValue::new(&mut fps)
                            .speed(0.0)
                            .range(1.0..=240.0)
                            .max_decimals(3),
                    )
                    .changed();
                let depth = c.bit_depth;
                widgets::combo(ui, "comp-bit-depth", format!("{depth}-bit"), None, |ui| {
                    for d in vjc_model::BIT_DEPTHS {
                        if widgets::choice(ui, depth == d, format!("{d}-bit")).clicked() {
                            cmds.push(Command::SetCompBitDepth { depth: d });
                        }
                    }
                });
            });
            help(size_row.response, "comp-size");
            if size_changed {
                cmds.push(Command::SetCompositionSize {
                    width: w,
                    height: h,
                    fps,
                });
            }
            let op = &c.master.opacity;
            if let Value::Number(d) = &op.default {
                let shown = vjc_core::anim::effective_number(op, &actx);
                let (v, a) = params::param_slider_anim(
                    ui,
                    "mop",
                    "Opacity",
                    shown,
                    *d,
                    (0.0, 1.0),
                    &op.animation,
                );
                if let Some(value) = v {
                    cmds.push(Command::SetMasterOpacity { value });
                }
                if let Some(animation) = a {
                    cmds.push(Command::SetAnimation {
                        target: vjc_core::AnimTarget::MasterOpacity,
                        animation,
                    });
                }
            }
        }
        if self.prefs.layouts.current.speed_sliders
            && params::section(ui, "sec-transport", "Transport")
            && let Some(value) =
                params::param_slider(ui, "msp", "Speed", c.master_speed, 1.0, (0.0, 4.0))
        {
            cmds.push(Command::SetMasterSpeed { value });
        }
        if params::section(ui, "sec-comp-trigger", "Clip trigger") {
            let styles: Vec<(vjc_model::TriggerStyle, String)> = vjc_model::TriggerStyle::CHOICES
                .into_iter()
                .map(|m| (m, m.label().to_string()))
                .collect();
            if let Some(style) = params::combo(
                ui,
                "ctstyle",
                "Trigger style",
                &c.trigger_style.resolve(vjc_model::TriggerStyle::Normal),
                &styles,
            ) {
                cmds.push(Command::SetCompTriggerStyle { style });
            }
            let targets: Vec<(vjc_model::ClipTarget, String)> = vjc_model::ClipTarget::CHOICES
                .into_iter()
                .map(|m| (m, m.label().to_string()))
                .collect();
            if let Some(target) = params::combo(
                ui,
                "cttarget",
                "Plays on",
                &c.clip_target.resolve(vjc_model::ClipTarget::Own),
                &targets,
            ) {
                cmds.push(Command::SetCompClipTarget { target });
            }
            if let Some(snap) =
                autopilot_ui::snap_combo(ui, "cbsnap", "Beat snap", c.beat_snap, "App default")
            {
                cmds.push(Command::SetCompBeatSnap { snap });
            }
        }
        if params::section(ui, "sec-crossfader", "Crossfader") {
            let (cf_val, cf_anim, cf_curve) = match &c.crossfader {
                Some(x) => (
                    vjc_core::anim::effective_number(&x.position, &actx),
                    x.position.animation.clone(),
                    vjc_core::crossfader::CrossfadeCurve::from_name(&x.curve),
                ),
                None => (
                    vjc_core::crossfader::CENTER,
                    None,
                    vjc_core::crossfader::CrossfadeCurve::default(),
                ),
            };
            let (v, a) = params::param_slider_anim(
                ui,
                "cfp",
                "Crossfader A-B",
                cf_val,
                vjc_core::crossfader::CENTER,
                (0.0, 1.0),
                &cf_anim,
            );
            if let Some(value) = v {
                cmds.push(Command::SetCrossfader { value });
            }
            if let Some(animation) = a {
                cmds.push(Command::SetAnimation {
                    target: vjc_core::AnimTarget::Crossfader,
                    animation,
                });
            }
            ui.horizontal(|ui| {
                params::row_label(ui, "Crossfader curve");
                crate::widgets::combo(ui, "cfcurve", cf_curve.label(), None, |ui| {
                    for curve in vjc_core::crossfader::CrossfadeCurve::ALL {
                        if crate::widgets::choice(ui, cf_curve == curve, curve.label()).clicked() {
                            cmds.push(Command::SetCrossfaderCurve { curve });
                        }
                    }
                });
            });
            let behaviours = vjc_model::CrossfadeBehaviour::ALL.map(|b| (b, b.label().to_string()));
            let cur = c
                .crossfader
                .as_ref()
                .map(|x| x.behaviour)
                .unwrap_or_default();
            if let Some(behaviour) =
                params::combo(ui, "cfbeh", "Crossfader behaviour", &cur, &behaviours)
            {
                cmds.push(Command::SetCrossfaderBehaviour { behaviour });
            }
            let mut modes: Vec<(Option<BlendMode>, String)> =
                vec![(None, "Layer's own".to_string())];
            modes.extend(
                vjc_core::blend::COMPOSITION_MODES
                    .iter()
                    .map(|(m, n)| (Some(m.clone()), n.to_string())),
            );
            let cur = c.crossfader.as_ref().and_then(|x| x.blend.clone());
            if let Some(blend) = params::combo(ui, "cfblend", "Crossfader blend", &cur, &modes) {
                cmds.push(Command::SetCrossfaderBlend { blend });
            }
        }
        if params::section(ui, "sec-comp-autopilot", "Autopilot") {
            autopilot_ui::comp_section(ui, &c.autopilot, cmds);
        }
        if params::section(ui, "sec-comp-audio", "Master audio") {
            self.audio_rows(ui, AudioTarget::Master, &c.master, cmds);
            self.audio_chain_ui(ui, EffectTarget::MasterAudio, cmds);
        }
        if params::section(ui, "sec-transform", "Transform") {
            self.transform_rows(ui, EffectTarget::Master, cmds);
        }
        if params::section(ui, "sec-cues", "Cues") {
            self.cue_list(ui, cmds);
        }
        if params::section(ui, "sec-dashboard", "Dashboard") {
            let vals: Vec<f64> = dials.to_vec();
            self.dial_rows(ui, DialOwner::Comp, &vals, cmds);
        }
        if params::section(ui, "sec-effects", "Effects") {
            self.chain_ui(ui, EffectTarget::Master, cmds);
        }
    }

    /// Timecode cues: (time, column, layer) rows; edits send the whole list as one command.
    fn cue_list(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let comp = &self.project.composition;
        let mut cues = comp.timecode_cues.clone();
        let mut changed = false;
        ui.strong("Timecode cues");
        let mut remove = None;
        for (i, cue) in cues.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                changed |= timecode_io::tc_editor(ui, ("cue", i), &mut cue.time);
                ui.label("col");
                let mut col = cue.column + 1;
                let max = comp.columns.len().max(1) as u32;
                if crate::widgets::field_int(ui, &mut col, 1..=max, "", "").changed() {
                    cue.column = col - 1;
                    changed = true;
                }
                let name = |l: Option<LayerId>| {
                    l.and_then(|id| comp.layers.iter().find(|x| x.id == id))
                        .map_or("All layers".to_owned(), |x| x.name.clone())
                };
                crate::widgets::combo(ui, ("cuelayer", i), name(cue.layer), None, |ui| {
                    changed |= ui
                        .selectable_value(&mut cue.layer, None, "All layers")
                        .changed();
                    for l in &comp.layers {
                        changed |= ui
                            .selectable_value(&mut cue.layer, Some(l.id), &l.name)
                            .changed();
                    }
                });
                if icons::button(ui, icons::i::X, "Remove cue").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            cues.remove(i);
            changed = true;
        }
        ui.horizontal(|ui| {
            if crate::widgets::btn(ui, "Add cue", crate::widgets::Kind::Secondary).clicked() {
                let time = self.tc.last.unwrap_or(vjc_model::Timecode::new(
                    0,
                    0,
                    0,
                    0,
                    vjc_model::TcFps::F25,
                ));
                cues.push(vjc_model::TimecodeCue {
                    time,
                    column: 0,
                    layer: None,
                });
                changed = true;
            }
            if cues.is_empty() {
                ui.weak("Fires a column when the timecode crosses its time.");
            }
        });
        if changed {
            cmds.push(Command::SetTimecodeCues { cues });
        }
    }

    fn layer_tab(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let Some(lid) = self.selected.map(|s| s.0) else {
            ui.label("Select a layer (click its name or a cell).");
            return;
        };
        let dials = self.dials_for(Some(EffectTarget::Layer(lid)));
        let actx = self.anim_ctx(&dials);
        let Some(l) = self.project.composition.layers.iter().find(|l| l.id == lid) else {
            return;
        };
        let layer_dials = vjc_core::anim::dial_values(&l.track.dials);
        if params::section(ui, "sec-layer", "Layer") {
            if let Some(name) = params::text_field(ui, ("lname", lid), "Name", &l.name) {
                cmds.push(Command::RenameLayer { layer: lid, name });
            }
            let op = &l.track.opacity;
            if let Value::Number(d) = &op.default {
                let shown = vjc_core::anim::effective_number(op, &actx);
                let (v, a) = params::param_slider_anim(
                    ui,
                    ("lop", lid),
                    "Opacity",
                    shown,
                    *d,
                    (0.0, 1.0),
                    &op.animation,
                );
                if let Some(value) = v {
                    cmds.push(Command::SetOpacity { layer: lid, value });
                }
                if let Some(animation) = a {
                    cmds.push(Command::SetAnimation {
                        target: vjc_core::AnimTarget::LayerOpacity(lid),
                        animation,
                    });
                }
            }
            // Master (M) scales picture and sound; Opacity (V) only fades the picture.
            let ex = l.extras.clone();
            if let Some(master) =
                params::param_slider(ui, ("lmaster", lid), "Master", ex.master, 1.0, (0.0, 1.0))
            {
                cmds.push(Command::SetLayerExtras {
                    layer: lid,
                    extras: vjc_model::LayerExtras {
                        master,
                        ..ex.clone()
                    },
                });
            }
            let sizes: Vec<(vjc_model::AutoSize, String)> = vjc_model::AutoSize::ALL
                .iter()
                .map(|a| {
                    let label = match a {
                        vjc_model::AutoSize::Off => "Off (clip's fit)",
                        vjc_model::AutoSize::Fill => "Fill",
                        vjc_model::AutoSize::Fit => "Fit",
                        vjc_model::AutoSize::Stretch => "Stretch",
                    };
                    (*a, label.to_string())
                })
                .collect();
            if let Some(auto_size) =
                params::combo(ui, ("las", lid), "Auto-size", &ex.auto_size, &sizes)
            {
                cmds.push(Command::SetLayerExtras {
                    layer: lid,
                    extras: vjc_model::LayerExtras {
                        auto_size,
                        ..ex.clone()
                    },
                });
            }
            ui.horizontal(|ui| {
                params::row_label(ui, "Size");
                let max = vjc_model::MAX_LAYER_SIDE as f32;
                let (mut w, mut h) = (ex.width as f32, ex.height as f32);
                let cw = widgets::field_num(ui, &mut w, 0.0..=max, 1.0).changed();
                ui.label("x");
                let ch = widgets::field_num(ui, &mut h, 0.0..=max, 1.0).changed();
                if cw || ch {
                    cmds.push(Command::SetLayerExtras {
                        layer: lid,
                        extras: vjc_model::LayerExtras {
                            width: w.round() as u32,
                            height: h.round() as u32,
                            ..ex.clone()
                        },
                    });
                }
            })
            .response
            .on_hover_text("Picture area in composition px; 0 = the composition's size");
            let modes: Vec<(BlendMode, String)> = vjc_core::blend::COMPOSITION_MODES
                .iter()
                .map(|(m, n)| (m.clone(), n.to_string()))
                .collect();
            if let Some(blend) = params::combo(ui, ("lbl", lid), "Blend", &l.track.blend, &modes) {
                cmds.push(Command::SetBlend { layer: lid, blend });
            }
            let trig: Vec<(TriggerMode, String)> = TriggerMode::LAYER_CHOICES
                .into_iter()
                .map(|m| (m, m.label().to_string()))
                .collect();
            if let Some(mode) = params::combo(ui, ("ltm", lid), "Start at", &l.trigger_mode, &trig)
            {
                cmds.push(Command::SetLayerTriggerMode { layer: lid, mode });
            }
            let mut ignore = l.ignore_column_trigger;
            if ui
                .checkbox(&mut ignore, "Ignore column trigger")
                .on_hover_text("Triggering a column leaves this layer as it is")
                .changed()
            {
                cmds.push(Command::SetLayerIgnoreColumn { layer: lid, ignore });
            }
            let (mut solo, mut bypass) = (l.solo, l.bypass);
            ui.horizontal(|ui| {
                if icons::toggle(ui, &mut solo, icons::i::HEADPHONES, "Solo").changed() {
                    cmds.push(Command::SetLayerSolo { layer: lid, solo });
                }
                if icons::toggle(ui, &mut bypass, icons::i::POWER, "Bypass").changed() {
                    cmds.push(Command::SetLayerBypass { layer: lid, bypass });
                }
                let (mut video, mut audio) = (l.video, l.audio);
                if icons::toggle(ui, &mut video, icons::i::EYE, "Video").changed() {
                    cmds.push(Command::SetLayerVideo {
                        layer: lid,
                        enabled: video,
                    });
                }
                if icons::toggle(ui, &mut audio, icons::i::SPEAKER_HIGH, "Audio").changed() {
                    cmds.push(Command::SetLayerAudio {
                        layer: lid,
                        enabled: audio,
                    });
                }
            });
        }
        if params::section(ui, "sec-transition", "Transition") {
            let kinds = transition_kinds();
            let tr = l.transition.clone();
            if let Some(kind) = params::combo(ui, ("ltk", lid), "Transition", &tr.kind, &kinds) {
                let transition = Transition { kind, ..tr.clone() };
                cmds.push(Command::SetLayerTransition {
                    layer: lid,
                    transition,
                });
            }
            if let Some(secs) = params::param_slider(
                ui,
                ("ltd", lid),
                "Duration (s)",
                tr.duration_secs,
                0.0,
                (0.0, 8.0),
            ) {
                let transition = Transition {
                    duration_secs: secs,
                    ..tr.clone()
                };
                cmds.push(Command::SetLayerTransition {
                    layer: lid,
                    transition,
                });
            }
            let beats: Vec<(Option<f64>, String)> =
                [None, Some(1.0), Some(2.0), Some(4.0), Some(8.0)]
                    .into_iter()
                    .map(|b| {
                        (
                            b,
                            b.map_or("off (use seconds)".into(), |n| format!("{n} beats")),
                        )
                    })
                    .collect();
            if let Some(duration_beats) = params::combo(
                ui,
                ("ltb", lid),
                "Duration (beats)",
                &tr.duration_beats,
                &beats,
            ) {
                let transition = Transition {
                    duration_beats,
                    ..tr.clone()
                };
                cmds.push(Command::SetLayerTransition {
                    layer: lid,
                    transition,
                });
            }
        }
        if params::section(ui, "sec-autopilot", "Autopilot") {
            let ap = l.autopilot.clone();
            let modes = labeled(&[
                (AutopilotMode::Off, "Off"),
                (AutopilotMode::Next, "Next"),
                (AutopilotMode::Random, "Random"),
                (AutopilotMode::Previous, "Previous"),
                (AutopilotMode::FirstLast, "First / last"),
            ]);
            let units = labeled(&[
                (AutopilotUnit::Loops, "Loops"),
                (AutopilotUnit::Beats, "Beats"),
                (AutopilotUnit::Seconds, "Seconds"),
            ]);
            if let Some(unit) =
                params::combo(ui, ("lapu", lid), "Autopilot after", &ap.unit, &units)
            {
                cmds.push(Command::SetLayerAutopilot {
                    layer: lid,
                    autopilot: Autopilot { unit, ..ap.clone() },
                });
            }
            if let Some(mode) = params::combo(ui, ("lap", lid), "Autopilot", &ap.mode, &modes) {
                cmds.push(Command::SetLayerAutopilot {
                    layer: lid,
                    autopilot: Autopilot { mode, ..ap.clone() },
                });
            }
            autopilot_ui::layer_extras(ui, lid, &ap, cmds);
            if ap.unit == AutopilotUnit::Loops {
                if let Some(n) = params::param_slider(
                    ui,
                    ("lapn", lid),
                    "Autopilot every N loops",
                    f64::from(ap.every_loops),
                    1.0,
                    (1.0, 16.0),
                ) {
                    cmds.push(Command::SetLayerAutopilot {
                        layer: lid,
                        autopilot: Autopilot {
                            every_loops: n.round() as u32,
                            ..ap
                        },
                    });
                }
            } else {
                let (label, range) = if ap.unit == AutopilotUnit::Beats {
                    ("Autopilot every N beats", (1.0, 64.0))
                } else {
                    ("Autopilot every N seconds", (1.0, 120.0))
                };
                if let Some(n) =
                    params::param_slider(ui, ("lapt", lid), label, ap.every, 8.0, range)
                {
                    cmds.push(Command::SetLayerAutopilot {
                        layer: lid,
                        autopilot: Autopilot {
                            every: n.round().max(1.0),
                            ..ap
                        },
                    });
                }
            }
        }
        if params::section(ui, "sec-layer-audio", "Audio") {
            self.audio_rows(ui, AudioTarget::Layer(lid), &l.track, cmds);
            self.audio_chain_ui(ui, EffectTarget::LayerAudio(lid), cmds);
        }
        if params::section(ui, "sec-layer-transform", "Transform") {
            self.transform_rows(ui, EffectTarget::Layer(lid), cmds);
        }
        if params::section(ui, "sec-layer-dashboard", "Dashboard") {
            self.dial_rows(ui, DialOwner::Layer(lid), &layer_dials, cmds);
        }
        if params::section(ui, "sec-layer-effects", "Effects") {
            self.chain_ui(ui, EffectTarget::Layer(lid), cmds);
        }
    }

    fn group_tab(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let Some(g) = self
            .sel_group
            .and_then(|id| self.project.composition.group(id))
            .cloned()
        else {
            ui.label("Select a group (click its header in the grid).");
            return;
        };
        let gid = g.id;
        if params::section(ui, "sec-group", "Group") {
            if let Some(name) = params::text_field(ui, ("gname", gid), "Name", &g.name) {
                cmds.push(Command::RenameGroup { group: gid, name });
            }
            if let Some(value) = params::param_slider(
                ui,
                ("gop", gid),
                "Opacity",
                match g.track.opacity.value {
                    Value::Number(n) => n,
                    _ => 1.0,
                },
                1.0,
                (0.0, 1.0),
            ) {
                cmds.push(Command::SetGroupOpacity { group: gid, value });
            }
            let modes: Vec<(BlendMode, String)> = vjc_core::blend::COMPOSITION_MODES
                .iter()
                .map(|(m, n)| (m.clone(), n.to_string()))
                .collect();
            if let Some(blend) = params::combo(ui, ("gbl", gid), "Blend", &g.track.blend, &modes) {
                cmds.push(Command::SetGroupBlend { group: gid, blend });
            }
            if self.prefs.layouts.current.speed_sliders
                && let Some(value) =
                    params::param_slider(ui, ("gsp", gid), "Speed", g.speed, 1.0, (0.0, 4.0))
            {
                cmds.push(Command::SetGroupSpeed { group: gid, value });
            }
            ui.horizontal(|ui| {
                ui.label("Mask");
                for mode in vjc_model::MaskMode::ALL {
                    if crate::widgets::choice(ui, g.mask_mode == mode, mode.label())
                        .on_hover_text("The composed group masks the ungrouped layers below it")
                        .clicked()
                    {
                        cmds.push(Command::SetGroupMaskMode { group: gid, mode });
                    }
                }
            });
            let (mut ignore, mut lock) = (g.ignore_column_trigger, g.lock_content);
            if crate::widgets::toggle(ui, &mut ignore, "Ignore column trigger").changed() {
                cmds.push(Command::SetGroupIgnoreColumn { group: gid, ignore });
            }
            if crate::widgets::toggle(ui, &mut lock, "Lock content").changed() {
                cmds.push(Command::SetGroupLock {
                    group: gid,
                    locked: lock,
                });
            }
            let (mut solo, mut bypass) = (g.solo, g.bypass);
            ui.horizontal(|ui| {
                for (txt, bus) in [("A", vjc_model::Bus::A), ("B", vjc_model::Bus::B)] {
                    let on = g.crossfader_bus == Some(bus.clone());
                    if ui
                        .selectable_label(on, txt)
                        .on_hover_text(format!("Group on crossfader bus {txt}"))
                        .clicked()
                    {
                        cmds.push(Command::SetGroupBus {
                            group: gid,
                            bus: if on { None } else { Some(bus) },
                        });
                    }
                }
                if icons::toggle(ui, &mut solo, icons::i::HEADPHONES, "Solo").changed() {
                    cmds.push(Command::SetGroupSolo { group: gid, solo });
                }
                if icons::toggle(ui, &mut bypass, icons::i::POWER, "Bypass").changed() {
                    cmds.push(Command::SetGroupBypass { group: gid, bypass });
                }
                if crate::widgets::btn(ui, "Ungroup", crate::widgets::Kind::Secondary).clicked() {
                    cmds.push(Command::Ungroup { group: gid });
                }
                if ui
                    .button("Eject")
                    .on_hover_text("Stop every layer of the group")
                    .clicked()
                {
                    cmds.extend(vjc_core::groups::eject_commands(&self.project, gid));
                }
                if let Some((_, col)) = self.selected
                    && ui
                        .button("Trigger column")
                        .on_hover_text("Trigger the selected cell's column on this group only")
                        .clicked()
                {
                    let deck = self.project.composition.current_deck;
                    cmds.extend(vjc_core::groups::trigger_column_commands(
                        &self.project,
                        gid,
                        deck,
                        col,
                    ));
                }
            });
        }
        if params::section(ui, "sec-group-effects", "Effects") {
            self.chain_ui(ui, EffectTarget::Group(gid), cmds);
        }
    }

    /// Deck the Clip tab shows: the current one, or, when that deck has no clip in the
    /// selected cell, the deck of the clip playing in this very cell.
    fn clip_tab_deck(&self, sel: Option<(LayerId, u32)>) -> DeckId {
        let cur = self.project.composition.current_deck;
        let Some((layer, column)) = sel else {
            return cur;
        };
        if vjc_core::clip_in(&self.project, cur, layer, column).is_some() {
            return cur;
        }
        self.dest_of(None, layer, column)
            .and_then(|l| self.active(l))
            .map_or(cur, |(d, _)| d)
    }

    fn clip_tab(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let sel = self.selected;
        let deck = self.clip_tab_deck(sel);
        let Some((layer, column, k)) =
            sel.and_then(|(l, c)| Some((l, c, vjc_core::clip_in(&self.project, deck, l, c)?)))
        else {
            let avail = ui.available_height();
            ui.add_space((avail * 0.5 - 70.0).max(0.0));
            ui.vertical_centered(|ui| {
                ui.label(
                    icons::glyph(icons::i::FILM_SLATE)
                        .size(theme::FS_BASE * 2.0)
                        .color(theme::TEXT_MUTED),
                );
                ui.label("No clip selected.");
                ui.weak("Click a filled cell to edit its transport, opacity and effects.");
                ui.add_space(theme::SP_2);
                ui.weak("Drop a video file on a cell, or right-click it to import a file or add a generator.");
            });
            return;
        };
        let k = k.clone();
        let dials = self.dials_for(Some(EffectTarget::Clip {
            deck,
            layer,
            column,
        }));
        let actx = self.anim_ctx(&dials);
        let t = &k.transport;
        let (clip_style, clip_target) = (k.trigger_style, k.target);
        let clip_snap = k.beat_snap;
        if let Some(name) = params::text_field(ui, ("kname", layer, column), "Name", &k.name) {
            cmds.push(Command::RenameClip {
                deck,
                layer,
                column,
                name,
            });
        }
        self.src_info.set_ctx(ui.ctx());
        // Length of the file, once its probe has landed.
        let file_secs = match &k.source {
            vjc_model::Source::File { path } => self
                .src_info
                .get(path)
                .flatten()
                .map(|i| i.duration)
                .filter(|d| *d > 0.0),
            _ => None,
        };
        match &k.source {
            vjc_model::Source::File { path } => {
                let status = self
                    .dest_of(Some(deck), layer, column)
                    .and_then(|dest| self.players.get(&dest))
                    .and_then(|(_, _, p)| p.decode_status());
                let info = self.src_info.get(path);
                if info.is_none() {
                    // The probe runs on a thread; look again shortly.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
                let mut line = source_info::describe(path, info);
                if let Some(st) = status {
                    line = format!("{line} · {st}");
                }
                ui.label(line).on_hover_text(format!(
                    "{}\nDecoder: {}",
                    path.display(),
                    match status {
                        Some("hw") => "hardware",
                        Some(_) => "software",
                        None => "not started",
                    }
                ));
            }
            vjc_model::Source::Sequence { pattern, fps } => {
                ui.label(format!("Image sequence: {}", pattern.display()));
                let mut f = *fps;
                let r = crate::widgets::field_int(ui, &mut f, 1.0..=240.0, "", " fps");
                if r.changed() {
                    let mut clip = k.clone();
                    clip.source = vjc_model::Source::Sequence {
                        pattern: pattern.clone(),
                        fps: f,
                    };
                    cmds.push(Command::SetClip {
                        deck,
                        layer,
                        column,
                        clip,
                    });
                }
            }
            vjc_model::Source::Generator { kind, params } => {
                ui.strong(format!("Source: generator {}", fx_label(kind)));
                let inputs = self.fx.isf(kind).map(|f| f.inputs.clone());
                if inputs.is_none() {
                    ui.colored_label(crate::theme::ERROR_TEXT, "unknown generator (not rendered)");
                }
                for (pi, p) in params.iter().enumerate() {
                    let Some(ik) = inputs.as_ref().and_then(|v| v.get(pi)).map(|i| &i.kind) else {
                        continue;
                    };
                    let mut an = None;
                    let r = ui
                        .push_id(pi, |ui| param_widget(ui, p, ik, &actx, &mut an))
                        .inner;
                    if let Some(animation) = an {
                        cmds.push(Command::SetAnimation {
                            target: vjc_core::AnimTarget::Generator {
                                deck,
                                layer,
                                column,
                                param: pi,
                            },
                            animation,
                        });
                    }
                    if let Some(value) = r {
                        cmds.push(Command::SetGeneratorParam {
                            deck,
                            layer,
                            column,
                            param: pi,
                            value,
                        });
                    }
                }
            }
            vjc_model::Source::Network { protocol, name } => {
                ui.label(format!("Source: {protocol:?} {name}"));
                if let Some((_, _, p)) = self
                    .dest_of(Some(deck), layer, column)
                    .and_then(|dest| self.players.get(&dest))
                {
                    match (p.error(), p.latest_frame()) {
                        (Some(e), _) => {
                            ui.colored_label(crate::theme::ERROR_TEXT, e);
                        }
                        (None, None) => {
                            ui.weak("Waiting for the source...");
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                ui.label("Source: not supported yet");
            }
        }
        ui.separator();
        let transport_open = params::section(ui, "sec-clip-transport", "Transport");
        if transport_open {
            let modes = labeled(&[
                (PlayMode::Loop, "Loop"),
                (PlayMode::Bounce, "Bounce"),
                (PlayMode::Once, "Once"),
                (PlayMode::OnceHold, "Once, hold last frame"),
                (PlayMode::Random, "Random position"),
            ]);
            if let Some(mode) = params::combo(ui, "pm", "Play mode", &t.play_mode, &modes) {
                cmds.push(Command::SetPlayMode {
                    deck,
                    layer,
                    column,
                    mode,
                });
            }
            // Duration in seconds: speed follows so the in..out region lasts this long
            // (beat sync wins when set).
            ui.horizontal(|ui| {
                params::row_label(ui, "Duration (s)");
                let mut secs = t.duration_secs.unwrap_or(0.0) as f32;
                if widgets::field_num(ui, &mut secs, 0.0..=86_400.0, 0.1).changed() {
                    cmds.push(Command::SetClipDuration {
                        deck,
                        layer,
                        column,
                        secs: (secs > 0.0).then_some(f64::from(secs).max(0.001)),
                    });
                }
            })
            .response
            .on_hover_text("0 = free speed. Ignored while beat sync is on.");
            {
                use vjc_model::AutopilotMode as M;
                let modes: Vec<(Option<M>, String)> = vec![
                    (None, "Layer's".into()),
                    (Some(M::Off), "Off".into()),
                    (Some(M::Next), "Next".into()),
                    (Some(M::Random), "Random".into()),
                    (Some(M::Previous), "Previous".into()),
                    (Some(M::FirstLast), "First / last".into()),
                ];
                let own = k.extras.autopilot.clone();
                let layer_ap = self
                    .project
                    .composition
                    .layers
                    .iter()
                    .find(|l| l.id == layer)
                    .map(|l| l.autopilot.clone());
                let base = own.clone().or(layer_ap);
                if let Some(mode) = params::combo(
                    ui,
                    "capm",
                    "Autopilot",
                    &own.as_ref().map(|a| a.mode),
                    &modes,
                ) {
                    let mut extras = k.extras.clone();
                    extras.autopilot = mode
                        .and_then(|mode| base.clone().map(|b| vjc_model::Autopilot { mode, ..b }));
                    cmds.push(Command::SetClipExtras {
                        deck,
                        layer,
                        column,
                        extras,
                    });
                }
                // The count is in loops only for the Loops unit; editing it must not
                // switch a Beats or Seconds autopilot to Loops.
                if let Some(a) = own
                    .as_ref()
                    .filter(|a| a.mode != M::Off && a.unit == vjc_model::AutopilotUnit::Loops)
                {
                    ui.horizontal(|ui| {
                        params::row_label(ui, "Every loops");
                        let mut n = a.every_loops as f32;
                        if widgets::field_num(ui, &mut n, 1.0..=1024.0, 1.0).changed() {
                            let mut extras = k.extras.clone();
                            extras.autopilot = Some(every_loops_edit(a, n));
                            cmds.push(Command::SetClipExtras {
                                deck,
                                layer,
                                column,
                                extras,
                            });
                        }
                    });
                }
            }
            let trig: Vec<(TriggerMode, String)> = [TriggerMode::Layer]
                .into_iter()
                .chain(TriggerMode::LAYER_CHOICES)
                .map(|m| (m, m.label().to_string()))
                .collect();
            if let Some(mode) = params::combo(ui, "trig", "Start at", &t.trigger_mode, &trig) {
                cmds.push(Command::SetTriggerMode {
                    deck,
                    layer,
                    column,
                    mode,
                });
            }
            let styles: Vec<(vjc_model::TriggerStyle, String)> =
                [vjc_model::TriggerStyle::Composition]
                    .into_iter()
                    .chain(vjc_model::TriggerStyle::CHOICES)
                    .map(|m| (m, m.label().to_string()))
                    .collect();
            if let Some(style) = params::combo(ui, "tstyle", "Trigger style", &clip_style, &styles)
            {
                cmds.push(Command::SetClipTriggerStyle {
                    deck,
                    layer,
                    column,
                    style,
                });
            }
            let targets: Vec<(vjc_model::ClipTarget, String)> =
                [vjc_model::ClipTarget::Composition]
                    .into_iter()
                    .chain(vjc_model::ClipTarget::CHOICES)
                    .map(|m| (m, m.label().to_string()))
                    .collect();
            if let Some(target) = params::combo(ui, "ttarget", "Plays on", &clip_target, &targets) {
                cmds.push(Command::SetClipTarget {
                    deck,
                    layer,
                    column,
                    target,
                });
            }
            if let Some(snap) =
                autopilot_ui::snap_combo(ui, "kbsnap", "Beat snap", clip_snap, "Composition")
            {
                cmds.push(Command::SetClipBeatSnap {
                    deck,
                    layer,
                    column,
                    snap,
                });
            }
            let dirs = labeled(&[
                (Direction::Forward, "Forward"),
                (Direction::Backward, "Backward"),
                (Direction::Pause, "Pause"),
                (Direction::Random, "Random"),
            ]);
            if let Some(direction) = params::combo(ui, "dir", "Direction", &t.direction, &dirs) {
                cmds.push(Command::SetDirection {
                    deck,
                    layer,
                    column,
                    direction,
                });
            }
            if self.prefs.layouts.current.speed_sliders
                && let Some(speed) =
                    params::param_slider(ui, "spd", "Speed", t.speed, 1.0, (0.0, 4.0))
            {
                cmds.push(Command::SetSpeed {
                    deck,
                    layer,
                    column,
                    speed,
                });
            }
            let tmode = TransportMode::of(t);
            let tmodes: Vec<(TransportMode, String)> = TransportMode::ALL
                .iter()
                .map(|m| (*m, m.label().to_string()))
                .collect();
            if let Some(m) = params::combo(ui, "tmode", "Mode", &tmode, &tmodes) {
                let seed = self.tc.last.unwrap_or(vjc_model::Timecode::new(
                    0,
                    0,
                    0,
                    0,
                    vjc_model::TcFps::F25,
                ));
                let (beat_sync, timecode_start) = m.sync_for(t, seed);
                cmds.push(Command::SetClipSync {
                    deck,
                    layer,
                    column,
                    beat_sync,
                    timecode_start,
                });
            }
            if tmode == TransportMode::BpmSync {
                let syncs: Vec<(Option<f64>, String)> =
                    [None, Some(1.0), Some(2.0), Some(4.0), Some(8.0), Some(16.0)]
                        .into_iter()
                        .map(|b| (b, b.map_or("off".into(), |n| format!("{n} beats"))))
                        .collect();
                if let Some(beats) = params::combo(ui, "sync", "Beat sync", &t.beat_sync, &syncs) {
                    cmds.push(Command::SetClipSync {
                        deck,
                        layer,
                        column,
                        beat_sync: beats,
                        timecode_start: None,
                    });
                }
            }
            if let Some(mut start) = t.timecode_start {
                ui.horizontal(|ui| {
                    ui.label("Start");
                    if timecode_io::tc_editor(ui, ("tcstart", layer, column), &mut start) {
                        cmds.push(Command::SetTimecodeStart {
                            deck,
                            layer,
                            column,
                            start: Some(start),
                        });
                    }
                    if crate::widgets::btn_if(
                        ui,
                        self.tc.last.is_some(),
                        "Set from current",
                        crate::widgets::Kind::Secondary,
                    )
                    .clicked()
                        && let Some(cur) = self.tc.last
                    {
                        cmds.push(Command::SetTimecodeStart {
                            deck,
                            layer,
                            column,
                            start: Some(cur),
                        });
                    }
                });
                if self.prefs.timecode.source == vjc_io::prefs::TcSource::Off {
                    ui.weak("Choose a timecode source in Preferences > Timecode.");
                }
            }
            ui.separator();
            let dest = self.dest_of(Some(deck), layer, column);
            let playing = dest.is_some();
            let pos = dest
                .and_then(|d| self.flow.pos.get(&d))
                .copied()
                .unwrap_or(0.0);
            let reg = vjc_core::playback::region(t);
            self.scrub_bar(ui, (deck, layer, column), &k, (playing, pos), cmds);
            if let Some(fs) = file_secs {
                let at = |p: f64| format!("{:.2} s", fs * p);
                ui.weak(format!("In {}   Out {}", at(reg.0), at(reg.1)));
                self.duration_row(ui, (deck, layer, column), t, fs, tmode, cmds);
            } else {
                ui.weak(format!(
                    "In {:.1}%   Out {:.1}%",
                    reg.0 * 100.0,
                    reg.1 * 100.0
                ));
            }
            if !self.prefs.layouts.current.is_hidden("cue-points") {
                ui.horizontal(|ui| {
                    ui.label("Cues");
                    if crate::widgets::btn_if(ui, playing, "Set", crate::widgets::Kind::Secondary)
                        .clicked()
                    {
                        cmds.push(Command::AddCue {
                            deck,
                            layer,
                            column,
                            pos,
                        });
                    }
                    for (i, c) in t.cue_points.iter().enumerate() {
                        let jump = crate::widgets::btn_if(
                            ui,
                            playing,
                            &format!("Jump {}", i + 1),
                            crate::widgets::Kind::Secondary,
                        );
                        if let Some(a) = vjc_core::shortcuts::cue_address(&self.project, layer, i) {
                            self.targets.push((jump.rect, a));
                        }
                        if jump.clicked() {
                            cmds.push(Command::JumpToCue {
                                deck,
                                layer,
                                column,
                                index: i,
                            });
                        }
                        if icons::button(ui, icons::i::X, "Remove cue").clicked() {
                            cmds.push(Command::RemoveCue {
                                deck,
                                layer,
                                column,
                                pos: *c,
                            });
                        }
                    }
                });
            }
            let active_loop = self.flow.beat_loops.get(&layer).map(|l| l.0);
            let mut loop_pick = None;
            ui.horizontal(|ui| {
                ui.label("Beat loop");
                for b in [1.0, 2.0, 4.0, 8.0] {
                    let on = playing && active_loop == Some(b);
                    if ui
                        .add_enabled(playing, egui::Button::selectable(on, format!("{b}")))
                        .clicked()
                    {
                        loop_pick = Some(if on { None } else { Some(b) });
                    }
                }
            });
            if let Some(b) = loop_pick {
                self.set_beat_loop(layer, b);
            }
        }
        let clip_dials = vjc_core::anim::dial_values(&k.track.dials);
        if params::section(ui, "sec-clip-dashboard", "Dashboard") {
            self.dial_rows(ui, DialOwner::Clip(deck, layer, column), &clip_dials, cmds);
        }
        if params::section(ui, "sec-clip-video", "Video") {
            let fits: Vec<(FitMode, String)> = FitMode::ALL
                .iter()
                .map(|m| (*m, m.label().to_string()))
                .collect();
            if let Some(fit) = params::combo(ui, "fit", "Fit", &k.fit, &fits) {
                cmds.push(Command::SetFit {
                    deck,
                    layer,
                    column,
                    fit,
                });
            }
            let alphas = [
                (false, "Straight".to_string()),
                (true, "Premultiplied".to_string()),
            ];
            if let Some(premultiplied) = params::combo(
                ui,
                "alphat",
                "Alpha type",
                &k.channels.premultiplied,
                &alphas,
            ) {
                cmds.push(Command::SetClipChannels {
                    deck,
                    layer,
                    column,
                    channels: vjc_model::Channels {
                        premultiplied,
                        ..k.channels
                    },
                });
            }
            ui.horizontal(|ui| {
                let mut ex = k.extras.clone();
                let mut changed = ui
                    .checkbox(&mut ex.freeze, "Freeze")
                    .on_hover_text("Holds the current picture while the clip keeps running")
                    .changed();
                ui.label("Stop-motion fps");
                let mut fps = ex.stop_motion_fps as f32;
                if widgets::field_num(ui, &mut fps, 0.0..=60.0, 0.5)
                    .on_hover_text("0 = off")
                    .changed()
                {
                    ex.stop_motion_fps = f64::from(fps);
                    changed = true;
                }
                if changed {
                    cmds.push(Command::SetClipExtras {
                        deck,
                        layer,
                        column,
                        extras: ex,
                    });
                }
            });
            if ui
                .button("Refresh thumbnail")
                .on_hover_text("Decode this clip's thumbnail again")
                .clicked()
            {
                cmds.push(Command::RefreshClipThumb {
                    deck,
                    layer,
                    column,
                });
            }
            let mut blends: Vec<(Option<BlendMode>, String)> =
                vec![(None, "Layer determined".to_string())];
            blends.extend(
                vjc_core::blend::COMPOSITION_MODES
                    .iter()
                    .map(|(m, n)| (Some(m.clone()), n.to_string())),
            );
            if let Some(blend) = params::combo(ui, "bov", "Blend", &k.blend_override, &blends) {
                cmds.push(Command::SetBlendOverride {
                    deck,
                    layer,
                    column,
                    blend,
                });
            }
            if params::section(ui, "sec-clip-transition", "Transition") {
                let layer_kinds = transition_kinds();
                // None = "From layer": the layer's transition applies.
                let mut kinds: Vec<(Option<String>, String)> =
                    vec![(None, "From layer".to_string())];
                kinds.extend(layer_kinds.into_iter().map(|(a, b)| (Some(a), b)));
                let cur = k.transition.as_ref().map(|t| t.kind.clone());
                if let Some(choice) = params::combo(ui, "ctk", "Type", &cur, &kinds) {
                    let transition = choice.map(|kind| Transition {
                        kind,
                        ..k.transition.clone().unwrap_or(Transition {
                            kind: String::new(),
                            duration_beats: None,
                            duration_secs: 1.0,
                        })
                    });
                    cmds.push(Command::SetClipTransition {
                        deck,
                        layer,
                        column,
                        transition,
                    });
                }
                if let Some(tr) = k.transition.clone() {
                    if let Some(secs) = params::param_slider(
                        ui,
                        "ctd",
                        "Duration (s)",
                        tr.duration_secs,
                        1.0,
                        (0.0, 8.0),
                    ) {
                        cmds.push(Command::SetClipTransition {
                            deck,
                            layer,
                            column,
                            transition: Some(Transition {
                                duration_secs: secs,
                                ..tr.clone()
                            }),
                        });
                    }
                    let beats: Vec<(Option<f64>, String)> =
                        [None, Some(1.0), Some(2.0), Some(4.0), Some(8.0)]
                            .into_iter()
                            .map(|b| {
                                (
                                    b,
                                    b.map_or("off (use seconds)".into(), |n| format!("{n} beats")),
                                )
                            })
                            .collect();
                    if let Some(duration_beats) =
                        params::combo(ui, "ctb", "Duration (beats)", &tr.duration_beats, &beats)
                    {
                        cmds.push(Command::SetClipTransition {
                            deck,
                            layer,
                            column,
                            transition: Some(Transition {
                                duration_beats,
                                ..tr
                            }),
                        });
                    }
                }
            }
            ui.horizontal(|ui| {
                let mut ch = k.channels;
                let mut changed = false;
                for (txt, flag) in [
                    ("R", &mut ch.r),
                    ("G", &mut ch.g),
                    ("B", &mut ch.b),
                    ("A", &mut ch.a),
                ] {
                    changed |= ui.toggle_value(flag, txt).changed();
                }
                if changed {
                    cmds.push(Command::SetClipChannels {
                        deck,
                        layer,
                        column,
                        channels: ch,
                    });
                }
            });
        }
        if params::section(ui, "sec-clip-transform", "Transform") {
            self.transform_rows(
                ui,
                EffectTarget::Clip {
                    deck,
                    layer,
                    column,
                },
                cmds,
            );
        }
        if params::section(ui, "sec-clip-audio", "Audio") {
            self.audio_rows(ui, AudioTarget::Clip(deck, layer, column), &k.track, cmds);
            self.audio_chain_ui(
                ui,
                EffectTarget::ClipAudio {
                    deck,
                    layer,
                    column,
                },
                cmds,
            );
        }
        if params::section(ui, "sec-clip-effects", "Effects") {
            self.chain_ui(
                ui,
                EffectTarget::Clip {
                    deck,
                    layer,
                    column,
                },
                cmds,
            );
        }
    }

    /// Scrub bar of a clip: playhead, cue points, beat loop and draggable in / out markers.
    /// Dragging a marker edits the region (`SetInOut`, undoable); pressing anywhere else
    /// moves the playhead (`SeekClip`, runtime only).
    fn scrub_bar(
        &self,
        ui: &mut egui::Ui,
        (deck, layer, column): (DeckId, LayerId, u32),
        k: &vjc_model::Clip,
        (playing, pos): (bool, f64),
        cmds: &mut Vec<Command>,
    ) {
        use vjc_core::playback::{ScrubHit, drag_marker, region, scrub_hit};
        let t = &k.transport;
        let reg = region(t);
        let (rect, bar) = ui.allocate_exact_size(
            egui::vec2(
                (ui.available_width() - theme::SCRUB_MARGIN).clamp(80.0, 400.0),
                theme::SCRUB_H,
            ),
            egui::Sense::click_and_drag(),
        );
        bar.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Scrub bar"));
        let frac = |x: f32| f64::from((x - rect.left()) / rect.width().max(1.0));
        let tol = f64::from(theme::SCRUB_GRAB / rect.width().max(1.0));
        let grab = bar.id.with("grab");
        if bar.drag_started() {
            let p0 = ui
                .input(|i| i.pointer.press_origin())
                .map_or(0.0, |o| frac(o.x));
            ui.data_mut(|d| d.insert_temp(grab, scrub_hit(p0, reg, tol)));
        }
        if bar.dragged()
            && let Some(at) = bar.interact_pointer_pos()
        {
            let p = frac(at.x);
            match ui.data(|d| d.get_temp::<ScrubHit>(grab)) {
                Some(ScrubHit::Seek) | None => cmds.push(Command::SeekClip {
                    deck,
                    layer,
                    column,
                    pos: p.clamp(0.0, 1.0),
                }),
                Some(hit) => {
                    let (in_point, out_point) = drag_marker(hit, p, reg);
                    if (in_point, out_point) != reg {
                        cmds.push(Command::SetInOut {
                            deck,
                            layer,
                            column,
                            in_point,
                            out_point,
                        });
                    }
                }
            }
        }
        if bar.clicked()
            && let Some(at) = bar.interact_pointer_pos()
            && scrub_hit(frac(at.x), reg, tol) == ScrubHit::Seek
        {
            cmds.push(Command::SeekClip {
                deck,
                layer,
                column,
                pos: frac(at.x).clamp(0.0, 1.0),
            });
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        let x_at = |p: f64| rect.left() + rect.width() * p as f32;
        let span = |a: f64, b: f64| egui::Rect::from_x_y_ranges(x_at(a)..=x_at(b), rect.y_range());
        for (a, b) in [(0.0, reg.0), (reg.1, 1.0)] {
            painter.rect_filled(span(a, b), 0.0, theme::SCRUB_OUTSIDE);
        }
        for c in &t.cue_points {
            painter.line_segment(
                [
                    egui::pos2(x_at(*c), rect.top()),
                    egui::pos2(x_at(*c), rect.bottom()),
                ],
                egui::Stroke::new(2.0, ui.visuals().warn_fg_color),
            );
        }
        if let Some((_, (a, b))) = self.flow.beat_loops.get(&layer).filter(|_| playing) {
            painter.rect_filled(
                span(*a, *b),
                0.0,
                ui.visuals().selection.bg_fill.linear_multiply(0.5),
            );
        }
        if playing {
            painter.line_segment(
                [
                    egui::pos2(x_at(pos), rect.top()),
                    egui::pos2(x_at(pos), rect.bottom()),
                ],
                egui::Stroke::new(1.0, ui.visuals().text_color()),
            );
        }
        for (p, dir) in [(reg.0, 1.0f32), (reg.1, -1.0)] {
            let x = x_at(p);
            let stroke = egui::Stroke::new(2.0, theme::ACCENT);
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                stroke,
            );
            let w = theme::SCRUB_GRAB * 0.6;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(x, rect.top()),
                    egui::pos2(x + dir * w, rect.top()),
                    egui::pos2(x, rect.top() + w),
                ],
                theme::ACCENT,
                egui::Stroke::NONE,
            ));
        }
    }

    /// Duration of the in..out region with -/+ and halve / double. Every edit becomes a
    /// clip speed (`SetSpeed`) that makes the region last that long.
    fn duration_row(
        &self,
        ui: &mut egui::Ui,
        (deck, layer, column): (DeckId, LayerId, u32),
        t: &vjc_model::Transport,
        file_secs: f64,
        mode: TransportMode,
        cmds: &mut Vec<Command>,
    ) {
        let reg = vjc_core::playback::region(t);
        let secs = vjc_core::playback::region_secs(file_secs, reg, t.speed);
        let mut target = None;
        ui.horizontal(|ui| {
            ui.label("Duration");
            ui.add_enabled_ui(mode == TransportMode::Timeline, |ui| {
                let mut v = if secs.is_finite() { secs } else { 0.0 };
                let btn = |ui: &mut egui::Ui, text: &str, name: &str| {
                    let b = ui.small_button(text);
                    b.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name)
                    });
                    b.clicked()
                };
                if btn(ui, "-", "Duration shorter") {
                    target = Some(v - DURATION_STEP);
                }
                if ui
                    .add(
                        egui::DragValue::new(&mut v)
                            .speed(0.0)
                            .range(0.05..=3600.0)
                            .max_decimals(2)
                            .suffix(" s"),
                    )
                    .changed()
                {
                    target = Some(v);
                }
                if btn(ui, "+", "Duration longer") {
                    target = Some(v + DURATION_STEP);
                }
                if btn(ui, "\u{f7}2", "Halve duration") {
                    target = Some(v / 2.0);
                }
                if btn(ui, "\u{d7}2", "Double duration") {
                    target = Some(v * 2.0);
                }
            });
        });
        if let Some(speed) = target
            .and_then(|d| vjc_core::playback::speed_for_duration(file_secs, reg, d))
            .filter(|s| (*s - t.speed).abs() > 1e-9)
        {
            cmds.push(Command::SetSpeed {
                deck,
                layer,
                column,
                speed,
            });
        }
    }

    /// The 8 dials of a layer's or clip's own dashboard; each is a mappable target.
    fn dial_rows(
        &mut self,
        ui: &mut egui::Ui,
        owner: DialOwner,
        vals: &[f64],
        cmds: &mut Vec<Command>,
    ) {
        let dial_ref = match owner {
            DialOwner::Comp => vjc_core::DialRef::Comp,
            DialOwner::Layer(l) => vjc_core::DialRef::Layer(l),
            DialOwner::Clip(d, l, c) => vjc_core::DialRef::Clip(d, l, c),
        };
        let list: Vec<vjc_model::Param> = match owner {
            DialOwner::Comp => self.project.composition.dashboard.clone(),
            DialOwner::Layer(l) => self
                .project
                .composition
                .layers
                .iter()
                .find(|x| x.id == l)
                .map(|x| x.track.dials.clone())
                .unwrap_or_default(),
            DialOwner::Clip(deck, l, c) => vjc_core::clip_in(&self.project, deck, l, c)
                .map(|k| k.track.dials.clone())
                .unwrap_or_default(),
        };
        let set_dial = |i: usize, value: f64| match owner {
            DialOwner::Comp => Command::SetDashboardDial {
                dial: i as u32,
                value,
            },
            DialOwner::Layer(layer) => Command::SetLayerDial {
                layer,
                dial: i as u32,
                value,
            },
            DialOwner::Clip(deck, layer, column) => Command::SetClipDial {
                deck,
                layer,
                column,
                dial: i as u32,
                value,
            },
        };
        self.dash_presets_menu(ui, dial_ref, &list, vals, &set_dial, cmds);
        for (i, v) in vals.iter().enumerate() {
            let n = i + 1;
            let id = match owner {
                DialOwner::Comp => ("dial", i as u64),
                DialOwner::Layer(l) => ("ldial", l.as_u128() as u64 ^ i as u64),
                DialOwner::Clip(_, l, c) => {
                    ("cdial", l.as_u128() as u64 ^ u64::from(c) << 8 ^ i as u64)
                }
            };
            let (name, (lo, hi)) = list.get(i).map_or_else(
                || (format!("Dial {n}"), (0.0, 1.0)),
                |p| (p.name.clone(), p.range.unwrap_or((0.0, 1.0))),
            );
            let r = ui.horizontal(|ui| params::param_slider(ui, id, &name, *v, 0.0, (0.0, 1.0)));
            // Right click: rename the dial and set its range.
            r.response.context_menu(|ui| {
                let key = egui::Id::new(("dial-name", id));
                let mut buf: String = ui.data(|d| d.get_temp(key)).unwrap_or_else(|| name.clone());
                let e = crate::widgets::field_text_hint(ui, &mut buf, 120.0, "");
                let (mut nlo, mut nhi) = (lo, hi);
                ui.horizontal(|ui| {
                    crate::widgets::field_drag(ui, &mut nlo, f64::MIN..=f64::MAX, 0.1, "Min ", "");
                    crate::widgets::field_drag(ui, &mut nhi, f64::MIN..=f64::MAX, 0.1, "Max ", "");
                });
                let commit_name = e.lost_focus() && !buf.trim().is_empty();
                ui.data_mut(|d| d.insert_temp(key, buf.clone()));
                if nhi <= nlo {
                    nhi = nlo + 0.001;
                }
                if commit_name || (nlo, nhi) != (lo, hi) {
                    cmds.push(Command::SetDialInfo {
                        owner: dial_ref,
                        dial: i as u32,
                        name: if commit_name { buf } else { name.clone() },
                        range: (nlo, nhi),
                    });
                }
            });
            if let Some(value) = r.inner {
                cmds.push(set_dial(i, value));
            }
            let addr = match owner {
                DialOwner::Comp => Some(format!("/comp/dashboard/{n}")),
                DialOwner::Layer(l) => {
                    vjc_core::shortcuts::dial_address(&self.project, l, None, None, i)
                }
                DialOwner::Clip(deck, l, c) => {
                    let (t, active) = self.addressed(EffectTarget::Clip {
                        deck,
                        layer: l,
                        column: c,
                    });
                    let at = match t {
                        EffectTarget::Clip { layer, .. } => layer,
                        _ => l,
                    };
                    vjc_core::shortcuts::dial_address(&self.project, at, Some((deck, c)), active, i)
                }
            };
            if let Some(a) = addr {
                self.targets.push((r.response.rect, a));
            }
        }
    }

    /// Position, scale, rotation and anchor sliders of a track's transform; each row is a
    /// mappable target (`/comp/.../transform/...`) and animatable like the other params.
    fn transform_rows(&mut self, ui: &mut egui::Ui, target: EffectTarget, cmds: &mut Vec<Command>) {
        let Some(tf) = vjc_core::effects::track(&self.project, target).map(|t| t.transform.clone())
        else {
            return;
        };
        let dials = self.dials_for(Some(target));
        let actx = self.anim_ctx(&dials);
        let (cw, ch) = {
            let c = &self.project.composition;
            (f64::from(c.width), f64::from(c.height))
        };
        let (addr_target, active) = self.addressed(target);
        for field in TransformField::ALL {
            let prm = tf.field(field);
            let Value::Number(default) = prm.default else {
                continue;
            };
            let (range, step) = transform_ui_range(field, (cw, ch));
            let shown = vjc_core::anim::effective_number(prm, &actx);
            let r = ui.scope(|ui| {
                params::param_slider_anim_step(
                    ui,
                    ("tf", target, field.address()),
                    &prm.name,
                    shown,
                    default,
                    range,
                    step,
                    &prm.animation,
                )
            });
            let (value, anim) = r.inner;
            if let Some(value) = value {
                cmds.push(Command::SetTransform {
                    target,
                    field,
                    value,
                });
            }
            if let Some(animation) = anim {
                cmds.push(Command::SetAnimation {
                    target: vjc_core::AnimTarget::Transform { target, field },
                    animation,
                });
            }
            if let Some(a) =
                vjc_core::shortcuts::transform_address(&self.project, addr_target, field, active)
            {
                self.targets.push((r.response.rect, a));
            }
        }
    }

    /// Volume and pan sliders of a layer or clip track.
    fn audio_rows(
        &self,
        ui: &mut egui::Ui,
        target: AudioTarget,
        track: &vjc_model::Track,
        cmds: &mut Vec<Command>,
    ) {
        let num = |p: &vjc_model::Param, d: f64| match p.value {
            Value::Number(n) => n,
            _ => d,
        };
        for (param, label, v, default, range) in [
            (
                AudioParam::Volume,
                "Volume",
                num(&track.volume, 1.0),
                1.0,
                (0.0, 1.0),
            ),
            (
                AudioParam::Pan,
                "Pan",
                num(&track.pan, 0.0),
                0.0,
                (-1.0, 1.0),
            ),
        ] {
            if let Some(value) =
                params::param_slider(ui, ("audio", target, label), label, v, default, range)
            {
                cmds.push(Command::SetAudio {
                    target,
                    param,
                    value,
                });
            }
        }
    }

    /// Registers an effect control as a mappable (learn) target.
    fn fx_target(&mut self, rect: egui::Rect, target: EffectTarget, k: usize, tail: &str) {
        let (target, active) = self.addressed(target);
        if let Some(a) = vjc_core::shortcuts::effect_address(&self.project, target, k, tail, active)
        {
            self.targets.push((rect, a));
        }
    }

    /// Effect chain of a target with per-effect controls.
    fn chain_ui(&mut self, ui: &mut egui::Ui, target: EffectTarget, cmds: &mut Vec<Command>) {
        ui.menu_button("Add effect", |ui| add_effect_menu(ui, target, cmds));
        let chain = vjc_core::effects::chain(&self.project, target)
            .unwrap_or_default()
            .to_vec();
        let n = chain.len();
        for (i, e) in chain.iter().enumerate() {
            let inputs = self
                .fx
                .isf(&e.kind)
                .map(|f| f.inputs.clone())
                .or_else(|| self.fx.ffgl.inputs(&e.kind));
            let dials = self.dials_for(Some(target));
            let actx = self.anim_ctx(&dials);
            let failure = match self.fx.status(&e.kind) {
                Some(fx::Status::Failed(why)) => Some(why.clone()),
                _ => self.ml.problem(&e.kind, e.id),
            };
            let stopped = self.fx.ffgl.badge(e);
            let open_id = ui.id().with(("fx-open", e.id));
            let open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(true);
            let mut toggle_open = false;
            let (dw, dw_rect) = egui::Frame::new()
                .fill(theme::EFFECT_BG)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let label = self
                            .fx
                            .ffgl
                            .name_of(&e.kind)
                            .unwrap_or_else(|| fx_label(&e.kind));
                        let name = ui.strong(label);
                        if let Some(b) = &stopped {
                            ui.colored_label(theme::WARN, b.as_str())
                                .on_hover_text(self.fx.ffgl.stop_reason(e).unwrap_or_default());
                            if crate::widgets::btn(ui, "Restart", crate::widgets::Kind::Secondary)
                                .clicked()
                            {
                                self.fx.ffgl.restart(e);
                            }
                        }
                        name.context_menu(|ui| {
                            if crate::widgets::btn_if(
                                ui,
                                i > 0,
                                "Move up",
                                crate::widgets::Kind::Secondary,
                            )
                            .clicked()
                            {
                                cmds.push(Command::MoveEffect {
                                    target,
                                    effect: e.id,
                                    index: i - 1,
                                });
                                ui.close();
                            }
                            if crate::widgets::btn_if(
                                ui,
                                i + 1 < n,
                                "Move down",
                                crate::widgets::Kind::Secondary,
                            )
                            .clicked()
                            {
                                cmds.push(Command::MoveEffect {
                                    target,
                                    effect: e.id,
                                    index: i + 1,
                                });
                                ui.close();
                            }
                            if let Some(root) = self.fx_presets_dir.clone() {
                                ui.separator();
                                fx_preset_menu(ui, &root, target, e, &mut self.status, cmds);
                            }
                        });
                        let dw = ui
                            .push_id((e.id, "dry_wet"), |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().slider_width = theme::FX_SLIDER_W;
                                    let v = params::param_slider(
                                        ui,
                                        "dry_wet",
                                        "Dry/Wet",
                                        e.dry_wet as f64,
                                        1.0,
                                        (0.0, 1.0),
                                    );
                                    (v, ui.min_rect())
                                })
                                .inner
                            })
                            .inner;
                        let by = icons::letter(
                            ui,
                            "B",
                            "Bypass effect",
                            theme::FX_BTN.y,
                            Some(e.bypass),
                        );
                        self.fx_target(by.rect, target, i, "bypass");
                        if by.clicked() {
                            cmds.push(Command::SetEffectBypass {
                                target,
                                effect: e.id,
                                bypass: !e.bypass,
                            });
                        }
                        let pr = icons::letter(
                            ui,
                            "P",
                            "Effect parameters",
                            theme::FX_BTN.y,
                            Some(open),
                        );
                        toggle_open = pr.clicked();
                        let rm =
                            icons::letter(ui, "\u{00d7}", "Remove effect", theme::FX_BTN.y, None);
                        if rm.clicked() {
                            cmds.push(Command::RemoveEffect {
                                target,
                                effect: e.id,
                            });
                        }
                        if let Some(why) = &failure {
                            ui.colored_label(theme::ERROR_TEXT, format!("disabled: {why}"));
                        }
                        dw
                    })
                    .inner
                })
                .inner;
            if toggle_open {
                ui.data_mut(|d| d.insert_temp(open_id, !open));
            }
            self.fx_target(dw_rect, target, i, "drywet");
            if let Some(v) = dw {
                cmds.push(Command::SetEffectDryWet {
                    target,
                    effect: e.id,
                    dry_wet: v as f32,
                });
            }
            if !open {
                continue;
            }
            // How the effect output is mixed over its input (Dry/Wet is the opacity).
            ui.horizontal(|ui| {
                params::row_label(ui, "Blend");
                let label = vjc_core::blend::COMPOSITION_MODES
                    .iter()
                    .find(|(m, _)| *m == e.blend)
                    .map_or("Normal", |(_, n)| *n);
                crate::widgets::combo(ui, (e.id, "fxblend"), label, Some(110.0), |ui| {
                    for mode in &vjc_core::blend::FX_MIX_MODES {
                        let name = vjc_core::blend::COMPOSITION_MODES
                            .iter()
                            .find(|(m, _)| m == mode)
                            .map_or("Normal", |(_, n)| *n);
                        if crate::widgets::choice(ui, e.blend == *mode, name).clicked() {
                            cmds.push(Command::SetEffectBlend {
                                target,
                                effect: e.id,
                                blend: mode.clone(),
                            });
                        }
                    }
                });
            });
            if (e.kind == "lut3d" || e.kind == "image_mask")
                && let Some(pi) = e.params.iter().position(|p| p.name == "file")
            {
                let is_lut = e.kind == "lut3d";
                let cur = match &e.params[pi].value {
                    Value::Text(t) => t.clone(),
                    _ => String::new(),
                };
                ui.horizontal(|ui| {
                    let (label, filter, exts): (_, _, &[&str]) = if is_lut {
                        ("Load .cube...", "3D LUT", &["cube"])
                    } else {
                        ("Load image...", "Image", &["png", "jpg", "jpeg"])
                    };
                    if crate::widgets::btn(ui, label, crate::widgets::Kind::Secondary).clicked()
                        && let Some(f) = rfd::FileDialog::new().add_filter(filter, exts).pick_file()
                    {
                        cmds.push(Command::SetEffectParam {
                            target,
                            effect: e.id,
                            param: pi,
                            value: Value::Text(f.to_string_lossy().into_owned()),
                        });
                    }
                    let name = std::path::Path::new(&cur).file_name().map_or_else(
                        || {
                            if is_lut {
                                "No LUT loaded"
                            } else {
                                "No image loaded"
                            }
                            .to_string()
                        },
                        |n| n.to_string_lossy().into_owned(),
                    );
                    crate::widgets::help(ui, &name);
                });
            }
            let groups = inputs
                .as_ref()
                .map(|v| vjc_fx::param_groups(v))
                .unwrap_or_default();
            for (title, idxs) in groups {
                if let Some(t) = &title {
                    let open = ui
                        .push_id((e.id, t.as_str()), |ui| params::section(ui, "fx-group", t))
                        .inner;
                    if !open {
                        continue;
                    }
                }
                for pi in idxs {
                    let Some(p) = e.params.get(pi) else {
                        continue;
                    };
                    let Some(kind) = inputs.as_ref().and_then(|v| v.get(pi)).map(|i| &i.kind)
                    else {
                        continue;
                    };
                    if (e.kind == "lut3d" || e.kind == "image_mask") && p.name == "file" {
                        continue;
                    }
                    // Scope ids per effect so equal param names do not share edit state.
                    let mut an = None;
                    let (r, prect) = ui
                        .push_id((e.id, pi), |ui| {
                            let r = param_widget(ui, p, kind, &actx, &mut an);
                            (r, ui.min_rect())
                        })
                        .inner;
                    if !p.name.contains('/') && !p.name.contains(' ') {
                        self.fx_target(prect, target, i, &format!("param/{}", p.name));
                    }
                    if let Some(animation) = an {
                        cmds.push(Command::SetAnimation {
                            target: vjc_core::AnimTarget::Effect {
                                target,
                                effect: e.id,
                                param: pi,
                            },
                            animation,
                        });
                    }
                    if let Some(value) = r {
                        cmds.push(Command::SetEffectParam {
                            target,
                            effect: e.id,
                            param: pi,
                            value,
                        });
                    }
                }
            }
        }
    }

    /// Texture-memory watchdog (see vram.rs): drops thumbnails over the limit, else warns (never the scratch pool).
    fn vram_watch(&mut self) {
        let Some(comp) = self.comp.as_mut() else {
            self.vram_warn = None;
            self.vram_dog.clear();
            return;
        };
        let thumbs = std::cell::RefCell::new(&mut self.browser.fx_thumbs);
        let comp = std::cell::RefCell::new(comp);
        self.vram_bytes = {
            let mut u = comp.borrow().vram();
            u.thumbs = thumbs.borrow().bytes();
            u.total()
        };
        self.vram_warn = self.vram_dog.step(
            self.vram_limit,
            || {
                let mut u = comp.borrow().vram();
                u.thumbs = thumbs.borrow().bytes();
                u
            },
            |c| match c {
                vram::Cache::Thumbs => {
                    thumbs.borrow_mut().evict_all();
                }
            },
        );
    }

    /// Tabs above the grid: click selects, double click renames, "+" adds, menu removes.
    fn deck_tabs(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, cf_pos: f64) {
        let cur = self.project.composition.current_deck;
        let decks: Vec<(DeckId, String, usize)> = self
            .project
            .composition
            .decks
            .iter()
            .map(|d| (d.id, d.name.clone(), d.clips.len()))
            .collect();
        let last = decks.len() == 1;
        ui.horizontal_centered(|ui| {
            ui.add_space(theme::SP_3);
            ui.spacing_mut().item_spacing.x = theme::SP_2;
            ui.label("A");
            let mut pos = cf_pos;
            ui.spacing_mut().slider_width = 150.0;
            let cf = ui.add(
                egui::Slider::new(&mut pos, 0.0..=1.0)
                    .show_value(false)
                    .smallest_positive(0.001),
            );
            ui.label("B");
            let cf = help(cf, "crossfader");
            self.targets.push((cf.rect, "/comp/crossfader".into()));
            if cf.changed() && !self.learn {
                cmds.push(Command::SetCrossfader { value: pos });
            }
            let at = decks.iter().position(|d| d.0 == cur).unwrap_or(0);
            for (icon, tip, to) in [
                (icons::i::CARET_LEFT, "Previous deck", at.checked_sub(1)),
                (
                    icons::i::CARET_RIGHT,
                    "Next deck",
                    Some(at + 1).filter(|n| *n < decks.len()),
                ),
            ] {
                let r = ui.add_enabled(to.is_some(), egui::Button::new(icons::glyph(icon)));
                if icons::name(r, tip, None).clicked()
                    && let Some(t) = to
                {
                    cmds.push(Command::SelectDeck { deck: decks[t].0 });
                }
            }
            ui.add_space(theme::SP_3);
            for (i, (id, name, nclips)) in decks.iter().enumerate() {
                if let Some((rid, buf)) = self.renaming.as_mut().filter(|(r, _)| r == id) {
                    let rid = *rid;
                    let e = crate::widgets::field_text_hint(ui, buf, 90.0, "");
                    e.request_focus();
                    if e.lost_focus() {
                        let ok = ui.input(|i| !i.key_pressed(egui::Key::Escape));
                        if ok && !buf.trim().is_empty() && buf != name {
                            cmds.push(Command::RenameDeck {
                                deck: rid,
                                name: buf.trim().to_string(),
                            });
                        }
                        self.renaming = None;
                    }
                    continue;
                }
                let on = *id == cur;
                let text = egui::RichText::new(name).color(if on {
                    theme::ON_ACCENT
                } else {
                    theme::TEXT
                });
                let r = ui.add(
                    egui::Button::new(text)
                        .corner_radius(theme::PILL_ROUNDING)
                        .fill(if on { theme::ACCENT } else { theme::SECTION_BG })
                        .min_size(egui::vec2(60.0, theme::DECK_TABS_H - 6.0)),
                );
                r.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, name.clone())
                });
                let r = help(r, "deck-tab");
                if r.hovered() && *id != cur {
                    self.preload_deck(*id);
                }
                if r.double_clicked() {
                    self.renaming = Some((*id, name.clone()));
                } else if r.clicked() && *id != cur {
                    cmds.push(Command::SelectDeck { deck: *id });
                }
                r.context_menu(|ui| {
                    if crate::widgets::btn(ui, "Rename", crate::widgets::Kind::Secondary).clicked()
                    {
                        self.renaming = Some((*id, name.clone()));
                        ui.close();
                    }
                    if crate::widgets::btn(ui, "Duplicate", crate::widgets::Kind::Secondary)
                        .clicked()
                    {
                        if let Some(deck) = vjc_core::duplicate_deck(&self.project, *id) {
                            let new = deck.id;
                            cmds.push(Command::AddDeck {
                                deck,
                                index: i + 1,
                                select: None,
                            });
                            cmds.push(Command::SelectDeck { deck: new });
                        }
                        ui.close();
                    }
                    if crate::widgets::btn_if(
                        ui,
                        i > 0,
                        "Move left",
                        crate::widgets::Kind::Secondary,
                    )
                    .clicked()
                    {
                        cmds.push(Command::MoveDeck {
                            deck: *id,
                            index: i - 1,
                        });
                        ui.close();
                    }
                    if crate::widgets::btn_if(
                        ui,
                        i + 1 < decks.len(),
                        "Move right",
                        crate::widgets::Kind::Secondary,
                    )
                    .clicked()
                    {
                        cmds.push(Command::MoveDeck {
                            deck: *id,
                            index: i + 1,
                        });
                        ui.close();
                    }
                    if crate::widgets::btn_if(ui, !last, "Remove", crate::widgets::Kind::Secondary)
                        .clicked()
                    {
                        ui.close();
                        if confirm_remove_deck(name, *nclips) {
                            cmds.push(Command::RemoveDeck {
                                deck: *id,
                                select: None,
                            });
                        }
                    }
                });
            }
            if icons::button(ui, icons::i::PLUS, "Add deck").clicked() {
                let deck = Deck {
                    id: Uuid::new_v4(),
                    name: format!("Deck {}", decks.len() + 1),
                    clips: vec![],
                };
                let id = deck.id;
                cmds.push(Command::AddDeck {
                    deck,
                    index: decks.len(),
                    select: None,
                });
                cmds.push(Command::SelectDeck { deck: id });
            }
        });
    }

    /// Drops the per-screen UI state of screens that no longer exist (removed, or replaced by
    /// a preset), so the maps do not grow over a long session.
    fn prune_screen_state(&mut self) {
        let screens = &self.project.output.screens;
        // The legacy single output is keyed by the nil id.
        let alive = |id: &Uuid| id.is_nil() || screens.iter().any(|s| s.id == *id);
        self.test_cards.retain(alive);
        self.blackouts.retain(alive);
        self.pixel_grids.retain(|id, _| alive(id));
        self.closed_screens.retain(alive);
        self.out_fps.retain(|id, _| alive(id));
    }

    /// Re-reads the monitor list about once a second; closes the output if its monitor vanished.
    fn poll_monitors(&mut self) {
        let due = self
            .monitors_polled
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1));
        if !due {
            return;
        }
        self.monitors_polled = Some(std::time::Instant::now());
        let fresh = monitors::list();
        if let Some(id) = self.output
            && monitors::output_lost(&fresh, id)
        {
            self.output = None;
            self.status = "Output monitor disconnected: output window closed".into();
        }
        // Keep the last known list when enumeration transiently returns nothing.
        if !fresh.is_empty() {
            self.monitors = fresh;
        }
    }

    /// Dev aid (VJC_REHEARSE=1): a 1920x1080 LED-wall screen with three slices (left,
    /// middle, right) on the main display, windowed, with the test card on.
    fn rehearse_setup(&mut self) {
        let screen =
            vjc_core::rehearsal::screen(self.project.composition.layers.last().map(|l| l.id));
        let id = screen.id;
        let Some(mon) = self
            .monitors
            .iter()
            .find(|m| m.name.contains("main"))
            .or(self.monitors.first())
            .map(|m| m.id)
        else {
            self.status = "Rehearsal: no monitor found".into();
            return;
        };
        self.output_windowed = true;
        self.rehearsal = true;
        self.test_cards.insert(id);
        self.exec(Command::AddScreen { screen, index: 0 });
        self.exec(Command::SetScreenDevice {
            screen: id,
            device: OutputDevice::Monitor {
                name: mon.to_string(),
            },
        });
        // The scratch screen is not the user's edit.
        self.dirty = false;
    }

    /// Output > Fullscreen on a monitor (also leaves the rehearsal windowed mode).
    fn choose_fullscreen(&mut self, monitor: u32) {
        self.output_windowed = false;
        if let Some(first) = self.project.output.screens.first().map(|s| s.id) {
            self.closed_screens.remove(&first);
            self.exec(Command::SetScreenDevice {
                screen: first,
                device: OutputDevice::Monitor {
                    name: monitor.to_string(),
                },
            });
        } else {
            self.output = Some(monitor);
        }
    }

    /// One fullscreen window per open output: the legacy single output (no screens in the
    /// project) shows the whole composition; each monitor screen shows its sliced texture,
    /// or the whole composition when it has no slices.
    fn output_window(&mut self, ctx: &egui::Context) {
        reliability::set_output_live(false);
        let Some(comp_id) = self.comp_tex else { return };
        let comp_size = self.comp.as_ref().map_or((1, 1), |c| c.size);
        type Win = (Uuid, usize, egui::TextureId, Option<Screen>, (u32, u32));
        let mut wins: Vec<Win> = vec![];
        if self.project.output.screens.is_empty() {
            if let Some(mon) = self
                .output
                .and_then(|id| monitors::find(&self.monitors, id))
            {
                wins.push((Uuid::nil(), mon, comp_id, None, comp_size));
            }
        } else {
            for (id, mon) in self.open_screens() {
                // Before the screen's own target exists (first frame) the composition is shown,
                // and pixel-perfect must size by that texture, not by the screen.
                let (tex, tex_size) = monitors::pick_canvas(
                    self.screen_rt.get(&id).map(|t| (t.id, t.size)),
                    (comp_id, comp_size),
                );
                let info = self
                    .project
                    .output
                    .screens
                    .iter()
                    .find(|s| s.id == id)
                    .cloned();
                wins.push((id, mon, tex, info, tex_size));
            }
        }
        reliability::set_output_live(!wins.is_empty());
        for (sid, mon, tex, info, tex_size) in wins {
            // The list can change between pick and use (hotplug): skip a stale index.
            let Some(mid) = monitors::id_at(&self.monitors, mon) else {
                continue;
            };
            let mut builder = egui::ViewportBuilder::default().with_title("VJCraft Output");
            let place = monitors::output_placement(self.output_windowed, mon);
            builder = if place.fullscreen {
                builder.with_fullscreen(true).with_decorations(false)
            } else {
                builder.with_inner_size([960.0, 540.0])
            };
            if place.always_on_top {
                // A window behind the main one is occluded and macOS stops presenting it.
                builder = builder.with_always_on_top();
            }
            builder.monitor = place.monitor;
            let mut close = false;
            let mut leave_learn = false;
            // Esc leaves learn mode first; it must not also close the output.
            let learning = self.learn;
            let card = self.test_cards.contains(&sid);
            let black = self.blackouts.contains(&sid);
            let mirror = self.out_win.as_ref().filter(|w| w.show_in_output);
            let slice_ids = mirror.is_some_and(|w| w.identify);
            let grid = self.pixel_grids.get(&sid).copied().or_else(|| {
                mirror.filter(|w| w.led_grid).map(|w| {
                    (
                        w.led_cell.max(output_overlay::MIN_TILE),
                        w.led_cell.max(output_overlay::MIN_TILE),
                    )
                })
            });
            let mut toggle_black = false;
            let identify = output_overlay::identifying(self.identify_at, std::time::Instant::now());
            let number = self
                .project
                .output
                .screens
                .iter()
                .position(|s| s.id == sid)
                .map_or(1, |i| i + 1);
            let show_fps = self.show_fps;
            let out_fps = &mut self.out_fps;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(monitors::viewport_key(sid, mid)),
                builder,
                |ui, _class| {
                    let ctx = ui.ctx().clone();
                    ui_stats::child_pass(&ctx);
                    let full = ui.max_rect();
                    ui.painter()
                        .rect_filled(full, 0.0, crate::theme::MONITOR_BG);
                    // Physical pixels: the window's own scale factor (Retina, mixed monitors).
                    let rect = match &info {
                        Some(s) => monitors::present_rect(
                            full.size(),
                            ctx.pixels_per_point(),
                            tex_size,
                            s.pixel_perfect,
                        )
                        .translate(full.min.to_vec2()),
                        None => full,
                    };
                    ui.painter().image(
                        tex,
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        crate::theme::IMAGE_TINT,
                    );
                    if let Some(s) = &info {
                        let now = std::time::Instant::now();
                        let meter = out_fps.entry(sid).or_default();
                        meter.tick(now);
                        output_overlay::draw(
                            ui.painter(),
                            rect,
                            &output_overlay::Overlay {
                                screen: s,
                                test_card: card,
                                blackout: black,
                                grid,
                                identify: identify.then_some(number),
                                slice_ids,
                                fps: show_fps.then_some(meter.fps),
                            },
                        );
                    }
                    if ctx.input_mut(|i| i.consume_key(BLACKOUT_KEY.0, BLACKOUT_KEY.1)) {
                        // The main window never sees this viewport's keys.
                        toggle_black = true;
                    }
                    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
                    if esc && learning {
                        // The main window never sees this viewport's keys.
                        leave_learn = true;
                    }
                    if (esc && !learning) || ctx.input(|i| i.viewport().close_requested()) {
                        close = true;
                    }
                },
            );
            if leave_learn {
                self.learn_key(egui::Key::Escape);
            }
            if toggle_black {
                self.toggle_blackout();
            }
            if close {
                if sid.is_nil() {
                    self.output = None;
                } else {
                    self.closed_screens.insert(sid);
                }
            }
        }
    }
}

/// Every other app-wide Cmd chord, to prove none shares the black-out key.
#[cfg(test)]
const OTHER_CHORDS: &[egui::Key] = &[
    egui::Key::Num1,
    egui::Key::Num2,
    egui::Key::Num3,
    egui::Key::Num4,
    egui::Key::Slash,
    egui::Key::Z,
    egui::Key::S,
    egui::Key::O,
    egui::Key::N,
    egui::Key::Comma,
];

/// Global black-out chord (also read by the output windows).
const BLACKOUT_KEY: (egui::Modifiers, egui::Key) = (
    egui::Modifiers {
        alt: false,
        ctrl: false,
        shift: true,
        mac_cmd: false,
        command: true,
    },
    egui::Key::B,
);

impl eframe::App for State {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        reliability::beat();
        if reliability::parent_gone() {
            self.quit_orphaned();
        }
        if reliability::is_child()
            && let Some(wait) = self.publish_recovery()
        {
            ui.ctx().request_repaint_after(wait);
        }
        // Pace the UI: an unthrottled swapchain (vsync off, occluded window, output window
        // redrawing the parent) would otherwise run the whole UI thousands of times a second.
        let min_gap = ui_pace_gap(effective_fps(
            self.project.composition.fps,
            self.prefs.video.max_fps,
        ));
        if let Some(wait) = ui_pace_wait(self.last_ui, std::time::Instant::now(), min_gap) {
            std::thread::sleep(wait);
        }
        let t0 = std::time::Instant::now();
        self.last_ui = Some(t0);
        self.poll_monitors();
        self.tick_clock();
        self.audio_react.refresh(&self.project.composition);
        // Extra vsync ticks (120 Hz output) keep the last composition frame.
        if self.render_gate.due(
            t0,
            effective_fps(self.project.composition.fps, self.prefs.video.max_fps),
        ) {
            self.compose(frame);
        }
        self.serve_snapshot(frame);
        if self.frames.is_multiple_of(30) {
            self.vram_watch();
        }
        if let Some(rs) = frame.wgpu_render_state() {
            if self.gpu_name.is_empty() {
                self.gpu_name = rs.adapter.get_info().name;
            }
            self.browser
                .fx_thumbs
                .attach(rs.device.clone(), rs.queue.clone(), ui.ctx().clone());
        }
        State::ui(self, ui);
        let ctx = ui.ctx().clone();
        self.ui_stats.frame(&ctx);
        if self.show_stats {
            let fps = effective_fps(self.project.composition.fps, self.prefs.video.max_fps);
            self.stats.frame(t0, self.is_live(), fps);
        } else {
            self.stats.reset();
        }
        self.output_window(&ctx);
        self.frame_ms.push(t0.elapsed().as_secs_f32() * 1000.0);
        // VJC_FRAME_LOG=1 prints the counter every 120 frames (measuring N layers).
        self.frames += 1;
        if self.frames.is_multiple_of(120) && std::env::var_os("VJC_FRAME_LOG").is_some() {
            eprintln!("frame time: {}", self.frame_ms.label());
        }
        // Video is live: keep repainting. With nothing playing or published, wake up
        // at a low rate (input events still repaint at once).
        if self.is_live() {
            // Paced at the composition rate; input events still repaint at once.
            ctx.request_repaint_after(repaint_interval(effective_fps(
                self.project.composition.fps,
                self.prefs.video.max_fps,
            )));
        } else {
            ctx.request_repaint_after(IDLE_REPAINT);
        }
    }

    /// Closing the app mid-recording still writes the mp4 trailer (bounded wait).
    fn on_exit(&mut self) {
        self.hap.shutdown();
        if self.prefs_dirty {
            self.save_prefs();
        }
        if let Some(cap) = self.capture.take()
            && let Err(e) = cap.shutdown()
        {
            // The window is gone: stderr is the only place left to say it.
            eprintln!("Record failed on exit: {e}");
            self.status = format!("Record failed on exit: {e}");
        }
    }
}

#[cfg(test)]
mod ui_tests;

/// VJC_AUTOPLAY paths: a value that exists as a file is one path (it may contain the
/// PATH separator); otherwise it is split like PATH.
fn autoplay_paths(v: &std::ffi::OsStr) -> Vec<PathBuf> {
    let whole = PathBuf::from(v);
    if whole.is_file() {
        vec![whole]
    } else {
        std::env::split_paths(v).collect()
    }
}

/// Texture cap to use for a reported adapter limit: 16384 when unknown (0).
fn tex_cap_from(limit: u32) -> u32 {
    // Above 16384 a single texture is gigabytes; keep the old ceiling as the upper bound.
    if limit == 0 { 16384 } else { limit.min(16384) }
}

fn main() -> eframe::Result {
    // MCP server mode: stdio client of the running app's REST API, no window.
    let args: Vec<String> = std::env::args().collect();
    // Normal launch: a tiny supervisor runs the app as a child and relaunches it with
    // --recover after a crash (docs/dev/reliability.md).
    if reliability::should_supervise(&args, std::env::var("VJC_NO_SUPERVISOR").ok().as_deref()) {
        match reliability::run_supervisor(&args) {
            Some(code) => std::process::exit(code),
            None => eprintln!("VJCraft: running unsupervised"),
        }
    }
    if args.iter().any(|a| a == "--child") {
        reliability::start_parent_watch();
        reliability::fake_child(&args);
    }
    reliability::install_panic_hook();
    if args.iter().any(|a| a == "--panic-test") {
        panic!("panic-test: deliberate panic for the crash report test");
    }
    let recover = args.iter().any(|a| a == "--recover");
    reliability::start_watchdog();
    if args.iter().any(|a| a == "--mcp") {
        std::process::exit(mcp::run(&args));
    }
    let (prefs, warn) = match prefs_path() {
        Some(p) => Prefs::load(&p),
        None => (Prefs::default(), None),
    };
    let mut options = eframe::NativeOptions::default();
    // Open at 1600x1000, or maximized when the screen is smaller than that.
    let small = monitors::main_size_points().is_some_and(|(w, h)| w < 1600.0 || h < 1000.0);
    options.viewport = egui::ViewportBuilder::default()
        .with_inner_size([1600.0, 1000.0])
        .with_maximized(small);
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut options.wgpu_options.wgpu_setup {
        // eframe asks for 8192 by default; ask for what the adapter can do.
        let base = c.device_descriptor.clone();
        c.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut d = base(adapter);
            d.required_limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d;
            // HAP clips upload their blocks as BC textures when the GPU takes them.
            d.required_features |= adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
            d
        });
    }
    if !prefs.video.vsync {
        options.wgpu_options.surface.present_mode = wgpu::PresentMode::AutoNoVsync;
    }
    eframe::run_native(
        "VJCraft",
        options,
        Box::new(move |cc| {
            theme::apply_style(&cc.egui_ctx);
            wake::install(&cc.egui_ctx);
            let mut s = State::new(prefs);
            if let Some(rs) = &cc.wgpu_render_state {
                vjc_media::hap::set_gpu_supported(hap_gpu::supported(&rs.device));
                s.tex_cap = tex_cap_from(rs.adapter.limits().max_texture_dimension_2d);
                if s.prefs.video.output_10bit {
                    let (_, note) = color_depth::select_output_format(true, &[rs.target_format]);
                    if let Some(n) = note {
                        eprintln!("{n}");
                        s.warnings.push(n);
                    }
                }
            }
            if let Some(w) = warn {
                s.status = w.clone();
                s.warnings.push(w);
            }
            if recover {
                auto_recover(&mut s);
            } else {
                offer_recovery(&mut s);
            }
            if s.file.is_none()
                && !s.dirty
                && s.recovery_offer.is_none()
                && s.prefs.general.open_last_project
                && let Some(p) = s.prefs.general.last_project.clone().filter(|p| p.is_file())
            {
                s.load(p);
            }
            // Debug aid: VJC_AUTOPLAY=/path/video.mov loads it in the bottom layer, column 1.
            // Several paths (separated like PATH) fill layers from the bottom up, to measure
            // N layers together with VJC_FRAME_LOG=1.
            if let Some(p) = std::env::var_os("VJC_AUTOPLAY") {
                let ids: Vec<_> = s
                    .project
                    .composition
                    .layers
                    .iter()
                    .rev()
                    .map(|l| l.id)
                    .collect();
                for (l, path) in ids.into_iter().zip(autoplay_paths(&p)) {
                    s.drop_file(l, 0, path);
                }
            }
            if std::env::var_os("VJC_REHEARSE").is_some() {
                s.rehearse_setup();
            }
            // Debug aid: VJC_NETOUT=syphon|ndi publishes the composition on a new screen;
            // VJC_NETIN=<server name> plays that Syphon server on the top layer, column 1.
            if let Some(d) = std::env::var_os("VJC_NETOUT") {
                let device = if d == "ndi" {
                    OutputDevice::Ndi
                } else {
                    OutputDevice::Syphon
                };
                let (w, h) = (s.project.composition.width, s.project.composition.height);
                let scr = vjc_core::mapping::new_screen("Screen 1", w, h);
                let id = scr.id;
                s.exec(Command::AddScreen {
                    screen: scr,
                    index: 0,
                });
                s.exec(Command::SetScreenDevice { screen: id, device });
            }
            if let Some(n) = std::env::var_os("VJC_NETIN") {
                let top = s.project.composition.layers.first().map(|l| l.id);
                let deck = s.project.composition.current_deck;
                if let Some(layer) = top {
                    let name = n.to_string_lossy().into_owned();
                    let mut clip = vjc_core::clip_from_path(PathBuf::new());
                    clip.name = name.clone();
                    clip.source = vjc_model::Source::Network {
                        protocol: vjc_model::NetProtocol::Syphon,
                        name,
                    };
                    s.exec(Command::SetClip {
                        deck,
                        layer,
                        column: 0,
                        clip,
                    });
                    s.exec(Command::TriggerClip {
                        deck,
                        layer,
                        column: 0,
                    });
                }
            }
            Ok(Box::new(s))
        }),
    )
}

/// Local time of day as HH:MM:SS (the status bar clock).
#[cfg(unix)]
fn local_clock() -> String {
    // SAFETY: `time` and `localtime_r` only write into the locals passed to them.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return "--:--:--".into();
        }
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

/// UTC time of day where the local zone is not read (non-Unix targets).
#[cfg(not(unix))]
fn local_clock() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let t = secs % 86_400;
    format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

/// Autopilot targets: files (advance by loops or at the end) and generators (advance by
/// bars). Live sources never finish or loop, so the layer would stop advancing on them.
fn autopilot_playable(source: &vjc_model::Source) -> bool {
    matches!(
        source,
        vjc_model::Source::File { .. } | vjc_model::Source::Generator { .. }
    )
}

/// True when the window got larger in either axis. A zero `prev` (first frame, or a
/// minimized window) counts: restoring from nothing must re-force clipped panels.
fn window_grew(prev: egui::Vec2, now: egui::Vec2) -> bool {
    now.x > prev.x + 0.5 || now.y > prev.y + 0.5
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_layer_shows_its_own_cell_and_every_redirect() {
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let deck = Uuid::from_u128(9);
        // Source layer `a`: its cells 0 and 1 play redirected on `b` and `c`.
        let origin = HashMap::from([(b, (deck, a, 0)), (c, (deck, a, 1))]);
        let air = |l: LayerId| {
            if l == b {
                Some((deck, 0))
            } else if l == c {
                Some((deck, 1))
            } else {
                None
            }
        };
        assert_eq!(
            super::cells_on_air(&origin, a, air),
            vec![(deck, 0), (deck, 1)]
        );
        // A layer that only shows a redirected clip does not light its own column.
        assert!(super::cells_on_air(&origin, b, air).is_empty());
        // Own clip and a redirect together.
        let own = |l: LayerId| if l == a { Some((deck, 2)) } else { air(l) };
        assert_eq!(
            super::cells_on_air(&origin, a, own),
            vec![(deck, 2), (deck, 0), (deck, 1)],
            "own column first, so `active_column` is not the lowest redirect"
        );
    }

    #[test]
    fn resume_ignores_a_replaced_clip_and_is_pruned() {
        let mut p = vjc_model::Project::new("t");
        let l = new_layer(0);
        let lid = l.id;
        p.composition.layers.push(l);
        let deck = p.composition.decks[0].id;
        p.composition.decks[0].clips.push(vjc_model::CellClip {
            layer: lid,
            column: 0,
            clip: vjc_core::clip_from_path("/nope/a.mp4".into()),
        });
        let key = (deck, lid, 0);
        let id = p.composition.decks[0].clips[0].clip.id;
        let mut r = std::collections::HashMap::from([(key, (id, 0.4))]);
        assert_eq!(super::resume_pos(&p, &r, key), Some(0.4));
        // Negative control: the cell now holds another file/clip.
        p.composition.decks[0].clips[0].clip = vjc_core::clip_from_path("/nope/b.mp4".into());
        assert_eq!(super::resume_pos(&p, &r, key), None);
        super::prune_resume(&p, &mut r);
        assert!(r.is_empty());
        // A deleted cell is pruned too.
        r.insert(key, (id, 0.4));
        p.composition.decks[0].clips.clear();
        super::prune_resume(&p, &mut r);
        assert!(r.is_empty());
    }
    #[test]
    fn autoplay_whole_value_is_one_path_when_it_is_a_file() {
        let d = std::env::temp_dir().join(format!("vjc-ap-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(format!("a{}b.mp4", if cfg!(windows) { ';' } else { ':' }));
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(super::autoplay_paths(f.as_os_str()), vec![f.clone()]);
        // negative control: a non-existing value is still split
        let sep = if cfg!(windows) { ";" } else { ":" };
        let multi = std::ffi::OsString::from(format!("/no/a.mp4{sep}/no/b.mp4"));
        assert_eq!(super::autoplay_paths(&multi).len(), 2);
        std::fs::remove_dir_all(&d).ok();
    }
    #[test]
    fn preview_warns_when_selected_layer_video_is_off() {
        assert_eq!(preview_note(true, false), "Selected layer");
        assert!(preview_note(true, true).contains("video is off"));
        assert!(preview_note(false, false).starts_with("Composition"));
    }

    #[test]
    fn outgoing_transition_keeps_the_clip_channel_mask() {
        let mut k = vjc_model::Clip {
            extras: Default::default(),
            id: Uuid::new_v4(),
            name: "c".into(),
            source: vjc_model::Source::File {
                path: "a.mov".into(),
            },
            transport: Default::default(),
            track: vjc_model::Track::new(vjc_model::BlendMode::Normal),
            blend_override: None,
            transition: None,
            trigger_style: Default::default(),
            target: Default::default(),
            beat_snap: None,
            fit: Default::default(),
            channels: Default::default(),
        };
        k.channels.g = false;
        assert!(!channels_of(Some(&k)).g);
        assert_eq!(channels_of(None), Default::default());
    }

    #[test]
    fn window_growth_reforces_panels() {
        let v = egui::vec2;
        assert!(super::window_grew(v(800.0, 500.0), v(1200.0, 500.0)));
        assert!(super::window_grew(v(800.0, 500.0), v(800.0, 700.0)));
        assert!(!super::window_grew(v(800.0, 500.0), v(800.0, 500.0)));
        assert!(!super::window_grew(v(800.0, 500.0), v(600.0, 400.0)));
        assert!(super::window_grew(v(0.0, 0.0), v(800.0, 500.0)));
        assert!(!super::window_grew(v(800.0, 500.0), v(0.0, 0.0)));
    }

    #[test]
    fn autopilot_takes_files_and_generators_not_live() {
        use vjc_model::Source;
        let f = Source::File {
            path: "a.mp4".into(),
        };
        assert!(super::autopilot_playable(&f));
        let g = Source::Generator {
            kind: "Plasma".into(),
            params: Default::default(),
        };
        assert!(super::autopilot_playable(&g));
        let live = Source::Network {
            protocol: vjc_model::NetProtocol::Syphon,
            name: "x".into(),
        };
        assert!(!super::autopilot_playable(&live));
    }

    use super::*;

    #[test]
    fn recovery_cleanup_and_quarantine() {
        let d = std::env::temp_dir().join(format!("vjc-rec-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let r = d.join("recovery.vjc");
        std::fs::write(&r, b"x").unwrap();
        clear_recovery(Some(r.clone()));
        assert!(!r.exists());
        clear_recovery(Some(r.clone())); // already gone: no panic
        clear_recovery(None);
        std::fs::write(&r, b"bad").unwrap();
        quarantine_recovery(&r);
        assert!(!r.exists());
        assert_eq!(std::fs::read(d.join("recovery.bad.vjc")).unwrap(), b"bad");
        std::fs::write(&r, b"bad2").unwrap();
        quarantine_recovery(&r);
        assert_eq!(std::fs::read(d.join("recovery.bad.vjc")).unwrap(), b"bad");
        assert_eq!(
            std::fs::read(d.join("recovery.bad-2.vjc")).unwrap(),
            b"bad2"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    fn cell(x: f32, layer: LayerId, col: u32, empty: bool) -> CellInfo {
        CellInfo {
            rect: egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(10.0, 10.0)),
            layer,
            col,
            empty,
        }
    }

    #[test]
    fn hit_test_and_fallbacks() {
        let (top, bot) = (Uuid::new_v4(), Uuid::new_v4());
        let cells = [
            cell(0.0, top, 0, true),
            cell(20.0, top, 1, true),
            cell(0.0, bot, 0, false),
            cell(20.0, bot, 1, true),
        ];
        let hit = resolve_drop(&cells, Some(egui::pos2(22.0, 5.0)), None);
        assert_eq!(hit, Some((top, 1, DropWhere::Pointer)));
        let miss = Some(egui::pos2(500.0, 500.0));
        assert_eq!(
            resolve_drop(&cells, miss, Some((bot, 0))),
            Some((bot, 0, DropWhere::Selected))
        );
        assert_eq!(
            resolve_drop(&cells, None, None),
            Some((bot, 1, DropWhere::FirstEmpty))
        );
        let full = [cell(0.0, bot, 0, false), cell(20.0, bot, 1, false)];
        assert_eq!(
            resolve_drop(&full, None, None),
            Some((bot, 0, DropWhere::FirstCell))
        );
        assert_eq!(resolve_drop(&[], None, None), None);
    }
}

/// True when the playhead only moves forward (Loop/Once/OnceHold, forward, speed > 0), so
/// the player can stream instead of caching the clip.
fn is_sequential(mode: vjc_model::PlayMode, dir: Direction, rate: f64) -> bool {
    use vjc_model::PlayMode;
    // A paused playhead (Pause or rate 0) holds its frame; the stream can
    // serve that without caching the whole clip.
    matches!(mode, PlayMode::Loop | PlayMode::Once | PlayMode::OnceHold)
        && (matches!(dir, Direction::Pause) || (matches!(dir, Direction::Forward) && rate >= 0.0))
}

#[cfg(test)]
mod audio_mix_tests {
    use super::*;

    #[test]
    fn audio_mix_mutes_when_layer_audio_is_off_and_applies_master() {
        let mut p = Project::new("t");
        let layer = new_layer(1);
        let id = layer.id;
        p.composition.layers.push(layer);
        let clip = vjc_core::clip_from_path("/x/a.mov".into());
        p.composition.master.volume.set_number(0.5);
        let (vol, _) = audio_mix(&p, id, &clip);
        assert!((vol - 0.5).abs() < 1e-6, "master volume scales: {vol}");
        p.composition.layers[0].audio = false;
        assert_eq!(audio_mix(&p, id, &clip).0, 0.0);
        // Video off leaves the audio alone.
        p.composition.layers[0].audio = true;
        p.composition.layers[0].video = false;
        assert!(audio_mix(&p, id, &clip).0 > 0.0);
        // Layer master scales the sound; the picture-only opacity does not.
        p.composition.layers[0].extras.master = 0.5;
        let half = audio_mix(&p, id, &clip).0;
        assert!((half - 0.25).abs() < 1e-6, "master {half}");
        p.composition.layers[0].track.opacity.set_number(0.1);
        assert!((audio_mix(&p, id, &clip).0 - half).abs() < 1e-6);
        p.composition.layers[0].extras.master = 1.0;
        // Stripping the clip's audio mutes it; stripping video does not.
        let mut stripped = clip.clone();
        stripped.extras.strip_video = true;
        assert!(audio_mix(&p, id, &stripped).0 > 0.0);
        stripped.extras.strip_audio = true;
        assert_eq!(audio_mix(&p, id, &stripped).0, 0.0);
    }
}

#[cfg(test)]
mod sequential_tests {
    use super::*;

    #[test]
    fn non_number_param_uses_default() {
        let p = vjc_model::Param {
            name: "v".into(),
            value: vjc_model::Value::Bool(true),
            default: vjc_model::Value::Bool(true),
            range: None,
            animation: None,
        };
        assert_eq!(num_or(&p, 1.0), 1.0);
        assert_eq!(num_or(&p, 0.0), 0.0);
    }
    use vjc_model::PlayMode;

    #[test]
    fn only_forward_modes_stream() {
        assert!(is_sequential(PlayMode::Loop, Direction::Forward, 1.0));
        assert!(is_sequential(PlayMode::OnceHold, Direction::Forward, 2.0));
        assert!(!is_sequential(PlayMode::Bounce, Direction::Forward, 1.0));
        assert!(!is_sequential(PlayMode::Loop, Direction::Backward, 1.0));
        assert!(!is_sequential(PlayMode::Loop, Direction::Forward, -1.0));
        assert!(!is_sequential(PlayMode::Unknown, Direction::Forward, 1.0));
        // Pausing must not pull the whole clip into RAM.
        assert!(is_sequential(PlayMode::Loop, Direction::Forward, 0.0));
        assert!(is_sequential(PlayMode::Loop, Direction::Pause, 1.0));
    }

    #[test]
    fn palette_keys_yield_to_another_focused_widget() {
        let q = egui::Id::new("q");
        assert!(super::palette_owns_keys(None, q));
        assert!(super::palette_owns_keys(Some(q), q));
        assert!(!super::palette_owns_keys(Some(egui::Id::new("other")), q));
    }

    #[test]
    fn every_loops_edit_keeps_the_unit() {
        let a = vjc_model::Autopilot {
            unit: vjc_model::AutopilotUnit::Seconds,
            ..Default::default()
        };
        let b = super::every_loops_edit(&a, 4.2);
        assert_eq!(b.every_loops, 4);
        assert_eq!(b.unit, vjc_model::AutopilotUnit::Seconds);
    }
}

/// Numeric value of a param; `default` when it holds anything else (volume: unity, not mute).
fn num_or(param: &vjc_model::Param, default: f64) -> f64 {
    match param.value {
        vjc_model::Value::Number(n) => n,
        _ => default,
    }
}

/// Linear volume and pan for a clip on a layer: volumes multiply (clip, layer, master), pans
/// add (clamped). Opacity does not affect audio.
fn audio_mix(p: &Project, layer: LayerId, clip: &vjc_model::Clip) -> (f32, f32) {
    let num = |param: &vjc_model::Param| num_or(param, 0.0);
    let vnum = |param: &vjc_model::Param| num_or(param, 1.0);
    let lt = p
        .composition
        .layers
        .iter()
        .find(|l| l.id == layer)
        .map(|l| &l.track);
    let mut vol = vnum(&clip.track.volume) * vnum(&p.composition.master.volume);
    if p.composition
        .layers
        .iter()
        .any(|l| l.id == layer && !l.audio)
        || clip.extras.strip_audio
    {
        vol = 0.0;
    }
    let mut pan = num(&clip.track.pan) + num(&p.composition.master.pan);
    if let Some(t) = lt {
        vol *= vnum(&t.volume);
        pan += num(&t.pan);
    }
    // Layer master scales the sound with the picture.
    if let Some(l) = p.composition.layers.iter().find(|l| l.id == layer) {
        vol *= l.extras.master;
    }
    (vol.max(0.0) as f32, pan.clamp(-1.0, 1.0) as f32)
}

/// Size of a transport bar button.
fn vec2_btn() -> egui::Vec2 {
    egui::vec2(28.0, 28.0)
}

/// Small-caps style label for transport buttons (semibold, extra small).
fn caps(ui: &egui::Ui, t: &str) -> egui::RichText {
    egui::RichText::new(t).font(theme::bold_font(ui.ctx(), theme::FS_XS))
}

/// Text of a panel tab: semibold and bright when current, muted otherwise.
fn tab_text(ui: &egui::Ui, t: &str, on: bool) -> egui::RichText {
    egui::RichText::new(t)
        .font(theme::bold_font(ui.ctx(), theme::FS_SM))
        .color(if on { theme::TEXT } else { theme::TEXT_MUTED })
}

/// Context-menu part of an effect row: saved presets of this effect (click to load onto
/// this instance, undoable) and a name box that saves the current params.
fn fx_preset_menu(
    ui: &mut egui::Ui,
    root: &std::path::Path,
    target: EffectTarget,
    e: &vjc_model::Effect,
    status: &mut String,
    cmds: &mut Vec<Command>,
) {
    use vjc_io::effect_presets as fp;
    let names = fp::list(root, &e.kind);
    ui.menu_button("Load preset", |ui| {
        if names.is_empty() {
            ui.weak("No presets yet");
        }
        for n in &names {
            ui.horizontal(|ui| {
                if crate::widgets::btn(ui, n, crate::widgets::Kind::Secondary).clicked() {
                    match fp::load(root, &e.kind, n) {
                        Ok(p) => {
                            cmds.push(Command::SetEffectParams {
                                target,
                                effect: e.id,
                                params: p.params,
                            });
                            ui.close();
                        }
                        Err(err) => *status = format!("Preset: {err}"),
                    }
                }
                if icons::button(ui, icons::i::X, "Delete preset").clicked() {
                    match fp::delete(root, &e.kind, n) {
                        Ok(()) => ui.close(),
                        Err(err) => *status = format!("Preset: {err}"),
                    }
                }
            });
        }
    });
    let id = ui.id().with(("fx-preset-name", e.id));
    let mut name: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal(|ui| {
        crate::widgets::field_text_hint(ui, &mut name, 120.0, "Preset name");
        let ok = crate::widgets::btn_if(
            ui,
            !name.trim().is_empty(),
            "Save",
            crate::widgets::Kind::Secondary,
        )
        .clicked();
        if ok {
            match fp::save(root, &e.kind, &name, &e.params) {
                Ok(_) => {
                    *status = format!("Saved preset {}", name.trim());
                    name.clear();
                    ui.close();
                }
                Err(err) => *status = format!("Preset: {err}"),
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, name));
}

#[test]
fn numbered_image_folder_drops_as_one_sequence_clip() {
    let d = std::env::temp_dir().join(format!("vjc-seqdrop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for i in 1..=3 {
        std::fs::write(d.join(format!("shot_{i:04}.png")), b"x").unwrap();
    }
    // Option on: the folder stays whole (one clip); option off: one file per frame.
    let (on, _) = expand_drop(vec![d.clone()], true);
    assert_eq!(on, vec![d.clone()]);
    let (off, _) = expand_drop(vec![d.clone()], false);
    assert_eq!(off.len(), 3);
    // Negative control: another file in the folder makes it an ordinary folder drop.
    std::fs::write(d.join("clip.mp4"), b"x").unwrap();
    let (mixed, _) = expand_drop(vec![d.clone()], true);
    assert_eq!(mixed.len(), 4);
    // The clip built from a scanned folder plays frames at the default fps.
    std::fs::remove_file(d.join("clip.mp4")).unwrap();
    let s = vjc_media::sequence::scan_dir(&d).unwrap();
    let c = vjc_core::clip_from_sequence(s.pattern.clone(), 30.0);
    assert_eq!(c.name, "shot");
    assert!(matches!(c.source, vjc_model::Source::Sequence { fps, .. } if fps == 30.0));
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(test)]
mod remap_tests {
    use super::*;

    #[test]
    fn ui_scale_drag_follows_the_pointer_and_cannot_stick() {
        use egui::{Event, PointerButton, Pos2, RawInput};
        let ctx = egui::Context::default();
        let mut v = 1.0f32;
        let frame = |ev: Vec<Event>, draw: bool, v: &mut f32| -> bool {
            let raw = RawInput {
                events: ev,
                ..Default::default()
            };
            let mut dragging = false;
            let mut out = ctx.run_ui(raw, |ui| {
                if draw {
                    let r = ui.add(egui::Slider::new(v, 0.0..=10.0));
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(ui_scale_slider_key(), r.id));
                }
                dragging = ui_scale_dragging(ui.ctx());
            });
            out.textures_delta.clear();
            dragging
        };
        let at = |x: f32| Pos2::new(x, 12.0);
        let mv = |x: f32| Event::PointerMoved(at(x));
        let btn = |x: f32, pressed: bool| Event::PointerButton {
            pos: at(x),
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(vec![mv(40.0)], true, &mut v);
        frame(vec![btn(40.0, true)], true, &mut v);
        let mut during = false;
        for x in [60.0, 80.0, 100.0] {
            during = frame(vec![mv(x)], true, &mut v);
        }
        assert!(during, "dragging while the button is held on the slider");
        // The slider stops being drawn mid-drag, then the button goes up: not stuck.
        frame(vec![mv(110.0)], false, &mut v);
        let released = frame(vec![btn(110.0, false)], false, &mut v);
        assert!(!released, "false on the release frame");
        assert!(!frame(vec![], false, &mut v), "and after it");
    }

    #[test]
    fn ui_scale_is_applied_only_when_the_pref_changes() {
        let mut applied = None;
        assert_eq!(
            scale_to_apply(&mut applied, 1.0, false),
            Some(1.0),
            "first frame"
        );
        assert_eq!(
            scale_to_apply(&mut applied, 1.0, false),
            None,
            "unchanged: Cmd +/- survives"
        );
        // While the slider is dragged nothing is applied; the release applies the value.
        assert_eq!(scale_to_apply(&mut applied, 1.25, true), None);
        assert_eq!(scale_to_apply(&mut applied, 1.5, true), None);
        assert_eq!(scale_to_apply(&mut applied, 1.5, false), Some(1.5));
        assert_eq!(scale_to_apply(&mut applied, 1.5, false), None);
    }

    #[test]
    fn remap_covers_every_column_bearing_command() {
        let map = |c: u32| Some(c + 10);
        let (d, l) = (Uuid::new_v4(), Uuid::new_v4());
        let mut clear = Command::ClearCell {
            deck: d,
            layer: l,
            column: 1,
        };
        assert!(remap_command(&mut clear, &map));
        assert!(matches!(clear, Command::ClearCell { column: 11, .. }));
        let mut mv = Command::MoveClip {
            deck: d,
            from: (l, 1),
            to: (l, 2),
            copy: false,
        };
        assert!(remap_command(&mut mv, &map));
        assert!(matches!(
            mv,
            Command::MoveClip {
                from: (_, 11),
                to: (_, 12),
                ..
            }
        ));
        let mut paused = Command::SetPaused {
            clips: vec![(d, l, 3), (d, l, 4)],
            paused: true,
        };
        assert!(remap_command(&mut paused, &|c| (c == 3).then_some(0)));
        assert!(matches!(&paused, Command::SetPaused { clips, .. } if clips == &[(d, l, 0)]));
        let mut gone = Command::ClearCell {
            deck: d,
            layer: l,
            column: 5,
        };
        assert!(
            !remap_command(&mut gone, &|_| None),
            "removed column drops it"
        );
    }
}
