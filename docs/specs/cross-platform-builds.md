# Windows and Linux builds
Issue: #15 · Status: draft · Estimate: 2-3 h + a test session on a Windows PC

## Problem
Only macOS has been built and run. Shows often run on Windows PCs with NVIDIA GPUs.

## Goals / non-goals
- Goal: the app builds, launches, plays clips and drives fullscreen outputs on Windows 10/11 and Ubuntu 22.04+.
- Goal: CI builds all three on every PR (the repo is public, so runners are free).
- Non-goal: Syphon on non-macOS (Spout on Windows is a follow-up), signing.

## Design
- Audit every `cfg(target_os = "macos")` and Unix-only call (libc, getrusage, statvfs, signals, `sample`, paths) and give each a Windows/Linux branch or a stub.
- FFmpeg on Windows: prebuilt shared LGPL build pinned by version and checksum, copied next to the exe.
- Supervisor on Windows: child exit codes instead of signals; Force Quit = TerminateProcess code 1 must not relaunch (treat as user stop).
- Data dir: `%APPDATA%\VJCraft`, Linux `$XDG_DATA_HOME/vjcraft`.
- CI matrix: ubuntu-latest, windows-latest, macos-latest: fmt, clippy, test (GPU tests skip without adapter).

## Test plan (written before the code)
- CI green on the three OSes.
- Unit tests for the new platform branches (free space, data dir, exit-code classification).
- Owner session on the Windows PC: checklist in led-wall-test.md, plus 30 min soak.

## Acceptance
- Windows zip/installer launches on the owner's PC, plays 4 layers, outputs fullscreen to a second display.

## Risks and open questions
- wgpu backend on Windows (DX12 vs Vulkan) per GPU; HAP BC formats are supported on desktop GPUs.
