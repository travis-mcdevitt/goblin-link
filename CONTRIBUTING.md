# Contributing

Thanks for looking. The project is small and early; the most useful things right now are
reports from real use and support for more handhelds.

## Reporting a problem

Open an issue with the device, the firmware and its version, what you did, what you
expected and what happened. Logs are in the app's data folder under `logs/`; **read them
before attaching them**: they can contain names of chats and commands.

## Changes

- This standalone repository is the source of truth; development no longer uses private-tree
  exports. Start with an issue describing the problem, desired result, acceptance criteria,
  and dependencies. Claim it with `in-progress` and an agent/branch comment; `ready` is unclaimed.
- Use a separate worktree and `issue-<number>-<slug>` branch for each task. Coordinate changes
  to the same files. Keep each PR independently testable and link its issue.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` should be clean.
- Run the tests in a container: `tools/test-in-container.sh` (all crates) or
  `tools/test-in-container.sh -p goblin-ui`. Some daemon tests start and kill processes;
  the container keeps that away from your own session.
- The screens are compared with reference pictures (`crates/goblin-ui/tests/golden`). If you
  change how something looks on purpose, regenerate them with `GOBLIN_REGEN=1` and include
  the new pictures.
- Say in the pull request what you tried it on. "Builds, not run on a device" is a fine
  thing to say; just say it.
- A separate agent session reviews the final commit for defects and missed requirements.
  Record its identity, findings, and commit SHA in the PR; repeat after changes. Agent reviews
  under the author's GitHub login are a convention, not a second person's GitHub approval.
- Agents may merge reviewed changes once CI passes on the current base, using the reviewed
  head SHA. Permission expansion, credentials, migrations, releases, and merge-safeguard
  changes need explicit owner approval on the PR. See [AGENTS.md](AGENTS.md) for standing rules.
- Squash-merge routine PRs and close their issues. For a refactor series, report source/test/
  config additions and deletions, with docs/workflow overhead separate; the series must shrink.

CI checks formatting, strict clippy, and container tests. It uses no device, account, or
repository secrets. If Docker is unavailable locally, compile tests with `cargo test
--workspace --no-run` and say they were not run; wait for CI before claiming test success.

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
