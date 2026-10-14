//! OSC as shortcut source and feedback sink, pure: packet bytes -> events -> commands, and
//! the state diff plus rate limit for output. No sockets here (the app owns them).
//! Addresses in our own space ("/comp/layer/1/opacity") resolve with no mapping; any other
//! address can be learned through `ControlSource::Osc`.

use crate::Command;
use crate::shortcuts::{self, Ctx};
use rosc::{OscMessage, OscPacket, OscType};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use vjc_model::{ColumnIdx, ControlSource, DeckId, LayerId, Project, Shortcut, ShortcutMode};

/// One incoming message: `value` is None for a trigger (no usable argument).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub address: String,
    pub value: Option<f64>,
}

/// Max messages per second per address on output.
pub const MAX_RATE: f64 = 60.0;

fn value_of(args: &[OscType]) -> Option<f64> {
    args.iter().find_map(|a| match a {
        OscType::Float(f) => Some(f64::from(*f)),
        OscType::Double(d) => Some(*d),
        OscType::Int(i) => Some(f64::from(*i)),
        OscType::Long(l) => Some(*l as f64),
        OscType::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    })
}

fn flatten(packet: OscPacket, out: &mut Vec<Event>) {
    match packet {
        OscPacket::Message(OscMessage { addr, args }) => {
            let value = value_of(&args);
            // NaN/Inf would otherwise read as "no value" and act as full value.
            if value.is_some_and(|v| !v.is_finite()) {
                return;
            }
            out.push(Event {
                address: addr,
                value,
            });
        }
        OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| flatten(p, out)),
    }
}

/// Decodes one UDP datagram (message or nested bundle). Malformed data gives no events.
pub fn decode(bytes: &[u8]) -> Vec<Event> {
    let mut out = vec![];
    if let Ok((_, packet)) = rosc::decoder::decode_udp(bytes) {
        flatten(packet, &mut out);
    }
    out
}

/// Wire bytes for a float message.
pub fn encode(address: &str, value: f64) -> Option<Vec<u8>> {
    rosc::encoder::encode(&OscPacket::Message(OscMessage {
        addr: address.into(),
        args: vec![OscType::Float(value as f32)],
    }))
    .ok()
}

/// Our own address space: resolves directly. A trigger event counts as value 1.
pub fn direct(p: &Project, ctx: &Ctx, ev: &Event) -> Option<Command> {
    shortcuts::resolve_with(p, ctx, &ev.address, ev.value.unwrap_or(1.0))
}

/// Command for a shortcut learned from an external OSC address. Absolute maps 0..1 onto the
/// range (reversed when inverted); Relative adds value * (hi - lo); Trigger/Toggle act on
/// value > 0 (a bare message is a press).
pub fn command_for(p: &Project, ctx: &Ctx, s: &Shortcut, ev: &Event) -> Option<Command> {
    let ControlSource::Osc { address } = &s.source else {
        return None;
    };
    if *address != ev.address {
        return None;
    }
    let v = ev.value.unwrap_or(1.0);
    let (lo, hi) = s.range;
    match s.mode {
        ShortcutMode::Absolute => {
            let t = v.clamp(0.0, 1.0);
            let t = if s.invert { 1.0 - t } else { t };
            shortcuts::resolve_with(p, ctx, &s.target, lo + (hi - lo) * t)
        }
        ShortcutMode::Relative => {
            let sign = if s.invert { -1.0 } else { 1.0 };
            let cur = shortcuts::current(p, ctx, &s.target)?;
            shortcuts::resolve_with(p, ctx, &s.target, cur + sign * v * (hi - lo))
        }
        ShortcutMode::Trigger | ShortcutMode::Toggle => {
            if v > 0.0 {
                shortcuts::activate(p, ctx, s)
            } else {
                None
            }
        }
    }
}

pub fn source_of(ev: &Event) -> ControlSource {
    ControlSource::Osc {
        address: ev.address.clone(),
    }
}

/// Commands that remove every OSC shortcut of `target`.
pub fn unlearn(p: &Project, target: &str) -> Vec<Command> {
    p.shortcuts
        .iter()
        .filter(|s| s.target == target && is_osc(&s.source))
        .map(|s| Command::RemoveShortcut { id: s.id })
        .collect()
}

/// Whether a message may be learned: our own address space (`/comp/...`, e.g. an echo of
/// our feedback) is not a controller.
pub fn learnable(ev: &Event) -> bool {
    !ev.address.starts_with("/comp/")
}

fn is_osc(src: &ControlSource) -> bool {
    matches!(src, ControlSource::Osc { .. })
}

/// Commands that map the OSC source to `target`: the target's old OSC shortcut and any other
/// target on the same address go away first. Key and MIDI shortcuts are untouched.
pub fn learn(p: &Project, target: &str, source: ControlSource) -> Vec<Command> {
    let same = |s: &Shortcut| s.target == target && s.source == source;
    let mut cmds: Vec<Command> = p
        .shortcuts
        .iter()
        .filter(|s| !same(s) && ((s.target == target && is_osc(&s.source)) || s.source == source))
        .map(|s| Command::RemoveShortcut { id: s.id })
        .collect();
    // A fader streams values: once mapped, further messages change nothing.
    if p.shortcuts.iter().any(same) {
        return cmds;
    }
    let (mode, range) = if shortcuts::is_continuous(target) {
        (ShortcutMode::Absolute, (0.0, 1.0))
    } else if target.ends_with("/bpm") {
        (
            ShortcutMode::Absolute,
            (crate::clock::MIN_BPM, crate::clock::MAX_BPM),
        )
    } else if target.ends_with("/bypass") {
        (ShortcutMode::Toggle, (0.0, 1.0))
    } else {
        (ShortcutMode::Trigger, (0.0, 1.0))
    };
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

/// Address of the OSC control mapped to `target`, for the overlay.
pub fn label_for(p: &Project, target: &str) -> Option<String> {
    p.shortcuts
        .iter()
        .filter(|s| s.target == target)
        .find_map(|s| match &s.source {
            ControlSource::Osc { address } => Some(address.clone()),
            _ => None,
        })
}

/// Which deck/column plays on a layer; the app owns that state.
pub type Active<'a> = &'a dyn Fn(LayerId) -> Option<(DeckId, ColumnIdx)>;

/// Output state in our own address space: clip playing per layer (1 on the playing clip),
/// layer and master opacity, bpm, and the beat index.
pub fn out_states(p: &Project, ctx: &Ctx, active: Active, beat: i64) -> HashMap<String, f64> {
    let deck = p.composition.current_deck;
    let mut out = HashMap::new();
    for l in &p.composition.layers {
        if let Some(a) = shortcuts::layer_address(p, l.id, "opacity")
            && let Some(v) = shortcuts::current(p, ctx, &a)
        {
            out.insert(a, v);
        }
        if let Some((d, col)) = active(l.id)
            && d == deck
            && let Some(a) = shortcuts::clip_address(p, l.id, col)
        {
            out.insert(a, 1.0);
        }
    }
    for a in ["/comp/master/opacity", "/comp/tempo/bpm"] {
        if let Some(v) = shortcuts::current(p, ctx, a) {
            out.insert(a.into(), v);
        }
    }
    out.insert("/comp/tempo/beat".into(), beat as f64);
    out
}

/// Messages needed to go from `prev` to `next`: new or changed values; addresses that
/// vanished while non-zero (a clip that stopped) send 0. Sorted, so deterministic.
pub fn out_diff(prev: &HashMap<String, f64>, next: &HashMap<String, f64>) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = next
        .iter()
        .filter(|(k, v)| prev.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), *v))
        .chain(
            prev.iter()
                .filter(|(k, v)| **v != 0.0 && !next.contains_key(*k))
                .map(|(k, _)| (k.clone(), 0.0)),
        )
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Per-address rate limit for output.
#[derive(Debug, Default)]
pub struct RateLimit {
    last: HashMap<String, Instant>,
}

impl RateLimit {
    /// Keeps the messages whose address was not sent within 1/MAX_RATE s, and records them.
    /// The rest are dropped: the caller must not count them as sent, so the next diff
    /// offers them again.
    pub fn filter(&mut self, msgs: Vec<(String, f64)>, now: Instant) -> Vec<(String, f64)> {
        let gap = Duration::from_secs_f64(1.0 / MAX_RATE);
        msgs.into_iter()
            .filter(|(a, _)| {
                let ok = self
                    .last
                    .get(a)
                    .is_none_or(|t| now.saturating_duration_since(*t) >= gap);
                if ok {
                    self.last.insert(a.clone(), now);
                }
                ok
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn non_finite_value_is_dropped() {
        let pkt = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/comp/master/opacity".into(),
            args: vec![rosc::OscType::Float(f32::NAN)],
        }))
        .unwrap();
        assert!(super::decode(&pkt).is_empty());
    }

    use super::*;
    use rosc::{OscBundle, OscTime};

    fn layer() -> vjc_model::Layer {
        vjc_model::Layer {
            id: uuid::Uuid::new_v4(),
            name: "L".into(),
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
            group: None,
        }
    }

    fn msg(addr: &str, args: Vec<OscType>) -> OscPacket {
        OscPacket::Message(OscMessage {
            addr: addr.into(),
            args,
        })
    }

    fn bytes(p: &OscPacket) -> Vec<u8> {
        rosc::encoder::encode(p).unwrap()
    }

    #[test]
    fn decodes_values_and_trigger() {
        let f = decode(&bytes(&msg("/a", vec![OscType::Float(0.5)])));
        assert_eq!(f[0].value, Some(0.5));
        let i = decode(&bytes(&msg("/a", vec![OscType::Int(3)])));
        assert_eq!(i[0].value, Some(3.0));
        let b = decode(&bytes(&msg("/a", vec![OscType::Bool(true)])));
        assert_eq!(b[0].value, Some(1.0));
        let t = decode(&bytes(&msg("/a", vec![])));
        assert_eq!(t[0].value, None);
        assert!(decode(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn bundles_flatten_nested() {
        let inner = OscPacket::Bundle(OscBundle {
            timetag: OscTime::from((0, 1)),
            content: vec![msg("/b", vec![])],
        });
        let outer = OscPacket::Bundle(OscBundle {
            timetag: OscTime::from((0, 1)),
            content: vec![msg("/a", vec![OscType::Int(1)]), inner],
        });
        let ev = decode(&bytes(&outer));
        let names: Vec<_> = ev.iter().map(|e| e.address.as_str()).collect();
        assert_eq!(names, ["/a", "/b"]);
    }

    #[test]
    fn own_space_resolves_directly() {
        let mut p = Project::new("t");
        p.composition.layers.push(layer());
        let ctx = Ctx::new(None, false);
        let ev = Event {
            address: "/comp/layer/1/opacity".into(),
            value: Some(0.25),
        };
        let Some(Command::SetOpacity { value, .. }) = direct(&p, &ctx, &ev) else {
            panic!("no command");
        };
        assert_eq!(value, 0.25);
        let trig = Event {
            address: "/comp/clear".into(),
            value: None,
        };
        assert!(matches!(direct(&p, &ctx, &trig), Some(Command::ClearAll)));
        let unknown = Event {
            address: "/nope".into(),
            value: None,
        };
        assert!(direct(&p, &ctx, &unknown).is_none());
    }

    #[test]
    fn learned_external_address() {
        let p = Project::new("t");
        let ctx = Ctx::new(None, false);
        let src = ControlSource::Osc {
            address: "/1/fader1".into(),
        };
        let cmds = learn(&p, "/comp/tempo/bpm", src);
        let Some(Command::AddShortcut { shortcut, .. }) = cmds.last() else {
            panic!("no add");
        };
        let ev = Event {
            address: "/1/fader1".into(),
            value: Some(1.0),
        };
        let Some(Command::SetBpm { bpm }) = command_for(&p, &ctx, shortcut, &ev) else {
            panic!("no bpm");
        };
        assert_eq!(bpm, crate::clock::MAX_BPM);
        let other = Event {
            address: "/x".into(),
            value: Some(1.0),
        };
        assert!(command_for(&p, &ctx, shortcut, &other).is_none());
        assert_eq!(label_for(&p, "/comp/tempo/bpm"), None);
    }

    #[test]
    fn learn_ignores_own_space_and_is_idempotent() {
        let ev = |a: &str| Event {
            address: a.into(),
            value: Some(0.5),
        };
        assert!(!learnable(&ev("/comp/master/opacity")));
        assert!(learnable(&ev("/1/fader1")));
        let mut p = Project::new("t");
        let src = ControlSource::Osc {
            address: "/1/fader1".into(),
        };
        let first = learn(&p, "/comp/master/opacity", src.clone());
        let Some(Command::AddShortcut { shortcut, .. }) = first.last() else {
            panic!("no add");
        };
        p.shortcuts.push(shortcut.clone());
        assert!(learn(&p, "/comp/master/opacity", src).is_empty());
        assert_eq!(unlearn(&p, "/comp/master/opacity").len(), 1);
    }

    #[test]
    fn diff_sends_changes_and_zeroes_vanished() {
        let prev: HashMap<String, f64> =
            [("/a".into(), 1.0), ("/b".into(), 0.5), ("/c".into(), 0.0)].into();
        let next: HashMap<String, f64> = [("/b".into(), 0.5), ("/d".into(), 2.0)].into();
        assert_eq!(
            out_diff(&prev, &next),
            vec![("/a".into(), 0.0), ("/d".into(), 2.0)]
        );
        assert!(out_diff(&next, &next).is_empty());
    }

    #[test]
    fn rate_limit_per_address() {
        let mut r = RateLimit::default();
        let t = Instant::now();
        let m = vec![("/a".to_string(), 1.0), ("/b".to_string(), 1.0)];
        assert_eq!(r.filter(m.clone(), t).len(), 2);
        assert!(r.filter(m.clone(), t + Duration::from_millis(5)).is_empty());
        let one = vec![("/a".to_string(), 2.0)];
        assert_eq!(r.filter(one, t + Duration::from_millis(17)).len(), 1);
    }
}
