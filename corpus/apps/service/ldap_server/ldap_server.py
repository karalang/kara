"""Oracle for ldap_server.kara: an LDAPv3 directory server on loopback TCP
(RFC 4511), every message BER-encoded, and the scripted clients that bind,
search with RFC 4515 filter strings, compare, add, modify and delete.
Written with socket and threading only; the BER codec is our own."""

import socket
import threading

ADDR = ("127.0.0.1", 47787)
SESSIONS = 6
BASE = "dc=kara,dc=test"
ADMIN = "cn=admin,dc=kara,dc=test"
# Obviously fake credentials.
ADMIN_PASSWORD = "not-a-real-secret/admin"
NOTICE_OID = "1.3.6.1.4.1.1466.20036"
MAX_MESSAGE = 1 << 20

SUCCESS = 0
OPERATIONS_ERROR = 1
PROTOCOL_ERROR = 2
SIZE_LIMIT_EXCEEDED = 4
COMPARE_FALSE = 5
COMPARE_TRUE = 6
AUTH_METHOD_NOT_SUPPORTED = 7
NO_SUCH_ATTRIBUTE = 16
ATTRIBUTE_OR_VALUE_EXISTS = 20
NO_SUCH_OBJECT = 32
INVALID_DN_SYNTAX = 34
INVALID_CREDENTIALS = 49
INSUFFICIENT_ACCESS = 50
NAMING_VIOLATION = 64
OBJECT_CLASS_VIOLATION = 65
NOT_ALLOWED_ON_NON_LEAF = 66
ENTRY_ALREADY_EXISTS = 68

RESULT_NAMES = {
    0: "success",
    1: "operationsError",
    2: "protocolError",
    4: "sizeLimitExceeded",
    5: "compareFalse",
    6: "compareTrue",
    7: "authMethodNotSupported",
    16: "noSuchAttribute",
    20: "attributeOrValueExists",
    32: "noSuchObject",
    34: "invalidDNSyntax",
    49: "invalidCredentials",
    50: "insufficientAccessRights",
    64: "namingViolation",
    65: "objectClassViolation",
    66: "notAllowedOnNonLeaf",
    68: "entryAlreadyExists",
}

# Attribute types whose values order and match as integers.
NUMERIC = {"uidnumber", "gidnumber"}
# Attribute types whose values match exactly, byte for byte.
EXACT = {"userpassword"}


class BerError(Exception):
    pass


# ---- BER


def enc_len(n):
    if n < 0x80:
        return bytes([n])
    if n < 0x100:
        return bytes([0x81, n])
    if n < 0x10000:
        return bytes([0x82, n >> 8, n & 0xFF])
    return bytes([0x83, n >> 16, (n >> 8) & 0xFF, n & 0xFF])


def tlv(tag, content):
    return bytes([tag]) + enc_len(len(content)) + content


def enc_int(tag, n):
    out = []
    while True:
        out.insert(0, n & 0xFF)
        n >>= 8
        if (n == 0 and out[0] < 0x80) or (n == -1 and out[0] >= 0x80):
            break
    return tlv(tag, bytes(out))


def enc_str(tag, s):
    return tlv(tag, s.encode() if isinstance(s, str) else s)


def seq(tag, parts):
    return tlv(tag, b"".join(parts))


class Reader:
    """Reads TLVs off a byte string; every read checks the bounds."""

    def __init__(self, data):
        self.data = data
        self.pos = 0

    def done(self):
        return self.pos >= len(self.data)

    def peek(self):
        if self.done():
            raise BerError("truncated")
        return self.data[self.pos]

    def read(self):
        if self.pos + 2 > len(self.data):
            raise BerError("truncated")
        tag = self.data[self.pos]
        if tag & 0x1F == 0x1F:
            raise BerError("high tag numbers are not used")
        first = self.data[self.pos + 1]
        self.pos += 2
        if first < 0x80:
            n = first
        else:
            k = first & 0x7F
            if k == 0 or k > 3:
                raise BerError("bad length")
            if self.pos + k > len(self.data):
                raise BerError("truncated")
            n = 0
            for b in self.data[self.pos:self.pos + k]:
                n = n * 256 + b
            self.pos += k
        if self.pos + n > len(self.data):
            raise BerError("truncated")
        body = self.data[self.pos:self.pos + n]
        self.pos += n
        return tag, body

    def expect(self, tag):
        t, body = self.read()
        if t != tag:
            raise BerError(f"expected tag 0x{tag:02x}, got 0x{t:02x}")
        return body

    def int(self, tag=0x02):
        body = self.expect(tag)
        if not body or len(body) > 8:
            raise BerError("bad integer")
        n = int.from_bytes(body, "big", signed=True)
        return n

    def text(self, tag=0x04):
        try:
            return self.expect(tag).decode()
        except UnicodeDecodeError:
            raise BerError("not UTF-8")

    def bool(self):
        body = self.expect(0x01)
        if len(body) != 1:
            raise BerError("bad boolean")
        return body[0] != 0


def ascii_lower(s):
    return "".join(chr(ord(c) + 32) if "A" <= c <= "Z" else c for c in s)


# ---- distinguished names


def split_dn(dn):
    """The RDNs of a DN string, split at unescaped commas; None if bad."""
    if dn == "":
        return []
    parts = []
    cur = ""
    i = 0
    while i < len(dn):
        c = dn[i]
        if c == "\\":
            if i + 1 >= len(dn):
                return None
            cur += dn[i:i + 2]
            i += 2
            continue
        if c == ",":
            parts.append(cur)
            cur = ""
        else:
            cur += c
        i += 1
    parts.append(cur)
    out = []
    for p in parts:
        k = p.find("=")
        if k <= 0:
            return None
        t = p[:k].strip()
        v = p[k + 1:].strip()
        if not t or not v:
            return None
        out.append((t, v))
    return out


def unescape(v):
    out = ""
    i = 0
    while i < len(v):
        if v[i] == "\\" and i + 1 < len(v):
            out += v[i + 1]
            i += 2
        else:
            out += v[i]
            i += 1
    return out


def norm_dn(rdns):
    """The DN's identity: its RDNs, types and values case-folded and the
    values unescaped, nearest first."""
    return tuple(f"{ascii_lower(t)}={ascii_lower(unescape(v))}" for t, v in rdns)


# ---- the directory


class Entry:
    def __init__(self, dn, attrs):
        self.dn = dn
        self.attrs = attrs  # [(type, [values])], in the order they came

    def get(self, name):
        n = ascii_lower(name)
        for t, vs in self.attrs:
            if ascii_lower(t) == n:
                return vs
        return None


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.entries = {}
        self.ops = {}
        self.results = {}
        for dn, attrs in seed():
            self.entries[norm_dn(split_dn(dn))] = Entry(dn, attrs)


def seed():
    return [
        (BASE, [("objectClass", ["top", "domain"]), ("dc", ["kara"])]),
        ("ou=people," + BASE, [("objectClass", ["organizationalUnit"]), ("ou", ["people"])]),
        ("ou=groups," + BASE, [("objectClass", ["organizationalUnit"]), ("ou", ["groups"])]),
        (ADMIN, [("objectClass", ["person"]), ("cn", ["admin"]), ("sn", ["Admin"]),
                 ("userPassword", [ADMIN_PASSWORD])]),
        ("uid=alice,ou=people," + BASE, [
            ("objectClass", ["person", "posixAccount"]), ("uid", ["alice"]),
            ("cn", ["Alice Liddell"]), ("sn", ["Liddell"]), ("mail", ["alice@kara.test"]),
            ("uidNumber", ["1001"]), ("userPassword", ["not-a-real-secret/alice"])]),
        ("uid=bob,ou=people," + BASE, [
            ("objectClass", ["person", "posixAccount"]), ("uid", ["bob"]),
            ("cn", ["Bob Builder"]), ("sn", ["Builder"]),
            ("mail", ["bob@kara.test", "builder@kara.test"]), ("uidNumber", ["1002"]),
            ("userPassword", ["not-a-real-secret/bob"])]),
        ("uid=carol,ou=people," + BASE, [
            ("objectClass", ["person", "posixAccount"]), ("uid", ["carol"]),
            ("cn", ["Carol Danvers"]), ("sn", ["Danvers"]), ("uidNumber", ["1010"]),
            ("userPassword", ["not-a-real-secret/carol"])]),
        ("cn=Smith\\, John,ou=people," + BASE, [
            ("objectClass", ["person"]), ("cn", ["Smith, John"]), ("sn", ["Smith"]),
            ("mail", ["jsmith@kara.test"])]),
        ("cn=staff,ou=groups," + BASE, [
            ("objectClass", ["groupOfNames"]), ("cn", ["staff"]),
            ("member", ["uid=alice,ou=people," + BASE, "uid=bob,ou=people," + BASE])]),
    ]


def tree_key(norm):
    return list(reversed(norm))


def parent_of(norm):
    return norm[1:]


def under(norm, base):
    """Whether `norm` is `base` or below it."""
    return len(norm) >= len(base) and norm[len(norm) - len(base):] == base


def matched_dn(st, rdns):
    """The DN of the deepest existing ancestor of `rdns`, as stored."""
    for i in range(1, len(rdns) + 1):
        n = norm_dn(rdns[i:])
        if n in st.entries:
            return st.entries[n].dn
    return ""


# ---- filters


def value_cmp(t, a, b):
    """-1, 0 or 1 comparing value a with assertion b for type t; None if
    they cannot be compared."""
    t = ascii_lower(t)
    if t in NUMERIC:
        try:
            x, y = int(a), int(b)
        except ValueError:
            return None
        return (x > y) - (x < y)
    if t in EXACT:
        return (a > b) - (a < b)
    x, y = ascii_lower(a), ascii_lower(b)
    return (x > y) - (x < y)


def substr_match(v, initial, anys, final):
    v = ascii_lower(v)
    pos = 0
    if initial is not None:
        i = ascii_lower(initial)
        if not v.startswith(i):
            return False
        pos = len(i)
    for a in anys:
        k = v.find(ascii_lower(a), pos)
        if k < 0:
            return False
        pos = k + len(a)
    if final is not None:
        f = ascii_lower(final)
        if len(v) - pos < len(f) or not v.endswith(f):
            return False
    return True


def visible(e, t, admin):
    if ascii_lower(t) in EXACT and not admin:
        return None
    return e.get(t)


def match(e, f, admin):
    kind = f[0]
    if kind == "and":
        return all(match(e, g, admin) for g in f[1])
    if kind == "or":
        return any(match(e, g, admin) for g in f[1])
    if kind == "not":
        return not match(e, f[1], admin)
    if kind == "present":
        return visible(e, f[1], admin) is not None
    if kind == "substr":
        vs = visible(e, f[1], admin) or []
        return any(substr_match(v, f[2], f[3], f[4]) for v in vs)
    t, want = f[1], f[2]
    vs = visible(e, t, admin) or []
    for v in vs:
        c = value_cmp(t, v, want)
        if c is None:
            continue
        if kind == "eq" and c == 0:
            return True
        if kind == "ge" and c >= 0:
            return True
        if kind == "le" and c <= 0:
            return True
    return False


def read_filter(r):
    tag, body = r.read()
    inner = Reader(body)
    if tag in (0xA0, 0xA1):
        parts = []
        while not inner.done():
            parts.append(read_filter(inner))
        if not parts:
            raise BerError("empty filter set")
        return ("and" if tag == 0xA0 else "or", parts)
    if tag == 0xA2:
        f = read_filter(inner)
        if not inner.done():
            raise BerError("trailing bytes in not")
        return ("not", f)
    if tag in (0xA3, 0xA5, 0xA6, 0xA8):
        t = inner.text()
        v = inner.text()
        kind = {0xA3: "eq", 0xA5: "ge", 0xA6: "le", 0xA8: "eq"}[tag]
        return (kind, t, v)
    if tag == 0xA4:
        t = inner.text()
        subs = Reader(inner.expect(0x30))
        initial, anys, final = None, [], None
        while not subs.done():
            st, sv = subs.read()
            sv = sv.decode()
            if st == 0x80 and initial is None and not anys and final is None:
                initial = sv
            elif st == 0x81 and final is None:
                anys.append(sv)
            elif st == 0x82 and final is None:
                final = sv
            else:
                raise BerError("bad substrings")
        return ("substr", t, initial, anys, final)
    if tag == 0x87:
        return ("present", body.decode())
    raise BerError(f"unknown filter tag 0x{tag:02x}")


# ---- operations


def result(tag, code, matched="", message=""):
    return seq(tag, [enc_int(0x0A, code), enc_str(0x04, matched), enc_str(0x04, message)])


def entry_attrs(e, wanted, types_only, admin):
    out = []
    all_user = not wanted or "*" in wanted
    names = {ascii_lower(w) for w in wanted}
    if names == {"1.1"}:
        return out
    for t, vs in e.attrs:
        lt = ascii_lower(t)
        if lt in EXACT and not admin:
            continue
        if all_user or lt in names:
            vals = [] if types_only else [enc_str(0x04, v) for v in vs]
            out.append(seq(0x30, [enc_str(0x04, t), seq(0x31, vals)]))
    return out


def do_search(st, mid, r, admin, send):
    base = r.text()
    scope = r.int(0x0A)
    r.int(0x0A)  # derefAliases
    size_limit = r.int()
    r.int()  # timeLimit
    types_only = r.bool()
    f = read_filter(r)
    attrs = Reader(r.expect(0x30))
    wanted = []
    while not attrs.done():
        wanted.append(attrs.text())
    if scope not in (0, 1, 2):
        return PROTOCOL_ERROR, "", "scope must be 0, 1 or 2", 0x65
    rdns = split_dn(base)
    if rdns is None:
        return INVALID_DN_SYNTAX, "", f"invalid DN '{base}'", 0x65
    nb = norm_dn(rdns)
    if nb not in st.entries:
        return NO_SUCH_OBJECT, matched_dn(st, rdns), f"no entry {base}", 0x65
    found = []
    for n in sorted(st.entries, key=tree_key):
        if scope == 0:
            inside = n == nb
        elif scope == 1:
            inside = len(n) > 0 and parent_of(n) == nb
        else:
            inside = under(n, nb)
        if inside and match(st.entries[n], f, admin):
            found.append(st.entries[n])
    sent = 0
    for e in found:
        if size_limit > 0 and sent == size_limit:
            return SIZE_LIMIT_EXCEEDED, "", f"more than {size_limit} entries", 0x65
        body = seq(0x64, [enc_str(0x04, e.dn), seq(0x30, entry_attrs(e, wanted, types_only, admin))])
        send(seq(0x30, [enc_int(0x02, mid), body]))
        sent += 1
    return SUCCESS, "", "", 0x65


def read_attr_list(r, tag):
    """SEQUENCE OF SEQUENCE { type, SET OF value }."""
    out = []
    rr = Reader(r.expect(tag))
    while not rr.done():
        one = Reader(rr.expect(0x30))
        t = one.text()
        vals = Reader(one.expect(0x31))
        vs = []
        while not vals.done():
            vs.append(vals.text())
        out.append((t, vs))
    return out


def do_add(st, r, admin):
    dn = r.text()
    attrs = read_attr_list(r, 0x30)
    if not admin:
        return INSUFFICIENT_ACCESS, "", "add needs the admin bind"
    rdns = split_dn(dn)
    if rdns is None or not rdns:
        return INVALID_DN_SYNTAX, "", f"invalid DN '{dn}'"
    n = norm_dn(rdns)
    if n in st.entries:
        return ENTRY_ALREADY_EXISTS, "", f"{dn} already exists"
    if parent_of(n) not in st.entries:
        return NO_SUCH_OBJECT, matched_dn(st, rdns), f"no parent for {dn}"
    e = Entry(dn, attrs)
    if not e.get("objectClass"):
        return OBJECT_CLASS_VIOLATION, "", "an entry needs an objectClass"
    t, v = rdns[0]
    vs = e.get(t) or []
    if not any(value_cmp(t, x, unescape(v)) == 0 for x in vs):
        return NAMING_VIOLATION, "", f"the entry has no {t} value {unescape(v)}"
    st.entries[n] = e
    return SUCCESS, "", ""


def do_delete(st, body, admin):
    dn = body.decode()
    if not admin:
        return INSUFFICIENT_ACCESS, "", "delete needs the admin bind"
    rdns = split_dn(dn)
    if rdns is None or not rdns:
        return INVALID_DN_SYNTAX, "", f"invalid DN '{dn}'"
    n = norm_dn(rdns)
    if n not in st.entries:
        return NO_SUCH_OBJECT, matched_dn(st, rdns), f"no entry {dn}"
    if any(len(m) > 0 and parent_of(m) == n for m in st.entries):
        return NOT_ALLOWED_ON_NON_LEAF, "", f"{dn} has children"
    del st.entries[n]
    return SUCCESS, "", ""


def do_modify(st, r, admin, bound):
    dn = r.text()
    changes = []
    rr = Reader(r.expect(0x30))
    while not rr.done():
        one = Reader(rr.expect(0x30))
        op = one.int(0x0A)
        mod = Reader(one.expect(0x30))
        t = mod.text()
        vals = Reader(mod.expect(0x31))
        vs = []
        while not vals.done():
            vs.append(vals.text())
        if op not in (0, 1, 2):
            return PROTOCOL_ERROR, "", f"unknown modify operation {op}"
        changes.append((op, t, vs))
    rdns = split_dn(dn)
    if rdns is None or not rdns:
        return INVALID_DN_SYNTAX, "", f"invalid DN '{dn}'"
    n = norm_dn(rdns)
    if n not in st.entries:
        return NO_SUCH_OBJECT, matched_dn(st, rdns), f"no entry {dn}"
    self_mail = bound == n and all(ascii_lower(t) == "mail" for _, t, _ in changes)
    if not admin and not self_mail:
        return INSUFFICIENT_ACCESS, "", "only the admin, or a user changing their own mail"
    e = st.entries[n]
    attrs = [(t, list(vs)) for t, vs in e.attrs]
    for op, t, vs in changes:
        lt = ascii_lower(t)
        at = next((i for i, (x, _) in enumerate(attrs) if ascii_lower(x) == lt), None)
        if op == 0:
            if at is None:
                attrs.append((t, list(vs)))
                continue
            for v in vs:
                if any(value_cmp(t, x, v) == 0 for x in attrs[at][1]):
                    return ATTRIBUTE_OR_VALUE_EXISTS, "", f"{t} already has {v}"
                attrs[at][1].append(v)
        elif op == 1:
            if at is None:
                return NO_SUCH_ATTRIBUTE, "", f"no {t} to delete"
            if not vs:
                attrs.pop(at)
                continue
            for v in vs:
                k = next((i for i, x in enumerate(attrs[at][1]) if value_cmp(t, x, v) == 0), None)
                if k is None:
                    return NO_SUCH_ATTRIBUTE, "", f"{t} has no value {v}"
                attrs[at][1].pop(k)
            if not attrs[at][1]:
                attrs.pop(at)
        else:
            if at is None:
                if vs:
                    attrs.append((t, list(vs)))
            elif vs:
                attrs[at] = (attrs[at][0], list(vs))
            else:
                attrs.pop(at)
    if not any(ascii_lower(t) == "objectclass" for t, _ in attrs):
        return OBJECT_CLASS_VIOLATION, "", "an entry needs an objectClass"
    rt, rv = rdns[0]
    rvs = next((vs for t, vs in attrs if ascii_lower(t) == ascii_lower(rt)), [])
    if not any(value_cmp(rt, x, unescape(rv)) == 0 for x in rvs):
        return NAMING_VIOLATION, "", f"the change removes the naming value {unescape(rv)}"
    e.attrs = attrs
    return SUCCESS, "", ""


def do_compare(st, r, admin):
    dn = r.text()
    ava = Reader(r.expect(0x30))
    t = ava.text()
    v = ava.text()
    rdns = split_dn(dn)
    if rdns is None:
        return INVALID_DN_SYNTAX, "", f"invalid DN '{dn}'"
    n = norm_dn(rdns)
    if n not in st.entries:
        return NO_SUCH_OBJECT, matched_dn(st, rdns), f"no entry {dn}"
    vs = visible(st.entries[n], t, admin)
    if vs is None:
        return NO_SUCH_ATTRIBUTE, "", f"no {t}"
    if any(value_cmp(t, x, v) == 0 for x in vs):
        return COMPARE_TRUE, "", ""
    return COMPARE_FALSE, "", ""


def do_bind(st, r):
    version = r.int()
    name = r.text()
    tag, cred = r.read()
    if version != 3:
        return PROTOCOL_ERROR, "", f"version {version} is not supported", None
    if tag != 0x80:
        return AUTH_METHOD_NOT_SUPPORTED, "", "only simple binds", None
    password = cred.decode()
    if name == "" and password == "":
        return SUCCESS, "", "", ""
    rdns = split_dn(name)
    if rdns is None:
        return INVALID_DN_SYNTAX, "", f"invalid DN '{name}'", None
    n = norm_dn(rdns)
    e = st.entries.get(n)
    pw = e.get("userPassword") if e else None
    if not pw or password not in pw:
        return INVALID_CREDENTIALS, "", "", None
    return SUCCESS, "", "", n


def count(d, k):
    d[k] = d.get(k, 0) + 1


def read_message(f):
    head = f.read(2)
    if len(head) == 0:
        return None
    if len(head) < 2:
        raise BerError("truncated")
    data = head
    first = head[1]
    if first >= 0x80:
        k = first & 0x7F
        if k == 0 or k > 3:
            raise BerError("bad length")
        more = f.read(k)
        if len(more) < k:
            raise BerError("truncated")
        data += more
        n = int.from_bytes(more, "big")
    else:
        n = first
    if n > MAX_MESSAGE:
        raise BerError("message too large")
    body = f.read(n)
    if len(body) < n:
        raise BerError("truncated")
    return data + body


def serve(st, conn):
    f = conn.makefile("rb")
    bound = ""

    def send(b):
        conn.sendall(b)

    try:
        while True:
            try:
                raw = read_message(f)
                if raw is None:
                    break
                outer = Reader(raw)
                msg = Reader(outer.expect(0x30))
                mid = msg.int()
                tag, body = msg.read()
            except BerError as e:
                notice = seq(0x78, [enc_int(0x0A, PROTOCOL_ERROR), enc_str(0x04, ""),
                                    enc_str(0x04, f"malformed message: {e}"),
                                    enc_str(0x8A, NOTICE_OID)])
                send(seq(0x30, [enc_int(0x02, 0), notice]))
                with st.lock:
                    count(st.ops, "malformed")
                break
            if tag == 0x42:
                with st.lock:
                    count(st.ops, "unbind")
                break
            r = Reader(body)
            with st.lock:
                admin = bound == norm_dn(split_dn(ADMIN))
                try:
                    if tag == 0x60:
                        count(st.ops, "bind")
                        code, m, text, who = do_bind(st, r)
                        bound = who if who is not None else ""
                        rtag = 0x61
                    elif tag == 0x63:
                        count(st.ops, "search")
                        code, m, text, rtag = do_search(st, mid, r, admin, send)
                    elif tag == 0x68:
                        count(st.ops, "add")
                        code, m, text = do_add(st, r, admin)
                        rtag = 0x69
                    elif tag == 0x4A:
                        count(st.ops, "delete")
                        code, m, text = do_delete(st, body, admin)
                        rtag = 0x6B
                    elif tag == 0x66:
                        count(st.ops, "modify")
                        code, m, text = do_modify(st, r, admin, bound)
                        rtag = 0x67
                    elif tag == 0x6E:
                        count(st.ops, "compare")
                        code, m, text = do_compare(st, r, admin)
                        rtag = 0x6F
                    else:
                        count(st.ops, "unknown")
                        code, m, text, rtag = PROTOCOL_ERROR, "", f"unsupported operation 0x{tag:02x}", 0x78
                except BerError as e:
                    code, m, text = PROTOCOL_ERROR, "", f"bad request: {e}"
                    rtag = {0x60: 0x61, 0x63: 0x65, 0x68: 0x69, 0x4A: 0x6B, 0x66: 0x67, 0x6E: 0x6F}.get(tag, 0x78)
                count(st.results, code)
            send(seq(0x30, [enc_int(0x02, mid), result(rtag, code, m, text)]))
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


# ---- the clients: RFC 4515 filter strings to BER


def filter_ber(s):
    f, rest = parse_filter(s, 0)
    if rest != len(s):
        raise ValueError(f"trailing text in filter '{s}'")
    return f


def unescape_filter(v):
    out = bytearray()
    i = 0
    b = v.encode()
    while i < len(b):
        if b[i] == 0x5C:
            if i + 2 >= len(b):
                raise ValueError("bad escape")
            out.append(int(b[i + 1:i + 3].decode(), 16))
            i += 3
        else:
            out.append(b[i])
            i += 1
    return bytes(out)


def parse_filter(s, i):
    if i >= len(s) or s[i] != "(":
        raise ValueError(f"expected '(' at {i}")
    i += 1
    if s[i] in "&|":
        tag = 0xA0 if s[i] == "&" else 0xA1
        i += 1
        parts = []
        while i < len(s) and s[i] == "(":
            p, i = parse_filter(s, i)
            parts.append(p)
        if i >= len(s) or s[i] != ")":
            raise ValueError(f"expected ')' at {i}")
        return seq(tag, parts), i + 1
    if s[i] == "!":
        p, i = parse_filter(s, i + 1)
        if i >= len(s) or s[i] != ")":
            raise ValueError(f"expected ')' at {i}")
        return seq(0xA2, [p]), i + 1
    end = s.find(")", i)
    if end < 0:
        raise ValueError("unclosed filter")
    item = s[i:end]
    for op, tag in ((">=", 0xA5), ("<=", 0xA6), ("~=", 0xA8)):
        k = item.find(op)
        if k > 0:
            return seq(tag, [enc_str(0x04, item[:k]), enc_str(0x04, unescape_filter(item[k + 2:]))]), end + 1
    k = item.find("=")
    if k <= 0:
        raise ValueError(f"no '=' in '{item}'")
    t, v = item[:k], item[k + 1:]
    if v == "*":
        return enc_str(0x87, t), end + 1
    if "*" in v:
        pieces = v.split("*")
        subs = []
        if pieces[0]:
            subs.append(enc_str(0x80, unescape_filter(pieces[0])))
        for p in pieces[1:-1]:
            if p:
                subs.append(enc_str(0x81, unescape_filter(p)))
        if pieces[-1]:
            subs.append(enc_str(0x82, unescape_filter(pieces[-1])))
        return seq(0xA4, [enc_str(0x04, t), seq(0x30, subs)]), end + 1
    return seq(0xA3, [enc_str(0x04, t), enc_str(0x04, unescape_filter(v))]), end + 1


SCOPES = {"base": 0, "one": 1, "sub": 2}


class Client:
    def __init__(self, name):
        self.name = name
        self.sock = socket.create_connection(ADDR)
        self.sock.settimeout(2.0)
        self.f = self.sock.makefile("rb")
        self.next_id = 1

    def say(self, line):
        print(f"{self.name} {line}")

    def send(self, op):
        mid = self.next_id
        self.next_id += 1
        self.sock.sendall(seq(0x30, [enc_int(0x02, mid), op]))
        return mid

    def recv(self):
        """(messageID, tag, body), or None at the close."""
        try:
            raw = read_message(self.f)
        except BerError as e:
            self.say(f"<- bad message: {e}")
            return None
        if raw is None:
            return None
        msg = Reader(Reader(raw).expect(0x30))
        mid = msg.int()
        tag, body = msg.read()
        return mid, tag, body

    def result_line(self, mid, got):
        if got is None:
            return "(closed)"
        rid, tag, body = got
        r = Reader(body)
        code = r.int(0x0A)
        matched = r.text()
        text = r.text()
        out = RESULT_NAMES.get(code, str(code))
        if rid != mid:
            out += f" (for message {rid}, sent {mid})"
        if matched:
            out += f" matched={matched}"
        if text:
            out += f": {text}"
        if tag == 0x78 and not r.done():
            out += f" [{r.text(0x8A)}]"
        return out

    def bind(self, dn, password, version=3):
        op = seq(0x60, [enc_int(0x02, version), enc_str(0x04, dn), enc_str(0x80, password)])
        mid = self.send(op)
        who = dn or "anonymous"
        self.say(f"bind {who} -> {self.result_line(mid, self.recv())}")

    def search(self, base, scope, flt, attrs=(), size_limit=0, types_only=False):
        op = seq(0x63, [
            enc_str(0x04, base), enc_int(0x0A, SCOPES[scope]), enc_int(0x0A, 0),
            enc_int(0x02, size_limit), enc_int(0x02, 0), tlv(0x01, b"\xff" if types_only else b"\x00"),
            filter_ber(flt), seq(0x30, [enc_str(0x04, a) for a in attrs])])
        mid = self.send(op)
        entries = []
        while True:
            got = self.recv()
            if got is None or got[1] != 0x64:
                break
            r = Reader(got[2])
            dn = r.text()
            attrs_r = Reader(r.expect(0x30))
            shown = []
            while not attrs_r.done():
                one = Reader(attrs_r.expect(0x30))
                t = one.text()
                vals = Reader(one.expect(0x31))
                vs = []
                while not vals.done():
                    vs.append(vals.text())
                shown.append(f"{t}={'|'.join(vs)}" if vs else t)
            entries.append(f"  {dn}" + (": " + "; ".join(shown) if shown else ""))
        what = f"search {scope} {base or '(root)'} {flt}"
        if attrs:
            what += " [" + ",".join(attrs) + "]"
        if size_limit:
            what += f" limit {size_limit}"
        self.say(f"{what} -> {len(entries)} entries, {self.result_line(mid, got)}")
        for line in entries:
            print(line)

    def add(self, dn, attrs):
        op = seq(0x68, [enc_str(0x04, dn), seq(0x30, [
            seq(0x30, [enc_str(0x04, t), seq(0x31, [enc_str(0x04, v) for v in vs])]) for t, vs in attrs])])
        mid = self.send(op)
        self.say(f"add {dn} -> {self.result_line(mid, self.recv())}")

    def delete(self, dn):
        mid = self.send(enc_str(0x4A, dn))
        self.say(f"delete {dn} -> {self.result_line(mid, self.recv())}")

    def modify(self, dn, changes):
        ops = {"add": 0, "delete": 1, "replace": 2}
        op = seq(0x66, [enc_str(0x04, dn), seq(0x30, [
            seq(0x30, [enc_int(0x0A, ops[o]), seq(0x30, [enc_str(0x04, t), seq(0x31, [enc_str(0x04, v) for v in vs])])])
            for o, t, vs in changes])])
        mid = self.send(op)
        what = ", ".join(f"{o} {t}" + (f" {'|'.join(vs)}" if vs else "") for o, t, vs in changes)
        self.say(f"modify {dn} ({what}) -> {self.result_line(mid, self.recv())}")

    def compare(self, dn, t, v):
        op = seq(0x6E, [enc_str(0x04, dn), seq(0x30, [enc_str(0x04, t), enc_str(0x04, v)])])
        mid = self.send(op)
        self.say(f"compare {dn} {t}={v} -> {self.result_line(mid, self.recv())}")

    def raw(self, label, data):
        self.sock.sendall(data)
        got = self.recv()
        self.say(f"{label} -> {self.result_line(0, got)}")
        after = self.recv()
        self.say(f"then -> {'(closed)' if after is None else 'more'}")

    def unbind(self):
        self.send(tlv(0x42, b""))
        got = self.recv()
        self.say(f"unbind -> {'(closed)' if got is None else 'more'}")
        self.close()

    def close(self):
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

    print("== anonymous reads")
    anon = Client("anon")
    anon.bind("", "")
    anon.search(BASE, "one", "(objectClass=*)", ["ou"])
    anon.search(BASE, "sub", "(&(objectClass=person)(uidNumber>=1002))", ["cn", "uidNumber"])
    anon.search("ou=people," + BASE, "one", "(|(cn=*liddell)(sn=Dan*)(mail=*@kara.test))", ["uid"])
    anon.search("ou=people," + BASE, "sub", "(cn=b*b*r)", ["cn"])
    anon.search(BASE, "sub", "(!(objectClass=posixAccount))", ["1.1"])
    anon.search(BASE, "sub", "(cn=Smith\\2c John)", ["cn", "mail"])
    anon.search("CN=smith\\, john,OU=People," + BASE, "base", "(objectClass=*)", ["sn"])
    anon.search("uid=bob,ou=people," + BASE, "base", "(objectClass=*)")
    anon.search(BASE, "sub", "(userPassword=*)", ["uid"])
    anon.search(BASE, "sub", "(uidNumber<=1001)", ["uid"], types_only=True)
    anon.search("ou=people," + BASE, "one", "(objectClass=person)", ["uid"], size_limit=2)
    anon.search("ou=robots," + BASE, "sub", "(objectClass=*)")
    anon.search("not a dn", "sub", "(objectClass=*)")
    anon.compare("uid=alice,ou=people," + BASE, "mail", "ALICE@KARA.TEST")
    anon.compare("uid=alice,ou=people," + BASE, "uidNumber", "1002")
    anon.compare("uid=alice,ou=people," + BASE, "userPassword", "not-a-real-secret/alice")
    anon.compare("uid=zed,ou=people," + BASE, "cn", "Zed")
    anon.add("uid=mallory,ou=people," + BASE, [("objectClass", ["person"]), ("uid", ["mallory"])])
    anon.unbind()

    print("== alice")
    alice = Client("alice")
    alice.bind("uid=alice,ou=people," + BASE, "wrong")
    alice.bind("uid=nobody,ou=people," + BASE, "x")
    alice.bind("uid=alice,ou=people," + BASE, "not-a-real-secret/alice")
    alice.modify("uid=alice,ou=people," + BASE, [("add", "mail", ["liddell@kara.test"])])
    alice.modify("uid=alice,ou=people," + BASE, [("add", "mail", ["Alice@Kara.Test"])])
    alice.modify("uid=alice,ou=people," + BASE, [("replace", "cn", ["Alice"])])
    alice.modify("uid=bob,ou=people," + BASE, [("delete", "mail", ["bob@kara.test"])])
    alice.search("uid=alice,ou=people," + BASE, "base", "(objectClass=*)", ["mail"])
    alice.delete("uid=carol,ou=people," + BASE)
    alice.unbind()

    print("== admin")
    admin = Client("admin")
    admin.bind(ADMIN, ADMIN_PASSWORD)
    admin.add("uid=dave,ou=people," + BASE, [
        ("objectClass", ["person", "posixAccount"]), ("uid", ["dave"]), ("cn", ["Dave Lister"]),
        ("sn", ["Lister"]), ("uidNumber", ["1003"]), ("userPassword", ["not-a-real-secret/dave"])])
    admin.add("uid=dave,ou=people," + BASE, [("objectClass", ["person"]), ("uid", ["dave"])])
    admin.add("uid=eve,ou=robots," + BASE, [("objectClass", ["person"]), ("uid", ["eve"])])
    admin.add("uid=eve,ou=people," + BASE, [("uid", ["eve"])])
    admin.add("uid=eve,ou=people," + BASE, [("objectClass", ["person"]), ("cn", ["Eve"])])
    admin.add("uid eve,ou=people," + BASE, [("objectClass", ["person"])])
    admin.modify("uid=bob,ou=people," + BASE, [
        ("delete", "mail", ["BOB@kara.test"]), ("replace", "uidNumber", ["2002"]),
        ("add", "description", ["builds things", "fixes things"])])
    admin.modify("uid=bob,ou=people," + BASE, [("delete", "mail", ["nobody@kara.test"]), ("replace", "sn", ["Gone"])])
    admin.modify("uid=bob,ou=people," + BASE, [("delete", "telephoneNumber", [])])
    admin.modify("uid=bob,ou=people," + BASE, [("delete", "uid", [])])
    admin.modify("uid=bob,ou=people," + BASE, [("replace", "objectClass", [])])
    admin.modify("uid=bob,ou=people," + BASE, [("delete", "description", [])])
    admin.search("uid=bob,ou=people," + BASE, "base", "(objectClass=*)")
    admin.search(BASE, "sub", "(uidNumber>=1003)", ["uid", "uidNumber", "userPassword"])
    admin.compare("uid=dave,ou=people," + BASE, "userPassword", "not-a-real-secret/dave")
    admin.compare("uid=dave,ou=people," + BASE, "userPassword", "NOT-A-REAL-SECRET/DAVE")
    admin.delete("ou=people," + BASE)
    admin.delete("uid=carol,ou=people," + BASE)
    admin.delete("uid=carol,ou=people," + BASE)
    admin.delete("cn=Smith\\, John,ou=people," + BASE)
    admin.unbind()

    print("== old client")
    old = Client("old")
    old.bind("uid=alice,ou=people," + BASE, "not-a-real-secret/alice", version=2)
    old.close()

    print("== broken client")
    bad = Client("broken")
    bad.raw("indefinite length", bytes([0x30, 0x80, 0x02, 0x01, 0x01, 0x00, 0x00]))
    bad.close()

    print("== final tree")
    last = Client("anon")
    last.search(BASE, "sub", "(objectClass=*)", ["1.1"])
    last.unbind()

    server.join()
    with st.lock:
        ops = " ".join(f"{k}={st.ops[k]}" for k in sorted(st.ops))
        res = " ".join(f"{k}={st.results[k]}" for k in sorted(st.results))
    print(f"== server: {len(st.entries)} entries")
    print(f"ops {ops}")
    print(f"results {res}")
    listener.close()


if __name__ == "__main__":
    main()
