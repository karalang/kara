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

All compiler output available as structured JSON with machine-applicable fix diffs. Compiler query API for programmatic access to effect inference, ownership decisions, and concurrency analysis. Canonical formatter for clean semantic diffs.

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
- **Demo 1 verified on r8g.4xlarge (Linux, 16 vCPU) at the 2M ceiling, head-to-head with a Rust (tokio + rustls) reference on the same box:** both impls hold **2 000 000 idle WebSocket-over-TLS connections, 0 failures**. **Kāra at ~7.8 KB/conn** server-side RSS (so ~14.7 GB for 2M) vs **Rust at ~27.9 KB/conn** — **3.55× density advantage**, empirically scale-invariant from 1M to 2M (drift <0.4% on either side). Connect-phase latency at `--concurrency 64`: Kāra **mean 214.6 ms, p50 41 ms, p99 798 ms**; Rust **mean 206.9 ms, p50 2.93 ms, p99 872 ms**. Multi-dimensional tradeoff: **Kāra wins memory (3.55×) and tail (p95–max ~8–10 % tighter); Rust wins handshake hop (p50 ~14× tighter) and throughput (~4 % mean)**. Source: [`examples/ws_idle_holder`](examples/ws_idle_holder); full methodology + caveats + commercial-reframe lens in [`examples/ws_idle_holder/bench/REPORT.md`](examples/ws_idle_holder/bench/REPORT.md); reproduction harness in [`examples/ws_idle_holder/bench`](examples/ws_idle_holder/bench). The bench README documents why the headline is a `--concurrency` sweep rather than a single oversubscribed point.

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
