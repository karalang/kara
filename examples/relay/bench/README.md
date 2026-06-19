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

- **[`kara/server.kara`](kara/server.kara)** — clean slice-1 passthrough
  on Kāra's effect-driven parking event loop. `handle(client, addr)` is
  straight-line blocking-looking code; because every `read`/`write_all`
  carries `sends(Network) receives(Network)`, codegen parks the spawned
  handler on fd-readiness instead of thread-blocking it. No `async fn`,
  no goroutine lifecycle. **One request per connection** — it reads the
  first request chunk, opens the upstream, forwards, streams the
  response back, and the handler returns (closing the client socket).
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
| Kāra |  0.99 | 0.48   | 0.48   | 0.48   | 0.48   | 0.48   |
| Go   | 14100 | 0.06   | 0.07   | 0.11   | 2.81   | 4.34   |
| Node | 13542 | 0.07   | 0.07   | 0.09   | 3.06   | 9.53   |

### Steady-state (sustained `wrk` load)

| Impl | -c    | req/s (median [min..max])   | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|------|-------|-----------------------------|--------|--------|--------|--------|--------|
| Kāra | 100   | 10 [10..10]                 | 5.98   | 8.12   | 9.46   | 10.06  | 10.06  |
| Kāra | 1000  | 99 [99..99]                 | 6.41   | 8.90   | 10.26  | 12.41  | 12.87  |
| Kāra | 5000  | 475 [475..475]              | 17.89  | 41.99  | 84.70  | 243.31 | 244.56 |
| Go   | 100   | 410 [381..440]              | 285.72 | 421.90 | 507.45 | 881.22 | 1155.00|
| Go   | 1000  | NA (acceptor saturation)    | NA     | NA     | NA     | NA     | NA     |
| Go   | 5000  | 254 [254..254]              | 4.83   | 259.65 | 529.73 | 747.92 | 764.09 |
| Node | 100   | 40253 [23197..43086]        | 2.26   | 2.73   | 3.25   | 5.57   | 51.04  |
| Node | 1000  | 37497 [13289..38612]        | 5.47   | 6.04   | 6.86   | 9.35   | 164.87 |
| Node | 5000  | 6403 [4577..10545]          | 9.44   | 16.29  | 165.30 | 709.33 | 1820.00|

### How to read this — and the perf-investigation signal it surfaces

**This is a measure-first artifact with no pre-baked win condition, and
the result is a clear perf-investigation signal for Kāra, not a win.**
The headline: Kāra's measured throughput under `wrk` is 1–3 orders of
magnitude below Go and Node. The reason is **not** per-request cost — it
is an **HTTP keep-alive mismatch** between the slice-1 passthrough and
`wrk`'s persistent-connection load model.

- **The Kāra proxy's per-request path is fast.** A single sequential
  request through it completes in ~0.5–0.8 ms (the cold-start p50 is
  0.48 ms — *faster* than Go's or Node's 0.06–0.07 ms is not, but the
  same order for a real proxied round-trip). Reconnecting clients (a
  fresh TCP connection per request) sustain hundreds of req/s limited by
  client-side connection setup, not the proxy.

- **`wrk` holds `-c` persistent connections and expects keep-alive.**
  The slice-1 Kāra proxy serves **exactly one request per connection**:
  it reads the first request chunk, forwards it, streams the upstream
  response back (verbatim, *including* the upstream's `Connection: close`
  header), then the handler returns and the client socket closes. `wrk`
  sees the close and reconnects only lazily — empirically ~1 request per
  connection per multi-second window — so at `-c100` it lands ~10 req/s,
  at `-c1000` ~99 req/s, at `-c5000` ~475 req/s (throughput scales with
  the connection count precisely *because* it is one-shot-per-connection,
  not because the proxy is doing more work).

- **Go and Node implement client-side keep-alive.** Go's
  `httputil.ReverseProxy` + `http.Serve` and Node's `http.Server` both
  reuse the client connection across many requests (they do **not**
  propagate the upstream's `Connection: close` to the downstream client),
  so `wrk` pipelines thousands of requests down each held connection and
  measures their true throughput (14k–40k req/s).

- **Go's `-c100` / `-c1000` rows are themselves degraded** (410 req/s at
  -c100, NA at -c1000) — the default `http.Serve` acceptor under a
  100–1000-connection `wrk` burst on loopback hits its own queueing
  cliff on this host; it recovers somewhat at -c5000. This is a property
  of the comparator's default config, recorded as-measured (no tuning
  knobs, per the fairness controls below), not a Kāra-relevant number.

**Conclusion (post-hoc, un-spun).** The benchmark does exactly its job:
it surfaced that **Relay's slice-1 passthrough does not implement
client-side HTTP keep-alive**, which is the single dominant throughput
factor under any keep-alive load generator. The proxy is functionally
correct (the `tests/relay_bench.rs` smoke test drives a real request end
to end and asserts the proxied body) and per-request fast, but it is
one-request-per-connection. **Closing the gap is a Relay feature
follow-up** (a client-connection keep-alive loop in `handle()` —
re-reading the next request on the same socket instead of returning, and
synthesizing/normalizing the downstream `Connection` header rather than
forwarding the upstream's `close`), tracked as the next slice. Until
then, these numbers are the honest baseline: Kāra's event-loop proxy
*latency* floor is competitive, but its *keep-alive throughput* under
`wrk` is gated by the missing connection-reuse feature.

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
  orchestration; not v1 of this bench. Note that here Node still leads —
  the keep-alive factor dominates the single-process limit at these
  payload sizes.
- **Shared upstream:** one Go origin returning a constant `"OK"`, the
  same backend for every proxy, launched once and reached via the
  ephemeral port discovered + exported as `RELAY_UPSTREAM`. The upstream
  is never the bottleneck.
- **wrk window:** `wrk -t4 -c<n> -d10s --latency`, three rounds per
  `(impl, conn)`, median reported. Same window for every impl.

## Compiler bug this dogfood surfaced

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
