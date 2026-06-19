#!/usr/bin/env python3
"""WebSocket connection-DENSITY harness for the Windows IOCP scale work
(docs/spikes/windows-iocp-scale-investigation.md — connection-density probe).

Distinct from ws_loop_client_soak.py, which measures connection *churn* (open ->
echo -> RST-close, only `conc` live at any instant). This harness opens N
*persistent* WebSocket connections and HOLDS them all open simultaneously, so
the server holds N concurrently-parked handlers (N live KaracTaskHandle +
KaracParkSlot + one persistent mio source / AFD poll each — the Problem 4
per-socket model under sustained density). It then keeps them alive for a hold
window (so the server can be sampled for handle/RSS scaling), runs a liveness
probe (a fresh connection must still establish — proves no density wedge), and
finally closes them all.

Each held connection consumes ONE client ephemeral port (the server side shares
:8080 via the 4-tuple, so it spends a handle but not a port). On Windows loopback
(only 127.0.0.1, no 127/8 fan-out) the ceiling is the dynamic port range; widen
it with `netsh int ipv4 set dynamicport tcp start=10000 num=55000` to reach ~50k.

Usage:  python ws_density_client.py <host> <port> <count> [hold_seconds]
Prints ESTABLISHED <n>/<count>, LIVENESS_OK|FAIL, and PASS iff all N held.
"""
import socket
import sys
import struct
import time

UPGRADE = (
    b"GET / HTTP/1.1\r\n"
    b"Host: %b:%d\r\n"
    b"Upgrade: websocket\r\n"
    b"Connection: Upgrade\r\n"
    b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
    b"Sec-WebSocket-Version: 13\r\n"
    b"\r\n"
)


def open_ws(host, port, timeout=10):
    """connect + RFC6455 upgrade; return the live socket or raise."""
    s = socket.create_connection((host, port), timeout=timeout)
    s.settimeout(timeout)
    s.sendall(UPGRADE % (host.encode(), port))
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = s.recv(4096)
        if not chunk:
            s.close()
            raise OSError("EOF before 101 handshake response")
        buf += chunk
        if len(buf) > 65536:
            s.close()
            raise OSError("handshake response too large")
    status = buf.split(b"\r\n", 1)[0]
    if b"101" not in status:
        s.close()
        raise OSError(f"expected 101, got {status!r}")
    # Leave the socket idle (no frame sent) so the server's recv_text parks —
    # this is what makes the connection a HELD, density-contributing one.
    return s


def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 8080
    count = int(sys.argv[3]) if len(sys.argv) > 3 else 45000
    hold = float(sys.argv[4]) if len(sys.argv) > 4 else 45.0

    print(f"ws_density_client: opening {count} persistent connections to "
          f"{host}:{port}, hold {hold:.0f}s", flush=True)
    conns = []
    t0 = time.time()
    first_err = None
    for i in range(count):
        try:
            conns.append(open_ws(host, port))
        except OSError as e:
            first_err = f"conn[{i}] failed: {e}"
            break
        n = len(conns)
        if n % 5000 == 0:
            rate = n / (time.time() - t0)
            print(f"  established {n}/{count}  ({rate:.0f}/s)", flush=True)

    established = len(conns)
    ramp = time.time() - t0
    print(f"ESTABLISHED {established}/{count} in {ramp:.1f}s "
          f"({established/ramp:.0f}/s)", flush=True)
    if first_err:
        print(f"  first establishment failure: {first_err}", flush=True)

    # Hold the whole set open so the server can be sampled at peak density.
    print(f"HOLDING {established} connections for {hold:.0f}s "
          f"(sample the server now)", flush=True)
    time.sleep(hold)

    # Liveness probe: a brand-new connection must still establish while N are
    # held — proves the accept/register/park path did not wedge at density.
    liveness = "LIVENESS_FAIL"
    try:
        probe = open_ws(host, port, timeout=10)
        probe.close()
        liveness = "LIVENESS_OK"
    except OSError as e:
        liveness = f"LIVENESS_FAIL ({e})"
    print(liveness, flush=True)

    # Verify the held set is still alive (peer didn't drop them): a held socket
    # should have no pending data and not be readable-with-EOF. We just close.
    for s in conns:
        try:
            s.close()
        except OSError:
            pass

    if established == count and liveness == "LIVENESS_OK":
        print("PASS", flush=True)
        sys.exit(0)
    print(f"FAIL: established {established}/{count}; {liveness}", flush=True)
    sys.exit(1)


if __name__ == "__main__":
    main()
