//! Reusable parameter widget (slider + typed value + reset) and its pure helpers.

use eframe::egui;
use vjc_model::{Animation, Curve, FftBand, Value};

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

/// Collapsible section header: full-width bar with a fold arrow and the title. The open
/// state lives in egui memory under `id`, sections start open. Returns whether the body
/// should be drawn. AccessKit name: "<title> (expanded|collapsed)".
pub fn section(ui: &mut egui::Ui, id: &'static str, title: &str) -> bool {
    use crate::theme;
    let key = ui.id().with(("section", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(true);
    let (rect, r) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), theme::SECTION_ROW_H),
        egui::Sense::click(),
    );
    let state = if open { "expanded" } else { "collapsed" };
    r.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{title} ({state})"))
    });
    let fill = if r.hovered() {
        theme::SURFACE_RAISED
    } else {
        crate::theme::SECTION_BG
    };
    let p = ui.painter();
    p.rect_filled(rect, 0.0, fill);
    let arrow = if open {
        crate::icons::i::CARET_DOWN
    } else {
        crate::icons::i::CARET_RIGHT
    };
    let mid = rect.left_center();
    p.text(
        mid + egui::vec2(theme::SP_3 + 4.0, 0.0),
        egui::Align2::CENTER_CENTER,
        arrow,
        theme::icon_font(theme::FS_SM),
        theme::SECTION_HEAD,
    );
    p.text(
        mid + egui::vec2(theme::SP_3 + 16.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(theme::FS_SM),
        theme::TEXT,
    );
    if r.has_focus() {
        p.rect_stroke(
            rect,
            theme::CELL_ROUNDING,
            egui::Stroke::new(2.0, theme::CELL_SELECTED),
            egui::StrokeKind::Inside,
        );
    }
    if r.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(key, open));
    }
    open
}

/// Label column of a property row: right-aligned in a column as wide as the widest label seen
/// so far (capped at `PARAM_LABEL_MAX`), so values line up. Longer labels are cut with "..."
/// and show the full text on hover.
pub fn row_label(ui: &mut egui::Ui, label: &str) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text_w = ui.fonts_mut(|f| {
        f.layout_no_wrap(label.to_owned(), font, crate::theme::TEXT)
            .size()
            .x
    });
    let key = egui::Id::new("param-label-w");
    let seen: f32 = ui.data(|d| d.get_temp(key)).unwrap_or(0.0);
    let want = text_w.ceil().min(crate::theme::PARAM_LABEL_MAX);
    // Never more than 40% of the row: narrow panels keep room for the value and slider.
    let room = (ui.available_width() * 0.4).max(crate::theme::PARAM_LABEL_MIN);
    let w = seen.max(want).max(crate::theme::PARAM_LABEL_MIN).min(room);
    if want > seen {
        ui.data_mut(|d| d.insert_temp(key, want));
        ui.ctx().request_repaint();
    }
    let r = ui.allocate_ui_with_layout(
        egui::vec2(w, crate::theme::PROP_ROW_H),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| ui.add(egui::Label::new(label).truncate()),
    );
    if text_w > w {
        r.inner.on_hover_text(label);
    }
    ui.add_space(crate::theme::SP_2);
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
    slider_inner(ui, id, label, value, default, range, None, None).0
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
    slider_inner(ui, id, label, value, default, range, Some(anim), None)
}

/// Like `param_slider_anim`, with the -/+ buttons stepping by `step` instead of 1% of the range.
#[allow(clippy::too_many_arguments)]
pub fn param_slider_anim_step(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: f64,
    default: f64,
    range: (f64, f64),
    step: f64,
    anim: &Option<Animation>,
) -> (Option<f64>, Option<Option<Animation>>) {
    slider_inner(ui, id, label, value, default, range, Some(anim), Some(step))
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
        "Dashboard" => Some(Animation::Dashboard {
            dial: 0,
            scope: Default::default(),
        }),
        "Audio" => Some(Animation::Audio {
            band: FftBand::Low,
            gain: 1.0,
            threshold: 0.0,
            attack: 0.02,
            release: 0.2,
            min: 0.0,
            max: 1.0,
        }),
        _ => None,
    }
}

fn band_name(b: &FftBand) -> &'static str {
    match b {
        FftBand::Low => "Low",
        FftBand::LowMid => "Low-mid",
        FftBand::Mid => "Mid",
        FftBand::High => "High",
        FftBand::Level => "Level",
        FftBand::Bin(_) => "Bin",
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

/// Draws one cycle of a Timeline/Beats/Envelope animation as a curve; nothing for other kinds.
fn curve_graph(ui: &mut egui::Ui, a: &Animation) -> egui::Rect {
    let size = egui::vec2(GRAPH_W, GRAPH_H);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let samples = vjc_core::anim::preview(a, GRAPH_W as usize);
    if samples.len() > 1 {
        let pts: Vec<egui::Pos2> = samples
            .iter()
            .enumerate()
            .map(|(i, v)| graph_pos(rect, i as f64 / samples.len() as f64, *v))
            .collect();
        ui.painter()
            .rect_filled(rect, crate::theme::PILL_ROUNDING, crate::theme::SECTION_BG);
        ui.painter().add(egui::Shape::line(
            pts,
            egui::Stroke::new(1.5, crate::theme::ACCENT),
        ));
    }
    rect
}

/// The envelope curve with draggable points: drag a point to move it, double click on empty
/// space to add one. Returns true when the points changed.
fn envelope_editor(ui: &mut egui::Ui, points: &mut Vec<(f64, f64)>, beats: f64) -> bool {
    let a = Animation::Envelope {
        points: points.clone(),
        beats,
    };
    let rect = curve_graph(ui, &a);
    let r = ui.interact(
        rect,
        ui.id().with("env-graph"),
        egui::Sense::click_and_drag(),
    );
    let to_unit = |p: egui::Pos2| {
        (
            f64::from((p.x - rect.left()) / rect.width()),
            f64::from((rect.bottom() - p.y) / rect.height()),
        )
    };
    let drag_id = ui.id().with("env-drag");
    let mut changed = false;
    if r.drag_started()
        && let Some(p) = r.interact_pointer_pos()
    {
        let (x, y) = to_unit(p);
        let hit = vjc_core::anim::env_nearest(points, x, y, ENV_PICK);
        ui.data_mut(|d| d.insert_temp(drag_id, hit));
    }
    if r.dragged()
        && let Some(p) = r.interact_pointer_pos()
        && let Some(Some(i)) = ui.data(|d| d.get_temp::<Option<usize>>(drag_id))
    {
        let (x, y) = to_unit(p);
        changed |= vjc_core::anim::env_move(points, i, x, y);
    }
    if r.double_clicked()
        && let Some(p) = r.interact_pointer_pos()
    {
        let (x, y) = to_unit(p);
        if vjc_core::anim::env_nearest(points, x, y, ENV_PICK).is_none() {
            changed |= vjc_core::anim::env_insert(points, x, y).is_some();
        }
    }
    for p in points.iter() {
        ui.painter()
            .circle_filled(graph_pos(rect, p.0, p.1), 3.0, crate::theme::TEXT);
    }
    changed
}

/// Pick radius for envelope points, in 0..1 graph units.
const ENV_PICK: f64 = 0.08;

const GRAPH_W: f32 = 160.0;
const GRAPH_H: f32 = 48.0;

/// Screen position of curve point (x, y), both 0..=1, y up.
fn graph_pos(rect: egui::Rect, x: f64, y: f64) -> egui::Pos2 {
    egui::pos2(
        rect.left() + rect.width() * x as f32,
        rect.bottom() - rect.height() * y as f32,
    )
}

/// Popup body: kind selector and the fields of the current kind. Returns the edited animation.
fn anim_menu(
    ui: &mut egui::Ui,
    id: egui::Id,
    cur: &Option<Animation>,
) -> Option<Option<Animation>> {
    let mut a = cur.clone();
    let mut changed = false;
    for k in ["Off", "Timeline", "Beats", "Envelope", "Audio", "Dashboard"] {
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
            curve_graph(
                ui,
                &Animation::Timeline {
                    duration_secs: *duration_secs,
                    curve: curve.clone(),
                },
            );
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
            curve_graph(
                ui,
                &Animation::Beats {
                    beats: *beats,
                    curve: curve.clone(),
                },
            );
        }
        Some(Animation::Envelope { points, beats }) => {
            changed |= ui
                .add(
                    egui::DragValue::new(beats)
                        .range(0.25..=64.0)
                        .suffix(" beats"),
                )
                .changed();
            changed |= envelope_editor(ui, points, *beats);
            ui.menu_button("Preset", |ui| {
                for pre in vjc_core::anim::EnvPreset::ALL {
                    if ui.button(pre.label()).clicked() {
                        *points = pre.points();
                        changed = true;
                        ui.close();
                    }
                }
            });
            let mut remove = None;
            for (i, (x, y)) in points.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(egui::DragValue::new(x).range(0.0..=1.0).speed(0.01))
                        .changed();
                    changed |= ui
                        .add(egui::DragValue::new(y).range(0.0..=1.0).speed(0.01))
                        .changed();
                    if crate::icons::button(ui, crate::icons::i::X, "Remove point").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                points.remove(i);
                changed = true;
            }
            if crate::icons::button(ui, crate::icons::i::PLUS, "Add point").clicked() {
                points.push((1.0, 0.5));
                changed = true;
            }
        }
        Some(Animation::Dashboard { dial, scope }) => {
            let before = *scope;
            egui::ComboBox::from_id_salt(id.with("scope"))
                .selected_text(scope.label())
                .show_ui(ui, |ui| {
                    for s in vjc_model::DialScope::ALL {
                        ui.selectable_value(scope, s, s.label());
                    }
                });
            changed |= *scope != before;
            let mut n = *dial + 1;
            changed |= ui
                .add(egui::DragValue::new(&mut n).range(1..=8).prefix("Dial "))
                .changed();
            *dial = n - 1;
        }
        Some(Animation::Audio {
            band,
            gain,
            threshold,
            attack,
            release,
            min,
            max,
        }) => {
            let before = band.clone();
            egui::ComboBox::from_id_salt(id.with("band"))
                .selected_text(band_name(band))
                .show_ui(ui, |ui| {
                    for b in [
                        FftBand::Low,
                        FftBand::LowMid,
                        FftBand::Mid,
                        FftBand::High,
                        FftBand::Level,
                    ] {
                        let n = band_name(&b);
                        ui.selectable_value(band, b, n);
                    }
                });
            changed |= *band != before;
            let mut f =
                |ui: &mut egui::Ui, v: &mut f32, r: std::ops::RangeInclusive<f32>, p: &str| {
                    changed |= ui
                        .add(egui::DragValue::new(v).range(r).speed(0.01).prefix(p))
                        .changed();
                };
            f(ui, gain, 0.0..=16.0, "Gain ");
            f(ui, threshold, 0.0..=1.0, "Threshold ");
            f(ui, attack, 0.0..=5.0, "Attack s ");
            f(ui, release, 0.0..=5.0, "Release s ");
            f(ui, min, 0.0..=1.0, "Min ");
            f(ui, max, 0.0..=1.0, "Max ");
        }
        None => {}
    }
    changed.then_some(a)
}

#[allow(clippy::too_many_arguments)]
fn slider_inner(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: f64,
    default: f64,
    range: (f64, f64),
    anim: Option<&Option<Animation>>,
    step: Option<f64>,
) -> (Option<f64>, Option<Option<Animation>>) {
    let id = ui.id().with(id);
    let mut out = None;
    let mut anim_out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = crate::theme::SP_2.max(ui.spacing().item_spacing.x);
        row_label(ui, label);
        // Value | - | + | slider | reset | animation.
        let editing: Option<String> = ui.data(|d| d.get_temp(id));
        if let Some(mut buf) = editing {
            let e = ui.add(
                egui::TextEdit::singleline(&mut buf)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(crate::theme::PROP_VALUE_W),
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
            let (rect, r) = ui.allocate_exact_size(
                egui::vec2(crate::theme::PROP_VALUE_W, crate::theme::PROP_ROW_H),
                egui::Sense::click(),
            );
            let text = format_value(value);
            r.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text.clone())
            });
            let stroke = if r.hovered() {
                egui::Stroke::new(1.0, crate::theme::BORDER_INPUT)
            } else {
                egui::Stroke::NONE
            };
            let p = ui.painter();
            p.rect_filled(rect, 3.0, crate::theme::BG);
            p.rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
            p.text(
                rect.left_center() + egui::vec2(crate::theme::SP_3, 0.0),
                egui::Align2::LEFT_CENTER,
                &text,
                egui::TextStyle::Monospace.resolve(ui.style()),
                crate::theme::TEXT,
            );
            if r.double_clicked() {
                ui.data_mut(|d| d.insert_temp(id, format_value(value)));
            }
        }
        let step = step.unwrap_or((range.1 - range.0) / 100.0);
        for (icon, name, sign) in [
            (crate::icons::i::MINUS, "Decrease", -1.0),
            (crate::icons::i::PLUS, "Increase", 1.0),
        ] {
            let b = crate::icons::button(ui, icon, name);
            if b.clicked() {
                out = Some((value + sign * step).clamp(range.0, range.1));
            }
        }
        let mut x = value;
        let tail = if anim.is_some() { 84.0 } else { 44.0 };
        // Bounded by what is visible: the content width must never feed back into itself.
        let room = ui
            .available_width()
            .min(ui.clip_rect().right() - ui.cursor().left());
        ui.spacing_mut().slider_width = (room - tail).clamp(48.0, 480.0);
        ui.spacing_mut().slider_rail_height = 4.0;
        let s = egui::Slider::new(&mut x, range.0..=range.1)
            .show_value(false)
            .trailing_fill(true)
            .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.4 });
        if ui.add(s).changed() {
            out = Some(x);
        }
        let reset = reset_to(&Value::Number(value), &Value::Number(default));
        if crate::icons::button_enabled(
            ui,
            reset.is_some(),
            crate::icons::i::ARROW_COUNTER_CLOCKWISE,
            "Reset to default",
        )
        .clicked()
        {
            out = Some(default.clamp(range.0, range.1));
        }
        if let Some(cur) = anim {
            let b = ui.menu_button(
                crate::icons::glyph(if cur.is_some() {
                    crate::icons::i::WAVE_SINE
                } else {
                    crate::icons::i::WAVE_SAWTOOTH
                }),
                |ui| anim_menu(ui, id, cur),
            );
            crate::icons::name(b.response.clone(), "Animate", None);
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
        row_label(ui, label);
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
        row_label(ui, label);
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
