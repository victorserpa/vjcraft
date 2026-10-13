//! Layer groups: adjacent layers composed together, then blended into the stack as one.
//! Invariants kept here: a group's layers are adjacent, groups never nest, a group is never
//! empty, `Composition::groups` is ordered like the layers.

use super::{Applied, Command, Error, layer_idx, number, undoable};
use vjc_model::{GroupId, LayerId, Project};

pub fn group_idx(p: &Project, id: GroupId) -> Result<usize, Error> {
    p.composition
        .groups
        .iter()
        .position(|g| g.id == id)
        .ok_or(Error::NoGroup)
}

/// Indices (top to bottom) of the layers in the group.
pub fn members(p: &Project, id: GroupId) -> Vec<usize> {
    p.composition
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.group == Some(id))
        .map(|(i, _)| i)
        .collect()
}

/// Group-vector slot for a group whose first layer sits at layer index `first`.
fn slot(p: &Project, first: usize) -> usize {
    p.composition
        .groups
        .iter()
        .filter(|g| members(p, g.id).first().is_some_and(|i| *i < first))
        .count()
}

fn check_group(p: &Project, id: GroupId) -> Result<usize, Error> {
    group_idx(p, id)
}

pub(crate) fn apply(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
    match cmd {
        Command::CreateGroup { group, layers } => {
            if layers.is_empty() || p.composition.groups.iter().any(|g| g.id == group.id) {
                return Err(Error::BadGroup);
            }
            let mut idx = layers
                .iter()
                .map(|l| layer_idx(p, *l))
                .collect::<Result<Vec<_>, _>>()?;
            idx.sort_unstable();
            let adjacent = idx.windows(2).all(|w| w[1] == w[0] + 1);
            if !adjacent || idx.iter().any(|i| p.composition.layers[*i].group.is_some()) {
                return Err(Error::BadGroup);
            }
            let at = slot(p, idx[0]);
            let id = group.id;
            for i in idx {
                p.composition.layers[i].group = Some(id);
            }
            p.composition.groups.insert(at, group);
            undoable(Command::Ungroup { group: id }, vec![])
        }
        Command::Ungroup { group } => {
            let gi = check_group(p, group)?;
            let layers: Vec<LayerId> = members(p, group)
                .into_iter()
                .map(|i| p.composition.layers[i].id)
                .collect();
            for l in &mut p.composition.layers {
                if l.group == Some(group) {
                    l.group = None;
                }
            }
            let g = p.composition.groups.remove(gi);
            undoable(Command::CreateGroup { group: g, layers }, vec![])
        }
        Command::RenameGroup { group, name } => {
            let gi = check_group(p, group)?;
            let prev = std::mem::replace(&mut p.composition.groups[gi].name, name);
            undoable(Command::RenameGroup { group, name: prev }, vec![])
        }
        Command::SetGroupOpacity { group, value } => {
            let gi = check_group(p, group)?;
            let op = &mut p.composition.groups[gi].track.opacity;
            let prev = number(&op.value);
            op.set_number(value);
            undoable(Command::SetGroupOpacity { group, value: prev }, vec![])
        }
        Command::SetGroupBlend { group, blend } => {
            let gi = check_group(p, group)?;
            let prev = std::mem::replace(&mut p.composition.groups[gi].track.blend, blend);
            undoable(Command::SetGroupBlend { group, blend: prev }, vec![])
        }
        Command::SetGroupBypass { group, bypass } => {
            let gi = check_group(p, group)?;
            let prev = std::mem::replace(&mut p.composition.groups[gi].bypass, bypass);
            undoable(
                Command::SetGroupBypass {
                    group,
                    bypass: prev,
                },
                vec![],
            )
        }
        Command::SetGroupSolo { group, solo } => {
            let gi = check_group(p, group)?;
            let prev = std::mem::replace(&mut p.composition.groups[gi].solo, solo);
            undoable(Command::SetGroupSolo { group, solo: prev }, vec![])
        }
        Command::SetGroupSpeed { group, value } => {
            if !value.is_finite() || !(0.0..=4.0).contains(&value) {
                return Err(Error::InvalidValue);
            }
            let gi = check_group(p, group)?;
            let prev = std::mem::replace(&mut p.composition.groups[gi].speed, value);
            undoable(Command::SetGroupSpeed { group, value: prev }, vec![])
        }
        Command::MoveLayerToGroup { layer, group } => {
            check_group(p, group)?;
            let li = layer_idx(p, layer)?;
            let m = members(p, group);
            let (first, last) = (m[0], m[m.len() - 1]);
            if p.composition.layers[li].group.is_some() || (li + 1 != first && li != last + 1) {
                return Err(Error::BadGroup);
            }
            p.composition.layers[li].group = Some(group);
            undoable(Command::MoveLayerOutOfGroup { layer }, vec![])
        }
        Command::MoveLayerOutOfGroup { layer } => {
            let li = layer_idx(p, layer)?;
            let group = p.composition.layers[li].group.ok_or(Error::BadGroup)?;
            let m = members(p, group);
            // Only an edge layer can leave (the rest stays adjacent), never the last one.
            if m.len() < 2 || (li != m[0] && li != m[m.len() - 1]) {
                return Err(Error::BadGroup);
            }
            p.composition.layers[li].group = None;
            undoable(Command::MoveLayerToGroup { layer, group }, vec![])
        }
        _ => Err(Error::InvalidValue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{self, EffectTarget};
    use crate::{History, shortcuts};
    use std::time::Instant;
    use uuid::Uuid;
    use vjc_model::{BlendMode, Layer, LayerGroup, Track, Transition};

    fn layer(name: &str) -> Layer {
        Layer {
            id: Uuid::new_v4(),
            name: name.into(),
            track: Track::new(BlendMode::Normal),
            solo: false,
            bypass: false,
            transition: Transition {
                kind: "cut".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            autopilot: Default::default(),
            crossfader_bus: None,
            group: None,
        }
    }

    /// Four layers, top to bottom: A B C D.
    fn setup() -> (Project, Vec<LayerId>) {
        let mut p = Project::new("t");
        p.composition.layers = ["A", "B", "C", "D"].map(layer).to_vec();
        let ids = p.composition.layers.iter().map(|l| l.id).collect();
        (p, ids)
    }

    fn exec(p: &mut Project, h: &mut History, c: Command) -> Result<(), Error> {
        h.exec(p, c, Instant::now()).map(|_| ())
    }

    fn create(p: &mut Project, h: &mut History, ls: &[LayerId]) -> Result<GroupId, Error> {
        let g = LayerGroup::new("G");
        let id = g.id;
        exec(
            p,
            h,
            Command::CreateGroup {
                group: g,
                layers: ls.to_vec(),
            },
        )
        .map(|_| id)
    }

    #[test]
    fn create_ungroup_undo_redo() {
        let (mut p, ids) = setup();
        let mut h = History::new();
        let before = p.clone();
        let g = create(&mut p, &mut h, &[ids[2], ids[1]]).unwrap();
        assert_eq!(p.composition.layers[1].group, Some(g));
        assert_eq!(p.composition.layers[2].group, Some(g));
        assert_eq!(p.composition.layers[0].group, None);
        let grouped = p.clone();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(p, grouped);
        exec(&mut p, &mut h, Command::Ungroup { group: g }).unwrap();
        assert_eq!(p, before);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, grouped);
    }

    #[test]
    fn normalize_groups_fixes_dangling_and_split_groups() {
        let (mut p, ids) = setup();
        let mut h = History::new();
        let g = create(&mut p, &mut h, &[ids[2], ids[1]]).unwrap();
        assert_eq!(p.normalize_groups(), 0);
        // Dangling id.
        p.composition.layers[0].group = Some(Uuid::new_v4());
        assert_eq!(p.normalize_groups(), 1);
        assert_eq!(p.composition.layers[0].group, None);
        // Split group: members no longer adjacent.
        p.composition.layers[0].group = Some(g);
        p.composition.layers[1].group = None;
        assert_eq!(p.normalize_groups(), 2);
        assert!(p.composition.layers.iter().all(|l| l.group.is_none()));
    }

    #[test]
    fn validation() {
        let (mut p, ids) = setup();
        let mut h = History::new();
        // Not adjacent, empty, unknown layer.
        assert_eq!(
            create(&mut p, &mut h, &[ids[0], ids[2]]).unwrap_err(),
            Error::BadGroup
        );
        assert_eq!(create(&mut p, &mut h, &[]).unwrap_err(), Error::BadGroup);
        assert_eq!(
            create(&mut p, &mut h, &[Uuid::new_v4()]).unwrap_err(),
            Error::NoLayer
        );
        let g = create(&mut p, &mut h, &[ids[1], ids[2]]).unwrap();
        // No nesting: a grouped layer cannot join another group.
        assert_eq!(
            create(&mut p, &mut h, &[ids[0], ids[1]]).unwrap_err(),
            Error::BadGroup
        );
        // Move in: only edge-adjacent layers.
        let m = |layer, group| Command::MoveLayerToGroup { layer, group };
        exec(&mut p, &mut h, m(ids[0], g)).unwrap();
        assert_eq!(members(&p, g), vec![0, 1, 2]);
        exec(&mut p, &mut h, m(ids[3], g)).unwrap();
        assert_eq!(members(&p, g).len(), 4);
        // Out: only edges, never the last one.
        let out = |layer| Command::MoveLayerOutOfGroup { layer };
        assert_eq!(exec(&mut p, &mut h, out(ids[1])), Err(Error::BadGroup));
        exec(&mut p, &mut h, out(ids[0])).unwrap();
        assert_eq!(exec(&mut p, &mut h, m(ids[2], g)), Err(Error::BadGroup));
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p.composition.layers[0].group, Some(g));
        // Unknown group.
        assert_eq!(
            exec(
                &mut p,
                &mut h,
                Command::Ungroup {
                    group: Uuid::new_v4()
                }
            ),
            Err(Error::NoGroup)
        );
        // Removing a layer must not leave an empty group.
        exec(&mut p, &mut h, Command::Ungroup { group: g }).unwrap();
        let g2 = create(&mut p, &mut h, &[ids[3]]).unwrap();
        assert_eq!(
            exec(&mut p, &mut h, Command::RemoveLayer { layer: ids[3] }),
            Err(Error::BadGroup)
        );
        assert!(p.composition.group(g2).is_some());
    }

    #[test]
    fn add_layer_never_splits_a_group() {
        let (mut p, ids) = setup();
        let mut h = History::new();
        let g = create(&mut p, &mut h, &[ids[1], ids[2]]).unwrap();
        exec(
            &mut p,
            &mut h,
            Command::AddLayer {
                layer: layer("N"),
                index: 2,
            },
        )
        .unwrap();
        assert_eq!(members(&p, g), vec![2, 3]);
        assert_eq!(p.composition.layers[1].name, "N");
    }

    #[test]
    fn setters_undo_and_coalesce() {
        let (mut p, ids) = setup();
        let mut h = History::new();
        let g = create(&mut p, &mut h, &[ids[0], ids[1]]).unwrap();
        let before = p.clone();
        let t = Instant::now();
        for v in [0.9, 0.7, 0.5] {
            h.exec(&mut p, Command::SetGroupOpacity { group: g, value: v }, t)
                .unwrap();
        }
        assert_eq!(
            p.composition.groups[0].track.opacity.value,
            vjc_model::Value::Number(0.5)
        );
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        for c in [
            Command::RenameGroup {
                group: g,
                name: "X".into(),
            },
            Command::SetGroupBlend {
                group: g,
                blend: BlendMode::Add,
            },
            Command::SetGroupBypass {
                group: g,
                bypass: true,
            },
            Command::SetGroupSolo {
                group: g,
                solo: true,
            },
            Command::SetGroupSpeed {
                group: g,
                value: 2.0,
            },
        ] {
            exec(&mut p, &mut h, c).unwrap();
        }
        let gr = &p.composition.groups[0];
        assert!(gr.bypass && gr.solo && gr.speed == 2.0 && gr.name == "X");
        assert_eq!(
            exec(
                &mut p,
                &mut h,
                Command::SetGroupSpeed {
                    group: g,
                    value: 9.0
                }
            ),
            Err(Error::InvalidValue)
        );
        assert_eq!(
            exec(
                &mut p,
                &mut h,
                Command::RenameGroup {
                    group: g,
                    name: " ".into()
                }
            ),
            Err(Error::InvalidValue)
        );
        for _ in 0..5 {
            h.undo(&mut p).unwrap().unwrap();
        }
        assert_eq!(p, before);
    }

    #[test]
    fn solo_bypass_and_speed_follow_the_group() {
        let (mut p, ids) = setup();
        let mut h = History::new();
        let g = create(&mut p, &mut h, &[ids[0], ids[1]]).unwrap();
        let off = |p: &Project| -> Vec<bool> {
            let c = &p.composition;
            c.layers.iter().map(|l| c.layer_off(l)).collect()
        };
        assert_eq!(off(&p), [false; 4]);
        p.composition.groups[0].bypass = true;
        assert_eq!(off(&p), [true, true, false, false]);
        p.composition.groups[0].bypass = false;
        p.composition.groups[0].solo = true;
        assert_eq!(off(&p), [false, false, true, true]);
        assert!(!p.composition.group_off(&p.composition.groups[0]));
        // A soloed layer inside the group keeps the group on, others in it go off.
        p.composition.groups[0].solo = false;
        p.composition.layers[1].solo = true;
        assert_eq!(off(&p), [true, false, true, true]);
        assert!(!p.composition.group_off(&p.composition.groups[0]));
        // Solo elsewhere turns the whole group off.
        p.composition.layers[1].solo = false;
        p.composition.layers[3].solo = true;
        assert!(p.composition.group_off(&p.composition.groups[0]));
        p.composition.groups[0].speed = 2.0;
        assert_eq!(p.composition.group_speed(&p.composition.layers[0]), 2.0);
        assert_eq!(p.composition.group_speed(&p.composition.layers[2]), 1.0);
        let _ = g;
    }

    #[test]
    fn addresses_and_effects() {
        let (mut p, ids) = setup();
        let mut h = History::new();
        let g1 = create(&mut p, &mut h, &[ids[0], ids[1]]).unwrap();
        let g2 = create(&mut p, &mut h, &[ids[3]]).unwrap();
        // Numbered from the bottom, like layers: g2 is /1, g1 is /2.
        assert_eq!(
            shortcuts::group_address(&p, g1, "opacity").unwrap(),
            "/comp/group/2/opacity"
        );
        assert_eq!(
            shortcuts::resolve(&p, "/comp/group/1/opacity", 0.25),
            Some(Command::SetGroupOpacity {
                group: g2,
                value: 0.25
            })
        );
        assert_eq!(
            shortcuts::resolve(&p, "/comp/group/2/bypass", 1.0),
            Some(Command::SetGroupBypass {
                group: g1,
                bypass: true
            })
        );
        assert_eq!(
            shortcuts::resolve(&p, "/comp/group/2/solo", 0.0),
            Some(Command::SetGroupSolo {
                group: g1,
                solo: false
            })
        );
        assert_eq!(
            shortcuts::resolve(&p, "/comp/group/2/speed", 0.5),
            Some(Command::SetGroupSpeed {
                group: g1,
                value: 2.0
            })
        );
        assert_eq!(shortcuts::resolve(&p, "/comp/group/3/opacity", 0.5), None);
        assert_eq!(shortcuts::resolve(&p, "/comp/group/2/nope", 0.5), None);
        let ctx = shortcuts::Ctx::new(None, false);
        p.composition.groups[0].speed = 1.0;
        assert_eq!(
            shortcuts::current(&p, &ctx, "/comp/group/2/speed"),
            Some(0.25)
        );
        assert!(shortcuts::is_continuous("/comp/group/2/speed"));
        // Effect chain on the group.
        let fx = vjc_model::Effect {
            id: Uuid::new_v4(),
            kind: "invert".into(),
            bypass: false,
            dry_wet: 1.0,
            params: vec![vjc_model::Param::number("amount", 0.0, Some((0.0, 1.0)))],
        };
        let target = EffectTarget::Group(g1);
        exec(
            &mut p,
            &mut h,
            Command::AddEffect {
                target,
                effect: fx.clone(),
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(effects::chain(&p, target).unwrap().len(), 1);
        assert_eq!(
            shortcuts::effect_address(&p, target, 0, "drywet", None).unwrap(),
            "/comp/group/2/effect/1/drywet"
        );
        assert_eq!(
            shortcuts::resolve(&p, "/comp/group/2/effect/1/drywet", 0.5),
            Some(Command::SetEffectDryWet {
                target,
                effect: fx.id,
                dry_wet: 0.5
            })
        );
        assert!(matches!(
            shortcuts::resolve(&p, "/comp/group/2/effect/1/param/amount", 1.0),
            Some(Command::SetEffectParam { .. })
        ));
        assert_eq!(
            shortcuts::resolve(&p, "/comp/group/1/effect/1/drywet", 0.5),
            None
        );
        h.undo(&mut p).unwrap().unwrap();
        assert!(effects::chain(&p, target).unwrap().is_empty());
    }
}
