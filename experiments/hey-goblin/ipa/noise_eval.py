"""Choose a common optional noise front end using noisy development clips."""

from collections import Counter
from pathlib import Path

import numpy as np

from denoise import suppress_noise
from detect import Head, checked_encoder
from evaluate import read_manifest
from fixtures import TARGETS

HERE = Path(__file__).resolve().parent


def main():
    folder = HERE / "target/work/development"
    rows = read_manifest(folder, "development")
    heads = {n: Head(str(HERE / "models" / (n.lower().replace(" ", "_") + ".onnx"))) for n in TARGETS[:2]}
    encoder = checked_encoder(threads=3)
    stats = Counter()
    for i, row in enumerate(rows):
        if row["condition"] != "noise" or row["group"] == "nonspeech":
            continue
        with np.load(folder / f"{i}.npz") as cache:
            pcm, baseline = cache["pcm"], cache["portable"]
        group = row["target"] or row["group"]
        stats[group, "trials"] += 1
        for strength in (0, 1.3, 2.5):
            logits = baseline if not strength else encoder.logits(suppress_noise(pcm, strength))
            fires = {n for n, h in heads.items() if h.scores(logits).max() >= h.cutoff}
            stats[group, strength] += row["target"] in fires if row["target"] else bool(fires)
    for group in sorted({group for group, _ in stats}):
        print(group, [stats[group, key] for key in ("trials", 0, 1.3, 2.5)])


if __name__ == "__main__":
    main()
