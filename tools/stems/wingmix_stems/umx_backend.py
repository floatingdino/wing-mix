"""open-unmix `umxhq` backend (weights MIT). Needs `uv sync --extra umx`. Not exercised by repository tests."""

import os

VERSION = "1.3.0"


def fetch_weights(model):
    from openunmix import utils

    utils.load_separator(model_str_or_path="umxhq", targets=None, niter=1, residual=False,
                         wiener_win_len=300, device="cpu", pretrained=True)


def separate(input_wav, out_dir, model, device, progress):
    import soundfile as sf
    import torch
    from openunmix import predict, utils

    os.makedirs(out_dir, exist_ok=True)
    data, sr = sf.read(input_wav, dtype="float32", always_2d=True)
    audio = torch.from_numpy(data.T.copy())
    if audio.shape[0] == 1:
        audio = audio.repeat(2, 1)
    dev = "mps" if device == "mps" else "cpu"  # umx is fast enough on CPU; "auto" -> cpu
    sep = utils.load_separator(model_str_or_path="umxhq", targets=None, niter=1, residual=False,
                               wiener_win_len=300, device=dev, pretrained=True)
    sep.freeze()
    progress(0.05)
    est = predict.separate(audio[None].to(dev), rate=sr, aggregate_dict=None, separator=sep, device=dev)
    paths = {}
    for name, src in est.items():
        p = os.path.join(out_dir, name + ".wav")
        sf.write(p, src[0].cpu().numpy().T, sr, subtype="FLOAT")
        paths[name] = p
    progress(1.0)
    return paths, VERSION, dev
