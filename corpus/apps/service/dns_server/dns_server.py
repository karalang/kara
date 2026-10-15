"""Mirror of dns_server.kara: an authoritative DNS server over TCP.

IPv4/IPv6 text forms come from Python's `ipaddress` (RFC 5952 output);
the zone parser, wire format and server logic are a port of the Kara.
"""
import ipaddress
import socket

ZONE = """$ORIGIN example.test.
$TTL 300
@        IN SOA ns1 hostmaster 2026101501 7200 900 1209600 60
@        IN NS  ns1
@        IN NS  ns2.backup.test.
@        IN MX  10 mail
@        IN MX  20 mail.backup.test.
ns1      IN A   10.0.0.53
www      IN A   10.0.0.10
www      IN A   10.0.0.11
www      IN AAAA 2001:db8:0:0:0:0:0:10
api  600 IN CNAME edge
edge     IN CNAME www
mail     IN A   10.0.0.25
v6       IN AAAA 2001:0db8:0000:0000:0001:0000:0000:0001
txt      IN TXT "hello world" "v=spf1 -all"
*.dev    IN A   10.0.9.9
gone     IN CNAME old.elsewhere.test.
"""

TYPES = {"A": 1, "NS": 2, "CNAME": 5, "SOA": 6, "MX": 15, "TXT": 16, "AAAA": 28, "ANY": 255}
NAMES = {v: k for k, v in TYPES.items()}
RCODES = {0: "NOERROR", 1: "FORMERR", 3: "NXDOMAIN", 4: "NOTIMP", 5: "REFUSED"}
PORT = 47743


def type_name(t):
    return NAMES.get(t, f"TYPE{t}")


class Fail(Exception):
    pass


class Record:
    def __init__(self, name, ttl, rtype, data):
        self.name, self.ttl, self.rtype, self.data = name, ttl, rtype, data


def absolute(name, origin):
    if name == "@":
        return origin
    if name.endswith("."):
        return name.lower()
    return f"{name.lower()}.{origin}"


def fields(line):
    out, cur, quoted, in_field = [], "", False, False
    for c in line:
        if quoted:
            if c == '"':
                quoted = False
                out.append(cur)
                cur, in_field = "", False
            else:
                cur += c
        elif c == '"':
            quoted, in_field = True, True
        elif c in " \t":
            if in_field:
                out.append(cur)
                cur, in_field = "", False
        elif c == ";":
            break
        else:
            cur += c
            in_field = True
    if in_field:
        out.append(cur)
    return out


def parse_ipv4(s):
    parts = s.split(".")
    if len(parts) != 4:
        return None
    try:
        nums = [int(p) for p in parts]
    except ValueError:
        return None
    if any(n < 0 or n > 255 for n in nums):
        return None
    return bytes(nums)


def parse_ipv6(s):
    try:
        return ipaddress.IPv6Address(s).packed
    except ValueError:
        return None


def parse_zone(text):
    origin, ttl, records = "", 3600, []
    for line_no, line in enumerate(text.splitlines(), 1):
        f = fields(line)
        if not f:
            continue
        if f[0] == "$ORIGIN":
            origin = f[1].lower()
            continue
        if f[0] == "$TTL":
            ttl = int(f[1])
            continue
        name = absolute(f[0], origin)
        at, rttl = 1, ttl
        if f[at].lstrip("-").isdigit():
            rttl = int(f[at])
            at += 1
        if f[at] == "IN":
            at += 1
        if f[at] not in TYPES:
            raise Fail(f"line {line_no}: unknown type {f[at]}")
        rtype = TYPES[f[at]]
        at += 1
        need = 2 if rtype == 15 else 7 if rtype == 6 else 1
        if len(f) < at + need:
            raise Fail(f"line {line_no}: {type_name(rtype)} needs {need} fields")
        if rtype == 1:
            b = parse_ipv4(f[at])
            if b is None:
                raise Fail(f"line {line_no}: bad IPv4 address {f[at]}")
            data = b
        elif rtype == 28:
            b = parse_ipv6(f[at])
            if b is None:
                raise Fail(f"line {line_no}: bad IPv6 address {f[at]}")
            data = b
        elif rtype == 15:
            data = (int(f[at]), absolute(f[at + 1], origin))
        elif rtype == 16:
            data = f[at:]
        elif rtype == 6:
            data = (absolute(f[at], origin), absolute(f[at + 1], origin), [int(x) for x in f[at + 2:at + 7]])
        else:
            data = absolute(f[at], origin)
        records.append(Record(name, rttl, rtype, data))
    if not origin:
        raise Fail("no $ORIGIN")
    return origin, records


class Writer:
    def __init__(self):
        self.buf = bytearray()
        self.names = {}

    def u8(self, v):
        self.buf.append(v & 0xff)

    def u16(self, v):
        self.buf += bytes([(v >> 8) & 0xff, v & 0xff])

    def u32(self, v):
        self.u16((v >> 16) & 0xffff)
        self.u16(v & 0xffff)

    def name(self, name):
        rest = name
        while rest and rest != ".":
            if rest in self.names:
                self.u16(0xc000 | self.names[rest])
                return
            if len(self.buf) < 0x3fff:
                self.names[rest] = len(self.buf)
            dot = rest.find(".")
            if dot < 0:
                dot = len(rest)
            label = rest[:dot]
            self.u8(len(label))
            self.buf += label.encode()
            rest = "" if dot + 1 >= len(rest) else rest[dot + 1:]
        self.u8(0)


def write_rdata(w, rtype, d):
    at = len(w.buf)
    w.u16(0)
    if rtype in (1, 28):
        w.buf += d
    elif rtype == 15:
        w.u16(d[0])
        w.name(d[1])
    elif rtype == 16:
        for p in d:
            w.u8(len(p))
            w.buf += p.encode()
    elif rtype == 6:
        w.name(d[0])
        w.name(d[1])
        for n in d[2]:
            w.u32(n)
    else:
        w.name(d)
    n = len(w.buf) - at - 2
    w.buf[at] = (n >> 8) & 0xff
    w.buf[at + 1] = n & 0xff


def write_rr(w, owner, r):
    w.name(owner)
    w.u16(r.rtype)
    w.u16(1)
    w.u32(r.ttl)
    write_rdata(w, r.rtype, r.data)


class Reader:
    def __init__(self, buf):
        self.buf, self.pos = buf, 0

    def u8(self):
        if self.pos >= len(self.buf):
            raise Fail("message truncated")
        v = self.buf[self.pos]
        self.pos += 1
        return v

    def u16(self):
        return (self.u8() << 8) | self.u8()

    def u32(self):
        return (self.u16() << 16) | self.u16()

    def name(self):
        out, at, jumped, hops = "", self.pos, False, 0
        while True:
            if at >= len(self.buf):
                raise Fail("name runs off the end")
            n = self.buf[at]
            if n == 0:
                if not jumped:
                    self.pos = at + 1
                break
            if n & 0xc0 == 0xc0:
                if at + 1 >= len(self.buf):
                    raise Fail("pointer cut short")
                target = ((n & 0x3f) << 8) | self.buf[at + 1]
                if target >= at or hops > 20:
                    raise Fail("bad compression pointer")
                if not jumped:
                    self.pos = at + 2
                jumped = True
                hops += 1
                at = target
                continue
            if n > 63 or at + 1 + n > len(self.buf):
                raise Fail("bad label")
            out += self.buf[at + 1:at + 1 + n].decode("latin-1").lower() + "."
            at += 1 + n
        return out or "."


class FormErr(Exception):
    def __init__(self, qid, msg):
        self.qid, self.msg = qid, msg


def parse_query(msg):
    r = Reader(msg)
    try:
        qid = r.u16()
    except Fail as e:
        raise FormErr(-1, str(e))
    try:
        flags = r.u16()
        qd = r.u16()
        r.u16(); r.u16(); r.u16()
        if flags & 0x8000:
            raise FormErr(qid, "a response, not a query")
        opcode = (flags >> 11) & 0xf
        if qd != 1:
            raise FormErr(qid, f"{qd} questions")
        qname = r.name()
        qtype = r.u16()
        qclass = r.u16()
    except Fail as e:
        raise FormErr(qid, str(e))
    if qclass != 1:
        raise FormErr(qid, f"class {qclass}")
    return qid, opcode, bool(flags & 0x0100), qname, qtype


def in_zone(name, origin):
    return name == origin or name.endswith("." + origin)


def name_exists(records, name):
    return any(r.name == name or r.name.endswith("." + name) for r in records)


def owner_records(records, name):
    out = [i for i, r in enumerate(records) if r.name == name]
    if out or name_exists(records, name):
        return out
    dot = name.find(".")
    if dot >= 0:
        wild = "*" + name[dot:]
        out = [i for i, r in enumerate(records) if r.name == wild]
    return out


def header(w, qid, opcode, aa, rd, rcode, qd, an, ns):
    w.u16(qid)
    flags = 0x8000 | (opcode << 11) | rcode
    if aa:
        flags |= 0x0400
    if rd:
        flags |= 0x0100
    w.u16(flags)
    w.u16(qd); w.u16(an); w.u16(ns); w.u16(0)


def error_reply(qid, opcode, rcode):
    w = Writer()
    header(w, 0 if qid < 0 else qid, opcode, False, False, rcode, 0, 0, 0)
    return bytes(w.buf)


def answer(zone, msg):
    origin, records = zone
    try:
        qid, opcode, rd, qname, qtype = parse_query(msg)
    except FormErr as e:
        return error_reply(e.qid, 0, 1), f"FORMERR ({e.msg})"
    if opcode != 0:
        return error_reply(qid, opcode, 4), f"NOTIMP (opcode {opcode})"
    if not in_zone(qname, origin):
        w = Writer()
        header(w, qid, 0, False, rd, 5, 1, 0, 0)
        w.name(qname); w.u16(qtype); w.u16(1)
        return bytes(w.buf), f"REFUSED {qname}"
    answers, name, rcode, chased = [], qname, 0, 0
    while True:
        idx = owner_records(records, name)
        if not idx:
            if not name_exists(records, name):
                rcode = 3
            break
        cname = None
        for i in idx:
            r = records[i]
            if r.rtype == 5 and qtype != 5:
                answers.append((name, i))
                cname = r.data
        if cname is not None:
            chased += 1
            if chased > 8 or not in_zone(cname, origin):
                break
            name = cname
            continue
        for i in idx:
            if records[i].rtype == qtype or qtype == 255:
                answers.append((name, i))
        break
    soa = next(i for i, r in enumerate(records) if r.rtype == 6)
    with_soa = not answers
    w = Writer()
    header(w, qid, 0, True, rd, rcode, 1, len(answers), 1 if with_soa else 0)
    w.name(qname); w.u16(qtype); w.u16(1)
    for owner, i in answers:
        write_rr(w, owner, records[i])
    if with_soa:
        write_rr(w, origin, records[soa])
    what = "NXDOMAIN" if rcode == 3 else "NODATA" if not answers else "NOERROR"
    return bytes(w.buf), f"{what} {qname} {type_name(qtype)} answers={len(answers)}"


def query(qid, name, qtype):
    w = Writer()
    w.u16(qid); w.u16(0x0100); w.u16(1); w.u16(0); w.u16(0); w.u16(0)
    w.name(name); w.u16(qtype); w.u16(1)
    return bytearray(w.buf)


def rdata_text(r, rtype, n):
    end = r.pos + n
    if rtype == 1:
        text = str(ipaddress.IPv4Address(bytes(r.u8() for _ in range(4))))
    elif rtype == 28:
        text = str(ipaddress.IPv6Address(bytes(r.u8() for _ in range(16))))
    elif rtype == 15:
        p = r.u16()
        text = f"{p} {r.name()}"
    elif rtype == 16:
        parts = []
        while r.pos < end:
            k = r.u8()
            parts.append('"' + "".join(chr(r.u8()) for _ in range(k)) + '"')
        text = " ".join(parts)
    elif rtype == 6:
        m = r.name()
        rn = r.name()
        text = f"{m} {rn} " + " ".join(str(r.u32()) for _ in range(5))
    elif rtype in (2, 5):
        text = r.name()
    else:
        r.pos = end
        text = "?"
    if r.pos != end:
        raise Fail(f"rdata length {n} but read {n - (end - r.pos)}")
    return text


def print_reply(msg):
    r = Reader(msg)
    qid, flags, qd, an, ns, ar = (r.u16() for _ in range(6))
    fl = ["qr"] + (["aa"] if flags & 0x0400 else []) + (["rd"] if flags & 0x0100 else [])
    print(f"  ;; id {qid} {RCODES.get(flags & 0xf, f'RCODE{flags & 0xf}')} flags: {' '.join(fl)}; "
          f"QUERY: {qd}, ANSWER: {an}, AUTHORITY: {ns}, ADDITIONAL: {ar}")
    for _ in range(qd):
        n = r.name()
        t = r.u16()
        r.u16()
        print(f"  ;{n} IN {type_name(t)}")
    for k in range(an + ns):
        n = r.name()
        t = r.u16()
        r.u16()
        ttl = r.u32()
        ln = r.u16()
        text = rdata_text(r, t, ln)
        print(f"  {'' if k < an else 'auth '}{n} {ttl} IN {type_name(t)} {text}")
    if r.pos != len(r.buf):
        raise Fail(f"{len(r.buf) - r.pos} trailing bytes")
    print(f"  ;; {len(msg)} bytes")


def frame_msg(out, msg):
    out += bytes([(len(msg) >> 8) & 0xff, len(msg) & 0xff]) + bytes(msg)


def read_all(s):
    out = b""
    while True:
        b = s.recv(64)
        if not b:
            return out
        out += b


def unframe(data):
    out, at = [], 0
    while at + 2 <= len(data):
        n = (data[at] << 8) | data[at + 1]
        if at + 2 + n > len(data):
            break
        out.append(data[at + 2:at + 2 + n])
        at += 2 + n
    return out, len(data) - at


def session(listener, zone, label, msgs):
    print(f"== {label}")
    client = socket.create_connection(("127.0.0.1", PORT))
    wire = bytearray()
    for m in msgs:
        frame_msg(wire, m)
    client.sendall(bytes(wire))
    client.shutdown(socket.SHUT_WR)
    conn, _ = listener.accept()
    queries, leftover = unframe(read_all(conn))
    out = bytearray()
    for q in queries:
        reply, log = answer(zone, q)
        print(f"  server: {log}")
        frame_msg(out, reply)
    if leftover > 0:
        print(f"  server: dropped {leftover} bytes of a cut-off message")
    conn.sendall(bytes(out))
    conn.close()
    replies, _ = unframe(read_all(client))
    client.close()
    for m in replies:
        try:
            print_reply(m)
        except Fail as e:
            print(f"  bad reply: {e}")


def main():
    with open("zone.db", "w") as f:
        f.write(ZONE)
    with open("zone.db") as f:
        zone = parse_zone(f.read())
    print(f"zone {zone[0]}: {len(zone[1])} records")
    try:
        parse_zone("$ORIGIN x.test.\nwww IN A 10.0.0.300\n")
        print("bad zone accepted")
    except Fail as e:
        print(f"bad zone refused: {e}")
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", PORT))
    listener.listen(16)
    session(listener, zone, "pipelined lookups", [
        query(1, "www.example.test.", 1),
        query(2, "WWW.Example.Test.", 28),
        query(3, "example.test.", 15),
        query(4, "example.test.", 2),
        query(5, "v6.example.test.", 28),
        query(6, "txt.example.test.", 16),
    ])
    session(listener, zone, "cnames, wildcards, misses", [
        query(7, "api.example.test.", 1),
        query(8, "api.example.test.", 5),
        query(9, "build.dev.example.test.", 1),
        query(10, "gone.example.test.", 1),
        query(11, "nope.example.test.", 1),
        query(12, "mail.example.test.", 15),
        query(13, "example.test.", 6),
        query(14, "www.example.test.", 255),
    ])
    opcode2 = query(16, "www.example.test.", 1)
    opcode2[2] = 0x11
    loop_ptr = query(17, "www.example.test.", 1)
    loop_ptr[12] = 0xc0
    loop_ptr[13] = 0x0c
    cut = query(18, "www.example.test.", 1)[:20]
    session(listener, zone, "errors", [query(15, "www.other.test.", 1), opcode2, loop_ptr, cut])


main()
