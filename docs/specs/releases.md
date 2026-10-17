# Releases and installers
Issue: new · Status: draft · Estimate: 1-2 h

## Problem
There is no download. The macOS package script exists (`scripts/package-macos.sh`, ad-hoc or Developer ID) but nothing publishes it.

## Goals / non-goals
- Goal: pushing a tag `vX.Y.Z` builds and attaches macOS `.dmg`, Windows `.zip` (then `.msi`), Linux `.AppImage` to a GitHub Release with checksums.
- Goal: release notes from merged PR titles.
- Non-goal: auto-update (later), app stores.

## Design
- `.github/workflows/release.yml` on `push: tags: v*`: matrix build, package, `sha256sum`, upload with `gh release`.
- macOS: ad-hoc signed until the owner has an Apple Developer account; then `SIGN_ID` and `NOTARY_PROFILE` secrets switch to Developer ID + notarization (the script already supports both).
- Windows: unsigned at first (SmartScreen warning documented in the README).
- Version comes from the workspace `Cargo.toml`; a check fails the job if the tag and version differ.
- README: Download section with the unsigned-app instructions per OS.

## Test plan (written before the code)
- Dry run on a `v0.0.0-test` tag in a fork or with `workflow_dispatch`; download each artifact and launch it.
- Version check fails on a mismatched tag.

## Acceptance
- A `v0.1.0-beta` Release with three working downloads.

## Risks and open questions
- Owner decision: Apple Developer account (USD 99/year) for signing.
