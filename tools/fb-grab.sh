#!/bin/sh
# fb-grab.sh <serial> <out.png> -- capture what the Brick's display currently shows.
# /dev/fb0 is 1024x16384 (stacked 1024x768 pages), 32 bpp BGRA, stride 4096; the page on screen
# is at the y offset in /sys/class/graphics/fb0/pan. Read-only: nothing on the device changes.
set -eu
SERIAL="$1"; OUT="$2"
TMP=$(mktemp)
trap 'rm -f "$TMP"' EXIT
# This adbd has no exec-out; stage the page in /tmp on the device and pull it.
adb -s "$SERIAL" shell 'Y=$(cut -d, -f2 /sys/class/graphics/fb0/pan); dd if=/dev/fb0 of=/tmp/fbgrab.raw bs=4096 skip=$Y count=768 2>/dev/null' </dev/null
adb -s "$SERIAL" pull /tmp/fbgrab.raw "$TMP" >/dev/null
adb -s "$SERIAL" shell 'rm -f /tmp/fbgrab.raw' </dev/null
python3 - "$TMP" "$OUT" <<'PY'
import sys
from PIL import Image
raw = open(sys.argv[1], "rb").read()
assert len(raw) == 4096 * 768, len(raw)
Image.frombuffer("RGBA", (1024, 768), raw, "raw", "BGRA", 4096, 1).convert("RGB").save(sys.argv[2])
PY
echo "$OUT"
