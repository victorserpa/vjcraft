mod compositor;

use compositor::{Compositor, LayerDraw};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;
use vjc_media::{Frame, Player};
use vjc_model::{BlendMode, Clip, Column, Layer, LayerId, Project, Source, Track, Transition, Transport};

struct State {
    project: Project,
    warnings: Vec<String>,
    status: String,
    /// Playing clip per layer: (column, player). One clip per layer.
    players: HashMap<LayerId, (u32, Player)>,
    comp: Option<Compositor>,
    comp_tex: Option<egui::TextureId>,
    /// Output window monitor index (0-based) while open.
    output: Option<usize>,
    /// Cell rects from the last frame, for hit-testing file drops.
    cells: Vec<CellInfo>,
    last_pointer: Option<egui::Pos2>,
    /// Cell selected by click; fallback target for drops.
    selected: Option<(LayerId, u32)>,
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
fn resolve_drop(cells: &[CellInfo], pointer: Option<egui::Pos2>, selected: Option<(LayerId, u32)>) -> Option<(LayerId, u32, DropWhere)> {
    if let Some(p) = pointer {
        if let Some(c) = cells.iter().find(|c| c.rect.contains(p)) {
            return Some((c.layer, c.col, DropWhere::Pointer));
        }
    }
    if let Some((l, col)) = selected {
        if cells.iter().any(|c| c.layer == l && c.col == col) {
            return Some((l, col, DropWhere::Selected));
        }
    }
    let bottom = cells.last()?.layer;
    let mut row = cells.iter().filter(|c| c.layer == bottom);
    if let Some(c) = row.clone().find(|c| c.empty) {
        return Some((c.layer, c.col, DropWhere::FirstEmpty));
    }
    row.next().map(|c| (c.layer, c.col, DropWhere::FirstCell))
}


fn new_layer(n: usize) -> Layer {
    Layer {
        id: Uuid::new_v4(),
        name: format!("Layer {n}"),
        track: Track::new(BlendMode::Normal),
        solo: false,
        bypass: false,
        transition: Transition { kind: "Cut".into(), duration_beats: None, duration_secs: 0.0 },
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
            c.columns.push(Column { id: Uuid::new_v4(), name: format!("{i}") });
        }
        State {
            project,
            warnings: vec![],
            status: "Drop video files on the grid cells.".into(),
            players: HashMap::new(),
            comp: None,
            comp_tex: None,
            output: None,
            cells: vec![],
            last_pointer: None,
            selected: None,
        }
    }

    fn import(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Composition", &["avc"]).pick_file() else {
            return;
        };
        match vjc_avc::import_file(&path) {
            Ok(i) => {
                self.status = format!("Imported {}", path.display());
                self.warnings = i.warnings;
                self.project = i.project;
                self.players.clear();
            }
            Err(e) => self.status = format!("Import failed: {e}"),
        }
    }

    fn clip_path(&self, layer: LayerId, col: u32) -> Option<PathBuf> {
        let c = &self.project.composition;
        let deck = c.decks.iter().find(|d| d.id == c.current_deck)?;
        match &deck.clip_at(layer, col)?.source {
            Source::File { path } => Some(path.clone()),
            _ => None,
        }
    }

    fn trigger(&mut self, layer: LayerId, col: u32) {
        match self.clip_path(layer, col) {
            Some(p) if p.exists() => {
                self.players.remove(&layer); // joins the old decode thread
                self.players.insert(layer, (col, Player::open(&p)));
            }
            Some(p) => self.status = format!("File not found: {}", p.display()),
            None => self.status = "Clip has no playable file source".into(),
        }
    }

    fn drop_file(&mut self, layer: LayerId, col: u32, path: PathBuf) {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let clip = Clip {
            id: Uuid::new_v4(),
            name,
            source: Source::File { path },
            transport: Transport::default(),
            track: Track::new(BlendMode::Normal),
            blend_override: None,
        };
        let c = &mut self.project.composition;
        let cur = c.current_deck;
        if let Some(d) = c.decks.iter_mut().find(|d| d.id == cur) {
            d.set_clip(layer, col, clip);
        }
        self.trigger(layer, col);
    }

    fn compose(&mut self, frame: &eframe::Frame) {
        let Some(rs) = frame.wgpu_render_state() else { return };
        let c = &self.project.composition;
        let (w, h) = (c.width.max(1), c.height.max(1));
        if self.comp.is_none() {
            let comp = Compositor::new(&rs.device, w, h);
            self.comp_tex = Some(rs.renderer.write().register_native_texture(&rs.device, &comp.target_view, wgpu::FilterMode::Linear));
            self.comp = Some(comp);
        }
        let comp = self.comp.as_mut().unwrap();
        if comp.resize(&rs.device, w, h) {
            if let Some(id) = self.comp_tex {
                rs.renderer.write().update_egui_texture_from_wgpu_texture(&rs.device, &comp.target_view, wgpu::FilterMode::Linear, id);
            }
        }
        // Layers are ordered top to bottom; draw bottom to top.
        let frames: Vec<(&Layer, Option<Frame>)> = c
            .layers
            .iter()
            .rev()
            .map(|l| (l, if l.bypass { None } else { self.players.get(&l.id).and_then(|(_, p)| p.latest_frame()) }))
            .collect();
        let draws: Vec<LayerDraw> = frames
            .iter()
            .map(|(l, f)| LayerDraw {
                id: l.id,
                opacity: match l.track.opacity.value {
                    vjc_model::Value::Number(n) => n as f32,
                    _ => 1.0,
                },
                blend: l.track.blend.clone(),
                frame: f.as_ref().map(|f| (f.width, f.height, f.data.as_slice(), f.seq)),
            })
            .collect();
        comp.render(&rs.device, &rs.queue, &draws);
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Import .avc...").clicked() {
                    ui.close();
                    self.import();
                }
            });
            ui.menu_button("Output", |ui| {
                for n in 1..=4usize {
                    if ui.button(format!("Fullscreen on monitor {n}")).clicked() {
                        ui.close();
                        self.output = Some(n - 1);
                    }
                }
                if ui.add_enabled(self.output.is_some(), egui::Button::new("Close output")).clicked() {
                    ui.close();
                    self.output = None;
                }
            });
            ui.separator();
            ui.label("BPM");
            ui.add(egui::DragValue::new(&mut self.project.composition.tempo.bpm).range(20.0..=300.0).speed(0.1));
            // TODO: BPM is fixed for M0; no clock/sync yet.
        });

        // File drops onto grid cells.
        self.last_pointer = ctx.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())).or(self.last_pointer);
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for path in dropped {
            match resolve_drop(&self.cells, self.last_pointer, self.selected) {
                Some((l, c, w)) => {
                    let how = match w {
                        DropWhere::Pointer => "under pointer",
                        DropWhere::Selected => "selected cell (no pointer position)",
                        DropWhere::FirstEmpty => "first empty cell of bottom layer",
                        DropWhere::FirstCell => "first cell of bottom layer",
                    };
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.drop_file(l, c, path);
                    self.status = format!("Dropped {name} on column {} ({how})", c + 1);
                }
                None => self.status = "No grid cells to drop on".into(),
            }
        }
        let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let drag_target = if dragging { resolve_drop(&self.cells, self.last_pointer, self.selected).map(|(l, c, _)| (l, c)) } else { None };
        let mut pick: Option<(LayerId, u32)> = None;

        ui.label(&self.status);
        if !self.warnings.is_empty() {
            ui.collapsing(format!("{} import warnings", self.warnings.len()), |ui| {
                for w in &self.warnings {
                    ui.label(w);
                }
            });
        }
        for (l, p) in self.players.values().map(|(_, p)| ((), p)) {
            let _ = l;
            if let Some(e) = p.error() {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("{}: {e}", p.path.display()));
            }
        }
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("Add layer").clicked() {
                let n = self.project.composition.layers.len() + 1;
                self.project.composition.layers.insert(0, new_layer(n));
            }
            if ui.button("Add column").clicked() {
                let n = self.project.composition.columns.len() + 1;
                self.project.composition.columns.push(Column { id: Uuid::new_v4(), name: n.to_string() });
            }
        });

        // Monitor (aspect-fit).
        if let Some(id) = self.comp_tex {
            let c = &self.project.composition;
            let aspect = c.width as f32 / c.height.max(1) as f32;
            let h = 220.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(h * aspect, h), egui::Sense::hover());
            ui.painter().image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        ui.separator();

        let mut trigger: Option<(LayerId, u32)> = None;
        let mut clear: Option<LayerId> = None;
        let mut cells = vec![];
        let ncols = self.project.composition.columns.len() as u32;
        let cur = self.project.composition.current_deck;
        let names: Vec<String> = self.project.composition.columns.iter().map(|c| c.name.clone()).collect();
        let deck_idx = self.project.composition.decks.iter().position(|d| d.id == cur);
        let comp = &mut self.project.composition;
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("grid").spacing([4.0, 4.0]).show(ui, |ui| {
                ui.label("");
                for n in &names {
                    ui.label(n);
                }
                ui.label("Opacity");
                ui.label("Blend");
                ui.label("");
                ui.end_row();
                for li in 0..comp.layers.len() {
                    let lid = comp.layers[li].id;
                    ui.label(&comp.layers[li].name);
                    for ci in 0..ncols {
                        let clip = deck_idx.and_then(|d| comp.decks[d].clip_at(lid, ci));
                        let on = self.players.get(&lid).map(|(c, _)| *c) == Some(ci);
                        let text = clip.map(|k| k.name.as_str()).unwrap_or("(drop / right-click)");
                        let btn = egui::Button::new(text).selected(on).min_size(egui::vec2(90.0, 24.0));
                        let r = ui.add(btn);
                        cells.push(CellInfo { rect: r.rect, layer: lid, col: ci, empty: clip.is_none() });
                        if drag_target == Some((lid, ci)) {
                            ui.painter().rect_stroke(r.rect, 3.0, egui::Stroke::new(2.5, egui::Color32::YELLOW), egui::StrokeKind::Outside);
                        } else if self.selected == Some((lid, ci)) {
                            ui.painter().rect_stroke(r.rect, 3.0, egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
                        }
                        if r.clicked() || r.secondary_clicked() {
                            self.selected = Some((lid, ci));
                        }
                        if r.clicked() && clip.is_some() {
                            trigger = Some((lid, ci));
                        }
                        if clip.is_none() && (r.secondary_clicked() || r.double_clicked()) {
                            pick = Some((lid, ci));
                        }
                    }
                    let layer = &mut comp.layers[li];
                    let mut op = match layer.track.opacity.value {
                        vjc_model::Value::Number(n) => n as f32,
                        _ => 1.0,
                    };
                    if ui.add(egui::Slider::new(&mut op, 0.0..=1.0).fixed_decimals(2)).changed() {
                        layer.track.opacity.set_number(op as f64);
                    }
                    let label = if layer.track.blend == BlendMode::Add { "Add" } else { "Normal" };
                    egui::ComboBox::from_id_salt(("blend", lid)).selected_text(label).width(70.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut layer.track.blend, BlendMode::Normal, "Normal");
                        ui.selectable_value(&mut layer.track.blend, BlendMode::Add, "Add");
                    });
                    if ui.button("Clear").clicked() {
                        clear = Some(lid);
                    }
                    ui.end_row();
                }
            });
        });
        self.cells = cells;
        if let Some((l, c)) = pick {
            if let Some(path) = rfd::FileDialog::new().add_filter("Video / image", &["mov", "mp4", "m4v", "mkv", "avi", "webm", "png", "jpg", "jpeg", "webp"]).pick_file() {
                self.drop_file(l, c, path);
            }
        }
        if let Some(l) = clear {
            self.players.remove(&l);
        }
        if let Some((l, c)) = trigger {
            self.trigger(l, c);
        }
    }

    fn output_window(&mut self, ctx: &egui::Context) {
        let Some(mon) = self.output else { return };
        let Some(id) = self.comp_tex else { return };
        let mut builder = egui::ViewportBuilder::default().with_title("VJCraft Output").with_fullscreen(true).with_decorations(false);
        builder.monitor = Some(mon);
        let mut close = false;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("vjc-output"), builder, |ui, _class| {
            let ctx = ui.ctx().clone();
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            ui.painter().image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            if ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.viewport().close_requested()) {
                close = true;
            }
        });
        if close {
            self.output = None;
        }
    }
}

impl eframe::App for State {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.compose(frame);
        State::ui(self, ui);
        let ctx = ui.ctx().clone();
        self.output_window(&ctx);
        // Video is live: keep repainting.
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: f32, layer: LayerId, col: u32, empty: bool) -> CellInfo {
        CellInfo { rect: egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(10.0, 10.0)), layer, col, empty }
    }

    #[test]
    fn hit_test_and_fallbacks() {
        let (top, bot) = (Uuid::new_v4(), Uuid::new_v4());
        let cells = [cell(0.0, top, 0, true), cell(20.0, top, 1, true), cell(0.0, bot, 0, false), cell(20.0, bot, 1, true)];
        let hit = resolve_drop(&cells, Some(egui::pos2(22.0, 5.0)), None);
        assert_eq!(hit, Some((top, 1, DropWhere::Pointer)));
        let miss = Some(egui::pos2(500.0, 500.0));
        assert_eq!(resolve_drop(&cells, miss, Some((bot, 0))), Some((bot, 0, DropWhere::Selected)));
        assert_eq!(resolve_drop(&cells, None, None), Some((bot, 1, DropWhere::FirstEmpty)));
        let full = [cell(0.0, bot, 0, false), cell(20.0, bot, 1, false)];
        assert_eq!(resolve_drop(&full, None, None), Some((bot, 0, DropWhere::FirstCell)));
        assert_eq!(resolve_drop(&[], None, None), None);
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "VJCraft",
        eframe::NativeOptions::default(),
        Box::new(|_cc| {
            let mut s = State::new();
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
