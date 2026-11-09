"""Mirror of tus_upload.kara: a tus 1.0.0 resumable-upload server
(core protocol plus the creation, termination and checksum extensions)
and a client that drives it over a real loopback socket."""

import base64
import hashlib
import os
import shutil
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ADDR = ("127.0.0.1", 47748)
BASE = "http://127.0.0.1:47748"
ROOT = "tus_data"
MAX_SIZE = 1048576
EXTENSIONS = "creation,termination,checksum"
ALGORITHMS = "sha1,sha256"


def parse_length(text):
    if text is None or text == "" or not all("0" <= c <= "9" for c in text) or len(text) > 15:
        return None
    return int(text)


def parse_metadata(text):
    """The pairs of an `Upload-Metadata` header, or None when it is malformed."""
    pairs = []
    if text.strip() == "":
        return pairs
    for item in text.split(","):
        parts = item.strip().split(" ")
        key = parts[0]
        if key == "" or len(parts) > 2 or any(k == key for k, _ in pairs):
            return None
        if len(parts) == 2:
            try:
                raw = base64.b64decode(parts[1], validate=True)
                if base64.b64encode(raw).decode() != parts[1]:
                    return None
                value = raw.decode("utf-8")
            except (ValueError, UnicodeDecodeError):
                return None
        else:
            value = ""
        pairs.append((key, value))
    return pairs


def upload_id(path):
    if not path.startswith("/files/"):
        return None
    rest = path[len("/files/"):]
    if rest == "" or not all(c.isascii() and (c.isalnum() or c == "-") for c in rest):
        return None
    return rest


def read_info(uid):
    try:
        with open(f"{ROOT}/{uid}.info", encoding="utf-8") as f:
            lines = f.read().split("\n")
    except OSError:
        return None
    length = int(lines[0][len("length="):])
    meta = lines[1][len("metadata="):]
    return length, meta


def offset_of(uid):
    return os.path.getsize(f"{ROOT}/{uid}.bin")


def next_id():
    path = f"{ROOT}/next"
    n = int(open(path, encoding="utf-8").read()) if os.path.exists(path) else 1
    with open(path, "w", encoding="utf-8") as f:
        f.write(str(n + 1))
    return f"u{n:04d}"


def check_sum(header, body):
    """None when the checksum matches, else the status and the reason."""
    parts = header.split(" ")
    if len(parts) != 2:
        return 400, "malformed Upload-Checksum"
    algo, want = parts
    if algo == "sha1":
        got = base64.b64encode(hashlib.sha1(body).digest()).decode()
    elif algo == "sha256":
        got = base64.b64encode(hashlib.sha256(body).digest()).decode()
    else:
        return 400, f"unsupported checksum algorithm {algo}"
    if got != want:
        return 460, "checksum mismatch"
    return None


def handle(method, path, headers, body):
    """The status, body and headers answering one request."""
    if method == "OPTIONS":
        return 204, "", [
            ("Tus-Version", "1.0.0"),
            ("Tus-Extension", EXTENSIONS),
            ("Tus-Max-Size", str(MAX_SIZE)),
            ("Tus-Checksum-Algorithm", ALGORITHMS),
        ]
    if headers.get("tus-resumable") != "1.0.0":
        return 412, "unsupported tus version", [("Tus-Version", "1.0.0")]
    if path == "/files/" or path == "/files":
        if method != "POST":
            return 405, "method not allowed", [("Allow", "POST, OPTIONS")]
        length = parse_length(headers.get("upload-length"))
        if length is None:
            return 400, "missing or invalid Upload-Length", []
        if length > MAX_SIZE:
            return 413, f"upload of {length} bytes exceeds {MAX_SIZE}", []
        raw_meta = headers.get("upload-metadata", "")
        if parse_metadata(raw_meta) is None:
            return 400, "malformed Upload-Metadata", []
        uid = next_id()
        with open(f"{ROOT}/{uid}.info", "w", encoding="utf-8") as f:
            f.write(f"length={length}\nmetadata={raw_meta}\n")
        with open(f"{ROOT}/{uid}.bin", "w", encoding="utf-8") as f:
            f.write("")
        return 201, "", [("Location", f"/files/{uid}")]
    uid = upload_id(path)
    if uid is None:
        return 404, "not found", []
    info = read_info(uid)
    if method not in ("HEAD", "PATCH", "DELETE"):
        return 405, "method not allowed", [("Allow", "HEAD, PATCH, DELETE, OPTIONS")]
    if info is None:
        return 404, f"no upload {uid}", []
    length, meta = info
    if method == "HEAD":
        hs = [
            ("Upload-Offset", str(offset_of(uid))),
            ("Upload-Length", str(length)),
            ("Cache-Control", "no-store"),
        ]
        if meta != "":
            hs.append(("Upload-Metadata", meta))
        return 200, "", hs
    if method == "DELETE":
        os.remove(f"{ROOT}/{uid}.info")
        os.remove(f"{ROOT}/{uid}.bin")
        return 204, "", []
    if headers.get("content-type") != "application/offset+octet-stream":
        return 415, "PATCH needs Content-Type: application/offset+octet-stream", []
    claimed = parse_length(headers.get("upload-offset"))
    if claimed is None:
        return 400, "missing or invalid Upload-Offset", []
    have = offset_of(uid)
    if claimed != have:
        return 409, f"offset {claimed} does not match {have}", [("Upload-Offset", str(have))]
    if have + len(body) > length:
        return 400, f"{have} + {len(body)} bytes exceeds Upload-Length {length}", []
    sum_header = headers.get("upload-checksum")
    if sum_header is not None:
        bad = check_sum(sum_header, body)
        if bad is not None:
            return bad[0], bad[1], []
    with open(f"{ROOT}/{uid}.bin", "ab") as f:
        f.write(body)
    return 204, "", [("Upload-Offset", str(have + len(body)))]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def answer(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n > 0 else b""
        headers = {k.lower(): v for k, v in self.headers.items()}
        status, text, extra = handle(self.command, self.path, headers, body)
        payload = text.encode()
        self.send_response(status)
        self.send_header("Tus-Resumable", "1.0.0")
        for k, v in extra:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(payload)

    do_OPTIONS = do_POST = do_HEAD = do_PATCH = do_DELETE = do_GET = do_PUT = answer

    def log_message(self, *args):
        pass


# The client.

SHOWN = [
    "tus-resumable", "tus-version", "tus-extension", "tus-max-size",
    "tus-checksum-algorithm", "location", "upload-offset", "upload-length",
    "upload-metadata", "cache-control", "allow",
]


def send(method, path, headers, body=""):
    """One request: the status and the shown headers, or the client's error."""
    url = BASE + path
    req = urllib.request.Request(url, data=body.encode() if body != "" else None, method=method)
    for k, v in headers:
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            hs = sorted(
                f"{k.lower()}={v}" for k, v in resp.headers.items() if k.lower() in SHOWN
            )
            return True, resp.status, hs, resp.headers.get("Upload-Offset")
    except urllib.error.HTTPError as e:
        return False, e.code, [f"{url}: status code {e.code}"], None


def show(label, result):
    ok, status, hs, _ = result
    if ok:
        print(f"{label}: {status} {' '.join(hs)}")
    else:
        print(f"{label}: error {hs[0]}")


TUS = [("Tus-Resumable", "1.0.0")]


def payload(seed, rows):
    hosts = ["edge-1", "edge-2", "api-3", "db-4"]
    levels = ["INFO", "INFO", "INFO", "WARN", "ERROR"]
    lines = ["ts,host,level,latency_ms"]
    for i in range(rows):
        seed = (seed * 1103515245 + 12345) % 2147483648
        host = hosts[seed % 4]
        level = levels[(seed // 7) % 5]
        latency = (seed // 13) % 900 + 3
        lines.append(f"2026-11-09T10:{i // 60:02d}:{i % 60:02d}Z,{host},{level},{latency}")
    return "\n".join(lines) + "\n"


def b64(text):
    return base64.b64encode(text.encode()).decode()


def digest(algo, text):
    h = hashlib.sha1 if algo == "sha1" else hashlib.sha256
    return f"{algo} {base64.b64encode(h(text.encode()).digest()).decode()}"


def patch(uid, offset, chunk, extra):
    hs = TUS + [
        ("Upload-Offset", str(offset)),
        ("Content-Type", "application/offset+octet-stream"),
    ] + extra
    return send("PATCH", f"/files/{uid}", hs, chunk)


def upload_rest(label, uid, data, chunk, algo):
    """Uploads `data` from the server's offset in chunks of `chunk` bytes."""
    _, _, _, off = send("HEAD", f"/files/{uid}", TUS)
    offset = int(off)
    while offset < len(data):
        end = min(offset + chunk, len(data))
        piece = data[offset:end]
        r = patch(uid, offset, piece, [("Upload-Checksum", digest(algo, piece))])
        show(f"{label} patch {offset}..{end}", r)
        if not r[0]:
            break
        offset = int(r[3])
    return offset


def main():
    shutil.rmtree(ROOT, ignore_errors=True)
    os.makedirs(ROOT)
    server = ThreadingHTTPServer(ADDR, Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    up = False
    for _ in range(100):
        if send("OPTIONS", "/files/", [])[0]:
            up = True
            break
        time.sleep(0.02)
    print(f"server up: {str(up).lower()}")
    show("options", send("OPTIONS", "/files/", []))

    a = payload(7, 90)
    b = payload(11, 60)
    print(f"payload a: {len(a)} bytes, payload b: {len(b)} bytes")

    show("create without Tus-Resumable", send("POST", "/files/", [("Upload-Length", "10")]))
    show("create too large", send("POST", "/files/", TUS + [("Upload-Length", "2097152")]))
    show("create bad length", send("POST", "/files/", TUS + [("Upload-Length", "12x")]))
    show("create no length", send("POST", "/files/", TUS))
    show("create bad metadata", send("POST", "/files/", TUS + [("Upload-Length", "5"), ("Upload-Metadata", "filename not*base64")]))
    show("create duplicate key", send("POST", "/files/", TUS + [("Upload-Length", "5"), ("Upload-Metadata", f"k {b64('x')},k {b64('y')}")]))

    meta_a = f"filename {b64('edge-latency.csv')},filetype {b64('text/csv')},is_confidential"
    r = send("POST", "/files/", TUS + [("Upload-Length", str(len(a))), ("Upload-Metadata", meta_a)])
    show("create a", r)
    loc_a = next(h for h in r[2] if h.startswith("location="))[len("location="):]
    uid_a = loc_a[len("/files/"):]
    show("head a", send("HEAD", loc_a, TUS))

    first = a[:1000]
    show("a patch 0..1000", patch(uid_a, 0, first, [("Upload-Checksum", digest("sha1", first))]))
    show("a stale offset", patch(uid_a, 0, a[:10], []))
    show("a wrong content type", send("PATCH", loc_a, TUS + [("Upload-Offset", "1000"), ("Content-Type", "text/plain")], a[1000:1100]))
    show("a bad checksum", patch(uid_a, 1000, a[1000:1500], [("Upload-Checksum", digest("sha1", a[1000:1499] + "!"))]))
    show("a unknown algorithm", patch(uid_a, 1000, a[1000:1500], [("Upload-Checksum", "md5 AAAA")]))
    show("a past the end", patch(uid_a, 1000, a[1000:] + "overflow", []))
    show("a bad offset header", patch(uid_a, -1, a[1000:1010], []))
    show("a get", send("GET", loc_a, TUS))
    show("a head after errors", send("HEAD", loc_a, TUS))
    end = upload_rest("a", uid_a, a, 1000, "sha1")
    print(f"a uploaded to {end} of {len(a)}")
    show("head a done", send("HEAD", loc_a, TUS))
    show("a patch when complete", patch(uid_a, len(a), "x", []))

    r = send("POST", "/files", TUS + [("Upload-Length", str(len(b)))])
    show("create b", r)
    loc_b = next(h for h in r[2] if h.startswith("location="))[len("location="):]
    uid_b = loc_b[len("/files/"):]
    end = upload_rest("b", uid_b, b, 700, "sha256")
    print(f"b uploaded to {end} of {len(b)}")

    r = send("POST", "/files/", TUS + [("Upload-Length", "0"), ("Upload-Metadata", f"filename {b64('empty.txt')}")])
    show("create empty", r)
    loc_e = next(h for h in r[2] if h.startswith("location="))[len("location="):]
    show("head empty", send("HEAD", loc_e, TUS))

    with open(f"{ROOT}/{uid_a}.bin", encoding="utf-8") as f:
        on_disk = f.read()
    print(f"a on disk: {len(on_disk)} bytes, sha256 {hashlib.sha256(on_disk.encode()).hexdigest()[:16]}, matches {str(on_disk == a).lower()}")
    print(f"a lines: {on_disk.count(chr(10))}, last: {on_disk.split(chr(10))[-2]}")

    show("delete b", send("DELETE", loc_b, TUS))
    show("head b", send("HEAD", loc_b, TUS))
    show("patch b", patch(uid_b, 0, "x", []))
    show("delete b again", send("DELETE", loc_b, TUS))
    show("head bad id", send("HEAD", "/files/..%2Fnext", TUS))
    show("put a", send("PUT", loc_a, TUS))

    for name in sorted(os.listdir(ROOT)):
        print(f"  {name} {os.path.getsize(f'{ROOT}/{name}')}")
    server.shutdown()
    shutil.rmtree(ROOT, ignore_errors=True)
    print("done")


main()
