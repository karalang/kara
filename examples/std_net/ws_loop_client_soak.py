#!/usr/bin/env python3
"""Instrumented WebSocket soak client for the Windows IOCP scale investigation
(docs/spikes/windows-iocp-eventloop.md — 1M friction probe).

Same per-connection protocol as ws_loop_client_abortive.py (RFC 6455 upgrade,
one masked text frame, read the echo, abortive/RST close so the client never
parks ephemeral ports in TIME_WAIT). The differences are all about *surfacing
friction* at scale rather than a bare pass/fail:

  * Fixed worker pool of `conc` threads, each pulling from a shared counter —
    in-flight is exactly `conc`, with NO unbounded futures list. (The earlier
    ThreadPoolExecutor harness submitted all N futures upfront and retained
    them, so the *client* accumulated ~1 handle per connection; at 1M that
    client-side growth would confound a server measurement.)
  * Per-connection latency (connect -> abortive close) recorded into a compact
    array('d'); min / p50 / p90 / p99 / p99.9 / max reported at the end so a
    tail-latency stall (shard imbalance, a periodic pause) shows up.
  * A reporter thread prints cumulative count + instantaneous rate every 2s
    (flushed), so a throughput cliff is visible live instead of buffered.

Usage:  python ws_loop_client_soak.py <host> <port> <count> <concurrency>
Exit 0 + "PASS" iff all `count` connections round-tripped.
"""
import socket
import sys
import os
import struct
import time
import threading
from array import array

PAYLOAD = b"hello-iocp"


def recv_exact(s, n):
    out = b""
    while len(out) < n:
        chunk = s.recv(n - len(out))
        if not chunk:
            return None
        out += chunk
    return out


def ws_roundtrip(host, port, idx):
    """One connect -> handshake -> send -> echo -> RST-close cycle.
    Returns None on success, or an error string."""
    try:
        s = socket.create_connection((host, port), timeout=10)
    except OSError as e:
        return f"conn[{idx}] connect failed: {e}"
    # Abortive (RST) close: free the ephemeral port immediately, no TIME_WAIT.
    s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("HH", 1, 0))
    step = "init"
    try:
        s.settimeout(10)
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
        step = "frame-send"
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(PAYLOAD))
        frame = bytes([0x81, 0x80 | len(PAYLOAD)]) + mask + masked
        s.sendall(frame)
        step = "echo-recv"
        hdr = recv_exact(s, 2)
        if hdr is None:
            return f"conn[{idx}] EOF before echo header"
        b0, b1 = hdr[0], hdr[1]
        if b0 != 0x81:
            return f"conn[{idx}] unexpected echo opcode byte 0x{b0:02x}"
        ln = b1 & 0x7F
        if ln == 126:
            ln = struct.unpack(">H", recv_exact(s, 2))[0]
        elif ln == 127:
            ln = struct.unpack(">Q", recv_exact(s, 8))[0]
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


class Worker(threading.Thread):
    def __init__(self, host, port, counter, count, lock, prog):
        super().__init__(daemon=True)
        self.host, self.port = host, port
        self.counter, self.count, self.lock, self.prog = counter, count, lock, prog
        self.done = 0
        self.first_err = None
        self.lat = array("d")  # per-connection seconds

    def run(self):
        while True:
            with self.lock:
                idx = self.counter[0]
                if idx >= self.count:
                    return
                self.counter[0] += 1
            t0 = time.perf_counter()
            err = ws_roundtrip(self.host, self.port, idx)
            dt = time.perf_counter() - t0
            if err is None:
                self.done += 1
                self.lat.append(dt)
                self.prog[0] += 1  # racy-but-fine progress counter
            elif self.first_err is None:
                self.first_err = err


def pct(sorted_lat, p):
    if not sorted_lat:
        return 0.0
    k = min(len(sorted_lat) - 1, int(round(p / 100.0 * (len(sorted_lat) - 1))))
    return sorted_lat[k]


def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 8080
    count = int(sys.argv[3]) if len(sys.argv) > 3 else 1_000_000
    conc = int(sys.argv[4]) if len(sys.argv) > 4 else 16

    print(f"ws_loop_client_soak: {count} connections to {host}:{port}, "
          f"concurrency {conc} (bounded pool, RST close)", flush=True)
    counter = [0]
    prog = [0]
    lock = threading.Lock()
    workers = [Worker(host, port, counter, count, lock, prog) for _ in range(conc)]

    t0 = time.time()
    stop = threading.Event()

    def reporter():
        last_n, last_t = 0, t0
        while not stop.wait(2.0):
            n, now = prog[0], time.time()
            inst = (n - last_n) / (now - last_t) if now > last_t else 0
            print(f"  {n}/{count}  inst={inst:.0f}/s  avg={n/(now-t0):.0f}/s",
                  flush=True)
            last_n, last_t = n, now

    rt = threading.Thread(target=reporter, daemon=True)
    rt.start()
    for w in workers:
        w.start()
    for w in workers:
        w.join()
    stop.set()
    dt = time.time() - t0

    done = sum(w.done for w in workers)
    first_err = next((w.first_err for w in workers if w.first_err), None)
    lat = array("d")
    for w in workers:
        lat.extend(w.lat)
    lat = sorted(lat)
    ms = lambda s: s * 1000.0
    print(f"completed {done}/{count} in {dt:.1f}s ({done/dt:.0f}/s)", flush=True)
    if lat:
        print(f"latency ms: min={ms(lat[0]):.2f} p50={ms(pct(lat,50)):.2f} "
              f"p90={ms(pct(lat,90)):.2f} p99={ms(pct(lat,99)):.2f} "
              f"p99.9={ms(pct(lat,99.9)):.2f} max={ms(lat[-1]):.2f}", flush=True)
    if done == count:
        print("PASS", flush=True)
        sys.exit(0)
    print(f"FAIL: {done}/{count}; first error: {first_err}", flush=True)
    sys.exit(1)


if __name__ == "__main__":
    main()
