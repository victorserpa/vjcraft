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

- Video and audio decode. Linked dynamically to the libav* libraries of the user's Homebrew `ffmpeg` (`/opt/homebrew/opt/ffmpeg/lib`); the .app and .dmg do not bundle them. Binding crate `ffmpeg-next`: WTFPL. FFmpeg itself is LGPL-2.1-or-later, or GPL-2.0-or-later when built with GPL options (the Homebrew build enables them); both are compatible with GPL-3.0-only for this use. Source: https://ffmpeg.org. If a future .dmg bundles the libraries, ship their license texts and source offer with it.

## tiny_http and tungstenite (Remote API)

- Used by the optional Remote API (`crates/vjc-app/src/api_io.rs`): `tiny_http` 0.12 serves REST, `tungstenite` 0.30 (feature `handshake` only, no TLS) serves WebSocket. Both are off the network unless the user enables the API in Preferences.
- Licenses: tiny_http Apache-2.0 / MIT, tungstenite Apache-2.0 / MIT; both compatible with GPL-3.0-only. Sources: https://github.com/tiny-http/tiny-http, https://github.com/snapview/tungstenite-rs.

## tract (ML inference, `crates/vjc-ml`)

- ONNX inference for the Depth and Remove Background effects, pinned to `tract-onnx` 0.23.8. License: Apache-2.0 / MIT, compatible with GPL-3.0-only. Source: https://github.com/sonos/tract.

## ML models (not distributed)

- The effects load `.onnx` files from a user folder; VJCraft neither bundles nor downloads them (`*.onnx` is git-ignored). How to get them: `docs/ml.md`.
- MiDaS v2.1 small (`model-small.onnx`), Depth effect. License: MIT. Source: https://github.com/isl-org/MiDaS.
- Robust Video Matting, MobileNetV3 fp32 (`rvm_mobilenetv3_fp32.onnx`), Remove Background effect. License: GPL-3.0, same as this project. Source: https://github.com/PeterL1n/RobustVideoMatting.
