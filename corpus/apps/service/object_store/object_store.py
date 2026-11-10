"""Oracle for object_store.kara: the same object store over real sockets.

The server is an `http.server` server and the client `http.client`, with
the Kara client's rule that a status of 400 or more is an error carrying
only "<url>: status code <n>" (B-2026-11-09-44); 2xx and 3xx (a 304
included) come back as responses. The self-test calls the handler directly
with request values. Objects persist to a data directory as the Kara
program writes it.
"""

import hashlib
import http.client
import http.server
import os
import shutil
import socket
import sys
import threading
import time
import urllib.parse

ADDR = ("127.0.0.1", 47755)
MIN_PART = 5
MAX_KEYS = 1000


class Response:
    def __init__(self, status, body, headers):
        self.status = status
        self.body = body
        self.headers = list(headers)


class HttpError(Exception):
    pass


def header_of(headers, name):
    for k, v in headers:
        if k.lower() == name.lower():
            return v
    return None


def digest_hex(data):
    return hashlib.sha256(data.encode()).hexdigest()


def etag_of(body):
    return '"' + digest_hex(body)[:32] + '"'


def parse_int(s):
    """Kara's `i64.parse`: surrounding whitespace is ignored."""
    t = s.strip()
    digits = t[1:] if t[:1] in ("-", "+") else t
    if not digits or not digits.isdigit() or not digits.isascii():
        return None
    return int(t)


class Obj:
    def __init__(self, key, body, etag, content_type, meta):
        self.key = key
        self.body = body
        self.etag = etag
        self.content_type = content_type
        self.meta = list(meta)


class Upload:
    def __init__(self, key):
        self.key = key
        self.parts = {}


class Store:
    def __init__(self, dir_):
        self.objects = {}
        self.uploads = {}
        self.next_upload = 0
        self.dir = dir_
        self.events = []
        self.lock = threading.Lock()


# ------------------------------------------------------------ persistence

def object_file(dir_, key):
    return f"{dir_}/objects/{digest_hex(key)[:16]}"


def escape_field(s):
    return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")


def unescape_field(s):
    out = []
    esc = False
    for c in s:
        if esc:
            out.append({"t": "\t", "n": "\n"}.get(c, c))
            esc = False
        elif c == "\\":
            esc = True
        else:
            out.append(c)
    return "".join(out)


def index_text(objects):
    out = []
    for key in sorted(objects):
        o = objects[key]
        meta = ";".join(f"{k}={v}" for k, v in o.meta)
        out.append(f"{escape_field(key)}\t{o.etag}\t{o.content_type}\t{escape_field(meta)}\n")
    return "".join(out)


def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


def read_text(path):
    with open(path, encoding="utf-8", newline="") as f:
        return f.read()


def persist(st, key):
    if st.dir is None:
        return None
    try:
        os.makedirs(f"{st.dir}/objects", exist_ok=True)
        o = st.objects.get(key)
        path = object_file(st.dir, key)
        if o is not None:
            write_text(path, o.body)
        elif os.path.exists(path):
            os.remove(path)
        tmp = f"{st.dir}/index.tsv.tmp"
        write_text(tmp, index_text(st.objects))
        os.replace(tmp, f"{st.dir}/index.tsv")
    except OSError as e:
        return str(e)
    return None


def load(dir_):
    store = Store(dir_)
    problems = []
    index = f"{dir_}/index.tsv"
    if not os.path.exists(index):
        return store, problems
    text = read_text(index)
    for line in text.splitlines():
        fields = line.split("\t")
        if len(fields) != 4:
            problems.append(f"bad index line: {line}")
            continue
        key = unescape_field(fields[0])
        try:
            body = read_text(object_file(dir_, key))
        except FileNotFoundError:
            problems.append(f"{key}: not found")
            continue
        etag = fields[1]
        if "-" not in etag and etag_of(body) != etag:
            problems.append(f"{key}: content does not match its etag {etag}")
            continue
        meta = []
        for pair in unescape_field(fields[3]).split(";"):
            if "=" in pair:
                k, v = pair.split("=", 1)
                meta.append((k, v))
        store.objects[key] = Obj(key, body, etag, fields[2], meta)
    return store, problems


# ----------------------------------------------------------------- answers

def answer(status, body):
    return Response(status, body, [("Content-Type", "text/plain")])


def error(status, code, detail):
    return answer(status, f"{code}: {detail}")


def object_headers(o):
    h = [("ETag", o.etag), ("Content-Type", o.content_type), ("Accept-Ranges", "bytes")]
    for k, v in o.meta:
        h.append((f"x-meta-{k}", v))
    return h


# ------------------------------------------------------------------ ranges

WHOLE = ("whole",)
UNSAT = ("unsat",)


def parse_range(header, length):
    if header is None:
        return WHOLE
    h = header.strip()
    if not h.startswith("bytes="):
        return WHOLE
    spec = h[len("bytes="):].strip()
    if "," in spec:
        return WHOLE
    if "-" not in spec:
        return WHOLE
    a, b = spec.split("-", 1)
    if a == "":
        n = parse_int(b)
        if n is None:
            return WHOLE
        if n == 0 or length == 0:
            return UNSAT
        start = 0 if n > length else length - n
        return ("part", start, length - 1)
    start = parse_int(a)
    if start is None:
        return WHOLE
    if b == "":
        end = length - 1
    else:
        end = parse_int(b)
        if end is None:
            return WHOLE
    if b != "" and end < start:
        return WHOLE
    if start >= length:
        return UNSAT
    return ("part", start, min(end, length - 1))


# ------------------------------------------------------------ conditionals

def tag_list_matches(header, etag, weak):
    for t in header.split(","):
        t = t.strip()
        if t == "*":
            return True
        if weak and t.startswith("W/"):
            t = t[2:]
        if t == etag:
            return True
    return False


def preconditions(req, current, read):
    m = req.header("If-Match")
    if m is not None:
        if current is None or not tag_list_matches(m, current.etag, False):
            return error(412, "PreconditionFailed", "If-Match")
    m = req.header("If-None-Match")
    if m is not None and current is not None and tag_list_matches(m, current.etag, True):
        if read:
            r = answer(304, "")
            r.headers = [("ETag", current.etag)]
            return r
        return error(412, "PreconditionFailed", "If-None-Match")
    return None


# ----------------------------------------------------------------- requests

class Request:
    def __init__(self, method, url, headers=(), body=""):
        self._method = method
        self._url = url
        self._headers = list(headers)
        self._body = body

    def with_header(self, k, v):
        return Request(self._method, self._url, self._headers + [(k, v)], self._body)

    def with_body(self, b):
        return Request(self._method, self._url, self._headers, b)

    def method(self):
        return self._method

    def url(self):
        return self._url

    def path(self):
        return self._url.split("?", 1)[0]

    def query(self):
        if "?" not in self._url:
            return []
        out = []
        for seg in self._url.split("?", 1)[1].split("&"):
            if not seg:
                continue
            k, _, v = seg.partition("=")
            out.append((urllib.parse.unquote_plus(k), urllib.parse.unquote_plus(v)))
        return out

    def headers(self):
        return list(self._headers)

    def header(self, name):
        return header_of(self._headers, name)

    def body(self):
        return self._body


def query_param(req, name):
    for k, v in req.query():
        if k == name:
            return v
    return None


def has_param(req, name):
    return any(k == name for k, _ in req.query())


def percent_decode(text):
    out = bytearray()
    raw = text.encode()
    i = 0
    hexd = b"0123456789abcdefABCDEF"
    while i < len(raw):
        c = raw[i]
        if c == ord("%"):
            if i + 2 < len(raw) + 0 and raw[i + 1] in hexd and raw[i + 2] in hexd:
                out.append(int(raw[i + 1:i + 3], 16))
                i += 3
                continue
            return None
        out.append(c)
        i += 1
    try:
        return out.decode()
    except UnicodeDecodeError:
        return None


# ------------------------------------------------------------------ handler

def get_object(store, req, key):
    with store.lock:
        o = store.objects.get(key)
        if o is None:
            return error(404, "NoSuchKey", key)
        r = preconditions(req, o, True)
        if r is not None:
            return r
        length = len(o.body.encode())
        spec = parse_range(req.header("Range"), length)
        if spec == WHOLE:
            return Response(200, o.body, object_headers(o))
        if spec == UNSAT:
            r = error(416, "InvalidRange", f"bytes */{length}")
            r.headers.append(("Content-Range", f"bytes */{length}"))
            return r
        _, a, b = spec
        h = object_headers(o)
        h.append(("Content-Range", f"bytes {a}-{b}/{length}"))
        return Response(206, o.body.encode()[a:b + 1].decode(), h)


def put_object(store, req, key):
    with store.lock:
        r = preconditions(req, store.objects.get(key), False)
        if r is not None:
            return r
        body = req.body()
        meta = []
        for k, v in req.headers():
            low = k.lower()
            if low.startswith("x-meta-"):
                meta.append((low[len("x-meta-"):], v))
        meta.sort()
        ctype = req.header("Content-Type")
        o = Obj(key, body, etag_of(body), ctype if ctype is not None else "application/octet-stream", meta)
        store.objects[key] = o
        store.events.append(f"put {key} {o.etag}")
        err = persist(store, key)
        if err is not None:
            return error(500, "InternalError", err)
        r = answer(200, "")
        r.headers.append(("ETag", o.etag))
        return r


def delete_object(store, key):
    with store.lock:
        if store.objects.pop(key, None) is None:
            return error(404, "NoSuchKey", key)
        store.events.append(f"delete {key}")
        err = persist(store, key)
        if err is not None:
            return error(500, "InternalError", err)
        return answer(204, "")


def list_(store, req):
    prefix = query_param(req, "prefix") or ""
    delimiter = query_param(req, "delimiter") or ""
    start_after = query_param(req, "start-after") or ""
    s = query_param(req, "max-keys")
    if s is not None:
        n = parse_int(s)
        if n is None or n < 0:
            return error(400, "InvalidArgument", f"max-keys {s}")
        max_keys = min(n, MAX_KEYS)
    else:
        max_keys = MAX_KEYS
    with store.lock:
        lines = []
        prefixes = []
        count = 0
        last = ""
        truncated = False
        for key in sorted(store.objects, key=lambda k: k.encode()):
            o = store.objects[key]
            if not key.startswith(prefix) or key.encode() <= start_after.encode():
                continue
            rest = key[len(prefix):]
            entry = None
            if delimiter and delimiter in rest:
                i = rest.index(delimiter)
                entry = prefix + rest[:i + len(delimiter)]
            if entry is not None:
                if prefixes and prefixes[-1] == entry:
                    continue
                if count == max_keys:
                    truncated = True
                    break
                prefixes.append(entry)
                lines.append(f"prefix {entry}")
                last = entry
            else:
                if count == max_keys:
                    truncated = True
                    break
                lines.append(f"key {key} {len(o.body.encode())} {o.etag}")
                last = key
            count += 1
        if truncated:
            lines.append(f"truncated next={last}")
        return answer(200, "\n".join(lines))


# ---------------------------------------------------------------- multipart

def create_upload(store, key):
    with store.lock:
        store.next_upload += 1
        uid = f"u{store.next_upload}"
        store.uploads[uid] = Upload(key)
        store.events.append(f"upload {uid} {key}")
        return answer(200, uid)


def upload_part(store, req, key, uid):
    s = query_param(req, "partNumber")
    n = parse_int(s) if s is not None else None
    if n is None or not (1 <= n <= 10000):
        return error(400, "InvalidArgument", "partNumber")
    with store.lock:
        up = store.uploads.get(uid)
        if up is None or up.key != key:
            return error(404, "NoSuchUpload", uid)
        body = req.body()
        etag = etag_of(body)
        up.parts[n] = (etag, body)
        r = answer(200, "")
        r.headers.append(("ETag", etag))
        return r


def complete_upload(store, req, key, uid):
    with store.lock:
        up = store.uploads.get(uid)
        if up is None or up.key != key:
            return error(404, "NoSuchUpload", uid)
        wanted = []
        for line in req.body().splitlines():
            line = line.strip()
            if not line:
                continue
            if " " not in line:
                return error(400, "MalformedXML", line)
            a, b = line.split(" ", 1)
            n = parse_int(a)
            if n is None:
                return error(400, "MalformedXML", line)
            if wanted and n <= wanted[-1][0]:
                return error(400, "InvalidPartOrder", f"part {n}")
            wanted.append((n, b.strip()))
        if not wanted:
            return error(400, "MalformedXML", "no parts")
        body = []
        digests = []
        for i, (n, tag) in enumerate(wanted):
            p = up.parts.get(n)
            if p is None:
                return error(400, "InvalidPart", f"part {n} was not uploaded")
            etag, data = p
            if etag != tag:
                return error(400, "InvalidPart", f"part {n} etag {tag}")
            if i + 1 < len(wanted) and len(data.encode()) < MIN_PART:
                return error(400, "EntityTooSmall", f"part {n} is {len(data.encode())} bytes")
            body.append(data)
            digests.append(etag.strip('"'))
        etag = f'"{digest_hex("".join(digests))[:32]}-{len(wanted)}"'
        del store.uploads[uid]
        store.objects[key] = Obj(key, "".join(body), etag, "application/octet-stream", [])
        store.events.append(f"complete {uid} {key} {etag}")
        err = persist(store, key)
        if err is not None:
            return error(500, "InternalError", err)
        r = answer(200, etag)
        r.headers.append(("ETag", etag))
        return r


def abort_upload(store, key, uid):
    with store.lock:
        up = store.uploads.get(uid)
        if up is None or up.key != key:
            return error(404, "NoSuchUpload", uid)
        del store.uploads[uid]
        store.events.append(f"abort {uid}")
        return answer(204, "")


def handle(store, req):
    path = req.path()
    if path in ("/o", "/o/"):
        if req.method() == "GET":
            return list_(store, req)
        return error(405, "MethodNotAllowed", req.method())
    if not path.startswith("/o/"):
        return error(404, "NoSuchBucket", path)
    raw = path[len("/o/"):]
    key = percent_decode(raw)
    if key is None:
        return error(400, "InvalidURI", raw)
    if len(key.encode()) > 1024:
        return error(400, "KeyTooLongError", f"{len(key.encode())} bytes")
    uid = query_param(req, "uploadId")
    m = req.method()
    if m == "POST" and uid is None and has_param(req, "uploads"):
        return create_upload(store, key)
    if m == "PUT" and uid is not None:
        return upload_part(store, req, key, uid)
    if m == "POST" and uid is not None:
        return complete_upload(store, req, key, uid)
    if m == "DELETE" and uid is not None:
        return abort_upload(store, key, uid)
    if m == "GET" and uid is None:
        return get_object(store, req, key)
    if m == "PUT" and uid is None:
        return put_object(store, req, key)
    if m == "DELETE" and uid is None:
        return delete_object(store, key)
    return error(405, "MethodNotAllowed", m)


# ---------------------------------------------------------------- self-test

def show(store, req):
    line = f"{req.method()} {req.url()}"
    r = handle(store, req)
    shown = []
    for name in ("ETag", "Content-Range", "Content-Type"):
        v = header_of(r.headers, name)
        if v is not None:
            shown.append(f"{name}: {v}")
    for k, v in r.headers:
        if k.startswith("x-meta-"):
            shown.append(f"{k}: {v}")
    body = r.body.replace("\n", " | ")
    print(f"  {line} -> {r.status} [{', '.join(shown)}] {body}")


def req(method, url):
    return Request(method, url)


def self_test():
    print("== self-test")
    store = Store(None)
    show(store, req("PUT", "/o/notes/today.txt").with_header("Content-Type", "text/plain").with_header("X-Meta-Author", "ana").with_body("hello, object store"))
    show(store, req("GET", "/o/notes/today.txt"))
    for r in ("bytes=0-4", "bytes=7-", "bytes=-5", "bytes=-100", "bytes=10-400", "bytes=19-",
              "bytes=-0", "bytes=0-1,4-5", "lines=1-2", "bytes=5-2"):
        show(store, req("GET", "/o/notes/today.txt").with_header("Range", r))
    tag = etag_of("hello, object store")
    show(store, req("GET", "/o/notes/today.txt").with_header("If-None-Match", tag))
    show(store, req("GET", "/o/notes/today.txt").with_header("If-None-Match", f"W/{tag}"))
    show(store, req("GET", "/o/notes/today.txt").with_header("If-Match", '"stale"'))
    show(store, req("GET", "/o/notes/today.txt").with_header("If-Match", f'"x", {tag}'))
    show(store, req("PUT", "/o/notes/today.txt").with_header("If-None-Match", "*").with_body("again"))
    show(store, req("PUT", "/o/notes/today.txt").with_header("If-Match", '"stale"').with_body("lost update"))
    show(store, req("PUT", "/o/notes/today.txt").with_header("If-Match", tag).with_body("hello, version two"))
    show(store, req("PUT", "/o/notes/new.txt").with_header("If-Match", "*").with_body("nothing to match"))
    show(store, req("PUT", "/o/notes/new.txt").with_header("If-None-Match", "*").with_body("created once"))
    show(store, req("GET", "/o/notes/missing.txt"))
    show(store, req("PUT", "/o/a%20b%2Fc.txt").with_body("spaces and a slash"))
    show(store, req("GET", "/o/a%20b%2Fc.txt"))
    show(store, req("GET", "/o/bad%zz"))
    for k in ("photos/2024/jan/1.jpg", "photos/2024/feb/2.jpg", "photos/2025/mar/3.jpg", "photos/readme", "videos/x.mp4"):
        show(store, req("PUT", f"/o/{k}").with_body(k))
    show(store, req("GET", "/o/?prefix=photos/"))
    show(store, req("GET", "/o/?prefix=photos/&delimiter=/"))
    show(store, req("GET", "/o/?delimiter=/"))
    show(store, req("GET", "/o/?prefix=photos/&max-keys=2"))
    show(store, req("GET", "/o/?prefix=photos/&max-keys=2&start-after=photos/2024/jan/1.jpg"))
    show(store, req("GET", "/o/?max-keys=-1"))
    show(store, req("DELETE", "/o/videos/x.mp4"))
    show(store, req("DELETE", "/o/videos/x.mp4"))
    show(store, req("PATCH", "/o/notes/new.txt"))
    show(store, req("GET", "/elsewhere"))

    print("== self-test: multipart")
    show(store, req("POST", "/o/big.log?uploads"))
    show(store, req("PUT", "/o/big.log?uploadId=u1&partNumber=1").with_body("first-"))
    show(store, req("PUT", "/o/big.log?uploadId=u1&partNumber=2").with_body("second-"))
    show(store, req("PUT", "/o/big.log?uploadId=u1&partNumber=3").with_body("end"))
    show(store, req("PUT", "/o/big.log?uploadId=u1&partNumber=0").with_body("zero"))
    show(store, req("PUT", "/o/other.log?uploadId=u1&partNumber=1").with_body("wrong key"))
    p1, p2, p3 = etag_of("first-"), etag_of("second-"), etag_of("end")
    show(store, req("POST", "/o/big.log?uploadId=u1").with_body(f"2 {p2}\n1 {p1}\n"))
    show(store, req("POST", "/o/big.log?uploadId=u1").with_body(f'1 {p1}\n2 "nope"\n'))
    show(store, req("POST", "/o/big.log?uploadId=u1").with_body(f"1 {p1}\n4 {p3}\n"))
    show(store, req("POST", "/o/big.log?uploadId=u1").with_body(f"1 {p1}\n3 {p3}\n"))
    show(store, req("GET", "/o/big.log"))
    show(store, req("POST", "/o/big.log?uploadId=u1").with_body(f"1 {p1}\n"))
    show(store, req("POST", "/o/tiny.bin?uploads"))
    show(store, req("PUT", "/o/tiny.bin?uploadId=u2&partNumber=1").with_body("abc"))
    show(store, req("PUT", "/o/tiny.bin?uploadId=u2&partNumber=2").with_body("def"))
    t1, t2 = etag_of("abc"), etag_of("def")
    show(store, req("POST", "/o/tiny.bin?uploadId=u2").with_body(f"1 {t1}\n2 {t2}\n"))
    show(store, req("DELETE", "/o/tiny.bin?uploadId=u2"))
    show(store, req("DELETE", "/o/tiny.bin?uploadId=u2"))
    show(store, req("GET", "/o/tiny.bin"))
    print(f"  events: {len(store.events)}, objects: {len(store.objects)}, open uploads: {len(store.uploads)}")


# ------------------------------------------------------------------- live

def real_send(method, url, headers, body, timeout_ms):
    """The Kara client: a status of 400 or more is an error."""
    rest = url[len("http://"):]
    hostport, _, path = rest.partition("/")
    host, _, port = hostport.partition(":")
    conn = http.client.HTTPConnection(host, int(port), timeout=timeout_ms / 1000)
    try:
        conn.putrequest(method, "/" + path, skip_accept_encoding=True)
        for k, v in headers:
            conn.putheader(k, v)
        data = body.encode() if body else None
        if data is not None:
            conn.putheader("Content-Length", str(len(data)))
        conn.endheaders(data)
        resp = conn.getresponse()
        text = resp.read().decode()
        status = resp.status
        hs = list(resp.getheaders())
    except OSError as e:
        raise HttpError(f"{url}: {e}")
    finally:
        conn.close()
    if status >= 400:
        raise HttpError(f"{url}: status code {status}")
    return Response(status, text, hs)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def answer(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n).decode() if n else ""
        r = self.server.app(Request(self.command, self.path, list(self.headers.items()), body))
        data = r.body.encode()
        self.send_response_only(r.status)
        has_type = False
        for k, v in r.headers:
            if k.lower() in ("content-length", "connection", "transfer-encoding"):
                continue
            if k.lower() == "content-type":
                has_type = True
            self.send_header(k, v)
        if not has_type:
            self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_POST = do_PUT = do_DELETE = answer


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def call(method, path, headers, body):
    shown = "".join(f" {k}: {v}" for k, v in headers)
    try:
        r = real_send(method, f"http://{ADDR[0]}:{ADDR[1]}{path}", headers, body, 3000)
        tag = header_of(r.headers, "ETag") or "-"
        rng = header_of(r.headers, "Content-Range") or "-"
        text = r.body.replace("\n", " | ")
        print(f"  {method} {path}{shown} -> {r.status} {tag} {rng} {text}")
    except HttpError as e:
        print(f"  {method} {path}{shown} -> error: {e}")


def dump(store):
    for key in sorted(store.objects):
        o = store.objects[key]
        print(f"  {key} {len(o.body.encode())} {o.etag} {o.content_type}")


def main():
    self_test()

    dir_ = "objstore_data"
    shutil.rmtree(dir_, ignore_errors=True)
    store = Store(dir_)
    srv = Server(ADDR, Handler)
    srv.app = lambda r: handle(store, r)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    up = False
    for _ in range(100):
        try:
            socket.create_connection(ADDR, timeout=1).close()
            up = True
            break
        except OSError:
            time.sleep(0.02)
    print(f"== live: server up {'true' if up else 'false'}")
    call("PUT", "/o/site/index.html", [("Content-Type", "text/html"), ("X-Meta-Owner", "web")], "<h1>home</h1>")
    call("PUT", "/o/site/about.html", [("Content-Type", "text/html")], "<h1>about</h1>")
    call("PUT", "/o/site/css/main.css", [("Content-Type", "text/css")], "body { margin: 0 }")
    call("PUT", "/o/logs/2025-01-01.log", [], "boot ok\nready\n")
    call("GET", "/o/site/index.html", [], "")
    call("GET", "/o/site/index.html", [("Range", "bytes=4-7")], "")
    call("GET", "/o/site/index.html", [("Range", "bytes=50-")], "")
    home = etag_of("<h1>home</h1>")
    call("GET", "/o/site/index.html", [("If-None-Match", home)], "")
    call("PUT", "/o/site/index.html", [("If-Match", '"old"')], "<h1>lost</h1>")
    call("PUT", "/o/site/index.html", [("If-Match", home), ("Content-Type", "text/html")], "<h1>home v2</h1>")
    call("GET", "/o/?prefix=site/&delimiter=/", [], "")
    call("GET", "/o/?max-keys=2", [], "")
    call("POST", "/o/backup.tar?uploads", [], "")
    call("PUT", "/o/backup.tar?uploadId=u1&partNumber=1", [], "aaaaaaaa")
    call("PUT", "/o/backup.tar?uploadId=u1&partNumber=2", [], "bbbb")
    b1, b2 = etag_of("aaaaaaaa"), etag_of("bbbb")
    call("POST", "/o/backup.tar?uploadId=u1", [], f"1 {b1}\n2 {b2}\n")
    call("GET", "/o/backup.tar", [("Range", "bytes=-6")], "")
    call("DELETE", "/o/logs/2025-01-01.log", [], "")
    call("GET", "/o/logs/2025-01-01.log", [], "")
    srv.shutdown()

    print("== live: on disk")
    files = sorted(os.listdir(f"{dir_}/objects"))
    print(f"  {len(files)} object files")
    for line in read_text(f"{dir_}/index.tsv").splitlines():
        print(f"  {line.replace(chr(9), ' ')}")

    print("== restart")
    s, problems = load(dir_)
    dump(s)
    print(f"  problems: {len(problems)}")

    print("== restart after damage")
    write_text(object_file(dir_, "site/about.html"), "<h1>tampered</h1>")
    os.remove(object_file(dir_, "site/css/main.css"))
    s, problems = load(dir_)
    dump(s)
    for p in problems:
        print(f"  problem: {p}")
    shutil.rmtree(dir_, ignore_errors=True)
    print("done")


if __name__ == "__main__":
    sys.exit(main())
