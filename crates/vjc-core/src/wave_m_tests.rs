//! Wave M: layer master / size, clip duration, alpha type, clip autopilot, freeze, OSC.

use super::tests::setup;
use super::*;
use vjc_model::{AutoSize, Autopilot, AutopilotMode, Channels, LayerExtras};

const A: DeckId = uuid::Uuid::from_u128(1);

fn with_clip() -> (Project, LayerId) {
    let (mut p, l) = setup();
    let mut h = History::default();
    h.exec(
        &mut p,
        Command::ImportMedia {
            deck: A,
            layer: l,
            column: 0,
            path: "/x/a.mov".into(),
        },
        Instant::now(),
    )
    .unwrap();
    (p, l)
}

fn undo_roundtrip(p: &mut Project, cmd: Command) {
    let before = p.clone();
    let inv = apply(p, cmd).unwrap().inverse.unwrap();
    assert_ne!(*p, before);
    apply(p, inv).unwrap();
    assert_eq!(*p, before);
}

fn osc(p: &Project, address: &str, value: f64, layer: LayerId) -> Option<Command> {
    let ctx = shortcuts::Ctx::new(None, false).with_active(vec![(layer, A, 0)]);
    shortcuts::resolve_with(p, &ctx, address, value)
}

#[test]
fn layer_extras_validate_and_undo() {
    let (mut p, l) = with_clip();
    let ok = LayerExtras {
        master: 0.5,
        auto_size: AutoSize::Fill,
        width: 800,
        height: 600,
    };
    undo_roundtrip(
        &mut p,
        Command::SetLayerExtras {
            layer: l,
            extras: ok.clone(),
        },
    );
    for bad in [
        LayerExtras {
            master: 1.5,
            ..ok.clone()
        },
        LayerExtras {
            master: f64::NAN,
            ..ok.clone()
        },
        LayerExtras {
            width: vjc_model::MAX_LAYER_SIDE + 1,
            ..ok.clone()
        },
    ] {
        assert!(
            apply(
                &mut p,
                Command::SetLayerExtras {
                    layer: l,
                    extras: bad
                }
            )
            .is_err()
        );
    }
}

#[test]
fn clip_duration_validates_and_undoes() {
    let (mut p, l) = with_clip();
    let set = |secs| Command::SetClipDuration {
        deck: A,
        layer: l,
        column: 0,
        secs,
    };
    undo_roundtrip(&mut p, set(Some(12.5)));
    assert!(apply(&mut p, set(Some(0.0))).is_err());
    assert!(apply(&mut p, set(Some(f64::INFINITY))).is_err());
    assert!(apply(&mut p, set(Some(-3.0))).is_err());
    let missing = Command::SetClipDuration {
        deck: A,
        layer: l,
        column: 5,
        secs: Some(1.0),
    };
    assert!(apply(&mut p, missing).is_err());
}

#[test]
fn osc_addresses_reach_the_new_fields() {
    let (mut p, l) = with_clip();
    let run = |p: &mut Project, addr: &str, v: f64| {
        let c = osc(p, addr, v, l).unwrap_or_else(|| panic!("{addr} unresolved"));
        apply(p, c).unwrap_or_else(|e| panic!("{addr}: {e:?}"));
    };
    run(&mut p, "/comp/layer/1/master", 0.25);
    run(&mut p, "/comp/layer/1/auto_size", 2.0 / 3.0);
    run(&mut p, "/comp/layer/1/width", 0.5);
    let e = &p.composition.layers[0].extras;
    assert_eq!(
        (e.master, e.auto_size, e.width),
        (0.25, AutoSize::Fit, 8192)
    );
    run(&mut p, "/comp/layer/1/clip/duration", 0.1);
    run(&mut p, "/comp/layer/1/clip/alpha", 1.0);
    run(&mut p, "/comp/layer/1/clip/freeze", 1.0);
    run(&mut p, "/comp/layer/1/clip/stop_motion", 0.5);
    run(&mut p, "/comp/layer/1/clip/autopilot", 0.4); // 2 = Next
    run(&mut p, "/comp/layer/1/clip/autopilot_loops", 0.25);
    let k = clip_in(&p, A, l, 0).unwrap();
    assert!((k.transport.duration_secs.unwrap() - 60.0).abs() < 1e-9);
    assert!(k.channels.premultiplied && k.extras.freeze);
    assert_eq!(k.extras.stop_motion_fps, 30.0);
    let ap = k.extras.autopilot.as_ref().unwrap();
    assert_eq!((ap.mode, ap.every_loops), (AutopilotMode::Next, 16));
    // 0 hands the clip back to the layer's autopilot; 0 duration is free speed.
    run(&mut p, "/comp/layer/1/clip/autopilot", 0.0);
    run(&mut p, "/comp/layer/1/clip/duration", 0.0);
    let k = clip_in(&p, A, l, 0).unwrap();
    assert!(k.extras.autopilot.is_none() && k.transport.duration_secs.is_none());
    // Negative controls: a layer that is not there, and a clip address with no clip playing.
    assert!(osc(&p, "/comp/layer/9/master", 0.5, l).is_none());
    let ctx = shortcuts::Ctx::new(None, false);
    assert!(shortcuts::resolve_with(&p, &ctx, "/comp/layer/1/clip/freeze", 1.0).is_none());
}

#[test]
fn refresh_thumb_is_a_runtime_event() {
    let (mut p, l) = with_clip();
    let c = osc(&p, "/comp/layer/1/clip/refresh_thumb", 1.0, l).unwrap();
    let a = apply(&mut p, c).unwrap();
    assert!(a.inverse.is_none());
    assert!(matches!(
        a.events.as_slice(),
        [Event::ThumbRefresh { column: 0, .. }]
    ));
    // Release of a button (value 0) does nothing; an empty cell is an error.
    assert!(osc(&p, "/comp/layer/1/clip/refresh_thumb", 0.0, l).is_none());
    let empty = Command::RefreshClipThumb {
        deck: A,
        layer: l,
        column: 7,
    };
    assert!(apply(&mut p, empty).is_err());
}

#[test]
fn old_projects_load_with_defaults() {
    let (mut p, l) = with_clip();
    let k = {
        let deck = p.composition.decks.iter_mut().find(|d| d.id == A).unwrap();
        deck.clips.iter_mut().find(|k| k.layer == l).unwrap()
    };
    k.clip.transport.duration_secs = Some(9.0);
    k.clip.channels.premultiplied = true;
    k.clip.extras.freeze = true;
    p.composition.layers[0].extras.master = 0.3;
    let mut v = serde_json::to_value(&p).unwrap();
    // Strip every new key the way a file from before this wave would lack them.
    fn strip(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                for k in [
                    "extras",
                    "duration_secs",
                    "premultiplied",
                    "compensate_transition",
                ] {
                    let layer_extras = m.get(k).is_some_and(|e| e.get("master").is_some());
                    let transport_secs = m.contains_key("play_mode");
                    if (k == "extras" && !layer_extras) || (k == "duration_secs" && !transport_secs)
                    {
                        continue;
                    }
                    m.remove(k);
                }
                m.values_mut().for_each(strip);
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut v);
    let old: Project = serde_json::from_value(v).unwrap();
    let ex = &old.composition.layers[0].extras;
    assert_eq!((ex.master, ex.auto_size, ex.width), (1.0, AutoSize::Off, 0));
    let k = clip_in(&old, A, l, 0).unwrap();
    assert!(k.transport.duration_secs.is_none() && !k.channels.premultiplied);
    // The strip left the clip's own extras (freeze) alone, and Channels default is straight.
    assert!(k.extras.freeze);
    assert_eq!(Channels::default().off_mask(), 0);
    assert_eq!(
        Channels {
            premultiplied: true,
            ..Default::default()
        }
        .off_mask(),
        16
    );
}

#[test]
fn clip_autopilot_rules_the_layer_only_while_it_plays() {
    let (mut p, l) = with_clip();
    let layer_ap = Autopilot {
        mode: AutopilotMode::Next,
        ..p.composition.layers[0].autopilot.clone()
    };
    p.composition.layers[0].autopilot = layer_ap.clone();
    let own = Autopilot {
        mode: AutopilotMode::Random,
        every_loops: 3,
        ..layer_ap.clone()
    };
    let mut extras = clip_in(&p, A, l, 0).unwrap().extras.clone();
    extras.autopilot = Some(own.clone());
    let set = Command::SetClipExtras {
        deck: A,
        layer: l,
        column: 0,
        extras,
    };
    undo_roundtrip(&mut p.clone(), set.clone());
    apply(&mut p, set).unwrap();
    let layer = &p.composition.layers[0];
    let clip = clip_in(&p, A, l, 0);
    // No override: the layer's. Override set: the clip's.
    assert_eq!(playback::autopilot_of(layer, None), &layer_ap);
    assert_eq!(playback::autopilot_of(layer, clip), &own);
    let mut plain = clip.unwrap().clone();
    plain.extras.autopilot = None;
    assert_eq!(playback::autopilot_of(layer, Some(&plain)), &layer_ap);
}
