mod api_io;
mod browser;
mod capture;
mod compositor;
mod fx;
mod link_io;
mod midi_io;
mod monitors;
mod netout;
mod osc_io;
mod output_ui;
mod params;
mod prefs_ui;
mod slice_render;
mod timecode_io;

use compositor::{Compositor, EffectDraw, LayerDraw};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;
use vjc_core::clock::{Clock, LinkSync, beat_sync_rate};
use vjc_core::effects::EffectTarget;
use vjc_core::playhead::Playhead;
use vjc_core::{Command, Event, History};
use vjc_io::prefs::Prefs;
use vjc_media::audio::AudioEngine;
use vjc_media::{Frame, Player};
use vjc_model::{
    Autopilot, AutopilotMode, BlendMode, Column, Deck, DeckId, Direction, FitMode, Layer, LayerId,
    OutputDevice, PlayMode, Project, Track, Transition, Value,
};

/// GPU target of one output screen with slices.
struct ScreenRt {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    id: egui::TextureId,
}

struct State {
    project: Project,
    history: History,
    warnings: Vec<String>,
    status: String,
    /// Playing clip per layer: (deck it was triggered from, column, player). One clip per
    /// layer. Lookups of the playing clip use this deck, never `current_deck`.
    players: HashMap<LayerId, (DeckId, u32, Player)>,
    /// Audio output, opened on the first clip start (None until then or when unavailable).
    audio: Option<AudioEngine>,
    audio_failed: bool,
    /// Writes prefs and the recovery file (false in UI tests).
    persist: bool,
    /// Playing generator clip per layer: (deck, column, own TIME in seconds). The time
    /// follows the clip transport (speed, direction, pause); a layer has a player or a
    /// generator, never both.
    gens: HashMap<LayerId, (DeckId, u32, f64)>,
    /// Deck tab being renamed, with its edit buffer.
    renaming: Option<(DeckId, String)>,
    /// Playhead per layer, restarted with each trigger (stale entries are harmless).
    heads: HashMap<LayerId, Playhead>,
    last_head_tick: std::time::Instant,
    /// Runtime-only playback flow per layer (transitions, loop counts, beat loops).
    flow: Flow,
    comp: Option<Compositor>,
    fx: fx::FxLib,
    fx_epoch: std::time::Instant,
    /// Tab of the bottom parameter panel.
    panel_tab: PanelTab,
    comp_tex: Option<egui::TextureId>,
    /// Output recording in progress (runtime only).
    capture: Option<capture::Capture>,
    /// Syphon / NDI publishers of the screens set to those devices (runtime only).
    netout: netout::NetOut,
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
    master_rect: Option<egui::Rect>,
    browser: browser::Browser,
    last_pointer: Option<egui::Pos2>,
    /// Cell selected by click; fallback target for drops.
    selected: Option<(LayerId, u32)>,
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
    /// External timecode (MTC / LTC) state.
    tc: timecode_io::TcRuntime,
    /// MIDI input ports and audio input devices listed in Preferences > Timecode.
    tc_devices: Option<(Vec<String>, Vec<String>)>,
    led_prev: HashMap<vjc_core::midi::Led, bool>,
    /// OSC sockets thread, the config last sent to it, and the output state last sent.
    osc: osc_io::Osc,
    osc_cfg: Option<osc_io::Cfg>,
    osc_prev: HashMap<String, f64>,
    osc_limit: vjc_core::osc::RateLimit,
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
    sel_pt_in: Option<usize>,
    sel_pt_out: Option<usize>,
    snap: bool,
    /// Output canvas edits the slice mask polygon instead of the warp mesh.
    edit_mask: bool,
    sel_mask_pt: Option<usize>,
    /// Screens whose output window the user closed (runtime only).
    closed_screens: std::collections::HashSet<Uuid>,
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
}

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
    Api,
    Timecode,
}

#[derive(Clone, Copy, PartialEq)]
enum PanelTab {
    Composition,
    Layer,
    Group,
    Clip,
}

/// Active effects of a chain with this frame's uniforms. Effects still compiling or that
/// failed are skipped.
fn draw_chain(
    lib: &mut fx::FxLib,
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
        if let Some(isf) = lib.isf(&e.kind) {
            out.push(EffectDraw {
                kind: e.kind.clone(),
                uniforms: isf.pack(&vjc_core::anim::resolve(&e.params, ctx), time, size),
                dry_wet: e.dry_wet,
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
    let name = ui.add(
        egui::Button::new(egui::RichText::new(format!("\u{25be} {}", g.name)).strong())
            .selected(*sel_group == Some(gid)),
    );
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
    for _ in 0..ncols {
        ui.label("");
    }
    let (mut bypass, mut solo) = (g.bypass, g.solo);
    ui.horizontal(|ui| {
        let b = ui
            .toggle_value(&mut bypass, "B")
            .on_hover_text("Group bypass");
        if let Some(a) = vjc_core::shortcuts::group_address(project, gid, "bypass") {
            targets.push((b.rect, a));
        }
        if b.changed() {
            cmds.push(Command::SetGroupBypass { group: gid, bypass });
        }
        let s = ui.toggle_value(&mut solo, "S").on_hover_text("Group solo");
        if let Some(a) = vjc_core::shortcuts::group_address(project, gid, "solo") {
            targets.push((s.rect, a));
        }
        if s.changed() {
            cmds.push(Command::SetGroupSolo { group: gid, solo });
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
    if ui.button("Ungroup").clicked() {
        cmds.push(Command::Ungroup { group: gid });
    }
    ui.end_row();
}

fn add_effect_menu(ui: &mut egui::Ui, target: EffectTarget, cmds: &mut Vec<Command>) {
    ui.menu_button("Add effect", |ui| {
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
    });
}

type Gens = HashMap<LayerId, (DeckId, u32, f64)>;

/// Deck and column of the clip playing on a layer (video or generator).
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

/// Effects and generators offered by the browser.
fn browser_fx() -> Vec<(String, browser::Item)> {
    let fx = vjc_fx::BUILTIN
        .iter()
        .map(|(id, _)| (fx_label(id), browser::Item::Effect(id.to_string())));
    let gens = vjc_fx::GENERATORS
        .iter()
        .map(|(id, _)| (fx_label(id), browser::Item::Generator(id.to_string())));
    fx.chain(gens).collect()
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "VJCraft")
}

/// Per-user data dir (macOS: ~/Library/Application Support/VJCraft) + recovery.vjc
fn recovery_path() -> Option<PathBuf> {
    Some(project_dirs()?.data_dir().join("recovery.vjc"))
}

/// Per-user config dir (macOS: ~/Library/Application Support/VJCraft) + prefs.json
fn prefs_path() -> Option<PathBuf> {
    Some(project_dirs()?.config_dir().join("prefs.json"))
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
    for i in (1..=2).rev() {
        c.layers.push(new_layer(i));
    }
    for i in 1..=4 {
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
        self.persist.then(recovery_path).flatten()
    }

    fn build(prefs: Prefs, live: bool) -> Self {
        let project = fresh_project(&prefs);
        State {
            project,
            history: History::new(),
            warnings: vec![],
            status: "Drop video files on the grid cells.".into(),
            players: HashMap::new(),
            audio: None,
            gens: HashMap::new(),
            heads: HashMap::new(),
            flow: Flow::default(),
            last_head_tick: std::time::Instant::now(),
            comp: None,
            fx: fx::FxLib::new(),
            fx_epoch: std::time::Instant::now(),
            panel_tab: PanelTab::Composition,
            comp_tex: None,
            capture: None,
            netout: Default::default(),
            rec_request: None,
            output: None,
            monitors: if live { monitors::list() } else { vec![] },
            monitors_polled: (!live).then(std::time::Instant::now),
            persist: live,
            audio_failed: !live,
            cells: vec![],
            layer_rects: vec![],
            master_rect: None,
            browser: browser::Browser::new(browser_start(None, &prefs.general)),
            last_pointer: None,
            selected: None,
            sel_group: None,
            col_sel: None,
            learn: false,
            learn_target: None,
            learn_midi: false,
            tc: timecode_io::TcRuntime::new(),
            tc_devices: None,
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
            sel_pt_in: None,
            sel_pt_out: None,
            snap: false,
            edit_mask: false,
            sel_mask_pt: None,
            closed_screens: Default::default(),
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
            renaming: None,
        }
    }

    /// Audio output, opened lazily; a failure is reported once in the status line.
    fn audio_engine(&mut self) -> Option<AudioEngine> {
        if self.audio.is_none() && !self.audio_failed {
            match AudioEngine::start() {
                Ok(e) => self.audio = Some(e),
                Err(e) => {
                    self.audio_failed = true;
                    self.status = format!("Audio off: {e}");
                }
            }
        }
        self.audio.clone()
    }

    fn reset_players(&mut self) {
        self.flow = Flow::default();
        self.players.clear();
        self.gens.clear();
        self.history.clear();
        self.selected = None;
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
        self.link_sync.note_bpm_edit();
        self.file = None;
        self.dirty = false;
        self.warnings.clear();
        self.reset_players();
        self.status = "New project".into();
    }

    fn save_prefs(&mut self) {
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
                self.link_sync.note_bpm_edit();
                self.warnings = warnings;
                self.status = format!("Opened {}", path.display());
                self.prefs.general.last_project = Some(path.clone());
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

    /// Writes the effective tempo (possibly from Link) into the project before saving.
    fn store_effective_bpm(&mut self) {
        let bpm = self.clock.bpm();
        self.project.composition.tempo.bpm = bpm;
    }

    fn save_to(&mut self, path: PathBuf) -> bool {
        self.store_effective_bpm();
        match vjc_io::save(&self.project, &path) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.prefs.general.last_project = Some(path.clone());
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
        let every = std::time::Duration::from_secs(self.prefs.general.autosave_secs.into());
        if !self.prefs.general.autosave || !self.dirty || self.last_autosave.elapsed() < every {
            return;
        }
        self.last_autosave = std::time::Instant::now();
        if let Some(r) = self.recovery_path() {
            if let Some(d) = r.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            self.store_effective_bpm();
            if let Err(e) = vjc_io::save(&self.project, &r) {
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
                self.link_sync.note_bpm_edit();
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
    fn exec(&mut self, cmd: Command) {
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
        let is_set_bpm = matches!(cmd, Command::SetBpm { .. });
        match self
            .history
            .exec(&mut self.project, cmd, std::time::Instant::now())
        {
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
                if !runtime {
                    self.dirty = true;
                }
                self.handle(events);
            }
            Err(e) => self.status = format!("Command failed: {e}"),
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
                self.handle(ev);
            }
            Some(Err(e)) => self.status = format!("Redo failed: {e}"),
            None => {}
        }
    }

    fn handle(&mut self, events: Vec<Event>) {
        for e in events {
            match e {
                Event::ClipStarted {
                    deck,
                    layer,
                    column,
                    path,
                } => {
                    if path.exists() {
                        self.gens.remove(&layer);
                        self.flow.loops.remove(&layer);
                        self.flow.beat_loops.remove(&layer);
                        self.cross_from_old(layer);
                        let mut player = Player::open_scrub(&path);
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
                    self.flow.beat_loops.remove(&layer);
                    self.cross_from_old(layer);
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
                    self.flow.beat_loops.remove(&layer);
                    self.cross_from_old(layer);
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
                    self.flow.beat_loops.remove(&layer);
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
                        if let Some(c) = self.flow.loops.get_mut(&layer) {
                            c.rebase(pos);
                        }
                        self.seek(layer, pos);
                    }
                }
                Event::LayerCleared { layer } => {
                    self.flow.outgoing.remove(&layer);
                    self.flow.beat_loops.remove(&layer);
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
                Event::DeckRemoved { deck } => {
                    self.flow.outgoing.clear();
                    self.players.retain(|_, (d, _, _)| *d != deck);
                    self.gens.retain(|_, (d, _, _)| *d != deck);
                }
            }
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

    /// Jumps the layer's playhead and player to a normalised position.
    fn seek(&mut self, layer: LayerId, pos: f64) {
        if let Some(h) = self.heads.get_mut(&layer) {
            h.seek(pos);
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
                (Some(n), Some(d)) => beat_sync_rate(d, n, bpm),
                _ => 1.0,
            };
            let rate = vjc_core::playback_rate(tr.speed, sync, speed_of(layer));
            // Play modes: the playhead picks the position, the player serves the frame.
            if let (Some(d), Some(head)) = (player.duration(), self.heads.get_mut(layer)) {
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
                let mut position = t.position;
                if let Some((_, w)) = self.flow.beat_loops.get(layer)
                    && !t.finished
                    && let Some(np) = vjc_core::playback::loop_wrap(position, *w)
                {
                    head.seek(np);
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
                {
                    let c = self.flow.loops.entry(*layer).or_default();
                    if c.observe(position, tr.direction)
                        && vjc_core::playback::loops_reached(c.count, l.autopilot.every_loops)
                    {
                        autopilot.push(*layer);
                    }
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

    /// Dashboard dial values 0..1 (missing dials read 0).
    fn dials(&self) -> Vec<f64> {
        let d = &self.project.composition.dashboard;
        (0..vjc_core::anim::DIALS)
            .map(|i| d.get(i).map_or(0.0, |p| number_of(&p.value)))
            .collect()
    }

    /// Animation inputs of this frame. TODO(#24): no FFT bands until audio input exists.
    fn anim_ctx<'a>(&self, dials: &'a [f64]) -> vjc_core::anim::AnimCtx<'a> {
        vjc_core::anim::AnimCtx {
            beat_phase: self.clock.phase(std::time::Instant::now()),
            time_secs: self.fx_epoch.elapsed().as_secs_f64(),
            fft_bands: &[],
            dashboard: dials,
        }
    }

    fn compose(&mut self, frame: &eframe::Frame) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
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
                    })
                }
                _ => None,
            };
            let mut effects =
                draw_chain(&mut self.fx, clip_fx.unwrap_or_default(), time, size, &actx);
            effects.extend(draw_chain(
                &mut self.fx,
                &l.track.effects,
                time,
                size,
                &actx,
            ));
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
            if let Some((o, (kind, prog), of)) = cross {
                let fx = draw_chain(
                    &mut self.fx,
                    &l.track.effects,
                    time,
                    (of.width, of.height),
                    &actx,
                );
                draw_groups.push(l.group);
                draws.push(LayerDraw {
                    id: Uuid::from_u128(l.id.as_u128() ^ OUT_MASK),
                    opacity,
                    blend: l.track.blend.clone(),
                    fit: o.fit,
                    effects: fx,
                    generator: None,
                    transition: Some((kind.shader_id(), prog, true)),
                    frame: Some((of.width, of.height, of.data.as_slice(), of.seq)),
                });
            }
            draw_groups.push(l.group);
            draws.push(LayerDraw {
                id: l.id,
                transition: cross.map(|(_, (k, p), _)| (k.shader_id(), p, false)),
                opacity,
                blend: l.track.blend.clone(),
                fit: fit_of(l).unwrap_or_default(),
                effects,
                generator,
                frame: f
                    .as_ref()
                    .map(|f| (f.width, f.height, f.data.as_slice(), f.seq)),
            });
        }
        let master = draw_chain(&mut self.fx, &c.master.effects, time, (w, h), &actx);
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
                    start: i,
                    len,
                    opacity: vjc_core::anim::effective_number(&g.track.opacity, &actx) as f32,
                    blend: g.track.blend.clone(),
                    effects: draw_chain(&mut self.fx, &g.track.effects, time, (w, h), &actx),
                });
            }
            i += len;
        }
        comp.render_groups(
            &rs.device,
            &rs.queue,
            &draws,
            &group_draws,
            &master,
            master_opacity,
        );
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

    /// Renders the warped slices of each open screen into its own texture, and publishes
    /// the Syphon / NDI screens.
    fn render_screens(&mut self, rs: &eframe::egui_wgpu::RenderState) {
        let wants = self.net_wants();
        self.netout.sync(&wants);
        let mut open: Vec<Uuid> = self.open_screens().into_iter().map(|(id, _)| id).collect();
        open.extend(wants.iter().map(|w| w.id));
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
            if scr.slices.is_empty() {
                if let Some(old) = self.screen_rt.remove(&id) {
                    rs.renderer.write().free_texture(&old.id);
                }
                continue;
            }
            let size = (scr.width.max(1), scr.height.max(1));
            let r = self
                .slice_r
                .get_or_insert_with(|| slice_render::SliceRenderer::new(&rs.device));
            let fresh = self.screen_rt.get(&id).is_none_or(|t| t.size != size);
            if fresh {
                let (tex, view) = slice_render::make_screen_target(&rs.device, size.0, size.1);
                let tid = rs.renderer.write().register_native_texture(
                    &rs.device,
                    &view,
                    wgpu::FilterMode::Linear,
                );
                if let Some(old) = self.screen_rt.insert(
                    id,
                    ScreenRt {
                        tex,
                        view,
                        size,
                        id: tid,
                    },
                ) {
                    rs.renderer.write().free_texture(&old.id);
                }
            }
            let t = &self.screen_rt[&id];
            r.render(&rs.device, &rs.queue, scr, &comp.target_view, &t.view);
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
    }

    /// Master row: bypass, Clear All (opacity and speed live in the Composition tab).
    fn master_bar(&mut self, ui: &mut egui::Ui) {
        let mut cmds = vec![];
        let bar = ui.horizontal(|ui| {
            ui.label("Master");
            let mut bypass = self.master_bypass;
            let by = ui.toggle_value(&mut bypass, "Bypass");
            self.targets.push((by.rect, "/comp/master/bypass".into()));
            if by.changed() {
                cmds.push(Command::SetMasterBypass { bypass });
            }
            if ui.button("Clear All").clicked() {
                cmds.push(Command::ClearAll);
            }
            let recording = self.capture.is_some();
            let label = if recording { "Stop rec" } else { "Record" };
            if ui.selectable_label(recording, label).clicked() {
                cmds.push(if recording {
                    Command::StopRecording
                } else {
                    Command::StartRecording
                });
            }
            if let Some(cap) = &self.capture {
                ui.label(format!("{} dropped", cap.dropped()));
            }
            ui.separator();
            let dials = self.dials();
            let actx = self.anim_ctx(&dials);
            let mut pos = self
                .project
                .composition
                .crossfader
                .as_ref()
                .map_or(vjc_core::crossfader::CENTER, |x| {
                    vjc_core::anim::effective_number(&x.position, &actx)
                });
            ui.label("A");
            let cf = ui.add(
                egui::Slider::new(&mut pos, 0.0..=1.0)
                    .show_value(false)
                    .smallest_positive(0.001),
            );
            ui.label("B");
            self.targets.push((cf.rect, "/comp/crossfader".into()));
            if cf.changed() {
                cmds.push(Command::SetCrossfader { value: pos });
            }
        });
        self.master_rect = Some(bar.response.rect);
        if self.learn {
            cmds.clear();
        }
        for c in cmds {
            self.exec(c);
        }
    }

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
                midi_io::In::Connected(d) => {
                    self.led_prev.retain(|(dev, _, _), _| *dev != d);
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
                    self.status = format!("{device} -> {target}");
                    for c in m::learn(&self.project, &target, src) {
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
                .filter_map(|s| m::command_for(&self.project, &rt, s, &device, msg))
                .collect();
            for c in cmds {
                self.exec(c);
            }
        }
        let rt = self.rt_ctx();
        let active = |l| active_of(&self.players, &self.gens, l);
        let next = m::led_states(&self.project, &rt, &active);
        for ((device, ch, note), on) in m::feedback_diff(&self.led_prev, &next) {
            self.midi.send(&device, m::led_bytes(ch, note, on));
        }
        self.led_prev = next;
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
                    self.status = format!("{} -> {target}", ev.address);
                    for c in o::learn(&self.project, &target, o::source_of(&ev)) {
                        self.exec(c);
                    }
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
    fn unlearn_midi_osc(&mut self) {
        let Some(target) = self.learn_target.clone() else {
            return;
        };
        self.status = format!("Unmapped {target}");
        let mut cmds = vjc_core::midi::unlearn(&self.project, &target);
        cmds.extend(vjc_core::osc::unlearn(&self.project, &target));
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
                format!("{a}: move a MIDI or OSC control (Delete unmaps, Esc leaves)")
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
        ui.horizontal(|ui| {
            ui.label("BPM");
            let mut bpm = self.clock.bpm();
            if ui
                .add(
                    egui::DragValue::new(&mut bpm)
                        .range(vjc_core::clock::MIN_BPM..=vjc_core::clock::MAX_BPM)
                        .speed(0.1),
                )
                .changed()
            {
                cmds.push(Command::SetBpm { bpm });
            }
            let tap = ui.button("Tap");
            self.targets.push((tap.rect, "/comp/tempo/tap".into()));
            if tap.clicked() {
                cmds.push(Command::Tap { at: now });
            }
            let minus = ui.button("\u{2212}").on_hover_text("Nudge slower (hold)");
            let plus = ui.button("+").on_hover_text("Nudge faster (hold)");
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
            if ui.button("Resync").clicked() {
                cmds.push(Command::Resync { at: now });
            }
            let bpm = self.clock.bpm();
            if ui.button("\u{00f7}2").clicked() {
                cmds.push(Command::SetBpm {
                    bpm: (bpm / 2.0).max(vjc_core::clock::MIN_BPM),
                });
            }
            if ui.button("\u{00d7}2").clicked() {
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
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::LIGHT_BLUE
                    };
                    base.gamma_multiply(0.35 + 0.65 * fade)
                } else {
                    ui.visuals().widgets.inactive.bg_fill
                };
                ui.painter().circle_filled(rect.center(), 6.0, col);
            }
            ui.toggle_value(&mut self.metronome, "Metronome");
            let mut link = self.project.composition.tempo.link_enabled;
            let label = if link {
                format!("Link ({})", self.link_sync.peers)
            } else {
                "Link".to_owned()
            };
            if ui.toggle_value(&mut link, label).changed() {
                cmds.push(Command::SetLink { enabled: link });
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
                ui.label(
                    egui::RichText::new(format!("TC {text}"))
                        .monospace()
                        .color(color),
                )
                .on_hover_text(if cur.is_some() {
                    "External timecode"
                } else {
                    "No timecode signal"
                });
            }
        });
        if self.learn {
            cmds.clear();
        }
        for c in cmds {
            self.exec(c);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
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
                            (true, true) => "Click a cell, column, opacity, Tap or Bypass, then move a MIDI or OSC control (Delete unmaps, Esc leaves)".into(),
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
                        if let Some(first) = self.project.output.screens.first().map(|s| s.id) {
                            self.closed_screens.remove(&first);
                            self.exec(Command::SetScreenDevice {
                                screen: first,
                                device: OutputDevice::Monitor {
                                    name: m.id.to_string(),
                                },
                            });
                        } else {
                            self.output = Some(m.id);
                        }
                    }
                }
                if ui.button("Setup...").clicked() {
                    ui.close();
                    self.output_setup_open = true;
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
        egui::Panel::right("browser")
            .default_size(260.0)
            .show(ui, |ui| opened = self.browser.show(ui, &fxs));
        let mut pcmds = vec![];
        // Heights are relative to the window so the grid always keeps room.
        let win_h = ui.max_rect().height();
        egui::Panel::bottom("params")
            .default_size((win_h * 0.25).min(220.0))
            .resizable(true)
            .size_range(40.0..=(win_h * 0.3).max(40.0))
            .show(ui, |ui| self.params_panel(ui, &mut pcmds));
        for c in pcmds {
            self.exec(c);
        }
        if let Some(path) = opened {
            match self.selected {
                Some((l, c)) => self.drop_file(l, c, path),
                None => self.status = "Select a cell to load the file".into(),
            }
        }
        if ctx.input(|i| i.pointer.any_released())
            && let Some(item) = egui::DragAndDrop::payload::<browser::Item>(&ctx)
            && let Some(pos) = ctx.pointer_latest_pos()
        {
            self.browser_drop((*item).clone(), pos);
        }
        self.layer_rects.clear();
        self.targets.clear();
        egui::Panel::top("topbars").show(ui, |ui| {
            self.time_bar(ui);
            self.master_bar(ui);
        });
        self.shortcut_keys(&ctx);
        self.midi_pump();
        self.osc_pump();
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
        self.output_setup_window(&ctx);
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
        let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let drag_target = if dragging {
            resolve_drop(&self.cells, self.last_pointer, self.selected).map(|(l, c, _)| (l, c))
        } else {
            None
        };
        let mut pick: Option<(LayerId, u32)> = None;

        ui.label(&self.status);
        if !self.warnings.is_empty() {
            ui.collapsing(format!("{} import warnings", self.warnings.len()), |ui| {
                for w in &self.warnings {
                    ui.label(w);
                }
            });
        }
        for (_, _, p) in self.players.values() {
            if let Some(e) = p.error() {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    format!("{}: {e}", p.path.display()),
                );
            }
        }
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("Add layer").clicked() {
                let n = self.project.composition.layers.len() + 1;
                self.exec(Command::AddLayer {
                    layer: new_layer(n),
                    index: 0,
                });
            }
            if ui.button("Add column").clicked() {
                let n = self.project.composition.columns.len() + 1;
                self.exec(Command::AddColumn {
                    column: Column {
                        id: Uuid::new_v4(),
                        name: n.to_string(),
                    },
                });
            }
        });

        // Monitors: preview and output side by side, each letterboxed to the comp aspect.
        // Keep >= 80 px for the grid (+ deck tabs); monitors take only what is left.
        let mon_max = (ui.available_height() - 80.0 - 40.0)
            .min(win_h * 0.35)
            .min(700.0);
        if mon_max < 40.0 {
            // Too short for monitors: collapse them entirely.
        } else if let Some(id) = self.comp_tex {
            egui::Panel::top("monitors")
                .resizable(true)
                .default_size(220.0_f32.min(mon_max))
                .size_range(40.0..=mon_max)
                .show(ui, |ui| {
                    let c = &self.project.composition;
                    let aspect = c.width as f32 / c.height.max(1) as f32;
                    let avail = ui.available_size();
                    let label_h = ui.text_style_height(&egui::TextStyle::Body);
                    let w = ((avail.x - ui.spacing().item_spacing.x) / 2.0).max(0.0);
                    ui.horizontal(|ui| {
                        for name in ["Preview", "Output"] {
                            ui.vertical(|ui| {
                                ui.set_width(w);
                                ui.label(name);
                                let h = (avail.y - label_h - ui.spacing().item_spacing.y).max(0.0);
                                let size = egui::vec2(w, h);
                                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                                ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                                let mut fit = egui::vec2(size.x, size.x / aspect);
                                if fit.y > size.y {
                                    fit = egui::vec2(size.y * aspect, size.y);
                                }
                                ui.painter().image(
                                    id,
                                    egui::Rect::from_center_size(rect.center(), fit),
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );
                            });
                        }
                    });
                });
        } else {
            egui::Panel::top("monitors")
                .resizable(false)
                .show(ui, |ui| {
                    ui.weak("No signal");
                });
        }

        let mut cmds: Vec<Command> = vec![];
        self.deck_tabs(ui, &mut cmds);
        let mut cells = vec![];
        let ncols = self.project.composition.columns.len() as u32;
        let cur = self.project.composition.current_deck;
        let names: Vec<String> = self
            .project
            .composition
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let deck_idx = self
            .project
            .composition
            .decks
            .iter()
            .position(|d| d.id == cur);
        let comp = &self.project.composition;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("grid").spacing([4.0, 4.0]).show(ui, |ui| {
                    ui.label("");
                    ui.label("Opacity");
                    for (ci, n) in names.iter().enumerate() {
                        let b = egui::Button::new(format!("\u{25b6} {n}"))
                            .selected(self.col_sel == Some(ci as u32));
                        let r = ui.add(b);
                        self.targets
                            .push((r.rect, vjc_core::shortcuts::column_address(ci as u32)));
                        if r.clicked() {
                            cmds.push(Command::TriggerColumn {
                                deck: cur,
                                column: ci as u32,
                            });
                        }
                    }
                    ui.label("A/B");
                    ui.label("Blend");
                    ui.label("");
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
                        let name = ui.add(
                            egui::Label::new(if in_group {
                                format!("  \u{2502} {}", comp.layers[li].name)
                            } else {
                                comp.layers[li].name.clone()
                            })
                            .sense(egui::Sense::click()),
                        );
                        self.layer_rects.push((name.rect, lid));
                        if name.clicked() || name.secondary_clicked() {
                            self.selected = Some((lid, self.selected.map_or(0, |s| s.1)));
                        }
                        name.context_menu(|ui| {
                            let from = active_of(&self.players, &self.gens, lid)
                                .filter(|(d, _)| *d == cur)
                                .map(|(_, c)| c);
                            for (label, delta) in [("Previous clip", -1), ("Next clip", 1)] {
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
                        let layer = &comp.layers[li];
                        let mut op = match layer.track.opacity.value {
                            vjc_model::Value::Number(n) => n as f32,
                            _ => 1.0,
                        };
                        let op_r = ui.add(egui::Slider::new(&mut op, 0.0..=1.0).fixed_decimals(2));
                        if let Some(a) =
                            vjc_core::shortcuts::layer_address(&self.project, lid, "opacity")
                        {
                            self.targets.push((op_r.rect, a));
                        }
                        if op_r.changed() {
                            cmds.push(Command::SetOpacity {
                                layer: lid,
                                value: op as f64,
                            });
                        }
                        for ci in 0..ncols {
                            let clip = deck_idx.and_then(|d| comp.decks[d].clip_at(lid, ci));
                            // Highlight only when playing from the deck being shown.
                            let on = active_of(&self.players, &self.gens, lid) == Some((cur, ci));
                            let text = clip
                                .map(|k| k.name.as_str())
                                .unwrap_or("(drop / right-click)");
                            let btn = egui::Button::new(text)
                                .selected(on)
                                .min_size(egui::vec2(90.0, 24.0));
                            let r = ui.add(btn);
                            if clip.is_some() {
                                r.context_menu(|ui| {
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
                                    egui::Stroke::new(2.5, egui::Color32::YELLOW),
                                    egui::StrokeKind::Outside,
                                );
                            } else if self.selected == Some((lid, ci)) {
                                ui.painter().rect_stroke(
                                    r.rect,
                                    3.0,
                                    egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if r.clicked() || r.secondary_clicked() {
                                self.selected = Some((lid, ci));
                            }
                            if r.clicked() && clip.is_some() {
                                cmds.push(Command::TriggerClip {
                                    deck: cur,
                                    layer: lid,
                                    column: ci,
                                });
                            }
                            if clip.is_none() && r.double_clicked() {
                                pick = Some((lid, ci));
                            }
                        }
                        let layer = &comp.layers[li];
                        ui.horizontal(|ui| {
                            for (txt, bus) in [
                                ("A", Some(vjc_model::Bus::A)),
                                ("-", None),
                                ("B", Some(vjc_model::Bus::B)),
                            ] {
                                if ui
                                    .selectable_label(layer.crossfader_bus == bus, txt)
                                    .clicked()
                                {
                                    cmds.push(Command::SetLayerBus { layer: lid, bus });
                                }
                            }
                        });
                        let label = vjc_core::blend::COMPOSITION_MODES
                            .iter()
                            .find(|(m, _)| *m == layer.track.blend)
                            .map_or("Normal", |(_, n)| *n);
                        egui::ComboBox::from_id_salt(("blend", lid))
                            .selected_text(label)
                            .width(90.0)
                            .show_ui(ui, |ui| {
                                for (b, name) in vjc_core::blend::COMPOSITION_MODES {
                                    if ui
                                        .selectable_label(layer.track.blend == *b, *name)
                                        .clicked()
                                    {
                                        cmds.push(Command::SetBlend {
                                            layer: lid,
                                            blend: b.clone(),
                                        });
                                    }
                                }
                            });
                        if ui.button("Clear").clicked() {
                            cmds.push(Command::ClearLayer { layer: lid });
                        }
                        ui.end_row();
                    }
                });
            });
        self.cells = cells;
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

    /// Bottom panel: Composition / Layer / Clip parameters of the selection.
    fn params_panel(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.panel_tab, PanelTab::Composition, "Composition");
            ui.selectable_value(&mut self.panel_tab, PanelTab::Layer, "Layer");
            ui.selectable_value(&mut self.panel_tab, PanelTab::Group, "Group");
            ui.selectable_value(&mut self.panel_tab, PanelTab::Clip, "Clip");
        });
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| match self.panel_tab {
            PanelTab::Composition => self.composition_tab(ui, cmds),
            PanelTab::Layer => self.layer_tab(ui, cmds),
            PanelTab::Group => self.group_tab(ui, cmds),
            PanelTab::Clip => self.clip_tab(ui, cmds),
        });
    }

    fn composition_tab(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let dials = self.dials();
        let actx = self.anim_ctx(&dials);
        let c = &self.project.composition;
        if let Some(name) = params::text_field(ui, "cname", "Name", &c.name) {
            cmds.push(Command::SetCompositionName { name });
        }
        let (mut w, mut h, mut fps) = (c.width, c.height, c.fps);
        let mut size_changed = false;
        ui.horizontal(|ui| {
            ui.label("Size");
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
        if let Some(value) =
            params::param_slider(ui, "msp", "Speed", c.master_speed, 1.0, (0.0, 4.0))
        {
            cmds.push(Command::SetMasterSpeed { value });
        }
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
            ui.label("Crossfader curve");
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
        ui.separator();
        self.cue_list(ui, cmds);
        ui.separator();
        ui.strong("Dashboard");
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
        ui.separator();
        self.chain_ui(ui, EffectTarget::Master, cmds);
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
                if ui
                    .small_button("\u{2715}")
                    .on_hover_text("Remove cue")
                    .clicked()
                {
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
        let (mut solo, mut bypass) = (l.solo, l.bypass);
        ui.horizontal(|ui| {
            if ui.toggle_value(&mut solo, "Solo").changed() {
                cmds.push(Command::SetLayerSolo { layer: lid, solo });
            }
            if ui.toggle_value(&mut bypass, "Bypass").changed() {
                cmds.push(Command::SetLayerBypass { layer: lid, bypass });
            }
        });
        ui.separator();
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
        let beats: Vec<(Option<f64>, String)> = [None, Some(1.0), Some(2.0), Some(4.0), Some(8.0)]
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
        let ap = l.autopilot.clone();
        let modes = labeled(&[
            (AutopilotMode::Off, "Off"),
            (AutopilotMode::Next, "Next"),
            (AutopilotMode::Random, "Random"),
        ]);
        if let Some(mode) = params::combo(ui, ("lap", lid), "Autopilot", &ap.mode, &modes) {
            cmds.push(Command::SetLayerAutopilot {
                layer: lid,
                autopilot: Autopilot { mode, ..ap.clone() },
            });
        }
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
        ui.separator();
        self.chain_ui(ui, EffectTarget::Layer(lid), cmds);
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
            if ui.toggle_value(&mut solo, "Solo").changed() {
                cmds.push(Command::SetGroupSolo { group: gid, solo });
            }
            if ui.toggle_value(&mut bypass, "Bypass").changed() {
                cmds.push(Command::SetGroupBypass { group: gid, bypass });
            }
            if ui.button("Ungroup").clicked() {
                cmds.push(Command::Ungroup { group: gid });
            }
        });
        ui.separator();
        self.chain_ui(ui, EffectTarget::Group(gid), cmds);
    }

    fn clip_tab(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let deck = self.project.composition.current_deck;
        let sel = self.selected;
        let Some((layer, column, k)) =
            sel.and_then(|(l, c)| Some((l, c, vjc_core::clip_in(&self.project, deck, l, c)?)))
        else {
            ui.label("Select a cell with a clip.");
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
        match &k.source {
            vjc_model::Source::File { path } => {
                ui.label(format!("Source: file {}", path.display()));
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
        if let Some(speed) = params::param_slider(ui, "spd", "Speed", t.speed, 1.0, (0.0, 4.0)) {
            cmds.push(Command::SetSpeed {
                deck,
                layer,
                column,
                speed,
            });
        }
        let syncs: Vec<(Option<f64>, String)> =
            [None, Some(1.0), Some(2.0), Some(4.0), Some(8.0), Some(16.0)]
                .into_iter()
                .map(|b| (b, b.map_or("off".into(), |n| format!("{n} beats"))))
                .collect();
        if let Some(beats) = params::combo(ui, "sync", "Beat sync", &t.beat_sync, &syncs) {
            cmds.push(Command::SetBeatSync {
                deck,
                layer,
                column,
                beats,
            });
        }
        ui.separator();
        ui.strong("Timecode sync");
        let mut on = t.timecode_start.is_some();
        let seed =
            self.tc
                .last
                .unwrap_or(vjc_model::Timecode::new(0, 0, 0, 0, vjc_model::TcFps::F25));
        if ui
            .checkbox(&mut on, "Follow timecode (position = timecode - start)")
            .changed()
        {
            cmds.push(Command::SetTimecodeStart {
                deck,
                layer,
                column,
                start: on.then_some(seed),
            });
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
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().min(400.0), 14.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        let x_at = |p: f64| rect.left() + rect.width() * p as f32;
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
            let r = egui::Rect::from_x_y_ranges(x_at(*a)..=x_at(*b), rect.y_range());
            painter.rect_filled(r, 0.0, ui.visuals().selection.bg_fill.linear_multiply(0.5));
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
                if ui.small_button("x").on_hover_text("Remove cue").clicked() {
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
        ui.separator();
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
                _ => None,
            };
            ui.horizontal(|ui| {
                let mut on = !e.bypass;
                let en = ui.checkbox(&mut on, "").on_hover_text("Enabled");
                self.fx_target(en.rect, target, i, "bypass");
                if en.changed() {
                    cmds.push(Command::SetEffectBypass {
                        target,
                        effect: e.id,
                        bypass: !on,
                    });
                }
                ui.strong(fx_label(&e.kind));
                if ui.add_enabled(i > 0, egui::Button::new("Up")).clicked() {
                    cmds.push(Command::MoveEffect {
                        target,
                        effect: e.id,
                        index: i - 1,
                    });
                }
                if ui
                    .add_enabled(i + 1 < n, egui::Button::new("Down"))
                    .clicked()
                {
                    cmds.push(Command::MoveEffect {
                        target,
                        effect: e.id,
                        index: i + 1,
                    });
                }
                if ui.button("Remove").clicked() {
                    cmds.push(Command::RemoveEffect {
                        target,
                        effect: e.id,
                    });
                }
                if let Some(why) = failure {
                    ui.colored_label(egui::Color32::LIGHT_RED, format!("disabled: {why}"));
                }
            });
            let (dw, dw_rect) = ui
                .push_id((e.id, "dry_wet"), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        ui.label("Dry/Wet");
                        let v = params::param_slider(
                            ui,
                            "dry_wet",
                            "",
                            e.dry_wet as f64,
                            1.0,
                            (0.0, 1.0),
                        );
                        (v, ui.min_rect())
                    })
                    .inner
                })
                .inner;
            self.fx_target(dw_rect, target, i, "drywet");
            if let Some(v) = dw {
                cmds.push(Command::SetEffectDryWet {
                    target,
                    effect: e.id,
                    dry_wet: v as f32,
                });
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
    fn deck_tabs(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let cur = self.project.composition.current_deck;
        let decks: Vec<(DeckId, String, usize)> = self
            .project
            .composition
            .decks
            .iter()
            .map(|d| (d.id, d.name.clone(), d.clips.len()))
            .collect();
        let last = decks.len() == 1;
        ui.horizontal(|ui| {
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
                let r = ui.selectable_label(*id == cur, name);
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
            if ui.button("+").on_hover_text("Add deck").clicked() {
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

    /// One fullscreen window per open output: the legacy single output (no screens in the
    /// project) shows the whole composition; each monitor screen shows its sliced texture,
    /// or the whole composition when it has no slices.
    fn output_window(&mut self, ctx: &egui::Context) {
        let Some(comp_id) = self.comp_tex else { return };
        let mut wins: Vec<(Uuid, usize, egui::TextureId)> = vec![];
        let ids: Vec<u32> = self.monitors.iter().map(|m| m.id).collect();
        if self.project.output.screens.is_empty() {
            if let Some(mon) = self
                .output
                .and_then(|id| monitors::find(&self.monitors, id))
            {
                wins.push((Uuid::nil(), mon, comp_id));
            }
        } else {
            for (id, mon) in self.open_screens() {
                let tex = self.screen_rt.get(&id).map_or(comp_id, |t| t.id);
                wins.push((id, mon, tex));
            }
        }
        for (sid, mon, tex) in wins {
            let mut builder = egui::ViewportBuilder::default()
                .with_title("VJCraft Output")
                .with_fullscreen(true)
                .with_decorations(false);
            builder.monitor = Some(mon);
            let mut close = false;
            let mut leave_learn = false;
            // Esc leaves learn mode first; it must not also close the output.
            let learning = self.learn;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(monitors::viewport_key(sid, ids[mon])),
                builder,
                |ui, _class| {
                    let ctx = ui.ctx().clone();
                    let rect = ui.max_rect();
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                    ui.painter().image(
                        tex,
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
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
        self.poll_monitors();
        self.tick_clock();
        self.compose(frame);
        State::ui(self, ui);
        let ctx = ui.ctx().clone();
        self.output_window(&ctx);
        // Video is live: keep repainting.
        ctx.request_repaint();
    }

    /// Closing the app mid-recording still writes the mp4 trailer (bounded wait).
    fn on_exit(&mut self) {
        if self.prefs_dirty {
            self.save_prefs();
        }
        if let Some(cap) = self.capture.take() {
            let _ = cap.shutdown();
        }
    }
}

#[cfg(test)]
mod ui_tests;

fn main() -> eframe::Result {
    let (prefs, warn) = match prefs_path() {
        Some(p) => Prefs::load(&p),
        None => (Prefs::default(), None),
    };
    let mut options = eframe::NativeOptions::default();
    if !prefs.video.vsync {
        options.wgpu_options.surface.present_mode = wgpu::PresentMode::AutoNoVsync;
    }
    eframe::run_native(
        "VJCraft",
        options,
        Box::new(move |_cc| {
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
            if let Some(p) = std::env::var_os("VJC_AUTOPLAY") {
                let l = s.project.composition.layers.last().map(|l| l.id);
                if let Some(l) = l {
                    s.drop_file(l, 0, PathBuf::from(p));
                }
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

/// Only clips with an end can be autopilot targets: generators and live
/// sources never finish or loop, so the layer would stop advancing on them.
fn autopilot_playable(source: &vjc_model::Source) -> bool {
    matches!(source, vjc_model::Source::File { .. })
}

#[cfg(test)]
mod tests {
    #[test]
    fn autopilot_skips_generators_and_live() {
        use vjc_model::Source;
        let f = Source::File {
            path: "a.mp4".into(),
        };
        assert!(super::autopilot_playable(&f));
        let g = Source::Generator {
            kind: "Plasma".into(),
            params: Default::default(),
        };
        assert!(!super::autopilot_playable(&g));
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
    let mut pan = num(&clip.track.pan) + num(&p.composition.master.pan);
    if let Some(t) = lt {
        vol *= vnum(&t.volume);
        pan += num(&t.pan);
    }
    (vol.max(0.0) as f32, pan.clamp(-1.0, 1.0) as f32)
}
