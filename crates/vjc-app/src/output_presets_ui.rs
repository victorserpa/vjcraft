//! Output presets in the Output setup window: save, load, rename, delete, import and
//! export whole output setups as files in the user folder, apart from the project.
//! File work is in `vjc_io::output_presets`; loading is the undoable `SetOutputSetup`.

use crate::widgets::{Kind, btn, btn_if};
use crate::{State, theme};
use eframe::egui;
use std::path::Path;
use vjc_core::Command;
use vjc_io::output_presets as presets;

impl State {
    /// Presets in the folder, listed once per opening of the window and after each change.
    fn preset_names(&mut self) -> &[String] {
        if self.preset_names.is_none() {
            let names = self.presets_dir.as_deref().map(presets::list);
            self.preset_names = Some(names.unwrap_or_default());
        }
        self.preset_names.as_deref().unwrap_or_default()
    }

    fn preset_done(&mut self, msg: String) {
        self.preset_names = None;
        self.preset_confirm_delete = false;
        self.status = msg;
    }

    fn preset_failed(&mut self, what: &str, e: presets::Error) {
        self.status = format!("Preset {what} failed: {e}");
    }

    /// Saves the current output setup as the preset `name`.
    pub(crate) fn preset_save(&mut self, name: &str) {
        let Some(dir) = self.presets_dir.clone() else {
            return;
        };
        match presets::save(&dir, name, &self.project.output) {
            Ok(_) => {
                let name = presets::clean_name(name).unwrap_or_default();
                self.preset_sel = Some(name.clone());
                self.preset_done(format!("Saved preset \"{name}\""));
            }
            Err(e) => self.preset_failed("save", e),
        }
    }

    /// Replaces the output setup with the preset `name`. A screen on a display this
    /// machine lacks becomes a window; the warnings stay visible in the window.
    pub(crate) fn preset_load(&mut self, name: &str) {
        let Some(dir) = self.presets_dir.clone() else {
            return;
        };
        let preset = match presets::load(&dir, name) {
            Ok(p) => p,
            Err(e) => return self.preset_failed("load", e),
        };
        let monitors: Vec<String> = self.monitors.iter().map(|m| m.id.to_string()).collect();
        let c = &self.project.composition;
        let layers: Vec<_> = c.layers.iter().map(|l| l.id).collect();
        let groups: Vec<_> = c.groups.iter().map(|g| g.id).collect();
        let env = presets::Env {
            monitors: &monitors,
            layers: &layers,
            groups: &groups,
        };
        let (screens, warnings) = presets::instantiate(&preset, env);
        let first = screens.first().map(|s| s.id);
        if self.exec_ok(Command::SetOutputSetup { screens }) {
            self.sel_screen = first;
            self.sel_slice = None;
            self.sel_slices.clear();
            self.sel_pt_in = None;
            self.sel_pt_out = None;
            self.sel_mask_pt = None;
            self.edit_mask = false;
            self.closed_screens.clear();
            self.test_cards.clear();
            let n = warnings.len();
            self.preset_warnings = warnings;
            self.preset_done(if n == 0 {
                format!("Loaded preset \"{}\"", preset.name)
            } else {
                format!("Loaded preset \"{}\" with {n} warning(s)", preset.name)
            });
        }
    }

    pub(crate) fn preset_rename(&mut self, old: &str, new: &str) {
        let Some(dir) = self.presets_dir.clone() else {
            return;
        };
        match presets::rename(&dir, old, new) {
            Ok(()) => {
                self.preset_sel = presets::clean_name(new);
                self.preset_done(format!("Renamed preset to \"{}\"", new.trim()));
            }
            Err(e) => self.preset_failed("rename", e),
        }
    }

    pub(crate) fn preset_delete(&mut self, name: &str) {
        let Some(dir) = self.presets_dir.clone() else {
            return;
        };
        match presets::delete(&dir, name) {
            Ok(()) => {
                self.preset_sel = None;
                self.preset_done(format!("Deleted preset \"{name}\""));
            }
            Err(e) => self.preset_failed("delete", e),
        }
    }

    pub(crate) fn preset_export(&mut self, name: &str, dest: &Path) {
        let Some(dir) = self.presets_dir.clone() else {
            return;
        };
        match presets::export(&dir, name, dest) {
            Ok(()) => self.status = format!("Exported preset to {}", dest.display()),
            Err(e) => self.preset_failed("export", e),
        }
    }

    pub(crate) fn preset_import(&mut self, src: &Path) {
        let Some(dir) = self.presets_dir.clone() else {
            return;
        };
        match presets::import(&dir, src) {
            Ok(name) => {
                self.preset_sel = Some(name.clone());
                self.preset_done(format!("Imported preset \"{name}\""));
            }
            Err(e) => self.preset_failed("import", e),
        }
    }

    /// The preset row at the top of the Output setup window.
    pub(crate) fn presets_row(&mut self, ui: &mut egui::Ui) {
        if self.presets_dir.is_none() {
            return;
        }
        let names: Vec<String> = self.preset_names().to_vec();
        let sel = self.preset_sel.clone().filter(|s| names.contains(s));
        ui.horizontal_wrapped(|ui| {
            ui.strong("Preset");
            egui::ComboBox::from_id_salt("out-preset")
                .selected_text(sel.clone().unwrap_or_else(|| "(none)".into()))
                .show_ui(ui, |ui| {
                    for n in &names {
                        if ui.selectable_label(sel.as_ref() == Some(n), n).clicked() {
                            self.preset_sel = Some(n.clone());
                            self.preset_text = n.clone();
                            self.preset_confirm_delete = false;
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.preset_text)
                    .hint_text("name")
                    .desired_width(120.0),
            );
            let text = self.preset_text.clone();
            let valid = presets::clean_name(&text).is_some();
            if btn_if(ui, valid, "Save preset", Kind::Primary).clicked() {
                self.preset_save(&text);
            }
            let Some(cur) = sel else {
                btn_if(ui, false, "Load preset", Kind::Secondary);
                if btn(ui, "Import preset...", Kind::Secondary).clicked() {
                    self.preset_import_dialog();
                }
                return;
            };
            if btn(ui, "Load preset", Kind::Secondary).clicked() {
                self.preset_load(&cur);
            }
            let renamable = valid && text.trim() != cur;
            if btn_if(ui, renamable, "Rename preset", Kind::Secondary).clicked() {
                self.preset_rename(&cur, &text);
            }
            let label = if self.preset_confirm_delete {
                "Confirm delete"
            } else {
                "Delete preset"
            };
            let kind = if self.preset_confirm_delete {
                Kind::Danger
            } else {
                Kind::Secondary
            };
            if btn(ui, label, kind).clicked() {
                if self.preset_confirm_delete {
                    self.preset_delete(&cur);
                } else {
                    self.preset_confirm_delete = true;
                }
            }
            if btn(ui, "Import preset...", Kind::Secondary).clicked() {
                self.preset_import_dialog();
            }
            if btn(ui, "Export preset...", Kind::Secondary).clicked()
                && let Some(dest) = rfd::FileDialog::new()
                    .add_filter("Output preset", &[presets::EXT])
                    .set_file_name(format!("{cur}.{}", presets::EXT))
                    .save_file()
            {
                self.preset_export(&cur, &dest);
            }
        });
        if !self.preset_warnings.is_empty() {
            let mut dismiss = false;
            for w in &self.preset_warnings {
                ui.colored_label(theme::DANGER, w);
            }
            ui.horizontal(|ui| dismiss = btn(ui, "Dismiss warnings", Kind::Secondary).clicked());
            if dismiss {
                self.preset_warnings.clear();
            }
        }
        ui.separator();
    }

    fn preset_import_dialog(&mut self) {
        if let Some(src) = rfd::FileDialog::new()
            .add_filter("Output preset", &[presets::EXT])
            .pick_file()
        {
            self.preset_import(&src);
        }
    }
}
