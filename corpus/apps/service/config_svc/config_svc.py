# Python mirror of config_svc.kara: the same hand-written JSON reader and
# writer (integers only), JSON Pointer, Merge Patch, schema check, layers
# and HTTP handling, over a loopback socket with the client and server in
# one thread. Objects are dicts (insertion-ordered, which matches the Kara
# program's Vec of pairs: replace in place, remove, append at the end).
import os
import socket

ADDR = ("127.0.0.1", 47736)
FILE = "config.json"

DEFAULTS = '{"server": {"host": "0.0.0.0", "port": 8080, "tls": false}, "log": {"level": "info", "targets": ["stdout"]}, "limits": {"max_conn": 100, "timeout_ms": 30000}, "features": {"beta": false, "regions": ["eu"]}}'

SCHEMA = """{"type": "object", "props": {
  "server": {"type": "object", "props": {
    "host": {"type": "string"},
    "port": {"type": "int", "min": 1, "max": 65535},
    "tls": {"type": "bool"}}},
  "log": {"type": "object", "props": {
    "level": {"type": "string", "enum": ["debug", "info", "warn", "error"]},
    "targets": {"type": "array", "max_items": 3, "items": {"type": "string"}}}},
  "limits": {"type": "object", "props": {
    "max_conn": {"type": "int", "min": 1, "max": 10000},
    "timeout_ms": {"type": "int", "min": 100}}},
  "features": {"type": "object", "props": {
    "beta": {"type": "bool"},
    "regions": {"type": "array", "items": {"type": "string", "enum": ["eu", "us", "ap"]}}}}}}"""

FIRST_FILE = '{"server": {"host": "127.0.0.1"}, "log": {"targets": ["stdout", "file"]}, "features": {"regions": ["eu", "us"]}}'


class JsonError(Exception):
    pass


# ---- JSON ----------------------------------------------------------------

class Reader:
    def __init__(self, text):
        self.chars = text
        self.at = 0

    def fail(self, what):
        return JsonError(f"{what} at offset {self.at}")

    def skip_ws(self):
        while self.at < len(self.chars) and self.chars[self.at] in " \n\t\r":
            self.at += 1

    def peek(self):
        return self.chars[self.at] if self.at < len(self.chars) else None

    def expect(self, c):
        self.skip_ws()
        d = self.peek()
        if d is None:
            raise self.fail(f"expected '{c}', found end of input")
        if d != c:
            raise self.fail(f"expected '{c}', found '{d}'")
        self.at += 1

    def word(self, w, v):
        for c in w:
            if self.peek() != c:
                raise self.fail(f"invalid literal, expected {w}")
            self.at += 1
        return v

    def string(self):
        self.expect('"')
        out = ""
        while True:
            c = self.peek()
            if c is None:
                raise self.fail("unterminated string")
            self.at += 1
            if c == '"':
                return out
            if c != "\\":
                out += c
                continue
            e = self.peek()
            if e is None:
                raise self.fail("unterminated escape")
            self.at += 1
            esc = {'"': '"', "\\": "\\", "/": "/", "n": "\n", "t": "\t"}
            if e not in esc:
                raise self.fail(f"bad escape '\\{e}'")
            out += esc[e]

    def number(self):
        start = self.at
        if self.peek() == "-":
            self.at += 1
        while self.at < len(self.chars) and self.chars[self.at].isdigit():
            self.at += 1
        if self.peek() in (".", "e", "E"):
            raise self.fail("only integers are supported")
        text = self.chars[start:self.at]
        try:
            return int(text)
        except ValueError:
            self.at = start
            raise self.fail(f"bad number '{text}'")

    def value(self, depth):
        if depth > 16:
            raise self.fail("nesting too deep")
        self.skip_ws()
        c = self.peek()
        if c is None:
            raise self.fail("expected a value, found end of input")
        if c == "n":
            return self.word("null", None)
        if c == "t":
            return self.word("true", True)
        if c == "f":
            return self.word("false", False)
        if c == '"':
            return self.string()
        if c == "[":
            self.at += 1
            items = []
            self.skip_ws()
            if self.peek() == "]":
                self.at += 1
                return items
            while True:
                items.append(self.value(depth + 1))
                self.skip_ws()
                p = self.peek()
                if p == ",":
                    self.at += 1
                elif p == "]":
                    self.at += 1
                    return items
                else:
                    raise self.fail("expected ',' or ']'")
        if c == "{":
            self.at += 1
            fields = {}
            self.skip_ws()
            if self.peek() == "}":
                self.at += 1
                return fields
            while True:
                self.skip_ws()
                k = self.string()
                self.expect(":")
                v = self.value(depth + 1)
                if k in fields:
                    del fields[k]
                fields[k] = v
                self.skip_ws()
                p = self.peek()
                if p == ",":
                    self.at += 1
                elif p == "}":
                    self.at += 1
                    return fields
                else:
                    raise self.fail("expected ',' or '}'")
        if c == "-" or c.isdigit():
            return self.number()
        raise self.fail(f"unexpected '{c}'")


def parse_json(text):
    r = Reader(text)
    v = r.value(0)
    r.skip_ws()
    if r.at < len(r.chars):
        raise r.fail("trailing characters")
    return v


def quote(s):
    out = '"'
    for c in s:
        out += {'"': '\\"', "\\": "\\\\", "\n": "\\n", "\t": "\\t"}.get(c, c)
    return out + '"'


def to_json(v):
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, str):
        return quote(v)
    if isinstance(v, list):
        return "[" + ",".join(to_json(x) for x in v) + "]"
    return "{" + ",".join(quote(k) + ":" + to_json(x) for k, x in v.items()) + "}"


def kind(v):
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "a bool"
    if isinstance(v, int):
        return "an int"
    if isinstance(v, str):
        return "a string"
    if isinstance(v, list):
        return "an array"
    return "an object"


def copy(v):
    if isinstance(v, list):
        return [copy(x) for x in v]
    if isinstance(v, dict):
        return {k: copy(x) for k, x in v.items()}
    return v


def field(v, name):
    if isinstance(v, dict) and name in v:
        return copy(v[name])
    return None


def same(a, b):
    return kind(a) == kind(b) and to_json(a) == to_json(b)


# ---- JSON Pointer --------------------------------------------------------

def parse_pointer(p):
    if p == "":
        return []
    if not p.startswith("/"):
        raise JsonError(f"pointer '{p}' must start with '/'")
    out = []
    for s in p[1:].split("/"):
        seg = ""
        i = 0
        while i < len(s):
            if s[i] != "~":
                seg += s[i]
                i += 1
                continue
            if i + 1 < len(s) and s[i + 1] == "0":
                seg += "~"
            elif i + 1 < len(s) and s[i + 1] == "1":
                seg += "/"
            else:
                raise JsonError(f"bad '~' escape in pointer '{p}'")
            i += 2
        out.append(seg)
    return out


def escape_seg(k):
    return k.replace("~", "~0").replace("/", "~1")


def array_index(seg, n):
    if seg == "" or (len(seg) > 1 and seg.startswith("0")):
        return None
    if not all(c in "0123456789" for c in seg):
        return None
    i = int(seg)
    return i if i < n else None


def get_at(v, path, i):
    if i == len(path):
        return copy(v)
    if isinstance(v, dict):
        return get_at(v[path[i]], path, i + 1) if path[i] in v else None
    if isinstance(v, list):
        j = array_index(path[i], len(v))
        return None if j is None else get_at(v[j], path, i + 1)
    return None


MISSING = object()


def lookup(v, path, i):
    if i == len(path):
        return v
    if isinstance(v, dict):
        return lookup(v[path[i]], path, i + 1) if path[i] in v else MISSING
    if isinstance(v, list):
        j = array_index(path[i], len(v))
        return MISSING if j is None else lookup(v[j], path, i + 1)
    return MISSING


class SetError(Exception):
    pass


def set_at(v, path, i, val):
    last = i + 1 == len(path)
    key = path[i]
    if isinstance(v, dict):
        if key in v:
            if last:
                v[key] = val
            else:
                set_at(v[key], path, i + 1, val)
        else:
            if last:
                v[key] = val
                return
            v[key] = {}
            set_at(v[key], path, i + 1, val)
        return
    if isinstance(v, list):
        if key == "-":
            if not last:
                raise SetError("'-' must be the last segment")
            v.append(val)
            return
        j = array_index(key, len(v))
        if j is None:
            raise SetError(f"index '{key}' is outside an array of {len(v)}")
        if last:
            v[j] = val
        else:
            set_at(v[j], path, i + 1, val)
        return
    raise SetError(f"cannot step into {kind(v)} at '{key}'")


def remove_at(v, path, i):
    last = i + 1 == len(path)
    if isinstance(v, dict):
        if path[i] not in v:
            raise SetError("not set")
        if last:
            del v[path[i]]
        else:
            remove_at(v[path[i]], path, i + 1)
        return
    if isinstance(v, list):
        j = array_index(path[i], len(v))
        if j is None:
            raise SetError("not set")
        if last:
            del v[j]
        else:
            remove_at(v[j], path, i + 1)
        return
    raise SetError("not set")


# ---- Merge Patch ---------------------------------------------------------

def merge_patch(target, patch):
    if not isinstance(patch, dict):
        return copy(patch)
    fields = target if isinstance(target, dict) else {}
    for k, pv in patch.items():
        if pv is None:
            fields.pop(k, None)
            continue
        if k in fields:
            fields[k] = merge_patch(copy(fields[k]), pv)
        else:
            fields[k] = merge_patch(None, pv)
    return fields


def leaves(patch, at, out):
    if isinstance(patch, dict):
        for k, v in patch.items():
            leaves(v, f"{at}/{escape_seg(k)}", out)
    else:
        out.append(at)


# ---- schema --------------------------------------------------------------

def type_of(schema):
    t = field(schema, "type")
    return t if isinstance(t, str) else "any"


def is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def validate(v, s, at, errs):
    here = at if at else "(root)"
    ty = type_of(s)
    if ty == "object":
        if not isinstance(v, dict):
            errs.append(f"{here}: expected an object, found {kind(v)}")
            return
        props = field(s, "props") or {}
        for k, fv in v.items():
            child = f"{at}/{escape_seg(k)}"
            if k in props:
                validate(fv, props[k], child, errs)
            else:
                errs.append(f"{child}: unknown key")
    elif ty == "int":
        if not is_int(v):
            errs.append(f"{here}: expected an int, found {kind(v)}")
            return
        m = field(s, "min")
        if is_int(m) and v < m:
            errs.append(f"{here}: {v} is below the minimum {m}")
        m = field(s, "max")
        if is_int(m) and v > m:
            errs.append(f"{here}: {v} is above the maximum {m}")
    elif ty == "string":
        if not isinstance(v, str):
            errs.append(f"{here}: expected a string, found {kind(v)}")
            return
        choices = field(s, "enum")
        if isinstance(choices, list) and not any(same(c, v) for c in choices):
            names = ", ".join(to_json(c) for c in choices)
            errs.append(f'{here}: "{v}" is not one of {names}')
    elif ty == "bool":
        if not isinstance(v, bool):
            errs.append(f"{here}: expected a bool, found {kind(v)}")
    elif ty == "array":
        if not isinstance(v, list):
            errs.append(f"{here}: expected an array, found {kind(v)}")
            return
        m = field(s, "max_items")
        if is_int(m) and len(v) > m:
            errs.append(f"{here}: {len(v)} items, at most {m} allowed")
        item_schema = field(s, "items") or {}
        for i, it in enumerate(v):
            validate(it, item_schema, f"{at}/{i}", errs)


# ---- layers --------------------------------------------------------------

def env_value(ty, raw):
    if ty == "int":
        try:
            if not raw.lstrip("-").isdigit():
                raise ValueError
            return int(raw)
        except ValueError:
            raise SetError("expected an int")
    if ty == "bool":
        if raw in ("true", "1"):
            return True
        if raw in ("false", "0"):
            return False
        raise SetError("expected true/false/1/0")
    return raw


def env_layer(schema, at, prefix, layer, ptrs, notes):
    ty = type_of(schema)
    if ty == "object":
        props = field(schema, "props")
        if isinstance(props, dict):
            for k, ps in props.items():
                env_layer(ps, f"{at}/{escape_seg(k)}", f"{prefix}__{k.upper()}", layer, ptrs, notes)
        return
    if ty == "array":
        return
    var = prefix.replace("__", "_", 1)
    if var not in os.environ:
        return
    raw = os.environ[var]
    try:
        v = env_value(ty, raw)
    except SetError as e:
        notes.append(f"env {var}={raw} ignored: {e}")
        return
    set_at(layer, parse_pointer(at), 0, v)
    ptrs.append(at)
    notes.append(f"env {var}={raw} -> {at}")


def etag_of(v):
    h = 2166136261
    for b in to_json(v).encode():
        h ^= b
        h = (h * 16777619) % 4294967296
    return f'"{h:08x}"'


class Resp:
    def __init__(self, status, headers, body):
        self.status, self.headers, self.body = status, headers, body


class Store:
    def __init__(self, defaults, env, env_ptrs, schema):
        self.defaults = defaults
        self.file = {}
        self.env = env
        self.env_ptrs = env_ptrs
        self.schema = schema
        self.effective = None
        self.etag = ""
        self.version = 0
        self.log = []

    def commit(self, file, touched):
        base = merge_patch(copy(self.defaults), file)
        errs = []
        validate(base, self.schema, "", errs)
        candidate = merge_patch(copy(base), self.env)
        if not errs:
            validate(candidate, self.schema, "", errs)
        if errs:
            return Resp(422, [], to_json({"errors": errs}))
        self.file = file
        self.effective = candidate
        self.etag = etag_of(self.effective)
        self.version += 1
        headers = [("ETag", self.etag)]
        with open(FILE, "w") as f:
            f.write(to_json(self.file))
        shadowed = []
        for p in self.env_ptrs:
            for t in touched:
                if p == t or p.startswith(f"{t}/"):
                    shadowed.append(p)
        if shadowed:
            headers.append(("Warning", f'299 - "overridden by environment: {", ".join(shadowed)}"'))
        return Resp(200, headers, to_json(self.effective))


# ---- HTTP ----------------------------------------------------------------

REASONS = {200: "OK", 304: "Not Modified", 400: "Bad Request", 404: "Not Found",
           405: "Method Not Allowed", 409: "Conflict", 412: "Precondition Failed",
           415: "Unsupported Media Type", 422: "Unprocessable Content",
           428: "Precondition Required"}


def reason(status):
    return REASONS.get(status, "Internal Server Error")


def error(status, msg):
    return Resp(status, [], to_json({"error": msg}))


def header(r, name):
    for k, v in r["headers"]:
        if k.lower() == name:
            return v
    return None


def with_etag(st, body):
    return Resp(200, [("ETag", st.etag)], body)


def precondition(st, r):
    t = header(r, "if-match")
    if t is None:
        return error(428, "writes need If-Match")
    if t == "*" or t == st.etag:
        return None
    return error(412, f"config is at {st.etag}, not {t}")


def reload(st):
    try:
        with open(FILE) as f:
            text = f.read()
    except OSError:
        return error(500, "could not read config.json")
    try:
        file = parse_json(text)
    except JsonError as e:
        return error(400, f"config.json: {e}")
    return st.commit(file, [])


def handle(st, r):
    path, method = r["path"], r["method"]
    if path == "/config/reload":
        if method != "POST":
            return error(405, "use POST")
        return reload(st)
    if path != "/config" and not path.startswith("/config/"):
        return error(404, f"no route for {path}")
    ptr_text = path[7:]
    try:
        ptr = parse_pointer(ptr_text)
    except JsonError as e:
        return error(400, str(e))
    if method == "GET":
        if not ptr:
            if header(r, "if-none-match") == st.etag:
                return Resp(304, [("ETag", st.etag)], "")
            return with_etag(st, to_json(st.effective))
        if lookup(st.effective, ptr, 0) is MISSING:
            return error(404, f"no value at {ptr_text}")
        return with_etag(st, to_json(get_at(st.effective, ptr, 0)))
    if method == "PATCH":
        if ptr:
            return error(405, "PATCH applies to /config; use PUT on a pointer")
        bad = precondition(st, r)
        if bad:
            return bad
        if header(r, "content-type") != "application/merge-patch+json":
            return error(415, "send application/merge-patch+json")
        try:
            patch = parse_json(r["body"])
        except JsonError as e:
            return error(400, f"invalid JSON: {e}")
        touched = []
        leaves(patch, "", touched)
        file = merge_patch(copy(st.file), patch)
        return st.commit(file, touched)
    if method in ("PUT", "DELETE"):
        if not ptr:
            return error(405, "use PATCH on /config")
        bad = precondition(st, r)
        if bad:
            return bad
        file = copy(st.file)
        if method == "PUT":
            try:
                v = parse_json(r["body"])
            except JsonError as e:
                return error(400, f"invalid JSON: {e}")
            try:
                set_at(file, ptr, 0, v)
            except SetError as e:
                return error(409, str(e))
        else:
            try:
                remove_at(file, ptr, 0)
            except SetError:
                return error(404, f"{ptr_text} is not set in config.json")
        return st.commit(file, [ptr_text])
    return error(405, "use GET, PATCH, PUT or DELETE")


class ReqError(Exception):
    pass


def parse_requests(raw):
    out = []
    at = 0
    while at < len(raw):
        rest = raw[at:]
        head_end = rest.find("\r\n\r\n")
        if head_end < 0:
            raise ReqError("incomplete request head")
        lines = rest[:head_end].split("\r\n")
        parts = lines[0].split(" ")
        if len(parts) != 3:
            raise ReqError(f"bad request line '{lines[0]}'")
        headers = []
        length = 0
        for l in lines[1:]:
            i = l.find(":")
            if i < 0:
                raise ReqError(f"bad header '{l}'")
            k, v = l[:i].strip(), l[i + 1:].strip()
            if k.lower() == "content-length":
                length = int(v)
            headers.append((k, v))
        body_start = at + head_end + 4
        if body_start + length > len(raw):
            raise ReqError("body shorter than Content-Length")
        out.append({"method": parts[0], "path": parts[1], "headers": headers,
                    "body": raw[body_start:body_start + length]})
        at = body_start + length
    return out


def render(resp):
    s = f"HTTP/1.1 {resp.status} {reason(resp.status)}\r\n"
    for k, v in resp.headers:
        s += f"{k}: {v}\r\n"
    s += f"Content-Length: {len(resp.body.encode())}\r\n\r\n"
    return s + resp.body


def read_all(conn):
    data = b""
    while True:
        chunk = conn.recv(64)
        if not chunk:
            break
        data += chunk
    return data.decode()


def serve(st, raw):
    try:
        reqs = parse_requests(raw)
    except ReqError as e:
        return render(error(400, str(e)))
    out = ""
    for r in reqs:
        resp = handle(st, r)
        st.log.append(f"{r['method']} {r['path']} -> {resp.status} (v{st.version})")
        out += render(resp)
    return out


# ---- client --------------------------------------------------------------

def request(method, target, extra, body):
    s = f"{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}"
    if body:
        s += f"Content-Length: {len(body.encode())}\r\n"
    return s + "\r\n" + body


def if_match(tag):
    return f"If-Match: {tag}\r\n"


def patch_headers(tag):
    return f"If-Match: {tag}\r\nContent-Type: application/merge-patch+json\r\n"


def print_responses(resp, tag):
    at = 0
    while at < len(resp):
        rest = resp[at:]
        head_end = rest.find("\r\n\r\n")
        if head_end < 0:
            print("  (truncated response)")
            return tag
        lines = rest[:head_end].split("\r\n")
        length = 0
        line = lines[0]
        warning = ""
        for l in lines[1:]:
            if l.startswith("Content-Length: "):
                length = int(l[16:])
            elif l.startswith("ETag: "):
                tag = l[6:]
                line += f"  etag {tag}"
            elif l.startswith("Warning: "):
                warning = l[9:]
        print(f"  {line}")
        if warning:
            print(f"    warning: {warning}")
        body = rest[head_end + 4:head_end + 4 + length]
        if body:
            print(f"    {body}")
        at += head_end + 4 + length
    return tag


def session(listener, st, label, batch, tag):
    print(f"== {label}")
    client = socket.create_connection(ADDR)
    client.sendall(batch.encode())
    client.shutdown(socket.SHUT_WR)
    conn, _ = listener.accept()
    out = serve(st, read_all(conn))
    conn.sendall(out.encode())
    conn.close()
    tag = print_responses(read_all(client), tag)
    client.close()
    return tag


def operator_edit(text):
    print(f"== operator writes config.json: {text}")
    with open(FILE, "w") as f:
        f.write(text)


def start():
    print("== startup")
    defaults = parse_json(DEFAULTS)
    schema = parse_json(SCHEMA)
    with open(FILE, "w") as f:
        f.write(FIRST_FILE)
    with open(FILE) as f:
        file = parse_json(f.read())
    print(f"  defaults    {to_json(defaults)}")
    print(f"  config.json {to_json(file)}")
    env_doc, ptrs, notes = {}, [], []
    env_layer(schema, "", "CFG", env_doc, ptrs, notes)
    for n in notes:
        print(f"  {n}")
    print(f"  environment {to_json(env_doc)}")
    st = Store(defaults, env_doc, ptrs, schema)
    resp = st.commit(file, [])
    if resp.status != 200:
        raise SystemExit(f"  startup config is invalid: {resp.body}")
    print(f"  effective   {to_json(st.effective)}")
    print(f"  etag {st.etag}")
    return st


def main():
    st = start()
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR)
    listener.listen(8)

    b = request("GET", "/config", "", "")
    b += request("GET", "/config/server/port", "", "")
    b += request("GET", "/config/log/targets/1", "", "")
    b += request("GET", "/config/features", "", "")
    b += request("GET", "/config/nope", "", "")
    b += request("GET", "/config/log/targets/7", "", "")
    b += request("GET", "/config/log/targets/01", "", "")
    b += request("GET", "/config/a~2b", "", "")
    b += request("GET", "/status", "", "")
    b += request("POST", "/config", "", "{}")
    e1 = session(listener, st, "read", b, "")

    b = request("GET", "/config", f"If-None-Match: {e1}\r\n", "")
    b += request("GET", "/config/limits", 'If-None-Match: "00000000"\r\n', "")
    b += request("PATCH", "/config", "Content-Type: application/merge-patch+json\r\n", '{"log": {"level": "warn"}}')
    b += request("PATCH", "/config", patch_headers('"deadbeef"'), '{"log": {"level": "warn"}}')
    b += request("PATCH", "/config", f"If-Match: {e1}\r\nContent-Type: application/json\r\n", '{"log": {"level": "warn"}}')
    b += request("PATCH", "/config", patch_headers(e1), '{"log": {"level": "warn"}, "features": {"regions": null}}')
    b += request("PATCH", "/config", patch_headers(e1), '{"log": {"level": "debug"}}')
    b += request("GET", "/config/features/regions", "", "")
    e2 = session(listener, st, "conditional writes", b, e1)

    b = request("PATCH", "/config", patch_headers(e2), '{"server": {"port": 70000}}')
    b += request("PATCH", "/config", patch_headers(e2), '{"log": {"level": "loud", "targets": ["a", "b", "c", "d"]}, "extra": 1, "server": {"tls": "yes"}}')
    b += request("PATCH", "/config", patch_headers(e2), '{"limits": 5, "features": {"regions": ["eu", "mars"]}}')
    b += request("PATCH", "/config", patch_headers(e2), '{"log": {"level": "warn",}}')
    b += request("PATCH", "/config", patch_headers(e2), '{"limits": {"timeout_ms": 2.5}}')
    b += request("PATCH", "/config", patch_headers(e2), '{"server": {"port": 8443, "tls": true}}')
    e3 = session(listener, st, "validation", b, e2)

    any_tag = if_match("*")
    b = request("PUT", "/config/limits/timeout_ms", any_tag, "5000")
    b += request("PUT", "/config/log/targets/0", any_tag, '"stderr"')
    b += request("PUT", "/config/log/targets/-", any_tag, '"syslog"')
    b += request("PUT", "/config/log/targets/-", any_tag, '"extra"')
    b += request("PUT", "/config/log/targets/5", any_tag, '"x"')
    b += request("PUT", "/config/server/port", any_tag, "1234")
    b += request("DELETE", "/config/server/host", any_tag, "")
    b += request("DELETE", "/config/server/host", any_tag, "")
    b += request("PUT", "/config/features/beta", any_tag, "maybe")
    b += request("PUT", "/config/features/beta", "", "true")
    b += request("PUT", "/config", any_tag, "{}")
    b += request("GET", "/config/server", "", "")
    e4 = session(listener, st, "pointer writes", b, e3)

    operator_edit('{"server": {"port": 7000, "tls": false}, "log": {"level": "error"}}')
    b = request("POST", "/config/reload", "", "")
    b += request("GET", "/config/log", "", "")
    e5 = session(listener, st, "reload", b, e4)
    operator_edit('{"server": {')
    b = request("POST", "/config/reload", "", "")
    b += request("GET", "/config/reload", "", "")
    e6 = session(listener, st, "reload broken file", b, e5)
    operator_edit('{"log": {"level": "verbose"}}')
    b = request("POST", "/config/reload", "", "")
    b += request("GET", "/config", f"If-None-Match: {e6}\r\n", "")
    session(listener, st, "reload invalid file", b, e6)

    print("== on disk")
    with open(FILE) as f:
        print(f"  {f.read()}")
    print(f"== access log (version {st.version})")
    for l in st.log:
        print(f"  {l}")


main()
