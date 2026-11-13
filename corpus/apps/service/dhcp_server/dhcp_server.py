"""Oracle for dhcp_server.kara: a DHCP server (RFC 2131, options of RFC
2132) on loopback UDP. One server thread answers each datagram in turn from
a pool of five addresses: DISCOVER gets an OFFER, REQUEST an ACK or a NAK,
INFORM an ACK without a lease, and RELEASE and DECLINE no answer. Scripted
clients on the main thread, each with a socket of its own, send each kind
and print what comes back; then the server prints its log and leases."""

import socket
import threading

ADDR = ("127.0.0.1", 47769)
DATAGRAMS = 25
QUIET = 0.3
COOKIE = bytes([99, 130, 83, 99])

SERVER_ID = [10, 0, 0, 1]
ROUTER = [10, 0, 0, 1]
DNS = [10, 0, 0, 53]
MASK = [255, 255, 255, 0]
POOL_FIRST = 100
POOL_SIZE = 5
LEASE = 3600

DISCOVER, OFFER, REQUEST, DECLINE, ACK, NAK, RELEASE, INFORM = range(1, 9)
KINDS = ["?", "DISCOVER", "OFFER", "REQUEST", "DECLINE", "ACK", "NAK", "RELEASE", "INFORM"]

OPT_MASK, OPT_ROUTER, OPT_DNS, OPT_HOSTNAME = 1, 3, 6, 12
OPT_REQUESTED, OPT_LEASE, OPT_TYPE, OPT_SERVER = 50, 51, 53, 54
OPT_PARAMS, OPT_MESSAGE, OPT_T1, OPT_T2 = 55, 56, 58, 59


class Reject(Exception):
    pass


def ip_text(b):
    return ".".join(str(x) for x in b)


def mac_text(b):
    return ":".join(f"{x:02x}" for x in b)


def parse(pkt):
    """(fields, options) of a client's message; Reject if it is not one."""
    if len(pkt) < 240:
        raise Reject("short")
    if pkt[0] != 1:
        raise Reject("not a request")
    if pkt[1] != 1 or pkt[2] != 6:
        raise Reject("not ethernet")
    if pkt[236:240] != COOKIE:
        raise Reject("bad cookie")
    opts = {}
    i = 240
    while True:
        if i >= len(pkt):
            raise Reject("no end option")
        code = pkt[i]
        if code == 255:
            break
        if code == 0:
            i += 1
            continue
        if i + 1 >= len(pkt) or i + 2 + pkt[i + 1] > len(pkt):
            raise Reject("option overruns")
        n = pkt[i + 1]
        opts[code] = pkt[i + 2:i + 2 + n]
        i += 2 + n
    kind = opts.get(OPT_TYPE)
    if kind is None or len(kind) != 1 or not 1 <= kind[0] <= 8:
        raise Reject("no message type")
    fields = dict(
        kind=kind[0],
        xid=pkt[4:8],
        ciaddr=list(pkt[12:16]),
        chaddr=pkt[28:34],
    )
    return fields, opts


def option(code, data):
    return bytes([code, len(data)]) + bytes(data)


def message(op, xid, ciaddr, yiaddr, chaddr, opts):
    pkt = bytearray(240)
    pkt[0] = op
    pkt[1] = 1
    pkt[2] = 6
    pkt[4:8] = xid
    pkt[12:16] = bytes(ciaddr)
    pkt[16:20] = bytes(yiaddr)
    pkt[20:24] = bytes(SERVER_ID)
    pkt[28:34] = chaddr
    pkt[236:240] = COOKIE
    for o in opts:
        pkt += o
    pkt += bytes([255])
    return bytes(pkt)


def be32(n):
    return [(n >> 24) & 255, (n >> 16) & 255, (n >> 8) & 255, n & 255]


class Server:
    def __init__(self):
        self.lock = threading.Lock()
        self.log = []
        self.counts = {}
        # address's last octet -> ("offered" | "bound" | "declined", mac)
        self.slots = {}

    def note(self, line, count):
        self.log.append(line)
        self.counts[count] = self.counts.get(count, 0) + 1

    def holder(self, mac, state):
        for k in sorted(self.slots):
            if self.slots[k] == (state, mac):
                return k
        return None

    def free(self, k):
        return POOL_FIRST <= k < POOL_FIRST + POOL_SIZE and k not in self.slots

    def drop_offers(self, mac):
        for k in sorted(self.slots):
            if self.slots[k] == ("offered", mac):
                del self.slots[k]

    def reply(self, kind, f, ciaddr, yiaddr, opts, lease):
        params = opts.get(OPT_PARAMS)
        out = [option(OPT_TYPE, [kind]), option(OPT_SERVER, SERVER_ID)]
        if lease:
            out += [option(OPT_LEASE, be32(LEASE)), option(OPT_T1, be32(LEASE // 2)),
                    option(OPT_T2, be32(LEASE * 7 // 8))]
        if kind != NAK:
            for code, data in [(OPT_MASK, MASK), (OPT_ROUTER, ROUTER), (OPT_DNS, DNS)]:
                if params is None or code in params:
                    out.append(option(code, data))
        else:
            out.append(option(OPT_MESSAGE, b"address not yours"))
        return message(2, f["xid"], ciaddr, yiaddr, f["chaddr"], out)

    def handle(self, pkt):
        """The reply to `pkt`, or None."""
        try:
            f, opts = parse(pkt)
        except Reject as e:
            self.note(f"dropped: {e}", "dropped")
            return None
        mac = bytes(f["chaddr"])
        who = mac_text(mac)
        name = opts.get(OPT_HOSTNAME)
        if name is not None:
            who += f" ({name.decode('utf-8', 'replace')})"
        kind = f["kind"]
        want = opts.get(OPT_REQUESTED)
        want = list(want) if want is not None and len(want) == 4 else None
        if kind == DISCOVER:
            self.drop_offers(mac)
            k = self.holder(mac, "bound")
            if k is None and want is not None and want[:3] == SERVER_ID[:3] and self.free(want[3]):
                k = want[3]
            if k is None:
                k = next((j for j in range(POOL_FIRST, POOL_FIRST + POOL_SIZE) if self.free(j)), None)
            if k is None:
                self.note(f"{who} DISCOVER: pool exhausted", "exhausted")
                return None
            if self.slots.get(k) != ("bound", mac):
                self.slots[k] = ("offered", mac)
            self.note(f"{who} DISCOVER -> OFFER 10.0.0.{k}", "offer")
            return self.reply(OFFER, f, [0, 0, 0, 0], [10, 0, 0, k], opts, True)
        if kind == REQUEST:
            sid = opts.get(OPT_SERVER)
            if sid is not None and list(sid) != SERVER_ID:
                self.drop_offers(mac)
                self.note(f"{who} REQUEST: chose server {ip_text(sid)}", "elsewhere")
                return None
            addr = want if want is not None else f["ciaddr"]
            k = addr[3]
            ours = addr[:3] == SERVER_ID[:3] and POOL_FIRST <= k < POOL_FIRST + POOL_SIZE
            state = self.slots.get(k)
            if sid is not None:
                ok = ours and state == ("offered", mac)
            else:
                ok = ours and state == ("bound", mac)
            if not ok:
                self.drop_offers(mac)
                self.note(f"{who} REQUEST {ip_text(addr)} -> NAK", "nak")
                return self.reply(NAK, f, [0, 0, 0, 0], [0, 0, 0, 0], opts, False)
            self.slots[k] = ("bound", mac)
            self.note(f"{who} REQUEST {ip_text(addr)} -> ACK", "ack")
            return self.reply(ACK, f, [0, 0, 0, 0], [10, 0, 0, k], opts, True)
        if kind == RELEASE:
            k = f["ciaddr"][3]
            if self.slots.get(k) == ("bound", mac):
                del self.slots[k]
                self.note(f"{who} RELEASE {ip_text(f['ciaddr'])}", "release")
            else:
                self.note(f"{who} RELEASE {ip_text(f['ciaddr'])}: not its lease", "ignored")
            return None
        if kind == DECLINE:
            if want is not None and self.slots.get(want[3], (None, None))[1] == mac:
                self.slots[want[3]] = ("declined", b"")
                self.note(f"{who} DECLINE {ip_text(want)}: marked in use", "decline")
            else:
                self.note(f"{who} DECLINE: not its address", "ignored")
            return None
        if kind == INFORM:
            self.note(f"{who} INFORM {ip_text(f['ciaddr'])} -> ACK", "inform")
            return self.reply(ACK, f, f["ciaddr"], [0, 0, 0, 0], opts, False)
        self.note(f"{who} {KINDS[kind]}: not a client message", "dropped")
        return None


def serve(srv, sock):
    for _ in range(DATAGRAMS):
        pkt, peer = sock.recvfrom(1500)
        with srv.lock:
            out = srv.handle(pkt)
        if out is not None:
            sock.sendto(out, peer)


def describe(pkt, xid):
    """One line for a server's reply."""
    try:
        if len(pkt) < 240 or pkt[0] != 2 or pkt[236:240] != COOKIE:
            return "not a reply"
        if pkt[4:8] != xid:
            return "wrong xid"
        opts = {}
        i = 240
        while pkt[i] != 255:
            if pkt[i] == 0:
                i += 1
                continue
            opts[pkt[i]] = pkt[i + 2:i + 2 + pkt[i + 1]]
            i += 2 + pkt[i + 1]
    except IndexError:
        return "truncated reply"
    parts = [KINDS[opts[OPT_TYPE][0]]]
    yi = pkt[16:20]
    if yi != bytes(4):
        parts.append(f"yiaddr={ip_text(yi)}")
    ci = pkt[12:16]
    if ci != bytes(4):
        parts.append(f"ciaddr={ip_text(ci)}")
    names = {OPT_SERVER: "server", OPT_MASK: "mask", OPT_ROUTER: "router", OPT_DNS: "dns"}
    for code in sorted(opts):
        data = opts[code]
        if code in names:
            parts.append(f"{names[code]}={ip_text(data)}")
        elif code in (OPT_LEASE, OPT_T1, OPT_T2):
            n = int.from_bytes(data, "big")
            parts.append(f"{ {OPT_LEASE: 'lease', OPT_T1: 't1', OPT_T2: 't2'}[code]}={n}")
        elif code == OPT_MESSAGE:
            parts.append(f"message=\"{data.decode()}\"")
    return " ".join(parts)


class Client:
    def __init__(self, name, n, hostname):
        self.name = name
        self.mac = bytes([2, 0, 0, 0, 0, n])
        self.xid = bytes([0x4B, 0x41, 0x52, n])
        self.hostname = hostname
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))

    def send(self, kind, ciaddr=(0, 0, 0, 0), extra=()):
        opts = [option(OPT_TYPE, [kind])]
        if self.hostname:
            opts.append(option(OPT_HOSTNAME, self.hostname.encode()))
        opts += list(extra)
        self.sock.sendto(message(1, self.xid, list(ciaddr), [0, 0, 0, 0], self.mac, opts), ADDR)

    def send_raw(self, pkt):
        self.sock.sendto(pkt, ADDR)

    def recv(self, wait):
        self.sock.settimeout(wait)
        try:
            pkt, _ = self.sock.recvfrom(1500)
            print(f"{self.name}: {describe(pkt, self.xid)}")
        except socket.timeout:
            print(f"{self.name}: no reply")


def requested(ip):
    return option(OPT_REQUESTED, ip)


def server_id(ip):
    return option(OPT_SERVER, ip)


def main():
    srv = Server()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(ADDR)
    t = threading.Thread(target=serve, args=(srv, sock))
    t.start()

    alpha = Client("alpha", 1, "alpha")
    alpha.send(DISCOVER)
    alpha.recv(2)
    alpha.send(REQUEST, extra=[requested([10, 0, 0, 100]), server_id(SERVER_ID)])
    alpha.recv(2)

    beta = Client("beta", 2, "beta")
    beta.send(DISCOVER, extra=[requested([10, 0, 0, 103])])
    beta.recv(2)
    beta.send(REQUEST, extra=[requested([10, 0, 0, 103]), server_id([10, 0, 0, 9])])
    beta.recv(QUIET)
    beta.send(DISCOVER, extra=[option(OPT_PARAMS, [OPT_MASK, OPT_DNS])])
    beta.recv(2)
    beta.send(REQUEST, extra=[requested([10, 0, 0, 101]), server_id(SERVER_ID),
                              option(OPT_PARAMS, [OPT_MASK, OPT_DNS])])
    beta.recv(2)

    alpha.send(REQUEST, ciaddr=(10, 0, 0, 100))
    alpha.recv(2)

    gamma = Client("gamma", 3, "")
    gamma.send(REQUEST, extra=[requested([10, 0, 0, 104])])
    gamma.recv(2)
    gamma.send(DISCOVER)
    gamma.recv(2)
    gamma.send(REQUEST, extra=[requested([10, 0, 0, 104]), server_id(SERVER_ID)])
    gamma.recv(2)
    gamma.send(DISCOVER)
    gamma.recv(2)
    gamma.send(REQUEST, extra=[requested([10, 0, 0, 102]), server_id(SERVER_ID)])
    gamma.recv(2)
    gamma.send(DECLINE, extra=[requested([10, 0, 0, 102]), server_id(SERVER_ID)])
    gamma.recv(QUIET)

    alpha.send(RELEASE, ciaddr=(10, 0, 0, 100), extra=[server_id(SERVER_ID)])
    alpha.recv(QUIET)
    beta.send(RELEASE, ciaddr=(10, 0, 0, 100), extra=[server_id(SERVER_ID)])
    beta.recv(QUIET)

    delta = Client("delta", 4, "delta")
    delta.send(DISCOVER)
    delta.recv(2)

    for i, name in enumerate(["epsilon", "zeta", "eta"]):
        c = Client(name, 7 + i, name)
        c.send(DISCOVER)
        c.recv(2 if name != "eta" else QUIET)

    office = Client("office", 5, "printer")
    office.send(INFORM, ciaddr=(10, 0, 0, 50), extra=[option(OPT_PARAMS, [OPT_ROUTER])])
    office.recv(2)

    bad = Client("broken", 6, "")
    bad.send_raw(bytes(10))
    bad.recv(QUIET)
    pkt = bytearray(message(1, bad.xid, [0] * 4, [0] * 4, bad.mac, [option(OPT_TYPE, [DISCOVER])]))
    pkt[236] = 0
    bad.send_raw(bytes(pkt))
    bad.recv(QUIET)
    bad.send_raw(message(2, bad.xid, [0] * 4, [0] * 4, bad.mac, [option(OPT_TYPE, [OFFER])]))
    bad.recv(QUIET)
    bad.send_raw(message(1, bad.xid, [0] * 4, [0] * 4, bad.mac, [option(OPT_HOSTNAME, b"x")]))
    bad.recv(QUIET)
    pkt = message(1, bad.xid, [0] * 4, [0] * 4, bad.mac, [option(OPT_TYPE, [DISCOVER])])
    bad.send_raw(pkt[:-1] + bytes([OPT_HOSTNAME, 9, 65]))
    bad.recv(QUIET)

    t.join()
    sock.close()
    print("== server")
    for i, line in enumerate(srv.log):
        print(f"#{i + 1} {line}")
    for k in sorted(srv.counts):
        print(f"{k}: {srv.counts[k]}")
    print("== leases")
    for k in sorted(srv.slots):
        state, mac = srv.slots[k]
        print(f"10.0.0.{k} {state} {mac_text(mac) if mac else '-'}")


main()
