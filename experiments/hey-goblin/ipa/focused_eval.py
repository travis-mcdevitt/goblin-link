"""Confirm pronunciation and competitor edits on additional synthetic voices."""

from collections import Counter
import json
from pathlib import Path
import sys

import numpy as np

from compile_ipa import compile_head
from corpus import render
from detect import Head, checked_encoder
from denoise import suppress_noise

HERE = Path(__file__).resolve().parent
NAMES = ("Hey Goblin", "Hey Hermes")
CONTROL = [
    "Say Hermes",
    "Hey armies",
    "Hey Hermes's",
    "Hi Hermes",
    "OK Hermes",
    "Hey Herman",
    "They say Hermes is here",
    "The goblin",
    "Hey gobbling",
    "Hey goblins",
    "Hey goblin's",
    "A goblin",
    "Hey Robin",
    "I got a helmet",
]


def main(shard, final=False):
    specs = json.loads((HERE / "focused.json").read_text())
    final_specs = json.loads((HERE / "english-wakes.json").read_text()) if final else specs
    comparison = "previous" if final else "variants"
    methods = ("default", comparison, "focused", "denoised")
    models = {}
    for name, spec in specs.items():
        for method, variants, reject in (
            ("default", [], []),
            (comparison, spec["variants"], spec["reject"] if final else []),
            ("focused", spec["variants"], final_specs[name]["reject"]),
        ):
            models[name, method] = Head(
                compile_head(spec["ipa"], variants, reject=reject).SerializeToString()
            )
    voices = [("Piper", i) for i in range(780, 798)]
    voices += [("eSpeak", v) for v in ("en-us+m7", "en-us+f5", "en-gb+m7", "en-sc+f4")]
    controls = CONTROL
    if final:
        voices = [("Piper", i) for i in range(860, 872)] + [("eSpeak", "en-gb+m6"), ("eSpeak", "en-sc+f2")]
        controls = CONTROL + ["Pay Hermes", "Try Hermes", "Hail Hermes", "Play Hermes"]
    voices = voices[shard::2]
    rng = np.random.default_rng((138171 if final else 90713) + shard)
    encoder = checked_encoder()
    stats = Counter()
    prompts = [(n, n, condition) for n in NAMES for condition in ("clean", "quiet", "noise")]
    prompts += [(n + ", stop", n, "clean") for n in NAMES]
    if final:
        prompts += [("I said " + n + " just now", n, "clean") for n in NAMES]
    positives_per_name = len(prompts) // len(NAMES)
    prompts += [(n, "", None) for n in controls]
    print("columns", *methods, flush=True)
    for engine, voice in voices:
        for text, target, condition in prompts:
            actual = condition or rng.choice(("clean", "quiet", "noise"))
            pcm, _ = render(rng, engine, voice, text, actual, round(rng.uniform(0.80, 1.20), 2))
            logits = encoder.logits(pcm)
            filtered = suppress_noise(pcm)
            clean_logits = logits if filtered is pcm else encoder.logits(filtered)
            group = target or "controls"
            stats[group, "total"] += 1
            stats[text, "total text"] += 1
            if target:
                stats["positive " + actual, "total"] += 1
            for method in methods:
                key, frames = ("focused", clean_logits) if method == "denoised" else (method, logits)
                fires = {n for n in NAMES if models[n, key].scores(frames).max() >= models[n, key].cutoff}
                hit = target in fires if target else bool(fires)
                stats[group, method] += hit
                stats[text, method + " text"] += hit
                if target:
                    stats["wrong", method] += bool(fires - {target})
                    stats["positive " + actual, method] += hit
        print("finished", engine, voice, flush=True)
    for category in (*NAMES, "controls", "wrong", "positive clean", "positive quiet", "positive noise"):
        print(
            "summary",
            category,
            stats[category, "total"]
            if category != "wrong"
            else len(NAMES) * positives_per_name * len(voices),
            *(stats[category, x] for x in methods),
            sep=" | ",
        )
    for text in controls:
        print(
            "control",
            text,
            stats[text, "total text"],
            *(stats[text, x + " text"] for x in methods),
            sep=" | ",
        )
    assert all(stats[n, "total"] == positives_per_name * len(voices) for n in NAMES)


if __name__ == "__main__":
    main(int(sys.argv[1]), "--final" in sys.argv[2:])
