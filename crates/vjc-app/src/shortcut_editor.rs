//! Shortcuts editor window: every mapping (keyboard, MIDI, OSC, DMX input) in one table,
//! with filter, inline edit, learn, conflict highlight and export/import to a `.vjcmap`
//! file. It edits `Project::shortcuts` through undoable commands; the models and the
//! per-source learn code stay in `vjc_core`.

use crate::widgets::{self, Kind, Width};
use crate::{State, theme};
use eframe::egui;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vjc_core::Command;
use vjc_model::{ControlSource, Project, Shortcut, ShortcutMode};

/// Input family of a shortcut, for the filter and for learn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    Key,
    Midi,
    Osc,
    Dmx,
}

pub fn src_of(s: &ControlSource) -> Src {
    match s {
        ControlSource::Key { .. } => Src::Key,
        ControlSource::MidiNote { .. } | ControlSource::MidiCc { .. } => Src::Midi,
        ControlSource::Osc { .. } => Src::Osc,
        ControlSource::Dmx { .. } => Src::Dmx,
    }
}

fn src_name(s: Src) -> &'static str {
    match s {
        Src::Key => "Keyboard",
        Src::Midi => "MIDI",
        Src::Osc => "OSC",
        Src::Dmx => "DMX",
    }
}

/// Human-readable input: key name, MIDI channel + note/CC, OSC address, DMX universe.channel.
pub fn input_label(s: &ControlSource) -> String {
    match s {
        ControlSource::Key { code } => code.clone(),
        ControlSource::MidiNote {
            channel,
            note,
            device,
        } => format!("ch{} note {note} ({device})", u16::from(*channel) + 1),
        ControlSource::MidiCc {
            channel,
            cc,
            device,
        } => format!("ch{} CC {cc} ({device})", u16::from(*channel) + 1),
        ControlSource::Osc { address } => address.clone(),
        ControlSource::Dmx { universe, channel } => format!("{universe}.{channel}"),
    }
}

fn mode_name(m: &ShortcutMode) -> &'static str {
    match m {
        ShortcutMode::Trigger => "Trigger",
        ShortcutMode::Toggle => "Toggle",
        ShortcutMode::Absolute => "Absolute",
        ShortcutMode::Relative => "Relative",
    }
}

/// Rows to show: the source filter and a case-insensitive search over input, target and mode.
pub fn visible<'a>(p: &'a Project, filter: Option<Src>, search: &str) -> Vec<&'a Shortcut> {
    let q = search.trim().to_lowercase();
    p.shortcuts
        .iter()
        .filter(|s| filter.is_none_or(|f| src_of(&s.source) == f))
        .filter(|s| {
            q.is_empty()
                || input_label(&s.source).to_lowercase().contains(&q)
                || s.target.to_lowercase().contains(&q)
                || mode_name(&s.mode).to_lowercase().contains(&q)
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
struct MapFile {
    vjcmap: u32,
    shortcuts: Vec<Shortcut>,
}

/// All mappings as `.vjcmap` JSON (apart from the project).
pub fn export_json(p: &Project) -> String {
    let f = MapFile {
        vjcmap: 1,
        shortcuts: p.shortcuts.clone(),
    };
    serde_json::to_string_pretty(&f).unwrap_or_default()
}

/// Commands that add the mappings of a `.vjcmap` text, with fresh ids.
pub fn import_json(p: &Project, text: &str) -> Result<Vec<Command>, String> {
    let f: MapFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if f.vjcmap != 1 {
        return Err(format!("unsupported .vjcmap version {}", f.vjcmap));
    }
    for (i, s) in f.shortcuts.iter().enumerate() {
        if let Some(why) = midi_out_of_range(&s.source) {
            return Err(format!("shortcut {} ({}): {why}", i + 1, s.target));
        }
    }
    Ok(f.shortcuts
        .into_iter()
        .filter(|s| s.range.0.is_finite() && s.range.1.is_finite())
        .enumerate()
        .map(|(i, mut s)| {
            s.id = Uuid::new_v4();
            s.effect = None;
            Command::AddShortcut {
                shortcut: s,
                index: p.shortcuts.len() + i,
            }
        })
        .collect())
}

/// MIDI channel is 0..15 and note/CC 0..127; anything else is a corrupt file.
fn midi_out_of_range(s: &ControlSource) -> Option<String> {
    let (channel, num, kind) = match s {
        ControlSource::MidiNote { channel, note, .. } => (*channel, *note, "note"),
        ControlSource::MidiCc { channel, cc, .. } => (*channel, *cc, "CC"),
        _ => return None,
    };
    if channel > 15 {
        return Some(format!("MIDI channel {channel} is out of range (0..15)"));
    }
    (num > 127).then(|| format!("MIDI {kind} {num} is out of range (0..127)"))
}

/// Editor window state (not saved).
#[derive(Default)]
pub struct Editor {
    pub open: bool,
    pub filter: Option<Src>,
    pub search: String,
    /// Row waiting for its next input.
    pub learn: Option<Uuid>,
    /// Learn mode: after each capture the next visible row of the same source is armed.
    pub learn_all: bool,
    /// Control that was captured last: its further input is ignored until another arrives.
    pub last: Option<ControlSource>,
    /// A row was captured during the current pump call (at most one per call).
    pub captured: bool,
}

impl State {
    /// Family whose inputs are being learned; those inputs are swallowed, never fired.
    pub(crate) fn sc_armed(&self) -> Option<Src> {
        let id = self.sc_ed.learn?;
        let s = self.project.shortcuts.iter().find(|s| s.id == id)?;
        Some(src_of(&s.source))
    }

    /// Gives the armed row `src` when it is of the same family; true when it did.
    pub(crate) fn sc_capture(&mut self, src: ControlSource) -> bool {
        let Some(id) = self.sc_ed.learn else {
            return false;
        };
        let Some(row) = self.project.shortcuts.iter().find(|s| s.id == id).cloned() else {
            self.sc_ed.learn = None;
            return false;
        };
        let fam = src_of(&row.source);
        if src_of(&src) != fam || self.sc_ed.captured || self.sc_ed.last.as_ref() == Some(&src) {
            return false;
        }
        self.sc_ed.captured = true;
        self.sc_ed.last = Some(src.clone());
        self.status = format!("{} -> {}", input_label(&src), row.target);
        self.sc_ed.learn = None;
        self.exec(Command::UpdateShortcut {
            shortcut: Shortcut { source: src, ..row },
        });
        if self.sc_ed.learn_all {
            let rows = visible(&self.project, self.sc_ed.filter, &self.sc_ed.search);
            let next = rows
                .iter()
                .skip_while(|s| s.id != id)
                .skip(1)
                .find(|s| src_of(&s.source) == fam)
                .map(|s| s.id);
            self.sc_ed.learn = next;
            self.sc_ed.learn_all = next.is_some();
        }
        true
    }

    fn sc_export(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("VJCraft mappings", &["vjcmap"])
            .set_file_name("mappings.vjcmap")
            .save_file()
        else {
            return;
        };
        self.status = match std::fs::write(&path, export_json(&self.project)) {
            Ok(()) => format!("Exported {} mappings", self.project.shortcuts.len()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    fn sc_import(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("VJCraft mappings", &["vjcmap"])
            .pick_file()
        else {
            return;
        };
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string());
        match text.and_then(|t| import_json(&self.project, &t)) {
            Ok(cmds) => {
                self.status = format!("Imported {} mappings", cmds.len());
                for c in cmds {
                    self.exec(c);
                }
            }
            Err(e) => self.status = format!("Import failed: {e}"),
        }
    }

    pub(crate) fn shortcut_editor_window(&mut self, ctx: &egui::Context) {
        if !self.sc_ed.open {
            return;
        }
        let mut open = true;
        let mut cmds: Vec<Command> = vec![];
        let (mut export, mut import) = (false, false);
        widgets::panel_window(ctx, "Shortcuts")
            .open(&mut open)
            .collapsible(false)
            .default_width(900.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (f, l) in [
                        (None, "All"),
                        (Some(Src::Key), "Keyboard"),
                        (Some(Src::Midi), "MIDI"),
                        (Some(Src::Osc), "OSC"),
                        (Some(Src::Dmx), "DMX"),
                    ] {
                        if widgets::tab(ui, self.sc_ed.filter == f, l).clicked() {
                            self.sc_ed.filter = f;
                        }
                    }
                    ui.label("Search");
                    widgets::field_text(ui, &mut self.sc_ed.search, Width::Md, None);
                });
                ui.horizontal(|ui| {
                    let mut all = self.sc_ed.learn_all;
                    if widgets::toggle(ui, &mut all, "Learn mode").changed() {
                        self.sc_ed.last = None;
                        self.sc_ed.learn = if all {
                            visible(&self.project, self.sc_ed.filter, &self.sc_ed.search)
                                .first()
                                .map(|s| s.id)
                        } else {
                            None
                        };
                        self.sc_ed.learn_all = all && self.sc_ed.learn.is_some();
                    }
                    export = widgets::btn(ui, "Export...", Kind::Secondary).clicked();
                    import = widgets::btn(ui, "Import...", Kind::Secondary).clicked();
                });
                widgets::help(
                    ui,
                    "Learn captures the next input of the row's source without firing it. \
                     Rows in red share their input.",
                );
                ui.separator();
                let clash = vjc_core::shortcuts::conflicts(&self.project);
                let rows: Vec<Shortcut> =
                    visible(&self.project, self.sc_ed.filter, &self.sc_ed.search)
                        .into_iter()
                        .cloned()
                        .collect();
                egui::ScrollArea::both().max_height(420.0).show(ui, |ui| {
                    egui::Grid::new("shortcut_rows")
                        .striped(true)
                        .show(ui, |ui| {
                            for h in [
                                "Source", "Input", "Target", "Mode", "Min", "Max", "Invert", "",
                            ] {
                                widgets::help(ui, h);
                            }
                            ui.end_row();
                            for row in &rows {
                                self.sc_row(ui, row, clash.contains(&row.id), &mut cmds);
                                ui.end_row();
                            }
                        });
                });
                if rows.is_empty() {
                    widgets::help(ui, "No shortcuts match.");
                }
            });
        if !open {
            self.sc_ed.open = false;
            self.sc_ed.learn = None;
            self.sc_ed.learn_all = false;
            self.sc_ed.last = None;
        }
        for c in cmds {
            self.exec(c);
        }
        if export {
            self.sc_export();
        }
        if import {
            self.sc_import();
        }
    }

    fn sc_row(&mut self, ui: &mut egui::Ui, row: &Shortcut, clash: bool, cmds: &mut Vec<Command>) {
        let mut s = row.clone();
        ui.label(src_name(src_of(&s.source)));
        ui.horizontal(|ui| {
            if clash {
                ui.label(egui::RichText::new("!").color(theme::DANGER));
            }
            match &mut s.source {
                ControlSource::Key { code } => {
                    widgets::field_text(ui, code, Width::Sm, None);
                }
                ControlSource::MidiNote { channel, note, .. } => {
                    midi_fields(ui, channel, "N", note);
                }
                ControlSource::MidiCc { channel, cc, .. } => {
                    midi_fields(ui, channel, "CC", cc);
                }
                ControlSource::Osc { address } => {
                    widgets::field_text(ui, address, Width::Md, None);
                }
                ControlSource::Dmx { universe, channel } => {
                    widgets::field_int(ui, universe, 0..=32767, "U", "");
                    widgets::field_int(ui, channel, 0..=512, "Ch", "");
                }
            }
            if clash {
                widgets::help(ui, &input_label(&s.source));
            }
        });
        widgets::field_text(ui, &mut s.target, Width::Lg, None);
        egui::ComboBox::from_id_salt(("sc_mode", s.id))
            .selected_text(mode_name(&s.mode))
            .show_ui(ui, |ui| {
                for m in [
                    ShortcutMode::Trigger,
                    ShortcutMode::Toggle,
                    ShortcutMode::Absolute,
                    ShortcutMode::Relative,
                ] {
                    let l = mode_name(&m);
                    ui.selectable_value(&mut s.mode, m, l);
                }
            });
        let (mut lo, mut hi) = (s.range.0 as f32, s.range.1 as f32);
        if widgets::field_num(ui, &mut lo, -1.0e6..=1.0e6, 0.01).changed() {
            s.range.0 = f64::from(lo);
        }
        if widgets::field_num(ui, &mut hi, -1.0e6..=1.0e6, 0.01).changed() {
            s.range.1 = f64::from(hi);
        }
        widgets::toggle(ui, &mut s.invert, "");
        ui.horizontal(|ui| {
            let armed = self.sc_ed.learn == Some(row.id);
            let kind = if armed {
                Kind::Primary
            } else {
                Kind::Secondary
            };
            if widgets::btn(ui, if armed { "Listening..." } else { "Learn" }, kind).clicked() {
                self.sc_ed.learn = if armed { None } else { Some(row.id) };
                self.sc_ed.learn_all = false;
                self.sc_ed.last = None;
            }
            if widgets::btn(ui, "Duplicate", Kind::Secondary).clicked() {
                let mut d = row.clone();
                d.id = Uuid::new_v4();
                let index = self
                    .project
                    .shortcuts
                    .iter()
                    .position(|x| x.id == row.id)
                    .map_or(self.project.shortcuts.len(), |i| i + 1);
                cmds.push(Command::AddShortcut { shortcut: d, index });
            }
            if widgets::btn(ui, "Delete", Kind::Danger).clicked() {
                cmds.push(Command::RemoveShortcut { id: row.id });
            }
        });
        if s != *row {
            cmds.push(Command::UpdateShortcut { shortcut: s });
        }
    }
}

fn midi_fields(ui: &mut egui::Ui, channel: &mut u8, kind: &str, num: &mut u8) {
    let mut ch = channel.saturating_add(1);
    if widgets::field_int(ui, &mut ch, 1..=16, "ch", "").changed() {
        *channel = ch.clamp(1, 16) - 1;
    }
    widgets::field_int(ui, num, 0..=127, kind, "");
}
