"""Oracle for mqtt_broker.kara: an MQTT 3.1.1 broker subset on loopback,
one thread per connection, the shared state under one lock, and scripted
clients on the main thread."""

import socket
import threading

ADDR = ("127.0.0.1", 47764)
CONNECTIONS = 6

NAMES = {
    1: "CONNECT", 2: "CONNACK", 3: "PUBLISH", 4: "PUBACK", 8: "SUBSCRIBE",
    9: "SUBACK", 10: "UNSUBSCRIBE", 11: "UNSUBACK", 12: "PINGREQ",
    13: "PINGRESP", 14: "DISCONNECT",
}

# ------------------------------------------------------------ wire format


def encode_len(n):
    out = bytearray()
    while True:
        b = n % 128
        n //= 128
        if n > 0:
            b |= 0x80
        out.append(b)
        if n == 0:
            return bytes(out)


def packet(first, body):
    return bytes([first]) + encode_len(len(body)) + body


def mqtt_str(s):
    b = s.encode()
    return len(b).to_bytes(2, "big") + b


def read_exact(conn, n):
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def read_packet(conn):
    """(first byte, body), or None at the end of the stream."""
    head = read_exact(conn, 1)
    if head is None:
        return None
    n, shift = 0, 0
    for _ in range(4):
        b = read_exact(conn, 1)
        if b is None:
            return None
        n |= (b[0] & 0x7F) << shift
        shift += 7
        if b[0] & 0x80 == 0:
            break
    body = read_exact(conn, n) if n > 0 else b""
    if body is None:
        return None
    return head[0], body


class Cursor:
    def __init__(self, body):
        self.body, self.at = body, 0

    def u8(self):
        if self.at >= len(self.body):
            return None
        v = self.body[self.at]
        self.at += 1
        return v

    def u16(self):
        if self.at + 2 > len(self.body):
            return None
        v = int.from_bytes(self.body[self.at:self.at + 2], "big")
        self.at += 2
        return v

    def text(self):
        n = self.u16()
        if n is None or self.at + n > len(self.body):
            return None
        s = self.body[self.at:self.at + n].decode("utf-8", "replace")
        self.at += n
        return s

    def rest(self):
        r = self.body[self.at:]
        self.at = len(self.body)
        return r

    def done(self):
        return self.at >= len(self.body)


# ------------------------------------------------------------ topics


def valid_filter(f):
    if f == "":
        return False
    levels = f.split("/")
    for i, lv in enumerate(levels):
        if "#" in lv and (lv != "#" or i != len(levels) - 1):
            return False
        if "+" in lv and lv != "+":
            return False
    return True


def valid_topic(t):
    return t != "" and "+" not in t and "#" not in t


def matches(f, t):
    fl, tl = f.split("/"), t.split("/")
    for i, lv in enumerate(fl):
        if lv == "#":
            return True
        if i >= len(tl):
            return False
        if lv != "+" and lv != tl[i]:
            return False
    return len(fl) == len(tl)


# ------------------------------------------------------------ the broker


class Session:
    def __init__(self, out):
        self.out = out
        self.client_id = None
        self.subs = []  # [filter, qos], in subscription order
        self.next_id = 1
        self.will = None  # (topic, payload, qos, retain)
        self.packets = 0


class Broker:
    def __init__(self):
        self.lock = threading.Lock()
        self.sessions = {}
        self.retained = {}  # topic -> (payload, qos)
        self.counts = {}
        self.log = {}


def send(s, data):
    try:
        s.out.sendall(data)
    except OSError:
        pass


def publish_packet(topic, payload, qos, retain, pid):
    first = 0x30 | (qos << 1) | (1 if retain else 0)
    body = mqtt_str(topic)
    if qos > 0:
        body += pid.to_bytes(2, "big")
    return packet(first, body + payload)


def deliver(b, s, topic, payload, qos, retain):
    pid = 0
    if qos > 0:
        pid = s.next_id
        s.next_id += 1
    send(s, publish_packet(topic, payload, qos, retain, pid))


def route(b, topic, payload, qos, retain):
    """Hands a message to every subscriber, in connection order, once each at
    the highest QoS among its matching filters; a retained one is kept."""
    if retain:
        if payload:
            b.retained[topic] = (payload, qos)
        else:
            b.retained.pop(topic, None)
    for cid in sorted(b.sessions):
        s = b.sessions[cid]
        best = -1
        for f, q in s.subs:
            if matches(f, topic):
                best = max(best, q)
        if best >= 0:
            deliver(b, s, topic, payload, min(qos, best), False)


def handle(b, cid, first, body):
    """Returns how the connection ends, or None to keep reading."""
    s = b.sessions[cid]
    kind = first >> 4
    name = NAMES.get(kind, f"type {kind}")
    b.counts[name] = b.counts.get(name, 0) + 1
    s.packets += 1
    c = Cursor(body)
    if s.client_id is None and kind != 1:
        return "protocol error: no CONNECT"
    if kind == 1:
        if s.client_id is not None:
            return "protocol error: second CONNECT"
        proto, level, flags, _keep = c.text(), c.u8(), c.u8(), c.u16()
        if proto != "MQTT" or level != 4:
            send(s, packet(0x20, bytes([0, 1])))
            return "refused: protocol level"
        client = c.text()
        will = None
        if flags & 0x04:
            wt, wp = c.text(), c.u16()
            payload = c.body[c.at:c.at + wp]
            c.at += wp
            will = (wt, payload, (flags >> 3) & 3, bool(flags & 0x20))
        clean = bool(flags & 0x02)
        if client == "" and not clean:
            send(s, packet(0x20, bytes([0, 2])))
            return "refused: identifier"
        if client == "":
            client = f"auto-{cid}"
        s.client_id = client
        s.will = will
        send(s, packet(0x20, bytes([0, 0])))
        return None
    if kind == 3:
        qos, retain = (first >> 1) & 3, bool(first & 1)
        topic = c.text()
        pid = c.u16() if qos > 0 else 0
        payload = c.rest()
        if topic is None or not valid_topic(topic):
            return f"protocol error: topic {topic}"
        route(b, topic, payload, min(qos, 1), retain)
        if qos == 1:
            send(s, packet(0x40, pid.to_bytes(2, "big")))
        return None
    if kind == 4:
        c.u16()
        return None
    if kind == 8:
        pid = c.u16()
        granted, added = [], []
        while not c.done():
            f, q = c.text(), c.u8()
            if f is None or q is None or not valid_filter(f):
                granted.append(0x80)
                continue
            q = min(q, 1)
            s.subs = [x for x in s.subs if x[0] != f] + [[f, q]]
            granted.append(q)
            added.append((f, q))
        send(s, packet(0x90, pid.to_bytes(2, "big") + bytes(granted)))
        for topic in sorted(b.retained):
            best = -1
            for f, q in added:
                if matches(f, topic):
                    best = max(best, q)
            if best >= 0:
                payload, mq = b.retained[topic]
                deliver(b, s, topic, payload, min(mq, best), True)
        return None
    if kind == 10:
        pid = c.u16()
        while not c.done():
            f = c.text()
            s.subs = [x for x in s.subs if x[0] != f]
        send(s, packet(0xB0, pid.to_bytes(2, "big")))
        return None
    if kind == 12:
        send(s, packet(0xD0, b""))
        return None
    if kind == 14:
        s.will = None
        return "disconnect"
    return f"protocol error: {name}"


def serve(b, cid, conn):
    how = None
    while how is None:
        try:
            p = read_packet(conn)
        except OSError:
            p = None
        if p is None:
            how = "dropped"
            break
        with b.lock:
            how = handle(b, cid, p[0], p[1])
    with b.lock:
        s = b.sessions.pop(cid)
        if s.will is not None and how != "disconnect":
            wt, wp, wq, wr = s.will
            route(b, wt, wp, min(wq, 1), wr)
        b.log[cid] = (s.client_id or "-", how, s.packets)
    try:
        conn.shutdown(socket.SHUT_WR)
    except OSError:
        pass
    conn.close()


def run_server(b, listener):
    threads = []
    for cid in range(1, CONNECTIONS + 1):
        conn, _ = listener.accept()
        with b.lock:
            b.sessions[cid] = Session(conn)
        t = threading.Thread(target=serve, args=(b, cid, conn))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


# ------------------------------------------------------------ the clients


class Client:
    def __init__(self, name):
        self.name = name
        self.sock = socket.create_connection(ADDR)
        self.sock.settimeout(2.0)

    def send(self, data):
        self.sock.sendall(data)

    def connect(self, client_id, clean=True, level=4, will=None):
        flags = 0x02 if clean else 0
        tail = b""
        if will is not None:
            wt, wp, wq, wr = will
            flags |= 0x04 | (wq << 3) | (0x20 if wr else 0)
            tail = mqtt_str(wt) + len(wp).to_bytes(2, "big") + wp
        body = mqtt_str("MQTT") + bytes([level, flags]) + (60).to_bytes(2, "big")
        self.send(packet(0x10, body + mqtt_str(client_id) + tail))

    def publish(self, topic, payload, qos=0, retain=False, pid=0):
        self.send(publish_packet(topic, payload, qos, retain, pid))

    def subscribe(self, pid, filters):
        body = pid.to_bytes(2, "big")
        for f, q in filters:
            body += mqtt_str(f) + bytes([q])
        self.send(packet(0x82, body))

    def unsubscribe(self, pid, filters):
        body = pid.to_bytes(2, "big")
        for f in filters:
            body += mqtt_str(f)
        self.send(packet(0xA2, body))

    def puback(self, pid):
        self.send(packet(0x40, pid.to_bytes(2, "big")))

    def expect(self, n):
        for _ in range(n):
            try:
                p = read_packet(self.sock)
            except OSError as e:
                print(f"{self.name} <- error {e}")
                return
            if p is None:
                print(f"{self.name} <- (closed)")
                return
            print(f"{self.name} <- {show(p[0], p[1])}")

    def expect_close(self):
        try:
            p = read_packet(self.sock)
        except OSError as e:
            print(f"{self.name} <- error {e}")
            return
        if p is None:
            print(f"{self.name} <- (closed)")
        else:
            print(f"{self.name} <- unexpected {show(p[0], p[1])}")

    def close(self):
        self.sock.close()


def show(first, body):
    kind = first >> 4
    name = NAMES.get(kind, f"type {kind}")
    c = Cursor(body)
    if kind == 2:
        return f"{name} sp={c.u8()} rc={c.u8()}"
    if kind == 3:
        qos, retain = (first >> 1) & 3, first & 1
        topic = c.text()
        pid = f" id={c.u16()}" if qos > 0 else ""
        payload = c.rest()
        if len(payload) > 40:
            text = f"<{len(payload)} bytes>"
        else:
            text = repr(payload.decode())
        return f"{name} {topic} q{qos} r{retain}{pid} {text}"
    if kind in (4, 11):
        return f"{name} id={c.u16()}"
    if kind == 9:
        pid = c.u16()
        codes = " ".join(f"{b:02x}" for b in c.rest())
        return f"{name} id={pid} [{codes}]"
    return name


def self_test():
    print("== lengths")
    for n in [0, 127, 128, 16383, 16384, 2097151, 268435455]:
        print(f"{n} -> {' '.join(f'{b:02x}' for b in encode_len(n))}")
    print("== filters")
    for f in ["a/b", "a/+/c", "#", "a/#", "a/#/b", "a+", "+", "", "/x"]:
        print(f"\"{f}\" {'ok' if valid_filter(f) else 'bad'}")
    print("== matches")
    for f, t in [("a/+/c", "a/b/c"), ("a/+/c", "a/b/d"), ("a/#", "a"),
                 ("a/#", "a/b/c"), ("+/+", "a/b"), ("+", "a/b"),
                 ("#", "x"), ("a/b", "a/b"), ("+/b", "/b")]:
        print(f"{f} {t} {'yes' if matches(f, t) else 'no'}")


def main():
    self_test()
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR)
    listener.listen(16)
    b = Broker()
    server = threading.Thread(target=run_server, args=(b, listener))
    server.start()

    print("== live")
    old = Client("old")
    old.connect("old", level=3)
    old.expect(1)
    old.expect_close()
    old.close()

    anon = Client("anon")
    anon.connect("", clean=False)
    anon.expect(1)
    anon.expect_close()
    anon.close()

    sensor = Client("sensor")
    sensor.connect("sensor")
    sensor.expect(1)
    sensor.publish("home/kitchen/temp", b"21.5", retain=True)
    sensor.publish("home/hall/light", b"on", qos=1, retain=True, pid=7)
    sensor.expect(1)

    dash = Client("dash")
    dash.connect("dash")
    dash.expect(1)
    dash.subscribe(1, [("home/+/temp", 1), ("home/#", 0), ("bad/#/x", 0)])
    dash.expect(3)

    logger = Client("logger")
    logger.connect("")
    logger.expect(1)
    logger.subscribe(1, [("#", 1)])
    logger.expect(3)
    logger.puback(1)

    sensor.publish("home/kitchen/temp", b"22.0", qos=1, pid=8)
    sensor.expect(1)
    dash.expect(1)
    dash.puback(1)
    logger.expect(1)
    logger.puback(2)

    sensor.publish("home/log", b"x" * 200)
    dash.expect(1)
    logger.expect(1)

    dash.unsubscribe(2, ["home/+/temp"])
    dash.expect(1)
    sensor.publish("home/kitchen/temp", b"22.5")
    dash.expect(1)
    logger.expect(1)
    dash.send(packet(0xC0, b""))
    dash.expect(1)

    probe = Client("probe")
    probe.connect("probe", will=("status/probe", b"lost", 1, True))
    probe.expect(1)
    probe.close()
    logger.expect(1)
    logger.puback(3)

    sensor.publish("home/+/temp", b"?")
    sensor.expect_close()
    sensor.close()

    dash.send(packet(0xE0, b""))
    dash.expect_close()
    dash.close()
    logger.send(packet(0xE0, b""))
    logger.expect_close()
    logger.close()

    server.join()
    listener.close()
    print("== broker")
    for cid in sorted(b.log):
        client, how, n = b.log[cid]
        print(f"conn {cid} {client}: {how} after {n} packets")
    for name in sorted(b.counts):
        print(f"{name} {b.counts[name]}")
    for topic in sorted(b.retained):
        payload, qos = b.retained[topic]
        print(f"retained {topic} q{qos} {payload.decode()!r}")


main()
