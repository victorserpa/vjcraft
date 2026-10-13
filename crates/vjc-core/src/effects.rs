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
}

pub fn track_mut(p: &mut Project, t: EffectTarget) -> Result<&mut Track, Error> {
    match t {
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
    }
}

/// The track of a target, read-only (None if the target does not exist).
pub fn track(p: &Project, t: EffectTarget) -> Option<&Track> {
    match t {
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
    }
}

/// The chain of a target, read-only (None if the target does not exist).
pub fn chain(p: &Project, t: EffectTarget) -> Option<&[Effect]> {
    match t {
        EffectTarget::Master => Some(&p.composition.master.effects),
        EffectTarget::Group(g) => p
            .composition
            .groups
            .iter()
            .find(|x| x.id == g)
            .map(|x| x.track.effects.as_slice()),
        EffectTarget::Layer(l) => p
            .composition
            .layers
            .iter()
            .find(|x| x.id == l)
            .map(|x| x.track.effects.as_slice()),
        EffectTarget::Clip {
            deck,
            layer,
            column,
        } => crate::clip_in(p, deck, layer, column).map(|c| c.track.effects.as_slice()),
    }
}

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
            let fx = &mut track_mut(p, target)?.effects;
            if fx.iter().any(|e| e.id == effect.id) {
                return Err(Error::InvalidValue);
            }
            let id = effect.id;
            fx.insert(index.min(fx.len()), effect);
            undoable(Command::RemoveEffect { target, effect: id }, vec![])
        }
        Command::RemoveEffect { target, effect } => {
            let fx = &mut track_mut(p, target)?.effects;
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
            let fx = &mut track_mut(p, target)?.effects;
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
            let fx = &mut track_mut(p, target)?.effects;
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
            let fx = &mut track_mut(p, target)?.effects;
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
            let fx = &mut track_mut(p, target)?.effects;
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
        _ => Err(Error::InvalidValue),
    }
}
