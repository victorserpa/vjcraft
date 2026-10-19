#!/usr/bin/env bash
# Usage: check-third-party.sh VJCraft.app
# Fails unless every dylib in Contents/Frameworks has a non-empty license dir under
# Contents/Resources/ThirdParty/, NOTICE.md and LICENSE exist, and NOTICE.md names each dylib.
set -euo pipefail
APP="${1:?usage: check-third-party.sh path/to/VJCraft.app}"
TP="$APP/Contents/Resources/ThirdParty"
bad=0
fail() { echo "check-third-party: $*" >&2; bad=1; }
[ -s "$APP/Contents/Resources/LICENSE" ] || fail "Resources/LICENSE missing"
[ -s "$TP/NOTICE.md" ] || fail "ThirdParty/NOTICE.md missing"
[ -f "$TP/dylibs.tsv" ] || { fail "dylibs.tsv missing"; exit 1; }
n=0
for f in "$APP"/Contents/Frameworks/*.dylib; do
  name="$(basename "$f")"; n=$((n+1))
  pkg="$(awk -F'\t' -v n="$name" '$1==n{print $2}' "$TP/dylibs.tsv")"
  [ -n "$pkg" ] || { fail "$name has no formula in dylibs.tsv"; continue; }
  [ -n "$(ls "$TP/$pkg" 2>/dev/null)" ] || fail "$name: no license files in ThirdParty/$pkg"
  grep -qF "$name" "$TP/NOTICE.md" || fail "$name not listed in NOTICE.md"
done
[ "$n" -gt 0 ] || fail "no dylibs in Frameworks"
[ ! -f "$APP/Contents/Resources/ffmpeg" ] || grep -q '^| FFmpeg ' "$TP/NOTICE.md" || fail "NOTICE.md lacks the FFmpeg CLI entry"
[ "$bad" -eq 0 ] || exit 1
echo "check-third-party: ok ($n dylibs)"
