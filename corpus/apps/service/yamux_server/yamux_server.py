"""The mirror of yamux_server.kara: the same yamux session engine, server
and scripted clients, on socket and threading. It wrote expected.out."""

import socket
import struct
import threading
from collections import deque

VERSION = 0
TYPE_DATA, TYPE_WINDOW, TYPE_PING, TYPE_GO_AWAY = 0, 1, 2, 3
SYN, ACK, FIN, RST = 1, 2, 4, 8
WINDOW = 262144
GO_NORMAL, GO_PROTOCOL = 0, 1
SERVER_CHUNK = 16384
CLIENT_CHUNK = 10000
SESSIONS = 10
WAIT_S = 10.0
FNV_START = 2166136261


def header(kind, flags, stream, length):
    return struct.pack(">BBHII", VERSION, kind, flags, stream, length)


def parse_header(b):
    return struct.unpack(">BBHII", b)


def flag_names(flags):
    names = [n for bit, n in ((SYN, "SYN"), (ACK, "ACK"), (FIN, "FIN"), (RST, "RST")) if flags & bit]
    return "|".join(names) if names else "-"


def kind_name(kind):
    return {TYPE_DATA: "data", TYPE_WINDOW: "window update", TYPE_PING: "ping",
            TYPE_GO_AWAY: "go away"}.get(kind, f"type {kind}")


def read_full(conn, n):
    buf = bytearray()
    while len(buf) < n:
        try:
            got = conn.recv(n - len(buf))
        except OSError:
            return None
        if not got:
            return None
        buf += got
    return bytes(buf)


def fnv1a(h, data):
    for b in data:
        h ^= b
        h = (h * 16777619) & 0xFFFFFFFF
    return h


def cat_bytes(n):
    return bytes(i % 251 for i in range(n))


def put_bytes(n):
    return bytes((i * 7 + i // 256) % 256 for i in range(n))


class Stream:
    def __init__(self, sid, send_window, recv_max):
        self.id = sid
        self.label = "-"
        self.send_window = send_window
        self.recv_window = recv_max
        self.recv_max = recv_max
        self.out = bytearray()
        self.sent = 0
        self.syn_on_data = False
        self.fin_on_data = False
        self.fin_wanted = False
        self.fin_sent = False
        self.fin_received = False
        self.stalled = False
        self.bytes_in = self.bytes_out = 0
        self.frames_in = self.frames_out = 0
        self.stalls = self.updates = 0

    def summary(self, how):
        return (f"stream {self.id} {self.label}: in {self.bytes_in}/{self.frames_in}, "
                f"out {self.bytes_out}/{self.frames_out}, stalls {self.stalls}, "
                f"updates {self.updates}, {how}")


class Session:
    def __init__(self, conn, server):
        conn.settimeout(WAIT_S)
        self.conn = conn
        self.chunk = SERVER_CHUNK if server else CLIENT_CHUNK
        self.streams = {}
        self.control = deque()
        self.next_id = 2 if server else 1
        self.peer_parity = 1 if server else 0
        self.closing = False
        self.local_go_away = False
        self.remote_go_away = False
        self.frames_in = self.frames_out = 0
        self.bytes_in = self.bytes_out = 0
        self.dropped = 0
        self.log = []
        self.counts = {}

    def count(self, key, n=1):
        self.counts[key] = self.counts.get(key, 0) + n

    def send(self, frame):
        try:
            self.conn.sendall(frame)
        except OSError:
            pass
        self.frames_out += 1

    def open(self, label, by_data, extra):
        sid = self.next_id
        self.next_id += 2
        s = Stream(sid, WINDOW, WINDOW + extra)
        s.label = label
        if by_data:
            s.syn_on_data = True
        else:
            self.control.append(header(TYPE_WINDOW, SYN, sid, extra))
        self.streams[sid] = s
        self.count("streams opened")
        return sid

    def set_label(self, sid, label):
        if sid in self.streams:
            self.streams[sid].label = label

    def write(self, sid, data):
        if sid in self.streams:
            self.streams[sid].out += data

    def close(self, sid):
        if sid in self.streams:
            self.streams[sid].fin_wanted = True

    def reset(self, sid):
        s = self.streams.pop(sid, None)
        if s is not None:
            self.control.append(header(TYPE_WINDOW, RST, sid, 0))
            self.log.append(s.summary("reset"))
            self.count("resets sent")

    def ping(self, opaque):
        self.control.append(header(TYPE_PING, SYN, 0, opaque))

    def go_away(self, code):
        self.control.append(header(TYPE_GO_AWAY, 0, 0, code))
        self.local_go_away = True
        self.log.append(f"go away {code} sent")
        self.count(f"go away sent {code}")

    def protocol_error(self, why):
        self.log.append(f"protocol error: {why}")
        self.count("protocol errors")
        self.go_away(GO_PROTOCOL)
        self.closing = True

    def finish(self, sid):
        s = self.streams.get(sid)
        if s is not None and s.fin_sent and s.fin_received:
            del self.streams[sid]
            self.log.append(s.summary("closed"))

    def flush(self):
        while True:
            progress = False
            while self.control:
                self.send(self.control.popleft())
                progress = True
            for sid in sorted(self.streams):
                s = self.streams[sid]
                left = len(s.out) - s.sent
                frame = b""
                n = 0
                if left > 0:
                    if s.send_window == 0:
                        if not s.stalled:
                            s.stalled = True
                            s.stalls += 1
                        continue
                    n = min(left, self.chunk, s.send_window)
                    flags = 0
                    if s.syn_on_data:
                        flags |= SYN
                        s.syn_on_data = False
                    if n == left and s.fin_wanted and s.fin_on_data:
                        flags |= FIN
                        s.fin_sent = True
                    frame = header(TYPE_DATA, flags, sid, n) + bytes(s.out[s.sent:s.sent + n])
                    s.sent += n
                    s.send_window -= n
                    s.bytes_out += n
                    s.frames_out += 1
                elif s.fin_wanted and not s.fin_sent:
                    frame = header(TYPE_WINDOW, FIN, sid, 0)
                    s.fin_sent = True
                if frame:
                    self.send(frame)
                    self.bytes_out += n
                    progress = True
                    self.finish(sid)
            if not progress:
                break

    def next(self):
        head = read_full(self.conn, 12)
        if head is None:
            if not self.remote_go_away:
                self.log.append("peer closed without go away")
            return None
        self.frames_in += 1
        version, kind, flags, sid, length = parse_header(head)
        events = []
        if version != VERSION:
            self.protocol_error(f"version {version}")
            return events
        if kind in (TYPE_DATA, TYPE_WINDOW):
            self.stream_frame(kind, flags, sid, length, events)
        elif kind == TYPE_PING:
            if flags & SYN:
                self.control.append(header(TYPE_PING, ACK, 0, length))
                self.log.append(f"ping {length} answered")
                self.count("pings answered")
            else:
                events.append(("pong", length))
        elif kind == TYPE_GO_AWAY:
            self.remote_go_away = True
            self.count(f"go away received {length}")
            events.append(("go away", length))
        else:
            self.protocol_error(f"frame type {kind}")
        return events

    def stream_frame(self, kind, flags, sid, length, events):
        if flags & SYN:
            if sid in self.streams:
                return self.protocol_error(f"stream {sid} opened twice")
            if sid == 0 or sid % 2 != self.peer_parity:
                return self.protocol_error(f"stream {sid} has the wrong parity")
            extra = length if kind == TYPE_WINDOW else 0
            self.streams[sid] = Stream(sid, WINDOW + extra, WINDOW)
            self.control.append(header(TYPE_WINDOW, ACK, sid, 0))
            self.count("streams accepted")
            events.append(("opened", sid))
        if sid not in self.streams:
            if kind == TYPE_DATA and length > 0:
                read_full(self.conn, length)
            self.dropped += 1
            return
        s = self.streams[sid]
        if kind == TYPE_DATA:
            if length > s.recv_window:
                return self.protocol_error(
                    f"stream {sid} sent {length} bytes on a {s.recv_window}-byte window")
            body = read_full(self.conn, length) if length else b""
            if body is None:
                return
            s.recv_window -= length
            s.bytes_in += len(body)
            s.frames_in += 1
            self.bytes_in += len(body)
            if not flags & (FIN | RST):
                delta = s.recv_max - s.recv_window
                if delta >= s.recv_max // 2:
                    s.recv_window = s.recv_max
                    s.updates += 1
                    self.control.append(header(TYPE_WINDOW, 0, sid, delta))
            events.append(("data", sid, body))
        elif not flags & SYN and length > 0:
            s.send_window += length
            s.stalled = False
        if flags & RST:
            del self.streams[sid]
            self.log.append(s.summary("reset by peer"))
            self.count("resets received")
            events.append(("reset", sid))
            return
        if flags & FIN:
            s.fin_received = True
            events.append(("fin", sid))
            self.finish(sid)

    def wrap_up(self):
        for sid in sorted(self.streams):
            self.log.append(self.streams[sid].summary("open at the end"))
        self.log.append(f"frames in {self.frames_in} out {self.frames_out}, bytes in "
                        f"{self.bytes_in} out {self.bytes_out}, dropped {self.dropped}")
        self.count("frames in", self.frames_in)
        self.count("frames out", self.frames_out)
        self.count("bytes in", self.bytes_in)
        self.count("bytes out", self.bytes_out)
        self.count("dropped frames", self.dropped)


def run(sess, app):
    while True:
        sess.flush()
        if sess.closing:
            break
        if app.step(sess):
            continue
        events = sess.next()
        if events is None:
            break
        for ev in events:
            app.on_event(sess, ev)


class Shared:
    def __init__(self):
        self.lock = threading.Lock()
        self.counts = {}
        self.logs = {}
        self.unnamed = 0


class Server:
    def __init__(self, shared):
        self.name = None
        self.services = {}
        self.shared = shared

    def on_event(self, sess, ev):
        what = ev[0]
        if what == "opened":
            self.services[ev[1]] = ["request", bytearray()]
        elif what == "data":
            self.data(sess, ev[1], ev[2])
        elif what == "fin":
            self.fin(sess, ev[1])
        elif what == "reset":
            self.services.pop(ev[1], None)
        elif what == "pong":
            sess.log.append(f"pong {ev[1]}")
        elif what == "go away":
            sess.log.append(f"go away {ev[1]} received")

    def step(self, sess):
        if sess.remote_go_away and not sess.local_go_away and not sess.streams:
            sess.go_away(GO_NORMAL)
            sess.closing = True
            return True
        return False

    def data(self, sess, sid, data):
        svc = self.services.get(sid)
        if svc is None:
            return
        if svc[0] == "request":
            svc[1] += data
            at = svc[1].find(b"\n")
            if at >= 0:
                line = bytes(svc[1][:at]).decode("utf-8", "replace")
                rest = bytes(svc[1][at + 1:])
                self.services[sid] = self.dispatch(sess, sid, line, rest)
        elif svc[0] == "echo":
            sess.write(sid, data)
        elif svc[0] == "put":
            svc[1] += len(data)
            svc[2] = fnv1a(svc[2], data)

    def dispatch(self, sess, sid, line, rest):
        words = line.split(" ")
        verb = words[0]
        arg = words[1] if len(words) > 1 else ""
        sess.set_label(sid, line)
        if verb == "hello":
            self.name = arg
            sess.count("service hello")
            sess.write(sid, f"welcome {arg}\n".encode())
            sess.close(sid)
            sess.ping(1)
            return ["done"]
        if verb == "echo":
            sess.count("service echo")
            sess.write(sid, rest)
            return ["echo"]
        if verb == "cat":
            sess.count("service cat")
            n = int(arg) if arg.isdigit() else 0
            sess.write(sid, cat_bytes(n))
            sess.close(sid)
            return ["done"]
        if verb == "put":
            sess.count("service put")
            return ["put", len(rest), fnv1a(FNV_START, rest)]
        if verb == "push":
            sess.count("service push")
            n = int(arg) if arg.isdigit() else 0
            p = sess.open("push", False, 0)
            sess.write(p, "".join(f"note {k}\n" for k in range(1, n + 1)).encode())
            sess.close(p)
            sess.write(sid, f"pushed {n} notes on stream {p}\n".encode())
            sess.close(sid)
            return ["done"]
        if verb == "stats":
            sess.count("service stats")
            with self.shared.lock:
                text = "".join(f"{k} {v}\n" for k, v in sorted(self.shared.counts.items()))
            sess.write(sid, text.encode())
            sess.close(sid)
            return ["done"]
        sess.count("service unknown")
        sess.log.append(f"stream {sid}: unknown service '{line}'")
        sess.reset(sid)
        return ["done"]

    def fin(self, sess, sid):
        svc = self.services.get(sid)
        if svc is None:
            return
        if svc[0] == "echo":
            sess.close(sid)
        elif svc[0] == "put":
            sess.write(sid, f"stored {svc[1]} bytes, fnv {svc[2]:08x}\n".encode())
            sess.close(sid)
        elif svc[0] == "request":
            sess.log.append(f"stream {sid}: closed before its request")
            sess.reset(sid)


def serve(conn, shared):
    sess = Session(conn, True)
    app = Server(shared)
    run(sess, app)
    sess.wrap_up()
    sess.count("sessions")
    with shared.lock:
        for k, v in sess.counts.items():
            shared.counts[k] = shared.counts.get(k, 0) + v
        if app.name is not None:
            name = app.name
        else:
            shared.unnamed += 1
            name = f"unnamed-{shared.unnamed}"
        shared.logs[name] = list(sess.log)
    conn.close()


def serve_all(listener, shared):
    tasks = []
    for _ in range(SESSIONS):
        conn, _ = listener.accept()
        t = threading.Thread(target=serve, args=(conn, shared))
        t.start()
        tasks.append(t)
    for t in tasks:
        t.join()


class Job:
    def __init__(self, request):
        self.request = request
        self.upload = b""
        self.by_data = True
        self.extra = 0
        self.fin_on_data = False
        self.reset_after = 0
        self.show = True
        self.echo = False


class Pending:
    def __init__(self, job):
        self.job = job
        self.got = 0
        self.hash = FNV_START
        self.text = bytearray()


class Client:
    def __init__(self, phases, stray):
        self.phases = phases
        self.phase = 0
        self.stray = stray
        self.pending = {}

    def report(self, sess, sid):
        p = self.pending.pop(sid, None)
        if p is None:
            return
        if p.job.show:
            line = " | ".join(bytes(p.text).decode("utf-8", "replace").splitlines())
        else:
            line = f"{p.got} bytes, fnv {p.hash:08x}"
            if p.job.echo:
                same = p.hash == fnv1a(FNV_START, p.job.upload) and p.got == len(p.job.upload)
                line += ", same as sent" if same else ", NOT what was sent"
        sess.log.append(f"stream {sid} {p.job.request}: {line}")

    def on_event(self, sess, ev):
        what = ev[0]
        if what == "opened":
            j = Job("from server")
            j.by_data = False
            sess.set_label(ev[1], "from server")
            self.pending[ev[1]] = Pending(j)
        elif what == "data":
            sid, data = ev[1], ev[2]
            p = self.pending.get(sid)
            if p is not None:
                p.got += len(data)
                p.hash = fnv1a(p.hash, data)
                if p.job.show:
                    p.text += data
                if p.job.reset_after > 0 and p.got >= p.job.reset_after:
                    del self.pending[sid]
                    sess.log.append(f"stream {sid} {p.job.request}: reset after {p.got} bytes")
                    sess.reset(sid)
        elif what == "fin":
            sess.close(ev[1])
            self.report(sess, ev[1])
        elif what == "reset":
            p = self.pending.pop(ev[1], None)
            if p is not None:
                sess.log.append(f"stream {ev[1]} {p.job.request}: reset by the server")
        elif what == "pong":
            sess.log.append(f"pong {ev[1]:x}")
        elif what == "go away":
            sess.log.append(f"go away {ev[1]} received")

    def step(self, sess):
        if sess.streams:
            return False
        if self.phase < len(self.phases):
            if self.phase == 0:
                sess.ping(0xC0FFEE)
            jobs = self.phases[self.phase]
            self.phase += 1
            if self.stray and self.phase == 2:
                sess.send(header(TYPE_DATA, 0, 99, 5) + b"stray")
            for j in jobs:
                sid = sess.open(j.request, j.by_data, j.extra)
                sess.write(sid, f"{j.request}\n".encode() + j.upload)
                if j.fin_on_data:
                    sess.streams[sid].fin_on_data = True
                sess.close(sid)
                self.pending[sid] = Pending(j)
            return True
        if not sess.local_go_away:
            sess.go_away(GO_NORMAL)
            return True
        return False


def client(addr, phases, stray=False):
    conn = socket.create_connection(addr)
    sess = Session(conn, False)
    run(sess, Client(phases, stray))
    sess.wrap_up()
    conn.close()
    return sess.log


def job(request, **kw):
    j = Job(request)
    for k, v in kw.items():
        setattr(j, k, v)
    return j


def alice(addr):
    echo = job("echo", by_data=False, upload=b"first line\nsecond line\nthird\n")
    return client(addr, [[job("hello alice")], [echo, job("push 3"), job("nope")]])


def bob(addr):
    cat = job("cat 700000", by_data=False, show=False)
    wide = job("cat 300000", by_data=False, extra=WINDOW, show=False)
    put = job("put", upload=put_bytes(600000), fin_on_data=True)
    return client(addr, [[job("hello bob")], [cat, wide], [put]])


def carol(addr):
    echo = job("echo", upload=put_bytes(40000), show=False, echo=True)
    cut = job("cat 500000", show=False, reset_after=16384)
    put = job("put", by_data=False, upload=put_bytes(25000))
    return client(addr, [[job("hello carol")], [echo, cut], [put]], stray=True)


def admin(addr):
    return client(addr, [[job("hello admin")], [job("stats")]])


def raw(addr, label, frames, half_close=False):
    conn = socket.create_connection(addr)
    conn.settimeout(WAIT_S)
    for f in frames:
        conn.sendall(f)
    if half_close:
        conn.shutdown(socket.SHUT_WR)
    print(label)
    while True:
        head = read_full(conn, 12)
        if head is None:
            break
        _, kind, flags, sid, length = parse_header(head)
        print(f"  got {kind_name(kind)} stream {sid} flags {flag_names(flags)} length {length}")
        if kind == TYPE_DATA and length > 0:
            read_full(conn, length)
    print("  closed")
    conn.close()


def main():
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(16)
    addr = listener.getsockname()
    shared = Shared()
    server = threading.Thread(target=serve_all, args=(listener, shared))
    server.start()

    print("== three sessions at once")
    results = {}

    def go(name, fn):
        results[name] = fn(addr)

    threads = [threading.Thread(target=go, args=(n, f)) for n, f in
               (("alice", alice), ("bob", bob), ("carol", carol))]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    for name in ("alice", "bob", "carol"):
        print(f"-- {name}")
        for line in results[name]:
            print(line)

    print("== protocol errors")
    raw(addr, "version 1", [bytes([1, TYPE_WINDOW, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0])])
    raw(addr, "300000 bytes announced on a fresh window",
        [header(TYPE_WINDOW, SYN, 1, 0) + header(TYPE_DATA, 0, 1, 300000)])
    raw(addr, "a client stream with an even id", [header(TYPE_WINDOW, SYN, 2, 0)])
    raw(addr, "stream 1 opened twice", [header(TYPE_WINDOW, SYN, 1, 0), header(TYPE_DATA, SYN, 1, 0)])
    raw(addr, "frame type 7", [header(7, 0, 0, 0)])
    raw(addr, "a ping, then a half close", [header(TYPE_PING, SYN, 0, 42)], half_close=True)

    print("== admin")
    for line in admin(addr):
        print(line)

    server.join()
    listener.close()
    print("== server logs")
    for name in sorted(shared.logs):
        print(f"-- {name}")
        for line in shared.logs[name]:
            print(line)
    print("== totals")
    for k in sorted(shared.counts):
        print(f"{k} {shared.counts[k]}")


if __name__ == "__main__":
    main()
