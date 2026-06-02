# 71. Accumulating Diagnostics — Primitive Shape and v1 Position

**Status:** Open. Seeded 2026-06-01.

**Trigger:** v70 Section B's compiler-as-lens audit surfaced "accumulating diagnostic channel" as a real gap with backend-workload overlap. Triaged in [v70 Section B5](v70_arena_handle_v1_surface.md#problem-b5-triaged-gap-list) as out-of-scope for v70 but in-scope for v1 (multi-field form validation, batch import validation, parser error accumulation, HTTP request validation — every workload that collects many errors before bailing). This brainstorm opens the design question and seeds the workload lenses; synthesis deferred until needed (likely after v70's Section A primitives land and the consumer shape is real enough to test against).

This brainstorm decides:
- What shape an accumulating diagnostic channel takes — language-level primitive, stdlib type, or a convention over `Vec[Diagnostic]`.
- Whether the primitive composes with `?`-propagation, `errdefer`, and `collect_all`.
- The v1 / v1.x / deferred triage under the priority-tier discipline.

Per stored priority-tier definitions, **v1 = P0 + P1**.

---

## Problem 1. What "accumulating diagnostics" is, and where it lives in real code

The shape: a piece of code wants to report *many* errors before bailing, not fail-fast on the first one. The user typing a form should see *all* invalid fields, not be told to fix one, resubmit, see the next, fix it, resubmit. A parser typing 20 lines of broken code should see *all* the type errors, not just the first. A batch import of 10,000 rows should report *every* rejected row.

This pattern is structurally distinct from fail-fast `?`-propagation:

```kara
// Fail-fast (existing): first Err bails immediately, caller sees only the first failure.
fn validate_signup(form: SignupForm) -> Result[ValidatedForm, ValidationError] {
    let email = validate_email(form.email)?;
    let password = validate_password(form.password)?;
    let username = validate_username(form.username)?;
    Ok(ValidatedForm { email, password, username })
}

// Accumulating (gap): every check runs; caller sees every failure at once.
fn validate_signup_all(form: SignupForm) -> Result[ValidatedForm, Vec[ValidationError]] {
    // What shape goes here?
}
```

The naive shape (`Vec[Diagnostic]` threaded through every function returning `Result[T, Vec[E]]`) works but is verbose, doesn't compose with `?`, and pushes the error-accumulation discipline onto every function in the call chain. Real codebases either invent ad-hoc accumulators or reach for a library.

**The workloads that want this primitive (backend overlap from v70 B5):**

1. **Multi-field form validation** — every web app login / signup / settings form.
2. **Batch import validation** — CSV / JSON / protobuf bulk loaders that need to report every rejected row.
3. **Parser error recovery** — every parser in every compiled-language stdlib (markdown, regex, SQL, .proto, .kara itself).
4. **HTTP request validation** — middleware that accumulates header / query / body errors.
5. **Configuration file validation** — yaml/toml/json config loaders reporting every misconfiguration.
6. **Linter / type-checker / static-analysis pass output** — Kāra's own compiler diagnostics surface, plus any user-built static analysis tool.

All six are mainstream backend workloads. None is compiler-only. The triage discipline (a gap is in-scope only if backend workloads also want it) is comfortably met.

---

## Problem 2. Candidate shapes

To be filled in when the brainstorm goes deep. Seed candidates:

**Candidate 1 — Convention over `Vec[Diagnostic]`.** No new primitive. The stdlib documents an idiom: functions that want to accumulate return `Result[T, Vec[E]]`, and the call site uses a helper combinator like `accumulate(...)` (similar to `collect_all`) that runs multiple validators and unions their error vectors.

*Pro:* zero new primitive surface; uses existing types; semver-stable.

*Con:* `?` doesn't compose (`Vec[E]` doesn't naturally chain into a parent `Vec[E]` without flattening); every function in the call chain pays verbosity.

**Candidate 2 — `Diagnostics[E]` stdlib type with `accumulate()` combinator.** Stdlib type that wraps the accumulation pattern, with operations to add errors, flatten nested accumulators, and either bail-or-emit at the boundary.

*Pro:* one named primitive; composes via a documented chain operator; `Diagnostics[E]` can carry rich context (source spans, suggested fixes) beyond a plain `Vec[E]`.

*Con:* two parallel error stories (`Result[T, E]` for fail-fast, `Result[T, Diagnostics[E]]` for accumulating) — users need to know when to reach for which.

**Candidate 3 — Effect-system-level diagnostic resource.** Diagnostics surface as `writes(Diagnostics)` on functions that emit them. The effect system carries the accumulation; the function signature just says "may emit diagnostics" and the runtime collects them at the boundary of a `with_provider[Diagnostics]` scope.

*Pro:* zero ceremony at the function signature beyond effects (which the language already requires for I/O); naturally composes; the boundary is explicit and the failure mode (the function emitted diagnostics, did we ever consume them?) becomes a compile-time check.

*Con:* extends the effect system with a new builtin resource; the `Diagnostics` provider is genuinely new vocabulary; might overlap or conflict with other planned effect-system surfaces.

**Candidate 4 — Language-level `accumulating` block.** New syntactic form `accumulating diag { ... }` that scopes a diagnostic-collection region. Inside the block, `?` accumulates rather than propagates; at the block's end, the accumulated vector is the block expression value.

*Pro:* most ergonomic at the use site; the contrast with fail-fast is structural (different keyword, different shape).

*Con:* largest language-surface addition; needs a grammar slot; parallel to `seq { }` and `par { }` but at a different semantic level (error handling, not concurrency).

---

## Problem 3. Composition questions to answer in synthesis

Each candidate must answer:

- **How does it compose with `?`?** Does `?` inside an accumulator become "accumulate this error, continue" or "this branch can't produce a meaningful value, accumulate and short-circuit *this branch*"?
- **How does it compose with `errdefer`?** When the accumulator bails at the boundary because some errors accumulated, does `errdefer(e)` bind `e` to the vector? To the first error?
- **How does it compose with `collect_all`?** `collect_all` already wants every branch to run regardless of failures. The natural pairing is `collect_all_into[Diagnostics]` that flattens branch failures into the accumulator. Is that automatic, or a separate combinator?
- **How does it interact with `par {}` block-piercing `?`?** Inside a `par {}`, `?` pierces to the enclosing function. Does an accumulating-`?` pierce to the enclosing accumulator instead? What if there are nested accumulators?
- **How does the user know when to use which?** Teaching surface: fail-fast for "this whole operation is invalid if any part is," accumulating for "report every problem so the user can fix them all at once." The split is real and not always obvious at the call site.

---

## Problem 4. Priority tier — seed positions, not decisions

The triage question is whether this primitive is **v1 (P0/P1)** — ships at launch — or **v1.x** — a documented gap that lands in a subsequent release.

**Argument for v1 (P1):**
- Six independent backend workloads want it (form validation, batch import, parser, HTTP middleware, config loader, linter/typechecker). Each has its own ad-hoc accumulator today.
- Kāra's compiler itself is one of the consumers — every typechecker, effect-checker, ownership-checker, parser already accumulates diagnostics through ad-hoc paths.
- A v1.x landing means every v1 backend workload invents its own pattern, fragmenting the ecosystem at the moment when standardization is highest-value.
- Consistent with v64's backend-first claim: if forms-and-batch-imports are real v1 workloads, the primitive they all want belongs at v1.

**Argument for v1.x:**
- The four candidate shapes are genuinely different; picking the wrong one and shipping it is worse than waiting.
- v70's Section A primitives (`Arena[T]`, `Symbol`/`Interner`) are already P1 additions to the v1 floor. Adding another P1 primitive expands scope.
- The compiler is the canonical consumer; the compiler already accumulates diagnostics through its own pattern, so the language doesn't need the primitive to ship.
- Real-world feedback from the first wave of stdlib consumers (`std.http` middleware, `std.json` validator hooks) would inform the candidate choice better than upfront design.

Neither argument is settled here. The synthesis pass decides.

---

## Cross-references

- [v70 Section B5](v70_arena_handle_v1_surface.md#problem-b5-triaged-gap-list) — surfaced this gap; flagged it as v71 scope.
- [v70 Section B2](v70_arena_handle_v1_surface.md#problem-b2-walkthrough--parser) — parser walkthrough that surfaced error accumulation as one of five parser needs.
- [v70 Section B3](v70_arena_handle_v1_surface.md#problem-b3-walkthrough--typechecker) — typechecker walkthrough that surfaced accumulation extended with rich diagnostics + suggested fixes.
- [v64 backend-first positioning](v64_backend_first_v1_concurrency.md) — the framing under which "backend workloads want this" implies v1 inclusion.
- [docs/design.md § AI-First Compiler Interface](../../docs/design.md#ai-first-compiler-interface) — the existing diagnostic-emission surface in the compiler itself; the user-facing primitive should compose with or live alongside this.

## Status

Open. No synthesis. Decision deferred until either (a) v70 follow-ons land and consumer shape clarifies, or (b) a v1 backend workload (form validation in `std.http`?, parser in `std.json`?) blocks on the primitive's absence.
