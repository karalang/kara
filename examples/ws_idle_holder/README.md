# `ws_idle_holder` — Flagship Demo 1, slice 1 (plaintext `ws://`)

Smallest viable Kāra source that holds N idle WebSocket connections over
plain TCP. Used as the **M1 / M2 / M3 verification gate** per
`docs/implementation_checklist/phase-6-runtime.md` line 170 — the
per-milestone bench harness (slice 3 of the same entry) opens N
concurrent connections against this server and measures
connect-establishment latency, per-connection memory cost, and
steady-state P99 latency under churn.

M1 target: **100K stable idle connections on a single Linux box**.

## What slice 1 ships

Plaintext `ws://` only. TLS lands in slice 2 of the line-170 entry once
the `TLS / HTTPS server-side stdlib` entry ships (5 sub-slices: rustls
runtime FFI, `TlsListener` / `TlsStream` stdlib types, WebSocket-over-TLS
chain, test-cert fixtures, design.md update). The diff between slice 1
and slice 2 is minimal — swap `TcpListener.bind` for
`TlsListener.bind_tls(addr, cert, key)`; everything else (accept loop,
per-connection handler) is unchanged because `WebSocket.accept(listener)`
is parameterised on listener type (slice 9e.2 of phase-6 line 17 shipped
2026-05-25).

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
    let listener: TcpListener = TcpListener.bind("127.0.0.1:0");
    let mut tg: TaskGroup = TaskGroup.new();
    loop {
        let ws: WebSocket = WebSocket.accept(listener);
        tg.spawn(|| handle_connection(ws));
    }
}
```

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
fixed port, edit `src/main.kara`'s `TcpListener.bind("127.0.0.1:0")` to
the desired port literal.

## Manual smoke test

The simplest verification path uses raw HTTP through `nc`:

```sh
# 1. Start the demo (it auto-binds to 127.0.0.1:0)
./ws_idle_holder > /tmp/demo.out &
DEMO=$!

# 2. Read the ephemeral port from BOUND_PORT
PORT=$(awk -F= '/BOUND_PORT/ {print $2; exit}' /tmp/demo.out)

# 3. Send a WebSocket Upgrade request, expect HTTP/1.1 101 Switching Protocols
printf 'GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n' \
    | nc -w 2 127.0.0.1 $PORT

# Expected stdout:
#   HTTP/1.1 101 Switching Protocols
#   Upgrade: websocket
#   Connection: Upgrade
#   Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=

# 4. Clean up
kill $DEMO
```

The `Sec-WebSocket-Accept` value is the RFC 6455-mandated SHA-1 of the
client's `Sec-WebSocket-Key` (`dGhlIHNhbXBsZSBub25jZQ==`) concatenated
with the protocol GUID (`258EAFA5-E914-47DA-95CA-C5AB0DC85B11`), base64
encoded; `s3pPLMBiTxaQ9kYGzzhZRbK+xOo=` is the canonical reference
value from the RFC 6455 spec § 1.3.

Once `websocat` is installed locally, full bidirectional frame round-trip
verification:

```sh
websocat ws://127.0.0.1:$PORT
# (type some text, press Ctrl-D; the demo discards but accepts the frame)
```

## What slice 1 deliberately omits

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
- **No Rust-side automated e2e test.** Slice 1 ships the kara source +
  smoke-test recipe in this README; the automated bench harness (slice
  3 of line 170) is the natural home for the e2e test — it already
  needs to open N concurrent connections, so a single-connection
  acceptance test is a degenerate case of the harness it builds.

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
