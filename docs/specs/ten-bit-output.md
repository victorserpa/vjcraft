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

## Risks and open questions
- Needs a 10-bit capable display to verify.
