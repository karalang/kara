"""Oracle for ntp_server.kara: an NTP server (RFC 5905, the SNTP subset of
RFC 4330) on loopback UDP, and the clients that set their clocks from it.
Every clock is a fake that moves a fixed step on each read, so the
timestamps, offsets and delays are the same on every run: the server's
clock is the reference, and each client's runs ahead of or behind it."""

import socket
import threading

NTP_UNIX = 2208988800  # seconds from 1900 to 1970
BASE_NS = 1763337600 * 1_000_000_000  # 2025-11-17 00:00:00 UTC
SERVER_STEP = 437_500
CLIENT_STEP = 3_250_000
RATE_NS = 1_500_000
QUIET_S = 0.15


class FakeClock:
    """Wall time that starts at `start` and moves `step` ns on each read."""

    def __init__(self, start, step):
        self.t = start
        self.step = step

    def wall_ns(self):
        v = self.t
        self.t += self.step
        return v


def to_ntp(ns):
    secs, sub = divmod(ns, 1_000_000_000)
    frac = (sub << 32) // 1_000_000_000
    return ((secs + NTP_UNIX) << 32) | frac


def from_ntp(ts):
    secs = (ts >> 32) - NTP_UNIX
    frac = ts & 0xFFFFFFFF
    return secs * 1_000_000_000 + ((frac * 1_000_000_000 + (1 << 31)) >> 32)


def be32(v):
    return (v & 0xFFFFFFFF).to_bytes(4, "big")


def be64(v):
    return (v & 0xFFFFFFFFFFFFFFFF).to_bytes(8, "big")


def rd32(b, i):
    return int.from_bytes(b[i:i + 4], "big")


def rd64(b, i):
    return int.from_bytes(b[i:i + 8], "big")


def civil(ns):
    """UTC date and time of `ns` since the Unix epoch, to the nanosecond."""
    secs, sub = divmod(ns, 1_000_000_000)
    days, rem = divmod(secs, 86400)
    z = days + 719468
    era = z // 146097
    doe = z - era * 146097
    yoe = (doe - doe // 1460 + doe // 36524 - doe // 146096) // 365
    y = yoe + era * 400
    doy = doe - (365 * yoe + yoe // 4 - yoe // 100)
    mp = (5 * doy + 2) // 153
    d = doy - (153 * mp + 2) // 5 + 1
    m = mp + 3 if mp < 10 else mp - 9
    if m <= 2:
        y += 1
    hh, rem = divmod(rem, 3600)
    mm, ss = divmod(rem, 60)
    return f"{y:04d}-{m:02d}-{d:02d} {hh:02d}:{mm:02d}:{ss:02d}.{sub:09d}"


def fmt_frac(whole, frac, digits):
    s = str(whole)
    f = str(frac).rjust(digits, "0").rstrip("0")
    return s + ("." + f if f else "")


def duration(ns):
    """Go's time.Duration.String(), which Kāra's Duration Display follows."""
    if ns == 0:
        return "0s"
    neg = ns < 0
    u = -ns if neg else ns
    if u < 1_000_000_000:
        if u < 1_000:
            out = f"{u}ns"
        elif u < 1_000_000:
            out = fmt_frac(u // 1_000, u % 1_000, 3) + "µs"
        else:
            out = fmt_frac(u // 1_000_000, u % 1_000_000, 6) + "ms"
    else:
        secs = fmt_frac((u // 1_000_000_000) % 60, u % 1_000_000_000, 9) + "s"
        mins = u // 60_000_000_000
        if mins == 0:
            out = secs
        else:
            hours = mins // 60
            out = (f"{hours}h" if hours else "") + f"{mins % 60}m" + secs
    return ("-" + out) if neg else out


class Server:
    def __init__(self, sock):
        self.sock = sock
        self.clock = FakeClock(BASE_NS, SERVER_STEP)
        self.reference = to_ntp(self.clock.wall_ns())
        self.last = {}
        self.log = []
        self.counts = {}

    def count(self, k):
        self.counts[k] = self.counts.get(k, 0) + 1

    def drop(self, peer, why):
        self.log.append(f"{peer}: dropped, {why}")
        self.count("dropped")

    def serve(self):
        while True:
            pkt, addr = self.sock.recvfrom(1024)
            peer = self.peers.get(addr, "?")
            n = len(pkt)
            if n < 48:
                self.drop(peer, f"short packet ({n} bytes)")
                continue
            vn = (pkt[0] >> 3) & 7
            mode = pkt[0] & 7
            if mode == 7 and pkt[4:8] == b"HALT":
                self.log.append(f"{peer}: halt")
                return
            if vn < 1 or vn > 4:
                self.drop(peer, f"unsupported version {vn}")
                continue
            if mode != 3:
                self.drop(peer, f"mode {mode} is not a client request")
                continue
            xmt = rd64(pkt, 40)
            if xmt == 0:
                self.drop(peer, "zero transmit timestamp")
                continue
            extra = n - 48
            if extra > 0 and (extra < 16 or extra % 4 != 0):
                self.drop(peer, f"bad extension ({extra} bytes)")
                continue
            t2 = self.clock.wall_ns()
            prev = self.last.get(addr)
            self.last[addr] = t2
            out = bytearray(48)
            out[2] = pkt[2]
            out[24:32] = be64(xmt)
            out[32:40] = be64(to_ntp(t2))
            if prev is not None and t2 - prev < RATE_NS:
                out[0] = (3 << 6) | (vn << 3) | 4
                out[1] = 0
                out[12:16] = b"RATE"
                out[40:48] = be64(to_ntp(t2))
                self.sock.sendto(bytes(out), addr)
                self.log.append(f"{peer}: v{vn} kiss-o'-death RATE")
                self.count("kod")
                continue
            out[0] = (0 << 6) | (vn << 3) | 4
            out[1] = 1
            out[3] = 0xEC  # precision -20
            out[4:8] = be32(0)
            out[8:12] = be32(16)
            out[12:16] = b"GPS\x00"
            out[16:24] = be64(self.reference)
            t3 = self.clock.wall_ns()
            out[40:48] = be64(to_ntp(t3))
            self.sock.sendto(bytes(out), addr)
            note = f", ignored {extra} extension bytes" if extra else ""
            self.log.append(f"{peer}: v{vn} answered{note}")
            self.count("answered")


class Client:
    def __init__(self, name, server, skew_ns, server_obj):
        self.name = name
        self.server = server
        self.clock = FakeClock(BASE_NS + skew_ns, CLIENT_STEP)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(QUIET_S)
        server_obj.peers[self.sock.getsockname()] = name

    def request(self, vn, mode, xmt):
        b = bytearray(48)
        b[0] = (vn << 3) | mode
        b[2] = 6
        b[40:48] = be64(xmt)
        return b

    def show(self, what, reply, t1, t4):
        if reply is None:
            print(f"{self.name}: {what} -> no reply")
            return
        li = reply[0] >> 6
        vn = (reply[0] >> 3) & 7
        mode = reply[0] & 7
        stratum = reply[1]
        if mode != 4:
            print(f"{self.name}: {what} -> mode {mode}, not a server reply")
            return
        if rd64(reply, 24) != t1:
            print(f"{self.name}: {what} -> origin does not match")
            return
        if stratum == 0:
            code = reply[12:16].decode("ascii")
            print(f"{self.name}: {what} -> v{vn} li {li} kiss-o'-death {code}")
            return
        refid = reply[12:16].rstrip(b"\x00").decode("ascii")
        prec = reply[3] - 256 if reply[3] >= 128 else reply[3]
        disp_ns = (rd32(reply, 8) * 1_000_000_000) >> 16
        ref = from_ntp(rd64(reply, 16))
        t2 = from_ntp(rd64(reply, 32))
        t3 = from_ntp(rd64(reply, 40))
        t1n = from_ntp(t1)
        t4n = from_ntp(t4)
        both = (t2 - t1n) + (t3 - t4n)
        offset = -((-both) // 2) if both < 0 else both // 2  # toward zero
        delay = (t4n - t1n) - (t3 - t2)
        print(f"{self.name}: {what} -> v{vn} li {li} stratum {stratum} refid {refid} "
              f"poll {reply[2]} precision {prec} dispersion {duration(disp_ns)}")
        print(f"{self.name}:   reference {civil(ref)}")
        print(f"{self.name}:   received  {civil(t2)}  sent {civil(t3)}")
        print(f"{self.name}:   offset {duration(offset)}  delay {duration(delay)}  "
              f"corrected {civil(t4n + offset)}")

    def exchange(self, what, vn=4, mode=3, extra=b""):
        t1 = to_ntp(self.clock.wall_ns())
        pkt = bytes(self.request(vn, mode, t1)) + extra
        self.sock.sendto(pkt, self.server)
        try:
            reply, _ = self.sock.recvfrom(1024)
        except socket.timeout:
            reply = None
        t4 = to_ntp(self.clock.wall_ns())
        self.show(what, reply, t1, t4)

    def send_raw(self, what, pkt):
        self.sock.sendto(pkt, self.server)
        try:
            reply, _ = self.sock.recvfrom(1024)
            print(f"{self.name}: {what} -> {len(reply)}-byte reply")
        except socket.timeout:
            print(f"{self.name}: {what} -> no reply")


def main():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    server = Server(sock)
    server.peers = {}
    addr = sock.getsockname()
    laptop = Client("laptop", addr, 2_500_000_000, server)
    meter = Client("meter", addr, -1_250_000_000, server)
    old = Client("old", addr, 61_000_000_000, server)
    greedy = Client("greedy", addr, 7_000_000, server)
    broken = Client("broken", addr, 0, server)
    admin = Client("admin", addr, 0, server)
    t = threading.Thread(target=server.serve)
    t.start()

    print("== sync")
    laptop.exchange("v4 request")
    meter.exchange("v3 request", vn=3)
    laptop.exchange("again")
    old.exchange("v1 request", vn=1)

    print("== greedy")
    for i in range(3):
        greedy.exchange(f"burst {i + 1}")
    old.exchange("with a 16-byte extension", extra=bytes(range(16)))

    print("== broken")
    base = broken.request(4, 3, to_ntp(BASE_NS))
    broken.send_raw("47 bytes", bytes(base[:47]))
    broken.send_raw("version 5", bytes([(5 << 3) | 3]) + bytes(base[1:]))
    broken.send_raw("version 0", bytes([3]) + bytes(base[1:]))
    broken.send_raw("symmetric active", bytes([(4 << 3) | 1]) + bytes(base[1:]))
    broken.send_raw("broadcast", bytes([(4 << 3) | 5]) + bytes(base[1:]))
    broken.send_raw("zero transmit", bytes(broken.request(4, 3, 0)))
    broken.send_raw("6-byte extension", bytes(base) + bytes(6))
    broken.send_raw("20-byte extension", bytes(base) + bytes(20))

    print("== admin")
    halt = bytearray(48)
    halt[0] = (4 << 3) | 7
    halt[4:8] = b"HALT"
    admin.sock.sendto(bytes(halt), addr)
    t.join()

    print("== server log")
    for line in server.log:
        print(line)
    print("== counts")
    for k in sorted(server.counts):
        print(f"{k}: {server.counts[k]}")


main()
