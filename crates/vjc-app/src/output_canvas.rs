//! Centre of the Output setup window: the toolbar (tabs, zoom, undo / redo, hand, snap, grid,
//! mode) and the Input / Output canvas. The canvas shows the live composition (Input) or the
//! live render of the selected screen (Output), every slice as a dashed outline with its name,
//! and box handles on the selected slice.

use crate::output_hints::hint;
use crate::output_tree::slice_label;
use crate::output_ui::{Drag, GridKind, Opts, OutMode, OutTab, handles};
use crate::slice_inspector::{GUIDE_PX, arrow_move, snap_dragged};
use crate::theme::{
    BORDER, CANVAS_DASH, CANVAS_GAP, CANVAS_ZOOM_MAX, CANVAS_ZOOM_MIN, FS_SM, GUIDE_ALPHA,
    HANDLE_GRAB_K, HANDLE_R, SNAP_PX, SP_2, SP_4, STROKE_CANVAS, STROKE_HANDLE, STROKE_W, TEXT,
};
use crate::widgets;
use crate::{PreviewTarget, State, icons};
use eframe::egui;
use std::path::Path;
use uuid::Uuid;
use vjc_core::slice_geom::{self, Rect, Side};
use vjc_core::{Command, mapping};
use vjc_model::{Screen, Slice, SliceSource};

/// Where the canvas drew the content last frame (read by tests).
pub(crate) fn rect_id() -> egui::Id {
    egui::Id::new("out-canvas-rect")
}

/// Width of the grid dropdown's list, wide enough for its longest toggle.
const GRID_MENU_W: f32 = 190.0;
/// Tallest the grid dropdown grows before it scrolls (it holds the grids and the view toggles).
const GRID_MENU_H: f32 = 360.0;
/// Zoom steps of the dropdown, in percent.
const ZOOM_STEPS: [u32; 4] = [25, 50, 100, 200];
/// Room kept around the content when it is fitted.
const FIT_MARGIN: f32 = SP_4;
/// Wheel sensitivity: zoom factor per scrolled point.
const WHEEL_ZOOM: f32 = 0.0015;
/// Grab distance of a box handle, in canvas points.
const HANDLE_GRAB: f32 = HANDLE_R * HANDLE_GRAB_K;

/// Gesture in progress, kept in egui memory between frames.
#[derive(Clone)]
enum Grab {
    Pan,
    /// Moving the selected slices: the primary slice's rectangle when the drag began and the
    /// pointer offset since (content pixels).
    Move {
        start: Rect,
        acc: (f32, f32),
    },
    /// Resizing the primary slice by handle `handle` (0..8, clockwise from the top left).
    Resize {
        handle: usize,
        start: Rect,
        off: (f32, f32),
    },
    /// Box (or lasso, with Alt) selection; the pointer path in canvas points.
    Select {
        lasso: bool,
        path: Vec<egui::Pos2>,
    },
}

/// Where the content sits on screen.
#[derive(Clone, Copy)]
struct View {
    /// Rectangle the content (composition or screen) occupies, in canvas points.
    rect: egui::Rect,
    /// Canvas points per content pixel.
    scale: f32,
}

impl View {
    fn px(&self, p: (f32, f32)) -> egui::Pos2 {
        egui::pos2(
            self.rect.min.x + p.0 * self.scale,
            self.rect.min.y + p.1 * self.scale,
        )
    }

    fn unit(&self, q: egui::Pos2) -> (f32, f32) {
        (
            (q.x - self.rect.min.x) / self.scale,
            (q.y - self.rect.min.y) / self.scale,
        )
    }
}

/// Outline of a slice in content pixels: the edge of the warp mesh (Output) or the input
/// polygon (Input).
fn outline(sl: &Slice, side: Side, comp: (u32, u32), screen: (u32, u32)) -> Vec<(f32, f32)> {
    match side {
        Side::Input => {
            let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
            mapping::input_quad(&sl.input)
                .iter()
                .map(|p| (p.0 * cw, p.1 * ch))
                .collect()
        }
        Side::Output => {
            if let vjc_model::SliceShape::Polygon(poly) = &sl.shape
                && poly.len() >= 3
            {
                let (w, h) = (screen.0 as f32, screen.1 as f32);
                return poly.iter().map(|p| (p.0 * w, p.1 * h)).collect();
            }
            let cols = sl.mesh_cols.max(2) as usize;
            let m = &sl.output_mesh;
            if !mapping::mesh_valid(m, sl.mesh_cols) {
                return slice_geom::bbox(m).map_or(vec![], |r| {
                    vec![
                        (r.x, r.y),
                        (r.x + r.w, r.y),
                        (r.x + r.w, r.y + r.h),
                        (r.x, r.y + r.h),
                    ]
                });
            }
            let rows = m.len() / cols;
            let mut o = vec![];
            o.extend((0..cols).map(|i| m[i]));
            o.extend((1..rows).map(|j| m[j * cols + cols - 1]));
            o.extend((0..cols - 1).rev().map(|i| m[(rows - 1) * cols + i]));
            o.extend((1..rows - 1).rev().map(|j| m[j * cols]));
            o
        }
    }
}

/// The command that puts `pts` (content pixels) on `side` of the slice.
fn set_pts(
    scr: &Screen,
    sl: &Slice,
    side: Side,
    comp: (u32, u32),
    pts: Vec<(f32, f32)>,
) -> Command {
    match side {
        Side::Input => {
            let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
            Command::SetSliceInput {
                screen: scr.id,
                slice: sl.id,
                input: pts.iter().map(|p| (p.0 / cw, p.1 / ch)).collect(),
            }
        }
        Side::Output => Command::SetSliceMesh {
            screen: scr.id,
            slice: sl.id,
            mesh: pts,
            cols: sl.mesh_cols,
        },
    }
}

/// Handle positions of `r`: corners and edge midpoints, clockwise from the top left.
fn handle_pts(r: Rect) -> [(f32, f32); 8] {
    let (x1, y1) = (r.x + r.w, r.y + r.h);
    let (xm, ym) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    [
        (r.x, r.y),
        (xm, r.y),
        (x1, r.y),
        (x1, ym),
        (x1, y1),
        (xm, y1),
        (r.x, y1),
        (r.x, ym),
    ]
}

/// The rectangle that results from dragging handle `h` of `start` to `q`. Corners keep the
/// aspect ratio with `keep`. Sizes never drop below one pixel.
fn resized(start: Rect, h: usize, q: (f32, f32), keep: bool) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (start.x, start.y, start.x + start.w, start.y + start.h);
    if matches!(h, 0 | 6 | 7) {
        x0 = q.0.min(x1 - 1.0);
    }
    if matches!(h, 2..=4) {
        x1 = q.0.max(x0 + 1.0);
    }
    if matches!(h, 0..=2) {
        y0 = q.1.min(y1 - 1.0);
    }
    if matches!(h, 4..=6) {
        y1 = q.1.max(y0 + 1.0);
    }
    let mut r = Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    };
    if keep && h.is_multiple_of(2) && start.w > 0.0 && start.h > 0.0 {
        // The axis that changed most leads; the other follows.
        let (kw, kh) = (r.w / start.w, r.h / start.h);
        let k = if (kw - 1.0).abs() >= (kh - 1.0).abs() {
            kw
        } else {
            kh
        };
        let (w, hh) = (start.w * k, start.h * k);
        // The corner opposite to the dragged one stays where it was.
        r.x = if matches!(h, 0 | 6) {
            start.x + start.w - w
        } else {
            start.x
        };
        r.y = if matches!(h, 0 | 2) {
            start.y + start.h - hh
        } else {
            start.y
        };
        r.w = w;
        r.h = hh;
    }
    r
}

fn cursor_for(h: usize) -> egui::CursorIcon {
    match h {
        0 | 4 => egui::CursorIcon::ResizeNwSe,
        2 | 6 => egui::CursorIcon::ResizeNeSw,
        1 | 5 => egui::CursorIcon::ResizeVertical,
        _ => egui::CursorIcon::ResizeHorizontal,
    }
}

/// Is `p` inside the polygon `poly`?
fn inside(poly: &[(f32, f32)], p: (f32, f32)) -> bool {
    slice_geom::point_in_polygon(poly, p)
}

impl State {
    /// Makes `ids` the selection (primary = first); with `add` they join the current one.
    pub(crate) fn select_slices(&mut self, scr: &Screen, ids: &[Uuid], add: bool) {
        let primary = self.sel_slice;
        let set: Vec<Uuid> = scr
            .slices
            .iter()
            .filter(|s| {
                ids.contains(&s.id)
                    || (add && (Some(s.id) == primary || self.sel_slices.contains(&s.id)))
            })
            .map(|s| s.id)
            .collect();
        self.sel_screen = Some(scr.id);
        if let Some(first) = ids.first() {
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

    /// Snap step per axis for the canvas of `space` content pixels: the grid cell when a
    /// grid is set, otherwise the usual pixel step. `(0, 0)` with Snap off.
    fn snap_step(&self, space: (f32, f32)) -> (f32, f32) {
        if !self.snap {
            return (0.0, 0.0);
        }
        match self.out_win.as_ref().and_then(|w| w.grid_dims()) {
            Some((c, r)) => (space.0 / c as f32, space.1 / r as f32),
            None => (SNAP_PX, SNAP_PX),
        }
    }

    /// Tabs, zoom, undo / redo, hand, snap, grid and mode.
    pub(crate) fn out_toolbar(&mut self, ui: &mut egui::Ui) {
        let Some(w) = self.out_win.as_ref() else {
            return;
        };
        let (tab, mode, zoom, hand, grid) = (w.tab, w.mode, w.zoom, w.hand, w.grid);
        let (can_undo, can_redo) = (w.can_undo(), w.can_redo());
        let custom = w.grid_custom;
        let mut view = (
            w.show_slices,
            w.led_grid,
            w.led_cell,
            w.identify,
            w.show_in_output,
        );
        let (mut new_tab, mut new_mode) = (tab, mode);
        let (mut new_zoom, mut new_hand, mut new_grid, mut new_custom) = (zoom, hand, grid, custom);
        let mut act = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = SP_2;
            if hint(widgets::tab(ui, tab == OutTab::Input, "Input"), "tab-input").clicked() {
                new_tab = OutTab::Input;
            }
            if hint(
                widgets::tab(ui, tab == OutTab::Output, "Output"),
                "tab-output",
            )
            .clicked()
            {
                new_tab = OutTab::Output;
            }
            ui.separator();
            let label = match zoom {
                None => "Fit".to_string(),
                Some(z) => format!("{}%", (z * 100.0).round()),
            };
            let r = egui::ComboBox::from_id_salt("out-zoom")
                .width(72.0)
                .selected_text(label)
                .show_ui(ui, |ui| {
                    if widgets::choice(ui, zoom.is_none(), "Fit").clicked() {
                        new_zoom = None;
                    }
                    for p in ZOOM_STEPS {
                        if widgets::choice(ui, false, format!("{p}%")).clicked() {
                            new_zoom = Some(p as f32 / 100.0);
                        }
                    }
                });
            r.response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Zoom")
            });
            hint(r.response, "zoom");
            if hint(
                icons::button_enabled(ui, can_undo, icons::i::ARROW_COUNTER_CLOCKWISE, "Undo edit"),
                "undo",
            )
            .clicked()
            {
                act = Some(false);
            }
            if hint(
                icons::button_enabled(ui, can_redo, icons::i::ARROW_CLOCKWISE, "Redo edit"),
                "redo",
            )
            .clicked()
            {
                act = Some(true);
            }
            ui.separator();
            let mut h = hand;
            if hint(
                icons::toggle(ui, &mut h, icons::i::HAND, "Hand tool"),
                "hand",
            )
            .changed()
            {
                new_hand = h;
            }
            let mut sn = self.snap;
            if hint(icons::toggle(ui, &mut sn, icons::i::MAGNET, "Snap"), "snap").changed() {
                self.snap = sn;
            }
            let glabel = match grid {
                GridKind::Off => "Grid: off".to_string(),
                GridKind::Two => "Grid: 2x2".to_string(),
                GridKind::Three => "Grid: 3x3".to_string(),
                GridKind::Custom => format!("Grid: {}x{}", custom.0, custom.1),
            };
            let r = egui::ComboBox::from_id_salt("out-grid")
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .width(100.0)
                .height(GRID_MENU_H)
                .selected_text(glabel)
                .show_ui(ui, |ui| {
                    for (g, name) in [
                        (GridKind::Off, "Off"),
                        (GridKind::Two, "2x2"),
                        (GridKind::Three, "3x3"),
                        (GridKind::Custom, "Custom"),
                    ] {
                        if widgets::choice(ui, grid == g, name).clicked() {
                            new_grid = g;
                        }
                    }
                    ui.horizontal(|ui| {
                        widgets::field_int(ui, &mut new_custom.0, 1..=64, "Cols ", "");
                        widgets::field_int(ui, &mut new_custom.1, 1..=64, "Rows ", "");
                    });
                    ui.separator();
                    ui.set_min_width(GRID_MENU_W);
                    widgets::toggle(ui, &mut view.0, "Show slice grid");
                    ui.horizontal(|ui| {
                        widgets::toggle(ui, &mut view.1, "LED grid");
                        widgets::field_int(ui, &mut view.2, 2..=256, "", " px");
                    });
                    widgets::toggle(ui, &mut view.3, "Identify slices");
                    widgets::toggle(ui, &mut view.4, "Show in output");
                });
            r.response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Grid lines")
            });
            hint(r.response, "grid");
            // Right side: the editing mode (the active one filled), then the guide switch.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if hint(
                    widgets::choice(ui, mode == OutMode::Points, "Edit points"),
                    "mode-points",
                )
                .clicked()
                {
                    new_mode = OutMode::Points;
                }
                if hint(
                    widgets::choice(ui, mode == OutMode::Transform, "Transform"),
                    "mode-transform",
                )
                .clicked()
                {
                    new_mode = OutMode::Transform;
                }
                if let Some(id) = self.sel_screen
                    && Self::guide_texture(ui.ctx(), id).is_some()
                    && let Some(w) = self.out_win.as_mut()
                {
                    let mut on = w.guide_on;
                    if hint(widgets::toggle(ui, &mut on, "Guide"), "guide").changed() {
                        w.guide_on = on;
                    }
                }
            });
        });
        if let Some(w) = self.out_win.as_mut() {
            if new_tab != w.tab {
                self.sel_pt_in = None;
                self.sel_pt_out = None;
            }
            w.tab = new_tab;
            w.mode = new_mode;
            if new_zoom != w.zoom {
                w.zoom = new_zoom;
                w.pan = egui::Vec2::ZERO;
            }
            w.hand = new_hand;
            w.grid = new_grid;
            w.grid_custom = new_custom;
            (
                w.show_slices,
                w.led_grid,
                w.led_cell,
                w.identify,
                w.show_in_output,
            ) = view;
        }
        match act {
            Some(false) => self.out_undo(),
            Some(true) => self.out_redo(),
            None => {}
        }
        ui.add_space(SP_2);
    }

    /// The canvas: content, guide, grid, slice outlines, handles and the gestures on them.
    pub(crate) fn out_canvas(
        &mut self,
        ui: &mut egui::Ui,
        screens: &[Screen],
        cmds: &mut Vec<Command>,
    ) {
        let Some((scr, prim)) = self.out_selected(screens) else {
            ui.label("No screens: output shows the whole composition.");
            return;
        };
        let Some(win) = self.out_win.as_ref() else {
            return;
        };
        let (tab, mode, hand) = (win.tab, win.mode, win.hand);
        let (zoom, pan, grid) = (win.zoom, win.pan, win.grid_dims());
        let guide_on = win.guide_on;
        let (show_slices, led, identify) = (
            win.show_slices,
            win.led_grid.then_some(win.led_cell),
            win.identify,
        );
        let comp = (
            self.project.composition.width,
            self.project.composition.height,
        );
        let side = match tab {
            OutTab::Input => Side::Input,
            OutTab::Output => Side::Output,
        };
        let space = match tab {
            OutTab::Input => (comp.0.max(1) as f32, comp.1.max(1) as f32),
            OutTab::Output => (scr.width.max(1) as f32, scr.height.max(1) as f32),
        };
        let canvas = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(canvas, egui::Sense::click_and_drag());
        hint(resp.clone(), "canvas");
        let p = ui.painter_at(canvas);
        p.rect_filled(canvas, 0.0, ui.visuals().extreme_bg_color);
        // Scale and placement.
        let fit = ((canvas.width() - 2.0 * FIT_MARGIN) / space.0)
            .min((canvas.height() - 2.0 * FIT_MARGIN) / space.1)
            .max(CANVAS_ZOOM_MIN);
        let mut scale = zoom.unwrap_or(fit).clamp(CANVAS_ZOOM_MIN, CANVAS_ZOOM_MAX);
        let mut pan = pan;
        let place = |scale: f32, pan: egui::Vec2| {
            egui::Rect::from_center_size(
                canvas.center() + pan,
                egui::vec2(space.0 * scale, space.1 * scale),
            )
        };
        // Mouse wheel zooms around the pointer.
        if resp.hovered() {
            let (dy, z) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let k = (dy * WHEEL_ZOOM).exp() * z;
            if (k - 1.0).abs() > 1e-4
                && let Some(at) = ui.input(|i| i.pointer.hover_pos())
            {
                let new = (scale * k).clamp(CANVAS_ZOOM_MIN, CANVAS_ZOOM_MAX);
                let centre = place(scale, pan).center();
                let c2 = at - (at - centre) * (new / scale);
                pan = c2 - canvas.center();
                scale = new;
                if let Some(w) = self.out_win.as_mut() {
                    w.zoom = Some(scale);
                    w.pan = pan;
                }
            }
        }
        let gid = egui::Id::new("out-grab");
        let mods = ui.input(|i| i.modifiers);
        let multi = mods.shift || mods.command;
        let mut grab: Option<Grab> = ui.data(|d| d.get_temp::<Option<Grab>>(gid)).flatten();
        // Panning (hand tool or middle button) moves the content under the pointer.
        if matches!(grab, Some(Grab::Pan)) && resp.dragged() {
            pan += resp.drag_delta();
            scale = zoom.unwrap_or(scale);
            if let Some(w) = self.out_win.as_mut() {
                w.zoom = Some(scale);
                w.pan = pan;
            }
        }
        let view = View {
            rect: place(scale, pan),
            scale,
        };
        ui.data_mut(|d| d.insert_temp(rect_id(), view.rect));
        // Content.
        match tab {
            OutTab::Input => {
                p.rect_filled(view.rect, 0.0, ui.visuals().faint_bg_color);
                if let Some(id) = self.comp_tex {
                    p.image(id, view.rect, full_uv(), egui::Color32::WHITE);
                }
            }
            OutTab::Output => {
                p.rect_filled(view.rect, 0.0, crate::theme::MONITOR_BG);
                self.paint_screen(&p, scr, &view);
            }
        }
        if tab == OutTab::Output
            && guide_on
            && let Some(tex) = Self::guide_texture(ui.ctx(), scr.id)
        {
            p.image(
                tex.id(),
                view.rect,
                full_uv(),
                egui::Color32::WHITE.gamma_multiply(GUIDE_ALPHA),
            );
        }
        p.rect_stroke(
            view.rect,
            0.0,
            egui::Stroke::new(STROKE_W, BORDER),
            egui::StrokeKind::Outside,
        );
        if let Some((c, r)) = grid {
            let line = egui::Stroke::new(STROKE_W, egui::Color32::from_white_alpha(40));
            for i in 1..c {
                let x = view.rect.min.x + view.rect.width() * i as f32 / c as f32;
                p.vline(x, view.rect.y_range(), line);
            }
            for j in 1..r {
                let y = view.rect.min.y + view.rect.height() * j as f32 / r as f32;
                p.hline(view.rect.x_range(), y, line);
            }
        }
        let mut led_lines = 0;
        if let Some(cell) = led {
            let lines = crate::output_extras::led_segments(view.rect, space, cell);
            led_lines = lines.len();
            let stroke = egui::Stroke::new(STROKE_W, crate::theme::LED_GRID);
            p.extend(
                lines
                    .into_iter()
                    .map(|l| egui::Shape::line_segment(l, stroke)),
            );
        }
        ui.data_mut(|d| d.insert_temp(crate::output_extras::led_drawn_id(), led_lines));
        // Slice outlines with their names.
        let sel_ids: Vec<Uuid> = scr
            .slices
            .iter()
            .filter(|s| Some(s.id) == self.sel_slice || self.sel_slices.contains(&s.id))
            .map(|s| s.id)
            .collect();
        let vis = ui.visuals().clone();
        for (i, sl) in scr.slices.iter().enumerate() {
            let poly = outline(sl, side, comp, (scr.width, scr.height));
            if poly.len() < 3 {
                continue;
            }
            if identify && let Some(b) = slice_geom::bbox(&slice_geom::side_points(sl, side, comp))
            {
                let c = view.px((b.x + b.w / 2.0, b.y + b.h / 2.0));
                p.text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    (i + 1).to_string(),
                    egui::FontId::proportional((view.scale * b.h * 0.5).clamp(FS_SM, 96.0)),
                    crate::theme::IDENTIFY_TEXT,
                );
            }
            if !show_slices {
                continue;
            }
            let pts: Vec<egui::Pos2> = poly.iter().map(|q| view.px(*q)).collect();
            let on = sel_ids.contains(&sl.id);
            let mut closed = pts.clone();
            closed.push(pts[0]);
            if on {
                p.add(egui::Shape::convex_polygon(
                    pts.clone(),
                    vis.selection.bg_fill.gamma_multiply(0.18),
                    egui::Stroke::NONE,
                ));
                p.add(egui::Shape::line(
                    closed,
                    egui::Stroke::new(STROKE_CANVAS, vis.selection.stroke.color),
                ));
            } else {
                let stroke = egui::Stroke::new(STROKE_HANDLE, vis.widgets.inactive.fg_stroke.color);
                p.extend(egui::Shape::dashed_line(
                    &closed,
                    stroke,
                    CANVAS_DASH,
                    CANVAS_GAP,
                ));
            }
            let top_left = pts
                .iter()
                .fold(pts[0], |a, q| egui::pos2(a.x.min(q.x), a.y.min(q.y)));
            let galley = p.layout_no_wrap(
                slice_label(i, sl),
                egui::FontId::proportional(FS_SM),
                if on {
                    TEXT
                } else {
                    vis.widgets.inactive.fg_stroke.color
                },
            );
            let at = top_left + egui::vec2(SP_2, SP_2);
            p.rect_filled(
                egui::Rect::from_min_size(at, galley.size()).expand(SP_2 / 2.0),
                2.0,
                crate::theme::BG.gamma_multiply(0.7),
            );
            p.galley(at, galley, TEXT);
        }
        // Primary slice: its rectangle and handles.
        let dims = (scr.width, scr.height);
        let sel_rect = prim.and_then(|sl| slice_geom::handle_rect(sl, side, comp, dims));
        let hit_handle = |at: egui::Pos2| {
            let r = sel_rect?;
            handle_pts(r)
                .iter()
                .enumerate()
                .map(|(i, hp)| (i, view.px(*hp).distance(at)))
                .filter(|(_, d)| *d <= HANDLE_GRAB)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        let slice_at = |at: egui::Pos2| {
            let u = view.unit(at);
            let hit = |s: &&Slice| inside(&outline(s, side, comp, (scr.width, scr.height)), u);
            scr.slices
                .iter()
                .filter(|s| sel_ids.contains(&s.id))
                .find(hit)
                .or_else(|| scr.slices.iter().rev().find(hit))
                .map(|s| s.id)
        };
        let mut dragged_body: Option<Command> = None;
        let points_mode = mode == OutMode::Points;
        let out_tab = tab == OutTab::Output;
        let drawing = out_tab && self.poly.draw.is_some();
        let outline_edit = out_tab
            && !drawing
            && self.poly.edit_outline
            && prim.is_some_and(|s| matches!(s.shape, vjc_model::SliceShape::Polygon(_)));
        let omask_edit = out_tab && !drawing && self.poly.edit_omask && scr.output_mask.is_some();
        let poly_busy = drawing || outline_edit || omask_edit;
        let mask_edit =
            !poly_busy && self.edit_mask && out_tab && prim.is_some_and(|s| s.mask.is_some());
        // Every mode that owns the pointer on the canvas.
        let locked = mask_edit || poly_busy;
        if hand {
            ui.ctx().set_cursor_icon(if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else if resp.hovered() {
                egui::CursorIcon::Grab
            } else {
                egui::CursorIcon::Default
            });
        } else if let Some(h) = resp.hover_pos().and_then(hit_handle)
            && !points_mode
        {
            ui.ctx().set_cursor_icon(cursor_for(h));
        }
        // Start of a drag.
        if resp.drag_started() {
            let at = ui
                .input(|i| i.pointer.press_origin())
                .or(resp.interact_pointer_pos());
            grab = at.and_then(|at| {
                if hand || resp.drag_started_by(egui::PointerButton::Middle) {
                    return Some(Grab::Pan);
                }
                if points_mode || locked {
                    return None;
                }
                if let (Some(h), Some(r)) = (hit_handle(at), sel_rect) {
                    let hp = handle_pts(r)[h];
                    let q = view.unit(at);
                    return Some(Grab::Resize {
                        handle: h,
                        start: r,
                        off: (hp.0 - q.0, hp.1 - q.1),
                    });
                }
                if let Some(id) = slice_at(at) {
                    if !sel_ids.contains(&id) {
                        self.click_slice(scr, id, multi);
                        self.sel_pt_in = None;
                        self.sel_pt_out = None;
                    }
                    let sl = scr.slices.iter().find(|s| s.id == id)?;
                    let start = if prim.is_some_and(|p| p.id == id) || !sel_ids.contains(&id) {
                        slice_geom::handle_rect(sl, side, comp, dims)?
                    } else {
                        sel_rect?
                    };
                    return Some(Grab::Move {
                        start,
                        acc: (0.0, 0.0),
                    });
                }
                Some(Grab::Select {
                    lasso: mods.alt,
                    path: vec![at],
                })
            });
        }
        // The drag goes on.
        if resp.dragged()
            && let Some(g) = grab.clone()
        {
            let cur = resp.interact_pointer_pos();
            let step = self.snap_step(space);
            match g {
                Grab::Pan => {}
                Grab::Move { start, acc } => {
                    let d = resp.drag_delta();
                    // The raw pointer offset is kept; only what is applied gets quantised.
                    let acc = (acc.0 + d.x / scale, acc.1 + d.y / scale);
                    let mut eff = acc;
                    if self.snap {
                        eff.0 = mapping::snap(start.x + acc.0, step.0) - start.x;
                        eff.1 = mapping::snap(start.y + acc.1, step.1) - start.y;
                    }
                    let thr = if self.snap { 0.0 } else { GUIDE_PX / scale };
                    if let Some(sl) = prim {
                        dragged_body = self.drag_body(ui, scr, sl, side, (start, eff), thr);
                    }
                    grab = Some(Grab::Move { start, acc });
                }
                Grab::Resize { handle, start, off } => {
                    if let (Some(at), Some(sl)) = (cur, prim) {
                        let q = view.unit(at);
                        let mut q = (q.0 + off.0, q.1 + off.1);
                        q = (mapping::snap(q.0, step.0), mapping::snap(q.1, step.1));
                        let r = resized(start, handle, q, mods.shift);
                        let pts = slice_geom::side_points(sl, side, comp);
                        let from = slice_geom::handle_rect(sl, side, comp, dims).unwrap_or(start);
                        let new = slice_geom::fit_from(&pts, from, r);
                        if new != pts {
                            dragged_body = Some(set_pts(scr, sl, side, comp, new));
                        }
                    }
                }
                Grab::Select { lasso, mut path } => {
                    if let Some(q) = cur {
                        path.push(q);
                        let line = egui::Stroke::new(STROKE_CANVAS, vis.selection.stroke.color);
                        if lasso {
                            p.add(egui::Shape::line(path.clone(), line));
                        } else {
                            p.rect_stroke(
                                egui::Rect::from_two_pos(path[0], q),
                                0.0,
                                line,
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                    grab = Some(Grab::Select { lasso, path });
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(Grab::Select { lasso, path }) = grab.take()
                && path.len() > 1
            {
                let items: Vec<(Uuid, Rect)> = scr
                    .slices
                    .iter()
                    .filter_map(|s| {
                        Some((
                            s.id,
                            slice_geom::bbox(&slice_geom::side_points(s, side, comp))?,
                        ))
                    })
                    .collect();
                let picked = if lasso {
                    let poly: Vec<_> = path.iter().map(|q| view.unit(*q)).collect();
                    slice_geom::pick_lasso(&items, &poly)
                } else {
                    slice_geom::pick_box(
                        &items,
                        view.unit(path[0]),
                        view.unit(*path.last().unwrap_or(&path[0])),
                    )
                };
                if !picked.is_empty() {
                    self.select_slices(scr, &picked, multi);
                    self.sel_pt_in = None;
                    self.sel_pt_out = None;
                }
            }
            grab = None;
        }
        ui.data_mut(|d| d.insert_temp::<Option<Grab>>(gid, grab));
        // Handles of the primary slice (Transform) or its points (Edit points).
        if let (Some(sl), false) = (prim, hand) {
            if poly_busy {
            } else if mask_edit {
                if let Some(m) = &sl.mask {
                    self.mask_canvas(ui, &resp, (scr, sl), m, view.rect, canvas, cmds);
                }
            } else if points_mode {
                if let Some(c) = self.points_canvas(ui, &resp, (scr, sl), side, comp, view, canvas)
                {
                    dragged_body = dragged_body.or(Some(c));
                }
                if side == Side::Output {
                    self.bezier_handles(ui, (scr, sl), (view.rect, view.scale), canvas, cmds);
                }
            } else if let Some(r) = sel_rect {
                let v = &vis;
                for (i, hp) in handle_pts(r).iter().enumerate() {
                    let rad = if i % 2 == 0 { HANDLE_R } else { HANDLE_R * 0.7 };
                    p.circle(
                        view.px(*hp),
                        rad,
                        v.selection.stroke.color,
                        egui::Stroke::new(STROKE_HANDLE, crate::theme::BG),
                    );
                }
            }
        }
        if drawing {
            self.draw_canvas(ui, &resp, scr, (view.rect, canvas), cmds);
        } else if outline_edit && let Some(sl) = prim {
            if let vjc_model::SliceShape::Polygon(poly) = &sl.shape {
                let mut sel = self.poly.sel;
                let (sid, slid) = (scr.id, sl.id);
                let set = move |pts: Vec<(f32, f32)>| Command::SetSliceShape {
                    screen: sid,
                    slice: slid,
                    shape: vjc_model::SliceShape::Polygon(pts),
                };
                self.poly_canvas(
                    ui,
                    &resp,
                    (egui::Id::new(("slice-shape", sl.id)), space),
                    poly,
                    (view.rect, canvas),
                    &mut sel,
                    &set,
                    cmds,
                );
                self.poly.sel = sel;
            }
        } else if omask_edit && let Some(m) = &scr.output_mask {
            self.poly.follow_screen(scr.id);
            let mut sel = self.poly.sel_omask;
            let (sid, pen) = (scr.id, matches!(m, vjc_model::Mask::Path(_)));
            let set = move |pts: Vec<(f32, f32)>| Command::SetScreenMask {
                screen: sid,
                mask: Some(if pen {
                    vjc_model::Mask::Path(pts)
                } else {
                    vjc_model::Mask::Polygon(pts)
                }),
            };
            self.poly_canvas(
                ui,
                &resp,
                (egui::Id::new(("screen-mask", scr.id)), space),
                m.points(),
                (view.rect, canvas),
                &mut sel,
                &set,
                cmds,
            );
            self.poly.sel_omask = sel;
        }
        if let Some(c) = dragged_body {
            cmds.push(c);
        }
        // Clicks: select the slice under the pointer, or nothing.
        if resp.clicked()
            && !hand
            && !locked
            && let Some(at) = resp.interact_pointer_pos()
        {
            let on_point = points_mode && (self.sel_pt_in.is_some() || self.sel_pt_out.is_some());
            if !on_point {
                match slice_at(at) {
                    Some(id) => {
                        if resp.double_clicked() {
                            self.preview_slice(scr, id);
                        } else {
                            self.click_slice(scr, id, multi);
                        }
                    }
                    None if !multi => {
                        self.sel_slice = None;
                        self.sel_slices.clear();
                    }
                    None => {}
                }
            }
        }
        // Arrow keys move the selection one pixel (ten with Shift) while the pointer is over
        // the canvas and no single point is chosen.
        if !hand
            && !locked
            && let Some(sl) = prim
            && (!points_mode || (self.sel_pt_in.is_none() && self.sel_pt_out.is_none()))
            && let Some(by) = arrow_move(ui, canvas)
            && let Some(c) = self.arrow_nudge(ui, scr, sl, side, by)
        {
            cmds.push(c);
        }
    }

    /// Mesh points (Output) or input corners (Input) of the selected slice.
    #[allow(clippy::too_many_arguments)]
    fn points_canvas(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        (scr, sl): (&Screen, &Slice),
        side: Side,
        comp: (u32, u32),
        view: View,
        clip: egui::Rect,
    ) -> Option<Command> {
        match side {
            Side::Output => {
                if !mapping::mesh_valid(&sl.output_mesh, sl.mesh_cols) {
                    return None;
                }
                self.paint_mesh(ui, sl, &view, clip);
                let space = (scr.width as f32, scr.height as f32);
                let id = egui::Id::new(("slice-out", sl.id));
                let snap = self.snap_step(space);
                let mut sel = self.sel_pt_out;
                let body = slice_geom::bbox(&sl.output_mesh);
                let drag = handles(
                    ui,
                    resp,
                    id,
                    (view.rect, space),
                    &sl.output_mesh,
                    &mut sel,
                    Opts {
                        snap,
                        keys: true,
                        body,
                        clip,
                    },
                );
                self.sel_pt_out = sel;
                self.points_result(ui, (scr, sl), Side::Output, comp, (view, space), drag)
            }
            Side::Input => {
                let id = egui::Id::new(("slice-in", sl.id));
                let mut sel = self.sel_pt_in;
                let body = slice_geom::bbox(&sl.input);
                let drag = handles(
                    ui,
                    resp,
                    id,
                    (view.rect, (1.0, 1.0)),
                    &sl.input,
                    &mut sel,
                    Opts {
                        snap: (0.0, 0.0),
                        keys: false,
                        body,
                        clip,
                    },
                );
                self.sel_pt_in = sel;
                self.points_result(ui, (scr, sl), Side::Input, comp, (view, (1.0, 1.0)), drag)
            }
        }
    }

    /// The command a point or body drag asks for.
    fn points_result(
        &mut self,
        ui: &egui::Ui,
        (scr, sl): (&Screen, &Slice),
        side: Side,
        comp: (u32, u32),
        (view, space): (View, (f32, f32)),
        drag: Drag,
    ) -> Option<Command> {
        let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
        if let Some((start, acc)) = drag.body {
            // Normalized input units -> composition pixels, where Nudge works.
            let (start, acc) = match side {
                Side::Input => (
                    Rect {
                        x: start.x * cw,
                        y: start.y * ch,
                        w: start.w * cw,
                        h: start.h * ch,
                    },
                    (acc.0 * cw, acc.1 * ch),
                ),
                Side::Output => (start, acc),
            };
            let thr = if self.snap {
                0.0
            } else {
                GUIDE_PX / view.scale
            };
            return self.drag_body(ui, scr, sl, side, (start, acc), thr);
        }
        let (i, q) = drag.moved?;
        let others: Vec<_> = scr
            .slices
            .iter()
            .filter(|s| s.id != sl.id)
            .filter_map(|s| match side {
                Side::Input => slice_geom::bbox(&s.input),
                Side::Output => slice_geom::bbox(&s.output_mesh),
            })
            .collect();
        // Space units per canvas point: the input quad is normalised to the composition.
        let per_pt = match side {
            Side::Input => 1.0 / (view.scale * cw),
            Side::Output => 1.0 / view.scale,
        };
        let q = snap_dragged(ui, q, space, &others, per_pt);
        Some(match side {
            Side::Input => {
                let mut input = sl.input.clone();
                *input.get_mut(i)? = q;
                Command::SetSliceInput {
                    screen: scr.id,
                    slice: sl.id,
                    input,
                }
            }
            Side::Output => {
                let mut mesh = sl.output_mesh.clone();
                *mesh.get_mut(i)? = q;
                Command::SetSliceMesh {
                    screen: scr.id,
                    slice: sl.id,
                    mesh,
                    cols: sl.mesh_cols,
                }
            }
        })
    }

    /// Grid lines of the selected slice's warp mesh.
    fn paint_mesh(&self, ui: &egui::Ui, sl: &Slice, view: &View, clip: egui::Rect) {
        let p = ui.painter_at(clip);
        let (cols, rows) = (
            sl.mesh_cols as usize,
            mapping::rows_of(sl.output_mesh.len(), sl.mesh_cols) as usize,
        );
        let stroke = ui.visuals().widgets.noninteractive.fg_stroke;
        let m = &sl.output_mesh;
        for j in 0..rows {
            for i in 0..cols {
                let a = view.px(m[j * cols + i]);
                if i + 1 < cols {
                    p.line_segment([a, view.px(m[j * cols + i + 1])], stroke);
                }
                if j + 1 < rows {
                    p.line_segment([a, view.px(m[(j + 1) * cols + i])], stroke);
                }
            }
        }
    }

    /// The live render of the screen, or (before it exists) the slices drawn from the
    /// composition texture with their meshes.
    fn paint_screen(&self, p: &egui::Painter, scr: &Screen, view: &View) {
        if let Some(t) = self.screen_rt.get(&scr.id) {
            p.image(t.id, view.rect, full_uv(), egui::Color32::WHITE);
            return;
        }
        let Some(tex) = self.comp_tex else { return };
        for sl in &scr.slices {
            if !mapping::mesh_valid(&sl.output_mesh, sl.mesh_cols) {
                continue;
            }
            let (cols, rows) = (
                sl.mesh_cols,
                mapping::rows_of(sl.output_mesh.len(), sl.mesh_cols),
            );
            let mut mesh = egui::Mesh::with_texture(tex);
            for j in 0..rows {
                for i in 0..cols {
                    let uv = mapping::node_uv(&sl.input, i, j, cols, rows);
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: view.px(sl.output_mesh[(j * cols + i) as usize]),
                        uv: egui::pos2(uv.0, uv.1),
                        color: egui::Color32::WHITE,
                    });
                }
            }
            for j in 0..rows - 1 {
                for i in 0..cols - 1 {
                    let a = j * cols + i;
                    mesh.add_triangle(a, a + 1, a + cols);
                    mesh.add_triangle(a + 1, a + cols + 1, a + cols);
                }
            }
            p.add(egui::Shape::mesh(mesh));
        }
    }

    /// Mask polygon over the output canvas (normalized screen coordinates).
    #[allow(clippy::too_many_arguments)]
    fn mask_canvas(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        (scr, sl): (&Screen, &Slice),
        m: &[(f32, f32)],
        rect: egui::Rect,
        clip: egui::Rect,
        cmds: &mut Vec<Command>,
    ) {
        let (sid, slid) = (scr.id, sl.id);
        let set = move |mask: Vec<(f32, f32)>| Command::SetSliceMask {
            screen: sid,
            slice: slid,
            mask: Some(mask),
        };
        let mut sel = self.sel_mask_pt;
        let size = (scr.width.max(1) as f32, scr.height.max(1) as f32);
        self.poly_canvas(
            ui,
            resp,
            (egui::Id::new(("slice-mask", sl.id)), size),
            m,
            (rect, clip),
            &mut sel,
            &set,
            cmds,
        );
        self.sel_mask_pt = sel;
    }

    /// Guide image of a screen, if one was loaded this session.
    pub(crate) fn guide_texture(ctx: &egui::Context, screen: Uuid) -> Option<egui::TextureHandle> {
        ctx.data(|d| d.get_temp::<egui::TextureHandle>(guide_id(screen)))
    }

    /// Loads an image file as the guide of `screen`.
    pub(crate) fn load_guide(&mut self, ctx: &egui::Context, screen: Uuid, path: &Path) {
        match crate::reliability::image_open(path) {
            Ok(img) => {
                let img = if img.width().max(img.height()) > GUIDE_MAX_SIDE {
                    img.thumbnail(GUIDE_MAX_SIDE, GUIDE_MAX_SIDE)
                } else {
                    img
                };
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let ci = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                let tex =
                    ctx.load_texture(format!("guide-{screen}"), ci, egui::TextureOptions::LINEAR);
                ctx.data_mut(|d| d.insert_temp(guide_id(screen), tex));
                if let Some(w) = &mut self.out_win {
                    w.guide_on = true;
                }
                self.status = format!("Guide image loaded: {}", path.display());
            }
            Err(e) => self.status = format!("Could not read the guide image: {e}"),
        }
    }

    /// Removes the guide image of `screen`.
    pub(crate) fn clear_guide(&mut self, ctx: &egui::Context, screen: Uuid) {
        ctx.data_mut(|d| d.remove::<egui::TextureHandle>(guide_id(screen)));
    }

    /// Files dropped on the open window become the guide image of the selected screen.
    /// Returns the drops left for the rest of the app (none when the window took them).
    pub(crate) fn out_take_drops(
        &mut self,
        ctx: &egui::Context,
        dropped: Vec<std::path::PathBuf>,
    ) -> Vec<std::path::PathBuf> {
        let Some(w) = &self.out_win else {
            return dropped;
        };
        let over = ctx
            .input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()))
            .is_some_and(|q| w.rect.contains(q));
        if dropped.is_empty() || !over {
            return dropped;
        }
        match (self.sel_screen, dropped.first()) {
            (Some(id), Some(path)) => self.load_guide(ctx, id, path),
            _ => self.status = "Select a screen to give it a guide image".into(),
        }
        vec![]
    }
}

/// Longest side kept of a guide image.
const GUIDE_MAX_SIDE: u32 = 4096;

fn guide_id(screen: Uuid) -> egui::Id {
    egui::Id::new(("out-guide", screen))
}

fn full_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}
