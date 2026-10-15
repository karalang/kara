"""Mirror of rev_proxy.kara: a reverse proxy in front of loopback origins.

Same single-threaded choreography over real sockets: the client writes a
request and shuts down its write side, the proxy accepts and reads it,
connects to the chosen origin, the origin accepts and answers, and the
replies flow back through kernel buffers.
"""
import socket

MAX_BODY = 64
COOLDOWN = 3

REASONS = {200: "OK", 201: "Created", 400: "Bad Request", 404: "Not Found",
           413: "Payload Too Large", 500: "Internal Server Error",
           502: "Bad Gateway", 503: "Service Unavailable"}


def reason(status):
    return REASONS.get(status, "Unknown")


def read_all(conn):
    chunks = []
    while True:
        b = conn.recv(64)
        if not b:
            break
        chunks.append(b)
    return b"".join(chunks).decode("utf-8")


class Message:
    def __init__(self, start, headers, body):
        self.start = start
        self.headers = headers
        self.body = body


class ParseError(Exception):
    pass


def header(m, name):
    for k, v in m.headers:
        if k.lower() == name:
            return v
    return None


def dechunk(raw):
    out = []
    at = 0
    while True:
        rest = raw[at:]
        eol = rest.find("\r\n")
        if eol < 0:
            raise ParseError("chunk size line missing")
        try:
            size = int(rest[:eol].strip(), 16)
        except ValueError:
            raise ParseError(f"bad chunk size [{rest[:eol]}]")
        if size == 0:
            return "".join(out)
        start = eol + 2
        if start + size + 2 > len(rest):
            raise ParseError("chunk truncated")
        out.append(rest[start:start + size])
        at += start + size + 2


def parse_message(raw):
    head_end = raw.find("\r\n\r\n")
    if head_end < 0:
        raise ParseError("no end of headers")
    lines = raw[:head_end].split("\r\n")
    headers = []
    for line in lines[1:]:
        c = line.find(":")
        if c < 0:
            raise ParseError(f"bad header [{line}]")
        headers.append((line[:c].strip(), line[c + 1:].strip()))
    m = Message(lines[0], headers, "")
    rest = raw[head_end + 4:]
    te = header(m, "transfer-encoding")
    if te is not None and te.lower() == "chunked":
        m.body = dechunk(rest)
    else:
        v = header(m, "content-length")
        n = 0
        if v is not None:
            try:
                n = int(v)
            except ValueError:
                raise ParseError(f"bad content-length [{v}]")
        if n > len(rest):
            raise ParseError(f"body short by {n - len(rest)} bytes")
        m.body = rest[:n]
    return m


HOP = ["connection", "keep-alive", "proxy-connection", "te", "trailer",
       "transfer-encoding", "upgrade"]


def end_to_end(m):
    extra = []
    v = header(m, "connection")
    if v is not None:
        extra = [t.strip().lower() for t in v.split(",")]
    out = []
    for k, val in m.headers:
        n = k.lower()
        if n in HOP or n in extra or n == "content-length":
            continue
        out.append((k, val))
    return out


def render(start, headers, body):
    s = start + "\r\n"
    for k, v in headers:
        s += f"{k}: {v}\r\n"
    s += f"Content-Length: {len(body)}\r\n\r\n"
    return s + body


def simple(status, body):
    return render(f"HTTP/1.1 {status} {reason(status)}",
                  [("Content-Type", "text/plain"), ("Via", "1.1 kproxy")], body)


def status_of(start):
    parts = start.split(" ")
    if len(parts) < 2:
        return 0
    try:
        return int(parts[1])
    except ValueError:
        return 0


# ---- origins ----

def origin_reply(name, req):
    parts = req.start.split(" ")
    method, path = parts[0], parts[1]
    headers = [("Server", f"origin-{name}"),
               ("Connection", "close, X-Origin-Trace"),
               ("X-Origin-Trace", f"{name}-internal")]
    status = 200
    if path == "/users" and method == "GET":
        body = '[{"id":1,"name":"ada"},{"id":2,"name":"bob"}]'
    elif path == "/users" and method == "POST":
        status = 201
        body = f'{{"created":{req.body},"by":"{name}"}}'
    elif path.startswith("/users/"):
        body = f'{{"id":{path[7:]},"from":"{name}"}}'
    elif path == "/flaky":
        if name == "b1":
            status = 503
            body = "draining"
        else:
            body = f"steady on {name}"
    elif path == "/headers":
        names = sorted(k.lower() for k, _ in req.headers)
        xff = header(req, "x-forwarded-for") or "-"
        pre = header(req, "x-forwarded-prefix") or "-"
        body = f"{','.join(names)} xff={xff} prefix={pre}"
    elif path == "/export":
        headers.append(("Transfer-Encoding", "chunked"))
        s = "HTTP/1.1 200 OK\r\n"
        for k, v in headers:
            s += f"{k}: {v}\r\n"
        s += "\r\n"
        for r in ["id,name\n", "1,ada\n", "2,bob\n", "3,cy\n"]:
            s += f"{len(r):x}\r\n{r}\r\n"
        return s + "0\r\n\r\n"
    else:
        status = 404
        body = f"origin {name} has no {path}"
    return render(f"HTTP/1.1 {status} {reason(status)}", headers, body)


class Origin:
    def __init__(self, name, port):
        self.name = name
        self.seen = 0
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", port))
        self.listener.listen(16)


def serve_origin(origins, name):
    for o in origins:
        if o.name == name:
            if o.listener is None:
                print(f"origin {name} is not listening")
                return
            conn, _ = o.listener.accept()
            o.seen += 1
            try:
                out = origin_reply(name, parse_message(read_all(conn)))
            except ParseError as e:
                out = simple(400, str(e))
            conn.sendall(out.encode())
            conn.close()
            return
    print(f"no origin {name}")


# ---- proxy ----

class Upstream:
    def __init__(self, name, port):
        self.name = name
        self.addr = ("127.0.0.1", port)
        self.down_for = 0
        self.refused = 0
        self.served = 0


class Cluster:
    def __init__(self, name, members):
        self.name = name
        self.members = members
        self.next = 0


def pick(pool, tried):
    n = len(pool.members)
    for k in range(n):
        idx = (pool.next + k) % n
        if pool.members[idx].down_for == 0 and not tried[idx]:
            pool.next = (idx + 1) % n
            return idx
    return None


def route_for(routes, path):
    best, best_len = None, 0
    for i, (prefix, _, _) in enumerate(routes):
        if path.startswith(prefix) and len(prefix) > best_len:
            best, best_len = i, len(prefix)
    return best


def try_pool(pool, origins, outgoing, method):
    tried = [False] * len(pool.members)
    tries = 0
    last = None
    while True:
        idx = pick(pool, tried)
        if idx is None:
            break
        tried[idx] = True
        tries += 1
        up = pool.members[idx]
        name = up.name
        conn = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            conn.connect(up.addr)
        except ConnectionRefusedError:
            conn.close()
            up.refused += 1
            up.down_for = COOLDOWN
            print(f"  [pool {pool.name}] {name} refused, out for {COOLDOWN}")
            continue
        conn.sendall(outgoing.encode())
        conn.shutdown(socket.SHUT_WR)
        serve_origin(origins, name)
        raw = read_all(conn)
        conn.close()
        try:
            resp = parse_message(raw)
        except ParseError as e:
            last = (simple(502, f"bad upstream reply: {e}"), name)
            break
        up.served += 1
        out_headers = end_to_end(resp)
        out_headers.append(("Via", "1.1 kproxy"))
        out_headers.append(("X-Upstream", name))
        reply = render(resp.start, out_headers, resp.body)
        if status_of(resp.start) == 503 and method == "GET":
            print(f"  [pool {pool.name}] {name} answered 503, retrying GET")
            last = (reply, name)
            continue
        return reply, name, tries
    if last is not None:
        return last[0], last[1], tries
    return simple(502, f"no healthy upstream in {pool.name}"), "-", tries


class Proxy:
    def __init__(self, port, routes, pools):
        self.addr = ("127.0.0.1", port)
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(self.addr)
        self.listener.listen(16)
        self.routes = routes
        self.pools = pools
        self.log = []


def forward(proxy, origins, req, method, path):
    ri = route_for(proxy.routes, path)
    if ri is None:
        return simple(404, f"no route for {path}"), "-", 0
    prefix, pi, strip = proxy.routes[ri]
    pool = proxy.pools[pi]
    for m in pool.members:
        if m.down_for > 0:
            m.down_for -= 1
    upstream_path = path[len(prefix) - 1:] if strip else path
    headers = end_to_end(req)
    headers.append(("X-Forwarded-For", "127.0.0.1"))
    headers.append(("X-Forwarded-Prefix", prefix[:-1]))
    headers.append(("Via", "1.1 kproxy"))
    outgoing = render(f"{method} {upstream_path} HTTP/1.1", headers, req.body)
    return try_pool(pool, origins, outgoing, method)


def handle_client(proxy, origins):
    conn, _ = proxy.listener.accept()
    raw = read_all(conn)
    try:
        req = parse_message(raw)
        parts = req.start.split(" ")
        if len(parts) != 3 or not parts[2].startswith("HTTP/1."):
            reply, upstream, tries, method, path = simple(400, "bad request line"), "-", 0, "?", "?"
        elif len(req.body) > MAX_BODY:
            reply, upstream, tries = simple(413, f"body over {MAX_BODY} bytes"), "-", 0
            method, path = parts[0], parts[1]
        else:
            reply, upstream, tries = forward(proxy, origins, req, parts[0], parts[1])
            method, path = parts[0], parts[1]
    except ParseError as e:
        reply, upstream, tries, method, path = simple(400, str(e)), "-", 0, "?", "?"
    status = status_of(reply)
    i = reply.find("\r\n\r\n")
    body_len = len(reply) - i - 4 if i >= 0 else 0
    proxy.log.append((method, path, upstream, status, body_len, tries))
    conn.sendall(reply.encode())
    conn.close()


# ---- client ----

def send(proxy, origins, label, raw):
    print(f"== {label}")
    client = socket.create_connection(proxy.addr)
    client.sendall(raw.encode())
    client.shutdown(socket.SHUT_WR)
    handle_client(proxy, origins)
    resp = read_all(client)
    client.close()
    try:
        m = parse_message(resp)
        print(f"  {m.start}")
        for k, v in m.headers:
            print(f"  {k}: {v}")
        print(f"  | {m.body}")
    except ParseError as e:
        print(f"  unparseable reply: {e}")


def get(path):
    return f"GET {path} HTTP/1.1\r\nHost: shop.test\r\nUser-Agent: kclient\r\n\r\n"


def post(path, body):
    return (f"POST {path} HTTP/1.1\r\nHost: shop.test\r\nContent-Type: application/json\r\n"
            f"Content-Length: {len(body)}\r\n\r\n{body}")


def main():
    origins = [Origin("b1", 47738), Origin("b2", 47739)]
    proxy = Proxy(47737,
                  [("/api/", 0, True), ("/api/admin/", 1, False), ("/static/", 1, False)],
                  [Cluster("api", [Upstream("b1", 47738), Upstream("b2", 47739), Upstream("b4", 47740)]),
                   Cluster("static", [Upstream("s1", 47741)])])

    send(proxy, origins, "list users", get("/api/users"))
    send(proxy, origins, "one user", get("/api/users/7"))
    send(proxy, origins, "dead upstream is skipped", get("/api/users/8"))
    send(proxy, origins, "create", post("/api/users", '{"name":"cy"}'))
    send(proxy, origins, "503 GET is retried", get("/api/flaky"))
    send(proxy, origins, "503 POST is not", post("/api/flaky", "{}"))
    send(proxy, origins, "hop-by-hop headers dropped",
         "GET /api/headers HTTP/1.1\r\nHost: shop.test\r\nConnection: keep-alive, X-Session\r\n"
         "Keep-Alive: timeout=5\r\nX-Session: s3cret\r\nX-Request-Id: r-42\r\n\r\n")
    send(proxy, origins, "chunked origin reply", get("/api/export"))
    send(proxy, origins, "static pool is all down", get("/static/logo.png"))
    send(proxy, origins, "longer prefix wins", get("/api/admin/stats"))
    send(proxy, origins, "no route", get("/nowhere"))
    send(proxy, origins, "body too large",
         post("/api/users", '{"bio":"0123456789012345678901234567890123456789012345678901234567890123"}'))
    send(proxy, origins, "malformed", "BLAH\r\n\r\n")

    print("-- b2 shuts down")
    origins[1].listener.close()
    origins[1].listener = None
    for i in range(1, 6):
        send(proxy, origins, f"after b2 closes ({i})", get(f"/api/users/{i}"))

    print("-- access log")
    for method, path, upstream, status, nbytes, tries in proxy.log:
        print(f"{method} {path} -> {upstream} {status} {nbytes}b tries={tries}")
    print("-- upstreams")
    for p in proxy.pools:
        for m in p.members:
            print(f"{p.name}/{m.name} served={m.served} refused={m.refused} down_for={m.down_for}")
    for o in origins:
        print(f"origin {o.name} saw {o.seen}")


main()
