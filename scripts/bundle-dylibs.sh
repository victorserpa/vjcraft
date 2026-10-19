#!/usr/bin/env bash
# Usage: bundle-dylibs.sh VJCraft.app
# Copies every non-system dylib the main binary needs (transitively) into
# Contents/Frameworks and rewrites install names to @rpath. Uses only bash,
# otool and install_name_tool (Xcode command line tools); no python3.
#
# Dependencies are resolved against the directory of the ORIGINAL library that
# names them (not the copy in Frameworks), so @loader_path/@rpath entries find
# their sibling libs. Two different libraries with the same file name abort the
# run instead of overwriting each other.
set -euo pipefail

APP="${1:?usage: bundle-dylibs.sh path/to/VJCraft.app}"
BIN="$APP/Contents/MacOS/vjcraft"
FW="$APP/Contents/Frameworks"
RPATH_FW="@executable_path/../Frameworks"
# Last-resort places for an @rpath/@loader_path name that is not next to its parent.
FALLBACK_DIRS=(/opt/homebrew/lib /opt/homebrew/opt/*/lib /usr/local/lib)
mkdir -p "$FW"

die() { echo "bundle-dylibs: $*" >&2; exit 1; }

is_system() { case "$1" in /usr/lib/*|/System/*) return 0 ;; *) return 1 ;; esac; }

# Canonical path of a file: symlinks followed, no python3 / realpath needed.
real() {
  local p="$1" t
  while [ -L "$p" ]; do
    t="$(readlink "$p")"
    case "$t" in /*) p="$t" ;; *) p="$(dirname "$p")/$t" ;; esac
  done
  echo "$(cd -P "$(dirname "$p")" && pwd)/$(basename "$p")"
}

# Non-system install names a Mach-O file links against (its own id excluded).
deps() {
  local self; self="$(otool -D "$1" 2>/dev/null | sed -n 2p)"
  otool -L "$1" | tail -n +2 | awk '{print $1}' | while read -r d; do
    is_system "$d" && continue
    [ -n "$self" ] && [ "$d" = "$self" ] && continue
    echo "$d"
  done
}

# LC_RPATH entries of $1, with @loader_path expanded to $1's own directory.
rpaths() {
  local f="$1" dir; dir="$(dirname "$f")"
  otool -l "$f" | awk '/cmd LC_RPATH/{r=1} r&&/ path /{print $2; r=0}' | while read -r r; do
    case "$r" in
      @loader_path*) echo "$dir${r#@loader_path}" ;;
      @executable_path*) echo "$(dirname "$BIN")${r#@executable_path}" ;;
      *) echo "$r" ;;
    esac
  done
}

# Real file for dependency $1 of the original library $2.
resolve() {
  local dep="$1" from="$2" base dir
  case "$dep" in
    /*) [ -f "$dep" ] || die "missing $dep (needed by $from)"; real "$dep"; return ;;
    @loader_path/*) base="${dep#*/}"; dir="$(dirname "$from")"
      [ -f "$dir/$base" ] && { real "$dir/$base"; return; } ;;
    @rpath/*|@executable_path/*) base="${dep#*/}"
      for dir in "$(dirname "$from")" $(rpaths "$from"); do
        [ -f "$dir/$base" ] && { real "$dir/$base"; return; }
      done ;;
  esac
  base="${dep##*/}"
  for dir in "${FALLBACK_DIRS[@]}"; do
    [ -f "$dir/$base" ] && { real "$dir/$base"; return; }
  done
  die "cannot resolve $dep (needed by $from)"
}

QUEUE="$(mktemp)"; MAP="$(mktemp)"
trap 'rm -f "$QUEUE" "$MAP"' EXIT
# QUEUE: original path of each file to scan. MAP: "<name>\t<original real path>".
echo "$BIN" > "$QUEUE"
: > "$MAP"

# Breadth-first: copy each lib once (by real path), enqueue its deps.
i=1
while [ "$i" -le "$(wc -l < "$QUEUE")" ]; do
  cur="$(sed -n "${i}p" "$QUEUE")"; i=$((i+1))
  while read -r dep; do
    [ -z "$dep" ] && continue
    src="$(resolve "$dep" "$cur")"
    name="$(basename "$src")"
    seen="$(awk -F'\t' -v n="$name" '$1==n{print $2}' "$MAP")"
    if [ -z "$seen" ]; then
      printf '%s\t%s\n' "$name" "$src" >> "$MAP"
      cp -L "$src" "$FW/$name"
      chmod u+w "$FW/$name"
      echo "$src" >> "$QUEUE"
    elif [ "$seen" != "$src" ]; then
      die "two different libraries are both named $name: $seen and $src"
    fi
  done < <(deps "$cur")
done

# Rewrite install names. $1 is the file to edit, $2 the original it was copied from.
fix() {
  local f="$1" orig="$2" dep src
  while read -r dep; do
    [ -z "$dep" ] && continue
    src="$(resolve "$dep" "$orig")"
    install_name_tool -change "$dep" "@rpath/$(basename "$src")" "$f"
  done < <(deps "$orig")
}

while IFS=$'\t' read -r name orig; do
  install_name_tool -id "@rpath/$name" "$FW/$name"
  fix "$FW/$name" "$orig"
done < "$MAP"
fix "$BIN" "$BIN"

# Let the binary find Frameworks; a failure here is a failure of the bundle.
if ! otool -l "$BIN" | grep -qF "path $RPATH_FW "; then
  install_name_tool -add_rpath "$RPATH_FW" "$BIN" || die "add_rpath failed on $BIN"
fi

# Verify: every non-system reference is an @rpath name that exists in Frameworks.
bad=0
for f in "$BIN" "$FW"/*.dylib; do
  while read -r d; do
    [ -z "$d" ] && continue
    case "$d" in
      @rpath/*) [ -f "$FW/${d#@rpath/}" ] || { echo "bundle-dylibs: $f needs $d, not bundled" >&2; bad=1; } ;;
      *) echo "bundle-dylibs: $f still references $d" >&2; bad=1 ;;
    esac
  done < <(deps "$f")
done
[ "$bad" -eq 0 ] || die "leftover external paths"
echo "bundled $(ls "$FW" | wc -l | tr -d ' ') dylibs into $FW"

# Licenses: for every bundled dylib find its Homebrew formula (the original lives in
# <prefix>/Cellar/<formula>/<version>/...), copy that formula's license files into
# Contents/Resources/ThirdParty/<formula>/ and record "<dylib>\t<formula>\t<version>" in
# ThirdParty/dylibs.tsv (read by scripts/gen-third-party.sh). A dylib without a formula or
# without any license file aborts packaging.
TP="$APP/Contents/Resources/ThirdParty"
mkdir -p "$TP"
: > "$TP/dylibs.tsv"
while IFS=$'\t' read -r name orig; do
  # /opt/homebrew/opt/<pkg> is a symlink into the Cellar; resolve directories fully.
  orig="$(cd -P "$(dirname "$orig")" && pwd)/$(basename "$orig")"
  case "$orig" in
    */target/ffmpeg-libs/lib/*)
      # Our own LGPL FFmpeg build (scripts/build-ffmpeg.sh); license files from its source.
      pkg=ffmpeg-vjc; ver="$(sed -n 's/^FFMPEG_VER=//p' "$(dirname "$0")/build-ffmpeg.sh" | head -1 | tr -d "\"'")"
      mkdir -p "$TP/$pkg"
      for lf in COPYING.LGPLv2.1 LICENSE.md; do
        src_lf="${orig%%/target/ffmpeg-libs/*}/target/ffmpeg-src/ffmpeg/$lf"
        [ -f "$src_lf" ] && cp -L "$src_lf" "$TP/$pkg/$lf"
      done
      [ -f "$TP/$pkg/COPYING.LGPLv2.1" ] || die "no FFmpeg license file for $name"
      printf '%s\t%s\t%s\n' "$name" "$pkg" "$ver" >> "$TP/dylibs.tsv"
      continue ;;
    */target/ffmpeg-src/prefix/lib/*)
      # Our own libvpx / dav1d (scripts/build-ffmpeg.sh); licenses were copied to share/licenses.
      case "$name" in libdav1d*) pkg=dav1d ;; libvpx*) pkg=libvpx ;; *) die "unexpected $name in the deps prefix" ;; esac
      ver="$(sed -n "s/^$(echo "$pkg" | tr a-z A-Z)_VER=//p" "$(dirname "$0")/build-ffmpeg.sh" | head -1 | tr -d "\"'")"
      [ -n "$ver" ] || die "no version for $pkg in build-ffmpeg.sh"
      mkdir -p "$TP/$pkg"
      cp -L "${orig%/lib/*}/share/licenses/$pkg/"* "$TP/$pkg/" || die "no license files for $pkg"
      printf '%s\t%s\t%s\n' "$name" "$pkg" "$ver" >> "$TP/dylibs.tsv"
      continue ;;
    */Cellar/*) rest="${orig#*/Cellar/}"; pkg="${rest%%/*}"; rest="${rest#*/}"; ver="${rest%%/*}"
      root="${orig%%/Cellar/*}/Cellar/$pkg/$ver" ;;
    *) die "$name ($orig) is not from a Homebrew Cellar; cannot find its license" ;;
  esac
  mkdir -p "$TP/$pkg"
  n=0
  while IFS= read -r lf; do
    [ -z "$lf" ] && continue
    dest="$TP/$pkg/$(echo "${lf#$root/}" | tr '/' '_')"
    [ -e "$dest" ] || { cp -L "$lf" "$dest"; n=$((n+1)); }
  done < <(find "$root" -maxdepth 4 -type f \( -iname 'LICEN[SC]E*' -o -iname 'COPYING*' -o -iname 'COPYRIGHT*' -o -iname 'NOTICE*' -o -iname 'UNLICENSE*' \) \
             -not -path '*/include/*' -not -path '*/lib/python*' 2>/dev/null | sort)
  if [ -z "$(ls "$TP/$pkg")" ]; then
    # Bottle ships no license file: record the SPDX id and upstream URL from the formula.
    . "$(dirname "$0")/third-party-lib.sh"
    j="$("$BREW" info --json=v2 "$pkg")" || die "no license file for $pkg $ver and brew info failed"
    IFS=$'\t' read -r lic home < <(formula_meta "$pkg" "$ver" "$j") || die "no license id for $pkg"
    printf '%s\n%s\nHomebrew bottle %s %s ships no license file; this is the license id declared by its formula.\n' \
      "$lic" "$home" "$pkg" "$ver" > "$TP/$pkg/LICENSE-SPDX.txt"
    echo "bundle-dylibs: $pkg $ver ships no license file, wrote LICENSE-SPDX.txt ($lic)"
  fi
  printf '%s\t%s\t%s\n' "$name" "$pkg" "$ver" >> "$TP/dylibs.tsv"
done < "$MAP"
echo "licenses for $(cut -f2 "$TP/dylibs.tsv" | sort -u | wc -l | tr -d ' ') formulae in $TP"
