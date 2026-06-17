#!/usr/bin/env python3
"""Minimal WebSocket loopback client for the Windows IOCP functional run
(docs/spikes/windows-iocp-eventloop.md step 6).

Opens N short-lived WebSocket connections to a plaintext ws_echo server
(examples/std_net/ws_echo.kara): for each, do the RFC 6455 upgrade handshake,
send one masked text frame, read the server's echoed (unmasked) frame, verify
it round-trips, then close. This churns the karac IOCP event loop's
register/deregister cycle N times (listener accept-readiness + per-connection
recv-readiness), exercising the Problem-3 source-map-lifetime fix at scale and
catching any wedge (a connection that never completes) or fd/source-map leak.

Pure stdlib (no `websockets` dependency) so it runs on a bare Python.

Usage:  python ws_loop_client.py <host> <port> <count> <concurrency>
Exit 0 + "PASS" iff all `count` connections round-tripped; non-zero + "FAIL"
otherwise (with the first failure reason and how many completed).
"""
import socket
import sys
import os
import struct
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

PAYLOAD = b"hello-iocp"


def ws_roundtrip(host, port, idx):
    """One full connect -> handshake -> send -> echo -> close cycle.
    Returns None on success, or an error string."""
    try:
        s = socket.create_connection((host, port), timeout=10)
    except OSError as e:
        return f"conn[{idx}] connect failed: {e}"
    # Abortive close (RST instead of FIN) so the client's ephemeral port is
    # freed immediately and never parks in TIME_WAIT. At 250k short-lived
    # loopback connections this is what keeps the ~16k-port ephemeral pool from
    # exhausting (FIN-close would pile up TIME_WAITs faster than they expire).
    # struct linger on Windows is two u_shorts {l_onoff, l_linger}; onoff=1,
    # linger=0 => RST on close. The server's final recv then returns an error
    # (ECONNRESET) instead of a 0-length read, breaking its echo loop and
    # closing the socket exactly as a FIN would — same IOCP deregister path.
    s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("HH", 1, 0))
    step = "init"
    try:
        s.settimeout(10)
        # RFC 6455 opening handshake. Fixed Sec-WebSocket-Key (the server echoes
        # back the matching Accept; we don't validate it here beyond seeing 101).
        step = "handshake-send"
        req = (
            b"GET / HTTP/1.1\r\n"
            b"Host: %b:%d\r\n"
            b"Upgrade: websocket\r\n"
            b"Connection: Upgrade\r\n"
            b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
            b"Sec-WebSocket-Version: 13\r\n"
            b"\r\n"
        ) % (host.encode(), port)
        s.sendall(req)

        # Read the response headers up to the CRLFCRLF terminator.
        step = "handshake-recv-101"
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = s.recv(4096)
            if not chunk:
                return f"conn[{idx}] EOF before 101 (got {buf!r})"
            buf += chunk
            if len(buf) > 65536:
                return f"conn[{idx}] handshake response too large"
        status = buf.split(b"\r\n", 1)[0]
        if b"101" not in status:
            return f"conn[{idx}] expected 101, got: {status!r}"

        # Send one masked text frame (client->server MUST be masked, RFC 6455
        # 5.1). FIN=1, opcode=0x1; payload len < 126 so it fits the 7-bit field.
        step = "frame-send"
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(PAYLOAD))
        frame = bytes([0x81, 0x80 | len(PAYLOAD)]) + mask + masked
        s.sendall(frame)

        # Read the server's echoed frame (server->client is unmasked).
        step = "echo-recv"
        hdr = recv_exact(s, 2)
        if hdr is None:
            return f"conn[{idx}] EOF before echo header"
        b0, b1 = hdr[0], hdr[1]
        if b0 != 0x81:
            return f"conn[{idx}] unexpected echo opcode byte 0x{b0:02x}"
        ln = b1 & 0x7F
        if ln == 126:
            ext = recv_exact(s, 2)
            ln = struct.unpack(">H", ext)[0]
        elif ln == 127:
            ext = recv_exact(s, 8)
            ln = struct.unpack(">Q", ext)[0]
        payload = recv_exact(s, ln) if ln else b""
        if payload is None:
            return f"conn[{idx}] EOF before echo payload"
        if payload != PAYLOAD:
            return f"conn[{idx}] echo mismatch: {payload!r} != {PAYLOAD!r}"
        return None
    except OSError as e:
        return f"conn[{idx}] io error at step={step}: {e}"
    finally:
        try:
            s.close()
        except OSError:
            pass


def recv_exact(s, n):
    out = b""
    while len(out) < n:
        chunk = s.recv(n - len(out))
        if not chunk:
            return None
        out += chunk
    return out


def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 8080
    count = int(sys.argv[3]) if len(sys.argv) > 3 else 10000
    conc = int(sys.argv[4]) if len(sys.argv) > 4 else 50

    print(f"ws_loop_client: {count} connections to {host}:{port}, concurrency {conc}")
    t0 = time.time()
    done = 0
    first_err = None
    with ThreadPoolExecutor(max_workers=conc) as ex:
        futs = [ex.submit(ws_roundtrip, host, port, i) for i in range(count)]
        for f in as_completed(futs):
            err = f.result()
            if err is None:
                done += 1
            elif first_err is None:
                first_err = err
            if (done % 1000) == 0 and done > 0 and err is None:
                print(f"  {done}/{count} ok  ({done/(time.time()-t0):.0f}/s)")
    dt = time.time() - t0
    print(f"completed {done}/{count} in {dt:.1f}s ({done/dt:.0f}/s)")
    if done == count:
        print("PASS")
        sys.exit(0)
    print(f"FAIL: {done}/{count} completed; first error: {first_err}")
    sys.exit(1)


if __name__ == "__main__":
    main()
