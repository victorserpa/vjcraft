#!/usr/bin/env bash
# Builds a small, LGPL FFmpeg CLI with the HAP encoder (needs libsnappy) into
# target/ffmpeg/bin/ffmpeg. Used by "Convert to HAP"; playback keeps the linked libav*.
#
# Network: exactly two downloads, both pinned and SHA-256 checked, nothing else.
#   FFmpeg 9.0.2  https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz   (official release tarball)
#   snappy 1.3.1  https://github.com/google/snappy/archive/refs/tags/1.3.1.tar.gz
# Checksum source: ffmpeg.org publishes only .asc signatures for current releases (no gpg
# on the build machine), so both SHA-256 values are the ones in the Homebrew formulas
# (Formula/f/ffmpeg.rb, Formula/s/snappy.rb), an independent source; the download is
# refused if it does not match. Re-check and bump them together when changing a version.
# Licenses: FFmpeg configured without --enable-gpl / --enable-version3 / --enable-nonfree
# (LGPL-2.1+); snappy is BSD-3-Clause. The result is checked at the end.
# Source goes to target/ffmpeg-src/{dl,ffmpeg,snappy} (each its own empty dir); only the
# trees' own cmake / configure / make are run.
# Flags: --force rebuild even when up to date. Env: JOBS (default 4).
set -euo pipefail

FFMPEG_VER=9.0.2
FFMPEG_URL="https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VER.tar.xz"
FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
SNAPPY_VER=1.3.1
SNAPPY_URL="https://github.com/google/snappy/archive/refs/tags/$SNAPPY_VER.tar.gz"
SNAPPY_SHA256=893f708a0bf4b5529d555ffcee390e940e932fcf90261f682604475a76cd0247
MIN_MACOS=12.0
JOBS="${JOBS:-4}"

FORCE=0
for a in "$@"; do
  case "$a" in
    --force) FORCE=1 ;;
    *) echo "unknown flag $a" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."
ROOT=$PWD
# Always <repo>/target (tests and packaging look there), even when CARGO_TARGET_DIR is set.
OUT_BASE="$ROOT/target"
SRC="$OUT_BASE/ffmpeg-src"
OUT="$OUT_BASE/ffmpeg"
DEPS="$SRC/prefix"
STAMP="$FFMPEG_VER+$SNAPPY_VER+$(shasum -a 256 "$0" | cut -c1-16)"

if [ "$FORCE" -eq 0 ] && [ -x "$OUT/bin/ffmpeg" ] && [ "$(cat "$OUT/stamp" 2>/dev/null)" = "$STAMP" ]; then
  echo "up to date: $OUT/bin/ffmpeg"
  exit 0
fi

fetch() { # url sha256 file
  if [ ! -f "$3" ]; then
    curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$3.tmp"
    mv "$3.tmp" "$3"
  fi
  local got
  got=$(shasum -a 256 "$3" | cut -d' ' -f1)
  if [ "$got" != "$2" ]; then
    echo "SHA-256 mismatch for $3: got $got, want $2" >&2
    rm -f "$3"
    exit 1
  fi
}

unpack() { # tarball dir
  rm -rf "$2"; mkdir -p "$2"
  if tar -tf "$1" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then
    echo "unsafe path in $1" >&2; exit 1
  fi
  tar -xf "$1" -C "$2" --strip-components=1
}

mkdir -p "$SRC/dl"
fetch "$SNAPPY_URL" "$SNAPPY_SHA256" "$SRC/dl/snappy-$SNAPPY_VER.tar.gz"
fetch "$FFMPEG_URL" "$FFMPEG_SHA256" "$SRC/dl/ffmpeg-$FFMPEG_VER.tar.xz"
unpack "$SRC/dl/snappy-$SNAPPY_VER.tar.gz" "$SRC/snappy"
unpack "$SRC/dl/ffmpeg-$FFMPEG_VER.tar.xz" "$SRC/ffmpeg"

# Be gentle with a busy machine: wait for the 1-minute load to drop below 4 (max 20 min).
for _ in $(seq 40); do
  load=$(sysctl -n vm.loadavg | awk '{print $2}')
  awk -v l="$load" 'BEGIN{exit !(l <= 4)}' && break
  echo "load $load > 4, waiting 30 s"; sleep 30
done

rm -rf "$DEPS" "$SRC/snappy-build"
cmake -S "$SRC/snappy" -B "$SRC/snappy-build" -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=OFF -DSNAPPY_BUILD_TESTS=OFF -DSNAPPY_BUILD_BENCHMARKS=OFF \
  -DCMAKE_INSTALL_PREFIX="$DEPS" -DCMAKE_OSX_DEPLOYMENT_TARGET="$MIN_MACOS" \
  -DCMAKE_POLICY_VERSION_MINIMUM=3.5 >/dev/null
cmake --build "$SRC/snappy-build" -j "$JOBS" >/dev/null
cmake --install "$SRC/snappy-build" >/dev/null

rm -rf "$SRC/ffmpeg-build" "$OUT"
mkdir -p "$SRC/ffmpeg-build"
START=$(date +%s)
# Minimal on purpose: conversion needs a few decoders, the HAP encoder, PCM, the mov muxer.
# No --enable-gpl, --enable-version3 or --enable-nonfree: the build stays LGPL-2.1+.
(
  cd "$SRC/ffmpeg-build"
  "$SRC/ffmpeg/configure" \
    --prefix="$OUT" \
    --disable-everything --disable-autodetect --disable-doc --disable-debug \
    --disable-network --disable-ffplay --disable-ffprobe \
    --enable-ffmpeg --enable-zlib --enable-libsnappy \
    --enable-swscale --enable-swresample \
    --enable-protocol=file,pipe \
    --enable-demuxer=mov,matroska,avi,rawvideo,image2,wav \
    --enable-muxer=mov \
    --enable-decoder=h264,hevc,prores,mjpeg,png,rawvideo,mpeg4,vp8,vp9,hap,aac,mp3,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le,pcm_u8 \
    --enable-encoder=hap,pcm_s16le \
    --enable-parser=h264,hevc,aac,mpegaudio,mjpeg,png,mpeg4video,vp8,vp9 \
    --enable-bsf=aac_adtstoasc,h264_mp4toannexb,hevc_mp4toannexb,extract_extradata \
    --enable-filter=scale,format,aformat,aresample,null,anull \
    --extra-cflags="-I$DEPS/include -mmacosx-version-min=$MIN_MACOS" \
    --extra-ldflags="-L$DEPS/lib -mmacosx-version-min=$MIN_MACOS" \
    --extra-libs="-lc++"
  make -j"$JOBS"
  make install
)
END=$(date +%s)

BIN="$OUT/bin/ffmpeg"
# Checks: LGPL flags, HAP encoder present, no non-system dynamic libs.
CONF=$("$BIN" -hide_banner -buildconf)
if echo "$CONF" | grep -Eq -- '--enable-(gpl|version3|nonfree)'; then
  echo "license check failed: GPL/nonfree flag in configuration" >&2; exit 1
fi
"$BIN" -hide_banner -L | head -3 | grep -q "GNU Lesser General Public" || { echo "not LGPL" >&2; exit 1; }
"$BIN" -hide_banner -encoders | grep -q " hap " || { echo "no HAP encoder" >&2; exit 1; }
if otool -L "$BIN" | tail -n +2 | grep -Ev '^\s+(/usr/lib/|/System/)' | grep -q .; then
  echo "non-system dynamic library linked:" >&2; otool -L "$BIN" >&2; exit 1
fi
echo "$STAMP" > "$OUT/stamp"
echo "built $BIN in $((END - START)) s"
