//! Preferences window (File > Preferences..., Cmd+,). Edits apply live;
//! the file is written when the window closes.

use crate::{PrefsTab, State};
use eframe::egui;
use vjc_model::FitMode;

impl State {
    pub(crate) fn prefs_window(&mut self, ctx: &egui::Context) {
        if !self.prefs_open {
            return;
        }
        let mut open = true;
        let mut done = false;
        let before = self.prefs.clone();
        egui::Window::new("Preferences")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (t, l) in [
                        (PrefsTab::General, "General"),
                        (PrefsTab::Audio, "Audio"),
                        (PrefsTab::Video, "Video"),
                        (PrefsTab::Control, "Control"),
                    ] {
                        ui.selectable_value(&mut self.prefs_tab, t, l);
                    }
                });
                ui.separator();
                let p = &mut self.prefs;
                match self.prefs_tab {
                    PrefsTab::General => {
                        ui.checkbox(&mut p.general.autosave, "Autosave recovery file");
                        ui.add_enabled_ui(p.general.autosave, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut p.general.autosave_secs)
                                    .range(5..=3600)
                                    .suffix(" s"),
                            );
                        });
                        ui.checkbox(
                            &mut p.general.open_last_project,
                            "Open last project at startup",
                        );
                        ui.horizontal(|ui| {
                            ui.label("Browser start folder");
                            let mut t = p
                                .general
                                .browser_folder
                                .as_ref()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            if ui.text_edit_singleline(&mut t).changed() {
                                p.general.browser_folder =
                                    (!t.trim().is_empty()).then(|| t.trim().into());
                            }
                            if ui.button("Choose...").clicked()
                                && let Some(d) = rfd::FileDialog::new().pick_folder()
                            {
                                p.general.browser_folder = Some(d);
                            }
                        });
                        ui.weak("Empty = project folder, else ~/Movies. Applies on next launch.");
                    }
                    PrefsTab::Audio => {
                        // TODO(#19): list output devices with cpal (not a dependency yet).
                        ui.horizontal(|ui| {
                            ui.label("Output device");
                            let mut t = p.audio.output_device.clone().unwrap_or_default();
                            if ui.text_edit_singleline(&mut t).changed() {
                                p.audio.output_device =
                                    (!t.trim().is_empty()).then(|| t.trim().into());
                            }
                        });
                        ui.weak("Placeholder: saved, not used by the engine yet.");
                    }
                    PrefsTab::Video => {
                        ui.label("New composition");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut p.video.width).range(1..=16384));
                            ui.label("x");
                            ui.add(egui::DragValue::new(&mut p.video.height).range(1..=16384));
                            ui.add(
                                egui::DragValue::new(&mut p.video.fps)
                                    .range(1.0..=240.0)
                                    .suffix(" fps"),
                            );
                        });
                        egui::ComboBox::from_label("Default clip fit")
                            .selected_text(p.video.fit.label())
                            .show_ui(ui, |ui| {
                                for f in FitMode::ALL {
                                    ui.selectable_value(&mut p.video.fit, f, f.label());
                                }
                            });
                        ui.checkbox(&mut p.video.vsync, "VSync (applies on next launch)");
                    }
                    PrefsTab::Control => {
                        ui.checkbox(
                            &mut p.control.default_shortcuts,
                            "New projects get the default shortcut preset",
                        );
                        ui.checkbox(
                            &mut p.control.offer_shortcuts,
                            "Offer the preset when opening a project without shortcuts",
                        );
                    }
                }
                ui.separator();
                if ui.button("Close").clicked() {
                    done = true;
                }
            });
        if self.prefs != before {
            self.prefs_dirty = true;
        }
        if !open || done {
            self.prefs_open = false;
            if self.prefs_dirty {
                self.save_prefs();
            }
        }
    }
}
