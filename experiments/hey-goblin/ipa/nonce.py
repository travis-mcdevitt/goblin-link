"""Test IPA-only pseudowords without a spelling or pronunciation dictionary."""

import argparse
from collections import Counter

from compile_ipa import compile_head
from detect import Head, checked_encoder
from phones import SUPPORTED, tokenize
from synth_ipa import ESPEAK, espeak_input, synthetic

CASES = {
    "/ˈmɪpə zuːv/": ("/ˈnɪpə zuːv/", "/ˈmɪpə suːv/", "/zuːv ˈmɪpə/"),
    "/ˈflɑnɪk weɪb/": ("/ˈflɑmɪk weɪb/", "/ˈflɑnɪk reɪb/", "/weɪb ˈflɑnɪk/"),
    "/ˈθɛmbə dʒɔɪp/": ("/ˈfɛmbə dʒɔɪp/", "/ˈθɛmbə tʃɔɪp/", "/dʒɔɪp ˈθɛmbə/"),
}


def main(neural):
    assert all(phone in ESPEAK for symbol in SUPPORTED for phone in tokenize(symbol))
    assert espeak_input("/heɪ ˈɡɑblɪn/") == "[[heI g'A:blIn]]"
    heads = {n: Head(compile_head(n).SerializeToString()) for n in CASES}
    encoder = checked_encoder()
    voices = ["en-us", "en-us+f2", "en-us+m4", "en-gb+f4", "en-gb+m1"]
    if neural:
        voices = list(range(810, 818))
    stats = Counter()
    for iv, voice in enumerate(voices):
        for i, (ipa, controls) in enumerate(CASES.items()):
            for j, text in enumerate([ipa, ipa, ipa, *controls]):
                pcm = synthetic(
                    text,
                    voice if not neural else "en-us",
                    piper_speaker=voice if neural else None,
                    seed=8031 + iv * 100 + i * 10 + j,
                    pace=(0.9, 1.0, 1.1)[j % 3],
                    condition=("clean", "quiet", "noise")[j % 3],
                )
                frames = encoder.logits(pcm)
                fires = {n for n, h in heads.items() if h.scores(frames).max() >= h.cutoff}
                if j < 3:
                    stats[ipa, "hit"] += ipa in fires
                    stats[ipa, "cross"] += bool(fires - {ipa})
                else:
                    stats[ipa, "false"] += bool(fires)
    total = len(voices) * 3
    for ipa in CASES:
        print(
            ipa,
            "positive",
            stats[ipa, "hit"],
            "/",
            total,
            "controls",
            stats[ipa, "false"],
            "/",
            total,
            "wrong head",
            stats[ipa, "cross"],
            "/",
            total,
            flush=True,
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--neural", action="store_true")
    main(parser.parse_args().neural)
