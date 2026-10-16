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

/// Hash of everything `test_card` depends on besides the size: canvas size and slice
/// outline meshes. The card has no animated part, so this is the whole cache key.
pub fn card_key(screen: &Screen) -> u64 {
    use std::hash::{Hash, Hasher};
    // The whole screen (every field `test_card` might read): any edit invalidates the card.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(screen)
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

#[cfg(test)]
thread_local! {
    /// Calls to `rasterize` on this thread (tests assert the cache avoids them).
    pub static RASTERIZE_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Rasterizes test card items into straight-alpha RGBA (`w` x `h`, transparent where
/// nothing is drawn). Later items win. Twin of what `draw` paints over the output window,
/// minus the text, so the card can also be stamped on the GPU screen target.
pub fn rasterize(items: &[Item], w: u32, h: u32) -> Vec<u8> {
    #[cfg(test)]
    RASTERIZE_CALLS.with(|c| c.set(c.get() + 1));
    let mut img = vec![0u8; (w as usize) * (h as usize) * 4];
    let mut put = |x: i64, y: i64, c: Color32| {
        if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
            let i = ((y as usize) * (w as usize) + x as usize) * 4;
            img[i..i + 4].copy_from_slice(&c.to_srgba_unmultiplied());
        }
    };
    for it in items {
        match it {
            Item::Fill([x0, y0, x1, y1], c) => {
                for y in (y0.round() as i64)..(y1.round() as i64) {
                    for x in (x0.round() as i64)..(x1.round() as i64) {
                        put(x, y, *c);
                    }
                }
            }
            Item::Outline(pts, c) => {
                for i in 0..pts.len() {
                    let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                    let n = (b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil().max(1.0) as i64;
                    for k in 0..=n {
                        let t = k as f32 / n as f32;
                        put(
                            (a.0 + (b.0 - a.0) * t).floor() as i64,
                            (a.1 + (b.1 - a.1) * t).floor() as i64,
                            *c,
                        );
                    }
                }
            }
        }
    }
    img
}

/// Smallest tile the pixel-map grid draws (finer would be thousands of lines).
pub const MIN_TILE: u32 = 4;

/// Pixel-map grid: a 1 px line on every multiple of the panel tile size (`tile` in canvas
/// pixels, clamped to `MIN_TILE`), never on the canvas edges. Lines of the tile column /
/// row that is cut by the canvas edge are the last ones drawn, so a partial panel shows.
pub fn pixel_grid(screen: &Screen, tile: (u32, u32)) -> Vec<Item> {
    let (w, h) = (screen.width.max(1), screen.height.max(1));
    let (tw, th) = (tile.0.max(MIN_TILE), tile.1.max(MIN_TILE));
    let mut v = vec![];
    for x in (tw..w).step_by(tw as usize) {
        v.push(Item::Fill(
            [x as f32, 0.0, x as f32 + 1.0, h as f32],
            theme::PIXEL_GRID,
        ));
    }
    for y in (th..h).step_by(th as usize) {
        v.push(Item::Fill(
            [0.0, y as f32, w as f32, y as f32 + 1.0],
            theme::PIXEL_GRID,
        ));
    }
    v
}

/// Ids after the global black-out key: any screen already black -> all back on; otherwise
/// every screen goes black.
pub fn toggle_blackout(
    current: &std::collections::HashSet<uuid::Uuid>,
    all: &[uuid::Uuid],
) -> std::collections::HashSet<uuid::Uuid> {
    if all.iter().any(|id| current.contains(id)) {
        Default::default()
    } else {
        all.iter().copied().collect()
    }
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
    /// Whole canvas black (global black-out).
    pub blackout: bool,
    /// Pixel-map grid tile size in canvas pixels.
    pub grid: Option<(u32, u32)>,
    /// Big number (1-based screen number) while identifying.
    pub identify: Option<usize>,
    /// Slice numbers drawn at the slice centres (Output setup: Identify slices).
    pub slice_ids: bool,
    pub fps: Option<f32>,
}

type GalleyKey = (String, u32, u32, Color32);

thread_local! {
    /// Laid-out labels, so the test card does not re-shape its text every frame.
    static GALLEYS: std::cell::RefCell<std::collections::HashMap<GalleyKey, std::sync::Arc<egui::Galley>>> =
        Default::default();
}

fn cached_galley(
    painter: &egui::Painter,
    text: String,
    size: f32,
    color: Color32,
) -> std::sync::Arc<egui::Galley> {
    let key = (
        text,
        size.to_bits(),
        painter.ctx().pixels_per_point().to_bits(),
        color,
    );
    GALLEYS.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(g) = m.get(&key) {
            return g.clone();
        }
        if m.len() > 64 {
            m.clear();
        }
        let g = painter.layout_no_wrap(key.0.clone(), egui::FontId::proportional(size), color);
        m.insert(key, g.clone());
        g
    })
}

/// Draws the overlays on `painter`; `rect` is where the canvas is presented.
pub fn draw(painter: &egui::Painter, rect: Rect, o: &Overlay) {
    let s = o.screen;
    let (w, h) = (s.width.max(1) as f32, s.height.max(1) as f32);
    let (kx, ky) = (rect.width() / w, rect.height() / h);
    let at = |x: f32, y: f32| pos2(rect.min.x + x * kx, rect.min.y + y * ky);
    let label = |p: Pos2, align: egui::Align2, text: String, size: f32, color: Color32| {
        let g = cached_galley(painter, text, size, color);
        let r = align.anchor_size(p, g.size()).expand(size * 0.2);
        painter.rect_filled(r, 2.0, theme::TESTCARD_TEXT_BG);
        painter.galley(r.center() - g.size() / 2.0, g, color);
    };
    let unit = (rect.height() / 40.0).clamp(10.0, 48.0);
    let paint = |items: Vec<Item>| {
        for it in items {
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
    };
    if o.blackout {
        // Black-out wins over every overlay: nothing but black reaches the wall.
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        return;
    }
    if let Some(tile) = o.grid {
        paint(pixel_grid(s, tile));
    }
    if o.test_card {
        paint(test_card(s));
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
    if o.slice_ids {
        for (i, sl) in s.slices.iter().enumerate() {
            if let Some((cx, cy)) = slice_center(sl) {
                label(
                    at(cx, cy),
                    egui::Align2::CENTER_CENTER,
                    (i + 1).to_string(),
                    unit * 3.0,
                    theme::IDENTIFY_TEXT,
                );
            }
        }
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

    /// Shapes `draw` paints for `o`, through a real egui painter.
    fn shapes(o: &Overlay) -> usize {
        let ctx = egui::Context::default();
        let mut n = 0;
        // A first empty pass loads the fonts the text overlays need.
        ctx.begin_pass(Default::default());
        ctx.end_pass().textures_delta.clear();
        ctx.begin_pass(Default::default());
        let p = ctx.layer_painter(egui::LayerId::background());
        draw(
            &p,
            Rect::from_min_size(Pos2::ZERO, egui::vec2(960.0, 540.0)),
            o,
        );
        let mut out = ctx.end_pass();
        out.textures_delta.clear();
        n += out.shapes.len();
        n
    }

    #[test]
    fn black_out_hides_grid_test_card_and_identify() {
        let s = screen();
        let mut o = Overlay {
            screen: &s,
            test_card: true,
            blackout: true,
            grid: Some((128, 128)),
            identify: Some(1),
            slice_ids: false,
            fps: Some(60.0),
        };
        assert_eq!(shapes(&o), 1, "only the black fill");
        // Negative control: without black-out the overlays do paint.
        o.blackout = false;
        assert!(shapes(&o) > 1);
    }

    #[test]
    fn pixel_grid_lines_sit_on_tile_multiples_and_skip_the_edges() {
        let mut s = screen();
        s.width = 384;
        s.height = 256;
        let items = pixel_grid(&s, (128, 128));
        // x = 128, 256 ; y = 128 : three lines, none on 0 or the canvas limit.
        assert_eq!(items.len(), 3);
        assert!(items.contains(&Item::Fill([128.0, 0.0, 129.0, 256.0], theme::PIXEL_GRID)));
        assert!(items.contains(&Item::Fill([0.0, 128.0, 384.0, 129.0], theme::PIXEL_GRID)));
        // Odd LED size: the last partial tile gets no line past the edge.
        s.width = 300;
        let xs = pixel_grid(&s, (128, 128)).len();
        assert_eq!(xs, 2 + 1);
        // Negative control: a tile equal to the canvas draws nothing; tiny tiles are clamped.
        assert!(pixel_grid(&s, (300, 256)).is_empty());
        assert_eq!(
            pixel_grid(&s, (0, 0)).len(),
            pixel_grid(&s, (MIN_TILE, MIN_TILE)).len()
        );
    }

    #[test]
    fn blackout_toggle_blacks_all_then_restores_all() {
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let none = std::collections::HashSet::new();
        let on = toggle_blackout(&none, &[a, b]);
        assert!(on.contains(&a) && on.contains(&b));
        // Negative control: with one screen already black the key restores, not extends.
        let one = std::collections::HashSet::from([a]);
        assert!(toggle_blackout(&one, &[a, b]).is_empty());
        assert!(toggle_blackout(&none, &[]).is_empty());
    }

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
