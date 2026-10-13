//! Colors and sizes of the work layout. Components take their look from here, never from
//! literals, so the whole look can be retuned in one place.

use eframe::egui::{Color32, Vec2, vec2};

/// Clip cell: thumbnail area plus a name strip below it.
pub const CELL_W: f32 = 140.0;
pub const THUMB_H: f32 = 79.0;
pub const NAME_H: f32 = 16.0;
pub const CELL_ROUNDING: f32 = 3.0;
pub const THUMB_MAX_SIDE: u32 = 160;

pub const CELL_EMPTY: Color32 = Color32::from_rgb(24, 26, 30);
pub const CELL_NAME_BG: Color32 = Color32::from_rgb(38, 41, 48);
pub const CELL_NAME_TEXT: Color32 = Color32::from_rgb(214, 218, 226);
pub const CELL_PLAYING: Color32 = Color32::from_rgb(70, 190, 120);
pub const CELL_SELECTED: Color32 = Color32::from_rgb(110, 170, 240);
pub const CELL_DROP: Color32 = Color32::from_rgb(240, 200, 60);
pub const CELL_PENDING: Color32 = Color32::from_rgb(44, 48, 56);

/// Layer header block (left of the cells).
pub const LAYER_HEAD_W: f32 = 170.0;
pub const LAYER_FADER_W: f32 = 44.0;
pub const LAYER_BTN: f32 = 20.0;
pub const LAYER_ON: Color32 = Color32::from_rgb(230, 170, 50);
pub const LAYER_SOLO_ON: Color32 = Color32::from_rgb(90, 170, 240);

/// Column header trigger.
pub const COLUMN_HEAD_H: f32 = 22.0;

/// Bottom area.
pub const MONITOR_COL_W: f32 = 240.0;
pub const MONITOR_MIN_W: f32 = 160.0;
pub const MONITOR_BG: Color32 = Color32::BLACK;

/// Property sections.
pub const PROP_VALUE_W: f32 = 56.0;
pub const SECTION_HEAD: Color32 = Color32::from_rgb(150, 160, 178);

/// Effect chain rows.
pub const FX_SLIDER_W: f32 = 120.0;
pub const FX_BTN: Vec2 = vec2(22.0, 20.0);

/// Status bar.
pub const STATUS_H: f32 = 22.0;
pub const CLOCK_TEXT: Color32 = Color32::from_rgb(170, 178, 192);

/// Beat lights and warnings.
pub const BEAT_DOWN: Color32 = Color32::YELLOW;
pub const BEAT_OTHER: Color32 = Color32::LIGHT_BLUE;
pub const ERROR_TEXT: Color32 = Color32::LIGHT_RED;

/// Tile color for a generator, stable per kind.
pub fn tile_color(key: &str) -> Color32 {
    let mut h: u32 = 2166136261;
    for b in key.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32 / 360.0;
    let c = eframe::egui::ecolor::Hsva::new(hue, 0.45, 0.42, 1.0);
    c.into()
}
