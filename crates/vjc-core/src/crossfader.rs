//! Crossfader A/B: pure gain curves and the helpers the compositor and UI share.
//! Position 0 = all A, 1 = all B.

use vjc_model::{Bus, CrossfadeBehaviour, Crossfader, Param};

/// Fade law. `ConstantPower` is the default: cos/sin keeps perceived energy flat at the
/// middle (both buses at ~0.707), where `Linear` dips (0.5 + 0.5) and `Cut` switches
/// abruptly at the middle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrossfadeCurve {
    Linear,
    #[default]
    ConstantPower,
    Cut,
    /// Smoothstep: eases in and out around both ends, linear sum 1.
    Gentle,
    /// Both buses stay full until the fader leaves the middle (no dip, no dark centre).
    Fast,
}

impl CrossfadeCurve {
    pub const ALL: [CrossfadeCurve; 5] = [
        Self::Linear,
        Self::ConstantPower,
        Self::Cut,
        Self::Gentle,
        Self::Fast,
    ];

    /// Name stored in the project (`Crossfader.curve`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::ConstantPower => "ConstantPower",
            Self::Cut => "Cut",
            Self::Gentle => "Gentle",
            Self::Fast => "Fast",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::ConstantPower => "Constant power",
            Self::Cut => "Cut",
            Self::Gentle => "Gentle",
            Self::Fast => "Fast",
        }
    }

    /// Unknown names fall back to the default.
    pub fn from_name(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|c| c.name() == s)
            .unwrap_or_default()
    }
}

/// Position used when no crossfader exists yet.
pub const CENTER: f64 = 0.5;

/// (gain A, gain B) at `pos` (clamped to 0..=1). Cut: A below the middle, B above, both
/// at the exact middle.
pub fn gains(pos: f64, curve: CrossfadeCurve) -> (f64, f64) {
    let t = if pos.is_finite() {
        pos.clamp(0.0, 1.0)
    } else {
        CENTER
    };
    match curve {
        CrossfadeCurve::Linear => (1.0 - t, t),
        CrossfadeCurve::ConstantPower => {
            let a = t * std::f64::consts::FRAC_PI_2;
            (a.cos(), a.sin())
        }
        CrossfadeCurve::Gentle => {
            let s = t * t * (3.0 - 2.0 * t);
            (1.0 - s, s)
        }
        CrossfadeCurve::Fast => ((2.0 * (1.0 - t)).min(1.0), (2.0 * t).min(1.0)),
        CrossfadeCurve::Cut => {
            if t < 0.5 {
                (1.0, 0.0)
            } else if t > 0.5 {
                (0.0, 1.0)
            } else {
                (1.0, 1.0)
            }
        }
    }
}

/// Multiplier applied to a layer's opacity (1 without a bus). With no
/// crossfader yet, the default curve is used, so the first touch of the
/// fader does not make bus layers jump.
pub fn layer_gain(cf: Option<&Crossfader>, bus: Option<&Bus>, position: f64) -> f64 {
    let Some(bus) = bus else {
        return 1.0;
    };
    if cf.is_some_and(|c| c.behaviour == CrossfadeBehaviour::Jump) {
        let to_b = position.is_finite() && position >= 0.5;
        return if matches!(bus, Bus::B) == to_b {
            1.0
        } else {
            0.0
        };
    }
    let curve = cf.map_or(CrossfadeCurve::default(), |c| match c.behaviour {
        CrossfadeBehaviour::Cut | CrossfadeBehaviour::CutReturn => CrossfadeCurve::Cut,
        CrossfadeBehaviour::Fade | CrossfadeBehaviour::Jump => CrossfadeCurve::from_name(&c.curve),
    });
    let (a, b) = gains(position, curve);
    match bus {
        Bus::A => a,
        Bus::B => b,
    }
}

/// Bus a layer is mixed on: its own, else its group's.
pub fn effective_bus<'a>(
    layer: &'a vjc_model::Layer,
    groups: &'a [vjc_model::LayerGroup],
) -> Option<&'a Bus> {
    layer.crossfader_bus.as_ref().or_else(|| {
        let g = layer.group?;
        groups
            .iter()
            .find(|x| x.id == g)
            .and_then(|x| x.crossfader_bus.as_ref())
    })
}

/// Blend mode of a layer: the crossfader's own when the layer sits on a bus and the
/// crossfader sets one, else the layer's.
pub fn layer_blend(
    cf: Option<&Crossfader>,
    bus: Option<&Bus>,
    own: &vjc_model::BlendMode,
) -> vjc_model::BlendMode {
    match (bus, cf.and_then(|c| c.blend.as_ref())) {
        (Some(_), Some(b)) => b.clone(),
        _ => own.clone(),
    }
}

pub fn new_crossfader() -> Crossfader {
    Crossfader {
        position: Param::number("Position", CENTER, Some((0.0, 1.0))),
        curve: CrossfadeCurve::default().name().to_string(),
        behaviour: Default::default(),
        blend: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn linear() {
        assert!(close(gains(0.0, CrossfadeCurve::Linear), (1.0, 0.0)));
        assert!(close(gains(0.5, CrossfadeCurve::Linear), (0.5, 0.5)));
        assert!(close(gains(1.0, CrossfadeCurve::Linear), (0.0, 1.0)));
    }

    #[test]
    fn constant_power() {
        let c = CrossfadeCurve::ConstantPower;
        assert!(close(gains(0.0, c), (1.0, 0.0)));
        assert!(close(gains(1.0, c), (0.0, 1.0)));
        let h = std::f64::consts::FRAC_1_SQRT_2;
        assert!(close(gains(0.5, c), (h, h)));
        let (a, b) = gains(0.3, c);
        assert!((a * a + b * b - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cut() {
        let c = CrossfadeCurve::Cut;
        assert!(close(gains(0.0, c), (1.0, 0.0)));
        assert!(close(gains(0.49, c), (1.0, 0.0)));
        assert!(close(gains(0.5, c), (1.0, 1.0)));
        assert!(close(gains(0.51, c), (0.0, 1.0)));
        assert!(close(gains(1.0, c), (0.0, 1.0)));
    }

    #[test]
    fn gentle() {
        let c = CrossfadeCurve::Gentle;
        assert!(close(gains(0.0, c), (1.0, 0.0)));
        assert!(close(gains(0.5, c), (0.5, 0.5)));
        assert!(close(gains(1.0, c), (0.0, 1.0)));
        // Eases: slower than linear near the ends.
        assert!(gains(0.1, c).1 < 0.1);
        let (a, b) = gains(0.3, c);
        assert!((a + b - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fast() {
        let c = CrossfadeCurve::Fast;
        assert!(close(gains(0.0, c), (1.0, 0.0)));
        assert!(close(gains(0.5, c), (1.0, 1.0)));
        assert!(close(gains(0.25, c), (1.0, 0.5)));
        assert!(close(gains(1.0, c), (0.0, 1.0)));
    }

    #[test]
    fn jump_and_cut_return_behaviours() {
        let mut cf = new_crossfader();
        cf.behaviour = CrossfadeBehaviour::Jump;
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::A), 0.49), 1.0);
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::B), 0.49), 0.0);
        // The exact middle belongs to B (Cut would give both).
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::A), 0.5), 0.0);
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::B), 0.5), 1.0);
        cf.behaviour = CrossfadeBehaviour::CutReturn;
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::A), 0.5), 1.0);
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::B), 0.5), 1.0);
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::B), 0.2), 0.0);
    }

    #[test]
    fn group_bus_is_inherited_unless_the_layer_has_its_own() {
        let mut g = vjc_model::LayerGroup::new("g");
        g.crossfader_bus = Some(Bus::B);
        let mut l = crate::tests::layer(1);
        l.group = Some(g.id);
        let gs = [g];
        assert_eq!(effective_bus(&l, &gs), Some(&Bus::B));
        l.crossfader_bus = Some(Bus::A);
        assert_eq!(effective_bus(&l, &gs), Some(&Bus::A));
        l.group = None;
        l.crossfader_bus = None;
        assert_eq!(effective_bus(&l, &gs), None);
    }

    #[test]
    fn clamps_and_names() {
        assert!(close(gains(-3.0, CrossfadeCurve::Linear), (1.0, 0.0)));
        assert!(close(gains(9.0, CrossfadeCurve::Linear), (0.0, 1.0)));
        for c in CrossfadeCurve::ALL {
            assert_eq!(CrossfadeCurve::from_name(c.name()), c);
        }
        assert_eq!(
            CrossfadeCurve::from_name("?"),
            CrossfadeCurve::ConstantPower
        );
    }

    #[test]
    fn layer_gain_ignores_missing_bus() {
        let cf = new_crossfader();
        assert_eq!(layer_gain(Some(&cf), None, 0.0), 1.0);
        // Without a crossfader the app passes the center position (see first_touch_tests).
        assert!(layer_gain(Some(&cf), Some(&Bus::B), 0.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod first_touch_tests {
    use super::*;

    #[test]
    fn no_crossfader_matches_default_at_center() {
        let none = layer_gain(None, Some(&Bus::A), 0.5);
        let (a, _) = gains(0.5, CrossfadeCurve::default());
        assert!((none - a).abs() < 1e-12);
        assert_eq!(layer_gain(None, None, 0.5), 1.0);
    }

    #[test]
    fn cut_behaviour_overrides_the_curve() {
        let mut cf = new_crossfader();
        cf.curve = CrossfadeCurve::Linear.name().to_string();
        assert!((layer_gain(Some(&cf), Some(&Bus::A), 0.25) - 0.75).abs() < 1e-9);
        cf.behaviour = CrossfadeBehaviour::Cut;
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::A), 0.25), 1.0);
        assert_eq!(layer_gain(Some(&cf), Some(&Bus::B), 0.25), 0.0);
    }

    #[test]
    fn crossfader_blend_replaces_the_blend_of_bus_layers_only() {
        use vjc_model::BlendMode;
        let mut cf = new_crossfader();
        assert_eq!(
            layer_blend(Some(&cf), Some(&Bus::A), &BlendMode::Multiply),
            BlendMode::Multiply
        );
        cf.blend = Some(BlendMode::Add);
        assert_eq!(
            layer_blend(Some(&cf), Some(&Bus::B), &BlendMode::Multiply),
            BlendMode::Add
        );
        assert_eq!(
            layer_blend(Some(&cf), None, &BlendMode::Multiply),
            BlendMode::Multiply
        );
        assert_eq!(
            layer_blend(None, Some(&Bus::A), &BlendMode::Multiply),
            BlendMode::Multiply
        );
    }
}

#[cfg(test)]
mod go_tests {
    use crate::{Command, apply};
    use vjc_model::{Bus, CrossfadeBehaviour, Project};

    fn pos(p: &Project) -> f64 {
        crate::number(&p.composition.crossfader.as_ref().unwrap().position.value)
    }

    #[test]
    fn go_sets_the_ends_and_undoes() {
        let mut p = Project::new("t");
        let r = apply(
            &mut p,
            Command::GoCrossfader {
                bus: Bus::B,
                pressed: true,
            },
        )
        .unwrap();
        assert_eq!(pos(&p), 1.0);
        apply(&mut p, r.inverse.unwrap()).unwrap();
        assert_eq!(pos(&p), 0.5);
        apply(
            &mut p,
            Command::GoCrossfader {
                bus: Bus::A,
                pressed: true,
            },
        )
        .unwrap();
        assert_eq!(pos(&p), 0.0);
    }

    #[test]
    fn release_only_returns_with_cut_return() {
        let mut p = Project::new("t");
        apply(
            &mut p,
            Command::GoCrossfader {
                bus: Bus::B,
                pressed: true,
            },
        )
        .unwrap();
        let r = apply(
            &mut p,
            Command::GoCrossfader {
                bus: Bus::B,
                pressed: false,
            },
        )
        .unwrap();
        assert!(r.inverse.is_none());
        assert_eq!(pos(&p), 1.0);
        apply(
            &mut p,
            Command::SetCrossfaderBehaviour {
                behaviour: CrossfadeBehaviour::CutReturn,
            },
        )
        .unwrap();
        apply(
            &mut p,
            Command::GoCrossfader {
                bus: Bus::B,
                pressed: false,
            },
        )
        .unwrap();
        assert_eq!(pos(&p), 0.0);
    }

    #[test]
    fn group_bus_undo_and_unknown_group() {
        let mut p = Project::new("t");
        let g = vjc_model::LayerGroup::new("g");
        let id = g.id;
        p.composition.groups.push(g);
        let r = apply(
            &mut p,
            Command::SetGroupBus {
                group: id,
                bus: Some(Bus::A),
            },
        )
        .unwrap();
        assert_eq!(p.composition.groups[0].crossfader_bus, Some(Bus::A));
        apply(&mut p, r.inverse.unwrap()).unwrap();
        assert_eq!(p.composition.groups[0].crossfader_bus, None);
        assert!(
            apply(
                &mut p,
                Command::SetGroupBus {
                    group: uuid::Uuid::nil(),
                    bus: None
                }
            )
            .is_err()
        );
    }

    #[test]
    fn old_projects_load_without_the_new_fields() {
        let mut v = serde_json::to_value(vjc_model::LayerGroup::new("g")).unwrap();
        v.as_object_mut().unwrap().remove("crossfader_bus");
        let g: vjc_model::LayerGroup = serde_json::from_value(v).unwrap();
        assert_eq!(g.crossfader_bus, None);
        let b: CrossfadeBehaviour = serde_json::from_str("\"Cut\"").unwrap();
        assert_eq!(b, CrossfadeBehaviour::Cut);
    }

    #[test]
    fn addresses_resolve() {
        let p = Project::new("t");
        let ctx = crate::shortcuts::Ctx::new(None, false);
        let go = crate::shortcuts::resolve_with(&p, &ctx, "/comp/crossfader/go/b", 1.0);
        assert!(matches!(
            go,
            Some(Command::GoCrossfader {
                bus: Bus::B,
                pressed: true
            })
        ));
        let rel = crate::shortcuts::resolve_with(&p, &ctx, "/comp/crossfader/go/a", 0.0);
        assert!(matches!(
            rel,
            Some(Command::GoCrossfader {
                bus: Bus::A,
                pressed: false
            })
        ));
        let c = crate::shortcuts::resolve_with(&p, &ctx, "/comp/crossfader/curve", 3.0);
        assert!(matches!(
            c,
            Some(Command::SetCrossfaderCurve {
                curve: super::CrossfadeCurve::Gentle
            })
        ));
        let b = crate::shortcuts::resolve_with(&p, &ctx, "/comp/crossfader/behaviour", 9.0);
        assert!(matches!(
            b,
            Some(Command::SetCrossfaderBehaviour {
                behaviour: CrossfadeBehaviour::Jump
            })
        ));
    }
}
