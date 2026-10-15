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

/// Workers wake the UI through `after`: a burst of results (thumbnails, probes) collapses
/// into one repaint instead of one per result.
pub const WORKER_REPAINT: std::time::Duration = std::time::Duration::from_millis(33);

/// Asks for a repaint within `WORKER_REPAINT`; egui keeps only the earliest pending request.
pub fn after(ctx: &egui::Context) {
    ctx.request_repaint_after(WORKER_REPAINT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_burst_is_one_delayed_repaint() {
        let ctx = egui::Context::default();
        for _ in 0..3 {
            ctx.run_ui(Default::default(), |_| {})
                .textures_delta
                .clear();
        }
        let mut out = ctx.run_ui(Default::default(), |ui| {
            for _ in 0..100 {
                after(ui.ctx());
            }
        });
        out.textures_delta.clear();
        let d = out.viewport_output.values().next().unwrap().repaint_delay;
        assert!(d >= std::time::Duration::from_millis(1), "got {d:?}");
        assert!(d <= WORKER_REPAINT, "got {d:?}");
    }
}
