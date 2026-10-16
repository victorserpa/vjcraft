# VJCraft

An open-source, GPL-3.0 live-visuals (VJ) application written in Rust.
Free, open and built for the long show. Early release (v0.1.0).

**Clean-room.** The design comes from public documentation and observation of file
formats only. No third-party code, assets, names or icons are included.
VJCraft is not affiliated with, endorsed by, or derived from any commercial VJ software.

## Crates

- `vjc-model`: project data model (JSON serializable).
- `vjc-avc`: `.avc` importer (XML to model, returns warnings for unsupported features).
- `vjc-app`: desktop app (egui) with a layers x columns grid.

## Install (macOS, Apple Silicon)

1. `brew install ffmpeg` (the app links the Homebrew ffmpeg libraries).
2. Build the disk image: `scripts/package-macos.sh`. It writes `target/dist/VJCraft-<version>-macos.dmg` (ad-hoc signed, not notarized).
3. Open the .dmg, drag VJCraft to Applications. First launch: right-click > Open.

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

GPL-3.0, see `LICENSE`.
