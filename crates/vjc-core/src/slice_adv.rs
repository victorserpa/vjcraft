//! Advanced slices and masks: polygon slices, freehand (pen) masks, Bezier warp, shape
//! presets and group resize. Pure geometry; the compositor rasterises once per edit.

use crate::mapping::Pt;
use crate::slice_geom::Rect;
use vjc_model::ShapePreset;

/// Most points a polygon slice, mask or simplified pen stroke may have.
pub const MAX_POLY_POINTS: usize = 63;

/// Even-odd point-in-polygon test.
pub fn point_in_polygon(_poly: &[Pt], _p: Pt) -> bool {
    todo!()
}

/// Coverage texture of `poly` (normalized 0..1) at `w` x `h`: 255 inside, 0 outside,
/// sampled at pixel centres, row-major.
pub fn rasterize_coverage(_poly: &[Pt], _w: u32, _h: u32) -> Vec<u8> {
    todo!()
}

/// Simplifies a freehand stroke (Ramer-Douglas-Peucker) to at most `max` points, growing
/// the tolerance from `tol` until it fits.
pub fn simplify_stroke(_stroke: &[Pt], _tol: f32, _max: usize) -> Vec<Pt> {
    todo!()
}

/// `to` moved onto the 15 degree (or `step_deg`) ray nearest to the one from `from`.
pub fn snap_angle(_from: Pt, _to: Pt, _step_deg: f32) -> Pt {
    todo!()
}

/// Polygon of a preset inside `r` (any unit).
pub fn preset_polygon(_preset: ShapePreset, _r: Rect) -> Vec<Pt> {
    todo!()
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
    pub fn click(&mut self, _p: Pt, _close_radius: f32) -> DrawStep {
        todo!()
    }
}

/// Tessellates a Bezier mesh to a dense grid: `sub` steps per cell. Handles are offsets
/// from the one-third points of each segment (zero handles = the linear mesh). Returns the
/// points and the column count.
pub fn tessellate_bezier(
    _mesh: &[Pt],
    _cols: u32,
    _tangents: &[(Pt, Pt)],
    _sub: u32,
) -> (Vec<Pt>, u32) {
    todo!()
}

/// Scales every point of `meshes` from the group bounding box onto `to`.
pub fn resize_group(_meshes: &[Vec<Pt>], _to: Rect) -> Vec<Vec<Pt>> {
    todo!()
}
