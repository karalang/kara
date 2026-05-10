# Kāra HTTP layer — path to 1M+ req/s

**Status:** open. **Started:** 2026-05-10. **Owner:** unassigned.

This doc tracks the diagnostic and execution path from Kāra's
current Parallax-bench ceiling (~135 K req/s on Apple M5 Pro) to
modern hyper-class servers' practical ceiling (500 K – 1 M+ req/s
in public TechEmpower-style plaintext benches on similar hardware).
The codegen IR-opts work (`280ce2d`) closed the 1 K → 135 K gap;
the 135 K → 1 M+ gap is **all in the HTTP-handler dispatch path**
inside `runtime/src/lib.rs`.

Cross-refs:
- Parallax bench investigation: [`parallax_perf.md`](parallax_perf.md).
  Probe sweep + codegen-IR-opts findings establish the 135 K
  ceiling and rule out runtime / `karac_par_run` / frame-tracking
  as remaining bottlenecks.
- Slice E "Out of scope — closing the Kāra-vs-Rust gap" path at
  [`docs/demo_ideas.md § Slice E`](../demo_ideas.md). H2 / H3 from
  that section are this investigation's H2 / H3.
- HTTP handler ABI trampoline (predecessor): commit `5f4cbcc`. The
  shipped trampoline shape is what the hypotheses below mutate.

---

## Setup recap

**Today's ceiling.** Connection-count sweep against the Kāra
Parallax server (with `default<O2>` codegen, post-`280ce2d`):

| `wrk` config | Kāra req/s | p99 |
|---|---|---|
| `-t4 -c100`   |  94 K | 67 ms |
| `-t4 -c500`   | 105 K | 50 ms |
| `-t4 -c1000`  | 122 K | 36 ms |
| `-t4 -c2000`  | 131 K | 47 ms |
| `-t8 -c5000`  | 135 K | 71 ms (+ socket errors) |

Plateau at **~135 K req/s**. Beyond that, errors climb without
throughput climbing — server-side saturation.

**Aspiration target.** Modern `hyper` plaintext servers on M-class
hardware hit 500 K – 1 M+ req/s in public benches (TechEmpower
plaintext round, public hyper microbenches). The Kāra runtime sits
on top of `hyper 1.x` + `tokio multi-thread`, so the underlying
stack is capable; the gap is overhead in *our* dispatch path
between hyper accepting a request and the user's `handle()` fn
running.

**What we already know is NOT the bottleneck.**
- Codegen quality: shipped `280ce2d` (Loop-Idiom + DCE confirmed
  via disasm).
- `karac_par_run` thread churn: shipped `3953a14` (long-lived pool).
- `ACTIVE_FRAMES` mutex contention: ruled out (env-var probe
  `KARAC_RUNTIME_DEBUG_METADATA=0` gave 1,074 vs 1,054 — noise).
- Auto-par fan-out dispatch: ruled out (sequential vs parallel
  give same throughput at saturated load).
- Trampoline + HTTP path *as a whole*: this is exactly what we're
  investigating. The no-op-handler probe (108 K req/s) tells us
  the *combined* per-request overhead is ~9 µs at the floor; this
  investigation is about decomposing that 9 µs into individual
  hypotheses.

---

## Hypotheses — 135 K → 1 M+ gap

Ranked by **suspected-impact × tractability-of-probing**.

### H1 — `block_in_place` per request (top suspect)

**Claim.** `Server.serve(handler)` in `runtime/src/lib.rs` invokes
the user's `handle()` fn synchronously via `tokio::task::block_
in_place` (per the locked design choice in `runtime/Cargo.toml`'s
tokio-feature comment). Every request pays:

1. Tokio worker T1 picks up an HTTP request via hyper's service
   future.
2. The service calls `block_in_place(|| handler(req))`. Tokio
   marks T1 as a blocking thread + spawns a fresh worker T2 to
   take T1's place in the multi-thread runtime's worker rotation.
3. `handler()` runs on T1 synchronously.
4. `handler()` returns; T1 is "unblocked" but not necessarily
   immediately returned to the worker pool.
5. T2 keeps running other requests; T1 is recycled later.

The worker-replacement dance has measured ~1-3 µs of overhead per
call in tokio's own microbenches. At 1 M req/s target, 1-3 µs *is*
the per-request budget.

**Why it's the top suspect.** It's the only per-request overhead
that *grows with concurrency* — at higher request rates, the
worker-replacement traffic increases proportionally. Other
overheads (path-string allocation, `Request` packing) are ~constant
per request and would scale linearly with throughput; this one
adds super-linear pressure on the runtime.

**How to probe (cheap, do this first).**
1. **Count `block_in_place` calls per second under load.** Tokio
   exposes `runtime::stats::WorkerMetrics::poll_count`,
   `noop_count`, `blocking_thread_count` etc. behind
   `--features tokio_unstable`. At 130 K req/s, expect
   `block_in_place` count ≈ 130 K/s (one per request); at 1 M+
   we'd want zero or near-zero.
2. **Replace `block_in_place` with a fully-async path** in a
   prototype branch: `Server.serve` accepts a handler that returns
   `impl Future<Output = Response>`, removing the
   block-in-place dance entirely. Re-run the bench. If throughput
   doubles or triples, H1 is confirmed.

**Mitigation if confirmed.** Async-aware handler ABI. Surface
change to user code: handler signature becomes
`async fn handle(req: Request) -> Response` instead of
`fn handle(req: Request) -> Response`. The runtime side stops
calling `block_in_place`. Trade-off: every Kāra HTTP handler now
requires async support in the language (`async fn`, `.await`),
which is a larger Phase 6.3 surface. The MVP-of-MVP path is to
keep `fn` signatures (no language-level async) but have the
runtime auto-wrap into a future via `tokio::task::spawn`, paying
a different overhead but removing `block_in_place`.

### H2 — Handler trampoline + value-type packing

**Claim.** Each request pays the FFI shim cost in
`__karac_http_shim_handle`:

1. Hyper hands us `hyper::Request<Incoming>`. We extract the path
   bytes, allocate a fresh `String` (`karac_runtime_http_request_
   path`), and pack into a Kāra-native `Request` value.
2. User handler runs, returns a Kāra `Response` (`status: i64,
   body: String`).
3. We unpack the `Response`, allocate a `Bytes` from `body`, and
   build a `hyper::Response<Full<Bytes>>`.

Per-request heap allocations: 1 path `String`, 1 body `Bytes`,
plus the `Request` / `Response` Kāra structs themselves. At 1 M
req/s, that's 4 M allocations/sec just for the trampoline — the
allocator becomes hot.

**How to probe.**
1. **Time profile under load** (samply + analyze_profile.py from
   `parallax_perf.md`). Look for inclusive % on
   `karac_runtime_http_request_path`, `String::from`,
   `karac_runtime_http_response_set_body`, `Bytes::copy_from_slice`.
   Pre-fix Parallax profile already shows
   `__karac_http_shim_handle` at 70 % inclusive — much of that is
   the trampoline machinery.
2. **A/B with a trampoline-bypass handler** — emit a
   `Server::serve_raw(fn(hyper::Request) -> hyper::Response)`
   alternative entry that skips Kāra's `Request`/`Response`
   packing entirely. If a no-op `serve_raw` handler exceeds the
   135 K ceiling, H2 is real.

**Mitigation.** Three-step closure path enumerated in
`docs/demo_ideas.md § Slice E` "Out of scope":
- (i) **Borrowed accessors** — `req.path() -> ref StringSlice`
  returning a view into hyper's request buffer, no allocation.
  Requires threading hyper's request lifetime through Kāra's
  borrow checker across the FFI boundary; promotes `Request` from
  opaque heap pointer to a typed handle whose lifetime is the
  borrow root. ~2-5× expected from killing the per-request String
  alloc alone.
- (ii) **Inline the handler trampoline** — emit the shim body at
  the handler call site instead of as a separate symbol. Trades
  binary size for a saved call frame + no PLT indirection. Modest
  improvement.
- (iii) **`#[repr(C)]`-compatible `Request`** — let user code
  manipulate the same memory hyper handed the runtime, no packing
  layer at all. Largest design surface (Kāra's value-type ABI
  needs a story for "this struct is laid out by an external
  producer"); most powerful improvement.

### H3 — `Bytes`/`String` allocator pressure

**Claim.** Per-request lifecycle creates and drops a path
`String`, a body `String` (or two if the handler builds the JSON
body via interpolation), and a hyper `Bytes`. Even with a
high-perf allocator (`mimalloc` / `jemalloc`), 4 alloc/free pairs
× 1 M req/s = 4-8 M allocator calls/sec. macOS's default malloc
becomes a bottleneck above ~10 M ops/sec.

**How to probe.**
1. **`MallocStackLogging` / `heap` (macOS)** to count
   allocations per request.
2. **Switch to `mimalloc` as the global allocator** in
   `runtime/Cargo.toml` via the `mimalloc` crate. Re-run bench.
   Measured improvement under the existing trampoline shape is the
   ceiling H3 alone can deliver.

**Mitigation.** Largely subsumed by H2 fixes — borrowed accessors
eliminate the path-String allocation; `repr(C)` Request eliminates
the response-body packing allocation. After H2, H3 should shrink
to allocator microbench territory.

### H4 — Tokio runtime tuning

**Claim.** Default tokio multi-thread runtime parameters
(`worker_threads = num_cpus = 18`, default blocking pool size
512, default task budget) may not be tuned for high-throughput
HTTP. Tuning these can deliver 1.5-2× without code changes.

**How to probe.**
1. **Vary `worker_threads`** (e.g., 8, 12, 16, 18, 24, 32) and
   re-run bench. Sometimes fewer workers reduces scheduler
   contention.
2. **Disable `block_in_place` worker replacement** via
   `tokio::runtime::Builder::max_blocking_threads(0)` to confirm
   the worker-pool replacement traffic is the issue from H1.
3. **`tokio_unstable` runtime stats** to characterize task budget
   exhaustion, scheduler latency, blocking pool utilization.

**Mitigation.** Possibly just config in `runtime/src/lib.rs`'s
runtime builder. May expose a `KARAC_RUNTIME_*` env knob for
production tuning.

### H5 — HTTP/1 protocol overhead

**Claim.** HTTP/1 requires per-request parse + serialize. HTTP/2
multiplexing over fewer connections has lower per-request
protocol cost. Modern servers using HTTP/2 see 2-3× over HTTP/1
at the same hardware budget.

**Status.** Out of scope for this investigation — Phase 11
long-tail; locked design choice (i) in `runtime/Cargo.toml` is
HTTP/1 only. Mentioned for completeness; the 135 K → 1 M+ path
should not depend on it.

---

## Suggested next-session pickup order

If a single short session lands first, do **H1 step 1** (count
`block_in_place` calls under load via `tokio_unstable` runtime
stats) — gives strong signal on whether the worker-replacement
dance is the dominant contributor before any code changes.

If a longer session is available, prototype **H1 step 2** (async-
aware handler ABI bypass). Even a hacky non-shippable prototype
(skip Kāra-side compatibility, just rewire the runtime side) is
enough to validate the throughput improvement.

If the data justifies it, the H2 closure path is well-scoped and
multi-slice — borrowed accessors → inline trampoline → repr(C)
Request — and is the right next investment after H1.

---

## Out of scope (for this investigation)

- TLS / HTTPS — Phase 11 long-tail. Adds 5-10× protocol overhead
  but is orthogonal to the in-process dispatch path being
  investigated here.
- HTTP/2, HTTP/3 — Phase 11. See H5 above.
- Real database I/O — Phase 11. The bench is in-process by design.
- Connection pooling for outbound requests — out of scope; this
  investigation is about *inbound* request handling.
- Per-handler async / cooperative scheduling — Phase 6.3 territory;
  the H1 mitigation may need to interact with this surface but
  shouldn't drive its design.
- Kernel-bypass networking (DPDK, io_uring, etc.) — research-scale,
  not v1.
- Better allocator (jemalloc / mimalloc as default) — separate
  slice; cross-cutting concern, decision-point not gated on this
  investigation.

---

## Findings

_(empty — fill in as probes run; date each entry, link to commits
or supporting artifacts.)_
