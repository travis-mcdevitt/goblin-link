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

mkdir -p "$LOGS" "$RUN" "$CMDS" "$HOMES"
chmod 755 "$INTERNAL" "$CMDS"
mkdir -p "$CMDS/.shipped"
chmod 755 "$CMDS/.shipped"
# Command files an earlier pak shipped and this one does not. The Pak Store updates by
# unpacking over the pak folder and deletes nothing, so the old file is still there and would
# be installed again below. The installed copy goes too, unless the owner changed it: then it
# is theirs. (chat.suggest_replies: reply buttons are built in now.)
RETIRED="chat.suggest_replies.json"
for name in $RETIRED; do
    rm -f "$PAK_DIR/commands.d/$name"
    old="$CMDS/$name"
    if [ -f "$old" ] && [ -f "$CMDS/.shipped/$name" ] && cmp -s "$old" "$CMDS/.shipped/$name"; then
        rm -f "$old"
    fi
    rm -f "$CMDS/.shipped/$name"
done
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
# On a device nobody has qualified yet (anything but the Brick), or when asked to with a
# file named device-report in the data folder: write down what the device is, once, where
# the owner can find it on the card.
REPORT="$HOMES/device-report.txt"
if [ ! -f "$REPORT" ] && { [ "${DEVICE:-brick}" != brick ] || [ -f "$DATA/device-report" ]; }; then
    sh "$PAK_DIR/device-report.sh" "$REPORT" >/dev/null 2>&1
fi
export LD_LIBRARY_PATH="/usr/trimui/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

running() {
    # A daemon answering on the socket counts even if the pidfile is stale (a refused second
    # start would otherwise overwrite the pidfile with its own, dead, pid).
    "$PAK_DIR/bin/goblinctl" --socket "$RUN/active" status </dev/null >/dev/null 2>&1 && return 0
    [ -f "$PIDFILE" ] || return 1
    p="$(cat "$PIDFILE" 2>/dev/null)"
    [ -n "$p" ] && [ "$p" -gt 1 ] 2>/dev/null && kill -0 "$p" 2>/dev/null
}

# The daemon's pid from the pidfile, when that process is goblind (a pid is reused).
daemon_pid() {
    p="$(cat "$PIDFILE" 2>/dev/null)"
    [ -n "$p" ] && [ "$p" -gt 1 ] 2>/dev/null && [ "$(cat "/proc/$p/comm" 2>/dev/null)" = goblind ] && echo "$p"
}

# A daemon from another pak version (the pak was updated while it ran) is stopped so this
# pak's starts: the app talks to it, and its consent rules are the ones in force. So is one
# whose binary was replaced by a reinstall of the same version: it would stop itself a moment
# later (it watches its binary), after this script had decided not to start one.
want="$("$PAK_DIR/bin/goblind" --version </dev/null 2>/dev/null | sed -n '1s/^goblind //p')"
have="$("$PAK_DIR/bin/goblinctl" --socket "$RUN/active" status </dev/null 2>/dev/null | sed -n 's/^version  *\([^ ]*\).*/\1/p')"
replaced() {
    case "$(readlink "/proc/$1/exe" 2>/dev/null)" in *" (deleted)") return 0 ;; esac
    return 1
}
if [ -n "$want" ] && [ -n "$have" ] && p="$(daemon_pid)" && { [ "$want" != "$have" ] || replaced "$p"; }; then
    echo "$(date '+%Y-%m-%d %H:%M:%S') start-daemon: replacing goblind $have with $want" >>"$LOGS/goblind.log"
    # SIGTERM lets it write its stores and hand the LEDs back; it gets 10 s for that.
    kill "$p" 2>/dev/null
    i=0
    while kill -0 "$p" 2>/dev/null && [ $i -lt 50 ]; do sleep 0.2; i=$((i + 1)); done
    kill -0 "$p" 2>/dev/null && kill -9 "$p" 2>/dev/null
    rm -f "$PIDFILE"
fi

if ! running; then
    # Hand the profile over from the Python service, if it is up.
    if ps | grep -q "[p]ython3 -m mus" && [ -x "$OLD_PAK/bin/mg-ctl" ]; then
        "$OLD_PAK/bin/mg-ctl" stop >/dev/null 2>&1
    fi
    rm -f "$PIDFILE"
    # Detached from this script's session so it survives the app closing.
    start-stop-daemon -S -b -m -p "$PIDFILE" -x /bin/sh -- -c \
        "RUST_LOG=info exec '$PAK_DIR/bin/goblind' --data-dir '$DATA' --runtime-dir '$RUN' --commands-dir '$CMDS' --homes-dir '$HOMES' --built-dir '$INTERNAL/built' --storage-root '$SDCARD' >>'$LOGS/goblind.log' 2>&1"
    # Wait (up to 5 s) for the IPC socket; the app also retries on its own.
    i=0
    while [ ! -S "$RUN/active" ] && [ $i -lt 25 ]; do sleep 0.2; i=$((i + 1)); done
fi
