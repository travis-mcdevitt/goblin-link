#!/usr/bin/env python3
"""Run in a container: package fixture binaries, never build or install on a device."""
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile

source = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(source / "tools"))
from third_party_licenses import FINGERPRINT, lock_fingerprint  # noqa: E402

# Every licence file NOTICE names must ship, and NOTICE must name the ones we know about.
notice = (source / "NOTICE").read_text(encoding="utf-8")
named = {word for word in re.findall(r"(?<![\w/:.])[\w.-]+(?:/[\w.-]+)*", notice)
         if re.search(r"(^|/)(LICEN[CS]E|OFL-|THIRD-PARTY-)", word)}
missing = [name for name in named if not (source / name).is_file()]
assert not missing, f"NOTICE names licence files that do not exist: {missing}"
licenses = sorted(named | {"LICENSE"})
for expected in ("THIRD-PARTY-LICENSES.txt", "assets/hermes/LICENSE-hermes-agent.txt",
                 "assets/icons/lucide/LICENSE", "assets/fonts/OFL-Inter.txt"):
    assert expected in licenses, f"NOTICE does not name {expected}"

# The crate licences must match Cargo.lock; regenerating needs cargo, which is not here.
third_party = (source / "THIRD-PARTY-LICENSES.txt").read_text(encoding="utf-8")
lock = (source / "Cargo.lock").read_text(encoding="utf-8")
assert FINGERPRINT + lock_fingerprint(lock) in third_party, (
    "THIRD-PARTY-LICENSES.txt is stale: run tools/third_party_licenses.py")
locked = {(p["name"], p["version"]) for p in tomllib.loads(lock)["package"] if "source" in p}
listed = re.findall(r"^([\w-]+) (\d[^:\s]*): ", third_party, re.MULTILINE)
assert listed and all(crate in locked for crate in listed), "a listed crate is not in Cargo.lock"
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    for name in ("platforms", "packaging", "tools"):
        shutil.copytree(source / name, root / name, symlinks=True)
    for name in ("Cargo.toml", "pak.json", "NOTICE", *licenses):
        (root / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / name, root / name)
    binaries = root / "target/aarch64-unknown-linux-gnu/release"
    binaries.mkdir(parents=True)
    for name in ("goblind", "goblin-ui", "goblinctl", "goblin-nextui-power"):
        (binaries / name).write_text("fixture, never executed\n")
    subprocess.run(["sh", root / "tools/package-pak.sh", "--zip"], check=True)
    pak = root / "target/pak/Goblin Link.pak"
    for name in licenses:
        assert (pak / name).read_bytes() == (source / name).read_bytes()
    adapters = sorted(p.parent.name for p in (root / "platforms").glob("*/platform.toml"))
    assert sorted(p.name for p in (pak / "platforms").iterdir()) == adapters
    assert not any(p.is_symlink() for p in pak.rglob("*"))
    hooks_checked = 0
    enabled = False
    for adapter in adapters:
        for original in (root / "platforms" / adapter / "hooks").glob("*.sh"):
            packaged = pak / "platforms" / adapter / "hooks" / original.name
            assert packaged.read_bytes() == original.read_bytes()
            assert os.access(packaged, os.X_OK)
            hooks_checked += 1
        hook = pak / "platforms" / adapter / "hooks/autostart.sh"
        if hook.exists():
            auto = root / f"{adapter}-auto.sh"
            env = dict(os.environ, GOBLIN_AUTO_SH=str(auto))
            env.pop("GOBLIN_BOOT_SH", None)
            subprocess.run(["sh", hook, "enable"], env=env, check=True)
            assert f'"{pak}/on-boot.sh" &' in auto.read_text()
            enabled = True
    # Turning start on boot on records the consent inside the pak; a release never carries one,
    # even when packaged over a folder that has it.
    assert enabled and (pak / ".start-on-boot").is_file()
    subprocess.run(["sh", root / "tools/package-pak.sh", "--zip"], check=True)
    assert not (pak / ".start-on-boot").exists()
    for line in (pak / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        assert hashlib.sha256((pak / name).read_bytes()).hexdigest() == digest
    with zipfile.ZipFile(root / "target/pak/Goblin.Link.pak.zip") as archive:
        assert "pak.json" in archive.namelist()
        assert not any(name.endswith(".start-on-boot") for name in archive.namelist())
        assert not any(name.startswith("Goblin Link.pak/") for name in archive.namelist())
        for path in pak.rglob("*"):
            if path.is_file():
                assert archive.read(path.relative_to(pak).as_posix()) == path.read_bytes()
    # A checkout with core.symlinks=false must fail rather than silently ship text links.
    link = root / "platforms/tg5040-brick-nextui/hooks"
    target = os.readlink(link)
    link.unlink()
    link.write_text(target)
    failed = subprocess.run(["sh", root / "tools/package-pak.sh"], capture_output=True, text=True)
    assert failed.returncode != 0 and "symlinks enabled" in failed.stderr
    print(f"Shipped {len(licenses)} licence files for {len(listed)} crates and the assets")
    print(f"Packaged {len(adapters)} adapters and {hooks_checked} ordinary hooks; archive/checksums match; invalid checkout rejected")
