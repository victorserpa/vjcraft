//! Palette chips, per-clip extras menu, "Show in Finder" and the effect clipboard.

use crate::theme;
use crate::widgets::{Kind, btn, choice, toggle};
use eframe::egui;
use std::path::{Path, PathBuf};
use vjc_core::Command;
use vjc_model::{Clip, ClipExtras, DeckId, Effect, LayerId, Source};

/// Row of colour chips plus "None". Returns the pick (`Some(None)` = no colour).
pub fn palette_row(ui: &mut egui::Ui, current: Option<u8>) -> Option<Option<u8>> {
    let mut pick = None;
    ui.horizontal(|ui| {
        let r = ui.selectable_label(current.is_none(), "None");
        if r.clicked() {
            pick = Some(None);
        }
        for i in 1..=vjc_model::PALETTE_LEN {
            let (rect, r) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
            let col = theme::palette(Some(i)).unwrap_or(theme::TEXT);
            ui.painter().rect_filled(rect, theme::RADIUS_SM, col);
            if current == Some(i) {
                ui.painter().rect_stroke(
                    rect.expand(2.0),
                    theme::RADIUS_SM,
                    egui::Stroke::new(theme::FOCUS_W, theme::TEXT),
                    egui::StrokeKind::Outside,
                );
            }
            r.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    true,
                    current == Some(i),
                    format!("Colour {i}"),
                )
            });
            if r.clicked() {
                pick = Some(Some(i));
            }
        }
    });
    pick
}

/// Submenu "Color" with the chips.
pub fn color_menu(ui: &mut egui::Ui, current: Option<u8>) -> Option<Option<u8>> {
    ui.menu_button("Color", |ui| {
        let p = palette_row(ui, current);
        if p.is_some() {
            ui.close();
        }
        p
    })
    .inner
    .flatten()
}

/// Opens a file's folder in the system file manager. Tests use `NoReveal`.
pub trait Reveal {
    fn reveal(&self, path: &Path);
}

/// Finder on macOS (`open -R`), the file manager on the parent folder elsewhere.
pub struct SystemReveal;

/// Program and arguments that reveal `path`.
pub fn reveal_command(path: &Path) -> (&'static str, Vec<PathBuf>) {
    if cfg!(target_os = "macos") {
        ("open", vec!["-R".into(), path.to_path_buf()])
    } else {
        let dir = path.parent().unwrap_or(path).to_path_buf();
        ("xdg-open", vec![dir])
    }
}

impl Reveal for SystemReveal {
    fn reveal(&self, path: &Path) {
        let (prog, args) = reveal_command(path);
        let _ = std::process::Command::new(prog).args(args).spawn();
    }
}

/// Does nothing; records what would have been revealed (tests).
#[derive(Default)]
pub struct NoReveal {
    pub seen: std::sync::Mutex<Vec<PathBuf>>,
}

impl Reveal for NoReveal {
    fn reveal(&self, path: &Path) {
        if let Ok(mut v) = self.seen.lock() {
            v.push(path.to_path_buf());
        }
    }
}

/// What the clip menu asks the app to do beyond editing the project.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipAct {
    Reveal(PathBuf),
    Snapshot,
}

fn fx_clip_id() -> egui::Id {
    egui::Id::new("effect_clipboard")
}

/// Effects copied from a clip, ready to paste (empty when none).
pub fn clipboard(ctx: &egui::Context) -> Vec<Effect> {
    ctx.data(|d| d.get_temp::<Vec<Effect>>(fx_clip_id()))
        .unwrap_or_default()
}

/// Copies with fresh ids, so a pasted chain never shares ids (or persistent history).
pub fn fresh(effects: &[Effect]) -> Vec<Effect> {
    effects
        .iter()
        .map(|e| Effect {
            id: uuid::Uuid::new_v4(),
            ..e.clone()
        })
        .collect()
}

/// Three-way override: layer's setting, forced on, forced off.
pub(crate) fn tri(
    ui: &mut egui::Ui,
    label: &str,
    cur: Option<bool>,
    layer_on: bool,
) -> Option<Option<bool>> {
    let mut pick = None;
    let shown = match cur {
        None => format!("{label}: layer ({})", if layer_on { "on" } else { "off" }),
        Some(true) => format!("{label}: on"),
        Some(false) => format!("{label}: off"),
    };
    ui.menu_button(shown, |ui| {
        for (v, t) in [
            (None, "Follow layer"),
            (Some(true), "On"),
            (Some(false), "Off"),
        ] {
            if choice(ui, cur == v, t).clicked() {
                pick = Some(v);
                ui.close();
            }
        }
    });
    pick
}

/// Entries added to a clip's context menu. Edits go to `cmds`; file and snapshot actions
/// come back for the app to run.
#[allow(clippy::too_many_arguments)]
pub fn clip_menu(
    ui: &mut egui::Ui,
    (deck, layer, column): (DeckId, LayerId, u32),
    clip: &Clip,
    layer_flags: (bool, bool),
    cmds: &mut Vec<Command>,
) -> Option<ClipAct> {
    let mut act = None;
    let x: ClipExtras = clip.extras.clone();
    let set = |extras: ClipExtras| Command::SetClipExtras {
        deck,
        layer,
        column,
        extras,
    };
    if let Some(color) = color_menu(ui, x.color) {
        cmds.push(Command::SetColor {
            target: vjc_core::ColorTarget::Clip {
                deck,
                layer,
                column,
            },
            color,
        });
    }
    let (mut sa, mut sv) = (x.strip_audio, x.strip_video);
    if toggle(ui, &mut sa, "Strip audio").changed() {
        cmds.push(set(ClipExtras {
            strip_audio: sa,
            ..x.clone()
        }));
    }
    if toggle(ui, &mut sv, "Strip video").changed() {
        cmds.push(set(ClipExtras {
            strip_video: sv,
            ..x.clone()
        }));
    }
    if let Some(v) = tri(
        ui,
        "Ignore column trigger",
        x.ignore_column_trigger,
        layer_flags.0,
    ) {
        cmds.push(set(ClipExtras {
            ignore_column_trigger: v,
            ..x.clone()
        }));
    }
    if let Some(v) = tri(ui, "Fader start", x.fader_start, layer_flags.1) {
        cmds.push(set(ClipExtras {
            fader_start: v,
            ..x.clone()
        }));
    }
    ui.separator();
    let target = vjc_core::effects::EffectTarget::Clip {
        deck,
        layer,
        column,
    };
    if btn(ui, "Copy effects", Kind::Secondary).clicked() {
        let fx = clip.track.effects.clone();
        ui.ctx().data_mut(|d| d.insert_temp(fx_clip_id(), fx));
        ui.close();
    }
    let have = clipboard(ui.ctx());
    ui.add_enabled_ui(!have.is_empty(), |ui| {
        if btn(ui, "Paste effects", Kind::Secondary).clicked() {
            cmds.push(Command::ReplaceEffects {
                target,
                effects: fresh(&have),
            });
            ui.close();
        }
    });
    ui.separator();
    if let Source::File { path } = &clip.source
        && btn(ui, "Show in Finder", Kind::Secondary).clicked()
    {
        act = Some(ClipAct::Reveal(path.clone()));
        ui.close();
    }
    if btn(ui, "Snapshot PNG", Kind::Secondary).clicked() {
        act = Some(ClipAct::Snapshot);
        ui.close();
    }
    act
}

/// The menu-bar Clip menu for the selected clip: transport mode, target, trigger style, fader
/// start and ignore column. Every edit is the same command the clip panel sends.
pub fn clip_bar_menu(
    ui: &mut egui::Ui,
    (deck, layer, column): (DeckId, LayerId, u32),
    clip: &Clip,
    layer_flags: (bool, bool),
    seed: vjc_model::Timecode,
    cmds: &mut Vec<Command>,
) {
    use vjc_core::playback::TransportMode;
    let t = &clip.transport;
    ui.menu_button("Transport", |ui| {
        let cur = TransportMode::of(t);
        for m in TransportMode::ALL {
            if choice(ui, cur == m, m.label()).clicked() {
                let (beat_sync, timecode_start) = m.sync_for(t, seed);
                cmds.push(Command::SetClipSync {
                    deck,
                    layer,
                    column,
                    beat_sync,
                    timecode_start,
                });
                ui.close();
            }
        }
    });
    ui.menu_button("Target", |ui| {
        for m in std::iter::once(vjc_model::ClipTarget::Composition)
            .chain(vjc_model::ClipTarget::CHOICES)
        {
            if choice(ui, clip.target == m, m.label()).clicked() {
                cmds.push(Command::SetClipTarget {
                    deck,
                    layer,
                    column,
                    target: m,
                });
                ui.close();
            }
        }
    });
    ui.menu_button("Trigger style", |ui| {
        for m in std::iter::once(vjc_model::TriggerStyle::Composition)
            .chain(vjc_model::TriggerStyle::CHOICES)
        {
            if choice(ui, clip.trigger_style == m, m.label()).clicked() {
                cmds.push(Command::SetClipTriggerStyle {
                    deck,
                    layer,
                    column,
                    style: m,
                });
                ui.close();
            }
        }
    });
    let x = &clip.extras;
    let set = |extras: ClipExtras| Command::SetClipExtras {
        deck,
        layer,
        column,
        extras,
    };
    if let Some(v) = tri(ui, "Fader start", x.fader_start, layer_flags.1) {
        cmds.push(set(ClipExtras {
            fader_start: v,
            ..x.clone()
        }));
    }
    if let Some(v) = tri(ui, "Ignore column", x.ignore_column_trigger, layer_flags.0) {
        cmds.push(set(ClipExtras {
            ignore_column_trigger: v,
            ..x.clone()
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_command_targets_the_file() {
        let (prog, args) = reveal_command(Path::new("/a/b/c.mov"));
        if cfg!(target_os = "macos") {
            assert_eq!(prog, "open");
            assert_eq!(args, vec![PathBuf::from("-R"), PathBuf::from("/a/b/c.mov")]);
        } else {
            assert_eq!(args, vec![PathBuf::from("/a/b")]);
        }
    }

    #[test]
    fn no_reveal_records_instead_of_opening() {
        let r = NoReveal::default();
        r.reveal(Path::new("/x.mov"));
        assert_eq!(r.seen.lock().unwrap().as_slice(), [PathBuf::from("/x.mov")]);
    }

    #[test]
    fn pasted_effects_get_fresh_ids_and_keep_settings() {
        let e = vjc_fx::new_effect("invert").unwrap();
        let copy = fresh(std::slice::from_ref(&e));
        assert_ne!(copy[0].id, e.id);
        assert_eq!(copy[0].kind, e.kind);
        assert_eq!(copy[0].params, e.params);
    }
}
