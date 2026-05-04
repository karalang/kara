# Kāra Design Audit

## Purpose

This document tracks a systematic, section-by-section audit of `docs/design.md`. The goal is to
stress-test the **design itself** — not the implementation. Development happens in parallel and will
eventually catch up to findings here.

## What this is NOT

- Not a test of whether the compiler implements a feature correctly.
- Not a replacement for the implementation checklist (`docs/implementation_checklist/`).
- Not a code review.

## Reference documents

| Document | Role |
|----------|------|
| `docs/design.md` | Primary source of truth — authoritative language spec |
| `docs/syntax.md` | Syntax reference — use to verify that example programs are syntactically legal |
| `docs/implementation_checklist/` | Known implementation gaps — avoid duplicating these as findings |

## Method: write programs, not just prose

For each section, the primary tool is **writing Kāra code examples** and asking whether the spec
fully describes their behavior. This is sharper than reading the spec in the abstract because:

- You cannot hand-wave an edge case once you have to write the actual syntax.
- Syntax gaps surface immediately — if you cannot write the example, the spec didn't define the
  syntax (cross-check with `docs/syntax.md`).
- Two conflicting readings of a sentence produce two different programs you can compare directly.
- Well-chosen adversarial examples become test cases once gaps are resolved.

**Start with the happy path** — write the examples the spec itself implies. Then write the
**adversarial examples**: combinations, empty cases, recursive cases, error cases, and cross-feature
interactions. Check each one: does the spec tell you what this program should do? Is the syntax
legal? What is the expected output or diagnostic?

## What we are looking for

For each section, ask these questions — and write a program to probe each one:

1. **Gaps** — Cases or sub-cases the spec is silent on. What happens when X meets Y and the design
   doesn't say?
2. **Ambiguities** — The spec says something but two reasonable readers could interpret it
   differently. Can you write two programs that make the ambiguity concrete?
3. **Contradictions** — Two parts of the spec imply different behavior for the same input.
4. **Missing error cases** — The spec defines valid behavior but doesn't specify what happens on
   invalid inputs, or doesn't pin down the diagnostic. Write the invalid program and ask: what
   error does the spec require?
5. **Interaction blindspots** — A feature works fine in isolation but its interaction with another
   feature (generics + effects, ownership + closures, etc.) is unspecified. Write the combined
   program.
6. **Syntax gaps** — A construct is described semantically but its concrete syntax is absent or
   ambiguous. Cross-check with `docs/syntax.md` and write the program to verify.
7. **Test missing** — The spec is clear, the implementation exists, but no test covers the case.
   Add the test and note it here as `test-added`.

## Finding dispositions

Each finding is tagged with one of:

| Tag | Meaning |
|-----|---------|
| `gap` | Spec is silent; needs a decision |
| `ambiguity` | Spec is underspecified; needs clarification |
| `contradiction` | Two spec sections conflict |
| `missing-error` | Valid path defined; invalid path unspecified |
| `interaction` | Cross-feature interaction not addressed |
| `syntax-gap` | Semantics defined but concrete syntax absent or ambiguous |
| `test-missing` | Spec clear + impl exists but no test; becomes `test-added` once written |
| `ok` | Section is solid; no issues found |

## How to continue in a new session

1. Open this file and find the first section in the Audit Progress index whose status is `[ ]` or
   `[~]`.
2. Open `docs/design.md` and navigate to that section (line numbers are in the index).
3. Also open `docs/syntax.md` for syntax cross-checks.
4. Read the section carefully. Write Kāra programs — happy path first, then adversarial examples —
   and apply the seven questions above to each one.
5. Record each finding as a short summary line directly under the section entry in the index
   below, using the next F-NNN number. Format: `F-NNN \`tag\` \`open\` — one-line description`.
   Include an anchor link to the finding detail if one was written. The index is the single
   source of truth — no separate findings section.
7. Mark the section `[x]` done when satisfied. Mark `[~]` if partially done but stopping mid-section.
8. **For straightforward fixes** (typos, missing error code, obvious omission with a clear answer):
   fix it directly in `docs/design.md` and note `spec-updated` alongside the finding. No discussion
   needed.
9. **For anything that needs a design decision** (genuine gap, ambiguity with multiple valid
   resolutions, interaction that could go multiple ways): add a `## F-NNN` section to the
   current findings file in `docs/design_audit/` (e.g. `findings-026-050.md`) and record the
   tradeoffs, options, and open questions there. Findings are grouped 25 per file; when a file
   reaches 25 entries, start the next (e.g. `findings-051-075.md`). Link from the index with
   the anchor format: `docs/design_audit/findings-026-050.md#f-047`. Do not guess a resolution
   — leave it for the author to decide. Note `brainstorm-created` alongside the finding entry here.
10. If a finding requires a new test, write it in the appropriate `tests/` file and add `test-added`.
11. Commit all changes (findings doc + any spec/test edits) before ending the session.

## Status legend

Section status:
- `[ ]` — not started
- `[~]` — in progress
- `[x]` — done

Finding status (inline on each finding):
- `open` — decision pending from author
- `resolved` — fixed and closed

---

## Audit Progress

### Preamble

- `[x]` **Starting Assumptions** (line 86) — foundational constraints the rest of the design rests on
  - F-006 `gap` `resolved` — "memory-safe" never formally defined; scope (spatial/temporal/type/race) unspecified → closed: five properties defined in table at Starting Assumption 5 (temporal/type/race-freedom universal; spatial configurable on embedded; integer overflow always defined); embedded/isr profile weakening note added to § Project Profiles `spec-updated` → `docs/design_audit/findings-001-025.md#f-006`
- `[x]` **Specification Layers / Seed Classification** (line 96) — what is v1, deferred, future
  - F-001 `contradiction` `resolved` — layout constraint compliance moved to Guaranteed semantics; Implementation Freedom bullet covers only within-constraint freedom → `docs/design_audit_v1/findings-001-025.md#f-001`
  - F-002 `gap` `resolved` — "unclassified" behavior had no defined default treatment → fixed in spec
  - F-003 `ambiguity` `resolved` — "observable ordering within a single effect resource": per instance or type? reads covered? language-level or hardware-level? → closed: per instance, reads covered, language-level; hardware ordering deferred to Volatile Memory Access `spec-updated` → `docs/design_audit/findings-001-025.md#f-003`
  - F-004 `gap` `resolved` — RC flavor (Rc→Arc) classified as reported behavior but has semantic implications for Send/thread-safety → closed: rule is now Guaranteed (thread-crossing → Arc, no crossing → Rc, cannot-prove → Arc conservatively); reported behavior narrowed to the specific flavor chosen per binding `spec-updated` → `docs/design_audit/findings-001-025.md#f-004`
  - F-005 `ambiguity` `resolved` — spec permits "broadening" of private inferred effects, which can silently break public function verification on compiler upgrade → closed: within-edition inference is monotonically non-increasing; broadening only at edition boundaries as a documented breaking change (warning-then-error, LLM-automatable from changelog) `spec-updated` → `docs/design_audit/findings-001-025.md#f-005`
  - F-007 `ambiguity` `resolved` — "public function" in guaranteed semantics: `pub` at item level vs externally reachable from outside the crate → closed: false premise; Kāra has no `mod` keyword, no `pub(crate)`, no `pub(super)`; `pub` is the only visibility level that crosses the package boundary — unambiguous by design; Guaranteed semantics bullet clarified `spec-updated` → `docs/design_audit/findings-001-025.md#f-007`

---

### Core Language

- `[x]` **Identifiers and Naming** (line 143) — case classes, rules, Unicode
  - F-008 `contradiction` `resolved` — single uppercase letters (`T`, `K`, `V`) can't be Type-class by the definition but are used as generic type params throughout the spec → closed: Type-class definition extended with single-letter carve-out; CN-7 rule added; syntax.md TYPE_IDENT comment updated `spec-updated` → `docs/design_audit/findings-001-025.md#f-008`
  - F-009 `ambiguity` `resolved` — path disambiguation rationale claims "Value-class starts a value expression" but `std.fs.read_to_string` uses Value-class `std` as a path → closed: rationale rewritten to correctly describe three cases (Type-class = lex-time; Const-class = lex-time by elimination; Value-class = resolver-phase scope lookup); bad example replaced `spec-updated` → `docs/design_audit/findings-001-025.md#f-009`
  - F-010 `contradiction` `resolved` — CN-4 rationale incorrectly claimed `HTTPClient` would be Const-class; it is Type-class by the formal rules → fixed in spec `spec-updated`
  - F-011 `ambiguity` `resolved` — TYPE_IDENT grammar in syntax.md is broader than its comment; all-uppercase multi-char names like `HTTP` match TYPE_IDENT but should be CONST_IDENT → closed: post-lex four-step classification algorithm added to syntax.md; grammar intentionally over-permissive, algorithm is normative `spec-updated` → `docs/design_audit/findings-001-025.md#f-011`
- `[x]` **Type System and Object Model** (line 216) — type inference, generics `[T]` syntax
  - F-014 `gap` `resolved` — LUB algorithm referenced but never defined: behavior for generic types with partial params, refinement types, and cross-branch inference is unspecified → closed: five-rule LUB algorithm defined in § Type Inference (identical; same-constructor partial-unknown; same-constructor known-differing; refinement→base; error) `spec-updated` → `docs/design_audit/findings-001-025.md#f-014`
- `[x]` **Error Handling** (line 321) — Result, panic, propagation rules
  - F-012 `gap` `resolved` — `??` operator result type not stated; also silently discards `Err` value — clarified in spec `spec-updated`
  - F-015 `ambiguity` `resolved` — `?` desugaring inserts implicit `From::from` call which may have effects; spec doesn't say whether these are tracked → closed: compiler-inserted `from` call contributes impl's declared effects to enclosing function (soundness requirement); E1==E2 edge case (no from call) and missing-impl edge case pinned `spec-updated` → `docs/design_audit/findings-001-025.md#f-015`
  - F-016 `gap` `resolved` — `errdefer(e)` in functions returning `Option[T]` undefined; also unclear whether parameterless `errdefer` fires on `None` propagation → closed: `errdefer(e)` in Option functions = compile error; parameterless `errdefer` fires on None-propagation ("non-success" redefined to cover both Err and None) `spec-updated` → `docs/design_audit/findings-001-025.md#f-016`
- `[x]` **Module System** (line 420) — visibility, paths, re-exports
  - F-013 `gap` `resolved` — no platform file for a module on some target was undefined; missing-platform rule added to spec `spec-updated`
  - F-017 `ambiguity` `resolved` — prelude uses `import std.prelude.*` notation but wildcard imports are blocked for users; internal mechanism vs grammar-level `*` never clarified → closed: both wildcard imports and nested grouping promoted to v1; precedence rules defined (explicit > wildcard > prelude; wildcard collision = use-site error); prelude now uses real wildcard machinery `spec-updated` → `docs/design_audit/findings-001-025.md#f-017`
- `[x]` **Package System** (line 529) — manifests, versioning, dependencies
  - F-018 `contradiction` `resolved` — scaffold described as "four files" but table lists five → fixed in spec `spec-updated`
  - F-019 `gap` `resolved` — `edition` field is validated but its semantic implications (what editions change, what the default is, unknown-edition behavior) are never defined → closed: Edition semantics subsection added; v1 only "2026"; unknown = compile error; default = "2026"; per-crate property; gates breaking changes; declaration process deferred `spec-updated` → `docs/design_audit/findings-001-025.md#f-019`
  - F-020 `gap` `resolved` — workspace-level dependency declaration syntax not shown; `workspace = true` in member manifests references an undeclared location → closed: `[workspace.dependencies]` at root; missing declaration = compile error; full root+member example added `spec-updated` → `docs/design_audit/findings-001-025.md#f-020`
  - F-021 `gap` `resolved` — package name as module path prefix undefined: when is it required, how cross-package imports differ from intra-package, and how collisions are resolved → closed: intra omits package name; cross uses dep name as first segment; collision = local wins, dep must use alias key; canonical path includes package name internally `spec-updated` → `docs/design_audit/findings-001-025.md#f-021`
- `[x]` **Module-Level Bindings** (line 630) — statics, thread-locals, constants
  - F-022 `contradiction` `resolved` — `LazyLock.new` described as using `const fn` which is deferred to post-v1; reworded as a compiler-recognized special form with explicit closure-capture restriction `spec-updated`
  - F-023 `contradiction` `closed` — public functions writing module-level `let mut` must declare effects, but the synthetic resource is not namable; the two requirements are irreconcilable → `docs/design_audit/findings-001-025.md#f-023`
  - F-024 `ambiguity` `closed` — `String` at module scope: spec shows `pub let APP_NAME: String = "karac"` as ✓ allowed, but `String` is heap-allocated and heap allocation is a forbidden initializer → `docs/design_audit/findings-001-025.md#f-024`
  - F-025 `ambiguity` `closed` — `par {}` write rule: "mutating from inside" reads as direct-mutation-only, but the synthetic-resource effect propagates transitively through the call graph — which scope does the rule cover? → `docs/design_audit/findings-001-025.md#f-025`
- `[x]` **Struct Field Visibility** (line 773)
  - F-027 `gap` `closed` — `mut` as a field modifier is only defined for `shared struct`; whether it is valid on plain struct fields and what it would mean there is never stated → `docs/design_audit/findings-026-050.md#f-027`
- `[x]` **Standard Data Structures** (line 805) — collection literals, core methods
  - F-026 `ambiguity` `resolved` — `Vec.last(n)` indexing scheme ("end-relative") never defined; clarified as `self[len-1-n]` `spec-updated`
  - F-028 `interaction` `closed` — prefix literal form `Array[1, 2, 3]` conflicts with the generic instantiation disambiguation rule; single-element case `Array[42]` is ambiguous → `docs/design_audit/findings-026-050.md#f-028`
  - F-029 `gap` `closed` — `Option[ref T]` in method signatures: `ref` appears inside a generic type argument but is only defined as a parameter mode; whether borrows are first-class types is never stated → `docs/design_audit/findings-026-050.md#f-029`
- `[x]` **Numerical Types — Tensor / Column / DataFrame** (line 1042)
  - F-031 `syntax-gap` `closed` — `t[i, j, k]` multi-index conflicts with `expr[expr]` grammar; tuple-desugar vs separate production unspecified; `t[i]` on rank-3 undefined → `docs/design_audit/findings-026-050.md#f-031`
  - F-032 `gap` `closed` — two `?` dims that must match (e.g. `K` in matmul) have no specified runtime check; error type and message undefined → `docs/design_audit/findings-026-050.md#f-032`
- `[x]` **Numeric Semantics** (line 1205) — overflow, float, casting rules
  - F-033 `contradiction` `closed` — "overflow always traps in all build modes" conflicts with `embedded` profile permitting wrapping on bare operators; Reading B (bare ops always trap, only `.wrapping_*()` escapes) is probably correct but unconfirmed → `docs/design_audit/findings-026-050.md#f-033`
  - F-034 `ambiguity` `closed` — `Column[f64]` dual null conventions: NaN propagates via IEEE arithmetic, bitmap-null via SQL semantics; spec says "SQL null propagation" without clarifying it applies only to bitmap-null elements → `docs/design_audit/findings-026-050.md#f-034`
  - spec-updated: "four arithmetic binary operators" → "five" (listing `+`,`-`,`*`,`/`,`%`)
- `[x]` **Variable Binding Rules** (line 1327)
  - F-035 `syntax-gap` `closed` — `let x: T;` (uninit declaration) used in design.md and Array section but absent from grammar in syntax.md; LET_STATEMENT requires `= EXPR`; flow-sensitive definite-assignment analysis (branches, loops, struct fields) never defined → `docs/design_audit/findings-026-050.md#f-035`
  - F-036 `syntax-gap` `closed` — `LET_ELSE_STATEMENT` grammar omits `mut`; `let mut Ok(x) = result else { ... }` not supported by grammar; also `let mut (a, b) = ...` semantics (all-mutable vs per-binding) undefined; per-binding `mut` inside patterns not in grammar → `docs/design_audit/findings-026-050.md#f-036`
- `[x]` **Conditionals** (line 1379) — `if`, `if let`, `let...else`
  - F-037 `gap` `closed` — `if` without `else` type constraint not stated; `else if let` undocumented (grammar supports it); or-pattern binding consistency rule (mismatched names/types in `if let A(x) | B(y)`) never stated → `docs/design_audit/findings-026-050.md#f-037`
- `[x]` **Loops** (line 1435)
  - F-038 `gap` `closed` — `break expr` on `while`/`for` undefined (spec says "only meaningful with loop" but not "compile error"); conditional `break` loop type unspecified; `for` desugaring calls `.iter()` but Iterator/Iterable/IntoIterator relationship to `for` is unclear → `docs/design_audit/findings-026-050.md#f-038`
- `[x]` **Iterator Traits and Adaptors** (line 1488)
  - F-039 `gap` `closed` — `for` desugars to `.iter()` requiring `Iterable`, but three traits defined; `IntoIterator` role in `for` loops unspecified; no blanket `impl[I: Iterator] Iterable for I` stated; `for x in my_vec.into_iter()` undefined → `docs/design_audit/findings-026-050.md#f-039`
- `[x]` **Derive** (line 1598) — `#[derive(...)]`, serialization
  - F-040 `ambiguity` `closed` — example derives `Eq, Hash` without `PartialEq` (violates stated dependency chain); `Copy` + `Clone` auto-derive behavior unstated; Serializer/Deserializer methods carry no effect annotations despite I/O use; `#[serde(default = ...)]` accepts only literals or any const-expr? → `docs/design_audit/findings-026-050.md#f-040`
- `[x]` **Conditional `impl` Blocks** (line 1689)
  - F-040 already covers derive example showing Eq without PartialEq (same issue repeated here)
- `[x]` **`where` Clauses for Generic Bounds** (line 1721)
  - ok — declaration order and mixing rules clearly stated; effect-variable bounds noted as deferred
- `[x]` **Orphan Rules for `impl` Blocks** (line 1784)
  - F-041 `gap` `closed` — blanket impl `impl[T: Bound] ForeignTrait for T` not addressed; ownership of generic parameter T is ambiguous under the "define either trait or type" rule → `docs/design_audit/findings-026-050.md#f-041`
- `[x]` **Method Resolution** (line 1820) — algorithm, generics, effect-conflict case
  - ok — four-step algorithm, UFCS disambiguation, effect-conflict case, and generic interaction all well-specified
- `[x]` **Trait Constraints (Supertraits)** (line 1934)
  - ok — semantics, default method bodies, bound compression all specified; transitive supertrait enforcement implied but not stated (minor)
- `[x]` **Associated Types in Traits** (line 1979)
  - ok — projection syntax, equality constraints, one-impl-per-trait rule, and associated type bounds all specified
- `[x]` **Conversion Traits** (line 2062) — `From`, `Into`, `TryFrom`, `TryInto`
  - ok — blanket Into/TryInto derivation, effect propagation through blanket, prohibition on direct Into impl all specified; F-015 (? desugaring effects) already covers From::from interactions
- `[x]` **Refinement Types** (line 2162)
  - spec-updated: `q: i64` → `q: i32` in arithmetic-preserves-base-type example (typo fix)
  - F-042 `ambiguity` `closed` — `as` keyword has dual semantics: numeric cast (no check) vs refinement assertion (runtime check + `panics`); disambiguation rule and combined-conversion ordering never stated → `docs/design_audit/findings-026-050.md#f-042`
  - F-043 `gap` `closed` — refinement constraint language grammar not formally specified; only examples given; allowed predicates beyond numeric comparisons and `self.len()` undefined (const refs? modulo? field access?) → `docs/design_audit/findings-026-050.md#f-043`
- `[x]` **Distinct Types (Newtypes)** (line 2276)
  - F-044 `gap` `closed` — `distinct type T = Base where predicate` construction semantics undefined: does `T(v)` check the predicate? is `TryFrom` auto-generated? what does `.raw()` return? → `docs/design_audit/findings-026-050.md#f-044`
- `[x]` **Contracts (`requires` / `ensures` / `invariant`)** (line 2315)
  - ok — four contract evaluation rules (purity, panic-during-eval, invariant trigger sites, `old()` pre-state) all precisely specified
- `[x]` **Testing** (line 2449) — `#[test]`, property-based, snapshot
  - F-045 `gap` `closed` — `assert`/`assert_snapshot` used but never defined; property test shrinking unspecified (no `Shrink` trait); snapshot file location missing → `docs/design_audit/findings-026-050.md#f-045`
- `[x]` **Strings** (line 2605)
  - ok — String/StringSlice split, UTF-8 semantics, SSO, `s[i]` compile error all clearly specified
- `[x]` **Slices** (line 2661)
  - ok — coercion rules at call boundaries, `mut Slice[T]`, sub-range indexing all clear
- `[x]` **Byte and Byte-String Literals** (line 2709)
  - ok — `b'x'`/`b"..."` types and coercion to `Slice[u8]` specified; `String.from_utf8` conversion noted
- `[x]` **String Interpolation / f-strings** (line 2733)
  - spec-updated: `parse.[i64]()` → `parse[i64]()` in I/O example (missing `.` is a typo)
  - F-046 `ambiguity` `closed` — `Display.to_string(self)` takes owned receiver but `Debug.fmt_debug(ref self)` borrows; f-string `{expr}` would consume non-Copy values unless auto-ref is applied; asymmetry likely a spec error → `docs/design_audit/findings-026-050.md#f-046`
- `[x]` **Standard I/O Surface** (line 2779)
  - ok — function signatures, error types, buffering, and effect contracts all specified; streaming I/O explicitly deferred
- `[x]` **First-Class Functions** (line 2866)
  - F-047 `syntax-gap` `closed` — `sort[i64]` (generic specialization without a call) has no grammar production; `CALL_EXPR` requires `()` suffix → `docs/design_audit/findings-026-050.md#f-047`
  - F-048 `syntax-gap` `closed` — `Fn(T) -> U with E` effect clause in type expressions has no grammar production in spec or syntax.md → `docs/design_audit/findings-026-050.md#f-048`
- `[x]` **Closures — parameter modes, capture, escape** (line 2955)
  - ok — Rule 1/2/3 are detailed; pass ordering is spelled out; four escape sub-cases cover the key scenarios; once-callable semantics derivable from ownership
  - F-051 `ambiguity` `resolved` — bound method ref with `mut ref self` receiver: multi-callable; requires mut ref-accessible binding at creation site → `docs/design_audit_v1/findings-051-075.md#f-051`
- `[x]` **Default Parameter Values** (line 3008)
  - ok — constant-expression-only defaults, per-call evaluation, trailing-only, label-skip rule all stated
- `[x]` **Named / Labeled Function Arguments** (line 3050)
  - ok — contiguous-suffix rule, no reordering, UFCS receiver unlabeled all clear
- `[x]` **Destructuring in Function and Closure Parameters** (line 3078)
  - F-052 `ambiguity` `resolved` — explicit mode annotations not permitted on destructured parameters; always inferred from usage → `docs/design_audit_v1/findings-051-075.md#f-052`
- `[x]` **Pipe Operator `\|>`** (line 3125)
  - F-053 `ambiguity` `resolved` — `_` pipe hole is top-level only; `data |> f(g(_))` is a compile error → `docs/design_audit_v1/findings-051-075.md#f-053`
- `[x]` **Subscript Trait (`Index` / `IndexMut`)** (line 3167)
  - F-050 `contradiction` `closed` — `IndexMut` range case: spec prose says returns `mut Slice[T]` but trait signature requires `mut ref Self.Output`; three occurrences need reconciling → `docs/design_audit/findings-026-050.md#f-050`
- `[x]` **Operator Traits** (line 3204)
  - F-049 `contradiction` `closed` — `String` implements `Add` with `rhs: ref String` but `Add` trait requires `rhs: Self`; spec acknowledges deviation without resolving it → `docs/design_audit/findings-026-050.md#f-049`
  - F-054 `ambiguity` `resolved` — compound assignment on `mut ref` lvalues performs assign-through (`*a = *a + b`); local name retains `mut ref T` type → `docs/design_audit_v1/findings-051-075.md#f-054`
- `[x]` **`Hash` and `Hasher`** (line 3322)
  - ok — trait split, `BuildHasher`, stability policy, `Eq` contract, derive behavior all covered

---

### Feature 1: Data Layout (line 3369)

- `[x]` **Layout Rules**
  - F-055 `gap` `resolved` — layout-annotated collections are the same type as their plain counterpart; layout is a codegen hint only → `docs/design_audit_v1/findings-051-075.md#f-055`
- `[x]` **`#[repr]` — ABI and Physical Layout Control**
  - ok — five forms covered; interaction with layout blocks (C/packed disables SoA, pub-error vs. private-warning) specified
- `[x]` **Enum Discriminant Runtime Surface**
  - ok — `.discriminant()`, `TryFrom`, `.values()`, semver lock, payload-enum exclusions all specified

---

### Feature 2: Effect Types (line 3511)

- `[x]` **Fixed verbs and user-defined resources — core model** (line 3513)
  - F-056 `gap` `resolved` — user-defined verbs conflict on same verb + same resource; all other combinations safe → `docs/design_audit_v1/findings-051-075.md#f-056`
  - F-058 `gap` `resolved` — bare `effect resource R;` (no trait bound) is valid as annotation-only resource; cannot be used with `with_provider` → `docs/design_audit_v1/findings-051-075.md#f-058`
  - F-063 `syntax-gap` `resolved` — `@noblock` renamed to `#[noblock]` throughout spec for consistency with `#[...]` attribute syntax → `docs/design_audit_v1/findings-051-075.md#f-063`
- `[x]` **Conflict Rules** (line 3534)
  - ok — reads/writes/sends/receives/allocates/panics matrix covered; cross-category safe rule stated in prose; allocates-never-conflicts and panics-resource-less both stated
- `[x]` **Execution Effects (`blocks` / `suspends`)** (line 3565)
  - F-062 `gap` `resolved` — `blocks`/`suspends` must be explicitly declared on public fns; not default-permitted unlike `allocates(Heap)` → `docs/design_audit_v1/findings-051-075.md#f-062`
- `[x]` **Effect Inference and Boundaries** (line 3630)
  - F-057 `ambiguity` `resolved` — exhaustive `panics`-producing list defined; arithmetic overflow is NOT a `panics` atom → `docs/design_audit_v1/findings-051-075.md#f-057`
  - F-067 `ambiguity` `resolved` — transparent-verb carve-out is transitive (full call graph of transparent-verb fn, not direct calls only) → `docs/design_audit_v1/findings-051-075.md#f-067`
- `[x]` **Effect Lattice** (line 3767)
  - ok — powerset lattice formally defined; join/meet/bottom/top; monotonicity and Knaster–Tarski termination; n·m iteration bound; effect variables vs lattice elements distinction clear
- `[x]` **Mutual Recursion and SCCs** (line 3811)
  - ok — Tarjan-based SCC; reverse-topological processing; public-boundary firewall; effect-polymorphic SCC resolution all specified
- `[x]` **Effect Annotation Workflow** (line 3839)
  - ok — 5-step mechanical workflow; AI-applicable fix-diff; code-review role stated
- `[x]` **Entry Point (`main()`)** (line 3856)
  - ok — three valid return types; ExitCode; process.exit() unwind; E: Display bound; error display format; script mode; item/statement mixing; reject-ambiguous-files diagnostic all specified
- `[x]` **Built-in Primitive Resources** (line 3933)
  - ok — full primitive table; GpuBuffer[buf_id] parameterized form; Hardware catch-all; reserved CompileTimeEnv/CompileTimeHeap names stated
- `[x]` **Nondeterminism as an Explicit Resource** (line 3952)
  - ok — Clock/RandomSource/Env as blessed resources; transparent-verb carve-out; structured-logging non-exemption; provider injection for deterministic replay stated (F-067 covers a carve-out edge case)
- `[x]` **Web / Host Effect Vocabulary** (line 4019)
  - ok — capability-over-host naming; std.web resource table; reuse of native primitives; std.wasi rationale; module-gating not prelude; effect-driven target gating all stated
- `[x]` **Effect Groups and Composition** (line 4049)
  - ok — group composition with `+`; sub-group propagation; library stability via groups; group-name annotations stated
- `[x]` **Effect Polymorphism** (line 4065)
  - F-059 `gap` `resolved` — `with_provider` signature made effect-polymorphic: `Fn() -> T with R, E`; R consumed at boundary, E propagates to caller → `docs/design_audit_v1/findings-051-075.md#f-059`
  - ok otherwise — named `with E`; `with _` wildcard; multiple variables; fixed+variable composition; viral annotation rule; private inferred polymorphism; dyn Trait unbound-trait rule all stated
- `[x]` **Effect Set Subtyping** (line 4067)
  - ok — subset-inclusion ordering; covariance at function-type boundaries; pure closure as lattice bottom all stated
- `[x]` **Monomorphization Order for Compound Polymorphism** (line 4189)
  - ok — four-step resolution; types-first dependency justification; multi-variable compound case; bottom-up nested polymorphism; monomorphization identity keyed on (types, effects); error-reporting order; worked pipeline example — all formally specified
- `[x]` **Trait Coherence and Effects** (line 4335)
  - ok — subset rule for impls; trait-level `with` as method default (replaced not unioned); default body checking; private traits infer from local impls; SCC-aware inference; no-impl unbound case; public trait no-ceiling effect-opaque case all specified
- `[x]` **Closures and Effect Capture** (line 4396)
  - ok — four parameter forms; invoke vs store distinction; dataflow-based inference; struct-field invocation tracking; heterogeneous collection union; explicit opt-in for `with _` collections all specified
- `[x]` **Parameterized Resources** (line 4470)
  - F-064 `syntax-gap` `resolved` — `EFFECT_RESOURCE` and `EFFECT_ATOM` grammar extended in syntax.md §3.6 to cover parameterized forms → `docs/design_audit_v1/findings-051-075.md#f-064`
  - ok otherwise — alias tri-state (proven-disjoint / proven-identical / unproven); dominance definition; runtime partition guard; scaling; Eq+Hash key type requirements all specified
- `[x]` **Provider-Rooted Resources** (line 4540)
  - F-061 `gap` `resolved` — `with_provider` boundary consumes `with R` from caller's perspective; formally defined as "effect satisfaction" in spec → `docs/design_audit_v1/findings-051-075.md#f-061`
  - ok otherwise — why two declarations; multiple bounds; Send+Sync at call site; Arc internals; runtime stack semantics; escape-prevention rule; restructuring escape hatches; scope-rule restricted to provider-rooted resources all specified
- `[x]` **`providers { } in { }` Block** (line 4622)
  - F-060 `syntax-gap` `resolved` — `providers {} in {}` grammar defined; `in` disambiguation by preceding keyword context; semantics documented → `docs/design_audit_v1/findings-051-075.md#f-060`
  - ok otherwise — desugaring to nested with_provider; evaluation order; fail-fast; scope nesting; error handling; provider interdependency pattern all specified
- `[x]` **Resource Aliasing** (line 4689)
  - F-065 `syntax-gap` `resolved` — `alias A = B;` grammar and semantics defined; `pub` visibility, asymmetry, and scoping rules documented → `docs/design_audit_v1/findings-051-075.md#f-065`
  - F-066 `gap` `resolved` — `independent A, B;` declaration defined with syntax, semantics, and examples; trusted without verification → `docs/design_audit_v1/findings-051-075.md#f-066`
- `[x]` **Effect Semver Rules** (line 4697)
  - ok — breaking/non-breaking change table; open-contract for group-name annotations; compiler prefers group names; closed-contract for individual annotations; parallelization side-effect minor; library-side and consumer-side diagnostics; `stable effect group` modifier all specified

---

### Feature 3: Algebraic Data Types (line 5509)

- `[x]` **ADT core — enums, structs, pattern matching, exhaustiveness**
  - F-068 `syntax-gap` `resolved` — `FIELD_PATTERN` defined in syntax.md §5.6: shorthand `IDENT`, sub-pattern `IDENT: PATTERN`, and `..` rest forms → `docs/design_audit_v1/findings-051-075.md#f-068`
  - F-069 `syntax-gap` `resolved` — array/slice pattern `[p1, p2, ..rest]` added to `PATTERN` grammar in syntax.md §5.6 → `docs/design_audit_v1/findings-051-075.md#f-069`
  - F-070 `contradiction` `resolved` — design.md updated with all five range pattern forms (both-bounds-required restriction removed) → `docs/design_audit_v1/findings-051-075.md#f-070`
  - F-071 `gap` `resolved` — match arm binding modes section added: owned scrutinee may move; `ref T` scrutinee auto-borrows all bindings → `docs/design_audit_v1/findings-051-075.md#f-071`
  - F-072 `ambiguity` `resolved` — `bool` is a two-constructor type; `match flag { true => ..., false => ... }` is exhaustive without wildcard → `docs/design_audit_v1/findings-051-075.md#f-072`

---

### Feature 4: Tiered Ownership (line 5749)

- `[x]` **Ownership tiers, parameter modes, call-site markers, RC fallback, `Copy`/`Clone`/`Drop`**
  - spec-updated: `@no_rc struct` → `#[no_rc] struct` (RC budget controls section) — same `@` vs `#[...]` issue as F-063 `spec-updated`
  - F-073 `missing-error` `resolved` — `mut ref self` and bare `self` are compile errors on `shared struct` methods; explicit diagnostic added to spec → `docs/design_audit_v1/findings-051-075.md#f-073`
  - F-074 `gap` `resolved` — `shared struct` pub fn methods use auto-generated per-type `writes(TypeName)` resource, matching `par struct` rule → `docs/design_audit_v1/findings-051-075.md#f-074`
  - F-075 `gap` `resolved` — standalone `Mutex[T]` lock syntax defined; positional alias has type `mut ref T` → `docs/design_audit_v1/findings-051-075.md#f-075`

---

### Feature 5: Auto-Concurrency (line 6572)

- `[x]` **Three-layer model, `seq {}`, `par {}`, `spawn()`, `TaskGroup`, `collect_all`** (line 6572)
  - F-077 `ambiguity` `resolved` — `?` inside `par {}` is block-piercing: fires fail-fast and propagates to enclosing function; `par {}` type is always the success type → `docs/design_audit_v1/findings-076-100.md#f-077`
  - F-078 `contradiction` `resolved` — `go { ... }` was informal shorthand; all occurrences in Feature 7 replaced with canonical `spawn(|| { ... })` from Feature 5 → `docs/design_audit_v1/findings-076-100.md#f-078`
  - F-079 `missing-error` `resolved` — phase corrected to typechecker; `ScopeLocal` sealed marker trait introduced as enforcement mechanism; three forbidden positions defined → `docs/design_audit_v1/findings-076-100.md#f-079`
  - F-080 `syntax-gap` `resolved` — auto-thunking removed; explicit closures `|| expr` required; 2- and 3-arg type signatures written (pattern extends to 8); `collect_all_vec` type signature added; effect inference follows standard closure-call rules → `docs/design_audit_v1/findings-076-100.md#f-080`
  - F-081 `gap` `resolved` — "### Channels" subsection added: `Sender[T]`/`Receiver[T]` split; `channel[T](cap)` constructor; `send`/`recv` with `blocks` effect; ownership, capacity, clone, and drop semantics; `after()` return type fixed to `Receiver[()]`; deferred table updated to "channel combinators" → `docs/design_audit_v1/findings-076-100.md#f-081`
- `[x]` **Parallel Failure and Cleanup, Scheduler-layer invariants** (line 6841)
  - F-076 `gap` `resolved` — `Cancellable` trait introduced; `?` inside `par {}` requires `Err: Cancellable`; `#[derive(Cancellable)]` adds `Cancelled` variant to enums; "no special-case machinery" claim corrected → `docs/design_audit_v1/findings-076-100.md#f-076`

---

### Feature 6: Gradual Verification (line 6941)

- `[x]` **Gradual verification model**
  - ok — brief, referential section; Level 2 (refinement types) and Level 2.5 (contracts) reference already-audited sections; elision procedure exactly two rules (const-evaluable at compile time, type-identity passthrough); Level 3–4 deferred; no new gaps beyond F-042 (`as` dual semantics applies here too)

---

### Feature 7: Compilation Target Flexibility (line 6951)

- `[x]` **GPU Subset Constraints** (line 6962)
  - ok — GPU-safe vs not-safe table, `GpuSafe` trait, `#[gpu]` annotation, pre-monomorphization call-graph validation, `gpu.dispatch` effect semantics, parameterized `GpuBuffer[buf]` all clearly specified
- `[x]` **Cross-target Compilation, Effect-Driven Target Gating, Provider Injection for SSR** (line 7075)
  - ok — closed target set, target-provided resource table, effect-driven gating as primary mechanism, SSR provider injection pattern all clearly specified
- `[x]` **`#[target(...)]` escape hatch, `karac check` Under Multiple Targets** (line 7173)
  - ok — positive/negative target attribute grammar stated, no runtime `if target` form, once-per-target checking all specified; minor: `#[target(not(T1), T2)]` mixing positive and negative arguments in one attribute is syntactically allowed by the grammar description but its semantics (intersection? union?) are unstated
- `[x]` **Concurrency Across Targets, WASM lowering, Async Host APIs, Target Build Artifacts** (line 7198)
  - F-078 (already filed) covers `go { ... }` contradiction with Feature 5
  - F-081 (already filed) covers `Channel[T]` undefined
  - ok otherwise — WASM sequential default + wasm-threads opt-in, Promise/callback-as-channel patterns, native/wasm/wasi/gpu artifact contracts all specified

---

### Systems and Embedded Surface

- `[x]` **Project Profiles** (line 7332)
  - ok — built-in kernel/embedded profile restrictions, profile-as-union-of-no_effects rule, `engine`/`server` reserved names, spatial/overflow weakening explicitly opt-in, memory-safety invariants clearly stated; no_recursion available in custom profiles
- `[x]` **Secret Type (`Secret[T]`)** (line 7815)
  - ok — compiler-enforced non-implements table, access paths (`.expose()` / `.expose_mut()`), containing-type derive redaction, Clone/Drop/Zeroize semantics, `not in prelude` requirement all specified
- `[x]` **Unsafe Escape Hatch** (line 7878)
  - ok — `undocumented_unsafe` lint, `Safety:` comment rule, multi-block separate comment requirement, `#[allow]` suppression, `unsafe fn` doc `# Safety` section all stated; effects still required inside `unsafe`
- `[x]` **Volatile Memory Access (MMIO)** (line 7922)
  - ok — `volatile_read`/`volatile_write` intrinsics with `T: Copy` bound, `VolatileCell[T]` stdlib wrapper, `reads(Hardware)`/`writes(Hardware)` effect integration, per-device resource pattern all specified
- `[x]` **Inline Assembly** (line 7966)
  - ok — `asm` keyword expression (not macro), operand forms (`in`/`out`/`inout`/`inout =>`), constraint forms, all 7 options and their effect implications, `global_asm` at file scope, effect integration all stated
- `[x]` **Standard Library Layers (`core` / `alloc` / `std`)** (line 8091)
  - ok — three-layer split, `no_std`/`no_alloc` rules, profile mapping table, `format_into!` for `no_alloc` context all specified
- `[x]` **Interrupt Handler ABI (`#[interrupt]`)** (line 8152)
  - ok — `#[interrupt(NAME)]` attribute semantics, platform `Interrupt` enum pattern, ISR profile restrictions, `extern "interrupt"` escape hatch, critical section RAII guard all specified; `TIMER1_FLAG.store(true, Release)` in example uses `Atomic[T]` which is deferred — minor, example only
- `[x]` **Linker Control Attributes** (line 8218)
  - ok — `#[link_section]`, `#[no_mangle]`, `#[used]`, `#[no_mangle]` vs ABI distinction, dead-code elimination interaction all stated
- `[x]` **C Calling Convention Variants** (line 8271)
  - ok — ABI table, `"C-unwind"` and `panics` defaulted by ABI, v1 constraint note, `"interrupt"` vs `#[interrupt]` relationship all specified; other ABIs are reserved names with clear not-yet-supported behavior
- `[x]` **FFI** (line 8331)
  - ok — `extern "C"` syntax, effect defaults table, trust-not-verify rule, `#[noblock]` per-declaration and per-block forms, `blocks`/`panics`/`allocates(Heap)` annotation guidance all stated; F-063 resolved `@noblock` → `#[noblock]` rename
- `[x]` **Host Functions** (line 8393)
  - ok — `host fn` vs `extern "C"` rationale, target lowering summary, required effect declarations (no defaults), v1 parameter/return restrictions (primitives, `Copy`, opaque handles), ownership semantics for opaque handles, Component Model migration path all specified

---

### Tooling and Meta

- `[x]` **AI-First Compiler Interface** (line 7394)
  - ok — `--output=json` document shape, five diagnostic quality commitments (channel separation / root-cause grouping / cascade cap / `concept` field / `derivation` chain), error return trace ring buffer, `--output=jsonl` streaming protocol, six event types, fail-fast semantics, cascade cap preservation, superset guarantee all specified
- `[x]` **Compilation Model** (line 7664)
  - ok — four principles (function-local analysis, SCC as cache unit, named dependencies, no numeric targets pre-measurement), three known-risky passes (monomorphization, SCC fixed-point, ownership with RC fallback), phase names as public contract all stated
- `[x]` **Interactive Evaluation Model** (line 7688)
  - ok — REPL/browser playground/Jupyter kernel, cell scope rules, ownership across cells, effect semantics per-cell and session-wide, auto-clone opt-in mode, session export guarantee, `RichDisplay` trait protocol all specified
- `[x]` **Performance Diagnostics** (line 7794)
  - ok — three-tier model (inline notes / summary report / suppression), `perf[rc-fallback]` and `perf[layout-opportunity]` note formats, `#[allow(rc_fallback)]` suppression stated
- `[x]` **Deferred Items** (line 8493)
  - F-081 (already filed): channels deferred to Phase 6 ("design still open") in the Deferred Items table directly contradicts the spec body's use of `Channel[T]` in Feature 7 WASM section and "Concurrency Across Targets" — contradiction now noted in F-081
  - ok otherwise — priority notation (P0/P1/P2), deferred items table with target phases, detailed design shapes for committed-but-blocked items (effect variable bounds, SIMD vectors, Perceus, bitfields, static stack analysis) all stated; `Cross-task ? propagation` deferred to Phase 6 (consistent with F-077 being intra-`par` only)
- `[x]` **What Kāra Is Not** (line 8465)
  - ok — twelve explicit non-goals (named lifetimes, async fn, GC, cycle collector, algebraic effect handlers, field-level effects, auto-SoA, classes/inheritance, exceptions, implicit parallel conflicts, universal pitch) all clearly stated with rationale

---

## After the Sequential Audit — Fail-Fast Follow-up

Once the section-by-section pass is complete, these higher-leverage techniques will surface
the remaining cross-cutting gaps faster than another sequential read.

### 1. Write complete programs, not snippets

The sequential audit catches within-feature gaps. The bigger gaps hide at feature intersections
and only surface when you write a *complete program* that uses multiple features together.
Pick 4–5 non-trivial programs and try to fully spec every line:

- CSV parser — strings + slices + iterators + error handling
- HTTP request handler — effects + closures + async + ownership
- Type-safe config loader — refinement types + serde + error propagation
- Parallel data pipeline — effects + `par {}` + closures + RC

Every line you can't write without a spec answer is a gap.

### 2. Build a feature-interaction matrix

Most gaps live at intersections, not in individual features. Explicitly audit each pair:



|  | **Effects** | **Generics** | **Ownership** | **Layout** | **Concurrency** | **Providers** | **ADTs** |
|--|-------------|--------------|---------------|------------|-----------------|---------------|----------|
| **Effects** | F-056 ✓ | GAP-A *new* | ? | ? | GAP-S *new* | GAP-N *new* P0 | GAP-T *new* |
| **Generics** | GAP-A *new* | F-041 ✓ | GAP-C *new* | ? | ? | GAP-O *new* | ? |
| **Ownership** | ? | GAP-C *new* | F-029 ✓ | GAP-H *new* | GAP-J *new* | GAP-P *new* | GAP-M *new* |
| **Layout** | ? | ? | GAP-H *new* | F-055 ✓ | GAP-I *new* P1 | ? | ? |
| **Concurrency** | GAP-S *new* | ? | GAP-J *new* | GAP-I *new* P1 | F-059 ✓ | ? | ? |
| **Providers** | GAP-N *new* P0 | GAP-O *new* | GAP-P *new* | ? | ? | F-061 ✓ | ? |
| **ADTs** | GAP-T *new* | ? | GAP-M *new* | ? | ? | ? | F-044 ✓ |

Every `?` is an unaudited interaction. The matrix shows that **Providers × Effects** and
**Concurrency × Layout** are the densest clusters of open questions.


### 3. Prioritize Feature 2 (Effects) and Feature 4 (Ownership) for a second pass

Both are cross-cutting — they affect every function in the language. A gap there multiplies
across the entire surface. The sequential audit will cover them once, but a dedicated second
pass focused purely on adversarial interactions is worth the time.

### 4. Implementer's mindset test

For any spec section, ask: *"If I were writing the compiler, what decision would I need to make
that the spec doesn't tell me?"* Every such decision is a gap. After finishing a section, list
every branch the compiler would need — e.g., "if the type is refined AND the receiver is
`mut ref` AND the function is generic, which check runs first?" If the spec doesn't answer it,
it's a finding.

---





