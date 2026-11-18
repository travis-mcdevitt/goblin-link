"""Run IPA wake heads on successive chunks of a synthetic mono 16 kHz WAV."""

import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import wave

import numpy as np
import onnxruntime as ort

from acoustic import CHECKPOINT, HOP, RATE, RECEPTIVE_FIELD, PhoneEncoder
from compile_ipa import REVISION
from denoise import suppress_noise
from phones import VOCAB, VOCAB_SHA
from portable import PORTABLE, PortableEncoder


class Head:
    def __init__(self, source):
        options = ort.SessionOptions()
        options.intra_op_num_threads = 1
        options.inter_op_num_threads = 1
        self.session = ort.InferenceSession(source, options, providers=["CPUExecutionProvider"])
        meta = self.session.get_modelmeta().custom_metadata_map
        if meta.get("revision") != REVISION or meta.get("vocab_sha256") != VOCAB_SHA:
            raise ValueError("head and acoustic model use different phone inventories")
        self.cutoff = float(json.loads(meta["settings"])["cutoff"])
        self.lookahead = int(json.loads(meta["settings"])["lookahead"])
        self.max_phones = int(meta["max_phones"])

    def scores(self, logits):
        if (
            logits.ndim != 2
            or logits.shape[0] < 1
            or logits.shape[1] != len(VOCAB)
            or logits.dtype != np.float32
            or not np.isfinite(logits).all()
        ):
            raise ValueError("expected a nonempty float32 matrix of finite acoustic logits")
        return self.session.run(["scores"], {"logits": logits})[0]


@dataclass(frozen=True)
class Event:
    name: str
    end_sample: int
    decision_sample: int
    score: float


class Stream:
    # Half a second of new audio per forward pass. The encoder is bidirectional,
    # so only score endpoints whose right context has already arrived.
    step = 24 * HOP
    cooldown = RATE

    def __init__(self, encoder, heads, denoise=False):
        if not heads:
            raise ValueError("provide at least one phrase head")
        self.encoder = encoder
        self.denoise = denoise
        self.heads = dict(heads)
        longest = max(h.max_phones for h in heads.values())
        lag = max(h.lookahead for h in heads.values())
        self.window = max(120, 9 * longest + lag + self.step // HOP + 5) * HOP
        self.buffer = np.empty(0, np.int16)
        self.received = 0
        self.next_step = self.step
        self.next_frame = {name: 0 for name in heads}
        self.last_event = {name: -self.cooldown for name in heads}

    def _advance(self):
        left = self.received - len(self.buffer)
        trim = (-left) % HOP
        left += trim
        pcm = self.buffer[trim:]
        logits = self.encoder.logits(suppress_noise(pcm) if self.denoise else pcm)
        events = []
        for name, head in self.heads.items():
            scores = head.scores(logits)
            first = max(0, self.next_frame[name] - left // HOP)
            limit = max(0, len(scores) - head.lookahead)
            for i in np.flatnonzero(scores[first:limit] >= head.cutoff) + first:
                endpoint = left + int(i) * HOP + RECEPTIVE_FIELD // 2
                if endpoint >= self.last_event[name] + self.cooldown:
                    events.append(Event(name, endpoint, self.received, float(scores[i])))
                    self.last_event[name] = endpoint
            self.next_frame[name] = max(self.next_frame[name], left // HOP + limit)
        return events

    def feed(self, pcm):
        if pcm.ndim != 1 or pcm.dtype != np.int16:
            raise ValueError("expected mono 16 kHz int16 PCM")
        events = []
        while len(pcm):
            take = min(len(pcm), self.next_step - self.received)
            self.buffer = np.concatenate((self.buffer, pcm[:take]))[-self.window :]
            self.received += take
            pcm = pcm[take:]
            if self.received == self.next_step:
                events.extend(self._advance())
                self.next_step += self.step
        return events

    def finish(self):
        # Flush only the actual partial step. Repeated calls cannot rescore an
        # endpoint and no synthetic silence is appended at the end of a file.
        if self.received % self.step and len(self.buffer) >= RECEPTIVE_FIELD:
            return self._advance()
        return []


def checked_encoder(reference=False, threads=6):
    folder = CHECKPOINT if reference else PORTABLE.parent
    if hashlib.sha256((folder / "vocab.json").read_bytes()).hexdigest() != VOCAB_SHA:
        raise ValueError("acoustic vocabulary checksum mismatch")
    return PhoneEncoder(threads) if reference else PortableEncoder(threads=threads)


def print_event(event):
    print(
        json.dumps(
            {
                "phrase": event.name,
                "end_s": round(event.end_sample / RATE, 4),
                "decision_s": round(event.decision_sample / RATE, 4),
                "score": round(event.score, 4),
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wav", type=Path)
    parser.add_argument("heads", nargs="*", type=Path)
    parser.add_argument("--bank", type=Path, help="one programmed PCM-to-scores ONNX file")
    parser.add_argument("--reference", action="store_true", help="use the full-precision lab checkpoint")
    parser.add_argument(
        "--denoise", action="store_true", help="apply spectral subtraction to each available window"
    )
    parser.add_argument("--threads", type=int, default=6)
    args = parser.parse_args()
    if args.threads < 1:
        parser.error("--threads must be positive")
    if bool(args.bank) == bool(args.heads) or (args.bank and args.reference):
        parser.error("supply either --bank or one or more separate heads")
    if args.bank:
        from bank import BankEncoder

        encoder = BankEncoder(args.bank, args.threads)
        heads = encoder.heads
    else:
        encoder = checked_encoder(args.reference, args.threads)
        heads = {str(p): Head(str(p)) for p in args.heads}
    with wave.open(str(args.wav), "rb") as wav:
        if (wav.getframerate(), wav.getnchannels(), wav.getsampwidth()) != (RATE, 1, 2):
            parser.error("supply 16 kHz mono signed 16-bit PCM")
        stream = Stream(encoder, heads, denoise=args.denoise)
        while chunk := wav.readframes(4096):
            for event in stream.feed(np.frombuffer(chunk, dtype="<i2")):
                print_event(event)
        for event in stream.finish():
            print_event(event)
