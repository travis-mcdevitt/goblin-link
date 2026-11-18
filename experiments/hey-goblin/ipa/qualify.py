"""Measure an arbitrary IPA recipe on separate synthetic development and confirmation voices."""
import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path

import numpy as np

from compile_ipa import compile_head
from design import Settings
from detect import Head, checked_encoder
from phones import plural_endings, tokenize
from synth_ipa import synthetic

HERE = Path(__file__).resolve().parent
GRID = (-.35, -.55, -.75, -.95, -1.15, -1.35, -1.65, -2., -2.5, -3., -4.)
SPEAKERS = {
    "development": ([604, 605, 606, 607], ["en-us+m3", "en-gb+f1"]),
    "confirmation": ([616, 617, 618, 619], ["en-us+f3", "en-gb+m4"]),
}


def controls(spec, other, stage):
    ipa = spec["ipa"].strip().strip("/[]")
    words = ipa.split()
    ending = plural_endings(tokenize(ipa)[-1])[0]
    prompts = [*spec.get("controls", []), *spec.get("reject", []), ipa + "".join(ending)]
    if len(words) > 1:
        prompts += [" ".join(words[1:]), " ".join(reversed(words))]
    prompts.extend(other)
    if stage == "confirmation":
        prompts.extend(spec.get("test_controls", []))
    positive = {tokenize(text) for text in [ipa, *spec.get("variants", [])]}
    unique = {}
    for text in prompts:
        phones = tokenize(text)
        if phones not in positive:
            unique.setdefault(phones, text)
    return list(unique.values())


def assess(name, spec, other, encoder, stage):
    cutoff = spec.get("cutoff", Settings().cutoff)
    head = Head(compile_head(spec["ipa"], spec.get("variants", []), Settings(cutoff=cutoff),
                             spec.get("reject", [])).SerializeToString())
    rivals = controls(spec, other, stage)
    if not rivals:
        raise ValueError(f"add at least one distinguishable control for {name!r}")
    positives = [spec["ipa"], *spec.get("variants", [])]
    piper, espeak = SPEAKERS[stage]
    speakers = [(i, "en-us") for i in piper] + [(None, v) for v in espeak]
    scores = defaultdict(list)
    for i, (piper_id, voice) in enumerate(speakers):
        prompts = [(True, positives[(i + j) % len(positives)], c) for j, c in enumerate(("clean", "quiet", "noise"))]
        prompts.extend((False, t, ("clean", "quiet", "noise")[(i + j) % 3]) for j, t in enumerate(rivals))
        for positive, text, condition in prompts:
            salt = json.dumps([name, stage, i, text, condition]).encode()
            seed = int.from_bytes(hashlib.sha256(salt).digest()[:4], "big")
            speed = round(np.random.default_rng(seed).uniform(.85, 1.15), 2)
            pcm = synthetic(text, voice, seed, speed, condition, piper_id)
            scores[positive].append(float(head.scores(encoder.logits(pcm)).max()))
    return np.array(scores[True]), np.array(scores[False])


def summary(scores, cutoff):
    pos, neg = scores
    return {"hits": int((pos >= cutoff).sum()), "positives": len(pos),
            "false": int((neg >= cutoff).sum()), "negatives": len(neg)}


def select(scores, default, limit):
    options = []
    for cutoff in sorted(set(GRID) | {default}):
        row = summary(scores, cutoff)
        if row["hits"] and row["false"] <= limit * row["negatives"]:
            options.append((row["hits"], -row["false"], -abs(cutoff - default), cutoff))
    return max(options)[-1] if options else None


def main(path, selected, max_false):
    if not 0 <= max_false <= 1:
        raise ValueError("max-false must be between zero and one")
    specs = json.loads(path.read_text())
    if not isinstance(specs, dict) or not specs or (selected and selected not in specs):
        raise ValueError("supply a named phrase in an existing recipe")
    encoder = checked_encoder()
    for name, spec in specs.items():
        if selected and name != selected:
            continue
        other = [s["ipa"] for n, s in specs.items() if n != name]
        if any(tokenize(s) == tokenize(spec["ipa"]) for s in other):
            raise ValueError(f"{name!r} shares its pronunciation with another wake phrase")
        for key in ("variants", "reject", "controls", "test_controls"):
            if not isinstance(spec.get(key, []), list) or not all(isinstance(v, str) for v in spec.get(key, [])):
                raise ValueError(f"{name!r} needs a list of IPA strings in {key}")
        default = spec.get("cutoff", Settings().cutoff)
        development = assess(name, spec, other, encoder, "development")
        chosen = select(development, default, max_false)
        confirmation = assess(name, spec, other, encoder, "confirmation")
        result = {"phrase": name, "starting_cutoff": default, "selected_cutoff": chosen,
                  "development_false_limit": max_false, "default_confirmation": summary(confirmation, default)}
        if chosen is not None:
            result["selected_development"] = summary(development, chosen)
            result["selected_confirmation"] = summary(confirmation, chosen)
        print(json.dumps(result, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("recipe", type=Path, nargs="?", default=HERE / "qualification.json")
    parser.add_argument("--phrase")
    parser.add_argument("--max-false", type=float, default=.05)
    args = parser.parse_args()
    main(args.recipe, args.phrase, args.max_false)
