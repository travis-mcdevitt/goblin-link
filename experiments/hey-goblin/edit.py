"""Write a new readout into the pinned SDK model from synthetic activations."""

import argparse
import ctypes as ct
import hashlib
import os
import re
import struct
import tempfile
from collections import defaultdict
from pathlib import Path

import numpy as np
from musegadget.wakeword import WakeWordDetector

from corpus import ALIASES, TARGETS, clips


STOCK = Path("/opt/sdk/esp32/components/muse_wakeword/models/hey_muse.pte")
STOCK_SHA = "ad31331ef5d1782b60952df634c32a4c065492a8c635b240982bb681b6b95cbc"
# XTS3 op 30, LINEAR(58, 59, 60) -> 61. Tensor 59 is [1, 544] int8,
# tensor 60 is [1] int32 LE. Op 31 maps tensor 61 through its sigmoid table.
WEIGHT = 22944
BIAS = 23488
STEPS = 17
CHANNELS = 32


class Head:
    def __init__(self):
        self.model = STOCK.read_bytes()
        if hashlib.sha256(self.model).hexdigest() != STOCK_SHA:
            raise ValueError("the tensor offsets apply only to the pinned SDK checkpoint")
        self.weights = np.frombuffer(self.model[WEIGHT:BIAS], np.int8).astype(np.int32)
        self.bias, = struct.unpack_from("<i", self.model, BIAS)
        self.multiplier, shift = struct.unpack_from("<ii", self.model, 11410)
        self.right = -shift
        self.sigmoid = np.frombuffer(self.model, np.int8, 256, 11418).astype(np.int16) + 128

    def probability_bytes(self, accumulator):
        # ESP-NN rounds once in its high multiply and again in the right shift.
        high = (accumulator.astype(np.int64) * self.multiplier + (1 << 30)) >> 31
        scaled = (high + (1 << (self.right - 1)) - (high < 0)) >> self.right
        return self.sigmoid[np.clip(scaled + 88 + 128, 0, 255)]

    def write(self, weights, bias, path):
        result = bytearray(self.model)
        result[WEIGHT:BIAS] = weights.astype(np.int8).tobytes()
        struct.pack_into("<i", result, BIAS, bias)
        path.write_bytes(result)


class Probe:
    def __init__(self, detector, head):
        self.detector = detector
        self.head = head
        self.steps = []
        self.scores = []
        step = (ct.c_int8 * CHANNELS)()
        read = detector._library.goblin_read_step
        read.argtypes = [ct.c_void_p, ct.POINTER(ct.c_int8)]
        read.restype = ct.c_bool

        @ct.CFUNCTYPE(None, ct.c_void_p, ct.c_float)
        def observe(ctx, probability):
            if not read(detector._handle, step):
                self.routing_error = True
            self.steps.append(bytes(step))
            self.scores.append(round(probability * 256))

        self.callback = observe  # the native runner borrows the callback
        hook = detector._library.ww_runner_set_score_hook
        hook.argtypes = [ct.c_void_p, type(observe), ct.c_void_p]
        hook.restype = None
        hook(detector._handle, observe, None)

    def windows(self, clip):
        self.routing_error = False
        self.steps.clear()
        self.scores.clear()
        self.detector.reset()
        self.detector.feed(clip.samples.tobytes())
        if self.routing_error:
            raise RuntimeError("programmed delays disagree with the previous block’s state")
        steps = np.frombuffer(b"".join(self.steps), np.int8).reshape(-1, CHANNELS)
        # The SDK starts each state at -128, which represents zero activation.
        steps = np.pad(steps.astype(np.int16) + 128, ((STEPS - 1, 0), (0, 0))).astype(np.uint8)
        frames = np.lib.stride_tricks.sliding_window_view(steps, (STEPS, CHANNELS)).reshape(-1, STEPS * CHANNELS)
        stock_scores = self.head.probability_bytes(frames @ self.head.weights + self.head.bias)
        if not np.array_equal(stock_scores, self.scores):
            raise RuntimeError("reconstructed head inputs disagree with the SDK score trace")
        return frames


def firing_bias(head, dot):
    # For one clip the five-step decision is monotone in the integer bias.
    lo, hi = -int(dot.max()) - 300_000, -int(dot.min()) + 300_000
    while hi - lo > 1:
        mid = (lo + hi) // 2
        probs = head.probability_bytes(dot + mid)
        sums = np.convolve(probs, np.ones(5, dtype=np.int16), "valid")
        if np.max(sums) > 0.22 * 5 * 256:
            hi = mid
        else:
            lo = mid
    return hi


def route(model):
    """Program the last mixed-convolution block as a bank of acoustic delays."""
    result = bytearray(model)
    # SDK op 25 has 23 temporal taps per channel. Alternating groups retain
    # context from the preceding syllable and the current syllable.
    depth = np.zeros((23, 32), dtype=np.int8)
    for c in range(32):
        depth[22 - (c % 4) * 3, c] = 1
    # Op 26 copies each channel. Biases compensate the stored zero points;
    # Q31 multiplier 1/2 with a left shift of one implements unit gain.
    for offset, values in (
        (20928, depth), (21664, np.full(32, -131, dtype="<i4")),
        (21792, np.eye(32, dtype=np.int8)), (22816, np.full(32, 131, dtype="<i4")),
    ):
        result[offset:offset + values.nbytes] = values.tobytes()
    for offset in (10382 + 20, 10658 + 20):
        struct.pack_into("<64i", result, offset, *([1 << 30] * 32 + [1] * 32))
    return bytes(result)


def collect(probe, corpus):
    records = []
    for clip in corpus:
        frames = probe.windows(clip)
        time = 800 + 480 * np.arange(len(frames)) - clip.end
        records.append((clip.kind, frames.copy(), time))
    return records


def fit(records, phrase):
    pos, neg = [], []
    for kind, x, time in records:
        if kind == phrase:
            pos.append(x[(time >= -960) & (time <= 1440)])
            neg.append(x[time < -5000][::5])
        else:
            neg.extend((x[::5], x[(time >= -1920) & (time <= 2400)]))
    positive = np.concatenate(pos).astype(float)
    background = np.concatenate(neg)
    hard = background
    # Recompute a linear discriminant on the most confusable window in each
    # negative clip. Otherwise silence dominates the negative average.
    for _ in range(3):
        negative = hard.astype(float)
        direction = positive.mean(0) - negative.mean(0)
        cov = (np.cov(positive, rowvar=False) + np.cov(negative, rowvar=False)) * 0.5
        ridge = 0.14 * np.trace(cov) / len(direction)
        weights = np.linalg.solve(cov + ridge * np.eye(len(direction)), direction)
        weights = np.rint(120 * weights / max(abs(weights))).astype(np.int32)
        peaks = []
        for kind, x, time in records:
            if kind == phrase:
                x = x[time < -5000]
            if len(x) >= 5:
                start = np.argmax(np.convolve(x @ weights, np.ones(5), "valid"))
                peaks.append(x[start:start + 5])
        hard = np.concatenate([background[::8], *peaks])
    return weights


def calibrate(head, records, phrase, weights):
    limits = defaultdict(list)
    for kind, frames, time in records:
        projection = frames @ weights
        if kind == phrase:
            projection = projection[(time >= -2400) & (time <= 8640)]
        limits[kind].append(firing_bias(head, projection))
    positives = np.array(limits.pop(phrase))
    # Equal-category error prevents the number of easy controls from deciding
    # the operating point. The generated nonspeech set must stay quiet.
    ceiling = min(limits["nonspeech"]) - 1
    choices = sorted({ceiling, *(int(v) for v in positives if v <= ceiling)})
    def quality(bias):
        false = [np.mean(np.asarray(group) <= bias) for group in limits.values()]
        return np.mean(positives <= bias) - 2 * max(false)
    bias = max(choices, key=quality)
    print(phrase, "calibration", sum(positives <= bias), "/", len(positives),
          {k: sum(v <= bias for v in vs) for k, vs in limits.items()}, flush=True)
    return bias


def filename(phrase):
    # Use a content-derived path for every target, including names that slug alike.
    slug = re.sub(r"[^a-z0-9]+", "_", phrase.casefold()).strip("_")[:48]
    return (slug or "phrase") + "_" + hashlib.sha256(phrase.encode()).hexdigest()[:8]


def build(out, source=clips):
    os.environ["MUSE_WAKEWORD_LIBRARY"] = "/opt/probe/libmuse_wakeword.so"
    head = Head()
    head.model = route(head.model)
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as scratch:
        routed = Path(scratch) / "routing.pte"
        routed.write_bytes(head.model)
        with WakeWordDetector(routed) as detector:
            probe = Probe(detector, head)
            design = collect(probe, source("design"))
            calibration = collect(probe, source("calibrate"))
    for phrase in TARGETS:
        weights = fit(design, phrase)
        bias = calibrate(head, calibration, phrase, weights)
        head.write(weights, bias, out / (filename(phrase) + ".pte"))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    out = parser.parse_args().output
    from letters import compile_head
    heads = {name: compile_head(name, ALIASES.get(name, ())) for name in TARGETS}
    build(out)
    for phrase, model in heads.items():
        (out / (filename(phrase) + ".onnx")).write_bytes(model.SerializeToString())
