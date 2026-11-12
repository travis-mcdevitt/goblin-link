#!/bin/sh
# ui-device.sh <serial> {deploy|start|stop|key|status} [goblin-ui args...]
#
# Runs goblin-ui on the Brick detached from the adb session (start-stop-daemon), borrowing the
# display from NextUI: `start` SIGSTOPs NextUI's foreground SDL app and records it; `stop`
# SIGTERMs goblin-ui (by its pidfile only) and SIGCONTs exactly the recorded process. Deploys only
# to /mnt/UDISK/goblin-link-dev/. Opens no audio.
#   tools/ui-device.sh <serial> deploy
#   tools/ui-device.sh <serial> start --live
#   tools/ui-device.sh <serial> stop
#   tools/ui-device.sh <serial> key down down a   # scripted presses (goblin-ui reads a FIFO)
set -eu
SERIAL="$1"; CMD="$2"; shift 2
ARGS="$*"
UI_ENV="${UI_ENV:-}"   # extra env for goblin-ui, e.g. UI_ENV=GOBLIN_UI_STATS=1
HERE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ADB="adb -s $SERIAL"
DEV=/mnt/UDISK/goblin-link-dev
PID=/tmp/goblin-ui.pid
PAUSED=/tmp/goblin-ui.paused
LOG=$DEV/goblin-ui.log
FIFO=/tmp/goblin-ui.in

sh_dev() { $ADB shell "$1" </dev/null; }

case "$CMD" in
deploy)
    sh_dev "mkdir -p $DEV"
    $ADB push "$HERE/target/aarch64-unknown-linux-gnu/release/goblin-ui" "$DEV/goblin-ui.new" >/dev/null
    sh_dev "chmod +x $DEV/goblin-ui.new && mv $DEV/goblin-ui.new $DEV/goblin-ui && ls -la $DEV/goblin-ui"
    ;;
start)
    sh_dev "
      [ -f $PID ] && kill -0 \$(cat $PID) 2>/dev/null && { echo 'goblin-ui already running'; exit 0; }
      FG=\$(ps | grep -E '[n]extui.elf|[m]input.elf' | awk '{print \$1}' | head -1)
      if [ -n \"\$FG\" ] && [ \"\$FG\" -gt 1 ]; then kill -STOP \"\$FG\"; echo \"\$FG\" > $PAUSED; fi
      rm -f $PID $FIFO; mkfifo $FIFO
      start-stop-daemon -S -b -m -p $PID -x /bin/sh -- -c 'cd $DEV && LD_LIBRARY_PATH=/usr/trimui/lib GOBLIN_UI_INPUT_FIFO=$FIFO $UI_ENV exec ./goblin-ui $ARGS >>$LOG 2>&1'
      sleep 3; echo \"goblin-ui pid \$(cat $PID), paused \$(cat $PAUSED 2>/dev/null)\"; tail -3 $LOG"
    ;;
stop)
    sh_dev "
      if [ -f $PID ]; then p=\$(cat $PID); [ \"\$p\" -gt 1 ] && kill -TERM \"\$p\"; sleep 1; rm -f $PID; fi
      if [ -f $PAUSED ]; then f=\$(cat $PAUSED); [ \"\$f\" -gt 1 ] && kill -CONT \"\$f\"; rm -f $PAUSED; echo \"resumed \$f\"; fi
      rm -f $FIFO; echo stopped"
    ;;
key)
    # One press per name, 0.4 s apart, so each frame settles before the next.
    for k in $ARGS; do sh_dev "[ -p $FIFO ] && echo $k > $FIFO"; sleep 0.4; done
    ;;
status)
    sh_dev "ps w | grep -E '[m]use-ui|[n]extui.elf|[m]input.elf'; tail -5 $LOG 2>/dev/null"
    ;;
*)
    echo "usage: $0 <serial> {deploy|start|stop|key|status} [goblin-ui args | key names]" >&2; exit 1 ;;
esac
