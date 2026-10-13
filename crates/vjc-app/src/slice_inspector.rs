//! Precise slice editing in the Output setup: numeric X/Y/W/H and rotation in pixels for
//! the input selection and the output mesh (with simple math in the fields), quarter
//! turns, flips, "match output shape", duplicate, move to another screen, and the helpers
//! the canvases use for arrow-key nudging and snapping to guides.
//! The maths lives in `vjc_core::slice_geom`; every edit is one undoable Command.

use crate::State;
use eframe::egui;
use vjc_core::Command;
use vjc_core::slice_geom::{self, Field, Op, Side};
use vjc_model::{Screen, Slice, SliceSource};

/// Distance, in canvas pixels, at which a dragged point jumps to a guide.
const GUIDE_PX: f32 = 8.0;

/// Text of a pixel value: whole numbers without decimals.
fn show(v: f32) -> String {
    if (v - v.round()).abs() < 0.005 {
        format!("{:.0}", v.round())
    } else {
        format!("{v:.2}")
    }
}

/// A text field that evaluates simple math (`/3`, `*2`, `+10`, `-10`, `1920/4`) against its
/// current value when it loses focus. Returns the new value when it changed.
fn math_field(ui: &mut egui::Ui, id: egui::Id, value: f32) -> Option<f32> {
    let mut text = ui
        .data_mut(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| show(value));
    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(58.0));
    let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let mut out = None;
    // An untouched field shows a rounded value; leaving it must not write that back.
    if r.lost_focus() && !escaped && text != show(value) {
        match slice_geom::eval_field(&text, value) {
            Some(v) if (v - value).abs() > 1e-4 => out = Some(v),
            _ => {}
        }
    }
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
    } else {
        ui.data_mut(|d| d.remove_temp::<String>(id));
    }
    out
}

/// Arrow keys over `rect` as a move in pixels: 1 px, 10 px with Shift. `None` when the
/// pointer is elsewhere or no arrow was pressed.
pub(crate) fn arrow_move(ui: &egui::Ui, rect: egui::Rect) -> Option<(f32, f32)> {
    // Arrows typed into a text field (inspector, preset name) are not a nudge.
    if ui.memory(|m| m.focused().is_some()) {
        return None;
    }
    if !ui.rect_contains_pointer(rect) {
        return None;
    }
    let (dx, dy) = ui.input(|inp| {
        let step = if inp.modifiers.shift { 10.0 } else { 1.0 };
        let k = |a, b| (inp.key_pressed(b) as i32 - inp.key_pressed(a) as i32) as f32 * step;
        (
            k(egui::Key::ArrowLeft, egui::Key::ArrowRight),
            k(egui::Key::ArrowUp, egui::Key::ArrowDown),
        )
    });
    (dx != 0.0 || dy != 0.0).then_some((dx, dy))
}

/// Snaps a point being dragged to the edges and centres of the space (`space` units) and
/// of `others`, unless Alt is held. Only while the pointer button is down, so arrow-key
/// nudges are never pulled back onto a guide. `scale` = space units per canvas pixel.
pub(crate) fn snap_dragged(
    ui: &egui::Ui,
    p: (f32, f32),
    space: (f32, f32),
    others: &[slice_geom::Rect],
    scale: f32,
) -> (f32, f32) {
    let (alt, down) = ui.input(|i| (i.modifiers.alt, i.pointer.primary_down()));
    if alt || !down {
        return p;
    }
    let (xs, ys) = slice_geom::guides(space, others);
    slice_geom::snap_to_guides(p, &xs, &ys, GUIDE_PX * scale)
}

impl State {
    /// Moves the whole side of the selected slice by arrow keys (pixels).
    pub(crate) fn nudge_slice(
        &self,
        scr: &Screen,
        sl: &Slice,
        side: Side,
        by: (f32, f32),
    ) -> Option<Command> {
        let c = &self.project.composition;
        slice_geom::edit(scr, sl, side, (c.width, c.height), Op::Nudge(by.0, by.1))
    }

    /// Dropdown of what the slice shows: composition, preview, one layer or one group.
    fn source_picker(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        cmds: &mut Vec<Command>,
    ) {
        let comp = &self.project.composition;
        let current = match sl.source {
            SliceSource::Composition => "Composition".to_string(),
            SliceSource::Preview => "Preview".to_string(),
            SliceSource::Layer(id) => comp
                .layers
                .iter()
                .find(|l| l.id == id)
                .map_or("Layer (missing)".to_string(), |l| {
                    format!("Layer: {}", l.name)
                }),
            SliceSource::Group(id) => comp
                .groups
                .iter()
                .find(|g| g.id == id)
                .map_or("Group (missing)".to_string(), |g| {
                    format!("Group: {}", g.name)
                }),
        };
        let mut choices = vec![
            ("Composition".to_string(), SliceSource::Composition),
            ("Preview".to_string(), SliceSource::Preview),
        ];
        choices.extend(
            comp.layers
                .iter()
                .map(|l| (format!("Layer: {}", l.name), SliceSource::Layer(l.id))),
        );
        choices.extend(
            comp.groups
                .iter()
                .map(|g| (format!("Group: {}", g.name), SliceSource::Group(g.id))),
        );
        egui::ComboBox::from_label("Source")
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (label, source) in choices {
                    if ui.selectable_label(sl.source == source, label).clicked()
                        && sl.source != source
                    {
                        cmds.push(Command::SetSliceSource {
                            screen: scr.id,
                            slice: sl.id,
                            source,
                        });
                    }
                }
            });
    }

    pub(crate) fn slice_inspector(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        cmds: &mut Vec<Command>,
    ) {
        let c = &self.project.composition;
        let comp = (c.width, c.height);
        let side = self.geom_side;
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong("Slice (px)");
            ui.selectable_value(&mut self.geom_side, Side::Input, "Input");
            ui.selectable_value(&mut self.geom_side, Side::Output, "Output");
            ui.weak("fields take math: /3  *2  +10  -10");
        });
        if let Some(v) = slice_geom::readout(sl, side, comp) {
            ui.horizontal(|ui| {
                for (n, (label, f)) in [
                    ("X", Field::X),
                    ("Y", Field::Y),
                    ("W", Field::W),
                    ("H", Field::H),
                    ("Rot", Field::Rot),
                ]
                .into_iter()
                .enumerate()
                {
                    ui.label(label);
                    let id = egui::Id::new(("slice-field", sl.id, side == Side::Input, n));
                    if let Some(new) = math_field(ui, id, v[n])
                        && let Some(cmd) = slice_geom::edit(scr, sl, side, comp, Op::Set(f, new))
                    {
                        cmds.push(cmd);
                    }
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            let mut ops = vec![];
            for (label, op) in [
                ("Rotate 90", Op::Rotate(1)),
                ("Rotate 180", Op::Rotate(2)),
                ("Rotate 270", Op::Rotate(3)),
                ("Flip H", Op::FlipH),
                ("Flip V", Op::FlipV),
            ] {
                if ui.button(label).clicked() {
                    ops.push(op);
                }
            }
            if ui
                .button("Match output shape")
                .on_hover_text("Copy the output rectangle to the input: 1:1 pixel map")
                .clicked()
                && let Some(cmd) = slice_geom::edit(scr, sl, Side::Input, comp, Op::MatchOutput)
            {
                cmds.push(cmd);
            }
            for op in ops {
                if let Some(cmd) = slice_geom::edit(scr, sl, side, comp, op) {
                    cmds.push(cmd);
                }
            }
        });
        self.source_picker(ui, scr, sl, cmds);
        ui.horizontal(|ui| {
            if ui.button("Duplicate slice").clicked() {
                let mut copy = sl.clone();
                copy.id = uuid::Uuid::new_v4();
                let at = scr
                    .slices
                    .iter()
                    .position(|s| s.id == sl.id)
                    .map_or(scr.slices.len(), |i| i + 1);
                self.sel_slice = Some(copy.id);
                cmds.push(Command::AddSlice {
                    screen: scr.id,
                    slice: copy,
                    index: at,
                });
            }
            let others: Vec<(uuid::Uuid, String, usize)> = self
                .project
                .output
                .screens
                .iter()
                .filter(|s| s.id != scr.id)
                .map(|s| (s.id, s.name.clone(), s.slices.len()))
                .collect();
            ui.add_enabled_ui(!others.is_empty(), |ui| {
                egui::ComboBox::from_label("Move to screen")
                    .selected_text("Choose")
                    .show_ui(ui, |ui| {
                        for (id, name, n) in &others {
                            if ui.selectable_label(false, name).clicked() {
                                self.sel_screen = Some(*id);
                                self.sel_slice = Some(sl.id);
                                cmds.push(Command::MoveSlice {
                                    from: scr.id,
                                    to: *id,
                                    slice: sl.id,
                                    index: *n,
                                });
                            }
                        }
                    });
            });
        });
    }
}
