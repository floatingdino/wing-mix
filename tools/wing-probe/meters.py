"""Subscribe to WING meters (native channel 3 request, UDP data) and print decoded values.

usage: python3 -I meters.py [--host H] [--native-port 2222] [--channels 1,2,3] [--seconds 3]

Requests ch V2 meters (AB: 11 words/strip), the monitor block (A9: 6 words) and the RTA (AA: 120
words), renews every 3 s, decodes:
  levels: int16 / 256 = dBFS; V2 words = in L/R, out L/R, gate key, gate gain, gate LED,
          dyn key, dyn gain, dyn state, automix gain; GR words = meter position*256 (model law)
  RTA:    int16 / 128 = dB (x2 when cfg/rta/rtadet is not PEAK), semitone bands E0 (20.6 Hz) .. D#10.
Findings: docs/research/wing-control.md section 'Meters and RTA'.
"""
import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wingnrp import NativeClient, meter_socket, read_meter_packet  # noqa: E402

V2 = ["inL", "inR", "outL", "outR", "gKey", "gGain", "gLED", "dKey", "dGain", "dState", "amix"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--native-port", type=int, default=2222)
    ap.add_argument("--channels", default="1,2")
    ap.add_argument("--seconds", type=float, default=3.0)
    a = ap.parse_args()
    chans = [int(x) for x in a.channels.split(",")]
    c = NativeClient(a.host, a.native_port)
    s = meter_socket()
    rid = 0x00010000 | s.getsockname()[1]
    sel = [(0xAB, [n - 1 for n in chans]), (0xA9, []), (0xAA, [])]
    c.meter_request(s.getsockname()[1], rid, sel)
    t0 = last_renew = time.time()
    pkts = 0
    words = []
    while time.time() - t0 < a.seconds:
        if time.time() - last_renew > 3.0:
            c.meter_renew(rid)
            last_renew = time.time()
        got, words, _ = read_meter_packet(s)
        if got == rid:
            pkts += 1
    print(f"{pkts} packets in {a.seconds:.1f} s, {len(words)} words "
          f"(expected {11 * len(chans) + 6 + 120})")
    for k, n in enumerate(chans):
        w = words[11 * k:11 * (k + 1)]
        print(f"ch{n}: " + " ".join(
            f"{name}={v / 256:.1f}" if name not in ("gLED", "dState") else f"{name}={v}"
            for name, v in zip(V2, w)))
    m = words[11 * len(chans):11 * len(chans) + 6]
    print("monitor: soloL/R mon1L/R mon2L/R dB =", [round(v / 256, 1) for v in m])
    rta = words[-120:]
    a4 = rta[53]
    print(f"rta: 120 bands, A4 (440 Hz, idx 53) = {a4 / 128:.1f} dB; first 6 = "
          f"{[round(v / 128, 1) for v in rta[:6]]}")


if __name__ == "__main__":
    main()
