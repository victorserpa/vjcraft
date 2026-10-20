//! Audio effect chains in the app: building effects, feeding the mixer each frame, and the
//! chain editor shown under the Audio section of the master, layer and clip panels.

use crate::{State, icons, params, theme};
use eframe::egui;
use vjc_core::{AnimTarget, Command, EffectTarget};
use vjc_media::audio_fx::{FxKind, FxSpec, MAX_BUSES, MAX_PARAMS};
use vjc_model::{Effect, Param, Value};

/// A new instance of `kind` with its default params (all animatable numbers).
pub fn new_effect(kind: FxKind) -> Effect {
    Effect {
        blend: Default::default(),
        id: uuid::Uuid::new_v4(),
        kind: kind.id().into(),
        bypass: false,
        dry_wet: kind.default_mix(),
        params: kind
            .params()
            .iter()
            .map(|s| Param::number(s.name, s.default as f64, Some((s.min as f64, s.max as f64))))
            .collect(),
    }
}

/// A new CLAP effect for a scanned plugin: one ranged number param per plugin parameter, in
/// plugin order, so animation, mapping and the dashboard treat them like any other param.
pub fn new_clap_effect(entry: &vjc_clap::Entry) -> Effect {
    Effect {
        blend: Default::default(),
        id: uuid::Uuid::new_v4(),
        kind: crate::clap_host::kind_of(entry),
        bypass: false,
        dry_wet: 1.0,
        params: entry
            .params
            .iter()
            .map(|p| {
                Param::number(
                    &p.name,
                    p.default,
                    Some((p.min.min(p.max), p.max.max(p.min))),
                )
            })
            .collect(),
    }
}

/// The DSP description of a chain at this moment (animations resolved). Effects of unknown
/// kinds (from a newer version) are skipped. CLAP effects need the host that owns their plugin state: their params are copied to it here.
pub fn specs_with(
    chain: &[Effect],
    ctx: &vjc_core::anim::AnimCtx,
    clap: Option<&crate::clap_host::ClapHost>,
) -> Vec<FxSpec> {
    chain
        .iter()
        .filter_map(|e| {
            let kind = FxKind::from_id(&e.kind)?;
            if kind == FxKind::Clap {
                let shared = clap?.shared(e)?;
                for (i, p) in e.params.iter().enumerate() {
                    let mut v = vjc_core::anim::effective_number(p, ctx);
                    if let Some((lo, hi)) = p.range {
                        v = v.clamp(lo, hi);
                    }
                    shared.set_param(i, v);
                }
                return Some(FxSpec {
                    kind,
                    clap: Some(shared),
                    bypass: e.bypass,
                    mix: e.dry_wet.clamp(0.0, 1.0),
                    params: [0.0; MAX_PARAMS],
                });
            }
            let mut params = [0.0; MAX_PARAMS];
            for (i, s) in kind.params().iter().enumerate() {
                params[i] = match e.params.get(i) {
                    Some(p) => {
                        (vjc_core::anim::effective_number(p, ctx) as f32).clamp(s.min, s.max)
                    }
                    None => s.default,
                };
            }
            Some(FxSpec {
                kind,
                clap: None,
                bypass: e.bypass,
                mix: e.dry_wet.clamp(0.0, 1.0),
                params,
            })
        })
        .collect()
}

impl State {
    /// Pushes the audio chains (clip, layer bus, master) of the project to the mixer.
    pub(crate) fn sync_audio_fx(&self) {
        let Some(engine) = &self.audio else { return };
        let (rate, bpm) = (engine.sample_rate(), self.clock.bpm() as f32);
        let buses = engine.buses();
        let comp = &self.project.composition;
        let dials = self.dials_for(None);
        let host = Some(&self.clap);
        let mut live: Vec<vjc_model::EffectId> =
            comp.master.audio_effects.iter().map(|e| e.id).collect();
        buses.master.set(
            &specs_with(&comp.master.audio_effects, &self.anim_ctx(&dials), host),
            rate,
            bpm,
        );
        let bus_of = |i: usize| if i < MAX_BUSES { i as u32 + 1 } else { 0 };
        for (i, l) in comp.layers.iter().enumerate().take(MAX_BUSES) {
            let dials = self.dials_for(Some(EffectTarget::Layer(l.id)));
            let s = specs_with(&l.track.audio_effects, &self.anim_ctx(&dials), host);
            live.extend(l.track.audio_effects.iter().map(|e| e.id));
            buses.layers[i].set(&s, rate, bpm);
        }
        let clip_specs = |layer, deck, col, clip: &vjc_model::Clip| {
            let t = EffectTarget::Clip {
                deck,
                layer,
                column: col,
            };
            let dials = self.dials_for(Some(t));
            specs_with(&clip.track.audio_effects, &self.anim_ctx(&dials), host)
        };
        for (layer, (deck, col, player)) in &self.players {
            let Some(clip) = crate::clip_for(&self.project, &self.origin, *layer, *deck, *col)
            else {
                continue;
            };
            let bus = comp
                .layers
                .iter()
                .position(|l| l.id == *layer)
                .map_or(0, bus_of);
            live.extend(clip.track.audio_effects.iter().map(|e| e.id));
            player.set_audio_fx(bus, &clip_specs(*layer, *deck, *col, clip), rate, bpm);
        }
        self.clap.retain(&live);
    }

    /// Add menu and the effects of an audio chain; `target` is a `*Audio` target.
    pub(crate) fn audio_chain_ui(
        &self,
        ui: &mut egui::Ui,
        target: EffectTarget,
        cmds: &mut Vec<Command>,
    ) {
        ui.menu_button("Add audio effect", |ui| {
            for k in FxKind::ALL {
                if crate::widgets::btn(ui, k.label(), crate::widgets::Kind::Secondary).clicked() {
                    cmds.push(Command::AddEffect {
                        target,
                        effect: new_effect(k),
                        index: usize::MAX,
                    });
                    ui.close();
                }
            }
            self.clap_menu_group(ui, target, cmds);
        });
        let chain = vjc_core::effects::chain(&self.project, target)
            .unwrap_or_default()
            .to_vec();
        let n = chain.len();
        let dials = self.dials_for(Some(target.base()));
        let actx = self.anim_ctx(&dials);
        for (i, e) in chain.iter().enumerate() {
            let Some(kind) = FxKind::from_id(&e.kind) else {
                continue;
            };
            let clap = (kind == FxKind::Clap).then(|| self.clap_label(e));
            egui::Frame::new()
                .fill(theme::EFFECT_BG)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let name = ui.strong(clap.as_deref().unwrap_or(kind.label()));
                        name.context_menu(|ui| {
                            for (text, ok, to) in [
                                ("Move up", i > 0, i.wrapping_sub(1)),
                                ("Move down", i + 1 < n, i + 1),
                            ] {
                                if crate::widgets::btn_if(
                                    ui,
                                    ok,
                                    text,
                                    crate::widgets::Kind::Secondary,
                                )
                                .clicked()
                                {
                                    cmds.push(Command::MoveEffect {
                                        target,
                                        effect: e.id,
                                        index: to,
                                    });
                                    ui.close();
                                }
                            }
                        });
                        ui.spacing_mut().slider_width = theme::FX_SLIDER_W;
                        if let Some(v) = params::param_slider(
                            ui,
                            (e.id, "mix"),
                            "Mix",
                            e.dry_wet as f64,
                            kind.default_mix() as f64,
                            (0.0, 1.0),
                        ) {
                            cmds.push(Command::SetEffectDryWet {
                                target,
                                effect: e.id,
                                dry_wet: v as f32,
                            });
                        }
                        let by = icons::letter(
                            ui,
                            "B",
                            "Bypass effect",
                            theme::FX_BTN.y,
                            Some(e.bypass),
                        );
                        if by.clicked() {
                            cmds.push(Command::SetEffectBypass {
                                target,
                                effect: e.id,
                                bypass: !e.bypass,
                            });
                        }
                        if icons::letter(ui, "\u{00d7}", "Remove effect", theme::FX_BTN.y, None)
                            .clicked()
                        {
                            cmds.push(Command::RemoveEffect {
                                target,
                                effect: e.id,
                            });
                        }
                    });
                });
            if clap.is_some() {
                ui.horizontal(|ui| self.clap_badge(ui, e));
            }
            // built-in effects: ranges from the DSP catalog; CLAP effects: from the plugin
            let rows: Vec<(String, f64, (f64, f64))> = if clap.is_some() {
                e.params
                    .iter()
                    .map(|p| {
                        let d = match p.default {
                            Value::Number(n) => n,
                            _ => 0.0,
                        };
                        (p.name.clone(), d, p.range.unwrap_or((0.0, 1.0)))
                    })
                    .collect()
            } else {
                kind.params()
                    .iter()
                    .map(|s| {
                        (
                            s.name.to_string(),
                            s.default as f64,
                            (s.min as f64, s.max as f64),
                        )
                    })
                    .collect()
            };
            for (pi, (p, (name, default, range))) in e.params.iter().zip(rows).enumerate() {
                let (v, an) = params::param_slider_anim(
                    ui,
                    (e.id, pi),
                    &name,
                    vjc_core::anim::effective_number(p, &actx),
                    default,
                    range,
                    &p.animation,
                );
                if let Some(value) = v {
                    cmds.push(Command::SetEffectParam {
                        target,
                        effect: e.id,
                        param: pi,
                        value: Value::Number(value),
                    });
                }
                if let Some(animation) = an {
                    cmds.push(Command::SetAnimation {
                        target: AnimTarget::Effect {
                            target,
                            effect: e.id,
                            param: pi,
                        },
                        animation,
                    });
                }
            }
        }
    }
}

impl State {
    /// Display name of a CLAP effect: the scanned plugin name, else its id.
    fn clap_label(&self, e: &Effect) -> String {
        let Some((id, _)) = crate::clap_host::parse_kind(&e.kind) else {
            return e.kind.clone();
        };
        self.clap
            .entries()
            .into_iter()
            .find(|en| en.id == id)
            .map_or_else(|| id.to_string(), |en| en.name)
    }

    /// Status of a CLAP effect: running, or bypassed (crash, over budget, not usable) with a
    /// Restart button.
    fn clap_badge(&self, ui: &mut egui::Ui, e: &Effect) {
        let Some(shared) = self.clap.shared(e) else {
            return;
        };
        use crate::clap_host::Badge;
        match self.clap.badge(e) {
            Some(Badge::Running) => {
                ui.label(egui::RichText::new("Running").color(theme::TEXT_MUTED));
            }
            Some(Badge::Idle) => {
                ui.label(egui::RichText::new("Idle").color(theme::TEXT_MUTED))
                    .on_hover_text("The plugin loads when the clip plays");
            }
            Some(Badge::Missing) if self.clap.scanning() => {
                ui.label(egui::RichText::new("Looking for plugin").color(theme::TEXT_MUTED));
            }
            Some(Badge::Missing) => {
                ui.label(egui::RichText::new("Plugin not found").color(theme::WARN))
                    .on_hover_text(
                        "This plugin is not in the scanned list. Check the plugin folders in Prefs and rescan.",
                    );
            }
            Some(Badge::Bypassed(why)) => {
                ui.label(egui::RichText::new("Bypassed").color(theme::WARN))
                    .on_hover_text(why);
                if crate::widgets::btn(ui, "Restart", crate::widgets::Kind::Secondary)
                    .on_hover_text("Start the plugin again")
                    .clicked()
                {
                    shared.restart();
                }
            }
            None => {}
        }
    }

    /// The "CLAP plugins" group of the Add audio effect menu.
    fn clap_menu_group(&self, ui: &mut egui::Ui, target: EffectTarget, cmds: &mut Vec<Command>) {
        let entries = self.clap.entries();
        if entries.is_empty() && !self.clap.scanning() {
            return;
        }
        ui.separator();
        ui.label(egui::RichText::new("CLAP plugins").color(theme::TEXT_MUTED));
        if entries.is_empty() {
            ui.label("Scanning...");
        }
        for en in entries {
            if en.stereo {
                if crate::widgets::btn(ui, &en.name, crate::widgets::Kind::Secondary).clicked() {
                    cmds.push(Command::AddEffect {
                        target,
                        effect: new_clap_effect(&en),
                        index: usize::MAX,
                    });
                    ui.close();
                }
            } else {
                crate::widgets::btn_if(
                    ui,
                    false,
                    &format!("{} (no audio port)", en.name),
                    crate::widgets::Kind::Secondary,
                )
                .on_disabled_hover_text(
                    "This plugin has no audio input and output, so it cannot be used as an effect here.",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_effects_match_the_dsp_catalog() {
        for k in FxKind::ALL {
            let e = new_effect(k);
            assert_eq!(e.params.len(), k.params().len());
            assert_eq!(FxKind::from_id(&e.kind), Some(k));
        }
    }
}
