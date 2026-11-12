#!/bin/sh
# Deploy the Goblin Link UI to the TrimUI Brick and run it, borrowing the display from NextUI.
#
# Deploys ONLY to /mnt/UDISK/goblin-link-dev/ (never the installed pak). To take the screen it
# SIGSTOPs NextUI's foreground app (minput.elf, the Input tool) and SIGCONTs it afterwards, so
# the launcher is always restored even on Ctrl-C. Keeps audio silent (no audio is opened).
#
# Usage: tools/run-device.sh [adb-serial] [-- <goblin-ui args>]
#   e.g. tools/run-device.sh <serial> -- --fps
#        tools/run-device.sh <serial> -- --input-test
#        tools/run-device.sh <serial> -- --benchmark
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SERIAL=""
case "${1:-}" in
    --) ;;                                   # no serial, args follow
    "") ;;                                    # nothing
    *) SERIAL="$1"; shift ;;
esac
[ "${1:-}" = "--" ] && shift
ARGS="$*"

ADB="adb"
[ -n "$SERIAL" ] && ADB="adb -s $SERIAL"

BIN="$HERE/target/aarch64-unknown-linux-gnu/release/goblin-ui"
[ -f "$BIN" ] || { echo "build first: tools/build-device.sh" >&2; exit 1; }

$ADB shell 'mkdir -p /mnt/UDISK/goblin-link-dev'
$ADB push "$BIN" /mnt/UDISK/goblin-link-dev/goblin-ui >/dev/null
$ADB shell 'chmod +x /mnt/UDISK/goblin-link-dev/goblin-ui'

# Run on-device: find NextUI's foreground SDL app, pause it, run ours, always resume it.
$ADB shell "sh -c '
set +e
FG=\$(ps | grep -E \"[m]input.elf|[n]extui.elf\" | awk \"{print \\\$1}\" | head -1)
[ -n \"\$FG\" ] && kill -STOP \$FG
cd /mnt/UDISK/goblin-link-dev
LD_LIBRARY_PATH=/usr/trimui/lib ./goblin-ui $ARGS
RC=\$?
[ -n \"\$FG\" ] && kill -CONT \$FG
echo \"goblin-ui exited \$RC; launcher resumed\"
'"
