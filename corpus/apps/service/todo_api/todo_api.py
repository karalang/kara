# Mirror of source.kara: same JSON library, router, middleware and output.
import socket

ADDR = ("127.0.0.1", 47733)
TOKEN = "s3cret"

class JErr(Exception):
    pass

class Reader:
    def __init__(self, text):
        self.chars = list(text); self.at = 0
    def fail(self, what):
        return JErr(f"{what} at offset {self.at}")
    def skip_ws(self):
        while self.at < len(self.chars) and self.chars[self.at] in " \n\t\r":
            self.at += 1
    def peek(self):
        return self.chars[self.at] if self.at < len(self.chars) else None
    def expect(self, c):
        self.skip_ws()
        d = self.peek()
        if d is None: raise self.fail(f"expected '{c}', found end of input")
        if d != c: raise self.fail(f"expected '{c}', found '{d}'")
        self.at += 1
    def word(self, w, v):
        for c in w:
            if self.peek() != c: raise self.fail(f"invalid literal, expected {w}")
            self.at += 1
        return v
    def string(self):
        self.expect('"')
        out = []
        while True:
            c = self.peek()
            if c is None: raise self.fail("unterminated string")
            self.at += 1
            if c == '"': return "".join(out)
            if c != "\\":
                if ord(c) < 32: raise self.fail("control character in string")
                out.append(c); continue
            e = self.peek()
            if e is None: raise self.fail("unterminated escape")
            self.at += 1
            simple = {'"': '"', "\\": "\\", "/": "/", "n": "\n", "t": "\t", "r": "\r", "b": "\b", "f": "\f"}
            if e in simple: out.append(simple[e])
            elif e == "u":
                code = 0
                for _ in range(4):
                    h = self.peek()
                    if h is None: raise self.fail("short \\u escape")
                    if h not in "0123456789abcdefABCDEF": raise self.fail("bad hex digit in \\u escape")
                    code = code * 16 + int(h, 16); self.at += 1
                if 0xD800 <= code <= 0xDFFF: raise self.fail("\\u escape is not a scalar value")
                out.append(chr(code))
            else:
                raise self.fail(f"bad escape '\\{e}'")
    def number(self):
        start = self.at
        while self.at < len(self.chars) and (self.chars[self.at].isdigit() or self.chars[self.at] in "-+.eE"):
            self.at += 1
        text = "".join(self.chars[start:self.at])
        try:
            return ("num", float(text))
        except ValueError:
            self.at = start
            raise self.fail(f"bad number '{text}'")
    def value(self, depth):
        if depth > 32: raise self.fail("nesting too deep")
        self.skip_ws()
        c = self.peek()
        if c is None: raise self.fail("expected a value, found end of input")
        if c == "n": return self.word("null", ("null", None))
        if c == "t": return self.word("true", ("bool", True))
        if c == "f": return self.word("false", ("bool", False))
        if c == '"': return ("text", self.string())
        if c == "[":
            self.at += 1; items = []; self.skip_ws()
            if self.peek() == "]":
                self.at += 1; return ("arr", items)
            while True:
                items.append(self.value(depth + 1)); self.skip_ws()
                p = self.peek()
                if p == ",": self.at += 1
                elif p == "]":
                    self.at += 1; return ("arr", items)
                else: raise self.fail("expected ',' or ']'")
        if c == "{":
            self.at += 1; fields = []; self.skip_ws()
            if self.peek() == "}":
                self.at += 1; return ("obj", fields)
            while True:
                self.skip_ws()
                k = self.string(); self.expect(":")
                v = self.value(depth + 1); fields.append((k, v)); self.skip_ws()
                p = self.peek()
                if p == ",": self.at += 1
                elif p == "}":
                    self.at += 1; return ("obj", fields)
                else: raise self.fail("expected ',' or '}'")
        if c == "-" or c.isdigit(): return self.number()
        raise self.fail(f"unexpected '{c}'")

def parse_json(text):
    r = Reader(text); v = r.value(0); r.skip_ws()
    if r.at < len(r.chars): raise r.fail("trailing characters")
    return v

def quote(s):
    out = ['"']
    for c in s:
        m = {'"': '\\"', "\\": "\\\\", "\n": "\\n", "\r": "\\r", "\t": "\\t", "\b": "\\b", "\f": "\\f"}
        if c in m: out.append(m[c])
        elif ord(c) < 32: out.append(f"\\u{ord(c):04x}")
        else: out.append(c)
    out.append('"')
    return "".join(out)

def to_json(v):
    k, x = v
    if k == "null": return "null"
    if k == "bool": return "true" if x else "false"
    if k == "num":
        return str(int(x)) if x == int(x) and abs(x) < 1e15 else repr(x)
    if k == "text": return quote(x)
    if k == "arr": return "[" + ",".join(to_json(i) for i in x) + "]"
    return "{" + ",".join(quote(a) + ":" + to_json(b) for a, b in x) + "}"

def field(v, name):
    if v[0] != "obj": return None
    for k, it in v[1]:
        if k == name: return it
    return None

REASON = {200: "OK", 201: "Created", 204: "No Content", 400: "Bad Request", 401: "Unauthorized", 404: "Not Found", 405: "Method Not Allowed"}

def error(status, msg):
    return (status, to_json(("obj", [("error", ("text", msg))])))

def header(r, name):
    for k, v in r["headers"]:
        if k.lower() == name.lower(): return v
    return None

def segments(p):
    return [s for s in p.split("/") if s]

def match_route(pat, path):
    if len(pat) != len(path): return None
    params = []
    for a, b in zip(pat, path):
        if a.startswith(":"): params.append(b)
        elif a != b: return None
    return params

def percent_decode(s):
    b = s.encode(); out = bytearray(); i = 0
    while i < len(b):
        if b[i] == 43: out.append(32)
        elif b[i] == 37 and i + 2 < len(b):
            try:
                out.append(int(s[i + 1:i + 3], 16)); i += 2
            except ValueError:
                out.append(b[i])
        else: out.append(b[i])
        i += 1
    try: return out.decode()
    except UnicodeDecodeError: return s

def todo_json(t):
    return ("obj", [("id", ("num", float(t["id"]))), ("title", ("text", t["title"])),
                    ("done", ("bool", t["done"])), ("tags", ("arr", [("text", g) for g in t["tags"]]))])

def to_int(s):
    try:
        if s.strip() != s or s.startswith("+"): return None
        return int(s, 10)
    except ValueError:
        return None

def id_param(p):
    n = to_int(p[0])
    if n is None: return None, error(400, f"bad id '{p[0]}'")
    return n, None

def h_health(st, r, p):
    return (200, to_json(("obj", [("ok", ("bool", True)), ("todos", ("num", float(len(st["todos"]))))])))

def h_list(st, r, p):
    wd = None; wt = None
    for k, v in r["query"]:
        if k == "done": wd = v == "true"
        elif k == "tag": wt = v
    items = [todo_json(t) for _, t in sorted(st["todos"].items())
             if (wd is None or t["done"] == wd) and (wt is None or wt in t["tags"])]
    return (200, to_json(("arr", items)))

def h_create(st, r, p):
    try: body = parse_json(r["body"])
    except JErr as e: return error(400, f"invalid JSON: {e}")
    t = field(body, "title")
    if t is None: return error(400, "title is required")
    if t[0] != "text": return error(400, "title must be a string")
    tags = []
    g = field(body, "tags")
    if g is not None:
        if g[0] != "arr": return error(400, "tags must be an array")
        for it in g[1]:
            if it[0] != "text": return error(400, "tags must be strings")
            tags.append(it[1])
    i = st["next_id"]; st["next_id"] += 1
    td = {"id": i, "title": t[1], "done": False, "tags": tags}
    st["todos"][i] = td
    return (201, to_json(todo_json(td)))

def h_get(st, r, p):
    i, e = id_param(p)
    if e: return e
    t = st["todos"].get(i)
    return (200, to_json(todo_json(t))) if t else error(404, f"no todo {i}")

def h_patch(st, r, p):
    i, e = id_param(p)
    if e: return e
    try: body = parse_json(r["body"])
    except JErr as ex: return error(400, f"invalid JSON: {ex}")
    t = st["todos"].get(i)
    if t is None: return error(404, f"no todo {i}")
    problem = None
    d = field(body, "done")
    if d is not None:
        if d[0] == "bool": t["done"] = d[1]
        else: problem = "done must be a boolean"
    ti = field(body, "title")
    if ti is not None:
        if ti[0] == "text": t["title"] = ti[1]
        else: problem = "title must be a string"
    return error(400, problem) if problem else (200, to_json(todo_json(t)))

def h_delete(st, r, p):
    i, e = id_param(p)
    if e: return e
    if i in st["todos"]:
        del st["todos"][i]; return (204, "")
    return error(404, f"no todo {i}")

ROUTES = [("GET", "/health", h_health), ("GET", "/todos", h_list), ("POST", "/todos", h_create),
          ("GET", "/todos/:id", h_get), ("PATCH", "/todos/:id", h_patch), ("DELETE", "/todos/:id", h_delete)]

def dispatch(st, r):
    path = segments(r["path"]); allowed = []
    for m, pat, h in ROUTES:
        params = match_route(segments(pat), path)
        if params is not None:
            if m == r["method"]: return h(st, r, params)
            allowed.append(m)
    if not allowed: return error(404, f"no route for {r['path']}")
    return error(405, "use " + ", ".join(allowed))

def auth(r):
    if r["method"] == "GET": return None
    h = header(r, "authorization")
    if h is None: return error(401, "missing bearer token")
    return None if h == f"Bearer {TOKEN}" else error(401, "bad token")

def too_big(r):
    return error(400, "body too large") if len(r["body"].encode()) > 256 else None

def handle(st, r):
    for m in (auth, too_big):
        resp = m(r)
        if resp: return resp
    return dispatch(st, r)

def parse_requests(raw):
    out = []; at = 0
    while at < len(raw):
        rest = raw[at:]
        he = rest.find(b"\r\n\r\n")
        head = rest[:he].decode()
        lines = head.split("\r\n")
        first = lines[0]
        parts = first.split(" ")
        headers = []; length = 0
        for l in lines[1:]:
            i = l.find(":")
            k = l[:i].strip(); v = l[i + 1:].strip()
            if k.lower() == "content-length": length = int(v)
            headers.append((k, v))
        bs = at + he + 4
        body = raw[bs:bs + length].decode()
        target = parts[1]
        if "?" in target:
            i = target.find("?"); path, qs = target[:i], target[i + 1:]
        else:
            path, qs = target, ""
        query = []
        for pair in qs.split("&"):
            if not pair: continue
            if "=" in pair:
                i = pair.find("="); query.append((percent_decode(pair[:i]), percent_decode(pair[i + 1:])))
            else:
                query.append((percent_decode(pair), ""))
        out.append({"method": parts[0], "path": path, "query": query, "headers": headers, "body": body})
        at = bs + length
    return out

def render(resp, rid):
    status, body = resp
    s = f"HTTP/1.1 {status} {REASON.get(status, 'Unknown')}\r\nX-Request-Id: {rid}\r\nContent-Length: {len(body.encode())}\r\n"
    if body: s += "Content-Type: application/json\r\n"
    return s + "\r\n" + body

def read_all(s):
    data = b""
    while True:
        c = s.recv(64)
        if not c: return data
        data += c

def request(method, target, auth_, body):
    s = f"{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n"
    if auth_: s += f"Authorization: Bearer {TOKEN}\r\n"
    if body: s += f"Content-Type: application/json\r\nContent-Length: {len(body.encode())}\r\n"
    return s + "\r\n" + body

def print_responses(resp):
    at = 0
    while at < len(resp):
        rest = resp[at:]
        he = rest.find(b"\r\n\r\n")
        head = rest[:he].decode().split("\r\n")
        status = head[0]; rid = ""; length = 0
        for l in head[1:]:
            if l.startswith("Content-Length: "): length = int(l[16:])
            elif l.startswith("X-Request-Id: "): rid = l[14:]
        body = rest[he + 4:he + 4 + length].decode()
        print(f"  [{rid}] {status}" if not body else f"  [{rid}] {status} {body}")
        at += he + 4 + length

def session(listener, srv, st, label, batch):
    print(f"== {label}")
    c = socket.create_connection(ADDR); c.sendall(batch.encode()); c.shutdown(socket.SHUT_WR)
    conn, _ = listener.accept()
    out = ""
    for r in parse_requests(read_all(conn)):
        srv["n"] += 1
        resp = handle(st, r)
        st["log"].append(f"#{srv['n']} {r['method']} {r['path']} -> {resp[0]} ({len(resp[1].encode())} bytes)")
        out += render(resp, srv["n"])
    conn.sendall(out.encode()); conn.close()
    print_responses(read_all(c)); c.close()

def main():
    listener = socket.socket(); listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR); listener.listen(16)
    srv = {"n": 0}; st = {"todos": {}, "next_id": 1, "log": []}
    b = request("GET", "/health", False, "")
    b += request("POST", "/todos", True, '{"title": "buy milk", "tags": ["home", "errand"]}')
    b += request("POST", "/todos", True, '{"title":"write \\"report\\"\\n\\tdraft \\u00e9t\\u00e9","tags":["work"]}')
    b += request("POST", "/todos", True, '  { "title" : "call mum" , "tags" : [ ] , "priority" : 2.5 }  ')
    b += request("GET", "/todos", False, "")
    session(listener, srv, st, "create", b)
    b = request("PATCH", "/todos/1", True, '{"done": true}')
    b += request("GET", "/todos?done=true", False, "")
    b += request("GET", "/todos?done=false&tag=work", False, "")
    b += request("GET", "/todos?tag=home%20x", False, "")
    b += request("GET", "/todos/2", False, "")
    b += request("DELETE", "/todos/3", True, "")
    b += request("GET", "/todos/3", False, "")
    session(listener, srv, st, "update", b)
    b = request("POST", "/todos", False, '{"title": "no auth"}')
    b += request("DELETE", "/todos/1", False, "")
    b += request("PUT", "/todos/1", True, "{}")
    b += request("GET", "/nope", False, "")
    b += request("GET", "/todos/abc", False, "")
    b += request("POST", "/todos", True, '{"title": 5}')
    b += request("POST", "/todos", True, '{"tags": []}')
    b += request("POST", "/todos", True, '{"title": "x", "tags": [1]}')
    b += request("POST", "/todos", True, '{"title" "x"}')
    b += request("POST", "/todos", True, "[1, 2,]")
    b += request("POST", "/todos", True, '{"title": "bad \\q"}')
    b += request("POST", "/todos", True, '{"title": "x"} extra')
    b += request("POST", "/todos", True, "tru")
    b += request("PATCH", "/todos/2", True, '{"done": "yes"}')
    b += request("PATCH", "/todos/9", True, '{"done": true}')
    session(listener, srv, st, "errors", b)
    long = '{"title": "' + "x" * 300 + '"}'
    b = request("POST", "/todos", True, long) + request("GET", "/health", False, "")
    session(listener, srv, st, "limits", b)
    print("== access log")
    for l in st["log"]: print(f"  {l}")

main()
