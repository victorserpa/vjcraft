# FFmpeg with the HAP encoder
Issue: new · Status: draft · Estimate: 0.5-1 h

## Problem
"Convert to HAP" needs an FFmpeg built with libsnappy. The Homebrew FFmpeg used for local builds and bundled into the macOS app has no HAP encoder, so conversion fails with "ffmpeg cannot write HAP" and the H.264-vs-HAP soak cannot run.

## Goals / non-goals
- Goal: a reproducible FFmpeg build (LGPL configuration where possible) with `--enable-libsnappy` for macOS, then Windows/Linux in CI.
- Goal: the app finds and uses that binary for conversion; playback keeps using the linked libraries.
- Non-goal: changing the libraries the app links for decode.

## Design
- `scripts/build-ffmpeg.sh`: pinned FFmpeg release tarball + checksum, pinned snappy release + checksum, configure flags listed in the script, output to `target/ffmpeg/`.
- Audit before use: source origin, licenses (FFmpeg LGPL/GPL flags, snappy BSD-3), no network at build time beyond the two pinned downloads.
- Packaging copies the binary into the app bundle (`Contents/Resources/ffmpeg`); the converter looks there first, then PATH.
- THIRD_PARTY.md and the About box list FFmpeg and snappy with their licenses.

## Test plan (written before the code)
- Convert a generated 1080p clip to Hap, Hap Alpha, Hap Q with the built binary; open each with the native HAP path; compare with the FFmpeg decode.
- Converter picks the bundled binary over PATH (fake both).

## Acceptance
- "Convert to HAP" works end to end in the packaged app.
- Licenses documented; build is reproducible from the script.

## Risks and open questions
- GPL vs LGPL: keep to LGPL flags so the app license is not affected; confirm before shipping.
