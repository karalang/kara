# `ws_idle_holder` — Flagship Demo 1, slice 2 (`wss://` over TLS)

Smallest viable Kāra source that holds N idle WebSocket connections over
TLS. Used as the **M1 / M2 / M3 verification gate** per
`docs/implementation_checklist/phase-6-runtime.md` line 170 — the
per-milestone bench harness (slice 3 of the same entry) opens N
concurrent `wss://` connections against this server and measures
connect-establishment latency, per-connection memory cost, and
steady-state P99 latency under churn.

M1 target: **100K stable idle connections on a single Linux box**.

## What slice 2 ships

TLS-wrapped `wss://`. The diff from slice 1 (plaintext `ws://`) is two
surface swaps:

- `TcpListener.bind(addr)` → `TlsListener.bind_tls(addr, cert_pem, key_pem)`
- `WebSocket.accept(listener)` → `WebSocket.accept_tls(listener)`

Everything below those calls (accept loop, per-connection handler,
`recv_text` / Drop) is unchanged: the `WebSocket` returned by
`accept_tls` has the same kara-visible shape as `accept`'s, and the WS
framing FFIs auto-dispatch encryption through the per-fd rustls session
registered during the handshake (phase-6 line 236 slice 3). No kara-side
TLS flag.

The `TLS / HTTPS server-side stdlib` entry (phase-6 line 236, 5
sub-slices: rustls runtime FFI, `TlsListener` / `TlsStream` types,
`WebSocket.accept_tls`, test-cert fixtures, design.md update) shipped
2026-05-27 and is the prereq this slice consumed.

## Architecture

Single accept thread, fan-out per connection through `TaskGroup.spawn`:

```kara
fn handle_connection(ws: WebSocket) {
    let mut buf: Array[u8, 4096] = [0u8; 4096];
    loop {
        let r = ws.recv_text(mut buf);
        match r {
            Result.Ok(n) => { if n == 0 { break; } }
            Result.Err(_) => { break; }
        }
    }
}

fn main() {
    // cert_pem / key_pem inlined as PEM literals (see "Cert handling" below)
    let listener: TlsListener = TlsListener.bind_tls("127.0.0.1:0", cert_pem, key_pem);
    let mut tg: TaskGroup = TaskGroup.new();
    loop {
        let ws: WebSocket = WebSocket.accept_tls(listener);
        tg.spawn(|| handle_connection(ws));
    }
}
```

## Cert handling

The demo carries the v1 self-signed test cert (CN=localhost, valid
through 2036) inlined as PEM string literals in `src/main.kara`. The
same bytes live in `tests/fixtures/tls/cert.pem` + `key.pem` (phase-6
line 236 slice 4) — they are committed test fixtures, so inlining
exposes nothing not already in the repo.

**Why inlined rather than read from disk?** The natural shape —
`FileSystem.read_to_string("tests/fixtures/tls/cert.pem").unwrap()` —
trips a codegen gap: `FileSystem.read_to_string` is a `#[compiler_builtin]`
stub with no codegen lowering yet (only the stateful `File` handle path
from Phase 8 slice F is wired), so the call returns an `i64 0` at the
LLVM layer instead of a `Result`. Disk-loading lands when the
`FileSystem.read_to_string` codegen lowering ships (tracked as a
follow-on under phase-6 line 236); the bench harness (Demo 1 slice 3)
will load real certs from disk once that gap closes.

The recv_text loop is a pure "wait-and-wake" pattern: the parking
primitive (`karac_park_on_fd`) suspends the per-connection task on the
connection's read-readiness, so 100K idle connections occupy ~100K
*parked* tasks in the scheduler's wait-set, not 100K *running*
threads. The `Drop` impl on `WebSocket` (slice 9d of phase-6 line 17)
closes the fd when the per-task handler returns, so clean peer
disconnects don't leak file descriptors.

## Prereqs the demo surfaced

Demo 1 slice 1 shipped exactly the workload-shaped pressure on the
spawn/TaskGroup design that the tracker (line 182) anticipated. Three
slices landed as direct unblockers:

- **Slice 8** of phase-6 line 218 (`spawn` slot widened to `OnceFn() -> T`)
  — the canonical `tg.spawn(|| handle_connection(ws))` shape consumed
  `ws` (move-capture), making the closure once-callable, which the
  original `Fn() -> T` slot rejected. `OnceFn` is the
  semantically-correct slot since `karac_runtime_spawn` invokes the
  closure body exactly once.

- **Slice 9** of phase-6 line 218 (stdlib struct-by-value param LLVM
  ABI) — passing `WebSocket` by value to `handle_connection` produced
  an LLVM-verifier mismatch (`i64` declared param vs `{ i32 }`
  call-site value). The fix added explicit arms in
  `llvm_type_for_name` for `TcpListener` / `TcpStream` / `WebSocket`
  mirroring the existing `String` / `Vec` / `Slice` baked-stdlib
  precedent.

- **Slice 3a** of phase-6 line 170 (cross-task-safe boundary check) —
  the per-iteration `ws` is a baked-stdlib struct with only
  cross-task-safe leaves (`i32`), so the check passes without
  diagnostic; pinned the design ahead of the demo's actual capture.

## How to run

```sh
# from karac-rust workspace root:
cargo build --bin karac --features llvm
cargo build -p karac-runtime --release

cd examples/ws_idle_holder/
../../target/debug/karac build
./ws_idle_holder &
```

The binary prints `BOUND_PORT=<n>\n` to stdout (the
`runtime/stdlib/tcp.kara` BOUND_PORT convention for the `:0` ephemeral
case) so smoke-test scripts can read back the assigned port. To pin a
fixed port, edit `src/main.kara`'s
`TlsListener.bind_tls("127.0.0.1:0", ...)` to the desired port literal.

## Manual smoke test

Because the listener is TLS-wrapped, the smoke test must speak TLS — a
raw `nc` connection no longer works. Easiest path is a short Python
`ssl` client that completes the TLS handshake then sends the WebSocket
HTTP upgrade:

```sh
# 1. Start the demo (it auto-binds to 127.0.0.1:0)
./ws_idle_holder > /tmp/demo.out &
DEMO=$!

# 2. Read the ephemeral port from BOUND_PORT
PORT=$(awk -F= '/BOUND_PORT/ {print $2; exit}' /tmp/demo.out)

# 3. TLS-connect + send a WebSocket Upgrade, expect 101 Switching Protocols
python3 - "$PORT" <<'PY'
import socket, ssl, sys, base64, os, hashlib
port = int(sys.argv[1])
ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE          # self-signed test cert
raw = socket.create_connection(("127.0.0.1", port), timeout=5)
s = ctx.wrap_socket(raw, server_hostname="localhost")
key = base64.b64encode(os.urandom(16)).decode()
s.sendall((
    "GET / HTTP/1.1\r\nHost: localhost\r\n"
    "Upgrade: websocket\r\nConnection: Upgrade\r\n"
    f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
).encode())
resp = s.recv(4096).decode(errors="replace")
GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
expect = base64.b64encode(hashlib.sha1((key+GUID).encode()).digest()).decode()
print(resp)
print("MATCH" if "101 Switching Protocols" in resp and expect in resp else "FAIL")
PY

# Expected stdout:
#   HTTP/1.1 101 Switching Protocols
#   Upgrade: websocket
#   Connection: Upgrade
#   Sec-WebSocket-Accept: <base64 of sha1(client_key + GUID)>
#   MATCH

# 4. Clean up
kill $DEMO
```

The `Sec-WebSocket-Accept` value is the RFC 6455-mandated SHA-1 of the
client's `Sec-WebSocket-Key` concatenated with the protocol GUID
(`258EAFA5-E914-47DA-95CA-C5AB0DC85B11`), base64-encoded. Because the
client key is randomized per run, the script computes the expected
accept value rather than hardcoding it — `MATCH` confirms the full TLS
handshake + WebSocket-over-TLS upgrade round-tripped correctly.

Once `websocat` is installed locally, full bidirectional frame round-trip
verification (note the `--insecure` flag for the self-signed cert):

```sh
websocat --insecure wss://localhost:$PORT
# (type some text, press Ctrl-D; the demo discards but accepts the frame)
```

## What the demo deliberately omits

- **No echo.** M1's target is *idle* connections; echo would introduce
  per-frame CPU and stdout traffic that confounds the memory + latency
  measurements at scale.
- **No structured logging.** `std.tracing` integration lands when the
  demo grows beyond the smoke-test phase (slice 3+ of line 170).
- **No graceful shutdown.** The accept loop runs until interrupted
  (SIGINT / process kill). M1 verification scripts handle subprocess
  lifecycle externally.
- **No max-connection cap.** Defer to OS-level `ulimit -n`; the
  `#[concurrency(max_tasks: N)]` annotation on `TaskGroup` lands when a
  real cap-shaped requirement surfaces.
- **No Rust-side automated e2e test.** The kara source + smoke-test
  recipe in this README is the slice-2 verification surface; the
  automated bench harness (slice 3 of line 170) is the natural home for
  the e2e test — it already needs to open N concurrent connections, so
  a single-connection acceptance test is a degenerate case of the
  harness it builds.
- **Cert inlined, not loaded from disk.** See "Cert handling" above —
  blocked on the `FileSystem.read_to_string` codegen lowering, tracked
  as a follow-on under phase-6 line 236.

## See also

- `docs/implementation_checklist/phase-6-runtime.md` § Flagship Demo 1
  (line 170) — the parent epic + sub-slices 2 (TLS), 3 (bench harness),
  4 (M1 100K verification), 5 (M2 250K), 6 (M3 1M+), 7 (CI gate).
- `docs/implementation_checklist/phase-6-runtime.md` § spawn() /
  TaskGroup runtime + codegen + stdlib (line 218) slices 1–9 — the
  spawn/TaskGroup compiler stack the demo builds on.
- `docs/design.md` § Explicit Concurrency (lines 9357–9366) — the
  canonical accept-loop shape the demo realises.
- `runtime/stdlib/tcp.kara` + `runtime/stdlib/ws.kara` — the stdlib
  types the demo composes.
- `runtime/stdlib/task_group.kara` — `TaskGroup.spawn` / `TaskHandle`
  surface.
- `examples/parallax_lite/` — sister project, same multi-file shape,
  different concurrency surface (auto-par over effect resources rather
  than explicit `TaskGroup.spawn`).
