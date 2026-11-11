"""Oracle for tftp_server.kara: a TFTP server (RFC 1350) on loopback UDP,
each transfer in a thread with a socket of its own, and scripted clients on
the main thread."""

import socket
import threading

ADDR = ("127.0.0.1", 47765)
REQUESTS = 10
BLOCK = 512
RESEND_MS = 500
WAIT_MS = 2000
TRIES = 3

RRQ, WRQ, DATA, ACK, ERROR = 1, 2, 3, 4, 5


def u16(n):
    return n.to_bytes(2, "big")


def error_packet(code, msg):
    return u16(ERROR) + u16(code) + msg.encode() + b"\0"


def data_packet(block, chunk):
    return u16(DATA) + u16(block) + chunk


def ack_packet(block):
    return u16(ACK) + u16(block)


def to_netascii(data):
    return data.replace(b"\n", b"\r\n")


def from_netascii(data):
    return data.replace(b"\r\n", b"\n")


def checksum(data):
    total = 0
    for i, b in enumerate(data):
        total = (total * 31 + b + i) % 1000003
    return total


def parse_request(pkt):
    """(filename, mode) of a request, or None when it is malformed."""
    parts = pkt[2:].split(b"\0")
    if len(parts) < 3 or parts[0] == b"" or parts[1] == b"":
        return None
    return parts[0].decode(), parts[1].decode().lower()


class Server:
    def __init__(self):
        self.lock = threading.Lock()
        self.files = {}
        self.log = {}


def recv_ack(sock, peer, block):
    """Waits for the ACK of `block` from `peer`: True, or False on a timeout.
    A datagram from anyone else gets ERROR 5, and an older ACK is ignored."""
    while True:
        try:
            pkt, addr = sock.recvfrom(1024)
        except socket.timeout:
            return False
        if addr != peer:
            sock.sendto(error_packet(5, "Unknown transfer ID"), addr)
            continue
        if len(pkt) >= 4 and int.from_bytes(pkt[0:2], "big") == ACK \
                and int.from_bytes(pkt[2:4], "big") == block:
            return True


def serve_read(srv, n, sock, peer, name, mode):
    with srv.lock:
        data = srv.files.get(name)
    if data is None:
        sock.sendto(error_packet(1, "File not found"), peer)
        return f"rrq {name} {mode}: file not found"
    if mode == "netascii":
        data = to_netascii(data)
    block, at, resends = 1, 0, 0
    while True:
        chunk = data[at:at + BLOCK]
        pkt = data_packet(block, chunk)
        tries = 0
        sock.sendto(pkt, peer)
        while not recv_ack(sock, peer, block):
            tries += 1
            if tries == TRIES:
                return f"rrq {name} {mode}: timed out at block {block}"
            resends += 1
            sock.sendto(pkt, peer)
        at += len(chunk)
        if len(chunk) < BLOCK:
            return f"rrq {name} {mode}: {block} blocks, {len(data)} bytes, {resends} resends"
        block += 1


def serve_write(srv, n, sock, peer, name, mode):
    with srv.lock:
        exists = name in srv.files
    if exists:
        sock.sendto(error_packet(6, "File already exists"), peer)
        return f"wrq {name} {mode}: file already exists"
    sock.settimeout(WAIT_MS / 1000)
    sock.sendto(ack_packet(0), peer)
    got = b""
    expect = 1
    dups = 0
    while True:
        try:
            pkt, addr = sock.recvfrom(1024)
        except socket.timeout:
            return f"wrq {name} {mode}: timed out at block {expect}"
        if addr != peer:
            sock.sendto(error_packet(5, "Unknown transfer ID"), addr)
            continue
        if len(pkt) < 4 or int.from_bytes(pkt[0:2], "big") != DATA:
            sock.sendto(error_packet(4, "Illegal TFTP operation"), peer)
            return f"wrq {name} {mode}: illegal operation"
        block = int.from_bytes(pkt[2:4], "big")
        chunk = pkt[4:]
        if block == expect - 1:
            dups += 1
            sock.sendto(ack_packet(block), peer)
            continue
        if block != expect:
            continue
        got += chunk
        sock.sendto(ack_packet(block), peer)
        if len(chunk) < BLOCK:
            if mode == "netascii":
                got = from_netascii(got)
            with srv.lock:
                srv.files[name] = got
            return f"wrq {name} {mode}: {block} blocks, {len(got)} bytes, {dups} duplicates"
        expect += 1


def transfer(srv, n, op, peer, name, mode):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    sock.settimeout(RESEND_MS / 1000)
    if op == RRQ:
        line = serve_read(srv, n, sock, peer, name, mode)
    else:
        line = serve_write(srv, n, sock, peer, name, mode)
    sock.close()
    with srv.lock:
        srv.log[n] = line


def run_server(srv, sock):
    threads = []
    for n in range(1, REQUESTS + 1):
        pkt, peer = sock.recvfrom(1024)
        op = int.from_bytes(pkt[0:2], "big") if len(pkt) >= 2 else 0
        if op not in (RRQ, WRQ):
            sock.sendto(error_packet(4, "Illegal TFTP operation"), peer)
            with srv.lock:
                srv.log[n] = f"opcode {op}: illegal operation"
            continue
        req = parse_request(pkt)
        if req is None:
            sock.sendto(error_packet(4, "Malformed request"), peer)
            with srv.lock:
                srv.log[n] = "malformed request"
            continue
        name, mode = req
        if mode not in ("octet", "netascii"):
            sock.sendto(error_packet(0, f"Unsupported mode {mode}"), peer)
            with srv.lock:
                srv.log[n] = f"{'rrq' if op == RRQ else 'wrq'} {name} {mode}: unsupported mode"
            continue
        t = threading.Thread(target=transfer, args=(srv, n, op, peer, name, mode))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


# ------------------------------------------------------------ the clients


def show(pkt):
    if len(pkt) < 4:
        return f"short packet ({len(pkt)} bytes)"
    op = int.from_bytes(pkt[0:2], "big")
    n = int.from_bytes(pkt[2:4], "big")
    if op == DATA:
        return f"DATA {n} ({len(pkt) - 4} bytes)"
    if op == ACK:
        return f"ACK {n}"
    if op == ERROR:
        msg = pkt[4:].split(b"\0")[0].decode()
        return f"ERROR {n} {msg}"
    return f"opcode {op}"


class Client:
    def __init__(self, name):
        self.name = name
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(2.0)
        self.tid = None

    def request(self, op, filename, mode):
        self.sock.sendto(u16(op) + filename.encode() + b"\0" + mode.encode() + b"\0", ADDR)

    def recv(self):
        """The next datagram, printed; None on a timeout."""
        try:
            pkt, addr = self.sock.recvfrom(1024)
        except socket.timeout:
            print(f"{self.name} <- timed out")
            return None
        self.tid = addr
        print(f"{self.name} <- {show(pkt)}")
        return pkt

    def send(self, pkt):
        self.sock.sendto(pkt, self.tid)

    def get(self, filename, mode, lose=0):
        """Reads a file, not acknowledging block `lose` the first time."""
        self.request(RRQ, filename, mode)
        data = b""
        lost = False
        while True:
            pkt = self.recv()
            if pkt is None or int.from_bytes(pkt[0:2], "big") != DATA:
                return None
            block = int.from_bytes(pkt[2:4], "big")
            if block == lose and not lost:
                lost = True
                continue
            data += pkt[4:]
            self.send(ack_packet(block))
            if len(pkt) - 4 < BLOCK:
                return data

    def close(self):
        self.sock.close()


def describe(data):
    head = ""
    for b in data[:12]:
        if b == 10:
            head += "\\n"
        elif b == 13:
            head += "\\r"
        elif 32 <= b < 127:
            head += chr(b)
        else:
            head += "."
    return f"{len(data)} bytes, sum {checksum(data)}, starts \"{head}\""


def main():
    srv = Server()
    srv.files["hello.txt"] = b"hello, tftp\nbye\n"
    srv.files["big.bin"] = bytes((i * 7 + 3) % 256 for i in range(1300))
    srv.files["exact.bin"] = bytes(i % 251 for i in range(1024))
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(ADDR)
    server = threading.Thread(target=run_server, args=(srv, sock))
    server.start()

    c = Client("get-hello")
    data = c.get("hello.txt", "octet")
    print(f"get-hello: {describe(data)}")
    c.close()

    c = Client("get-big")
    data = c.get("big.bin", "OCTET", lose=2)
    print(f"get-big: {describe(data)}")
    c.close()

    c = Client("get-exact")
    data = c.get("exact.bin", "octet")
    print(f"get-exact: {describe(data)}")
    c.close()

    c = Client("get-missing")
    c.get("missing.txt", "octet")
    c.close()

    c = Client("get-ascii")
    data = c.get("hello.txt", "netascii")
    print(f"get-ascii: {describe(data)}")
    c.close()

    notes = b"".join(f"note {i}\n".encode() for i in range(100))[:700]
    c = Client("put-notes")
    c.request(WRQ, "notes.txt", "octet")
    c.recv()
    stranger = Client("stranger")
    stranger.tid = c.tid
    stranger.send(data_packet(1, b"intrude"))
    stranger.recv()
    stranger.close()
    c.send(data_packet(1, notes[:512]))
    c.recv()
    c.send(data_packet(1, notes[:512]))
    c.recv()
    c.send(data_packet(2, notes[512:]))
    c.recv()
    c.close()

    c = Client("put-hello")
    c.request(WRQ, "hello.txt", "octet")
    c.recv()
    c.close()

    c = Client("mail-mode")
    c.request(RRQ, "hello.txt", "mail")
    c.recv()
    c.close()

    c = Client("bad-opcode")
    c.sock.sendto(ack_packet(1), ADDR)
    c.recv()
    c.close()

    c = Client("get-notes")
    data = c.get("notes.txt", "octet")
    print(f"get-notes: {describe(data)}, same as put: {'yes' if data == notes else 'no'}")
    c.close()

    server.join()
    sock.close()
    print("== server")
    for n in sorted(srv.log):
        print(f"#{n} {srv.log[n]}")
    for name in sorted(srv.files):
        print(f"file {name} {describe(srv.files[name])}")


main()
