#!/usr/bin/env python3
"""Write THIRD-PARTY-LICENSES.txt: the licences of the crates linked into the pak's binaries.

Usage: tools/third_party_licenses.py [--check]

Needs cargo and the crates' sources (`cargo fetch --locked`); it only reads, and runs no
build. --check exits 1 when the file in the repository is not what this would write.
tools/test-packaging.py, which has no cargo, checks the file's Cargo.lock fingerprint.
"""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "THIRD-PARTY-LICENSES.txt"
TARGET = "aarch64-unknown-linux-gnu"
# The pak's binaries, grouped as they are built (features are unified within a build).
BUILDS = (("goblin-ui", "goblin-platform"), ("goblind", "goblinctl"))
LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|copyright|notice|unlicense)", re.IGNORECASE)
FINGERPRINT = "Cargo.lock fingerprint: "
# Crates published without a licence file at their root: (crate of the same version, file).
# sdl2-sys is MIT like its sibling sdl2 (same project and authors) and Zlib like SDL.
ELSEWHERE = {"sdl2-sys": (("sdl2", "LICENSE"), ("sdl2-sys", "SDL/LICENSE.txt"))}


def lock_fingerprint(lock_text):
    """Changes when a third-party crate, or a dependency edge of ours, changes in Cargo.lock.

    Our own crates' version numbers are left out so that a release bump needs no rewrite."""
    lines = []
    for package in tomllib.loads(lock_text).get("package", []):
        if "source" in package:
            lines.append(f"{package['name']} {package['version']} {package.get('checksum', '')}")
        else:
            lines.append(f"{package['name']} -> {' '.join(sorted(package.get('dependencies', [])))}")
    return hashlib.sha256("\n".join(sorted(lines)).encode()).hexdigest()


def cargo(*args):
    return subprocess.run(["cargo", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def linked_crates():
    """(name, version) of every third-party crate compiled into a shipped binary.

    Build scripts, proc macros and dev-dependencies run on the build machine only."""
    found = set()
    for build in BUILDS:
        selection = [arg for name in build for arg in ("-p", name)]
        out = cargo("tree", "--locked", *selection, "--target", TARGET, "-e", "normal,no-proc-macro",
                    "--prefix", "none", "--format", "{p}")
        for line in out.splitlines():
            parts = line.split()
            # Our own crates print their path; third-party ones print only name and version.
            if len(parts) >= 2 and parts[1].startswith("v") and not any(p.startswith("(/") for p in parts):
                found.add((parts[0], parts[1][1:]))
    return found


def licence_texts(directory):
    files = sorted(p for p in directory.iterdir() if p.is_file() and LICENCE_FILE.match(p.name))
    return [(p.name, p.read_text(encoding="utf-8", errors="replace").strip()) for p in files]


def rust_std():
    template = Path(cargo_sysroot()) / "share/doc/rust/licenses/MIT.txt"
    text = template.read_text(encoding="utf-8").strip()
    text = text.replace("<year> <copyright holders>", "The Rust Project Contributors")
    return text


def cargo_sysroot():
    return subprocess.run(["rustc", "--print", "sysroot"], cwd=ROOT, check=True, capture_output=True,
                          text=True).stdout.strip()


def render():
    metadata = json.loads(cargo("metadata", "--locked", "--format-version", "1", "--filter-platform", TARGET))
    packages = {(p["name"], p["version"]): p for p in metadata["packages"]}
    linked = sorted(linked_crates())
    missing = [f"{n} {v}" for n, v in linked if (n, v) not in packages]
    if missing:
        raise SystemExit(f"not in cargo metadata: {', '.join(missing)}")

    # Identical licence texts (ignoring line wrapping) are printed once, with every crate
    # that ships them.
    texts = {}
    first = {}
    crates = []
    for name, version in linked:
        package = packages[(name, version)]
        files = licence_texts(Path(package["manifest_path"]).parent)
        for owner, relative in ELSEWHERE.get(name, ()):
            path = Path(packages[(owner, version)]["manifest_path"]).parent / relative
            files.append((relative, path.read_text(encoding="utf-8", errors="replace").strip()))
        if not files:
            raise SystemExit(f"{name} {version} ships no licence file: add its text by hand")
        crates.append((name, version, package.get("license") or package.get("license_file") or "see text"))
        for _, text in files:
            text = first.setdefault(" ".join(text.split()), text)
            texts.setdefault(text, []).append(f"{name} {version}")
    std = first.setdefault(" ".join(rust_std().split()), rust_std())
    texts.setdefault(std, []).insert(0, "Rust standard library (std, core, alloc)")

    out = [
        "Third-party licences for the Goblin Link binaries",
        "=================================================",
        "",
        "goblind, goblin-ui, goblinctl and goblin-nextui-power (aarch64) are built from the crates",
        "below, each under the licence named. Where a crate offers a choice of licences, its",
        "published licence files are all reproduced.",
        "",
        "Generated by tools/third_party_licenses.py; do not edit by hand.",
        FINGERPRINT + lock_fingerprint((ROOT / "Cargo.lock").read_text(encoding="utf-8")),
        "",
        "Crates",
        "------",
        "Rust standard library: MIT OR Apache-2.0",
    ]
    out += [f"{name} {version}: {licence}" for name, version, licence in crates]
    for number, (text, users) in enumerate(texts.items(), 1):
        out += ["", "", f"Licence text {number}", "-" * 60, "Used by: " + ", ".join(users), "", text]
    return "\n".join(out) + "\n"


def main():
    text = render()
    if sys.argv[1:] == ["--check"]:
        if OUTPUT.read_text(encoding="utf-8") != text:
            raise SystemExit(f"{OUTPUT.name} is stale: run tools/third_party_licenses.py")
        print(f"{OUTPUT.name} is current")
    else:
        OUTPUT.write_text(text, encoding="utf-8")
        print(f"wrote {OUTPUT.name}")


if __name__ == "__main__":
    main()
