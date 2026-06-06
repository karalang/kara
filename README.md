# Kāra

```
 compiling the compiler...
 [▓▓▓▓▓▓▓▓▓▓▓▓▒▒░░░░░░░░░░░░]
```

Kāra is a systems programming language for the age of AI-written code. Declare intent; the compiler handles what LLMs get wrong — memory layout, ownership, concurrency — and emits every decision as structured output agents can consume.

Questions, ideas, or design feedback? [Start a GitHub Discussion](https://github.com/karalang/kara/discussions/new/choose) — all input welcome.

---

## What Makes Kāra Different

### AI-First Compiler Interface

The compiler's analyses aren't trapped in human-oriented text — every decision it makes is queryable as JSON.

**Query API.** `karac query <kind> <file>[.<function>]` exposes what the compiler inferred:

| Query | Returns |
|---|---|
| `effects` | inferred vs. declared effects, per function |
| `ownership` | parameter modes, RC fallbacks (with the trigger line), closure captures |
| `concurrency` | which statements auto-parallelize, and why |
| `cost-summary` | RC ops, Arc wraps, and perf notes per function or file |
| `monomorphization` | generic instantiation table — which types, at which sites |
| `affected-by` | transitive callers/callees/tests of a function or line range |
| `attributes` | index of namespaced `#[tool::*]` attributes |

Querying the `load_dashboard` example from the next section:

```bash
$ karac query concurrency dashboard.kara.load_dashboard
{"function":"load_dashboard","total_statements":3,
 "parallel_groups":[{"statements":[0,1,2],"reason":"no data or effect dependencies"}]}
```

**Structured diagnostics with machine-applicable fixes.** `karac check|build|run --output=json` emits diagnostics with phase, error code, span, and — where the compiler knows the answer — the fix as concrete edits:

```json
{"severity":"error","phase":"resolve","code":"E0100","line":3,"column":13,
 "message":"undefined name 'totl', did you mean 'total'?",
 "replacement":{"offset":44,"length":4,"text":"total"}}
```

`karac fix` applies those edits directly (`--dry-run` to preview). `--output=jsonl` streams build events (`phase_start`, `diagnostic`, `build_complete`, …) so agents can react before the build finishes.

**Explanations on demand.** `karac explain --class=TYPE_MISMATCH` (or `--concept=<name>`) returns the spec-grounded explanation behind any diagnostic class, as text or JSON.

**Canonical source form.** `karac fmt` emits one canonical formatting, so agent-written diffs stay semantic. `karac catalog` emits one JSONL signature record per function/type for cheap codebase indexing.

### Effect System — No Async/Await, No Colored Functions

Every function declares what it does to the world. The compiler uses this for automatic parallelization:

```
pub effect resource UserDB: UserDatabase;
pub effect resource OrderDB: OrderDatabase;
pub effect resource NotifDB: NotificationDatabase;

fn load_dashboard(user_id: i64) -> Dashboard
    with reads(UserDB) reads(OrderDB) reads(NotifDB)
{
    let profile = fetch_profile(user_id);       // reads(UserDB)
    let orders = fetch_orders(user_id);         // reads(OrderDB)
    let notifications = fetch_notifs(user_id);  // reads(NotifDB)

    // Compiler sees non-conflicting effects → runs all three concurrently
    // Data dependency on all three → inserts sync point here
    build_dashboard(profile, orders, notifications)
}
```

No `async fn`. No colored functions. No `Promise.all`. The compiler handles concurrency because it understands effects and data dependencies.

### Tiered Ownership — No Lifetime Annotations

Rust's ownership model without `'a` noise:

```
// Parameter modes are declared at the signature: bare T is owned,
// ref T / mut ref T are explicit borrows. No lifetimes required.
fn process(data: Data, config: ref Config) -> Summary {
    let result = transform(data, config.threshold);
    //                     ^^^^ consumed (owned)
    //                           ^^^^^^ read through borrow
    result.summarize()
}

// Zero-copy returns borrow from a parameter — no 'a annotation needed.
fn first_word(s: ref String) -> ref String {
    s.split(' ').first()
}
```

Escalation path: owned → `ref` → RC. Each step is an explicit choice, not a compiler surprise.

### Data Layout Separation

Logical structure stays clean. Physical layout is a separate, opt-in concern:

```
struct Entity {
    id: u64, name: String,
    position: Vec3, velocity: Vec3,
    health: f32, armor: f32, is_alive: bool,
}

layout entities: Collection<Entity> {
    group physics { position, velocity }   // hot path: physics tick
    group combat { health, armor, is_alive } // hot path: combat
    group metadata { id, name }              // cold
}
```

## Production Readiness

What v1 ships with, what the numbers look like, and what the toolchain gives you.

### Concurrency Runtime

- Target: **1M+ idle connections per process.**
- Blocking-style I/O syntax; effect-driven scheduling moves blocking work off the par-runtime threads.
- **Demo 1 verified on r8g.4xlarge (Linux, 16 vCPU) at 1M and 2M, head-to-head with a Rust (tokio + rustls) reference on the same box** — with the per-connection handler **executing** (recv/send over the coroutine network-async transform; the recv buffer + coroutine frame are held, not freed): both impls hold **2 000 000 idle WebSocket-over-TLS connections, 0 failures**. **Kāra at ~12.1 KB/conn** server-side RSS vs **Rust at ~27.9 KB/conn** — **2.30× runtime-density advantage**, scale-invariant 1M↔2M (Kāra −0.03 % drift). In production-cost terms, counting the kernel socket buffer both stacks pay equally, total server-side memory is **15.0 KB vs 30.4 KB/conn (2.03×)** — so at a realistic 250K conns/box Kāra fits an 8 GiB `m7g.large` where Rust needs a 16 GiB `m7g.xlarge`, ≈**50 % lower infra cost** (~$473 vs ~$946/yr per 250K unit on a 1-yr reserved instance). Connect-phase latency at `--concurrency 64` (1M): Kāra **mean 82 ms, p50 46 ms, p99 255 ms**; Rust keeps a ~3 ms p50 (tighter handshake hop) but a wider tail. Source: [`examples/ws_idle_holder`](examples/ws_idle_holder); full methodology + cost model + caveats in [`examples/ws_idle_holder/bench/REPORT.md`](examples/ws_idle_holder/bench/REPORT.md); reproduction harness in [`examples/ws_idle_holder/bench`](examples/ws_idle_holder/bench). _Note: an earlier 7.8 KB/conn / 3.55× figure was measured before the handler executed and is superseded._

### Standard Library at v1

In-tree, no third-party runtime dependencies.

- `std.http` server (HTTP/1.1, HTTP/2) — _TBD: link to module + minimal example_
- TLS — _TBD: link to module + minimal example_
- WebSocket — _TBD: link to module + minimal example_

### Performance

Cross-language benchmarks vs. Rust and Go, reported in two lanes:

- **Sequential lane** (`KARAC_AUTO_PAR=0`): apples-to-apples comparison against single-threaded Rust/Go. **This is the headline lane.**
- **Auto-parallel lane** (default): Kāra with the auto-par runtime enabled, reported separately and clearly labeled.

_TBD: per-kata table and graphs, sourced from `bench/` and the `kara-katas` repo. Sequential lane leads; auto-par follows in its own callout._

### Toolchain

- LLVM-backed codegen.
- Address-sanitizer–clean across the codegen E2E suite.
- Structured diagnostics and the AI-first compiler interface described above.

### Targets

- **Native** — the v1 compile target.
- **WASM, GPU, and embedded** — on the roadmap (Phase 10); one language across targets under per-target profile constraints. GPU ships as a compile target first; call-site ergonomics come later. See [docs/design.md](docs/design.md).

## Docs

- **[docs/design.md](docs/design.md)** — The language specification. Authoritative source for all committed design decisions.
- **[docs/syntax.md](docs/syntax.md)** — Syntax reference and quick lookup.
- **[docs/glossary.md](docs/glossary.md)** — Terminology used across the design and compiler.
- **[docs/roadmap.md](docs/roadmap.md)** — Compiler implementation plan, phase by phase.
- **[docs/implementation_checklist/](docs/implementation_checklist/)** — Items to validate, benchmark, or revisit during specific phases.
- **[docs/deferred.md](docs/deferred.md)** — Committed designs for deferred features (P1: decided/non-breaking, P2: speculative).
- **[docs/demo_ideas.md](docs/demo_ideas.md)** — Programs that showcase Kāra's differentiating features.

## Project Status

Actively developed, pre-1.0. The frontend, interpreter, query API, auto-concurrency runtime, and LLVM codegen are in place; the standard library is being filled in. End-to-end compilation works for a growing subset of the language. See [docs/roadmap.md](docs/roadmap.md) for the current phase breakdown.

We took a **tree-walk interpreter first** approach: language semantics were validated with an interpreter before LLVM code generation.

## Prior Art

| Language/System | What Kāra takes |
|---|---|
| **Rust** | Ownership, enums, pattern matching, traits, `Result<T,E>` |
| **Koka** | Algebraic effect system (simplified: no handlers, trait injection instead) |
| **Zig** | Memory layout control, comptime (deferred) |
| **Go** | Simple concurrency model (blocking I/O on threads) |
| **Swift** | Inferred reference counting (as fallback, not primary) |
| **Unity DOTS / Bevy** | Data-oriented design, SoA layouts |

## Getting Started

```bash
cargo build                          # build the compiler (no LLVM backend)
cargo test                           # run the front-end tests (lexer, parser, resolver, typechecker, effect, ownership, interpreter)
cargo test --features llvm           # also run codegen E2E and memory-sanitizer tests
cargo clippy --all --all-targets -- -D warnings   # lint
cargo fmt                            # format
```

Codegen E2E tests (`tests/codegen.rs`, `tests/par_codegen.rs`, `tests/memory_sanitizer.rs`) are gated on `--features llvm` and need the runtime library built once via `cargo build -p karac-runtime --release`. The memory-sanitizer suite additionally needs a `cc` toolchain that supports `-fsanitize=address`; it skips gracefully on hosts that don't.

See [docs/roadmap.md](docs/roadmap.md) for current progress and [docs/design.md](docs/design.md) for the language specification.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
