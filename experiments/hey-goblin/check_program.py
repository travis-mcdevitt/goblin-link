"""Small synthetic regressions for the phrase compiler and calibration labels."""
import json
import tempfile
from pathlib import Path

import numpy as np

from edit import Head, calibrate, firing_bias
from letters import VOCAB, compile_head, session


def ideal_logits(text, vocab):
    symbols = ["<pad>"]
    for letter in text.upper().replace(" ", "|"):
        symbols.extend([letter, "<pad>"])
    logits = np.full((len(symbols), len(vocab)), -100.0, np.float32)
    logits[np.arange(len(symbols)), [vocab[c] for c in symbols]] = 0
    return logits


def check():
    vocab = json.loads(VOCAB.read_text())
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "head.onnx"
        path.write_bytes(compile_head("Little Kettle").SerializeToString())
        head = session(path)
        for text, match in (("a little kettle here", True), ("kettle little", False),
                            ("little kettles", False), ("litle ketle", False)):
            score = head.run(None, {"logits": ideal_logits(text, vocab)})[0].item()
            assert (score == 0) == match, (text, score)

    # A user can choose a word such as “nonspeech” as the spoken target.
    head = Head()
    t = np.arange(8) * 480
    pos = np.full((8, 1), 180, np.int32)
    neg = np.zeros_like(pos)
    w = np.array([1000], np.int32)
    for name in ("nonspeech", "other", "old", "near"):
        bias = calibrate(head, [("target", name, pos, t),
                                ("nonspeech", "silence", neg, t)], name, w)
        assert firing_bias(head, pos @ w) <= bias < firing_bias(head, neg @ w)


if __name__ == "__main__":
    check()
