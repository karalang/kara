# Oracle for pg_wire.kara: the same PostgreSQL wire-protocol (v3) server and
# scripted clients, written with socket, struct and threads. The server
# speaks the startup handshake with cleartext password authentication and
# the simple query protocol over one in-memory table, with transactions.
# Run: python3 -I -B pg_wire.py > expected.out
import socket
import struct
import threading

ADDR = ("127.0.0.1", 47781)
SESSIONS = 4
PASSWORDS = {"alice": "wonder", "bob": "builder"}
INT4, TEXT = 23, 25
COLUMNS = [("id", INT4), ("name", TEXT), ("qty", INT4)]


class State:
    def __init__(self):
        self.rows = [[1, "apple", 5], [2, "banana", 12], [3, "cherry", 0]]
        self.lock = threading.Lock()
        self.next_pid = 1001
        self.queries = 0


# ------------------------------------------------------------ framing

def msg(tag, body):
    return tag + struct.pack(">i", len(body) + 4) + body


def cstr(s):
    return s.encode() + b"\0"


def error(code, text):
    return msg(b"E", b"S" + cstr("ERROR") + b"C" + cstr(code) + b"M" + cstr(text) + b"\0")


def ready(status):
    return msg(b"Z", status.encode())


def row_description(cols):
    body = struct.pack(">h", len(cols))
    for name, oid in cols:
        size = 4 if oid == INT4 else -1
        body += cstr(name) + struct.pack(">ihihih", 0, 0, oid, size, -1, 0)
    return msg(b"T", body)


def data_row(values):
    body = struct.pack(">h", len(values))
    for v in values:
        if v is None:
            body += struct.pack(">i", -1)
        else:
            b = str(v).encode()
            body += struct.pack(">i", len(b)) + b
    return msg(b"D", body)


def complete(tag):
    return msg(b"C", cstr(tag))


def read_exact(f, n):
    b = f.read(n)
    if len(b) < n:
        raise EOFError
    return b


# ------------------------------------------------------------ SQL subset

class SqlError(Exception):
    def __init__(self, code, text):
        self.code = code
        self.text = text


def tokens(sql):
    out = []
    i = 0
    while i < len(sql):
        c = sql[i]
        if c.isspace():
            i += 1
        elif c == "'":
            j = i + 1
            text = ""
            while True:
                if j >= len(sql):
                    raise SqlError("42601", "unterminated quoted string")
                if sql[j] == "'":
                    if j + 1 < len(sql) and sql[j + 1] == "'":
                        text += "'"
                        j += 2
                        continue
                    break
                text += sql[j]
                j += 1
            out.append(("str", text))
            i = j + 1
        elif c.isdigit() or (c == "-" and i + 1 < len(sql) and sql[i + 1].isdigit()):
            j = i + 1
            while j < len(sql) and sql[j].isdigit():
                j += 1
            out.append(("num", int(sql[i:j])))
            i = j
        elif c.isalpha() or c == "_":
            j = i
            while j < len(sql) and (sql[j].isalnum() or sql[j] == "_"):
                j += 1
            out.append(("word", sql[i:j].lower()))
            i = j
        elif sql[i:i + 2] in ("<=", ">=", "<>", "!="):
            out.append(("op", sql[i:i + 2]))
            i += 2
        elif c in "=<>,()*":
            out.append(("op", c))
            i += 1
        else:
            raise SqlError("42601", f'syntax error at or near "{c}"')
    return out


class Parser:
    def __init__(self, toks):
        self.toks = toks
        self.pos = 0

    def peek(self):
        return self.toks[self.pos] if self.pos < len(self.toks) else ("end", "")

    def next(self):
        t = self.peek()
        self.pos += 1
        return t

    def near(self):
        t = self.peek()
        return "end of input" if t[0] == "end" else f'"{t[1]}"'

    def expect_word(self, w):
        t = self.next()
        if t != ("word", w):
            self.pos -= 1
            raise SqlError("42601", f"syntax error at or near {self.near()}")

    def expect_op(self, o):
        t = self.next()
        if t != ("op", o):
            self.pos -= 1
            raise SqlError("42601", f"syntax error at or near {self.near()}")

    def table(self):
        t = self.next()
        if t[0] != "word":
            self.pos -= 1
            raise SqlError("42601", f"syntax error at or near {self.near()}")
        if t[1] != "items":
            raise SqlError("42P01", f'relation "{t[1]}" does not exist')

    def column(self):
        t = self.next()
        if t[0] != "word":
            self.pos -= 1
            raise SqlError("42601", f"syntax error at or near {self.near()}")
        names = [c[0] for c in COLUMNS]
        if t[1] not in names:
            raise SqlError("42703", f'column "{t[1]}" does not exist')
        return names.index(t[1])

    def value(self):
        t = self.next()
        if t[0] in ("num", "str"):
            return t[1]
        if t == ("word", "null"):
            return None
        self.pos -= 1
        raise SqlError("42601", f"syntax error at or near {self.near()}")

    def where(self):
        if self.peek() != ("word", "where"):
            return None
        self.next()
        col = self.column()
        t = self.next()
        if t[0] != "op" or t[1] not in ("=", "<", ">", "<=", ">=", "<>", "!="):
            self.pos -= 1
            raise SqlError("42601", f"syntax error at or near {self.near()}")
        return (col, t[1], self.value())

    def done(self):
        if self.peek()[0] != "end":
            raise SqlError("42601", f"syntax error at or near {self.near()}")


def matches(row, cond):
    if cond is None:
        return True
    col, op, v = cond
    x = row[col]
    if x is None or v is None:
        return False
    if type(x) != type(v):
        raise SqlError("42883", f"operator does not exist: {type_name(x)} {op} {type_name(v)}")
    return {
        "=": x == v, "<": x < v, ">": x > v, "<=": x <= v, ">=": x >= v,
        "<>": x != v, "!=": x != v,
    }[op]


def type_name(v):
    return "integer" if isinstance(v, int) else "text"


def check_types(values):
    for (name, oid), v in zip(COLUMNS, values):
        if v is None:
            continue
        if oid == INT4 and not isinstance(v, int):
            raise SqlError("22P02", f'invalid input syntax for type integer: "{v}"')
        if oid == TEXT and not isinstance(v, str):
            raise SqlError("42804", f'column "{name}" is of type text but expression is of type integer')


# Runs one statement against `rows`; returns the messages to send.
def execute(rows, sql):
    p = Parser(tokens(sql))
    verb = p.next()
    if verb == ("word", "select"):
        if p.peek() == ("op", "*"):
            p.next()
            cols = list(range(len(COLUMNS)))
        else:
            cols = [p.column()]
            while p.peek() == ("op", ","):
                p.next()
                cols.append(p.column())
        p.expect_word("from")
        p.table()
        cond = p.where()
        order = None
        if p.peek() == ("word", "order"):
            p.next()
            p.expect_word("by")
            order = p.column()
        p.done()
        hits = [r for r in rows if matches(r, cond)]
        if order is not None:
            hits.sort(key=lambda r: (r[order] is None, r[order] if r[order] is not None else 0))
        out = [row_description([COLUMNS[c] for c in cols])]
        for r in hits:
            out.append(data_row([r[c] for c in cols]))
        out.append(complete(f"SELECT {len(hits)}"))
        return out
    if verb == ("word", "insert"):
        p.expect_word("into")
        p.table()
        p.expect_word("values")
        p.expect_op("(")
        values = [p.value()]
        while p.peek() == ("op", ","):
            p.next()
            values.append(p.value())
        p.expect_op(")")
        p.done()
        if len(values) != len(COLUMNS):
            raise SqlError("42601", "INSERT has more expressions than target columns"
                           if len(values) > len(COLUMNS) else
                           "INSERT has more target columns than expressions")
        check_types(values)
        if values[0] is not None and any(r[0] == values[0] for r in rows):
            raise SqlError("23505", f"duplicate key value violates unique constraint \"items_pkey\"")
        rows.append(values)
        return [complete("INSERT 0 1")]
    if verb == ("word", "update"):
        p.table()
        p.expect_word("set")
        col = p.column()
        p.expect_op("=")
        v = p.value()
        cond = p.where()
        p.done()
        probe = [None] * len(COLUMNS)
        probe[col] = v
        check_types(probe)
        n = 0
        for r in rows:
            if matches(r, cond):
                r[col] = v
                n += 1
        return [complete(f"UPDATE {n}")]
    if verb == ("word", "delete"):
        p.expect_word("from")
        p.table()
        cond = p.where()
        p.done()
        keep = [r for r in rows if not matches(r, cond)]
        n = len(rows) - len(keep)
        rows[:] = keep
        return [complete(f"DELETE {n}")]
    p.pos -= 1
    raise SqlError("42601", f"syntax error at or near {p.near()}")


# ------------------------------------------------------------ the server

class Session:
    def __init__(self):
        self.snapshot = None   # the transaction's own copy of the rows
        self.failed = False


def status(s):
    if s.snapshot is None:
        return "I"
    return "E" if s.failed else "T"


def run_query(st, s, text):
    out = []
    stmts = [q.strip() for q in text.split(";")]
    if all(q == "" for q in stmts):
        return [msg(b"I", b"")]
    for q in stmts:
        if q == "":
            continue
        word = q.split()[0].lower()
        if s.failed and word not in ("rollback", "commit"):
            out.append(error("25P02", "current transaction is aborted, commands ignored until end of transaction block"))
            return out
        if word == "begin":
            if s.snapshot is not None:
                out.append(msg(b"N", b"S" + cstr("WARNING") + b"C" + cstr("25001") + b"M" + cstr("there is already a transaction in progress") + b"\0"))
            else:
                s.snapshot = [list(r) for r in st.rows]
            out.append(complete("BEGIN"))
            continue
        if word in ("commit", "rollback"):
            if s.snapshot is None:
                out.append(msg(b"N", b"S" + cstr("WARNING") + b"C" + cstr("25P01") + b"M" + cstr("there is no transaction in progress") + b"\0"))
                out.append(complete(word.upper()))
                continue
            if word == "commit" and not s.failed:
                st.rows = s.snapshot
                out.append(complete("COMMIT"))
            else:
                out.append(complete("ROLLBACK"))
            s.snapshot = None
            s.failed = False
            continue
        rows = s.snapshot if s.snapshot is not None else [list(r) for r in st.rows]
        try:
            out.extend(execute(rows, q))
        except SqlError as e:
            out.append(error(e.code, e.text))
            if s.snapshot is not None:
                s.failed = True
            return out
        if s.snapshot is None:
            st.rows = rows
    return out


def serve(st, conn):
    f = conn.makefile("rb")
    try:
        head = read_exact(f, 8)
        length, proto = struct.unpack(">ii", head)
        body = read_exact(f, length - 8)
        parts = body.split(b"\0")
        params = dict(zip(parts[0::2], parts[1::2]))
        user = params.get(b"user", b"").decode()
        if proto != 196608:
            conn.sendall(error("0A000", f"unsupported frontend protocol {proto >> 16}.{proto & 0xFFFF}"))
            return
        conn.sendall(msg(b"R", struct.pack(">i", 3)))
        tag = read_exact(f, 1)
        n = struct.unpack(">i", read_exact(f, 4))[0]
        password = read_exact(f, n - 4).rstrip(b"\0").decode()
        if tag != b"p" or PASSWORDS.get(user) != password:
            conn.sendall(error("28P01", f'password authentication failed for user "{user}"'))
            return
        with st.lock:
            pid = st.next_pid
            st.next_pid += 1
        out = msg(b"R", struct.pack(">i", 0))
        out += msg(b"S", cstr("server_version") + cstr("16.0-kara"))
        out += msg(b"S", cstr("client_encoding") + cstr("UTF8"))
        out += msg(b"K", struct.pack(">ii", pid, pid * 7919 % 65536))
        out += ready("I")
        conn.sendall(out)
        s = Session()
        while True:
            tag = f.read(1)
            if not tag:
                break
            n = struct.unpack(">i", read_exact(f, 4))[0]
            body = read_exact(f, n - 4)
            if tag == b"X":
                break
            if tag != b"Q":
                conn.sendall(error("08P01", f"unsupported message type '{tag.decode()}'") + ready(status(s)))
                continue
            with st.lock:
                st.queries += 1
                out = run_query(st, s, body.rstrip(b"\0").decode())
            conn.sendall(b"".join(out) + ready(status(s)))
    except EOFError:
        pass
    finally:
        f.close()
        conn.close()


def run_server(st, listener):
    threads = []
    for _ in range(SESSIONS):
        conn, _ = listener.accept()
        t = threading.Thread(target=serve, args=(st, conn))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


# ------------------------------------------------------------ the clients

TYPE_NAMES = {INT4: "int4", TEXT: "text"}


class Client:
    def __init__(self, name):
        self.name = name
        self.sock = socket.create_connection(ADDR)
        self.sock.settimeout(2.0)
        self.f = self.sock.makefile("rb")

    def say(self, line):
        print(f"{self.name} {line}")

    def recv(self):
        tag = self.f.read(1)
        if not tag:
            self.say("<- (closed)")
            return None, b""
        n = struct.unpack(">i", read_exact(self.f, 4))[0]
        return tag.decode(), read_exact(self.f, n - 4)

    def fields(self, body):
        out = []
        for part in body.split(b"\0"):
            if part:
                out.append(f"{chr(part[0])}={part[1:].decode()}")
        return " ".join(out)

    # Prints every message up to ReadyForQuery (or the close); returns the
    # ReadyForQuery status, or "" when the server closed.
    def drain(self):
        while True:
            tag, body = self.recv()
            if tag is None:
                return ""
            if tag == "R":
                code = struct.unpack(">i", body[:4])[0]
                self.say("<- Authentication " + {0: "ok", 3: "cleartext password"}.get(code, str(code)))
                if code == 3:
                    return "R"
            elif tag == "S":
                k, v = body.rstrip(b"\0").split(b"\0")
                self.say(f"<- ParameterStatus {k.decode()}={v.decode()}")
            elif tag == "K":
                pid, key = struct.unpack(">ii", body)
                self.say(f"<- BackendKeyData pid={pid} key={key}")
            elif tag == "Z":
                self.say(f"<- ReadyForQuery {body.decode()}")
                return body.decode()
            elif tag == "T":
                n = struct.unpack(">h", body[:2])[0]
                at = 2
                cols = []
                for _ in range(n):
                    end = body.index(b"\0", at)
                    name = body[at:end].decode()
                    _, _, oid, size, _, _ = struct.unpack(">ihihih", body[end + 1:end + 19])
                    cols.append(f"{name}:{TYPE_NAMES.get(oid, oid)}/{size}")
                    at = end + 19
                self.say("<- RowDescription " + " ".join(cols))
            elif tag == "D":
                n = struct.unpack(">h", body[:2])[0]
                at = 2
                vals = []
                for _ in range(n):
                    size = struct.unpack(">i", body[at:at + 4])[0]
                    at += 4
                    if size < 0:
                        vals.append("NULL")
                    else:
                        vals.append(body[at:at + size].decode())
                        at += size
                self.say("<- DataRow " + " | ".join(vals))
            elif tag == "C":
                self.say(f"<- CommandComplete {body.rstrip(b'\0').decode()}")
            elif tag == "I":
                self.say("<- EmptyQueryResponse")
            elif tag == "E":
                self.say(f"<- ErrorResponse {self.fields(body)}")
            elif tag == "N":
                self.say(f"<- NoticeResponse {self.fields(body)}")
            else:
                self.say(f"<- {tag} ({len(body)} bytes)")

    def startup(self, user, password, proto=196608):
        params = cstr("user") + cstr(user) + cstr("database") + cstr("shop") + b"\0"
        self.say(f"-> StartupMessage user={user} protocol={proto >> 16}.{proto & 0xFFFF}")
        self.sock.sendall(struct.pack(">ii", len(params) + 8, proto) + params)
        if self.drain() != "R":
            return False
        self.say("-> PasswordMessage")
        self.sock.sendall(msg(b"p", cstr(password)))
        return self.drain() != ""

    def query(self, sql):
        self.say(f"-> Query {sql}")
        self.sock.sendall(msg(b"Q", cstr(sql)))
        self.drain()

    def raw(self, tag, body):
        self.say(f"-> {tag} ({len(body)} bytes)")
        self.sock.sendall(msg(tag.encode(), body))
        self.drain()

    def terminate(self):
        self.say("-> Terminate")
        self.sock.sendall(msg(b"X", b""))
        tag, _ = self.recv()
        self.f.close()
        self.sock.close()


def main():
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR)
    listener.listen(8)
    st = State()
    server = threading.Thread(target=run_server, args=(st, listener))
    server.start()

    print("== session 1: a wrong password")
    m = Client("mallory")
    m.startup("alice", "guess")
    m.f.close()
    m.sock.close()

    print("== session 2: an unsupported protocol version")
    o = Client("old")
    o.startup("alice", "wonder", proto=131072)
    o.f.close()
    o.sock.close()

    print("== session 3: alice queries and writes")
    a = Client("alice")
    a.startup("alice", "wonder")
    a.query("SELECT * FROM items")
    a.query("SELECT name, qty FROM items WHERE qty > 3 ORDER BY qty")
    a.query("INSERT INTO items VALUES (4, 'dragon''s fruit', 7)")
    a.query("INSERT INTO items VALUES (5, NULL, 1); SELECT id, name FROM items WHERE id >= 4")
    a.query("INSERT INTO items VALUES (4, 'again', 1)")
    a.query("INSERT INTO items VALUES (6, 'fig')")
    a.query("INSERT INTO items VALUES ('six', 'fig', 1)")
    a.query("UPDATE items SET qty = 9 WHERE name = 'apple'")
    a.query("UPDATE items SET name = 3 WHERE id = 1")
    a.query("DELETE FROM items WHERE qty = 0")
    a.query("SELECT price FROM items")
    a.query("SELECT * FROM orders")
    a.query("SELEC * FROM items")
    a.query("SELECT * FROM items WHERE name = 'x")
    a.query("SELECT * FROM items WHERE qty = 'many'")
    a.query("")
    a.query(" ; ")
    a.raw("P", cstr("") + cstr("SELECT 1") + b"\0\0")

    print("== session 4: bob's transaction while alice reads")
    b = Client("bob")
    b.startup("bob", "builder")
    b.query("BEGIN")
    b.query("UPDATE items SET qty = 100 WHERE id = 2")
    b.query("INSERT INTO items VALUES (7, 'grape', 30)")
    b.query("SELECT id, qty FROM items ORDER BY id")
    a.query("SELECT id, qty FROM items ORDER BY id")
    b.query("BEGIN")
    b.query("COMMIT")
    a.query("SELECT id, qty FROM items ORDER BY id")
    b.query("BEGIN; DELETE FROM items WHERE id = 1; SELECT * FROM nowhere; SELECT 1")
    b.query("SELECT * FROM items")
    b.query("ROLLBACK")
    b.query("COMMIT")
    b.query("SELECT count FROM items")
    b.query("SELECT * FROM items ORDER BY name")
    b.terminate()
    a.terminate()

    server.join()
    print(f"== server: {st.queries} queries, {len(st.rows)} rows")
    for r in sorted(st.rows, key=lambda r: r[0]):
        print(" | ".join("NULL" if v is None else str(v) for v in r))
    listener.close()


main()
