# FFGL plugin host
Issue: #28 · Status: draft · Estimate: ~1 day

## Problem
Many VJs own FFGL effect and source plugins; VJCraft cannot load them.

## Goals / non-goals
- Goal: load FFGL 2.x plugins (effects and sources) from folders set in Prefs > Video; parameters exposed like ISF params (dashboard, MIDI/OSC, automation).
- Goal: a crashing plugin does not take the app down.
- Non-goal: FFGL 1.x, CPU FreeFrame plugins.

## Design
- FFGL is an OpenGL API; the compositor is wgpu. Run plugins in a helper process with an OpenGL context; share textures via IOSurface (macOS) or DXGI shared handles / GL interop (Windows); frames exchanged through a small shared-memory ring with timeouts.
- If the helper dies or times out, the effect shows "plugin stopped" and passes the input through; the helper restarts on next enable.
- Scan results cached with plugin path + mtime.

## Test plan (written before the code)
- Build an open-source FFGL example plugin in the test; load it; process a frame; compare output with the plugin's documented behaviour.
- Kill the helper mid-frame: the layer keeps rendering (pass-through) and no panic.
- Parameter mapping roundtrip.

## Acceptance
An open-source FFGL plugin appears in Effects and runs on a layer at 60 fps.

## Risks and open questions
- GL deprecation on macOS; shared-texture interop details per platform.

## Implementation status
Stage 1 (crate `vjc-ffgl`): done.
- Helper process `vjc-ffgl-helper` loads the plugin (`plugMain` ABI, written from the documented layout, nothing vendored); line-JSON protocol on stdin/stdout, request timeout, kill on timeout or death.
- Frames cross through a CPU shared file mapping (single slot, input then output), not a ring and not textures.
- Crash, hang or timeout: `FfglEffect::process` returns the input unchanged and `status` is `Stopped(..)` ("plugin stopped"); `enable()` restarts the helper and restores parameters.
- Scan cache by path + mtime; a plugin that crashes during the scan is skipped. Effect id `ffgl:<id>`; parameters map to `vjc_fx::Input` (float 0..1, bool). `Prefs.video.ffgl_dirs` stores the folders.
- Tests build their own minimal plugin (tests/fixtures/gain_plugin.rs) with rustc. It answers a VJCraft-only CPU process code (0x7F00); real FFGL plugins do not, so they report "needs the OpenGL host".

Stage 2 (not done): OpenGL context in the helper and PROCESSOPENGL, IOSurface / DXGI texture sharing, frame ring, Prefs > Video folder field, listing the entries in the Effects browser and compositor layer wiring, 60 fps acceptance with a real plugin. FFGL function-code constants were written from memory of the public spec and are not checked against the SDK.
