//! Transform math: turns a track's `Transform` params into the inverse affine map the
//! compositor shader uses (output pixel -> source pixel). Pure, no GPU.

use crate::anim::{self, AnimCtx};
use vjc_model::Transform;

/// Smallest scale used for the inverse (0 would divide by zero; the picture vanishes anyway).
const MIN_SCALE: f64 = 1e-4;

/// Animated values of a `Transform` for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    pub x: f64,
    pub y: f64,
    pub scale: (f64, f64),
    /// Degrees, clockwise on screen.
    pub rotation: f64,
    pub anchor: (f64, f64),
}

impl Resolved {
    pub const IDENTITY: Resolved = Resolved {
        x: 0.0,
        y: 0.0,
        scale: (1.0, 1.0),
        rotation: 0.0,
        anchor: (0.0, 0.0),
    };

    pub fn of(t: &Transform, ctx: &AnimCtx) -> Self {
        let n = |p| anim::effective_number(p, ctx);
        let s = n(&t.scale);
        Resolved {
            x: n(&t.x),
            y: n(&t.y),
            scale: (s * n(&t.scale_w), s * n(&t.scale_h)),
            rotation: n(&t.rotation),
            anchor: (n(&t.anchor_x), n(&t.anchor_y)),
        }
    }
}

/// `C = M * P + t`: maps a pixel of the output (centred on the frame, y down) to the pixel
/// of the untransformed picture it shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    /// Row-major 2x2: [m00, m01, m10, m11].
    pub m: [f64; 4],
    pub t: [f64; 2],
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        m: [1.0, 0.0, 0.0, 1.0],
        t: [0.0, 0.0],
    };

    /// Inverse of "scale and rotate about the anchor, then move by (x, y)".
    pub fn inverse_of(r: &Resolved) -> Self {
        let (sx, sy) = (r.scale.0.max(MIN_SCALE), r.scale.1.max(MIN_SCALE));
        let (s, c) = r.rotation.to_radians().sin_cos();
        // S^-1 * R^-1 with R = [c -s; s c] (clockwise on a y-down screen).
        let m = [c / sx, s / sx, -s / sy, c / sy];
        let (a, b) = r.anchor;
        let (px, py) = (r.x + a, r.y + b);
        Affine {
            m,
            t: [a - (m[0] * px + m[1] * py), b - (m[2] * px + m[3] * py)],
        }
    }

    pub fn is_identity(&self) -> bool {
        self.m
            .iter()
            .zip(Self::IDENTITY.m)
            .all(|(a, b)| (a - b).abs() < 1e-9)
            && self.t.iter().all(|v| v.abs() < 1e-9)
    }

    pub fn apply(&self, p: (f64, f64)) -> (f64, f64) {
        (
            self.m[0] * p.0 + self.m[1] * p.1 + self.t[0],
            self.m[2] * p.0 + self.m[3] * p.1 + self.t[1],
        )
    }

    /// Map for an inner transform (`self`, e.g. a layer's) seen through `inner` (a clip's,
    /// applied to the picture first): output -> `self` -> `inner`.
    pub fn through(&self, inner: &Affine) -> Affine {
        let (a, b) = (inner.m, self.m);
        let t = inner.apply((self.t[0], self.t[1]));
        Affine {
            m: [
                a[0] * b[0] + a[1] * b[2],
                a[0] * b[1] + a[1] * b[3],
                a[2] * b[0] + a[3] * b[2],
                a[2] * b[1] + a[3] * b[3],
            ],
            t: [t.0, t.1],
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
}
