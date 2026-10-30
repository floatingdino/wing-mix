"""Write-then-read-back round trips per parameter class against a WING (or wing-sim).

usage: python3 -I roundtrip.py [--host 127.0.0.1] [--osc-port 2223] [--native-port 2222]

For every case: (1) OSC set by *name* -> OSC get + native get (by positional hash);
(2) native set -> OSC get. Also checks that a second native client receives the change
event. Prints one line per case and a summary; exit code 1 on any mismatch.
WARNING: writes to the console. Run it against a disposable wing-sim, never a live show.
Findings: docs/research/wing-control.md.
"""
import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wingosc import OscClient  # noqa: E402
from wingnrp import NativeClient, wing_hash  # noqa: E402

# (class, OSC path by name, positional path (hash), value A (OSC), value B (native))
# Floats are engineering units (dB, Hz, ms, %); enums are tokens; ints are 0/1 or indices.
CASES = [
    # setup so model-specific names resolve: GATE / STD / COMP / HALL
    ("setup", "/ch/1/gate/mdl", "ch/1/gate/mdl", "GATE", "GATE"),
    ("setup", "/ch/1/eq/mdl", "ch/1/eq/mdl", "STD", "STD"),
    ("setup", "/ch/1/dyn/mdl", "ch/1/dyn/mdl", "COMP", "COMP"),
    ("setup", "/fx/1/mdl", "fx/1/mdl", "HALL", "HALL"),
    # input stage
    ("trim", "/ch/1/in/set/trim", "ch/1/in/set/trim", 2.5, -3.0),
    ("trim", "/ch/1/in/set/inv", "ch/1/in/set/inv", 1, 0),
    ("preamp", "/io/in/LCL/1/g", "io/in/LCL/1/g", 12.5, 0.0),
    # filter
    ("filter", "/ch/1/flt/lc", "ch/1/flt/lc", 1, 1),
    ("filter", "/ch/1/flt/lcf", "ch/1/flt/lcf", 80.0, 120.0),
    ("filter", "/ch/1/flt/lcs", "ch/1/flt/lcs", "18", "24"),
    ("filter", "/ch/1/flt/hcf", "ch/1/flt/hcf", 12000.0, 9000.0),
    ("filter", "/ch/1/flt/tf", "ch/1/flt/tf", 1, 0),
    # gate (unified dynamics block, model GATE)
    ("gate", "/ch/1/gate/on", "ch/1/gate/on", 1, 1),
    ("gate", "/ch/1/gate/thr", "ch/1/gate/1", -35.0, -50.0),
    ("gate", "/ch/1/gate/range", "ch/1/gate/2", 30.0, 20.0),
    ("gate", "/ch/1/gate/rel", "ch/1/gate/5", 150.0, 300.0),
    # EQ (model STD)
    ("eq", "/ch/1/eq/on", "ch/1/eq/on", 1, 1),
    ("eq", "/ch/1/eq/2g", "ch/1/eq/8", -4.0, 3.0),
    ("eq", "/ch/1/eq/2f", "ch/1/eq/9", 350.0, 2500.0),
    ("eq", "/ch/1/eq/2q", "ch/1/eq/10", 2.0, 0.7),
    ("eq", "/ch/1/eq/leq", "ch/1/eq/4", "PEQ", "SHV"),
    # dynamics (model COMP)
    ("dyn", "/ch/1/dyn/on", "ch/1/dyn/on", 1, 1),
    ("dyn", "/ch/1/dyn/thr", "ch/1/dyn/1", -20.0, -15.0),
    ("dyn", "/ch/1/dyn/ratio", "ch/1/dyn/2", "4.0", "2.0"),
    ("dyn", "/ch/1/dyn/att", "ch/1/dyn/5", 10.0, 25.0),
    ("dyn", "/ch/1/dyn/rel", "ch/1/dyn/7", 100.0, 250.0),
    ("dyn", "/ch/1/dyn/gain", "ch/1/dyn/gain", 3.0, 2.0),
    # inserts
    ("insert", "/ch/1/preins/ins", "ch/1/preins/ins", "FX1", "FX1"),
    ("insert", "/ch/1/preins/on", "ch/1/preins/on", 1, 1),
    ("insert", "/ch/1/postins/ins", "ch/1/postins/ins", "FX9", "NONE"),
    # FX slot (model HALL)
    ("fx", "/fx/1/fxmix", "fx/1/fxmix", 80.0, 100.0),
    ("fx", "/fx/1/dcy", "fx/1/3", 2.0, 1.5),
    ("fx", "/fx/1/pdel", "fx/1/1", 20, 30),
    # sends (bus 1..16) and main sends
    ("send", "/ch/1/send/1/on", "ch/1/send/1/on", 1, 1),
    ("send", "/ch/1/send/1/lvl", "ch/1/send/1/lvl", -6.0, -10.0),
    ("send", "/ch/1/send/1/mode", "ch/1/send/1/mode", "POST", "PRE"),
    ("send", "/ch/1/send/1/pan", "ch/1/send/1/pan", -30.0, 0.0),
    ("send", "/ch/1/main/1/lvl", "ch/1/main/1/lvl", -2.0, 0.0),
    ("send", "/ch/1/main/1/on", "ch/1/main/1/on", 0, 1),
    # pan / fader / mute
    ("fader", "/ch/1/fdr", "ch/1/fdr", -5.0, 0.0),
    ("pan", "/ch/1/pan", "ch/1/pan", 25.0, -40.0),
    ("pan", "/ch/1/wid", "ch/1/wid", 80.0, 100.0),
    ("mute", "/ch/1/mute", "ch/1/mute", 1, 0),
    # bus / main / matrix processing
    ("bus", "/bus/1/fdr", "bus/1/fdr", -4.0, 0.0),
    ("bus", "/bus/1/eq/on", "bus/1/eq/on", 1, 1),
    ("bus", "/bus/1/dyn/on", "bus/1/dyn/on", 1, 0),
    ("main", "/main/1/fdr", "main/1/fdr", -1.0, 0.0),
    ("main", "/main/1/dyn/on", "main/1/dyn/on", 1, 0),
    ("mtx", "/mtx/1/fdr", "mtx/1/fdr", -6.0, 0.0),
    ("dca", "/dca/1/fdr", "dca/1/fdr", -3.0, 0.0),
    # solo + monitor
    ("solo", "/cfg/solo/chtap", "cfg/solo/chtap", "AFL", "PFL"),
    ("solo", "/ch/1/$solo", "ch/1/$solo", 1, 0),
    ("monitor", "/cfg/mon/1/src", "cfg/mon/1/src", "BUS.1", "MAIN.1"),
    ("rta", "/cfg/rta/rtasrc", "cfg/rta/rtasrc", 1, 2),
    ("rta", "/cfg/rta/rtatap", "cfg/rta/rtatap", "POSTEQ", "PREEQ"),
    # topology
    ("topology", "/ch/1/name", "ch/1/name", "Kick In", "Kick"),
    ("topology", "/ch/1/col", "ch/1/col", 3, 5),
    ("topology", "/ch/1/tags", "ch/1/tags", "#D1", "#D1,#M2"),
    ("topology", "/bus/1/name", "bus/1/name", "IEM Vox", "Drum Bus"),
    ("topology", "/io/out/LCL/1/grp", "io/out/LCL/1/grp", "BUS", "MAIN"),
    ("topology", "/io/out/LCL/1/in", "io/out/LCL/1/in", 3, 1),
]


def osc_value(reply):
    """Engineering value of a get reply (`,sff` / `,sfi` -> 3rd arg; `,s` -> string)."""
    if not reply:
        return None
    args = reply[1]
    if len(args) == 3:
        return args[2]
    return args[0] if args else None


def same(a, b):
    if isinstance(a, float) or isinstance(b, float):
        try:
            a, b = float(a), float(b)
        except (TypeError, ValueError):
            return False
        return abs(a - b) <= max(0.02 * abs(b), 0.06)
    return str(a) == str(b)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--osc-port", type=int, default=2223)
    ap.add_argument("--native-port", type=int, default=2222)
    a = ap.parse_args()
    osc = OscClient(a.host, a.osc_port)
    nat = NativeClient(a.host, a.native_port)
    watcher = NativeClient(a.host, a.native_port)
    fails = 0
    print(f"{'class':9} {'path':24} {'osc set':>10} {'osc get':>10} {'nat get':>10} "
          f"{'nat set':>10} {'osc get':>10}  hash      event")
    for cls, path, pos, va, vb in CASES:
        err = osc.set(path, va)
        time.sleep(0.02)
        og1 = osc_value(osc.get(path))
        ng1 = nat.get(pos)
        ev = watcher.collect_events(0.05)
        h = wing_hash(pos)
        got_ev = any(e[0] == h for e in ev)
        nat.set(pos, vb)
        time.sleep(0.02)
        og2 = osc_value(osc.get(path))
        ok = (err is None and same(og1, va) and same(ng1, va) and same(og2, vb))
        if cls == "setup":
            ok = ok or same(og2, vb)
        fails += not ok
        print(f"{cls:9} {path:24} {str(va):>10} {str(og1):>10.10} {str(ng1):>10.10} "
              f"{str(vb):>10} {str(og2):>10.10}  {h:08x}  {'yes' if got_ev else 'no '}"
              f"  {'OK' if ok else 'MISMATCH'}{'' if err is None else ' err=' + str(err)}")
    print(f"\n{len(CASES) - fails}/{len(CASES)} round trips OK")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
