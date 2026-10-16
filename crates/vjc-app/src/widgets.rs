//! Shared UI primitives. Every look value comes from `theme`, never from literals.

use crate::theme::{
    ACCENT, DANGER, DIALOG_W, FIELD_W_LG, FIELD_W_MD, FIELD_W_SM, FS_SM, FS_XS, ON_ACCENT,
    RADIUS_SM, SP_1, SP_2, SP_4, STATUS_H, STROKE_W, SURFACE_RAISED, TEXT, TEXT_MUTED, TOAST_TTL_S,
    TOAST_W, WARN, bold_font,
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

/// Button that is greyed out and inert unless `enabled`.
pub fn btn_if(ui: &mut egui::Ui, enabled: bool, label: &str, kind: Kind) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| btn(ui, label, kind)).inner
}

/// Checkbox with the kit text color.
pub fn toggle(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    ui.add(egui::Checkbox::new(value, RichText::new(label).color(TEXT)))
}

/// Integer/any-numeric field (drag or type) clamped to `range`, with optional prefix/suffix.
pub fn field_int<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    prefix: &str,
    suffix: &str,
) -> egui::Response {
    field_drag(ui, value, range, 1.0, prefix, suffix)
}

/// Like `field_int` with an explicit drag `speed`.
pub fn field_drag<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    speed: f64,
    prefix: &str,
    suffix: &str,
) -> egui::Response {
    let bad = !range.contains(value);
    ui.scope(|ui| {
        if bad {
            let v = ui.visuals_mut();
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered] {
                w.bg_stroke = Stroke::new(STROKE_W, DANGER);
            }
        }
        ui.add(
            egui::DragValue::new(value)
                .range(range)
                .speed(speed)
                .prefix(prefix)
                .suffix(suffix),
        )
    })
    .inner
}

/// Pixel-size field capped by the GPU texture limit. A value already above `cap`
/// is shown as is, with a warning outline, and never rewritten on draw: only what
/// the user types is clamped.
pub fn size_field(ui: &mut egui::Ui, value: &mut u32, cap: u32, prefix: &str) -> egui::Response {
    let bad = *value > cap;
    ui.scope(|ui| {
        if bad {
            let v = ui.visuals_mut();
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered] {
                w.bg_stroke = Stroke::new(STROKE_W, DANGER);
            }
        }
        ui.add(
            egui::DragValue::new(value)
                .speed(0.0)
                .range(1..=cap)
                .clamp_existing_to_range(false)
                .prefix(prefix),
        )
    })
    .inner
}

/// Tool window (non-modal) with the kit frame: SURFACE fill and BORDER stroke.
pub fn panel_window<'a>(
    ctx: &egui::Context,
    title: impl Into<egui::WidgetText>,
) -> egui::Window<'a> {
    egui::Window::new(title).frame(
        egui::Frame::window(&ctx.global_style())
            .fill(crate::theme::SURFACE)
            .stroke(Stroke::new(STROKE_W, crate::theme::BORDER)),
    )
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

/// Like `section`, but the header folds the body. The fold state lives in egui memory under
/// `group` + title; Shift+click on a header folds or unfolds every section of the group.
pub fn fold_section(ui: &mut egui::Ui, group: &str, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let key = |t: &str| egui::Id::new(("fold", group, t.to_owned()));
    let list = egui::Id::new(("fold-all", group));
    let open = ui.data(|d| d.get_temp::<bool>(key(title))).unwrap_or(true);
    ui.data_mut(|d| {
        let seen = d.get_temp_mut_or_default::<Vec<String>>(list);
        if !seen.iter().any(|t| t == title) {
            seen.push(title.to_owned());
        }
    });
    let caret = if open {
        crate::icons::i::CARET_DOWN
    } else {
        crate::icons::i::CARET_RIGHT
    };
    let r = ui
        .horizontal(|ui| {
            let c = ui.add(
                egui::Label::new(crate::icons::glyph(caret).color(TEXT_MUTED))
                    .sense(egui::Sense::click()),
            );
            let t = ui.add(
                egui::Label::new(
                    RichText::new(title)
                        .font(bold_font(ui.ctx(), FS_SM))
                        .color(TEXT),
                )
                .sense(egui::Sense::click()),
            );
            c.union(t)
        })
        .inner;
    if r.clicked() {
        let all = ui.input(|i| i.modifiers.shift);
        let titles = if all {
            ui.data(|d| d.get_temp::<Vec<String>>(list))
                .unwrap_or_default()
        } else {
            vec![title.to_owned()]
        };
        ui.data_mut(|d| {
            titles.iter().for_each(|t| {
                d.insert_temp(key(t), !open);
            })
        });
    }
    ui.separator();
    if open {
        body(ui);
    }
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

/// Single-line text field with a placeholder shown while empty and an explicit pixel width.
pub fn field_text_hint(
    ui: &mut egui::Ui,
    text: &mut String,
    width: f32,
    hint: &str,
) -> egui::Response {
    ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .desired_width(width),
    )
}

/// Selectable row/toggle button (combo entries, view switches).
pub fn choice(
    ui: &mut egui::Ui,
    selected: bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    ui.selectable_label(selected, label)
}

/// Drop-down: `id` keys it, `selected` is the closed text, `width` an optional fixed width.
pub fn combo<R>(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    selected: impl Into<egui::WidgetText>,
    width: Option<f32>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let mut c = egui::ComboBox::from_id_salt(id).selected_text(selected);
    if let Some(w) = width {
        c = c.width(w);
    }
    c.show_ui(ui, body).inner
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

/// Panel tab: the selected one is bold on BG, the others muted on the header.
pub fn tab(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    let font = if selected {
        bold_font(ui.ctx(), FS_SM)
    } else {
        egui::FontId::proportional(FS_SM)
    };
    let text = RichText::new(label)
        .font(font)
        .color(if selected { TEXT } else { TEXT_MUTED });
    let r = ui.add(
        egui::Button::new(text)
            .corner_radius(0.0)
            .fill(if selected {
                crate::theme::BG
            } else {
                crate::theme::HEADER_BG
            })
            .min_size(egui::vec2(0.0, crate::theme::PANEL_HEAD_H)),
    );
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label));
    r
}

/// Small tag pill.
pub fn chip(ui: &mut egui::Ui, text: &str) -> egui::Response {
    egui::Frame::new()
        .fill(SURFACE_RAISED)
        .stroke(Stroke::new(STROKE_W, crate::theme::BORDER))
        .corner_radius(RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(SP_4 as i8, SP_1 as i8))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(FS_XS).color(TEXT_MUTED))
        })
        .response
}

/// Toast severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    #[allow(dead_code, reason = "part of the scale; no caller yet")]
    Info,
    Warn,
    Error,
}

#[derive(Clone)]
struct ToastItem {
    msg: String,
    level: Level,
    born: f64,
}

/// Most toasts on screen at once.
const TOAST_MAX: usize = 3;

fn toast_queue_id() -> egui::Id {
    egui::Id::new("vjc-toast-queue")
}

/// Queues a toast; `toasts_show` draws it.
pub fn toast(ctx: &egui::Context, msg: &str, level: Level) {
    let born = ctx.input(|i| i.time);
    ctx.data_mut(|d| {
        let q = d.get_temp_mut_or_default::<Vec<ToastItem>>(toast_queue_id());
        // Same message again: refresh it instead of stacking a copy.
        q.retain(|t| t.msg != msg);
        q.push(ToastItem {
            msg: msg.to_owned(),
            level,
            born,
        });
        // A burst never covers the panels: keep only the newest few.
        let extra = q.len().saturating_sub(TOAST_MAX);
        q.drain(..extra);
    });
    ctx.request_repaint();
}

/// Draws the queue stacked at the bottom right above the status bar. Info and Warn expire
/// after `TOAST_TTL_S`; Error stays until clicked. Call once per frame.
pub fn toasts_show(ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let mut q = ctx
        .data(|d| d.get_temp::<Vec<ToastItem>>(toast_queue_id()))
        .unwrap_or_default();
    q.retain(|t| t.level == Level::Error || now - t.born < TOAST_TTL_S);
    if !q.is_empty() {
        let mut dismissed = None;
        egui::Area::new(egui::Id::new("vjc-toasts"))
            .order(egui::Order::Foreground)
            .anchor(
                egui::Align2::RIGHT_BOTTOM,
                egui::vec2(-SP_4, -(STATUS_H + SP_4)),
            )
            .show(ctx, |ui| {
                ui.set_width(TOAST_W);
                for (i, t) in q.iter().enumerate() {
                    let edge = match t.level {
                        Level::Info => ACCENT,
                        Level::Warn => WARN,
                        Level::Error => DANGER,
                    };
                    let r = egui::Frame::new()
                        .fill(crate::theme::SURFACE)
                        .stroke(Stroke::new(STROKE_W, edge))
                        .corner_radius(RADIUS_SM)
                        .inner_margin(egui::Margin::same(SP_4 as i8))
                        .show(ui, |ui| {
                            ui.set_width(TOAST_W - 2.0 * SP_4);
                            ui.label(RichText::new(&t.msg).size(FS_SM).color(TEXT))
                        })
                        .response
                        .interact(egui::Sense::click());
                    if r.clicked() {
                        dismissed = Some(i);
                    }
                    ui.add_space(SP_2);
                }
            });
        if let Some(i) = dismissed {
            q.remove(i);
        }
        let next = q
            .iter()
            .filter(|t| t.level != Level::Error)
            .map(|t| t.born + TOAST_TTL_S - now)
            .fold(f64::INFINITY, f64::min);
        if next.is_finite() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(next.max(0.0) + 0.05));
        }
    }
    ctx.data_mut(|d| d.insert_temp(toast_queue_id(), q));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    /// Alpha of the text colors drawn by a button and a text field in one frame.
    fn text_opacities(enabled: bool) -> Vec<f32> {
        fn walk(s: &egui::epaint::Shape, out: &mut Vec<f32>) {
            match s {
                egui::epaint::Shape::Text(t) => {
                    // Painted glyph colors: the opacity is baked into the row meshes.
                    for row in &t.galley.rows {
                        if let Some(v) = row.visuals.mesh.vertices.first() {
                            let c = if v.color == egui::Color32::PLACEHOLDER {
                                t.fallback_color
                            } else {
                                v.color
                            };
                            out.push(f32::from(c.a()) / 255.0);
                        }
                    }
                }
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        crate::theme::apply_style(&ctx);
        let mut text = String::from("abc");
        let mut run = || {
            ctx.run_ui(Default::default(), |ui| {
                ui.add_enabled_ui(enabled, |ui| {
                    btn(ui, "Go", Kind::Secondary);
                    field_text(ui, &mut text, Width::Md, None);
                });
            })
        };
        run().textures_delta.clear();
        let mut out = run();
        out.textures_delta.clear();
        let mut v = vec![];
        for c in &out.shapes {
            walk(&c.shape, &mut v);
        }
        v
    }

    #[test]
    fn disabled_buttons_and_fields_are_dimmed_by_the_token() {
        let on = text_opacities(true);
        let off = text_opacities(false);
        assert!(on.len() >= 2 && on.len() == off.len(), "{on:?} {off:?}");
        assert!(on.iter().all(|o| *o == 1.0), "enabled stays opaque: {on:?}");
        assert!(
            off.iter()
                .all(|o| (*o - crate::theme::DISABLED_ALPHA).abs() < 1e-3),
            "disabled uses DISABLED_ALPHA: {off:?}"
        );
    }

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
    fn selected_tab_is_marked() {
        let mut h = Harness::new_ui(|ui| {
            tab(ui, true, "One");
            tab(ui, false, "Two");
        });
        h.run();
        assert!(
            h.get_by_label("One").accesskit_node().toggled()
                == Some(egui::accesskit::Toggled::True)
        );
        assert!(
            h.get_by_label("Two").accesskit_node().toggled()
                != Some(egui::accesskit::Toggled::True)
        );
    }

    fn toast_harness(level: Level) -> Harness<'static> {
        let mut first = true;
        Harness::new_ui(move |ui| {
            let ctx = ui.ctx().clone();
            if std::mem::take(&mut first) {
                toast(&ctx, "Hello toast", level);
            }
            toasts_show(&ctx);
        })
    }

    #[test]
    fn info_toast_expires() {
        let mut h = toast_harness(Level::Info);
        h.run();
        h.get_by_label("Hello toast");
        for _ in 0..40 {
            h.step();
        }
        assert!(h.query_by_label("Hello toast").is_none());
    }

    #[test]
    fn error_toast_persists_until_clicked() {
        let mut h = toast_harness(Level::Error);
        h.run();
        for _ in 0..80 {
            h.step();
        }
        h.get_by_label("Hello toast").click();
        h.run();
        assert!(h.query_by_label("Hello toast").is_none());
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

    #[test]
    fn toast_burst_is_deduped_and_capped() {
        let ctx = egui::Context::default();
        for _ in 0..5 {
            toast(&ctx, "Save failed", Level::Error);
        }
        for k in 0..5 {
            toast(&ctx, &format!("w{k}"), Level::Warn);
        }
        let q = ctx
            .data(|d| d.get_temp::<Vec<ToastItem>>(toast_queue_id()))
            .unwrap();
        assert_eq!(q.len(), TOAST_MAX);
        assert_eq!(q.iter().filter(|t| t.msg == "Save failed").count(), 0);
    }

    #[test]
    fn field_text_hint_edits() {
        let mut h = Harness::new_ui_state(
            |ui, s: &mut String| {
                field_text_hint(ui, s, 100.0, "name");
            },
            String::new(),
        );
        h.run();
        assert!(h.state().is_empty());
    }

    #[test]
    fn choice_reports_selected_and_click() {
        let mut h = Harness::new_ui_state(
            |ui, n: &mut u32| {
                if choice(ui, true, "Pick").clicked() {
                    *n += 1;
                }
            },
            0,
        );
        h.run();
        h.get_by_label("Pick").click();
        h.run();
        assert_eq!(*h.state(), 1);
    }

    #[test]
    fn combo_is_closed_until_clicked() {
        let mut h = Harness::new_ui_state(
            |ui, ran: &mut bool| {
                combo(ui, "cb", "Closed", Some(80.0), |_| *ran = true);
            },
            false,
        );
        h.run();
        assert!(!*h.state(), "body must not run while closed");
    }

    #[test]
    fn field_drag_clamps_to_range() {
        let mut h = Harness::new_ui_state(
            |ui, v: &mut f32| {
                field_drag(ui, v, 0.0..=1.0, 0.01, "", "");
            },
            5.0_f32,
        );
        h.run();
        assert_eq!(*h.state(), 1.0);
    }
}
