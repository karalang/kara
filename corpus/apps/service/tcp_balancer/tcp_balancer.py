"""Oracle for tcp_balancer.kara: the same layer-4 balancer over real sockets.

Backends, the front and the clients are plain `socket` code on threads. The
self-test's health checks reach a fake network: an object whose `connect`,
`write` and `read` answer each check by the same script as the Kara
program's `FakeNet` provider. The Kara read timeout is set on every probe;
the fake accepts it and changes nothing.
"""

import socket
import sys
import threading
import time

FRONT = ("127.0.0.1", 47756)
ALPHA = ("127.0.0.1", 47757)
BETA = ("127.0.0.1", 47758)
GAMMA = ("127.0.0.1", 47759)
RISE = 2
FALL = 2
CHECK_TIMEOUT_MS = 500

ROUND_ROBIN = "rr"
LEAST_CONN = "lc"


class NetError(Exception):
    pass


class Backend:
    def __init__(self, name, addr):
        self.name = name
        self.addr = addr
        self.up = False
        self.streak = 0
        self.active = 0
        self.total = 0
        self.bytes_in = 0
        self.bytes_out = 0


class Balancer:
    def __init__(self, backends):
        self.backends = [Backend(n, a) for n, a in backends]
        self.next = 0
        self.policy = ROUND_ROBIN
        self.events = []
        self.lock = threading.Lock()
        self.open = 0
        self.ready = False

    def log(self, e):
        with self.lock:
            self.events.append(e)

    def take_events(self):
        with self.lock:
            ev, self.events = self.events, []
            return ev


# ------------------------------------------------------------ networks

class RealNet:
    def connect(self, addr):
        host, port = addr
        try:
            s = socket.create_connection((host, port))
        except ConnectionRefusedError:
            raise NetError("connection refused")
        return s

    def set_timeout(self, s, ms):
        s.settimeout(ms / 1000)

    def write(self, s, data):
        s.sendall(data)

    def read(self, s):
        try:
            return s.recv(64)
        except socket.timeout:
            raise NetError("timed out")

    def close(self, s):
        s.close()


class FakeNet:
    """The Kara program's FakeNet: alpha always PONG, beta FAIL for its
    first two checks, gamma refuses its first three connections, delta
    answers nothing."""

    def __init__(self):
        self.next = 0
        self.handles = {}
        self.attempts = {}
        self.answered = set()

    def connect(self, addr):
        n = self.attempts.get(addr, 0) + 1
        self.attempts[addr] = n
        if addr == "gamma:1" and n <= 3:
            raise NetError("connection refused")
        self.next += 1
        self.handles[self.next] = addr
        return self.next

    def set_timeout(self, h, ms):
        pass

    def write(self, h, data):
        pass

    def read(self, h):
        if h in self.answered:
            return b""
        self.answered.add(h)
        addr = self.handles[h]
        n = self.attempts[addr]
        reply = {"alpha:1": "PONG\n", "beta:1": "FAIL\n" if n <= 2 else "PONG\n", "gamma:1": "PONG\n"}.get(addr, "")
        return reply.encode()

    def close(self, h):
        pass


# ------------------------------------------------------------ health checks

def probe(net, addr):
    try:
        s = net.connect(addr)
    except NetError as e:
        return f"connect: {e}"
    try:
        net.set_timeout(s, CHECK_TIMEOUT_MS)
        net.write(s, b"PING\n")
        got = b""
        while True:
            try:
                d = net.read(s)
            except NetError as e:
                return f"read: {e}"
            if not d:
                break
            got += d
            if b"\n" in got:
                break
    finally:
        net.close(s)
    line = got.decode(errors="replace").strip()
    if line == "PONG":
        return None
    if not line:
        return "closed without an answer"
    return f"answered {line}"


def check_round(bal, net):
    with bal.lock:
        addrs = [b.addr for b in bal.backends]
    results = [probe(net, a) for a in addrs]
    with bal.lock:
        for b, why in zip(bal.backends, results):
            if why is None:
                if b.up:
                    b.streak = 0
                else:
                    b.streak += 1
                    if b.streak >= RISE:
                        b.up = True
                        b.streak = 0
                        bal.events.append(f"{b.name} up")
            else:
                if not b.up:
                    b.streak = 0
                else:
                    b.streak += 1
                    if b.streak >= FALL:
                        b.up = False
                        b.streak = 0
                        bal.events.append(f"{b.name} down: {why}")


# --------------------------------------------------------------- balancing

def pick(bal):
    n = len(bal.backends)
    if bal.policy == ROUND_ROBIN:
        for k in range(n):
            i = (bal.next + k) % n
            if bal.backends[i].up:
                bal.next = (i + 1) % n
                return i
        return None
    best = None
    for i, b in enumerate(bal.backends):
        if not b.up:
            continue
        if best is None or b.active < bal.backends[best].active:
            best = i
    return best


def pipe(src, dst):
    total = 0
    while True:
        try:
            d = src.recv(1024)
        except OSError:
            break
        if not d:
            break
        try:
            dst.sendall(d)
        except OSError:
            break
        total += len(d)
    try:
        dst.shutdown(socket.SHUT_WR)
    except OSError:
        pass
    return total


def refuse(client, why):
    try:
        client.sendall(f"{why}\n".encode())
        client.shutdown(socket.SHUT_WR)
        client.settimeout(1)
        while client.recv(256):
            pass
    except OSError:
        pass


def proxy(bal, client):
    with bal.lock:
        i = pick(bal)
        if i is not None:
            b = bal.backends[i]
            b.active += 1
            b.total += 1
            addr, name = b.addr, b.name
    if i is None:
        refuse(client, "503 no backend available")
        client.close()
        bal.log("refused a client: no backend up")
        with bal.lock:
            bal.open -= 1
        return
    try:
        up = socket.create_connection(addr)
    except OSError as e:
        refuse(client, f"502 {name} unreachable")
        client.close()
        with bal.lock:
            bal.backends[i].active -= 1
            bal.events.append(f"{name}: connect failed: {e}")
            bal.open -= 1
        return
    box = {}
    t = threading.Thread(target=lambda: box.setdefault("sent", pipe(client, up)))
    t.start()
    received = pipe(up, client)
    t.join()
    up.close()
    client.close()
    with bal.lock:
        b = bal.backends[i]
        b.active -= 1
        b.bytes_in += box["sent"]
        b.bytes_out += received
        bal.open -= 1


def listen(addr):
    l = socket.socket()
    l.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    l.bind(addr)
    l.listen(64)
    return l


def front(bal):
    l = listen(FRONT)
    bal.ready = True
    while True:
        c, _ = l.accept()
        with bal.lock:
            bal.open += 1
        threading.Thread(target=proxy, args=(bal, c), daemon=True).start()


# ----------------------------------------------------------------- backends

def serve_backend(name, conn, sick, me):
    pending = b""
    while True:
        try:
            d = conn.recv(256)
        except OSError:
            break
        if not d:
            break
        pending += d
        while b"\n" in pending:
            raw, pending = pending.split(b"\n", 1)
            line = raw.decode(errors="replace").rstrip()
            if line == "PING":
                reply = "FAIL" if sick[me] else "PONG"
            else:
                reply = f"{name} {line.upper()}"
            try:
                conn.sendall(f"{reply}\n".encode())
            except OSError:
                conn.close()
                return
    conn.close()


def backend(name, addr, sick, me):
    l = listen(addr)
    while True:
        c, _ = l.accept()
        threading.Thread(target=serve_backend, args=(name, c, sick, me), daemon=True).start()


# ---------------------------------------------------------------- self-test

def show_pool(bal):
    with bal.lock:
        states = []
        for b in bal.backends:
            streak = f"({b.streak})" if b.streak > 0 else ""
            states.append(f"{b.name}={'up' if b.up else 'down'}{streak}")
    print(f"    {' '.join(states)}")


def self_test():
    print("== self-test: health checks")
    bal = Balancer([("alpha", "alpha:1"), ("beta", "beta:1"), ("gamma", "gamma:1"), ("delta", "delta:1")])
    net = FakeNet()
    for rnd in range(1, 6):
        check_round(bal, net)
        print(f"  round {rnd}")
        for e in bal.take_events():
            print(f"    event: {e}")
        show_pool(bal)

    print("== self-test: picking")
    order = []
    for _ in range(5):
        i = pick(bal)
        order.append(bal.backends[i].name if i is not None else "none")
    print(f"  round-robin: {' '.join(order)}")
    bal.policy = LEAST_CONN
    bal.backends[0].active = 3
    bal.backends[1].active = 1
    bal.backends[2].active = 1
    order = []
    for _ in range(4):
        i = pick(bal)
        if i is None:
            order.append("none")
        else:
            bal.backends[i].active += 1
            order.append(bal.backends[i].name)
    print(f"  least-conn from 3/1/1: {' '.join(order)}")
    for b in bal.backends:
        b.up = False
    print(f"  all down: {'true' if pick(bal) is None else 'false'}")


# --------------------------------------------------------------------- live

def wait_up(addrs):
    for _ in range(100):
        ok = True
        for a in addrs:
            try:
                socket.create_connection(a).close()
            except OSError:
                ok = False
                break
        if ok:
            return True
        time.sleep(0.02)
    return False


def wait_ready(bal):
    for _ in range(100):
        if bal.ready:
            return True
        time.sleep(0.02)
    return False


def settle(bal, n):
    for _ in range(500):
        with bal.lock:
            if bal.open == n:
                return
        time.sleep(0.005)
    print(f"  (still {bal.open} open, wanted {n})")


def read_all(s):
    out = b""
    while True:
        try:
            d = s.recv(256)
        except OSError as e:
            out += f"<os error {e.errno}>".encode()
            break
        if not d:
            break
        out += d
    return out.decode(errors="replace")


def client(bal, cid, lines, keep):
    try:
        s = socket.create_connection(FRONT)
    except OSError as e:
        print(f"  client {cid}: connect failed: {e}")
    else:
        for l in lines:
            s.sendall(f"{l}\n".encode())
        s.shutdown(socket.SHUT_WR)
        text = read_all(s)
        s.close()
        print(f"  client {cid}: {text.rstrip().replace(chr(10), ' | ')}")
    settle(bal, keep)


def rounds(bal, n):
    net = RealNet()
    for _ in range(n):
        check_round(bal, net)
    for e in bal.take_events():
        print(f"  event: {e}")


def spawn(f, *args):
    threading.Thread(target=f, args=args, daemon=True).start()


def main():
    self_test()

    sick = [False, False, False]
    bal = Balancer([("alpha", ALPHA), ("beta", BETA), ("gamma", GAMMA)])
    spawn(backend, "alpha", ALPHA, sick, 0)
    spawn(backend, "beta", BETA, sick, 1)
    spawn(front, bal)
    ok = wait_up([ALPHA, BETA]) and wait_ready(bal)
    print(f"== live: listening {'true' if ok else 'false'}")

    print("== live: alpha and beta come up; gamma is not running")
    rounds(bal, 2)
    client(bal, 1, ["hello", "world"], 0)
    client(bal, 2, ["one"], 0)
    client(bal, 3, ["two", "three", "four"], 0)
    client(bal, 4, [], 0)

    print("== live: beta reports FAIL")
    sick[1] = True
    rounds(bal, 1)
    rounds(bal, 1)
    client(bal, 5, ["five"], 0)
    client(bal, 6, ["six"], 0)

    print("== live: gamma starts")
    spawn(backend, "gamma", GAMMA, sick, 2)
    print(f"  listening {'true' if wait_up([GAMMA]) else 'false'}")
    rounds(bal, 2)
    client(bal, 7, ["seven"], 0)
    client(bal, 8, ["eight"], 0)

    print("== live: least connections, one client held open")
    with bal.lock:
        bal.policy = LEAST_CONN
    held = socket.create_connection(FRONT)
    held.sendall(b"held\n")
    got = held.recv(64)
    print(f"  held: {got.decode(errors='replace').rstrip()}")
    settle(bal, 1)
    client(bal, 9, ["nine"], 1)
    client(bal, 10, ["ten"], 1)
    held.shutdown(socket.SHUT_WR)
    print(f"  held, after: {read_all(held).rstrip()}")
    held.close()
    settle(bal, 0)

    print("== live: every backend sick")
    sick[0] = True
    sick[2] = True
    rounds(bal, 2)
    client(bal, 11, ["anyone?"], 0)
    for e in bal.take_events():
        print(f"  event: {e}")

    print("== live: totals")
    with bal.lock:
        for b in bal.backends:
            print(f"  {b.name}: {'up' if b.up else 'down'}, {b.total} connections, {b.bytes_in} bytes in, {b.bytes_out} bytes out")
    print("done")


if __name__ == "__main__":
    sys.exit(main())
