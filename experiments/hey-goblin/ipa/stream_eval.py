"""Measure event timing and false wakes with the deployed chunk-by-chunk runtime."""
import argparse
from collections import Counter
import json
from pathlib import Path
import time
import tempfile

import onnx

import numpy as np

from acoustic import RATE
from bank import BankEncoder, build_bank
from corpus import nonspeech, render
from detect import Head, Stream, checked_encoder
from evaluate import read_manifest
from fixtures import TARGETS

HERE = Path(__file__).resolve().parent


def load_heads():
    return {n: Head(str(HERE / "models" / (n.lower().replace(" ", "_") + ".onnx"))) for n in TARGETS}


def play(stream, signal):
    events = []
    start = time.monotonic()
    for i in range(0, len(signal), 1600):
        events.extend(stream.feed(signal[i:i + 1600]))
    events.extend(stream.finish())
    return events, time.monotonic() - start


def mixed():
    folder = HERE / "target/work/holdout"
    rows = read_manifest(folder, "holdout")
    with tempfile.TemporaryDirectory() as temporary:
        path = Path(temporary) / "detector.onnx"
        onnx.save(build_bank(json.loads((HERE / "recipes.json").read_text())), path)
        encoder = BankEncoder(path)
    heads = encoder.heads
    rng = np.random.default_rng(73519)
    found, expected, extra = Counter(), Counter(), Counter()
    delays, totals, runtime = [], 0, 0.
    for scene, engine in enumerate(("Piper", "eSpeak", "Flite") * 2):
        pieces = [np.zeros(int(rng.integers(400, 6900)), np.int16)]
        cursor = len(pieces[0])
        positives = []
        targets = list(TARGETS) * 2
        schedule = targets + ["near"] * 5 + ["suffix"] * 4 + ["unrelated"] * 2 + ["nonspeech"]
        rng.shuffle(schedule)
        for kind in schedule:
            if kind in TARGETS:
                candidates = [i for i, r in enumerate(rows) if r["target"] == kind and r["group"] == "target"
                              and r["voice"].startswith(engine + "/")]
            else:
                candidates = [i for i, r in enumerate(rows) if r["group"] == kind]
            i = int(rng.choice(candidates))
            with np.load(folder / f"{i}.npz") as f:
                clip = f["pcm"]
            if kind in TARGETS:
                positives.append((kind, cursor + rows[i]["end"]))
                expected[kind] += 1
            pieces.append(clip)
            cursor += len(clip)
        signal = np.concatenate(pieces)
        events, spent = play(Stream(encoder, heads), signal)
        runtime += spent
        totals += len(signal)
        matched = set()
        for e in events:
            match = next((i for i, (n, end) in enumerate(positives)
                          if n == e.name and i not in matched and -.30 <= (e.end_sample - end) / RATE <= .35
                          and -.05 <= (e.decision_sample - end) / RATE <= 1.25), None)
            if match is None:
                extra[e.name] += 1
            else:
                matched.add(match)
                found[e.name] += 1
                delays.append((e.decision_sample - positives[match][1]) * 1000 / RATE)
        print("scene", scene, engine, "seconds", round(len(signal) / RATE, 2),
              "matches", len(matched), "extra", len(events) - len(matched), flush=True)
    for n in TARGETS:
        print(n, found[n], "/", expected[n], "extra", extra[n])
    print("continuous seconds", round(totals / RATE, 2), "RTF", round(runtime / (totals / RATE), 3))
    if delays:
        print("decision delay ms p50/p90/range", np.round(np.percentile(delays, (50, 90, 0, 100))).astype(int))
    assert all(expected[n] == 12 and found[n] > 0 for n in TARGETS)


SUBJECTS = ["The blue kettle", "Our goblin figurine", "My green lantern", "The other courier",
            "Robin's paper bag", "The garden gate", "A silver coin", "Our radio",
            "The little night light", "Her jacket", "The first book", "Our holiday card"]
VERBS = ["was missing from", "seems too large for", "is still sitting on",
         "won't quite fit in", "used to belong in", "landed beside"]
PLACES = ["the kitchen drawer", "a pile of leaves", "the bookshelf", "a tin box",
          "my suitcase", "the old workshop", "the back seat", "a cabin by the sea"]
CHALLENGES = ["Hey goblins", "Hey gobbling", "Hey goblin's", "Hey Hermes's",
              "Hey armies", "Her knees", "Hey Herman", "Hey Robin", "A goblin",
              "The goblin", "Say Hermes", "Hi Merlin", "Hey Herbert", "Hey Muse",
              "Blue lanterns", "Blue pattern", "The lantern", "They blew land inward"]


def longform(shard):
    # Each seed selects new synthetic utterances and speaking rates. About one
    # utterance in four deliberately resembles a wake phrase.
    rng = np.random.default_rng(59031 + shard)
    voices = [("Piper", 900 + shard), ("eSpeak", ("en-us+m3", "en-gb+f3")[shard % 2]),
              ("Flite", ("awb", "rms", "slt")[shard % 3])]
    goal = 300 * RATE
    chunks, bounds = [], []
    length = 0
    while length < goal:
        group = rng.choice(("ordinary", "near", "nonspeech"), p=(.65, .25, .10))
        if group == "nonspeech":
            text = rng.choice(("silence", "hum", "noise", "sweep"))
            clip = nonspeech(rng, text, 3)
        else:
            text = (rng.choice(CHALLENGES) if group == "near" else
                    " ".join(rng.choice(values) for values in (SUBJECTS, VERBS, PLACES)))
            engine, speaker = voices[int(rng.integers(len(voices)))]
            clip, _ = render(rng, engine, speaker, text, rng.choice(("clean", "quiet", "noise")),
                             round(rng.uniform(.80, 1.20), 2))
        clip = clip[:goal - length]
        length += len(clip)
        chunks.append(clip)
        bounds.append((length, group, text))
    events, spent = play(Stream(checked_encoder(), load_heads()), np.concatenate(chunks))
    for e in events:
        _, group, text = next(b for b in bounds if b[0] > e.end_sample)
        print("false wake", e.name, round(e.end_sample / RATE, 2), group, text)
    print("shard", shard, "seconds", length / RATE, "utterances", len(bounds), "false wakes", len(events),
          "RTF", round(spent / (length / RATE), 3), flush=True)
    assert length == goal


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("shard", nargs="?", type=int)
    args = parser.parse_args()
    mixed() if args.shard is None else longform(args.shard)
