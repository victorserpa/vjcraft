//! Left column of the Output setup window: the tree of screens and slices (select, rename,
//! context menu, "+" menu) and the help panel that explains the hovered control.

use crate::State;
use crate::icons;
use crate::output_extras::Axis;
use crate::output_hints::{self, hint};
use crate::output_poly::DrawFor;
use crate::output_ui::OutTab;
use crate::theme::{
    ACCENT, BG, BORDER, FS_SM, FS_XS, ON_ACCENT, RADIUS_SM, SP_2, SP_3, SURFACE_RAISED, TEXT,
    TEXT_MUTED, TREE_INDENT, TREE_SCREEN_H, bold_font,
};
use crate::widgets;
use eframe::egui::{self, RichText};
use uuid::Uuid;
use vjc_core::slice_adv;
use vjc_core::slice_geom::{self, Side};
use vjc_core::{Command, mapping};
use vjc_model::{Screen, Slice};

/// A new mask is an inset square.
pub(crate) const MASK_INSET: f32 = 0.1;

/// Name shown for the slice at position `i`: its own, or "Slice N".
pub(crate) fn slice_label(i: usize, s: &Slice) -> String {
    if s.name.trim().is_empty() {
        format!("Slice {}", i + 1)
    } else {
        s.name.clone()
    }
}

/// "1 slice" / "8 slices": the count shown on a screen row.
pub(crate) fn slice_count_label(n: usize) -> String {
    if n == 1 {
        "1 slice".into()
    } else {
        format!("{n} slices")
    }
}

/// "Screen N" with the lowest N (from the count + 1 up) that no screen uses yet.
pub(crate) fn next_screen_name(screens: &[Screen]) -> String {
    (screens.len() + 1..)
        .map(|n| format!("Screen {n}"))
        .find(|name| screens.iter().all(|s| s.name != *name))
        .unwrap_or_default()
}

/// Square mask inset from the slice edges.
pub(crate) fn inset_mask() -> Vec<(f32, f32)> {
    vec![
        (MASK_INSET, MASK_INSET),
        (1.0 - MASK_INSET, MASK_INSET),
        (1.0 - MASK_INSET, 1.0 - MASK_INSET),
        (MASK_INSET, 1.0 - MASK_INSET),
    ]
}

/// One command from several: the command itself for one, a flat `Batch` for more.
pub(crate) fn one_command(mut cmds: Vec<Command>) -> Option<Command> {
    let mut flat = vec![];
    for c in cmds.drain(..) {
        match c {
            Command::Batch { commands } => flat.extend(commands),
            c => flat.push(c),
        }
    }
    match flat.len() {
        0 => None,
        1 => flat.pop(),
        _ => Some(Command::Batch { commands: flat }),
    }
}

/// `cols` x `rows` slices that tile the screen, each reading the matching part of the
/// composition. One `Batch`, so it is a single undo step.
pub(crate) fn grid_of_slices(scr: &Screen, cols: u32, rows: u32) -> Option<Command> {
    let (cols, rows) = (cols.max(1), rows.max(1));
    let (w, h) = (scr.width as f32, scr.height as f32);
    let mut cmds = vec![];
    for r in 0..rows {
        for c in 0..cols {
            let (x0, x1) = (c as f32 / cols as f32, (c + 1) as f32 / cols as f32);
            let (y0, y1) = (r as f32 / rows as f32, (r + 1) as f32 / rows as f32);
            let mut sl = mapping::new_slice(scr.width, scr.height);
            sl.input = vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
            sl.output_mesh = vec![
                (x0 * w, y0 * h),
                (x1 * w, y0 * h),
                (x0 * w, y1 * h),
                (x1 * w, y1 * h),
            ];
            cmds.push(Command::AddSlice {
                screen: scr.id,
                slice: sl,
                index: scr.slices.len() + cmds.len(),
            });
        }
    }
    one_command(cmds)
}

/// A tree row of `width`: the label left-aligned on a fill when selected or hovered.
fn tree_row(ui: &mut egui::Ui, width: f32, on: bool, label: &str) -> egui::Response {
    let h = TREE_SCREEN_H;
    let (rect, r) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::click());
    r.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label.to_owned())
    });
    if on {
        ui.painter().rect_filled(rect, RADIUS_SM, ACCENT);
    } else if r.hovered() {
        ui.painter().rect_filled(rect, RADIUS_SM, SURFACE_RAISED);
    }
    ui.painter().text(
        rect.left_center() + egui::vec2(SP_3, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(FS_SM),
        if on { ON_ACCENT } else { TEXT },
    );
    r
}

/// Room kept right of a screen name for its slice count.
const COUNT_W: f32 = 56.0;

/// How a rename ended.
enum Renamed {
    Commit(String),
    Cancel,
}

impl State {
    /// Side of the slices the open tab edits.
    pub(crate) fn out_side(&self) -> Side {
        match self.out_win.as_ref().map(|w| w.tab) {
            Some(OutTab::Input) => Side::Input,
            _ => Side::Output,
        }
    }

    /// The tree: header with the "+" menu, then every screen with its slices.
    pub(crate) fn out_tree(
        &mut self,
        ui: &mut egui::Ui,
        screens: &[Screen],
        cmds: &mut Vec<Command>,
    ) {
        ui.horizontal(|ui| {
            hint(
                ui.label(
                    RichText::new("Screens")
                        .font(bold_font(ui.ctx(), FS_SM))
                        .color(TEXT),
                ),
                "tree",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.add_menu(ui, screens, cmds);
            });
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("out-tree-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if screens.is_empty() {
                    widgets::help(ui, "No screens yet: use + to add one.");
                }
                for scr in screens {
                    self.tree_screen(ui, scr, screens, cmds);
                }
            });
    }

    /// The "+" menu: add a screen, a slice (plain, polygon or from a preset), a mask, an output
    /// mask, or a grid of slices.
    fn add_menu(&mut self, ui: &mut egui::Ui, screens: &[Screen], cmds: &mut Vec<Command>) {
        let (cw, ch) = (
            self.project.composition.width.max(1),
            self.project.composition.height.max(1),
        );
        let cur = self.out_selected(screens);
        let scr = cur.map(|(s, _)| s);
        let sel = cur.and_then(|(s, sl)| sl.map(|sl| (s, sl)));
        let mut grid = self.out_win.as_ref().map_or((3, 2), |w| w.add_grid);
        let r = ui.menu_button(icons::glyph(icons::i::PLUS), |ui| {
            if ui.button("Add screen").clicked() {
                let s = mapping::new_screen(&next_screen_name(screens), cw, ch);
                cmds.push(Command::AddScreen {
                    screen: s,
                    index: screens.len(),
                });
                ui.close();
            }
            if ui
                .button("Add Art-Net output (DMX universe, fixtures)")
                .clicked()
            {
                let s = crate::output_extras::artnet_screen(&next_screen_name(screens), cw, ch);
                self.sel_screen = Some(s.id);
                cmds.push(Command::AddScreen {
                    screen: s,
                    index: screens.len(),
                });
                ui.close();
            }
            if ui
                .add_enabled(scr.is_some(), egui::Button::new("Add slice"))
                .clicked()
                && let Some(s) = scr
            {
                cmds.push(Command::AddSlice {
                    screen: s.id,
                    slice: mapping::new_slice(s.width, s.height),
                    index: s.slices.len(),
                });
                ui.close();
            }
            ui.add_enabled_ui(scr.is_some(), |ui| {
                ui.menu_button("Add polygon slice", |ui| {
                    if ui.button("Draw points").clicked() {
                        self.start_draw(DrawFor::Slice, false);
                        ui.close();
                    }
                    ui.separator();
                    for preset in vjc_model::ShapePreset::ALL {
                        if ui.button(slice_adv::preset_name(preset)).clicked()
                            && let Some(s) = scr
                        {
                            let slice = slice_adv::preset_slice(s.width, s.height, preset);
                            self.sel_slice = Some(slice.id);
                            self.sel_slices.clear();
                            cmds.push(Command::AddSlice {
                                screen: s.id,
                                slice,
                                index: s.slices.len(),
                            });
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Add output mask", |ui| {
                    for (label, pen) in [("Draw points", false), ("Draw with pen", true)] {
                        if ui.button(label).clicked() {
                            self.start_draw(DrawFor::OutputMask, pen);
                            ui.close();
                        }
                    }
                });
            });
            let can_mask = sel.is_some_and(|(_, sl)| sl.mask.is_none());
            if ui
                .add_enabled(can_mask, egui::Button::new("Add mask"))
                .clicked()
                && let Some((s, sl)) = sel
            {
                self.edit_mask = true;
                self.sel_mask_pt = None;
                cmds.push(Command::SetSliceMask {
                    screen: s.id,
                    slice: sl.id,
                    mask: Some(inset_mask()),
                });
                ui.close();
            }
            ui.add_enabled_ui(scr.is_some(), |ui| {
                ui.menu_button("Add grid of slices", |ui| {
                    for (c, r) in [(2, 2), (3, 3)] {
                        if ui.button(format!("{c} x {r}")).clicked()
                            && let Some(cmd) = scr.and_then(|s| grid_of_slices(s, c, r))
                        {
                            cmds.push(cmd);
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        widgets::field_int(ui, &mut grid.0, 1..=16, "Cols ", "");
                        widgets::field_int(ui, &mut grid.1, 1..=16, "Rows ", "");
                    });
                    if widgets::btn(ui, "Add grid", widgets::Kind::Secondary).clicked()
                        && let Some(cmd) = scr.and_then(|s| grid_of_slices(s, grid.0, grid.1))
                    {
                        cmds.push(cmd);
                        ui.close();
                    }
                });
            });
        });
        if let Some(w) = &mut self.out_win {
            w.add_grid = grid;
        }
        hint(icons::name(r.response, "Add", None), "tree-add");
    }

    fn tree_screen(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        screens: &[Screen],
        cmds: &mut Vec<Command>,
    ) {
        let folded = self
            .out_win
            .as_ref()
            .is_some_and(|w| w.folded.contains(&scr.id));
        let on = self.sel_screen == Some(scr.id) && self.sel_slice.is_none();
        let renaming = self
            .out_win
            .as_ref()
            .and_then(|w| w.rename.as_ref())
            .is_some_and(|(id, _)| *id == scr.id);
        let row = ui.horizontal(|ui| {
            let caret = if folded {
                icons::i::CARET_RIGHT
            } else {
                icons::i::CARET_DOWN
            };
            let c = ui.add(
                egui::Label::new(icons::glyph(caret).color(TEXT_MUTED)).sense(egui::Sense::click()),
            );
            icons::name(c.clone(), if folded { "Expand" } else { "Collapse" }, None);
            if c.clicked()
                && let Some(w) = &mut self.out_win
                && !w.folded.remove(&scr.id)
            {
                w.folded.insert(scr.id);
            }
            let mut en = scr.enabled;
            let t = widgets::toggle(ui, &mut en, "");
            t.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    true,
                    en,
                    format!("Enable {}", scr.name),
                )
            });
            if hint(t, "tree-enable").changed() {
                cmds.push(Command::SetScreenEnabled {
                    screen: scr.id,
                    enabled: en,
                });
            }
            if renaming {
                match self.rename_edit(ui) {
                    Some(Renamed::Commit(name)) => cmds.push(Command::SetScreenName {
                        screen: scr.id,
                        name,
                    }),
                    Some(Renamed::Cancel) | None => {}
                }
                return None;
            }
            let count = slice_count_label(scr.slices.len());
            let w = (ui.available_width() - COUNT_W).max(20.0);
            let r = hint(tree_row(ui, w, on, &scr.name), "tree-screen");
            ui.label(RichText::new(count).size(FS_XS).color(TEXT_MUTED));
            Some(r)
        });
        if let Some(r) = row.inner {
            if r.clicked() {
                self.sel_screen = Some(scr.id);
                self.sel_slice = None;
                self.sel_slices.clear();
            }
            if r.double_clicked()
                && let Some(w) = &mut self.out_win
            {
                w.rename = Some((scr.id, scr.name.clone()));
            }
            if r.secondary_clicked() {
                self.sel_screen = Some(scr.id);
                self.sel_slice = None;
                self.sel_slices.clear();
            }
            r.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    if let Some(w) = &mut self.out_win {
                        w.rename = Some((scr.id, scr.name.clone()));
                    }
                    ui.close();
                }
                if ui.button("Add slice").clicked() {
                    cmds.push(Command::AddSlice {
                        screen: scr.id,
                        slice: mapping::new_slice(scr.width, scr.height),
                        index: scr.slices.len(),
                    });
                    ui.close();
                }
                if ui.button("Delete screen").clicked() {
                    cmds.push(Command::RemoveScreen { screen: scr.id });
                    ui.close();
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add_space(TREE_INDENT + SP_3);
            ui.label(
                RichText::new(format!(
                    "{} \u{b7} {}x{}",
                    self.device_label(scr),
                    scr.width,
                    scr.height
                ))
                .size(FS_XS)
                .color(TEXT_MUTED),
            );
        });
        if !folded {
            let key = egui::Id::new("out-tree-slice-h");
            let known: Option<f32> = ui.data(|d| d.get_temp(key));
            for (i, sl) in scr.slices.iter().enumerate() {
                let y = ui.cursor().top();
                // A row wholly outside the scroll viewport is only skipped over, not built.
                let clip = ui.clip_rect();
                let busy = self.sel_slice == Some(sl.id)
                    || self.sel_slices.contains(&sl.id)
                    || self
                        .out_win
                        .as_ref()
                        .and_then(|w| w.rename.as_ref())
                        .is_some_and(|(id, _)| *id == sl.id);
                if let Some(h) = known
                    && !busy
                    && (y > clip.bottom() || y + h < clip.top())
                {
                    ui.add_space(h);
                    continue;
                }
                self.tree_slice(ui, scr, screens, (i, sl), cmds);
                let h = ui.cursor().top() - y;
                if known != Some(h) {
                    ui.data_mut(|d| d.insert_temp(key, h));
                }
            }
        }
        ui.add_space(SP_2);
    }

    fn tree_slice(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        screens: &[Screen],
        (i, sl): (usize, &Slice),
        cmds: &mut Vec<Command>,
    ) {
        let on = self.sel_screen == Some(scr.id)
            && (self.sel_slice == Some(sl.id) || self.sel_slices.contains(&sl.id));
        let label = slice_label(i, sl);
        let renaming = self
            .out_win
            .as_ref()
            .and_then(|w| w.rename.as_ref())
            .is_some_and(|(id, _)| *id == sl.id);
        let mut delete = false;
        let row = ui.horizontal(|ui| {
            ui.add_space(TREE_INDENT);
            if renaming {
                match self.rename_edit(ui) {
                    Some(Renamed::Commit(name)) => cmds.push(Command::SetSliceName {
                        screen: scr.id,
                        slice: sl.id,
                        name,
                    }),
                    Some(Renamed::Cancel) | None => {}
                }
                return None;
            }
            let hovered = ui.rect_contains_pointer(ui.max_rect());
            let x_w = ui.spacing().interact_size.y + 2.0 * ui.spacing().item_spacing.x;
            let w = (ui.available_width() - x_w).max(20.0);
            let r = tree_row(ui, w, on, &label);
            if on || hovered {
                delete = hint(
                    icons::button(ui, icons::i::X, "Delete slice"),
                    "tree-delete",
                )
                .clicked();
            }
            Some(hint(r, "tree-slice"))
        });
        let Some(r) = row.inner else { return };
        let multi = ui.input(|inp| inp.modifiers.shift || inp.modifiers.command);
        if r.double_clicked()
            && let Some(w) = &mut self.out_win
        {
            w.rename = Some((sl.id, label.clone()));
        } else if r.clicked() {
            self.click_slice(scr, sl.id, multi);
            self.sel_pt_in = None;
            self.sel_pt_out = None;
        }
        if r.secondary_clicked() && !on {
            self.click_slice(scr, sl.id, false);
        }
        // The actions apply to the whole selection when the row is part of it.
        let ids: Vec<Uuid> = if on {
            scr.slices
                .iter()
                .filter(|s| {
                    s.id == sl.id || Some(s.id) == self.sel_slice || self.sel_slices.contains(&s.id)
                })
                .map(|s| s.id)
                .collect()
        } else {
            vec![sl.id]
        };
        if delete {
            cmds.extend(self.remove_cmd(scr, &ids));
        }
        r.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                if let Some(w) = &mut self.out_win {
                    w.rename = Some((sl.id, label.clone()));
                }
                ui.close();
            }
            if ui.button("Duplicate").clicked() {
                cmds.extend(self.duplicate_cmd(scr, &ids));
                ui.close();
            }
            if ui.button("Mirror").clicked() {
                cmds.extend(self.mirror_cmd(scr, &ids));
                ui.close();
            }
            ui.separator();
            for (label, axis, flip) in [
                ("Centre X", Axis::X, false),
                ("Centre Y", Axis::Y, false),
                ("Mirror X", Axis::X, true),
                ("Mirror Y", Axis::Y, true),
            ] {
                let cmd = if flip {
                    self.flip_cmd(scr, &ids, axis)
                } else {
                    self.centre_cmd(scr, &ids, axis)
                };
                if ui
                    .add_enabled(cmd.is_some(), egui::Button::new(label))
                    .clicked()
                {
                    cmds.extend(cmd);
                    ui.close();
                }
            }
            ui.separator();
            for (label, fwd) in [("Bring forward", true), ("Send backward", false)] {
                let cmd = self.order_cmd(scr, sl.id, fwd);
                if ui
                    .add_enabled(cmd.is_some(), egui::Button::new(label))
                    .clicked()
                {
                    cmds.extend(cmd);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Copy").clicked() {
                self.copy_slices(scr, &ids);
                ui.close();
            }
            if ui.button("Cut").clicked() {
                self.copy_slices(scr, &ids);
                cmds.extend(self.remove_cmd(scr, &ids));
                ui.close();
            }
            if ui
                .add_enabled(!self.slice_clip.is_empty(), egui::Button::new("Paste"))
                .clicked()
            {
                cmds.extend(self.paste_cmd(scr));
                ui.close();
            }
            ui.separator();
            let others: Vec<&Screen> = screens.iter().filter(|s| s.id != scr.id).collect();
            ui.add_enabled_ui(!others.is_empty(), |ui| {
                ui.menu_button("Move to screen", |ui| {
                    for o in &others {
                        if ui.button(&o.name).clicked() {
                            cmds.extend(self.move_cmd(scr, &ids, o));
                            ui.close();
                        }
                    }
                });
            });
            ui.separator();
            if ui.button("Delete").clicked() {
                cmds.extend(self.remove_cmd(scr, &ids));
                ui.close();
            }
        });
    }

    /// Text field for the row being renamed. Enter or clicking away commits (never an empty
    /// name for a screen; an empty slice name goes back to "Slice N"); Escape cancels.
    fn rename_edit(&mut self, ui: &mut egui::Ui) -> Option<Renamed> {
        let w = self.out_win.as_mut()?;
        let (_, buf) = w.rename.as_mut()?;
        let r = ui.add(
            egui::TextEdit::singleline(buf)
                .desired_width(ui.available_width())
                .id_salt("out-rename"),
        );
        if !r.has_focus() && !r.lost_focus() {
            r.request_focus();
            return None;
        }
        if !r.lost_focus() {
            return None;
        }
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let text = buf.trim().to_owned();
        w.rename = None;
        Some(if esc {
            Renamed::Cancel
        } else {
            Renamed::Commit(text)
        })
    }

    /// Delete the slices `ids` of `scr`.
    pub(crate) fn remove_cmd(&self, scr: &Screen, ids: &[Uuid]) -> Option<Command> {
        one_command(
            ids.iter()
                .map(|slice| Command::RemoveSlice {
                    screen: scr.id,
                    slice: *slice,
                })
                .collect(),
        )
    }

    /// Copies of the slices `ids`, each right after its original.
    pub(crate) fn duplicate_cmd(&self, scr: &Screen, ids: &[Uuid]) -> Option<Command> {
        let mut cmds = vec![];
        for (n, s) in scr
            .slices
            .iter()
            .enumerate()
            .filter(|(_, s)| ids.contains(&s.id))
        {
            let mut copy = s.clone();
            copy.id = Uuid::new_v4();
            // Each copy lands right after its original; earlier copies shift it by one.
            cmds.push(Command::AddSlice {
                screen: scr.id,
                slice: copy,
                index: n + 1 + cmds.len(),
            });
        }
        one_command(cmds)
    }

    /// Mirrored copies (flipped left to right) of the slices `ids`.
    pub(crate) fn mirror_cmd(&self, scr: &Screen, ids: &[Uuid]) -> Option<Command> {
        let c = &self.project.composition;
        let side = self.out_side();
        one_command(
            scr.slices
                .iter()
                .filter(|s| ids.contains(&s.id))
                .map(|s| slice_geom::mirror(scr, s, side, (c.width, c.height), Uuid::new_v4()))
                .collect(),
        )
    }

    /// Moves the slices `ids` to the end of screen `to`.
    pub(crate) fn move_cmd(&self, scr: &Screen, ids: &[Uuid], to: &Screen) -> Option<Command> {
        one_command(
            ids.iter()
                .enumerate()
                .map(|(n, slice)| Command::MoveSlice {
                    from: scr.id,
                    to: to.id,
                    slice: *slice,
                    index: to.slices.len() + n,
                })
                .collect(),
        )
    }

    /// The panel under the tree: the line for the control the pointer is over.
    pub(crate) fn out_help_panel(&mut self, ui: &mut egui::Ui) {
        let size = ui.max_rect().size();
        egui::Frame::new()
            .fill(BG)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(RADIUS_SM)
            .inner_margin(egui::Margin::same(SP_3 as i8))
            .show(ui, |ui| {
                ui.set_min_size(size - egui::Vec2::splat(2.0 * SP_3));
                ui.set_max_size(size - egui::Vec2::splat(2.0 * SP_3));
                ui.label(
                    RichText::new(output_hints::title(ui.ctx()))
                        .font(bold_font(ui.ctx(), FS_SM))
                        .color(TEXT),
                );
                ui.label(
                    RichText::new(output_hints::shown(ui.ctx()))
                        .size(FS_SM)
                        .color(TEXT_MUTED),
                );
            });
    }
}
