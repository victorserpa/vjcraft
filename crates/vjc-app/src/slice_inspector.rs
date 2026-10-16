//! Helpers of the Output setup canvas for precise slice editing: arrow-key nudging, snapping
//! to guides, and moving the selected slices together. The numeric fields are in
//! `output_props`.
//! The maths lives in `vjc_core::slice_geom`; every edit is one undoable Command.

use crate::State;
use crate::theme::SP_4;
use eframe::egui;
use vjc_core::Command;
use vjc_core::slice_geom::{self, Op, Side};
use vjc_model::{Screen, Slice};

/// Distance, in canvas pixels, at which a dragged point jumps to a guide.
pub(crate) const GUIDE_PX: f32 = SP_4;

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
}
