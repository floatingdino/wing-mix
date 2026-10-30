"""Recursively dump definitions + values of a WING subtree over OSC (`,s "#"`).

usage: python3 -I dump_tree.py HOST OSC_PORT /ch/1 [/bus/1 ...] [--depth N]
Prints `path  definition = value` lines. Used to build the address tables in
docs/research/wing-control.md.
"""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wingosc import OscClient  # noqa: E402


def entries(c, path):
    r = c.query(path, "#", timeout=1.0)
    if not r or not r[1]:
        return []
    out = []
    for e in r[1][0].split("~ "):
        e = e.strip()
        if not e:
            continue
        name, _, rest = e.partition(" ")
        out.append((name, rest))
    return out


def walk(c, path, depth):
    for name, rest in entries(c, path):
        child = path.rstrip("/") + "/" + name
        if rest.startswith("node"):
            if depth > 0:
                walk(c, child, depth - 1)
            else:
                print(f"{child}  node")
        else:
            print(f"{child}  {rest}")


if __name__ == "__main__":
    args = sys.argv[1:]
    depth = 6
    if "--depth" in args:
        i = args.index("--depth"); depth = int(args[i + 1]); del args[i:i + 2]
    host, port, paths = args[0], int(args[1]), args[2:]
    c = OscClient(host, port)
    for p in paths:
        walk(c, p, depth)
