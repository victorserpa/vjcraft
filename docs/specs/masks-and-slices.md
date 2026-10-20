# Masks and slices: remaining work

Issue: #7 · Status: approved · Estimate: 1.5–2 days

## Problem

Most of #7 is done:
- mask effects
- group masks
- Bezier slice warp
- pen masks
- presets
- lasso
- soft edge

Three gaps remain:
- the content inside a slice cannot be zoomed or shifted;
- mask outlines are always straight lines;
- a DMX fixture cannot follow a slice.

## Goals and non-goals

Goals, in order of value per effort:
1. **Slice Transform:** scale, position and rotation of the content inside each slice.
2. **Fixture follows slice:** a DMX fixture samples the area of a chosen slice.
3. **Curved mask edges:** polygon and pen masks can have smooth corners.

Non-goal: animating the transform from audio or an LFO. That can come later through the existing parameter modulation.

## 1. Slice Transform (PR 1, about 350 lines)

- **Behaviour:** the slice properties get a "Transform" section with Scale (0.1–10), Position X/Y and Rotation. The content zooms or pans inside the slice, and the slice outline on the output does not move. With the identity values (scale 1, offset 0, rotation 0), the output is the same as today.
- **Pivot:** the transform pivots on the centre of the slice's input area.
- **Model:** `Slice.transform: SliceTransform { scale, offset: (f32, f32), rotation }` with `#[serde(default)]`. The default is the identity, so old projects load unchanged.
- **Render:** the input UV goes through the inverse transform before sampling, in the slice shader used by `crates/vjc-app/src/slice_render.rs`. Areas outside the source are transparent.
- **UI:** a fold section in `crates/vjc-app/src/output_props.rs` next to "Mask" (line 808).
  - Right-clicking a slider resets it to its default, as every slider already does.
  - A drag is one undo step.
- **Tests, written first:**
  - The model round-trips, and an old file loads with the identity transform.
  - On the GPU, the identity matches the current output, and scale 2 shows the centre quarter of the source.
  - In the UI, dragging Scale and then undoing restores 1.0.

## 2. Fixture follows slice (PR 2, about 400 lines)

- **Behaviour:** the Art-Net fixture editor gets an input picker with "Free rect" (today's behaviour) or "Slice: <name>". When the slice moves, the fixture follows it.
- **Model:** `Fixture.input_slice: Option<Uuid>` with `#[serde(default)]`. `None` uses the existing `input` rect. If the slice is deleted, the fixture falls back to its rect and the UI shows a note.
- **Sampling:** the fixture samples the bounding box of the slice's output area. Polygon coverage is out of this round.
- **Tests, written first:**
  - The model round-trips with and without `input_slice`.
  - The rect derived from a moved slice matches its bounds within 1 px.
  - A Uuid that points to no slice falls back to the rect.
- **Hardware:** check on a real Art-Net fixture.

## 3. Curved mask edges (PR 3, about 450 lines)

- **Behaviour:**
  - In point edit, Alt+drag on a point pulls out smooth handles.
  - Alt+click turns the point back into a sharp corner.
  - Pen strokes get a "Smooth" slider.
- **Model:** `Mask::Polygon` and `Mask::Path` stay as they are. Two fields are added, both `#[serde(default)]`: `Screen.output_mask_tangents` and `Slice.mask_tangents`. An empty list means straight edges, the same convention as `mesh_tangents`.
- **Render:** curves are flattened into a dense polygon when you edit, not every frame, and fed to the existing coverage raster. The Bezier step is shared with `crates/vjc-core/src/slice_adv.rs`.
- **Tests, written first:**
  - With zero tangents, the coverage is the same as today.
  - Four points with tangents match a reference disc within 1 px.
  - In the UI, Alt+drag adds a handle, and undo removes it.
- **Hardware:** check the projector and the LED wall for aliasing on the curves.

## Acceptance

A slice zoomed with Transform, a fixture following a slice and a rounded mask are all visible in the output window and in the Syphon/NDI readback.
