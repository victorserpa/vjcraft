#!/usr/bin/env bash
# Usage: bundle-dylibs.sh VJCraft.app
# Copies every non-system dylib the main binary needs (transitively) into
# Contents/Frameworks and rewrites install names to @rpath. Uses only
# otool / install_name_tool, which ship with the Xcode command line tools.
set -euo pipefail

APP="${1:?usage: bundle-dylibs.sh path/to/VJCraft.app}"
BIN="$APP/Contents/MacOS/vjcraft"
FW="$APP/Contents/Frameworks"
mkdir -p "$FW"

is_system() { case "$1" in /usr/lib/*|/System/*) return 0 ;; *) return 1 ;; esac; }

# Non-system dependency paths of a Mach-O file.
deps() { otool -L "$1" | tail -n +2 | awk '{print $1}' | while read -r d; do is_system "$d" || echo "$d"; done; }

# Resolve a dependency to a real file: absolute, or @rpath/@loader_path relative to $2's dir.
resolve() {
  local dep="$1" from="$2" base
  case "$dep" in
    /*) echo "$dep" ;;
    @rpath/*|@loader_path/*|@executable_path/*)
      base="${dep#*/}"
      for dir in "$(dirname "$from")" /opt/homebrew/lib /opt/homebrew/opt/*/lib /usr/local/lib; do
        [ -f "$dir/$base" ] && { echo "$dir/$base"; return; }
      done
      echo "bundle-dylibs: cannot resolve $dep (from $from)" >&2; return 1 ;;
  esac
}

QUEUE="$(mktemp)"; SEEN="$(mktemp)"
trap 'rm -f "$QUEUE" "$SEEN"' EXIT
echo "$BIN" > "$QUEUE"
: > "$SEEN"

# Breadth-first: copy each lib once (by real path), enqueue its deps.
i=1
while [ "$i" -le "$(wc -l < "$QUEUE")" ]; do
  cur="$(sed -n "${i}p" "$QUEUE")"; i=$((i+1))
  while read -r dep; do
    [ -z "$dep" ] && continue
    src="$(resolve "$dep" "$cur")"
    real="$(cd "$(dirname "$src")" && echo "$PWD/$(basename "$src")")"
    real="$(python3 -c 'import os,sys;print(os.path.realpath(sys.argv[1]))' "$real")"
    name="$(basename "$real")"
    if ! grep -qxF "$real" "$SEEN"; then
      echo "$real" >> "$SEEN"
      cp -L "$real" "$FW/$name"
      chmod u+w "$FW/$name"
      echo "$FW/$name" >> "$QUEUE"
    fi
  done < <(deps "$cur")
done

# Rewrite install names.
fix() {
  local f="$1"
  while read -r dep; do
    [ -z "$dep" ] && continue
    src="$(resolve "$dep" "$f" 2>/dev/null || true)"
    # Map any alias (e.g. libavcodec.62.dylib symlink) to the copied real name.
    name="$(basename "$(python3 -c 'import os,sys;print(os.path.realpath(sys.argv[1]))' "${src:-$dep}")")"
    install_name_tool -change "$dep" "@rpath/$name" "$f"
  done < <(deps "$f")
}

for lib in "$FW"/*.dylib; do
  install_name_tool -id "@rpath/$(basename "$lib")" "$lib"
  fix "$lib"
done
fix "$BIN"
install_name_tool -add_rpath "@executable_path/../Frameworks" "$BIN" 2>/dev/null || true

# Verify: nothing outside the system may remain.
bad=0
for f in "$BIN" "$FW"/*.dylib; do
  if otool -L "$f" | tail -n +2 | grep -E '/opt/homebrew|/usr/local' ; then bad=1; fi
done
[ "$bad" -eq 0 ] || { echo "bundle-dylibs: leftover external paths" >&2; exit 1; }
echo "bundled $(ls "$FW" | wc -l | tr -d ' ') dylibs into $FW"
