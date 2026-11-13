# Platform adapters

One directory per platform: `platforms/<id>/platform.toml` plus `hooks/*.sh`. The loader is the
`goblin-platform` crate; `goblinctl platform ...` exercises it. A section missing from the file means the
capability is unavailable and the related API returns `Unavailable` (CLI exit status 3).

The three NextUI adapters link their `hooks` directories to `shared-nextui/hooks` in source.
Keep identical hooks there; a device that needs different behavior can use its own directory.
Packaging copies only adapters with `platform.toml` and follows these links, so paks contain
ordinary files at each device's existing hook paths, including on FAT cards.

## Search order

`GOBLIN_PLATFORMS_DIR`, else `../platforms`, `../share/goblin-link/platforms` or `./platforms` next to the
executable, else `./platforms`. `GOBLIN_PLATFORM=<id>` forces a platform. The directory name must equal `id`.

## platform.toml

Unknown keys are errors. Sections:

| Section | Keys (defaults) |
|---|---|
| top level | `id` (required, `[a-z0-9-]+`), `name`, `model` (the product's name) |
| `[os]` | `name`, `version_file` (its first line is the installed version), `base`, `repository` (where its source and documentation are, in full) |
| `[hardware]` | `touch`, `keyboard`, `wifi`, `bluetooth`, `headphone_jack`, `video_out`, `motion_sensors`, `gps`, `camera` (all `false`; what a device lacks is told as plainly as what it has), `cpu`, `gpu`, `usb_host`, `usb_device` (a phrase each) |
| `[software]` | `package_database`, `library_dirs` (folders that are the same on every unit; their library names are reported), `[[software.places]]` with `path` and `what`. Pointers for the agent to follow with `file.list` and `file.read`, not an inventory |
| `[os_conventions]` | Any keys, one plain sentence each: what an owner of this firmware expects everywhere (volume, mute, what closes an app, sleep). Facts, not advice on how to build. Told to the agent by `handheld.about`. State only what you have seen; point to the firmware's documentation for the rest |
| `[detect]` | `env = {K="V"}` (all must match), `device_tree_model_contains`, `fallback = false`. A platform matches when **any** criterion holds; the fallback platform is used last. |
| `[display]` | `width`, `height`, `scale = 1.0`, `rotation = 0` (0/90/180/270), `brightness_hook` |
| `[paths]` | `data`, `games`, `tools`, `tmp = "/tmp"` (overridable by `GOBLIN_DATA_DIR`, `GOBLIN_GAMES_DIR`, `GOBLIN_TOOLS_DIR`, `GOBLIN_TMP_DIR`) |
| `[sdl]` | `library_path = []` |
| `[input]` | `map`, `layout` (`nintendo`/`xbox`), button indices `a b x y l1 r1 select start menu f1 f2 vol_down vol_up` (unique), `l2_axis`, `r2_axis`; `[input.glyphs]` logical button to glyph name |
| `[audio]` | `playback`, `capture`, `capture_channels = 1` (1-2), `capture_use = "left"` (`left`/`right`/`mix`), `output_hook`, `mic_gain_hook`, `mic_gain_range = [min, max]` |
| `[leds]` | `driver` (`trimui-led-anim` or `hook`), `sysfs_path = "/sys/class/led_anim"`, `hook`, `zones`, `max_brightness`, `presence_zones` (subset of zones), `accent_zones` (also show working/attention, else firmware setting), `frame_leds`, `[leds.locations]` zone to description |
| `[rumble]` | `hook`, `max_ms = 2000` |
| `[power]` | `battery`, `usb` (power_supply directories), `thermal_zone` (millidegree file), `stay_awake_hook` |
| `[autostart]` | `hook` |
| `[firmware]` | `register_hook` |
| `[tls]` | `ca_bundle` (`embedded` or a PEM path) |

A hook is `"hooks/x.sh"` or `{ path = "hooks/x.sh", timeout_ms = 2000 }`. Paths are relative to the platform
directory, may not contain `..`, and must exist when the platform is loaded.

## Hook contract

POSIX `sh`, invoked as `sh <platform>/<path> <args>` with the platform directory as working directory. Environment:
`GOBLIN_PLATFORM_ID`, `GOBLIN_PLATFORM_DIR`, `GOBLIN_TMP_DIR`, and when known `GOBLIN_DATA_DIR`, `GOBLIN_GAMES_DIR`,
`GOBLIN_TOOLS_DIR` (a variable already in the caller's environment wins over the file). Stdout is the result (64 KiB kept),
stderr is for diagnostics. Exit 0 = success, **2 = not supported here** (reported as unavailable), anything else = failure.
Killed with SIGKILL after the time limit (default 5000 ms), so a hook must not rely on traps to undo hardware state
within its limit; keep real work short.

| Hook | Calls | Output |
|---|---|---|
| `output_hook` | `volume-get`, `volume-set <0-100>`, `mute-get`, `mute-set <0\|1>` | percent / `0` or `1` |
| `mic_gain_hook` | `get`, `set <db>` | integer dB |
| `brightness_hook` | `get`, `set <0-100>` | percent |
| `rumble.hook` | `<ms> <strength 0-100>` | none |
| `stay_awake_hook` | `on`, `off` | none |
| `autostart.hook` | `enable`, `disable`, `status` | `enabled` or `disabled` |
| `register_hook` | `add game\|tool <name> <command>`, `remove game\|tool <name>` | none |
| `leds.hook` (driver `hook`) | `set <zone> <rrggbb> <effect> <ms> <cycles>`, `off <zone>`, `brightness <n>`, `snapshot` (prints a token), `restore` (token on stdin) | none / token |

## Safety conventions

The platform API enforces the LED brightness ceiling, `rumble.max_ms`, and valid entry names. Callers that
change shared hardware state (microphone gain, LED settings, speaker mute) restore it: `MicGainGuard`, `Leds::snapshot/restore`,
`PresenceLeds`. For sound, always set the volume first, unmute last, mute first when finished.
