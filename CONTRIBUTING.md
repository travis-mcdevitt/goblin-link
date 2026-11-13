# Contributing

Thanks for looking. The project is small and early; the most useful things right now are
reports from real use and support for more handhelds.

## Reporting a problem

Open an issue with the device, the firmware and its version, what you did, what you
expected and what happened. Logs are in the app's data folder under `logs/`; **read them
before attaching them**: they can contain names of chats and commands.

## Changes

- `cargo fmt --all` and `cargo clippy --workspace` should be clean.
- Run the tests in a container: `tools/test-in-container.sh` (all crates) or
  `tools/test-in-container.sh -p goblin-ui`. Some daemon tests start and kill processes;
  the container keeps that away from your own session.
- The screens are compared with reference pictures (`crates/goblin-ui/tests/golden`). If you
  change how something looks on purpose, regenerate them with `GOBLIN_REGEN=1` and include
  the new pictures.
- Say in the pull request what you tried it on. "Builds, not run on a device" is a fine
  thing to say; just say it.

## Testing on a device nobody has tried

Goblin Link has run on one device, the TrimUI Brick. The TrimUI Brick Pro and Smart Pro
have platform files written from NextUI's source and never run. If you have one, you can
do the most useful thing anyone can right now:

1. Install the pak and open it once. On a device that is not yet qualified it writes
   `GoblinLink/device-report.txt` on the SD card: the screen, input devices, sound, lights,
   radios and USB as the system reports them. Serial numbers and hardware addresses are
   blanked, but read it before you share it.
2. Attach that file to the qualification issue for your device (or open one), and say what
   you saw for each line of its checklist: does it open and fill the screen, do the
   buttons do what the hint bar says, does pairing find the handheld, do voice notes and
   calls work, do the lights and vibration.
3. "It opens but pairing never finds it" is a complete and useful report.

A device is marked `qualified = false` in its `platform.toml` until someone has been through
the checklist on real hardware.

## A new device

See [platforms/README.md](platforms/README.md). A device is a `platform.toml` and a few
shell hooks; no Rust is needed for most of them.

## Licence

By contributing you agree your work is licensed under Apache-2.0, as the rest is.
