"""Compile an English IPA pronunciation into a numerical ONNX wake-phrase head."""
import argparse
from dataclasses import asdict
import json
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

from design import FLOOR, Settings, suffix_columns, weights
from phones import ENGLISH_IDS, VOCAB, VOCAB_SHA, tokenize

REVISION = "ae45363bf3413b374fecd9dc8bc1df0e24c3b7f4"


def info(name, shape):
    return helper.make_tensor_value_info(name, TensorProto.FLOAT, shape)


class Graph:
    def __init__(self, prefix="node"):
        self.prefix = prefix
        self.nodes = []
        self.constants = []

    def constant(self, name, data):
        self.constants.append(numpy_helper.from_array(np.asarray(data), name))
        return name

    def op(self, kind, *inputs, name=None, **attrs):
        name = name or f"{self.prefix}_{len(self.nodes)}"
        self.nodes.append(helper.make_node(kind, inputs, [name], **attrs))
        return name


def scan_body(arcs):
    g = Graph("step")
    transition = g.constant("transition_weights", arcs)
    column = g.op("Unsqueeze", "previous", axes=[1])
    incoming = g.op("ReduceMax", g.op("Add", column, transition), axes=[0], keepdims=0)
    state = g.op("Add", incoming, "frame", name="next")
    end = g.constant("terminal_state", np.int64(len(arcs) - 1))
    g.op("Gather", state, end, axis=0, name="completed")
    return helper.make_graph(g.nodes, "ctc_step",
                             [info("previous", [len(arcs)]), info("frame", [len(arcs)])],
                             [info("next", [len(arcs)]), info("completed", [])], g.constants)


def suffix_evidence(g, ahead, route, zero_vector, low_vector):
    prefix = zero_vector
    states = [low_vector] * (2 * len(route) - 1)
    peak = low_vector
    for columns in ahead:
        previous = states.copy()
        for j, old in enumerate(previous):
            starts = [old, previous[j - 1] if j else prefix]
            if j >= 2 and j % 2 == 0:
                starts.append(previous[j - 2])
            col = 0 if j % 2 else route[j // 2]
            states[j] = g.op("Add", g.op("Max", *starts), columns[col])
        prefix = g.op("Add", prefix, g.op("Max", columns[0], columns[1]))
        peak = g.op("Max", peak, states[-1])
    return peak


def single_path(ipa, cfg):
    phones = tokenize(ipa)
    emissions, arcs = weights(phones, cfg)
    g = Graph()
    zero = g.constant("zero", np.float32(0))
    one = g.constant("one", np.float32(1))
    floor = g.constant("floor", np.float32(FLOOR))
    tiny = g.constant("tiny", np.float32(1e-20))
    ids = g.constant("english_ids", np.array(ENGLISH_IDS, dtype=np.int64)) if cfg.english else None
    competition = g.op("Gather", "logits", ids, axis=1) if ids else "logits"
    greedy = g.op("ReduceMax", competition, axes=[1], keepdims=1)
    rel = g.op("Exp", g.op("Min", g.op("Sub", "logits", greedy), zero))

    def evidence(matrix, name):
        mixed = g.op("MatMul", rel, g.constant(name, matrix))
        return g.op("Log", g.op("Clip", mixed, tiny, one))

    frames = evidence(emissions, "emission_weights")
    initial = np.full(len(arcs), FLOOR, np.float32)
    initial[0] = 0
    g.nodes.append(helper.make_node("Scan", [g.constant("initial", initial), frames],
                                    ["last_state", "completed_scores"], body=scan_body(arcs),
                                    num_scan_inputs=1))
    score = g.op("Div", "completed_scores", g.constant("phone_count", np.float32(len(phones))))
    last = g.op("Gather", frames, g.constant("last_column", np.int64(len(arcs) - 1)), axis=1)
    valid = g.op("Not", g.op("Less", last, g.constant("anchor", np.float32(-cfg.anchor_cost))))
    if cfg.lookahead:
        matrix, routes = suffix_columns(phones, cfg)
        ctx = evidence(matrix, "suffix_emission_weights")
        idx0 = g.constant("index_zero", np.int64(0))
        idx1 = g.constant("index_one", np.int64(1))
        count = g.op("Gather", g.op("Shape", last), idx0, axis=0)
        idx = g.op("Range", idx0, count, idx1)
        valid = g.op("And", valid, g.op("Less", idx, g.op("Sub", count, g.constant("lag", np.int64(cfg.lookahead)))))
        padding = g.constant("suffix_padding", np.full((cfg.lookahead, matrix.shape[1]), FLOOR, np.float32))
        padded = g.op("Concat", ctx, padding, axis=0)
        columns = [g.constant(f"column_{i}", np.int64(i)) for i in range(matrix.shape[1])]
        ahead = []
        for delta in range(1, cfg.lookahead + 1):
            shifted = g.op("Gather", padded, g.op("Add", idx, g.constant(f"offset_{delta}", np.int64(delta))), axis=0)
            ahead.append([g.op("Gather", shifted, col, axis=1) for col in columns])
        zeros = g.op("Mul", last, zero)
        lows = g.op("Add", zeros, floor)
        for i, route in enumerate(routes):
            peak = suffix_evidence(g, ahead, route, zeros, lows)
            minimum = g.constant(f"suffix_threshold_{i}", np.float32(-cfg.suffix_cost * len(route)))
            valid = g.op("And", valid, g.op("Less", peak, minimum))
    g.op("Where", valid, score, floor, name="scores")
    graph = helper.make_graph(g.nodes, "ipa_wake_phrase", [info("logits", ["frames", len(VOCAB)])],
                              [info("scores", ["frames"])], g.constants)
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 11)], ir_version=8)
    helper.set_model_props(model, {"ipa": ipa, "phones": " ".join(phones), "revision": REVISION,
                                   "vocab_sha256": VOCAB_SHA, "settings": json.dumps(asdict(cfg), sort_keys=True)})
    onnx.checker.check_model(model)
    return model


def compile_head(ipa, variants=(), cfg=None):
    cfg = cfg or Settings()
    paths = [ipa, *variants]
    if len(paths) > 8:
        raise ValueError("use at most eight pronunciations per phrase")
    model = single_path(ipa, cfg)
    if len(paths) > 1:
        g = Graph()
        outputs = []
        for i, text in enumerate(paths):
            prefix = f"path{i}_"
            part = onnx.compose.add_prefix(single_path(text, cfg), prefix)
            g.op("Identity", "logits", name=prefix + "logits")
            g.nodes.extend(part.graph.node)
            g.constants.extend(part.graph.initializer)
            outputs.append(prefix + "scores")
        g.op("Max", *outputs, name="scores")
        merged = helper.make_graph(g.nodes, "pronunciations", list(model.graph.input),
                                   list(model.graph.output), g.constants)
        model.graph.CopyFrom(merged)
    props = {p.key: p.value for p in model.metadata_props}
    props["pronunciations"] = json.dumps(paths, ensure_ascii=False)
    props["max_phones"] = str(max(len(tokenize(text)) for text in paths))
    helper.set_model_props(model, props)
    onnx.checker.check_model(model)
    return model


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ipa")
    parser.add_argument("output", type=Path)
    parser.add_argument("--variant", action="append", default=[], help="another IPA pronunciation of this phrase")
    args = parser.parse_args()
    onnx.save(compile_head(args.ipa, args.variant), args.output)
