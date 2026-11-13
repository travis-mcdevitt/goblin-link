#!/bin/sh
# Vibration pulse for the TrimUI Brick (NextUI).
#
#   rumble.sh <milliseconds> <strength 0-100>
#
# NextUI's keymon drives the motor with gpio227 (1 = on) and writes the supply level to
# /sys/class/motor/voltage (the strings it contains use 900000 and 1500000 microvolts).
# Strength maps linearly onto 900000..1500000. The previous voltage is restored afterwards.
# The caller's time limit must exceed the pulse length (platform.toml sets 5 s for 2 s max).
# Exit 2 = no motor.

GPIO=/sys/class/gpio/gpio227
VOLT=/sys/class/motor/voltage

case "$1" in '' | *[!0-9]*) echo "milliseconds required" >&2; exit 1 ;; esac
case "$2" in '' | *[!0-9]*) echo "strength required" >&2; exit 1 ;; esac
[ "$1" -gt 0 ] && [ "$1" -le 2000 ] || { echo "ms must be 1-2000" >&2; exit 1; }
[ "$2" -le 100 ] || { echo "strength must be 0-100" >&2; exit 1; }

[ -w "$GPIO/value" ] && [ -w "$VOLT" ] || { echo "no motor interface" >&2; exit 2; }

previous=$(cat "$VOLT")
level=$((900000 + $2 * 6000))
secs=$(($1 / 1000))
millis=$(($1 % 1000))

cleanup() {
    echo 0 > "$GPIO/value"
    echo "$previous" > "$VOLT"
}
trap cleanup EXIT HUP INT TERM

echo "$level" > "$VOLT"
echo 1 > "$GPIO/value"
sleep "$(printf '%d.%03d' "$secs" "$millis")"
