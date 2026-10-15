//! MIDI as shortcut source, pure: bytes -> message, message + shortcut -> command, and the
//! LED feedback diff. No device I/O here (the app owns the ports), so all of it tests
//! without hardware. Channels are 0-based (the low nibble of the status byte).

use crate::Command;
use crate::shortcuts::{self, Ctx};
use std::collections::HashMap;
use vjc_model::{ColumnIdx, ControlSource, DeckId, LayerId, Project, Shortcut, ShortcutMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    NoteOn { channel: u8, note: u8 },
    NoteOff { channel: u8, note: u8 },
    Cc { channel: u8, cc: u8, value: u8 },
}

/// Parses one complete MIDI message. Note On with velocity 0 is a Note Off. Anything else
/// (clock, sysex, pitch bend, truncated data) is None.
pub fn parse(bytes: &[u8]) -> Option<Msg> {
    let [status, a, b] = *bytes else {
        return None;
    };
    if a > 127 || b > 127 {
        return None;
    }
    let channel = status & 0x0F;
    match status & 0xF0 {
        0x90 if b > 0 => Some(Msg::NoteOn { channel, note: a }),
        0x90 | 0x80 => Some(Msg::NoteOff { channel, note: a }),
        0xB0 => Some(Msg::Cc {
            channel,
            cc: a,
            value: b,
        }),
        _ => None,
    }
}

/// The control a message comes from (Note Off has none), for learn.
pub fn source_of(device: &str, m: Msg) -> Option<ControlSource> {
    match m {
        Msg::NoteOn { channel, note } => Some(ControlSource::MidiNote {
            device: device.into(),
            channel,
            note,
        }),
        Msg::Cc { channel, cc, .. } => Some(ControlSource::MidiCc {
            device: device.into(),
            channel,
            cc,
        }),
        Msg::NoteOff { .. } => None,
    }
}

fn matches(src: &ControlSource, device: &str, m: Msg) -> bool {
    match (src, m) {
        (
            ControlSource::MidiNote {
                device: d,
                channel,
                note,
            },
            Msg::NoteOn {
                channel: c,
                note: n,
            },
        ) => d == device && *channel == c && *note == n,
        (
            ControlSource::MidiCc {
                device: d,
                channel,
                cc,
            },
            Msg::Cc {
                channel: c, cc: n, ..
            },
        ) => d == device && *channel == c && *cc == n,
        _ => false,
    }
}

/// Command for a message that hits `s`, if any.
/// - Note On: same as pressing a key (`activate`).
/// - CC, Absolute: 0..127 maps linearly onto `range` (reversed when `invert`).
/// - CC, Relative: encoder in 64 +- n mode (65..127 up, 0..63 down, 64 = still); each
///   tick moves the value by (hi - lo) / 127, reversed when `invert`.
/// - CC, Trigger/Toggle: acts like a press when the value is >= 64.
pub fn command_for(p: &Project, ctx: &Ctx, s: &Shortcut, device: &str, m: Msg) -> Option<Command> {
    if !matches(&s.source, device, m) {
        return None;
    }
    let Msg::Cc { value, .. } = m else {
        return shortcuts::activate(p, ctx, s);
    };
    let (lo, hi) = s.range;
    match s.mode {
        ShortcutMode::Absolute => {
            let t = f64::from(value) / 127.0;
            let t = if s.invert { 1.0 - t } else { t };
            shortcuts::resolve_with(p, ctx, &s.target, lo + (hi - lo) * t)
        }
        ShortcutMode::Relative => {
            let delta = i32::from(value) - 64;
            if delta == 0 {
                return None;
            }
            let sign = if s.invert { -1.0 } else { 1.0 };
            let cur = shortcuts::current(p, ctx, &s.target)?;
            let v = cur + sign * f64::from(delta) * (hi - lo) / 127.0;
            shortcuts::resolve_with(p, ctx, &s.target, v)
        }
        ShortcutMode::Trigger | ShortcutMode::Toggle => {
            if value >= 64 {
                shortcuts::activate(p, ctx, s)
            } else {
                None
            }
        }
    }
}

/// Whether `m` is the release of the control behind `s`: its note going off, or its CC
/// (Trigger/Toggle) falling below the press threshold. Lets a Hold clip go.
pub fn releases(s: &Shortcut, device: &str, m: Msg) -> bool {
    match (&s.source, m) {
        (
            ControlSource::MidiNote {
                device: d,
                channel,
                note,
            },
            Msg::NoteOff {
                channel: c,
                note: n,
            },
        ) => d == device && *channel == c && *note == n,
        (
            ControlSource::MidiCc {
                device: d,
                channel,
                cc,
            },
            Msg::Cc {
                channel: c,
                cc: n,
                value,
            },
        ) => {
            d == device
                && *channel == c
                && *cc == n
                && matches!(s.mode, ShortcutMode::Trigger | ShortcutMode::Toggle)
                && value < 64
        }
        _ => false,
    }
}

/// Defaults for a learned MIDI control: knobs on continuous targets are Absolute over the
/// target's range; everything else follows the key defaults.
fn default_for(target: &str, src: &ControlSource) -> (ShortcutMode, (f64, f64)) {
    let cc = matches!(src, ControlSource::MidiCc { .. });
    if cc && shortcuts::is_continuous(target) {
        (ShortcutMode::Absolute, (0.0, 1.0))
    } else if cc && target.ends_with("/bpm") {
        (
            ShortcutMode::Absolute,
            (crate::clock::MIN_BPM, crate::clock::MAX_BPM),
        )
    } else if target.ends_with("/bypass") || shortcuts::is_continuous(target) {
        (ShortcutMode::Toggle, (0.0, 1.0))
    } else {
        (ShortcutMode::Trigger, (0.0, 1.0))
    }
}

fn is_midi(src: &ControlSource) -> bool {
    matches!(
        src,
        ControlSource::MidiNote { .. } | ControlSource::MidiCc { .. }
    )
}

/// Commands that remove every MIDI shortcut of `target`.
pub fn unlearn(p: &Project, target: &str) -> Vec<Command> {
    p.shortcuts
        .iter()
        .filter(|s| s.target == target && is_midi(&s.source))
        .map(|s| Command::RemoveShortcut { id: s.id })
        .collect()
}

/// Commands that map `source` to `target`: the target's old MIDI shortcut and any other
/// target using the same control go away first. Keyboard shortcuts are untouched.
pub fn learn(p: &Project, target: &str, source: ControlSource) -> Vec<Command> {
    // A CC knob sends a stream: once the mapping exists, further ticks change nothing.
    if p.shortcuts
        .iter()
        .any(|s| s.target == target && s.source == source)
    {
        let others = p.shortcuts.iter().filter(|s| {
            let same = s.target == target && s.source == source;
            !same && ((s.target == target && is_midi(&s.source)) || s.source == source)
        });
        return others
            .map(|s| Command::RemoveShortcut { id: s.id })
            .collect();
    }
    let mut cmds = unlearn(p, target);
    for s in &p.shortcuts {
        if s.source == source && s.target != target {
            cmds.push(Command::RemoveShortcut { id: s.id });
        }
    }
    let (mode, range) = default_for(target, &source);
    cmds.push(Command::AddShortcut {
        shortcut: Shortcut {
            id: uuid::Uuid::new_v4(),
            source,
            target: target.into(),
            mode,
            range,
            invert: false,
            effect: None,
        },
        index: p.shortcuts.len(),
    });
    cmds
}

/// Short label of the MIDI control mapped to `target`, for the overlay.
pub fn label_for(p: &Project, target: &str) -> Option<String> {
    p.shortcuts
        .iter()
        .filter(|s| s.target == target)
        .find_map(|s| match &s.source {
            ControlSource::MidiNote { channel, note, .. } => {
                Some(format!("N{note} ch{}", channel + 1))
            }
            ControlSource::MidiCc { channel, cc, .. } => Some(format!("CC{cc} ch{}", channel + 1)),
            _ => None,
        })
}

/// A controller LED: device name, channel, note.
pub type Led = (String, u8, u8);

/// Every deck/column cell playing on a layer (its own clip and the ones redirected to
/// it); the app owns that state.
pub type Active<'a> = &'a dyn Fn(LayerId) -> Vec<(DeckId, ColumnIdx)>;

/// On/off state of every note-mapped target with state: a clip playing, a column playing
/// on any layer, or a bypass/toggle at or above the middle of its range.
pub fn led_states(p: &Project, ctx: &Ctx, active: Active) -> HashMap<Led, bool> {
    let deck = p.composition.current_deck;
    let playing = |layer: LayerId, col: ColumnIdx| active(layer).contains(&(deck, col));
    let mut out = HashMap::new();
    for s in &p.shortcuts {
        let ControlSource::MidiNote {
            device,
            channel,
            note,
        } = &s.source
        else {
            continue;
        };
        let parts: Vec<&str> = s.target.trim_start_matches('/').split('/').collect();
        let col = |m: &str| m.parse::<usize>().ok()?.checked_sub(1)?.try_into().ok();
        let on = match parts.as_slice() {
            ["comp", "layer", n, "clip", m, "connect"] => {
                let layer = n.parse::<usize>().ok().and_then(|n| {
                    let ls = &p.composition.layers;
                    (n >= 1 && n <= ls.len()).then(|| ls[ls.len() - n].id)
                });
                match (layer, col(m)) {
                    (Some(l), Some(c)) => playing(l, c),
                    _ => continue,
                }
            }
            ["comp", "column", m, "connect"] => match col(m) {
                Some(c) => p.composition.layers.iter().any(|l| playing(l.id, c)),
                None => continue,
            },
            _ => match shortcuts::current(p, ctx, &s.target) {
                Some(v) => {
                    let (lo, hi) = s.range;
                    v >= (lo + hi) / 2.0 && v > lo
                }
                None => continue,
            },
        };
        out.insert((device.clone(), *channel, *note), on);
    }
    out
}

/// LED writes needed to go from `prev` to `next`: new or changed LEDs, and LEDs that lost
/// their mapping go dark. Sorted, so the result is deterministic.
/// LED messages to send this frame: the diff against what was sent, restricted to enabled
/// devices. Returns the state to remember as sent (disabled devices left out, so enabling
/// one again resends everything to it).
pub fn led_sends(
    prev: &HashMap<Led, bool>,
    mut next: HashMap<Led, bool>,
    enabled: impl Fn(&str) -> bool,
) -> (Vec<(Led, bool)>, HashMap<Led, bool>) {
    next.retain(|(d, _, _), _| enabled(d));
    let sends = feedback_diff(prev, &next)
        .into_iter()
        .filter(|((d, _, _), _)| enabled(d))
        .collect();
    (sends, next)
}

pub fn feedback_diff(prev: &HashMap<Led, bool>, next: &HashMap<Led, bool>) -> Vec<(Led, bool)> {
    let mut out: Vec<(Led, bool)> = next
        .iter()
        .filter(|(k, v)| prev.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), *v))
        .chain(
            prev.iter()
                .filter(|(k, v)| **v && !next.contains_key(*k))
                .map(|(k, _)| (k.clone(), false)),
        )
        .collect();
    out.sort();
    out
}

/// Wire bytes for an LED: Note On velocity 127, or Note Off.
pub fn led_bytes(channel: u8, note: u8, on: bool) -> [u8; 3] {
    if on {
        [0x90 | (channel & 0x0F), note, 127]
    } else {
        [0x80 | (channel & 0x0F), note, 0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc_shortcut(target: &str, mode: ShortcutMode, range: (f64, f64)) -> Shortcut {
        Shortcut {
            id: uuid::Uuid::new_v4(),
            source: ControlSource::MidiCc {
                device: "pad".into(),
                channel: 0,
                cc: 7,
            },
            target: target.into(),
            mode,
            range,
            invert: false,
            effect: None,
        }
    }

    fn cc(value: u8) -> Msg {
        Msg::Cc {
            channel: 0,
            cc: 7,
            value,
        }
    }

    #[test]
    fn parses_bytes() {
        assert_eq!(
            parse(&[0x91, 60, 100]),
            Some(Msg::NoteOn {
                channel: 1,
                note: 60
            })
        );
        assert_eq!(
            parse(&[0x90, 60, 0]),
            Some(Msg::NoteOff {
                channel: 0,
                note: 60
            })
        );
        assert_eq!(
            parse(&[0x80, 60, 64]),
            Some(Msg::NoteOff {
                channel: 0,
                note: 60
            })
        );
        assert_eq!(
            parse(&[0xB2, 7, 99]),
            Some(Msg::Cc {
                channel: 2,
                cc: 7,
                value: 99
            })
        );
        assert_eq!(parse(&[0xF8]), None);
        assert_eq!(parse(&[0x90, 60]), None);
        assert_eq!(parse(&[0x90, 200, 1]), None);
    }

    #[test]
    fn absolute_cc_scales_and_inverts() {
        let p = Project::new("t");
        let ctx = Ctx::new(None, false);
        let mut s = cc_shortcut("/comp/master/opacity", ShortcutMode::Absolute, (0.0, 1.0));
        let v = |c: Option<Command>| match c {
            Some(Command::SetMasterOpacity { value }) => value,
            other => panic!("{other:?}"),
        };
        assert_eq!(v(command_for(&p, &ctx, &s, "pad", cc(127))), 1.0);
        assert_eq!(v(command_for(&p, &ctx, &s, "pad", cc(0))), 0.0);
        s.invert = true;
        assert_eq!(v(command_for(&p, &ctx, &s, "pad", cc(127))), 0.0);
    }

    #[test]
    fn relative_cc_steps_around_64() {
        let mut p = Project::new("t");
        p.composition.tempo.bpm = 120.0;
        let ctx = Ctx::new(None, false);
        let s = cc_shortcut("/comp/tempo/bpm", ShortcutMode::Relative, (0.0, 127.0));
        let bpm = |m| match command_for(&p, &ctx, &s, "pad", m) {
            Some(Command::SetBpm { bpm }) => Some(bpm),
            _ => None,
        };
        assert_eq!(bpm(cc(65)), Some(121.0));
        assert_eq!(bpm(cc(62)), Some(118.0));
        assert_eq!(bpm(cc(64)), None);
    }

    #[test]
    fn ignores_other_device_channel_and_note_off() {
        let p = Project::new("t");
        let ctx = Ctx::new(None, false);
        let s = cc_shortcut("/comp/master/opacity", ShortcutMode::Absolute, (0.0, 1.0));
        assert!(command_for(&p, &ctx, &s, "other", cc(5)).is_none());
        let m = Msg::Cc {
            channel: 1,
            cc: 7,
            value: 5,
        };
        assert!(command_for(&p, &ctx, &s, "pad", m).is_none());
        let n = Msg::NoteOff {
            channel: 0,
            note: 7,
        };
        assert!(command_for(&p, &ctx, &s, "pad", n).is_none());
    }

    #[test]
    fn note_on_triggers() {
        let p = Project::new("t");
        let ctx = Ctx::new(None, false);
        let s = Shortcut {
            source: ControlSource::MidiNote {
                device: "pad".into(),
                channel: 0,
                note: 36,
            },
            ..cc_shortcut("/comp/clear", ShortcutMode::Trigger, (0.0, 1.0))
        };
        let on = Msg::NoteOn {
            channel: 0,
            note: 36,
        };
        assert!(matches!(
            command_for(&p, &ctx, &s, "pad", on),
            Some(Command::ClearAll)
        ));
    }

    #[test]
    fn learn_replaces_and_picks_defaults() {
        let p = Project::new("t");
        let src = source_of("pad", cc(1)).unwrap();
        let cmds = learn(&p, "/comp/master/opacity", src);
        match cmds.last() {
            Some(Command::AddShortcut { shortcut, .. }) => {
                assert_eq!(shortcut.mode, ShortcutMode::Absolute)
            }
            other => panic!("{other:?}"),
        }
        assert!(
            source_of(
                "pad",
                Msg::NoteOff {
                    channel: 0,
                    note: 1
                }
            )
            .is_none()
        );
    }

    #[test]
    fn learn_same_control_again_is_noop() {
        let mut p = Project::new("t");
        let src = source_of("pad", cc(1)).unwrap();
        let first = learn(&p, "/comp/master/opacity", src.clone());
        let Some(Command::AddShortcut { shortcut, .. }) = first.last() else {
            panic!("no add");
        };
        p.shortcuts.push(shortcut.clone());
        assert!(learn(&p, "/comp/master/opacity", src).is_empty());
    }

    #[test]
    fn led_sends_skip_disabled_devices_and_resend_on_enable() {
        let k = |d: &str, n: u8| (d.to_string(), 0u8, n);
        let on = HashMap::from([(k("pad", 1), true), (k("pad", 2), false)]);
        let (s, prev) = led_sends(&HashMap::new(), on.clone(), |_| true);
        assert_eq!(s.len(), 2);
        // Disabling sends nothing to that device, not even a last off.
        let (s, prev) = led_sends(&prev, on.clone(), |d| d != "pad");
        assert!(s.is_empty(), "{s:?}");
        assert!(prev.is_empty());
        // Enabling again resends the full state.
        let (s, _) = led_sends(&prev, on, |_| true);
        assert_eq!(s, vec![(k("pad", 1), true), (k("pad", 2), false)]);
    }

    #[test]
    fn feedback_diff_sends_only_changes() {
        let k = |n: u8| ("pad".to_string(), 0u8, n);
        let prev = HashMap::from([(k(1), true), (k(2), false), (k(3), true)]);
        let next = HashMap::from([(k(1), true), (k(2), true), (k(4), false)]);
        assert_eq!(
            feedback_diff(&prev, &next),
            vec![(k(2), true), (k(3), false), (k(4), false)]
        );
        assert!(feedback_diff(&next, &next).is_empty());
        assert_eq!(led_bytes(2, 60, true), [0x92, 60, 127]);
        assert_eq!(led_bytes(2, 60, false), [0x82, 60, 0]);
    }

    #[test]
    fn led_state_follows_bypass() {
        let p = Project::new("t");
        let s = Shortcut {
            source: ControlSource::MidiNote {
                device: "pad".into(),
                channel: 0,
                note: 5,
            },
            ..cc_shortcut("/comp/master/bypass", ShortcutMode::Toggle, (0.0, 1.0))
        };
        let mut p = p;
        p.shortcuts.push(s);
        let none = |_: LayerId| Vec::new();
        let off = led_states(&p, &Ctx::new(None, false), &none);
        let on = led_states(&p, &Ctx::new(None, true), &none);
        let key = ("pad".to_string(), 0u8, 5u8);
        assert!(!off[&key]);
        assert!(on[&key]);
    }
}
