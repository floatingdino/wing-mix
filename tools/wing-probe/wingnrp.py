"""Minimal WING native-protocol ("NRP", TCP 2222) client + meter receiver, Python stdlib only.

Framing: 0xDF escape, `DF Dx` selects logical channel x (1 = control, 3 = meter requests).
Tokens (channel 1): `D7 <hash u32>` select node, `DC` get, `DD` definitions, value tokens set
(console->client: same tokens = value / change event), `DE` end of response.
Meters: request on channel 3 (`D3 port`, `D4 id DC <type idx..> DE`), data arrives as UDP
`id u32 + int16[]` (big-endian) every ~50 ms for 5 s after the last `D4 id` renew.
Spec: wing-simulator docs/research/protocol.md sections 2-4; findings: docs/research/wing-control.md.
"""
import socket
import struct
import time

NATIVE_PORT = 2222
CH_CONTROL = 1
CH_METER = 3

# ---- node hash (wing-model hash.rs / wing-edit-schema.md section 3) -------------------------
_SEED = 0xCB21C6BE
M32 = 0xFFFFFFFF


def _rotl(x, r):
    return ((x << r) | (x >> (32 - r))) & M32


def _mix(h, k):
    k = (k * 0xCC9E2D51) & M32
    k = _rotl(k, 15)
    k = (k * 0x1B873593) & M32
    h ^= k
    h = _rotl(h, 13)
    return (h * 5 + 0xE6546B64) & M32


def wing_hash(path: str) -> int:
    """Hash of a slash path. Model-specific names (eq/lg, dyn/thr) must be given by slot
    position (eq/1, dyn/1): ids are positional."""
    b = path.strip("/")
    if not b:
        return 0
    h, k, cnt, total = _SEED, 0, 0, 0
    for ch in b:
        c = ord(ch)
        if ch == "/":
            c = ord(".")
        elif "a" <= ch <= "z":
            c -= 0x20
        elif ch == " ":
            continue
        k = ((k << 8) | c) & M32
        cnt += 1
        total += 1
        if cnt == 4:
            h, k, cnt = _mix(h, k), 0, 0
    if cnt:
        h = _mix(h, k)
    return h ^ total


assert wing_hash("/ch/1/mute") == 0xF50F69F8
assert wing_hash("ch/1/fdr") == 0x38AE75C2


# ---- framing ---------------------------------------------------------------------------------
def frame(ch: int, payload: bytes) -> bytes:
    """Select channel `ch` and escape every payload DF as DF DE."""
    return bytes([0xDF, 0xD0 + ch]) + payload.replace(b"\xdf", b"\xdf\xde")


class Demux:
    def __init__(self):
        self.esc = False
        self.ch = -1
        self.out = {}

    def feed(self, data: bytes):
        for b in data:
            if b == 0xDF and not self.esc:
                self.esc = True
                continue
            if self.esc:
                if b != 0xDF:
                    self.esc = False
                    if b == 0xDE:
                        b = 0xDF
                    elif 0xD0 <= b < 0xD0 + 14:
                        self.ch = b - 0xD0
                        continue
                    elif self.ch >= 0:
                        self.out.setdefault(self.ch, bytearray()).append(0xDF)
            if self.ch >= 0:
                self.out.setdefault(self.ch, bytearray()).append(b)

    def take(self, ch):
        return bytes(self.out.pop(ch, b""))


# ---- value tokens ----------------------------------------------------------------------------
def enc_value(v) -> bytes:
    if isinstance(v, bool):
        v = int(v)
    if isinstance(v, float):
        return b"\xd5" + struct.pack(">f", v)  # engineering units (dB, Hz, ...)
    if isinstance(v, int):
        if 0 <= v <= 63:
            return bytes([v])
        if -32768 <= v <= 32767:
            return b"\xd3" + struct.pack(">h", v)
        return b"\xd4" + struct.pack(">i", v)
    if isinstance(v, str):
        raw = v.encode()
        if not raw:
            return b"\xd0"
        if len(raw) <= 64:
            return bytes([0x7F + len(raw)]) + raw
        return bytes([0xD1, len(raw) - 1]) + raw
    raise TypeError(v)


def parse_tokens(buf: bytes):
    """Yield ('hash', h) / ('val', v) / ('end',) / ('def', bytes) / ('nav', tok) tuples."""
    i = 0
    n = len(buf)
    while i < n:
        t = buf[i]; i += 1
        if t <= 0x3F:
            yield ("val", t)
        elif t <= 0x7F:
            yield ("nav", t)
        elif t <= 0xBF:
            ln = t - 0x7F
            yield ("val", buf[i:i + ln].decode(errors="replace")); i += ln
        elif t <= 0xCF:
            ln = t - 0xBF
            yield ("nav", buf[i:i + ln]); i += ln
        elif t == 0xD0:
            yield ("val", "")
        elif t == 0xD1:
            ln = buf[i] + 1; i += 1
            yield ("val", buf[i:i + ln].decode(errors="replace")); i += ln
        elif t == 0xD2:
            i += 2; yield ("nav", t)
        elif t == 0xD3:
            yield ("val", struct.unpack_from(">h", buf, i)[0]); i += 2
        elif t == 0xD4:
            yield ("val", struct.unpack_from(">i", buf, i)[0]); i += 4
        elif t in (0xD5, 0xD6):
            yield ("val", struct.unpack_from(">f", buf, i)[0]); i += 4
        elif t == 0xD7:
            yield ("hash", struct.unpack_from(">I", buf, i)[0]); i += 4
        elif t == 0xDE:
            yield ("end",)
        elif t == 0xDF:
            ln = struct.unpack_from(">H", buf, i)[0]; i += 2
            if ln == 0:
                ln = struct.unpack_from(">I", buf, i)[0]; i += 4
            yield ("def", buf[i:i + ln]); i += ln
        else:
            yield ("other", t)


class NativeClient:
    def __init__(self, host="127.0.0.1", port=NATIVE_PORT, timeout=1.0):
        self.host = host
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.demux = Demux()
        self.pending = bytearray()
        self.events = []  # (hash, value) change events seen
        self.sock.sendall(bytes([0xDF, 0xD1]))

    def _pump(self, timeout):
        self.sock.settimeout(timeout)
        try:
            data = self.sock.recv(65536)
        except socket.timeout:
            return False
        if not data:
            raise ConnectionError("closed")
        self.demux.feed(data)
        self.pending += self.demux.take(CH_CONTROL)
        return True

    def _complete_tokens(self):
        """Parse pending bytes; keep an incomplete tail (rare with small messages)."""
        buf = bytes(self.pending)
        self.pending.clear()
        try:
            return list(parse_tokens(buf))
        except (struct.error, IndexError):
            self.pending += buf
            return []

    def send(self, payload: bytes, ch=CH_CONTROL):
        self.sock.sendall(frame(ch, payload))

    def get(self, path, timeout=1.0):
        h = wing_hash(path)
        self.send(b"\xd7" + struct.pack(">I", h) + b"\xdc")
        deadline = time.time() + timeout
        cur, val = None, None
        while time.time() < deadline:
            if not self._pump(deadline - time.time()):
                break
            for tok in self._complete_tokens():
                if tok[0] == "hash":
                    cur = tok[1]
                elif tok[0] == "val" and cur is not None:
                    if cur == h:
                        val = tok[1]
                    else:
                        self.events.append((cur, tok[1]))
                elif tok[0] == "end" and val is not None:
                    return val
        return val

    def set(self, path, value):
        self.send(b"\xd7" + struct.pack(">I", wing_hash(path)) + enc_value(value))

    def toggle(self, path):
        self.send(b"\xd7" + struct.pack(">I", wing_hash(path)) + b"\xd8")

    def collect_events(self, seconds):
        """Read unsolicited change events for `seconds`; returns [(hash, value)]."""
        deadline = time.time() + seconds
        out = []
        cur = None
        while time.time() < deadline:
            if not self._pump(max(0.01, deadline - time.time())):
                continue
            for tok in self._complete_tokens():
                if tok[0] == "hash":
                    cur = tok[1]
                elif tok[0] == "val" and cur is not None:
                    out.append((cur, tok[1]))
        self.events += out
        return out

    def children(self, path):
        """Node definition records (DD) of `path`: list of (hash, index, name, longname, flags)."""
        h = wing_hash(path)
        self.send(b"\xd7" + struct.pack(">I", h) + b"\xdd")
        recs = []
        deadline = time.time() + 2
        while time.time() < deadline:
            if not self._pump(deadline - time.time()):
                break
            done = False
            for tok in self._complete_tokens():
                if tok[0] == "def":
                    r = tok[1]
                    parent, nh, idx = struct.unpack_from(">IIH", r, 0)
                    j = 10
                    nl = r[j]; name = r[j + 1:j + 1 + nl].decode(); j += 1 + nl
                    ll = r[j]; lname = r[j + 1:j + 1 + ll].decode(); j += 1 + ll
                    flags = struct.unpack_from(">H", r, j)[0]
                    recs.append((nh, idx, name, lname, flags))
                elif tok[0] == "end":
                    done = True
            if done:
                break
        return recs

    # ---- meters (channel 3 request, UDP data) ----
    def meter_request(self, udp_port, report_id, selections):
        """selections: list of (type_token, [zero-based indices]); A9 monitor / AA RTA take []."""
        p = b"\xd3" + struct.pack(">H", udp_port)
        p += b"\xd4" + struct.pack(">I", report_id) + b"\xdc"
        for typ, idxs in selections:
            p += bytes([typ]) + bytes(idxs)
        p += b"\xde"
        self.send(p, ch=CH_METER)
        self.send(b"", ch=CH_CONTROL)

    def meter_renew(self, report_id):
        self.send(b"\xd4" + struct.pack(">I", report_id), ch=CH_METER)
        self.send(b"", ch=CH_CONTROL)

    def close(self):
        self.sock.close()


def meter_socket():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", 0))
    s.settimeout(1.0)
    return s


def read_meter_packet(s):
    data, src = s.recvfrom(65536)
    rid = struct.unpack_from(">I", data, 0)[0]
    n = (len(data) - 4) // 2
    words = list(struct.unpack_from(f">{n}h", data, 4))
    return rid, words, src
