"""Compare one-file, projection-edited inference with the unspecialized CPU graph."""
import json
from pathlib import Path
import tempfile

import numpy as np
import onnx

from bank import BankEncoder, build_bank
from compile_ipa import compile_head
from detect import Head, checked_encoder
from phones import VOCAB

HERE = Path(__file__).resolve().parent


def main():
    specs = json.loads((HERE / "recipes.json").read_text())
    specs["alternate"] = {"ipa": "/kɛtəl/", "variants": ["/dɔɡ/"], "reject": ["/kɛtəm/"]}
    model = build_bank(specs)
    props = {p.key: p.value for p in model.metadata_props}
    keep = json.loads(props["phone_ids"])
    assert 1 < len(keep) < len(VOCAB)
    bias = next(v for v in model.graph.initializer if v.name == "lm_head.bias")
    assert list(bias.dims) == [len(keep)]
    portable = checked_encoder()
    separate = [Head(compile_head(s["ipa"], s.get("variants", []), reject=s.get("reject", [])).SerializeToString()) for s in specs.values()]
    with tempfile.TemporaryDirectory() as folder:
        path = Path(folder) / "bank.onnx"
        onnx.save(model, path)
        del model
        bank = BankEncoder(path)
        for i in (0, 1, 4, 7, 11, 21, 50):
            with np.load(HERE / "target/work/development" / f"{i}.npz") as f:
                x = f["pcm"]
            full = portable.logits(x)
            expected = np.stack([h.scores(full) for h in separate], axis=1)
            actual = bank.logits(x)
            assert actual.shape == expected.shape
            # Compare defined scores and numerical decisions. The upstream
            # quantized kernels can round differently with a narrower output.
            np.testing.assert_array_equal(actual == -10000, expected == -10000)
            np.testing.assert_allclose(actual, expected, atol=.025, rtol=.005)
            for j, head in enumerate(separate):
                np.testing.assert_array_equal(actual[:, j] >= head.cutoff, expected[:, j] >= head.cutoff)
        print("Edited acoustic projection", len(VOCAB), "->", len(keep), "rows.",
              "Verified seven synthetic PCM clips against separate heads.", flush=True)


if __name__ == "__main__":
    main()
