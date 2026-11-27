"""docs/library/io.md § HTTP server: the server's HTTP/1 request parsing
against h11 (the pinned version below), a strict RFC 9112 parser, as an
offline reference. Each input is raw bytes on a fresh connection: valid
requests, mutated request lines and headers, header folding, duplicate and
malformed `Content-Length`, `Transfer-Encoding` codings, chunked bodies with
extensions, trailers and bad framing, a body longer than declared, and
pipelined requests. The server's handler echoes each request it is given
(method, target, every header, the body's bytes), so an accepted request is
compared field by field with h11's parse (a value's obs-text as the page
says the handler reads it, decoded with U+FFFD).

The verdicts:
  - both accept: the echoes must equal h11's requests, in order, the same
    number answered;
  - both refuse: the server must answer the refusal with a 4xx or 5xx from
    the page's list after the requests h11 accepted before it, and close;
  - they differ: a FAIL, unless the page or RFC 9112 lets a server choose
    (listed in ALLOWED, each with the rule that allows it); those are counted
    and reported, not failed.
After every input a fresh `GET` must still be answered: a server that died
is a crash, whatever the input was. The ASan lane ends the server through a
handler that calls `exit(0)`, so a leak or a bad access fails the run.

    python3 conformance/http_server_fuzz.py [--seed N] [--cases N] [--lanes build,asan]
"""

import argparse
import os
import random
import re
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import h11

from harness import KARAC

H11_VERSION = "0.16.0"
REFUSALS = {400, 408, 413, 414, 431, 501, 505}

SERVER = r'''import std.process.{exit};
import std.encoding.{hex_encode};
import std.net.{Request, Server};

struct Response { status: i64, body: String, headers: Vec[(String, String)] }

fn hx(s: Str) -> String {
    hex_encode(s.bytes())
}

fn handle(req: Request) -> Response {
    if req.path() == "/__quit" {
        exit(0);
    }
    let mut out = f"{hx(req.method())} {hx(req.url())}";
    for (k, v) in req.headers() {
        out = f"{out} {hx(k)}={hx(v)}";
    }
    out = f"{out} B{hex_encode(req.bytes().as_slice())}";
    Response { status: 200, body: out, headers: Vec.new() }
}

fn main() {
    let _ = Server.serve("127.0.0.1:0", |req| handle(req));
}
'''


# --- the reference ---------------------------------------------------------

def reference(data: bytes):
    """h11's reading of `data` as a server: (requests, verdict), each request
    (method, target, [(name, value)], body); verdict is "ok" when every byte
    was consumed into complete requests, "partial" when it stops inside one,
    ("refuse", hint) on a protocol error, "closed" when a request asked to
    close and bytes after it are never read."""
    conn = h11.Connection(h11.SERVER)
    conn.receive_data(data)
    reqs, cur, body = [], None, b""
    while True:
        try:
            ev = conn.next_event()
        except h11.RemoteProtocolError as e:
            # RFC 9112 §2.2: a server SHOULD ignore at least one empty line
            # before a request line; h11 refuses it, so the reference takes
            # the RFC's reading and goes on from the next byte.
            rest = conn.trailing_data[0]
            if cur is None and str(e).lower().startswith("no request line received"):
                if not rest:
                    return reqs, "ok"
                conn = h11.Connection(h11.SERVER)
                conn.receive_data(rest)
                continue
            # A lone CR may be the start of an empty line's CRLF; h11 refuses
            # it as an illegal request line before the LF can come.
            if cur is None and rest == b"\r":
                return reqs, "partial"
            return reqs, ("refuse", e.error_status_hint)
        if ev is h11.NEED_DATA:
            if cur is None and not conn.trailing_data[0]:
                return reqs, "ok"
            return reqs, "partial"
        if isinstance(ev, h11.Request):
            cur = (ev.method.decode("latin-1"), ev.target.decode("latin-1"),
                   [(k.decode("latin-1"), v.decode("latin-1")) for k, v in ev.headers.raw_items()])
            body = b""
        elif isinstance(ev, h11.Data):
            body += bytes(ev.data)
        elif isinstance(ev, h11.EndOfMessage):
            reqs.append((*cur, body))
            cur = None
            conn.send(h11.Response(status_code=200, headers=[("content-length", "0")]))
            conn.send(h11.EndOfMessage())
            if conn.our_state is h11.MUST_CLOSE:
                return reqs, "closed"
            conn.start_next_cycle()
        elif isinstance(ev, h11.ConnectionClosed):
            return reqs, "ok"


# --- the server under test -------------------------------------------------

def read_responses(sock, idle=1.5, total=8.0):
    """Every response the server sent until it closed or went quiet: (status,
    headers, body) each, and whether the connection closed."""
    buf, closed = b"", False
    sock.settimeout(idle)
    start = time.time()
    while time.time() - start < total:
        try:
            chunk = sock.recv(65536)
        except socket.timeout:
            break
        except ConnectionResetError:
            closed = True
            break
        if not chunk:
            closed = True
            break
        buf += chunk
    out = []
    while buf:
        head, sep, rest = buf.partition(b"\r\n\r\n")
        if not sep:
            out.append((-1, [], buf))
            break
        lines = head.split(b"\r\n")
        m = re.match(rb"HTTP/1\.[01] (\d{3})", lines[0])
        if not m:
            out.append((-1, [], buf))
            break
        hs = [tuple(x.strip() for x in l.split(b":", 1)) for l in lines[1:]]
        n = int(dict((k.lower(), v) for k, v in hs).get(b"content-length", b"0"))
        out.append((int(m.group(1)), hs, rest[:n]))
        buf = rest[n:]
    return out, closed


def echo_of(body: bytes):
    """The handler's echo back as (method, target, headers, body)."""
    parts = body.decode().split(" ")
    unhex = lambda s: bytes.fromhex(s).decode("latin-1")
    hs = [tuple(unhex(x) for x in p.split("=", 1)) for p in parts[2:-1]]
    return unhex(parts[0]), unhex(parts[1]), hs, bytes.fromhex(parts[-1][1:])


class Server:
    def __init__(self, lane, exe_dir):
        self.lane, self.exe_dir = lane, exe_dir
        self.src = Path(exe_dir) / "server.kara"
        self.src.write_text(SERVER)
        if lane == "build":
            exe = Path(exe_dir) / "server"
            b = subprocess.run([KARAC, "build", "-o", str(exe), str(self.src)], capture_output=True, text=True)
            if b.returncode != 0:
                sys.exit(f"build failed:\n{b.stdout}{b.stderr}")
            self.cmd = [str(exe)]
        else:
            self.cmd = [KARAC, "__mir-llvm-asan", str(self.src)]
        self.start()

    def start(self):
        self.err = open(Path(self.exe_dir) / f"stderr-{time.time_ns()}", "w+")
        self.proc = subprocess.Popen(self.cmd, env=dict(os.environ, KARAC_ANNOUNCE_PORT="1"),
                                     stdout=subprocess.DEVNULL, stderr=self.err, cwd=self.exe_dir)
        deadline = time.time() + 300
        while time.time() < deadline:
            self.err.seek(0)
            m = re.search(r"BOUND_PORT=(\d+)", self.err.read())
            if m:
                self.port = int(m.group(1))
                return
            if self.proc.poll() is not None:
                break
            time.sleep(0.1)
        self.err.seek(0)
        sys.exit(f"[{self.lane}] server did not start:\n{self.err.read()[-3000:]}")

    def stderr(self):
        self.err.seek(0)
        return self.err.read()

    def exchange(self, data: bytes):
        s = socket.create_connection(("127.0.0.1", self.port), timeout=5)
        try:
            s.sendall(data)
        except (BrokenPipeError, ConnectionResetError):
            pass
        try:
            return read_responses(s)
        finally:
            s.close()

    def alive(self):
        try:
            rs, _ = self.exchange(b"GET /health HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n")
            return bool(rs) and rs[0][0] == 200
        except OSError:
            return False

    def stop(self):
        """Ends the server through its handler's `exit(0)`: the exit status,
        and under ASan its leak report, are the run's last verdict."""
        try:
            self.exchange(b"GET /__quit HTTP/1.1\r\nHost: h\r\n\r\n")
        except OSError:
            pass
        try:
            return self.proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            return -9


# --- inputs ---------------------------------------------------------------

def req(method="GET", target="/", version="HTTP/1.1", headers=(("Host", "h"),), body=b"", eol=b"\r\n"):
    head = f"{method} {target} {version}".encode("latin-1") + eol
    for k, v in headers:
        head += (k.encode("latin-1") if isinstance(k, str) else k) + b": " + \
                (v.encode("latin-1") if isinstance(v, str) else v) + eol
    return head + eol + body


def chunked(parts, ext=b"", trailers=b"", eol=b"\r\n"):
    out = b""
    for p in parts:
        out += f"{len(p):x}".encode() + ext + eol + p + eol
    return out + b"0" + eol + trailers + eol


def fixed_cases():
    H = ("Host", "h")
    c = []
    c.append(("valid get", req()))
    c.append(("valid post", req("POST", "/p?q=1&r=%20", headers=(H, ("Content-Length", "5")), body=b"hello")))
    c.append(("bare LF head", req(eol=b"\n")))
    c.append(("lowercase method", req("get")))
    c.append(("token method", req("M-SEARCH")))
    c.append(("two spaces in line", b"GET  / HTTP/1.1\r\nHost: h\r\n\r\n"))
    c.append(("tab separator", b"GET\t/ HTTP/1.1\r\nHost: h\r\n\r\n"))
    c.append(("no version", b"GET /\r\nHost: h\r\n\r\n"))
    c.append(("trailing space", b"GET / HTTP/1.1 \r\nHost: h\r\n\r\n"))
    c.append(("HTTP/1.0 no host", req(version="HTTP/1.0", headers=())))
    c.append(("HTTP/1.2", req(version="HTTP/1.2")))
    c.append(("HTTP/2.0", req(version="HTTP/2.0")))
    c.append(("HTTP/10.1", req(version="HTTP/10.1")))
    c.append(("http/1.1 lowercase", req(version="http/1.1")))
    c.append(("absolute form", req(target="http://h/x?y=1")))
    c.append(("asterisk", req("OPTIONS", "*")))
    c.append(("space in target", b"GET /a b HTTP/1.1\r\nHost: h\r\n\r\n"))
    c.append(("high byte target", b"GET /\xc3\xa9 HTTP/1.1\r\nHost: h\r\n\r\n"))
    c.append(("control in target", b"GET /\x01 HTTP/1.1\r\nHost: h\r\n\r\n"))
    c.append(("empty method", b" / HTTP/1.1\r\nHost: h\r\n\r\n"))
    c.append(("missing host", req(headers=())))
    c.append(("two hosts", req(headers=(H, ("Host", "i")))))
    c.append(("no colon", req(headers=(H, ), body=b"").replace(b"Host: h\r\n", b"Host: h\r\nNoColon\r\n")))
    c.append(("space before colon", b"GET / HTTP/1.1\r\nHost: h\r\nX-A : 1\r\n\r\n"))
    c.append(("empty name", b"GET / HTTP/1.1\r\nHost: h\r\n: v\r\n\r\n"))
    c.append(("obs-fold", b"GET / HTTP/1.1\r\nHost: h\r\nX-A: 1\r\n 2\r\n\r\n"))
    c.append(("obs-fold tab", b"GET / HTTP/1.1\r\nHost: h\r\nX-A: 1\r\n\t2\r\n\r\n"))
    c.append(("fold first line", b"GET / HTTP/1.1\r\n Host: h\r\n\r\n"))
    c.append(("NUL in value", req(headers=(H, ("X", b"a\x00b")))))
    c.append(("DEL in value", req(headers=(H, ("X", b"a\x7fb")))))
    c.append(("tab in value", req(headers=(H, ("X", b"a\tb")))))
    c.append(("obs-text value", req(headers=(H, ("X", b"caf\xe9")))))
    c.append(("UTF-8 value", req(headers=(H, ("X", "café".encode())))))
    c.append(("obs-text target", b"GET /caf\xe9 HTTP/1.1\r\nHost: h\r\n\r\n"))
    c.append(("value OWS", req(headers=(H, ("X", "  padded \t")))))
    c.append(("empty value", req(headers=(H, ("X", "")))))
    c.append(("bad name char", req(headers=(H, ("X(y)", "1")))))
    c.append(("CL and TE", req("POST", headers=(H, ("Content-Length", "3"), ("Transfer-Encoding", "chunked")),
                               body=chunked([b"abc"]))))
    c.append(("two CL same", req("POST", headers=(H, ("Content-Length", "3"), ("Content-Length", "3")), body=b"abc")))
    c.append(("two CL differ", req("POST", headers=(H, ("Content-Length", "3"), ("Content-Length", "4")), body=b"abcd")))
    c.append(("CL list", req("POST", headers=(H, ("Content-Length", "3, 3")), body=b"abc")))
    c.append(("CL plus", req("POST", headers=(H, ("Content-Length", "+3")), body=b"abc")))
    c.append(("CL leading zero", req("POST", headers=(H, ("Content-Length", "003")), body=b"abc")))
    c.append(("CL hex", req("POST", headers=(H, ("Content-Length", "0x3")), body=b"abc")))
    c.append(("CL negative", req("POST", headers=(H, ("Content-Length", "-1")))))
    c.append(("CL huge", req("POST", headers=(H, ("Content-Length", "99999999999999999999999")))))
    c.append(("CL over 16 MiB", req("POST", headers=(H, ("Content-Length", str(16 * 1024 * 1024 + 1))))))
    c.append(("body over CL", req("POST", headers=(H, ("Content-Length", "3")), body=b"abcdef")))
    c.append(("body over CL then request", req("POST", headers=(H, ("Content-Length", "3")), body=b"abc") + req()))
    c.append(("chunked", req("POST", headers=(H, ("Transfer-Encoding", "chunked")), body=chunked([b"ab", b"cde"]))))
    c.append(("chunked upper hex", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                       body=b"A\r\n0123456789\r\n0\r\n\r\n")))
    c.append(("chunked ext", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                 body=chunked([b"ab"], ext=b";name=value"))))
    c.append(("chunked quoted ext", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                        body=chunked([b"ab"], ext=b';n="a b"'))))
    c.append(("chunked bad ext", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                     body=chunked([b"ab"], ext=b";="))))
    c.append(("chunked trailers", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                      body=chunked([b"ab"], trailers=b"X-Sum: 2\r\nX-T: z\r\n"))))
    c.append(("chunked bad trailer", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                         body=chunked([b"ab"], trailers=b"NoColon\r\n"))))
    c.append(("chunked bad hex", req("POST", headers=(H, ("Transfer-Encoding", "chunked")), body=b"zz\r\nab\r\n0\r\n\r\n")))
    c.append(("chunked space size", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                        body=b" 2\r\nab\r\n0\r\n\r\n")))
    c.append(("chunked size 0x", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                     body=b"0x2\r\nab\r\n0\r\n\r\n")))
    c.append(("chunked data long", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                       body=b"2\r\nabc\r\n0\r\n\r\n")))
    c.append(("chunked bare LF", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                     body=chunked([b"ab"], eol=b"\n"))))
    c.append(("chunked huge size", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                       body=b"ffffffffffffffffffff\r\nab\r\n0\r\n\r\n")))
    c.append(("TE gzip chunked", req("POST", headers=(H, ("Transfer-Encoding", "gzip, chunked")), body=chunked([b"x"]))))
    c.append(("TE chunked gzip", req("POST", headers=(H, ("Transfer-Encoding", "chunked, gzip")), body=chunked([b"x"]))))
    c.append(("TE identity", req("POST", headers=(H, ("Transfer-Encoding", "identity")), body=b"")))
    c.append(("TE Chunked case", req("POST", headers=(H, ("Transfer-Encoding", "Chunked")), body=chunked([b"x"]))))
    c.append(("TE twice", req("POST", headers=(H, ("Transfer-Encoding", "chunked"), ("Transfer-Encoding", "chunked")),
                              body=chunked([b"x"]))))
    c.append(("TE on 1.0", req("POST", version="HTTP/1.0", headers=(("Transfer-Encoding", "chunked"),),
                               body=chunked([b"x"]))))
    c.append(("pipelined 3", req(target="/1") + req("POST", "/2", headers=(H, ("Content-Length", "2")), body=b"ok")
              + req(target="/3")))
    c.append(("pipelined close mid", req(target="/1", headers=(H, ("Connection", "close"))) + req(target="/2")))
    c.append(("pipelined chunked", req("POST", "/1", headers=(H, ("Transfer-Encoding", "chunked")),
                                       body=chunked([b"a"])) + req(target="/2")))
    c.append(("pipelined then garbage", req(target="/1") + b"NOT HTTP\r\n\r\n"))
    c.append(("expect continue", req("POST", headers=(H, ("Expect", "100-continue"), ("Content-Length", "2")), body=b"hi")))
    c.append(("leading CRLF", b"\r\n" + req()))
    c.append(("leading bare LFs", b"\n\n" + req()))
    c.append(("leading space line", b" \r\n" + req()))
    c.append(("leading tab line", b"\t\r\n" + req()))
    c.append(("trailing lone CR", req() + b"\r"))
    c.append(("leading CR space line", b"\r \n" + req()))
    c.append(("chunked colon ext", req("POST", headers=(H, ("Transfer-Encoding", "chunked")),
                                       body=chunked([b"ab"], ext=b";nam: e=value"))))
    c.append(("long header line", req(headers=(H, ("X", "v" * 70000)))))
    c.append(("long target", req(target="/" + "a" * 70000)))
    return c


def mutate(rng, data: bytes) -> bytes:
    data = bytearray(data)
    for _ in range(rng.randrange(1, 4)):
        k = rng.randrange(6)
        i = rng.randrange(len(data) + 1)
        if k == 0 and data:
            del data[min(i, len(data) - 1)]
        elif k == 1:
            data[i:i] = bytes([rng.choice(b" \t\r\n:;,\x00\x7f\x80\xff0aA/=\"")])
        elif k == 2 and data:
            data[min(i, len(data) - 1)] = rng.randrange(256)
        elif k == 3:
            data[i:i] = rng.choice([b"\r\n", b"\n", b"\r", b" ", b": ", b"\r\n\r\n"])
        elif k == 4 and data:
            j = rng.randrange(len(data))
            data[i:i] = data[j:j + rng.randrange(1, 12)]
        else:
            data[i:i] = rng.choice([b"Content-Length: 1\r\n", b"Transfer-Encoding: chunked\r\n", b"Host: x\r\n",
                                    b"Connection: close\r\n"])
    return bytes(data)


# --- the verdict ----------------------------------------------------------

TCHAR = rb"[!#$%&'*+.^_`|~0-9A-Za-z-]"
CHUNK_EXT = re.compile(rb"(?:[ \t]*;[ \t]*" + TCHAR + rb"+(?:[ \t]*=[ \t]*(?:" + TCHAR
                       + rb'+|"(?:[\t !#-\[\]-~\x80-\xff]|\\[\t -~\x80-\xff])*"))?)*')


def bad_chunk_ext(body: bytes) -> bool:
    """Whether a chunk-size line in `body` has extensions outside RFC 9112
    §7.1.1: `chunk-ext = *( BWS ";" BWS name [ BWS "=" BWS value ] )`."""
    for m in re.finditer(rb"(?m)^([0-9a-fA-F]+)([^\r\n]*)\r?$", body):
        if m.group(2) and not CHUNK_EXT.fullmatch(m.group(2)):
            return True
    return False


def allowed(label, data, ref, got_status):
    """Why a difference is a choice the page or RFC 9112 leaves the server,
    or None. `ref` is h11's verdict, `got_status` the server's status for
    the request where they part."""
    head = data.split(b"\r\n\r\n", 1)[0]
    if got_status == 200 and isinstance(ref, tuple) and re.match(rb"[0-9a-fA-F]+[ \t]+;", data.split(b"\r\n\r\n", 1)[-1]):
        return "whitespace before a chunk extension's `;` accepted (RFC 9112 §7.1.1: BWS)"
    if got_status in REFUSALS:
        if re.search(rb"\r\n[^\r\n:]*:[^\r\n]*[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]", head):
            return "a control character in a field value refused (io.md; RFC 9110 §5.5 field-content)"
        if re.search(rb"(?i)\r\ncontent-length:.*\r\ntransfer-encoding:|\r\ntransfer-encoding:.*\r\ncontent-length:",
                     head, re.S):
            return "Content-Length with Transfer-Encoding refused (io.md; RFC 9112 §6.3 allows)"
        if b"chunked" in head.lower() and bad_chunk_ext(data.split(b"\r\n\r\n", 1)[-1]):
            return "a chunk extension outside RFC 9112 §7.1.1's grammar refused (`;=`, `;nam: e=value`)"
        if re.match(rb"(?:\r?\n)*[ \t\r]+\r?\n", data):
            return "a line of whitespace before the request line refused (io.md; RFC 9112 §2.2 skips only empty lines)"
        if re.search(rb"\n[ \t]", head.split(b"\n", 1)[-1] if b"\n" in head else b""):
            return "obs-fold refused with 400 (RFC 9112 §5.2: reject or unfold)"
        if got_status == 505:
            return "major version other than 1 answered 505 (io.md; RFC 9112 §2.3)"
        if got_status == 413:
            return "Content-Length over 16 MiB answered 413 before the body (io.md limits)"
        if got_status in (414, 431):
            return "request line or head over 64 KiB (io.md limits)"
        if b"\n" in data and b"\r\n" not in data.split(b"\r\n\r\n", 1)[-1] and b"chunked" in data.lower():
            return "chunked body with bare LF line ends refused (io.md)"
        if re.search(rb"\r\n(Content-Length:[^\r\n]*\r\n)(?:.*\r\n)?Content-Length:", head, re.S | re.I):
            return "two Content-Length headers refused even when equal (io.md; RFC 9112 §6.3 allows)"
        if re.search(rb"Content-Length: *[0-9]+ *,", head, re.I):
            return "a Content-Length list refused (io.md: plain decimal digits; RFC 9112 §6.3 allows)"
    return None


def seen_by_handler(want):
    """h11's request as the page says a handler sees it: `url` of an
    absolute-form target is its path and query (io.md: "for a request a
    server is answering its target, the path and query"), and header names
    compare without case. h11 lowercases a `Transfer-Encoding` value, which
    the server passes as sent, so that one value compares without case."""
    m, t, hs, b = want
    a = re.match(r"[A-Za-z][A-Za-z0-9+.-]*://[^/?#]*([^#]*)", t)
    if a:
        t = a.group(1) or "/"
        if t.startswith("?"):
            t = "/" + t
    # A value's obs-text reaches the handler decoded, each invalid UTF-8
    # sequence U+FFFD (io.md, B-2026-11-26-94); the echo is its UTF-8.
    lossy = lambda v: v.encode("latin-1").decode("utf-8", "replace").encode("utf-8").decode("latin-1")
    return m, t, [(k.lower(), lossy(v.lower() if k.lower() == "transfer-encoding" else v)) for k, v in hs], b


# Differences filed in docs/bugs-v2.jsonl and still open: reported as known,
# not failed, so the instrument stays usable while the row is open. A fix
# flips the row's cases to "same" and the entry is removed here (as
# B-2026-11-26-94's, obs-text in a header value, was).
KNOWN = {}


def judge(label, data, server, rep):
    ref_reqs, verdict = reference(data)
    rs, closed = server.exchange(data)
    interim = [r for r in rs if 100 <= r[0] < 200]
    rs = [r for r in rs if not (100 <= r[0] < 200)]
    show = f"{label}: {data[:120]!r}"
    # The requests h11 accepted must be the server's first answers, echoed.
    n_ok = 0
    for want, got in zip(ref_reqs, rs):
        if got[0] != 200:
            break
        try:
            e = echo_of(got[2])
        except Exception as x:  # an echo that does not parse is a server fault
            rep["fail"].append(f"{show}: echo unreadable ({x}): {got[2][:200]!r}")
            return
        wm, wt, wh, wb = seen_by_handler(want)
        eh = [(k.lower(), v.lower() if k.lower() == "transfer-encoding" else v) for k, v in e[2]]
        if (e[0], e[1], eh, e[3]) != (wm, wt, wh, wb):
            rep["fail"].append(f"{show}: request {n_ok} read differently:\n    h11:    {want}\n    server: {e}")
            return
        n_ok += 1
    if n_ok < len(ref_reqs):
        st = rs[n_ok][0] if n_ok < len(rs) else None
        why = allowed(label, data, verdict, st)
        if why:
            rep["allowed"].setdefault(why, []).append(label)
            return
        for row, (pattern, what) in KNOWN.items():
            if re.search(pattern, data.split(b"\r\n\r\n", 1)[0]):
                rep["known"].setdefault(f"{row}: {what}", []).append(label)
                return
        rep["fail"].append(f"{show}: h11 accepted request {n_ok}, server answered {st} "
                           f"({len(rs)} answers)")
        return
    extra = rs[n_ok:]
    if isinstance(verdict, tuple):
        if not extra:
            rep["fail"].append(f"{show}: h11 refuses ({verdict[1]}), server answered nothing after {n_ok}")
        elif extra[0][0] not in REFUSALS and allowed(label, data, verdict, extra[0][0]):
            rep["allowed"].setdefault(allowed(label, data, verdict, extra[0][0]), []).append(label)
        elif extra[0][0] not in REFUSALS:
            rep["fail"].append(f"{show}: h11 refuses ({verdict[1]}), server answered {extra[0][0]}")
        elif len(extra) > 1 or not closed:
            rep["fail"].append(f"{show}: refusal {extra[0][0]} did not end the connection")
        else:
            rep["same"] += 1
            if extra[0][0] != verdict[1]:
                rep["codes"].setdefault((verdict[1], extra[0][0]), []).append(label)
        return
    if verdict == "closed" or verdict == "ok":
        if extra:
            why = allowed(label, data, verdict, extra[0][0])
            if why:
                rep["allowed"].setdefault(why, []).append(label)
                return
            rep["fail"].append(f"{show}: h11 accepts all {n_ok}, server then answered {extra[0][0]}")
            return
        rep["same"] += 1
        return
    # "partial": h11 waits for more; the server must not have answered more.
    if extra and extra[0][0] not in REFUSALS:
        rep["fail"].append(f"{show}: h11 waits for more bytes, server answered {extra[0][0]}")
        return
    if extra:
        why = allowed(label, data, verdict, extra[0][0]) or "server refused a request h11 still waits on"
        rep["allowed"].setdefault(why, []).append(label)
        return
    rep["same"] += 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=300)
    ap.add_argument("--lanes", default="build,asan")
    args = ap.parse_args()
    if h11.__version__ != H11_VERSION:
        sys.exit(f"h11 {h11.__version__} is installed; this reference is pinned to {H11_VERSION}")
    rng = random.Random(args.seed)
    base = fixed_cases()
    cases = list(base)
    for i in range(args.cases):
        label, data = rng.choice(base)
        cases.append((f"mut{i}<{label}>", mutate(rng, data)))
    failed = False
    for lane in args.lanes.split(","):
        rep = {"same": 0, "fail": [], "allowed": {}, "codes": {}, "known": {}}
        with tempfile.TemporaryDirectory(prefix="kara-httpfuzz-") as d:
            server = Server(lane, d)
            for label, data in cases:
                judge(label, data, server, rep)
                if not server.alive():
                    rep["fail"].append(f"{label}: CRASH, the server stopped answering after {data[:120]!r}:\n"
                                       f"{server.stderr()[-2000:]}")
                    server.start()
            code = server.stop()
            err = server.stderr()
            if code != 0 or "ERROR: AddressSanitizer" in err or "ERROR: LeakSanitizer" in err:
                rep["fail"].append(f"server ended with {code}:\n{err[-4000:]}")
        for f in rep["fail"][:40]:
            print(f"FAIL [{lane}] {f}")
        for why, labels in sorted(rep["allowed"].items()):
            print(f"allowed [{lane}] {len(labels)}x {why}: {', '.join(labels[:6])}")
        for why, labels in sorted(rep["known"].items()):
            print(f"known [{lane}] {len(labels)}x {why}: {', '.join(labels[:6])}")
        for (want, got), labels in sorted(rep["codes"].items()):
            print(f"code [{lane}] h11 {want}, server {got}: {len(labels)}x {', '.join(labels[:6])}")
        allowed_n = sum(len(v) for v in rep["allowed"].values())
        print(f"http_server_fuzz [{lane}]: {rep['same']} same, {allowed_n} allowed, {len(rep['fail'])} failed "
              f"of {len(cases)}")
        failed |= bool(rep["fail"])
        if rep["same"] == 0:
            sys.exit(2)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
