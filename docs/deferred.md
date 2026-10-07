# Deferred features

This file keeps the design of every feature that is not in v1, grouped by the track that brings it back. v1's scope is [design.md §1](design.md#1-what-kāra-is). A feature described here is not part of the language until its design moves into design.md.

Each track opens with what it brings. Within a track, each feature has its own section; a short quoted note at the top of a section says how it relates to v1 where that is not obvious. Notes marked **Open** are questions the track must answer before the feature returns.

**Older spelling in examples.** Code here was written before 2026-10-07 and keeps that day's spelling. A bare parameter or `self` was owned and `ref T` or `ref self` borrowed, where v1 now writes `own T` / `own self` and a bare `T` / `self` ([design.md §7](design.md#parameter-modes)). A few examples also use call-site type arguments (`offset_of[T]`) or `::` paths, which v1 has neither of. A track that brings a feature back rewrites its examples in v1 spelling.

**Tracks**

1. [M4a services](#m4a-services)
2. [M4b auto-par](#m4b-auto-par)
3. [M4c data](#m4c-data)
4. [Systems](#systems)
5. [Verification](#verification)
6. [Effects expressiveness](#effects-expressiveness)
7. [Interactive](#interactive)
8. [Web](#web)
9. [GPU](#gpu)
10. [Comptime](#comptime)
11. [dyn](#dyn)
12. [Layout](#layout)
13. [Self-hosting](#self-hosting)
14. [Tooling](#tooling)
15. [Unscheduled language extensions](#unscheduled-language-extensions)
16. [Permanent omissions](#permanent-omissions)

## M4a services

Coroutines and the `suspends` effect, non-blocking networking, one scheduler and reactor, cooperative cancellation and deadlines, channel `select` with timeouts, task-level failure isolation, providers for test doubles, effect variables for stored callbacks, and effect groups if the services programs show they are needed. The track comes before the v1 release. Its gate is the services programs in the corpus plus a load test of connections and tail latency; blocking-I/O services already run in v1.

### Execution Effects (`blocks` / `suspends`)

`blocks` is part of v1 ([design.md §12](design.md#blocks)). This entry keeps the design of `suspends` and of task placement between the two.

The resource verbs ([design.md §12](design.md#effect-verbs)) answer *"can these two operations conflict?"* — they drive conflict analysis and auto-concurrency parallelization. They do **not** answer *"where should this operation run?"*. A function that is conflict-free with its siblings can still be a disaster to schedule if it parks the OS thread (freezing a worker that other tasks need) or yields across a critical section. The scheduler needs a separate axis to reason about *placement*.

Kāra adds two **execution verbs** for this purpose:

- `blocks` — may park the OS thread in a kernel wait state. While a `blocks` call is in progress, the thread cannot do any other work — it is off the table for the scheduler until the wait ends. Examples: `std.time.sleep`, `std.fs.read_sync`, `std.sync.Mutex.lock` under contention, synchronous FFI into a C library that waits on a `pthread_cond_t`.
- `suspends` — may cooperatively yield; the task parks itself and the thread is freed to run other tasks. Unlike `blocks`, this is the *desired* behavior on async worker pools — it is what the pool exists for. The compiler inserts yield points automatically at calls to functions with the `suspends` effect — no programmer syntax is needed. Examples: `async_channel.recv()`, `net.http.get(url)`.

Execution verbs are declared alongside resource verbs on public functions and inferred on private functions. They take **no resource parameter** — a function either may block/suspend or it may not. Unlike `allocates(Heap)` — which is default-permitted and need not be declared on public functions under the standard profile — **`blocks` and `suspends` must be explicitly declared on public functions**: they are not default-permitted in any profile. The rationale: `blocks` and `suspends` change where and how a function can be safely called (blocking pool vs. async worker pool), which is a consequential fact for callers and schedulers. Omitting them from a public signature would make the scheduling behavior invisible, defeating the purpose of the execution-verb system. For private functions, `blocks` and `suspends` are inferred via the same transitive-closure algorithm as resource effects — no annotation is needed at private boundaries. Conflict analysis ignores them: two `blocks` tasks do not *conflict* in the effect sense, and the scheduler is still free to parallelize them — but it uses the verbs to decide *where* to place each task:

- `blocks` tasks are routed to a dedicated blocking pool separate from the async worker pool, or given a fresh OS thread per call.
- `suspends` tasks run on the async worker pool (their normal home).
- Tasks with neither verb can run anywhere.

```
fn sleep(duration: Duration) with blocks { ... }                      // stdlib — kernel timer wait
fn read_sync(path: Path) with reads(Fs) blocks { ... }                // stdlib — synchronous I/O
fn recv[T](rx: Receiver[T]) -> T with receives(rx) suspends { ... }  // async channel
fn compute(x: f64) -> f64 { ... }                                // pure — neither blocks nor suspends
```

**Why execution verbs are a separate category, not new resource verbs.** Thread occupancy is not a read/write relationship on a named resource. Modeling `blocks` as `writes(Thread)` would shoehorn a scheduling concept into the conflict system and produce misleading diagnostics ("writes to Thread"). Keeping execution verbs as their own category makes the distinction honest: resource verbs answer *conflict*, execution verbs answer *placement*. The two axes are orthogonal — a function can have any combination of resource and execution effects, and they are reasoned about independently.

**Why `blocks` and `suspends` differ from `panics` for scheduling.** A panicking function ends the process at once; a blocking function does not return until the kernel releases it. Different runtime behavior, different scheduling implication, different verb.

**Thread pinning is modeled as a resource, not a third execution verb.** Some FFI libraries (OpenGL contexts, macOS Cocoa UI calls, certain GPU command queues) require a task to own a dedicated OS thread for its entire lifetime — it cannot be moved across threads, cannot share a worker, cannot be suspended and resumed elsewhere. This is an *identity* constraint, not a *placement* constraint, and it fits the resource/conflict model cleanly. Kāra exposes a built-in resource `Thread.pinned`; declaring `writes(Thread.pinned)` means "this task claims exclusive ownership of its OS thread for its lifetime." Two such tasks conflict on the resource and cannot share a worker, handled by the existing distinctness-graph machinery. Adding a third execution verb (`requires_thread`) would have duplicated mechanism that already exists for resource conflicts.

#### Execution Effect Inference

Inference for `blocks` and `suspends` uses the same transitive-closure algorithm as resource effects — a private function has every execution effect that any of its callees have, plus any effect produced by primitive operations it performs directly. The fixed-point iteration over SCCs (described below for resource effects) applies unchanged.

**Leaves for `blocks`:**

1. **Stdlib annotations.** Primitives like `std.time.sleep`, `std.fs.read_sync`, `std.sync.Mutex.lock`, and blocking channel ops are declared `blocks` at their public signatures. The compiler trusts these the same way it trusts stdlib resource annotations.

2. **Nothing else.** `blocks` has no syntactic trigger in user code. A `loop { }` that burns CPU is *not* blocking — it is using the thread, not parking it. Only syscalls and FFI into parked-wait primitives block, and those always enter inference through (1).

**Leaves for `suspends`:**

1. **Stdlib primitives declared `suspends`.** Network I/O, async channel operations, timer waits — any stdlib function whose public signature includes `suspends`. These are the leaves of the inference graph.
2. **User functions that transitively call a `suspends` function.** Same closure rule as resource effects.

FFI is **not** a leaf for `suspends` — C code cannot suspend a Kāra task. `extern` functions never contribute `suspends`.

**Diagnostics.** `karac explain f` traces the inference of `blocks` and `suspends` the same way it traces resource effects: every edge in the reasoning chain is surfaced. A function inferred `blocks` because it transitively calls `std.fs.read_sync` gets a trace showing exactly that path — `f → g → std.fs.read_sync (declared blocks)`. This is not an opt-in diagnostic mode; it is part of the always-available `explain` output for execution effects, exactly mirroring the existing trace for resource verbs.

**What async I/O looks like in practice.** There is no visible yield-point syntax, no `async fn`, no `Future` type to construct, no coloring. The programmer writes plain function calls:

```kara
// stdlib — public, so effects are declared
pub fn http_get(url: String) -> Result[Response, HttpError]
    with reads(Network) suspends { ... }

// Application code — private, so effects are inferred
fn fetch_user(id: u64) -> Result[User, AppError] {
    let resp = http_get(f"/users/{id}")?;
    parse_user(resp.body)
}
```

`fetch_user` is inferred as `reads(Network), suspends` because it calls `http_get(...)`. The compiler inserts a yield point at the `http_get` call automatically — the task parks, the thread is freed, and the scheduler resumes the task when the network response arrives. The caller of `fetch_user` writes it the same way: `let user = fetch_user(42)?`. No special syntax propagates up the call chain — only the inferred `suspends` effect.

### Async I/O in Practice

This section shows what non-blocking I/O looks like from the programmer's perspective. The key point: there is no `async fn`, no `Future` type, no function coloring, and no yield-point syntax. The programmer writes normal function calls, and the compiler handles everything — yield insertion, state machine transforms, and scheduling.

**HTTP client call.**

```kara
fn load_dashboard(user_id: u64) -> Result[Dashboard, AppError] {
    let profile = http_get(f"/users/{user_id}")?;
    let orders  = http_get(f"/orders?user={user_id}")?;
    let notifs  = http_get(f"/notifs?user={user_id}")?;

    // All three calls read different resources — the compiler can parallelize them
    // (the auto-concurrency rules of Feature 5, in the M4b track)
    Ok(build_dashboard(
        Profile.parse(profile.body)?,
        Vec[Order].parse(orders.body)?,
        Vec[Notification].parse(notifs.body)?,
    ))
}
```

The compiler infers `reads(Network), suspends` for this function. Because the three `http_get` calls have no data dependencies between them and no conflicting effects (all `reads(Network)` — read/read is not a conflict), auto-concurrency runs them in parallel. The compiler inserts yield points at each `http_get` call automatically — the task parks while waiting for the network response, and the thread serves other tasks.

**Async message loop.**

```kara
fn handle_connections(listener: TcpListener) -> Result[(), ServerError]
    with reads(Network) writes(Network) suspends
{
    let tasks = TaskGroup.new();
    loop {
        let conn = listener.accept()?;       // suspends until a client connects
        tasks.spawn(|| handle_client(conn)); // spawn a task per connection
    }
    // tasks joins all spawned tasks on scope exit
}

fn handle_client(conn: TcpConnection) -> Result[(), ServerError] {
    loop {
        let request = conn.read_request()?;   // suspends until data arrives
        let response = process(request)?;   // pure computation
        conn.write_response(response)?;    // suspends until write completes
    }
}
```

`handle_client` is inferred as `reads(Network), writes(Network), suspends`. The compiler inserts yield points at `read_request` and `write_response` automatically — thousands of connections share a small thread pool because parked tasks consume no thread. Each spawned task operates on its own `TcpConnection`, so their `reads(Network)` / `writes(Network)` effects are non-conflicting — the connection value is a parameterized resource instance. The `TaskGroup` provides the structured join boundary (see [design.md §13 `TaskGroup`](design.md#taskgroup)).

**Mixing sync and async.** A function that calls both `suspends` and `blocks` callees is inferred as both. The scheduler routes it to the blocking pool (the more conservative placement), ensuring it does not starve the async worker pool:

```kara
fn ingest(path: Path, url: String) -> Result[Report, IngestError] {
    let local_data = fs.read_sync(path);          // blocks (synchronous file I/O)
    let remote_data = http_get(url)?;              // suspends (async network I/O)
    merge(local_data, remote_data)
}
// Inferred: reads(Fs), reads(Network), blocks, suspends
// Scheduler placement: blocking pool (blocks dominates)
```

### Runtime Phases

v1 runs blocking I/O on OS threads ([design.md §13](design.md#runtime)). Two later runtime stages build on it.

**Network event loop.**

The compiler routes network I/O effects (`sends(Network)`, `receives(Network)`) to an epoll/kqueue-based event loop. Network tasks park without blocking a thread and resume on I/O completion. Compute and file I/O remain on OS threads.

```
Scaling: ~1M+ concurrent network-bound tasks.

Adds support for:
- High-connection web servers
- WebSocket servers with many idle connections
- API gateways and reverse proxies
```

The effect system provides the routing signal — the compiler knows `sends(Network)` is network I/O and parks the task on the event loop. No programmer intervention. The compiler also warns when RAII resources (mutex guards, file handles) span effect boundaries where a yield may occur.

**Full hybrid runtime.**

All I/O effects can park on the event loop. io_uring backend for file I/O on Linux. Full state machine transform at all effect boundaries. Async-aware debugging tooling with source-mapped stack frames.

Code written for the v1 runtime runs unchanged on both. The language surface is the same; only the runtime execution strategy changes.

### Network Event Loop and State-Machine Transform

Five design commitments together specify the network event loop: the state-machine transform's lowering shape, the RAII-across-yield compile-error rule, the debugger contract extension for parked tasks, the FFI-across-yield prohibition, and drop ordering under cancellation. Each is specced as a standalone subsection so any future change touches one rule without re-opening the others.

#### State-Machine Transform — Network-Boundary Functions

A function whose inferred or declared effect set includes `sends(Network)` or `receives(Network)` is **network-boundary** and is lowered to a state machine at codegen time. Every other `suspends`-effecting function — channel receive, custom suspending primitives, timer waits routed through `blocks`, FFI into `pthread_cond_wait` — stays **thread-blocking**: the task occupies its OS thread until the wait ends, and the work-stealing scheduler compensates. The asymmetry is intentional and bounded: the network-boundary case is the workload this design commits to scaling to 1M+ concurrent idle tasks; other `suspends` paths keep their thread-blocking semantics. The full-hybrid transform (every `suspends` function lowered to a state machine) is a later extension; see [Full-Hybrid State-Machine Transform](#full-hybrid-state-machine-transform-arbitrary-suspends-functions).

**Lowering shape.** A network-boundary function `f` lowers to two artifacts:

1. A **state struct** `__kara_state_f` carrying every local that is live across at least one yield point inside `f`, plus a `state: u32` tag that discriminates the active yield site. The struct has one *state value* per yield point in `f`'s body plus one for the entry state and one for the terminal state; the captured-locals union is shared across states because the compiler computes per-yield live sets and packs locals into overlapping storage where lifetimes do not intersect.
2. A **poll function** `__kara_poll_f(state: mut ref __kara_state_f) -> Poll[Output, Error]` that switches on the `state` tag and resumes execution from the corresponding yield site. `Poll[T, E]` is a three-armed return — `Ready(T)`, `Err(E)`, and `Pending` — internal to the runtime, not user-visible. The runtime drives the state machine by calling the poll function whenever the event loop signals that the I/O the task is parked against is ready.

The state struct lives on the heap, allocated through the same `allocates(Heap)` budget the surrounding code already accounts for; allocation happens at the entry to the network-boundary function, and the struct is freed when the function returns `Ready` or `Err` (or when the task is cancelled; see [Drop Ordering Across Yield Points](#drop-ordering-across-yield-points)). The poll function is stack-frame-light: every binding live across a yield point lives in the state struct, so the poll function's own stack frame holds only temporaries.

**Yield points are at network-effect call boundaries, not arbitrary statements.** The transform inserts a yield point only at calls whose callee carries `sends(Network)` or `receives(Network)`. Calls that do not carry network effects — pure computations, `reads(Cache)`, `writes(Log)`, `allocates(Heap)` — do not yield; they execute to completion inside the poll function's body without re-entering the runtime. The state machine therefore has a small, predictable number of states (bounded by the count of network-effect call sites in the function body) rather than one per statement. The yield-point set is surfaced through `karac explain` for inspection.

**Trigger rule — inferred or declared.** A function is network-boundary iff `sends(Network)` or `receives(Network)` appears anywhere in its post-inference effect set, whether the effect was inferred (private function) or declared (public function). The transform decision keys off the post-inference set, so a private function that transitively calls `http_get` is lowered without any annotation. Functions that do not have network effects in their inferred set are not lowered.

**Interaction with monomorphization.** The transform runs *after* monomorphization in the pipeline — analysis and codegen consume monomorphized IR, not generic templates. Each monomorphized instantiation of a generic network-boundary function gets its own state machine. A generic `fn fetch[T](url: String) -> Result[T, Error] with sends(Network) receives(Network)` is lowered to one state struct per concrete `T` it is instantiated against; the captured-locals analysis depends on `T`'s layout, so the state struct's size and field layout differ between monomorphizations. Monomorphization counts surfaced via `karac query monomorphization` already include network-boundary monomorphizations alongside the usual generic-and-effect tuple identity.

**Effect-polymorphic generics in yielding functions.** A function with `with E` polymorphism *and* `sends(Network)` / `receives(Network)` in its baseline ceiling is lowered to a state machine per `(types, effects)` instantiation. The `E` polymorphism does not change the lowering shape — the network-effect baseline is what triggers the transform, and each concrete `E` instantiation gets its own state machine specialised on the captured-effect set. If a closure passed in as `with E` itself carries `sends(Network)` / `receives(Network)`, the calling function's yield-point set grows to include the closure-call site; this composition is regular and follows the same yield-at-network-call-boundary rule.

**Debug-info preservation across the transform.** The DWARF emission (and source-map emission for WASM targets) for a network-boundary function preserves source-level locals and statement boundaries despite the state-machine lowering. The state struct's fields are tagged with their source-level binding names; the poll function's per-state switch arms map back to source statement spans; yield points carry both the network-call call-site span (where the yield happens) and the network-effect callee name (what the task is waiting on). A debugger attaching to a parked task sees the source-level binding values and the await target by name, identically to a debugger attaching to a thread-blocked task at a syscall — the transform is invisible at the debugger surface. The contract extension at [Debugger Contract Extension for Parked Tasks](#debugger-contract-extension-for-parked-tasks) defines the structural surface this debug-info supports.

**Error-path lowering — `?` propagation through yield points.** The `?` operator inside a network-boundary function body — applied to a call that itself may yield — lowers to a check-and-early-return that is integrated into the state machine. The yield point is inserted before the call evaluates; on resume, the post-call check decides whether to propagate `Err(e)` (and exit the poll function with `Err`, dropping the state struct) or continue. Drop ordering for locals that go out of scope on the error path is the same as the non-yielding case: locals are dropped in reverse construction order before the early return fires. The state struct's fields representing those locals are dropped as part of the state-struct destructor, which the poll function invokes before returning `Err`. The user-visible semantics of `?` are unchanged from the non-yielding case; only the runtime mechanism differs.

**FFI interaction — no FFI inside yielding code paths.** A network-boundary function body cannot make an `extern "C"` foreign-function call across a yield point. The transform rejects this at typecheck — see [FFI Across Yield Points](#ffi-across-yield-points) for the rule + diagnostic. The narrow exception is the `unsafe` escape hatch: code that genuinely needs to invoke FFI from within a network-boundary function must do so in an `unsafe { ... }` block AND must not have a yield point between the FFI call's preparation and its return. The compiler does not statically verify the no-intervening-yield property inside the unsafe block — it is the author's responsibility, with a documented `// Safety:` comment ([design.md §15](design.md#unsafe)). The escape hatch exists for niche cases (custom event-loop adapters, embedded targets, certain timer / signal integrations); production code should never reach for it.

**Cancellation cooperation.** A network-boundary task observes cancellation cooperatively at every yield point: the poll function checks the task's cancel flag before re-entering user code on resume and tears down the state machine (via the state-struct destructor) if cancellation has been signalled. The teardown drops captured locals in reverse construction order; see [Drop Ordering Across Yield Points](#drop-ordering-across-yield-points) for the exact rule.

> **Open.** [design.md §13](design.md#failure) plans cooperative cancellation as `Err(Cancelled)` returned from the task's next I/O or suspending call, which the task propagates with `?`. Whether a parked network-boundary task is torn down at its yield point (as above) or resumes with `Err(Cancelled)` is settled when this track returns.

**Layer classification.** The state struct's exact field layout, the encoding of the state tag, whether the poll function is inlined or kept as a separate symbol, and the specific IR shape used by codegen are *Implementation freedom* — the compiler may change them between releases without notice. What is *Guaranteed*: (a) network-boundary functions yield cooperatively at network-effect call boundaries; (b) yield-and-resume preserves source-level binding values; (c) cancellation is observed at every yield point and triggers reverse-construction-order drop of captured locals; (d) the user-visible call/return semantics match a function that did not yield. The state-struct field naming surfaced through `karac explain` and DWARF is *Reported behavior* — stable within a release, may evolve across releases with the same discipline as other `karac explain` output.

#### RAII Across Yield Points

A network-boundary function (per [State-Machine Transform — Network-Boundary Functions](#state-machine-transform--network-boundary-functions)) cannot hold a non-cancel-safe resource across a suspension point. This is a **hard compile error**. The alternative — a runtime resource leak when the task is cancelled while parked — would be silent, untraceable, and would corrode the cancellation story.

**The rule.** For every yield point in a network-boundary function `f`, the typechecker computes the set of bindings live across that yield point. If any binding's type does not satisfy `CancelSafe`, the function fails to typecheck with `error[E_RAII_ACROSS_YIELD]`. The check fires at every yield point independently — a single non-cancel-safe binding live across any yield point rejects the whole function.

**The `CancelSafe` marker trait.** `CancelSafe` is a user-extensible marker trait (per [design.md §9 Marker traits](design.md#marker-traits)) — stdlib types ship with compiler-emitted `impl CancelSafe`, and user code may add its own `impl CancelSafe for T { }` for types whose destructor leaves the world in a sound state when invoked under cancellation.

```kara
pub marker trait CancelSafe;
```

**The semantic contract.** A type `T : CancelSafe` declares: *"If a task holding a value of type `T` is cancelled mid-flight, dropping the value as part of the teardown leaves every resource the value owns in a sound state — no partial writes, no half-committed transactions, no locks held longer than the type's documented scope, no buffered data lost without a documented contract."* The contract is a **safety claim**, not a UB guarantee — incorrectly marking a type `CancelSafe` produces resource leaks or surprising recovery behavior, not undefined behavior. The compiler trusts the impl; the author is responsible for the claim being sound.

**Stdlib `CancelSafe` impls.** The following types ship with `impl CancelSafe` in stdlib:

| Type | Why cancel-safe |
|---|---|
| Every primitive type (`i*`, `u*`, `f*`, `bool`, `char`) | No resources held; drop is a no-op. |
| `String`, `Str`, `Vec[T]`, `Map[K, V]`, etc. — when `T` / `K` / `V` are `CancelSafe` | Drop releases the backing allocation; no observable state escapes. |
| `Mutex[T].Guard`, `RwLock[T].ReadGuard`, `RwLock[T].WriteGuard` | Drop releases the lock; the lock's documented scope is held until drop, no surprise. |
| `TcpConnection`, `TlsStream`, `HttpConnection` | Drop closes the connection cleanly; partial reads / writes appear to the peer as a connection reset — documented behavior for the network types. |
| `sync struct` / `sync enum` values — when every field is `CancelSafe` | Per-field cancel-safe transitively. |

**Stdlib types that are NOT `CancelSafe` by default.** The following carry no stdlib `impl CancelSafe`, so holding them across a yield point in a network-boundary function is rejected:

| Type | Why not cancel-safe |
|---|---|
| `File` before fsync | Drop closes the descriptor, but writes may sit in the page cache and not yet be durable. Cancellation could lose data the program "wrote" but did not flush. The user is responsible for issuing the documented sync call (`sync_data()` / `sync_all()`) before any yield point if durability is required. |
| `BufReader[R]` while buffer non-empty | Drop discards buffered bytes; partial-read data lost without notice. User must explicitly `discard()` (which clears the buffer and is documented to leave the reader cancel-safe) before yielding. |
| `BufWriter[W]` while buffer non-empty | Drop discards buffered writes; data the program "wrote" silently disappears. User must `flush()` before yielding. |
| Database transaction handles (`TransactionGuard`, `Statement` mid-execute) | Drop without commit rolls the transaction back — usually desired — but partial writes visible to other transactions through `READ UNCOMMITTED` are leaked. The driver author audits cancel semantics on a per-handle-shape basis; until the audit lands, no `impl CancelSafe`. |
| Critical-section guards (`InterruptDisabled`, `SignalMask.Guard`) | Drop re-enables interrupts / restores signals, but the time between disable and yield is undefined under cancellation, possibly violating real-time constraints. Embedded profiles do not implement `CancelSafe` on these types. |
| Raw pointers `*const T` / `*mut T` | Drop is a no-op; the pointer outlives any structured reasoning about its referent's lifetime. Cancel-safety is a manual reasoning task — implementing `CancelSafe` on a raw pointer is rejected by stdlib lint convention. |
| `shared struct` / `shared enum` values | Drops during teardown cross unsynchronised state. Replace with `sync struct` (which is `CancelSafe` per its definition contract). |

**Diagnostic shape.** When the rule rejects a function:

```
error[E_RAII_ACROSS_YIELD]: holding 'guard' (type 'std.io.BufWriter[File]') across a suspension point is not cancel-safe
  --> src/handler.kara:42:9
   |
40 |     let mut guard = BufWriter.new(file);
   |         --------- live across the next yield point
41 |     guard.write_all(payload)?;
42 |     server.respond(response)?;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^ yield point here (sends(Network))
   |
note: 'BufWriter[File]' is not 'CancelSafe' while its internal buffer holds unwritten bytes
   = note: if this task is cancelled while parked at the yield point, the buffered writes will be lost — the program "wrote" data that never reached disk
help: flush the buffer before the yield point
   |
42 +     guard.flush()?;
   |
help: alternatively, mark the type 'CancelSafe' if your cancellation semantics tolerate buffer-loss
   |
20 + impl CancelSafe for BufWriter[File] { }
   |
   = note: marking a type 'CancelSafe' is a safety claim; see deferred.md § RAII Across Yield Points
```

The two fix-its appear in preferred order: (1) tighten the scope by releasing or committing the resource before the yield, (2) mark the type `CancelSafe` if the author has audited cancel semantics. The diagnostic includes the source span where the resource was constructed (the binding's introduction site) and the yield point's call-site span (so the user sees both ends of the live range).

**Check site — typechecker, not codegen.** The check runs in the typechecker, *after* effect inference (so the function's network-boundary status is known) and *before* state-machine transform (so the transform never has to lower a function the check would reject). The live-range analysis reuses the existing live-range pass that ownership / NLL already runs; the addition is the per-yield-point set construction plus the `CancelSafe`-bound check on every binding in that set. No new analysis substrate is required.

**Interaction with `unsafe`.** `unsafe { ... }` blocks do not exempt their contents from the rule — a non-cancel-safe binding live across a yield point inside an `unsafe` block still fails to typecheck. The `unsafe` keyword in Kāra grants the ability to invoke unsafe operations; it does not grant the ability to bypass the cancellation contract. Code that genuinely needs to hold a non-cancel-safe resource across a yield is either restructured (release-before-yield) or the type author audits cancel semantics and adds `impl CancelSafe`.

**Orphan rule.** Standard marker-trait orphan rules apply (per [design.md §9](design.md#coherence-and-the-orphan-rule)): a user-package author may add `impl CancelSafe for T { }` for their own types, but cannot add an impl when both `CancelSafe` and `T` are foreign. Stdlib evolution may add new `impl CancelSafe` impls over time; doing so is non-breaking (impl addition is additive). Removing an existing `impl CancelSafe` is a breaking change and is rolled into edition migrations.

**Layer classification.** The rule "network-boundary function cannot hold a non-cancel-safe binding across a yield point" is *Guaranteed semantics* — a conforming compiler must enforce it. The specific stdlib set carrying `impl CancelSafe` is *Reported behavior* — the set is documented here, may grow across compiler releases (always additively per the SemVer rule for impls), and tools that inspect the set must tolerate version skew. Diagnostic wording is *Reported behavior* per the general carve-out.

#### Debugger Contract Extension for Parked Tasks

The four debugger-contract elements of [design.md §17](design.md#debugger-contract) (static spawn-site IDs, parent-frame reference, await-chain pointer, `std.runtime.list_tasks()` / `list_par_blocks()`) apply to network-event-loop-parked tasks identically to `par {}` and `TaskGroup.spawn` workers. This subsection makes the extension explicit: the existing contract surface is the language-level contract for parked tasks too, and the network-event-loop case extends the `WaitTarget` enum with typed variants for network I/O readiness.

**Element 1 — Static spawn-site IDs.** Network-boundary functions inherit `SpawnSiteId`s from their nearest enclosing `par {}` block or `TaskGroup.spawn` site, exactly as workers do. A network-boundary function called inside a `par {}` branch carries the branch's `SpawnSiteId`; one called from the root task carries the root sentinel. The network event loop itself is not a spawn site — it is the runtime substrate that drives the state machine for an already-spawned task. No new IDs are introduced by parking.

**Element 2 — Parent-frame reference.** Every parked task carries a parent-frame pointer per the existing contract. The pointer is set at the moment the network-boundary function is entered (before the first yield point) and persists across every yield-and-resume cycle inside the function — the runtime preserves the pointer in the state struct's metadata header alongside the `state` tag. The pointer survives every exit from the function (cancellation or normal return).

**Element 3 — Await-chain pointer with typed `WaitTarget` variants.** A parked task's `WaitTarget` carries one of three typed variants, distinguishing what the task is blocked against:

```kara
pub enum WaitTarget {
    NetworkIo { fd: RawFd, direction: IoDirection, timeout: Option[Duration] },
    Timer { deadline: Instant },
    Channel { receiver_id: ChannelId, side: ChannelSide },
}

pub enum IoDirection { Read, Write, Both }
pub enum ChannelSide { Sender, Receiver }
```

`NetworkIo` is the network event loop's contribution: every network-boundary task parked at a yield point carries this variant, populated with the file descriptor of the socket the task is parked against, the direction the task is waiting on (read-readable, write-ready, or both — depending on whether the yielded call was `sends(Network)`, `receives(Network)`, or both), and the timeout (if the call was made through a deadline-bearing wrapper). The `RawFd` exposure is intentional: a debugger plugin that wants to correlate the parked task with `lsof` / `netstat` output uses the fd as the join key.

`Timer` and `Channel` cover the broader `suspends` surface (timer waits via the kernel timer wheel, channel-receive blocks) — included here for completeness; the network-event-loop story drives `NetworkIo`, the others are already covered by the broader runtime contract.

**Element 4 — `std.runtime.list_tasks()` includes parked tasks.** The enumeration function returns parked tasks alongside thread-blocking and synchronously-running tasks. Each parked task entry carries: the `TaskId`, the source location of the yield-point call (`file:line` + callee name), the `WaitTarget` variant + payload, the source-level effect summary (the function's declared / inferred effect set), the `SpawnSiteId` inherited from the enclosing concurrency scope, and the parent-frame reference. The same data is available through the structured crash report ([design.md §17 Panic record](design.md#panic-record)), so post-mortem analyzers see the parked-task tree at panic time without needing a live runtime attach.

**Profile-gated metadata emission.** Per the existing contract, the four elements are emitted under `runtime_debug_metadata = true` (default `true` for `[profile.dev]`, default `false` for `[profile.release]`, programmer-overridable in `kara.toml`). The network-event-loop case follows the same gate: a release binary without metadata returns an empty list from `list_tasks()` (degrades gracefully — not an error), and a `WaitTarget` lookup returns `None`. The runtime cost of the metadata is the same shape as for `par {}` and `TaskGroup.spawn` workers — a small per-task header on the state struct, an entry in the global parked-task table maintained by the event loop, and an event-loop-side hook that updates `WaitTarget` on park / unpark transitions. Embedded and `isr` profiles default the gate off (per the existing rule — incompatible with `panics_off` / `default_no_alloc` semantics).

**Stability.** The `WaitTarget.NetworkIo` payload (`fd`, `direction`, `timeout`) is part of the language-level contract and stable within a major version. New `WaitTarget` variants may be added (additive) over time as new wait-target classes emerge (e.g., GPU-fence wait targets for the future GPU backend); existing variants' payloads cannot change shape without an edition-gated migration. Tools keying on `WaitTarget` see the same variant names, field names, and types across compatible compiler versions.

**Deliverables.** The runtime metadata emission for parked tasks, the typed `WaitTarget` enum with the three variants, the `list_tasks()` extension to include parked tasks, and the profile-gating wiring. **Out of scope:** debugger plugins (gdb / lldb adapters that render the parked-task tree), DAP-server task views, profiler GUIs surfacing per-task park duration histograms. These are downstream consumers built on the contract; ecosystem and engineering bandwidth, not language design, gates them.

**Sequencing.** This extension lands with the network event loop. The contract must be in place *before* the event loop's runtime first emits parked-task metadata, or the surface gets locked in by accident.

**Layer classification.** Per the debugger contract ([design.md §17](design.md#debugger-contract)), the four elements form a *Reported behavior* surface — stable within a release, additive evolution across releases. The `NetworkIo` variant's specific shape (the `RawFd` / `IoDirection` / `Option[Duration]` payload triple) is itself *Reported*; tools must tolerate additive evolution.

#### FFI Across Yield Points

A network-boundary function cannot make a foreign-function call across a yield point. The FFI boundary must complete before the function yields — every FFI call must enter the foreign code, return, and produce its result, all without a yield point intervening. This is a **hard compile error**.

**The rule.** For every `extern "C"` call inside a network-boundary function `f`, the typechecker verifies that no yield point lies between the FFI call's argument evaluation and its return. The check is structural — the FFI call expression and any expressions that consume its result must lie within the same yield-free statement boundary. If a yield point would interleave (e.g., an FFI call returns a handle and `f` later awaits a network response that depends on the handle), the function fails to typecheck with `error[E_FFI_ACROSS_YIELD]`.

**Diagnostic shape.**

```
error[E_FFI_ACROSS_YIELD]: foreign-function call cannot span a yield point in a network-boundary function
  --> src/handler.kara:42:13
   |
40 |     let handle = ffi_register(req);   // foreign call
   |                  ------------ FFI boundary entered here
41 |     let response = server.fetch(url)?;
   |                    ^^^^^^^^^^^^^^^^^ yield point (sends(Network))
42 |     ffi_finalize(handle, response);
   |                  ------ FFI handle consumed here, after a yield
   |
note: foreign code does not participate in Kāra's cooperative cancellation; a yield point intervening between the FFI call's preparation and the consumption of its result would leave the foreign code's expected synchronous-progress assumption violated
help: extract the cross-yield work into a non-yielding helper that completes the FFI sequence in one go, then make the network call in the caller
   |
35 + fn finalize_now(req: Request) -> Handle {
36 +     ffi_register(req)
37 + }
   |
   |     let handle = finalize_now(req);
   |     let response = server.fetch(url)?;
   |     ffi_finalize(handle, response);
```

The diagnostic surfaces both the FFI-call site and the intervening yield-point site, names the rule explicitly, and includes the canonical fix-it shape — extract the cross-yield work into a non-yielding helper so the FFI sequence and the network call sit in two separate frames.

**Why FFI cannot yield.** Foreign code makes a synchronous-progress assumption: when Kāra invokes an `extern "C"` function, the foreign side expects the call to enter its body, execute to completion, and return. Yielding mid-call would require either (a) suspending the foreign frame — which the C ABI does not support; there is no portable way to park a C stack and resume it on a different OS thread, and even on platforms where it is technically possible (`ucontext_t`, fiber APIs), the cost and edge-case surface are prohibitive — or (b) abandoning the foreign frame, which means leaking whatever heap allocations, file descriptors, and locks the foreign code is holding. Neither is acceptable; the stance is that the FFI boundary is *transactional* — enter, execute, return, atomic from Kāra's perspective.

**Interaction with `unsafe` blocks.** The rule fires inside `unsafe { ... }` blocks too, with one narrow `unsafe` escape hatch: a function that genuinely needs to invoke FFI from within a network-boundary function may do so per the state-machine transform spec's documented exception ([State-Machine Transform — Network-Boundary Functions](#state-machine-transform--network-boundary-functions) § FFI interaction) — the `unsafe { ... }` block must not have a yield point between the FFI call's argument preparation and the consumption of its result, and the author is responsible for verifying the property with a `// Safety:` comment. The escape hatch widens the rule for niche cases (custom event-loop adapters, embedded targets, certain timer / signal integrations) but does not turn off the check — the typechecker still rejects `unsafe { ... }` blocks where the FFI / yield interleave is structurally observable in the source.

**Why not a soft warning.** Treating FFI-across-yield as a warning rather than a hard error would defer the failure mode to runtime: the foreign code would execute against torn state when the yield resumed, manifesting as memory corruption, double-frees, file-descriptor leaks, or — worst case — silent wrong results. The rule pays its cost up front (some valid programs require the fix-it restructure) to avoid an entire category of runtime defects that would otherwise be effectively undebuggable. This matches the design's broader stance: cancellation-related foot-guns are surfaced at compile time, not at runtime.

**Pure-CPU FFI (the `pure` case).** An FFI function declared `pure` ([design.md §15](design.md#extern-c)) is a pure-CPU helper that does not block. The rule applies identically: even a fast `strlen()` call cannot span a yield point in a network-boundary function. The cost of the rule is uniform across blocking and non-blocking FFI — the fix-it is the same in either case (extract the FFI work into a non-yielding helper). Treating `pure` externs specially would complicate the rule for marginal benefit; the helper-extraction pattern is cheap.

**Layer classification.** The rule "network-boundary function cannot make an FFI call across a yield point" is *Guaranteed semantics* — a conforming compiler must enforce it. The diagnostic wording is *Reported behavior* per the general carve-out. The `unsafe`-block escape hatch is *Guaranteed* (the rule's exception is part of the spec), but the `// Safety:` comment the author writes to justify use of the hatch is content the compiler does not interpret — auditing belongs to the author ([design.md §15](design.md#unsafe)).

<a id="rc-drop-ordering-across-yield-points"></a>
#### Drop Ordering Across Yield Points

When a task yields and is later cancelled at any yield point in a network-boundary function, the resources captured in the state struct are dropped in **reverse construction order** — same rule as scope-exit drop ordering applied to the state-struct fields. This subsection makes the rule explicit so user code can rely on it for resource-ordering guarantees.

**The rule.** For a network-boundary function `f` parked at yield point `Y`, let `L = [l_1, l_2, ..., l_n]` be the bindings live at `Y` in the order they were constructed in `f`'s body. When the task is cancelled, the state-struct destructor drops the bindings as `drop(l_n), drop(l_{n-1}), ..., drop(l_1)` — the reverse of construction order. This is identical to the rule for non-yielding scope exit ([core-semantics.md §7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)); the state-struct destructor is the teardown vehicle, not a new ordering policy.

**Why explicit specification.** Without this commitment, code that yields cannot rely on resource-ordering during cancellation cleanup. A function that constructs a `TcpConnection` (closing it releases an fd and notifies the peer) before a `MetricsCounter` (decrementing it updates a shared atomic) might rely on the counter being decremented *first* — so the peer-notification path observes the metric state without the connection's just-closed event having raced. Reverse-construction-order guarantees the counter drops first (since it was constructed second), then the connection. The rule is the same the language already guarantees for non-yielding scope exit; the explicit spec confirms that yielding does not change it.

```kara
fn handle(req: Request) -> Result[(), AppError]
    with sends(Network) receives(Network)
{
    let conn = TcpConnection.connect(req.target)?;     // constructed first
    let counter = MetricsCounter.acquire("requests");  // constructed second

    let response = conn.fetch(req.body)?;              // yield point — sends(Network)
    counter.record_success();
    Ok(())
}
```

If the task is cancelled while parked at the `fetch(...)` yield point:

- `counter` is dropped first (constructed second — reverse order) → metric decremented; the cancellation-recording side effect runs.
- `conn` is dropped second (constructed first) → the connection's `Drop` impl closes the fd and notifies the peer.

User code can rely on this order: the counter side-effect is visible to any observer of the metric system before the peer notification fires. Swapping the construction order in source code swaps the drop order in the cancellation path, predictably.

**State-struct field layout does not affect drop order.** The state-machine transform is free to pack the captured-locals union with overlapping storage (per [State-Machine Transform — Network-Boundary Functions](#state-machine-transform--network-boundary-functions)), reorder fields for cache locality, or split a single binding across multiple state struct fields when the live range is non-contiguous. **None of these implementation-freedom choices affect drop order** — the destructor invokes drops in the source-level reverse construction order regardless of physical field layout. The transform records construction-order metadata as part of the state struct's per-state map and the destructor consults it; field layout is irrelevant to the ordering decision.

**`defer` and `errdefer` blocks.** `defer` and `errdefer` blocks declared inside a network-boundary function follow the rule in [design.md §6](design.md#defer-and-errdefer) (the LIFO scope-exit cleanup contract for `defer` / `errdefer`): they fire at scope exit in reverse declaration order, interleaved with drops per the source-level live-range pass. Yielding does not change the interleave: the state-struct destructor invokes defer / errdefer blocks at the right scope-exit points exactly as the non-yielding path would, with drops appearing in source-level reverse construction order alongside them.

**Per-yield drop set.** The drops invoked on cancellation are the ones whose source live range includes the active yield point. A binding constructed *after* yield point `Y` is not in `Y`'s drop set (since it cannot have been constructed yet when the task parked at `Y`); a binding constructed *before* `Y` and then moved out before reaching `Y` is also not in the drop set (since it is no longer live). The transform's per-yield-point live-set construction is the same machinery the [RAII Across Yield Points](#raii-across-yield-points) check runs against; the drop set at a yield point is the same set the `CancelSafe`-bound check reads.

**Owned and shared drops.** The rule applies uniformly to owned values and to `shared` and `sync` handles. All have destructors invoked by the state-struct teardown, and all drop in source-level reverse construction order.

**Cross-package observability.** The drop order is part of the language-level contract — a library author who exports a `pub struct` with a side-effecting `Drop` impl can rely on user code observing the documented order. Stdlib types document their `Drop` side effects (e.g., `Mutex[T].Guard.Drop` releases the lock; `TcpConnection.Drop` closes the fd and sends a TCP FIN); the rule applies across package boundaries identically.

**Layer classification.** The rule "drops at a yield-point cancellation fire in source-level reverse construction order" is *Guaranteed semantics* — a conforming compiler+runtime must invoke drops in this order. The state-struct's per-state construction-order metadata representation is *Implementation freedom*. The diagnostic surface that surfaces drop order through `karac explain` (a per-yield-point drop list, when requested) is *Reported behavior* and may evolve with the usual `karac explain` discipline.

#### Cooperative Cancellation

These rules describe the cooperative cancellation planned in [design.md §13](design.md#failure). Once a branch fails or a deadline passes, a sibling's next I/O or suspending call returns `Err(Cancelled)`, which converts into the branch's error type through `From` and propagates with `?`. Nothing is cancelled in v1. `TaskGroup.cancel(ref self)` is declared in the standard library and has no semantics until this track returns ([library/concurrency.md](library/concurrency.md)).

**Cancellation is cooperative, not asynchronous.** When the scope decides to cancel siblings, it sets a flag; each sibling observes the flag at its next compiler-inserted effect-boundary check and exits with `Err(Cancelled)`. There is no `pthread_cancel`-style asynchronous interruption. The worst-case latency between a failing branch and a sibling observing cancellation is the distance between two effect-boundary checks in the sibling's control flow. Tight pure loops without effect boundaries are not cancellable mid-iteration — this is a deliberate tradeoff, because inserting preemption points into pure computation would defeat the single-pass compilation and zero-runtime-tax goals.

**Cancellation is not an effect.** A function that may cause sibling cancellation — by failing inside a `par` region, or by calling another function that does — declares nothing extra. Cancellation is a control-flow outcome of the enclosing structured-concurrency scope, not a resource interaction the effect system tracks. Making it an effect would force every `-> Result` function called inside a parallel region to widen its caller's declared effect set transitively, which would be both pervasive and un-actionable (the caller cannot opt out of being cancellable while still participating in a parallel region). Readers who want to know whether a function can be cancelled should read the surrounding `par` structure, not look for a verb in the signature.

**`Cancelled` in error return traces.** `Cancelled` is excluded from the `?`-propagation error trace ([design.md §17](design.md#error-return-traces)). A sibling propagating `Cancelled` through `?` does not push a frame onto the trace's ring buffer (the intrinsic skips the push when the `Err` variant is `Cancelled`); a frame would pollute traces with cancellation noise unrelated to the actual failure. Only the originating branch's error contributes trace frames.

**Completion wins cancellation.** A sibling branch that has already passed its last compiler-inserted effect-boundary check when cancellation fires completes naturally and returns `Ok`. Cancellation is cooperative (observed at effect boundaries), so a branch past its final check has no place left to observe the flag — it is logically already done, and its result is real work. The language rule is: **real work is never retroactively converted to `Cancelled`.** Whether that `Ok` reaches the caller is a separate question governed by the scope's return precedence: a source-earlier `Err` still dominates and the completed `Ok` is not exposed to the scope's return value (though its `defer` blocks still run normally, and its effects on mutable state remain).

**Cancellation cascades into nested regions through the same cooperative mechanism.** When an outer scope cancels a child that contains its own inner `par` region, the outer cancellation becomes an effect-boundary observation inside the child, which triggers the child's own fail-fast path, which propagates cancellation to the inner region's siblings. No special cross-scope machinery is required — the existing next-effect-boundary-check rule composes through any depth of nesting. The worst-case latency from an outer cancel to an inner sibling observing it is the sum of the effect-boundary distances along the nesting path, not a constant. This is adequate for structured concurrency; programs that need hard preemption latency bounds must use platform-specific mechanisms outside the language's standard model.

#### Scheduler-layer concerns and minimum invariants

[design.md §13](design.md#runtime) states the minimum invariants every conforming scheduler satisfies in v1: no lost work, termination and deadlock freedom. It leaves behavior under overload and fairness to the runtime. Cooperative cancellation adds two invariants, one exception to termination and one more question for the runtime.

**Additional invariants.**

1. **Cancellation is eventually observed.** A sibling branch that has been signalled to cancel observes the flag at its next compiler-inserted effect-boundary check. The worst-case latency is the distance between effect boundaries in the sibling's control flow — bounded statically for any given function body. The scheduler may not defer the observation indefinitely (no priority inversion, no starvation) once the branch reaches an effect boundary.
2. **Cancellation cascades.** Cancellation from an outer scope propagates into nested `par` regions via the cooperative mechanism above — the scheduler does not need special cross-scope machinery, but it must not block the propagation path.

**Pure-tight-loop exception to termination.** A `par` branch containing a loop whose body has no effect-boundary checks is not cancellable mid-iteration — the cooperative cancellation mechanism has nowhere to observe the flag. Termination of such branches is the programmer's responsibility. The compiler emits `warn[pure_loop_in_par]` when it detects a loop body with no effect boundaries inside a `par` branch. The programmer can suppress the warning with `#[allow(pure_loop_in_par)]` if the loop is intentionally bounded (e.g., a fixed-iteration numeric computation). The compiler does not automatically insert effect-boundary checks at loop backedges — doing so would add overhead to pure computation, which is the exact case where zero overhead matters most.

**Left to the runtime chapter.**

- **Scheduler-specific cancellation propagation beyond the cooperative baseline.** A scheduler may choose to accelerate cancellation observation — e.g., by deliberately forcing a yield at task-queue pop time, or by tracking a per-worker cancel flag that is checked on every work-steal transition. These optimizations are permitted but not required; the language-level contract is only the cooperative next-effect-boundary rule.

This will be specified in the runtime chapter per deployment profile. Programs written against the language-level rules remain valid across every future runtime; programs that additionally depend on a specific scheduler's behavior must declare that dependency explicitly in their build profile.

### Full-Hybrid State-Machine Transform (Arbitrary `suspends` Functions)

State-machine codegen for *every* `suspends` function, not just network-boundary functions. The network event loop (above) scopes the transform to functions whose effect set includes `sends(Network)` / `receives(Network)`; this entry covers the broader form where any `suspends` function — disk I/O, channel receives, custom suspending primitives — gets the same lowering. Conceptually equivalent to Tokio / async-Rust applied to arbitrary control flow rather than network-bounded code.

**Why deferred:** State-machine transform of arbitrary control flow (across `try` / `defer` / `errdefer`, drops, generics, FFI boundaries) is a multi-quarter codegen effort by itself. The cost/benefit is wrong for now: the network-boundary form's 1M+ ceiling already covers the dominant backend workloads, so going broader buys flexibility (any user-written suspending function composes naturally) without buying meaningfully more headline concurrency. Worse, full-hybrid would force RAII-across-yield from a network-boundary rule into a *language-wide* rule for every `suspends` function (a compile error for the bounded subset), and a much larger language-surface commitment if extended.

**Promotion gate:** Promote when (a) the bounded form is shipping and at least one of the deferred secondary workload classes — disk I/O at scale, channel-heavy actor patterns, custom user-defined suspending primitives — has surfaced concrete user demand for full-hybrid that the bounded form cannot serve, and (b) the language-surface design audit (RAII-across-yield, FFI-across-yield, drop ordering) has solidified to the point that extending the rules to every `suspends` function does not reopen design questions.

**Why non-breaking:** Purely additive. The network-boundary transform continues to apply; the full-hybrid lowering extends the same machinery to a broader function set without changing observable semantics for code that already worked. RAII-across-yield as a compile error widens its check surface, which can only reject additional code under the upgraded edition (per the warn-then-error edition migration policy in `design.md § Editions`).

**Cross-reference:** [design.md §16 Concurrency across targets](design.md#concurrency-across-targets).

### `select` Across Channels

**Decision:** `select` comes with this track, together with timeouts. It waits on multiple channels at once (plus `after()` timers), taking the first ready arm. It is the *first* wall a real concurrent server hits: a long-lived main loop routinely needs "a new message **or** a shutdown signal **or** a timeout," and the v1 channel API (`recv` blocks on one channel) cannot express that. The other four channel combinators (recv/send timeout, unbounded, fan-out/fan-in, priority) are in [Channel Combinators](#channel-combinators).

**Why after the channel core:** The v1 channel API (`channel[T](cap) -> (Sender[T], Receiver[T])`) is independently useful for single-producer/single-consumer handoff. `select` is the next layer once a multi-channel consumer exists — it is not needed for the channel surface to be correct, only for the server main-loop shape.

**Why non-breaking:** New syntax over existing channel ops; introduces no change to the `Sender` / `Receiver` surface. Programs without `select` are unaffected.

**Design shape:**

```kara
select {
    msg = requests.recv()  => handle(msg),
    _   = shutdown.recv()  => break,
    _   = after(30s)       => tick(),
    default                => idle(),   // optional; omit for blocking select
}
```

A block expression. Each arm is a channel op (`recv` / `send`) or an `after(Duration)` timer, followed by `=>` and a handler. The first ready arm runs; with no `default`, `select` blocks (carries `blocks` / `suspends` per the channel-op effect surface) until one arm is ready. Arms are otherwise like `match` arms — the block's value is the chosen arm's value, all arms must agree on type.

**Cross-reference:** [Channel Combinators](#channel-combinators); [design.md §13 Channels](design.md#channels) and [library/concurrency.md](library/concurrency.md) (the v1 surface this extends).

### Channel Combinators

**Decision:** Advanced channel patterns come after the v1 channel API. v1 has one bounded channel, `channel[T](cap) -> (Sender[T], Receiver[T])`, with blocking `send` and `recv` ([library/concurrency.md](library/concurrency.md)). The combinators below are the next layer, each independently shippable, together with the broader async message-passing story. `select` across channels, the most valuable of them, has its own entry: [`select` Across Channels](#select-across-channels).

1. **`recv` / `send` timeout.** `Receiver.recv_timeout(Duration) -> Option[T]` and `Sender.send_timeout(value, Duration) -> Result[(), T]`. Precedent exists in the standard library (`RequestBuilder.timeout(ms)`, `Semaphore.acquire(timeout)`); this extends the same deadline shape to channels. Subsumed by `select` with `after`, but useful standalone.
2. **Unbounded channels.** A `channel` constructor variant with no capacity; `send` never blocks. Deliberately **not** the default — bounded-with-blocking is the safe default because it propagates backpressure; unbounded is opt-in for producers provably rate-limited elsewhere.
3. **Fan-out / fan-in combinators.** MPMC convenience over the existing `Sender: Clone` (fan-in is already expressible by cloning senders; fan-out needs a shared-receiver / work-stealing wrapper). Library-level, no language change.
4. **Priority / selective receive — lowest priority.** Erlang-style "handle messages matching a pattern first." No committed design shape yet; likely a `PriorityChannel[T, Pri]` library type rather than a `recv` pattern-match, since arbitrary selective receive interacts badly with bounded buffers.

**Why deferred:** None of the four blocks v1. The bounded blocking channel covers the common producer/consumer handoff; `par`, `par for` and `TaskGroup` cover structured concurrency; and `select` covers the multi-wait main loop. The exact shapes of these four are best fixed against a real concurrent Kāra application, not designed speculatively.

**Why non-breaking:** All additive. `recv_timeout` / unbounded constructor / priority type are new APIs; none change the v1 channel API.

**Promotion gates:**
1. **First real backend Kāra app with a long-lived concurrent main loop.** These four follow as the app exercises them (a timeout need, an unbounded producer, fan-out, or priority).
2. A demo / kata where the absence of one of these four forces an awkward workaround.

**Cross-reference:** [`select` Across Channels](#select-across-channels); [design.md §13 Channels](design.md#channels).

### Par-Region Saturation Strategy Configuration

**Decision:** User-facing configuration of the `par` / worker-pool saturation strategy comes later. [design.md §13](design.md#runtime) leaves behavior under overload to the runtime: the default runtime **queues** excess work, embedded runtimes may reject, and GPU dispatch rejects at grid-size validation. What is deferred is a *user-selectable* policy (queue / fail-region / backpressure-to-caller / reject-at-spawn).

**Why deferred:** The default (queue) satisfies the minimum invariants and is correct for the common case. A user-selectable strategy is only needed once a real workload demonstrates the default is wrong for it — at which point that workload fixes the shape of the config surface. This is explicitly "runtime configuration, not the language," so it does not touch `par` syntax.

**Why non-breaking:** Additive runtime-config surface; the `par` block syntax and default behavior are unchanged.

**Promotion gate:** A real long-running Kāra service where the default queue-on-saturation causes unbounded queue growth or latency the author needs to bound — the same "first real backend app" trigger, observed specifically as saturation pressure. Until then, application code bounds its own admission, for example with a bounded channel.

**Cross-reference:** [design.md §13 Runtime](design.md#runtime).

### Cross-task `?` Propagation

Propagating `?` across task boundaries, if the services runtime needs it. There is no cross-task error flow in v1: a task's `Err` reaches whoever joins its handle ([library/concurrency.md](library/concurrency.md)).

### Unstructured `spawn`

Task spawn where the task's live range outlasts the spawning function. Kāra's v1 concurrency model is strictly structured (`par {}`, `par for`, `TaskGroup`; a task is spawned only through `TaskGroup.spawn`), which covers accept-loops and fan-out without an unstructured primitive. Unstructured spawn adds real complexity around task lifetime, error propagation, and resource cleanup; deferring it until real-world usage demonstrates where structured concurrency is insufficient keeps the v1 surface narrow. No committed design.

**Cross-reference:** [design.md §13 `TaskGroup`](design.md#taskgroup).

### Provider-Rooted Resources (trait-based injection)

> Providers return with this track, redesigned without an implicit shared handle and without `dyn` dispatch ([design.md §12](design.md#deferred)). The handle and vtable mechanics below are the previous design and are reworked when the track returns.

Resources are backed by swappable trait implementations. No algebraic effect handlers:

```
trait DatabaseProvider {
    fn query(self, sql: String) -> Result[Rows, Error];
    fn execute(self, sql: String) -> Result[(), Error];
}

effect resource UserDB: DatabaseProvider;

impl DatabaseProvider for PostgresUserDB { ... }
impl DatabaseProvider for InMemoryUserDB { ... }  // for tests

with_provider[UserDB](InMemoryUserDB.new(), || {
    process_order(test_order);  // uses in-memory DB
});
```

**Why two declarations.** `trait DatabaseProvider` names a predicate (does a type satisfy this contract?); `effect resource UserDB` names a tracked identity (which specific instance do effects route through?). The two names preserve a many-to-many relation that real programs lean on: two resources can share one trait (`UserDB` and `AuditDB` both bound to `DatabaseProvider` — two tracked identities, one contract) and a trait can exist with no resource at all (`Eq`, `Ord`, `Iterator`). Collapsing the two into one declaration would either force every trait to claim resource semantics or force every multi-resource program to introduce a second form anyway — the separation is cheaper than either collapse.

Multiple trait bounds are allowed on a resource declaration:

```kara
effect resource UserDB: DatabaseProvider + HealthCheckable;
```

Any provider passed to `with_provider` must implement all declared bounds, and its concrete type must be `CrossTask` ([design.md §9](design.md#crosstask)); see **Cross-task-safety enforcement on the provider type** below. `CrossTask` is computed by the compiler and checked on the concrete type; it is not a bound in the provider trait.

**Trait bound is optional.** A resource declaration with no trait bound — `effect resource Latency;` — is valid. A bare resource is an **annotation-only resource**: it participates in effect tracking and conflict analysis exactly like a provider-bound resource, but it has no associated provider trait and cannot be used with `with_provider` or `providers {}`. Bare resources are useful for labeling accesses to infrastructure that does not need runtime swapping — for example, an `effect resource LogFile;` used to annotate which functions write to a specific log destination, where test injection is not needed. Attempting to pass a bare resource to `with_provider[BareResource](...)` is a compile error, with a diagnostic noting that the resource has no declared provider trait. The `.Provider` associated type (used in `with_provider`'s signature) is only defined for resources with at least one declared trait bound; it is an error to reference `BareResource.Provider` for a bare resource.

**`with_provider` signature:**

```kara
with_provider[R: effect resource, T, with E, P: R.Provider](
    provider: P,
    f: Fn() -> T with R, E,
) -> T with E
```

The signature is **effect-polymorphic** over `E` — the additional effects the closure may perform beyond `R`. The closure parameter accepts any closure whose effects include `R` among others; the extra effects `E` pass through to the return type and propagate to the caller. Without this polymorphism, `with_provider` would only accept closures whose sole effect is `R`, making it unusable in real programs where a provider block always carries additional effects (e.g., `reads(Env)`, `writes(Cache)` alongside `writes(UserDB)`). Thread-safety is enforced at the call site on the **concrete** provider type — not on the provider trait, and not through a bound in this signature at all. Provider traits carry no implicit thread-safety bound; the constraint is `CrossTask`, checked against the resolved `P` at every call site and reported there when violated.

**Effect satisfaction at the `with_provider` boundary.** `with_provider[R](p, || body)` **satisfies** the resource effect `R` within the block: the compiler treats the `with R` portion of the closure's effect set as consumed at the `with_provider` boundary and does not propagate it to callers of the enclosing function. Formally, if the closure has effect set `{R} ∪ E`, the `with_provider` call contributes only `E` to the enclosing function's inferred effect set — `R` is removed. This is the rule that makes `with_provider` the authority for resource `R` in its scope: callers of a function containing `with_provider[R]` do not need `writes(R)` or `reads(R)` in their own signatures, because the resource is fully handled within. The satisfying rule covers all verbs on the resource: if the body uses `reads(R)`, `writes(R)`, `sends(R)`, `receives(R)`, all are consumed at the boundary. After the block exits, the provider is torn down; no `R` effects can arise outside the block anyway. This rule is implemented as a compiler built-in — `with_provider` is not just a library function but a recognized form that the effect checker applies the satisfaction rule to. No user-defined function can declare an equivalent effect-satisfaction behavior; only `with_provider` and `providers {}` (which desugars to `with_provider`) have this semantic.

**Concurrency semantics of `with_provider`.** `with_provider` wraps the provider in an atomically reference-counted handle internally and passes the handle to any tasks spawned within the block. This means:

- All parallel tasks within the block share the same provider instance through that handle.
- The provider's internal synchronization (connection pool semaphore, `Mutex`, etc.) handles concurrent access — this is where backpressure and concurrency limits belong, not on the effect declaration.
- Production providers (database connection pools, HTTP clients) are cross-task-safe naturally.
- Test providers with mutable in-memory state must use `Mutex` internally to be safe under concurrent access. For per-test state isolation, use `#[with_provider(...)]` on each test case — the test runner constructs a fresh provider instance per test (see [Test Attributes for Live and Substituted Resources](#test-attributes-for-live-and-substituted-resources)). `karac test --sequential` is available for tests that require genuine serialization (e.g., shared ports or files), but does not provide state isolation between tests sharing a provider instance.

The handle is an implementation detail: the programmer passes a plain value to `with_provider` and the compiler handles the rest. **Cost note:** Reading through the handle is a plain pointer dereference. The atomic refcount operations only occur on clone and drop, not on every field access. In a single-threaded test scenario, the provider is created once, used through one reference, and dropped once; the total atomic overhead is two operations (~10-40ns), which is negligible.

**Resource call desugaring.** A call of the form `Db.method(args)` inside a function with `reads(Db)` or `writes(Db)` in its effect set desugars to a vtable call through the current task's top-of-stack provider binding for `Db`. The compiler emits the equivalent of: look up the type-erased provider handle for resource `Db` in the per-task stack, then invoke `method` through its vtable. This is **runtime dispatch**: the concrete provider type is not known at the call site; the call goes through one pointer dereference and one vtable indirection, with no monomorphization per provider type.

**Capability requirement.** Calling `Db.method(args)` in a function body contributes `reads(Db)` or `writes(Db)` to the function's inferred effect set. The verb is derived from the method's **receiver mode** — `ref self` contributes `reads(Db)`; `mut ref self` and bare `self` (consuming) contribute `writes(Db)` — because the receiver mode is what bounds what the method can do to the provider, and conflict analysis must not let two provider-mutating calls run concurrently on the strength of an optimistic clause. A declared `with` clause on the trait method must be consistent with that floor: a method whose receiver implies `writes(Db)` but whose clause mentions `Db` without `writes(Db)` is unsatisfiable as written and is rejected at the trait definition (**E0412**, with a machine-applicable `ref self` rewrite). Clause effects beyond the dispatch verb — e.g. a `writes(Log)` on another resource — propagate to the call site like any callee effect. For private functions all of this is inferred automatically. For public functions it must be declared; omitting it is an effect-mismatch compile error — the function body calls into `Db` but the signature does not declare it. There is no separate "capability gate" distinct from effect verification — the declared effect IS the capability.

**No provider in scope → runtime panic.** If a resource call executes and the per-task provider stack has no binding for `Db` (i.e., no `with_provider[Db]` / `providers {}` block was entered before this call), the runtime panics with a structured diagnostic:

```
runtime panic: no provider bound for resource `db.Db`
  --> src/executor.kara:18:5
   |
18 |     Db.query_table(plan.table)
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^
   = note: resource calls dispatch through the per-task provider stack;
           enter a `with_provider[Db](...) { ... }` or `providers { Db => ... } in { ... }` block
           before calling functions that use `Db`
```

Program-rooted resources (`FileSystem`, `Clock`, `Env`, etc.) always have a default provider installed at program start and never panic this way. Only user-declared provider-rooted resources require explicit `with_provider` setup.

**Runtime mechanics.** `with_provider` maintains a per-task stack of provider maps. Entering a `with_provider[R](p, || body)` block pushes a frame binding resource `R` to a shared handle holding `p`; exiting the block pops the frame. Resource method calls inside the body (e.g., `UserDB.query(...)`) dispatch through the top-of-stack binding for that resource. Map keys are stable resource ids derived from the resolver's interned fully-qualified path — the same identity basis the type system already uses for resource equality, which makes same-instance aliasing detection a pointer comparison on the stored handle. Spawned tasks inherit the current map by sharing the handle, preserving the cost model above (one count increment on spawn, one drop on task exit). Ambient program-rooted resources (`FileSystem`, `Clock` default provider, etc.) live in the base frame installed at program start, so resource-using code outside any `with_provider` block still resolves deterministically.

**`with_provider` and parameterized resources.** `with_provider[Db](provider, ...)` always binds the **base resource name** `Db`. Inside the block, all parameterized instances — `Db["users"]`, `Db["orders"]`, `Db[42]` — route through the same provider instance. This matches the natural model: a database connection pool is one physical backend; whether a call touches the `"users"` table or the `"orders"` table, it goes through the same pool. The provider's internal implementation decides how to dispatch on the key (typically as part of the SQL it constructs or the table name it queries).

There is no `with_provider[Db["users"]](users_provider, ...)` form — parameterized resource instances cannot be individually bound. Binding is always at the base-resource level. If two parameterized instances genuinely need different physical backends, define two separate resources (`effect resource UserDb: DbProvider` and `effect resource OrderDb: DbProvider`) and bind each independently.

**Cross-task-safety enforcement on the provider type.** A `with_provider[R](provider, || ...)` call whose concrete provider type is not `CrossTask` ([design.md §13](design.md#what-may-cross-a-task-boundary)) is rejected at the call site with `E_NOT_CROSS_TASK` and the standard type-path diagnostic.

**Plain data can always be returned from `with_provider`.** The return type `T` in `with_provider[R](...) -> T` is unrestricted — `T` may be any type that does not itself capture a provider-rooted resource. Returning plain data from the block is always safe:

```kara
// Correct — Vec[Row] is plain data, does not capture Db
let rows = with_provider[Db](db, || execute(plan(q)));
assert_eq(rows.len(), 2);   // fine: rows is outside the provider scope
```

The restriction applies only to **closures and function values** that would invoke the resource after scope exit — because calling them later would use a provider that has already been torn down.

**For test fixtures, prefer `#[with_provider]` over inline scoping.** Placing the provider scope on the test case (via the attribute) avoids nesting entirely and gives each test a fresh provider instance automatically:

```kara
#[with_provider(Db, InMemoryDb.new)]
test "query returns two rows" {
    let rows = execute(plan(q));   // Db is in scope for the entire test body
    assert_eq(rows.len(), 2);
}
```

See [Test Attributes for Live and Substituted Resources](#test-attributes-for-live-and-substituted-resources) for `#[with_provider]` attribute semantics and multi-provider setup.

**Provider-rooted resources cannot escape their provider scope.** A closure that captures a provider-rooted resource is bound to the `with_provider` block that rooted it, and cannot be returned from, stored beyond, or handed out of that block. The compiler rejects any escape path — return value, field assignment, channel send, spawned task that outlives the block — with a diagnostic naming the captured resource, the provider block, and the specific escape path:

```
error: closure captures provider-rooted resource `TestClock` but escapes its provider scope
  --> tests/clock.kara:42:9
   |
40 |     with_provider[Clock](TestClock.new(), || {
   |     ------------------------------------------ Clock is rooted here; dropped at block exit
41 |         let later = |d: Duration| Clock.now() + d;
42 |         return later;
   |         ^^^^^^^^^^^^ escape path: value returned from with_provider block
   |
   = help: move the closure construction outside the provider block and take `Clock` as a parameter,
           or have the caller enter its own `with_provider[Clock](...)` scope before invoking the closure
```

**Why strict scoping.** Three properties depend on the rule:

1. **Test isolation is mechanical.** `with_provider[Clock](TestClock.new(), || { ... })` must give a test a clean clock that *disappears* at block exit. If leaked closures could keep a test's provider alive, a stray reference in test 1 could contaminate test 2 through a shared scheduler or worker pool — a bug no typechecker could catch after the fact.
2. **Teardown stays predictable.** Providers run cleanup (flush buffers, close sockets, release connection pool slots) at block exit via RAII. Extending provider lifetime to match escaping closures would make teardown run at an arbitrary later point governed by refcount, not by lexical scope — the opposite of what structured resource management promises.
3. **No ambient globals by accident.** An escaping closure holding a provider-bound resource is observationally identical to a module-level global: callers anywhere in the program can invoke it and perform effects on a resource they never requested. This is exactly the implicit-shared-state pattern ruled out by the language's sharing rules ([design.md §11](design.md#11-ownership-and-sharing)).

**Escape hatches — restructure, not relax.** Users who want a "configured helper" pattern have two options that preserve the invariant:

- **Build the closure outside the provider block**, taking the resource as an explicit parameter. The closure becomes pure over the resource axis; the provider scope is established by whoever eventually invokes it.
- **Push the provider block up to the caller.** Instead of returning a closure from inside `with_provider`, have the outermost function enter the provider block and run the caller's logic within it. This is the standard pattern for test fixtures: `#[with_provider(TestClock.new())]` on the test case, not on a helper it calls.

Neither escape hatch relaxes the rule; both restructure the code so the provider-bound resource never needs to outlive its scope.

**Scope of the rule.** This restriction applies *only* to provider-rooted resources — resources declared with `effect resource R: ProviderTrait` and injected via `with_provider` / `providers { }`. Ambient program-rooted resources (built-in primitives: `FileSystem`, `Network`, `Heap`, `Clock` under the default system provider, `Env`, `Stdin`, `Stdout`, `Stderr`, `RandomSource`) have no lifetime constraint beyond program lifetime — a closure capturing `FileSystem` is fine and the compiler places no restriction on where it travels. The distinction is deliberate: program-rooted resources are bootstrapped once at program start and live until `main` returns, whereas provider-rooted resources are scoped injections that exist for test isolation, dependency inversion, or controlled runtime swapping — and their *whole value proposition* is that they do not leak past their scope.

### `providers { } in { }` Block (multi-provider bootstrapping)

Programs typically need several providers active simultaneously. Nested `with_provider` calls create a callback pyramid; the `providers` block flattens it:

```kara
fn main() -> Result[(), AppError] {
    let config = load_config("app.toml")?;

    providers {
        OrderDB => PostgresOrderDB.connect(config.db_url)?,
        UserDB  => PostgresUserDB.connect(config.db_url)?,
        Cache   => RedisCache.connect(config.cache_url)?,
    } in {
        run_server()
    }
}
```

**Desugaring.** `providers` is syntactic sugar for nested `with_provider` calls with evaluate-all-then-scope semantics. The `in` keyword separates the provider bindings from the body block. All provider expressions are evaluated **before** any provider scope is entered:

```kara
// The above desugars to:
let __p0 = PostgresOrderDB.connect(config.db_url)?;
let __p1 = PostgresUserDB.connect(config.db_url)?;
let __p2 = RedisCache.connect(config.cache_url)?;
with_provider[OrderDB](__p0, || {
    with_provider[UserDB](__p1, || {
        with_provider[Cache](__p2, || {
            run_server()
        })
    })
})
```

**Semantics:**

- **Evaluation order:** Top-to-bottom. All provider expressions are evaluated sequentially before any scope starts.
- **Fail-fast:** If the third provider expression fails (via `?`), the first two are plain values with no active scope, so there is nothing to clean up.
- **Scope nesting:** First-listed = outermost scope (last to clean up). Cleanup runs innermost-first when the body exits.
- **Error handling:** `?` in provider expressions propagates to the enclosing function's return type. The body's return value becomes the block's return value.
- **`providers` is a reserved keyword.** It is not a contextual identifier — using `providers` as a variable name is a compile error.
- **`in` disambiguation.** `in` is already a keyword (`for x in collection`). The parser disambiguates: after `providers { ... }`, the `in` keyword is unambiguous because `providers {...}` cannot appear as the left-hand side of a `for ... in ...` expression.
- **Trailing comma:** The trailing comma after the last provider binding is optional.
- **Expression position.** `providers { ... } in { body }` is an expression — its value is the value of the body block. It may appear anywhere an expression is valid, including `let x = providers { ... } in { ... }`.
- **`?` in provider expressions** propagates to the enclosing function's return type, as shown in the desugaring above.

**When to use each form:**

| Pattern | Use |
|---|---|
| `providers { ... } in { body }` | Bootstrapping multiple providers — `main()`, test setup, server init |
| `with_provider[R](p, \|\| body)` | Single provider, mid-function scope, dynamic/conditional swapping |

`with_provider` remains the primitive for cases where `providers` adds ceremony for no benefit: single-provider injection, conditional provider swapping, or scoping a provider to part of a function.

**Provider interdependency.** Since all expressions evaluate before any scope starts, one provider cannot use another provider during construction. Use `let` bindings before the block for shared setup values:

```kara
fn main() -> Result[(), AppError] {
    // config is a plain value, not a provider — available to all expressions
    let config = load_config("app.toml")?;

    providers {
        Config  => config,                                    // also inject as provider for runtime access
        UserDB  => PostgresUserDB.connect(config.db_url)?,   // uses config value, not Config provider
        Cache   => RedisCache.connect(config.cache_url)?,
    } in {
        run_server()
    }
}
```

### Deterministic Replay via Provider Injection

The nondeterminism resources (`Clock`, `RandomSource`, `Env`; [design.md §12](design.md#resources)) are named so that they become first-class targets for `#[with_provider]`-based test injection. A test supplies a fake `Clock` that returns a fixed timestamp, a fake `RandomSource` seeded from a test vector, a fake `Env` with controlled variables, and the function under test runs deterministically with zero runtime hooks and zero test-only code paths in the function body. This is the same mechanism already described for user-defined resources ([Provider-Rooted Resources](#provider-rooted-resources-trait-based-injection)); the nondeterminism resources are blessed so that every Kāra program can use the same pattern without having to wrap the stdlib.

```
#[with_provider(Clock, FakeClock.at(1_700_000_000))]
#[with_provider(RandomSource, FakeRandom.from_seed(42))]
test "request id is deterministic under a fake clock" {
    assert_eq(generate_request_id(), "req-1700000000-6b86b273");
}
```

### Test Attributes for Live and Substituted Resources

These test attributes depend on effect resources and providers. They extend the v1 test runner ([design.md §14](design.md#14-testing)).

Tests that require live external services (databases, APIs) are marked with `#[test(requires = [...])]` on the test case. `karac test` skips tests with unsatisfied `requires`; `karac test --all` fails if any required resource is unavailable:

```
#[test(requires = [db.UserDB, payment.PaymentAPI])]
test "checkout flow" {
    // needs real database and payment service running
}
```

Names in `requires` are module-qualified paths matching the effect resource declaration (e.g., `db.UserDB`, not bare `UserDB`). This avoids collisions when multiple modules define resources with similar names.

**Resource availability probing.** The compiler uses a two-level check:

1. **Environment variable (default, zero-config).** For each resource, the compiler derives an env var by uppercasing the qualified name and replacing `.` with `_`, prefixed with `KARA_RESOURCE_`. For example, `db.UserDB` → `KARA_RESOURCE_DB_USERDB`. If the variable is set and non-empty, the resource is considered available.

2. **Health-check command (explicit override).** A `[test.resources]` table in `kara.toml` maps resource paths to shell commands. If a command is present, it takes precedence over the env var probe — the resource is available iff the command exits 0:

```toml
[test.resources]
"db.UserDB" = "pg_isready -d $DATABASE_URL"
"payment.PaymentAPI" = "curl -sf $PAYMENT_API_URL/health"
```

When a test is skipped due to an unavailable resource, `karac test` prints which resource is missing and which env var or config key to set. `karac test --all` treats any skipped test as a failure.

**Per-test provider injection.** Tests for stateful providers (e.g., an in-memory database) should receive a fresh provider instance per test. Use `#[with_provider(...)]` on the test case — the test runner wraps the test body in `with_provider(Provider.new(), || { ... })` automatically:

```
#[with_provider(db.UserDB, InMemoryUserDB.new)]
test "create user" {
    // InMemoryUserDB.new() is called fresh for this test
    create_user("alice");
    assert(find_user("alice") != None);
}
```

The argument to `#[with_provider]` is `(resource_path, constructor_fn)`. Multiple `#[with_provider]` attributes are allowed on a single test for tests that use several resources. This is distinct from `#[test(requires = [...])]`, which is for tests that need a real external service — `#[with_provider]` is for tests that supply their own in-memory implementation.

**Multi-attribute order.** When several `#[with_provider]` attributes decorate one test, source order is outer-to-inner: the first attribute is the outermost scope, the last is innermost. All constructors are evaluated before any provider scope is entered (evaluate-all-then-scope), matching `providers { } in { }`'s semantics. If a constructor fails, later constructors don't run and the test fails with `reason = "provider_construction_failed"` (see the runner output additions below).

**`requires` and `with_provider` on the same resource are rejected.** A test decorated with both `#[test(requires = [X])]` and `#[with_provider(X, ...)]` for the same resource `X` is a contradiction — `requires` gates on an external service, `with_provider` supplies a fake. The runner rejects this at discovery time and emits `test_fail` with `reason = "requires_and_with_provider_conflict"`. Different resources in the two lists are fine (e.g., `requires = [db.UserDB]` + `with_provider(Clock, ...)`).

**Built-in primitives use the same mechanism.** `#[with_provider(Clock, FakeClock.at(T))]` and `#[with_provider(RandomSource, FakeRandom.from_seed(42))]` work identically to user-declared resources. Built-in primitives (`Clock`, `RandomSource`, `Env`, `FileSystem`, etc.) register their default providers at program start; `#[with_provider]` pushes a replacement on the provider stack for the test's duration. The override is uniformly visible inside the test body via the same resource-using calls.

**Runner output additions.** The `test_fail` and `test_skip` events of [design.md §14](design.md#runner-output) gain these fields and reasons:

| Event | Fields | Notes |
|---|---|---|
| `test_fail` | `test: string`, `duration_ms: int`, `location: {file, line, col}`, `message: string` | Optional `assertion: string` (source text of the assertion), `left: string` + `right: string` for `assert_eq` / `assert_ne` failures. Optional `reason: string` for non-assertion failures. Reasons: `"provider_construction_failed"` (a `#[with_provider]` constructor panicked or returned `Err`; `duration_ms: 0` since the body never ran), `"requires_and_with_provider_conflict"` (same resource appears in both `#[test(requires = [X])]` and `#[with_provider(X, ...)]`). Optional `providers: [string]` lists fully-qualified resource paths that were active for this test (via `#[with_provider]` or program-rooted defaults); surfaced only on failure events so post-hoc triage is self-contained without source access. |
| `test_skip` | `test: string`, `reason: string` | Reasons: `"unsatisfied_requires"` (with `resources: [string]` field listing missing resource paths). Future slices may introduce new reasons — consumers must tolerate unknown reason strings. |

**Exit code.** `0` if every test passes (or is skipped under permitted conditions). Non-zero if any `test_fail` event was emitted, or if any `test_skip` event was emitted under `--all`.

### Diagnostics and Runtime Reports for Services

These additions to the v1 tooling contract ([design.md §17](design.md#17-tooling-contract)) come with this track.

- **Error return traces exclude `Cancelled`.** See [Cooperative Cancellation](#cooperative-cancellation).
- **Provider stack in the panic record.** The active `with_provider` bindings at panic time: for each provider-rooted resource, which provider value was bound and at which `with_provider` site. Lets crash analyzers see which provider configuration produced the failure.
- **`karac run` stops on execution-soundness violations.** Provider-rooted resource escape (E0600) and RAII-across-yield (`E_RAII_ACROSS_YIELD`) exit 1 before execution, same as `check`/`build`.

### Effect Variables for Stored Callbacks

v1 needs no annotation for a non-escaping function parameter: its effects come from the argument at each call. A function value that escapes has the effects its type declares, or every effect when it declares none ([design.md §12](design.md#function-values-and-effects)). Effect variables let a stored callback's effects follow the caller instead. They return with this track.

Effect variables are declared in the same generic list using the `with` keyword: `fn map[T, U, with E](list: Vec[T], f: Fn(T) -> U with E) -> Vec[U] with E`.

**Effect variables — a separate namespace.**

Effect variables (`with E`) are **not** type metavariables. They live in a separate solution map with their own rules. When checking a closure against an effect-polymorphic parameter, `E` remains symbolic until the closure body is inferred; the effects collected from the body (via the effect lattice) are then **unified** with `E`. "Unify" here is a single-assignment rule: `E` may be solved at most once per call. A second use of the same `E` that resolves to a different effect set is an effect-variable-conflict error. The conflict scope is **per call** — each call to a function with an effect variable `E` gets a fresh instance of `E`, so conflicts are local to that call site. Type metavariables and effect variables never meet on the same lattice.

### Effect Groups and Composition

Effect groups come with this track only if the services programs show a need for them. [Effect Semver Rules](#effect-semver-rules) and `stable` groups depend on them.

Named groups reduce annotation burden. Groups compose with `+`:

```
effect group Validation = reads(UserDB, InventoryDB) + sends(FraudService);
effect group Fulfillment = writes(OrderDB) + sends(PaymentGateway);
effect group OrderProcessing = Validation + Fulfillment;

pub fn process_order(order: Order) -> Result[Receipt, Error]
    with OrderProcessing
{ ... }
```

Changes to sub-groups propagate automatically. Libraries publish effect groups as API stability boundaries — callers using the group absorb individual effect changes.

**Effect groups are sugar.** A declared `effect group G = reads(DB) + writes(Cache)` expands to the atom set `{reads(DB), writes(Cache)}` before `T_f` is evaluated. Groups do not introduce new lattice elements or affect termination reasoning.

### Stdlib Scope for Non-Primitive Resources

Whether the Kāra stdlib should ship opinionated traits for common non-primitive resource categories (SQL connections, HTTP clients, KV caches, message queues, etc.) or leave all non-primitive categories to the ecosystem. Built-in *primitive* resources (`FileSystem`, `Clock`, `Env`, `Network`, `Stdin/Stdout/Stderr`, `RandomSource`, `Heap`, `Hardware`, `GpuBuffer`; see [design.md §12 Resources](design.md#resources)) are hardwired by the compiler/stdlib and not in question — they're the language-level set with compiler-known verbs. Everything else (databases, caches, HTTP clients, queues, vendor APIs) currently requires user- or library-written traits.

**Current lean:** (a) thin stdlib — ship only primitive resources. Non-primitive categories are ecosystem-defined. Rust's model. Matches the minimum-viable I/O posture of the v1 library ([library/io.md](library/io.md)).

**Rejected alternatives:**

- **(b) Opinionated stdlib** — ship `std.sql.Connection`, `std.http.Client`, etc. — premature. Kāra has no ecosystem yet; choosing the 3–5 categories and their trait shapes without real-world usage data is pure speculation. A bad `std.sql.Connection` is harder to fix than no `std.sql.Connection`. Go's `database/sql` is often cited as a success, but the ecosystem that validated its shape existed first; Kāra does not have the corresponding corpus.
- **(c) Marker traits only** — ship empty marker traits (`std.resource.Sql`, `std.resource.Http`) for category-level tooling. Unclear what problem this solves. The effect system already treats every resource as independent; parallelism analysis doesn't need categories. Thin value proposition and risk of cargo-culting.
- **(d) Drop the trait bound on `effect resource` entirely** — breaks the `with_provider` injection model ([Provider-Rooted Resources](#provider-rooted-resources-trait-based-injection)), breaks the test-substitution story, requires significant spec rewrite. Not viable.

**Why non-breaking later:** Adding stdlib traits is additive. Existing user-written traits continue to work. Libraries that want to implement the new stdlib trait do so voluntarily. The only compatibility risk is name collision (a user's `my_app.sql.Connection` won't collide with `std.sql.Connection` because they're in different modules), which is manageable.

**Re-evaluation triggers (both required):**

1. A package manager / registry exists and two or more independent libraries have shipped in *at least one* category (SQL, HTTP, cache, queue).
2. The shapes of those libraries' core traits have converged enough that a stdlib trait would codify consensus rather than impose opinion. Heuristic: at least two independent libraries share ≥70% of method signatures on the "connection" or "client" primitive.

If either condition is absent, skip — the stdlib trait would be a bet on a shape that hasn't been tested.

**Why it waits:** entirely speculative for now. No ecosystem, no empirical shape data, no urgent forcing function. The question is what posture Kāra takes once the ecosystem starts forming. Until then, any stdlib commitment is pure assumption.

### Bytes-Level Stdin and Typed Buffering

- **Bytes-level stdin.** `stdin.read_line_bytes() -> Result[Vec[u8], IoError]`, which skips UTF-8 validation. The v1 stdin reads are text-only ([library/io.md](library/io.md)).
- **Typed buffering traits.** `BufRead`-style traits that tell buffered and unbuffered readers apart in their types. v1 buffers by default (line-buffered stdin and stdout, block-buffered files) and provides `BufReader` and `BufWriter`.

### Language-Integrated Query (SQL DSL) and ORM

Whether Kāra grows a language-level query mechanism — either an embedded query syntax (LINQ / F# query expressions / sqlx-style compile-time-checked SQL strings) or a struct-to-table ORM framework (derive-macro-driven mapping, Diesel / SQLAlchemy / ActiveRecord shape).

**Distinct from** "Stdlib Scope for Non-Primitive Resources" (above), which covers the *runtime driver* question (`std.sql.Connection`). This entry is about language-level query integration on top of whatever driver exists. The two axes are orthogonal: the driver question is ecosystem-shape; the query-integration question is whether Kāra spends language-design budget on a SQL-specific surface.

**Current lean:** no language-level query DSL or ORM in v1. Users write plain function calls against whatever driver ships (user-space first, stdlib eventually per the entry above). Compile-time-checked SQL strings, if they appear, start as a library using f-string interpolation + a user-written `sql!(...)` macro once macros exist — not a language feature.

**Why deferred (not rejected):**

1. **Contracts and refinement types are load-bearing for the interesting version of this.** A SQL DSL whose distinguishing value over plain strings is *type-checked column access, row schemas, and query-composition safety* needs refinement types and compile-time row-shape tracking to land first. Shipping a DSL before those would force early commitments (how does "column exists" check at compile time? how are join row types represented?) without the primitives that make the answers clean.
2. **Comptime / heterogeneous varargs interact.** Typed row shapes like `Row[String, i64, bool]` are already named as a motivating case for [Heterogeneous Varargs / Variadic Generics](#heterogeneous-varargs--variadic-generics). A query DSL that returns strongly-typed rows depends on that feature's shape. Committing to DSL syntax before variadic generics is decided is a retrofit trap.
3. **ORM shape is an ecosystem question.** Go (`database/sql` → sqlx → sqlc → gorm), Rust (Diesel vs sqlx vs SeaORM), and Python (SQLAlchemy Core vs ORM vs Django ORM) all show the same pattern: the community explores several shapes before a consensus lifts. Kāra has no ecosystem yet; an ORM chosen now would be a bet on a shape that hasn't been tested.
4. **Effect system covers the correctness floor already.** `reads`/`writes` on a user-defined `Database` resource plus user-written `DatabaseProvider` trait already deliver the "this function touches the database" story. A DSL adds ergonomics and compile-time schema checking but not a new safety primitive.

**Why non-breaking later:** Purely additive — a new syntactic form for queries, or a new derive-macro for row structs. Existing plain-function-call driver usage continues to work. A library-level `sql!(...)` macro (once macros exist) is forward-compatible with any later language-integrated form.

**Re-evaluation triggers (any one of):**

1. [Refinement types](#refinement-types) land and stabilize, AND ≥1 user-space query-builder or ORM library has shipped and its shape suggests language-level lift would deliver value the library can't.
2. Compile-time-checked SQL strings appear as a recurring request after the macro system ships, with a clear pattern of what the library version cannot express.
3. A concrete refinement-types + effect-system interaction emerges that would make Kāra's version genuinely distinctive (e.g., effect-tracked query composition, or refinement-typed WHERE clauses that prove index usage at compile time). If the version Kāra could ship is just "LINQ, again, in Kāra syntax," skip — the value-add doesn't justify the language budget.

**If none of the triggers fire:** query integration stays library-level indefinitely. Plain-function-call drivers + a community `sql!` macro are the permanent answer. That is a valid end state, not a failure mode.

**Cross-reference:** [Stdlib Scope for Non-Primitive Resources](#stdlib-scope-for-non-primitive-resources) (driver question); [Heterogeneous Varargs / Variadic Generics](#heterogeneous-varargs--variadic-generics) (typed row shapes); [Refinement Types](#refinement-types) (prerequisite for the distinguishing version of this feature).

### Canonical Postgres Driver (`kara-postgres`) — Project-Owned Package

**Decision:** Ship `kara-postgres` with this track as a project-owned package, not stdlib. Lives at `karalang/kara-postgres`; published to the package registry; installed via `karac add kara-postgres`. **Handover-to-community policy explicitly deferred** to engineering-start time — not designing handover triggers now.

**Why ship it with the services track: dogfooding as validation.** Kāra needs `kara-postgres` to stress-test the language against real backend workloads. The driver is *internal infrastructure* for validating the effect system, `Pool[T]`, the auto-concurrency runtime, `std.http` composition, structured errors, and `with_provider` against the workloads the language is positioned to serve. A capability the project cannot exercise locally is not a ready capability. This is stronger than the launch-credibility argument: it's not "users at launch need a Postgres driver to take Kāra seriously" but "the project itself cannot validate its claims about backend workloads without exercising them against a real backend stack including database access."

**Why a project-owned package, not stdlib.** Stdlib-omission position for `database/sql`-class drivers is correct as long-term principle (see [Stdlib Scope for Non-Primitive Resources](#stdlib-scope-for-non-primitive-resources) and [Database `database/sql`-Class Stdlib](#database-databasesql-class-stdlib)). The driver lives outside `std.*`; the project owns the package as a launch artifact while the ecosystem matures. The driver should be **written to exercise the language's distinctive capabilities** — use `Pool[T]`, user-defined `Database` effect resources, `with_provider`, auto-concurrency, structured errors. Not minimum-viable Postgres driver; dogfooding-grade Postgres driver.

**Minimum viable scope:** TCP connection, prepared statements, simple-query protocol, basic type mapping (i64 / String / f64 / bool / bytes / NULL / timestamp / uuid), transactions, prepared-statement parameter binding, `Pool[T]` integration. No advanced features (LISTEN/NOTIFY, COPY, async streaming) in the first release.

**Cost estimate:** moderate — 4-6 weeks for the minimum, slightly more for dogfooding-grade. Binary protocol type-mapping surface is wide.

**Handover policy — explicitly deferred.** Re-open the handover question once the driver's actual maintenance shape is visible. The project owns it without timeline pressure to hand off. Dogfooding and handover pull in opposite directions; cannot hand off a tool used to find bugs in the language itself.

### std.crypto

Constant-time cryptographic primitives. Cryptography is one of the few domains where a wrong stdlib choice causes real-world security incidents — algorithm agility, side-channel-safe implementations, and a narrow default-secure API surface matter more than flexibility.

The implementation delegates to a vetted C library (libsodium or similar) for the primitives themselves, rather than implementing raw cryptographic algorithms in Kāra.

**Why the shape is committed now:** Cryptography is not speculative. Every networked application needs it, and getting the API shape wrong at the stdlib level is a long-term security liability. Committing the API shape before implementation prevents community libraries from proliferating incompatible interfaces that become impossible to consolidate.

**Algorithm choices (committed):**

| Purpose | Algorithm | Rationale |
|---|---|---|
| Authenticated encryption | ChaCha20-Poly1305 | Misuse-resistant; no padding oracles; fast in software; safe without hardware AES |
| Key exchange | X25519 | Widely deployed; constant-time Curve25519 DH |
| Signatures | Ed25519 | Deterministic; fast; small keys; no nonce reuse risk (unlike ECDSA) |
| Password hashing | Argon2id | Memory-hard; 2019 PHC winner; tunable time/memory cost |
| General hashing | BLAKE3 | Fast; parallel; keyed and extendable modes; not SHA-2 (which requires HMAC wrapping) |

**No algorithm agility in the default API.** `std.crypto.seal(key, plaintext)` takes a `ChaCha20Poly1305Key` — not a `dyn CipherKey`. Negotiating algorithms is the responsibility of protocol libraries (`std.tls` if it ever ships), not the primitive layer. Algorithm agility at the primitive level is where most cryptographic accidents originate.

**Effect annotations:**

```kara
// Key generation touches the OS entropy source
fn generate_key() -> ChaCha20Poly1305Key
    with reads(EntropySource) allocates(Heap)

// Seal / open are allocation-free for fixed-size output
fn seal(key: ref ChaCha20Poly1305Key, nonce: Nonce, plaintext: Slice[u8]) -> Vec[u8]
    with allocates(Heap)

fn open(key: ref ChaCha20Poly1305Key, nonce: Nonce, ciphertext: Slice[u8]) -> Result[Vec[u8], AuthError]
    with allocates(Heap)

// Hash is allocation-free if result is written to caller-provided buffer
fn hash(data: Slice[u8]) -> Array[u8, 32]   // BLAKE3, stack-allocated output
```

`EntropySource` is a stdlib-declared resource representing OS-level entropy (`/dev/urandom`, `getrandom(2)`, `BCryptGenRandom`). Functions that read entropy must declare `reads(EntropySource)` — this makes entropy consumption visible at API boundaries and allows embedded/deterministic-testing profiles to forbid it via `#[no_effect]`.

**Nonce handling:** nonces are explicit parameters (not hidden state) so callers must manage them. `std.crypto` provides a `NonceCounter` helper (increment-and-return, single-threaded) and a `RandomNonce` generator (reads entropy per call). This makes nonce reuse a visible programming decision, not a silent default.

### Constant-Time Integer Types (`CtU64`, `CtBool`)

Side-channel-resistant integer types with a restricted op set (no early-exit branches, no data-dependent timing) for cryptographic code that needs constant-time arithmetic beyond the constant-time *equality* already provided by `Secret[T]`. Typical members: `CtU8`, `CtU32`, `CtU64`, `CtI32`, `CtI64`, `CtBool`. Operations cover addition, subtraction, bitwise, conditional-move, and conditional-select — each op constant-time by construction.

**Current lean:** not in v1. [library/secret.md](library/secret.md) covers constant-time equality via `ConstantTimeEq`; constant-time *arithmetic* is additive and less load-bearing for common v1 use cases (session tokens, HMAC digests, CSRF tokens — compared, rarely arithmetic'd).

**Why non-breaking later:** new wrapper types in `std.secret.ct` (or similar). No existing `u64` op is invalidated; `CtU64` is a distinct type.

**Re-evaluation triggers (any one of):**

1. Kāra stdlib ships a cryptographic primitive (`crypto.chacha20`, `crypto.x25519`) that would benefit from language-level constant-time arithmetic rather than hand-rolled per primitive.
2. A Kāra crypto library emerges and its authors report hand-rolling constant-time arithmetic is error-prone enough to justify a language-level primitive.

**Cross-reference:** [library/secret.md](library/secret.md), the constant-time-equality primitive this builds on.

### Generalized `#[zeroize]` Attribute

A `#[zeroize]` attribute applicable to struct fields or whole types that are *not* wrapped in `Secret[T]` but should still have their backing bytes wiped on drop. Covers the case where the full `Secret[T]` wrapper is too heavy (e.g., a large existing struct with one sensitive field where rewrapping would require rethreading `.expose()` through many call sites) but zero-on-drop behavior is still wanted.

**Current lean:** not in v1. `Secret[T]` (which dispatches through the `Zeroize` trait in its own `Drop` impl) handles the common case. `#[zeroize]` is additive when the wrapper's ergonomics don't fit.

**Why non-breaking later:** new attribute on existing struct/field syntax. Absent `#[zeroize]`, current drop semantics hold.

**Re-evaluation triggers (any one of):**

1. Real Kāra code accumulates the "large struct, one sensitive field, cannot rewrap into `Secret[T]`" pattern often enough to justify an attribute shortcut.
2. `Secret[T]` usage surfaces specific composition limitations (e.g., trait bounds the wrapper introduces that block certain generic uses).

**Cross-reference:** [library/secret.md](library/secret.md), the primary mechanism; [design.md §9 `Copy`, `Clone` and `Drop`](design.md#copy-clone-and-drop), the drop infrastructure.

### Taint Tracking (`Untrusted[T]` / `Validated[T]`)

Type-level marker for data originating at an external trust boundary (HTTP body, env var, CLI arg, file contents) with a `.validate(Validator)` step that strips the marker before it reaches a sink (SQL driver, `Process.spawn`, path join, URL constructor, template engine). The lever: sinks require `Validated[T]` instead of `T` at their signature, and the compile error surfaces "this untrusted value was never validated" at every missed site.

**Current lean:** not in v1. The injection-bug class (SQL, shell, path traversal, SSRF, XSS, template) is real and worth addressing eventually, but a v1 shape for the marker + `Validator` trait + stdlib sink adoption carries too many under-designed pieces to commit to now.

**Why deferred (not rejected):**

1. **Sink-coverage gap.** For every stdlib sink that takes `Validated[T]`, there are ten that take `T`. Users routinely `.as_raw_untrusted()` to thread values through non-aware APIs — at which point the type-level guarantee dissolves. The value degrades gracefully but the *expectation* set by shipping it may not: users assume their code is safe because types compile.
2. **API-churn tax.** Every stdlib surface that accepts external input has to pick: does `std.fs.read(path: Path)` take `Validated[Path]` or `Path`? If `Path`, the marker is bypassed; if `Validated[Path]`, every caller with a plain `Path` needs to `.validate()`. The wrong pick is a daily friction; picking blind (before operational experience) is a coin flip.
3. **Validator composability is under-specified.** Is `MaxLen[10]` + `AsciiPrintable` one `Validator[String]` or two chained validators? Is a validator value-level (`.validate(NameValidator)`) or type-level (`.validate[NameValidator]()`)? Several right answers; settling them in v1 without real use cases invites retrofit.
4. **Scope creep risk.** A taint system done well involves flow-sensitive analysis (was this value *derived from* an untrusted value?), integration with the effect system (`reads(Network)` return types), and a mature validator library. The v1 scope does not accommodate all of this — a partial system is worse than none if it creates false confidence.
5. **Reserving a prelude name without behavior is worse than absence.** `Untrusted` in the prelude that implements nothing tells users the language has an opinion it doesn't actually have. Kāra has namespaces (`user_package.Untrusted` doesn't collide with `std.taint.Untrusted`), so squat-prevention is cosmetic rather than operational.

**Why non-breaking later:** purely additive. Introducing `std.taint.{Untrusted, Validated, Validator}` and migrating stdlib sinks to require `Validated[T]` in a minor version is source-compatible: existing call sites wrap inputs with `Untrusted.new(...)` + `.validate(...)`, and the signature-level contract becomes visible at call sites without invalidating typed code. Already-covered classes (memory safety, integer overflow, safe deserialization) remain unaffected.

**Re-evaluation triggers (any one of):**

1. Enough v1 stdlib surfaces (`std.http`, `std.process`, a SQL driver) ship and accumulate real-world usage that the sink set stabilizes — at which point "which surfaces require validation" becomes a concrete question rather than a speculative one.
2. A credible Kāra-shaped proposal for validator composability (free-standing `fn validate_name` vs. `Validator` trait, value-level vs. type-level dispatch, interaction with refinement types) emerges with worked examples.
3. A concrete injection-bug incident in Kāra user code demonstrates the class is not being caught by existing defenses (effect-system capability gating, parameterized resources at sink boundaries, explicit ownership transfer through parse-before-use boundaries).

**If none of the triggers fire:** injection prevention stays at the effect-system + capability-gating layer (`reads(Network)` declares external data entering a function; `sends(Db)` declares a database sink) plus convention (parse-before-use, typed query builders at the library layer). That is a valid end state — the OWASP injection class can be addressed by disciplined boundary parsing without a language-level marker.

**Open design questions to settle when this is built:**

1. **Effect-system integration.** `reads(Network)` / `reads(Env)` / `reads(FileSystem)` all produce externally-originating data. Should these functions' *return types* automatically be `Untrusted[T]`? Tentative answer: too coercive — `read_config_file` returns structured, parsed config, and by the time it returns the deserialization boundary has already produced structured data. Better: a convention that *deserialization-boundary* functions return `Untrusted[T]`, and stdlib deserializers (`json.parse`, form-decode, etc.) expose this at their API.
2. **Taint propagation — sanitizers vs. transforms vs. derivations.** Is `u.to_lowercase()` still `Untrusted[String]`? Yes — transformation preserves taint. `u.len()`: `Untrusted[i64]` or plain `i64`? Likely plain `i64` — length is not injectable content. The rule needs a coherent story: *sanitizers* strip taint (the `.validate(Validator)` step), *transforms* preserve taint (operations whose output semantically carries input content), *derivations* produce plain values (operations whose output is metadata about the input, not the input itself).
3. **Generic containers.** `Vec[Untrusted[String]]`: iterating yields `ref Untrusted[String]` by construction. `.sort()` is fine — it does not sink contents. `.join(",")` produces `Untrusted[String]` — concatenation of tainted strings is tainted. Rule: any op whose output semantically carries content from the input carries taint.
4. **Composition with `Secret[T]`.** `Secret[Untrusted[String]]` is a legal composition but stylistically confusing — one wrapper says "do not print," the other says "validate before sinking." In practice, secrets are usually produced by our own code (token mint, derive-from-master-key) rather than accepted from external boundaries, so `Secret[String]` alone suffices. When external tokens *are* accepted (`Bearer` headers from inbound HTTP), the intended flow is: `Untrusted[String]` → `.validate(BearerFormat)` → constructor-wraps in `Secret[String]`. The two stages are sequential, not nested.

**Cross-reference:** [design.md §12](design.md#12-effects), the capability primitive that already constrains *which* boundaries untrusted data can enter through; [Refinement Types](#refinement-types), the closest in-language mechanism for validated-at-boundary types without a separate marker layer; [library/secret.md](library/secret.md), the sibling wrapper with distinct semantics.

### gRPC (Streaming, Reflection, Server / Client)

A first-class stdlib gRPC stack — server, client, streaming RPCs (server-stream, client-stream, bidirectional), reflection, codegen from `.proto` files, interceptors, deadlines / cancellation. Equivalent to Go's `google.golang.org/grpc` or Rust's `tonic`.

**Why deferred:** gRPC depends on HTTP/2 (multiplexed streams, flow control, HPACK) and protobuf (wire format, codegen). Both come with this track, and gRPC sits as a layer above them. Shipping gRPC at the same time would gate the release on a dependency chain (event loop, HTTP/1.1, HTTP/2, protobuf, gRPC) where any single link's slip propagates. Better: ship HTTP/2 and protobuf first, and gRPC once both have real users.

**Promotion gate:** Promote once HTTP/2 and protobuf have shipped and gRPC user demand surfaces concretely (cloud microservices use case, internal service mesh, Kubernetes-shape integration). Prior art (Tonic for Rust) exists; the implementation is well-understood once the substrate is in place.

**Why non-breaking:** Purely additive — `std.grpc` lands as a new module with no effect on existing surfaces. gRPC's tight coupling to HTTP/2 means its addition cannot break existing HTTP/2 users (HTTP/2 stays the lower-level stable surface).

**Cross-reference:** [design.md §1 v1 scope](design.md#v1-scope).

### HTTP/3 / QUIC

HTTP/3 over QUIC, including the QUIC transport itself (UDP-based, encrypted-by-default, 0-RTT, connection migration). Equivalent in scope to Cloudflare's `quiche` or Google's QUIC implementation.

**Why deferred:** Industry-wide rollout is slow. Even Go is rolling out HTTP/3 incrementally; the IETF QUIC RFC 9000 stabilized in 2021 and ecosystem deployment is still partial in 2026. The return on an early commitment is low: HTTP/1.1 + HTTP/2 cover effectively all backend workloads, and HTTP/3 adoption is still partial for most server stacks. Building a QUIC transport is itself a multi-quarter project — UDP-level packet handling, congestion control, encryption integration, connection ID rotation, datagram extensions — and ships best once the language has ecosystem ground truth on QUIC use cases.

**Promotion gate:** Promote when (a) HTTP/3 deployment has crossed an inflection point in mainstream backends (target: >40% of Cloudflare-fronted traffic, or equivalent industry benchmarks), AND (b) Kāra's TLS substrate (rustls + aws-lc-rs) has matured QUIC integration on its side (rustls's QUIC support is ongoing).

**Why non-breaking:** Purely additive. `std.http`'s stable surface stays HTTP/1.1 + HTTP/2; HTTP/3 lands as additional negotiation paths and connection types without changing existing semantics.

**Cross-reference:** [design.md §1 v1 scope](design.md#v1-scope).

### Microservice Mesh Primitives

Service mesh primitives — service discovery, mutual TLS auto-enrollment, retry / timeout / circuit-breaker policies, distributed-trace context propagation, mesh-aware load balancing. Equivalent in scope to a service-side library that integrates with Linkerd / Istio / Consul Connect, or a sidecar-less alternative built into the language runtime.

**Why deferred:** Service mesh design is opinionated and ecosystem-divergent — sidecar (Envoy + Linkerd) vs. sidecar-less (Cilium service mesh) vs. library-mode are competing architectures with significant trade-offs. Committing Kāra to one now would foreclose options before the language has users to validate the choice. Further, mesh primitives sit above the basic backend platform — they're a layer on `std.http` + TLS + `std.tracing` rather than peer to them. Best to ship the floor first and let mesh integrations emerge from real deployment patterns.

**Promotion gate:** Promote when (a) a sustained Kāra deployment cohort surfaces concrete mesh-integration patterns (which retry / circuit-breaker shapes recur, which trace-propagation conventions stick), AND (b) the ecosystem mesh architecture (sidecar vs. sidecar-less) has stabilized enough that a library can ship without picking a losing side.

**Why non-breaking:** Purely additive — new stdlib module(s), no impact on existing `std.http` or `std.tracing` surfaces.

**Cross-reference:** [design.md §1 v1 scope](design.md#v1-scope).

### Custom Executors / Pluggable Schedulers

User-extensible scheduler — pluggable executor implementations replacing the v1 work-stealing scheduler with custom shapes (single-threaded for embedded, custom-priority for real-time, deterministic-test for property testing, custom-instrumentation for profiling). Equivalent to Tokio's `Runtime::Builder::worker_threads` plus its `LocalSet` / `current_thread_runtime` shapes, but exposed as a language-level extension surface.

**Why deferred:** v1 ships a *single* opinionated work-stealing scheduler. The cost-model decisions (which group parallelizes, fork threshold, distinctness collapse rules) are coupled to the scheduler shape, and exposing pluggable scheduler implementations now would force every language-surface commitment about parallelization to abstract over a scheduler interface that hasn't been validated. Tokio took years to settle the shape of its executor extension surface. Kāra's posture that the cost model is unspecified ([Cost Model](#cost-model)) means the right move is to ship the work-stealing default, observe user workloads under it, and let extension demands surface from concrete shape mismatches rather than speculation.

**Promotion gate:** Promote when (a) user feedback surfaces concrete workload classes the work-stealing scheduler does not serve well — e.g., real-time embedded control loops, soft-deadline scheduling for low-latency inference, deterministic property-test harnesses — and (b) the cost model has moved from unspecified to a stable shape, so the executor interface can abstract over a known cost-model contract rather than a moving target.

**Why non-breaking:** Purely additive. The work-stealing default scheduler stays the default; pluggable executors land as opt-in alternatives via a new builder / config surface. Existing programs run identically.

**Cross-reference:** [Cost Model](#cost-model); [design.md §13 Runtime](design.md#runtime) (the v1 work-stealing scheduler).

### Full-Stack Server Web Framework (Django / Rails / Spring Boot / Phoenix class)

A server-side web framework bundling HTTP server, routing, middleware, ORM integration, templating, authentication, session/CSRF, forms/validation, admin/scaffolding tooling, and observability hooks into one opinionated stack. The slot occupied by Django (Python), Rails (Ruby), Spring Boot (Java), Phoenix (Elixir), Laravel (PHP), and ASP.NET Core MVC (C#).

**Philosophy axis — design decision for when this is built.** The existing frameworks cluster along two axes:

- **Monolithic / batteries-included** (Django, Rails, Laravel) — auto-admin UI, convention-over-configuration, tight ORM integration. "You don't assemble, you customize the provided shape."
- **Modular / DI-assembly** (Spring Boot, ASP.NET Core MVC) — pick starters, compose with dependency injection. Stronger microservices story, no auto-admin default.
- **Real-time-first** (Phoenix LiveView) — server-pushed UI via WebSocket channels, distinctive shape neither of the above has.

A Kāra equivalent picks one (or blends) when it is built; this entry names the slot, not the philosophy.

**What it rests on:**
- Stdlib HTTP **server** (`std.http` is client-only in v1).
- Database driver stdlib scope: see [Stdlib Scope for Non-Primitive Resources](#stdlib-scope-for-non-primitive-resources).
- ORM story: see [Language-Integrated Query (SQL DSL) and ORM](#language-integrated-query-sql-dsl-and-orm).
- Effect system + provider injection (see [Provider-Rooted Resources](#provider-rooted-resources-trait-based-injection)). These are the distinguishing substrate.
- Macros (for templating ergonomics — `html!(...)` or equivalent — and for admin-UI reflection / scaffolding).

**Kāra-specific differentiator — effect-scoped request handlers.** Existing frameworks have no way to express "this endpoint touches the user DB and sends email but nothing else" at the type level. Kāra's effect system does: a request handler's effect set becomes part of its signature; the framework can auto-verify effect budgets per route, inject providers (DB, cache, auth, email) by type, and refuse to register a handler whose effect set violates a per-service policy. This is a shape that Django / Rails / Spring Boot cannot easily retrofit and would be the primary justification for building a Kāra-specific framework rather than targeting compatibility with an existing one.

**Why post-v1, not a stdlib ship:**

1. Each existing framework represents a decade of design iteration around a specific philosophy. Committing Kāra's stdlib to one of those philosophies early is an assumption bet the language cannot afford before it has users.
2. The substrate is not in place — HTTP server, ORM, and templating-macro primitives are all separately deferred or stdlib-scoped.
3. Admin-UI generation (Django admin, Rails scaffolding) depends on a reflection / derive-macro story that is itself deferred ([Comptime](#comptime)).
4. Effect-scoped handlers — the differentiator — only becomes meaningful once real effect-declaration patterns have emerged in user code. Designing the framework's effect-policy vocabulary before seeing idiomatic effect usage would freeze it too early.

**Pre-build checklist (all must be done before building this):**
- [ ] `std.http` server primitives shipped
- [ ] Database driver(s) available in stdlib or well-established community crate
- [ ] Macros shipped (for templating ergonomics and admin scaffolding)
- [ ] ORM story decided at the library vs. language-integrated level per [Language-Integrated Query (SQL DSL) and ORM](#language-integrated-query-sql-dsl-and-orm)
- [ ] Real-world effect-usage patterns observed in Kāra apps so the framework's effect-policy vocabulary is grounded in data, not speculation

**Cross-reference:** [Stdlib Scope for Non-Primitive Resources](#stdlib-scope-for-non-primitive-resources) (database driver scope); [Language-Integrated Query (SQL DSL) and ORM](#language-integrated-query-sql-dsl-and-orm) (ORM shape); [Provider-Rooted Resources](#provider-rooted-resources-trait-based-injection) (effect-scoped injection substrate).

### Stdlib and Ecosystem Security Conventions

Security-related conventions that are not language features but are design choices to make deliberately when the respective stdlib module or ecosystem tooling is built. Listed here so the decisions are considered rather than accreted by default.

**Safe-by-default regex engine.** When `std.regex` ships, default to a non-backtracking engine (RE2-style / linear-time matching) to prevent ReDoS. Catastrophic-backtracking regex engines are a recurring source of production outages — a non-backtracking default trades a small syntactic-feature reduction (no backreferences, no lookahead in patterns that cannot be compiled to a DFA) for a large class of DoS bugs eliminated at no cost to the caller. Explicit opt-in to a backtracking engine remains available for use cases (complex PCRE patterns, interactive text tools) where the caller controls the inputs.

**Cryptographic primitive choices.** When `std.crypto` ships, default primitives are chosen from modern, widely-reviewed designs: ChaCha20Poly1305 for AEAD, X25519 for ECDH, Ed25519 for signatures, Argon2id for password hashing, BLAKE3 for non-cryptographic hashing where performance matters. Legacy-compatible defaults (RC4, MD5 for anything, 3DES, SHA-1 for anything) are not shipped. Users needing legacy compatibility import an explicit `crypto.legacy` module — present, but always a visible choice in code review.

**`#[must_use]` on security-sensitive stdlib return types.** Stdlib functions whose return value encodes a security decision — `authenticate(...) -> Result[Session, AuthError]`, `verify(...) -> Result[(), VerifyError]`, `check_csrf_token(...) -> Result[(), CsrfError]` — carry `#[must_use]` in their signatures. Ignoring these results is almost always a bug; `#[must_use]` (a v1 attribute, [design.md §11](design.md#must_use)) makes the mistake a warning. The convention applies to stdlib design going forward; library authors are encouraged to follow.

**Supply-chain signing and SBOM.** Kāra's package manager, when it ships, is expected to support Sigstore (or equivalent) for signed releases and emit SBOM metadata (SPDX or CycloneDX) alongside build artifacts. Entirely outside the language; listed here for continuity of intent so the requirement isn't rediscovered during package-manager design.

**Cross-reference:** [design.md §12](design.md#12-effects), the capability substrate these conventions complement; [library/secret.md](library/secret.md), the primitive that handles the credential-leak vector these conventions round out.

## M4b auto-par

Automatic parallelism of loops and statements, under the proof bar of [core-semantics.md §11.5](core-semantics.md#11-concurrency-c9): no observable change (same output, same values bit for bit, same panics). `seq {}` keeps code in order where the effect system cannot see an ordering requirement. The gate is a measured speedup with zero output changes across the corpus.

Explicit `par` is v1 ([design.md §13](design.md#13-concurrency)). There, a conflict between branches is a compile error ([core-semantics.md §11.2](core-semantics.md#11-concurrency-c9)). Where the text below says conflicting operations are serialized in source order, it describes automatic parallelism only.

### Feature 5: Auto-Concurrency via Effect Analysis

No `async fn`. No colored functions. The compiler builds a dependency graph from two independent analyses and parallelizes everything not constrained by either:

**Three layers, named.** Auto-concurrency separates three concerns that are easy to conflate. *Semantic independence* — when two operations *may* run concurrently — is defined by the effect-and-data-dependency rules below. *Compiler parallelization* — when the compiler *chooses* to parallelize independent work — is a cost model that weighs expected speedup against scheduling overhead. *Runtime scheduling* — how the scheduler actually places tasks on threads — is a runtime concern (see [Runtime Phases](#runtime-phases)). The layers compose top-down: only semantically independent operations are eligible for parallelization, and only parallelized operations reach the scheduler.

**Two delivery mechanisms, named.** The three layers above describe *when* work may run concurrently; this is a separate axis — *how* eligible work reaches the runtime. There are two distinct mechanisms, easy to conflate because both are called "auto-concurrency," and they should be reasoned about as two systems rather than one. *Compute fan-out* partitions independent CPU-bound operations across a worker pool and joins their results — the path for parallel `let`-groups, associative reductions, and **loops over provably-disjoint indexed writes** (`out[f(i)] = ...`, where the compiler proves each iteration's write footprint is a contiguous range that no other iteration touches; see *Auto-Parallel Loops over Provably-Disjoint Indexed Writes* below). *Cooperative suspension* takes operations that suspend on network I/O — those whose effects include `sends` / `receives(Network)` — and, via a state-machine transform, parks them on the network event loop so a single OS thread services many in-flight operations. Both are driven by the same semantic-independence analysis above, and a program may use either or both; but they are separate codegen paths over separate runtime substrates (a thread pool vs. an epoll / kqueue / IOCP reactor) with different scaling characteristics (cores vs. connections). They are specified separately: compute fan-out in this track, cooperative suspension under [Network Event Loop and State-Machine Transform](#network-event-loop-and-state-machine-transform) in M4a.

**Independence is an optimization asset, not just a concurrency input.** The semantic-independence relation defined by rules 1–2 below is not consumed only by the thread-level fan-out this feature is named for. The same proof — that two operations cannot interfere — is exactly what the *backend* needs to widen instruction-level parallelism and autovectorize, and it is information LLVM's own pointer-level alias analysis cannot reconstruct once Kāra has lowered away the types and effects that proved it. The design commitment is therefore that the independence relation drives a **tiered** set of optimizations, cheapest and most universal first, and that *independence is necessary but not sufficient* for any of them; a cost model selects the axis:

- **Tier 0 — backend alias facts (ILP + autovectorization).** The proven disjointness is lowered to LLVM as `noalias` / scoped-alias (`!alias.scope` / `!noalias`) metadata, enabling instruction-level parallelism, memory-op reordering across the out-of-order window, and — the dominant win — autovectorization, which is a compile-time transform the hardware cannot perform at runtime. It composes with bounds-check elision (see [Karac-Side Bounds-Check Elimination Pass](#karac-side-bounds-check-elimination-pass)), which already removes the per-iteration exits that otherwise block the autovectorizer; Tier 0 supplies the *aliasing* half BCE does not. Zero runtime cost, fully static, deterministic — and it is the tier that makes *single-threaded* code faster. It is a principled, compiler-*derived* form of the no-aliasing guarantee that gave Fortran its historical HPC edge over C (and that C99 `restrict` and Rust's `noalias` express by annotation) — except Kāra derives it from ownership + effects rather than requiring the programmer to assert it.
- **Tier 1 — explicit portable SIMD (`Vector[T, N]`).** Programmer-directed data parallelism (see [Portable SIMD](#portable-simd--vectort-n)).
- **Tier 2 — thread-level parallelism (compute fan-out / `par {}` / `TaskGroup`).** The coarsest and most expensive axis; it carries spawn/join cost, so it is gated by the cost model's profitability threshold and applies to high-intensity, large-N work. This is the mechanism the remainder of this feature specifies.

**Soundness boundary (Tier 0).** Lowering an independence claim to the backend is a *correctness* boundary, not a performance knob: an over-broad `noalias` is a **silent miscompilation**, never a slowdown. Tier 0 therefore ships only behind a differential-equivalence guarantee — a fuzzed corpus must produce observationally identical results with alias metadata enabled and disabled. (The cautionary precedent is Rust's multi-year `-Zmutable-noalias` stabilization, where correct `noalias` emission repeatedly exposed latent LLVM miscompiles.)

**Profitability (the cost model).** Independence gates *eligibility*; a *data-to-code ratio* — arithmetic / operational intensity in the roofline sense — gates *profitability* and selects the axis. That ratio is frequently a **static structural** property of the code even when the iteration count N is not, giving an N-independent profitability gate compatible with the ahead-of-time determinism commitment: the compiler need not see N to know that low-intensity work (much control flow, little data) is not worth a heavier axis at any N. Pointer density is a negative signal and is statically visible from types (a dense `Vec[f64]` versus a pointer-chasing aggregate); value-semantics-by-default and SoA layout blocks push programs toward the dense, alias-free, vectorizable regime where Tiers 0 and 1 pay off.

**Determinism.** Axis selection and Tier-0 lowering are compile-time and deterministic — the same source, compiler, and target yield the same optimization decisions, with no runtime-adaptive forking. This is what keeps the `karac query` audit surfaces stable.

**1. Data dependency analysis** — if the return value of call A is used as an input to call B, B depends on A and must run after it. This is read directly from the AST; no annotation needed.

**2. Effect conflict analysis** — if A and B access the same resource with a conflicting verb pair (`reads`+`writes` or `writes`+`writes`), they are serialized. Non-conflicting accesses to different resources are independent.

Only when *both* analyses find no dependency can the compiler parallelize.

**Serialization order is always source order.** When either analysis forces two operations to be sequential, they execute in the order they appear in the source file. This applies to both data-dependency serialization and effect-conflict serialization. The programmer can rely on this: writing A before B in source is a guarantee that A completes before B starts, whenever the compiler determines they cannot be parallelized.

```
fn load_dashboard(user_id: u64) -> Dashboard {
    let profile = fetch_profile(user_id);       // reads(UserDB)
    let orders = fetch_orders(user_id);         // reads(OrderDB)
    let notifications = fetch_notifs(user_id);  // reads(NotifDB)

    // No data dependencies between the three calls (none uses another's return value)
    // No effect conflicts (different resources, all reads)
    // → compiler runs all three concurrently
    // All three results used here → sync point inserted automatically
    build_dashboard(profile, orders, notifications)
}

fn update_then_fetch(user: User) -> User {
    let id = user.id;
    save_user(user);                        // writes(UserDB)
    let updated = fetch_user(id);           // reads(UserDB)
    updated
    // Effect conflict: writes(UserDB) + reads(UserDB) → serialized
}

fn enrich_profile(id: u64) -> Profile {
    let user = fetch_user(id);              // reads(UserDB)
    let orders = fetch_orders(user.id);     // reads(OrderDB) — uses user.id
    build_profile(user, orders)
    // Data dependency: fetch_orders takes user.id → sequential
    // Effect analysis alone would permit parallelism; data dependency overrides
}
```

The dual analysis correctly handles the common cases. The remaining edge cases — cross-resource semantic dependencies not expressed via data flow (e.g., FK relationships between independently-declared resources), or side effects inside `unsafe` blocks — are the programmer's responsibility to express via explicit data flow or effect aliasing.

**`seq { }` block — forcing source-order execution.** When two operations have no data dependency and no conflicting effects, the compiler will parallelize them. If there is a semantic ordering requirement the effect system cannot see (a protocol step, a hardware register sequence, a precondition not expressible as a resource), use a `seq { }` block:

```
seq {
    init_hardware();    // returns ()
    configure_mode();   // returns ()
    enable_output();    // returns ()
}
```

All statements inside execute in source order. Auto-parallelism is suppressed for the block. The block is an expression: if the final element is a bare expression (no trailing `;`), the block evaluates to that expression's value; if the final element is a statement (trailing `;`), the block evaluates to `()`. **Scoping: `seq { }` is a normal block** — bindings declared inside are scoped to the block, consistent with all other blocks in the language. To extract values, use the block's expression value: `let handle = seq { init_hardware() };`. For multiple values, use a tuple: `let (a, b) = seq { ... (a, b) };`. This is the correct tool when `step_a()` returns `()` and threading an artificial return value would be noise. If `step_a()` returns a meaningful value, prefer expressing the dependency via data flow instead (natural and self-documenting). `seq { }` is for cases where making the dependency visible in the effect system would require an artificial resource declaration — use it sparingly.

**`seq { }` is not a code smell on its own, and the compiler does not treat it as one.** Normal builds emit no warning, note, or lint when a `seq { }` block appears, regardless of how many exist in a module or project. Some domains (hardware initialization, protocol state machines, explicit register sequences) legitimately need many sequential regions, and a threshold-based lint would either fire on correct code or be silenced project-wide — both outcomes destroy the signal. However, the *frequency* of `seq { }` across a codebase is a useful **language-health metric**: if real-world Kāra code reaches for this escape hatch often, that is feedback to the language designers that the effect system may need additional resources, finer-grained parameterization, or new built-in verbs to capture the ordering constraint users are expressing manually. When `karac build --perf-report` lands (future tooling, not specified here), `seq { }` block count and locations will be one signal among several — alongside monomorphization sizes, unblocked-parallel-region counts, and other auto-parallelism diagnostics — surfaced only when the user opts in. The distinction is deliberate: the language-health metric belongs in a tool users run occasionally and designers read regularly, not in the noisy feedback loop of normal compilation.

**Resource-modeling friction signals.** The same language-health rationale extends beyond `seq { }` frequency to other resource-modeling patterns; see [Resource-Modeling Friction Lints](#resource-modeling-friction-lints).

**Frozen handles.** Auto-concurrency exempts places rooted at a frozen parameter from the cross-task-safety gate, so a loop touching one reports `gate: "proven"` and fans out instead of reporting `not_cross_task_safe`. The exemption is keyed on **place roots, never on types**: a body holding both a `frozen S` and an ordinary `S` parameter must keep the second refused, and a type-keyed exemption would clear both. [design.md §11](design.md#frozen-handles) covers frozen handles in `par {}`.

### Resource-Modeling Friction Lints

Compile-time advisory lints that flag suspicious resource-modeling patterns in user code: dense `independent A, B;` declarations across related resources (over-fragmentation hint), `resource` declarations that exist only to force ordering between independent operations (phantom-resource hint), and other heuristics for "you may be modeling resources too coarsely or too finely." Triggered through `karac build --perf-report`, not in normal compilation — the language-health-metric framing in [Feature 5](#feature-5-auto-concurrency-via-effect-analysis) governs.

**Why deferred:** Without enough real-world Kāra programs, speccing specific lints is guesswork — patterns that look like misuse in one domain are legitimate in another. Revisit once production codebases reveal which patterns reliably indicate modeling errors.

**Why non-breaking:** Lints are warning-level, suppressible with `#[allow(...)]`, and do not change auto-concurrency decisions, conflict analysis, or runtime behavior. Existing programs continue to compile and run identically; users see new advisory diagnostics they may opt to act on.

### Auto-Parallel Loops over Provably-Disjoint Indexed Writes

The third compute-fan-out shape, alongside parallel `let`-groups and associative reductions. **There is no syntax for it** — the ordinary sequential `for` is the surface, and a qualifying loop fans out unchanged:

```kara
// Runs on the worker pool. Nothing in the source says so.
for dy in 0..dh {
    for dx in 0..dw {
        for c in 0..4 { out[(dy * dw + dx) * 4 + c] = sample(src, dy, dx, c); }
    }
}
```

**What is proven.** The compiler expands each indexed write into a linear form over the loop variables in scope, whose coefficients are symbolic polynomials in loop-invariant quantities — linear in the loop variables, deliberately *non-linear* in the invariants, because `dy * dw` is not affine over `{dy, dw}` but is `dy` scaled by the symbolic coefficient `dw`, which is what a contiguous footprint looks like. The outer variable's coefficient is the **stride** `S`, the invariant part is the tiling's **base** `B`, and the inner variables' terms form the **residual** `R` bounded by each inner loop's range. Two obligations discharge the proof: `Rmin >= 0` and `Rmax < S`. Together they place iteration `v`'s writes in `[B + v*S, B + (v+1)*S)` — half-open ranges at a fixed positive stride, hence pairwise disjoint. This covers image kernels, Game of Life steps, row-wise matrix multiply, and volumetric nests.

**This is not dependence analysis.** There is no solver and no polyhedral model. Indirect indexing (`out[idx[i]]`), overlapping windows, and reductions into a shared slot are out of scope and **decline** — falling back to sequential execution plus a queryable reason.

**Every decision is queryable, and that is what replaces an annotation.** Silent parallelization is only trustworthy if a developer can ask whether it happened; `karac query concurrency` reports, per loop, whether the footprint proof discharged, whether the binary actually fans out, and — when it does not — which obligation or cost gate declined it. The answer to "why isn't my loop parallel" is a compiler explanation, not an override keyword.

**What still declines even when the footprint is disjoint.** A body that writes to the console (the fan-out substrate does not replay output in source order, and *auto-par never changes what your program prints*), one that performs a resource effect two concurrent iterations would reorder, one that touches a `shared` value (whose refcount is not atomic), and one whose per-iteration work is too small or too memory-bandwidth-bound for dispatch to pay.

**Hard gate: differential harness before broad enablement.** Wrong disjointness on indexed writes is a **silent miscompile**, never a perf regression; the cautionary precedent is rustc's multi-year `-Zmutable-noalias` stabilization. Fan-out-on must be observationally identical to fan-out-off across a fuzzed corpus, with an order-sensitive oracle (a position-folded digest over the whole output, not a single element: a spot-check passes under reordering).

**Why non-breaking:** purely additive to what the compiler *chooses* to do. No syntax changes; `seq {}` remains the opt-out; programs that don't qualify are unaffected. Programs that do qualify get faster with byte-identical output, which the differential harness proves.

### Composition with SIMD

Auto-concurrency (across cores) and SIMD (within a lane) are independent dimensions of parallelism. The runtime composes them without coordination:

- **Multiplicative on workloads that admit both.** Auto-par splits the iteration space across N cores; SIMD chunks each thread's slice into K lanes. Peak speedup is N×K on the embarrassingly-parallel + auto-vec-friendly case (e.g., element-wise Tensor arithmetic over a large flat buffer). Real-world ceilings are lower (cache contention, scheduling overhead, NUMA effects), but the model is correct.
- **No SIMD-lane migration across threads.** Threads cooperate only at chunk boundaries — work is split per-thread before any SIMD ops execute, and aggregated only after each thread's SIMD work completes. The runtime does not move individual SIMD lanes (or partial-vector state) across thread boundaries; that would require cross-thread vector-register coordination, which neither LLVM nor any production OS scheduler provides cheaply.
- **`#[require_simd]` is orthogonal to auto-par.** A `#[require_simd]` function inside a `par for` body must still vectorize — which means the function body must remain auto-vec-friendly (no early exit, no function calls into non-vectorizable code, no aliasing-defeating patterns) even though the outer loop is parallelized. The `#[require_simd]` diagnostic fires on the inner SIMD-fallback path; the outer auto-par decision is unchanged by the attribute.

The compositional model means: when writing performance-critical numerical code, use `Vector[T, N]` (or hand-vectorized stdlib kernels per [Hand-Vectorized Data-Spine Commitment](#hand-vectorized-data-spine-commitment)) for the inner lane-level work, and let the auto-par effect analysis split the outer iteration space across cores automatically. No additional annotation is required to opt into the combination.

<a id="cost-model--v1-status"></a>
### Cost Model

The "Three layers, named" framing above separates *semantic independence* (when operations *may* parallelize, governed by data-dependency and effect-conflict rules) from *compiler parallelization* (when the compiler *chooses* to, governed by a cost model). **The cost model is deliberately not specified.** This subsection documents what is and is not committed.

**What is specified.** Semantic independence is fully specified: two operations may parallelize iff they have neither a data dependency nor an effect conflict. Anything the spec says about "the compiler can parallelize these" is a statement about eligibility, not about whether the compiler will actually fork them.

**What is not specified.** The cost model itself — the per-call cost heuristic (how the compiler classifies a function body as cheap/medium/expensive based on its shape — pure arithmetic vs. allocation vs. I/O vs. effect class), the fork threshold (when sum-of-costs exceeds scheduling overhead), the loop-body parallelization rule (whether iterations of a `for` loop with a pure body fork), and the distinctness policy when parameterized-resource keys are dynamic — is left unspecified. Specifying it without empirical measurements would either lock in numbers that turn out to be wrong, or ship a model so vague it constrains nothing.

**Interim policy.** The current implementation uses a degenerate cost model: parallelize a group whenever (a) semantic independence permits it, and (b) the group is not trivial (it contains at least one call that is not pure computation). Trivial pure-computation groups are emitted sequentially; everything else forks. This is intentionally simple, and it avoids commitment to numbers that empirical tuning will reset.

**Specification work.** The full cost model lands once the cumulative cost surface ([Cumulative Cost Surface](#cumulative-cost-surface)) provides ground-truth measurements to tune against. That spec will pin the per-call cost heuristic, fork threshold, loop rule, and distinctness policy as Reported Behavior — stable within a compiler release, may improve across releases (every change itemized in the release changelog, the same discipline applied to inferred private-function effects).

**Determinism guarantee — same source, same compiler, same target → same parallelization.** Whatever the cost model becomes, its decisions are a pure function of (source, compiler version, target profile). Two compilations of the same source by the same compiler version targeting the same profile must always produce the same parallelization graph. Across compiler versions or target profiles, the model can change. This determinism is what makes `karac query concurrency` a useful audit surface — running it on Monday and Friday gives the same answer for the same source. (Cost-model decisions affect compile-time choices, not runtime scheduling — the runtime scheduler retains its existing freedom over task placement and ordering across threads; see [design.md §2](design.md#2-specification-layers).)

**Layer classification.** Cost-model decisions are *Reported Behavior* — see [design.md §2 Seed classification](design.md#seed-classification). Tools that consume `karac query concurrency` must tolerate version-skew on which groups the compiler chose to fork; the *eligibility* of a group to fork (semantic independence) is *Guaranteed* and unaffected by cost-model changes.

**`--sequential`.** Non-conflicting effects mean order doesn't affect results. `karac build --sequential` is a debug-only, global, compile-time flag that disables all auto-parallelism, useful for isolating correctness bugs where parallelism is suspected. It is not for production use. For per-scope sequential execution in production code, use a `seq { }` block (see [Feature 5](#feature-5-auto-concurrency-via-effect-analysis)).

### Determinism Contract

[design.md §13](design.md#determinism-contract) states the v1 determinism contract for `par {}` and `par for`: the property, the guarantees that give it, and what is not guaranteed. Automatic parallelism must keep that contract and change nothing observable ([core-semantics.md §11.5](core-semantics.md#11-concurrency-c9)). It adds two guarantees:

1. **Compile-time parallelization-graph determinism.** Same (source, compiler version, target profile) produces the same parallelization decisions. See [Cost Model](#cost-model). This is what makes `karac query concurrency` stable across runs.
2. **Source-order serialization on conflict.** When two operations have either a data dependency or a conflicting effect on the same resource, they execute in source order, deterministically. See [Feature 5 — Serialization order is always source order](#feature-5-auto-concurrency-via-effect-analysis).

**The `for`-loop case.** A `for i in 0..N { body }` with a side-effecting body — `println(i)`, file writes, accumulator updates — runs deterministically, in iteration order. For most resources the reason is rule (2): every iteration shares the same effect resource (`writes(Fs)`, a user resource, …), that resource conflicts with itself, and the conflict serializes iterations in source order. The **console** resources reach the same guarantee by a different route, because `writes(Stdout)` / `writes(Stderr)` are deliberately non-conflicting with themselves: a body whose only effects are console writes is classified trivial and is not forked at all, and console output that *does* end up inside a parallel region is captured per branch and replayed at the join in source order. Either way the observable order is the sequential one — but do not read the console case as an instance of conflict serialization; it is not one. The cost model that decides whether *pure* `for` bodies fork (see [Cost Model](#cost-model)) does not change this property: pure bodies have no user-observable ordering to non-determinize, and side-effecting bodies keep the ordering guarantee above — by conflict for a self-conflicting resource, by the trivial-body classification and ordered replay for the console ones. The cost model is unspecified-but-deterministic, not unspecified-and-non-deterministic.

### Auto-Parallel Regions and Cleanup

These rules extend `defer` and `errdefer` ([design.md §6](design.md#defer-and-errdefer)) to a scope that contains auto-parallelized work.

- **Implicit join barrier before cleanup.** When the scope contains auto-parallelized work, all parallel tasks spawned in the scope must complete before any `defer` or `errdefer` block runs. This is a direct consequence of structured concurrency — parallel regions join before the enclosing scope can exit, and cleanup runs on exit — stated explicitly here so the ordering is unambiguous. Consequence: a parallel task may legally borrow a binding that is later referenced by a `defer`/`errdefer` block, because the task joins strictly before the cleanup fires.

- **Cleanup effects do not serialize with the parallel region.** Because cleanup runs after the join barrier — not interleaved with the main body — the effects inside `defer`/`errdefer` never cause the parallelized statements inside the scope to be sequentialized against each other. Cleanup effects only contribute to the enclosing function's inferred effect summary (and thus to its callers' parallelization decisions).

- **Failure inside a parallel region.** When the enclosing scope contains auto-parallelized work and a branch fails with `Err`, no sibling is cancelled. Every branch runs to completion, every branch's cleanup runs on its own exit path, the scope's own `defer`/`errdefer` runs only after the join barrier, and the source-earliest error is returned. A panic ends the process ([core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8)). These are the rules of explicit `par` ([design.md §13 Failure](design.md#failure)).

### `karac query concurrency` Output Schema

This query joins the compiler query API ([design.md §17](design.md#compiler-query-api)) with this track. It returns the compiler's full parallelization graph for a function — every call, which stage it runs in, why calls are sequential or parallel, and (where applicable) what the programmer would need to declare to unlock parallelism:

```json
{
  "function": "load_dashboard",
  "stages": [
    {
      "type": "parallel_group",
      "calls": [
        {"call": "fetch_profile(user_id)", "line": 2, "effects": ["reads(UserDB)"]},
        {"call": "fetch_orders(user_id)",  "line": 3, "effects": ["reads(OrderDB)"]},
        {"call": "fetch_notifs(user_id)",  "line": 4, "effects": ["reads(NotifDB)"]}
      ],
      "derivation": [
        {"reason": "no data dependencies between fetch_profile, fetch_orders, fetch_notifs"},
        {"reason": "no effect conflicts — different resources, all reads"}
      ]
    },
    {
      "type": "sync_point",
      "derivation": [
        {"reason": "build_dashboard takes return values of all three fetches"}
      ]
    },
    {
      "type": "sequential",
      "call": "build_dashboard(profile, orders, notifications)",
      "line": 7,
      "effects": []
    }
  ],
  "serialized_pairs": []
}
```

Each serialized pair includes a `"kind"` tag (`"data_dependency"` or `"effect_conflict"`) and a `"would_parallelize_if"` hint — `null` when the ordering is semantically required (read-after-write, data dependency); a description of the declaration needed (e.g., `independent` resources) when parallelism is blocked only by conservative defaults:

```json
"serialized_pairs": [
  {
    "a": "save_user(user)",
    "b": "fetch_user(user.id)",
    "kind": "effect_conflict",
    "conflict": "writes(UserDB) vs reads(UserDB)",
    "would_parallelize_if": null
  },
  {
    "a": "write_audit_log(event)",
    "b": "update_record(id, data)",
    "kind": "effect_conflict",
    "conflict": "writes(AuditDB) vs writes(UserDB)",
    "would_parallelize_if": "declare `independent AuditDB, UserDB;` — safe if audit and user tables are on separate connections"
  }
]
```

`"would_parallelize_if"` is a hint string, not a machine-applicable diff — declaring resources `independent` has semantic implications the programmer must verify. Machine-applicable diffs (e.g., missing effect annotations) appear only in `karac build --output=json`.

### Deterministic Parallel Runtime as a Standalone Library

The auto-concurrency runtime — work-stealing scheduler, task groups, the join model — packaged as a library consumable from Rust (and via C ABI from elsewhere), carrying the property that makes it distinctive: the determinism contract. Same source, same compiler, same target yields the same parallelization, and any two operations the analysis cannot prove independent keep source order. The audience is people who will not adopt a new language but do want reproducible parallelism.

**Honest scope limit.** The determinism contract is a property of the *compiler's* analysis, not of the runtime substrate alone. Extracted without the effect checker, the library ships a scheduler, not the guarantee — the caller becomes responsible for declaring independence, which is precisely the burden the language removes. This entry is therefore a genuinely reduced product, and that reduction must be stated plainly wherever it is published, or it misrepresents what Kāra does.

**What it rests on:**
- The auto-concurrency runtime, the substrate.
- `runtime/` already builds as a `staticlib` for AOT linking, so a consumable artifact is close to what exists; the work is API design, not extraction.
- The exported C ABI ([Exported C ABI](#exported-c-abi)), the producer direction this would ship through.

**Why post-v1, not a compiler ship:**
1. It competes with `rayon`/`tokio` on their turf while shipping strictly less than Kāra does — worth doing only if it functions as a funnel back to the language, which is a marketing judgment to make after 1.0, not before.
2. A second public API surface is a second compatibility commitment; taking that on while the language's own surface is pre-1.0 doubles the freeze cost.
3. The interesting claim (determinism) is only fully true inside the language, per the scope limit above.

**Pre-build checklist (all must be done before building this):**
- [ ] Kāra 1.0 shipped — no second API commitment before the first is frozen.
- [ ] Determinism contract's exact boundary written down for an out-of-language caller (what the library guarantees vs. what the compiler guarantees).
- [ ] Decision that a funnel-back-to-Kāra story exists; without one this is effort spent competing with `rayon` for no strategic return.

**Cross-reference:** [Determinism Contract](#determinism-contract); [design.md §13 Determinism contract](design.md#determinism-contract).

## M4c data

The `kara-data` package: `Tensor`, `Column`, `DataFrame`, statistics (`Stats`, `Reduce`), autograd and Arrow interop, with the reduced-precision floats `f16` and `bf16`. It comes after the v1 release. These are package types, not prelude or `std` types; where an entry below says "stdlib" or `std.*` for them, read the `kara-data` package. The `Numeric` bound returns with this track and with [GPU](#gpu).

### Numerical Types (Tensor, Column, DataFrame)

Three stdlib types carry the numerical and data-frame story: `Tensor[T, Shape]` for dense N-dimensional arrays, `Column[T]` for nullable 1D data, and `DataFrame` for schema-bearing tables of columns. They are types of the `kara-data` package, not keywords. All three commit to Apache Arrow's memory layout — see [Memory Layout Commitments](#memory-layout-commitments-arrow) below.

#### Tensor — shape types

`Tensor[T, Shape]` is an N-dimensional dense container carrying shape information in the type:

```kara
Tensor[f64, [3, 4, 5]]      // fully static — all three dims known at compile time
Tensor[f64, [3, 4, ?]]      // partial — first two dims static, third determined at runtime
Tensor[f64, [?, ?, ?]]      // rank-3, all dims runtime
```

**Shape is a new generic-parameter kind.** Alongside type params (`[T]`) and const-expr params (`[const N: i64]`; see [design.md §8](design.md#const-generic-parameters)), Kāra has a third generic kind: `Shape`, a type-level list of dims. A shape literal `[3, 4, ?]` in type-argument position constructs a shape. The `?` token — reused from the expression-level question-mark operator — is a dynamic-dim marker legal only inside a shape literal.

Dim-kinded generic params are inferred from context: a param appearing only in shape position is `Dim`-kinded without an explicit annotation. Explicit annotation is available for clarity: `fn reduce[T, N: Dim](t: Tensor[T, [N]]) -> T`.

**Generic dims with relations.** Shape params may appear in multiple positions of the same signature, and the compiler unifies them at type-check time:

```kara
fn matmul[M, K, N](
    a: Tensor[f64, [M, K]],
    b: Tensor[f64, [K, N]],
) -> Tensor[f64, [M, N]]
{ ... }

let a: Tensor[f64, [3, 4]] = ...;
let b: Tensor[f64, [4, 5]] = ...;
let c = matmul(a, b);       // inferred: Tensor[f64, [3, 5]]

let wrong: Tensor[f64, [7, 5]] = ...;
matmul(a, wrong);
// compile error[E_SHAPE]: K dim mismatch
//   in matmul(a, wrong)
//   a's K = 4, wrong's K = 7
```

Every operation where dim checking catches real bugs — matmul, conv, dot product, elementwise-same-shape, gather, scatter — is expressible in this form.

**No `@` matmul operator.** Python's `@` operator (PEP 465) is the ML convention for matrix multiplication. In Kāra, `@` is the pattern-binding operator (`value @ pattern` in match arms and `let` bindings) and cannot be repurposed. The idiomatic forms are `a.matmul(b)` (method) and the free function `matmul(a, b)`. This is a settled decision; `@` as pattern binding predates the numerical stdlib and the conflict has no clean resolution.

**Shape-param arithmetic (`[A + B]`, `[N * 2]`) comes later.** Requires a type-level const-evaluator. Concat, reshape-by-factor, split-along-dim become shape-checked when arithmetic ships; until then, these operations return partially-dynamic shapes. (See [Shape-Parameter Arithmetic](#shape-parameter-arithmetic-a--b-n--2).)

**Variadic rank polymorphism uses `...S` syntax.** A shape-variadic param binds the full shape list of its argument:

```kara
fn reduce[T, ...S](t: Tensor[T, S]) -> T { ... }
fn transpose[T, ...S, M: Dim, N: Dim](
    t: Tensor[T, [...S, M, N]],
) -> Tensor[T, [...S, N, M]]
{ ... }
```

**Dynamic-dim unification.** When a call site has a `?` in one position, it unifies with any concrete or generic `Dim` on the other side and degrades the result's corresponding position to `?`:

```kara
let a: Tensor[f64, [3, ?]] = ...;
let b: Tensor[f64, [?, 5]] = ...;
let c = matmul(a, b);       // inferred: Tensor[f64, [3, 5]] — both ?s unify with the concrete dims
let d: Tensor[f64, [?, ?]] = ...;
let e = matmul(d, b);       // inferred: Tensor[f64, [?, 5]] — left M stays dynamic
```

This preserves type-level information where possible and degrades gracefully where not, instead of forcing a per-call choice between "fully static" and "fully dynamic."

**Runtime equality check for unified `?` dims.** When the type checker unifies two dynamic dims that must be equal — because both map to the same generic `Dim` parameter (e.g., `K` in `matmul`) — neither side is known at compile time, but correctness still requires them to agree at runtime. The compiler inserts a runtime assertion at the call site:

```kara
let a: Tensor[f64, [3, ?]] = Tensor.zeros([3, 4]);
let b: Tensor[f64, [?, 5]] = Tensor.zeros([7, 5]);   // K=4 vs K=7

let c = matmul(a, b);
// compiler inserts: assert(a.shape[1] == b.shape[0], "shape mismatch: K dim (4 != 7)")
// panics at runtime with the above message
```

The assertion fires before the operation begins, so no out-of-bounds memory access can occur. The call contributes a `panics` effect to the enclosing function (joining any `panics` already present from other sources). When one side is a concrete dim and the other is `?`, the compiler emits a bounds check against the static value rather than a full equality check (`assert(a.shape[1] == 4, ...)`), which may be folded by the optimizer. When both sides are concrete, the check is resolved at compile time and no code is emitted.

**Indexing.** `t[i, j, k]` is multi-dimensional indexing on a rank-3 tensor. The parser desugars it to `t[(i, j, k)]` — a single tuple passed to `Index.index`. `Tensor[T, [M, K, N]]` implements `Index[(i64, i64, i64)]`; the two forms are exactly equivalent. `t[i]` on a rank-3 tensor is a **compile error** because there is no `Index[i64]` implementation for a rank > 1 tensor — use `t[i, :, :]` style slicing (which comes later) or index all dimensions explicitly. When all dims are static, bounds are checked at compile time; when one or more dims are `?`, bounds are checked at runtime. `t.shape()` returns the shape as a runtime value for inspection.

#### Broadcasting

Broadcasting is **implicit for scalar-tensor operations** and **explicit for tensor-tensor** operations where a dim differs.

**Implicit scalar broadcasting:**
```kara
let arr: Tensor[f64, [3, 4]] = ...;
arr + 1.0                   // OK — scalar broadcasts, each element adds 1.0
arr * 2.0                   // OK
arr > 0.0                   // OK — Tensor[bool, [3, 4]]
```

A scalar in this position is `T` or `Tensor[T, []]` (rank-0 tensor); the operator trait handles both identically.

**Tensor-tensor requires exact shape match:**
```kara
let a: Tensor[f64, [3, 4]] = ...;
let b: Tensor[f64, [3, 4]] = ...;
let c: Tensor[f64, [1, 4]] = ...;

a + b       // OK — shapes match
a + c
// compile error[E_SHAPE]: tensor-tensor operator requires exact shape match
//   a: Tensor[f64, [3, 4]]
//   c: Tensor[f64, [1, 4]]
//   note: to broadcast c over a, use a.broadcast_add(c)
```

**Explicit tensor-tensor broadcasting via methods:**
```kara
a.broadcast_add(c)          // broadcasts [1, 4] over [3, 4] — shape-checked at compile time
a.broadcast_mul(col_vec)    // broadcasts column vector over matrix
```

Broadcasting methods shape-check their arguments against standard expand-singleton-dim rules at compile time when shapes are static. Rationale: scalar broadcasting is universally expected; NumPy-style implicit tensor-tensor rules (trailing-axis alignment, singleton expansion) are exactly where runtime bugs breed. Forcing tensor-tensor broadcasts through named methods preserves shape-type safety.

Fully-typed broadcasting (where the type system encodes broadcast compatibility and rejects incompatible cases at compile time) is a research target.

#### Column — nullable 1D data

`Column[T]` is the general nullable 1D primitive — a bitmap-backed column matching Apache Arrow's layout:

```
Column[T] = {
    data:         *contiguous buffer of T
    null_bitmap:  *bitmap, 1 bit per element, 0 = null, 1 = valid
    len:          i64
    capacity:     i64
}
```

Zero per-element indirection; ~12% memory overhead for the bitmap; SIMD-friendly.

**NaN as a float convention.** For `Column[f64]` / `Column[f32]`, users may additionally rely on IEEE-754 NaN as a per-element sentinel — real-world float data (CSVs, scientific measurements) routinely uses NaN for "bad reading." Stdlib methods honor both conventions via an argument:

```kara
let prices: Column[f64] = Column.from_arrow(...);
prices.null_count()                                 // bitmap-based
prices.fillna(0.0)                                  // replaces bitmap-nulls only (default)
prices.fillna(0.0, treat_nan_as_null: true)         // also replaces NaN values
```

**Null propagation uses SQL semantics:** `null + x = null`, `null == null = null` (three-valued logic). Stdlib methods maintain the bitmap through arithmetic; users who want "treat null as zero" opt in via `.fillna(0.0)` first. Matches Arrow, Polars, DuckDB, and SQL.

**SQL null semantics apply only to bitmap-null elements. NaN is not bitmap-null.** A `Column[f64]` element that is NaN but whose null-bitmap bit is valid (1) is treated as a valid float value — arithmetic on it follows IEEE 754 (`NaN + x = NaN`), not SQL three-valued logic. The result value is the same in this case, but the result's bitmap bit stays valid (not null). This means `null_count()` returns 0 for a column of NaN-only values, and `fillna(0.0)` (without `treat_nan_as_null: true`) is a no-op on them. Code that uses `null_count()` or bare `fillna` as a correctness signal must normalize NaN-as-null to bitmap-null first:

```kara
// Normalize before operating — ensures consistent null semantics
let prices = raw_prices.fillna(0.0, treat_nan_as_null: true);
let result  = prices + other;    // NaN-originated values are now bitmap-null; SQL semantics apply
```

There are no operations where the two conventions are unified automatically — NaN is never promoted to bitmap-null implicitly. The two representations coexist in `Column[f64]` because real-world float data uses both, and silently conflating them would mask distinct missing-data causes.

**`Tensor` is dense-only.** Tensors have no nullability option. Numerical computing (BLAS, SIMD, GPU kernels) assumes validity per element; nullability would be dead weight 99% of the time. Users needing a nullable numerical array use `Column[f64]` and convert to `Tensor` when they need dense numerical work.

**Variance divisor — `Stats` is population, `Column` is sample, deliberately.** The two stat surfaces disagree on the denominator, and the disagreement is a commitment, not drift:

| Call | Divisor | `n = 1` | `n = 0` |
|---|---|---|---|
| `Stats.variance(s)` / `Stats.stddev(s)` | `n` (**population**) | `0.0` | traps |
| `col.var()` / `col.std()` | `n − 1` (**sample**, Bessel) | traps | traps |
| `gpu.variance(v)` / `gpu.stddev(v)` | `n` (**population**) | `Some(0.0)` | `None` |

Each surface matches the default of the ecosystem it is modeled on: `Stats.*` is a plain numeric-slice API in the shape of NumPy, whose `np.var` defaults to `ddof=0`; `Column`/`DataFrame` is the dataframe surface, in the shape of pandas, whose `Series.var` defaults to `ddof=1`. Unifying on either divisor would surprise one of the two audiences at the point where they are least likely to check — a variance that is silently off by `n/(n−1)` produces plausible numbers, not errors. `Column`'s sample form also explains why it **traps at `n = 1`** where `Stats` answers `0.0`: sample variance of one observation is `0/0`, undefined rather than zero.

The real hazard is not the divisor but the near-homograph: `variance` and `var` read as the same word abbreviated. They are not the same function. Code that needs a specific convention should say which — `Stats.variance(col.valid_values())` for the population form of a column, or scale `Stats.variance` by `n/(n−1)` for the sample form of a slice. `gpu.variance` follows `Stats`, since it consumes a dense `Vec`, not a nullable column; there is deliberately no `gpu.var`, which would import the ambiguous short spelling into a third namespace.

#### DataFrame — schema-bearing tables

`DataFrame` is a table of named `Column`s with an associated schema. It carries column-name-to-type mappings at the schema level; individual columns carry their bitmap nulls. Row-oriented iteration produces struct views; column-oriented iteration is the common case for analytical workloads.

#### Memory Layout Commitments (Arrow)

`Tensor`, `Column`, `DataFrame`, and `String` conform to Apache Arrow's memory layout as a **specification-layer commitment** (per the layer model in [design.md §2](design.md#2-specification-layers); changing buffer layout is a breaking change to FFI consumers and memory-mapped data files):

- **Tensor buffers default to C-order (row-major) contiguous layout.** This matches Arrow's convention and the de-facto layout used by cuBLAS, NumPy (default), and most GPU kernels. Operations that produce logically non-contiguous views (transpose, step-slicing) carry Arrow-compatible stride metadata — see [Tensor Strides / Non-Contiguous Views](#tensor-strides--non-contiguous-views) below. FFI callers that require contiguous input use `.compact()` to force a C-order copy.
- **Column validity is bitmap-based**, one bit per element, 0 = null, 1 = valid.
- **Strings are UTF-8.** (Already Kāra's commitment; consistent with Arrow's string layout.)
- **DataFrame schema and column dispatch** follow Arrow's schema model. Dictionary-encoded columns, run-end encoding, and fixed-size-list are natural future additions.

The commitment enables zero-copy interop with pyarrow, Polars, DuckDB, Parquet readers, and the rest of the modern data stack. A Kāra program can hand a `Tensor` pointer to a Python process over Arrow IPC and the data is directly consumable — no serialization, no layout translation.

**GPU compatibility.** Arrow layout maps cleanly to GPU device buffers (cuDF uses Arrow-on-CUDA as its canonical in-memory representation). Dense C-order tensors transfer to GPU memory without reshaping; bitmap validity maps to per-lane masks in compute shaders with small per-kernel overhead. This keeps the future GPU backend (see below) consistent with the CPU stdlib.

<a id="gpu-backend-future-direction-v1-deferred"></a>
#### GPU Backend (future direction)

GPU call-site dispatch (`arr.on(gpu).map(f).collect()`) does not come with the first `kara-data` release. The [GPU](#gpu) track brings GPU as a compile target first; call-site ergonomics are revisited after it, shaped by:

- **Direction:** method-level backend selection — `GpuTensor[T, Shape]` mirrors `Tensor[T, Shape]` with device memory; `.on(gpu)` / `.to_cpu()` move data across the boundary. Matches CuPy / PyTorch / JAX device semantics — zero-surprise for the target audience.
- **Deferred because** committing to a specific `GpuTensor` API before GPU codegen has real ground truth would lock in design decisions (error propagation across the device boundary, stream / queue semantics, memory-pooling story) without the evidence to make them well.
- **Numerical stdlib composes regardless.** Shape types, broadcasting, Arrow layout, bitmap nulls, and trait-dispatched stats / reduce / element-wise ops are all designed so a future `GpuTensor` plugs into the same surface without API break.

Effect-based GPU dispatch (a `runs_on(GPU)` effect routing ops through the effect system) is a research-grade idea revisited later if the effect-dispatch pattern has crystallized from other feature work.

**Relationship to `Shape` / `Dim` kinds.** `Shape` (a type-level dim list, e.g. `[3, 4, ?]`) and `Dim` (an individual tensor dim that may be a concrete int or the runtime-`?` marker) are distinct generic kinds from const generics. `Shape` / `Dim` carry runtime-`?` polymorphism and shape-arithmetic semantics specific to tensors (see [Tensor — shape types](#tensor--shape-types)). Const generics are pure compile-time values: every const-arg is fully evaluated before monomorphization, and the runtime-`?` marker is not legal in const-arg position. An `Array[T, N]`'s `N` is a const generic, not a `Dim`; a `Tensor` shape is not expressible as a tuple of const-`i64` params.

### Tensor Strides / Non-Contiguous Views

**Decision:** Relax the Memory Layout Commitment from "Tensor buffers are contiguous in C-order" to: *C-order contiguous is the default allocation layout; operations that produce logically non-contiguous views (transpose, step-slicing) carry Arrow-compatible stride metadata.* (See the softened wording in [Memory Layout Commitments](#memory-layout-commitments-arrow).)

**Why:** Arrow's tensor specification includes stride fields — a strided `Tensor` is a first-class Arrow concept. Zero-copy transpose (`arr.T`) is the single most important performance property of a numerical array type for ML workloads. Without strides, every transposition copies — a critical regression for matmul-heavy pipelines.

**Why non-breaking (to the spec commitment):** Relaxing a "must be contiguous" guarantee to "contiguous by default; strides allowed" is additive. Existing allocations are unaffected. The breaking-change risk sat in the other direction: locking in "always contiguous" now would have made introducing strides later a spec-layer break.

**Why non-breaking (to FFI callers):** Arrow IPC handles strided tensors natively. For callers that require contiguous input (Parquet, most GPU kernels, raw C array pointers), `.compact() -> Tensor[T, Shape]` forces a C-order copy explicitly — the caller opts in, no silent copying on every FFI call.

**Design shape:**

Every `Tensor[T, Shape]` internally carries optional stride metadata in the Arrow buffer descriptor. The default stride for a rank-N tensor with dims `[d0, d1, ..., dN-1]` is C-order: `strides[i] = prod(d_{i+1}..d_{N-1}) * T.size_of()`.

Operations that return strided views (zero-copy):
- `t.T` — reverse all strides
- `t[start..end..step, ...]` — multiply the relevant stride by `step`
- `t.broadcast_view(new_shape)` — set stride to 0 for any singleton dim being broadcast-expanded

Operations that always return a fresh contiguous allocation:
- Arithmetic operators (`+`, `-`, `*`, etc.) — element-wise results materialize into a new buffer
- `t.compact()` — explicit copy to C-order contiguous; use before FFI hand-off when the consumer requires contiguity

Predicate: `t.is_contiguous() -> bool` — true iff strides match C-order for the current shape.

### Axis-Indexed Reductions

**Sequencing:** comes with [shape-parameter arithmetic](#shape-parameter-arithmetic-a--b-n--2).

**Decision:** `Tensor` ships first with only global reductions (`sum`, `mean`, `min`, `max`, `argmin`, `argmax`) that collapse all dimensions and return a scalar. Axis-indexed reductions come with shape arithmetic.

**Why deferred:** An axis reduction on `Tensor[T, [M, N]]` along axis 0 should return `Tensor[T, [N]]`. Expressing `remove_dim(Shape, axis)` in the type system requires shape arithmetic, which itself requires a type-level const-evaluator. Shipping axis reductions early with `Tensor[T, [?]]` return types and later tightening them would be a breaking return-type change for all callers.

**Why non-breaking:** axis reductions do not ship before shape arithmetic, so there is no API to break; they arrive with correct types.

**Design shape:**

`AXIS` is a `const i64` generic parameter, not a runtime `i64` argument — this lets the compiler compute `remove_dim(Shape, AXIS)` at type-check time:

```kara
let t: Tensor[f64, [3, 4, 5]] = ...;
let s = t.sum[1]();      // Tensor[f64, [3, 5]] — axis 1 collapsed
let m = t.mean[0]();     // Tensor[f64, [4, 5]] — axis 0 collapsed
let mx = t.max[2]();     // Tensor[f64, [3, 4]] — axis 2 collapsed
let am = t.argmin[0]();  // Tensor[i64, [4, 5]]
```

A dynamic-axis variant (`sum_dyn(axis: i64) -> Tensor[T, [?]]`) is not planned. Callers needing runtime-selected axes can match over small axis counts.

### Shape-Parameter Arithmetic (`[A + B]`, `[N * 2]`)

**Decision:** Arithmetic over shape parameters — concat (`[A + B]`), reshape-by-factor (`[N * 2]`), split-along-dim — comes after shape unification. Requires a type-level const-evaluator.

**Why deferred:** Shape unification (same parameter appearing in multiple positions) ships with `Tensor` and covers the common tensor-ops cases. Arithmetic requires const-evaluation infrastructure that is better designed once comptime lands. Until then, the affected operations (concat, reshape-by-factor, split-along-dim) return partially-dynamic shapes.

**Why non-breaking:** Purely additive. Existing shape parameters remain unchanged; arithmetic extends the grammar in type-parameter position.

**Design shape:** See [Tensor — shape types](#tensor--shape-types).

<a id="f16--bf16-implementation"></a>
### Reduced-Precision Floats (`f16`, `bf16`)

**Decision:** `f16` (IEEE 754-2008 half-precision) and `bf16` (bfloat16) are first-class numeric primitives for mixed-precision ML workloads. They are primitives, not library types, and follow the same rules as `f32`/`f64`. They come with this track. Until then `f16` and `bf16` are ordinary identifiers, not reserved words, and the literal suffixes `1.0f16` and `1.0bf16` are not valid.

- **`f16`** — IEEE 754-2008 half-precision (1 sign, 5 exponent, 10 mantissa bits). Range: ±65504. Native on ARM (FP16 extension), x86 (AVX-512FP16), and all modern ML accelerators.
- **`bf16`** — bfloat16 (1 sign, 8 exponent, 7 mantissa bits). Same exponent range as `f32` — tolerant of gradient underflow, easy conversion. Native on Google TPU, NVIDIA A100+, Intel AMX, Apple Silicon (ANE).

**Traits.** Same trait surface as `f32`/`f64`: `Copy`, `PartialEq`, `PartialOrd`, `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`. Not `Eq`, `Ord` or `Hash` (NaN semantics apply). The total-order wrappers `F16` and `Bf16` follow the `F32`/`F64` pattern for contexts that require `Eq`, `Ord` or `Hash`. The bfloat16 wrapper is spelled `Bf16`, not `BF16`: a name whose letters are all uppercase is a constant under the naming rules ([design.md §3](design.md#identifiers-and-naming)). `F16` takes the single-letter type escape hatch, like `F32` and `F64`.

**Generic code.** A bound such as `fn g[T: Add](a: T, b: T) -> T` instantiates at both widths. Each width gets its own monomorphized body, and the interpreter resolves the type parameter to the width before rounding, so a generic computes at the precision it is instantiated at.

**Literals and widening.** Literal suffixes are `1.0f16` and `1.0bf16`. Widening is implicit where it is lossless, as for the other floats ([design.md §5](design.md#numeric-semantics)):

| Conversion | Implicit? | Reason |
|---|---|---|
| `f16` → `f32` → `f64` | Yes | `f16` is a strict subset of `f32`; widening is always lossless |
| `bf16` → `f32` | Yes | `bf16` has the same exponent range as `f32`; widening is lossless |

Narrowing needs `as`. Mixed-precision is the standard ML training pattern: store weights in `f16`/`bf16`, compute in `f32`, store results back. The `as` cast is the explicit mechanism; there is no implicit narrowing:

```kara
let w: Tensor[bf16, [768, 768]] = load_weights();
let x: Tensor[bf16, [batch, 768]] = input;

// Upcast to f32 for stable computation
let y = (x as Tensor[f32, _]).matmul(w as Tensor[f32, _]);

// Store back as bf16
let out: Tensor[bf16, [batch, 768]] = y as Tensor[bf16, _];
```

`Tensor[f16, Shape]` and `Tensor[bf16, Shape]` are valid once the types and `Tensor` ship.

**Codegen.**

- `f16` lowers to LLVM `half`; `bf16` lowers to LLVM `bfloat`.
- On targets with native hardware support (ARM FP16, AVX-512FP16, NVIDIA Tensor Cores, TPU), LLVM emits native instructions.
- On targets without native support, each operation is widened to `f32` and the result is rounded back after every operation. The compiler emits a lint, suppressible with `#[allow(f16_software_emulated)]`, so the performance cost is never silent.
- **The widening is codegen's job, not the backend's.** Some LLVM targets cannot select scalar `bfloat` operations at all: on AArch64 and wasm32, `fadd`, `fsub`, `fmul`, `fdiv`, `fneg`, `fcmp` and every conversion abort the compiler. So `karac` widens every `bf16` arithmetic, comparison, negation and conversion to `f32` itself, on every target, and rejects a module that still contains a native `bfloat` operation.
- On wasm32, LLVM keeps `half` values in an `f32` carrier and rounds only where the narrow type is forced (a store, a comparison, a call). Values that no `f16` can represent then survive between operations: `65504f16 * 3f16` must be `inf`, not `196512`. So on wasm codegen widens and rounds `f16` itself, and a check after optimization rejects native `half` arithmetic.
- `f16` is also widened wherever an inexact constant is built. A constant such as 180/π built at `f16` is already wrong before the multiply, and no intermediate width recovers it.
- The general rule: a reduced-precision format's semantics are codegen's responsibility on every target. `Vector[bf16, N]` lane operations are not yet widened.

### `std.einsum` — Einstein Summation

**Decision:** Ship `einsum` as a string-notation function once the core numerical library (`Tensor`, shape types, `std.linalg`) is stable.

**Why deferred:** Pure stdlib addition — no language changes required. Holding it lets the broader numerical surface stabilize so `einsum` fits cleanly alongside matmul, reduce, and broadcast methods.

**Why non-breaking:** New stdlib function. No existing API affected.

**Design shape:**

```kara
import std.einsum.einsum;

let c   = einsum("ij,jk->ik", a, b);        // matmul
let tr  = einsum("ii->", a);                // trace
let out = einsum("i,j->ij", u, v);          // outer product
let bat = einsum("bij,bjk->bik", a, b);     // batched matmul

// Return type: Tensor[T, [?]] — shape derived at runtime from the einsum string.
// Typed einsum with compile-time shape checking is a separate, later entry.
```

The string parser validates index consistency (each index appears at most twice per operand on the left, exactly once in the output) at runtime and returns an error on malformed strings.

### `std.embeddings` — Cosine Similarity and Top-K Primitives

**Decision:** Ship `std.embeddings` with this track. Minimum surface: cosine similarity (scalar + batched single-query + Q×N matrix), L2 normalize (in-place + non-mutating), batched dot product, top-k indices+scores. Six functions over existing `Tensor[f32, ...]` primitives.

**Why ship it.** RAG, semantic search, and recommendation workloads are mainstream backend patterns. Without `std.embeddings`, every adopter doing AI-adjacent work hand-rolls the same `cosine_similarity` against `Tensor` primitives — wasteful for a 6-function surface. Vector indices (HNSW, IVF, scalar quantization) stay community territory.

**Why non-breaking:** New stdlib module.

**Design shape:**

```kara
import std.embeddings;

let sim: f32 = embeddings.cosine_similarity(query, target);                    // Tensor[f32, [D]] × Tensor[f32, [D]]
let sims: Tensor[f32, [N]] = embeddings.cosine_similarity_batched(query, corpus);     // [D] × [N, D]  — SGEMV (BLAS-2)
let mat:  Tensor[f32, [Q, N]] = embeddings.cosine_similarity_matrix(queries, corpus); // [Q, D] × [N, D]  — SGEMM (BLAS-3)
let normed: Tensor[f32, S] = embeddings.l2_normalize_to(t);
embeddings.l2_normalize(mut t);                                                  // in-place
let dots: Tensor[f32, [N, M]] = embeddings.dot_batched(a, b);                  // [N, D] × [M, D]
let top: Tensor[(i64, f32), [k]] = embeddings.top_k(scores, k: 10);            // indices + scores
```

`cosine_similarity_matrix` is the Q×N production-RAG shape: Q queries against an N-vector corpus produces a Q×N similarity matrix. SGEMM-shaped (BLAS-3) — this is where the compute-bound speedup lives. `cosine_similarity_batched` (single query × N corpus) remains the SGEMV-shaped convenience for the common single-query path. Adding `_matrix` rather than overloading `_batched` on the query rank is deliberate: Kāra prefers explicit-over-magic in API surface.

**Cross-reference:** [Hand-Vectorized Data-Spine Commitment](#hand-vectorized-data-spine-commitment), the spine this surface relies on.

### `std.linalg` — Linear Algebra Suite

**Decision:** `std.linalg` minimum surface: SVD, eigendecomposition, QR factorization, Cholesky, least-squares (`lstsq`), matrix norm, inverse, determinant, and rank. Dispatch through LAPACK (linked at build time) or a pure-Kāra Cooley-Tukey fallback.

**Why deferred:** Requires a stable `Tensor` and LAPACK linkage. No language decisions are blocking.

**Why non-breaking:** New stdlib module.

**Design shape:**

```kara
import std.linalg;

let (u, s, vt) = linalg.svd(a);
let (vals, vecs) = linalg.eig(a);           // square matrix only
let (q, r) = linalg.qr(a);
let l = linalg.cholesky(a);                 // positive-definite — panics otherwise
let x = linalg.lstsq(a, b);
let n = linalg.norm(a, ord: linalg.Norm.Fro);  // Norm.L1, Norm.L2, Norm.Inf also available
let inv = linalg.inv(a);
let d   = linalg.det(a);
let r   = linalg.matrix_rank(a);
```

All functions require `T: Float` (`f32` or `f64`). Output shapes follow standard linear algebra conventions and return partially-dynamic shapes until shape arithmetic allows full static expression.

### `std.fft` — Fourier Transforms

**Decision:** `std.fft` minimum surface: 1D FFT/IFFT, N-D FFT, real FFT (`rfft`), and frequency helper (`fftfreq`). Dispatch through FFTW (linked at build time) or a pure Cooley-Tukey fallback.

**Why deferred:** Pure library work, no language decisions blocking. Requires FFTW linkage.

**Why non-breaking:** New stdlib module.

**Design shape:**

```kara
import std.fft;

let spectrum  = fft.fft(signal);                        // Tensor[Complex[f64], [N]]
let recovered = fft.ifft(spectrum);
let rspec     = fft.rfft(signal);                       // Tensor[Complex[f64], [N/2 + 1]]
let freqs     = fft.fftfreq(n: 1024, d: 1.0 / rate);   // Tensor[f64, [1024]]
let spec2d    = fft.fftn(image);                        // Tensor[Complex[f64], [H, W]]
```

`Complex[T]` is a stdlib struct with `real` and `imag` fields and the standard arithmetic traits. It is not a new numeric primitive.

### `std.random` — Distribution Extensions

**Decision:** Statistical distribution sampling beyond basic uniform random comes with this track. Minimum surface: normal (Gaussian), uniform (continuous), binomial, Poisson, and exponential.

**Why deferred:** Basic uniform sampling is in v1 ([library/time-random-env.md](library/time-random-env.md)). Distribution extensions are a follow-on slice with no language dependencies.

**Why non-breaking:** Additive to the existing `std.random` module.

**Design shape:**

```kara
import std.random.{Rng, distributions};

let mut rng = Rng.from_seed(42);

let x = rng.sample(distributions.Normal(mean: 0.0, std: 1.0));
let y = rng.sample(distributions.Uniform(lo: 0.0, hi: 1.0));
let n = rng.sample(distributions.Binomial(n: 10, p: 0.3));     // u64
let k = rng.sample(distributions.Poisson(lambda: 2.5));        // u64
let e = rng.sample(distributions.Exponential(rate: 1.5));

let arr: Tensor[f64, [100, 100]] = Tensor.from_fn(|_, _| rng.sample(distributions.Normal(0.0, 1.0)));
```

### `std.autograd` — Automatic Differentiation (reverse-mode)

**Decision:** Ship `std.autograd` with this track. **Reverse-mode only in the first release.** Wrapper type: separate `Var[T, S]` (not `Tensor.requires_grad: bool`); design rationale below.

**Why ship it.** Autograd is the dividing line between "Kāra has tensors" (commodity) and "Kāra can train models" (a category most general-purpose languages don't occupy at launch). Combined with GPU codegen, this puts Kāra in a credible position for ML-curious adopters without leading the pitch with ML.

**Why non-breaking:** New stdlib module.

**`Var[T, S]` over `requires_grad: bool` — locked design choice.** Kāra's type system (shape types + effect types + ownership) is the differentiator; autograd leverages it rather than bypassing it with runtime flags. Only `Var` operators carry `writes(GradTape)` — coarse `writes(GradTape)` on every Tensor op (the PyTorch shape forced by Python's type system) is avoided. PyTorch chose `requires_grad: bool` because Python couldn't express the alternative; Kāra doesn't inherit that workaround.

**Minimum viable surface:**
- `shared struct Tape` — single-use, append-only operation log. Effect: `writes(GradTape)`.
- `Var[T, S]` wrapper over `Tensor[T, S]`. Conversions: `Var.track(tensor)` / `var.detach() -> Tensor`.
- Operator overloads on `Var` for `+`, `-`, `*`, `/`, matmul, broadcasting, reductions (`sum`, `mean`), reshape, transpose, indexing.
- Activations with hand-coded backwards: `relu`, `sigmoid`, `tanh`, `softmax`, `gelu`, `silu`.
- Losses with backwards: `mse_loss`, `cross_entropy`, `binary_cross_entropy`.
- `grad(fn, args) -> Args.Grads` and `value_and_grad(fn, args) -> (Output, Args.Grads)`.
- GPU-aware: autograd ops on GPU `Var` record on the same tape; backward pass dispatches kernel launches via GPU codegen.

**Out of the first release's scope:**
- Forward-mode AD.
- Higher-order gradients (`grad(grad(f))`).
- Custom backward definitions (`@custom_vjp` decorator equivalent). Stdlib-blessed ops only.
- Checkpointing / activation rematerialization.
- JIT-traced graphs (eager only).
- Distributed AD / multi-GPU gradient sync.

**Effect-system advantage.** Public functions performing gradient-tracked operations declare `with writes(GradTape)`. Inference and preprocessing functions carry no `GradTape` effect — the compiler statically enforces the separation. Accidentally calling a tracked op inside an inference-only function is a compile error, not a silent correctness bug. This is the load-bearing reason for `Var[T, S]` over `requires_grad: bool` — bool-flagged tensors over-approximate `writes(GradTape)` to all Tensor ops, useless for inference.

**Open at engineering start:** `Var`↔`Tensor` conversion ergonomics, `Differentiable` trait shape for operator overloading once-on-trait vs twice-on-types, exact `grad`/`value_and_grad` signature with shape preservation.

<a id="lazy-dataframe-query-planner--option-a-v1-scope"></a>
### Lazy DataFrame Query Planner

**Decision:** Ship `LazyDataFrame` with this track, with a minimum-viable optimizer (**Option A**): predicate pushdown, projection pushdown, constant folding, common-subexpression elimination. Target ~2-3K LOC, ~6-8 weeks focused engineering. Written fresh.

**Why ship it.** Eager DataFrame ops are fine for small data, but the analytical workload that makes Polars beat pandas (and makes DuckDB feel cheap) is the lazy planner. Without it, "Kāra has DataFrame" reads as "Kāra has a slow pandas." With Option A, "Kāra has DataFrame" reads as "Kāra has a moderately-capable analytical engine; reach for DuckDB on multi-join warehouse queries."

**Why Option A and not the full expansion.** A 5-7K LOC fresh optimizer is the right *target* but the wrong first *commitment*: it's exactly the kind of scope that slips by months and pulls the release with it. Option A's gap vs Polars is in complex multi-join analytics; users already reach for DuckDB there. Honest docs framing: "Polars-comparable on simple-to-moderate queries, weaker on complex multi-join analytics — reach for DuckDB for warehouse queries." See [Lazy DataFrame Query Optimizer Expansion](#lazy-dataframe-query-optimizer-expansion) for the later path.

**Why non-breaking:** Additive — `df.lazy()` returns a new `LazyDataFrame`; existing eager `DataFrame` API unchanged.

**Design shape:**

```kara
let lazy = df.lazy();                                            // -> LazyDataFrame
let result = lazy
    .filter(col("age") > 21)
    .select([col("name"), col("city")])
    .group_by([col("city")])
    .agg([col("name").count().alias("cnt")])
    .sort([col("cnt")])
    .collect();                                                  // -> DataFrame
let plan: String = lazy.explain();                               // optimized plan as text
```

Optimizer passes in the first release: predicate pushdown (move filters before scans/joins), projection pushdown (only read columns that contribute), constant folding (evaluate constants at plan time), CSE (deduplicate identical sub-expressions). Later: join reordering, filter combining, push-aggregations-through-joins, scan-time filters, projection-aware Parquet reads (see the expansion entry).

### Statistical Methods on `Column` / `DataFrame`

**Decision:** Ship statistical methods on `Column` and `DataFrame` with this track, trait-dispatched the same way as `std.stats` so future `GpuColumn` / `GpuTensor` implements the same surface.

**Why ship it.** General-purpose data work routinely calls `.mean()`, `.std()`, `.median()`, `.quantile()`, `.describe()`. Each individual method is trivial; the absence of them as canonical stdlib surface is the kind of "Kāra doesn't have basic stats?" objection that's cheap to prevent.

**Why non-breaking:** Additive method surface on existing `Column[T]` and `DataFrame` types.

**Design shape:**

```kara
let col: Column[f64] = df.column("score");
let mean: f64 = col.mean();
let std: f64 = col.std();
let med: f64 = col.median();
let p99: f64 = col.quantile(0.99);
let corr_xy: f64 = df.column("x").corr(df.column("y"));

let summary: DataFrame = df.describe();   // count / mean / std / min / 25% / 50% / 75% / max per numeric column
```

Surface: on `Column[T: Numeric]` — `mean`, `std`, `var`, `median`, `quantile(q)`, `min`, `max`, `sum`. On `Column[f64]` additionally: `corr(other)`. On `DataFrame`: `describe()`.

NaN handling delegates to the existing `std.stats` discipline (NaN-propagating vs NaN-skipping variants); see [NaN and Inf Handling](#nan-and-inf-handling).

### Data Documentation and Examples (Discoverability Surface)

**Decision:** Ship a dedicated data chapter and worked examples with this track. Data is not the headline pitch, which makes discoverability a real concern: depth that doesn't surface in the docs is depth users will not find.

**Why ship it.** Without this, the data library's breadth (Tensor, Column, DataFrame, Arrow IPC, `std.linalg`, `std.fft`, `std.einsum`, `std.embeddings`, `std.autograd`, lazy DataFrame planner) is reachable only by reading the API reference. The chapter and examples make it discoverable from the book's table of contents and `examples/` directory.

**Why non-breaking:** Doc-only.

**Surface:**

- **`docs/book/src/data.md`** — single book chapter. Covers Tensor (rank, shape types, indexing, broadcasting, common ops), Column (nullable 1D, null semantics, NaN handling, Arrow layout), DataFrame (schema, read_csv / read_parquet, lazy querying, group-by, joins). One end-to-end example (~50 lines): load CSV → filter → group by → compute → write Parquet. Pointers to `std.linalg`, `std.fft`, `std.einsum`, `std.embeddings`, `std.random.distributions`, `std.autograd` with one-line each.
- **`examples/data/`** — 3-4 programs of 30-80 lines each: `csv-to-parquet.kara` (basic ETL), `embeddings-rag.kara` (load corpus → embed via external HTTP embedder → top-k semantic search), `stats-summary.kara` (group-by + describe over a CSV), `lazy-query.kara` (Polars-class analytical query against Parquet via the lazy planner). Doubles as integration tests against the data stdlib.

Not a promotional document — a structural reference. The pitch still reads "general-purpose AOT systems language"; the chapter exists so users who arrive and discover the data depth can navigate it.

### `Tensor.where` — Conditional Element Selection

**Decision:** Element-wise conditional selection is a library function.

**Why deferred:** Pure stdlib addition — depends only on boolean tensor support being in place (ships with the `Tensor` type itself via element-wise comparison operators).

**Why non-breaking:** New stdlib function.

**Design shape:**

```kara
// Free function: Tensor.where(condition, if_true, if_false)
let result  = Tensor.where(mask, x, y);      // shapes of mask/x/y must match exactly
let clipped = Tensor.where(arr > 0.0, arr, 0.0);  // scalar broadcasts as with other operators

// Method alias
let result = mask.select(x, y);
```

Shapes must match exactly — no implicit tensor-tensor broadcasting (consistent with Kāra's broadcasting design). Scalar arguments broadcast as with other scalar-tensor operators.

### Boolean and Fancy Indexing

**Decision:** Boolean mask indexing and index-array indexing come after scalar indexing. Result shape is always partially dynamic — boolean mask result count is data-dependent; index-array result shape depends on the index array's shape.

**Why deferred:** The first `Tensor` handles scalar-index access (`t[i, j, k]`) only. Boolean and fancy indexing require additional `Index` trait impls, a pure library extension on top of `Tensor`.

**Why non-breaking:** New `Index` trait impls for new argument types. Existing `t[i, j, k]` form is unaffected.

**Design shape:**

```kara
let arr: Tensor[f64, [10, 5]] = ...;

// Boolean indexing — result row count = number of true entries in mask
let mask: Tensor[bool, [10]] = arr[:, 0] > 0.0;
let filtered = arr[mask];           // Tensor[f64, [?, 5]]

// Fancy indexing — index with an array of integer indices
let idx: Tensor[i64, [3]] = Tensor.from([1, 4, 7]);
let rows = arr[idx];                // Tensor[f64, [3, 5]]
```

Both forms return owned tensors (not views) — the gathered elements may be non-contiguous in the source buffer and must be materialized into a fresh allocation.

### `Tensor.meshgrid` — Coordinate Grid Generation

**Decision:** `meshgrid` is a library convenience.

**Why deferred:** Pure stdlib — no language changes. Low priority relative to `std.linalg`, `std.fft`, and `std.einsum`.

**Why non-breaking:** New stdlib function.

**Design shape:**

```kara
import std.tensor.meshgrid;

let x = Tensor.from([0.0, 1.0, 2.0]);   // Tensor[f64, [3]]
let y = Tensor.from([0.0, 1.0]);        // Tensor[f64, [2]]

let (xx, yy) = meshgrid(x, y);
// xx: Tensor[f64, [2, 3]] — x values broadcast over rows
// yy: Tensor[f64, [2, 3]] — y values broadcast over columns
```

Returns broadcast-expanded (strided) views by default. `.compact()` materializes into contiguous memory when needed.

### Tensor Element-Wise Math and Clamp

**Decision:** The full suite of element-wise unary math functions, and the `clip` clamp utility, are `Tensor` methods and free functions in `std.math`.

**Why deferred:** These require `Tensor`. No language decisions are blocking.

**Performance contract.** Element-wise rows on the numerical surface (autograd activations, statistical reductions, Tensor arithmetic) are covered by the hand-vectorized spine; see [Hand-Vectorized Data-Spine Commitment](#hand-vectorized-data-spine-commitment). Transcendentals (`exp`, `log`, `sqrt`, `sin`, `cos`, `tanh`, etc.) get their per-element vectorization via the new `std.simd.math` sub-surface (Sleef-class polynomial approximations) rather than auto-vec — LLVM auto-vec does *not* substitute vectorized exp for scalar `expf`, so transcendentals are a separate implementation surface. Rounding (`floor`, `ceil`, `round`, `abs`, `sign`) and `clip` family are auto-vec-friendly under bounds-check elision; they trust LLVM rather than ship hand-written kernels.

**Why non-breaking:** New methods/functions. No existing API affected.

**Design shape:**

Transcendental and rounding functions dispatch through `std.math`; transcendentals route through `std.simd.math` per the spine commitment, rounding/clip vectorize via LLVM:

```kara
// Element-wise — return Tensor of same shape
arr.exp()       // e^x per element
arr.log()       // natural log; log2(), log10() also available
arr.sqrt()
arr.abs()
arr.sign()      // -1.0, 0.0, or 1.0
arr.floor()
arr.ceil()
arr.round()
arr.sin()  arr.cos()  arr.tan()
arr.sinh() arr.cosh() arr.tanh()
arr.asin() arr.acos() arr.atan()
Tensor.atan2(y, x)   // element-wise two-argument arctangent

// Clamp — the most common value-bounding operation
arr.clip(lo: 0.0, hi: 1.0)          // element-wise clamp; lo/hi are scalars
arr.clip_lo(0.0)                     // lower bound only (ReLU idiom)
arr.clip_hi(1.0)                     // upper bound only
```

All functions require `T: Float`. The `clip` family operates analogously to scalar broadcasting: `lo` and `hi` are `T`, not `Tensor[T, Shape]`.

### Hand-Vectorized Data-Spine Commitment

**Decision:** Ship a designated set of data kernels as **hand-written `Vector[T, N]` implementations** wherever measurement shows LLVM auto-vectorization falls short. The kernel families cover embeddings, autograd activations, Tensor element-wise arithmetic and statistical reductions.

**Why.** Each of these surfaces needs a documented performance floor; hand-vectorizing the spine turns "hopefully fast" into "measurably fast." This is a library-internals and performance decision. The user-facing API is unchanged.

**Kernel list (a ceiling, narrowed by measurement).**

| Kernel family | BLAS class | Bound by | Speedup target vs scalar |
|---|---|---|---|
| `embeddings.cosine_similarity` (single, `[D] × [D]`) | BLAS-1 | Memory | 2–4× |
| `embeddings.cosine_similarity_batched` (single-query, `[D] × [N, D]`) | BLAS-2 | Memory | 2–4× |
| `embeddings.cosine_similarity_matrix` (Q×N, `[Q, D] × [N, D]`) | BLAS-3 | Compute | NumPy parity (5–10× over scalar) |
| `embeddings.dot_batched` (`[N, D] × [M, D]`) | BLAS-3 | Compute | NumPy parity (5–10× over scalar) |
| `embeddings.l2_normalize` (in-place + non-mutating) | BLAS-1 | Memory | 2–4× |
| `embeddings.top_k` | BLAS-1 + reduction | Memory | 2–3× |
| Tensor element-wise `+`, `-`, `*`, `/` | BLAS-1 | Memory | 2–4× |
| Tensor reductions: `sum`, `mean`, `min`, `max` | BLAS-1 + reduction | Memory | 2–4× |
| Activations: `relu`, `sigmoid`, `tanh` | BLAS-1 | Memory (`relu`) / Mixed (`sigmoid`, `tanh`) | 2–3× / 4–8× |
| `softmax` | BLAS-1 + reduction + transcendental | Mixed | 4–6× |
| `exp`, `log`, `sqrt` element-wise (via `std.simd.math`) | BLAS-1 (transcendental) | Compute (per element) | 4–8× (~2× of NumPy contingent on `std.simd.math` quality) |

**`std.simd.math` sub-surface.** New stdlib surface for SIMD-friendly polynomial approximations of transcendentals: `Vector[f32, N].exp()`, `.log()`, `.sqrt()`, `.tanh()`, `.sigmoid()`. Sleef-class quality for f32; f64 follows the same pattern. Required because LLVM auto-vec does not substitute vectorized exp for scalar `expf` — that is a known auto-vec dead end. Without `std.simd.math`, the transcendental rows in the spine degrade to "auto-vec maybe, scalar usually" and the 4–8× target vanishes.

**Per-kernel perf targets.**
- BLAS-3 rows target **NumPy parity** (NumPy itself dispatches to OpenBLAS / MKL; matching is the goal, not beating).
- BLAS-1 memory-bound rows target **NumPy ±20%** (memory bandwidth is the ceiling; both implementations approach it).
- Transcendental rows target **~2× of NumPy** (NumPy's `exp` calls into libm which is already vectorized on most platforms; closing this gap is `std.simd.math`-quality-dependent).

**Bit-exactness scope.** Treat user-observable bit-exactness as a guarantee **for a given execution path** — same target, same compile flags, same hardware feature level. Cross-path bit-exactness (SIMD vs scalar fallback; AVX-2 baseline vs an AVX-512 multiversioned variant) is *not* promised, because the reduction order differs by construction. Polars and NumPy make the same scoped commitment.

**Narrowing is per kernel, by measurement.** Each row is kept or dropped by an A/B measurement against the shipping implementation. A row where auto-vectorization or a fused iterator path already matches hand-written SIMD drops out, with its benchmark number documented. Measured on x86-64 (AVX2), the answer differs by kernel, not by BLAS class:

- `cosine_similarity_batched`: hand-vectorization wins, about 3.6×. Dropping a row copy gives about 2.2×, and 8-lane arithmetic about 1.7× on top.
- `dot_batched`: hand-vectorization loses, about 3.4× slower, because the shipping version already takes the fused row-view reduction and never copies a row.
- Single-vector BLAS-1 rows (`cosine_similarity`, `l2_norm`, `l2_normalize`) drop out: the fused path beats a hand-written 8-lane loop by about 3×.
- An indexed scalar loop gets no auto-vectorization at all: float `+` is not associative, and nothing licenses reassociation.

Timings on one host drift with page-cache and memory state, so only interleaved A/B runs (alternating binaries in one loop) are trustworthy.

**Prerequisite: a borrowing row view.** `iter_axis` materializes rows as copies, and that allocation is most of the gap for the batched kernels that miss the fused path. A borrowing row view would deliver most of the available win without hand-writing any kernel, and would change which rows remain worth hand-vectorizing. It should land before more of this list is attempted. A bulk-load primitive (`chunks_simd`) is ergonomics, not a blocker: eight indexed reads into a `Vector[f32, 8]` already fold to one vector load.

**Why non-breaking:** Implementation strategy — no API change. The same scalar-equivalent semantics are observable; only the perf curve changes.

**Cross-reference:** [Portable SIMD](#portable-simd--vectort-n) (the type the kernels build on); [Multiversioning](#multiversioning-cpu-baseline-and-multiversion) (`cpu-baseline` and `#[multiversion]` for AVX-512 / SVE2 variant kernels); [Tensor Element-Wise Math and Clamp](#tensor-element-wise-math-and-clamp) (the entry whose perf contract this binds).

### Tensor Construction Functions

**Decision:** Explicit construction helpers ship with `Tensor`. They are required for almost every numerical program, and the exact API is pinned here to avoid ad-hoc decisions during implementation.

**Why deferred:** Pure library work, no language decisions blocking. Pinning the API shape now ensures the interpreter and codegen don't grow incompatible ad-hoc constructors.

**Why non-breaking:** New functions on `Tensor`.

**Design shape:**

```kara
// Filled
Tensor.zeros[T: Numeric](shape: Shape) -> Tensor[T, Shape]
Tensor.ones[T: Numeric](shape: Shape) -> Tensor[T, Shape]
Tensor.full[T](shape: Shape, value: T) -> Tensor[T, Shape]

// Range — 1D only
Tensor.arange(stop: f64) -> Tensor[f64, [?]]
Tensor.arange(start: f64, stop: f64; step: f64 = 1.0) -> Tensor[f64, [?]]
Tensor.linspace(start: f64, stop: f64, n: i64) -> Tensor[f64, [?]]

// Identity / diagonal
Tensor.eye[T: Numeric](n: i64) -> Tensor[T, [?, ?]]        // n×n identity
Tensor.diag(v: Tensor[T, [?]]) -> Tensor[T, [?, ?]]        // 1D → diagonal matrix
Tensor.diag(m: Tensor[T, [?, ?]]) -> Tensor[T, [?]]        // matrix → main diagonal

// Element-wise construction
Tensor.from_fn[T](shape: Shape, f: Fn(i64...) -> T) -> Tensor[T, Shape]

// From nested Vec / array literals
Tensor.from[T](data: Vec[T]) -> Tensor[T, [?]]             // 1D
Tensor.from_nested[T](data: Vec[Vec[T]]) -> Tensor[T, [?, ?]]
```

Static-shape overloads (where `Shape` is fully static) are resolved at compile time; dynamic overloads return `Tensor[T, [?...]]`.

### Scan Operations (`cumsum`, `cumprod`)

**Decision:** Prefix-scan operations ship with `Tensor`.

**Why deferred:** Pure library work. Unlike axis reductions, the output has the same shape as the input (no dimension removal), so axis-indexed scans have fully static output types without shape arithmetic.

**Why non-breaking:** New methods on `Tensor`.

**Design shape:**

Unlike axis reductions, scans preserve the input shape, so no shape arithmetic is required:

```kara
let t: Tensor[f64, [3, 4]] = ...;

// Global (flatten then scan)
t.cumsum() -> Tensor[f64, [12]]
t.cumprod() -> Tensor[f64, [12]]

// Axis-indexed — output shape identical to input (no remove_dim needed)
t.cumsum[1]() -> Tensor[f64, [3, 4]]   // running sum along columns
t.cumprod[0]() -> Tensor[f64, [3, 4]]  // running product along rows
```

Axis-indexed scans can therefore ship with `Tensor`, unlike axis-indexed reductions, which need shape arithmetic to express the reduced dimension.

### Shape-Manipulating Operations (`concat`, `stack`, `reshape`, `squeeze`, `expand_dims`)

**Decision:** Ship with `Tensor`, with partially-dynamic output shapes. Shape arithmetic will provide fully-typed versions where the output shape is statically known.

**Why deferred:** Depends on `Tensor`. Output shapes require shape arithmetic for full static typing; partially-dynamic shapes are acceptable at first.

**Why not held for shape arithmetic (unlike axis reductions):** These are too fundamental to hold — without them, users cannot assemble tensors from parts or change layout. The dynamic return shapes are safe to ship; callers that need the precise output shape can call `.shape()` at runtime or wait for shape arithmetic.

**Why non-breaking:** The output type changes from `Tensor[T, [?...]]` to a more specific static shape when shape arithmetic lands, which is additive — code accepting the dynamic type continues to work with the more specific type.

**Design shape:**

```kara
// Concatenate along an existing axis
Tensor.concat(tensors: Slice[Tensor[T, [?, ...]]], axis: i64) -> Tensor[T, [?, ...]]
// with shape arithmetic: concat[const AXIS: i64] -> Tensor[T, concat_dim(S, AXIS)]

// Stack along a new axis (tensors must have identical shape)
Tensor.stack(tensors: Slice[Tensor[T, S]], axis: i64) -> Tensor[T, [?, ...]]
// with shape arithmetic: stack[const AXIS: i64] -> Tensor[T, insert_dim(S, AXIS, N)]

// Reshape — total element count must match; panics at runtime if not
t.reshape(new_shape: Slice[i64]) -> Tensor[T, [?, ...]]
// with shape arithmetic: reshape[...NewS](t: Tensor[T, S]) -> Tensor[T, NewS] where prod(S) == prod(NewS)

t.flatten() -> Tensor[T, [?]]        // reshape to 1D

// Add / remove size-1 dimensions
t.expand_dims(axis: i64) -> Tensor[T, [?, ...]]
t.squeeze(axis: i64) -> Tensor[T, [?, ...]]     // panics if dim != 1
t.squeeze_all() -> Tensor[T, [?, ...]]          // removes all size-1 dims
```

### Set-Like Operations (`unique`, `searchsorted`)

**Decision:** Library functions on 1D tensors.

**Why deferred:** Pure library work. `unique` output length is data-dependent (always `[?]`); `searchsorted` output shape matches the index array shape.

**Why non-breaking:** New stdlib functions.

**Design shape:**

```kara
// unique — deduplicated sorted values
let (vals, counts, inverse) = t.unique();
// vals:    Tensor[T, [?]] — sorted unique values
// counts:  Tensor[i64, [?]] — frequency of each unique value (optional)
// inverse: Tensor[i64, [?]] — index into vals that reconstructs t

// searchsorted — binary search in a sorted array
let idx = sorted.searchsorted(values, side: SearchSide.Left);
// idx: Tensor[i64, same shape as values]
// SearchSide.Left: first valid insertion point; SearchSide.Right: last
```

Both require `T: Ord`. `unique` always returns owned tensors.

### NaN and Inf Handling

**Decision:** NaN/Inf predicates and NaN-ignoring reductions ship with `Tensor`.

**Why deferred:** Requires `Tensor`. NaN handling is a floating-point concern; the predicates are element-wise (no shape change) and the NaN-ignoring reductions follow the same shape rules as their non-NaN counterparts.

**Why non-breaking:** New methods and functions. No existing API affected.

**Design shape:**

```kara
// Predicates — element-wise, same shape as input
arr.is_nan()    -> Tensor[bool, S]
arr.is_inf()    -> Tensor[bool, S]
arr.is_finite() -> Tensor[bool, S]

// NaN-ignoring global reductions (treat NaN as absent, not as error)
arr.nansum()    -> T
arr.nanmean()   -> T
arr.nanmin()    -> T
arr.nanmax()    -> T
arr.nan_count() -> i64      // number of NaN elements

// Replace NaN with a fill value
arr.fill_nan(value: T) -> Tensor[T, S]
```

Axis-indexed NaN-ignoring reductions (`nansum[AXIS]()`) wait for shape arithmetic, as axis reductions do: they need it for the return type.

**Floating-point special values.** `f32` and `f64` gain associated constants:

```kara
f64.NAN      // Not-a-Number
f64.INF      // positive infinity
f64.NEG_INF  // negative infinity
```

These are value-level constants, not types. `Column[T]` uses bitmap nullability for missing data (distinct from NaN). Using NaN as a missing-value sentinel in a `Tensor` is discouraged — use `Column[T]` if nullability is semantic.

### `.npy` / `.npz` Array File I/O

**Decision:** NumPy array file format support, as `std.io.npy`.

**Why deferred:** The ML ecosystem uses `.npy`/`.npz` ubiquitously for saving and loading tensors (model weights, datasets, intermediate results). Arrow covers the data-engineering stack (Parquet, IPC), but the ML checkpoint workflow runs on `.npy`. Without this, users who load a pre-trained weight file must shell out to Python. No language changes required.

**Why non-breaking:** New stdlib module.

**Design shape:**

```kara
import std.io.npy;

// Single-array .npy
let arr: Tensor[f64, [?, ?]] = npy.load("weights.npy")?;   // shape inferred at runtime
npy.save("output.npy", arr)?;

// Multi-array .npz archive
let archive = npy.load_npz("checkpoint.npz")?;
let w1 = archive.get[f64]("layer1.weight")?;   // Tensor[f64, [?...]]
let b1 = archive.get[f64]("layer1.bias")?;

let mut builder = npy.NpzBuilder.new();
builder.insert("weights", w1);
builder.insert("bias", b1);
builder.save("checkpoint.npz")?;
```

Supported dtypes: `f32`, `f64`, `i32`, `i64`, `u8`, `bool`. Complex dtypes (`complex64`, `complex128`) are supported once the `Complex[T]` stdlib type is defined (see [`Complex[T]`](#complext--complex-number-type)). All other dtypes surface as `IoError.UnsupportedDtype`. Fortran-order (column-major) arrays are loaded and converted to C-order via `.compact()` automatically. Effect annotation: `reads(Fs)` for load, `writes(Fs)` for save.

### `Complex[T]` — Complex Number Type

**Decision:** `Complex[T]` is the canonical complex number struct of the data library. Must be the single shared definition — two libraries defining incompatible `Complex` types cannot interop (FFT output feeding a filter, `Tensor[Complex[f64], S]` crossing a module boundary, etc.).

**Why deferred:** Pure library work, no language changes required. Validated alongside `std.fft` and `std.linalg`, which are its primary consumers.

**Why non-breaking:** New stdlib type. No existing API affected.

**Design shape:**

```kara
struct Complex[T: Float] {
    real: T,
    imag: T,
}

impl Complex[T] {
    fn new(real: T, imag: T) -> Complex[T]
    fn from_polar(r: T, theta: T) -> Complex[T]   // r * e^(i*theta)
    fn imag_unit() -> Complex[T]                   // 0 + 1i

    fn abs(ref self) -> T             // magnitude: sqrt(real² + imag²)
    fn arg(ref self) -> T             // phase angle in radians
    fn conj(ref self) -> Complex[T]   // conjugate: real - imag*i
    fn norm_sq(ref self) -> T         // real² + imag²  (avoids sqrt)
}

impl Add[Complex[T]] for Complex[T] { ... }
impl Sub[Complex[T]] for Complex[T] { ... }
impl Mul[Complex[T]] for Complex[T] { ... }   // (a+bi)(c+di) = (ac-bd) + (ad+bc)i
impl Div[Complex[T]] for Complex[T] { ... }   // multiply by conjugate / norm_sq
impl Neg for Complex[T] { ... }
impl PartialEq for Complex[T] { ... }
impl Display for Complex[T] { ... }   // "3+2i", "3-2i", "2i", "3"
impl Debug for Complex[T] { ... }
```

**Memory layout:** interleaved `[real0, imag0, real1, imag1, ...]` — matches FFTW's convention and C99's `_Complex` ABI, enabling zero-copy handoff to FFTW or CUDA complex kernels. `Tensor[Complex[f64], Shape]` is the canonical type for FFT output and complex-valued signal processing.

**No complex literal syntax.** Users write `Complex.new(3.0, 2.0)` or `Complex.from_polar(r, theta)`. A `2.0i` suffix is deferred — it requires careful lexer work to avoid ambiguity with the integer suffixes `i8`, `i16`, `i32`, `i64`.

### Typed `einsum` with Compile-Time Index Checking

Named index dimensions checked at compile time, eliminating string parsing and all runtime shape errors from contraction expressions.

```kara
// Hypothetical syntax — named index dims as const-generic symbols
let c  = einsum[i j, j k -> i k](a, b);   // K-dim mismatch caught at compile time
let tr = einsum[i i ->](a);               // diagonal constraint enforced statically
```

Requires either a proc-macro equivalent or new generic symbol kinds in the type system. The string-notation [`einsum`](#stdeinsum--einstein-summation) covers the practical use case; typed einsum is an ergonomics and safety improvement. Revisit once comptime is stable and the numerical stdlib has real-world usage data.

### Lazy DataFrame Query Optimizer Expansion

**Decision:** A later expansion of the first `LazyDataFrame` optimizer (Option A; see [Lazy DataFrame Query Planner](#lazy-dataframe-query-planner)). Adds: join reordering, filter combining, push aggregations through joins, scan-time filter pushdown, projection-aware Parquet reads. Target ~5-7K LOC additional, ~3-4 months focused. Non-breaking: optimizer extension only; user-facing `LazyDataFrame` API unchanged.

**Why later.** The full optimizer is the right *target* but the wrong first *commitment*. Polars in Rust ships ~10K LOC of query optimizer; even a half-sized fresh implementation is a 3-4 month line item. Option A's gap vs Polars is in complex multi-join analytics, which is exactly the workload where users reach for DuckDB. The first release ships Option A with honest docs framing; this expansion lands when user feedback identifies multi-join analytical workloads as a recurring friction point.

**Why non-breaking:** Optimizer-internal — the `LazyDataFrame` surface (filter/select/group_by/agg/join/sort/limit/collect/explain) does not change. Plans that previously executed produce identical (or strictly better) results with the expanded optimizer.

**Re-evaluation trigger (any one of):**
1. User feedback showing multi-join analytical workloads as a recurring friction point.
2. A flagship-data-engineering demo where the first optimizer's join handling is the visible weakness.
3. Engineering bandwidth available with no higher-priority data-stack work pending.

**Alternative considered (Option C — DataFusion integration, designs-not-taken).** Considered for the first release: wire `LazyDataFrame` → DataFusion `LogicalPlan` → run DataFusion's optimizer → lower back to Kāra physical execution. Rejected then because (a) plan-IR bridge work in both directions is non-trivial and underestimated by the "4-6 weeks integration" framing — DataFusion assumes Arrow throughout, which aligns with Kāra Column layout, but plan-translation in both directions is real work; (b) external optimizer dependency conflicts with the language's "owns the stack" posture; (c) Kāra Column nullability and NaN semantics would have to bend to DataFusion's Arrow assumptions or accept a semantic-mismatch layer. Documented as the alternative considered so future contributors don't re-litigate. If this expansion (Option B) proves harder than expected, Option C revives as a fallback — but with full awareness of these trade-offs.

### Neural Network Framework (`std.nn` / `std.optim`) — Decision Deferred

**Scope:** This entry covers the neural-network framework layer on top of [`std.autograd`](#stdautograd--automatic-differentiation-reverse-mode): `std.nn` (layers — Linear, Conv2d, BatchNorm, LayerNorm, MultiheadAttention, `Sequential` composition) and `std.optim` (optimizers — SGD, Adam, AdamW, lr schedulers).

**Decision deferred to engineering start.** Whether `std.nn` and `std.optim` ship alongside `std.autograd` or live as community territory is **not committed**. Decide at engineering start when there's signal on (a) how clean the manual-layer-composition story feels with autograd only, (b) whether early users and dogfooding workloads are asking for layer abstractions in the library, (c) whether positioning tension (an NN framework pulls Kāra harder toward "ML framework" framing) has cashed out in practice. Default until then: not included.

**What it rests on (when built):**
- `std.autograd`, the gradient engine.
- `Tensor[T, Shape]` and the `Var[T, S]` autograd wrapper.
- `f16`/`bf16` numeric types — mixed-precision training.
- GPU codegen ([GPU](#gpu)).

**Minimum viable scope (when built):** `nn` module with `Linear`, `Conv2d`, `LayerNorm`, `BatchNorm`, `Embedding`, `Dropout`, `MultiheadAttention`, `Sequential` for composition; optimizers (SGD, Adam, AdamW) with lr schedulers; loss functions (`cross_entropy`, `mse`, `huber`, `binary_cross_entropy`). All built on top of `std.autograd` `Var[T, S]`.

**JAX-style `grad(f)` as a language primitive** — speculative. A pure function transform `grad(f)` where the compiler verifies `f` carries no effects could be offered natively. Deferred until comptime is stable and `std.autograd`'s tape-based library has revealed what such an API actually needs.

## Systems

Profiles, interrupts, volatile access, inline assembly, linker control, the exported C ABI and SIMD, with the other low-level controls that embedded and kernel code need.

### Project Profiles

> Project profiles return under a manifest key other than `[profile]`: in v1 that table holds the build profiles `[profile.dev]` and `[profile.release]` ([design.md §16](design.md#build-profiles)). A custom panic handler is the `#[panic_handler]` attribute alone ([design.md §16](design.md#panic-handler)), so profiles carry no handler setting.

Project-wide constraints declared in `kara.toml`. Profiles map to sets of effect restrictions enforced by the compiler across the entire project — no per-function annotations needed.

```toml
[profile]
name = "kernel"
no_effects = ["allocates(Heap)", "panics"]
no_std = true
```

**Built-in profiles:**

| Profile | Restrictions | Use case |
|---|---|---|
| `kernel` | No heap allocation, no panics, no std | OS kernels, hypervisors |
| `embedded` | No heap allocation, no panics, no std, no concurrency | Microcontrollers, interrupt handlers |

**Memory safety properties under restricted profiles.** The `embedded` and `isr` profiles may weaken two of the five memory-safety properties defined in [design.md §1](design.md#starting-assumptions), explicitly, never silently:

- **Spatial safety (bounds checking):** `embedded`/`isr` profiles may disable slice bounds-checking panics via `bounds_checks = false` in `kara.toml`. This is an explicit opt-in the profile author takes responsibility for. Without it, bounds checking is active even on embedded targets.
- **Integer overflow behavior:** `embedded` profile defaults to `wrapping` arithmetic rather than panicking. This is still defined behavior (never UB), but the behavior changes. The profile declaration makes this visible; individual projects may override to `checked` if they prefer.

**Profile flag `panic_on_alloc_failure`.** See [Fallible-Allocation Mode](#fallible-allocation-mode).

Temporal safety, type safety, and data race freedom are not weakened by any built-in profile — they hold on all targets in safe code. `unsafe` blocks are the only escape from these guarantees, and they require explicit annotation.

**Recursion in profiles.** GPU code forbids recursion entirely — GPU hardware has no call stack, so recursive calls cannot be lowered. The `kernel` and `embedded` profiles do **not** ban recursion: embedded hardware has a real call stack, and bounded recursion (tree walks, parser descents, state machines) is a legitimate pattern even on small MCU stacks. Deep recursion is impractical on typical embedded targets (2–16 KB stack), but that is a runtime constraint, not a language-level prohibition. Projects that want to ban recursion in embedded code can add `no_recursion = true` to a custom profile.

**Custom profiles** are supported from day one — a profile is just a named list of effect restrictions, so user-defined profiles cost nothing extra:

```toml
[profile]
name = "isr"                           # interrupt service routine
no_effects = ["allocates(Heap)", "panics", "writes(FileSystem)"]
no_std = true
```

A `deterministic` profile — useful for replayable simulations, offline batch jobs, build-system tooling, and any code where every run must produce byte-identical output — is just a bundle of forbidden nondeterminism resources, built from the resources named in [design.md §12](design.md#nondeterminism-resources). No new language feature is needed:

```toml
[profile]
name = "deterministic"
no_effects = ["reads(Clock)", "reads(RandomSource)", "reads(Env)"]
```

Under this profile, any function that transitively reads the wall clock, draws from the system RNG, or touches environment variables is a compile error. Tests that want determinism can still use `#[with_provider]` to inject fake clocks and fake RNGs without enabling the profile project-wide; the profile is for shipping binaries where determinism must hold by construction, not just by test discipline.

The compiler treats a profile as project-wide `#[no_effect(...)]` applied to every function — public and private. Effect checking is transitive, so restricting only public functions would allow violations to hide inside private call chains. Violations are compile errors with the same diagnostics as per-function effect violations. There is no `#[allow]` override; the only escape hatch is `unsafe { ... }`.

**Composing profiles.** A project may enable more than one profile at once by listing them in `kara.toml`:

```toml
profiles = ["embedded", "isr"]
```

The effective constraint set is the union: every `no_effects` entry across all enabled profiles is forbidden, and boolean flags like `no_std` are required if any enabled profile requires them. Libraries do not declare profile membership — a library is compatible with a profile iff its reachable functions' declared effects are disjoint from the profile's forbidden set, which the effect checker already verifies transitively. The same library therefore works under any profile whose constraints it satisfies, without per-profile annotations. This is deliberately not a general-purpose constraint system: profiles are named bundles over a closed taxonomy (forbidden effects plus a small set of boolean flags), so composition is set union on atoms the effect checker already understands — no fixpoint, no subtyping, no solver.

`engine` and `server` profiles are reserved names — they will be defined when their underlying features (SIMD auto-optimization, observability hooks, hot-reload) are implemented. Using them before implementation is a compile error.

**Profile-level disable.** The `embedded` and `isr` profiles may set `bounds_checks = false` in `kara.toml` to disable runtime bounds-checking panics entirely (see above). This is an explicit opt-in that the profile author takes responsibility for; safe code on `app` and `lib` profiles always has runtime bounds checks (subject to (a)'s elision for provably-redundant cases).

#### Profile mapping

| Profile | `std` | `alloc` | `core` |
|---|---|---|---|
| default | yes | yes | yes |
| `embedded` | no | optional | yes |
| `kernel` | no | no | yes |

The `kernel` profile sets `no_std = true` and `no_alloc = true`. The `embedded`
profile sets `no_std = true`; `no_alloc` is opt-in (some embedded systems have
heap allocators).

#### `no_alloc` formatting

`f"..."` interpolation requires `alloc` and is unavailable in `no_alloc`
contexts, so a `no_std` + `no_alloc` program needs a way to format into a
caller-supplied fixed-size buffer:

```
let mut buf: Array[u8, 64] = Array.zeroed();
// ... format `temp` and `rpm` into `buf` ...
uart.write(buf.as_str());
```

**The spelling is deliberately not shown.** Kāra has no macros, so the
operation cannot be a `name!(...)` form. A bare call does not express it either: the arguments are
variadic behind a format string, which no v1 call syntax provides. Fixing the
notation without deciding the mechanism would only move the problem, so the
example above is left as a comment rather than as code an author could
transcribe and be misled by.

**Panic record under `panics_off`.** The panic record ([design.md §17](design.md#panic-record)) does not apply: panics are statically unreachable, the runtime's panic-report path is compiled out. Programs that compile under `panics_off` do not link against `std.panic`'s report writer at all (zero overhead, zero footprint).

**Why kernel and ISR profiles forbid allocation and panics.** The prohibition of both in kernel/ISR profiles (`no_effects = ["allocates(Heap)", "panics"]`) can make them look equivalent, but the reason is different in each case: `allocates(Heap)` is forbidden because `mmap`/`brk` require kernel VM management unavailable at ring 0; `panics` is forbidden because unwinding infrastructure (DWARF tables, landing pads) is absent in freestanding environments. Coincident prohibition, different root cause.

> **Open.** A panic aborts in every build ([core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8)), so the unwinding reason given for `panics` no longer holds. This track must restate why the kernel profile forbids `panics`, or drop that restriction.

Where heap allocation is forbidden:

- **Embedded interrupt service routines (ISRs)** — most embedded MCUs have no heap at all; the `embedded` project profile raises `allocates(Heap)` to a compile error everywhere, covering the ISR case structurally.
- **Kernel / hypervisor** — heap allocation requires VM management (`mmap`, `brk`) unavailable at ring 0; the `kernel` profile enforces the same prohibition.

**Relationship to `#[profile(...)]`.** `#[profile(P1, P2, ...)]` on a function asserts that its effect set satisfies the constraints of every listed profile — the profile's `no_effects` list (above) supplies the forbidden set, and a violation is `error[E_PROFILE_INCOMPATIBLE_EFFECT]`. The two attributes are the same machinery reached from opposite ends: `#[profile]` names a target environment and inherits whatever that environment forbids, while `#[no_effect]` ([design.md §12](design.md#effect-verbs)) names the forbidden effects directly and is independent of any profile. Reach for `#[profile(embedded)]` when the function must be valid for a named target; reach for `#[no_effect(allocates(Heap))]` when one specific boundary carries a constraint the surrounding project does not.

**`sync` types under the kernel profile.** Forbidden: a `sync struct` requires heap allocation, and the kernel profile forbids `allocates(Heap)`.

**The `Hardware` resource.** This track adds one built-in resource to [design.md §12](design.md#resources):

| Verbs | Resource | Covers |
|---|---|---|
| `reads`/`writes` | `Hardware` | memory-mapped I/O, volatile register access, inline assembly with hardware side effects |

### Fallible-Allocation Mode

v1 has the fallible `try_*` methods and `AllocError` ([library/collections.md](library/collections.md#fallible-allocation)). This entry adds a project-wide mode in which only those methods may be called.

**Profile flag — `panic_on_alloc_failure`.** A `kara.toml` profile flag, default `true` (panic on OOM), settable to `false` to switch the project to fallible-allocation-only mode. It is settable in any profile that permits heap allocation (not `embedded`, `kernel` or `isr`, where heap is forbidden outright). The canonical use case is a project running under the `app` profile that wants explicit OOM handling for real-time, fault-tolerant, or kernel-with-allocator workloads:

```toml
[profile]
name = "realtime_server"
panic_on_alloc_failure = false
```

Under `panic_on_alloc_failure = false`, the panicking variants are rejected at call sites:

- **Direct calls to panicking allocators** are rejected with `error[E_PANICKING_ALLOC_REJECTED]: 'Vec.push' may panic on allocation failure; use 'Vec.try_push' instead under panic_on_alloc_failure = false`. The diagnostic includes the type-swap fix-it inline. The check fires at typechecker phase against a hardcoded list of "alloc-on-OOM" stdlib functions/methods enumerated below.
- **Indirect allocators** are rejected at the call / operator / literal site that actually allocates. Concretely: every `+=` operator on `String` (`s += other`), every iterator chain landing in a `collect()` into a `Vec` (the panicking `Vec.collect` variant), every `Vec` collection literal `[1, 2, 3]` (which lowers to `Vec.with_capacity` + per-element `Vec.push`), every `f"..."` interpolated string (which lowers to `String.with_capacity` + `String.push_str`), every `Vec.clone()` / `String.clone()` / `Map.clone()` invocation (the derived `Clone` body allocates), every `?` propagation from a `From`-chain conversion that happens to allocate. The diagnostic at each site names the panicking primitive and the `try_*` equivalent; the user replaces the literal/operator with an explicit fallible call, e.g. `let v = Vec.try_from_iter(0..1024)?;` instead of `let v: Vec[i64] = (0..1024).collect();`.
- **`#[derive(Clone)]` on a type whose generated `Clone` body allocates** is rejected at the derive site under fallible-alloc with `error[E_DERIVE_CLONE_ALLOCATES]` plus the suggestion to write a manual `try_clone` method instead. (A more general "fallible Clone" derive is not designed yet.) The user's options are (a) write a manual `Clone` impl that panics on OOM and disables this check via a per-binding `#[allow(derive_clone_allocates)]`, (b) write a manual `try_clone` and skip the `Clone` impl, or (c) make the type non-cloneable.
- **`?`-propagation note.** A `Result[T, AllocError]` returned by a `try_*` call propagates through `?` exactly as any other `Result` does. Functions that call `try_*` methods on caller-owned collections must include `AllocError` in their error type's `From`-chain, the same convention as any other propagated error.

The flag is **orthogonal** to existing profiles. The `embedded` and `kernel` profiles continue to forbid heap entirely via `no_effects = ["allocates(Heap)"]`; they are unaffected by `panic_on_alloc_failure` because there is no heap to fail over. A user who wants heap-with-no-panics-on-OOM opts into the flag in any profile that permits heap (typically `app` or `lib`, or a custom profile). The flag controls whether OOM is *recoverable* (the user gets a `Result`) or panics; a panic, when one happens, always ends the process.

Rows of the fallible-allocation table that apply only under this flag:

| Type | Panicking method | Fallible companion |
|---|---|---|
| `String` | `+` operator on `String` | rejected under fallible-alloc; use explicit `try_push_str` |
|  | `f"..."` interpolation | rejected under fallible-alloc; use explicit `try_*` builder |
| `Vec` / `String` / `Map` / `Set` collection literals | `[1, 2, 3]`, `f"..."`, `[k: v]` | rejected under fallible-alloc; users construct via explicit `try_with_capacity` + `try_push` chains |

**Runtime allocator interface.** The stdlib's underlying allocator function (`alloc`, `realloc`) returns `*mut u8` with `null` on failure. The panicking variant in stdlib calls `alloc` and panics on `null` via a `#[track_caller]` helper that surfaces the *caller's* source location in the panic record (per the existing stdlib panic-emitter rule). The fallible variant returns `Err(AllocError.OutOfMemory { requested_bytes })`. Under `panic_on_alloc_failure = false`, the panicking variant is unreachable from user code, but the variant's body still exists in the binary — the variant is rejected at *call sites*, not removed from the stdlib. Stdlib code itself (which the user may transitively call through generic interfaces) is checked under the same flag — stdlib functions that internally call panicking allocators are themselves rejected from fallible-alloc projects; stdlib provides explicit fallible counterparts for the public surface.

**Why a flag, not a new profile.** A dedicated profile (e.g., `realtime`) was rejected because the panic-on-OOM choice is orthogonal to other profile choices (wrapping arithmetic, no atomics, `no_std`). Forcing users into a multi-axis profile matrix to express "I want heap with fallible alloc but otherwise app-like behavior" is the wrong granularity. A single flag is the right knob; users compose it with any existing profile.

### Embedded-Profile Arithmetic

[design.md §5](design.md#numeric-semantics) states that integer arithmetic traps in every build. The `embedded` profile changes that default.

On the `embedded` profile, bare arithmetic defaults to two's-complement **wrapping** instead of trapping — embedded targets often lack panic infrastructure, and wrapping matches hardware ALU behavior. In all profiles the behavior is fully defined; no profile produces undefined behavior on overflow.

```
let x: i64 = i64.MAX;
let y = x + 1;                  // RUNTIME ERROR in app/lib: integer overflow
                                 // WRAPS to i64.MIN in embedded (default behavior)
let z = x.wrapping_add(1);      // wraps to i64.MIN in any profile — explicit intent
```

For `app` and `lib` profiles, the named method families above are the only escape hatches from the default-trap behavior. The `embedded` profile changes the default: bare `+`/`-`/`*`/`/`/`%`/`<<`/`>>` operators wrap by two's complement throughout (not just in `unsafe` blocks) — the whole profile opts into wrapping as the default for all arithmetic. Named methods remain available in all profiles for the non-default behavior (e.g., `x.checked_add(y)` in `embedded` for explicit trap-on-overflow). Per-function or per-block `#[checked]` / `#[wrapping]` attribute regions are a separate entry ([Scoped `#[wrapping]` / `#[checked]` Arithmetic Regions](#scoped-wrapping--checked-arithmetic-regions)): their layering questions (does the attribute cross inlining boundaries? does it propagate through generics?) must be resolved once the full overflow-semantics story has real-user pressure. Until then, named methods handle every per-site override and the profile handles the project-wide default.

### Scoped `#[wrapping]` / `#[checked]` Arithmetic Regions

A block- (and function-) scoped attribute that flips the default integer-overflow behavior for the bare arithmetic operators (`+ - * << >> …`) lexically inside the annotated region: `#[wrapping] { … }` makes bare operators two's-complement-wrap (straight-line `add`/`sub`/`mul`, no `llvm.s*.with.overflow` + trap branch), and `#[checked] { … }` re-arms trapping inside an `embedded`-profile (wrapping-default) region. It is the *mid-granularity* opt-out between the per-operation `wrapping_*` / `checked_*` method families (v1) and the project-wide profile default — the overflow-check counterpart of the [Karac-Side Bounds-Check Elimination Pass](#karac-side-bounds-check-elimination-pass) (both recover the cost of a safety check on a hot region without weakening the default). Prior art: Zig's block-scoped `@setRuntimeSafety(false)`, Rust's `Wrapping` newtype.

**Why deferred:** The layering questions are genuinely unsettled and the empirical motivation is thin. (1) *Semantics of "scope."* The attribute must be **lexical, not dynamic** — `#[wrapping] { foo() }` wraps only the operators textually inside the braces, never `foo`'s body — and, critically, must **not depend on inlining**: if the optimizer later inlines `foo` into a wrapping region, `foo`'s arithmetic must not silently start wrapping, so the wrapping-ness has to be bound per-operator at the AST level before any inlining pass runs. Generic propagation (does a `#[wrapping]` region reach into a monomorphized callee body? — no, same firewall as the call boundary) and nesting (innermost attribute wins; `#[checked]` inside `#[wrapping]` re-arms the trap) also need pinning. (2) *Name collision.* This arithmetic-region `#[checked]` currently shares a spelling with the contract-survival `#[checked]` of [Production Contract Checking](#production-contract-checking-checked) — disambiguating the two (a distinct spelling for one, or a shared-attribute-with-argument scheme) is part of resolving the layering. (3) *Motivation.* The v1 opt-out surface already covers every per-site case: `wrapping_*` methods produce identical straight-line, autovectorizable code per operation (the block form is only ergonomics + a region-wide autovec unblock over those methods), and Kāra already sits level with safety-matched `rustc -C overflow-checks=on` across the corpus; the only gap is vs *unchecked* `rustc -O`, which the scoped block would close for a region at the cost of that region's guarantee. Building it before a real workload shows the per-op `wrapping_*` verbosity is friction risks designing the inlining/generic semantics for a hypothetical.

**Promotion gate:** Promote when user data shows a real workload with a hot arithmetic region where (a) the per-operation `wrapping_*` rewrite is materially verbose or obscures the code, AND (b) the region-wide `#[wrapping]` delivers a measured >1.3× win over the trapping default (the arithmetic-kernel gap band measured so far). The trigger is *frequency in real code*, not theoretical coverage — one deliberately-wrapping hash kernel that reads fine with three `wrapping_mul` calls does not justify the attribute.

**Why non-breaking:** Purely additive. Programs without the attribute are unaffected — bare arithmetic keeps its profile default (trap in `app`/`lib`, wrap in `embedded`). The escape hatch stays deliberately **local and greppable** — a bounded lexical region, never a project-wide `overflow-checks=off` flag that would strip the guarantee invisibly (the loud-failure property the trapping default exists to provide; see [design.md §5](design.md#numeric-semantics)).

**Design shape (sketch — finalize at promotion):**

```kara
fn fnv1a(bytes: Slice[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    #[wrapping] {
        for b in bytes {
            h = h ^ (b as u64);
            h = h * 1099511628211;   // bare * wraps here; no trap branch; loop autovectorizes
        }
    }
    h                                // outside the region: trap-default restored
}
```

- Codegen-only switch: a per-region flag read when lowering each arithmetic-operator node (a bare op or a checked op). Type-checking, effect-checking, and the `panics` surface are untouched — arithmetic overflow is already outside the effect system.
- Region membership bound at the AST/operator level *before* inlining, so it is a source-lexical fact rather than an optimizer-dependent one.
- Nesting composes innermost-wins; `#[checked]` is the inverse region, primarily for `embedded`-profile code that wants a trapping island.
- `karac explain` affordance: point at a bare operator and report which region (if any) governs its overflow behavior.

**Cross-reference:** [design.md §5](design.md#numeric-semantics) (the trapping default and the `checked_*`/`wrapping_*`/`saturating_*`/`overflowing_*` method families that are the v1 per-site opt-out); [Embedded-Profile Arithmetic](#embedded-profile-arithmetic); [Karac-Side Bounds-Check Elimination Pass](#karac-side-bounds-check-elimination-pass) (the bounds-check counterpart, with the same promotion-by-real-frequency gate); [Production Contract Checking](#production-contract-checking-checked) (the `#[checked]` spelling this collides with). A compile-time overflow prover was tried and rejected; this scoped opt-out is the lever recommended instead.

<a id="fine-grained-conditional-compilation-cfg"></a>
### Conditional Compilation Beyond Targets

v1 has item-level `#[cfg]` with the `target` and `target_os` keys and the `all`, `any` and `not` combinators, on items, impl members, struct fields and enum variants, plus platform files ([design.md §4](design.md#4-modules-and-packages)). This entry covers what v1 leaves out: CPU-feature keys and `#[cfg]` on statements.

**Why deferred:** Platform files and item-level `#[cfg]` cover the common cases. Every case where `#[cfg]` would be needed on a statement can be handled by extracting the platform-specific part into a function and calling it from shared code. Slightly more verbose but keeps conditional logic in one place rather than scattered through the codebase. A `target_feature` key is needed only by code that calls architecture-specific intrinsics, which arrives with [Portable SIMD](#portable-simd--vectort-n).

**Why non-breaking:** Purely additive. New keys and new positions; existing conditions keep their meaning.

**Design shape:**

```kara
#[cfg(target_feature = "avx512")]
fn fast_multiply(a: Vector[f32, 8], b: Vector[f32, 8]) -> Vector[f32, 8] { ... }
```

v1 has no feature flags, in `kara.toml` or as `#[cfg(feature = ...)]`, and a constrained feature axis is held for a later RFC ([design.md §4](design.md#4-modules-and-packages)).

### `#[no_std]` / Bare Metal Support

**Decision:** The design is bare-metal-compatible. No design changes needed. To add later: (1) `#[no_std]` module attribute, (2) compiler enforces no `allocates(Heap)` effect, no `shared struct`, no Vec/Map/String construction, (3) `ref String` for string literals works on bare metal (static data, no heap). Effect system provides better foundation than Rust's blanket `#![no_std]` — can use fine-grained `#[no_effect(allocates(Heap))]`.

**Why non-breaking:** Purely additive. Existing programs unaffected.

<a id="additional-compilation-targets-phase-10"></a>
### Additional Compilation Targets

**Decision:** Extend codegen beyond the v1 targets (`native` and `wasm_wasi`, [design.md §16](design.md#targets)) to embedded targets. The browser target arrives with [Web](#web) and GPU targets with [GPU](#gpu).

**Why deferred:** Each target has unique constraints (no heap for embedded, no recursion for GPU, etc.) that are better addressed after the core compiler pipeline is stable.

**Why non-breaking:** Purely additive backend targets.

<a id="atomic-operations-and-memory-ordering-phase-10--partial"></a>
### Atomic Operations and Memory Ordering

v1 has `Atomic[T]` with sequentially consistent `load`, `store`, `swap`, `compare_exchange`, `fetch_add` and `fetch_sub`, and no ordering argument ([library/concurrency.md](library/concurrency.md#atomics)). This entry adds the rest: weaker memory orderings, `fetch_and` and `fetch_or`, fences, and atomics under the embedded profile.

**Why deferred:** The weaker orderings and the bitwise read-modify-write operations need more careful memory-model reasoning, and their main users are embedded and kernel code, which arrive with this track.

**Why non-breaking:** Purely additive, provided the v1 methods keep their sequentially consistent meaning when no ordering is given.

> **Open.** How an ordering is passed is not decided. The examples below pass it as a trailing positional argument, which v1 methods do not take; a named argument that defaults to `SeqCst`, or separate methods, would keep v1 calls unchanged.

**Design shape:**

The canonical embedded use is ISR-to-main signaling: `store(true, Release)` in the ISR, `load(Acquire)` in the main loop.

```kara
let flag: Atomic[bool] = Atomic.new(false);

// In interrupt handler:
flag.store(true, Release);

// In main loop:
while not flag.load(Acquire) { /* spin */ }
```

#### Methods

The v1 methods gain an ordering, and two bitwise methods are added:

| Method | Signature |
|---|---|
| `compare_exchange` | `fn compare_exchange(self, current: T, new: T, success: Ordering, failure: Ordering) -> Result[T, T]` |
| `fetch_and` | `fn fetch_and(self, val: T, ord: Ordering) -> T` (integer `T` only) |
| `fetch_or` | `fn fetch_or(self, val: T, ord: Ordering) -> T` (integer `T` only) |

#### Memory Ordering

```kara
enum Ordering {
    Relaxed,
    Acquire,
    Release,
    AcqRel,
    SeqCst,
}
```

Semantics match the C11/LLVM memory model exactly.

#### Fences

```kara
// Safe — compiler-only reordering barrier; no hardware instruction emitted
compiler_fence(Ordering);

// Unsafe — emits a hardware barrier instruction (dmb, mfence, etc.)
unsafe { fence(Ordering); }
```

`fence` is `unsafe` because a misplaced hardware barrier can cause incorrect behavior in concurrent/interrupt-driven code, not just a performance regression. `compiler_fence` is safe — it only constrains compiler instruction scheduling.

```kara
// Memory fence for DMA completion:
unsafe {
    dma_buffer[0] = payload;
    fence(Release);         // all writes visible before DMA sees the buffer
    dma_start_reg.write(1); // trigger DMA
}
```

#### Effect Model

v1 atomics are effect-free ([library/concurrency.md](library/concurrency.md#atomics)); `fetch_and`, `fetch_or` and the fences are too. The reasoning:

**Why effect-free.** Atomic operations are linearizable by construction — any interleaving of concurrent atomic ops on the same atomic is a valid execution. Tracking atomic ops as `writes(...)` would force auto-concurrency to serialize them, discarding the entire point of lock-free concurrent access. Tracking them as `reads(...)` would misrepresent `fetch_add`/`store` as pure. The only consistent choice is to leave atomics outside the effect system's resource-access model: hardware atomicity ensures memory safety at the language level, and the programmer's ordering reasoning happens one layer lower via the memory-ordering arguments.

**Effect-free is not conflict-free.** If two private functions both update the same `Counter` via `fetch_add`, both infer empty effect sets and auto-concurrency is free to run them in parallel. This is *correct* — `fetch_add` is linearizable, so any interleaving produces a well-defined result (one of the valid orderings). Programmers who need a specific ordering between two atomic updates must use memory orderings (`Release` / `Acquire` happens-before) or a higher-level synchronization mechanism (`Mutex[T]`, channels), neither of which is expressed as an effect-system atom.

**Memory orderings are not effects.** `Ordering.Relaxed`, `Acquire`, `Release`, `AcqRel`, and `SeqCst` are codegen attributes that constrain hardware memory barriers, not effect-system atoms. They do not appear in `karac explain` effect-inference output, do not participate in effect subsumption, and are not rejected by an effect-boundary check. A `karac explain --memory-model` view may later surface them for reasoning about data-race freedom.

**Summary.** For any code that uses atomics or locks, the effect system tracks *what* is touched (via the resources reached by reads/writes to non-atomic state) and the mutex/atomic layer tracks *when* it is safe to touch it. Programmers who want "which resources does this function access" read the function's declared or inferred effect set. Programmers who want "in what order will these operations observe each other" read the memory-ordering arguments and the `Mutex[T]` structure. The two layers do not overlap, and the language commits to never collapsing them into a single notion.

#### Embedded Profile

Atomics and fences are available in the `embedded` profile. The `embedded` profile bans scheduler-level concurrency (spawning tasks, task queues, channels requiring a runtime). ISR-to-main communication via `Atomic[T]` is the *correct* embedded pattern and is explicitly permitted. Single-core multi-context (ISR + main) is not "concurrency" in the scheduler sense.

### Module-Level Mutable State

v1 allows only `const` at module scope ([design.md §4](design.md#constants)). This entry brings back module-level `let` and `let mut`, mainly for embedded and bare-metal code, together with the lazy-initialization family.

Module-level initializers would additionally allow:

- FFI pointer literals for hardware addresses in `unsafe` embedded contexts (see [design.md §15](design.md#unsafe)).

```kara
let mut SCRATCH: Array[u8, 256] = [0; 256];
```

**`let` vs `let mut` at module scope.** Both are allowed. `let` is immutable; `let mut` permits the binding to be mutated at runtime by code that declares the appropriate effect. Module-scope `let mut` is primarily intended for embedded and bare-metal code where memory-mapped regions, DMA buffers, and hardware registers live at fixed addresses. In application code, prefer runtime-initialized values inside `main` over module-level `let mut`, or the thread-local / wrapper types below.

**Effect attribution — synthetic per-binding resource.** Every module-level `let mut BINDING` implicitly declares a project-internal `effect resource BINDING_resource;` associated with that binding. Reading the binding (in any function body) contributes `reads(BINDING_resource)` to that function's inferred effect set; assignment contributes `writes(BINDING_resource)`. This plugs straight into conflict analysis (Feature 2) — two concurrent readers never conflict, a reader and a writer always conflict, and two writers always conflict. The per-binding resource is intentionally fine-grained: a single `writes(GlobalModule)` bucket would force every module-level mutation to serialize against every other, defeating the whole point of the effect system. The synthetic resource is not exportable and not namable in user code — it exists only for conflict analysis. A function that mutates `BINDING` has `writes(BINDING_resource)` in its inferred effect set, and effect inference then propagates normally.

**Function-local bindings and heap allocations.** Only module-level `let mut` bindings receive synthetic per-binding resources. Within a function body, reads and writes to struct fields, `Vec` elements, and other heap-allocated data do not contribute to the function's named-resource effect set — they are governed solely by the ownership system's aliasing analysis (parameter modes, borrow rules, and the move checker). Two function parameters of the same type that are proven non-aliased by the ownership checker — for example, `ref self: Grid` and `next: Grid` as distinct parameters — access independent heap allocations; no named-resource conflict arises, and the effect system allows parallel callers of such functions. The module-level restriction is intentional: synthetic resources are non-exportable identifiers used for conflict analysis at a cross-call granularity; intra-function heap accesses already have the ownership layer as their correctness guarantee.

**`pub fn` and the synthetic resource — by-design restriction.** A `pub fn` that directly mutates a module-level `let mut` binding cannot satisfy `public_effects = "declared"` because the synthetic resource is not namable. This is intentional: direct public mutation of global state without a named synchronisation primitive is not representable in the full effect-declaration discipline. The two supported paths are: (a) wrap the binding in a named concurrency primitive (`Atomic[T]` or `Mutex[T]`) and expose `pub fn` methods that declare effects on those well-known resources; or (b) use `public_effects = "inferred"` at the project level, which causes the compiler to infer and display the synthetic-resource effect without requiring an explicit declaration — the correct escape hatch for `embedded`-profile device-driver packages that expose raw MMIO or DMA writes publicly. Under `public_effects = "inferred"` the effect is still tracked and participates in conflict analysis; callers that call two such functions concurrently still see the conflict.

**Concurrency rule.** Any `par { }` branch or spawned task whose *transitive* effect set contains `writes(BINDING_resource)` — whether from a direct assignment in the branch body or from a call to a function that carries the effect — is a **compile error** unless the binding's type is an explicit concurrency primitive (`Atomic[T]` or `Mutex[T]`). The check is effect-set-based, not syntactic: calling `bump()` inside `par { }` when `bump` has `writes(COUNTER_resource)` in its inferred effect set is caught exactly as if the assignment appeared inline. Likewise, a `par { }` branch with `reads(BINDING_resource)` in its effect set and a sibling branch with `writes(BINDING_resource)` is also an error (reader–writer conflict on the same resource). Two branches that both only `reads(BINDING_resource)` are fine — `reads` + `reads` is a non-conflict in the effect system. This is a static rule backed by the synthetic-resource effect: because `writes(BINDING_resource)` conflicts with itself (and with `reads`) across branches, the existing conflict analysis would already serialize the offending branches — but serializing within a `par { }` region is almost never what the programmer meant, so the compiler upgrades the conflict to an error and requires the programmer to either use a concurrency primitive or refactor. The diagnostic is specific: `"module-level let mut 'counter' cannot be written from inside par { } — wrap in Atomic[i64], Mutex[i64], or use #[thread_local] for per-task state"`.

**`#[thread_local]` — the per-task alternative.** A module-level `let mut` may be annotated `#[thread_local]`, giving each OS thread (and therefore each task under the v1 runtime) its own independent copy of the binding. The attribute is permitted in every profile that allows module-level bindings in the first place (i.e., all profiles except `gpu`, where thread-local has no meaningful execution-model analog). Effect attribution becomes `writes(ThreadLocal[BINDING_resource])` — a parameterized resource that, because each task holds a disjoint instance, never conflicts with itself across tasks. The binding's initializer must still be compile-time-constant; the per-thread copy is zero-cost to create (a segment of thread-local storage is reserved at link time).

```kara
#[thread_local]
let mut SCRATCH: Array[u8, 4096] = [0; 4096];    // each task gets its own

fn process(msg: Message) {
    // mutation is per-task — no synchronization, no conflict with siblings
    SCRATCH[0] = msg.header;
    // ...
}
```

**`LazyLock[T]` — expensive-init, thread-safe, write-once.** The stdlib ships `LazyLock[T]` (the equivalent of Rust's `OnceLock`/`LazyLock`) for values that need runtime initialization but are thereafter immutable and shared across tasks. `LazyLock[T]` is constructed at module scope with a closure-typed initializer; the initializer runs on first access and is guaranteed to run at most once. Because the value is logically immutable after init, `LazyLock` is declared with `let` (not `let mut`) at module scope, and the effect system attributes the first-access initialization to the *calling* function, not to the module. This sidesteps the entire "module init" hazard while giving programmers a clean non-`let mut` path for the common "load a regex once, reuse forever" case. The constant-init rule applies to the `LazyLock.new(|| ...)` call site — `LazyLock.new` is a compiler-recognized special form (not a user-callable `const fn`) whose argument closure is stored as a function pointer in read-only data; only the closure body runs later at first access. The closure passed to `LazyLock.new` may only capture other module-level compile-time bindings; captures of runtime state are a compile error.

```kara
let GREETING_REGEX: LazyLock[Regex] = LazyLock.new(|| Regex.compile("hello.*").unwrap());

fn handle(msg: String) -> bool {
    GREETING_REGEX.get().is_match(msg)    // first call initializes; subsequent calls hit cached value
}
```

**`OnceLock[T]` — explicit-set, thread-safe, write-once.** Sibling of `LazyLock[T]` for the *late-bound* case: a value that must be settable from a function context after module load, typically because the value depends on runtime input the closure cannot see at module-load time. Canonical use: `main` parses CLI flags, opens a database connection, or reads a config file, and stores the result in a globally visible `OnceLock[T]` binding for the rest of the program to read. No closure is baked into the constructor; the value is provided at runtime via `set(val) -> Result[(), AlreadySetError[T]]`. Once set, the cell is logically immutable; subsequent `set` calls return `Err(AlreadySetError { rejected: val })` and do not overwrite. The internal representation carries a once-init synchronisation primitive — concurrent `set` calls from sibling tasks produce exactly one winner, the rest get back their rejected value through the `Err` arm, and the same memory-ordering guarantees as `LazyLock` apply.

```kara
let CONFIG: OnceLock[AppConfig] = OnceLock.new();

fn main() -> Result[(), Error]
    with reads(FileSystem) reads(Env)
{
    let cfg = parse_args_and_load_config()?;
    CONFIG.set(cfg).expect("CONFIG must be set exactly once during startup")
    run_server()
}

fn handle_request() {
    let cfg = CONFIG.get().expect("called before main set CONFIG");
    // ... use cfg.timeout_ms, cfg.max_connections, ...
}
```

The method surface:

- `OnceLock.new() -> OnceLock[T]` — compiler-recognized constant-init special form, parallel to `LazyLock.new`. Permitted as a module-level binding's right-hand side; not a user-callable `const fn`.
- `set(ref self, val: T) -> Result[(), AlreadySetError[T]]` — moves `val` into the cell on success; on failure (the cell was already set) returns `Err` carrying back the rejected `val` so the caller can recover it.
- `get(ref self) -> Option[ref T]` — `Some(ref T)` once `set` has succeeded, `None` before. The borrow's lifetime is tied to the cell.
- `get_or_init(ref self, init: OnceFn() -> T) -> ref T` — convenience: if the cell is unset, run the closure, set the cell with the result, and return a borrow; if already set, return the existing borrow without invoking the closure. Race-safe — only one closure execution wins; in a race, the losing tasks discard their constructed values and observe the winner's result.
- `is_set(ref self) -> bool` — purely advisory; the value can flip from `false` to `true` between this call and the next access. Use `get`'s `Option` shape for actual control flow.

Effect attribution mirrors `LazyLock`: first-access initialization (whether through `set` or `get_or_init`) is attributed to the *calling* function, not to the binding. The closure passed to `get_or_init` has its own effect set checked at the call site against the calling function's effect ceiling.

**`OnceCell[T]` — explicit-set, single-task, write-once.** The non-shared sibling of `OnceLock[T]` for **struct-field memoization** and other patterns where the cell is owned by a single task throughout its lifetime. No internal synchronisation — `OnceCell.set` is a plain store behind the cell's interior-mutability surface, and `OnceCell.get` is a bare read. Faster than `OnceLock` because it carries no once-init lock, and intended for the case where the surrounding type's structure already guarantees single-task access:

```kara
struct Parser {
    source: String,
    tokens: OnceCell[Vec[Token]],
}

impl Parser {
    pub fn new(source: String) -> Parser {
        Parser { source, tokens: OnceCell.new() }
    }

    pub fn tokens(ref self) -> ref Vec[Token] {
        self.tokens.get_or_init(|| tokenize(self.source))
    }
}
```

The method surface is **identical in shape and signature** to `OnceLock[T]`'s — `new`, `set`, `get`, `get_or_init`, `is_set` — so a type swap from `OnceCell[T]` to `OnceLock[T]` is mechanical at every call site. The difference is structural: `OnceCell` carries no synchronisation, so the compiler must prove it never crosses a task boundary.

> In v1 terms `OnceCell[T]` is a task-local type, so it is not `CrossTask` ([design.md §9](design.md#crosstask)), and that bound already rejects the two cross-task cases below. Only the module-scope rule is specific to `OnceCell`. The rules are kept as written because they fix the diagnostics.

**Single-task enforcement.** Single-task safety on `OnceCell[T]` is enforced through three explicit structural rules at the type-decl and use sites:

- **Module-scope rejection.** A module-level `let X: OnceCell[T]` binding is rejected at the typechecker with `error[E_ONCE_CELL_AT_MODULE_SCOPE]: 'OnceCell[T]' is single-task; module-level bindings are visible to every task and must be 'OnceLock[T]' instead`. The fix-it offers the type swap inline.
- **Cross-task type rejection.** A field of type `OnceCell[T]` inside a `sync struct`, `sync enum`, or any field reachable from `sync struct` storage is rejected with `error[E_ONCE_CELL_IN_SYNC_TYPE]: 'OnceCell[T]' is single-task; fields of 'sync struct'/'sync enum' must be 'OnceLock[T]' instead`. `shared struct` fields are accepted (`shared struct` is itself single-task, [core-semantics.md §6](core-semantics.md#6-sharing-c7)) — the rejection fires only at the `sync`-keyword boundary, which is the language-level "this type crosses tasks" marker.
- **Cross-task move rejection.** A value of type `OnceCell[T]` moved into a `par {}` branch or a spawned task, or sent through a channel, is rejected with `error[E_ONCE_CELL_CROSSES_TASK_BOUNDARY]: 'OnceCell[T]' cannot cross task boundaries; replace with 'OnceLock[T]' or move the surrounding owned struct's task-local clone into the branch instead`. The check piggybacks on the existing par-region escape analysis (the same pass that already rejects a `shared` handle crossing into a `par {}` branch); no new dataflow is needed.

These three rules give `OnceCell[T]` the same effective single-task containment Rust achieves through `!Sync`, but at three concrete sites with focused diagnostics.

**`AlreadySetError[T]`.** Prelude error type, single-variant struct enum: `enum AlreadySetError[T] { rejected: T }`. The rejected value is preserved on the `Err` arm so a caller who lost a `set` race can recover the value and decide what to do with it (drop it, log it, retry on a different cell, etc.). Implements `Debug` always (so `expect`/`unwrap` panic messages are useful); implements `Display` only when `T: Display` — the conditional impl mirrors the existing rule for derived trait conditional bounds.

**Family comparison.** Three primitives in the lazy-init family:

| Primitive | Initializer | Concurrency | Module-scope | `sync struct` field | Method surface |
|---|---|---|---|---|---|
| `LazyLock[T]` | Embedded closure (`new(\|\| ...)`) | Thread-safe (once-init lock) | Permitted | Permitted | `get` only |
| `OnceLock[T]` | Explicit `set(val)` | Thread-safe (once-init lock) | Permitted | Permitted | `new`, `set`, `get`, `get_or_init`, `is_set` |
| `OnceCell[T]` | Explicit `set(val)` | Single-task (no lock) | **Rejected** | **Rejected** (use `OnceLock`) | `new`, `set`, `get`, `get_or_init`, `is_set` |

Picking the right primitive:

- **`LazyLock`** — value is computed by a known closure that captures only module-level constants. Canonical case: "compile a regex once, use it forever" / "build a static lookup table on first use". The value is logically derivable at module load; lazy is an optimization, not a necessity.
- **`OnceLock`** — value is computed from runtime input that the closure cannot see at module-load time (CLI flags, env vars, config files). `main` (or comparable startup code) calls `set` once during initialization; the rest of the program reads via `get`. Late-bound globals.
- **`OnceCell`** — value memoizes a computation derivable from already-owned state in the surrounding struct (`ref self.source` → `Vec[Token]`). The surrounding struct is owned by one task. Lazy struct-field accessors.

**Profile gating.**

| Profile | Module-level `let mut` | `#[thread_local] let mut` | `let X: LazyLock[T]` |
|---|---|---|---|
| `lib` (default) | **Warning** (`module_mut_binding`) — suppress with `#[allow(module_mut_binding)]` | Permitted | Permitted |
| `app` | **Compile error** — use context-struct, `Mutex`, `Atomic`, `#[thread_local]`, `LazyLock`, or `OnceLock` | Permitted | Permitted |
| `embedded` | Permitted (MMIO, DMA, static buffers) | Permitted | Permitted |
| `gpu` | **Compile error** — global mutable state is incompatible with kernel execution | **Compile error** (no thread-local storage at the GPU execution level) | Permitted (read-only) |

`OnceLock[T]` follows the same per-profile gating as `LazyLock[T]` (the rightmost column of the table) — both are thread-safe, both are write-once, both are permitted at module scope in every profile that allows module-level bindings at all. `OnceCell[T]` is **rejected at module scope unconditionally** in every profile (per the `E_ONCE_CELL_AT_MODULE_SCOPE` rule above) — it is a struct-field primitive, not a module primitive.

Rationale: hidden global mutation is the classic server/CLI footgun the app profile exists to prevent, so app-profile code must name its mutation primitive explicitly. Libraries (`lib` profile, the default) should *prefer* alternatives but may legitimately want module-level `let mut` for niche patterns (config registries, feature-flag stores) — hence the warning-and-allow model rather than an outright ban. Embedded needs the bare-metal MMIO path and is where the feature's core justification lives. GPU forbids both because kernel execution has no notion of persistent per-process or per-thread mutable state.

The `#[allow(module_mut_binding)]` suppression is per-binding, not module-wide — forcing an explicit opt-in at every site — and appears in `karac explain --concept=module-state` with the full alternative menu (context struct, `Mutex`, `Atomic`, `#[thread_local]`, `LazyLock`, `OnceLock`) alongside.

**No `static mut` — deliberate omission.** Kāra has no `static mut` keyword. The closest Rust analog — a raw mutable global accessed without a synchronisation primitive — is rejected by design: every read and write of a module-level `let mut` is attributed to a synthetic per-binding effect resource, conflict-analysed against concurrent siblings, and (in the `app` profile) outright forbidden unless the binding is wrapped in an explicit primitive. The supported paths for genuinely-mutable program-scoped state are `Atomic[T]` (lock-free scalars), `Mutex[T]` (mutual exclusion), `OnceLock[T]` (write-once runtime init via explicit `set`), `LazyLock[T]` (write-once init from a baked-in closure), and `#[thread_local]` (per-task disjoint copies). There is no fall-back path to "raw mutable global, caller's responsibility to synchronise" — the absence of that path is the point.

**Embedded and bare-metal use.** The compile-time initializer rule is consistent with the embedded examples in [Volatile access, inline assembly, interrupts](#volatile-access-inline-assembly-interrupts): interrupt vector tables, DMA buffers, and memory-mapped hardware regions all use array literals and `[0; N]` repeat forms — the rule covers them without special cases. Concurrent mutation of `let mut` module-level bindings from ISR + main-loop contexts uses `Atomic[T]` or explicit memory fences (see [Atomic Operations and Memory Ordering](#atomic-operations-and-memory-ordering)).

Variance of the lazy-initialization types:

| Type | Declaration | Notes |
|---|---|---|
| `LazyLock[=T]` / `OnceLock[=T]` / `OnceCell[=T]` | invariant | Same reasoning as concurrency primitives — `set` writes a `T`, `get` reads it. |

### `#[repr(packed)]`, `#[repr(align(N))]`, `offset_of`

v1 has `#[repr(C)]`, integer `#[repr]` forms and `#[repr(transparent)]` ([design.md §15](design.md#repr-abi-layout)). This entry adds packing, minimum alignment and field offsets.

| Form | Effect | Safety |
|---|---|---|
| `#[repr(packed)]` | No padding at all — every byte counts | Taking a reference to a misaligned field requires `unsafe` |
| `#[repr(align(N))]` | Minimum alignment of `N` bytes (must be power of two) | Safe |

```kara
#[repr(C, packed)]
struct EthernetFrame {
    dst_mac:    Array[u8, 6],
    src_mac:    Array[u8, 6],
    ether_type: u16,
}

#[repr(align(4096))]
struct PageTableEntry {
    entries: Array[u64, 512],
}
```

**`#[repr(packed)]` and references:** Taking a reference to a field of a packed struct is `unsafe` — the field may be misaligned and dereferencing a misaligned pointer is undefined behavior. The compiler enforces this: safe code cannot take `ref packed_struct.field`; the value must be read via `ptr.read_unaligned` inside an `unsafe` block.

**Per-field alignment:** Not supported as a direct attribute. Use a wrapper type with `#[repr(align(N))]` as the field type. This keeps the attribute model simple and the alignment guarantee visible at the type level.

#### Field Offsets: `offset_of[T](field)`

`offset_of[T](field)` returns the byte offset of `field` from the start of a value of type `T`. The form sits beside `T.size_of()` / `T.align_of()` ([design.md §15](design.md#intrinsics)) — type lives in the bracket slot, field is a special-form positional argument. The result is `i64`, the value is computed at compile time, and the operation contributes no effects.

```kara
#[repr(C)]
struct UsbDescriptor {
    length:           u8,
    descriptor_type:  u8,
    bcd_usb:          u16,
    device_class:     u8,
}

let off_type:  i64 = offset_of[UsbDescriptor](descriptor_type);   // 1
let off_class: i64 = offset_of[UsbDescriptor](device_class);      // 4 (with #[repr(C)] padding rules)
```

**Why a primitive.** Four use cases need byte-precise field offsets and have no clean alternative:

1. **FFI.** Binding C structs whose offsets are documented in headers. Cross-checking the Kāra-side field order against the C side is the difference between a working binding and a memory corruption.
2. **Serialisers.** Writing fields to a buffer in offset order, skipping padding bytes — the standard fast-path pattern for `bincode`-style codecs.
3. **Intrusive data structures.** The `container_of` idiom — given a pointer to a field, recover a pointer to the enclosing struct. Used in kernel linked lists, lock-free queues, and any structure where the link pointer is embedded in the contained type rather than wrapping it.
4. **Layout-block introspection.** When a `layout` block has SoA-transformed a struct, knowing the *actual* runtime offset of each field is what serialisers and FFI bindings need — see *Layout-block interaction* below.

**Form.**

```
OFFSET_OF_EXPR = "offset_of" "[" TYPE "]" "(" FIELD_PATH ")"
FIELD_PATH     = IDENT ( "." IDENT )*
```

The argument is a *field path*, not a value expression — a sequence of field-name identifiers separated by `.`. The typechecker special-cases `offset_of[T](path)` so the bare identifier `path` is resolved as a field path against `T` rather than as a value-binding lookup.

**Nested fields.** A dotted path follows the field chain through nested structs:

```kara
#[repr(C)]
struct Outer { a: i32, inner: Inner, c: i32 }

#[repr(C)]
struct Inner { x: i32, y: i32 }

let off_y: i64 = offset_of[Outer](inner.y);   // offset of `inner` plus offset of `y` within `Inner`
```

Each segment must be a named field of the type at the previous segment's resolved type. Indexing (`arr[0]`), method calls, dereferences, and any other expression form are not legal in a field path — a path is identifier-only. The diagnostic `error[E_OFFSET_OF_INVALID_PATH]: offset_of accepts a field-name path; expression forms (indexing, method calls, dereferences) are not legal here` covers attempts to write `offset_of[T](field[0])` or `offset_of[T](*p.field)`.

**Permitted target types.**

| Type kind | Behavior |
|---|---|
| `struct` | Offset of the named field. |
| `#[repr(C)] struct` | Offset under C ABI rules (well-defined, stable). |
| `#[repr(packed)] struct` | Offset under packed rules (no padding). |
| Layout-block struct | Offset under the SoA-transformed runtime layout — see below. |
| `#[repr(transparent)]` carrier | Offset of the inner field is `0`; offset of zero-sized companion fields is `0` (they have no size). |
| `#[repr(C)] union` | Every field's offset is `0`. The compiler accepts the call but emits `warning[W_OFFSET_OF_UNION_FIELD]: every field of a #[repr(C)] union has offset 0; this query may be unintentional`. |
| Generic struct with concrete type args | `offset_of[Vec[i32]](len)` — works whenever the type is fully concrete at the call site. |

**Forbidden target types and field paths.**

- **Opaque foreign type** (`extern "C" { type Foo; }`) — `error[E_OFFSET_OF_OPAQUE_TYPE]: offset_of cannot be applied to opaque foreign type 'Foo'; the type's layout is unknown to Kāra`.
- **Generic type parameter `T`** without a concrete instantiation — `error[E_OFFSET_OF_GENERIC_PARAM]: offset_of requires a concrete type; the type parameter 'T' is not resolvable to a layout at this call site`. Generic functions that need `offset_of` must take the concrete type as a `const` parameter or a separate generic that the caller supplies.
- **Unknown field** — `error[E_OFFSET_OF_UNKNOWN_FIELD]: type 'T' has no field 'name'; available fields are: ...`. The diagnostic enumerates the visible fields (subject to the visibility rule below).
- **Private field** accessed from outside the defining module — `error[E_OFFSET_OF_PRIVATE_FIELD]: field 'name' is private to module 'm'; offset_of honours field visibility`. The visibility rule mirrors field-read visibility: private fields are inaccessible to `offset_of` from outside the module the same way they are inaccessible to direct field reads.
- **Enum variant fields** — `offset_of[MyEnum.SomeVariant](field)` is rejected at parse with `error[E_OFFSET_OF_ENUM_VARIANT]: enum variants do not have stable offsets; offset_of accepts only struct types`. Future extension is additive if a stable per-variant layout commitment ever lands.
- **Dynamically-sized field** (a `[T]` slice or `str` field at the end of a struct, when those land) — when the field's offset is computable, accepted; when the field itself is unsized and lacks a known starting offset (very rare — only the trailing flexible array case), rejected with `error[E_OFFSET_OF_UNSIZED_FIELD]`.

**Layout-block interaction.** When `T` carries a `layout` block that triggers SoA transformation, `offset_of[T](field)` returns the *actual* runtime offset under the transformed layout — i.e., the byte position serialisers and FFI bindings actually need to read or write. The "logical" pre-transformation offset is not exposed; the transformation is total, and a serialiser that read logical offsets would be reading the wrong bytes. For `#[repr(C)]` structs (where SoA is disabled per [`#[repr]`](design.md#repr-abi-layout)), the logical and physical layouts coincide, so the question doesn't arise.

**Const-evaluable.** `offset_of[T](field)` is a compile-time constant when `T` and the field path are statically known. The result may appear in const-generic argument position (`Array[u8, offset_of[Frame](payload)]`), in `static` initializer expressions, in match-arm guard predicates, and anywhere a `const i64` value is permitted. The same compile-time evaluation rules apply as for `size_of` / `align_of` — the value is folded into the binary, no runtime cost.

**Pointer-arithmetic helper: `container_of`.** The intrusive-DS use case is concentrated enough to warrant a stdlib helper:

```kara
// In the prelude `ptr` module — see design.md §15, Raw pointers.
unsafe fn container_of[T, F](field_ptr: *const F, field: <field-path>) -> *const T;
unsafe fn container_of_mut[T, F](field_ptr: *mut F, field: <field-path>) -> *mut T;
```

Both are `unsafe` (the caller asserts that `field_ptr` actually points at a `field`-named field of an enclosing `T` value). Implementation lowers to `field_ptr.with_addr(field_ptr.addr() - offset_of[T](field))` (per [Pointer Provenance](design.md#pointer-provenance)) — the address arithmetic is bounded and provenance-preserving, but the *interpretation* of "this `F` is part of a `T`" is what makes the call unsafe. The `field` argument is a special-form field path identical to `offset_of`'s.

**Why a special form rather than a real function.** A real function `fn offset_of[T](???) -> i64` cannot be written: the second argument is a field name, not a value, and Kāra has no first-class field selectors. The same constraint forces Rust into a macro (`offset_of!`). Kāra has no macros, so the typechecker special-cases the call shape — exactly as it does for `ptr.const(place)` / `ptr.mut(place)`. The sigil `[T]` plus the bare-identifier-path argument is enough to disambiguate the special form from any user-defined function call; no new keyword is needed.

### Bitfield Types

**Decision:** Add `#[repr(C, bitfield)]` with sub-byte integer fields (`u1`-`u63`, `i1`-`i63`) for hardware register modeling. Layout is LSB-first within each word.

**Why deferred:** Bitfield layout rules interact with endianness and padding in non-obvious ways — needs real register-description use cases.

**Why non-breaking:** Purely additive.

**Design shape:**

```kara
#[repr(C, bitfield)]
struct UartStatus {
    error_flags:   u6,    // bits [5:0]
    rx_fifo_full:  u1,    // bit [6]
    tx_fifo_empty: u1,    // bit [7]
    _reserved:     u24,   // bits [31:8]
}
```

Sub-byte integer types are valid only inside `#[repr(C, bitfield)]` structs.

### Volatile access, inline assembly, interrupts

#### Volatile Memory Access (MMIO)

Memory-mapped hardware registers must be read and written with **volatile semantics** — the compiler must not reorder, coalesce, or eliminate reads/writes to I/O registers. Two intrinsics in the `unsafe` surface:

```kara
unsafe fn volatile_read[T: Copy](ptr: *const T) -> T
unsafe fn volatile_write[T: Copy](ptr: *mut T, val: T)
```

**`T` bound is `Copy`**, not a hardware-specific marker. MMIO registers are almost universally integer types (`u8`/`u16`/`u32`/`u64`), all of which are `Copy`. The bound prevents types with destructors or ownership from being used as register values.

**`VolatileCell[T: Copy]`** is a stdlib wrapper (built on the intrinsics above, not a language primitive) for ergonomic register map definitions. It eliminates raw pointer arithmetic at every use site:

```kara
struct Uart {
    data:    VolatileCell[u32],   // 0x000 — transmit/receive data
    status:  VolatileCell[u32],   // 0x004 — status flags
    control: VolatileCell[u32],   // 0x008 — configuration
}

unsafe {
    uart.data.write(b'A' as u32);    // volatile, one bus transaction
    let s = uart.status.read();      // volatile, not cached
}
```

`VolatileCell.read` and `VolatileCell.write` are `unsafe fn` — MMIO access is inherently unsafe. There is no safe abstraction at the language level; safe wrappers are a driver-library concern.

**Effect integration:** `volatile_read` implies `reads(Hardware)`; `volatile_write` implies `writes(Hardware)`. `Hardware` is a built-in primitive resource this track adds (see [Project Profiles](#project-profiles)), parallel to `FileSystem` and `Network`. Kernel and driver functions that touch hardware registers advertise it in their signature.

**`Hardware` is undifferentiated at the language level.** Drivers that need finer-grained effect tracking (e.g., distinguishing `uart0` from `spi1`) define user-defined resources and use them instead:

```kara
effect resource Uart0;
effect resource Spi1;

fn uart_write(b: u8) with writes(Uart0) { ... }
fn spi_transfer(b: u8) with writes(Spi1) { ... }
```

The built-in `Hardware` resource is the default for unattributed volatile access. User-defined resources can be used alongside it when per-device effect tracking is needed.

#### Inline Assembly

Kāra provides `asm` as a keyword expression (not a macro) inside `unsafe` blocks, and `global_asm` as a top-level item. The LLVM operand model is used directly — constraints and options map to LLVM inline asm.

##### Syntax

```kara
unsafe {
    // No operands — side effect only
    asm("cpsid i", options(nomem, nostack, preserves_flags));

    // Output operand — initializes an uninitialized binding
    let cycles: u32;
    asm(
        "mrs {0}, pmccntr_el0",
        out(reg) cycles,
        options(nomem, nostack),
    );

    // Named register, separate input/output
    let eax: u32;
    let ebx: u32;
    asm(
        "cpuid",
        inout("eax") 0u32 => eax,
        out("ebx") ebx,
        options(nostack, preserves_flags),
    );
}

// File-scope assembly — top-level item, no operands
global_asm("
    .section .vectors
    .word _stack_top
    .word _reset_handler
");
```

##### Operand Forms

| Form | Meaning |
|---|---|
| `in(constraint) expr` | Input — value read by asm; `expr` must be `Copy` |
| `out(constraint) binding` | Output — binding uninitialized before, initialized after |
| `out(constraint) _` | Clobber — the register's contents are clobbered, no Kāra binding receives the result |
| `lateout(constraint) binding` | Output that may *share* a register with an input (the input is fully consumed before the output is written) |
| `inout(constraint) lvalue` | Read-modify-write on a `mut` binding |
| `inout(constraint) expr => binding` | Separate input expression, output binding |
| `inlateout(constraint) ...` | `inout` whose output is `late` (may share a register with another input) |
| `const expr` | Compile-time constant substituted into the template; `expr` must be a `const` integer expression |
| `sym path` | Symbol reference — substitutes the linker name of a function or static into the template |

Constraints: `reg` (any general-purpose register), specialised register classes (`reg_byte`, `freg`, `vreg`, etc., per architecture), or named registers (`"eax"`, `"r0"`, etc.).

**`in(...)` requires `Copy`** — MMIO registers and CPU state are always integer types. Moves into asm operands are not supported; this keeps the ownership model simple at the unsafe boundary.

**`out(...)` operands** behave like `let` without an initializer — the binding is uninitialized before the `asm` and must be initialized by the asm block. The compiler treats the binding as definitively initialized after. The `out(...) _` form is a *clobber*: the asm writes to the register but no Kāra binding receives the value, telling the compiler the register's previous contents are gone.

**`inout(...)` operands** require a `mut` binding. The value is read in, the asm runs, and the result is written back.

**`lateout` / `inlateout`** mark outputs as written *after* all inputs are consumed, allowing the register allocator to reuse an input register for the output. Required when the asm sequence reads its inputs in the same instructions that produce its outputs (the common case for tight RMW idioms). Without `late`, the allocator must keep input and output registers disjoint.

**`const expr`** substitutes a compile-time integer into the template. The expression is evaluated in the const-evaluation context (no runtime captures, no effects); useful for embedding offsets, ABI-specified syscall numbers, and bit-positions without rebuilding the template per call site.

**`sym path`** substitutes the linker symbol of a Kāra function or static. The path resolves at compile time; the asm template sees a symbol reference (e.g., `bl {0}` where `{0}` is `sym my_handler`) that the linker resolves like any FFI call. Required for asm sequences that branch or call into Kāra-defined code.

##### Clobbers and ABI Clobbers

Two forms declare registers as clobbered without producing Kāra-visible output:

| Form | Meaning |
|---|---|
| `out("reg_name") _` | The named register's contents are clobbered. |
| `clobber_abi("C")`, `clobber_abi("system")`, `clobber_abi("C", "system")` | Every caller-saved register for the named ABI is clobbered. |

`clobber_abi` is the form to reach for when an asm block contains a function call (`bl`, `call`, `blr`) — the called function may have written every caller-saved register defined by its calling convention, and the compiler must assume so. Listing every caller-saved register by name is fragile across architectures; `clobber_abi("C")` names the convention symbolically and the compiler computes the per-target register set.

```kara
unsafe {
    asm(
        "bl {handler}",
        handler = sym irq_handler,
        clobber_abi("C"),                  // every caller-saved register may have changed
        options(nostack),
    );
}
```

##### Options

| Option | Meaning |
|---|---|
| `nomem` | Asm does not read or write memory visible to the compiler |
| `nostack` | Asm does not use the stack |
| `preserves_flags` | Asm does not modify processor condition flags |
| `att_syntax` | Use AT&T syntax (x86 only; default is Intel) |
| `pure` | Asm has no side effects; result depends only on inputs (enables CSE/DCE) |
| `readonly` | Like `nomem` but asm may read memory (not write) |
| `volatile` | Explicitly forces hardware side effects even if `nomem` is set |
| `raw` | Disables `{N}` / `{name}` template substitution — the asm string is passed to the assembler verbatim. Use when the assembly contains literal `{` / `}` characters (Intel-syntax memory operands, macro expansions). When `raw` is set, no operands may be supplied. |

##### Deferred Forms

Two further inline-asm shapes are recognised by the design but not part of the first systems release:

- **`label` blocks (asm goto).** A future operand form `label(name) { ... block ... }` would let an asm sequence branch into a Kāra block, with the branch destinations enumerated at the asm site. The integration cost is non-trivial — every asm goto target becomes an entry edge into the surrounding control-flow graph, which must be reconciled with effect inference, ownership flow, and the borrow checker's drop-tracking. Until then, embedded code that needs jump-out-of-asm uses a `*const fn()` operand and `bl {0}` plus an explicit return path.
- **`#[naked]` functions with `naked_asm`.** A function attribute that suppresses the prologue/epilogue and requires the body to be a single `naked_asm("...")` call. Used for ISR entry points and trampolines that hand-roll the calling convention. Until then, ISR shims are written as `extern "interrupt"` functions whose body is a single `unsafe { asm(...) }` block — the calling-convention name carries the prologue/epilogue contract instead of opting out of it. The design needs a coherent answer for how `#[naked]` interacts with the effect system, the unsafe-fn rule, and function-pointer types.

##### Effect Integration

`asm` blocks integrate with the effect system conservatively:

- **Default (no `nomem`/`nostack`):** implies `reads(Hardware) + writes(Hardware)` — the enclosing function must declare these effects.
- **`options(nomem, nostack)`:** no hardware effects implied — pure register manipulation (e.g., reading a cycle counter, setting CPU flags).
- **`options(pure, nomem, nostack)`:** no side effects at all — still requires `unsafe`, but the enclosing function needs no hardware effect declaration.
- **`options(volatile)`:** explicitly asserts hardware side effects even when `nomem` is present — use for instructions like `hlt`, `wfi`, `fence` that have architectural side effects beyond memory.

```kara
// This function needs writes(Hardware) — asm has memory side effects
fn disable_mmu() with writes(Hardware) {
    unsafe {
        asm("mcr p15, 0, r0, c1, c0, 0", options(nostack, preserves_flags));
    }
}

// No hardware effect needed — pure register read
fn read_cycles() -> u32 {
    let cycles: u32;
    unsafe {
        asm(
            "mrs {0}, pmccntr_el0",
            out(reg) cycles,
            options(nomem, nostack),
        );
    }
    cycles
}
```

##### `global_asm`

`global_asm` emits raw assembly at file scope, outside any function. Used for:
- Interrupt vector tables
- Bootstrap / `_start` entry point
- Architecture-specific initialization before `main` runs

```kara
global_asm("
    .global _start
    .section .text
_start:
    ldr sp, =_stack_top
    bl  kara_main
");
```

`global_asm` takes no operands — it is a raw string passed through to the assembler. It is not inside `unsafe` (there is no ownership context at file scope), but it has unrestricted hardware effects by definition.

**Effect integration:** Symbols defined in `global_asm` are always accessed from Kāra code through `extern` declarations. The existing FFI effect annotation mechanism covers them — the programmer declares effects on the `extern` signature, and the compiler trusts those declarations at the FFI boundary (see [design.md §15](design.md#effects-of-extern-functions)). No separate effect annotation on `global_asm` itself is needed.

#### Interrupt Handler ABI (`#[interrupt]`)

Interrupt service routines (ISRs) are called by hardware, not software. They
use a different calling convention (all registers saved/restored, `iret`/`eret`
return), cannot return values, and must be placed at a specific location in the
interrupt vector table.

##### `#[interrupt]` attribute

```kara
#[interrupt(TIMER1)]
fn timer1_isr() {
    TIMER1_FLAG.store(true, Release);
}
```

The argument is an identifier from a platform-defined `Interrupt` enum (defined
in the target's platform package, not the language). The compiler:

- Forces the interrupt calling convention (`extern "interrupt"` under the hood)
- Emits the correct vector table placement directive
- Forbids the function from being called directly from normal Kāra code (compile error)
- Implicitly applies the `isr` profile restrictions (no heap allocation, no
  panics) to the function body
- Infers the function's effects as `reads(Hardware) writes(Hardware)` unless
  more specific resource annotations are present

The `#[interrupt]` attribute is sugar for `extern "interrupt"` + linker placement.
Users write `#[interrupt]`; the raw `extern "interrupt"` form is available for
advanced FFI use only.

##### Vector table placement

Platform packages define the `Interrupt` enum mapping vector names to positions:

```kara
// In a platform package (e.g., stm32f4)
enum Interrupt {
    TIMER1 = 28,
    UART0  = 37,
    // ...
}
```

The compiler uses the discriminant to place the ISR at the correct vector table
offset. The language does not hard-code MCU-specific vector names — all
MCU-specific knowledge lives in platform packages.

##### Critical sections

Temporarily disabling interrupts uses an RAII guard:

```kara
fn update_shared_state() with writes(Hardware) {
    let _guard = critical_section.acquire();  // disables interrupts
    // ... access shared state ...
    // _guard drop re-enables interrupts
}
```

`critical_section.acquire()` returns a `CriticalSectionGuard` that re-enables
interrupts on drop. The guard type is `#[must_use]` — accidentally discarding it
immediately re-enables interrupts and is a compile warning.

##### `extern "interrupt"` and `#[interrupt]`

`extern "interrupt"` is the low-level ABI form. `#[interrupt(NAME)]` is sugar
that generates an `extern "interrupt"` function and emits the correct vector
table placement. Users should use `#[interrupt]`; `extern "interrupt"` is
available for cases where the attribute abstraction is insufficient — for
example, when the interrupt vector is not listed in the platform package's
`Interrupt` enum (custom hardware, bootloader trampolines), or when vector
table placement is handled by an external linker script rather than the
compiler. Like every other foreign-import declaration, the raw form lives
inside an `unsafe extern "interrupt" { ... }` block (see
[`unsafe extern` blocks](design.md#unsafe-extern-blocks)).
`#[interrupt]` itself, applied to a Kāra function with a body, is a
*definition* and stays plain — the compiler-generated lowering produces an
appropriately-placed function whose ABI and section the compiler can verify
from the platform package's `Interrupt` enum.

### Codegen hints and linker control

#### Codegen Hint Attributes (`#[inline]`, `#[cold]`)

Four attributes hint at code-generation choices the optimizer would otherwise make on its own. None of them changes program semantics — every program compiles to functionally identical output regardless of which subset is present. They influence performance characteristics only: code placement, call-site lowering, and inlining decisions. The hints are recorded on the function symbol and lowered to the corresponding LLVM function attribute (`alwaysinline`, `noinline`, `inlinehint`, `cold`).

| Attribute | Effect on the optimizer |
|---|---|
| `#[inline]` | Suggest inlining at call sites — non-binding. The compiler may inline or not, weighting the hint with its own cost model. |
| `#[inline(always)]` | Strong directive to inline at every call site. The compiler must inline whenever inlining is technically possible. |
| `#[inline(never)]` | Strong directive to **not** inline. The compiler keeps the function as a real call frame at every site. |
| `#[cold]` | Hint that the function is rarely executed. The compiler may move the function to a separate code section, optimize call sites for the fall-through path (assuming the call doesn't happen), and skip aggressive inlining even if `#[inline]` is present. Common shape: a slow-path error-formatting helper or panic emitter. |

**Hints, not contracts.** All four are *reported behavior*, not *guaranteed semantics* ([design.md §2](design.md#2-specification-layers)). The compiler chooses the lowering it judges correct for the target and profile; the attributes shift the cost model, they do not control it. A function with `#[inline]` may not be inlined if the compiler decides it would hurt code size; a function with `#[inline(always)]` is not inlined at sites where inlining is **technically impossible** (see below); a function with `#[inline(never)]` may still be inlined by the linker under cross-package LTO when LTO judges it profitable. The Kāra compiler honors all four hints faithfully — `#[inline(always)]` inlines everywhere it can, `#[inline(never)]` blocks Kāra-pass inlining categorically — but the linker and the LLVM backend remain free to override at lower layers, exactly as for Rust. Programs that need a hard guarantee (e.g., to keep a frame in stack traces) cannot get it from these attributes alone; the language offers no stronger primitive.

**When `#[inline(always)]` cannot inline.** Inlining is technically impossible for:

- **Recursive call sites** (a function inlined into itself either bottoms out via a base case or expands forever; the compiler picks the safe option and leaves the recursive call out of line). A non-recursive call to a `#[inline(always)]` recursive function is still inlined.
- **Function-pointer use sites** (`let f: fn() = my_fn; f()` — the indirection blocks inlining at the call). The function may still be inlined at *direct* call sites.
- **Cross-package calls without LTO** when the called function lives in a precompiled dependency that does not ship its IR. Kāra's default build pipeline ships IR for `pub` functions to support cross-package inlining; this restriction primarily matters for prebuilt vendor libraries.
- **Functions that contain blocked-from-inlining constructs** (large `asm` blocks where register allocation would interact poorly across the inline boundary; `#[interrupt]` ISR handlers — see [Interrupt Handler ABI](#interrupt-handler-abi-interrupt)).

In each blocked case the compiler emits no diagnostic — silently leaving the call out of line is the right behavior. Programs that depend on inlining for correctness are misusing the attribute; the language does not validate that.

**Conflict rules.** The four attributes split into two axes — inlining (`inline` / `inline(always)` / `inline(never)`) and hot/cold placement (`cold`). Within the inlining axis, only one attribute may appear per function. Violations:

- `#[inline]` and `#[inline(always)]` on the same function: `error[E_INLINE_HINT_CONFLICT]: \`#[inline]\` and \`#[inline(always)]\` are mutually exclusive`.
- `#[inline]` and `#[inline(never)]` on the same function: `error[E_INLINE_HINT_CONFLICT]: \`#[inline]\` and \`#[inline(never)]\` are mutually exclusive`.
- `#[inline(always)]` and `#[inline(never)]` on the same function: `error[E_INLINE_HINT_CONFLICT]: \`#[inline(always)]\` and \`#[inline(never)]\` are mutually exclusive`.
- `#[cold]` and `#[inline(always)]` on the same function: `error[E_COLD_INLINE_ALWAYS_CONFLICT]: \`#[cold]\` and \`#[inline(always)]\` express opposite intents`. (`#[cold]` + `#[inline]` and `#[cold]` + `#[inline(never)]` are both legal; the latter is the canonical "definitely cold, definitely keep out of line" combination.)

Repeating the same attribute on one function is `error[E_DUPLICATE_ATTRIBUTE]` (covered by the general attribute checker).

**Where they may appear.**

| Position | Allowed |
|---|---|
| Free functions (`fn`) | Yes |
| Methods inside `impl` blocks | Yes |
| Methods inside `impl Trait for T` blocks | Yes |
| Trait method *declarations* | Yes — the hint applies to every impl unless that impl carries its own override (last-writer-wins, parallels `#[track_caller]` propagation). |
| Closures and lambdas | No — closures are inlined by the compiler when they cross a closure-typed parameter (`Fn`-trait dispatch is monomorphized inline by default). The attributes are rejected on closure expressions with `error[E_CODEGEN_HINT_ON_CLOSURE]: codegen hints attach to named functions; closures are inlined by the dispatch lowering`. |
| `extern "C" fn` definitions | Yes — applies to the Kāra-side body the same way as a normal function. |
| Foreign-function declarations inside `unsafe extern { ... }` | No — there is no Kāra-side body to inline; the compiler cannot reach inside a foreign symbol. Rejected with `error[E_CODEGEN_HINT_ON_EXTERN_DECL]`. |
| `impl Drop for T` destructors | Yes — Kāra-pass inlining of destructors is governed by the same hints; no special case. |
| Entire `impl` block (apply to every method) | No. The attribute attaches per-method. (A block-level shorthand may come later.) |

**No semantic effect on the effect system.** Inlining and cold placement are purely codegen concerns; the inlined body's effect set has already been incorporated into the caller's inferred effects through normal effect inference, and the hint does not change the inferred set. A `#[cold] fn report_error(e: ref Error) with writes(Log) { ... }` inlined or not still contributes `writes(Log)` to its caller's effects.

**No interaction with `#[track_caller]`.** A `#[track_caller]` function carries a hidden caller-location parameter; this is orthogonal to inlining decisions. A `#[track_caller] #[inline]` function may be inlined; when inlined, the caller-location parameter is folded away (the receiver function's site replaces the hidden parameter). When not inlined, the caller-location is passed normally. Both attributes coexist without diagnostic.

**Diagnostic for misapplication on bodies whose inlining never happens.** When a `#[inline(always)]` function's only call sites are dynamic (every call is through a `fn`-pointer), the compiler emits no diagnostic — the silent-leave-out-of-line rule applies. Future tooling (`karac build --perf-report`) will surface "`#[inline(always)]` had no inlining sites" as a hint, but the build itself never fails on it.

#### Linker Control Attributes

Three attributes control how symbols appear in the linker's view of the binary.
Two of them — the ones that pick a symbol name or place a symbol at a
programmer-specified linker location — carry obligations the type system
cannot verify (symbol-name uniqueness, section validity, layout/aliasing
constraints), so they must be written in the **`#[unsafe(...)]` wrapping
form**. The third (`#[used]`) only suppresses dead-code elimination on its own
and stays plain.

The `let` and `let mut` bindings below are module-level bindings in the sense
of [Module-Level Mutable State](#module-level-mutable-state): their initializers are
compile-time constant expressions (array literals and `[0; N]` repeat forms),
so they satisfy the compile-time initializer rule without special cases.

| Attribute | Purpose | Wrapping form |
|---|---|---|
| `#[unsafe(link_section("name"))]` | Place the symbol in a named linker section | `#[unsafe(...)]` required |
| `#[unsafe(no_mangle)]` | Use the Kāra identifier as-is; disable name mangling | `#[unsafe(...)]` required |
| `#[used]` | Prevent dead-code elimination even if no Kāra code references the symbol | plain (no wrap) |

The bare forms `#[link_section(...)]` and `#[no_mangle]` are rejected at parse
time with a focused diagnostic suggesting the `#[unsafe(...)]` wrap. The wrap
is a visual trust-boundary marker — a reviewer scanning for everything that
can violate symbol uniqueness or layout assumptions can grep for `#[unsafe(`.

```kara
// Interrupt vector table at a fixed address (ARM Cortex-M)
#[unsafe(link_section(".vectors"))]
#[used]
let INTERRUPT_VECTORS: Array[*const fn(), 256] = [...];

// Stable export name for bootloader / OS loader
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() { ... }

// DMA buffer in fast SRAM (cache-line aligned for DMA)
#[unsafe(link_section(".dtcmram"))]
#[repr(align(32))]
let mut dma_rx_buffer: Array[u8, 512] = [0; 512];
```

**Definitions vs. imports.** The `extern "C" fn name(...) { ... }` form
above (with a body) is a Kāra function exposed under a foreign ABI — it is a
*definition*, not a *foreign import*. Definitions keep their syntactic shape;
their soundness-affecting attributes (`#[unsafe(no_mangle)]`,
`#[unsafe(link_section(...))]`) carry the trust-boundary load. Foreign
imports — declarations of foreign symbols with no Kāra-side body — must live
inside `unsafe extern "ABI" { ... }` blocks instead; see [design.md §15](design.md#extern-c). When the
defined function panics, the process aborts, as on every panic.

##### `#[unsafe(no_mangle)]` vs ABI

`#[unsafe(no_mangle)]` controls only the *symbol name* — it does not imply
`extern "C"` ABI. A function can have `#[unsafe(no_mangle)]` with Kāra
calling convention (linker can find it by name, but callers must know the
ABI) or with `extern "C"` for C interop. Both annotations are needed when a
bootloader expects a C-ABI entry point with a stable name.

Kāra mangles names by default (to support generics and avoid cross-module
collisions). `#[unsafe(no_mangle)]` disables this for the annotated item.
The wrapping is mandatory because the programmer is asserting the chosen
symbol name does not collide with any foreign symbol in the final binary —
an obligation the compiler cannot check across the FFI boundary.

##### `#[used]` and dead-code elimination

The compiler performs dead-code elimination on unreferenced globals. `#[used]`
marks a symbol as externally referenced (by hardware, a linker script, or a
foreign loader) and suppresses elimination. Required for interrupt vector tables,
linker-placed constants, and any symbol that hardware will access by address.

`#[used]` stays plain (no `#[unsafe(...)]` wrap) because on its own it has no
soundness impact — it only suppresses DCE. A `#[used]` symbol that is mangled
and not exported by name remains unreachable from outside the binary; the
worst case of an over-applied `#[used]` is dead bytes. The attributes that
*do* expose the symbol (`#[unsafe(no_mangle)]`, `#[unsafe(link_section)]`)
carry their own wrap; layering `#[unsafe(used)]` on top would be ceremony
without information.

### Calling conventions

#### C Calling Convention Variants

The general form is `extern "<abi>"`. Kāra supports the following ABIs:

| ABI string | Used for |
|---|---|
| `"C"` | Standard C ABI (default for FFI) |
| `"stdcall"` | Win32 API (Windows kernel/driver) |
| `"fastcall"` | x86 register-passing convention |
| `"win64"` | Windows x64 ABI |
| `"sysv64"` | System V AMD64 ABI (Linux/macOS x64) |
| `"interrupt"` | ISR entry points; used by `#[interrupt]` internally |

v1 has only `"C"`. Until this track lands, the others are
reserved names — specifying them produces a "not yet supported" compile error
rather than silently falling back to `"C"`.

### FFI unions and exported C ABI

#### FFI Unions: `union Foo { ... }`

C unions — the type whose storage is shared by every named field rather than partitioned among them — exist in Kāra solely as an FFI shape. They are spelled with the `union` keyword as a top-level item, take `#[repr(C)]` for C-ABI compatibility, and require `unsafe { }` at every field *read*.

```kara
#[repr(C)]
union FloatBits {
    f:    f32,
    bits: u32,
}

let x = FloatBits { f: 3.14 };          // construction is safe — exactly one field is named

let raw: u32 = unsafe { x.bits };        // read is unsafe — reinterprets f32 storage as u32

x.bits = 0x40490FDB;                     // write of a Pod field is safe — no interpretation
```

**Why a separate kind from `enum`.** Kāra's tagged `enum` already covers the "one of several shapes" use case at the language level — it carries a discriminant, the typechecker proves which variant is live, and the borrow checker tracks per-variant payloads. A C union is the *opposite*: the variant is *not* tracked; the bytes are reinterpreted on every access. The two should not share a syntax. `union` is the FFI-only shape for binding C headers that expose untagged unions (`union sigval`, `union epoll_data`, x86 register-context structs, audio/video codec descriptors), reading hardware registers whose meaning depends on bits elsewhere, and the niche bit-twiddling cases where `transmute` would be the alternative.

**Declaration form.**

```
UNION_DECL = ATTRS "union" IDENT "{" UNION_FIELDS "}"
UNION_FIELDS = UNION_FIELD ( "," UNION_FIELD )* ","?
UNION_FIELD  = VIS? IDENT ":" TYPE
```

A `union` must have at least one field. Empty unions are rejected at parse with `error[E_EMPTY_UNION]`. Tuple-style unions (`union Foo(i32, u32)`) are not provided — every field must be named so reads reference the field by name rather than by index.

**Required `#[repr]`.** Every `union` must carry `#[repr(C)]` (or `#[repr(C, packed)]`). A bare `union Foo { ... }` without a `#[repr]` declaration is rejected at typecheck with `error[E_UNION_REQUIRES_REPR]: \`union\` requires #[repr(C)] — without an ABI annotation a union has no defined layout`. The Rust-style "default repr" is deliberately omitted: unions exist only for FFI, and committing to a layout without naming it is exactly the footgun this whole feature is meant to prevent.

**Layout.** `size_of` is the max of the field sizes; `align_of` is the max of the field alignments; size is rounded up to a multiple of the alignment. Padding bytes between the union's start and its end (when the largest field is shorter than the rounded-up size) are uninitialized — reading them is unsafe and reinterprets unspecified memory.

**Field constraints.** Every field type must be `Copy` (see [design.md §9](design.md#copy-clone-and-drop)). Non-`Copy` field types are rejected at typecheck with `error[E_UNION_FIELD_NOT_COPY]`. The rule covers two soundness footguns at once:

1. **Drop ordering.** A union with a `Drop` field cannot be dropped soundly — the language doesn't know which variant is live, so it cannot decide whose `drop` to run. Forbidding `Drop` fields removes the question.
2. **Write semantics.** Writing a `Copy` field to a union is a pure bit-overwrite — it cannot drop the previously-stored variant (because the previous bytes were `Copy` too, with no destructor). The rule keeps writes safe.

The `Copy` constraint is checked at union-declaration time, not at use site, so an `unsafe` workaround does not exist. Programs that need a non-POD payload reach for `enum` (the right tool for tagged variants) or wrap the field in `MaybeUninit[T]` and accept the unsafe-init / unsafe-deinit dance per-variant.

**Construction.** A `union` is constructed with a struct-literal-shaped expression naming exactly one field:

```kara
let bits = FloatBits { f: 3.14 };           // OK
let bits = FloatBits { bits: 0x40490FDB };  // OK
let bits = FloatBits { f: 1.0, bits: 1 };   // ERROR — exactly one field per construction
let bits = FloatBits {};                    // ERROR — exactly one field per construction
```

`error[E_UNION_LITERAL_REQUIRES_ONE_FIELD]` covers both the multi-field and zero-field cases. The single-field rule is what makes construction *safe*: the bytes are written with a known interpretation, so no reinterpretation is happening at the construction site.

**Field access semantics.**

| Operation | Safety |
|---|---|
| Read a field — `let v = u.field` | `unsafe { }` required |
| Take a reference to a field — `ref u.field` / `mut ref u.field` | `unsafe { }` required |
| Take a raw pointer — `ptr.const(u.field)` / `ptr.mut(u.field)` | safe (per [design.md §15](design.md#construction-ptrconst-and-ptrmut)) |
| Assign to a field — `u.field = v` | safe (`Pod` field, no drop) |
| Pattern-match on a field — `let Foo { field } = u` | `unsafe { }` required |

The asymmetry between assignment-safe and read-unsafe is the same rule as Rust's: writing a `Copy` field commits to a fresh interpretation; reading any field reinterprets bytes whose interpretation the compiler cannot verify.

**Forbidden interactions.**

- **No generics.** `union Foo[T] { ... }` is rejected at parse. Layout depends on the largest field, and a polymorphic union would have to compute layouts per monomorphisation; that is feasible but buys nothing the FFI use case requires (every C union has a fixed shape). Lifting this restriction later is additive.
- **No `Drop` impl.** `impl Drop for Foo` on a union type is rejected at typecheck (the field-`Copy` rule forbids fields that need dropping; a user-supplied `Drop` impl would be the only way to introduce one and is therefore forbidden too).
- **No automatic derives.** `#[derive(...)]` on a union is rejected at parse. None of the builtin derives have well-defined semantics on a union (`Eq` / `Hash` / `Debug` would have to pick a field, `Clone` is provided implicitly because every field is `Copy`, `PartialEq` over raw bytes is a reasonable but surprising default we do not commit to). Custom `impl` blocks for user-defined traits are permitted.
- **No `#[non_exhaustive]`.** A `#[non_exhaustive]` union would have a hidden field, making `size_of` non-public — incoherent with the FFI use case.

**Effect integration.** Union field reads are `unsafe { }`-only but contribute no effects to the enclosing function's inferred effect set on their own. Reading reinterpreted bytes is a *language-level* unsafe operation; if the read triggers a hardware effect (an MMIO register read, a volatile read), that is captured by the surrounding `unsafe` block via the existing `volatile_read` / `reads(Hardware)` machinery — not by the union read itself.

**Cross-`extern` use.** Unions appear in two FFI shapes:

1. **As a field of an `extern "C"` struct or as a function argument/return.** No special handling — the C ABI rules in [design.md §15](design.md#repr-abi-layout) cover layout, and the `#[repr(C)]` requirement on the union is what makes the ABI shape defined.
2. **Imported from a C header.** The Kāra `unsafe extern "C" { }` block does not contain `union` declarations directly — `union` is a *type definition* and lives outside the extern block. The pattern is to declare the `#[repr(C)] union Foo { ... }` as a top-level type and reference it from the `unsafe extern { }` block's signatures, mirroring the pattern for `#[repr(C)] struct`.

```kara
#[repr(C)]
union epoll_data {
    ptr: *mut u8,
    fd:  i32,
    u32: u32,
    u64: u64,
}

#[repr(C)]
struct epoll_event {
    events: u32,
    data:   epoll_data,
}

unsafe extern "C" {
    fn epoll_wait(epfd: i32, events: *mut epoll_event, maxevents: i32, timeout: i32)
        -> i32 with reads(Network) writes(Network);
}
```

**`MaybeUninit[T]` is the better answer when one exists.** Unions exist for binding *given* C shapes. New Kāra code that needs "I have storage, I will fill it later" should reach for [`MaybeUninit[T]`](design.md#uninitialised-memory-maybeuninitt) — it is the typed wrapper for the same problem and gives the compiler more information.

#### Exported C ABI

v1's `extern "C"` ([design.md §15](design.md#extern-c)) is the **consume** direction — Kāra calling foreign code. This section specifies the **produce** direction: building a Kāra program as a linkable library (`.a` / `.so` / `.dylib`) with a stable C surface that a foreign C, C++, or C-ABI-wrapped Rust program links against and calls into. It is the "*write the parallel data kernel in Kāra, keep everything else*" adoption path — Kāra as a component you add, not a rewrite you commit to.

The governing constraint is **honesty about the ABI**: C is the only durable cross-language contract, so the exported surface is a C surface, and only the shapes C can name transparently cross transparently. Everything else crosses as an opaque handle owned by the Kāra runtime. This keeps the README from writing a check the ABI cannot cash ("call Rust crates cleanly" is un-cashable as written — Rust has no stable ABI; the achievable promise is "call C, and call Rust crates wrapped to expose a C ABI").

##### The exported surface — discovery

The public C surface of a Kāra library is **every `pub extern "C" fn` definition** — a function with a *body* (a definition, not an `unsafe extern { }` import), a C ABI, and `pub` visibility:

```kara
pub extern "C" fn saxpy(n: i64, a: f32, x: *const f32, y: *mut f32) with writes(Buffer) {
    // hot parallel kernel — the reason C is calling into Kāra
    ...
}
```

Both markers are load-bearing, and the discovery rule reuses machinery that already exists:

- **`pub`** puts the function in the external API (the same `pub`-crosses-the-package-boundary tier that governs effect declarations, [design.md §12](design.md#12-effects)) and gives the symbol external linkage, so it survives dead-code elimination.
- **`extern "C"`** selects the C calling convention. **`#[unsafe(no_mangle)]`** pins the bare C symbol ([`#[unsafe(no_mangle)]` vs ABI](#unsafeno_mangle-vs-abi)), so library authors should write it. Discovery keys on `pub extern "C"`, independent of the attribute.

Discovery is **language-driven, not manifest-driven**: the surface is the set of tagged `pub fn`s, not a list in `kara.toml`. This keeps a single source of truth: the export set is visible at the definition site. The manifest `[lib]` table (below) carries *artifact metadata* (name, default kind), **not** an export list — there is no second place to keep in sync. `main` is never an export; a library artifact has no entry point.

##### Type mapping — what crosses, and how

The honest answer: **primitives, `#[repr(C)]` structs, and opaque handles cross transparently; everything else crosses as a Kāra-owned opaque pointer with accessor and destructor functions** — the boxing convention, not a transparent layout. An exported `pub extern "C" fn` may only name **boundary-legal** types in its signature; a non-boundary-legal type in an exported signature is rejected at typecheck with `error[E_EXPORT_TYPE_NOT_C_ABI]` naming the offending type and pointing at the handle convention.

**Transparent set (crosses by value / by pointer, appears verbatim in the emitted header):**

| Kāra type | C type | Notes |
|---|---|---|
| `i8`…`i64`, `u8`…`u64` | `int8_t`…`int64_t`, `uint8_t`…`uint64_t` | `<stdint.h>` fixed-width |
| `f32`, `f64` | `float`, `double` | |
| `bool` | `uint8_t` | `0`/`1`; C `_Bool` layout is not guaranteed portable across compilers |
| `()` (unit return) | `void` | return position only |
| `usize`, `isize` | `size_t`, `ptrdiff_t` | the FFI-only size types ([design.md §5](design.md#numeric-semantics); idiomatic Kāra uses `i64`) |
| `*const T`, `*mut T` | `const T*`, `T*` | `T` must itself be boundary-legal |
| `#[repr(C)] struct S { … }` | `struct S { … }` | all fields boundary-legal; emitted into the header. **Only `#[repr(C)]`** — a default-layout `struct` has no stable layout and crosses as a handle |
| all-unit `#[repr(C)] enum E { … }` | `typedef int64_t E;` + named constants | value is the discriminant; `int64_t` (not a bare C `enum`, which is `int`/4 B and mismatches the 8-B tag) with an anonymous `enum { E_Variant = tag, … }` for readability |
| scalar-payload `#[repr(C)] enum E { … }` (return) | `E*` (boxed) + `karac_free_<fn>` | unit + single-scalar variants → a heap-boxed pointer to a faithful C tagged union `{ int64_t tag; union { … } payload; } E;` matching the `{ i64 tag, i64 w0 }` layout; scalar payloads read through per-variant union members. Return-only (params never box). Multi-scalar / aggregate-payload variants stay rejected |
| declared opaque foreign type `Foo` | `Foo*` | already-`extern`-declared opaque handles pass through |

**Opaque-handle set (crosses as `KaraHandle`-typed opaque pointer, never a transparent layout):** `Vec[T]`, `String`, default-layout `struct`, non-`#[repr(C)]` or multi-scalar / aggregate-payload `enum` (an all-unit `#[repr(C)]` enum is transparent, and a scalar-payload `#[repr(C)]` enum boxes to a tagged union — see the two rows above), `Option[T]`, `Result[T, E]`, `shared struct`/`shared enum`, and any RC-carrying aggregate. For each exported handle type the emitter produces a distinct opaque typedef (`typedef struct KaraVec_i32 KaraVec_i32;` — an incomplete type the C side only ever holds by pointer) plus the accessor/destructor exports the library author wrote. There is **no** transparent field access into these from C — the layout is not part of the ABI and is free to change between compiler versions, exactly as with C opaque handles ([design.md §15](design.md#opaque-foreign-types), inverted).

**Boxing convention.** An exported fn that returns a non-transparent value *boxes* it: the value is heap-allocated on the Kāra side (owned by the runtime), and an opaque pointer is returned. The C caller **must not `free()`** the pointer — the box may own RC children or heap sub-allocations whose drop glue only Kāra knows. It returns the value to Kāra for destruction through a destructor export (the `karac_free_*` / `Drop`-export path).

**The move-out primitive — `forget`.** The export boundary is a move *out* of Kāra's ownership universe. The primitive is `forget(value)` ([design.md §15](design.md#intrinsics)): it consumes `value` and suppresses the destructor, so Kāra allocates, `forget`s, and hands a raw pointer to C, which later returns it to a Kāra `free_*` export. Soundness is by construction — `forget`'s owned parameter makes the ownership checker *and* the drop oracle both treat the call as a consume (no scheduled drop), which the codegen suppression matches. Like Rust's `mem::forget`, it is *safe* (leaking is not UB).

**The full round-trip handoff has two paths — a manual path and an auto path.**

- **Manual (Path A)** — allocate a raw buffer, fill it via the raw-pointer instance methods `.offset` / `.read` / `.write` ([design.md §15](design.md#raw-pointers)), hand the pointer to C, and free it via a Kāra `free_*` export. No per-type drop synthesis; the sound baseline.
- **Auto (Path B)** — a `pub extern "C" fn` returning a boxable aggregate is auto-boxed for the C ABI with zero boilerplate. Kāra returns a `{data,len,cap}` value in registers, which does not match the SysV struct-return ABI (a 24-byte struct → sret); so the export **heap-boxes the value and returns an opaque pointer** — a scalar return the C ABI passes in a register, sidestepping sret entirely. The C side reads the `{data,len,cap}` fields transparently through the emitted struct (`typedef struct { E* data; int64_t len; int64_t cap; } KaraVec_<E>;`) and frees the handle via the auto-emitted `karac_free_<name>` (which frees the owned buffer + the box). An internal Kāra call to such an export is rejected at compile time (its lowered return is a pointer, not a Vec) rather than miscompiled — a boxed export is a C-facing surface. Covers `Vec[scalar]`, `String`, and **one level of aggregate nesting** — `Vec[String]` and `Vec[Vec[scalar]]`, which cross as nested transparent structs (`{ KaraString* data; … }`, `{ KaraVec_int64_t* data; … }`) whose destructor recursively frees each element's buffer before the outer. Deeper nesting / `enum` / user-struct returns are **not** boxed — they cross via a raw pointer to a Kāra-owned box (the manual Path-A pattern). **The ABI-honesty gate** (`validate_exports`) rejects a library-build export whose return or param is non-transparent *and* non-boxable, so a produced `.a`/`.so`/`.h` never claims an opaque `KaraHandle` while the ABI actually returns/expects a multi-register aggregate (a silent miscompile).

This section fixes the *convention* (opaque + Kāra-owned destructor, never C `free`), the *move primitive* (`forget`), and both round-trip paths (manual pointer methods + auto-boxing).

**`String` convenience (still opaque underneath).** Because "hand C a string" is common, an exported `String` handle additionally supports a borrowing accessor — `karac_string_bytes(handle, const uint8_t** out_ptr, size_t* out_len)` — that lends the UTF-8 bytes without transferring ownership; the bytes are valid until the handle is destroyed. This is accessor sugar over the opaque handle, not a transparent layout, and does not weaken the boxing convention.

##### The effect contract for an effect-blind caller

A C caller has no effect system, but — unlike the consume direction's *trust-not-verify* rule — an **exported fn's effects are KNOWN**: they were checked against the fn's body. So the contract can state them precisely, and must not copy the extern-*import* default (`{blocks}`) onto exports. Three treatments:

- **Documentation.** The emitted header annotates each prototype with its checked effect set as a doc comment (`/** @effects reads(Db), writes(Buffer), blocks */`). Informational for the C reader — C cannot enforce it — but it is a *precise* record, not a guess.
- **`panics` — operational.** A C caller cannot catch a Kāra panic. A panic in an exported fn aborts the process, as every panic does; it never unwinds into C frames.
- **`suspends` — operational.** A C caller drives no Kāra scheduler, so an exported fn that `suspends` (network-boundary / async) cannot run on a bare foreign thread. The export boundary is **synchronous only**: a `pub extern "C" fn` whose effect set contains `suspends` is rejected with `error[E_EXPORT_SUSPENDS_UNSUPPORTED]`, pointing at the pattern of exposing a *blocking* wrapper that owns a runtime-scoped task internally. Async export across the C boundary may come later.

##### Runtime-init contract and self-containment

A produced Kāra library is **not self-contained**: it references `libkarac_runtime.a` symbols (allocator, RC, channels, scheduler) exactly as an executable does. The artifact links only when the runtime archive is bundled alongside it — and a consumer must link **with no karac toolchain present**. The emitted header states the required link line (`-lfoo -lkarac_runtime -lpthread`).

Because the runtime owns allocation and (optionally) a task pool, a C host initializes it once before the first call and shuts it down at teardown. The emitter always surfaces two idempotent lifecycle exports into the header:

```c
void karac_runtime_init(void);      /* idempotent; safe to call once at host startup */
void karac_runtime_shutdown(void);  /* drains runtime-owned tasks; call at host teardown */
```

For a pure-compute kernel with no `par {}` / `TaskGroup`, `init` is a no-op beyond arming the allocator, but it is still required so the alloc/RC surface is live before any exported call boxes a value.

**A Rust host must link the `cdylib`, not the `staticlib`.** The runtime is itself a Rust crate that bundles `std`, so the thick `.a` carries std symbols (`rust_eh_personality`, allocator shims, the panic runtime). A *C* host has no `std` of its own and links the archive cleanly — but a *Rust* host's own `std` provides those same symbols, and static-linking both is a duplicate-symbol error. The `.so`/`.dylib` encapsulates the runtime's internal symbols (only the exported `pub extern "C"` surface + the lifecycle entries are dynamic), so a Rust host links it without collision. The rule: **C hosts, either artifact; Rust hosts, the shared library.**

##### Build mode and header emission (surface)

Two `karac build` modes route the exported surface through the existing native link path with external linkage:

- `karac build --crate-type staticlib <file.kara>` → `.a`
- `karac build --crate-type cdylib <file.kara>` → `.so` (Linux) / `.dylib` (macOS)

The output artifact is written to an explicit `-o <path>` or, failing that, to `dist/` under the manifest `[lib] name` (or the source stem) — a library build must **not** clobber a stray executable in the working directory the way an ordinary `karac build` writes to CWD. On macOS a `cdylib` is emitted with `-install_name @rpath/lib<name>.dylib` so the consumer resolves it via rpath rather than an absolute build-machine path.

Alongside the artifact, the compiler emits a C header (`lib<name>.h` by default; `--header <path>` / `--no-header` override) — the cbindgen analogue that makes the surface *ergonomic* rather than merely possible. The header contains, in order: an include guard, `#include <stdint.h>` / `<stddef.h>`, a `#ifdef __cplusplus extern "C" {` guard, the two runtime-lifecycle prototypes, the opaque handle typedefs, the `#[repr(C)]` struct definitions, and one `@effects`-annotated prototype per exported fn. An optional manifest table names the artifact:

```toml
[lib]
name = "kernels"          # → libkernels.a / libkernels.so / libkernels.h
crate-type = "staticlib"  # default kind when --crate-type is omitted
```

The `[lib]` table is the symmetric addition to the existing `[link]` table (which links *foreign* libraries *into* a Kāra build); `[lib]` describes the Kāra library *this* build *produces*. It carries only artifact metadata — never the export list, which stays language-driven per *discovery* above.

### Portable SIMD — `Vector[T, N]`

**Decision:** `Vector[T: Numeric, const N: i64]` is the single portable-SIMD type — the same type used for CPU SIMD, GPU vectors, and any future accelerator. Element-wise arithmetic, dot product, cross product (`N == 3`), and shuffle / lane-access operations are part of the type's surface. Codegen lowers to the widest available primitive on the target; when no native vector unit covers `N` lanes, the compiler emits a scalar fallback. The user's source program is identical across targets — performance, not correctness, is what varies.

**Integer lane arithmetic WRAPS.** `+`, `-`, `*`, `/` and `%` on an integer-lane `Vector[T, N]` truncate to the lane width in two's complement. `Vector[u8, 4]` lanes of `200` added to themselves give `144`, not a trap: `Vector[i8, 4]` lanes of `100` doubled give `-56`.

This is a **deliberate exception** to the trap-at-the-declared-width rule of [design.md §5](design.md#numeric-semantics), and the only one in the language. Scalar integer arithmetic traps; so does element-wise `Column[T]` and `Tensor[T, S]` arithmetic, at the element's width, in both backends. `Vector[T, N]` departs because its contract is different in kind: it is the portable-SIMD type, defined above as lowering "to the widest available primitive on the target", so what a user asks for when they reach for it is *machine lane semantics*. No SIMD ISA traps, and checking each lane would mean a per-operation cross-lane reduction to decide whether to fault — plausibly costlier than the arithmetic itself, on the one type in the language whose entire purpose is arithmetic throughput.

The exception is recorded here rather than left implicit precisely because the `Column` / `Tensor` precedent points the other way: a reader who finds the wrap and assumes the trap rule was forgotten should find this paragraph instead. Code that needs the trap should compute in the scalar or `Column` surface; code that needs the wrap in a scalar context has no shorthand today.

**Why deferred:** CPU lowering and the GPU backend's SPIR-V / WGSL emission are both work for this track and the [GPU](#gpu) track. The type, the operations, and the auto-fallback rule are designed.

**Why non-breaking:** Purely additive. `Array[f32, 4]` and other element-wise patterns continue to work; `Vector[T, N]` is a separate type, not a re-typing of fixed-size arrays.

**Design shape:**

```kara
type Vector[T: Numeric, const N: i64];

let a: Vector[f32, 4] = Vector[f32, 4](1.0, 2.0, 3.0, 4.0)
let b: Vector[f32, 4] = Vector[f32, 4](0.5, 0.5, 0.5, 0.5)
let c = a + b       // element-wise add
let d = a * b       // element-wise multiply
let s = a.dot(b)    // dot product -> f32
let x = a.cross(b)  // cross product -> Vector[f32, 3] (cross is 3D only)
let y = a[0]        // lane access -> f32
```

**Permitted element types.** `T` is bound by the `Numeric` trait — every primitive integer (`i8` … `i128`, `u8` … `u128`) and floating-point type (`f32`, `f64`, plus `f16` / `bf16` once they ship). `bool` lane vectors are exposed via the comparison and mask APIs (`a.lt(b) -> Vector[bool, N]`); they are not constructed directly. `usize` is not a permitted element type, consistent with the rule that idiomatic Kāra uses `i64` for sizes and indices (§ Numeric Semantics).

**Permitted lane counts.** `N` is any positive `i64` const expression. Powers of two (2, 4, 8, 16, 32, 64) map to native vector primitives on every supported CPU and GPU; non-power-of-two `N` (e.g., `Vector[f32, 3]` for RGB / 3D math) is legal and lowers either to the next-larger native vector with masked tail lanes or to a scalar loop, whichever the backend chooses. `N <= 0` is a compile error at the const-arg evaluation site.

**Auto-fallback rule.** A `Vector[T, N]` operation lowers in this order:

1. **Native instruction.** If the target exposes a vector instruction that covers `T` and `N` exactly (e.g., `Vector[f32, 4]` on SSE / NEON / **wasm-simd-128**, `Vector[f64, 4]` on AVX, `Vector[i32, 16]` on AVX-512), the operation is emitted as one instruction.
2. **Wider lane width with masking.** If the target has a vector unit but `N` doesn't fit a single instruction, the compiler emits two or more vector operations and combines the results — still vectorised, just spread across instructions.
3. **Scalar fallback.** If the target has no vector unit at all, or the element type isn't supported by any vector unit (e.g., `Vector[i128, 4]`), the compiler emits a scalar `for i in 0..N { ... }` loop. The program still compiles, links, and runs — it is portable by guarantee, fast where the hardware allows.

The fallback is silent by default. A `--simd-report=verbose` flag prints which operations dropped to scalar, for tuning hot loops. A `#[require_simd]` attribute on a function makes scalar fallback a hard error: useful for code where the *whole point* is hardware vectorisation and a scalar loop would silently lose orders of magnitude of throughput.

**WebAssembly SIMD-128 is a first-class lowering target.** WASM exposes a fixed 128-bit vector register file (`v128`) with element types matching every primitive Kāra `Numeric` lane. `+simd128` is the wasm feature default (SIMD-128 is WASM 2.0 baseline, shipped by every current engine): `Vector[f32, 4]`, `Vector[i32, 4]`, `Vector[i16, 8]`, `Vector[i8, 16]`, `Vector[f64, 2]` all lower to single WASM SIMD-128 instructions on `wasm_browser` and `wasm_wasi` targets, with no flag. Wider Kāra vectors (e.g., `Vector[f32, 8]`) lower under tier 2 (two `v128` operations combined). The portable-by-guarantee escape for hosts without SIMD-128 is the *build*, not runtime dispatch — WASM feature validation is module-granular, so such a host rejects any `v128`-carrying module outright rather than falling back per-instruction: `--target-features=-simd128` (last-wins over the default) scalarizes every vector op into an MVP-clean module. Under that opt-out the target has no vector unit at all, so every vector op classifies tier 3 — `--simd-report` says so, and `#[require_simd]` becomes a hard error naming `-simd128` as the cause. Browser-playground perf benchmarks depend on this being committed — same code, same perf curve story, different ISA.

**Idiom note — construction inside hot loops.** Building a fresh `Vector[T, N]` from scalar lane expressions **every loop iteration** is an anti-pattern the optimizer does not clean up: each construction is N scalar evaluations plus N lane inserts (and on wasm the backend never re-fuses the chain into a `v128.load` — the per-tap-construction form measured **~4.7x slower** than the scalar baseline it replaced, while the keep-vectors-live form of the same kernel won 1.47x). The winning idiom: construct/`splat` **outside** the loop, keep accumulators and coefficient vectors live **across** iterations, and extract lanes rarely. One shape is compiler-recognized: `Vector[T, N](v[b], v[b + 1], …, v[b + N-1])` — all lanes consecutive indexes into one `Vec[T]` whose element type is exactly `T`, with a side-effect-free base — lowers as a **single elem-aligned vector load** (`v128.load` on wasm, unaligned vector load on native) with the same two bounds checks and the same panic order the scalar form has. Anything else (casts in the lanes — `src[p] as f64` cannot be a contiguous load; mixed sources; non-consecutive offsets) stays on the insertelement chain, so hoist it out of the loop instead.

**Architecture-specific intrinsics.** Beyond the portable surface, target-specific intrinsics — AVX-512 mask shuffles, NEON pairwise adds, SVE predicates — live in platform modules gated by `#[cfg(target_feature = "...")]`. Code that uses them is non-portable by construction, so cfg-gating is mandatory: every direct intrinsic call is in a `#[cfg]`-scoped function. The portable `Vector[T, N]` API never panics on a missing vector unit; cfg-gated intrinsic modules simply don't compile when the target feature is absent.

**GPU mapping.** On the GPU backend, `Vector[T, N]` for `N ∈ {2, 3, 4}` maps directly to SPIR-V `OpTypeVector` / WGSL `vec<N, T>`. Larger `N` lowers to GPU buffer ops with explicit lane-width loops — same auto-fallback principle, different hardware.

**Memory layout.** `Vector[T, N]` is `repr(simd)`-equivalent: `N` contiguous `T` lanes, alignment equal to `T.align_of() * next_pow2(N)`. The layout is FFI-stable for power-of-two `N` (matches `__m128`, `__m256`, NEON / SVE register layouts); non-power-of-two `N` is not FFI-stable — pad to the next power of two and ignore the trailing lanes if interop is required.

**Trait surface.** `Vector[T, N]` exposes the following operations at the language level. Borrowed from Rust's `std::simd` as the baseline; gather/scatter are tier-dependent (cheap on AVX-2 / AVX-512, expensive on NEON):

| Category | Operations |
|---|---|
| Construction | `Vector[T, N].splat(x)` (broadcast scalar), `Vector[T, N].from_array([...])`, `Vector[T, N].from_slice(s)` |
| Element-wise arithmetic | `+`, `-`, `*`, `/`, `%` (per `Numeric` trait) |
| Element-wise comparison | `lt`, `le`, `gt`, `ge`, `eq`, `ne` → `Mask[N]` |
| Bitwise | `&`, `\|`, `^`, `!` (integer lanes only) |
| Horizontal reductions | `reduce_sum`, `reduce_product`, `reduce_max`, `reduce_min`, `reduce_and`, `reduce_or`, `reduce_xor` |
| Lane access / mutation | `v[i]`, `v.set(i, x)`, `v.replace(i, x)` (returns new vector) |
| Lane shuffling | `v.shuffle([i0, i1, …, i_{M-1}])` (compile-time index list; result lane `j` = source lane `indices[j]`, so `M` may differ from `N`), `v.reverse()`, `v.rotate_lanes_left(n)`, `v.rotate_lanes_right(n)` |
| Masked load/store | `Vector[T, N].load_masked(slice, mask)`, `v.store_masked(slice, mask)` |
| Conditional select | `mask.select(a, b)` — per-lane choice between two vectors |
| Conversion | `Vector[U, N].cast_from(v)` (lossy where applicable), saturating-cast variants |
| Cross product | `v.cross(w)` (`N == 3` only) |
| Dot product | `v.dot(w) -> T` |
| Gather / scatter | `Vector[T, N].gather(slice, indices)`, `v.scatter(slice_mut, indices)` — native where the target supports gather/scatter (AVX-2+, AVX-512). Always available semantically; the auto-fallback rule covers the unsupported case. |

The full method-by-method specification lives in the `core::simd` API reference; this table enumerates the surface as a contract.

**`Tensor[T, S]` ↔ `Vector[T, N]` interop.** Iterating a Tensor as a stream of `Vector[T, N]` chunks is the canonical pattern for writing hand-vectorized stdlib kernels (see [Hand-Vectorized Data-Spine Commitment](#hand-vectorized-data-spine-commitment)). The four-function interop API:

```kara
// Yields successive Vector[T, N] chunks of a contiguous Tensor view, plus a Slice for the non-multiple tail.
fn chunks_simd[T, S, const N: i64](t: ref Tensor[T, S]) -> (Iter[Vector[T, N]], Slice[T])

// Mutable counterpart for in-place ops.
fn chunks_simd_mut[T, S, const N: i64](t: mut ref Tensor[T, S]) -> (Iter[mut ref Vector[T, N]], mut Slice[T])

// Single-vector load/store at a given offset, with mask for the tail.
fn load_simd[T, const N: i64](t: ref Tensor[T, [?]], offset: i64, mask: Mask[N]) -> Vector[T, N]
fn store_simd[T, const N: i64](t: mut ref Tensor[T, [?]], offset: i64, mask: Mask[N], v: Vector[T, N])
```

**Split-borrow handling.** `chunks_simd` returns *both* an iterator over disjoint vector chunks and a `Slice[T]` over the tail — a split borrow against the same Tensor. `Slice[T]` is a borrow form (not an owned thing), and the iterator's `Vector[T, N]` chunks borrow disjoint segments. The implementation verifies the split point is not reachable through both handles simultaneously — same shape as Rust's `split_at_mut`. The user-facing API exposes the split via tuple destructuring; no annotation is required.

**Non-contiguous Tensors.** `chunks_simd` requires `t.is_contiguous()` and panics otherwise — caller must `.contiguous()` first, matching NumPy / PyTorch convention. Non-contiguous SIMD via gather/scatter is a later extension (the gather/scatter trait-surface entries above are the substrate).

**Multi-dimensional chunking.** `Tensor[T, [M, N]]` chunks the **last axis** (row-major); the iterator yields rows of vector chunks. Axis-stride-aware SIMD (chunking arbitrary axes) is a later extension.

### Multiversioning: `cpu-baseline` and `#[multiversion]`

> v1 sets the CPU baseline per target with `--target-cpu` and a per-target table ([design.md §16](design.md#cpu-baseline)). The `cpu-baseline` manifest knob below is this track's proposal and must be reconciled with that table when it returns.

CPU-feature variance across deployment hardware is handled by two complementary mechanisms:

**1. `cpu-baseline` (project-level, `kara.toml`).** Declares the minimum CPU feature level the produced binary requires. Single binary, no runtime dispatch overhead. Default: `cpu-baseline = "v3"` — covers ~95% of x86 hardware deployed in 2026 (excludes pre-Haswell) and aligns with the recent Linux distro shift (RHEL 9, Ubuntu 23.10+) toward v3-class baselines. On aarch64, the corresponding sweet spot is ARMv8.4-A: Apple M1+ are ARMv8.4+; AWS Graviton 3 is ARMv8.4-A; Graviton 4 is ARMv9-A (a strict superset). Users on M-series Macs and modern Graviton get the v3 baseline automatically.

The `cpu-baseline` knob is **target-agnostic at the surface**; the actual feature implications are per-architecture:

| Knob value | x86_64 (`-march=...`) | aarch64 (`-march=...`) |
|---|---|---|
| `"v1"` | `x86-64` (SSE2 baseline) | `armv8-a` (NEON baseline) |
| `"v2"` | `x86-64-v2` (SSE3 / SSSE3 / SSE4.1 / SSE4.2 / POPCNT) | `armv8.2-a` (FP16, dotprod) |
| `"v3"` (default) | `x86-64-v3` (AVX / AVX2 / BMI / FMA) | `armv8.4-a` (extended FP16) |
| `"v4"` | `x86-64-v4` (AVX-512F / BW / CD / DQ / VL) | `armv8.6-a` (BF16, I8MM) |

ARMv9 / SVE / SVE2 as a *programming model* — variable-length SIMD vectors — is out of scope; `Vector[T, N]` is fixed-length by construction and lowers to NEON, not SVE. ARMv8.6-A's BF16 and I8MM extensions cover the practical "modern feature-rich" payoff (Tensor Core-style INT8 matmul, BF16 arithmetic) without entering SVE territory.

**2. `#[target_feature]` / `#[multiversion]` (function-level, opt-in).** For hot kernels — the [hand-vectorized data spine](#hand-vectorized-data-spine-commitment) — selected functions can ship multiple variants compiled against different feature sets. A dispatcher selects at first call:

```kara
#[target_feature("avx512f", "avx512bw")]
fn cosine_similarity_avx512(a: ref Tensor[f32, [D]], b: ref Tensor[f32, [D]]) -> f32 { ... }

#[multiversion(baseline, "avx2", "avx512f")]
fn cosine_similarity(a: ref Tensor[f32, [D]], b: ref Tensor[f32, [D]]) -> f32 {
    // Compiler synthesizes the dispatcher; body is the baseline implementation.
    ...
}
```

`#[multiversion(...)]` is sugar over multiple `#[target_feature(...)]` variants of the same function. The dispatcher is a runtime function-pointer set on first call (ifunc on Linux; manual pointer-swap on macOS / Windows). On aarch64, the analogous attribute names are `#[target_feature("sve2")]`, `#[target_feature("i8mm")]`, `#[target_feature("bf16")]`; same `#[multiversion(...)]` sugar wraps either set.

**ABI / inlining interaction.** Function-multiversioning canonically defeats inlining of the multiversioned function (since the call site doesn't know which variant will run). Mitigated by ensuring the multiversioned function is *itself* the hot kernel — the caller is cold, the callee is hot, so inlining at the call site is low-value.

**Stdlib-internals SIMD policy.** Stdlib implementations may use `Vector[T, N]` and `#[target_feature]` paths internally where doing so provides a measurable improvement on representative workloads. The user-facing API surface remains scalar — `String.contains`, `Vec.iter().sum()`, `Map.get`, etc. all present the same scalar signatures regardless of internal vectorization. Multiversioned variants follow the rules above. This policy unlocks simdjson-class JSON parsing, simdutf-class UTF-8 validation, SWAR-accelerated HTTP header tokenization, Hyperscan-style regex prefilter, and similar internal optimizations — none of which are commitments, each of which follows per-workload as benchmarks justify.

### CPU feature dispatch

These rules extend the v1 CPU baseline ([design.md §16](design.md#cpu-baseline)).

**Interaction with `#[target_feature(...)]`.** The two operate on different layers and never conflict: `--target-features` edits the **floor** — the baseline feature set every monomorphized function compiles against — while a function-level `#[target_feature(enable = "...")]` widens **above** the floor for that one function, guarded by multiversioning dispatch. A baseline `-feat` therefore does not narrow, disable, or error against a function-level `+feat`: the function still compiles with the feature and still dispatches only on hardware that has it — exactly the floor/ceiling composition described under "What this is not" below. The converse also holds: a function-level enable never leaks the feature into the floor for the rest of the program. (An attribute-level *disable* form, if ever added, would be the only place a conflict could arise and must be specified then; only the `enable` direction is reserved.)

**What this is not.** The baseline is *not* a substitute for function multiversioning (per-function feature dispatch at runtime). The baseline is the floor every monomorphized function compiles against; multiversioning is the ceiling specific functions can opt up to. They compose: a build with `--target-cpu=apple-m1` and a `#[multiversion]` hot path for `apple-m4` features stays portable to every Apple Silicon Mac and dispatches the wider implementation on M4-or-newer at runtime.

Under this proposal, CPU-feature targeting is controlled by `cpu-baseline` in `kara.toml` (default `"v3"` — see [Multiversioning](#multiversioning-cpu-baseline-and-multiversion)) rather than `--target-cpu=native`, because single-binary distribution needs predictable feature requirements per build target.

<a id="complexity-budgets-g43-and-static-stack-depth-analysis"></a>
### Complexity Budgets and Static Stack Depth Analysis

**Decision:** Defer both heap complexity budgets and static stack depth analysis. The heap case is already covered by the effect system (`allocates(Heap)` detection). Stack depth analysis requires transitive call graph computation and is primarily useful for embedded builds.

**Why deferred:** Effect system handles heap allocation tracking. Stack budget analysis is embedded-only. Both depend on [project profiles](#project-profiles).

**Why non-breaking:** Purely additive. Opt-in annotations (`#[max_stack(N)]`).

**Design shape:** See below for the `#[max_stack(N)]` spec.

#### Static Stack Depth Analysis

**Decision:** For `embedded` profile builds, compute maximum stack depth statically. Expose as `#[max_stack(N)]` enforcement annotation and `karac build --output=json` field.

**Why deferred:** Requires fully resolved call graph. Useful only for `embedded` builds.

**Why non-breaking:** Purely additive. `#[max_stack(N)]` is opt-in.

**Design shape:**

```kara
#[max_stack(512)]
fn handle_interrupt() { ... }
```

Always computed for `embedded` builds. Queryable via `karac query stack`.

### CircularBuffer[T]

A fixed-capacity ring buffer with O(1) push/pop at both ends and no heap reallocation after construction. The standard workhorse for audio DSP, networking packet queues, sensor data pipelines, and any producer/consumer pattern with bounded memory requirements.

**Why deferred:** `Vec[T]` suffices for most uses, but the absence of a ring buffer in the standard library forces every audio and networking library to ship its own, leading to incompatible types at API boundaries. The design is fully settled (classic ring buffer, no open questions), and the demand is deterministic: any audio, DSP, or real-time networking library will need this. It is non-breaking and only waits on library scope.

**Design shape:**

```kara
struct CircularBuffer[T] {
    // capacity fixed at construction; no reallocation
}

impl[T] CircularBuffer[T] {
    fn new(capacity: i64) -> CircularBuffer[T]
        with allocates(Heap)

    fn push_back(mut ref self, value: T) -> Result[(), Full]
    fn push_front(mut ref self, value: T) -> Result[(), Full]
    fn pop_back(mut ref self) -> Option[T]
    fn pop_front(mut ref self) -> Option[T]
    fn peek_back(ref self) -> Option[ref T]
    fn peek_front(ref self) -> Option[ref T]

    fn len(ref self) -> i64
    fn capacity(ref self) -> i64
    fn is_empty(ref self) -> bool
    fn is_full(ref self) -> bool
    fn clear(mut ref self)

    // Contiguous read window (for DMA / zero-copy I/O)
    // Returns one or two slices depending on wrap state
    fn as_slices(ref self) -> (Slice[T], Slice[T])
}
```

**No allocation after construction.** Every method after `new` is allocation-free — callers can include `push_back` / `pop_front` in functions that omit `allocates`, providing the same real-time guarantee as stack allocation with the flexibility of a queue.

**Overwrite mode (library extension).** A non-erroring `push_overwrite` that evicts the oldest element is a common variant (audio capture ring buffers almost always want this). It is intentionally omitted from the core API to keep the default behavior explicit — `push_back` returning `Err(Full)` forces the caller to handle backpressure.

## Verification

Predicates on distinct types, refinement types and contracts (`requires`, `ensures`, `invariant`), with the tooling built on them.

> The scope review recommends that this track keep one predicate mechanism: `distinct … where` plus `requires`. Structural refinement subtyping (implicit widening of a refined value to its base, and through covariant slots) and the asserting meaning of `as` are already removed from the text below. Whether plain `type … where` refinements return beside `distinct … where` is for the track to decide.

### Refinement Types

Refinement types attach value constraints to types via `where` clauses. The constraint is checked at construction boundaries; once a value has the refined type, the constraint is guaranteed:

```
type NonZero = i32 where self != 0;
type Positive = i32 where self > 0;
type Percentage = f64 where self >= 0.0 and self <= 100.0;
type ValidPort = u16 where self >= 1 and self <= 65535;
type NonEmpty[T] = Vec[T] where self.len() > 0;
```

**Refinement constraint language.** The `where` clause accepts any *pure expression* over `self` and compile-time constants. Formally:

```
REFINEMENT_PRED = PURE_EXPR
PURE_EXPR       = PURE_EXPR ARITH_OP PURE_EXPR        // + - * / % & | ^ << >>
                | PURE_EXPR COMPARE_OP PURE_EXPR       // < <= > >= == !=
                | PURE_EXPR ("and" | "or") PURE_EXPR
                | "not" PURE_EXPR
                | "(" PURE_EXPR ")"
                | "self"
                | "self" "." FIELD_NAME                // struct field access: self.lo
                | "self" "." PURE_METHOD "(" ")"       // pure zero-arg method: self.len(), self.is_empty(), self.is_ascii()
                | CONST_EXPR                           // literal or module-level const
PURE_METHOD     = any method declared with no effects (i.e., no effect annotations)
```

**What is allowed:**
- Arithmetic and bitwise operators on `self` — `self % 2 == 0`, `self & (self - 1) == 0`
- Comparison operators — `self >= 0 and self <= 100`
- Boolean combinators — `and`, `or`, `not`
- Field access on struct refinements — `self.lo < self.hi`
- Zero-argument method calls on `self` that have no effect annotations (`self.len()`, `self.is_empty()`, `self.is_ascii()`, `self.is_sorted()`, etc.)
- References to module-level `const` constants and literals on either side

**What is not allowed:**
- Method calls with arguments — `self.contains(x)` is disallowed
- Calls to functions outside `self` — `hash(self)` is disallowed
- Mutable state, closures, or any expression that carries an effect

The constraint language and static elision are **separate concerns.** The `where` grammar defines what is *expressible*; the two elision rules (§ Compile-time elision procedure) define when the runtime check can be omitted. A constraint may be expressible but always emit a runtime check (e.g., `self.is_ascii()` is a valid constraint; the compiler does not attempt to prove it statically).

**Construction:** Dynamic values enter refined types via `try_from`, which returns `Result`:

```
let port = ValidPort.try_from(user_input);  // Err if out of range
```

**Compile-time elision procedure.** Kāra's refinement elision is intentionally minimal. There are exactly **two elision rules**, and nothing else. No SMT solver, no interval arithmetic, no cross-type predicate matching, no flow-sensitive narrowing, no symbolic implication — all of those belong to [Level 3-4 gradual verification](#gradual-verification-level-3-4). The two rules are:

1. **Const-evaluable narrowing.** If the initializer of a binding with a refined target type is a **const-evaluable expression** (whatever the language's const-evaluator can reduce to a concrete value — at minimum, literals and const-literal arithmetic), the compiler:
   - evaluates the expression at compile time to a concrete value `v`;
   - evaluates the refinement predicate against `v`;
   - if the predicate holds, emits **no runtime check and no error path** — the value is admitted as the refined type;
   - if the predicate fails, emits a **compile error** (not a runtime error — const-eval has the value in hand, so the failure is deterministic and catchable at build time).
2. **Type-identity narrowing.** If the static type of the initializer is *exactly* the target refined type (same type name, same predicate, same base), no check is emitted. This is trivially redundant and covers pass-through cases like `let y: Positive = x` where `x: Positive`.

Any narrowing that does not match one of these two rules requires an **explicit** coercion: `Refined.try_from(value)` (recoverable, returns `Result`). There is no implicit form.

```
let port: ValidPort = 80;              // OK — rule 1: const-eval 80, predicate 1 <= 80 <= 65535 holds
let port: ValidPort = 70000;           // COMPILE ERROR — rule 1: const-eval 70000, predicate fails
let port = ValidPort.try_from(80)?;   // OK — explicit form; error path optimized away because 80 is a literal
let port = ValidPort.try_from(n)?;    // OK — runtime value; runtime check emitted
let port: ValidPort = n;               // COMPILE ERROR — rule 1 does not apply (n is not const-evaluable)
                                       // and rule 2 does not apply (n: i32, not ValidPort)
                                       // — programmer must write try_from
```

**Call-site behavior.** Function call argument passing is governed by the same two rules as binding initialization. At `f(x)` where `f` expects a refined parameter type `R` and `x` has type `T`:

- If `T = R` exactly: no check (rule 2).
- If `x` is a const-evaluable expression: compile-time evaluation (rule 1) — emit nothing on success, compile error on failure.
- Otherwise: **compile error**. The programmer must write `f(R.try_from(x)?)` explicitly at the call site.

There is no implicit runtime-value narrowing at call boundaries, ever. This keeps call-site behavior identical to binding initialization and keeps the surface area of "where can a refinement check silently appear" down to a single, const-eval-only form.

**No flow-sensitive narrowing.** A runtime value cannot be locally refined by surrounding control flow. `if x > 0 { f(x) }` does **not** make `x` a `Positive` inside the `then` branch; the static type of `x` remains `i32`, and the call `f(x)` where `f: fn(Positive)` is still a compile error inside that branch. Programmers who want the flow-sensitive effect write `let x: Positive = 5` at the binding site (where rule 1 applies), or use explicit `Positive.try_from(x)?` inside the branch. A restricted form of flow-sensitive narrowing (immutable locals, syntactic predicate match, single-function scope) is a separate entry: [Flow-Sensitive Refinement Narrowing](#flow-sensitive-refinement-narrowing-restricted).

**Known ergonomic cost — accepted trade-off.** The `Positive.try_from(x)?` inside a branch whose guard already proves `x > 0` will feel redundant and will be a common user complaint. This is an accepted trade-off: the compiler's job is to verify program correctness from static type information, not to execute branches and analyze runtime values. Flow-sensitive narrowing requires the compiler to reason about control-flow-dependent value properties across closures, pattern matching, mutable rebinding, and effect boundaries — a dataflow analysis where unsoundness (incorrect narrowing) is far worse than verbosity. The explicit `try_from` makes the proof obligation visible and keeps the type system's reasoning local to declarations, not control flow.

**Generic code never elides.** A polymorphic function `fn f[T](v: T)` operates on the abstract type `T`. Even when monomorphized against a refined type, the *body* of `f` sees `T` as opaque — it cannot inspect the predicate, so no elision is possible inside `f`. The two elision rules apply only to the *caller*: the caller performs widening/narrowing against the concrete instantiated type, and rule 1 or rule 2 decides whether a check is emitted at the call boundary.

**Narrowing — base → refined.** Two forms, governed by the elision procedure above:

- **Const-evaluable initializer** (rule 1): implicit, compile-time-checked, no runtime check emitted. Compile error on predicate failure.
- **Explicit `try_from` (recoverable):** returns `Result`; runtime check unless the argument is a literal (in which case the error path is elided by the optimizer). No `panics` effect.

```
let n: NonZero = 42;                      // implicit, const-eval — no check emitted
let n: NonZero = 0;                       // COMPILE ERROR — const-eval, predicate fails
let n: NonZero = NonZero.try_from(42)?;  // explicit; literal — error path elided by optimizer
let n: NonZero = NonZero.try_from(x)?;   // explicit; runtime value — check emitted
let n: NonZero = x;                       // COMPILE ERROR — no implicit runtime-value narrowing
```

**Cross-refinement — refined A → refined B sharing the same base.** Two distinct refinements of the same base type have **no implicit subtyping relationship**, even when their predicates are textually identical. Implication-based elision is **not part of this design** — every cross-refinement coercion goes through an explicit form, and a runtime check is always emitted (or, for `try_from` with a literal argument, elided by the optimizer just like single-type narrowing). The coercion form is:

- **`try_from` (recoverable):** Returns `Result`; no `panics` effect. Use when the check may fail and you want to handle it.

```
type NonZero = i32 where self != 0;
type Positive = i32 where self > 0;

let p: Positive = 5;
let nz: NonZero = p;                         // ERROR — no implicit cross-refinement coercion
let nz: NonZero = NonZero.try_from(p)?;     // OK — returns Result, no panics effect
```

**No propagation through operations.** Arithmetic on refined types returns the base type. This is intentional — the compiler cannot prove the result still satisfies the constraint without running the check again. Re-refine explicitly via `try_from` if the constraint needs to be re-established:

```kara
let p: Positive = 5;
let q = p + 1;                        // q: i32 — NOT Positive; constraint is not preserved
let r: Positive = Positive.try_from(p + 1)?;   // OK — constraint re-checked at this point
```

This rule applies to all arithmetic operators, bitwise operators, and method calls that return the base numeric type.

**Interaction with effects.** `try_from` returns `Result` — no special effect. Contract violations at construction are value-level errors, not effects.

**Interaction with generics.** Refinements apply to the container, not the element: `fn head[T](list: NonEmpty[T]) -> T` works naturally.

**Composition with refinement `where` clauses.** Generic-parameter bounds and refinement predicates are orthogonal mechanisms; both can appear on one alias:

```kara
type SortedNonEmpty[T: Ord] = Vec[T] where self.len() > 0;
```

The generic bound (`T: Ord`) is checked at every use site against the type argument; the refinement (`self.len() > 0`) is checked at every value-construction site (per [Refinement Types](#refinement-types)). The two rules layer cleanly — the generic bound gates which `T`s the alias accepts; the refinement gates which values within `Vec[T]` are valid `SortedNonEmpty[T]`s.

**Interaction with refinement types.** Distinct and constrained can combine:

```
distinct type ValidPort = u16 where self >= 1 and self <= 65535;
```

This gives you both type safety (can't pass a random `u16` as a port) and value safety (port is always in range).

**Construction semantics for `distinct type T = Base where predicate`.** The combined form layers the two mechanisms; the rules are:

1. **`T(value)` constructor always checks the predicate.**
   - If `value` is a const-evaluable expression: compile-time check — compile error on failure, no runtime check on success.
   - If `value` is a runtime expression: runtime assertion, panics on failure, propagates `panics`.
   - There is no "raw wrap without checking" path for the combined form — every constructor call validates the constraint.

2. **`T.try_from(value)` is auto-generated**, just as for plain refinement types. Returns `Result[T, RefinementError]`. This is the recoverable path; use it when the value may be out of range and you want to handle the error.

3. **Binding-site elision rules apply unchanged.** `let p: ValidPort = 80` triggers rule 1 (const-eval 80, check `1 <= 80 <= 65535`, emit no runtime check on success). `let p: ValidPort = n` where `n: u16` is a compile error — neither elision rule applies; use `ValidPort(n)` or `ValidPort.try_from(n)?`.

4. **`.raw()` returns the raw base type `Base`**, stripping both the `distinct` wrapper and the predicate. The predicate is a construction-time guarantee, not a runtime tag; `.raw()` is an explicit opt-out of both.

```kara
distinct type ValidPort = u16 where self >= 1 and self <= 65535;

let p = ValidPort(80);            // OK — const-eval: predicate holds, no runtime check
let q = ValidPort(70000);         // COMPILE ERROR — const-eval: 70000 > 65535
let r = ValidPort(n);             // OK if n: u16 — runtime check; panics if out of range
let s = ValidPort.try_from(n)?;   // recoverable — returns Err if out of range
let raw: u16 = p.raw();           // 80 — base type, no predicate
```

**Refinement types.** Exhaustiveness is checked over the **base type**, not the refinement-narrowed value set. A match on `type Positive = i64 where self > 0` uses `i64`'s constructor space and therefore still requires a wildcard arm. The refinement constraint `self > 0` is a *runtime* narrowing enforced at construction and by cross-refinement casts; it is not visible to the exhaustiveness algorithm. Reasoning about which base values are excluded by an arbitrary constraint is SMT territory and is reserved for [Level 3-4 gradual verification](#gradual-verification-level-3-4). Programmers who need an exhaustive match over a narrow value set should use an enum, not a refinement.

**Exception — bounded integer ranges.** A refinement whose constraint is exactly `self >= A and self <= B` (or `self >= A and self < B+1`, etc.), where `A` and `B` are integer compile-time constants and the base type is an integer primitive (`i8`..`i64`, `u8`..`u64`), defines a **closed finite domain** that the compiler can enumerate without SMT. For such types the exhaustiveness algorithm treats the type like an enum whose variants are the integers `A..=B`. A match that covers all values in `[A, B]` via literal and range patterns is accepted as exhaustive without a wildcard arm:

```kara
type ValidFloor = i64 where self >= 1 and self <= 10;

fn describe(floor: ValidFloor) -> String {
    match floor {
        1 => "ground",
        2..=9 => "upper",
        10 => "penthouse",
    }   // exhaustive — all values 1..=10 covered
}
```

The check is decidable because the range is bounded and the domain enumeration terminates. Unbounded refinements (`self > 0`) continue to require a wildcard. When B − A exceeds 1024 the compiler falls back to requiring a wildcard and emits a lint suggesting an enum. The `distinct type T = Base where self >= A and self <= B` combined form applies the same rule: if the range is bounded and small, the distinct type participates in exhaustiveness as a finite domain.

### Refinement Types in Inference, Declarations and Traits

**Refinement type predicates in check mode.**

A refinement type `type Positive = i64 where self > 0` reduces to its base type for arithmetic (per [Refinement Types](#refinement-types)) but retains its predicate for check-mode proof obligations:

- When checking `e ⇐ Positive`, synthesize `e ⇒ τ`, require `τ ≤ i64`, and emit a proof obligation `e > 0`.
- Obligations are discharged by (i) literal constant folding, (ii) arithmetic invariants (`abs(x) >= 0`), (iii) a bound variable already carrying the same refinement, or (iv) an explicit `@contract_check` runtime assertion.
- Obligations that cannot be discharged emit `cannot-prove P for expression e` with the specific predicate listed.

The full proof procedure — decidable fragment, SMT integration, confidence tiers — belongs to [Level 3-4 gradual verification](#gradual-verification-level-3-4). The bidirectional split is the correct home for refinement checks because obligations are inherently directional: you prove `e ⇒ P`, not the reverse.

**Declaration ordering.** The full order for function declarations is: signature → effects → `requires`/`ensures` → `where` → body:

```kara
fn binary_search[T](haystack: ref Vec[T], needle: ref T) -> Option[i64]
    with reads(SearchIndex)
    requires haystack.is_sorted()
where T: Ord
{ ... }
```

**Note:** `where` on type alias definitions (e.g., `type NonZero = i32 where self != 0`) is the refinement type syntax — a different feature using the same keyword. The distinction is context: type alias `where` is a value predicate on `self`; declaration `where` is a type constraint on a generic parameter.

5. If `T` is a refinement type `type P = B where C`, the base type `B` is also a candidate (but later than `T` itself — inherent and trait methods on the refined type win over methods on the base). Distinct types do *not* deref to their base: `distinct type UserId = i64` does not make `i64`'s methods available on `UserId` (that is the whole point of distinct types).

**Refinement types use `TryFrom`.** The standard re-refinement pattern is:

```kara
type Positive = i64 where self > 0;

// Compiler generates:
impl TryFrom[i64] for Positive {
    type Error = String;
    fn try_from(value: i64) -> Result[Positive, String] {
        if value > 0 { Ok(value) } else { Err("expected positive i64") }
    }
}

let p = Positive.try_from(n)?;
```

**Interaction with refinement types.** A refinement type `type EvenNumber = i64 where self % 2 == 0` does not implement `Add` directly — arithmetic on a refined value coerces to the base type first, so `(p: EvenNumber) + 1` lowers to `(p as i64) + 1` with result type `i64`. This matches the "arithmetic returns the base type" rule already specified for refinement types. No operator plumbing is required on the refinement itself.

### Contracts (requires / ensures / invariant)

Contracts express what a function guarantees about its inputs and outputs, and what a struct guarantees about its fields. Checked at runtime in debug builds, stripped in release:

```
fn binary_search[T: Ord](
    haystack: ref Vec[T],
    needle: ref T,
) -> Option[i64]
    with reads(SearchIndex)
    requires haystack.is_sorted()
    ensures(result) match result {
        Some(i) => haystack[i] == *needle,
        None => true,
    }
{
    // implementation
}
```

**Three keywords:**

| Keyword | Applies to | When checked | Purpose |
|---|---|---|---|
| `requires` | Function inputs | Before body executes | Preconditions on parameters |
| `ensures` | Function output | After body returns | Postconditions on `result` |
| `invariant` | Struct fields | After every `pub` method | Cross-field relationships (public boundary) |
| `impl invariant` | Struct fields | After every method (`pub` and private) | Internal cross-field relationships |

**Struct invariants:**

```
struct SortedVec[T: Ord] {
    data: Vec[T],

    invariant self.data.is_sorted()
}

struct DateRange {
    start: i64,
    end: i64,

    invariant self.start <= self.end
}
```

**`impl invariant` — invariants on all method exits.** The plain `invariant` form fires only at public method boundaries (see Rule 3 below). This is intentional for `pub`-API types: internal helpers are allowed to temporarily break the invariant as long as the outermost `pub` method restores it. But for types where correctness must hold after every private mutation step — complex internal state machines, `sync struct` types where field changes happen across many private methods — the looser guarantee is insufficient.

The `impl invariant` block fires at the exit of *every* method, including private ones:

```kara
struct Elevator {
    stops: Vec[i64],
    direction: Direction,

    // internal correctness: if we have stops, we must have a direction
    impl invariant not self.stops.is_empty() => self.direction != Direction.Idle
}
```

The two forms are independent and may coexist:

```kara
struct SortedQueue[T: Ord] {
    data: Vec[T],
    max_seen: Option[T],

    // checked after every method
    impl invariant self.data.is_sorted()

    // checked only at pub method exits (public API guarantee)
    invariant match self.max_seen {
        Some(m) => self.data.iter().all(|x| x <= m),
        None    => self.data.is_empty(),
    }
}
```

`impl invariant` carries a higher runtime cost in debug builds (the check fires more frequently), so it is appropriate for types where internal correctness must be verified at each step, not just at the public boundary. For the common case — rebalancing trees, multi-step state transitions in private helpers — the plain `invariant` is the right tool and `impl invariant` should be reserved for cases where the intermediate state must never be wrong even internally.

**Ordering with effects and generic bounds.** When a function has effect annotations, contracts, and a `where` clause, the order is: effects → requires → ensures → where:

```
fn process(id: i64) -> Report
    with reads(UserDB)
    requires id > 0
    ensures(result) result.entries.len() > 0
{ ... }
```

**Contract expressions must be pure.** No observable side effects allowed in `requires`, `ensures`, or `invariant` — enforced by the effect checker. The precise rule and the behavior under panic, invariant-check sites, and pre-state references are all specified in the **Contract evaluation semantics** subsection immediately below.

#### Contract evaluation semantics

This subsection nails down four questions that "pure + runtime-checked in debug" leaves open: which effects are allowed in contract expressions, what happens if a contract predicate panics during evaluation, when invariants fire, and how a postcondition references pre-state on a consumed receiver.

**Rule 1 — Contract purity is "effect set ⊆ `{panics}`."** The effect checker analyzes contract expressions (`requires`, `ensures`, `invariant`) as ordinary expressions and imposes the constraint that the inferred effect set is a subset of `{panics}` — meaning only the bare `panics` effect (not resource-scoped variants like `panics(X)`) is permitted. Any of the seven non-panic effects (`reads`, `writes`, `sends`, `receives`, `allocates`, `blocks`, `suspends`) appearing in a contract expression is a **compile error** at the contract boundary, with the diagnostic pointing at the offending call and naming the forbidden effect.

`panics` is allowed — and only `panics` — because indexing (`haystack[i]`), division, `unwrap()`, and explicit `panic()` all carry the `panics` effect, and all four are idiomatic in contract predicates. Forbidding `panics` would force contracts to be written in terms of `.get()`, `checked_div`, and `match`-on-`Option` machinery, making contracts harder to write than the code they check. Non-panic effects stay forbidden because they would introduce *observable* side effects from runtime predicate evaluation — mutating state, reading files, sending messages — which is the real hazard that "pure" is trying to rule out.

Method calls inside contracts are admitted iff the called method's effect set is a subset of `{panics}`. This is the same rule as any other expression position, applied at the contract boundary.

**Rule 2 — Panic during contract evaluation is a distinct fault category.** If a contract predicate panics during runtime evaluation (in debug builds), the program aborts with the fault category **`contract predicate panicked at <loc>: <panic message>`**, which is *distinct* from the predicate-returned-false fault **`contract violated: <predicate>`**. These two faults are reported separately and are not conflated.

The distinction matters because the two cases have different root causes:
- **"Contract violated"** means the checked code passed inputs or produced outputs that the contract said it would not — the bug is in the *checked code*.
- **"Contract predicate panicked"** means the *predicate itself* failed to evaluate — the bug is typically in the *contract*. For example, `ensures(result) haystack[i] == *needle` can panic when `i >= haystack.len()` even though the checked code is correct; the fix is to rewrite the contract, not to change the function.

Collapsing both cases into one fault would misdirect programmers debugging contract failures. Aborting the program on either fault is the debug-build behavior; both are stripped in release. Under `karac test`, the test runner catches both categories and reports each as a test failure with its own distinct message, so failing tests point at the right bug class.

**Rule 3 — Plain `invariant` is checked at the exit of every `pub` method; `impl invariant` is checked at the exit of every method (pub and private).** Both rules are **structural, not dataflow-driven**: every qualifying method of a type with an `invariant` or `impl invariant` block inserts the check immediately before every return point, regardless of whether the method syntactically mutates any field, and regardless of receiver mode (`self`, `ref self`, `mut ref self`, or no receiver for constructors).

For the plain `invariant` form, private methods **do not** check the invariant. This is deliberate — a multi-step update that needs to temporarily break the invariant (e.g., rebalance a tree) can be factored into private helpers that freely leave the invariant violated between calls, as long as the outermost pub method restores it before returning. Use `impl invariant` when the invariant must hold after every private mutation step as well — see § Struct invariants.

Shared and `sync` types use the same rule. A pub method on a `sync struct` can mutate its `Atomic` and `Mutex` fields through a borrowed receiver; the invariant check still fires at method exit. No "does this method observe or modify state" analysis is performed — the check site is receiver-kind-agnostic and purely based on whether the method is `pub` and whether the type has an `invariant` block.

Constructors (pub associated functions that return `Self`) also check the invariant at their return point. A type's invariant therefore holds at every externally observable boundary — creation, mutation through any pub method, and any combination of the two — and the checker does not need to trust programmer annotations about which operations mutate.

**Rule 4 — Postconditions referencing pre-state use `old(expr)`.** When a function consumes its receiver (bare `self`, which is the owned/consuming receiver form) or a consumed parameter, that value is no longer in scope at the postcondition evaluation point, and referencing it directly is a **compile error**: `"cannot reference consumed parameter \`self\` in ensures clause; use \`old(self)\` or \`old(self.field)\` to capture pre-state"`.

The `old(expr)` form is a special form valid **only inside `ensures` clauses**. It evaluates `expr` at function entry (before the body runs), captures the value via `Clone`, and substitutes the captured snapshot wherever `old(expr)` appears in the postcondition. In debug builds, the compiler desugars `old(expr)` to a generated local that clones `expr` at the entry prologue, and rewrites the postcondition's reference to read from the local. In release builds, contracts are stripped, so neither the clone nor the check is emitted — pre-state capture costs zero at runtime in release.

Capture cloneability: the type of `expr` inside `old()` must implement `Clone` (`Copy` types satisfy this automatically). Non-cloneable types inside `old()` produce a compile error pointing at the `old()` call with the message `"\`old(...)\` requires the captured expression to be Clone"`. In practice this pushes programmers toward `old(self.balance)` (a primitive field, always cloneable) rather than `old(self)` (the whole aggregate, which may not be Clone) — a natural pressure toward narrow captures that match the postcondition's actual needs.

Scope restrictions:
- `old(...)` is **not** valid in `requires` — preconditions already run at entry, so pre-state is just "state".
- `old(...)` is **not** valid in `invariant` — invariants are checked after every pub method exit, and the pre-state of the method is irrelevant to the field-level invariant being checked.
- `old(result)` is a compile error — `result` does not exist at function entry.

Worked example:

```kara
struct Account { balance: i64 }

impl Account {
    fn transfer(self, amount: i64) -> (Account, i64)
        requires amount > 0
        requires self.balance >= amount
        ensures(result)
            result.0.balance == old(self.balance) - amount
            and result.1 == amount
    {
        (Account { balance: self.balance - amount }, amount)
    }
}
```

Here `old(self.balance)` captures the `i64` field (Copy, so Clone is automatic) at entry. The body consumes `self` and constructs a new `Account`. At exit, the postcondition reads `result.0.balance` (from the returned value) and compares it against the captured `old(self.balance)`. Because `self` is consumed, writing `self.balance` directly in the ensures clause would be a compile error — `old()` is the only legal form.

**Summary.** Contract expressions are `{panics}`-pure. A panic during evaluation is a distinct fault, not conflated with a violation. Invariants check after every pub method and every constructor, on all struct flavors. Pre-state capture uses `old(expr)` with a Clone requirement, available only in `ensures`.

**Verification strategy:** Runtime-checked in debug builds (like `assert`), stripped in release builds. No SMT solver. Immediately useful for:
- Documentation that doesn't drift from reality
- AI-generated code verification
- Debug-time bug detection

**Interaction with refinement types.** Refinement types are lightweight type-level constraints. Contracts are heavyweight function/struct-level constraints. They compose:

```
fn clamp(x: i32) -> Positive
    requires x != 0
    ensures(result) result <= 100
{ ... }
```

**Static discharge.** When a contract predicate is expressible within the refinement type system (numeric comparisons, `len()`), the compiler may discharge it statically at monomorphization — no runtime check is emitted in any build mode. For example, if a parameter's refinement type already guarantees `x > 0`, a `requires x > 0` contract is proven at compile time and removed. Predicates that exceed the refinement system's expressiveness (e.g., `haystack.is_sorted()`) remain runtime-checked in debug builds. This reuses the existing refinement infrastructure; no SMT solver is involved. Concretely, static discharge applies the same two-rule system as refinement type elision: (1) if the contract predicate can be const-evaluated to true from the call-site arguments, it is discharged; (2) if a parameter's refinement predicate syntactically matches the contract predicate (same comparison operator, same variable), it is discharged. All other predicates remain as runtime checks in debug builds.

**Contracts do not constrain effects.** Predicates of the form "this function does not allocate," "does not panic," or "does not write to resource R" are *not* expressed through `requires` / `ensures`. The declared effect set on a public function is the canonical mechanism: omitting `writes(R)` from a public function's declaration is the commitment that the function does not write to `R`, and the effect checker verifies it statically against the body. (One refinement for “does not allocate” specifically: `allocates(Heap)` is default-permitted — see the effect-verb section — so under the default profile that commitment is expressed by a profile that removes the permit, or by `#[no_effect(allocates(Heap))]` at the boundary, not by mere omission.) Mismatch is a compile error, not a debug-time runtime check. This keeps the spec layered cleanly — declared effects are *guaranteed semantics* ([design.md §2](design.md#2-specification-layers)), while contract predicates are runtime-checked in debug and stripped in release. Mixing the two would put effect-subset guarantees in the strippable layer, which is strictly weaker than what the effect system already provides. There is therefore no `forbids(verb)` or `forbids(verb, resource)` predicate. For private functions, where effects are inferred rather than declared, the same guarantee is available by promoting the function to declared-effect form, or by stating the constraint at the nearest enclosing public boundary.

### Production Contract Checking (`#[checked]`)

**Decision:** Contracts (`requires`/`ensures`/`invariant`) are stripped in release builds; production-time validation uses explicit `if` checks with `Result` returns.

**Why deferred:** `#[checked]` creates a third category between "debug assertion" and "real validation logic." The `Result`-based failure mode is too restrictive (only works on functions returning `Result`). The `panics` failure mode is simpler but adds complexity to the effect system. Until then, the guidance is clear: contracts for development-time verification, explicit validation for production-time checks. If experience shows users want a shorthand for "keep this contract in release," add `#[checked]` later.

**Why non-breaking:** Purely additive. Existing contracts remain stripped in release. `#[checked]` would be a new attribute on contracts that currently have no attribute.

**Design shape:**

```kara
fn transfer(amount: i64, from: Account, to: Account) -> Result[Receipt, Error]
    #[checked] requires amount > 0
    #[checked] requires from.balance >= amount
    ensures(result) match result {
        Ok(r) => r.amount == amount,
        Err(_) => true,
    }
{ ... }
```

`#[checked]` contracts use `panics` semantics — a violated contract panics and adds `panics` to the function's effect set.

#### Opt-in Release-Mode Contract Checks

**Decision:** Allow individual contracts to survive release builds via a `#[checked]` annotation, plus a build-level flag for blanket control.

**Why deferred:** Static discharge (contracts provable via refinement types are eliminated at compile time) and debug-mode checking cover the immediate needs. No user demand yet for release-mode contract checks.

**Why non-breaking:** Purely additive. Default behavior (contracts stripped in release) is unchanged.

**Design shape:**

```kara
fn transfer(amount: i64, balance: i64)
    #[checked] requires amount > 0          // survives release builds
    #[checked] requires amount <= balance   // survives release builds
    requires some_expensive_validation(amount) // debug-only (default)
{ ... }
```

Build-level override:
- `karac build --contracts=none` — strip all (current default)
- `karac build --contracts=checked` — keep only `#[checked]` contracts in release
- `karac build --contracts=all` — keep all contracts in release (safety-critical domains)

### Gradual Verification (Level 3-4)

SMT solver integration for proving contracts at compile time. May never be built — the cost/benefit ratio depends on how far the effect system and refinement types take the language without formal verification.

#### Verification Levels

**Level 2 (Refinement Types):** This track. `type Name = BaseType where constraint` — see [Refinement Types](#refinement-types). Numeric comparisons + `len()`, no SMT solver. The elision procedure is exactly two rules: (1) const-evaluable initializers are checked at compile time (no runtime check on success, compile error on failure); (2) type-identity pass-through emits no check. Every other narrowing — runtime-value binding, runtime-value call-site argument, cross-refinement coercion — requires an explicit `try_from`. No flow-sensitive narrowing, no occurrence typing, no implication-based cross-refinement elision.

**Level 2.5 (Contracts):** This track. `requires`/`ensures`/`invariant` — see [Contracts](#contracts-requires--ensures--invariant). Runtime-checked in debug builds, stripped in release. Pure expressions only. When a contract predicate falls within the refinement type system's expressiveness (numeric comparisons, `len()`), the compiler discharges it statically — no runtime check in any build mode.

**Level 3-4 (Full formal proofs):** May never be built. SMT solver integration (Z3), quantifiers, and proofs that use `old()` references.

### Flow-Sensitive Refinement Narrowing (Restricted)

Within the `then` branch of `if x > 0 { ... }`, automatically narrow `x` to the matching refinement type (e.g., `Positive`) without requiring `Positive.try_from(x)?`. Restricted to: immutable local bindings only (not `mut`, not a parameter, not a closure capture, not reassigned), syntactic predicate match against a refinement type's constraint, single-function scope. No closure interaction, no cross-scope reasoning, no mutable rebinding. This avoids the complexity of general flow-sensitive narrowing while covering the common case of simple numeric predicates after a guard.

### Spec-First Programming

**Decision:** Depends on working pre/post conditions (contracts, gradual verification Level 3) before it is meaningful.

**Why non-breaking:** Purely additive tooling/workflow feature.

### Promote Passing Test Assertions into Contracts (`karac test --suggest-contracts`)

A `karac test` mode that, for each *passing* assertion in the test corpus, emits structured suggestions mapping the assertion expression to candidate `requires` / `ensures` clauses on the function under test. The natural inverse of derivation chains: an LLM authors a test, the compiler proposes a contract, the next build either statically discharges the contract (free) or surfaces it as a runtime check (declared cost). This is the only place in the design where test artifacts feed back into the declarative surface of the language.

Concrete example: `test_sort_preserves_length` asserting `assert_eq(sort(v).len(), v.len())` becomes a candidate `ensures(result) result.len() == v.len()` clause on `sort`. The compiler would emit:

```json
{
  "type": "suggest_contract",
  "function": "sort",
  "function_file": "src/sort.kara",
  "function_line": 14,
  "kind": "ensures",
  "predicate": "result.len() == v.len()",
  "evidence_test": "math_test::test_sort_preserves_length",
  "static_discharge": "likely",   // or "unlikely" / "uncertain"
  "derivation": [...]
}
```

**Why deferred:**

The translation/inference quality is genuinely uncertain. Three open questions block a confident commitment:

1. **Specific-vs-universal classification.** Most assertions check specific cases (`assert_eq(add(2, 3), 5)`) — not contract candidates. Some express universal claims (`assert_eq(sort(v).len(), v.len())`) — good candidates. Distinguishing these requires identifying which assertion variables are bound to function arguments vs. literal test inputs. Heuristic at best; without a Kāra test corpus, we can't calibrate the heuristic.
2. **Pre-condition inference is much harder than post-condition.** A test that happens to pass non-empty `v` to `find_min(v)` doesn't logically *require* non-empty input — the test just doesn't exercise the empty case. Inferring `requires` from "tests that happen to pass" is unsound; the spec should focus on `ensures` first.
3. **Static-discharge integration is the unique compiler value-add.** LLM clients can analyze passing tests for contract candidates from source today via prompting — but only the compiler can tell whether a candidate would be statically dischargeable (free) or would add runtime check cost on every call. That value-add depends on the maturity of static discharge.

**Promotion gates** (when to revisit):

- Static discharge handles `len` / equality / arithmetic refinements reliably (the static-discharge story works for the common assertion shapes the tool would surface).
- A corpus of ≥10 real Kāra projects with substantive test suites exists, providing calibration data for the specific-vs-universal classification heuristic.
- A prototype implementation, run against the corpus, shows an honest acceptance rate for proposed contracts — a number the prototype itself reveals, not pre-committed.

Until those gates are met, the entry stays deferred, with the gates documenting what's missing.

**Additive to the LLM TDD loop.** This is *additive* to the [`karac tdd` Watch Driver](#karac-tdd-watch-driver--unified-tdd-cycle-loop), not a prerequisite. The watch driver — together with envelope unification, the cycle-summary status taxonomy, test-selection flags, `karac test --init` scaffolding, and the signature-from-call-site stub diagnostic — ships fine without contract suggestion. The value here is the *next layer*: once the loop is humming and contracts are mature, suggestions feed test artifacts back into the declarative surface. The capstone is never blocked on this entry.

**Why non-breaking when shipped:** new `karac test --suggest-contracts` flag (default off); new JSONL `suggest_contract` event slots into the existing schema discriminator; existing consumers ignore unknown event types. No language-surface change — suggestions are advisory output, not enforced code modifications. The user (or LLM client) decides whether to accept any given suggestion.

### Auto-Derived `Arbitrary` and `Shrink` Honoring Refinements and Invariants

Extend `#[derive(Arbitrary)]` to automatically produce property-test generators *and* invariant-respecting shrinkers for types carrying refinement predicates or `invariant` blocks. Today, `#[derive(Arbitrary)]` generates fields independently — types with non-trivial constraints must hand-write `Arbitrary` and `Shrink` (per [Property Tests](#property-tests)). For LLM-driven property testing, this is the largest grunt-work tax in the test surface; the proposal is to remove it by letting one piece of source — the refinement predicate or invariant — do triple duty: type rule, contract, and test-input generator.

**Two-strategy generator:**

1. **Direct constructor** when the predicate is *structural* — the compiler recognizes a fixed catalogue of patterns it can satisfy by construction without rejection. Examples: `x > N` / `x >= N` / `x < N` / `N <= x < M` (numeric ranges → generate within the satisfying interval), `x.len() > 0` (`Vec` / `String` → generate at least one element), `s.is_ascii()` (`String` → generate from ASCII alphabet), conjunctions of recognized patterns. Output: a generator that produces only valid values, no rejection cycle.
2. **Rejection filter** when the predicate is non-structural (`is_prime(x)`, arbitrary user functions). Output: generate the underlying type, evaluate the predicate, retry on failure. Configurable bailout — abort after N rejections with a structured diagnostic (`refinement_unsatisfiable` or similar) rather than hanging indefinitely.

**Invariant-respecting shrinker.** When a property test fails on `xs: Vec[PositiveI32]`, the shrinker walks toward smaller-but-still-valid inputs. Shrinking `[5, 3, 2]` to `[5, 3, 0]` violates `PositiveI32`'s refinement — the shrinker must reject that step. For refinement types on single fields this is straightforward (filter shrink candidates through the predicate). For struct-level `invariant` blocks involving multiple fields conspiring (e.g., `start <= end`), the shrinker must either co-shrink the conspiring fields or reject shrink steps that break the invariant. Either approach works; the right choice depends on shrinking quality.

**Why deferred:**

The rejection-vs-construction split has real implications for shrinking quality and test runtime, so it needs a separate design pass before committing. Three substantive open questions:

1. **Predicate-pattern catalogue.** Which patterns should the structural-constructor recognize? Too narrow → most refinements fall back to rejection (slow). Too broad → the compiler ships a sprawling pattern matcher that's hard to maintain. The right catalogue is empirical, calibrated against real refinement usage in real Kāra programs.
2. **Invariant-aware shrinking is research territory.** No widely-deployed PBT framework (QuickCheck, Hypothesis, proptest, jqwik) has solved invariant-respecting shrinking generically. The naive "filter shrink steps through the invariant" approach can produce poor shrinking quality (the shrinker gets stuck in local minima where every step violates the invariant). Constraint-solving alternatives are more general but expensive.
3. **Bailout-default calibration.** What's the right default rejection bailout? Too low → false-negative test failures ("no inputs found"). Too high → tests hang on impossible refinements. The right default is empirically calibrated, not theoretically derivable.

**Promotion gates** (when to revisit):

- The refinement types and `invariant` blocks of this track are mature (the substrate this feature derives from is stable enough to commit to).
- A prototype implementation of structural-pattern recognition exists, with a *measured* catalogue size that handles the common cases reflected in real Kāra programs.
- An invariant-respecting shrinker prototype shows acceptable shrinking quality on a benchmark suite — the prototype itself defines the threshold, since "acceptable shrinking" depends on the corpus. Either the rejection-filter approach is empirically good enough, or a constraint-solving approach has demonstrably better quality at acceptable runtime cost.
- A corpus of ≥10 real Kāra projects with substantive refinement-typed property tests exists, providing calibration data for the bailout default and the structural-pattern catalogue.

Until those gates are met, hand-written `Arbitrary` and `Shrink` impls remain the supported path for types with non-trivial constraints — annoying but tractable.

**Additive, not blocking.** Like the contract-suggestion entry above, this is *additive* to the LLM TDD loop. Property tests with refinement-typed inputs can be written today by hand-implementing `Arbitrary` / `Shrink`; the `karac tdd` Watch Driver capstone, its sub-features (envelope unification, cycle-summary status, test-selection flags, scaffolding, signature stub), and the contract-suggestion entry all ship fine without auto-derived `Arbitrary` / `Shrink`. The value here is removing a specific grunt-work tax once the substrate is mature.

**Why non-breaking when shipped:** extension to existing `#[derive(Arbitrary)]` and a new `#[derive(Shrink)]` (or expansion of the existing derive) — purely additive derive behavior. Existing hand-written `Arbitrary` impls are unaffected (they're hand-written, not derived). New refinement-typed types that opt into the derive get the auto-generation; types that don't keep using hand-written impls.

### Machine-Verifiable Intent Annotations

Programmer states intent in a machine-checkable form beyond contracts. Depends on a verification system that doesn't exist yet. Waiting for real AI usage patterns.

### Formal Specification as Primary Artifact

The spec becomes a formally verifiable document (not just prose). Only meaningful if pre/post conditions (Level 3) land; effect annotations are a lightweight precursor. Revisit if Level 3 ships.

### Oracle Synthesis from Contracts

Automatic generation of Mend-task correctness oracles from declared contracts and refinement types, so a task's oracle stops being hand-written. Today every entry in `examples/mend/TASK_FORMAT.md` ships a human-supplied oracle answering "how to check the RESULT is correct," and that hand-authoring is what caps corpus size. Since a contract *is* an executable correctness statement, a task whose functions carry contracts already contains its own oracle; this entry is the machinery to extract and run it.

**Why this is the highest-leverage entry in this section.** The taxonomy in `TASK_FORMAT.md` names `fixed-by-karac` + oracle **FAIL** — the fix compiled but changed behavior — as the category worth hunting, and that category is only observable where an oracle exists. Automating oracles therefore does not merely enlarge the corpus; it widens the only lens that catches behavior-changing fixes, which is the failure mode a compile-only gate is structurally blind to.

**What it rests on:**
- The contracts and refinement types of this track — the source material.
- `examples/mend/TASK_FORMAT.md` — the task+oracle format and outcome taxonomy that consumes the generated oracle.
- Property-test generation, which does *not* exist: deriving inputs that exercise a contract is a separate capability from checking one.

**Why deferred, and why not a compiler feature:**
1. Contract-to-oracle is only as good as contract coverage in the corpus. Blind-authored tasks carry few contracts by construction, which is a chicken-and-egg problem needing real data to resolve.
2. Input generation is a research-shaped subproblem (property-based testing, shrinking); scoping it before contracts have real usage would design against guesses.
3. Nothing about it gates the language — the corpus works today with hand-written oracles, just at lower volume.

**Pre-build checklist (all must be done before building this):**
- [ ] Contract/refinement usage in real Kāra code substantial enough to synthesize from.
- [ ] Input-generation approach chosen (property-based generation vs. bounded exhaustive vs. symbolic).
- [ ] Measured baseline: what fraction of current hand-written oracles a synthesizer would reproduce.
- [ ] Decision on where it runs — a `karac` subcommand vs. harness-side tooling in `examples/mend/harness/`.

**Cross-reference:** `examples/mend/TASK_FORMAT.md § Oracles`.

## Effects expressiveness

Named effect variables beyond stored callbacks, parameterized resources, and finer control over how effect sets are declared, compared and audited.

### Named Effect Variables

v1 effect polymorphism needs no annotation: effects that arrive through a non-escaping function parameter, or through a trait method with no clause, are charged to the caller ([design.md §12](design.md#function-values-and-effects)). Effect variables for stored callbacks come with [M4a services](#effect-variables-for-stored-callbacks). This entry is the general named form.

**Effect variables are unification variables, not lattice elements.** A named `with E` is *not* a member of `P(Atoms)`. An effect-polymorphic SCC is analyzed once in symbolic form, with `T_f` parameterized by `E`; the variable is resolved at the first call site outside the SCC that supplies a concrete closure or impl. Resolution performs ordinary *unification*, not *join*: two call sites supplying different concrete effect sets each resolve `E` separately, and their sets are not joined into one polymorphic summary. Call sites are not re-iterated — each inherits the symbolic fixed point with `E` substituted. Instances are keyed by types only ([design.md §12](design.md#trait-methods-and-generic-calls)), so resolving `E` differently at two call sites does not create two instances.

**Effect-polymorphic SCCs.** When every function in a private SCC is parameterized by the same effect variable `E`, the SCC is polymorphic in `E`. The variable resolves at the first call site outside the SCC that provides a concrete closure or impl. The fixed-point iteration unifies `E` across the cycle — no conservative approximation needed.

```
fn f[with E](data: Data, cb: Fn(Item) -> Out with E) -> Out with E {
    map(data.items, |item| g(item, cb))
}

fn g[with E](item: Item, cb: Fn(Item) -> Out with E) -> Out with E {
    f(transform(item), cb)
}

// E resolves at the call site — both f and g get: writes(DB)
f(data, |item| write_to_db(item))
```

Effect polymorphism lets a function's effects vary based on what closures or impls it receives.

**Named effect variables — `with E`** — precise, threadable. Declared in the generic parameter list alongside type parameters using the `with` keyword:

```
fn map[T, U, with E](list: Vec[T], f: Fn(T) -> U with E) -> Vec[U] with E
fn flatmap[T, U, with E](list: Vec[T], f: Fn(T) -> Vec[U] with E) -> Vec[U] with E
```

`E` stands for "whatever concrete effects the caller provides." The variable is unified at the call site:

```
map(data, |x| write_to_db(x))   // E resolves to: writes(DB)
map(data, |x| pure_compute(x))  // E resolves to: (pure)

// E threads precisely through both combinators
flatmap(data, |x| map(x.items, |i| read_sensor(i)))  // E = reads(SensorBus)
```

**Multiple effect variables.** A function can declare more than one in the generic list:

```
fn zip_with[T, U, V, with E1, E2](
    f: Fn(T) -> V with E1,
    g: Fn(U) -> V with E2,
    xs: Vec[T],
    ys: Vec[U],
) -> Vec[V] with E1 E2    // union — consistent with reads(X) writes(Y) syntax
```

**Combining effect variables with fixed effects.** A polymorphic function may declare fixed effects in addition to whatever the closure brings. The result clause is a union of variables and concrete effects, written with the same juxtaposition syntax as `reads(X) writes(Y)`:

```
fn run_with_config[T, U, with E](
    cfg: ref Config,
    xs: Vec[T],
    f: Fn(T) -> U with E,
) -> Vec[U] with E reads(Config)    // closure's effects, plus the function's own reads
```

The fixed and variable parts compose freely; there is no precedence between them and no special syntax is required to mix them. At each call site, `E` resolves to the closure's effects and the union with `reads(Config)` becomes the call's effective effect set.

**Bounds on effect variables** are a separate entry ([Effect Variable Bounds](#effect-variable-bounds-with-e-no-writesr)). Constraints of the form `with E: no writes(R)` or `with E: subset(reads(Log) writes(Log))` are reserved syntax. Without them, polymorphic functions accept whatever effects the closure brings without an upper bound; callers that need to forbid specific effects do so by declaring a narrower effect set on their own enclosing function, which the viral annotation rule will then enforce against the call.

**Traits: effects depend on which impl is used**

```
trait Processor {
    fn process(self, data: Data) -> Result[Output, Error];
}

impl Processor for LocalProcessor {
    fn process(self, data: Data) -> Result[Output, Error] {
        Ok(compute(data))  // pure
    }
}

impl Processor for RemoteProcessor {
    fn process(self, data: Data) -> Result[Output, Error]
        with sends(Network) {
        remote_call(data)
    }
}

// Named effect variable threads the impl's effects outward
pub fn run[T: Processor, with E](p: T, data: Data) -> Result[Output, Error] with E {
    p.process(data)
}

run(LocalProcessor.new(), data);   // resolved: pure
run(RemoteProcessor.new(), data);  // resolved: sends(Network)
```

In v1 a call through a bound whose method has no clause is already charged to the instantiating caller ([design.md §12](design.md#trait-methods-and-generic-calls)). The named form makes that effect visible in `run`'s own signature.

#### Resolution order for compound polymorphism

> Instances are keyed by types only, so "monomorphization" below means resolving a call site's signature, not creating an instance per effect set. The text needs that rewrite when it returns.

When a generic function declares *both* type parameters and effect variables *and* accepts closure arguments whose types depend on those parameters — the compound case, e.g., `fn f[T: Iterator, U, with E](it: T, cb: Fn(T.Item) -> U with E) -> Vec[U] with E` — the compiler needs a well-defined order for resolving the two kinds of variables at each call site. Kāra's rule is **types first, effects second**, applied per call site, with nested calls resolved bottom-up.

**The four-step resolution algorithm.** At a call `f(a1, a2, ..., aN)` where `f` has type parameters `[T1..Tk]` and effect variables `[E1..Em]`:

1. **Constraint generation.** From each argument position, collect both type constraints (`Ti = ConcreteType`, `Tj.Assoc = U`) and effect-shape constraints (`Ei = {effects of closure arg}`, left unresolved for now). No unification happens yet — this pass only records the pairings.
2. **Type substitution and bound check.** Run the ordinary type-inference pass: resolve all `T1..Tk` from the type constraints, solve all projected types (`T.Item`, `T.Output`, etc.) to concrete types, and check every trait bound (including `where` clauses). At the end of this pass the signature is fully monomorphized with respect to types — every `Fn(T.Item) -> U with E` parameter slot is concrete except for the effect variable.
3. **Effect unification.** For each effect variable `Ei`, unify it with the inferred effect set of the closure at its argument position. Because types are fully resolved, each closure body has already been type-checked and its effect set is concrete. Effect unification is a straightforward set equation: `Ei := {effects inferred from closure body at argument position i}`.
4. **Call-site effect contribution.** Compute `f`'s effective effect set at this call site as the union of `f`'s fixed effects (if any), the method-level effects from resolved `T`-bound trait method calls inside `f`, and the resolved effect variables `E1..Em`. Add this set to the caller's inferred effect set, subject to the viral annotation rule on the caller's public signature.

**Why types-first is forced, not a preference.** The ordering is imposed by the dependency structure between the two constraint kinds:

- **Type constraints never depend on effect constraints.** Types are "above" effects in the language's dependency graph — a type is fully determined by its syntactic form and its bounds, never by the effects its values perform. Type inference can proceed without knowing any effect information.
- **Effect constraints may depend on type constraints.** A closure `|x| g(x)` at an argument position `Fn(T.Item) -> U with E` cannot be effect-checked until `T.Item` is known, because the closure's parameter `x` has type `T.Item` and the effects of `g(x)` depend on what `g` accepts. Likewise, the closure's body can only be fully type-checked (and therefore its effect set only fully inferred) after its parameter types are concrete.

These two facts together mean "effects-first, types-second" would require deferring the closure's body check until effect unification — a two-pass dance with no upside. "Types-and-effects interleaved per-parameter" would create cascading constraint dependencies that muddle error reporting. The unique consistent order is all-types-then-all-effects, which matches the dependency direction and lets each phase run to completion before the next begins.

**Multi-variable compound case.** When a function declares multiple type parameters and multiple effect variables (`fn zip_with[T, U, V, with E1, E2](...)`), all type parameters resolve together in step 2 (standard multi-variable type inference), then all effect variables resolve together in step 3. The ordering is strictly "all types before any effects," not per-parameter interleaved. This gives cleaner error reporting — a type error is surfaced before any effect unification is attempted, so the programmer never sees an effect-mismatch diagnostic that is secretly caused by an unresolved type variable upstream.

**Nested polymorphism.** When a closure argument itself calls another generic effect-polymorphic function (e.g., `outer(x, |n| inner(n, |m| write_db(m)))`), each call site is resolved independently in **bottom-up expression-typing order**. The innermost call site runs the four-step algorithm first, producing a concrete effect set for its context. That concrete set then becomes part of the enclosing closure's inferred body effects, which are used as input to the next call site up. There is no global constraint system spanning multiple call sites, and no deferred resolution.

```
// inner's E2 resolves first
// inner(n, |m| write_db(m))  →  E2 = {writes(DB)}
//                            →  inner's call-site effect contribution = {writes(DB)}
//
// then outer's E1 resolves
// outer(x, |n| <closure whose body's effects are {writes(DB)}>)
//                            →  E1 = {writes(DB)}
//                            →  outer's call-site effect contribution = {writes(DB)}
```

**Error reporting order.** Type errors at step 2 are reported before effect unification at step 3 is attempted, so the programmer debugging `f(wrong_type_arg, closure)` sees a type-mismatch diagnostic and never an effect diagnostic cascading from the type failure. If type resolution succeeds but effect unification at step 3 fails (e.g., a closure with an effect that violates a narrower downstream slot via subset-subtyping), the effect diagnostic names the slot, the closure, and the offending effect, with the signature fully monomorphized — no type variables in the error message.

**Worked example — pipeline.** A concrete walk-through of the algorithm:

```kara
fn pipeline[T: Iterator, U, with E](
    it: T,
    transform: Fn(T.Item) -> U with E,
) -> Vec[U] with E
where T.Item: Clone
{
    let mut out = Vec.new();
    for item in it { out.push(transform(item)); }
    out
}

fn log_and_transform(x: i32) -> String with reads(Log) { ... }

// Call site:
let numbers: Vec[i32] = [1, 2, 3];
let result = pipeline(numbers.iter(), |x| log_and_transform(x));
```

Step-by-step resolution:

1. **Constraint generation.** From `numbers.iter()`: `T = Iter[i32]`. From the closure: argument type `Fn(ref i32) -> String with {reads(Log)}` (inferred from `log_and_transform`'s effects), giving the constraint `Fn(T.Item) -> U with E = Fn(ref i32) -> String with {reads(Log)}`.
2. **Type substitution and bound check.** Unify `T = Iter[i32]`, so `T.Item = ref i32` (from the `Iterator` impl's associated type) and `U = String`. Check `Iter[i32]: Iterator` ✓ (the impl exists) and `ref i32: Clone` ✓ (the `where` clause). The signature is now fully concrete except for `E`: `pipeline(Iter[i32], Fn(ref i32) -> String with E) -> Vec[String] with E`.
3. **Effect unification.** The closure's inferred body effects are `{reads(Log)}` (propagated from `log_and_transform`). Unify `E := {reads(Log)}`.
4. **Call-site effect contribution.** `pipeline`'s own body effects: the `for` loop calls `it.next()` which for `Iter[i32]` is pure, and `transform(item)` which contributes `E = {reads(Log)}`. Final call-site effect = `{reads(Log)}`. This is added to the caller's inferred effect set, and the caller's public signature must declare `reads(Log)`.

Each step is independent: the type resolution in step 2 does not consult the effect state, and the effect unification in step 3 runs against a fully concrete signature. The two passes never need to iterate.

**Interaction with method-effects from trait bounds.** Inside `pipeline`'s body, calls like `it.next()` resolve through `T: Iterator`'s per-method effect declarations (via the trait method clauses of [design.md §12](design.md#trait-methods-and-generic-calls)). These method effects are a *separate* contribution to `pipeline`'s inferred effect set — they do not participate in the effect-variable unification at step 3, because they are already concrete once `T` is resolved. `pipeline`'s total inferred effect set is the union of (a) fixed effects declared in its signature (none in this example), (b) trait-method effects from any `it.method()` calls resolved via `T: Iterator`, and (c) the resolved effect variable(s) from closure arguments. The three sources compose freely and commutatively — the union is order-independent.

**Effect bounds on type parameters.** `fn run[T: Processor + writes(DB)](...)` is reserved syntax. A `T: writes(DB)` bound means "the concrete type's methods write to DB," checked at monomorphization against the impl's inferred/declared effects. Trait definitions do not change — the bound lives at the generic function, not on the trait or the impl. This enables the auto-concurrency analysis to determine effect conflicts through generic function call boundaries without waiting for full monomorphization.

### Parameterized Resources (opt-in finer granularity)

v1 keys resources by value instead ([design.md §12](design.md#value-rooted-resources)). This is the declared, parameterized form.

```
effect resource UserDB[user_id: i64];

fn update_profile(id: i64) with writes(UserDB[id]) { ... }
fn update_settings(id: i64) with writes(UserDB[id]) { ... }

update_profile(42); update_settings(42);  // same user → conflict, serialized
update_profile(42); update_settings(99);  // different users → safe, parallelized
```

The parameter is a **partition key**. The compiler proves distinctness statically when it can, and inserts a minimal runtime guard when it cannot. Silent under-serialization is never accepted — data-race-freedom is a hard guarantee, not a best-effort.

#### The alias tri-state — principle before algorithm

At every fork point where two parameterized-resource accesses are candidates for parallelization, the compiler classifies the pair into exactly one of three cases:

1. **Proven disjoint** — the compiler has evidence that the two parameters denote different partition keys (via literal comparison, algebraic distinctness, or lexical dominance analysis). The pair is free to parallelize; no runtime check emitted.
2. **Proven identical** — the compiler has evidence that the two parameters denote the same partition key (same literal after const-folding, same SSA value, same scope-stable binding). The pair is a full conflict; the scheduler serializes them.
3. **Unproven** — the compiler cannot statically decide the pair. This is assumed to alias conservatively, which means the pair is either runtime-partitioned (for pairs inside a runtime-distinguishable group, the distinctness graph lowers to a partition-by-key) or falls back to sequential execution (for pairs where even a runtime key comparison would be unsound).

Case 3 is the conservative default: *absence of proof* is never treated as *proof of absence*. A pair whose distinctness cannot be shown is always treated as potentially aliased, never as assumed-disjoint. This rule is what makes data-race-freedom a hard guarantee rather than a best-effort property.

**Classification is per-fork-point, not global.** The same pair `(UserDB[a], UserDB[b])` can land in case 1 at one call site (because both are literals at that site) and in case 3 at another (because both are computed expressions at that site). "Proven disjoint" is a property of *this use of this pair in this scope*, not a global property of the resource names or the expressions. Readers following the algorithm should not expect to memoize "these two are distinct" across scopes.

**Diagnostic guarantee.** `karac explain` always surfaces case 3 at every fork point: each unproven pair is listed with the reason it could not be classified (e.g., "both keys are loads from a mutable local; lexical dominance insufficient"), along with whether the fallback is runtime partitioning or serialization. A programmer writing a hot loop can read this report, see exactly which checks are costing them, and hoist or restructure to move pairs from case 3 into case 1. Case 1 and case 2 are summarized (counts only) rather than enumerated — the noise from listing every proven edge would bury the cases that actually need attention.

The rest of this section describes the *mechanism* that implements this principle: which static checks produce case 1, how the runtime partition-by-key handles case 3, and how the compiler keeps the unproven edge set small in practice.

**Dominance (definition).** A binding *dominates* a use iff the `let` that introduces the binding is in the same block or an enclosing block of the use site, and no reassignment of that binding occurs between the `let` and the use. This is purely lexical — the compiler does not build a CFG dominator tree. Cases that lexical dominance cannot prove fall to case 3 (runtime partition), which is always safe.

**Static distinctness.** Two resource parameters are statically distinguishable iff any of:

- Both are integer or string literals (or compile-time constants reducible to literals via const-folding), and their evaluated values differ.
- They are algebraically distinct — e.g., `id` and `id + 1`, or `id` and `id + k` where `k` is a nonzero constant.
- They are references to syntactically distinct variable names *and* the compiler can prove no aliasing is possible — specifically, one is a `let` binding that lexically dominates the use (see definition above) and the other is a parameter from a disjoint scope, so they cannot refer to the same value.

**Runtime partition guard.** For any pair the compiler cannot prove distinct, it builds a **distinctness graph** at each fork point: nodes are the parameterized-resource accesses about to be parallelized, edges connect pairs whose distinctness is unproven. The compiler then lowers the fork to a partition-by-key:

```
// conceptual lowering of a fork with N accesses of UserDB[key_i]
let classes = partition_by_key([key_1, key_2, ..., key_N]);
parallel for class in classes {
    seq for i in class { run(call[i]); }
}
```

Within each equivalence class the calls are serialized; across classes they run in parallel. This preserves data-race-freedom regardless of runtime aliasing.

**Scaling.** The runtime cost is proportional to the number of unproven edges, not to N:

- **N = 2, one unproven edge:** a single `if key1 == key2` compare. Negligible.
- **Small N (≤ 4), all edges unproven:** pairwise compares, at most `N*(N-1)/2`.
- **Large N or many unproven edges:** hash-based partition — insert keys into a small `Map[Key -> ClassId]`, O(N) amortized. One task per unique key.
- **Cost-model override:** the existing auto-concurrency cost model serializes the whole group if partition cost plus scheduling overhead exceeds expected parallel speedup. No new knob is needed.

Static pruning keeps the runtime edge set small in practice — most pairs get eliminated by literal/algebraic/scope reasoning before any runtime check is emitted.

**Key type requirements.** A parameterized resource's key type must implement `Eq` for the pairwise path and `Hash + Eq` for the hash-partition path. In practice resource keys are integers, strings, or opaque IDs that satisfy both automatically. Keys that cannot implement `Hash + Eq` (e.g., function values, closures) are rejected at resource declaration with a clear diagnostic.

**Edge cases and examples:**

- **Computed expressions:** `UserDB[ids[0]]` and `UserDB[ids[1]]` are unproven pairs — the compiler evaluates each expression once, binds the result to a temporary, and adds the temporaries to the distinctness graph. At runtime they participate in the partition like any other keys.
- **Constant folding:** `UserDB[1 + 1]` and `UserDB[2]` are the same resource statically. Const-folding runs before the distinguishability check, so `1 + 1` reduces to `2` and no runtime check is needed.
- **Branch merging:** The effect set of an `if`/`match` is the union of all arms. `UserDB[x]` appearing in both branches of an `if` is one occurrence of the same resource — no path-sensitive reasoning is applied.
- **Diagnostic surface:** `karac explain` shows, at each fork point, which pairs were proven distinct statically, which are runtime-guarded, and whether the partition uses pairwise compares or hash-based grouping. This lets the programmer see where runtime cost lives and hoist checks if a hot loop needs it.

Parameterized resources partition a resource by key, not by field: `writes(UserDB.email)` is not a form this design adds. Field-level effects are a separate entry, [for `sync` types](#field-level-effect-granularity-for-sync-struct).

**`alias` declaration.** `alias A = B;` declares that resource `A` and resource `B` are the same physical resource — operations on one conflict with operations on the other exactly as if they were the same resource. Use this when the compiler cannot detect the aliasing automatically (typically across package boundaries). Grammar: `alias PATH "=" PATH ";"`. `alias` is a module-level declaration. It can be `pub` — a pub alias exports the aliasing fact to consumers of the module. `alias` is **not symmetric**: `alias A = B` does not imply `alias B = A`; declare both if symmetric aliasing is required. The right-hand side may be a fully-qualified path from an external dependency. The declaration does not create a new resource name — it is purely a conflict-analysis directive telling the scheduler that two existing names refer to the same underlying resource.

> **Open.** `--strict-effects`, which assumed cross-module resources might alias, was removed from the design. `independent` needs its use case restated when this returns.

**Effect resource scoping.** If two libraries independently define resources that map to the same underlying system, use `alias` declarations.

**`independent` declaration.** `independent A, B;` declares that resource `A` and resource `B` are statically disjoint — operations on one can never conflict with operations on the other. Use this to override the conservative `--strict-effects` assumption that cross-module resources might alias. Grammar: `independent PATH "," PATH ";"`. `independent` is a module-level declaration; it can be `pub`. Like `alias`, it is **not symmetric** in declaration (declare both directions if needed). The compiler trusts the declaration without verification — an incorrect `independent` on resources that actually alias can produce data races. Use `independent` only when the aliasing has been manually verified. The primary use case is `--strict-effects` mode, where cross-module resources are assumed to potentially alias unless declared `independent`.

### Effect Semver Rules

The group rows depend on [effect groups](#effect-groups-and-composition), which come with M4a services only if those programs need them. These rules assume `public_effects = "declared"` (the default). Under `public_effects = "inferred"`, the public effect surface is not part of the API contract — none of the rows below are enforced by the compiler, because there is no declaration to diff against. Libraries that want semver-stable effect surfaces must keep declared mode.

| Change | Impact |
|---|---|
| Add effect to public function | **Breaking** (major) |
| Remove effect from public function | Non-breaking (minor) |
| Change private function effects | Non-breaking |
| Add transparent effect | Non-breaking |
| Add any effect to published effect group | Non-breaking (minor) — see note below |

**Group-name annotations are open-contract.** When a public function annotates with a group name (`with OrderProcessing`), it accepts that the group's effect surface may grow in minor versions. The annotation remains valid as the group evolves — no rewrite required. This is the intended way to annotate functions that operate over a logical subsystem.

**Compiler-suggested annotations prefer group names.** When the compiler emits a fix diff for a missing or incomplete effect declaration, it suggests the narrowest applicable group name rather than expanding effects individually. If no group covers the required effects, it falls back to individual effects. This is what makes the "minor" ruling mechanically consistent: callers following the compiler's guidance automatically get the open-contract annotation.

**Callers who expand group effects to individual annotations opt out of this contract.** If a caller writes `with reads(UserDB) writes(OrderDB)` instead of `with Validation`, those individual annotations are closed-contract — adding an effect to the group is breaking for them. That is their choice; the group abstraction is available but not mandatory.

**Behavioral side-effect:** Adding to a group may cause the compiler to serialize previously parallelizable calls (new effect creates a conflict with a caller's own effects). This is accepted as minor — the caller signed up for the group's full effect surface, including future growth. The semver classification is a floor, not a ceiling — a library author may always choose a major version bump if they judge the parallelization impact to be significant.

**Diagnostics for effect group expansion.** Two diagnostics make the performance consequence visible without changing the semver classification:

1. **Library-side (at publish time).** When a library author modifies a non-stable effect group, the compiler emits a note:
   ```
   note: effect group `Validation` gained `writes(Cache)`.
         This is a minor-compatible change (open contract), but may serialize
         downstream parallel code. Consider a major version bump if this group
         is used in performance-sensitive paths.
   ```

2. **Consumer-side (at upgrade time).** When a dependency update causes previously-parallel code to become sequential due to a group expansion, the compiler flags it:
   ```
   note: updating `validation-lib` from 1.2 to 1.3 added `writes(Cache)` to
         effect group `Validation`, which serializes calls in `my_handler()`
         that were previously parallel.
   ```

Neither diagnostic is an error — the code compiles and is correct. The notes ensure that performance regressions from group expansion are discovered at the point of change, not in production.

**`stable` effect groups — opting into a closed contract.** The `stable` modifier is an opt-in annotation that overrides the open-contract default described above. Without `stable`, groups may grow (minor version). With `stable`, growth is a compile error enforced on the library author. A library author can mark a group `stable` to promise that its effect set will not grow:

```
stable effect group Validation = reads(UserDB, InventoryDB) + sends(FraudService);
```

The compiler enforces this promise: adding an effect to a `stable` group is a compile error. Removing an effect or changing a non-`stable` sub-group it composes with remains allowed. The `stable` modifier is a library-facing API contract — downstream callers get the same open-contract behavior as before, but the library itself is prevented from accidentally widening the group in a minor version.

`stable` groups are the right choice when the group represents a complete, fixed contract (e.g., "this subsystem reads exactly these two databases, no more"). Non-`stable` groups remain open by default, which is the right choice for groups expected to grow as the library evolves. The distinction mirrors the open-contract / closed-annotation split described above: non-`stable` groups are the extensibility mechanism; `stable` groups are the stability promise.

#### `stable` Modifier on Effect Groups

**Decision:** Defer the `stable` annotation on effect groups (`stable effect group Name = ...;`). Only meaningful for library authors publishing packages with semver guarantees.

**Why deferred:** Until a package registry exists, there is no semver boundary to enforce. Effect groups work without the `stable` modifier. Purely additive.

**Why non-breaking:** Existing effect groups are unaffected. New opt-in annotation.

### Effect Variable Bounds (`with E: no writes(R)`)

**Decision:** Upper-bound constraints on effect variables are deferred. Unbounded `with E` is sufficient at first.

**Why deferred:** Bounds require a more complex checker (effect set subsumption, not just propagation) and add surface area to error messages. The feature is opt-in per function. No existing or planned code requires bounded effect variables.

**Why non-breaking:** Existing `with E` declarations have no bounds — equivalent to `with E: any`. Adding bounds is opt-in.

**Design shape:** See below (exclusion bounds `no writes(R)`, inclusion bounds `only reads(AuditLog)`, checked at each call site).

#### Effect Variable Bounds

**Decision:** Support upper-bound constraints on effect variables: `with E: no writes(R)`.

**Why deferred:** Unbounded effect variables (`with E`) cover the current systems coding use cases. Bounds require a more complex checker (effect set subsumption, not just propagation) and add surface area to error messages.

**Why non-breaking:** Existing `[with E]` declarations have no bounds — they are equivalent to `[with E: any]`. Adding a bound is opt-in per function.

**Design shape:**

```kara
// E must not include any writes effects
fn safe_transform[T, with E: no writes(_)](data: T, f: Fn(T) -> T with E) -> T with E

// E must be a subset of a specific effect set
fn audit_safe[T, with E: only reads(AuditLog)](f: Fn(T) -> T with E, x: T) -> T with E
```

Bounds use `no <effect-expr>` (exclusion) and `only <effect-expr>` (inclusion). Multiple bounds are ANDed:

```kara
fn controlled[T, with E: no writes(_), no sends(_)](f: Fn(T) -> T with E, x: T) -> T with E
```

Checked at each call site — the concrete effect set provided by the caller is verified against the bound. Violations are compile errors at the call site.

**`where` covers type bounds only.** Effect variable bounds (`with E: no writes(R)`) use inline syntax on the effect variable declaration: `[with E: no writes(R)]`, not the `where` clause.

<a id="field-level-effect-granularity-for-sync-struct-v15"></a>
### Field-Level Effect Granularity for `sync struct`

> **Open.** This entry assumes that all `mut` field accesses on a `sync struct` attribute to one `writes(T_resource)` effect for the containing type. In v1 a `sync` type keeps its mutable state in `Mutex` and `Atomic` fields, which are never written `mut` ([design.md §11](design.md#sync-types)), and atomics are effect-free. Whether the premise survives must be checked when this returns.

**Decision:** Per-field synthetic effect resources (`writes(Elevator.stops)` vs. `reads(Elevator.floor)`) for `sync struct` types. The conservative model attributes all `mut` field accesses on a `sync struct` to a single `writes(T_resource)` effect for the containing type. This is conservative: a method that reads `self.floor` and a method that writes `self.stops` both attribute to the same resource, serializing them in a `par {}` region even though they access independent fields. For large `sync struct` types with logically independent subsystems (config, metrics, queue, cache), the conservative model is safe but may serialize work unnecessarily.

**Why deferred:** The effect system tracks resources at binding granularity, not field granularity. Extending it to field-level requires a non-trivial rework of how synthetic resources are generated and unified.

**Why non-breaking:** The conservative model is always safe; per-field granularity is a precision improvement that reduces unnecessary serialization in `par {}` regions. Existing effect signatures remain valid; no new keyword or syntax is required.

**Mitigation until then:** Split logically independent subsystems into separate `sync struct` types, each with its own effect resource. This is the structurally correct long-term design anyway — the per-field optimization makes the merged form competitive without requiring the split.

<a id="panic-recovery-catch_panic-and-processexit-interaction"></a>
### Process Exit as an Effect (`exits`)

v1's `process.exit(code: i32) -> Never` declares no effects and exists on every target. It flushes standard output and standard error, runs no `Drop`, `defer` or `errdefer`, and may be called anywhere ([library/io.md](library/io.md#processes)).

**Possible addition:** `exits` as a separate built-in effect (alongside `panics`). Private functions would get it inferred automatically; public functions would gain `with exits` in their signature. If the distinction between "unexpected termination" (`panics`) and "intentional exit" (`exits`) proves useful in practice — e.g., for linting, or for tooling that wants to find all exit points — carve `exits` out then.

**Why non-breaking:** only if `exits` is never added to the declared sets of existing public functions automatically; adding an effect to a public function is otherwise breaking under [Effect Semver Rules](#effect-semver-rules).

### Package Manifest Capability Declarations

A package manifest field declaring the transitive effect set a library's public API requires (e.g., `capabilities = ["reads(FileSystem)", "sends(Network)"]`). The package manager flags when a dependency adds a capability to its declared set in a minor version — effectively a semver-visible permissions change. Covers the supply-chain vector where a dependency silently gains a new effect (a previously-pure formatter begins reading `Env`, or a logger begins sending to `Network`).

**Current lean:** deferred. The effect system makes capability-transitive-requirements visible *per function*; lifting that to the package manifest is tooling that builds on the language feature. [Effect Semver Rules](#effect-semver-rules) already covers the per-function semver classification this would aggregate.

**Why non-breaking later:** purely additive. Manifests without the field are unconstrained; manifests with the field gain the check. Compiler and package manager cooperate — the compiler verifies the manifest against inferred/declared effects; the package manager enforces the change-in-minor-version rule at dependency resolution.

**Re-evaluation triggers (any one of):**

1. Kāra ecosystem grows enough that dependency auditing becomes a real user concern.
2. A supply-chain incident (in Kāra or an adjacent ecosystem) surfaces a concrete gap between per-function effect declaration and package-manifest-level policy.

**Cross-reference:** [design.md §12](design.md#12-effects) — the language foundation; [Effect Semver Rules](#effect-semver-rules) — the per-function treatment this lifts to packages.

### Effect Diff Tooling for Cross-Version `panics` Surfacing

A build-side tool that diffs a library's effect surface across two versions and flags any function that gained `panics` as a candidate for major-version bump (panics are observable, so a minor release adding them to a previously-panic-free function is in principle a semver break). [Effect Semver Rules](#effect-semver-rules) classify "adding an effect" as breaking; the tooling surfaces *which* effect was added and highlights `panics` specifically because its security and reliability implications are different from (say) `writes(Cache)`.

**Current lean:** deferred. The effect semver classification comes first; standalone diffing tooling is additive and more valuable once an ecosystem exists to diff against.

**Why non-breaking later:** entirely tooling — no language change required. Existing effect declarations feed directly into the diff.

**Re-evaluation triggers (any one of):**

1. Kāra package registry ships and dependency-version-upgrade audits become a user concern.
2. A Kāra library publishes a minor version that silently added `panics` and breaks downstream users, surfacing a concrete need for the tool.

**Cross-reference:** [Effect Semver Rules](#effect-semver-rules) — the classification this builds on.

### Effect-Row Verbosity Audit

Whether Kāra forces `with ...` declarations in places where the user would reasonably expect implicit propagation — e.g., inside a generic bound that already restricts what effects a type parameter's impls can carry, across trait-method boundaries that inherit the trait's ceiling, or on closures passed to effect-polymorphic adaptors.

**How to resolve:** pick 3–5 representative programs from `design_studies/` and `examples/`, count every `with ...` clause, and ask whether removing it would (a) produce a useful diagnostic at a reasonable distance (same fn body) or (b) hide a real cost from the call site. If every `with` earns its presence, close as "Kāra is already where it should be." If one or more feel like pure ceremony, open a focused design item to relax that case.

**Why deferred:** the audit itself is bounded (~30–60 min of careful reading), but it produces a useful decision only once representative programs exist. Current `design_studies/` and `examples/` are spec-illustration sized, not application sized. Revisit once application-sized example programs accumulate.

**Why non-breaking:** if the audit surfaces a simplification, the change would relax a current requirement (fewer declarations required in some position) — purely additive in the backward-compatible direction.

## Interactive

The REPL, notebooks and the playground.

### REPL (`karac repl`)

**Decision:** Defer the interactive REPL. `karac run` for executing `.kara` files and `karac check` for type-checking are the critical CLI tools.

**Why deferred:** A REPL requires significant additional infrastructure (incremental compilation, state persistence, expression-vs-statement disambiguation) that is orthogonal to the compiler pipeline. No test or example depends on REPL availability.

**Why non-breaking:** Purely additive CLI feature.

### Interactive Evaluation Model

The interactive surfaces will be re-platformed on the MIR interpreter. The text below describes them on the LLJIT execution backend ([design.md §16](design.md#execution-model)).

Kāra ships interactive evaluation as a first-class delivery alongside the compiler. The execution backend backs three surfaces:

- **`karac repl`**. Terminal REPL binary. Line-based with multi-line continuation; persistent session; meta-commands `:help`, `:quit`, `:type expr`, `:effects`, `:save file.kara`, `:provide R = expr` / `:end-provide R` (see *Cross-Cell Providers*), `:dep name = "..."` (see *Session Dependencies*).
- **Browser playground**. Zero-install entry point hosting the interpreter behind a web frontend. No Python, no install, no IDE — intended as the common first-try surface.
- **Jupyter kernel**. `jupyter_client` protocol compliance, distributed as `pip install karac-kernel`. Notebook-native rendering for effects, ownership, and rich values. Scheduled to ship once the stdlib is stable enough that a first-time notebook user does not hit "function not found" on common types.

The `.kara` file remains the authoritative source format. The interactive surfaces execute the same language — not a dialect — and guarantee round-trip fidelity with saved `.kara` files (see *Session Export* below).

#### Cell Scope

A cell is a fragment of Kāra submitted to the interpreter as a single unit. Cells execute against one flat top-level scope that accumulates across the session:

- `let`, `fn`, `struct`, `enum`, `trait`, `impl`, and `import` declarations from prior cells remain visible in later cells.
- Re-declaring a name shadows the earlier binding. This applies to **types** as well: a later cell's `struct Point { ... }` shadows the earlier `Point`. Values typed under the older definition become unreachable by name; using them surfaces a type error identifying the superseding cell ("`Point` was redefined in cell N; the value here was created against the cell M definition").

The session's accumulated content behaves as the body of an implicit `fn main() -> Result[(), AnyError] { ... }`:

- The `?` operator is legal in any cell.
- The session's implicit return type is `Result[(), AnyError]`, matching a `main()` that uses `?`.
- Effects accumulate on this implicit `main` — see *Effect Semantics* below.

#### Ownership Across Cells

Ownership and move semantics operate on the session's accumulating history, not on the cell boundary. A value consumed in cell *N* is gone in cell *N+1*:

```kara
// cell 3
let v = Vec.new();
v.push(1);
consume_vec(v);  // moves v

// cell 4
print(v.len());  // use-after-move error
```

The REPL **rejects the cell** for this, and renders it with a **notebook-aware hint** rather than the bare compiler diagnostic:

```
ownership error: value `v` moved here, used again here (E0500)
  cell 4, line 1: print(v.len())
                        ^
  note: `v` was consumed in cell 3 by `consume_vec(v)`
  hint: to keep `v` live across the move, clone at the call site:
          consume_vec(v.clone())
```

No implicit clones.

The REPL refuses the cell and rolls it back out of history, so the moved-from binding never becomes readable-by-accident mid-session; `karac repl --auto-clone` is the opt-in escape.

**Opt-in auto-clone mode.** Users who prefer Python-like prototyping ergonomics may enable `karac repl --auto-clone` (flag) or `%set auto-clone on` (Jupyter magic). In this mode, the interpreter inserts `.clone()` at the consume site when the consumed binding is referenced again in a later cell, and emits a `perf[auto-clone-in-repl]` note identifying the inserted clone. Auto-clone is never silent, and the inserted clones appear verbatim in the session export so that saved files compile without modification.

#### Effect Semantics

Effects are tracked and surfaced at two granularities:

- **Per-cell.** Each cell's effect set is displayed in its output footer (e.g., `reads(FileSystem), writes(Stdout)`). This is the immediate feedback loop — users see what the cell touched the moment it runs.
- **Session-wide.** The union of all cells' effects is the session's effect set, accessible via the `%effects` magic (Jupyter) or `:effects` meta-command (REPL). This is what the cell history would require as declared effects if saved as a `.kara` file's `main()`.

Effects are a language differentiator that conventional REPLs (e.g., Python's) cannot surface; exposing them per-cell and session-wide is a load-bearing part of the interactive value proposition.

Cross-cell effect conflict detection (e.g., "cell 3 reads a file cell 2 wrote") is not a correctness concern — cells execute sequentially by definition — but may be surfaced as a teaching / timeline visualization in a future release.

#### Cross-Cell Providers

Provider injection ([Provider-Rooted Resources](#provider-rooted-resources-trait-based-injection)) is strictly scoped: `with_provider[R](p, || body)` opens a scope, `body` runs, the provider tears down at the closing brace. The interactive surface needs the opposite shape — set up a fake `UserDB` in cell 1, run analysis cells 2–10 against it, swap to a real `UserDB` in cell 11, continue. This is a load-bearing data-science workflow ("connect once, analyze for an hour") and has no syntactic equivalent in the file form because file code does not have cells.

The interactive surfaces resolve this with two meta-commands that compile, in the saved session, to ordinary `with_provider` blocks:

```text
:provide DB = TestDB.new()       # REPL meta-command
:end-provide DB
%provide DB = TestDB.new()       # Jupyter cell magic — same semantics
%end-provide DB
```

Between `:provide` and `:end-provide` (or `%provide` / `%end-provide`), every cell executes inside the corresponding `with_provider` scope. The `:provide` form takes a resource binding `R = expr` where `expr` evaluates to a value implementing `R.Provider`.

**Saved-session form.** A `:provide`/`:end-provide` pair in the cell history compiles as a single `with_provider[R](expr, || { /* cells in scope */ })` block in the exported `.kara` file. Cell boundaries do not survive export — they are presentation, not semantics — so every statement that ran inside the provide scope becomes a statement inside the closure body.

```text
:provide DB = TestDB.new()
let users = DB.query("SELECT id FROM users")
let count = users.len()
:end-provide DB
```

→

```kara
with_provider[DB](TestDB.new(), || {
    let users = DB.query("SELECT id FROM users");
    let count = users.len();
})
```

**Binding-scope restriction (the trade-off, named).** Bindings declared between `:provide` and `:end-provide` are visible only within that scope — both at the saved-session form (scoped to the closure body, by ordinary block semantics) and at the live REPL surface (where the REPL projects the same scoping). After `:end-provide DB`, attempting to reference `users` or `count` from the example above fails with a structured diagnostic that names the provider scope that closed:

```
error[E0425]: cannot find value `users` in this scope
  cell 7, line 1: print(users.len())
                        ^^^^^
  note: `users` was declared inside `:provide DB` (cell 3) and went out of
        scope when `:end-provide DB` ran in cell 6
  hint: extract `users` to an outer binding before `:end-provide`, or delay
        `:end-provide` until you no longer need bindings from this scope
```

This is the price of consistency with the file form. The REPL does not implicitly hoist bindings out of provider scopes — that would be a divergent dialect. Users who need `users` to outlive the provide scope either (a) delay `:end-provide` until the analysis is done, (b) write the value to an external sink (file, separate global) before closing, or (c) stop using `:provide` for that workflow and inline `with_provider` blocks in the saved form directly.

**Closure capture is the same case.** A closure that captures a provider-resource binding is itself a binding inside the provide scope; if the closure escapes the scope (assigned to a name referenced after `:end-provide`), the saved form is a closure escape error and the REPL surfaces the same error eagerly when the offending cell runs:

```text
:provide DB = TestDB.new()
let handler = || DB.query("SELECT 1")     # closure captures DB
:end-provide DB
handler()                                  # → escape error
```

Same diagnostic shape as the binding case above; no special closure-capture rule is needed.

**Nested provides.** `:provide A = ...` inside an outer `:provide B = ...` works the same way as nested `with_provider` blocks in file code — the inner provider shadows for resource `A`, the outer shadows for `B`. `:end-provide` closes the **innermost** matching scope; the REPL rejects out-of-order closes with `error: :end-provide DB attempts to close an outer scope while :provide A is still active; close A first`.

**No-provider error inside `:provide`.** A resource call that resolves to no active provider raises the same runtime panic as in file code ([Provider-Rooted Resources](#provider-rooted-resources-trait-based-injection)), with the cell number named in the diagnostic. The provider stack the REPL maintains across cells is the same per-task stack the runtime uses everywhere else; `:provide` only changes who pushes to it.

**Jupyter widget — later.** A richer widget UI (active-provider panel with swap buttons, per-cell-effect timeline showing which provider was bound when) is a stretch goal for after the first notebook release. The first release's magic (`%provide` / `%end-provide`) is parity with the REPL meta-commands and uses the same compilation path; the widget is presentation only and adds no new semantics.

#### Session Dependencies

Manifest-discovery semantics for `karac repl` and `karac run` are documented under *REPL and script dependency scope* below. This subsection covers the in-session counterpart: the REPL meta-command for adding dependencies on the fly without editing the manifest.

**`:dep` meta-command.** `:dep name = "1.2"` adds a package to the current session's in-memory manifest. The accepted right-hand side is the same shape as the right-hand side of a `[dependencies]` entry in `kara.toml` — a bare semver string, an inline table with `version = ...` / `git = ...` / `path = ...`, etc.

```text
:dep http = "1.2"
:dep myutil = { git = "https://github.com/me/myutil-kara" }
let response = http.get("https://example.com")?
```

After `:dep` succeeds, the package's surface is in scope for subsequent cells exactly as if it had been declared in the project's `kara.toml`. Resolution and download flow through the registry proxy and obey the same `kara.lock` rules as a normal build. Failures (resolution conflict, network error, missing package) surface in the cell output and leave session state unchanged.

**State is in-memory only.** When the session ends, deps added via `:dep` are gone. Symmetric with `:provide`: both are session-scoped state mutations with no implicit persistence. If long-lived REPL session resumption becomes a felt need, it lands as an explicit `:save-session` / `:load-session` snapshot mechanism, not via implicit persistence of `:dep` state.

**Jupyter parity.** `:dep` works in Jupyter via the existing kernel meta-command channel — no Jupyter-specific work needed. A `%dep` cell-magic alias (matching `%pip` muscle memory) is a later comfort feature; pure aliasing, identical semantics. A manifest-editor widget is deferred indefinitely.

**Interaction with `:provide`.** Independent state mutations. Neither invalidates the other. If `:dep` swaps out a package whose types were referenced by an active `:provide` registration, the user gets a normal type error when they next try to use that provider — no special cross-reference tracking. Diagnostic: standard "type X not found" with the location pointing at the `:provide` site.

#### Session Export

`%save session.kara` (Jupyter magic) or `:save session.kara` (REPL meta-command) writes the session's cell history to a single `.kara` file. The file wraps the history in `fn main() -> Result[(), AnyError]` with declared effects matching the session's accumulated effect set. If auto-clone mode was enabled, the inserted `.clone()` calls appear in the exported file verbatim.

**Guarantee.** A saved session file compiles with `karac build` and produces identical observable behavior when run. The interactive surface is a faster path to the same language, not a divergent dialect.

#### Rich Output Display Protocol

**Purpose.** When the Jupyter kernel evaluates a cell whose final expression implements `RichDisplay`, it emits a MIME-typed payload to JupyterLab / VS Code Notebooks / Colab rather than falling back to plain `Debug` text. This is the protocol that plotting libraries, DataFrame renderers, and tensor visualizers implement to show charts and tables inline in notebooks.

**`RichDisplay` trait (stdlib, `std.display`):**

```kara
trait RichDisplay {
    // Keys are MIME types; values are content strings (base64 for binary MIME types).
    // "text/plain" is required — it is the terminal and fallback representation.
    fn rich_display(ref self) -> Map[String, String];
}
```

Supported MIME types and their conventions:

| MIME type | Content | Use case |
|---|---|---|
| `text/plain` | UTF-8 text | Required fallback; truncated ASCII table for DataFrames, shape/dtype for Tensors |
| `text/html` | HTML string | DataFrame tables, styled output |
| `image/png` | Base64-encoded PNG bytes | Plots, heatmaps, images |
| `image/svg+xml` | SVG string | Vector plots, diagrams |
| `application/json` | JSON string | Machine-readable structured data |

The kernel picks the richest MIME type the frontend supports; `text/plain` is always emitted as the fallback.

**Auto-display rule.** In a Jupyter cell, the last expression is displayed:
- If the type implements `RichDisplay` → via `rich_display()`
- Otherwise → via `Debug` (wrapped as `text/plain`)
- `Unit` / `()` → no output (cell run for side effects)

**Library contract.** Any library that wants inline rendering implements `RichDisplay` on its output types. A plotting library returns `"image/png"` (base64 PNG) or `"image/svg+xml"`. A `DataFrame` renderer returns `"text/html"` (an HTML table) plus `"text/plain"` (a truncated ASCII table). A `Tensor` renderer returns shape/dtype in `"text/plain"` and optionally a heatmap in `"image/png"`.

**REPL.** The REPL always uses `Debug` — terminal-only. `RichDisplay` is a Jupyter-only concern; the trait is importable but the REPL ignores it.

#### REPL and script dependency scope

**REPL and script dependency scope.** The `karac repl` and `karac run <script>` entry points discover the dependency graph from filesystem context, not from cwd:

- **`karac repl`** walks upward from the *current working directory* looking for `kara.toml`. If found, the project's deps are in scope. If not, the REPL has stdlib only.
- **`karac run path/to/script.kara`** walks upward from the *script's own directory*, not the cwd. A script under a project tree gets project deps regardless of where it is invoked from; a script outside any project tree (e.g., `/tmp/foo.kara`) has stdlib only, even if invoked from inside another project's directory. This is intentional — the script's filesystem location is the stable identity; making scope depend on cwd would mean the same script behaves differently based on where you happen to be standing.
- **Overrides.** `--manifest path/to/kara.toml` forces a specific manifest. `--no-manifest` forces stdlib-only.

The companion REPL meta-command for ad-hoc deps (`:dep name = "1.2"`) is documented under *Session Dependencies* above. `karac repl` joins the CLI's subcommands when this track lands.

v1's script mode ([design.md §10](design.md#script-mode)) synthesizes `fn main()` for a file of top-level statements, which aligns the file surface with the REPL's cell-as-main-body model.

### Rust ↔ Kāra Web Playground

A browser-hosted UI where a user pastes Rust on the left and sees Kāra on the right — and vice versa. No install, no account, no commitment. Evaluates the language against the user's own code in ~30 seconds.

**Why it's a separate entry rather than bundled with the transpiler:** the playground is a distinct engineering investment — a UI over the transpiler, not part of the transpiler itself — and it has different prerequisites (front-end UI framework, WASM codegen so the transpiler can run client-side, or a server-hosted transpile endpoint).

**What it rests on:**
- [Rust ↔ Kāra Bidirectional Transpiler](#rust--kāra-bidirectional-transpiler) — the playground is a UI over its transforms.
- [Frontend UI Framework](#frontend-ui-framework) — the playground is a UI; if the Kāra-built-frontend slot isn't filled, the playground ships on an existing framework (React, Solid, or similar via `host fn` bindings) as a pragmatic shortcut.
- The browser target of the [web track](#web) — if the transpiler runs client-side. If it runs server-side instead (paste-and-POST), the browser target is not required for the playground itself, only the transpile-to-browser path.
- Output quality from the transpiler mature enough not to embarrass the project.

**Why deferred, and why not a library or compiler feature:**
1. Best deployed once there's a 1.0 to point people at — otherwise the playground shows off an unfinished language and a half-baked transpiler.
2. The playground IS the marketing for the adoption mechanism. Shipping it before the language is coherent undermines the positioning.
3. Costs nothing incremental *after* the transpiler ships — but that's a "then," not a "now."

**Pre-build checklist (all must be done before building this):**
- [ ] Bidirectional transpiler mature in both directions with output quality credible for public exposure.
- [ ] Kāra 1.0 shipped.
- [ ] Transpile execution model decided (client-side WASM vs. server-side endpoint).
- [ ] If client-side: the browser target shipped.
- [ ] Landing-page / positioning copy aligned with the peer-language framing (not "Kāra is Rust's Kotlin").

**Cross-reference:** [Rust ↔ Kāra Bidirectional Transpiler](#rust--kāra-bidirectional-transpiler) — hard prerequisite.

### Compiler-Decision Explorer (hosted)

A public, no-install web surface where a visitor pastes Kāra and sees what the compiler *decided* — the `karac query` output for effects, ownership, concurrency, and cost-summary — rather than the emitted assembly. Compiler Explorer's shape aimed at semantic decisions instead of codegen. The pitch it serves is the one the README leads with ("don't take the concurrency on faith — ask the compiler what it did"), made clickable.

**Distinct from Cartographer** (`examples/cartographer/`), a *dogfooding demo*: whole-program `karac query effects`/`concurrency` plus a live WASM studio (D3 + Monaco, compiler-in-browser). Cartographer proves the capability on a curated program. This entry is the productized surface — arbitrary pasted code, durable hosting, shareable permalinks, and the full query set rather than the effect graph alone. The engineering delta is deployment and input-hardening, not analysis.

**What it rests on:**
- `karac query` ([design.md §17](design.md#compiler-query-api)) — the whole surface being displayed; `effects`, `ownership`, `concurrency`, `cost-summary`, `monomorphization`, `attributes`.
- WASM codegen + `playground/web` — the compiler-in-browser path, so pastes never leave the client.
- `examples/cartographer/`'s WASM studio — the working precedent for Monaco-plus-compiler-in-browser; the viewer work is largely reusable.

**Why deferred, and why not a compiler feature:**
1. It is hosting and operations, not compilation — a durable public endpoint with abuse handling is a different commitment from shipping a binary.
2. Pointing the public at it before 1.0 advertises whatever is half-finished that week; the value is highest against a stable language.
3. Untrusted paste input needs a hardened story (resource caps, pathological-program timeouts) that no local `karac` invocation currently needs.

**Pre-build checklist (all must be done before building this):**
- [ ] Kāra 1.0 shipped.
- [ ] `karac query` surface frozen enough that permalinks stay meaningful across releases.
- [ ] Compile-in-browser resource caps decided (timeout, memory ceiling, program-size limit).
- [ ] Reuse-vs-rewrite call made on Cartographer's studio front-end.

**Cross-reference:** `examples/cartographer/` (the demo this productizes); [Rust ↔ Kāra Web Playground](#rust--kāra-web-playground) (a sibling browser surface with the same hosting prerequisites; if both are built, they should share infrastructure).

### Compiled-Speed Notebook Product

A hosted notebook environment on the `karac` Jupyter kernel, positioned on **execution-model parity**. A notebook cell exhibits the same effect, ownership, and performance behavior as a `karac build` artifact, because it is the same LLJIT path — unlike `evcxr`-style recompile-per-cell or JShell's JVM startup tax. The target user is doing numerics or data preprocessing at systems speed and currently pays a Python-to-compiled-language rewrite to ship it.

**What it rests on:**
- `kernel/` (Rust kernel, ZMQ transport) + `kernel/python/karac_kernel` — the kernel exists.
- One execution backend for `karac run`, `repl` and `test` — parity is the whole pitch, and it is a property of that decision.
- The [M4c data](#m4c-data) packages — notebook users expect numeric and data-frame breadth that v1's library does not provide.

**Why deferred, and why not a compiler feature:**
1. The kernel is the compiler's concern; hosting, auth, persistence, and per-user sandboxing are not.
2. Notebook users arrive expecting a numeric/plotting ecosystem. Shipping the environment before the data track sets up a bad first impression against a language that is otherwise strong here.
3. The honest framing — "the REPL has built-in compile latency by design," ~100 ms for trivial cells — needs to be stated up front, and lands much better next to a stdlib that makes the tradeoff worth it.

**Pre-build checklist (all must be done before building this):**
- [ ] M4c data packages shipped (numeric + data breadth).
- [ ] Per-cell latency measured and published honestly, including the trivial-cell floor.
- [ ] Multi-tenant sandboxing story decided (a JIT executing arbitrary user code per tenant is the whole security surface).
- [ ] Plotting/visualization answer chosen — own it, or bridge to an existing stack.

## Web

Browser glue and the component model: the `wasm_browser` target, host bindings and a frontend framework.

### Targets and resources

#### Cross-target Compilation

A Kāra source tree often compiles to more than one target in a single build — the canonical case is server-side rendering (SSR) where a `components` module compiles for both `native` (server renders initial HTML) and `wasm_browser` (client hydrates and handles events). This subsection specifies how the compiler decides which items exist on which target and how the effect system enforces target correctness without `#[cfg]`-spam.

This track adds `wasm_browser` to v1's closed target set ([design.md §16](design.md#the-v1-target-set)); the [GPU](#gpu) track adds `gpu`:

| Target | Purpose |
|---|---|
| `wasm_browser` | WASM module intended for browser runtimes. Generated JS glue, wasm-bindgen-compatible calling convention. |
| `gpu` | GPU kernel subset — SPIR-V / WGSL via the `#[gpu]` constraint declaration (see [GPU Subset Constraints](#gpu-subset-constraints)). |

Opening the set to user-defined targets (custom embedded, FPGA, a future application runtime) is additive and does not break existing code.

#### Target-Provided Resource Sets

This is v1's table ([design.md §16](design.md#resources-each-target-provides)) with the `wasm_browser` and `gpu` columns added. Each target provides a specific subset of the built-in primitive resources. A function whose inferred effect set references a resource the target does not provide is rejected when compiling for that target.

| Resource | `native` | `wasm_browser` | `wasm_wasi` | `gpu` |
|---|---|---|---|---|
| `FileSystem` | ✓ | — | ✓ | — |
| `Stdin` / `Stdout` / `Stderr` | ✓ | — (use `Console` from `std.web`) | ✓ | — |
| `Env` | ✓ | — | ✓ (limited) | — |
| `Network` | ✓ | ✓ (via `fetch`) | ✓ | — |
| `Clock` | ✓ | ✓ | ✓ | — |
| `RandomSource` | ✓ | ✓ | ✓ | — |
| `Heap` | ✓ | ✓ | ✓ | — (GPU is heap-forbidden) |
| `Hardware` | ✓ (kernel / driver builds) | — | — | — |
| `ProcessTable` | ✓ | — | — | — |
| `GpuBuffer[_]` | ✓ (host side of dispatch) | — | — | ✓ |
| `Display` / `Storage` / `Console` / `Timer` / `Input` (from `std.web`) | — | ✓ | — | — |

User-defined resources have no intrinsic target affinity — they exist wherever a provider for them exists. A resource like `UserDB` typically has a `PostgresUserDB` provider on `native` and a `JsonRpcUserDB` provider on `wasm_browser`; the target gate does not see the user resource directly, only the transitively-reached primitives.

#### Provider Injection for SSR-shared Code (primary pattern)

For SSR components that must run on both `native` (server rendering) and `wasm_browser` (client hydration), the Kāra-shaped pattern is **not** to partition the component with `#[cfg(target = ...)]` attributes. The component is an ordinary target-agnostic function; **different I/O providers are bound on different targets** via the existing `with_provider` mechanism (see [Provider-Rooted Resources](#provider-rooted-resources-trait-based-injection)).

```kara
// Target-agnostic component. Declares effect against a user resource.
pub fn user_profile(user_id: UserId) -> Result[Html, Error]
    with reads(UserStore) writes(Display)
{
    let user = store.load(user_id)?;
    render_profile(user)
}

// Server entry point. Binds PostgresUserStore for `native`.
#[cfg(target = "native")]
fn main() -> Result[(), AppError] {
    providers {
        UserStore => PostgresUserStore.connect(env.var("DATABASE_URL")?)?,
        Display   => HtmlStringBuilder.new(),
    } in {
        serve_ssr(user_profile)
    }
}

// Client entry point. Binds JsonRpcUserStore for `wasm_browser`.
#[cfg(target = "wasm_browser")]
pub fn hydrate(user_id: UserId) -> Result[(), AppError] {
    providers {
        UserStore => JsonRpcUserStore.new("/api"),
        Display   => DomMutator.root(),
    } in {
        user_profile(user_id)
    }
}
```

The component `user_profile` has one implementation. Each target binds the providers its platform can satisfy. This keeps shared code annotation-free and gives the programmer type-safe I/O routing per target — no `#[cfg]` chains in the component body.

**Worked example.** The `ssr_counter` example shows this pattern end-to-end — one shared component rendered to HTML on `native` and to the live DOM on `wasm_browser`, with `#[cfg(target = ...)]` only on the two entry points. It uses nested `with_provider` blocks, a user-defined render-sink resource, and DOM mutation through `host fn`s in place of the `providers { } in { }` block and the `std.web` `Display` provider.

### Web / Host Effect Vocabulary

Browser and host-runtime APIs live behind effect resources **declared in stdlib modules**, not language primitives. They use the same `effect resource` form any user-defined resource uses — the stdlib blesses a canonical set so every WASM-targeted library shares one vocabulary.

**Naming principle: capability over host.** Where a capability exists across multiple hosts (browser display surface, a native GUI toolkit, a hypothetical future application runtime), Kāra names the *capability*, not the host. A UI library writes `writes(Display)`, not `writes(DOM)`. This keeps the same Kāra source usable against any display-providing host and keeps `std.web` a cohesive module rather than a browser-historical name grab. The principle is language-quality, not platform-speculation — it applies whether or not non-browser WASM hosts materialize.

**`std.web` — browser-family hosts.** Declares:

| Resource | Purpose | Verbs |
|---|---|---|
| `Display` | Display-surface mutation and read-back (DOM nodes, innerText, bounding rects, computed style) | `reads`, `writes` |
| `Storage` | Persistent key-value storage (localStorage, sessionStorage, IndexedDB) | `reads`, `writes` |
| `Console` | Host diagnostic channel (`console.log`, `console.error`) | `writes` |
| `Timer` | Scheduled callbacks (`setTimeout`, `setInterval`, `requestAnimationFrame`) | `reads`, `writes` |
| `Input` | User input sources (keyboard, pointer, gamepad, touch) | `reads` |

**Reuse over invention.** Where a host API is semantically identical to a built-in primitive, the stdlib reuses the primitive rather than introducing a new resource:

- `fetch(url)` is declared in `std.web.net` with effect `sends(Network) receives(Network)` — the same signature a native HTTP client would carry. No browser-specific "Fetch" resource exists.
- `performance.now()` and `Date.now()` are `reads(Clock)`, same as `std.time.now()` on native.
- `crypto.getRandomValues()` is `reads(RandomSource)`, same as `std.random` on native.

**`std.wasi` — headless WASM / server-side runtimes.** A WASM module running in wasmtime or a Component-Model host does not have `Display` or `Input`. It does have filesystem, env, and network, which are already the native primitives. `std.wasi` declares any WASI-specific capabilities as they emerge; it starts as a thin module because WASI's current surface maps directly onto Kāra's existing primitives.

**Module gating, not prelude.** Web/host resources are **not** in the prelude. Native-only code never imports `std.web` and never sees `Display`, `Storage`, etc. in its resource namespace, avoiding inference noise for server-only programs. A cross-target program (SSR + hydration) imports `std.web` only in the modules that touch browser APIs; shared business logic imports neither and stays target-agnostic by construction.

**Effect-driven target gating.** A function with effect `writes(Display)` cannot compile to native (no provider for `Display`); a function with `writes(FileSystem)` cannot compile to browser-WASM (no provider there). This is how the effect system enforces target correctness without `#[cfg(target = ...)]` boilerplate. The formal rules for target-provided resource sets and cross-target shared code are spec'd in [Cross-target Compilation](#cross-target-compilation).

**Why not conflate with `FileSystem` or `Process`.** Reusing `FileSystem` for `localStorage` or `Process` for `console.log` is semantic dishonesty. A UI library declaring `writes(FileSystem)` to mean "mutates the DOM" confuses every reviewer; the cost of a small, honest vocabulary is paid once, the cost of mental translation compounds forever.

### WASM concurrency and async host APIs

For compute-bound workloads where single-thread performance is insufficient, `--features wasm-threads` opts into **shared-memory multithreading** using Web Workers + SharedArrayBuffer + atomics. This requires the user to deploy with COOP/COEP headers — a real-world constraint that belongs in the user's hands rather than as a default cost. Shared-memory multithreading is strictly additive: the same source compiles both ways; the opt-in swaps the lowering of task spawning and channels from sequential to worker-based without changing any source.

**Shape of the opt-in.** It covers `TaskGroup` and `par {}`, and it re-enables auto-parallelization, since the threaded module has a real worker pool. `karac build --target=wasm_browser --features wasm-threads` emits a **dual artifact**: the sequential module plus `<stem>.threads.wasm`, a shared-memory module built on the wasi-threads ABI. The JS glue feature-detects SharedArrayBuffer and cross-origin isolation at load: threaded when available, else a `console.warn` and the sequential module. `[wasm] fallback = false` in `kara.toml` makes that a hard error instead; both modules are always emitted, so the artifact set never depends on the deploy environment. The glue services `wasi.thread-spawn` with Web Workers (node: `worker_threads`) and runs the program's `_start` in a *primary worker* (the PROXY_TO_PTHREAD model), since `memory.atomic.wait` traps on the browser main thread. Pool size is `navigator.hardwareConcurrency`, overridable via `[wasm] pool-size`. The opt-in applies to `wasm_browser` only: wasi-threads and the component model don't compose, so `wasm_wasi` host-thread integration stays a future concern.

**`host fn` under wasm-threads** works through a **synchronous worker→main proxy**. `host fn` implementations are main-thread JS closures, but the program runs in a worker — so each call round-trips across a shared control block (a `SharedArrayBuffer`: mutex + request doorbell + `(scalar args, return)` payload slots): the worker publishes the call and `Atomics.wait`s; the main thread's service loop (`Atomics.waitAsync`, since the page agent may be non-blockable) runs the closure with the shared linear memory in scope (so `(ptr, len)` string args read directly, no copy) and notifies the result back. Uniform across the primary worker and pool workers — every worker's `kara_host` import binds to the same control block. Glue-side only; codegen's `kara_host` import surface is unchanged.

| Build shape | Task / channel lowering | User deployment action |
|---|---|---|
| `--target wasm_browser` (default) | Cooperative sequential on main thread | — |
| `--target wasm_browser --features wasm-threads` | Web Worker pool + SAB + atomics (dual artifact; auto-par re-enabled; load-time fallback to sequential) | Set COOP/COEP headers |
| `--target wasm_wasi` (default) | Cooperative sequential on main thread (host-thread integration is a wasm-threads-era concern) | — |
| `--target native` | OS threads + runtime scheduler | — |

#### Async Host APIs on WASM

Browser host APIs that return Promises (`fetch`, `crypto.subtle.digest`, IndexedDB operations) and callback-driven APIs (`setTimeout`, `requestAnimationFrame`, DOM event listeners) integrate with Kāra's concurrency primitives **without any new language surface**. The existing `suspends` effect, channel-receive, and the scheduler's yield-to-event-loop behavior cover the full use case.

**Promise-returning host APIs.** A `host fn` that wraps a Promise-returning host API declares `suspends` in its effect list. The call site looks like any other function call:

```kara
// stdlib, std.web.net
host fn fetch(url: ref String) -> Result[Response, HttpError]
    with sends(Network) receives(Network) suspends;

// user code
fn load_user(id: UserId) -> Result[User, Error] with sends(Network) receives(Network) suspends {
    let resp = net.fetch(f"/users/{id}")?;
    resp.parse_json()
}
```

No `.await`, no `.then`, no `async` keyword. Consistent with Kāra's existing "no function coloring" stance (see [design.md §1](design.md#what-kāra-is-not)): `suspends` propagates through the effect system; the scheduler handles yield bookkeeping.

**Callback-driven host APIs.** Timers, animation frames, and DOM events are modeled as **channel producers** in library wrappers on top of the `host fn` substrate. A library pattern:

```kara
// stdlib, std.web.time
pub fn after(duration: Duration) -> Receiver[()]
    with writes(Timer) allocates(Heap)
{
    let (tx, rx) = channel[()](1);
    host.set_timeout(|| tx.send(()), duration.as_ms());
    rx
}

// user code
fn delay_then(op: Fn() -> ()) with writes(Timer) allocates(Heap) {
    after(Duration.ms(500)).recv();
    op();
}
```

Event streams (clicks, keypresses, pointer moves) use the same channel shape — the library wires the DOM listener to a channel sender; the user consumes via `channel.recv()` or iteration.

**Scheduler contract on WASM.** Channel-receive on a channel whose sender is host-async yields the waiting task and returns control to the browser event loop. Host-async completion (a Promise resolving, a `setTimeout` firing, an event dispatching) wakes the waiting task by posting to the scheduler's ready queue. The source-level semantics are identical to native: the function appears to block on the receive; the scheduler implements it as a yield without source-level ceremony. **This contract is mechanism-agnostic by construction** — it constrains source-level observable behavior, not the suspension mechanism — so a target may realize it however its substrate allows, and the realizing mechanism may change without a source break.

**Realizing the contract.** On threads targets (native and `--features wasm-threads`) it is honored by thread-blocking: `recv` parks the calling thread or worker until a `send` or a channel close, and on `--features wasm-threads` the host-async `send` fires on the main-thread event loop and wakes the parked worker, so the main-thread loop is never occupied. On the **sequential default** (`wasm_browser` and `wasm_wasi` without threads), block-then-resume on a single thread needs a task-suspension mechanism (stack unwind and rewind out of wasm to the event loop and back), which is not chosen yet. Until it is, the compiler rejects host-async `recv` on a sequential target as a hard error pointing at `--features wasm-threads`, rather than letting `recv` return without waiting. The candidate mechanisms are asyncify (works everywhere, with a permanent per-binary instrumentation tax), JSPI (cleaner and host-managed, but its runtime support must be reconfirmed at decision time), WASM stack-switching, and the [full-hybrid state-machine transform](#full-hybrid-state-machine-transform-arbitrary-suspends-functions). Because the contract is mechanism-agnostic, landing any of them is a zero-source-break change.

**Why no `await` keyword.** The effect system + scheduler cover the full use case without one; adding `await` would introduce a new form that interacts with the effect system, ownership, and target-gating surface area, with no offsetting value the existing machinery does not already deliver. A future `await` has concrete re-evaluation triggers in [`await` Keyword for Async APIs](#await-keyword-for-async-apis) — the source surface is kept minimal today to avoid a locked-in keyword choice before real UI code has exercised the channel-based approach.

### Compiler-Managed Transparent Threading on WASM

Kāra's ownership system proves data-race freedom at compile time. In principle, the compiler can use that property to automatically partition a WASM program across Web Workers + SharedArrayBuffer with **zero user annotation** — a spawned task transparently becomes a cross-worker boundary without any `--features wasm-threads` flag and without any worker/postMessage code in user space. Optional layering of WASM stack-switching gives fiber-weight tasks over a small worker pool.

**Current lean:** deferred. This track ships sequential-by-default plus the `--features wasm-threads` opt-in (see [design.md §16](design.md#concurrency-across-targets)). Transparent threading is a substantial research and engineering commitment that would stall that baseline.

**Why deferred (not rejected):**

1. **The baseline needs to ship.** Getting a WASM backend working with a baseline concurrency story is prerequisite to learning what users actually need. Committing to transparent threading as the first story invites either a long delay or shipping a half-working version that poisons the differentiator claim.
2. **The differentiator claim is real.** No other language has both (a) compile-time-proven data-race freedom and (b) a first-class browser story. If the transparent-threading lowering lands correctly, Kāra says something that Rust, Go, and JavaScript cannot. That is worth doing — *after* the baseline is established.
3. **The WASM concurrency platform is still moving.** The W3C shared-everything-threads proposal may relax SAB's COOP/COEP requirement; WASM stack-switching is mid-landing in browsers. Designing against SAB today and redesigning against shared-everything-threads tomorrow is churn — a single re-evaluation after those proposals stabilize is cheaper than shipping twice.
4. **The language-level cost is already paid.** Source-level commitments already in place (task spawning and channels target-agnostic, ownership transfer through channels specified once, data-race freedom as a language property) mean the transparent-threading lowering can land non-breaking at any future point.

**Why non-breaking later:** the source commitments in [design.md §16](design.md#concurrency-across-targets) guarantee that swapping the WASM lowering from sequential-default to transparent-multi-worker is additive. The source-level surface does not change; programs that use the opt-in `--features wasm-threads` flag keep working; programs that did not opt in gain throughput without code changes when the compiler's partitioning lands.

**Re-evaluation triggers (any one of):**

1. A real user-space workload demonstrates `--features wasm-threads` opt-in is insufficient — the COOP/COEP opt-in ceremony is a deployment blocker *and* ownership-proven data-race-freedom is load-bearing for the program's correctness.
2. WASM stack-switching ships in enough browsers with enough maturity that fiber-weight tasks over a small worker pool become implementable without an outsized engineering investment.
3. The W3C shared-everything-threads proposal (or successor) lands in shipping browsers, removing the COOP/COEP friction. Design against the stabilized shape.
4. [Self-hosting](#self-hosting) reveals the Kāra compiler itself would benefit from transparent threading on WASM, giving a first-party motivating workload.

**If none of the triggers fire:** `--features wasm-threads` opt-in stays the answer indefinitely. That is a valid end state — users who need shared-memory multithreading opt in and set their deployment headers; users who don't remain on the sequential default. Kāra does not lose language-quality points for not having transparent threading.

**Cross-reference:** [design.md §16](design.md#concurrency-across-targets) — the v1 baseline.

### `await` Keyword for Async APIs

A dedicated `await` expression form for yielding a task on an async operation. Today the effect system (`suspends`) plus channel-receive semantics plus the scheduler's yield-to-event-loop behavior cover the full use case on both WASM and native — see [Async Host APIs on WASM](#async-host-apis-on-wasm). A Promise-returning host API looks like `let x = fetch(url)?;` with `suspends` inferred / declared; there is no `.await` and no `async` keyword.

**Current lean:** no `await` keyword. The existing primitives are sufficient; adding a keyword introduces new surface without replacing anything.

**Why deferred (not rejected):**

1. **The effect + channel machinery covers the functional need.** A user can write UI and networking code today (given the scheduler contract above) using channels and `suspends`. `await` would be ergonomic sugar, not a new capability.
2. **Keyword choice is a high-commitment decision.** Once `await` ships, its interaction with the effect system (does `await` require a specific effect on the expression? does it propagate something?), with ownership (does the awaited expression's ownership transfer survive the yield?), and with target gating needs to be nailed down. Committing to those answers before seeing real library shapes is a retrofit trap.
3. **Real UI code has not been written yet.** Kāra has no ecosystem. The hypothesis that channels feel awkward for UI code is untested. Shipping the web track with channels only gives users a chance to surface concrete pain points that an `await` keyword would address — or to confirm channels are fine and `await` is unnecessary.

**Why non-breaking later:** purely additive. Adding `await expr` as a new expression form does not invalidate existing channel-based code. A library that uses `channel.recv()` continues to work; an alternative library using `await` is strictly new code.

**Re-evaluation triggers (any one of):**

1. At least one user-space UI library ships on Kāra, and its authors report that the channel-based pattern is awkward enough to justify language-surface addition — with specific examples of code that would be materially cleaner with `await`.
2. The scheduler or the WASM lowering surfaces a case where the channel contract cannot express something a Promise-adapter needs (e.g., cancellation semantics, structured concurrency composition). If the primitives need to change anyway, re-evaluate whether `await` is part of the cleaner answer.
3. A third primitive concurrency style emerges in the Kāra ecosystem that doesn't fit channels or effects cleanly — a sign that the primitive set is incomplete and `await` (or something) should be added deliberately.

**If none of the triggers fire:** channels + effects remains the permanent answer. That is a valid end state — Kāra stays function-coloring-free as a defining property.

**Cross-reference:** [Async Host APIs on WASM](#async-host-apis-on-wasm) — the mechanism; [design.md §1](design.md#what-kāra-is-not) — the "no `async fn`, no function coloring" stance.

### Bindings, component model, entry points

#### WASM (`--target wasm_browser` or `--target wasm_wasi`)

v1 builds a core module for `wasm_wasi` ([design.md §16](design.md#build-artifacts)); the `--bindings` option, browser bindings and the Component Model form come with this track.

The `karac build` command accepts a `--bindings` flag selecting the output shape:

```
karac build --target wasm_browser --bindings browser    # browser-ready
karac build --target wasm_wasi    --bindings component  # Component Model
karac build --target wasm_browser --bindings none       # raw .wasm only
```

**Default `--bindings` is inferred from the target.** `wasm_browser` defaults to `--bindings browser`; `wasm_wasi` defaults to `--bindings component`. Omitting `--bindings` is the common path. There is no universal default for `--target wasm` in the abstract — the `--target` choice already declares the host family, so defaulting off that choice avoids silent browser-lock-in.

**Browser bindings (`--bindings browser`).** Produces a flat directory:

| File | Purpose |
|---|---|
| `dist/wasm/<pkg>.wasm` | The WASM module. |
| `dist/wasm/<pkg>.js` | ES-module glue with wasm-bindgen-compatible calling convention. Default loaders in vite / webpack / esbuild / rollup work without custom configuration. |
| `dist/wasm/<pkg>.d.ts` | TypeScript declarations for every exported function, including the JS-side shapes for `Result` / `Option` and any exported struct. |

The project-mode layout takes the package name from `kara.toml`; single-file builds emit the same set as `<stem>.{wasm,js,d.ts}` in the working directory. The `.d.ts` declares the glue module's full surface, a `HostImpls` interface typing every `host fn` per the boundary contract, and a `KaraExports` interface typing every export on the handle ([entry point discovery](#entry-point-discovery)). The generated glue marshals rich exports: an exported struct ↔ a JS object, `Option[T]` ↔ `T | null`, `Result[T,E]` ↔ `{ ok: T } | { err: E }`, `String` ↔ `string`, `Vec[T]` ↔ `T[]` — against the same canonical layout the component WIT describes (one set of codegen trampolines backs both bindings).

**Component Model bindings (`--bindings component`).** Produces a single Component Model file:

| File | Purpose |
|---|---|
| `dist/wasm/<pkg>.wasm` | Component Model module with WIT interfaces embedded per the spec. Usable by wasmtime, jco, and other Component Model hosts. |

The component is the **embedded-WIT single-component form**. Project mode writes `dist/wasm/<pkg>.wasm` (package name from `kara.toml`; single-file builds emit `<stem>.wasm` in the working directory). The artifact is a WASI 0.2 command component — the preview1 command adapter synthesizes `export wasi:cli/run` from the core module's `_start` — whose embedded world imports `kara:<pkg>/host` (every `host fn`, typed per the WIT boundary mapping — 64-bit ints ⇒ `s64`/`u64`, raw pointers ⇒ wasm32 `u32` addresses, opaque handles at their field's scalar width). `wasmtime run <pkg>.wasm` works directly. Each exported `pub fn` ([entry point discovery](#entry-point-discovery)) is lifted into the embedded world as an idiomatic `func` over the canonical ABI — scalars, flat `record`s (params + returns), `option`/`result` over scalar inners, `string`, and scalar-element `list<T>` — via codegen trampolines and an exported `cabi_realloc`. Nested aggregates and variant params are omitted from the WIT with a build-time note rather than mis-lowered.

The no-external-tool fallback is `--bindings none` (raw C-ABI core module).

**Raw `.wasm` (`--bindings none`).** Produces only `dist/wasm/<pkg>.wasm`. For users who want to write their own glue or target an unusual host.

#### Component Model emission

Kāra never bakes the Component Model spec into the compiler — that would couple compiler releases to a specification that is still evolving. The embedded-WIT default delegates the spec-coupled transform to `wasm-tools` as an **external tool**: `karac` renders the WIT world itself (plain text, no spec machinery), then shells out to `wasm-tools component embed` (host-fn builds only) and `wasm-tools component new`. The binary resolves from `KARAC_WASM_TOOLS` then `PATH`, and the package can pin the exact version in `kara.toml`:

```toml
[toolchain]
wasm-tools = "1.251.0"   # exact match against `wasm-tools --version`; drift is a hard error
```

Unpinned builds accept whatever version is discovered; a missing binary is a hard error naming the install recipe and the escape hatch (`--bindings none`, which needs no Component Model machinery at all). The one spec-adjacent data ingredient — the `wasi_snapshot_preview1` **command** adapter that lifts the preview1-ABI core module — is vendored into `karac` through the `wasi-preview1-component-adapter-provider` crate (wasmtime's own release artifact, pinned by karac's `Cargo.lock`); `KARAC_WASI_ADAPTER=<path>` substitutes an on-disk adapter. No wasi WIT files are vendored anywhere: the adapter contributes the `wasi:cli/run` export and the `wasi:*` imports at `component new` time. Migration to in-compiler Component Model emission is a later decision, dependent on spec stability.

#### Entry point discovery

Exported symbols in a WASM build are the public functions tagged for the corresponding target. A function is exported when all of the following hold:

1. It is `pub`.
2. It carries `#[cfg(target = "wasm_browser")]` or `#[cfg(target = "wasm_wasi")]` matching the build target *(or is target-agnostic and transitively reachable from such an entry point — that case is tracked by the linker's dead-code elimination in the usual way)*.
3. Its parameter and return types are expressible in the binding surface (`wasm_browser` exports require primitives / `Copy` / opaque-handle types — same restriction as `host fn`; `wasm_wasi` exports can additionally use WIT-expressible types).

There is no separate `#[export]` attribute. `pub` + `#[cfg(target = ...)]` is sufficient.

> **Open.** `#[target]` was merged into `#[cfg]`, so export discovery now keys on a `#[cfg(target = ...)]` condition, which makes `#[cfg]` double as an export marker. The track should confirm that, or bring back a separate marker.

### `host fn` browser and Component Model lowering

- **Browser-WASM:** to a WASM `import` entry under the **`kara_host`** import-module namespace, with compiler-generated JS glue (wasm-bindgen-compatible calling convention so existing JS tooling works). See *Browser-WASM lowering* below.
- **Server-WASM (WASI / Component Model):** under the default `--bindings component`, to a WIT-backed import — the canonical-ABI instance `kara:<pkg>/host` with kebab-case function names, matching the `interface host` the embedded world declares; the Component Model host supplies the implementation at component instantiation. Under `--bindings none` / `browser`, the C-ABI shape remains: the same `kara_host` import entry as the browser target, where the embedder's import object is the thin shim. No glue file is generated either way; the `kara_host` shape is v1's ([design.md §15](design.md#lowering-on-wasm_wasi)). The user-facing `host fn` surface is identical across all shapes.

A library author declares a browser API once as `host fn`; the same source file compiles against all three targets.

```kara
host fn dom_append(parent: ElementHandle, child: ElementHandle)
    with writes(Display);

host fn fetch_begin(url_ptr: *const u8, url_len: i64) -> RequestHandle
    with sends(Network) receives(Network) suspends;
```

No body — ends with `;`. Placed at module scope. Supports attributes and visibility like any other item. (`ElementHandle` / `RequestHandle` are opaque-handle newtypes and the string crosses as a `(ptr, len)` pair, per the parameter rules of [design.md §15](design.md#parameter-and-return-types); a richer `fetch_json(url: ref String)` is the *library wrapper* built on top, not the `host fn` itself.)

Stdlib modules (`std.web`, `std.wasi`) exemplify correct `host fn` effect declarations — `fetch` declared with `sends(Network) receives(Network) suspends`; DOM mutations declared with `writes(Display)`; timer callbacks declared with `writes(Timer)`.

| Target | `host fn` lowering |
|---|---|
| Browser-WASM | WASM `import` entry under the `kara_host` namespace + generated JS glue (wasm-bindgen-compatible calling convention). See below. |
| Server-WASM (WASI / Component Model) | Default (`--bindings component`): WIT-backed import under the canonical-ABI `kara:<pkg>/host` instance, supplied at component instantiation. `--bindings none`/`browser`: WASM `import` entry under the `kara_host` namespace, implementations supplied by the embedder at instantiation (the C-ABI shape). |

The lowering layer lives in the compiler. The user-facing `host fn` surface is stable across all three paths.

#### Browser-WASM lowering

`karac build <file>.kara --target=wasm_browser` emits `<stem>.wasm` plus `<stem>.js`, a zero-dependency ES-module glue file. The decisions:

- **Browser modules are wasm32-wasip1 modules.** The browser target reuses the `wasm_wasi` module flavor — same runtime archive, allocator unification, and entry-shim chain — and the generated glue supplies a minimal console-backed WASI preview-1 polyfill (`fd_write` → `console.log`/`console.error`, `proc_exit`, clock, randomness via `crypto.getRandomValues`; un-polyfilled syscalls throw loudly by name). A WASI-free `wasm32-unknown-unknown` flavor is a possible later refinement; the glue API is the stability boundary, not the module flavor.
- **`kara_host` is the import-module namespace** for every `host fn` — a stable contract. The glue maps the user's implementation object onto it and rejects instantiation loudly (naming the functions) when an implementation is missing; hand-rolled hosts that skip the glue instantiate with `{ kara_host: {...}, wasi_snapshot_preview1: {...} }`. Plain `extern "C"` declarations do **not** get import entries — an unresolved one stays a hard link error.
- **JS boundary types:** `i64`/`u64`/`isize`/`usize` cross as `BigInt`; every other legal scalar (including wasm32 pointers) is a JS number. Opaque handles cross **at their declared scalar width** — an i32-field handle is a number, an i64-field handle a BigInt. Strings cross as `(ptr, len)` scalar pairs; the glue exports `readString(memory, ptr, len)`, and every host implementation receives one trailing context argument `{ memory, readString }` so string params decode without plumbing the memory export by hand.
- **Loader compatibility:** the glue's default loader is `new URL("<stem>.wasm", import.meta.url)` — the asset-reference pattern vite / webpack / esbuild / rollup rewrite without custom configuration — with a `node:fs` branch for `file:` URLs (the same glue runs under node ≥ 18) and a compile-from-bytes fallback when a server mis-types the wasm MIME. `instantiate(hostImpls, opts)` accepts `opts.module` / `opts.bytes` to bypass the loader; `run(hostImpls)` drives `_start` and swallows a clean exit.

- **Component Model lowering.** Under `--bindings component` the same declarations lower to `wasm-import-module = "kara:<pkg>/host"` with kebab-case `wasm-import-name`s, the canonical-ABI strings the embedded world declares, both generated from one source so they cannot drift. Source-level `host fn` declarations and their effect contracts are the same in every shape.

**Panic record on the browser target.** No filesystem in the browser. The structured payload ([design.md §17](design.md#panic-record)) is delivered to a JS-side handler hook (`window.karac_crash` by default; configurable via `KARA_CRASH_HANDLER` import in `kara.toml`); the human-readable summary goes to `console.error`. WASI targets behave like native (filesystem-backed).

#### Per-target manifest blocks for the browser

**Cross-compilation ergonomics.** The manifest's per-target blocks ([design.md §4](design.md#packages-and-manifests)) carry browser glue dependencies and a size-tuned profile:

```toml
[target.wasm32-unknown-unknown.dependencies]
wasm-bindgen = "0.2"

[target.wasm32-unknown-unknown.profile]
opt-level = "z"
```

### Frontend UI Framework

A React / SwiftUI / Vue / Solid-class framework for building user interfaces, covering the full toolkit a web application needs on top of the `std.web` effect substrate and `host fn` bindings. Frontend is not optional — a general-purpose language with no browser story is, in 2026+, a language with a hole where most consumer-facing software lives. Kāra needs an answer; the answer does not have to be in v1.

Scope (any one of these is a substantial library in itself; the full framework bundles all of them):

- **Component model** — how UI components are declared, composed, and given lifecycle (mount / update / unmount). Expected shape: functions that take props and return a declarative view tree; lifecycle hooks modeled as channel subscriptions or provider injection rather than magic names.
- **Reactive primitives** — signals, observables, derived state, or whatever primitive the ecosystem converges on. The effect system + channels are the runtime substrate; the framework decides the user-facing reactivity model.
- **JSX / template syntax for HTML** — declarative view construction. Expected path: a library feature, possibly built on [`comptime`](#comptime), not a language feature; Kāra has no macros. f-strings cover the simple interpolation case today.
- **Routing** — URL-to-view mapping, history integration, nested routes. Standard web-framework fare.
- **Styling** — CSS-in-Kara, utility-class generation, or a CSS-module-style convention. Library choice, not language concern.
- **Hydration protocol** — the contract between SSR-rendered HTML and client-side event binding. Depends on the framework's component model; see [Cross-target Compilation](#cross-target-compilation) for the provider-injection pattern that makes the same component run on both targets.

**What it rests on:**
- [Web / Host Effect Vocabulary](#web--host-effect-vocabulary) (the `Display` / `Input` / `Timer` / `Storage` / `Console` resources the framework calls into).
- [design.md §15 Host functions](design.md#host-functions) (the `host fn` primitives the stdlib exposes for DOM / events / storage).
- [Cross-target Compilation](#cross-target-compilation) (the SSR-shared-component + per-target-provider pattern the framework enforces on user code).
- [Async Host APIs on WASM](#async-host-apis-on-wasm) (channel-over-Promise pattern for host API integration).
- An ergonomic view syntax — not yet designed; without one the framework works but the DX is `View.div(View.text("hello"))`-style.

**Why deferred:**
1. **Not in v1.** The v1 launch story does not require frontend. Pulling it into v1 trades 6-12 months of frontend design and implementation against a launch that already has enough surface to defend. Better to ship v1 and then commit serious effort to a frontend story than to delay v1 for it.
2. **Not optional either.** Treating it as a library that may or may not ship understates its importance. The project will ship a frontend story; the only question is which release it lands in.
3. **Substrate dependencies.** Every viable shape (React hooks, Solid signals, SwiftUI declarative, Vue composition API) has active ecosystem evolution. The view syntax and the browser target must land first.

**Pre-design work (not blocking):**
- Sketch DOM/JS-interop type-system bridge. How does an effect-typed language interact with JS callbacks? What's the equivalent of `wasm-bindgen`?
- Survey the design space (Yew, Leptos, Sycamore, Dioxus from Rust; Solid/React from JS). What does Kāra's effect system change about the reactivity model?
- Identify whether the framework is a separate-team effort or a project-owned reference (parallel to the `kara-postgres` decision).

**Pre-build checklist (all must be done before building this):**
- [ ] Browser target shipped and stable
- [ ] View syntax designed (the framework is buildable without it, but users will hit the view-syntax wall fast)
- [ ] `std.web` stdlib layer for `Display` / `Storage` / `Console` / `Timer` / `Input` host-fn bindings shipped

This entry is the canonical tracker for a Kāra frontend UI framework.

## GPU

GPU compute: the `#[gpu]` kernel subset, `gpu.dispatch` and the device backends.

<a id="feature-7-compilation-target-flexibility"></a>
### Compilation Target Flexibility

v1 compiles to `native` and `wasm_wasi` ([design.md §16](design.md#targets)). The full target picture:

One language, multiple targets:

- **Native code** via LLVM (systems, CLI tools, game engines)
- **WebAssembly** (browser, edge computing)
- **GPU compute shaders** (data processing, ML inference) — `#[gpu]` marks kernel entry points; compiler enforces GPU-safe subset. See [GPU Subset Constraints](#gpu-subset-constraints).
- **Embedded** — `embedded` and `kernel` profiles raise `allocates(Heap)` and `panics` to compile errors; ISR-targeted code is tighter still. See [Project Profiles](#project-profiles).
- **FPGA bitstreams** (future goal)

Targets have profile constraints — code that respects a target's constraints compiles to that target. Programs that use restricted resources (heap and panics on embedded; closures-capturing-host, heap, recursion, dyn, panics on GPU) need either a different target or a refactor to stay within the profile. The compiler reports exactly what won't compile and why.

Layout groups map naturally to GPU buffers. Auto-concurrency degrades gracefully per target.

### GPU Subset Constraints

Multi-vendor coverage comes from the wgpu-primary codegen path, which auto-selects Metal on macOS, Vulkan on Linux, DX12 on Windows and WebGPU in the browser; CUDA is opt-in via `--target cuda`. The `#[gpu]` constraint, `GpuSafe` trait, and `gpu.dispatch` semantics below are vendor-neutral by construction.

GPU code is not a separate language — it is a restricted subset of Kāra. The `#[gpu]` annotation is a **constraint declaration**: "this function uses only GPU-compatible features." It does not route the function to the GPU. Dispatch is always explicit via `gpu.dispatch`.

```
#[gpu]
fn dot(a: ref Array[f64, 3], b: ref Array[f64, 3]) -> f64 {
    a[0]*b[0] + a[1]*b[1] + a[2]*b[2]
}

// Call on CPU — fine, it's just a function
let result = dot(a, b);

// Dispatch to GPU — explicit programmer decision
let result = gpu.dispatch(dot, a, b);
```

Small computation? Call it directly on CPU. Large batch? Dispatch it. The programmer controls where work runs.

#### What is and is not GPU-compatible

| Allowed | Not Allowed |
|---|---|
| Primitives (`i32`, `f64`, `bool`, …) | `String`, `Vec[T]`, `Map[K, V]` (heap-allocated) |
| `Array[T, N]` (fixed-size) | `shared struct` / RC types |
| Structs and enums with GPU-compatible fields | Closures that capture from host memory |
| Pattern matching | Dynamic dispatch (`dyn Trait`) |
| Loops (`for`, `while`, `loop`) | I/O effects (`reads(FileSystem)`, etc.) |
| Generics (monomorphized at compile time) | `panics` |
| Pure functions | Recursion |
| Traits (static dispatch only) | Heap allocation (`allocates(Heap)`) |
| Refinement types, distinct types, layout blocks | `Option[String]`, `Result[String, E]` (contains heap type) |

The constraint propagates through generic types: `Option[i64]` is GPU-compatible; `Option[String]` is not, because `String` is not.

#### `GpuSafe` trait

Compatibility is structural by default — the compiler walks the type tree. All primitive types and `Array[T, N]` implement `GpuSafe` automatically when `T: GpuSafe`. Structs and enums implement it when all fields do.

`GpuSafe` is also available as an explicit bound for generic GPU functions:

```
#[gpu]
fn batch_op[T: GpuSafe](data: ref Array[T, 1024]) -> Array[T, 1024] { ... }
```

#### Generics and `#[gpu]`

A generic function must be explicitly annotated with `#[gpu]` to be callable from a GPU kernel — even if all concrete type parameters at a given call site satisfy `GpuSafe`. The annotation is not inferred from monomorphization: the intent to be GPU-callable must be declared at the definition site, not silently inferred at the call site. A generic without `#[gpu]` that is instantiated with only `GpuSafe` types is still a compile error when called from a GPU context.

**Enforcement phase.** The `#[gpu]` annotation check is performed during call-graph validation (type checking phase), *before* monomorphization. When the type checker walks the call graph from a `#[gpu]` root and encounters a call to a generic function that lacks `#[gpu]`, it emits an error immediately — it does not wait to see what types the generic is instantiated with. This is a deliberate pre-monomorphization check: the question is "did the author declare this function GPU-callable?" not "does this particular instantiation happen to be GPU-safe?"

```
// Correct — explicitly GPU-callable
#[gpu]
fn map[T: GpuSafe](data: ref Array[T, 1024], f: Fn(T) -> T) -> Array[T, 1024] { ... }

// Wrong — calling from #[gpu] without annotation is an error, even if T: GpuSafe
fn map[T: GpuSafe](data: ref Array[T, 1024], f: Fn(T) -> T) -> Array[T, 1024] { ... }

#[gpu]
fn kernel(data: ref Array[f64, 1024]) -> Array[f64, 1024] {
    map(data, |x| x * 2.0)   // ERROR: `map` is not annotated #[gpu]
}
```

#### Call graph validation

The compiler validates the full call graph from a `#[gpu]` root using two existing phases — no dedicated GPU pass is needed:

- **Effect violations** (`panics`, `allocates(Heap)`, `reads`/`writes`/`sends`/`receives` on I/O resources) are caught by the **effect checker**. It rejects any forbidden effect in the transitive call graph from a `#[gpu]` root. `todo()`, `unreachable()`, and `panic()` all carry the `panics` effect and are caught by this same check.
- **Feature violations** (heap types, recursion, dynamic dispatch, host-capturing closures) are caught by **call-graph validation during type checking**. The type checker walks the call graph from each `#[gpu]` root and rejects structural incompatibilities — e.g., a `String` field in a return type, a recursive call cycle, or a `dyn Trait` parameter.

Both phases produce errors that include the full call chain from the `#[gpu]` root to the violation site.

Non-generic functions called from a `#[gpu]` function do not need to be annotated — if they contain no incompatible features, they are automatically GPU-compatible. If any function in the call graph uses an incompatible feature, the error points to the specific call with the full chain:

```
error[E0801]: `String` is not GPU-compatible
  --> src/compute.kara:12:5
   |
12 |     let name = "hello".to_string();
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = note: GPU functions cannot use heap-allocated types
   = note: called from: physics_step() → format_label() → String.new()
   = hint: use fixed-size `Array[u8, N]` for GPU-compatible text
```

#### Data transfer via effects

CPU-side dispatch uses the effect system to make data movement explicit. `gpu.dispatch` carries `reads(GpuBuffer[buf])` for input buffers and `writes(GpuBuffer[buf])` for output buffers — the compiler infers read vs write from whether the buffer is consumed as input or produced as output. The effect is parameterized by the buffer handle, using the same parameterized-resource distinctness rules as [Parameterized Resources](#parameterized-resources-opt-in-finer-granularity):

```
fn run_simulation(world: mut ref World) with writes(GpuBuffer[positions]) {
    // compiler handles: upload → run kernel → download result
    let new_positions = gpu.dispatch(physics_step, world.positions, world.velocities, world.dt);
    world.positions = new_positions;
}

// Two independent dispatches can parallelize — different buffer parameters
fn update_all(world: mut ref World) with writes(GpuBuffer[positions]) writes(GpuBuffer[normals]) {
    par {
        world.positions = gpu.dispatch(physics_step, world.positions, world.velocities, world.dt);
        world.normals = gpu.dispatch(recompute_normals, world.mesh, world.positions);
    }
    // conflict analysis sees writes(GpuBuffer[positions]) vs writes(GpuBuffer[normals])
    // — proven disjoint via literal distinctness, so both dispatches run in parallel
}
```

Layout groups ([Layout](#layout)) map directly to GPU buffers — `group physics { position, velocity }` corresponds to a single GPU buffer, so the compiler can generate efficient bulk transfers.

#### Whole-buffer reductions

`gpu.dispatch` is map-shaped: `N` inputs in, `N` outputs out. A **reduction** collapses a buffer to one value, which no dispatch can express, so the reductions are named operations of their own:

```
let s   = gpu.sum(buf)        // f32
let p   = gpu.prod(buf)       // f32
let lo  = gpu.min(buf)        // Option[f32] — None iff the buffer is empty
let hi  = gpu.max(buf)        // Option[f32]
let mu  = gpu.mean(buf)       // Option[f32]
let d   = gpu.dot(a, b)       // f32 — traps if the lengths differ; the element type over integers
let a   = gpu.mean(buf)       // Option[f32]; Option[f64] over an integer buffer
let s2  = gpu.variance(buf)   // Option[f32] — population (÷ n); Option[f64] over an integer buffer
let sd  = gpu.stddev(buf)     // Option[f32]; Option[f64] over an integer buffer
let i   = gpu.argmin(buf)     // Option[i64] — the INDEX of the minimum
let j   = gpu.argmax(buf)     // Option[i64]
let ps  = gpu.prefix_sum(buf) // Vec[f32] — inclusive; NOT a reduction; Vec[i32] / Vec[u32] too
```

The fallible ones are fallible because the answer genuinely does not exist, not because the implementation is awkward: an empty buffer has no minimum, no maximum and no mean. Returning the shader's padding identity (`+inf`) or `0.0 / 0` (`NaN`) would be a plausible-looking value that propagates silently through everything downstream, which is the failure mode this whole family is built to avoid. `sum`/`prod`/`dot` are total — the empty case is the identity (`0`, `1`, `0`) — so they return a bare `f32`.

They take a `Vec[f32]` rather than a kernel, and that is the design, not a shortcut. A user-supplied combiner would have to be **associative** for a tree reduction to mean anything, and nothing in the language can check associativity — a non-associative combiner would produce a plausible, order-dependent, irreproducible number. Naming the operation moves that obligation to the compiler, where it is discharged once.

**The reduction order is language semantics, not an implementation detail.** A GPU reduction is a tree; a CPU fold is a line; `f32` addition is not associative, so the two genuinely disagree — 64 copies of `0.1` sum to `6.400000` as a tree and `6.399996` as a left fold. Kāra therefore **specifies** the tree order and the interpreter reproduces it exactly, so `karac run` and `karac build` agree bit-for-bit rather than within an epsilon:

> Pad the buffer to the workgroup width with the operation's identity, then halve — `s[t] = s[t] OP s[t + stride]` for stride 32, 16, 8, 4, 2, 1. A buffer longer than one workgroup is reduced in workgroup-wide chunks, and the resulting partials are reduced the same way, recursively.

Two consequences worth stating plainly:

- A long reduction is a **tree of trees**, and the grouping is observable in `f32`. `gpu.sum` over 4096 elements is not the same number a flat 4096-wide tree would give, nor a left fold. That is fine — it is specified — but it means the answer is a property of the language, not of the device.
- `min`/`max` are **NaN-ignoring** (`min(x, NaN) == min(NaN, x) == x`), matching `f32::min`. A NaN-propagating min is fine in a left fold and broken in a tree: the halving would decide which side a NaN lands on, so the answer would depend on the buffer length. NaN-ignoring min is associative, so every grouping agrees. `sum`/`prod` cannot make that promise — which is exactly why their grouping is part of the specified result.

Two operations are defined in terms of `sum` rather than having a tree of their own, and both equalities are exact:

- `gpu.argmin` / `gpu.argmax` report an **index**, so they yield `Option[i64]` whatever the element type — including over `Vec[i32]` and `Vec[u32]`, where signedness affects only the comparison and never the result. Above 2³¹ the two integer orders disagree at both ends (`4294967295` is the largest `u32` and `-1` as `i32`), so the element type decides the answer, not the bits. Ties take the **first** occurrence, matching `Stats.argmin`. Their tree carries (value, index) pairs and its combine is lexicographic — strictly better value wins, exact tie goes to the smaller index — which is a semilattice, so like `min`/`max` they are grouping-independent.

They diverge from `Stats.argmin` on **NaN**, and necessarily. `Stats.argmin` seeds its running best with element 0 and displaces it only on a strict comparison, so a leading NaN is never displaced: `Stats.argmin([NaN, 3.0, 1.0])` is `0` while `Stats.argmin([3.0, 1.0, NaN])` is `1`. That position-dependence cannot survive a halving tree, where the grouping decides the positions. In `gpu.argmin` a NaN always loses, so the first buffer answers `2`; an all-NaN buffer answers `0`, since nothing ever wins and the leftmost survives.

`gpu.variance` / `gpu.stddev` are the **population** forms (÷ n), matching `Stats.variance` and `Stats.stddev`, so the two answer the same number on the same buffer — and differing from `Column`'s `var` / `std`, which are the **sample** (÷ n−1) forms; see [Numerical Types](#numerical-types-tensor-column-dataframe) § "Variance divisor" for why the split is deliberate. They are the only **two-pass** reductions: the mean has to exist before a single deviation can be formed, so the device runs a complete sum reduction, reads the mean back, and dispatches again with it as a uniform. Both passes go through the same sum tree, so the grouping caveat above is inherited rather than re-derived — and `gpu.stddev(v)` is exactly `gpu.variance(v)` rooted once at the end, not a separate accumulation.

**`prefix_sum` works over `Vec[i32]` / `Vec[u32]` too, and it traps — but where it traps is the interesting part.**

Every checked integer operation above it is a **reduction**: only lane 0 survives, a lane above the stride holds a partial nobody reads, and its overflow is irrelevant. A scan writes all `n` values, so an overflow in **any** lane is an overflow in the answer. The emitted shader therefore ORs the flag across the whole workgroup rather than reading lane 0 — inheriting the reduction's habit would silently drop overflows in exactly the elements the caller asked for. Padded lanes are checked as well: a lane past the buffer starts at the identity, but the scan sweeps real values into it, so it ends up holding the chunk total that phase 2 folds and every later chunk's offset depends on.

The phase-3 offset add is checked too, and it is the step most easily forgotten — phases 1 and 2 look like *the arithmetic* while shifting a chunk looks like bookkeeping, but `scanned[i] + offset` adds two real values and is precisely where a long buffer's total finally leaves the range.

**The specified order decides *whether* an integer scan traps, not merely what it returns.** This is the prefix sum's version of the fact recorded above for the reductions, and it is sharper here because Hillis-Steele forms **window sums** a sequential scan never does. With `MAX = i32.MAX`:

| buffer | running total | `gpu.prefix_sum` |
|---|---|---|
| `[-MAX, MAX, MAX]` | `-MAX`, `0`, `MAX` — all in range | **traps** |

The first Hillis-Steele step computes `prev[2] + prev[1]`, which is `MAX + MAX`. All three surfaces trap on it, because the interpreter reproduces the device's step order — so this is specified behaviour, not a divergence. It does mean that replacing a running total with `gpu.prefix_sum` is **not** a pure speedup on integer data: it can introduce a trap that was not there. Float scans have no analogue, because float addition saturates rather than trapping.

**`prod`, `dot` and `matmul` work over `Vec[i32]` / `Vec[u32]` and rank-2 `Tensor[i32]` / `Tensor[u32]`, and they trap.** WGSL has no widening-multiply intrinsic, but a `u32 × u32 → u64` product is four 16-bit partial products and two carries, which the emitted `karac_mul_wide` performs exactly. Once an exact wide product exists, "did this multiply overflow 32 bits?" is a question about the high word — which is how a hardware multiplier answers it too.

So all three follow the rule the integer reductions already set (see *Integer reductions overflow-check* below): compute in the element type, and **trap rather than wrap**, matching `v.product()` on a `Vec[i32]`.

- **`gpu.prod`** checks each multiply and folds the overflow bit exactly as `sum` folds its own. The empty product is `1` — the multiplicative identity, supplied by the host, since no shader ever sees an empty buffer.
- **`gpu.dot`** checks the per-element product *and* every accumulation, because either can overflow alone (`65536 * 65536` leaves `i32` in a single term, with nothing yet accumulated). The identity `gpu.dot(a, b) == gpu.sum(a * b)` therefore extends over integers to **which programs trap**, not only to what the successful ones return. Its operands must share an element type: a mixed pair has no single overflow rule.
- **`gpu.matmul`** over an integer tensor equals `a.matmul(b)` **trap for trap**. That is the sharper form of the float promise: tiling preserves the naive `k`-ascending order, so the same intermediates are formed — and overflow is a property of the intermediates. Reordering the tile loop would break the trap agreement even where it preserved every returned value. One output cell that cannot be represented traps the whole product; a matrix with a wrapped entry in it is not an answer.

A padded lane can never raise an overflow flag. Lanes past `m`, `n` or `k` stage zeros and multiply them, so an edge workgroup on a perfectly valid matrix cannot trap on arithmetic that is not in the data.

**Over an integer buffer, `variance` and `stddev` are EXACT, and return `Option[f64]`.** This is the one place in the family where the GPU is more accurate than the CPU, so it is worth saying why rather than only that.

The obvious construction fails: forming `(x − mean)` on the device means forming it in `f32`, and `f32` represents integers exactly only to 2²⁴, so every element of a buffer centred above ~16.7 million is quantised before the subtraction happens. Measured, a buffer centred at 2²⁸ with a spread of ±100 reports a variance about 6% wrong — an ordinary `i32` range producing a plausible, silently incorrect number, which is the failure this family exists to prevent. `mean`'s promote-late trick does not rescue it, because the damage is done on the device rather than at the divide.

Two changes make it exact instead:

- **Shift by an integer, not by the mean.** `Var(x) = Var(x − K)` for any constant `K`, so the host sends `K = round(mean)` — an integer — and the device subtracts it in exact integer arithmetic. Nothing becomes a float at any point. The deviation's magnitude is then bounded by the data's **spread** rather than by its position on the number line, which is the dependency a variance ought to have.
- **Square exactly.** WGSL has no widening-multiply *intrinsic*, but a `u32 × u32 → u64` product is four 16-bit partial products and two carries. So `d²` need not be a float either: it is an exact `u64`, tree-accumulated with carry.

The whole computation is then exact until a single final rounding — `Σd² ` on the device, `Σd = Σx − n·K` on the host from the sum it already has, and `(n·Σd² − (Σd)²) / n²` formed as one integer numerator and rounded once into `f64`. That makes it **more accurate than `Stats.variance`**, which sums `f64` deviations and rounds at every step; the two may therefore differ in the last bit, and the GPU is the one that is right. Every other operation in this family treats the CPU as the reference, so this inversion is deliberate and narrow.

Overflow is the only way it can fail. `Σd²` is a `u64`, so a buffer whose spread genuinely does not fit **traps**, exactly as an overflowing integer `gpu.sum` does — roughly `n · spread² > 1.8 × 10¹⁹`, which for a million elements means a spread past ~4.3 million. Note what does *not* trap: magnitude. A buffer of values within a few of `i32.MAX` has a tiny spread and computes cleanly.

The unsigned form differs only in which exact sum feeds the mean; `K` travels to the device as two 32-bit words because `round(mean)` of a `u32` buffer can exceed `i32.MAX`, and a 32-bit shift would wrap on precisely the buffers this design exists to serve.

`gpu.dot(a, b)` is `gpu.sum(a * b)`, to the last bit. It is a separate operation only because the product is formed **on load** inside the reduction's first level, so no `n`-element intermediate is ever written — a device-traffic win, not a semantic difference.
- `gpu.mean(buf)` is `gpu.sum(buf) / n`, to the last bit: the specified tree sum, divided by the count, in `f32`, **once**. Not compensated, not accumulated wider. So `mean` inherits the sum's grouping rather than having one of its own, and adds exactly one further rounding — that is the whole of its precision story. The division happens on the host after the fold converges, never in the shader: a shader cannot know it is running the last level of the tree, so a division inside it would divide once per level.

#### Integer reductions overflow-check, and the tree order decides when

An integer reduction can overflow where a float one saturates to infinity, and Kāra traps on integer overflow ([design.md §5](design.md#numeric-semantics)) where WGSL is defined to wrap. That collision cannot be inherited from the kernel-body rule, because the kernel-body escape hatch is an *expression* spelling — `x.wrapping_add(y)` — and `gpu.sum(v)` has no `+` in the source to annotate.

**The rule: integer reductions trap, exactly as their CPU counterparts do.** `v.sum()` over a `Vec[i32]` whose total exceeds `i32` already fails with `integer overflow` on both `karac run` and `karac build`; `gpu.sum(v)` does the same. Anything else would make moving a reduction to the GPU silently change a trap into a wrong answer, which is the divergence class the `#[gpu]` overflow rule was introduced to close.

`min` and `max` cannot overflow and are therefore unconditionally available over `i32`/`u32`.

`mean` over an integer buffer **promotes**, matching `Stats.mean` — the mean of `[1, 2]` is `1.5`, not `1`. It promotes to `f64`, and it promotes **late**: the sum is computed in the integer type (exactly, or trapping), and only the finished sum is widened. That widen is lossless — 32 bits into a 53-bit mantissa — so the whole operation rounds exactly once, at the divide. Promoting the *elements* first, as the CPU implementation does, would mean promoting to `f32` on a GPU, whose 24-bit mantissa loses whole integers above `16777216`.

The price of computing exactly is that the **sum** must fit even where the mean would: `gpu.mean` of `[i32.MAX, i32.MAX]` traps, while `Stats.mean` promotes first and does not. A float buffer's `mean` keeps its own width (`Vec[f32]` → `Option[f32]`) — an `f32` tree cannot justify an `f64` answer.

Signedness follows the element type all the way through — the shader's comparison, the overflow rule (a signed add overflows on a shared-sign-then-flip, an unsigned one on a carry), and the result's width. `gpu.sum` over a `Vec[u32]` containing `2147483647` and `1` yields `2147483648`; the same buffer typed `Vec[i32]` traps. The two are different functions, not the same function with a cast.

**The consequence worth stating loudly: the specified tree order determines *whether* an integer reduction traps, not merely what it returns.** Overflow is a property of the intermediate sums, and a tree forms different intermediates than a line. Both directions are reachable — with `MAX = i32.MAX`:

| buffer | tree (specified) | left fold |
|---|---|---|
| `[MAX, MAX, -MAX, -MAX]` | `0` | **traps** |
| `[MAX, -MAX, MAX, -MAX]` | **traps** | `0` |

So `gpu.sum(v)` and `v.sum()` over the same integer buffer may legitimately disagree about whether they fail. That is not a bug and not a divergence: they are different operations with different, specified evaluation orders, exactly as they are for `f32` values. It does mean that replacing `v.sum()` with `gpu.sum(v)` is **not** a pure speedup for integer data — it can introduce a trap that was not there, or remove one that was. Float reductions have no analogue of this, because float addition saturates rather than trapping.

#### Reducing a buffer that is already on the device

Every reduction above takes a host `Vec` — it uploads, reduces, and reads one value back. Inside a resident sim loop that is the wrong shape. `gpu.upload` has already put the grid on the device and `gpu.dispatch` keeps it there across substeps, so asking for its total each step by uploading a *second copy* of data the device is already holding reinstates exactly the transfer the resident path exists to remove.

A reduction may therefore name a field of a live `GpuBuffer[S]`:

```
let buf   = gpu.upload(bodies)   // GpuBuffer[Body], resident
let total = gpu.sum(buf.mass)    // f32   — no upload; 4 bytes come back
let peak  = gpu.max(buf.speed)   // Option[f32]
```

**A field, not the buffer.** `gpu.upload` takes a `Vec[S]` over an all-`f32` struct, so a `GpuBuffer[S]` holds *records*; "the sum of a buffer of records" names nothing. The quantity a loop actually wants is a reduction over one field, and `buf.mass` already parses as an ordinary field access, so this needs no new syntax — only the recognition that a field access on a device buffer means a projection rather than a load.

**The result types are unchanged**, and deliberately so: `sum`/`prod` stay total, `min`/`max`/`mean` stay `Option[f32]` because an empty buffer still has no minimum. Moving a reduction onto a resident buffer is a change of *where the data is*, never of what the operation means, so it is a refactor rather than a signature change.

**The answer is bit-identical to the round-trip.** `gpu.sum(buf.mass)` equals `gpu.sum(v)` for the host `Vec[f32]` of those same values — not within an epsilon, exactly. The device path reads the field with a stride and the host path reads a contiguous array, but both walk the *same* padded-halving tree at the same width, and every level above the first is literally the same shader. So the grouping rule above applies unchanged, and the resident form inherits its consequences rather than acquiring its own.

That equality is what makes the feature safe to reach for: it is an optimisation whose observable result is the operation it optimises. A design where the resident form had its own summation order would force every caller to decide which number they meant.

**Layout groups are resolved, not assumed.** A `layout` block splits `S` across several device buffers, so `mass` is a *(group, stride, offset)* rather than a field index — and the compiler resolves it through the same layout lookup `gpu.upload` and `gpu.dispatch` use. All three must agree: a buffer uploaded under one grouping and reduced under another would read a different field entirely and report a plausible wrong number, with nothing to flag it.

**`GpuBuffer[S]` is a first-class type.** It can be written wherever a type can be written — a struct field, a parameter, a return type — so a device buffer is storable rather than confined to the local binding that produced it:

```
struct Sim { grid: GpuBuffer[Body], step: i32 }

fn make(v: Vec[Body]) -> GpuBuffer[Body] { gpu.upload(v) }
fn total(b: ref GpuBuffer[Body]) -> f32   { gpu.sum(b.mass) }

let sim = Sim { grid: make(bodies), step: 0 }
let m    = gpu.sum(sim.grid.mass)     // a field of a buffer the struct owns
```

`S` rides in the type, which is what lets a buffer reached through a field or a parameter behave exactly like a bound one. A `ref GpuBuffer[S]` parameter is the natural way to pass one to a function that only reads it, and a reduction projects through the borrow the way field access does for any other aggregate.

**A buffer that escapes its scope is disarmed, not freed.** A `let` queues a scope-exit free; if the buffer is then returned — as a tail expression, through an explicit `return`, or inside a struct that is returned — that free would run while the caller still holds the handle. Every escaping position therefore zeroes the origin's handle word, which makes its free inert (`karac_runtime_gpu_free_soa(0)` is a no-op, the same property that lets the drain skip a live-guard) and leaves the destination sole owner. Zeroing rather than retracting the queued action is what reaches a *field*: a buffer moved into a struct is freed by that struct's drop, which lives inside `__karac_drop_struct_<S>` and is in no scope's action list to retract.

**A by-value argument is not an escape.** A by-value aggregate parameter is normally callee-owned, because the callee deep-copies on entry — but a device buffer cannot be copied by duplicating its handle, so the callee only aliases it and registers no free. The caller stays the owner and its free must survive; disarming there would leave the allocation with no owner at all. This is the one place the rule is an asymmetry rather than a uniform "a move disarms the source", and it follows from there being no copy to hand over.

**A field is freed by its struct's drop.** No `let` binds a buffer that lives in a field, so nothing else would ever reclaim it — and a leaked *device* allocation is invisible to LeakSanitizer, which walks the host heap, so it surfaces only as a GPU eventually refusing to allocate. The drop is emitted by the same field classifier that closes a `File` field and frees an HTTP handle, guarded the same two ways: a user type named `GpuBuffer` shadows the builtin and keeps whatever class it had, and the field must actually lower to the two-word handle.

**The receiver may be a place or a temporary, and that decides who frees it.** `buf.mass` and `sim.grid.mass` reduce a buffer someone else owns, so the reduction only reads it — freeing it there would leave the binding, or the struct's field, holding a dangling handle. `gpu.upload(cells).mass` and `gpu.dispatch(k, buf).mass` reduce a buffer that has no owner at all: no `let` bound it and no struct holds it, so nothing else will ever free it and the reduction is the only site that can. Both forms are accepted and the compiler emits the free for exactly the second.

The distinction is the receiver's shape: a *place* — a binding, `self`, or a projection out of one — belongs to whoever declared it, and anything else produced the buffer on the spot. Neither half shows up in a program's output (an unfreed temporary still prints the right number, and so does the *first* read of a wrongly-freed place), so the emitted free is asserted structurally in the codegen tests rather than left to an end-to-end run.

This is a **compiled-only** surface, like the rest of the resident API — `karac run` reports that rather than pretending, since there is no device buffer to project a field out of.

#### Prefix sum — the one that is not a fold

`gpu.prefix_sum(buf)` is the odd member of this family: its result is a **buffer**, not a value. `out[i]` is the sum of `buf[0..=i]` — **inclusive**, matching NumPy's `cumsum`, C++'s `partial_sum` and Python's `itertools.accumulate`. It returns a bare `Vec[f32]`, with no `Option`: the prefix sums of an empty buffer are the empty buffer, so unlike `min`/`mean` there is no missing answer for `None` to carry.

It is spelled `prefix_sum` rather than `scan` because **`scan` already means the iterator adapter** — a stateful map, as in Rust. Two unrelated operations under one name is the hazard the `variance`/`var` split is documented against elsewhere in this document; the language does not need a third instance of it.

**Its order is specified, like every other order here, and it is a different order.** Within one workgroup-wide chunk, for stride 1, 2, 4, 8, 16, 32: every lane at or past `stride` adds the lane `stride` below it, with all lanes reading the values as they stood *before* the step (Hillis-Steele). Past one chunk, the chunks are scanned independently, their totals are prefix-summed by the same procedure one level up, and each chunk's exclusive offset is added back — so a long prefix sum is a **prefix sum of prefix sums**, the same self-similarity the reduction tree has.

The consequence follows directly, and it is worth knowing before it is discovered:

> **`gpu.prefix_sum(v)` ending in the total does not make that total `gpu.sum(v)`.** Both compute the sum of everything, by different groupings. For `[a, b, c, d]` the halving tree computes `(a+c) + (b+d)` while the scan's last lane computes `(a+b) + (c+d)`, and `f32` addition is not associative.

They agree far more often than not — every uniform buffer agrees, and roughly 60% of random ones do — which is exactly why it is stated rather than left to be found. No choice of algorithm removes it: Blelloch's up-sweep does form precisely the reduction tree's total, but its down-sweep overwrites the root with the identity before any output exists, so that total never reaches the result. This is the same class of fact as the tree-of-trees grouping above: a property of the specified operation, not of the device, and identical under `karac run` and `karac build`.

The interpreter twin reproduces the trap *points*, not just the final value, by running the same tree with checked operations — so the two surfaces agree on which programs fail as well as on what the surviving ones return.

#### Tiled matmul — the one that agrees with the CPU

`gpu.matmul(a, b)` multiplies two rank-2 `Tensor[f32]`s: `[m, k] × [k, n] → [m, n]`.

It is the **only operation in this family that takes tensors rather than `Vec`s**, and it has to be. Every reduction above contracts a flat buffer to a scalar, so `Vec[f32]` says everything about its input. A matmul's meaning depends on a shape, and `m·k` values can be read as `[1, m·k]`, `[m, k]`, or any other factorisation. Passing the shape as extra integer arguments would let a caller state a shape the data does not have — and a wrong `k` does not fail, it reads real values from the wrong places. `Tensor[f32, [m, k]]` already carries the shape, checked, next to the data. The operand rules are `Tensor.matmul`'s, reused rather than restated, so the two surfaces accept exactly the same programs.

It is also the **only operation in this family whose result equals its ordinary CPU counterpart bit-for-bit**:

> **`gpu.matmul(a, b)` is `a.matmul(b)`, exactly, on every surface.**

That is a promise rather than a coincidence, and it is the reverse of every other entry above. Each workgroup computes a `16×16` block of the output, walking the contraction in 16-wide steps: stage one tile of each operand into workgroup memory, barrier, accumulate 16 products, barrier, advance. Tiles are visited in ascending `k` and the inner loop runs in order within a tile, so the products accumulate in `k = 0, 1, 2, …` order — element for element, the order the naive triple loop uses. **Tiling changes where the operands are read from, not when they are added.** Contrast `gpu.sum`, whose halving tree is genuinely a different grouping from `v.sum()`'s line.

The corollary is worth stating because it looks like a contradiction:

> **`gpu.matmul` disagreeing with `gpu.dot` of the same row and column is correct.** `gpu.dot` reduces with the halving tree; `gpu.matmul` accumulates in the naive order. Both compute the same mathematical dot product, by different groupings, and `f32` addition is not associative.

So the two GPU operations differ from each other while one of them matches the CPU exactly. Both facts follow from the same rule that has governed this whole family: the order is part of the specification, and each operation's order is whichever one its shader actually performs.

**Accumulation is in `f32`, rounding at every step**, matching `Tensor.matmul` on both CPU surfaces. This is forced rather than chosen — WGSL has no `f64`, so a wider accumulator would put the device permanently out of reach of its own twin.

The tile is `16×16` = 256 invocations, the portable `maxComputeInvocationsPerWorkgroup` floor; the 64-wide reduction workgroup is an unrelated number, a *line* of lanes where this is a *square*. At a ragged edge **both operand tiles are zero-padded at the same `k`**, so a padded lane contributes `0.0 × 0.0`. Padding one side only would let a real value meet a padded zero, and `inf × 0.0` is NaN — an output element poisoned by arithmetic that was never in the data.

`k == 0` is not the empty case: `[m, 0] × [0, n]` is an `[m, n]` block of zeros, because the empty sum is the additive identity. Unlike the reductions, where an empty input genuinely has no answer, an empty contraction has a perfectly good one.

#### GPU (`--target gpu`)

Produces SPIR-V / WGSL per `#[gpu]` kernel; see [GPU Subset Constraints](#gpu-subset-constraints). Not a standalone deployable binary — GPU kernels are consumed by a host program via `gpu.dispatch`.

**Panic record on the GPU target.** GPU code cannot write disk or stderr. A panic in GPU lowering is surfaced as a host-side panic at the kernel-launch site, with `panic_kind: "gpu_kernel_failed"` and the GPU-side error code (when retrievable) in a `gpu_marker` field. Full GPU stack reconstruction comes later.

#### Integer arithmetic in kernels

**Inside a `#[gpu]` kernel, bare integer `+ - * / %` is a COMPILE ERROR** — not a silent change of meaning. A GPU is the `embedded` situation without the profile that opts into it: WGSL defines integer overflow as wrapping and division by zero as returning an implementation-defined value, and offers no trapping form, so a kernel physically cannot honour the `app`/`lib` trap. Rather than let `#[gpu]` act as an invisible `overflow-checks=off` region — precisely what [design.md §5](design.md#numeric-semantics) rules out — the kernel must NAME the wraparound: `a.wrapping_add(b)` / `wrapping_sub` / `wrapping_mul`, which lower to the same WGSL operator the bare form would have emitted. Division has no such spelling, because the divergence there is division-by-zero rather than overflow; guard the divisor in the kernel or keep the division on the host. **Float arithmetic is unaffected** (IEEE ops do not trap), and a `for`-over-range counter is unaffected (its increment is generated by the lowering, not written by the user) — so the cost falls only on hand-written integer arithmetic, chiefly a `while` loop's own counter.

#### The `GpuBuffer` resource and heap allocation

This track adds one built-in resource to [design.md §12](design.md#resources):

| Verbs | Resource | Covers |
|---|---|---|

| `reads`/`writes` | `GpuBuffer[buf_id]` | GPU memory upload/download via `gpu.dispatch`; parameterized by buffer handle |

**`Heap` allocation is default-permitted** — the effect checker infers and propagates `allocates(Heap)` like any other effect, but the default project profile permits it without requiring explicit declaration on public functions. This is distinct from *transparent* effects, which never appear in effect sets at all. At GPU/embedded boundaries, heap allocation is enforced via two complementary mechanisms: (1) *structural* — the `GpuSafe` type check rejects heap-allocated types (`String`, `Vec[T]`, etc.) so any function that uses them fails compilation with a clear diagnostic; (2) *project profiles* — `no_effects = ["allocates(Heap)"]` in `kara.toml` removes the default permit rule, causing undeclared `allocates(Heap)` to be flagged as an error on public functions. **Allocation failure** is orthogonal to allocation permission: the `panic_on_alloc_failure` profile flag (default `true`) controls whether OOM panics or returns `Result[(), AllocError]` from `try_*` companion methods — see [Fallible-Allocation Mode](#fallible-allocation-mode) for the full split.

### Browser GPU Graphics Library (`kara-gfx` or equivalent)

A graphics and adjacent-real-time-media library equivalent to Rust's `wgpu` (~15k LoC) — textures, render passes, vertex / fragment pipelines, depth / stencil, swapchains, plus the adjacent Web Audio / Gamepad / PointerLock surfaces that real games and interactive apps need.

**Distinct from** the GPU *compute* story in [GPU Subset Constraints](#gpu-subset-constraints) — compute-shader codegen (`#[gpu]` + `gpu.dispatch`) is a Kāra-compiler feature of this track. GPU *graphics* (render passes, pipelines, textures, swapchains) is a library built on top of that substrate plus WebGPU / Vulkan / Metal / DX12 bindings per target.

**What it rests on:**
- [GPU Subset Constraints](#gpu-subset-constraints) + `#[gpu]` compute shaders (for any shader authoring in the library).
- [design.md §15 Host functions](design.md#host-functions) (for WebGPU / Vulkan / Metal / DX12 host bindings — the library would expose one portable API that lowers to whichever host is available per target).
- [Web / Host Effect Vocabulary](#web--host-effect-vocabulary) (a `Gpu` resource alongside `Display` if WebGPU is treated as distinct from compute-side `GpuBuffer[_]`, or reuse of the compute-side resource if the distinction isn't useful — library decides).
- The [web track](#web)'s browser target for browser delivery.

**Why deferred, and why not a standard-library feature:**
1. Graphics API design is a full domain-specific design project (see `wgpu`'s multi-year evolution). The stdlib cannot absorb that cost.
2. The compute / graphics split is itself an open question — reusing the existing compute-side GPU resource vs. introducing a distinct graphics resource is a decision that benefits from ground-truth usage data before being frozen.

**Pre-build checklist:**
- [ ] Browser target shipped and stable
- [ ] `#[gpu]` compute-shader codegen shipped (SPIR-V / WGSL emission)
- [ ] `host fn` lowering on WASM stabilized (for WebGPU bindings)

**Cross-reference:** [GPU Subset Constraints](#gpu-subset-constraints) — the compute-side foundation.

### Heterogeneous Compute — Beyond CPU + GPU

**Decision:** Defer heterogeneous-compute capability beyond CPU + GPU. CPU SIMD ([Portable SIMD](#portable-simd--vectort-n)) and GPU codegen (wgpu/WGSL primary, NVPTX opt-in) form a capability surface — but **not** a positioning axis: the project rejects "Kāra for AI" and "Mojo competitor" framings. Further accelerators, kernel fusion, and unified-memory abstraction are later work, scheduled per the sub-item promotion gates below.

**Why deferred.** Each sub-item has real engineering cost and a small population of users given Kāra's positioning. Building them early would compete with the general-purpose floor for engineering bandwidth and would signal a positioning shift the project has explicitly rejected. None are precluded by the architecture — they are additive capabilities, deferrable without design debt.

**Sub-items (each independently promoteable):**

- **NPU / ANE backend.** Apple Neural Engine (CoreML), Qualcomm AI Engine, modern Snapdragon NPUs, Intel AMX. Codegen target: a new dialect / IR layer or direct lowering through MLIR (see [MLIR Adoption as Codegen Substrate](#mlir-adoption-as-codegen-substrate)). **Promotion gate:** on-device inference becomes a Kāra workload class with concrete users (not before).
- **TPU backend.** Google Cloud TPU via XLA / OpenXLA HLO. **Promotion gate:** Kāra develops a Google-Cloud-resident user base willing to fund the toolchain work (small audience; unlikely without a positioning shift).
- **FPGA bitstreams.** Listed as a future goal under [Compilation Target Flexibility](#compilation-target-flexibility). **Promotion gate:** not before stable CPU and GPU codegen, and not before a concrete FPGA workload exists.
- **Unified-memory abstraction.** Apple M-series, integrated GPUs, AMD APUs all share physical RAM between CPU and GPU; the `Tensor.on(gpu)` / `.to_cpu()` boundary ops on those platforms could be zero-copy. Industry-standard frameworks (NumPy + CuPy, PyTorch `mps`) don't unify this cleanly. **Promotion gate:** GPU codegen has shipped and produced runtime data on transfer overhead on M-series / APU platforms; concrete workloads exist where the unification matters.
- **Kernel fusion compiler pass.** Automatic fusion of adjacent elementwise + reduction kernels to amortize GPU launch overhead and reduce VRAM round-trips. The optimization that separates "uses a GPU" from "uses a GPU well" — `torch.compile` / `XLA` / `JAX` all do this. **Promotion gate:** (a) GPU codegen has shipped and produced data on the launch-overhead ceiling for representative Kāra ML workloads, AND (b) MLIR adoption has been scheduled OR an LLVM-based fusion pass has been spec'd. Without (b), the engineering cost is multi-year. The MLIR `linalg` dialect is the obvious substrate.

**Why non-breaking:** All sub-items are additive capabilities. Each composes with the existing `Tensor` / `GpuTensor` / `Vector[T, N]` surface and the trait-dispatched ops (`Reduce`, `ElementwiseMap`, etc.). New backend = new lowering target; new optimization pass = new IR pass. No source-language changes implied.

**Cross-reference:** [Compilation Target Flexibility](#compilation-target-flexibility); [MLIR Adoption as Codegen Substrate](#mlir-adoption-as-codegen-substrate) (the paired substrate question).

## Comptime

Compile-time evaluation: `comptime fn`, the reflection it needs, and the effects it may have.

### Comptime — AST→AST `comptime fn`

**Why AST→AST.** Kāra's metaprogramming surface must cover three jobs that mainstream languages typically split across separate mechanisms: value-level compile-time computation (Rust's `const fn`), derive macros (Rust's proc-macro crates), and code generation (Rust's `build.rs` + `proc-macro2`). Splitting these would force three separate sub-languages — a value subset, a procedural macro DSL, and ad-hoc build scripts. AST→AST `comptime fn` collapses all three into one mechanism, written in Kāra itself with the same type system, the same diagnostics, and the same effect surface. The cost is a larger upfront spec; the benefit is one language surface for everything that runs at compile time, which matches Kāra's stance on LLM-written code (single surface = simpler synthesis target) and on full-feature-up-front design.

#### Surface forms

Comptime introduces three syntactic forms, all gated by the `comptime` keyword (reserved in v1; see [design.md §3](design.md#reserved-for-future-use)):

```kara
// 1. Function declaration — body runs at compile time when called from a comptime context.
comptime fn build_lookup_table(size: i64) -> Array[i64, size] { ... }

// 2. Block expression — forces compile-time evaluation of the inner expression/block.
let table = comptime { build_lookup_table(1024) };

// 3. Parameter prefix — argument must be a comptime-known value.
comptime fn matrix[const ROWS: i64, const COLS: i64](
    comptime kind: MatrixKind,
    init: Fn(i64, i64) -> f64,
) -> Matrix[ROWS, COLS] { ... }
```

The three forms compose. A `comptime fn` may call ordinary `fn`s — but only those whose effects are subset-restricted to the comptime-permitted set (see *Effects* below). An ordinary `fn` may call a `comptime fn` only inside a `comptime { ... }` block or by binding the result to a `static` / `const generic argument` / `default parameter value` — the boundary is explicit, never implicit.

**Definition-time validation of metavariable specifiers.** Every `comptime fn` parameter must carry a type annotation at the declaration site — no anonymous parameter form, no inferred-from-call-site shape. The rule is already implicit in Kāra's broader function-parameter rules (every fn parameter requires a name and type; comptime fn participates in the same rule). The rejection diagnostic is `error[E_MISSING_FRAGMENT_SPECIFIER]: comptime fn parameter '<name>' must declare a fragment specifier — annotate with a typed AST shape (`Expr`, `Stmt`, `Type`, etc.) at the declaration` and fires at the comptime fn's definition site, never at a call site. This is the load-bearing answer to the Rust pre-1.55 footgun where macro definitions could omit fragment specifiers on metavariables and surface mysterious matching failures at every call. Kāra forbids the omission outright at the declaration site so the bug's evidence is local to the macro definition, not scattered across calls. The general principle is stated under *Forward-commitment* at the end of this entry.

#### Types as first-class values

At comptime, types are values of the built-in pseudotype `Type`. This is the central enabling fact for AST→AST work: a `comptime fn` can take a `Type` as a parameter, inspect its structure, and emit code parameterized by it.

```kara
comptime fn print_fields(comptime T: Type) {
    for field in T.fields() {
        compiler.print(f"  {field.name}: {field.ty.name()}");
    }
}

comptime { print_fields(User) };   // prints User's fields at build time
```

`Type` values are first-class only at comptime — they cannot appear in runtime expressions. A runtime function may not take a parameter of type `Type` (it would be a value-level reference to a compile-time-only value). The boundary is enforced by the typechecker: a `Type` value flowing into a non-comptime context is a compile error with diagnostic `error[E_TYPE_VALUE_AT_RUNTIME]`.

The `Type` pseudotype's reflection surface — `fields()`, `variants()`, `methods()`, `name()`, `size_of()`, `align_of()`, `is_struct()`, `is_enum()`, `is_union()`, `is_generic()`, `generic_args()`, `attributes()` — is fixed by the language; user code reads it but cannot extend it.

#### Reflection API

The reflection API exposes the program tree to comptime code as ordinary Kāra values. The full surface is rooted at the `compiler` module (a comptime-only prelude module — see *Comptime stdlib surface* below):

| API | Returns | Description |
|---|---|---|
| `T.fields() -> Slice[Field]` | per struct | iterable list of `Field { name, ty, vis, attributes }` |
| `T.variants() -> Slice[Variant]` | per enum | each variant exposes its fields |
| `T.methods() -> Slice[Method]` | per type | methods declared on `T` (inherent + visible trait impls) |
| `T.attributes() -> Slice[Attribute]` | per item | the `#[...]` attributes attached at the declaration site |
| `T.name() -> String` | per type | canonical fully-qualified name |
| `T.size_of() -> i64` | per sized type | runtime size in bytes |
| `T.align_of() -> i64` | per sized type | runtime alignment |
| `compiler.current_module() -> Module` | global | the module the calling site lives in |
| `compiler.callsite_location() -> SourceLocation` | global | file/line/column of the comptime invocation |
| `compiler.diagnostic(severity, span, message)` | global effect | emit a build-time diagnostic at a chosen span |

The reflection API is read-only on the existing program tree. Code generation goes through the AST builder API.

#### AST builder API

A `comptime fn` emits code by constructing AST values and either returning them (when the function appears in declaration position) or invoking compiler-provided emit operations. The AST node types — `Expr`, `Stmt`, `Item`, `Pattern`, `Type`, etc. — are stdlib-defined enums with one variant per AST shape; their definitions are part of the comptime stdlib surface.

```kara
// Stdlib (sketch — comptime-only module `compiler.ast`):
shared enum Expr {
    Literal(LiteralValue),
    Variable(Ident),
    Call { callee: Expr, args: Vec[Expr] },
    Block { stmts: Vec[Stmt], tail: Option[Expr] },
    /* ... */
}

enum Item {
    Fn(FunctionDef),
    Struct(StructDef),
    ImplBlock(ImplBlock),
    /* ... */
}
```

A derive desugars to a call to a `comptime fn` that takes the target type as a parameter and returns a `Vec[Item]` to splice into the surrounding module:

```kara
// Stdlib derive — `#[derive(Eq)]` on a struct desugars to a call to this fn.
comptime fn derive_eq(comptime T: Type) -> Vec[Item] {
    let body = T.fields()
        .map(|f| ast.expr(f"self.{f.name} == other.{f.name}"))
        .reduce(|a, b| ast.expr(f"({a}) and ({b})"))
        .unwrap_or(ast.expr("true"));

    [ast.impl_block(
        target: T,
        traits: [ast.path("Eq")],
        items:  [ast.method("eq", [("self", ast.ref_self()), ("other", ast.ref_t(T))],
                             ast.bool_ty(), body)],
    )]
}
```

The AST builder offers two surfaces for constructing nodes: a **typed builder** (`ast.expr(...)`, `ast.method(...)`, etc. — checked at definition site, no string concatenation) and a **quasi-quote** form (`ast.expr("self.{f.name} == other.{f.name}")` — string interpolation with embedded comptime values, parsed at build time). Quasi-quote is the ergonomic form; the typed builder is the form for programmatic construction over arbitrary shapes.

#### Code generation and derive desugaring

`#[derive(Trait1, Trait2, ...)]` on a struct/enum desugars to one `comptime fn` invocation per derive name. Each derive resolves to a `comptime fn` named `derive_<TraitName>` (snake-case) that must:

- Take exactly one parameter: `comptime T: Type`.
- Return `Vec[Item]` — the items to splice into the same module.
- Live in the same module as the trait (lookup by lexical sibling), or be re-exported under the trait's path.

Built-in derives (`Eq`, `Hash`, `Display`, `Debug`, `Clone`, `Copy`, `PartialEq`, `PartialOrd`, `Ord`, `Arithmetic`, `Serialize`, `Deserialize`) are all stdlib `comptime fn`s with no special compiler treatment beyond the lookup convention. User-defined derives use the same mechanism — there is no separate "proc macro" sub-language.

Splice rules: items returned from a `comptime fn` invoked via `#[derive]` are spliced *after* the derive site at module scope. They can reference items declared earlier in the module but not items declared later (one-pass module-level resolution preserves source-order semantics).

#### Effect system integration

Comptime effects live in their own resource family, distinct from runtime resources:

| Effect | Verb | Meaning |
|---|---|---|
| `reads(CompileTimeEnv)` | reads | inspect compiler state — module table, type registry, attribute reads |
| `writes(CompileTimeEnv)` | writes | emit diagnostics, record metadata for later compilation phases |
| `allocates(CompileTimeHeap)` | allocates | comptime-heap allocation for buffers, AST nodes, intermediate vectors |
| `panics` | panics | a comptime panic is a **compile error**, not a runtime panic — the diagnostic surfaces at the calling site |

All runtime resource verbs (`reads(File)`, `writes(Network)`, `sends(Channel)`, ...) are forbidden inside `comptime fn` — calling a runtime-effectful function from a comptime context is `error[E_RUNTIME_EFFECT_AT_COMPTIME]`. Execution verbs (`blocks`, `suspends`) are forbidden too. The comptime evaluator runs synchronously inside the compiler; there is no scheduler, no I/O, no FFI.

`CompileTimeEnv` and `CompileTimeHeap` are reserved built-in resource names — reserved in v1 ([design.md §12](design.md#built-in-resources)); see also [Comptime Effect Defaults](#comptime-effect-defaults). When called from a runtime context (via `comptime { ... }` or static initializer), comptime effects are *stripped* — the call site does not need to declare `reads(CompileTimeEnv)` because the work happens before the binary exists.

Cross-reference rule for the embedded/kernel profile: those profiles forbid `allocates(Heap)` but permit `allocates(CompileTimeHeap)`. A comptime fn that builds a 4 KB lookup table at build time and emits it as a `static` array is valid in both `embedded` and `kernel` profiles — the heap allocation happened in the compiler, not on-device.

#### Const-generic, refinement, and default-value integration

Three features lower to the comptime evaluator under the hood — once comptime ships, the const-evaluator they depend on stops being a special-case mechanism and becomes a degenerate use of the comptime evaluator:

- **Const generic arguments** ([design.md §8](design.md#const-generic-parameters)): the expression in const-arg position is a comptime expression. In v1 the evaluator handles constant expressions only, and calls wait for this track; once comptime lands, any `comptime fn` call is permitted in this position.
- **Refinement-type predicates** ([Refinement Types](#refinement-types), verification track): predicate evaluation at binding sites uses the comptime evaluator.
- **Default parameter values** ([design.md §7](design.md#named-and-default-parameters)) and `const` initializers ([design.md §4](design.md#constants)): v1 says calls to `const fn` come with the comptime track. That `const fn` is this entry's `comptime fn`, a single mechanism.

This composition is intentional: v1's restrictive const-eval is a forward-compatible subset of the comptime evaluator. Code written against v1's const-eval continues to compile after comptime ships; the surface only widens.

#### Hygiene rules

Identifiers emitted by a `comptime fn` resolve at the *invocation site*, not the *definition site*, with two exceptions:

1. **Names that are unambiguous at the definition site** — references to stdlib items, items from the comptime fn's own module — resolve at the definition site and are stable across invocation sites.
2. **Names introduced *inside* the emitted code** — a `let` binding emitted by the comptime fn — are scoped to the emitted item, and the comptime fn is responsible for picking names that don't collide with surrounding bindings (the `compiler.fresh_ident()` builder helper produces guaranteed-fresh identifiers).

References to *types*, *traits*, *struct fields*, and *enum variants* always resolve at the invocation site — they're the natural reference targets for derives and template-style code generation. References to *functions* resolve at the invocation site by default; the `ast.path("module.name")` builder fixes resolution at the definition-site path when the comptime fn wants a stable reference.

This hygiene model is closer to scheme/clojure's syntax-case than to C macros — every identifier has a tracked origin, and accidental capture is the exception rather than the norm. It is more permissive than Rust's macro hygiene (which is fully hygienic by default) because Kāra's comptime fns take typed parameters and return typed AST values; the type system catches many mistakes that hygiene rules would have to catch in an untyped macro system.

#### Resource limits

The comptime evaluator runs inside the compiler with hard ceilings:

- **Iteration limit** — `2^24` total instructions per top-level `comptime` invocation. Configurable via `--comptime-iter-limit=N` for build-time tuning, but the default is the language commitment. Exceeding the limit produces `error[E_COMPTIME_ITER_LIMIT_EXCEEDED]` with a stack trace of the comptime call chain.
- **Memory limit** — `512 MiB` of `CompileTimeHeap` allocation per top-level invocation. Configurable via `--comptime-heap-limit=N`. Exceeded ⇒ `error[E_COMPTIME_HEAP_LIMIT_EXCEEDED]`.
- **Recursion limit** — 1024 frames of comptime call depth. Configurable. Exceeded ⇒ `error[E_COMPTIME_RECURSION_LIMIT_EXCEEDED]`.

Cycle detection: the evaluator tracks the in-flight comptime call set and rejects mutual recursion that doesn't terminate (a `comptime fn` calling itself with the same arguments produces `error[E_COMPTIME_INFINITE_RECURSION]` once detected, typically within ~16 stack frames).

#### Tooling integration

- `karac doc` — comptime fns are documented like ordinary fns, with an extra "comptime" badge. Items emitted by derives are documented in the type's doc page under "Auto-generated impls", with a hyperlink to the derive's source.
- `karac explain --expand <span>` — at any source span containing a derive or comptime block, prints the post-expansion AST that the comptime fn produced. This is the answer to "what did `#[derive(Eq)]` actually generate?" — readable, line-numbered, identical to what the rest of the compiler sees.
- Debugger — comptime evaluation is visible to the `karac` debugger as a dedicated comptime frame stack; breakpoints, stepping, and variable inspection all work the same way as for runtime code.
- `karac query monomorphization` — comptime invocations are tracked alongside type-parameter monomorphizations in the per-instance identity tuple `(T1..Tk, const C1..Cm, comptime args...)`.

#### Comptime stdlib surface

A `compiler` module (and its `compiler.ast` submodule) is added to the comptime-only prelude. Importing it from runtime code is `error[E_COMPTIME_MODULE_AT_RUNTIME]`. Module contents (sketch):

- `compiler.print(s: String)` — emit text to the build log
- `compiler.diagnostic(severity, span, message)` — emit a diagnostic
- `compiler.fresh_ident() -> Ident` — guaranteed-fresh identifier
- `compiler.callsite_location() -> SourceLocation`
- `compiler.current_module() -> Module`
- `compiler.ast.expr(...)`, `compiler.ast.stmt(...)`, `compiler.ast.item(...)`, etc. — typed builders
- `compiler.ast.parse_expr(s: String) -> Result[Expr, ParseError]` — quasi-quote shim

The `compiler` module is small at the surface — fewer than fifty exported items — because the heavy lifting happens through ordinary value-level code on `Type` and AST values. It is *not* a procedural-macro library; it is a thin window into compiler state plus the AST node constructors.

#### Implementation phases

Comptime ships as a single complete unit. The implementation has four discrete substrates that can be built in order:

1. **Comptime evaluator.** A treewalk interpreter over the typed AST, implementing the runtime-language subset (everything in `comptime fn` bodies that doesn't touch the AST or type-as-value). This is essentially the existing interpreter retargeted to compile-time invocation.
2. **`Type` as first-class value + reflection API.** The compiler's existing type registry exposed as immutable `Type` values; field/variant/method iteration; size/align queries.
3. **AST builder + emission.** Stdlib `compiler.ast` module; typed builder API; quasi-quote parser; splicing rules at the module level.
4. **Derive desugaring.** Replaces compiler-built-in derives with stdlib `comptime fn` calls; adds the lookup convention for user-defined derives.

Substrates 1+2 enable value-level comptime computation and type-inspection diagnostics. Substrate 3 enables programmatic code generation. Substrate 4 unifies the existing derive surface. Every substrate is internally complete on its own — partial deployment never leaks a half-built feature into the language.

#### Comptime Effect Defaults

v1 reserves the resource names `CompileTimeEnv` and `CompileTimeHeap` ([design.md §12](design.md#built-in-resources)). This subsection restates the effect rules of the comptime surface above.

**Effect rules.**

1. **Permitted verbs:** `reads(CompileTimeEnv)`, `writes(CompileTimeEnv)`, `allocates(CompileTimeHeap)`. `panics` is a compile error (a comptime panic surfaces at the calling site as a build-time diagnostic). All runtime resource verbs (`reads(File)`, `writes(Network)`, `sends`, `receives`, etc.) forbidden — `error[E_RUNTIME_EFFECT_AT_COMPTIME]`. Execution verbs (`blocks`, `suspends`) forbidden.
2. **`CompileTimeHeap` is distinct from `Heap`.** `embedded`/`kernel` profiles forbid `allocates(Heap)` but permit `allocates(CompileTimeHeap)`.
3. **Parameter modes unchanged.** Same ownership semantics as runtime functions.
4. **Stripped effects at runtime boundary.** Comptime effects do not propagate to runtime callers — a `static X = comptime { ... }` does not require its enclosing scope to declare `reads(CompileTimeEnv)`.
5. **Reserved names.** `CompileTimeEnv` and `CompileTimeHeap` are reserved from v1 onward.

```kara
comptime fn build_lookup_table(size: i64) -> Array[i64, size]
    with reads(CompileTimeEnv) allocates(CompileTimeHeap)
{
    let mut table = Array.new();
    for i in 0..size {
        table.push(expensive_computation(i));
    }
    table
}

fn lookup(idx: i64) -> i64 {
    static TABLE: Array[i64, 1024] = comptime { build_lookup_table(1024) };
    TABLE[idx]    // pure read — comptime effects stripped at the boundary
}
```

**Forward-commitment — definition-time fragment-specifier validation.** When Kāra eventually ships a metaprogramming surface that uses fragment specifiers (whether the form is `comptime fn` parameter types, as above, or a future `macro_rules!`-style pattern-matching macro system), every metavariable specifier MUST be validated at the **definition site**, not at the use site. A macro definition that omits or malforms a metavariable's specifier produces a hard error at the macro's declaration, never a deferred error at every call site. **Why** (rationale): Rust pre-1.55 accepted macro definitions like `macro_rules! foo { ($x) => { ... } }` (no `:expr` / `:tt` / etc. specifier on `$x`) and surfaced the error only at call time, with the error message pointing at the call site rather than the broken macro. The result was confusing diagnostics that scattered the bug's evidence across every call. Kāra commits to definition-time rejection from day one — when the metaprogramming surface ships, every specifier is validated at the macro's `comptime fn` parameter list / pattern shape / equivalent declaration form, with a focused diagnostic at the declaration site naming the missing or malformed specifier. **Status under `comptime fn`:** the rule is already implicit — `comptime fn` parameters require type annotations per the existing function-parameter rules. A `comptime fn my_macro(e)` (no type) is rejected at definition by the existing typechecker; this entry pins that the rule is intentional and applies to every future metaprogramming form. **Status under hypothetical `macro_rules!`** (not committed for any release): if such a system ever ships, the parser rejects `macro_rules! foo { ($x) => { ... } }` at the macro's declaration site with `error[E_MISSING_FRAGMENT_SPECIFIER]: metavariable '$x' must have a fragment specifier ('$x:expr', '$x:tt', etc.); the error fires here at the macro definition because every call site would otherwise see a mysterious matching failure`.

## dyn

Trait objects (`dyn Trait`), the effects of calls through them, and trait-level effect ceilings. The roadmap brings `dyn` with the services track (M4a); its design is kept as a track of its own because it touches traits, effects and codegen together.

### Effects of `dyn Trait` Calls

> In v1, a trait method's `with` clause is a ceiling that every impl stays within, and a trait method with no clause has no ceiling: a call through a bound has the effects of the instance's method, charged to whoever instantiates the generic function ([design.md §12](design.md#trait-methods-and-generic-calls)). A `with` clause on the `trait` header and every rule for `dyn Trait` below are this track's additions.

**Interaction with `dyn Trait`.** Trait method ceilings form an upper bound; impls may narrow individual methods (effect subtyping), and `dyn Trait` call sites must declare at least the trait-level bound for any method they may invoke. Static dispatch through `T: Trait` uses the per-method declaration — also narrower than the ceiling — by the same rule. See the `dyn Trait` subsection below for the worked example.

**`dyn Trait` (dynamic dispatch).** Dynamic dispatch hides the concrete impl at the call site, so effect analysis cannot resolve an effect variable at monomorphization — there is no monomorphization. The trait definition must commit to an effect contract that every impl, present and future, is bound by. Kāra uses **trait-level effect bounds**: the contract is stated once at the trait definition, not inferred from the set of known impls across the program. This is the standard treatment for trait-object effect contracts under separate compilation, and it is compatible with incremental builds and downstream libraries adding impls.

**Why the keyword is explicit.** Kāra requires the literal `dyn` in every trait-object type; a bare trait name in a type position is a compile error. Explicit keying makes the choice of dynamic dispatch — and, for owned positions, the heap allocation it implies — visible at the type itself rather than inferred from cross-file knowledge of whether the name denotes a trait or a struct. This is consistent with the language's broader "costs are legible" posture (effects, ownership modes, `shared`, `allocates(Heap)`), gives renames and trait-for-struct swaps a noisy diagnostic instead of a silent semantic change, and pairs symmetrically with the `impl Trait` keyword for existentials — both polymorphism choices surface at the type itself (see [design.md §9](design.md#9-traits)).

**Two levels of declaration.** A trait may declare effects at two levels, which serve different call-site modes:

1. **Trait-level bound** — the *ceiling*. The union of everything any method of any impl may do. This is what `dyn Trait` call sites use, because the concrete method being invoked may not even be statically known (different code paths may call different methods).
2. **Per-method declaration** — the *actual* effect set of an individual method, tighter than or equal to the trait-level bound. This is what static-dispatch call sites use when the method is called through a `T: Trait` generic bound.

```
trait Storage with reads(Data) writes(Data) {
    fn load(ref self, key: Key) -> Option[Value] with reads(Data);
    fn save(ref self, key: Key, value: Value) with writes(Data);
    fn exists(ref self, key: Key) -> bool with reads(Data);
}
```

The trait-level `with reads(Data) writes(Data)` is the union. `load` and `exists` narrow to `reads(Data)` only; `save` narrows to `writes(Data)` only. The trait-level bound must be the superset (or equal) of the per-method union — it is not allowed to be tighter, because `dyn Trait` must safely cover any method the caller might invoke.

**Impls must fit each method's declaration.** An impl is checked against the per-method declaration, not only against the trait-level bound. An impl method with effects that exceed *any* of the trait method's declared effects is a compile error at the impl site:

```
impl Storage for NetworkBackedStore {
    fn load(ref self, key: Key) -> Option[Value]
        with reads(Data) sends(Network)   // ✗ error: sends(Network) is outside
                                           //   Storage.load's declared reads(Data)
    { ... }
}
```

The diagnostic points at the impl method, names the extra effect, and quotes the trait method's declared effects. Downstream libraries cannot widen the effect surface by adding a new impl, which is what makes the open-world model safe under separate compilation.

**Static dispatch narrows to per-method declarations.** A generic function `fn f[T: Storage](s: ref T)` that calls `s.load(key)` sees `reads(Data)` — the method-level declaration — not the full trait-level bound. The effect system treats the method declaration as the bound for `T: Storage`; auto-concurrency at the monomorphization site may further narrow using the impl's inferred effects.

```
fn read_key[T: Storage](store: ref T, key: Key) -> Option[Value] with reads(Data) {
    store.load(key)   // method-level reads(Data), not the trait-level union
}
```

**`dyn Trait` call sites use the trait-level bound.** When the concrete type is erased, the caller's declared effects must cover the trait-level bound of every method it invokes:

```
fn sync(store: ref dyn Storage, input: ref Input) -> Result[(), AnyError]
    with reads(Data) writes(Data)     // trait-level bound, not impl-level
{
    if let Some(existing) = store.load(input.key) {
        if existing != input.value {
            store.save(input.key, input.value);
        }
    }
    Ok(())
}
```

Calling only `store.load` on a `dyn Storage` still requires `reads(Data)` (the method's declared effect), not the full trait-level bound — per-method declarations are load-bearing for `dyn` callers that invoke only a subset of the methods.

**Unbound traits.** A trait declared without any `with` clause has no effect ceiling. A static call through such a trait follows the v1 rule: it has the effects of the instance's method. A `dyn Trait` call to such a trait has no instance to read effects from.

> **Open.** What a `dyn` call to an unbound trait may do. The choices are to require a trait-level bound before a trait can be used as `dyn`, to give such a call every effect so that the caller must declare them, or to carry its effects in a named effect variable ([Named Effect Variables](#named-effect-variables)).

**Viral annotation extends to `dyn Trait`.** Calling a method on `dyn Trait` from a public function requires the caller's signature to cover the relevant effects explicitly — either the per-method effects (if only some methods are called) or the trait-level bound (if the set is open), or an effect variable. Nothing is implicitly inherited from the trait definition. `dyn Trait` is not a back door that launders effects through the vtable.

**Trait-level `with` clauses are defaults for methods that declare no `with` of their own.** A `with` clause written on the `trait` header applies to every method that omits its own `with`. Method-level `with` clauses fully *replace* the trait-level default for that method — they do not union with it:

```kara
trait Reader with reads(DB) {
    fn access(ref self, key: Key) -> Value;             // ceiling: reads(DB) (from trait)
    fn stream(ref self) -> Iterator[Value] with sends(Wire);
    // stream's ceiling is sends(Wire) — NOT reads(DB) + sends(Wire).
}
```

Rationale: method-level `with` is the authoritative declaration. The trait-level shorthand exists to reduce stutter when a trait's methods share a common effect envelope. Unioning would hide the authoritative declaration behind implicit trait-level context; replacement keeps each method's ceiling readable from its own signature alone.

**Private traits infer effects from local impls.** A private (module-internal or package-internal) trait need not declare a ceiling. The compiler infers each method's effect set from the union of per-method effects across all impls visible in the same compilation unit, then propagates the inferred ceiling to call sites. There is no cross-package dimension because private traits are not exported — `pub` is the gate that turns an effect ceiling from an inferred internal detail into a published commitment. Inference uses the same subset-subtyping rule at call sites: a generic call through a private trait sees the inferred ceiling, and auto-concurrency may further narrow at monomorphization using the specific impl's effects.

**Private-trait inference is SCC-aware.** A private trait's per-method ceilings participate in the same strongly-connected-component pass as private `fn` inference ([design.md §12](design.md#inference-and-declarations), mutual recursion rules). Impl bodies that call through the trait, and default method bodies that recurse into impls, are resolved simultaneously. An SCC containing trait-method nodes and impl-body nodes produces a single fixed-point ceiling per method.

**A private trait with no visible impls has an unbound ceiling.** The compiler does not invent an empty effect set for such a trait. An unimpled private trait is treated as effect-opaque, the same rule that applies to a `pub trait` with no declared ceiling. This is the forgiving choice: work-in-progress traits do not trigger surprise errors at every call site until the first impl lands.

**Public traits with no declared effect ceiling are effect-opaque.** A public trait that declines to declare any `with` clause has no ceiling at all. This is the unbound-trait case above. This closes the "forgot to declare" loophole: the language never silently admits widening through an omitted ceiling. A library author who wants precise effect tracking declares the ceiling; one who wants to stay open declares nothing and accepts what the unbound-trait rule gives callers. Both are sound; the choice is ergonomic.

## Layout

Layout blocks: a separate description of how a collection of structs is stored in memory (field groups, cold fields, alignment), applied without changing the logical type.

<a id="feature-1-data-layout-as-a-first-class-concept-opt-in"></a>
### Layout Blocks

Separates **logical structure** from **physical layout**:

```
struct Entity {
    id: u64, name: String,
    position: Vec3, velocity: Vec3,
    health: f32, armor: f32, is_alive: bool,
}

layout entities: Vec[Entity] {
    group physics { position, velocity }      // hot path: physics tick
    group combat  { health, armor, is_alive } // hot path: combat
    cold          { id, name }                // separate allocation; never touched in hot loops
}
```

The programmer writes code against `Entity`. The compiler generates code that accesses the correct physical layout. Iterating over `entities` touching only `position` and `velocity` loads only the `physics` group into cache.

**What this feature delivers.** Three layout directives — `group` (SoA field grouping), `cold` (separate allocation for infrequently-accessed fields), `align(N)` (cache-line alignment for false-sharing avoidance) — applicable to any collection binding, including collections of types declared in external packages. The library always sees its own AoS layout unchanged; only the consumer generates a specialized monomorph. `karac explain` analyzes your loops and suggests a concrete layout block. You write it; the compiler generates the code.

#### Layout Rules

- **Default: AoS** (array-of-structs). No compiler auto-detection of SoA. Performance notes suggest opportunities.
- **Scope:** Homogeneous collections only. Not for Map, trees, or pointer-based structures.
- **Generics:** Monomorphized per layout variant. Layout is part of the concrete type at codegen level.
- **Visibility:** Layout is project-internal (not exposed to end users). Public APIs use logical struct types. Changing layout is not an API break.
- **Enums:** `split_by_variant` is the only layout directive for enum types; `group` is not allowed in an enum layout block. `split_by_variant` stores each variant contiguously for cache-friendly iteration. Exception: a single-variant enum is treated as a struct for layout purposes — `split_by_variant` is a no-op on it and `group` is allowed instead. A zero-variant enum layout block is a no-op (no instances exist to lay out). Per-variant field grouping (nesting `group` inside a specific variant) is not currently supported; if needed, define a struct for each variant's payload and apply a separate layout block to each.
- **Unassigned fields:** Fields not listed in any `group` or `cold` section are collected into an implicit trailing hot group, stored after all explicitly named groups. The implicit group is an ordinary group — same physical representation, same indexing, same drop semantics as explicitly named groups, with no special-case codegen path. The compiler emits a warning once per layout block listing the unassigned fields — suppress with `#[allow(layout_unassigned_fields)]` when the omission is intentional. A field listed in more than one group is a compile error.
- **`cold` section:** A layout block may contain at most one `cold` section; a second `cold` section in the same block is a compile error. Fields listed in `cold` are moved to a separate heap allocation — they are not part of the main SoA groups. Hot loops that never access cold fields pay no cache cost for them. A field may not appear in both a `group` and the `cold` section — that is a compile error. Unassigned fields fall into the implicit trailing hot group, not into `cold`. Borrows into cold fields and borrows into any `group` do not alias (same disjointness guarantee as cross-group borrows). Field access syntax is identical whether the field is hot or cold — the compiler generates the correct pointer arithmetic transparently.
- **Heap-allocated fields in layout blocks.** A `group` directive that names a heap-allocated field (e.g., `cells: Vec[bool]`) controls only the placement of that field's *header* in the struct's SoA layout — the three-word `{ ptr, len, cap }` header moves into the group's backing array, but the actual heap buffer is unaffected. For most structs this provides modest header-locality benefit, not the primary motivation for SoA grouping. An `inline {}` directive that collapses a heap buffer directly into the struct (a fixed-capacity inline array) is not part of this design; use `Array[T, N]` for fixed-size in-struct storage, which carries no heap pointer and groups correctly under `group`.
- **`align(N)` modifier on a group:** A group may carry an alignment modifier — `group a { thread_a } align(64)` — forcing that group's backing array in the SoA allocation to start on an `N`-byte boundary. `N` must be a power of two. Common values: `64` (x86/x86-64 cache line), `128` (Apple Silicon cache line). The primary use case is false-sharing avoidance: fields written by separate threads, placed in separate groups each with `align(64)`, are guaranteed to occupy distinct cache lines. `align(N)` on a group is independent of `#[repr(align(N))]` on the struct — `#[repr]` aligns the struct type itself; `align(N)` aligns the group's backing array within the collection.
- **GPU mapping:** Layout groups map directly to GPU buffers with coalesced access.
- **Struct construction:** Struct literals always use source field order — the order fields are declared in the `struct` definition. The layout block affects only physical memory organization; it has no effect on construction syntax or field access order in code. This means a layout group reorganization is always a zero-diff change to existing construction sites.
- **References into SoA-laid-out collections:** `ref entities[i].field` and `mut ref entities[i].field` return a reference into the relevant group's array; standard borrowing rules apply within each group. `ref entities[i]` (a reference to a whole element) is a **compile error** — a single logical element does not exist as a contiguous memory region under SoA, so no reference can name it. Code that needs a whole-element value binds it: `let e = entities[i]` materializes an AoS copy. The same rule applies to `ref enum_value` taken from a collection laid out with `split_by_variant`.
- **Cross-group disjointness (borrow checker):** two borrows into *distinct groups* of the same SoA-laid-out collection do not alias, even at the same index. `mut ref entities[i].position` and `ref entities[j].health` can coexist for any `i`, `j`. The ownership checker encodes cross-group disjointness as a fact, so idiomatic code such as `for e in entities { e.position += e.velocity; read_health(e.health) }` does not produce spurious borrow errors. Within a single group, normal aliasing rules apply.
- **Layout binds to the binding site, not the type globally.** Writing `layout entities: Vec[Entity] { group ... }` makes the name `entities` a SoA-laid-out collection; the underlying struct `Entity` retains its default AoS view everywhere else — including inside the library that declared it. A consuming project may apply a layout block to any struct type, including types declared in external packages; only the consuming project generates a new codegen monomorph for the chosen layout. The library's own compilation unit always sees the AoS layout of its types unchanged. Fields named in a cross-package layout block must be `pub` at the layout site; referencing a private or nonexistent field is a **compile error**. If a library upgrade renames or removes a field referenced in a consumer's layout block, the consumer gets a compile error at the layout block — not a silent fallback to AoS. Within a project, two layout blocks for the same collection type with different groupings produce distinct codegen monomorphs.
- **Layout-annotated collections are the same type as their plain counterpart.** `Vec[Entity]` with a SoA layout block and `Vec[Entity]` without are the same *type* at the type-system level — they are spelled identically and are type-compatible at every call boundary. A function declared `fn process(data: Vec[Entity])` accepts any `Vec[Entity]` regardless of its physical layout. The layout is a codegen specialization contained to the compilation unit that applied the layout block; it is invisible to callers and does not affect the logical type. This is the formal underpinning of "changing layout is not an API break": the API surface (`Vec[Entity]`) never changes. The receiving function always accesses fields through the standard field-access interface; the compiler generates the appropriate pointer arithmetic for whichever layout the collection actually uses at the call site. There is no implicit O(n) copy at call boundaries — the callee operates on the collection's existing layout. Code that requires a specific physical layout (e.g., a GPU kernel that must have a SoA layout) must document that requirement separately; the type system does not enforce it. A type-system-level mechanism for expressing layout *capability* (so a function could opt in to "requires SoA" without making layout part of the type signature) is described under [Layout-Capability Bound](#layout-capability-bound-type-system-enforced-layout-requirements), and waits for GPU code to show what mechanism real users need.

#### Layout Blocks and `#[repr]`

`#[repr(C)]` and `#[repr(packed)]` are v1 ([design.md §15](design.md#repr-abi-layout)). These rules say how they combine with a layout block.

**Interaction with layout blocks:** `#[repr(C)]` disables SoA transformation. A layout block on a `#[repr(C)]` struct becomes documentation-only — group annotations are valid (for readability/IDE display) but no codegen transformation occurs. The ABI layout takes precedence. Severity depends on visibility:
- **`pub` struct** — combining `#[repr(C)]` with a layout block that would change field ordering or grouping is a **compile error**. A `pub` type may be consumed by FFI code that depends on the exact field order; silently ignoring the layout block risks a misleading contract. Either remove the layout block or remove `#[repr(C)]`.
- **Private struct** — the compiler emits a **warning** listing which groups are being ignored. Private structs cannot be observed via FFI, so the layout block is redundant but not harmful — hence a warning rather than an error. Suppress with `#[allow(repr_c_layout_ignored)]` when the layout block is intentionally kept as documentation.

**`#[repr(packed)]` and layout blocks:** `#[repr(packed)]` disables SoA transformation, same as `#[repr(C)]`. A layout block on a `#[repr(packed)]` struct is documentation-only — group annotations are valid for readability but no codegen transformation occurs. The packed layout takes precedence.

### String Small-String Optimization

In v1 a `String` is owned and heap-allocated. A later representation stores short strings inline:

For strings ≤ 23 bytes, stored inline in the struct with no heap allocation (SSO — Small String Optimization). This is an internal detail; the programmer never sees it.

#### Enums in Layout Blocks

Layout integration: `split_by_variant` in layout blocks stores each variant contiguously.

### Layout-Capability Bound (Type-System-Enforced Layout Requirements)

A type-system mechanism that lets a function require a specific physical layout (e.g., SoA) without making layout part of the type signature. Today, layout is a codegen specialization at the binding site — `Vec[Entity]` with SoA and `Vec[Entity]` without are the same type ([Layout Blocks](#layout-blocks)). This preserves "changing layout is not an API break," but leaves a gap: a GPU kernel that *requires* SoA can be passed AoS data, with the failure mode being a runtime perf cliff or wrong results. The current spec acknowledges this gap explicitly and routes layout requirements to documentation.

A future mechanism — a bound or attribute like `where Vec[T]: SoaLayout`, an `#[expects_layout(soa)]` attribute, or a structural trait derived from a binding's applied layout block — would let a function declare its layout requirement without forcing the requirement into every caller's type signature. The exact shape is open: too many plausible mechanisms, none chosen, all hypothetical until real GPU users hit the gap in practice.

**Why deferred:** GPU code generation belongs to the [GPU track](#gpu). Designing a mechanism without real GPU code risks the wrong shape — the right design depends on what kernels users actually write, what diagnostics they want at the `gpu.dispatch` boundary, and whether a `karac explain` suggestion ("this kernel expects SoA but received AoS — try `layout entities { group ... }`") is enough or whether type-level enforcement is needed. Revisit when the GPU track ships and a measurable corpus of `#[gpu]` code exists.

**Why non-breaking:** Any layout-capability bound added later is opt-in and additive. Existing function signatures (`fn process(data: Vec[Entity])`) continue to accept any layout. New signatures that opt in (`fn process(data: Vec[Entity] where Vec[Entity]: SoaLayout)`) gain compile-time enforcement; callers that previously passed AoS to such a kernel were already buggy at runtime — surfacing the bug at compile time is a behavior improvement, not a regression. No semantic change to existing code.

### Schedule-Language Layer

**Decision:** Defer Halide-style decoupled schedule languages. `layout` blocks ([Layout Blocks](#layout-blocks)) cover the data-layout half of "separate authoring surface for optimization"; the loop-schedule half is deferred. The queries channel ([design.md §17](design.md#compiler-query-api)) is the smaller v1 commitment for surfacing optimization decisions.

**Why deferred.** Halide / TVM / Tiramisu / Exo all separate the algorithm from the schedule, with the schedule being a complete, parallel authoring discipline targeted at perf engineers writing tight numeric loops. This is a substantial language commitment with its own grammar, its own type system for schedule values, and its own audience. The queries channel surfaces only the un-baked residual decisions and resolves them as item attributes — a much narrower surface that fits Kāra's existing attribute discipline. If real Kāra workloads accumulate that need full decoupled schedules, that's the signal to add the schedule layer alongside the queries channel.

**Promotion gate.** Promote when (a) the [data track](#m4c-data) (numerical and data-science library) has shipped, AND (b) a corpus of Kāra numerical code accumulates that hits the limits of `layout` blocks + queries-channel resolution annotations. Without (a), the audience for a schedule language doesn't exist yet; without (b), the queries channel + `layout` blocks may be sufficient, and shipping a schedule language pre-empts the simpler combination.

**Why non-breaking:** Additive — a new authoring surface that opts in. Existing programs without schedule annotations would compile unchanged.

**Cross-reference:** [Layout Blocks](#layout-blocks) (decoupled-layout authoring); [design.md §17](design.md#compiler-query-api) (decoupled-optimization-decision authoring).

## Self-hosting

The compiler and its runtime written in Kāra.

<a id="self-hosting-phase-12"></a>
### Self-Hosting

**Decision:** The Kāra compiler should eventually be written in Kāra.

**Why deferred:** Requires a mature, working compiler and a full standard library before the language is expressive enough to implement its own compiler. Logically follows all other tracks.

**Why non-breaking:** Implementation concern, not a language change.

**Self-hosting.** When the compiler is eventually rewritten in Kāra, the runtime is rewritten in Kāra alongside it — mirroring Rust's `std` being written in Rust and Go's runtime being written in Go. The architecture does not change; only the implementation language does.

## Tooling

Compiler commands, editor support, test-framework extensions, diagnostics, optimization infrastructure and interoperability tools beyond the v1 tooling contract ([design.md §17](design.md#17-tooling-contract)). Tooling changes no program's meaning, so items here can arrive in any order.

### Hot Reloading

**Decision:** Shared library reloading is the recommended approach. It requires a stable ABI and a state serialization design.

**Why deferred:** The ABI and state serialization design cannot be finalized until the runtime memory layout is stable.

**Why non-breaking:** Purely additive runtime feature.

### Workspace Scaffolding (`karac init --workspace`)

**Decision:** Defer workspace-aware scaffolding to the package manager work. In v1, `karac init` only scaffolds single-package projects. Users who want a workspace hand-write the root `kara.toml` with `[workspace] members = [...]` and then run `karac init <name>` inside each member subdirectory.

**Why deferred:** Workspaces are part of the v1 manifest ([design.md §4](design.md#packages-and-manifests)), but scaffolding them is a convenience that belongs with the package manager's workspace handling, not with `karac init`.

**Why non-breaking:** Purely additive. Adding `--workspace` to `karac init` at a later date is a new flag on an existing subcommand; v1-scaffolded projects remain valid workspace members once added.

**Design shape:**

```bash
karac init --workspace myrepo           # writes myrepo/kara.toml with [workspace] members = []
cd myrepo
karac init mylib --lib                  # auto-adds "mylib" to root's members array
karac init mycli                        # auto-adds "mycli" to root's members array
```

Key behaviors to settle with the package manager:
- Detecting that the CWD (or an ancestor) is a workspace root and auto-registering new members.
- Whether `--workspace` on an already-initialized project is an error or a promotion (converting a single-package `kara.toml` into a root).
- Interaction with `--force` when a root already exists.

### `karac test --coverage` (LLVM-instrumented coverage)

**Decision:** Defer LLVM-backed coverage support for `karac test`. In v1 the `--coverage` flag is not accepted and produces an unknown-flag error, and the JSONL schema reserves no `coverage` or `coverage_delta` events.

**Why deferred:** coverage needs LLVM instrumentation (`-fprofile-instr-generate`, `-fcoverage-mapping` equivalents) on the codegen path for tests:

1. Synthesize a per-package test-runner binary entry point so `karac test` has something to compile and link.
2. Provide codegen implementations of the test prelude builtins (`assert`, `assert_eq`, `assert_ne`).
3. Thread instrumentation flags through IR generation and the link.
4. Post-process `.profraw` → `.profdata` → `lcov` via `llvm-profdata` / `llvm-cov`.
5. Emit a `coverage` JSONL event summarizing aggregate line / branch / function coverage AND a `coverage_delta` JSONL event reporting changed-but-uncovered code against a git ref; write `dist/coverage/lcov.info` for tooling consumption (Codecov, Coveralls, GitHub Actions).
6. `--coverage --min=N` for CI gating — exits non-zero if aggregate coverage falls below the threshold.
7. `--coverage --since REV` for delta-oriented reporting — emits the `coverage_delta` event computed against the named git revision.

**Reporting surfaces — primary vs secondary.** The two surfaces serve different consumers:

- **Delta-oriented (primary for PR review and LLM-loop consumers).** The `coverage_delta` JSONL event reports what the active change set did and did not cover, against a git ref supplied via `--since REV` (composes with the test-runner `--since` selector — the same revision serves both flags). Two delta signals: (a) changed functions with no direct test (a test function whose body syntactically calls the function), (b) changed branches not covered by any executed test. The forward direction — "which tests reach this function transitively" — lives in [`karac query affected-by`](#karac-query-affected-by--call-graph-reach-query); coverage focuses on the reverse direction (uncovered code under change). This is the surface a PR reviewer or LLM TDD client should anchor on.

- **Aggregate (secondary, retained for CI gating).** The `coverage` JSONL event reports total-program line / branch / function coverage; `--coverage --min=N` gates CI on a project-wide threshold. Aggregate has its place — historical tracking, compliance reporting, threshold-based gates — but it is not the headline metric for change review. Global percentage alone hides the case where coverage stays high while a new untested branch lands.

**Stale-snapshot reporting** is *not* part of this entry — `karac test --clean-snapshots` ([Snapshot tests](#snapshot-tests)) reports orphaned snapshot files. A future composition could surface stale-snapshot count alongside coverage delta in the same `cycle_complete` summary (see the `karac tdd` Watch Driver entry), but the data source remains the existing `--clean-snapshots` walk, not the LLM coverage instrumentation.

**Why non-breaking:** purely additive. `--coverage`, `--since`, and `--min` are new flags on `karac test`; default behavior is unchanged. `dist/coverage/lcov.info` is a new artifact path under the existing `dist/` convention. Both `coverage` and `coverage_delta` JSONL events slot into the existing schema discriminator (existing consumers ignore unknown event kinds).

**Interpreter-path coverage:** explicit non-goal unless real demand surfaces; compiled-binary instrumentation is the single supported path.

**Sequencing:** (a) codegen `assert` builtins, (b) test-binary entry synthesis, (c) instrumentation flags through codegen, (d) `llvm-cov` post-processing + lcov + aggregate `coverage` event, (e) `--min=N` aggregate-threshold gating, (f) `--since REV` delta computation + `coverage_delta` event.

### Structured Diagnostics and Error Class Enum (`karac explain --format=json`)

**Decision:** Defer machine-parseable output for `karac explain` — `karac explain --format=json` with a finite error-class enum, typed `expected` / `got` fields, and ranked candidate patches. In v1, `karac explain` prints human-readable prose; the class enum is not frozen until the catalogue of diagnostics has matured. (Compiler diagnostics themselves are already structured JSON in v1: [design.md §17](design.md#structured-compiler-output).)

**Shape when delivered:** each JSON record carries `class` (enum from a published catalogue — `TYPE_MISMATCH`, `EFFECT_UNDECLARED`, `OWNERSHIP_MOVE_AFTER_USE`, target-incompatibility classes, etc.), `span` (byte offsets + file), typed `expected` / `got` where applicable (effect sets, type names, generic bounds), and `fixes: [{ description, edits: [{ span, replacement }] }]` for machine-applicable candidate patches. Enum values live in the reported-behavior tier (unstable across releases, stable within a release) per the specification layers ([design.md §2](design.md#2-specification-layers)) — the same policy already governs `karac explain` prose.

**Target-incompatibility errors** are one class in the enumeration, not a standalone diagnostic category — file-suffix conditional compilation mismatches, target-feature-gated intrinsics, and cross-target effect violations all land under a shared `TARGET_INCOMPATIBLE` family.

**Why deferred:** enumerating error classes before the diagnostic surface has stabilized locks in a shape that may not match how the diagnostics actually land. Diagnostics keep being written as features ship — waiting until the catalogue is ~20+ entries deep gives enough signal to finalize the enum and the patch-edit shape without retrofitting. The human-readable output format continues to be reported-behavior in the interim.

**Why non-breaking:** purely additive CLI flag. Default `karac explain` behavior unchanged; JSON output is opt-in.

### Signature-from-Call-Site Stub Diagnostic

When the resolver encounters an unresolved-identifier call inside a `_test.kara` file (the classic TDD opener — a test that calls a function that doesn't exist yet), enrich the existing `unresolved identifier` diagnostic with a `"suggested_stub"` machine-applicable diff that defines the function in the sibling production file with a best-effort inferred signature. The stub follows the `karac test --init` compiling-skeleton convention (see the `karac tdd` Watch Driver entry below): parameter types inferred from the call's argument expressions, return type inferred from any `assert_eq` / `==` comparison the call participates in, body is `todo()`. The diff slots into the existing `hints[].diff` shape ([design.md §17](design.md#structured-compiler-output)) — no new protocol.

**Why this matters for the LLM-driven TDD loop.** The classic red-green opener is a test that fails to compile because the function under test is unwritten. Today the LLM consumes one parse round-trip to learn the unresolved name, then writes the stub itself with whatever type guesses it makes from the call site. With this diagnostic the LLM begins each cycle from the first parse with the stub already proposed — fewer round-trips, fewer guesses, and the proposal is grounded in argument types the *compiler* sees rather than types the LLM infers from textual context.

**Diagnostic shape** (extending the existing `hints[].diff`):

```json
{
  "id": "d1",
  "severity": "error", "primary": true,
  "code": "E0100", "category": "resolve",
  "concept": "resolve/unresolved-identifier",
  "file": "src/math_test.kara", "line": 3, "column": 15,
  "message": "undefined name 'add'",
  "hints": [{
    "description": "stub `add` in src/math.kara with inferred signature",
    "diff": {
      "file": "src/math.kara",
      "line": <end-of-file>,
      "old": "",
      "new": "fn add(arg0: i32, arg1: i32) -> i32 {\n    todo()\n}\n"
    }
  }]
}
```

**Inference scope** — left to implementation time. Two layers are plausible:

1. **Resolver-time best-effort.** Cheap, local-only inference: literal arguments (`add(2, 3)` → `i32, i32`), explicit-typed bindings (`let x: u64 = ...; add(x)` → `u64`), and obvious comparison context (`assert_eq(call(...), 5)` → return type `i32`). Falls back to `_` placeholders for argument expressions whose types depend on typechecking. Ships first.
2. **Post-typecheck refinement.** Typechecker continues past unresolved-call errors (synthesizing a placeholder signature for the missing function) and infers argument and return types where context permits. Higher-quality stubs but a bigger pipeline change. Optional second milestone.

The implementation chooses the layer based on what real LLM-loop usage shows: if the resolver-time best-effort produces enough quality to drive most cycles, the post-typecheck layer can be deferred or skipped. The diagnostic shape is identical at both layers — the difference is how many `_` placeholders the LLM has to fill in.

**Body convention.** The stub body is `todo()` per the same compiling-skeleton policy as `karac test --init` (see *`karac tdd` Watch Driver* above, "Test scaffolding" subsection — the parameter-type-default table). For `ref T` / `mut ref T` parameters, no synthetic `_owned` binding is generated at the function signature site — that's a test-body concern, not a function-signature concern.

**Activation gate.** This diagnostic enrichment fires *only* when the unresolved-call site is inside a `_test.kara` file. Production files emit the plain `unresolved identifier` diagnostic without the stub hint — for production code, the failure usually means a typo or a missing import, not a function the user is about to write. Limiting to test files matches the classic TDD red-green workflow without polluting non-TDD diagnostics.

**Why non-breaking:** the `hints` field already exists; adding entries to it is additive. Existing JSONL consumers see new hint records under the existing schema. The plain-text human-readable diagnostic format may surface the suggestion as an additional hint line; humans who don't want the suggestion can ignore it. No new flag, no new event type.

**Distinct from `karac test --init` scaffolding.** The `karac test --init` subsection in the `karac tdd` Watch Driver entry below scaffolds *tests* for existing functions; this entry scaffolds *functions* for tests that don't resolve. Both feed the LLM TDD loop in different directions: `karac test --init` is "I have a function, give me a test"; this is "I have a test, give me the function." Both reuse the compiling-skeleton convention; both emit machine-applicable diffs.

### `karac tdd` Watch Driver — Unified TDD Cycle Loop

A `karac tdd` subcommand that orchestrates the existing build, diagnostic, and test surfaces into a tight red-green-refactor loop suitable for LLM-driven test-first development. Watches the project filesystem; on change, re-runs the affected pipeline and emits a unified JSONL event stream covering build phases, diagnostics, test execution, and a per-cycle summary.

Sketch of the cycle envelope (final shape TBD; specifics emerge from prerequisite items):

```bash
karac tdd --watch --output=jsonl
karac tdd src/foo.kara::function_name --output=jsonl
```

```json
{"type":"cycle_start","changed":["src/foo.kara","src/foo_test.kara"]}
{"type":"phase_start","phase":"parse"}
{"type":"diagnostic","phase":"parse","id":"d1","primary":true}
{"type":"test_fail","test":"foo::test_empty_input","left":"...","right":"..."}
{"type":"cycle_complete","status":"red","next_best_action":"fix_primary_diagnostic"}
```

**Prerequisite work** (each is its own committed item):

- Stable `karac test` JSONL contract ([design.md §14](design.md#runner-output))
- `karac build --output=jsonl` streaming mode ([design.md §17](design.md#streaming-phase-events))
- Unified envelope shape across build and test JSONL streams
- Targeted test selection (`--failed`, `--related FILE`, `--since REV`)
- Cycle-summary status taxonomy distinguishing compile-fail / no-tests-discovered / tests-failed / tests-passed / tests-skipped-resource-unavailable
- `karac test --init <module::function>` scaffolding for stub creation
- `karac explain --format=json` for structured diagnostic patches (see *Structured Diagnostics and Error Class Enum* above)

**Why deferred:** This is integrating tooling, not a language feature. It composes pieces specified elsewhere in the design plus the prerequisites above. The watch loop itself is a thin shim over file watching (`notify`-style crates handle portability) and successive `karac build` / `karac test` invocations. The non-trivial parts — the cycle envelope, `next_best_action` triage policy, affected-tests selection — depend on the prerequisites above being battle-tested. Specifying the cycle event schema before its prerequisites land would over-spec; specifying it after gives schema choices grounded in real client integrations.

**Why non-breaking:** New subcommand. Existing `karac build` / `karac test` invocations are unchanged. The watch driver wraps them as a sub-process or in-process call; both are unaffected by the wrapper's existence. Cycle events are additive on top of the existing per-tool envelopes.

**Capstone framing.** Together with its prerequisites, this entry builds out the LLM-driven TDD surface end to end. Each prerequisite stands on its own merit and is decided independently; this entry gives them an integrating destination.

**Cycle-summary status taxonomy.** TDD starts in red, so the `cycle_complete` event needs a status field richer than green/red. The five distinct end-states an LLM client must act on differently:

| Status                        | When                                                                                                | LLM action                                                                              |
|-------------------------------|-----------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------|
| `compile_error`               | Build phase emitted at least one error diagnostic; test phase did not run                           | Surface the primary diagnostic; route to fixing it before any test inference            |
| `no_tests_discovered`         | Build succeeded but no `_test.kara` files or no `test` blocks matched the active scope                | Distinct from `tests_passed` because nothing was verified — write a test, do not assume green |
| `tests_failed`                | Build succeeded; at least one `test_fail` event                                                     | Standard red state; primary loop driver — fix the failing assertion                      |
| `tests_passed`                | Build succeeded; at least one test ran; every event was `test_pass` (or permitted `test_skip` not under `--all`) | Green; refactor or extend coverage                                                       |
| `tests_skipped_unavailable`   | Build succeeded; at least one `test_skip` with reason `unsatisfied_requires` (and not running under `--all`) | Provider not configured or external service unavailable — surface which resource, not silently treat as green |

Precedence when multiple conditions apply within a single cycle: `compile_error` > `tests_failed` > `tests_skipped_unavailable` > `tests_passed` > `no_tests_discovered`. Under `--all` mode, any skip becomes a fail and the status collapses to `tests_failed`. Under permitted-skip mode, a mix of `test_pass` and `test_skip` resolves to `tests_skipped_unavailable` — the skipped resource is information the loop surfaces rather than silently loses, and an LLM that wants green must address the missing provider.

The taxonomy locks the *set* of statuses so future test-runner reasons can extend `test_skip` without expanding the cycle-status vocabulary. Adding new statuses is a major-version decision; adding new `test_skip` reasons (per design.md test-runner forward-compat rules) routes through `tests_skipped_unavailable` until evidence justifies a new top-level cycle status.

**Test-selection flags.** Substring filtering (`karac test <substring>`, [design.md §14](design.md#14-testing)) is useful but crude — LLM loops and watch clients need precise selection. The flag set:

| Flag                       | Semantics                                                                                          | Dependency                                                                                       |
|----------------------------|----------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------|
| `--failed`                 | Re-run only test IDs that emitted `test_fail` in the previous run.                                 | None — pure runner book-keeping. Persists last-run state in a cache file (`.kara/test-state.json` or similar). Ships standalone with the watch driver; no compiler analysis required. |
| `--related <FILE>`         | Run tests whose transitive call graph reaches code in `<FILE>`.                                    | Thin wrapper over [`karac query affected-by`](#karac-query-affected-by--call-graph-reach-query). |
| `--since <REV>`            | Run tests affected by changes since git ref `<REV>` (e.g., `--since HEAD`, `--since main`).        | Composes [`karac query affected-by`](#karac-query-affected-by--call-graph-reach-query) over the files surfaced by `git diff <REV>...HEAD`. |
| `--module <path>`          | Run tests in the named module path (e.g., `--module db.connection`).                               | None — discovery already groups tests by module path; flag is a literal-prefix filter on the fully-qualified ID. |
| `--exact <full::test::id>` | Run exactly the named test (e.g., `--exact db.connection::test_reconnect`).                        | None — equality filter on the fully-qualified ID. Distinct from substring (which can ambiguously match multiple). |

The existing substring filter remains — it is the casual default. The flags above are additive and orthogonal: `karac test --failed --module db` runs the previous-failure set intersected with the `db` module. Combinations resolve as set intersections. `--all` overrides selection (runs everything regardless of selectors), preserving its existing "fail-on-skip" semantics.

`--related` and `--since` block on the affected-by query landing — without it, both flags would need ad-hoc heuristics that miss real reach edges (closure captures, trait-object dispatch, generic monomorphizations). The watch driver lands these flags only after the query is in place.

The `.kara/test-state.json` cache for `--failed` is per-project, gitignored, regenerated each run; corruption resets to "no previous state" (treats `--failed` as `--all` with a stderr note). The cache schema is internal — not part of the JSONL contract — so it can evolve freely.

**Test scaffolding (`karac test --init`).** Generates a compiling test skeleton for a named function, removing the boilerplate "open the test file, write a `test "..." { }` block, plumb default arguments, wrap the call in `with_provider` for any effects" sequence from the LLM loop.

```bash
karac test --init src/db/user.kara::create_user
karac test --init db.user::create_user        # module-path form
```

Both forms are accepted; module path is resolved to its source file via the standard module walk, and the file path resolves to its module path the same way `karac build` does. The function must be defined in the current project — scaffolding tests for dependency code is rejected.

Target file resolution:
- Sibling test file path is the source path with `.kara` replaced by `_test.kara` (e.g., `src/db/user.kara` → `src/db/user_test.kara`).
- If the file exists, append the new test block at the end (after the last item).
- If it does not exist, create it with no extra preamble — `_test.kara` files inherit private sibling access and auto-injected `assert` / `assert_eq` ([design.md §14](design.md#14-testing)).

Test case name:
- Default: the function's name (e.g., `test "create_user" { }`).
- On collision (a `test "create_user"` block already exists in the file): append a numeric suffix incrementing from `_2` until unique, and emit a stderr note naming the chosen name.

Generated body — the compiling-skeleton policy:

| Parameter type                       | Default value generated                                       |
|--------------------------------------|---------------------------------------------------------------|
| Numeric primitives (`i32`, `u64`, `f32`, etc.) | `0`, `0u64`, `0.0` matching the target type            |
| `bool`                               | `false`                                                       |
| `String`                             | `String.from("")`                                             |
| `Option[T]`                          | `None`                                                        |
| `Result[T, E]`                       | `Ok(<default for T>)` — `Err` would force the caller to construct an `E` value |
| `Vec[T]` / `Map[K, V]` / `Set[T]`    | `Vec.new()` / `Map.new()` / `Set.new()`                       |
| `ref T` / `mut ref T`                | Synthesize `let <param>_owned: T = <default>;` above the call, pass `ref <param>_owned` (or `mut ref ...`) |
| Refinement types (`Positive[i32]`, etc.) | `todo()` — defaults may not satisfy the refinement predicate |
| User-defined struct / enum           | `todo()` — the compiler cannot know which constructor to pick |
| Generic type parameter (e.g., `T`)   | `todo()` — no concrete type chosen at scaffold time           |

The skeleton compiles whenever every parameter is in the "concrete default" rows of the table; otherwise it compiles after the user replaces `todo()` calls with values. The scaffold's *intent* is "ready to run with a green build the moment defaults work, with `todo()` markers showing exactly what to fill." For functions whose return type is `Result[T, E]` or whose body has fallible refinement returns, the assertion line is `assert(/* TODO */ true);` — a literal placeholder that compiles but is meaningless until the user writes a real assertion.

Effect-aware scaffolding: if the function under test declares effects (e.g., `with reads(Db)`), the generated test wraps the call in `with_provider[Db](/* TODO */ todo(), || { ... })` ([Providers](#provider-rooted-resources-trait-based-injection)). The provider value is `todo()` — the user supplies a fake. This makes the effect surface visible at the scaffolding site without forcing the scaffolder to know which fakes are available.

Errors:
- Function not found: `E0xxx` "function `<name>` not found in module `<path>`".
- Function is private to a sibling that is not the source file's sibling test: rejected (the test file would need access the language doesn't grant).
- Function is in a dependency: rejected as above.
- Source file is itself a `_test.kara` file: rejected ("cannot scaffold tests for test code").

The exit code is `0` on success (file written or appended); non-zero on any of the above errors. On stdout, the command emits one JSONL `init` event with the chosen test name, target file path, and any `todo()` markers placed, so an LLM client can read what to fill next without re-parsing the file.

### Signature Catalog (`karac catalog`)

**Decision:** Defer a tooling subcommand — `karac catalog` — that indexes the public API surface (fully qualified name, kind, generic parameters with bounds, parameter modes and types, return type, declared effect row, refinement constraints, source span) and emits JSONL for downstream consumers (LLM agents, IDE plugins, documentation generators).

**Shape when delivered:** public surface only — private functions have inferred, reported-tier effect rows that are not stable enough to index. One entry per exported item (`fn`, `struct`, `trait`, `impl`, `const`, type alias). Queryable by any field component: "find all public fns that take a `Path` and produce `writes(Fs)`," "find all traits with a `Display` bound in their supertrait set," etc.

**Why deferred:** pure tooling, blocks on no language decisions. Natural fit once the language surface is stable and real consumers (LLM agents, IDE tooling) materialize. Overlaps with **Structured Diagnostics and Error Class Enum** above — both are JSONL-emitting tooling that benefits from a shared schema vocabulary; build them in concert when their respective consumers are real.

**Why non-breaking:** new `karac` subcommand; no language-surface impact.

### `karac query affected-by` — Call-Graph Reach Query

Extension to the query API ([design.md §17](design.md#compiler-query-api)) that exposes the compiler's call graph as a queryable surface, alongside `karac query effects` / `ownership` / `monomorphization`. Inputs: a file path with optional line range, or a fully-qualified function path. Outputs: the transitive callers and callees that the call graph already computes for effect inference, plus the test functions that reach the input through that graph.

**Invocation:**

```bash
karac query affected-by src/sort.kara                    # all functions affected by changes to file
karac query affected-by src/sort.kara:42-58              # affected by changes to specific line range
karac query affected-by math::sort                       # affected by changes to a specific function
karac query affected-by math::sort --tests-only          # only test functions reaching this
karac query affected-by math::sort --direction=callees   # transitive callees only (not callers)
```

**Output format (JSONL):**

```json
{"type":"affected_by","input":"math::sort","callers":[{"fn":"app::main","file":"src/main.kara","line":12}],"callees":[{"fn":"std::cmp::min","file":"std/cmp.kara","line":34}],"tests":[{"fn":"math_test::test_sort_preserves_length","file":"src/math_test.kara","line":3}]}
```

Schema:
- `input`: the function or file the query was issued against (echoed for client correlation).
- `callers`: array of `{fn, file, line}` for every function that transitively calls into the input. Direct callers first, then their callers, etc. — partial topological order.
- `callees`: array of `{fn, file, line}` for every function the input transitively calls.
- `tests`: array of test cases (the `test "..." { }` blocks in `_test.kara` files) that reach the input through the call graph. Subset of `callers` filtered to test cases, surfaced separately because the test-selection consumers (`--related`, `--since`) want this view directly.

**Call-graph construction subtleties** (well-understood engineering, not research):

- **Trait-object dispatch (`t.method()` on `dyn Trait`).** The graph includes every impl of `Trait` known at query time as a possible callee. Conservative — false positives (impls the runtime never reaches) are acceptable for affected-by; false negatives (real reaches missed) would break the test-selection use case.
- **Generic monomorphization.** A generic function `fn f[T](x: T)` instantiated with multiple concrete `T` values may have different call graphs per instantiation. The query summarizes across all instantiations the compiler sees in the project — the union of every monomorph's reach. A future flag could parameterize by `T` if a concrete use case emerges.
- **Closure captures and escape.** When a closure escapes its creation site (stored, returned, passed to a function that calls it later), the call site of its body is the *escape consumer*, not the closure-creation site. The graph traces escape paths so callers of the consumer are correctly attributed as transitive callers of the closure body.
- **FFI / `extern` boundaries.** The graph does not cross `extern` boundaries — `extern fn`s are leaf nodes. Their declared effects propagate, but their internal call graph is opaque (no body to analyze).
- **Recursion / SCCs.** Strongly-connected components in the call graph are treated as a single unit for the affected-by closure — every function in an SCC affects every other function in the SCC.

**Why it comes first:** structural prerequisite for three other entries:

- [`karac tdd` Watch Driver](#karac-tdd-watch-driver--unified-tdd-cycle-loop) — uses affected-by to scope cycles to changed code.
- Test-selection flags `--related <FILE>` and `--since <REV>` (in the `karac tdd` entry's flag taxonomy) — both block on this query landing; without it they need ad-hoc heuristics that miss real reach edges.
- `coverage_delta` event in [`karac test --coverage`](#karac-test---coverage-llvm-instrumented-coverage) — uses affected-by to compute the "tests covering changed function" delta signal.

Without this query, those three features either ship with reduced functionality or wait. Shipping them as designed requires the affected-by data.

**Why non-breaking:** new query subcommand under the existing `karac query` umbrella. No existing query behavior changes. JSONL output uses the standard `"type"` discriminator; existing JSONL consumers ignore unknown event types.

**Implementation cost.** Moderate. The data exists already — effect inference computes the call graph, including the trait-dispatch / generics / closure handling above. The work is plumbing it into a query interface, defining the JSONL output, and exposing the existing graph traversals as a public surface. No research questions; well-understood engineering.

### Doctests as `#[example]` Blocks on `pub` Items

Compiler-extracted runnable examples on `pub`-item documentation. Following the well-trodden pattern from Rust (`cargo test --doc`), Python (`doctest`), Haskell (`doctest`), and OCaml (`mdx`) — examples in or attached to docstrings that the compiler extracts and runs as tests under `karac test`, with assertion failures reported through the existing test-runner JSONL envelope.

**Kāra-specific value-add.** Beyond the standard documentation-drift defense (an example that fails to compile or run blocks CI; the API can't evolve away from its examples without breaking the build), Kāra's effect / contract / refinement system means examples cover the drift gap from two directions: an LLM-written example that violates the function's `ensures` clause becomes a compile error rather than runtime evidence; a succeeding example gives the contract executable verification. The shape that LLMs reach for first when documenting an API becomes a first-class verification artifact.

**Mechanics (committed):**

1. **Discovery rule.** `karac test` extends its current `_test.kara`-only walk to also visit regular source files looking for example items / blocks attached to `pub` items. Examples on non-`pub` items are a parse-level diagnostic — examples are public-API artifacts. Examples on items in `_test.kara` files are also rejected (test files have their own testing surface; examples belong to the documented API).
2. **Test-prelude injection.** Examples receive the same prelude as `_test.kara` files (`assert`, `assert_eq`, `Arbitrary` if applicable). Imports inside the example body resolve through the example's enclosing module — examples have access to the public surface of the module they live in plus any `import`s the file already brings in.
3. **Doc rendering interaction.** `karac doc` renders example bodies as code blocks alongside the docstring prose — same source, two views. The renderer reuses `pulldown-cmark` for markdown-flavored examples, or emits attribute-shaped examples as `<pre><code class="language-kara">` blocks under a "Examples" heading.
4. **Effect inference.** Examples are normal compiled functions; effects propagate through standard inference. An example calling `pub fn read_file()` with `reads(FileSystem)` inherits `reads(FileSystem)` on its synthetic test-fn signature. If the project profile permits `reads(FileSystem)` for tests, the example runs; otherwise the example is rejected at compile time, surfacing the same effect-mismatch diagnostic that any other effect-violating function would.
5. **Compilation cost / when to run.** `karac build` does NOT compile or run examples (preserves the fast-build property — examples don't gate plain compilation). `karac test` discovers and runs examples alongside `_test.kara` tests. A `karac test --no-examples` flag lets developers iterate on test-file changes without re-running every example.
6. **Scoping for effectful examples.** Pure examples (`assert_eq(abs(-5), 5)`) come first. Effectful examples that require providers (`with_provider` setup in a `_test.kara` file) need a syntax for declaring providers within the example block — defer this to a follow-up once the pure-example surface lands and real demand for effectful examples surfaces.
7. **Failure reporting.** A failing example emits a `test_fail` JSONL event with `test: <module_path>::<item_name>::example` (or `::example_<n>` if multiple examples are attached to one item). The failure event includes the example body in the diagnostic so the reader sees exactly what was being asserted, even without source access.
8. **Discovery-error handling.** An example that doesn't compile is a hard error under `karac test`, the same way a `_test.kara` file that doesn't compile is. No silent skip — broken examples are broken docs.

**Syntax candidates (pick deferred to implementation prototyping):**

The three plausible shapes each have honest tradeoffs. Implementation prototypes each on a representative slice of the stdlib (~10 `pub` items with varied effect surfaces) and picks the winner against artifact, not speculation.

**(i) `#[example] fn _ex() { ... }` — explicit function with attribute.**

```kara
/// Computes the absolute value.
pub fn abs(x: i32) -> i32 { if x < 0 { -x } else { x } }

#[example]
fn abs_handles_negatives() {
    assert_eq(abs(-5), 5);
    assert_eq(abs(0), 0);
}
```

*Pros:* fits Kāra's existing attribute culture (`#[property]`, `#[snapshot]`, `#[derive(...)]`); reuses existing AST infrastructure (the example IS a function); explicit naming gives precise test IDs; effect declaration via the standard `with` clause works transparently.

*Cons:* visually separated from the docstring it documents; readers must scan past the `pub fn` to find the example; verbose for one-line assertions.

**(ii) Rust-style fenced code blocks in `///`.**

```kara
/// Computes the absolute value.
///
/// ```
/// assert_eq(abs(-5), 5);
/// assert_eq(abs(0), 0);
/// ```
pub fn abs(x: i32) -> i32 { if x < 0 { -x } else { x } }
```

*Pros:* visually collocated with the docs they verify (the prose-with-example flow that's the whole point of doctests); most natural form for LLM-generated docs (markdown is the lingua franca); `karac doc` rendering is trivially natural (the code block is already markdown); concise.

*Cons:* requires parser support for extracting fenced code blocks from doc comments as test bodies; effect declaration is awkward (where does `with reads(FileSystem)` go on a fenced block?); test ID is positional within the docstring rather than named.

**(iii) `#[example(of = path)]` as a separate top-level item.**

```kara
/// Computes the absolute value.
pub fn abs(x: i32) -> i32 { if x < 0 { -x } else { x } }

#[example(of = abs)]
fn abs_handles_negatives() {
    assert_eq(abs(-5), 5);
    assert_eq(abs(0), 0);
}
```

*Pros:* most flexible (multiple examples per item, examples in different files, examples organized by topic rather than co-located with the item); explicit cross-reference makes the relationship machine-readable.

*Cons:* most verbose; loses the prose-with-example flow entirely; requires a path-resolution pass to validate `of = abs` references a real `pub` item; falls back to (i) if the cross-reference is degenerate (one-to-one with the documented item).

**Implementation guidance.** Prototype (i) and (ii) on a representative slice of the stdlib. Measure: example density per item, ergonomics for one-liners vs. multi-statement examples, integration with effect declaration when the example calls effectful code, doc-rendering quality, parser complexity. Pick the winner. (iii) is a fallback considered only if the primary candidates have a structural problem we don't currently see.

**Why it is cheap:** well-trodden pattern (no research uncertainty), existing infrastructure leverage is clean (test prelude, doc rendering, effect inference all already exist), real LLM-loop value (LLM-written examples auto-verified, examples-as-contracts story is genuinely distinctive to Kāra), independent of the `karac tdd` capstone and its sub-features so the work can land on its own timeline.

**Why non-breaking:** new attribute syntax (or doc-comment convention, depending on which candidate wins); existing `pub` items without examples are unaffected. `karac build` behavior is unchanged (build doesn't run examples); `karac test` gains additional discovery scope but pre-existing tests still run identically. New JSONL `test_pass` / `test_fail` events for examples slot into the existing schema discriminator.

### Structured Runtime Traces Keyed to Source Spans

**Decision:** Defer tooling for structured runtime trace output — events annotated with the source span of the emitting site, suitable for debugging effect conflicts, ownership timing, scheduler placement, and other properties that manifest only when the program runs. Complementary to compile-time `karac explain`.

**Why deferred:** depends on a mature runtime. The source-span side is cheap — every AST node already carries a `Span`. The runtime side requires stable instrumentation hooks that cannot be pinned until the codegen path and scheduler are real enough to instrument. Deciding the output format now risks a mismatch with the emission points once they exist.

**Ecosystem compatibility — open.** Candidate formats include an OpenTelemetry-compatible emitter, a `tokio-trace`-style layered subscriber, or a Kāra-specific JSONL format shared with `karac test`. Pick when the instrumentation hooks land; the trade-off is familiarity vs. schema control.

**Why non-breaking:** opt-in runtime feature; off by default.

<a id="language-server-kara-lsp--v1-editor-surface"></a>
### Language Server (`kara-lsp`)

**Decision:** Ship a `kara-lsp` binary and a VS Code extension close to the v1 release, followed by Neovim and JetBrains integrations.

**Why ship early.** Editor friction is a momentum-killer. A general-purpose language v1 launched without working VS Code / Neovim / JetBrains support out of the box does not get past the "I tried it but my editor was useless" early-adopter filter. Every successful general-purpose language post-2015 (Rust, Go, Swift, Kotlin, Zig late, Gleam) shipped editor integration at or before v1. The cohort that tries Kāra in week 1 leaves and does not come back if VS Code support is missing.

**Why non-breaking:** New binary + extension; no compiler API changes beyond exposing the existing query surface over LSP protocol.

**Engineering surface — the analysis is reused.** `karac query` and structured-diagnostic JSON are part of v1 ([design.md §17](design.md#17-tooling-contract)). The LSP binary is a long-lived process wrapping the existing analysis surface and translating to LSP wire protocol. Work is plumbing + IDE-side glue, not new compiler design.

**Floor (must ship):**
- Syntax highlighting (TextMate grammar — book infrastructure mostly exists).
- Diagnostics streaming (`textDocument/publishDiagnostics` over existing `karac` structured-diagnostic JSON).
- Go-to-definition (resolver symbol table).
- Hover (type + effect signature; typechecker + effectchecker already produce this).
- Find references (resolver symbol table).
- Document symbols / outline (parser AST).
- **Type-aware completion** (`.`-completion of methods/fields on the receiver type — requires partial-parse + typecheck-of-incomplete-source; ~4-6 weeks engineering; the line below which the LSP feels half-broken).
- Formatting via LSP (wraps `karac fmt`).
- Signature help (parameter-info popup).

**Stretch (ship if engineering time allows):**
- Rename symbol; code actions (apply structured fix-diffs from `karac` diagnostics); semantic tokens (beyond TextMate); workspace symbols / global search.

**After launch:**
- **Effect-aware completion** — `.`-completions filtered by effect compatibility with the surrounding `with`-clause. Kāra-specific differentiator, ~2-3 weeks on top of type-aware. Ship post-launch as a "Kāra LSP now does X" announcement.
- Inline-explain / type lens (surface `karac explain` reasoning in-editor).
- Refactoring (extract function, inline variable).

**Future direction:** the reactive query-based LSP (Salsa-style subscribe/notify model, sub-100ms live-edit re-computation) comes after self-hosting. The first LSP runs a batch query model over the existing `karac query` surface — sufficient for AI clients and editor integration at launch; the reactive layer becomes necessary at scale.

### Bidirectional Compiler Hints

Compiler suggests code changes to the AI; AI suggests optimization strategies to the compiler. Waiting for real AI-assisted development usage to reveal whether this is valuable.

### Lint on Explicit `ref T` for Copy Primitives

Whether the compiler should emit a non-fatal diagnostic when a programmer writes `ref T` in a parameter list for a small Copy primitive (`i*`, `u*`, `f*`, `bool`, `char`) where bare `T` (owned) would carry the same information in less machine code. The pessimization: `ref i64` is an 8-byte pointer with one indirection; owned `i64` is the 8-byte value itself — same argument size, one fewer load. All modes are declared explicitly, so the question is whether the compiler flags a declared `ref` on Copy primitives as likely-unintentional.

**Current lean:** (a) silent at source level — no lint. Rely on (i) `karac explain` to surface inferred-vs-written modes on demand, and (ii) standard backend optimizer passes (argument promotion, inlining + SROA) to narrow the observable perf gap between `ref` and `own` on small Copy types at the machine-code level.

**Guiding principle:** parameter modes are part of a function's semantic signature, not optimizer hints. They govern what the callee can observe and do, participate in trait coherence, and are visible to external callers. The compiler must not silently rewrite them. Performance recovery belongs in the backend, where `ref i64` can be lowered to a register-held value without changing the source-level contract. Frontend lint/rewrite conflates two concerns that Kāra keeps separate.

**Why not a lint (c):** competes with `karac explain` for the same user-facing teaching role. Every viable threshold rule has problems — R4 (primitives only) fires where the pessimization is most obvious and misses tuples-of-primitives where it's most confusing; R1/R2 couple the lint to ABI heuristics. A lint framework plus attribute/suppression syntax are larger spec commitments than this single lint justifies.

**Why not auto-rewrite:** breaks trait conformance (impl signatures must match their trait), violates the "declared modes are the public contract" principle on which signature stability depends, discards non-perf reasons to write `ref` (documentation signal, signature uniformity across Copy/non-Copy instantiations, future-proofing), and creates source-to-codegen mismatches that confuse performance profiling.

**Why non-breaking:** adding a lint later is additive. Removing would be too. Either direction is safe from a compatibility standpoint.

**Re-evaluation triggers (all required):**

1. A corpus scan of real Kāra code shows a non-trivial number of explicit `ref <primitive>` parameters (heuristic: ≥5 instances across examples, tests and the ecosystem).
2. `karac explain` has shipped and empirically failed to close the teaching gap for the patterns found above.
3. A general lint framework exists for reasons independent of this specific lint (i.e., there are ≥2 other lints pending that would justify the framework cost).

If any trigger is absent, skip — the lint is dead weight against `explain` + backend passes.

**Why low priority:** the lint addresses a narrow pattern that the language design already discourages at the teaching level (the idiomatic spelling for Copy primitives is bare `T`, not `ref T`). Its teaching value is duplicated by `karac explain`. Its perf value is recoverable in the backend. The cost of the infrastructure it would require (lint framework, attribute syntax) is disproportionate to a single warning.

### Machine-Applicable Replacement Metadata on Typechecker / Effectchecker Diagnostics

Whether typechecker `TypeError` and effectchecker `EffectError` should carry `suggestion` / `replacement` fields so their `did you mean`-style diagnostics flow through the same `karac fix` and IDE quick-fix infrastructure that resolver and ownership classes already use. Today neither phase has a `suggestion` field on its error struct; adding one is a per-struct expansion.

**Current lean:** not in v1. The infrastructure is in place for resolver and ownership diagnostics, with `karac fix`, single-file JSON, and multi-file JSON / JSONL paths all wired through. Extending coverage to typechecker / effectchecker phases is per-class metadata work that lands opportunistically alongside the per-pass refactors that benefit. Most existing diagnostic surfaces in those phases carry sentence-prose suggestions, not single-token edits.

**What's needed:**

1. **Diagnostic-struct expansion** — add `pub suggestion: Option<String>` and `pub replacement: Option<Box<crate::resolver::TextEdit>>` to `TypeError` (`src/typechecker.rs`) and `EffectError` (`src/effectchecker.rs`). Propagate `None` defaults through every existing construction site (mechanical, multi-site).
2. **CLI rendering** — extend the typechecker / effectchecker JSON-rendering paths in `src/cli.rs` to emit the `replacement` payload (mirror the ownership pattern at `cli.rs:1411`).
3. **Per-class tagging** — pick high-value sites with mechanical fixes:
   - `TypeErrorKind::UndefinedField` — when the field is misspelled, `suggest_similar` against the struct's known fields produces a single-token replacement.
   - `TypeErrorKind::UndefinedVariant` — same shape against enum variants.
   - `EffectErrorKind::UnknownEffectVerb` — fuzz-match against the eight built-in verbs.
4. **`karac fix` dispatcher** — already runs the full pipeline, so newly-tagged classes are picked up automatically.

**Why non-breaking:** purely additive. New fields default to `None` for untagged classes; new metadata flows through the same JSON envelope pattern; no existing diagnostic class changes shape.

**Why not sooner:** the resolver + ownership coverage in v1 already covers the common-case quick-fixes a v1 user hits (typo'd identifier, typo'd type, typo'd module / import, unused-mut perf note). Typechecker / effectchecker tagging adds polish for less-common cases that an IDE could surface but a CLI user is rarely blocked on.

**Re-evaluation triggers (any one of):**

1. An IDE / LSP integration ships and the absence of typechecker / effectchecker quick-fixes becomes a user-visible gap.
2. A standalone typechecker or effectchecker refactor lands that naturally adds `suggestion` infrastructure as a side-effect.
3. A corpus scan of real Kāra programs shows a non-trivial fraction of typechecker / effectchecker diagnostics where a mechanical fix exists — i.e., the polish would matter at scale.

### Package Manifest Artifact Format (`.karapack`)

A structured, tool-consumable descriptor for a `karac build` output that would complement or replace the per-file-naming convention of [design.md §16](design.md#build-artifacts). Fields would include: module set, public export list, embedded WIT (for WASM Component Model), declared effect requirements per export, and toolchain version.

**Current lean:** not in v1. `karac build` emits the flat per-file layout (`dist/<target>/<pkg>.{wasm,js,d.ts}` etc.) for every target. Downstream tooling consumes files by name and convention.

**Why deferred (not rejected):**

1. **No ecosystem pressure.** No bundler, loader, or deployment pipeline currently asks for a Kāra-specific manifest. Committing to a shape before the tools exist forces premature decisions.
2. **Per-file convention is sufficient for the known use cases.** Browser bundlers consume `.wasm` + sibling `.js` + `.d.ts` by file naming. Component Model hosts consume embedded-WIT `.wasm`. Neither needs a separate descriptor for v1 deployments.
3. **Embedded WIT already covers a large fraction of what a manifest would carry.** For Component Model targets, the WIT interface describes exports, effect-like capabilities (via interface types), and versioning. A manifest would layer additional Kāra-specific fields, but the value over plain embedded WIT is speculative.

**Why non-breaking later:** purely additive. A `--manifest` flag on `karac build` emits the `.karapack` file alongside existing outputs; the per-file layout continues to work. Tools that want the manifest opt in; tools that don't are unaffected.

**Re-evaluation triggers (any one of):**

1. A downstream tool (deploy platform, registry, bundler plugin) emerges with a concrete request for structured build metadata that cannot be derived from the per-file artifacts + embedded WIT.
2. Multi-module Kāra packages become common enough that a descriptor listing "which modules are in this build" is useful.
3. Effect declarations per export become a value-add for downstream security / auditing tools — a `.karapack` could carry the full effect signature of every public export in a form those tools can read without loading the `.wasm`.

**Cross-reference:** [design.md §16](design.md#build-artifacts) — the per-file contract.

### Test Framework Extensions

v1's test runner runs `test "name" { }` blocks from `_test.kara` files ([design.md §14](design.md#14-testing)). These extensions add property tests, snapshot tests, fuzz tests and benchmarks, with two more file conventions:

| File convention | Command | Purpose |
|---|---|---|
| `_test.kara` | `karac test` | Unit, property, and snapshot tests — fast, runs in CI |
| `_fuzz.kara` | `karac fuzz` | Coverage-guided fuzz tests — slow, run separately |
| `_bench.kara` | `karac bench` | Benchmarks — performance measurement, not correctness |

`karac build` ignores all of them.

#### Property tests

**Test-only derivable traits** (available only in `_test.kara` files):

| Trait | What it generates |
|---|---|
| `Arbitrary` | Random instance generation for property tests; each field generated independently. Types with invariants implement `Arbitrary` manually. |

`Arbitrary` is part of the built-in test framework (`karac test`), not the core standard library. It is automatically available in `_test.kara` files without an explicit import — the compiler injects it into the test prelude. The compiler enforces that it cannot be derived in production code.

**Property tests.** `#[property]` attribute on a test case. The framework generates random inputs using the `Arbitrary` trait and runs the test body for each. On failure, it shrinks the input to the minimal failing case. A property test case takes typed parameters, which v1's test blocks do not:

```
#[property]
test "sort preserves length"(input: Vec[i32]) {
    assert(sort(input.clone()).len() == input.len());
}

#[property(cases = 1000)]          // default is 100
test "sort is idempotent"(input: Vec[i32]) {
    let once = sort(input.clone());
    let twice = sort(once.clone());
    assert(once == twice);
}
```

`#[derive(Arbitrary)]` generates random instances for custom types by generating each field independently. Types with invariants implement `Arbitrary` manually:

```
#[derive(Arbitrary)]
struct Point { x: f64, y: f64 }

#[derive(Arbitrary)]
enum Shape {
    Circle { center: Point, radius: f64 },
    Rect { top_left: Point, width: f64, height: f64 },
}
```

**Shrinking.** `Arbitrary` and `Shrink` are separate traits; `#[derive(Arbitrary)]` auto-derives both. A manual `Arbitrary` impl must also provide a `Shrink` impl (or use the `NoShrink` wrapper to opt out). The default derived shrinking strategy is: integers shrink toward zero; collections remove tail elements one at a time; structs shrink each field independently in order. `Shrink` is a test-only trait — the compiler rejects it in production code.

```kara
trait Shrink {
    fn shrink(ref self) -> Vec[Self];   // returns candidate smaller values; empty = minimal
}
```

#### Snapshot tests

| Function | Signature | Failure message |
|---|---|---|
| `assert_snapshot(expr)` | `fn assert_snapshot[T: Display](expr: ref T) with panics` | diff of current output vs saved snapshot |

`assert_snapshot` propagates `panics` and requires `Display` (user-facing text format, not debug repr).

**Snapshot tests.** `#[snapshot]` attribute on a test case. First run saves the output to a file. Subsequent runs compare against the saved snapshot — failure means the output changed. `karac test --update-snapshots` accepts new output as the baseline. Useful for compilers, formatters, and code generators:

```
#[snapshot]
test "pretty printer" {
    let ast = parse("fn foo() { 42 }");
    assert_snapshot(pretty_print(ast));
}
```

**Snapshot file location and identity.** Snapshots are stored at `tests/snapshots/<module_path>/<slugified-case-name>.snap` relative to the package root. The file name is derived from the case-name string with a stable slugification rule (lowercase, replace runs of non-alphanumeric characters with a single underscore, trim leading / trailing underscores). Renaming a case produces a new snapshot path; the old `.snap` file becomes an orphan and `karac test` reports it as stale (remove with `karac test --clean-snapshots`). Snapshot files are plain text — the `Display` output of the value passed to `assert_snapshot`. They are committed to source control.

#### Fuzz tests

**Fuzz tests.** Functions in `_fuzz.kara` files take raw bytes and must not crash. The compiler generates a libFuzzer-compatible harness. Coverage-guided — libFuzzer mutates inputs to maximize code path coverage. Run with `karac fuzz`, never in CI:

```
// parser_fuzz.kara
fn fuzz_parser(input: Vec[u8]) {
    if let Ok(s) = String.from_utf8(input) {
        let _ = parse(s);
    }
}
```

Seed corpora live in a `corpus/` directory next to the `_fuzz.kara` file. `karac fuzz` runs until interrupted; findings are saved for replay.

#### Benchmarks

**Benchmarks.** Functions prefixed with `bench_` in `_bench.kara` files. The runner handles warmup, iteration count, and statistics (min/max/mean/stddev):

```
// sort_bench.kara
fn bench_sort(b: Bencher) {
    let data = random_vec(10000);
    b.iter(|| sort(data.clone()));
}
```

`karac bench` runs all benchmarks. `karac bench sort` filters by name.

### Performance Diagnostics

Three-tier structured diagnostics, designed for machine consumption first:

**Tier 1: Inline notes** (always emitted, never block compilation):
```
perf[layout-opportunity]: src/physics.kara:12
  `entities` iterated 3 times — each iteration accesses only {position, velocity}.
  hint: consider adding a layout block with `group physics { position, velocity }`.
```

**Tier 2: Summary report** (opt-in, `karac build --perf-report`).

**Tier 3: Suppression** via `#[allow]` on the note's lint name.

#### Cumulative Cost Surface

The "no-compromise" claim leans on the compiler picking the right representation. In practice the compiler reaches for runtime-cost fallbacks in several places, each well-specified individually. This subsection enumerates them in one place so the cumulative cost of a program is auditable rather than scattered across the chapters that introduce each fallback.

| Cost site | Trigger | Cost shape | Section |
|---|---|---|---|
| Per-field borrow flag on `shared struct` mut-field | `shared struct` with at least one `mut` field; conflict detected at runtime | 1 byte per mut field; check is a load + CAS; runtime panic on conflict | [design.md §11](design.md#11-ownership-and-sharing) |
| Per-fork partition guard | Parameterized resource key is dynamic (not literal); compiler can't prove key distinctness statically | One comparison per fork-edge per access — `O(forks)` extra branches at parallel-group entry | [Parameterized Resources](#parameterized-resources-opt-in-finer-granularity) |
| REPL `--auto-clone` insertion | Cell binding consumed in one cell, referenced in a later cell; opt-in flag enabled | One `.clone()` per cross-cell consume; cost is `T`'s clone; emits `perf[auto-clone-in-repl]` note | [Interactive Evaluation Model](#interactive-evaluation-model) |

**The summation problem.** Each individual fallback reads as cheap; the sum across a non-trivial program is not analyzed inline. A binary that silently picks borrow-flag + partition-guard pays the sum of all of them. The audit mechanism (`karac explain`, the `perf[]` notes above) catches each decision at its site. What the programmer cannot answer from those alone is "across this binary, how much am I paying to fallbacks total?" That is the question `karac query cost-summary` exists to answer (an addition to the [compiler query API](design.md#compiler-query-api)).

**Static counts vs. runtime attribution.** `karac query cost-summary` returns *static counts* — per-function and per-binary tallies of how many borrow-flag fields, partition-guard sites, and auto-clone insertions the compiler emitted, with a `derivation` link to each source location. These are facts the compiler knows from its own passes — no instrumentation, no profile required, available the moment those passes exist. *Runtime attribution* — what fraction of wall-clock time is actually paid at each cost site against a representative workload — requires a sampling profiler over an instrumented binary, and comes later. The static-count form is sufficient to answer "where does the compiler reach for fallbacks in this binary?"; the runtime-attribution form turns that into "what does it actually cost?"

**Discipline.** Every silent runtime cost the compiler can insert appears in the table above, with an order-of-magnitude estimate and a link back to the section that introduces it. A new compiler pass that inserts a cost without updating this table is a spec-completeness bug. Order-of-magnitude estimates are deliberate — exact numbers depend on target, profile, and workload, and tightening the estimates without ground-truth measurements would mislead. The Cumulative Cost Surface is descriptive (this is what the compiler may insert), not normative (these costs are acceptable) — programs that find any cost unacceptable have the standard escape hatches: restructure to eliminate the trigger, declare an explicit ownership tier, or use `#[allow]` / `seq {}` / `independent` declarations to overrule the compiler's conservative defaults.

**`karac query cost-summary` output schema.** Returns a per-function and per-module aggregate of every silent runtime cost the compiler emitted, keyed by the categories enumerated under [Cumulative Cost Surface](#cumulative-cost-surface). It reports *static counts* — the compiler's own bookkeeping of how many sites of each kind it emitted, plus a `derivation` array of source locations:

```json
{
  "scope": "src/order.kara",
  "totals": {
    "borrow_flag_fields":     4,
    "partition_guard_sites":  1,
    "auto_clone_insertions":  0
  },
  "by_function": [
    {
      "function": "process_order",
      "borrow_flag_fields": 1,
      "derivation": [
        {"reason": "`shared struct Cart` has a `mut` field",
         "site": "src/order.kara:42:9"}
      ]
    }
  ]
}
```

*Runtime attribution* — what fraction of wall-clock time is actually paid at each site against a real workload — comes later. It requires a sampling profiler over an instrumented binary; the static-count form is sufficient to answer "where does the compiler reach for fallbacks in this binary?", and the runtime-attribution form turns that into "what does it actually cost?" The static answer is the one the compiler can give without external infrastructure, so it ships first; the runtime answer is acknowledged as a follow-up so the spec stays honest about the distinction.

A future `--monomorphization-budget=warn:N,error:M` flag will read the data of `karac query monomorphization` ([design.md §17](design.md#compiler-query-api)) at compile time to enforce per-generic ceilings (warn at one threshold, error at another). Its default is left unspecified until it can be measured against real programs — picking thresholds without that measurement would lock in numbers that turn out to be wrong (same trap as the auto-concurrency cost model). The tracking mechanism is in v1; the policy threshold lands later.

### Diagnostic Namespace Attributes (`#[diagnostic::*]`)

> In v1 an attribute path has one segment, so `#[diagnostic::*]` and tool namespaces are both errors ([design.md §17](design.md#attributes)).

Trait designers — both stdlib and user — sometimes know better than the compiler what error message a failed bound deserves, or which of several legal `impl`s a diagnostic should suggest. Kāra reserves the `#[diagnostic::*]` attribute namespace as the channel for these hints. Two members are designed; the namespace itself is open for compiler additions across versions.

**Surface.**

```kara
#[diagnostic::on_unimplemented(
    message: "the type `{Self}` cannot be used as a map key",
    label: "`{Self}` is not Hash",
    note: "derive it with `#[derive(Hash)]` or write an impl"
)]
trait Hash { ... }

#[diagnostic::do_not_recommend]
impl[T: Iterator] IntoIterator for T { ... }
```

**The two members.**

- **`#[diagnostic::on_unimplemented(...)]`** — applies to a `trait` declaration. Replaces or augments the standard "trait `T` not implemented for `U`" diagnostic when a bound `U: T` fails. Three named arguments (all optional): `message: "..."` (the headline), `label: "..."` (the underline label at the offending span), `note: "..."` (an accompanying explanation). The argument values are template strings — `{Self}` interpolates the type that failed the bound; `{T0}`, `{T1}` interpolate trait type parameters; the placeholder set is fixed (no general expression evaluation). Unknown placeholders produce a build-time warning at the *trait declaration site* and render literally in the diagnostic. A trait with no `#[diagnostic::on_unimplemented]` falls back to the compiler's standard "the trait `T` is not implemented for `U`" wording.

- **`#[diagnostic::do_not_recommend]`** — applies to an `impl` block. When a downstream call fails to satisfy a bound and the compiler scans candidate impls to suggest in the diagnostic ("note: the trait is implemented by ..."), an impl carrying this attribute is **omitted from the suggestion list**. The impl is still legal, still selected by ordinary lookup, and still part of coherence — only the diagnostic-suggestion path skips it. Used for blanket `impl[T: Iterator] IntoIterator for T` shapes where the suggestion would be misleading because the *direct* impl is the recommended path.

**Advisory contract.** Every member of the `#[diagnostic::*]` namespace is **advisory only**:

- The compiler **may** ignore the attribute (or honor it differently across versions) without breaking program semantics. Programs compile to the same code whether or not the attribute is present; only diagnostic shape changes.
- The compiler **may** add new `#[diagnostic::*]` attributes in any release without an edition bump. New attributes are additive — older compilers see them as "unknown but in the diagnostic namespace" and accept silently.
- An unrecognized attribute in this namespace is **silently accepted**, not rejected. This is the load-bearing distinguishing rule from the rest of the attribute system, where unknown attributes are a hard error. A `#[diagnostic::polish_my_error_pretty]` written today against a future compiler version that doesn't recognize it produces no diagnostic at the use site.
- A malformed attribute *within* the namespace (wrong argument shape, unknown named field, invalid value type) is a *warning*, not an error, attached to the attribute span. The build proceeds; the attribute is ignored. The lint name is `malformed_diagnostic_attribute` (warn-by-default, suppressible with `#[allow(malformed_diagnostic_attribute)]`).

The advisory rule reflects a deliberate trade: in exchange for the compiler retaining freedom over diagnostic wording (which is part of the *reported-behavior* layer of the spec, not the *guaranteed-semantics* layer — see [design.md §2](design.md#2-specification-layers)), trait designers can hint at improvements without locking the compiler into a specific output format. The same trade-off underpins Rust's `#[diagnostic::*]` namespace; Kāra adopts the convention so cross-language muscle memory carries.

**Where they may appear.**

| Attribute | Valid item kinds |
|---|---|
| `#[diagnostic::on_unimplemented(...)]` | `trait` declaration only |
| `#[diagnostic::do_not_recommend]` | `impl` block only (inherent or trait) |

Applying either to an unsupported item kind is a malformed-attribute warning (not an error), and the attribute is ignored. Applying multiple `#[diagnostic::on_unimplemented]` to one trait emits the warning and uses the first one in source order.

**Reservation.** The `diagnostic::` path prefix is reserved for compiler use — adding attributes here is a non-breaking minor change for the compiler. User code may write `#[diagnostic::custom_thing]` and have it silently accepted, but no semantics are guaranteed and the name space may collide with a future compiler addition (in which case the new built-in member takes precedence and the user's intent is lost). Stdlib trait designers are the primary audience; third-party trait authors may use the namespace at their own risk under this reservation policy.

**Cross-reference: tool namespaces.** The general path-prefixed attribute form `#[NAMESPACE::NAME]` is also used by tool-namespaced attributes (e.g., `#[rustfmt::skip]`-style external-tool hints) — see [Tool-Namespaced Attributes](#tool-namespaced-attributes) for the mirror system reserved for formatters/linters/analyzers.

### Tool-Namespaced Attributes

External tools — formatters, linters, static analyzers, IDE plugins, custom code generators — sometimes need to attach hints to source items (`don't reformat this block`, `suppress this lint here`, `this function is the entry point of my custom analysis`). Kāra reserves the multi-segment `#[TOOL::NAME]` form for this purpose. The compiler accepts these attributes syntactically, stores them on the AST so tools can read them, and **does nothing else with them** — no validation of arguments, no semantic interpretation, no behavior change in compiled output.

**Surface.**

```kara
#[karafmt::skip]
fn manually_aligned_table() { ... }

#[karalint::allow(complexity)]
fn complicated_inner_loop(data: ref Slice[Frame]) -> Frame {
    // ...
}

#[someanalyzer::entry_point(group: "auth")]
pub fn login(username: String, password: String) -> Result[Session, AuthError] { ... }
```

The compiler treats every multi-segment attribute path as a tool-namespace hint *unless* the first segment names a compiler-reserved namespace (currently only `diagnostic::*` — see [Diagnostic Namespace Attributes](#diagnostic-namespace-attributes-diagnostic)). The discriminator is structural: a *bare-name* attribute path (`#[derive]`, `#[no_mangle]`, etc.) must match a known compiler attribute or it is `error[E_UNKNOWN_ATTRIBUTE]`; a *multi-segment* path is either a compiler-reserved namespace (validated) or a tool namespace (silently accepted).

**The compiler's contract.** For every `#[TOOL::NAME(...)]` attribute on an item:

1. **Parse.** The attribute path is parsed per the standard `ATTR_PATH = IDENT { "::" IDENT }` rule. Arguments inside the optional parens are parsed as `ATTR_ARGS` — same grammar as bare-name attributes — but are **not** type-checked or validated against any expected schema. Any `IDENT ":" EXPR` form whose `EXPR` is syntactically valid at the attribute-arg level is accepted; semantic interpretation is the tool's concern.
2. **Store.** The full attribute (path, arguments, source span) is captured on the AST and exposed via `karac query attributes` and the language-server protocol so external tools can iterate over them.
3. **Ignore.** The compiler emits no diagnostic, no warning, no semantic effect. The attribute does not appear in compiled output. A `#[karafmt::skip]` on a function that the formatter never sees is harmless.

**Why no per-tool registration.** This design deliberately avoids a `#[register_tool(name)]`-style mechanism (Rust has one, currently unstable). The cost of registration is friction (every project's `kara.toml` must enumerate every tool it allows; CI has to gate); the benefit is typo detection on tool names (a `#[karafmpt::skip]` typo currently goes undetected at compile time). The simpler open-namespace rule is the better trade — language-server and tool-side validation can catch typos in their own namespace cheaply, and adding a per-project allow-list later is non-breaking.

**Reserved tool names.** The Kāra organisation reserves the following tool namespaces for the canonical first-party tools. User code may write attributes against them today — they parse and store like any other tool namespace — but their semantics will be defined when the corresponding tool ships, and the names will not be reused by any other tool:

- **`karafmt::*`** — the canonical formatter (planned). Initial members: `karafmt::skip` on any item suppresses formatting for that item.
- **`karalint::*`** — the canonical lint pack ride-along (planned, separate from the compiler's built-in lints, [design.md §17](design.md#lint-levels)). Initial members: `karalint::allow(NAME)`, `karalint::warn(NAME)`, `karalint::deny(NAME)`, `karalint::expect(NAME)` — same shape as the compiler's built-in lint attributes but scoped to lints that live in the external `karalint` package.
- **`karadoc::*`** — the canonical doc generator (planned). Initial members: `karadoc::hidden` to omit an item from generated docs.

The reservation is a name-claim, not an implementation commitment — it prevents accidental name collision when the tools land. Until they do, `#[karafmt::skip]` is functionally a no-op.

**Third-party tool namespaces.** Any other multi-segment path is also accepted. By convention, third-party tools should use a namespace matching their package or organisation name (e.g., `acmecorp_security::audit_required`) to avoid collision with the reserved names above. The compiler does not enforce this convention; the conflict-resolution authority is social — first registered, first served, with the reserved names taking absolute precedence.

**Reading tool attributes from outside the compiler.** Tools are expected to consume the source via one of three paths:

- **`karac query attributes [--tool TOOL]`** — emits a JSON list of every attribute matching the requested namespace, with item kind, item name, source span, and parsed argument values. The `--tool` filter is a prefix match; `--tool karafmt` returns every `#[karafmt::*]`. Without `--tool`, returns every tool-namespaced attribute.
- **Language Server Protocol** — the IDE-facing surface exposes the same data through workspace-symbol and document-symbol responses.
- **Direct AST access (in-process)** — tools written in Kāra and using the compiler-as-library API read the same `Attribute { path, args, span }` structures the typechecker stores.

**What is not allowed.** Tool-namespaced attributes are accepted in any item-attaching position the bare-name family supports (functions, structs, enums, traits, impls, modules, statements, expression blocks). A tool-namespaced attribute at the **expression** level (where bare-name attributes are also accepted) is syntactically legal but exotic; the compiler stores it without comment, but tooling discoverability is weaker because the LSP surfaces item-level attributes more readily than expression-level ones. Tools expecting to be portable should attach to items.

**Cross-reference.** The `#[diagnostic::*]` namespace ([Diagnostic Namespace Attributes](#diagnostic-namespace-attributes-diagnostic)) is *compiler-reserved*, not a tool namespace — its members have semantic effects in the compiler. The discriminator between the two regimes is the namespace's first-segment identity, baked into the attribute checker.

### Cranelift as a JIT Backend

**Why LLJIT, not MCJIT or Cranelift.** Three options were considered for `karac run`'s JIT:
- **MCJIT.** Feature-frozen by LLVM upstream; `clients should migrate to LLJIT` per the official guidance.
- **LLJIT / ORC v2** (the modern LLVM JIT). Keeps Kāra on a single LLVM stack across AOT and JIT. **Selected for v1.**
- **Cranelift** (a separate JIT-focused codegen). ~10× faster compile than LLVM at ~14% worse code quality; would require maintaining a second backend. Left for later evaluation — Kāra's headline path is `karac build` to native, not REPL-as-primary, and the cost of a dual-backend story is not yet justified. [Runtime Monomorphization JIT](#runtime-monomorphization-jit) leans toward Cranelift for a shared in-process compiler.

### Debug-Mode Leak Detector

**Debug-mode leak detector (safety net).** In debug builds, the runtime tracks `shared struct` allocations and reports unreachable instances with non-zero RC at program exit. In release builds, the detector is compiled out — zero overhead. A cycle of strong `shared` handles leaks ([design.md §11](design.md#11-ownership-and-sharing)); this detector reports such cycles, which should have used `weak` for a back-edge.

### Migrating `shared` Types to `sync`

These aids help a program move a `shared struct` to a `sync struct` ([design.md §11](design.md#sync-types)) when it starts to need the value across tasks.

**Tier 2 perf note at definition time (opt-in via `karac build --perf-report`).** When the compiler sees a `shared struct` definition with one or more bare `mut` fields, it includes a perf note in the summary report: *"`shared struct Foo` has mut fields; if a future caller needs concurrent access, the migration to `sync struct` is structural — consider defining as `sync struct` from the start."* This is **off by default** — definition-site notes are predictive (they fire before any cost is paid), so they belong in Tier 2 rather than the always-emitted Tier 1 inline stream. `--strict` is for correctness lints; perf notes use the existing `--perf-report` opt-in and are not coupled to `--strict`. See [Performance Diagnostics](#performance-diagnostics) for the three-tier taxonomy.

**Preemptive migration tool: `karac migrate shared-to-sync <Type>`.** For projects that have grown around a `shared struct` and want to migrate before any `par {}` forces the issue, the rewrite is available as a workspace command. The tool:

- Rewrites the type definition (`shared struct` → `sync struct`).
- Wraps every bare `mut` field in a synchronized type (defaulting to `Mutex[T]`; programmer can re-edit the diff to choose `Atomic[T]` where the field type is `Copy` and lock-free access is appropriate).
- Rewrites every `.field` read/write across the workspace to the matching access form: bare reads/writes go through a guard (`let g = self.field.lock();`) for `Mutex[T]` fields, or become `self.field.load()` / `self.field.store(v)` for `Atomic[T]` fields ([library/concurrency.md](library/concurrency.md#atomics)).
- Operates in dry-run mode by default (prints the unified diff to stdout); `--apply` writes the changes.
- Refuses to run if the workspace has uncommitted changes outside the rewrite footprint, unless `--force` is passed.

The migration is always **manual at the review step** — the tool produces the diff and the programmer reviews and applies. Concurrent programs carry context-dependent correctness requirements, and a machine-applied fix in this domain is never unconditionally safe without programmer review.

### Forward-looking — `.expose()` as an attention point

`.expose()` and `.expose_mut()` are intentionally greppable. A future iteration of the `karac explain` diagnostic surface may catalog them as "attention points" — surfaced in per-file summaries alongside `unsafe` blocks, effect-heavy boundary functions, and FFI crossings — so that security review tooling can enumerate secret-touching sites without parsing source. It is not part of v1; it is listed here so the ergonomic properties of `.expose()` (greppable, unique method name, no operator alias) are preserved as enabling conditions.

### Per-SCC Effect Diagnostics

**Decision:** Extend the `"mutual_recursion_groups"` JSON field to include the full effect resolution trace — which call site resolved which effect variable, and through which chain.

**Why deferred:** The basic mutual recursion note ships with SCC inference. The full resolution trace requires storing provenance through fixed-point iterations.

**Why non-breaking:** Purely additive to compiler output.

**Design shape:**

```json
{
  "mutual_recursion_groups": [
    {
      "functions": ["f", "g"],
      "effect_variable": "E",
      "resolved_at": "src/main.kara:42",
      "resolved_to": ["writes(DB)"],
      "resolution_chain": [
        { "call_site": "src/main.kara:42", "argument": "|item| write_to_db(item)", "effect": "writes(DB)" }
      ]
    }
  ]
}
```

### Perceus-Style In-Place Reuse

**Decision:** When a `shared` value has exactly one live strong reference at a consumption point, the compiler may reuse the allocation in-place. No language surface changes — purely a codegen optimization.

**Why deferred:** A codegen optimization with no language surface; it waits for measurements that show the reuse pays.

**Why non-breaking:** Purely an optimization. Existing programs are semantically unchanged.

**Design shape:**

- **Condition:** A `shared` value at a consumption site is eligible when no other live variable holds a strong reference. Local dataflow analysis — same pass as the body-level ownership analysis.
- **Weak references:** Do not count toward the Perceus condition.
- **Guarantee:** Best-effort only. Falls back to allocate-copy-drop.

### Karac-Side Bounds-Check Elimination Pass

A compiler-internal pass that pattern-matches common safe-indexing idioms (`for i in 0..xs.len() { xs[i] }`, `if i < xs.len() { xs[i] }`, monotone-step induction over slices) and rewrites the indexing to skip the runtime bounds check before LLVM codegen runs. Sits *above* the v1 bounds-check strategy (LLVM-friendly emission via `llvm.assume` + cold-attribute panic blocks + SCEV/GVN-friendly idioms), catching cases where Karac knows the bound is satisfied but LLVM's range analysis does not.

**Why deferred:** Empirical motivation is missing. The v1 BCE strategy (LLVM-friendly emission + `unsafe { xs.get_unchecked(i) }` escape hatch) is sufficient for every workload measured so far — sieve, brute_force and coin_change all sit in stride-1 / step-based induction territory that LLVM's SCEV/GVN handles natively. A Karac-side pass would catch a *different* class of cases (computed indices the user proves safe, multi-dimensional indexing patterns LLVM can't relate, custom range-bound idioms) that haven't surfaced in measured workloads. Building the pass before real-world data shows the gap risks designing for hypothetical patterns.

**Promotion gate:** Promote when user data shows ≥2 distinct workload classes where the v1 bounds-check strategy leaves a >1.5× perf gap that the user would have to close via `get_unchecked`. The trigger is *frequency in real code*, not theoretical coverage — one rare pattern doesn't justify a Karac-side pass.

**Why non-breaking:** Purely additive. The pass either eliminates a bounds check (faster) or leaves the LLVM-friendly form in place (current behavior). No semantic change. Existing programs run identically or faster. `unsafe { xs.get_unchecked(i) }` continues to be the user-visible escape hatch regardless of whether the pass exists.

**Design shape (sketch — finalize at promotion):**

- Karac-side pass running between typecheck and codegen.
- Pattern-matches `for i in 0..xs.len()` / `for i in 0..N where N == xs.len()` / `if i < xs.len() { xs[i] }` and similar.
- Marks each matched indexing site as "skip bounds check" before lowering to LLVM IR.
- Falls back to LLVM-friendly emission for non-matched sites.
- Diagnostic affordance: `karac explain` should be able to point at an indexing site and say "this could not be elided because *X*; consider rewriting *Y* or using `get_unchecked`."

### Type-Based Alias Analysis (TBAA)

**Decision:** Do **not** emit general C-style type-based alias metadata (`!tbaa`). It does not fit Kāra's memory model; a narrow *sound-subset* investigation stays open. The sound alias facts are `noalias` on `mut ref` / owned params, `readonly` on Freeze `ref T`, and scoped-alias on slice params.

**Why not.** C's strict-aliasing rule — "an access of type A never aliases an access of type B unless the types are related" — is what makes `!tbaa` sound in C/C++. Kāra has **no such rule**, so blanket TBAA is a *silent miscompilation* risk, not a slowdown. Three independent reasons:

1. **The design never specified it.** The backend alias facts are exactly `noalias` and scoped-alias (`!alias.scope` / `!noalias`) — never TBAA.
2. **Kāra permits type punning in `unsafe`.** It adopts **strict-provenance** ([design.md §15](design.md#pointer-provenance)) and allows one address to be accessed under two types: C `union`s "reinterpret the bytes on every access" ([FFI Unions](#ffi-unions-union-foo---)), `transmute`, and raw-pointer `as` casts (`*const T as *const U` in `unsafe`). A `!tbaa` no-alias claim between the two types would be false for such a program — this is the same reason rustc emits essentially no type-based TBAA (its model is uniqueness-based, expressed via `noalias`, exactly the axis Kāra already lowers).
3. **Even a safe-only scalar subset is unsound.** A tagged `enum`'s payload is read at one offset as type A in one match arm and written as type B in another — a genuine same-address / different-type pair reachable entirely in *safe* code. Sound scalar TBAA would have to exclude enum payloads (and audit padding, `MaybeUninit`, allocator reuse, `#[repr]` overlays, …), and the failure mode of any missed case is a silent miscompile.

**The soundness bar.** Any backend alias fact ships only behind a differential-equivalence fuzzed guarantee (compare Rust's multi-year `-Zmutable-noalias` saga). No such fuzzed differential corpus for TBAA exists, so TBAA could not ship even if a sound subset were identified.

**What a future sound-subset investigation would cover.** `!invariant.load` tagging of genuinely-immutable metadata (const data, vtable pointers) — an orthogonal, always-sound mechanism that is not type-based; and a punning-aware struct-path TBAA restricted to accesses the ownership model already proves distinct, gated behind the differential-fuzz corpus. Promote only if a measured kernel shows the scoped-alias facts leave TBAA-shaped headroom on the table.

**Why non-breaking:** Purely a backend-metadata decision; no source-visible surface. The `noalias` / scoped-alias facts already deliver the autovectorization and LICM win.

### Profile-Guided Optimization Loop

**Decision:** Defer instrumented and sample-based (AutoFDO) PGO. The compiler queries channel ([design.md §2](design.md#compiler-queries)) is v1 and covers *intent-shaped* optimization decisions; PGO answers *distribution-shaped* questions and is the complementary signal, not a substitute.

**Why deferred.** PGO requires a full instrumented-or-sampled build flow, a representative-workload protocol, multi-platform replication, and the storage / merge / version-skew machinery for `.profdata` / AutoFDO files. Large surface, separate from the queries channel architecturally. The architectural prerequisites — debug info quality and symbol-stable identity — are partially helped by v1's stable definition ids ([design.md §2](design.md#compiler-queries)) but neither blocks the queries channel itself.

**Two flavors, different cost ladders.**

- **Instrumented PGO (first).** Standard `--profile-generate` → run workload → `llvm-profdata merge` → `--profile-use` flow. New codegen mode invoking LLVM's `InstrProfiling` pass; counter runtime in `libkarac_runtime` (atomic u64 counters, `__llvm_profile_write_file` analog, signal-safe dump on exit); CLI flags `karac build --profile-generate=DIR` and `--profile-use=PATH`; profile lifecycle defaulting to `target/profile/` with `--profile-out=PATH` for committable "blessed" profiles. Counter runtime stance: **Rust port from day one** (~200 lines: atomics + file write + signal-safe formatting), not a `compiler-rt/lib/profile` link — keeps the runtime minimal-dependency, matches Kāra's "small runtime" pitch.

- **Sample-based PGO / AutoFDO (second).** No instrumented build, no separate workload run. `perf record` → `create_llvm_prof` → `--profile-use`. Requires DWARF-quality debug info that survives optimization; `create_llvm_prof` is external (link, don't bundle); function-name stability across rebuilds — i.e., the stable definition ids, with *higher* tolerance for source drift than the queries channel needs.

**Post-link rewriting (BOLT, Propeller).** Plan around them, not against them. Propeller is more interesting long-term (linker-integrated).

**Distinction from the queries channel.** PGO answers questions the LLM author cannot ("what fraction of inputs are ≤16 bytes?", "which call site is on the hot path in production?"); the queries channel answers questions PGO cannot ("is this branch unreachable in correct usage?", "should this trait method specialize on `i64`?"). The two operate on different signals — runtime measurement vs. spec context — and their outputs are independent. A build will likely consume both: the queries channel for intent, PGO for distribution. **PGO also unblocks two deferrals:** an empirically tuned auto-concurrency cost model ([Cost Model](#cost-model)), and the verifier-backed-resolution narrow case for distribution-shaped author claims (`#[likely]` / `#[unlikely]`). Alive2-class verification of arbitrary author invariants stays separately deferred (see § Verifier-Backed Query Resolution).

**Profile representation.** Reuse LLVM `.profdata`. Custom format = no benefit, lots of work, breaks tool interop. Structural-hash keying is what we want for source-drift resilience. **Key alignment risk:** v1's stable identity is a definition id plus an AST-shape structural hash ([design.md §2](design.md#compiler-queries)); LLVM's `.profdata` keys on its own structural hash over LLVM IR. These are not the same hash. The implementation must decide whether `.profdata` keys are computed at the LLVM-IR level (LLVM's hash, opaque to Kāra) or re-keyed against Kāra's DefId before serialization — open question, decide at PGO ship time.

**Promotion gate.** Promote when (a) the queries channel has wired real queries and observed real-world resolution patterns, AND (b) the stable definition ids have shown adequate symbol stability across realistic source-edit patterns to support PGO-style profile keying. Without (a), shipping PGO first risks confusing the channel-vs-PGO boundary in user mental models; without (b), profile-key drift dominates the cost-benefit calculus.

**Why non-breaking:** Purely additive. PGO flags, the `.profdata` format, and the corresponding `karac build --profile-generate/use=...` invocation are all new build-time surface. Existing builds continue unchanged.

### Continuous PGO with Shared-Object Hot-Swap

**Decision:** Defer continuous PGO (live counter collection in production + background recompile + hot-swap), with the rest of the warehouse-class adaptive-performance story.

**What it adds beyond static PGO.** Mechanically: PGO (above) plus a hot-reload story.

1. Production binary collects counters live (low-overhead instrumentation, AutoFDO-style sampling, or hardware perf counters).
2. Counter snapshots ship to a build farm or sidecar periodically.
3. Background compile produces a new shared object with an updated profile.
4. Running process `dlopen`s the new shared object; function pointers redirect to new bodies. Old bodies stay live until in-flight calls drain.

No deopt, no OSR, no fresh verification — the new binary went through the same AOT checker as the old one. Soundness story identical to AOT; effects/ownership invariants survive trivially. Latency is minutes, not microseconds — fine for warehouse-scale services, wrong shape for sub-second adaptation.

**Architectural commitments.** Without these, retrofitting hot-swap means recompiling every binary:

1. **`--enable-hot-swap` codegen flag** (off by default) — emits PLT-style indirection for `extern`-public module symbols. Default off; turning it on is non-breaking. **Granularity is module-level, not function-level** — internal calls stay direct; hot-swap targets module boundaries. Reload `auth` module to swap `auth.verify`, not the function in isolation.
2. **AOT-perf cost of indirection** must be benchmarked at flag-ship time. "Tentative <1% overall" applies amortized over a whole program; worst-case hot inner-loop sites can be 10–20%. Per-symbol opt-in is a fallback if module-wide cost is unacceptable, but contradicts the warehouse use case if hot-swap targets are dispersed.

**What comes later.**

- Drain protocol — RCU-style quiescence for retiring old code. Tied to the `suspends` effect verb: loops that already have suspend points are drain-safe; loops without get a compile warning. Realistic engineering scope: 10–12 weeks.
- Orchestrator — daemon, k8s sidecar, or `karac` subcommand that triggers rebuild and reload.
- Counter collection wire format — concatenable per LLVM `.profdata` precedent, with the same key-alignment caveat as the PGO entry above.

**Audience constraint.** Same W^X gate as runtime monomorphization (below): production with strict W^X (browsers, iOS, gVisor sandboxes, FIPS deployments) cannot hot-swap; falls back to AOT-only. Real audience is "Linux + macOS + Windows servers without strict W^X enforcement."

**Promotion gate.** Warehouse-grade adaptive performance becoming a stated goal. Other promotion criteria (drain protocol design audit; orchestrator design; counter collection format spec) are downstream — gate first on that positioning decision.

**Why non-breaking:** The hot-swap codegen flag is off by default. Existing binaries continue to work; opting into hot-swap requires a rebuild with `--enable-hot-swap`.

### Runtime Monomorphization JIT

**Decision:** Defer runtime monomorphization JIT (in-process specialization of generics on first call, for `T` arriving via a dynamic boundary).

**What it is.** Kāra is monomorphization-first; AOT generates one body per `Vec[T]` instantiation it can see. The narrow gap: a `T` arriving via a dynamic boundary — JSON / msgpack / protobuf deserialization into a generic container, FFI returning an opaque type, dynamically-loaded plugins instantiating templates declared in the host. For these cases, today's options are monomorphize-everything-needed at AOT (impossible if `T` is genuinely runtime-discovered) or fall back to `dyn Trait`. The runtime monomorphization JIT compiles the missing instantiation on first use; subsequent calls hit a code cache.

**Why uniquely defensible for Kāra.**

- **Unit of JIT is well-defined** — one generic instantiation. Not a hot loop, not an inlining decision; a whole function body for a specific `T`.
- **No fresh verification.** Effects, ownership, trait bounds were AOT-checked on the *generic* body. The JIT's job is purely codegen-substitution. **This is what differentiates it from speculative tiering (HotSpot-class) — engineering you can throw bodies at; verification surface you can't.**
- **IR shipping is bounded.** Bitcode for JIT-deferred generics ships in the binary's `.kara_jit_template` section. Binary-size cost is opt-in per author.
- **Fallback is well-defined.** JIT-unavailable (W^X-locked target) → call site errors at the dynamic boundary, not silently.

**Strongest motivating use case: deserialization.** Every Kāra service that parses JSON / msgpack / protobuf into a `Vec[T]` where `T` is data-driven (a polymorphic event union, a schema-discovered row type) hits exactly this gap. Not an HPC niche — mainstream backend code. The use-case overlap argument is *stronger* in Kāra than in C++: Kāra has fewer escape hatches than `std::variant` / virtual dispatch / `dlopen`-plugin patterns / external codegen frameworks, so the narrow gap matters more.

**Bitcode-embedding policy.** Author opt-in via `#[jit_template]` annotation — predictable, requires per-library decisions. **Picked over compiler-derived ("any generic crossing a dynamic boundary")**; compiler-derived is a refinement once usage patterns surface. "Embed all generics" is untenable (template-heavy libraries 10–100× the bytecode size when bodies are embedded as IR).

**Architectural commitment.**

- **`.kara_jit_template` section + opaque-payload version manifest** — define the section name and version manifest first, and leave actual emission and consumption for later. Manifest format: single byte for "version" + length-prefixed opaque payload. The empty form is `[0x00, 0x00, 0x00, 0x00]`; a later format takes any version 1+. **Trivially future-proof.**
- **Hard-error on `karac build --target=embedded` and `--target=wasm-*`** — both gate categorically (no `mmap(PROT_EXEC)` on embedded; WASM has no equivalent at all). Same gate that applies to `--enable-hot-swap`.

**IR ABI stability across runtime / compiler version skew.** This is the operational kill that ended ClangJIT (the C++ research project that prototyped this exact architecture in 2019) — embedded LLVM IR is not stable across LLVM major versions; binaries with embedded bitcode broke under runtime upgrades. **Initial stance is (a):** pin runtime + AOT-compiler to the same Kāra version. Practical short-term; means a binary with embedded JIT templates is not redistributable across Kāra releases. The harder solutions — (b) Cranelift CLIF as the embedded format, or (c) re-emit a portable Kāra-side stable IR (KIR) — are evaluated at promotion time.

**JIT engine choice (Cranelift vs LLVM ORC2).** Tentative Cranelift: smaller, faster-compiling, JIT-tuned, ~10% slower steady-state code than AOT in exchange for ~30× compile speed and ~10× smaller runtime footprint. ORC2 reuses the AOT pipeline exactly. Decide at ship time. **Position: the REPL JIT and runtime monomorphization JIT (this entry) share infrastructure** — locks future implementations to converge on a single Kāra in-process compiler runtime rather than shipping two. Implies Cranelift; the REPL pays a small steady-state perf cost vs. LLJIT but the runtime stays single-source.

**Audience constraint (W^X).** Production with strict W^X enforcement cannot run runtime JIT: browsers (Chrome's V8 hardening), iOS, Android, hardened kernels, gVisor-style sandboxes, FIPS-compliant deployments. WASM target categorically lacks `mmap(PROT_EXEC)`. Real audience is "Linux + macOS + Windows servers without strict W^X" — real but smaller than naive framing implies.

**Modeled as effects.** A function that triggers JIT compilation `allocates(JitCode)` and `panics`. The type system reflects the runtime cost.

**Cost surface (engineering).** Cranelift-based runtime specializer + code cache + mmap+exec capability detection + W^X fallback + IR-version-skew handling + security review of arbitrary code generation in production processes + operational tooling (cache invalidation on binary upgrade, profile-of-JIT'd-code observability). **Realistic estimate: 16–20 weeks** for a production-shippable version, not the 8–12 happy-path number.

**Prior art that worked / didn't.** CUDA driver JIT (GPU bitcode → device code on kernel launch) — works at warehouse scale, validates the embedded-IR + runtime-specializer architecture. ClangJIT (Hal Finkel et al., SC19 2019) — C++ research project that designed exactly this for templates; worked technically; never landed in mainline Clang. Failure modes documented above (IR-version coupling; W^X; bitcode size; use-case overlap with existing escape hatches; maintenance ownership; scope of upstream surgery).

**Promotion gate.** Promote when (a) the IR ABI stability question has a definite position (pick (b) or (c) above; (a) is a stopgap), (b) the W^X audience constraint is acceptable to the target user base, and (c) at least one in-tree use case (e.g., dynamic deserialization in a stdlib JSON path) has materialized.

**Why non-breaking:** The `.kara_jit_template` section and manifest are reserved-and-empty surface; binaries do not embed bitcode until the JIT exists. `#[jit_template]` annotation, when added, attaches to opt-in items only.

### MLGO Trained Policy Artifacts

**Decision:** Defer MLGO-style trained policy artifacts. A trained model is the *answer* (compiler output), not the *question* (compiler input) — different shape from the queries channel. Possible later if real-world data shows the queries channel alone underperforms.

**Why deferred.** LLVM's MLGO trains TFLite policies for inliner / regalloc decisions and ships the trained model as a build-time artifact. Kāra's queries channel takes a different stance: surface the decision back to the LLM author at authorship time and bake the resolution into source. The MLGO and queries-channel approaches are not contradictory — they could coexist — but the queries channel is the smaller commitment and ships first.

**Promotion gate.** Promote when (a) the queries channel has wired its planned queries and observed real-world adoption, AND (b) measurable evidence exists that author-resolved queries fail to capture optimization wins MLGO would capture. Without (a), shipping a policy artifact pre-empties the channel; without (b), the case for MLGO over queries is hypothetical.

**Why non-breaking:** Additive. Trained policy artifacts are build-time inputs to specific optimization passes; they do not change source semantics, public APIs, or the queries channel's interface.

**Cross-reference:** [design.md §2](design.md#compiler-queries) (the v1 alternative).

### Verifier-Backed Query Resolution

**Decision:** Defer Alive2-class equivalence verification of author-supplied query resolution annotations. Trust-the-author is the v1 baseline; verification is the known future direction.

**Why deferred.** A wrong `#[likely]` or `#[specialize(T = i64)]` produces suboptimal codegen — no worse than today's annotation surface. But the queries channel deliberately concentrates author claims into a structured surface, which both invites *more* claims and makes those claims *more* tractable for verification than today's scattered annotations. STOKE / Souper / Alive2 / Hydra / Minotaur / Iago demonstrate that verifier-backed claim-checking is feasible at the LLVM-IR level; the same primitives could check author claims like "this branch is unreachable in correct usage" against the program's effect/type structure.

**Promotion gate.** Promote when (a) the queries channel has wired its planned queries and adoption has surfaced concrete cases of wrong author claims producing observable codegen pessimization, AND (b) a verifier infrastructure exists in Kāra (Alive2-style equivalence checks against author invariants, separate from but adjacent to the existing effect / ownership / type systems). Without (a), the case for verification is theoretical; without (b), shipping verifier-backed resolution requires building the verifier from scratch alongside, which doubles the commitment.

**Intended design shape.** A `karac check --verify-resolutions` mode (or build-time flag) that, for each query resolution annotation in the source tree, attempts to verify the author's claim against the surrounding program structure. Verification failures emit a new diagnostic class — distinct from "this annotation is suboptimal" — that names the specific invariant the verifier could not establish. Authors who don't run verification continue to operate in the trust-the-author mode.

**Why non-breaking:** Purely additive. Existing resolution annotations continue to be honored at trust-the-author level by the codegen path. The new verification mode is opt-in; failure to run it does not change codegen behavior.

**Cross-reference:** [design.md §2](design.md#compiler-queries) (the v1 trust-the-author baseline).

### MLIR Adoption as Codegen Substrate

**Decision:** Defer MLIR adoption as Kāra's codegen substrate. LLVM-direct is the right substrate for Kāra's positioning ([design.md §1](design.md#1-what-kāra-is)). MLIR's value is heterogeneous numerical compute as a primary thesis — Mojo's territory — which is explicitly **not** Kāra's positioning.

**Why deferred.** MLIR is a multi-level IR designed for compilers whose center of gravity is heterogeneous numerical compute (CPU + GPU + TPU + custom accelerators) with cross-target kernel fusion as a load-bearing optimization. Adopting MLIR would cost a substantial codegen rewrite for marginal gain: Kāra's CPU + GPU coverage already routes through LLVM (NVPTX for CUDA, wgpu/WGSL for vendor-neutral GPU), and `Vector[T, N]` SIMD lowering is well-served by LLVM's existing SIMD infrastructure. The MLIR ecosystem's chief advantage — *cheap new-target codegen via dialect plug-in* — only pays off if Kāra commits to adding new accelerator backends (NPU/TPU/FPGA), which is itself deferred (see [Heterogeneous Compute — Beyond CPU + GPU](#heterogeneous-compute--beyond-cpu--gpu)).

**Architectural intent.** Codegen is deliberately the only LLVM-aware phase in the pipeline; upstream phases (AST, typecheck, effect, ownership, concurrency analysis) treat the backend as a black box. A future MLIR adoption would be a **contained surgery on one module**, not a rewrite of the compiler. The maintainership invariant is that contributors must not couple LLVM types into AST-level or analysis-level structures.

**Promotion gate.** Promote when (a) Kāra's positioning shifts toward heterogeneous numerical compute as a primary thesis — itself a positioning decision, not a backend one, AND (b) the heterogeneous-compute capability expansion in [Heterogeneous Compute — Beyond CPU + GPU](#heterogeneous-compute--beyond-cpu--gpu) has been scheduled, generating concrete demand for cheap new-target codegen. Without (a), MLIR is solving a problem Kāra has chosen not to have; without (b), MLIR's chief advantage is unrealized.

**Why non-breaking:** Codegen-substrate swap is invisible to source. No user-language semantics change. Build-system flags and intermediate artifacts may change (LLVM IR dump → MLIR dump for diagnostics), but those are tooling, not language surface.

### Binding Generator (`kara-bindgen`) — Mechanizing the C-Shim Consume Path

**Decision:** Defer the *tool*; take the *worked example* first. The consume-direction FFI surface is part of v1 (`unsafe extern "C" { }` blocks, effect defaults, opaque types; [design.md §15](design.md#extern-c)), and calling a Rust crate goes through a C shim. What this entry adds is the tooling that mechanizes hand-writing bindings: (a) a **C-header importer** — Rust's `bindgen` analogue, `.h` → generated `unsafe extern "C" { }` declarations with effect-annotation decisions; (b) a **Rust-crate shim generator** — crate → `extern "C"` shim crate + effect-annotated Kāra declarations, mechanizing what the runtime's regex/arrow/wgpu/rustls bridges do by hand.

**Why the tool is deferred.** A generator rushed for a launch fails on the first real header (`sqlite3.h`, `curl.h`, `glibc`), and real C headers are a tarpit — function-like macros, conditional compilation, varargs, bitfields, function pointers; Rust's `bindgen` rides libclang and took years to be trustworthy. A half-working generator turns an honest limitation ("bindings are hand-written today; here's the pattern, a real worked binding, and the planned tool") into a broken promise. The generator also gets strictly *better* by waiting: each hand-written binding becomes validation corpus + oracle for the tool, and the effect-annotation heuristics (when to default `blocks`, when to flag known-allocating patterns — seeded by the linter hints for extern functions, [design.md §15](design.md#effects-of-extern-functions)) need that corpus to be tuned honestly.

**The worked example.** A consume-direction binding of a real C library, `examples/interop-zlib/`, with a binding guide in its README: a zlib compress → decompress → CRC32 round trip through an effect-annotated `unsafe extern "C" { }` block and `[link] libs = ["z"]`, checked byte-identical against a C reference implementation. It is binding #1 of the promotion-gate corpus.

**Why non-breaking:** Purely additive tooling — generated declarations are ordinary `unsafe extern { }` blocks; no language surface changes. The worked example is examples/docs only.

**Promotion gates (build the generator when any of):**
1. User feedback shows binding *cost* — not binding *possibility* — is the recurring adoption objection (the signal the tool answers, distinguished from the objection the worked example answers).
2. A corpus of 3+ hand-written bindings exists in-tree/katas to serve as the tool's validation oracles.
3. The header-importer scope gets a cheap first leg: evaluate riding libclang (as Rust's bindgen does) vs. a declaration-subset parser before any greenfield parsing work — this fork is the first experiment, not a settled design.

**Cross-reference:** [design.md §15](design.md#extern-c) and [Effects of extern functions](design.md#effects-of-extern-functions) (the surface the generator emits into, and the linter-hint machinery that seeds its effect heuristics).

### Rust ↔ Kāra Bidirectional Transpiler

Two officially supported, stability-committed transforms:

- `karac --emit rust` — Kāra source → idiomatic, maintainable Rust.
- `karac --emit kara --from rust` — Rust source → idiomatic, maintainable Kāra.

Neither is a debugging aid or backup plan; both are published output targets. Common-subset code transpiles mechanically (deterministic, reproducible, no LLM in the loop) — owned/borrowed data, generics, traits, pattern matching, most control flow, most effect annotations. Divergent features (effect system annotations beyond docs, layout blocks, shared structs, anything without a 1:1 Rust equivalent) lower via an LLM-assisted impedance-matching tier that emits best-effort functionally-equivalent Rust. Impedance-matched lowerings are cached and reviewable so non-determinism doesn't leak into every build.

**Strong stance — no design compromise for transpile.** Kāra features are decided on their own merits. The transpiler bends, never the language. If a Kāra construct has no mechanical Rust equivalent, that is the transpiler's problem to solve.

**Ownership mode mapping for the common subset:** `ref T` ↔ `&T`, `mut ref T` ↔ `&mut T`, owned ↔ `T`. Receivers map symmetrically. Effects become Kāra-checker metadata on the Kāra side and Rust-side doc comments / proc-macro annotations on the Rust side — they matter for Kāra checking, not for Rust compilation.

**What it rests on:**
- Kāra's "Rust without being Rust" design principle (semantic compatibility with Rust for the common subset is already committed).
- Stable language spec — transpiling against a moving spec produces churn.
- Stable public AST / semantic IR.
- A high-capability LLM invoked at transpile time plus a maintained prompt / test-suite artifact for impedance matching, and a fallback compile-time error path when verification fails ("this construct cannot be mechanically transpiled and LLM verification failed — file an issue").

**Why after v1, not a stdlib / compiler ship:**
1. Language must be stable. A transpiler whose output is a stability-committed artifact cannot chase a moving spec.
2. Two backends is real engineering cost. LLVM (performance, GPU) and Rust-transpile (adoption, exit-ramp) both need maintenance.
3. LLM infrastructure — prompt catalog, verification harness, regression suites, offline-capable fallback library — is a nontrivial maintained artifact in itself.
4. Premature adoption work locks in bad decisions for early users. Ship the language coherently first; add the transpiler when adoption becomes an active concern.

**Pre-build checklist (all must be done before building this):**
- [ ] Kāra 1.0 language spec stable.
- [ ] Stable public AST / semantic IR suitable for both emission directions.
- [ ] LLM verification infrastructure decided (property tests against representative inputs, or formal methods, or reviewed test suites).
- [ ] Impedance-matching cache format and review workflow defined.
- [ ] Offline-capable fallback — library of pre-verified lowering recipes for known divergent patterns — so builds don't require network access to the LLM.

**Cross-reference:** [design.md §1](design.md#1-what-kāra-is) — the "Rust without being Rust" principle this rests on. The transpiler is a separate backend beside LLVM and GPU codegen, not a replacement.

### File-Level Rust / Kāra Coexistence

`.kara` and `.rs` files living side by side in the same project — whether Cargo-rooted or Kāra-rooted. Cross-language module imports resolve to the same shared IR: a Kāra file writes `import rust_module.{func}`, a Rust file writes `use kara_module::func;`, and both see each other's types and functions as native to their own language for the common subset. Shared `Cargo.toml` / equivalent governs both sides; crates.io is the shared registry; existing Cargo-based CI/CD works unchanged.

**Concrete sketch for the Cargo-rooted direction:** a `build.rs` script invokes `karac --emit rust --out-dir ${OUT_DIR}` on all `.kara` files under `src/`. Cargo compiles the emitted Rust alongside the hand-written Rust. From Cargo's perspective, `build.rs` just generates more Rust source — same category of extension as `bindgen` / `prost`. `cargo build`, `cargo test`, `cargo check` all work unchanged.

**For the Kāra-rooted direction:** symmetric — Kāra's build system detects `.rs` files, transpiles them via Rust → Kāra and compiles through the Kāra pipeline. Alternatively, Rust source is compiled directly to Kāra's IR via a Rust frontend (skipping the source-to-source round-trip for speed). Both paths are viable.

**Why it matters for adoption:** a Rust team drops one `.kara` file where Kāra's ergonomics help most; a Kāra team imports Rust crates as native Kāra libraries without an FFI boundary. No parallel ecosystem to bootstrap — the largest cost of launching a new language is zero here.

**What it rests on:**
- `Rust ↔ Kāra Bidirectional Transpiler` (above) — prerequisite; cross-language imports need both directions well-defined.
- `build.rs` integration on the Cargo-rooted side (standard Cargo extension, nothing novel).
- A Rust frontend to Kāra's IR for the Kāra-rooted direction — tractable given the semantic equivalence, but genuinely newer ground than the Cargo side.
- Debugger story — source maps back to `.kara` / `.rs` as a starting point; native multi-language debugger later.
- LSP — thin Kāra LSP over rust-analyzer via the transpile is plausible as bootstrapping; standalone Kāra LSP is the long-term shape.

**Why after v1, not a stdlib / compiler ship:**
1. The bidirectional transpiler must exist and be stable first — coexistence is an integration layer on top of it.
2. Cross-language module imports for *divergent* features depend on LLM-assisted binding shims from the transpiler's impedance-matching tier; those need to be reliable before the coexistence story is defensible.
3. Build-tool integration specifics (`build.rs` vs. standalone `karac` Cargo subcommand vs. a more integrated form) are an open design decision that benefits from real user pressure before committing.

**Pre-build checklist (all must be done before building this):**
- [ ] Bidirectional transpiler (above) shipped and mature in both directions.
- [ ] Stable Kāra IR suitable for consumption by a Rust frontend.
- [ ] Build-tool integration approach decided (`build.rs` generator vs. Cargo subcommand vs. rustc plugin — leaning `build.rs` for pragmatic reasons).
- [ ] Debugger source-map story prototyped.

**Cross-reference:** `Rust ↔ Kāra Bidirectional Transpiler` (above) — hard prerequisite.

### Blast-Radius / Test-Selection Service

A CI-facing tool that answers "what can this diff break, and which tests must re-run" by consuming `karac query affected-by` across a changeset instead of re-running a whole suite. The audience is twofold: monorepo CI (where the payoff is compute) and coding agents (where the payoff is that an agent editing a function currently has no principled way to scope its verification).

**What it rests on:**
- [`karac query affected-by`](#karac-query-affected-by--call-graph-reach-query) — transitive callers / callees / tests for a function or line range. This is the entire analysis; the product is the harness around it.
- [`karac catalog`](#signature-catalog-karac-catalog) — the per-function/type JSONL signature index, for resolving a diff's touched spans to symbols cheaply.
- `--output=jsonl` streaming build events ([design.md §17](design.md#streaming-phase-events)) — so a selective run can report progress incrementally.

**Why after v1, not a compiler ship:**
1. `affected-by` is the compiler feature; what is missing is CI integration, caching across runs, and a diff→symbol resolver — none of which belong in `karac`.
2. Test selection is only trustworthy with a stable test-discovery story; it should not be built while `karac test` conventions are still moving.
3. The value case needs a codebase large enough for selection to beat whole-suite runs. No Kāra codebase is there yet except the self-hosted compiler itself, which is the natural first customer ([Self-Hosting](#self-hosting)).

**Pre-build checklist (all must be done before building this):**
- [ ] Self-hosting shipped — the first codebase big enough to validate the payoff.
- [ ] `karac test` discovery + reporting conventions stable.
- [ ] `affected-by` precision measured against a known-good whole-suite baseline (a selection tool that misses a real dependency is worse than useless).
- [ ] Decision on scope: Kāra-only, or a multi-language shell where Kāra is one provider.

**Cross-reference:** [`karac query affected-by`](#karac-query-affected-by--call-graph-reach-query), [`karac catalog`](#signature-catalog-karac-catalog), [Self-Hosting](#self-hosting) (the first candidate corpus).

### Agent-Facing Package Index

A registry surface that serves compact, typed signature indexes for published packages so an agent can resolve an API without fetching and reading sources. [`karac catalog`](#signature-catalog-karac-catalog) emits exactly the right record shape — one JSONL signature record per function and type — for a package's own tree; this entry is that idea extended across a registry, so "what does this package expose" is one cheap fetch rather than a repository crawl.

**What it rests on:**
- `karac catalog` — the record format and the emitter.
- The registry proxy protocol and its reference implementation. The reference proxy is explicitly *not* a production mirror (no upstream mirroring, caching, auth, signatures, or HA), so a real index is additive work on top of a defined protocol, not a protocol design project.
- A package manager with enough published packages for an index to be worth serving.

**Why after v1, not a compiler ship:**
1. An index over an empty ecosystem indexes nothing. This is gated on package-registry adoption, not on compiler capability.
2. Serving it is hosting + operations (availability, staleness, auth for private indexes), which is outside what `karac` should own.
3. The catalog record format should absorb real agent usage before being frozen as a served API — freezing it early repeats the `database/sql` mistake described elsewhere in this section.

**Pre-build checklist (all must be done before building this):**
- [ ] Package manager + production registry shipped and carrying real packages.
- [ ] `karac catalog` record format stable enough to serve as a public API contract.
- [ ] Private/authenticated index story decided (this is where any revenue would come from, and it changes the auth design).
- [ ] Supply-chain signing decided — see [Stdlib and Ecosystem Security Conventions](#stdlib-and-ecosystem-security-conventions), which commits to Sigstore-or-equivalent and SBOM emission.

**Cross-reference:** [Stdlib and Ecosystem Security Conventions](#stdlib-and-ecosystem-security-conventions) (signing/SBOM intent that an index must honor); [`karac catalog`](#signature-catalog-karac-catalog).

### Machine-Fix-Rate Benchmark and Public Leaderboard

A published measurement of how *repairable* a toolchain is by an LLM that has never seen its diagnostics: write a task blind, run the toolchain's own fix path, check the result against an oracle, and report the rate. `examples/mend/harness/mend_batch.py` already does this for Kāra. This entry is the generalization — the same protocol run across other languages' toolchains, published as a comparison.

**The honesty rule is the hard part, and it is the moat.** The rate is a statistic *only* over fresh, blind authorship. Authoring by anyone who already knows the language will not make the known mistakes and therefore cannot produce a rate — it produces dogfooding. Any public leaderboard inherits this constraint, and most naive attempts at such a benchmark will violate it and publish biased numbers. Publishing the protocol is arguably more valuable than publishing Kāra's own score.

**What it rests on:**
- `examples/mend/harness/mend_batch.py` (live) — the working harness.
- `examples/mend/TASK_FORMAT.md` — the task+oracle format, including the outcome taxonomy that makes the result readable (`fixed-by-karac` + oracle **pass** is the wedge result; `fixed-by-karac` + oracle **FAIL** is the category worth hunting).
- A per-language adapter layer that does not exist: each compared toolchain needs its own "apply the machine-applicable fixes" invocation, and most have no equivalent of `karac fix`.

**Why after v1, not a compiler ship:**
1. Measuring competitors publicly is a positioning act with a credibility cost if the methodology is weak. It should follow 1.0, when the number being defended is stable.
2. Live mode needs an authenticated `claude` CLI (it 401s headless), so the measurement is a periodic developer-environment run, not a CI gate. A public leaderboard needs a reproducible, non-interactive execution story that does not exist today.
3. Cross-language fairness is a research problem: languages differ in what counts as a machine-applicable fix, and an unfair comparison is worse than none.

**Pre-build checklist (all must be done before building this):**
- [ ] Kāra 1.0 shipped.
- [ ] Non-interactive, reproducible harness execution (no authenticated-CLI dependency).
- [ ] Cross-language fairness protocol written and defensible — especially "what counts as a machine-applicable fix" per toolchain.
- [ ] Blind-authorship guarantee mechanized, not conventional (the honesty rule cannot rely on participants policing themselves).

**Cross-reference:** `examples/mend/TASK_FORMAT.md` (the task+oracle format).

## Unscheduled language extensions

Language extensions and library additions that have a design but no track. Each waits for programs that need it.

### Tail-Call Optimization (`#[tailrec]`)

**Decision:** Both verification (compile error if a recursive call is not in tail position) and codegen (LLVM `musttail`) for `#[tailrec]` are deferred together. Keeping both halves together avoids a false promise (verifying tail position without emitting `musttail`).

**Why deferred:**

1. Promotes a backend optimization (`musttail`) to a language contract — couples language semantics to LLVM's codegen guarantees.
2. The primary use case is deeply recursive functional patterns; users who need guaranteed non-overflow can hand-write loops for the ~90% case.
3. No concrete use case beyond deep recursion has emerged that can't be solved by iteration.
4. Verification without codegen gives the programmer a false promise — both should ship together.

**Why non-breaking:** Adding `#[tailrec]` later is purely additive — a new attribute on existing function syntax.

**Design shape:**

- `#[tailrec]` on a function is a compile error if any recursive call to that function is not in tail position.
- The compiler emits LLVM `musttail` on each recursive call, guaranteeing loop-equivalent stack usage.
- Functions without `#[tailrec]` receive no TCO guarantee; LLVM may still optimize opportunistically.
- Mutual tail recursion is not covered — direct self-calls only.
- GPU and `embedded` profiles forbid recursion entirely; `#[tailrec]` is not valid in those profiles.

**Relationship to the reserved `become` keyword.** v1 reserves `become` for guaranteed tail calls ([design.md §3](design.md#reserved-for-future-use)). When this entry is picked up, decide whether the surface is the fn-level `#[tailrec]` attribute specced above, call-site `become f(args)`, or both (attribute = verification scope, `become` = per-site marker); the keyword reservation keeps every option open without a source break.

**Trigger.** No corpus workload is blocked on TCO. A program whose recursion depth is a real constraint, written in accumulator style, is the validation workload.

### List Comprehensions

**Decision:** Python-inspired syntax sugar: `[expr for item in collection if condition]`. Desugars to iterator chain (`.filter().map().collect()`).

**Why deferred:** Iterator chains may be sufficient. Purely a parser desugaring feature.

**Why non-breaking:** New syntax that doesn't conflict with existing expressions.

### Generators (`yield`)

**Decision:** Defer generators. `gen` and `yield` are reserved ([design.md §3](design.md#reserved-for-future-use)). Manual `Iterator` implementations cover v1.

**Why deferred:** Add generators when manual `Iterator` boilerplate becomes friction. Manual `Iterator` impls work correctly without generators. No v1 feature depends on generators. The design is settled: `yield` is pure iteration, orthogonal to `suspends`.

**Why non-breaking:** `yield` is already a reserved keyword. Purely additive desugaring to `Iterator` implementations.

**Design shape:** `yield` desugars to `Iterator` implementations. **Orthogonal to `suspends`:** `yield` is a pure iteration mechanism — it does not produce the `suspends` effect and does not interact with the concurrency runtime. `suspends` is concurrency-only (compiler-inferred from call graph).

### `if let` Chains

**Decision:** Support chained `if let` patterns using `and` in a single condition.

**Why deferred:** Basic `if let` and `let...else` cover the common cases. Chains add parser complexity.

**Why non-breaking:** Purely additive. `if let` without chaining is unaffected.

**Design shape:**

```kara
if let Some(user) = find_user(id)
   and let Some(addr) = user.address
   and addr.country == "US"
{
    process_us_order(user, addr);
}
```

Each `let` binding is in scope for subsequent conditions. A plain `bool` condition may appear anywhere after at least one `let` binding.

### Yielding Subscripts

**Decision:** Keep `Index`/`IndexMut` traits as current subscript mechanism. Revisit yielding subscripts as complement once coroutines exist ([M4a services](#m4a-services)).

**Why deferred:** Requires `subscript` keyword, coroutine mechanism, and separation from streaming iterators. The 95% case (arrays, vecs) is covered by `Index`/`IndexMut`.

**Why non-breaking:** `Index`/`IndexMut` remain valid. Yielding subscripts are additive.

**Design shape:**

```kara
subscript fn index(ref self, idx: i64) -> ref T {
    assert(idx >= 0 and idx < self.len());
    yield self.data[idx]
}

subscript fn index_mut(mut ref self, idx: i64) -> mut ref T {
    assert(idx >= 0 and idx < self.len());
    yield self.data[idx]
    // post-yield cleanup runs after caller releases the reference
}
```

Open questions for coroutines design: (1) `subscript` as keyword vs annotation; (2) replace or coexist with `Index`/`IndexMut`; (3) default implementations in traits.

### Try Blocks (`try { ... }`)

A `try { ... }` block is a block expression whose value is a `Result[T, E]`. Inside the block, the `?` operator short-circuits to **the block's** result rather than to the enclosing function's return:

```kara
fn process(input: String) -> Report {
    let parsed: Result[Parsed, ParseError] = try {
        let tokens = lex(input)?;        // ? routes to the try block's Err arm
        let ast = parse(tokens)?;        // same — block early-exits, function continues
        normalize(ast)?                  // tail expression after ? — wraps in Ok
    };

    match parsed {
        Ok(p)   => render(p),
        Err(e)  => Report.error(e),
    }
}
```

**Surface form.** `try` followed by a block expression. The block is parsed with the same grammar as any other block (`{` STMTS [TAIL_EXPR] `}`). The `try` keyword is reserved in v1 ([design.md §3](design.md#reserved-for-future-use)).

**An expected type is required.** A `try` block needs an expected `Result[T, E]` from context (check mode), and each `?` inside converts into that `E` with `From`.

**Type rule.** A `try { ... }` block has type `Result[T, E]` where:

- `T` is the type of the block's tail expression (the final expression with no trailing semicolon). If the tail is omitted (the block ends in a semicolon or is empty), `T = ()`.
- `E` is unified across every `expr?` site inside the block, exactly as `?` already unifies error types in a function returning `Result[_, E]` ([design.md §10](design.md#results-and-propagation)). Cross-error-type propagation through `From[E1] for E2` impls works inside try blocks the same way it works at the function boundary.

The block's tail value is wrapped: `T` becomes `Ok(T)`. Every `expr?` site that fires becomes the block's `Err(...)`. There is no separate "yield" or "return-from-block" syntax — a value-producing tail and `?`-induced early-exit are the only two paths.

**`?` retargets to the innermost `try`.** Inside a `try` block, `?` short-circuits to the block, not to the enclosing function. A function returning `()` may use `?` inside a `try { ... }` because the propagation target is the block, not the function. Outside any `try` block (or *between* nested `try` blocks at the same lexical level), `?` retargets to the next surrounding return target — either the next outer `try` block or, ultimately, the enclosing function's return type.

**Empty try blocks.** `try { }` evaluates to `Ok(())`. The block's `E` type is metavariable until a use site (a binding's type annotation, a function-return annotation, or downstream type-inference) determines it. An entirely-empty `try` block in a context where `E` cannot be inferred produces the standard "cannot infer type" diagnostic.

**Try blocks without `?`.** A `try` block with no `?` site is legal — it just evaluates to `Ok(tail_expr)` (or `Ok(())` for an empty tail). The compiler does not warn about this case; the construct is sometimes useful as a type-narrowing form during refactor.

**Effect interaction.** `try { ... }` is purely a control-flow construct — the block does not introduce or remove any effect from the enclosing function. The `?` desugaring rule for `From` conversion (which contributes effects from `From::from`'s declared effects to the enclosing function's inferred set, per [design.md §10](design.md#results-and-propagation)) applies inside try blocks the same way; the From-conversion's effects flow to the enclosing function, not to the block's value.

**`defer` / `errdefer` inside a try block.** Same scope rules as any other block. `defer` blocks declared inside a `try` fire on the block's exit (whether via tail expression or via `?` short-circuit). `errdefer` declared inside a `try` block fires only when the block exits via the `Err` path (a `?` short-circuit, or an explicit tail value of an `Err`-typed expression — though the latter shape is rare since `?` is the canonical early-exit mechanism). The `errdefer`/`defer` chain's interaction with the *function's* outer cleanup is unchanged: try blocks introduce their own cleanup scope, exactly as ordinary blocks do.

**Diverging tail.** A `try { panic("..."); }` (no tail expression because the body diverges) has type `Result[Never, E]` for some `E` — the block diverges, but its type is still well-formed because `Never` coerces to any `T`. This case is unusual but consistent with the LUB rule ([design.md §8](design.md#least-upper-bound) — `Never` is the bottom).

**Why try blocks pay for themselves.** Without `try`, the only way to scope `?` to a sub-region of a function is to extract the region into a helper function. That works but introduces a new name, new generic-parameter list, new effect-set declaration, and new call site. Try blocks let the same scoping land inline — useful for early-validation patterns (`let validated: Result[Config, ConfigError] = try { ... };` followed by a `match`) and for `?` chains whose error-type stack does not match the enclosing function's signature.

<a id="try-blocks--retargeting-and-error-type-unification"></a>
#### Implementing try blocks

**Why deferred:** The typechecker work touches three machineries that interact in non-trivial ways: (a) the `?`-target stack, which currently has only one frame (the enclosing function's return type); try blocks add per-block frames that nest. (b) The error-type unification pass that runs at function-return time needs a per-block variant. (c) The From-chain coercion that already runs at `?` sites needs a per-block error-type target rather than the function's return type. Each piece is small individually; the integration testing surface is large enough that shipping it is unmotivated while the workaround (extract a helper function returning `Result[T, E]`) is mechanical.

**Why non-breaking:** Purely additive. `try` is reserved, and a helper function returning `Result[T, E]` keeps working; users can later replace the helper with an inline `try` block without changing semantics.

The implementation covers:

1. **`?`-target stack.** Currently the typechecker's `?`-resolution looks up the enclosing function's return type. Add a stack of `try`-block return targets; `?` resolves to the innermost frame. The function-return frame is the bottom of the stack and remains the fallback.
2. **Per-block error-type unification.** Each `try` block has its own `E` metavariable, unified across all `?` sites inside the block (using the same algorithm the typechecker already uses for the function-level case). The From-chain coercion machinery of `?` ([design.md §10](design.md#results-and-propagation)) applies inside try blocks the same way it applies at the function boundary; the From conversion's effects flow to the enclosing function's inferred set, not to the block's value.
3. **Block-level `T`/`E` inference.** The block's `T` is the type of its tail expression (or `()` for an empty / no-tail block); the block's `E` is the unified error type. Both flow into the enclosing context's type inference normally — an annotated binding `let r: Result[Foo, MyError] = try { ... };` constrains `T = Foo` and `E = MyError`; an unannotated binding solves both via downstream uses.
4. **Empty try block.** `try { }` has type `Result[(), E]` where `E` is a metavariable that downstream context must solve; if no context is available, the existing "cannot infer type" diagnostic fires.
5. **Diverging tail.** A `try` block whose tail diverges (`panic(...)`, `return`, `loop { }` with no break) has type `Result[Never, E]` — the block diverges, but its type is still well-formed because `Never` coerces to any `T`. This is consistent with the LUB rule (`Never` is the bottom).
6. **`defer` / `errdefer` integration.** A `try` block introduces its own cleanup scope, exactly as ordinary blocks do. `defer` declared inside a `try` fires on the block's exit (whether tail or `?`-short-circuit). `errdefer` fires on the `Err` exit path only. The function-level `defer`/`errdefer` chain is unaffected — try blocks nest cleanly with the existing scope rules.
7. **Effect interaction.** `try { ... }` itself contributes no effects to the enclosing function — it is a control-flow construct, not an operation. Effects from the block's body (operations, From conversions at `?` sites) flow to the enclosing function's inferred set as they would in any block.
8. **Diagnostic shape — `?`-target ambiguity.** When a `?` site's error-type does not unify against the enclosing `try` block's `E` (and no `From` conversion exists), the diagnostic must name *which* return target the `?` is resolving to (the innermost `try` block, or the enclosing function) so users can see whether they need to fix the `From` impl, fix the block's expected error type, or restructure the nesting. The existing "no `From` impl" diagnostic gets a context line: `(propagating to the try block at <span>)`.
9. **Closure-boundary rule.** A `?` inside a closure body never targets a `try` block in the *enclosing* lexical scope — closures are a control-flow boundary the same way they are for `break label` ([design.md §6](design.md#6-expressions-and-statements)). A `?` inside a closure resolves to the closure's own return type, which must itself be `Result[_, _]` or `Option[_]`.

Test coverage: positive — `try { lex(s)? }` evaluates to `Ok(...)` on success; the same with `Err` on a failing `?`; nested try blocks short-circuit to the innermost one; From-chain conversion across try-block `?` sites works the same as across function `?` sites; empty try block infers correctly given binding context; `defer` inside a `try` block fires on the block's exit; `errdefer` inside a `try` block fires on `Err` exit only; tail expression of `Never` type still produces a well-formed `Result[Never, E]`. Negative — `?` inside a closure body inside a `try` block does not target the outer `try` (closure-boundary rule); `?` site whose error type does not unify with the block's `E` produces the diagnostic with the named try-block context line; an unannotated empty try block in a context without downstream constraints produces the standard "cannot infer type" diagnostic.

### Trait Aliases

A trait alias names a `+`-separated set of trait bounds and exposes the set under a single identifier:

```kara
trait IntegerOps = Add + Sub + Mul + Div + Eq + Ord;

trait Sortable[T] = Iterator[Item = T] + ExactSizeIterator;

pub trait Numeric = Copy + Add + Sub + Mul + Div + PartialOrd;
```

The alias is a *name for a bound list*, not a new trait. The compiler's surface contract is exactly: every place a programmer writes the alias name as a bound, the compiler textually substitutes the alias's bound list. There is no new vtable, no new method-resolution candidate set, no new coherence object — the alias is fully expanded before trait resolution sees it.

**Surface forms.**

```kara
// Declaration — at the same scope as `trait` definitions
trait Numeric = Copy + Add + Sub + Mul + Div;

// As a bound on a generic parameter
fn average[T: Numeric](xs: ref Slice[T]) -> T { ... }

// As a `where`-clause predicate
fn matmul[A, B, C](a: A, b: B) -> C
    where A: Numeric, B: Numeric, C: Numeric { ... }

// As a `dyn` trait object (with the dyn track)
let drawables: Vec[ref dyn Drawable] = ...;  // when `Drawable = Display + Debug`
```

**What an alias *cannot* be.**

- **Not implementable.** `impl Numeric for f64 { ... }` is a compile error — `error[E_IMPL_TRAIT_ALIAS]: cannot implement trait alias 'Numeric'; implement each component trait separately`. The diagnostic enumerates the alias's expansion (`Copy`, `Add`, `Sub`, `Mul`, `Div`) so the programmer sees exactly which impls are required.
- **Not nameable as the receiver of method-call syntax** unrelated to the bound role. The alias is a bound, not a value type — `Numeric.method(...)` is the same diagnostic as for a non-alias trait used in value position.
- **Not recursive.** `trait A = B; trait B = A;` produces `error[E_TRAIT_ALIAS_CYCLE]` listing the cycle. The compiler builds an SCC of the trait-alias dependency graph at type-check time and rejects any non-trivial SCC. Self-reference (`trait A = A + Foo;`) is the same error.
- **Not a way to add methods.** An alias `trait Foo = Bar + Baz` exposes only the methods `Bar` and `Baz` declare. Adding methods requires writing a real `trait` block.

**Composition.** Aliases compose freely. Given `trait A = X + Y;` and `trait B = A + Z;`, the bound `T: B` expands to `T: X + Y + Z`. The expansion is fully transparent — the compiler computes the *transitive flattened bound set* once at alias-resolution time and uses that wherever the alias appears.

**Generics.** A trait alias may take type and effect parameters in the same `[...]` form used by trait definitions. Generic parameters of the alias may appear inside the bound list:

```kara
trait IteratorOver[T] = Iterator[Item = T];

fn collect_into[T, I: IteratorOver[T]](iter: I) -> Vec[T] { ... }
```

Constraint propagation is uniform: a use site supplies the alias's generic args, the compiler substitutes them into the bound list, and the resulting bounds are checked normally.

**Visibility.** Trait aliases follow the standard visibility rules ([design.md §4](design.md#visibility)). The visibility applies to the alias name itself; the bound list's component traits are referenced by their own visibility (an alias whose body names a non-`pub` trait can only be used in scopes where every component trait is also visible — same as any bound list).

**`where`-clause aliases.** Trait-alias bodies may include `where` clauses on the alias's own generic parameters:

```kara
trait OrderedFloat[T] = Ord where T: Numeric + Bounded;
```

The `where` clause restricts which `T` arguments the alias accepts. At a use site `[U: OrderedFloat[i64]]`, the compiler verifies `i64: Numeric + Bounded` (the `where`) and then enforces `U: Ord` (the body).

**Effect bounds inside a trait alias body.** Effect-set predicates are *not* part of trait bounds — Kāra's effect system is orthogonal. A trait alias may not list effect predicates in its body (`trait Foo = Iterator + writes(Db);` is rejected with `error[E_EFFECT_IN_TRAIT_ALIAS]: effect predicates do not belong in a trait alias body; use an effect group declaration`). Effect groups ([Effect Groups and Composition](#effect-groups-and-composition)) are the parallel mechanism for naming effect-set unions.

<a id="trait-aliases--expansion-trait-numeric--copy--add--sub--"></a>
#### Implementing trait aliases

**Why deferred:** Substituting an alias's bound list at every use site, computing transitive flattened bound sets across nested aliases, propagating generic arguments, enforcing the alias's `where` clause, rejecting impl-of-alias, and detecting cycles is *implementation* work that touches the trait resolver, the bound-checking machinery, and the diagnostic surface.

**Why non-breaking:** Purely additive. Code that writes the bound list explicitly keeps compiling once aliases exist.

The implementation covers:

1. **Resolver — alias-reference recording.** When the resolver hits a trait reference at a use site, it consults the trait registry. If the resolved name is a trait alias, the resolver emits an `AliasReference { alias: AliasId, generic_args: Vec<TypeArg> }` placeholder rather than a concrete `TraitRef`.
2. **Bound expansion.** A new pass between resolution and bound-checking walks every `AliasReference`, looks up the alias's stored bound list, substitutes the alias's generic parameters with the call-site's arguments, and produces a `Vec<TraitRef>` (the flattened expansion). Nested aliases recurse; the SCC check from below catches cycles before the recursion explodes.
3. **Cycle detection.** At alias-registration time (just after resolver), the compiler builds a directed graph of "alias `A` mentions alias `B`" edges and runs Tarjan's SCC. Any non-trivial SCC is `error[E_TRAIT_ALIAS_CYCLE]` listing the cycle path. Self-edges (a trait alias whose body mentions itself) are the same error.
4. **Where-clause-on-alias propagation.** When the alias body carries a `where` clause, the alias's expansion at a use site is gated on the `where` clause's predicates. Concretely: the use site `[U: OrderedFloat[i64]]` first verifies `i64: Numeric + Bounded` (the alias's `where`) and then enforces `U: Ord` (the body). Failure on the `where` clause produces a focused diagnostic that names the alias *and* the failing predicate, distinguishing it from ordinary bound-resolution failures.
5. **Impl-rejection.** When the typechecker encounters an `impl AliasName for T { ... }`, it produces `error[E_IMPL_TRAIT_ALIAS]: cannot implement trait alias 'AliasName'; implement each component trait separately`. The diagnostic enumerates the alias's expansion (e.g., `Copy`, `Add`, `Sub`, `Mul`, `Div`) so the programmer sees exactly which impls are required.
6. **`dyn` handling** (with the [dyn track](#dyn)). `dyn AliasName` is accepted as a `dyn` trait object iff the alias's expansion contains exactly one trait that produces a vtable (marker traits with no methods are vtable-free; one method-bearing trait is the canonical case). `dyn AliasName` where the expansion has zero or two-plus method-bearing traits is rejected with `error[E_DYN_REQUIRES_SINGLE_METHOD_TRAIT]` listing the alias's expansion. (This rule mirrors Rust's `dyn` object-safety constraints; the alias does not loosen them.)
7. **`where`-clause use-site.** `where T: AliasName` desugars to the alias's expanded bound list. Generic arguments propagate normally.
8. **Effect-bound rejection.** Trait alias bodies cannot list effect predicates (`trait Foo = Iterator + writes(Db);` is rejected with `error[E_EFFECT_IN_TRAIT_ALIAS]: effect predicates do not belong in a trait alias body; use an effect group declaration`). Effect groups ([Effect Groups and Composition](#effect-groups-and-composition)) are the parallel mechanism for naming effect-set unions.
9. **Diagnostic shape — surfacing the alias name in errors.** When a use-site bound fails because of the *alias's* expansion (not the underlying trait directly), the diagnostic should name the alias *and* the offending component: `the trait \`Add\` is not implemented for \`T\` (required by the trait alias \`Numeric\` at <span>)`. The alias declaration's source span is reachable through the typechecker's symbol table.
10. **Re-exports across packages.** A `pub trait Foo = ...;` is re-exportable through `pub import` exactly like a regular trait. The alias's expansion is computed at the consumer's compile time using the consumer's view of every component trait — there is no compile-time materialisation that crosses package boundaries (the alias is fully expanded at each use site).

Test coverage: grammar; positive expansion at every position (bound, `where`-clause, `dyn`); nested alias chains expand correctly with generic-arg propagation; impl-of-alias rejection with enumeration of components; cycle detection on direct and indirect cycles; `where`-clause-on-alias gating; effect-bound rejection diagnostic; `dyn` accepted on single-method-trait expansions, rejected on zero / multi-method-trait expansions; cross-package re-exports compile and resolve correctly.

### Generic Associated Types (GATs)

An associated type may itself take type parameters — `type Mapped[U]` — making it a type-level function from those parameters to a concrete output. This is the well-known **generic associated type** (GAT) feature, with the same surface syntax as a non-generic associated type ([design.md §9](design.md#associated-types)), just with `[...]` parameters appended.

```kara
trait Functor {
    type Mapped[U];
    fn map[U, with E](self, f: Fn(Self.Item) -> U with E) -> Self.Mapped[U] with E;
}

impl Functor for Vec[T] {
    type Item = T;
    type Mapped[U] = Vec[U];
    fn map[U, with E](self, f: Fn(T) -> U with E) -> Vec[U] with E { ... }
}
```

**Declaration syntax.** `type Name[P1, P2, ...]` inside a trait body declares a GAT. The parameters use the same generic-parameter syntax as `fn` and `struct` declarations; bounds (`type Mapped[U]: Trait`) and `where` clauses are permitted on the declaration. Effect-polymorphic GATs (`type Mapped[U, with E]`) are not part of this design — GATs are over types only. (When effect polymorphism on associated outputs is needed, the carrying *method* takes the `with E` parameter, not the GAT.)

**Binding syntax.** An impl binds the GAT with the same parameter list it was declared with: `type Mapped[U] = Vec[U]`. The right-hand side may reference the impl's own type parameters and the GAT's own parameters interchangeably.

**Projection syntax.** Generic associated types are projected with `T.Assoc[X1, X2, ...]` — the same dot-projection used for non-generic associated types, with the type arguments supplied in `[...]`:

```kara
fn double_each[F: Functor](functor: F) -> F.Mapped[i64]
where F.Item = i64
{
    functor.map(|x| x * 2)
}
```

In type position, `F.Mapped[i64]` reads as "the `Mapped` GAT of trait `Functor` (resolved by the bound) projected at `U = i64`". As with non-generic projections, this avoids a redundant generic parameter on the call site.

**Bounds and `where` constraints.** GAT projections may appear in `where` clauses on either side of a `:` or `=`:

```kara
fn collect_doubled[F: Functor](f: F) -> F.Mapped[i64]
where
    F.Item = i64,
    F.Mapped[i64]: FromIterator[i64]
{ ... }
```

Bounds attached to the GAT declaration itself (`type Mapped[U]: Trait`) are enforced at every impl site for every legal `U`.

**One impl per trait, still.** The "one impl per trait per type" coherence rule from non-generic associated types extends unchanged: a type implements `Functor` once, picking one definition for `Mapped[U]` parameterised over `U`. Two impls with different `Mapped` definitions are a coherence error.

**Out of scope.**

- *No higher-ranked binders.* Kāra has no `for<...>` syntax, so bounds like `for<X> T.Mapped[X]: CrossTask` cannot be written. If a real use case lands, the binder syntax will be designed alongside it.
- *No GATs over borrow regions.* Kāra has no explicit lifetime parameters; the LendingIterator pattern (`type Item[a]` projected at the receiver borrow) is expressed through borrow-elision on the carrying method, not through a borrow-parameterised GAT. An impl returning a borrow uses `type Item = ref T` and the method's elision rule ties the borrow to the receiver.
- *No effect-parameterised GATs.* Effect polymorphism rides on methods (`fn map[U, with E]`), not on the associated output type itself.

<a id="type-alias-impl-trait-tait--witness-inference-and-opaque-surface"></a>
### Type Alias `impl Trait` (TAIT)

> v1 has `impl Trait` in argument and return position and in trait methods ([design.md §9](design.md#9-traits)). This entry adds a named existential.

A type alias may name a return-position existential, fixing it once and re-using the name:

```kara
type LineIter = impl Iterator[Item = String] with reads(FileSystem) panics;

fn iter_lines(path: String) -> LineIter with allocates(Heap) {
    File.open(path)?.lines()
}

fn iter_csv(path: String) -> LineIter with allocates(Heap) {
    File.open(path)?.lines()    // must produce the SAME concrete type as iter_lines
}
```

**The defining package fixes the concrete type.** Within the package that declares the type alias, every function returning the TAIT must produce the *same* concrete type — the alias is one existential, not one-per-function. The compiler enforces this at the package boundary: if `iter_lines` returns `LinesIter[File]` and `iter_csv` returns `CsvLinesIter[File]`, both functions are flagged with a "TAIT concrete-type mismatch" diagnostic naming both sites and both concrete types.

**Cross-package consumers see only the alias.** From outside the defining package, `LineIter` is opaque — consumers can only call `Iterator` methods on it (under the declared `with reads(FileSystem) panics` ceiling).

The capture set of a TAIT is determined by the type alias's own type parameters (TAITs may be generic: `type IterOf[T] = impl Iterator[Item = T]`); no implicit capture of the *defining function's* generic parameters or borrow regions, since those are not in scope at the alias's declaration site.

**Composition with TAIT.** A type-alias `impl Trait` (`type LineIter = impl Iterator[Item = String]`) is a separate alias kind whose body is an existential type, not a transparent rename. Bounds on the alias's generic parameters apply uniformly: `type IterOf[T: Display] = impl Iterator[Item = T]` requires `T: Display` at every use site that names `IterOf[U]`. The witness-inference rules at the defining-package boundary (the alias's body must produce a single concrete witness type) do not interact with the bound rule — they are separate checks.

#### Implementing TAIT

**Why deferred.** The full TAIT machinery requires four interlocking pieces:

1. **Defining-use inference.** Walk every function in the defining package whose return type names the TAIT, infer the concrete return type from the body, and pin it as the alias's witness. Multiple defining-use sites must produce the same witness — otherwise `error[E_TAIT_CONCRETE_MISMATCH]` naming both sites and both witnesses.
2. **Same-concrete-type enforcement.** Run after typecheck of every function in the defining package; aggregate the candidate witnesses and reject any disagreement. The check is package-boundary-relative because the witness is a package-private fact.
3. **Opaque cross-package surface.** Cross-package consumers see the trait bound, never the witness — even though the witness is computable inside the defining package. The compiler's symbol export marks TAIT names as opaque to downstream consumers; the downstream consumer's type-checker treats the alias as if it were `impl Trait` (an existential, not a name for a concrete type).
4. **Generic TAITs** (`type Iter[T] = impl Iterator[Item = T];`). The witness is parametric over the alias's type parameters; the `Iter[i32]` and `Iter[String]` instantiations have independent witnesses; same-concrete-type enforcement is per-instantiation.

The four pieces are tightly coupled — shipping any subset alone produces either a hole in the encapsulation guarantee (cross-package consumers seeing the witness) or a hole in soundness (witness inference without the same-concrete-type check could allow inconsistent dispatch). All four land together.

**Why non-breaking:** Purely additive. A function that names its return type, or returns `impl Trait` without a shared alias, keeps compiling.

The implementation covers:

1. **Resolver — TAIT-reference recording.** Every type-position reference to a TAIT name is recorded as `TaitReference { tait: TaitId, generic_args: Vec<TypeArg> }`. The resolver does not yet substitute the witness — it just marks the reference for the witness-inference pass.
2. **Witness-inference pass.** A new typechecker pass (between ordinary typechecking and effect checking) walks every function in the defining package whose return type contains a `TaitReference`. For each defining-use site, infer the return-expression's concrete type. Aggregate the candidate witnesses per `(TaitId, generic_args)`; if all agree, pin the witness; if not, emit `E_TAIT_CONCRETE_MISMATCH` naming both sites and both witnesses. The witness is stored in the package's TAIT-witness table.
3. **Use-site resolution.** Inside the defining package, after the witness-inference pass has run, every TAIT reference is resolved to its witness type for the purpose of inherent-method resolution (the witness's methods are reachable through the alias name *only* when the use site is in the same package). Method calls on a TAIT value through methods *not* on the trait require resolution through the witness. Use sites in other packages always go through the trait surface.
4. **Cross-package opacity.** When the compiler's symbol export pass writes the package's metadata, TAIT names export their *trait bound* (and any generic params), not their witness. A consumer's resolver and typechecker reading this metadata see the alias as a fresh `impl Trait` existential. No witness leakage.
5. **Generic TAIT instantiation.** `type Iter[T] = impl Iterator[Item = T];` — the witness inference runs per `(TaitId, generic_args)` tuple; `Iter[i32]` and `Iter[String]` are independent witnesses. The same-concrete-type check is per-instantiation.
6. **Capture-set rule.** The TAIT's capture set is determined by the type alias's own type parameters. There is no implicit capture of the *defining function's* generic parameters or borrow regions — those are not in scope at the alias's declaration site.
7. **Diagnostic shape — surfacing the alias name in errors.** When a use site fails because the inferred witness does not satisfy a bound the use site requires (e.g., the witness lacks a method the consumer is calling), the diagnostic should name the alias *and* the inferred witness: `the inferred witness type \`SomeIter[i32]\` for TAIT \`Iter\` does not implement \`ExactSizeIterator\`` etc. The alias declaration's source span is reachable through the typechecker's symbol table.
8. **Re-exports.** A `pub type Foo = impl Trait;` re-exported via `pub import` exposes only the trait bound to the re-exporter's downstream consumers. The witness remains private to the original defining package.

Test coverage: positive — two defining-use sites returning the same concrete type compile cleanly with the alias; method calls *through the trait* work in any package; method calls *through the witness* work only in the defining package. Negative — two defining-use sites returning different concrete types produce `E_TAIT_CONCRETE_MISMATCH`; cross-package consumer trying to use a non-trait method on a TAIT value gets the trait-surface-only diagnostic; generic TAIT with two instantiations producing inconsistent witnesses (each instantiation independently checked); re-exported TAIT remains opaque to the second-hop consumer.

### Declared Variance Markers

> v1 fixes variance by position and, for built-in types, by the specification ([design.md §8](design.md#subtyping-and-variance)). This entry adds marker syntax for declaring it.

**Per-type variance — declared by the stdlib type.** Nominal parametric types declare each parameter's variance with an explicit marker prefix in the generic-parameter list. Three markers:

| Marker | Variance | Reading |
|---|---|---|
| `+T` | covariant | `Foo[Sub]` is a subtype of `Foo[Super]` when `Sub <: Super` |
| `-T` | contravariant | `Foo[Super]` is a subtype of `Foo[Sub]` when `Sub <: Super` |
| `=T` (or no marker) | invariant | `Foo[A]` and `Foo[B]` are unrelated unless `A == B` |

The default — no marker — is **invariant**, the conservative choice. A stdlib type author opts into co- or contravariance by adding `+` or `-` explicitly. The compiler verifies the declaration at the type-decl site: `+T` requires the parameter to appear only in covariant positions inside the type's structure; `-T` requires only contravariant positions; violations are rejected with `error[E_VARIANCE_DECLARATION_INCONSISTENT]: parameter 'T' declared '+T' (covariant) appears in invariant/contravariant position '<field>'`. The verification reuses the position-based rules of [design.md §8](design.md#subtyping-and-variance).

**Stdlib audit — per-type variance pinned.** Every parametric stdlib type carries an explicit variance declaration on each parameter:

| Type | Declaration | Notes |
|---|---|---|
| `Vec[=T]` | invariant | Mutable; `mut ref` field access in `push`/`insert`/`extend` forces invariance. |
| `Slice[=T]` | invariant | `mut Slice[T]` exists; the type-name is shared between read-only and mut variants. |
| `Array[=T, const N]` | invariant in T | Mutable through `mut ref`. The const param is not a type, no variance applies. |
| `String` | n/a | Not parametric. |
| `Map[=K, =V]` | both invariant | Mutable. |
| `Set[=T]` | invariant | Mutable. |
| `VecDeque[=T]` | invariant | Mutable. |
| `SortedSet[=T]` | invariant | Mutable. |
| `Option[+T]` | covariant in T | `Option[Sub] <: Option[Super]`. The type is read-only at the surface; pattern-matching extracts a `T` (or nothing). |
| `Result[+T, +E]` | covariant in both | Read-only enum; both arms produce values, never consume them through the `Result` type. |
| `Iterator[+T]` | covariant in T | Produces `T`s; never consumes a `T` through the `Iterator` interface. `Iterator[Positive]` widens to `Iterator[i32]` for free. |
| `Fn(-T1, -T2, ..., -Tn) -> +U with +E` | contra in args, co in return + effects | The function-type form has its variance fixed by the position-based rules (function argument contravariance, return covariance, effect-set covariance); the `-` / `+` markers in the syntax are descriptive — they reflect the position-based rule, not an additional declaration. |
| `Sender[=T]` / `Receiver[=T]` | invariant | Both directions of a channel; `send` consumes `T`, `recv` produces `T`. |
| `Atomic[=T]` / `Mutex[=T]` | invariant | Interior mutability — the soundness-critical case where auto-inference would silently produce a wrong answer. Explicit `=T` is the right declaration. |
| `MaybeUninit[=T]` | invariant | Holds a `T`-shaped slot; `assume_init` produces a `T`; the slot is morally a `mut ref T` location. |
| `PhantomData[=T]` | invariant by default | A future stdlib type may opt into covariance via `+T` if a use case appears (e.g., `PhantomData[+T]` for type-level markers); it defaults to invariant. |
| `TaskHandle[+T]` | covariant in T | Read-only through `.join() -> T`; consuming the handle yields a `T`. |

The audit is part of the design. Adding a new stdlib parametric type without a variance declaration is rejected by the stdlib lint suite (every parametric stdlib type must carry explicit variance on every parameter — invariance via `=T` or no-marker is fine; the lint rejects ambiguity, not invariance).

**User types stay invariant at first.** A user-declared parametric type is invariant in every parameter, and the `+T` / `-T` markers are stdlib-only. User-side variance opens corner cases (interior mutability through trait impls, conditional-variance patterns, generic-bound variance) that the verification pass needs to handle correctly; the user-side surface lifts when the verifier has been stress-tested against the stdlib audit.

### Higher-Kinded Polymorphism and Phantom Variance

Higher-kinded type parameters (abstracting over type constructors — the `* -> *` class) and explicit phantom variance markers are deferred with no committed design. The single-kinded type system plus monomorphized generics covers the v1 expressiveness range; higher-kinded abstraction is a research-grade extension if real Kāra code accumulates pressure for it.

**Cross-reference:** [design.md §8](design.md#subtyping-and-variance).

### Units of Measure

F#-style dimensional analysis at the type level: `Meters`, `Seconds`, `Newtons`, etc. as phantom type parameters, with compiler-enforced dimensional correctness (`Meters / Seconds` is `MetersPerSecond`; `Meters + Seconds` is a type error).

```kara
// Hypothetical syntax — not committed
type Meters   = f64 tagged Meters
type Seconds  = f64 tagged Seconds
type MetersPerSecond = f64 tagged Div[Meters, Seconds]

let distance: Meters  = 10.0<m>
let time: Seconds     = 2.0<s>
let speed             = distance / time   // : MetersPerSecond, inferred
let wrong             = distance + time   // compile error: Meters ≠ Seconds
```

**Status: explicitly deferred (not absent).** Units-of-measure checking is a well-understood, high-value feature for scientific computing and embedded control systems (NASA Mars Climate Orbiter, medical device dosing errors, avionics unit bugs are canonical examples of what static dimensional analysis prevents). It is deferred — not rejected — because:

1. **Type system prerequisite.** F#-style units require phantom type parameters or a dedicated dimension-kinded parameter that participates in type inference. Kāra's v1 generic system does not have dimension-kinded parameters. Adding them is a significant type-system extension, not a library concern.
2. **Syntax is unsettled.** `10.0<m>`, `10.0[m]`, `10.0 m`, and `@meters(10.0)` are all plausible; the right choice depends on how the literal suffix system and generics interact.
3. **Not a post-v1 breaking change.** Unit types introduced later do not need to affect existing code — a `Meters` tagged type can be introduced as a new stdlib type without breaking any programs that use plain `f64`.

**Revisit trigger:** `comptime` ([Comptime](#comptime)) stabilizes and at least one scientific-computing library author files a concrete use case with a proposed syntax.

### Homogeneous Varargs

Variable-length parameter lists where every argument has the same type: `fn sum(nums: ...i64) -> i64`, called as `sum(1, 2, 3)` or `sum()`. Inside the function, `nums` is received as either a slice (`Slice[i64]`, zero-allocation) or an owned `Vec[i64]` — design choice. Distinct from *Heterogeneous Varargs* (below), which allows each argument to have a different type tracked at the type level.

**Motivating use cases:** builder-style APIs (`query.where_in("id", 1, 2, 3)`), N-ary constructors (when combined with the `Call` trait, enabling `Set(1, 2, 3)`), and generic helpers that accept "any number of Ts" without forcing callers to wrap arguments in `[...]` or `Vec.from([...])`.

**Not needed for:** `println`/`format`-style functions. Kāra's f-strings (`f"hello {x} {y}"`) already cover that use case more ergonomically than varargs would — one argument, first-class interpolation, no runtime format-string parsing.

**Design questions to settle:**

1. **Received type.** Slice (`Slice[T]`) is zero-allocation but read-only; `Vec[T]` is owned but forces heap allocation on every call (and would contribute `allocates(Heap)` to the caller's inferred effects even for three-element calls). Slice is probably the right default, with opt-in `Vec` via a trailing `.collect()` inside the body.
2. **Position restriction.** Almost certainly last-parameter-only; anywhere-in-the-signature varargs creates genuine ambiguity with default-valued parameters (which Kāra has — [design.md §7](design.md#named-and-default-parameters)).
3. **Zero-arg calls.** `sum()` — allowed (empty slice) or compile error? Allowed is simpler and matches Go/Java.
4. **Interaction with default parameter values.** A signature like `fn f(x: i64 = 0, ...rest: i64)` needs clear rules for which positional args go where.

**Why deferred:** Kāra's f-strings and array-literal coercion absorb most practical varargs pressure. The remaining use cases (builder APIs, N-ary constructors) are nice-to-haves. Revisit once concrete examples from real Kāra code accumulate — if the pattern keeps appearing with `[...]`-wrapped args, that's the signal.

**Why non-breaking:** Purely additive. New `...T` parameter-declaration syntax; existing parameter declarations unchanged. Call sites `f(1, 2, 3)` remain well-defined against fixed-arity signatures.

### Heterogeneous Varargs / Variadic Generics

Type-level variable-length parameter lists where each argument can have a different type, tracked statically. Syntax sketch: `fn row[Ts...](values: Ts...) -> Row[Ts...]`. This is the type-system-heavy cousin of *Homogeneous Varargs* — much more powerful but requires comptime infrastructure ([Comptime](#comptime)).

**Motivating use cases:**

- Generic `zip`, `map_all`, and similar N-ary combinators across collections of different element types — no more fixed-arity overload explosion.
- Typed heterogeneous tuples for ORM-style row types (`Row[String, i64, bool]`).
- Multi-arg `Call` sugar — `Set(1, 2, 3)` via a variadic `impl Call[Ts..., Set[T]]` where all `Ts` unify to a common bound.

**Why deferred:** No committed design. Const generics are v1 ([design.md §8](design.md#const-generic-parameters)), so the compile-time-value half of the type system is already settled; the remaining unknown is how comptime / type reflection shape the generic-list machinery once user code can synthesize types. Variadic generics is genuinely hard — every mainstream language that has it (C++ parameter packs, Scala HList, Haskell type-level lists) ended up with a heavyweight design. Kāra should have a clear picture of its comptime model before committing to a shape here.

**Promotion gate.** Do **not** design now. Promote only when *use cases beyond fixed-arity overload sets materialize* in real Kāra code. The named criterion is deliberately narrow because a fixed-arity pattern (an overload set covering tuples up to length 8) is *already* enough for >95% of "N collections at once" needs in practice — promoting on the strength of that pattern alone would be a heavyweight design move serving a problem the workaround already solves. The gate fires when *all three* of the following are observable in committed Kāra code (stdlib or external crates with broad usage), not just hypothetical:
1. **Recurring user code that hits the arity cap.** Concrete `zip_10`, ad-hoc tuple-of-9 patterns showing up across multiple unrelated projects — not one specialised library.
2. **Non-tuple shapes.** Use cases that genuinely need *type-level* heterogeneity beyond what a fixed-arity overload set can express: ORM row-type families, typed message schemas, builder APIs whose argument types depend on prior arguments. If everything reduces to "N parallel iterators of homogeneous element type each," homogeneous varargs (the entry above) is the better promotion.
3. **A workable interaction story with the comptime model.** Heterogeneous varargs and `comptime fn` ([Comptime](#comptime)) overlap at the type-synthesis layer; promotion presupposes the comptime substrates have shipped and the design can express variadic generics *as* a comptime/type-reflection pattern rather than a parallel mechanism. Promoting before comptime ships risks committing to a shape that comptime later subsumes or contradicts.

If only criterion (1) fires and (2)/(3) do not, the right answer is to extend the fixed-arity overload set (e.g., raise a cap from 8 to 12) — not to ship variadic generics. If (2) fires under (3) without (1), document the use cases and revisit at the next edition gate; isolated demand is not enough to justify the design cost.

**Why non-breaking:** Purely additive. New type-level syntax on generic parameter lists; existing generics (`[T]`, `[T: Ord]`, `[T, U]`) unchanged.

**Cross-reference:** **User-Defined Callable Types (`Call` trait)** — below. `Call` + heterogeneous varargs is what unlocks Python-style `Vec(1, 2, 3)`; `Call` without varargs only delivers single-argument sugar.

### Call-Site Spread

Expand an existing collection into positional arguments at a call site. Sketch (syntax TBD): `let xs = [1, 2, 3]; f(...xs)` where `f: fn(i64, i64, i64) -> T` or `f: fn(...nums: i64) -> T`. Dual of varargs on the caller side; orthogonal to both varargs flavors above — works with fixed-arity signatures, homogeneous varargs, or (eventually) heterogeneous varargs.

**Design questions to settle:**

1. **Syntax choice.** `...xs` (JS/TS), `*xs` (Python — collides with dereference in Rust-family), `xs: _*` (Scala). Must not collide with Kāra's existing `..` and `..=` range syntax or the parameter-declaration form from *Homogeneous Varargs*. Leading `...` on an expression is probably safe.
2. **Arity checking.** Reject at compile time when the collection's length doesn't match the target arity (possible for `Array[T, N]` with statically-known length; impossible for `Vec[T]` / slices). Runtime check otherwise.
3. **Position.** Trailing only, or anywhere in the argument list? Trailing is simpler; anywhere enables `f(a, ...middle, z)`.
4. **Mixed with named/default args.** Interaction with Kāra's default parameters needs explicit rules.

**Why deferred:** Niche ergonomic convenience. Workarounds exist today (`f(xs[0], xs[1], xs[2])` for known arity, or redesigning `f` to accept a slice). Not blocked on varargs — can ship independently if the need arises.

**Why non-breaking:** Purely additive. New expression-position syntax (`...expr`); no existing parse rule uses a prefix `...` at the expression level.

### User-Defined Callable Types (`Call` trait / `apply`)

Allow user types to be invoked with parens-call syntax by implementing a callable trait, paralleling Scala `apply`, Kotlin `invoke`, Python `__call__`, and Swift `callAsFunction`. The natural shape:

```kara
trait Call[Args, Output] {
    fn call(ref self, args: Args) -> Output;
}
```

Call-site sugar: `t(x, y)` desugars to `t.call((x, y))` whenever `t: impl Call[(A, B), _]`. Closures already implement this family implicitly; the feature would simply unseal it for user types and unify the closure-vs-user-callable distinction into one trait.

**Motivating use cases:** memoized functions, interpolation tables, parser combinators, validators, and DSLs that want a function-like surface without naming the type at every call site. A secondary (smaller) payoff is sugar for conversions like `Set(words)` in place of `Set.from(words)` — but only for single-argument cases without variadic generics.

**Key design decisions to settle before implementation:**

1. **Tuple-struct construction interaction.** `Point(1.0, 2.0)` is direct tuple-struct init today. Either auto-derive `impl Call` for every tuple struct (clean unification, non-breaking pre-1.0) or keep tuple-struct init as a parser-level-precedence rule that runs before `Call` dispatch. Auto-derivation is cleaner but formally makes tuple structs a special case of the callable mechanism.
2. **Enum-variant construction.** Probably *not* subsumed — variants carry a discriminant that's semantically distinct from arbitrary callable dispatch. `Some(x)` stays variant construction.
3. **Relation to `From` / `.from`.** Not subsumed. `From` carries conversion-specific semantics (reversible via `Into`, used by `?` for error widening). `Call` is more general; they coexist.
4. **Orphan rules.** Whether third-party modules can `impl Call[X] for StdType` needs an explicit rule; default should be "no" — otherwise any type in the ecosystem becomes arbitrarily callable by downstream code.
5. **Diagnostic quality.** Non-callable types hit with parens-call need a specific error naming the fix: "type `T` is not callable; implement `Call[Args, _]` or use an associated function such as `T.new(...)`."
6. **Effect integration.** A `call` with no `with` clause has no ceiling, so a call through `Call` has the effects of the instance's method ([design.md §12](design.md#trait-methods-and-generic-calls)). No new machinery needed.

**Why deferred:** The construction-sugar ergonomic payoff is crippled without heterogeneous varargs — `Vec(1, 2, 3)` requires a 3-arity `Call` impl distinct from the 2-arity and 4-arity ones, which scales poorly. The unification-of-closures-and-user-callables payoff is real but modest in a systems-oriented language where the callable-object pattern is rarer than in DSL-heavy or scientific-computing languages. Better to revisit once **Heterogeneous Varargs / Variadic Generics** (above) has a committed design — `Call` plus heterogeneous varargs together is what makes this genuinely useful; `Call` alone delivers at most single-argument sugar.

**Why non-breaking:** Purely additive. Existing code using explicit associated-function names (`T.from(x)`, `T.new()`, `T.with_capacity(n)`) is unaffected. Opt-in per type via trait impl.

**Cross-reference:** **Heterogeneous Varargs / Variadic Generics** (above). Should be decided together or in sequence — `Call` without varargs is strictly less valuable.

### Struct Literal Type Prefix in Check Mode

Whether the struct literal prefix (`WordCount { total: 42, unique: 30 }`) should remain required in every position (status quo) or become elidable to `{ total: 42, unique: 30 }` when a unique target struct type is known from context — return type of the enclosing function, `let x: T = ...`, argument position, or a nested struct-literal field value.

**Current lean:** ~55/45 toward elidable-in-check-mode, weakly held. Consistency is the deciding factor: Kāra already infers generic type arguments (`Vec.filled(5, 0) → Vec[i64]`), integer literal types (`let x: u8 = 42`), and closure parameter types from check-mode context (grammar accepts, typechecker errors if unresolved). Requiring the struct-literal prefix is the only redundant annotation Kāra currently mandates in a check-mode position. The "semantics-Rust, syntax-mainstream" tiebreaker also favors elision — C# target-typed `new()`, Swift `.init(...)` on typed target, Java record target-typing all elide.

**Strongest counter-argument:** local readability in long functions — a reader shouldn't have to trace outward to the return signature, let-binding, or call site to identify the type of a brace-literal. Real but not a consistency argument. An unexplored alternative (`.{ ... }` à la Zig as a distinct "infer-from-context" syntax) is rejected — second syntax for a minor ergonomic gain.

**Why non-breaking:** Purely additive. Existing code with explicit prefixes continues to work under either resolution. Elision is opt-in at the construction site.

**Re-evaluation criterion:** Revisit once enough real Kāra code exists to count how often the prefix is genuinely redundant vs. load-bearing for local readability. Heuristic: if >80% of struct literals sit next to a target annotation within ~3 lines, favor elision; if long functions with deeply nested literals are common and the prefix materially helps reading, keep status quo.

**Backstop:** Must decide before any tutorial introduces struct literals to external readers. Syntax shown there becomes muscle memory and is costly to change afterward.

### Non-ASCII Identifiers

The lexer's case-class rules are defined on ASCII alphabetic characters; identifiers containing non-ASCII characters are a parse error in v1 ([design.md §3](design.md#unicode)). A future edition may extend classification to Unicode case via UAX #31 conformance. No committed design.

### `std.cli` — Argument Parsing

**Decision:** Ship `std.cli`, a standard argument parser. Minimum surface: builder-style `Parser`, named args + flags + positional, subcommands, automatic `--help`/`--version`, structured error type.

**Why ship it.** Every scripting/CLI workload — and a meaningful fraction of user code will be CLI tools — needs argument parsing beyond raw `env.args()`. Without a canonical stdlib argparse, every user writes the same boilerplate or pulls a third-party crate before their first feature. For a general-purpose language, "argparse is third-party" is the wrong default.

**Why non-breaking:** New stdlib module.

**Design shape:**

```kara
import std.cli

let parser = cli.Parser.new("greet")
    .about("Greets a name")
    .arg("--name", cli.Arg.string().required().help("name to greet"))
    .flag("--verbose", short: 'v', help: "verbose output")
    .subcommand("upper", cli.Parser.new("upper").about("uppercase the greeting"));

let args = parser.parse()?;                  // Result[Args, CliError]
let name = args.get_string("--name")?;
let verbose = args.get_flag("--verbose");
```

Effect: `reads(Env)` on `.parse()` (consumes `env.args()`). API surface inspired by clap's builder pattern; perfection is not required, canonicality is.

### `OrderedMap[K, V]` / `OrderedSet[T]` — Insertion-Ordered Collections

**Decision:** Add `OrderedMap[K, V]` (and its set counterpart `OrderedSet[T]`) as separate stdlib types alongside `Map[K, V]` / `Set[T]`. Iteration yields entries in the order they were first inserted; re-inserting an existing key updates the value but does *not* move the key in the order.

**Why deferred:** v1 ships with `Map[K, V]` / `Set[T]` (unordered, hash-table-backed) and `SortedMap[K, V]` (sorted by key). A third collection axis — insertion-order — is genuinely useful (deterministic iteration for Display / golden tests, removes the "linked-list-of-(key, value)-pairs" boilerplate users otherwise write), but the use cases are narrow enough that v1 doesn't need to ship three hash-table flavors. Once `Map` / `Set` are stable, lifting the implementation to `OrderedMap` / `OrderedSet` is mechanical.

**Why non-breaking:** Purely additive. New collection types; `Map[K, V]` semantics — including the unspecified iteration order ([library/collections.md](library/collections.md)) — are unchanged. Code written against `Map` continues to compile and behave identically.

**Why a separate type, not "promote `Map` to insertion-ordered":** Keeping `Map[K, V]` order-unspecified preserves runtime freedom — the implementation can swap hash strategies (Robin Hood, Swiss-table variants, sharded concurrent maps) and rehash on growth without breaking semantics. Pinning insertion-order into `Map` would be a one-way door: every future strategy must preserve it, and concurrent `Map` variants become significantly harder. Users who want stable iteration opt into `OrderedMap` and accept its costs (extra memory for the order spine, branch + pointer writes per insert/remove, harder concurrent variants). This is the Rust ecosystem's split (`HashMap` + `indexmap`); we follow it.

**Design shape:**

- API mirrors `Map[K, V]` / `Set[T]` exactly — `insert`, `remove`, `get`, `contains`, `entry`, `len`, `is_empty`, iteration, etc. The only observable difference is iteration order.
- Two viable implementation strategies:
  - **Linked-list spine.** Hash table entries also carry `prev` / `next` pointers; iteration walks the linked list. Java's `LinkedHashMap` shape. Adds 16 bytes/entry (two pointers).
  - **Compact-dict.** A dense `Vec[(K, V)]` in insertion order plus a sparse hash table storing indices into the dense array. Python 3.6+'s shape. Adds ~1 index/entry to the hash table; deletion tombstones the dense slot and triggers periodic compaction. Better cache locality on iteration.
- Choice between the two is an implementation detail; `OrderedMap[K, V]` semantics don't depend on which is used. Lean toward compact-dict for memory + iteration speed; spine is simpler if compaction proves fiddly.
- Removal semantics: `remove(k)` removes the entry; later iteration skips the removed key. No order shift on remove (the surviving keys keep their original positions). `entry(k).or_insert_with(...)` on a missing key inserts at the end; on a present key, value is updated but order is unchanged.
- Effect parity with `Map[K, V]`: `allocates(Heap)` on growth, `panics` on `unwrap`-style accessors. No new effect surface.

**Cross-reference:** [library/collections.md](library/collections.md) (unordered `Map`/`Set` semantics). Deterministic `Display` output is a real ergonomic gap that `OrderedMap` would close.

### Terminal Control Library (`std.terminal` or `kara-terminal`)

A stdlib module or external package providing cursor movement, screen clearing, and color control — the minimum needed to write CLIs, TUI dashboards, and game-of-life–style display loops without embedding raw ANSI escape sequences.

**Why not in the v1 standard library.** Terminal control is platform-specific (ANSI/VT on Unix/macOS, Console API on Windows), depends on terminal capability queries (`TERM`, `COLORTERM`), and requires graceful fallback for pipes and redirected output. This scope is better owned by a dedicated library (Rust's `crossterm` is the reference point) than baked into the language's core stdlib. For v1, callers write raw ANSI via `print` / `println` — which correctly carries `writes(Stdout)` and participates in effect tracking — at the cost of platform portability.

**Minimum API surface:**

```kara
pub fn clear_screen() with writes(Stdout)
pub fn move_cursor(row: i64, col: i64) with writes(Stdout)
pub fn hide_cursor() with writes(Stdout)
pub fn show_cursor() with writes(Stdout)
pub fn set_color(fg: Color, bg: Color) with writes(Stdout)
pub fn reset_color() with writes(Stdout)

pub enum Color { Black, Red, Green, Yellow, Blue, Magenta, Cyan, White, Reset, Rgb(u8, u8, u8) }
```

All functions carry `writes(Stdout)` so they participate in conflict analysis and are correctly serialized against other stdout writes in a parallel region. Platform dispatch (ANSI vs. Windows Console API) is hidden behind the function boundary.

**What it rests on:**
- `writes(Stdout)` effect, which is already in v1

**Cross-reference:** [library/io.md](library/io.md) — `print`/`println` with `writes(Stdout)` are the v1 primitive; this module is an ergonomic layer above them.

## Permanent omissions

Features the project has explicitly chosen *not* to build. Distinct from every other section of this file (which all express some degree of "will or might ship"): permanent-omission entries record decisions that the language will *not* allocate design surface or stdlib infrastructure to a given feature, ever. The category exists so the decision is durable — future contributors can see *why* the omission is intentional rather than rediscovering the question and re-litigating it.

A permanent omission is not "we'll never reconsider." If a future ecosystem reality genuinely changes the trade-offs, an entry can be moved out of this section and reopened. The bar for that motion is high: a concrete real-world need that the omission's stated rationale fails to address.

### Dynamic Linking (Runtime `dlopen` / Plugin Loading)

**Decision:** Static linking is the canonical default. Kāra does not provide dynamic-linking (`dlopen`, `dlsym`, plugin system, runtime `.so`/`.dll` loading) as a first-class feature. The runtime is statically linked into every `karac build` artifact; users who need plugin-style extensibility use one of the alternatives below.

**Why permanent:** Dynamic linking trades binary size for distribution headache (versioned runtime dependency on the target machine, ABI stability burden, package-manager friction, install-dance for the runtime, dlopen-at-startup overhead). Static linking matches Kāra's value proposition: predictable memory layout, zero-cost abstractions, compile-time effect verification, "ship one binary, run it anywhere with the right architecture." Dynamic linking would also blur the effect system at the FFI boundary — runtime-loaded code can't participate in compile-time effect verification, so any plugin ABI ends up bypassing the language's main correctness story.

The binary-size cost that motivates dynamic-linking adoption in C/C++ is addressed in Kāra by other means: strip + LTO + DCE in release builds, and runtime decomposition into per-feature archives if shipping data ever justifies it. The wins available from those mechanisms are sufficient without changing the deployment model.

**Alternatives (when extensibility is genuinely needed):**

1. **IPC + effect-typed services.** Separate processes communicating over channels; effects are tracked per-process boundary. This is the recommended pattern for plugin-style extensibility in Kāra — it preserves the language's correctness guarantees and matches the auto-concurrency story.
2. **WASM plugins.** AOT-compiled plugin code, fully sandboxed, effect-safe by construction. WASM is a v1 target ([design.md §16](design.md#the-v1-target-set)), which enables this pattern.
3. **Hand-rolled `dlopen` in `unsafe` blocks.** Users who need raw C-style plugins (e.g., loading vendor-supplied `.so` files) can use the FFI surface in `unsafe` blocks. They opt out of the effect system at that boundary and accept manual responsibility for the loaded code's behavior. This is a v1-supported escape hatch, not a recommended default.

**Why non-breaking:** Not a restriction on existing code. Purely a statement that the language does not allocate first-class surface or stdlib infrastructure to dynamic linking. Users who need the feature today construct it through FFI; those mechanisms remain available.

**Cross-reference:** [design.md §15](design.md#extern-c) (manual `dlopen` in `unsafe` blocks); [design.md §16](design.md#the-v1-target-set) (WASM target as the supported plugin pattern); [design.md §13](design.md#channels) (channels as the in-language extensibility pattern).

### Full Bytecode-First JIT (HotSpot / V8 / JVM-class)

**Decision:** Kāra does not ship as a bytecode-first language with a tier-up JIT (interpret → baseline JIT → optimizing JIT). Source ships as native; the optimizing compiler runs at AOT time, not in-process for every program. Permanent omission.

**Why permanent.** Bytecode-first JIT is a different language design, not just a different runtime. It implies:

- Source ships as IR or bytecode, not native binaries — contradicts `karac build` producing distributable artifacts.
- Cold-start penalty becomes the norm — every program pays "warm-up" before steady-state perf kicks in.
- The optimizing compiler runs in-process for *every* program, not just adaptive workloads — ~10–50 MB of compiler in every binary, every server, every embedded target.
- Effect / ownership / borrow checking would have to (partially) move to JIT time — splits the verification surface and complicates the soundness story.
- Backend services and embedded targets — Kāra's primary positioning — gain nothing from this and pay all of it.

**Why this is rejected on design grounds, not feasibility.** The technique works (HotSpot, V8, JavaScriptCore, .NET, modern Java with C2/Graal). It's the right shape for languages where source distribution is the deployment model (browsers, JVM containers, .NET assemblies) and cold-start latency is acceptable. Kāra's positioning — AOT-first systems language, deployable as native binaries, embedded-friendly — makes bytecode-first the wrong shape *by design*, not by accident of cost.

**Alternatives that cover the genuine adaptive-perf use cases:**

1. **Static PGO + AutoFDO** (see [Profile-Guided Optimization Loop](#profile-guided-optimization-loop)). Distribution-shaped optimization without bytecode in the binary.
2. **Continuous PGO + shared-object hot-swap** (see [Continuous PGO with Shared-Object Hot-Swap](#continuous-pgo-with-shared-object-hot-swap)). Minutes-of-latency adaptive perf for warehouse services.
3. **Runtime monomorphization JIT** (see [Runtime Monomorphization JIT](#runtime-monomorphization-jit)). Narrow, AOT-shaped JIT for the specific case of dynamic-boundary-discovered generic instantiations. Does not speculate, does not deopt, does not require an interpreter tier.

These three together cover the adaptive-perf use cases without changing the language's deployment model or the soundness story.

**Cross-reference:** [Speculative Tiering with Deopt](#speculative-tiering-with-deopt), the half-step short of full bytecode JIT, which is also declined for similar reasons.

### Database `database/sql`-Class Stdlib

**Decision:** Kāra does not ship a stdlib database driver layer (e.g., a `std.sql` module providing `Connection`, `Statement`, `Rows` over a stable cross-DB interface — Go's `database/sql` shape). Database driver design is community territory. Permanent omission.

**Why permanent.** Every modern systems language has settled on this same answer. Go's `database/sql` is widely regarded as a partially-frozen early commitment that the ecosystem has been working around for fifteen years (driver inconsistency, awkward statement caching, no first-class typed-row support, prepared-statement footguns). Rust deliberately punted: `sqlx` / `diesel` / `sea-orm` / `tokio-postgres` are community libraries with diverging philosophies (compile-time-checked SQL vs. ORM vs. raw query builder vs. async-first-low-level), and the ecosystem benefits from that diversity rather than being constrained by an early stdlib choice.

Database driver design has strong ecosystem-divergent forces — connection pooling philosophy (per-connection vs. global pool vs. provider-injected), async vs. sync semantics, ORM-ish vs. query-builder vs. raw-SQL, type-safe queries vs. dynamic-SQL, transaction lifecycle, prepared-statement caching, schema migration integration. A stdlib choice picks winners and ages poorly.

Kāra's comptime story makes a *typed-SQL* community driver dramatically better than what Go / Rust have today: comptime SQL parameter binding can produce compile-time SQL type-check (Diesel-shape but with first-class language support, no proc-macro indirection), comptime schema migration validation is naturally expressible, comptime query plan inspection is on the table. That's a Kāra-native ecosystem opportunity, not a stdlib opportunity — locking in stdlib shape closes off the most interesting community-driver designs.

**What drivers build on instead.** A connection pool and application-layer backpressure (semaphores, bounded channels, rate limiters) are ordinary library code a driver can use or provide. The provider machinery gives drivers a clean test-injection story without driver-specific test infrastructure, and the networking runtime (event loop, `sends(Network)` / `receives(Network)` effects) is what database drivers route through; both arrive with the [services track](#m4a-services).

**Why non-breaking:** Not a restriction on existing code. There is no `std.sql` module; community drivers (a Postgres library, a comptime-typed SQL builder, etc.) operate as ordinary packages. Later reconsideration is possible if a community driver emerges as a near-universal default and ecosystem signal supports stdlib promotion — but the bar is high: a clear convergence, not a single popular library.

**Cross-reference:** [design.md §1](design.md#what-kāra-is-not) (Kāra's positioning); [Canonical Postgres Driver](#canonical-postgres-driver-kara-postgres--project-owned-package) (a project-owned package, not a stdlib module).

### Speculative Tiering with Deopt

**Decision:** Decline speculative tiering (HotSpot-class adaptive optimization with deoptimization points and on-stack replacement). Considered, declined, documented for durability.

**What it would be.** AOT-compiled binary recompiles hot paths at runtime with speculative assumptions ("this `match` arm never taken"; "this virtual call always dispatches to `Foo`"); when the assumption is invalidated, deoptimize back to a slower, more general body without losing in-flight execution. Required infrastructure: deoptimization points in IR, type-feedback profiling at runtime, on-stack replacement (OSR) for tier-up, invariant-violation handlers.

**Why declined.** Three Kāra-specific frictions plus one architectural cost:

- **Effects.** Speculative inlining across an effect boundary changes the function's effect set. Re-checking at JIT time doubles the verification surface.
- **Ownership.** Move/borrow analysis is a property of AOT-checked source; speculative reordering must preserve it. Re-running ownership analysis on JIT'd code is feasible but complicates the soundness argument.
- **Frame layout.** Stack frames for OSR need a stable on-disk schema; none exists today.
- **Deopt-point cost on AOT performance.** Even programs that never deopt pay a pessimization tax: deopt points constrain instruction scheduling (no reordering across them) and frame state preservation (more spills, less aggressive register allocation). Java HotSpot has this; V8 has this; **it's the reason GraalVM native image underperforms HotSpot in steady state**. Adopting speculative tiering means accepting this tax for *every* Kāra binary, even ones that never speculate.

**The reward-to-complexity ratio is poor.** Backend services and embedded targets do not need HotSpot-class adaptation. The cases where speculative tiering pays off (long-running JVM-style monoliths with very high throughput) are precisely the cases where continuous PGO + hot-swap (above) gets most of the win at a fraction of the cost. Continuous PGO has minutes-of-latency adaptation; speculative tiering has sub-second. For warehouse services, minutes is fine.

**Conflict with Kāra's positioning.** Effects + ownership + monomorphization-first are load-bearing for Kāra's correctness story. Speculative tiering routes around all three: it speculates past effect boundaries, reorders past ownership analysis, and re-specializes past AOT monomorphization. The whole runtime invariant-stack would need to be re-validated under speculation. Even a credible design effort here is multi-quarter and conflicts with the language's first-principles correctness narrative.

**Cost surface.** ~16–24+ engineering weeks. Very high risk; interacts with every part of the runtime/codegen stack.

**Why this isn't simply "rejected".** Speculative tiering is a real technique that real systems benefit from; it's not technically unsound. The decision is that the trade-off doesn't fit Kāra's design — not that the technique is wrong. It may be revisited if circumstances change.

**Cross-reference:** [Continuous PGO with Shared-Object Hot-Swap](#continuous-pgo-with-shared-object-hot-swap) is the documented alternative for warehouse-scale adaptive perf needs.

### Declined — Adjacent Products Not Built on the Language

Recorded so they are not re-proposed. Three product ideas were **declined for this repository** on a single consistent test: a deferred entry must be able to answer *"what it rests on in the language,"* and these cannot. They are not rejected as ideas — they are recognized as work that is not this project's.

- **Effect inference for existing Python / TypeScript codebases.** Declined because its value proposition is explicitly "get the benefit *without* adopting Kāra" — it is a hedge against the language rather than something built on it, so no rests-on line exists. The legitimate version of the underlying need — incremental adoption without a rewrite — is answered by Kāra as a component you add, through FFI in both directions ([design.md §15](design.md#extern-c), [Exported C ABI](#exported-c-abi)).
- **Structured defect-class telemetry as a product.** Declined because nothing about it requires Kāra.
- **Hosted pre-merge verification sandbox.** Declined for the same reason — generic CI infrastructure.
