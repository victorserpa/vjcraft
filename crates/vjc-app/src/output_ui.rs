//! "Output setup" window (Output > Setup...): screens, slice input quads and warp meshes.

use crate::State;
use crate::artnet_out;
use crate::slice_inspector::{GUIDE_PX, arrow_move, snap_dragged};
use eframe::egui;
use vjc_core::slice_geom::{self, Side};
use vjc_core::{Command, dmx, mapping};
use vjc_model::{ChannelOrder, Fixture, OutputDevice, PixelFormat, Screen};

use crate::theme::{
    COMBO_W_SM, FIELD_W_MD, FIELD_W_NAME, HANDLE_GRAB_K, HANDLE_R, OUTPUT_CANVAS_W, OUTPUT_LIST_W,
    OUTPUT_WIN_W, OUTPUT_WIN_X, OUTPUT_WIN_Y, SNAP_PX, STROKE_CANVAS, STROKE_HANDLE,
};
use crate::widgets::{self, Kind};

/// Largest screen size, fixture grid side and Art-Net universe the fields accept.
const MAX_SCREEN_PX: u32 = 16384;
const MAX_FIXTURE_SIDE: u32 = 1024;
const MAX_UNIVERSE: i32 = 32767;
/// A new mask is an inset square; a new soft edge starts 10% wide on the sides.
const MASK_INSET: f32 = 0.1;
const SOFT_EDGE_DEFAULT: f32 = 0.1;

/// Behaviour switches of `handles`: grid `snap` (0 = off), arrow-key nudging of the
/// selected point, and the slice rectangle that a drag off every handle moves.
struct Opts {
    snap: f32,
    keys: bool,
    body: Option<slice_geom::Rect>,
}

/// What a canvas drag produced this frame.
#[derive(Default)]
struct Drag {
    /// A point moved to a new position `(index, position)`.
    moved: Option<(usize, (f32, f32))>,
    /// The slice body is being dragged: its rectangle when the drag began, and the offset
    /// since then (both in the canvas space units).
    body: Option<(slice_geom::Rect, (f32, f32))>,
}

/// Draggable handles for `pts` (in `space` units) over `rect`. Returns the moved point as
/// `(index, new position)`; a drag that starts off every handle but inside `body` drags the
/// whole slice. Arrow keys nudge the selected point 1 unit (10 with Shift) when
/// `keys` is set and the pointer is over the canvas.
fn handles(
    ui: &mut egui::Ui,
    id: egui::Id,
    (rect, space): (egui::Rect, (f32, f32)),
    pts: &[(f32, f32)],
    sel: &mut Option<usize>,
    Opts { snap, keys, body }: Opts,
) -> Drag {
    let resp = ui.interact(rect, id, egui::Sense::click_and_drag());
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
                    d.insert_temp::<Option<(slice_geom::Rect, (f32, f32))>>(
                        body_id,
                        inside.then_some((r, (0.0, 0.0))),
                    )
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
            let c = |v: f32, max: f32| mapping::snap(v, snap).clamp(0.0, max);
            moved = Some((i, (c(u.0, space.0), c(u.1, space.1))));
        }
    }
    if resp.dragged()
        && let Some((r, acc)) = ui
            .data(|d| d.get_temp::<Option<(slice_geom::Rect, (f32, f32))>>(body_id))
            .flatten()
    {
        let dd = resp.drag_delta();
        let acc = (
            acc.0 + dd.x / rect.width() * space.0,
            acc.1 + dd.y / rect.height() * space.1,
        );
        ui.data_mut(|d| {
            d.insert_temp::<Option<(slice_geom::Rect, (f32, f32))>>(body_id, Some((r, acc)))
        });
        body_drag = Some((r, acc));
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| {
            d.insert_temp::<Option<usize>>(drag_id, None);
            d.insert_temp::<Option<(slice_geom::Rect, (f32, f32))>>(body_id, None);
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
    for (i, p) in pts.iter().enumerate() {
        let on_it = *sel == Some(i);
        ui.painter().circle(
            to_px(*p),
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

fn canvas_rect(ui: &mut egui::Ui, w: f32, aspect: f32) -> egui::Rect {
    let size = egui::vec2(w, w / aspect.max(0.05));
    ui.allocate_exact_size(size, egui::Sense::hover()).0
}

impl State {
    pub(crate) fn output_setup_window(&mut self, ctx: &egui::Context) {
        if !self.output_setup_open {
            self.preset_names = None;
            return;
        }
        let mut open = true;
        let mut cmds: Vec<Command> = vec![];
        let screens = self.project.output.screens.clone();
        let comp = &self.project.composition;
        let (cw, ch) = (comp.width.max(1), comp.height.max(1));
        let sel_screen = self
            .sel_screen
            .and_then(|id| screens.iter().find(|s| s.id == id))
            .or(screens.first())
            .cloned();
        egui::Window::new("Output setup")
            .open(&mut open)
            .default_width(OUTPUT_WIN_W)
            // Open below the menu bar: the default position covered "Output" and ate its clicks.
            .default_pos(egui::pos2(OUTPUT_WIN_X, OUTPUT_WIN_Y))
            .show(ctx, |ui| {
                self.presets_row(ui);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(OUTPUT_LIST_W);
                        self.screen_list(ui, &screens, sel_screen.as_ref(), (cw, ch), &mut cmds);
                    });
                    ui.separator();
                    if let Some(scr) = &sel_screen {
                        self.slice_editor(ui, scr, &mut cmds);
                    } else {
                        ui.label("No screens: output shows the whole composition.");
                    }
                });
            });
        self.output_setup_open = open;
        self.run_output_cmds(cmds);
    }

    /// "Display info" window: name, resolution and refresh of each monitor, and which
    /// screens are bound to it.
    pub(crate) fn display_info_window(&mut self, ctx: &egui::Context) {
        if !self.display_info_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Display info")
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
            // Selection follows a slice only once the command was accepted.
            let select = match &c {
                Command::AddSlice { slice, .. } => Some((None, slice.id)),
                Command::MoveSlice { to, slice, .. } => Some((Some(*to), *slice)),
                _ => None,
            };
            let removes = match &c {
                Command::RemoveSlice { .. } => true,
                Command::Batch { commands } => commands
                    .iter()
                    .any(|c| matches!(c, Command::RemoveSlice { .. })),
                _ => false,
            };
            let ok = self.exec_ok(c);
            if ok {
                if let Some((screen, slice)) = select {
                    if let Some(screen) = screen {
                        self.sel_screen = Some(screen);
                    }
                    self.sel_slice = Some(slice);
                    self.sel_slices.clear();
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

    fn screen_list(
        &mut self,
        ui: &mut egui::Ui,
        screens: &[Screen],
        cur: Option<&Screen>,
        (cw, ch): (u32, u32),
        cmds: &mut Vec<Command>,
    ) {
        ui.strong("Screens");
        for s in screens {
            let on = cur.is_some_and(|c| c.id == s.id);
            ui.horizontal(|ui| {
                let mut en = s.enabled;
                if ui
                    .checkbox(&mut en, "")
                    .on_hover_text("On / off (off sends black)")
                    .changed()
                {
                    cmds.push(Command::SetScreenEnabled {
                        screen: s.id,
                        enabled: en,
                    });
                }
                if crate::help::help(ui.selectable_label(on, &s.name), "out-screens").clicked() {
                    self.sel_screen = Some(s.id);
                    self.sel_slice = None;
                    self.sel_slices.clear();
                }
            });
        }
        ui.horizontal(|ui| {
            if crate::help::help(widgets::btn(ui, "Add", Kind::Secondary), "out-add-screen")
                .clicked()
            {
                let s = mapping::new_screen(&format!("Screen {}", screens.len() + 1), cw, ch);
                self.sel_screen = Some(s.id);
                cmds.push(Command::AddScreen {
                    screen: s,
                    index: screens.len(),
                });
            }
            if ui
                .add_enabled_ui(cur.is_some(), |ui| {
                    widgets::btn(ui, "Remove", Kind::Secondary)
                })
                .inner
                .clicked()
                && let Some(c) = cur
            {
                self.sel_screen = None;
                cmds.push(Command::RemoveScreen { screen: c.id });
            }
        });
        let Some(c) = cur else { return };
        ui.separator();
        let label = match &c.device {
            OutputDevice::Monitor { name } => self
                .monitors
                .iter()
                .find(|m| m.id.to_string() == *name)
                .map_or("Monitor (missing)".to_string(), |m| m.label()),
            OutputDevice::Syphon => "Syphon".into(),
            OutputDevice::Ndi => "NDI".into(),
            OutputDevice::ArtNet { .. } => "Art-Net".into(),
            _ => "Window (none)".into(),
        };
        egui::ComboBox::from_label("Device")
            .selected_text(label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(
                        !matches!(
                            c.device,
                            OutputDevice::Monitor { .. }
                                | OutputDevice::Syphon
                                | OutputDevice::Ndi
                                | OutputDevice::ArtNet { .. }
                        ),
                        "None",
                    )
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
                            fixture: Default::default(),
                        },
                    });
                }
                for m in &self.monitors {
                    if ui.selectable_label(false, m.label()).clicked() {
                        cmds.push(Command::SetScreenDevice {
                            screen: c.id,
                            device: OutputDevice::Monitor {
                                name: m.id.to_string(),
                            },
                        });
                    }
                }
            });
        let (mut w, mut h) = (c.width, c.height);
        let a = ui.add(
            egui::DragValue::new(&mut w)
                .range(1..=MAX_SCREEN_PX)
                .prefix("W "),
        );
        let b = ui.add(
            egui::DragValue::new(&mut h)
                .range(1..=MAX_SCREEN_PX)
                .prefix("H "),
        );
        if a.changed() || b.changed() {
            cmds.push(Command::SetScreenSize {
                screen: c.id,
                width: w,
                height: h,
            });
        }
        let mut pp = c.pixel_perfect;
        if crate::help::help(
            ui.checkbox(&mut pp, "Pixel-perfect (nearest)"),
            "out-pixel-perfect",
        )
        .changed()
        {
            cmds.push(Command::SetScreenPixelPerfect {
                screen: c.id,
                pixel_perfect: pp,
            });
        }
        widgets::section(ui, "Colour", |ui| {
            let mut n = c.color;
            widgets::slider(ui, &mut n.opacity, 0.0..=1.0, "Opacity");
            let r = 0.0..=mapping::MAX_GAIN;
            widgets::slider(ui, &mut n.color.brightness, r.clone(), "Brightness");
            widgets::slider(ui, &mut n.color.contrast, r.clone(), "Contrast");
            widgets::slider(ui, &mut n.color.r, r.clone(), "R");
            widgets::slider(ui, &mut n.color.g, r.clone(), "G");
            widgets::slider(ui, &mut n.color.b, r, "B");
            if widgets::btn(ui, "Reset", Kind::Secondary).clicked() {
                n = Default::default();
            }
            if n != c.color {
                cmds.push(Command::SetScreenColor {
                    screen: c.id,
                    color: n,
                });
            }
        });
        self.artnet_setup(ui, c, cmds);
    }

    /// Art-Net device: target IP, first universe and the fixture (pick, edit, save to the
    /// library in the preferences).
    fn artnet_setup(&mut self, ui: &mut egui::Ui, scr: &Screen, cmds: &mut Vec<Command>) {
        let OutputDevice::ArtNet {
            ip,
            universe,
            fixture,
        } = &scr.device
        else {
            return;
        };
        ui.separator();
        ui.strong("Art-Net");
        let set = |ip: String, universe: u16, fixture: Fixture| Command::SetScreenDevice {
            screen: scr.id,
            device: OutputDevice::ArtNet {
                ip,
                universe,
                fixture,
            },
        };
        // The text is applied when the field loses focus, not on every key.
        let ip_id = egui::Id::new(("artnet_ip", scr.id));
        let mut text = ui
            .data_mut(|d| d.get_temp::<String>(ip_id))
            .unwrap_or_else(|| ip.clone());
        ui.horizontal(|ui| {
            ui.label("IP");
            let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(FIELD_W_MD));
            if r.lost_focus() && text.trim() != ip {
                cmds.push(set(text.trim().to_owned(), *universe, fixture.clone()));
            }
            if r.has_focus() {
                ui.data_mut(|d| d.insert_temp(ip_id, text.clone()));
            } else {
                ui.data_mut(|d| d.remove_temp::<String>(ip_id));
            }
        });
        ui.weak("host or host:port (default port 6454)");
        let mut u = i32::from(*universe);
        if ui
            .add(
                egui::DragValue::new(&mut u)
                    .range(0..=MAX_UNIVERSE)
                    .prefix("Universe "),
            )
            .changed()
        {
            cmds.push(set(ip.clone(), u as u16, fixture.clone()));
        }
        let mut f = fixture.clone();
        let library: Vec<Fixture> = dmx::presets()
            .into_iter()
            .chain(self.prefs.dmx.fixtures.iter().cloned())
            .collect();
        egui::ComboBox::from_label("Fixture")
            .selected_text(&f.name)
            .show_ui(ui, |ui| {
                for l in &library {
                    if ui.selectable_label(l == fixture, &l.name).clicked() {
                        f = l.clone();
                    }
                }
            });
        self.fixture_editor(ui, &mut f, egui::Id::new(("artnet_fx_name", scr.id)));
        if f != *fixture {
            cmds.push(set(ip.clone(), *universe, f));
        }
        let cpp = dmx::channels_per_pixel(fixture.format);
        let channels = u64::from(fixture.width) * u64::from(fixture.height) * cpp as u64;
        let per = dmx::universe_chunk(fixture.format) as u64;
        ui.weak(format!(
            "{} channels, {} universe(s), up to {} Hz",
            channels,
            channels.div_ceil(per.max(1)),
            artnet_out::MAX_HZ
        ));
        ui.horizontal(|ui| {
            let saved = self
                .prefs
                .dmx
                .fixtures
                .iter()
                .position(|l| l.name == fixture.name);
            if widgets::btn(ui, "Save to library", Kind::Secondary).clicked() {
                match saved {
                    Some(i) => self.prefs.dmx.fixtures[i] = fixture.clone(),
                    None => self.prefs.dmx.fixtures.push(fixture.clone()),
                }
                self.save_prefs();
            }
            if ui
                .add_enabled_ui(saved.is_some(), |ui| {
                    widgets::btn(ui, "Remove from library", Kind::Secondary)
                })
                .inner
                .clicked()
                && let Some(i) = saved
            {
                self.prefs.dmx.fixtures.remove(i);
                self.save_prefs();
            }
        });
    }

    /// Basic fixture fields: name, pixel grid, channels per pixel, order, gamma, wiring.
    /// The name is applied on Enter / losing focus, not on every key; `name_id` keeps the
    /// text being typed.
    fn fixture_editor(&mut self, ui: &mut egui::Ui, f: &mut Fixture, name_id: egui::Id) {
        ui.horizontal(|ui| {
            ui.label("Name");
            let mut text = ui
                .data_mut(|d| d.get_temp::<String>(name_id))
                .unwrap_or_else(|| f.name.clone());
            let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(FIELD_W_NAME));
            if r.lost_focus() && !text.trim().is_empty() {
                f.name = text.trim().to_owned();
            }
            if r.has_focus() {
                ui.data_mut(|d| d.insert_temp(name_id, text));
            } else {
                ui.data_mut(|d| d.remove_temp::<String>(name_id));
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut f.width)
                    .range(1..=MAX_FIXTURE_SIDE)
                    .prefix("W "),
            );
            ui.add(
                egui::DragValue::new(&mut f.height)
                    .range(1..=MAX_FIXTURE_SIDE)
                    .prefix("H "),
            );
            while !dmx::valid(f) {
                // Keep the grid within what the mapper accepts.
                f.height = (f.height / 2).max(1);
                f.width = (f.width / 2).max(1);
            }
        });
        egui::ComboBox::from_label("Channels")
            .selected_text(format!("{:?}", f.format).to_uppercase())
            .show_ui(ui, |ui| {
                for v in [PixelFormat::Rgb, PixelFormat::Rgbw, PixelFormat::W] {
                    ui.selectable_value(&mut f.format, v, format!("{v:?}").to_uppercase());
                }
            });
        ui.add_enabled_ui(f.format != PixelFormat::W, |ui| {
            egui::ComboBox::from_label("Order")
                .selected_text(format!("{:?}", f.order).to_uppercase())
                .show_ui(ui, |ui| {
                    for v in [
                        ChannelOrder::Rgb,
                        ChannelOrder::Rbg,
                        ChannelOrder::Grb,
                        ChannelOrder::Gbr,
                        ChannelOrder::Brg,
                        ChannelOrder::Bgr,
                    ] {
                        ui.selectable_value(&mut f.order, v, format!("{v:?}").to_uppercase());
                    }
                });
        });
        widgets::slider(ui, &mut f.gamma, 0.5..=3.0, "Gamma");
        ui.checkbox(&mut f.serpentine, "Serpentine (odd rows reversed)");
    }

    /// Click on a slice number: plain selects only it; shift / cmd toggles it in the
    /// multi-selection (the clicked slice becomes the primary one when added).
    pub(crate) fn click_slice(&mut self, scr: &Screen, id: uuid::Uuid, multi: bool) {
        if !multi {
            self.sel_slice = Some(id);
            self.sel_slices.clear();
            return;
        }
        let first = scr.slices.first().map(|s| s.id);
        let primary = self.sel_slice.or(first);
        let mut set: Vec<uuid::Uuid> = scr
            .slices
            .iter()
            .filter(|s| Some(s.id) == primary || self.sel_slices.contains(&s.id))
            .map(|s| s.id)
            .collect();
        if let Some(i) = set.iter().position(|x| *x == id) {
            if set.len() > 1 {
                set.remove(i);
                if primary == Some(id) {
                    self.sel_slice = set.first().copied();
                }
            }
        } else {
            set.push(id);
            self.sel_slice = Some(id);
        }
        self.sel_slices = if set.len() > 1 { set } else { vec![] };
    }

    fn slice_editor(&mut self, ui: &mut egui::Ui, scr: &Screen, cmds: &mut Vec<Command>) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Slices");
                for (i, s) in scr.slices.iter().enumerate() {
                    let primary =
                        self.sel_slice == Some(s.id) || (self.sel_slice.is_none() && i == 0);
                    let on = primary || self.sel_slices.contains(&s.id);
                    if ui.selectable_label(on, format!("{}", i + 1)).clicked() {
                        let multi = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                        self.click_slice(scr, s.id, multi);
                        self.sel_pt_in = None;
                        self.sel_pt_out = None;
                    }
                }
                if crate::help::help(
                    widgets::btn(ui, "Add slice", Kind::Secondary),
                    "out-add-slice",
                )
                .clicked()
                {
                    let s = mapping::new_slice(scr.width, scr.height);
                    cmds.push(Command::AddSlice {
                        screen: scr.id,
                        slice: s,
                        index: scr.slices.len(),
                    });
                }
            });
            let Some(sl) = scr
                .slices
                .iter()
                .find(|s| Some(s.id) == self.sel_slice)
                .or(scr.slices.first())
            else {
                ui.label("No slices: this screen shows the whole composition.");
                return;
            };
            ui.horizontal(|ui| {
                let label = if self.selected_slices(scr, sl).len() > 1 {
                    "Remove slices"
                } else {
                    "Remove slice"
                };
                if crate::help::help(widgets::btn(ui, label, Kind::Secondary), "out-remove-slice")
                    .clicked()
                {
                    let mut rm: Vec<Command> = self
                        .selected_slices(scr, sl)
                        .into_iter()
                        .map(|slice| Command::RemoveSlice {
                            screen: scr.id,
                            slice,
                        })
                        .collect();
                    cmds.push(if rm.len() == 1 {
                        rm.remove(0)
                    } else {
                        Command::Batch { commands: rm }
                    });
                }
                ui.checkbox(&mut self.snap, "Snap");
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
            self.mask_and_edge_row(ui, scr, sl, cmds);
            self.color_row(ui, scr, sl, cmds);
            self.slice_inspector(ui, scr, sl, cmds);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.label("Input (composition; arrows move 1 px / Shift 10 px)");
                    self.input_canvas(ui, scr, sl, cmds);
                });
                ui.vertical(|ui| {
                    ui.label("Output (mesh; arrows move 1 px / Shift 10 px; Alt = no snap)");
                    self.output_canvas(ui, scr, sl, cmds);
                });
            });
        });
    }

    /// Mask polygon buttons and soft-edge sliders of the selected slice.
    fn mask_and_edge_row(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &vjc_model::Slice,
        cmds: &mut Vec<Command>,
    ) {
        let set_mask = |m: Option<Vec<(f32, f32)>>| Command::SetSliceMask {
            screen: scr.id,
            slice: sl.id,
            mask: m,
        };
        widgets::section(ui, "Mask", |ui| {
            ui.horizontal(|ui| {
                if sl.mask.is_none() {
                    self.edit_mask = false;
                    if crate::help::help(
                        widgets::btn(ui, "Create mask", Kind::Secondary),
                        "out-create-mask",
                    )
                    .clicked()
                    {
                        self.edit_mask = true;
                        self.sel_mask_pt = None;
                        cmds.push(set_mask(Some(vec![
                            (MASK_INSET, MASK_INSET),
                            (1.0 - MASK_INSET, MASK_INSET),
                            (1.0 - MASK_INSET, 1.0 - MASK_INSET),
                            (MASK_INSET, 1.0 - MASK_INSET),
                        ])));
                    }
                } else if let Some(m) = &sl.mask {
                    ui.checkbox(&mut self.edit_mask, "Edit points");
                    let can_rm = m.len() > 3 && self.sel_mask_pt.is_some_and(|i| i < m.len());
                    if ui
                        .add_enabled_ui(can_rm, |ui| {
                            widgets::btn(ui, "Remove point", Kind::Secondary)
                        })
                        .inner
                        .clicked()
                        && let Some(i) = self.sel_mask_pt.take()
                    {
                        let mut m = m.clone();
                        m.remove(i);
                        cmds.push(set_mask(Some(m)));
                    }
                    if crate::help::help(
                        widgets::btn(ui, "Clear mask", Kind::Secondary),
                        "out-clear-mask",
                    )
                    .clicked()
                    {
                        self.edit_mask = false;
                        self.sel_mask_pt = None;
                        cmds.push(set_mask(None));
                    }
                    if self.edit_mask {
                        ui.weak("click empty space to add a point");
                    }
                }
            });
        });
        widgets::section(ui, "Soft edge", |ui| {
            ui.horizontal_wrapped(|ui| {
                let mut on = sl.soft_edge.is_some();
                if ui.checkbox(&mut on, "Enabled").changed() {
                    cmds.push(Command::SetSliceSoftEdge {
                        screen: scr.id,
                        slice: sl.id,
                        soft_edge: on.then_some(vjc_model::SoftEdge {
                            left: SOFT_EDGE_DEFAULT,
                            right: SOFT_EDGE_DEFAULT,
                            top: 0.0,
                            bottom: 0.0,
                            gamma: 1.0,
                        }),
                    });
                }
                let Some(e) = &sl.soft_edge else { return };
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
        });
    }

    /// Brightness and RGB gains of a slice, to match panels of different batches.
    fn color_row(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &vjc_model::Slice,
        cmds: &mut Vec<Command>,
    ) {
        widgets::section(ui, "Colour", |ui| {
            ui.horizontal_wrapped(|ui| {
                let mut n = sl.color;
                let r = 0.0..=mapping::MAX_GAIN;
                ui.add(egui::Slider::new(&mut n.brightness, r.clone()).text("Brightness"));
                ui.add(egui::Slider::new(&mut n.contrast, r.clone()).text("Contrast"));
                ui.add(egui::Slider::new(&mut n.r, r.clone()).text("R"));
                ui.add(egui::Slider::new(&mut n.g, r.clone()).text("G"));
                widgets::slider(ui, &mut n.b, r, "B");
                if widgets::btn(ui, "Reset", Kind::Secondary).clicked() {
                    n = Default::default();
                }
                if n != sl.color {
                    cmds.push(Command::SetSliceColor {
                        screen: scr.id,
                        slice: sl.id,
                        color: n,
                    });
                }
            });
        });
    }

    fn input_canvas(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &vjc_model::Slice,
        cmds: &mut Vec<Command>,
    ) {
        let c = &self.project.composition;
        let rect = canvas_rect(ui, OUTPUT_CANVAS_W, c.width as f32 / c.height.max(1) as f32);
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        if let Some(id) = self.comp_tex {
            p.image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        let px = |q: (f32, f32)| {
            egui::pos2(
                rect.min.x + q.0 * rect.width(),
                rect.min.y + q.1 * rect.height(),
            )
        };
        for s in &scr.slices {
            let on = s.id == sl.id;
            let col = if on {
                ui.visuals().selection.stroke.color
            } else {
                ui.visuals().widgets.noninteractive.fg_stroke.color
            };
            let pts: Vec<egui::Pos2> = s.input.iter().map(|q| px(*q)).collect();
            p.add(egui::Shape::closed_line(
                pts,
                egui::Stroke::new(STROKE_HANDLE, col),
            ));
        }
        let id = egui::Id::new(("slice-in", sl.id));
        let snap = 0.0;
        let mut sel = self.sel_pt_in;
        let comp = (c.width, c.height);
        let body = slice_geom::bbox(&sl.input);
        let drag = handles(
            ui,
            id,
            (rect, (1.0, 1.0)),
            &sl.input,
            &mut sel,
            Opts {
                snap,
                keys: false,
                body,
            },
        );
        self.sel_pt_in = sel;
        if let Some((start, acc)) = drag.body {
            // Normalized canvas units -> composition pixels, where Nudge works.
            let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
            let start = slice_geom::Rect {
                x: start.x * cw,
                y: start.y * ch,
                w: start.w * cw,
                h: start.h * ch,
            };
            let acc = (acc.0 * cw, acc.1 * ch);
            let thr = GUIDE_PX * cw / rect.width();
            if let Some(cmd) = self.drag_body(ui, scr, sl, Side::Input, (start, acc), thr) {
                cmds.push(cmd);
            }
        } else if let Some((i, q)) = drag.moved {
            let others: Vec<_> = scr
                .slices
                .iter()
                .filter(|s| s.id != sl.id)
                .filter_map(|s| slice_geom::bbox(&s.input))
                .collect();
            let q = snap_dragged(ui, q, (1.0, 1.0), &others, 1.0 / rect.width());
            let mut input = sl.input.clone();
            input[i] = q;
            cmds.push(Command::SetSliceInput {
                screen: scr.id,
                slice: sl.id,
                input,
            });
        } else if sel.is_none()
            && let Some(by) = arrow_move(ui, rect)
            && let Some(cmd) = self.arrow_nudge(ui, scr, sl, Side::Input, by)
        {
            cmds.push(cmd);
        }
    }

    fn output_canvas(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &vjc_model::Slice,
        cmds: &mut Vec<Command>,
    ) {
        let rect = canvas_rect(
            ui,
            OUTPUT_CANVAS_W,
            scr.width as f32 / scr.height.max(1) as f32,
        );
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let (cols, rows) = (
            sl.mesh_cols as usize,
            mapping::rows_of(sl.output_mesh.len(), sl.mesh_cols) as usize,
        );
        if !mapping::mesh_valid(&sl.output_mesh, sl.mesh_cols) {
            return;
        }
        let px = |q: (f32, f32)| {
            egui::pos2(
                rect.min.x + q.0 / scr.width as f32 * rect.width(),
                rect.min.y + q.1 / scr.height as f32 * rect.height(),
            )
        };
        let stroke = ui.visuals().widgets.noninteractive.fg_stroke;
        for j in 0..rows {
            for i in 0..cols {
                let a = px(sl.output_mesh[j * cols + i]);
                if i + 1 < cols {
                    p.line_segment([a, px(sl.output_mesh[j * cols + i + 1])], stroke);
                }
                if j + 1 < rows {
                    p.line_segment([a, px(sl.output_mesh[(j + 1) * cols + i])], stroke);
                }
            }
        }
        if self.edit_mask
            && let Some(m) = &sl.mask
        {
            self.mask_canvas(ui, scr, sl, m, rect, cmds);
            return;
        }
        let id = egui::Id::new(("slice-out", sl.id));
        let snap = if self.snap { SNAP_PX } else { 0.0 };
        let space = (scr.width as f32, scr.height as f32);
        let mut sel = self.sel_pt_out;
        let body = slice_geom::bbox(&sl.output_mesh);
        let drag = handles(
            ui,
            id,
            (rect, space),
            &sl.output_mesh,
            &mut sel,
            Opts {
                snap,
                keys: true,
                body,
            },
        );
        self.sel_pt_out = sel;
        if let Some((start, acc)) = drag.body {
            let thr = GUIDE_PX * space.0 / rect.width();
            if let Some(cmd) = self.drag_body(ui, scr, sl, Side::Output, (start, acc), thr) {
                cmds.push(cmd);
            }
        } else if let Some((i, q)) = drag.moved {
            let others: Vec<_> = scr
                .slices
                .iter()
                .filter(|s| s.id != sl.id)
                .filter_map(|s| slice_geom::bbox(&s.output_mesh))
                .collect();
            let q = snap_dragged(ui, q, space, &others, space.0 / rect.width());
            let mut mesh = sl.output_mesh.clone();
            mesh[i] = q;
            cmds.push(Command::SetSliceMesh {
                screen: scr.id,
                slice: sl.id,
                mesh,
                cols: sl.mesh_cols,
            });
        } else if sel.is_none()
            && let Some(by) = arrow_move(ui, rect)
            && let Some(cmd) = self.arrow_nudge(ui, scr, sl, Side::Output, by)
        {
            // No point selected: the arrows move the whole slice.
            cmds.push(cmd);
        }
    }

    /// Mask polygon over the output canvas (normalized screen coordinates).
    fn mask_canvas(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        sl: &vjc_model::Slice,
        m: &[(f32, f32)],
        rect: egui::Rect,
        cmds: &mut Vec<Command>,
    ) {
        let px = |q: (f32, f32)| {
            egui::pos2(
                rect.min.x + q.0 * rect.width(),
                rect.min.y + q.1 * rect.height(),
            )
        };
        let col = ui.visuals().selection.stroke.color;
        let pts: Vec<egui::Pos2> = m.iter().map(|q| px(*q)).collect();
        ui.painter_at(rect).add(egui::Shape::closed_line(
            pts,
            egui::Stroke::new(STROKE_CANVAS, col),
        ));
        let id = egui::Id::new(("slice-mask", sl.id));
        // The selection may come from another slice's mask: drop it if out of range.
        let mut sel = self.sel_mask_pt.filter(|&i| i < m.len());
        let moved = handles(
            ui,
            id,
            (rect, (1.0, 1.0)),
            m,
            &mut sel,
            Opts {
                snap: 0.0,
                keys: true,
                body: None,
            },
        )
        .moved;
        let set = |mask: Vec<(f32, f32)>| Command::SetSliceMask {
            screen: scr.id,
            slice: sl.id,
            mask: Some(mask),
        };
        if let Some((i, q)) = moved.filter(|&(i, _)| i < m.len()) {
            let mut mask = m.to_vec();
            mask[i] = q;
            cmds.push(set(mask));
        } else if let Some(at) = ui.input(|i| {
            (i.pointer.primary_clicked() && ui.rect_contains_pointer(rect))
                .then(|| i.pointer.interact_pos())
                .flatten()
        }) {
            let q = (
                (at.x - rect.min.x) / rect.width(),
                (at.y - rect.min.y) / rect.height(),
            );
            let radius = HANDLE_R * HANDLE_GRAB_K / rect.width();
            if mapping::hit_test(m, q, radius).is_none() && m.len() < mapping::MAX_MASK_POINTS {
                let i = mapping::nearest_edge_insert(m, q);
                let mut mask = m.to_vec();
                mask.insert(i, q);
                sel = Some(i);
                cmds.push(set(mask));
            }
        }
        self.sel_mask_pt = sel;
    }
}
