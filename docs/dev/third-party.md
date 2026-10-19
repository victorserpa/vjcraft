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
   Needs `brew`, `jq` and `cargo`; downloads nothing.
3. `scripts/check-third-party.sh VJCraft.app` fails unless every Frameworks dylib has a license
   dir and a NOTICE.md entry. Run it by hand on any built app.

Formulae whose source is a git repo (for example x264) are listed as `url @ revision` with no sha256.
Keep the hand-written notes in `THIRD_PARTY.md` in sync when dependencies change.
