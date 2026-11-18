"""Compare CPU inference settings on a fixed sample of development audio."""
import gc
import json
from pathlib import Path
import time

import numpy as np

from acoustic import RATE, PhoneEncoder
from compile_ipa import compile_head
from detect import Head

HERE = Path(__file__).resolve().parent


def main():
    folder = HERE / "target/work/development"
    rows = json.loads((folder / "manifest.json").read_text())["rows"]
    examples = []
    for i in np.random.default_rng(191).choice(len(rows), 32, replace=False):
        with np.load(folder / f"{i}.npz") as f:
            examples.append((f["pcm"], f["phones"]))
    heads = [Head(compile_head(p["ipa"]).SerializeToString())
             for p in json.loads((HERE / "recipes.json").read_text()).values()][:2]
    total = sum(len(x) / RATE for x, _ in examples)
    from portable import PortableEncoder
    for backend in ("q4", "uint8", "int8"):
        for threads in (2, 4, 6):
            try:
                encoder = PortableEncoder(folder.parent / "backbones" / (backend + ".onnx"), threads)
            except Exception as error:
                print(backend, error, flush=True)
                break
            encoder.logits(examples[0][0])
            duration, changed, phones, all_phones = 0., 0, 0, 0
            for pcm, original in examples:
                start = time.monotonic()
                logits = encoder.logits(pcm)
                duration += time.monotonic() - start
                phones += (logits.argmax(1) != original.argmax(1)).sum()
                all_phones += len(logits)
                for head in heads:
                    changed += ((head.scores(logits).max() >= head.cutoff)
                                != (head.scores(original).max() >= head.cutoff))
            print(backend, "threads", threads, "RTF", round(duration / total, 3),
                  "changed decisions", changed, "/", len(heads) * len(examples),
                  "changed frame labels", round(phones / all_phones, 3), flush=True)
            del encoder
            gc.collect()


if __name__ == "__main__":
    main()
