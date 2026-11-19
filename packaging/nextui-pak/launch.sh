#!/bin/sh
# Goblin Link.pak launch.sh (NextUI, tg5040): make sure the Muse daemon is running, then open the app.
#
# NextUI hands this script the screen and returns to its menu when it exits. The daemon (goblind)
# keeps running after the app closes so the handheld stays connected to Muse.
PAK_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
. "$PAK_DIR/start-daemon.sh"
# Start on boot was a line in the shared auto.sh before NextUI ran boot hooks; move it to our
# own hook, or remove a hook or line this install was not asked for (left by an uninstalled
# copy). The adapters share one autostart.sh, so the first copy will do.
for h in "$PAK_DIR"/platforms/*/hooks/autostart.sh; do
    [ -f "$h" ] && sh "$h" migrate >/dev/null 2>&1
    break
done

# Debug: with $DATA/debug-input present, log raw controller events (with times, and the
# presses and repeats made from them) to goblin-ui.log; with $DATA/debug-stats, frame times.
[ -f "$DATA/debug-input" ] && export GOBLIN_UI_INPUT_LOG=1
[ -f "$DATA/debug-stats" ] && export GOBLIN_UI_STATS=1

exec "$PAK_DIR/bin/goblin-nextui-power" "$PAK_DIR/bin/goblin-ui" --live --socket "$RUN/active" >>"$LOGS/goblin-ui.log" 2>&1
