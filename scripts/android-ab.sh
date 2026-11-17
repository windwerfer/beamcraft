#!/usr/bin/env bash
# A/B of two presenter builds against the Android viewer over Wi-Fi (testing.md §3.8):
# alternates A and B run by run, so a link that drifts hits both, and records the device's
# Wi-Fi signal after each run. A run where the viewer never connected (e.g. a handshake lost
# to a weak link) is repeated once. Ends with scripts/run-stats.py over all runs.
# usage: scripts/android-ab.sh <a> <b> <pairs> <seconds> <out-dir> [presenter args...]
#   <a>, <b>   a git revision (built in a temporary worktree into target/ab-<rev>) or a binary
#   ENCODER, ADB_SERIAL, ADVERTISE_IP   as for scripts/android-wifi-run.sh (ENCODER default vaapi)
# Writes <out-dir>/{a,b}-<n>.log (+ .logcat), <out-dir>/wifi.txt and <out-dir>/summary.txt.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $# -ge 5 ]] || { sed -n '2,9p' "$0" >&2; exit 2; }
A=$1; B=$2; PAIRS=$3; DUR=$4; OUT=$5; shift 5
ADB=(adb ${ADB_SERIAL:+-s "$ADB_SERIAL"})
[[ $("${ADB[@]}" get-state 2>/dev/null) == device ]] ||
  { echo "no device for adb (plug it in, or set ADB_SERIAL)" >&2; exit 1; }
mkdir -p "$OUT"

binary() {
  if [[ -x "$1" && -f "$1" ]]; then echo "$1"; return; fi
  local rev dir target
  rev=$(git rev-parse --short "$1")
  target="$PWD/target/ab-$rev"
  if [[ ! -x "$target/release/mirrorwire" ]]; then
    dir=$(mktemp -d)
    git worktree add -q --detach "$dir" "$rev" >&2
    (cd "$dir" && CARGO_TARGET_DIR="$target" cargo build --release -q \
      -p mirrorwire-app --no-default-features) >&2
    git worktree remove --force "$dir" >&2
  fi
  echo "$target/release/mirrorwire"
}

BIN_A=$(binary "$A"); BIN_B=$(binary "$B")
echo "a = $A ($BIN_A), b = $B ($BIN_B)" | tee "$OUT/wifi.txt"

wifi() {
  "${ADB[@]}" shell "dumpsys wifi | grep -m1 mWifiInfo" 2>/dev/null |
    grep -oE "RSSI: [-0-9]+|Link speed: [0-9]+Mbps|Frequency: [0-9]+MHz" | head -3 | tr '\n' ' ' || true
}

run() {
  local side=$1 bin=$2 n=$3 log="$OUT/$1-$3.log"
  shift 3
  for attempt in 1 2; do
    MIRRORWIRE_BIN=$bin ENCODER=${ENCODER:-vaapi} scripts/android-wifi-run.sh "$DUR" "$log" "$@" \
      > /dev/null 2>&1 || true
    grep -q "viewer queue" "$log" && break
    echo "$side-$n: no viewer report (attempt $attempt)" | tee -a "$OUT/wifi.txt"
  done
  echo "$side-$n $(wifi)" | tee -a "$OUT/wifi.txt"
}

for n in $(seq "$PAIRS"); do
  run a "$BIN_A" "$n" "$@"
  run b "$BIN_B" "$n" "$@"
done
shopt -s nullglob
logs=("$OUT"/a-*.log "$OUT"/b-*.log)
[[ ${#logs[@]} -gt 0 ]] || { echo "no run produced a log, see $OUT/wifi.txt" >&2; exit 1; }
scripts/run-stats.py "${logs[@]}" | tee "$OUT/summary.txt"
