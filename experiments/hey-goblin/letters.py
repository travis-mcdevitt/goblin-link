"""Compile a spelling into fixed CTC transition and emission weights."""
import argparse
import json
import re
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto as T, helper as h, numpy_helper

VOCAB = Path("/opt/ctc/vocab.json")
BASE = Path("/opt/ctc/acoustic.onnx")
CUTOFF = -1.5  # mean logit regret per non-space letter, selected on calibration voices


def tensor(name, array):
    return numpy_helper.from_array(np.asarray(array), name)


def info(name, shape):
    return h.make_tensor_value_info(name, T.FLOAT, shape)


def spelling_path(phrase):
    text = " ".join(phrase.upper().split())
    if not re.fullmatch("[A-Z']+( [A-Z']+)*", text) or len(text) > 80:
        raise ValueError("use 1–80 English letters, spaces and pronunciation apostrophes")
    vocab = json.loads(VOCAB.read_text())
    blank = vocab["<pad>"]
    tokens = [vocab[c] for c in "|" + text.replace(" ", "|") + "|"]
    # State zero can start a match at any word boundary. Other states spell
    # the phrase in order, with CTC blanks between adjacent labels.
    labels = [tokens[0]]
    for token in tokens[1:]:
        labels.extend([blank, token])
    size = len(labels) + 1
    transition = np.full((size, size), -10000, np.float32)
    np.fill_diagonal(transition, 0)
    emission = np.zeros((len(vocab), size), np.float32)
    for state, label in enumerate(labels, 1):
        emission[label, state] = 1
        transition[state - 1, state] = 0
        if state > 2 and label != blank and label != labels[state - 3]:
            transition[state - 2, state] = 0
    start = np.full(size, -10000, np.float32)
    start[0] = 0
    boundary = np.full((1, len(vocab)), -10000, np.float32)
    boundary[0, vocab["|"]] = 0
    node = h.make_node
    body = h.make_graph([
        node("Unsqueeze", ["previous"], ["column"], axes=[1]),
        node("Add", ["column", "transition"], ["paths"]),
        node("ReduceMax", ["paths"], ["best"], axes=[0], keepdims=0),
        node("MatMul", ["frame", "emission"], ["acoustic"]),
        node("Add", ["best", "acoustic"], ["next"]),
        node("Gather", ["next", "final_index"], ["completed"], axis=0),
    ], "CTC_step", [info("previous", [size]), info("frame", [len(vocab)])],
       [info("next", [size]), info("completed", [])],
       [tensor("transition", transition), tensor("emission", emission),
        tensor("final_index", np.int64(size - 1))])
    graph = h.make_graph([
        node("ReduceMax", ["logits"], ["greedy"], axes=[1], keepdims=1),
        node("Sub", ["logits", "greedy"], ["relative"]),
        node("Concat", ["boundary", "relative", "boundary"], ["frames"], axis=0),
        node("Scan", ["initial", "frames"], ["last", "scores"], body=body, num_scan_inputs=1),
        node("ReduceMax", ["scores"], ["peak"], keepdims=0),
        node("Div", ["peak", "length"], ["score"]),
    ], text, [info("logits", ["frames", len(vocab)])], [info("score", [])],
       [tensor("initial", start), tensor("boundary", boundary),
        tensor("length", np.float32(len(text.replace(" ", ""))))])
    model = h.make_model(graph, opset_imports=[h.make_opsetid("", 11)], ir_version=8)
    onnx.checker.check_model(model)
    return model


def compile_head(phrase, aliases=()):
    """Accept the best complete spelling path for a single spoken phrase."""
    if not aliases:
        return spelling_path(phrase)
    nodes, constants, scores = [], [], []
    for i, spelling in enumerate([phrase, *aliases]):
        prefix = f"path{i}_"
        part = onnx.compose.add_prefix(spelling_path(spelling), prefix)
        nodes.append(h.make_node("Identity", ["logits"], [prefix + "logits"]))
        nodes.extend(part.graph.node)
        constants.extend(part.graph.initializer)
        scores.append(prefix + "score")
    nodes.append(h.make_node("Max", scores, ["score"]))
    graph = h.make_graph(nodes, phrase, [info("logits", ["frames", 32])],
                         [info("score", [])], constants)
    model = h.make_model(graph, opset_imports=[h.make_opsetid("", 11)], ir_version=8)
    onnx.checker.check_model(model)
    return model


def session(path):
    options = ort.SessionOptions()
    options.intra_op_num_threads = 2
    options.inter_op_num_threads = 1
    return ort.InferenceSession(str(path), options, providers=["CPUExecutionProvider"])


def acoustic_logits(acoustic, pcm):
    if pcm.ndim != 1 or pcm.dtype != np.int16 or len(pcm) < 400:
        raise ValueError("expected at least 25 ms of mono 16 kHz int16 samples")
    x = pcm.astype(np.float32) / 32768
    x = (x - x.mean()) / np.sqrt(x.var() + 1e-7)
    inputs = {"input_values": x[None, :]}
    if any(i.name == "attention_mask" for i in acoustic.get_inputs()):
        inputs["attention_mask"] = np.ones((1, len(x)), np.int64)
    return acoustic.run(None, inputs)[0][0]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phrase")
    parser.add_argument("output", type=Path)
    parser.add_argument("--alias", action="append", default=[], help="another spelling of the same sounds")
    args = parser.parse_args()
    onnx.save(compile_head(args.phrase, args.alias), args.output)
