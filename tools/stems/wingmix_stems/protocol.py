"""JSON-lines protocol loop (stdlib only; backends are imported lazily so this runs without torch).

stdin, one request per line:
    {"id": 1, "input_wav": "/x/in.wav", "out_dir": "/x/out", "model": "htdemucs", "device": "auto"}
stdout, one message per line (anything else a library prints is routed to stderr):
    {"ready": true, "protocol": 1}                                   once at start-up
    {"id": 1, "progress": 0.25}                                      zero or more
    {"id": 1, "stems": {"vocals": "/x/out/vocals.wav", ...}, "model": "htdemucs", "version": "4.1.0",
     "device": "cpu", "elapsed_s": 61.2}                             success
    {"id": 1, "error": "message"}                                    failure (the loop keeps serving)
Stem outputs are float32 WAVs at the input sample rate. Exits on stdin EOF.
"""

import json
import sys
import time

from . import PROTOCOL


def get_backend(model, env):
    """Returns (backend_module, effective_model). WINGMIX_STEMS_BACKEND=mock selects the stdlib test backend."""
    if env.get("WINGMIX_STEMS_BACKEND") == "mock":
        from . import mock_backend

        return mock_backend
    if model.startswith("htdemucs"):
        from . import demucs_backend

        return demucs_backend
    if model == "umxhq":
        from . import umx_backend

        return umx_backend
    raise ValueError("unknown model %r (supported: htdemucs, htdemucs_ft, htdemucs_6s, umxhq)" % model)


def serve(stdin, out, env):
    def send(obj):
        out.write(json.dumps(obj, separators=(",", ":")) + "\n")
        out.flush()

    send({"ready": True, "protocol": PROTOCOL})
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        rid = None
        try:
            req = json.loads(line)
            rid = req.get("id")
            model = req.get("model", "htdemucs")
            backend = get_backend(model, env)
            t0 = time.time()

            def progress(p, rid=rid):
                send({"id": rid, "progress": round(min(max(float(p), 0.0), 1.0), 4)})

            stems, version, device = backend.separate(
                req["input_wav"], req["out_dir"], model, req.get("device", "auto"), progress
            )
            send(
                {
                    "id": rid,
                    "stems": stems,
                    "model": model,
                    "version": version,
                    "device": device,
                    "elapsed_s": round(time.time() - t0, 3),
                }
            )
        except Exception as e:  # keep serving; the client decides what to do
            send({"id": rid, "error": "%s: %s" % (type(e).__name__, e)})
    return 0


def main(argv, stdin=None, stdout=None, env=None):
    import os

    env = os.environ if env is None else env
    if "--fetch-weights" in argv:
        model = argv[argv.index("--fetch-weights") + 1]
        backend = get_backend(model, env)
        backend.fetch_weights(model)
        print("weights ready: %s" % model, file=sys.stderr)
        return 0
    # Libraries print to stdout; keep it for the protocol only.
    out = stdout or sys.stdout
    sys.stdout = sys.stderr
    return serve(stdin or sys.stdin, out, env)
