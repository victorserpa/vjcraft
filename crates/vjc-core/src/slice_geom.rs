//! Pure geometry for precise slice editing: bounding rectangles in pixels, moving and
//! resizing, rotation, flips, snapping and the "simple math" of numeric fields.
//! Points are `(x, y)` in whatever unit the caller works in (screen pixels for the
//! output mesh; composition pixels for the input after scaling by the composition size).

use crate::Command;
use crate::mapping::{self, Pt};
use vjc_model::{Screen, Slice};

/// Axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn center(&self) -> Pt {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// Smallest size a rectangle may be resized to.
pub const MIN_SIZE: f32 = 1.0;

/// Bounding rectangle of `pts`; `None` when empty or not finite.
pub fn bbox(pts: &[Pt]) -> Option<Rect> {
    if pts.is_empty() || pts.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    Some(Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// Maps the bounding box of `pts` onto `to` (translate + scale, keeps the shape). A
/// degenerate source axis is spread evenly; sizes are clamped to [`MIN_SIZE`].
pub fn fit_to_rect(pts: &[Pt], to: Rect) -> Vec<Pt> {
    let Some(from) = bbox(pts) else {
        return pts.to_vec();
    };
    let (w, h) = (to.w.max(MIN_SIZE), to.h.max(MIN_SIZE));
    pts.iter()
        .map(|p| {
            let u = if from.w > 1e-6 {
                (p.0 - from.x) / from.w
            } else {
                0.0
            };
            let v = if from.h > 1e-6 {
                (p.1 - from.y) / from.h
            } else {
                0.0
            };
            (to.x + u * w, to.y + v * h)
        })
        .collect()
}

pub fn translate(pts: &[Pt], dx: f32, dy: f32) -> Vec<Pt> {
    pts.iter().map(|p| (p.0 + dx, p.1 + dy)).collect()
}

/// Rotates around `c` by `deg` (clockwise on a y-down screen).
pub fn rotate_about(pts: &[Pt], c: Pt, deg: f32) -> Vec<Pt> {
    let (s, k) = deg.to_radians().sin_cos();
    pts.iter()
        .map(|p| {
            let (x, y) = (p.0 - c.0, p.1 - c.1);
            (c.0 + x * k - y * s, c.1 + x * s + y * k)
        })
        .collect()
}

/// Rotates around the bounding-box centre by a multiple of 90 degrees; the result is
/// snapped so quarter turns stay exact.
pub fn rotate_quarter(pts: &[Pt], quarters: i32) -> Vec<Pt> {
    let Some(b) = bbox(pts) else {
        return pts.to_vec();
    };
    let c = b.center();
    let q = quarters.rem_euclid(4);
    pts.iter()
        .map(|p| {
            let (x, y) = (p.0 - c.0, p.1 - c.1);
            let (x, y) = match q {
                1 => (-y, x),
                2 => (-x, -y),
                3 => (y, -x),
                _ => (x, y),
            };
            (c.0 + x, c.1 + y)
        })
        .collect()
}

/// Mirrors left-right around the bounding-box centre.
pub fn flip_h(pts: &[Pt]) -> Vec<Pt> {
    let Some(b) = bbox(pts) else {
        return pts.to_vec();
    };
    pts.iter()
        .map(|p| (2.0 * b.center().0 - p.0, p.1))
        .collect()
}

/// Mirrors top-bottom around the bounding-box centre.
pub fn flip_v(pts: &[Pt]) -> Vec<Pt> {
    let Some(b) = bbox(pts) else {
        return pts.to_vec();
    };
    pts.iter()
        .map(|p| (p.0, 2.0 * b.center().1 - p.1))
        .collect()
}

/// Rotation in degrees of the first edge `a -> b` (0 = pointing right), -180..=180.
pub fn edge_angle(a: Pt, b: Pt) -> f32 {
    (b.1 - a.1).atan2(b.0 - a.0).to_degrees()
}

/// Shortest signed difference `to - from` in degrees, -180..=180.
pub fn angle_delta(from: f32, to: f32) -> f32 {
    let d = (to - from).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

/// Moves `p` onto the nearest guide line within `thr` on each axis independently.
pub fn snap_to_guides(p: Pt, xs: &[f32], ys: &[f32], thr: f32) -> Pt {
    let near = |v: f32, g: &[f32]| {
        g.iter()
            .copied()
            .filter(|l| (l - v).abs() <= thr)
            .min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()))
            .unwrap_or(v)
    };
    (near(p.0, xs), near(p.1, ys))
}

/// Guide lines (edges and centres) of the screen and of `others`, as `(xs, ys)`.
pub fn guides(screen: (f32, f32), others: &[Rect]) -> (Vec<f32>, Vec<f32>) {
    let mut xs = vec![0.0, screen.0 / 2.0, screen.0];
    let mut ys = vec![0.0, screen.1 / 2.0, screen.1];
    for r in others {
        xs.extend([r.x, r.x + r.w / 2.0, r.x + r.w]);
        ys.extend([r.y, r.y + r.h / 2.0, r.y + r.h]);
    }
    (xs, ys)
}

/// "Match output shape": the normalized input rectangle (TL, TR, BR, BL) that samples
/// `out` (an output rectangle in pixels) from a `comp` sized composition one pixel to one
/// pixel. Same size and position as the output rectangle, shifted to stay inside the
/// composition when it fits (a larger rectangle is anchored at the top-left).
pub fn match_output_shape(out: Rect, comp: (u32, u32)) -> Vec<Pt> {
    let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
    let (w, h) = (out.w.max(MIN_SIZE), out.h.max(MIN_SIZE));
    let x = if w >= cw {
        0.0
    } else {
        out.x.clamp(0.0, cw - w)
    };
    let y = if h >= ch {
        0.0
    } else {
        out.y.clamp(0.0, ch - h)
    };
    let (x0, y0, x1, y1) = (x / cw, y / ch, (x + w) / cw, (y + h) / ch);
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// Evaluates a numeric field. A leading operator (`+ - * /`) applies to `current`
/// (`/3`, `*2`, `+10`, `-10`); otherwise the text is an expression of numbers with
/// `+ - * /` (`1920/3`, `100+20*2`). A leading `=` forces an absolute value (`=-5`).
/// `None` when the text is not valid or the result is not finite.
pub fn eval_field(text: &str, current: f32) -> Option<f32> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let t = t.replace(',', ".");
    let (expr, base) = match t.chars().next()? {
        '=' => (t[1..].to_string(), None),
        '+' | '-' | '*' | '/' => (t.clone(), Some(current)),
        _ => (t.clone(), None),
    };
    let v = match base {
        Some(c) => eval_expr(&format!("{c}{expr}"))?,
        None => eval_expr(&expr)?,
    };
    v.is_finite().then_some(v)
}

/// `number (op number)*` with `*` and `/` binding tighter than `+` and `-`. A number may
/// carry a leading sign and an exponent (as printed by `f32`'s `Display`).
fn eval_expr(s: &str) -> Option<f32> {
    let b = s.as_bytes();
    let mut i = 0;
    let num = |i: &mut usize| -> Option<f64> {
        let start = *i;
        if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
            *i += 1;
        }
        while *i < b.len() && (b[*i].is_ascii_digit() || b[*i] == b'.') {
            *i += 1;
        }
        if *i < b.len() && (b[*i] == b'e' || b[*i] == b'E') {
            let mut j = *i + 1;
            if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                *i = j;
            }
        }
        s[start..*i].parse::<f64>().ok()
    };
    let mut sum = 0.0f64;
    let mut term = num(&mut i)?;
    while i < b.len() {
        let op = b[i];
        i += 1;
        let rhs = num(&mut i)?;
        match op {
            b'*' => term *= rhs,
            b'/' => {
                if rhs == 0.0 {
                    return None;
                }
                term /= rhs;
            }
            b'+' | b'-' => {
                sum += term;
                term = if op == b'+' { rhs } else { -rhs };
            }
            _ => return None,
        }
    }
    Some((sum + term) as f32)
}

/// Which side of a slice an edit touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Input selection on the composition (normalized; edited in composition pixels).
    Input,
    /// Output warp mesh on the screen (pixels).
    Output,
}

/// Numeric inspector fields of a side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    X,
    Y,
    W,
    H,
    /// Rotation in degrees of the top edge.
    Rot,
}

/// One precise edit of a slice side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Set(Field, f32),
    Nudge(f32, f32),
    /// Quarter turns clockwise (1 = 90, 2 = 180, 3 = 270).
    Rotate(i32),
    FlipH,
    FlipV,
    /// Copy the output rectangle to the input (1:1 pixel map). `Side::Input` only.
    MatchOutput,
}

/// The side's points in pixels and the two points that give its rotation.
fn side_points(sl: &Slice, side: Side, comp: (u32, u32)) -> Vec<Pt> {
    let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
    match side {
        Side::Input => sl.input.iter().map(|p| (p.0 * cw, p.1 * ch)).collect(),
        Side::Output => sl.output_mesh.clone(),
    }
}

fn angle_ref(sl: &Slice, side: Side, comp: (u32, u32)) -> (Pt, Pt) {
    let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
    match side {
        Side::Input => {
            let q = mapping::input_quad(&sl.input);
            ((q[0].0 * cw, q[0].1 * ch), (q[1].0 * cw, q[1].1 * ch))
        }
        Side::Output => {
            let m = &sl.output_mesh;
            match (m.first(), m.get(sl.mesh_cols.max(1) as usize - 1)) {
                (Some(a), Some(b)) => (*a, *b),
                _ => ((0.0, 0.0), (1.0, 0.0)),
            }
        }
    }
}

/// Current values of the inspector fields `[x, y, w, h, rotation]` of a side.
pub fn readout(sl: &Slice, side: Side, comp: (u32, u32)) -> Option<[f32; 5]> {
    let b = bbox(&side_points(sl, side, comp))?;
    let (a, c) = angle_ref(sl, side, comp);
    Some([b.x, b.y, b.w, b.h, edge_angle(a, c)])
}

/// The command that applies `op` to `side` of the slice, or `None` when it changes nothing
/// or does not apply. Every precise edit goes through here, so it is one undoable command.
pub fn edit(scr: &Screen, sl: &Slice, side: Side, comp: (u32, u32), op: Op) -> Option<Command> {
    let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
    let pts = side_points(sl, side, comp);
    let b = bbox(&pts)?;
    let out = match op {
        Op::Set(f, v) => {
            let v = if f == Field::Rot { v } else { v.max(0.0) };
            match f {
                Field::X => translate(&pts, v - b.x, 0.0),
                Field::Y => translate(&pts, 0.0, v - b.y),
                Field::W => fit_to_rect(&pts, Rect { w: v, ..b }),
                Field::H => fit_to_rect(&pts, Rect { h: v, ..b }),
                Field::Rot => {
                    let (a, c) = angle_ref(sl, side, comp);
                    rotate_about(&pts, b.center(), angle_delta(edge_angle(a, c), v))
                }
            }
        }
        Op::Nudge(dx, dy) => translate(&pts, dx, dy),
        Op::Rotate(q) => rotate_quarter(&pts, q),
        Op::FlipH => flip_h(&pts),
        Op::FlipV => flip_v(&pts),
        Op::MatchOutput => {
            if side != Side::Input {
                return None;
            }
            let ob = bbox(&sl.output_mesh)?;
            return Some(Command::SetSliceInput {
                screen: scr.id,
                slice: sl.id,
                input: match_output_shape(ob, comp),
            });
        }
    };
    if !out.iter().all(|p| p.0.is_finite() && p.1.is_finite()) {
        return None;
    }
    let unchanged = out
        .iter()
        .zip(&pts)
        .all(|(a, b)| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4);
    if unchanged {
        return None;
    }
    Some(match side {
        Side::Input => Command::SetSliceInput {
            screen: scr.id,
            slice: sl.id,
            input: out.iter().map(|p| (p.0 / cw, p.1 / ch)).collect(),
        },
        Side::Output => Command::SetSliceMesh {
            screen: scr.id,
            slice: sl.id,
            mesh: out,
            cols: sl.mesh_cols,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[Pt], b: &[Pt]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(p, q)| (p.0 - q.0).abs() < 1e-3 && (p.1 - q.1).abs() < 1e-3)
    }

    const QUAD: [Pt; 4] = [(10.0, 20.0), (110.0, 20.0), (110.0, 70.0), (10.0, 70.0)];

    #[test]
    fn math_relative_and_absolute() {
        assert_eq!(eval_field("/3", 90.0), Some(30.0));
        assert_eq!(eval_field("*2", 21.0), Some(42.0));
        assert_eq!(eval_field("+10", 5.0), Some(15.0));
        assert_eq!(eval_field("-10", 5.0), Some(-5.0));
        assert_eq!(eval_field("1920/3", 0.0), Some(640.0));
        assert_eq!(eval_field("100+20*2", 0.0), Some(140.0));
        assert_eq!(eval_field(" 12,5 ", 0.0), Some(12.5));
        assert_eq!(eval_field("=-5", 9.0), Some(-5.0));
        assert_eq!(eval_field("/0", 9.0), None);
        assert_eq!(eval_field("abc", 9.0), None);
        assert_eq!(eval_field("", 9.0), None);
        assert_eq!(eval_field("3*", 9.0), None);
        // Relative to a value printed with an exponent still parses.
        assert_eq!(eval_field("*2", 1.0e-7), Some(2.0e-7));
    }

    #[test]
    fn fit_moves_and_resizes() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 100.0,
        };
        let m = fit_to_rect(&QUAD, r);
        assert!(close(
            &m,
            &[(0.0, 0.0), (200.0, 0.0), (200.0, 100.0), (0.0, 100.0)]
        ));
        assert_eq!(bbox(&m), Some(r));
        // Width never collapses below one pixel.
        let thin = fit_to_rect(&QUAD, Rect { w: 0.0, ..r });
        assert_eq!(bbox(&thin).unwrap().w, MIN_SIZE);
    }

    #[test]
    fn quarter_turns_are_exact_and_swap_size() {
        let r = rotate_quarter(&QUAD, 1);
        let b = bbox(&r).unwrap();
        assert_eq!((b.w, b.h), (50.0, 100.0));
        assert_eq!(b.center(), bbox(&QUAD).unwrap().center());
        assert!(close(&rotate_quarter(&QUAD, 4), &QUAD));
        assert!(close(
            &rotate_quarter(&QUAD, 2),
            &rotate_quarter(&rotate_quarter(&QUAD, 1), 1)
        ));
        assert!(close(&rotate_quarter(&QUAD, 3), &rotate_quarter(&QUAD, -1)));
    }

    #[test]
    fn flips_mirror_inside_the_same_box() {
        let f = flip_h(&QUAD);
        assert_eq!(bbox(&f), bbox(&QUAD));
        assert_eq!(f[0], QUAD[1]);
        assert!(close(&flip_h(&f), &QUAD));
        let v = flip_v(&QUAD);
        assert_eq!(v[0], QUAD[3]);
        assert!(close(&flip_v(&v), &QUAD));
    }

    #[test]
    fn free_rotation_and_angle_readout() {
        let c = bbox(&QUAD).unwrap().center();
        let r = rotate_about(&QUAD, c, 90.0);
        assert!(close(&r, &rotate_quarter(&QUAD, 1)));
        let a = edge_angle(r[0], r[1]);
        assert!((a - 90.0).abs() < 1e-3);
        assert!((angle_delta(170.0, -170.0) - 20.0).abs() < 1e-4);
        assert!((angle_delta(-170.0, 170.0) + 20.0).abs() < 1e-4);
    }

    #[test]
    fn match_output_shape_is_one_to_one() {
        let out = Rect {
            x: 100.0,
            y: 50.0,
            w: 480.0,
            h: 270.0,
        };
        let q = match_output_shape(out, (1920, 1080));
        assert_eq!(q.len(), 4);
        let b = bbox(&q).unwrap();
        assert!((b.w * 1920.0 - 480.0).abs() < 1e-2 && (b.h * 1080.0 - 270.0).abs() < 1e-2);
        assert!((b.x * 1920.0 - 100.0).abs() < 1e-2);
        // Pushed back inside the composition, size kept.
        let edge = match_output_shape(Rect { x: 1800.0, ..out }, (1920, 1080));
        let e = bbox(&edge).unwrap();
        assert!((e.w * 1920.0 - 480.0).abs() < 1e-2 && (e.x + e.w - 1.0).abs() < 1e-5);
    }

    fn rig() -> (Project, Uuid, Uuid) {
        let mut p = Project::new("t");
        let mut scr = mapping::new_screen("S", 1920, 1080);
        let mut sl = mapping::new_slice(1920, 1080);
        sl.output_mesh = vec![
            (100.0, 200.0),
            (580.0, 200.0),
            (100.0, 470.0),
            (580.0, 470.0),
        ];
        sl.input = vec![(0.0, 0.0), (0.5, 0.0), (0.5, 0.5), (0.0, 0.5)];
        let ids = (scr.id, sl.id);
        scr.slices.push(sl);
        p.output.screens.push(scr);
        (p, ids.0, ids.1)
    }

    fn run(p: &mut Project, h: &mut History, side: Side, op: Op) -> bool {
        let (scr, sl) = (&p.output.screens[0], &p.output.screens[0].slices[0]);
        let comp = (p.composition.width, p.composition.height);
        match edit(scr, sl, side, comp, op) {
            Some(c) => {
                h.exec(p, c, Instant::now()).unwrap();
                true
            }
            None => false,
        }
    }

    use crate::History;
    use std::time::Instant;
    use uuid::Uuid;
    use vjc_model::Project;

    #[test]
    fn output_fields_in_pixels_with_undo() {
        let (mut p, _, _) = rig();
        let before = p.clone();
        let mut h = History::new();
        assert!(run(&mut p, &mut h, Side::Output, Op::Set(Field::X, 300.0)));
        assert!(run(&mut p, &mut h, Side::Output, Op::Set(Field::W, 240.0)));
        assert!(run(&mut p, &mut h, Side::Output, Op::Nudge(1.0, -10.0)));
        let sc = &p.output.screens[0];
        let r = readout(&sc.slices[0], Side::Output, (1920, 1080)).unwrap();
        assert_eq!([r[0], r[1], r[2], r[3]], [301.0, 190.0, 240.0, 270.0]);
        // Setting the value it already has is not a command.
        assert!(!run(&mut p, &mut h, Side::Output, Op::Set(Field::Y, 190.0)));
        while h.undo(&mut p).is_some() {}
        assert_eq!(p, before);
    }

    #[test]
    fn input_fields_use_composition_pixels() {
        let (mut p, _, _) = rig();
        p.composition.width = 1000;
        p.composition.height = 500;
        let mut h = History::new();
        // 0.5 x 0.5 of 1000x500 is 500 x 250 px.
        let sc = &p.output.screens[0];
        let r = readout(&sc.slices[0], Side::Input, (1000, 500)).unwrap();
        assert_eq!([r[2], r[3]], [500.0, 250.0]);
        assert!(run(&mut p, &mut h, Side::Input, Op::Set(Field::W, 100.0)));
        assert!(run(&mut p, &mut h, Side::Input, Op::Set(Field::X, 250.0)));
        let sl = &p.output.screens[0].slices[0];
        assert!((sl.input[1].0 - 0.35).abs() < 1e-5 && (sl.input[0].0 - 0.25).abs() < 1e-5);
    }

    #[test]
    fn rotation_field_and_quarter_buttons() {
        let (mut p, _, _) = rig();
        let mut h = History::new();
        assert!(run(&mut p, &mut h, Side::Output, Op::Set(Field::Rot, 90.0)));
        let sc = &p.output.screens[0];
        let r = readout(&sc.slices[0], Side::Output, (1920, 1080)).unwrap();
        assert!((r[4] - 90.0).abs() < 1e-3);
        // 90 degrees swaps the box: 480 x 270 becomes 270 x 480.
        assert!((r[2] - 270.0).abs() < 1e-2 && (r[3] - 480.0).abs() < 1e-2);
        assert!(run(&mut p, &mut h, Side::Output, Op::Rotate(1)));
        let sc = &p.output.screens[0];
        let r = readout(&sc.slices[0], Side::Output, (1920, 1080)).unwrap();
        assert!((r[4].abs() - 180.0).abs() < 1e-2, "{}", r[4]);
        assert!(run(&mut p, &mut h, Side::Output, Op::FlipH));
    }

    #[test]
    fn match_output_shape_makes_the_slice_unscaled() {
        let (mut p, _, _) = rig();
        let mut h = History::new();
        assert!(!mapping::slice_unscaled(
            (1920, 1080),
            &p.output.screens[0].slices[0]
        ));
        assert!(run(&mut p, &mut h, Side::Input, Op::MatchOutput));
        let comp = (p.composition.width, p.composition.height);
        let sl = &p.output.screens[0].slices[0];
        assert!(mapping::slice_unscaled(comp, sl));
        // Not offered on the output side.
        let sc = &p.output.screens[0];
        assert!(edit(sc, &sc.slices[0], Side::Output, comp, Op::MatchOutput).is_none());
        h.undo(&mut p).unwrap().unwrap();
        assert!(!mapping::slice_unscaled(
            comp,
            &p.output.screens[0].slices[0]
        ));
    }

    #[test]
    fn snapping_picks_nearest_guide_per_axis() {
        let others = [Rect {
            x: 100.0,
            y: 0.0,
            w: 50.0,
            h: 50.0,
        }];
        let (xs, ys) = guides((1920.0, 1080.0), &others);
        // Near the other slice's right edge (150) and the screen top (0).
        assert_eq!(snap_to_guides((147.0, 4.0), &xs, &ys, 6.0), (150.0, 0.0));
        // Nothing within the threshold: unchanged.
        assert_eq!(
            snap_to_guides((600.0, 300.0), &xs, &ys, 6.0),
            (600.0, 300.0)
        );
        // Screen centre.
        assert_eq!(
            snap_to_guides((958.0, 538.0), &xs, &ys, 6.0),
            (960.0, 540.0)
        );
    }
}
