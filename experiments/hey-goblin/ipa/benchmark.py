"""Compare CPU acoustic inference on a fixed sample of development audio."""

import gc
import hashlib
import json
from pathlib import Path
import time

import numpy as np
from onnxruntime.capi.onnxruntime_pybind11_state import NotImplemented as UnsupportedKernel

from acoustic import RATE, PhoneEncoder
from compile_ipa import compile_head
from detect import Head
from portable import PORTABLE, PortableEncoder

HERE = Path(__file__).resolve().parent
OPTIONAL = {
    "uint8": "3d1479b5c70836645a4310902479f7f7e8bbc8c16c58745d918804f4f6f9d120",
    "int8": "74174710e34035bbb7f611601d016c32fc575de7a6f53b1078107dc10a84e7ae",
}


def main():
    folder = HERE / "target/work/development"
    rows = json.loads((folder / "manifest.json").read_text())["rows"]
    examples = []
    for i in np.random.default_rng(191).choice(len(rows), 32, replace=False):
        with np.load(folder / f"{i}.npz") as f:
            examples.append((f["pcm"], f["phones"]))
    recipes = json.loads((HERE / "recipes.json").read_text())
    heads = [Head(compile_head(p["ipa"]).SerializeToString()) for p in list(recipes.values())[:2]]
    total = sum(len(x) / RATE for x, _ in examples)
    sources = {"reference": None, "q4": PORTABLE}
    for name, digest in OPTIONAL.items():
        path = folder.parent / "backbones" / (name + ".onnx")
        if path.exists():
            with path.open("rb") as source:
                if hashlib.file_digest(source, "sha256").hexdigest() != digest:
                    raise ValueError(f"checksum mismatch for {path}")
            sources[name] = path
    for name, path in sources.items():
        for threads in (2, 4, 6):
            try:
                encoder = PhoneEncoder(threads) if path is None else PortableEncoder(path, threads)
            except UnsupportedKernel as error:
                if name not in OPTIONAL:
                    raise
                print(name, error, flush=True)
                break
            encoder.logits(examples[0][0])
            duration, changed, phones, all_phones = 0.0, 0, 0, 0
            for pcm, original in examples:
                start = time.monotonic()
                logits = encoder.logits(pcm)
                duration += time.monotonic() - start
                phones += (logits.argmax(1) != original.argmax(1)).sum()
                all_phones += len(logits)
                for head in heads:
                    changed += (head.scores(logits).max() >= head.cutoff) != (
                        head.scores(original).max() >= head.cutoff
                    )
            print(
                name,
                "threads",
                threads,
                "RTF",
                round(duration / total, 3),
                "changed decisions",
                changed,
                "/",
                len(heads) * len(examples),
                "changed frame labels",
                round(phones / all_phones, 3),
                flush=True,
            )
            del encoder
            gc.collect()


if __name__ == "__main__":
    main()
