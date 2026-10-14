//! Painted widgets of the clip grid (cells, headers). Look comes from `theme`.

use crate::theme;
use eframe::egui::{self, Align2, FontId, Sense, StrokeKind, TextureId};

/// What fills the picture area of a cell.
pub enum Tile<'a> {
    Empty,
    /// Texture and its width/height ratio; the picture is cover-cropped to the cell.
    Thumb(TextureId, f32),
    /// Generator or source without a frame: colored tile with the kind's name.
    Color(&'a str),
    /// Thumbnail still decoding.
    Pending,
}

/// Highlight state of a cell.
#[derive(Clone, Copy, Default)]
pub struct CellState {
    pub playing: bool,
    pub selected: bool,
}

/// UV rectangle that fills a box of ratio `box_aspect` with an image of ratio `img_aspect`
/// without stretching: the longer axis is cropped around the centre.
pub fn cover_uv(img_aspect: f32, box_aspect: f32) -> egui::Rect {
    let (mut u, mut v) = (1.0_f32, 1.0_f32);
    if img_aspect > box_aspect {
        u = (box_aspect / img_aspect).clamp(0.0, 1.0);
    } else if img_aspect > 0.0 {
        v = (img_aspect / box_aspect).clamp(0.0, 1.0);
    }
    egui::Rect::from_min_max(
        egui::pos2((1.0 - u) / 2.0, (1.0 - v) / 2.0),
        egui::pos2((1.0 + u) / 2.0, (1.0 + v) / 2.0),
    )
}

/// One-row layout of `text` that ends in an ellipsis when wider than `width`.
/// Returns the galley and whether it was cut.
pub fn elide(
    ui: &egui::Ui,
    text: &str,
    font: FontId,
    color: egui::Color32,
    width: f32,
) -> (std::sync::Arc<egui::Galley>, bool) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let elided = galley.elided;
    (galley, elided)
}

/// A clip cell: picture on top, name strip below. `label` is the accessible name.
pub fn cell(
    ui: &mut egui::Ui,
    label: &str,
    name: &str,
    tile: Tile,
    state: CellState,
) -> egui::Response {
    let CellState { playing, selected } = state;
    let size = egui::vec2(theme::CELL_W, theme::THUMB_H + theme::NAME_H);
    let (rect, mut r) = ui.allocate_exact_size(size, Sense::click());
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
        Tile::Thumb(id, aspect) => {
            p.rect_filled(pic, round, theme::MONITOR_BG);
            p.image(
                id,
                pic,
                cover_uv(aspect, pic.aspect_ratio()),
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
    let filled = !matches!(tile, Tile::Empty);
    if filled {
        let strip_fill = if playing {
            theme::CELL_PLAYING
        } else {
            theme::CELL_NAME_BG
        };
        p.rect_filled(strip, round, strip_fill);
        let ink = if playing {
            theme::ON_ACCENT
        } else {
            theme::CELL_NAME_TEXT
        };
        let (galley, elided) = elide(
            ui,
            name,
            FontId::proportional(theme::FS_XS),
            ink,
            strip.width() - 2.0 * theme::SP_2,
        );
        let pos = strip.left_center() + egui::vec2(theme::SP_2, -galley.size().y / 2.0);
        ui.painter().galley(pos, galley, ink);
        if elided {
            r = r.on_hover_text(name);
        }
    }
    let p = ui.painter();
    if r.hovered() && filled {
        p.rect_filled(pic, round, egui::Color32::from_white_alpha(22));
    }
    if playing && filled {
        let badge = egui::Rect::from_min_size(
            pic.min + egui::vec2(theme::SP_1, theme::SP_1),
            egui::vec2(14.0, 14.0),
        );
        p.rect_filled(badge, 2.0, theme::CELL_PLAYING);
        p.text(
            badge.center(),
            Align2::CENTER_CENTER,
            crate::icons::i::PLAY,
            theme::icon_font(theme::FS_XS),
            theme::ON_ACCENT,
        );
    }
    let (stroke, kind) = if playing {
        (
            egui::Stroke::new(2.0, theme::CELL_PLAYING),
            StrokeKind::Inside,
        )
    } else if selected {
        (
            egui::Stroke::new(2.0, theme::CELL_SELECTED),
            StrokeKind::Inside,
        )
    } else if r.has_focus() {
        (egui::Stroke::new(2.0, theme::ACCENT), StrokeKind::Inside)
    } else if r.hovered() {
        (egui::Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside)
    } else {
        (egui::Stroke::NONE, StrokeKind::Inside)
    };
    if stroke.width > 0.0 {
        p.rect_stroke(rect, round, stroke, kind);
    }
    r
}

/// Flat tool button placed in `rect`. `on` marks a toggle; `on_fill` colors it when lit.
pub fn tool(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    text: egui::RichText,
    label: &str,
    on: Option<bool>,
    on_fill: Option<egui::Color32>,
) -> egui::Response {
    let mut b = egui::Button::new(text)
        .corner_radius(2.0)
        .fill(theme::SECTION_BG);
    match (on, on_fill) {
        (Some(true), Some(c)) => b = b.fill(c),
        (Some(true), None) => b = b.selected(true),
        _ => {}
    }
    let r = ui.put(rect, b);
    crate::icons::name(r, label, on)
}

/// Master state the composition header shows.
pub struct MasterState {
    pub bypass: bool,
    pub recording: bool,
    pub dropped: Option<u64>,
    pub layers: usize,
    pub columns: usize,
}

/// Top-left cell: "Composition" title over X (clear all), B (bypass), Rec and add layer/column.
pub fn composition_header(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    m: MasterState,
    cmds: &mut Vec<vjc_core::Command>,
    targets: &mut Vec<(egui::Rect, String)>,
) {
    use crate::help::help;
    use vjc_core::Command;
    let row = theme::COLUMN_HEAD_H;
    ui.painter().rect_filled(rect, 0.0, theme::HEADER_BG);
    ui.painter().text(
        rect.min + egui::vec2(6.0, row / 2.0),
        Align2::LEFT_CENTER,
        "Composition",
        theme::bold_font(ui.ctx(), theme::FS_SM),
        theme::TEXT,
    );
    let slot = |i: f32| {
        egui::Rect::from_min_size(
            rect.min + egui::vec2(2.0 + i * 24.0, row + 2.0),
            egui::vec2(22.0, row - 4.0),
        )
    };
    if help(
        tool(
            ui,
            slot(0.0),
            crate::icons::glyph(crate::icons::i::X),
            "Clear All",
            None,
            None,
        ),
        "clear-all",
    )
    .clicked()
    {
        cmds.push(Command::ClearAll);
    }
    let by = help(
        tool(
            ui,
            slot(1.0),
            egui::RichText::new("B").font(theme::bold_font(ui.ctx(), theme::FS_XS)),
            "Bypass",
            Some(m.bypass),
            None,
        ),
        "master-bypass",
    );
    targets.push((by.rect, "/comp/master/bypass".into()));
    if by.clicked() {
        cmds.push(Command::SetMasterBypass { bypass: !m.bypass });
    }
    let label = if m.recording { "Stop rec" } else { "Record" };
    let rec = tool(
        ui,
        slot(2.0),
        crate::icons::glyph(crate::icons::i::RECORD),
        label,
        Some(m.recording),
        Some(theme::DANGER),
    );
    if help(rec, "record").clicked() {
        cmds.push(if m.recording {
            Command::StopRecording
        } else {
            Command::StartRecording
        });
    }
    if let Some(n) = m.dropped {
        ui.painter().text(
            slot(3.0).left_center() + egui::vec2(4.0, 0.0),
            Align2::LEFT_CENTER,
            format!("{n} dropped"),
            FontId::proportional(theme::FS_XS),
            theme::TEXT_MUTED,
        );
    }
    let right = |i: f32| {
        egui::Rect::from_min_size(
            egui::pos2(rect.right() - 24.0 * (i + 1.0), rect.top() + row + 2.0),
            egui::vec2(22.0, row - 4.0),
        )
    };
    if help(
        tool(
            ui,
            right(1.0),
            crate::icons::glyph(crate::icons::i::STACK_PLUS),
            "Add layer",
            None,
            None,
        ),
        "add-layer",
    )
    .clicked()
    {
        cmds.push(Command::AddLayer {
            layer: crate::new_layer(m.layers + 1),
            index: 0,
        });
    }
    if help(
        tool(
            ui,
            right(0.0),
            crate::icons::glyph(crate::icons::i::COLUMNS_PLUS_RIGHT),
            "Add column",
            None,
            None,
        ),
        "add-column",
    )
    .clicked()
    {
        cmds.push(Command::AddColumn {
            column: vjc_model::Column {
                id: uuid::Uuid::new_v4(),
                name: (m.columns + 1).to_string(),
            },
        });
    }
}

/// Column header: trigger arrow row over the name row (accent when selected).
pub fn column_head(
    ui: &egui::Ui,
    rect: egui::Rect,
    name: &str,
    selected: bool,
    r: &egui::Response,
) {
    let p = ui.painter();
    let top = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), theme::COLUMN_HEAD_H));
    let bot = egui::Rect::from_min_max(egui::pos2(rect.left(), top.bottom()), rect.max);
    let lift = if r.hovered() {
        theme::SURFACE_RAISED
    } else {
        theme::COLUMN_HEAD_BG
    };
    p.rect_filled(top, 0.0, lift);
    p.rect_filled(
        bot,
        0.0,
        if selected {
            theme::COLUMN_SELECTED
        } else {
            lift
        },
    );
    p.text(
        top.center(),
        Align2::CENTER_CENTER,
        crate::icons::i::PLAY,
        theme::icon_font(theme::FS_XS),
        theme::TEXT_MUTED,
    );
    let ink = if selected {
        theme::ON_ACCENT
    } else {
        theme::TEXT
    };
    let (galley, _) = elide(
        ui,
        name,
        FontId::proportional(theme::FS_SM),
        ink,
        bot.width() - 12.0,
    );
    let pos = egui::pos2(
        bot.center().x - galley.size().x / 2.0,
        bot.center().y - galley.size().y / 2.0,
    );
    p.galley(pos, galley, ink);
}

#[cfg(test)]
mod tests {
    use super::{cover_uv, elide};
    use eframe::egui;

    #[test]
    fn cover_crops_the_longer_axis() {
        let wide = cover_uv(2.0, 1.0);
        assert!((wide.width() - 0.5).abs() < 1e-6 && (wide.height() - 1.0).abs() < 1e-6);
        let tall = cover_uv(0.5, 1.0);
        assert!((tall.height() - 0.5).abs() < 1e-6 && (tall.width() - 1.0).abs() < 1e-6);
        let same = cover_uv(1.5, 1.5);
        assert!((same.width() - 1.0).abs() < 1e-6 && (same.height() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn long_names_are_elided_and_short_ones_are_not() {
        let ctx = egui::Context::default();
        let mut out = None;
        let mut full = ctx.run_ui(egui::RawInput::default(), |ui| {
            let f = egui::FontId::proportional(11.0);
            let c = egui::Color32::WHITE;
            let long = elide(
                ui,
                "a very long clip name that cannot fit",
                f.clone(),
                c,
                60.0,
            );
            let short = elide(ui, "ok", f, c, 60.0);
            out = Some((long.1, long.0.size().x, short.1));
        });
        full.textures_delta.clear();
        let (long_cut, long_w, short_cut) = out.unwrap();
        assert!(
            long_cut && long_w <= 61.0,
            "long cut to width, got {long_w}"
        );
        assert!(!short_cut);
    }
}
