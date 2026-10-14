//! Icon buttons. Glyphs come from Phosphor (see `theme::apply_style`); every button keeps a
//! text name for tooltips and AccessKit, so tests and screen readers never see the glyph.

use crate::theme;
use eframe::egui::{self, Button, Response, RichText, WidgetInfo, WidgetType};

pub use egui_phosphor::regular as i;

/// Name `r` for AccessKit and the tooltip.
pub fn name(r: Response, label: &str, toggled: Option<bool>) -> Response {
    let enabled = r.enabled();
    let l = label.to_owned();
    r.widget_info(move || match toggled {
        Some(on) => WidgetInfo::selected(WidgetType::Button, enabled, on, l.clone()),
        None => WidgetInfo::labeled(WidgetType::Button, enabled, l.clone()),
    });
    r.on_hover_text(label)
}

pub fn glyph(icon: &str) -> RichText {
    RichText::new(icon).font(theme::icon_font(theme::FS_BASE + 2.0))
}

/// Icon followed by text, for buttons and rows that show both.
pub fn with_text(ui: &egui::Ui, icon: &str, text: &str) -> egui::text::LayoutJob {
    let color = ui.visuals().text_color();
    let mut job = egui::text::LayoutJob::default();
    job.append(
        icon,
        0.0,
        egui::TextFormat::simple(theme::icon_font(theme::FS_BASE), color),
    );
    job.append(
        &format!(" {text}"),
        0.0,
        egui::TextFormat::simple(egui::FontId::proportional(theme::FS_SM), color),
    );
    job
}

/// Button showing `icon`; `label` is its tooltip and accessible name.
pub fn button(ui: &mut egui::Ui, icon: &str, label: &str) -> Response {
    with(ui, Button::new(glyph(icon)), label)
}

/// Same, from a prepared button (fill, min size...). Give it `glyph`-sized text via `text`.
pub fn with(ui: &mut egui::Ui, b: Button<'_>, label: &str) -> Response {
    let r = ui.add(b);
    name(r, label, None)
}

/// Compact letter button (effect rows: B / P / ×); `label` is the tooltip and accessible name.
pub fn letter(ui: &mut egui::Ui, text: &str, label: &str, side: f32, on: Option<bool>) -> Response {
    let b = Button::new(RichText::new(text).font(theme::bold_font(ui.ctx(), theme::FS_SM)))
        .min_size(egui::vec2(side, side))
        .selected(on == Some(true));
    let r = ui.add(b);
    name(r, label, on)
}

/// Like `button`, but disabled when `enabled` is false.
pub fn button_enabled(ui: &mut egui::Ui, enabled: bool, icon: &str, label: &str) -> Response {
    let r = ui.add_enabled(enabled, Button::new(glyph(icon)));
    name(r, label, None)
}

/// Toggle button: flips `on` when clicked (marks the response changed).
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, icon: &str, label: &str) -> Response {
    let mut r = ui.add(Button::new(glyph(icon)).selected(*on));
    if r.clicked() {
        *on = !*on;
        r.mark_changed();
    }
    name(r, label, Some(*on))
}
