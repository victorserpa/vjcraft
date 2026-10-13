//! Reusable parameter widget (slider + typed value + reset) and its pure helpers.

use eframe::egui;
use vjc_model::Value;

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

/// Slider with value label (double click to type, Enter commits, Esc cancels) and a reset
/// button. Returns the new value when the user changed it; always inside `range`.
pub fn param_slider(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug + std::fmt::Debug,
    label: &str,
    value: f64,
    default: f64,
    range: (f64, f64),
) -> Option<f64> {
    let id = ui.id().with(id);
    let mut out = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let mut x = value;
        let s = egui::Slider::new(&mut x, range.0..=range.1).show_value(false);
        if ui.add(s).changed() {
            out = Some(x);
        }
        let editing: Option<String> = ui.data(|d| d.get_temp(id));
        if let Some(mut buf) = editing {
            let e = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(56.0));
            e.request_focus();
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
        let reset = reset_to(&Value::Number(value), &Value::Number(default));
        let btn = egui::Button::new("\u{21ba}");
        if ui
            .add_enabled(reset.is_some(), btn)
            .on_hover_text("Reset to default")
            .clicked()
        {
            out = Some(default.clamp(range.0, range.1));
        }
    });
    out
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
        let mut buf: String = ui
            .data(|d| d.get_temp(id))
            .unwrap_or_else(|| current.into());
        let e = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(160.0));
        if e.lost_focus() {
            ui.data_mut(|d| d.remove_temp::<String>(id));
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if !esc && !buf.trim().is_empty() && buf.trim() != current {
                out = Some(buf.trim().to_string());
            }
        } else if e.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, buf));
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
    fn reset_returns_default() {
        let d = Value::Number(1.0);
        assert_eq!(reset_to(&Value::Number(0.3), &d), Some(d.clone()));
        assert_eq!(reset_to(&d, &d), None);
    }
}
