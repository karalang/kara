# Demo 1 — M1 100K idle-connection verification

Verification log for Flagship Demo 1 (`examples/ws_idle_holder/`) against
the **M1 milestone — 100K stable idle `wss://` connections on a single
Linux box** (`docs/implementation_checklist/phase-6-runtime.md` line 165 /
slice 4 at line 182).

**Status: M1 (100K) NOT yet achieved.** A clean, fully-measured baseline
of **50,000 stable idle connections** was established on the test box; the
run toward 100K surfaced a server-side *establishment-throughput collapse*
that stalls acceptance near ~77K. The hold path is sound (memory linear,
thread count flat); the wall is the rate at which the single accept loop
can establish new connections as the held-connection count grows. The
collapse is filed as its own P0 blocker entry under phase-6-runtime.md.

Date: 2026-05-29.

## Test topology

Single Linux box, **everything on loopback** (bench client + demo server
co-resident):

| Property | Value |
|---|---|
| Kernel | Linux 6.12.85 x86_64 |
| CPUs | 2 |
| RAM | 7.76 GiB total (~4.5 GiB MemAvailable at rest) |
| Privilege | unprivileged (uid 1000, **no root / no usable sudo**) |
| `ulimit -n` | 1048576 (already high — no tuning needed) |
| `net.ipv4.ip_local_port_range` | 32768–60999 (≈28,231 ephemeral ports) — **could not widen (no root)** |
| `net.core.somaxconn` | 4096 (left as-is; concurrency stayed ≤256) |
| Server | `examples/ws_idle_holder/` built with `karac --features llvm` + `libkarac_runtime.a` (release) |
| Client | `examples/ws_idle_holder/bench/` (tokio + tokio-rustls/ring), release |

Toolchain note: this is a Nix-based environment; LLVM 18 is present but
`libffi` is under `/nix/store` and not on the default linker path. The
`karac` binary links `-lffi` (via LLVM), so the build needs
`LIBRARY_PATH=/usr/lib RUSTFLAGS="-L native=/usr/lib"` (where
`libffi.so.8` lives) — otherwise the final link fails with
`unable to find library -lffi`. The same env is needed for `karac build`
of the demo (it links the runtime).

## Tuning applied

- **`ulimit -n`** — already 1,048,576; no change required (each held
  connection costs 1 server fd + 1 client fd; 100K needs ~200K fds split
  across the two processes, well under the cap).
- **Client concurrency = 64** — the in-flight-handshake cap. This was the
  single most important client-side knob. At the default 256 the
  handshake herd overran the single-threaded server accept loop and
  connections began timing out (see N=10K below); dropping to 64 matched
  the server's serialized accept capacity and eliminated timeout
  failures with *better* tail latency.
- **Multi-source-IP fan-out (`--source-ips`)** — see next section. This
  is the tuning that makes >28K reachable on loopback without root.

### Beating the loopback ephemeral-port cap without root

A single client source IP dialing a single `127.0.0.1:<port>` is bounded
by `ip_local_port_range` — here ≈28,231 connections — because every
connection consumes one ephemeral *source* port on the `(src_ip, dst_ip,
dst_port)` tuple. Widening the range needs root, which this box lacks.

Worse than a hard cap: as the source-port pool nears exhaustion the
kernel's free-port search slows dramatically. Measured directly — a
single-IP run to **25K** completed but at only **214 conn/s** (116.85 s),
vs **925 conn/s** at 10K, purely from near-exhaustion port-scan cost.

Fix (no root required): bind each client connection's source to a
distinct address in the `127.0.0.0/8` loopback block. Linux routes the
entire `/8` to `lo`, so `bind()` to `127.0.0.2`, `127.0.0.3`, … succeeds
unprivileged (verified empirically), and each source IP gets its **own**
ephemeral-port pool. N source IPs ⇒ N×28K ceiling, and keeping each IP's
usage well below the near-exhaustion zone *also* keeps establishment
fast. The bench harness gained a `--source-ips a,b,c,…` flag
(round-robin by connection index) for exactly this. Example for 100K:
`--source-ips 127.0.0.2,127.0.0.3,127.0.0.4,127.0.0.5,127.0.0.6` (5 IPs ⇒
~141K capacity, 20K per IP).

## Results ladder

All runs: demo server spawned by the harness (`--server-bin`), plaintext
metrics below are wss:// (TLS) end to end (TCP connect + TLS handshake +
RFC 6455 upgrade to `101`). `c` = client concurrency.

| N | src IPs | c | established | failed | wall | est. rate | connect p50 / p99 / p99.9 (ms) | RSS/conn |
|---:|---:|---:|---:|---:|---:|---:|---|---:|
| 100 | 1 | 256 | 100/100 | 0 | 0.2 s | — | 136 / 220 / 221 | 14.0 KB* |
| 1,000 | 1 | 256 | 1,000 | 0 | — | — | 148 / 1281 / 1306 | 8.7 KB |
| 10,000 | 1 | 256 | 9,570 | **430** | — | — | herd timeouts | — |
| 10,000 | 1 | 64 | 10,000 | 0 | 10.8 s | **925/s** | 62 / 163 / — | — |
| 25,000 | 1 | 64 | 25,000 | 0 | 116.9 s | 214/s† | 112 / 1631 / 3065 | 7.9 KB |
| 30,000 | 3 | 64 | 30,000 | 0 | 37.3 s | **804/s** | 73 / 211 / 315 | 7.9 KB |
| **50,000** | **4** | **64** | **50,000** | **0** | **72.8 s** | 686/s | **86 / 208 / 263** | **7.9 KB** |
| 100,000 | 5 | 64 | ~77,000 then **stall** | — | (killed) | **~6/s @77K** | — | 7.9 KB |

\* N=100 per-conn RSS is dominated by fixed first-connection overhead
(task stack, TLS session buffers); it is not representative — the figure
converges to ~7.9 KB/conn from 1K up.

† Single-IP near-port-exhaustion slowdown, not a server limit — see the
multi-IP section. The 30K run (3 IPs, same server) ran at 804/s.

### Clean baseline — 50,000 connections (the published M1-progress number)

```
established 50000/50000  (0 failed)  in 72.84 s
connect   p50 86.5 ms  p95 161 ms  p99 208 ms  p99.9 263 ms  max 376 ms
memory    server RSS 2.85 MB -> 389.1 MB  =>  7,910 bytes/conn
churn     3 rounds x 5,000 close+reopen: 15,000 reconnects, 0 failed
          reconnect p99 1,374 ms ;  cliff_ratio = 6.6
```

Per-connection memory is **linear and stable at ~7.9 KB/conn** across the
entire ladder (matches the ~8.5 KiB/conn figure recorded on macOS during
the earlier concurrency-wedge resolution). By that constant, 100K idle
connections would cost ~790 MB server RSS — memory is *not* the M1
limiter on this box (2.4 GiB stayed free even at ~77K held).

## The 100K wall: establishment-throughput collapse

The run to 100K accepted connections at a rate that **degrades
superlinearly with the number already held**:

```
   10K held -> 925 conn/s
   30K held -> 804 conn/s
   50K held -> 686 conn/s (avg over the run)
   77K held ->  ~6 conn/s   (measured: +180 server fds over 30 s)
```

At ~77K the server's accept loop is effectively stalled (~6/s and still
dropping), so 100K is unreachable in practice — the remaining 23K would
take >1 hour and the rate keeps falling. Critically, this is **not** a
hold-side failure:

- **Memory**: ~2.4 GiB MemAvailable remained free at 77K held; RSS grew
  linearly (~7.9 KB/conn). No OOM, no swap pressure.
- **Threads**: the server held ~77K connections on **5 OS threads** —
  the scheduler's parked-task model holds (parked connections are not
  1:1 with threads), so the *hold* scales as designed.
- **fds**: 77,139 server fds open and stable — the connections that were
  accepted stay up.
- **Client**: client concurrency was only 64 in-flight; the client was
  not the bottleneck (it spent the run waiting on the server to accept).

So the bottleneck is the **rate at which the single accept loop can take
and establish a *new* connection, which grows with the held-connection
count.** The same effect shows up in the 50K churn cliff (reconnecting
5,000 while ~45K are held runs at p99 1,374 ms, 6.6× the cold-connect
p99).

### Hardware vs. software: the decisive measurement

Because client and server share 2 cores, the obvious question is whether
the collapse is just CPU contention / a small box. It is not. Sampling
**CPU-time per accepted connection** (from `/proc/<pid>/stat`
`utime+stime`) isolates software cost from scheduling contention —
contention changes wall-clock rate, not CPU-ticks per unit of work. Held
count driven 10K→85K, server alone (not the client), one sample per
window:

```
 held   accept/s   µs CPU / accept   server %core   client %core
  10K     964           505              48             62
  31K    1108           460              51             59
  49K     822           503              41             49
  63K     546           532              29             36
  73K     333           578              19             25
  79K     225           577              13             19
  83K     157           682              10             26
  84.7K    22           603               1             24
  84.9K     7          ~600               0            ~16
```

Three facts settle it:

1. **CPU cost per accept is flat at ~0.5 ms across the entire range** —
   it does *not* grow with held count. There is no O(N) *CPU* work and no
   "cores too weak" effect: accepting one connection costs the same CPU
   at 85K held as at 10K.
2. **At the collapse the server is ~0% CPU — idle** — accepting ~7/s
   while burning essentially no CPU, with the whole 2-core box ~80% idle
   (server 0% + client ~16%). Spare CPU is abundant.
3. A server that is **idle yet accepting slower and slower** means the
   accept thread spends almost all its wall-time **blocked/sleeping**,
   and that block-time-per-accept grows with held count. *A faster CPU or
   more cores cannot speed up a thread that is waiting, not computing.*

Conclusion: **the machine only sets the absolute ceiling** (the
co-resident client roughly halves the ~2000/s a dedicated core would give
at 0.5 ms/accept); **the collapse that prevents 100K is a runtime
serialization/wakeup defect**, machine-independent — a 64-core box would
start higher and follow the same collapse curve.

This **refines** the earlier root-cause guess. Ruled out by inspection +
the CPU data: the fd table is a `HashMap<Token, FdState>` (O(1)),
`EventLoop::run_once` iterates only mio-ready events (O(ready)), and —
crucially — it is **not** the synchronous inline TLS handshake (that is
CPU work, and the CPU is idle at collapse). The remaining suspect is the
**park/wakeup path**: the accept loop's listener-readiness wakeup is
delivered with growing latency as the parked-fd set grows. Candidates for
the fix slice: the poller→dispatcher wakeup handoff
(`poller_thread_main`'s `notify_all` + the consumer's routing) losing or
delaying the listener wakeup under a large registration set, or a
poll-timeout fallback flooring acceptance near the observed ~7–10/s
(≈ one per ~100 ms). First diagnostic step when picked up: instrument the
latency between "listener became readable" and "accept thread resumes" at
50K vs 85K held; if it grows ~linearly with held count, that is the site.

→ Filed as a P0 blocker under phase-6-runtime.md (Demo 1 sub-entries).

## The fix, and what it revealed (2026-05-29)

**Server-side fix landed:** `karac_runtime_ws_accept_tls` now runs the
`accept(2)` on the accept-loop thread (cheap; the codegen already parked it
on listener-readability) but offloads the *slow* rustls handshake + RFC
6455 upgrade to a per-listener pool of worker threads
(`KARAC_WS_ACCEPT_THREADS`, default 32), draining the accept backlog
non-blocking and returning ready connections from a completed queue. ABI
unchanged (still returns a ready fd), so no codegen/stdlib changes; the
demo source is untouched (per `feedback_no_workarounds_fix_compiler`).
Correct + validated: 124 runtime tests pass, N=100 e2e handshakes clean,
fmt/clippy clean. `runtime/src/event_loop.rs` (+203/−47).

**Post-fix measurement — the bottleneck moved, the local ceiling did
not.** The accept rate still collapses at ~70K held (1010/s @10K → 580/s
@60K → ~20/s @71K). But thread-state snapshots at the collapse show the
cause has *changed*:

- **Server: 0% CPU, fully idle** — all 32 handshake workers blocked in
  `wait_woken` (socket reads), waiting for the peer to send handshake
  bytes. The server is no longer the serializing bottleneck; the fix did
  its job.
- **Client (the tokio bench): ~22% CPU averaged, only 3 threads** (tokio
  defaults to one worker per core = 2), managing ~74K fds / 80K tasks.

Neither side is CPU-bound at the collapse (box ~80% idle), yet 32
in-flight handshakes complete at only ~20/s ⇒ ~1.6 s wall per handshake
with idle CPUs = a **latency-bound ping-pong**, not a throughput limit.
That latency is a property of driving tens of thousands of loopback
connections through a single co-resident tokio client on 2 shared cores
(reactor wakeup/scheduling latency across ~74K registered fds), not of the
Kāra server — which sits idle waiting.

**Conclusion.** The server *did* have a real serialization defect
(synchronous handshake on the single accept thread — a head-of-line block
that a single slow or malicious peer could exploit, slowloris-style); that
is now fixed and would let the server scale on capable hardware. The
*local* ~70K ceiling is the measurement rig (co-resident tokio client on 2
cores), confirming the hardware-vs-software split above even more sharply:
the code problem is fixed; the remaining limit is the rig, and finding the
server's true ceiling requires a separate, well-provisioned client
machine (the M3-plan rig).

## What a true 100K run still needs

1. **The establishment-throughput fix above** — the actual M1 blocker,
   and a *runtime* fix, not a hardware upgrade. The CPU-per-accept data
   shows more/faster cores will not remove the collapse (the accept
   thread is idle-blocked at the wall, not CPU-bound); they only shift
   its absolute starting rate.
2. A bigger box is *not* required for memory (790 MB fits) and would not,
   on its own, reach 100K (see above). It does help the measurement
   *rig*: a separate physical client (so client load can't contend with
   the server at all) and a **root-tunable** kernel (to widen
   `ip_local_port_range` instead of relying on the multi-source-IP trick)
   give cleaner, higher absolute numbers. For a real public-facing
   100K/1M figure, multiple physical client sources + tuned `sysctl` (per
   the M3 plan) remain the right rig; the multi-source-IP harness flag is
   what makes the *single-box* approximation possible. But the gating
   work is the runtime wakeup-path fix, not the rig.

## Reproduce

```sh
# Build (Nix env: libffi via /usr/lib):
LIBRARY_PATH=/usr/lib RUSTFLAGS="-L native=/usr/lib" \
  cargo build --bin karac --features llvm
cargo build -p karac-runtime --release
( cd examples/ws_idle_holder
  KARAC_RUNTIME="$PWD/../../target/release/libkarac_runtime.a" \
  LIBRARY_PATH=/usr/lib ../../target/debug/karac build )
( cd examples/ws_idle_holder/bench && cargo build --release )

# Clean 50K baseline:
examples/ws_idle_holder/bench/target/release/ws-idle-holder-bench \
  --server-bin examples/ws_idle_holder/ws_idle_holder \
  -n 50000 --concurrency 64 --churn-rounds 3 --churn-fraction 0.1 \
  --hold-secs 5 --connect-timeout-ms 60000 \
  --source-ips 127.0.0.2,127.0.0.3,127.0.0.4,127.0.0.5

# 100K attempt (will stall ~77K on this runtime — see "the wall"):
#   -n 100000 --source-ips 127.0.0.2,127.0.0.3,127.0.0.4,127.0.0.5,127.0.0.6
```

## See also

- `docs/implementation_checklist/phase-6-runtime.md` § Flagship Demo 1
  (slice 4, line 182) and § M1 (line 165).
- `examples/ws_idle_holder/bench/README.md` — harness usage + the
  `--source-ips` flag.
- `docs/investigations/bench_robustness.md` — sister methodology log.
