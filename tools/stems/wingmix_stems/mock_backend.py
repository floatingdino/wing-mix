"""Stdlib-only stand-in for a separation model, used by tests (WINGMIX_STEMS_BACKEND=mock).

"Separates" by band splitting with one-pole filters (no numpy, no torch, no downloads):
bass = < 200 Hz, other = 200-800 Hz, vocals = 800 Hz-4 kHz, drums = > 4 kHz. The four stems sum to the input.

Failure injection for client tests, via WINGMIX_STEMS_MOCK_MODE: `error` (error reply), `crash` (exit(3) after a
progress message), `hang` (start a child `sleep` whose pid is written to <out_dir>/child.pid, then block forever).
"""

import math
import os
import subprocess
import time

from .wavio import read_wav, write_wav_f32

VERSION = "mock-1"
CUTS_HZ = (200.0, 800.0, 4000.0)


def fetch_weights(model):
    pass


def _lowpass(x, sr, fc):
    a = 1.0 - math.exp(-2.0 * math.pi * fc / sr)
    y, s = [0.0] * len(x), 0.0
    for i, v in enumerate(x):
        s += a * (v - s)
        y[i] = s
    return y


def separate(input_wav, out_dir, model, device, progress):
    mode = os.environ.get("WINGMIX_STEMS_MOCK_MODE", "")
    os.makedirs(out_dir, exist_ok=True)
    if mode == "error":
        raise RuntimeError("mock failure")
    sr, _, chans = read_wav(input_wav)
    progress(0.0)
    if mode == "crash":
        os._exit(3)
    if mode == "hang":
        child = subprocess.Popen(["sleep", "300"])
        with open(os.path.join(out_dir, "child.pid"), "w") as f:
            f.write(str(child.pid))
        while True:
            time.sleep(1)
    names = ("bass", "other", "vocals", "drums")
    out = {n: [] for n in names}
    for ci, x in enumerate(chans):
        l1, l2, l3 = (_lowpass(x, sr, fc) for fc in CUTS_HZ)
        out["bass"].append(l1)
        out["other"].append([b - a for a, b in zip(l1, l2)])
        out["vocals"].append([b - a for a, b in zip(l2, l3)])
        out["drums"].append([v - b for v, b in zip(x, l3)])
        progress((ci + 1) / len(chans) * 0.9)
    paths = {}
    for n in names:
        p = os.path.join(out_dir, n + ".wav")
        write_wav_f32(p, sr, out[n])
        paths[n] = p
    progress(1.0)
    return paths, VERSION, "cpu"
