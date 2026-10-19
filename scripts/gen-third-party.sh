#!/usr/bin/env bash
# Usage: gen-third-party.sh VJCraft.app
# Writes Contents/Resources/ThirdParty/NOTICE.md and copies LICENSE (GPL-3.0) to
# Contents/Resources/LICENSE. Needs the dylibs.tsv left by scripts/bundle-dylibs.sh, plus
# brew, jq and cargo. See docs/dev/third-party.md.
set -euo pipefail
APP="${1:?usage: gen-third-party.sh path/to/VJCraft.app}"
APP="$(cd "$APP" && pwd)" || { echo "gen-third-party: no such app $APP" >&2; exit 1; }
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
die() { echo "gen-third-party: $*" >&2; exit 1; }
. scripts/third-party-lib.sh

RES="$APP/Contents/Resources"
TP="$RES/ThirdParty"
TSV="$TP/dylibs.tsv"
[ -f "$TSV" ] || die "$TSV missing; run scripts/bundle-dylibs.sh first"
cp LICENSE "$RES/LICENSE"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT="$TP/NOTICE.md"

# Normal (non-dev, non-build) dependencies reachable from vjc-app on macOS, external only.
CRATES="$(mktemp)"; trap 'rm -f "$CRATES"' EXIT
cargo metadata --format-version 1 --locked --filter-platform aarch64-apple-darwin 2>/dev/null \
 | jq '. as $m
   | ($m.resolve.nodes | map({key: .id, value: .}) | from_entries) as $n
   | ($m.packages[] | select(.name=="vjc-app") | .id) as $root
   | def reach($seen):
       ($seen + [$seen[] | $n[.].deps[] | select(any(.dep_kinds[]; .kind == null)) | .pkg] | unique) as $nx
       | if ($nx|length) == ($seen|length) then $seen else reach($nx) end;
     reach([$root]) as $ids
   | [$m.packages[] | select(.id as $i | $ids|index($i)) | select(.source != null)
      | {name, version, license, dir: (.manifest_path | sub("/Cargo.toml$"; ""))}]
   | sort_by(.name, .version)' > "$CRATES" || die "cargo metadata failed"
[ "$(jq length "$CRATES")" -gt 0 ] || die "no crates found from vjc-app"

# License texts: Syphon, and each crate's LICENSE/COPYING/NOTICE files from its source dir.
mkdir -p "$TP/Syphon"; cp vendor/syphon/License.txt "$TP/Syphon/License.txt"
rm -rf "$TP/crates"; mkdir -p "$TP/crates"
WELL_KNOWN=" MIT Apache-2.0 BSD-2-Clause BSD-3-Clause ISC Zlib Unlicense 0BSD MPL-2.0 CC0-1.0 BSL-1.0 WTFPL OFL-1.1 Ubuntu-font-1.0 "
ncrate=0; nspdx=0; bad=0
while IFS=$'\t' read -r name ver lic dir; do
  dest="$TP/crates/$name-$ver"; mkdir -p "$dest"
  find "$dir" -maxdepth 1 -type f \( -iname 'LICEN[SC]E*' -o -iname 'COPYING*' -o -iname 'NOTICE*' \) -exec cp {} "$dest/" \;
  if [ -z "$(ls "$dest")" ]; then
    # Well-known SPDX ids, alone or combined with OR/AND (dual-licensed crates published
    # without the text, e.g. egui, objc2-*). Anything else must ship a license file.
    ids="$(echo "$lic" | tr -d '()' | sed 's/ OR / /g; s/ AND / /g')"
    ok=1; [ -n "$ids" ] || ok=0
    for id in $ids; do case "$WELL_KNOWN" in *" $id "*) ;; *) ok=0 ;; esac; done
    if [ "$ok" -eq 0 ]; then
      echo "gen-third-party: $name $ver: no license file and \"$lic\" is not made of well-known SPDX ids" >&2; bad=1; continue
    fi
    printf '%s\nThe crate %s %s ships no license file; its Cargo.toml declares this SPDX id.\n' "$lic" "$name" "$ver" > "$dest/LICENSE-SPDX.txt"
    nspdx=$((nspdx+1))
  fi
  ncrate=$((ncrate+1))
done < <(jq -r '.[] | [.name, .version, (.license // ""), .dir] | @tsv' "$CRATES")
[ "$bad" -eq 0 ] || die "crates above ship no license text"
echo "crate licenses: $ncrate crates ($nspdx with SPDX id only)"

{
cat <<HDR
# VJCraft $VERSION: third-party notices

VJCraft is GPL-3.0-only (see \`LICENSE\` in this folder's parent). This file lists what the
app bundle ships. Full license texts are in \`ThirdParty/<library>/\`.

## Source offer (GPL)

Parts of this bundle (for example Ableton Link via rusty_link and VJCraft itself) are under
the GPL. The complete corresponding source code for them is available at the source URLs
below (checksums given, so you can verify the tarballs) and the source for VJCraft itself is
available from the project repository, https://github.com/victorserpa/vjcraft. We will also
provide it on request for at least three years after the release of this version.

## Bundled libraries (Contents/Frameworks, from Homebrew)

| library | version | license | homepage | source | sha256 |
| --- | --- | --- | --- | --- | --- |
HDR
cut -f2,3 "$TSV" | sort -u | grep -v '^ffmpeg-vjc	' | while IFS=$'\t' read -r pkg ver; do brew_row "$pkg" "$ver"; done

cat <<HDR

Dylibs and the formula each comes from:

$(sort "$TSV" | awk -F'\t' '{printf "- %s (%s %s)\n", $1, $2, $3}')
HDR

if [ -f "$RES/ffmpeg" ] || grep -q '^[^	]*	ffmpeg-vjc	' "$TSV"; then
cat <<HDR

## FFmpeg (libav* in Contents/Frameworks, CLI Contents/Resources/ffmpeg for HAP conversion)

Built by \`scripts/build-ffmpeg.sh\` from pinned official sources, LGPL (no --enable-gpl,
--enable-version3 or --enable-nonfree), statically linked with snappy. The libav* dylibs
(ffmpeg-vjc in the dylib list above) use the Homebrew dav1d and libvpx listed in the table above
(decoders only); there is no x264, x265, OpenSSL or network protocol.

| library | version | license | homepage | source | sha256 |
| --- | --- | --- | --- | --- | --- |
HDR
fv() {
  local v; v="$(sed -n "s/^$1=//p" scripts/build-ffmpeg.sh | head -1 | tr -d "\"'")"
  [ -n "$v" ] || die "$1 not found in scripts/build-ffmpeg.sh"
  echo "$v"
}
FV=$(fv FFMPEG_VER); SV=$(fv SNAPPY_VER); FSHA=$(fv FFMPEG_SHA256); SSHA=$(fv SNAPPY_SHA256)
echo "| FFmpeg | $FV | LGPL-2.1-or-later | https://ffmpeg.org/ | https://ffmpeg.org/releases/ffmpeg-$FV.tar.xz | $FSHA |"
echo "| snappy | $SV | BSD-3-Clause | https://github.com/google/snappy | https://github.com/google/snappy/archive/refs/tags/$SV.tar.gz | $SSHA |"
echo
echo "The build recipe is \`scripts/build-ffmpeg.sh\` in the project repository."
fi

cat <<HDR

## Vendored C code

- Syphon Framework (BSD-2-Clause), \`ThirdParty/Syphon/License.txt\`.
- Ableton Link via rusty_link (GPL-2.0-or-later), listed with the Rust crates below.
- NDI is not bundled (loaded at runtime from the user's NDI install).

## Rust crates (from \`cargo metadata\`, normal dependencies of vjc-app on macOS; texts in \`ThirdParty/crates/\`)

| crate | version | license |
| --- | --- | --- |
HDR
jq -r '.[] | "| \(.name) | \(.version) | \(.license // "see license file") |"' "$CRATES"
} > "$OUT"
grep -q '| UNKNOWN |' "$OUT" && echo "gen-third-party: WARNING some entries have an unknown license, see $OUT" >&2
echo "wrote $OUT"
