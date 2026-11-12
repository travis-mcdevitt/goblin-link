#!/bin/sh
# start-daemon.sh: make sure the Muse daemon (goblind) is running. Sourced by launch.sh (the app)
# and on-boot.sh (start on boot); it sets PAK_DIR, DATA, RUN and LOGS for them.
#
# Profiles are shared with MuseGadget.pak (same data dir, same pairing files), so pairings carry
# over. Both cannot serve the same device at once: if the Python service is running it is
# stopped first (MuseGadget.pak's own "service" toggle restarts it).
PAK_DIR="${PAK_DIR:-$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)}"
SDCARD="${SDCARD_PATH:-/mnt/SDCARD}"
# An install from before the project was called Goblin Link keeps its folders (data, the
# profiles' home folders, owner commands): they hold pairings, and moving them gains nothing.
DATA="$SDCARD/.userdata/tg5040/goblin-link"
[ -d "$DATA" ] || [ ! -d "$SDCARD/.userdata/tg5040/musegadget" ] || DATA="$SDCARD/.userdata/tg5040/musegadget"
HOMES="$SDCARD/GoblinLink"
[ -d "$HOMES" ] || [ ! -d "$SDCARD/Muse" ] || HOMES="$SDCARD/Muse"
INTERNAL=/mnt/UDISK/goblin-link
[ -d "$INTERNAL" ] || [ ! -d /mnt/UDISK/muse-gadget ] || INTERNAL=/mnt/UDISK/muse-gadget
RUN=/run/goblin-link
LOGS="$DATA/logs"
PIDFILE="$RUN/goblind.pid"
OLD_PAK="$SDCARD/Tools/tg5040/MuseGadget.pak"
# Owner commands Muse can call (commands.d). They live on the internal ext4 storage: the SD
# card is FAT, where every file is world-writable, and goblind only loads command files that
# are owned and writable by root alone. A shipped command is installed when missing and
# updated with the pak as long as the owner has not edited it (it still matches the copy
# kept in .shipped/); an edited one is left alone.
CMDS="$INTERNAL/commands.d"

mkdir -p "$LOGS" "$RUN" "$CMDS"
chmod 755 "$INTERNAL" "$CMDS"
mkdir -p "$CMDS/.shipped"
chmod 755 "$CMDS/.shipped"
for f in "$PAK_DIR"/commands.d/*.json; do
    [ -f "$f" ] || continue
    name="$(basename "$f")"
    dest="$CMDS/$name"
    shipped="$CMDS/.shipped/$name"
    sed "s#@PAK_DIR@#$PAK_DIR#g" "$f" > "$dest.new" || continue
    chmod 644 "$dest.new"
    if [ ! -e "$dest" ] || { [ -f "$shipped" ] && cmp -s "$dest" "$shipped"; }; then
        cp "$dest.new" "$shipped" && chmod 644 "$shipped"
        mv "$dest.new" "$dest"
    else
        rm -f "$dest.new"
    fi
done
export LD_LIBRARY_PATH="/usr/trimui/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

running() {
    # A daemon answering on the socket counts even if the pidfile is stale (a refused second
    # start would otherwise overwrite the pidfile with its own, dead, pid).
    "$PAK_DIR/bin/goblinctl" --socket "$RUN/active" status </dev/null >/dev/null 2>&1 && return 0
    [ -f "$PIDFILE" ] || return 1
    p="$(cat "$PIDFILE" 2>/dev/null)"
    [ -n "$p" ] && [ "$p" -gt 1 ] 2>/dev/null && kill -0 "$p" 2>/dev/null
}

if ! running; then
    # Hand the profile over from the Python service, if it is up.
    if ps | grep -q "[p]ython3 -m mus" && [ -x "$OLD_PAK/bin/mg-ctl" ]; then
        "$OLD_PAK/bin/mg-ctl" stop >/dev/null 2>&1
    fi
    rm -f "$PIDFILE"
    # Detached from this script's session so it survives the app closing.
    start-stop-daemon -S -b -m -p "$PIDFILE" -x /bin/sh -- -c \
        "RUST_LOG=info exec '$PAK_DIR/bin/goblind' --data-dir '$DATA' --runtime-dir '$RUN' --commands-dir '$CMDS' --homes-dir '$HOMES' --storage-root '$SDCARD' >>'$LOGS/goblind.log' 2>&1"
    # Wait (up to 5 s) for the IPC socket; the app also retries on its own.
    i=0
    while [ ! -S "$RUN/active" ] && [ $i -lt 25 ]; do sleep 0.2; i=$((i + 1)); done
fi
