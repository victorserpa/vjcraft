//! MIDI and OSC monitor windows: the last incoming messages, read-only.

use crate::widgets::{help, panel_window};
use eframe::egui;
use std::collections::VecDeque;
use vjc_io::prefs::MiddleC;

/// Messages kept per monitor.
pub const MAX_LINES: usize = 64;

/// The last `MAX_LINES` messages, newest last.
#[derive(Debug, Default, Clone)]
pub struct Log {
    lines: VecDeque<String>,
}

impl Log {
    pub fn push(&mut self, line: String) {
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn lines(&self) -> impl Iterator<Item = &String> {
        self.lines.iter()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// One line for a raw MIDI message: device, channel (1-based), kind and data.
pub fn midi_line(device: &str, bytes: &[u8], middle_c: MiddleC) -> String {
    let hex = || {
        bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    match vjc_core::midi::parse(bytes) {
        Some(vjc_core::midi::Msg::NoteOn { channel, note }) => format!(
            "{device}  ch {}  Note on {} ({note})  vel {}",
            channel + 1,
            middle_c.note_name(note),
            bytes.get(2).copied().unwrap_or(0)
        ),
        Some(vjc_core::midi::Msg::NoteOff { channel, note }) => format!(
            "{device}  ch {}  Note off {} ({note})",
            channel + 1,
            middle_c.note_name(note)
        ),
        Some(vjc_core::midi::Msg::Cc { channel, cc, value }) => {
            format!("{device}  ch {}  CC {cc}  value {value}", channel + 1)
        }
        None => format!("{device}  {}", hex()),
    }
}

/// One line for an incoming OSC message.
pub fn osc_line(e: &vjc_core::osc::Event) -> String {
    match e.value {
        Some(v) => format!("{}  {v:.3}", e.address),
        None => e.address.clone(),
    }
}

/// Read-only list window; returns false once the user closed it.
pub fn window(ctx: &egui::Context, title: &str, log: &mut Log, hint: &str) -> bool {
    let mut open = true;
    panel_window(ctx, title)
        .open(&mut open)
        .default_width(420.0)
        .default_height(260.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                help(ui, hint);
                if crate::widgets::btn(ui, "Clear", crate::widgets::Kind::Secondary).clicked() {
                    log.clear();
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if log.len() == 0 {
                        help(ui, "Nothing received yet.");
                    }
                    for l in log.lines() {
                        ui.label(
                            egui::RichText::new(l)
                                .monospace()
                                .size(crate::theme::FS_XS)
                                .color(crate::theme::TEXT),
                        );
                    }
                });
        });
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_keeps_only_the_last_lines() {
        let mut l = Log::default();
        for i in 0..MAX_LINES + 5 {
            l.push(i.to_string());
        }
        assert_eq!(l.len(), MAX_LINES);
        assert_eq!(l.lines().next().unwrap(), "5");
        assert_eq!(l.lines().last().unwrap(), &(MAX_LINES + 4).to_string());
    }

    #[test]
    fn midi_lines_name_notes_by_the_middle_c_option() {
        let on = [0x90, 60, 100];
        assert!(midi_line("Pad", &on, MiddleC::C3).contains("Note on C3 (60)"));
        assert!(midi_line("Pad", &on, MiddleC::C4).contains("Note on C4 (60)"));
        assert!(midi_line("Pad", &[0xB1, 7, 99], MiddleC::C3).contains("ch 2  CC 7  value 99"));
        // Unparsed data (clock, sysex) shows as hex.
        assert!(midi_line("Pad", &[0xF8], MiddleC::C3).ends_with("F8"));
    }

    #[test]
    fn osc_lines_show_address_and_value() {
        let e = vjc_core::osc::Event {
            address: "/comp/master/opacity".into(),
            value: Some(0.5),
        };
        assert_eq!(osc_line(&e), "/comp/master/opacity  0.500");
    }
}
