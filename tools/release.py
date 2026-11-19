#!/usr/bin/env python3
"""The release workflow's decisions, kept here so they can be tested without GitHub.

  release.py check <dev|stable> <commit> <facts dir> [--on-main] [--contains-main]
      Refuse, or print the version, tag, notes and CI run as GitHub step outputs. The
      facts directory holds what the workflow fetched: main/Cargo.toml, main/pak.json,
      commit/Cargo.toml, commit/pak.json, tags.txt (one per line), ci-runs.json (GitHub's
      runs of ci.yml for the commit) and environment.json (the `release` environment, or
      {} when there is none).
  release.py stamp <version>
      Write a version into this checkout's Cargo.toml, Cargo.lock and pak.json.
"""
import json
import os
from pathlib import Path
import re
import secrets
import sys
import tomllib

PLAIN = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
COMMIT = re.compile(r"[0-9a-f]{40}")
CI = ".github/workflows/ci.yml"
REPO = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")


class Refusal(Exception):
    pass


def cargo_version(text):
    try:
        version = tomllib.loads(text)["workspace"]["package"]["version"]
    except (tomllib.TOMLDecodeError, KeyError, TypeError) as e:
        raise Refusal(f"Cargo.toml has no workspace version: {e}") from None
    if not isinstance(version, str):
        raise Refusal("Cargo.toml's workspace version is not a string")
    return version


def pak(text):
    try:
        data = json.loads(text)
    except ValueError as e:
        raise Refusal(f"pak.json is not JSON: {e}") from None
    if not isinstance(data, dict) or not isinstance(data.get("version"), str):
        raise Refusal("pak.json has no version")
    return data


def plain_version(cargo_text, pak_text, where):
    """The X.Y.Z that Cargo.toml and pak.json agree on."""
    version = cargo_version(cargo_text)
    named = pak(pak_text)["version"]
    if named != f"v{version}":
        raise Refusal(f"{where}: pak.json says {named}, Cargo.toml says {version}")
    if not PLAIN.fullmatch(version):
        raise Refusal(f"{where}: {version} is not a plain X.Y.Z version")
    return version


def ci_passed(ci_runs, commit, repo):
    """The newest run of CI's workflow on exactly this commit finished and passed; its URL.

    Judged by the workflow, not by a job's name: other workflows' jobs, this one's included,
    leave check runs on the same commit. A push to main and a pull request's head both count."""
    runs = ci_runs.get("workflow_runs") if isinstance(ci_runs, dict) else None
    if not isinstance(runs, list):
        raise Refusal("could not read CI's runs for the commit")
    ours = [r for r in runs if isinstance(r, dict) and r.get("path") == CI
            and r.get("head_sha") == commit and r.get("event") in ("push", "pull_request")
            and type(r.get("id")) is int]
    if not ours:
        raise Refusal(f"CI has not run on {commit}")
    newest = max(ours, key=lambda r: r["id"])
    if newest.get("status") != "completed" or newest.get("conclusion") != "success":
        state = newest.get("conclusion") or newest.get("status")
        raise Refusal(f"CI did not pass on {commit}: its latest run is {state}")
    return f"https://github.com/{repo}/actions/runs/{newest['id']}"


def numbers(version):
    """X.Y.Z as numbers, from a version or a tag."""
    match = PLAIN.fullmatch(version.removeprefix("v"))
    if not match:
        raise Refusal(f"{version} is not a plain X.Y.Z version")
    return tuple(map(int, match.groups()))


def guarded(environment):
    """The `release` environment exists and waits for a reviewer. GitHub creates a missing
    environment, with no reviewer, the first time a job names it, so without this check a run
    before the owner sets it up would publish with nobody's approval."""
    rules = environment.get("protection_rules") if isinstance(environment, dict) else None
    for rule in rules if isinstance(rules, list) else []:
        if (isinstance(rule, dict) and rule.get("type") == "required_reviewers"
                and isinstance(rule.get("reviewers"), list) and rule["reviewers"]):
            return
    raise Refusal("the release environment does not exist or has no required reviewer")


def next_dev(stable, tags):
    """<stable>-dev.N, N one more than the highest such tag.

    The current stable version, not the next one: the Pak Store reads -dev.N as a fourth
    number, so this sorts below the next release, which testers are then offered."""
    dev = re.compile(re.escape(f"v{stable}-dev.") + r"(0|[1-9][0-9]*)")
    numbers = [int(m.group(1)) for m in map(dev.fullmatch, tags) if m]
    return f"{stable}-dev.{max(numbers, default=0) + 1}"


def plan(channel, commit, facts, on_main, contains_main, repo):
    """Return (version, notes, CI run URL), or raise Refusal saying why not."""
    if not COMMIT.fullmatch(commit):
        raise Refusal(f"{commit!r} is not a full commit SHA")
    if not REPO.fullmatch(repo):
        raise Refusal(f"{repo!r} is not a repository name")
    guarded(facts["environment"])
    tags = set(facts["tags"])
    ci_run = ci_passed(facts["ci_runs"], commit, repo)
    plugin = (f"The Hermes plugin that matches is `hermes-plugin/` from that commit: "
              f"https://github.com/{repo}/tree/{commit}/hermes-plugin")
    if channel == "dev":
        if not on_main:
            raise Refusal(f"{commit} is not on main")
        stable = plain_version(facts["main_cargo"], facts["main_pak"], "main")
        if f"v{stable}" not in tags:
            raise Refusal(f"main names v{stable}, which has not been released")
        version = next_dev(stable, tags)
        notes = (f"A development build of {commit} from main, for testers. The Pak Store "
                 f"does not offer it, and offers the next release over it.\n\n{plugin}\n")
        return version, notes, ci_run
    if channel == "stable":
        if not contains_main:
            raise Refusal(f"{commit} does not contain main's latest commit: update the PR")
        version = plain_version(facts["commit_cargo"], facts["commit_pak"], commit)
        if f"v{version}" in tags:
            raise Refusal(f"v{version} already exists")
        released = pak(facts["main_pak"])["version"]
        if numbers(version) <= numbers(released):
            raise Refusal(f"v{version} is not above {released}, the version main names")
        changelog = pak(facts["commit_pak"]).get("changelog")
        entry = changelog.get(f"v{version}") if isinstance(changelog, dict) else None
        if not isinstance(entry, str) or not entry.strip():
            raise Refusal(f"pak.json has no changelog entry for v{version}")
        notes = f"{entry.strip()}\n\nBuilt from {commit}. {plugin}\n"
        return version, notes, ci_run
    raise Refusal(f"unknown channel {channel!r}")


def outputs(version, notes, ci_run):
    """GitHub step outputs; the delimiter is random so the notes cannot end the block."""
    delimiter = f"EOF_{secrets.token_hex(16)}"
    return (f"version={version}\ntag=v{version}\nci_run={ci_run}\n"
            f"notes<<{delimiter}\n{notes}{delimiter}\n")


def error(message):
    """A workflow error line. Escaped as GitHub's commands are, so text from a file cannot
    start a command of its own on a new line."""
    escaped = message.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    return f"::error::{escaped}"


def stamp(root, version):
    """Give the workspace another version, so the binaries and pak.json report it.

    Cargo.lock lists the workspace's own crates with their version, and `--locked` builds
    refuse a lock that disagrees, so it is rewritten too. Registry crates are left alone even
    when their version happens to be the same."""
    if not re.fullmatch(r"[0-9A-Za-z.+-]+", version):
        raise Refusal(f"{version!r} is not a version")
    cargo_toml, cargo_lock, pak_json = root / "Cargo.toml", root / "Cargo.lock", root / "pak.json"
    text = cargo_toml.read_text(encoding="utf-8")
    old = cargo_version(text)
    section = text.index("[workspace.package]")
    line = re.compile(r'^version = "[^"]*"$', re.MULTILINE)
    match = line.search(text, section)
    if not match:
        raise Refusal("Cargo.toml's workspace version is not on its own line")
    text = text[:match.start()] + f'version = "{version}"' + text[match.end():]
    if cargo_version(text) != version:
        raise Refusal("could not set Cargo.toml's workspace version")

    lock = cargo_lock.read_text(encoding="utf-8")
    blocks = lock.split("\n[[package]]\n")
    for i, block in enumerate(blocks[1:], 1):
        if "\nsource = " not in f"\n{block}" and f'\nversion = "{old}"\n' in f"\n{block}\n":
            blocks[i] = block.replace(f'\nversion = "{old}"', f'\nversion = "{version}"', 1)
    new_lock = "\n[[package]]\n".join(blocks)
    before, after = (tomllib.loads(t)["package"] for t in (lock, new_lock))
    for a, b in zip(before, after):
        local = "source" not in a and a["version"] == old
        if b["version"] != (version if local else a["version"]):
            raise Refusal(f"could not set {a['name']}'s version in Cargo.lock")

    data = pak(pak_json.read_text(encoding="utf-8"))
    data["version"] = f"v{version}"
    cargo_toml.write_text(text, encoding="utf-8")
    cargo_lock.write_text(new_lock, encoding="utf-8")
    pak_json.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def read_facts(folder):
    def text(name):
        return (folder / name).read_text(encoding="utf-8")
    try:
        ci_runs = json.loads(text("ci-runs.json"))
        environment = json.loads(text("environment.json"))
    except ValueError as e:
        raise Refusal(f"what GitHub said is not JSON: {e}") from None
    return {
        "main_cargo": text("main/Cargo.toml"), "main_pak": text("main/pak.json"),
        "commit_cargo": text("commit/Cargo.toml"), "commit_pak": text("commit/pak.json"),
        "tags": text("tags.txt").split(), "ci_runs": ci_runs, "environment": environment,
    }


def main(args):
    try:
        if len(args) >= 4 and args[0] == "check":
            flags = set(args[4:])
            if not flags <= {"--on-main", "--contains-main"}:
                raise Refusal(f"unknown flags: {sorted(flags)}")
            result = plan(args[1], args[2], read_facts(Path(args[3])),
                          "--on-main" in flags, "--contains-main" in flags,
                          os.environ.get("GITHUB_REPOSITORY", ""))
            sys.stdout.write(outputs(*result))
        elif len(args) == 2 and args[0] == "stamp":
            stamp(Path(__file__).resolve().parent.parent, args[1])
        else:
            print(__doc__, file=sys.stderr)
            return 2
    except Refusal as e:
        print(error(str(e)), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
