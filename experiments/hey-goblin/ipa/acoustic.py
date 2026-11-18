"""The frozen phoneme recognizer. Load once and reuse its frames for all heads."""
from pathlib import Path

import numpy as np

CHECKPOINT = Path("/opt/phones")
RATE = 16_000
HOP = 320
RECEPTIVE_FIELD = 400


class PhoneEncoder:
    def __init__(self, threads=2):
        import torch
        from transformers import Wav2Vec2ForCTC

        torch.set_num_threads(threads)
        self.torch = torch
        self.model = Wav2Vec2ForCTC.from_pretrained(
            CHECKPOINT, local_files_only=True, weights_only=True,
        ).eval()

    def logits(self, samples):
        if samples.dtype != np.int16 or samples.ndim != 1 or len(samples) < RECEPTIVE_FIELD:
            raise ValueError("expected at least 400 mono 16 kHz int16 samples")
        wave = samples.astype(np.float32) / 32768
        wave = (wave - wave.mean()) / np.sqrt(wave.var() + 1e-7)
        with self.torch.inference_mode():
            scores = self.model(self.torch.from_numpy(wave[None, :])).logits
        return scores[0].numpy()
