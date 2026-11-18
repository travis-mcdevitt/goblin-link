"""Construct the numerical emissions and transitions for one English IPA path."""
from dataclasses import dataclass

import numpy as np

from phones import VOCAB, ENGLISH_IDS, alternatives, plural_endings

FLOOR = -10000.


@dataclass(frozen=True)
class Settings:
    loose: bool = True
    english: bool = True
    broad: bool = True
    initial_h_cost: float = 5.5
    blank_cost: float = .025
    anchor_cost: float = 1.75
    lookahead: int = 18
    suffix_cost: float = 4.
    cutoff: float = -.95


def membership(costs):
    weights = np.zeros(len(VOCAB), np.float32)
    for label, cost in costs.items():
        weights[VOCAB[label]] = np.exp(-cost)
    return weights


def weights(phones, cfg):
    blank = membership({"<pad>": 0.})
    labels = [np.ones(len(VOCAB), dtype=np.float32)]
    path = []
    for i, phone in enumerate(phones):
        if i:
            labels.append(blank)
        path.append(len(labels))
        labels.append(membership(alternatives(phone, cfg.broad, cfg.loose)))
    count = len(labels)
    arcs = np.full((count, count), FLOOR, np.float32)
    np.fill_diagonal(arcs, 0)
    for i in range(1, count):
        arcs[i - 1, i] = 0
    for i in range(1, len(path)):
        if not np.any(labels[path[i]] * labels[path[i - 1]]):
            arcs[path[i - 1], path[i]] = 0
        arcs[path[i] - 1, path[i] - 1] = -cfg.blank_cost
    if len(phones) > 1 and phones[0] == "h":
        arcs[0, path[1]] = -cfg.initial_h_cost
    return np.stack(labels, axis=1), arcs


def relative(logits, english=True):
    competitors = logits[:, ENGLISH_IDS] if english else logits
    return np.exp(np.minimum(logits - competitors.max(axis=-1, keepdims=True), 0.))


def evidence(relative_scores, columns):
    return np.log(np.clip(relative_scores @ columns, 1e-20, 1.))


def suffix_columns(phones, cfg):
    ends = plural_endings(phones[-1])
    columns = [membership({"<pad>": 0}), membership(alternatives(phones[-1], cfg.broad, cfg.loose))]
    endings = []
    for ending in ends:
        route = []
        for phone in ending:
            route.append(len(columns))
            # Plural voicing matters for "goblins" versus "Goblin, stop".
            costs = {phone: 0.} if phone in ("s", "z") else alternatives(phone, cfg.broad, cfg.loose)
            columns.append(membership(costs))
        endings.append(route)
    return np.stack(columns, axis=1), endings


def right_context(context, route, lookahead):
    """Best relative cost of an immediate suffix anchored to EACH candidate end."""
    frames = len(context)
    future = np.pad(context, ((0, lookahead), (0, 0)), constant_values=FLOOR)
    prefix = np.zeros(frames, np.float32)
    state = np.full((2 * len(route) - 1, frames), FLOOR, np.float32)
    best = np.full(frames, FLOOR, np.float32)
    for delta in range(1, lookahead + 1):
        ahead = future[delta:delta + frames]
        old = state.copy()
        for j in range(len(state)):
            start = prefix if j == 0 else old[j - 1]
            if j >= 2 and j % 2 == 0:
                start = np.maximum(start, old[j - 2])
            col = route[j // 2] if j % 2 == 0 else 0
            state[j] = np.maximum(start, old[j]) + ahead[:, col]
        prefix += np.maximum(ahead[:, 0], ahead[:, 1])
        best = np.maximum(best, state[-1])
    return best / len(route)


def trace(logits, phones, cfg):
    gains, arcs = weights(phones, cfg)
    rel = relative(logits, cfg.english)
    emits = evidence(rel, gains)
    previous = np.full(len(arcs), FLOOR, np.float32)
    previous[0] = 0
    raw = np.empty(len(emits), np.float32)
    for i, frame in enumerate(emits):
        previous = np.max(previous[:, None] + arcs, axis=0) + frame
        raw[i] = previous[-1] / len(phones)
    return raw, emits[:, -1], rel


def timeline(logits, phones, cfg):
    raw, last, rel = trace(logits, phones, cfg)
    scores = np.where(last >= -cfg.anchor_cost, raw, FLOOR)
    if cfg.lookahead:
        columns, endings = suffix_columns(phones, cfg)
        ctx = evidence(rel, columns)
        for ending in endings:
            scores[right_context(ctx, ending, cfg.lookahead) >= -cfg.suffix_cost] = FLOOR
        scores[-cfg.lookahead:] = FLOOR
    return scores
