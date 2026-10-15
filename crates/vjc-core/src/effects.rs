//! Effect chain commands: add, remove, move, bypass and param edits on a clip, a layer or
//! the composition (master) track.

use super::{Applied, Command, Error, check_cell, deck_mut, layer_idx, undoable};
use vjc_model::{ColumnIdx, DeckId, Effect, EffectId, GroupId, LayerId, Project, Track, Value};

/// Whose chain a command edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectTarget {
    Clip {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
    Layer(LayerId),
    /// Chain run over the composed result of a layer group.
    Group(GroupId),
    Master,
    /// Audio chain of a clip (processed on the audio thread, not drawn).
    ClipAudio {
        deck: DeckId,
        layer: LayerId,
        column: ColumnIdx,
    },
    /// Audio chain of a layer: its clips summed.
    LayerAudio(LayerId),
    /// Audio chain of the master output.
    MasterAudio,
}

impl EffectTarget {
    /// True for the audio chains.
    pub fn is_audio(self) -> bool {
        matches!(
            self,
            EffectTarget::ClipAudio { .. }
                | EffectTarget::LayerAudio(_)
                | EffectTarget::MasterAudio
        )
    }

    /// The track that owns the chain (the video target of an audio one).
    pub fn base(self) -> EffectTarget {
        match self {
            EffectTarget::ClipAudio {
                deck,
                layer,
                column,
            } => EffectTarget::Clip {
                deck,
                layer,
                column,
            },
            EffectTarget::LayerAudio(l) => EffectTarget::Layer(l),
            EffectTarget::MasterAudio => EffectTarget::Master,
            t => t,
        }
    }
}

/// The effect list a command edits: video effects, or the audio ones of audio targets.
pub fn fx_mut(p: &mut Project, t: EffectTarget) -> Result<&mut Vec<Effect>, Error> {
    let tr = track_mut(p, t.base())?;
    Ok(if t.is_audio() {
        &mut tr.audio_effects
    } else {
        &mut tr.effects
    })
}

pub fn track_mut(p: &mut Project, t: EffectTarget) -> Result<&mut Track, Error> {
    match t.base() {
        EffectTarget::Master => Ok(&mut p.composition.master),
        EffectTarget::Group(g) => {
            let i = crate::groups::group_idx(p, g)?;
            Ok(&mut p.composition.groups[i].track)
        }
        EffectTarget::Layer(l) => {
            let i = layer_idx(p, l)?;
            Ok(&mut p.composition.layers[i].track)
        }
        EffectTarget::Clip {
            deck,
            layer,
            column,
        } => {
            check_cell(p, layer, column)?;
            deck_mut(p, deck)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .map(|k| &mut k.clip.track)
                .ok_or(Error::NoClip)
        }
        _ => Err(Error::InvalidValue),
    }
}

/// The track of a target, read-only (None if the target does not exist).
pub fn track(p: &Project, t: EffectTarget) -> Option<&Track> {
    match t.base() {
        EffectTarget::Master => Some(&p.composition.master),
        EffectTarget::Group(g) => p.composition.group(g).map(|x| &x.track),
        EffectTarget::Layer(l) => p
            .composition
            .layers
            .iter()
            .find(|x| x.id == l)
            .map(|x| &x.track),
        EffectTarget::Clip {
            deck,
            layer,
            column,
        } => crate::clip_in(p, deck, layer, column).map(|c| &c.track),
        _ => None,
    }
}

/// The chain of a target, read-only (None if the target does not exist).
pub fn chain(p: &Project, t: EffectTarget) -> Option<&[Effect]> {
    track(p, t).map(|x| {
        if t.is_audio() {
            x.audio_effects.as_slice()
        } else {
            x.effects.as_slice()
        }
    })
}

/// Longest string a text parameter keeps (bad data guard; the shader reads fewer).
pub const MAX_TEXT_CHARS: usize = 256;

fn pos(fx: &[Effect], id: EffectId) -> Result<usize, Error> {
    fx.iter().position(|e| e.id == id).ok_or(Error::NoEffect)
}

pub(crate) fn apply(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
    match cmd {
        Command::AddEffect {
            target,
            effect,
            index,
        } => {
            let fx = fx_mut(p, target)?;
            if fx.iter().any(|e| e.id == effect.id) {
                return Err(Error::InvalidValue);
            }
            let id = effect.id;
            fx.insert(index.min(fx.len()), effect);
            undoable(Command::RemoveEffect { target, effect: id }, vec![])
        }
        Command::RemoveEffect { target, effect } => {
            let fx = fx_mut(p, target)?;
            let i = pos(fx, effect)?;
            let removed = fx.remove(i);
            undoable(
                Command::AddEffect {
                    target,
                    effect: removed,
                    index: i,
                },
                vec![],
            )
        }
        Command::MoveEffect {
            target,
            effect,
            index,
        } => {
            let fx = fx_mut(p, target)?;
            let i = pos(fx, effect)?;
            let e = fx.remove(i);
            fx.insert(index.min(fx.len()), e);
            undoable(
                Command::MoveEffect {
                    target,
                    effect,
                    index: i,
                },
                vec![],
            )
        }
        Command::SetEffectBypass {
            target,
            effect,
            bypass,
        } => {
            let fx = fx_mut(p, target)?;
            let i = pos(fx, effect)?;
            let prev = std::mem::replace(&mut fx[i].bypass, bypass);
            undoable(
                Command::SetEffectBypass {
                    target,
                    effect,
                    bypass: prev,
                },
                vec![],
            )
        }
        Command::SetEffectDryWet {
            target,
            effect,
            dry_wet,
        } => {
            if !dry_wet.is_finite() {
                return Err(Error::InvalidValue);
            }
            let fx = fx_mut(p, target)?;
            let i = pos(fx, effect)?;
            let prev = std::mem::replace(&mut fx[i].dry_wet, dry_wet.clamp(0.0, 1.0));
            undoable(
                Command::SetEffectDryWet {
                    target,
                    effect,
                    dry_wet: prev,
                },
                vec![],
            )
        }
        Command::SetEffectParam {
            target,
            effect,
            param,
            value,
        } => {
            let fx = fx_mut(p, target)?;
            let i = pos(fx, effect)?;
            let prm = fx[i].params.get_mut(param).ok_or(Error::InvalidValue)?;
            if std::mem::discriminant(&prm.value) != std::mem::discriminant(&value) {
                return Err(Error::InvalidValue);
            }
            let prev = prm.value.clone();
            match value {
                Value::Number(n) if n.is_finite() => prm.set_number(n),
                Value::Number(_) => return Err(Error::InvalidValue),
                Value::Choice { selected, options } => {
                    // Selection only; the option list belongs to the effect.
                    let n = match &prm.value {
                        Value::Choice { options, .. } => options.len() as u32,
                        _ => 0,
                    };
                    let _ = options;
                    if selected >= n {
                        return Err(Error::InvalidValue);
                    }
                    if let Value::Choice { selected: s, .. } = &mut prm.value {
                        *s = selected;
                    }
                }
                Value::Text(t) => {
                    prm.value = Value::Text(t.chars().take(MAX_TEXT_CHARS).collect());
                }
                v => prm.value = v,
            }
            undoable(
                Command::SetEffectParam {
                    target,
                    effect,
                    param,
                    value: prev,
                },
                vec![],
            )
        }
        Command::SetEffectParams {
            target,
            effect,
            params,
        } => {
            let fx = fx_mut(p, target)?;
            let i = pos(fx, effect)?;
            let prev = fx[i].params.clone();
            for new in params {
                let Some(prm) = fx[i].params.iter_mut().find(|q| q.name == new.name) else {
                    continue;
                };
                merge_param(prm, new);
            }
            undoable(
                Command::SetEffectParams {
                    target,
                    effect,
                    params: prev,
                },
                vec![],
            )
        }
        _ => Err(Error::InvalidValue),
    }
}

/// Copies the value and animation of `new` onto `prm` when they fit it; what does not fit
/// (other kind, bad number, choice out of range) is left as it was.
fn merge_param(prm: &mut vjc_model::Param, new: vjc_model::Param) {
    let accepted = match (&mut prm.value, new.value) {
        (Value::Number(_), Value::Number(n)) if n.is_finite() => {
            prm.set_number(n);
            true
        }
        (Value::Bool(b), Value::Bool(n)) => {
            *b = n;
            true
        }
        (Value::Text(t), Value::Text(n)) => {
            *t = n.chars().take(MAX_TEXT_CHARS).collect();
            true
        }
        (
            Value::Choice {
                selected, options, ..
            },
            Value::Choice { selected: n, .. },
        ) if (n as usize) < options.len() => {
            *selected = n;
            true
        }
        (Value::Color(c), Value::Color(n)) if n.iter().all(|x| x.is_finite()) => {
            *c = n.map(|x| x.clamp(0.0, 1.0));
            true
        }
        _ => false,
    };
    if accepted && matches!(prm.value, Value::Number(_)) {
        prm.animation = new.animation;
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use vjc_model::{Animation, Param};

    fn anim() -> Animation {
        Animation::Dashboard {
            dial: 1,
            scope: Default::default(),
        }
    }

    #[test]
    fn accepted_value_takes_animation() {
        let mut prm = Param::number("x", 0.5, Some((0.0, 1.0)));
        let mut new = Param::number("x", 0.7, Some((0.0, 1.0)));
        new.animation = Some(anim());
        merge_param(&mut prm, new);
        assert_eq!(prm.animation, Some(anim()));
    }

    #[test]
    fn skipped_value_keeps_live_animation() {
        for bad in [Value::Number(f64::NAN), Value::Bool(true)] {
            let mut prm = Param::number("x", 0.5, Some((0.0, 1.0)));
            prm.animation = Some(anim());
            let mut new = Param::number("x", 0.0, Some((0.0, 1.0)));
            new.value = bad;
            new.animation = None;
            merge_param(&mut prm, new);
            assert_eq!(prm.animation, Some(anim()));
        }
    }
}
