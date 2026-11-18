"""Specialize the pinned 4-bit acoustic projection and append IPA phrase weights."""
import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto as T, helper, numpy_helper
import onnxruntime as ort

from acoustic import RECEPTIVE_FIELD
from compile_ipa import REVISION, compile_head, save_model
from phones import VOCAB, VOCAB_SHA
from design import Settings
from portable import PORTABLE

EXPORT_SHA = "f020386a4f730675b59f833af1581c7a0fa2f1e1c48adcf526e5c3c96115f772"


def used_labels(head):
    keep = set()
    for item in head.graph.initializer:
        if item.name.endswith("english_ids"):
            keep.update(numpy_helper.to_array(item).tolist())
        elif item.name.endswith("emission_weights"):
            cols = numpy_helper.to_array(item)
            # The background column has zero cost on every frame. Its sum
            # still reaches the clip ceiling because we keep the greedy winner.
            if not item.name.endswith("suffix_emission_weights"):
                cols = cols[:, 1:]
            keep.update(np.flatnonzero(cols.any(axis=1)).tolist())
    return keep


def restrict_head(head, ids):
    remap = {old: new for new, old in enumerate(ids)}
    for item in head.graph.initializer:
        if item.name.endswith("english_ids"):
            new = np.array([remap[i] for i in numpy_helper.to_array(item)], np.int64)
        elif item.name.endswith("emission_weights"):
            new = numpy_helper.to_array(item)[ids]
        else:
            continue
        item.CopyFrom(numpy_helper.from_array(new, item.name))
    head.graph.input[0].type.tensor_type.shape.dim[1].dim_value = len(ids)


def restrict_projection(model, ids):
    # The packed four-bit blocks and scales are independent for each output
    # phone. Keep their bytes and order together with the matching bias.
    matmul = next(n for n in model.graph.node if n.name == "/lm_head/MatMul_Q4")
    attrs = {a.name: a for a in matmul.attribute}
    k, block, n = (attrs[key].i for key in ("K", "block_size", "N"))
    if matmul.op_type != "MatMulNBits" or attrs["bits"].i != 4 or n != len(VOCAB) or k % block:
        raise ValueError("unsupported output projection")
    initializers = {i.name: i for i in model.graph.initializer}
    packed = initializers[matmul.input[1]]
    scales = initializers[matmul.input[2]]
    bias = initializers["lm_head.bias"]
    for tensor, shape in ((packed, (n, k // block, block // 2)), (scales, (n, k // block)), (bias, (n,))):
        values = numpy_helper.to_array(tensor).reshape(shape)[ids]
        if tensor.name == matmul.input[2]:
            values = values.ravel()
        tensor.CopyFrom(numpy_helper.from_array(values, tensor.name))
    attrs["N"].i = len(ids)


def wrap_pcm(graph):
    # Embed the exact 16 kHz window normalization used by the reference loader.
    def tensor(name, array):
        graph.initializer.append(numpy_helper.from_array(np.asarray(array), name))
    tensor("wake_gain", np.float32(32768))
    tensor("wake_eps", np.float32(1e-7))
    tensor("wake_axis", np.array([0], np.int64))
    make = helper.make_node
    prefix = [
        make("Cast", ["pcm"], ["wake_float"], to=T.FLOAT),
        make("Div", ["wake_float", "wake_gain"], ["wake_wave"]),
        make("ReduceMean", ["wake_wave"], ["wake_mean"], keepdims=0),
        make("Sub", ["wake_wave", "wake_mean"], ["wake_centered"]),
        make("Mul", ["wake_centered", "wake_centered"], ["wake_squared"]),
        make("ReduceMean", ["wake_squared"], ["wake_variance"], keepdims=0),
        make("Add", ["wake_variance", "wake_eps"], ["wake_variance_eps"]),
        make("Sqrt", ["wake_variance_eps"], ["wake_scale"]),
        make("Div", ["wake_centered", "wake_scale"], ["wake_normalized"]),
        make("Unsqueeze", ["wake_normalized", "wake_axis"], ["input_values"]),
    ]
    for node in reversed(prefix):
        graph.node.insert(0, node)
    del graph.input[:]
    graph.input.append(helper.make_tensor_value_info("pcm", T.INT16, ["samples"]))


def build_bank(specs, source=PORTABLE):
    if not isinstance(specs, dict) or not 1 <= len(specs) <= 32:
        raise ValueError("supply a JSON object containing 1–32 named phrases")
    heads = {}
    meta = {}
    for name, recipe in specs.items():
        if not isinstance(name, str) or not name.strip() or not isinstance(recipe, dict):
            raise ValueError("each phrase needs a name and an IPA recipe")
        variants = recipe.get("variants", [])
        if not isinstance(recipe.get("ipa"), str) or not isinstance(variants, list) or not all(isinstance(x, str) for x in variants):
            raise ValueError(f"invalid pronunciation list for {name!r}")
        reject = recipe.get("reject", [])
        if not isinstance(reject, list) or not all(isinstance(x, str) for x in reject):
            raise ValueError(f"invalid reject list for {name!r}")
        head = compile_head(recipe["ipa"], variants, cfg=Settings(cutoff=recipe.get("cutoff", Settings().cutoff)),
                            reject=reject)
        heads[name] = head
        props = {p.key: p.value for p in head.metadata_props}
        settings = json.loads(props["settings"])
        meta[name] = dict(ipa=json.loads(props["pronunciations"]), max_phones=int(props["max_phones"]),
                          lookahead=settings["lookahead"], cutoff=settings["cutoff"], reject=reject)
    ids = sorted(set().union(*(used_labels(h) for h in heads.values())))
    with open(source, "rb") as f:
        if hashlib.file_digest(f, "sha256").hexdigest() != EXPORT_SHA:
            raise ValueError("unexpected 4-bit acoustic checkpoint")
    model = onnx.load(source)
    restrict_projection(model, ids)
    graph = model.graph
    wrap_pcm(graph)
    graph.node.append(helper.make_node("Squeeze", ["logits", "wake_axis"], ["wake_frames"]))
    outputs = []
    graph.initializer.append(numpy_helper.from_array(np.array([1], np.int64), "wake_column"))
    for i, head in enumerate(heads.values()):
        restrict_head(head, ids)
        head = onnx.version_converter.convert_version(head, 14)
        prefix = f"wake_{i}_"
        part = onnx.compose.add_prefix(head, prefix)
        graph.node.append(helper.make_node("Identity", ["wake_frames"], [prefix + "logits"]))
        graph.node.extend(part.graph.node)
        graph.initializer.extend(part.graph.initializer)
        column = prefix + "column_scores"
        graph.node.append(helper.make_node("Unsqueeze", [prefix + "scores", "wake_column"], [column]))
        outputs.append(column)
    graph.node.append(helper.make_node("Concat", outputs, ["scores"], axis=1))
    del graph.output[:]
    graph.output.append(helper.make_tensor_value_info("scores", T.FLOAT, ["frames", len(outputs)]))
    # The upstream export includes inferred shapes for its original full head.
    # Inference derives fresh internal shapes from the specialized matrices.
    del graph.value_info[:]
    model.ir_version = max(model.ir_version, 8)
    helper.set_model_props(model, {"base_revision": REVISION, "base_q4_sha256": EXPORT_SHA,
                                   "vocab_sha256": VOCAB_SHA, "phone_ids": json.dumps(ids),
                                   "phrase_order": json.dumps(list(meta), ensure_ascii=False),
                                   "phrases": json.dumps(meta, ensure_ascii=False)})
    onnx.checker.check_model(model)
    return model


class BankEncoder:
    def __init__(self, path, threads=6):
        options = ort.SessionOptions()
        options.intra_op_num_threads = threads
        options.inter_op_num_threads = 1
        self.session = ort.InferenceSession(str(path), options, providers=["CPUExecutionProvider"])
        meta = self.session.get_modelmeta().custom_metadata_map
        if meta.get("base_q4_sha256") != EXPORT_SHA or meta.get("vocab_sha256") != VOCAB_SHA:
            raise ValueError("unrecognized IPA bank")
        specs = json.loads(meta["phrases"])
        order = json.loads(meta["phrase_order"])
        if len(order) != len(specs) or set(order) != set(specs):
            raise ValueError("inconsistent phrase order in bank metadata")
        self.heads = {name: Column(i, specs[name]["max_phones"], specs[name]["lookahead"], specs[name]["cutoff"])
                      for i, name in enumerate(order)}

    def logits(self, samples):
        if samples.ndim != 1 or samples.dtype != np.int16 or len(samples) < RECEPTIVE_FIELD:
            raise ValueError("expected at least 400 mono 16 kHz int16 samples")
        return self.session.run(["scores"], {"pcm": samples})[0]


@dataclass(frozen=True)
class Column:
    index: int
    max_phones: int
    lookahead: int
    cutoff: float

    def scores(self, matrix):
        return matrix[:, self.index]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("recipe", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    model = build_bank(json.loads(args.recipe.read_text()))
    save_model(model, args.output)
