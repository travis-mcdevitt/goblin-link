#!/usr/bin/env python3
"""Run in a container: package fixture binaries, never build or install on a device."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import zipfile

source = Path(__file__).resolve().parent.parent
licenses = ["LICENSE", "assets/icons/lucide/LICENSE"]
licenses += [p.relative_to(source).as_posix() for p in (source / "assets/fonts").glob("*.txt")]
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    for name in ("platforms", "packaging", "tools"):
        shutil.copytree(source / name, root / name, symlinks=True)
    for name in ("Cargo.toml", "pak.json", "NOTICE", *licenses):
        (root / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / name, root / name)
    binaries = root / "target/aarch64-unknown-linux-gnu/release"
    binaries.mkdir(parents=True)
    for name in ("goblind", "goblin-ui", "goblinctl"):
        (binaries / name).write_text("fixture, never executed\n")
    subprocess.run(["sh", root / "tools/package-pak.sh", "--zip"], check=True)
    pak = root / "target/pak/Goblin Link.pak"
    for name in licenses:
        assert (pak / name).read_bytes() == (source / name).read_bytes()
    adapters = sorted(p.parent.name for p in (root / "platforms").glob("*/platform.toml"))
    assert sorted(p.name for p in (pak / "platforms").iterdir()) == adapters
    assert not any(p.is_symlink() for p in pak.rglob("*"))
    hooks_checked = 0
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
    for line in (pak / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        assert hashlib.sha256((pak / name).read_bytes()).hexdigest() == digest
    with zipfile.ZipFile(root / "target/pak/Goblin.Link.pak.zip") as archive:
        assert "pak.json" in archive.namelist()
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
    print(f"Packaged {len(adapters)} adapters and {hooks_checked} ordinary hooks; archive/checksums match; invalid checkout rejected")
