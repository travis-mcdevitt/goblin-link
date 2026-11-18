"""Deterministic synthetic wake-phrase fixtures. All audio is generated in RAM."""
from dataclasses import dataclass
import multiprocessing

import numpy as np

from corpus import nonspeech, render

GOBLIN, HERMES, LANTERN = "Hey Goblin", "Hey Hermes", "Blue Lantern"
TARGETS = (GOBLIN, HERMES, LANTERN)
SPLITS = {
    "development": (range(208, 215), ("en-us+m2", "en-gb+f2"), (), 8123),
    "holdout": (range(740, 752), ("en-us+f4", "en-sc+m5"), ("awb", "rms", "slt"), 19031),
}


@dataclass
class Fixture:
    text: str
    group: str
    target: str
    voice: str
    condition: str
    end: int
    samples: np.ndarray


def utterances(split):
    phrases = TARGETS[:2] if split == "development" else TARGETS
    for name in phrases:
        yield name, "target", name, 4
        yield name + ", stop", "command", name, 2
        yield "I said " + name + " just now", "carrier", name, 1
    for text in ("Hey goblins", "Hey goblin's", "Hey gobbling", "Hey Hermes's"):
        yield text, "suffix", "", 2
    near = ["Hey gobble", "Hey goblet", "Hey Robin", "A goblin", "Goblin", "Hermes",
            "Hey Herman", "Hey Ernie", "Her knees", "Hey armies", "Hey", "Hey Muse"]
    other = ["The radio is on", "Close the window", "Call me in the morning"]
    if split == "holdout":
        near += ["Hey Merlin", "Say Hermes", "Hey Herbert", "Blue lanterns", "Blue pattern",
                 "Lantern", "The lantern"]
        other += ["Throw me the blue blanket", "They heard me talking"]
    for text in near:
        yield text, "near", "", 1
    for text in other:
        yield text, "unrelated", "", 1


def synthesize(job):
    split, index, engine, speaker = job
    rng = np.random.default_rng([SPLITS[split][-1], index])
    result = []
    for text, group, target, repeats in utterances(split):
        for i in range(repeats):
            condition = ("clean", "quiet", "noise")[i % 3] if repeats >= 3 else rng.choice(("clean", "quiet", "noise"))
            speed = round(rng.uniform(.78, 1.22), 2)
            pcm, end = render(rng, engine, speaker, text, condition, speed)
            result.append(Fixture(text, group, target, f"{engine}/{speaker}", str(condition), int(end), pcm))
    return result


def fixtures(split):
    piper, espeak, flite, seed = SPLITS[split]
    voices = [("Piper", i) for i in piper] + [("eSpeak", v) for v in espeak] + [("Flite", v) for v in flite]
    jobs = [(split, i, engine, speaker) for i, (engine, speaker) in enumerate(voices)]
    with multiprocessing.get_context("spawn").Pool(4) as pool:
        for batch in pool.imap(synthesize, jobs):
            yield from batch
    rng = np.random.default_rng(seed + 1)
    for kind in ("silence", "hum", "sweep", "noise"):
        for _ in range(5):
            yield Fixture(kind, "nonspeech", "", "generated", kind, 0, nonspeech(rng, kind, 3))
