#!/bin/sh
# Keep the Brick from suspending while the gadget is busy.
#
#   stay-awake.sh on     take the "goblin-link" kernel wake lock
#   stay-awake.sh off    release it
#
# Uses /sys/power/wake_lock, which is independent of NextUI's own screen-off and sleep timers.
# Exit 2 = wake locks not supported.

LOCK=/sys/power/wake_lock
UNLOCK=/sys/power/wake_unlock
NAME=goblin-link

[ -w "$LOCK" ] && [ -w "$UNLOCK" ] || { echo "no wake lock interface" >&2; exit 2; }

case "$1" in
on) echo "$NAME" > "$LOCK" ;;
off) echo "$NAME" > "$UNLOCK" ;;
*) echo "usage: stay-awake.sh on|off" >&2; exit 1 ;;
esac
