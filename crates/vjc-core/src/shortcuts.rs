//! Mappable shortcuts: VJCraft's own address space ("/comp/layer/1/opacity") and the pure
//! `resolve` that turns an address plus a value into a `Command`. Layers are numbered from
//! the bottom (Layer 1 is the last one listed), columns from 1, on the current deck.

use super::effects::{self, EffectTarget};
use super::{Applied, Command, Error, number, undoable};
use std::time::Instant;
use uuid::Uuid;
use vjc_model::{
    ColumnIdx, ControlSource, DeckId, Effect, LayerId, Project, Shortcut, ShortcutMode,
    TransformField, Value,
};

/// Runtime state `resolve` needs and the project does not hold.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub col_sel: Option<ColumnIdx>,
    pub master_bypass: bool,
    pub now: Instant,
    /// Clip playing on each layer; "/comp/layer/N/clip/effect/..." addresses follow it.
    pub active: Vec<(LayerId, DeckId, ColumnIdx)>,
    /// Redirected clips: (layer it plays on, layer of the cell it lives in). Addresses name
    /// the first; the clip's own state (cues, dials, effects) belongs to the second.
    pub origin: Vec<(LayerId, LayerId)>,
}

impl Ctx {
    pub fn new(col_sel: Option<ColumnIdx>, master_bypass: bool) -> Self {
        Ctx {
            col_sel,
            master_bypass,
            now: Instant::now(),
            active: vec![],
            origin: vec![],
        }
    }

    pub fn with_origin(mut self, origin: Vec<(LayerId, LayerId)>) -> Self {
        self.origin = origin;
        self
    }

    /// Cell (deck, layer, column) of the clip playing on `layer`: its own layer unless the
    /// clip was redirected there.
    pub fn cell(&self, layer: LayerId) -> Option<(DeckId, LayerId, ColumnIdx)> {
        let &(_, deck, column) = self.active.iter().find(|a| a.0 == layer)?;
        let src = self
            .origin
            .iter()
            .find(|o| o.0 == layer)
            .map_or(layer, |o| o.1);
        Some((deck, src, column))
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
        EffectTarget::Group(g) => group_address(p, g, &format!("effect/{}/{tail}", k + 1)),
        EffectTarget::Clip {
            deck,
            layer,
            column,
        } => (active == Some((deck, column)))
            .then(|| layer_address(p, layer, &format!("clip/effect/{}/{tail}", k + 1)))?,
        // Audio chains have no OSC/MIDI address yet (their params map through animations).
        EffectTarget::ClipAudio { .. }
        | EffectTarget::LayerAudio(_)
        | EffectTarget::MasterAudio => None,
    }
}

/// Transform addresses: "/comp/transform/F" (composition), "/comp/layer/N/transform/F" and
/// "/comp/layer/N/clip/transform/F" (clip playing on the layer); F is a
/// `TransformField::address`. Values are normalized 0..1 over the field's range.
pub fn transform_address(
    p: &Project,
    target: EffectTarget,
    field: TransformField,
    active: Option<(DeckId, ColumnIdx)>,
) -> Option<String> {
    let tail = format!("transform/{}", field.address());
    match target {
        EffectTarget::Master => Some(format!("/comp/{tail}")),
        EffectTarget::Layer(l) => layer_address(p, l, &tail),
        EffectTarget::Group(_)
        | EffectTarget::ClipAudio { .. }
        | EffectTarget::LayerAudio(_)
        | EffectTarget::MasterAudio => None,
        EffectTarget::Clip {
            deck,
            layer,
            column,
        } => (active == Some((deck, column)))
            .then(|| layer_address(p, layer, &format!("clip/{tail}")))?,
    }
}

/// Outer None: not a transform address; inner None: one that points at nothing.
fn parse_tf(
    p: &Project,
    ctx: &Ctx,
    parts: &[&str],
) -> Option<Option<(EffectTarget, TransformField)>> {
    let (scope, f): (Option<EffectTarget>, &str) = match parts {
        ["comp", "transform", f] => (Some(EffectTarget::Master), f),
        ["comp", "layer", n, "transform", f] => (
            number_of(n)
                .and_then(|n| layer_by_number(p, n))
                .map(EffectTarget::Layer),
            f,
        ),
        ["comp", "layer", n, "clip", "transform", f] => (
            number_of(n)
                .and_then(|n| layer_by_number(p, n))
                .and_then(|layer| {
                    let (deck, layer, column) = ctx.cell(layer)?;
                    Some(EffectTarget::Clip {
                        deck,
                        layer,
                        column,
                    })
                }),
            f,
        ),
        _ => return None,
    };
    Some(scope.zip(TransformField::from_address(f)))
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
        ["comp", "group", n, "effect", k, rest @ ..] => (
            number_of(n)
                .and_then(|n| group_by_number(p, n))
                .map(EffectTarget::Group),
            k,
            rest,
        ),
        ["comp", "layer", n, "clip", "effect", k, rest @ ..] => (
            number_of(n)
                .and_then(|n| layer_by_number(p, n))
                .and_then(|layer| {
                    let (deck, layer, column) = ctx.cell(layer)?;
                    Some(EffectTarget::Clip {
                        deck,
                        layer,
                        column,
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

/// Prefix of the target of a shortcut whose effect was removed; resolves to nothing, and
/// goes back to a real address if the effect returns (undo).
const ORPHAN: &str = "/orphan/";

pub fn is_orphan(s: &Shortcut) -> bool {
    s.effect.is_some() && s.target.starts_with(ORPHAN)
}

/// Scope prefix ("/comp", "/comp/layer/N", ...) and tail of an effect address.
fn split_fx_target(target: &str) -> Option<(&str, &str)> {
    if let Some(tail) = target.strip_prefix(ORPHAN) {
        return Some(("", tail));
    }
    let i = target.find("/effect/")?;
    let rest = &target[i + "/effect/".len()..];
    let (k, tail) = rest.split_once('/')?;
    k.parse::<usize>().ok()?;
    Some((&target[..i], tail))
}

/// The effect a positional effect address points at right now. Clip addresses pick the first
/// clip of that layer (current deck) that has an effect there.
fn effect_of_target(p: &Project, target: &str) -> Option<vjc_model::EffectId> {
    let parts: Vec<&str> = target.trim_start_matches('/').split('/').collect();
    let mut ctx = Ctx::new(None, false);
    if let ["comp", "layer", n, "clip", ..] = parts.as_slice() {
        let layer = layer_by_number(p, number_of(n)?)?;
        let deck = p
            .composition
            .decks
            .iter()
            .find(|d| d.id == p.composition.current_deck)?;
        ctx.active = deck
            .clips
            .iter()
            .filter(|c| c.layer == layer)
            .map(|c| (layer, deck.id, c.column))
            .collect();
    }
    // `active` may list several clips; parse_fx takes the first, so try each in turn.
    let tries = if ctx.active.is_empty() {
        vec![vec![]]
    } else {
        ctx.active.iter().map(|a| vec![*a]).collect()
    };
    tries.into_iter().find_map(|a| {
        ctx.active = a;
        parse_fx(p, &ctx, &parts)?.map(|(_, e, _)| e.id)
    })
}

/// Where effect `id` sits now: its scope prefix and 1-based chain position.
fn locate_effect(p: &Project, id: vjc_model::EffectId) -> Option<(String, usize)> {
    let c = &p.composition;
    let at = |fx: &[Effect]| fx.iter().position(|e| e.id == id).map(|i| i + 1);
    if let Some(k) = at(&c.master.effects) {
        return Some(("/comp".into(), k));
    }
    for l in &c.layers {
        if let Some(k) = at(&l.track.effects) {
            return layer_address(p, l.id, "").map(|a| (a.trim_end_matches('/').into(), k));
        }
    }
    for g in &c.groups {
        if let Some(k) = at(&g.track.effects) {
            return group_address(p, g.id, "").map(|a| (a.trim_end_matches('/').into(), k));
        }
    }
    for d in &c.decks {
        for cl in &d.clips {
            if let Some(k) = at(&cl.clip.track.effects) {
                return layer_address(p, cl.layer, "clip")
                    .map(|a| (a.trim_end_matches('/').into(), k));
            }
        }
    }
    None
}

/// Re-points every effect shortcut at its effect's current address, or orphans it when the
/// effect is gone. Idempotent; called after each applied command.
pub fn rebind(p: &mut Project) {
    for i in 0..p.shortcuts.len() {
        let Some(id) = p.shortcuts[i].effect else {
            continue;
        };
        let Some((_, tail)) = split_fx_target(&p.shortcuts[i].target) else {
            continue;
        };
        let target = match locate_effect(p, id) {
            Some((scope, k)) => format!("{scope}/effect/{k}/{tail}"),
            None => format!("{ORPHAN}{tail}"),
        };
        if p.shortcuts[i].target != target {
            p.shortcuts[i].target = target;
        }
    }
}

/// What `bind_legacy` did: how many shortcuts it attached to their effect, and the targets
/// of the ones it could not (the effect they point at does not exist).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LegacyReport {
    pub bound: usize,
    pub unresolved: Vec<String>,
}

/// Load-time migration: shortcuts saved before `effect` existed are still positional; bind
/// each effect address to the effect it points at now, so later reorders keep it attached.
pub fn bind_legacy(p: &mut Project) -> LegacyReport {
    let mut r = LegacyReport::default();
    for i in 0..p.shortcuts.len() {
        let s = &p.shortcuts[i];
        if s.effect.is_some() || split_fx_target(&s.target).is_none() {
            continue;
        }
        if let Some(id) = effect_of_target(p, &s.target) {
            p.shortcuts[i].effect = Some(id);
            r.bound += 1;
        } else {
            r.unresolved.push(s.target.clone());
        }
    }
    r
}

/// Layer number of a positional "/comp/layer/N/..." shortcut. Effect shortcuts are skipped:
/// `rebind` keeps those attached through their effect id.
pub fn layer_number(s: &vjc_model::Shortcut) -> Option<usize> {
    if s.effect.is_some() {
        return None;
    }
    s.target
        .strip_prefix("/comp/layer/")?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// Rewrites the layer number of a target `layer_number` accepted.
pub fn set_layer_number(s: &mut vjc_model::Shortcut, n: usize) {
    if let Some(rest) = s.target.strip_prefix("/comp/layer/")
        && let Some((_, tail)) = rest.split_once('/')
    {
        s.target = format!("/comp/layer/{n}/{tail}");
    }
}

/// True for targets a knob sweeps (opacity and effect params / dry-wet) rather than flips.
pub fn is_continuous(address: &str) -> bool {
    address.ends_with("/opacity")
        || address.ends_with("/speed")
        || address.ends_with("/drywet")
        || address.contains("/transform/")
        || (address.contains("/effect/") && address.contains("/param/"))
}

pub fn layer_address(p: &Project, layer: LayerId, tail: &str) -> Option<String> {
    let ls = &p.composition.layers;
    let i = ls.iter().position(|l| l.id == layer)?;
    Some(format!("/comp/layer/{}/{tail}", ls.len() - i))
}

/// Groups are numbered from the bottom like layers: "/comp/group/N/opacity|bypass|solo|speed".
pub fn group_address(p: &Project, group: vjc_model::GroupId, tail: &str) -> Option<String> {
    let gs = &p.composition.groups;
    let i = gs.iter().position(|g| g.id == group)?;
    Some(format!("/comp/group/{}/{tail}", gs.len() - i))
}

fn group_by_number(p: &Project, n: usize) -> Option<vjc_model::GroupId> {
    let gs = &p.composition.groups;
    (n >= 1 && n <= gs.len()).then(|| gs[gs.len() - n].id)
}

/// Group speed is 0..=4; addresses carry it normalized (0..1).
const GROUP_SPEED_MAX: f64 = 4.0;

pub fn clip_address(p: &Project, layer: LayerId, column: ColumnIdx) -> Option<String> {
    layer_address(p, layer, &format!("clip/{}/connect", column + 1))
}

/// "/comp/layer/N/clip/cue/M": jumps the clip playing on layer N to its cue M (from 1).
pub fn cue_address(p: &Project, layer: LayerId, index: usize) -> Option<String> {
    layer_address(p, layer, &format!("clip/cue/{}", index + 1))
}

/// Dial addresses: "/comp/layer/N/dashboard/M" (the layer's dial) and
/// "/comp/layer/N/clip/dashboard/M" (the clip playing on the layer); M counts from 1.
pub fn dial_address(
    p: &Project,
    layer: LayerId,
    clip: Option<(DeckId, ColumnIdx)>,
    active: Option<(DeckId, ColumnIdx)>,
    dial: usize,
) -> Option<String> {
    match clip {
        None => layer_address(p, layer, &format!("dashboard/{}", dial + 1)),
        Some(c) => (active == Some(c))
            .then(|| layer_address(p, layer, &format!("clip/dashboard/{}", dial + 1)))?,
    }
}

pub fn column_address(column: ColumnIdx) -> String {
    format!("/comp/column/{}/connect", column + 1)
}

fn layer_by_number(p: &Project, n: usize) -> Option<LayerId> {
    let ls = &p.composition.layers;
    (n >= 1 && n <= ls.len()).then(|| ls[ls.len() - n].id)
}

/// Zero-based dial of a 1-based address segment, None when outside 1..=DIALS.
fn dial_of(s: &str) -> Option<u32> {
    let i = number_of(s)?
        .checked_sub(1)
        .filter(|i| *i < super::anim::DIALS)?;
    Some(i as u32)
}

fn dial_value(dials: &[vjc_model::Param], dial: u32) -> f64 {
    dials.get(dial as usize).map_or(0.0, super::anim::dial_norm)
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
    if let Some(r) = parse_tf(p, ctx, &parts) {
        let (t, f) = r?;
        return param_norm(effects::track(p, t)?.transform.field(f));
    }
    match parts.as_slice() {
        ["comp", "layer", n, "dashboard", m] => {
            let layer = layer_by_number(p, number_of(n)?)?;
            let l = p.composition.layers.iter().find(|x| x.id == layer)?;
            Some(dial_value(&l.track.dials, dial_of(m)?))
        }
        ["comp", "layer", n, "clip", "dashboard", m] => {
            let (deck, layer, column) = ctx.cell(layer_by_number(p, number_of(n)?)?)?;
            let k = super::clip_in(p, deck, layer, column)?;
            Some(dial_value(&k.track.dials, dial_of(m)?))
        }
        ["comp", "layer", n, "opacity"] => {
            let l = layer_by_number(p, number_of(n)?)?;
            let l = p.composition.layers.iter().find(|x| x.id == l)?;
            Some(number(&l.track.opacity.value))
        }
        ["comp", "group", n, prop] => {
            let id = group_by_number(p, number_of(n)?)?;
            let g = p.composition.group(id)?;
            match *prop {
                "opacity" => Some(number(&g.track.opacity.value)),
                "bypass" => Some(f64::from(u8::from(g.bypass))),
                "solo" => Some(f64::from(u8::from(g.solo))),
                "speed" => Some(g.speed / GROUP_SPEED_MAX),
                _ => None,
            }
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
                    .map_or(0.0, super::anim::dial_norm),
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
    if let Some(r) = parse_tf(p, ctx, &parts) {
        let (target, field) = r?;
        let Value::Number(v) =
            param_value(effects::track(p, target)?.transform.field(field), value)?
        else {
            return None;
        };
        return Some(Command::SetTransform {
            target,
            field,
            value: v,
        });
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
        ["comp", "layer", n, "clip", "cue", m] => {
            let index = number_of(m)?.checked_sub(1)?;
            let (deck, layer, column) = ctx.cell(layer_by_number(p, number_of(n)?)?)?;
            let k = super::clip_in(p, deck, layer, column)?;
            // A cue that does not exist resolves to nothing, so a stray message is inert.
            (fire && index < k.transport.cue_points.len()).then_some(Command::JumpToCue {
                deck,
                layer,
                column,
                index,
            })
        }
        ["comp", "layer", n, "dashboard", m] => Some(Command::SetLayerDial {
            layer: layer_by_number(p, number_of(n)?)?,
            dial: dial_of(m)?,
            value: value.clamp(0.0, 1.0),
        }),
        ["comp", "layer", n, "clip", "dashboard", m] => {
            let (deck, layer, column) = ctx.cell(layer_by_number(p, number_of(n)?)?)?;
            Some(Command::SetClipDial {
                deck,
                layer,
                column,
                dial: dial_of(m)?,
                value: value.clamp(0.0, 1.0),
            })
        }
        ["comp", "layer", n, "opacity"] => Some(Command::SetOpacity {
            layer: layer_by_number(p, number_of(n)?)?,
            value: value.clamp(0.0, 1.0),
        }),
        ["comp", "group", n, prop] => {
            let group = group_by_number(p, number_of(n)?)?;
            match *prop {
                "opacity" => Some(Command::SetGroupOpacity {
                    group,
                    value: value.clamp(0.0, 1.0),
                }),
                "bypass" => Some(Command::SetGroupBypass {
                    group,
                    bypass: value >= 0.5,
                }),
                "solo" => Some(Command::SetGroupSolo {
                    group,
                    solo: value >= 0.5,
                }),
                "speed" => value.is_finite().then_some(Command::SetGroupSpeed {
                    group,
                    value: value.clamp(0.0, 1.0) * GROUP_SPEED_MAX,
                }),
                _ => None,
            }
        }
        ["comp", "layer", n, "clear"] => fire.then_some(Command::ClearLayer {
            layer: layer_by_number(p, number_of(n)?)?,
        }),
        ["comp", "layer", n, "pause"] => {
            let layer = layer_by_number(p, number_of(n)?)?;
            fire.then(|| toggle_pause(p, &ctx.active, &ctx.origin, Some(layer)))?
        }
        ["comp", "pause"] => fire.then(|| toggle_pause(p, &ctx.active, &ctx.origin, None))?,
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
        ["comp", "deck", "next"] => fire.then(|| step_deck(p, 1))?,
        ["comp", "deck", "prev"] => fire.then(|| step_deck(p, -1))?,
        ["comp", "deck", n, "select"] => {
            let i = number_of(n)?.checked_sub(1)?;
            let d = p.composition.decks.get(i)?;
            fire.then_some(Command::SelectDeck { deck: d.id })
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

/// Pause toggle for the clip playing on `layer` (None = every playing clip): resumes when
/// they are all paused, pauses otherwise. None when nothing is playing there.
pub fn toggle_pause(
    p: &Project,
    active: &[(LayerId, DeckId, ColumnIdx)],
    origin: &[(LayerId, LayerId)],
    layer: Option<LayerId>,
) -> Option<Command> {
    let clips: Vec<_> = active
        .iter()
        .filter(|a| layer.is_none_or(|l| a.0 == l))
        .map(|&(l, d, c)| {
            let src = origin.iter().find(|o| o.0 == l).map_or(l, |o| o.1);
            (d, src, c)
        })
        .collect();
    let paused = |&(d, l, c): &(DeckId, LayerId, ColumnIdx)| {
        p.composition
            .decks
            .iter()
            .find(|k| k.id == d)
            .and_then(|k| k.clips.iter().find(|k| k.layer == l && k.column == c))
            .is_some_and(|k| k.clip.transport.direction == vjc_model::Direction::Pause)
    };
    let all_paused = !clips.is_empty() && clips.iter().all(paused);
    (!clips.is_empty()).then_some(Command::SetPaused {
        clips,
        paused: !all_paused,
    })
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
        effect: None,
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
            let mut shortcut = shortcut;
            if shortcut.effect.is_none() {
                shortcut.effect = effect_of_target(p, &shortcut.target);
            }
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
        Command::UpdateShortcut { shortcut } => {
            if !shortcut.range.0.is_finite() || !shortcut.range.1.is_finite() {
                return Err(Error::InvalidValue);
            }
            let i = p
                .shortcuts
                .iter()
                .position(|s| s.id == shortcut.id)
                .ok_or(Error::NoShortcut)?;
            let mut shortcut = shortcut;
            if shortcut.target != p.shortcuts[i].target {
                shortcut.effect = effect_of_target(p, &shortcut.target);
            }
            let prev = std::mem::replace(&mut p.shortcuts[i], shortcut);
            undoable(Command::UpdateShortcut { shortcut: prev }, vec![])
        }
        _ => unreachable!("not a shortcut command"),
    }
}

/// Ids of the shortcuts that share their input with another one (same source).
pub fn conflicts(p: &Project) -> std::collections::HashSet<Uuid> {
    p.shortcuts
        .iter()
        .filter(|a| {
            p.shortcuts
                .iter()
                .any(|b| b.id != a.id && b.source == a.source)
        })
        .map(|s| s.id)
        .collect()
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

#[cfg(test)]
mod effect_id_tests {
    use super::*;
    use crate::{History, effects};
    use std::time::Instant;

    fn fx(kind: &str) -> Effect {
        Effect {
            blend: Default::default(),
            id: Uuid::new_v4(),
            kind: kind.into(),
            bypass: false,
            dry_wet: 1.0,
            params: vec![],
        }
    }

    fn setup() -> (Project, History, Effect, Effect) {
        let mut p = Project::new("t");
        let mut h = History::new();
        let (a, b) = (fx("a"), fx("b"));
        for (i, e) in [&a, &b].into_iter().enumerate() {
            let cmd = Command::AddEffect {
                target: EffectTarget::Master,
                effect: e.clone(),
                index: i,
            };
            h.exec(&mut p, cmd, Instant::now()).unwrap();
        }
        // Learned positionally: the 2nd effect's dry/wet.
        let s = key_shortcut(
            "Q",
            "/comp/effect/2/drywet",
            ShortcutMode::Toggle,
            (0.0, 1.0),
        );
        let cmd = Command::AddShortcut {
            shortcut: s,
            index: 0,
        };
        h.exec(&mut p, cmd, Instant::now()).unwrap();
        (p, h, a, b)
    }

    #[test]
    fn shortcut_follows_effect_on_reorder() {
        let (mut p, mut h, _a, b) = setup();
        assert_eq!(p.shortcuts[0].effect, Some(b.id));
        let mv = Command::MoveEffect {
            target: EffectTarget::Master,
            effect: b.id,
            index: 0,
        };
        h.exec(&mut p, mv, Instant::now()).unwrap();
        assert_eq!(p.shortcuts[0].target, "/comp/effect/1/drywet");
        let cmd = activate(&p, &Ctx::new(None, false), &p.shortcuts[0]).unwrap();
        assert!(matches!(cmd, Command::SetEffectDryWet { effect, .. } if effect == b.id));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.shortcuts[0].target, "/comp/effect/2/drywet");
    }

    #[test]
    fn removing_the_effect_orphans_instead_of_retargeting() {
        let (mut p, mut h, a, b) = setup();
        let rm = Command::RemoveEffect {
            target: EffectTarget::Master,
            effect: b.id,
        };
        h.exec(&mut p, rm, Instant::now()).unwrap();
        assert!(is_orphan(&p.shortcuts[0]));
        assert_eq!(activate(&p, &Ctx::new(None, false), &p.shortcuts[0]), None);
        // The surviving effect is never picked up, even once it shifts into the slot.
        assert_eq!(
            effects::chain(&p, EffectTarget::Master).unwrap()[0].id,
            a.id
        );
        h.undo(&mut p).unwrap().unwrap();
        assert!(!is_orphan(&p.shortcuts[0]));
        assert_eq!(p.shortcuts[0].target, "/comp/effect/2/drywet");
    }

    #[test]
    fn bind_legacy_reports_unresolved_targets() {
        let (mut p, _h, _a, _b) = setup();
        p.shortcuts[0].effect = None;
        p.shortcuts[0].target = "/comp/effect/9/drywet".into();
        let r = bind_legacy(&mut p);
        assert_eq!(r.bound, 0);
        assert_eq!(r.unresolved, vec!["/comp/effect/9/drywet".to_string()]);
    }

    #[test]
    fn bind_legacy_fills_effect_from_positional_address() {
        let (mut p, mut h, _a, b) = setup();
        p.shortcuts[0].effect = None; // as saved by an older version
        assert_eq!(bind_legacy(&mut p).bound, 1);
        assert_eq!(p.shortcuts[0].effect, Some(b.id));
        let mv = Command::MoveEffect {
            target: EffectTarget::Master,
            effect: b.id,
            index: 0,
        };
        h.exec(&mut p, mv, Instant::now()).unwrap();
        assert_eq!(p.shortcuts[0].target, "/comp/effect/1/drywet");
    }

    #[test]
    fn transform_addresses_resolve_and_read_back() {
        let mut p = Project::new("t");
        let layer = Uuid::new_v4();
        p.composition.layers.push(vjc_model::Layer {
            color: None,
            folded: false,
            id: layer,
            name: "L1".into(),
            track: vjc_model::Track::new(vjc_model::BlendMode::Normal),
            solo: false,
            bypass: false,
            transition: vjc_model::Transition {
                kind: "Cut".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            autopilot: Default::default(),
            crossfader_bus: None,
            audio: true,
            video: true,
            trigger_mode: vjc_model::TriggerMode::Restart,
            ignore_column_trigger: false,
            lock_content: false,
            fader_start: false,
            mask_mode: Default::default(),
            group: None,
        });
        let ctx = Ctx::new(None, false);
        // 0.75 over -16384..16384 px.
        let cmd = resolve_with(&p, &ctx, "/comp/layer/1/transform/x", 0.75).unwrap();
        let Command::SetTransform {
            target,
            field,
            value,
        } = cmd
        else {
            panic!("not a transform command");
        };
        assert_eq!(target, EffectTarget::Layer(layer));
        assert_eq!(field, TransformField::X);
        assert!((value - 8192.0).abs() < 1e-6);
        crate::apply(&mut p, cmd_of(target, field, value)).unwrap();
        let back = current(&p, &ctx, "/comp/layer/1/transform/x").unwrap();
        assert!((back - 0.75).abs() < 1e-9);
        assert!(matches!(
            resolve_with(&p, &ctx, "/comp/transform/rotation", 0.5),
            Some(Command::SetTransform {
                target: EffectTarget::Master,
                ..
            })
        ));
        // Unknown field, unknown layer and a clip that is not playing resolve to nothing.
        assert!(resolve_with(&p, &ctx, "/comp/transform/skew", 0.5).is_none());
        assert!(resolve_with(&p, &ctx, "/comp/layer/9/transform/x", 0.5).is_none());
        assert!(resolve_with(&p, &ctx, "/comp/layer/1/clip/transform/x", 0.5).is_none());
        assert!(is_continuous("/comp/layer/1/transform/scale"));
    }

    fn cmd_of(target: EffectTarget, field: TransformField, value: f64) -> Command {
        Command::SetTransform {
            target,
            field,
            value,
        }
    }
}

/// Select the deck `delta` tabs away from the current one; `None` at either end.
fn step_deck(p: &Project, delta: isize) -> Option<Command> {
    let ds = &p.composition.decks;
    let at = ds.iter().position(|d| d.id == p.composition.current_deck)?;
    let to = at.checked_add_signed(delta)?;
    Some(Command::SelectDeck {
        deck: ds.get(to)?.id,
    })
}
