# Design Audit Findings: F-076 – F-100

Each finding has an anchor matching its number (e.g. `#f-076`).

---

## F-076 ✓ RESOLVED — `Cancelled` type compatibility mechanism unspecified {#f-076}

**Decision:** Option D — `Cancellable` trait, enforced at the `par {}` / `?` site. `?` inside a `par {}` block is a compile error if the enclosing function's `Err` type does not implement `Cancellable`. Runtime calls `E::cancelled()` to produce the cancellation value for sibling `errdefer(e)` blocks. `#[derive(Cancellable)]` adds a `Cancelled` variant to an enum. "No special-case machinery" claim corrected — `Cancellable` is the stated mechanism. Block-piercing `?` bullet updated to include the `Cancellable` requirement. `spec-updated`

**Type:** `gap`
**Status:** `resolved`
**Spec ref:** `docs/design.md` lines 6857, 6877, 6905

### Problem

The spec makes a strong claim at line 6905:

> "The `Cancelled` value is a real member of the function's `Err` type, so the binding is well-typed without any special-case machinery."

But nowhere does the spec define the mechanism by which `Cancelled` becomes a member of an arbitrary error type. Consider a function with a concrete error type:

```kara
fn do_work() -> Result[i32, String] {
    par {
        fetch_value()?   // if this fails, sibling is cancelled
        compute()?       // cancelled sibling: errdefer(e) sees e = Cancelled
    }
}
```

The cancelled sibling's `errdefer(e)` must bind `e` to a value of type `String`. But `String` has no `Cancelled` constructor. Similarly:

```kara
enum AppError { DbTimeout, ParseFailed }

fn process() -> Result[Data, AppError] {
    par {
        load_db()?
        load_file()?   // if cancelled: e = Cancelled, but AppError has no Cancelled variant
    }
}
```

The spec says there is "no special-case machinery," yet something must inject `Cancelled` into every possible error type. The options are mutually exclusive, and the spec commits to none of them:

**Option A — Trait bound.** A `Cancellable` (or `FromCancelled`) trait is required on the function's `Err` type when it participates in a `par` region:
```kara
trait Cancellable {
    fn cancelled() -> Self;
}
```
Every user-defined error enum must either `derive(Cancellable)` (auto-adds a `Cancelled` variant) or implement `Cancellable` manually.
- Pro: explicit, discoverable, no compiler magic.
- Con: all error types used in `par` contexts must add this bound; breaks functions whose error types are from external libraries.

**Option B — Compiler injection.** The compiler automatically adds a hidden `Cancelled` variant to any enum used as an `Err` type in a `par` region.
- Pro: transparent to the programmer.
- Con: mutates user types invisibly; changes the enum variant set; breaks exhaustive pattern matches.

**Option C — `Cancelled` is a wrapper, not a member.** `errdefer(e)` in a cancelled sibling does NOT actually bind the user's error type — it binds a compiler-synthesized value that satisfies the binding syntactically but whose actual type is `CancelError`. The `with e = Cancelled` claim in the spec is inaccurate (or `Cancelled` is a unit `CancelError` value, not a `String` / `AppError` value).
- This directly contradicts "real member of the function's `Err` type."

**Option D — `par` requires a `Cancellable` error type implicitly.** Using `?` inside a `par` block is only allowed when the enclosing function's `Err` type implements `Cancellable`. If the type does not, using `?` inside `par` is a compile error.
- This is similar to Option A but is a restriction at the `par {}` / `?`-inside-par site rather than a general function requirement.

### Impact

Without specifying Option A/B/C/D (or another), the entire fail-fast / cancellation model is under-specified. Every user who writes an error enum and uses it in a `par` context must guess whether their type needs a `Cancelled` variant, and if so, how to add it.

### Recommendation

Option D (or A with `derive(Cancellable)` auto-generating the variant) is most consistent with Kāra's explicit style. The spec should:
1. Introduce a `Cancellable` trait in stdlib.
2. State that `?` inside a `par` block is a compile error when the enclosing function's `Err` type does not implement `Cancellable`.
3. Specify what `#[derive(Cancellable)]` generates (adds a `Cancelled` variant to an enum; for non-enum error types like `String`, manual impl required).

---

## F-077 ✓ RESOLVED — `?` propagation semantics across a `par {}` block boundary unspecified {#f-077}

**Decision:** Block-piercing semantics. `?` inside a `par {}` branch at any nesting depth triggers fail-fast and propagates to the enclosing function — not to the branch or block. The `par {}` expression type is always the success type (never `Result`-wrapped). Enclosing function must return `Result`/`Option`. Applying `?` to the `par {}` block result is a plain type error (block has type `T`, not `Result[T,E]`). "Error propagation in `par {}` branches — block-piercing `?`" subsection added to Feature 5. `spec-updated`

**Type:** `ambiguity`
**Status:** `resolved`
**Spec ref:** `docs/design.md` lines 6753–6762, 6769

### Problem

The spec's canonical `par {}` example shows `?` used inside branches:

```kara
fn load_dashboard(user_id: u64) -> Result[Dashboard, AppError] {
    let (profile, orders, notifs) = par {
        let p = fetch_profile(user_id)?
        let o = fetch_orders(user_id)?
        let n = fetch_notifs(user_id)?
        (p, o, n)
    }
    Ok(build_dashboard(profile, orders, notifs))
}
```

The binding is `let (profile, orders, notifs) = par { ... }`. This means the `par {}` block expression must have type `(Profile, Orders, Notifs)` — a plain tuple, not `Result[...]`. Yet `?` is used inside the branches.

The spec also says (line 6769): "`par {}` is a block expression. Its value is the value of the last expression in the block."

These two statements are only consistent if `?` inside `par {}` branches has **block-piercing semantics**: when a `?` fires inside a branch, control does not return to the branch-level call site — instead, the fail-fast mechanism runs (joining all branches, choosing the first-source-order `Err`), and the `Err` is returned from the **enclosing function**, never reaching `let (profile, orders, notifs) = ...` at all.

This is analogous to how `?` in normal code:
```kara
fn foo() -> Result[i32, E] {
    let x = bar()?;   // x: i32, not Result[i32, E] — `?` propagates to foo's return
    Ok(x + 1)
}
```
…makes the enclosing function return without reaching `Ok(x + 1)`.

But this analogy breaks down in a key way: in normal code, `?` on a `Result`-typed expression propagates out of the enclosing function **synchronously and directly**. Inside a `par {}` branch, `?` must instead:
1. Mark the branch as failed.
2. Signal siblings to cancel.
3. Wait for all siblings to join.
4. Return the first-source-order `Err` from the **enclosing function** (not just the branch).

This is a fundamentally different `?` propagation path — it goes through the `par` scope's fail-fast machinery before exiting the enclosing function. The spec never states that `?` has this behavior inside `par {}` blocks.

**Adversarial cases:**

```kara
// Case 1: par inside a non-Result function — is ? a compile error inside par?
fn transform(data: Data) -> Output {
    par {
        let a = risky_step(data)?   // ERROR? No Result return type
        let b = other_step(data)?
        combine(a, b)
    }
}

// Case 2: par { } return type when ? fires at different nesting levels
fn nested() -> Result[i32, E] {
    par {
        if condition {
            step_a()?   // how does ? here relate to the par block?
        }
        42
    }
}

// Case 3: explicit ? after par {}
fn explicit_result() -> Result[i32, E] {
    let x = (par {
        compute_a()?
        42
    })?;   // double ?
    Ok(x)
}
```

Case 3 is particularly interesting: if `par { compute_a()? ; 42 }` already causes the enclosing function to return `Err` when `compute_a()` fails, adding `?` at the block boundary would be syntactically legal but semantically dead — the inner `?` already caused an early return. Or the outer `?` is the real propagation mechanism and the inner `?` just returns an `Err` value to the `par` block's expression type `Result[i32, E]`?

### Recommendation

Add a "Error propagation in `par {}` branches" subsection to Feature 5 that explicitly states:

> `?` inside a `par {}` branch is syntactic sugar that makes the branch contribute its `Err` to the `par` block's fail-fast path. When a branch's `?` fires: (1) the branch runs its cleanup as the originating branch; (2) siblings are cancelled; (3) the `par {}` block as a whole is not entered (same as `?` causing early function return). The `par {}` block expression type is the success type of the last expression, not `Result[success, E]`. The enclosing function must return `Result[_, E]` for `?` to be legal inside a `par {}` block — same requirement as `?` outside `par {}`.

---

## F-078 ✓ RESOLVED — `go { ... }` syntax referenced in Feature 7 but undefined in Feature 5 {#f-078}

**Decision:** Interpretation A. `go { ... }` was informal shorthand for `spawn(|| { ... })` in Feature 7's "Concurrency Across Targets" section — Feature 7 describes cross-target consistency of existing Feature 5 primitives, not new ones. All `go { ... }` and `go / channel / par` references replaced with `spawn(|| { ... })` and `spawn / channel / par`. `spawn()` is now the canonical term throughout. `spec-updated`

**Type:** `contradiction`
**Status:** `resolved`
**Spec ref:** `docs/design.md` lines 6806, 7200–7207

### Problem

**Feature 5 (line 6806):**
> "Unstructured spawn (task outlives the spawning scope) is a future escape hatch, not part of v1."

Feature 5 defines two task-creation mechanisms: `spawn(|| ...)` (structured, returns `TaskHandle[T]`) and `TaskGroup.new()` + `group.spawn(|| ...)`. Neither is named `go`. There is no `go { ... }` syntax anywhere in Feature 5.

**Feature 7 / Concurrency Across Targets (lines 7200–7207):**
> "`go` / channel / `par` semantics are specified **target-agnostically at the source level.**"
>
> "**`go { ... }` task semantics.** Ownership transfer into the spawned closure, effect propagation to the spawning site, and completion semantics are identical across `native`, `wasm_browser`, `wasm_wasi`, and `gpu`. Targets that cannot support `go` (currently `gpu`) reject it during target-gate verification."

This directly contradicts Feature 5 in two ways:

1. `go { ... }` is treated as an already-existing, target-agnostic primitive in Feature 7, but is never introduced in Feature 5.
2. If `go { ... }` is unstructured spawn (akin to Go's `go` statement, creating a task that can outlive its lexical scope), it conflicts with Feature 5's explicit deferral of unstructured spawn.

**Candidate interpretations:**

**Interpretation A:** `go { ... }` is a different syntax for `spawn(|| { ... })` — structured spawn, same semantics, different spelling. Feature 7 uses `go` informally to mean "spawned task." The real primitive in v1 is `spawn()`.
- Contradiction: Feature 5 uses `spawn()`, Feature 7 uses `go {}`. One of them is wrong or the relationship is unspecified.

**Interpretation B:** `go { ... }` is unstructured spawn (Go-style), and Feature 5 is wrong to say it's deferred. Feature 7 considers it a v1 primitive.
- Contradiction with Feature 5's explicit deferral at line 6806.

**Interpretation C:** `go { ... }` is a third form — perhaps equivalent to `TaskGroup.new().spawn(|| { ... })` — that provides implicit `TaskGroup` semantics at function scope. Not introduced by Feature 5 and not the same as unstructured spawn.
- Undefined: no spec text supports this.

**Adversarial example:**

```kara
// Is this legal in v1?
fn handle_connections(listener: TcpListener) {
    loop {
        let conn = listener.accept();
        go { handle_client(conn) }   // go { ... } — defined?
    }
}

// Or must it be:
fn handle_connections(listener: TcpListener) {
    let group = TaskGroup.new();
    loop {
        let conn = listener.accept();
        group.spawn(|| handle_client(conn));
    }
}
```

### Recommendation

Feature 7's "Concurrency Across Targets" section should either:
1. Replace `go { ... }` with `spawn(|| { ... })` throughout, making Feature 5's terminology canonical, or
2. Define `go { ... }` in Feature 5 explicitly, specifying its relationship to `spawn()` and `TaskGroup`, and whether it is structured or unstructured.

Additionally: if `go {}` is retained, add it to `syntax.md` with a grammar production.

---

## F-079 ✓ RESOLVED — `TaskHandle` scope-binding check attributed to wrong compiler phase {#f-079}

**Decision:** Phase changed from "resolver" to "typechecker". Mechanism: `ScopeLocal` compiler-known marker trait, sealed in v1. `TaskHandle[T]` implements `ScopeLocal`. Typechecker rejects `ScopeLocal` types in three positions: function return type, struct/enum field type, channel-send argument. `ScopeLocal` is general-purpose for future scope-bound types. `spec-updated`

**Type:** `missing-error`
**Status:** `resolved`
**Spec ref:** `docs/design.md` line 6781

### Problem

Line 6781:
> "Returning a `TaskHandle` from a function, storing it in a long-lived field, or sending it across a channel is a compile error — the resolver treats `TaskHandle` as a scope-bound type that cannot escape."

The **resolver** is the name-resolution phase of the compiler. It has no access to type information. It cannot:
- Determine the return type of a call expression `spawn(...)` to know it is `TaskHandle[T]`
- Check if a `let` binding holds a `TaskHandle` without type analysis
- Detect that a binding is being stored in a struct field, which requires knowing the field's declared type
- Detect that a channel send transfers a `TaskHandle`

These checks require type information that is available only in the type checker or ownership checker, not the resolver.

**The impact is practical:** if the implementation follows the spec literally and places this check in the resolver, the check will silently fail (the resolver has no type information to check against). If the check is placed in the correct phase (type checker), the diagnostic's `phase` field in `--output=jsonl` will not match the spec's attribution, breaking clients that inspect phase attribution.

**Additionally:** the spec does not state *how* `TaskHandle` is made scope-bound. Mechanisms in Rust include:
- `PhantomData<*const T>` (makes `!Send`)
- Lifetime parameters (`TaskHandle<'scope, T>`) that tie the handle's lifetime to the spawning scope
- Compiler-special-cased types (like Rust's `JoinHandle` + compiler treatment)

None of these mechanisms exist in Kāra (which has no lifetime parameters in the Rust sense and a different ownership model). The spec must describe the actual mechanism.

**Adversarial examples that expose the gap:**

```kara
struct Server {
    pending: TaskHandle[Response],   // compile error — but which phase catches it?
}

fn start_work() -> TaskHandle[i32] {   // should be a compile error
    spawn(|| compute())
}

fn send_handle(ch: Channel[TaskHandle[i32]], h: TaskHandle[i32]) {
    ch.send(h)   // should be a compile error
}
```

### Recommendation

1. Change "the resolver treats `TaskHandle` as a scope-bound type" to "the type checker and ownership checker enforce `TaskHandle` as a scope-bound type" (or whatever phase is architecturally correct).
2. Specify the mechanism: either (a) `TaskHandle` has a compiler-magic non-escapable property, (b) a `!Escape` / `ScopeLocal` marker trait, or (c) a lifetime parameter `TaskHandle['scope, T]` binding it to the spawning scope. Given Kāra's ownership model, option (b) — a `ScopeLocal` trait that forbids the type from appearing in return positions, field types, or channel-send arguments — is most consistent with the language surface.

---

## F-080 ✓ RESOLVED — `collect_all` / `collect_all_vec` have no grammar and undefined auto-thunking behavior {#f-080}

**Decision:** Explicit closures — no auto-thunking. Auto-thunking was the only place in the language where a function call silently deferred argument evaluation; removed in favor of `collect_all(|| expr1, || expr2, ...)`. Type signatures written for 2- and 3-argument overloads (pattern extends to 8). `collect_all_vec` type signature added. Effect inference follows standard closure-invocation rules — no special case needed. `spec-updated`

**Type:** `syntax-gap`
**Status:** `resolved`
**Spec ref:** `docs/design.md` lines 6812–6836

### Problem

`collect_all` is described (line 6825) as:
> "a compiler builtin resolved via fixed-arity overloads — maximum 8 branches"

And (lines 6813–6823):
> "Arguments are **auto-thunked**: the compiler wraps each argument in a closure so branches execute concurrently, not eagerly at the call site"

```kara
let (profile, orders, notifs) = collect_all(
    fetch_profile(user_id),   // auto-thunked: becomes || fetch_profile(user_id)
    fetch_orders(user_id),    // auto-thunked: becomes || fetch_orders(user_id)
    fetch_notifs(user_id),    // auto-thunked: becomes || fetch_notifs(user_id)
);
```

**Three interrelated gaps:**

**Gap 1 — Grammar.** There is no grammar production in `syntax.md` for `collect_all`. It syntactically looks like a `CALL_EXPR`, but its arguments are evaluated lazily (auto-thunked), unlike every other function call. If the parser sees `collect_all(a, b, c)`, it must know `collect_all` is special *before* evaluating the arguments. This requires either:
- A keyword/builtin that the parser treats as a reserved identifier with special argument-evaluation rules, or
- An explicit closure form `collect_all(|| a, || b, || c)` (which would be a normal function call), contradicting the "auto-thunked" claim

**Gap 2 — Type signatures for fixed-arity overloads.** "Fixed-arity overloads" implies there are 8 separate functions:
```kara
fn collect_all[A, E1](f1: Fn() -> Result[A, E1]) -> (Result[A, E1])
fn collect_all[A, B, E1, E2](f1: ..., f2: ...) -> (Result[A, E1], Result[B, E2])
// ...up to 8 arguments
```
But none of these signatures are written anywhere in the spec. Without them, the return type inference for heterogeneous errors and the effect propagation rule have no formal grounding.

**Gap 3 — Effect inference for auto-thunked arguments.** The spec says (line 6823):
> "Effect: enclosing function inherits the union of all branch effects"

For auto-thunked arguments, the effects of `fetch_profile(user_id)` must propagate to the enclosing function even though the expression is wrapped in an implicit closure at compile time. For normal closures passed to `spawn()` or `par {}`, effects propagate because the closure's declared effect set is part of its type. For auto-thunked arguments, there is no user-written closure — the implicit thunk's effect must be inferred from the wrapped expression and then propagated. This is a special-case inference rule that needs to be stated.

**Adversarial examples:**

```kara
// Is this valid? (collect_all with non-Result branches)
let (a, b) = collect_all(compute_a(), compute_b());

// What if arguments have side effects before collect_all?
let x = side_effect_value();
let (r1, r2) = collect_all(
    use_x(x)?,     // x is moved here, but is it moved before or inside the thunk?
    other_fetch()
);
```

The second example shows an ownership interaction: if `use_x(x)?` is wrapped in a thunk, the capture of `x` inside the thunk follows standard closure capture rules. But if `x` is first evaluated (non-thunked) and then passed, `x` is moved before `collect_all` runs. The "auto-thunk" decision changes the ownership semantics of the arguments.

### Recommendation

1. Add a grammar production to `syntax.md`: `collect_all` should be a recognized compiler builtin keyword (not a function name), with explicit call-expression syntax distinct from normal function calls if its argument evaluation is different.
2. Alternatively, change `collect_all` to take explicit closures: `collect_all(|| fetch_profile(id), || fetch_orders(id))`. This removes the auto-thunking special case and makes it a normal function.
3. Whichever form is chosen, write the full type signatures for at least the 2-, 3-, and 4-argument cases, and state the effect-inference rule explicitly.
4. Define `collect_all_vec`'s type signature and effect rules similarly.

---

## F-081 ✓ RESOLVED — `Channel[T]` referenced as a language primitive but never formally specified {#f-081}

**Decision:** Promote the v1 minimal channel subset to formal spec. Added "### Channels" subsection to Feature 5: `channel[T](capacity) -> (Sender[T], Receiver[T])` stdlib function; `send(value: T) with blocks` and `recv() -> Option[T] with blocks` operations; ownership transfer, capacity semantics, multiple-sender Clone, drop semantics, and `ScopeLocal`-not-applied rationale. Fixed `after()` return type from `Channel[()]` to `Receiver[()]`. Updated deferred table: "Channels (async message passing)" → "Channel combinators (select, fan-out/fan-in, unbounded)" with a note that the basic primitive is now v1. `spec-updated`

**Type:** `gap, contradiction`
**Status:** `resolved`
**Spec ref:** `docs/design.md` lines 7200–7267, 8507

### Problem

The section "Concurrency Across Targets" (line 7200) treats `go / channel / par` as three co-equal target-agnostic primitives. The "Async Host APIs on WASM" section (line 7246) uses `Channel[T]` and `channel[T](capacity)` as idioms for async host integration. But the **Deferred Items** table at line 8507 explicitly says:

> "Channels (async message passing) | Concurrency runtime | Phase 6 | Fork-join isn't enough. `Mutex[T]` with `lock` block syntax is decided; **channels still open.**"

This is a direct contradiction: the deferred items table says channels are Phase 6 and **design is still open**, but the spec body uses `Channel[T]` as if it exists and is designed in v1. The "Async Host APIs on WASM" section (lines 7246–7267) shows `Channel[T]` being used as the implementation mechanism for async host API integration. But `Channel[T]` is never formally introduced anywhere in Feature 5 or elsewhere in the spec.

**What appears in the spec (all without prior definition):**

```kara
// Line 7250 — return type Channel[()] used in a stdlib function
pub fn after(duration: Duration) -> Channel[()]
    with writes(Timer), allocates(Heap)

// Line 7253 — channel constructor returning... what?
let (tx, rx) = channel[()](1);   // 1 = capacity?
                                  // What is the type of tx? rx? Both Channel[()]?

// Line 7254 — send on one end
host.set_timeout(|| tx.send(()), duration.as_ms());

// Line 7265 — recv on the other end
after(Duration.ms(500)).recv();
```

**Unspecified:**

1. **Type structure.** Is `Channel[T]` a single bidirectional type, or is there a `Sender[T]` / `Receiver[T]` split? The `let (tx, rx) = channel[T](cap)` syntax implies a split — `tx` is the send-end and `rx` is the receive-end — but their types are never named.

2. **Constructor syntax.** `channel[()](1)` — is `channel` a keyword, a function, a stdlib method on some type? The generic argument syntax `channel[()](1)` is unusual (unit type `()` as a generic argument, capacity `1` as a positional argument).

3. **Send / receive operations and effects.**
   - `tx.send(value)` — is this `sends(?)` or `writes(?)`? What resource?
   - `rx.recv()` — does this `blocks` or `suspends`? What effect?
   - Does `recv()` return `T` directly, or `Option[T]` (for closed channels), or `Result[T, ChannelError]`?

4. **Ownership transfer.** Line 7206 says "A value sent through a channel transfers ownership per Feature 5." But what exactly transfers? If `T: Copy`, does `send` copy or move? If `tx.send(value)` moves `value`, is `value` inaccessible after the call? The compile-error rule ("Post-send references by the sender are a compile error") at line 7206 is stated but never specified mechanically.

5. **Closing / dropping semantics.** When `tx` is dropped, what happens to pending `recv()` calls on `rx`? What does `rx.recv()` return when the sender is dropped?

6. **Bounded vs unbounded.** The capacity parameter `1` in `channel[()](1)` implies bounded channels. Is there an unbounded form? What happens when the bounded channel is full — does `send` block (`blocks`)? Panic? Return `Result[(), SendError]`?

7. **`after()` returns `Channel[()]` not `Receiver[()]`.** If the type is `Channel[()]`, then the channel object holds both send and receive ends — but the `(tx, rx)` destructuring pattern suggests a tuple. This is inconsistent.

### Impact

Channel semantics affect concurrent program correctness. Without a formal specification, programs that use channels for cancellation, event notification, or producer-consumer patterns cannot be verified against the spec. The WASM async integration pattern (`after()` as a channel-based timer abstraction) is described as idiomatic but its underlying type is undefined.

### Recommendation

Add a "Channels" subsection to Feature 5 that specifies:
- `channel[T](capacity: usize) -> (Sender[T], Receiver[T])` (or equivalent)
- Effect signatures for `Sender.send()` and `Receiver.recv()`
- Ownership transfer rules for `send()`
- `Receiver.recv()` return type on closed channel
- Drop semantics for both ends
- Relationship to `go { ... }` if `go` is retained

Cross-reference in the WASM async section.
