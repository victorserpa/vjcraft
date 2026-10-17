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
        name: String::new(),
        input: full_input(),
        output_mesh: identity_mesh(w, h, 2, 2),
        mesh_cols: 2,
        mask: None,
        soft_edge: None,
        source: Default::default(),
        color: Default::default(),
        mask_invert: false,
        is_key: false,
        black_bg: false,
        shape: Default::default(),
        mesh_mode: Default::default(),
        mesh_tangents: vec![],
    }
}

/// Distinct layer / group sources routed by the slices of `screens`: the only ones the
/// compositor has to render on their own.
pub fn routed_sources<'a>(
    screens: impl IntoIterator<Item = &'a Screen>,
) -> Vec<vjc_model::SliceSource> {
    use vjc_model::SliceSource;
    let mut out: Vec<SliceSource> = vec![];
    for sl in screens.into_iter().flat_map(|s| &s.slices) {
        if matches!(sl.source, SliceSource::Layer(_) | SliceSource::Group(_))
            && !out.contains(&sl.source)
        {
            out.push(sl.source);
        }
    }
    out
}

pub fn new_screen(name: &str, w: u32, h: u32) -> Screen {
    Screen {
        id: Uuid::new_v4(),
        name: name.into(),
        device: OutputDevice::Window,
        width: w,
        height: h,
        slices: vec![],
        pixel_perfect: false,
        enabled: true,
        color: Default::default(),
        delay_ms: 0,
        additive_overlap: false,
        output_mask: None,
    }
}

/// True when the slice maps source pixels to screen pixels one to one: axis-aligned input
/// rectangle and flat output grid whose size in pixels equals the input region of a
/// `src` (w, h) composition (within a quarter pixel). Such a slice may use nearest sampling.
pub fn slice_unscaled(src: (u32, u32), s: &Slice) -> bool {
    let q = input_quad(&s.input);
    let flat = |a: Pt, b: Pt, c: Pt, d: Pt| {
        (a.1 - b.1).abs() < 1e-4
            && (c.1 - d.1).abs() < 1e-4
            && (a.0 - d.0).abs() < 1e-4
            && (b.0 - c.0).abs() < 1e-4
    };
    if !flat(q[0], q[1], q[2], q[3]) || s.output_mesh.len() < 4 || s.mesh_cols < 2 {
        return false;
    }
    let (iw, ih) = (
        (q[1].0 - q[0].0) * src.0 as f32,
        (q[3].1 - q[0].1) * src.1 as f32,
    );
    let rows = rows_of(s.output_mesh.len(), s.mesh_cols);
    if rows < 2 || !mesh_valid(&s.output_mesh, s.mesh_cols) {
        return false;
    }
    let cols = s.mesh_cols as usize;
    let m = &s.output_mesh;
    let (tl, tr, bl) = (m[0], m[cols - 1], m[(rows as usize - 1) * cols]);
    let br = m[rows as usize * cols - 1];
    if (tl.1 - tr.1).abs() > 1e-3 || (bl.1 - br.1).abs() > 1e-3 {
        return false;
    }
    if (tl.0 - bl.0).abs() > 1e-3 || (tr.0 - br.0).abs() > 1e-3 {
        return false;
    }
    if ((tr.0 - tl.0) - iw).abs() >= 0.25 || ((bl.1 - tl.1) - ih).abs() >= 0.25 {
        return false;
    }
    // Interior nodes must sit on the flat grid too (no warp).
    (0..rows as usize).all(|j| {
        (0..cols).all(|i| {
            let e = quad_lerp(
                [tl, tr, br, bl],
                i as f32 / (cols - 1) as f32,
                j as f32 / (rows as usize - 1) as f32,
            );
            let n = m[j * cols + i];
            (n.0 - e.0).abs() < 0.25 && (n.1 - e.1).abs() < 0.25
        })
    })
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

/// True when `screens` can replace the output setup: unique screen and slice ids, sizes
/// 1..=16384, valid meshes, masks and inputs.
pub fn screens_valid(screens: &[Screen]) -> bool {
    let mut ids = std::collections::HashSet::new();
    screens.iter().all(|s| {
        (1..=16384).contains(&s.width)
            && (1..=16384).contains(&s.height)
            && ids.insert(s.id)
            && s.slices.iter().all(|sl| {
                ids.insert(sl.id)
                    && mesh_valid(&sl.output_mesh, sl.mesh_cols)
                    && sl.input.len() >= 3
                    && sl.input.iter().all(|p| p.0.is_finite() && p.1.is_finite())
                    && sl.mask.as_ref().is_none_or(|m| mask_valid(m))
                    && sl
                        .soft_edge
                        .as_ref()
                        .is_none_or(|e| clamp_soft_edge(e).is_some())
            })
    })
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
    slice_vertices_st(w, h, s)
        .iter()
        .map(|v| [v[0], v[1], v[2], v[3]])
        .collect()
}

/// Like `slice_vertices` plus the slice-surface coordinate `(s, t)` in 0..1 (for soft edge).
pub fn slice_vertices_st(w: u32, h: u32, s: &Slice) -> Vec<[f32; 6]> {
    let cols = s.mesh_cols;
    if !mesh_valid(&s.output_mesh, cols) {
        return vec![];
    }
    let rows = rows_of(s.output_mesh.len(), cols);
    let v = |i: u32, j: u32| -> [f32; 6] {
        let p = s.output_mesh[(j * cols + i) as usize];
        let uv = node_uv(&s.input, i, j, cols, rows);
        [
            2.0 * p.0 / w as f32 - 1.0,
            1.0 - 2.0 * p.1 / h as f32,
            uv.0,
            uv.1,
            i as f32 / (cols - 1) as f32,
            j as f32 / (rows - 1) as f32,
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

/// Most points a slice mask may have (the shader holds them in a fixed uniform array).
pub const MAX_MASK_POINTS: usize = 32;
pub const MAX_SOFT_WIDTH: f32 = 0.5;
pub const GAMMA_RANGE: (f32, f32) = (0.1, 5.0);

/// Shape presets for a slice mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskShape {
    Rectangle,
    Ellipse,
    Triangle,
    Diamond,
}

impl MaskShape {
    pub const ALL: [MaskShape; 4] = [
        MaskShape::Rectangle,
        MaskShape::Ellipse,
        MaskShape::Triangle,
        MaskShape::Diamond,
    ];
}

/// Points of a preset shape inscribed in `bbox` = (x0, y0, x1, y1), normalized screen
/// coordinates (clamped to 0..=1). Always a valid mask (`mask_valid`).
pub fn mask_preset(shape: MaskShape, bbox: (f32, f32, f32, f32)) -> Vec<Pt> {
    let (x0, y0) = (bbox.0.clamp(0.0, 1.0), bbox.1.clamp(0.0, 1.0));
    let (x1, y1) = (bbox.2.clamp(x0, 1.0), bbox.3.clamp(y0, 1.0));
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    match shape {
        MaskShape::Rectangle => vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
        MaskShape::Ellipse => (0..24)
            .map(|i| {
                let a = i as f32 / 24.0 * std::f32::consts::TAU;
                (cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect(),
        MaskShape::Triangle => vec![(cx, y0), (x1, y1), (x0, y1)],
        MaskShape::Diamond => vec![(cx, y0), (x1, cy), (cx, y1), (x0, cy)],
    }
}

/// Delay in whole frames for `delay_ms` at `fps` (0 ms or an unusable fps = 0).
pub fn delay_frames(delay_ms: u32, fps: f32) -> usize {
    if delay_ms == 0 || !fps.is_finite() || fps <= 0.0 {
        return 0;
    }
    (delay_ms.min(MAX_DELAY_MS) as f32 * fps / 1000.0).round() as usize
}

pub const MAX_DELAY_MS: u32 = 100;

/// Mask polygon in normalized screen coordinates: 3..=32 finite points.
pub fn mask_valid(m: &[Pt]) -> bool {
    (3..=MAX_MASK_POINTS).contains(&m.len()) && m.iter().all(|p| p.0.is_finite() && p.1.is_finite())
}

/// Soft edge with widths clamped to 0..=0.5 and gamma to 0.1..=5; `None` if not finite.
pub fn clamp_soft_edge(s: &vjc_model::SoftEdge) -> Option<vjc_model::SoftEdge> {
    let all = [s.left, s.right, s.top, s.bottom, s.gamma];
    if !all.iter().all(|v| v.is_finite()) {
        return None;
    }
    let w = |v: f32| v.clamp(0.0, MAX_SOFT_WIDTH);
    Some(vjc_model::SoftEdge {
        left: w(s.left),
        right: w(s.right),
        top: w(s.top),
        bottom: w(s.bottom),
        gamma: s.gamma.clamp(GAMMA_RANGE.0, GAMMA_RANGE.1),
    })
}

/// Largest brightness / channel gain.
pub const MAX_GAIN: f32 = 2.0;

/// Colour with every factor clamped to 0..=2; `None` if any is not finite.
pub fn clamp_color(c: &vjc_model::SliceColor) -> Option<vjc_model::SliceColor> {
    let all = [c.brightness, c.r, c.g, c.b, c.contrast];
    if !all.iter().all(|v| v.is_finite()) {
        return None;
    }
    let g = |v: f32| v.clamp(0.0, MAX_GAIN);
    Some(vjc_model::SliceColor {
        brightness: g(c.brightness),
        r: g(c.r),
        g: g(c.g),
        b: g(c.b),
        contrast: g(c.contrast),
    })
}

/// Screen colour with every factor clamped to 0..=2 (opacity to 0..=1); `None` if any is
/// not finite.
pub fn clamp_screen_color(c: &vjc_model::ScreenColor) -> Option<vjc_model::ScreenColor> {
    if !c.opacity.is_finite() {
        return None;
    }
    Some(vjc_model::ScreenColor {
        opacity: c.opacity.clamp(0.0, 1.0),
        color: clamp_color(&c.color)?,
    })
}

/// CPU twin of the shader's colour stage: contrast around 0.5 (clamped to 0..1), then the
/// brightness x channel gain; `opacity` multiplies last. Neutral colour returns `rgb` as is.
pub fn apply_color(rgb: [f32; 3], c: &vjc_model::SliceColor, opacity: f32) -> [f32; 3] {
    let g = slice_gain(c);
    let mut out = [0.0; 3];
    for i in 0..3 {
        let k = if c.contrast == 1.0 {
            rgb[i]
        } else {
            ((rgb[i] - 0.5) * c.contrast + 0.5).clamp(0.0, 1.0)
        };
        out[i] = k * g[i] * opacity;
    }
    out
}

/// Final RGB multiplier of a slice: brightness times each channel gain. CPU twin of the
/// shader's `gain.rgb`.
pub fn slice_gain(c: &vjc_model::SliceColor) -> [f32; 3] {
    [c.brightness * c.r, c.brightness * c.g, c.brightness * c.b]
}

/// Repairs masks and soft edges read from disk so later edits and their undo never fail:
/// an invalid mask is dropped, points are clamped to 0..1, soft edge values are clamped (or
/// dropped when not finite). Returns how many slices changed.
pub fn sanitize_slice_effects(out: &mut vjc_model::OutputSetup) -> usize {
    let mut changed = 0;
    for sl in out.screens.iter_mut().flat_map(|s| &mut s.slices) {
        let before = (sl.mask.clone(), sl.soft_edge.clone(), sl.color);
        sl.color = clamp_color(&sl.color).unwrap_or_default();
        if sl.mask.as_ref().is_some_and(|m| !mask_valid(m)) {
            sl.mask = None;
        }
        sl.mask = sl.mask.take().map(|m| {
            m.into_iter()
                .map(|p| (p.0.clamp(0.0, 1.0), p.1.clamp(0.0, 1.0)))
                .collect()
        });
        sl.soft_edge = sl.soft_edge.as_ref().and_then(clamp_soft_edge);
        changed += usize::from(before != (sl.mask.clone(), sl.soft_edge.clone(), sl.color));
    }
    changed
}

/// Even-odd point-in-polygon test (the shader runs the same algorithm).
pub fn point_in_polygon(poly: &[Pt], p: Pt) -> bool {
    let mut inside = false;
    let mut j = poly.len().wrapping_sub(1);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Index where a new polygon point `p` should be inserted: after the vertex that starts the
/// nearest edge, so the outline stays simple.
pub fn nearest_edge_insert(poly: &[Pt], p: Pt) -> usize {
    let n = poly.len();
    let dist = |a: Pt, b: Pt| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 {
            (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
    };
    (0..n)
        .min_by(|&i, &j| {
            dist(poly[i], poly[(i + 1) % n]).total_cmp(&dist(poly[j], poly[(j + 1) % n]))
        })
        .map_or(0, |i| i + 1)
}

/// Brightness factor of a soft edge at slice-surface coordinate `(s, t)`:
/// product of the linear edge ramps, raised to `gamma`.
pub fn edge_ramp(e: &vjc_model::SoftEdge, s: f32, t: f32) -> f32 {
    let r = |d: f32, w: f32| {
        if w > 0.0 {
            (d / w).clamp(0.0, 1.0)
        } else {
            1.0
        }
    };
    let lin = r(s, e.left) * r(1.0 - s, e.right) * r(t, e.top) * r(1.0 - t, e.bottom);
    lin.powf(e.gamma)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mask_presets_are_valid_and_delay_frames_round() {
        for s in super::MaskShape::ALL {
            assert!(super::mask_valid(&super::mask_preset(
                s,
                (0.1, 0.2, 0.9, 0.8)
            )));
        }
        assert_eq!(super::delay_frames(0, 60.0), 0);
        assert_eq!(super::delay_frames(100, 60.0), 6);
        assert_eq!(super::delay_frames(500, 60.0), 6, "clamped to 100 ms");
        assert_eq!(super::delay_frames(50, 0.0), 0);
    }

    use super::*;

    #[test]
    fn apply_color_neutral_is_identity_and_zero_is_black() {
        let px = [0.001, 0.4, 0.93];
        let n = vjc_model::SliceColor::default();
        assert_eq!(apply_color(px, &n, 1.0), px);
        let dark = vjc_model::SliceColor {
            brightness: 0.0,
            ..n
        };
        assert_eq!(apply_color(px, &dark, 1.0), [0.0; 3]);
        assert_eq!(apply_color(px, &n, 0.0), [0.0; 3]);
        // negative control: a non-neutral contrast does change the pixel
        let hi = vjc_model::SliceColor { contrast: 2.0, ..n };
        assert_ne!(apply_color(px, &hi, 1.0), px);
    }

    #[test]
    fn slice_gain_multiplies_brightness_into_each_channel() {
        let c = vjc_model::SliceColor {
            brightness: 0.5,
            r: 1.0,
            g: 0.8,
            b: 2.0,
            contrast: 1.0,
        };
        assert_eq!(slice_gain(&c), [0.5, 0.4, 1.0]);
        assert_eq!(
            slice_gain(&Default::default()),
            [1.0; 3],
            "default is identity"
        );
        // Negative control: the clamp is what keeps a wild value in range.
        let wild = vjc_model::SliceColor {
            brightness: 9.0,
            ..c
        };
        assert_eq!(clamp_color(&wild).unwrap().brightness, MAX_GAIN);
        assert!(
            clamp_color(&vjc_model::SliceColor {
                r: f32::INFINITY,
                ..c
            })
            .is_none()
        );
    }

    #[test]
    fn unscaled_slice_detection() {
        let mut sl = new_slice(1920, 1080);
        assert!(slice_unscaled((1920, 1080), &sl));
        assert!(!slice_unscaled((1280, 720), &sl), "scaled up");
        sl.output_mesh[1].0 -= 10.0;
        assert!(!slice_unscaled((1920, 1080), &sl), "warped");
        let mut half = new_slice(960, 1080);
        half.input = vec![(0.0, 0.0), (0.5, 0.0), (0.5, 1.0), (0.0, 1.0)];
        assert!(slice_unscaled((1920, 1080), &half), "crop 1:1");
    }

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
    fn polygon_membership() {
        let tri = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)];
        assert!(point_in_polygon(&tri, (0.2, 0.2)));
        assert!(!point_in_polygon(&tri, (0.8, 0.8)));
        assert!(!point_in_polygon(&tri, (-0.1, 0.5)));
        // Concave "L": the notch is outside.
        let l = [
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 0.5),
            (0.5, 0.5),
            (0.5, 1.0),
            (0.0, 1.0),
        ];
        assert!(point_in_polygon(&l, (0.25, 0.75)));
        assert!(!point_in_polygon(&l, (0.75, 0.75)));
    }

    #[test]
    fn insert_goes_on_nearest_edge() {
        let sq = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        assert_eq!(nearest_edge_insert(&sq, (0.5, -0.1)), 1);
        assert_eq!(nearest_edge_insert(&sq, (1.1, 0.5)), 2);
        assert_eq!(nearest_edge_insert(&sq, (-0.1, 0.5)), 4);
    }

    #[test]
    fn edge_ramp_and_validation() {
        let e = vjc_model::SoftEdge {
            left: 0.25,
            right: 0.0,
            top: 0.0,
            bottom: 0.0,
            gamma: 1.0,
        };
        assert_eq!(edge_ramp(&e, 0.0, 0.5), 0.0);
        assert!((edge_ramp(&e, 0.125, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(edge_ramp(&e, 0.25, 0.5), 1.0);
        assert_eq!(edge_ramp(&e, 1.0, 0.5), 1.0);
        let g = vjc_model::SoftEdge {
            gamma: 2.0,
            ..e.clone()
        };
        assert!((edge_ramp(&g, 0.125, 0.5) - 0.25).abs() < 1e-6);
        let c = clamp_soft_edge(&vjc_model::SoftEdge {
            left: 0.9,
            right: -1.0,
            top: 0.1,
            bottom: 0.0,
            gamma: 99.0,
        })
        .unwrap();
        assert_eq!((c.left, c.right, c.top, c.gamma), (0.5, 0.0, 0.1, 5.0));
        let nan = vjc_model::SoftEdge {
            gamma: f32::NAN,
            ..e
        };
        assert!(clamp_soft_edge(&nan).is_none());
        assert!(!mask_valid(&[(0.0, 0.0), (1.0, 1.0)]));
        assert!(!mask_valid(&[(0.0, 0.0), (1.0, f32::NAN), (0.0, 1.0)]));
        assert!(mask_valid(&[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]));
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
