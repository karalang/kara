# drop-fuzz corpus

Output home for the **drop-soundness fuzzer** (`src/bin/drop_fuzz.rs`) — Slice 1
of [`../ownership-model-mechanization.md`](../ownership-model-mechanization.md).

## What's here

- `report.md` — the last committed run's report: measured drop-bug rate +
  bucketed signature table + any shrunk minimal repros.
- `repro_*.kara` — shrunk, kata-sized repros, one per finding signature (only
  present when a run found something; **empty on a clean HEAD**, which is the
  current state).

## Run it

```bash
scripts/drop-fuzz.sh --count 400 --seed 5000 --out docs/spikes/drop-fuzz-corpus
```

The wrapper builds the runtime staticlib archives (the ASan link's hard
prerequisite) and the `drop_fuzz` binary, then runs the fuzzer. All flags after
the script name forward to the binary (`--count`, `--seed`, `--out`,
`--no-shrink`, `--keep-going`, `--verbose`). A run is fully reproducible from its
seed: program *k* uses `seed + k`, so a finding is re-derivable with
`--seed <s> --count 1`.

**Leak coverage needs Linux.** LeakSanitizer ships with upstream LLVM's ASan on
Linux; Apple-clang macOS has no LSan (double-free / UAF only there). On macOS,
run this inside the container gate (`scripts/lsan-local.sh --shell`).

## Current measurement (2026-07-07)

**0 findings over 1000+ valid (program, surface) executions** on HEAD. The known
drop-soundness classes in the generator's covered heap-core are closed on the
current compiler.

## Why "green" is not vacuous — validation by fault injection

Because HEAD is hardened, the fuzzer's ability to *catch* drop bugs was proven by
**mutation-testing the detector**: two temporary, env-gated, default-dormant
knobs were added to codegen, the fuzzer was run, and the knobs were then **fully
reverted** (the committed slice-1 artifact touches no compiler code). Each knob
reproduces a headline ledger class, and the fuzzer caught both on *both* build
surfaces (`seq` + `autopar`):

| knob (env var) | injected fault | class the fuzzer flagged |
|---|---|---|
| `DROPFUZZ_INJECT_LEAK` | skip the scope-cleanup drain | `memory-leak` (LSan) |
| `DROPFUZZ_INJECT_DOUBLE_FREE` | disable move-source suppression | `double-free` + `segv` (ASan) |

The double-free knob's finding shrank to this 3-line core (a `Vec[String]` moved
into a `Vec[Vec[String]]` whose source cleanup was left armed):

```rust
fn main() {
    let mut acc: i64 = 0i64;
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v3: Vec[String] = Vec["...40+ byte payload...".to_string(), "...".to_string(), "...".to_string()];
        let mut vv4: Vec[Vec[String]] = Vec.new();
        vv4.push(v3);
        round = round + 1i64;
    }
    println(acc);
}
```

### Exact injection diffs (to reproduce the validation)

These are **not** in the tree — apply them by hand, run the fuzzer with the
matching env var, then `git checkout` to revert.

**Leak** — `src/codegen/runtime.rs`, in `emit_scope_cleanup_from`, just before
the `emit_cleanup_action_at` call inside the action loop:

```rust
let inject_leak = std::env::var_os("DROPFUZZ_INJECT_LEAK").is_some();
// ... inside the `for action_idx` loop, after the UserErrDefer skip:
if inject_leak {
    continue; // skip the drop → the heap value leaks
}
```

**Double-free** — `src/codegen/call_dispatch.rs`, as the first statement of
`suppress_source_vec_cleanup_for_arg_ex`:

```rust
if std::env::var_os("DROPFUZZ_INJECT_DOUBLE_FREE").is_some() {
    return; // caller keeps its cleanup armed for a moved value → aliased double-free
}
```

Then, e.g.:

```bash
DROPFUZZ_INJECT_DOUBLE_FREE=1 cargo run --features llvm --bin drop_fuzz -- --count 8 --seed 1
```

> Note: emitting the *same* cleanup action twice does **not** double-free —
> codegen null-guards freed slots (sets `cap = 0` / nulls the pointer). A real
> double-free needs two *distinct* owners of one buffer, which is why the knob
> disables move-source suppression rather than double-emitting.

## `--differential` — the oracle↔codegen differential (Slice 4 down-payment)

Beyond the sanitizer-as-judge mode above, the fuzzer can run a **structural**
check: compare the Slice-3 ownership oracle's per-function drop *schedule*
against the drops codegen actually emits.

```bash
cargo run --features llvm --bin drop_fuzz -- --differential --count 1000 --seed 1
```

Per program it compiles in-process (`compile_to_ir`, seq surface) with a
read-only recorder (`src/codegen/drop_obs.rs`) armed. The recorder taps the
single funnel every emitted drop passes through (`emit_cleanup_action_at`) and
logs each `(function, place)`; the harness then diffs that set against the
oracle's schedule and flags a **missing drop** — the oracle scheduled a drop
codegen emitted no cleanup for, i.e. a leak — localized to `(function, place)`.
Unlike the ASan mode it needs **no runtime archives and no `cc`** (nothing is
linked or run), only LLVM. `--explain S` prints one seed's per-function drop
sets (oracle scheduled / codegen emitted / missing) for triage.

Only the **missing-drop (leak)** direction is checked — the extra-drop
(double-free) direction is not emit-time observable (codegen neutralizes a
moved-out value's drop with a runtime null/cap guard while keeping the action,
so a guarded no-op looks identical to a real free at emit time). The ASan mode
above stays the double-free authority. Three alignment rules keep it sound:
oracle runs on the *surface* tree (pre-`lower`, so desugared temporaries don't
false-positive); parameter drops are excluded (codegen frees bare `String`/`Vec`
params caller-side while the oracle models them callee-owned — freed once, across
the boundary); and the §7 closure/`spawn`/`par` capture edge is skipped and
**counted** (the oracle conservatively keeps a captured value Owned; codegen
moves it into the task).

**Current measurement (2026-07-07):** **330 programs checked, 2199 scheduled
drops, 0 divergences** (670 skipped as the §7 capture edge) at `--count 1000`.
The oracle and codegen agree on every checked function's drop schedule.

**Non-vacuity** (same discipline as the fault-injection above, but a *permanent*
env knob, not a reverted diff): `KARAC_DROPOBS_SILENCE=1` no-ops the recorder,
simulating codegen forgetting every drop, so the differential must report the
whole schedule as missing.

```bash
# off → 0 divergences;  on → every scheduled drop reported missing
KARAC_DROPOBS_SILENCE=1 cargo run --features llvm --bin drop_fuzz -- --differential --count 60 --seed 1
```

Measured: 137/137 scheduled drops flagged over 22 programs with the knob on, 0
with it off — proof the gate observes codegen's real drops rather than passing
vacuously.
