"""Optional speech-independent spectral subtraction on an available PCM window."""

import numpy as np


def suppress_noise(pcm, strength=2.5):
    if pcm.ndim != 1 or pcm.dtype != np.int16:
        raise ValueError("expected mono int16 PCM")
    size, hop = 400, 160
    if len(pcm) < 4 * size:
        return pcm
    wave = np.pad(pcm.astype(np.float32), (size // 2, size // 2 + hop))
    frames = np.lib.stride_tricks.sliding_window_view(wave, size)[::hop]
    window = np.hanning(size + 1)[:-1]
    spectrum = np.fft.rfft(frames * window)
    power = np.abs(spectrum) ** 2
    energy = power.sum(axis=1)
    # Estimate stationary noise from the lower-energy quarter of this window.
    noise = np.median(power[energy <= np.quantile(energy, 0.25)], axis=0)
    if noise.sum() < max(1.0, energy.max() * 1e-5):
        return pcm
    gain = np.sqrt(np.clip(1.0 - strength * noise / np.maximum(power, 1e-9), 0.04, 1.0))
    padded = np.pad(gain, ((1, 1), (0, 0)), mode="edge")
    gain = 0.25 * padded[:-2] + 0.5 * padded[1:-1] + 0.25 * padded[2:]
    restored = np.fft.irfft(spectrum * gain, n=size) * window
    output, norm = np.zeros(len(wave)), np.zeros(len(wave))
    for i, frame in enumerate(restored):
        section = slice(i * hop, i * hop + size)
        output[section] += frame
        norm[section] += window**2
    output /= np.maximum(norm, 1e-10)
    valid = output[size // 2 : size // 2 + len(pcm)]
    return np.rint(valid).clip(-32768, 32767).astype(np.int16)
