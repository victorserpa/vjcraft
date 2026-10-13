//! Mappable shortcuts: VJCraft's own address space ("/comp/layer/1/opacity") and the pure
//! `resolve` that turns an address plus a value into a `Command`. Layers are numbered from
//! the bottom (Layer 1 is the last one listed), columns from 1, on the current deck.

use super::effects::{self, EffectTarget};
use super::{Applied, Command, Error, number, undoable};
use std::time::Instant;
use uuid::Uuid;
use vjc_model::{
    ColumnIdx, ControlSource, DeckId, Effect, LayerId, Project, Shortcut, ShortcutMode, Value,
};

/// Runtime state `resolve` needs and the project does not hold.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub col_sel: Option<ColumnIdx>,
    pub master_bypass: bool,
    pub now: Instant,
    /// Clip playing on each layer; "/comp/layer/N/clip/effect/..." addresses follow it.
    pub active: Vec<(LayerId, DeckId, ColumnIdx)>,
}

impl Ctx {
    pub fn new(col_sel: Option<ColumnIdx>, master_bypass: bool) -> Self {
        Ctx {
            col_sel,
            master_bypass,
            now: Instant::now(),
            active: vec![],
        }
    }

    pub fn with_active(mut self, active: Vec<(LayerId, DeckId, ColumnIdx)>) -> Self {
        self.active = active;
        self
    }
}

/// Effect addresses: "/comp/effect/K/..." (master), "/comp/layer/N/effect/K/..." (layer) and
/// "/comp/layer/N/clip/effect/K/..." (clip playing on the layer); K counts from 1 in chain
/// order. Tails: "param/NAME", "drywet", "bypass".
pub fn effect_address(
    p: &Project,
    target: EffectTarget,
    k: usize,
    tail: &str,
    active: Option<(DeckId, ColumnIdx)>,
) -> Option<String> {
    match target {
        EffectTarget::Master => Some(format!("/comp/effect/{}/{tail}", k + 1)),
        EffectTarget::Layer(l) => layer_address(p, l, &format!("effect/{}/{tail}", k + 1)),
        EffectTarget::Clip {
            deck,
            layer,
            column,
        } => (active == Some((deck, column)))
            .then(|| layer_address(p, layer, &format!("clip/effect/{}/{tail}", k + 1)))?,
    }
}

enum FxTail<'a> {
    Param(&'a str),
    DryWet,
    Bypass,
}

/// Splits an effect address into its chain target, effect and tail. Outer None: not an
/// effect address; inner None: an effect address that points at nothing.
#[allow(clippy::type_complexity)]
fn parse_fx<'a>(
    p: &'a Project,
    ctx: &Ctx,
    parts: &[&'a str],
) -> Option<Option<(EffectTarget, &'a Effect, FxTail<'a>)>> {
    let (scope, k, rest): (Option<EffectTarget>, &str, &[&'a str]) = match parts {
        ["comp", "effect", k, rest @ ..] => (Some(EffectTarget::Master), k, rest),
        ["comp", "layer", n, "effect", k, rest @ ..] => (
            number_of(n)
                .and_then(|n| layer_by_number(p, n))
                .map(EffectTarget::Layer),
            k,
            rest,
        ),
        ["comp", "layer", n, "clip", "effect", k, rest @ ..] => (
            number_of(n)
                .and_then(|n| layer_by_number(p, n))
                .and_then(|layer| {
                    let (_, deck, column) = ctx.active.iter().find(|a| a.0 == layer)?;
                    Some(EffectTarget::Clip {
                        deck: *deck,
                        layer,
                        column: *column,
                    })
                }),
            k,
            rest,
        ),
        _ => return None,
    };
    let tail = match rest {
        ["param", name] => FxTail::Param(name),
        ["drywet"] => FxTail::DryWet,
        ["bypass"] => FxTail::Bypass,
        _ => return Some(None),
    };
    Some((|| {
        let target = scope?;
        let i = number_of(k)?.checked_sub(1)?;
        let e = effects::chain(p, target)?.get(i)?;
        Some((target, e, tail))
    })())
}

/// Normalized (0..1) reading of a param value; None for kinds that cannot be mapped.
fn param_norm(v: &vjc_model::Param) -> Option<f64> {
    match &v.value {
        Value::Number(n) => Some(match v.range {
            Some((lo, hi)) if hi > lo => ((n - lo) / (hi - lo)).clamp(0.0, 1.0),
            _ => n.clamp(0.0, 1.0),
        }),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Choice { selected, options } => Some(if options.len() > 1 {
            f64::from(*selected) / (options.len() - 1) as f64
        } else {
            0.0
        }),
        _ => None,
    }
}

/// The value a normalized `x` stands for in `prm`'s own type and range.
fn param_value(prm: &vjc_model::Param, x: f64) -> Option<Value> {
    let x = if x.is_finite() { x } else { return None };
    match &prm.value {
        Value::Number(_) => Some(Value::Number(match prm.range {
            Some((lo, hi)) => lo + x.clamp(0.0, 1.0) * (hi - lo),
            None => x.clamp(0.0, 1.0),
        })),
        Value::Bool(_) => Some(Value::Bool(x >= 0.5)),
        Value::Choice { options, .. } if !options.is_empty() => Some(Value::Choice {
            selected: (x.clamp(0.0, 1.0) * (options.len() - 1) as f64).round() as u32,
            options: options.clone(),
        }),
        _ => None,
    }
}

fn fx_current(p: &Project, ctx: &Ctx, parts: &[&str]) -> Option<Option<f64>> {
    let r = parse_fx(p, ctx, parts)?;
    Some(r.and_then(|(_, e, tail)| match tail {
        FxTail::Bypass => Some(f64::from(u8::from(e.bypass))),
        FxTail::DryWet => Some(f64::from(e.dry_wet)),
        FxTail::Param(name) => param_norm(e.params.iter().find(|q| q.name == name)?),
    }))
}

fn fx_resolve(p: &Project, ctx: &Ctx, parts: &[&str], value: f64) -> Option<Option<Command>> {
    let r = parse_fx(p, ctx, parts)?;
    Some(r.and_then(|(target, e, tail)| {
        let effect = e.id;
        match tail {
            FxTail::Bypass => value.is_finite().then_some(Command::SetEffectBypass {
                target,
                effect,
                bypass: value >= 0.5,
            }),
            FxTail::DryWet => value.is_finite().then(|| Command::SetEffectDryWet {
                target,
                effect,
                dry_wet: value.clamp(0.0, 1.0) as f32,
            }),
            FxTail::Param(name) => {
                let param = e.params.iter().position(|q| q.name == name)?;
                Some(Command::SetEffectParam {
                    target,
                    effect,
                    param,
                    value: param_value(&e.params[param], value)?,
                })
            }
        }
    }))
}

/// True for targets a knob sweeps (opacity and effect params / dry-wet) rather than flips.
pub fn is_continuous(address: &str) -> bool {
    address.ends_with("/opacity")
        || address.ends_with("/drywet")
        || (address.contains("/effect/") && address.contains("/param/"))
}

pub fn layer_address(p: &Project, layer: LayerId, tail: &str) -> Option<String> {
    let ls = &p.composition.layers;
    let i = ls.iter().position(|l| l.id == layer)?;
    Some(format!("/comp/layer/{}/{tail}", ls.len() - i))
}

pub fn clip_address(p: &Project, layer: LayerId, column: ColumnIdx) -> Option<String> {
    layer_address(p, layer, &format!("clip/{}/connect", column + 1))
}

pub fn column_address(column: ColumnIdx) -> String {
    format!("/comp/column/{}/connect", column + 1)
}

fn layer_by_number(p: &Project, n: usize) -> Option<LayerId> {
    let ls = &p.composition.layers;
    (n >= 1 && n <= ls.len()).then(|| ls[ls.len() - n].id)
}

fn number_of(s: &str) -> Option<usize> {
    s.parse().ok()
}

/// Current value of a continuous or boolean address (None for triggers).
pub fn current(p: &Project, ctx: &Ctx, address: &str) -> Option<f64> {
    let parts: Vec<&str> = address.trim_start_matches('/').split('/').collect();
    if let Some(r) = fx_current(p, ctx, &parts) {
        return r;
    }
    match parts.as_slice() {
        ["comp", "layer", n, "opacity"] => {
            let l = layer_by_number(p, number_of(n)?)?;
            let l = p.composition.layers.iter().find(|x| x.id == l)?;
            Some(number(&l.track.opacity.value))
        }
        ["comp", "master", "opacity"] => Some(number(&p.composition.master.opacity.value)),
        ["comp", "master", "bypass"] => Some(f64::from(u8::from(ctx.master_bypass))),
        ["comp", "tempo", "bpm"] => Some(p.composition.tempo.bpm),
        ["comp", "crossfader"] => Some(
            p.composition
                .crossfader
                .as_ref()
                .map_or(super::crossfader::CENTER, |c| number(&c.position.value)),
        ),
        ["comp", "dashboard", n] => {
            let i = number_of(n)?
                .checked_sub(1)
                .filter(|i| *i < super::anim::DIALS)?;
            Some(
                p.composition
                    .dashboard
                    .get(i)
                    .map_or(0.0, |d| number(&d.value)),
            )
        }
        _ => None,
    }
}

/// Pure: the command for `value` at `address`, or None for an unknown address / bad index /
/// a trigger address with value <= 0. Continuous addresses clamp the value to their range.
pub fn resolve_with(p: &Project, ctx: &Ctx, address: &str, value: f64) -> Option<Command> {
    let deck = p.composition.current_deck;
    let parts: Vec<&str> = address.trim_start_matches('/').split('/').collect();
    let fire = value > 0.0;
    if let Some(r) = fx_resolve(p, ctx, &parts, value) {
        return r;
    }
    match parts.as_slice() {
        ["comp", "layer", n, "clip", m, "connect"] => {
            let layer = layer_by_number(p, number_of(n)?)?;
            let column = ColumnIdx::try_from(number_of(m)?.checked_sub(1)?).ok()?;
            fire.then_some(Command::TriggerClip {
                deck,
                layer,
                column,
            })
        }
        ["comp", "layer", n, "opacity"] => Some(Command::SetOpacity {
            layer: layer_by_number(p, number_of(n)?)?,
            value: value.clamp(0.0, 1.0),
        }),
        ["comp", "layer", n, "clear"] => fire.then_some(Command::ClearLayer {
            layer: layer_by_number(p, number_of(n)?)?,
        }),
        ["comp", "column", "next"] => fire.then_some(Command::StepColumn {
            deck,
            from: ctx.col_sel,
            delta: 1,
        }),
        ["comp", "column", "prev"] => fire.then_some(Command::StepColumn {
            deck,
            from: ctx.col_sel,
            delta: -1,
        }),
        ["comp", "column", "selected", "connect"] => fire.then_some(Command::TriggerColumn {
            deck,
            column: ctx.col_sel.unwrap_or(0),
        }),
        ["comp", "column", m, "connect"] => {
            let column = ColumnIdx::try_from(number_of(m)?.checked_sub(1)?).ok()?;
            fire.then_some(Command::TriggerColumn { deck, column })
        }
        ["comp", "tempo", "tap"] => fire.then_some(Command::Tap { at: ctx.now }),
        ["comp", "tempo", "bpm"] => Some(Command::SetBpm {
            bpm: value.clamp(super::clock::MIN_BPM, super::clock::MAX_BPM),
        }),
        ["comp", "master", "bypass"] => Some(Command::SetMasterBypass {
            bypass: value >= 0.5,
        }),
        ["comp", "crossfader"] => Some(Command::SetCrossfader {
            value: value.clamp(0.0, 1.0),
        }),
        ["comp", "master", "opacity"] => Some(Command::SetMasterOpacity {
            value: value.clamp(0.0, 1.0),
        }),
        ["comp", "dashboard", n] => {
            let dial = number_of(n)?
                .checked_sub(1)
                .filter(|i| *i < super::anim::DIALS)?;
            Some(Command::SetDashboardDial {
                dial: dial as u32,
                value: value.clamp(0.0, 1.0),
            })
        }
        ["comp", "clear"] => fire.then_some(Command::ClearAll),
        _ => None,
    }
}

pub fn resolve(p: &Project, address: &str, value: f64) -> Option<Command> {
    resolve_with(p, &Ctx::new(None, false), address, value)
}

/// What pressing the shortcut's key does, honoring its mode: Trigger fires; Toggle flips
/// between `range.0` and `range.1`; Absolute sets `range.1` (`range.0` if inverted);
/// Relative adds `range.1` (subtracts if inverted) to the current value.
pub fn activate(p: &Project, ctx: &Ctx, s: &Shortcut) -> Option<Command> {
    let cur = current(p, ctx, &s.target);
    let (lo, hi) = s.range;
    // Adding to an on/off target is meaningless; flip it instead.
    let boolean = ["bypass", "solo", "metronome"]
        .iter()
        .any(|b| s.target.ends_with(b));
    let mode = match s.mode {
        ShortcutMode::Relative if boolean => &ShortcutMode::Toggle,
        ref m => m,
    };
    let value = match (mode, cur) {
        (ShortcutMode::Trigger, _) | (_, None) => 1.0,
        (ShortcutMode::Toggle, Some(c)) => {
            if c >= (lo + hi) / 2.0 {
                lo
            } else {
                hi
            }
        }
        (ShortcutMode::Absolute, _) => {
            if s.invert {
                lo
            } else {
                hi
            }
        }
        (ShortcutMode::Relative, Some(c)) => c + if s.invert { -hi } else { hi },
    };
    resolve_with(p, ctx, &s.target, value)
}

pub fn key_shortcut(key: &str, target: &str, mode: ShortcutMode, range: (f64, f64)) -> Shortcut {
    Shortcut {
        id: Uuid::new_v4(),
        source: ControlSource::Key { code: key.into() },
        target: target.into(),
        mode,
        range,
        invert: false,
    }
}

pub fn key_of(s: &Shortcut) -> Option<&str> {
    match &s.source {
        ControlSource::Key { code } => Some(code),
        _ => None,
    }
}

/// Default mode for a target when learned: triggers fire, switches and opacity toggle.
fn default_mode(address: &str) -> (ShortcutMode, (f64, f64)) {
    if address.ends_with("/bypass") || is_continuous(address) {
        (ShortcutMode::Toggle, (0.0, 1.0))
    } else {
        (ShortcutMode::Trigger, (0.0, 1.0))
    }
}

/// Commands that map `key` to `target` (learn): the key's and the target's previous
/// shortcuts go away first, so a key never has two meanings and a target has one key.
pub fn learn(p: &Project, target: &str, key: &str) -> Vec<Command> {
    let mut cmds = unlearn(p, target);
    for s in &p.shortcuts {
        if key_of(s) == Some(key) && s.target != target {
            cmds.push(Command::RemoveShortcut { id: s.id });
        }
    }
    let (mode, range) = default_mode(target);
    let index = p.shortcuts.len();
    cmds.push(Command::AddShortcut {
        shortcut: key_shortcut(key, target, mode, range),
        index,
    });
    cmds
}

/// Commands that remove every key shortcut of `target`.
pub fn unlearn(p: &Project, target: &str) -> Vec<Command> {
    p.shortcuts
        .iter()
        .filter(|s| s.target == target && key_of(s).is_some())
        .map(|s| Command::RemoveShortcut { id: s.id })
        .collect()
}

/// Key name mapped to `target`, for the overlay.
pub fn key_for<'a>(p: &'a Project, target: &str) -> Option<&'a str> {
    p.shortcuts
        .iter()
        .find(|s| s.target == target)
        .and_then(key_of)
}

/// The shortcut set a new project starts with: arrows step columns, Space fires the
/// selected one.
pub fn default_shortcuts() -> Vec<Shortcut> {
    let t = ShortcutMode::Trigger;
    [
        ("ArrowLeft", "/comp/column/prev"),
        ("ArrowRight", "/comp/column/next"),
        ("Space", "/comp/column/selected/connect"),
    ]
    .into_iter()
    .map(|(k, a)| key_shortcut(k, a, t.clone(), (0.0, 1.0)))
    .collect()
}

pub(crate) fn apply(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
    match cmd {
        Command::AddShortcut { shortcut, index } => {
            if p.shortcuts.iter().any(|s| s.id == shortcut.id)
                || !shortcut.range.0.is_finite()
                || !shortcut.range.1.is_finite()
            {
                return Err(Error::InvalidValue);
            }
            let id = shortcut.id;
            p.shortcuts.insert(index.min(p.shortcuts.len()), shortcut);
            undoable(Command::RemoveShortcut { id }, vec![])
        }
        Command::RemoveShortcut { id } => {
            let i = p
                .shortcuts
                .iter()
                .position(|s| s.id == id)
                .ok_or(Error::NoShortcut)?;
            let shortcut = p.shortcuts.remove(i);
            undoable(Command::AddShortcut { shortcut, index: i }, vec![])
        }
        _ => unreachable!("not a shortcut command"),
    }
}

#[cfg(test)]
mod relative_tests {
    use super::*;

    #[test]
    fn relative_on_bypass_flips() {
        let p = Project::new("t");
        let s = key_shortcut(
            "B",
            "/comp/master/bypass",
            ShortcutMode::Relative,
            (0.0, 1.0),
        );
        let on = activate(&p, &Ctx::new(None, true), &s);
        let off = activate(&p, &Ctx::new(None, false), &s);
        assert_ne!(format!("{on:?}"), format!("{off:?}"));
    }
}
