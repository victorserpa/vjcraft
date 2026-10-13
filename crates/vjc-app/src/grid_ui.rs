//! Painted widgets of the clip grid (cells, headers). Look comes from `theme`.

use crate::theme;
use eframe::egui::{self, Align2, FontId, Sense, StrokeKind, TextureId};

/// What fills the picture area of a cell.
pub enum Tile<'a> {
    Empty,
    Thumb(TextureId),
    /// Generator or source without a frame: colored tile with the kind's name.
    Color(&'a str),
    /// Thumbnail still decoding.
    Pending,
}

/// A clip cell: picture on top, name strip below. `label` is the accessible name.
pub fn cell(
    ui: &mut egui::Ui,
    label: &str,
    name: &str,
    tile: Tile,
    playing: bool,
) -> egui::Response {
    let size = egui::vec2(theme::CELL_W, theme::THUMB_H + theme::NAME_H);
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let p = ui.painter();
    let pic = egui::Rect::from_min_size(rect.min, egui::vec2(size.x, theme::THUMB_H));
    let strip = egui::Rect::from_min_max(egui::pos2(rect.left(), pic.bottom()), rect.max);
    let round = theme::CELL_ROUNDING;
    p.rect_filled(rect, round, theme::CELL_EMPTY);
    match tile {
        Tile::Empty => {}
        Tile::Pending => {
            p.rect_filled(pic, round, theme::CELL_PENDING);
        }
        Tile::Thumb(id) => {
            p.rect_filled(pic, round, theme::MONITOR_BG);
            // Letterbox to keep the aspect: thumbnails are at most THUMB_MAX_SIDE long.
            p.image(
                id,
                pic,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        Tile::Color(key) => {
            p.rect_filled(pic, round, theme::tile_color(key));
            p.text(
                pic.center(),
                Align2::CENTER_CENTER,
                key,
                FontId::proportional(12.0),
                theme::CELL_NAME_TEXT,
            );
        }
    }
    if !matches!(tile, Tile::Empty) {
        p.rect_filled(strip, round, theme::CELL_NAME_BG);
        let clipped = ui.painter().with_clip_rect(strip);
        clipped.text(
            strip.left_center() + egui::vec2(4.0, 0.0),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(11.0),
            theme::CELL_NAME_TEXT,
        );
    }
    if playing {
        ui.painter().rect_stroke(
            rect,
            round,
            egui::Stroke::new(2.0, theme::CELL_PLAYING),
            StrokeKind::Inside,
        );
    } else if r.hovered() {
        ui.painter().rect_stroke(
            rect,
            round,
            ui.visuals().widgets.hovered.bg_stroke,
            StrokeKind::Inside,
        );
    }
    r
}
