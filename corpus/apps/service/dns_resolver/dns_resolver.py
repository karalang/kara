# Oracle for dns_resolver.kara: an iterative DNS resolver over UDP with the
# authoritative servers it walks, written with socket, struct and threads.
# Every socket is bound to 127.0.0.1 port 0; a lab table maps the servers'
# documentation addresses (192.0.2.x) to the sockets they got.
# Run: python3 -I -B dns_resolver.py > expected.out
import socket
import struct
import threading

A, NS, CNAME, SOA, MX, TXT, AAAA = 1, 2, 5, 6, 15, 16, 28
TYPE_NAMES = {A: "A", NS: "NS", CNAME: "CNAME", SOA: "SOA", MX: "MX", TXT: "TXT", AAAA: "AAAA"}
IN, CH = 1, 3
RCODES = ["NOERROR", "FORMERR", "SERVFAIL", "NXDOMAIN", "NOTIMP", "REFUSED"]
NOERROR, FORMERR, SERVFAIL, NXDOMAIN, NOTIMP, REFUSED = range(6)
QR, AA, TC, RD, RA = 0x8000, 0x0400, 0x0200, 0x0100, 0x0080
UDP_LIMIT = 512
TIMEOUT = 0.2
LAME_HOLD = 900
MAX_CHAIN = 8


def tname(t):
    return TYPE_NAMES.get(t, f"TYPE{t}")


class RR:
    def __init__(self, name, rtype, ttl, rdata):
        self.name, self.rtype, self.ttl, self.rdata = name, rtype, ttl, rdata

    def with_ttl(self, ttl):
        return RR(self.name, self.rtype, ttl, self.rdata)

    def text(self):
        return f"{self.name} {self.ttl} IN {tname(self.rtype)} {rdata_text(self.rtype, self.rdata)}"


def rdata_text(t, rd):
    if t in (NS, CNAME, A):
        return rd
    if t == MX:
        return f"{rd[0]} {rd[1]}"
    if t == SOA:
        return " ".join(str(x) for x in rd)
    if t == TXT:
        parts = []
        for s in rd:
            if len(s) > 40:
                parts.append(f'"{s[:10]}..."({len(s)})')
            else:
                parts.append(f'"{s}"')
        return " ".join(parts)
    return rd.hex()


# ------------------------------------------------------------ wire format

def enc_name(n):
    if n == ".":
        return b"\0"
    out = b""
    for label in n.rstrip(".").split("."):
        out += bytes([len(label)]) + label.encode()
    return out + b"\0"


def enc_rdata(t, rd):
    if t == A:
        return bytes(int(x) for x in rd.split("."))
    if t in (NS, CNAME):
        return enc_name(rd)
    if t == MX:
        return struct.pack(">H", rd[0]) + enc_name(rd[1])
    if t == TXT:
        return b"".join(bytes([len(s)]) + s.encode() for s in rd)
    if t == SOA:
        return enc_name(rd[0]) + enc_name(rd[1]) + struct.pack(">IIIII", *rd[2:])
    return rd


def enc_rr(rr, qname):
    owner = b"\xc0\x0c" if qname is not None and rr.name == qname else enc_name(rr.name)
    rdata = enc_rdata(rr.rtype, rr.rdata)
    return owner + struct.pack(">HHIH", rr.rtype, IN, rr.ttl, len(rdata)) + rdata


def build(qid, flags, question, an=(), ns=(), ar=(), compress=False):
    out = struct.pack(">HHHHHH", qid, flags, 1 if question else 0, len(an), len(ns), len(ar))
    qname = None
    if question:
        out += enc_name(question[0]) + struct.pack(">HH", question[1], question[2])
        if compress:
            qname = question[0]
    for rr in list(an) + list(ns) + list(ar):
        out += enc_rr(rr, qname)
    return out


def read_name(buf, pos):
    labels = []
    jumps = 0
    end = None
    while True:
        if pos >= len(buf):
            raise ValueError("name runs past the end")
        n = buf[pos]
        if n & 0xC0 == 0xC0:
            if pos + 1 >= len(buf):
                raise ValueError("cut pointer")
            target = ((n & 0x3F) << 8) | buf[pos + 1]
            if end is None:
                end = pos + 2
            jumps += 1
            if jumps > 16 or target >= pos:
                raise ValueError("bad pointer")
            pos = target
            continue
        if n == 0:
            if end is None:
                end = pos + 1
            break
        if pos + 1 + n > len(buf):
            raise ValueError("label runs past the end")
        labels.append(buf[pos + 1:pos + 1 + n].decode().lower())
        pos += 1 + n
    return ("." if not labels else ".".join(labels) + "."), end


def take(buf, pos, n):
    if pos + n > len(buf):
        raise ValueError("cut short")
    return buf[pos:pos + n]


def read_rr(buf, pos):
    name, pos = read_name(buf, pos)
    t, _cls, ttl, rdlen = struct.unpack(">HHIH", take(buf, pos, 10))
    pos += 10
    raw = take(buf, pos, rdlen)
    start = pos
    if t == A:
        rd = ".".join(str(b) for b in raw)
    elif t in (NS, CNAME):
        rd, _ = read_name(buf, start)
    elif t == MX:
        pref = struct.unpack(">H", raw[:2])[0]
        target, _ = read_name(buf, start + 2)
        rd = (pref, target)
    elif t == TXT:
        rd = []
        i = 0
        while i < len(raw):
            n = raw[i]
            rd.append(raw[i + 1:i + 1 + n].decode())
            i += 1 + n
    elif t == SOA:
        m, p = read_name(buf, start)
        r, p = read_name(buf, p)
        rd = (m, r) + struct.unpack(">IIIII", take(buf, p, 20))
    else:
        rd = raw
    return RR(name, t, ttl, rd), pos + rdlen


def parse(buf):
    qid, flags, qd, an, ns, ar = struct.unpack(">HHHHHH", take(buf, 0, 12))
    pos = 12
    questions = []
    for _ in range(qd):
        name, pos = read_name(buf, pos)
        t, c = struct.unpack(">HH", take(buf, pos, 4))
        pos += 4
        questions.append((name, t, c))
    sections = []
    for count in (an, ns, ar):
        rrs = []
        for _ in range(count):
            rr, pos = read_rr(buf, pos)
            rrs.append(rr)
        sections.append(rrs)
    return {"id": qid, "flags": flags, "q": questions,
            "an": sections[0], "ns": sections[1], "ar": sections[2]}


def under(n, zone):
    return zone == "." or n == zone or n.endswith("." + zone)


def parent(n):
    if n == ".":
        return "."
    rest = n.split(".", 1)[1]
    return rest if rest else "."


# ------------------------------------------------------------ authoritative servers

class Zone:
    def __init__(self, origin, records, delegations):
        self.origin = origin
        self.records = records
        self.delegations = delegations  # child -> (ns rrs, glue rrs)
        self.soa = next(r for r in records if r.rtype == SOA)

    def negative_soa(self):
        return self.soa.with_ttl(min(self.soa.ttl, self.soa.rdata[6]))

    def answer(self, qname, qtype):
        """(flags, rcode, answer, authority, additional)"""
        if not under(qname, self.origin):
            return 0, REFUSED, [], [], []
        for child, (ns, glue) in self.delegations.items():
            if under(qname, child):
                return 0, NOERROR, [], ns, glue
        here = [r for r in self.records if r.name == qname]
        match = [r for r in here if r.rtype == qtype]
        if match:
            return AA, NOERROR, match, [], []
        cname = [r for r in here if r.rtype == CNAME]
        if cname and qtype != CNAME:
            return AA, NOERROR, cname, [], []
        if here:
            return AA, NOERROR, [], [self.negative_soa()], []
        return AA, NXDOMAIN, [], [self.negative_soa()], []


def make_zones():
    root = Zone(".", [
        RR(".", SOA, 86400, ("a.root.test.", "nstld.root.test.", 2026111401, 1800, 900, 604800, 86400)),
        RR(".", NS, 518400, "a.root.test."),
    ], {
        "test.": ([RR("test.", NS, 172800, "ns1.nic.test.")],
                  [RR("ns1.nic.test.", A, 172800, "192.0.2.10")]),
    })
    tld = Zone("test.", [
        RR("test.", SOA, 3600, ("ns1.nic.test.", "hostmaster.nic.test.", 41, 1800, 900, 604800, 900)),
        RR("test.", NS, 172800, "ns1.nic.test."),
        RR("ns1.nic.test.", A, 172800, "192.0.2.10"),
    ], {
        "example.test.": ([RR("example.test.", NS, 86400, "ns2.example.test."),
                           RR("example.test.", NS, 86400, "ns1.example.test.")],
                          [RR("ns2.example.test.", A, 86400, "192.0.2.21"),
                           RR("ns1.example.test.", A, 86400, "192.0.2.20")]),
        "other.test.": ([RR("other.test.", NS, 86400, "ns.example.test.")], []),
    })
    big = ["".join(chr(ord("a") + (i + j) % 26) for j in range(100)) for i in range(6)]
    example = Zone("example.test.", [
        RR("example.test.", SOA, 3600, ("ns1.example.test.", "hostmaster.example.test.", 7, 3600, 600, 86400, 300)),
        RR("example.test.", NS, 86400, "ns2.example.test."),
        RR("example.test.", NS, 86400, "ns1.example.test."),
        RR("example.test.", MX, 3600, (10, "mail.example.test.")),
        RR("example.test.", MX, 3600, (20, "backup.example.test.")),
        RR("ns1.example.test.", A, 86400, "192.0.2.20"),
        RR("ns2.example.test.", A, 86400, "192.0.2.21"),
        RR("ns.example.test.", A, 3600, "192.0.2.30"),
        RR("mail.example.test.", A, 3600, "198.51.100.25"),
        RR("www.example.test.", CNAME, 600, "web.other.test."),
        RR("big.example.test.", TXT, 60, big),
        RR("loop.example.test.", CNAME, 60, "loop2.example.test."),
        RR("loop2.example.test.", CNAME, 60, "loop.example.test."),
    ], {})
    other = Zone("other.test.", [
        RR("other.test.", SOA, 3600, ("ns.example.test.", "admin.other.test.", 1, 3600, 600, 86400, 120)),
        RR("other.test.", NS, 86400, "ns.example.test."),
        RR("web.other.test.", A, 300, "198.51.100.80"),
        RR("web.other.test.", A, 300, "198.51.100.81"),
    ], {})
    return {"192.0.2.1": root, "192.0.2.10": tld, "192.0.2.20": example, "192.0.2.30": other}


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.udp = {}
        self.tcp = {}

    def count(self, table, ip):
        with self.lock:
            table[ip] = table.get(ip, 0) + 1


def respond(zone, data):
    try:
        msg = parse(data)
    except ValueError:
        return build(struct.unpack(">H", data[:2])[0], QR | FORMERR, None)
    q = msg["q"][0]
    flags, rcode, an, ns, ar = zone.answer(q[0], q[1])
    return build(msg["id"], QR | flags | rcode, q, an, ns, ar)


def serve_udp(sock, zone, stats, ip):
    while True:
        data, peer = sock.recvfrom(4096)
        if data == b"STOP":
            break
        stats.count(stats.udp, ip)
        if zone is None:
            continue  # a lame server: it takes queries and never answers
        out = respond(zone, data)
        if len(out) > UDP_LIMIT:
            msg = parse(data)
            out = build(msg["id"], QR | AA | TC, msg["q"][0])
        sock.sendto(out, peer)
    sock.close()


def recv_exact(conn, n):
    out = b""
    while len(out) < n:
        chunk = conn.recv(n - len(out))
        if not chunk:
            return None
        out += chunk
    return out


def serve_tcp(listener, zone, stats, ip):
    while True:
        conn, _ = listener.accept()
        head = recv_exact(conn, 2)
        n = struct.unpack(">H", head)[0] if head else 0
        if n == 0:
            conn.close()
            break
        data = recv_exact(conn, n)
        stats.count(stats.tcp, ip)
        out = respond(zone, data)
        conn.sendall(struct.pack(">H", len(out)) + out)
        conn.close()
    listener.close()


# ------------------------------------------------------------ the resolver

class Entry:
    def __init__(self, rrs, at, permanent=False):
        self.rrs, self.at, self.permanent = rrs, at, permanent


class Resolver:
    def __init__(self, lab, sock):
        self.lab = lab
        self.sock = sock  # where stub clients ask
        self.up = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.up.bind(("127.0.0.1", 0))
        self.up.settimeout(TIMEOUT)
        self.now = 0
        self.cache = {}
        self.neg = {}
        self.lame = {}
        self.next_id = 0x1000
        self.log = []
        self.clients = 0
        self.upstream = 0
        self.timeouts = 0
        self.cache[(".", NS)] = Entry([RR(".", NS, 518400, "a.root.test.")], 0, True)
        self.cache[("a.root.test.", A)] = Entry([RR("a.root.test.", A, 518400, "192.0.2.1")], 0, True)

    def say(self, depth, line):
        self.log.append("  " * (depth + 1) + line)

    # -- cache

    def get(self, name, rtype):
        e = self.cache.get((name, rtype))
        if e is None:
            return None
        if e.permanent:
            return e.rrs
        left = min(r.ttl for r in e.rrs) - (self.now - e.at)
        if left <= 0:
            del self.cache[(name, rtype)]
            return None
        return [r.with_ttl(left) for r in e.rrs]

    def put(self, rrs):
        sets = {}
        for r in rrs:
            sets.setdefault((r.name, r.rtype), []).append(r)
        for key, rs in sets.items():
            self.cache[key] = Entry(rs, self.now)

    def neg_get(self, name, rtype):
        for key in ((name, None), (name, rtype)):
            e = self.neg.get(key)
            if e is None:
                continue
            rcode, soa = e.rrs
            left = soa.ttl - (self.now - e.at)
            if left <= 0:
                del self.neg[key]
                continue
            return rcode, soa.with_ttl(left)
        return None

    def neg_put(self, name, rtype, rcode, soa):
        ttl = min(soa.ttl, soa.rdata[6])
        self.neg[(name, rtype)] = Entry((rcode, soa.with_ttl(ttl)), self.now)

    # -- upstream

    def query(self, ip, name, rtype, depth):
        qid = self.next_id
        self.next_id += 1
        self.upstream += 1
        pkt = build(qid, 0, (name, rtype, IN))
        self.up.sendto(pkt, self.lab[ip][0])
        while True:
            try:
                data, _ = self.up.recvfrom(4096)
            except socket.timeout:
                return None
            try:
                msg = parse(data)
            except ValueError:
                continue
            if msg["id"] == qid and msg["q"] == [(name, rtype, IN)]:
                break
        if msg["flags"] & TC:
            self.say(depth, f"{ip} {name} {tname(rtype)} -> truncated, again over TCP")
            conn = socket.create_connection(self.lab[ip][1])
            conn.sendall(struct.pack(">H", len(pkt)) + pkt)
            n = struct.unpack(">H", recv_exact(conn, 2))[0]
            msg = parse(recv_exact(conn, n))
            conn.close()
        return msg

    def closest_zone(self, name):
        n = name
        while self.get(n, NS) is None:
            n = parent(n)
        return n

    def addresses(self, zone, depth):
        names = [r.rdata for r in self.get(zone, NS)]
        out = []
        for n in names:
            a = self.get(n, A)
            if a:
                out += [r.rdata for r in a]
        if out:
            return out
        for n in names:
            if under(n, zone):
                continue  # no glue for a name inside the zone: no way in
            self.say(depth, f"no glue for {n}, resolving it")
            rcode, an, _ = self.resolve(n, A, depth + 1, True)
            out = [r.rdata for r in an if r.rtype == A and r.name == an[-1].name] if rcode == NOERROR and an else []
            if out:
                return out
        return out

    def iterate(self, name, rtype, depth):
        """Ask servers until the answer is in the cache; False on SERVFAIL."""
        if depth > 4:
            self.say(depth, "too deep")
            return False
        while True:
            zone = self.closest_zone(name)
            servers = self.addresses(zone, depth)
            if not servers:
                self.say(depth, f"no usable server for {zone}")
                return False
            referred = False
            for ip in servers:
                if self.lame.get(ip, -1) > self.now:
                    continue
                msg = self.query(ip, name, rtype, depth)
                head = f"{ip} {name} {tname(rtype)} ->"
                if msg is None:
                    self.timeouts += 1
                    self.lame[ip] = self.now + LAME_HOLD
                    self.say(depth, f"{head} timeout, held off until {self.lame[ip]}")
                    continue
                rcode = msg["flags"] & 0xF
                soa = [r for r in msg["ns"] if r.rtype == SOA]
                if rcode == NXDOMAIN and soa:
                    self.neg_put(name, None, NXDOMAIN, soa[0])
                    self.say(depth, f"{head} NXDOMAIN")
                    return True
                if rcode != NOERROR:
                    self.say(depth, f"{head} {RCODES[rcode]}")
                    continue
                keep = [r for r in msg["an"] if under(r.name, zone)]
                if keep:
                    self.put(keep)
                    first = keep[0]
                    if first.rtype == CNAME and rtype != CNAME:
                        self.say(depth, f"{head} CNAME {first.rdata}")
                    else:
                        self.say(depth, f"{head} {len(keep)} {tname(first.rtype)}")
                    return True
                if msg["flags"] & AA:
                    if soa:
                        self.neg_put(name, rtype, NOERROR, soa[0])
                    self.say(depth, f"{head} NODATA")
                    return True
                ns = [r for r in msg["ns"] if r.rtype == NS]
                if ns and under(name, ns[0].name) and ns[0].name != zone and under(ns[0].name, zone):
                    child = ns[0].name
                    glue = [r for r in msg["ar"] if r.rtype == A and under(r.name, zone)]
                    self.put(ns)
                    self.put(glue)
                    self.say(depth, f"{head} referral {child} ({len(ns)} NS, {len(glue)} glue)")
                    referred = True
                    break
                self.say(depth, f"{head} lame answer")
            if not referred:
                self.say(depth, f"every server for {zone} failed")
                return False

    def resolve(self, name, rtype, depth, recurse):
        """(rcode, answers, authority)"""
        answers = []
        cur = name
        chain = 0
        while True:
            if chain >= MAX_CHAIN:
                self.say(depth, f"CNAME chain longer than {MAX_CHAIN}")
                return SERVFAIL, [], []
            hit = self.get(cur, rtype)
            if hit:
                return NOERROR, answers + hit, []
            if rtype != CNAME:
                c = self.get(cur, CNAME)
                if c:
                    answers += c
                    cur = c[0].rdata
                    chain += 1
                    continue
            n = self.neg_get(cur, rtype)
            if n:
                return n[0], answers, [n[1]]
            if not recurse:
                return REFUSED, [], []
            if not self.iterate(cur, rtype, depth):
                return SERVFAIL, [], []

    # -- serving stubs

    def control(self, msg, q):
        labels = q[0].rstrip(".").split(".")
        if len(labels) == 3 and labels[0] == "advance" and labels[2] == "clock":
            self.now += int(labels[1])
            text = f"now={self.now}"
        elif q[0] == "shutdown.control.":
            text = "bye"
        else:
            return build(msg["id"], QR | REFUSED, q), False
        return build(msg["id"], QR | AA, q, [RR(q[0], TXT, 0, [text])], compress=True), q[0] == "shutdown.control."

    def handle(self, data):
        """(reply or None, stop)"""
        if len(data) < 12:
            return None, False
        qid, flags = struct.unpack(">HH", data[:4])
        rd = flags & RD
        try:
            msg = parse(data)
        except ValueError:
            return build(qid, QR | rd | RA | FORMERR, None), False
        if (flags >> 11) & 0xF != 0:
            return build(qid, QR | rd | RA | NOTIMP, msg["q"][0] if msg["q"] else None), False
        if len(msg["q"]) != 1:
            return build(qid, QR | rd | RA | FORMERR, None), False
        q = msg["q"][0]
        if q[2] == CH and q[1] == TXT:
            return self.control(msg, q)
        if q[2] != IN:
            return build(qid, QR | rd | RA | REFUSED, q), False
        self.clients += 1
        before = self.upstream
        self.log.append(f"#{self.clients} {q[0]} {tname(q[1])}{'' if rd else ' (no recursion)'}")
        rcode, an, ns = self.resolve(q[0], q[1], 0, rd != 0)
        self.log.append(f"  = {RCODES[rcode]}, {len(an)} answers, {self.upstream - before} upstream")
        return build(qid, QR | rd | RA | rcode, q, an, ns, compress=True), False

    def serve(self):
        while True:
            data, peer = self.sock.recvfrom(4096)
            out, stop = self.handle(data)
            if out is not None:
                self.sock.sendto(out, peer)
            if stop:
                break
        self.up.close()
        self.sock.close()


# ------------------------------------------------------------ the stub client

class Stub:
    def __init__(self, resolver_addr):
        self.addr = resolver_addr
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(5)
        self.next_id = 1

    def exchange(self, pkt):
        self.sock.sendto(pkt, self.addr)
        data, _ = self.sock.recvfrom(4096)
        return parse(data)

    def show(self, msg):
        f = msg["flags"]
        flags = [n for bit, n in ((QR, "qr"), (AA, "aa"), (TC, "tc"), (RD, "rd"), (RA, "ra")) if f & bit]
        print(f"status {RCODES[f & 0xF]}, flags {' '.join(flags)}, "
              f"{len(msg['an'])} answers, {len(msg['ns'])} authority")
        for rr in msg["an"]:
            print("  an " + rr.text())
        for rr in msg["ns"]:
            print("  ns " + rr.text())

    def ask(self, name, rtype, rd=True, cls=IN):
        qid = self.next_id
        self.next_id += 1
        print(f"== {name} {tname(rtype)}{'' if rd else ' (no recursion)'}")
        msg = self.exchange(build(qid, RD if rd else 0, (name, rtype, cls)))
        if msg["id"] != qid:
            print("  wrong id")
        self.show(msg)
        return msg

    def control(self, name):
        msg = self.exchange(build(self.next_id, 0, (name, TXT, CH)))
        self.next_id += 1
        print(f"== control {name} -> {msg['an'][0].rdata[0]}")

    def raw(self, label, pkt):
        print(f"== {label}")
        self.show(self.exchange(pkt))


def main():
    zones = make_zones()
    stats = Stats()
    lab = {}
    threads = []
    stoppers = []
    for ip in ["192.0.2.1", "192.0.2.10", "192.0.2.20", "192.0.2.21", "192.0.2.30"]:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.bind(("127.0.0.1", 0))
        tcp = None
        if ip == "192.0.2.20":
            listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            listener.bind(("127.0.0.1", 0))
            listener.listen(4)
            tcp = listener.getsockname()
            threads.append(threading.Thread(target=serve_tcp, args=(listener, zones[ip], stats, ip)))
        lab[ip] = (s.getsockname(), tcp)
        stoppers.append(ip)
        threads.append(threading.Thread(target=serve_udp, args=(s, zones.get(ip), stats, ip)))
    rsock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rsock.bind(("127.0.0.1", 0))
    resolver = Resolver(lab, rsock)
    threads.append(threading.Thread(target=resolver.serve))
    for t in threads:
        t.start()

    stub = Stub(rsock.getsockname())
    stub.ask("www.example.test.", A)
    stub.ask("www.example.test.", A)
    stub.control("advance.100.clock.")
    stub.ask("WWW.Example.TEST.", A)
    stub.ask("example.test.", MX)
    stub.ask("big.example.test.", TXT)
    stub.ask("nope.other.test.", A)
    stub.ask("nope.other.test.", AAAA)
    stub.ask("example.test.", AAAA)
    stub.ask("foo.invalid.", A)
    stub.ask("loop.example.test.", A)
    stub.ask("example.test.", MX, rd=False)
    stub.ask("uncached.example.test.", A, rd=False)
    stub.ask("example.test.", A, cls=CH)
    stub.raw("a question cut short", struct.pack(">HHHHHH", 0x0BAD, RD, 1, 0, 0, 0) + b"\x03ab")
    stub.raw("opcode STATUS", build(0x0BAE, (2 << 11) | RD, ("example.test.", A, IN)))
    stub.control("advance.500.clock.")
    stub.ask("www.example.test.", A)
    stub.ask("nope.other.test.", A)
    stub.control("advance.400.clock.")
    stub.ask("mail.example.test.", A)
    stub.ask("backup.example.test.", A)
    stub.control("shutdown.control.")

    stopper = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    for ip in stoppers:
        stopper.sendto(b"STOP", lab[ip][0])
    stopper.close()
    conn = socket.create_connection(lab["192.0.2.20"][1])
    conn.sendall(b"\0\0")
    conn.close()
    for t in threads:
        t.join()
    stub.sock.close()

    print("== resolver log")
    for line in resolver.log:
        print(line)
    print(f"== resolver: {resolver.clients} client queries, {resolver.upstream} upstream, "
          f"{resolver.timeouts} timeouts")
    for ip in stoppers:
        line = f"{ip} {stats.udp.get(ip, 0)} udp"
        if ip in stats.tcp:
            line += f", {stats.tcp[ip]} tcp"
        print(line)


main()
