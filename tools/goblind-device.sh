#!/bin/sh
# goblind-device.sh <serial> {deploy|start|stop|status|log|python} [profile]
#
# Side-by-side dev control of the Rust daemon on the Brick:
#   deploy  push goblind + goblinctl to /mnt/UDISK/goblin-link-dev/ (never the installed pak)
#   start   stop the Python daemon, then start goblind for <profile> (default: work),
#           detached with start-stop-daemon so it survives the adb session closing
#   stop    stop goblind (by its pidfile only; never a broadcast kill)
#   python  stop goblind and hand the profile back to the Python pak daemon
#   status  pids and the last log lines;  log  follow the log
#
# goblind shares the pak's data dir (profiles/<name>/pairing.json etc.), so there is one copy of
# the device tokens. It runs as root like the Python daemon.
set -eu
SERIAL="$1"; CMD="$2"; PROFILE="${3:-work}"
GOBLIND_LOG="${GOBLIND_LOG:-info}"  # e.g. GOBLIND_LOG=info,goblind=debug
HERE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ADB="adb -s $SERIAL"
DEV=/mnt/UDISK/goblin-link-dev
DATA="${GOBLIN_DEVICE_DATA:-/mnt/SDCARD/.userdata/tg5040/goblin-link}"
PAK=/mnt/SDCARD/Tools/tg5040/MuseGadget.pak
PID=/tmp/goblind.pid
LOG=$DEV/goblind.log
BIN="$HERE/target/aarch64-unknown-linux-gnu/release"

sh_dev() { $ADB shell "$1" </dev/null; }

case "$CMD" in
deploy)
    sh_dev "mkdir -p $DEV"
    $ADB push "$BIN/goblind" "$DEV/goblind.new" >/dev/null
    $ADB push "$BIN/goblinctl" "$DEV/goblinctl.new" >/dev/null
    sh_dev "cd $DEV && chmod +x goblind.new goblinctl.new && mv goblind.new goblind && mv goblinctl.new goblinctl && ls -la goblind goblinctl"
    ;;
start)
    sh_dev "
      [ -f $PID ] && kill -0 \$(cat $PID) 2>/dev/null && { echo 'goblind already running'; exit 0; }
      start-stop-daemon -S -b -m -p /tmp/mg-stop.pid -x /bin/sh -- -c '$PAK/bin/mg-ctl stop' ; sleep 2
      rm -f $PID
      start-stop-daemon -S -b -m -p $PID -x /bin/sh -- -c 'RUST_LOG=$GOBLIND_LOG exec $DEV/goblind --data-dir $DATA --runtime-dir /run/goblin-link --profile $PROFILE >>$LOG 2>&1'
      sleep 6; tail -8 $LOG"
    ;;
stop)
    sh_dev "
      if [ -f $PID ]; then p=\$(cat $PID); [ \"\$p\" -gt 1 ] && kill -TERM \"\$p\"; sleep 2; rm -f $PID; echo stopped; else echo 'not running'; fi"
    ;;
python)
    "$0" "$SERIAL" stop
    sh_dev "start-stop-daemon -S -b -m -p /tmp/mgstart.pid -x /bin/sh -- -c '$PAK/bin/mg-ctl stop; sleep 1; $PAK/bin/mg-ctl start'; sleep 8; tail -2 $DATA/logs/\$(cat $DATA/active).log"
    ;;
status)
    sh_dev "ps w | grep -E '[m]used|[m]usegadget run'; tail -5 $LOG 2>/dev/null"
    ;;
log)
    $ADB shell "tail -f $LOG"
    ;;
*)
    echo "usage: $0 <serial> {deploy|start|stop|status|log|python} [profile]" >&2; exit 1 ;;
esac
