"""Generate synthetic 16 kHz mono PCM fixtures in memory."""

import io
import json
import math
import multiprocessing
import os
import subprocess
import wave
from dataclasses import dataclass
from functools import cache, lru_cache
from pathlib import Path

import numpy as np
import onnxruntime as ort
from piper.config import PiperConfig, SynthesisConfig
from piper.voice import PiperVoice
from scipy.signal import chirp, lfilter, resample_poly


RATE = 16_000
RECIPE = json.loads(Path(os.environ.get("WAKE_RECIPE", Path(__file__).with_name("phrases.json"))).read_text())
TARGETS = tuple(RECIPE["targets"])
OLD = "Hey Muse"
ALIASES = RECIPE.get("aliases", {})
if not isinstance(ALIASES, dict) or set(ALIASES) - set(TARGETS):
    raise ValueError("each aliases key must name a target")
if any(not isinstance(v, list) or not all(isinstance(a, str) for a in v) for v in ALIASES.values()):
    raise ValueError("aliases must be lists of alternative spellings")
NEAR, OTHER = RECIPE["near"], RECIPE["other"]
all_phrases = [*TARGETS, OLD, *NEAR, *OTHER, *RECIPE["test_near"], *RECIPE["test_other"]]
if not TARGETS or any(not isinstance(p, str) or not p.strip() for p in all_phrases):
    raise ValueError("the recipe needs nonempty phrase strings and at least one target")
if len({p.strip().casefold() for p in all_phrases}) != len(all_phrases):
    raise ValueError("targets and control phrases must be distinct")


@cache
def piper():
    options = ort.SessionOptions()
    options.intra_op_num_threads = 1
    options.inter_op_num_threads = 1
    session = ort.InferenceSession("/opt/voice/voice.onnx", options, providers=["CPUExecutionProvider"])
    config = PiperConfig.from_dict(json.loads(Path("/opt/voice/voice.json").read_text()))
    return PiperVoice(session=session, config=config)


@lru_cache(maxsize=64)
def speech(engine, voice, phrase, pace):
    if engine == "Piper":
        config = SynthesisConfig(speaker_id=voice, length_scale=pace, noise_scale=0, noise_w_scale=0)
        chunks = list(piper().synthesize(phrase, config))
        samples = np.concatenate([c.audio_float_array for c in chunks])
        rate = chunks[0].sample_rate
    else:
        if engine == "Flite":
            command = [
                "flite", "-voice", voice, "--setf", f"duration_stretch={pace}",
                "-t", phrase, "-o", "/dev/stdout",
            ]
        else:
            command = ["espeak-ng", "--stdout", "-v", voice, "-s", str(round(165 / pace)), phrase]
        with wave.open(io.BytesIO(subprocess.check_output(command))) as wav:
            if wav.getsampwidth() != 2 or wav.getnchannels() != 1:
                raise ValueError("expected mono signed 16-bit synthesis")
            rate = wav.getframerate()
            samples = np.frombuffer(wav.readframes(wav.getnframes()), dtype="<i2").astype(float)
    divisor = math.gcd(RATE, rate)
    samples = resample_poly(samples, RATE // divisor, rate // divisor)
    active = np.flatnonzero(np.abs(samples) > 0.008 * np.max(np.abs(samples)))
    if not len(active):
        raise ValueError(f"empty synthesis for {engine} {voice} {phrase}")
    return samples[active[0]:active[-1] + 1]


def pcm(signal):
    return np.rint(np.clip(signal, -1, 1) * 32767).astype("<i2")


@dataclass
class Clip:
    kind: str
    phrase: str
    engine: str
    condition: str
    samples: np.ndarray
    end: int  # first sample after speech


def render(rng, engine, voice, phrase, condition, pace):
    clean = speech(engine, voice, phrase, pace)
    rms = np.sqrt(np.mean(clean ** 2))
    level = rng.uniform(-38, -30) if condition == "quiet" else rng.uniform(-24, -16)
    clean = clean * (10 ** (level / 20) / rms)
    lead = rng.integers(RATE // 4, 3 * RATE // 4)
    end = lead + len(clean)
    signal = np.pad(clean, (lead, 3 * RATE // 4))
    if condition == "noise":
        noise = lfilter([1], [1, -0.8], rng.normal(size=len(signal)))
        snr = rng.uniform(6, 16)
        signal += noise * (10 ** ((level - snr) / 20) / np.sqrt(np.mean(noise ** 2)))
    return pcm(signal), end


def nonspeech(rng, kind, seconds):
    t = np.arange(RATE * seconds) / RATE
    if kind == "silence":
        return pcm(np.zeros_like(t))
    if kind == "noise":
        signal = lfilter([1], [1, -rng.uniform(0, 0.97)], rng.normal(size=len(t)))
    elif kind == "hum":
        hz = rng.choice([50, 60, 100, 120])
        signal = np.sin(2 * np.pi * hz * t) + 0.3 * np.sin(4 * np.pi * hz * t)
    else:
        signal = chirp(t, rng.uniform(70, 500), seconds, rng.uniform(800, 6000))
    return pcm(signal * rng.uniform(0.015, 0.25) / np.max(np.abs(signal)))



def voice_batch(job):
    split, index, engine, voice = job
    seed = {"design": 1729, "calibrate": 2026, "test": 4099, "confirm": 14021}[split]
    rng = np.random.default_rng([seed, index])
    phrases = list(TARGETS) + [OLD] + list(NEAR) + list(OTHER)
    if split in ("test", "confirm"):
        phrases += RECIPE["test_near"] + RECIPE["test_other"]
    batch = []
    for phrase in phrases:
        if phrase in TARGETS:
            kind = "target"
            repeats = 6
        elif phrase == OLD:
            kind, repeats = "old", 2
        elif phrase in OTHER or phrase in RECIPE["test_other"]:
            kind, repeats = "other", 1
        else:
            kind, repeats = "near", 1
        for i in range(repeats):
            condition = ("clean", "quiet", "noise")[i % 3] if repeats >= 3 else rng.choice(("clean", "quiet", "noise"))
            samples, end = render(rng, engine, voice, phrase, condition, round(rng.uniform(.82, 1.18), 2))
            batch.append(Clip(kind, phrase, engine, condition, samples, end))
    return batch


def clips(split):
    groups = {
        "design": (range(16), ("en-us+m1", "en-gb+f1"), ()),
        "calibrate": (range(128, 134), ("en-us+m3",), ()),
        "confirm": (range(660, 669), ("en-gb+m4", "en-us+f5"), ("kal",)),
        "test": (range(512, 520), ("en-us+f4", "en-sc+m5"), ("slt", "rms", "awb")),
    }
    speakers, espeak, flite = groups[split]
    voices = [("Piper", v) for v in speakers] + [("eSpeak", v) for v in espeak] + [("Flite", v) for v in flite]
    jobs = [(split, i, engine, voice) for i, (engine, voice) in enumerate(voices)]
    with multiprocessing.get_context("spawn").Pool(4) as pool:
        for batch in pool.imap(voice_batch, jobs):
            yield from batch
    rng = np.random.default_rng({"design": 397, "calibrate": 734, "test": 543, "confirm": 7981}[split])
    for kind in ("silence", "noise", "hum", "sweep"):
        for _ in range(4):
            yield Clip("nonspeech", kind, "generated", kind, nonspeech(rng, kind, 3), 0)
