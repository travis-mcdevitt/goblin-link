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
CONTROL = ["Say Hermes", "Hey armies", "Hey Hermes's", "Hi Hermes", "OK Hermes",
           "Hey Herman", "They say Hermes is here", "The goblin", "Hey gobbling",
           "Hey goblins", "Hey goblin's", "A goblin", "Hey Robin", "I got a helmet"]


def main(shard):
    specs = json.loads((HERE / "focused.json").read_text())
    models = {}
    for name, spec in specs.items():
        for method, variants, reject in (
            ("default", [], []), ("variants", spec["variants"], []),
            ("focused", spec["variants"], spec["reject"]),
        ):
            models[name, method] = Head(compile_head(spec["ipa"], variants, reject=reject).SerializeToString())
    voices = [("Piper", i) for i in range(780, 798)]
    voices += [("eSpeak", v) for v in ("en-us+m7", "en-us+f5", "en-gb+m7", "en-sc+f4")]
    voices = voices[shard::2]
    rng = np.random.default_rng(90713 + shard)
    encoder = checked_encoder()
    stats = Counter()
    prompts = [(n, n, condition) for n in NAMES for condition in ("clean", "quiet", "noise")]
    prompts += [(n + ", stop", n, "clean") for n in NAMES] + [(n, "", None) for n in CONTROL]
    for engine, voice in voices:
        for text, target, condition in prompts:
            actual = condition or rng.choice(("clean", "quiet", "noise"))
            pcm, _ = render(rng, engine, voice, text, actual, round(rng.uniform(.80, 1.20), 2))
            logits = encoder.logits(pcm)
            filtered = suppress_noise(pcm)
            clean_logits = logits if filtered is pcm else encoder.logits(filtered)
            group = target or "controls"
            stats[group, "total"] += 1
            stats[text, "total text"] += 1
            if target:
                stats["positive " + actual, "total"] += 1
            for method in ("default", "variants", "focused", "denoised"):
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
        print("summary", category, stats[category, "total"] if category != "wrong" else 8 * len(voices),
              *(stats[category, x] for x in ("default", "variants", "focused", "denoised")), sep=" | ")
    for text in CONTROL:
        print("control", text, stats[text, "total text"],
              *(stats[text, x + " text"] for x in ("default", "variants", "focused", "denoised")), sep=" | ")
    assert all(stats[n, "total"] == 4 * len(voices) for n in NAMES)


if __name__ == "__main__":
    main(int(sys.argv[1]))
