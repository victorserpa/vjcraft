# Build, test, run

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cargo run -p vjc-app                      # app
VJC_AUTOPLAY=path/to/video.mp4 ./target/debug/vjcraft   # starts playing a clip
scripts/package-macos.sh                  # target/dist/VJCraft.app + .dmg (needs brew ffmpeg; Developer ID + notarize, `--no-notarize` or `--adhoc` for local)
```

- `target/` grows past 50 GB; `cargo clean` when disk is short.
- Visual check without clicking: launch with VJC_AUTOPLAY, find the window id (CGWindowListCopyWindowInfo, owner "vjcraft"), `screencapture -x -o -l <id> out.png`. Kill only that PID.
- GPU snapshot tests write to target/ui-snapshots/; pattern: `gpu_master_opacity_and_bypass_fade_to_black`.
- Soak test: `VJC_SOAK_MINUTES=N cargo test -p vjc-app soak -- --ignored`.
- Tests read the owner's local Example.avc when present (vjc-avc/src/lib.rs); they skip when absent.
