# Mirror of source.kara: same commands, same logical clock, same AOF.
import socket

ADDR = ("127.0.0.1", 47732)
AOF = "store.aof"
CLOCK = "store.clock"
I64 = (-(1 << 63), (1 << 63) - 1)

def to_int(s):
    s = s.strip() if False else s
    try:
        if not s or s.strip() != s or s.startswith("+"):
            return None
        n = int(s, 10)
        return n if I64[0] <= n <= I64[1] else None
    except ValueError:
        return None

# replies: ('simple', s) ('error', s) ('int', n) ('bulk', s|None) ('many', [..])
OK = ("simple", "OK")
def err(m): return ("error", f"ERR {m}")
WRONG = ("error", "WRONGTYPE Operation against a key holding the wrong kind of value")
NOT_INT = err("value is not an integer or out of range")

def encode_cmd(args):
    out = f"*{len(args)}\r\n"
    for a in args:
        out += f"${len(a.encode())}\r\n{a}\r\n"
    return out

def encode(r):
    k, v = r
    if k == "simple": return f"+{v}\r\n"
    if k == "error": return f"-{v}\r\n"
    if k == "int": return f":{v}\r\n"
    if k == "bulk":
        return "$-1\r\n" if v is None else f"${len(v.encode())}\r\n{v}\r\n"
    return f"*{len(v)}\r\n" + "".join(encode(x) for x in v)

def glob(p, s):
    if not p: return not s
    if p[0] == "*":
        return any(glob(p[1:], s[i:]) for i in range(len(s) + 1))
    if not s: return False
    if p[0] == "?" or p[0] == s[0]:
        return glob(p[1:], s[1:])
    return False

class Store:
    def __init__(self):
        self.items = {}  # key -> [kind, value, expires_at]
        self.now = 0
        self.log = ""
        self.records = 0

    def live(self, k):
        it = self.items.get(k)
        if it is None: return False
        if it[2] >= 0 and self.now >= it[2]:
            del self.items[k]; return False
        return True

    def purge(self):
        for k in [k for k, it in self.items.items() if it[2] >= 0 and self.now >= it[2]]:
            del self.items[k]

    def record(self, args):
        self.log += encode_cmd(args); self.records += 1

    def text_of(self, k):
        if not self.live(k): return (True, None)
        it = self.items[k]
        if it[0] != "text": return (False, WRONG)
        return (True, it[1])

    def exec(self, args, logging):
        self.now += 1
        if not args: return err("empty command")
        name = args[0].upper(); argc = len(args)
        bad = err(f"wrong number of arguments for '{args[0].lower()}' command")
        if name == "PING":
            return ("simple", "PONG") if argc == 1 else ("bulk", args[1]) if argc == 2 else bad
        if name == "ECHO":
            return ("bulk", args[1]) if argc == 2 else bad
        if name == "SET":
            if argc not in (3, 4): return bad
            nx = argc == 4 and args[3].upper() == "NX"
            if argc == 4 and not nx: return err("syntax error")
            if nx and self.live(args[1]): return ("bulk", None)
            self.items[args[1]] = ["text", args[2], -1]
            if logging: self.record(args)
            return OK
        if name == "GET":
            if argc != 2: return bad
            ok, v = self.text_of(args[1])
            return ("bulk", v) if ok else v
        if name == "STRLEN":
            if argc != 2: return bad
            ok, v = self.text_of(args[1])
            return ("int", len(v.encode()) if v is not None else 0) if ok else v
        if name == "APPEND":
            if argc != 3: return bad
            ok, v = self.text_of(args[1])
            if not ok: return v
            cur = (v or "") + args[2]
            self.items[args[1]] = ["text", cur, -1]
            if logging: self.record(args)
            return ("int", len(cur.encode()))
        if name in ("INCR", "DECR", "INCRBY"):
            if name == "INCRBY":
                if argc != 3: return bad
                by = to_int(args[2])
                if by is None: return NOT_INT
            else:
                if argc != 2: return bad
                by = 1 if name == "INCR" else -1
            ok, v = self.text_of(args[1])
            if not ok: return v
            if v is None: cur = 0
            else:
                cur = to_int(v)
                if cur is None: return NOT_INT
            nxt = cur + by
            if not (I64[0] <= nxt <= I64[1]): return err("increment or decrement would overflow")
            keep = self.items[args[1]][2] if args[1] in self.items else -1
            self.items[args[1]] = ["text", str(nxt), keep]
            if logging: self.record(args)
            return ("int", nxt)
        if name in ("DEL", "EXISTS"):
            if argc < 2: return bad
            n = 0
            for k in args[1:]:
                if self.live(k):
                    n += 1
                    if name == "DEL": del self.items[k]
            if name == "DEL" and n > 0 and logging: self.record(args)
            return ("int", n)
        if name in ("LPUSH", "RPUSH"):
            if argc < 3: return bad
            k = args[1]
            if self.live(k):
                if self.items[k][0] != "list": return WRONG
            else:
                self.items[k] = ["list", [], -1]
            q = self.items[k][1]
            for v in args[2:]:
                if name == "LPUSH": q.insert(0, v)
                else: q.append(v)
            if logging: self.record(args)
            return ("int", len(q))
        if name == "LPOP":
            if argc != 2: return bad
            if not self.live(args[1]): return ("bulk", None)
            it = self.items[args[1]]
            if it[0] != "list": return WRONG
            v = it[1].pop(0) if it[1] else None
            if not it[1]: del self.items[args[1]]
            if logging: self.record(args)
            return ("bulk", v)
        if name == "LLEN":
            if argc != 2: return bad
            if not self.live(args[1]): return ("int", 0)
            it = self.items[args[1]]
            return ("int", len(it[1])) if it[0] == "list" else WRONG
        if name == "LRANGE":
            if argc != 4: return bad
            a, b = to_int(args[2]), to_int(args[3])
            if a is None or b is None: return NOT_INT
            if not self.live(args[1]): return ("many", [])
            it = self.items[args[1]]
            if it[0] != "list": return WRONG
            q = it[1]; n = len(q)
            if a < 0: a = n + a
            if b < 0: b = n + b
            if a < 0: a = 0
            if b >= n: b = n - 1
            return ("many", [("bulk", q[i]) for i in range(a, b + 1)])
        if name in ("EXPIRE", "EXPIREAT"):
            if argc != 3: return bad
            n = to_int(args[2])
            if n is None: return NOT_INT
            if not self.live(args[1]): return ("int", 0)
            at = self.now + n if name == "EXPIRE" else n
            self.items[args[1]][2] = at
            if logging: self.record(["EXPIREAT", args[1], str(at)])
            return ("int", 1)
        if name == "TTL":
            if argc != 2: return bad
            if not self.live(args[1]): return ("int", -2)
            at = self.items[args[1]][2]
            return ("int", -1 if at < 0 else at - self.now)
        if name == "TYPE":
            if argc != 2: return bad
            if not self.live(args[1]): return ("simple", "none")
            return ("simple", "string" if self.items[args[1]][0] == "text" else "list")
        if name == "KEYS":
            if argc != 2: return bad
            self.purge()
            return ("many", [("bulk", k) for k in sorted(self.items, key=lambda s: s.encode()) if glob(args[1], k)])
        if name == "DBSIZE":
            self.purge(); return ("int", len(self.items))
        if name == "FLUSHALL":
            self.items.clear()
            if logging: self.record(args)
            return OK
        return err(f"unknown command '{args[0]}'")

class Parser:
    def __init__(self, text):
        self.text = text.encode(); self.at = 0
    def line(self):
        i = self.text.find(b"\r\n", self.at)
        if i < 0: raise ValueError("Protocol error: unexpected end of input")
        l = self.text[self.at:i].decode(); self.at = i + 2
        return l
    def next(self):
        if self.at >= len(self.text): return None
        head = self.line()
        if not head.startswith("*"):
            return head.split()
        n = to_int(head[1:])
        if n is None: raise ValueError("Protocol error: invalid multibulk length")
        args = []
        for _ in range(n):
            h = self.line()
            if not h.startswith("$"): raise ValueError(f"Protocol error: expected '$', got '{h}'")
            ln = to_int(h[1:])
            if ln is None: raise ValueError("Protocol error: invalid bulk length")
            if self.at + ln + 2 > len(self.text): raise ValueError("Protocol error: unexpected end of input")
            args.append(self.text[self.at:self.at + ln].decode()); self.at += ln + 2
        return args

def serve(store, raw):
    p = Parser(raw); out = ""
    while True:
        try:
            args = p.next()
        except ValueError as e:
            out += encode(("error", f"ERR {e}")); break
        if args is None: break
        out += encode(store.exec(args, True))
    return out

def show(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"').replace("\r", "\\r").replace("\n", "\\n") + '"'

def show_arg(s):
    return show(s) if any(c in s for c in ' "\r\n') else s

def decode(text, at):
    b = text
    eol = b.find(b"\r\n", at)
    body = b[at + 1:eol].decode(); nxt = eol + 2
    t = chr(b[at])
    if t == "+": return ("simple", body), nxt
    if t == "-": return ("error", body), nxt
    n = int(body)
    if t == ":": return ("int", n), nxt
    if t == "$":
        if n < 0: return ("bulk", None), nxt
        return ("bulk", b[nxt:nxt + n].decode()), nxt + n + 2
    items = []; pos = nxt
    for _ in range(n):
        r, pos = decode(text, pos); items.append(r)
    return ("many", items), pos

def print_reply(r):
    k, v = r
    if k == "simple": print(v)
    elif k == "error": print(f"(error) {v}")
    elif k == "int": print(f"(integer) {v}")
    elif k == "bulk": print("(nil)" if v is None else show(v))
    else:
        if not v: print("(empty array)")
        for i, it in enumerate(v, 1):
            print(f"{i}) {show(it[1])}" if it[0] == "bulk" and it[1] is not None else f"{i}) ?")

def read_all(s):
    data = b""
    while True:
        c = s.recv(64)
        if not c: return data
        data += c

def session(listener, store, label, cmds, wire):
    print(f"== {label}")
    c = socket.create_connection(ADDR)
    c.sendall(wire.encode()); c.shutdown(socket.SHUT_WR)
    conn, _ = listener.accept()
    out = serve(store, read_all(conn).decode())
    conn.sendall(out.encode()); conn.close()
    resp = read_all(c); c.close()
    at = 0; i = 0
    while at < len(resp):
        r, at = decode(resp, at)
        if i < len(cmds):
            print("> " + " ".join(show_arg(a) for a in cmds[i]))
        print_reply(r); i += 1
    if store.log:
        with open(AOF, "a") as f: f.write(store.log)
        store.log = ""
    open(CLOCK, "w").write(str(store.now))

def resp_session(listener, store, label, lines, extra):
    cmds = [l.split() for l in lines]
    session(listener, store, label, cmds, "".join(encode_cmd(c) for c in cmds) + extra)

def restart():
    s = Store()
    text = open(AOF, newline="").read()
    p = Parser(text); n = 0
    while True:
        args = p.next()
        if args is None: break
        s.exec(args, False); n += 1
    t = to_int(open(CLOCK).read().strip())
    s.now = t if t is not None else 0
    print(f"replayed {n} records ({len(text.encode())} bytes), clock {s.now}")
    return s

def main():
    listener = socket.socket(); listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR); listener.listen(16)
    store = Store()
    open(AOF, "w").write("")
    resp_session(listener, store, "basics", [
        "PING", "SET greeting hello", "GET greeting", "GET missing",
        "APPEND greeting _world", "STRLEN greeting", "SET n 41", "INCR n",
        "INCRBY n -50", "DECR n", "INCR greeting", "EXISTS greeting n nope",
        "INCRBY n 9223372036854775807", "INCRBY n 10", "GET n"], "")
    resp_session(listener, store, "lists", [
        "RPUSH q a b c", "LPUSH q z", "LRANGE q 0 -1", "LRANGE q 1 2",
        "LPOP q", "LLEN q", "TYPE q", "GET q", "TYPE nope", "LRANGE nope 0 -1",
        "RPUSH greeting x"], "")
    resp_session(listener, store, "expiry", [
        "SET temp 1", "EXPIRE temp 3", "TTL temp", "GET temp", "PING",
        "GET temp", "TTL temp", "TTL greeting", "SET keep yes", "EXPIRE keep 100"], "")
    inline = ["SET user:1 ann", "SET user:2 bob", "SET admin x", "KEYS user:*",
              "KEYS *", "KEYS ?dmin", "DBSIZE", "DEL user:1 user:9", "FROB x", "GET"]
    session(listener, store, "inline", [l.split() for l in inline], "".join(l + "\r\n" for l in inline))
    multi = ["SET", "multi", "line one\r\nline two"]
    bin_ = [multi, "GET multi".split(), "STRLEN multi".split(), "SET once 1 NX".split(),
            "SET once 2 NX".split(), "GET once".split(), "SET once 3 XX".split()]
    session(listener, store, "binary-safe", bin_, "".join(encode_cmd(c) for c in bin_))
    resp_session(listener, store, "truncated", ["PING", "SET last ok"], "*2\r\n$3\r\nGET\r\n$5\r\nab")
    print(f"== restart ({store.records} records logged, {len(store.items)} keys in memory)")
    fresh = restart()
    resp_session(listener, fresh, "after restart", [
        "DBSIZE", "KEYS *", "GET greeting", "GET n", "LRANGE q 0 -1", "GET multi",
        "TTL keep", "GET temp", "GET once", "GET last"], "")

main()
