#!/usr/bin/env bash
# Generates short (3 s, small) sample videos covering common formats, to test that the
# app opens them (crates/vjc-media/tests/format_matrix.rs, set VJC_FORMAT_DIR to the output).
# Usage: scripts/gen-format-matrix.sh [outdir]   (default: a new temp dir, printed at the end)
# Two ffmpeg builds are used when present: the bundled one (scripts/build-ffmpeg.sh, has the
# HAP encoder) and `ffmpeg` from PATH (x264, ProRes, ...). Each case runs with the first
# build that has the encoder; a missing encoder skips the case and is listed, never fails.
# Each sample also gets <name>.expect: "WxH alpha|opaque ok|error" for the test.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$(mktemp -d "${TMPDIR:-/tmp}/vjc-formats.XXXXXX")}"
mkdir -p "$OUT"
BUNDLED="${VJC_FFMPEG:-$ROOT/target/ffmpeg/bin/ffmpeg}"
# Another build location (a sibling worktree, say): VJC_FFMPEG=/path/to/ffmpeg.
SYSTEM="$(command -v ffmpeg || true)"
BINS=()
[ -x "$BUNDLED" ] && BINS+=("$BUNDLED")
[ -n "$SYSTEM" ] && BINS+=("$SYSTEM")
[ ${#BINS[@]} -eq 0 ] && { echo "no ffmpeg found" >&2; exit 1; }
echo "ffmpeg builds: ${BINS[*]}"

SKIPPED=()
DONE=()
pick() { # encoder -> prints a binary that has it
  for b in "${BINS[@]}"; do
    "$b" -hide_banner -encoders 2>/dev/null | awk '{print $2}' | grep -qx "$1" && { echo "$b"; return 0; }
  done
  return 1
}
# gen NAME SIZE ALPHA ENCODER [ffmpeg output args...]   (SIZE WxH as stored, ALPHA alpha|opaque)
# Env SRC overrides the lavfi source filter chain (default testsrc2 + a vignette so alpha is real).
gen() {
  local name="$1" size="$2" alpha="$3" enc="$4"; shift 4
  local bin; bin="$(pick "$enc")" || { SKIPPED+=("$name (no encoder $enc)"); return; }
  local src="testsrc2=size=${SRCSIZE:-$size}:rate=${RATE:-25}:duration=3,format=${SRCFMT:-yuv420p}"
  if [ "$alpha" = alpha ]; then
    src="testsrc2=size=$size:rate=${RATE:-25}:duration=3,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(X,W/2),255,0)'"
  fi
  if "$bin" -v error -y -f lavfi -i "$src" ${AUDIO:--an} "$@" "$OUT/$name" </dev/null; then
    DONE+=("$name")
    echo "${EXPECT_SIZE:-$size} $alpha ${EXPECT_RESULT:-ok}" >"$OUT/$name.expect"
  else
    rm -f "$OUT/$name"
    SKIPPED+=("$name (encode failed with $enc)")
  fi
}
AUD=(-f lavfi -i "sine=frequency=440:duration=3")
S=320x180

# --- .mov ---
gen prores422.mov $S opaque prores_ks -c:v prores_ks -profile:v 2 -pix_fmt yuv422p10le
gen prores4444_alpha.mov $S alpha prores_ks -c:v prores_ks -profile:v 4 -pix_fmt yuva444p10le
gen qtrle_alpha.mov $S alpha qtrle -c:v qtrle -pix_fmt argb
gen qtrle_opaque.mov $S opaque qtrle -c:v qtrle -pix_fmt rgb24
gen png_in_mov.mov $S opaque png -c:v png -pix_fmt rgb24
gen png_in_mov_alpha.mov $S alpha png -c:v png -pix_fmt rgba
gen dnxhd.mov 1920x1080 opaque dnxhd -c:v dnxhd -b:v 36M -pix_fmt yuv422p
gen dnxhr_lb.mov $S opaque dnxhd -c:v dnxhd -profile:v dnxhr_lb -pix_fmt yuv422p
gen mjpeg.mov $S opaque mjpeg -c:v mjpeg -pix_fmt yuvj420p
gen h264_8bit.mov $S opaque libx264 -c:v libx264 -pix_fmt yuv420p
gen h264_10bit_422.mov $S opaque libx264 -c:v libx264 -pix_fmt yuv422p10le
gen hevc.mov $S opaque libx265 -c:v libx265 -pix_fmt yuv420p -x265-params log-level=error
gen hevc_hvc1.mov $S opaque libx265 -c:v libx265 -pix_fmt yuv420p -tag:v hvc1 -x265-params log-level=error
gen hevc_videotoolbox.mov $S opaque hevc_videotoolbox -c:v hevc_videotoolbox -tag:v hvc1 -pix_fmt yuv420p
# rotation: display matrix written by remuxing (stored 320x180, shown 180x320 after rotation)
if b="$(pick libx264)" && "$b" -v error -y -f lavfi -i "testsrc2=size=$S:rate=25:duration=3,format=yuv420p" -c:v libx264 -pix_fmt yuv420p "$OUT/rot_src.mp4" </dev/null \
  && "$b" -v error -y -display_rotation 90 -i "$OUT/rot_src.mp4" -c copy "$OUT/rotate90.mov" </dev/null; then
  echo "180x320 opaque ok" >"$OUT/rotate90.mov.expect"; DONE+=(rotate90.mov)
else SKIPPED+=("rotate90.mov"); fi
rm -f "$OUT/rot_src.mp4"
RATE=25 gen vfr.mov $S opaque libx264 -vf "setpts='PTS+if(mod(N,5),0,0.05/TB)'" -fps_mode vfr -c:v libx264 -pix_fmt yuv420p
SRCFMT=yuv444p SRCSIZE=1920x1080 gen odd_1919x1079.mov 1919x1079 opaque libx264 -vf crop=1919:1079 -c:v libx264 -pix_fmt yuv444p
SRCFMT=yuv444p SRCSIZE=1920x1080 gen odd_1919x1079_prores.mov 1919x1079 opaque prores_ks -vf crop=1919:1079 -c:v prores_ks -pix_fmt yuv444p10le
gen timecode.mov $S opaque libx264 -c:v libx264 -pix_fmt yuv420p -timecode 01:00:00:00
AUDIO=" " gen audio_pcm24_2tracks.mov $S opaque libx264 -f lavfi -i "sine=frequency=440:duration=3" -f lavfi -i "sine=frequency=880:duration=3" -map 0:v -map 1:a -map 2:a -c:v libx264 -pix_fmt yuv420p -c:a pcm_s24le
gen no_audio.mov $S opaque libx264 -an -c:v libx264 -pix_fmt yuv420p
gen hap.mov $S opaque hap -c:v hap -format hap
gen hap_alpha.mov $S alpha hap -c:v hap -format hap_alpha
gen hap_q.mov $S opaque hap -c:v hap -format hap_q
# --- .mp4 ---
gen h264.mp4 $S opaque libx264 -c:v libx264 -pix_fmt yuv420p -movflags +faststart
gen hevc.mp4 $S opaque libx265 -c:v libx265 -pix_fmt yuv420p -tag:v hvc1 -x265-params log-level=error
# --- .mkv / .webm / .avi ---
gen h264.mkv $S opaque libx264 -c:v libx264 -pix_fmt yuv420p
gen vp8.webm $S opaque libvpx -c:v libvpx -pix_fmt yuv420p
gen vp9.webm $S opaque libvpx-vp9 -c:v libvpx-vp9 -pix_fmt yuv420p
gen vp9_alpha.webm $S alpha libvpx-vp9 -c:v libvpx-vp9 -pix_fmt yuva420p
gen vp8_alpha.webm $S alpha libvpx -c:v libvpx -pix_fmt yuva420p -auto-alt-ref 0
gen av1.webm $S opaque libsvtav1 -c:v libsvtav1 -svtav1-params log=0 -pix_fmt yuv420p
gen av1.mkv $S opaque libaom-av1 -c:v libaom-av1 -cpu-used 8 -pix_fmt yuv420p
gen mjpeg.avi $S opaque mjpeg -c:v mjpeg -pix_fmt yuvj420p

# --- image sequence (named like the app's sequence labels: frame_%04d.png) ---
SEQ="$OUT/seq"; mkdir -p "$SEQ"
if b="$(pick png)" && "$b" -v error -y -f lavfi -i "testsrc2=size=$S:rate=25:duration=2" "$SEQ/frame_%04d.png" </dev/null; then
  DONE+=("seq/frame_0001.png (image sequence)")
else SKIPPED+=("image sequence"); fi

# --- broken files ---
if [ -f "$OUT/h264.mp4" ]; then
  head -c 20000 "$OUT/h264.mp4" >"$OUT/truncated.mp4"; echo "320x180 opaque any" >"$OUT/truncated.mp4.expect"
  # corrupt: valid header and tail, garbage in the middle of the mdat
  cp "$OUT/h264.mp4" "$OUT/corrupt.mp4"
  sz=$(wc -c <"$OUT/corrupt.mp4")
  dd if=/dev/urandom of="$OUT/corrupt.mp4" bs=1 seek=$((sz/3)) count=$((sz/3)) conv=notrunc 2>/dev/null
  echo "320x180 opaque any" >"$OUT/corrupt.mp4.expect"
  cp "$OUT/h264.mp4" "$OUT/wrong_ext.mov"; echo "$(cat "$OUT/h264.mp4.expect")" >"$OUT/wrong_ext.mov.expect"
  cp "$OUT/h264.mp4" "$OUT/wrong_ext.avi"; echo "$(cat "$OUT/h264.mp4.expect")" >"$OUT/wrong_ext.avi.expect"
  DONE+=(truncated.mp4 corrupt.mp4 wrong_ext.mov wrong_ext.avi)
fi
if [ -f "$OUT/h264_8bit.mov" ]; then
  # Codec nobody decodes: the sample entry fourcc is rewritten (same length).
  LC_ALL=C sed 's/avc1/zzzz/' "$OUT/h264_8bit.mov" >"$OUT/unknown_codec.mov"
  echo "0x0 opaque error" >"$OUT/unknown_codec.mov.expect"; DONE+=(unknown_codec.mov)
fi
: >"$OUT/zero_byte.mov"; echo "0x0 opaque error" >"$OUT/zero_byte.mov.expect"
head -c 4096 /dev/urandom >"$OUT/garbage.mov"; echo "0x0 opaque error" >"$OUT/garbage.mov.expect"
DONE+=(zero_byte.mov garbage.mov)

# Optional heavy sample for the multi-layer load test (HEAVY=1): 1080p25 ProRes 4444 + alpha, 10 s.
if [ "${HEAVY:-0}" = 1 ] && b="$(pick prores_ks)"; then
  "$b" -v error -y -f lavfi -i "testsrc2=size=1920x1080:rate=25:duration=10,format=yuva444p10le" \
    -c:v prores_ks -profile:v 4 -pix_fmt yuva444p10le "$OUT/heavy_prores4444_1080p.mov" </dev/null \
    && echo "heavy sample: $OUT/heavy_prores4444_1080p.mov"
fi

echo "generated ${#DONE[@]} in $OUT"
for s in "${SKIPPED[@]:-}"; do [ -n "$s" ] && echo "SKIPPED: $s"; done
echo "$OUT"
