//! Right column of the Output setup window: the properties of what is selected. A screen
//! (device, size, delay, colour, guide image, overlap), one slice (name, source, position,
//! size, rotation, colour, key, soft edge, mask, mesh) or several slices at once (the shared
//! fields; a change is applied to every selected slice as one undo step).

use crate::State;
use crate::icons;
use crate::output_hints::hint;
use crate::output_poly::DrawFor;
use crate::output_tree::{inset_mask, one_command, slice_label};
use crate::params::{param_slider_step, text_field, text_field_free};
use crate::theme::{self, COMBO_W_SM, FS_SM, PROP_LABEL_W, SP_2, SP_4, TEXT, bold_font};
use crate::widgets::{self, Kind, toggle};
use eframe::egui::{self, RichText};
use uuid::Uuid;
use vjc_core::slice_adv;
use vjc_core::slice_geom::{self, Field, Op, Side};
use vjc_core::{Command, mapping};
use vjc_model::{OutputDevice, Screen, Slice, SliceColor, SliceSource, SoftEdge};

/// Turns the soft edge of the selected slices on or off. Turning on keeps the widths a
/// slice already has; only a slice without an edge gets the default one.
fn soft_edge_toggle(scr: &Screen, ids: &[Uuid], on: bool) -> Vec<Command> {
    ids.iter()
        .map(|slice| {
            let own = scr
                .slices
                .iter()
                .find(|s| s.id == *slice)
                .and_then(|s| s.soft_edge.clone());
            Command::SetSliceSoftEdge {
                screen: scr.id,
                slice: *slice,
                soft_edge: on.then(|| {
                    own.unwrap_or(SoftEdge {
                        left: SOFT_EDGE_DEFAULT,
                        right: SOFT_EDGE_DEFAULT,
                        top: 0.0,
                        bottom: 0.0,
                        gamma: 1.0,
                    })
                }),
            }
        })
        .collect()
}

/// A new soft edge starts 10% wide on the sides.
const SOFT_EDGE_DEFAULT: f32 = 0.1;

/// Numeric geometry rows of a slice.
#[derive(Clone, Copy)]
enum Geo {
    CentreX,
    CentreY,
    Left,
    Top,
    Width,
    Height,
    Rotation,
}

/// A numeric geometry row: field, label, hint key, current value, range.
type Row<'a> = (Geo, &'a str, &'static str, f32, (f32, f32));

/// Runs `body` in a scope whose hover registers the hint `key`.
fn row<R>(ui: &mut egui::Ui, key: &'static str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = ui.scope(body);
    hint(r.response, key);
    r.inner
}

/// A numeric row: value, - and + buttons, mini slider. Returns the new value.
fn num(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: f32,
    (default, range, step): (f32, (f32, f32), f32),
) -> Option<f32> {
    let tint = match label {
        "R" => Some(theme::CHANNEL_R),
        "G" => Some(theme::CHANNEL_G),
        "B" => Some(theme::CHANNEL_B),
        _ => None,
    };
    widgets::prop_row_tint(ui, None, tint, |ui| {
        param_slider_step(
            ui,
            id,
            label,
            f64::from(value),
            f64::from(default),
            ((f64::from(range.0), f64::from(range.1)), f64::from(step)),
        )
    })
    .map(|v| v as f32)
}

impl State {
    /// Properties panel for the current selection.
    pub(crate) fn out_props(
        &mut self,
        ui: &mut egui::Ui,
        screens: &[Screen],
        cmds: &mut Vec<Command>,
    ) {
        // Align the label column of every row to the kit width while this panel draws.
        let key = egui::Id::new("param-label-w");
        let prev: Option<f32> = ui.data(|d| d.get_temp(key));
        ui.data_mut(|d| d.insert_temp(key, prev.unwrap_or(0.0).max(PROP_LABEL_W)));
        widgets::prop_reset(ui);
        self.out_props_body(ui, screens, cmds);
        ui.data_mut(|d| match prev {
            Some(v) => {
                d.insert_temp(key, v);
            }
            None => {
                d.remove_temp::<f32>(key);
            }
        });
    }

    fn out_props_body(&mut self, ui: &mut egui::Ui, screens: &[Screen], cmds: &mut Vec<Command>) {
        egui::ScrollArea::vertical()
            .id_salt("out-props-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let Some((scr, prim)) = self.out_selected(screens) else {
                    widgets::help(ui, "No screens yet: use + in the tree to add one.");
                    return;
                };
                match prim {
                    Some(sl) if self.sel_slices.len() > 1 => {
                        self.props_multi(ui, scr, sl, cmds);
                    }
                    Some(sl) => self.props_slice(ui, scr, sl, cmds),
                    None => self.props_screen(ui, scr, cmds),
                }
            });
    }

    // ---- screen ------------------------------------------------------------------------

    fn props_screen(&mut self, ui: &mut egui::Ui, scr: &Screen, cmds: &mut Vec<Command>) {
        widgets::prop_header(ui, "Screen");
        row(ui, "screen-name", |ui| {
            if let Some(name) = widgets::prop_row(ui, None, |ui| {
                text_field(ui, ("screen-name", scr.id), "Name", &scr.name)
            }) {
                cmds.push(Command::SetScreenName {
                    screen: scr.id,
                    name,
                });
            }
        });
        row(ui, "screen-device", |ui| self.device_combo(ui, scr, cmds));
        row(ui, "screen-size", |ui| {
            widgets::prop_row(ui, Some("Resolution"), |ui| {
                let (mut w, mut h) = (scr.width, scr.height);
                let a = widgets::size_field(ui, &mut w, self.tex_cap, "W ");
                let b = widgets::size_field(ui, &mut h, self.tex_cap, "H ");
                if a.changed() || b.changed() {
                    cmds.push(Command::SetScreenSize {
                        screen: scr.id,
                        width: w,
                        height: h,
                    });
                }
            });
        });
        row(ui, "screen-pixel", |ui| {
            let mut pp = scr.pixel_perfect;
            if widgets::prop_row(ui, None, |ui| {
                toggle(ui, &mut pp, "Pixel-perfect (nearest)").changed()
            }) {
                cmds.push(Command::SetScreenPixelPerfect {
                    screen: scr.id,
                    pixel_perfect: pp,
                });
            }
        });
        ui.add_space(SP_4);
        row(ui, "screen-delay", |ui| {
            let max = mapping::MAX_DELAY_MS as f32;
            if let Some(v) = num(
                ui,
                (scr.id, "delay"),
                "Delay (ms)",
                scr.delay_ms as f32,
                (0.0, (0.0, max), 1.0),
            ) {
                cmds.push(Command::SetScreenDelay {
                    screen: scr.id,
                    delay_ms: v.round() as u32,
                });
            }
        });
        let mut n = scr.color;
        let g = (1.0, (0.0, mapping::MAX_GAIN), 0.01);
        row(ui, "screen-opacity", |ui| {
            if let Some(v) = num(
                ui,
                (scr.id, "opacity"),
                "Opacity",
                n.opacity,
                (1.0, (0.0, 1.0), 0.01),
            ) {
                n.opacity = v;
            }
        });
        row(ui, "screen-colour", |ui| {
            for (key, label, field) in [
                ("brightness", "Brightness", &mut n.color.brightness),
                ("contrast", "Contrast", &mut n.color.contrast),
                ("r", "R", &mut n.color.r),
                ("g", "G", &mut n.color.g),
                ("b", "B", &mut n.color.b),
            ] {
                if let Some(v) = num(ui, (scr.id, key), label, *field, g) {
                    *field = v;
                }
            }
            if widgets::btn(ui, "Reset", Kind::Secondary).clicked() {
                n = Default::default();
            }
        });
        if n != scr.color {
            cmds.push(Command::SetScreenColor {
                screen: scr.id,
                color: n,
            });
        }
        ui.add_space(SP_4);
        row(ui, "screen-guide", |ui| {
            widgets::section(ui, "Guide image", |ui| {
                let has = Self::guide_texture(ui.ctx(), scr.id).is_some();
                ui.horizontal(|ui| {
                    if widgets::btn(ui, "Load...", Kind::Secondary).clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("Image", &["png", "jpg", "jpeg"])
                            .pick_file()
                    {
                        let ctx = ui.ctx().clone();
                        self.load_guide(&ctx, scr.id, &path);
                    }
                    if widgets::btn_if(ui, has, "Remove guide", Kind::Secondary).clicked() {
                        let ctx = ui.ctx().clone();
                        self.clear_guide(&ctx, scr.id);
                    }
                });
                widgets::help(
                    ui,
                    if has {
                        "Shown under the slices on the Output tab, at half strength."
                    } else {
                        "Press Load or drop an image file on this window."
                    },
                );
            });
        });
        row(ui, "screen-overlap", |ui| {
            widgets::section(ui, "Overlap", |ui| {
                let mut on = scr.additive_overlap;
                if toggle(ui, &mut on, "Add overlapping slices (edge blend)").changed() {
                    cmds.push(Command::SetScreenAdditive { screen: scr.id, on });
                }
                widgets::help(
                    ui,
                    "Slices add up where they overlap; outside them alpha is 0.",
                );
            });
        });
        self.output_mask_rows(ui, scr, cmds);
        self.artnet_setup(ui, scr, cmds);
    }

    /// Output mask of the screen: draw it, edit its points, clear it.
    fn output_mask_rows(&mut self, ui: &mut egui::Ui, scr: &Screen, cmds: &mut Vec<Command>) {
        row(ui, "screen-mask", |ui| {
            widgets::section(ui, "Output mask", |ui| {
                ui.horizontal_wrapped(|ui| {
                    if widgets::btn(ui, "Draw points", Kind::Secondary).clicked() {
                        self.start_draw(DrawFor::OutputMask, false);
                    }
                    if widgets::btn(ui, "Draw with pen", Kind::Secondary).clicked() {
                        self.start_draw(DrawFor::OutputMask, true);
                    }
                });
                if let Some(m) = &scr.output_mask {
                    toggle(ui, &mut self.poly.edit_omask, "Edit output mask points");
                    let n = m.points().len();
                    let can_rm = n > 3 && self.poly.sel.is_some_and(|i| i < n);
                    if widgets::btn_if(ui, can_rm, "Remove output mask point", Kind::Secondary)
                        .clicked()
                        && let Some(i) = self.poly.sel.take()
                    {
                        let mut p = m.points().to_vec();
                        p.remove(i);
                        cmds.push(Command::SetScreenMask {
                            screen: scr.id,
                            mask: Some(match m {
                                vjc_model::Mask::Path(_) => vjc_model::Mask::Path(p),
                                vjc_model::Mask::Polygon(_) => vjc_model::Mask::Polygon(p),
                            }),
                        });
                    }
                    if widgets::btn(ui, "Clear output mask", Kind::Secondary).clicked() {
                        self.poly.edit_omask = false;
                        self.poly.sel = None;
                        cmds.push(Command::SetScreenMask {
                            screen: scr.id,
                            mask: None,
                        });
                    }
                } else {
                    self.poly.edit_omask = false;
                }
            });
        });
    }

    /// Where the screen is sent: nothing (a window), Syphon, NDI, Art-Net or a monitor.
    fn device_combo(&mut self, ui: &mut egui::Ui, c: &Screen, cmds: &mut Vec<Command>) {
        let label = match &c.device {
            OutputDevice::Window => "Window (none)".to_string(),
            _ => self.device_label(c),
        };
        widgets::prop_row(ui, Some("Device"), |ui| {
            let combo = egui::ComboBox::from_id_salt(("screen-device", c.id))
            .selected_text(label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(c.device == OutputDevice::Window, "None")
                    .clicked()
                {
                    cmds.push(Command::SetScreenDevice {
                        screen: c.id,
                        device: OutputDevice::Window,
                    });
                }
                for (dev, label) in [(OutputDevice::Syphon, "Syphon"), (OutputDevice::Ndi, "NDI")] {
                    if ui.selectable_label(c.device == dev, label).clicked() {
                        cmds.push(Command::SetScreenDevice {
                            screen: c.id,
                            device: dev,
                        });
                    }
                }
                let is_art = matches!(c.device, OutputDevice::ArtNet { .. });
                if ui.selectable_label(is_art, "Art-Net").clicked() && !is_art {
                    cmds.push(Command::SetScreenDevice {
                        screen: c.id,
                        device: OutputDevice::ArtNet {
                            ip: "127.0.0.1".into(),
                            universe: 0,
                            fixtures: vec![Default::default()],
                            fps: 0.0,
                            delay_ms: 0,
                        },
                    });
                }
                for m in &self.monitors {
                    let on = matches!(&c.device, OutputDevice::Monitor { name } if *name == m.id.to_string());
                    if ui.selectable_label(on, m.label()).clicked() && !on {
                        cmds.push(Command::SetScreenDevice {
                            screen: c.id,
                            device: OutputDevice::Monitor {
                                name: m.id.to_string(),
                            },
                        });
                    }
                }
            });
            crate::icons::name(combo.response, "Device list", None);
        });
    }

    // ---- slices ------------------------------------------------------------------------

    /// Ids of the selected slices of `scr` (the primary one is always included).
    fn selected_ids(&self, scr: &Screen, sl: &Slice) -> Vec<Uuid> {
        self.selected_slices(scr, sl)
    }

    fn props_slice(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        cmds: &mut Vec<Command>,
    ) {
        let n = scr.slices.iter().position(|s| s.id == sl.id).unwrap_or(0);
        widgets::prop_header(ui, "Slice");
        row(ui, "slice-name", |ui| {
            if let Some(name) = widgets::prop_row(ui, None, |ui| {
                text_field_free(ui, ("slice-name", sl.id), "Name", &slice_label(n, sl))
            }) {
                cmds.push(Command::SetSliceName {
                    screen: scr.id,
                    slice: sl.id,
                    name,
                });
            }
        });
        let ids = [sl.id];
        row(ui, "slice-source", |ui| {
            self.source_picker(ui, scr, &ids, sl, cmds)
        });
        self.geometry_rows(ui, scr, sl, &ids, cmds);
        self.colour_rows(ui, scr, sl, &ids, cmds);
        self.key_rows(ui, scr, sl, &ids, cmds);
        self.edge_rows(ui, scr, sl, &ids, true, cmds);
        self.outline_rows(ui, scr, sl, cmds);
        self.mask_rows(ui, scr, sl, &ids, cmds);
        self.mesh_rows(ui, scr, sl, cmds);
        self.action_rows(ui, scr, sl, &ids, cmds);
    }

    fn props_multi(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        cmds: &mut Vec<Command>,
    ) {
        let ids = self.selected_ids(scr, sl);
        row(ui, "multi", |ui| {
            ui.label(
                RichText::new("Multiple slices")
                    .font(bold_font(ui.ctx(), FS_SM))
                    .color(TEXT),
            );
            widgets::help(
                ui,
                &format!("{} selected: a change applies to all of them.", ids.len()),
            );
        });
        ui.separator();
        row(ui, "slice-source", |ui| {
            self.source_picker(ui, scr, &ids, sl, cmds)
        });
        self.geometry_rows(ui, scr, sl, &ids, cmds);
        self.colour_rows(ui, scr, sl, &ids, cmds);
        self.key_rows(ui, scr, sl, &ids, cmds);
        self.edge_rows(ui, scr, sl, &ids, false, cmds);
        self.mask_rows(ui, scr, sl, &ids, cmds);
        self.action_rows(ui, scr, sl, &ids, cmds);
    }

    /// Dropdown of what the slices show: composition, preview, one layer or one group.
    fn source_picker(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        ids: &[Uuid],
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
        widgets::prop_row(ui, Some("Source"), |ui| {
            let combo = egui::ComboBox::from_id_salt(("slice-source", sl.id))
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (label, source) in choices {
                        if widgets::choice(ui, sl.source == source, label).clicked() {
                            let all = ids
                                .iter()
                                .filter(|id| {
                                    scr.slices
                                        .iter()
                                        .any(|s| s.id == **id && s.source != source)
                                })
                                .map(|slice| Command::SetSliceSource {
                                    screen: scr.id,
                                    slice: *slice,
                                    source,
                                })
                                .collect();
                            cmds.extend(one_command(all));
                        }
                    }
                });
            crate::icons::name(combo.response, "Source list", None);
        });
    }

    /// X, Y (centre), left, top, width, height and rotation, then the turn and flip buttons.
    /// Values are pixels of the tab being looked at (composition on Input, screen on Output).
    fn geometry_rows(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        ids: &[Uuid],
        cmds: &mut Vec<Command>,
    ) {
        let c = &self.project.composition;
        let comp = (c.width, c.height);
        let side = self.out_side();
        let space = match side {
            Side::Input => (comp.0.max(1) as f32, comp.1.max(1) as f32),
            Side::Output => (scr.width.max(1) as f32, scr.height.max(1) as f32),
        };
        let Some(v) = slice_geom::readout(sl, side, comp) else {
            return;
        };
        let edit = |g: Geo, value: f32| -> Option<Command> {
            let cmds: Vec<Command> = scr
                .slices
                .iter()
                .filter(|s| ids.contains(&s.id))
                .filter_map(|s| {
                    let (field, val) = match g {
                        Geo::CentreX => {
                            let w = slice_geom::readout(s, side, comp)?[2];
                            (Field::X, value - w / 2.0)
                        }
                        Geo::CentreY => {
                            let h = slice_geom::readout(s, side, comp)?[3];
                            (Field::Y, value - h / 2.0)
                        }
                        Geo::Left => (Field::X, value),
                        Geo::Top => (Field::Y, value),
                        Geo::Width => (Field::W, value),
                        Geo::Height => (Field::H, value),
                        Geo::Rotation => (Field::Rot, value),
                    };
                    slice_geom::edit(scr, s, side, comp, Op::Set(field, val))
                })
                .collect();
            one_command(cmds)
        };
        let unit = match side {
            Side::Input => "composition px",
            Side::Output => "screen px",
        };
        widgets::fold_section(ui, "slice", "Position and size", |ui| {
            widgets::help(ui, &format!("In {unit}; fields take math like /3 or +10."));
            let ex = (-space.0, 2.0 * space.0);
            let ey = (-space.1, 2.0 * space.1);
            let rows: [Row; 7] = [
                (Geo::CentreX, "X", "slice-xy", v[0] + v[2] / 2.0, ex),
                (Geo::CentreY, "Y", "slice-xy", v[1] + v[3] / 2.0, ey),
                (Geo::Left, "Left", "slice-lt", v[0], ex),
                (Geo::Top, "Top", "slice-lt", v[1], ey),
                (
                    Geo::Width,
                    "Width",
                    "slice-size",
                    v[2],
                    (1.0, 2.0 * space.0),
                ),
                (
                    Geo::Height,
                    "Height",
                    "slice-size",
                    v[3],
                    (1.0, 2.0 * space.1),
                ),
                (
                    Geo::Rotation,
                    "Rotation",
                    "slice-rot",
                    v[4],
                    (-180.0, 180.0),
                ),
            ];
            for (g, label, key, value, range) in rows {
                let step = 1.0;
                let new = row(ui, key, |ui| {
                    num(ui, (sl.id, label), label, value, (value, range, step))
                });
                if let Some(nv) = new
                    && let Some(cmd) = edit(g, nv)
                {
                    cmds.push(cmd);
                }
            }
            row(ui, "slice-flip", |ui| {
                ui.horizontal_wrapped(|ui| {
                    let mut ops = vec![];
                    for (label, icon, op) in [
                        ("Rotate 90", icons::i::ARROW_CLOCKWISE, Op::Rotate(1)),
                        ("Rotate 180", icons::i::ARROWS_CLOCKWISE, Op::Rotate(2)),
                        (
                            "Rotate 270",
                            icons::i::ARROW_COUNTER_CLOCKWISE,
                            Op::Rotate(3),
                        ),
                        ("Flip H", icons::i::FLIP_HORIZONTAL, Op::FlipH),
                        ("Flip V", icons::i::FLIP_VERTICAL, Op::FlipV),
                    ] {
                        if icons::button(ui, icon, label).clicked() {
                            ops.push(op);
                        }
                    }
                    for op in ops {
                        let all = scr
                            .slices
                            .iter()
                            .filter(|s| ids.contains(&s.id))
                            .filter_map(|s| slice_geom::edit(scr, s, side, comp, op))
                            .collect();
                        cmds.extend(one_command(all));
                    }
                    if widgets::btn(ui, "Match output shape", Kind::Secondary)
                        .on_hover_text("Copy the output rectangle to the input: 1:1 pixel map")
                        .clicked()
                    {
                        let all = scr
                            .slices
                            .iter()
                            .filter(|s| ids.contains(&s.id))
                            .filter_map(|s| {
                                slice_geom::edit(scr, s, Side::Input, comp, Op::MatchOutput)
                            })
                            .collect();
                        cmds.extend(one_command(all));
                    }
                });
            });
        });
    }

    /// Brightness, contrast and RGB gains, to match panels of different batches.
    fn colour_rows(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        ids: &[Uuid],
        cmds: &mut Vec<Command>,
    ) {
        widgets::fold_section(ui, "slice", "Colour", |ui| {
            row(ui, "slice-colour", |ui| {
                let g = (1.0, (0.0, mapping::MAX_GAIN), 0.01);
                type Pick = fn(&mut SliceColor) -> &mut f32;
                let fields: [(&str, &str, Pick); 5] = [
                    ("brightness", "Brightness", |c| &mut c.brightness),
                    ("contrast", "Contrast", |c| &mut c.contrast),
                    ("r", "R", |c| &mut c.r),
                    ("g", "G", |c| &mut c.g),
                    ("b", "B", |c| &mut c.b),
                ];
                for (key, label, pick) in fields {
                    let mut cur = sl.color;
                    let now = *pick(&mut cur);
                    if let Some(v) = num(ui, (sl.id, key), label, now, g) {
                        let all = scr
                            .slices
                            .iter()
                            .filter(|s| ids.contains(&s.id))
                            .map(|s| {
                                let mut color = s.color;
                                *pick(&mut color) = v;
                                Command::SetSliceColor {
                                    screen: scr.id,
                                    slice: s.id,
                                    color,
                                }
                            })
                            .collect();
                        cmds.extend(one_command(all));
                    }
                }
                if widgets::btn(ui, "Reset", Kind::Secondary).clicked() {
                    let all = ids
                        .iter()
                        .map(|slice| Command::SetSliceColor {
                            screen: scr.id,
                            slice: *slice,
                            color: Default::default(),
                        })
                        .collect();
                    cmds.extend(one_command(all));
                }
            });
        });
    }

    /// Is Key and Black BG toggles.
    fn key_rows(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        ids: &[Uuid],
        cmds: &mut Vec<Command>,
    ) {
        widgets::fold_section(ui, "slice", "Alpha", |ui| {
            ui.horizontal(|ui| {
                let (mut key, mut bg) = (sl.is_key, sl.black_bg);
                let a = hint(toggle(ui, &mut key, "Is Key"), "slice-key").changed();
                let b = hint(toggle(ui, &mut bg, "Black BG"), "slice-black").changed();
                if a || b {
                    let all = scr
                        .slices
                        .iter()
                        .filter(|s| ids.contains(&s.id))
                        .map(|s| Command::SetSliceKey {
                            screen: scr.id,
                            slice: s.id,
                            is_key: if a { key } else { s.is_key },
                            black_bg: if b { bg } else { s.black_bg },
                        })
                        .collect();
                    cmds.extend(one_command(all));
                }
            });
        });
    }

    /// Soft edge: the checkbox, and (for one slice) the width and gamma sliders in its fold.
    fn edge_rows(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        ids: &[Uuid],
        full: bool,
        cmds: &mut Vec<Command>,
    ) {
        let mut on = sl.soft_edge.is_some();
        let toggled = widgets::fold_section_with(
            ui,
            "slice",
            "Soft edge",
            Some(("Enabled", &mut on)),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    let Some(e) = &sl.soft_edge else { return };
                    if !full {
                        return;
                    }
                    let mut n = e.clone();
                    let w = 0.0..=mapping::MAX_SOFT_WIDTH;
                    ui.add(egui::Slider::new(&mut n.left, w.clone()).text("Edge left"));
                    ui.add(egui::Slider::new(&mut n.right, w.clone()).text("Edge right"));
                    ui.add(egui::Slider::new(&mut n.top, w.clone()).text("Edge top"));
                    widgets::slider(ui, &mut n.bottom, w, "Edge bottom");
                    let g = mapping::GAMMA_RANGE.0..=mapping::GAMMA_RANGE.1;
                    widgets::slider(ui, &mut n.gamma, g, "Edge gamma");
                    if n != *e {
                        cmds.push(Command::SetSliceSoftEdge {
                            screen: scr.id,
                            slice: sl.id,
                            soft_edge: Some(n),
                        });
                    }
                });
            },
        );
        if toggled {
            let all = soft_edge_toggle(scr, ids, on);
            cmds.extend(one_command(all));
        }
    }

    /// Mask: a row of shape buttons, plus create / edit / clear and invert.
    fn mask_rows(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        ids: &[Uuid],
        cmds: &mut Vec<Command>,
    ) {
        let single = ids.len() == 1;
        let set_mask = |slice: Uuid, m: Option<Vec<(f32, f32)>>| Command::SetSliceMask {
            screen: scr.id,
            slice,
            mask: m,
        };
        widgets::fold_section(ui, "slice", "Mask", |ui| {
            row(ui, "slice-mask", |ui| {
                ui.horizontal(|ui| {
                    for (shape, name, icon) in [
                        (mapping::MaskShape::Rectangle, "Rectangle", icons::i::SQUARE),
                        (mapping::MaskShape::Ellipse, "Ellipse", icons::i::CIRCLE),
                        (mapping::MaskShape::Triangle, "Triangle", icons::i::TRIANGLE),
                        (mapping::MaskShape::Diamond, "Diamond", icons::i::DIAMOND),
                    ] {
                        if icons::button(ui, icon, name).clicked() {
                            self.sel_mask_pt = None;
                            let all = scr
                                .slices
                                .iter()
                                .filter(|s| ids.contains(&s.id))
                                .map(|s| {
                                    set_mask(
                                        s.id,
                                        Some(mapping::mask_preset(shape, slice_bbox_norm(s, scr))),
                                    )
                                })
                                .collect();
                            cmds.extend(one_command(all));
                        }
                    }
                });
            });
            if !single {
                return;
            }
            ui.horizontal(|ui| {
                if sl.mask.is_none() {
                    self.edit_mask = false;
                    if widgets::btn(ui, "Create mask", Kind::Secondary).clicked() {
                        self.edit_mask = true;
                        self.sel_mask_pt = None;
                        cmds.push(set_mask(sl.id, Some(inset_mask())));
                    }
                } else if let Some(m) = &sl.mask {
                    toggle(ui, &mut self.edit_mask, "Edit mask points");
                    let can_rm = m.len() > 3 && self.sel_mask_pt.is_some_and(|i| i < m.len());
                    if widgets::btn_if(ui, can_rm, "Remove point", Kind::Secondary).clicked()
                        && let Some(i) = self.sel_mask_pt.take()
                    {
                        let mut m = m.clone();
                        m.remove(i);
                        cmds.push(set_mask(sl.id, Some(m)));
                    }
                    if widgets::btn(ui, "Clear mask", Kind::Secondary).clicked() {
                        self.edit_mask = false;
                        self.sel_mask_pt = None;
                        cmds.push(set_mask(sl.id, None));
                    }
                }
            });
            if widgets::btn(ui, "Draw mask with pen", Kind::Secondary).clicked() {
                self.start_draw(DrawFor::SliceMask(sl.id), true);
            }
            if self.edit_mask && sl.mask.is_some() {
                widgets::help(
                    ui,
                    "On the Output tab: click empty space to add a point, Delete removes the selected one, Shift snaps to 15 degrees.",
                );
            }
            let mut inv = sl.mask_invert;
            if toggle(ui, &mut inv, "Invert mask").changed() {
                cmds.push(Command::SetSliceMaskInvert {
                    screen: scr.id,
                    slice: sl.id,
                    invert: inv,
                });
            }
        });
    }

    /// Outline of the slice: a rectangle, a preset shape or a free polygon.
    fn outline_rows(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        cmds: &mut Vec<Command>,
    ) {
        let set = |shape: vjc_model::SliceShape| Command::SetSliceShape {
            screen: scr.id,
            slice: sl.id,
            shape,
        };
        widgets::fold_section(ui, "slice", "Outline", |ui| {
            row(ui, "slice-outline", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for preset in vjc_model::ShapePreset::ALL {
                        let name = slice_adv::preset_name(preset);
                        if widgets::btn(ui, name, Kind::Secondary).clicked() {
                            let shape = if preset == vjc_model::ShapePreset::Rect {
                                vjc_model::SliceShape::Rect
                            } else {
                                let (x, y, w, h) = slice_bbox_norm(sl, scr);
                                let r = slice_geom::Rect {
                                    x,
                                    y,
                                    w: w - x,
                                    h: h - y,
                                };
                                vjc_model::SliceShape::Polygon(slice_adv::preset_polygon(preset, r))
                            };
                            self.poly.sel = None;
                            cmds.push(set(shape));
                        }
                    }
                });
                if let vjc_model::SliceShape::Polygon(p) = &sl.shape {
                    toggle(ui, &mut self.poly.edit_outline, "Edit outline points");
                    let can_rm = p.len() > 3 && self.poly.sel.is_some_and(|i| i < p.len());
                    if widgets::btn_if(ui, can_rm, "Remove outline point", Kind::Secondary)
                        .clicked()
                        && let Some(i) = self.poly.sel.take()
                    {
                        let mut p = p.clone();
                        p.remove(i);
                        cmds.push(set(vjc_model::SliceShape::Polygon(p)));
                    }
                    if self.poly.edit_outline {
                        widgets::help(
                            ui,
                            "On the Output tab: drag a point, click an edge to add one, Delete removes it, Shift snaps to 15 degrees.",
                        );
                    }
                } else {
                    self.poly.edit_outline = false;
                }
            });
        });
    }

    /// Warp mesh size of the slice.
    fn mesh_rows(&mut self, ui: &mut egui::Ui, scr: &Screen, sl: &Slice, cmds: &mut Vec<Command>) {
        widgets::fold_section(ui, "slice", "Warp mesh", |ui| {
            row(ui, "slice-curve", |ui| {
                ui.horizontal(|ui| {
                    for (mode, label) in [
                        (vjc_model::MeshMode::Linear, "Straight"),
                        (vjc_model::MeshMode::Bezier, "Curved"),
                    ] {
                        if widgets::choice(ui, sl.mesh_mode == mode, label).clicked()
                            && sl.mesh_mode != mode
                        {
                            cmds.push(Command::SetSliceMeshMode {
                                screen: scr.id,
                                slice: sl.id,
                                mode,
                                tangents: sl.mesh_tangents.clone(),
                            });
                        }
                    }
                });
            });
            row(ui, "slice-mesh", |ui| {
                ui.horizontal(|ui| {
                    let rows = mapping::rows_of(sl.output_mesh.len(), sl.mesh_cols);
                    let mut grid = (sl.mesh_cols, rows);
                    for (n, v) in [("Cols", &mut grid.0), ("Rows", &mut grid.1)] {
                        egui::ComboBox::from_label(n)
                            .width(COMBO_W_SM)
                            .selected_text(v.to_string())
                            .show_ui(ui, |ui| {
                                for k in mapping::MIN_GRID..=mapping::MAX_GRID {
                                    ui.selectable_value(v, k, k.to_string());
                                }
                            });
                    }
                    if grid != (sl.mesh_cols, rows) {
                        self.sel_pt_out = None;
                        cmds.push(Command::SetSliceMeshSize {
                            screen: scr.id,
                            slice: sl.id,
                            cols: grid.0,
                            rows: grid.1,
                        });
                    }
                });
            });
        });
    }

    /// Duplicate, mirror, move to another screen and remove.
    fn action_rows(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &Slice,
        ids: &[Uuid],
        cmds: &mut Vec<Command>,
    ) {
        let many = ids.len() > 1;
        let _ = sl;
        row(ui, "slice-actions", |ui| {
            ui.add_space(SP_2);
            ui.horizontal_wrapped(|ui| {
                let dup = if many {
                    "Duplicate slices"
                } else {
                    "Duplicate slice"
                };
                if widgets::btn(ui, dup, Kind::Secondary).clicked() {
                    cmds.extend(self.duplicate_cmd(scr, ids));
                }
                if widgets::btn(ui, "Mirror", Kind::Secondary)
                    .on_hover_text("Duplicate and flip the copy horizontally (one undo step)")
                    .clicked()
                {
                    cmds.extend(self.mirror_cmd(scr, ids));
                }
                let rm = if many {
                    "Remove slices"
                } else {
                    "Remove slice"
                };
                if widgets::btn(ui, rm, Kind::Secondary).clicked() {
                    cmds.extend(self.remove_cmd(scr, ids));
                }
            });
            let others: Vec<Screen> = self
                .out_screens()
                .into_iter()
                .filter(|s| s.id != scr.id)
                .collect();
            ui.add_enabled_ui(!others.is_empty(), |ui| {
                egui::ComboBox::from_label("Move to screen")
                    .selected_text("Choose")
                    .show_ui(ui, |ui| {
                        for o in &others {
                            if widgets::choice(ui, false, &o.name).clicked() {
                                cmds.extend(self.move_cmd(scr, ids, o));
                            }
                        }
                    });
            });
        });
    }
}

/// The slice's output rectangle in 0..1 screen units (what mask shapes are fitted to).
fn slice_bbox_norm(sl: &Slice, scr: &Screen) -> (f32, f32, f32, f32) {
    let (w, h) = (scr.width.max(1) as f32, scr.height.max(1) as f32);
    let fold = |it: &mut dyn Iterator<Item = f32>| {
        it.fold((f32::MAX, f32::MIN), |a, v| (a.0.min(v), a.1.max(v)))
    };
    let (x0, x1) = fold(&mut sl.output_mesh.iter().map(|p| p.0 / w));
    let (y0, y1) = fold(&mut sl.output_mesh.iter().map(|p| p.1 / h));
    if x0 < x1 && y0 < y1 {
        (x0, y0, x1, y1)
    } else {
        (0.0, 0.0, 1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_edge_on_keeps_each_slices_own_widths() {
        let mut scr = vjc_core::mapping::new_screen("s", 1920, 1080);
        let mut b = vjc_core::mapping::new_slice(1920, 1080);
        b.soft_edge = Some(SoftEdge {
            left: 0.3,
            right: 0.0,
            top: 0.2,
            bottom: 0.0,
            gamma: 1.8,
        });
        let custom = b.soft_edge.clone();
        let a = vjc_core::mapping::new_slice(1920, 1080);
        let (a_id, b_id) = (a.id, b.id);
        scr.slices.extend([a, b]);
        let cmds = soft_edge_toggle(&scr, &[a_id, b_id], true);
        let edge = |i: usize| match &cmds[i] {
            Command::SetSliceSoftEdge { soft_edge, .. } => soft_edge.clone(),
            c => panic!("{c:?}"),
        };
        assert_eq!(edge(0).map(|e| e.left), Some(SOFT_EDGE_DEFAULT), "new edge");
        assert_eq!(edge(1), custom, "the custom widths stay");
        let off = soft_edge_toggle(&scr, &[a_id, b_id], false);
        assert!(off.iter().all(|c| matches!(
            c,
            Command::SetSliceSoftEdge {
                soft_edge: None,
                ..
            }
        )));
    }
}
