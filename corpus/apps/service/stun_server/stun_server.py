"""Oracle for stun_server.kara: a STUN server (RFC 5389, Binding only) on
loopback UDP. One reader takes datagrams off the socket and hands each to a
thread of its own, which answers on the shared socket; scripted clients on
the main thread send well-formed and broken messages and print what comes
back."""

import socket
import threading
import zlib

ADDR = ("127.0.0.1", 47766)
DATAGRAMS = 13
COOKIE = 0x2112A442
SOFTWARE_NAME = "kara-stun 1.0"
QUIET_MS = 300

BINDING = 0x001
REQUEST, INDICATION, SUCCESS, FAILURE = 0, 1, 2, 3

XOR_MAPPED_ADDRESS = 0x0020
ERROR_CODE = 0x0009
UNKNOWN_ATTRIBUTES = 0x000A
SOFTWARE = 0x8022
FINGERPRINT = 0x8028
FINGERPRINT_XOR = 0x5354554E


def u16(n):
    return n.to_bytes(2, "big")


def u32(n):
    return n.to_bytes(4, "big")


def be16(b, at):
    return int.from_bytes(b[at:at + 2], "big")


def be32(b, at):
    return int.from_bytes(b[at:at + 4], "big")


def msg_type(method, cls):
    """The 14-bit type: the class's two bits sit at bits 4 and 8."""
    return ((method & 0xF80) << 2) | ((method & 0x070) << 1) | (method & 0x00F) \
        | ((cls & 2) << 7) | ((cls & 1) << 4)


def split_type(t):
    cls = ((t >> 7) & 2) | ((t >> 4) & 1)
    method = ((t >> 2) & 0xF80) | ((t >> 1) & 0x070) | (t & 0x00F)
    return method, cls


def crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


def attribute(kind, value):
    pad = (4 - len(value) % 4) % 4
    return u16(kind) + u16(len(value)) + value + b"\0" * pad


def message(method, cls, txid, attrs, fingerprint=True):
    """A message of `attrs`, ending with a FINGERPRINT when asked."""
    body = b"".join(attrs)
    if fingerprint:
        head = u16(msg_type(method, cls)) + u16(len(body) + 8) + u32(COOKIE) + txid
        crc = crc32(head + body) ^ FINGERPRINT_XOR
        body += attribute(FINGERPRINT, u32(crc))
    return u16(msg_type(method, cls)) + u16(len(body)) + u32(COOKIE) + txid + body


def parse(pkt):
    """(method, class, txid, [(type, value, offset)]) or a reason to drop."""
    if len(pkt) < 20:
        return "short"
    if pkt[0] & 0xC0:
        return "not stun"
    if be32(pkt, 4) != COOKIE:
        return "bad cookie"
    length = be16(pkt, 2)
    if length != len(pkt) - 20 or length % 4 != 0:
        return "bad length"
    attrs = []
    at = 20
    while at < len(pkt):
        if at + 4 > len(pkt):
            return "bad attribute"
        kind = be16(pkt, at)
        n = be16(pkt, at + 2)
        if at + 4 + n > len(pkt):
            return "bad attribute"
        attrs.append((kind, pkt[at + 4:at + 4 + n], at))
        at += 4 + n + (4 - n % 4) % 4
    for i, (kind, value, off) in enumerate(attrs):
        if kind == FINGERPRINT:
            if i != len(attrs) - 1 or len(value) != 4:
                return "bad fingerprint"
            if be32(value, 0) != crc32(pkt[:off]) ^ FINGERPRINT_XOR:
                return "bad fingerprint"
    method, cls = split_type(be16(pkt, 0))
    return method, cls, pkt[8:20], attrs


def xor_address(host, port):
    a = bytes(int(p) for p in host.split("."))
    x = bytes(b ^ c for b, c in zip(a, u32(COOKIE)))
    return b"\0\x01" + u16(port ^ (COOKIE >> 16)) + x


def error_attr(code, reason):
    return attribute(ERROR_CODE, b"\0\0" + bytes([code // 100, code % 100]) + reason.encode())


class Server:
    def __init__(self):
        self.lock = threading.Lock()
        self.log = {}
        self.counts = {}

    def note(self, n, outcome, line):
        with self.lock:
            self.log[n] = line
            self.counts[outcome] = self.counts.get(outcome, 0) + 1


def handle(srv, sock, n, pkt, peer):
    got = parse(pkt)
    if isinstance(got, str):
        srv.note(n, "dropped", f"dropped: {got}")
        return
    method, cls, txid, attrs = got
    if cls in (SUCCESS, FAILURE):
        srv.note(n, "dropped", "dropped: a response")
        return
    unknown = [k for k, _, _ in attrs if k < 0x8000]
    if cls == INDICATION:
        srv.note(n, "indications", f"indication, method {method}")
        return
    soft = attribute(SOFTWARE, SOFTWARE_NAME.encode())
    if method != BINDING:
        reply = message(method, FAILURE, txid, [error_attr(400, "Bad Request"), soft])
        sock.sendto(reply, peer)
        srv.note(n, "errors", f"request, method {method}: 400")
        return
    if unknown:
        listed = b"".join(u16(k) for k in unknown)
        reply = message(method, FAILURE, txid,
                        [error_attr(420, "Unknown Attribute"),
                         attribute(UNKNOWN_ATTRIBUTES, listed), soft])
        sock.sendto(reply, peer)
        srv.note(n, "errors", f"binding request: 420, {len(unknown)} unknown")
        return
    optional = len([k for k, _, _ in attrs if k not in (SOFTWARE, FINGERPRINT)])
    mapped = attribute(XOR_MAPPED_ADDRESS, xor_address(peer[0], peer[1]))
    sock.sendto(message(method, SUCCESS, txid, [mapped, soft]), peer)
    srv.note(n, "answered",
             f"binding request: success, {len(attrs)} attributes, {optional} ignored")


def run_server(srv, sock):
    threads = []
    for n in range(1, DATAGRAMS + 1):
        pkt, peer = sock.recvfrom(2048)
        t = threading.Thread(target=handle, args=(srv, sock, n, pkt, peer))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


# ------------------------------------------------------------ the clients


def txid_of(seed):
    return bytes((seed * 17 + i * 5) % 256 for i in range(12))


def describe(pkt, txid, own):
    got = parse(pkt)
    if isinstance(got, str):
        return f"unreadable reply: {got}"
    method, cls, tx, attrs = got
    parts = []
    parts.append({SUCCESS: "success", FAILURE: "error"}.get(cls, f"class {cls}"))
    if tx != txid:
        parts.append("WRONG transaction")
    for kind, value, _ in attrs:
        if kind == XOR_MAPPED_ADDRESS:
            port = be16(value, 2) ^ (COOKIE >> 16)
            host = ".".join(str(b ^ c) for b, c in zip(value[4:8], u32(COOKIE)))
            parts.append(f"mapped is own address: {'yes' if (host, port) == own else 'no'}")
        elif kind == ERROR_CODE:
            code = value[2] * 100 + value[3]
            parts.append(f"code {code} {value[4:].decode()}")
        elif kind == UNKNOWN_ATTRIBUTES:
            kinds = [f"0x{be16(value, i):04x}" for i in range(0, len(value), 2)]
            parts.append(f"unknown [{', '.join(kinds)}]")
        elif kind == SOFTWARE:
            parts.append(f"software \"{value.decode()}\"")
        elif kind == FINGERPRINT:
            parts.append("fingerprint ok")
        else:
            parts.append(f"attribute 0x{kind:04x}")
    return ", ".join(parts)


class Client:
    def __init__(self, name, seed):
        self.name = name
        self.txid = txid_of(seed)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))

    def send(self, pkt):
        self.sock.sendto(pkt, ADDR)

    def recv(self, wait_ms):
        """Prints the reply, or that none came within `wait_ms`."""
        self.sock.settimeout(wait_ms / 1000)
        try:
            pkt, _ = self.sock.recvfrom(2048)
        except socket.timeout:
            print(f"{self.name}: no reply")
            return
        print(f"{self.name}: {describe(pkt, self.txid, self.sock.getsockname())}")
        self.sock.close()


def main():
    srv = Server()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(ADDR)
    server = threading.Thread(target=run_server, args=(srv, sock))
    server.start()

    c = Client("plain", 1)
    c.send(message(BINDING, REQUEST, c.txid, [], fingerprint=False))
    c.recv(2000)

    c = Client("fingerprinted", 2)
    c.send(message(BINDING, REQUEST, c.txid, []))
    c.recv(2000)

    c = Client("padded", 3)
    c.send(message(BINDING, REQUEST, c.txid,
                   [attribute(SOFTWARE, b"probe"), attribute(0x8030, b"\x07")]))
    c.recv(2000)

    c = Client("bad-fingerprint", 4)
    pkt = bytearray(message(BINDING, REQUEST, c.txid, []))
    pkt[-1] ^= 0xFF
    c.send(bytes(pkt))
    c.recv(QUIET_MS)

    c = Client("unknown-attrs", 5)
    c.send(message(BINDING, REQUEST, c.txid,
                   [attribute(0x7001, b"abcd"), attribute(0x8030, b""),
                    attribute(0x0003, b"\0\0\0\0")]))
    c.recv(2000)

    c = Client("indication", 6)
    c.send(message(BINDING, INDICATION, c.txid, []))
    c.recv(QUIET_MS)

    c = Client("bad-cookie", 7)
    pkt = bytearray(message(BINDING, REQUEST, c.txid, [], fingerprint=False))
    pkt[4] = 0x99
    c.send(bytes(pkt))
    c.recv(QUIET_MS)

    c = Client("short", 8)
    c.send(u16(msg_type(BINDING, REQUEST)) + u16(0) + u32(COOKIE) + b"\0\0")
    c.recv(QUIET_MS)

    c = Client("allocate", 9)
    c.send(message(0x003, REQUEST, c.txid, []))
    c.recv(2000)

    c = Client("response", 10)
    c.send(message(BINDING, SUCCESS, c.txid, []))
    c.recv(QUIET_MS)

    c = Client("bad-length", 11)
    pkt = bytearray(message(BINDING, REQUEST, c.txid, [], fingerprint=False))
    pkt[3] = 8
    c.send(bytes(pkt))
    c.recv(QUIET_MS)

    a = Client("pair-a", 12)
    b = Client("pair-b", 13)
    a.send(message(BINDING, REQUEST, a.txid, []))
    b.send(message(BINDING, REQUEST, b.txid, [attribute(SOFTWARE, b"pair")]))
    a.recv(2000)
    b.recv(2000)

    server.join()
    sock.close()
    print("== server")
    for n in sorted(srv.log):
        print(f"#{n} {srv.log[n]}")
    for k in sorted(srv.counts):
        print(f"{k}: {srv.counts[k]}")


main()
