#!/usr/bin/env bash
# Usage: gen-third-party.sh VJCraft.app
# Writes Contents/Resources/ThirdParty/NOTICE.md and copies LICENSE (GPL-3.0) to
# Contents/Resources/LICENSE. Needs the dylibs.tsv left by scripts/bundle-dylibs.sh, plus
# brew, jq and cargo. See docs/dev/third-party.md.
set -euo pipefail
APP="${1:?usage: gen-third-party.sh path/to/VJCraft.app}"
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
die() { echo "gen-third-party: $*" >&2; exit 1; }

RES="$APP/Contents/Resources"
TP="$RES/ThirdParty"
TSV="$TP/dylibs.tsv"
[ -f "$TSV" ] || die "$TSV missing; run scripts/bundle-dylibs.sh first"
cp LICENSE "$RES/LICENSE"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT="$TP/NOTICE.md"

# One table row for a Homebrew formula: library | version | license | homepage | source | sha256.
brew_row() {
  local pkg="$1" ver="$2" j rb url sha
  j="$(brew info --json=v2 "$pkg")" || die "brew info $pkg failed"
  echo "$j" | jq -e '.formulae[0].license' >/dev/null || die "brew has no license id for $pkg"
  if [ "$(echo "$j" | jq -r '.formulae[0].versions.stable')" = "$ver" ]; then
    echo "$j" | jq -r --arg v "$ver" '.formulae[0] |
      (.urls.stable) as $u |
      "| \(.name) | \($v) | \(.license // "UNKNOWN") | \(.homepage) | \(if $u.revision then "\($u.url) @ \($u.revision)" else $u.url end) | \($u.checksum // "git revision") |"'
    return
  fi
  # The installed version differs from the formula's current stable one, so the source
  # URL must come from the formula copy Homebrew kept with that installed version.
  rb="$(brew --cellar "$pkg")/$ver/.brew/$pkg.rb"
  [ -f "$rb" ] || die "$pkg $ver: no installed formula at $rb to find its source"
  url=$(sed -nE 's/^  url "([^"]+)".*/\1/p' "$rb" | head -1)
  sha=$(sed -nE 's/^  sha256 "([0-9a-f]{64})".*/\1/p' "$rb" | head -1)
  [ -n "$url" ] && [ -n "$sha" ] || die "$pkg $ver: no url/sha256 in $rb"
  echo "$j" | jq -r --arg v "$ver" --arg u "$url" --arg s "$sha" '.formulae[0] |
    "| \(.name) | \($v) | \(.license // "UNKNOWN") | \(.homepage) | \($u) | \($s) |"'
}

{
cat <<HDR
# VJCraft $VERSION: third-party notices

VJCraft is GPL-3.0-only (see \`LICENSE\` in this folder's parent). This file lists what the
app bundle ships. Full license texts are in \`ThirdParty/<library>/\`.

## Source offer (GPL)

Parts of this bundle (for example FFmpeg built with --enable-gpl, x264 and x265) are under
the GPL. The complete corresponding source code for them is available at the source URLs
below (checksums given, so you can verify the tarballs) and the source for VJCraft itself is
available from the project repository, https://github.com/victorserpa/vjcraft. We will also
provide it on request for at least three years after the release of this version.

## Bundled libraries (Contents/Frameworks, from Homebrew)

| library | version | license | homepage | source | sha256 |
| --- | --- | --- | --- | --- | --- |
HDR
cut -f2,3 "$TSV" | sort -u | while IFS=$'\t' read -r pkg ver; do brew_row "$pkg" "$ver"; done

cat <<HDR

Dylibs and the formula each comes from:

$(sort "$TSV" | awk -F'\t' '{printf "- %s (%s %s)\n", $1, $2, $3}')
HDR

if [ -f "$RES/ffmpeg" ]; then
cat <<HDR

## FFmpeg CLI (Contents/Resources/ffmpeg, HAP conversion)

Built by \`scripts/build-ffmpeg.sh\` from pinned official sources, LGPL (no --enable-gpl,
--enable-version3 or --enable-nonfree), statically linked with snappy.

| library | version | license | homepage | source | sha256 |
| --- | --- | --- | --- | --- | --- |
HDR
fv() { sed -n "s/^$1=//p" scripts/build-ffmpeg.sh | head -1; }
FV=$(fv FFMPEG_VER); SV=$(fv SNAPPY_VER)
echo "| FFmpeg | $FV | LGPL-2.1-or-later | https://ffmpeg.org/ | https://ffmpeg.org/releases/ffmpeg-$FV.tar.xz | $(fv FFMPEG_SHA256) |"
echo "| snappy | $SV | BSD-3-Clause | https://github.com/google/snappy | https://github.com/google/snappy/archive/refs/tags/$SV.tar.gz | $(fv SNAPPY_SHA256) |"
echo
echo "The build recipe is \`scripts/build-ffmpeg.sh\` in the project repository."
fi

cat <<HDR

## Vendored C code

- Syphon Framework (BSD-2-Clause), \`vendor/syphon/License.txt\`.
- Ableton Link via rusty_link (GPL-2.0-or-later), listed with the Rust crates below.
- NDI is not bundled (loaded at runtime from the user's NDI install).

## Rust crates (from \`cargo metadata\`, normal dependencies of vjc-app on macOS)

| crate | version | license |
| --- | --- | --- |
HDR
cargo metadata --format-version 1 --locked --filter-platform aarch64-apple-darwin 2>/dev/null \
 | jq -r '. as $m | [$m.resolve.nodes[] | select(.id as $i | ($m.packages[]|select(.id==$i)|.source) != null) | .id] as $ext
   | [$m.packages[] | select(.id as $i | $ext|index($i))] | sort_by(.name,.version)[]
   | "| \(.name) | \(.version) | \(.license // .license_file // "UNKNOWN") |"' \
 || die "cargo metadata failed"
} > "$OUT"
grep -q '| UNKNOWN |' "$OUT" && echo "gen-third-party: WARNING some entries have an unknown license, see $OUT" >&2
echo "wrote $OUT"
