"""Select a shared operating point using only synthetic development speakers."""
from itertools import product
import json
from pathlib import Path

import numpy as np
from design import FLOOR, Settings, evidence, right_context, suffix_columns, trace
from phones import tokenize

HERE = Path(__file__).resolve().parent
DEVELOPMENT = HERE / "target/work/development"
PHRASES = json.loads((HERE / "recipes.json").read_text())


def load_development():
    manifest = json.loads((DEVELOPMENT / "manifest.json").read_text())
    assert manifest["split"] == "development"
    rows, raw = manifest["rows"], []
    for i in range(len(rows)):
        with np.load(DEVELOPMENT / f"{i}.npz") as f:
            raw.append(f["phones"])
    return rows, raw


def sweep():
    rows, logits = load_development()
    names = list(PHRASES)[:2]
    target = np.asarray([r["target"] for r in rows])
    groups = np.asarray([r["group"] for r in rows])
    correct = target[:, None] == np.array(names)
    options = []
    for loose, english, broad, h_cost in ((False, False, True, 4.), (False, True, False, 10000.), (True, False, True, 4.), (True, True, True, 4.), (True, True, True, 2.5), (True, True, True, 5.5)):
        cfg = Settings(loose=loose, english=english, broad=broad, initial_h_cost=h_cost)
        traces = []
        for name in names:
            phones = tokenize(PHRASES[name]["ipa"])
            columns, endings = suffix_columns(phones, cfg)
            path = []
            for z in logits:
                raw, last, rel = trace(z, phones, cfg)
                ctx = evidence(rel, columns)
                suffixes = {n: np.maximum.reduce([right_context(ctx, end, n) for end in endings])
                            for n in (5, 9, 14, 18)}
                path.append((raw, last, suffixes))
            traces.append(path)
        for anchor, delay, guard in product((1., 1.75, 3.), (0, 5, 9, 14, 18), (.5, 2., 3., 4., 5.)):
            if delay == 0 and guard != .5:
                continue
            peaks = []
            for path in traces:
                values = []
                for raw, last, suffixes in path:
                    valid = last >= -anchor
                    if delay:
                        valid &= suffixes[delay] < -guard
                        valid[-delay:] = False
                    values.append(np.where(valid, raw, FLOOR).max())
                peaks.append(values)
            peak = np.array(peaks).T
            for threshold in (-.45, -.65, -.75, -.85, -.95, -1.05, -1.15, -1.25, -1.5, -1.75, -2.):
                fire = peak >= threshold
                any_fire = fire.any(axis=1)
                recall = [fire[correct[:, j], j].mean() for j in range(len(names))]
                suffix = any_fire[[r["group"] == "suffix" and r["text"] != "Hey gobbling" for r in rows]].mean()
                ing = any_fire[[r["text"] == "Hey gobbling" for r in rows]].mean()
                near = any_fire[groups == "near"].mean()
                easy = any_fire[np.isin(groups, ("unrelated", "nonspeech"))].sum()
                cross = (fire & ~correct & (target != "")[:, None]).any(axis=1).sum()
                ok = near <= .035 and suffix <= .055 and easy == 0 and cross == 0
                quality = min(recall) - near - 1.5 * suffix - .15 * ing - .025 * (easy + cross)
                settings = (loose, english, broad, h_cost, anchor, delay, guard, threshold)
                options.append((bool(ok), round(quality, 6), round(np.mean(recall), 6),
                                settings, [round(x, 3) for x in recall], round(suffix, 3), round(near, 3), int(easy), int(cross), round(ing, 3)))
        subset = [row for row in options if row[3][:4] == (loose,english,broad,h_cost)]
        print("best for", broad, h_cost, sorted(subset, reverse=True)[0], flush=True)
    for row in sorted(options, reverse=True)[:15]:
        print("choice", row)
    for delay in (0, 5, 9, 14, 18):
        print("delay", delay, max(row for row in options if row[3][5] == delay))


if __name__ == "__main__":
    sweep()
