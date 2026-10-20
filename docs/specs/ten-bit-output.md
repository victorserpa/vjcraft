# Output dithering and 10-bit output

Issue: #11 · Status: approved (phase 1) · Estimate: 0.5 day (phase 1)

## Problem

A composition can render at 16-bit (`Composition.bit_depth`), but every output is quantized to 8-bit. That truncation shows bands on slow gradients.

Most LED processors take only 8-bit, so a 10-bit swapchain would help few users. Also, egui_wgpu 0.36 forces one 8-bit surface format for every viewport:
- `WgpuConfiguration` has no format field;
- `preferred_framebuffer_format` returns Rgba8 or Bgra8 first;
- there is no per-viewport override.

## Already done

- Contrast per slice: `SliceColor.contrast`.
- Composition bit depth: `Composition.bit_depth`, default 8.

## Goals and non-goals

- **Phase 1 (now):** an ordered dither when a 16-bit composition is written to an 8-bit output. This covers screen windows, NDI and Syphon, and removes visible banding on 8-bit hardware.
- **Phase 2 (parked):** an Rgb10a2 surface for output windows, through our own wgpu surface outside egui_wgpu. It can reuse the 16-bit to Rgb10a2 blit on `feat/ten-bit-output`.
- **Non-goals:**
  - HDR;
  - 10-bit NDI or Syphon. These are parked because they need receivers that accept 10-bit.

## Data model

`Output.dither: bool`, project-wide, with `#[serde(default = "default_true")]`. It only takes effect when `composition.bit_depth > 8`. Old projects load with it on, which changes nothing for 8-bit compositions.

## Render path

1. With bit_depth 16, the composition texture stays 16-bit.
2. The output pass applies the slice colour. Before the 8-bit store, it adds `(bayer8(frag_xy) - 0.5) / 255`, where `bayer8` is an 8×8 Bayer matrix in the shader. No texture or asset is needed.
3. The pattern is fixed in space, with no change over time. This avoids flicker on LED processors that scale or compress.
4. Exact 0 and exact 1 are not dithered.
5. Dithering is skipped when bit_depth is 8 or dither is off. This is a uniform flag, not an extra pass.
6. NDI and Syphon read the same 8-bit screen texture, so they need no extra work.

## UI

Output settings get a "Dither 16-bit to 8-bit output" checkbox. It is greyed out when the composition is 8-bit.

## Tests, written first

- **Model:** a project without `dither` loads as `true`, and a round trip keeps `false`.
- **Headless GPU:** a 16-bit ramp from 0.40 to 0.42 rendered into an 8-bit target 1024 px wide.
  - With dither off: at most 6 distinct values, and runs of more than 100 equal pixels.
  - With dither on: the mean over each 64 px block stays within 0.5/255 of the ramp, and runs of equal pixels stay under 90 px (one 8×8 Bayer row spans about two thirds of the thresholds).
- **Headless GPU:** with bit_depth 8, turning dither on gives output byte-identical to dither off.
- **Headless GPU:** black stays exactly 0 and white exactly 255.

## Needs hardware

A gradient on the LED wall, through an 8-bit processor, shows no visible bands and no flicker on camera.

## Risks

Dithering may be visible at very low brightness on LED walls. The checkbox lets you turn it off.

## Phase 2 spike (2026-10-20): parked

A 10-bit surface for output windows was checked against egui/eframe 0.36.2:

- `egui_wgpu::RenderState` holds a single `target_format`, picked once by
  `preferred_framebuffer_format` and shared by every viewport and its renderer.
- eframe exposes a raw window handle only for the root window
  (`epi_integration.rs`), not per immediate viewport.

So a 10-bit output window needs either a patched egui-winit (to own a
viewport's surface) or output windows opened on raw winit outside egui, which
means re-implementing placement, fullscreen and overlays. Neither can be
verified without a 10-bit panel, and the dither from phase 1 already removes
the banding on 8-bit outputs. Phase 2 stays parked until egui exposes per-viewport
surfaces or a 10-bit display is available for testing. `select_output_format`
and the `output_10bit` preference stay as they are; the startup warning still
explains the fallback.
