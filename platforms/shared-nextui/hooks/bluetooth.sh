#!/bin/sh
# Bluetooth for pairing on the TrimUI Brick (Xradio controller on ttyS1).
#
#   bluetooth.sh up                      radio on, hci0 up, bluetoothd running
#   bluetooth.sh advertise <adv> <scan>  program the LE advertising and scan-response data
#                                        (hex, without the length byte) and enable advertising
#   bluetooth.sh down                    stop LE advertising (the radio is left as it is:
#                                        NextUI may be using it for audio)
#
# Why "advertise" exists: on this controller BlueZ accepts the advertisement over D-Bus, but
# the kernel only sends HCI Set Parameters and Enable, never Set Advertising Data or Set Scan
# Response Data, so a phone sees a nameless device without the service. The daemon registers
# the advertisement with BlueZ as usual and then has this hook fill in the payload with
# legacy HCI commands.

hci() {
    # hcitool can exit zero even when the controller rejects a command: the last byte of
    # the event it prints is the status.
    out="$(hcitool -i hci0 cmd 0x08 "$@" 2>/dev/null)" || return 1
    [ "$(printf '%s\n' "$out" | tail -n 1 | awk '{print $NF}')" = "00" ]
}

# <hex payload> -> "<len> b0 b1 ... " padded with zeros to 31 bytes.
padded() {
    hex="$1"
    n=$((${#hex} / 2))
    [ "$n" -le 31 ] || return 1
    bytes="$(printf '%s' "$hex" | sed 's/../& /g')"
    pad=""
    i="$n"
    while [ "$i" -lt 31 ]; do pad="$pad 00"; i=$((i + 1)); done
    printf '%02x %s%s' "$n" "$bytes" "$pad"
}

case "$1" in
up)
    rfkill unblock bluetooth 2>/dev/null
    # More than one hciattach fighting over the UART leaves the controller unusable.
    if [ "$(pgrep -f 'hciattach.*ttyS1' 2>/dev/null | wc -l)" -gt 1 ]; then
        for p in $(pgrep -f 'hciattach.*ttyS1' 2>/dev/null); do kill -9 "$p" 2>/dev/null; done
        sleep 1
    fi
    if ! hciconfig hci0 >/dev/null 2>&1; then
        start-stop-daemon -S -b -x /usr/bin/hciattach -- -n ttyS1 xradio
        i=0
        while [ "$i" -lt 10 ]; do
            hciconfig hci0 >/dev/null 2>&1 && break
            sleep 1
            i=$((i + 1))
        done
    fi
    hciconfig hci0 up >/dev/null 2>&1 || { echo "no Bluetooth controller" >&2; exit 1; }
    if ! pgrep bluetoothd >/dev/null 2>&1; then
        start-stop-daemon -S -b -x "$(command -v bluetoothd)"
        sleep 2
    fi
    pgrep bluetoothd >/dev/null 2>&1 || { echo "bluetoothd did not start" >&2; exit 1; }
    ;;
advertise)
    adv="$(padded "$2")" && scan="$(padded "$3")" || { echo "payload too long" >&2; exit 1; }
    hci 0x000a 00               # data and parameters need advertising off
    # shellcheck disable=SC2086
    hci 0x0008 $adv || { echo "set advertising data failed" >&2; exit 1; }
    # shellcheck disable=SC2086
    hci 0x0009 $scan || { echo "set scan response failed" >&2; exit 1; }
    hci 0x0006 a0 00 a0 00 00 00 00 00 00 00 00 00 00 07 00 || { echo "set parameters failed" >&2; exit 1; }
    hci 0x000a 01 || { echo "enable advertising failed" >&2; exit 1; }
    ;;
down)
    hci 0x000a 00
    exit 0
    ;;
*)
    echo "usage: bluetooth.sh up|advertise <adv-hex> <scan-hex>|down" >&2
    exit 1
    ;;
esac
