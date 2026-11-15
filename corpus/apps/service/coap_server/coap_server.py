"""Oracle for coap_server.kara: a CoAP server (RFC 7252, with block-wise
transfer from RFC 7959) on loopback UDP, and the clients that use it. One
server thread answers each datagram in turn from a tree of resources; the
clients on the main thread send well-formed and broken messages and print
what comes back."""

import socket
import threading

ADDR = ("127.0.0.1", 47783)
QUIET = 0.3

CON, NON, ACK, RST = 0, 1, 2, 3
TYPES = ["CON", "NON", "ACK", "RST"]

GET, POST, PUT, DELETE = 1, 2, 3, 4
METHODS = {1: "GET", 2: "POST", 3: "PUT", 4: "DELETE"}

ETAG, OBSERVE, URI_PATH, CONTENT_FORMAT, URI_QUERY, BLOCK2, SIZE2, SIZE1 = 4, 6, 11, 12, 15, 23, 28, 60
OPTION_NAMES = {4: "ETag", 6: "Observe", 11: "Uri-Path", 12: "Content-Format", 15: "Uri-Query",
                23: "Block2", 28: "Size2", 60: "Size1"}
KNOWN = {URI_PATH, CONTENT_FORMAT, URI_QUERY, BLOCK2, SIZE1}
TEXT, LINK, XML, JSON = 0, 40, 41, 50
BLOCK_SZX = 2


def code(c, d):
    return c << 5 | d


def code_text(c):
    return f"{c >> 5}.{c & 31:02d}"


def uint_bytes(n):
    out = []
    while n > 0:
        out.insert(0, n & 0xFF)
        n >>= 8
    return bytes(out)


def uint_of(b):
    n = 0
    for x in b:
        n = n << 8 | x
    return n


def nibble(n):
    """The 4-bit field for a delta or length, and the extended bytes."""
    if n < 13:
        return n, b""
    if n < 269:
        return 13, bytes([n - 13])
    return 14, (n - 269).to_bytes(2, "big")


def encode(typ, c, mid, token, options, payload):
    out = bytearray([0x40 | typ << 4 | len(token), c])
    out += mid.to_bytes(2, "big")
    out += token
    last = 0
    for num, val in sorted(options, key=lambda o: o[0]):
        d, dext = nibble(num - last)
        l, lext = nibble(len(val))
        out.append(d << 4 | l)
        out += dext + lext + val
        last = num
    if payload:
        out.append(0xFF)
        out += payload
    return bytes(out)


class FormatError(Exception):
    pass


def extended(n, b, at):
    if n < 13:
        return n, at
    if n == 13:
        if at >= len(b):
            raise FormatError("truncated option")
        return b[at] + 13, at + 1
    if n == 14:
        if at + 2 > len(b):
            raise FormatError("truncated option")
        return int.from_bytes(b[at:at + 2], "big") + 269, at + 2
    raise FormatError("reserved nibble 15")


def decode(b):
    if len(b) < 4:
        raise FormatError("short header")
    ver, typ, tkl = b[0] >> 6, b[0] >> 4 & 3, b[0] & 15
    if ver != 1:
        raise FormatError(f"version {ver}")
    if tkl > 8:
        raise FormatError(f"token length {tkl}")
    if len(b) < 4 + tkl:
        raise FormatError("short token")
    c, mid = b[1], int.from_bytes(b[2:4], "big")
    token = b[4:4 + tkl]
    at, last, options, payload = 4 + tkl, 0, [], b""
    while at < len(b):
        if b[at] == 0xFF:
            if at + 1 == len(b):
                raise FormatError("empty payload after marker")
            payload = b[at + 1:]
            break
        d, l = b[at] >> 4, b[at] & 15
        at += 1
        d, at = extended(d, b, at)
        l, at = extended(l, b, at)
        if at + l > len(b):
            raise FormatError("option runs past the end")
        last += d
        options.append((last, b[at:at + l]))
        at += l
    return {"type": typ, "code": c, "mid": mid, "token": token, "options": options, "payload": payload}


def option(m, num):
    return [v for n, v in m["options"] if n == num]


def show_option(num, val):
    name = OPTION_NAMES.get(num, f"#{num}")
    if num in (URI_PATH, URI_QUERY):
        return f"{name}={val.decode()}"
    if num == ETAG:
        return f"{name}={val.hex()}"
    if num == BLOCK2:
        v = uint_of(val)
        return f"{name}={v >> 4}/{v >> 3 & 1}/{16 << (v & 7)}"
    return f"{name}={uint_of(val)}"


def show(m):
    opts = " ".join(show_option(n, v) for n, v in m["options"])
    line = f"{TYPES[m['type']]} {code_text(m['code'])} mid={m['mid']:04x} token={m['token'].hex() or '-'}"
    if opts:
        line += " " + opts
    if m["payload"]:
        p = m["payload"]
        text = p.decode().replace("\n", "\\n") if len(p) <= 60 else f"{len(p)} bytes"
        line += f" | {text}"
    return line


class Resource:
    def __init__(self, body, fmt):
        self.body = body
        self.fmt = fmt
        self.version = 1


class Server:
    def __init__(self):
        self.tree = {
            "hello": Resource(b"hello, world", TEXT),
            "sensors/temp": Resource(b"21.5", TEXT),
            "big": Resource(b"".join(f"line {i:02d} of the big resource\n".encode() for i in range(10)), TEXT),
        }
        self.log = []
        self.seen = {}
        self.next_mid = 0x7000
        self.counts = {}
        self.stopping = False

    def count(self, what):
        self.counts[what] = self.counts.get(what, 0) + 1

    def well_known(self):
        return ",".join(f"</{p}>;ct={r.fmt}" for p, r in sorted(self.tree.items())).encode()

    def answer(self, m):
        """The response's code, options and payload for request `m`."""
        unknown = [n for n, _ in m["options"] if n not in KNOWN and n % 2 == 1]
        if unknown:
            return code(4, 2), [], f"unknown critical option {unknown[0]}".encode()
        path = "/".join(v.decode() for v in option(m, URI_PATH))
        query = [v.decode() for v in option(m, URI_QUERY)]
        method = m["code"]
        if path == ".well-known/core":
            if method != GET:
                return code(4, 5), [], b""
            return code(2, 5), [(CONTENT_FORMAT, uint_bytes(LINK))], self.well_known()
        if path == "halt":
            if method != POST:
                return code(4, 5), [], b""
            self.stopping = True
            return code(2, 4), [], b"halting"
        if path == "log":
            if method != POST:
                return code(4, 5), [], b""
            self.log.append(m["payload"].decode())
            return code(2, 4), [], f"{len(self.log)} entries".encode()
        res = self.tree.get(path)
        if method == GET:
            if res is None:
                return code(4, 4), [], f"no /{path}".encode()
            body = res.body
            if path == "hello" and query:
                names = [q[5:] for q in query if q.startswith("name=")]
                if names:
                    body = f"hello, {names[0]}".encode()
            opts = [(CONTENT_FORMAT, uint_bytes(res.fmt)), (ETAG, uint_bytes(res.version))]
            want = option(m, BLOCK2)
            if want or len(body) > 16 << BLOCK_SZX:
                v = uint_of(want[0]) if want else BLOCK_SZX
                num, szx = v >> 4, min(v & 7, BLOCK_SZX)
                size = 16 << szx
                if num * size >= len(body):
                    return code(4, 2), [], b"block out of range"
                more = (num + 1) * size < len(body)
                opts.append((BLOCK2, uint_bytes(num << 4 | more << 3 | szx)))
                if num == 0:
                    opts.append((SIZE2, uint_bytes(len(body))))
                body = body[num * size:(num + 1) * size]
            return code(2, 5), opts, body
        if method == PUT:
            fmt = option(m, CONTENT_FORMAT)
            fmt = uint_of(fmt[0]) if fmt else TEXT
            if fmt not in (TEXT, JSON):
                return code(4, 15), [], f"format {fmt}".encode()
            if path in ("", "hello"):
                return code(4, 5), [], b""
            size1 = option(m, SIZE1)
            if size1 and uint_of(size1[0]) > 64:
                return code(4, 13), [(SIZE1, uint_bytes(64))], b""
            if res is None:
                self.tree[path] = Resource(m["payload"], fmt)
                return code(2, 1), [], b""
            res.body, res.fmt, res.version = m["payload"], fmt, res.version + 1
            return code(2, 4), [(ETAG, uint_bytes(res.version))], b""
        if method == DELETE:
            if res is None:
                return code(4, 4), [], f"no /{path}".encode()
            del self.tree[path]
            return code(2, 2), [], b""
        return code(4, 5), [], b""

    def handle(self, sock, data, peer):
        """The reply to one datagram, or None."""
        try:
            m = decode(data)
        except FormatError as e:
            self.count("malformed")
            print_log(self, f"malformed: {e}")
            if len(data) >= 4 and data[0] >> 6 == 1 and data[0] >> 4 & 3 == CON:
                return encode(RST, 0, int.from_bytes(data[2:4], "big"), b"", [], b"")
            return None
        typ = m["type"]
        if typ in (ACK, RST):
            self.count("stray " + TYPES[typ])
            print_log(self, f"stray {TYPES[typ]} mid={m['mid']:04x}")
            return None
        if m["code"] == 0:
            self.count("ping")
            print_log(self, f"ping mid={m['mid']:04x}")
            return encode(RST, 0, m["mid"], b"", [], b"") if typ == CON else None
        if m["code"] >> 5 != 0:
            self.count("response")
            print_log(self, f"a response {code_text(m['code'])} sent to the server")
            return encode(RST, 0, m["mid"], b"", [], b"") if typ == CON else None
        key = (peer, m["mid"])
        if typ == CON and key in self.seen:
            self.count("duplicate")
            print_log(self, f"duplicate mid={m['mid']:04x}, answered again")
            return self.seen[key]
        c, opts, payload = self.answer(m)
        path = "/".join(v.decode() for v in option(m, URI_PATH))
        name = METHODS.get(m["code"], code_text(m["code"]))
        print_log(self, f"{TYPES[typ]} {name} /{path} -> {code_text(c)}")
        self.count(code_text(c))
        if typ == CON:
            reply = encode(ACK, c, m["mid"], m["token"], opts, payload)
            self.seen[key] = reply
        else:
            reply = encode(NON, c, self.next_mid, m["token"], opts, payload)
            self.next_mid += 1
        return reply


def print_log(srv, line):
    srv.lines.append(line)


def serve(srv, sock):
    while not srv.stopping:
        data, peer = sock.recvfrom(2048)
        reply = srv.handle(sock, data, peer)
        if reply is not None:
            sock.sendto(reply, peer)


class Client:
    def __init__(self, name):
        self.name = name
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))

    def send(self, data):
        self.sock.sendto(data, ADDR)

    def recv(self, wait):
        self.sock.settimeout(wait)
        try:
            data, _ = self.sock.recvfrom(2048)
        except socket.timeout:
            print(f"{self.name}: no reply")
            return None
        try:
            m = decode(data)
        except FormatError as e:
            print(f"{self.name}: unreadable reply: {e}")
            return None
        print(f"{self.name}: {show(m)}")
        return m

    def ask(self, data, wait=2.0):
        self.send(data)
        return self.recv(wait)


def path_options(path):
    return [(URI_PATH, p.encode()) for p in path.split("/") if p]


def main():
    srv = Server()
    srv.lines = []
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(ADDR)
    server = threading.Thread(target=serve, args=(srv, sock))
    server.start()

    c = Client("ping")
    c.ask(encode(CON, 0, 0x0101, b"", [], b""))

    c = Client("discover")
    c.ask(encode(CON, GET, 0x0102, b"\x01", path_options(".well-known/core"), b""))

    c = Client("hello")
    c.ask(encode(CON, GET, 0x0103, b"\xa1\xb2", path_options("hello"), b""))
    c.ask(encode(NON, GET, 0x0104, b"\xa1\xb3", path_options("hello") + [(URI_QUERY, b"name=kara")], b""))
    c.ask(encode(CON, GET, 0x0105, b"", path_options("sensors/temp"), b""))
    c.ask(encode(NON, GET, 0x0106, b"\x09", path_options("missing/thing"), b""))
    c.ask(encode(CON, POST, 0x0107, b"\x0a", path_options("hello"), b"hi"))

    c = Client("blocks")
    body = b""
    num = 0
    while True:
        opts = path_options("big")
        if num > 0:
            opts.append((BLOCK2, uint_bytes(num << 4 | BLOCK_SZX)))
        m = c.ask(encode(CON, GET, 0x0200 + num, bytes([num]), opts, b""))
        if m is None:
            break
        body += m["payload"]
        b2 = option(m, BLOCK2)
        if not b2 or not uint_of(b2[0]) >> 3 & 1:
            break
        num += 1
    lines = body.decode().splitlines()
    print(f"blocks: {len(body)} bytes in {num + 1} blocks, last line {lines[-1]!r}")
    c.ask(encode(CON, GET, 0x0210, b"", path_options("big") + [(BLOCK2, uint_bytes(1 << 4 | 1))], b""))
    c.ask(encode(CON, GET, 0x0211, b"", path_options("big") + [(BLOCK2, uint_bytes(40 << 4 | 2))], b""))

    c = Client("notes")
    c.ask(encode(CON, PUT, 0x0301, b"\x31", path_options("notes/today"), b"first draft"))
    c.ask(encode(CON, PUT, 0x0302, b"\x32", path_options("notes/today") + [(CONTENT_FORMAT, uint_bytes(JSON))], b'{"v": 2}'))
    c.ask(encode(CON, GET, 0x0303, b"\x33", path_options("notes/today"), b""))
    c.ask(encode(CON, PUT, 0x0304, b"\x34", path_options("notes/xml") + [(CONTENT_FORMAT, uint_bytes(XML))], b"<a/>"))
    c.ask(encode(CON, PUT, 0x0305, b"\x35", path_options("notes/huge") + [(SIZE1, uint_bytes(4000))], b"x"))
    c.ask(encode(CON, GET, 0x0306, b"\x36", path_options(".well-known/core"), b""))

    c = Client("logger")
    entry = encode(CON, POST, 0x0401, b"\x41\x42", path_options("log"), b"boot ok")
    c.ask(entry)
    c.ask(entry)
    c.ask(encode(CON, POST, 0x0402, b"\x43", path_options("log"), b"second"))

    c = Client("options")
    c.ask(encode(CON, GET, 0x0501, b"", path_options("hello") + [(9, b"x")], b""))
    c.ask(encode(CON, GET, 0x0502, b"", path_options("hello") + [(8, b"y"), (2052, b"z")], b""))
    long = "name=" + "k" * 20 + "-" + "a" * 280
    m = c.ask(encode(CON, GET, 0x0503, b"\x55", path_options("hello") + [(URI_QUERY, long.encode())], b""))
    if m is not None:
        print(f"options: echoed {len(m['payload'])} bytes")

    c = Client("broken")
    c.send(bytes([0x80, GET, 0x06, 0x01]))
    c.recv(QUIET)
    c.send(bytes([0x49, GET, 0x06, 0x02]) + b"\x00" * 9)
    c.recv(QUIET)
    c.send(bytes([0x40, GET, 0x06, 0x03, 0xB5]) + b"he")
    c.recv(QUIET)
    c.send(bytes([0x50, GET, 0x06, 0x04, 0xFF]))
    c.recv(QUIET)
    c.send(encode(ACK, 0, 0x0605, b"", [], b""))
    c.recv(QUIET)
    c.send(encode(CON, code(2, 5), 0x0606, b"", [], b"oops"))
    c.recv(QUIET)
    c.send(encode(NON, 0, 0x0607, b"", [], b""))
    c.recv(QUIET)

    c = Client("cleanup")
    c.ask(encode(CON, DELETE, 0x0701, b"", path_options("notes/today"), b""))
    c.ask(encode(CON, DELETE, 0x0702, b"", path_options("notes/today"), b""))
    c.ask(encode(CON, GET, 0x0703, b"", path_options(".well-known/core"), b""))

    c = Client("admin")
    c.ask(encode(CON, POST, 0x0801, b"\xad", path_options("halt"), b""))
    server.join()

    print("== server log")
    for i, line in enumerate(srv.lines, 1):
        print(f"{i:02d} {line}")
    print("== log resource")
    for e in srv.log:
        print(e)
    print("== counts")
    for k in sorted(srv.counts):
        print(f"{k}: {srv.counts[k]}")


main()
