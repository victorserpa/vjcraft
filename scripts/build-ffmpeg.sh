#!/usr/bin/env bash
# Builds, from one pinned FFmpeg source, two LGPL products:
#   1. a small static FFmpeg CLI with the HAP encoder (needs libsnappy) into
#      target/ffmpeg/bin/ffmpeg, used by "Convert to HAP";
#   2. shared libav* libs for playback (all native decoders/demuxers/parsers, swscale,
#      swresample, VideoToolbox hwaccel, libvpx + dav1d decoders, HAP encoder only) into
#      target/ffmpeg-libs. Packaging points the ffmpeg-sys build at it
#      (PKG_CONFIG_PATH=target/ffmpeg-libs/lib/pkgconfig); dev builds keep using Homebrew.
# libvpx and dav1d (decoders, BSD) are built here too, as shared libs with
# -mmacosx-version-min=$MIN_MACOS, so that no bundled dylib needs a newer macOS than the app
# (Homebrew's bottles target the macOS they were built on). FFmpeg links against them, not Homebrew.
# A REBUILD (first run, version bump, --force) needs meson and ninja (brew install meson ninja);
# an up-to-date tree needs neither.
#
# Network: exactly four downloads, all pinned and SHA-256 checked, nothing else.
#   FFmpeg 9.0.2  https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz   (official release tarball)
#   snappy 1.3.1  https://github.com/google/snappy/archive/refs/tags/1.3.1.tar.gz
#   libvpx 1.17.0 https://github.com/webmproject/libvpx/archive/refs/tags/v1.17.0.tar.gz
#   dav1d 1.5.4   https://code.videolan.org/videolan/dav1d/-/archive/1.5.4/dav1d-1.5.4.tar.bz2
# Checksum source: ffmpeg.org publishes only .asc signatures for current releases (no gpg
# on the build machine), so the FFmpeg and snappy SHA-256 values are the ones in the Homebrew formulas
# (Formula/f/ffmpeg.rb, Formula/s/snappy.rb), an independent source; the download is
# refused if it does not match. Re-check and bump them together when changing a version.
# Licenses: FFmpeg configured without --enable-gpl / --enable-version3 / --enable-nonfree
# (LGPL-2.1+); snappy is BSD-3-Clause. The result is checked at the end.
# Source goes to target/ffmpeg-src/{dl,ffmpeg,snappy,libvpx,dav1d} (each its own empty dir); only the
# trees' own cmake / configure / make are run.
# Flags: --force rebuild even when up to date. Env: JOBS (default 4).
set -euo pipefail

FFMPEG_VER=9.0.2
FFMPEG_URL="https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VER.tar.xz"
FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
SNAPPY_VER=1.3.1
SNAPPY_URL="https://github.com/google/snappy/archive/refs/tags/$SNAPPY_VER.tar.gz"
SNAPPY_SHA256=893f708a0bf4b5529d555ffcee390e940e932fcf90261f682604475a76cd0247
LIBVPX_VER=1.17.0
LIBVPX_URL="https://github.com/webmproject/libvpx/archive/refs/tags/v$LIBVPX_VER.tar.gz"
LIBVPX_SHA256=1020f184046187baa2985dbde38e0691f49c44088bca7a1842b0236c6081dc0a
DAV1D_VER=1.5.4
DAV1D_URL="https://code.videolan.org/videolan/dav1d/-/archive/$DAV1D_VER/dav1d-$DAV1D_VER.tar.bz2"
DAV1D_SHA256=2abfb0c89212e6e4733a54e0ae509ec00a5b845a6360946f918806e14aedb011
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
LIBS="$OUT_BASE/ffmpeg-libs"
DEPS="$SRC/prefix"

STAMP="$FFMPEG_VER+$SNAPPY_VER+dav1d$DAV1D_VER+vpx$LIBVPX_VER+min$MIN_MACOS+$(shasum -a 256 "$0" | cut -c1-16)"

if [ "$FORCE" -eq 0 ] && [ -x "$OUT/bin/ffmpeg" ] && [ -f "$LIBS/lib/pkgconfig/libavcodec.pc" ] && [ "$(cat "$OUT/stamp" 2>/dev/null)" = "$STAMP" ]; then
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
fetch "$LIBVPX_URL" "$LIBVPX_SHA256" "$SRC/dl/libvpx-$LIBVPX_VER.tar.gz"
fetch "$DAV1D_URL" "$DAV1D_SHA256" "$SRC/dl/dav1d-$DAV1D_VER.tar.bz2"
for t in meson ninja; do
  command -v $t >/dev/null || { echo "$t missing (needed to build dav1d; brew install meson ninja)" >&2; exit 1; }
done
unpack "$SRC/dl/snappy-$SNAPPY_VER.tar.gz" "$SRC/snappy"
unpack "$SRC/dl/ffmpeg-$FFMPEG_VER.tar.xz" "$SRC/ffmpeg"
unpack "$SRC/dl/libvpx-$LIBVPX_VER.tar.gz" "$SRC/libvpx"
unpack "$SRC/dl/dav1d-$DAV1D_VER.tar.bz2" "$SRC/dav1d"

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

# libvpx (decoders only, own configure) and dav1d (meson), shared, min macOS $MIN_MACOS.
# Their licenses are copied next to the libs (share/licenses) for scripts/bundle-dylibs.sh.
rm -rf "$SRC/libvpx-build" "$SRC/dav1d-build"
mkdir -p "$SRC/libvpx-build"
(
  cd "$SRC/libvpx-build"
  LDFLAGS="-mmacosx-version-min=$MIN_MACOS" "$SRC/libvpx/configure" --target="$(uname -m)-darwin25-gcc" --prefix="$DEPS" \
    --enable-shared --disable-static --disable-examples --disable-tools --disable-docs \
    --disable-unit-tests --disable-vp8-encoder --disable-vp9-encoder --enable-vp9-highbitdepth \
    --extra-cflags="-mmacosx-version-min=$MIN_MACOS" --extra-cxxflags="-mmacosx-version-min=$MIN_MACOS"
  make -j"$JOBS"
  make install
)
# libvpx installs with a bare install name; give it the absolute one (like dav1d's) so that
# FFmpeg records a path scripts/bundle-dylibs.sh can resolve.
install_name_tool -id "$DEPS/lib/libvpx.12.dylib" "$DEPS/lib/libvpx.12.dylib"
codesign --force -s - "$DEPS/lib/libvpx.12.dylib"
meson setup "$SRC/dav1d-build" "$SRC/dav1d" --prefix="$DEPS" --libdir=lib --buildtype=release \
  --default-library=shared -Denable_tools=false -Denable_tests=false \
  -Dc_args="-mmacosx-version-min=$MIN_MACOS" -Dc_link_args="-mmacosx-version-min=$MIN_MACOS" >/dev/null
ninja -C "$SRC/dav1d-build" -j "$JOBS" >/dev/null
ninja -C "$SRC/dav1d-build" install >/dev/null
mkdir -p "$DEPS/share/licenses/libvpx" "$DEPS/share/licenses/dav1d"
cp "$SRC/libvpx/LICENSE" "$SRC/libvpx/PATENTS" "$DEPS/share/licenses/libvpx/"
cp "$SRC/dav1d/COPYING" "$DEPS/share/licenses/dav1d/"
export PKG_CONFIG_PATH="$DEPS/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

rm -rf "$SRC/ffmpeg-build" "$SRC/ffmpeg-libs-build" "$OUT" "$LIBS"
mkdir -p "$SRC/ffmpeg-build"
START=$(date +%s)
# Inputs: every native demuxer, decoder, parser and bsf (so common sources convert); only the
# file/pipe protocols. Outputs stay minimal: HAP, PCM and AAC encoders, the mov muxer.
# No --enable-gpl, --enable-version3 or --enable-nonfree: the build stays LGPL-2.1+.
(
  cd "$SRC/ffmpeg-build"
  "$SRC/ffmpeg/configure" \
    --prefix="$OUT" \
    --disable-autodetect --disable-doc --disable-debug \
    --disable-network --disable-ffplay --disable-ffprobe \
    --enable-ffmpeg --enable-zlib --enable-libsnappy \
    --enable-swscale --enable-swresample \
    --disable-protocols --enable-protocol=file,pipe \
    --disable-encoders --enable-encoder=hap,pcm_s16le,aac \
    --disable-muxers --enable-muxer=mov \
    --extra-cflags="-I$DEPS/include -mmacosx-version-min=$MIN_MACOS" \
    --extra-ldflags="-L$DEPS/lib -mmacosx-version-min=$MIN_MACOS" \
    --extra-libs="-lc++"
  make -j"$JOBS"
  make install
)

# Shared libs for playback and recording (record.rs). Encoders: hap (Convert to HAP), pcm_s16le,
# aac, png, h264/hevc/prores via VideoToolbox, prores/prores_ks; muxers mov, mp4, image2.
# No libx264/libx265 (GPL): record.rs falls back to VideoToolbox. zlib (png, mkv) and bzlib
# (mkv) come from the macOS SDK. libvpx/dav1d are decoders (their encoders are not enabled). Protocols: file and pipe only.
mkdir -p "$SRC/ffmpeg-libs-build"
(
  cd "$SRC/ffmpeg-libs-build"
  "$SRC/ffmpeg/configure" \
    --prefix="$LIBS" \
    --enable-shared --disable-static \
    --disable-autodetect --disable-doc --disable-debug \
    --disable-network --disable-programs \
    --enable-zlib --enable-bzlib --enable-libsnappy \
    --enable-libdav1d --enable-libvpx --enable-videotoolbox --enable-audiotoolbox \
    --enable-swscale --enable-swresample \
    --disable-protocols --enable-protocol=file,pipe \
    --disable-encoders --enable-encoder=hap,pcm_s16le,aac,png,h264_videotoolbox,hevc_videotoolbox,prores,prores_ks,prores_videotoolbox \
    --disable-muxers --enable-muxer=mov,mp4,image2 \
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
# Shared libs: LGPL, no GPL codecs or network libs linked.
if grep -Eq '^#define CONFIG_(GPL|VERSION3|NONFREE) 1' "$SRC/ffmpeg-libs-build/config.h"; then
  echo "license check failed: GPL/nonfree flag in shared libs" >&2; exit 1
fi
# Allowlist: system libs, our own libav*/sw*, dav1d, vpx (first otool line is the lib's own id).
for l in "$LIBS"/lib/*.dylib; do
  [ -L "$l" ] && continue
  bad=$(otool -L "$l" | tail -n +2 | awk '{print $1}' \
    | grep -Ev '^(/usr/lib/|/System/|@rpath/lib(av|sw|postproc)[a-z]*\.|.*/lib(av|sw)[a-z]*\.[0-9.]*dylib$|.*/libdav1d[.0-9]*dylib$|.*/libvpx[.0-9]*dylib$)' || true)
  if [ -n "$bad" ]; then
    echo "unexpected dependency in $l:" >&2; echo "$bad" >&2; exit 1
  fi
done
for e in aac png h264_videotoolbox hevc_videotoolbox prores_ks prores_videotoolbox; do
  grep -q "^#define CONFIG_$(echo "$e" | tr a-z A-Z)_ENCODER 1" "$SRC/ffmpeg-libs-build/config_components.h" \
    || { echo "missing encoder $e" >&2; exit 1; }
done
echo "$STAMP" > "$OUT/stamp"
echo "built $BIN in $((END - START)) s"
