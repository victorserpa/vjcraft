//! Preferences window (File > Preferences..., Cmd+,). Edits apply live;
//! the file is written when the window closes.

use crate::widgets::{
    Kind, Width, btn, field_int, field_text, help, panel_window, section, toggle,
};
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
        panel_window(ctx, "Preferences")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (t, l) in [
                        (PrefsTab::General, "General"),
                        (PrefsTab::Recording, "Recording"),
                        (PrefsTab::Defaults, "Defaults"),
                        (PrefsTab::Audio, "Audio"),
                        (PrefsTab::Video, "Video"),
                        (PrefsTab::Control, "Control"),
                        (PrefsTab::Midi, "MIDI"),
                        (PrefsTab::Osc, "OSC"),
                        (PrefsTab::Dmx, "DMX"),
                        (PrefsTab::Api, "Remote API"),
                        (PrefsTab::Timecode, "Timecode"),
                    ] {
                        let r = ui.selectable_value(&mut self.prefs_tab, t, l);
 let key = match t { PrefsTab::General => "prefs-general", PrefsTab::Audio => "prefs-audio", PrefsTab::Recording => "prefs-recording", PrefsTab::Defaults => "prefs-defaults", PrefsTab::Video => "prefs-video", PrefsTab::Control => "prefs-control", PrefsTab::Osc => "prefs-osc", PrefsTab::Dmx => "prefs-dmx", PrefsTab::Api => "prefs-api", PrefsTab::Timecode => "prefs-timecode", PrefsTab::Midi => "prefs-midi" };
 crate::help::help(r, key);
                    }
                });
                ui.separator();
                let p = &mut self.prefs;
                match self.prefs_tab {
                    PrefsTab::General => {
                        toggle(ui, &mut p.general.autosave, "Autosave recovery file");
                        ui.add_enabled_ui(p.general.autosave, |ui| {
                            field_int(ui, &mut p.general.autosave_secs, 5..=3600, "", " s");
                        });
                        toggle(ui, &mut p.general.open_last_project, "Open last project at startup");
                        toggle(ui, &mut p.general.confirm_quit, "Confirm before quitting");
                        ui.horizontal(|ui| {
                            ui.label("Clip start offset");
                            field_int(ui, &mut p.general.clip_start_offset_ms, 0..=600_000, "", " ms");
                        });
                        help(ui, "A triggered clip starts this far into the file (0 = at its in-point).");
                        ui.horizontal(|ui| {
                            ui.label("Interface scale");
                            ui.add(
                                egui::Slider::new(&mut p.general.ui_scale, 0.75..=2.0)
                                    .step_by(0.05)
                                    .fixed_decimals(2),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Browser start folder");
                            let mut t = p
                                .general
                                .browser_folder
                                .as_ref()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            if field_text(ui, &mut t, Width::Lg, None).changed() {
                                p.general.browser_folder =
                                    (!t.trim().is_empty()).then(|| t.trim().into());
                            }
                            if btn(ui, "Choose...", Kind::Secondary).clicked()
                                && let Some(d) = rfd::FileDialog::new().pick_folder()
                            {
                                p.general.browser_folder = Some(d);
                            }
                        });
                        help(ui, "Empty = project folder, else ~/Movies. Applies on next launch.");
                        ui.horizontal(|ui| {
                            ui.label("ML models folder");
                            let mut t = p
                                .general
                                .models_folder
                                .as_ref()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            if field_text(ui, &mut t, Width::Lg, None).changed() {
                                p.general.models_folder =
                                    (!t.trim().is_empty()).then(|| t.trim().into());
                            }
                            if btn(ui, "Choose...", Kind::Secondary).clicked()
                                && let Some(d) = rfd::FileDialog::new().pick_folder()
                            {
                                p.general.models_folder = Some(d);
                            }
                        });
                        help(
                            ui,
                            "Where Depth and Remove Background look for their .onnx files \
                             (empty = the app data folder, models/). See docs/ml.md.",
                        );
                    }
                    PrefsTab::Recording => {
                        ui.horizontal(|ui| {
                            ui.label("Recording folder");
                            // Edited in a scratch string; committed on Enter or focus loss.
                            let id = ui.id().with("record-folder-text");
                            let saved = p
                                .general
                                .record_folder
                                .as_ref()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            let mut t = ui
                                .data(|d| d.get_temp::<String>(id))
                                .unwrap_or_else(|| saved.clone());
                            let r = field_text(ui, &mut t, Width::Lg, None);
                            if r.lost_focus() {
                                p.general.record_folder = crate::capture::absolute_folder(&t);
                                ui.data_mut(|d| d.remove_temp::<String>(id));
                            } else if r.has_focus() {
                                ui.data_mut(|d| d.insert_temp(id, t));
                            } else {
                                ui.data_mut(|d| d.remove_temp::<String>(id));
                            }
                            if btn(ui, "Choose...", Kind::Secondary).clicked()
                                && let Some(d) = rfd::FileDialog::new().pick_folder()
                            {
                                p.general.record_folder = Some(d);
                            }
                        });
                        help(
                            ui,
                            "Empty = ~/Movies/VJCraft. Recordings carry the master audio (not the PNG sequence).",
                        );
                        let have = self
                            .rec_codecs
                            .get_or_insert_with(vjc_media::record::available)
                            .clone();
                        let cur = vjc_media::record::RecSettings::from_prefs(
                            &p.record.codec,
                            p.record.quality,
                            &have,
                        );
                        egui::ComboBox::from_label("Recording codec")
                            .selected_text(cur.codec.label())
                            .show_ui(ui, |ui| {
                                for c in &have {
                                    if ui.selectable_label(*c == cur.codec, c.label()).clicked() {
                                        p.record.codec = c.id().into();
                                    }
                                }
                            });
                        ui.horizontal(|ui| {
                            ui.label("Quality");
                            field_int(ui, &mut p.record.quality, 1..=100, "", "");
                        });
                        help(
                            ui,
                            "Only codecs this build can encode are listed. Quality 1 (small) to 100 (best); \
                             ignored by the lossless PNG sequence. Applies to the next recording.",
                        );
                        ui.horizontal(|ui| {
                            ui.label("Snapshot folder");
                            let mut t = p
                                .general
                                .snapshot_folder
                                .as_ref()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            if field_text(ui, &mut t, Width::Lg, None).changed() {
                                p.general.snapshot_folder =
                                    (!t.trim().is_empty()).then(|| t.trim().into());
                            }
                            if btn(ui, "Choose...", Kind::Secondary).clicked()
                                && let Some(d) = rfd::FileDialog::new().pick_folder()
                            {
                                p.general.snapshot_folder = Some(d);
                            }
                        });
                        help(ui, "Empty = ask on the first snapshot.");
                        ui.horizontal(|ui| {
                            ui.label("HAP output folder");
                            let mut t = p
                                .general
                                .hap_folder
                                .as_ref()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            if field_text(ui, &mut t, Width::Lg, None).changed() {
                                p.general.hap_folder =
                                    (!t.trim().is_empty()).then(|| t.trim().into());
                            }
                            if btn(ui, "Choose...", Kind::Secondary).clicked()
                                && let Some(d) = rfd::FileDialog::new().pick_folder()
                            {
                                p.general.hap_folder = Some(d);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("HAP free-space reserve (GB, 0 = automatic)");
                            let mut gb = p.general.hap_reserve_gb.unwrap_or(0);
                            if field_int(ui, &mut gb, 0..=100_000, "", "").changed() {
                                p.general.hap_reserve_gb = (gb > 0).then_some(gb);
                            }
                        });
                        help(
                            ui,
                            "Convert to HAP writes next to the source unless a folder is set (point it \
                             at an external drive). It refuses, or stops, when free space would fall \
                             below the reserve. Automatic = the larger of 10 GB and 10% of the volume.",
                        );
                    }
                    PrefsTab::Defaults => {
                        help(ui, "Applied to clips and layers created from now on.");
                        egui::ComboBox::from_label("Blend of new layers")
                            .selected_text(blend_name(&p.general.default_blend))
                            .show_ui(ui, |ui| {
                                for b in DEFAULT_BLENDS {
                                    let name = blend_name(&b);
                                    ui.selectable_value(&mut p.general.default_blend, b, name);
                                }
                            });
                        ui.horizontal(|ui| {
                            ui.label("Play mode");
                            for (m, l) in PLAY_MODES {
                                ui.selectable_value(&mut p.defaults.play_mode, m, l);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Direction");
                            for (d, l) in DIRECTIONS {
                                ui.selectable_value(&mut p.defaults.direction, d, l);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Clip start");
                            for (m, l) in TRIGGER_MODES {
                                ui.selectable_value(&mut p.defaults.trigger_mode, m, l);
                            }
                        });
                        help(ui, "Clip start: from the in-point, where it stopped, at the same fraction as the previous clip, or the layer's choice.");
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
                        opt_choice(ui, "Sample rate", &mut p.audio.sample_rate, &[44_100, 48_000, 88_200, 96_000, 192_000], " Hz");
                        opt_choice(ui, "Buffer size", &mut p.audio.buffer_size, &[64, 128, 256, 512, 1024, 2048], " frames");
                        opt_choice(ui, "Output channels", &mut p.audio.channels, &[1, 2, 4, 6, 8], "");
                        ui.separator();
                        let inputs = self
                            .audio_inputs
                            .get_or_insert_with(vjc_media::ltc_in::input_devices);
                        let cur = p.audio.input_device.clone();
                        egui::ComboBox::from_label("Input device (FFT)")
                            .selected_text(cur.as_deref().unwrap_or("(default)"))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut p.audio.input_device, None, "(default)");
                                for n in inputs.iter() {
                                    ui.selectable_value(&mut p.audio.input_device, Some(n.clone()), n);
                                }
                            });
                        ui.horizontal(|ui| {
                            ui.label("Input channel");
                            let mut ch = u32::from(p.audio.input_channel) + 1;
                            field_int(ui, &mut ch, 1..=64, "", "");
                            p.audio.input_channel = (ch.clamp(1, 64) - 1) as u16;
                        });
                        egui::ComboBox::from_label("FFT size")
                            .selected_text(p.audio.fft_size.to_string())
                            .show_ui(ui, |ui| {
                                for n in vjc_io::prefs::FFT_SIZES {
                                    ui.selectable_value(&mut p.audio.fft_size, n, n.to_string());
                                }
                            });
                        if btn(ui, "Refresh devices", Kind::Secondary).clicked() {
                            self.audio_outputs = None;
                            self.audio_inputs = None;
                        }
                        help(
                            ui,
                            "Output changes reopen the audio at once; clips keep playing. A rate, buffer or \
                             channel count the device cannot do falls back to the nearest it supports. \
                             The input feeds audio-reactive animation and spectrum generators; it opens only \
                             while the project uses them.",
                        );
                    }
                    PrefsTab::Video => {
                        ui.label("New composition");
                        ui.horizontal(|ui| {
                            field_int(ui, &mut p.video.width, 1..=16384, "", "");
                            ui.label("x");
                            field_int(ui, &mut p.video.height, 1..=16384, "", "");
                            field_int(ui, &mut p.video.fps, 1.0..=240.0, "", " fps");
                        });
                        egui::ComboBox::from_label("Default clip fit")
                            .selected_text(p.video.fit.label())
                            .show_ui(ui, |ui| {
                                for f in FitMode::ALL {
                                    ui.selectable_value(&mut p.video.fit, f, f.label());
                                }
                            });
                        egui::ComboBox::from_label("Max frame rate")
                            .selected_text(match p.video.max_fps {
                                0 => "Unlimited (display)".to_string(),
                                n => format!("{n} fps"),
                            })
                            .show_ui(ui, |ui| {
                                for n in [30u32, 50, 60, 120, 0] {
                                    let l = if n == 0 {
                                        "Unlimited (display)".to_string()
                                    } else {
                                        format!("{n} fps")
                                    };
                                    ui.selectable_value(&mut p.video.max_fps, n, l);
                                }
                            });
                        toggle(ui, &mut p.video.folder_sequence, "Dropped folder of numbered images becomes one sequence clip");
                        toggle(ui, &mut p.video.vsync, "VSync (applies on next launch)");
                        toggle(ui, &mut p.video.output_10bit, "10-bit output when the display allows it (applies on next launch)");
                    }
                    PrefsTab::Control => {
                        toggle(ui, &mut p.control.default_shortcuts, "New projects get the default shortcut preset");
                        toggle(ui, &mut p.control.offer_shortcuts, "Offer the preset when opening a project without shortcuts");
                        ui.horizontal(|ui| {
                            ui.label("Beat snap");
                            for m in vjc_model::BeatSnap::ALL {
                                ui.selectable_value(&mut p.control.beat_snap, m, m.label());
                            }
                        });
                        help(ui, "Clip triggers and beat loops wait for the next beat or bar.");
                        ui.horizontal(|ui| {
                            ui.label("Tempo nudge range");
                            field_int(ui, &mut p.control.nudge_percent, 1..=50, "", " %");
                        });
                        help(ui, "How much the nudge buttons speed up or slow down the tempo while held.");
                    }
                    PrefsTab::Midi => {
                        let (ins, outs) = self.midi_ports.get_or_insert_with(|| {
                            (
                                crate::midi_io::input_ports(),
                                crate::midi_io::output_ports(),
                            )
                        });
                        section(ui, "Inputs", |ui| {
                            if ins.is_empty() {
                                help(ui, "No MIDI input ports found.");
                            }
                            for n in ins.iter() {
                                let mut on = p.midi.input_enabled(n);
                                if toggle(ui, &mut on, n).changed() {
                                    p.midi.set_input(n, on);
                                }
                            }
                        });
                        section(ui, "Outputs", |ui| {
                            if outs.is_empty() {
                                help(ui, "No MIDI output ports found.");
                            }
                            for n in outs.iter() {
                                let mut on = p.midi.output_enabled(n);
                                if toggle(ui, &mut on, n).changed() {
                                    p.midi.set_output(n, on);
                                }
                            }
                        });
                        if btn(ui, "Refresh ports", Kind::Secondary).clicked() {
                            self.midi_ports = None;
                        }
                        help(ui, "Disabled inputs are ignored; disabled outputs get no LED feedback.");
                        ui.horizontal(|ui| {
                            ui.label("Middle C (note 60) is");
                            for m in [vjc_io::prefs::MiddleC::C3, vjc_io::prefs::MiddleC::C4] {
                                ui.selectable_value(&mut p.midi.middle_c, m, m.label());
                            }
                        });
                        if btn(ui, "MIDI monitor...", Kind::Secondary).clicked() {
                            self.midi_monitor_open = true;
                        }
                    }
                    PrefsTab::Osc => {
                        let o = &mut p.osc;
                        toggle(ui, &mut o.in_enabled, "Receive OSC");
                        ui.add_enabled_ui(o.in_enabled, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Port");
                                field_int(ui, &mut o.in_port, 1..=65535, "", "");
                            });
                            toggle(ui, &mut o.accept_network, "Accept from network (default: this computer only)");
                        });
                        ui.separator();
                        toggle(ui, &mut o.out_enabled, "Send OSC feedback");
                        ui.add_enabled_ui(o.out_enabled, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Host");
                                field_text(ui, &mut o.out_host, Width::Md, None);
                                ui.label("Port");
                                field_int(ui, &mut o.out_port, 1..=65535, "", "");
                            });
                        });
                        help(ui, "Addresses like /comp/layer/1/opacity; learn others in Shortcuts > Edit MIDI.");
                        if btn(ui, "OSC monitor...", Kind::Secondary).clicked() {
                            self.osc_monitor_open = true;
                        }
                    }
                    PrefsTab::Dmx => {
                        let d = &mut p.dmx;
                        toggle(ui, &mut d.enabled, "Receive Art-Net (DMX in)");
                        ui.add_enabled_ui(d.enabled, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Interface IP");
                                field_text(ui, &mut d.interface, Width::Md, None);
                                ui.label("Port");
                                field_int(ui, &mut d.port, 1..=65535, "", "");
                            });
                            ui.horizontal(|ui| {
                                ui.label("First universe");
                                field_int(ui, &mut d.universe, 0..=32767, "", "");
                                ui.label("Count");
                                field_int(ui, &mut d.universes, 1..=64, "", "");
                            });
                        });
                        if d.enabled {
                            match self.dmx.status() {
                                crate::dmx_io::Status::Listening(addr) => {
                                    help(ui, &format!("Listening on {addr}"));
                                }
                                crate::dmx_io::Status::Failed(e) => {
                                    ui.colored_label(ui.visuals().error_fg_color, e);
                                }
                                crate::dmx_io::Status::Off => {
                                    help(ui, "Starting...");
                                }
                            }
                        }
                        help(ui, "127.0.0.1 = this computer only; 0.0.0.0 = every interface (anyone on the network can then send). Channels map to shortcuts in Shortcuts > Edit MIDI (move a fader to learn), value 0..255 = range.");
                        ui.separator();
                        ui.label(format!("Saved fixtures: {}", d.fixtures.len()));
                        let mut gone = None;
                        for (i, f) in d.fixtures.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "{} ({}x{}, {:?})",
                                    f.name, f.width, f.height, f.format
                                ));
                                if btn(ui, "Remove", Kind::Danger).clicked() {
                                    gone = Some(i);
                                }
                            });
                        }
                        if let Some(i) = gone {
                            d.fixtures.remove(i);
                        }
                        help(ui, "Add fixtures from Output > Setup (device Art-Net).");
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
                                    if field_int(ui, &mut c, 1..=64, "", "").changed() {
                                        *channel = (c - 1) as u16;
                                    }
                                });
                                let mut auto = t.ltc_fps.is_none();
                                ui.horizontal(|ui| {
                                    if toggle(ui, &mut auto, "Detect frame rate").changed() {
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
                        if btn(ui, "Refresh devices", Kind::Secondary).clicked() {
                            self.tc_devices = None;
                        }
                        ui.horizontal(|ui| {
                            ui.label("Offset");
                            field_int(ui, &mut t.offset_frames, -1_000_000..=1_000_000, "", " frames");
                        });
                        ui.horizontal(|ui| {
                            ui.label("On signal loss");
                            ui.selectable_value(&mut t.on_loss, OnLoss::Freeze, "Freeze");
                            ui.selectable_value(&mut t.on_loss, OnLoss::FreeRun, "Run free");
                        });
                        help(ui, "Clips with Timecode sync (Clip tab) and cues (Composition tab) follow this source.");
                    }
                    PrefsTab::Api => {
                        let a = &mut p.api;
                        toggle(ui, &mut a.enabled, "Enable Remote API (HTTP + WebSocket)");
                        ui.add_enabled_ui(a.enabled, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Port");
                                field_int(ui, &mut a.port, 1..=65534, "", "");
                                help(ui, &format!("WebSocket: {}", u32::from(a.port) + 1));
                            });
                            toggle(ui, &mut a.expose_network, "Expose to the network (default: this computer only)");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Web remote PIN");
                            ui.add(
                                egui::TextEdit::singleline(&mut a.web_pin)
                                    .password(true)
                                    .desired_width(100.0)
                                    .char_limit(32),
                            );
                            help(ui, "Empty = no PIN. Needed for changes and live updates.");
                        });
                        if a.enabled && a.expose_network && a.web_pin.is_empty() {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Warning: no PIN. Anyone on your network can control VJCraft.",
                            );
                        }
                        if a.enabled {
                            web_remote_links(ui, a);
                        }
                        if a.enabled {
                            match self.api.status() {
                                api_io::Status::Listening(addr) => {
                                    help(ui, &format!("Listening on {addr}"));
                                }
                                api_io::Status::Failed(e) => {
                                    ui.colored_label(ui.visuals().error_fg_color, e);
                                }
                                api_io::Status::Off => {
                                    help(ui, "Starting...");
                                }
                            }
                        }
                        help(ui, "See docs/api.md. Applies immediately.");
                    }
                }
                ui.separator();
                if btn(ui, "Close", Kind::Primary).clicked() {
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

/// Blend modes offered as the default of new layers.
const DEFAULT_BLENDS: [vjc_model::BlendMode; 6] = {
    use vjc_model::BlendMode::{Add, Multiply, Normal, Overlay, Screen, Subtract};
    [Normal, Add, Multiply, Screen, Overlay, Subtract]
};

const PLAY_MODES: [(vjc_model::PlayMode, &str); 5] = {
    use vjc_model::PlayMode::{Bounce, Loop, Once, OnceHold, Random};
    [
        (Loop, "Loop"),
        (Bounce, "Bounce"),
        (Once, "Once"),
        (OnceHold, "Once, hold"),
        (Random, "Random"),
    ]
};

const DIRECTIONS: [(vjc_model::Direction, &str); 3] = {
    use vjc_model::Direction::{Backward, Forward, Random};
    [
        (Forward, "Forward"),
        (Backward, "Backward"),
        (Random, "Random"),
    ]
};

const TRIGGER_MODES: [(vjc_model::TriggerMode, &str); 4] = {
    use vjc_model::TriggerMode::{Continue, Layer, Relative, Restart};
    [
        (Layer, "Layer's"),
        (Restart, "Restart"),
        (Continue, "Continue"),
        (Relative, "Relative"),
    ]
};

fn blend_name(b: &vjc_model::BlendMode) -> String {
    format!("{b:?}")
}

/// URLs of the web remote and a QR code of the first one (with the PIN when set, so a
/// phone camera opens it logged in: keep the screen to yourself).
fn web_remote_links(ui: &mut egui::Ui, a: &vjc_io::prefs::Api) {
    let urls = crate::web_remote::urls(a.port, a.expose_network);
    help(ui, "Web remote, open in a browser:");
    for u in &urls {
        ui.add(egui::Label::new(egui::RichText::new(u).monospace()).selectable(true));
    }
    // Loopback: only the local URL as text. No QR (nothing to scan from a phone), no PIN.
    if !a.expose_network {
        return;
    }
    let mut target = urls[0].clone();
    if !a.web_pin.is_empty() {
        let pin: String = a
            .web_pin
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        target = format!("{target}?pin={pin}");
    }
    let Some(qr) = crate::qr::encode(target.as_bytes()) else {
        return;
    };
    let px = 4.0;
    let quiet = 2usize;
    let side = (qr.size + 2 * quiet) as f32 * px;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, egui::Color32::WHITE);
    for y in 0..qr.size {
        for x in 0..qr.size {
            if qr.get(x, y) {
                let min = rect.min + egui::vec2((x + quiet) as f32, (y + quiet) as f32) * px;
                p.rect_filled(
                    egui::Rect::from_min_size(min, egui::vec2(px, px)),
                    0.0,
                    egui::Color32::BLACK,
                );
            }
        }
    }
}

/// "(default)" plus fixed choices for an optional number.
fn opt_choice<T: Copy + PartialEq + std::fmt::Display>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<T>,
    choices: &[T],
    unit: &str,
) {
    let text = value.map_or("(default)".to_owned(), |v| format!("{v}{unit}"));
    egui::ComboBox::from_label(label)
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "(default)");
            for c in choices {
                ui.selectable_value(value, Some(*c), format!("{c}{unit}"));
            }
        });
}
