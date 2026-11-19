#!/bin/sh
# Start-on-boot for NextUI handhelds (tg5040).
#
#   autostart.sh enable     start the daemon at boot
#   autostart.sh disable    stop doing that
#   autostart.sh status     print "enabled" or "disabled"
#   autostart.sh migrate    move an old auto.sh line to a boot hook, if NextUI runs them, and
#                           remove a hook or line this install was never asked for
#
# NextUI 6.11.0 and later run every script in $USERDATA_PATH/.hooks/boot.d at boot and ask
# each pak to manage its own file there instead of sharing auto.sh. We install
# boot.d/goblin-link.sh, which runs the pak's on-boot.sh (three directories up from this
# hook) in the background: NextUI waits for boot hooks before showing its menu, and drops
# their output, so on-boot.sh logs to its own file. A store uninstall deletes only the pak
# folder, so the hook does nothing once on-boot.sh is gone.
#
# The launcher that runs boot hooks is $SYSTEM_PATH/bin/run_hooks.sh; NextUI before 6.11.0
# has no such file and only runs auto.sh, so there we keep a marker-tagged line in auto.sh
# (the convention the Tailscale and MuseGadget paks use). Every other line of auto.sh is
# left as it is. Outside a pak there is no on-boot.sh and "enable" fails, so the switch
# reports unavailable.
#
# The hook and the auto.sh line live outside the pak, so they outlive an uninstall, and a
# reinstall to the same path would bring the old choice back. So "enable" also writes
# .start-on-boot next to on-boot.sh, inside the pak: a store update unpacks over the pak and
# keeps it, an uninstall deletes it with the pak, and the release zip never has it. on-boot.sh
# starts nothing without it, and a hook or line without it counts as off. An opt-in from
# before this file existed cannot be told apart from a reinstall, so it has to be turned on
# once more.
#
# For testing: GOBLIN_AUTO_SH overrides auto.sh, GOBLIN_BOOT_HOOK the hook file,
# GOBLIN_HOOK_RUNNER NextUI's run_hooks.sh and GOBLIN_BOOT_SH the boot script.

AUTO="${GOBLIN_AUTO_SH:-/mnt/SDCARD/.userdata/tg5040/auto.sh}"
HOOK="${GOBLIN_BOOT_HOOK:-/mnt/SDCARD/.userdata/tg5040/.hooks/boot.d/goblin-link.sh}"
RUNNER="${GOBLIN_HOOK_RUNNER:-/mnt/SDCARD/.system/tg5040/bin/run_hooks.sh}"
HOOKS="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
BOOT="${GOBLIN_BOOT_SH:-$(CDPATH='' cd -- "$HOOKS/../../.." && pwd)/on-boot.sh}"
MARKER="# goblin-link-on-boot"
CONSENT="$(dirname -- "$BOOT")/.start-on-boot"

hooks_supported() {
    [ -f "$RUNNER" ]
}

in_auto() {
    [ -f "$AUTO" ] && grep -qF -- "$MARKER" "$AUTO"
}

in_hook() {
    [ -f "$HOOK" ] && grep -qF -- "$MARKER" "$HOOK"
}

# Remove our tagged line from auto.sh, leaving the rest byte for byte.
auto_remove() {
    in_auto || return 0
    tmp="$AUTO.goblin-tmp.$$"
    grep -vF -- "$MARKER" "$AUTO" > "$tmp"
    # grep exits 1 when nothing is left; that is fine, any other status is an error.
    [ $? -le 1 ] || { rm -f "$tmp"; return 1; }
    cat "$tmp" > "$AUTO" && rm -f "$tmp"
}

auto_add() {
    in_auto && return 0
    if [ ! -f "$AUTO" ]; then
        printf '#!/bin/sh\n\n' > "$AUTO" || return 1
    fi
    # Make sure the file ends in a newline before appending.
    # (Command substitution drops a trailing newline, so a non-empty result means none.)
    if [ -n "$(tail -c 1 "$AUTO")" ]; then
        printf '\n' >> "$AUTO"
    fi
    printf 'test -x "%s" && "%s" & %s\n' "$BOOT" "$BOOT" "$MARKER" >> "$AUTO"
}

# Write the hook, replacing an older one of ours (the pak may have moved). A file of the
# same name that is not ours is left alone.
hook_add() {
    [ ! -e "$HOOK" ] || in_hook || { echo "$HOOK is not ours" >&2; return 1; }
    mkdir -p "$(dirname -- "$HOOK")" || return 1
    # Single-quoted in the hook, so any character in the pak's path is taken literally.
    quoted="'$(printf '%s' "$BOOT" | sed "s/'/'\\\\''/g")'"
    tmp="$HOOK.goblin-tmp.$$"
    {
        printf '#!/bin/sh\n%s: Goblin Link.pak, "Connect when the handheld starts".\n' "$MARKER"
        printf 'BOOT=%s\n' "$quoted"
        printf '[ -x "$BOOT" ] || exit 0\n'
        printf '"$BOOT" </dev/null >/dev/null 2>&1 &\n'
    } > "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$HOOK" || { rm -f "$tmp"; return 1; }
}

hook_remove() {
    in_hook || return 0
    rm -f "$HOOK"
}

consented() {
    [ -f "$CONSENT" ]
}

case "$1" in
status)
    if consented && { in_hook || in_auto; }; then echo enabled; else echo disabled; fi
    ;;
enable)
    [ -x "$BOOT" ] || { echo "no boot script at $BOOT" >&2; exit 1; }
    : > "$CONSENT" || exit 1
    if hooks_supported; then
        hook_add && auto_remove
    else
        auto_add
    fi || { rm -f "$CONSENT"; exit 1; }
    ;;
disable)
    # The consent goes first: should removing the hook fail, the next boot still starts nothing.
    rm -f "$CONSENT"
    hook_remove && auto_remove
    ;;
migrate)
    if ! consented; then
        # Left over from an earlier install, or from before the consent file.
        hook_remove && auto_remove
    elif hooks_supported && in_auto && [ -x "$BOOT" ]; then
        # Only a line we added earlier is moved; with none there, nothing is touched.
        hook_add && auto_remove
    fi
    ;;
*)
    echo "usage: autostart.sh enable|disable|status|migrate" >&2
    exit 1
    ;;
esac
