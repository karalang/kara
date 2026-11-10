"""Oracle for socks5_proxy.kara: the same SOCKS5 proxy over real sockets.

The proxy, its upstream echo service and the scripted clients are plain
`socket` code on threads. Each client step reads exactly what the proxy owes
it, so the output does not depend on scheduling.
"""

import socket
import threading

PROXY = ("127.0.0.1", 47761)
ECHO = ("127.0.0.1", 47762)
CLOSED_PORT = 47763
ALLOWED_PORTS = (47762, 47763)
USERS = {"alice": "wonderland", "bob": "builder"}
CONNECTIONS = 10
RELAYED = 2

OK, NOT_ALLOWED, HOST_UNREACHABLE, REFUSED, BAD_COMMAND, BAD_ADDRESS = 0, 2, 4, 5, 7, 8


class Closed(Exception):
    pass


def read_exact(s, n):
    out = b""
    while len(out) < n:
        chunk = s.recv(n - len(out))
        if not chunk:
            raise Closed()
        out += chunk
    return out


def reply(code):
    # VER REP RSV ATYP=IPv4 BND.ADDR=0.0.0.0 BND.PORT=0
    return bytes([5, code, 0, 1, 0, 0, 0, 0, 0, 0])


def handshake(conn, log):
    """The greeting, the RFC 1929 login and the request. Returns the
    address to connect to, or None once the connection is refused."""
    head = read_exact(conn, 2)
    if head[0] != 5:
        log.append(f"version {head[0]}: closed")
        return None
    methods = read_exact(conn, head[1])
    if 2 not in methods:
        conn.sendall(bytes([5, 0xFF]))
        log.append("no acceptable method")
        return None
    conn.sendall(bytes([5, 2]))
    ver = read_exact(conn, 1)[0]
    ulen = read_exact(conn, 1)[0]
    user = read_exact(conn, ulen).decode()
    plen = read_exact(conn, 1)[0]
    password = read_exact(conn, plen).decode()
    if ver != 1 or USERS.get(user) != password:
        conn.sendall(bytes([1, 1]))
        log.append(f"login failed for {user}")
        return None
    conn.sendall(bytes([1, 0]))
    ver, cmd, _rsv, atyp = read_exact(conn, 4)
    if atyp == 1:
        host = ".".join(str(b) for b in read_exact(conn, 4))
    elif atyp == 3:
        n = read_exact(conn, 1)[0]
        host = read_exact(conn, n).decode()
    elif atyp == 4:
        read_exact(conn, 16)
        read_exact(conn, 2)
        conn.sendall(reply(BAD_ADDRESS))
        log.append(f"{user}: IPv6 not supported")
        return None
    else:
        conn.sendall(reply(BAD_ADDRESS))
        log.append(f"{user}: address type {atyp}")
        return None
    port = int.from_bytes(read_exact(conn, 2), "big")
    if cmd != 1:
        conn.sendall(reply(BAD_COMMAND))
        log.append(f"{user}: command {cmd} not supported")
        return None
    if host == "localhost":
        host = "127.0.0.1"
    elif atyp == 3:
        conn.sendall(reply(HOST_UNREACHABLE))
        log.append(f"{user}: cannot resolve {host}")
        return None
    if port not in ALLOWED_PORTS:
        conn.sendall(reply(NOT_ALLOWED))
        log.append(f"{user}: port {port} not allowed")
        return None
    return (user, host, port)


def pump(src, dst, counter, key, lock):
    total = 0
    while True:
        try:
            chunk = src.recv(4096)
        except OSError:
            break
        if not chunk:
            break
        dst.sendall(chunk)
        total += len(chunk)
    try:
        dst.shutdown(socket.SHUT_WR)
    except OSError:
        pass
    with lock:
        counter[key] = total


def linger_close(conn):
    """Shut the write side and drain what the client still sends before
    closing, so a refused client reads the answer rather than a reset."""
    try:
        conn.shutdown(socket.SHUT_WR)
        conn.settimeout(0.5)
        while conn.recv(4096):
            pass
    except OSError:
        pass
    conn.close()


def serve(conn, cid, logs, lock):
    log = []
    try:
        target = handshake(conn, log)
        if target is not None:
            user, host, port = target
            try:
                up = socket.create_connection((host, port))
            except OSError:
                conn.sendall(reply(REFUSED))
                log.append(f"{user}: {host}:{port} refused")
                target = None
        if target is not None:
            conn.sendall(reply(OK))
            counts = {}
            t = threading.Thread(target=pump, args=(up, conn, counts, "down", lock))
            t.start()
            pump(conn, up, counts, "up", lock)
            t.join()
            up.close()
            log.append(f"{user}: {host}:{port} relayed {counts['up']} up, {counts['down']} down")
    except Closed:
        log.append("closed mid-handshake")
    linger_close(conn)
    with lock:
        logs[cid] = log


def run_proxy(listener, logs, lock):
    threads = []
    for cid in range(1, CONNECTIONS + 1):
        conn, _ = listener.accept()
        t = threading.Thread(target=serve, args=(conn, cid, logs, lock))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


def echo_one(conn):
    f = conn.makefile("rb")
    for raw in f:
        line = raw.decode().rstrip("\n")
        conn.sendall(f"echo: {line.upper()}\n".encode())
    f.close()
    conn.close()


def run_echo(listener):
    threads = []
    for _ in range(RELAYED):
        conn, _ = listener.accept()
        t = threading.Thread(target=echo_one, args=(conn,))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


def hexs(b):
    return " ".join(f"{x:02x}" for x in b)


def recv_all(s):
    out = b""
    while True:
        chunk = s.recv(4096)
        if not chunk:
            return out
        out += chunk


def ipv4_request(cmd, addr, port):
    return bytes([5, cmd, 0, 1]) + bytes(addr) + port.to_bytes(2, "big")


def name_request(name, port):
    return bytes([5, 1, 0, 3, len(name)]) + name.encode() + port.to_bytes(2, "big")


def login(user, password):
    return bytes([1, len(user)]) + user.encode() + bytes([len(password)]) + password.encode()


def client(tag, greeting, auth, request, payload):
    s = socket.create_connection(PROXY)
    s.settimeout(2.0)
    parts = []
    s.sendall(greeting)
    if greeting[0] != 5:
        rest = recv_all(s)
        parts.append(f"closed ({len(rest)} bytes)")
        print(f"{tag}: {'; '.join(parts)}")
        s.close()
        return
    choice = read_exact(s, 2)
    parts.append(f"method {hexs(choice)}")
    if choice[1] == 2:
        s.sendall(auth)
        status = read_exact(s, 2)
        parts.append(f"login {hexs(status)}")
        if status[1] == 0:
            s.sendall(request)
            rep = read_exact(s, 10)
            parts.append(f"reply {hexs(rep)}")
            if rep[1] == 0:
                s.sendall(payload)
                s.shutdown(socket.SHUT_WR)
                text = recv_all(s).decode()
                parts.append("relay " + " | ".join(text.rstrip("\n").split("\n")))
    rest = recv_all(s)
    if rest:
        parts.append(f"then {hexs(rest)}")
    print(f"{tag}: {'; '.join(parts)}")
    s.close()


def main():
    echo_l = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    echo_l.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    echo_l.bind(ECHO)
    echo_l.listen(8)
    proxy_l = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    proxy_l.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    proxy_l.bind(PROXY)
    proxy_l.listen(8)
    logs = {}
    lock = threading.Lock()
    echo_t = threading.Thread(target=run_echo, args=(echo_l,))
    echo_t.start()
    proxy_t = threading.Thread(target=run_proxy, args=(proxy_l, logs, lock))
    proxy_t.start()

    print("== clients")
    pw = bytes([5, 2, 0, 2])
    good = login("alice", "wonderland")
    local = [127, 0, 0, 1]
    client("1 connect", pw, good, ipv4_request(1, local, ECHO[1]), b"hello\nworld\n")
    client("2 no password method", bytes([5, 1, 0]), b"", b"", b"")
    client("3 wrong password", pw, login("alice", "looking-glass"), b"", b"")
    client("4 bind", pw, login("bob", "builder"), ipv4_request(2, local, ECHO[1]), b"")
    client("5 ipv6", pw, good, bytes([5, 1, 0, 4]) + bytes(15) + bytes([1]) + ECHO[1].to_bytes(2, "big"), b"")
    client("6 by name", pw, login("bob", "builder"), name_request("localhost", ECHO[1]), b"via name\n")
    client("7 unknown name", pw, good, name_request("example.invalid", ECHO[1]), b"")
    client("8 port 22", pw, good, ipv4_request(1, local, 22), b"")
    client("9 refused", pw, good, ipv4_request(1, local, CLOSED_PORT), b"")
    client("10 socks4", bytes([4, 1, 0, 80, 127, 0, 0, 1, 0]), b"", b"", b"")

    proxy_t.join()
    echo_t.join()
    proxy_l.close()
    echo_l.close()
    print("== proxy log")
    for cid in sorted(logs):
        for line in logs[cid]:
            print(f"{cid}: {line}")


main()
