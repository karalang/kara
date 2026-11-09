"""Oracle for http_cache.kara: the same caching proxy over real sockets.

The origin and the cache are `http.server` servers; the cache reaches the
origin, and the client the cache, through `http.client`, with the Kara
client's rule that a status of 400 or more is an error carrying only
"<url>: status code <n>" (B-2026-11-09-44). The self-test's fake origin is
an object whose `send` the cache calls in place of the network, and its
responses come back as they are, whatever their status.
"""

import http.client
import http.server
import socket
import sys
import threading
import time

ORIGIN_ADDR = ("127.0.0.1", 47753)
CACHE_ADDR = ("127.0.0.1", 47754)
PURGE_TOKEN = "test-purge-token"


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


HOP_BY_HOP = ["connection", "keep-alive", "proxy-connection", "te", "trailer",
              "transfer-encoding", "upgrade", "content-length", "host"]


def end_to_end(name):
    return name.lower() not in HOP_BY_HOP


# --------------------------------------------------------- cache-control

class Directives:
    def __init__(self):
        self.max_age = None
        self.s_maxage = None
        self.no_store = False
        self.no_cache = False
        self.private = False
        self.public = False
        self.must_revalidate = False
        self.stale_if_error = 0


def unquote(s):
    if len(s) >= 2 and s.startswith('"') and s.endswith('"'):
        return s[1:-1]
    return s


def parse_int(s):
    # Kara's i64.parse: an optional sign and ASCII digits.
    t = s[1:] if s[:1] in "+-" else s
    if t == "" or not all("0" <= ch <= "9" for ch in t):
        return None
    return int(s)


def parse_cache_control(value):
    d = Directives()
    for part in value.split(","):
        part = part.strip()
        if part == "":
            continue
        if "=" in part:
            n, a = part.split("=", 1)
            name, arg = n.strip().lower(), unquote(a.strip())
        else:
            name, arg = part.lower(), ""
        if name == "max-age":
            d.max_age = parse_int(arg)
        elif name == "s-maxage":
            d.s_maxage = parse_int(arg)
        elif name == "no-store":
            d.no_store = True
        elif name == "no-cache":
            d.no_cache = True
        elif name == "private":
            d.private = True
        elif name == "public":
            d.public = True
        elif name in ("must-revalidate", "proxy-revalidate"):
            d.must_revalidate = True
        elif name == "stale-if-error":
            v = parse_int(arg)
            d.stale_if_error = 0 if v is None else v
    return d


def lifetime_of(d):
    return d.s_maxage if d.s_maxage is not None else d.max_age


# ----------------------------------------------------------------- store

class Entry:
    def __init__(self, **kw):
        self.__dict__.update(kw)

    def clone(self):
        e = Entry(**self.__dict__)
        e.headers = list(self.headers)
        e.vary = list(self.vary)
        return e

    def age(self, now_ms):
        # Kara's `/` truncates toward zero; times only grow here.
        return self.age_s + (now_ms - self.stored_ms) // 1000

    def fresh(self, now_ms):
        return not self.no_cache and self.age(now_ms) < self.lifetime_s

    def matches(self, req):
        return all((req.header(name) or "") == value for name, value in self.vary)


class Cache:
    def __init__(self, origin, budget, max_object, send):
        self.origin = origin
        self.budget = budget
        self.max_object = max_object
        self.start = time.monotonic()
        self.send = send
        self.lock = threading.Lock()
        self.entries = {}
        self.bytes = 0
        self.tick = 0
        self.events = []
        self.labels = {}

    def find(self, key, req):
        with self.lock:
            self.tick += 1
            for e in self.entries.get(key, []):
                if e.matches(req):
                    e.used = self.tick
                    return e.clone()
            return None

    def event(self, text):
        with self.lock:
            self.events.append(text)

    def put(self, key, e):
        with self.lock:
            self.tick += 1
            e = e.clone()
            e.used = self.tick
            size = len(e.body.encode())
            variants = self.entries.setdefault(key, [])
            for i, v in enumerate(variants):
                if v.vary == e.vary:
                    self.bytes -= len(v.body.encode())
                    del variants[i]
                    break
            variants.append(e)
            self.bytes += size
            while self.bytes > self.budget:
                oldest = None
                for k, vs in self.entries.items():
                    for i, v in enumerate(vs):
                        if oldest is None or v.used < oldest[2]:
                            oldest = (k, i, v.used)
                k, i, _ = oldest
                vs = self.entries[k]
                gone = vs.pop(i)
                self.bytes -= len(gone.body.encode())
                if not vs:
                    del self.entries[k]
                self.events.append(f"evict {k} ({len(gone.body.encode())} bytes)")

    def invalidate(self, key):
        with self.lock:
            vs = self.entries.pop(key, None)
            if vs is None:
                return 0
            for v in vs:
                self.bytes -= len(v.body.encode())
            return len(vs)

    def count(self, label):
        with self.lock:
            self.labels[label] = self.labels.get(label, 0) + 1


# -------------------------------------------------------------- handling

class Request:
    def __init__(self, method, url, headers=(), body=""):
        self._method = method
        self._url = url
        self._headers = list(headers)
        self._body = body

    def with_header(self, k, v):
        return Request(self._method, self._url, self._headers + [(k, v)], self._body)

    def method(self):
        return self._method

    def url(self):
        return self._url

    def path(self):
        return self._url.split("?", 1)[0]

    def headers(self):
        return list(self._headers)

    def header(self, name):
        return header_of(self._headers, name)

    def body(self):
        return self._body


def with_label(headers, label):
    out = [(k, v) for k, v in headers if end_to_end(k)]
    out.append(("X-Cache", label))
    return out


def reply(e, label, now_ms):
    headers = [(k, v) for k, v in e.headers if k.lower() != "age"]
    headers.append(("Age", str(e.age(now_ms))))
    return Response(e.status, e.body, with_label(headers, label))


def pass_(r, label):
    return Response(r.status, r.body, with_label(r.headers, label))


def answer(status, body, label):
    return Response(status, body, with_label([("Content-Type", "text/plain")], label))


def fetch(cache, req, key, extra):
    headers = []
    for k, v in req.headers():
        lower = k.lower()
        if end_to_end(lower) and lower != "if-none-match" and lower != "if-modified-since":
            headers.append((k, v))
    headers.extend(extra)
    body = req.body() if req.method() != "GET" else None
    try:
        return cache.send(req.method(), f"{cache.origin}{key}", headers, body, 1000), None
    except HttpError as e:
        return None, str(e)


STORABLE = [200, 203, 301, 404, 410]


def to_entry(cache, req, r, now_ms):
    cc = parse_cache_control(header_of(r.headers, "cache-control") or "")
    if r.status not in STORABLE:
        return None, f"status {r.status}"
    if cc.no_store:
        return None, "no-store"
    if cc.private:
        return None, "private"
    vary = header_of(r.headers, "vary") or ""
    if vary.strip() == "*":
        return None, "vary *"
    if req.header("Authorization") is not None and not cc.public and cc.s_maxage is None:
        return None, "authorization"
    has_validator = header_of(r.headers, "etag") is not None or header_of(r.headers, "last-modified") is not None
    lifetime = lifetime_of(cc)
    if lifetime is None:
        if has_validator:
            lifetime = 0
        else:
            return None, "no freshness or validator"
    if len(r.body.encode()) > cache.max_object:
        return None, f"too big ({len(r.body.encode())} bytes)"
    varied = []
    for name in vary.split(","):
        name = name.strip().lower()
        if name != "":
            varied.append((name, req.header(name) or ""))
    age = header_of(r.headers, "age")
    age = parse_int(age) if age is not None else None
    return Entry(
        status=r.status,
        body=r.body,
        headers=list(r.headers),
        vary=varied,
        stored_ms=now_ms,
        age_s=0 if age is None else age,
        lifetime_s=lifetime,
        no_cache=cc.no_cache,
        must_revalidate=cc.must_revalidate,
        stale_if_error_s=cc.stale_if_error,
        used=0,
    ), None


def store(cache, req, key, r, now_ms, label):
    e, why = to_entry(cache, req, r, now_ms)
    if e is not None:
        cache.put(key, e.clone())
        return reply(e, label, now_ms)
    cache.event(f"pass {key}: {why}")
    return pass_(r, "PASS")


def refreshed(e, r, now_ms):
    e = e.clone()
    for k, v in r.headers:
        if not end_to_end(k):
            continue
        for i, (n, _) in enumerate(e.headers):
            if n.lower() == k.lower():
                e.headers[i] = (k, v)
                break
        else:
            e.headers.append((k, v))
    cc = parse_cache_control(header_of(e.headers, "cache-control") or "")
    lt = lifetime_of(cc)
    e.lifetime_s = 0 if lt is None else lt
    e.no_cache = cc.no_cache
    e.must_revalidate = cc.must_revalidate
    e.stale_if_error_s = cc.stale_if_error
    e.stored_ms = now_ms
    e.age_s = 0
    return e


def on_error(cache, key, e, now_ms, failed):
    stale_for = e.age(now_ms) - e.lifetime_s
    if e.must_revalidate:
        cache.event(f"error {key}: must revalidate, origin said {failed.status}")
        return answer(504, "origin unavailable and the entry must be revalidated", "ERROR")
    if stale_for <= e.stale_if_error_s:
        return reply(e, "STALE", now_ms)
    cache.event(f"error {key}: stale for {stale_for}s, past stale-if-error")
    return pass_(failed, "ERROR")


def client_has_it(req, e):
    asked = req.header("If-None-Match")
    tag = header_of(e.headers, "etag")
    return asked is not None and tag is not None and asked == tag


def not_modified(e, label, now_ms):
    r = reply(e, label, now_ms)
    r.status = 304
    r.body = ""
    return r


def revalidate(cache, req, key, e, now_ms):
    extra = []
    tag = header_of(e.headers, "etag")
    if tag is not None:
        extra.append(("If-None-Match", tag))
    when = header_of(e.headers, "last-modified")
    if when is not None:
        extra.append(("If-Modified-Since", when))
    r, msg = fetch(cache, req, key, extra)
    if r is None:
        return on_error(cache, key, e, now_ms, answer(502, msg, "ERROR"))
    if r.status == 304:
        e = refreshed(e, r, now_ms)
        cache.put(key, e.clone())
        if client_has_it(req, e):
            return not_modified(e, "REVALIDATED", now_ms)
        return reply(e, "REVALIDATED", now_ms)
    if r.status >= 500:
        return on_error(cache, key, e, now_ms, r)
    cache.invalidate(key)
    return store(cache, req, key, r, now_ms, "UPDATED")


def serve_get(cache, req, key, now_ms):
    asked = parse_cache_control(req.header("Cache-Control") or "")
    if asked.no_store:
        r, msg = fetch(cache, req, key, [])
        return pass_(r, "BYPASS") if r is not None else answer(502, msg, "ERROR")
    e = cache.find(key, req)
    if e is not None and e.fresh(now_ms) and not asked.no_cache:
        if client_has_it(req, e):
            return not_modified(e, "HIT", now_ms)
        return reply(e, "HIT", now_ms)
    if e is not None:
        return revalidate(cache, req, key, e, now_ms)
    r, msg = fetch(cache, req, key, [])
    if r is None:
        return answer(502, msg, "ERROR")
    return store(cache, req, key, r, now_ms, "MISS")


def purge(cache, req, key):
    if req.header("X-Purge-Token") != PURGE_TOKEN:
        return answer(403, "purge needs the token", "PURGE")
    n = cache.invalidate(key)
    if n == 0:
        return answer(404, f"{key} is not cached", "PURGE")
    cache.event(f"purge {key} ({n} variants)")
    return answer(200, f"purged {n}", "PURGE")


def write_through(cache, req, key):
    r, msg = fetch(cache, req, key, [])
    if r is None:
        return answer(502, msg, "ERROR")
    if r.status < 400:
        n = cache.invalidate(key)
        if n > 0:
            cache.event(f"invalidate {key} after {req.method()} ({n} variants)")
    return pass_(r, "PASS")


def handle(cache, req, now_ms):
    key = req.url()
    m = req.method()
    if m == "GET":
        r = serve_get(cache, req, key, now_ms)
    elif m == "PURGE":
        r = purge(cache, req, key)
    else:
        r = write_through(cache, req, key)
    cache.count(header_of(r.headers, "x-cache") or "?")
    return r


# ---------------------------------------------------------------- report

def print_state(cache):
    with cache.lock:
        keys = sorted(cache.entries)
        print(f"  {len(keys)} keys, {cache.bytes} bytes")
        for k in keys:
            for v in cache.entries[k]:
                vary = ",".join(f"{n}={x}" for n, x in v.vary)
                print(f"  {k} {v.status} {len(v.body.encode())} bytes lifetime {v.lifetime_s}s {vary}")
        for e in cache.events:
            print(f"  event: {e}")
        labels = sorted(cache.labels.items())
        print(f"  labels: {', '.join(f'{k} {n}' for k, n in labels)}")


# ----------------------------------------------------------- fake origin

def page(status, body, headers):
    return Response(status, body, list(headers))


class FakeOrigin:
    def __init__(self):
        self.hits = {}
        self.seen = []

    def send(self, method, url, headers, body, timeout_ms):
        req = Request(method, url, headers, body or "")
        path = url[len("http://origin.test"):] if url.startswith("http://origin.test") else ""
        n = self.hits.get(path, 0) + 1
        self.hits[path] = n
        line = f"{method} {path}"
        for k, v in headers:
            line = f"{line} [{k}: {v}]"
        self.seen.append(line)
        inm = req.header("If-None-Match") or ""
        if path in ("/doc", "/doc2"):
            if inm == '"a"':
                return page(304, "", [("ETag", '"a"'), ("Cache-Control", "max-age=10")])
            return page(200, "the document", [("ETag", '"a"'), ("Cache-Control", "max-age=10")])
        if path == "/public":
            return page(200, "for everyone", [("Cache-Control", "public, max-age=10")])
        if path == "/aged":
            return page(200, f"aged {n}", [("Age", "7"), ("Cache-Control", "max-age=10")])
        if path == "/shared":
            return page(200, "shared", [("Cache-Control", "max-age=0, s-maxage=30")])
        if path == "/gone":
            return page(404, "no such page", [("Cache-Control", "max-age=5")])
        if path == "/flaky":
            if n == 1:
                return page(200, "flaky ok", [("Cache-Control", "max-age=2, stale-if-error=5")])
            return page(503, "unavailable", [])
        if path == "/strict":
            if n == 1:
                return page(200, "strict ok", [("Cache-Control", "max-age=2, must-revalidate")])
            raise HttpError("http://origin.test/strict: connection refused")
        if path == "/nocache":
            if inm == '"n1"':
                return page(304, "", [("ETag", '"n1"')])
            return page(200, "check me", [("ETag", '"n1"'), ("Cache-Control", "no-cache")])
        if path == "/lang":
            lang = req.header("Accept-Language") or "en"
            body = "bonjour" if lang == "fr" else "hello"
            return page(200, body, [("Vary", "Accept-Language"), ("Cache-Control", "max-age=60")])
        if path in ("/a", "/b", "/c", "/d"):
            return page(200, f"body of {path} ......................", [("Cache-Control", "max-age=60")])
        return page(404, "not found", [])


def get(path):
    return Request("GET", path)


def show(cache, req, at_s):
    what = f"{req.method()} {req.url()}"
    r = handle(cache, req, at_s * 1000)
    label = header_of(r.headers, "x-cache") or "?"
    age = header_of(r.headers, "age")
    age = f" age {age}" if age is not None else ""
    print(f"  t={at_s:<3} {what:<14} {r.status} {label:<11} {r.body}{age}")


def opt(v):
    return "None" if v is None else f"Some({v})"


def self_test():
    print("== self-test: cache-control")
    for h in [
        "max-age=60",
        "public, max-age=0, s-maxage=30",
        "no-store",
        "private, max-age=600",
        "no-cache, must-revalidate",
        'max-age="20", stale-if-error=300',
        "MAX-AGE=5, Proxy-Revalidate",
        "max-age=soon",
    ]:
        d = parse_cache_control(h)
        print(f"  {h:<34} max-age {opt(d.max_age)} s-maxage {opt(d.s_maxage)} no-store {str(d.no_store).lower()} no-cache {str(d.no_cache).lower()} private {str(d.private).lower()} revalidate {str(d.must_revalidate).lower()} stale-if-error {d.stale_if_error}")

    print("== self-test: a fake origin, times chosen")
    origin = FakeOrigin()
    cache = Cache("http://origin.test", 120, 80, origin.send)
    show(cache, get("/doc"), 0)
    show(cache, get("/doc"), 4)
    show(cache, get("/doc").with_header("If-None-Match", '"a"'), 5)
    show(cache, get("/doc").with_header("Cache-Control", "no-cache"), 6)
    show(cache, get("/doc"), 17)
    show(cache, get("/doc"), 20)
    show(cache, get("/aged"), 0)
    show(cache, get("/aged"), 2)
    show(cache, get("/aged"), 3)
    show(cache, get("/shared"), 0)
    show(cache, get("/shared"), 20)
    show(cache, get("/gone"), 0)
    show(cache, get("/gone"), 1)
    show(cache, get("/flaky"), 0)
    show(cache, get("/flaky"), 4)
    show(cache, get("/flaky"), 8)
    show(cache, get("/strict"), 0)
    show(cache, get("/strict"), 4)
    show(cache, get("/nocache"), 0)
    show(cache, get("/nocache"), 1)
    show(cache, get("/lang").with_header("Accept-Language", "fr"), 0)
    show(cache, get("/lang"), 0)
    show(cache, get("/lang").with_header("Accept-Language", "fr"), 1)
    show(cache, get("/doc2").with_header("Authorization", "Bearer fake"), 0)
    show(cache, get("/public").with_header("Authorization", "Bearer fake"), 0)
    show(cache, get("/public").with_header("Authorization", "Bearer fake"), 1)
    show(cache, get("/doc").with_header("Cache-Control", "no-store"), 21)
    show(cache, get("/a"), 30)
    show(cache, get("/b"), 30)
    show(cache, get("/doc"), 30)
    show(cache, get("/c"), 31)
    show(cache, get("/d"), 31)
    show(cache, get("/missing"), 31)
    show(cache, Request("PURGE", "/doc"), 32)
    show(cache, Request("PURGE", "/doc").with_header("X-Purge-Token", PURGE_TOKEN), 32)
    show(cache, Request("PURGE", "/doc").with_header("X-Purge-Token", PURGE_TOKEN), 32)
    show(cache, Request("DELETE", "/d"), 33)
    show(cache, get("/d"), 33)
    print_state(cache)


# ---------------------------------------------------------------- network

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
        data = body.encode() if body is not None else None
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
        req = Request(self.command, self.path, list(self.headers.items()), body)
        r = self.server.app(req)
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

    do_GET = do_POST = do_PURGE = do_DELETE = answer


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def serve(addr, app):
    s = Server(addr, Handler)
    s.app = app
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s


class Hits:
    def __init__(self):
        self.lock = threading.Lock()
        self.counts = {}

    def next(self, key):
        with self.lock:
            n = self.counts.get(key, 0) + 1
            self.counts[key] = n
            return n

    def seen(self, key):
        with self.lock:
            return self.counts.get(key, 0)


def text(status, body, headers):
    r = page(status, body, headers)
    r.headers.append(("Content-Type", "text/plain"))
    return r


JS_TAG = '"js-1"'
LAST_MODIFIED = "Tue, 03 Nov 2026 10:00:00 GMT"


def origin(hits, req):
    path = req.path()
    method = req.method()
    n = hits.next(f"{method} {path}")
    inm = req.header("If-None-Match") or ""
    if path == "/static/app.js":
        if inm == JS_TAG:
            return text(304, "", [("ETag", JS_TAG), ("Cache-Control", "public, max-age=1")])
        return text(200, "console.log('app v1');", [("ETag", JS_TAG), ("Cache-Control", "public, max-age=1")])
    if path == "/news":
        tag = f'"news-{n}"'
        if inm == tag:
            return text(304, "", [("ETag", tag)])
        return text(200, f"headline {n}", [("ETag", tag), ("Cache-Control", "max-age=1")])
    if path == "/account":
        return text(200, "your account", [("Cache-Control", "private, max-age=60")])
    if path == "/cart":
        return text(200, f"cart {n}", [("Cache-Control", "no-store")])
    if path == "/greeting":
        lang = req.header("Accept-Language") or "en"
        body = "bonjour" if lang.startswith("fr") else "hello"
        return text(200, body, [("Vary", "Accept-Language"), ("Cache-Control", "max-age=60")])
    if path == "/everything":
        return text(200, "varies on all", [("Vary", "*"), ("Cache-Control", "max-age=60")])
    if path == "/items":
        if method == "POST":
            return text(201, "created", [])
        posts = hits.seen("POST /items")
        return text(200, f"items after {posts} posts", [("Cache-Control", "max-age=60")])
    if path == "/report":
        return text(200, "x" * 300, [("Cache-Control", "max-age=60")])
    if path == "/flaky":
        if n == 1:
            return text(200, "flaky ok", [("Cache-Control", "max-age=1, stale-if-error=60")])
        return text(503, "flaky down", [])
    if path == "/strict":
        if n == 1:
            return text(200, "strict ok", [("Cache-Control", "max-age=1, must-revalidate")])
        return text(503, "strict down", [])
    if path == "/archive":
        if req.header("If-Modified-Since") == LAST_MODIFIED:
            return text(304, "", [])
        return text(200, "the archive", [("Last-Modified", LAST_MODIFIED)])
    return text(404, "not found", [])


def call(method, path, headers):
    shown = "".join(f" {k}: {v}" for k, v in headers)
    try:
        r = real_send(method, f"http://{CACHE_ADDR[0]}:{CACHE_ADDR[1]}{path}", headers, None, 3000)
        label = header_of(r.headers, "X-Cache") or "?"
        print(f"  {method} {path}{shown} -> {r.status} {label} {r.body}")
    except HttpError as e:
        print(f"  {method} {path}{shown} -> error: {e}")


def main():
    self_test()
    hits = Hits()
    cache = Cache(f"http://{ORIGIN_ADDR[0]}:{ORIGIN_ADDR[1]}", 256, 200, real_send)
    serve(ORIGIN_ADDR, lambda req: origin(hits, req))
    serve(CACHE_ADDR, lambda req: handle(cache, req, int((time.monotonic() - cache.start) * 1000)))
    up = 0
    for _ in range(100):
        up = 0
        for addr in (ORIGIN_ADDR, CACHE_ADDR):
            try:
                socket.create_connection(addr, timeout=1).close()
                up += 1
            except OSError:
                pass
        if up == 2:
            break
        time.sleep(0.02)
    print(f"== live: {up} servers up")

    call("GET", "/static/app.js", [])
    call("GET", "/static/app.js", [])
    call("GET", "/static/app.js", [("If-None-Match", JS_TAG)])
    call("GET", "/news", [])
    call("GET", "/news", [])
    call("GET", "/flaky", [])
    call("GET", "/strict", [])
    call("GET", "/account", [])
    call("GET", "/account", [])
    call("GET", "/cart", [])
    call("GET", "/cart", [])
    call("GET", "/greeting", [("Accept-Language", "fr-CA")])
    call("GET", "/greeting", [("Accept-Language", "en")])
    call("GET", "/greeting", [("Accept-Language", "fr-CA")])
    call("GET", "/everything", [])
    call("GET", "/items", [])
    call("GET", "/items", [])
    call("POST", "/items", [])
    call("GET", "/items", [])
    call("GET", "/report", [])
    call("GET", "/archive", [])
    call("GET", "/archive", [])
    call("GET", "/missing", [])

    print("== live: a second later")
    time.sleep(1.1)
    call("GET", "/static/app.js", [])
    call("GET", "/news", [])
    call("GET", "/flaky", [])
    call("GET", "/strict", [])
    call("GET", "/items", [("Cache-Control", "no-cache")])
    call("PURGE", "/static/app.js", [])
    call("PURGE", "/static/app.js", [("X-Purge-Token", PURGE_TOKEN)])
    call("GET", "/static/app.js", [])

    print("== live: cache")
    print_state(cache)
    print("== live: origin hits")
    for k, n in sorted(hits.counts.items()):
        print(f"  {k} {n}")
    print("done")


if __name__ == "__main__":
    sys.exit(main())
