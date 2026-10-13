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

## tiny_http and tungstenite (Remote API)

- Used by the optional Remote API (`crates/vjc-app/src/api_io.rs`): `tiny_http` 0.12 serves REST, `tungstenite` 0.30 (feature `handshake` only, no TLS) serves WebSocket. Both are off the network unless the user enables the API in Preferences.
- Licenses: tiny_http Apache-2.0 / MIT, tungstenite Apache-2.0 / MIT; both compatible with GPL-3.0-only. Sources: https://github.com/tiny-http/tiny-http, https://github.com/snapview/tungstenite-rs.
