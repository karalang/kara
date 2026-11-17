# Parallax-lite

A compact auto-concurrency workload: three calls on three disjoint effect
resources, with no written `par`. It was the first measurement workload for
auto-par on the pre-M3 backend.

> **What holds on 0.8.** The package builds and runs (`karac build` from this
> directory, then `./dist/native/parallax_lite`), but **auto-par declines this
> workload today**, so a default build and a `KARAC_AUTO_PAR=0` build run in
> the same time. The speedup tables this README used to carry (2.81x, and a
> 15.88x scaling run) were measured on the pre-M3 backend, which was deleted
> at M3; they do not reproduce and have been removed. Since
> B-2026-11-16-274 the three writes no longer conflict; what declines them
> now is auto-par's cost rule.

## What it demonstrates

`src/workload.kara`'s `process_request` is three top-level call statements,
each writing a different resource:

```kara,ignore
pub fn process_request() with writes(MetricsA) writes(MetricsB) writes(MetricsC) {
    record_a();
    record_b();
    record_c();
}
```

Each `record_*` calls its resource's provider, whose `record(mut ref self,
value)` runs a 50-million-iteration loop (`src/resources.kara`). `main` binds
one provider per resource with nested `with_provider[R](...)` scopes.

The effect analysis groups the three statements (no two touch the same
resource). The auto-par verdict is computed for a program with a `main`, and
`karac query` reads one file, so flatten the package first:

```sh
cat src/resources.kara <(grep -v '^import ' src/workload.kara) \
    <(grep -v '^import ' src/main.kara) > /tmp/parallax_lite.kara
karac query concurrency /tmp/parallax_lite.kara.process_request
```

It reports the parallel group `[0, 1, 2]` ("independent effects on different
resources"), then an `auto_par` entry with `tasks: false` and the reason "0 of
its 3 statements nest loops or recurse". The writes are independent (that was
B-2026-11-16-274, fixed at ec237045da), but the loop is inside the provider's
method, one call away, so the cost rule does not count it, and auto-par keeps
the statements in order. The same three calls written as
`par { record_a(), record_b(), record_c() }` run in parallel.

## How to run

```sh
cd examples/parallax_lite

# Auto-par on (the default)
karac build && time ./dist/native/parallax_lite

# Sequential lane
KARAC_AUTO_PAR=0 karac build && time ./dist/native/parallax_lite
```

## Measured on 0.8

The examples probe, 2026-11-16, on a 4-core Linux x86_64 container with a
release karac (`--features llvm`) at 236666ac41, best of 5:

| Build | Wall-clock |
|---|---|
| default (auto-par on) | 0.049 s |
| `KARAC_AUTO_PAR=0` | 0.049 s |
| the same calls in a written `par { ... }` | 0.018 s |

## Files

- `kara.toml`: the package manifest.
- `src/resources.kara`: the `MetricsRecorder` trait, the three resources
  `MetricsA`, `MetricsB`, `MetricsC`, and the `InMemoryMetrics` provider with
  the busy-compute kernel.
- `src/workload.kara`: the three drivers and `process_request`.
- `src/main.kara`: binds the three providers and calls `process_request`.

## See also

- [Chapter 14](../../docs/book/src/ch14-concurrency.md): `par` blocks and
  auto-par.
- [`examples/parallax`](../parallax/): the larger sibling.
