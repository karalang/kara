# Design Audit Findings: F-051 – F-075

Each finding has an anchor matching its number (e.g. `#f-051`).

---

## F-051 ✓ RESOLVED

**Decision:** A bound reference to a `mut ref self` method is multi-callable. Forming it requires the captured binding to be `mut ref`-accessible at the creation site (owned `let mut` or `mut ref` parameter); using a `ref`-only binding is a compile error. Each call transiently borrows the captured value exclusively.

**Changes:** `docs/design.md` § UFCS method references and ownership — extended the existing paragraph with the `mut ref self` rule and an example.

---

### Bound method reference with `mut ref self` receiver — behavior unspecified

**Section:** First-Class Functions — UFCS method references and ownership (line 2955)
**Tags:** ambiguity

## Finding

The spec covers two cases for bound method references:

> A bound method reference `user.validate` captures `user`. If `validate` takes bare `self`
> (owned/consuming receiver), the reference is once-callable — calling it moves `user`. If
> `validate` takes `ref self`, the reference may be called any number of times.

The `mut ref self` case is silently omitted. For a method that takes `mut ref self`:

- Calling it requires an exclusive mutable borrow of `user`.
- A bound reference `user.save` (where `save` takes `mut ref self`) would need to hold an
  exclusive borrow for the duration of each call — which should be fine for multi-call.
- But if `user` is itself a `ref T` binding (not owned), can a bound method reference be
  formed at all? The reference would need to yield exclusive borrowing rights on demand.

Concretely: is `user.save` (where `save: fn(mut ref self) -> ()`) a `Fn() -> ()` that can be
called multiple times? Or does it require ownership of `user` to call? Or is it a compile error
to form the bound reference at all?

## Decision needed from author

Extend the "UFCS method references and ownership" paragraph to cover the `mut ref self` case.
Likely answer: a bound reference to a `mut ref self` method is multi-callable but requires
`user` to be a `mut ref`-accessible binding at the creation site; creating the reference itself
is fine, and each call transiently borrows `user` exclusively. Confirm and add an example.


---

## F-052 ✓ RESOLVED

**Decision:** Explicit mode annotations are **not permitted** on destructured parameters. `fn handle(ref Request { headers, body }: Request)` is a compile error. Mode is always inferred from usage for destructured params. Same for closures: `|ref (a, b)|` is a compile error. To use a uniform borrow, write a simple parameter and destructure in the body.

**Changes:** `docs/design.md` § Destructuring in Function and Closure Parameters — added a "Explicit mode annotations are not permitted on destructured parameters" bullet.

---

### Explicit mode annotation combined with parameter destructuring — unspecified

**Section:** Destructuring in Function and Closure Parameters (line 3106)
**Tags:** ambiguity

## Finding

The spec defines mode inference for destructured parameters:

> Mode inference is per-binding, not per-parameter. When at least one binding is consumed, the
> parameter as a whole is treated as `own`.

But it never addresses whether an explicit mode annotation is valid on a destructured parameter:

```kara
fn handle(ref Request { headers, body }: Request) { ... }
```

Is this syntax valid? Possible interpretations:

1. **Valid.** The explicit `ref` overrides inference; the whole parameter is treated as
   `ref T`, and all destructured bindings are read-only references into the borrowed value.
   Any body use that tries to move `body` out would be a compile error.
2. **Compile error.** Explicit mode annotations are not allowed on destructured parameters —
   only on simple `name: T` parameters. The compiler always infers mode for destructuring.
3. **Unspecified.** The grammar may or may not parse `ref PATTERN : TYPE`, and behavior is
   implementation-defined.

The closure form `|ref (a, b)| ...` or `|(ref a, ref b)| ...` is similarly unaddressed.

## Decision needed from author

State whether explicit mode annotations (`ref`, `mut ref`) are allowed on destructured
parameters. If allowed, specify the interaction: does the explicit annotation override
per-binding inference, or must it agree with what inference would have produced?


---

## F-053 ✓ RESOLVED

**Decision:** Option 1 — `_` is top-level only. `_` as a pipe hole is valid only as a direct argument to the pipe stage's outermost call. `data |> f(g(_))` is a compile error; the `_` inside `g(...)` is the wildcard/discard pattern.

**Changes:** `docs/design.md` § Pipe Operator `|>` — the `_` bullet now explicitly states "valid only as a direct argument to the pipe stage's outermost call" and gives a compile error example for nested `_`.

---

### Pipe `_` placeholder in nested call arguments — ambiguity unaddressed

**Section:** Pipe Operator `|>` (line 3148–3150)
**Tags:** ambiguity

## Finding

The spec says:

> `_` is the pipe-hole placeholder. It stands for the left-hand value when it is not the first
> argument. At most one `_` per stage.

But it says nothing about `_` appearing inside a *nested* function call that is an argument to
the pipe stage:

```kara
data |> f(g(_), extra)      // does inner _ refer to the piped value?
data |> f(g(data), extra)   // equivalent? or compile error?
```

If `_` is only valid at the top-level argument position of the pipe stage's call, then `g(_)`
is a compile error (ambiguous: is `_` a wildcard pattern or a pipe hole?). If `_` can appear
anywhere inside the argument list (including nested calls), the semantics become: substitute
the piped value at every `_` in the call expression — but then the "at most one `_`" rule
becomes "at most one top-level `_`" vs. "at most one `_` anywhere in the expression."

## Decision needed from author

Clarify whether `_` as a pipe hole is:

1. **Top-level only** — valid only as a direct argument to the piped function, not inside
   nested sub-expressions. `data |> f(g(_))` is a compile error; `_` inside `g()` is the
   wildcard/discard pattern, not the pipe hole.
2. **Anywhere in the call** — `_` inside any sub-expression within the pipe stage is the pipe
   hole (at most one occurrence total). `data |> f(g(_))` passes `data` as the argument to `g`.

Option 1 is simpler and avoids confusion with wildcard `_`. Recommend spelling out the rule
explicitly in the pipe section.


---

## F-054 ✓ RESOLVED

**Decision:** Option 2 — assign-through for `mut ref` lvalues. `a += b` where `a: mut ref T` desugars to `*a = *a + b`. The local name retains its `mut ref T` type; the caller's value is updated. Same rule for all `mut ref` compound assignments.

**Changes:** `docs/design.md` § Compound assignment — added a new "Compound assignment on `mut ref` lvalues" paragraph with the assign-through rule.

---

### Compound assignment `+=` on `mut ref` bindings — desugar target undefined

**Section:** Operator Traits — Compound assignment (line 3302)
**Tags:** ambiguity, interaction

## Finding

The spec says:

> `a += b` desugars to `a = a + b` in v1.

For a plain owned binding `let mut a: i32 = 0`, `a = a + b` rebinds `a`. Clear.

For a `mut ref` binding (a mutable reference to some value):

```kara
fn increment(x: mut ref i32) {
    x += 1   // desugars to: x = x + 1 ???
}
```

Does `x = x + 1` rebind the local name `x` (so it now holds the result integer rather than a
`mut ref i32`)? Or does it assign through the reference (`*x = *x + 1`)? These are different
operations:

- Rebind: `x` loses its `mut ref` type; `*x` is unchanged; the caller's value is not modified.
- Assign-through: `*x` is updated; the caller sees the change.

The obvious intended behavior is assign-through, but `a = a + b` spelled out literally is a
rebind. The spec does not distinguish these cases.

The same ambiguity applies to `Vec`, `String`, and any other non-`Copy` type where `+=` makes
sense: `s += "suffix"` where `s: mut ref String` — rebind `s` or extend the referent?

## Decision needed from author

Specify whether the desugar `a = a + b` means:

1. **Literal assignment** — rebinds `a` in all cases (including `mut ref` parameters). Compound
   assignment on `mut ref` parameters does *not* affect the caller's value. (Probably not the
   intended behavior.)
2. **Assignment-through for `mut ref` lvalues** — `a = a + b` when `a` is a `mut ref T` binding
   desugars to an assign-through (`*a = *a + b`). Add a note clarifying the desugar rule for
   `mut ref` lvalues.


---

## F-055 ✓ RESOLVED

**Decision:** Layout-annotated collections are the **same type** as their plain counterpart — both spell `Vec[Entity]` and are type-compatible everywhere. Layout is a codegen specialization invisible to the type system; no implicit copy occurs at call boundaries. The receiving function operates on whatever layout the collection has. This is the formal underpinning of "changing layout is not an API break."

**Changes:** `docs/design.md` § Layout Rules — added a new "Layout-annotated collections are the same type as their plain counterpart" bullet.

---

### Layout monomorph type identity — can a SoA `Vec[Entity]` be passed where `Vec[Entity]` is expected?

**Section:** Feature 1 — Data Layout (line 3407)
**Tags:** gap, interaction

## Finding

The spec says:

> Within a project, two layout blocks for the same collection type with different groupings
> produce distinct codegen monomorphs.

And:

> Public APIs use logical struct types. Changing layout is not an API break.

But the spec does not say whether SoA-laid-out `Vec[Entity]` and plain `Vec[Entity]` are the
same *type* or different types. Concretely:

```kara
layout entities: Vec[Entity] { group physics { position, velocity } ... }

fn process(data: Vec[Entity]) { ... }   // declared elsewhere — AoS

process(entities)   // is this valid?
```

If they are the *same type* (both spelled `Vec[Entity]`), then `process(entities)` compiles but
receives what ABI representation? SoA must be coerced/materialized to AoS at the call boundary —
but no coercion rule is specified, and the overhead (a full O(n) copy) is invisible.

If they are *different types*, then `process(entities)` is a type error unless an explicit
conversion is performed — but there is no conversion function specified, and the "changing layout
is not an API break" guarantee would be violated whenever a consumer passes a SoA collection to
an AoS-expecting callee.

## Decision needed from author

1. Are `layout`-annotated collections the *same type* as their plain counterpart, or do they
   form a distinct (opaque) type?
2. If the same type: what coercion rule applies when a SoA collection is passed to a function
   expecting the plain type? Is the copy implicit? Is it a compile-time warning?
3. If distinct: what is the conversion function, and what are the performance guarantees?


---

## F-056 ✓ RESOLVED

**Decision:** Non-transparent user-defined verbs self-conflict on the same resource (same verb + same resource = Conflict), and do not cross-conflict with other verbs or built-in verbs on the same resource. `logs(X) + logs(X)` is a Conflict; `logs(X) + reads(X)` is Safe. Transparent verbs never conflict.

**Changes:** `docs/design.md` § Conflict Rules — added a "Conflict rules for non-transparent user-defined verbs" paragraph after the existing conflict table.

---

### Conflict semantics for user-defined verbs undefined

**Section:** Feature 2 — Fixed verbs and user-defined resources (line 3513, 3657)
**Tags:** gap

## Finding

The spec says non-transparent user-defined verbs "participate in conflict analysis … force sequential execution when two calls conflict." The conflict table covers only the eight built-in verbs. No predicate defines what "conflict" means for a user-defined verb.

Concrete question: if two parallel tasks both call a function declared `with logs(Stdout)`, does that conflict? Does `logs(X) + logs(X)` behave like `writes(X) + writes(X)` (conflict) or like `sends(X) + sends(X)` (safe)? Does `logs(X) + reads(X)` conflict on the same resource?

Possible interpretations:

1. User-defined verbs always self-conflict on the same resource (same-verb + same-resource = conflict), never cross-conflict with built-in verbs on the same resource.
2. User-defined verbs use a declared conflict model (read-like / write-like / send-like) chosen at declaration time.
3. User-defined verbs always conflict with themselves and with all other verbs on the same resource (maximally conservative).

## Decision needed from author

Extend the Conflict Rules section to cover user-defined verbs. If interpretation (1): state it explicitly. If (2): add a declaration-site modifier (e.g., `effect verb logs: write;`). If (3): state it. The current "participates in conflict analysis" claim is vacuous without a predicate.


---

## F-057 ✓ RESOLVED

**Decision:** Option 1 — only explicit panic invocations produce the `panics` atom. Arithmetic overflow is NOT a `panics` atom; it traps at the hardware/runtime level but is outside the effect system's scope. The full list: `unwrap()`, `expect(msg)`, `panic(msg)`, `todo!()`, `unreachable!()`, `assert!()`, array/slice indexing `[]`, integer division `/` and modulo `%`, and `as` refinement assertions.

**Changes:** `docs/design.md` § Effect Inference and Boundaries — added a "Complete list of `panics`-producing primitives" paragraph after the `direct(B_f)` definition, with explicit rationale for excluding arithmetic overflow.

---

### `panics` inference from arithmetic operators — panics-producing primitives not enumerated

**Section:** Feature 2 — Effect Inference and Boundaries (line 3797–3801)
**Tags:** ambiguity

## Finding

The transfer function says: "`direct(B_f)` is the atoms produced by non-call expressions … `panics`-producing ops like `unwrap`." The "like `unwrap`" is illustrative, not exhaustive. The spec states elsewhere that integer overflow "always traps" (numeric semantics section). If trapping counts as `panics`, then `+`, `-`, `*` on bounded integer types are all panics-producing:

```kara
pub fn total(items: Vec[i32]) -> i32 {
    let mut sum = 0i32;
    for x in items { sum += x; }  // overflow → panics?
    sum
}
// If panics is inferred here, every public arithmetic function needs `panics`.
```

Two interpretations:

1. Only *explicit* panics invocations (`unwrap`, `todo()`, `unreachable()`, explicit `panic(msg)`) produce the `panics` atom. Arithmetic overflow traps at the hardware/runtime level but is not tracked by the effect system as `panics`.
2. All operations that can trap — including arithmetic on bounded integers, out-of-bounds indexing, `as` refinement assertions — produce `panics`. Every public function that does arithmetic must declare it.

Interpretation (1) keeps the system tractable but leaves a gap: a function can terminate via overflow without the caller seeing `panics`. Interpretation (2) is logically complete but creates extreme annotation noise.

## Decision needed from author

State the complete list of "panics-producing primitives" for the transfer function, and decide whether arithmetic overflow is among them. If (2): add guidance on managing the annotation noise (e.g., is `panics` implicitly default-permitted in non-embedded profiles the same way `allocates(Heap)` is?).


---

## F-058 ✓ RESOLVED

**Decision:** Trait bound is **optional**. `effect resource Latency;` is valid — a bare resource is annotation-only, participates in conflict analysis, but cannot be used with `with_provider`. `R.Provider` is only defined for resources with at least one declared trait bound. Attempting `with_provider[BareResource]` is a compile error.

**Changes:** `docs/design.md` § Provider-Rooted Resources — added a "Trait bound is optional" paragraph. `docs/syntax.md` § 3.6 Effect Declarations — updated `EFFECT_RESOURCE` grammar to make the trait bound optional with a `TRAIT_BOUND` non-terminal.

---

### `effect resource` without trait bound — validity unspecified

**Section:** Feature 2 — Provider-Rooted Resources (line 4540, 4572)
**Tags:** gap

## Finding

Every user-defined resource example uses a trait bound:
```kara
effect resource UserDB: DatabaseProvider;
```

The `with_provider` signature references `R.Provider`, implying every effect resource has an associated provider trait. But built-in primitive resources (`Heap`, `Stderr`, `Network`, `Clock`, …) have no declared trait bound — they are language primitives. Some annotation-only resources (e.g., a resource used only to label writes to a config file, with no need for test injection) don't logically need a provider interface.

Is `effect resource Latency;` (bare, no trait bound) a compile error?

If all user resources must have a trait bound, the spec should say so. If trait bounds are optional, what does `R.Provider` resolve to for a bare resource, and can bare resources appear in `with_provider` calls?

## Decision needed from author

State whether the trait bound in `effect resource R: Trait` is required or optional. If optional: clarify what happens when a bare resource is used with `with_provider`. If required: update the grammar in `docs/syntax.md` accordingly.


---

## F-059 ✓ RESOLVED

**Decision:** The `with_provider` signature is made effect-polymorphic with `with E`. The closure parameter is `Fn() -> T with R, E`; the return type is `T with E`. This allows closures carrying additional effects beyond `R`.

**Changes:** `docs/design.md` § Provider-Rooted Resources — replaced the `with_provider` signature with the effect-polymorphic form and added a "Effect satisfaction at the `with_provider` boundary" paragraph explaining that `with R` is consumed at the boundary while `E` propagates.

---

### `with_provider` closure parameter type is too restrictive

**Section:** Feature 2 — Provider-Rooted Resources (line 4572)
**Tags:** gap, interaction

## Finding

The spec gives `with_provider`'s signature as:

```kara
with_provider[R: effect resource, T, P: R.Provider + Send + Sync](
    provider: P,
    f: Fn() -> T with R,
) -> T
```

By effect-set subtyping ("a function with fewer effects is a subtype of one with more"), the parameter `Fn() -> T with R` only accepts closures whose effects are a *subset* of `{R}`. Any closure that also uses another resource is rejected:

```kara
// closure effects: writes(UserDB), reads(Env)
with_provider[UserDB](InMemoryUserDB.new(), || {
    let uid = env.var("USER_ID")?;   // reads(Env) — not subset of {UserDB}
    create_user(uid)                  // writes(UserDB)
})
// → compile error by the subtyping rule
```

Real programs always carry additional effects inside a `with_provider` block. The signature as written makes `with_provider` nearly unusable.

The signature needs to be effect-polymorphic:
```kara
with_provider[R: effect resource, T, with E, P: R.Provider + Send + Sync](
    provider: P,
    f: Fn() -> T with R E,
) -> T with E
```

Without this fix, `with R` in the closure type is either a spec error, or "uses R (among others)" — but that reading contradicts the stated subtyping rule.

## Decision needed from author

Fix the `with_provider` signature to be effect-polymorphic (`with E`), or clarify how `Fn() -> T with R` is interpreted here when the subtyping rule would make it unusable.


---

## F-060 ✓ RESOLVED

**Decision:** `providers` is a reserved keyword. `in` is disambiguated because `providers {...}` can never appear as `for x in ...` left-hand side. Trailing comma optional. `providers {} in {}` is an expression. `?` in provider expressions propagates to the enclosing function. Grammar already existed in `docs/syntax.md § 3.15`.

**Changes:** `docs/design.md` § `providers { } in { }` Block — added a semantics bullet list addressing keyword status, `in` disambiguation, trailing comma, expression position, and `?` propagation.

---

### `providers { } in { }` block has no grammar production

**Section:** Feature 2 — `providers { } in { }` Block (line 4622)
**Tags:** syntax-gap

## Finding

The `providers` block is specified only by example and desugaring. No grammar production appears in the spec or `docs/syntax.md`. Open questions:

1. Is `providers` a reserved keyword or a contextual identifier?
2. `in` is already a keyword in `for x in collection` — does `providers { ... } in { ... }` introduce a grammar ambiguity?
3. Is the trailing comma after the last provider binding required, optional, or forbidden?
4. Can `providers { } in { }` appear as an expression (yielding a value) or only as a statement?
5. Can provider expressions use `?` for early return — is `?` inside the `{ R => expr? }` binding position allowed?

## Decision needed from author

Add a `PROVIDERS_BLOCK` grammar production to `docs/syntax.md` and resolve the `in`-keyword disambiguation question.


---

## F-061 ✓ RESOLVED

**Decision:** `with_provider[R]` is a compiler built-in that satisfies all effect atoms over `R` within the block: `reads(R)`, `writes(R)`, `sends(R)`, `receives(R)` are all consumed at the boundary. Only `E` (other effects) propagates to the caller. After the block exits, R-effects resume normal propagation (but no more R-accesses arise since the provider is torn down). Resolved alongside F-059 in the same paragraph.

**Changes:** `docs/design.md` § Provider-Rooted Resources — the "Effect satisfaction at the `with_provider` boundary" paragraph (added for F-059) covers this fully.

---

### Effect satisfaction at `with_provider` boundary — stopping rule undefined

**Section:** Feature 2 — Provider-Rooted Resources (line 4540)
**Tags:** gap

## Finding

The `with_provider` signature carries no `with` clause, yet its body calls `f` which has effect `R`. Under the "Annotations are verified, not trusted" rule, `with_provider`'s inferred body effects would include `R`, and the missing declaration would be a compile error.

The intended behavior is clearly that `with_provider` *satisfies* the resource effect: callers of `with_provider[UserDB]` do not need `writes(UserDB)` in their own signatures. But no formal rule states this:

1. Is `with_provider` a compiler built-in exempt from normal effect inference?
2. Is there a general rule: "installing a provider for resource R satisfies all effect atoms over R within the scope body"?
3. Does it satisfy all verbs on the resource (`reads`, `writes`, `sends`, `receives`)? Or only the verbs the closure actually performs?
4. After the block exits, do effects resume propagating? (They must, since the provider is no longer in scope — but this is not stated.)

The `providers { } in { }` block inherits this gap via its desugaring to nested `with_provider` calls.

## Decision needed from author

State the effect-satisfaction rule for `with_provider`: precisely, which effect atoms are "consumed" at the block boundary, and how the compiler models this during inference and verification.


---

## F-062 ✓ RESOLVED

**Decision:** `blocks` and `suspends` must be **explicitly declared** on public functions — they are not default-permitted. They change scheduling behavior observable to callers, so they must appear at every public boundary. Private functions still infer them transitively.

**Changes:** `docs/design.md` § Execution Effects — updated the intro paragraph to explicitly state that `blocks` and `suspends` must be declared on public functions (unlike `allocates(Heap)` which is default-permitted), with rationale.

---

### `blocks` and `suspends` declaration requirement on public functions is unstated

**Section:** Feature 2 — Execution Effects (line 3574)
**Tags:** gap

## Finding

`allocates(Heap)` is explicitly "default-permitted" — public functions in standard profiles need not declare it. No analogous rule is stated for `blocks` and `suspends`.

```kara
pub fn load_config(path: String) -> Result[Config, IoError]
    with reads(FileSystem)    // required
    // blocks?                // required? default-permitted?
{
    std.fs.read_sync(path)
}
```

In an async HTTP server, every request handler calls I/O primitives and would infer `suspends`. If `suspends` must be declared on every public function in such a codebase, the annotation burden is high. If `blocks` and `suspends` are default-permitted like `allocates(Heap)`, that should be stated. If they must be declared, guidance on the expected annotation density is needed.

## Decision needed from author

State whether `blocks` and `suspends` follow the "must declare on public functions" rule or the "default-permitted" exception. If "must declare," confirm this applies to all profiles including the standard web/server profile.


---

## F-063 ✓ RESOLVED

**Decision:** `@noblock` was a spec typo. Renamed to `#[noblock]` throughout, matching the universal `#[...]` attribute syntax. The block-level form becomes `#[noblock] extern "C" { ... }`.

**Changes:** `docs/design.md` — all `@noblock` occurrences replaced with `#[noblock]` including the annotation description, block-level form, and the FFI defaults table.

---

### `@noblock` attribute uses `@` syntax inconsistent with `#[...]`

**Section:** Feature 2 — Execution Effects, FFI leaves for `blocks` (line 3600)
**Tags:** syntax-gap

## Finding

The spec states FFI functions may opt out of the `blocks` default "with `@noblock`." Every other attribute in the language uses `#[...]` syntax: `#[repr]`, `#[derive]`, `#[test]`, `#[target]`, `#[interrupt]`, `#[allow(...)]`. The `@noblock` form is an unexplained outlier. Either:

1. It is a spec typo and should be `#[noblock]`, or
2. `@` introduces a distinct annotation category, which is undefined anywhere.

## Decision needed from author

Rename to `#[noblock]` (or `#[no_block]`) to match all other attributes, or define the `@` syntax and explain how it differs from `#[...]`.


---

## F-064 ✓ RESOLVED

**Decision:** Named key required (no anonymous key). Multi-dimensional keys are not supported in v1. Grammar: `RESOURCE_KEY = "[" IDENT ":" TYPE "]"` on resource declarations; `EFFECT_RESOURCE_REF = PATH "[" EXPR "]"` on effect atoms.

**Changes:** `docs/syntax.md` § 3.6 Effect Declarations — rewrote `EFFECT_RESOURCE` grammar with `RESOURCE_KEY` and `TRAIT_BOUND` non-terminals; added `EFFECT_ATOM` and `EFFECT_RESOURCE_REF` productions; updated examples to show parameterized resource usage in effect atoms.

---

### Parameterized resource effect atoms have no grammar production

**Section:** Feature 2 — Parameterized Resources (line 4470)
**Tags:** syntax-gap

## Finding

The spec uses parameterized effect atoms in function signatures and resource declarations without giving a grammar production:

```kara
effect resource UserDB[user_id: i64];

fn update_profile(id: i64) with writes(UserDB[id]) { ... }
```

No grammar production exists in the spec or `docs/syntax.md` for:
- `VERB "(" RESOURCE "[" EXPR "]" ")"` in effect atoms
- `"effect" "resource" IDENT "[" IDENT ":" TYPE "]" ...` in resource declarations

Open questions: Is the key name required (`[user_id: i64]`) or can it be anonymous (`[i64]`)? Are multi-dimensional keys (`effect resource Matrix[row: i64, col: i64]`) supported in v1?

## Decision needed from author

Add grammar productions for parameterized resource declarations and parameterized effect atoms to `docs/syntax.md`. State whether multi-dimensional keys are in scope for v1.


---

## F-065 ✓ RESOLVED

**Decision:** `alias` is a module-level keyword. `pub` visibility is supported. `alias` is NOT symmetric. RHS may be a fully-qualified external path. Grammar: `[ VISIBILITY ] "alias" PATH "=" PATH ";"`. Already present in `docs/syntax.md § 3.10`.

**Changes:** `docs/design.md` § Resource Aliasing — added `alias` declaration paragraph with visibility, symmetry, and full semantics. `docs/syntax.md` § 3.10 — updated grammar to add `VISIBILITY`, added explanatory text, and examples showing `pub` form.

---

### `alias` declaration for resource aliasing has no grammar production

**Section:** Feature 2 — Resource Aliasing (line 4691)
**Tags:** syntax-gap

## Finding

The spec mentions: `alias mylib.UserDB = theirlib.TheirDB;` in one line with no further detail. No grammar production, no visibility rules, no module placement, and no scoping rules exist. Open questions:

1. Is `alias` a new reserved keyword at module level?
2. Can it be `pub`? Does it affect downstream consumers of the module?
3. Is `alias` symmetric — does `alias A = B` imply `alias B = A`?
4. Can the right-hand side be a fully-qualified path from an external dependency?

## Decision needed from author

Add a grammar production for `alias` to `docs/syntax.md` and define its visibility and scoping rules.


---

## F-066 ✓ RESOLVED

**Decision:** `independent A, B;` declares two resources as statically disjoint; overrides the conservative `--strict-effects` same-alias assumption. Not symmetric; may be `pub`. Trusted, not verified. Grammar already in `docs/syntax.md § 3.10`.

**Changes:** `docs/design.md` § Resource Aliasing — added `independent` declaration paragraph with syntax, placement, semantics, and trust-not-verify note. `docs/syntax.md` § 3.10 — updated grammar and examples.

---

### `independent` declaration referenced in `--strict-effects` is never defined

**Section:** Feature 2 — Resource Aliasing (line 4693)
**Tags:** gap

## Finding

The spec states `--strict-effects` mode "refuses to auto-parallelize across module boundaries without explicit `alias` or `independent` declarations." `independent` receives no definition — no syntax, no semantics, no examples.

It is unclear what `independent` asserts: that two resources do not alias? that two functions are safe to parallelize? that a specific effect atom is known-distinct from another? Whether it is a module-level declaration, a function attribute, or a call-site annotation is not stated. Whether the compiler checks it or trusts it (like FFI annotations) is also unspecified.

## Decision needed from author

Define the `independent` declaration: syntax, placement, semantics, and what the compiler extracts from it. Or explicitly mark it deferred if it is aspirational syntax.


---

## F-067 ✓ RESOLVED

**Decision:** Interpretation B — **transitively reachable**. Any `reads(Clock)`, `reads(RandomSource)`, or `reads(Env)` anywhere in the transitive call graph of a transparent-verb function is non-propagating. This prevents the one-level-indirection fragility of Interpretation A.

**Changes:** `docs/design.md` § Transparent-verb carve-out for observability — added "transitively" to the rule, with an explanation of why Interpretation A is fragile and the transitive rule is correct.

---

### Transparent-verb carve-out for nondeterminism: direct vs transitive scope ambiguous

**Section:** Feature 2 — Nondeterminism as an Explicit Resource (line 3966)
**Tags:** ambiguity

## Finding

The spec states: "`reads(Clock)`, `reads(RandomSource)`, and `reads(Env)` performed inside a transparent-verb operation are themselves transparent and do not propagate."

"Performed inside a transparent-verb operation" is ambiguous about depth:

**Interpretation A — direct only.** Only clock/random/env reads in the body of a transparent-declared function are non-propagating. If the transparent function calls a non-transparent private helper that reads the clock, `reads(Clock)` from the helper propagates to the transparent function and — since only the transparent verb atom is erased, not all effects — `reads(Clock)` propagates further to the transparent function's callers.

**Interpretation B — transitive.** Any clock/random/env read in the transitive call graph of a transparent function is non-propagating, regardless of whether the direct callee is itself transparent.

```kara
fn clock_helper() -> Instant { std.time.now() }   // non-transparent private function

pub transparent effect verb traces;

fn trace_ts(msg: String) with traces(Console) {
    let now = clock_helper();   // indirect reads(Clock)
    eprintln(f"{now}: {msg}");
}

fn process(data: Data) {
    trace_ts("start");
    // Under (A): process infers reads(Clock)
    // Under (B): process infers nothing extra
}
```

Under (A), the carve-out is fragile — adding one level of indirection breaks it. Under (B), the compiler must propagate transparency transitively, which is a novel inference rule not stated elsewhere.

## Decision needed from author

State whether "performed inside a transparent-verb operation" means (A) direct calls only or (B) transitively reachable calls. If (A): note the one-level-indirection fragility. If (B): describe how the compiler propagates transparency transitively.


---

## F-068 ✓ RESOLVED

**Decision:** `FIELD_PATTERN = IDENT | IDENT ":" PATTERN | ".."`. Shorthand `IDENT` is equivalent to `IDENT: IDENT`. Arbitrary nesting valid. `{ .. }` alone valid. `{ field, .. }` valid; `..` must be last. Trailing comma before `..` optional.

**Changes:** `docs/syntax.md` § 5.6 Patterns — added `FIELD_PATTERN` non-terminal definition with notes. Also added `FIELD_PATTERN` trailing-comma tolerance to the struct destructure production.

---

### `FIELD_PATTERN` non-terminal is undefined in syntax.md

**Section:** Feature 3 — Pattern matching (syntax.md §5.6 Patterns, design.md line 5509)
**Tags:** syntax-gap

## Finding

The `PATTERN` grammar uses `FIELD_PATTERN` at line 1143 of `syntax.md`:

```
PATTERN = ...
        | IDENT "{" FIELD_PATTERN { "," FIELD_PATTERN } "}"  // struct destructure
```

`FIELD_PATTERN` is never defined anywhere in `syntax.md` or `design.md`. From examples,
struct patterns appear to support three forms:

1. **Shorthand** — `{ field }` where the field name serves as both the pattern test and
   the created binding:
   ```kara
   match event { MouseDown { x, y } => handle(x, y) }
   ```
2. **Named sub-pattern** — `{ field: pattern }` where the field is matched against an
   inner pattern:
   ```kara
   match response { Response { status: code @ 500..=599, body } => log_error(code, body) }
   ```
3. **Rest wildcard `..`** — ignores all fields not explicitly listed:
   ```kara
   match shape { Circle { .. } | Ellipse { .. } => "round" }
   ```

All three forms appear in examples (`syntax.md:1168`, `1180`, `1234`; `design.md:5523`,
`5624`) but none are derivable from the grammar alone.

Open questions that `FIELD_PATTERN` must answer:
- Is `{ field }` always valid, or does it require the field name to match a local binding?
- Is arbitrary nesting (`{ a: Foo { b } }`) valid?
- Is `{ .. }` alone (match-everything) valid?
- Is `{ field, .. }` (match some fields, skip rest) valid?
- Is trailing comma required, optional, or forbidden before `..`?

## Decision needed from author

Define `FIELD_PATTERN` in `syntax.md §5.6`. Likely:

```
FIELD_PATTERN = IDENT                      // shorthand: field_name ≡ field_name: field_name
              | IDENT ":" PATTERN          // named sub-pattern
              | ".."                       // skip remaining fields (must appear last)
```

Add a note confirming that trailing `..` is a rest wildcard, not a range operator.


---

## F-069 ✓ RESOLVED

**Decision:** Production: `"[" [ PATTERN { "," PATTERN } [ "," ".." IDENT ] ] "]"`. `..rest` must be last. Irrefutable on `Array[T, N]`; refutable on `Vec[T]`. `[]` irrefutable on `Array[T, 0]`, refutable on `Vec[T]`.

**Changes:** `docs/syntax.md` § 5.6 Patterns — added array/slice pattern production to `PATTERN` grammar with notes on refutability and rest-segment constraints.

---

### Array/slice patterns missing from `PATTERN` grammar

**Section:** Feature 3 — Pattern matching (syntax.md §5.6, design.md line 5665–5667)
**Tags:** syntax-gap

## Finding

`design.md` describes two categories of patterns on collection types:

- `Array[T, N]` fixed-size patterns: "array pattern `[p1, p2, …, pN]` specializes exactly
  like a tuple" (line 5665)
- `Vec[T]` slice patterns with a rest: "Slice patterns with a rest (`[first, ..rest]`)
  cover all non-empty cases but still require `[]` or `_` to cover the empty case" (line
  5667)

Neither appears in the `PATTERN` grammar in `syntax.md §5.6`. The grammar has no
`"[" PATTERN { "," PATTERN } [ "," ".." IDENT ] "]"` production.

Adversarial programs with no grammar support:

```kara
let [a, b, c] = my_array;           // Array[T, 3] pattern
match vec { [first, ..rest] => ..., [] => ... }  // Vec slice pattern
match triple { [x, y, z] => x + y + z }          // tuple-like array pattern
```

Without a grammar production, these forms are impossible to parse, yet the spec's
exhaustiveness section presupposes they work.

## Decision needed from author

Add an array/slice pattern production to `syntax.md §5.6 PATTERN`:

```
| "[" [ PATTERN { "," PATTERN } [ "," ".." IDENT ] ] "]"   // array/slice pattern
```

Specify whether `..rest` is required to be the last element, whether it is allowed in
`let` patterns (irrefutable? refutable?), and whether `[]` is an irrefutable pattern on
`Array[T, 0]` but refutable on `Vec[T]`.


---

## F-070 ✓ RESOLVED

**Decision:** Grammar is authoritative. `design.md` range patterns section updated to document all five forms: `lo..=hi`, `lo..hi`, `lo..`, `..=hi`, `..hi`. Half-open patterns cannot alone cover a finite domain — wildcard still required.

**Changes:** `docs/design.md` § Range Patterns — replaced the "using `..=`" only description with a full five-form table plus an examples block with the half-open example. `docs/syntax.md` already had the five-form grammar (was already correct).

---

### Range pattern forms: `design.md` (inclusive only) contradicts `syntax.md` (all forms)

**Section:** Feature 3 — Range Patterns (design.md line 5580, syntax.md §5.6 RANGE_PATTERN)
**Tags:** contradiction

## Finding

**`design.md` (lines 5580–5604):**

> Range patterns match a contiguous inclusive range of values **using `..=`**:
> ```kara
> match c { 'a'..='z' => ... }
> ```
> Rules:
> - **Both bounds** must be the same type and must be integer or `char` literals.
> - The **start bound** must be less than or equal to the **end bound**.

This text implies that only bounded inclusive ranges (`lo..=hi`) are valid patterns.

**`syntax.md §5.6` (lines 1148–1212):**

```
RANGE_PATTERN = LITERAL ".."  LITERAL    // exclusive, [lo, hi)
              | LITERAL "..=" LITERAL    // inclusive, [lo, hi]
              | LITERAL ".."             // unbounded above, [lo, ∞)
              |          ".."  LITERAL   // exclusive end, (∞, hi)
              |          "..=" LITERAL   // inclusive end, (∞, hi]
```

With the example:
```
match n {
    ..=-1   => "negative",
    0       => "zero",
    1..=9   => "single digit",
    10..    => "large",
}
```

The two documents directly contradict each other. `design.md` says "both bounds required"
and "using `..=`"; `syntax.md` provides five forms including exclusive `..`, half-open
`lo..` and `..hi`, and shows a working example.

Specific contradictions:
1. Exclusive bounded `5..10` — invalid per design.md ("using `..=`"), valid per grammar.
2. Half-open `10..` — invalid per design.md ("both bounds"), valid per grammar.
3. Half-open `..=-1` — invalid per design.md ("both bounds"), shown in syntax.md example.

## Decision needed from author

Reconcile the two documents. Options:
1. **grammar is authoritative:** Update `design.md §Range Patterns` to document all five
   forms with semantics for each (exclusive `[lo, hi)`, half-open behaviour, etc.).
   Add exhaustiveness rules for half-open patterns (they cannot cover a finite domain
   alone — a wildcard is still required).
2. **design.md is authoritative:** Remove the exclusive and half-open forms from
   `syntax.md`, restrict the grammar to `LITERAL "..=" LITERAL`, and add a note that
   half-open patterns are deferred.


---

## F-071 ✓ RESOLVED

**Decision:** Match arm bindings follow the same per-binding inference rule as destructuring parameters, extended by scrutinee mode: owned scrutinee → bindings may move; `ref T` scrutinee → all bindings automatically borrow (match ergonomics). Partial moves leave the scrutinee partially moved. Explicit `ref` on a binding is supported. `ref T` scrutinee propagates the borrow transitively (all bindings at any nesting depth).

**Changes:** `docs/design.md` § Feature 4 — added a new "Match Arm Binding Modes" section before Part 1½ with the full rule, examples, and explicit `ref` annotation support.

---

### Pattern binding mode for match arm bindings — unspecified

**Section:** Feature 3 — Pattern matching / Feature 4 — Ownership (design.md lines 5509, 5749)
**Tags:** gap, interaction

## Finding

The use-predicate table (design.md line 6016) classifies `match v { ... }` as:

> `match v { ... }` (match scrutinee) | read (with consume propagated to arms that move bindings)

The parenthetical says that *if arms move bindings*, the whole match is a consume of `v`.
But "move bindings" is never defined for match arm patterns.

The parameter-destructuring section (line 3725) defines the rule for function parameters:

> **Mode inference is per-binding, not per-parameter.** When a struct is destructured, each
> extracted binding is analysed independently. If `x` is only read and `y` is consumed, the
> compiler infers `ref` for `x` and `own` for `y`. **Mixed usage:** when at least one
> binding is consumed, the parameter as a whole is treated as `own`.

This rule is stated exclusively for **function parameters**. It is never extended to
match arm bindings.

Adversarial programs that expose the gap:

```kara
fn f(val: Foo) {
    match val {
        Foo { field } => use_owned(field),  // does field move out? does val get consumed?
    }
}

fn g(val: ref Foo) {
    match val {
        Foo { field } => use_ref(field),    // field: ref String? or compile error?
    }
}

fn h(val: Foo) -> String {
    match val {
        Foo { name, .. } => name,           // moves name, ignores rest — valid?
    }
}
```

The gap has three components:
1. **Inference rule:** Is the parameter-destructuring per-binding inference rule (mode
   from subsequent usage) also the rule for match arm bindings?
2. **Scrutinee borrow propagation:** If the scrutinee is `ref Foo`, do all arm bindings
   automatically borrow from it (becoming `ref T` fields)? Or is matching on `ref Foo`
   invalid unless the pattern fully borrows?
3. **Partial consume:** Can a pattern move some fields of a struct while leaving others,
   and if so, does the scrutinee variable become "partially moved" (making it unusable
   below the match)?

## Decision needed from author

Extend the ownership section to cover match arm binding modes. The natural rule (analogous
to parameter destructuring) would be:

> **Match arm binding mode** follows the same per-binding inference rule as destructuring
> parameters: each pattern-created binding is `own` if subsequently consumed, `ref` if only
> read. When at least one binding is consumed, the scrutinee itself must be owned (bare `T`);
> if all bindings are only read, the scrutinee may be `ref T`.

For `ref T` scrutinees: pattern bindings should automatically borrow (yielding `ref FieldType`
for struct fields, `ref VariantPayload` for enum variant payloads) — analogous to how Rust
2021 match ergonomics work. Confirm or deny this, and specify the full rule.


---

## F-072 ✓ RESOLVED

**Decision:** `bool` is treated as a two-constructor type. Matching both `true` and `false` is exhaustive without a wildcard. Matches Rust's behavior and user expectation.

**Changes:** `docs/design.md` § Pattern Exhaustiveness — added a `bool` entry at the top of the type-specific rules, before the `Enums` entry.

---

### `bool` exhaustiveness not specified

**Section:** Feature 3 — Pattern Exhaustiveness (design.md line 5657)
**Tags:** ambiguity

## Finding

The type-specific exhaustiveness rules enumerate: enums, structs/tuples, integers/floats/
strings/`char`, `Array[T, N]`, `Vec[T]`/`Map`/`String`, `Never`, refinement types, distinct
types, and `shared struct`/`shared enum`. `bool` is not mentioned.

`bool` has exactly two values (`true` and `false`). A match that covers both is intuitively
exhaustive:

```kara
match flag {
    true  => "yes",
    false => "no",   // exhaustive — no wildcard needed?
}
```

But `true` and `false` are `LITERAL` values in the grammar, not enum variants. The
exhaustiveness algorithm's "enum" rule applies to types whose constructor space is the set of
declared variants. If `bool` is not declared as `enum Bool { false, true }` (or equivalent),
it falls into the "integer" bucket — which requires a wildcard.

Two possible interpretations:
1. **`bool` is a two-variant enum.** `true` and `false` as literals cover both variants; the
   match above is exhaustive. This is what most users expect.
2. **`bool` is a literal type.** Matching on `true` and `false` is literal pattern matching;
   a wildcard is required. `match flag { true => ..., false => ..., _ => unreachable!() }`
   is the correct form.

If interpretation 1, the spec must state that `bool` participates in exhaustiveness like a
two-variant enum. If interpretation 2, the spec should note the mismatch with user expectation
and explain why (for consistency with the general literal rule).

## Decision needed from author

State how `bool` participates in the exhaustiveness algorithm. Add a `bool` entry to the
type-specific rules in §Pattern Exhaustiveness: either as a two-variant "enum" (exhaustive
without wildcard when both literals appear) or as a literal type (wildcard always required).


---

## F-073 ✓ RESOLVED

**Decision:** `mut ref self` and bare `self` are compile errors on `shared struct` methods, matching the explicit rule for `par struct`. Diagnostics provided for both. Interior mutation goes through per-field borrow flags (already specified) or `Mutex[T]` + `lock` blocks.

**Changes:** `docs/design.md` § Part 5: Shared Types — updated the "`self` in methods" resolved-semantics bullet to state that `mut ref self` and bare `self` are compile errors with specific diagnostics.

---

### `mut ref self` on `shared struct` methods — implicit prohibition with no diagnostic

**Section:** Feature 4 — Part 5: Shared Types (design.md line 6163, 6248)
**Tags:** missing-error

## Finding

The spec states:

> **`self` in methods — always a shared reference (RC increment), never consumed**

This means `shared struct` methods always take `ref self`. But the spec never says that
`mut ref self` is a compile error on `shared struct` methods, what the diagnostic says,
or whether `bare self` (consuming receiver) is also forbidden.

Semantically, `mut ref self` requires *exclusive* ownership of the receiver for the
duration of the call. But a `shared struct` value may have multiple RC holders — exclusive
ownership is impossible to guarantee. `mut ref self` on `shared struct` is therefore
fundamentally unsound and must be rejected.

The same applies to bare `self` (consuming receiver): consuming an RC-allocated value
through a method call is also unsound if other holders remain.

Adversarial programs:

```kara
shared struct Node { val: i64 }

impl Node {
    fn mutate(mut ref self) { self.val = 42 }   // ERROR? diagnostic?
    fn consume(self) -> i64 { self.val }         // ERROR? diagnostic?
}
```

The spec does not provide:
1. The error code or message for `mut ref self` / bare `self` on `shared struct`
2. Whether this is a compile error at the `impl` site or at call sites
3. Whether `par struct` has the same restriction (its resolved semantics say "`ref self` only"
   with "a compile error" noted — this is specified for `par struct` but not for `shared struct`)

Note: `par struct` explicitly states "Methods on `par struct` accept only `ref self` receivers.
`mut self` — which requires exclusive ownership — is a compile error" (line 6300). The same
rule is not stated for `shared struct`.

## Decision needed from author

Extend the `shared struct` resolved semantics bullet on `self` to match `par struct`:
state that `mut ref self` and bare `self` receivers are compile errors on `shared struct`
methods, with a diagnostic explaining why (RC prevents exclusive or consuming access). The
`mut` field mutation story is already covered by the interior-mutability / borrow-flag
mechanism — `mut ref self` is not the path for that.


---

## F-074 ✓ RESOLVED

**Decision:** Option 1 — auto-generated per-type resource. The compiler generates `writes(TypeName)` for `pub fn` methods that mutate `mut` fields, matching the `par struct` rule. A `pub fn` method on `Counter` that mutates fields must declare `with writes(Counter)`.

**Changes:** `docs/design.md` § Part 5: Shared Types — updated the "Effects (external mutation)" resolved-semantics bullet to specify `writes(TypeName)` auto-generated resource, referencing the same mechanism as `par struct`.

---

### `shared struct` external mutation — no effect resource specified

**Section:** Feature 4 — Part 5: Shared Types (design.md line 6252–6253)
**Tags:** gap

## Finding

The spec states:

> **Effects (external mutation)** — from outside the project (end users), direct field
> writes to `pub mut` fields are disallowed; mutation must go through a `pub fn` method
> that declares `writes(...)` effects

But no rule specifies *what resource* the `writes(...)` effect names for a `shared struct`
method. The effect system uses user-defined resources (`writes(UserDB)`, `writes(OrderDB)`)
that must be explicitly declared with `effect resource R`. The spec does not say:

1. Whether the compiler auto-generates an effect resource for each `shared struct` type
2. Whether library authors must manually declare an `effect resource` for their type
3. What happens if a `pub fn` method mutates `shared struct` fields but declares no effects

In contrast, `par struct` has an explicit (if conservative) rule (line 6315): "all `mut`
field accesses on a `par struct` attribute to a single `writes(T_resource)` effect for
the containing type." This rule auto-generates a per-type resource for `par struct`.

No analogous rule exists for `shared struct`.

Adversarial program:

```kara
// Library code
shared struct Counter { pub mut val: i64 }

impl Counter {
    pub fn increment(ref self) {
        self.val += 1   // mutates val — what effect does this contribute?
    }
}

// Caller
pub fn do_work(c: Counter) {
    c.increment()   // what effect does the caller acquire?
}
```

Without knowing what effect `increment` contributes, callers of `do_work` cannot reason
about whether two calls to `do_work` on the same `Counter` will be parallelized or
serialized.

## Decision needed from author

Specify the effect rule for `shared struct` mutation. Options:
1. **Auto-generated per-type resource:** the compiler generates `writes(Counter)` (using
   the struct's name as the resource) for methods that mutate `mut` fields — matching the
   `par struct` rule.
2. **Library author declares:** `shared struct Counter` methods that mutate fields carry
   no inferred effect; the library author is responsible for declaring a resource if they
   want callers to see the effect. If no resource is declared, mutations are effect-invisible
   to the effect system.
3. **Internal mutation is always effect-invisible:** consistent with "within the project,
   mutating mut fields does not require an effect annotation" — the pub boundary might just
   propagate this absence, making `increment` always pure from the effect system's view.

Option 3 would be consistent with the "within the project" rule but would mean the effect
system cannot reason about `shared struct` mutation ordering — a notable constraint.


---

## F-075 ✓ RESOLVED

**Decision:** `lock` is valid on any `Mutex[T]` binding, not only on `par struct`/`shared struct` fields. Grammar: `lock IDENT [IDENT] BLOCK` (in `docs/syntax.md § 5.10`, already present). Positional alias has type `mut ref T`. `lock` is a reserved keyword. `lock EXPR` (arbitrary expression) is not valid — only `lock IDENT`.

**Changes:** `docs/design.md` § Part 5: Shared Types — added "Standalone `Mutex[T]` values" paragraph to the concurrency synchronization bullet, confirming standalone usage, documenting alias type (`mut ref T`), and pointing to the grammar.

---

### `lock` block syntax for standalone `Mutex[T]` — only shown on `par struct` fields

**Section:** Feature 4 — Part 5b: Concurrent Shared Types (design.md line 6242–6247)
**Tags:** gap, syntax-gap

## Finding

The `lock` block syntax is introduced in the `shared struct` section (used for single-task
mutual exclusion) and expanded in `par struct`:

```kara
lock node { node.val = 42 }
lock node { node.left = Some(child); node.right = None }
lock node n { n.val = 42 }
```

All examples show `lock IDENT { ... }` where `IDENT` is a `shared struct` or `par struct`
value. The spec mentions "`Mutex[T]` + `lock` block syntax remains available within `par
struct` field types and for any single-task mutual exclusion need" (line 6242) — the phrase
"for any single-task mutual exclusion need" implies standalone `Mutex[T]` values are supported.

But no grammar production for `lock` exists in `syntax.md`, and no example shows locking a
standalone `Mutex[T]` that is not a `par struct` field:

```kara
let counter: Mutex[i64] = Mutex.new(0);
lock counter v { v += 1 }   // is this valid? what is v's type?

// Or:
fn with_lock(m: ref Mutex[i64]) {
    lock m v { v += 1 }    // mut ref i64? or something else?
}
```

Open questions:
1. Is `lock` syntax allowed on any `Mutex[T]` value, or only on `par struct` / `shared struct`
   fields?
2. What is the type of the positional alias (`v` in `lock m v { ... }`) — `mut ref T`?
3. What is the grammar production for `lock`?
4. Is `lock` a keyword, or is it a contextual identifier?

## Decision needed from author

Add a grammar production for `lock` to `syntax.md` and show a standalone `Mutex[T]` example
to confirm the "any single-task mutual exclusion need" claim. Specify whether `lock EXPR ...`
(arbitrary expression) or only `lock IDENT ...` is valid, and what the type of the positional
alias is inside the block.


---
