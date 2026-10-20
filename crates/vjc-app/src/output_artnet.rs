//! Art-Net part of the Output setup properties: target, port-address, rate, delay and the
//! fixture list of a screen whose device is Art-Net.

use crate::State;
use crate::artnet_out;
use crate::widgets::{self, Kind, Width, field_int, field_text, toggle};
use eframe::egui;
use vjc_core::{Command, dmx};
use vjc_model::{ChannelOrder, Fixture, OutputDevice, PixelFormat, Screen};

/// Largest fixture grid side the fields accept.
const MAX_FIXTURE_SIDE: u32 = 1024;

impl State {
    /// Art-Net device: target IP (typed or picked from the nodes found on the network),
    /// port-address as net / sub-net / universe, rate and delay, and the list of fixtures.
    pub(crate) fn artnet_setup(
        &mut self,
        ui: &mut egui::Ui,
        scr: &Screen,
        cmds: &mut Vec<Command>,
    ) {
        let OutputDevice::ArtNet {
            ip,
            universe,
            fixtures,
            fps,
            delay_ms,
        } = &scr.device
        else {
            return;
        };
        ui.separator();
        let mut new_ip = ip.clone();
        let mut new_uni = *universe;
        let mut new_fx = fixtures.clone();
        let (mut new_fps, mut new_delay) = (*fps, *delay_ms);
        widgets::section(ui, "Art-Net", |ui| {
            // The text is applied when the field loses focus, not on every key.
            let ip_id = egui::Id::new(("artnet_ip", scr.id));
            let mut text = ui
                .data_mut(|d| d.get_temp::<String>(ip_id))
                .unwrap_or_else(|| ip.clone());
            ui.horizontal(|ui| {
                ui.label("IP");
                let r = field_text(ui, &mut text, Width::Md, None);
                if r.lost_focus() && text.trim() != ip {
                    new_ip = text.trim().to_owned();
                }
                if r.has_focus() {
                    ui.data_mut(|d| d.insert_temp(ip_id, text.clone()));
                } else {
                    ui.data_mut(|d| d.remove_temp::<String>(ip_id));
                }
                let scanning = self.artnet_scan.running();
                let label = if scanning {
                    "Scanning..."
                } else {
                    "Find nodes"
                };
                if ui
                    .add_enabled_ui(!scanning, |ui| widgets::btn(ui, label, Kind::Secondary))
                    .inner
                    .clicked()
                {
                    self.artnet_scan.start();
                }
            });
            widgets::help(ui, "host or host:port (default port 6454)");
            for n in self.artnet_scan.nodes() {
                if widgets::btn(ui, &format!("{}  {}", n.ip, n.name), Kind::Secondary).clicked() {
                    new_ip = n.ip.to_string();
                    ui.data_mut(|d| d.remove_temp::<String>(ip_id));
                }
            }
            let (mut net, mut sub, mut uni) = {
                let (n, s, u) = dmx::split_port(*universe);
                (f32::from(n), f32::from(s), f32::from(u))
            };
            ui.horizontal(|ui| {
                ui.label("Net");
                let a = widgets::field_num(ui, &mut net, 0.0..=127.0, 0.2).changed();
                ui.label("Sub-net");
                let b = widgets::field_num(ui, &mut sub, 0.0..=15.0, 0.1).changed();
                ui.label("Universe");
                let c = widgets::field_num(ui, &mut uni, 0.0..=15.0, 0.1).changed();
                if a || b || c {
                    new_uni = dmx::join_port(net as u8, sub as u8, uni as u8);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Max Hz");
                let mut hz = if *fps > 0.0 { *fps } else { artnet_out::MAX_HZ };
                if widgets::field_num(ui, &mut hz, 1.0..=artnet_out::TOP_HZ, 0.2).changed() {
                    new_fps = hz;
                }
                ui.label("Delay ms");
                let mut d = *delay_ms as f32;
                if widgets::field_num(ui, &mut d, 0.0..=1000.0, 1.0).changed() {
                    new_delay = d.round() as u32;
                }
            });
            let mut remove = None;
            let count = new_fx.len();
            // The properties panel scrolls, so the list does not need a scroll area of its own.
            for (i, f) in new_fx.iter_mut().enumerate() {
                ui.push_id(("artnet_fixture", scr.id, i), |ui| {
                    ui.group(|ui| {
                        if self.artnet_fixture(ui, f, scr, i, count > 1) {
                            remove = Some(i);
                        }
                    });
                });
            }
            if let Some(i) = remove {
                new_fx.remove(i);
            }
            if widgets::btn(ui, "Add fixture", Kind::Secondary).clicked() {
                let mut f = new_fx.last().cloned().unwrap_or_default();
                let used = u64::from(f.width)
                    * u64::from(f.height)
                    * dmx::channels_per_pixel(f.format) as u64;
                let next = u64::from(f.start_channel) + used;
                f.start_channel = if next <= dmx::MAX_CHANNELS as u64 {
                    next as u16
                } else {
                    1
                };
                new_fx.push(f);
            }
        });
        if new_ip != *ip
            || new_uni != *universe
            || new_fx != *fixtures
            || new_fps != *fps
            || new_delay != *delay_ms
        {
            cmds.push(Command::SetScreenDevice {
                screen: scr.id,
                device: OutputDevice::ArtNet {
                    ip: new_ip,
                    universe: new_uni,
                    fixtures: new_fx,
                    fps: new_fps,
                    delay_ms: new_delay,
                },
            });
        }
    }

    /// One fixture of an Art-Net device: library pick, grid, mapping, input rectangle, flips
    /// and colour. Returns true when its Remove button was clicked.
    fn artnet_fixture(
        &mut self,
        ui: &mut egui::Ui,
        f: &mut Fixture,
        scr: &Screen,
        index: usize,
        can_remove: bool,
    ) -> bool {
        let library: Vec<Fixture> = dmx::presets()
            .into_iter()
            .chain(self.prefs.dmx.fixtures.iter().cloned())
            .collect();
        egui::ComboBox::from_label("Fixture")
            .selected_text(&f.name)
            .show_ui(ui, |ui| {
                for l in &library {
                    if ui.selectable_label(l.name == f.name, &l.name).clicked() {
                        // The library holds the grid and wiring; where it sits stays.
                        *f = Fixture {
                            start_channel: f.start_channel,
                            input: f.input,
                            input_slice: f.input_slice,
                            flip_h: f.flip_h,
                            flip_v: f.flip_v,
                            color: f.color,
                            ..l.clone()
                        };
                    }
                }
            });
        self.fixture_editor(ui, f, egui::Id::new(("artnet_fx_name", scr.id, index)));
        let mut sc = f32::from(f.start_channel);
        ui.horizontal(|ui| {
            ui.label("Start channel");
            if widgets::field_num(ui, &mut sc, 1.0..=512.0, 1.0).changed() {
                f.start_channel = sc.round().clamp(1.0, 512.0) as u16;
            }
        });
        let cpp = dmx::channels_per_pixel(f.format);
        let channels = (u64::from(f.width) * u64::from(f.height)) as usize * cpp;
        widgets::help(
            ui,
            &format!(
                "Channels {}-{}, {} universe(s)",
                f.start_channel,
                usize::from(f.start_channel) + channels.saturating_sub(1),
                dmx::universes_used(f.start_channel, channels, cpp)
            ),
        );
        ui.horizontal(|ui| {
            toggle(ui, &mut f.flip_h, "Flip H");
            toggle(ui, &mut f.flip_v, "Flip V");
        });
        // What the fixture samples: a free rectangle, or the area of one slice that it follows.
        let slice_label = |n: usize, s: &vjc_model::Slice| {
            if s.name.is_empty() {
                format!("Slice {}", n + 1)
            } else {
                s.name.clone()
            }
        };
        let followed = f
            .input_slice
            .and_then(|id| scr.slices.iter().enumerate().find(|(_, s)| s.id == id));
        let selected = match followed {
            Some((n, s)) => format!("Slice: {}", slice_label(n, s)),
            None => "Free rect".to_string(),
        };
        egui::ComboBox::from_label("Samples")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(f.input_slice.is_none(), "Free rect")
                    .clicked()
                {
                    f.input_slice = None;
                }
                for (n, s) in scr.slices.iter().enumerate() {
                    let on = f.input_slice == Some(s.id);
                    if ui
                        .selectable_label(on, format!("Slice: {}", slice_label(n, s)))
                        .clicked()
                    {
                        f.input_slice = Some(s.id);
                    }
                }
            });
        if f.input_slice.is_some() && followed.is_none() {
            widgets::help(
                ui,
                "The followed slice is gone; sampling the free rectangle instead.",
            );
        }
        if f.input_slice.is_none() {
            ui.label("Input (centre, size, rotation)");
            ui.horizontal(|ui| {
                widgets::field_num(ui, &mut f.input.x, -2.0..=3.0, 0.005);
                widgets::field_num(ui, &mut f.input.y, -2.0..=3.0, 0.005);
                widgets::field_num(ui, &mut f.input.w, 0.01..=4.0, 0.005);
                widgets::field_num(ui, &mut f.input.h, 0.01..=4.0, 0.005);
                widgets::field_num(ui, &mut f.input.rotation, -360.0..=360.0, 0.5);
                if widgets::btn(ui, "Whole screen", Kind::Secondary).clicked() {
                    f.input = Default::default();
                }
            });
            // One-off copy of a slice's area into the free rectangle. The slices are the
            // working copy's (`scr`), not the project's, which lags while the window is open.
            ui.menu_button("Fit to slice", |ui| {
                for (n, s) in scr.slices.iter().enumerate() {
                    if ui.button(format!("Slice {}", n + 1)).clicked() {
                        if let Some(i) = dmx::input_for_slice(s, scr.width, scr.height) {
                            f.input = i;
                        }
                        ui.close();
                    }
                }
            });
        }
        widgets::slider(ui, &mut f.color.brightness, 0.0..=2.0, "Brightness");
        widgets::slider(ui, &mut f.color.contrast, 0.0..=2.0, "Contrast");
        widgets::slider(ui, &mut f.color.r, 0.0..=2.0, "Red");
        widgets::slider(ui, &mut f.color.g, 0.0..=2.0, "Green");
        widgets::slider(ui, &mut f.color.b, 0.0..=2.0, "Blue");
        let mut remove = false;
        ui.horizontal(|ui| {
            let saved = self
                .prefs
                .dmx
                .fixtures
                .iter()
                .position(|l| l.name == f.name);
            if widgets::btn(ui, "Save to library", Kind::Secondary).clicked() {
                match saved {
                    Some(i) => self.prefs.dmx.fixtures[i] = f.clone(),
                    None => self.prefs.dmx.fixtures.push(f.clone()),
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
            if ui
                .add_enabled_ui(can_remove, |ui| {
                    widgets::btn(ui, "Remove fixture", Kind::Danger)
                })
                .inner
                .clicked()
            {
                remove = true;
            }
        });
        remove
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
            let r = field_text(ui, &mut text, Width::Md, None);
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
            field_int(ui, &mut f.width, 1..=MAX_FIXTURE_SIDE, "W ", "");
            field_int(ui, &mut f.height, 1..=MAX_FIXTURE_SIDE, "H ", "");
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
        toggle(ui, &mut f.serpentine, "Serpentine (odd rows reversed)");
    }
}
