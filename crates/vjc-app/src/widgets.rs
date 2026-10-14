//! Shared UI primitives. Every look value comes from `theme`, never from literals.

use crate::theme::{
    ACCENT, DANGER, DIALOG_W, FIELD_W_LG, FIELD_W_MD, FIELD_W_SM, FS_SM, FS_XS, ON_ACCENT,
    RADIUS_SM, SP_4, SURFACE_RAISED, TEXT, TEXT_MUTED, bold_font,
};
use eframe::egui::{self, RichText, Stroke};

/// Button emphasis: the main action, a plain one, or a destructive one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
}

/// Text field width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    #[allow(
        dead_code,
        reason = "part of the scale; first user lands in a later screen"
    )]
    Sm,
    Md,
    Lg,
}

impl Width {
    pub fn px(self) -> f32 {
        match self {
            Width::Sm => FIELD_W_SM,
            Width::Md => FIELD_W_MD,
            Width::Lg => FIELD_W_LG,
        }
    }
}

pub fn btn(ui: &mut egui::Ui, label: &str, kind: Kind) -> egui::Response {
    let b = match kind {
        Kind::Primary => egui::Button::new(RichText::new(label).color(ON_ACCENT)).fill(ACCENT),
        Kind::Secondary => egui::Button::new(RichText::new(label).color(TEXT)),
        Kind::Danger => egui::Button::new(RichText::new(label).color(DANGER))
            .fill(SURFACE_RAISED)
            .stroke(Stroke::new(1.0, DANGER)),
    };
    ui.add(b.corner_radius(RADIUS_SM))
}

/// Titled group: bold title, separator, body, then spacing.
pub fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.label(
        RichText::new(title)
            .font(bold_font(ui.ctx(), FS_SM))
            .color(TEXT),
    );
    ui.separator();
    body(ui);
    ui.add_space(SP_4);
}

/// Small muted help line.
pub fn help(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).size(FS_XS).color(TEXT_MUTED))
}

/// Single-line text field; with `err` the outline turns DANGER and the message shows below.
pub fn field_text(
    ui: &mut egui::Ui,
    text: &mut String,
    width: Width,
    err: Option<&str>,
) -> egui::Response {
    let r = ui
        .scope(|ui| {
            if err.is_some() {
                let v = ui.visuals_mut();
                for w in [&mut v.widgets.inactive, &mut v.widgets.hovered] {
                    w.bg_stroke = Stroke::new(1.0, DANGER);
                }
                v.selection.stroke = Stroke::new(v.selection.stroke.width, DANGER);
            }
            ui.add(egui::TextEdit::singleline(text).desired_width(width.px()))
        })
        .inner;
    if let Some(e) = err {
        ui.label(RichText::new(e).size(FS_XS).color(DANGER));
    }
    r
}

/// Centered modal window on SURFACE tokens: a scrim blocks the rest of the UI.
pub fn dialog(ctx: &egui::Context, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Area::new(egui::Id::new(("dialog-scrim", title)))
        .order(egui::Order::Middle)
        .fixed_pos(egui::Pos2::ZERO)
        .show(ctx, |ui| {
            let r = ctx.content_rect();
            ui.painter().rect_filled(r, 0.0, crate::theme::SCRIM);
            ui.allocate_rect(r, egui::Sense::click_and_drag());
        });
    egui::Window::new(title)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .fixed_size(egui::vec2(DIALOG_W, 0.0))
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(crate::theme::SURFACE)
                .stroke(Stroke::new(1.0, crate::theme::BORDER)),
        )
        .show(ctx, body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn every_button_kind_renders() {
        let mut h = Harness::new_ui(|ui| {
            for (l, k) in [
                ("Go", Kind::Primary),
                ("Plain", Kind::Secondary),
                ("Erase", Kind::Danger),
            ] {
                btn(ui, l, k);
            }
        });
        h.run();
        for l in ["Go", "Plain", "Erase"] {
            h.get_by_label(l);
        }
    }

    #[test]
    fn field_text_shows_error() {
        let mut s = String::from("x");
        let mut h = Harness::new_ui(move |ui| {
            field_text(ui, &mut s, Width::Md, Some("Bad value"));
        });
        h.run();
        h.get_by_label("Bad value");
    }

    #[test]
    fn dialog_shows_title_and_body() {
        let mut h = Harness::new_ui(|ui| {
            let ctx = ui.ctx().clone();
            dialog(&ctx, "Title", |ui| {
                btn(ui, "Ok", Kind::Primary);
            })
        });
        h.run();
        h.get_by_label("Title");
        h.get_by_label("Ok");
    }
}
