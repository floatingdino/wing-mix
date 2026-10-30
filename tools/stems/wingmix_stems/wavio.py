"""Minimal stdlib WAV reader/writer for float32 / PCM16 / PCM24 (used by the mock backend; no numpy)."""

import array
import struct


def read_wav(path):
    """Returns (sample_rate, channels, [list of float per channel])."""
    with open(path, "rb") as f:
        data = f.read()
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError("not a RIFF/WAVE file")
    pos, fmt, pcm = 12, None, None
    while pos + 8 <= len(data):
        cid, size = data[pos : pos + 4], struct.unpack("<I", data[pos + 4 : pos + 8])[0]
        body = data[pos + 8 : pos + 8 + size]
        if cid == b"fmt ":
            fmt = struct.unpack("<HHIIHH", body[:16])
            if fmt[0] == 0xFFFE and len(body) >= 26:  # WAVE_FORMAT_EXTENSIBLE: real tag is the sub-format's
                fmt = (struct.unpack("<H", body[24:26])[0],) + fmt[1:]
        elif cid == b"data":
            pcm = body
        pos += 8 + size + (size & 1)
    if fmt is None or pcm is None:
        raise ValueError("missing fmt/data chunk")
    tag, ch, sr, _, _, bits = fmt
    if tag == 3 and bits == 32:
        a = array.array("f")
        a.frombytes(pcm[: len(pcm) // 4 * 4])
        vals = list(a)
    elif tag == 1 and bits == 16:
        a = array.array("h")
        a.frombytes(pcm[: len(pcm) // 2 * 2])
        vals = [x / 32768.0 for x in a]
    else:
        raise ValueError("unsupported WAV format tag=%s bits=%s" % (tag, bits))
    chans = [vals[c::ch] for c in range(ch)]
    return sr, ch, chans


def write_wav_f32(path, sr, chans):
    n = len(chans[0])
    inter = array.array("f", [0.0]) * (n * len(chans))
    for c, ch in enumerate(chans):
        inter[c :: len(chans)] = array.array("f", ch)
    payload = inter.tobytes()
    ch = len(chans)
    hdr = b"RIFF" + struct.pack("<I", 36 + len(payload)) + b"WAVE"
    hdr += b"fmt " + struct.pack("<IHHIIHH", 16, 3, ch, sr, sr * ch * 4, ch * 4, 32)
    hdr += b"data" + struct.pack("<I", len(payload))
    with open(path, "wb") as f:
        f.write(hdr + payload)
