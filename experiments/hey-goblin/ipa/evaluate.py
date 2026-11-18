"""Measure the frozen IPA and spelling heads on exactly the same held-out PCM."""
from collections import defaultdict
import json
from pathlib import Path

import numpy as np
import onnxruntime as ort

from compile_ipa import compile_head
from detect import Head
from fixtures import TARGETS
from letters import CUTOFF, compile_head as compile_letters, session
from prepare import fixture_version

HERE = Path(__file__).resolve().parent


def read_manifest(folder, split):
    manifest = json.loads((folder / "manifest.json").read_text())
    if manifest["split"] != split or manifest["version"] != fixture_version():
        raise ValueError(f"regenerate {split} with the current fixture sources")
    return manifest["rows"]


def make_heads():
    specs = json.loads((HERE / "recipes.json").read_text())
    compiled, spelling = {}, {}
    for name in TARGETS:
        stem = name.lower().replace(" ", "_")
        path = HERE / "models" / f"{stem}.onnx"
        assert path.read_bytes() == compile_head(specs[name]["ipa"]).SerializeToString(), name
        compiled[name] = Head(str(path))
        previous = list((HERE.parent / "models").glob(stem + "_*.onnx"))
        if previous:
            spelling[name] = session(previous[0])
        else:
            options = ort.SessionOptions()
            options.intra_op_num_threads = 1
            spelling[name] = ort.InferenceSession(compile_letters(name).SerializeToString(), options,
                                                  providers=["CPUExecutionProvider"])
    return compiled, spelling


def guarded_heads():
    specs = json.loads((HERE / "guarded.json").read_text())
    return {name: Head(compile_head(s["ipa"], reject=s.get("reject", [])).SerializeToString())
            for name, s in specs.items()}


def report():
    folder = HERE / "target/work/holdout"
    rows = read_manifest(folder, "holdout")
    ipa, spelling = make_heads()
    guards = guarded_heads()
    guard_stats = defaultdict(lambda: np.zeros(3, dtype=int))
    stats = defaultdict(lambda: np.zeros(8, dtype=int))

    def count(group, first, ref, spell):
        stats[group] += [1, first, ref, spell, first and not spell, spell and not first,
                         first and not ref, ref and not first]

    for i, r in enumerate(rows):
        with np.load(folder / f"{i}.npz") as cache:
            ph, compact, chars = cache["phones"], cache["portable"], cache["letters"]
        a = {n for n, h in ipa.items() if h.scores(compact).max() >= h.cutoff}
        ref = {n for n, h in ipa.items() if h.scores(ph).max() >= h.cutoff}
        b = {n for n, h in spelling.items()
             if h.run(None, {"logits": chars})[0].item() >= CUTOFF}
        guarded = {n for n, h in guards.items() if h.scores(compact).max() >= h.cutoff}
        name, group = r["target"], r["group"]
        if name:
            guard_stats[name, "positive"] += [1, name in a, name in guarded]
            guard_stats["wrong head", "any"] += [1, bool(a - {name}), bool(guarded - {name})]
        else:
            guard_stats["all", group] += [1, bool(a), bool(guarded)]
            guard_stats[group, r["text"]] += [1, bool(a), bool(guarded)]
        if name:
            hit_a, hit_ref, hit_b = name in a, name in ref, name in b
            count((name, group), hit_a, hit_ref, hit_b)
            count((name, "all"), hit_a, hit_ref, hit_b)
            count(("positive", r["voice"].split("/")[0]), hit_a, hit_ref, hit_b)
            count(("positive", r["condition"]), hit_a, hit_ref, hit_b)
            count(("wrong head on target", "any"), bool(a - {name}), bool(ref - {name}), bool(b - {name}))
        else:
            count(("controls", group), bool(a), bool(ref), bool(b))
            count((group, r["text"]), bool(a), bool(ref), bool(b))
            for detector in TARGETS:
                count((detector, "control " + group), detector in a, detector in ref, detector in b)
    print("group | subdivision | clips | Q4 | reference | spelling | Q4 only vs spelling | spelling only vs Q4 | Q4 only vs reference | reference only vs Q4")
    for k, v in sorted(stats.items()):
        print(*k, *v, sep=" | ")
    print("explicit rivals | clips | default Q4 | guarded Q4")
    for k, v in sorted(guard_stats.items()):
        print(*k, *v, sep=" | ")
    assert all(stats[(n, "all")][0] == 119 for n in TARGETS)
    assert all(stats[(n, "all")][1] > 0 for n in TARGETS)
    assert stats[("controls", "nonspeech")][1] == 0
    assert stats[("wrong head on target", "any")][0] == 357


if __name__ == "__main__":
    report()
