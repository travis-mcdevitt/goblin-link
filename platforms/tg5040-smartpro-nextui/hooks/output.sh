#!/bin/sh
# Speaker output control for the TrimUI Brick (NextUI).
#
#   output.sh volume-get        print the playback volume, 0-100
#   output.sh volume-set <pct>  set it (0-100)
#   output.sh mute-get          print 1 when the speaker is muted, 0 when not
#   output.sh mute-set <0|1>    mute (1) or unmute (0)
#
# Volume is the ALSA softvol control "Soft Volume Master" that the system's default PCM
# (/etc/asound.conf) applies in front of dmix, using amixer's mapped percentage like NextUI.
# The speaker amplifier is gated by /sys/class/speaker/mute (1 = muted); NextUI leaves it at 1
# when idle, so callers must unmute for sound and restore the previous value afterwards.
# Exit 2 = control not present.

MUTE=/sys/class/speaker/mute
CTL="Soft Volume Master"

softvol_present() {
    amixer cget name="$CTL" >/dev/null 2>&1
}

# The control is created the first time the default PCM is opened. Opening it with silence
# creates it without making sound.
ensure_softvol() {
    softvol_present && return 0
    aplay -q -D default -f S16_LE -r 48000 -c 2 -d 1 /dev/zero >/dev/null 2>&1
    softvol_present
}

case "$1" in
volume-get)
    ensure_softvol || { echo "no $CTL control" >&2; exit 2; }
    amixer -M sget "$CTL" | sed -n 's/.*\[\([0-9]*\)%\].*/\1/p' | head -n 1
    ;;
volume-set)
    case "$2" in
    '' | *[!0-9]*) echo "volume must be 0-100" >&2; exit 1 ;;
    esac
    [ "$2" -le 100 ] || { echo "volume must be 0-100" >&2; exit 1; }
    ensure_softvol || { echo "no $CTL control" >&2; exit 2; }
    amixer -M sset "$CTL" "$2%" >/dev/null
    ;;
mute-get)
    [ -r "$MUTE" ] || exit 2
    cat "$MUTE"
    ;;
mute-set)
    case "$2" in
    0 | 1) ;;
    *) echo "mute must be 0 or 1" >&2; exit 1 ;;
    esac
    [ -w "$MUTE" ] || exit 2
    echo "$2" > "$MUTE"
    ;;
*)
    echo "usage: output.sh volume-get|volume-set <pct>|mute-get|mute-set <0|1>" >&2
    exit 1
    ;;
esac
