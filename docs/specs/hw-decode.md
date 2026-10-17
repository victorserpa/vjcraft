# Hardware video decode
Issue: new · Status: draft · Estimate: 1.5-2 h

## Problem
H.264/HEVC are decoded on the CPU by FFmpeg. In the 4 h soak (8 layers, one 4K 80 Mbps H.264, one ProRes) the app used about 4.7 of 10 cores and fps sat at 54-55. Users who do not convert to HAP need this path to be cheap.

## Goals / non-goals
- Goal: H.264, HEVC and ProRes decode on the GPU media engine on macOS (VideoToolbox); D3D11VA/NVDEC on Windows and VAAPI on Linux behind the same switch.
- Goal: no behaviour change for seek, loop, speed, reverse, audio.
- Non-goal: zero-copy into wgpu in this step (frames still come back as CPU pixels or an IOSurface copy); that is a follow-up.

## Design
- vjc-media: when opening a stream, try `av_hwdevice_ctx_create` for the platform type; set `get_format` to pick the hw pixel format; on failure or unsupported profile fall back to software silently and log once.
- Transfer: `av_hwframe_transfer_data` to NV12/P010 in system memory, then the existing convert path (or an NV12→RGBA shader to skip swscale).
- Pref: Video > "Hardware decoding" (Auto / Off), default Auto. Per-clip status shows "hw" or "sw".
- Concurrency: VideoToolbox sessions are limited; cap simultaneous hw sessions (pref, default 8), extra clips use software.

## Test plan (written before the code)
- Unit: open a generated H.264 clip with hw on; decode 30 frames; compare against software decode (max channel error <= 4).
- Fallback: force the hw device creation to fail; frames still arrive via software.
- Seek and reverse: same frame index after seek with hw and sw.
- Benchmark (ignored test): CPU time per 1080p and 4K frame, hw vs sw.

## Acceptance
- Soak (same 8-layer setup, 30 min): CPU at least 40 % lower than the sw run, fps >= 58, 0 stalls.
- Off switch returns to the current behaviour.

## Risks and open questions
- 10-bit HEVC and ProRes RAW support differ per Mac model.
- Windows/Linux paths can only be checked on that hardware.
