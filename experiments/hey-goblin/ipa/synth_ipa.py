"""Make a 16 kHz synthetic WAV directly from English IPA using eSpeak NG."""
import argparse
from pathlib import Path
import re
import wave

import numpy as np

from acoustic import RATE
from corpus import render
from phones import tokenize

ESPEAK = {
    "p":"p", "b":"b", "t":"t", "d":"d", "k":"k", "ɡ":"g", "f":"f", "v":"v",
    "θ":"T", "ð":"D", "s":"s", "z":"z", "ʃ":"S", "ʒ":"Z", "h":"h", "tʃ":"tS",
    "dʒ":"dZ", "m":"m", "n":"n", "ŋ":"N", "l":"l", "ɫ":"L", "ɹ":"r", "j":"j",
    "w":"w", "ɾ":"t#", "ʔ":"?", "x":"x", "pʰ":"p", "tʰ":"t", "kʰ":"k",
    "i":"i:", "iː":"i:", "ɪ":"I", "eɪ":"eI", "eː":"eI", "ɛ":"E", "ɛː":"E",
    "æ":"aa", "æː":"aa", "ɑ":"A:", "ɑː":"A:", "ɒ":"0", "ɔ":"O:", "ɔː":"O:",
    "oʊ":"oU", "əʊ":"oU", "oː":"oU", "ʊ":"U", "u":"u:", "uː":"u:", "ʌ":"V",
    "ɐ":"V", "ə":"@", "ᵻ":"I", "ɚ":"3", "ɜ":"3:", "ɜː":"3:", "aɪ":"aI",
    "aʊ":"aU", "ɔɪ":"OI", "iə":"i@", "eə":"e@", "ʊə":"U@",
}
VOWELS = set("i iː ɪ eɪ eː ɛ ɛː æ æː ɑ ɑː ɒ ɔ ɔː oʊ əʊ oː ʊ u uː ʌ ɐ ə ᵻ ɚ ɜ ɜː aɪ aʊ ɔɪ iə eə ʊə".split())


def espeak_input(ipa):
    tokenize(ipa)
    ipa = ipa.strip().strip("/[]")
    words = []
    for word in ipa.split():
        out, stress = [], ""
        for part in re.split("([ˈˌ])", word):
            if part in ("ˈ", "ˌ"):
                stress = "'" if part == "ˈ" else ","
                continue
            if not part.strip("."):
                continue
            for phone in tokenize(part):
                code = ESPEAK[phone]
                if phone in VOWELS and stress:
                    code = stress + ("3:" if phone == "ɚ" else code)
                    stress = ""
                out.append(code)
        words.append("".join(out))
    return "[[" + " ".join(words) + "]]"


def synthetic(ipa, voice="en-us", seed=901, pace=1., condition="clean"):
    return render(np.random.default_rng(seed), "eSpeak", voice, espeak_input(ipa), condition, pace)[0]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ipa")
    parser.add_argument("output", type=Path)
    parser.add_argument("--voice", default="en-us")
    args = parser.parse_args()
    pcm = synthetic(args.ipa, args.voice)
    with wave.open(str(args.output), "wb") as wav:
        wav.setparams((1, 2, RATE, 0, "NONE", "not compressed"))
        wav.writeframes(pcm.astype("<i2", copy=False).tobytes())
    print(espeak_input(args.ipa), len(pcm) / RATE, "seconds")
