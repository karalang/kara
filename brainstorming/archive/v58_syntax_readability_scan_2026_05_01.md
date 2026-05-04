# Syntax Readability Scan — 2026-05-01

Scope: a pass over `docs/syntax.md` (full read) and `docs/design.md` (TOC + targeted sections — Identifiers and Naming, Effect Annotations, Provider Injection, Lock/Seq/Par blocks) plus `docs/glossary.md` (full read), with the lens: *which surface forms could read more like prose without sacrificing the precision the effect/ownership system needs?*

Lens-setting (carried over from a thought-experiment conversation about Python and curly braces): the natural-language feel of a language correlates with **(a) word operators over symbols** and **(b) how much low-level mechanism is forced into the syntax**. Brace-vs-indent is selection bias; word operators and ceremony reduction are the real levers. Kāra is constrained on (b) — effects, ownership tiers, and trait bounds *are* mechanism that must be visible. The realistic ceiling is Kotlin/Swift territory, not Python territory. Findings below stay inside that ceiling.

---

## What Kāra already gets right (do not touch)

Cataloged so future readers don't propose "fixing" these:

- **`[T]` for generics, no turbofish.** Already a prose-leaning win over `<T>` and `::<T>`. (`syntax.md` §6.4)
- **`for x in xs { }`.** Sentence-shaped. (`syntax.md` §5.7)
- **`with reads(Db)` effect clause.** The single keyword `with` is the universal "effects start here" marker — reads as English. (`syntax.md` §7)
- **`lock m { ... }` block.** No `.lock()` method, no guard values. Scope is visible from braces. (`syntax.md` §5.10)
- **No `async`/`await`, no `.await`, no colored functions.** Effects propagate; calls look like calls. This is Kāra's biggest readability win over Rust/JS and should never be re-litigated.
- **`defer` / `errdefer` keywords.** Word-keyword, not punctuation. (`syntax.md` §4)
- **Implicit modules from directory tree.** No `mod` declarations to maintain. (`design.md § Three-Level Visibility`)
- **Case-class identifier system.** Compiler-enforced `PascalCase` / `UPPER` / `snake_case` removes per-project bikeshed and gives readers one mental model. (`design.md § Identifiers and Naming`)
- **Single-colon path access.** `Vec:new` / `Result.Ok` is shorter than Rust's `::`. Disambiguation rides on the case-class invariant — no ambiguity cost. (`design.md`, same section)
- **`seq { }` and `par { }` blocks** as opt-out / explicit forms. Three letters each, but the keywords are domain-standard.

---

## Findings, ranked by impact

### F1. `&&` / `||` / `!` → `and` / `or` / `not` (high impact, low cost)

**Location.** `syntax.md` §1.2 (Symbols) lines 122–123 list `&&`, `||`, `!` as logical operators.

**Why this is the highest-impact item.** Word operators over symbol operators is the single strongest correlate of "reads like English" across languages — see Python, Ruby, Lua, SQL. Every `&&` your eye parses is a small symbol-decode step; every `and` is just a word. In a typical condition like:

```
if user.is_active && !user.is_banned && user.age >= 18 && user.country == "US" { ... }
```

vs.

```
if user.is_active and not user.is_banned and user.age >= 18 and user.country == "US" { ... }
```

the second reads as a sentence. In code review, sentence-shaped conditions are easier to scan for negation bugs (the `!` in front of `user.is_banned` is genuinely easy to miss; `not` is not).

**Cost.** Tiny.
- `and`, `or`, `not` are **not currently keywords** (verified against `syntax.md` §1.1) — they are available identifiers today. Reserving them would be a one-time breaking change for any code that uses them as identifiers. A grep over the in-repo `examples/` and stdlib scaffolding would tell us the real blast radius; based on the case-class rule (`and` / `or` / `not` are Value-class so could only collide with function/binding/parameter names) a quick mechanical rename would handle all collisions.
- Bitwise `&`, `|`, `^` stay as symbols (they are different operators, used much more rarely, and the symbol form is universal). No conflict.
- Precedence: `not` would bind tighter than `and`/`or`, same as `!`/`&&`/`||` today. No re-think needed.

**Recommendation.** Make `and`, `or`, `not` keywords; deprecate `&&`, `||`, `!` (keep parsing them with a `karac fix` migration warning for one release). Lowest-cost, highest-impact change in this list.

**Open question for the user.** Do you want short-circuit `&&=` / `||=` compound assignment? Currently absent from the symbol list (`syntax.md` §1.2) — neither short-circuit form exists. If you add word ops, `and=` / `or=` would look strange; cleanest is to keep these unimplemented.

---

### F2. Effect-list separator asymmetry (medium impact, low cost)

**Location.** `syntax.md` §7.1, lines 1972–1993. Within a `with` clause, effects are space-separated:

```
fn f() with reads(Db) reads(Cache) writes(Log) { ... }
```

Within an `effect group` definition, effects are joined with `+`:

```
effect group Validation = reads(Db) + sends(Net);
```

The doc itself flags the asymmetry: *"Effects within the `with` clause are space-separated — the effect verb keywords provide sufficient visual separation. Effect group definitions use `+` because they are declarative equations, not annotation lists."*

**Why this is worth reconsidering.** The reader has to learn two rules. For a non-Rust reader, neither form is intuitive — `with reads(Db) reads(Cache)` looks like two separate clauses (juxtaposition, not conjunction); `+` looks like arithmetic. The rationale ("declarative equation") is internal-design speak; from a reader's standpoint, both forms are *lists of effects* and should be punctuated identically.

**Three alternatives, each with tradeoffs:**

1. **Comma everywhere.** `with reads(Db), reads(Cache), writes(Log)` and `effect group Validation = reads(Db), sends(Net);`. Reads as a list. Cost: comma is also the inner separator inside `reads(A, B)`, so eyes do double duty — but humans handle this fine in `f(x, g(y, z))`. Lowest learning cost.
2. **`and` everywhere.** Same word-operator reasoning as F1: `with reads(Db) and reads(Cache) and writes(Log)`. Reads most prose-like. Cost: verbose for long signatures; `and` is overloaded with the boolean operator (though context disambiguates).
3. **`+` everywhere.** Symmetric, but reads as arithmetic. Mild loss vs. status quo for the `with` clause.

**Recommendation.** Pick one of (1) or (2) and use it both places. (1) is the conservative choice; (2) is a stronger natural-language statement and pairs with F1. Either beats the asymmetry.

---

### F3. `move` capture-mode prefix → `own` (low impact, very low cost)

**Location.** `syntax.md` §5.11, line 1428: `CAPTURE_MODE = "move" | "ref" | "mut" "ref"`.

**Why.** Kāra's design has unified the parameter-mode story around three forms — bare (owned), `ref` (borrow), `mut ref` (mutable borrow) — and reserved `own` as a keyword (line 51, kept reserved precisely so the diagnostic can point users at the unified rule). Closure capture modes use the same three semantic forms but spell the owned case `move` (Rust import) instead of the local convention.

`move` is jargon shorthand for "the closure takes ownership of every captured value." `own` is the word the rest of the language uses for that concept. Aligning closure-capture-mode keywords with parameter-mode keywords removes one piece of trivia.

```
own |x| x.consume()         // every capture is by-value (owned)
ref |x| x.read()
mut ref |x| x.mutate()
```

**Cost.** `own` is already reserved. The migration is a keyword swap. The `move` keyword would be retired (or kept as an alias for one release with a `karac fix` migration).

**Pushback to consider.** `move` is the universal Rust idiom; readers coming from Rust will look for it. But the case for *consistency within Kāra* outweighs cross-language familiarity — the entire ownership model already reads differently from Rust (no `&`/`&mut`/`'a`), so `move`-vs-`own` is consistent with the broader divergence.

---

### F4. Empty closure literal `||` is visually a logical-or (low impact, medium cost)

**Location.** `syntax.md` §5.11 line 1437: `|| { print("no params"); }`. Used in `with_provider` (`syntax.md` §3.14), `spawn` (`syntax.md` §5.9), `TaskGroup.spawn`, etc.

**Why.** A bare `||` opens a closure with zero parameters. To a non-Rust reader, `||` is the boolean-or operator. In `spawn(|| compute_result())` the `||` is reading visual noise — it's the *most common* shape (most spawn/defer/with_provider closures take no args) and it's the *least readable* form.

Compounded if F1 lands and `||` becomes obsolete as an operator: then `||` exists in the grammar *only* to introduce nullary closures, which is wasteful.

**Three alternatives:**

1. **Allow bare blocks in argument position when the parameter type is `Fn() -> T`.** `spawn { compute_result() }` instead of `spawn(|| compute_result())`. This is the "trailing lambda" shape Kotlin/Swift use. Cost: parser ambiguity at call sites — `spawn { ... }` looks like `spawn` followed by a struct literal in current Kāra grammar. Solvable but invasive.
2. **Introduce a `do { }` keyword for nullary closures.** `spawn(do { compute_result() })` reads as "spawn — do this." Word-keyword. Cost: one new keyword, but zero parser ambiguity.
3. **Status quo, but lean on F1 to reclaim `||`.** If logical-or becomes `or`, then `||` is unambiguously a nullary-closure marker and the visual collision disappears — at the cost of `||` still meaning "the empty parameter list" rather than anything intuitive.

**Recommendation.** (3) for now (it falls out of F1 for free); revisit (1) or (2) if user-testing shows readers still stumble. Trailing-lambda is the strictly-superior end state but the parser cost makes it a separate proposal worth its own design round.

---

### F5. `errdefer` is a compound word (low impact, very low cost)

**Location.** `syntax.md` §4 line 981, `design.md § defer / errdefer`.

**Why.** `errdefer` reads as one keyword, but the morpheme split (`err` + `defer`) is jargon. Compare:

```
errdefer { rollback_transaction(); }
defer on error { rollback_transaction(); }
defer if error { rollback_transaction(); }
```

The `defer on error` form composes with `defer` and `defer(e)` (the binding form `errdefer(e) { ... }`) more naturally:

```
defer            { close_connection(); }
defer on error   { rollback_transaction(); }
defer on error(e) { log_failure(e); }
```

**Cost.** Very low. Two keywords (`on`, `error`) — neither is currently reserved. `on` is fairly cheap to add; `error` could collide with a common identifier. Could use `defer if error` instead; `if` is already a keyword.

**Recommendation.** Defer (no pun) on this — `errdefer` works, and the savings are small. If you ever do a v2-grade syntax pass, fold this in.

---

### F6. Closure params don't accept patterns (clarity, not invented-here)

**Location.** `syntax.md` §5.11 line 1430: `CLOSURE_PARAM = IDENT [ ":" TYPE ]`. Params are `IDENT` only, not full `PATTERN`.

**Observation.** The rest of the language allows pattern destructuring in `let` and function parameters (verify against §3.1 and §4 — looks consistent). Closures are the exception. For a reader, this asymmetry is an "in this one place, you can't do the natural thing" — not a readability win or loss for typical closures, but it bites when destructuring tuple-returning iterators:

```
list.map(|tuple| { let (a, b) = tuple; a + b })       // current
list.map(|(a, b)| a + b)                              // pattern-allowing
```

**Cost.** Implementation work in the parser/typechecker. No grammar conflict.

**Recommendation.** Promote `CLOSURE_PARAM` from `IDENT [ ":" TYPE ]` to `PATTERN [ ":" TYPE ]`, restricted to irrefutable patterns (same rule that already applies to `fn` parameters). Modest implementation cost; modest readability win in the iterator/pipeline-heavy code that Kāra's `|>` pipe operator encourages.

(Verify this is not already handled — I read §5.11 closely but should sanity-check the grammar against `parser.rs` before proposing.)

---

### F7. `providers { ... } in { ... }` block trailing-`in` is awkward (low impact, low cost)

**Location.** `syntax.md` §3.15 lines 908–928.

**Why.** The grammar is `"providers" "{" bindings "}" "in" BLOCK`. The trailing `in` keyword is doing OCaml-`let-in` work, but it's the only `in` use outside `for x in xs`. Reads as a non-sequitur:

```
providers {
    OrderDB => PostgresOrderDB.connect(...)?,
    UserDB  => PostgresUserDB.connect(...)?,
} in {
    run_server()
}
```

Possible alternatives:
1. **Omit `in`** — `providers { ... } { run_server() }` — two adjacent braced blocks. Cost: parser ambiguity (need lookahead).
2. **Reuse a more familiar keyword** — `providers { ... } do { run_server() }` ("providers, do this"). Or `providers { ... } then { run_server() }`. `do` would also pick up F4(2)'s nullary-closure form, so it earns its keep.
3. **Status quo** — keep `in`. The form is rare (one block per program — usually in `main`), so the cost of awkwardness is paid once.

**Recommendation.** (3). The `providers` block is rare enough that polish here is low-priority. If a `do` keyword lands for closures (F4 alt 2), revisit.

---

## Rejected / out of scope

These were considered and not pursued. Recording so the reasoning isn't lost.

- **Drop curly braces, go indent-based.** Costs the macro/codegen story (though Kāra has no macros today, it has FFI, host functions, `extern`, and inline-asm support — all benefit from delimiter-clear blocks). The "Python feel" is *not* primarily from braces (see lens-setting note above) — Ruby has `end` and reads more naturally than Python. Brace removal would be a massive grammar shift for a small readability gain. **No.**

- **`==` → `is` and `!=` → `is not` (Python-style).** Python's `is` is *identity* (pointer equality), distinct from `==` (value equality), and the conflation is a famous source of bugs. Replicating the surface form without the semantic split would be a footgun. Kāra's `==` already reads fine. **No.**

- **Drop semicolons.** Major lex change, real cost (multi-line expressions need explicit continuation), small readability gain. Kāra already drops parens around `if`/`while` conditions, which is the bigger Rust-vs-JS-vs-Python win. **No.**

- **Word operators for arithmetic comparison (`<`, `<=`, `==`, etc.).** No language has tried this and won. The math notation is universal. **No.**

- **`return` keyword removal.** Kāra already makes `return` optional (last expression returns). Forcing implicit-return only would break patterns where mid-function return is clearer. **No.**

- **Replace `match` arrow `=>` with `->` or `then`.** `=>` is a strong universal convention (Rust, Swift, Scala, OCaml). Switching gains nothing. **No.**

- **`fn` → `function` or `def`.** `fn` is short; `function` is verbose; `def` confusable (Python = closure-ish with mutable defaults). `fn` is fine. **No.**

---

## Recommended next steps

If the user wants to act on this, the natural ordering is:

1. **F1 first** — reserve `and` / `or` / `not`, deprecate `&&` / `||` / `!`. Highest impact, lowest cost. Single round: keyword reservation, lexer/parser update, `karac fix` rule, error message rewrites, examples/stdlib sweep.
2. **F2 next** — pick comma-everywhere (safest) for effect-list separators, plus a sweep over docs/examples. Removes a learning bump.
3. **F3** — `move` → `own` keyword swap. Low-effort cleanup; bundle with F1 if doing a syntax-changes batch.
4. **F6** — closure pattern-params. Modest implementation work; modest payoff. Standalone item.
5. **F4 / F5 / F7** — defer until there's a v2-grade syntax pass.

Each of (1)–(3) plausibly fits in a single roadmap item; (4) is its own item; (5)–(7) are bundled or deferred.

---

## Caveats

- Findings F2, F3, F4 were verified against `syntax.md` directly; F1 was verified against the keyword list in §1.1; F6 needs a confirm-pass against `parser.rs` to ensure the grammar restriction is real and not just a doc oversight.
- I did not deep-read the implementation in `src/` — these are documentation-level proposals. Each would need an implementation cost-out before becoming a roadmap item.
- The lens here is **readability for non-Rust readers** specifically. A reader fluent in Rust will not benefit (and may slightly suffer) from F1, F3, F4. Kāra's positioning suggests this is the right tradeoff, but if "Rust-fluent migration" is a higher priority than "non-Rust accessibility," F1 weakens.

---

## Resolutions

Decisions are recorded here as we work through the findings. Implementation is **batched** — once all findings have a resolution, a single coherent change-set updates `syntax.md`, `design.md`, lexer, parser, linter, examples, and stdlib. Until then, this section is the running ledger.

The governing lens (added 2026-05-01 mid-discussion): **code in this project is LLM-written and human-reviewed.** Reviewer scan-speed dominates writing ergonomics; "more keystrokes" ≈ zero cost; "Rust-fluent familiarity" weakens as a pro; "easier to spot a bug in a diff" is a high-value win. This lens applies to F2–F7 going forward.

### Summary at a glance

| # | Finding | Decision | One-line rationale |
|---|---|---|---|
| F1 | `&&` / `\|\|` / `!` → `and` / `or` / `not` | **YES** (with mitigations) | High-frequency reviewer scan win; symbol form retired; precedence-trap linter required. |
| F2 | Effect-list separator asymmetry | NO | Fix adds common-case noise to repair rare-case inconsistency. |
| F3 | Closure capture mode keyword | **YES** (bare = owned) | Apply parameter-mode rule (bare = owned) to closures; retire `move`. |
| F4 | Empty closure literal `\|\|` | NO | F1 defuses the visual collision; symmetry across closure arities matters more than the aesthetic itch. |
| F5 | `errdefer` keyword | NO | Compact self-contained word beats multi-word phrase plus new keyword. |
| F6 | Closure params accept patterns | **YES** | Pure asymmetry removal; no new tokens or keywords. |
| F7 | `providers { ... } in { ... }` trailing-`in` | NO | Once-per-program construct; new keyword has bad ratio. |

**Implementation batch scope:** F1, F3, F6. Implementation pending; this doc remains the source of truth until the canonical docs (`syntax.md`, `design.md`) are updated and the lexer/parser changes land.

**Decision rules surfaced during the scan** (recorded for future syntax decisions):
1. **Don't add common-case noise to fix rare-case inconsistency.** (F2)
2. **Symmetry across closely-related forms beats fixing one form's prettiness.** (F4)
3. **Compact self-contained keywords beat multi-word phrases that introduce new reserved words.** Per-encounter cost compounds; new-keyword footprint is permanent. (F5, F7)
4. **Cleanest "yes" decisions remove complexity without introducing new complexity.** (F1, F6)
5. **Don't apply heuristics universally** — F1's "word operators win" did not transfer to F5 because the surface-area tradeoff was different (symbol → word vs. word → multiword + keyword).

### F1 — Word operators (RESOLVED: yes, with mitigations)

**Adopted:**
- `&&` → `and`, `||` → `or`, `!` → `not`. Reserve the three identifiers; deprecate the symbol forms with a one-release `karac fix` migration warning.
- Bitwise `&`, `|`, `^` stay as symbols (they're a different operator and the symbol form is universal).
- Precedence: `not` binds tighter than `and`/`or`, mirroring `!`/`&&`/`||`.

**Mitigations required (new, lens-driven additions to the original proposal):**

1. **Linter rule: warn on `not` adjacent to a comparison operator without parentheses.** Closes Python's 30-year-old `not x == y` precedence trap before it bites a reviewer. The rule: if `not` is followed by an expression containing a top-level `==`/`!=`/`<`/`<=`/`>`/`>=`, require parens around either the comparison or `not`'s operand. Example: `not x == y` → error, "use `not (x == y)` or `(not x) == y`."
2. **Lexer error on `&&`/`||`/`!` must offer the migration path.** Message format: ``Kāra uses `and` instead of `&&` — run `karac fix` to migrate.``. The lens makes this important: the LLM will reflexively type `&&` from Rust/C training data; a clear, specific error short-circuits the loop.

**Rejected: do not add `and=` / `or=` compound assignment.** Reasons (lens-driven):
- Rust and Python both omit them — strong "not load-bearing" signal.
- Short-circuit-in-assignment (`result or= compute()` only runs `compute()` if `result` is falsy) is a reviewer footgun: reads as plain assignment, behaves conditionally. Exactly the kind of trap the lens is set up to avoid.
- Kāra has cleaner type-aware alternatives: `OnceCell` / `memo(...)` for memoization, `result.unwrap_or(default)` for defaults. Ruby reaches for `||=` partly because it lacks `Option`/`Result`; Kāra wouldn't.

**Why this passes the lens:** reviewer scan-speed dominates; `if user.is_active and not user.is_banned and ...` is materially safer to review for negation bugs than the `!` form. Bitwise-vs-logical visual collision (`&` vs `&&`) — a known C-family footgun — disappears entirely. LLM-side "training-data scarcity" risk is small because Python provides the existence proof: LLMs already write `and`/`or`/`not` correctly in Python contexts and will do the same here once anchored.

**Implementation checklist (for the batch):**
- Lexer: add `and`/`or`/`not` keywords; emit migration error on `&&`/`||`/`!` for one release.
- Parser: same precedence as the symbol forms.
- Linter: ambiguous-`not`-with-comparison warning.
- `karac fix`: rote rewrite rule for the three operators.
- `syntax.md` §1.1, §1.2: add the new keywords; remove the symbol forms from the operator table.
- `design.md`: any place that uses `&&`/`||`/`!` in code examples.
- `examples/` and stdlib scaffolding: mechanical sweep + identifier-collision rename.

### F2 — Effect-list separator asymmetry (RESOLVED: not pursued)

**Status quo retained.** `with` clauses keep space-separation; `effect group` definitions keep `+`.

**Why not pursued:**

1. **Frequency mismatch.** `with` clauses appear on every public effectful function — the reader sees them constantly. `effect group` definitions are declarations (defined once, reused many times) — the reader sees them rarely, comparable in frequency to `type` aliases. The asymmetry is real, but the `+` form is encountered too rarely for the inconsistency to be a meaningful reviewer cost. The "two rules to learn" cost is paid once.

2. **The fix would harm the common case to help the rare case.** Every alternative considered (parens-wrapped, `and`-everywhere, comma-everywhere, `+`-everywhere) added characters or visual weight to the `with` clause — the form readers see constantly. We'd be adding small reviewer noise to the common case to fix small reviewer noise in the rare case. Bad trade under the lens.

3. **Real-world `with` clauses are short.** The "visual association" concern (eye reads `with reads(Db)` as a unit, has to re-parse to extend the clause) only really bites at 4+ effects; well-designed code keeps effect counts low. Status quo handles the typical 1-2-effect case fine.

4. **`+` reads as set union in a declarative context.** `effect group V = reads(Db) + sends(Net)` is semantically apt — *the effect set is the union of these families*. Even though it's not symmetric with `with`, it's locally meaningful in the one-off context where it appears.

**What this finding produced:** sharpened the lens-application heuristic — *every readability proposal must justify its reviewer cost on the common case, not just on the corner case it fixes*. Recorded for future findings.

**Considered alternatives (rejected):**

- Wrap effects in parens (`with (reads(Db), writes(Log))`) — solved visual association but cost two chars per `with` clause, paid on every function. Asymmetry was not painful enough to justify common-case noise.
- `and` everywhere — pairs with F1 but reads as unnatural English (real English uses comma between list items, "and" only before the last). Worse, doubles down on the common-case-noise tradeoff.
- Comma or `+` without parens — neither fully solves the visual-association concern, and either way changes the common case.

### F3 — Closure capture mode keyword (RESOLVED: remove `move`, no replacement — bare `|...|` = owned)

**Adopted:**
- Bare `|...|` = owned captures. Matches parameter modes (bare `T` = owned at function signatures).
- `ref |...|` = borrow captures.
- `mut ref |...|` = mutable borrow captures.
- The `move` keyword is removed entirely. No longer needed; the default *is* owned.
- The `own` keyword stays **reserved for diagnostics only** — never appears in user-written code. Diagnostic: `you wrote `own |x|` — bare `|x|` already means owned, drop the keyword`. Catches Rust-fluent writers reaching for the wrong vocabulary.
- `karac fix`: rote `move |...|` → `|...|` rewrite.

| Position | Owned | Borrow | Mut borrow |
|---|---|---|---|
| Function parameter | `T` | `ref T` | `mut ref T` |
| Closure capture | `\|x\|` | `ref \|x\|` | `mut ref \|x\|` |

One rule across the language: **bare = owned; explicit prefix = borrow mode**.

**Why this evolved from the original "rename `move` to `own`" proposal:**

The brainstorming originally proposed `move` → `own` as a keyword *swap*, which preserved the inconsistency it claimed to fix: parameters use bare-T = owned (no keyword), while closures would still require an explicit `own`. The user pushed back: "if inferred by default why add optional own. it's not consistent with bare T."

Re-examining why the original design required an explicit capture keyword at all surfaced three reasons, none of which survived the lens:

1. **Rust legacy.** Rust defaults to borrow at closures and uses `move` to opt into owned. Kāra's original closure design copied Rust's vocabulary wholesale rather than applying its own bare-owned-default convention.
2. **Silent-move footgun.** Fear that `|x| big.process(x)` silently consuming `big` would surprise the writer. But this is *identical* to the situation at function parameters (`fn f(big: BigStruct)` silently consumes), already solved by the bare-owned-default rule. The same rule applied to closures gives one mental model for the whole language.
3. **Rust-user familiarity.** Devalued by the lens (LLM adapts; within-language consistency matters more than cross-language familiarity).

So the right fix wasn't to rename the keyword — it was to remove it. Bare-owned closures match parameter modes exactly, and one language-wide rule replaces two per-position conventions.

**One detour considered and rejected: inference.**

I briefly proposed inferring the mode from body usage + escape analysis, with the keyword reserved as an optional override. The user noted this would still be inconsistent with bare-T = owned at parameters. Correct — inference solves the wrong problem (removes a redundant keyword by adding analysis machinery) when the simpler fix is *not having the keyword at all*. The bare-owned default removes the redundancy without any inference machinery.

**Implementation checklist (for the batch):**
- Lexer/parser: retire `move`; `|...|` defaults to owned captures; `ref` and `mut ref` retain their roles.
- `karac fix`: `move |...|` → `|...|`.
- Diagnostic: detect `own |...|` and `move |...|` in user code, suggest dropping the keyword.
- `syntax.md` §5.11: rewrite `CAPTURE_MODE` grammar (now optional; default omitted).
- `design.md`: any closure examples using `move` get updated.
- Examples and stdlib sweep.

### F4 — Empty closure literal `||` (RESOLVED: status quo, retained for symmetry)

**Status quo retained.** `||` continues to mark a zero-parameter closure (`spawn(|| compute())`); parameterized closures continue to use `|x|` / `|x, y|`. One pipe-pair glyph means "closure parameter list" regardless of arity.

**The actual positive case for status quo:**

The original concern was that `||` reads as logical-or to non-Rust readers. F1's resolution structurally removes that: post-F1, `||` no longer means logical-or anywhere in the language, so when a reviewer encounters `||`, the only thing it can mean is "empty closure parameter list." Visual collision: gone.

But the *stronger* case is symmetry. **Closures should look like closures regardless of arity.** A reviewer scanning code instantly recognizes `|...|` as "this is a closure parameter list" — whether empty (`||`), single (`|x|`), or multi (`|x, y|`). One pattern, one glyph. Breaking the symmetry between empty and parameterized forms forces the reviewer to learn two patterns ("when arity = 0, look for `{ }` after the call; when arity > 0, look for `|...|`"). That asymmetry costs the reviewer more than the original aesthetic concern ever did.

**Considered alternatives (rejected):**

- **Trailing lambda for empty closures only** (`spawn { compute() }`) — introduces the asymmetry above. Worse for reviewers: at a function-call site they no longer instantly know whether a closure is being passed; the `{}` after the call could in principle be many things, and the case-class invariant resolves it for the parser but doesn't help the reader who pattern-matches by glyph shape.
- **`do { }` keyword for empty closures only** — same asymmetry, plus a new reserved word for one syntactic role.
- **Full trailing-lambda redesign covering both empty and parameterized closures** — a much bigger design round (Kāra would have to choose a Kotlin-`it` / Scala-`_` / explicit-param-after-brace convention). Out of scope for this scan. If it ever happens, do it as its own design item, not bolted onto F4.

**Decision rule extracted:** *when a finding's "fix" introduces asymmetry to solve a small aesthetic concern, the asymmetry costs the reviewer more than the original concern did. Symmetry beats prettiness.* This matches the F2 decision rule (don't add common-case noise to fix rare-case asymmetry) — both reduce to "don't make the common scan-pattern more complex."

### F5 — `errdefer` keyword (RESOLVED: status quo)

**Status quo retained.** `errdefer { ... }` and `errdefer(e) { ... }` keep their current form. Not migrated to `defer on error`.

**Why retained — the deciding consideration: per-encounter vs. first-encounter cost.**

- `errdefer`: first-encounter cost is real (reader has to learn it; the morpheme split is Zig-imported jargon). Per-encounter cost is *zero* — one token, one glyph, instant pattern-match on every subsequent read.
- `defer on error`: first-encounter cost is slightly lower (more guessable from `defer`). Per-encounter cost is *higher* — three tokens to parse every time, and until the reader has internalized that `on` is exclusive to this construct, every encounter raises the latent question "is `on` used elsewhere too?"

Under the lens, the reviewer pays the per-encounter cost on every read; the first-encounter cost is paid once per reader. Compact-self-contained beats multi-word-with-new-keyword whenever the keyword is reasonably guessable on first sight — which `errdefer` is once you know the language uses `defer`.

**The new-keyword footprint cost is permanent.** Adding `on` carries cognitive footprint beyond its primary use site. Even if `on` were only used in `defer on error`, every reader scanning Kāra code has to *know* that — they can't tell from the keyword alone whether it appears elsewhere. A new reserved word always raises the question "where else does this show up?" and answering requires either documentation lookup or accumulated experience. That cost is paid forever, by every reader.

**Considered alternative (rejected): `defer on error` / `defer if error` form.** Discoverable, prose-shaped, composes with `defer`. Genuine pros, but outweighed by the per-encounter cost increase plus permanent new-keyword footprint. The first-encounter advantage doesn't justify the lifetime tax.

**Decision rule extracted:** *compact, self-contained keywords beat multi-word phrases that introduce new reserved words. The per-encounter cost of parsing multiple tokens, plus the permanent footprint of a new keyword, outweighs the first-encounter advantage of guessability.* This is a refinement on the F1 "word operators win" heuristic: F1 was symbol-vs-word at the same surface area; F5 is one-word vs. three-words-plus-keyword, which is a different tradeoff entirely.

**Meta-note for future syntax decisions:** the "post-F1, lean toward word keywords" heuristic does NOT transfer universally. F1 was symbol → word, no keyword-surface change. F5 would have been word → multiword, +1 keyword surface. Don't conflate them; weigh each case on its own merits.

### F6 — Closure params accept patterns (RESOLVED: yes)

**Adopted:** promote `CLOSURE_PARAM` from `IDENT [ ":" TYPE ]` to `PATTERN [ ":" TYPE ]`, restricted to irrefutable patterns. Same rule that already applies to `fn` parameters and `let` bindings.

```
list.map(|(a, b)| a + b)                       // tuple destructure
xs.iter().enumerate().map(|(i, x)| ...)        // pipeline-friendly
list.map(|Point { x, y }| x * y)               // struct destructure (irrefutable)
```

**Why this is the cleanest "yes" in the scan:**

Pure asymmetry removal. F6 doesn't introduce new tokens, new keywords, or new grammar shapes — it just expands what's accepted in an existing position. The reader already knows `(a, b)` and `Point { x, y }` from `let` bindings and function parameters; the closure case becomes one rule fewer to remember. Reviewers benefit forever; the implementation cost is paid once, by the LLM.

**Pros:**
- Removes a "rule + one exception" the reviewer otherwise has to remember (destructuring works at `let`, at `fn` params, in `match` arms — but not at closures, until now).
- Cleaner iterator/pipeline code — tuple-returning iterators (`zip`, `enumerate`) become natural to consume.
- Same restriction as `fn` params (irrefutable only) — one mental rule covers both.
- No common-case noise added; no new keywords; no asymmetry introduced elsewhere.

**Cons (none survive the lens):**
- Implementation cost is real but modest, no grammar conflict expected. Verification needed: confirm `parser.rs` actually rejects patterns today (not just a doc oversight) — if the parser already accepts them, F6 is docs-only.
- Style risk: `|((a, b), c, d)| ...` could get confusing. But `let` already permits the same construct without restriction, and the reviewer is the right gatekeeper for taste.

**Implementation checklist (for the batch):**
- Parser: closure-param production accepts irrefutable patterns (verify current state first).
- Typechecker: pattern-binding hookup so destructured params bind correctly.
- `syntax.md` §5.11: update `CLOSURE_PARAM` rule.
- Examples sweep: any closure that does inline destructure-via-`let` becomes pattern-param.

**Pattern observed:** F1 and F6 are the two clean "yes" decisions in the scan. Both share a structural property: the change *removes complexity* (a symbol form / a language exception) without introducing new mental models. The cases that scored "no" (F2, F4, F5) all proposed adding something — common-case noise, asymmetric forms, or new keywords. **The scan's strongest decision rule:** when a fix removes complexity without introducing new complexity, it's almost always worth doing; when a fix trades one kind of complexity for another, scrutinize harder.

### F7 — `providers { ... } in { ... }` trailing-`in` (RESOLVED: status quo)

**Status quo retained.** The `providers { ... } in { ... }` form keeps its trailing `in` keyword.

**Why retained:**

- **Frequency.** The `providers` block appears ~once per program (typically in `main`). The "two meanings of `in`" cost is paid once per reader; per-encounter cost in real codebases is near-zero because most files don't have a `providers` block at all.
- **New-keyword footprint (F5 rule).** Alternatives like `do` or `then` would add a permanent reserved word for one syntactic role used once per program. Bad ratio — same anti-pattern as F5.
- **Asymmetry is mild.** `in` does dual duty (iteration in `for x in xs`; scoped binding in `} in {`), but the contexts disambiguate by surrounding shape — the reviewer doesn't disambiguate by the `in` keyword itself. Low cost.
- **Removes-vs-adds rule (F1/F6).** Every alternative *adds* something (new keyword, parser ambiguity workaround). Status quo doesn't add or remove.

**Considered alternatives (rejected):**

- **Omit `in` entirely** (`providers { ... } { run_server() }`) — parser ambiguity requires lookahead. Implementation complexity for marginal aesthetic gain.
- **New keyword (`do` / `then`)** — permanent reserved-word footprint for once-per-program use. Same anti-pattern the F5 decision identified.

**This finding closes the scan.** Of the seven findings, three resolved YES (F1, F3, F6) and four resolved NO (F2, F4, F5, F7). The two YES cases that propose changes both share the same shape: removing complexity (a symbol form, a language exception) without adding any. The four NO cases all proposed adding something to fix something else — a tradeoff the lens consistently rejects.
