#!/bin/sh
# Goblin Link.pak on-boot.sh: start the Muse daemon when the handheld starts, so it is connected to
# Muse without the app being opened. Run from NextUI's boot hook (.hooks/boot.d/goblin-link.sh),
# or from auto.sh on NextUI before 6.11.0; the "Connect when the handheld starts" switch in
# Settings adds and removes it (hooks/autostart.sh).
PAK_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
# Only when start on boot was turned on for this install (hooks/autostart.sh writes the file): a
# hook left by an uninstalled copy must not start a reinstalled one. Opening the app removes
# such a hook; it is not done here because auto.sh may still be running the line that ran us.
[ -f "$PAK_DIR/.start-on-boot" ] || exit 0
. "$PAK_DIR/start-daemon.sh"
# start-daemon.sh returns once the socket file is there or after 5 s, which can be before the
# daemon answers or has written its pidfile. Give it up to 10 s more, so the log tells the truth.
i=0
while ! answering && [ $i -lt 50 ]; do sleep 0.2; i=$((i + 1)); done
if running; then
    echo "$(date '+%Y-%m-%d %H:%M:%S') on-boot: goblind running (pid $(cat "$PIDFILE" 2>/dev/null))" >>"$LOGS/on-boot.log"
else
    echo "$(date '+%Y-%m-%d %H:%M:%S') on-boot: goblind did not start" >>"$LOGS/on-boot.log"
fi
