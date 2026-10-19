//! "Output setup" window (Output > Setup...): a tree of screens and slices, an Input / Output
//! canvas, and a properties panel.
//!
//! The window edits a working copy of the project (`OutWin`). Edits go through the usual
//! commands into a private history, so the window has its own undo and redo. The live
//! project shows the working screens while the window is open (a preview: outputs and the
//! canvas follow the edits). Cancel puts the screens back; Save & Close applies the whole
//! result as ONE `SetOutputSetup` on the project history.
//!
//! This file holds the window shell, the working copy, selection helpers and the point
//! handles; the tree, canvas, properties and Art-Net parts are in their own files.

use crate::State;
use crate::output_hints::{self, hint};
use crate::theme::{
    HANDLE_GRAB_K, HANDLE_R, OUTPUT_FOOTER_H, OUTPUT_HELP_H, OUTPUT_LIST_W, OUTPUT_MIN_H,
    OUTPUT_MIN_W, OUTPUT_PROPS_W, OUTPUT_WIN_X, OUTPUT_WIN_Y, SP_4, STROKE_HANDLE,
};
use crate::widgets::{self, Kind, panel_window};
use eframe::egui;
use std::collections::HashSet;
use std::time::Instant;
use uuid::Uuid;
use vjc_core::slice_geom;
use vjc_core::{Command, History, mapping};
use vjc_model::{OutputDevice, Screen};

/// Which side of the slices the canvas shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutTab {
    /// Where each slice samples the composition.
    Input,
    /// Where each slice lands on the screen.
    Output,
}

/// What a drag on the canvas edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutMode {
    /// Whole slices, with box handles.
    Transform,
    /// Single points of the slice (warp mesh on Output, quad on Input).
    Points,
}

/// Grid drawn over the canvas and used by Snap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GridKind {
    Off,
    Two,
    Three,
    Custom,
}

/// The working copy of the Output setup and the state of the window around it.
pub(crate) struct OutWin {
    /// Screens when the window opened: what Cancel restores.
    orig: Vec<Screen>,
    /// Working project: only `output` is edited; the rest is a copy so commands validate.
    pub(crate) proj: vjc_model::Project,
    hist: History,
    /// Screens this window last pushed to the live project. A different live value means
    /// something else edited them (undo from the menu, the API): the copy follows.
    synced: Vec<Screen>,
    /// Screens before a running gesture (a drag, a multi-slice edit); the gesture lands in
    /// the history as one step when it ends.
    pre: Option<Vec<Screen>>,
    /// Pointer button held this frame.
    pub(crate) down: bool,
    pub(crate) tab: OutTab,
    pub(crate) mode: OutMode,
    /// Canvas scale (canvas points per content pixel); `None` = fit the window.
    pub(crate) zoom: Option<f32>,
    /// Offset of the content centre from the canvas centre, in canvas points.
    pub(crate) pan: egui::Vec2,
    pub(crate) hand: bool,
    pub(crate) grid: GridKind,
    pub(crate) grid_custom: (u32, u32),
    /// Grid size the "Add grid of slices" entry creates.
    pub(crate) add_grid: (u32, u32),
    pub(crate) guide_on: bool,
    /// Slice outlines and names are drawn on the canvas.
    pub(crate) show_slices: bool,
    /// LED cell grid over the canvas, and its cell size in content pixels.
    pub(crate) led_grid: bool,
    pub(crate) led_cell: u32,
    /// Slice numbers are drawn big over the slices.
    pub(crate) identify: bool,
    /// The LED grid and the slice numbers also show on the output windows.
    pub(crate) show_in_output: bool,
    /// Tree row being renamed and its text.
    pub(crate) rename: Option<(Uuid, String)>,
    /// Screens folded in the tree.
    pub(crate) folded: HashSet<Uuid>,
    /// Outer rectangle of the window (file drops over it go to the guide image).
    pub(crate) rect: egui::Rect,
}

impl OutWin {
    /// Grid divisions in use, if any.
    pub(crate) fn grid_dims(&self) -> Option<(u32, u32)> {
        match self.grid {
            GridKind::Off => None,
            GridKind::Two => Some((2, 2)),
            GridKind::Three => Some((3, 3)),
            GridKind::Custom => Some((self.grid_custom.0.max(1), self.grid_custom.1.max(1))),
        }
    }

    pub(crate) fn can_undo(&self) -> bool {
        self.hist.can_undo() || self.pre.is_some()
    }

    pub(crate) fn can_redo(&self) -> bool {
        self.hist.can_redo()
    }
}

/// Behaviour switches of `handles`: grid snap per axis (0 = off), arrow-key nudging of the
/// selected point, the slice rectangle that a drag off every handle moves, and the part of
/// the screen that reacts (the zoomed content can be larger than the canvas).
pub(crate) struct Opts {
    pub(crate) snap: (f32, f32),
    pub(crate) keys: bool,
    pub(crate) body: Option<slice_geom::Rect>,
    pub(crate) clip: egui::Rect,
}

/// What a canvas drag produced this frame.
#[derive(Default)]
pub(crate) struct Drag {
    /// A point moved to a new position `(index, position)`.
    pub(crate) moved: Option<(usize, (f32, f32))>,
    /// The slice body is being dragged: its rectangle when the drag began, and the offset
    /// since then (both in the canvas space units).
    pub(crate) body: Option<(slice_geom::Rect, (f32, f32))>,
}

type BodyDrag = Option<(slice_geom::Rect, (f32, f32))>;

/// Draggable handles for `pts` (in `space` units) over `rect`. Returns the moved point as
/// `(index, new position)`; a drag that starts off every handle but inside `body` drags the
/// whole slice. Arrow keys nudge the selected point 1 unit (10 with Shift) when
/// `keys` is set and the pointer is over the canvas. `resp` is the response of the whole canvas.
pub(crate) fn handles(
    ui: &mut egui::Ui,
    resp: &egui::Response,
    id: egui::Id,
    (rect, space): (egui::Rect, (f32, f32)),
    pts: &[(f32, f32)],
    sel: &mut Option<usize>,
    Opts {
        snap,
        keys,
        body,
        clip,
    }: Opts,
) -> Drag {
    let to_px = |p: (f32, f32)| {
        egui::pos2(
            rect.min.x + p.0 / space.0 * rect.width(),
            rect.min.y + p.1 / space.1 * rect.height(),
        )
    };
    let to_unit = |q: egui::Pos2| {
        (
            (q.x - rect.min.x) / rect.width() * space.0,
            (q.y - rect.min.y) / rect.height() * space.1,
        )
    };
    let radius = HANDLE_R * HANDLE_GRAB_K * space.0 / rect.width();
    let drag_id = id.with("drag");
    let body_id = id.with("body");
    let mut moved = None;
    let mut body_drag = None;
    if let Some(q) = resp.interact_pointer_pos() {
        let hit = mapping::hit_test(pts, to_unit(q), radius);
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, hit));
            if hit.is_some() {
                *sel = hit;
            } else if let Some(r) = body {
                let u = to_unit(q);
                let inside = u.0 >= r.x && u.0 <= r.x + r.w && u.1 >= r.y && u.1 <= r.y + r.h;
                ui.data_mut(|d| {
                    d.insert_temp::<BodyDrag>(body_id, inside.then_some((r, (0.0, 0.0))))
                });
            }
        } else if resp.clicked() {
            *sel = hit;
        }
        let dragging: Option<usize> = ui.data(|d| d.get_temp::<Option<usize>>(drag_id).flatten());
        if resp.dragged()
            && let Some(i) = dragging
        {
            let u = to_unit(q);
            let c = |v: f32, max: f32, step: f32| mapping::snap(v, step).clamp(0.0, max);
            moved = Some((i, (c(u.0, space.0, snap.0), c(u.1, space.1, snap.1))));
        }
    }
    if resp.dragged()
        && let Some((r, acc)) = ui.data(|d| d.get_temp::<BodyDrag>(body_id)).flatten()
    {
        let dd = resp.drag_delta();
        let acc = (
            acc.0 + dd.x / rect.width() * space.0,
            acc.1 + dd.y / rect.height() * space.1,
        );
        ui.data_mut(|d| d.insert_temp::<BodyDrag>(body_id, Some((r, acc))));
        body_drag = Some((r, acc));
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| {
            d.insert_temp::<Option<usize>>(drag_id, None);
            d.insert_temp::<BodyDrag>(body_id, None);
        });
    }
    if keys
        && resp.hovered()
        && let Some(i) = *sel
        && i < pts.len()
    {
        let (dx, dy, step) = ui.input(|inp| {
            let step = if inp.modifiers.shift { 10.0 } else { 1.0 };
            let k = |a, b| (inp.key_pressed(b) as i32 - inp.key_pressed(a) as i32) as f32;
            (
                k(egui::Key::ArrowLeft, egui::Key::ArrowRight),
                k(egui::Key::ArrowUp, egui::Key::ArrowDown),
                step,
            )
        });
        if dx != 0.0 || dy != 0.0 {
            let p = pts[i];
            moved = Some((
                i,
                (
                    (p.0 + dx * step).clamp(0.0, space.0),
                    (p.1 + dy * step).clamp(0.0, space.1),
                ),
            ));
        }
    }
    let v = ui.visuals();
    let (on, off) = (v.selection.bg_fill, v.widgets.inactive.fg_stroke.color);
    let p = ui.painter_at(clip);
    for (i, pt) in pts.iter().enumerate() {
        let on_it = *sel == Some(i);
        p.circle(
            to_px(*pt),
            HANDLE_R,
            if on_it {
                on
            } else {
                egui::Color32::TRANSPARENT
            },
            egui::Stroke::new(
                STROKE_HANDLE,
                if on_it { v.selection.stroke.color } else { off },
            ),
        );
    }
    Drag {
        moved,
        body: body_drag,
    }
}

/// A child region of `ui` with its own id, clipped to `rect`.
pub(crate) fn region(
    ui: &mut egui::Ui,
    salt: &str,
    rect: egui::Rect,
    body: impl FnOnce(&mut egui::Ui),
) {
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(salt)
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    c.set_clip_rect(rect.intersect(ui.clip_rect()));
    body(&mut c);
}

/// Applies `cmd` to the working project without a history entry. A `Batch` is all or
/// nothing; only the screens are saved for the rollback (the command in the core clones the
/// whole project, which a drag would repeat every frame).
fn apply_raw(p: &mut vjc_model::Project, cmd: Command) -> Result<(), vjc_core::Error> {
    match cmd {
        Command::Batch { commands } => {
            let backup = p.output.screens.clone();
            for c in commands {
                if let Err(e) = apply_raw(p, c) {
                    p.output.screens = backup;
                    return Err(e);
                }
            }
            Ok(())
        }
        c => vjc_core::apply(p, c).map(|_| ()),
    }
}

/// What the footer asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Footer {
    Cancel,
    Save,
}

impl State {
    /// Screens of the working copy (the live project's when the window is closed).
    pub(crate) fn out_screens(&self) -> Vec<Screen> {
        match &self.out_win {
            Some(w) => w.proj.output.screens.clone(),
            None => self.project.output.screens.clone(),
        }
    }

    pub(crate) fn out_open(&mut self) {
        let screens = self.project.output.screens.clone();
        self.out_win = Some(OutWin {
            orig: screens.clone(),
            proj: self.project.clone(),
            hist: History::new(),
            synced: screens,
            pre: None,
            down: false,
            tab: OutTab::Input,
            mode: OutMode::Transform,
            zoom: None,
            pan: egui::Vec2::ZERO,
            hand: false,
            grid: GridKind::Off,
            grid_custom: (4, 4),
            add_grid: (3, 2),
            guide_on: true,
            show_slices: true,
            led_grid: false,
            led_cell: crate::output_extras::LED_CELL_DEFAULT,
            identify: false,
            show_in_output: false,
            rename: None,
            folded: HashSet::new(),
            rect: egui::Rect::NOTHING,
        });
        self.sel_slice = None;
        self.sel_slices.clear();
        self.sel_pt_in = None;
        self.sel_pt_out = None;
        self.sel_mask_pt = None;
        self.edit_mask = false;
        self.poly = Default::default();
    }

    /// Cancel: the live project gets its screens back and the working copy is dropped.
    pub(crate) fn out_cancel(&mut self) {
        if let Some(w) = self.out_win.take() {
            self.project.output.screens = w.orig;
        }
        self.output_setup_open = false;
    }

    /// Save & Close: the whole result is ONE undoable `SetOutputSetup`. If the project
    /// refuses it (a monitor used twice) the window stays open and the status says why.
    pub(crate) fn out_save(&mut self) {
        self.out_end_gesture(true);
        let Some(w) = self.out_win.take() else {
            return;
        };
        let new = w.proj.output.screens.clone();
        if new == w.orig {
            self.output_setup_open = false;
            return;
        }
        self.project.output.screens = w.orig.clone();
        if self.exec_ok(Command::SetOutputSetup {
            screens: new.clone(),
        }) {
            for s in &new {
                if w.orig
                    .iter()
                    .find(|o| o.id == s.id)
                    .is_some_and(|o| o.device != s.device)
                {
                    self.closed_screens.remove(&s.id);
                }
            }
            self.output_setup_open = false;
        } else {
            self.project.output.screens = new;
            self.out_win = Some(w);
        }
    }

    /// An edit from outside the window (undo from the menu, the API) while it is open: the
    /// working copy follows the live project.
    fn out_rebase(&mut self) {
        let live = &self.project.output.screens;
        if let Some(w) = &mut self.out_win
            && *live != w.synced
        {
            w.proj.output.screens = live.clone();
            w.synced = live.clone();
            w.pre = None;
        }
    }

    /// Pushes the working screens to the live project (the preview).
    fn out_push(&mut self) {
        if let Some(w) = &mut self.out_win
            && w.proj.output.screens != w.synced
        {
            w.synced = w.proj.output.screens.clone();
            self.project.output.screens = w.synced.clone();
        }
    }

    fn out_now(&self) -> Instant {
        #[cfg(test)]
        return self.history_now.unwrap_or_else(Instant::now);
        #[cfg(not(test))]
        Instant::now()
    }

    /// Runs `cmd` on the working copy. While a gesture runs (pointer held, or a command made
    /// of several) it applies without a history entry; `out_end_gesture` records the whole
    /// gesture as one step.
    pub(crate) fn out_exec(&mut self, cmd: Command) -> bool {
        let now = self.out_now();
        let Some(w) = &mut self.out_win else {
            return false;
        };
        let gesture = w.down || w.pre.is_some() || matches!(cmd, Command::Batch { .. });
        let r = if gesture {
            if w.pre.is_none() {
                w.pre = Some(w.proj.output.screens.clone());
            }
            apply_raw(&mut w.proj, cmd)
        } else {
            w.hist.exec(&mut w.proj, cmd, now).map(|_| ())
        };
        match r {
            Ok(()) => {
                self.out_push();
                true
            }
            Err(e) => {
                self.status = format!("Command failed: {e}");
                false
            }
        }
    }

    /// Ends the running gesture once the pointer is up (or at once with `force`): its
    /// result becomes one history step.
    pub(crate) fn out_end_gesture(&mut self, force: bool) {
        let now = self.out_now();
        let Some(w) = &mut self.out_win else {
            return;
        };
        if w.pre.is_none() || (w.down && !force) {
            return;
        }
        let Some(pre) = w.pre.take() else { return };
        let cur = w.proj.output.screens.clone();
        if cur == pre {
            return;
        }
        w.proj.output.screens = pre;
        if let Err(e) = w.hist.exec(
            &mut w.proj,
            Command::SetOutputSetup {
                screens: cur.clone(),
            },
            now,
        ) {
            w.proj.output.screens = cur;
            self.status = format!("Command failed: {e}");
        }
        self.out_push();
    }

    pub(crate) fn out_undo(&mut self) {
        self.out_end_gesture(true);
        if let Some(w) = &mut self.out_win {
            let _ = w.hist.undo(&mut w.proj);
        }
        self.out_push();
        self.prune_sel_slices();
    }

    pub(crate) fn out_redo(&mut self) {
        self.out_end_gesture(true);
        if let Some(w) = &mut self.out_win {
            let _ = w.hist.redo(&mut w.proj);
        }
        self.out_push();
        self.prune_sel_slices();
    }

    /// Runs a command of the setup: on the working copy while the window is open, otherwise
    /// on the project.
    pub(crate) fn output_exec(&mut self, cmd: Command) -> bool {
        if self.out_win.is_some() {
            self.out_exec(cmd)
        } else {
            self.exec_ok(cmd)
        }
    }

    /// The screen to preview in the Output tab, if the window shows it.
    pub(crate) fn out_preview_screen(&self) -> Option<Uuid> {
        self.out_win
            .as_ref()
            .filter(|w| w.tab == OutTab::Output)
            .and(self.sel_screen)
    }

    /// Keeps the selection pointing at things that exist.
    fn out_fix_selection(&mut self, screens: &[Screen]) {
        if !self
            .sel_screen
            .is_some_and(|id| screens.iter().any(|s| s.id == id))
        {
            self.sel_screen = screens.first().map(|s| s.id);
            self.sel_slice = None;
            self.sel_slices.clear();
        }
        let scr = screens.iter().find(|s| Some(s.id) == self.sel_screen);
        if !self
            .sel_slice
            .is_some_and(|id| scr.is_some_and(|s| s.slices.iter().any(|x| x.id == id)))
        {
            self.sel_slice = None;
            self.sel_slices.clear();
        }
    }

    pub(crate) fn output_setup_window(&mut self, ctx: &egui::Context) {
        if !self.output_setup_open {
            if self.out_win.is_some() {
                self.out_cancel();
            }
            self.preset_names = None;
            return;
        }
        if self.out_win.is_none() {
            self.out_open();
        }
        self.out_rebase();
        let down = ctx.input(|i| i.pointer.any_down());
        if let Some(w) = &mut self.out_win {
            w.down = down;
        }
        let screens = self.out_screens();
        self.out_fix_selection(&screens);
        let screen = ctx.content_rect().size();
        let size = egui::vec2(
            (screen.x - 80.0).clamp(900.0, 1500.0).min(screen.x),
            (screen.y - 100.0).clamp(560.0, 920.0).min(screen.y),
        );
        let mut open = true;
        let mut cmds: Vec<Command> = vec![];
        let mut footer = None;
        let shown = panel_window(ctx, "Output setup")
            .open(&mut open)
            .resizable(true)
            .default_size(size)
            .min_size(egui::vec2(OUTPUT_MIN_W, OUTPUT_MIN_H))
            // Open below the menu bar: the default position covered "Output" and ate its clicks.
            .default_pos(egui::pos2(OUTPUT_WIN_X, OUTPUT_WIN_Y))
            .show(ctx, |ui| {
                self.out_body(ui, &screens, &mut cmds, &mut footer);
            });
        if let Some(r) = shown
            && let Some(w) = &mut self.out_win
        {
            w.rect = r.response.rect;
        }
        output_hints::end_frame(ctx);
        self.run_output_cmds(cmds);
        self.out_end_gesture(false);
        match footer {
            Some(Footer::Save) => self.out_save(),
            Some(Footer::Cancel) => self.out_cancel(),
            None if !open => self.out_cancel(),
            None => {}
        }
    }

    fn out_body(
        &mut self,
        ui: &mut egui::Ui,
        screens: &[Screen],
        cmds: &mut Vec<Command>,
        footer: &mut Option<Footer>,
    ) {
        self.presets_row(ui);
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, egui::Sense::hover());
        let left =
            egui::Rect::from_min_max(full.min, egui::pos2(full.min.x + OUTPUT_LIST_W, full.max.y));
        let right = egui::Rect::from_min_max(
            egui::pos2(
                (full.max.x - OUTPUT_PROPS_W).max(left.max.x + SP_4),
                full.min.y,
            ),
            full.max,
        );
        let mid = egui::Rect::from_min_max(
            egui::pos2(left.max.x + SP_4, full.min.y),
            egui::pos2(right.min.x - SP_4, full.max.y),
        );
        let line = ui.visuals().widgets.noninteractive.bg_stroke;
        for x in [left.max.x + SP_4 / 2.0, right.min.x - SP_4 / 2.0] {
            ui.painter().vline(x, full.y_range(), line);
        }
        // Left: tree above, help below.
        let tree = egui::Rect::from_min_max(
            left.min,
            egui::pos2(left.max.x, left.max.y - OUTPUT_HELP_H - SP_4),
        );
        let help =
            egui::Rect::from_min_max(egui::pos2(left.min.x, left.max.y - OUTPUT_HELP_H), left.max);
        region(ui, "out-tree", tree, |ui| self.out_tree(ui, screens, cmds));
        region(ui, "out-help", help, |ui| self.out_help_panel(ui));
        // Centre: toolbar and canvas.
        region(ui, "out-centre", mid, |ui| {
            self.out_toolbar(ui);
            self.out_canvas(ui, screens, cmds);
        });
        // Right: properties above, Cancel / Save & Close below.
        let props = egui::Rect::from_min_max(
            right.min,
            egui::pos2(right.max.x, right.max.y - OUTPUT_FOOTER_H - SP_4),
        );
        let foot = egui::Rect::from_min_max(
            egui::pos2(right.min.x, right.max.y - OUTPUT_FOOTER_H),
            right.max,
        );
        region(ui, "out-props", props, |ui| {
            self.out_props(ui, screens, cmds);
        });
        region(ui, "out-footer", foot, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if hint(widgets::btn(ui, "Save & Close", Kind::Primary), "save").clicked() {
                    *footer = Some(Footer::Save);
                }
                if hint(widgets::btn(ui, "Cancel", Kind::Secondary), "cancel").clicked() {
                    *footer = Some(Footer::Cancel);
                }
            });
        });
    }

    /// "Display info" window: name, resolution and refresh of each monitor, and which
    /// screens are bound to it.
    pub(crate) fn display_info_window(&mut self, ctx: &egui::Context) {
        if !self.display_info_open {
            return;
        }
        let mut open = true;
        panel_window(ctx, "Display info")
            .open(&mut open)
            .show(ctx, |ui| {
                if self.monitors.is_empty() {
                    ui.label("No monitors found");
                }
                for m in &self.monitors {
                    ui.strong(&m.name);
                    let hz = if m.refresh_hz > 0.0 {
                        format!("{:.0} Hz", m.refresh_hz)
                    } else {
                        "refresh unknown".into()
                    };
                    ui.label(format!("{} x {} px, {}", m.width, m.height, hz));
                    for s in &self.project.output.screens {
                        if matches!(&s.device, OutputDevice::Monitor { name } if *name == m.id.to_string())
                        {
                            ui.weak(format!("Screen: {} ({} x {})", s.name, s.width, s.height));
                        }
                    }
                }
            });
        self.display_info_open = open;
    }

    /// Runs the commands of the setup window. A screen reopens only if its device change
    /// was accepted; a monitor already used by another screen is reported.
    pub(crate) fn run_output_cmds(&mut self, cmds: Vec<Command>) {
        /// Selection that follows a command once it was accepted.
        enum Sel {
            /// A slice (and its screen when it moved there).
            Slice(Option<Uuid>, Uuid),
            Screen(Uuid),
        }
        for c in cmds {
            let reopen = match &c {
                Command::SetScreenDevice { screen, .. } => Some(*screen),
                _ => None,
            };
            let monitor_dup = matches!(
                &c,
                Command::SetScreenDevice {
                    device: OutputDevice::Monitor { .. },
                    ..
                }
            );
            let select = match &c {
                Command::AddSlice { slice, .. } => Some(Sel::Slice(None, slice.id)),
                Command::MoveSlice { to, slice, .. } => Some(Sel::Slice(Some(*to), *slice)),
                Command::AddScreen { screen, .. } => Some(Sel::Screen(screen.id)),
                // A batch that adds slices (duplicate, mirror, grid) selects the last one.
                Command::Batch { commands } => commands.iter().rev().find_map(|c| match c {
                    Command::AddSlice { slice, .. } => Some(Sel::Slice(None, slice.id)),
                    _ => None,
                }),
                _ => None,
            };
            let removes = match &c {
                Command::RemoveSlice { .. } | Command::RemoveScreen { .. } => true,
                Command::Batch { commands } => commands
                    .iter()
                    .any(|c| matches!(c, Command::RemoveSlice { .. })),
                _ => false,
            };
            let ok = self.output_exec(c);
            if ok {
                match select {
                    Some(Sel::Slice(screen, slice)) => {
                        if let Some(screen) = screen {
                            self.sel_screen = Some(screen);
                        }
                        self.sel_slice = Some(slice);
                        self.sel_slices.clear();
                    }
                    Some(Sel::Screen(id)) => {
                        self.sel_screen = Some(id);
                        self.sel_slice = None;
                        self.sel_slices.clear();
                    }
                    None => {}
                }
                if removes {
                    self.sel_slice = None;
                    self.sel_slices.clear();
                }
            }
            match reopen {
                // Only the edited screen reopens, and only if the change took effect.
                Some(screen) if ok => {
                    self.closed_screens.remove(&screen);
                }
                Some(_) if monitor_dup => {
                    self.status = "That monitor already shows another screen".into();
                }
                _ => {}
            }
        }
    }

    /// Click on a slice: plain selects only it; shift / cmd toggles it in the
    /// multi-selection (the clicked slice becomes the primary one when added).
    pub(crate) fn click_slice(&mut self, scr: &Screen, id: Uuid, multi: bool) {
        self.sel_screen = Some(scr.id);
        if !multi {
            self.sel_slice = Some(id);
            self.sel_slices.clear();
            return;
        }
        let primary = self.sel_slice;
        let mut set: Vec<Uuid> = scr
            .slices
            .iter()
            .filter(|s| Some(s.id) == primary || self.sel_slices.contains(&s.id))
            .map(|s| s.id)
            .collect();
        if let Some(i) = set.iter().position(|x| *x == id) {
            set.remove(i);
            if primary == Some(id) {
                self.sel_slice = set.first().copied();
            }
        } else {
            set.push(id);
            self.sel_slice = Some(id);
        }
        self.sel_slices = if set.len() > 1 { set } else { vec![] };
    }

    /// Short description of where a screen goes, e.g. "Window".
    pub(crate) fn device_label(&self, s: &Screen) -> String {
        match &s.device {
            OutputDevice::Monitor { name } => self
                .monitors
                .iter()
                .find(|m| m.id.to_string() == *name)
                .map_or("Monitor (missing)".to_string(), |m| m.label()),
            OutputDevice::Syphon => "Syphon".into(),
            OutputDevice::Ndi => "NDI".into(),
            OutputDevice::ArtNet { .. } => "Art-Net".into(),
            OutputDevice::Window => "Window".into(),
        }
    }

    /// The screen being edited and, if one is selected, the primary slice.
    pub(crate) fn out_selected<'a>(
        &self,
        screens: &'a [Screen],
    ) -> Option<(&'a Screen, Option<&'a vjc_model::Slice>)> {
        let scr = screens
            .iter()
            .find(|s| Some(s.id) == self.sel_screen)
            .or(screens.first())?;
        let sl = scr.slices.iter().find(|s| Some(s.id) == self.sel_slice);
        Some((scr, sl))
    }
}
