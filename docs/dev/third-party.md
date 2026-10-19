# Third-party licenses in the .app

`scripts/package-macos.sh` makes the bundle GPL-compliant in three steps:

1. `scripts/bundle-dylibs.sh` copies each dylib into `Contents/Frameworks`, finds its Homebrew
   formula from the original path (`.../Cellar/<formula>/<version>/...`), copies that formula's
   LICENSE/COPYING/COPYRIGHT/NOTICE files to `Contents/Resources/ThirdParty/<formula>/` and writes
   `ThirdParty/dylibs.tsv` (dylib, formula, version). A dylib outside a Cellar or without any
   license file aborts packaging.
2. `scripts/gen-third-party.sh` writes `ThirdParty/NOTICE.md` (library, version, license id,
   homepage, source URL and sha256 from `brew info --json=v2`; the bundled FFmpeg CLI and snappy
   from the pins in `scripts/build-ffmpeg.sh`; Rust crates with their license field from
   `cargo metadata`; the GPL source offer) and copies `LICENSE` to `Resources/LICENSE`.
   Also copies each normal-dependency Rust crate's LICENSE/COPYING/NOTICE files (from
   `~/.cargo/registry`) to `ThirdParty/crates/<name>-<version>/` and Syphon's `License.txt` to
   `ThirdParty/Syphon/`. A crate without a file is accepted only if its license is made of
   well-known SPDX ids; then `LICENSE-SPDX.txt` records the id. Needs `brew`, `jq` and `cargo`
   (crate sources already fetched, e.g. by a prior build). It runs `brew info` with
   `HOMEBREW_NO_AUTO_UPDATE=1`; license and homepage come from the installed formula
   (`.brew/<pkg>.rb`) when present. It downloads no sources. A bottle with no license file gets
   `LICENSE-SPDX.txt` (id + upstream URL) instead of failing.
3. `scripts/check-third-party.sh VJCraft.app` fails unless every Frameworks dylib has a license
   dir and a NOTICE.md entry. Run it by hand on any built app.

Installed `X_N` matches the formula's stable `X` with revision `N`; otherwise the source comes
from the installed `.brew/<pkg>.rb` (sha256, or git `url` + `revision:`/`tag:`).
`scripts/test-third-party.sh` self-tests this with a fake brew.

Known gaps, left as is: review points 7 and 9 are not addressed.

Formulae whose source is a git repo (for example x264) are listed as `url @ revision` with no sha256.
Keep the hand-written notes in `THIRD_PARTY.md` in sync when dependencies change.
