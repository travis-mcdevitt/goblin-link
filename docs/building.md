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

The Brick's own SDL2 is used at run time, so the build only needs it to link
against. With a Brick connected over `adb`:

```sh
cargo install cargo-zigbuild        # and install zig
mkdir -p sysroot/tg5040/lib
adb pull /usr/trimui/lib/libSDL2-2.0.so.0 sysroot/tg5040/lib/
ln -sf libSDL2-2.0.so.0 sysroot/tg5040/lib/libSDL2.so

tools/build-device.sh                                           # goblin-ui
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.28 -p goblind -p goblinctl
tools/package-pak.sh                                            # target/pak/Goblin Link.pak
tools/package-pak.sh --install <adb serial>                     # or copy the folder yourself
```

`tools/package-pak.sh --zip` also writes `target/pak/Goblin.Link.pak.zip`, the release
archive in the layout NextUI's Pak Store installs: the pak's contents at the top level, with
`pak.json`.

Without a Brick to copy SDL2 from, `tools/build-device.sh` links against a stand-in made from
SDL2's function names (`tools/make-sdl2-stub.sh`). The app is the same either way: it loads
the handheld's own SDL2 when it runs. CI builds it this way (`tools/ci-build-pak.sh`) for
every pull request and every push to `main`, and keeps the zip with the run for 14 days.

## Releases

The **Release** workflow (`.github/workflows/release.yml`) makes every release. Start it
from `main` under Actions with a full commit SHA and a channel, `dev` or `stable`. It refuses
a commit whose latest CI run did not pass, and every run while the `release` environment has
no required reviewer. It builds the pak as CI does and publishes only after the owner approves
its publish job, which runs in that environment. Each release carries `Goblin.Link.pak.zip`
and `Goblin.Link.pak.zip.sha256`; its notes name the commit, and `hermes-plugin/` from that
commit is the plugin that matches. The rules are in `tools/release.py`, tested by
`tools/test-release.py`. The environment guards only this workflow: anyone with write access
can still tag or release by hand, which immutable releases and a tag ruleset for `v*` would
stop.

**dev**, for testers: any commit on `main`. Its version is `main`'s stable version plus
`-dev.N`, N one more than the highest such tag: `v0.1.1-dev.1`, then `v0.1.1-dev.2`. The
build's own copy of `Cargo.toml` and `pak.json` says that version, so Settings shows it;
nothing is committed. It is a pre-release, never marked latest, so the README's links and
the store do not change.

**stable**:

1. Open a release PR that sets the workspace version in `Cargo.toml` and `Cargo.lock`, the
   same version prefixed with `v` in `pak.json`, and its `changelog` entry there. That entry
   opens the release notes.
2. When it is reviewed, contains `main`'s latest commit and its CI has passed, run the
   workflow on the PR's head commit with `stable`. It refuses unless `pak.json` names
   `v<Cargo.toml version>`, the version is plain `vX.Y.Z`, above the one `main` names, and
   its tag does not exist yet. The release is tagged at that commit and marked latest.
3. Merge the PR once the release exists.

Pak Store compares versions as dot-separated numbers and reads a suffix such as `-dev.N` as a
fourth number, so `v0.1.1-dev.2` sorts above `v0.1.1` and below `v0.1.2`. That is why a dev
build carries the current stable version: testers are offered the next stable release over
it, where a `v0.1.2-dev.N` would sort above `v0.1.2` and strand them. Only plain `vX.Y.Z`
versions go to the store, and `pak.json` on `main` changes only in a release PR whose
release already exists: the store reads it from `main` every hour and builds the download
link from its version.

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
