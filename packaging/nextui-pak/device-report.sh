#!/bin/sh
# device-report.sh <out-file>: write down what this handheld is, for qualifying Goblin Link
# on a device nobody has tried it on. Read-only. Serial numbers, hardware addresses and
# network names are left out or blanked; read the file before sharing it all the same.
OUT="${1:?usage: device-report.sh <out-file>}"
blank() { sed -E 's/([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}/xx:xx:xx:xx:xx:xx/g; s/(serial|Serial|SERIAL)[^:=]*[:=].*/\1: (left out)/'; }
{
    echo "Goblin Link device report"
    echo "== NextUI"
    echo "PLATFORM=${PLATFORM:-} DEVICE=${DEVICE:-} TRIMUI_MODEL=${TRIMUI_MODEL:-}"
    [ -f /usr/trimui/bin/MainUI ] && strings /usr/trimui/bin/MainUI 2>/dev/null | grep "^Trimui" | head -3
    head -1 "${SDCARD_PATH:-/mnt/SDCARD}/.system/version.txt" 2>/dev/null
    cat /etc/version 2>/dev/null | head -2
    echo "== system"
    uname -srm
    cat /proc/device-tree/model 2>/dev/null | tr '\0' '\n'
    grep -m1 -i "CPU part" /proc/cpuinfo; grep -c "^processor" /proc/cpuinfo
    grep -i "MemTotal" /proc/meminfo
    echo "== screen"
    cat /sys/class/graphics/fb0/virtual_size /sys/class/graphics/fb0/bits_per_pixel 2>/dev/null
    grep -i "lcd output\|hdmi" /sys/class/disp/disp/attr/sys 2>/dev/null
    echo "== input devices"
    grep -E "^N: |^H: " /proc/bus/input/devices 2>/dev/null
    echo "== sound"
    cat /proc/asound/cards 2>/dev/null
    ls /dev/snd 2>/dev/null | tr '\n' ' '; echo
    amixer scontrols 2>/dev/null | head -40
    ls /sys/class/speaker 2>/dev/null | tr '\n' ' '; echo
    echo "== lights"
    ls /sys/class/led_anim 2>/dev/null | tr '\n' ' '; echo
    echo "== vibration"
    ls -d /sys/class/gpio/gpio227 /sys/class/motor 2>&1 | tr '\n' ' '; echo
    cat /sys/class/motor/voltage 2>/dev/null
    echo "== radios"
    ls /sys/class/net 2>/dev/null | tr '\n' ' '; echo
    ls /sys/class/bluetooth 2>/dev/null | tr '\n' ' '; echo
    grep -h -i "hciattach\|insmod\|modprobe\|rtk\|aic\|xradio\|bcm" /etc/bluetooth/bt_init.sh 2>/dev/null | head -12
    lsmod 2>/dev/null | awk '{print $1}' | grep -i "xr\|aic\|rtl\|8[0-9][0-9][0-9]\|bcm\|bt\|wlan" | tr '\n' ' '; echo
    bluetoothd --version 2>/dev/null
    echo "== usb"
    ls /sys/class/udc 2>/dev/null | tr '\n' ' '; echo
    ls /sys/bus/usb/devices 2>/dev/null | tr '\n' ' '; echo
    echo "== power"
    ls /sys/class/power_supply 2>/dev/null | tr '\n' ' '; echo
    ls /sys/class/thermal 2>/dev/null | tr '\n' ' '; echo
    echo "== storage"
    grep -E "SDCARD|UDISK| / " /proc/mounts | awk '{print $1, $2, $3}'
    echo "== libraries"
    ls /usr/trimui/lib 2>/dev/null | grep -i "sdl\|asound" | tr '\n' ' '; echo
    echo "== shell"
    ls -l /bin/sh 2>/dev/null | awk '{print $NF}'
    for t in aplay arecord amixer hciattach hciconfig bluetoothctl start-stop-daemon strings; do command -v "$t" >/dev/null 2>&1 && printf '%s ' "$t"; done; echo
} 2>&1 | blank > "$OUT"
echo "wrote $OUT"
