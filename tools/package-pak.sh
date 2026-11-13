#!/bin/sh
# package-pak.sh [--install <serial> | --zip]: assemble target/pak/Goblin Link.pak from release binaries
# (aarch64, glibc 2.28) and, with --install, copy it to the Brick's Tools list side by side
# with MuseGadget.pak (which is left untouched).
set -eu
HERE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BIN="$HERE/target/aarch64-unknown-linux-gnu/release"
OUT="$HERE/target/pak/Goblin Link.pak"
for b in goblind goblin-ui goblinctl; do
    [ -f "$BIN/$b" ] || { echo "missing $BIN/$b: run tools/build-device.sh and cargo zigbuild -p goblind -p goblinctl" >&2; exit 1; }
done
rm -rf "$OUT"
mkdir -p "$OUT/bin"
cp "$HERE/packaging/nextui-pak/launch.sh" "$HERE/packaging/nextui-pak/start-daemon.sh" \
    "$HERE/packaging/nextui-pak/on-boot.sh" "$HERE/packaging/nextui-pak/device-report.sh" "$OUT/"
cp "$BIN/goblind" "$BIN/goblin-ui" "$BIN/goblinctl" "$OUT/bin/"
cp "$HERE/NOTICE" "$OUT/NOTICE"
# The Pak Store reads pak.json from the repository and expects it in the pak too. Its
# version is the release tag, so it must be the version the binaries were built as.
cp "$HERE/pak.json" "$OUT/pak.json"
VERSION="v$(sed -n 's/^version = "\(.*\)"/\1/p' "$HERE/Cargo.toml" | head -1)"
grep -q "\"version\": \"$VERSION\"" "$HERE/pak.json" || { echo "pak.json does not say version $VERSION (Cargo.toml)" >&2; exit 1; }
cp -r "$HERE/packaging/nextui-pak/commands.d" "$OUT/commands.d"
# Platform adapters: goblind finds them at <exe>/../platforms.
mkdir -p "$OUT/platforms"
# Materialize shared source hooks: FAT cards and pak extractors need ordinary files.
for p in "$HERE"/platforms/*/platform.toml; do
    cp -rL "${p%/platform.toml}" "$OUT/platforms/"
done
chmod 755 "$OUT/launch.sh" "$OUT/start-daemon.sh" "$OUT/on-boot.sh" "$OUT/device-report.sh" "$OUT/bin/"* "$OUT/platforms/"*/hooks/*.sh 2>/dev/null || true
(cd "$OUT" && find . -type f ! -name SHA256SUMS -exec sha256sum {} + | sort -k2 > SHA256SUMS)
du -sh "$OUT"
if [ "${1:-}" = "--zip" ]; then
    # A release archive as the Pak Store wants it: the pak's own contents at the top level
    # (no folder around them), named as pak.json's release_filename says.
    ZIP="$HERE/target/pak/Goblin.Link.pak.zip"
    rm -f "$ZIP"
    (cd "$OUT" && {
        if command -v zip >/dev/null 2>&1; then
            zip -q -r -X "$ZIP" . -x '.DS_Store' -x '__MACOSX/*'
        else
            python3 -c "import shutil, sys; shutil.make_archive(sys.argv[1][:-4], 'zip', '.')" "$ZIP"
        fi
    })
    ls -la "$ZIP"
fi
if [ "${1:-}" = "--install" ]; then
    SERIAL="$2"
    DEST="/mnt/SDCARD/Tools/tg5040/Goblin Link.pak"
    adb -s "$SERIAL" shell "rm -rf '$DEST.new' && mkdir -p '$DEST.new'" </dev/null
    adb -s "$SERIAL" push "$OUT/." "$DEST.new/" >/dev/null
    adb -s "$SERIAL" shell "cd '$DEST.new' && sha256sum -c SHA256SUMS >/dev/null && chmod 755 launch.sh start-daemon.sh on-boot.sh device-report.sh bin/* platforms/*/hooks/*.sh && rm -rf '$DEST' && mv '$DEST.new' '$DEST' && echo 'installed $DEST'" </dev/null
fi
