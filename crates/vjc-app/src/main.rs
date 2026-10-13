mod browser;
mod compositor;
mod fx;
mod monitors;

use compositor::{Compositor, EffectDraw, LayerDraw};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;
use vjc_core::clock::{Clock, beat_sync_rate};
use vjc_core::effects::EffectTarget;
use vjc_core::playhead::Playhead;
use vjc_core::{Command, Event, History};
use vjc_media::{Frame, Player};
use vjc_model::{
    BlendMode, Column, Deck, DeckId, Direction, FitMode, Layer, LayerId, PlayMode, Project, Track,
    Transition,
};

struct State {
    project: Project,
    history: History,
    warnings: Vec<String>,
    status: String,
    /// Playing clip per layer: (deck it was triggered from, column, player). One clip per
    /// layer. Lookups of the playing clip use this deck, never `current_deck`.
    players: HashMap<LayerId, (DeckId, u32, Player)>,
    /// Playing generator clip per layer: (deck, column, own TIME in seconds). The time
    /// follows the clip transport (speed, direction, pause); a layer has a player or a
    /// generator, never both.
    gens: HashMap<LayerId, (DeckId, u32, f64)>,
    /// Deck tab being renamed, with its edit buffer.
    renaming: Option<(DeckId, String)>,
    /// Playhead per layer, restarted with each trigger (stale entries are harmless).
    heads: HashMap<LayerId, Playhead>,
    last_head_tick: std::time::Instant,
    comp: Option<Compositor>,
    fx: fx::FxLib,
    fx_epoch: std::time::Instant,
    /// Whose effect chain the effects panel shows.
    fx_scope: FxScope,
    comp_tex: Option<egui::TextureId>,
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
    /// Column last triggered (by column, cell or step); the "selected" column of shortcuts.
    col_sel: Option<u32>,
    /// Shortcuts > Edit keyboard: clicks pick a target instead of acting.
    learn: bool,
    /// Target (address) waiting for a key while learning.
    learn_target: Option<String>,
    /// Mappable widgets drawn this frame, for learn clicks and the key overlay.
    targets: Vec<(egui::Rect, String)>,
    /// Master bypass: black output (runtime only, not saved).
    master_bypass: bool,
    /// Path of the open .vjc, if any.
    file: Option<PathBuf>,
    dirty: bool,
    last_autosave: std::time::Instant,
    title: String,
    /// Master clock (runtime only; BPM lives in the project, phase here).
    clock: Clock,
    /// Latest beat index and phase from `clock.tick`, for the indicator.
    beat: i64,
    phase: f64,
    nudge_dir: i8,
    /// Visual only for now. TODO(#9): click sound (see build-log).
    metronome: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum FxScope {
    Clip,
    Layer,
    Master,
}

/// Active effects of a chain with this frame's uniforms. Effects still compiling or that
/// failed are skipped.
fn draw_chain(
    lib: &mut fx::FxLib,
    chain: &[vjc_model::Effect],
    time: f32,
    size: (u32, u32),
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
                uniforms: isf.pack(&e.params, time, size),
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

/// Edits one effect param; returns the new value when the user changed it.
fn param_widget(
    ui: &mut egui::Ui,
    name: &str,
    kind: &vjc_fx::InputKind,
    value: &vjc_model::Value,
) -> Option<vjc_model::Value> {
    use vjc_fx::InputKind as K;
    use vjc_model::Value as V;
    let mut out = None;
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        ui.label(name);
        match (kind, value) {
            (K::Float { min, max, .. }, V::Number(n)) => {
                let mut x = *n;
                if ui.add(egui::Slider::new(&mut x, *min..=*max)).changed() {
                    out = Some(V::Number(x));
                }
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

/// Browser start folder: the project's folder, else ~/Movies, else home.
fn browser_start(project: Option<&std::path::Path>) -> PathBuf {
    if let Some(d) = project.and_then(|p| p.parent()).filter(|d| d.is_dir()) {
        return d.to_path_buf();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let movies = home.join("Movies");
    if movies.is_dir() { movies } else { home }
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

/// ~/Library/Application Support/VJCraft/recovery.vjc
fn recovery_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/VJCraft/recovery.vjc"))
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
    if matches!(ask, rfd::MessageDialogResult::Custom(ref l) if l == "Restore") && s.load(r.clone())
    {
        s.file = None;
        s.dirty = true;
    }
    // Restored state now lives in memory (and is re-autosaved while dirty);
    // a discarded or unreadable file must not be offered again.
    let _ = std::fs::remove_file(r);
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
        crossfader_bus: None,
        group: None,
    }
}

impl State {
    fn new() -> Self {
        let mut project = Project::new("Untitled");
        let c = &mut project.composition;
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
        project.shortcuts = vjc_core::shortcuts::default_shortcuts();
        State {
            project,
            history: History::new(),
            warnings: vec![],
            status: "Drop video files on the grid cells.".into(),
            players: HashMap::new(),
            gens: HashMap::new(),
            heads: HashMap::new(),
            last_head_tick: std::time::Instant::now(),
            comp: None,
            fx: fx::FxLib::new(),
            fx_epoch: std::time::Instant::now(),
            fx_scope: FxScope::Layer,
            comp_tex: None,
            output: None,
            monitors: monitors::list(),
            monitors_polled: None,
            cells: vec![],
            layer_rects: vec![],
            master_rect: None,
            browser: browser::Browser::new(browser_start(None)),
            last_pointer: None,
            selected: None,
            col_sel: None,
            learn: false,
            learn_target: None,
            targets: vec![],
            master_bypass: false,
            file: None,
            dirty: false,
            last_autosave: std::time::Instant::now(),
            title: String::new(),
            clock: Clock::new(120.0, std::time::Instant::now()),
            beat: 0,
            phase: 0.0,
            nudge_dir: 0,
            metronome: false,
            renaming: None,
        }
    }

    fn reset_players(&mut self) {
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
                if let Some(r) = recovery_path() {
                    let _ = std::fs::remove_file(r);
                }
                true
            }
            _ => false,
        }
    }

    fn new_project(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        let fresh = State::new();
        self.project = fresh.project;
        self.file = None;
        self.dirty = false;
        self.warnings.clear();
        self.reset_players();
        self.status = "New project".into();
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
            Ok((project, warnings)) => {
                self.project = project;
                self.warnings = warnings;
                self.status = format!("Opened {}", path.display());
                self.file = Some(path);
                self.dirty = false;
                self.reset_players();
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

    fn save_to(&mut self, path: PathBuf) -> bool {
        match vjc_io::save(&self.project, &path) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.file = Some(path);
                self.dirty = false;
                if let Some(r) = recovery_path() {
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

    /// Every 60 s while dirty, writes the recovery file.
    fn autosave(&mut self) {
        if !self.dirty || self.last_autosave.elapsed() < std::time::Duration::from_secs(60) {
            return;
        }
        self.last_autosave = std::time::Instant::now();
        if let Some(r) = recovery_path() {
            if let Some(d) = r.parent() {
                let _ = std::fs::create_dir_all(d);
            }
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
                self.history.clear();
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
                | Command::SetMasterBypass { .. }
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
        match self
            .history
            .exec(&mut self.project, cmd, std::time::Instant::now())
        {
            Ok(events) => {
                if col_after.is_some() {
                    self.col_sel = col_after;
                }
                if is_step {
                    // The step lands on the first playable clip: follow it.
                    let landed = events.iter().find_map(|e| match e {
                        Event::ClipStarted { column, .. }
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
        match self.history.undo(&mut self.project) {
            Some(Ok(ev)) => {
                self.dirty = true;
                self.handle(ev);
            }
            Some(Err(e)) => self.status = format!("Undo failed: {e}"),
            None => {}
        }
    }

    fn redo(&mut self) {
        match self.history.redo(&mut self.project) {
            Some(Ok(ev)) => {
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
                        self.players.remove(&layer); // joins the old decode thread
                        self.players
                            .insert(layer, (deck, column, Player::open_scrub(&path)));
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
                Event::GeneratorStarted {
                    deck,
                    layer,
                    column,
                } => {
                    self.players.remove(&layer);
                    self.gens.insert(layer, (deck, column, 0.0));
                }
                Event::LayerCleared { layer } => {
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
                Event::Tap { at } => {
                    if let Some(bpm) = self.clock.tap(at) {
                        self.exec(Command::SetBpm { bpm });
                    }
                }
                Event::Nudge { dir, at } => self.clock.nudge(dir, at),
                Event::Resync { at } => self.clock.resync(at),
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
                    self.players.retain(|_, (d, _, _)| *d != deck);
                    self.gens.retain(|_, (d, _, _)| *d != deck);
                }
            }
        }
    }

    /// Deck and column of the clip playing on the layer (video or generator).
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
        let bpm = self.project.composition.tempo.bpm;
        if self.clock.bpm() != bpm.clamp(vjc_core::clock::MIN_BPM, vjc_core::clock::MAX_BPM) {
            self.clock.set_bpm(bpm, now);
        }
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
        let mut finished = vec![];
        for (layer, (deck, col, player)) in &self.players {
            let Some(clip) = vjc_core::clip_in(&self.project, *deck, *layer, *col) else {
                continue;
            };
            let tr = &clip.transport;
            let sync = match (tr.beat_sync, player.duration()) {
                (Some(n), Some(d)) => beat_sync_rate(d, n, bpm),
                _ => 1.0,
            };
            let rate = vjc_core::playback_rate(tr.speed, sync, master_speed);
            // Play modes: the playhead picks the position, the player serves the frame.
            if let (Some(d), Some(head)) = (player.duration(), self.heads.get_mut(layer)) {
                let t = head.tick(dt, tr.play_mode, tr.direction, rate, d);
                player.set_position(t.position);
                if t.finished {
                    finished.push(*layer);
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
                *t += dt * vjc_core::playback_rate(tr.speed, 1.0, master_speed) * dir;
            }
        }
        // Once: clear the layer after the last frame.
        for layer in finished {
            self.exec(Command::ClearLayer { layer });
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
        self.exec(Command::ImportMedia {
            deck,
            layer,
            column,
            path,
        });
        self.exec(Command::TriggerClip {
            deck,
            layer,
            column,
        });
    }

    fn compose(&mut self, frame: &eframe::Frame) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let c = &self.project.composition;
        let (w, h) = (c.width.max(1), c.height.max(1));
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
                    if l.bypass {
                        None
                    } else {
                        self.players
                            .get(&l.id)
                            .and_then(|(_, _, p)| p.latest_frame())
                    },
                )
            })
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
        let mut draws: Vec<LayerDraw> = vec![];
        for (l, f) in &frames {
            // Clip chain first, then the layer chain.
            let size = f.as_ref().map_or((w, h), |f| (f.width, f.height));
            let clip = active_of(players, gens, l.id)
                .and_then(|(d, col)| vjc_core::clip_in(&self.project, d, l.id, col));
            let clip_fx = clip.map(|k| k.track.effects.as_slice());
            let generator = match (clip.map(|k| &k.source), gens.get(&l.id)) {
                (Some(vjc_model::Source::Generator { kind, params }), Some((_, _, t)))
                    if !l.bypass =>
                {
                    self.fx.request(kind);
                    let ready = self.fx.status(kind) == Some(&fx::Status::Ready);
                    self.fx.isf(kind).filter(|_| ready).map(|isf| EffectDraw {
                        kind: kind.clone(),
                        uniforms: isf.pack(params, *t as f32, (w, h)),
                    })
                }
                _ => None,
            };
            let mut effects = draw_chain(&mut self.fx, clip_fx.unwrap_or_default(), time, size);
            effects.extend(draw_chain(&mut self.fx, &l.track.effects, time, size));
            draws.push(LayerDraw {
                id: l.id,
                opacity: match l.track.opacity.value {
                    vjc_model::Value::Number(n) => n as f32,
                    _ => 1.0,
                },
                blend: l.track.blend.clone(),
                fit: fit_of(l).unwrap_or_default(),
                effects,
                generator,
                frame: f
                    .as_ref()
                    .map(|f| (f.width, f.height, f.data.as_slice(), f.seq)),
            });
        }
        let master = draw_chain(&mut self.fx, &c.master.effects, time, (w, h));
        let master_opacity = if self.master_bypass {
            0.0
        } else {
            match c.master.opacity.value {
                vjc_model::Value::Number(n) => n as f32,
                _ => 1.0,
            }
        };
        comp.render(&rs.device, &rs.queue, &draws, &master, master_opacity);
    }

    /// Master row: opacity, speed, bypass, Clear All.
    fn master_bar(&mut self, ui: &mut egui::Ui) {
        let mut cmds = vec![];
        let bar = ui.horizontal(|ui| {
            ui.label("Master");
            let mut op = match self.project.composition.master.opacity.value {
                vjc_model::Value::Number(n) => n,
                _ => 1.0,
            };
            if ui
                .add(egui::Slider::new(&mut op, 0.0..=1.0).text("Opacity"))
                .changed()
            {
                cmds.push(Command::SetMasterOpacity { value: op });
            }
            let mut sp = self.project.composition.master_speed;
            if ui
                .add(egui::Slider::new(&mut sp, 0.0..=4.0).text("Speed"))
                .changed()
            {
                cmds.push(Command::SetMasterSpeed { value: sp });
            }
            let mut bypass = self.master_bypass;
            let by = ui.toggle_value(&mut bypass, "Bypass");
            self.targets.push((by.rect, "/comp/master/bypass".into()));
            if by.changed() {
                cmds.push(Command::SetMasterBypass { bypass });
            }
            if ui.button("Clear All").clicked() {
                cmds.push(Command::ClearAll);
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
                self.learn_key(key);
            }
            return;
        }
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let rt = vjc_core::shortcuts::Ctx::new(self.col_sel, self.master_bypass);
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

    /// One key press in learn mode: Esc leaves, Delete unmaps the target, any other key
    /// maps to it (replacing its old key and taking the key from any other target).
    fn learn_key(&mut self, key: egui::Key) {
        use egui::Key;
        if key == Key::Escape {
            self.learn = false;
            self.learn_target = None;
            self.status = "Keyboard editing off".into();
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
            self.status = format!("{a}: press a key (Delete unmaps, Esc leaves)");
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
            if let Some(k) = vjc_core::shortcuts::key_for(&self.project, addr) {
                let galley =
                    paint.layout_no_wrap(k.to_owned(), font.clone(), v.strong_text_color());
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
            let mut bpm = self.project.composition.tempo.bpm;
            if ui
                .add(
                    egui::DragValue::new(&mut bpm)
                        .range(20.0..=300.0)
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
            let bpm = self.project.composition.tempo.bpm;
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
                if ui
                    .selectable_label(self.learn, "Edit keyboard")
                    .clicked()
                {
                    ui.close();
                    self.learn = !self.learn;
                    self.learn_target = None;
                    self.status = if self.learn {
                        "Click a cell, column, opacity, Tap or Bypass, then press a key (Esc leaves)"
                            .into()
                    } else {
                        "Keyboard editing off".into()
                    };
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
                        self.output = Some(m.id);
                    }
                }
                if ui
                    .add_enabled(self.output.is_some(), egui::Button::new("Close output"))
                    .clicked()
                {
                    ui.close();
                    self.output = None;
                }
            });
        });
        let fxs = browser_fx();
        let mut opened = None;
        egui::Panel::right("browser")
            .default_size(260.0)
            .show(ui, |ui| opened = self.browser.show(ui, &fxs));
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
        self.time_bar(ui);
        self.master_bar(ui);
        self.shortcut_keys(&ctx);

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

        // Monitor (aspect-fit).
        if let Some(id) = self.comp_tex {
            let c = &self.project.composition;
            let aspect = c.width as f32 / c.height.max(1) as f32;
            let h = 220.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(h * aspect, h), egui::Sense::hover());
            ui.painter().image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        ui.separator();

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
                    ui.label("Opacity");
                    ui.label("Blend");
                    ui.label("Speed");
                    ui.label("");
                    ui.end_row();
                    for li in 0..comp.layers.len() {
                        let lid = comp.layers[li].id;
                        let name = ui.add(
                            egui::Label::new(&comp.layers[li].name).sense(egui::Sense::click()),
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
                            add_effect_menu(ui, EffectTarget::Layer(lid), &mut cmds)
                        });
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
                            if let Some(cur_fit) = clip.map(|k| k.fit) {
                                let cur_sync = clip.and_then(|k| k.transport.beat_sync);
                                let cur_mode = clip.map(|k| k.transport.play_mode);
                                let cur_dir = clip.map(|k| k.transport.direction);
                                r.context_menu(|ui| {
                                    ui.label("Play mode");
                                    let modes = [
                                        (PlayMode::Loop, "Loop"),
                                        (PlayMode::Bounce, "Bounce"),
                                        (PlayMode::Once, "Once"),
                                        (PlayMode::OnceHold, "Once, hold last frame"),
                                    ];
                                    for (m, name) in modes {
                                        if ui.selectable_label(cur_mode == Some(m), name).clicked()
                                        {
                                            cmds.push(Command::SetPlayMode {
                                                deck: cur,
                                                layer: lid,
                                                column: ci,
                                                mode: m,
                                            });
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    ui.label("Direction");
                                    let dirs = [
                                        (Direction::Forward, "Forward"),
                                        (Direction::Backward, "Backward"),
                                        (Direction::Pause, "Pause"),
                                        (Direction::Random, "Random"),
                                    ];
                                    for (d, name) in dirs {
                                        if ui.selectable_label(cur_dir == Some(d), name).clicked() {
                                            cmds.push(Command::SetDirection {
                                                deck: cur,
                                                layer: lid,
                                                column: ci,
                                                direction: d,
                                            });
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    ui.label("Sync to BPM");
                                    let opts = [
                                        None,
                                        Some(1.0),
                                        Some(2.0),
                                        Some(4.0),
                                        Some(8.0),
                                        Some(16.0),
                                    ];
                                    for o in opts {
                                        let label = match o {
                                            None => "off".to_string(),
                                            Some(n) => format!("{n} beats"),
                                        };
                                        if ui.selectable_label(cur_sync == o, label).clicked() {
                                            cmds.push(Command::SetBeatSync {
                                                deck: cur,
                                                layer: lid,
                                                column: ci,
                                                beats: o,
                                            });
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    ui.label("Aspect ratio");
                                    for m in FitMode::ALL {
                                        if ui.selectable_label(cur_fit == m, m.label()).clicked() {
                                            cmds.push(Command::SetFit {
                                                deck: cur,
                                                layer: lid,
                                                column: ci,
                                                fit: m,
                                            });
                                            ui.close();
                                        }
                                    }
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
                        // Speed of the clip playing on this layer (from its own deck).
                        let active = active_of(&self.players, &self.gens, lid);
                        match active.and_then(|(d, c)| {
                            Some((d, c, vjc_core::clip_in(&self.project, d, lid, c)?))
                        }) {
                            Some((d, c, k)) => {
                                let mut sp = k.transport.speed as f32;
                                // Edit the clip in the deck it plays from.
                                ui.vertical(|ui| {
                                    if d != cur {
                                        let name = comp.decks.iter().find(|x| x.id == d);
                                        ui.small(format!(
                                            "from {}",
                                            name.map_or("?", |x| x.name.as_str())
                                        ));
                                    }
                                    if ui
                                        .add(
                                            egui::Slider::new(&mut sp, 0.0..=4.0).fixed_decimals(2),
                                        )
                                        .changed()
                                    {
                                        cmds.push(Command::SetSpeed {
                                            deck: d,
                                            layer: lid,
                                            column: c,
                                            speed: sp as f64,
                                        });
                                    }
                                });
                            }
                            None => {
                                ui.label("-");
                            }
                        }
                        if ui.button("Clear").clicked() {
                            cmds.push(Command::ClearLayer { layer: lid });
                        }
                        ui.end_row();
                    }
                });
            });
        self.cells = cells;
        self.effects_panel(ui, &mut cmds);
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

    /// Chain of the selected layer, its playing clip or the master, with per-effect controls.
    fn effects_panel(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong("Effects");
            ui.selectable_value(&mut self.fx_scope, FxScope::Clip, "Clip");
            ui.selectable_value(&mut self.fx_scope, FxScope::Layer, "Layer");
            ui.selectable_value(&mut self.fx_scope, FxScope::Master, "Master");
        });
        let sel = self.selected.map(|s| s.0);
        let target = match self.fx_scope {
            FxScope::Master => Some(EffectTarget::Master),
            FxScope::Layer => sel.map(EffectTarget::Layer),
            FxScope::Clip => sel.and_then(|l| {
                let (deck, column) = self.active(l)?;
                Some(EffectTarget::Clip {
                    deck,
                    layer: l,
                    column,
                })
            }),
        };
        let Some(target) = target else {
            ui.label("Select a layer (click its name); Clip needs a playing clip.");
            return;
        };
        ui.menu_button("Add effect", |ui| add_effect_menu(ui, target, cmds));
        if let EffectTarget::Clip {
            deck,
            layer,
            column,
        } = target
            && let Some(vjc_model::Source::Generator { kind, params }) =
                vjc_core::clip_in(&self.project, deck, layer, column).map(|k| k.source.clone())
        {
            ui.strong(format!("Generator: {}", fx_label(&kind)));
            let inputs = self.fx.isf(&kind).map(|f| f.inputs.clone());
            if inputs.is_none() {
                ui.colored_label(egui::Color32::LIGHT_RED, "unknown generator (not rendered)");
            }
            for (pi, p) in params.iter().enumerate() {
                let Some(ik) = inputs.as_ref().and_then(|v| v.get(pi)).map(|i| &i.kind) else {
                    continue;
                };
                if let Some(value) = param_widget(ui, &p.name, ik, &p.value) {
                    cmds.push(Command::SetGeneratorParam {
                        deck,
                        layer,
                        column,
                        param: pi,
                        value,
                    });
                }
            }
            ui.separator();
        }
        let chain = vjc_core::effects::chain(&self.project, target)
            .unwrap_or_default()
            .to_vec();
        let n = chain.len();
        for (i, e) in chain.iter().enumerate() {
            let inputs = self.fx.isf(&e.kind).map(|f| f.inputs.clone());
            let failure = match self.fx.status(&e.kind) {
                Some(fx::Status::Failed(why)) => Some(why.clone()),
                _ => None,
            };
            ui.horizontal(|ui| {
                let mut on = !e.bypass;
                if ui.checkbox(&mut on, "").on_hover_text("Enabled").changed() {
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
            for (pi, p) in e.params.iter().enumerate() {
                let Some(kind) = inputs.as_ref().and_then(|v| v.get(pi)).map(|i| &i.kind) else {
                    continue;
                };
                if let Some(value) = param_widget(ui, &p.name, kind, &p.value) {
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

    fn output_window(&mut self, ctx: &egui::Context) {
        let Some(mon) = self
            .output
            .and_then(|id| monitors::find(&self.monitors, id))
        else {
            return;
        };
        let Some(id) = self.comp_tex else { return };
        let mut builder = egui::ViewportBuilder::default()
            .with_title("VJCraft Output")
            .with_fullscreen(true)
            .with_decorations(false);
        builder.monitor = Some(mon);
        let mut close = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("vjc-output"),
            builder,
            |ui, _class| {
                let ctx = ui.ctx().clone();
                let rect = ui.max_rect();
                ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                ui.painter().image(
                    id,
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                if ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.viewport().close_requested())
                {
                    close = true;
                }
            },
        );
        if close {
            self.output = None;
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
}

fn main() -> eframe::Result {
    eframe::run_native(
        "VJCraft",
        eframe::NativeOptions::default(),
        Box::new(|_cc| {
            let mut s = State::new();
            offer_recovery(&mut s);
            // Debug aid: VJC_AUTOPLAY=/path/video.mov loads it in the bottom layer, column 1.
            if let Some(p) = std::env::var_os("VJC_AUTOPLAY") {
                let l = s.project.composition.layers.last().map(|l| l.id);
                if let Some(l) = l {
                    s.drop_file(l, 0, PathBuf::from(p));
                }
            }
            Ok(Box::new(s))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
