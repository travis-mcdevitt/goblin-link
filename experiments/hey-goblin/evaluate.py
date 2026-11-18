"""Measure the saved phrase models on synthetic voices excluded from design."""
from collections import Counter
from contextlib import ExitStack
import os
from pathlib import Path

import numpy as np
from musegadget.wakeword import WakeWordDetector

from corpus import ALIASES, RATE, TARGETS, clips, nonspeech, render
from edit import Head, STOCK, WEIGHT, filename, route
from letters import BASE, CUTOFF, acoustic_logits, compile_head, session

ROOT = Path(__file__).resolve().parent
MODELS = Path(os.environ.get("WAKE_MODELS", ROOT / "models"))



def events(detector, audio, chunk_size):
    for start in range(0, len(audio), chunk_size):
        chunk = audio[start:start + chunk_size]
        if detector.feed(chunk.tobytes()):
            yield start + len(chunk)


def measure():
    original = Head().model
    if (ROOT / "hey_muse.pte").read_bytes() != original:
        raise ValueError("stock model differs from the pinned download")
    template = route(original)
    acoustic = session(BASE)
    hits, counts = Counter(), Counter()
    with ExitStack() as stack:
        stock = stack.enter_context(WakeWordDetector(STOCK))
        native = {None: stock}
        continuous = {}
        continuous_hits = Counter()
        speech_samples = 0
        heads = {}
        for phrase in TARGETS:
            prefix = MODELS / filename(phrase)
            path = prefix.with_suffix(".pte")
            model = path.read_bytes()
            if model[:WEIGHT] != template[:WEIGHT] or len(model) != len(template):
                raise ValueError(f"{phrase}: unexpected acoustic weights or model size")
            native[phrase] = stack.enter_context(WakeWordDetector(path))
            continuous[phrase] = stack.enter_context(WakeWordDetector(path))
            path = prefix.with_suffix(".onnx")
            if path.read_bytes() != compile_head(phrase, ALIASES.get(phrase, ())).SerializeToString():
                raise ValueError(f"{phrase}: saved spelling head differs from compiler output")
            heads[phrase] = session(path)

        for c in clips("confirm"):
            if c.kind in ("near", "other", "old"):
                speech_samples += len(c.samples)
                for name, detector in continuous.items():
                    continuous_hits[name] += sum(1 for _ in events(detector, c.samples, RATE))
            row = (c.kind, c.phrase) if c.kind == "target" else (c.kind,)
            keys = [row]
            if c.kind == "near":
                keys.append(("near phrase", c.phrase))
            if c.kind == "target":
                keys.append(("voice", c.phrase, c.engine))
            for key in keys:
                counts[key] += 1
            logits = acoustic_logits(acoustic, c.samples)
            for name, detector in native.items():
                detector.reset()
                times = list(events(detector, c.samples, 480))
                fire = bool(times)
                if row == ("target", name):
                    fire = any(-2400 <= t - c.end <= 9600 for t in times)
                for key in keys:
                    hits[key, ("native", name)] += fire
            for name, head in heads.items():
                fire = float(head.run(None, {"logits": logits})[0]) >= CUTOFF
                for key in keys:
                    hits[key, ("CTC", name)] += fire

        names = [("native", p) for p in native] + [("CTC", p) for p in TARGETS]
        print("clips | count | " + " | ".join(f"{mode} {name or 'stock SDK'}" for mode, name in names))
        for key, n in counts.items():
            print(" / ".join(key), n, *(hits[key, name] for name in names), sep=" | ")
        print("continuous negative speech seconds", round(speech_samples / RATE, 1), "events", dict(continuous_hits))
        carrier_rng = np.random.default_rng(18976)
        for phrase in TARGETS:
            carriers = Counter()
            for voice in (700, 701, 702):
                audio, _ = render(carrier_rng, "Piper", voice, f"I said {phrase} just now", "clean", 1.)
                native[phrase].reset()
                carriers["native"] += bool(list(events(native[phrase], audio, 480)))
                score = heads[phrase].run(None, {"logits": acoustic_logits(acoustic, audio)})[0]
                carriers["CTC"] += float(score) >= CUTOFF
            print("carrier sentence with", phrase, "out of 3:", dict(carriers))
        rng = np.random.default_rng(6158)
        for detector in native.values():
            detector.reset()
        long_events = Counter()
        for i in range(120):
            block = nonspeech(rng, ("noise", "hum", "sweep", "silence")[i % 4], 15)
            for name, detector in native.items():
                long_events[name] += sum(1 for _ in events(detector, block, RATE))
        print("30-minute nonspeech stream (native wake counts)", dict(long_events))
        for name in TARGETS:
            assert long_events[name] == 0, name
            target = ("target", name)
            assert hits[target, ("native", name)] > hits[target, ("native", None)], name
            for detector in (("native", name), ("CTC", name)):
                assert hits[target, detector] > counts[target] / 2, detector
                if counts[("near",)]:
                    assert hits[("near",), detector] < counts[("near",)] / 3, detector
                assert hits[("nonspeech",), detector] < counts[("nonspeech",)] / 4, detector
                for other in set(TARGETS) - {name}:
                    other_target = ("target", other)
                    assert hits[other_target, detector] < counts[other_target] / 4, detector


if __name__ == "__main__":
    measure()
