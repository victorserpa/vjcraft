//! Wave J: colours, description, clip extras, effect chains, folds.

use super::tests::setup;
use super::*;
use vjc_model::{BlendMode, ClipExtras, Effect};

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

fn fx(kind: &str) -> Effect {
    Effect {
        id: uuid::Uuid::new_v4(),
        kind: kind.into(),
        bypass: false,
        dry_wet: 1.0,
        params: vec![],
        blend: BlendMode::Normal,
    }
}

fn undo_roundtrip(p: &mut Project, cmd: Command) {
    let before = p.clone();
    let inv = apply(p, cmd.clone()).unwrap().inverse.unwrap();
    assert_ne!(*p, before);
    apply(p, inv).unwrap();
    assert_eq!(*p, before);
}

#[test]
fn colors_are_undoable_and_validated() {
    let (mut p, l) = with_clip();
    let col = p.composition.columns[0].id;
    for target in [
        ColorTarget::Layer(l),
        ColorTarget::Column(col),
        ColorTarget::Clip {
            deck: A,
            layer: l,
            column: 0,
        },
    ] {
        undo_roundtrip(
            &mut p,
            Command::SetColor {
                target,
                color: Some(3),
            },
        );
        assert!(
            apply(
                &mut p,
                Command::SetColor {
                    target,
                    color: Some(7)
                }
            )
            .is_err()
        );
    }
}

#[test]
fn description_and_folds_roundtrip_and_old_json_loads() {
    let (mut p, l) = with_clip();
    undo_roundtrip(&mut p, Command::SetCompDescription { text: "set".into() });
    undo_roundtrip(
        &mut p,
        Command::SetLayerFolded {
            layer: l,
            folded: true,
        },
    );
    let mut v = serde_json::to_value(&p.composition).unwrap();
    v.as_object_mut().unwrap().remove("description");
    let l0 = &mut v["layers"][0];
    for k in ["color", "folded"] {
        l0.as_object_mut().unwrap().remove(k);
    }
    v["columns"][0].as_object_mut().unwrap().remove("color");
    let c: vjc_model::Composition = serde_json::from_value(v).unwrap();
    assert_eq!(c.description, "");
    assert!(!c.layers[0].folded && c.layers[0].color.is_none());
}

#[test]
fn group_fold_roundtrips() {
    let (mut p, l) = with_clip();
    let g = vjc_model::LayerGroup::new("G");
    let gid = g.id;
    p.composition.groups.push(g);
    p.composition.layers[0].group = Some(gid);
    let _ = l;
    undo_roundtrip(
        &mut p,
        Command::SetGroupFolded {
            group: gid,
            folded: true,
        },
    );
}

#[test]
fn clip_extras_ignore_column_and_fader_start_override_the_layer() {
    let (mut p, l) = with_clip();
    undo_roundtrip(
        &mut p,
        Command::SetClipExtras {
            deck: A,
            layer: l,
            column: 0,
            extras: ClipExtras {
                ignore_column_trigger: Some(true),
                strip_audio: true,
                ..ClipExtras::default()
            },
        },
    );
    let touches = |ev: &[Event]| {
        ev.iter().any(|e| {
            matches!(e, Event::LayerCleared { layer } | Event::Skipped { layer, .. } if *layer == l)
                || matches!(e, Event::ClipStarted { layer, .. } if *layer == l)
        })
    };
    apply(
        &mut p,
        Command::SetClipExtras {
            deck: A,
            layer: l,
            column: 0,
            extras: ClipExtras {
                ignore_column_trigger: Some(true),
                ..ClipExtras::default()
            },
        },
    )
    .unwrap();
    let ev = apply(&mut p, Command::TriggerColumn { deck: A, column: 0 })
        .unwrap()
        .events;
    assert!(!touches(&ev), "clip override skips the layer: {ev:?}");
    // Some(false) beats the layer flag.
    p.composition.layers[0].ignore_column_trigger = true;
    p.composition.decks[0].clips[0]
        .clip
        .extras
        .ignore_column_trigger = Some(false);
    let ev = apply(&mut p, Command::TriggerColumn { deck: A, column: 0 })
        .unwrap()
        .events;
    assert!(!ev.is_empty() && touches(&ev), "{ev:?}");

    // Fader start only through the clip override.
    let rise = |p: &mut Project| {
        apply(
            p,
            Command::SetOpacity {
                layer: l,
                value: 0.0,
            },
        )
        .unwrap();
        apply(
            p,
            Command::SetOpacity {
                layer: l,
                value: 1.0,
            },
        )
        .unwrap()
        .events
        .iter()
        .any(|e| matches!(e, Event::FaderStart { .. }))
    };
    p.composition.layers[0].fader_start = false;
    assert!(!rise(&mut p));
    p.composition.decks[0].clips[0].clip.extras.fader_start = Some(true);
    assert!(rise(&mut p));
}

#[test]
fn replace_effects_pastes_a_chain_and_undoes() {
    let (mut p, l) = with_clip();
    let t = effects::EffectTarget::Clip {
        deck: A,
        layer: l,
        column: 0,
    };
    undo_roundtrip(
        &mut p,
        Command::ReplaceEffects {
            target: t,
            effects: vec![fx("blur"), fx("invert")],
        },
    );
    apply(
        &mut p,
        Command::ReplaceEffects {
            target: t,
            effects: vec![fx("blur")],
        },
    )
    .unwrap();
    let id = effects::chain(&p, t).unwrap()[0].id;
    undo_roundtrip(
        &mut p,
        Command::SetEffectBlend {
            target: t,
            effect: id,
            blend: BlendMode::Multiply,
        },
    );
}

#[test]
fn effect_blend_defaults_to_normal_for_old_files() {
    let mut v = serde_json::to_value(fx("blur")).unwrap();
    v.as_object_mut().unwrap().remove("blend");
    let e: Effect = serde_json::from_value(v).unwrap();
    assert_eq!(e.blend, BlendMode::Normal);
}
