# Brick #3 — Parallel fan-out

**Program.** Fetch N user records concurrently from a JSON API, aggregate results, print.

**What this brick stress-tests.**
- Concurrency primitives across languages
- Kāra's `go` / channel / `par` / auto-parallelism machinery (which we've never touched in prior bricks)
- Data-race freedom at the type level
- Cost of "simple concurrency" for a one-shot CLI

---

## Per-axis scoring

| Axis | Java (VT) | Python (asyncio) | Rust (tokio+futures) | Kāra (current) |
|---|---|---|---|---|
| Robustness | Medium | Medium | High | Highest |
| Locality | High | High | High | High |
| Composability | Medium | Medium | High | Highest |
| Ceremony | Medium | Medium | Medium-high | Low–medium |

### Robustness
- **Java (virtual threads)** — `Future.get()` throws checked `ExecutionException` wrapping the real cause. Data-race freedom is the programmer's responsibility; JVM gives no help.
- **Python (asyncio)** — `asyncio.gather` collects results or raises on first error. Single-threaded event loop gives data-race freedom *by virtue of the GIL*, not the type system.
- **Rust (tokio)** — `try_join_all` composes typed errors via `?`. `Send` bound at spawn sites catches accidental non-thread-safe closures at compile time. Data-race freedom guaranteed by borrow checker.
- **Kāra** — same Rust-level guarantees (channel ownership transfer, effect-driven auto-parallelism is provably sound), **plus** effect-level reasoning: `reads(Network)` + `reads(Network)` is a compile-time proof that two fetches don't conflict. The compiler knows it's safe to parallelize before the programmer even writes `go`.

### Locality
All four tie. The concurrency logic fits in one file; the pattern is visually similar (list of IDs → parallel launches → gather).

### Composability
- **Java** — virtual threads compose but there's no type-level tracking of "this future might fail"; everything becomes `Exception`.
- **Python** — `asyncio.gather` works but mixing sync + async code produces subtle correctness issues (`run_in_executor`, `asyncio.run_coroutine_threadsafe`); seasoned Python devs know these pitfalls, others fall in them.
- **Rust** — `Future` composes cleanly; `try_join_all` is one of several options (`join_all`, `select`, `FuturesUnordered`); each has distinct semantics. The library ecosystem is the source of composability here, not the language.
- **Kāra** — `go { }` + channel is the language-level primitive; `par { }` is syntactic sugar over the same mechanism. Auto-parallelism via effect analysis is a **strict superset** of the other languages' capabilities — no other mainstream language infers "these three independent reads can run in parallel" without a library.

### Ceremony
- **Java virtual threads** reduce ceremony a lot vs. the old `ExecutorService`+callback shape. Still needs explicit `Future.get()` loop.
- **Python asyncio** needs `async def` + `await` + `asyncio.run` — three concepts for one task.
- **Rust** needs `#[tokio::main]` + `async/.await` + one of several `join` variants — most concepts to hold in head.
- **Kāra** needs `go { }` + channel (or `par { }`) — two concepts for the explicit shape. For the known-list case, **zero concepts** — the compiler does it.

---

## What each language forced

- **Java** — `ExecutorService` lifecycle, virtual-thread semantics (new enough that pre-Java-21 code will look very different). `Future<User>` + `.get()` ties exceptions to futures.
- **Python** — the whole async universe (event loop, `async def`, `await`). If *any* part of your codebase is async, the infection spreads — "what color is your function" problem.
- **Rust** — async too, but with the `Send`/`Sync` machinery to guarantee data-race freedom at compile time. Library-level primitives (`tokio`, `futures`) over language-level ones.
- **Kāra** — `go` + channel as a language primitive; `par { }` as a block form; auto-parallelism for known-list cases. **Two primitives replace the entire ecosystem tower** that Rust/Python need. No "async coloring" problem — the effect system handles the "can this run concurrently with that" question automatically.

---

## Where Kāra wins — and where v45 could break it

### Real Kāra-distinctive wins surfaced by this brick

1. **Auto-parallelism** for source-level independent operations. No other mainstream language infers this.
2. **No async coloring.** `fetch()` doesn't need an `async` keyword or a `.await` at the call site. The runtime handles suspension; the type system handles data-race freedom.
3. **Effect-driven conflict analysis.** `reads(Network)` + `reads(Network)` = no conflict. `reads(FileSystem)` + `writes(FileSystem)` = conflict, serialized automatically. The programmer never writes synchronization primitives for disjoint-resource access.
4. **Provider-rooted resources bind concurrency semantics.** A `with_provider[DB](connection, || ...)` block gives every spawned task the same Arc'd provider; the scheduler doesn't cross that boundary unless the provider's `Sync` bound says it's safe.

### Where a D1 change could accidentally break this

- **If D1b (bundled only) lands:** the `impl Trait for Type` form that `effect resource R: DatabaseProvider` depends on goes away, or needs a new escape hatch. Since provider-rooted resources are how Kāra binds concurrency semantics cleanly, weakening the trait/impl machinery weakens the concurrency story.
- **If the effect system changes how inference composes:** auto-parallelism breaks silently. A function whose body the inferencer can no longer prove conflict-free just... runs sequentially. No error, just lost throughput.

**These are not arguments against D1 changes.** They're arguments that *D1 changes must be analyzed against the concurrency story*, not treated as a standalone syntax question. v44 didn't flag this; v45 should.

---

## Provisional implication

**Brick #3 is the first brick that produced a strong Kāra-distinctive result.** The concurrency story is the *actual* language-level differentiator — typed errors and effect-tracked I/O are nice, but every serious language has analogs. Auto-parallelism via effect analysis genuinely isn't matched elsewhere.

**For v45:** the decision shape shifts. Rather than "what's the right trait/impl syntax?", the question becomes **"what's the right trait/impl syntax *such that the concurrency + effect story keeps working*?"**. D1a (current) passes this test. D1b (bundled only) fails it unless a new mechanism is introduced for effect-resource conformance. D1c (hybrid) passes but adds the cost of two forms. D1d (effect-trait fusion) would *simplify* this by collapsing `trait` and `effect resource` — now worth serious consideration.

**Recommendation:** Close v44 at **D1a** for now, but open a separate track to explore **D1d (effect-trait fusion)**, which this brick suggests may be the genuinely interesting redesign. It's orthogonal to D1a's current shape; if fusion lands, the trait/impl form becomes a smaller consequence, not the primary question.
