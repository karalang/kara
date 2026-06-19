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
  + `http.Serve`, the idiomatic standard-library reverse proxy, **with a
  pooled `Transport`** (`MaxIdleConnsPerHost = 10000`). The pool is
  load-bearing: the default transport caps idle upstream conns per host at
  2, which exhausts loopback ephemeral ports under load — see the package
  comment and "A note on fairness" below. Owns client-side keep-alive.
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
| Kāra |  9978 | 0.09   | 0.11   | 0.14   | 0.23   | 2.32   |
| Go   | 13883 | 0.06   | 0.07   | 0.10   | 2.18   | 5.27   |
| Node | 14359 | 0.06   | 0.07   | 0.09   | 2.54   | 5.16   |

### Steady-state (sustained `wrk` load)

| Impl | -c    | req/s (median [min..max])   | p50 ms | p75 ms | p90 ms | p99 ms  | max ms  |
|------|-------|-----------------------------|--------|--------|--------|---------|---------|
| Kāra | 100   | 37582 [36052..37990]        | 2.41   | 3.12   | 4.09   | 9.32    | 51.62   |
| Kāra | 1000  | 38654 [37965..39567]        | 23.56  | 30.78  | 39.19  | 110.74  | 445.68  |
| Kāra | 5000  | 34373 [28586..36639]        | 43.14  | 62.91  | 80.78  | 188.61  | 1890.00 |
| Go   | 100   | 41314 [40775..42879]        | 2.15   | 3.19   | 4.21   | 7.02    | 18.36   |
| Go   | 1000  | 52262 [52039..52780]        | 17.28  | 23.28  | 29.38  | 42.97   | 89.00   |
| Go   | 5000  | 16468 [11269..58660]        | 13.07  | 19.67  | 30.19  | 1040.00 | 1860.00 |
| Node | 100   | 46037 [45779..46410]        | 2.08   | 2.34   | 2.55   | 5.63    | 55.86   |
| Node | 1000  | 9398 [8943..9853]           | 11.77  | 138.97 | 483.09 | 1013.92 | 1398.05 |
| Node | 5000  | NA (single-process cliff)   | NA     | NA     | NA     | NA      | NA      |

### How to read this

**This is a measure-first artifact with no pre-baked win condition, and
the honest summary is: Kāra is competitive and uniquely stable, but not
the throughput leader.** All three impls — once each pools upstream
connections (see "A note on fairness and an earlier wrong conclusion"
below) — land in the same order of magnitude (~35–52k req/s where they
work). The differences are about *which* part of the connection sweep
each one is strong or weak at:

- **Kāra is the flattest.** 37.6k → 38.7k → 34.4k req/s from `-c100` to
  `-c5000`, with the tightest run-to-run ranges and no collapse anywhere.
  The event-loop reactor absorbs connection-count growth smoothly — the
  property the auto-concurrency story predicts (connection scaling is the
  runtime's job, not the handler's). This stability is Kāra's real,
  defensible result here.

- **Go (with a pooled transport) is the throughput leader at moderate
  concurrency** — 41k at `-c100`, **52k at `-c1000`**, beating Kāra at
  both. At `-c5000` its `httputil.ReverseProxy` gets noisy (11k–59k across
  rounds) but doesn't fully collapse. Goroutine-per-connection scales well
  here; the cost is the lifecycle and transport-pool tuning you have to
  get right (the un-tuned default is what produced the earlier wrong
  result — see below).

- **Node is fastest at `-c100`** (46k) but **collapses at higher
  concurrency** — 9.4k at `-c1000`, `NA` at `-c5000`. Single-process
  Node has one event loop and one acceptor; past ~hundreds of concurrent
  connections on this host it falls off a cliff. Cluster mode would lift
  this at the cost of process orchestration.

- **Cold start is noisy and not load-bearing.** It's a single 1 s `-t1
  -c1` probe; treat the ~10–14k req/s spread as in-the-noise, not a
  ranking. (Kāra's pre-keep-alive 0.99 req/s cold start *was* meaningful —
  it was the one-request-per-connection artifact — but that's fixed.)

- **`-c5000` on a single loopback host is a stress regime, not a clean
  data point.** At 5000 connections the `somaxconn = 128` accept backlog
  and the ~16k ephemeral-port range dominate for *everyone* (Go goes
  noisy, Node goes `NA`); only Kāra's one-persistent-upstream-connection-
  per-client design stays smooth. Read `-c100`/`-c1000` as the meaningful
  comparison and `-c5000` as "what happens past the host's limits."

**Conclusion (post-hoc, un-spun).** Where the proxies are fairly
configured and the host isn't saturated (`-c100`/`-c1000`), **Go is the
fastest, Node peaks then collapses, and Kāra sits in between — slower than
a tuned Go proxy but the most stable across the load range, and never the
one that falls over.** That is a genuinely good result for a young
language's event-loop runtime, and it comes with the ergonomic payoff the
whole demo is about: the handler is plain straight-line blocking-looking
code — no `async fn`, no goroutine lifecycle, and (unlike the Go
comparator) **no transport-pool knob you have to know to set**, because
the Kāra proxy holds one upstream connection per client by construction.
It is **not** a "Kāra is fastest" benchmark, and shouldn't be cited as one.

> ⚠️ **Caveat — loopback + a shared machine.** These numbers were taken on
> a single host over the loopback interface, with other compute
> (compiler builds) intermittently active on the box. Loopback removes the
> NIC/network but makes the client `wrk` contend with the servers for the
> same cores, and `somaxconn`/ephemeral-port limits bite at high `-c`. The
> qualitative story (Kāra competitive + most stable; Go fastest tuned;
> Node peaks then collapses) is robust across runs; the exact req/s are
> ±10–20% noisy. A separate-client/separate-server rig over a real network
> is the next step for defensible cross-language *multiples* — though for
> a 2-byte response that test becomes network-bound and would need a
> larger payload / higher concurrency to keep the proxy the bottleneck.

### A note on fairness and an earlier wrong conclusion

An earlier revision of this README reported Kāra as "competitive-to-
leading and the most stable, while Go/Node collapse to `NA`." **That
conclusion was wrong — it measured a crippled Go comparator.** Go's
`httputil.ReverseProxy` used the default transport, whose
`MaxIdleConnsPerHost = 2` made it open and discard a fresh upstream
connection per request under load, exhausting loopback ephemeral ports
(`connect: can't assign requested address`) and dropping Go to a few
hundred req/s with 502s. Giving the Go proxy a pooled `Transport`
(`MaxIdleConnsPerHost = 10000`, matching the spirit of Node's
`maxSockets: 4096` agent and Kāra's one-persistent-connection-per-client
design) took Go from `NA`/1.4k to 41k–52k req/s — and flipped the
headline. The lesson is the one this benchmark is supposed to teach about
itself: **diagnose the comparator before believing a win.**

## Fairness controls

- **Hardware:** all three proxies run on the same machine, sequentially
  (one proxy active at a time), forwarding to the *same* shared upstream
  process. Background load is the same for all.
- **Worker counts:** Kāra uses its runtime's natural reactor-thread
  default; Go uses the default `GOMAXPROCS = num_cpus`; Node runs
  single-process (its default).
- **Upstream connection pooling (made equal on purpose):** each proxy
  pools/reuses upstream connections per its own idiom — Go via an explicit
  `Transport` (`MaxIdleConnsPerHost = 10000`), Node via
  `new http.Agent({ keepAlive: true, maxSockets: 4096 })`, Kāra by holding
  one persistent upstream connection per client connection. This is the
  one knob that *must* be set fairly: leaving Go on the default
  `MaxIdleConnsPerHost = 2` is what produced the earlier wrong result
  (see "A note on fairness" above). Everything else is each language's
  default.
- **Single-process Node footnote:** Node's single-process default is
  faithful to the language's typical deployment reality. Cluster-mode
  (`cluster.fork()`) would scale ~`num_cpus`× at the cost of process
  orchestration; not v1 of this bench. Single-process is why Node hits an
  acceptor cliff at `-c1000`/`-c5000` on this host — an honest caveat on
  the comparison, not a Kāra claim.
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

## Cross-host benchmark (separate client / server)

The numbers above are **loopback** (client and servers on one machine) — fine
for "is the proxy fast and stable," but the client `wrk` contends with the
servers for cores and `somaxconn`/ephemeral-port limits bite at high `-c`. For
defensible *cross-language multiples* the rig in
[`remote/`](remote/) drives a separate-client / separate-server topology over
SSH:

```
  control (your Mac, SSH only)
       │
  client (wrk) ──network──► proxy (under test) ──network──► upstream (origin)
```

Two scripts (host-agnostic — ARM or x86 Linux):

- [`remote/provision.sh <role>`](remote/provision.sh) — runs on each Linux host;
  installs the role's toolchains (proxy → Rust + LLVM 18 + go + node; client →
  wrk; upstream → go) and builds that role's binaries.
- [`remote/bench-remote.sh`](remote/bench-remote.sh) — runs on the control
  machine; `--setup` rsyncs the repo + provisions each host, then a measure run
  sweeps **payload sizes** (`--payloads`) × connection counts, with an
  upstream-direct sanity check per payload (the upstream must out-throughput
  every proxy or it's the bottleneck) and an inter-host RTT probe.

```sh
# one-time provisioning
remote/bench-remote.sh --setup  --proxy P --client C --upstream U --user bench
# measure
remote/bench-remote.sh --proxy P --client C --upstream U --user bench \
    --payloads 0,1024,16384 --connections 100,1000 --impls k,g,n
```

**Why a payload sweep here and not on loopback:** across a real wire a 2-byte
response goes network-bound — every proxy converges on the link's RTT/bandwidth
ceiling and the proxy stops being the bottleneck. Sweeping 0 / 1 KiB / 16 KiB
keeps the proxy the variable. All four binaries take `RELAY_BIND` /
`RELAY_UPSTREAM_BIND` (routable `0.0.0.0:<port>`) and the upstream takes
`RELAY_BODY_BYTES`; unset, they default to the loopback ephemeral behavior, so
the local `bench.sh` path is unchanged.

### Cross-host results

**Measured 2026-06-19.** Three dedicated AWS **c-class ARM** instances
(Ubuntu 24.04 / aarch64 / 8 vCPU each), **same VPC + same AZ (us-east-1c)**,
client→proxy TCP-connect ~0.22 ms. Roles: client (wrk) → proxy (under test) →
upstream (Go origin), each on its own host, data plane over private IPs. Payload
sweep × `-c100`/`-c1000`, N=3 × 10 s, median req/s.

Upstream-direct sanity (client→upstream, no proxy) — the headroom check:

| payload | req/s | MB/s |
|---|---|---|
| 0 B | 430,782 | 463 |
| 1 KiB | 353,179 | 380 |
| 16 KiB | 424,676 | 457 |

Proxy throughput (req/s = median of 3):

| payload | -c | Kāra | Go | Node |
|---|---|---|---|---|
| 0 B | 100 | 138,785 | 151,991 | 158,895 |
| 0 B | 1000 | 205,365 | 210,301 | 207,419 |
| 1 KiB | 100 | 156,264 | 156,684 | 155,193 |
| 1 KiB | 1000 | 221,410 | 209,590 | 213,422 |
| 16 KiB | 100 | 164,638 | 167,939 | **207,711** |
| 16 KiB | 1000 | 217,514 | 220,042 | 220,291 |

**How to read it (honest):**

- **Absolute throughput is 4–6× the loopback numbers** (140–220k vs ~35k),
  because the client and server no longer fight for the same cores. This is the
  whole point of the separate-host rig.

- **At `-c1000` the three are statistically tied (~205–221k).** A direct
  client→upstream check sustains **392k rps at -c1000** (425k at -c100), so
  neither the client nor the network is the limit — the **proxy host's 8 cores
  are.** All three proxies saturate that CPU ceiling at ~the same level, so
  `-c1000` is a *proxy-CPU-bound* regime that doesn't discriminate between them.
  (It also retires the earlier loopback worry: nobody collapses cross-host.)

- **`-c100` is the cleaner comparison** (proxy not CPU-saturated), and there the
  honest result is: **all three are within ~5–15% of each other**, with Go and
  Node edging Kāra at 0 B, a three-way tie at 1 KiB, and **Node notably ahead on
  the 16 KiB payload** (208k vs ~165k — its `pipe()` path is efficient at larger
  bodies). Kāra is competitive throughout but is **not** the throughput leader.

- **Tails:** at 16 KiB `-c1000` every impl's p99 balloons to ~200 ms — a shared
  artifact of saturating the proxy host at large payloads, not Kāra-specific.

**Conclusion.** Cross-host confirms the corrected loopback story rather than
overturning it: **Kāra's effect-driven event-loop proxy is genuinely
competitive with mature stdlib Go/Node reverse proxies — same order of
magnitude, no collapse, within ~10% at moderate load, CPU-bound-tied at
saturation — while not being the fastest.** Given it's a young runtime and the
handler is plain blocking-looking code with no `async fn`, no goroutine
lifecycle, and no transport-pool knob, "competitive with Go/Node net/http" is
the real, defensible headline. (Caveat: a single 5 s validation round had shown
Kāra *leading* at 234k; the 3-run medians here correct that to a tie — a
reminder that single short runs are noise.)

> **Reproduce:** see `remote/bench-remote.sh` usage above. The rig was 3×
> `c7g.2xlarge`-class ARM instances; `--setup` provisions them (incl. the
> `karac` build on the proxy box) and the measure run produces the tables.

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
