#!/bin/sh
# Register or remove a launcher entry with NextUI.
#
#   register.sh add game|tool <name> <command>
#   register.sh remove game|tool <name>
#
# tool: creates "$GOBLIN_TOOLS_DIR/<name>.pak/" with launch.sh and pak.json, which NextUI lists
#       under Tools.
# game: creates "$GOBLIN_GAMES_DIR/Ports (PORTS)/<name>.sh", which the PORTS emulator lists
#       under Ports.
# <command> is an absolute path to an executable; the entry runs it with exec. Names are
# restricted to [A-Za-z0-9 ._-] by the caller and re-checked here. Only entries carrying the
# marker line written by this script are ever removed or overwritten.

TOOLS="${GOBLIN_TOOLS_DIR:-/mnt/SDCARD/Tools/tg5040}"
GAMES="${GOBLIN_GAMES_DIR:-/mnt/SDCARD/Roms}"
MARKER="# goblin-link-entry"

usage() {
    echo "usage: register.sh add game|tool <name> <command> | remove game|tool <name>" >&2
    exit 1
}

[ $# -ge 3 ] || usage
action=$1
kind=$2
name=$3

case "$name" in
'' | .* | */* | *[!A-Za-z0-9\ ._-]*) echo "bad entry name" >&2; exit 1 ;;
esac

case "$kind" in
tool)
    pak="$TOOLS/$name.pak"
    script="$pak/launch.sh"
    ;;
game)
    pak=""
    script="$GAMES/Ports (PORTS)/$name.sh"
    ;;
*) usage ;;
esac

owned() {
    [ -f "$script" ] && grep -qF -- "$MARKER" "$script"
}

case "$action" in
add)
    [ $# -eq 4 ] || usage
    command=$4
    case "$command" in
    /*) ;;
    *) echo "command must be an absolute path" >&2; exit 1 ;;
    esac
    case "$command" in
    *\"* | *\$* | *\`* | *\\*) echo "command contains unsafe characters" >&2; exit 1 ;;
    esac
    [ -x "$command" ] || { echo "command is not executable" >&2; exit 1; }
    if [ -e "$script" ] && ! owned; then
        echo "refusing to overwrite an entry not created by goblin-link" >&2
        exit 1
    fi
    if [ "$kind" = tool ]; then
        mkdir -p "$pak" || exit 1
    else
        mkdir -p "$GAMES/Ports (PORTS)" || exit 1
    fi
    printf '#!/bin/sh\n%s\nexec "%s"\n' "$MARKER" "$command" > "$script" || exit 1
    chmod +x "$script"
    if [ "$kind" = tool ]; then
        printf '{"name":"%s","launch":"launch.sh","platforms":["tg5040"],"type":"TOOL","version":"0.1.0"}\n' \
            "$name" > "$pak/pak.json"
    fi
    ;;
remove)
    owned || exit 0
    rm -f "$script"
    if [ "$kind" = tool ]; then
        rm -f "$pak/pak.json"
        rmdir "$pak" 2>/dev/null
    fi
    exit 0
    ;;
*) usage ;;
esac
