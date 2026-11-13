#!/bin/sh
# Cross-build the Goblin Link UI for the TrimUI Brick (aarch64, glibc 2.28) with cargo-zigbuild.
#
# The device's SDL2 and ALSA shared objects (pulled from the Brick) must be available as link
# targets. Point GOBLIN_SYSROOT at a directory containing libSDL2.so and libasound.so; by default
# we use gadgets/goblin-link/sysroot/tg5040/lib (gitignored). At runtime the device uses its own
# /usr/trimui/lib via LD_LIBRARY_PATH (set by the pak launch script), so these are link-only.
#
# Usage: tools/build-device.sh [--debug]
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=aarch64-unknown-linux-gnu.2.28
SYSROOT=${GOBLIN_SYSROOT:-"$HERE/sysroot/tg5040/lib"}

if [ ! -f "$SYSROOT/libSDL2.so" ]; then
    # No copy of the handheld's SDL2 here: link against a stand-in made from its function
    # names. The result is the same app; at run time it loads the handheld's library.
    SYSROOT="$HERE/target/link-stubs"
    "$HERE/tools/make-sdl2-stub.sh" "$SYSROOT" >&2
fi

PROFILE_FLAG=--release
PROFILE_DIR=release
if [ "${1:-}" = "--debug" ]; then
    PROFILE_FLAG=
    PROFILE_DIR=debug
fi

export RUSTFLAGS="-L native=$SYSROOT ${RUSTFLAGS:-}"
# SDL2 is linked by name (libSDL2.so); the sysroot provides the link target.
export SDL2_NO_PKG_CONFIG=1

cd "$HERE"
# shellcheck disable=SC2086
cargo zigbuild $PROFILE_FLAG --target "$TARGET" -p goblin-ui

# cargo-zigbuild drops the glibc suffix from the output directory name.
OUT="$HERE/target/aarch64-unknown-linux-gnu/$PROFILE_DIR/goblin-ui"
echo "built: $OUT"
ls -la "$OUT"
file "$OUT" 2>/dev/null || true
