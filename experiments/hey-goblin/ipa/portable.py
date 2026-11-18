"""CPU ONNX export of the same phoneme backbone."""
from pathlib import Path

import numpy as np
import onnxruntime as ort

from acoustic import HOP, RECEPTIVE_FIELD, normalize
from phones import VOCAB


PORTABLE = Path("/opt/ipa/model_q4.onnx")


class PortableEncoder:
    def __init__(self, path=PORTABLE, threads=6):
        options = ort.SessionOptions()
        options.intra_op_num_threads = threads
        options.inter_op_num_threads = 1
        self.session = ort.InferenceSession(str(path), options, providers=["CPUExecutionProvider"])

    def logits(self, samples):
        wave = normalize(samples)
        inputs = {"input_values": wave[None, :]}
        if any(p.name == "attention_mask" for p in self.session.get_inputs()):
            inputs["attention_mask"] = np.ones((1, len(samples)), np.int64)
        result = self.session.run(None, inputs)[0][0]
        if result.shape != (1 + (len(samples) - RECEPTIVE_FIELD) // HOP, len(VOCAB)):
            raise ValueError("unexpected acoustic time grid or vocabulary size")
        return result
