#!/usr/bin/env bash
# Builds target/dist/VJCraft.app and VJCraft-<version>-macos.dmg, self-contained
# (non-system dylibs bundled by scripts/bundle-dylibs.sh), signed with Developer ID
# + hardened runtime, notarized and stapled.
# Env (both have machine-specific defaults, set your own elsewhere):
#   SIGN_ID         codesign identity, e.g. "Developer ID Application: Name (TEAMID)";
#                   list yours with `security find-identity -v -p codesigning`.
#   NOTARY_PROFILE  notarytool keychain profile, created once with
#                   `xcrun notarytool store-credentials <name>`.
# Flags: --no-notarize (local build, still signed).
# --adhoc: sign ad hoc (no Developer ID), implies --no-notarize.
set -euo pipefail
SIGN_ID="${SIGN_ID:-Developer ID Application: SCHMUCKER DESENVOLVIMENTOS E SONORIZACAO LTDA (LHUJ4D96V2)}"
NOTARY_PROFILE="${NOTARY_PROFILE:-notchwake}"
NOTARIZE=1
for a in "$@"; do
  case "$a" in
    --no-notarize) NOTARIZE=0 ;;
    --adhoc) NOTARIZE=0; SIGN_ID="-" ;;
    *) echo "unknown flag $a" >&2; exit 2 ;;
  esac
done
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DIST=target/dist
APP="$DIST/VJCraft.app"
DMG="$DIST/VJCraft-$VERSION-macos.dmg"

# Our own LGPL FFmpeg (shared libs for playback + HAP CLI), built from pinned sources.
# The vjc-app build links these libs (ffmpeg-sys uses pkg-config), not Homebrew's GPL FFmpeg.
scripts/build-ffmpeg.sh || { echo "FFmpeg build failed" >&2; exit 1; }
FFMPEG_PC="$PWD/target/ffmpeg-libs/lib/pkgconfig"
FFMPEG_PC="$FFMPEG_PC:$PWD/target/ffmpeg-src/prefix/lib/pkgconfig"  # our dav1d + libvpx
# Clean the ffmpeg-sys artifacts so a build against Homebrew's FFmpeg is never reused.
cargo clean --release -p ffmpeg-sys-next
PKG_CONFIG_PATH="$FFMPEG_PC" cargo build --release -p vjc-app
# Plugin hosts run each plugin in a helper process found next to the app binary.
cargo build --release -p vjc-clap -p vjc-ffgl --bins

rm -rf "$APP" "$DMG" "$DIST/dmg-root"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "${CARGO_TARGET_DIR:-target}/release/vjcraft" "$APP/Contents/MacOS/vjcraft"
HELPERS=(vjc-clap-helper vjc-ffgl-helper)
for h in "${HELPERS[@]}"; do
  cp "${CARGO_TARGET_DIR:-target}/release/$h" "$APP/Contents/MacOS/$h" \
    || { echo "missing plugin helper $h" >&2; exit 1; }
done

# HAP-capable ffmpeg CLI for "Convert to HAP" (LGPL, static), built above.
if [ -x target/ffmpeg/bin/ffmpeg ]; then
  cp target/ffmpeg/bin/ffmpeg "$APP/Contents/Resources/ffmpeg"
else
  echo "WARNING: no target/ffmpeg/bin/ffmpeg; app has no bundled ffmpeg" >&2
fi

# Icon: use assets/icon/VJCraft.icns if present, else a violet placeholder.
if [ -f assets/icon/VJCraft.icns ]; then
  cp assets/icon/VJCraft.icns "$APP/Contents/Resources/VJCraft.icns"
else
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  python3 - "$TMP/icon.png" <<'PY'
import struct, sys, zlib
n = 1024
row = b"\x00" + bytes([0xA9, 0x8B, 0xFF]) * n
raw = row * n
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 2, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
PY
  SET="$TMP/VJCraft.iconset"
  mkdir "$SET"
  for s in 16 32 128 256 512; do
    sips -z $s $s "$TMP/icon.png" --out "$SET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s*2)) $((s*2)) "$TMP/icon.png" --out "$SET/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$SET" -o "$APP/Contents/Resources/VJCraft.icns"
  rm -rf "$TMP"
fi

cat > "$APP/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>VJCraft</string>
  <key>CFBundleDisplayName</key><string>VJCraft</string>
  <key>CFBundleIdentifier</key><string>org.vjcraft.app</string>
  <key>CFBundleExecutable</key><string>vjcraft</string>
  <key>CFBundleIconFile</key><string>VJCraft</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key><string>VJCraft uses the local network for NDI video, Ableton Link tempo sync, OSC and Art-Net.</string>
  <key>NSBonjourServices</key><array><string>_ndi._tcp</string></array>
  <key>NSCameraUsageDescription</key><string>VJCraft can use a camera as a video input.</string>
  <key>NSMicrophoneUsageDescription</key><string>VJCraft can use an audio input to drive effects.</string>
</dict>
</plist>
PL

scripts/bundle-dylibs.sh "$APP"

# Every Mach-O in the bundle must run on macOS 12.0 (minos <= 12.0).
min_ok=1
macho_list=$(find "$APP/Contents" -type f \( -perm -u+x -o -name '*.dylib' \)) \
  || { echo "minos check: find failed in $APP/Contents" >&2; exit 1; }
[ -n "$macho_list" ] || { echo "minos check: no files in $APP/Contents" >&2; exit 1; }
while IFS= read -r f; do
  file -b "$f" | grep -q 'Mach-O' || continue
  # LC_BUILD_VERSION prints "minos X"; legacy LC_VERSION_MIN_* prints "version X" in its block.
  mv_list=$(vtool -show-build "$f" | awk '$1=="minos"{print $2} $1=="cmd"{legacy=($2 ~ /^LC_VERSION_MIN/)} legacy&&$1=="version"{print $2}')
  [ -n "$mv_list" ] || { echo "minos check: no build version in $f" >&2; min_ok=0; continue; }
  for mv in $mv_list; do
    awk -v v="$mv" 'BEGIN{split(v,a,".");exit !((a[1]+0)*10000+(a[2]+0)*100+(a[3]+0) <= 120000)}' \
      || { echo "minos check: $f has minos $mv (> 12.0)" >&2; min_ok=0; }
  done
done <<< "$macho_list"
[ "$min_ok" -eq 1 ] || { echo "bundle needs a newer macOS than 12.0" >&2; exit 1; }

# Link audit: the binary must use our libav* via @rpath, and no GPL/network/Homebrew-FFmpeg libs.
EXE_LIBS=$(otool -L "$APP/Contents/MacOS/vjcraft" | tail -n +2 | awk '{print $1}')
echo "$EXE_LIBS" | grep -q '^@rpath/libavcodec' || { echo "vjcraft does not link libav* via @rpath" >&2; exit 1; }
if echo "$EXE_LIBS" | grep -E 'libav|libsw' | grep -v '^@rpath/' | grep -q .; then
  echo "vjcraft links libav*/libsw* by a non-@rpath path" >&2; exit 1
fi
if { echo "$EXE_LIBS"; otool -L "$APP"/Contents/Frameworks/*.dylib | awk '{print $1}'; ls "$APP/Contents/Frameworks"; } \
  | grep -Eq 'libx264|libx265|libssl|libcrypto|/opt/homebrew/opt/ffmpeg'; then
  echo "forbidden library (x264/x265/ssl/crypto/Homebrew ffmpeg) in the bundle" >&2; exit 1
fi
scripts/gen-third-party.sh "$APP"
scripts/check-third-party.sh "$APP"

# Sign inside-out: each dylib, then the app (no --deep). Hardened runtime needs a secure timestamp.
if [ "$SIGN_ID" = "-" ]; then
  SIGN_OPTS=(--force -s -)
else
  SIGN_OPTS=(--force --timestamp --options runtime -s "$SIGN_ID")
fi
for lib in "$APP"/Contents/Frameworks/*.dylib; do
  codesign "${SIGN_OPTS[@]}" "$lib"
done
# Helpers load third-party plugins: same entitlements as the app (library validation off).
for h in "${HELPERS[@]}"; do
  codesign "${SIGN_OPTS[@]}" --entitlements scripts/entitlements.plist "$APP/Contents/MacOS/$h"
done
if [ -f "$APP/Contents/Resources/ffmpeg" ]; then
  codesign "${SIGN_OPTS[@]}" --entitlements scripts/entitlements.plist "$APP/Contents/Resources/ffmpeg"
fi
codesign "${SIGN_OPTS[@]}" --entitlements scripts/entitlements.plist "$APP"
codesign --verify --deep --strict "$APP"

mkdir -p "$DIST/dmg-root"
cp -R "$APP" "$DIST/dmg-root/"
ln -s /Applications "$DIST/dmg-root/Applications"
hdiutil create -volname "VJCraft" -srcfolder "$DIST/dmg-root" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$DIST/dmg-root"
if [ "$SIGN_ID" != "-" ]; then codesign --force --timestamp -s "$SIGN_ID" "$DMG"; fi

if [ "$NOTARIZE" -eq 1 ]; then
  xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG"
  spctl -a -vv -t install "$DMG"
fi
echo "$DMG"
ls -lh "$DMG"
