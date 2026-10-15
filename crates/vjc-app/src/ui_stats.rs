//! `VJC_UI_STATS=1`: once per second prints UI frames/s, what asked for the repaint
//! (egui's own `file:line` tags) and the font atlas size (a growing or recreated atlas
//! throws the galley cache away and re-shapes all text).

use eframe::egui;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct UiStats {
    on: bool,
    since: Instant,
    frames: u32,
    causes: HashMap<String, u32>,
}

impl UiStats {
    pub fn new() -> Self {
        Self {
            on: std::env::var_os("VJC_UI_STATS").is_some(),
            since: Instant::now(),
            frames: 0,
            causes: HashMap::new(),
        }
    }

    /// Call once per UI frame.
    pub fn frame(&mut self, ctx: &egui::Context) {
        if !self.on {
            return;
        }
        self.frames += 1;
        for c in ctx.repaint_causes() {
            *self
                .causes
                .entry(format!("{}:{} {}", c.file, c.line, c.reason))
                .or_default() += 1;
        }
        if self.since.elapsed() < Duration::from_secs(1) {
            return;
        }
        let atlas = ctx.fonts_mut(|f| f.font_image_size());
        let mut top: Vec<_> = self.causes.drain().collect();
        top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        eprintln!(
            "ui-stats: {} frames/s, font atlas {}x{}",
            self.frames, atlas[0], atlas[1]
        );
        for (c, n) in top.iter().take(6) {
            eprintln!("  {n:>4}x {c}");
        }
        self.frames = 0;
        self.since = Instant::now();
    }
}

/// Output-window passes per second and what asked for them (`VJC_UI_STATS=1`).
pub fn child_pass(ctx: &egui::Context) {
    use std::sync::Mutex;
    type St = Option<(Instant, u32, HashMap<String, u32>)>;
    static ST: Mutex<St> = Mutex::new(None);
    if std::env::var_os("VJC_UI_STATS").is_none() {
        return;
    }
    let mut g = ST.lock().unwrap();
    let st = g.get_or_insert_with(|| (Instant::now(), 0, HashMap::new()));
    st.1 += 1;
    for c in ctx.repaint_causes() {
        *st.2
            .entry(format!("{}:{} {}", c.file, c.line, c.reason))
            .or_default() += 1;
    }
    if st.0.elapsed() >= Duration::from_secs(1) {
        eprintln!("ui-stats: output window {} passes/s", st.1);
        for (c, n) in st.2.drain() {
            eprintln!("  out {n:>4}x {c}");
        }
        *st = (Instant::now(), 0, HashMap::new());
    }
}
