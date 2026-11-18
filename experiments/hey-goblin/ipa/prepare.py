"""Cache target-independent acoustic logits and synthetic PCM for offline comparisons."""

import argparse
from dataclasses import asdict
import hashlib
import json
from pathlib import Path
import tempfile
import time

import numpy as np
from acoustic import PhoneEncoder
from portable import PortableEncoder
from fixtures import SPLITS, fixtures
from letters import BASE, acoustic_logits, session

HERE = Path(__file__).resolve().parent


def fixture_version():
    digest = hashlib.sha256()
    for path in (
        HERE / "acoustic.py",
        HERE / "portable.py",
        HERE / "fixtures.py",
        HERE.parent / "corpus.py",
        HERE.parent / "letters.py",
        HERE / "Dockerfile",
        HERE / "Dockerfile.portable",
    ):
        digest.update(path.read_bytes())
    return digest.hexdigest()


def prepare(split, destination):
    if destination.exists():
        raise FileExistsError(f"choose a fresh destination: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    phone, letter = PhoneEncoder(4), session(BASE)
    portable = PortableEncoder()
    rows = []
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        folder = Path(temporary) / split
        folder.mkdir()
        start = time.monotonic()
        for i, fixture in enumerate(fixtures(split)):
            sample = fixture.samples
            phonemes = phone.logits(sample)
            letters = acoustic_logits(letter, sample)
            np.savez(
                folder / f"{i}.npz",
                pcm=sample,
                phones=phonemes,
                letters=letters,
                portable=portable.logits(sample),
            )
            row = asdict(fixture)
            del row["samples"]
            rows.append(row)
            if (i + 1) % 100 == 0:
                print(i + 1, "clips", round(time.monotonic() - start, 1), "seconds", flush=True)
        manifest = {"split": split, "version": fixture_version(), "rows": rows}
        (folder / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False))
        folder.rename(destination)
    print(split, len(rows), "clips", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("split", choices=SPLITS)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    prepare(args.split, args.destination)
