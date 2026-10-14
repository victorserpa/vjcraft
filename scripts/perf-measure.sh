#!/usr/bin/env bash
# Usage: scripts/perf-measure.sh <label> <seconds> [video.mp4]
# Runs the release binary (idle if no video, else VJC_AUTOPLAY=<video>) and samples
# RSS (MB) and %CPU via ps every second. Kills only the PID it started.
# Prints: label, mean/max RSS, mean/max CPU (first 3 s skipped as warm-up).
set -u
label=${1:?label}; secs=${2:-20}; video=${3:-}
bin=target/release/vjcraft
[ -x "$bin" ] || { echo "build first: cargo build --release -p vjc-app" >&2; exit 1; }
if [ -n "$video" ]; then VJC_AUTOPLAY="$video" "$bin" >/dev/null 2>&1 & else "$bin" >/dev/null 2>&1 & fi
pid=$!
trap 'kill "$pid" 2>/dev/null' EXIT
samples=$(mktemp)
for ((i = 0; i < secs; i++)); do
  sleep 1
  kill -0 "$pid" 2>/dev/null || { echo "app exited early" >&2; break; }
  [ "$i" -ge 3 ] && ps -o rss=,%cpu= -p "$pid" >>"$samples"
done
kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
awk -v l="$label" '{r=$1/1024; c=$2; sr+=r; sc+=c; if(r>mr)mr=r; if(c>mc)mc=c; n++}
 END{ if(n==0){print l": no samples"; exit 1}
 printf "%s: RSS mean %.0f MB max %.0f MB | CPU mean %.1f%% max %.1f%% (n=%d)\n", l, sr/n, mr, sc/n, mc, n}' "$samples"
rm -f "$samples"
