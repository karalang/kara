# Mirror of log_shipper.kara: a log shipper in the shape of Vector or
# Fluent Bit, tailing real files and posting batches to a real socket.
import os, socket

PORT = 47746
BATCH = 4
MAX_ATTEMPTS = 4
MAX_MSG = 80

CRC = []
for n in range(256):
    c = n
    for _ in range(8):
        c = (0xEDB88320 ^ (c >> 1)) if c & 1 else c >> 1
    CRC.append(c)

def crc32(b):
    c = 0xFFFFFFFF
    for x in b:
        c = CRC[(c ^ x) & 0xFF] ^ (c >> 8)
    return c ^ 0xFFFFFFFF

# ------------------------------------------------------------ values

class Val:
    def __init__(self, kind, text):
        self.kind = kind  # "s", "n", "b", "null"
        self.text = text

def json_str(s):
    out = '"'
    for ch in s:
        o = ord(ch)
        if ch == '"': out += '\\"'
        elif ch == '\\': out += '\\\\'
        elif ch == '\n': out += '\\n'
        elif ch == '\t': out += '\\t'
        elif o < 0x20: out += f"\\u{o:04x}"
        else: out += ch
    return out + '"'

def render(rec):
    parts = []
    for k in sorted(rec):
        v = rec[k]
        if v.kind == "s":
            t = json_str(v.text)
        else:
            t = v.text
        parts.append(f"{json_str(k)}:{t}")
    return "{" + ",".join(parts) + "}"

# ------------------------------------------------------------ parsers

class JsonLines:
    def name(self):
        return "json"

    def parse(self, line):
        p = [0]
        s = line
        def ws():
            while p[0] < len(s) and s[p[0]] in " \t":
                p[0] += 1
        def string():
            if p[0] >= len(s) or s[p[0]] != '"':
                raise ValueError(f"expected string at col {p[0] + 1}")
            p[0] += 1
            out = ""
            while True:
                if p[0] >= len(s):
                    raise ValueError("unterminated string")
                c = s[p[0]]
                if c == '"':
                    p[0] += 1
                    return out
                if c == "\\":
                    if p[0] + 1 >= len(s):
                        raise ValueError("unterminated string")
                    e = s[p[0] + 1]
                    if e == "u":
                        h = s[p[0] + 2:p[0] + 6]
                        if len(h) != 4 or any(x not in "0123456789abcdefABCDEF" for x in h):
                            raise ValueError(f"bad \\u escape at col {p[0] + 1}")
                        out += chr(int(h, 16))
                        p[0] += 6
                        continue
                    m = {'"': '"', "\\": "\\", "/": "/", "n": "\n", "t": "\t", "r": "\r"}
                    if e not in m:
                        raise ValueError(f"bad escape at col {p[0] + 1}")
                    out += m[e]
                    p[0] += 2
                    continue
                out += c
                p[0] += 1
        rec = {}
        ws()
        if p[0] >= len(s) or s[p[0]] != "{":
            raise ValueError("expected '{' at col 1")
        p[0] += 1
        ws()
        if p[0] < len(s) and s[p[0]] == "}":
            p[0] += 1
        else:
            while True:
                ws()
                k = string()
                ws()
                if p[0] >= len(s) or s[p[0]] != ":":
                    raise ValueError(f"expected ':' at col {p[0] + 1}")
                p[0] += 1
                ws()
                if p[0] < len(s) and s[p[0]] == '"':
                    rec[k] = Val("s", string())
                else:
                    st = p[0]
                    while p[0] < len(s) and s[p[0]] not in ",} \t":
                        p[0] += 1
                    word = s[st:p[0]]
                    if word in ("true", "false"):
                        rec[k] = Val("b", word)
                    elif word == "null":
                        rec[k] = Val("null", word)
                    elif is_number(word):
                        rec[k] = Val("n", word)
                    else:
                        raise ValueError(f"bad value at col {st + 1}")
                ws()
                if p[0] < len(s) and s[p[0]] == ",":
                    p[0] += 1
                    continue
                if p[0] < len(s) and s[p[0]] == "}":
                    p[0] += 1
                    break
                raise ValueError(f"expected ',' or '}}' at col {p[0] + 1}")
        ws()
        if p[0] != len(s):
            raise ValueError(f"trailing data at col {p[0] + 1}")
        return rec

def is_number(w):
    i = 0
    if i < len(w) and w[i] == "-":
        i += 1
    st = i
    while i < len(w) and w[i].isdigit():
        i += 1
    if i == st:
        return False
    if i < len(w) and w[i] == ".":
        i += 1
        st = i
        while i < len(w) and w[i].isdigit():
            i += 1
        if i == st:
            return False
    return i == len(w)

MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]

class Combined:
    # 10.0.0.5 - - [15/Oct/2026:05:00:01 +0000] "GET /x HTTP/1.1" 200 512 "-" "curl/8.4"
    def name(self):
        return "combined"

    def parse(self, line):
        sp = line.find(" ")
        if sp <= 0:
            raise ValueError("no client address")
        client = line[:sp]
        lb = line.find("[")
        rb = line.find("]")
        if lb < 0 or rb < lb:
            raise ValueError("no [time]")
        t = line[lb + 1:rb]
        # 15/Oct/2026:05:00:01 +0000
        if len(t) != 26 or t[2] != "/" or t[6] != "/" or t[11] != ":":
            raise ValueError(f"bad time '{t}'")
        if t[3:6] not in MONTHS:
            raise ValueError(f"bad month '{t[3:6]}'")
        ts = f"{t[7:11]}-{MONTHS.index(t[3:6]) + 1:02}-{t[0:2]}T{t[12:20]}Z"
        rest = line[rb + 1:]
        q1 = rest.find('"')
        q2 = rest.find('"', q1 + 1)
        if q1 < 0 or q2 < 0:
            raise ValueError("no request")
        req = rest[q1 + 1:q2].split(" ")
        if len(req) != 3:
            raise ValueError("bad request line")
        nums = rest[q2 + 1:].strip().split(" ")
        if len(nums) < 2 or not nums[0].isdigit() or not (nums[1].isdigit() or nums[1] == "-"):
            raise ValueError("bad status or size")
        tail = rest[q2 + 1:]
        quoted = []
        i = 0
        while True:
            a = tail.find('"', i)
            if a < 0:
                break
            b = tail.find('"', a + 1)
            if b < 0:
                raise ValueError("unterminated quote")
            quoted.append(tail[a + 1:b])
            i = b + 1
        rec = {
            "client": Val("s", client),
            "ts": Val("s", ts),
            "method": Val("s", req[0]),
            "path": Val("s", req[1]),
            "status": Val("n", nums[0]),
            "bytes": Val("n", nums[1] if nums[1] != "-" else "0"),
        }
        if len(quoted) >= 2:
            if quoted[0] != "-":
                rec["referer"] = Val("s", quoted[0])
            rec["agent"] = Val("s", quoted[1])
        return rec

# ------------------------------------------------------------ transforms

def make_transforms(host):
    def drop_health(r):
        p = r.get("path")
        if p is not None and (p.text == "/healthz" or p.text.startswith("/healthz?")):
            return None
        return r
    def norm_level(r):
        lv = r.get("level")
        if lv is not None:
            t = lv.text.lower()
            if t == "warning":
                t = "warn"
            r["level"] = Val("s", t)
        return r
    def redact(r):
        u = r.get("user")
        if u is not None and "@" in u.text:
            at = u.text.find("@")
            r["user"] = Val("s", u.text[:1] + "***" + u.text[at:])
        c = r.get("client")
        if c is not None:
            parts = c.text.split(".")
            if len(parts) == 4:
                r["client"] = Val("s", ".".join(parts[:3]) + ".0")
        return r
    def add_host(r):
        r["host"] = Val("s", host)
        return r
    return [("drop_health", drop_health), ("norm_level", norm_level), ("redact", redact), ("add_host", add_host)]

# ------------------------------------------------------------ files

def read_from(path, offset):
    with open(path, "rb") as f:
        f.seek(offset)
        return f.read()

def fingerprint(path):
    data = open(path, "rb").read()
    nl = data.find(b"\n")
    if nl < 0:
        return ""
    return f"{crc32(data[:nl + 1]):08x}"

def load_checkpoints():
    cps = {}
    if not os.path.exists("state/checkpoints"):
        return cps
    for ln in open("state/checkpoints").read().splitlines():
        name, fp, off = ln.split("\t")
        cps[name] = (fp, int(off))
    return cps

def save_checkpoints(cps):
    os.makedirs("state", exist_ok=True)
    text = "".join(f"{k}\t{cps[k][0]}\t{cps[k][1]}\n" for k in sorted(cps))
    with open("state/checkpoints.tmp", "w") as f:
        f.write(text)
    os.rename("state/checkpoints.tmp", "state/checkpoints")

# ------------------------------------------------------------ collector

class Collector:
    def __init__(self, listener):
        self.listener = listener
        self.requests = 0
        self.stored = []
        self.keys = set()
        self.dups = 0
        self.script = {2: (503, "3"), 7: (503, ""), 8: (503, ""), 17: (503, ""), 18: (503, ""), 19: (503, ""), 20: (503, "")}

    def handle(self, data):
        end = data.find(b"\r\n\r\n")
        head = data[:end].decode().split("\r\n")
        body = data[end + 4:].decode()
        self.requests += 1
        n = self.requests
        if not head[0].startswith("POST /ingest "):
            return 404, "", "not found"
        if n in self.script:
            st, ra = self.script[n]
            return st, ra, "overloaded"
        lines = body.split("\n")[:-1]
        for ln in lines:
            if msg_len(ln.encode()) > MAX_MSG:
                return 413, "", "record too large"
        new = 0
        for ln in lines:
            fp = field_of(ln, "_fp")
            off = field_of(ln, "_off")
            key = fp + ":" + off
            if key in self.keys:
                self.dups += 1
                continue
            self.keys.add(key)
            self.stored.append(ln)
            new += 1
        return 200, "", f"stored {new} of {len(lines)}"

def msg_len(b):
    # Encoded bytes of the "msg" string, escapes counted as written.
    m = b.find(b'"msg":"')
    if m < 0:
        return 0
    i = m + 7
    while i < len(b) and b[i] != 34:
        i += 2 if b[i] == 92 else 1
    return i - (m + 7)

def field_of(ln, k):
    m = ln.find(f'"{k}":')
    st = m + len(k) + 3
    e = st
    while e < len(ln) and ln[e] not in ",}":
        e += 1
    return ln[st:e].strip('"')

REASON = {200: "OK", 404: "Not Found", 413: "Content Too Large", 503: "Service Unavailable"}

def post(col, ndjson):
    client = socket.create_connection(("127.0.0.1", PORT))
    body = ndjson.encode()
    req = f"POST /ingest HTTP/1.1\r\nHost: collector\r\nContent-Type: application/x-ndjson\r\nContent-Length: {len(body)}\r\n\r\n".encode() + body
    client.sendall(req)
    client.shutdown(socket.SHUT_WR)
    conn, _ = col.listener.accept()
    data = b""
    while True:
        c = conn.recv(64)
        if not c:
            break
        data += c
    st, ra, msg = col.handle(data)
    resp = f"HTTP/1.1 {st} {REASON[st]}\r\n"
    if ra:
        resp += f"Retry-After: {ra}\r\n"
    resp += f"Content-Length: {len(msg)}\r\n\r\n{msg}"
    conn.sendall(resp.encode())
    conn.close()
    got = b""
    while True:
        c = client.recv(64)
        if not c:
            break
        got += c
    client.close()
    lines = got.decode().split("\r\n")
    status = int(lines[0].split(" ")[1])
    retry = -1
    for ln in lines[1:]:
        if ln.startswith("Retry-After: "):
            retry = int(ln[13:])
    return status, retry, got.decode().split("\r\n\r\n", 1)[1]

# ------------------------------------------------------------ shipper

class Item:
    def __init__(self, rec, fp, off, end):
        self.rec = rec
        self.fp = fp
        self.off = off
        self.end = end

class Shipper:
    def __init__(self, col, host):
        self.col = col
        self.transforms = make_transforms(host)
        self.cps = load_checkpoints()
        self.clock = 0
        self.batches = 0
        self.dead = 0
        self.crash_after = -1
        self.crashed = False

    def deliver(self, items, label):
        # Returns True once the items are acked or dead-lettered.
        attempt = 1
        while True:
            body = "".join(render(it.rec) + "\n" for it in items)
            st, retry, msg = post(self.col, body)
            if st == 200:
                print(f"    {label}: {len(items)} records -> 200 {msg}")
                return True
            if st == 413:
                if len(items) == 1:
                    it = items[0]
                    with open("dead/letters.ndjson", "a") as f:
                        f.write(render(it.rec) + "\n")
                    self.dead += 1
                    print(f"    {label}: 413 on a single record, dead-lettered @{it.off}")
                    return True
                half = (len(items) + 1) // 2
                print(f"    {label}: 413, splitting {len(items)} -> {half}+{len(items) - half}")
                self.deliver(items[:half], label + "a")
                self.deliver(items[half:], label + "b")
                return True
            if attempt >= MAX_ATTEMPTS:
                with open("dead/letters.ndjson", "a") as f:
                    for it in items:
                        f.write(render(it.rec) + "\n")
                self.dead += len(items)
                print(f"    {label}: {st} after {attempt} attempts, dead-lettered {len(items)} records")
                return True
            wait = retry if retry >= 0 else (1 << (attempt - 1))
            self.clock += wait
            print(f"    {label}: {st}, attempt {attempt}, retry in {wait}s (t={self.clock}s)")
            attempt += 1

    def flush(self, name, fp, items, upto):
        self.batches += 1
        self.deliver(items, f"batch {self.batches}")
        if self.batches == self.crash_after:
            print(f"    crash after the ack of batch {self.batches}, before its checkpoint")
            self.crashed = True
            return
        self.cps[name] = (fp, upto)
        save_checkpoints(self.cps)

    def drain(self, name, path, parser, fp, offset):
        data = read_from(path, offset)
        pos = 0
        items = []
        consumed = offset
        while True:
            nl = data.find(b"\n", pos)
            if nl < 0:
                break
            line_off = offset + pos
            line = data[pos:nl].decode()
            pos = nl + 1
            consumed = offset + pos
            if line.strip() == "":
                continue
            try:
                rec = parser.parse(line)
            except ValueError as e:
                print(f"    {name}@{line_off}: {parser.name()} parse error: {e}")
                continue
            rec["_fp"] = Val("s", fp)
            rec["_off"] = Val("n", str(line_off))
            rec["_src"] = Val("s", name)
            dropped = ""
            for tname, t in self.transforms:
                rec = t(rec)
                if rec is None:
                    dropped = tname
                    break
            if dropped:
                print(f"    {name}@{line_off}: dropped by {dropped}")
                continue
            items.append(Item(rec, fp, line_off, consumed))
            if len(items) == BATCH:
                self.flush(name, fp, items, consumed)
                if self.crashed:
                    return
                items = []
        if items:
            self.flush(name, fp, items, consumed)
            if self.crashed:
                return
        elif consumed != self.cps.get(name, ("", -1))[1] or self.cps.get(name, ("", 0))[0] != fp:
            self.cps[name] = (fp, consumed)
            save_checkpoints(self.cps)
        if pos < len(data):
            print(f"    {name}: holding {len(data) - pos} bytes of a partial line")

    def run(self, inputs):
        for name, path, parser in inputs:
            if self.crashed:
                return
            if not os.path.exists(path):
                print(f"  {name}: missing")
                continue
            fp = fingerprint(path)
            if fp == "":
                print(f"  {name}: no complete first line yet, skipped")
                continue
            size = os.path.getsize(path)
            old = self.cps.get(name)
            if old is None:
                print(f"  {name}: new file {fp}, reading from 0")
                self.drain(name, path, parser, fp, 0)
                continue
            ofp, off = old
            if ofp == fp and size >= off:
                if size == off:
                    print(f"  {name}: up to date at {off}")
                    continue
                print(f"  {name}: resuming {fp} at {off} of {size}")
                self.drain(name, path, parser, fp, off)
                continue
            rotated = path + ".1"
            if ofp != fp and os.path.exists(rotated) and fingerprint(rotated) == ofp:
                rsize = os.path.getsize(rotated)
                print(f"  {name}: rotated; draining {rotated} ({ofp}) from {off} of {rsize}")
                self.drain(name, rotated, parser, ofp, off)
                if self.crashed:
                    return
            elif ofp == fp:
                print(f"  {name}: truncated ({size} < {off}), reading from 0")
            else:
                print(f"  {name}: replaced ({ofp} -> {fp}), reading from 0")
            print(f"  {name}: new file {fp}, reading from 0")
            self.drain(name, path, parser, fp, 0)

# ------------------------------------------------------------ the scenario

def append(path, text):
    with open(path, "a") as f:
        f.write(text)

APP1 = [
    '{"ts":"2026-10-15T05:00:01Z","level":"info","msg":"service started","port":8080}',
    '{"ts":"2026-10-15T05:00:02Z","level":"INFO","msg":"user login","user":"alice@example.com","ok":true}',
    '{"ts":"2026-10-15T05:00:03Z","level":"WARNING","msg":"slow query","ms":812.5}',
    'this is not json',
    '{"ts":"2026-10-15T05:00:04Z","level":"error","msg":"payment failed: \\"card declined\\"","user":"bob@example.com","code":null}',
    '{"ts":"2026-10-15T05:00:05Z","level":"debug","msg":"caf\\u00e9 menu loaded","items":12}',
    '{"ts":"2026-10-15T05:00:06Z","level":"info","msg":"' + "x" * 100 + '"}',
    '{"ts":"2026-10-15T05:00:07Z","level":"info","msg":"cache warm","hit_rate":0.93}',
    '{"ts":"2026-10-15T05:00:08Z","level":"info" "msg":"missing comma"}',
    '{"ts":"2026-10-15T05:00:09Z","level":"info","msg":"tab\\there","n":-4}',
    '{"ts":"2026-10-15T05:00:10Z","level":"warn","msg":"retrying upstream","attempt":2}',
]

ACC1 = [
    '10.0.0.5 - - [15/Oct/2026:05:00:01 +0000] "GET /api/items?page=2 HTTP/1.1" 200 512 "-" "curl/8.4"',
    '10.0.0.6 - - [15/Oct/2026:05:00:02 +0000] "GET /healthz HTTP/1.1" 200 2 "-" "kube-probe/1.30"',
    '192.168.1.20 - - [15/Oct/2026:05:00:03 +0000] "POST /api/orders HTTP/1.1" 201 87 "https://shop.example/cart" "Mozilla/5.0"',
    '10.0.0.5 - - [15/Oct/2026:05:00:04 +0000] "GET /static/app.js HTTP/1.1" 304 - "-" "Mozilla/5.0"',
    'garbage line without brackets',
    '10.0.0.7 - - [15/Foo/2026:05:00:05 +0000] "GET / HTTP/1.1" 200 10 "-" "x"',
    '10.0.0.6 - - [15/Oct/2026:05:00:06 +0000] "GET /healthz?full=1 HTTP/1.1" 200 2 "-" "kube-probe/1.30"',
]
ACC_PARTIAL = '10.0.0.8 - - [15/Oct/2026:05:00:07 +0000] "DELETE /api/orders/9 HTTP/1.1" 204 0 "-" "c'

def main():
    os.makedirs("logs", exist_ok=True)
    os.makedirs("dead", exist_ok=True)
    with open("logs/app.log", "w") as f:
        f.write("".join(l + "\n" for l in APP1))
    with open("logs/access.log", "w") as f:
        f.write("".join(l + "\n" for l in ACC1) + ACC_PARTIAL)
    print(f"== logs: app.log {os.path.getsize('logs/app.log')} bytes, access.log {os.path.getsize('logs/access.log')} bytes")

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", PORT))
    listener.listen(8)
    col = Collector(listener)
    inputs = [("app.log", "logs/app.log", JsonLines()), ("access.log", "logs/access.log", Combined())]

    print("== run 1 (crashes after batch 4)")
    s = Shipper(col, "web-1")
    s.crash_after = 4
    s.run(inputs)
    print(f"  checkpoints: {open('state/checkpoints').read().strip().replace(chr(10), ' | ')}")

    append("logs/app.log", '{"ts":"2026-10-15T05:01:00Z","level":"info","msg":"before rotation"}\n')
    os.rename("logs/app.log", "logs/app.log.1")
    with open("logs/app.log", "w") as f:
        f.write('{"ts":"2026-10-15T05:01:01Z","level":"info","msg":"after rotation","gen":2}\n')
        f.write('{"ts":"2026-10-15T05:01:02Z","level":"Warning","msg":"disk 91% full","user":"ops@example.com"}\n')
    append("logs/access.log", 'url/8.4"\n10.0.0.9 - - [15/Oct/2026:05:01:03 +0000] "GET /api/items HTTP/1.1" 200 2048 "-" "curl/8.4"\n')

    print("== run 2 (after rotation and appends)")
    s = Shipper(col, "web-1")
    s.run(inputs)
    print(f"  checkpoints: {open('state/checkpoints').read().strip().replace(chr(10), ' | ')}")

    print("== run 3 (nothing new)")
    s = Shipper(col, "web-1")
    s.run(inputs)

    with open("logs/access.log", "w") as f:
        f.write('10.0.0.5 - - [15/Oct/2026:05:02:00 +0000] "GET /after/truncate HTTP/1.1" 200 1 "-" "curl/8.4"\n')
    with open("logs/app.log", "a") as f:
        f.write('{"ts":"2026-10-15T05:02:01Z","level":"info","msg":"steady"}\n')
    print("== run 4 (access.log copytruncated, app.log appended)")
    s = Shipper(col, "web-2")
    s.run(inputs)
    print(f"  checkpoints: {open('state/checkpoints').read().strip().replace(chr(10), ' | ')}")
    listener.close()

    print("== collector")
    print(f"  {col.requests} requests, {len(col.stored)} records stored, {col.dups} duplicates ignored")
    by = {}
    for ln in col.stored:
        k = field_of(ln, "_src")
        by[k] = by.get(k, 0) + 1
    print("  by source: " + ", ".join(f"{k}={by[k]}" for k in sorted(by)))
    for ln in col.stored:
        if '"error"' in ln or "caf" in ln or "/api/orders" in ln or "rotation" in ln or "steady" in ln:
            print(f"  {ln}")
    dead = open("dead/letters.ndjson").read().splitlines()
    print(f"== dead letters: {len(dead)}")
    for ln in dead:
        print(f"  {ln[:90]}")

main()
