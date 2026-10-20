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

## Stage 2a (approved): real plugins over an OpenGL helper, CPU frame transfer

Checked against the open FFGL SDK headers (BSD-3, `FFGL.h` and `FFGL.cpp`):
- Stage 1 uses the retired FFGL 1 codes, so real FFGL 2 plugins fail:
  - `FF_INITIALISE` (1) must be `FF_INITIALISE_V2` (34);
  - `FF_INSTANTIATE` / `FF_DEINSTANTIATE` (11 / 12) must be `FF_INSTANTIATE_GL` / `FF_DEINSTANTIATE_GL` (18 / 19).
- Other codes in use:
  - `FF_PROCESS_OPENGL` is 17. `inputValue` is a `ProcessOpenGLStruct*` (`numInputTextures`, `FFGLTextureStruct**`, `HostFBO`).
  - `FF_INSTANTIATE_GL` takes an `FFGLViewportStruct*` (`x, y, width, height`) and returns the instance pointer.
  - `FF_SET_TIME` is 20 and takes a `double*`.
  - `FF_RESIZE` is 23 and takes a viewport.
- `FFGLTextureStruct` is `{Width, Height, HardwareWidth, HardwareHeight, Handle}`.
- The return codes `FF_SUCCESS = 0` and `FF_FAIL = 0xFFFFFFFF` are unchanged. A failed instantiate returns `FF_FAIL` cast to a pointer.
- `FF_SET_PARAMETER` passes the float bit-cast in `UIntValue`, as stage 1 already does.

Design (macOS first, no new crates):
1. The helper creates an offscreen OpenGL 4.1 core context with CGL, loading `OpenGL.framework` symbols with dlsym. It also creates an FBO with an RGBA8 colour texture at the effect's size, plus one RGBA8 input texture.
2. Per frame:
   - upload the input frame from the shared mapping with `glTexSubImage2D`;
   - bind the FBO and call `FF_PROCESS_OPENGL` with one input texture and `HostFBO` set to our FBO;
   - `glReadPixels` the result back into the mapping.

   The frame exchange and timeouts stay as in stage 1, so a crash or hang still passes the frame through.
3. Lifecycle:
   - `FF_INITIALISE_V2`, then `FF_INSTANTIATE_GL` with the viewport;
   - `FF_RESIZE` when the size changes, `FF_SET_TIME` every frame;
   - `FF_DEINSTANTIATE_GL`, then `FF_DEINITIALISE`.

   The stage 1 CPU test code (0x7F00) stays for the fixture plugin only. A plugin that answers `FF_PROCESS_OPENGL` is driven over GL.
4. App wiring:
   - Prefs > Video gets a folder list for FFGL plugins.
   - The folders are scanned at start-up and when they change on save, with the cache from stage 1.
   - Effect plugins are listed in the Effects browser under their own group and can be added to a layer like any effect.
   - Their parameters show up as effect params, so they work with the dashboard, MIDI and OSC.
   - The compositor reads the layer back, runs the effect, and uploads the result. That is the CPU path; GPU texture sharing is stage 2b.
5. Source plugins and mixer plugins are out of scope for 2a.

Tests, written first:
- The constants test compares the code values with the numbers above.
- A fixture GL plugin, built by the test with rustc and calling GL through dlsym, inverts its input in `FF_PROCESS_OPENGL`. The test loads it in the helper, processes a 64×64 frame, and checks the inverted pixels. It skips with a note when no GL context can be created.
- The helper is killed mid-frame: the frame passes through and nothing panics. This is the stage 1 behaviour kept.
- UI: with a folder that holds the fixture, the plugin shows up in the Effects browser and can be added to a layer.

Not verifiable here: a real third-party FFGL plugin binary, which the owner tests with his own plugins; 60 fps; Windows.

## Stage 2a+: pipelined frames (approved)

Measured in a release build, at 1080p with one FFGL effect: about 12.5 ms per frame. All of it runs serially on the render thread:
- readback map and wait: 1.2 ms
- copy into shared memory: 0.7 ms
- synchronous IPC round trip, including the helper's GL upload, process and read: 5.6 ms
- copy back and upload: 0.9 ms

Design:
- **Double buffering.** Each FFGL effect gets two shared-memory slots, so the render thread never waits on the helper. On frame N it:
  1. writes the input into the free slot;
  2. sends `Process` without waiting;
  3. uploads the most recent finished result, which is frame N-1.

  The effect output is one frame late. That is invisible in live use, and it is shown in the effect's tooltip.
- **First frame, or no finished result yet:** pass the input through.
- **Helper too slow:** if it has not finished the previous request when the next frame arrives, skip that frame's send and keep the last result, so the render thread never blocks. If no answer arrives within the existing timeout, the effect is marked stopped, as today.
- **Parameter changes** ride on the next request, as now.

Tests, written first:
- a 1080p frame with an FFGL effect costs the render thread under 4 ms in a release build. This is an ignored perf test, run by hand;
- the output equals the inverted input of the previous frame;
- frame 1 passes through;
- a helper that sleeps 50 ms per frame does not slow the render loop: 10 frames finish in under 100 ms;
- a killed helper still passes through.
