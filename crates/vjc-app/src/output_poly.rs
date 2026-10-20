//! Polygon tools of the Output setup window: drawing a polygon slice, an output mask or a
//! slice mask (click points, or a freehand pen stroke), editing the points of a polygon
//! (move, add, delete, Shift snaps to 15 degrees) and the Bezier handles of a warp mesh.

use crate::State;
use crate::output_ui::{Opts, handles};
use crate::theme::{
    BG, FS_SM, HANDLE_GRAB_K, HANDLE_R, SP_2, SP_4, STROKE_CANVAS, STROKE_HANDLE, TEXT_MUTED,
};
use eframe::egui;
use uuid::Uuid;
use vjc_core::slice_adv::{self, DrawStep, MAX_POLY_POINTS, PolyDraw};
use vjc_core::{Command, mapping};
use vjc_model::{Mask, Screen, Slice};

type Pt = (f32, f32);

/// Pen strokes are simplified with this tolerance (fraction of the screen) before they
/// become a polygon.
pub(crate) const PEN_TOLERANCE: f32 = 0.004;
/// Angle step Shift snaps to, in degrees.
pub(crate) const SNAP_DEG: f32 = 15.0;

/// What a finished drawing becomes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DrawFor {
    /// A new polygon slice on the selected screen.
    Slice,
    /// The output mask of the selected screen.
    OutputMask,
    /// The mask of one slice.
    SliceMask(Uuid),
}

/// A polygon being drawn on the Output canvas.
#[derive(Clone, Debug)]
pub(crate) struct DrawTool {
    pub(crate) target: DrawFor,
    /// Freehand pen instead of clicked points.
    pub(crate) pen: bool,
    pub(crate) poly: PolyDraw,
    /// Pen stroke so far, normalized.
    pub(crate) stroke: Vec<Pt>,
}

impl DrawTool {
    pub(crate) fn new(target: DrawFor, pen: bool) -> Self {
        Self {
            target,
            pen,
            poly: PolyDraw::default(),
            stroke: vec![],
        }
    }
}

/// Curve handles of the polygon `poly_canvas` edits: the current ones (one per point, or
/// none) and how to store a new list.
pub(crate) struct Curve<'a> {
    pub(crate) tangents: &'a [Pt],
    pub(crate) set: &'a dyn Fn(Vec<Pt>) -> Command,
}

/// Polygon editing state of the window.
#[derive(Default)]
pub(crate) struct PolyUi {
    pub(crate) draw: Option<DrawTool>,
    /// The canvas edits the outline points of the selected polygon slice.
    pub(crate) edit_outline: bool,
    /// The canvas edits the points of the output mask.
    pub(crate) edit_omask: bool,
    /// Selected point of the polygon being edited.
    pub(crate) sel: Option<usize>,
    /// Selected point of the output mask (kept apart from the outline selection).
    pub(crate) sel_omask: Option<usize>,
    /// Screen `sel_omask` belongs to.
    omask_screen: Option<Uuid>,
}

impl PolyUi {
    /// The output-mask point index only means something on one screen: drop it when the
    /// screen being edited changes.
    pub(crate) fn follow_screen(&mut self, screen: Uuid) {
        if self.omask_screen != Some(screen) {
            self.omask_screen = Some(screen);
            self.sel_omask = None;
        }
    }
}

/// The command a finished drawing asks for; `None` when the polygon is not valid.
pub(crate) fn finish_command(
    scr: &Screen,
    target: DrawFor,
    pts: &[Pt],
    pen: bool,
) -> Option<Command> {
    match target {
        DrawFor::Slice => Some(Command::AddSlice {
            screen: scr.id,
            slice: slice_adv::polygon_slice(scr.width, scr.height, pts)?,
            index: scr.slices.len(),
        }),
        DrawFor::OutputMask => {
            let pts = slice_adv::clamp_poly(pts).ok()?;
            Some(Command::SetScreenMask {
                screen: scr.id,
                mask: Some(if pen {
                    Mask::Path(pts)
                } else {
                    Mask::Polygon(pts)
                }),
            })
        }
        DrawFor::SliceMask(slice) => {
            let mask = slice_adv::clamp_poly(pts).ok()?;
            Some(Command::SetSliceMask {
                screen: scr.id,
                slice,
                mask: Some(mask),
            })
        }
    }
}

/// `q` moved onto the nearest 15 degree ray from `from`; both normalized, angles measured
/// on the screen in pixels.
pub(crate) fn snap_norm(from: Pt, q: Pt, size: (f32, f32)) -> Pt {
    let px = |p: Pt| (p.0 * size.0, p.1 * size.1);
    let s = slice_adv::snap_angle(px(from), px(q), SNAP_DEG);
    (
        (s.0 / size.0.max(1.0)).clamp(0.0, 1.0),
        (s.1 / size.1.max(1.0)).clamp(0.0, 1.0),
    )
}

/// Where the handle of mesh point `i` along `axis` (0 = columns, 1 = rows) sits, and the
/// sign that turns its offset into the handle offset: the control point toward the next
/// point, or (last of its line) toward the previous one.
fn handle_pos(mesh: &[Pt], cols: usize, i: usize, axis: usize, h: Pt) -> Option<(Pt, Pt)> {
    let rows = mesh.len() / cols.max(1);
    let (x, y) = (i % cols, i / cols);
    let (n, last) = if axis == 0 {
        (cols, x + 1 == cols)
    } else {
        (rows, y + 1 == rows)
    };
    if n < 2 {
        return None;
    }
    let step = if axis == 0 { 1 } else { cols };
    let (other, sign) = if last {
        (i - step, -1.0)
    } else {
        (i + step, 1.0)
    };
    let (p, o) = (mesh[i], mesh[other]);
    let base = (p.0 + (o.0 - p.0) / 3.0, p.1 + (o.1 - p.1) / 3.0);
    Some(((base.0 + sign * h.0, base.1 + sign * h.1), (base.0, base.1)))
}

impl State {
    /// Starts drawing on the Output tab.
    pub(crate) fn start_draw(&mut self, target: DrawFor, pen: bool) {
        self.poly.draw = Some(DrawTool::new(target, pen));
        self.poly.edit_outline = false;
        self.poly.edit_omask = false;
        self.edit_mask = false;
        if let Some(w) = self.out_win.as_mut() {
            w.tab = crate::output_ui::OutTab::Output;
        }
    }

    /// The drawing tool on the canvas: clicks add points (Shift snaps to 15 degrees, a click
    /// on the first point closes), or a pen stroke is simplified into a polygon. Enter closes,
    /// Backspace removes the last point, Esc and right click cancel.
    pub(crate) fn draw_canvas(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        scr: &Screen,
        (rect, clip): (egui::Rect, egui::Rect),
        cmds: &mut Vec<Command>,
    ) {
        let Some(mut tool) = self.poly.draw.take() else {
            return;
        };
        let size = (scr.width.max(1) as f32, scr.height.max(1) as f32);
        let norm = |at: egui::Pos2| -> Pt {
            (
                ((at.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
                ((at.y - rect.min.y) / rect.height()).clamp(0.0, 1.0),
            )
        };
        let px = |q: Pt| {
            egui::pos2(
                rect.min.x + q.0 * rect.width(),
                rect.min.y + q.1 * rect.height(),
            )
        };
        // Enter and Backspace belong to a focused text field; the canvas takes them only
        // while the draw tool is active, the canvas is the focused widget or nothing is, wherever
        // the pointer is (it may be over the props panel).
        let canvas_keys = ui
            .ctx()
            .memory(|m| m.focused().is_none_or(|f| f == resp.id));
        let (shift, esc, enter, back) = ui.input(|i| {
            (
                i.modifiers.shift,
                i.key_pressed(egui::Key::Escape),
                canvas_keys && i.key_pressed(egui::Key::Enter),
                canvas_keys && i.key_pressed(egui::Key::Backspace),
            )
        });
        let mut done: Option<Vec<Pt>> = None;
        let mut cancel = esc || resp.secondary_clicked();
        let hover = resp.hover_pos().map(norm);
        if tool.pen {
            if resp.drag_started() {
                tool.stroke.clear();
            }
            if resp.dragged()
                && let Some(at) = resp.interact_pointer_pos()
            {
                push_stroke(&mut tool.stroke, norm(at));
            }
            if resp.drag_stopped() {
                let pts = slice_adv::simplify_stroke(&tool.stroke, PEN_TOLERANCE, MAX_POLY_POINTS);
                if pts.len() >= 3 {
                    done = Some(pts);
                } else {
                    tool.stroke.clear();
                }
            }
        } else {
            if back {
                tool.poly.points.pop();
            }
            if enter && tool.poly.points.len() >= 3 {
                done = Some(std::mem::take(&mut tool.poly.points));
            }
            if resp.clicked()
                && let Some(at) = resp.interact_pointer_pos()
            {
                let mut q = norm(at);
                if shift && let Some(&last) = tool.poly.points.last() {
                    q = snap_norm(last, q, size);
                }
                let radius = HANDLE_R * HANDLE_GRAB_K / rect.width();
                if let DrawStep::Closed(pts) = tool.poly.click(q, radius) {
                    done = Some(pts);
                }
            }
        }
        // What is drawn so far.
        let col = ui.visuals().selection.stroke.color;
        let p = ui.painter_at(clip);
        let mut line: Vec<egui::Pos2> = if tool.pen {
            tool.stroke.iter().map(|q| px(*q)).collect()
        } else {
            tool.poly.points.iter().map(|q| px(*q)).collect()
        };
        if !tool.pen
            && let (Some(&last), Some(h)) = (tool.poly.points.last(), hover)
        {
            let h = if shift { snap_norm(last, h, size) } else { h };
            line.push(px(h));
        }
        if line.len() > 1 {
            p.add(egui::Shape::line(
                line,
                egui::Stroke::new(STROKE_CANVAS, col),
            ));
        }
        if !tool.pen {
            for (i, q) in tool.poly.points.iter().enumerate() {
                p.circle(
                    px(*q),
                    HANDLE_R,
                    if i == 0 {
                        col
                    } else {
                        egui::Color32::TRANSPARENT
                    },
                    egui::Stroke::new(STROKE_HANDLE, col),
                );
            }
        }
        let help = if tool.pen {
            "Drag to draw a shape. Esc cancels."
        } else {
            "Click to add points, click the first point to close. Shift snaps to 15 degrees. Esc cancels."
        };
        let galley = p.layout_no_wrap(help.into(), egui::FontId::proportional(FS_SM), TEXT_MUTED);
        let at = clip.min + egui::vec2(SP_4, SP_4);
        p.rect_filled(
            egui::Rect::from_min_size(at, galley.size()).expand(SP_2),
            2.0,
            BG.gamma_multiply(0.8),
        );
        p.galley(at, galley, TEXT_MUTED);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        if let Some(pts) = done {
            cancel = true;
            if let Some(cmd) = finish_command(scr, tool.target, &pts, tool.pen) {
                self.after_draw(scr, tool.target, &cmd);
                cmds.push(cmd);
            }
        }
        if !cancel {
            self.poly.draw = Some(tool);
        }
    }

    /// Selection that follows a finished drawing.
    fn after_draw(&mut self, scr: &Screen, target: DrawFor, cmd: &Command) {
        match (target, cmd) {
            (DrawFor::Slice, Command::AddSlice { slice, .. }) => {
                self.sel_slice = Some(slice.id);
                self.sel_slices.clear();
                self.poly.edit_outline = true;
                self.poly.sel = None;
            }
            (DrawFor::OutputMask, _) => {
                self.sel_screen = Some(scr.id);
                self.poly.edit_omask = true;
                self.poly.sel_omask = None;
            }
            (DrawFor::SliceMask(_), _) => {
                self.edit_mask = true;
                self.sel_mask_pt = None;
            }
            _ => {}
        }
    }

    /// Points of a polygon over the output canvas (normalized screen coordinates): drag a
    /// point to move it (Shift snaps to 15 degrees), click empty space to add one on the
    /// nearest edge, Delete removes the selected point (at least three stay).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn poly_canvas(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        (id, size): (egui::Id, (f32, f32)),
        pts: &[Pt],
        (rect, clip): (egui::Rect, egui::Rect),
        sel: &mut Option<usize>,
        set: &dyn Fn(Vec<Pt>) -> Command,
        curve: Option<Curve<'_>>,
        cmds: &mut Vec<Command>,
    ) {
        let col = ui.visuals().selection.stroke.color;
        let to_screen = |q: &Pt| {
            egui::pos2(
                rect.min.x + q.0 * rect.width(),
                rect.min.y + q.1 * rect.height(),
            )
        };
        let tg: Vec<Pt> = curve
            .as_ref()
            .filter(|c| c.tangents.len() == pts.len())
            .map(|c| c.tangents.to_vec())
            .unwrap_or_default();
        let outline = slice_adv::flatten_mask(pts, &tg);
        let screen_pts: Vec<egui::Pos2> = outline.iter().map(to_screen).collect();
        ui.painter_at(clip).add(egui::Shape::closed_line(
            screen_pts,
            egui::Stroke::new(STROKE_CANVAS, col),
        ));
        *sel = sel.filter(|&i| i < pts.len());
        if let Some(i) = *sel
            && let Some(t) = tg.get(i).filter(|t| **t != (0.0, 0.0))
        {
            // The two arms of the selected point's handle.
            let p = pts[i];
            let (a, b) = (
                to_screen(&(p.0 - t.0, p.1 - t.1)),
                to_screen(&(p.0 + t.0, p.1 + t.1)),
            );
            let painter = ui.painter_at(clip);
            painter.line_segment([a, b], egui::Stroke::new(STROKE_HANDLE, col));
            for end in [a, b] {
                painter.circle_filled(end, HANDLE_R * 0.7, col);
            }
        }
        // Alt+drag on a point pulls out its handle, Alt+click makes it a corner again.
        let alt = ui.input(|i| i.modifiers.alt);
        let to_norm = |q: egui::Pos2| {
            (
                (q.x - rect.min.x) / rect.width(),
                (q.y - rect.min.y) / rect.height(),
            )
        };
        let curve_key = id.with("curve-drag");
        let mut curved = false;
        if let Some(c) = &curve
            && let Some(q) = resp.interact_pointer_pos()
        {
            let radius = HANDLE_R * HANDLE_GRAB_K / rect.width();
            let hit = mapping::hit_test(pts, to_norm(q), radius);
            if resp.drag_started() {
                let start = hit.filter(|_| alt);
                ui.data_mut(|d| d.insert_temp::<Option<usize>>(curve_key, start));
            }
            let held = ui.data(|d| d.get_temp::<Option<usize>>(curve_key).flatten());
            let mut next = tg.clone();
            next.resize(pts.len(), (0.0, 0.0));
            if resp.dragged()
                && let Some(i) = held
            {
                let u = to_norm(q);
                next[i] = (u.0 - pts[i].0, u.1 - pts[i].1);
                *sel = Some(i);
                cmds.push((c.set)(next));
                curved = true;
            } else if resp.clicked()
                && alt
                && let Some(i) = hit
            {
                *sel = Some(i);
                curved = true;
                if tg.get(i).is_some_and(|t| *t != (0.0, 0.0)) {
                    next[i] = (0.0, 0.0);
                    cmds.push((c.set)(next));
                }
            }
        }
        if resp.drag_stopped() {
            ui.data_mut(|d| d.insert_temp::<Option<usize>>(curve_key, None));
        }
        let moved = handles(
            ui,
            resp,
            id,
            (rect, (1.0, 1.0)),
            pts,
            sel,
            Opts {
                snap: (0.0, 0.0),
                keys: true,
                body: None,
                clip,
            },
        )
        .moved;
        let shift = ui.input(|i| i.modifiers.shift);
        let delete = resp.hovered()
            && ui
                .input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
        // Points move, go away or appear with their handles in step.
        let with_handles = |next: Vec<Pt>, tangents: Vec<Pt>| match &curve {
            Some(c) if !tangents.is_empty() => Command::Batch {
                commands: vec![set(next), (c.set)(tangents)],
            },
            _ => set(next),
        };
        let moved = moved.filter(|_| !curved && !(alt && curve.is_some()));
        if curved {
        } else if let Some((i, mut q)) = moved.filter(|&(i, _)| i < pts.len()) {
            if shift {
                q = snap_norm(pts[(i + pts.len() - 1) % pts.len()], q, size);
            }
            let mut next = pts.to_vec();
            next[i] = q;
            cmds.push(set(next));
        } else if delete
            && pts.len() > 3
            && let Some(i) = *sel
        {
            let mut next = pts.to_vec();
            next.remove(i);
            *sel = None;
            let mut t = tg.clone();
            if !t.is_empty() {
                t.remove(i);
            }
            cmds.push(with_handles(next, t));
        } else if let Some(at) = ui.input(|i| {
            (i.pointer.primary_clicked() && resp.hovered())
                .then(|| i.pointer.interact_pos())
                .flatten()
        }) {
            let q = (
                (at.x - rect.min.x) / rect.width(),
                (at.y - rect.min.y) / rect.height(),
            );
            let radius = HANDLE_R * HANDLE_GRAB_K / rect.width();
            if mapping::hit_test(pts, q, radius).is_none() && pts.len() < MAX_POLY_POINTS {
                let i = mapping::nearest_edge_insert(pts, q);
                let mut next = pts.to_vec();
                next.insert(i, q);
                *sel = Some(i);
                let mut t = tg.clone();
                if !t.is_empty() {
                    t.insert(i, (0.0, 0.0));
                }
                cmds.push(with_handles(next, t));
            }
        }
    }

    /// Bezier handles of the selected mesh point: one toward the next column and one toward
    /// the next row (the previous one for the last of a line). Drag a handle to bend the
    /// mesh; one command per frame, grouped into one undo step by the window.
    pub(crate) fn bezier_handles(
        &mut self,
        ui: &mut egui::Ui,
        (scr, sl): (&Screen, &Slice),
        (rect, scale): (egui::Rect, f32),
        clip: egui::Rect,
        cmds: &mut Vec<Command>,
    ) {
        let Some(i) = self.sel_pt_out else { return };
        let cols = sl.mesh_cols as usize;
        if sl.mesh_mode != vjc_model::MeshMode::Bezier
            || !mapping::mesh_valid(&sl.output_mesh, sl.mesh_cols)
            || i >= sl.output_mesh.len()
        {
            return;
        }
        let mut tg = sl.mesh_tangents.clone();
        tg.resize(sl.output_mesh.len(), ((0.0, 0.0), (0.0, 0.0)));
        let to_px = |q: Pt| egui::pos2(rect.min.x + q.0 * scale, rect.min.y + q.1 * scale);
        let col = ui.visuals().selection.stroke.color;
        let p = ui.painter_at(clip);
        for axis in 0..2 {
            let h = if axis == 0 { tg[i].0 } else { tg[i].1 };
            let Some((pos, base)) = handle_pos(&sl.output_mesh, cols, i, axis, h) else {
                continue;
            };
            let at = to_px(pos);
            let anchor = to_px(sl.output_mesh[i]);
            p.line_segment([anchor, at], egui::Stroke::new(STROKE_HANDLE, col));
            let r =
                egui::Rect::from_center_size(at, egui::Vec2::splat(HANDLE_R * 2.0 * HANDLE_GRAB_K));
            let resp = ui.interact(
                r,
                egui::Id::new(("bezier-handle", sl.id, i, axis)),
                egui::Sense::drag(),
            );
            p.rect_filled(
                egui::Rect::from_center_size(at, egui::Vec2::splat(HANDLE_R * 1.4)),
                1.0,
                col,
            );
            if resp.dragged()
                && let Some(ptr) = resp.interact_pointer_pos()
            {
                let q = ((ptr.x - rect.min.x) / scale, (ptr.y - rect.min.y) / scale);
                let last = if axis == 0 {
                    i % cols + 1 == cols
                } else {
                    i / cols + 1 == sl.output_mesh.len() / cols
                };
                let sign = if last { -1.0 } else { 1.0 };
                let new = (sign * (q.0 - base.0), sign * (q.1 - base.1));
                if axis == 0 {
                    tg[i].0 = new;
                } else {
                    tg[i].1 = new;
                }
                cmds.push(Command::SetSliceMeshMode {
                    screen: scr.id,
                    slice: sl.id,
                    mode: sl.mesh_mode,
                    tangents: tg.clone(),
                });
                break;
            }
        }
    }
}

/// Adds a pen sample unless the pointer has not moved: holding still must not grow the
/// stroke by one point per frame.
fn push_stroke(stroke: &mut Vec<Pt>, p: Pt) {
    if stroke
        .last()
        .is_none_or(|l| (l.0 - p.0).abs() > 1e-4 || (l.1 - p.1).abs() > 1e-4)
    {
        stroke.push(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pen_stroke_ignores_a_pointer_that_did_not_move() {
        let mut s = vec![];
        for _ in 0..100 {
            push_stroke(&mut s, (0.5, 0.5));
        }
        assert_eq!(s.len(), 1, "held still: one sample");
        // Control: real movement still adds samples.
        push_stroke(&mut s, (0.6, 0.5));
        push_stroke(&mut s, (0.6, 0.6));
        assert_eq!(s.len(), 3);
    }

    fn scr() -> Screen {
        mapping::new_screen("t", 1920, 1080)
    }

    #[test]
    fn output_mask_point_selection_is_dropped_on_screen_change() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut p = PolyUi::default();
        p.follow_screen(a);
        p.sel_omask = Some(4);
        p.follow_screen(a);
        assert_eq!(p.sel_omask, Some(4));
        p.follow_screen(b);
        assert_eq!(p.sel_omask, None);
    }

    #[test]
    fn finished_drawings_become_the_right_commands() {
        let s = scr();
        let tri = [(0.1, 0.1), (0.9, 0.1), (0.5, 0.9)];
        assert!(matches!(
            finish_command(&s, DrawFor::Slice, &tri, false),
            Some(Command::AddSlice { .. })
        ));
        assert!(matches!(
            finish_command(&s, DrawFor::OutputMask, &tri, true),
            Some(Command::SetScreenMask {
                mask: Some(Mask::Path(_)),
                ..
            })
        ));
        assert!(matches!(
            finish_command(&s, DrawFor::OutputMask, &tri, false),
            Some(Command::SetScreenMask {
                mask: Some(Mask::Polygon(_)),
                ..
            })
        ));
        let id = Uuid::from_u128(5);
        assert!(matches!(
            finish_command(&s, DrawFor::SliceMask(id), &tri, true),
            Some(Command::SetSliceMask { slice, .. }) if slice == id
        ));
        assert!(finish_command(&s, DrawFor::Slice, &tri[..2], false).is_none());
    }

    #[test]
    fn shift_snap_works_in_pixels_not_normalized_units() {
        // 1920x1080: from the centre to a point 100 px right and 10 px down snaps level.
        let from = (0.5, 0.5);
        let q = (0.5 + 100.0 / 1920.0, 0.5 + 10.0 / 1080.0);
        let s = snap_norm(from, q, (1920.0, 1080.0));
        assert!((s.1 - 0.5).abs() < 1e-4, "{s:?}");
        assert!(s.0 > 0.5);
    }

    #[test]
    fn handle_sits_a_third_along_plus_its_offset() {
        let mesh = vec![(0.0, 0.0), (30.0, 0.0), (0.0, 30.0), (30.0, 30.0)];
        let (pos, base) = handle_pos(&mesh, 2, 0, 0, (0.0, 5.0)).unwrap();
        assert_eq!(base, (10.0, 0.0));
        assert_eq!(pos, (10.0, 5.0));
        // Last of its line: toward the previous point, sign flipped.
        let (pos, base) = handle_pos(&mesh, 2, 1, 0, (0.0, 5.0)).unwrap();
        assert_eq!(base, (20.0, 0.0));
        assert_eq!(pos, (20.0, -5.0));
    }
}
