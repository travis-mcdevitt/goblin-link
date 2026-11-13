# Building

## On a PC, to try the interface

You need Rust (1.80 or newer) and SDL2's development files.

```sh
cargo run --release -p goblin-ui            # the app with made-up data, in a window
cargo run --release -p goblin-ui -- --dark
```

Keys: arrows are the D-pad, Enter is A, Backspace is B, `A` is X, `S` is Y, `Q`/`W` are
L1/R1, `E`/`R` are L2/R2, Tab is Select, Space is Start, `M` is Menu, `F`/`G` are F1/F2.
Nothing here talks to Muse.

## For the TrimUI Brick

The Brick's own libraries are used at run time, so the build only needs them to link
against. With a Brick connected over `adb`:

```sh
cargo install cargo-zigbuild        # and install zig
mkdir -p sysroot/tg5040/lib
adb pull /usr/trimui/lib/libSDL2-2.0.so.0 sysroot/tg5040/lib/
ln -sf libSDL2-2.0.so.0 sysroot/tg5040/lib/libSDL2.so
adb pull /usr/lib/libasound.so.2 sysroot/tg5040/lib/    # wherever the firmware keeps it
ln -sf libasound.so.2 sysroot/tg5040/lib/libasound.so

tools/build-device.sh                                           # goblin-ui
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.28 -p goblind -p goblinctl
tools/package-pak.sh                                            # target/pak/Goblin Link.pak
tools/package-pak.sh --install <adb serial>                     # or copy the folder yourself
```

`tools/package-pak.sh --zip` also writes `target/pak/Goblin.Link.pak.zip`, the release
archive in the layout NextUI's Pak Store installs: the pak's contents at the top level, with
`pak.json`. Releases are owner-run: set the version in `Cargo.toml` and `pak.json` (as `vX.Y.Z`),
land the reviewed change, build the archive, then tag and push the tag
(`git tag vX.Y.Z && git push origin vX.Y.Z`). Attach `Goblin.Link.pak.zip` when creating the
GitHub release; CI checks changes but does not publish releases. The tag, the version in
`pak.json` and the file name must match exactly, or the store will not see it.

Without a Brick to copy SDL2 from, `tools/build-device.sh` links against a stand-in made from
SDL2's function names (`tools/make-sdl2-stub.sh`). The app is the same either way: it loads
the handheld's own SDL2 when it runs.

## Tests

```sh
tools/test-in-container.sh              # everything, in Docker
tools/test-in-container.sh -p goblin-ui   # one crate
```

Run them in the container, not directly: a few daemon tests start and kill processes.

## Useful switches

| Switch | What it does |
|---|---|
| `goblin-ui --screenshot <screen> out.png` | Draws one screen to a file, no window |
| `goblin-ui --benchmark` | Times drawing on the CPU |
| `goblin-ui --gpu-bench` | Says what the device's GPU renderer is and times it |
| `GOBLIN_UI_NO_GPU=1` | Draws every frame whole on the CPU |
| `GOBLIN_UI_STATS=1` | Prints frame times every five seconds |
| a file named `debug-input` in the data folder | Logs button events |
