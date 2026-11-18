"""Check compiled numerical behavior, including suffix and chunk boundaries."""
import json
from pathlib import Path

import numpy as np

from compile_ipa import compile_head
from design import Settings, timeline
from detect import Head, Stream
from denoise import suppress_noise
from phones import CONSONANTS, VOWELS, VOCAB, acoustic_paths, tokenize

HERE = Path(__file__).resolve().parent
BLANK = "<pad>"


def acoustic_tape(labels):
    z = np.full((len(labels), len(VOCAB)), -24, np.float32)
    for i, phone in enumerate(labels):
        z[i, VOCAB[phone]] = 0
    return z


def checked_head(ipa, variants=()):
    return Head(compile_head(ipa, variants).SerializeToString())


def verify():
    assert tokenize("/ˈgɑb.lɪn/") == tuple("ɡɑblɪn")
    assert tokenize("[t͡ʃɝt͜ʃ]") == ("tʃ", "ɚ", "tʃ")
    for invalid in ("/你好/", "", "ˈ", "h" * 49, "/heɪ]", "h2"):
        try:
            tokenize(invalid)
        except ValueError:
            continue
        raise AssertionError(f"accepted {invalid!r}")

    for symbol in CONSONANTS + VOWELS:
        head = checked_head(symbol)
        labels = list(tokenize(symbol)) + [BLANK] * 25
        assert max(head.scores(acoustic_tape(labels))) >= head.cutoff, symbol
        assert max(head.scores(acoustic_tape([BLANK] * 30))) < head.cutoff, symbol

    silence = np.zeros(16000, np.int16)
    assert suppress_noise(silence) is silence
    noise = np.random.default_rng(9).integers(-2000, 2000, 16007, dtype=np.int16)
    restored = suppress_noise(noise)
    assert restored.shape == noise.shape and restored.dtype == np.int16
    assert np.std(restored) < np.std(noise)

    recipes = json.loads((HERE / "recipes.json").read_text())
    for name, recipe in recipes.items():
        ipa = recipe["ipa"]
        phones = tokenize(ipa)
        model = compile_head(ipa)
        assert model.SerializeToString() == compile_head(ipa).SerializeToString()
        head = Head(model.SerializeToString())
        for size in (1, 9, 35, 110):
            logits = np.random.default_rng(size).normal(0, 2, (size, len(VOCAB))).astype(np.float32)
            np.testing.assert_allclose(head.scores(logits), timeline(logits, phones, Settings()), atol=3e-5)
        def fires(labels):
            return np.any(head.scores(acoustic_tape(labels)) >= head.cutoff)
        tail = [BLANK] * 24
        text = list(phones)
        assert fires(text + tail), name
        assert not fires(text), name
        assert not fires([BLANK] * 30), name
        suffix = ["ə", "z"] if phones[-1] == "z" else ["z"]
        assert not fires(text + suffix + tail), name
        assert fires(text + ["s", "t", "ɑ", "p"] + tail), name
        assert fires(text + tail + text + suffix + tail), name
    repeated = checked_head("/nn/")
    assert np.max(repeated.scores(acoustic_tape(["n"] * 2 + tail))) < repeated.cutoff
    assert np.max(repeated.scores(acoustic_tape(["n", BLANK, "n"] + tail))) >= repeated.cutoff
    variants = checked_head("/kæt/", ["/dɔɡ/"])
    assert np.max(variants.scores(acoustic_tape(["d", "ɔ", "ɡ"] + tail))) >= variants.cutoff
    assert np.max(variants.scores(acoustic_tape(["k", "æ", "t"] + tail))) >= variants.cutoff

    assert ("k", "ɛ", "t", "ə", "l") in acoustic_paths("/kɛtəl/")
    assert ("k", "ɛ", "t", "əl") in acoustic_paths("/kɛtl̩/")
    assert len(acoustic_paths("/nɔɹθ stɑɹ/")) == 4
    contrast = Head(compile_head("/bɹaʊn ɡuːs/", reject=["/bɹaʊn dʒuːs/"]).SerializeToString())
    good = list(tokenize("/bɹaʊn ɡuːs/"))
    bad = list(tokenize("/bɹaʊn dʒuːs/"))
    assert max(contrast.scores(acoustic_tape(good + tail))) >= contrast.cutoff
    assert max(contrast.scores(acoustic_tape(bad + tail))) < contrast.cutoff
    assert max(contrast.scores(acoustic_tape(good + tail * 2 + bad + tail))) >= contrast.cutoff

    class TapeEncoder:
        def logits(self, pcm):
            # First sample per hop contains a phone ID. Follow the acoustic
            # model's receptive field length, including a partial final hop.
            ids = pcm[0:len(pcm) - 399:320]
            z = np.full((len(ids), len(VOCAB)), -24, np.float32)
            z[np.arange(len(ids)), ids] = 0
            return z

    head = checked_head("/heɪ ɡɑblɪn/")
    text = list(tokenize("/heɪ ɡɑblɪn/"))
    labels = tail + text + tail * 4 + text + ["z"] + tail * 4 + text + tail
    pcm = np.repeat([VOCAB[s] for s in labels], 320).astype(np.int16)
    pcm = np.pad(pcm, (0, 127), constant_values=VOCAB[BLANK])
    def simulate(chunk):
        stream = Stream(TapeEncoder(), {"phrase": head})
        events = []
        for i in range(0, len(pcm), chunk):
            events.extend(stream.feed(pcm[i:i + chunk]))
        events.extend(stream.finish())
        assert not stream.finish()
        return events
    events = simulate(len(pcm))
    assert len(events) == 2 and all(x.decision_sample > x.end_sample for x in events)
    assert simulate(731) == events
    print("Verified IPA parsing, deterministic weights, numeric parity, variants, suffixes, and streaming.", flush=True)


if __name__ == "__main__":
    verify()
