#!/bin/sh
# Start-on-boot for the TrimUI Brick (NextUI) through the user's auto.sh.
#
#   autostart.sh enable     add the boot line
#   autostart.sh disable    remove it
#   autostart.sh status     print "enabled" or "disabled"
#
# NextUI runs $USERDATA_PATH/auto.sh at boot. The boot line is tagged with a marker comment so
# it can be found and removed, the same convention the Tailscale and MuseGadget paks use. It
# runs the pak's on-boot.sh (three directories up from this hook), which starts the daemon.
# Outside a pak there is no on-boot.sh and "enable" fails, so the switch reports unavailable.
# GOBLIN_AUTO_SH overrides the auto.sh location and GOBLIN_BOOT_SH the boot script (for testing).

AUTO="${GOBLIN_AUTO_SH:-/mnt/SDCARD/.userdata/tg5040/auto.sh}"
HOOKS="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
BOOT="${GOBLIN_BOOT_SH:-$(CDPATH='' cd -- "$HOOKS/../../.." && pwd)/on-boot.sh}"
MARKER="# goblin-link-on-boot"

is_enabled() {
    [ -f "$AUTO" ] && grep -qF -- "$MARKER" "$AUTO"
}

case "$1" in
status)
    if is_enabled; then echo enabled; else echo disabled; fi
    ;;
enable)
    is_enabled && exit 0
    [ -x "$BOOT" ] || { echo "no boot script at $BOOT" >&2; exit 1; }
    if [ ! -f "$AUTO" ]; then
        printf '#!/bin/sh\n\n' > "$AUTO" || exit 1
    fi
    # Make sure the file ends in a newline before appending.
    # (Command substitution drops a trailing newline, so a non-empty result means none.)
    if [ -n "$(tail -c 1 "$AUTO")" ]; then
        printf '\n' >> "$AUTO"
    fi
    printf 'test -x "%s" && "%s" & %s\n' "$BOOT" "$BOOT" "$MARKER" >> "$AUTO"
    ;;
disable)
    is_enabled || exit 0
    tmp="$AUTO.goblin-tmp.$$"
    grep -vF -- "$MARKER" "$AUTO" > "$tmp"
    # grep exits 1 when nothing is left; that is fine, any other status is an error.
    [ $? -le 1 ] || { rm -f "$tmp"; exit 1; }
    cat "$tmp" > "$AUTO" && rm -f "$tmp"
    ;;
*)
    echo "usage: autostart.sh enable|disable|status" >&2
    exit 1
    ;;
esac
