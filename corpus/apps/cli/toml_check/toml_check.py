# Mirror of toml_check.kara: a TOML 1.0 parser and validator that reads
# real config files and prints every leaf with its type.
import os, sys

class TomlError(Exception):
    pass

# ------------------------------------------------------------ the document arena

# A value is a tuple: ("string", s) ("integer", n) ("float", x) ("bool", b)
# (kind, text) for the four datetime kinds, ("array", id) ("table", id).

class Doc:
    def __init__(self):
        self.tables = [([], [])]   # (keys, vals); table 0 is the root
        self.arrays = []

    def new_table(self):
        self.tables.append(([], []))
        return len(self.tables) - 1

    def new_array(self, items):
        self.arrays.append(items)
        return len(self.arrays) - 1

    def find(self, tid, key):
        keys, _ = self.tables[tid]
        for i, k in enumerate(keys):
            if k == key:
                return i
        return -1

    def set(self, tid, key, val):
        keys, vals = self.tables[tid]
        keys.append(key)
        vals.append(val)

    def nest(self, path):
        # The table at `path`, created as needed; an array of tables
        # leads into its last element.
        cur = 0
        for k in path:
            i = self.find(cur, k)
            if i < 0:
                t = self.new_table()
                self.set(cur, k, ("table", t))
                cur = t
                continue
            v = self.tables[cur][1][i]
            if v[0] == "array":
                items = self.arrays[v[1]]
                if not items or items[-1][0] != "table":
                    raise TomlError("cannot overwrite a value")
                v = items[-1]
            if v[0] != "table":
                raise TomlError("cannot overwrite a value")
            cur = v[1]
        return cur

    def append_table(self, path):
        parent = self.nest(path[:-1])
        i = self.find(parent, path[-1])
        t = self.new_table()
        if i < 0:
            self.set(parent, path[-1], ("array", self.new_array([("table", t)])))
            return
        v = self.tables[parent][1][i]
        if v[0] != "array":
            raise TomlError("cannot append to a non-array")
        self.arrays[v[1]].append(("table", t))

# ------------------------------------------------------------ flags

FROZEN = 1
EXPLICIT = 2

def pkey(path):
    return "\x00".join(path)

class Flags:
    def __init__(self):
        self.own = {}
        self.rec = {}
        self.pending = []

    def set(self, path, flag, recursive):
        m = self.rec if recursive else self.own
        k = pkey(path)
        m[k] = m.get(k, 0) | flag

    def is_(self, path, flag):
        if not path:
            return False
        for i in range(1, len(path)):
            if self.rec.get(pkey(path[:i]), 0) & flag:
                return True
        k = pkey(path)
        return bool((self.own.get(k, 0) | self.rec.get(k, 0)) & flag)

    def unset_all(self, path):
        k = pkey(path)
        for m in (self.own, self.rec):
            for x in list(m):
                if x == k or x.startswith(k + "\x00"):
                    del m[x]

    def finalize(self):
        for p in self.pending:
            self.set(p, EXPLICIT, False)
        self.pending = []

# ------------------------------------------------------------ the parser

BARE = set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-")
HEX = set("0123456789abcdefABCDEF")

class Parser:
    def __init__(self, src):
        self.s = src.replace("\r\n", "\n")
        self.p = 0
        self.doc = Doc()
        self.flags = Flags()

    def line(self):
        return self.s.count("\n", 0, min(self.p, len(self.s))) + 1

    def fail(self, msg):
        raise TomlError(f"line {self.line()}: {msg}")

    def peek(self, k=0):
        i = self.p + k
        return self.s[i] if i < len(self.s) else ""

    def ws(self):
        while self.peek() in (" ", "\t") and self.peek() != "":
            self.p += 1

    def comment(self):
        if self.peek() == "#":
            while self.peek() not in ("\n", ""):
                c = self.peek()
                if (ord(c) < 32 and c != "\t") or ord(c) == 127:
                    self.fail("control character in a comment")
                self.p += 1

    def ws_nl(self):
        # whitespace, newlines and comments, as inside an array
        while True:
            self.ws()
            if self.peek() == "#":
                self.comment()
            if self.peek() == "\n":
                self.p += 1
                continue
            return

    # ---- keys

    def key_part(self):
        c = self.peek()
        if c == '"':
            return self.basic_string()
        if c == "'":
            return self.literal_string()
        st = self.p
        while self.peek() != "" and self.peek() in BARE:
            self.p += 1
        if self.p == st:
            self.fail("expected a key")
        return self.s[st:self.p]

    def key(self):
        parts = [self.key_part()]
        while True:
            self.ws()
            if self.peek() != ".":
                return parts
            self.p += 1
            self.ws()
            parts.append(self.key_part())

    # ---- strings

    def check_char(self, c, multiline):
        o = ord(c)
        if (o < 32 and c != "\t" and not (multiline and c == "\n")) or o == 127:
            self.fail("control character in a string")

    def escape(self):
        self.p += 1  # the backslash
        c = self.peek()
        simple = {"b": "\b", "t": "\t", "n": "\n", "f": "\f", "r": "\r", '"': '"', "\\": "\\"}
        if c in simple and c != "":
            self.p += 1
            return simple[c]
        if c in ("u", "U"):
            n = 4 if c == "u" else 8
            h = self.s[self.p + 1:self.p + 1 + n]
            if len(h) != n or any(x not in HEX for x in h):
                self.fail("bad unicode escape")
            cp = int(h, 16)
            if cp > 0x10FFFF or 0xD800 <= cp <= 0xDFFF:
                self.fail("escape is not a unicode scalar value")
            self.p += 1 + n
            return chr(cp)
        self.fail(f"bad escape \\{c}")

    def basic_string(self):
        self.p += 1
        out = []
        while True:
            c = self.peek()
            if c == "" or c == "\n":
                self.fail("unterminated string")
            if c == '"':
                self.p += 1
                return "".join(out)
            if c == "\\":
                out.append(self.escape())
                continue
            self.check_char(c, False)
            out.append(c)
            self.p += 1

    def literal_string(self):
        self.p += 1
        st = self.p
        while True:
            c = self.peek()
            if c == "" or c == "\n":
                self.fail("unterminated string")
            if c == "'":
                s = self.s[st:self.p]
                self.p += 1
                return s
            self.check_char(c, False)
            self.p += 1

    def multiline(self, quote):
        self.p += 3
        if self.peek() == "\n":
            self.p += 1
        out = []
        while True:
            c = self.peek()
            if c == "":
                self.fail("unterminated string")
            if self.s.startswith(quote * 3, self.p):
                self.p += 3
                extra = 0
                while extra < 2 and self.peek() == quote:
                    self.p += 1
                    extra += 1
                return "".join(out) + quote * extra
            if c == "\\" and quote == '"':
                q = self.p + 1
                while q < len(self.s) and self.s[q] in " \t":
                    q += 1
                if q < len(self.s) and self.s[q] == "\n":
                    self.p = q
                    while self.peek() in (" ", "\t", "\n") and self.peek() != "":
                        self.p += 1
                    continue
                out.append(self.escape())
                continue
            self.check_char(c, True)
            out.append(c)
            self.p += 1

    # ---- values

    def value(self):
        c = self.peek()
        if self.s.startswith('"""', self.p):
            return ("string", self.multiline('"'))
        if c == '"':
            return ("string", self.basic_string())
        if self.s.startswith("'''", self.p):
            return ("string", self.multiline("'"))
        if c == "'":
            return ("string", self.literal_string())
        if self.s.startswith("true", self.p):
            self.p += 4
            return ("bool", True)
        if self.s.startswith("false", self.p):
            self.p += 5
            return ("bool", False)
        if c == "[":
            return self.array()
        if c == "{":
            return self.inline_table()
        return self.scalar()

    def array(self):
        self.p += 1
        items = []
        while True:
            self.ws_nl()
            if self.peek() == "]":
                self.p += 1
                return ("array", self.doc.new_array(items))
            if self.peek() == "":
                self.fail("unclosed array")
            items.append(self.value())
            self.ws_nl()
            c = self.peek()
            if c == ",":
                self.p += 1
            elif c == "]":
                self.p += 1
                return ("array", self.doc.new_array(items))
            else:
                self.fail("expected ',' or ']' in an array")

    def inline_table(self):
        self.p += 1
        tid = self.doc.new_table()
        flags = Flags()
        self.ws()
        if self.peek() == "}":
            self.p += 1
            return ("table", tid)
        while True:
            self.ws()
            k = self.key()
            self.ws()
            if self.peek() != "=":
                self.fail("expected '=' after a key")
            self.p += 1
            self.ws()
            v = self.value()
            parent = self.nest_in(tid, k[:-1])
            if flags.is_(k[:-1], FROZEN):
                self.fail("cannot change an inline table")
            if self.doc.find(parent, k[-1]) >= 0:
                self.fail(f"duplicate key {show_key(k[-1])}")
            self.doc.set(parent, k[-1], v)
            if v[0] in ("table", "array"):
                flags.set(k, FROZEN, True)
            self.ws()
            c = self.peek()
            if c == "}":
                self.p += 1
                return ("table", tid)
            if c != ",":
                if c == "\n":
                    self.fail("newline in an inline table")
                self.fail("expected ',' or '}' in an inline table")
            self.p += 1
            self.ws()
            if self.peek() == "}":
                self.fail("trailing comma in an inline table")
            if self.peek() == "\n":
                self.fail("newline in an inline table")

    def nest_in(self, tid, path):
        cur = tid
        for k in path:
            i = self.doc.find(cur, k)
            if i < 0:
                t = self.doc.new_table()
                self.doc.set(cur, k, ("table", t))
                cur = t
                continue
            v = self.doc.tables[cur][1][i]
            if v[0] != "table":
                self.fail("cannot overwrite a value")
            cur = v[1]
        return cur

    def scalar(self):
        st = self.p
        while self.peek() != "" and self.peek() not in " \t\n,]}#":
            self.p += 1
        tok = self.s[st:self.p]
        # a date-time may hold one space between the date and the time
        if len(tok) == 10 and tok[4:5] == "-" and self.peek() == " " and self.peek(1).isdigit():
            self.p += 1
            st2 = self.p
            while self.peek() != "" and self.peek() not in " \t\n,]}#":
                self.p += 1
            tok = tok + "T" + self.s[st2:self.p]
        if tok == "":
            self.fail("expected a value")
        if len(tok) >= 5 and tok[4] == "-" and tok[:4].isdigit():
            return self.datetime(tok)
        if len(tok) >= 3 and tok[2] == ":" and tok[:2].isdigit():
            t = self.time(tok)
            return ("time-local", t)
        return number(self, tok)

    def two(self, s, at, lo, hi, what):
        d = s[at:at + 2]
        if len(d) != 2 or not d.isdigit() or not (lo <= int(d) <= hi):
            self.fail(f"invalid {what} in {s}")
        return int(d)

    def time(self, s):
        # HH:MM:SS[.frac] -> the normalised form and the rest
        if len(s) < 8 or s[2] != ":" or s[5] != ":":
            self.fail(f"invalid time {s}")
        self.two(s, 0, 0, 23, "hour")
        self.two(s, 3, 0, 59, "minute")
        self.two(s, 6, 0, 59, "second")
        out = s[:8]
        rest = s[8:]
        if rest.startswith("."):
            i = 1
            while i < len(rest) and rest[i].isdigit():
                i += 1
            if i == 1:
                self.fail(f"invalid time {s}")
            frac = rest[1:i][:6].ljust(6, "0")
            if frac != "000000":
                out += "." + frac
            rest = rest[i:]
        if rest != "" and self.offset_ok is None:
            self.fail(f"invalid time {s}")
        self.rest = rest
        return out

    offset_ok = None

    def datetime(self, s):
        if len(s) < 10 or s[4] != "-" or s[7] != "-" or not s[:4].isdigit():
            self.fail(f"invalid date {s}")
        y = int(s[:4])
        m = self.two(s, 5, 1, 12, "month")
        days = [31, 29 if (y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)) else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        self.two(s, 8, 1, days[m - 1], "day")
        date = s[:10]
        if len(s) == 10:
            return ("date-local", date)
        if s[10] not in "Tt":
            self.fail(f"invalid date {s}")
        self.offset_ok = True
        t = self.time(s[11:])
        self.offset_ok = None
        rest = self.rest
        if rest == "":
            return ("datetime-local", date + "T" + t)
        if rest in ("Z", "z"):
            return ("datetime", date + "T" + t + "+00:00")
        if len(rest) == 6 and rest[0] in "+-" and rest[3] == ":":
            hh = self.two(rest, 1, 0, 23, "offset")
            mm = self.two(rest, 4, 0, 59, "offset")
            sign = "-" if rest[0] == "-" and (hh or mm) else "+"
            return ("datetime", date + "T" + t + f"{sign}{hh:02}:{mm:02}")
        self.fail(f"invalid offset in {s}")

def digits_ok(s, allowed):
    # digits from `allowed`, with single underscores only between digits
    if s == "" or s[0] == "_" or s[-1] == "_":
        return False
    prev = ""
    for c in s:
        if c == "_":
            if prev == "_":
                return False
        elif c not in allowed:
            return False
        prev = c
    return True

DEC = "0123456789"

def number(p, tok):
    t = tok
    if t in ("inf", "+inf", "-inf", "nan", "+nan", "-nan"):
        return ("float", float(t))
    for prefix, base, allowed in (("0x", 16, "0123456789abcdefABCDEF"), ("0o", 8, "01234567"), ("0b", 2, "01")):
        if t.startswith(prefix):
            body = t[2:]
            if not digits_ok(body, allowed):
                p.fail(f"invalid number {tok}")
            v = int(body.replace("_", ""), base)
            if v > 2**63 - 1:
                p.fail(f"integer out of range {tok}")
            return ("integer", v)
    sign = ""
    if t[:1] in ("+", "-"):
        sign = t[0]
        t = t[1:]
    # split into integer part, fraction, exponent
    ip = t
    frac = None
    exp = None
    e = -1
    for i, c in enumerate(t):
        if c in "eE":
            e = i
            break
    if e >= 0:
        ip, exp = t[:e], t[e + 1:]
    d = ip.find(".")
    if d >= 0:
        ip, frac = ip[:d], ip[d + 1:]
    if not digits_ok(ip, DEC) or (len(ip) > 1 and ip[0] == "0"):
        p.fail(f"invalid number {tok}")
    if frac is not None and not digits_ok(frac, DEC):
        p.fail(f"invalid number {tok}")
    if exp is not None:
        x = exp[1:] if exp[:1] in ("+", "-") else exp
        if not digits_ok(x, DEC):
            p.fail(f"invalid number {tok}")
    if frac is None and exp is None:
        v = int(sign + ip.replace("_", ""))
        if v > 2**63 - 1 or v < -2**63:
            p.fail(f"integer out of range {tok}")
        return ("integer", v)
    return ("float", float(tok.replace("_", "")))

def show_key(k):
    if k != "" and all(c in BARE for c in k):
        return k
    return quote(k)

def quote(s):
    out = '"'
    for c in s:
        o = ord(c)
        if c == '"':
            out += '\\"'
        elif c == "\\":
            out += "\\\\"
        elif c == "\n":
            out += "\\n"
        elif c == "\t":
            out += "\\t"
        elif c == "\r":
            out += "\\r"
        elif o < 32 or o == 127:
            out += f"\\u{o:04x}"
        else:
            out += c
    return out + '"'

def parse(src):
    p = Parser(src)
    header = []
    while True:
        p.ws()
        c = p.peek()
        if c == "":
            break
        if c == "\n":
            p.p += 1
            continue
        if c == "[":
            p.flags.finalize()
            if p.peek(1) == "[":
                p.p += 2
                p.ws()
                k = p.key()
                p.ws()
                if p.s[p.p:p.p + 2] != "]]":
                    p.fail("expected ']]' after an array of tables")
                p.p += 2
                if p.flags.is_(k, FROZEN):
                    p.fail(f"cannot append to {'.'.join(show_key(x) for x in k)}, a static array")
                p.flags.unset_all(k)
                p.flags.set(k, EXPLICIT, False)
                try:
                    p.doc.append_table(k)
                except TomlError as e:
                    p.fail(str(e))
            else:
                p.p += 1
                p.ws()
                k = p.key()
                p.ws()
                if p.peek() != "]":
                    p.fail("expected ']' after a table name")
                p.p += 1
                if p.flags.is_(k, EXPLICIT) or p.flags.is_(k, FROZEN):
                    p.fail(f"table {'.'.join(show_key(x) for x in k)} defined twice")
                p.flags.set(k, EXPLICIT, False)
                try:
                    p.doc.nest(k)
                except TomlError as e:
                    p.fail(str(e))
            header = k
        elif c == "#":
            pass
        else:
            k = p.key()
            p.ws()
            if p.peek() != "=":
                p.fail("expected '=' after a key")
            p.p += 1
            p.ws()
            if p.peek() in ("\n", "", "#"):
                p.fail("expected a value")
            v = p.value()
            for i in range(1, len(k)):
                cont = header + k[:i]
                if p.flags.is_(cont, EXPLICIT):
                    p.fail(f"cannot redefine table {'.'.join(show_key(x) for x in cont)} with a dotted key")
                p.flags.pending.append(cont)
            parent_path = header + k[:-1]
            if p.flags.is_(parent_path, FROZEN):
                p.fail(f"cannot change {'.'.join(show_key(x) for x in parent_path)}, an inline value")
            try:
                parent = p.doc.nest(parent_path)
            except TomlError as e:
                p.fail(str(e))
            if p.doc.find(parent, k[-1]) >= 0:
                p.fail(f"duplicate key {'.'.join(show_key(x) for x in header + k)}")
            if v[0] in ("table", "array"):
                p.flags.set(header + k, FROZEN, True)
            p.doc.set(parent, k[-1], v)
        p.ws()
        p.comment()
        c = p.peek()
        if c == "":
            break
        if c != "\n":
            p.fail("expected the end of the line")
        p.p += 1
    return p.doc

# ------------------------------------------------------------ rendering

def render_value(doc, path, v, out):
    kind, x = v
    if kind == "table":
        keys, vals = doc.tables[x]
        if not keys:
            out.append(f"{path} = table {{}}")
        for k, w in zip(keys, vals):
            render_value(doc, (path + "." if path else "") + show_key(k), w, out)
    elif kind == "array":
        items = doc.arrays[x]
        out.append(f"{path} = array[{len(items)}]")
        for i, w in enumerate(items):
            render_value(doc, f"{path}[{i}]", w, out)
    elif kind == "string":
        out.append(f"{path} = string {quote(x)}")
    elif kind == "integer":
        out.append(f"{path} = integer {x}")
    elif kind == "float":
        out.append(f"{path} = float {fmt_float(x)}")
    elif kind == "bool":
        out.append(f"{path} = bool {'true' if x else 'false'}")
    else:
        out.append(f"{path} = {kind} {x}")

def fmt_float(x):
    if x != x:
        return "nan"
    if x == float("inf"):
        return "inf"
    if x == float("-inf"):
        return "-inf"
    return repr(x)

def render(doc):
    out = []
    render_value(doc, "", ("table", 0), out)
    return out

DOCS = {}

DOCS["Cargo.toml"] = r'''[package]
name = "kara-demo"
version = "0.3.1"
edition = "2021"
authors = ["Ada <ada@example.com>", "Grace <grace@example.com>"]
description = """\
    A demo crate that does \
    several things.\
    """
license = 'MIT OR Apache-2.0'

[dependencies]
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1", default-features = false, features = ["rt", "net"] }
log = "0.4"

[dev-dependencies]
criterion = "0.5"

[[bin]]
name = "server"
path = "src/bin/server.rs"

[[bin]]
name = "cli"
path = "src/bin/cli.rs"
required-features = ["full"]

[profile.release]
lto = true
opt-level = 3
codegen-units = 1

[features]
default = []
full = ["serde", "tokio"]  # everything
'''

DOCS["pyproject.toml"] = r'''# Project metadata
[build-system]
requires = ["setuptools>=68", "wheel"]
build-backend = "setuptools.build_meta"

[project]
name = "kara-tools"
version = "1.2.0"
requires-python = ">=3.11"
dependencies = [
    "requests>=2.31",  # HTTP
    "rich",
    # a comment between elements
    "typer[all]>=0.9",
]

[project.optional-dependencies]
dev = ["pytest>=8", "ruff"]

[project.scripts]
kara-tools = "kara_tools.cli:main"

[tool.ruff]
line-length = 100
target-version = "py311"

[tool.ruff.lint]
select = ["E", "F", "I"]
ignore = []

[tool.ruff.lint.per-file-ignores]
"tests/*" = ["S101"]
'''

DOCS["tour.toml"] = r'''title = "TOML Example"

[owner]
name = "Tom Preston-Werner"
dob = 1979-05-27T07:32:00-08:00

[database]
enabled = true
ports = [ 8000, 8001, 8002 ]
data = [ ["delta", "phi"], [3.14] ]
temp_targets = { cpu = 79.5, case = 72.0 }

[servers]

[servers.alpha]
ip = "10.0.0.1"
role = "frontend"

[servers.beta]
ip = "10.0.0.2"
role = "backend"

[numbers]
hex = 0xDEAD_BEEF
oct = 0o755
bin = 0b1101_0110
million = 1_000_000
plus = +99
minus = -17
zero = 0
floats = [+1.0, 3.1415, -0.01, 5e+22, 1e06, -2E-2, 6.626e-34, 224_617.445_991_228]
special = [inf, -inf, nan, +inf]

[strings]
escapes = "tab\tquote\" backslash\\ e-acute \u00E9 smile \U0001F600"
winpath = 'C:\Users\nodejs\templates'
regex = '<\i\c*\s*>'
lines = """
Roses are red
Violets are blue"""
quotes = """Here are two quotation marks: "". Simple enough."""
five = """""five quotes"""""
raw = \'\'\'
The first newline is
trimmed in raw strings.
   All other whitespace
   is preserved.
\'\'\'
empty = ""

[dates]
odt1 = 1979-05-27T07:32:00Z
odt2 = 1979-05-27T00:32:00.999999-07:00
odt3 = 1979-05-27 07:32:00+05:30
ldt = 1979-05-27T07:32:00.5
ld = 1979-05-27
lt = 07:32:00
lt2 = 00:32:00.999999999

[keys]
"" = "blank"
"127.0.0.1" = "localhost"
site."google.com" = true
3.14159 = "pi"
fruit.apple.color = "red"
fruit.apple.taste.sweet = true

[[fruits]]
name = "apple"

[fruits.physical]
color = "red"
shape = "round"

[[fruits.varieties]]
name = "red delicious"

[[fruits.varieties]]
name = "granny smith"

[[fruits]]
name = "banana"

[[fruits.varieties]]
name = "plantain"

[a.b.c]
deep = 1

[a]
shallow = 2
'''.replace("\\'", "'")

BAD = [
    ("duplicate key", "a = 1\nb = 2\na = 3\n"),
    ("table twice", "[a]\nx = 1\n[a]\ny = 2\n"),
    ("dotted, then table", "a.b = 1\n[a]\n"),
    ("table, then dotted redefinition", "[x]\ny.z = 1\n[x.y]\n"),
    ("header into inline table", "a = { b = 1 }\n[a.c]\n"),
    ("dotted into inline table", "a = { b = 1 }\na.c = 2\n"),
    ("array of tables over a static array", "fruit = []\n[[fruit]]\n"),
    ("table over a value", "a = 1\n[a]\n"),
    ("leading zero", "n = 012\n"),
    ("double underscore", "n = 1__2\n"),
    ("leading underscore", "n = _1\n"),
    ("bare fraction", "f = .5\n"),
    ("trailing dot", "f = 5.\n"),
    ("empty exponent", "f = 1e\n"),
    ("unterminated string", "s = \"abc\n"),
    ("bad escape", "s = \"a\\qb\"\n"),
    ("surrogate escape", "s = \"\\uD800\"\n"),
    ("month 13", "d = 1979-13-27\n"),
    ("february 30", "d = 2023-02-30\n"),
    ("missing value", "a =\n"),
    ("newline in inline table", "a = { b = 1,\n c = 2 }\n"),
    ("trailing comma in inline table", "a = { b = 1, }\n"),
    ("key without value", "[t]\nkey\n"),
    ("two values on a line", "a = 1 b = 2\n"),
    ("unclosed array", "a = [1, 2\n"),
    ("hex overflow", "n = 0x8000_0000_0000_0000\n"),
    ("dotted into a value", "a = 1\na.b = 2\n"),
]

def main():
    os.makedirs("conf", exist_ok=True)
    for name, text in DOCS.items():
        with open(f"conf/{name}", "w") as f:
            f.write(text)
    for i, (label, text) in enumerate(BAD):
        with open(f"conf/bad-{i:02}.toml", "w") as f:
            f.write(text)
    for name in sorted(os.listdir("conf")):
        src = open(f"conf/{name}").read()
        if name.startswith("bad-"):
            label = BAD[int(name[4:6])][0]
            try:
                parse(src)
                print(f"== {name} ({label}): ACCEPTED")
            except TomlError as e:
                print(f"== {name} ({label}): {e}")
            continue
        doc = parse(src)
        lines = render(doc)
        print(f"== {name}: {len(src.encode())} bytes, {len(lines)} leaves")
        for ln in lines:
            print(f"  {ln}")

if __name__ == "__main__":
    main()
