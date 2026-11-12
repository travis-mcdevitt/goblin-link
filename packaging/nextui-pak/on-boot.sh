#!/bin/sh
# Goblin Link.pak on-boot.sh: start the Muse daemon when the handheld starts, so it is connected to
# Muse without the app being opened. Run from NextUI's auto.sh; the line is added and removed
# by the "Connect when the handheld starts" switch in Settings (hooks/autostart.sh).
PAK_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
. "$PAK_DIR/start-daemon.sh"
if running; then
    echo "$(date '+%Y-%m-%d %H:%M:%S') on-boot: goblind running (pid $(cat "$PIDFILE" 2>/dev/null))" >>"$LOGS/on-boot.log"
else
    echo "$(date '+%Y-%m-%d %H:%M:%S') on-boot: goblind did not start" >>"$LOGS/on-boot.log"
fi
