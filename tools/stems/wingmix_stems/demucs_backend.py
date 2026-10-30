"""Demucs v4 backend (htdemucs, htdemucs_ft, htdemucs_6s). Needs the `uv sync`-installed environment.

Not exercised by the repository's tests (they use mock_backend); see backlog/tasks/M023 for the manual check.
"""

import os

VERSION = "4.1.0"
SEGMENT_S = 7  # <= HT-Demucs' 7.8 s training segment; bounds memory (~3 GB)


def fetch_weights(model):
    from demucs.pretrained import get_model

    get_model(model)  # downloads into TORCH_HOME (set by install.sh / the Rust client)


def _devices(device):
    import torch

    if device in ("cpu", "mps", "cuda"):
        return [device]
    return ["mps", "cpu"] if torch.backends.mps.is_available() else ["cpu"]


def separate(input_wav, out_dir, model, device, progress):
    import soundfile as sf
    import torch
    from demucs.apply import apply_model
    from demucs.pretrained import get_model

    os.makedirs(out_dir, exist_ok=True)
    m = get_model(model)
    m.eval()
    data, sr = sf.read(input_wav, dtype="float32", always_2d=True)  # (n, ch)
    wav = torch.from_numpy(data.T.copy())
    if wav.shape[0] == 1:
        wav = wav.repeat(2, 1)
    if sr != m.samplerate:
        import torchaudio

        wav = torchaudio.functional.resample(wav, sr, m.samplerate)
    ref = wav.mean(0)
    wav = (wav - ref.mean()) / (ref.std() + 1e-8)
    progress(0.02)
    last_err, used = None, None
    for dev in _devices(device):
        try:
            with torch.no_grad():
                out = apply_model(m, wav[None], device=dev, shifts=1, split=True, overlap=0.25,
                                  segment=SEGMENT_S, progress=False)[0]
            used = dev
            break
        except Exception as e:  # MPS support is patchy: fall back to CPU on any device error
            last_err = e
    if used is None:
        raise last_err
    out = out * (ref.std() + 1e-8) + ref.mean()
    paths = {}
    for name, src in zip(m.sources, out):
        p = os.path.join(out_dir, name + ".wav")
        sf.write(p, src.cpu().numpy().T, m.samplerate, subtype="FLOAT")
        paths[name] = p
    progress(1.0)
    return paths, VERSION, used
