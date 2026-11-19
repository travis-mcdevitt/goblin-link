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
MAJOR, MINOR, _ = release.numbers(CURRENT)
NEXT = f"{MAJOR}.{MINOR + 1}.0"


def with_version(version, pak_version=None, changelog=True):
    cargo = re.sub(r'(?m)^version = "[^"]*"$', f'version = "{version}"', CARGO, count=1)
    data = json.loads(PAK)
    data["version"] = pak_version or f"v{version}"
    if changelog:
        data["changelog"] = {f"v{version}": "What changed.", **data["changelog"]}
    return cargo, json.dumps(data)


def run(id, conclusion="success", status="completed", event="push", path=release.CI, sha=SHA,
        branch="main", source=REPO):
    """A workflow run as GET /repos/{repo}/actions/workflows/ci.yml/runs?head_sha= lists it."""
    return {"id": id, "name": "CI", "path": path, "event": event, "status": status,
            "conclusion": conclusion, "head_sha": sha, "head_branch": branch, "run_attempt": 1,
            "html_url": f"https://github.com/{REPO}/actions/runs/{id}",
            "head_repository": {"full_name": source}, "repository": {"full_name": REPO}}


# This workflow's own run on the same commit, still going: it must not count either way.
RELEASE = run(900, None, "in_progress", "workflow_dispatch", ".github/workflows/release.yml")


ENVIRONMENT = {"name": "release", "protection_rules": [
    {"type": "branch_policy"},
    {"type": "required_reviewers", "prevent_self_review": False,
     "reviewers": [{"type": "User", "reviewer": {"login": "owner"}}]}]}


def facts(tags=("v0.1.0", f"v{CURRENT}"), runs=(run(100), RELEASE),
          main=(CARGO, PAK), commit=(CARGO, PAK), environment=ENVIRONMENT):
    return {"main_cargo": main[0], "main_pak": main[1], "commit_cargo": commit[0],
            "commit_pak": commit[1], "tags": list(tags),
            "ci_runs": {"total_count": len(runs), "workflow_runs": list(runs)},
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
version, notes, ci_run = release.plan("dev", SHA, facts(), True, False, REPO)
assert version == f"{CURRENT}-dev.1", version
assert ci_run == f"https://github.com/{REPO}/actions/runs/100", ci_run
assert SHA in notes and f"https://github.com/{REPO}/tree/{SHA}/hermes-plugin" in notes
tags = ["v0.1.0", f"v{CURRENT}", f"v{CURRENT}-dev.1", f"v{CURRENT}-dev.9", f"v{CURRENT}-dev.10",
        f"v{CURRENT}-dev.x", f"v{CURRENT}-dev.10.1", f"{CURRENT}-dev.40", "v0.0.9-dev.50"]
version, _, _ = release.plan("dev", SHA, facts(tags=tags), True, False, REPO)
assert version == f"{CURRENT}-dev.11", version
# A dev build sorts above its stable version and below the next one, as the store compares.
store = [int(n) for n in re.split(r"[.-]", version) if n.isdigit()]
assert store[:3] == [int(n) for n in CURRENT.split(".")] and store[3] == 11

# dev refusals.
refused("is not on main", "dev", facts(), on_main=False)
refused("pak.json says v9.9.9, Cargo.toml says", "dev", facts(main=with_version(CURRENT, "v9.9.9")))
refused("not a plain X.Y.Z", "dev", facts(main=with_version(f"{CURRENT}-rc.1")))
refused(f"main names v{CURRENT}, which has not been released", "dev", facts(tags=["v0.1.0"]))
refused("its latest run is failure", "dev", facts(runs=[run(100, "failure"), RELEASE]))

# stable: the release PR's version, tag absent, with its changelog entry as the notes. A
# pull request's run of CI on its head counts.
commit = with_version(NEXT)
pr_run = [run(100, event="pull_request", branch="release-0.2")]
version, notes, _ = release.plan("stable", SHA, facts(commit=commit, runs=pr_run), False, True, REPO)
assert version == NEXT and notes.startswith("What changed.\n") and SHA in notes, notes
assert f"https://github.com/{REPO}/tree/{SHA}/hermes-plugin" in notes

# stable refusals, one for each thing the issue names and the rest the script checks.
refused(f"pak.json says v{NEXT}1, Cargo.toml says {NEXT}", "stable",
        facts(commit=with_version(NEXT, f"v{NEXT}1")))
refused(f"{NEXT}-rc.1 is not a plain X.Y.Z", "stable", facts(commit=with_version(f"{NEXT}-rc.1")))
refused(f"{NEXT}-dev.1 is not a plain X.Y.Z", "stable", facts(commit=with_version(f"{NEXT}-dev.1")))
refused(f"v{NEXT} already exists", "stable", facts(tags=[f"v{NEXT}"], commit=commit))
refused(f"v{CURRENT} already exists", "stable", facts())
refused(f"v{CURRENT} is not above v{CURRENT}, the version main names", "stable", facts(tags=[]))
refused(f"v{MAJOR}.{MINOR}.0 is not above v{CURRENT}", "stable",
        facts(tags=[], commit=with_version(f"{MAJOR}.{MINOR}.0")))
refused("its latest run is failure", "stable", facts(commit=commit, runs=[run(100, "failure")]))
refused("does not contain main's latest commit", "stable", facts(commit=commit), contains_main=False)
refused(f"no changelog entry for v{NEXT}", "stable",
        facts(commit=with_version(NEXT, changelog=False)))

# CI is the newest run of ci.yml on exactly this commit, from a push or a pull request. Other
# workflows' runs, this one's included, do not count, failed or not.
refused("CI has not run", "stable", facts(commit=commit, runs=[RELEASE]))
refused("CI has not run", "stable", facts(commit=commit, runs=[
    run(100, path=".github/workflows/release.yml", event="workflow_dispatch"),
    run(101, sha="f" * 40), run(102, event="workflow_dispatch"), {**run(103), "id": "103"}]))
for passed in ([run(100), run(901, "failure", path=".github/workflows/release.yml")],
               [run(100, "failure"), run(101)], [run(101), run(100, "failure")]):
    release.plan("stable", SHA, facts(commit=commit, runs=passed), False, True, REPO)
refused("its latest run is in_progress", "stable",
        facts(commit=commit, runs=[run(100), run(101, None, "in_progress", "pull_request")]))
refused("its latest run is cancelled", "stable",
        facts(commit=commit, runs=[run(100), run(101, "cancelled")]))
# A pull request's run from a fork does not count for a stable release; for a dev build only
# the run for the push to main counts.
refused("CI has not run", "stable", facts(commit=commit, runs=[
    run(100, event="pull_request", branch="main", source="someone/goblin-link"),
    {**run(101, event="pull_request"), "head_repository": None}]))
refused("CI has not run", "dev", facts(runs=[run(100, event="pull_request")]))
refused("CI has not run", "dev", facts(runs=[run(100, branch="release-0.2")]))
release.plan("dev", SHA, facts(runs=[run(100), run(101, "failure", event="pull_request")]),
             True, False, REPO)
broken = facts(commit=commit)
broken["ci_runs"] = {"message": "Not Found"}
refused("could not read CI's runs", "stable", broken)

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
text = release.outputs("1.2.0", "line\nEOF\n", "https://example/1")
head, rest = text.split("notes<<", 1)
delimiter, rest = rest.split("\n", 1)
assert head == "version=1.2.0\ntag=v1.2.0\nci_run=https://example/1\n", text
assert rest == f"line\nEOF\n{delimiter}\n", text
assert len(delimiter) > 20 and f"<<{delimiter}\n" not in release.outputs("1.2.0", "", "")
# An error that quotes a file cannot start a workflow command on a line of its own.
assert release.error("v1\n::add-mask::x 100%\r") == "::error::v1%0A::add-mask::x 100%25%0D"

# The command line, as the workflow calls it.
with tempfile.TemporaryDirectory() as tmp:
    folder = Path(tmp)
    for side, (cargo, pak) in {"main": (CARGO, PAK), "commit": with_version(NEXT)}.items():
        (folder / side).mkdir()
        (folder / side / "Cargo.toml").write_text(cargo)
        (folder / side / "pak.json").write_text(pak)
    (folder / "tags.txt").write_text(f"v0.1.0\nv{CURRENT}\nv{CURRENT}-dev.2\n")
    (folder / "ci-runs.json").write_text(json.dumps(facts()["ci_runs"]))
    (folder / "environment.json").write_text(json.dumps(ENVIRONMENT))
    env = {**os.environ, "GITHUB_REPOSITORY": REPO}
    script = [sys.executable, str(source / "tools/release.py"), "check"]
    done = subprocess.run([*script, "dev", SHA, tmp, "--on-main"], env=env,
                          capture_output=True, text=True)
    assert done.returncode == 0 and done.stdout.startswith(
        f"version={CURRENT}-dev.3\ntag=v{CURRENT}-dev.3\n"
        f"ci_run=https://github.com/{REPO}/actions/runs/100\nnotes<<"), done
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
    try:
        release.stamp(root, "1.0.0\nversion = 2")
    except release.Refusal:
        pass
    else:
        raise AssertionError("stamp took a version with a newline")

# Registry crates keep their version, even one that equals the workspace's.
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    (root / "Cargo.toml").write_text('[workspace]\nmembers = ["app"]\n\n'
                                     '[workspace.package]\nversion = "1.2.3"\nedition = "2021"\n')
    registry = 'source = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "00"\n'
    lock = ('version = 4\n\n'
            '[[package]]\nname = "app"\nversion = "1.2.3"\ndependencies = [\n "clash",\n]\n\n'
            f'[[package]]\nname = "clash"\nversion = "1.2.3"\n{registry}\n'
            f'[[package]]\nname = "other"\nversion = "0.9.0"\n{registry}')
    (root / "Cargo.lock").write_text(lock)
    (root / "pak.json").write_text('{"version": "v1.2.3"}')
    release.stamp(root, "1.2.3-dev.1")
    assert (root / "Cargo.lock").read_text() == lock.replace(
        'name = "app"\nversion = "1.2.3"', 'name = "app"\nversion = "1.2.3-dev.1"')
    assert json.loads((root / "pak.json").read_text()) == {"version": "v1.2.3-dev.1"}

print("release checks: ok")
