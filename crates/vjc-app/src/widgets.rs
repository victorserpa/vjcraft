//! Shared UI primitives. Every look value comes from `theme`, never from literals.

use crate::theme::{
    ACCENT, DANGER, DIALOG_W, FIELD_W_LG, FIELD_W_MD, FIELD_W_SM, FS_SM, FS_XS, ON_ACCENT,
    RADIUS_SM, SP_4, STROKE_W, SURFACE_RAISED, TEXT, TEXT_MUTED, bold_font,
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
            .stroke(Stroke::new(STROKE_W, DANGER)),
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
                    w.bg_stroke = Stroke::new(STROKE_W, DANGER);
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

/// Numeric field (drag or type). The value is clamped to `range`; a value that arrived
/// outside it shows the DANGER outline for that frame.
pub fn field_num(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f32,
) -> egui::Response {
    let bad = !range.contains(value);
    ui.scope(|ui| {
        if bad {
            let v = ui.visuals_mut();
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered] {
                w.bg_stroke = Stroke::new(STROKE_W, DANGER);
            }
        }
        // Typed math (`/3`) is relative to the value when editing began. egui re-parses the
        // text on Enter and again on losing focus, so a base that follows the value would
        // apply the operation twice. The base is kept while focused and one frame after.
        let id = ui.next_auto_id();
        let key = id.with("field-num-base");
        let focused = ui.memory(|m| m.has_focus(id));
        let kept = ui.data(|d| d.get_temp::<(f32, bool)>(key));
        let base = kept.map_or(*value, |(b, _)| b);
        let drag = egui::DragValue::new(value)
            .range(range)
            .speed(step)
            // Show the real value: a rounded display must not be re-parsed and written back.
            .custom_formatter(|v, _| {
                let t = format!("{v:.4}");
                t.trim_end_matches('0').trim_end_matches('.').to_owned()
            })
            .custom_parser(move |t| vjc_core::slice_geom::eval_field(t, base).map(f64::from));
        let r = ui.add(drag);
        let focused = focused || r.has_focus();
        ui.data_mut(|d| match (focused, kept) {
            (true, _) => {
                d.insert_temp(key, (base, true));
            }
            (false, Some((b, true))) => {
                d.insert_temp(key, (b, false));
            }
            _ => {
                d.remove_temp::<(f32, bool)>(key);
            }
        });
        r
    })
    .inner
}

/// Horizontal slider with a text label.
pub fn slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    label: &str,
) -> egui::Response {
    ui.add(egui::Slider::new(value, range).text(label))
}

/// Centered modal window on SURFACE tokens: a scrim (above every other window) blocks the
/// rest of the UI. With `esc_cancels` (the dialog has a cancel action) it returns true on the
/// frame Escape is pressed; the caller then closes it like the cancel button.
pub fn dialog(
    ctx: &egui::Context,
    title: &str,
    esc_cancels: bool,
    body: impl FnOnce(&mut egui::Ui),
) -> bool {
    egui::Area::new(egui::Id::new(("dialog-scrim", title)))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::Pos2::ZERO)
        .show(ctx, |ui| {
            let r = ctx.content_rect();
            ui.painter().rect_filled(r, 0.0, crate::theme::SCRIM);
            ui.allocate_rect(r, egui::Sense::click_and_drag());
        });
    let shown = egui::Window::new(title)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .fixed_size(egui::vec2(DIALOG_W, 0.0))
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(crate::theme::SURFACE)
                .stroke(Stroke::new(STROKE_W, crate::theme::BORDER)),
        )
        .show(ctx, body);
    // Same order as the scrim: keep the dialog on top, or a click on the scrim buries it.
    if let Some(w) = shown {
        ctx.move_to_top(w.response.layer_id);
    }
    esc_cancels && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
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
            dialog(&ctx, "Title", false, |ui| {
                btn(ui, "Ok", Kind::Primary);
            });
        });
        h.run();
        h.get_by_label("Title");
        h.get_by_label("Ok");
    }

    #[test]
    fn field_num_clamps_to_range() {
        let mut h = Harness::new_ui_state(
            |ui, v: &mut f32| {
                field_num(ui, v, 0.0..=100.0, 1.0);
            },
            500.0_f32,
        );
        h.run();
        assert_eq!(*h.state(), 100.0);
    }

    #[test]
    fn escape_closes_dialog_with_cancel() {
        let mut h = Harness::new_ui_state(
            |ui, closed: &mut bool| {
                let ctx = ui.ctx().clone();
                if dialog(&ctx, "Title", true, |ui| {
                    btn(ui, "Cancel", Kind::Secondary);
                }) {
                    *closed = true;
                }
            },
            false,
        );
        h.run();
        assert!(!*h.state());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(*h.state());
    }

    #[test]
    fn a_click_on_the_scrim_leaves_the_dialog_clickable() {
        let mut h = Harness::new_ui_state(
            |ui, hits: &mut u32| {
                let ctx = ui.ctx().clone();
                dialog(&ctx, "Title", false, |ui| {
                    if btn(ui, "Restore", Kind::Primary).clicked() {
                        *hits += 1;
                    }
                });
            },
            0,
        );
        h.run();
        // Click the scrim (top-left corner, outside the centered dialog).
        h.hover_at(egui::pos2(2.0, 2.0));
        h.run();
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos: egui::pos2(2.0, 2.0),
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.run();
        }
        h.get_by_label("Restore").click();
        h.run();
        assert_eq!(*h.state(), 1, "dialog button must still receive the click");
    }
}
