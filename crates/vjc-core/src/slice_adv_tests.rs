use super::slice_adv::*;
use crate::mapping::{self, Pt};
use crate::slice_geom::Rect;
use crate::{Command, History, apply};
use std::time::Instant;
use uuid::Uuid;
use vjc_model::{Mask, MeshMode, Project, ShapePreset, SliceShape};

fn setup() -> (Project, Uuid, Vec<Uuid>) {
    let mut p = Project::new("t");
    let mut scr = mapping::new_screen("s", 200, 100);
    let mut a = mapping::new_slice(200, 100);
    let mut b = mapping::new_slice(200, 100);
    a.output_mesh = vec![(0.0, 0.0), (100.0, 0.0), (0.0, 50.0), (100.0, 50.0)];
    b.output_mesh = vec![(100.0, 50.0), (200.0, 50.0), (100.0, 100.0), (200.0, 100.0)];
    let ids = vec![a.id, b.id];
    scr.slices = vec![a, b];
    let sid = scr.id;
    apply(
        &mut p,
        Command::AddScreen {
            screen: scr,
            index: 0,
        },
    )
    .unwrap();
    (p, sid, ids)
}

fn hexagon() -> Vec<Pt> {
    preset_polygon(
        ShapePreset::Hexagon,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        },
    )
}

/// Distance from p to the segment ab.
fn seg_dist(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

#[test]
fn old_slice_and_screen_json_load_with_defaults() {
    let old = r#"{"id":"00000000-0000-0000-0000-000000000001","name":"A",
        "device":"Window","width":4,"height":2,
        "slices":[{"id":"00000000-0000-0000-0000-000000000002","input":[],
        "output_mesh":[],"mesh_cols":2,"mask":null,"soft_edge":null}]}"#;
    let s: vjc_model::Screen = serde_json::from_str(old).unwrap();
    assert_eq!(s.slices[0].shape, SliceShape::Rect);
    assert_eq!(s.slices[0].mesh_mode, MeshMode::Linear);
    assert!(s.slices[0].mesh_tangents.is_empty());
    assert_eq!(s.output_mask, None);
}

#[test]
fn every_new_variant_roundtrips() {
    let (mut p, sid, ids) = setup();
    let scr = &mut p.output.screens[0];
    scr.output_mask = Some(Mask::Path(vec![(0.0, 0.0), (1.0, 0.0), (0.5, 1.0)]));
    scr.slices[0].shape = SliceShape::Polygon(hexagon());
    scr.slices[0].mesh_mode = MeshMode::Bezier;
    scr.slices[0].mesh_tangents = vec![((1.0, 2.0), (3.0, 4.0)); 4];
    scr.slices[1].shape = SliceShape::Rect;
    let json = serde_json::to_string(&p).unwrap();
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
    let _ = (sid, ids);
    let m = Mask::Polygon(vec![(0.0, 0.0), (1.0, 0.0), (0.5, 1.0)]);
    let back: Mask = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back, m);
}

#[test]
fn point_in_polygon_basics() {
    let sq = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    assert!(point_in_polygon(&sq, (0.5, 0.5)));
    assert!(!point_in_polygon(&sq, (1.5, 0.5)));
    let l = [
        (0.0, 0.0),
        (2.0, 0.0),
        (2.0, 1.0),
        (1.0, 1.0),
        (1.0, 2.0),
        (0.0, 2.0),
    ];
    assert!(point_in_polygon(&l, (0.5, 1.5)));
    assert!(
        !point_in_polygon(&l, (1.5, 1.5)),
        "concave notch is outside"
    );
}

#[test]
fn coverage_matches_reference_raster_within_one_pixel() {
    let (w, h) = (64u32, 48u32);
    let poly = hexagon();
    let cov = rasterize_coverage(&poly, w, h);
    assert_eq!(cov.len(), (w * h) as usize);
    let px: Vec<Pt> = poly
        .iter()
        .map(|p| (p.0 * w as f32, p.1 * h as f32))
        .collect();
    let (mut inside, mut checked) = (0, 0);
    for y in 0..h {
        for x in 0..w {
            let c = (x as f32 + 0.5, y as f32 + 0.5);
            let d = (0..px.len())
                .map(|i| seg_dist(c, px[i], px[(i + 1) % px.len()]))
                .fold(f32::MAX, f32::min);
            let got = cov[(y * w + x) as usize] > 127;
            inside += got as u32;
            if d > 1.0 {
                checked += 1;
                // Convex reference: inside iff on the same side of every edge.
                let side = |i: usize| {
                    let (a, b) = (px[i], px[(i + 1) % px.len()]);
                    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
                };
                let s0 = side(0) > 0.0;
                let want = (0..px.len()).all(|i| (side(i) > 0.0) == s0);
                assert_eq!(got, want, "pixel {x},{y}");
            }
        }
    }
    assert!(inside > 500 && checked > 2000);
}

#[test]
fn bezier_with_zero_tangents_equals_linear_mesh() {
    let mesh = vec![
        (0.0, 0.0),
        (90.0, 10.0),
        (200.0, 0.0),
        (0.0, 60.0),
        (100.0, 70.0),
        (200.0, 55.0),
    ];
    let sub = 4;
    let (dense, cols) = tessellate_bezier(&mesh, 3, &[], sub);
    assert_eq!(cols, 2 * sub + 1);
    assert_eq!(dense.len(), (cols * (sub + 1)) as usize);
    let bil = |u: f32, v: f32| {
        // u in 0..2, v in 0..1 over the 3x2 grid.
        let c = (u.floor() as usize).min(1);
        let fu = u - c as f32;
        let l = |r: usize| {
            let (a, b) = (mesh[r * 3 + c], mesh[r * 3 + c + 1]);
            (a.0 + (b.0 - a.0) * fu, a.1 + (b.1 - a.1) * fu)
        };
        let (t, b) = (l(0), l(1));
        (t.0 + (b.0 - t.0) * v, t.1 + (b.1 - t.1) * v)
    };
    for r in 0..=sub {
        for c in 0..cols {
            let want = bil(c as f32 / sub as f32, r as f32 / sub as f32);
            let got = dense[(r * cols + c) as usize];
            assert!(
                (got.0 - want.0).abs() < 1e-3 && (got.1 - want.1).abs() < 1e-3,
                "{r},{c}: {got:?} vs {want:?}"
            );
        }
    }
    // Explicit zero handles are the same as none.
    let z = vec![((0.0, 0.0), (0.0, 0.0)); 6];
    assert_eq!(tessellate_bezier(&mesh, 3, &z, sub).0, dense);
}

#[test]
fn bezier_handle_bends_the_curve() {
    let mesh = vec![(0.0, 0.0), (100.0, 0.0), (0.0, 100.0), (100.0, 100.0)];
    let mut t = vec![((0.0, 0.0), (0.0, 0.0)); 4];
    t[0].0 = (0.0, 30.0);
    let (dense, cols) = tessellate_bezier(&mesh, 2, &t, 4);
    let mid = dense[2];
    assert_eq!(cols, 5);
    assert!(mid.1 > 1.0, "top edge bows down: {mid:?}");
    assert_eq!(dense[0], (0.0, 0.0));
    assert_eq!(dense[4], (100.0, 0.0));
}

#[test]
fn polygon_drawn_with_four_clicks_closes_on_first_point() {
    let mut d = PolyDraw::default();
    assert_eq!(d.click((0.1, 0.1), 0.03), DrawStep::Added);
    assert_eq!(d.click((0.9, 0.1), 0.03), DrawStep::Added);
    assert_eq!(
        d.click((0.1, 0.1), 0.03),
        DrawStep::Ignored,
        "two points cannot close"
    );
    assert_eq!(d.click((0.9, 0.9), 0.03), DrawStep::Added);
    assert_eq!(d.click((0.1, 0.9), 0.03), DrawStep::Added);
    match d.click((0.11, 0.11), 0.03) {
        DrawStep::Closed(p) => assert_eq!(p.len(), 4),
        s => panic!("{s:?}"),
    }
    assert!(d.points.is_empty());
}

#[test]
fn pen_stroke_simplifies_below_64_points() {
    let stroke: Vec<Pt> = (0..2000)
        .map(|i| {
            let a = i as f32 / 2000.0 * std::f32::consts::TAU;
            (0.5 + 0.4 * a.cos(), 0.5 + 0.4 * a.sin())
        })
        .collect();
    let s = simplify_stroke(&stroke, 0.0005, 63);
    assert!(s.len() >= 3 && s.len() < 64, "{}", s.len());
    assert!(clamp_poly(&s).is_ok());
    // A straight line collapses to its ends.
    let line: Vec<Pt> = (0..100).map(|i| (i as f32 / 99.0, 0.5)).collect();
    assert_eq!(simplify_stroke(&line, 0.001, 63).len(), 2);
}

#[test]
fn shift_snaps_to_15_degrees() {
    let p = snap_angle((0.0, 0.0), (10.0, 1.0), 15.0);
    assert!(
        (p.1).abs() < 1e-3 && (p.0 - (101.0f32).sqrt()).abs() < 1e-3,
        "{p:?}"
    );
    let q = snap_angle((5.0, 5.0), (5.0 + 10.0, 5.0 + 9.0), 15.0);
    let ang = (q.1 - 5.0).atan2(q.0 - 5.0).to_degrees();
    assert!((ang - 45.0).abs() < 1e-2, "{ang}");
}

#[test]
fn presets_have_expected_vertex_counts_and_stay_in_rect() {
    let r = Rect {
        x: 0.2,
        y: 0.1,
        w: 0.5,
        h: 0.4,
    };
    for (pr, n) in [
        (ShapePreset::Rect, 4),
        (ShapePreset::Triangle, 3),
        (ShapePreset::Hexagon, 6),
    ] {
        assert_eq!(preset_polygon(pr, r).len(), n, "{pr:?}");
    }
    let c = preset_polygon(ShapePreset::Circle, r);
    assert!(c.len() >= 16 && clamp_poly(&c).is_ok());
    for pr in ShapePreset::ALL {
        for p in preset_polygon(pr, r) {
            assert!(
                p.0 >= 0.2 - 1e-4 && p.0 <= 0.7 + 1e-4 && p.1 >= 0.1 - 1e-4 && p.1 <= 0.5 + 1e-4
            );
        }
    }
}

#[test]
fn group_resize_scales_every_slice() {
    let (mut p, sid, ids) = setup();
    let before = p.clone();
    let mut h = History::new();
    // Group bbox is 0,0,200,100; double it.
    h.exec(
        &mut p,
        Command::ResizeSlices {
            screen: sid,
            slices: ids.clone(),
            to: (0.0, 0.0, 400.0, 200.0),
        },
        Instant::now(),
    )
    .unwrap();
    let s = &p.output.screens[0].slices;
    assert_eq!(s[0].output_mesh[3], (200.0, 100.0));
    assert_eq!(s[1].output_mesh[0], (200.0, 100.0));
    assert_eq!(s[1].output_mesh[3], (400.0, 200.0));
    h.undo(&mut p).unwrap().unwrap();
    assert_eq!(p, before, "one command, one undo");
    // Only the selected slices move: resize the second alone onto a new box.
    let r = resize_group(
        &[before.output.screens[0].slices[1].output_mesh.clone()],
        Rect {
            x: 0.0,
            y: 0.0,
            w: 50.0,
            h: 50.0,
        },
    );
    assert_eq!(r[0][0], (0.0, 0.0));
    assert_eq!(r[0][3], (50.0, 50.0));
}

#[test]
fn shape_mode_and_mask_commands_are_one_undo_step_and_validate() {
    let (mut p, sid, ids) = setup();
    let before = p.clone();
    let mut h = History::new();
    let now = Instant::now();
    let bad = Command::SetSliceShape {
        screen: sid,
        slice: ids[0],
        shape: SliceShape::Polygon(vec![(0.0, 0.0), (1.0, f32::NAN), (0.0, 1.0)]),
    };
    assert!(h.exec(&mut p, bad, now).is_err());
    let few = Command::SetScreenMask {
        screen: sid,
        mask: Some(Mask::Path(vec![(0.0, 0.0), (1.0, 1.0)])),
    };
    assert!(h.exec(&mut p, few, now).is_err());
    assert_eq!(p, before);
    for c in [
        Command::SetSliceShape {
            screen: sid,
            slice: ids[0],
            shape: SliceShape::Polygon(hexagon()),
        },
        Command::SetSliceMeshMode {
            screen: sid,
            slice: ids[0],
            mode: MeshMode::Bezier,
            tangents: vec![((1.0, 0.0), (0.0, 1.0)); 4],
        },
        Command::SetScreenMask {
            screen: sid,
            mask: Some(Mask::Path(vec![(0.0, 0.0), (1.0, 0.0), (0.5, 1.0)])),
        },
    ] {
        let before = p.clone();
        h.exec(&mut p, c, now).unwrap();
        assert_ne!(p, before);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap().unwrap();
    }
    assert_eq!(
        p.output.screens[0].slices[0].shape,
        SliceShape::Polygon(hexagon())
    );
    // Wrong tangent count is normalized, not rejected (see stale_tangents tests).
    let wrong = Command::SetSliceMeshMode {
        screen: sid,
        slice: ids[0],
        mode: MeshMode::Bezier,
        tangents: vec![((0.0, 0.0), (0.0, 0.0)); 3],
    };
    h.exec(&mut p, wrong, now).unwrap();
    assert_eq!(p.output.screens[0].slices[0].mesh_tangents.len(), 4);
}

#[test]
fn preset_slices_carry_their_outline_and_drawn_polygons_validate() {
    use crate::slice_adv::{polygon_slice, preset_slice};
    let (w, h) = (1920, 1080);
    let rect = preset_slice(w, h, ShapePreset::Rect);
    assert_eq!(rect.shape, SliceShape::Rect);
    for (preset, n) in [
        (ShapePreset::Triangle, 3),
        (ShapePreset::Hexagon, 6),
        (ShapePreset::Circle, 32),
    ] {
        let s = preset_slice(w, h, preset);
        let SliceShape::Polygon(p) = &s.shape else {
            panic!("{preset:?}");
        };
        assert_eq!(p.len(), n);
        assert!(
            p.iter()
                .all(|q| (0.0..=1.0).contains(&q.0) && (0.0..=1.0).contains(&q.1))
        );
        assert!(mapping::mesh_valid(&s.output_mesh, s.mesh_cols));
    }
    let tri = [(0.1, 0.1), (0.9, 0.1), (0.5, 0.9)];
    assert!(polygon_slice(w, h, &tri).is_some());
    assert!(polygon_slice(w, h, &tri[..2]).is_none());
    let many: Vec<Pt> = (0..64).map(|i| (i as f32 / 64.0, 0.5)).collect();
    assert!(polygon_slice(w, h, &many).is_none(), "over the 63 cap");
}

fn hex_setup() -> (Project, Uuid, Uuid) {
    let (mut p, sid, _) = setup();
    let mut sl = preset_slice(200, 100, ShapePreset::Hexagon);
    let id = sl.id;
    sl.mesh_tangents = vec![];
    p.output.screens[0].slices.push(sl);
    (p, sid, id)
}

fn outline(p: &Project, id: Uuid) -> Vec<Pt> {
    match &p.output.screens[0]
        .slices
        .iter()
        .find(|s| s.id == id)
        .unwrap()
        .shape
    {
        SliceShape::Polygon(v) => v.clone(),
        _ => panic!("not a polygon"),
    }
}

fn close(a: &[Pt], b: &[Pt]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| (x.0 - y.0).abs() < 1e-4 && (x.1 - y.1).abs() < 1e-4)
}

#[test]
fn moving_a_polygon_slice_moves_its_outline_in_one_undo_step() {
    let (mut p, sid, id) = hex_setup();
    let before = p.clone();
    let orig = outline(&p, id);
    let mesh: Vec<Pt> = p.output.screens[0].slices[2]
        .output_mesh
        .iter()
        .map(|q| (q.0 + 20.0, q.1 + 10.0))
        .collect();
    let mut h = History::new();
    h.exec(
        &mut p,
        Command::SetSliceMesh {
            screen: sid,
            slice: id,
            mesh,
            cols: 2,
        },
        Instant::now(),
    )
    .unwrap();
    // 20 px of 200 = 0.1, 10 px of 100 = 0.1.
    let want: Vec<Pt> = orig.iter().map(|q| (q.0 + 0.1, q.1 + 0.1)).collect();
    assert!(close(&outline(&p, id), &want));
    h.undo(&mut p).unwrap().unwrap();
    assert!(close(&outline(&p, id), &orig));
    // Group resize and SetSliceMeshes carry the outline too.
    h.exec(
        &mut p,
        Command::ResizeSlices {
            screen: sid,
            slices: vec![id],
            to: (0.0, 0.0, 400.0, 200.0),
        },
        Instant::now(),
    )
    .unwrap();
    let want: Vec<Pt> = orig.iter().map(|q| (q.0 * 2.0, q.1 * 2.0)).collect();
    assert!(close(&outline(&p, id), &want));
    h.undo(&mut p).unwrap().unwrap();
    assert!(close(&outline(&p, id), &orig));
    assert_eq!(
        p.output.screens[0].slices[2].mesh_cols,
        before.output.screens[0].slices[2].mesh_cols
    );
}

#[test]
fn point_edit_leaves_the_outline_alone() {
    let (mut p, sid, id) = hex_setup();
    let orig = outline(&p, id);
    let mut mesh = p.output.screens[0].slices[2].output_mesh.clone();
    mesh[3] = (mesh[3].0 + 5.0, mesh[3].1);
    apply(
        &mut p,
        Command::SetSliceMesh {
            screen: sid,
            slice: id,
            mesh,
            cols: 2,
        },
    )
    .unwrap();
    assert!(close(&outline(&p, id), &orig));
}

#[test]
fn mesh_mode_normalizes_stale_tangent_length() {
    let (mut p, sid, ids) = setup();
    let mut h = History::new();
    let now = Instant::now();
    for n in [3usize, 9] {
        let c = Command::SetSliceMeshMode {
            screen: sid,
            slice: ids[0],
            mode: MeshMode::Bezier,
            tangents: vec![((1.0, 1.0), (2.0, 2.0)); n],
        };
        h.exec(&mut p, c, now).unwrap();
        assert_eq!(p.output.screens[0].slices[0].mesh_tangents.len(), 4);
    }
    // Toggle back and undo never fail even when stored tangents are stale.
    p.output.screens[0].slices[0].mesh_tangents = vec![((1.0, 0.0), (0.0, 0.0)); 7];
    let c = Command::SetSliceMeshMode {
        screen: sid,
        slice: ids[0],
        mode: MeshMode::Linear,
        tangents: vec![],
    };
    h.exec(&mut p, c, now).unwrap();
    h.undo(&mut p).unwrap().unwrap();
}

#[test]
fn grid_size_change_resets_tangents_and_undo_restores_them() {
    let (mut p, sid, ids) = setup();
    let mut h = History::new();
    let now = Instant::now();
    let c = Command::SetSliceMeshMode {
        screen: sid,
        slice: ids[0],
        mode: MeshMode::Bezier,
        tangents: vec![((3.0, 0.0), (0.0, 3.0)); 4],
    };
    h.exec(&mut p, c, now).unwrap();
    let before = p.clone();
    let c = Command::SetSliceMeshSize {
        screen: sid,
        slice: ids[0],
        cols: 3,
        rows: 3,
    };
    h.exec(&mut p, c, now).unwrap();
    assert!(p.output.screens[0].slices[0].mesh_tangents.is_empty());
    h.undo(&mut p).unwrap().unwrap();
    assert_eq!(p, before);
}

#[test]
fn duplicate_slice_ids_are_rejected() {
    let (mut p, sid, ids) = setup();
    let before = p.clone();
    let m = p.output.screens[0].slices[0].output_mesh.clone();
    let c = Command::SetSliceMeshes {
        screen: sid,
        meshes: vec![(ids[0], m.clone()), (ids[0], m)],
    };
    assert!(apply(&mut p, c).is_err());
    let c = Command::ResizeSlices {
        screen: sid,
        slices: vec![ids[0], ids[0]],
        to: (0.0, 0.0, 50.0, 50.0),
    };
    assert!(apply(&mut p, c).is_err());
    assert_eq!(p, before);
}

#[test]
fn handles_sit_on_the_outline_and_resizing_from_it_resizes_the_outline() {
    use crate::slice_geom::{Side, fit_from, handle_rect};
    let (mut p, sid, id) = hex_setup();
    let sl = p.output.screens[0].slices[2].clone();
    let r = handle_rect(&sl, Side::Output, (1920, 1080), (200, 100)).unwrap();
    // The hexagon spans 0.1..0.9 of a 200 x 100 screen, not the whole mesh.
    assert!(
        (r.x - 20.0).abs() < 0.5 && (r.w - 160.0).abs() < 0.5,
        "{r:?}"
    );
    let to = Rect {
        x: 0.0,
        y: 0.0,
        w: 80.0,
        h: 40.0,
    };
    let mesh = fit_from(&sl.output_mesh, r, to);
    apply(
        &mut p,
        Command::SetSliceMesh {
            screen: sid,
            slice: id,
            mesh,
            cols: 2,
        },
    )
    .unwrap();
    let sl = p.output.screens[0].slices[2].clone();
    let r2 = handle_rect(&sl, Side::Output, (1920, 1080), (200, 100)).unwrap();
    assert!(
        (r2.w - 80.0).abs() < 0.1 && (r2.h - 40.0).abs() < 0.1,
        "{r2:?}"
    );
    assert!(r2.x.abs() < 0.1 && r2.y.abs() < 0.1, "{r2:?}");
}
