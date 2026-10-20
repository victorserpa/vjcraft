#!/usr/bin/env bash
# Cross-checks the crates that build for Linux without system C libraries or a Linux C compiler.
# Needs: rustup target add x86_64-unknown-linux-gnu. Nothing is linked or run.
# Blocked (see docs/dev/build-test.md): vjc-ml (tract-linalg needs a Linux C compiler),
# vjc-media (alsa-sys, ffmpeg-sys), vjc-app (depends on both).
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."
args=()
for c in vjc-net vjc-model vjc-core vjc-io vjc-avc vjc-fx vjc-ffgl vjc-clap; do args+=(-p "$c"); done
exec cargo check -j 4 --target x86_64-unknown-linux-gnu "${args[@]}" "$@"
