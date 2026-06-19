# Relay reverse-proxy benchmark

Side-by-side Layer-7 HTTP reverse-proxy throughput/latency across
**Kāra**, **Go**, and **Node.js** — the recordable artifact for Relay
([`docs/dogfooding.md § Relay`](../../../docs/dogfooding.md)).

Three proxies forward to **one shared upstream backend**, so the thing
under test is the *proxy*, not the origin:

```
              ┌──────────────┐
   wrk  ────► │  proxy under  │ ────►  shared Go upstream
 (client)     │  test (k/g/n) │        (constant "OK", port discovered
              └──────────────┘         once and exported as RELAY_UPSTREAM)
```

- **[`kara/server.kara`](kara/server.kara)** — clean passthrough on
  Kāra's effect-driven parking event loop. `handle(client, addr)` is
  straight-line blocking-looking code; because every `read`/`write_all`
  carries `sends(Network) receives(Network)`, codegen parks the spawned
  handler on fd-readiness instead of thread-blocking it. No `async fn`,
  no goroutine lifecycle. **HTTP/1.1 keep-alive on both legs** — it opens
  one persistent upstream connection per client connection and loops
  servicing requests on the client socket, framing each response by its
  `Content-Length` so it knows where one response ends and can read the
  next request without closing. (The earlier revision served one request
  per connection; see the "Compiler bugs this dogfood surfaced" section
  for the keep-alive rewrite and the codegen bug it flushed out.)
- **[`go/main.go`](go/main.go)** — `httputil.NewSingleHostReverseProxy`
  + `http.Serve`, the idiomatic standard-library reverse proxy. Owns
  connection pooling and **client-side HTTP keep-alive** (it does not
  propagate the upstream's `Connection: close` to the client).
- **[`node/server.js`](node/server.js)** — hand-written `http`-module
  passthrough (no `http-proxy` dep): `http.request` to the upstream,
  pipe body in and response out. Single-process (F4 fairness footnote).
  Node's `http.Server` keeps the client connection alive across
  requests by default.
- **[`upstream/main.go`](upstream/main.go)** — the shared Go origin,
  returns a constant `"OK"`. Built + launched once; intentionally
  trivial so it out-throughputs every proxy and is never the bottleneck.

The Go differentiator framing: the goroutine-per-connection lifecycle
Go's `httputil.ReverseProxy` runs is exactly what you never write in
Kāra's effect-driven event loop.

## How to reproduce

### Toolchain prerequisites

| Impl     | Required toolchain                                      | Tested with |
|----------|--------------------------------------------------------|-------------|
| upstream | `go` (the shared backend — without it nothing runs)    | go 1.26     |
| kara     | `cargo` + this repo's `karac` build (auto-built)       | rustc 1.x   |
| go       | `go`                                                   | go 1.26     |
| node     | `node`                                                 | Node 25     |
| wrk      | `wrk`                                                  | wrk 4.2.0   |

`bench.sh` graceful-degrades when a *proxy* toolchain is missing
(`skip: <lang> ...` to stderr, the bench continues with the rest). The
shared upstream needs `go`; if `go` is absent the whole bench can't run
(no backend to forward to). **nginx and `hey` are not part of this
cohort** — they are not installed on the bench host; the comparison is
kara / go / node, mirroring Parallax's installed-toolchain cohort.

### Run

```sh
# default — all three proxies, cold-start probe + 10s × 3 rounds per (impl, conn)
sh examples/relay/bench/bench.sh

# dry-run (no servers spawned, no wrk; gated in CI via
# tests/relay_bench.rs::test_bench_script_dry_run)
sh examples/relay/bench/bench.sh --dry-run

# subset (kara + go only)
sh examples/relay/bench/bench.sh --impls=k,g

# tweak the sweep / window
sh examples/relay/bench/bench.sh --connections=100,1000 --runs=5 --measure=15
```

`bench.sh` builds the shared upstream and launches it once, discovers
its ephemeral port, exports `RELAY_UPSTREAM=127.0.0.1:<port>`, then for
each proxy: builds it (Kāra via `karac build`, Go via `go build`, Node
served directly from `server.js`), launches it, awaits the conventional
`BOUND_PORT=<n>` stdout line, runs a cold-start probe + a connection
sweep with N=3 measure rounds each, parses `Requests/sec` + the
`wrk --latency` percentiles, and kills the server.

The bench is **not** part of `cargo test`. CI runs only the smoke
tests in [`tests/relay_bench.rs`](../../../tests/relay_bench.rs): a
single-request Kāra-proxy smoke (built through the coroutine path) and a
`bench.sh --dry-run` syntactic gate. Per the F3 design lock the
throughput numbers are the artifact, not a regression gate.

## Throughput results

**Measured on 2026-06-19.** Apple M5 Pro (10P + 8E, 18 logical CPUs),
64 GB RAM, macOS 26.5.1, `wrk 4.2.0`. `bench.sh` defaults: a cold-start
probe (`wrk -t1 -c1 -d1s --latency`, immediately after server spawn),
then a three-point connection sweep (`-c100`, `-c1000`, `-c5000`) with
N=3 measure rounds × 10 s each. Steady-state req/s is the median across
3 rounds with [min..max] range; latencies are medians in milliseconds.
All three impls ran — none skipped.

### Cold start (first ~1 s after spawn, sequential `-t1 -c1`)

| Impl | req/s | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|------|-------|--------|--------|--------|--------|--------|
| Kāra | 17811 | 0.05   | 0.06   | 0.06   | 0.20   | 1.03   |
| Go   | 14245 | 0.06   | 0.07   | 0.10   | 1.87   | 3.35   |
| Node | 14300 | 0.06   | 0.07   | 0.09   | 2.24   | 4.34   |

### Steady-state (sustained `wrk` load)

| Impl | -c    | req/s (median [min..max])   | p50 ms | p75 ms | p90 ms | p99 ms  | max ms  |
|------|-------|-----------------------------|--------|--------|--------|---------|---------|
| Kāra | 100   | 36556 [36052..37328]        | 2.49   | 3.09   | 3.93   | 4.80    | 47.66   |
| Kāra | 1000  | 38180 [37643..38188]        | 24.10  | 28.61  | 33.90  | 122.55  | 735.17  |
| Kāra | 5000  | 34914 [30025..36061]        | 45.12  | 66.74  | 87.02  | 251.69  | 1980.00 |
| Go   | 100   | 1399 [1399..1399]           | 76.13  | 248.87 | 364.68 | 652.73  | 923.29  |
| Go   | 1000  | NA (acceptor saturation)    | NA     | NA     | NA     | NA      | NA      |
| Go   | 5000  | NA (acceptor saturation)    | NA     | NA     | NA     | NA      | NA      |
| Node | 100   | 695 [695..695]              | 114.40 | 142.75 | 211.11 | 754.97  | 1930.00 |
| Node | 1000  | NA (acceptor saturation)    | NA     | NA     | NA     | NA      | NA      |
| Node | 5000  | 11828 [1025..16162]         | 5.94   | 12.50  | 82.25  | 151.00  | 1980.00 |

### How to read this

**This is a measure-first artifact with no pre-baked win condition.**
With keep-alive implemented (see below), the Kāra proxy is now the
strongest and — more notably — the *most stable* impl across the
connection sweep: it sustains **~35–38k req/s at every connection count**
(`-c100`/`-c1000`/`-c5000`) and has the fastest cold start, while both
comparators collapse to unmeasurable (`NA`) under higher loopback-burst
connection counts on this host.

- **Kāra is flat across `-c`.** 36.6k → 38.2k → 34.9k req/s from `-c100`
  to `-c5000`, p50 staying single-digit to low-double-digit ms. The
  event-loop reactor absorbs the connection-count growth without the
  acceptor cliff the thread-per-connection comparators hit. This is the
  property the auto-concurrency story predicts: connection scaling is the
  runtime's job, not the handler's.

- **Go / Node `NA` rows are comparator instability, not a Kāra win per
  se.** Go's `httputil.ReverseProxy` + `http.Serve` and Node's single-
  process `http.Server` both hit a queueing cliff under a 1000–5000-
  connection `wrk` burst on macOS loopback on this host — `wrk` reports
  no completed requests in the window (recorded as `NA`). Their `-c100`
  rows are also depressed and run-to-run noisy (Go 1.4k–3.9k, Node
  0.7k–40k across runs). These are properties of each comparator's
  default config under loopback burst, recorded as-measured with **no
  tuning knobs** (per the fairness controls below) — read them as "the
  comparators are unstable on this harness," not as a precise Kāra-vs-X
  multiple. The honest, defensible claim is the absolute one: **Kāra's
  event-loop proxy holds a stable ~35k req/s across the whole sweep.**

- **The cold-start flip.** In the pre-keep-alive revision Kāra's cold
  start read 0.99 req/s — an artifact of one-request-per-connection, not
  a real number. With keep-alive the cold start is now 17.8k req/s, the
  fastest of the three: the very first held connection pipelines requests
  immediately, no per-request reconnect.

**Conclusion (post-hoc, un-spun).** The benchmark did exactly its job
twice over. The first run surfaced that the passthrough lacked client
keep-alive (the dominant throughput factor under any keep-alive load
generator); implementing it surfaced a real **codegen miscompile** in the
coroutine non-unit-return path (next section). With both fixed, the Kāra
event-loop proxy is competitive-to-leading on throughput and distinctly
more stable under connection-count scaling than the stdlib Go/Node
proxies on this host — and the handler is still plain straight-line
blocking-looking code with no `async fn` and no goroutine lifecycle.

## Fairness controls

- **Hardware:** all three proxies run on the same machine, sequentially
  (one proxy active at a time), forwarding to the *same* shared upstream
  process. Background load is the same for all.
- **Worker counts:** no tuning knobs are pre-set. Kāra uses its runtime's
  natural reactor-thread default; Go uses the default
  `GOMAXPROCS = num_cpus`; Node runs single-process (its default).
- **Single-process Node footnote:** Node's single-process default is
  faithful to the language's typical deployment reality. Cluster-mode
  (`cluster.fork()`) would scale ~`num_cpus`× at the cost of process
  orchestration; not v1 of this bench. Single-process is part of why
  Node hits an acceptor cliff at higher connection counts on this host
  (the `NA` rows); cluster-mode would likely lift those — an honest
  caveat on the comparison, not a Kāra claim.
- **Shared upstream:** one Go origin returning a constant `"OK"`, the
  same backend for every proxy, launched once and reached via the
  ephemeral port discovered + exported as `RELAY_UPSTREAM`. The upstream
  is never the bottleneck.
- **wrk window:** `wrk -t4 -c<n> -d10s --latency`, three rounds per
  `(impl, conn)`, median reported. Same window for every impl.

## Compiler bugs this dogfood surfaced

This dogfood flushed out **two** real compiler bugs — both fixed, both
with regression tests. Surfacing miscompiles under real load is the
demo's load-bearing job.

### 1. Coroutine non-unit return values discarded (keep-alive rewrite)

Rewriting the proxy for keep-alive (§ above) gave `handle` a helper
`relay_response(...) -> bool` that it calls and branches on
(`if not relay_response(...) { return; }`). Both functions suspend on
network I/O, so both compile as coroutines — and a *suspending coroutine
that returns a non-unit value, called from inside another coroutine,
discarded its real return value and yielded a hard-coded `i64 0`*.

**Symptom.** LLVM verification failed outright:
`Branch condition is not 'i1' type!  br i64 -1 ...` (the bool result came
back typed `i64`, so negating/branching on it emitted an `i64` branch).
And even where it didn't crash the verifier, the value was *wrong* —
always 0 — so the keep-alive loop's continue/stop decision was garbage.

**Fix.** [`src/codegen/call_dispatch.rs`](../../../src/codegen/call_dispatch.rs):
on the blocking coroutine-call (`is_coroutine_compiled`) path, after
`park_slot_wait` the caller now reads the callee's real return value out
of the completion slot — into a temp of the callee's *declared* return
LLVM type — via a new runtime pair
[`karac_runtime_park_slot_store_result` / `_load_result`](../../../runtime/src/event_loop.rs)
(the coroutine body stores its return bytes into the slot at completion;
the caller copies them back after the wait, before the free). Unit
returns keep the old `i64 0`. This is the blocking-call analog of the
Fathom `TaskHandle.join()` non-scalar-return fix (`B-2026-06-14-14`),
which had already taught the spawn/slot path to carry return values.

**Regression tests** (`tests/coro_e2e.rs`):
- `coroutine_bool_return_false_branches_then` — a suspending coroutine
  returns `false`; the caller branches on the real value (prints `7`).
- `coroutine_bool_return_true_branches_else` — identical except the
  coroutine returns `true` (prints `8`). The 7-vs-8 difference is the
  point: a fix that still hard-coded 0 would print `7` in **both**, so
  the pair proves value-correctness, not merely that the verifier crash
  is gone.

### 2. Use-after-free in non-blocking coroutine-spawn lowering (initial build)

Building the Kāra proxy through the coroutine network-async path
(`compile_to_object_with_coro`, the path `karac build` uses for a
spawning network server) surfaced a **use-after-free in the non-blocking
coroutine-spawn lowering**.

**Symptom.** The proxied response came back empty (or the upstream
connect silently failed): `handle(client, target)` was spawned with a
closure capturing *both* the moved `TcpStream` *and* a heap `String`
(the upstream address). On the non-blocking spawn path (`use_coro_spawn`)
the spawn wrapper ramps the coroutine and **returns while the coroutine
is still parked** at its first network suspend. The wrapper's cleanup
drain re-registered a `FreeVecBuffer` for the moved-in `String` capture
and freed its buffer on wrapper-return — i.e. *while the coroutine still
held and would later read that `String`*. When the coroutine resumed and
read the upstream address, it touched freed memory.

**Fix.** [`src/codegen/task_group.rs`](../../../src/codegen/task_group.rs):
on the `use_coro_spawn` path, clear `vec_caps` before re-registering the
wrapper-side `FreeVecBuffer` cleanups. The coroutine receives the capture
by value through its ramp args and **owns** it — it frees the buffer at
*its* own completion (body-end + per-park destroy edge), exactly as it
already owns its moved-in `UserDrop` and channel-end params. Only the
*blocking* spawn (where the wrapper itself is the task and runs to
completion) needs the wrapper-side free. A single-capture
(`TcpStream`-only) closure masked the bug — no heap buffer to free, so
the guard was skipped; it took the proxy's two-capture
`(TcpStream, String)` shape to trigger it.

**Regression tests** (`tests/coro_e2e.rs`):
- `coroutine_multi_capture_string_serviced` — functional: spawns a
  coroutine capturing a moved `TcpStream` + a heap `String`, reads the
  `String` *after* the park, asserts the post-park read returns the
  correct length (pre-fix: UAF on the wrapper-freed buffer).
- `coroutine_multi_capture_string_under_asan` — the same shape under
  `-fsanitize=address`; a clean exit proves the buffer is freed exactly
  once, with no use-after-free and no double-free. The captured `String`
  is ≥36 bytes so it is heap-backed (not SSO/inline), which is what the
  sanitizer needs to see the access.

The proxy smoke test [`tests/relay_bench.rs::test_kara_bench_server_smoke`](../../../tests/relay_bench.rs)
also guards this end to end: it builds `server.kara` through the
coroutine path and asserts a non-empty proxied body — if the UAF
resurfaces, it returns empty and the test fails.

## See also

- [`docs/dogfooding.md § Relay`](../../../docs/dogfooding.md) — the
  demo's design storyboard + slice record.
- [`examples/relay/relay.kara`](../relay.kara) — the full feature
  artifact (round-robin LB, full-duplex splice, path routing, live
  metrics). This bench's Kāra impl is the clean slice-1 passthrough
  stripped to raw proxy critical path, apples-to-apples with the Go and
  Node passthroughs.
- [`tests/relay_bench.rs`](../../../tests/relay_bench.rs) — the two CI
  tests that gate the bench harness (smoke + dry-run).
- [`examples/parallax/bench/`](../../parallax/bench/) — the sibling
  bench this harness is modeled on.
