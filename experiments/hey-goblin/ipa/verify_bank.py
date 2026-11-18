"""Compare one-file, projection-edited inference with the unspecialized CPU graph."""

import json
import hashlib
from pathlib import Path
import tempfile
import subprocess
import sys

import numpy as np
import onnx

from bank import BankEncoder, build_bank
from compile_ipa import compile_head
from detect import Head, checked_encoder
from phones import VOCAB

HERE = Path(__file__).resolve().parent


def main():
    from synth_ipa import espeak_input, synthetic
    from qualify import piper_collisions

    assert espeak_input("/æt.ʃi/") == "[[aat|Si:]]"
    assert espeak_input("/ætʃi/") == "[[aatSi:]]"
    assert espeak_input("/ætˈʃi/") == "[[aat|S'i:]]"
    assert not np.array_equal(synthetic("/æt.ʃi/"), synthetic("/ætʃi/"))
    assert piper_collisions(["/ætʃi/"], ["/æt.ʃi/"]) == {"/æt.ʃi/"}
    specs = json.loads((HERE / "recipes.json").read_text())
    specs["alternate"] = {"ipa": "/kɛtəl/", "variants": ["/dɔɡ/"], "reject": ["/kɛtəm/"]}
    model = build_bank(specs)
    props = {p.key: p.value for p in model.metadata_props}
    keep = json.loads(props["phone_ids"])
    assert 1 < len(keep) < len(VOCAB)
    bias = next(v for v in model.graph.initializer if v.name == "lm_head.bias")
    assert list(bias.dims) == [len(keep)]
    portable = checked_encoder()
    separate = [
        Head(compile_head(s["ipa"], s.get("variants", []), reject=s.get("reject", [])).SerializeToString())
        for s in specs.values()
    ]
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
            np.testing.assert_allclose(actual, expected, atol=0.025, rtol=0.005)
            for j, head in enumerate(separate):
                np.testing.assert_array_equal(actual[:, j] >= head.cutoff, expected[:, j] >= head.cutoff)
        del bank, portable
        final = Path(folder) / "english-wakes.onnx"
        subprocess.run([sys.executable, "bank.py", "english-wakes.json", str(final)], check=True)
        with final.open("rb") as handle:
            print("Final bank SHA-256", hashlib.file_digest(handle, "sha256").hexdigest(), flush=True)
        for tested, options in ((path, []), (final, ["--piper-speaker", "810"])):
            for name, ipa in (("Hey Goblin", "/heɪ ˈɡɑblɪn/"), ("Hey Hermes", "/heɪ ˈhɝmiz/")):
                wav = Path(folder) / (name + ".wav")
                subprocess.run([sys.executable, "synth_ipa.py", ipa, str(wav), *options], check=True)
                cli = subprocess.run(
                    [sys.executable, "detect.py", str(wav), "--bank", str(tested)],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                names = [json.loads(line)["phrase"] for line in cli.stdout.splitlines()]
                assert all(x == name for x in names)
                if options or name == "Hey Goblin":
                    assert names == [name]
                print("IPA-synthesized CLI", tested.name, name, names, flush=True)
        print(
            "Edited acoustic projection",
            len(VOCAB),
            "->",
            len(keep),
            "rows.",
            "Verified seven synthetic PCM clips against separate heads.",
            flush=True,
        )


if __name__ == "__main__":
    main()
