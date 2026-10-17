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

cargo build --release -p vjc-app

rm -rf "$APP" "$DMG" "$DIST/dmg-root"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/vjcraft "$APP/Contents/MacOS/vjcraft"

# HAP-capable ffmpeg CLI for "Convert to HAP" (LGPL, static; built from pinned sources).
# build-ffmpeg.sh is a no-op when target/ffmpeg matches its stamp (version + configure), so a
# stale binary from an older configure is rebuilt. Set REQUIRE_FFMPEG=1 for release builds.
if ! scripts/build-ffmpeg.sh; then
  echo "WARNING: FFmpeg build failed; packaging without Convert to HAP support" >&2
  [ "${REQUIRE_FFMPEG:-0}" = 1 ] && exit 1
  rm -f target/ffmpeg/bin/ffmpeg
fi
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

# Sign inside-out: each dylib, then the app (no --deep). Hardened runtime needs a secure timestamp.
if [ "$SIGN_ID" = "-" ]; then
  SIGN_OPTS=(--force -s -)
else
  SIGN_OPTS=(--force --timestamp --options runtime -s "$SIGN_ID")
fi
for lib in "$APP"/Contents/Frameworks/*.dylib; do
  codesign "${SIGN_OPTS[@]}" "$lib"
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
