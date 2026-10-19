# VJCraft

An open-source, GPL-3.0 live-visuals (VJ) application written in Rust.
Free, open and built for the long show. Alpha release (v0.1.0).

**Clean-room.** The design comes from public documentation and observation of file
formats only. No third-party code, assets, names or icons are included.
VJCraft is not affiliated with, endorsed by, or derived from any commercial VJ software.

## Crates

- `vjc-model`: project data model (JSON serializable).
- `vjc-avc`: `.avc` importer (XML to model, returns warnings for unsupported features).
- `vjc-app`: desktop app (egui) with a layers x columns grid.

## Install (macOS, Apple Silicon)

1. Download `VJCraft-<version>-macos.dmg` from [Releases](https://github.com/victorserpa/vjcraft/releases).
2. Open the .dmg and drag VJCraft to Applications.
3. First launch: right-click the app > Open. The build is ad-hoc signed and not notarized.

FFmpeg ships inside the app; nothing else is needed. To build the disk image yourself, run
`scripts/package-macos.sh`. It writes `target/dist/VJCraft-<version>-macos.dmg`.

Optional: NDI Tools for NDI, ONNX models for Depth and Remove Background (`docs/ml.md`).

## Use

```
cargo run -p vjc-app --release     # run from source
cargo test
```

- Click a cell to trigger a clip; click a column header to trigger the whole column.
- File > Import .avc... loads a composition from other VJ software; unsupported parts are listed as warnings.
- Syphon, NDI, MIDI and Ableton Link are set up in Preferences. The Remote API is off by default (`docs/api.md`).
- No telemetry. Network is used only by features you turn on: Remote API, NDI, Ableton Link, OSC and Art-Net.

## Platforms

macOS (Apple Silicon) is tested. Windows and Linux builds are in progress; please report what
works and what breaks.

## Contributing

See `CONTRIBUTING.md`. Bugs and ideas go to GitHub Issues.

## License

GPL-3.0, see `LICENSE`. Bundled third-party licenses: `docs/dev/third-party.md`.
