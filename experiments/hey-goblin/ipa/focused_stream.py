"""Compare the focused and simple recipes on fresh continuous synthetic speech."""
from collections import Counter
import json
from pathlib import Path
import tempfile

import numpy as np
import onnx

from acoustic import RATE
from bank import BankEncoder, build_bank
from corpus import render
from detect import Stream
from focused_eval import NAMES
from stream_eval import play

HERE = Path(__file__).resolve().parent


def main():
    basic = json.loads((HERE / "recipes.json").read_text())
    focused = json.loads((HERE / "focused.json").read_text())
    specs = {f"{mode}/{n}": source[n] for mode, source in (("simple", basic), ("focused", focused)) for n in NAMES}
    with tempfile.TemporaryDirectory() as folder:
        path = Path(folder) / "wake.onnx"
        onnx.save(build_bank(specs), path)
        encoder = BankEncoder(path)
    stats = Counter()
    delays = {}
    elapsed = Counter()
    total = 0.
    for scene, (engine, voice) in enumerate([("Piper", 856), ("Piper", 857), ("eSpeak", "en-us+f1"), ("Flite", "rms")]):
        rng = np.random.default_rng(70419 + scene)
        prompts = [(n, n, c) for n in NAMES for c in ("clean", "quiet", "noise")]
        prompts += [(c, "", rng.choice(("clean", "quiet", "noise"))) for c in
                    ("Say Hermes", "Hey gobbling", "Our goblin figurine", "Hey armies", "Hey goblins", "Hey Muse",
                     "The radio is on", "They say Hermes is here")]
        rng.shuffle(prompts)
        pieces, anchors = [], []
        length = 0
        for text, target, condition in prompts:
            clip, end = render(rng, engine, voice, text, condition, round(rng.uniform(.8, 1.2), 2))
            pieces.append(clip)
            if target:
                anchors.append((target, length + end))
            length += len(clip)
        signal = np.concatenate(pieces)
        total += length / RATE
        for denoise in (False, True):
            events, runtime = play(Stream(encoder, encoder.heads, denoise=denoise), signal)
            elapsed[denoise] += runtime
            for mode in ("simple", "focused"):
                matched = set()
                lags = delays.setdefault((mode, denoise), [])
                count = 0
                for event in events:
                    source, name = event.name.split("/", 1)
                    if source != mode:
                        continue
                    count += 1
                    match = next((i for i, (n, end) in enumerate(anchors) if i not in matched and n == name
                                  and -.30 <= (event.end_sample - end) / RATE <= .35
                                  and -.05 <= (event.decision_sample - end) / RATE <= 1.25), None)
                    if match is not None:
                        matched.add(match)
                        lags.append((event.decision_sample - anchors[match][1]) * 1000 / RATE)
                stats[mode, denoise, "hits"] += len(matched)
                stats[mode, denoise, "extra"] += count - len(matched)
        print("finished", engine, voice, flush=True)
    for mode, denoise in delays:
        lags = delays[mode, denoise]
        print(mode, "denoise", denoise, "matches", stats[mode, denoise, "hits"], "/24 extra",
              stats[mode, denoise, "extra"], "median decision ms", round(np.median(lags)) if lags else None)
    print("seconds", round(total, 2), "RTF before/after filtering",
          round(elapsed[False] / total, 3), round(elapsed[True] / total, 3))


if __name__ == "__main__":
    main()
