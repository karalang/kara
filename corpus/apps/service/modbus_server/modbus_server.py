"""Oracle for modbus_server.kara: a Modbus/TCP server (the industrial
register protocol PLCs, meters and drives speak) on loopback TCP, and the
clients that use it. Each connection is served by its own thread; the data
model (coils, discrete inputs, holding and input registers) is shared under
one lock. Writing ON to the last coil stops the server."""

import socket
import threading

N_COILS, N_DISCRETE, N_HOLDING, N_INPUT = 64, 32, 32, 16
HALT_COIL = 63
UNIT = 1

EXCEPTIONS = {1: "illegal function", 2: "illegal data address", 3: "illegal data value",
              11: "gateway target failed to respond"}
DEVICE = [b"Kara Industrial", b"KX-485", b"v0.8.2"]


class Server:
    def __init__(self):
        self.coils = [i % 3 == 0 for i in range(N_COILS)]
        self.discrete = [(i * 7) % 5 < 2 for i in range(N_DISCRETE)]
        self.holding = [i * 100 for i in range(N_HOLDING)]
        self.inputs = [0x1000 + i * 3 for i in range(N_INPUT)]
        self.log = []
        self.counts = {}
        self.conns = {}
        self.stopping = False
        self.lock = threading.Lock()

    def count(self, key):
        self.counts[key] = self.counts.get(key, 0) + 1


def u16(b, i):
    return b[i] << 8 | b[i + 1]


def be16(v):
    return bytes([v >> 8 & 0xFF, v & 0xFF])


def pack_bits(bits):
    out = bytearray((len(bits) + 7) // 8)
    for i, on in enumerate(bits):
        if on:
            out[i // 8] |= 1 << (i % 8)
    return bytes(out)


def unpack_bits(data, n):
    return [data[i // 8] >> (i % 8) & 1 == 1 for i in range(n)]


class ModbusError(Exception):
    def __init__(self, code):
        self.code = code


def need(pdu, n):
    if len(pdu) < n:
        raise ModbusError(3)


def check_range(addr, qty, size):
    if addr + qty > size:
        raise ModbusError(2)


def handle(srv, pdu):
    """The response PDU for a request PDU (function code first)."""
    fc = pdu[0]
    if fc in (1, 2):
        need(pdu, 5)
        addr, qty = u16(pdu, 1), u16(pdu, 3)
        if qty < 1 or qty > 2000:
            raise ModbusError(3)
        table = srv.coils if fc == 1 else srv.discrete
        check_range(addr, qty, len(table))
        data = pack_bits(table[addr:addr + qty])
        return bytes([fc, len(data)]) + data
    if fc in (3, 4):
        need(pdu, 5)
        addr, qty = u16(pdu, 1), u16(pdu, 3)
        if qty < 1 or qty > 125:
            raise ModbusError(3)
        table = srv.holding if fc == 3 else srv.inputs
        check_range(addr, qty, len(table))
        out = bytearray([fc, qty * 2])
        for v in table[addr:addr + qty]:
            out += be16(v)
        return bytes(out)
    if fc == 5:
        need(pdu, 5)
        addr, value = u16(pdu, 1), u16(pdu, 3)
        if value not in (0xFF00, 0):
            raise ModbusError(3)
        check_range(addr, 1, N_COILS)
        srv.coils[addr] = value == 0xFF00
        if addr == HALT_COIL and value == 0xFF00:
            srv.stopping = True
        return pdu[:5]
    if fc == 6:
        need(pdu, 5)
        addr, value = u16(pdu, 1), u16(pdu, 3)
        check_range(addr, 1, N_HOLDING)
        srv.holding[addr] = value
        return pdu[:5]
    if fc == 15:
        need(pdu, 6)
        addr, qty, nbytes = u16(pdu, 1), u16(pdu, 3), pdu[5]
        if qty < 1 or qty > 1968 or nbytes != (qty + 7) // 8 or len(pdu) < 6 + nbytes:
            raise ModbusError(3)
        check_range(addr, qty, N_COILS)
        for i, on in enumerate(unpack_bits(pdu[6:6 + nbytes], qty)):
            srv.coils[addr + i] = on
        return pdu[:5]
    if fc == 16:
        need(pdu, 6)
        addr, qty, nbytes = u16(pdu, 1), u16(pdu, 3), pdu[5]
        if qty < 1 or qty > 123 or nbytes != qty * 2 or len(pdu) < 6 + nbytes:
            raise ModbusError(3)
        check_range(addr, qty, N_HOLDING)
        for i in range(qty):
            srv.holding[addr + i] = u16(pdu, 6 + 2 * i)
        return pdu[:5]
    if fc == 22:
        need(pdu, 7)
        addr, and_mask, or_mask = u16(pdu, 1), u16(pdu, 3), u16(pdu, 5)
        check_range(addr, 1, N_HOLDING)
        cur = srv.holding[addr]
        srv.holding[addr] = (cur & and_mask) | (or_mask & (~and_mask & 0xFFFF))
        return pdu[:7]
    if fc == 23:
        need(pdu, 10)
        raddr, rqty, waddr, wqty, nbytes = u16(pdu, 1), u16(pdu, 3), u16(pdu, 5), u16(pdu, 7), pdu[9]
        if rqty < 1 or rqty > 125 or wqty < 1 or wqty > 121 or nbytes != wqty * 2 \
                or len(pdu) < 10 + nbytes:
            raise ModbusError(3)
        check_range(raddr, rqty, N_HOLDING)
        check_range(waddr, wqty, N_HOLDING)
        for i in range(wqty):
            srv.holding[waddr + i] = u16(pdu, 10 + 2 * i)
        out = bytearray([fc, rqty * 2])
        for v in srv.holding[raddr:raddr + rqty]:
            out += be16(v)
        return bytes(out)
    if fc == 43:
        need(pdu, 2)
        if pdu[1] != 14:
            raise ModbusError(1)
        need(pdu, 4)
        code, obj = pdu[2], pdu[3]
        if code == 1:
            ids = [0, 1, 2]
        elif code == 4:
            if obj >= len(DEVICE):
                raise ModbusError(2)
            ids = [obj]
        else:
            raise ModbusError(3)
        out = bytearray([fc, 14, code, 1, 0, 0, len(ids)])
        for i in ids:
            out += bytes([i, len(DEVICE[i])]) + DEVICE[i]
        return bytes(out)
    raise ModbusError(1)


def read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return buf
        buf += chunk
    return buf


def serve_conn(srv, sock, conn_no):
    served = 0
    while True:
        head = read_exact(sock, 7)
        if len(head) == 0:
            break
        if len(head) < 7:
            with srv.lock:
                srv.log.append(f"conn {conn_no}: truncated header ({len(head)} bytes)")
            break
        tid, pid, length, unit = u16(head, 0), u16(head, 2), u16(head, 4), head[6]
        if length < 2 or length > 254:
            with srv.lock:
                srv.log.append(f"conn {conn_no}: bad length {length}, closing")
                srv.count("bad length")
            break
        pdu = read_exact(sock, length - 1)
        if len(pdu) < length - 1:
            with srv.lock:
                srv.log.append(f"conn {conn_no}: truncated frame t={tid:04x} ({len(pdu)} of {length - 1} bytes)")
            break
        with srv.lock:
            if pid != 0:
                srv.log.append(f"conn {conn_no}: t={tid:04x} protocol id {pid}, dropped")
                srv.count("dropped")
                continue
            served += 1
            fc = pdu[0]
            srv.count(f"fc {fc:02x}")
            try:
                if unit != UNIT and unit != 255:
                    raise ModbusError(11)
                reply = handle(srv, pdu)
                srv.log.append(f"conn {conn_no}: t={tid:04x} u={unit} fc={fc:02x} -> ok")
            except ModbusError as e:
                reply = bytes([fc | 0x80, e.code])
                srv.log.append(f"conn {conn_no}: t={tid:04x} u={unit} fc={fc:02x} -> exception {e.code:02x}")
                srv.count(f"exception {e.code:02x}")
        sock.sendall(head[:4] + be16(len(reply) + 1) + bytes([unit]) + reply)
    sock.close()
    with srv.lock:
        srv.conns[conn_no] = served


def serve(srv, listener):
    threads = []
    conn_no = 0
    while True:
        sock, _ = listener.accept()
        with srv.lock:
            stopping = srv.stopping
        if stopping:
            sock.close()
            break
        conn_no += 1
        t = threading.Thread(target=serve_conn, args=(srv, sock, conn_no))
        t.start()
        threads.append(t)
    listener.close()
    for t in threads:
        t.join()


# ------------------------------------------------------------ the clients

def bits_text(bits):
    return "".join("1" if b else "0" for b in bits)


class Client:
    def __init__(self, name, addr):
        self.name = name
        self.sock = socket.create_connection(addr)
        self.tid = 0

    def frame(self, unit, pdu, tid=None, pid=0):
        if tid is None:
            self.tid += 1
            tid = self.tid
        return be16(tid) + be16(pid) + be16(len(pdu) + 1) + bytes([unit]) + pdu

    def recv(self):
        """One response: (tid, unit, pdu), or None at end of stream."""
        head = read_exact(self.sock, 7)
        if len(head) < 7:
            return None
        pdu = read_exact(self.sock, u16(head, 4) - 1)
        return u16(head, 0), head[6], pdu

    def show(self, what, reply, qty=0):
        if reply is None:
            print(f"{self.name}: {what} -> closed by server")
            return
        tid, unit, pdu = reply
        fc = pdu[0]
        if fc & 0x80:
            code = pdu[1]
            text = f"exception {code:02x} {EXCEPTIONS.get(code, 'unknown')}"
        elif fc in (1, 2):
            text = "bits " + bits_text(unpack_bits(pdu[2:], qty)) + f" ({pdu[1]} bytes)"
        elif fc in (3, 4, 23):
            regs = [u16(pdu, 2 + 2 * i) for i in range(pdu[1] // 2)]
            text = "regs " + " ".join(f"{v:04x}" for v in regs)
        elif fc in (5, 6):
            text = f"echo addr={u16(pdu, 1)} value={u16(pdu, 3):04x}"
        elif fc in (15, 16):
            text = f"wrote addr={u16(pdu, 1)} qty={u16(pdu, 3)}"
        elif fc == 22:
            text = f"masked addr={u16(pdu, 1)} and={u16(pdu, 3):04x} or={u16(pdu, 5):04x}"
        elif fc == 43:
            n = pdu[6]
            i = 7
            parts = []
            for _ in range(n):
                obj, ln = pdu[i], pdu[i + 1]
                parts.append(f"{obj}={pdu[i + 2:i + 2 + ln].decode()}")
                i += 2 + ln
            text = f"device code={pdu[2]} " + ", ".join(parts)
        else:
            text = "?"
        print(f"{self.name}: t={tid:04x} u={unit} {what} -> {text}")

    def ask(self, what, pdu, unit=UNIT, qty=0):
        self.sock.sendall(self.frame(unit, pdu))
        self.show(what, self.recv(), qty)

    def read_bits(self, fc, addr, qty):
        kind = "coils" if fc == 1 else "inputs"
        self.ask(f"read {kind} {addr}+{qty}", bytes([fc]) + be16(addr) + be16(qty), qty=qty)

    def read_regs(self, fc, addr, qty, unit=UNIT):
        kind = "holding" if fc == 3 else "input regs"
        self.ask(f"read {kind} {addr}+{qty}", bytes([fc]) + be16(addr) + be16(qty), unit)

    def close(self):
        self.sock.close()


def main():
    srv = Server()
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(16)
    addr = listener.getsockname()
    server = threading.Thread(target=serve, args=(srv, listener))
    server.start()

    print("== plc")
    c = Client("plc", addr)
    c.read_bits(1, 0, 20)
    c.read_bits(2, 0, 10)
    c.read_regs(3, 0, 5)
    c.read_regs(4, 0, 4)
    c.ask("write coil 5 on", bytes([5]) + be16(5) + be16(0xFF00))
    c.ask("write coil 6 = 1234", bytes([5]) + be16(6) + be16(0x1234))
    c.read_bits(1, 0, 10)
    c.ask("write holding 3", bytes([6]) + be16(3) + be16(0xBEEF))
    c.read_regs(3, 0, 5)
    c.ask("write coils 10+10", bytes([15]) + be16(10) + be16(10) + bytes([2, 0b10101100, 0b01]))
    c.read_bits(1, 8, 16)
    c.ask("write holding 20+3", bytes([16]) + be16(20) + be16(3) + bytes([6]) + be16(1) + be16(2) + be16(3))
    c.read_regs(3, 18, 6)
    c.ask("mask holding 3", bytes([22]) + be16(3) + be16(0xF0F2) + be16(0x0025))
    c.read_regs(3, 3, 1)
    c.ask("read 0+4, write 1+2", bytes([23]) + be16(0) + be16(4) + be16(1) + be16(2) + bytes([4])
          + be16(0x1111) + be16(0x2222))
    c.close()

    print("== edge")
    e = Client("edge", addr)
    e.read_bits(1, 0, 0)
    e.read_bits(1, 0, 2001)
    e.read_bits(1, 60, 10)
    e.read_bits(2, 31, 1)
    e.read_regs(3, 0, 126)
    e.read_regs(3, 30, 5)
    e.read_regs(4, 15, 1)
    e.ask("function 07", bytes([7]))
    e.ask("encapsulated 0d", bytes([43, 13, 1, 0]))
    e.ask("device basic", bytes([43, 14, 1, 0]))
    e.ask("device object 1", bytes([43, 14, 4, 1]))
    e.ask("device object 9", bytes([43, 14, 4, 9]))
    e.ask("device code 2", bytes([43, 14, 2, 0]))
    e.read_regs(4, 0, 2, unit=7)
    e.read_regs(4, 0, 2, unit=255)
    e.ask("write coils, short count", bytes([15]) + be16(0) + be16(9) + bytes([1, 0xFF]))
    e.ask("short read", bytes([3, 0, 1]))
    e.ask("write holding 40", bytes([6]) + be16(40) + be16(1))
    e.ask("write coil 64", bytes([5]) + be16(64) + be16(0))
    e.close()

    print("== pipeline")
    p = Client("pipeline", addr)
    burst = b""
    for addr_ in (0, 4, 8):
        burst += p.frame(UNIT, bytes([3]) + be16(addr_) + be16(2), tid=0x10 + addr_)
    p.sock.sendall(burst)
    for addr_ in (0, 4, 8):
        p.show(f"burst read holding {addr_}+2", p.recv())
    p.sock.sendall(p.frame(UNIT, bytes([3]) + be16(0) + be16(1), pid=5)
                   + p.frame(UNIT, bytes([4]) + be16(1) + be16(1)))
    p.show("after a foreign protocol id", p.recv())
    p.close()

    print("== broken")
    b = Client("broken", addr)
    b.sock.sendall(be16(0x77) + be16(0) + be16(300) + bytes([UNIT]))
    b.show("length 300", b.recv())
    b.close()
    t = Client("torn", addr)
    t.sock.sendall(t.frame(UNIT, bytes([3]) + be16(0) + be16(2))[:9])
    t.sock.shutdown(socket.SHUT_WR)
    t.show("half a frame", t.recv())
    t.close()
    t = Client("torn", addr)
    t.sock.sendall(bytes([0, 1, 0]))
    t.sock.shutdown(socket.SHUT_WR)
    t.show("three bytes", t.recv())
    t.close()

    print("== admin")
    a = Client("admin", addr)
    a.ask("write coil 63 on", bytes([5]) + be16(HALT_COIL) + be16(0xFF00))
    a.close()
    socket.create_connection(addr).close()  # wakes the accept loop
    server.join()

    print("== server log")
    for line in srv.log:
        print(line)
    print("== counts")
    for key in sorted(srv.counts):
        print(f"{key}: {srv.counts[key]}")
    print("== connections")
    for conn_no in sorted(srv.conns):
        print(f"conn {conn_no}: {srv.conns[conn_no]} requests")
    print("== tables")
    print("coils   " + bits_text(srv.coils))
    print("holding " + " ".join(f"{v:04x}" for v in srv.holding[:8]))
    print("holding " + " ".join(f"{v:04x}" for v in srv.holding[16:24]))


main()
