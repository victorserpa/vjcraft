//! Transform math: turns a track's `Transform` params into the inverse map the compositor
//! shader uses (output pixel -> source pixel). Pure, no GPU.
//!
//! With rotation X and Y at 0 the map is affine (position, scale, rotation Z about the
//! anchor). With either set, the picture is rotated in 3D about the anchor (X, Y, Z) and
//! seen through a simple perspective: constant picture size at depth 0, no z-buffer, one
//! picture per transform. The map is then a homography (`Affine::p` is its bottom row).

use crate::anim::{self, AnimCtx};
use vjc_model::Transform;

/// Smallest scale used for the inverse (0 would divide by zero; the picture vanishes anyway).
const MIN_SCALE: f64 = 1e-4;

/// Frame size assumed by [`Affine::inverse_of`] for the perspective distance.
pub const DEFAULT_FRAME: (f64, f64) = (1920.0, 1080.0);

/// Animated values of a `Transform` for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    pub x: f64,
    pub y: f64,
    pub scale: (f64, f64),
    /// Degrees, clockwise on screen.
    pub rotation: f64,
    /// Degrees about the horizontal / vertical axis.
    pub rotation_x: f64,
    pub rotation_y: f64,
    pub anchor: (f64, f64),
    pub anchor_z: f64,
}

impl Resolved {
    pub const IDENTITY: Resolved = Resolved {
        x: 0.0,
        y: 0.0,
        scale: (1.0, 1.0),
        rotation: 0.0,
        rotation_x: 0.0,
        rotation_y: 0.0,
        anchor: (0.0, 0.0),
        anchor_z: 0.0,
    };

    pub fn of(t: &Transform, ctx: &AnimCtx) -> Self {
        let n = |p| anim::effective_number(p, ctx);
        let s = n(&t.scale);
        Resolved {
            x: n(&t.x),
            y: n(&t.y),
            scale: (s * n(&t.scale_w), s * n(&t.scale_h)),
            rotation: n(&t.rotation),
            rotation_x: n(&t.rotation_x),
            rotation_y: n(&t.rotation_y),
            anchor: (n(&t.anchor_x), n(&t.anchor_y)),
            anchor_z: n(&t.anchor_z),
        }
    }
}

type M3 = [[f64; 3]; 3];

fn mul3(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

/// `C = (M * P + t) / (p . (P, 1))`: maps a pixel of the output (centred on the frame, y
/// down) to the pixel of the untransformed picture it shows. Without perspective `p` is
/// `[0, 0, 1]` and this is the plain affine map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    /// Row-major 2x2: [m00, m01, m10, m11].
    pub m: [f64; 4],
    pub t: [f64; 2],
    /// Bottom row of the homography. Positive where the picture faces the camera.
    pub p: [f64; 3],
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        m: [1.0, 0.0, 0.0, 1.0],
        t: [0.0, 0.0],
        p: [0.0, 0.0, 1.0],
    };

    /// Inverse of "scale and rotate about the anchor, then move by (x, y)", with the
    /// perspective distance of a 1920x1080 frame (irrelevant when X/Y rotation is 0).
    pub fn inverse_of(r: &Resolved) -> Self {
        Self::inverse_of_sized(r, DEFAULT_FRAME)
    }

    /// As [`Affine::inverse_of`], for a frame of `size` px: the camera sits at twice the
    /// longer side, so the picture keeps its size at depth 0.
    pub fn inverse_of_sized(r: &Resolved, size: (f64, f64)) -> Self {
        let (sx, sy) = (r.scale.0.max(MIN_SCALE), r.scale.1.max(MIN_SCALE));
        let (s, c) = r.rotation.to_radians().sin_cos();
        if r.rotation_x == 0.0 && r.rotation_y == 0.0 {
            // S^-1 * R^-1 with R = [c -s; s c] (clockwise on a y-down screen).
            let m = [c / sx, s / sx, -s / sy, c / sy];
            let (a, b) = r.anchor;
            let (px, py) = (r.x + a, r.y + b);
            return Affine {
                m,
                t: [a - (m[0] * px + m[1] * py), b - (m[2] * px + m[3] * py)],
                p: [0.0, 0.0, 1.0],
            };
        }
        let d = 2.0 * size.0.max(size.1).max(1.0);
        let (sa, ca) = r.rotation_x.to_radians().sin_cos();
        let (sb, cb) = r.rotation_y.to_radians().sin_cos();
        let rz: M3 = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
        let ry: M3 = [[cb, 0.0, sb], [0.0, 1.0, 0.0], [-sb, 0.0, cb]];
        let rx: M3 = [[1.0, 0.0, 0.0], [0.0, ca, -sa], [0.0, sa, ca]];
        let rot = mul3(&rx, &mul3(&ry, &rz));
        // B = S^-1 * R^T: picture-plane offset from a world offset about the anchor.
        let b: M3 = std::array::from_fn(|i| {
            let k = if i == 2 {
                1.0
            } else if i == 0 {
                sx
            } else {
                sy
            };
            std::array::from_fn(|j| rot[j][i] / k)
        });
        let a3 = b[2];
        // Pivot in world space: anchor plus position.
        let cc = [r.anchor.0 + r.x, r.anchor.1 + r.y, r.anchor_z];
        let dot = |u: &[f64; 3], v: &[f64; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
        let ac = dot(&a3, &cc) - r.anchor_z;
        // Linear forms over (screen x, screen y, 1).
        let den = [-a3[0] / d, -a3[1] / d, a3[2]];
        let num = [-a3[0], -a3[1], ac];
        let k = a3[2] - ac / d;
        let dd: M3 = [
            [k - cc[0] * den[0], -cc[0] * den[1], -cc[0] * den[2]],
            [-cc[1] * den[0], k - cc[1] * den[1], -cc[1] * den[2]],
            std::array::from_fn(|j| num[j] - cc[2] * den[j]),
        ];
        let anchor = [r.anchor.0, r.anchor.1];
        let row = |i: usize| -> [f64; 3] {
            std::array::from_fn(|j| {
                (0..3).map(|q| b[i][q] * dd[q][j]).sum::<f64>() + anchor[i] * den[j]
            })
        };
        // Visible where den and k agree in sign: make that "den > 0".
        let sg = if k < 0.0 { -1.0 } else { 1.0 };
        let (n0, n1) = (row(0), row(1));
        Affine {
            m: [sg * n0[0], sg * n0[1], sg * n1[0], sg * n1[1]],
            t: [sg * n0[2], sg * n1[2]],
            p: den.map(|v| sg * v),
        }
    }

    /// True when there is a perspective part (X or Y rotation).
    pub fn has_perspective(&self) -> bool {
        self.p != [0.0, 0.0, 1.0]
    }

    pub fn is_identity(&self) -> bool {
        self.m
            .iter()
            .zip(Self::IDENTITY.m)
            .all(|(a, b)| (a - b).abs() < 1e-9)
            && self.t.iter().all(|v| v.abs() < 1e-9)
            && !self.has_perspective()
    }

    pub fn apply(&self, p: (f64, f64)) -> (f64, f64) {
        let w = self.p[0] * p.0 + self.p[1] * p.1 + self.p[2];
        (
            (self.m[0] * p.0 + self.m[1] * p.1 + self.t[0]) / w,
            (self.m[2] * p.0 + self.m[3] * p.1 + self.t[1]) / w,
        )
    }

    fn h(&self) -> M3 {
        [
            [self.m[0], self.m[1], self.t[0]],
            [self.m[2], self.m[3], self.t[1]],
            self.p,
        ]
    }

    /// Map for an inner transform (`self`, e.g. a layer's) seen through `inner` (a clip's,
    /// applied to the picture first): output -> `self` -> `inner`.
    pub fn through(&self, inner: &Affine) -> Affine {
        if !self.has_perspective() && !inner.has_perspective() {
            let (a, b) = (inner.m, self.m);
            let t = inner.apply((self.t[0], self.t[1]));
            return Affine {
                m: [
                    a[0] * b[0] + a[1] * b[2],
                    a[0] * b[1] + a[1] * b[3],
                    a[2] * b[0] + a[3] * b[2],
                    a[2] * b[1] + a[3] * b[3],
                ],
                t: [t.0, t.1],
                p: [0.0, 0.0, 1.0],
            };
        }
        let h = mul3(&inner.h(), &self.h());
        Affine {
            m: [h[0][0], h[0][1], h[1][0], h[1][1]],
            t: [h[0][2], h[1][2]],
            p: h[2],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn default_transform_is_identity() {
        let r = Resolved::of(&Transform::default(), &AnimCtx::default());
        assert_eq!(r, Resolved::IDENTITY);
        assert!(Affine::inverse_of(&r).is_identity());
    }

    #[test]
    fn position_moves_the_picture() {
        let r = Resolved {
            x: 100.0,
            y: -40.0,
            ..Resolved::IDENTITY
        };
        // The picture centre now sits at (100, -40) of the output.
        assert!(near(
            Affine::inverse_of(&r).apply((100.0, -40.0)),
            (0.0, 0.0)
        ));
    }

    #[test]
    fn scale_about_anchor_keeps_the_anchor_fixed() {
        let r = Resolved {
            scale: (2.0, 2.0),
            anchor: (50.0, 20.0),
            ..Resolved::IDENTITY
        };
        let a = Affine::inverse_of(&r);
        assert!(near(a.apply((50.0, 20.0)), (50.0, 20.0)));
        // A point 10 px right of the anchor shows source 5 px right of it.
        assert!(near(a.apply((60.0, 20.0)), (55.0, 20.0)));
    }

    #[test]
    fn rotation_is_clockwise_on_screen() {
        let r = Resolved {
            rotation: 90.0,
            ..Resolved::IDENTITY
        };
        // Source point to the right of centre ends up below it (y down).
        assert!(near(Affine::inverse_of(&r).apply((0.0, 10.0)), (10.0, 0.0)));
    }

    #[test]
    fn layer_over_clip_composes_in_order() {
        let clip = Affine::inverse_of(&Resolved {
            x: 10.0,
            ..Resolved::IDENTITY
        });
        let layer = Affine::inverse_of(&Resolved {
            scale: (2.0, 2.0),
            ..Resolved::IDENTITY
        });
        // Clip shifts +10, layer doubles: the source centre lands at x = 20.
        assert!(near(layer.through(&clip).apply((20.0, 0.0)), (0.0, 0.0)));
    }

    #[test]
    fn xy_rotation_zero_keeps_the_affine_map() {
        let r = Resolved {
            x: 5.0,
            rotation: 30.0,
            anchor_z: 77.0, // the pivot depth alone changes nothing
            ..Resolved::IDENTITY
        };
        let a = Affine::inverse_of_sized(&r, (800.0, 600.0));
        assert!(!a.has_perspective());
        assert_eq!(a, Affine::inverse_of(&r));
    }

    /// Projects a picture point forward (rotate about the anchor, move, perspective) and
    /// checks that the inverse map sends the screen point back to it.
    #[test]
    fn perspective_inverse_undoes_the_forward_projection() {
        let size = (400.0, 300.0);
        let d = 2.0 * 400.0;
        let r = Resolved {
            x: 12.0,
            y: -7.0,
            scale: (1.5, 0.8),
            rotation: 20.0,
            rotation_x: 35.0,
            rotation_y: -50.0,
            anchor: (30.0, -10.0),
            anchor_z: 40.0,
        };
        let inv = Affine::inverse_of_sized(&r, size);
        assert!(inv.has_perspective());
        let (sa, ca) = 35f64.to_radians().sin_cos();
        let (sb, cb) = (-50f64).to_radians().sin_cos();
        let (sz, cz) = 20f64.to_radians().sin_cos();
        for (u, v) in [(0.0, 0.0), (60.0, 40.0), (-80.0, 25.0), (10.0, -90.0)] {
            // P - A, scaled.
            let p = [(u - 30.0) * 1.5, (v + 10.0) * 0.8, -40.0];
            let q = [cz * p[0] - sz * p[1], sz * p[0] + cz * p[1], p[2]];
            let q = [cb * q[0] + sb * q[2], q[1], -sb * q[0] + cb * q[2]];
            let q = [q[0], ca * q[1] - sa * q[2], sa * q[1] + ca * q[2]];
            let w = [q[0] + 30.0 + 12.0, q[1] - 10.0 - 7.0, q[2] + 40.0];
            let f = d / (d - w[2]);
            let got = inv.apply((w[0] * f, w[1] * f));
            assert!(near(got, (u, v)), "({u},{v}) -> {got:?}");
        }
    }

    #[test]
    fn y_rotation_of_90_collapses_the_picture() {
        let r = Resolved {
            rotation_y: 90.0,
            ..Resolved::IDENTITY
        };
        let a = Affine::inverse_of_sized(&r, (400.0, 300.0));
        // Edge on: every screen point off the centre line maps far outside the picture.
        let (u, _) = a.apply((50.0, 0.0));
        assert!(u.abs() > 200.0, "{u}");
        // Rotated 45 degrees the centre stays put.
        let r45 = Resolved {
            rotation_y: 45.0,
            ..Resolved::IDENTITY
        };
        let a = Affine::inverse_of_sized(&r45, (400.0, 300.0));
        assert!(near(a.apply((0.0, 0.0)), (0.0, 0.0)));
        assert!(a.p[2] > 0.0);
    }

    #[test]
    fn perspective_composes_through_a_layer() {
        let layer = Affine::inverse_of_sized(
            &Resolved {
                rotation_y: 30.0,
                ..Resolved::IDENTITY
            },
            (400.0, 300.0),
        );
        let clip = Affine::inverse_of(&Resolved {
            x: 10.0,
            ..Resolved::IDENTITY
        });
        let both = layer.through(&clip);
        let direct = clip.apply(layer.apply((33.0, 21.0)));
        assert!(near(both.apply((33.0, 21.0)), direct));
        // Identity on the outside keeps the inner map.
        let same = Affine::IDENTITY.through(&layer);
        assert!(near(same.apply((5.0, 9.0)), layer.apply((5.0, 9.0))));
    }
}
