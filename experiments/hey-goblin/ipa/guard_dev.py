"""Measure the cost of explicit phonetic competitors on development voices."""
import json
from pathlib import Path

import numpy as np

from compile_ipa import compile_head
from detect import Head
from evaluate import read_manifest

HERE = Path(__file__).resolve().parent


def main():
    folder = HERE / "target/work/development"
    rows = read_manifest(folder, "development")
    frames = []
    for i in range(len(rows)):
        with np.load(folder / f"{i}.npz") as f:
            frames.append(f["portable"])
    specs = json.loads((HERE / "recipes.json").read_text())
    candidates = {"Hey Goblin": ["/ðə ˈɡɑblɪn/", "/ə ˈɡɑblɪn/", "/heɪ ˈɡɑblɪŋ/", "/heɪ ˈɡɑblɪnz/"],
                  "Hey Hermes": ["/seɪ ˈhɝmiz/", "/heɪ ˈɑɹmiz/"]}
    for name, rivals in candidates.items():
        spec = specs[name]
        for rejects in ([], *([r] for r in rivals), rivals):
            head = Head(compile_head(spec["ipa"], reject=rejects).SerializeToString())
            fire = [r for r, z in zip(rows, frames) if head.scores(z).max() >= head.cutoff]
            pos = sum(r["target"] == name for r in fire)
            fp = [r for r in fire if r["target"] != name]
            print(name, rejects, "hit", pos, "/ 63", "false", len(fp),
                  {t: sum(r["text"] == t for r in fp) for t in sorted({r["text"] for r in fp})}, flush=True)


if __name__ == "__main__":
    main()
