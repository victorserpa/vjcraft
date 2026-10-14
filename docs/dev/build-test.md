# Build, test, run

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cargo run -p vjc-app                      # app
VJC_AUTOPLAY=path/to/video.mp4 ./target/debug/vjcraft   # starts playing a clip
VJC_DATA_DIR=/tmp/vjc-data VJC_REHEARSE=1 VJC_AUTOPLAY=video.mp4 ./target/debug/vjcraft   # LED rehearsal (dev only): 1920x1080 screen, 3 slices (middle = layer alone, right = soft edge + mask), test card, windowed output (the OS picks its position)
scripts/package-macos.sh                  # target/dist/VJCraft.app + .dmg (needs brew ffmpeg; Developer ID + notarize, `--no-notarize` or `--adhoc` for local; override SIGN_ID and NOTARY_PROFILE, see the script header)
```

- `target/` grows past 50 GB; `cargo clean` when disk is short.
- Visual check without clicking: launch with VJC_AUTOPLAY, find the window id (CGWindowListCopyWindowInfo, owner "vjcraft"), `screencapture -x -o -l <id> out.png`. Kill only that PID.
- GPU snapshot tests write to target/ui-snapshots/; pattern: `gpu_master_opacity_and_bypass_fade_to_black`.
- Soak test: `VJC_SOAK_MINUTES=N cargo test -p vjc-app soak -- --ignored`.
- Tests read the owner's local Example.avc when present (vjc-avc/src/lib.rs); they skip when absent.
- `VJC_DATA_DIR=<dir>` (dev/testing) replaces the per-user data and config dir (recovery.vjc, prefs.json, output-presets, models), so runs never touch the real one. Rehearsal (`VJC_REHEARSE`) also disables autosave and recovery writes.
