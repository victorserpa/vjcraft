//! Reusable parameter widget (slider + typed value + reset) and its pure helpers.

use eframe::egui;
use vjc_model::{Animation, Curve, Value};

/// "0.5", "1", "-2.25": at most 3 decimals, no trailing zeros.
pub fn format_value(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Parses a typed number (comma or dot decimal) and clamps it to `range`.
/// None for empty, non-numeric or non-finite text.
pub fn parse_value(text: &str, range: Option<(f64, f64)>) -> Option<f64> {
    let v: f64 = text.trim().replace(',', ".").parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(match range {
        Some((lo, hi)) => v.clamp(lo, hi),
        None => v,
    })
}

/// The value a reset button applies: the default, or None when already there.
pub fn reset_to(current: &Value, default: &Value) -> Option<Value> {
    (current != default).then(|| default.clone())
}

/// Collapsible section header ("- Title" open, "+ Title" closed). The open state lives in
/// egui memory under `id`, sections start open. Returns whether the body should be drawn.
pub fn section(ui: &mut egui::Ui, id: &'static str, title: &str) -> bool {
    let key = ui.id().with(("section", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(true);
    let arrow = if open { "-" } else { "+" };
    let text = egui::RichText::new(format!("{arrow} {title}"))
        .strong()
        .color(crate::theme::SECTION_HEAD);
    if ui
        .add(egui::Label::new(text).sense(egui::Sense::click()))
        .clicked()
    {
        open = !open;
        ui.data_mut(|d| d.insert_temp(key, open));
    }
    open
}

/// Slider with value label (double click to type, Enter commits, Esc cancels) and a reset
/// button. Returns the new value when the user changed it; always inside `range`.
pub fn param_slider(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: f64,
    default: f64,
    range: (f64, f64),
) -> Option<f64> {
    slider_inner(ui, id, label, value, default, range, None).0
}

/// Like `param_slider` plus the "~" animation button. `value` is the effective (animated)
/// value to display. Returns (new base value, new animation when the user picked one).
pub fn param_slider_anim(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: f64,
    default: f64,
    range: (f64, f64),
    anim: &Option<Animation>,
) -> (Option<f64>, Option<Option<Animation>>) {
    slider_inner(ui, id, label, value, default, range, Some(anim))
}

/// Short name of the animation kind, "Off" for none.
pub fn anim_kind(a: &Option<Animation>) -> &'static str {
    match a {
        None => "Off",
        Some(Animation::Timeline { .. }) => "Timeline",
        Some(Animation::Beats { .. }) => "Beats",
        Some(Animation::Audio { .. }) => "Audio",
        Some(Animation::Envelope { .. }) => "Envelope",
        Some(Animation::Dashboard { .. }) => "Dashboard",
    }
}

/// A fresh animation of the named kind with usable defaults.
pub fn default_animation(kind: &str) -> Option<Animation> {
    match kind {
        "Timeline" => Some(Animation::Timeline {
            duration_secs: 4.0,
            curve: Curve::Sine,
        }),
        "Beats" => Some(Animation::Beats {
            beats: 4.0,
            curve: Curve::Sine,
        }),
        "Envelope" => Some(Animation::Envelope {
            points: vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)],
            beats: 4.0,
        }),
        "Dashboard" => Some(Animation::Dashboard { dial: 0 }),
        _ => None,
    }
}

fn curve_combo(ui: &mut egui::Ui, id: egui::Id, curve: &mut Curve) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(format!("{curve:?}"))
        .show_ui(ui, |ui| {
            for c in [
                Curve::Linear,
                Curve::Sine,
                Curve::Saw,
                Curve::Square,
                Curve::Random,
            ] {
                let n = format!("{c:?}");
                ui.selectable_value(curve, c, n);
            }
        });
}

/// Popup body: kind selector and the fields of the current kind. Returns the edited animation.
fn anim_menu(
    ui: &mut egui::Ui,
    id: egui::Id,
    cur: &Option<Animation>,
) -> Option<Option<Animation>> {
    let mut a = cur.clone();
    let mut changed = false;
    for k in ["Off", "Timeline", "Beats", "Envelope", "Dashboard"] {
        if ui.selectable_label(anim_kind(cur) == k, k).clicked() && anim_kind(cur) != k {
            a = default_animation(k);
            changed = true;
        }
    }
    ui.separator();
    match &mut a {
        Some(Animation::Timeline {
            duration_secs,
            curve,
        }) => {
            changed |= ui
                .add(
                    egui::DragValue::new(duration_secs)
                        .range(0.1..=600.0)
                        .suffix(" s"),
                )
                .changed();
            let before = curve.clone();
            curve_combo(ui, id.with("c"), curve);
            changed |= *curve != before;
        }
        Some(Animation::Beats { beats, curve }) => {
            changed |= ui
                .add(
                    egui::DragValue::new(beats)
                        .range(0.25..=64.0)
                        .suffix(" beats"),
                )
                .changed();
            let before = curve.clone();
            curve_combo(ui, id.with("c"), curve);
            changed |= *curve != before;
        }
        Some(Animation::Envelope { points, beats }) => {
            changed |= ui
                .add(
                    egui::DragValue::new(beats)
                        .range(0.25..=64.0)
                        .suffix(" beats"),
                )
                .changed();
            let mut remove = None;
            for (i, (x, y)) in points.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(egui::DragValue::new(x).range(0.0..=1.0).speed(0.01))
                        .changed();
                    changed |= ui
                        .add(egui::DragValue::new(y).range(0.0..=1.0).speed(0.01))
                        .changed();
                    if ui.small_button("x").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                points.remove(i);
                changed = true;
            }
            if ui.small_button("+ point").clicked() {
                points.push((1.0, 0.5));
                changed = true;
            }
        }
        Some(Animation::Dashboard { dial }) => {
            let mut n = *dial + 1;
            changed |= ui
                .add(egui::DragValue::new(&mut n).range(1..=8).prefix("Dial "))
                .changed();
            *dial = n - 1;
        }
        // TODO(#24): audio needs cpal + rustfft; the variant is evaluated but fed no bands.
        Some(Animation::Audio { .. }) | None => {}
    }
    changed.then_some(a)
}

fn slider_inner(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: f64,
    default: f64,
    range: (f64, f64),
    anim: Option<&Option<Animation>>,
) -> (Option<f64>, Option<Option<Animation>>) {
    let id = ui.id().with(id);
    let mut out = None;
    let mut anim_out = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let mut x = value;
        let s = egui::Slider::new(&mut x, range.0..=range.1).show_value(false);
        if ui.add(s).changed() {
            out = Some(x);
        }
        let editing: Option<String> = ui.data(|d| d.get_temp(id));
        if let Some(mut buf) = editing {
            let e = ui.add(
                egui::TextEdit::singleline(&mut buf).desired_width(crate::theme::PROP_VALUE_W),
            );
            // Focus only when the entry opens: asking again every frame would take the focus
            // back on the frame Enter/Esc/click-away releases it, and the entry never closed.
            if !e.has_focus() && !e.lost_focus() {
                e.request_focus();
            }
            if e.lost_focus() {
                if ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && let Some(v) = parse_value(&buf, Some(range))
                {
                    out = Some(v);
                }
                ui.data_mut(|d| d.remove_temp::<String>(id));
            } else {
                ui.data_mut(|d| d.insert_temp(id, buf));
            }
        } else {
            let r = ui.add(egui::Label::new(format_value(value)).sense(egui::Sense::click()));
            if r.double_clicked() {
                ui.data_mut(|d| d.insert_temp(id, format_value(value)));
            }
        }
        let step = (range.1 - range.0) / 100.0;
        for (txt, name, sign) in [("-", "Decrease", -1.0), ("+", "Increase", 1.0)] {
            let b = ui.small_button(txt);
            b.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
            if b.clicked() {
                out = Some((value + sign * step).clamp(range.0, range.1));
            }
        }
        let reset = reset_to(&Value::Number(value), &Value::Number(default));
        let btn = egui::Button::new("\u{21ba}");
        if ui
            .add_enabled(reset.is_some(), btn)
            .on_hover_text("Reset to default")
            .clicked()
        {
            out = Some(default.clamp(range.0, range.1));
        }
        if let Some(cur) = anim {
            let b = ui.menu_button(if cur.is_some() { "\u{223f}" } else { "~" }, |ui| {
                anim_menu(ui, id, cur)
            });
            if let Some(Some(a)) = b.inner {
                anim_out = Some(a);
            }
        }
    });
    (out, anim_out)
}

/// Single-line text field that commits on Enter or focus loss (never an empty name).
/// `id` must identify the edited entity so the buffer resets when the selection changes.
pub fn text_field(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug + std::fmt::Debug,
    label: &str,
    current: &str,
) -> Option<String> {
    let id = ui.id().with(id);
    let mut out = None;
    ui.horizontal(|ui| {
        ui.label(label);
        // A buffer left from an edit that never lost focus (tab switched away) is stale:
        // it only counts if the field was drawn in the previous frame too, so a focus
        // loss this frame (Tab, click outside) still sees the typed text.
        let eid = id.with("edit");
        let frame = ui.ctx().cumulative_frame_nr();
        let mut buf: String = ui
            .data(|d| d.get_temp::<(String, u64)>(id))
            .filter(|(_, f)| frame <= f + 1)
            .map_or_else(|| current.into(), |(b, _)| b);
        let e = ui.add(
            egui::TextEdit::singleline(&mut buf)
                .id(eid)
                .desired_width(160.0),
        );
        if e.lost_focus() {
            ui.data_mut(|d| d.remove_temp::<(String, u64)>(id));
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if !esc && !buf.trim().is_empty() && buf.trim() != current {
                out = Some(buf.trim().to_string());
            }
        } else if e.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, (buf, frame)));
        } else {
            ui.data_mut(|d| d.remove_temp::<(String, u64)>(id));
        }
    });
    out
}

/// Drop-down over `options`; returns the newly picked value.
pub fn combo<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug + std::fmt::Debug,
    label: &str,
    current: &T,
    options: &[(T, String)],
) -> Option<T> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let text = options
            .iter()
            .find(|(v, _)| v == current)
            .map_or("?", |(_, n)| n.as_str());
        egui::ComboBox::from_id_salt(ui.id().with(id))
            .selected_text(text)
            .show_ui(ui, |ui| {
                for (v, n) in options {
                    if ui.selectable_label(v == current, n).clicked() {
                        out = Some(v.clone());
                    }
                }
            });
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_compactly() {
        assert_eq!(format_value(0.5), "0.5");
        assert_eq!(format_value(1.0), "1");
        assert_eq!(format_value(-2.2504), "-2.25");
        assert_eq!(format_value(0.0), "0");
    }

    #[test]
    fn parses_and_clamps() {
        let r = Some((0.0, 1.0));
        assert_eq!(parse_value(" 0,25 ", r), Some(0.25));
        assert_eq!(parse_value("7", r), Some(1.0));
        assert_eq!(parse_value("-3", r), Some(0.0));
        assert_eq!(parse_value("9", None), Some(9.0));
        for bad in ["", "abc", "NaN", "inf"] {
            assert_eq!(parse_value(bad, r), None, "{bad}");
        }
    }

    #[test]
    fn anim_kinds_round_trip() {
        assert_eq!(anim_kind(&None), "Off");
        for k in ["Timeline", "Beats", "Envelope", "Dashboard"] {
            assert_eq!(anim_kind(&default_animation(k)), k);
        }
        assert_eq!(default_animation("Off"), None);
    }

    #[test]
    fn reset_returns_default() {
        let d = Value::Number(1.0);
        assert_eq!(reset_to(&Value::Number(0.3), &d), Some(d.clone()));
        assert_eq!(reset_to(&d, &d), None);
    }
}
