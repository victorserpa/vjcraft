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
    /// While paused, incoming messages are counted but not kept.
    paused: bool,
    missed: usize,
    /// Sequence number of the line the user clicked (what "Copy" puts on the clipboard).
    /// By number, not text: repeated messages are equal lines, and an evicted line
    /// takes the selection with it.
    selected: Option<u64>,
    /// Sequence number of `lines[0]`.
    first: u64,
}

impl Log {
    pub fn push(&mut self, line: String) {
        if self.paused {
            self.missed += 1;
            return;
        }
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
            self.first += 1;
            if self.selected.is_some_and(|s| s < self.first) {
                self.selected = None;
            }
        }
        self.lines.push_back(line);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn clear(&mut self) {
        self.first += self.lines.len() as u64;
        self.lines.clear();
        self.selected = None;
        self.missed = 0;
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, on: bool) {
        self.paused = on;
        if !on {
            self.missed = 0;
        }
    }

    pub fn missed(&self) -> usize {
        self.missed
    }

    /// Lines with their sequence number.
    pub fn numbered(&self) -> impl Iterator<Item = (u64, &String)> {
        (self.first..).zip(self.lines.iter())
    }

    pub fn selected(&self) -> Option<u64> {
        self.selected
    }

    /// Selects the line with sequence number `seq` (None = no selection).
    pub fn select(&mut self, seq: Option<u64>) {
        self.selected =
            seq.filter(|s| (self.first..self.first + self.lines.len() as u64).contains(s));
    }

    /// What Copy puts on the clipboard: the selected line, else every line.
    pub fn copy_text(&self) -> String {
        match self
            .selected
            .and_then(|s| self.numbered().find(|(n, _)| *n == s))
        {
            Some((_, l)) => l.clone(),
            None => self.lines.iter().cloned().collect::<Vec<_>>().join("\n"),
        }
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
                let label = if log.paused() { "Resume" } else { "Pause" };
                if crate::widgets::btn(ui, label, crate::widgets::Kind::Secondary).clicked() {
                    let p = !log.paused();
                    log.set_paused(p);
                }
                if crate::widgets::btn_if(
                    ui,
                    log.len() > 0,
                    "Copy",
                    crate::widgets::Kind::Secondary,
                )
                .on_hover_text("Copy the selected message, or all when none is selected")
                .clicked()
                {
                    ui.ctx().copy_text(log.copy_text());
                }
                if crate::widgets::btn(ui, "Clear", crate::widgets::Kind::Secondary).clicked() {
                    log.clear();
                }
                if log.paused() {
                    help(ui, &format!("Paused, {} missed", log.missed()));
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
                    let mut pick = None;
                    for (n, l) in log.numbered() {
                        let on = log.selected() == Some(n);
                        let t = egui::RichText::new(l)
                            .monospace()
                            .size(crate::theme::FS_XS)
                            .color(crate::theme::TEXT);
                        if ui.selectable_label(on, t).clicked() {
                            pick = Some(if on { None } else { Some(n) });
                        }
                    }
                    if let Some(p) = pick {
                        log.select(p);
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
        assert_eq!(l.numbered().next().unwrap().1, "5");
        assert_eq!(l.numbered().last().unwrap().1, &(MAX_LINES + 4).to_string());
    }

    #[test]
    fn paused_log_ignores_messages_and_resume_keeps_the_old_ones() {
        let mut l = Log::default();
        l.push("a".into());
        l.set_paused(true);
        l.push("b".into());
        assert_eq!((l.len(), l.missed()), (1, 1));
        l.set_paused(false);
        l.push("c".into());
        assert_eq!(l.copy_text(), "a\nc");
    }

    #[test]
    fn negative_control_unpaused_log_keeps_everything() {
        let mut l = Log::default();
        l.push("a".into());
        l.push("b".into());
        assert_eq!((l.len(), l.missed()), (2, 0));
    }

    #[test]
    fn copy_text_is_the_selection_or_everything() {
        let mut l = Log::default();
        l.push("a".into());
        l.push("b".into());
        assert_eq!(l.copy_text(), "a\nb");
        l.select(Some(1));
        assert_eq!(l.copy_text(), "b");
        l.clear();
        assert_eq!(l.copy_text(), "");
    }

    #[test]
    fn selection_is_by_line_and_clears_when_evicted() {
        let mut l = Log::default();
        l.push("same".into());
        l.push("same".into());
        l.select(Some(1));
        assert_eq!(l.selected(), Some(1), "the second of two equal lines");
        for i in 0..MAX_LINES - 1 {
            l.push(i.to_string());
        }
        assert_eq!(l.selected(), Some(1), "still in the buffer");
        l.push("x".into());
        l.push("y".into());
        assert_eq!(l.selected(), None, "evicted: nothing selected");
        assert!(
            l.copy_text().contains('\n'),
            "Copy falls back to everything"
        );
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
