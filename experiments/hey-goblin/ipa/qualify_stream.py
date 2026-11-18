"""Check one named phrase using its full integrated bank on continuous synthetic PCM."""

import argparse
import json
from pathlib import Path
import tempfile

import numpy as np
import onnx

from acoustic import RATE
from bank import BankEncoder, build_bank
from detect import Stream
from qualify import controls, piper_collisions
from stream_eval import play
from synth_ipa import render_ipa


def main(recipe, name):
    specs = json.loads(recipe.read_text())
    if name is None and len(specs) == 1:
        name = next(iter(specs))
    if name not in specs:
        raise ValueError("choose one recipe key with GOBLIN_IPA_NAME")
    spec = specs[name]
    other = [text for n, s in specs.items() if n != name for text in [s["ipa"], *s.get("variants", [])]]
    candidates = controls(spec, other, "confirmation")
    positive = [spec["ipa"], *spec.get("variants", [])]
    collisions = piper_collisions(positive, candidates)
    with tempfile.TemporaryDirectory() as folder:
        path = Path(folder) / "detector.onnx"
        onnx.save(build_bank(specs), path)
        encoder = BankEncoder(path)
    report = {"phrase": name, "targets": 0, "matched": 0, "extra": 0, "controls": 0}
    delays, tested_controls = [], set()
    for i, (speaker, voice) in enumerate(((623, "en-us"), (624, "en-us"), (None, "en-sc+m3"))):
        rng = np.random.default_rng(40177 + i)
        prompts = [
            (True, positive[(j + i) % len(positive)], c) for j, c in enumerate(("clean", "quiet", "noise"))
        ]
        # Keep a named check bounded even when the bank contains many phrases.
        available = [c for c in candidates if speaker is None or c not in collisions]
        selected = rng.choice(available, size=min(6, len(available)), replace=False)
        tested_controls.update(selected)
        prompts.extend((False, text, rng.choice(("clean", "quiet", "noise"))) for text in selected)
        rng.shuffle(prompts)
        samples, anchors = [], []
        length = 0
        for target, text, condition in prompts:
            pcm, end = render_ipa(
                text, voice, int(rng.integers(1 << 30)), round(rng.uniform(0.85, 1.15), 2), condition, speaker
            )
            if target:
                anchors.append(length + end)
            samples.append(pcm)
            length += len(pcm)
        events, _ = play(Stream(encoder, encoder.heads), np.concatenate(samples))
        report["targets"] += len(anchors)
        report["controls"] += len(selected)
        matched = set()
        for event in events:
            if event.name != name:
                continue
            hit = next(
                (
                    j
                    for j, end in enumerate(anchors)
                    if j not in matched
                    and -0.30 <= (event.end_sample - end) / RATE <= 0.35
                    and -0.05 <= (event.decision_sample - end) / RATE <= 1.25
                ),
                None,
            )
            if hit is None:
                report["extra"] += 1
            else:
                matched.add(hit)
                delays.append((event.decision_sample - anchors[hit]) * 1000 / RATE)
        report["matched"] += len(matched)
    report["control_pronunciations_tested"] = sorted(tested_controls)
    report["controls_indistinguishable_to_piper"] = sorted(collisions)
    report["median_decision_ms"] = round(float(np.median(delays))) if delays else None
    print(json.dumps(report, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("recipe", type=Path)
    parser.add_argument("--phrase")
    args = parser.parse_args()
    main(args.recipe, args.phrase)
