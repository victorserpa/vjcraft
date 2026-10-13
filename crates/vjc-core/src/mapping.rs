//! Pure output-mapping math: slice meshes, resampling, hit-testing and UV transforms.
//! A mesh is a row-major grid of output-pixel points (`cols` x `rows`); a slice input is a
//! normalized quad (TL, TR, BR, BL) or a polygon (its bounding box is used).

use uuid::Uuid;
use vjc_model::{OutputDevice, Screen, Slice};

pub const MIN_GRID: u32 = 2;
pub const MAX_GRID: u32 = 9;

pub type Pt = (f32, f32);

/// GPU vertex: NDC position and composition UV.
pub type Vertex = [f32; 4];

/// Evenly spaced mesh covering the whole `w` x `h` screen.
pub fn identity_mesh(w: u32, h: u32, cols: u32, rows: u32) -> Vec<Pt> {
    let mut m = Vec::with_capacity((cols * rows) as usize);
    for j in 0..rows {
        for i in 0..cols {
            m.push((
                w as f32 * i as f32 / (cols - 1) as f32,
                h as f32 * j as f32 / (rows - 1) as f32,
            ));
        }
    }
    m
}

pub fn full_input() -> Vec<Pt> {
    vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
}

/// A slice showing the whole composition on the whole screen (2x2 mesh).
pub fn new_slice(w: u32, h: u32) -> Slice {
    Slice {
        id: Uuid::new_v4(),
        input: full_input(),
        output_mesh: identity_mesh(w, h, 2, 2),
        mesh_cols: 2,
        mask: None,
        soft_edge: None,
    }
}

pub fn new_screen(name: &str, w: u32, h: u32) -> Screen {
    Screen {
        id: Uuid::new_v4(),
        name: name.into(),
        device: OutputDevice::Window,
        width: w,
        height: h,
        slices: vec![],
    }
}

pub fn rows_of(mesh_len: usize, cols: u32) -> u32 {
    (mesh_len as u32).checked_div(cols).unwrap_or(0)
}

/// True when `mesh` is a valid `cols` x rows grid (2..=9 each, finite points).
pub fn mesh_valid(mesh: &[Pt], cols: u32) -> bool {
    let ok = (MIN_GRID..=MAX_GRID).contains(&cols)
        && (mesh.len() as u32).is_multiple_of(cols)
        && (MIN_GRID..=MAX_GRID).contains(&rows_of(mesh.len(), cols));
    ok && mesh.iter().all(|p| p.0.is_finite() && p.1.is_finite())
}

/// Replaces every invalid slice mesh with a 2x2 grid covering its screen.
pub fn normalize_output(out: &mut vjc_model::OutputSetup) {
    for scr in &mut out.screens {
        let (w, h) = (scr.width, scr.height);
        for sl in &mut scr.slices {
            if !mesh_valid(&sl.output_mesh, sl.mesh_cols) {
                sl.output_mesh = identity_mesh(w, h, 2, 2);
                sl.mesh_cols = 2;
            }
        }
    }
}

/// Piecewise-bilinear point of the mesh at `(u, v)` in 0..1.
pub fn eval_mesh(mesh: &[Pt], cols: u32, rows: u32, u: f32, v: f32) -> Pt {
    let fu = u.clamp(0.0, 1.0) * (cols - 1) as f32;
    let fv = v.clamp(0.0, 1.0) * (rows - 1) as f32;
    let (i, j) = (
        (fu as usize).min(cols as usize - 2),
        (fv as usize).min(rows as usize - 2),
    );
    let (s, t) = (fu - i as f32, fv - j as f32);
    let c = cols as usize;
    quad_lerp(
        [
            mesh[j * c + i],
            mesh[j * c + i + 1],
            mesh[(j + 1) * c + i + 1],
            mesh[(j + 1) * c + i],
        ],
        s,
        t,
    )
}

/// Bilinear interpolation over a quad (TL, TR, BR, BL).
pub fn quad_lerp(q: [Pt; 4], s: f32, t: f32) -> Pt {
    let l = |a: Pt, b: Pt, k: f32| (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k);
    let top = l(q[0], q[1], s);
    let bot = l(q[3], q[2], s);
    l(top, bot, t)
}

/// Resamples to a new grid keeping the surface shape.
pub fn resample_mesh(mesh: &[Pt], cols: u32, ncols: u32, nrows: u32) -> Vec<Pt> {
    let rows = rows_of(mesh.len(), cols);
    let mut out = Vec::with_capacity((ncols * nrows) as usize);
    for j in 0..nrows {
        for i in 0..ncols {
            let (u, v) = (i as f32 / (ncols - 1) as f32, j as f32 / (nrows - 1) as f32);
            out.push(eval_mesh(mesh, cols, rows, u, v));
        }
    }
    out
}

/// Index of the point nearest to `p` within `radius`, if any.
pub fn hit_test(points: &[Pt], p: Pt, radius: f32) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(i, q)| (i, (q.0 - p.0).hypot(q.1 - p.1)))
        .filter(|(_, d)| *d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

pub fn snap(v: f32, grid: f32) -> f32 {
    if grid > 0.0 {
        (v / grid).round() * grid
    } else {
        v
    }
}

/// Input quad of a slice: the 4 corners as given, or the bounding box of a polygon.
pub fn input_quad(input: &[Pt]) -> [Pt; 4] {
    if let [a, b, c, d] = input {
        return [*a, *b, *c, *d];
    }
    let (mut x0, mut y0, mut x1, mut y1) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
    for p in input {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// Composition UV for mesh node `(i, j)`.
pub fn node_uv(input: &[Pt], i: u32, j: u32, cols: u32, rows: u32) -> Pt {
    quad_lerp(
        input_quad(input),
        i as f32 / (cols - 1) as f32,
        j as f32 / (rows - 1) as f32,
    )
}

/// Triangle list (2 per cell) for a slice on a `w` x `h` screen.
pub fn slice_vertices(w: u32, h: u32, s: &Slice) -> Vec<Vertex> {
    let cols = s.mesh_cols;
    if !mesh_valid(&s.output_mesh, cols) {
        return vec![];
    }
    let rows = rows_of(s.output_mesh.len(), cols);
    let v = |i: u32, j: u32| -> Vertex {
        let p = s.output_mesh[(j * cols + i) as usize];
        let uv = node_uv(&s.input, i, j, cols, rows);
        [
            2.0 * p.0 / w as f32 - 1.0,
            1.0 - 2.0 * p.1 / h as f32,
            uv.0,
            uv.1,
        ]
    };
    let mut out = Vec::new();
    for j in 0..rows - 1 {
        for i in 0..cols - 1 {
            let (a, b, c, d) = (v(i, j), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1));
            out.extend([a, b, c, a, c, d]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_shape() {
        // Warped 2x2: bottom-right pulled in.
        let m = vec![(0.0, 0.0), (100.0, 0.0), (0.0, 100.0), (60.0, 100.0)];
        let r = resample_mesh(&m, 2, 3, 3);
        assert_eq!(r.len(), 9);
        assert_eq!(r[0], (0.0, 0.0));
        assert_eq!(r[2], (100.0, 0.0));
        assert_eq!(r[8], (60.0, 100.0));
        assert_eq!(r[4], (40.0, 50.0));
        // Identity stays identity under any size.
        let id = identity_mesh(200, 100, 2, 2);
        let (a, b) = (resample_mesh(&id, 2, 5, 4), identity_mesh(200, 100, 5, 4));
        assert!(
            a.iter()
                .zip(&b)
                .all(|(p, q)| (p.0 - q.0).abs() + (p.1 - q.1).abs() < 1e-3)
        );
        // Round trip through a finer grid keeps the corners.
        let back = resample_mesh(&resample_mesh(&m, 2, 9, 9), 9, 2, 2);
        assert!(
            back.iter()
                .zip(&m)
                .all(|(p, q)| (p.0 - q.0).abs() + (p.1 - q.1).abs() < 1e-3)
        );
    }

    #[test]
    fn hit_test_picks_nearest_in_radius() {
        let pts = [(0.0, 0.0), (10.0, 0.0), (12.0, 0.0)];
        assert_eq!(hit_test(&pts, (11.5, 0.0), 5.0), Some(2));
        assert_eq!(hit_test(&pts, (50.0, 50.0), 5.0), None);
        assert_eq!(snap(13.0, 10.0), 10.0);
        assert_eq!(snap(13.0, 0.0), 13.0);
    }

    #[test]
    fn uv_is_bilinear() {
        let input = [(0.2, 0.2), (0.8, 0.2), (0.8, 0.6), (0.2, 0.6)];
        let c = node_uv(&input, 1, 1, 3, 3);
        assert!((c.0 - 0.5).abs() < 1e-6 && (c.1 - 0.4).abs() < 1e-6);
        assert_eq!(node_uv(&input, 0, 0, 3, 3), (0.2, 0.2));
        assert_eq!(node_uv(&input, 2, 2, 3, 3), (0.8, 0.6));
        // Polygon falls back to its bounding box.
        let tri = [(0.1, 0.1), (0.9, 0.2), (0.5, 0.7)];
        assert_eq!(input_quad(&tri)[0], (0.1, 0.1));
        assert_eq!(input_quad(&tri)[2], (0.9, 0.7));
    }

    #[test]
    fn identity_slice_covers_ndc() {
        let s = new_slice(100, 50);
        let v = slice_vertices(100, 50, &s);
        assert_eq!(v.len(), 6);
        assert_eq!(v[0], [-1.0, 1.0, 0.0, 0.0]);
        assert_eq!(v[2], [1.0, -1.0, 1.0, 1.0]);
        assert!(mesh_valid(&s.output_mesh, 2));
        assert!(!mesh_valid(&s.output_mesh, 3));
    }
}
