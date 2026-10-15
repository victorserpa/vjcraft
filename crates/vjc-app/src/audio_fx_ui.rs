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

/// The DSP description of a chain at this moment (animations resolved). Effects of unknown
/// kinds (from a newer version) are skipped.
pub fn specs(chain: &[Effect], ctx: &vjc_core::anim::AnimCtx) -> Vec<FxSpec> {
    chain
        .iter()
        .filter_map(|e| {
            let kind = FxKind::from_id(&e.kind)?;
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
        buses.master.set(
            &specs(&comp.master.audio_effects, &self.anim_ctx(&dials)),
            rate,
            bpm,
        );
        let bus_of = |i: usize| if i < MAX_BUSES { i as u32 + 1 } else { 0 };
        for (i, l) in comp.layers.iter().enumerate().take(MAX_BUSES) {
            let dials = self.dials_for(Some(EffectTarget::Layer(l.id)));
            let s = specs(&l.track.audio_effects, &self.anim_ctx(&dials));
            buses.layers[i].set(&s, rate, bpm);
        }
        let clip_specs = |layer, deck, col, clip: &vjc_model::Clip| {
            let t = EffectTarget::Clip {
                deck,
                layer,
                column: col,
            };
            let dials = self.dials_for(Some(t));
            specs(&clip.track.audio_effects, &self.anim_ctx(&dials))
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
            player.set_audio_fx(bus, &clip_specs(*layer, *deck, *col, clip), rate, bpm);
        }
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
                if ui.button(k.label()).clicked() {
                    cmds.push(Command::AddEffect {
                        target,
                        effect: new_effect(k),
                        index: usize::MAX,
                    });
                    ui.close();
                }
            }
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
            egui::Frame::new()
                .fill(theme::EFFECT_BG)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let name = ui.strong(kind.label());
                        name.context_menu(|ui| {
                            for (text, ok, to) in [
                                ("Move up", i > 0, i.wrapping_sub(1)),
                                ("Move down", i + 1 < n, i + 1),
                            ] {
                                if ui.add_enabled(ok, egui::Button::new(text)).clicked() {
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
            for (pi, (p, s)) in e.params.iter().zip(kind.params()).enumerate() {
                let (v, an) = params::param_slider_anim(
                    ui,
                    (e.id, pi),
                    s.name,
                    vjc_core::anim::effective_number(p, &actx),
                    s.default as f64,
                    (s.min as f64, s.max as f64),
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
