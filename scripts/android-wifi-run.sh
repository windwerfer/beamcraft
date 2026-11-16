#!/usr/bin/env bash
# One real-Wi-Fi run: test-pattern presenter on this machine, the Android viewer on a phone or
# tablet attached with adb (USB for control; the video goes over Wi-Fi).
# usage: scripts/android-wifi-run.sh <seconds> <log-out> [presenter args...]
#   BEAMCRAFT_BIN=path   presenter binary (default: build target/release/beamcraft)
#   ADB_SERIAL=serial    which device (default: the only one attached)
#   ADVERTISE_IP=ip      presenter address the device connects to (default: the local
#                        address that routes to the device's wlan0)
# Writes the presenter log to <log-out> and the device's `beamcraft` logcat to <log-out>.logcat.
# Needs the APK installed (packaging/android/build-apk.sh, then `adb install -r`) and the
# same BeamCraft release on both sides. Summarise runs with scripts/run-stats.py.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $# -ge 2 ]] || { sed -n '2,11p' "$0" >&2; exit 2; }
DUR=$1; LOG=$2; shift 2
PKG=io.github.windwerfer.beamcraft
ADB=(adb ${ADB_SERIAL:+-s "$ADB_SERIAL"})
BIN=${BEAMCRAFT_BIN:-}
if [[ -z "$BIN" ]]; then
  cargo build --release -q -p beamcraft-app --no-default-features
  BIN=target/release/beamcraft
fi
if [[ -z "${ADVERTISE_IP:-}" ]]; then
  DEV_IP=$("${ADB[@]}" shell ip -4 addr show wlan0 | sed -n 's/.*inet \([0-9.]*\).*/\1/p' | head -1)
  [[ -n "$DEV_IP" ]] || { echo "device has no wlan0 IPv4 address (Wi-Fi off?)" >&2; exit 1; }
  ADVERTISE_IP=$(ip -4 route get "$DEV_IP" | sed -n 's/.* src \([0-9.]*\).*/\1/p')
fi
W=$(mktemp -d)
"${ADB[@]}" shell am force-stop "$PKG"
"$BIN" --config-dir "$W/p" present --source test --encoder x264 --no-mdns --no-pairing \
  --uri-file "$W/uri" --advertise-ip "$ADVERTISE_IP" "$@" > "$LOG" 2>&1 &
PP=$!
trap 'kill -INT $PP 2>/dev/null || true; "${ADB[@]}" shell am force-stop "$PKG"; rm -rf "$W"' EXIT
for _ in $(seq 100); do [[ -s "$W/uri" ]] && break; sleep 0.1; done
[[ -s "$W/uri" ]] || { echo "presenter did not start, see $LOG" >&2; exit 1; }
"${ADB[@]}" logcat -c
"${ADB[@]}" shell am start -a android.intent.action.VIEW -d "\"$(cat "$W/uri")\"" "$PKG" >/dev/null
sleep "$DUR"
"${ADB[@]}" logcat -d -s beamcraft > "$LOG.logcat" || true
