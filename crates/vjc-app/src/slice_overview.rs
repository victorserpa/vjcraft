//! Slice overview of the Output setup window: every slice of the screen drawn as its output
//! rectangle. Click selects (Shift / Cmd adds), a drag on the background selects the slices
//! the box touches, Alt + drag draws a lasso around the slice centres, and a double-click
//! previews the slice's source.

use crate::theme::OUTPUT_LIST_W;
use crate::{PreviewTarget, State};
use eframe::egui;
use uuid::Uuid;
use vjc_core::slice_geom::{self, Rect};
use vjc_model::{Screen, SliceSource};

/// Where the overview canvas was drawn last frame (read by tests).
pub(crate) fn rect_id() -> egui::Id {
    egui::Id::new("slice-overview-rect")
}

/// Selection gesture in progress, kept in egui memory between frames.
#[derive(Clone, Default)]
struct Gesture {
    /// Alt held when the drag began: a lasso instead of a box.
    lasso: bool,
    /// Pointer path (canvas points).
    path: Vec<egui::Pos2>,
}

impl State {
    /// Makes `ids` the selection (primary = first); with `add` they join the current one.
    pub(crate) fn select_slices(&mut self, scr: &Screen, ids: &[Uuid], add: bool) {
        let primary = self.sel_slice.or(scr.slices.first().map(|s| s.id));
        let set: Vec<Uuid> = scr
            .slices
            .iter()
            .filter(|s| {
                ids.contains(&s.id)
                    || (add && (Some(s.id) == primary || self.sel_slices.contains(&s.id)))
            })
            .map(|s| s.id)
            .collect();
        if let Some(first) = ids.first().filter(|_| !ids.is_empty()) {
            self.sel_slice = Some(*first);
        }
        self.sel_slices = if set.len() > 1 { set } else { vec![] };
    }

    /// Double-click on a slice: the preview monitor shows the slice's source (a layer on
    /// its own); anything else previews the composition.
    pub(crate) fn preview_slice(&mut self, scr: &Screen, id: Uuid) {
        let Some(s) = scr.slices.iter().find(|s| s.id == id) else {
            return;
        };
        self.sel_slice = Some(id);
        self.preview_target = match s.source {
            SliceSource::Layer(l) if self.project.composition.layers.iter().any(|x| x.id == l) => {
                PreviewTarget::Layer(l)
            }
            _ => PreviewTarget::Composition,
        };
    }

    pub(crate) fn slice_overview(&mut self, ui: &mut egui::Ui, scr: &Screen) {
        let (sw, sh) = (scr.width.max(1) as f32, scr.height.max(1) as f32);
        let size = egui::vec2(OUTPUT_LIST_W, OUTPUT_LIST_W * sh / sw);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        ui.data_mut(|d| d.insert_temp(rect_id(), rect));
        let v = ui.visuals().clone();
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, v.extreme_bg_color);
        let to_px = |q: (f32, f32)| {
            egui::pos2(
                rect.min.x + q.0 / sw * rect.width(),
                rect.min.y + q.1 / sh * rect.height(),
            )
        };
        let to_unit = |q: egui::Pos2| {
            (
                (q.x - rect.min.x) / rect.width() * sw,
                (q.y - rect.min.y) / rect.height() * sh,
            )
        };
        let items: Vec<(Uuid, Rect)> = scr
            .slices
            .iter()
            .filter_map(|s| Some((s.id, slice_geom::bbox(&s.output_mesh)?)))
            .collect();
        let hit = |at: egui::Pos2| {
            let u = to_unit(at);
            items
                .iter()
                .rev()
                .find(|(_, r)| u.0 >= r.x && u.0 <= r.x + r.w && u.1 >= r.y && u.1 <= r.y + r.h)
                .map(|(id, _)| *id)
        };
        let primary = self.sel_slice.or(scr.slices.first().map(|s| s.id));
        for (n, s) in scr.slices.iter().enumerate() {
            let Some((_, r)) = items.iter().find(|(id, _)| *id == s.id) else {
                continue;
            };
            let on = Some(s.id) == primary || self.sel_slices.contains(&s.id);
            let rr = egui::Rect::from_two_pos(to_px((r.x, r.y)), to_px((r.x + r.w, r.y + r.h)));
            let (fill, stroke) = if on {
                (v.selection.bg_fill.gamma_multiply(0.5), v.selection.stroke)
            } else {
                (egui::Color32::TRANSPARENT, v.widgets.inactive.fg_stroke)
            };
            p.rect(rr, 0.0, fill, stroke, egui::StrokeKind::Inside);
            p.text(
                rr.center(),
                egui::Align2::CENTER_CENTER,
                format!("{}", n + 1),
                egui::FontId::proportional(crate::theme::FS_SM),
                v.text_color(),
            );
        }
        let gid = egui::Id::new("slice-overview-gesture");
        let (shift, alt) =
            ui.input(|i| (i.modifiers.shift || i.modifiers.command, i.modifiers.alt));
        if resp.drag_started() {
            ui.data_mut(|d| {
                d.insert_temp(
                    gid,
                    Gesture {
                        lasso: alt,
                        path: vec![],
                    },
                )
            });
        }
        let mut g = ui.data(|d| d.get_temp::<Gesture>(gid)).unwrap_or_default();
        if resp.dragged()
            && let Some(q) = resp.interact_pointer_pos()
        {
            if g.path.is_empty() {
                // The press point, from before the first drag movement.
                g.path
                    .push(ui.input(|i| i.pointer.press_origin()).unwrap_or(q));
            }
            g.path.push(q);
            let line = egui::Stroke::new(crate::theme::STROKE_CANVAS, v.selection.stroke.color);
            if g.lasso {
                p.add(egui::Shape::line(g.path.clone(), line));
            } else {
                let b = egui::Rect::from_two_pos(g.path[0], q);
                p.rect_stroke(b, 0.0, line, egui::StrokeKind::Inside);
            }
            ui.data_mut(|d| d.insert_temp(gid, g.clone()));
        }
        if resp.drag_stopped() && g.path.len() > 1 {
            let picked = if g.lasso {
                let poly: Vec<_> = g.path.iter().map(|q| to_unit(*q)).collect();
                slice_geom::pick_lasso(&items, &poly)
            } else {
                slice_geom::pick_box(&items, to_unit(g.path[0]), to_unit(*g.path.last().unwrap()))
            };
            if !picked.is_empty() {
                self.select_slices(scr, &picked, shift);
                self.sel_pt_in = None;
                self.sel_pt_out = None;
            }
            ui.data_mut(|d| d.insert_temp(gid, Gesture::default()));
        }
        if let Some(at) = resp.interact_pointer_pos().or(resp.hover_pos())
            && let Some(id) = hit(at)
        {
            if resp.double_clicked() {
                self.preview_slice(scr, id);
            } else if resp.clicked() {
                self.click_slice(scr, id, shift);
                self.sel_pt_in = None;
                self.sel_pt_out = None;
            }
        }
        resp.on_hover_text(
            "Click selects (Shift adds), drag boxes, Alt + drag lassos, double-click previews",
        );
    }
}
