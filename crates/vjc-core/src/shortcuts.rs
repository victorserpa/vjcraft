//! Mappable shortcuts: VJCraft's own address space ("/comp/layer/1/opacity") and the pure
//! `resolve` that turns an address plus a value into a `Command`. Layers are numbered from
//! the bottom (Layer 1 is the last one listed), columns from 1, on the current deck.

use super::{Applied, Command, Error, number, undoable};
use std::time::Instant;
use uuid::Uuid;
use vjc_model::{ColumnIdx, ControlSource, LayerId, Project, Shortcut, ShortcutMode};

/// Runtime state `resolve` needs and the project does not hold.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub col_sel: Option<ColumnIdx>,
    pub master_bypass: bool,
    pub now: Instant,
}

impl Ctx {
    pub fn new(col_sel: Option<ColumnIdx>, master_bypass: bool) -> Self {
        Ctx {
            col_sel,
            master_bypass,
            now: Instant::now(),
        }
    }
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
    match parts.as_slice() {
        ["comp", "layer", n, "opacity"] => {
            let l = layer_by_number(p, number_of(n)?)?;
            let l = p.composition.layers.iter().find(|x| x.id == l)?;
            Some(number(&l.track.opacity.value))
        }
        ["comp", "master", "opacity"] => Some(number(&p.composition.master.opacity.value)),
        ["comp", "master", "bypass"] => Some(f64::from(u8::from(ctx.master_bypass))),
        ["comp", "tempo", "bpm"] => Some(p.composition.tempo.bpm),
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
    if address.ends_with("/bypass") || address.ends_with("/opacity") {
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
