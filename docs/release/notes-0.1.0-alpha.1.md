# VJCraft 0.1.0-alpha.1 (macOS)

First alpha on the road to 1.0 (see `docs/release/1.0-checklist.md`). It is for testers: everything below runs in the repo's test suite, but most of it has not been checked on real show hardware yet.

## Install
1. Download `VJCraft-0.1.0-alpha.1-macos.dmg` and check its SHA-256 against the one on the release page.
2. Open the .dmg and drag VJCraft to Applications.
3. First launch: right-click the app > Open. This build is ad-hoc signed and not notarized.

Requires macOS 12 or later. Only Apple Silicon is tested.

## What is in it
- **Composition**: layers, groups, columns, transport, blend modes, crossfader, transitions, 16-bit composition, masks, generators, transform widget, presets and palette (#17-#24, #27).
- **Media**: H.264/HEVC/ProRes with hardware decode and software fallback, native HAP playback on the GPU, Convert to HAP, alpha WebM, rotation; bundled LGPL FFmpeg built for macOS 12 (#34, #38, #39, #44, #54, #56).
- **Output**: rebuilt Output setup with many screens, polygon slices, Bezier mesh warp, output masks with curved edges, Slice Transform, soft edge, dithering of 16-bit compositions to 8-bit outputs (#25, #26, #40, #64, #66, #68).
- **Art-Net**: fixtures can follow a slice (#65).
- **Plugins**: FFGL 2 video effects in a crash-isolated OpenGL helper, with pipelined frames and async readback; CLAP audio effects with crash-safe pass-through (#43, #45, #71, #72, #79, #87).
- **DJ sync**: listen-only network tempo sync, and a clip can follow a DJ player; off by default (#46, #74).
- **Control**: web remote on the local network, REST and MCP APIs with real preview and output snapshots (#16, #21, #76).
- **Reliability**: a supervisor relaunches the app after a crash and restores the project, plugin and helper processes are isolated, crash and stall reports are written, and there is now a log file with View > Show Logs in Finder (#32, #52, #62, #85).

## Verified on hardware vs not
- Verified: a 4 h soak with 8 layers and software decode (about 4.7 of 10 cores, 54-55 fps).
- **Not verified yet**: LED wall output, DJ sync with real players, real FFGL and CLAP plugin packs, macOS 12, a 2-hour show soak on this packaged build. Please run `docs/testing/hardware-test-guide.md`.

## Known limits
- Output is 8-bit. The 10-bit surface is parked; a dither toggle is in Output setup.
- macOS only. Linux and Windows come in 1.x; Linux has a type-check only.
- FFGL: FFGL 2 effects only, frames go through the CPU (no GPU texture sharing), at least two frames of latency. CLAP: parameters only, no plugin GUIs, no VST3.
- DJ sync is listen-only. Follow-player needs status packets that may not reach a passive listener.

## Reporting a bug
Use View > Show Logs in Finder and attach the `vjcraft.log*` files. Also send `~/Library/Application Support/VJCraft/crashes/` from that day, `prefs.json`, and the project if you can. File it on GitHub with the `hardware` label when it is about a real rig.
