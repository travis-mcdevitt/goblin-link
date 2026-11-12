#!/bin/sh
# Goblin Link.pak launch.sh (NextUI, tg5040): make sure the Muse daemon is running, then open the app.
#
# NextUI hands this script the screen and returns to its menu when it exits. The daemon (goblind)
# keeps running after the app closes so the handheld stays connected to Muse.
PAK_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
. "$PAK_DIR/start-daemon.sh"

# Debug: with $DATA/debug-input present, log raw controller events (with times, and the
# presses and repeats made from them) to goblin-ui.log; with $DATA/debug-stats, frame times.
[ -f "$DATA/debug-input" ] && export GOBLIN_UI_INPUT_LOG=1
[ -f "$DATA/debug-stats" ] && export GOBLIN_UI_STATS=1

exec "$PAK_DIR/bin/goblin-ui" --live --socket "$RUN/active" >>"$LOGS/goblin-ui.log" 2>&1
