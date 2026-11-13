#!/bin/sh
# make-sdl2-stub.sh <dir>: write a stand-in libSDL2 into <dir>, good only for linking.
#
# The app uses the handheld's own SDL2 at run time; to build it, the linker needs a library
# of that name that defines the functions. This makes one from the list of SDL2's function
# names (packaging/link-stubs/SDL2.symbols): every function is empty. Nothing from it ends up
# in the app, which only records "needs libSDL2-2.0.so.0" and the names it calls. It lets the
# app be built where there is no handheld to copy the real library from (CI, a release).
set -eu
HERE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUT="${1:?usage: tools/make-sdl2-stub.sh <dir>}"
TARGET="${GOBLIN_STUB_TARGET:-aarch64-linux-gnu.2.28}"
mkdir -p "$OUT"
SRC="$OUT/sdl2-stub.c"
sed 's/.*/void &(void) {}/' "$HERE/packaging/link-stubs/SDL2.symbols" > "$SRC"
zig cc -target "$TARGET" -shared -nostdlib -Wl,-soname,libSDL2-2.0.so.0 -o "$OUT/libSDL2-2.0.so.0" "$SRC"
ln -sf libSDL2-2.0.so.0 "$OUT/libSDL2.so"
rm -f "$SRC"
echo "stub SDL2 for linking: $OUT/libSDL2.so"
