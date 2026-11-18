"""Tokenize broad English IPA against the pinned acoustic model's phone IDs."""

import hashlib
import json
import re
from pathlib import Path
import unicodedata

VOCAB_PATH = Path(__file__).with_name("phoneme_vocab.json")
VOCAB_SHA = "d732ab2456c0c017930001dc9af0b41b3b93d25b2eb9740bf9d925508d7d87d0"
raw = VOCAB_PATH.read_bytes()
if hashlib.sha256(raw).hexdigest() != VOCAB_SHA:
    raise ValueError("phoneme vocabulary differs from the pinned acoustic model")
VOCAB = json.loads(raw)

# These are acoustic alternatives for broad transcriptions. An exact match
# costs zero. The secondary forms get a small, fixed penalty in the head.
FAMILIES = [
    ("i", "iː"),
    ("u", "uː"),
    ("ɑ", "ɑː", "ɒ"),
    ("ɔ", "ɔː"),
    ("ɚ", "ɜː", "ɜ"),
    ("eɪ", "eː"),
    ("oʊ", "əʊ", "oː"),
    ("ʌ", "ɐ"),
    ("ə", "ɐ", "ᵻ"),
    ("ɛ", "ɛː"),
    ("æ", "æː"),
    ("l", "ɫ"),
    ("tʃ",),
    ("dʒ",),
    ("p", "pʰ"),
    ("t", "tʰ"),
    ("k", "kʰ"),
    ("n̩",),
    ("l̩", "əl"),
    ("iə",),
    ("eə",),
    ("ʊə",),
]
CONSONANTS = "p b t d k ɡ f v θ ð s z ʃ ʒ h tʃ dʒ m n ŋ l ɹ j w".split()
VOWELS = "i ɪ eɪ ɛ æ ɑ ɒ ɔ oʊ ʊ u ʌ ə ɚ aɪ aʊ ɔɪ".split()
EXTRA = "ɾ ʔ x ʍ".split()
ALTERNATES = {
    "ɪ": ("ə", "ᵻ"),
    "ə": ("ɪ",),
    "ɫ": ("l",),
    "z": ("s",),
    "d": ("ɾ",),
    "t": ("ɾ",),
    "ɒ": ("ɔ",),
    "ɹ": ("r",),
}
# Dictionary notation and syllabic /m/ can be expanded into acoustic tokens.
ALIASES = {
    "g": ("ɡ",),
    "r": ("ɹ",),
    "ɝ": ("ɚ",),
    "ɝː": ("ɚ",),
    "ɚː": ("ɚ",),
    "ɪə": ("iə",),
    "ɛə": ("eə",),
    "m̩": ("ə", "m"),
    "əl": ("ə", "l"),
    "l̩": ("ə", "l"),
    "n̩": ("ə", "n"),
    "ʍ": ("h", "w"),
    "a": ("æ",),
    "e": ("ɛ",),
    "ɜɹ": ("ɚ",),
    "ɜːɹ": ("ɚ",),
    "ɑːɹ": ("ɑː", "ɹ"),
    "ɔːɹ": ("ɔː", "ɹ"),
}
SUPPORTED = set(CONSONANTS + VOWELS + EXTRA + list(ALIASES))
SUPPORTED.update(symbol for family in FAMILIES for symbol in family)
SPELLINGS = sorted(SUPPORTED, key=lambda s: (-len(s), s))


def tokenize(text):
    """Return acoustic phone labels. Whitespace can separate adjacent phones."""
    value = unicodedata.normalize("NFC", text.strip())
    if len(value) >= 2 and (value[0], value[-1]) in (("/", "/"), ("[", "]")):
        value = value[1:-1]
    for tied, plain in (("t͡ʃ", "tʃ"), ("d͡ʒ", "dʒ"), ("t͜ʃ", "tʃ"), ("d͜ʒ", "dʒ")):
        value = value.replace(tied, plain)
    result = []
    i = 0
    while i < len(value):
        if value[i].isspace() or value[i] in "ˈˌ.":
            i += 1
            continue
        match = next((phone for phone in SPELLINGS if value.startswith(phone, i)), None)
        if match is None:
            raise ValueError(f"unsupported English IPA at position {i + 1}: {value[i : i + 6]!r}")
        result.extend(ALIASES.get(match, (match,)))
        i += len(match)
        if len(result) > 48:
            raise ValueError("use at most 48 acoustic phones per pronunciation")
    if not result:
        raise ValueError("enter a pronunciation with at least one English phone")
    return tuple(result)


# Loose matches carry a higher cost than dictionary allophones. They capture
# voicing, vowel reduction and labels with a tone on the multilingual head.
NEIGHBORS = (
    "p b",
    "t d",
    "k ɡ",
    "f v",
    "θ ð",
    "s z",
    "ʃ ʒ",
    "tʃ dʒ",
    "h x ç f",
    "ɹ r ɻ",
    "l ɫ ɭ",
    "ɛ e æ",
    "ɑ ɑː a aː ɒ ʌ",
    "eɪ eː e ei",
    "oʊ əʊ o ou",
    "ɚ ɜː ɜ ø",
    "i iː ɪ",
    "ɪ ə ᵻ ʉ",
    "u ʊ ʉ",
    "ɔ ɔː ɒ",
    "aɪ ai",
    "aʊ au",
    "ɔɪ oɪ",
)


def loose_weights(phone):
    adjacent = {phone}
    for family in NEIGHBORS:
        if phone in family.split():
            adjacent.update(family.split())
    costs = {}
    for label in VOCAB:
        base = re.sub(r"[1245ʲː]+$", "", label)
        if label in adjacent:
            costs[label] = 2.1
        elif base in adjacent:
            costs[label] = 2.5
    return costs


def alternatives(phone, broad=True, loose=False):
    costs = loose_weights(phone) if loose else {}
    if broad:
        for family in FAMILIES:
            if phone in family:
                costs.update((member, 0.35) for member in family)
        costs.update((member, 1.1) for member in ALTERNATES.get(phone, ()))
    costs[phone] = 0.0
    return costs


def plural_endings(last):
    if last in ("s", "z", "ʃ", "ʒ", "tʃ", "dʒ"):
        return (("ɪ", "z"), ("ə", "z"))
    if last in ("p", "t", "k", "f", "θ", "pʰ", "tʰ", "kʰ"):
        return (("s",),)
    return (("z",),)


# English decoding constrains the multilingual competition, including silence.
ENGLISH_IDS = sorted(
    {VOCAB[s] for s in ("<pad>", "n̩", "l̩", "əl")}
    | {
        VOCAB[a]
        for symbol in SUPPORTED
        for phone in ALIASES.get(symbol, (symbol,))
        for a in alternatives(phone)
    }
)

# Some dictionary sequences have both a single acoustic label and a two-label
# realization in this checkpoint. Preserve canonical phones as the first path.
JOINED = {
    ("ə", "l"): "əl",
    ("ə", "n"): "n̩",
    ("ɑ", "ɹ"): "ɑːɹ",
    ("ɑː", "ɹ"): "ɑːɹ",
    ("ɔ", "ɹ"): "ɔːɹ",
    ("ɔː", "ɹ"): "ɔːɹ",
    ("ɛ", "ɹ"): "ɛɹ",
    ("ʊ", "ɹ"): "ʊɹ",
    ("ɪ", "ɹ"): "ɪɹ",
}


def acoustic_paths(text):
    phones = tokenize(text)
    sites = [i for i in range(len(phones) - 1) if phones[i : i + 2] in JOINED]
    if not sites:
        return [phones]
    # Keep all separate, all joined, and then the mixed segmentations. Cap the
    # extra paths for unusually long phrases with many joinable syllables.
    masks = [0, (1 << len(sites)) - 1, *range(1, min(7, (1 << len(sites)) - 1))]
    result = []
    for mask in dict.fromkeys(masks):
        joins = {pos for bit, pos in enumerate(sites) if mask & (1 << bit)}
        path, i = [], 0
        while i < len(phones):
            if i in joins:
                path.append(JOINED[phones[i : i + 2]])
                i += 2
            else:
                path.append(phones[i])
                i += 1
        result.append(tuple(path))
    return result
