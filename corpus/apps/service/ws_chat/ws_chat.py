"""Mirror of ws_chat.kara: a WebSocket chat server over a loopback socket.

hashlib and base64 stand in for the Kara program's own SHA-1 and base64;
the framing, room and round choreography are the same.
"""
import base64
import hashlib
import socket

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
MAX_MESSAGE = 200
PORT = 47742


def accept_key(key):
    return base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()


def encode(fin, opcode, payload, mask=None):
    out = bytearray([(0x80 if fin else 0) | opcode])
    mbit = 0x80 if mask is not None else 0
    n = len(payload)
    if n < 126:
        out.append(mbit | n)
    else:
        out.append(mbit | 126)
        out += bytes([n >> 8, n & 0xff])
    if mask is not None:
        out += bytes(mask)
        out += bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    else:
        out += payload
    return bytes(out)


class Frame:
    def __init__(self, fin, opcode, masked, payload):
        self.fin, self.opcode, self.masked, self.payload = fin, opcode, masked, payload


def decode_frame(buf, at):
    if at + 2 > len(buf):
        return None
    b0, b1 = buf[at], buf[at + 1]
    masked = (b1 & 0x80) != 0
    n = b1 & 0x7f
    pos = at + 2
    if n == 126:
        if pos + 2 > len(buf):
            return None
        n = (buf[pos] << 8) | buf[pos + 1]
        pos += 2
    elif n == 127:
        if pos + 8 > len(buf):
            return None
        n = int.from_bytes(buf[pos:pos + 8], "big")
        pos += 8
    key = b""
    if masked:
        if pos + 4 > len(buf):
            return None
        key = buf[pos:pos + 4]
        pos += 4
    if pos + n > len(buf):
        return None
    payload = bytes((b ^ key[i % 4]) if masked else b for i, b in enumerate(buf[pos:pos + n]))
    return Frame((b0 & 0x80) != 0, b0 & 0x0f, masked, payload), pos + n


def close_payload(code, reason):
    return bytes([code >> 8, code & 0xff]) + reason.encode()


def read_all(s):
    out = b""
    while True:
        b = s.recv(64)
        if not b:
            return out
        out += b


def utf8(b):
    try:
        return b.decode("utf-8")
    except UnicodeDecodeError:
        return None


def handshake(head):
    lines = head.split("\r\n")
    first = lines[0].split(" ")
    key, upgrade, version = "", False, ""
    for line in lines[1:]:
        c = line.find(":")
        if c < 0:
            continue
        name = line[:c].strip().lower()
        value = line[c + 1:].strip()
        if name == "sec-websocket-key":
            key = value
        elif name == "upgrade":
            upgrade = value.lower() == "websocket"
        elif name == "sec-websocket-version":
            version = value
    if len(first) != 3 or first[0] != "GET" or first[1] != "/chat":
        return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n", False
    if not upgrade or not key:
        return "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n", False
    if version != "13":
        return "HTTP/1.1 426 Upgrade Required\r\nSec-WebSocket-Version: 13\r\nContent-Length: 0\r\n\r\n", False
    return (f"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept_key(key)}\r\n\r\n"), True


class Conn:
    def __init__(self, stream, nick):
        self.stream, self.nick = stream, nick
        self.out = bytearray()
        self.open = False
        self.partial = bytearray()
        self.partial_op = 0


class Room:
    def __init__(self):
        self.conns = []
        self.history = []


def send_text(room, to, text):
    if room.conns[to].open:
        room.conns[to].out += encode(True, 1, text.encode())


def broadcast(room, text):
    room.history.append(text)
    for i in range(len(room.conns)):
        send_text(room, i, text)


def close_conn(room, i, code, reason):
    c = room.conns[i]
    if not c.open:
        return
    c.out += encode(True, 8, close_payload(code, reason))
    c.open = False
    print(f"  server: closed {c.nick} with {code} ({reason})")


def on_message(room, i, text):
    if text.startswith("/nick "):
        new = text[6:].strip()
        taken = any(c.open and c.nick == new for c in room.conns)
        if taken or not new:
            send_text(room, i, f"* nick {new} is taken")
        else:
            old = room.conns[i].nick
            room.conns[i].nick = new
            broadcast(room, f"* {old} is now {new}")
    elif text == "/who":
        names = sorted(c.nick for c in room.conns if c.open)
        send_text(room, i, f"* here: {', '.join(names)}")
    elif text == "/history":
        for line in room.history[-3:]:
            send_text(room, i, f"| {line}")
    else:
        broadcast(room, f"<{room.conns[i].nick}> {text}")


def on_frames(room, i, buf, at):
    c = room.conns[i]
    while c.open:
        r = decode_frame(buf, at)
        if r is None:
            if at < len(buf):
                close_conn(room, i, 1002, "truncated frame")
            return
        frame, at = r
        if not frame.masked:
            close_conn(room, i, 1002, "client frames must be masked")
            return
        op = frame.opcode
        if op == 9:
            c.out += encode(True, 10, frame.payload)
        elif op == 10:
            pass
        elif op == 8:
            code = (frame.payload[0] << 8) | frame.payload[1] if len(frame.payload) >= 2 else 1005
            c.out += encode(True, 8, close_payload(1000, "bye"))
            c.open = False
            print(f"  server: {c.nick} closed with {code}")
            broadcast(room, f"* {c.nick} left")
            return
        elif op in (0, 1, 2):
            if op == 0 and c.partial_op == 0:
                close_conn(room, i, 1002, "continuation with nothing to continue")
                return
            if op != 0 and c.partial_op != 0:
                close_conn(room, i, 1002, "new message inside a fragmented one")
                return
            if op != 0:
                c.partial_op = op
            c.partial += frame.payload
            if len(c.partial) > MAX_MESSAGE:
                close_conn(room, i, 1009, "message too big")
                return
            if frame.fin:
                whole, kind = bytes(c.partial), c.partial_op
                c.partial, c.partial_op = bytearray(), 0
                if kind == 2:
                    send_text(room, i, f"* binary message of {len(whole)} bytes ignored")
                else:
                    t = utf8(whole)
                    if t is None:
                        close_conn(room, i, 1007, "text is not UTF-8")
                        return
                    on_message(room, i, t)
        else:
            close_conn(room, i, 1002, f"unknown opcode {op}")
            return


class Peer:
    def __init__(self, name, key):
        self.name = name
        self.wire = bytearray(
            (f"GET /chat HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
             f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        self.stream = None


def mask_for(name, n):
    b = name.encode()
    return [b[0], n * 37 % 256, 0x5a, (len(b) * 11) % 256]


def text(c, msg):
    c.wire += encode(True, 1, msg.encode(), mask_for(c.name, len(c.wire)))


def frame(c, fin, op, payload):
    c.wire += encode(fin, op, payload, mask_for(c.name, len(c.wire)))


def describe(f):
    if f.opcode == 1:
        t = utf8(f.payload)
        return "?" if t is None else t
    if f.opcode == 8:
        code = (f.payload[0] << 8) | f.payload[1] if len(f.payload) >= 2 else 0
        return f"[close {code} {utf8(f.payload[2:]) or ''}]"
    if f.opcode == 10:
        return f"[pong {utf8(f.payload) or ''}]"
    return f"[opcode {f.opcode}]"


def round_(listener, room, clients, label):
    print(f"== {label}")
    for c in clients:
        s = socket.create_connection(("127.0.0.1", PORT))
        s.sendall(bytes(c.wire))
        s.shutdown(socket.SHUT_WR)
        c.stream = s
    inputs, starts = [], []
    room.conns = []
    for c in clients:
        s, _ = listener.accept()
        inputs.append(read_all(s))
        room.conns.append(Conn(s, c.name))
        starts.append(0)
    for i, conn in enumerate(room.conns):
        head_end = inputs[i].find(b"\r\n\r\n")
        if head_end < 0:
            conn.out += b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n"
            continue
        reply, ok = handshake(utf8(inputs[i][:head_end]) or "")
        conn.out += reply.encode()
        starts[i] = head_end + 4
        if ok:
            conn.open = True
            broadcast(room, f"* {conn.nick} joined")
    for i, conn in enumerate(room.conns):
        if conn.open:
            on_frames(room, i, inputs[i], starts[i])
    for i, conn in enumerate(room.conns):
        if conn.open:
            close_conn(room, i, 1001, "server going away")
        conn.stream.sendall(bytes(conn.out))
        conn.stream.close()
    room.conns = []
    for c in clients:
        data = read_all(c.stream)
        c.stream.close()
        c.stream = None
        head_end = data.find(b"\r\n\r\n")
        if head_end < 0:
            print(f"  {c.name}: no reply")
            continue
        head = utf8(data[:head_end]) or ""
        status = head.splitlines()[0].split(" ")
        accept = "-"
        for l in head.splitlines():
            if l.lower().startswith("sec-websocket-accept:"):
                accept = l[21:].strip()
        print(f"  {c.name}: {status[1]} accept={accept}")
        at = head_end + 4
        while True:
            r = decode_frame(data, at)
            if r is None:
                break
            f, at = r
            print(f"    {describe(f)}")
        c.wire = bytearray()


def main():
    print(f"RFC 6455 sample key: {accept_key('dGhlIHNhbXBsZSBub25jZQ==')}")
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", PORT))
    listener.listen(16)
    room = Room()

    a = Peer("ada", "x3JJHMbDL1EzLkh9GBhXDw==")
    text(a, "hello room")
    text(a, "/nick countess")
    b = Peer("bob", "AQIDBAUGBwgJCgsMDQ4PEA==")
    frame(b, False, 1, b"frag")
    frame(b, True, 9, b"ping-1")
    frame(b, False, 0, b"mented ")
    frame(b, True, 0, b"hello")
    text(b, "/who")
    cy = Peer("cy", "dGhlIHNhbXBsZSBub25jZQ==")
    text(cy, "/nick bob")
    text(cy, "word " * 30)
    text(cy, "/history")
    frame(cy, True, 8, close_payload(1000, "done"))
    round_(listener, room, [a, b, cy], "three clients chat")

    d = Peer("dee", "ZGVlZGVlZGVlZGVlZGVlZA==")
    d.wire += encode(True, 1, b"no mask")
    e = Peer("eve", "ZXZlZXZlZXZlZXZlZXZlZQ==")
    frame(e, True, 1, bytes([0xff, 0xfe, 0x41]))
    f = Peer("fay", "ZmF5ZmF5ZmF5ZmF5ZmF5Zg==")
    frame(f, False, 1, b"x" * 120)
    frame(f, True, 0, b"x" * 120)
    g = Peer("gus", "Z3VzZ3VzZ3VzZ3VzZ3VzZw==")
    frame(g, True, 3, b"?")
    h = Peer("hal", "aGFsaGFsaGFsaGFsaGFsaA==")
    frame(h, True, 0, b"orphan")
    round_(listener, room, [d, e, f, g, h], "protocol errors")

    old = Peer("ivy", "aXZ5aXZ5aXZ5aXZ5aXZ5aQ==")
    old.wire = bytearray(b"GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                         b"Sec-WebSocket-Key: aXZ5aXZ5aXZ5aXZ5aXZ5aQ==\r\nSec-WebSocket-Version: 8\r\n\r\n")
    plain = Peer("joe", "-")
    plain.wire = bytearray(b"GET /chat HTTP/1.1\r\nHost: x\r\n\r\n")
    lost = Peer("kim", "-")
    lost.wire = bytearray(b"GET /other HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n"
                          b"Sec-WebSocket-Key: a2ltCg==\r\nSec-WebSocket-Version: 13\r\n\r\n")
    ok = Peer("lee", "bGVlbGVlbGVlbGVlbGVlbA==")
    text(ok, "/history")
    round_(listener, room, [old, plain, lost, ok], "handshake failures")
    print(f"history: {len(room.history)} lines")


main()
