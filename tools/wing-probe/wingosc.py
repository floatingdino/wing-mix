"""Minimal WING OSC client (UDP 2223), Python stdlib only.

WING subset of OSC 1.0: messages only (no bundles), args i/f/s/b, big-endian, 4-byte padded.
Spec: wing-simulator docs/research/protocol.md section 5; findings: docs/research/wing-control.md.
"""
import socket
import struct
import time

OSC_PORT = 2223


def _pad(b: bytes) -> bytes:
    return b + b"\0" * (4 - len(b) % 4)


def _pad_blob(b: bytes) -> bytes:
    return b + b"\0" * ((4 - len(b) % 4) % 4)


def encode(addr: str, *args) -> bytes:
    tags = ","
    data = b""
    for a in args:
        if isinstance(a, bool):
            a = int(a)
        if isinstance(a, int):
            tags += "i"
            data += struct.pack(">i", a)
        elif isinstance(a, float):
            tags += "f"
            data += struct.pack(">f", a)
        elif isinstance(a, str):
            tags += "s"
            data += _pad(a.encode())
        elif isinstance(a, (bytes, bytearray)):
            tags += "b"
            data += struct.pack(">i", len(a)) + _pad_blob(bytes(a))
        else:
            raise TypeError(a)
    return _pad(addr.encode()) + _pad(tags.encode()) + data


def _read_str(buf: bytes, i: int):
    j = buf.index(b"\0", i)
    s = buf[i:j].decode(errors="replace")
    return s, (j + 4) & ~3


def decode(buf: bytes):
    addr, i = _read_str(buf, 0)
    if i >= len(buf) or buf[i:i + 1] != b",":
        return addr, []
    tags, i = _read_str(buf, i)
    args = []
    for t in tags[1:]:
        if t == "i":
            args.append(struct.unpack_from(">i", buf, i)[0]); i += 4
        elif t == "f":
            args.append(struct.unpack_from(">f", buf, i)[0]); i += 4
        elif t == "s":
            s, i = _read_str(buf, i); args.append(s)
        elif t == "b":
            n = struct.unpack_from(">i", buf, i)[0]; i += 4
            args.append(buf[i:i + n]); i += (n + 3) & ~3
        else:
            raise ValueError(f"tag {t}")
    return addr, args


class OscClient:
    def __init__(self, host="127.0.0.1", port=OSC_PORT, timeout=0.5):
        self.dst = (host, port)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("0.0.0.0", 0))
        self.sock.settimeout(timeout)

    def send(self, addr, *args):
        self.sock.sendto(encode(addr, *args), self.dst)

    def recv(self, timeout=None):
        if timeout is not None:
            self.sock.settimeout(timeout)
        try:
            data, _ = self.sock.recvfrom(65536)
        except socket.timeout:
            return None
        return decode(data)

    def drain(self):
        out = []
        while True:
            m = self.recv(0.05)
            if m is None:
                return out
            out.append(m)

    def query(self, addr, *args, timeout=0.5):
        """Send and return the first reply (or None)."""
        self.drain()
        self.send(addr, *args)
        return self.recv(timeout)

    def get(self, path):
        return self.query(path)

    def set(self, path, value):
        """Leaf set. Not echoed; returns an error reply if one arrives within 100 ms."""
        self.drain()
        self.send(path, value)
        return self.recv(0.1)

    def close(self):
        self.sock.close()


if __name__ == "__main__":
    import sys
    host, port = sys.argv[1], int(sys.argv[2])
    c = OscClient(host, port)
    for a in sys.argv[3:]:
        if "=" in a:
            p, v = a.split("=", 1)
            for conv in (int, float):
                try:
                    v = conv(v); break
                except ValueError:
                    pass
            print(a, "->", c.set(p, v))
        elif ":" in a:
            p, s = a.split(":", 1)
            print(a, "->", c.query(p, s))
        else:
            print(a, "->", c.get(a))
