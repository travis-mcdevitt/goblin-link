#!/bin/sh
# Built-in microphone gain for the TrimUI Brick (NextUI).
#
#   mic-gain.sh get         print the MIC1 gain in dB (integer, 0-31)
#   mic-gain.sh set <db>    set it
#
# MIC1 is capture channel 0 of hw:0,0 (the "left" channel in platform.toml). The Voice Notes
# pak runs it at +12 dB; +24 dB clipped louder speech in its calibration. MIC2 (channel 1) is
# left alone. The caller restores the previous value (Platform::set_mic_gain returns a guard).

CTL="MIC1 gain volume"

case "$1" in
get)
    amixer cget name="$CTL" 2>/dev/null | sed -n 's/.*: values=\([0-9-]*\).*/\1/p' | head -n 1
    ;;
set)
    case "$2" in
    '' | *[!0-9]*) echo "gain must be an integer 0-31" >&2; exit 1 ;;
    esac
    [ "$2" -le 31 ] || { echo "gain must be 0-31" >&2; exit 1; }
    amixer cset name="$CTL" "$2" >/dev/null
    ;;
*)
    echo "usage: mic-gain.sh get|set <db>" >&2
    exit 1
    ;;
esac
