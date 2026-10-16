//! Wave O: multi-field transform command used by the preview transform widget.

use super::tests::setup;
use super::*;

fn read(p: &mut Project, t: EffectTarget, f: TransformField) -> f64 {
    match effects::track_mut(p, t)
        .unwrap()
        .transform
        .field_mut(f)
        .value
    {
        Value::Number(n) => n,
        _ => f64::NAN,
    }
}

fn multi(t: EffectTarget, x: f64, y: f64) -> Command {
    Command::SetTransformMulti {
        target: t,
        values: vec![(TransformField::X, x), (TransformField::Y, y)],
    }
}

#[test]
fn transform_multi_drag_is_one_undo_step() {
    let (mut p, l) = setup();
    let t = EffectTarget::Layer(l);
    let mut h = History::default();
    let now = Instant::now();
    for i in 1..=5 {
        h.exec(&mut p, multi(t, i as f64 * 10.0, -(i as f64)), now)
            .unwrap();
    }
    assert_eq!(read(&mut p, t, TransformField::X), 50.0);
    assert_eq!(read(&mut p, t, TransformField::Y), -5.0);
    h.undo(&mut p).unwrap().unwrap();
    assert_eq!(read(&mut p, t, TransformField::X), 0.0);
    assert_eq!(read(&mut p, t, TransformField::Y), 0.0);
    assert!(h.undo(&mut p).is_none(), "whole drag was one step");
    h.redo(&mut p).unwrap().unwrap();
    assert_eq!(read(&mut p, t, TransformField::X), 50.0);
}

#[test]
fn transform_multi_rejects_non_finite_and_empty() {
    let (mut p, l) = setup();
    let t = EffectTarget::Layer(l);
    assert!(apply(&mut p, multi(t, 5.0, f64::NAN)).is_err());
    assert_eq!(read(&mut p, t, TransformField::X), 0.0, "all or nothing");
    let empty = Command::SetTransformMulti {
        target: t,
        values: vec![],
    };
    assert!(apply(&mut p, empty).is_err());
}
