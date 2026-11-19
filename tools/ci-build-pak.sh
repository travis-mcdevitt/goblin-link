#!/bin/sh
# ci-build-pak.sh: build target/pak/Goblin.Link.pak.zip on a GitHub runner, for CI's pak job
# and the release workflow alike, so the two cannot drift apart. It downloads zig and
# installs cargo-zigbuild, so it is meant for a throwaway runner, not a developer's machine.
# It links against the SDL2 stand-in, never a handheld's libraries.
set -eu
ZIG_VERSION=0.17.1
ZIG_SHA256=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a
CARGO_ZIGBUILD_VERSION=0.24.1

HERE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=${RUNNER_TEMP:?run this on a GitHub runner}

archive="zig-x86_64-linux-$ZIG_VERSION.tar.xz"
curl -fsSL --retry 3 -o "$TMP/$archive" "https://ziglang.org/download/$ZIG_VERSION/$archive"
echo "$ZIG_SHA256  $TMP/$archive" | sha256sum -c -
tar -xJf "$TMP/$archive" -C "$TMP"
PATH="$TMP/zig-x86_64-linux-$ZIG_VERSION:$PATH"
export PATH

cargo install --locked --version "$CARGO_ZIGBUILD_VERSION" cargo-zigbuild

cd "$HERE"
# A folder that does not exist, so build-device.sh always uses the SDL2 stand-in.
GOBLIN_SYSROOT="$TMP/no-handheld-libraries" tools/build-device.sh
cargo zigbuild --locked --release --target aarch64-unknown-linux-gnu.2.28 -p goblind -p goblinctl
tools/package-pak.sh --zip
