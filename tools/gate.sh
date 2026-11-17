#!/usr/bin/env bash
# Boot indyboot on an emulated SGI Indy through the PROM's
# `boot -f bootp()indyboot` path, pick an image from the menu and wait for a
# marker from that image on the serial console.
#
# Usage:
#   tools/gate.sh IMAGE MARKER [TIMEOUT_SECONDS]
#
# IMAGE is an ECOFF netboot image of any operating system. The gate serves it
# over HTTP on this host with a generated manifest, serves build/indyboot
# over the emulator's TFTP, and types the boot command at the PROM monitor.
# It passes when MARKER appears on the serial console after the handoff.
#
# Environment:
#   IRIS, IRIS_CI   emulator binaries (default: ../iris/target/release)
#   INDYBOOT        bootloader image (default: build/indyboot)
#   WORK            working directory (default: build/gate)
#   RAM_MB          first memory bank in MiB (default: 32)
#   HTTP_PORT       host port of the HTTP server (default: 18280)
#   MONITOR_PORT    iris monitor port (default: 18895)
#   BOOT_ARGS       extra words for the boot command, e.g. "ip=dhcp"
#   IRIS_ARGS       extra emulator options, e.g. "--cache full --cpu r5000"
#   CORRUPT         1 to publish a wrong sha256 and expect a refusal
#   AUTOBOOT        1 to set `default` and a 3 s `timeout` in the manifest and
#                   type nothing at the menu
#   QUIT            1 to type q at the menu and expect the PROM monitor prompt
#   URL             manifest URL to boot with, in place of the local server
#   EXPECT          with URL: text to wait for after the boot command, in
#                   place of the menu steps
#   DIRECT          1 to boot IMAGE straight from the PROM, without indyboot.
#                   This is the control: an image that fails here fails for
#                   reasons outside the bootloader.
set -euo pipefail

IMAGE="${1:?usage: gate.sh IMAGE MARKER [TIMEOUT_SECONDS]}"
MARKER="${2:?usage: gate.sh IMAGE MARKER [TIMEOUT_SECONDS]}"
TIMEOUT="${3:-180}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IRIS="${IRIS:-$ROOT/../iris/target/release/iris}"
IRIS_CI="${IRIS_CI:-$ROOT/../iris/target/release/iris-ci}"
INDYBOOT="${INDYBOOT:-$ROOT/build/indyboot}"
WORK="${WORK:-$ROOT/build/gate}"
RAM_MB="${RAM_MB:-32}"
HTTP_PORT="${HTTP_PORT:-18280}"
MONITOR_PORT="${MONITOR_PORT:-18895}"

for path in "$IMAGE" "$IRIS" "$IRIS_CI" "$INDYBOOT"; do
  if [ ! -e "$path" ]; then
    echo "gate: missing $path" >&2
    exit 2
  fi
done

rm -rf "$WORK"
mkdir -p "$WORK/tftp" "$WORK/www/os"
cp "$INDYBOOT" "$WORK/tftp/indyboot"
cp "$IMAGE" "$WORK/www/os/image"

SUM="$(sha256sum "$WORK/www/os/image" | cut -d' ' -f1)"
if [ "${CORRUPT:-0}" = 1 ]; then
  SUM="$(printf '%064d' 0)"
fi
SIZE="$(stat -c %s "$WORK/www/os/image")"

# The main manifest includes a fragment, the way an OS project publishes one.
cat >"$WORK/www/boot.cfg" <<CFG
# indyboot gate manifest
${AUTOBOOT:+timeout = 3}
${AUTOBOOT:+default = gate}
include = os/indyboot.cfg
include = missing/indyboot.cfg
CFG
cat >"$WORK/www/os/indyboot.cfg" <<CFG
[gate]
title = Gate image
image = image
sha256 = $SUM
size = $SIZE
args = gate=1 second
CFG

python3 -m http.server "$HTTP_PORT" --bind 127.0.0.1 --directory "$WORK/www" >"$WORK/http.log" 2>&1 &
HTTP_PID=$!

cat >"$WORK/machine.toml" <<TOML
banks = [$RAM_MB, 0, 0, 0]
no_audio = true
headless = true
nvram = "$WORK/nvram.bin"

[machine]
profile = "indy_ip24"

[network]
mode = "nat"
tftp_dir = "$WORK/tftp"

[scsi]
TOML

IRIS_NO_IDLE=1 IRIS_TFTP_TRACE=1 "$IRIS" --config "$WORK/machine.toml" --ci \
  --ci-socket "$WORK/iris.sock" --monitor-port "$MONITOR_PORT" \
  --serial-log "$WORK/serial.log" --headless --noaudio ${IRIS_ARGS:-} >"$WORK/iris.out" 2>&1 &
IRIS_PID=$!

cleanup() {
  local status=$?
  trap - EXIT
  kill "$IRIS_PID" "$HTTP_PID" 2>/dev/null || true
  wait "$IRIS_PID" "$HTTP_PID" 2>/dev/null || true
  if [ "$status" -ne 0 ]; then
    echo "--- iris output (tail)" >&2
    tail -15 "$WORK/iris.out" >&2 || true
    echo "--- http log (tail)" >&2
    tail -8 "$WORK/http.log" >&2 || true
    echo "--- serial log (tail)" >&2
    tail -40 "$WORK/serial.log" | tr -d '\r' >&2 || true
  fi
  exit "$status"
}
trap cleanup EXIT

ic() { "$IRIS_CI" --socket "$WORK/iris.sock" -q "$@"; }

ic status --wait-ready 30
ic start
# Headless, the PROM puts its maintenance menu on the serial console.
ic serial-wait 'Option?' --timeout 60
ic serial-send '5'
ic serial-wait '>>' --timeout 30
if [ "${DIRECT:-0}" = 1 ]; then
  cp "$IMAGE" "$WORK/tftp/direct"
  ic serial-send 'boot -f bootp()direct'
  ic serial-wait "$MARKER" --timeout "$TIMEOUT"
  echo "gate: PASS  marker \"$MARKER\" seen on a direct PROM boot; logs in $WORK"
  exit 0
fi
ic serial-send "boot -f bootp()indyboot url=${URL:-http://192.168.0.1:$HTTP_PORT/boot.cfg} ${BOOT_ARGS:-}"
if [ -n "${EXPECT:-}" ]; then
  ic serial-wait "$EXPECT" --timeout "$TIMEOUT"
  echo "gate: PASS  \"$EXPECT\" seen; logs in $WORK"
  exit 0
fi
if [ -n "${AUTOBOOT:-}" ]; then
  ic serial-wait 'Starting Gate image in 3 s' --timeout 90
else
  ic serial-wait 'Boot which?' --timeout 90
  ic serial-send 'i'
  ic serial-wait 'Boot which?' --timeout 20
  if [ "${QUIT:-0}" = 1 ]; then
    ic serial-send 'q'
    ic serial-wait '>> ' --timeout 30
    ic serial-send 'printenv netaddr'
    ic serial-wait 'netaddr=192.168.0.2' --timeout 30
    echo "gate: PASS  q returned to a working PROM monitor; logs in $WORK"
    exit 0
  fi
  ic serial-send '1'
fi
if [ "${CORRUPT:-0}" = 1 ]; then
  ic serial-wait 'SHA-256 mismatch' --timeout "$TIMEOUT"
  ic serial-wait 'Press a key for the menu' --timeout 10
  echo "gate: PASS  indyboot refused an image with a wrong sha256; logs in $WORK"
  exit 0
fi
ic serial-wait 'SHA-256 verified' --timeout "$TIMEOUT"
ic serial-wait "$MARKER" --timeout "$TIMEOUT"

echo "gate: PASS  marker \"$MARKER\" seen after the handoff; logs in $WORK"
grep -E 'address|loaded at' "$WORK/serial.log" | tr -d '\r' | sed 's/^/gate: /'
