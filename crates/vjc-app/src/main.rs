mod compositor;
mod monitors;

use compositor::{Compositor, LayerDraw};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;
use vjc_core::clock::{Clock, beat_sync_rate};
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
    /// Deck tab being renamed, with its edit buffer.
    renaming: Option<(DeckId, String)>,
    /// Playhead per layer, restarted with each trigger (stale entries are harmless).
    heads: HashMap<LayerId, Playhead>,
    last_head_tick: std::time::Instant,
    comp: Option<Compositor>,
    comp_tex: Option<egui::TextureId>,
    /// Display id of the monitor the output window is on, while open.
    output: Option<u32>,
    monitors: Vec<monitors::MonitorInfo>,
    monitors_polled: Option<std::time::Instant>,
    /// Cell rects from the last frame, for hit-testing file drops.
    cells: Vec<CellInfo>,
    last_pointer: Option<egui::Pos2>,
    /// Cell selected by click; fallback target for drops.
    selected: Option<(LayerId, u32)>,
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
        State {
            project,
            history: History::new(),
            warnings: vec![],
            status: "Drop video files on the grid cells.".into(),
            players: HashMap::new(),
            heads: HashMap::new(),
            last_head_tick: std::time::Instant::now(),
            comp: None,
            comp_tex: None,
            output: None,
            monitors: monitors::list(),
            monitors_polled: None,
            cells: vec![],
            last_pointer: None,
            selected: None,
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
        );
        match self
            .history
            .exec(&mut self.project, cmd, std::time::Instant::now())
        {
            Ok(events) => {
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
                Event::LayerCleared { layer } => {
                    self.players.remove(&layer);
                }
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
                    if self.players.get(&layer).map(|(d, c, _)| (*d, *c)) == Some((deck, column)) {
                        self.players.remove(&layer);
                    }
                }
                Event::DeckRemoved { deck } => self.players.retain(|_, (d, _, _)| *d != deck),
            }
        }
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
            // Play modes: the playhead picks the position, the player serves the frame.
            if let (Some(d), Some(head)) = (player.duration(), self.heads.get_mut(layer)) {
                let t = head.tick(dt, tr.play_mode, tr.direction, tr.speed * sync, d);
                player.set_position(t.position);
                if t.finished {
                    finished.push(*layer);
                }
            }
        }
        // Once: clear the layer after the last frame.
        for layer in finished {
            self.exec(Command::ClearLayer { layer });
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
        let fit_of = |l: &Layer| {
            let (deck, col, _) = self.players.get(&l.id)?;
            Some(vjc_core::clip_in(&self.project, *deck, l.id, *col)?.fit)
        };
        let draws: Vec<LayerDraw> = frames
            .iter()
            .map(|(l, f)| LayerDraw {
                id: l.id,
                opacity: match l.track.opacity.value {
                    vjc_model::Value::Number(n) => n as f32,
                    _ => 1.0,
                },
                blend: l.track.blend.clone(),
                fit: fit_of(l).unwrap_or_default(),
                frame: f
                    .as_ref()
                    .map(|f| (f.width, f.height, f.data.as_slice(), f.seq)),
            })
            .collect();
        comp.render(&rs.device, &rs.queue, &draws);
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
            if ui.button("Tap").clicked() {
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
        self.time_bar(ui);

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
                    for n in &names {
                        ui.label(n);
                    }
                    ui.label("Opacity");
                    ui.label("Blend");
                    ui.label("Speed");
                    ui.label("");
                    ui.end_row();
                    for li in 0..comp.layers.len() {
                        let lid = comp.layers[li].id;
                        ui.label(&comp.layers[li].name);
                        for ci in 0..ncols {
                            let clip = deck_idx.and_then(|d| comp.decks[d].clip_at(lid, ci));
                            // Highlight only when playing from the deck being shown.
                            let on =
                                self.players.get(&lid).map(|(d, c, _)| (*d, *c)) == Some((cur, ci));
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
                                });
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
                            if clip.is_none() && (r.secondary_clicked() || r.double_clicked()) {
                                pick = Some((lid, ci));
                            }
                        }
                        let layer = &comp.layers[li];
                        let mut op = match layer.track.opacity.value {
                            vjc_model::Value::Number(n) => n as f32,
                            _ => 1.0,
                        };
                        if ui
                            .add(egui::Slider::new(&mut op, 0.0..=1.0).fixed_decimals(2))
                            .changed()
                        {
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
                        let active = self.players.get(&lid).map(|(d, c, _)| (*d, *c));
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
        for c in cmds {
            self.exec(c);
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
