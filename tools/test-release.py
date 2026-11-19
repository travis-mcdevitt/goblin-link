#!/usr/bin/env python3
"""Run in a container: the release workflow's checks and version numbering, without GitHub."""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib

source = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(source / "tools"))
import release  # noqa: E402

SHA = "0123456789abcdef0123456789abcdef01234567"
REPO = "owner/goblin-link"
CARGO = (source / "Cargo.toml").read_text(encoding="utf-8")
PAK = (source / "pak.json").read_text(encoding="utf-8")
CURRENT = release.cargo_version(CARGO)
assert release.PLAIN.fullmatch(CURRENT), f"main's version {CURRENT} is not plain"


def with_version(version, pak_version=None, changelog=True):
    cargo = re.sub(r'(?m)^version = "[^"]*"$', f'version = "{version}"', CARGO, count=1)
    data = json.loads(PAK)
    data["version"] = pak_version or f"v{version}"
    if changelog:
        data["changelog"] = {f"v{version}": "What changed.", **data["changelog"]}
    return cargo, json.dumps(data)


def run(name, conclusion="success", status="completed", app="github-actions"):
    return {"name": name, "status": status, "conclusion": conclusion, "app": {"slug": app}}


ENVIRONMENT = {"name": "release", "protection_rules": [
    {"type": "branch_policy"},
    {"type": "required_reviewers", "prevent_self_review": False,
     "reviewers": [{"type": "User", "reviewer": {"login": "owner"}}]}]}


def facts(tags=("v0.1.0", f"v{CURRENT}"), runs=(run("check"), run("pak")),
          main=(CARGO, PAK), commit=(CARGO, PAK), environment=ENVIRONMENT):
    return {"main_cargo": main[0], "main_pak": main[1], "commit_cargo": commit[0],
            "commit_pak": commit[1], "tags": list(tags),
            "check_runs": {"total_count": len(runs), "check_runs": list(runs)},
            "environment": environment}


def refused(why, channel, f, on_main=True, contains_main=True, commit=SHA, repo=REPO):
    try:
        release.plan(channel, commit, f, on_main, contains_main, repo)
    except release.Refusal as e:
        assert why in str(e), f"refused for {e!r}, expected {why!r}"
        print(f"refused ({channel}): {e}")
        return
    raise AssertionError(f"{channel} was not refused: {why}")


# dev: the current stable version plus -dev.N, N one more than the highest such tag.
version, notes = release.plan("dev", SHA, facts(), True, False, REPO)
assert version == f"{CURRENT}-dev.1", version
assert SHA in notes and f"https://github.com/{REPO}/tree/{SHA}/hermes-plugin" in notes
tags = ["v0.1.0", f"v{CURRENT}", f"v{CURRENT}-dev.1", f"v{CURRENT}-dev.9", f"v{CURRENT}-dev.10",
        f"v{CURRENT}-dev.x", f"v{CURRENT}-dev.10.1", f"{CURRENT}-dev.40", "v0.0.9-dev.50"]
version, _ = release.plan("dev", SHA, facts(tags=tags), True, False, REPO)
assert version == f"{CURRENT}-dev.11", version
# A dev build sorts above its stable version and below the next one, as the store compares.
store = [int(n) for n in re.split(r"[.-]", version) if n.isdigit()]
assert store[:3] == [int(n) for n in CURRENT.split(".")] and store[3] == 11

# dev refusals.
refused("is not on main", "dev", facts(), on_main=False)
refused("pak.json says v9.9.9, Cargo.toml says", "dev", facts(main=with_version(CURRENT, "v9.9.9")))
refused("not a plain X.Y.Z", "dev", facts(main=with_version(f"{CURRENT}-rc.1")))
refused(f"main names v{CURRENT}, which has not been released", "dev", facts(tags=["v0.1.0"]))
refused("check is failure", "dev", facts(runs=[run("check", "failure")]))

# stable: the release PR's version, tag absent, with its changelog entry as the notes.
commit = with_version("0.2.0")
version, notes = release.plan("stable", SHA, facts(commit=commit), False, True, REPO)
assert version == "0.2.0" and notes.startswith("What changed.\n") and SHA in notes, notes
assert f"https://github.com/{REPO}/tree/{SHA}/hermes-plugin" in notes

# stable refusals, one for each thing the issue names and the rest the script checks.
refused("pak.json says v0.2.1, Cargo.toml says 0.2.0", "stable",
        facts(commit=with_version("0.2.0", "v0.2.1")))
refused("0.2.0-rc.1 is not a plain X.Y.Z", "stable", facts(commit=with_version("0.2.0-rc.1")))
refused("0.2.0-dev.1 is not a plain X.Y.Z", "stable", facts(commit=with_version("0.2.0-dev.1")))
refused("v0.2.0 already exists", "stable", facts(tags=["v0.2.0"], commit=commit))
refused(f"v{CURRENT} already exists", "stable", facts())
refused("check is failure", "stable", facts(commit=commit, runs=[run("check", "failure")]))
refused("does not contain main's latest commit", "stable", facts(commit=commit), contains_main=False)
refused("no changelog entry for v0.2.0", "stable",
        facts(commit=with_version("0.2.0", changelog=False)))

# CI must have run `check` on the commit and passed every time it last ran.
refused("CI has not run", "stable", facts(commit=commit, runs=[run("pak")]))
refused("CI has not run", "stable", facts(commit=commit, runs=[run("check", app="someone-else")]))
refused("check is in_progress", "stable",
        facts(commit=commit, runs=[run("check", None, "in_progress")]))
refused("check is cancelled", "stable",
        facts(commit=commit, runs=[run("check"), run("check", "cancelled")]))
broken = facts(commit=commit)
broken["check_runs"] = {"message": "Not Found"}
refused("could not read the commit's check runs", "stable", broken)

# Nothing is published unless the release environment waits for a reviewer: GitHub would
# make a missing one, unguarded, when the publish job names it.
for environment in ({}, {"name": "release", "protection_rules": []},
                    {"protection_rules": [{"type": "wait_timer", "wait_timer": 5}]},
                    {"protection_rules": [{"type": "required_reviewers", "reviewers": []}]},
                    {"message": "Not Found"}, []):
    refused("no required reviewer", "dev", facts(environment=environment))

# Inputs.
refused("not a full commit SHA", "stable", facts(commit=commit), commit=SHA[:7])
refused("not a full commit SHA", "stable", facts(commit=commit), commit=SHA.upper())
refused("not a repository name", "stable", facts(commit=commit), repo="owner/repo\nx")
refused("unknown channel", "beta", facts())
refused("pak.json is not JSON", "stable", facts(commit=(commit[0], "{")))
refused("Cargo.toml has no workspace version", "stable", facts(commit=("[package]\n", commit[1])))

# Step outputs: the notes cannot close their own block.
text = release.outputs("0.2.0", "line\nEOF\n")
head, rest = text.split("notes<<", 1)
delimiter, rest = rest.split("\n", 1)
assert head == "version=0.2.0\ntag=v0.2.0\n" and rest == f"line\nEOF\n{delimiter}\n", text
assert len(delimiter) > 20 and f"<<{delimiter}\n" not in release.outputs("0.2.0", "")

# The command line, as the workflow calls it.
with tempfile.TemporaryDirectory() as tmp:
    folder = Path(tmp)
    for side, (cargo, pak) in {"main": (CARGO, PAK), "commit": with_version("0.2.0")}.items():
        (folder / side).mkdir()
        (folder / side / "Cargo.toml").write_text(cargo)
        (folder / side / "pak.json").write_text(pak)
    (folder / "tags.txt").write_text(f"v0.1.0\nv{CURRENT}\nv{CURRENT}-dev.2\n")
    (folder / "check-runs.json").write_text(json.dumps(facts()["check_runs"]))
    (folder / "environment.json").write_text(json.dumps(ENVIRONMENT))
    env = {**os.environ, "GITHUB_REPOSITORY": REPO}
    script = [sys.executable, str(source / "tools/release.py"), "check"]
    done = subprocess.run([*script, "dev", SHA, tmp, "--on-main"], env=env,
                          capture_output=True, text=True)
    assert done.returncode == 0 and done.stdout.startswith(
        f"version={CURRENT}-dev.3\ntag=v{CURRENT}-dev.3\nnotes<<"), done
    done = subprocess.run([*script, "stable", SHA, tmp], env=env, capture_output=True, text=True)
    assert done.returncode == 1 and done.stdout == "" and done.stderr == (
        f"::error::{SHA} does not contain main's latest commit: update the PR\n"), done
    done = subprocess.run([*script, "stable", SHA, tmp, "--contains-main", "--skip"], env=env,
                          capture_output=True, text=True)
    assert done.returncode == 1 and "unknown flags" in done.stderr, done

# stamp: the job's own copy reports the dev version, and a --locked build still accepts it.
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    for name in ("Cargo.toml", "Cargo.lock", "pak.json"):
        shutil.copyfile(source / name, root / name)
    release.stamp(root, f"{CURRENT}-dev.3")
    assert release.cargo_version((root / "Cargo.toml").read_text()) == f"{CURRENT}-dev.3"
    stamped = json.loads((root / "pak.json").read_text())
    assert stamped == {**json.loads(PAK), "version": f"v{CURRENT}-dev.3"}
    members = {p.parent.name for p in (source / "crates").glob("*/Cargo.toml")}
    before = tomllib.loads((source / "Cargo.lock").read_text())["package"]
    after = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    changed = {b["name"] for a, b in zip(before, after) if a != b}
    assert changed == members, changed ^ members
    assert all(b == {**a, "version": f"{CURRENT}-dev.3"} for a, b in zip(before, after)
               if a["name"] in members)
    # Registry crates keep their version, even one that equals ours.
    assert any("source" in a and a["version"] == CURRENT for a in before)
    try:
        release.stamp(root, "1.0.0\nversion = 2")
    except release.Refusal:
        pass
    else:
        raise AssertionError("stamp took a version with a newline")

print("release checks: ok")
