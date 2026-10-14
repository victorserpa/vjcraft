mod api_io;
mod artnet_out;
mod audio_react;
mod browser;
mod capture;
mod clip_thumbs;
mod compositor;
mod dmx_io;
mod frame_stats;
mod fx;
mod fx_thumbs;
mod grid_ui;
mod help;
mod icons;
mod link_io;
mod midi_io;
mod ml_fx;
mod ml_gpu;
mod monitor;
mod monitors;
mod netout;
mod osc_io;
mod output_overlay;
mod output_presets_ui;
mod output_ui;
mod params;
mod persist;
mod pool;
mod prefs_ui;
mod preload;
mod slice_inspector;
mod slice_render;
mod source_info;
mod theme;
mod timecode_io;
mod wake;

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
}

struct State {
    project: Project,
    history: History,
    warnings: Vec<String>,
    status: String,
    frame_ms: frame_stats::FrameStats,
    /// Decoders opened ahead for the deck the user is about to switch to.
    preload: preload::Preload,
    frames: u64,
    /// Playing clip per layer: (deck it was triggered from, column, player). One clip per
    /// layer. Lookups of the playing clip use this deck, never `current_deck`.
    players: HashMap<LayerId, (DeckId, u32, Player)>,
    /// Audio output, opened on the first clip start (None until then or when unavailable).
    audio: Option<AudioEngine>,
    /// Click output, created the first time a beat lands with Metronome on.
    metro: Option<vjc_media::audio::Metronome>,
    audio_failed: bool,
    /// Output device `audio` was (or will be) opened on.
    audio_device: Option<String>,
    /// `audio_failed` came from a real open error (not from the UI-test setup).
    audio_error: bool,
    /// Names of the audio output devices listed in Preferences > Audio.
    audio_outputs: Option<Vec<String>>,
    /// Writes prefs and the recovery file (false in UI tests).
    persist: bool,
    /// Playing generator clip per layer: (deck, column, own TIME in seconds). The time
    /// follows the clip transport (speed, direction, pause); a layer has a player or a
    /// generator, never both.
    gens: HashMap<LayerId, (DeckId, u32, f64)>,
    /// Deck tab being renamed, with its edit buffer.
    renaming: Option<(DeckId, String)>,
    /// Tests pin the clock the undo history coalesces with, so a slow machine cannot split
    /// one gesture into two steps.
    #[cfg(test)]
    history_now: Option<std::time::Instant>,
    /// Column header being renamed: (column id, text so far).
    col_renaming: Option<(Uuid, String)>,
    /// Playhead per layer, restarted with each trigger (stale entries are harmless).
    heads: HashMap<LayerId, Playhead>,
    /// Codec / size / fps of file clips, probed on demand.
    src_info: source_info::SourceInfos,
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
    /// Set by `monitor_column` when drawn; `compose` renders the preview only then.
    monitor_seen: bool,
    /// Output recording in progress (runtime only).
    capture: Option<capture::Capture>,
    /// Syphon / NDI publishers of the screens set to those devices (runtime only).
    netout: netout::NetOut,
    artnet: artnet_out::ArtNetOut,
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
    led_prev: HashMap<vjc_core::midi::Led, bool>,
    /// OSC sockets thread, the config last sent to it, and the output state last sent.
    osc: osc_io::Osc,
    osc_cfg: Option<osc_io::Cfg>,
    osc_prev: HashMap<String, f64>,
    osc_limit: vjc_core::osc::RateLimit,
    /// Art-Net input thread and the config last sent to it.
    dmx: dmx_io::Dmx,
    dmx_cfg: Option<dmx_io::Cfg>,
    api: api_io::Api,
    api_cfg: Option<api_io::Cfg>,
    /// Mappable widgets drawn this frame, for learn clicks and the key overlay.
    targets: Vec<(egui::Rect, String)>,
    /// Master bypass: black output (runtime only, not saved).
    master_bypass: bool,
    /// Path of the open .vjc, if any.
    file: Option<PathBuf>,
    dirty: bool,
    last_autosave: std::time::Instant,
    prefs: Prefs,
    prefs_open: bool,
    output_setup_open: bool,
    sel_screen: Option<Uuid>,
    sel_slice: Option<Uuid>,
    /// Slices selected together (shift / cmd click); empty means only `sel_slice`.
    sel_slices: Vec<Uuid>,
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
    geom_side: vjc_core::slice_geom::Side,
    /// Output canvas edits the slice mask polygon instead of the warp mesh.
    edit_mask: bool,
    sel_mask_pt: Option<usize>,
    /// Screens whose output window the user closed (runtime only).
    closed_screens: std::collections::HashSet<Uuid>,
    /// Dev rehearsal (VJC_REHEARSE): output windows open windowed, not fullscreen.
    output_windowed: bool,
    /// Rehearsal session: its scratch screen is never autosaved or recovered.
    rehearsal: bool,
    /// Screens showing the test card on their output (runtime only).
    test_cards: std::collections::HashSet<Uuid>,
    /// FPS overlay on every output.
    show_fps: bool,
    /// When "Identify displays" was pressed (shows for `output_overlay::IDENTIFY_FOR`).
    identify_at: Option<std::time::Instant>,
    display_info_open: bool,
    /// Presented-frame rate per output window.
    out_fps: HashMap<Uuid, output_overlay::FpsMeter>,
    slice_r: Option<slice_render::SliceRenderer>,
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
    nudge_dir: i8,
    /// Visual only for now. TODO(#9): click sound (see build-log).
    metronome: bool,
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
    Audio,
    Video,
    Control,
    Osc,
    Dmx,
    Api,
    Timecode,
    Midi,
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
        if let Some(isf) = lib.isf(&e.kind) {
            out.push(EffectDraw {
                kind: e.kind.clone(),
                uniforms: isf.pack(&vjc_core::anim::resolve(&e.params, ctx), time, size),
                dry_wet: e.dry_wet,
                ml: ml_draw,
                persistent: isf.persistent.then_some(e.id),
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
                    if ui.button(join).clicked() {
                        cmds.push(Command::MoveLayerToGroup {
                            layer: me.id,
                            group,
                        });
                        ui.close();
                    }
                } else if ui.button(label).clicked() {
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
            if ui
                .add_enabled(members > 1 && edge, egui::Button::new("Remove from group"))
                .clicked()
            {
                cmds.push(Command::MoveLayerOutOfGroup { layer: me.id });
                ui.close();
            }
            if ui.button("Ungroup").clicked() {
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
            egui::Button::new(icons::with_text(ui, icons::i::CARET_DOWN, &g.name))
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
            if ui.button("Ungroup").clicked() {
                cmds.push(Command::Ungroup { group: gid });
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
    egui::ComboBox::from_id_salt(("gblend", gid))
        .selected_text(label)
        .width(90.0)
        .show_ui(ui, |ui| {
            for (b, name) in vjc_core::blend::COMPOSITION_MODES {
                if ui.selectable_label(g.track.blend == *b, *name).clicked() {
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
        if ui.button(fx_label(id)).clicked()
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

fn active_of(
    players: &HashMap<LayerId, (DeckId, u32, Player)>,
    gens: &Gens,
    layer: LayerId,
) -> Option<(DeckId, u32)> {
    let video = players.get(&layer).map(|(d, c, _)| (*d, *c));
    video.or_else(|| gens.get(&layer).map(|(d, c, _)| (*d, *c)))
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
            if ui.button(fx_label(id)).clicked() {
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
    }
}

fn labeled<T: Copy>(v: &[(T, &str)]) -> Vec<(T, String)> {
    v.iter().map(|(a, n)| (*a, n.to_string())).collect()
}

fn number_of(v: &vjc_model::Value) -> f64 {
    match v {
        vjc_model::Value::Number(n) => *n,
        _ => 0.0,
    }
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
                if ui.checkbox(&mut x, "").changed() {
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
                egui::ComboBox::from_id_salt(("fxlong", name))
                    .selected_text(labels.get(sel as usize).map_or("", String::as_str))
                    .show_ui(ui, |ui| {
                        for (i, l) in labels.iter().enumerate() {
                            ui.selectable_value(&mut sel, i as u32, l);
                        }
                    });
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
    if let Some(r) = path {
        let _ = std::fs::remove_file(r);
    }
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
    let Some(r) = recovery_path() else { return };
    if !r.exists() {
        return;
    }
    let ask = rfd::MessageDialog::new()
        .set_title("Recover project")
        .set_description("VJCraft found an autosaved project from a previous session. Restore it?")
        .set_buttons(rfd::MessageButtons::OkCancelCustom(
            "Restore".into(),
            "Discard".into(),
        ))
        .show();
    let restore = matches!(ask, rfd::MessageDialogResult::Custom(ref l) if l == "Restore");
    if restore && s.load(r.clone()) {
        s.file = None;
        s.dirty = true;
        // Restored state now lives in memory (and is re-autosaved while dirty).
        clear_recovery(Some(r));
    } else if restore {
        // Unreadable: do not offer it again, but do not destroy the user's work either.
        quarantine_recovery(&r);
    } else {
        clear_recovery(Some(r));
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

fn new_layer(n: usize) -> Layer {
    Layer {
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
        audio: true,
        video: true,
        trigger_mode: vjc_model::TriggerMode::Restart,
        group: None,
    }
}

/// New project: default grid plus the user's composition and shortcut prefs.
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
        Self::build(prefs, true)
    }

    /// State for UI tests: no MIDI/OSC threads, no monitors, no audio, and nothing written
    /// to the user's preferences or recovery file.
    #[cfg(test)]
    fn headless(prefs: Prefs) -> Self {
        Self::build(prefs, false)
    }

    /// Recovery file, only for the live app.
    fn recovery_path(&self) -> Option<PathBuf> {
        (self.persist && !self.rehearsal)
            .then(recovery_path)
            .flatten()
    }

    fn build(prefs: Prefs, live: bool) -> Self {
        let project = fresh_project(&prefs);
        State {
            project,
            history: History::new(),
            warnings: vec![],
            status: "Drop video files on the grid cells.".into(),
            frame_ms: frame_stats::FrameStats::new(),
            preload: preload::Preload::default(),
            frames: 0,
            players: HashMap::new(),
            audio: None,
            metro: None,
            gens: HashMap::new(),
            heads: HashMap::new(),
            src_info: Default::default(),
            flow: Flow::default(),
            last_head_tick: std::time::Instant::now(),
            comp: None,
            fx: fx::FxLib::new(),
            ml: ml_fx::MlHub::new(),
            fx_epoch: std::time::Instant::now(),
            audio_react: audio_react::AudioReact::default(),
            panel_tab: PanelTab::Composition,
            comp_tex: None,
            prev_tex: None,
            mon_views: Default::default(),
            monitor_seen: false,
            capture: None,
            netout: Default::default(),
            artnet: Default::default(),
            rec_request: None,
            output: None,
            monitors: if live { monitors::list() } else { vec![] },
            monitors_polled: (!live).then(std::time::Instant::now),
            persist: live,
            audio_failed: !live,
            audio_device: prefs.audio.output_device.clone(),
            audio_error: false,
            audio_outputs: None,
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
            led_prev: HashMap::new(),
            targets: vec![],
            master_bypass: false,
            file: None,
            dirty: false,
            last_autosave: std::time::Instant::now(),
            prefs_open: false,
            output_setup_open: false,
            sel_screen: None,
            sel_slice: None,
            sel_slices: vec![],
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
            geom_side: vjc_core::slice_geom::Side::Output,
            edit_mask: false,
            sel_mask_pt: None,
            closed_screens: Default::default(),
            output_windowed: false,
            rehearsal: false,
            test_cards: Default::default(),
            show_fps: false,
            identify_at: None,
            display_info_open: false,
            out_fps: HashMap::new(),
            slice_r: None,
            screen_rt: HashMap::new(),
            prefs_tab: PrefsTab::General,
            prefs_dirty: false,
            prefs,
            title: String::new(),
            clock: Clock::new(120.0, std::time::Instant::now()),
            beat: 0,
            phase: 0.0,
            nudge_dir: 0,
            metronome: false,
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
        }
    }

    /// Audio output, opened lazily; a failure is reported once in the status line.
    fn audio_engine(&mut self) -> Option<AudioEngine> {
        if self.audio.is_none() && !self.audio_failed {
            match AudioEngine::start_on(self.prefs.audio.output_device.as_deref()) {
                Ok(e) => self.audio = Some(e),
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
            if ui.selectable_label(&cur == n, n).clicked() {
                ui.close();
                self.apply_layout_named(n);
            }
        }
        ui.separator();
        if ui.button("Save layout as...").clicked() {
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
                if ui.button(&n).clicked() {
                    ui.close();
                    self.delete_layout(&n);
                }
            }
        });
        ui.separator();
        let l = self.prefs.layouts.current.clone();
        let mut new = l.clone();
        for (on, label, key) in [
            (&mut new.browser, "Browser", "Cmd+1"),
            (&mut new.monitors, "Monitors", "Cmd+2"),
            (&mut new.params, "Parameters", "Cmd+3"),
            (&mut new.grid, "Clip grid", "Cmd+4"),
        ] {
            if ui
                .add(egui::Button::selectable(*on, label).shortcut_text(key))
                .clicked()
            {
                *on = !*on;
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
        if ctx.input_mut(|i| i.consume_key(cmd, Key::Slash)) {
            self.set_help(!self.prefs.layouts.help);
        }
    }

    fn save_layout_window(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.save_layout.take() else {
            return;
        };
        let (mut keep, mut save) = (true, false);
        egui::Window::new("Save layout as")
            .collapsible(false)
            .resizable(false)
            .open(&mut keep)
            .show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut name);
                r.request_focus();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui
                    .add_enabled(!name.trim().is_empty(), egui::Button::new("Save"))
                    .clicked()
                    || enter
                {
                    save = true;
                }
            });
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
        let m = egui::Modal::new(egui::Id::new("remove_layer_modal")).show(ctx, |ui| {
            ui.heading("Remove layer?");
            ui.label(format!(
                "\"{name}\" is playing. Remove it with all its clips? You can undo this."
            ));
            ui.horizontal(|ui| {
                yes = ui.button("Yes").clicked();
                no = ui.button("No").clicked();
            });
        });
        if yes {
            self.remove_layer_confirm = None;
            self.remove_layer_now(layer);
        } else if no || m.should_close() {
            self.remove_layer_confirm = None;
        }
    }

    fn overwrite_layout_window(&mut self, ctx: &egui::Context) {
        let Some(name) = self.overwrite_layout.clone() else {
            return;
        };
        let (mut yes, mut no) = (false, false);
        egui::Window::new("Replace layout?")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("Layout \"{name}\" already exists. Replace it?"));
                ui.horizontal(|ui| {
                    yes = ui.button("Replace").clicked();
                    no = ui.button("Cancel").clicked();
                });
            });
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

    fn save_prefs(&mut self) {
        // A new output device takes effect for clips started from now on.
        if self.audio_device != self.prefs.audio.output_device {
            self.audio_device = self.prefs.audio.output_device.clone();
            // Retry only if audio was live (open, or failed on a real error).
            self.metro = None;
            if self.audio.take().is_some() || self.audio_error {
                self.audio_failed = false;
                self.audio_error = false;
            }
        }
        let Some(p) = self.persist.then(prefs_path).flatten() else {
            self.prefs_dirty = false;
            return;
        };
        self.prefs.sanitize();
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
                self.status = format!("Open failed: {e}");
                false
            }
        }
    }

    /// File > Relink missing media: asks for a folder, then applies `relink_from`.
    fn relink_media(&mut self) {
        if vjc_io::media::missing(&self.project).is_empty() {
            self.status = "No missing media".into();
            return;
        }
        let Some(root) = rfd::FileDialog::new()
            .set_title("Folder to search for missing media")
            .pick_folder()
        else {
            return;
        };
        self.relink_from(&root);
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
                if let Some(r) = self.recovery_path() {
                    let _ = std::fs::remove_file(r);
                }
                true
            }
            Err(e) => {
                self.status = format!("Save failed: {e}");
                false
            }
        }
    }

    /// Every `autosave_secs` while dirty (if enabled), writes the recovery file.
    fn autosave(&mut self) {
        let path = self.recovery_path();
        self.autosave_to(path);
    }

    /// `autosave` with an explicit recovery file (`None` = nowhere to write).
    fn autosave_to(&mut self, path: Option<PathBuf>) {
        let every = std::time::Duration::from_secs(self.prefs.general.autosave_secs.into());
        if self.rehearsal
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
            if let Err(e) = self.with_effective_bpm(|p| vjc_io::save(p, &r)) {
                self.status = format!("Autosave failed: {e}");
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
                self.flow = Flow::default();
                self.players.clear();
                self.gens.clear();
                self.file = None;
                self.dirty = true;
            }
            Err(e) => self.status = format!("Import failed: {e}"),
        }
    }

    /// Sends a command through the bus and reacts to the resulting events.
    /// Runs a command; a rejection is reported in the status line.
    fn exec(&mut self, cmd: Command) {
        self.exec_ok(cmd);
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
            Command::TriggerColumn { column, .. } | Command::TriggerClip { column, .. } => {
                Some(*column)
            }
            Command::StepColumn { from, delta, .. } => Some(from.map_or(0, |c| {
                (i64::from(c) + i64::from(*delta)).clamp(0, (ncols - 1).max(0)) as u32
            })),
            _ => None,
        };
        let is_step = matches!(cmd, Command::StepClip { .. });
        let deck_switch = matches!(cmd, Command::SelectDeck { .. });
        let is_set_bpm = matches!(cmd, Command::SetBpm { .. });
        #[cfg(test)]
        let now = self.history_now.unwrap_or_else(std::time::Instant::now);
        #[cfg(not(test))]
        let now = std::time::Instant::now();
        match self.history.exec(&mut self.project, cmd, now) {
            Ok(events) => {
                if is_set_bpm {
                    self.link_sync.note_bpm_edit();
                }
                if col_after.is_some() {
                    self.col_sel = col_after;
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
                }
                if deck_switch {
                    self.col_sel = None;
                    let cur = self.project.composition.current_deck;
                    self.preload_deck(cur);
                }
                self.prune_col_sel();
                if !runtime {
                    self.dirty = true;
                }
                self.handle(events);
                true
            }
            Err(e) => {
                self.status = format!("Command failed: {e}");
                false
            }
        }
    }

    fn undo(&mut self) {
        let before = self.project.composition.tempo.bpm;
        match self.history.undo(&mut self.project) {
            Some(Ok(ev)) => {
                if self.project.composition.tempo.bpm != before {
                    self.link_sync.note_bpm_edit();
                }
                self.dirty = true;
                self.prune_col_sel();
                self.handle(ev);
            }
            Some(Err(e)) => self.status = format!("Undo failed: {e}"),
            None => {}
        }
    }

    fn redo(&mut self) {
        let before = self.project.composition.tempo.bpm;
        match self.history.redo(&mut self.project) {
            Some(Ok(ev)) => {
                if self.project.composition.tempo.bpm != before {
                    self.link_sync.note_bpm_edit();
                }
                self.dirty = true;
                self.prune_col_sel();
                self.handle(ev);
            }
            Some(Err(e)) => self.status = format!("Redo failed: {e}"),
            None => {}
        }
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
        for e in events {
            match e {
                Event::ClipStarted {
                    deck,
                    layer,
                    column,
                    path,
                } => {
                    if path.exists() {
                        let prev_local = self.layer_fraction(layer);
                        self.remember_pos(layer);
                        self.gens.remove(&layer);
                        self.flow.loops.remove(&layer);
                        self.flow.auto_time.remove(&layer);
                        self.flow.beat_loops.remove(&layer);
                        self.cross_from_old(layer);
                        let mut player = self
                            .preload
                            .take(deck, layer, column, &path)
                            .unwrap_or_else(|| Player::open_scrub(&path));
                        if let Some(engine) = self.audio_engine() {
                            player.attach_audio(&engine);
                        }
                        self.players.insert(layer, (deck, column, player));
                        let tr = self.transport(deck, layer, column);
                        let dir = tr.map_or(Direction::Forward, |t| t.direction);
                        let seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(1, |d| d.as_nanos() as u64);
                        self.heads.insert(layer, Playhead::new(dir, seed));
                        self.flow.regions.remove(&layer);
                        let own_last =
                            resume_pos(&self.project, &self.flow.resume, (deck, layer, column));
                        if let Some(start) = vjc_core::trigger_start(
                            &self.project,
                            deck,
                            layer,
                            column,
                            own_last,
                            prev_local,
                        ) {
                            self.seek(layer, start);
                        }
                    } else {
                        self.status = format!("File not found: {}", path.display());
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
                    self.cross_from_old(layer);
                    self.flow.regions.remove(&layer);
                    self.flow.pos.remove(&layer);
                    self.players
                        .insert(layer, (deck, column, Player::open_capture(&device)));
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
                    self.cross_from_old(layer);
                    self.flow.regions.remove(&layer);
                    self.flow.pos.remove(&layer);
                    match browser::net_backend(&protocol) {
                        Some(p) => {
                            self.players
                                .insert(layer, (deck, column, open_network(p, name)));
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
                }
                Event::Seek {
                    deck,
                    layer,
                    column,
                    pos,
                } => {
                    if self.active(layer) == Some((deck, column)) {
                        // A cue jump ends an active beat loop and is not a completed loop.
                        self.flow.beat_loops.remove(&layer);
                        let local = vjc_core::playback::to_local(pos, self.region_of(layer));
                        if let Some(c) = self.flow.loops.get_mut(&layer) {
                            c.rebase(local);
                        }
                        self.seek(layer, pos);
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
                    if self.active(layer) == Some((deck, column)) {
                        self.players.remove(&layer);
                        self.gens.remove(&layer);
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
        let playing = |s: &Self, cell: (LayerId, u32)| {
            active_of(&s.players, &s.gens, cell.0) == Some((deck, cell.1))
        };
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
        if let (Some((d, c)), Some(p)) = (self.active(layer), self.flow.pos.get(&layer))
            && let Some(k) = vjc_core::clip_in(&self.project, d, layer, c)
        {
            self.flow.resume.insert((d, layer, c), (k.id, *p));
        }
    }

    /// Moves the playing clip of `layer` (if any) into `flow.outgoing` when the layer's
    /// transition asks for a crossing; otherwise drops it. Joins the old decode thread
    /// only in the second case.
    fn cross_from_old(&mut self, layer: LayerId) {
        self.flow.outgoing.remove(&layer);
        let old_clip = self
            .active(layer)
            .and_then(|(d, c)| vjc_core::clip_in(&self.project, d, layer, c))
            .cloned();
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
                vjc_core::transition::State::start(&l.transition, self.clock.bpm())
            });
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
        self.players
            .get(&layer)
            .and_then(|(d, c, _)| vjc_core::clip_in(&self.project, *d, layer, *c))
            .map_or((0.0, 1.0), |k| vjc_core::playback::region(&k.transport))
    }

    /// Jumps the layer's playhead and player to a normalised position.
    fn seek(&mut self, layer: LayerId, pos: f64) {
        let reg = self.region_of(layer);
        if let Some(h) = self.heads.get_mut(&layer) {
            h.seek(vjc_core::playback::to_local(pos, reg));
        }
        if let Some((_, _, p)) = self.players.get(&layer) {
            p.set_position(pos);
        }
        self.flow.pos.insert(layer, pos);
    }

    /// Beat loop: repeat `beats` beats from the current position (None clears it).
    fn set_beat_loop(&mut self, layer: LayerId, beats: Option<f64>) {
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

    /// Column the layer's autopilot would play next, if any.
    fn autopilot_target(&self, layer: LayerId) -> Option<(DeckId, u32)> {
        let l = self
            .project
            .composition
            .layers
            .iter()
            .find(|l| l.id == layer)?;
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
        vjc_core::playback::next_clip(l.autopilot.mode, cur, &playable, rand).map(|c| (deck, c))
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
        vjc_core::shortcuts::Ctx::new(self.col_sel, self.master_bypass).with_active(active)
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
        let on = self.project.composition.tempo.link_enabled;
        self.link_sync.update(
            &mut self.link,
            on,
            self.project.composition.tempo.bpm,
            &mut self.clock,
            now,
        );
        // Effective tempo (may come from the Link session, runtime only).
        let bpm = self.clock.bpm();
        let t = self.clock.tick(now);
        self.phase = t.phase;
        if let Some(b) = t.beat {
            self.beat = b;
        }
        if let Some(accent) = vjc_media::audio::beat_click(self.metronome, t.beat) {
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
        for (layer, (deck, col, player)) in &self.players {
            let Some(clip) = vjc_core::clip_in(&self.project, *deck, *layer, *col) else {
                continue;
            };
            let tr = &clip.transport;
            let sync = match (tr.beat_sync, player.duration()) {
                (Some(n), Some(d)) => {
                    let reg = vjc_core::playback::region(tr);
                    beat_sync_rate(d * (reg.1 - reg.0), n, bpm)
                }
                _ => 1.0,
            };
            let rate = vjc_core::playback_rate(tr.speed, sync, speed_of(layer));
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
                            tr.direction,
                            rate,
                            d,
                        );
                        head.seek(t.position);
                        t
                    }
                    (Some(_), None) if tc_on && !tc_free_run => {
                        head.tick(0.0, tr.play_mode, tr.direction, rate, d)
                    }
                    _ => head.tick(dt, tr.play_mode, tr.direction, rate, d),
                };
                // Forward playback streams; everything else needs random access.
                player.set_sequential(!by_tc && is_sequential(tr.play_mode, tr.direction, rate));
                let mut position = vjc_core::playback::to_actual(t.position, reg);
                if let Some((_, w)) = self.flow.beat_loops.get(layer)
                    && !t.finished
                    && let Some(np) = vjc_core::playback::loop_wrap(position, *w)
                {
                    head.seek(vjc_core::playback::to_local(np, reg));
                    position = np;
                }
                player.set_position(position);
                // Audio only at forward 1x; any other speed, direction or pause is muted.
                let audible = rate.is_finite()
                    && !by_tc
                    && (rate - 1.0).abs() < 1e-3
                    && is_sequential(tr.play_mode, tr.direction, rate)
                    && tr.direction != Direction::Pause
                    && !t.finished;
                let (vol, pan) = audio_mix(&self.project, *layer, clip);
                player.set_audio(vol, pan, audible);
                self.flow.pos.insert(*layer, position);
                if t.finished {
                    finished.push(*layer);
                } else if tr.play_mode == PlayMode::Loop
                    && let Some(l) = self
                        .project
                        .composition
                        .layers
                        .iter()
                        .find(|l| l.id == *layer)
                    && l.autopilot.mode != vjc_model::AutopilotMode::Off
                    && l.autopilot.unit == vjc_model::AutopilotUnit::Loops
                {
                    let c = self.flow.loops.entry(*layer).or_default();
                    if c.observe(t.position, tr.direction)
                        && vjc_core::playback::loops_reached(c.count, l.autopilot.every_loops)
                    {
                        autopilot.push(*layer);
                    }
                }
            }
        }
        // Autopilot on generators: they never finish, so a bar of the app clock counts as a loop.
        let bar = (self.clock.phase(std::time::Instant::now()) / 4.0).floor() as i64;
        for layer in self.gens.keys() {
            let Some(l) = self
                .project
                .composition
                .layers
                .iter()
                .find(|l| l.id == *layer)
                .filter(|l| l.autopilot.mode != vjc_model::AutopilotMode::Off)
                .filter(|l| l.autopilot.unit == vjc_model::AutopilotUnit::Loops)
            else {
                continue;
            };
            let st = vjc_core::playback::gen_bars(self.flow.gen_bars.get(layer).copied(), bar);
            self.flow.gen_bars.insert(*layer, st);
            if vjc_core::playback::loops_reached(st.1, l.autopilot.every_loops) {
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
                .map(|l| l.autopilot.clone())
                .filter(|a| {
                    a.mode != vjc_model::AutopilotMode::Off
                        && a.unit != vjc_model::AutopilotUnit::Loops
                })
            else {
                self.flow.auto_time.remove(&layer);
                continue;
            };
            // A paused clip does not run the clock.
            let playing = vjc_core::clip_in(&self.project, deck, layer, col)
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
                vjc_core::clip_in(&self.project, *deck, *layer, *col).map(|k| &k.transport)
            {
                let dir = match tr.direction {
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
                    self.exec(match c.layer {
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
            match self.autopilot_target(layer) {
                Some((deck, column)) => self.exec(Command::TriggerClip {
                    deck,
                    layer,
                    column,
                }),
                None => self.exec(Command::ClearLayer { layer }),
            }
        }
        for layer in autopilot {
            if let Some((deck, column)) = self.autopilot_target(layer) {
                self.exec(Command::TriggerClip {
                    deck,
                    layer,
                    column,
                });
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
                Some(c) => self.drop_file(c.layer, c.col, path),
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
                match (target, vjc_fx::new_effect(&id)) {
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

    fn drop_file(&mut self, layer: LayerId, column: u32, path: PathBuf) {
        let deck = self.project.composition.current_deck;
        // One undo step: the default fit is set on the clip before it is placed.
        let mut clip = vjc_core::clip_from_path(path);
        clip.fit = self.prefs.video.fit;
        self.exec(Command::SetClip {
            deck,
            layer,
            column,
            clip,
        });
        self.exec(Command::TriggerClip {
            deck,
            layer,
            column,
        });
    }

    /// Dashboard dials and audio levels of this frame, owned so no borrow of the app is held.
    fn dials(&self) -> AnimIn {
        let d = &self.project.composition.dashboard;
        AnimIn {
            dials: (0..vjc_core::anim::DIALS)
                .map(|i| d.get(i).map_or(0.0, |p| number_of(&p.value)))
                .collect(),
            bands: self.audio_react.levels,
            smoother: self.audio_react.smoother.clone(),
        }
    }

    /// Animation inputs of this frame.
    fn anim_ctx<'a>(&self, input: &'a AnimIn) -> vjc_core::anim::AnimCtx<'a> {
        vjc_core::anim::AnimCtx {
            beat_phase: self.clock.phase(std::time::Instant::now()),
            time_secs: self.fx_epoch.elapsed().as_secs_f64(),
            fft_bands: &input.bands,
            smoother: Some(&input.smoother),
            dashboard: &input.dials,
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
            vjc_model::Source::File { path } if path.exists() => Some(Player::open_scrub(path)),
            _ => None,
        };
        self.clip_prev = Some(ClipPreview {
            key,
            clip,
            player,
            since: std::time::Instant::now(),
        });
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
        let (w, h) = (c.width.max(1), c.height.max(1));
        let dials = self.dials();
        let actx = self.anim_ctx(&dials);
        if self.comp.is_none() {
            let comp = Compositor::new(&rs.device, w, h);
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
        let out_frames: HashMap<LayerId, Frame> = self
            .flow
            .outgoing
            .iter()
            .filter_map(|(id, o)| Some((*id, o.player.latest_frame()?)))
            .collect();
        // Fit comes from the deck the clip was triggered from, not the visible one.
        let (players, gens) = (&self.players, &self.gens);
        let fit_of = |l: &Layer| {
            let (deck, col) = active_of(players, gens, l.id)?;
            Some(vjc_core::clip_in(&self.project, deck, l.id, col)?.fit)
        };
        // Shaders compile on worker threads; install the ones that finished.
        for done in self.fx.poll() {
            let st = match done.result {
                Ok(wgsl) => match comp.add_effect(&rs.device, &done.kind, &wgsl) {
                    Ok(()) => fx::Status::Ready,
                    Err(e) => fx::Status::Failed(e),
                },
                Err(e) => fx::Status::Failed(e),
            };
            self.fx.set_status(&done.kind, st);
        }
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
        // Group of each entry of `draws` (an outgoing transition source shares its layer's).
        let mut draw_groups: Vec<Option<vjc_model::GroupId>> = vec![];
        for (l, f) in &frames {
            // Video toggle off: the layer is not drawn (its clips keep playing).
            if !l.video {
                continue;
            }
            // Clip chain first, then the layer chain.
            let size = f.as_ref().map_or((w, h), |f| (f.width, f.height));
            let clip = active_of(players, gens, l.id)
                .and_then(|(d, col)| vjc_core::clip_in(&self.project, d, l.id, col));
            let clip_fx = clip.map(|k| k.track.effects.as_slice());
            let generator = match (clip.map(|k| &k.source), gens.get(&l.id)) {
                (Some(vjc_model::Source::Generator { kind, params }), Some((_, _, t)))
                    if !c.layer_off(l) =>
                {
                    self.fx.request(kind);
                    let ready = self.fx.status(kind) == Some(&fx::Status::Ready);
                    self.fx.isf(kind).filter(|_| ready).map(|isf| EffectDraw {
                        kind: kind.clone(),
                        uniforms: isf.pack(
                            &vjc_core::anim::resolve(params, &actx),
                            *t as f32,
                            (w, h),
                        ),
                        dry_wet: 1.0,
                        ml: None,
                        persistent: None,
                    })
                }
                _ => None,
            };
            // Identifies the clip feeding this layer: ML effects reset their state when it changes.
            let src_tag = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                active_of(players, gens, l.id).hash(&mut h);
                h.finish()
            };
            let mut effects = draw_chain(
                &mut self.fx,
                &mut self.ml,
                Some(src_tag),
                clip_fx.unwrap_or_default(),
                time,
                size,
                &actx,
            );
            effects.extend(draw_chain(
                &mut self.fx,
                &mut self.ml,
                Some(src_tag),
                &l.track.effects,
                time,
                size,
                &actx,
            ));
            // Clip transform first, then the layer's (see vjc_core::transform).
            let layer_xf = vjc_core::transform::Affine::inverse_of(
                &vjc_core::transform::Resolved::of(&l.track.transform, &actx),
            );
            let xf = clip.map_or(layer_xf, |k| {
                layer_xf.through(&vjc_core::transform::Affine::inverse_of(
                    &vjc_core::transform::Resolved::of(&k.track.transform, &actx),
                ))
            });
            let cross = self
                .flow
                .outgoing
                .get(&l.id)
                .filter(|_| !c.layer_off(l))
                .and_then(|o| Some((o, o.state.current()?, out_frames.get(&l.id)?)));
            let opacity = (vjc_core::anim::effective_number(&l.track.opacity, &actx)
                * vjc_core::crossfader::layer_gain(
                    c.crossfader.as_ref(),
                    l.crossfader_bus.as_ref(),
                    cf_pos,
                )) as f32;
            let blend = vjc_core::blend::effective_blend(
                &vjc_core::crossfader::layer_blend(
                    c.crossfader.as_ref(),
                    l.crossfader_bus.as_ref(),
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
                    (of.width, of.height),
                    &actx,
                );
                draw_groups.push(l.group);
                draws.push(LayerDraw {
                    id: Uuid::from_u128(l.id.as_u128() ^ OUT_MASK),
                    opacity,
                    blend: blend.clone(),
                    fit: o.fit,
                    effects: fx,
                    generator: None,
                    transition: Some((kind.shader_id(), prog, true)),
                    transform: layer_xf,
                    channels: channels_of(o.clip.as_ref()),
                    frame: Some((of.width, of.height, of.data.as_slice(), of.seq)),
                });
            }
            draw_groups.push(l.group);
            draws.push(LayerDraw {
                id: l.id,
                transition: cross.map(|(_, (k, p), _)| (k.shader_id(), p, false)),
                opacity,
                blend,
                fit: fit_of(l).unwrap_or_default(),
                effects,
                generator,
                transform: xf,
                channels: channels_of(clip),
                frame: f
                    .as_ref()
                    .map(|f| (f.width, f.height, f.data.as_slice(), f.seq)),
            });
        }
        // The previewed clip, drawn alone with its own effects: last in the list, so the
        // compositor keeps it out of the composition.
        if want_preview
            && let PreviewTarget::Clip(d, l, col) = self.preview_target
            && let Some(k) = vjc_core::clip_in(&self.project, d, l, col)
        {
            let t = self
                .clip_prev
                .as_ref()
                .map_or(0.0, |p| p.since.elapsed().as_secs_f32());
            let size = prev_frame.as_ref().map_or((w, h), |f| (f.width, f.height));
            let generator = match &k.source {
                vjc_model::Source::Generator { kind, params } => {
                    self.fx.request(kind);
                    let ready = self.fx.status(kind) == Some(&fx::Status::Ready);
                    self.fx.isf(kind).filter(|_| ready).map(|isf| EffectDraw {
                        kind: kind.clone(),
                        uniforms: isf.pack(&vjc_core::anim::resolve(params, &actx), t, (w, h)),
                        dry_wet: 1.0,
                        ml: None,
                        persistent: None,
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
                &actx,
            );
            let frame = prev_frame
                .as_ref()
                .map(|f| (f.width, f.height, f.data.as_slice(), f.seq));
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
                    transform: vjc_core::transform::Affine::inverse_of(
                        &vjc_core::transform::Resolved::of(&k.track.transform, &actx),
                    ),
                    channels: k.channels,
                    frame,
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
        comp.master_transform = vjc_core::transform::Affine::inverse_of(
            &vjc_core::transform::Resolved::of(&c.master.transform, &actx),
        );
        comp.preview = if want_preview { preview_src } else { None };
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
                match capture::Capture::start(capture::default_path(), size, c.fps) {
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
                            "Saved {} ({} frames, {} dropped, {})",
                            s.path.display(),
                            s.encoded,
                            s.dropped,
                            s.codec
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
                cap.capture(&rs.device, &rs.queue, &comp.target);
            }
        }
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
                    fixture,
                } => Some(artnet_out::Want {
                    id: s.id,
                    ip: ip.clone(),
                    universe: *universe,
                    fixture: fixture.clone(),
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
                    },
                ) {
                    rs.renderer.write().free_texture(&old.id);
                }
            }
            let t = &self.screen_rt[&id];
            // Each slice samples its own source; a routed layer / group has its own texture.
            let source = |sl: &vjc_model::Slice| {
                let view = comp.routed_view(sl.source).unwrap_or(&comp.target_view);
                (view, comp.size)
            };
            r.render(&rs.device, &rs.queue, scr, &source, &t.view);
        }
        // Publish: the screen's own target when it has slices, else the composition.
        let fps = self.project.composition.fps;
        for w in &wants {
            let (tex, size) = match self.screen_rt.get(&w.id) {
                Some(t) => (&t.tex, t.size),
                None => (&comp.target, comp.size),
            };
            self.netout
                .feed(&rs.device, &rs.queue, w.id, tex, size, fps);
        }
        for e in self.netout.poll(&rs.device) {
            self.status = e;
        }
        for w in &art {
            let (tex, size) = match self.screen_rt.get(&w.id) {
                Some(t) => (&t.tex, t.size),
                None => (&comp.target, comp.size),
            };
            self.artnet.feed(&rs.device, &rs.queue, w.id, tex, size);
        }
        for e in self.artnet.poll(&rs.device) {
            self.status = e;
        }
    }

    /// Master row: bypass, Clear All (opacity and speed live in the Composition tab).
    /// Key shortcuts from the project, plus the learn flow. Nothing fires while a widget
    /// has keyboard focus (text field or a focused button), so Space never also presses it.
    fn shortcut_keys(&mut self, ctx: &egui::Context) {
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
            let cmds: Vec<Command> = self
                .project
                .shortcuts
                .iter()
                .filter(|s| vjc_core::shortcuts::key_of(s) == Some(name))
                .filter_map(|s| vjc_core::shortcuts::activate(&self.project, &rt, s))
                .collect();
            for c in cmds {
                self.exec(c);
            }
        }
    }

    /// MIDI, once per frame and never blocking: incoming messages learn or fire shortcuts,
    /// then LEDs of mapped notes are updated where the state changed.
    fn midi_pump(&mut self) {
        use vjc_core::midi as m;
        while let Ok(ev) = self.midi.rx.try_recv() {
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
            let rt = self.rt_ctx();
            let cmds: Vec<Command> = self
                .project
                .shortcuts
                .iter()
                .filter_map(|s| m::command_for(&self.project, &rt, s, &device, msg))
                .collect();
            for c in cmds {
                self.exec(c);
            }
        }
        let rt = self.rt_ctx();
        let active = |l| active_of(&self.players, &self.gens, l);
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
        while let Ok(ev) = self.osc.rx.try_recv() {
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
            let rt = self.rt_ctx();
            let mut cmds: Vec<Command> = o::direct(&self.project, &rt, &ev).into_iter().collect();
            cmds.extend(
                self.project
                    .shortcuts
                    .iter()
                    .filter_map(|s| o::command_for(&self.project, &rt, s, &ev)),
            );
            for c in cmds {
                self.exec(c);
            }
        }
        if !self.prefs.osc.out_enabled {
            self.osc_prev.clear();
            return;
        }
        let now = std::time::Instant::now();
        let rt = self.rt_ctx();
        let active = |l| active_of(&self.players, &self.gens, l);
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
        while let Ok(ev) = self.dmx.rx.try_recv() {
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

    /// Remote API, once per frame: applies the config, then answers the queued requests
    /// (the server threads wait for the reply, the render loop never waits for them).
    fn api_pump(&mut self, ctx: &egui::Context) {
        self.api.set_ctx(ctx);
        let cfg = api_io::Cfg::from_prefs(&self.prefs.api);
        if self.api_cfg.as_ref() != Some(&cfg) {
            self.api.configure(cfg.clone());
            self.api_cfg = Some(cfg);
        }
        for _ in 0..api_io::MAX_PER_FRAME {
            let Ok(msg) = self.api.rx.try_recv() else {
                break;
            };
            let rt = self.rt_ctx();
            let active = |l| active_of(&self.players, &self.gens, l);
            let (cmd, reply) = api_io::answer(&self.project, &rt, &active, msg.req);
            if let Some(c) = cmd {
                self.exec(c);
            }
            let _ = msg.reply.send(reply);
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
                egui::ScrollArea::horizontal()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = theme::SP_2;
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
                let active = self.rt_ctx().active;
                let scoped = layer.filter(|l| active.iter().any(|a| a.0 == *l));
                if let Some(c) =
                    vjc_core::shortcuts::toggle_pause(&self.project, &active, scoped)
                {
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
                cmds.push(Command::Tap { at: now });
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
            help(
                icons::toggle(ui, &mut self.metronome, icons::i::METRONOME, "Metronome"),
                "metronome",
            );
            ui.separator();
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
        self.help_shown = help::take_hovered(&ctx);
        if self.thumbs.poll(&ctx) || self.thumbs.is_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
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
                if ui.button("Save As...").clicked() {
                    ui.close();
                    self.save_as();
                }
                ui.separator();
                if ui.button("Relink missing media...").clicked() {
                    ui.close();
                    self.relink_media();
                }
                if ui.button("Collect media...").clicked() {
                    ui.close();
                    self.collect_media();
                }
                ui.separator();
                if ui.button("Import .avc...").clicked() {
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
                if ui
                    .add_enabled(sel.is_some(), egui::Button::new("Remove layer"))
                    .clicked()
                    && let Some(l) = sel
                {
                    ui.close();
                    self.request_remove_layer(l);
                }
            })
            .response
            // The panel tab is also called "Layer".
            .widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Layer menu")
            });
            ui.menu_button("View", |ui| self.view_menu(ui));
            ui.menu_button("Shortcuts", |ui| {
                for (midi, label) in [(false, "Edit keyboard"), (true, "Edit MIDI")] {
                    let on = self.learn && self.learn_midi == midi;
                    if ui.selectable_label(on, label).clicked() {
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
                    if ui
                        .selectable_label(on, format!("Fullscreen on {}", m.label()))
                        .clicked()
                    {
                        ui.close();
                        self.choose_fullscreen(m.id);
                    }
                }
                if ui.button("Setup...").clicked() {
                    ui.close();
                    self.output_setup_open = true;
                }
                ui.menu_button("Show test card", |ui| {
                    if self.project.output.screens.is_empty() {
                        ui.label("No screens");
                    }
                    for s in self.project.output.screens.clone() {
                        let mut on = self.test_cards.contains(&s.id);
                        if ui.checkbox(&mut on, &s.name).changed() {
                            if on {
                                self.test_cards.insert(s.id);
                            } else {
                                self.test_cards.remove(&s.id);
                            }
                        }
                    }
                });
                if ui.button("Identify displays").clicked() {
                    ui.close();
                    self.identify_at = Some(std::time::Instant::now());
                    // Repaint once more when it expires, even if nothing else animates.
                    ui.ctx().request_repaint_after(
                        output_overlay::IDENTIFY_FOR + std::time::Duration::from_millis(50),
                    );
                }
                ui.checkbox(&mut self.show_fps, "Show FPS");
                if ui.button("Display info").clicked() {
                    ui.close();
                    self.display_info_open = true;
                }
                if ui
                    .add_enabled(
                        self.output.is_some() || !self.open_screens().is_empty(),
                        egui::Button::new("Close output"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.output = None;
                    self.closed_screens
                        .extend(self.project.output.screens.iter().map(|s| s.id));
                }
            });
        });
        let fxs = browser_fx();
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
                    ui.label(&self.status);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(local_clock())
                                .monospace()
                                .color(theme::CLOCK_TEXT),
                        );
                        if self.frame_ms.len() > 0 {
                            ui.separator();
                            ui.weak(self.frame_ms.label())
                                .on_hover_text("Frame time (CPU, last 240 frames)");
                        }
                    });
                });
            });
        if self.persist {
            // Live app only: keeps the clock ticking while idle.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
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
                Some((l, c)) => self.drop_file(l, c, path),
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
        self.targets.clear();
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
        self.save_layout_window(&ctx);
        self.overwrite_layout_window(&ctx);
        self.remove_layer_modal(&ctx);
        self.output_setup_window(&ctx);
        self.display_info_window(&ctx);
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty && !self.confirm_discard() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
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
        for path in dropped {
            match resolve_drop(&self.cells, self.last_pointer, self.selected) {
                Some((l, c, w)) => {
                    let how = match w {
                        DropWhere::Pointer => "under pointer",
                        DropWhere::Selected => "selected cell (no pointer position)",
                        DropWhere::FirstEmpty => "first empty cell of bottom layer",
                        DropWhere::FirstCell => "first cell of bottom layer",
                    };
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.drop_file(l, c, path);
                    self.status = format!("Dropped {name} on column {} ({how})", c + 1);
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
        for (_, _, p) in self.players.values() {
            if let Some(e) = p.error() {
                ui.colored_label(theme::ERROR_TEXT, format!("{}: {e}", p.path.display()));
            }
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
                                    layers: comp.layers.len(),
                                    columns: ncols as usize,
                                },
                                &mut cmds,
                                &mut self.targets,
                            );
                        });
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
                                    let r = ui.interact(
                                        top,
                                        egui::Id::new(("colhead-trigger", cid)),
                                        egui::Sense::click(),
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
                                        self.col_sel == Some(ci as u32),
                                        &hover,
                                    );
                                    let r = help(r, "column");
                                    let nr = help(nr, "column");
                                    let stop_rect = egui::Rect::from_min_size(
                                        rect.right_top() + egui::vec2(-22.0, 2.0),
                                        egui::vec2(20.0, theme::COLUMN_HEAD_H - 4.0),
                                    );
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
                                self.col_sel = Some(ci as u32);
                            }
                            nr.context_menu(|ui| {
                                if ui.button("Rename column").clicked() {
                                    self.col_renaming = Some((*cid, n.clone()));
                                    ui.close();
                                }
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
                                if ui.button("Add column").clicked() {
                                    cmds.push(Command::AddColumn {
                                        column: vjc_model::Column {
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
                            ui.scope(|ui| {
                                ui.spacing_mut().slider_width = theme::LAYER_ROW_H - 16.0;
                                ui.spacing_mut().slider_rail_height = 4.0;
                                let layer = &comp.layers[li];
                                let (hr, _) = ui.allocate_exact_size(
                                    egui::vec2(theme::LAYER_HEAD_W, theme::LAYER_ROW_H),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(hr, 0.0, theme::HEADER_BG);
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
                                    theme::lh::NAME_W,
                                    theme::lh::BUS_H,
                                );
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
                                    self.selected = Some((lid, self.selected.map_or(0, |s| s.1)));
                                    self.selected_deck = Some(cur);
                                    self.preview_target = PreviewTarget::Layer(lid);
                                }
                                name.context_menu(|ui| {
                                    if ui.button("Remove layer").clicked() {
                                        remove_layer_req = Some(lid);
                                        ui.close();
                                    }
                                    ui.separator();
                                    let from = active_of(&self.players, &self.gens, lid)
                                        .filter(|(d, _)| *d == cur)
                                        .map(|(_, c)| c);
                                    for (label, delta) in [("Previous clip", -1), ("Next clip", 1)]
                                    {
                                        if ui.button(label).clicked() {
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
                                                    if ui
                                                        .selectable_label(
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
                                let on =
                                    active_of(&self.players, &self.gens, lid) == Some((cur, ci));
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
                                    },
                                );
                                let r = help(
                                    cell.all(),
                                    if clip.is_some() { "cell" } else { "cell-empty" },
                                );
                                if clip.is_some() {
                                    r.context_menu(|ui| {
                                        for (txt, act) in [
                                            ("Trigger", CellAct::Trigger),
                                            ("Preview", CellAct::Preview),
                                            ("Cut", CellAct::Cut),
                                            ("Copy", CellAct::Copy),
                                        ] {
                                            if ui.button(txt).clicked() {
                                                acts.push((lid, ci, act));
                                                ui.close();
                                            }
                                        }
                                        ui.add_enabled_ui(has_clipboard, |ui| {
                                            if ui.button("Paste").clicked() {
                                                acts.push((lid, ci, CellAct::Paste));
                                                ui.close();
                                            }
                                        });
                                        if ui.button("Clear").clicked() {
                                            acts.push((lid, ci, CellAct::Clear));
                                            ui.close();
                                        }
                                        ui.menu_button("Rename...", |ui| {
                                            let key = ui.id().with(("rename_clip", lid, ci));
                                            let mut buf: String = ui
                                                .data_mut(|d| d.get_temp(key))
                                                .unwrap_or_else(|| label.to_owned());
                                            let e = ui.add(
                                                egui::TextEdit::singleline(&mut buf)
                                                    .desired_width(140.0),
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
                                            if ui.button("Paste").clicked() {
                                                acts.push((lid, ci, CellAct::Paste));
                                                ui.close();
                                            }
                                        });
                                        if ui.button("Import file...").clicked() {
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
                                if cell.pic.drag_started() && clip.is_some() {
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
                                if cell.pic.clicked() && clip.is_some() {
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
                                    if ui.button("Add column").clicked() {
                                        cmds.push(Command::AddColumn {
                                            column: vjc_model::Column {
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
                    layers: comp.layers.len(),
                    columns: ncols as usize,
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
            self.drop_file(l, c, path);
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
            if resp.dragged() {
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
                        egui::Color32::WHITE,
                    );
                }
                None => {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "No signal",
                        egui::FontId::proportional(12.0),
                        ui.visuals().weak_text_color(),
                    );
                }
            }
            if i == 1 {
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
            let (mut w, mut h, mut fps) = (c.width, c.height, c.fps);
            let mut size_changed = false;
            let size_row = ui.horizontal(|ui| {
                params::row_label(ui, "Size");
                // speed 0: typed only, so a drag does not resize the output every frame.
                size_changed |= ui
                    .add(egui::DragValue::new(&mut w).speed(0.0).range(1..=16384))
                    .changed();
                ui.label("x");
                size_changed |= ui
                    .add(egui::DragValue::new(&mut h).speed(0.0).range(1..=16384))
                    .changed();
                ui.label("fps");
                size_changed |= ui
                    .add(
                        egui::DragValue::new(&mut fps)
                            .speed(0.0)
                            .range(1.0..=240.0)
                            .max_decimals(3),
                    )
                    .changed();
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
        if params::section(ui, "sec-transport", "Transport")
            && let Some(value) =
                params::param_slider(ui, "msp", "Speed", c.master_speed, 1.0, (0.0, 4.0))
        {
            cmds.push(Command::SetMasterSpeed { value });
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
                egui::ComboBox::from_id_salt("cfcurve")
                    .selected_text(cf_curve.label())
                    .show_ui(ui, |ui| {
                        for curve in vjc_core::crossfader::CrossfadeCurve::ALL {
                            if ui
                                .selectable_label(cf_curve == curve, curve.label())
                                .clicked()
                            {
                                cmds.push(Command::SetCrossfaderCurve { curve });
                            }
                        }
                    });
            });
            let behaviours = [
                (vjc_model::CrossfadeBehaviour::Fade, "Fade".to_string()),
                (vjc_model::CrossfadeBehaviour::Cut, "Cut".to_string()),
            ];
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
        if params::section(ui, "sec-comp-audio", "Master audio") {
            self.audio_rows(ui, AudioTarget::Master, &c.master, cmds);
        }
        if params::section(ui, "sec-transform", "Transform") {
            self.transform_rows(ui, EffectTarget::Master, cmds);
        }
        if params::section(ui, "sec-cues", "Cues") {
            self.cue_list(ui, cmds);
        }
        if params::section(ui, "sec-dashboard", "Dashboard") {
            for (i, v) in dials.iter().enumerate() {
                let n = i + 1;
                let r = ui.horizontal(|ui| {
                    params::param_slider(ui, ("dial", i), &format!("Dial {n}"), *v, 0.0, (0.0, 1.0))
                });
                if let Some(value) = r.inner {
                    cmds.push(Command::SetDashboardDial {
                        dial: i as u32,
                        value,
                    });
                }
                self.targets
                    .push((r.response.rect, format!("/comp/dashboard/{n}")));
            }
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
                if ui
                    .add(egui::DragValue::new(&mut col).range(1..=max))
                    .changed()
                {
                    cue.column = col - 1;
                    changed = true;
                }
                let name = |l: Option<LayerId>| {
                    l.and_then(|id| comp.layers.iter().find(|x| x.id == id))
                        .map_or("All layers".to_owned(), |x| x.name.clone())
                };
                egui::ComboBox::from_id_salt(("cuelayer", i))
                    .selected_text(name(cue.layer))
                    .show_ui(ui, |ui| {
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
            if ui.button("Add cue").clicked() {
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
        let dials = self.dials();
        let actx = self.anim_ctx(&dials);
        let Some(lid) = self.selected.map(|s| s.0) else {
            ui.label("Select a layer (click its name or a cell).");
            return;
        };
        let Some(l) = self.project.composition.layers.iter().find(|l| l.id == lid) else {
            return;
        };
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
            let kinds: Vec<(String, String)> = [
                "Cut",
                "Dissolve",
                "ToBlack",
                "PushLeft",
                "PushRight",
                "ZoomIn",
                "WipeEllipse",
            ]
            .iter()
            .map(|k| (k.to_string(), k.to_string()))
            .collect();
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
        }
        if params::section(ui, "sec-layer-transform", "Transform") {
            self.transform_rows(ui, EffectTarget::Layer(lid), cmds);
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
            if let Some(value) =
                params::param_slider(ui, ("gsp", gid), "Speed", g.speed, 1.0, (0.0, 4.0))
            {
                cmds.push(Command::SetGroupSpeed { group: gid, value });
            }
            let (mut solo, mut bypass) = (g.solo, g.bypass);
            ui.horizontal(|ui| {
                if icons::toggle(ui, &mut solo, icons::i::HEADPHONES, "Solo").changed() {
                    cmds.push(Command::SetGroupSolo { group: gid, solo });
                }
                if icons::toggle(ui, &mut bypass, icons::i::POWER, "Bypass").changed() {
                    cmds.push(Command::SetGroupBypass { group: gid, bypass });
                }
                if ui.button("Ungroup").clicked() {
                    cmds.push(Command::Ungroup { group: gid });
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
        match self.players.get(&layer) {
            Some((d, c, _)) if *c == column => *d,
            _ => cur,
        }
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
        let dials = self.dials();
        let actx = self.anim_ctx(&dials);
        let t = &k.transport;
        if let Some(name) = params::text_field(ui, ("kname", layer, column), "Name", &k.name) {
            cmds.push(Command::RenameClip {
                deck,
                layer,
                column,
                name,
            });
        }
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
                let info = self.src_info.get(path);
                if info.is_none() {
                    // The probe runs on a thread; look again shortly.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
                let line = source_info::describe(path, info);
                ui.label(line).on_hover_text(path.display().to_string());
            }
            vjc_model::Source::Generator { kind, params } => {
                ui.strong(format!("Source: generator {}", fx_label(kind)));
                let inputs = self.fx.isf(kind).map(|f| f.inputs.clone());
                if inputs.is_none() {
                    ui.colored_label(egui::Color32::LIGHT_RED, "unknown generator (not rendered)");
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
                    .players
                    .get(&layer)
                    .filter(|(d, c, _)| (*d, *c) == (deck, column))
                {
                    match (p.error(), p.latest_frame()) {
                        (Some(e), _) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, e);
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
            ]);
            if let Some(mode) = params::combo(ui, "pm", "Play mode", &t.play_mode, &modes) {
                cmds.push(Command::SetPlayMode {
                    deck,
                    layer,
                    column,
                    mode,
                });
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
            if let Some(speed) = params::param_slider(ui, "spd", "Speed", t.speed, 1.0, (0.0, 4.0))
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
                    if ui
                        .add_enabled(
                            self.tc.last.is_some(),
                            egui::Button::new("Set from current"),
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
            let playing = self.active(layer) == Some((deck, column));
            let pos = self.flow.pos.get(&layer).copied().unwrap_or(0.0);
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
            ui.horizontal(|ui| {
                ui.label("Cues");
                if ui.add_enabled(playing, egui::Button::new("Set")).clicked() {
                    cmds.push(Command::AddCue {
                        deck,
                        layer,
                        column,
                        pos,
                    });
                }
                for (i, c) in t.cue_points.iter().enumerate() {
                    if ui
                        .add_enabled(playing, egui::Button::new(format!("Jump {}", i + 1)))
                        .clicked()
                    {
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

    /// Position, scale, rotation and anchor sliders of a track's transform; each row is a
    /// mappable target (`/comp/.../transform/...`) and animatable like the other params.
    fn transform_rows(&mut self, ui: &mut egui::Ui, target: EffectTarget, cmds: &mut Vec<Command>) {
        let Some(tf) = vjc_core::effects::track(&self.project, target).map(|t| t.transform.clone())
        else {
            return;
        };
        let dials = self.dials();
        let actx = self.anim_ctx(&dials);
        let (cw, ch) = {
            let c = &self.project.composition;
            (f64::from(c.width), f64::from(c.height))
        };
        let active = match target {
            EffectTarget::Clip { layer, .. } => active_of(&self.players, &self.gens, layer),
            _ => None,
        };
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
                vjc_core::shortcuts::transform_address(&self.project, target, field, active)
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
        let active = match target {
            EffectTarget::Clip { layer, .. } => active_of(&self.players, &self.gens, layer),
            _ => None,
        };
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
            let inputs = self.fx.isf(&e.kind).map(|f| f.inputs.clone());
            let dials = self.dials();
            let actx = self.anim_ctx(&dials);
            let failure = match self.fx.status(&e.kind) {
                Some(fx::Status::Failed(why)) => Some(why.clone()),
                _ => self.ml.problem(&e.kind, e.id),
            };
            let open_id = ui.id().with(("fx-open", e.id));
            let open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(true);
            let mut toggle_open = false;
            let (dw, dw_rect) = egui::Frame::new()
                .fill(theme::EFFECT_BG)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let name = ui.strong(fx_label(&e.kind));
                        name.context_menu(|ui| {
                            if ui
                                .add_enabled(i > 0, egui::Button::new("Move up"))
                                .clicked()
                            {
                                cmds.push(Command::MoveEffect {
                                    target,
                                    effect: e.id,
                                    index: i - 1,
                                });
                                ui.close();
                            }
                            if ui
                                .add_enabled(i + 1 < n, egui::Button::new("Move down"))
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
            for (pi, p) in e.params.iter().enumerate() {
                let Some(kind) = inputs.as_ref().and_then(|v| v.get(pi)).map(|i| &i.kind) else {
                    continue;
                };
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
                    let e = ui.add(egui::TextEdit::singleline(buf).desired_width(90.0));
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
                    if ui.button("Rename").clicked() {
                        self.renaming = Some((*id, name.clone()));
                        ui.close();
                    }
                    if ui
                        .add_enabled(i > 0, egui::Button::new("Move left"))
                        .clicked()
                    {
                        cmds.push(Command::MoveDeck {
                            deck: *id,
                            index: i - 1,
                        });
                        ui.close();
                    }
                    if ui
                        .add_enabled(i + 1 < decks.len(), egui::Button::new("Move right"))
                        .clicked()
                    {
                        cmds.push(Command::MoveDeck {
                            deck: *id,
                            index: i + 1,
                        });
                        ui.close();
                    }
                    if ui.add_enabled(!last, egui::Button::new("Remove")).clicked() {
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
                    let full = ui.max_rect();
                    ui.painter().rect_filled(full, 0.0, egui::Color32::BLACK);
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
                        egui::Color32::WHITE,
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
                                identify: identify.then_some(number),
                                fps: show_fps.then_some(meter.fps),
                            },
                        );
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

impl eframe::App for State {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let t0 = std::time::Instant::now();
        self.poll_monitors();
        self.tick_clock();
        self.audio_react.refresh(&self.project.composition);
        self.compose(frame);
        if let Some(rs) = frame.wgpu_render_state() {
            self.browser
                .fx_thumbs
                .attach(rs.device.clone(), rs.queue.clone(), ui.ctx().clone());
        }
        State::ui(self, ui);
        let ctx = ui.ctx().clone();
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
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(IDLE_REPAINT);
        }
    }

    /// Closing the app mid-recording still writes the mp4 trailer (bounded wait).
    fn on_exit(&mut self) {
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

fn main() -> eframe::Result {
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
            if let Some(w) = warn {
                s.status = w.clone();
                s.warnings.push(w);
            }
            offer_recovery(&mut s);
            if s.file.is_none()
                && !s.dirty
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
            id: Uuid::new_v4(),
            name: "c".into(),
            source: vjc_model::Source::File {
                path: "a.mov".into(),
            },
            transport: Default::default(),
            track: vjc_model::Track::new(vjc_model::BlendMode::Normal),
            blend_override: None,
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
    {
        vol = 0.0;
    }
    let mut pan = num(&clip.track.pan) + num(&p.composition.master.pan);
    if let Some(t) = lt {
        vol *= vnum(&t.volume);
        pan += num(&t.pan);
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
                if ui.button(n).clicked() {
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
        ui.add(
            egui::TextEdit::singleline(&mut name)
                .hint_text("Preset name")
                .desired_width(120.0),
        );
        let ok = ui
            .add_enabled(!name.trim().is_empty(), egui::Button::new("Save"))
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
