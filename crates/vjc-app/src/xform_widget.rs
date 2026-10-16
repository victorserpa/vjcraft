//! Transform widget of the Preview monitor: bounding box, corner scale handles, rotation
//! handle and anchor point. Pure geometry in composition pixels (origin at the frame centre,
//! y down, rotation clockwise), so it is testable without a UI. The app turns the result into
//! `Command::SetTransformMulti`.

use eframe::egui::{Vec2, vec2};
use vjc_model::{Transform, TransformField};

const MIN_SCALE: f64 = 0.01;
const MAX_SCALE: f64 = 10.0;

/// Raw (un-animated) transform values the widget edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xf {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub sw: f64,
    pub sh: f64,
    pub rot: f64,
    pub ax: f64,
    pub ay: f64,
}

impl Xf {
    pub fn of(t: &Transform) -> Self {
        let n = |p: &vjc_model::Param| match p.value {
            vjc_model::Value::Number(n) => n,
            _ => 0.0,
        };
        Xf {
            x: n(&t.x),
            y: n(&t.y),
            scale: n(&t.scale),
            sw: n(&t.scale_w),
            sh: n(&t.scale_h),
            rot: n(&t.rotation),
            ax: n(&t.anchor_x),
            ay: n(&t.anchor_y),
        }
    }
    fn s(&self) -> (f64, f64) {
        (self.scale * self.sw, self.scale * self.sh)
    }
    fn rs(&self, v: (f64, f64)) -> (f64, f64) {
        let (s, c) = self.rot.to_radians().sin_cos();
        let (sx, sy) = self.s();
        let (u, w) = (v.0 * sx, v.1 * sy);
        (c * u - s * w, s * u + c * w)
    }
    fn rs_inv(&self, v: (f64, f64)) -> (f64, f64) {
        let (s, c) = self.rot.to_radians().sin_cos();
        let (sx, sy) = self.s();
        let (u, w) = (c * v.0 + s * v.1, -s * v.0 + c * v.1);
        (u / sx.max(1e-6), w / sy.max(1e-6))
    }
    /// Where picture point `c` (frame centre origin) lands.
    pub fn map(&self, c: (f64, f64)) -> (f64, f64) {
        let d = self.rs((c.0 - self.ax, c.1 - self.ay));
        (self.x + self.ax + d.0, self.y + self.ay + d.1)
    }
    /// Pivot (anchor) in frame coordinates.
    pub fn pivot(&self) -> (f64, f64) {
        (self.x + self.ax, self.y + self.ay)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// 0 TL, 1 TR, 2 BR, 3 BL.
    Corner(usize),
    Rotate,
    Anchor,
    Body,
}

fn v(p: (f64, f64)) -> Vec2 {
    vec2(p.0 as f32, p.1 as f32)
}

/// Corners TL, TR, BR, BL of the picture in frame coordinates.
pub fn corners(xf: &Xf, comp: (u32, u32)) -> [Vec2; 4] {
    let (hw, hh) = (comp.0 as f64 / 2.0, comp.1 as f64 / 2.0);
    [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)].map(|c| v(xf.map(c)))
}

/// Rotation handle: `off` frame px inside the middle of the top edge (it stays reachable
/// when the picture fills the monitor).
pub fn rotate_handle(xf: &Xf, comp: (u32, u32), off: f64) -> Vec2 {
    let top = v(xf.map((0.0, -(comp.1 as f64) / 2.0)));
    let (s, c) = xf.rot.to_radians().sin_cos();
    // Inward = rotated (0, 1).
    top + vec2(-s as f32, c as f32) * off as f32
}

pub fn anchor_point(xf: &Xf) -> Vec2 {
    v(xf.pivot())
}

/// Whether frame point `p` lies inside the box.
pub fn inside(xf: &Xf, comp: (u32, u32), p: Vec2) -> bool {
    let l = xf.rs_inv((p.x as f64 - xf.pivot().0, p.y as f64 - xf.pivot().1));
    let (hw, hh) = (comp.0 as f64 / 2.0, comp.1 as f64 / 2.0);
    let (x, y) = (l.0 + xf.ax, l.1 + xf.ay);
    x.abs() <= hw && y.abs() <= hh
}

/// Handle under `p` (frame coordinates); `r` is the grab radius in frame px.
pub fn hit(xf: &Xf, comp: (u32, u32), p: Vec2, r: f32) -> Option<Handle> {
    if (anchor_point(xf) - p).length() <= r {
        return Some(Handle::Anchor);
    }
    if (rotate_handle(xf, comp, 3.0 * r as f64) - p).length() <= r {
        return Some(Handle::Rotate);
    }
    if let Some(i) = corners(xf, comp)
        .iter()
        .position(|c| (*c - p).length() <= r)
    {
        return Some(Handle::Corner(i));
    }
    inside(xf, comp, p).then_some(Handle::Body)
}

/// Fields changed by dragging `h` from frame point `p0` to `p1`, starting from `start`.
/// `shift` keeps the aspect ratio when scaling.
pub fn drag(
    h: Handle,
    start: &Xf,
    comp: (u32, u32),
    p0: Vec2,
    p1: Vec2,
    shift: bool,
) -> Vec<(TransformField, f64)> {
    let dq = (f64::from(p1.x - p0.x), f64::from(p1.y - p0.y));
    let px = TransformField::X;
    match h {
        Handle::Body => vec![(px, start.x + dq.0), (TransformField::Y, start.y + dq.1)],
        Handle::Anchor => {
            // Move the pivot without moving the picture.
            let d = start.rs_inv(dq);
            let rs_d = start.rs(d);
            vec![
                (TransformField::AnchorX, start.ax + d.0),
                (TransformField::AnchorY, start.ay + d.1),
                (px, start.x - (d.0 - rs_d.0)),
                (TransformField::Y, start.y - (d.1 - rs_d.1)),
            ]
        }
        Handle::Rotate => {
            let pv = start.pivot();
            let ang = |p: Vec2| (f64::from(p.y) - pv.1).atan2(f64::from(p.x) - pv.0);
            let mut d = (ang(p1) - ang(p0)).to_degrees();
            while d > 180.0 {
                d -= 360.0;
            }
            while d <= -180.0 {
                d += 360.0;
            }
            vec![(
                TransformField::Rotation,
                (start.rot + d).clamp(-360.0, 360.0),
            )]
        }
        Handle::Corner(i) => {
            let (hw, hh) = (comp.0 as f64 / 2.0, comp.1 as f64 / 2.0);
            let c = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)][i % 4];
            let cl = (c.0 - start.ax, c.1 - start.ay);
            let pv = start.pivot();
            // Pointer in the picture's rotated frame, relative to the pivot, unscaled units.
            let (s, co) = start.rot.to_radians().sin_cos();
            let rel = (f64::from(p1.x) - pv.0, f64::from(p1.y) - pv.1);
            let loc = (co * rel.0 + s * rel.1, -s * rel.0 + co * rel.1);
            let (sx0, sy0) = start.s();
            let fit = |a: f64| a.clamp(MIN_SCALE, MAX_SCALE);
            if shift || cl.0.abs() < 1.0 || cl.1.abs() < 1.0 {
                let r0 = (cl.0 * sx0).hypot(cl.1 * sy0);
                let f = loc.0.hypot(loc.1) / r0.max(1e-6);
                vec![(TransformField::Scale, fit(start.scale * f))]
            } else {
                vec![
                    (TransformField::ScaleW, fit(loc.0 / cl.0 / start.scale)),
                    (TransformField::ScaleH, fit(loc.1 / cl.1 / start.scale)),
                ]
            }
        }
    }
}

/// Arrow-key nudge: (dx, dy) frame px, 1 or 10 with Shift.
pub fn nudge(xf: &Xf, dx: i32, dy: i32, shift: bool) -> Vec<(TransformField, f64)> {
    let k = if shift { 10.0 } else { 1.0 };
    vec![
        (TransformField::X, xf.x + f64::from(dx) * k),
        (TransformField::Y, xf.y + f64::from(dy) * k),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMP: (u32, u32) = (200, 100);

    fn id() -> Xf {
        Xf::of(&Transform::default())
    }
    fn get(f: &[(TransformField, f64)], k: TransformField) -> f64 {
        f.iter().find(|(a, _)| *a == k).unwrap().1
    }

    #[test]
    fn identity_box_is_the_frame_and_hits() {
        let c = corners(&id(), COMP);
        assert_eq!(c[0], vec2(-100.0, -50.0));
        assert_eq!(c[2], vec2(100.0, 50.0));
        let x = id();
        assert_eq!(
            hit(&x, COMP, vec2(100.0, 50.0), 5.0),
            Some(Handle::Corner(2))
        );
        assert_eq!(hit(&x, COMP, vec2(0.0, 0.0), 5.0), Some(Handle::Anchor));
        assert_eq!(hit(&x, COMP, vec2(30.0, 10.0), 5.0), Some(Handle::Body));
        assert_eq!(hit(&x, COMP, vec2(0.0, -35.0), 5.0), Some(Handle::Rotate));
        assert_eq!(hit(&x, COMP, vec2(500.0, 0.0), 5.0), None);
    }

    #[test]
    fn move_adds_the_delta() {
        let f = drag(
            Handle::Body,
            &id(),
            COMP,
            vec2(0.0, 0.0),
            vec2(12.0, -7.0),
            false,
        );
        assert_eq!(get(&f, TransformField::X), 12.0);
        assert_eq!(get(&f, TransformField::Y), -7.0);
    }

    #[test]
    fn corner_scales_about_the_anchor_and_shift_keeps_aspect() {
        // Drag the BR corner out to double the half-size on both axes.
        let f = drag(
            Handle::Corner(2),
            &id(),
            COMP,
            vec2(100.0, 50.0),
            vec2(200.0, 100.0),
            false,
        );
        assert!((get(&f, TransformField::ScaleW) - 2.0).abs() < 1e-9);
        assert!((get(&f, TransformField::ScaleH) - 2.0).abs() < 1e-9);
        // Only X stretched: free scale changes W only, shift changes uniform Scale.
        let to = vec2(200.0, 50.0);
        let free = drag(Handle::Corner(2), &id(), COMP, vec2(100.0, 50.0), to, false);
        assert!((get(&free, TransformField::ScaleW) - 2.0).abs() < 1e-9);
        assert!((get(&free, TransformField::ScaleH) - 1.0).abs() < 1e-9);
        let kept = drag(Handle::Corner(2), &id(), COMP, vec2(100.0, 50.0), to, true);
        assert_eq!(kept.len(), 1);
        assert!(get(&kept, TransformField::Scale) > 1.0);
    }

    #[test]
    fn rotate_follows_the_pointer_angle() {
        // Pivot at the centre: pointer from straight up to straight right = +90 clockwise.
        let f = drag(
            Handle::Rotate,
            &id(),
            COMP,
            vec2(0.0, -60.0),
            vec2(60.0, 0.0),
            false,
        );
        assert!((get(&f, TransformField::Rotation) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn anchor_drag_keeps_the_picture_in_place() {
        let mut s = id();
        s.rot = 30.0;
        s.scale = 1.5;
        let f = drag(
            Handle::Anchor,
            &s,
            COMP,
            vec2(0.0, 0.0),
            vec2(20.0, 10.0),
            false,
        );
        let mut n = s;
        n.ax = get(&f, TransformField::AnchorX);
        n.ay = get(&f, TransformField::AnchorY);
        n.x = get(&f, TransformField::X);
        n.y = get(&f, TransformField::Y);
        for c in [(-100.0, -50.0), (100.0, 50.0), (30.0, -20.0)] {
            let (a, b) = (s.map(c), n.map(c));
            assert!(
                (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6,
                "{c:?}"
            );
        }
        // The pivot ended up under the pointer.
        let (px, py) = n.pivot();
        assert!((px - 20.0).abs() < 1e-6 && (py - 10.0).abs() < 1e-6);
    }

    #[test]
    fn nudge_is_one_or_ten_px() {
        assert_eq!(get(&nudge(&id(), 1, 0, false), TransformField::X), 1.0);
        assert_eq!(get(&nudge(&id(), 0, -1, true), TransformField::Y), -10.0);
    }
}
