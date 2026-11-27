#!/usr/bin/env python3
"""Mirror of radius_server.kara: a RADIUS server (RFC 2865 authentication,
RFC 2866 accounting, RFC 3579 Message-Authenticator, RFC 5997
Status-Server) on two loopback UDP ports, one thread per port, and the
scripted NAS clients that drive it. Written independently with socket,
threading, hashlib and hmac. Every secret and password is obviously fake."""

import hashlib
import hmac
import socket
import threading

ACCESS_REQUEST, ACCESS_ACCEPT, ACCESS_REJECT = 1, 2, 3
ACCOUNTING_REQUEST, ACCOUNTING_RESPONSE = 4, 5
ACCESS_CHALLENGE, STATUS_SERVER = 11, 12

USER_NAME, USER_PASSWORD, CHAP_PASSWORD = 1, 2, 3
SERVICE_TYPE, FRAMED_IP = 6, 8
REPLY_MESSAGE, STATE, CLASS, VENDOR, SESSION_TIMEOUT = 18, 24, 25, 26, 27
ACCT_STATUS, ACCT_IN, ACCT_OUT, ACCT_SESSION_ID = 40, 42, 43, 44
ACCT_TIME, ACCT_CAUSE, CHAP_CHALLENGE, MESSAGE_AUTH = 46, 49, 60, 80

CODE_NAMES = {
    1: "Access-Request", 2: "Access-Accept", 3: "Access-Reject",
    4: "Accounting-Request", 5: "Accounting-Response",
    11: "Access-Challenge", 12: "Status-Server",
}
CAUSES = {1: "User-Request", 2: "Lost-Carrier", 4: "Idle-Timeout",
          5: "Session-Timeout", 10: "NAS-Request", 11: "NAS-Reboot"}
HALT = bytes([0, 1, 0x86, 0x9F, 1, 6]) + b"halt"  # vendor 99999, sub 1
SHORT_WAIT = 0.2
LONG_WAIT = 5.0


def md5(data):
    return hashlib.md5(data).digest()


def hmac_md5(key, data):
    return hmac.new(key, data, "md5").digest()


def hexs(b):
    return b.hex()


def be32(v):
    return v.to_bytes(4, "big")


def attr(t, v):
    return bytes([t, len(v) + 2]) + v


def ip_bytes(s):
    return bytes(int(p) for p in s.split("."))


def ip_text(b):
    return ".".join(str(x) for x in b)


def text(b):
    return b.decode("utf-8", "replace")


def pap_hide(password, secret, ra):
    p = password + bytes((-len(password)) % 16 or (16 if not password else 0))
    out = b""
    prev = ra
    for i in range(0, len(p), 16):
        b = md5(secret + prev)
        c = bytes(x ^ y for x, y in zip(p[i:i + 16], b))
        out += c
        prev = c
    return out


def pap_reveal(hidden, secret, ra):
    out = b""
    prev = ra
    for i in range(0, len(hidden), 16):
        c = hidden[i:i + 16]
        b = md5(secret + prev)
        out += bytes(x ^ y for x, y in zip(c, b))
        prev = c
    return out.rstrip(b"\0")


def parse_attrs(pkt):
    """[(type, value, offset of value)] or the offset of a bad attribute."""
    out = []
    i = 20
    while i < len(pkt):
        if i + 2 > len(pkt):
            return i
        t, n = pkt[i], pkt[i + 1]
        if n < 2 or i + n > len(pkt):
            return i
        out.append((t, pkt[i + 2:i + n], i + 2))
        i += n
    return out


def first(attrs, t):
    for at, v, _ in attrs:
        if at == t:
            return v
    return None


def int_of(v):
    return int.from_bytes(v, "big") if v is not None else 0


def finish(code, ident, ra, body, with_ma, secret):
    """A reply: Message-Authenticator first when asked, then the Response
    Authenticator over the finished attributes."""
    if with_ma:
        body = body + attr(MESSAGE_AUTH, bytes(16))
    length = 20 + len(body)
    head = bytes([code, ident]) + length.to_bytes(2, "big")
    if with_ma:
        mac = hmac_md5(secret, head + ra + body)
        body = body[:-16] + mac
    auth = md5(head + ra + body + secret)
    return head + auth + body


class User:
    def __init__(self, password, ip, timeout, cls, max_sessions=0, otp="", disabled=False):
        self.password = password
        self.ip = ip
        self.timeout = timeout
        self.cls = cls
        self.max_sessions = max_sessions
        self.otp = otp
        self.disabled = disabled


class Session:
    def __init__(self, user, nas, ip):
        self.user = user
        self.nas = nas
        self.ip = ip
        self.octets_in = 0
        self.octets_out = 0
        self.time = 0


class State:
    def __init__(self):
        self.users = {
            "alice": User("fake-alice-pw", "10.0.0.11", 3600, "staff", max_sessions=1),
            "bob": User("fake-bob-pw", "10.0.0.12", 1800, "staff"),
            "carol": User("fake-carol-pw", "10.0.0.13", 900, "contractor", otp="424242"),
            "dave": User("fake-dave-pw", "10.0.0.14", 600, "staff", disabled=True),
            "erin": User("fake-erin-a-rather-long-passphrase-0042", "10.0.0.15", 7200, "staff"),
        }
        self.sessions = {}
        self.closed = []
        self.challenges = {}
        self.next_challenge = 0
        self.counts = {}

    def count(self, k):
        self.counts[k] = self.counts.get(k, 0) + 1

    def open_for(self, user):
        return sum(1 for s in self.sessions.values() if s.user == user)

    def close(self, key, cause):
        s = self.sessions.pop(key)
        self.closed.append(f"{key} {s.user} in {s.octets_in} out {s.octets_out} {s.time}s {cause}")


class Drop(Exception):
    pass


class Port:
    def __init__(self, name, sock, peers, state, lock):
        self.name = name
        self.sock = sock
        self.peers = peers
        self.state = state
        self.lock = lock
        self.log = []
        self.cache = {}
        self.requests = 0
        self.running = True

    def serve(self):
        while self.running:
            data, addr = self.sock.recvfrom(4096)
            self.requests += 1
            peer = self.peers.get(f"{addr[0]}:{addr[1]}")
            if peer is None:
                self.dropped("?", "unknown client")
                continue
            nas, secret = peer
            try:
                reply, what = self.handle(nas, secret, data)
            except Drop as e:
                self.dropped(nas, str(e))
                continue
            self.log.append(f"{nas}: {what}")
            self.sock.sendto(reply, addr)

    def dropped(self, nas, why):
        self.log.append(f"{nas}: dropped, {why}")
        with self.lock:
            self.state.count("dropped")

    def handle(self, nas, secret, data):
        n = len(data)
        if n < 20:
            raise Drop(f"short packet of {n} bytes")
        length = int.from_bytes(data[2:4], "big")
        if length < 20 or length > 4096:
            raise Drop(f"length {length} out of range")
        if length > n:
            raise Drop(f"length {length} beyond the {n}-byte datagram")
        pkt = data[:length]
        attrs = parse_attrs(pkt)
        if isinstance(attrs, int):
            raise Drop(f"bad attribute at offset {attrs}")
        code, ident, ra = pkt[0], pkt[1], pkt[4:20]
        allowed = (ACCESS_REQUEST, STATUS_SERVER) if self.name == "auth" else (ACCOUNTING_REQUEST, STATUS_SERVER)
        if code not in allowed:
            raise Drop(f"code {code} on the {self.name} port")
        cached = self.cache.get((nas, ident))
        if cached is not None and cached[0] == ra:
            with self.lock:
                self.state.count("duplicate")
            return cached[1], f"id {ident} again, cached reply resent"
        ma_at = None
        for t, v, off in attrs:
            if t == MESSAGE_AUTH:
                if len(v) != 16:
                    raise Drop(f"Message-Authenticator of {len(v)} bytes")
                ma_at = off
        if ma_at is not None:
            zeroed = pkt[:ma_at] + bytes(16) + pkt[ma_at + 16:]
            if not hmac.compare_digest(hmac_md5(secret, zeroed), pkt[ma_at:ma_at + 16]):
                raise Drop("bad Message-Authenticator")
        if code == STATUS_SERVER and ma_at is None:
            raise Drop("Status-Server without Message-Authenticator")
        if code == ACCOUNTING_REQUEST:
            want = md5(pkt[:4] + bytes(16) + pkt[20:] + secret)
            if want != ra:
                raise Drop("bad request authenticator")
        with self.lock:
            if code == STATUS_SERVER:
                rcode, body, what = self.status(attrs)
            elif code == ACCESS_REQUEST:
                rcode, body, what = self.access(secret, ra, attrs)
            else:
                rcode, body, what = self.accounting(nas, attrs)
            self.state.count(CODE_NAMES[rcode].lower())
        reply = finish(rcode, ident, ra, body, ma_at is not None, secret)
        self.cache[(nas, ident)] = (ra, reply)
        if n > length:
            what += f", {n - length} padding bytes ignored"
        return reply, what

    def status(self, attrs):
        if first(attrs, VENDOR) == HALT:
            self.running = False
            what = "status, halt"
        else:
            what = "status"
        self.state.count("status-server")
        if self.name == "auth":
            body = attr(REPLY_MESSAGE, f"up, {self.requests} requests".encode())
            return ACCESS_ACCEPT, body, f"{what} -> Access-Accept"
        return ACCOUNTING_RESPONSE, b"", f"{what} -> Accounting-Response"

    def access(self, secret, ra, attrs):
        st = self.state
        name = first(attrs, USER_NAME)
        pw = first(attrs, USER_PASSWORD)
        chap = first(attrs, CHAP_PASSWORD)
        state = first(attrs, STATE)
        if pw is not None and chap is not None:
            raise Drop("both User-Password and CHAP-Password")
        if pw is None and chap is None and state is None:
            raise Drop("no User-Password, CHAP-Password or State")
        if pw is not None and (len(pw) % 16 != 0 or not 16 <= len(pw) <= 128):
            raise Drop(f"User-Password of {len(pw)} bytes")
        if chap is not None and len(chap) != 17:
            raise Drop(f"CHAP-Password of {len(chap)} bytes")
        method = "PAP" if pw is not None else "CHAP" if chap is not None else "State"
        uname = text(name) if name is not None else "(none)"
        head = f"access {uname} by {method}"

        def reject(why):
            return ACCESS_REJECT, attr(REPLY_MESSAGE, why.encode()), f"{head} -> Access-Reject, {why}"

        if name is None:
            return reject("No User-Name")
        u = st.users.get(uname)
        if u is None:
            return reject("Unknown user")
        if u.disabled:
            return reject("Account disabled")
        if state is not None:
            owner = st.challenges.pop(text(state), None)
            if owner != uname or pw is None:
                return reject("Unknown State")
            if pap_reveal(pw, secret, ra) != u.otp.encode():
                return reject("Wrong code")
        else:
            if pw is not None:
                ok = pap_reveal(pw, secret, ra) == u.password.encode()
            else:
                challenge = first(attrs, CHAP_CHALLENGE)
                if challenge is None:
                    challenge = ra
                ok = md5(chap[:1] + u.password.encode() + challenge) == chap[1:]
            if not ok:
                return reject("Wrong password")
            if u.otp:
                st.next_challenge += 1
                s = f"otp-{st.next_challenge}"
                st.challenges[s] = uname
                body = attr(STATE, s.encode()) + attr(REPLY_MESSAGE, b"Enter the 6-digit code")
                return ACCESS_CHALLENGE, body, f"{head} -> Access-Challenge, {s}"
        if u.max_sessions > 0:
            n = st.open_for(uname)
            if n >= u.max_sessions:
                return reject(f"Already logged in ({n} of {u.max_sessions})")
        body = (attr(SERVICE_TYPE, be32(2)) + attr(FRAMED_IP, ip_bytes(u.ip))
                + attr(SESSION_TIMEOUT, be32(u.timeout)) + attr(CLASS, u.cls.encode())
                + attr(REPLY_MESSAGE, f"Welcome, {uname}".encode()))
        return ACCESS_ACCEPT, body, f"{head} -> Access-Accept"

    def accounting(self, nas, attrs):
        st = self.state
        kind = first(attrs, ACCT_STATUS)
        if kind is None:
            raise Drop("no Acct-Status-Type")
        kind = int_of(kind)
        sid = first(attrs, ACCT_SESSION_ID)
        user = first(attrs, USER_NAME)
        user = text(user) if user is not None else "(none)"
        if kind in (1, 2, 3):
            if sid is None:
                raise Drop("no Acct-Session-Id")
            key = f"{nas}/{text(sid)}"
        if kind == 1:
            if key in st.sessions:
                what = f"start {key} for {user}: already open"
            else:
                ip = first(attrs, FRAMED_IP)
                st.sessions[key] = Session(user, nas, ip_text(ip) if ip is not None else "-")
                what = f"start {key} for {user}"
        elif kind == 3:
            s = st.sessions.get(key)
            if s is None:
                what = f"interim {key}: unknown session"
            else:
                s.octets_in = int_of(first(attrs, ACCT_IN))
                s.octets_out = int_of(first(attrs, ACCT_OUT))
                s.time = int_of(first(attrs, ACCT_TIME))
                what = f"interim {key}: in {s.octets_in} out {s.octets_out} {s.time}s"
        elif kind == 2:
            s = st.sessions.get(key)
            if s is None:
                what = f"stop {key}: unknown session"
            else:
                s.octets_in = int_of(first(attrs, ACCT_IN))
                s.octets_out = int_of(first(attrs, ACCT_OUT))
                s.time = int_of(first(attrs, ACCT_TIME))
                cause = CAUSES.get(int_of(first(attrs, ACCT_CAUSE)), "?")
                st.close(key, cause)
                what = f"stop {key} for {user}: {s.time}s, {cause}"
        elif kind in (7, 8):
            keys = sorted(k for k, s in st.sessions.items() if s.nas == nas)
            for k in keys:
                st.close(k, "NAS-Reboot")
            word = "on" if kind == 7 else "off"
            what = f"accounting-{word}: closed {len(keys)} session(s)"
        else:
            raise Drop(f"Acct-Status-Type {kind}")
        return ACCOUNTING_RESPONSE, b"", f"{what} -> Accounting-Response"


class Req:
    def __init__(self, code, ident, ra):
        self.code = code
        self.ident = ident
        self.ra = ra
        self.attrs = b""

    def add(self, t, v):
        self.attrs += attr(t, v)

    def add_text(self, t, s):
        self.add(t, s.encode())

    def add_int(self, t, v):
        self.add(t, be32(v))


class Client:
    def __init__(self, name, secret, auth, acct, peers, real_secret=None):
        self.name = name
        self.secret = secret
        self.auth = auth
        self.acct = acct
        self.next_id = 250
        self.seq = 0
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        if real_secret is not None:
            a = self.sock.getsockname()
            peers[f"{a[0]}:{a[1]}"] = (name, real_secret)

    def start(self, code):
        ident = self.next_id
        self.next_id = (self.next_id + 1) % 256
        self.seq += 1
        ra = md5(f"{self.name}:{self.seq}".encode())
        return Req(code, ident, ra)

    def pap(self, req, password):
        req.add(USER_PASSWORD, pap_hide(password.encode(), self.secret, req.ra))

    def chap(self, req, ident, password, challenge):
        if challenge is not None:
            req.add(CHAP_CHALLENGE, challenge)
            c = challenge
        else:
            c = req.ra
        req.add(CHAP_PASSWORD, bytes([ident]) + md5(bytes([ident]) + password.encode() + c))

    def encode(self, req, ma, bad_ma):
        body = req.attrs
        if ma:
            body += attr(MESSAGE_AUTH, bytes(16))
        length = 20 + len(body)
        head = bytes([req.code, req.ident]) + length.to_bytes(2, "big")
        if req.code == ACCOUNTING_REQUEST:
            req.ra = md5(head + bytes(16) + body + self.secret)
        if ma:
            mac = hmac_md5(self.secret, head + req.ra + body)
            if bad_ma:
                mac = bytes([mac[0] ^ 1]) + mac[1:]
            body = body[:-16] + mac
        return head + req.ra + body

    def send(self, label, req, port, ma=False, bad_ma=False, quiet=False):
        data = self.encode(req, ma, bad_ma)
        self.exchange(label, data, req, port, quiet)
        return data

    def raw(self, label, data, req, port, quiet=True):
        self.exchange(label, data, req, port, quiet)

    def exchange(self, label, data, req, port, quiet):
        self.sock.settimeout(SHORT_WAIT if quiet else LONG_WAIT)
        self.sock.sendto(data, self.auth if port == "auth" else self.acct)
        try:
            reply, _ = self.sock.recvfrom(4096)
        except socket.timeout:
            print(f"{self.name}: {label} -> no reply")
            return
        self.report(label, reply, req)

    def report(self, label, reply, req):
        code, ident = reply[0], reply[1]
        length = int.from_bytes(reply[2:4], "big")
        body = reply[20:length]
        auth = reply[4:20]
        want = md5(reply[:4] + req.ra + body + self.secret)
        verdict = "ok" if want == auth else "BAD"
        attrs = parse_attrs(reply[:length])
        shown = []
        for t, v, off in attrs:
            if t in (SERVICE_TYPE, SESSION_TIMEOUT):
                shown.append(f"{name_of(t)}={int_of(v)}")
            elif t == FRAMED_IP:
                shown.append(f"{name_of(t)}={ip_text(v)}")
            elif t in (REPLY_MESSAGE, STATE, CLASS):
                shown.append(f'{name_of(t)}="{text(v)}"')
            elif t == MESSAGE_AUTH:
                zeroed = reply[:4] + req.ra + reply[20:off] + bytes(16) + reply[off + 16:length]
                ok = hmac_md5(self.secret, zeroed) == v
                shown.append(f"Message-Authenticator={'ok' if ok else 'BAD'}")
            else:
                shown.append(f"Attr-{t}={hexs(v)}")
        tail = " | " + " ".join(shown) if shown else ""
        print(f"{self.name}: {label} -> {CODE_NAMES.get(code, str(code))} id {ident} auth {verdict} {hexs(auth)[:8]}{tail}")


def name_of(t):
    return {SERVICE_TYPE: "Service-Type", FRAMED_IP: "Framed-IP-Address",
            REPLY_MESSAGE: "Reply-Message", STATE: "State", CLASS: "Class",
            SESSION_TIMEOUT: "Session-Timeout"}[t]


def access(c, label, user, password, port="auth", ma=False, bad_ma=False, quiet=False):
    r = c.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, user)
    c.pap(r, password)
    r.add(FRAMED_IP, ip_bytes("127.0.0.1"))
    return c.send(label, r, port, ma, bad_ma, quiet)


def acct(c, label, kind, user, sid, extra=(), quiet=False, bad_auth=False):
    r = c.start(ACCOUNTING_REQUEST)
    if kind:
        r.add_int(ACCT_STATUS, kind)
    if user:
        r.add_text(USER_NAME, user)
    if sid:
        r.add_text(ACCT_SESSION_ID, sid)
    for t, v in extra:
        r.add(t, v)
    if bad_auth:
        data = c.encode(r, False, False)
        data = data[:4] + bytes([data[4] ^ 1]) + data[5:]
        c.raw(label, data, r, "acct", quiet=True)
        return
    c.send(label, r, "acct", quiet=quiet)


def status(c, label, port, halt=False, ma=True):
    r = c.start(STATUS_SERVER)
    if halt:
        r.add(VENDOR, HALT)
    c.send(label, r, port, ma=ma, quiet=not ma)


def main():
    auth_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    auth_sock.bind(("127.0.0.1", 0))
    acct_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    acct_sock.bind(("127.0.0.1", 0))
    auth = auth_sock.getsockname()
    acctaddr = acct_sock.getsockname()
    peers = {}
    nas1 = Client("nas1", b"fake-secret-nas1", auth, acctaddr, peers, b"fake-secret-nas1")
    nas2 = Client("nas2", b"fake-secret-nas2", auth, acctaddr, peers, b"fake-secret-nas2")
    nas3 = Client("nas3", b"fake-secret-old", auth, acctaddr, peers, b"fake-secret-nas3")
    rogue = Client("rogue", b"fake-secret-rogue", auth, acctaddr, peers)
    state = State()
    lock = threading.Lock()
    ports = [Port("auth", auth_sock, dict(peers), state, lock),
             Port("acct", acct_sock, dict(peers), state, lock)]
    threads = [threading.Thread(target=p.serve) for p in ports]
    for t in threads:
        t.start()

    print("== pap")
    access(nas1, "alice, right password", "alice", "fake-alice-pw")
    access(nas1, "alice, wrong password", "alice", "fake-alice-PW")
    access(nas1, "erin, a 39-byte password", "erin", "fake-erin-a-rather-long-passphrase-0042")
    access(nas1, "erin, cut at 16 bytes", "erin", "fake-erin-a-rath")
    access(nas1, "ghost", "ghost", "fake-ghost-pw")
    access(nas1, "dave, disabled", "dave", "fake-dave-pw")
    r = nas1.start(ACCESS_REQUEST)
    nas1.pap(r, "fake-alice-pw")
    nas1.send("no User-Name", r, "auth")

    print("== chap")
    for label, ch, pw in [("bob, CHAP-Challenge", b"0123456789abcdef", "fake-bob-pw"),
                          ("bob, authenticator as challenge", None, "fake-bob-pw"),
                          ("bob, wrong password", b"fedcba9876543210", "fake-bob-PW")]:
        r = nas2.start(ACCESS_REQUEST)
        r.add_text(USER_NAME, "bob")
        nas2.chap(r, 7, pw, ch)
        nas2.send(label, r, "auth")
    r = nas2.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "alice")
    nas2.chap(r, 200, "fake-alice-pw", b"a 24-byte challenge str!")
    nas2.send("alice by CHAP, 24-byte challenge", r, "auth")

    print("== challenge")
    access(nas1, "carol, password", "carol", "fake-carol-pw")
    r = nas1.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "carol")
    r.add_text(STATE, "otp-1")
    nas1.pap(r, "424242")
    nas1.send("carol, the code", r, "auth")
    access(nas1, "carol, password again", "carol", "fake-carol-pw")
    for label in ["carol, a wrong code", "carol, otp-2 replayed"]:
        r = nas1.start(ACCESS_REQUEST)
        r.add_text(USER_NAME, "carol")
        r.add_text(STATE, "otp-2")
        nas1.pap(r, "000000")
        nas1.send(label, r, "auth")
    r = nas1.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "bob")
    r.add_text(STATE, "otp-3")
    nas1.pap(r, "424242")
    nas1.send("bob, a State never issued", r, "auth")

    print("== message-authenticator")
    access(nas2, "alice, signed", "alice", "fake-alice-pw", ma=True)
    access(nas2, "alice, signature off by a bit", "alice", "fake-alice-pw", ma=True, bad_ma=True, quiet=True)

    print("== accounting")
    ip = (FRAMED_IP, ip_bytes("10.0.0.11"))
    acct(nas1, "start alice s-1", 1, "alice", "s-1", [ip])
    access(nas1, "alice, while s-1 is open", "alice", "fake-alice-pw")
    acct(nas1, "start alice s-1 again", 1, "alice", "s-1", [ip])
    acct(nas1, "interim s-1", 3, "alice", "s-1",
         [(ACCT_IN, be32(1200)), (ACCT_OUT, be32(34000)), (ACCT_TIME, be32(60))])
    acct(nas1, "stop s-1", 2, "alice", "s-1",
         [(ACCT_IN, be32(5000)), (ACCT_OUT, be32(99000)), (ACCT_TIME, be32(300)), (ACCT_CAUSE, be32(1))])
    access(nas1, "alice, after the stop", "alice", "fake-alice-pw")
    acct(nas2, "start bob b-1", 1, "bob", "b-1", [(FRAMED_IP, ip_bytes("10.0.0.12"))])
    acct(nas2, "start bob b-2", 1, "bob", "b-2")
    acct(nas2, "interim b-2", 3, "bob", "b-2",
         [(ACCT_IN, be32(77)), (ACCT_OUT, be32(4096)), (ACCT_TIME, be32(15))])
    acct(nas1, "start erin e-1", 1, "erin", "e-1", [(FRAMED_IP, ip_bytes("10.0.0.15"))])
    acct(nas2, "accounting-on (nas2 rebooted)", 7, "", "")
    acct(nas2, "interim b-1 after the reboot", 3, "bob", "b-1")
    acct(nas1, "stop of an unknown session", 2, "alice", "nope", [(ACCT_CAUSE, be32(4))])
    acct(nas1, "start, bad request authenticator", 1, "alice", "s-9", bad_auth=True)
    acct(nas1, "no Acct-Status-Type", 0, "alice", "s-9", quiet=True)
    acct(nas1, "start without a session id", 1, "alice", "", quiet=True)
    acct(nas1, "Acct-Status-Type 9", 9, "alice", "s-9", quiet=True)
    acct(nas1, "start alice s-2", 1, "alice", "s-2", [ip])

    print("== duplicates")
    data = access(nas2, "bob, first send", "bob", "fake-bob-pw")
    r = nas2.start(ACCESS_REQUEST)
    r.ident = data[1]
    r.ra = data[4:20]
    nas2.raw("bob, the same bytes again", data, r, "auth", quiet=False)
    nas2.next_id = data[1]
    access(nas2, "same id, new authenticator", "bob", "fake-bob-PW")

    print("== broken")
    access(rogue, "an unknown client", "alice", "fake-alice-pw", quiet=True)
    access(nas3, "nas3 with an old secret", "alice", "fake-alice-pw")
    access(nas3, "nas3 with an old secret, signed", "alice", "fake-alice-pw", ma=True, quiet=True)
    r = nas1.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "bob")
    nas1.pap(r, "fake-bob-pw")
    good = nas1.encode(r, False, False)
    nas1.raw("19 bytes", good[:19], r, "auth")
    nas1.raw("length 4097", good[:2] + (4097).to_bytes(2, "big") + good[4:], r, "auth")
    nas1.raw("length past the datagram", good[:2] + (len(good) + 6).to_bytes(2, "big") + good[4:], r, "auth")
    bad = bytearray(good)
    bad[21] = 1
    nas1.raw("attribute of length 1", bytes(bad), r, "auth")
    bad = bytearray(good)
    bad[21] = 60
    nas1.raw("attribute past the end", bytes(bad), r, "auth")
    nas1.raw("8 bytes of padding", good + bytes(8), r, "auth", quiet=False)
    bad = bytearray(good)
    bad[0] = ACCESS_ACCEPT
    nas1.raw("an Access-Accept sent to the server", bytes(bad), r, "auth")
    bad[0] = ACCOUNTING_REQUEST
    nas1.raw("an Accounting-Request on the auth port", bytes(bad), r, "auth")
    bad[0] = ACCESS_REQUEST
    nas1.raw("an Access-Request on the acct port", bytes(bad), r, "acct")
    r = nas1.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "bob")
    r.add(USER_PASSWORD, bytes(20))
    nas1.send("a 20-byte User-Password", r, "auth", quiet=True)
    r = nas1.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "bob")
    nas1.pap(r, "fake-bob-pw")
    nas1.chap(r, 1, "fake-bob-pw", None)
    nas1.send("PAP and CHAP together", r, "auth", quiet=True)
    r = nas1.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "bob")
    nas1.send("no credentials", r, "auth", quiet=True)
    r = nas1.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "bob")
    r.add(CHAP_PASSWORD, bytes(16))
    nas1.send("a 16-byte CHAP-Password", r, "auth", quiet=True)
    r = nas1.start(ACCESS_REQUEST)
    r.add_text(USER_NAME, "bob")
    nas1.pap(r, "fake-bob-pw")
    r.add(MESSAGE_AUTH, bytes(8))
    nas1.send("an 8-byte Message-Authenticator", r, "auth", quiet=True)
    status(nas1, "Status-Server unsigned", "auth", ma=False)

    print("== status")
    status(nas2, "Status-Server to auth", "auth")
    status(nas2, "Status-Server to acct", "acct")
    status(nas1, "halt auth", "auth", halt=True)
    status(nas1, "halt acct", "acct", halt=True)
    for t in threads:
        t.join()

    for p in ports:
        print(f"== {p.name} log ({p.requests} datagrams)")
        for line in p.log:
            print(line)
    print("== open sessions")
    for k in sorted(state.sessions):
        s = state.sessions[k]
        print(f"{k} {s.user} {s.ip} in {s.octets_in} out {s.octets_out} {s.time}s")
    print("== closed sessions")
    for line in state.closed:
        print(line)
    print("== counts")
    for k in sorted(state.counts):
        print(f"{k} {state.counts[k]}")


if __name__ == "__main__":
    main()
