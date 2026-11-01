#!/usr/bin/env python3
"""Reference log-mel features for crates/core/src/stt/mel.rs (dot-4uzgol.69).

Emulates the graph of FluidInference/parakeet-tdt-0.6b-v2-coreml Preprocessor.mlmodelc (model.mil) with
that model's own weights: the int8 DFT kernels (window folded in) and mel filterbank are dequantized from
Preprocessor.mlmodelc/weights/weight.bin and applied in float64. The Rust front end computes the same
features from first principles (Hann window, FFT, Slaney mel), so the fixture checks it against the export.

    curl -sLO https://huggingface.co/FluidInference/parakeet-tdt-0.6b-v2-coreml/resolve/ee09c569f73759e6d44c9bd16766f477b2b36d39/Preprocessor.mlmodelc/weights/weight.bin
    python3 ci/parakeet-mel-fixture.py weight.bin > crates/core/data/fixtures/parakeet-mel.json

Pure Python (no numpy); takes a few seconds.
"""
import json
import math
import struct
import sys

# Blob offsets in weight.bin, from Preprocessor.mlmodelc/model.mil (data, scale, zero point).
DFT_RE = (64, 132096, 131712, 257, 512)
DFT_IM = (132736, 264768, 264384, 257, 512)
MEL_FB = (265408, 298560, 298368, 128, 257)

# The test signal: SAMPLES of speech-band tones and noise, padded with zeros to PADDED (as the app pads a
# piece to the encoder window).
SAMPLES = 8000
PADDED = 16000
HOP = 160
PAD = 256


def blob(b, off):
    sentinel, _dtype, size, data = struct.unpack_from("<IIQQ", b, off)
    assert sentinel == 0xDEADBEEF, hex(sentinel)
    return b[data:data + size]


def dequantize(b, spec):
    qo, so, zo, rows, cols = spec
    q = struct.unpack(f"<{rows * cols}b", blob(b, qo))
    s = struct.unpack(f"<{rows}e", blob(b, so))
    z = struct.unpack(f"<{rows}b", blob(b, zo))
    return [[(q[r * cols + c] - z[r]) * s[r] for c in range(cols)] for r in range(rows)]


def signal():
    seed, out = 12345, []
    for t in range(SAMPLES):
        seed = (seed * 1103515245 + 12345) % 2**31
        noise = seed / 2**31 - 0.5
        x = (0.3 * math.sin(2 * math.pi * 220 * t / 16000) + 0.2 * math.sin(2 * math.pi * 1300 * t / 16000)
             + 0.1 * math.sin(2 * math.pi * 4100 * t / 16000) + 0.3 * noise)
        out.append(x * (0.5 + 0.5 * math.sin(2 * math.pi * 3 * t / 16000)))
    return out


def main():
    b = open(sys.argv[1], "rb").read()
    re, im, fb = dequantize(b, DFT_RE), dequantize(b, DFT_IM), dequantize(b, MEL_FB)
    pcm = signal()
    x = pcm + [0.0] * (PADDED - SAMPLES)
    y = [x[0]] + [x[t] - 0.97 * x[t - 1] for t in range(1, len(x))]
    n = len(y)
    p = [y[PAD - i] for i in range(PAD)] + y + [y[n - 2 - i] for i in range(PAD)]
    frames = n // HOP + 1
    valid = SAMPLES // HOP + 1
    logmel = []
    for f in range(frames):
        seg = p[f * HOP:f * HOP + 512]
        power = []
        for k in range(257):
            r = sum(a * w for a, w in zip(seg, re[k]))
            i = sum(a * w for a, w in zip(seg, im[k]))
            power.append(r * r + i * i)
        logmel.append([math.log(sum(a * w for a, w in zip(power, fb[m])) + 2**-24) for m in range(128)])
    mel = []
    for m in range(128):
        col = [logmel[f][m] for f in range(valid)]
        mean = sum(col) / valid
        std = math.sqrt(sum((c - mean) ** 2 for c in col) / (valid - 1)) + 1e-5
        mel.append([round((c - mean) / std, 4) for c in col])
    json.dump({"_doc": __doc__.strip().splitlines()[0], "samples": SAMPLES, "frames": frames, "valid": valid,
               "pcm_seed": 12345, "mel": mel}, sys.stdout, separators=(",", ":"))


main()
