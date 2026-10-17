# Advanced slices and masks
Issue: #7 · Status: draft · Estimate: 3-4 h

## Problem
The output window covers rectangular slices, mesh warp and shape masks. Missing: free polygon slices, freehand (pen) masks, smooth (Bezier) warp, shape presets for slices, resize of a multi-selection, output masks per screen.

## Goals / non-goals
- Goals: polygon slice (click to add points, close on first point), pen mask (freehand stroke simplified to a polygon), Bezier warp mode with per-point handles, shape presets (rect, circle, triangle, hexagon) for new slices, multi-select resize from the group bounding box, output mask on a screen.
- Non-goal: DMX fixture per slice (Art-Net editor covers it).

## Design
- Model: `SliceShape::{Rect, Polygon(Vec<Pt>)}`, `Mask::Path(Vec<Pt>)`, `Mesh.mode: Linear | Bezier` with per-point tangents; serde defaults keep old files loading.
- Render: polygon slices and path masks rasterised to a coverage texture once per edit (not per frame); Bezier mesh tessellated to a dense grid on edit.
- UI: + menu entries Polygon and Output mask; canvas tools for point add/move/delete; Shift snaps 15°.
- Undo: each finished shape or drag is one command.

## Test plan (written before the code)
- Model roundtrip of every new variant and of an old file.
- Point-in-polygon coverage matches a reference raster within 1 px.
- Bezier with zero tangents equals the linear mesh.
- UI tests: draw a polygon with 4 clicks; pen stroke simplifies to < 64 points; group resize scales every slice.
- GPU snapshot of each new shape.

## Acceptance
- A curved projection surface can be mapped with the Bezier mode; a non-rectangular LED area with a polygon slice.

## Risks and open questions
- Verify on a projector and on the LED wall (led-wall-test.md).
