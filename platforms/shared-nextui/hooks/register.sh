#!/bin/sh
# Register or remove a launcher entry with NextUI.
#
#   register.sh add game|tool <name> <command>
#   register.sh remove game|tool <name>
#
# Both games and apps use standalone Paks; PortMaster is optional.
# tool/game: creates "$GOBLIN_TOOLS_DIR/<name>.pak/" with launch.sh and pak.json, which NextUI lists
#       under Tools.
# <command> is an absolute path to an executable; the entry captures its output and exit status. Names are
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
tool|game) ;;
*) usage ;;
esac
pak="$TOOLS/$name.pak"
script="$pak/launch.sh"
legacy="$GAMES/Ports (PORTS)/$name.sh"

owned() {
    [ ! -L "$script" ] && [ ! -L "$pak" ] && [ -f "$script" ] && grep -qxF -- "$MARKER" "$script"
}

same_kind() {
    grep -qxF -- "# goblin-link-kind: $kind" "$script" ||
        { [ "$kind" = tool ] && ! grep -q '^# goblin-link-kind:' "$script"; }
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
    if [ -L "$pak" ] || [ -L "$script" ] || { [ -e "$pak" ] && ! owned; }; then
        echo "refusing to overwrite an entry not created by goblin-link" >&2
        exit 1
    fi
    if owned && ! same_kind && ! grep -qxF -- "    \"$command\"" "$script" && ! grep -qxF -- "exec \"$command\"" "$script"; then
        echo "refusing to replace a different kind of build" >&2
        exit 1
    fi
    mkdir -p "$pak" || exit 1
    # The installed entry point owns its working directory and launch diagnostics.
    # Drain output after the limit so a noisy game neither fills storage nor gets SIGPIPE.
    cat > "$script" <<EOF
#!/bin/sh
$MARKER
# goblin-link-kind: $kind
cd "$(dirname "$command")" || exit 1
umask 077
rm -f .goblin-launch.log .goblin-launch.status
printf 'running\n' > .goblin-launch.status
(
    "$command"
    result=\$?
    printf '%s\n' "\$result" > .goblin-launch.status
) 2>&1 | { head -c 65536 > .goblin-launch.log; cat > /dev/null; }
EOF
    chmod +x "$script" || exit 1
    printf '{"name":"%s","launch":"launch.sh","platforms":["tg5040"],"type":"TOOL","version":"0.1.0"}\n' \
        "$name" > "$pak/pak.json" || exit 1
    # Remove only our old Ports entry, after the replacement exists.
    if [ "$kind" = game ] && [ -f "$legacy" ] && grep -qxF -- "$MARKER" "$legacy"; then
        rm -f "$legacy" || exit 1
    fi
    ;;
remove)
    if owned && same_kind; then
        rm -f "$script" "$pak/pak.json"
        rmdir "$pak" 2>/dev/null || true
    fi
    if [ "$kind" = game ] && [ -f "$legacy" ] && grep -qxF -- "$MARKER" "$legacy"; then
        rm -f "$legacy"
    fi
    exit 0
    ;;
*) usage ;;
esac
