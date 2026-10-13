#!/usr/bin/env bash
# Builds target/dist/VJCraft.app and VJCraft-<version>-macos.dmg (ad-hoc signed).
# Not notarized. ffmpeg stays a dynamic dependency (brew install ffmpeg).
set -euo pipefail
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

codesign --force --deep -s - "$APP"
codesign --verify --deep --strict "$APP"

mkdir -p "$DIST/dmg-root"
cp -R "$APP" "$DIST/dmg-root/"
ln -s /Applications "$DIST/dmg-root/Applications"
hdiutil create -volname "VJCraft" -srcfolder "$DIST/dmg-root" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$DIST/dmg-root"
echo "$DMG"
ls -lh "$DMG"
