//! Advanced slices and masks: polygon slices, freehand (pen) masks, Bezier warp, shape
//! presets and group resize. Pure geometry; the compositor rasterises once per edit.

use crate::mapping::Pt;
use crate::slice_geom::{Rect, bbox};
use vjc_model::ShapePreset;

/// Most points a polygon slice, mask or simplified pen stroke may have.
pub const MAX_POLY_POINTS: usize = 63;

/// Even-odd point-in-polygon test.
pub fn point_in_polygon(poly: &[Pt], p: Pt) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
    }
    inside
}

/// Coverage texture of `poly` (normalized 0..1) at `w` x `h`: 255 inside, 0 outside,
/// sampled at pixel centres, row-major.
pub fn rasterize_coverage(poly: &[Pt], w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize)];
    let n = poly.len();
    if n < 3 || w == 0 || h == 0 {
        return out;
    }
    let px: Vec<Pt> = poly
        .iter()
        .map(|p| (p.0 * w as f32, p.1 * h as f32))
        .collect();
    let mut xs: Vec<f32> = vec![];
    for y in 0..h {
        let cy = y as f32 + 0.5;
        xs.clear();
        for i in 0..n {
            let (a, b) = (px[i], px[(i + 1) % n]);
            if (a.1 > cy) != (b.1 > cy) {
                xs.push((b.0 - a.0) * (cy - a.1) / (b.1 - a.1) + a.0);
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        for pair in xs.as_chunks::<2>().0 {
            // Pixel x is inside when its centre x + 0.5 lies in [x0, x1).
            let from = (pair[0] - 0.5).ceil().max(0.0) as u32;
            let to = ((pair[1] - 0.5).ceil().max(0.0) as u32).min(w);
            for x in from..to {
                out[(y * w + x) as usize] = 255;
            }
        }
    }
    out
}

fn seg_dist(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

fn rdp(pts: &[Pt], tol: f32, keep: &mut [bool], lo: usize, hi: usize) {
    if hi <= lo + 1 {
        return;
    }
    let (mut worst, mut at) = (0.0, lo);
    for i in lo + 1..hi {
        let d = seg_dist(pts[i], pts[lo], pts[hi]);
        if d > worst {
            (worst, at) = (d, i);
        }
    }
    if worst > tol {
        keep[at] = true;
        rdp(pts, tol, keep, lo, at);
        rdp(pts, tol, keep, at, hi);
    }
}

/// Simplifies a freehand stroke (Ramer-Douglas-Peucker) to at most `max` points, growing
/// the tolerance from `tol` until it fits.
pub fn simplify_stroke(stroke: &[Pt], tol: f32, max: usize) -> Vec<Pt> {
    let pts: Vec<Pt> = stroke
        .iter()
        .copied()
        .filter(|p| p.0.is_finite() && p.1.is_finite())
        .collect();
    if pts.len() <= 2 {
        return pts;
    }
    let mut tol = tol.max(1e-6);
    loop {
        let mut keep = vec![false; pts.len()];
        keep[0] = true;
        *keep.last_mut().unwrap() = true;
        rdp(&pts, tol, &mut keep, 0, pts.len() - 1);
        let out: Vec<Pt> = pts
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(p, _)| *p)
            .collect();
        if out.len() <= max.max(2) {
            return out;
        }
        tol *= 1.5;
    }
}

/// `to` moved onto the 15 degree (or `step_deg`) ray nearest to the one from `from`.
pub fn snap_angle(from: Pt, to: Pt, step_deg: f32) -> Pt {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len == 0.0 || step_deg <= 0.0 {
        return to;
    }
    let step = step_deg.to_radians();
    let a = (dy.atan2(dx) / step).round() * step;
    (from.0 + len * a.cos(), from.1 + len * a.sin())
}

/// Polygon of a preset inside `r` (any unit).
pub fn preset_polygon(preset: ShapePreset, r: Rect) -> Vec<Pt> {
    let (cx, cy) = r.center();
    let (rx, ry) = (r.w / 2.0, r.h / 2.0);
    let ring = |n: usize| -> Vec<Pt> {
        (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                (cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect()
    };
    match preset {
        ShapePreset::Rect => vec![
            (r.x, r.y),
            (r.x + r.w, r.y),
            (r.x + r.w, r.y + r.h),
            (r.x, r.y + r.h),
        ],
        ShapePreset::Triangle => vec![(cx, r.y), (r.x + r.w, r.y + r.h), (r.x, r.y + r.h)],
        ShapePreset::Hexagon => ring(6),
        ShapePreset::Circle => ring(32),
    }
}

/// Result of a click while drawing a polygon.
#[derive(Debug, Clone, PartialEq)]
pub enum DrawStep {
    Added,
    Closed(Vec<Pt>),
    Ignored,
}

/// Click-to-add polygon drawing: a click near the first point (with 3+ points) closes it.
#[derive(Debug, Default, Clone)]
pub struct PolyDraw {
    pub points: Vec<Pt>,
}

impl PolyDraw {
    pub fn click(&mut self, p: Pt, close_radius: f32) -> DrawStep {
        if !(p.0.is_finite() && p.1.is_finite()) {
            return DrawStep::Ignored;
        }
        if let Some(&f) = self.points.first()
            && ((p.0 - f.0).powi(2) + (p.1 - f.1).powi(2)).sqrt() <= close_radius
        {
            return if self.points.len() >= 3 {
                DrawStep::Closed(std::mem::take(&mut self.points))
            } else {
                DrawStep::Ignored
            };
        }
        if self.points.len() >= MAX_POLY_POINTS {
            return DrawStep::Ignored;
        }
        self.points.push(p);
        DrawStep::Added
    }
}

fn lerp(a: Pt, b: Pt, t: f32) -> Pt {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// Cubic through `p0`..`p1` whose inner control points sit a third along the segment,
/// moved by the handles `h0` / `h1` (the second is mirrored so the curve stays smooth).
fn cubic(p0: Pt, p1: Pt, h0: Pt, h1: Pt, t: f32) -> Pt {
    let c1 = (
        p0.0 + (p1.0 - p0.0) / 3.0 + h0.0,
        p0.1 + (p1.1 - p0.1) / 3.0 + h0.1,
    );
    let c2 = (
        p1.0 + (p0.0 - p1.0) / 3.0 - h1.0,
        p1.1 + (p0.1 - p1.1) / 3.0 - h1.1,
    );
    let u = 1.0 - t;
    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        w0 * p0.0 + w1 * c1.0 + w2 * c2.0 + w3 * p1.0,
        w0 * p0.1 + w1 * c1.1 + w2 * c2.1 + w3 * p1.1,
    )
}

/// Tessellates a Bezier mesh to a dense grid: `sub` steps per cell. Handles are offsets
/// from the one-third points of each segment (zero handles = the linear mesh). Returns the
/// points and the column count.
pub fn tessellate_bezier(
    mesh: &[Pt],
    cols: u32,
    tangents: &[(Pt, Pt)],
    sub: u32,
) -> (Vec<Pt>, u32) {
    let (c, sub) = (cols as usize, sub.max(1) as usize);
    if c < 2 || mesh.len() < 2 * c || !mesh.len().is_multiple_of(c) {
        return (mesh.to_vec(), cols);
    }
    let r = mesh.len() / c;
    let tg = |i: usize| tangents.get(i).copied().unwrap_or(((0.0, 0.0), (0.0, 0.0)));
    let (dc, dr) = ((c - 1) * sub + 1, (r - 1) * sub + 1);
    // Along the columns for every mesh row, with the row handles carried along.
    let mut rows = vec![vec![(0.0, 0.0); dc]; r];
    let mut dv = vec![vec![(0.0, 0.0); dc]; r];
    for y in 0..r {
        for x in 0..c - 1 {
            let (i0, i1) = (y * c + x, y * c + x + 1);
            for k in 0..=sub {
                let t = k as f32 / sub as f32;
                let j = x * sub + k;
                rows[y][j] = cubic(mesh[i0], mesh[i1], tg(i0).0, tg(i1).0, t);
                dv[y][j] = lerp(tg(i0).1, tg(i1).1, t);
            }
        }
    }
    let mut out = vec![(0.0, 0.0); dc * dr];
    for j in 0..dc {
        for y in 0..r - 1 {
            for k in 0..=sub {
                let t = k as f32 / sub as f32;
                out[(y * sub + k) * dc + j] =
                    cubic(rows[y][j], rows[y + 1][j], dv[y][j], dv[y + 1][j], t);
            }
        }
    }
    (out, dc as u32)
}

/// Scales every point of `meshes` from the group bounding box onto `to`. An axis where the
/// group has no extent is only moved.
pub fn resize_group(meshes: &[Vec<Pt>], to: Rect) -> Vec<Vec<Pt>> {
    let all: Vec<Pt> = meshes.iter().flatten().copied().collect();
    let Some(from) = bbox(&all) else {
        return meshes.to_vec();
    };
    let sx = if from.w > 0.0 { to.w / from.w } else { 1.0 };
    let sy = if from.h > 0.0 { to.h / from.h } else { 1.0 };
    meshes
        .iter()
        .map(|m| {
            m.iter()
                .map(|p| (to.x + (p.0 - from.x) * sx, to.y + (p.1 - from.y) * sy))
                .collect()
        })
        .collect()
}

/// Polygon validated (3..=`MAX_POLY_POINTS` finite points) and clamped to 0..1.
pub fn clamp_poly(pts: &[Pt]) -> Result<Vec<Pt>, crate::Error> {
    let ok = (3..=MAX_POLY_POINTS).contains(&pts.len())
        && pts.iter().all(|p| p.0.is_finite() && p.1.is_finite());
    if !ok {
        return Err(crate::Error::InvalidValue);
    }
    Ok(pts
        .iter()
        .map(|p| (p.0.clamp(0.0, 1.0), p.1.clamp(0.0, 1.0)))
        .collect())
}
