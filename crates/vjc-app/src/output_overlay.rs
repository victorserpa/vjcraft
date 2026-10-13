//! Overlays drawn over a screen's output window: test card, identify number, FPS.
//! The geometry is in canvas pixels (the screen's own resolution) so it can be checked
//! without a window; `draw` maps it onto the rect where the canvas is presented.

use crate::theme;
use eframe::egui::{self, Color32, Pos2, Rect, pos2};
use std::time::{Duration, Instant};
use vjc_model::Screen;

/// How long "Identify displays" stays on.
pub const IDENTIFY_FOR: Duration = Duration::from_secs(3);

/// Test card primitive in canvas pixels: `[x0, y0, x1, y1]` filled rectangle.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Fill([f32; 4], Color32),
    /// Closed outline, 1 px wide.
    Outline(Vec<(f32, f32)>, Color32),
}

/// Grid every 1/8 of the canvas, color bars, slice outlines and a 1 px border exactly on
/// the canvas limits (drawn last, so it is never covered).
pub fn test_card(screen: &Screen) -> Vec<Item> {
    let (w, h) = (screen.width.max(1) as f32, screen.height.max(1) as f32);
    let mut v = vec![];
    for i in 1..8 {
        let (x, y) = ((w * i as f32 / 8.0).round(), (h * i as f32 / 8.0).round());
        v.push(Item::Fill([x, 0.0, x + 1.0, h], theme::TESTCARD_GRID));
        v.push(Item::Fill([0.0, y, w, y + 1.0], theme::TESTCARD_GRID));
    }
    let (top, bot) = ((h * 3.0 / 8.0).round(), (h * 4.0 / 8.0).round());
    for (i, c) in theme::TESTCARD_BARS.iter().enumerate() {
        let (x0, x1) = (w * (i + 1) as f32 / 10.0, w * (i + 2) as f32 / 10.0);
        v.push(Item::Fill([x0.round(), top, x1.round(), bot], *c));
    }
    for s in &screen.slices {
        let cols = s.mesh_cols as usize;
        let rows = vjc_core::mapping::rows_of(s.output_mesh.len(), s.mesh_cols) as usize;
        if cols < 2 || rows < 2 || s.output_mesh.len() != cols * rows {
            continue;
        }
        let m = &s.output_mesh;
        let mut ring: Vec<(f32, f32)> = (0..cols).map(|i| m[i]).collect();
        ring.extend((1..rows).map(|j| m[j * cols + cols - 1]));
        ring.extend((0..cols - 1).rev().map(|i| m[(rows - 1) * cols + i]));
        ring.extend((1..rows - 1).rev().map(|j| m[j * cols]));
        v.push(Item::Outline(ring, theme::TESTCARD_SLICE));
    }
    let c = theme::TESTCARD_BORDER;
    v.push(Item::Fill([0.0, 0.0, w, 1.0], c));
    v.push(Item::Fill([0.0, h - 1.0, w, h], c));
    v.push(Item::Fill([0.0, 0.0, 1.0, h], c));
    v.push(Item::Fill([w - 1.0, 0.0, w, h], c));
    v
}

/// Center of a slice's output mesh, in canvas pixels.
pub fn slice_center(s: &vjc_model::Slice) -> Option<(f32, f32)> {
    let n = s.output_mesh.len();
    (n > 0).then(|| {
        let (x, y) = s
            .output_mesh
            .iter()
            .fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
        (x / n as f32, y / n as f32)
    })
}

/// What to draw over one screen.
pub struct Overlay<'a> {
    pub screen: &'a Screen,
    pub test_card: bool,
    /// Big number (1-based screen number) while identifying.
    pub identify: Option<usize>,
    pub fps: Option<f32>,
}

/// Draws the overlays on `painter`; `rect` is where the canvas is presented.
pub fn draw(painter: &egui::Painter, rect: Rect, o: &Overlay) {
    let s = o.screen;
    let (w, h) = (s.width.max(1) as f32, s.height.max(1) as f32);
    let (kx, ky) = (rect.width() / w, rect.height() / h);
    let at = |x: f32, y: f32| pos2(rect.min.x + x * kx, rect.min.y + y * ky);
    let label = |p: Pos2, align: egui::Align2, text: String, size: f32, color: Color32| {
        let font = egui::FontId::proportional(size);
        let g = painter.layout_no_wrap(text, font, color);
        let r = align.anchor_size(p, g.size()).expand(size * 0.2);
        painter.rect_filled(r, 2.0, theme::TESTCARD_TEXT_BG);
        painter.galley(r.center() - g.size() / 2.0, g, color);
    };
    let unit = (rect.height() / 40.0).clamp(10.0, 48.0);
    if o.test_card {
        for it in test_card(s) {
            match it {
                Item::Fill([x0, y0, x1, y1], c) => {
                    painter.rect_filled(Rect::from_min_max(at(x0, y0), at(x1, y1)), 0.0, c);
                }
                Item::Outline(pts, c) => {
                    let pts: Vec<Pos2> = pts.iter().map(|p| at(p.0, p.1)).collect();
                    painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.0, c)));
                }
            }
        }
        for (i, sl) in s.slices.iter().enumerate() {
            if let Some((cx, cy)) = slice_center(sl) {
                label(
                    at(cx, cy),
                    egui::Align2::CENTER_CENTER,
                    format!("Slice {}", i + 1),
                    unit * 1.5,
                    theme::TESTCARD_SLICE,
                );
            }
        }
        label(
            at(w / 2.0, h * 3.0 / 16.0),
            egui::Align2::CENTER_CENTER,
            format!("{} - {} x {} px", s.name, s.width, s.height),
            unit * 2.0,
            theme::TESTCARD_TEXT,
        );
    }
    if let Some(n) = o.identify {
        let size = (rect.height() * 0.5).max(24.0);
        painter.rect_filled(rect, 0.0, theme::IDENTIFY_BG);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            n.to_string(),
            egui::FontId::proportional(size),
            theme::IDENTIFY_TEXT,
        );
    }
    if let Some(f) = o.fps {
        label(
            rect.min + egui::vec2(unit, unit),
            egui::Align2::LEFT_TOP,
            format!("{f:.0} FPS"),
            unit * 1.5,
            theme::FPS_TEXT,
        );
    }
}

/// Frames per second of one output, measured over half-second windows.
#[derive(Debug, Default)]
pub struct FpsMeter {
    since: Option<Instant>,
    frames: u32,
    pub fps: f32,
}

impl FpsMeter {
    /// Call once per presented frame.
    pub fn tick(&mut self, now: Instant) {
        let start = *self.since.get_or_insert(now);
        self.frames += 1;
        let dt = now.duration_since(start).as_secs_f32();
        if dt >= 0.5 {
            self.fps = self.frames as f32 / dt;
            self.frames = 0;
            self.since = Some(now);
        }
    }
}

/// True while an identify started at `t0` is still showing.
pub fn identifying(t0: Option<Instant>, now: Instant) -> bool {
    t0.is_some_and(|t| now.duration_since(t) < IDENTIFY_FOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_core::mapping;

    fn screen() -> Screen {
        let mut s = mapping::new_screen("Wall", 1920, 1080);
        s.slices.push(mapping::new_slice(1920, 1080));
        s
    }

    #[test]
    fn border_is_one_pixel_exactly_on_canvas_limits() {
        let items = test_card(&screen());
        let b = theme::TESTCARD_BORDER;
        for r in [
            [0.0, 0.0, 1920.0, 1.0],
            [0.0, 1079.0, 1920.0, 1080.0],
            [0.0, 0.0, 1.0, 1080.0],
            [1919.0, 0.0, 1920.0, 1080.0],
        ] {
            assert!(items.contains(&Item::Fill(r, b)), "{r:?}");
        }
        // Drawn last so nothing covers it.
        assert!(matches!(items.last(), Some(Item::Fill(_, c)) if *c == b));
    }

    #[test]
    fn grid_every_eighth_and_eight_bars() {
        let items = test_card(&screen());
        let g = theme::TESTCARD_GRID;
        assert!(items.contains(&Item::Fill([240.0, 0.0, 241.0, 1080.0], g)));
        assert!(items.contains(&Item::Fill([0.0, 540.0, 1920.0, 541.0], g)));
        let bars = items
            .iter()
            .filter(|i| matches!(i, Item::Fill(_, c) if theme::TESTCARD_BARS.contains(c) && *c != theme::TESTCARD_BORDER))
            .count();
        assert!(bars >= 7, "{bars}");
        assert!(items.contains(&Item::Fill(
            [192.0, 405.0, 384.0, 540.0],
            theme::TESTCARD_BARS[0]
        )));
    }

    #[test]
    fn slice_outline_follows_output_mesh() {
        let items = test_card(&screen());
        let ring = items
            .iter()
            .find_map(|i| match i {
                Item::Outline(p, _) => Some(p.clone()),
                _ => None,
            })
            .expect("outline");
        assert_eq!(ring.len(), 4);
        assert!(ring.contains(&(1920.0, 1080.0)) && ring.contains(&(0.0, 0.0)));
        assert_eq!(slice_center(&screen().slices[0]), Some((960.0, 540.0)));
    }

    #[test]
    fn fps_meter_and_identify_window() {
        let t0 = Instant::now();
        let mut m = FpsMeter::default();
        for i in 0..=30 {
            m.tick(t0 + Duration::from_millis(i * 20));
        }
        assert!((m.fps - 50.0).abs() < 3.0, "{}", m.fps);
        assert!(identifying(Some(t0), t0 + Duration::from_millis(2900)));
        assert!(!identifying(Some(t0), t0 + Duration::from_millis(3000)));
        assert!(!identifying(None, t0));
    }
}
