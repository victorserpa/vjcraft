//! Preferences window (File > Preferences..., Cmd+,). Edits apply live;
//! the file is written when the window closes.

use crate::{PrefsTab, State, api_io};
use eframe::egui;
use vjc_model::FitMode;

impl State {
    pub(crate) fn prefs_window(&mut self, ctx: &egui::Context) {
        if !self.prefs_open {
            return;
        }
        let mut open = true;
        let mut done = false;
        let before = self.prefs.clone();
        egui::Window::new("Preferences")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (t, l) in [
                        (PrefsTab::General, "General"),
                        (PrefsTab::Audio, "Audio"),
                        (PrefsTab::Video, "Video"),
                        (PrefsTab::Control, "Control"),
                        (PrefsTab::Osc, "OSC"),
                        (PrefsTab::Dmx, "DMX"),
                        (PrefsTab::Api, "Remote API"),
                        (PrefsTab::Timecode, "Timecode"),
                    ] {
                        let r = ui.selectable_value(&mut self.prefs_tab, t, l);
 let key = match t { PrefsTab::General => "prefs-general", PrefsTab::Audio => "prefs-audio", PrefsTab::Video => "prefs-video", PrefsTab::Control => "prefs-control", PrefsTab::Osc => "prefs-osc", PrefsTab::Dmx => "prefs-dmx", PrefsTab::Api => "prefs-api", PrefsTab::Timecode => "prefs-timecode" };
 crate::help::help(r, key);
                    }
                });
                ui.separator();
                let p = &mut self.prefs;
                match self.prefs_tab {
                    PrefsTab::General => {
                        ui.checkbox(&mut p.general.autosave, "Autosave recovery file");
                        ui.add_enabled_ui(p.general.autosave, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut p.general.autosave_secs)
                                    .range(5..=3600)
                                    .suffix(" s"),
                            );
                        });
                        ui.checkbox(
                            &mut p.general.open_last_project,
                            "Open last project at startup",
                        );
                        ui.horizontal(|ui| {
                            ui.label("Browser start folder");
                            let mut t = p
                                .general
                                .browser_folder
                                .as_ref()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            if ui.text_edit_singleline(&mut t).changed() {
                                p.general.browser_folder =
                                    (!t.trim().is_empty()).then(|| t.trim().into());
                            }
                            if ui.button("Choose...").clicked()
                                && let Some(d) = rfd::FileDialog::new().pick_folder()
                            {
                                p.general.browser_folder = Some(d);
                            }
                        });
                        ui.weak("Empty = project folder, else ~/Movies. Applies on next launch.");
                        ui.horizontal(|ui| {
                            ui.label("ML models folder");
                            let mut t = p
                                .general
                                .models_folder
                                .as_ref()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            if ui.text_edit_singleline(&mut t).changed() {
                                p.general.models_folder =
                                    (!t.trim().is_empty()).then(|| t.trim().into());
                            }
                            if ui.button("Choose...").clicked()
                                && let Some(d) = rfd::FileDialog::new().pick_folder()
                            {
                                p.general.models_folder = Some(d);
                            }
                        });
                        ui.weak(
                            "Where Depth and Remove Background look for their .onnx files \
                             (empty = the app data folder, models/). See docs/ml.md.",
                        );
                    }
                    PrefsTab::Audio => {
                        let outputs = self
                            .audio_outputs
                            .get_or_insert_with(vjc_media::audio::output_devices);
                        let cur = p.audio.output_device.clone();
                        egui::ComboBox::from_label("Output device")
                            .selected_text(cur.as_deref().unwrap_or("(default)"))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut p.audio.output_device, None, "(default)");
                                for n in outputs.iter() {
                                    ui.selectable_value(&mut p.audio.output_device, Some(n.clone()), n);
                                }
                            });
                        if ui.button("Refresh devices").clicked() {
                            self.audio_outputs = None;
                        }
                        ui.weak("Applies to clips started after the change.");
                    }
                    PrefsTab::Video => {
                        ui.label("New composition");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut p.video.width).range(1..=16384));
                            ui.label("x");
                            ui.add(egui::DragValue::new(&mut p.video.height).range(1..=16384));
                            ui.add(
                                egui::DragValue::new(&mut p.video.fps)
                                    .range(1.0..=240.0)
                                    .suffix(" fps"),
                            );
                        });
                        egui::ComboBox::from_label("Default clip fit")
                            .selected_text(p.video.fit.label())
                            .show_ui(ui, |ui| {
                                for f in FitMode::ALL {
                                    ui.selectable_value(&mut p.video.fit, f, f.label());
                                }
                            });
                        ui.checkbox(&mut p.video.vsync, "VSync (applies on next launch)");
                    }
                    PrefsTab::Control => {
                        ui.checkbox(
                            &mut p.control.default_shortcuts,
                            "New projects get the default shortcut preset",
                        );
                        ui.checkbox(
                            &mut p.control.offer_shortcuts,
                            "Offer the preset when opening a project without shortcuts",
                        );
                    }
                    PrefsTab::Osc => {
                        let o = &mut p.osc;
                        ui.checkbox(&mut o.in_enabled, "Receive OSC");
                        ui.add_enabled_ui(o.in_enabled, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Port");
                                ui.add(egui::DragValue::new(&mut o.in_port).range(1..=65535));
                            });
                            ui.checkbox(
                                &mut o.accept_network,
                                "Accept from network (default: this computer only)",
                            );
                        });
                        ui.separator();
                        ui.checkbox(&mut o.out_enabled, "Send OSC feedback");
                        ui.add_enabled_ui(o.out_enabled, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Host");
                                ui.text_edit_singleline(&mut o.out_host);
                                ui.label("Port");
                                ui.add(egui::DragValue::new(&mut o.out_port).range(1..=65535));
                            });
                        });
                        ui.weak("Addresses like /comp/layer/1/opacity; learn others in Shortcuts > Edit MIDI.");
                    }
                    PrefsTab::Dmx => {
                        let d = &mut p.dmx;
                        ui.checkbox(&mut d.enabled, "Receive Art-Net (DMX in)");
                        ui.add_enabled_ui(d.enabled, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Interface IP");
                                ui.add(
                                    egui::TextEdit::singleline(&mut d.interface).desired_width(110.0),
                                );
                                ui.label("Port");
                                ui.add(egui::DragValue::new(&mut d.port).range(1..=65535));
                            });
                            ui.horizontal(|ui| {
                                ui.label("First universe");
                                ui.add(egui::DragValue::new(&mut d.universe).range(0..=32767));
                                ui.label("Count");
                                ui.add(egui::DragValue::new(&mut d.universes).range(1..=64));
                            });
                        });
                        if d.enabled {
                            match self.dmx.status() {
                                crate::dmx_io::Status::Listening(addr) => {
                                    ui.weak(format!("Listening on {addr}"));
                                }
                                crate::dmx_io::Status::Failed(e) => {
                                    ui.colored_label(ui.visuals().error_fg_color, e);
                                }
                                crate::dmx_io::Status::Off => {
                                    ui.weak("Starting...");
                                }
                            }
                        }
                        ui.weak("127.0.0.1 = this computer only; 0.0.0.0 = every interface (anyone on the network can then send). Channels map to shortcuts in Shortcuts > Edit MIDI (move a fader to learn), value 0..255 = range.");
                        ui.separator();
                        ui.label(format!("Saved fixtures: {}", d.fixtures.len()));
                        let mut gone = None;
                        for (i, f) in d.fixtures.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "{} ({}x{}, {:?})",
                                    f.name, f.width, f.height, f.format
                                ));
                                if ui.small_button("Remove").clicked() {
                                    gone = Some(i);
                                }
                            });
                        }
                        if let Some(i) = gone {
                            d.fixtures.remove(i);
                        }
                        ui.weak("Add fixtures from Output > Setup (device Art-Net).");
                    }
                    PrefsTab::Timecode => {
                        use vjc_io::prefs::{OnLoss, TcSource};
                        let (ports, inputs) = self.tc_devices.get_or_insert_with(|| {
                            (
                                crate::midi_io::input_ports(),
                                vjc_media::ltc_in::input_devices(),
                            )
                        });
                        let t = &mut p.timecode;
                        let kind = match t.source {
                            TcSource::Off => 0,
                            TcSource::Mtc { .. } => 1,
                            TcSource::Ltc { .. } => 2,
                        };
                        let mut pick = kind;
                        ui.horizontal(|ui| {
                            ui.label("Source");
                            for (i, l) in ["Off", "MTC (MIDI)", "LTC (audio)"].into_iter().enumerate() {
                                ui.selectable_value(&mut pick, i, l);
                            }
                        });
                        if pick != kind {
                            t.source = match pick {
                                1 => TcSource::Mtc {
                                    port: ports.first().cloned().unwrap_or_default(),
                                },
                                2 => TcSource::Ltc {
                                    device: String::new(),
                                    channel: 0,
                                },
                                _ => TcSource::Off,
                            };
                        }
                        match &mut t.source {
                            TcSource::Off => {}
                            TcSource::Mtc { port } => {
                                egui::ComboBox::from_label("MIDI port")
                                    .selected_text(if port.is_empty() { "(none)" } else { port.as_str() })
                                    .show_ui(ui, |ui| {
                                        for n in ports.iter() {
                                            ui.selectable_value(port, n.clone(), n);
                                        }
                                    });
                            }
                            TcSource::Ltc { device, channel } => {
                                egui::ComboBox::from_label("Audio input")
                                    .selected_text(if device.is_empty() { "(default)" } else { device.as_str() })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(device, String::new(), "(default)");
                                        for n in inputs.iter() {
                                            ui.selectable_value(device, n.clone(), n);
                                        }
                                    });
                                ui.horizontal(|ui| {
                                    ui.label("Channel");
                                    let mut c = u32::from(*channel) + 1;
                                    if ui.add(egui::DragValue::new(&mut c).range(1..=64)).changed() {
                                        *channel = (c - 1) as u16;
                                    }
                                });
                                let mut auto = t.ltc_fps.is_none();
                                ui.horizontal(|ui| {
                                    if ui.checkbox(&mut auto, "Detect frame rate").changed() {
                                        t.ltc_fps = (!auto).then_some(vjc_model::TcFps::F25);
                                    }
                                    if let Some(f) = &mut t.ltc_fps {
                                        for o in vjc_model::TcFps::ALL {
                                            ui.selectable_value(f, o, o.label());
                                        }
                                    }
                                });
                            }
                        }
                        if ui.button("Refresh devices").clicked() {
                            self.tc_devices = None;
                        }
                        ui.horizontal(|ui| {
                            ui.label("Offset");
                            ui.add(
                                egui::DragValue::new(&mut t.offset_frames)
                                    .range(-1_000_000..=1_000_000)
                                    .suffix(" frames"),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("On signal loss");
                            ui.selectable_value(&mut t.on_loss, OnLoss::Freeze, "Freeze");
                            ui.selectable_value(&mut t.on_loss, OnLoss::FreeRun, "Run free");
                        });
                        ui.weak("Clips with Timecode sync (Clip tab) and cues (Composition tab) follow this source.");
                    }
                    PrefsTab::Api => {
                        let a = &mut p.api;
                        ui.checkbox(&mut a.enabled, "Enable Remote API (HTTP + WebSocket)");
                        ui.add_enabled_ui(a.enabled, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Port");
                                ui.add(egui::DragValue::new(&mut a.port).range(1..=65534));
                                ui.weak(format!("WebSocket: {}", u32::from(a.port) + 1));
                            });
                            ui.checkbox(
                                &mut a.expose_network,
                                "Expose to the network (default: this computer only)",
                            );
                        });
                        if a.enabled && a.expose_network {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Warning: no authentication. Anyone on your network can control VJCraft.",
                            );
                        }
                        if a.enabled {
                            match self.api.status() {
                                api_io::Status::Listening(addr) => {
                                    ui.weak(format!("Listening on {addr}"));
                                }
                                api_io::Status::Failed(e) => {
                                    ui.colored_label(ui.visuals().error_fg_color, e);
                                }
                                api_io::Status::Off => {
                                    ui.weak("Starting...");
                                }
                            }
                        }
                        ui.weak("See docs/api.md. Applies immediately.");
                    }
                }
                ui.separator();
                if ui.button("Close").clicked() {
                    done = true;
                }
            });
        if self.prefs != before {
            self.prefs_dirty = true;
        }
        if !open || done {
            self.prefs_open = false;
            if self.prefs_dirty {
                self.save_prefs();
            }
        }
    }
}
