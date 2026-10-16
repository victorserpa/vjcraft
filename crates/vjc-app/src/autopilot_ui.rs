//! Composition autopilot controls: the "Autopilot" section of the Composition panel and the
//! per-column override in the column menu. Look values come from `widgets` / `theme`.

use crate::params;
use crate::widgets;
use eframe::egui;
use vjc_core::Command;
use vjc_model::{ColumnAutopilot, ColumnDuration, ColumnTarget, CompAutopilot, RandomKind};

pub fn target_label(t: ColumnTarget) -> &'static str {
    match t {
        ColumnTarget::Off => "Off",
        ColumnTarget::Next => "Next column",
        ColumnTarget::Previous => "Previous column",
        ColumnTarget::Random => "Random column",
    }
}

pub fn duration_label(d: ColumnDuration) -> &'static str {
    match d {
        ColumnDuration::Beats => "Beats",
        ColumnDuration::Seconds => "Seconds",
        ColumnDuration::ShortestClip => "Shortest clip",
        ColumnDuration::LongestClip => "Longest clip",
        ColumnDuration::TopClip => "Top clip",
        ColumnDuration::BottomClip => "Bottom clip",
    }
}

pub fn random_label(r: RandomKind) -> &'static str {
    match r {
        RandomKind::Any => "Any",
        RandomKind::Other => "Other",
        RandomKind::Bag => "Bag",
    }
}

const TARGETS: [ColumnTarget; 4] = [
    ColumnTarget::Off,
    ColumnTarget::Next,
    ColumnTarget::Previous,
    ColumnTarget::Random,
];
const DURATIONS: [ColumnDuration; 6] = [
    ColumnDuration::Beats,
    ColumnDuration::Seconds,
    ColumnDuration::ShortestClip,
    ColumnDuration::LongestClip,
    ColumnDuration::TopClip,
    ColumnDuration::BottomClip,
];
const RANDOMS: [RandomKind; 3] = [RandomKind::Any, RandomKind::Other, RandomKind::Bag];

/// Random kind and loop switch of a layer's autopilot (same options as the composition's).
pub fn layer_extras(
    ui: &mut egui::Ui,
    layer: vjc_model::LayerId,
    ap: &vjc_model::Autopilot,
    cmds: &mut Vec<Command>,
) {
    use vjc_model::AutopilotMode as M;
    if ap.mode == M::Random
        && let Some(random) = params::combo(
            ui,
            ("laprk", layer),
            "Random",
            &ap.random,
            &opts(&RANDOMS, random_label),
        )
    {
        cmds.push(Command::SetLayerAutopilot {
            layer,
            autopilot: vjc_model::Autopilot {
                random,
                ..ap.clone()
            },
        });
    }
    if matches!(ap.mode, M::Next | M::Previous) {
        let mut looping = ap.looping;
        let changed = ui
            .horizontal(|ui| {
                params::row_label(ui, "Loop");
                crate::widgets::toggle(ui, &mut looping, "").changed()
            })
            .inner;
        if changed {
            cmds.push(Command::SetLayerAutopilot {
                layer,
                autopilot: vjc_model::Autopilot {
                    looping,
                    ..ap.clone()
                },
            });
        }
        widgets::help(ui, "Loop off stops at the first or last clip.");
    }
}

fn opts<T: Copy>(all: &[T], label: fn(T) -> &'static str) -> Vec<(T, String)> {
    all.iter().map(|v| (*v, label(*v).to_string())).collect()
}

/// A drag field for a positive amount; true when it changed.
fn amount(ui: &mut egui::Ui, label: &str, v: &mut f64, range: (f32, f32)) -> bool {
    let mut x = *v as f32;
    let mut changed = false;
    ui.horizontal(|ui| {
        params::row_label(ui, label);
        changed = widgets::field_num(ui, &mut x, range.0..=range.1, 0.25).changed();
    });
    if changed {
        *v = f64::from(x);
    }
    changed
}

/// Beats / seconds field that applies to `duration`; true when edited.
fn amount_for(ui: &mut egui::Ui, d: ColumnDuration, beats: &mut f64, secs: &mut f64) -> bool {
    match d {
        ColumnDuration::Beats => amount(ui, "Beats", beats, (0.015_625, 1_048_576.0)),
        ColumnDuration::Seconds => amount(ui, "Seconds", secs, (0.001, 604_800.0)),
        _ => {
            widgets::help(ui, "Uses the length of the clips in the column.");
            false
        }
    }
}

/// Body of the Composition panel section.
pub fn comp_section(ui: &mut egui::Ui, a: &CompAutopilot, cmds: &mut Vec<Command>) {
    let mut n = a.clone();
    let mut edited = false;
    if let Some(t) = params::combo(
        ui,
        "apt",
        "Target",
        &a.target,
        &opts(&TARGETS, target_label),
    ) {
        n.target = t;
        edited = true;
    }
    if let Some(d) = params::combo(
        ui,
        "apd",
        "Duration",
        &a.duration,
        &opts(&DURATIONS, duration_label),
    ) {
        n.duration = d;
        edited = true;
    }
    edited |= amount_for(ui, n.duration, &mut n.beats, &mut n.seconds);
    if a.target == ColumnTarget::Random
        && let Some(r) = params::combo(
            ui,
            "apr",
            "Random",
            &a.random,
            &opts(&RANDOMS, random_label),
        )
    {
        n.random = r;
        edited = true;
    }
    ui.horizontal(|ui| {
        params::row_label(ui, "Loop");
        edited |= crate::widgets::toggle(ui, &mut n.looping, "")
            .on_hover_text("Loop off stops at the first or last column.")
            .changed();
        ui.label("Compensate transition");
        edited |= crate::widgets::toggle(ui, &mut n.compensate_transition, "")
            .on_hover_text(
                "Compensate transition: starts the next column early by the longest layer transition, so the crossing ends when the time is up.",
            )
            .changed();
    });
    if edited {
        cmds.push(Command::SetCompAutopilot { autopilot: n });
    }
}

/// Column menu entry: "Composition" or an own target / duration.
pub fn column_menu(
    ui: &mut egui::Ui,
    column: uuid::Uuid,
    a: &ColumnAutopilot,
    cmds: &mut Vec<Command>,
) {
    let mut n = a.clone();
    let mut edited = false;
    widgets::section(ui, "Autopilot", |ui| {
        let mut t_opts: Vec<(Option<ColumnTarget>, String)> = vec![(None, "Composition".into())];
        t_opts.extend(
            TARGETS
                .iter()
                .map(|t| (Some(*t), target_label(*t).to_string())),
        );
        if let Some(t) = params::combo(ui, "cap-t", "Target", &a.target, &t_opts) {
            n.target = t;
            edited = true;
        }
        let mut d_opts: Vec<(Option<ColumnDuration>, String)> = vec![(None, "Composition".into())];
        d_opts.extend(
            DURATIONS
                .iter()
                .map(|d| (Some(*d), duration_label(*d).to_string())),
        );
        if let Some(d) = params::combo(ui, "cap-d", "Duration", &a.duration, &d_opts) {
            n.duration = d;
            edited = true;
        }
        if let Some(d) = n.duration {
            edited |= amount_for(ui, d, &mut n.beats, &mut n.seconds);
        }
    });
    if edited {
        cmds.push(Command::SetColumnAutopilot {
            column,
            autopilot: n,
        });
    }
}

/// Beat snap selector: `none_label` stands for "no override" (the level above decides);
/// the divisions follow. Returns the new value when the user picks one.
pub fn snap_combo(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    current: Option<vjc_model::BeatSnap>,
    none_label: &str,
) -> Option<Option<vjc_model::BeatSnap>> {
    let mut opts: Vec<(Option<vjc_model::BeatSnap>, String)> = vec![(None, none_label.into())];
    opts.extend(
        vjc_model::BeatSnap::ALL
            .into_iter()
            .map(|s| (Some(s), s.label().to_string())),
    );
    params::combo(ui, id, label, &current, &opts)
}

/// Column menu entry for the column's beat snap ("Composition" = no override).
pub fn column_snap_menu(
    ui: &mut egui::Ui,
    column: uuid::Uuid,
    current: Option<vjc_model::BeatSnap>,
    cmds: &mut Vec<Command>,
) {
    widgets::section(ui, "Beat snap", |ui| {
        if let Some(snap) = snap_combo(ui, "cbs", "Snap", current, "Composition") {
            cmds.push(Command::SetColumnBeatSnap { column, snap });
        }
    });
}
