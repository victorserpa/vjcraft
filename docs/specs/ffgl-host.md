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
