# For coding agents

Read this before you run anything in this repository.

## Rules that do not bend

- **Never run a test binary on the host.** Not `cargo test`, not a file under `target/`. Tests
  run only through `tools/test-in-container.sh`, which builds on the host and runs the binaries
  in a throwaway container. The daemon's tests send signals to process groups, and a mistake
  there once killed every process of a developer's login session. `cargo build`, `cargo fmt`,
  `cargo clippy` and `cargo test --no-run` are safe. If you cannot reach Docker, compile the
  tests with `--no-run`, say that they were not run, and stop there.
- **Do not touch a handheld or a Muse account.** Nothing here needs one to build or to pass its
  tests. Leave `adb`, the `tools/*-device.sh` scripts, `tools/package-pak.sh --install` and
  pairing alone unless the person you work for asks for exactly that.
- **A panic stops the daemon.** Release builds use `panic = "abort"`, and on the supported
  handhelds the daemon runs as root. Anything that arrives from Muse, the network, a file on
  the SD card or a child process is checked before it is indexed, sliced, added or unwrapped.
- **The permission checks are the product.** `crates/goblind/src/exec/`, `builds.rs`, `asks.rs`
  and the consent store decide what Muse may do on someone's handheld. A change there comes
  with a test that fails without it. Read `SECURITY.md` first; if the code and that file
  disagree, say so instead of picking one.

## Conventions

- Format and lint before you finish: `cargo fmt --all`, then
  `cargo clippy --workspace --all-targets -- -D warnings`.
- Words on screen are few and plain: a label and a value. No subtitle that explains a setting,
  no decorative text.
- Comments say why, in plain words.
- The protocol between the daemon, the app and the command-line tool is in `docs/ipc.md`.
  `docs/ipc-examples.json` is generated from `crates/goblin-ipc/src/examples.rs`, and a test
  fails when it is stale.
- Screens have golden images in `crates/goblin-ui/tests/golden/`. A change that is not meant
  to alter a screen must leave them passing. Do not regenerate them to make a test pass.

## Working through GitHub

- This repository is the source of truth. Work from an assigned issue with a clear outcome
  and acceptance criteria. Claim it with `in-progress` and a comment naming the agent and
  branch; `ready` means available. Confirm no other agent has claimed it before editing.
- Use one `issue-<number>-<slug>` branch and worktree per task. Never share a mutable checkout
  or Cargo target directory. Coordinate overlapping files before working in parallel.
- An assigned issue authorizes implementation, checks, commits, pushing the issue branch,
  opening a PR, and resolving review findings. Finish these without routine permission asks.
  Keep unrelated work in another issue. Link the issue and test evidence in the PR.
- Ask a separate agent session, preferably the other model, to review the final diff against
  the issue and these rules. Record findings, reviewer identity, and the reviewed commit SHA
  in the PR. Fix defects and review the new commit before merging; do not manufacture approval.
- Agents may squash-merge tested, independently reviewed changes, including features and
  refactors. Permission expansion, credential handling, migrations, releases, and changes to
  merge safeguards need explicit owner approval on the PR. Approval of a scoped task persists.
- Require passing CI on the current base and merge only the reviewed head SHA (for example,
  `gh pr merge --squash --match-head-commit <sha>`). Never bypass checks or push directly to
  `main`. Two agents using one GitHub account are not two GitHub approvers.
- For refactors, report source/test/config additions and deletions against the base. The
  refactor series must shrink overall; report docs/workflow overhead separately. Moves,
  compressed formatting, and removing useful tests do not count as simplification.
