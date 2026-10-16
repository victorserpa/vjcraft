# Contributing to VJCraft

Thanks for helping. VJCraft is GPL-3.0; by sending a pull request you agree your change is
released under the same license.

## Before you start

- Open or pick an issue first, so work is not duplicated.
- Clean-room rules (`docs/dev/clean-room.md`): never copy code, assets, presets, icons, UI text
  or screenshots from commercial VJ software. Interop (reading `.avc` files, Syphon/NDI/Spout
  names) is fine.

## Build and test

See `docs/dev/build-test.md`. Short version:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

GPU tests skip when no adapter is available. UI tests use `egui_kittest`.

## Pull requests

- One topic per PR, with a test that fails before and passes after for any bug fix.
- Keep the show safe: selecting, loading or dropping media must never change what is on air
  unless that cell is already on air; black-out must give black on every output; autosave must
  never overwrite an unanswered recovery file.
- Platforms: macOS is the main target today; Windows and Linux are being brought up (see the
  "cross-platform" issue). Reports from those systems are very welcome.
