use std::collections::HashMap;
use vjc_model::{LayerId, Project};

#[derive(Default)]
struct State {
    project: Option<Project>,
    warnings: Vec<String>,
    status: String,
    /// Active (playing) clip column per layer. No video yet: this only marks the cell.
    active: HashMap<LayerId, u32>,
}

impl State {
    fn import(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Composition", &["avc"]).pick_file() else {
            return;
        };
        match vjc_avc::import_file(&path) {
            Ok(i) => {
                self.status = format!("Imported {}", path.display());
                self.warnings = i.warnings;
                self.project = Some(i.project);
                self.active.clear();
            }
            Err(e) => self.status = format!("Import failed: {e}"),
        }
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Import .avc...").clicked() {
                    ui.close();
                    self.import();
                }
            });
        });
        ui.label(&self.status);
        if !self.warnings.is_empty() {
            ui.collapsing(format!("{} import warnings", self.warnings.len()), |ui| {
                for w in &self.warnings {
                    ui.label(w);
                }
            });
        }
        ui.separator();
        let Some(p) = &self.project else {
            ui.label("No composition loaded. Use File > Import .avc...");
            return;
        };
        let c = &p.composition;
        let deck = c.decks.iter().find(|d| d.id == c.current_deck);
        egui::ScrollArea::both().show(ui, |ui| {
            egui::Grid::new("grid").spacing([4.0, 4.0]).show(ui, |ui| {
                ui.label("");
                for col in &c.columns {
                    ui.label(&col.name);
                }
                ui.end_row();
                for layer in &c.layers {
                    ui.label(&layer.name);
                    for ci in 0..c.columns.len() as u32 {
                        let clip = deck.and_then(|d| d.clip_at(layer.id, ci));
                        let on = self.active.get(&layer.id) == Some(&ci);
                        let text = clip.map(|k| k.name.as_str()).unwrap_or("-");
                        let btn = egui::Button::new(text).selected(on).min_size(egui::vec2(90.0, 24.0));
                        if ui.add_enabled(clip.is_some(), btn).clicked() {
                            self.active.insert(layer.id, ci);
                        }
                    }
                    ui.end_row();
                }
            });
        });
    }
}

impl eframe::App for State {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        State::ui(self, ui);
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "VJCraft",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(State::default()))),
    )
}
