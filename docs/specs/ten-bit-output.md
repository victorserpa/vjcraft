# 10-bit output
Issue: #11 · Status: draft · Estimate: 0.5-1 day

## Problem
The composition can render at 16-bit, but output windows still present 8-bit: egui_wgpu picks one global surface format.

## Goals / non-goals
- Goal: output windows present Rgb10a2 when the pref is on and the surface supports it; the main UI stays 8-bit.
- Non-goal: HDR / PQ.

## Design
- Output windows get their own wgpu surface configured outside egui_wgpu (they already render the screen texture; only a blit is needed), choosing Rgb10a2Unorm when listed in surface capabilities.
- Fallback to 8-bit with a status note.

## Test plan (written before the code)
- Format selection unit tests (exists).
- Headless: blit 16-bit gradient to an Rgb10a2 target and check there are more than 256 distinct levels.

## Acceptance
Gradient test without banding on the owner's 10-bit display.

## Implementation status
- Done: `output_blit.rs` (16-bit to any-format blit) with the headless gradient test (more than 256 levels on Rgb10a2; 8-bit control capped at 256); format selection and the fallback note exist.
- Left: output windows are eframe viewports, whose surface and format egui_wgpu owns and does not expose; presenting Rgb10a2 needs our own winit window plus surface (or an eframe change). Until then outputs stay 8-bit.

## Risks and open questions
- Needs a 10-bit capable display to verify.
