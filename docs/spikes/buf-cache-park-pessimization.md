# Buffer-cache park pessimization (B-2026-07-30-4)

The large-buffer recycling cache in `runtime/src/alloc.rs` (`mod buf_cache`,
gated on `KARAC_BUF_CACHE`, park threshold `BUF_CACHE_MIN_BYTES` = 1 MiB) is a
**6.79x win** on the workload it was designed for and a **1.75-1.84x loss** on
LeetCode #3629's sieve. Both probes are preserved here because the ledger entry
names them as the two regression oracles any fix must satisfy simultaneously.

Measured 2026-07-31 on a quiet x86-64 Linux container, karac built from main
`9b9d397`, seq lane (`KARAC_AUTO_PAR=0` on every kara build).

## Results

Real kata (`bfs_sieve`, sink 24350 identical in all three):

| build | time | vs C |
|---|---|---|
| kara, cache ON | 16.879 s ± 0.395 | 1.84x |
| kara, cache OFF | 9.982 s ± 0.254 | 1.09x |
| C, `clang -O3` | 9.170 s ± 0.219 | — |

Sieve alone, cap 999983, K=50 (sink 50 in all three):

| build | time | vs C |
|---|---|---|
| kara, cache ON | 15.510 s ± 0.129 | 1.75x |
| kara, cache OFF | 9.172 s ± 0.268 | 1.03x |
| C, `clang -O3` | 8.876 s ± 0.998 | — |

`bigbuf` — the cache's INTENDED shape, one 24 MB Vec built and dropped 200
times with nothing else on the heap (`take hit=199 miss=1`):

| build | time |
|---|---|
| cache ON | 307.2 ms |
| cache OFF | 2.087 s (System 1.830 s of it) |

The cache is 6.79x faster here: it elides an mmap/munmap + page-zero cycle per
iteration. **A fix must keep this win and remove the sieve loss.**

## Why the sieve is different

The park decision reads only the buffer's SIZE. In `bigbuf` nothing happens
between park and take. In the sieve, ~1e6 small allocations happen in between —
the outer `Vec[Vec[i64]]` buffer is `(cap+1) x 24 B` ~= 24 MB, 24x over the park
threshold, and `KARAC_BUF_CACHE_STATS=1` shows exactly one park per iteration
(`take hit=4 miss=1 | put parked=5` at K=5). The ~1e6 inner buffers (32-48 B)
never touch the cache: `karac_free_buf` short-circuits a positive hint below
`BUF_CACHE_MIN_BYTES` straight to libc `free` (alloc.rs:526), and the alloc side
does the same (alloc.rs:58).

The glibc-level mechanism — why retaining a 24 MB parked buffer across 1e6 small
allocations costs ~7 s of USER time while saving ~0.6 s of system time — is
**not established**, and is deliberately left as an open question rather than a
story. See the ledger entry for the full evidence chain and for the four
hypotheses this round refuted.

## Probe caveat

The first version of `bigbuf.kara` read only `v[0]`, so LLVM deleted the entire
allocation: 200 iterations "ran" in 1.3 ms and the cache counters stayed empty.
**Always confirm `KARAC_BUF_CACHE_STATS=1` reports non-zero park/take counts
before believing a buf_cache measurement.**

## Probes

### `sieve_only.kara`

```kara
// B-2026-07-30-4 probe: the sieve ALONE, no BFS.
// Same cap and K as the #3629 bench, so the Vec[Vec[i64]] many-small-buffers
// pattern is reproduced at full scale: ~1e6 inner Vecs, ~2.85e6 pushes,
// built and dropped 50 times.

fn build_factors(cap: i64) -> Vec[Vec[i64]] {
    let mut factors: Vec[Vec[i64]] = Vec.filled(cap + 1, Vec.new());
    for i in 2..=cap {
        if factors[i].is_empty() {
            for j in (i..=cap).step_by(i) {
                factors[j].push(i);
            }
        }
    }
    factors
}

fn main() {
    let cap = 999983;
    let mut sum = 0;
    for _ in 0..50 {
        let f = build_factors(cap);
        sum = sum + f[cap].len();
    }
    println(sum);
}
```

### `sieve_only.c` (mirror; growth policy byte-identical to codegen's push arm)

```c
/* B-2026-07-30-4 probe: C mirror of sieve_only.kara — the sieve alone.
 * IntVec growth policy is byte-identical to kara codegen's push arm:
 * new_cap = (cap == 0 ? 4 : cap * 2), grown with realloc.
 *
 * Compile-time switch PROBE_NO_FREE=1 leaks instead of freeing, which
 * isolates the cost of the 1e6-buffer free walk. */
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>

typedef struct {
    int64_t *data;
    size_t len;
    size_t cap;
} IntVec;

static void intvec_push(IntVec *v, int64_t x) {
    if (v->len == v->cap) {
        size_t new_cap = v->cap == 0 ? 4 : v->cap * 2;
        int64_t *new_data = (int64_t *)realloc(v->data, new_cap * sizeof(int64_t));
        if (!new_data) {
            fprintf(stderr, "realloc failed\n");
            exit(1);
        }
        v->data = new_data;
        v->cap = new_cap;
    }
    v->data[v->len++] = x;
}

static IntVec *build_factors(int64_t cap) {
    IntVec *factors = (IntVec *)calloc((size_t)cap + 1, sizeof(IntVec));
    if (!factors) {
        fprintf(stderr, "calloc failed\n");
        exit(1);
    }
    for (int64_t i = 2; i <= cap; i++) {
        if (factors[i].len == 0) {
            for (int64_t j = i; j <= cap; j += i) {
                intvec_push(&factors[j], i);
            }
        }
    }
    return factors;
}

static void free_factors(IntVec *factors, int64_t cap) {
#if PROBE_NO_FREE
    (void)factors;
    (void)cap;
#else
    for (int64_t i = 0; i <= cap; i++) {
        free(factors[i].data);
    }
    free(factors);
#endif
}

int main(void) {
    const int64_t cap = 999983;
    int64_t sum = 0;
    for (int k = 0; k < 50; k++) {
        IntVec *f = build_factors(cap);
        sum += (int64_t)f[cap].len;
        free_factors(f, cap);
    }
    printf("%lld\n", (long long)sum);
    return 0;
}
```

### `bigbuf.kara` — the cache's intended shape

```kara
// Discriminator for B-2026-07-30-4: is the buffer cache a pessimization on
// its OWN intended shape (repeated large alloc/free), or only when a large
// parked buffer coexists with ~1e6 small live allocations?
//
// This probe is the cache's intended workload with the small allocations
// removed: build and drop one ~24 MB buffer, 200 times, nothing else on the
// heap. Fill value is non-zero so this takes the cache-aware alloc path
// rather than the `Vec.filled(n, 0)` calloc fast path.
//
// The sink strides the whole buffer so the allocation and fill cannot be
// elided — an earlier revision read only v[0] and LLVM deleted the entire
// allocation (200 iterations "ran" in 1.3 ms and the cache was never hit).

fn main() {
    let n = 3000000;
    let mut sum = 0;
    for _ in 0..200 {
        let mut v: Vec[i64] = Vec.filled(n, 7);
        v[0] = 1;
        let mut j = 0;
        while j < n {
            sum = sum + v[j];
            j = j + 4096;
        }
    }
    println(sum);
}
```
