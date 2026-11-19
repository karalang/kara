"""Oracle for snmp_agent.kara: an SNMP agent (SNMPv1, RFC 1157, and
SNMPv2c, RFCs 1901 and 3416) on loopback UDP, and the managers that query
it. Every message is BER-encoded ASN.1 (X.690): a type tag, a length in
short or long form, and the contents."""

import socket
import threading

QUIET_S = 0.15
MAX_SIZE = 484  # the smallest message size every SNMP entity must accept

# Tags
INTEGER, OCTETS, NULL, OID, SEQ = 0x02, 0x04, 0x05, 0x06, 0x30
IPADDR, COUNTER32, GAUGE32, TICKS, COUNTER64 = 0x40, 0x41, 0x42, 0x43, 0x46
NO_OBJECT, NO_INSTANCE, END_OF_VIEW = 0x80, 0x81, 0x82
GET, GETNEXT, RESPONSE, SET, GETBULK = 0xA0, 0xA1, 0xA2, 0xA3, 0xA5
PDU_NAMES = {GET: "get", GETNEXT: "getnext", RESPONSE: "response", SET: "set",
             GETBULK: "getbulk"}
ERRORS = ["noError", "tooBig", "noSuchName", "badValue", "readOnly", "genErr",
          "noAccess", "wrongType", "wrongLength", "wrongEncoding", "wrongValue",
          "noCreation", "inconsistentValue", "resourceUnavailable", "commitFailed",
          "undoFailed", "authorizationError", "notWritable", "inconsistentName"]


class BerError(Exception):
    pass


# ---- encoding


def enc_len(n):
    if n < 0x80:
        return bytes([n])
    out = []
    while n:
        out.append(n & 0xFF)
        n >>= 8
    return bytes([0x80 | len(out)]) + bytes(reversed(out))


def tlv(tag, body):
    return bytes([tag]) + enc_len(len(body)) + body


def enc_int(tag, v):
    """Two's complement, as few bytes as keep the sign."""
    out = [v & 0xFF]
    v >>= 8
    while not ((v == 0 and out[-1] < 0x80) or (v == -1 and out[-1] >= 0x80)):
        out.append(v & 0xFF)
        v >>= 8
    return tlv(tag, bytes(reversed(out)))


def enc_oid(arcs):
    body = bytearray()
    subs = [arcs[0] * 40 + arcs[1]] + list(arcs[2:])
    for s in subs:
        chunk = [s & 0x7F]
        s >>= 7
        while s:
            chunk.append(0x80 | (s & 0x7F))
            s >>= 7
        body.extend(reversed(chunk))
    return tlv(OID, bytes(body))


def enc_value(v):
    tag, x = v
    if tag in (INTEGER, COUNTER32, GAUGE32, TICKS, COUNTER64):
        return enc_int(tag, x)
    if tag == OCTETS:
        return tlv(OCTETS, x)
    if tag == OID:
        return enc_oid(x)
    if tag == IPADDR:
        return tlv(IPADDR, bytes(x))
    return tlv(tag, b"")  # NULL and the three exceptions


def enc_message(version, community, pdu, req_id, a, b, varbinds):
    vbs = b"".join(tlv(SEQ, enc_oid(o) + enc_value(v)) for o, v in varbinds)
    body = enc_int(INTEGER, req_id) + enc_int(INTEGER, a) + enc_int(INTEGER, b) + tlv(SEQ, vbs)
    return tlv(SEQ, enc_int(INTEGER, version) + tlv(OCTETS, community) + tlv(pdu, body))


# ---- decoding


class Reader:
    def __init__(self, buf, start=0, end=None):
        self.buf = buf
        self.at = start
        self.end = len(buf) if end is None else end

    def done(self):
        return self.at >= self.end

    def take(self):
        """One TLV: (tag, start, end) of its contents."""
        if self.at + 2 > self.end:
            raise BerError("truncated header")
        tag = self.buf[self.at]
        n = self.buf[self.at + 1]
        self.at += 2
        if n & 0x80:
            k = n & 0x7F
            if k == 0 or k > 2:
                raise BerError(f"length of {k} bytes")
            if self.at + k > self.end:
                raise BerError("truncated length")
            n = 0
            for i in range(k):
                n = (n << 8) | self.buf[self.at + i]
            self.at += k
        if self.at + n > self.end:
            raise BerError(f"length {n} past the end")
        start = self.at
        self.at += n
        return tag, start, self.at

    def expect(self, tag):
        t, s, e = self.take()
        if t != tag:
            raise BerError(f"tag {t:#04x} where {tag:#04x} was expected")
        return s, e

    def integer(self, tag=INTEGER):
        s, e = self.expect(tag)
        return dec_int(self.buf, s, e, tag != INTEGER)


def dec_int(buf, s, e, unsigned):
    if e == s or e - s > (9 if unsigned else 8):
        raise BerError(f"integer of {e - s} bytes")
    v = 0
    for i in range(s, e):
        v = (v << 8) | buf[i]
    if not unsigned and buf[s] & 0x80:
        v -= 1 << (8 * (e - s))
    return v


def dec_oid(buf, s, e):
    if e == s:
        raise BerError("empty OID")
    subs, v = [], 0
    for i in range(s, e):
        v = (v << 7) | (buf[i] & 0x7F)
        if not buf[i] & 0x80:
            subs.append(v)
            v = 0
    if buf[e - 1] & 0x80:
        raise BerError("OID ends mid sub-identifier")
    first = subs[0]
    head = [min(first // 40, 2), first - 40 * min(first // 40, 2)]
    return tuple(head + subs[1:])


def dec_value(r):
    tag, s, e = r.take()
    if tag in (INTEGER, COUNTER32, GAUGE32, TICKS, COUNTER64):
        return tag, dec_int(r.buf, s, e, tag != INTEGER)
    if tag == OCTETS:
        return tag, bytes(r.buf[s:e])
    if tag == OID:
        return tag, dec_oid(r.buf, s, e)
    if tag == IPADDR:
        if e - s != 4:
            raise BerError("IpAddress of the wrong size")
        return tag, tuple(r.buf[s:e])
    if tag in (NULL, NO_OBJECT, NO_INSTANCE, END_OF_VIEW):
        return tag, None
    raise BerError(f"value tag {tag:#04x}")


def dec_message(buf):
    top = Reader(buf)
    s, e = top.expect(SEQ)
    if not top.done():
        raise BerError("bytes after the message")
    r = Reader(buf, s, e)
    version = r.integer()
    cs, ce = r.expect(OCTETS)
    community = bytes(buf[cs:ce])
    pdu, ps, pe = r.take()
    if pdu not in PDU_NAMES:
        raise BerError(f"PDU tag {pdu:#04x}")
    p = Reader(buf, ps, pe)
    req_id = p.integer()
    a = p.integer()
    b = p.integer()
    vs, ve = p.expect(SEQ)
    v = Reader(buf, vs, ve)
    varbinds = []
    while not v.done():
        bs, be_ = v.expect(SEQ)
        one = Reader(buf, bs, be_)
        os_, oe = one.expect(OID)
        varbinds.append((dec_oid(buf, os_, oe), dec_value(one)))
    return version, community, pdu, req_id, a, b, varbinds


# ---- the agent

SYS = (1, 3, 6, 1, 2, 1, 1)
IFX = (1, 3, 6, 1, 2, 1, 2, 2, 1)
SNMP = (1, 3, 6, 1, 2, 1, 11)
HC = (1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 6)
IPAD = (1, 3, 6, 1, 2, 1, 4, 20, 1, 1)
HALT = (1, 3, 6, 1, 4, 1, 99999, 2, 0)


class Agent:
    def __init__(self, sock):
        self.sock = sock
        self.peers = {}
        self.stats = {"inPkts": 0, "badVersions": 0, "badCommunity": 0, "parseErrs": 0}
        self.log = []
        self.counts = {}
        s = lambda t: (OCTETS, t.encode())
        self.mib = {
            SYS + (1, 0): s("Kara SNMP agent 0.1 on loopback"),
            SYS + (2, 0): (OID, (1, 3, 6, 1, 4, 1, 99999, 1)),
            SYS + (4, 0): s("ops@example.invalid"),
            SYS + (5, 0): s("agent-7"),
            SYS + (6, 0): s("rack 4, shelf 2"),
            SYS + (7, 0): (INTEGER, 72),
            (1, 3, 6, 1, 2, 1, 2, 1, 0): (INTEGER, 2),
            IPAD + (10, 0, 0, 7): (IPADDR, (10, 0, 0, 7)),
            IPAD + (127, 0, 0, 1): (IPADDR, (127, 0, 0, 1)),
            HC + (1,): (COUNTER64, (1 << 40) + 7),
            HC + (2,): (COUNTER64, (1 << 64) - 1),
            HALT: (INTEGER, 0),
        }
        for i, (name, speed, rx, tx) in enumerate(
                [("lo", 10_000_000, 123_456, 654_321), ("eth0", 4294967295, 4294967295, 0)], 1):
            self.mib[IFX + (1, i)] = (INTEGER, i)
            self.mib[IFX + (2, i)] = s(name)
            self.mib[IFX + (5, i)] = (GAUGE32, speed)
            self.mib[IFX + (10, i)] = (COUNTER32, rx)
            self.mib[IFX + (16, i)] = (COUNTER32, tx)
        self.writable = {SYS + (4, 0): OCTETS, SYS + (5, 0): OCTETS, SYS + (6, 0): OCTETS,
                         HALT: INTEGER}
        self.order = sorted(set(self.mib) | {SYS + (3, 0)} | self.dynamic_oids())

    def dynamic_oids(self):
        return {SNMP + (1, 0), SNMP + (3, 0), SNMP + (4, 0), SNMP + (6, 0)}

    def value(self, oid):
        if oid == SYS + (3, 0):
            return TICKS, 4200 + 17 * self.stats["inPkts"]
        dyn = {SNMP + (1, 0): "inPkts", SNMP + (3, 0): "badVersions",
               SNMP + (4, 0): "badCommunity", SNMP + (6, 0): "parseErrs"}
        if oid in dyn:
            return COUNTER32, self.stats[dyn[oid]]
        return self.mib.get(oid)

    def after(self, oid):
        for o in self.order:
            if o > oid:
                return o
        return None

    def missing(self, oid):
        """noSuchInstance when an object of the MIB shares this OID's parent."""
        if any(o[:-1] == oid[:-1] for o in self.order):
            return NO_INSTANCE, None
        return NO_OBJECT, None

    def count(self, k):
        self.counts[k] = self.counts.get(k, 0) + 1

    def serve(self):
        while True:
            pkt, addr = self.sock.recvfrom(2048)
            peer = self.peers.get(addr, "?")
            self.stats["inPkts"] += 1
            try:
                version, community, pdu, req_id, a, b, vbs = dec_message(pkt)
            except BerError as e:
                self.stats["parseErrs"] += 1
                self.log.append(f"{peer}: dropped, {e}")
                self.count("dropped")
                continue
            if version not in (0, 1):
                self.stats["badVersions"] += 1
                self.log.append(f"{peer}: dropped, version {version}")
                self.count("dropped")
                continue
            v2 = version == 1
            vname = "v2c" if v2 else "v1"
            if community not in (b"public", b"private") or (pdu == SET and community != b"private"):
                self.stats["badCommunity"] += 1
                self.log.append(f"{peer}: dropped, community {community.decode()!r} "
                                f"for {PDU_NAMES[pdu]}")
                self.count("dropped")
                continue
            if pdu == RESPONSE or (pdu == GETBULK and not v2):
                self.log.append(f"{peer}: dropped, {vname} {PDU_NAMES[pdu]}")
                self.count("dropped")
                continue
            status, index, out = self.answer(v2, pdu, a, b, vbs)
            reply = enc_message(version, community, RESPONSE, req_id, status, index, out)
            if len(reply) > MAX_SIZE:
                if pdu == GETBULK:
                    while len(reply) > MAX_SIZE:
                        out = out[:-1]
                        reply = enc_message(version, community, RESPONSE, req_id, 0, 0, out)
                    status = -1
                else:
                    status, index = 1, 0
                    reply = enc_message(version, community, RESPONSE, req_id, 1, 0, vbs)
            self.sock.sendto(reply, addr)
            what = (f"truncated to {len(out)}" if status == -1
                    else ERRORS[status] + (f" at {index}" if index else ""))
            self.log.append(f"{peer}: {vname} {PDU_NAMES[pdu]} of {len(vbs)} -> {what}")
            self.count(PDU_NAMES[pdu])
            if pdu == SET and status == 0 and any(o == HALT for o, _ in vbs):
                self.log.append(f"{peer}: halt")
                return

    def answer(self, v2, pdu, a, b, vbs):
        if pdu == GET:
            out = []
            for i, (oid, _) in enumerate(vbs, 1):
                v = self.value(oid)
                if v is None:
                    if not v2:
                        return 2, i, vbs
                    v = self.missing(oid)
                out.append((oid, v))
            return 0, 0, out
        if pdu == GETNEXT:
            out = []
            for i, (oid, _) in enumerate(vbs, 1):
                nxt = self.after(oid)
                if nxt is None:
                    if not v2:
                        return 2, i, vbs
                    out.append((oid, (END_OF_VIEW, None)))
                else:
                    out.append((nxt, self.value(nxt)))
            return 0, 0, out
        if pdu == GETBULK:
            non_rep = max(a, 0)
            reps = max(b, 0)
            out = []
            for oid, _ in vbs[:non_rep]:
                nxt = self.after(oid)
                out.append((nxt, self.value(nxt)) if nxt else (oid, (END_OF_VIEW, None)))
            cursors = [oid for oid, _ in vbs[non_rep:]]
            for _ in range(reps):
                if not cursors:
                    break
                ended = True
                for k, oid in enumerate(cursors):
                    nxt = self.after(oid)
                    if nxt is None:
                        out.append((oid, (END_OF_VIEW, None)))
                    else:
                        out.append((nxt, self.value(nxt)))
                        cursors[k] = nxt
                        ended = False
                if ended:
                    break
            return 0, 0, out
        # SET: check every binding before writing any
        for i, (oid, v) in enumerate(vbs, 1):
            kind = self.writable.get(oid)
            if kind is None:
                if not v2:
                    return 2, i, vbs
                return (17 if self.value(oid) is not None else 11), i, vbs
            if v[0] != kind:
                return (7 if v2 else 3), i, vbs
            if kind == OCTETS and len(v[1]) > 255:
                return (8 if v2 else 3), i, vbs
            if kind == INTEGER and v[1] not in (0, 1):
                return (10 if v2 else 3), i, vbs
        for oid, v in vbs:
            self.mib[oid] = v
        return 0, 0, vbs


# ---- the managers


def show_oid(o):
    return ".".join(str(x) for x in o)


def show_value(v):
    tag, x = v
    if tag == INTEGER:
        return f"INTEGER: {x}"
    if tag == OCTETS:
        if len(x) > 32:
            return f'STRING: "{x[:32].decode()}..." ({len(x)} bytes)'
        return f'STRING: "{x.decode()}"'
    if tag == OID:
        return f"OID: {show_oid(x)}"
    if tag == IPADDR:
        return "IpAddress: " + ".".join(str(b) for b in x)
    if tag == COUNTER32:
        return f"Counter32: {x}"
    if tag == GAUGE32:
        return f"Gauge32: {x}"
    if tag == COUNTER64:
        return f"Counter64: {x}"
    if tag == TICKS:
        cs = x % 100
        s = x // 100
        return f"Timeticks: ({x}) {s // 86400} days, {s // 3600 % 24}:{s // 60 % 60:02d}:{s % 60:02d}.{cs:02d}"
    if tag == NULL:
        return "NULL"
    if tag == NO_OBJECT:
        return "No Such Object available on this agent at this OID"
    if tag == NO_INSTANCE:
        return "No Such Instance currently exists at this OID"
    return "No more variables left in this MIB View (It is past the end of the MIB tree)"


def parse_oid(text):
    return tuple(int(p) for p in text.split("."))


class Manager:
    def __init__(self, name, agent_addr, agent, version=1, community=b"public"):
        self.name = name
        self.agent = agent_addr
        self.version = version
        self.community = community
        self.req_id = 1000
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(QUIET_S)
        agent.peers[self.sock.getsockname()] = name

    def say(self, line):
        print(f"{self.name}: {line}")

    def request(self, pdu, varbinds, a=0, b=0, req_id=None):
        """Send one PDU; the decoded response, or None after the quiet time."""
        if req_id is None:
            self.req_id += 1
            req_id = self.req_id
        msg = enc_message(self.version, self.community, pdu, req_id, a, b, varbinds)
        self.sock.sendto(msg, self.agent)
        try:
            reply, _ = self.sock.recvfrom(2048)
        except socket.timeout:
            return None
        return reply

    def report(self, what, reply):
        if reply is None:
            self.say(f"{what} -> no reply")
            return None
        version, community, pdu, req_id, status, index, vbs = dec_message(reply)
        head = f"{what} -> {PDU_NAMES[pdu]} id {req_id}, {len(reply)} bytes"
        if status:
            self.say(f"{head}, error {ERRORS[status]}({status}) at {index}")
        else:
            self.say(head)
        for oid, v in vbs:
            self.say(f"  {show_oid(oid)} = {show_value(v)}")
        return vbs

    def get(self, *oids, what=None):
        vbs = [(parse_oid(o), (NULL, None)) for o in oids]
        return self.report(what or f"get {len(oids)}", self.request(GET, vbs))

    def getnext(self, *oids, what=None):
        vbs = [(parse_oid(o), (NULL, None)) for o in oids]
        return self.report(what or f"getnext {len(oids)}", self.request(GETNEXT, vbs))

    def getbulk(self, non_rep, reps, *oids, what=None):
        vbs = [(parse_oid(o), (NULL, None)) for o in oids]
        return self.report(what or f"getbulk {non_rep}/{reps}",
                           self.request(GETBULK, vbs, non_rep, reps))

    def set(self, what, *pairs):
        vbs = [(parse_oid(o), v) for o, v in pairs]
        return self.report(what, self.request(SET, vbs))

    def walk(self, root):
        """snmpwalk: getnext until the answer leaves the subtree."""
        base = parse_oid(root)
        oid = base
        n = 0
        while True:
            reply = self.request(GETNEXT, [(oid, (NULL, None))])
            if reply is None:
                self.say("walk: no reply")
                return
            _, _, _, _, status, _, vbs = dec_message(reply)
            nxt, v = vbs[0]
            if status or v[0] == END_OF_VIEW or nxt[:len(base)] != base:
                break
            self.say(f"  {show_oid(nxt)} = {show_value(v)}")
            n += 1
            oid = nxt
        self.say(f"walk {root}: {n} objects")

    def raw(self, what, data):
        self.sock.sendto(data, self.agent)
        try:
            reply, _ = self.sock.recvfrom(2048)
            self.say(f"{what} -> {len(reply)}-byte reply")
        except socket.timeout:
            self.say(f"{what} -> no reply")


def main():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    agent = Agent(sock)
    addr = sock.getsockname()
    nms = Manager("nms", addr, agent, version=1)
    old = Manager("old", addr, agent, version=0)
    ops = Manager("ops", addr, agent, version=1, community=b"private")
    v1ops = Manager("v1ops", addr, agent, version=0, community=b"private")
    probe = Manager("probe", addr, agent, version=1)
    t = threading.Thread(target=agent.serve)
    t.start()

    print("== get")
    nms.get("1.3.6.1.2.1.1.1.0", "1.3.6.1.2.1.1.3.0", "1.3.6.1.2.1.1.2.0")
    nms.get("1.3.6.1.2.1.2.2.1.5.2", "1.3.6.1.2.1.2.2.1.10.2", "1.3.6.1.2.1.2.2.1.16.2",
            "1.3.6.1.2.1.31.1.1.1.6.1", "1.3.6.1.2.1.31.1.1.1.6.2")
    nms.get("1.3.6.1.2.1.1.9.0", "1.3.6.1.2.1.1.5.1", "1.3.6.1.2.1.4.20.1.1.127.0.0.1",
            what="get missing (v2c)")
    old.get("1.3.6.1.2.1.1.5.0", "1.3.6.1.2.1.1.9.0", what="get missing (v1)")

    print("== walk")
    nms.walk("1.3.6.1.2.1.1")
    nms.walk("1.3.6.1.2.1.4.20")
    nms.getnext("1.3.6.1.4.1.99999.2.0", what="getnext past the end (v2c)")
    old.getnext("1.3.6.1.4.1.99999.2.0", what="getnext past the end (v1)")

    print("== bulk")
    nms.getbulk(1, 3, "1.3.6.1.2.1.1.7", "1.3.6.1.2.1.2.2.1.1", "1.3.6.1.2.1.2.2.1.2")
    nms.getbulk(0, 4, "1.3.6.1.4.1.99999.1", what="getbulk into the end")
    nms.getbulk(0, 60, "1.3.6.1.2.1.1", what="getbulk 60, truncated")
    many = ["1.3.6.1.2.1.1.1.0"] * 20
    nms.get(*many, what="get 20 descriptions, too big")
    old.getbulk(0, 2, "1.3.6.1.2.1.1", what="getbulk on v1")

    print("== set")
    ops.set("set sysName and sysLocation", ("1.3.6.1.2.1.1.5.0", (OCTETS, b"agent-7b")),
            ("1.3.6.1.2.1.1.6.0", (OCTETS, "rack 5, shelf 1".encode())))
    ops.set("set sysName to an integer", ("1.3.6.1.2.1.1.5.0", (INTEGER, 7)))
    v1ops.set("set sysName to an integer (v1)", ("1.3.6.1.2.1.1.5.0", (INTEGER, 7)))
    ops.set("set sysDescr", ("1.3.6.1.2.1.1.1.0", (OCTETS, b"x")))
    ops.set("set a new object", ("1.3.6.1.2.1.1.8.0", (INTEGER, 1)))
    ops.set("set the halt switch to 9", ("1.3.6.1.4.1.99999.2.0", (INTEGER, 9)))
    ops.set("set sysContact, 300 bytes", ("1.3.6.1.2.1.1.4.0", (OCTETS, b"x" * 300)))
    nms.set("set with the public community", ("1.3.6.1.2.1.1.5.0", (OCTETS, b"evil")))
    nms.get("1.3.6.1.2.1.1.4.0", "1.3.6.1.2.1.1.5.0", "1.3.6.1.2.1.1.6.0", what="get after set")

    print("== broken")
    probe.community = b"secret"
    probe.get("1.3.6.1.2.1.1.5.0", what="wrong community")
    probe.community = b"public"
    probe.version = 3
    probe.get("1.3.6.1.2.1.1.5.0", what="version 3")
    probe.version = 1
    good = enc_message(1, b"public", GET, 77, 0, 0, [((1, 3, 6, 1, 2, 1, 1, 5, 0), (NULL, None))])
    probe.raw("truncated by 3 bytes", good[:-3])
    probe.raw("trailing byte", good + b"\x00")
    probe.raw("3-byte long length", bytes([0x30, 0x83, 0, 0, 5]) + bytes(5))
    probe.raw("not a sequence", b"\x04\x03abc")
    bad_oid = bytearray(good)
    bad_oid[-3] |= 0x80  # last sub-identifier's continuation bit
    probe.raw("OID ends mid sub-identifier", bytes(bad_oid))
    resp = enc_message(1, b"public", RESPONSE, 78, 0, 0, [])
    probe.raw("a response PDU", resp)
    probe.report("request id -5", probe.request(GET, [((1, 3, 6, 1, 2, 1, 1, 7, 0), (NULL, None))],
                                                req_id=-5))
    probe.report("request id 2147483647",
                 probe.request(GET, [((1, 3, 6, 1, 2, 1, 1, 7, 0), (NULL, None))],
                               req_id=2147483647))
    nms.get("1.3.6.1.2.1.11.1.0", "1.3.6.1.2.1.11.3.0", "1.3.6.1.2.1.11.4.0",
            "1.3.6.1.2.1.11.6.0", "1.3.6.1.2.1.1.3.0", what="snmp counters")

    print("== admin")
    ops.set("halt", ("1.3.6.1.4.1.99999.2.0", (INTEGER, 1)))
    t.join()

    print("== agent log")
    for line in agent.log:
        print(line)
    print("== counts")
    for k in sorted(agent.counts):
        print(f"{k}: {agent.counts[k]}")


main()
