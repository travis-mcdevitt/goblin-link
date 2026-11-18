"""Measure transfer to an English sound inventory with no further calibration."""

import argparse
from collections import Counter
import json
from pathlib import Path

import numpy as np

from compile_ipa import compile_head
from corpus import render
from detect import Head, checked_encoder
from phones import tokenize

HERE = Path(__file__).resolve().parent


def main(split):
    specs = json.loads((HERE / "transfer.json").read_text())
    voices = [("Piper", i) for i in range(840, 846)] + [("eSpeak", "en-us+f3"), ("Flite", "kal")]
    if split != "discovery":
        specs.update(
            {
                "Dizzy Zebra": {"ipa": "/ˈdɪzi ˈzibɹə/", "near": "busy zebra", "suffix": "dizzy zebras"},
                "Bright Torch": {"ipa": "/bɹaɪt tɔɹtʃ/", "near": "bright porch", "suffix": "bright torches"},
            }
        )
        voices = [("Piper", i) for i in range(830, 836)] + [("eSpeak", "en-us+m6"), ("Flite", "kal16")]
    heads = {n: Head(compile_head(v["ipa"]).SerializeToString()) for n, v in specs.items()}
    guarded = Head(compile_head(specs["Brown Goose"]["ipa"], reject=["/bɹaʊn dʒuːs/"]).SerializeToString())
    prompts_for = list(specs)
    if split != "discovery":
        prompts_for = prompts_for[:6] if split == "confirm-a" else prompts_for[6:]
    symbols = set().union(*(tokenize(v["ipa"]) for v in specs.values()))
    print("Distinct programmed phone labels", len(symbols), " ".join(sorted(symbols)), flush=True)
    encoder = checked_encoder()
    stats = Counter()
    rng = np.random.default_rng({"discovery": 23519, "confirm-a": 41791, "confirm-b": 41792}[split])
    for engine, speaker in voices:
        for name in prompts_for:
            entry = specs[name]
            prompts = [
                (name, "clean"),
                (name, "noise"),
                (entry["near"], "near"),
                (name.split()[-1], "bare"),
                (entry["suffix"], "suffix"),
            ]
            for text, kind in prompts:
                condition = kind if kind in ("clean", "noise") else rng.choice(("clean", "quiet", "noise"))
                x, _ = render(rng, engine, speaker, text, condition, round(rng.uniform(0.85, 1.15), 2))
                logits = encoder.logits(x)
                predicted = {n for n, h in heads.items() if h.scores(logits).max() >= h.cutoff}
                if name == "Brown Goose":
                    stats["Brown Goose with reject", kind] += guarded.scores(logits).max() >= guarded.cutoff
                stats[name, kind] += name in predicted if kind in ("clean", "noise") else bool(predicted)
                stats[name, "cross"] += bool(predicted - {name}) if kind in ("clean", "noise") else 0
        print("finished", engine, speaker, flush=True)
    print("phrase | clean/8 | noise/8 | wrong head/16 | near/8 | bare/8 | suffix/8")
    for name in prompts_for + (["Brown Goose with reject"] if "Brown Goose" in prompts_for else []):
        print(
            name,
            *(stats[name, key] for key in ("clean", "noise", "cross", "near", "bare", "suffix")),
            sep=" | ",
        )
    assert sum(stats[n, "clean"] for n in specs) > len(specs)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "split", nargs="?", choices=("discovery", "confirm-a", "confirm-b"), default="discovery"
    )
    main(parser.parse_args().split)
