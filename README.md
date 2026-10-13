# VJCraft

An open-source, GPL-3.0 live-visuals (VJ) application written in Rust.
Early work in progress (milestone M0a): data model, importer for third-party `.avc`
composition files, and a minimal clip-grid window. No video playback yet.

**Clean-room.** The design comes from public documentation and observation of file
formats only. No third-party code, assets, names or icons are included.
VJCraft is not affiliated with, endorsed by, or derived from any commercial VJ software.

## Crates

- `vjc-model`: project data model (JSON serializable).
- `vjc-avc`: `.avc` importer (XML to model, returns warnings for unsupported features).
- `vjc-app`: desktop app (egui) with a layers x columns grid.

## Run

```
cargo run -p vjc-app --release
cargo test
```

Use File > Import .avc... to load a composition, click a cell to mark it active.

## License

GPL-3.0, see `LICENSE`.
