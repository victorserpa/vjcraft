# Third-party components

## Ableton Link (via rusty_link 0.4.9)

- Used for tempo/phase sync (cargo feature `link` of `vjc-app`, on by default).
- License: GPL-2.0-or-later, compatible with this project (GPL-3.0-only).
- Source: https://github.com/Ableton/link, bindings: https://crates.io/crates/rusty_link
- Build without it (no cmake needed): `cargo build -p vjc-app --no-default-features`.

## Syphon Framework (vendored in `vendor/syphon`)

- Used for Syphon input and output on macOS (cargo feature `syphon`, on by default; built by `crates/vjc-net/build.rs` with the `cc` crate, Metal API only).
- License: BSD 2-clause, see `vendor/syphon/License.txt`. Source: https://github.com/Syphon/Syphon-Framework, commit f476167.
- Only the Metal server/client and discovery files are vendored (no OpenGL code). One local change: `SyphonServerRendererMetal.m` compiles its two shaders from source at runtime instead of loading a `.metallib`, so no Metal offline toolchain is needed to build.

## NDI (runtime only, not distributed)

- Used for NDI input and output (cargo feature `ndi`, on by default). The NDI library (`libndi`) is loaded at runtime with `libloading` from NDI Tools / the NDI runtime (`NDI_RUNTIME_DIR_V6`, `/usr/local/lib`, `/Library/NDI SDK for Apple`). VJCraft neither links, bundles nor downloads it; without it NDI is reported as unavailable.
- The few C structs and functions used are declared by hand in `crates/vjc-net/src/ndi.rs`. NDI is a trademark of Vizrt NDI AB; users install the runtime under its own license (https://ndi.video).

## FFmpeg (via ffmpeg-next 9.0.0, `crates/vjc-media`)

- Video and audio decode. The .app/.dmg bundles the Homebrew build of the libav* libraries and their dependencies in `Contents/Frameworks` (copied by `scripts/bundle-dylibs.sh`, install names rewritten to `@rpath`). Binding crate `ffmpeg-next`: WTFPL. That Homebrew FFmpeg is configured with `--enable-gpl --enable-version3`, so the bundled libav* are GPL-3.0-or-later (LGPL-2.1+ otherwise); the project is GPL-3.0-only, so this is compatible, and the whole bundle is distributed under GPL-3.0. Source: https://ffmpeg.org (the corresponding source is the upstream release matching the bundled version; offer it with every .dmg).
- Bundled libraries (versions as built by Homebrew at packaging time): libavcodec/avdevice/avfilter/avformat/avutil/swresample/swscale (GPL-3.0+ as configured), libx264 (GPL-2.0+), libx265 (GPL-2.0+), libvpx (BSD-3-Clause), libdav1d (BSD-2-Clause), libSvtAv1Enc (BSD-3-Clause-Clear + AOM patent license), libopus (BSD-3-Clause), libmp3lame (LGPL-2.0+), libmpg123 (LGPL-2.1), liblzma (0BSD / public domain), libvmaf (BSD-2-Clause+Patent), libssl/libcrypto (OpenSSL 3, Apache-2.0). Re-check the list and licenses whenever the Homebrew ffmpeg changes.

## tiny_http and tungstenite (Remote API)

- Used by the optional Remote API (`crates/vjc-app/src/api_io.rs`): `tiny_http` 0.12 serves REST, `tungstenite` 0.30 (feature `handshake` only, no TLS) serves WebSocket. Both are off the network unless the user enables the API in Preferences.
- Licenses: tiny_http Apache-2.0 / MIT, tungstenite Apache-2.0 / MIT; both compatible with GPL-3.0-only. Sources: https://github.com/tiny-http/tiny-http, https://github.com/snapview/tungstenite-rs.

## tract (ML inference, `crates/vjc-ml`)

- ONNX inference for the Depth and Remove Background effects, pinned to `tract-onnx` 0.23.8. License: Apache-2.0 / MIT, compatible with GPL-3.0-only. Source: https://github.com/sonos/tract.

## ML models (not distributed)

- The effects load `.onnx` files from a user folder; VJCraft neither bundles nor downloads them (`*.onnx` is git-ignored). How to get them: `docs/ml.md`.
- MiDaS v2.1 small (`model-small.onnx`), Depth effect. License: MIT. Source: https://github.com/isl-org/MiDaS.
- Robust Video Matting, MobileNetV3 fp32 (`rvm_mobilenetv3_fp32.onnx`), Remove Background effect. License: GPL-3.0, same as this project. Source: https://github.com/PeterL1n/RobustVideoMatting.

## Inter (font, `assets/fonts/`)

- Proportional UI font (Regular and SemiBold, embedded with `include_bytes!`). License: SIL OFL 1.1, see `assets/fonts/Inter-LICENSE.txt`. Source: https://github.com/rsms/inter (release v4.1).

## egui-phosphor 0.14 and Phosphor Icons

- Icon font for the UI (crate `egui-phosphor`, depends only on egui, no build script, no network). Crate and icons: MIT. Sources: https://crates.io/crates/egui-phosphor, https://phosphoricons.com.

## epaint 0.36.2 (vendor/epaint)

Copy of the `epaint` crate from crates.io, used through `[patch.crates-io]`, with one change
in `src/text/fonts.rs` (the galley cache keeps two generations, so an immediate viewport does
not evict the main UI's text every frame). License: MIT OR Apache-2.0; used under MIT, see
`vendor/epaint/LICENSE-MIT`. Copyright (c) 2018-2021 Emil Ernerfeldt.
