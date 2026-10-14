//! Precise slice editing in the Output setup: numeric X/Y/W/H and rotation in pixels for
//! the input selection and the output mesh (with simple math in the fields), quarter
//! turns, flips, "match output shape", duplicate, move to another screen, and the helpers
//! the canvases use for arrow-key nudging and snapping to guides.
//! The maths lives in `vjc_core::slice_geom`; every edit is one undoable Command.

use crate::State;
use crate::theme::SP_4;
use crate::widgets::{self, Kind};
use eframe::egui;
use vjc_core::Command;
use vjc_core::slice_geom::{self, Field, Op, Side};
use vjc_model::{Screen, Slice, SliceSource};

/// Distance, in canvas pixels, at which a dragged point jumps to a guide.
pub(crate) const GUIDE_PX: f32 = SP_4;

/// Range the pixel fields accept (far beyond any real screen).
const PX_RANGE: std::ops::RangeInclusive<f32> = -1.0e6..=1.0e6;

/// A numeric field that evaluates simple math (`/3`, `*2`, `+10`, `-10`, `1920/4`) against
/// its current value when typed into. Returns the new value when it changed.
fn math_field(ui: &mut egui::Ui, value: f32) -> Option<f32> {
    let mut v = value;
    let r = widgets::field_num(ui, &mut v, PX_RANGE, 0.0); // typed only: one undo step per edit
    (r.changed() && (v - value).abs() > 1e-4).then_some(v)
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
    /// Ids of the slices an edit applies to: the multi-selection of this screen, or just
    /// `sl` (the primary one, always included).
    pub(crate) fn selected_slices(&self, scr: &Screen, sl: &Slice) -> Vec<uuid::Uuid> {
        scr.slices
            .iter()
            .filter(|s| s.id == sl.id || self.sel_slices.contains(&s.id))
            .map(|s| s.id)
            .collect()
    }

    /// Moves the given side of every selected slice by `by` pixels: one command for one
    /// slice, a `Batch` (one undo step) for several.
    pub(crate) fn nudge_selected(
        &self,
        scr: &Screen,
        sl: &Slice,
        side: Side,
        by: (f32, f32),
    ) -> Option<Command> {
        let c = &self.project.composition;
        let ids = self.selected_slices(scr, sl);
        let mut cmds: Vec<Command> = scr
            .slices
            .iter()
            .filter(|s| ids.contains(&s.id))
            .filter_map(|s| {
                slice_geom::edit(scr, s, side, (c.width, c.height), Op::Nudge(by.0, by.1))
            })
            .collect();
        match cmds.len() {
            0 => None,
            1 => cmds.pop(),
            _ => Some(Command::Batch { commands: cmds }),
        }
    }

    /// Guide lines in pixels of `side`: the screen / composition edges and centres plus the
    /// bounding boxes of the slices that are not being moved.
    fn side_guides(&self, scr: &Screen, moving: &[uuid::Uuid], side: Side) -> (Vec<f32>, Vec<f32>) {
        let c = &self.project.composition;
        let comp = (c.width, c.height);
        let space = match side {
            Side::Input => (comp.0 as f32, comp.1 as f32),
            Side::Output => (scr.width as f32, scr.height as f32),
        };
        let others: Vec<_> = scr
            .slices
            .iter()
            .filter(|s| !moving.contains(&s.id))
            .filter_map(|s| slice_geom::bbox(&slice_geom::side_points(s, side, comp)))
            .collect();
        slice_geom::guides(space, &others)
    }

    /// Arrow-key move of the selected slices; it stops on a guide it would cross (Alt = free).
    pub(crate) fn arrow_nudge(
        &self,
        ui: &egui::Ui,
        scr: &Screen,
        sl: &Slice,
        side: Side,
        by: (f32, f32),
    ) -> Option<Command> {
        let c = &self.project.composition;
        let mut by = by;
        if !ui.input(|i| i.modifiers.alt)
            && let Some(b) =
                slice_geom::bbox(&slice_geom::side_points(sl, side, (c.width, c.height)))
        {
            let ids = self.selected_slices(scr, sl);
            let (xs, ys) = self.side_guides(scr, &ids, side);
            by = slice_geom::stop_at_guides(b, by, &xs, &ys);
        }
        self.nudge_selected(scr, sl, side, by)
    }

    /// Body drag of the selected slices: `start` is the primary slice's rectangle when the
    /// drag began and `acc` the pointer offset since, in pixels of `side`. Snaps the
    /// rectangle's edges and centre to guides within `thr` pixels unless Alt is held.
    pub(crate) fn drag_body(
        &self,
        ui: &egui::Ui,
        scr: &Screen,
        sl: &Slice,
        side: Side,
        (start, acc): (slice_geom::Rect, (f32, f32)),
        thr: f32,
    ) -> Option<Command> {
        let c = &self.project.composition;
        let want = slice_geom::Rect {
            x: start.x + acc.0,
            y: start.y + acc.1,
            ..start
        };
        let sh = if ui.input(|i| i.modifiers.alt) {
            (0.0, 0.0)
        } else {
            let ids = self.selected_slices(scr, sl);
            let (xs, ys) = self.side_guides(scr, &ids, side);
            slice_geom::snap_rect(want, &xs, &ys, thr)
        };
        let cur = slice_geom::bbox(&slice_geom::side_points(sl, side, (c.width, c.height)))?;
        self.nudge_selected(
            scr,
            sl,
            side,
            (want.x + sh.0 - cur.x, want.y + sh.1 - cur.y),
        )
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
        widgets::section(ui, "Slice (px)", |ui| {
            ui.horizontal(|ui| {
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
                        if let Some(new) = math_field(ui, v[n])
                            && let Some(cmd) =
                                slice_geom::edit(scr, sl, side, comp, Op::Set(f, new))
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
                    if widgets::btn(ui, label, Kind::Secondary).clicked() {
                        ops.push(op);
                    }
                }
                if widgets::btn(ui, "Match output shape", Kind::Secondary)
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
                if widgets::btn(ui, "Duplicate slice", Kind::Secondary).clicked() {
                    let mut copy = sl.clone();
                    copy.id = uuid::Uuid::new_v4();
                    let at = scr
                        .slices
                        .iter()
                        .position(|s| s.id == sl.id)
                        .map_or(scr.slices.len(), |i| i + 1);
                    cmds.push(Command::AddSlice {
                        screen: scr.id,
                        slice: copy,
                        index: at,
                    });
                }
                if widgets::btn(ui, "Mirror", Kind::Secondary)
                    .on_hover_text("Duplicate and flip the copy horizontally (one undo step)")
                    .clicked()
                {
                    cmds.push(slice_geom::mirror(
                        scr,
                        sl,
                        side,
                        comp,
                        uuid::Uuid::new_v4(),
                    ));
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
        });
    }
}
