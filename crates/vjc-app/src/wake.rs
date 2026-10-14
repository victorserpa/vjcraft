//! Wakes the UI thread from I/O threads (MIDI, OSC, DMX, timecode) so an incoming
//! message is applied on the next frame even while the app idles between repaints.

use eframe::egui;
use std::sync::OnceLock;

static CTX: OnceLock<egui::Context> = OnceLock::new();

/// Called once at startup with the app's context.
pub fn install(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Asks for a repaint now; a no-op before `install` (tests, headless).
pub fn now() {
    if let Some(c) = CTX.get() {
        c.request_repaint();
    }
}
