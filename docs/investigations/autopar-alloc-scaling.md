# Auto-par scaling on the M5: it is allocation, not partitioning

Measured 2026-09-06 (the four results) and 2026-09-09 (everything from "The
Linux control" on, which reverses result 3's verdict), Apple M5 Pro (6P+12E),
`karac 0.1.0-dev.8222+g2923cb4f2`
(runtime source byte-identical to `main` at the time of writing — 0 commits
touched `runtime/src/lib.rs` between that revision and `4dba34463`).

Probes live in [`autopar-alloc-scaling/`](autopar-alloc-scaling/). Every one is
a **single admitted parallel region** (verified with `KARAC_COST_DEBUG=1`) with
**uniform per-iteration cost**, so load imbalance cannot explain any result
here. Each was A/B-verified identical across `karac run`, `KARAC_AUTO_PAR=0
karac build` and the default auto-par build before being timed.

## Why this exists

`B-2026-08-28-76`, `B-2026-09-05-22` and `B-2026-09-05-23` were all filed off
**kata:282** and **kata:288**. Both katas are allocation-heavy *and*
non-uniform: 282 is an exponential recursive search that builds two Strings per
node, 288 allocates a String per punch. Filing from those two alone confounded
four separate variables — the dispatch machinery, the static partition, core
heterogeneity, and allocation. These probes vary one at a time.

## The four results

### 1. The dispatch machinery and static partition are fine

`uniform.kara` — arithmetic only, no allocation, one region:

| N | wall | user | sys |
|---|---|---|---|
| 1 | 45.60 ms | 43.38 | 1.27 |
| 2 | 23.78 ms | 43.87 | 0.98 |
| 4 | 13.28 ms | 45.19 | 1.05 |
| 18 | 4.76 ms | 45.14 | 1.27 |

**9.58× at N=18 on a 6P+12E host, with user CPU flat to 4%.** N=2 gives a clean
1.92×. 9.58× is close to the ~10.2 an asymmetric 6P+12E machine can offer if an
E core retires roughly a third of a P core, so the static equal-count split is
*not* leaving meaningful parallelism on the table.

This refutes `B-2026-08-28-76`'s leading hypothesis — "a static equal-count
partition sized to `available_parallelism()` meeting 12 efficiency cores".
The same partition, on the same host, scales essentially as well as the hardware
allows the moment the workload stops allocating.

Under background QoS (`taskpolicy -b`, all-E placement) the same probe is also
healthy — N=2 → 1.96×. So background QoS is not itself pathological, which
retires a confound in `B-2026-09-05-22`'s original evidence.

### 2. Allocation is what collapses it — and N=2 specifically

`alloc3.kara` — identical shape and uniform cost, but two short-lived Strings
per inner step via `substring` + concat, **no formatting**. 30 runs:

| N | wall | user | sys |
|---|---|---|---|
| 1 | 8.72 ms | 7.64 | 0.61 |
| **2** | **52.11 ms** | **100.25** | 1.14 |
| 3 | 5.55 ms | 10.48 | 0.62 |
| 18 | 3.96 ms | 22.97 | 1.57 |

**N=2 is 6.0× slower than N=1 and 9.4× slower than N=3**, burning 13.1× the
user CPU with essentially no system time — pure user-space burn. N=3 recovers
completely. `sample` at N=2 puts `_xzm_free` (libsystem_malloc) at the top of
stack with 2383 samples against 770 at N=3.

This is `B-2026-09-05-22`, reproduced without formatting, without imbalance,
without core-placement games, at normal QoS, in 22 lines.

### 3. It is Kāra-specific, but libmalloc bounds everyone

> **SUPERSEDED by "The M5 answer" below (2026-09-09).** The "Kāra-specific"
> half of this section is WRONG, and wrong because of the control below rather
> than the
> measurement: `mt_malloc.c` does two malloc/free pairs per step where the
> compiled Kāra probes do one. At one pair the same C program collapses at two
> threads exactly as Kāra does. The libmalloc-ceiling half stands. Read
> that section before quoting anything here.

`mt_malloc.c` — N pthreads, same allocate/copy/free pattern, no shared state,
total allocation count held constant:

| threads | wall | user |
|---|---|---|
| 1 | 117.26 ms | 114.46 |
| 2 | 73.13 ms | 140.50 |
| 4 | 52.65 ms | 183.50 |
| 18 | 57.51 ms | 870.68 |

**Two threads in C is 1.60× faster, not 6× slower** — so the N=2 collapse
belongs to Kāra, not to macOS malloc, and `B-2026-09-05-22` stands as a real
bug with a corrected mechanism.

*(That inference is the one section 6 retracts: this table's two-pairs-per-step
body is not the body Kāra runs.)*

But note the right-hand column: past 4 threads C's wall time goes **flat** while
user CPU grows **linearly** (114 → 871 ms). macOS libmalloc does not scale this
pattern in *any* language. That is a ceiling on what auto-par can ever return
for allocation-heavy work on this host, and it should be stated as a limit
rather than chased as a Kāra defect.

### 4. The system-time explosion is `snprintf`, not page churn

`alloc2.kara` is `alloc3.kara` with the Strings built by f-string
interpolation (`f"n{acc}-{k}"`) instead of `substring`:

| N | wall | user | **sys** |
|---|---|---|---|
| 1 | 22.98 ms | 21.63 | 0.78 |
| 2 | 58.93 ms | 111.93 | 2.63 |
| 6 | 67.12 ms | 146.03 | 227.22 |
| 18 | 47.15 ms | 86.66 | **579.80** |

`alloc3`, which differs *only* in not formatting, holds sys at 1.57 ms at N=18.

`sample` on the f-string version: `__ulock_wait2` 15070, `__ulock_wake` 7516,
`__psynch_cvwait` 5195, then `__vfprintf` 840, `os_unfair_lock_lock` 539,
`_os_unfair_lock_lock_slow` 464, `__v2printf` 186, `__ultoa` 173,
`localeconv_l` 140. `/usr/bin/time -l` shows page reclaims **flat** (267 → 299)
and page faults **falling** (10 → 2).

So `B-2026-09-05-23`'s recorded mechanism — "allocator arena / page churn …
per-thread magazine growth and madvise / mach_vm traffic" — is **refuted**. The
cause is that **f-string interpolation lowers to libc `snprintf`**, which
serializes on locale and lock state across threads. Every parallel Kāra program
that formats a string pays this.

This is the most tractable of the three: a lock-free integer/float formatter in
the runtime, used by the f-string lowering instead of `snprintf`, removes it
without touching the pool or the partition.

### 5. The same bug survived in the SPEC'D f-string path

`B-2026-09-05-23` moved `f"{n}"` off `snprintf` but left `f"{n:5}"` on it, so
the two spellings sat ~23x apart. `spec.kara` is `alloc2.kara` with width specs
on the holes and nothing else changed:

| N | snprintf | via `karac_runtime_fmt_int` | allocation-free fast path |
|---|---:|---:|---:|
| 1 | 23.71 ms | 97.73 ms | **6.58 ms** |
| 18 | 46.71 ms | 35.32 ms | **2.16 ms** |
| sys @ 18 | 553.46 ms | 4.78 ms | **1.12 ms** |

The middle column is the obvious fix that does not work, and it is worth
keeping: routing spec'd holes to the EXISTING runtime formatter removes the
lock but is **4.1x slower single-threaded**, because that entry point re-parses
the spec string and returns a `String` from `FormatSpec::apply_int` on every
call. Codegen knows the spec at compile time, so the fix that works passes the
decoded fields as constants and renders straight into the caller's buffer —
no parse, no allocation, no lock. Fixed in `B-2026-09-07-24`.

## The Linux control: the collapse is macOS-only

Measured 2026-09-09, x86_64 Linux container (4 cores), **glibc 2.39**, `karac`
at `3f2342ca9` with the runtime archives rebuilt at that revision. Same probe
sources, same `KARAC_PAR_WORKERS` sweep, `bench.py` (min of 10 after 2 warmups)
in place of hyperfine.

This is the "homogeneous many-core Linux control" the section below lists as
missing. It answers a question the M5 cannot: whether the two-active-worker
collapse belongs to Kāra's allocation shape as such, or to that shape meeting
**macOS libmalloc** in particular. glibc's ptmalloc2 is a different allocator
with a different threading model (per-thread arenas rather than magazines), so
running the identical Kāra binaries against it isolates exactly that variable.

**It does not reproduce. Two active workers are a SPEEDUP on glibc.**

| probe | active workers | N=1 | pool=2 | pool=3 | pool=4 | pool=8 | pool=18 |
|---|---|---|---|---|---|---|---|
| `decouple2` | exactly 2 | 8.40 ms | 5.35 | 5.10 | 5.19 | 5.73 | 5.97 |
| `decouple3` | exactly 3 | 8.15 ms | — | 4.04 | 4.12 | 4.74 | 4.75 |
| `alloc3` | pool | 8.61 ms | 5.43 | 4.37 | 4.50 | 3.71 | — |
| `uniform` | pool | 86.47 ms | 44.47 | 30.36 | 23.15 | 24.20 | — |

`decouple2` runs **0.61-0.71x of N=1 at every pool size** — against
**5.83-5.90x SLOWER** at pool 2, 3 and 18 on the M5. The `decouple2` /
`decouple3` ratio, which is the sharpest form of the anomaly, is **1.26x** here
(5.10 vs 4.04 at pool=3) against **8.0x** there (54.61 vs 6.81); and 1.26x is
just the expected cost of splitting the same work two ways instead of three, on
a box with cores to spare.

The C controls replicate on glibc too, so the negative control travels rather
than being a macOS artifact: `mt_malloc` is 0.51x at two threads (127.7 → 65.1
ms) and `mt_malloc_main` 0.53x (121.8 → 64.1).

**Consequence for the row.** The trigger is not Kāra's allocation shape on its
own — glibc is untroubled by the identical shape at the identical worker
counts. It is that shape *meeting macOS libmalloc*, which is also consistent
with the M5 signature (`mach_absolute_time` top-of-stack out of
`libsystem_malloc`, absent at three workers). The two surviving candidates —
size class and free path — are therefore **allocator-interaction** questions to
settle on macOS, not portable defects.

## Kāra's actual allocation shape, measured

The counting probe `B-2026-09-05-22` records as ABANDONED is tractable on
Linux, and [`autopar-alloc-scaling/mcount.c`](autopar-alloc-scaling/mcount.c)
is it. The macOS DYLD interposer hit self-interposition recursion and a
pre-constructor null pointer, and cost ~2500x once both were fixed — enough to
change the contention behaviour under study. `LD_PRELOAD` avoids all three:
`dlsym(RTLD_NEXT, ...)` for the real symbols, a static bootstrap arena for
dlsym's own `calloc`, and **thread-local** counters summed only at exit, so the
hot path is one TLS increment with no atomic and no lock. Verified against a
known-answer program before use.

`decouple2` at `pool=2`, 432000 inner steps:

```
malloc=432021  calloc=6  realloc=3  free=432016  bytes=1303647
432000 requests in one bit-length bucket; mean 3.0 B
```

**One malloc per inner step, every one of them exactly 3 bytes** — not the two
per step of 1-10 bytes that `mt_malloc.c`, `mt_malloc_main.c` and
`mt_malloc_k.c` all assumed from reading the probe source. Kāra allocates
*half* as often as the control it is being compared against, which refutes
allocation density from the opposite direction to `mt_malloc_k.c`'s sweep.

Which call allocates was measured rather than inferred — three variants of one
100000-step loop with a loop-carried (non-foldable) length `n = 1..4`:

| body | mallocs | size histogram |
|---|---|---|
| `substring` only | 100033 | 25000 @ 1 B · 50000 @ 2-3 B · 25000 @ 4 B |
| `substring` + one concat | 100033 | identical |
| `substring` + two concats | 100033 | identical |

Byte-identical across all three: the **substring's buffer is the only libc
allocation**, and the concats grow it without ever reaching the allocator (the
request is `n` bytes and malloc's usable size covers the appended bytes). Two
consequences worth carrying: adding concats to a Kāra loop adds no allocator
traffic at all, and `alloc3`/`decouple2`'s "two short-lived Strings per step"
comment describes the source, not the behaviour.

One probe limitation this exposed: `decouple2`'s `acc` reaches a fixed point at
`acc % 8 == 2`, so the intended 1..8 length spread collapses to a constant. The
probe exercises exactly **one** size class, not eight — which matters before
reading anything into size-class effects.

[`autopar-alloc-scaling/mt_malloc_shape.c`](autopar-alloc-scaling/mt_malloc_shape.c)
is the control that matches the measured shape: one malloc + free per step at a
settable size, defaulting to the measured 3 bytes. On glibc it scales cleanly
(N=1 74.7 ms → N=2 38.1 → N=3 26.7 → N=4 20.3). **Running it on the M5 is the
decisive next step**: if 3-byte single-allocation steps are fast there at two
threads, size class and density are both fully refuted and the free path is
what remains.

## The M5 answer: the C control collapses too, so it is not Kāra's

Measured 2026-09-09 on the M5, `karac` built from `2a23fc7b6` with a matched
archive pair, `libsystem_malloc.dylib` 812.160.5. This runs the decisive step
the section above asks for, and the answer is the opposite of both its branches:
**3-byte single-allocation steps are NOT fast at two threads on macOS**, so what
is refuted is not the size class but the premise that this belongs to Kāra.

`mt_malloc_shape.c`, the control written for exactly this, on the M5:

| size | 1 thread | **2 threads** | 3 threads | 4 threads |
|---|---|---|---|---|
| 3 B (the measured size) | 59.94 ms | **463.15 ms** | 27.52 ms | 28.43 ms |
| 32 B | 69.73 ms | **467.14 ms** | 32.20 ms | 27.12 ms |
| 512 B | 102.83 ms | **398.41 ms** | 43.40 ms | 37.07 ms |

**7.7× slower at two threads than at one, 16.8× slower than at three — in a C
program with no Kāra in it**, at the shape and size `mcount.c` measured. The
same cell reached independently through `mt_malloc_k.c` at K=1, the density
value its recorded sweep (K = 2, 4, 8) never ran: 113.83 ms → **948.96** →
49.15 → 51.70 (4 threads) → 43.93 (6). `sample` there gives `_xzm_free` 1031,
`__ulock_wait` 676, `_xzm_xzone_malloc_tiny` 146, `mach_absolute_time` 65 —
the signature this investigation recorded for Kāra.

[`autopar-alloc-scaling/mt_pair1.c`](autopar-alloc-scaling/mt_pair1.c) bounds
it, holding total allocation volume constant:

| configuration (1 pair/step) | 1 thread | **2 threads** | 3 threads |
|---|---|---|---|
| size varies 1..8 | 119.20 ms | **948.91** | 50.63 ms |
| size fixed 8 | 119.97 ms | **960.77** | 54.27 ms |
| size fixed 64 | 123.29 ms | **779.35** | 57.67 ms |
| size fixed 1024 | 195.52 ms | **869.53** | 82.79 ms |
| compute between the alloc and the free | 157.15 ms | **989.94** | 68.19 ms |

- **Not the size class.** 1 byte through 1 KiB all collapse — so the one-size-
  class limitation of `decouple2` noted above, real as it is, was never load-
  bearing for this row's conclusion.
- **Not thread phase alignment.** Work inserted between the `malloc` and the
  `free` changes the timing without changing the allocation stream; it does not
  help.
- **Not the thread count.** Two allocating threads: 853 ms alone, 977 ms with
  one extra idle thread, 881 ms with sixteen — `decouple2`'s pool-size
  independence, reproduced in C.
- **It is per-size-class state.** Give the two threads a size class each and the
  collapse largely lifts: 853 ms → 167 ms.

The Linux control travels to this shape as well: `mt_pair1` under **arm64**
Linux/glibc on this same machine (colima, `gcc:14`) scales cleanly at one pair
per step — 49 → 24 → 17 → 13 ms across 1-4 threads, matching the x86 result
above from the other side of the architecture.

And the Kāra side, re-measured here rather than quoted, on this tree:

| probe | N=1 | **N=2** | N=3 | N=18 |
|---|---|---|---|---|
| `decouple2` (iter_total = 2, always 2 active) | 8.79 ms | **52.70** | 53.19 ms | 52.54 ms |
| `alloc3` (iter_total = 720) | 10.62 ms | **52.49** | 5.52 ms | — |

### One correction to the shape measurement

The `otool -tvV` disassembly of `___karac_reduce_worker_0` agrees exactly with
`mcount.c` on the shape — one `karac_alloc_or_panic` / `memcpy` /
`karac_free_buf` triple per inner step — but it also shows *why*, and the
mechanism is not the one inferred from the identical histograms:

```
    add  x9, x23, #0x1      ; t.len() folded to n + 1
    adds x21, x8, x9        ; ...and that is all `t` was ever used for
```

The concat is **dead-code eliminated**: no buffer is allocated for it at all,
rather than its bytes being absorbed into the substring's usable size. So the
correct statement is narrower than "adding concats to a Kāra loop adds no
allocator traffic" — a concat whose result is *live* allocates
(`src/codegen/expr_ops.rs`, `BinOp::Add`, `malloc(l_len + r_len)`). In these
probes it never is.

### What it means

Two concurrent allocators, each holding exactly one small block live at a time
in the same size class, are pathological in macOS libmalloc **in any language**.
Kāra's exposure is real but narrow: the default worker count is
`available_parallelism()`, so a program runs exactly two *active* workers only
when a parallel region's `iter_total` is 2, or under an explicit
`KARAC_PAR_WORKERS=2`.

Nothing in the partitioner or the pool addresses it. The one Kāra-side move the
measurements support is a per-thread small-block free list in the runtime, which
would keep these allocations off libmalloc's shared per-size-class state
entirely — filed as `B-2026-09-09-6`, sized against what a two-iteration
parallel region is worth rather than against this row's 6×.

## What this leaves open

- **`B-2026-09-05-22`** — CLOSED `wontfix`. Two Linux controls narrowed it to
  an interaction with macOS libmalloc; running the shape-matched C control on
  the M5 finished the job by reproducing the collapse with no Kāra involved.
  What remains is not this row but a possible mitigation — a per-thread
  small-block cache in the runtime (`B-2026-09-09-6`).
- **`B-2026-08-28-76`** — its stated hypothesis is refuted, but the katas do
  collapse. On this evidence the cause is (2) and (4) above plus the libmalloc
  ceiling in (3), not the partition. The homogeneous Linux control this listed
  as missing has now been run (see above) — on 4 container cores, which settles
  the allocator question but not the many-core scaling one.
- The `order_free` path already has heterogeneity-aware dynamic chunking
  (`karac_par_reduce_pooled`, `KARAC_PAR_CHUNK_FACTOR`, default 8). Neither
  kata is order-free, so neither uses it. Whether extending it to ordinary
  reductions helps is untested — and on this evidence it would not address the
  actual cause.

## Reproducing

```sh
cd docs/investigations/autopar-alloc-scaling
karac build uniform.kara -o u_par
sh sweep.sh ./u_par "" 10          # add "taskpolicy -b" as $2 for all-E
clang -O3 mt_malloc.c -o mt_malloc -lpthread && sh csweep.sh
clang -O3 mt_pair1.c -o mt_pair1 -lpthread
./mt_pair1 2 1                     # the collapsing cell: 2 threads, 1 pair/step
./mt_pair1 3 1                     # recovers
./mt_pair1 2 2                     # 2 pairs/step: no collapse
sh prof.sh 2 a3l s_n2.txt          # top-of-stack profile at a worker count
```

On Linux (no hyperfine, no `taskpolicy`, no `sample`):

```sh
karac build decouple2.kara -o d2
KARAC_PAR_WORKERS=3 RUNS=10 python3 bench.py ./d2

gcc -shared -fPIC -O2 -o mcount.so mcount.c -ldl
KARAC_PAR_WORKERS=2 LD_PRELOAD=./mcount.so ./d2      # malloc/free counts + sizes

clang -O3 mt_malloc_shape.c -o mt_malloc_shape -lpthread
./mt_malloc_shape 2 3                                 # 2 threads, 3-byte steps
```

## The collapse reproduces on homogeneous Linux cores, on demand

Measured 2026-09-10 (B-2026-08-28-76, Group D), x86_64 Linux container, **4
homogeneous Intel Xeon @ 2.10GHz cores, no SMT**, Linux 6.18.44, **glibc 2.39**,
16 GB. `karac` at `e898cb1` with **both** runtime archives rebuilt at that
revision — the lean archive on disk predated `6cc0298` and `2ed5deb`, two
Map-probe runtime commits, and `karac` links the *lean* archive for these
programs, so measuring the tree as found would have silently benchmarked the
old probe on a Map-heavy workload. No hyperfine on this host: medians over the
stated run counts, child stdout to `/dev/null`, user/sys from `wait4` rusage.

The section above concludes that the two-worker collapse "is not Kāra's
allocation shape on its own — glibc is untroubled by the identical shape". That
is true and it is worth being precise about *why* glibc is untroubled: it
supplies per-thread allocation caching (an arena per thread, plus tcache) that
the Kāra runtime does not have. `karac_alloc_or_panic` → `karac_alloc_fallible`
→ plain `malloc`, per object, on every worker thread; the only cache in
`runtime/src/alloc.rs` is the ≥ 1 MiB large-buffer recycler, which small String
churn deliberately never touches.

**Take that caching away and the collapse reproduces here** — same binary, same
static partition, homogeneous cores, four of them.

### kata:288, `KARAC_PAR_WORKERS=4`, three interleaved passes of 15 runs

| lane | wall | user CPU | vs own seq | user infl |
|---|---|---|---|---|
| sequential twin (`KARAC_AUTO_PAR=0`) | 86.69 ms | 83.71 ms | 1.00x | 1.00x |
| auto-par, default arenas | **27.56 ms** | 89.91 ms | **3.15x** | 1.07x |
| auto-par, `MALLOC_ARENA_MAX=1` | **68.75 ms** | 208.49 ms | **1.26x** | **2.49x** |
| auto-par, `glibc.malloc.tcache_count=0` | 41.42 ms | 143.14 ms | 2.09x | 1.71x |

Per-pass spread was 26.3–28.8 (default), 63.0–74.0 (arena1), 39.8–44.2
(tcache0) — the ordering never changes. An earlier non-interleaved tcache0 cell
read 91.63 ms; three interleaved passes do not reproduce it and the table above
supersedes it.

**1.26x for four cores at 2.49x user-CPU inflation is this row's M5 signature**
(1.08x for 15.7 cores at 3.15x inflation), reproduced with no heterogeneity, no
many-core machine, and no macOS.

### The worker sweep, which is where it is clearest

| N | default arenas | user | `MALLOC_ARENA_MAX=1` | user |
|---|---|---|---|---|
| 1 | 85.18 ms | 84.02 | 79.69 ms | 78.52 |
| 2 | 48.97 ms | 90.27 | 64.22 ms | 105.27 |
| 3 | 32.41 ms | 84.03 | 73.43 ms | 161.80 |
| 4 | 26.84 ms | 85.67 | 71.22 ms | 198.97 |

Default: near-linear, user CPU **flat** (84.02 → 85.67). Starved: wall time gets
*worse* from N=2 on while user CPU climbs monotonically to 2.53x. That is the
row's "NO WORKER COUNT RECOVERS IT", on this box, on demand.

### kata:282 goes past flat into net loss

| lane | wall | user |
|---|---|---|
| seq | 886.03 ms | 883.82 |
| par, default arenas | 222.85 / 237.83 / 254.64 ms | 860.62 / 901.29 / 942.62 |
| par, `MALLOC_ARENA_MAX=1` | **1375.65 / 1334.22 ms** | 4099.86 / 3117.91 |
| seq, `MALLOC_ARENA_MAX=1` | 873.96 ms | 873.54 |
| seq, `tcache_count=0` | 1175.00 ms | 1170.63 |
| par, `tcache_count=0` | 576.40 ms | 2157.78 |

Starved of arenas, kata:282's parallel lane is **0.65x of its own sequential
twin** — a net loss, which is where `uniqueabbr_par.c` sits on the M5 (0.76x).

`MALLOC_ARENA_MAX=4` — one arena per core — does **not** restore it either
(288: 67.73 ms, 282: 532.09 ms), and it fails differently: CPU utilisation
*drops* (133% / 182%) with user CPU near flat, i.e. workers blocking on the
arena mutex rather than burning cycles. Arena1 shows both blocking and burn.

### The controls that make it a mechanism rather than a slowdown

The sequential twins barely move under arena starvation — 288 seq 87.58 → 92.05
ms, 282 seq 886.03 → 873.96 ms. `MALLOC_ARENA_MAX` is a *concurrency* knob and
it costs a single-threaded program nothing, so the entire effect is contention
between workers, not a slower allocator. (`tcache_count=0` is not a clean
control in this respect — it costs the sequential lane 1.18x on 288 and 1.33x
on 282 — which is why the arena knob is the one to quote.)

### What this changes

Nothing about the M5 measurements, and nothing about `B-2026-09-05-22`'s
finding that macOS libmalloc collapses on a shape glibc handles. What it changes
is the **reading**. "A platform ceiling that is nobody's bug" is too generous to
the compiler: Kāra's auto-par throughput on allocation-heavy work is a function
of a host-allocator property Kāra neither provides nor requires. glibc happens
to provide it, libmalloc happens not to for this shape, and one env var moves
Linux to the wrong side of that line. Go — the only comparator that scales on
the M5 — has a per-P allocator cache, which is the same property held
internally rather than borrowed.

It also gives **`B-2026-09-09-6`** (per-thread small-block free list, closed
`wontfix`) the test bed it never had. That row measured its prototype **2.76x
slower on kata:288** and concluded the TLS access cost more than the malloc it
replaced — measured on hosts where the host allocator was *already* doing the
job, so the prototype could only add cost. `MALLOC_ARENA_MAX=1` on Linux is a
contended allocator reachable in one env var, and it is the lane where such a
cache has something to win. A re-test there is cheap and would separate "the
idea is wrong" from "the idea was measured where it could not pay".

### The C mirror is immune here, and that corrects an earlier reading

Same-host comparators, `clang -O3`, two interleaved passes of 15 runs:

| lane | pass 1 | pass 2 | user |
|---|---|---|---|
| C seq | 58.28 ms | 67.00 ms | 57.62 / 65.41 |
| C par, default arenas | 18.54 ms | 19.77 ms | 64.28 / 64.30 |
| C par, `MALLOC_ARENA_MAX=1` | **17.94 ms** | **18.62 ms** | 62.44 / 61.61 |
| Kāra par, default arenas | 27.53 ms | 24.91 ms | 92.62 / 83.49 |
| Kāra par, `MALLOC_ARENA_MAX=1` | **75.50 ms** | **70.75 ms** | 214.66 / 194.24 |
| Kāra seq | 87.60 ms | 90.45 ms | 84.72 / 87.04 |

The arena knob reproduces the **Kāra** half of the signature and not the C half:
`uniqueabbr_par.c` does not move. The reason is checkable and decisive — **that
file contains no dynamic allocation anywhere**. No `malloc`, `calloc`,
`realloc`, `strdup`, `aligned_alloc` or `mmap` appears in it; the table is a
fixed `slot tbl[TABLE_SZ]` of inline `char key[MAXW]` arrays and the worker
formats into a stack buffer (`char a[MAXW]`). A program with no allocator
traffic cannot contend on the allocator, so one arena costs it nothing.

**This refutes the sentence above** — "the pthreads mirror and kara collapse
together on kata:288 not because they share a partitioning strategy, but because
they share an allocator." They do not share an allocator problem, because the
mirror does not use the allocator. Whatever puts `uniqueabbr_par.c` at 0.76x on
the M5, malloc contention is not it, and `B-2026-08-28-76` leans on that C row
as its single most important piece of evidence.

**A hypothesis for the M5, not a finding here.** `abbrev` in that C file is
`sprintf(out, "%c%zu%c", ...)`, once per punch, a million times, across 18
threads. `B-2026-09-05-23` already measured libc `snprintf` serializing on this
exact kata on macOS, worth the difference between 1.08x and 3.45x on the *Kāra*
lane. The C mirror calls the same family in the same loop at the same rate. If
the M5's C collapse is `sprintf` rather than the partition or the allocator,
then all three witnesses reduce to causes already named and the C row stops
cutting against a Kāra-side diagnosis. It is minutes to test on an M5 — replace
that `sprintf` with manual digit formatting and re-run the par lane. Not
testable here: no macOS, and on glibc the C par lane has no collapse to remove.

Kāra's par lane is **not** at parity with C on this host — 24.9–27.5 ms against
18.5–19.8 ms, ~1.35x slower — against a mirror that allocates nothing while
Kāra allocates a String per punch.

### Still out of reach here

A **homogeneous many-core** control. This container is 4 cores, the same width
as the lane already in the corpus, so it cannot separate core count from
heterogeneity. That measurement still needs a wide homogeneous Linux box.

### Reproducing this section

```sh
cd kara-katas/leetcode/201-300/288-unique-word-abbreviation/bench
karac build uniqueabbr.kara -o /tmp/u_par
KARAC_AUTO_PAR=0 karac build uniqueabbr.kara -o /tmp/u_seq
sh docs/investigations/autopar-alloc-scaling/arena_sweep.sh /tmp/u_par /tmp/u_seq
```

Rebuild **both** runtime archives first if `git log <archive-revision>..HEAD --
runtime/src` is non-empty; these katas link the lean one.

The C comparators in that table are built from the kata's own sources:

```sh
clang -O3 uniqueabbr.c     -o c_seq
clang -O3 uniqueabbr_par.c -o c_par -lpthread
python3 docs/investigations/autopar-alloc-scaling/timeit.py --runs 15 --warmup 3 \
    --env MALLOC_ARENA_MAX=1 ./c_par
```
