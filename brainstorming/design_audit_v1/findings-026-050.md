# Design Audit Findings: F-026 – F-050

Each finding has an anchor matching its number (e.g. `#f-001`).

---

## F-027 ✓ RESOLVED

### `mut` as a field modifier is only defined for `shared struct`; plain struct fields are unaddressed

**Decision: Design A.** `mut` is not a valid modifier on plain struct fields — it is a compile
error. Plain struct field mutability is governed entirely by the receiver mode: `mut ref S`
grants write access to every field; `ref S` makes every field read-only. No per-field
mutability granularity exists for plain structs.

`docs/design.md` § Struct Field Visibility now carries an explicit "Plain struct field
mutability" paragraph stating this rule and the contrast with `shared struct`. `docs/syntax.md`
line 414 already contained the matching semantic note on the grammar.


---

## F-028 ✓ RESOLVED

### Prefix literal form `Array[1, 2, 3]` conflicts with the generic instantiation disambiguation rule

**Decision: Option A** (prefix literal as a distinct grammar production), resolved via the
existing case-class rule. `Array`, `Vec`, `Map`, `Set` etc. are Type-class identifiers and
can never be local value bindings, so `TypeClassIdent[...]` in expression context is always
a `PREFIX_LITERAL` — never an `INDEX_ACCESS`. The case-class invariant makes this unambiguous
at lex time with no lookahead.

Single-element case (`Array[42]`) is a one-element prefix literal — not an index.
`Array[i32]` where `i32` is a type name is a type error (not a parse ambiguity).
`Map[a, b]` without `:` is a compile error (Map prefix literals require `key: value` pairs).

Changes:
- `docs/syntax.md` §5.17b: new `PREFIX_LITERAL` grammar production with `COLLECTION_IDENT`,
  `SEQ_CONTENT`, `MAP_CONTENT`, `REPEAT_CONTENT`, and empty-requires-annotation rule.
- `docs/design.md` § Generic vs. index disambiguation: added Type-class carve-out, single-
  element note, and case-class-as-load-bearing-property explanation.

## Finding

`docs/design.md` § Type System states:

> **Expression context**: `expr[expr]` is always an index operation. A generic function call is
> recognized by `(` immediately following `]`: `ident[T₁, ..., Tₙ](args)`.

And § Collection Literals states:

> **Prefix-literal form `TypeName[...]`.** Any stdlib collection type accepts a prefix-literal
> form at expression position.

```kara
let xs = Array[1, 2, 3]   // prefix literal
let ys = Vec[1, 2, 3]     // prefix literal
let m  = Map["a": 1]      // prefix literal
```

These are in expression context. By the generic instantiation rule, `expr[expr]` is always an
index. `Array[1, 2, 3]` has multiple comma-separated expressions inside `[]` — which the index
rule doesn't accommodate (index takes a single expression). So the parser must use a different
rule for prefix literals.

## The ambiguity

How does the parser decide that `Array[1, 2, 3]` is a prefix literal rather than an index?

**Case 1: multiple elements** — `Array[1, 2, 3]` has commas. The index grammar `expr[expr]`
takes one expression. A comma inside `[]` is not part of the expression grammar in index
position. So multiple-element prefix literals are unambiguous: the comma forces the prefix-
literal interpretation.

**Case 2: single element** — `Array[1]` has one expression. This matches `expr[expr]` (index
into `Array` with index `1`). But `Array` is a Type-class identifier — types are not values
that can be indexed. The parser doesn't do type-checking, so it sees a valid index expression
syntactically. Does the parser treat `Array[1]` as:
- A prefix literal producing `Array[i64, 1]` (one element), or
- An index into the type name `Array` (syntactically valid, semantically wrong)?

**Case 3: single type argument** — `Array[i32]` — `i32` is a type name. Is this:
- Generic instantiation: `Array` parameterized with `i32` (but no `(` follows, so it's not a call), or
- An index into `Array` using `i32` as the index expression (semantic error — `i32` is not a value)?

The spec says: "`sort[i32]` alone is an index into `sort`, not a generic reference." By analogy,
`Array[i32]` alone (without `(`) is an index — but `Array` is not an array value. This would
be a type error, not a prefix literal.

So the single-element prefix literal `Array[1]` and the single-type generic reference
`Array[i32]` both parse as index expressions, and neither is a prefix literal. This means
single-element prefix literals cannot be written without a binding annotation:

```kara
let xs = Array[42]           // parsed as index into Array with 42 — type error
let xs: Array[i64, 1] = [42] // must use bare form with annotation
```

Is this intended? The spec doesn't address the single-element case.

## Map prefix literals with `:`

`Map["a": 1, "b": 2]` — the `:` inside `[]` is not part of any existing grammar production
(not a type annotation, not a range). The parser must recognize `key: value` as a special
production inside Map literal brackets. But since `[...]` with commas is already ambiguous
(Vec literal vs Map literal via the `:` rule for bare `[...]`), the prefix form `Map[...]`
should use the same key-value rule.

Does `Map[a, b]` (no `:`) produce a compile error or is it ambiguous between Set-like and
Map-like? For bare `[...]`, commas without `:` → Vec; first `:` → Map. For prefix `Map[...]`,
the `:` rule should be enforced — `Map[a, b]` without `:` should be a parse error.

## Options

**Option A — Specify the prefix literal as a distinct grammar production.**
Add to the grammar: `PREFIX_LITERAL = TYPE_IDENT "[" LITERAL_ELEMENTS "]"` where
`LITERAL_ELEMENTS` is always the collection-element grammar, never the generic-argument grammar.
The parser recognizes known stdlib collection names in this position and routes to the literal
production. Single-element case: `Array[42]` is a prefix literal (element `42`).

**Option B — Require multiple elements or explicit annotation for prefix literals.**
Single-element prefix literals are not supported; use annotation form or `Array.filled(1, val)`.
`Array[1, 2, 3]` (multi-element) is unambiguous; `Array[1]` is an index expression (error).

## Decision needed from author

Clarify the parser's disambiguation rule for single-element prefix literals. Update the grammar
in `docs/syntax.md` to show the prefix-literal production explicitly.


---

## F-029 ✓ RESOLVED

### `Option[ref T]` in collection method signatures: `ref` inside generic type arguments

**Decision: Interpretation B.** `ref T` is a first-class type (already in the `syntax.md`
TYPE grammar at §6.1) and is valid in any type position, including generic arguments in
return types (`Option[ref T]`, `Result[ref T, E]`). The collection method signatures are
literal Kāra syntax, not informal notation.

Borrow-tracking rules are identical to plain `ref T` returns (Part 3). The compiler traces
the inner `ref T` back to `ref self` using the single-source rule. `Vec[ref T]` is also
syntactically valid but makes the vector a borrowed struct — its scope is bounded by the
shortest-lived element source. In practice the intended pattern for temporary element borrows
is `iter()`, not a stored `Vec[ref T]`.

Change: `docs/design.md` Part 3 now has an explicit "`ref T` inside generic wrappers in
return types" paragraph explaining the rule and the `Vec[ref T]` edge case.

## Finding

`docs/design.md` § Collection Core Methods shows:

```kara
fn get(ref self, idx: i64) -> Option[ref T]   // Vec.get
fn get(ref self, key: ref K) -> Option[ref V] // Map.get
```

`ref T` appears as a generic type argument to `Option`. The spec defines `ref` as a **parameter
mode** in function signatures — `ref T` means a borrowed parameter, not an owned one. But here,
`ref T` appears inside `Option[...]`, which is a generic type argument position, not a function
parameter position.

The spec never states whether `ref` is a valid type-level modifier that can appear inside
generic type arguments.

## Why this matters

In Rust, the equivalent is `Option<&T>` — a reference is a first-class type (`&T`) that can
appear in any type position. In Kāra, `ref T` is described as a parameter mode annotation,
not a first-class type. If `ref` is only a parameter mode, it should not appear inside
`Option[...]`.

If `ref T` is valid inside generic type arguments, it means borrows are a first-class type
in Kāra (similar to Rust's `&T`), which has significant implications:

```kara
// If Option[ref T] is valid, are these also valid?
let x: Option[ref String] = Some(ref s)    // Option of borrowed string
let xs: Vec[ref i64] = [ref a, ref b]      // Vec of borrowed integers
fn process(items: Vec[ref T]) -> ()        // Vec parameter holding borrows
```

A `Vec[ref T]` would be a collection of borrowed references, all tied to some lifetime. This
is a powerful but complex feature (Rust's lifetime system exists entirely to manage this). Does
Kāra support this? If not, `Option[ref T]` in the method signatures may be an informal notation
meaning "returns a reference to an element, expressed as Option" rather than a literal type.

## Two interpretations

**Interpretation A — `ref T` in generic position is informal notation.**
`Option[ref T]` in the method table is shorthand describing the semantic intent (the returned
element is borrowed, not owned). The actual return type might be something like a special
`BorrowedOption[T]` or just documented as returning a borrow. The notation is not literally
valid Kāra syntax.

**Interpretation B — `ref T` is a first-class type modifier valid anywhere.**
`Option[ref T]` is literal Kāra syntax. Borrows (`ref T`) are first-class types that can appear
in generic positions. The spec needs to define the type-level semantics of `ref T` beyond its
role as a parameter mode.

## Adversarial example

```kara
fn find_first(v: ref Vec[i64], pred: Fn(i64) -> bool) -> Option[ref i64] {
    for item in v.iter() {
        if pred(*item) { return Some(item) }
    }
    None
}
```

Is this valid Kāra? Can `Some(item)` where `item: ref i64` produce `Option[ref i64]`?

## Decision needed from author

State explicitly whether `ref T` is a valid type modifier in generic argument positions. If
yes, define the full type-level semantics. If no, replace the method signatures in the spec
with the correct notation (e.g., a special borrowed-wrapper type, or a different return type
for `get`).


---

## F-031 ✓ RESOLVED

### Tensor multi-index `t[i, j, k]` syntax conflicts with the single-index grammar rule

**Decision:** `t[i, j, k]` is syntactic sugar for `t[(i, j, k)]` — the parser folds
comma-separated index expressions into a tuple and calls `Index.index` / `IndexMut.index_mut`
with that tuple. `Tensor[T, [M, K, N]]` implements `Index[(i64, i64, i64)]`. The two forms
are exactly equivalent.

`t[i]` on a rank-3 tensor is a compile error — no `Index[i64]` impl exists for rank > 1.
Per-axis slicing (`t[i, :, :]`) is deferred to v1.5.

Changes:
- `docs/syntax.md` §5.10: `INDEX_ACCESS` extended to `EXPR "[" EXPR { "," EXPR } "]"` with
  multi-index desugaring note and updated `PLACE_EXPR` production.
- `docs/design.md` § Tensor Indexing: explicit desugaring statement and `t[i]` compile-error note.
- `docs/design.md` § Subscript Trait desugaring table: added multi-index and multi-index-assign rows.

## Finding

`docs/design.md` § Tensor shows:

> `t[i, j, k]` is multi-dimensional indexing on a rank-3 tensor.

But § Type System establishes:

> **Expression context**: `expr[expr]` is always an index operation.

The grammar `expr[expr]` takes a single expression. `t[i, j, k]` has three comma-separated
expressions inside `[]`. Like the prefix-literal case (F-028), commas inside `[]` are not part
of the single-index grammar. The parser needs a separate rule to handle tensor indexing.

## Interaction with the Index trait

§ Subscript Trait (`Index` / `IndexMut`) defines how `[]` is lowered:

> `t[key]` desugars to `t.index(key)` / `t.index_set(key, val)`

For `t[i, j, k]`, the key would be a tuple `(i, j, k)`. So the lowering could be:
- `t[(i, j, k)]` — explicit tuple syntax (always unambiguous, one expression)
- `t[i, j, k]` — syntactic sugar for the tuple form (requires grammar extension)

The spec shows `t[i, j, k]` as the canonical form, implying it is first-class syntax,
not just `t[(i, j, k)]`.

## Adversarial examples

```kara
// These should all be equivalent — or are only some valid?
t[1, 2, 3]          // shown in spec as valid multi-index
t[(1, 2, 3)]        // explicit tuple — valid?
t[1][2][3]          // chained single-index — valid? returns Tensor[T, [5]] from Tensor[T, [3,4,5]]?

// Ambiguity with 1-element tuple
t[(1,)]             // 1-tuple index — valid?
t[1]                // single i64 index — indexes first dim? or error for rank-3?
```

## Connection to F-028

This is the runtime analog of F-028 (prefix literal disambiguation). Both reveal that the
spec establishes a simple `expr[expr]` grammar and then introduces special cases (prefix
literals, tensor indexing) that require commas inside `[]` without stating how the parser
handles them.

## Decision needed from author

1. Is `t[i, j, k]` syntactic sugar for `t[(i, j, k)]` (tuple desugaring), or is it a separate
   grammar production?
2. What does `t[i]` mean for a rank-3 tensor — is it a single-dim slice returning
   `Tensor[T, [4, 5]]`, or a compile error?
3. Update `docs/syntax.md` to show the tensor index production explicitly.


---

## F-032 ✓ RESOLVED

### Two `?` dims that must match (e.g. `K` in matmul) have no specified runtime check

**Decision:** The compiler inserts a runtime equality assertion at the call site whenever two
`?` dims unify to the same generic `Dim` parameter. Failure panics with a shape-mismatch
message and contributes `panics` to the calling function. When one side is concrete and the
other is `?`, a bounds check against the static value is emitted instead. When both sides
are concrete, the check is resolved at compile time and emits no code.

Change: `docs/design.md` § Dynamic-dim unification — new "Runtime equality check for unified
`?` dims" paragraph with the matmul example showing the inserted assertion and panic message.

## Finding

`docs/design.md` § Tensor — Dynamic-dim unification states:

> When a call site has a `?` in one position, it unifies with any concrete or generic `Dim`
> on the other side and degrades the result's corresponding position to `?`.

But the example shows:

```kara
let a: Tensor[f64, [3, ?]] = ...;   // a's K dim is ?
let b: Tensor[f64, [?, 5]] = ...;   // b's K dim is ?
let c = matmul(a, b);               // inferred: Tensor[f64, [3, 5]]
```

In `matmul[M, K, N]`, the `K` dim appears in both `a`'s second position and `b`'s first
position. Both are `?` at the call site. The spec says the result is `Tensor[f64, [3, 5]]`
and both `?`s unify with the concrete dims. But `M=3` (from `a`'s first dim) and `N=5`
(from `b`'s second dim) are concrete. What about `K`?

`a`'s K is `?` (runtime value), `b`'s K is `?` (runtime value). For the matmul to be valid,
`a.shape[1]` must equal `b.shape[0]` at runtime. The spec says dynamic dims "degrade
gracefully" — but degrading gracefully does NOT mean skipping the shape check. Two `?` dims
that must be equal still require a runtime assertion.

The spec does not say:
1. Whether a runtime check is inserted for the `K` dimension.
2. What error fires if `a.shape[1] != b.shape[0]` at runtime.
3. Whether the result shape `[3, 5]` is correct when `K` is dynamic (it is — `K` cancels out
   in matmul's output shape, but the check must still happen).

## Adversarial example

```kara
let a: Tensor[f64, [3, ?]] = Tensor.zeros([3, 4])
let b: Tensor[f64, [?, 5]] = Tensor.zeros([7, 5])   // K=7 != K=4

let c = matmul(a, b)   // should panic: K mismatch (4 != 7)
                        // but the spec doesn't say this
```

Without a runtime check, `matmul(a, b)` would silently produce undefined behavior (reading
out-of-bounds memory) if K dimensions don't match.

## What needs to be specified

1. **Runtime check insertion rule.** When two `?` dims unify (must be equal), the compiler
   inserts a runtime assertion `assert_eq(a.shape[i], b.shape[j], "K dim mismatch")` before
   the operation. This is analogous to how Rust's slice indexing panics on out-of-bounds.

2. **Error type and message.** What does the panic message say? What is the error code?

3. **Static vs dynamic dispatch.** When one side is concrete and the other is `?`, the static
   concrete value is compared against the runtime value — the compiler may emit this as a
   bounds check rather than a full equality check.

## Decision needed from author

State explicitly: for every pair of `?` dims that the type system requires to be equal (because
they map to the same generic `Dim` parameter), the compiler inserts a runtime equality check.
Failure panics with a shape-mismatch diagnostic. Add an example showing the runtime-check
behavior.


---

## F-033 ✓ RESOLVED

### "Integer overflow always traps in all build modes" conflicts with the profile-level wrapping description

**Decision: Reading A.** The `embedded` profile genuinely changes bare arithmetic operator
behavior to two's-complement wrapping throughout (not just in `unsafe` blocks). The Guarantees
table (line 102) was already correct; the contradiction was in the § Arithmetic Overflow text.

Changes to `docs/design.md` § Numeric Semantics / Integer overflow:
- Fixed the opening claim from "Always traps in all build modes" to the accurate rule:
  traps by default in `app`/`lib`; wraps by default in `embedded`.
- Removed the misleading "only for `unsafe` MMIO registers" qualification — the `embedded`
  profile's wrapping default applies project-wide, not only to unsafe blocks.
- Clarified that named methods remain available in all profiles for the non-default behavior.

## Finding

`docs/design.md` § Numeric Semantics states in two places:

**Place 1:**
> **Integer overflow:** Always traps (runtime error) in all build modes. No silent wrapping.

**Place 2 (profile table description):**
> the profile default (`checked` arithmetic in `app`, `checked` in `lib`, `wrapping` only for
> `unsafe` MMIO registers in `embedded`) sets the behavior of the bare `+`/`-`/`*`/`/`/`%`/
> `<<`/`>>` operators

"Always traps in all build modes" is an absolute guarantee with no exceptions. But the profile
description says the `embedded` profile allows `wrapping` for bare operators (at least for MMIO
registers in `unsafe`). These are contradictory.

## The two plausible readings

**Reading A — "Always traps" is the default; profiles and `unsafe` can change it.**
The `embedded` profile + `unsafe` block allows wrapping arithmetic via the bare operators.
"Always traps" applies only to safe code in non-embedded profiles. This is Rust's model:
debug = trap, release = wrap (though Kāra takes a different position).

**Reading B — "Always traps" is a guaranteed-layer invariant; only named methods escape.**
The bare `+`, `-`, `*`, `/`, `%` always trap in ALL contexts (including `embedded` and `unsafe`).
The `embedded` profile's "wrapping" is only accessible through `.wrapping_*()` methods, never
through bare operators. "wrapping only for `unsafe` MMIO registers" refers to using
`.wrapping_add()` etc., not to the bare `+` operator.

Reading B is consistent with "in all build modes" and with the statement "These are the only
escape hatches from the default-trap behavior in v1." If `.wrapping_*` are the ONLY escapes,
then bare operators in `embedded`+`unsafe` still trap.

## Why the profile description is confusing

The phrase "sets the behavior of the bare `+`/`-`/`*`/`/`/`%`/`<<`/`>>` operators" strongly
implies the profile changes what bare operators do. If profiles DON'T change bare operator
behavior, the sentence is misleading.

## Likely intent

Reading B is probably correct: overflow always traps, no profile changes bare operator behavior,
the only escapes are named methods. The profile description was written loosely.

## Decision needed from author

Clarify: do profiles ever change the behavior of bare arithmetic operators? If not, the profile
table description should say: "Bare operators always trap regardless of profile. The profile
column shows which arithmetic *method family* is preferred in documentation and lint guidance
for each profile — not a behavior change in the operators themselves." If profiles DO change
bare operator behavior (Reading A), state which profiles allow wrapping bare operators and
under what conditions.


---

## F-034 ✓ RESOLVED

### `Column[f64]` dual null conventions: NaN-as-null and bitmap-null propagate differently

**Decision:** SQL null semantics apply only to bitmap-null elements. NaN values propagate
under IEEE 754 arithmetic — they are never promoted to bitmap-null implicitly. No operations
unify the two conventions automatically. Users must normalize NaN-as-null to bitmap-null
explicitly via `fillna(..., treat_nan_as_null = true)` before relying on `null_count()` or
bare `fillna` as correctness signals.

Change: `docs/design.md` § Column — new paragraph after "Null propagation uses SQL semantics"
stating the NaN/bitmap distinction, the bitmap consequence, the normalization pattern, and the
explicit note that no operation silently unifies the two.

## Finding

`docs/design.md` § Column states:

> **NaN as a float convention.** For `Column[f64]`, users may additionally rely on IEEE-754
> NaN as a per-element sentinel...
> **Null propagation uses SQL semantics:** `null + x = null`, `null == null = null`

Two independent null conventions exist simultaneously:
1. **Bitmap null** — Arrow's canonical nullability, propagates via SQL semantics
2. **NaN-as-null** — IEEE float convention, propagates via IEEE arithmetic semantics

Under IEEE arithmetic: `NaN + x = NaN`. Under SQL semantics: `null + x = null`. Both produce
the same value in this case — but the propagation rule is **different**:

```kara
let a: Column[f64] = ...   // a[0] = NaN (not bitmap-null, just NaN)
let b: Column[f64] = ...   // b[0] = 2.0

let c = a + b              // What is c[0]?
// Under IEEE: NaN + 2.0 = NaN  ✓ (NaN propagates)
// Under SQL:  not-null + 2.0 = 2.0  (a[0] is not bitmap-null, so SQL treats it as a value)
```

If `a[0]` is NaN but **not** bitmap-null, SQL semantics says it's a valid (non-null) value and
produces `NaN + 2.0 = NaN` (IEEE arithmetic applies to valid elements). This is consistent.

BUT: the user may believe NaN means null and expect SQL semantics (`null + x = null`, giving
`null` with the null bitmap set). Instead they get `NaN` (not bitmap-null). The two conventions
produce the same numeric value but differ in the bitmap:

```kara
let c = a + b
c.null_count()    // 0 — no bitmap nulls (NaN propagated as NaN, not as bitmap-null)
c.fillna(0.0)     // no-op — no bitmap nulls to fill
c.fillna(0.0, treat_nan_as_null = true)   // replaces NaN with 0.0
```

The subtle bug: code that checks `null_count()` or uses `fillna()` without `treat_nan_as_null`
will silently treat NaN-originated values as valid data.

## The coherence gap

The spec says "null propagation uses SQL semantics" but this only applies to **bitmap nulls**.
NaN values in `Column[f64]` propagate under **IEEE arithmetic**, not SQL semantics. The spec
doesn't say this clearly — "null propagation uses SQL semantics" reads as applying to all
nulls, including NaN-as-null.

Operations that need to handle both require the programmer to always explicitly choose:
```kara
prices.fillna(0.0, treat_nan_as_null = true)   // explicitly normalize first
let result = prices + other                     // now safe — no NaN-as-null ambiguity
```

But the spec doesn't require this normalization or warn about the coherence gap.

## Decision needed from author

1. State explicitly: "SQL null propagation applies only to bitmap-null elements. NaN values
   in `Column[f64]` are treated as valid (non-null) values under arithmetic unless
   `treat_nan_as_null` is explicitly set. NaN propagates under IEEE arithmetic rules, not SQL."

2. Consider: should there be a lint warning when a `Column[f64]` is used in arithmetic without
   first normalizing NaN-as-null (e.g., via `.fillna(..., treat_nan_as_null = true)`)?

3. Are there any operations where the two conventions ARE unified (SQL semantics applied to
   NaN without explicit treatment)? If so, list them.


---

## F-035 ✓ RESOLVED

### Uninitialized `let x: T;` has no grammar production; flow analysis undefined

**Decisions:**
1. Added `LET_UNINIT_STATEMENT = "let" [ "mut" ] IDENT ":" TYPE ";"` to `docs/syntax.md`.
   Requires explicit TYPE (for stack allocation), plain IDENT only (no destructuring).
2. Also added `[ "mut" ]` to `LET_ELSE_STATEMENT` (omission was a bug — see F-036).
3. DA analysis is **flow-sensitive**: both-branches-assign → DA; partial assign → error;
   loop body → never satisfies DA for post-loop code (zero-iteration case); `loop` with
   always-assigns-before-break → DA. Struct field-by-field writes do not satisfy DA
   (consistent with the Array rule already in the spec).
4. First assignment to an uninit `let x: T;` is **initialization**, not reassignment — does
   not require `mut`. Subsequent writes require `mut` as usual.

Changes: `docs/syntax.md` grammar productions; `docs/design.md` § Variable Binding Rules —
expanded "Explicit initialization required" with first-assignment rule and full DA flowchart.

## Finding

`docs/design.md` § Variable Binding Rules shows:

```kara
let x: i64;          // declared but not usable
print(x);            // ERROR: x is not initialized
x = 5;
print(x);            // OK
```

`docs/design.md` § Array (line 882) cross-references this pattern:

> `let arr: Array[T, N];` without an initializer declares the array with `N` slots reserved on
> the stack, none initialized. Consistent with the **scalar definite-assignment rule**...

But `docs/syntax.md` grammar:

```
LET_STATEMENT = "let" [ "mut" ] PATTERN [ ":" TYPE ] "=" EXPR ";"
```

The `= EXPR` is mandatory — there is no grammar production for `let x: T;` without an
initializer. Both the design doc's examples and its Array section treat it as valid syntax, but
the grammar does not support it.

## Two missing elements

### 1. Grammar production

The syntax.md needs either:
- A separate `LET_UNINIT_STATEMENT = "let" [ "mut" ] IDENT ":" TYPE ";"` production
- Or `LET_STATEMENT` changed to `"let" [ "mut" ] PATTERN [ ":" TYPE ] [ "=" EXPR ] ";"`

The second form has parsing consequences: `let x: i64` (no `=`, no `;`) would now be ambiguous
with the start of `let x: i64 = ...`. Likely the uninit form requires the pattern to be a plain
`IDENT` and the type annotation to be present (you need the type to know how to allocate stack
space).

### 2. Definite-assignment flow analysis rules

The spec only shows the trivial case (`let x; ... use(x)` with no intervening branches). The
analysis for conditional code is undefined:

```kara
let x: i64;
if condition { x = 5; }
print(x);              // What happens? "possibly uninitialized"? always an error? allowed?

let y: i64;
if cond_a { y = 1; } else { y = 2; }
print(y);              // Should be OK — definitely initialized on both branches. Is it?

let z: i64;
for _ in 0..5 { z = 42; }
print(z);              // After loop — possibly-zero-iterations: what does the analyzer say?
```

The spec doesn't state:
1. Whether definite-assignment analysis is **flow-sensitive** (tracks branches individually) or
   **conservative** (any conditional path = possibly uninitialized).
2. What happens for a `for`/`while` loop: if the loop body always assigns `z`, but the loop
   might not execute at all, is `z` considered initialized after the loop?
3. Whether the analysis tracks partial initialization of struct fields:
   ```kara
   let p: Point;
   p.x = 1;
   p.y = 2;
   print(p);  // All fields written — is p considered fully initialized?
   ```
   (The Array section says per-slot writes don't satisfy DA for `Array`; is the same true for structs?)

### 3. First-assignment semantics for immutable `let`

The example shows `let x: i64; x = 5;` — the assignment `x = 5` is done on a `let` (not `let
mut`) binding. But the spec also says: "Reassigning the binding... is a compile error" for
immutable `let`. Is `x = 5` a reassignment or an initialization?

The intent is almost certainly: "the first assignment to an uninitialized binding is
initialization, not reassignment, and does not require `mut`." But the spec never states this
distinction. If not stated, a reading of "any assignment to a `let` binding is an error" would
make the entire uninitialized form useless.

## Decision needed from author

1. Add the uninitialized declaration form to `docs/syntax.md` (with exact grammar production).
2. Define the definite-assignment analysis: flow-sensitive or conservative? What are the rules
   for conditional branches, loops, and struct fields?
3. Add a sentence to Variable Binding Rules clarifying: "The first assignment to an uninitialized
   `let x: T;` binding is its initialization and does not require `mut`. Subsequent assignments
   require `let mut`."


---

## F-036 ✓ RESOLVED

### `let mut` with destructuring patterns and `let...else` omits `mut`

**Decisions:**
1. `let mut PATTERN = expr` — `mut` applies uniformly to every binding the pattern
   introduces. No per-binding `mut` inside patterns in v1. Use shadowing for mixed mutability.
2. `LET_ELSE_STATEMENT` — `[ "mut" ]` was added in F-035 (the omission was a bug).
3. `mut` is also not valid inside `match` arm patterns or `if let` / `while let` patterns;
   use a `let mut` rebind inside the arm body.

Changes:
- `docs/design.md` § Variable Binding Rules — new "`let mut` with destructuring patterns"
  paragraph with examples of uniform-mut and the shadowing workaround.
- `docs/syntax.md` §5.6 Patterns — new "`mut` and patterns" note stating `mut` is not valid
  inside a pattern and applies only at the `LET_STATEMENT` / `LET_UNINIT_STATEMENT` level.

## Finding

### Part 1 — `let mut` with destructuring

`docs/design.md` shows `let mut y = 5;` for a scalar. The grammar:

```
LET_STATEMENT = "let" [ "mut" ] PATTERN [ ":" TYPE ] "=" EXPR ";"
```

`mut` is a single modifier before the entire pattern. But the spec never says what `let mut`
means when the pattern is a destructuring:

```kara
let mut (a, b) = (1, 2);       // mut before tuple pattern — are both a and b mutable?
let mut Point { x, y } = p;   // mut before struct pattern — are both x and y mutable?
let mut Ok(v) = result;        // mut before enum variant — is v mutable?
```

In Rust, `let mut (a, b) = ...` makes both `a` and `b` mutable. Kāra's grammar places `mut`
before the pattern and has no `mut` inside pattern bindings — so the question is whether:

**Option A — `mut` before pattern = all bindings in pattern are mutable.** `let mut (a, b)` →
both `a` and `b` are `let mut`. This is Rust's behavior, but Rust also allows `let (mut a, b)`
for selective mutability.

**Option B — `mut` before pattern = pattern root is mutable.** The pattern root binding gets
`mut`; inner bindings do not. But "pattern root" is undefined for `(a, b)` — there is no
single binding.

**Option C — Selective `mut` inside patterns is supported.** The grammar must be extended to
allow `mut` before individual `IDENT` bindings within a pattern (like Rust). But the spec
grammar does not show this.

Without knowing which option applies, `let mut (a, b) = (1, 2); a = 5;` has undefined
behavior — is it an error or valid?

### Part 2 — `let...else` omits `mut`

`docs/syntax.md`:

```
LET_ELSE_STATEMENT = "let" PATTERN "=" EXPR "else" BLOCK ";"
```

There is no `mut` in this production. But a use-case like:

```kara
let mut Ok(config) = load_config(path) else {
    return Err("config load failed");
};
config.timeout = 30;  // needs config to be mutable
```

is clearly valid intent. Without `mut`, `config` is immutable and `config.timeout = 30;` would
be an error. The grammar must either:
- Allow `mut` in `let...else`: `"let" [ "mut" ] PATTERN "=" EXPR "else" BLOCK ";"`
- Or require users to re-bind after the fact: `let Ok(config) = ...; let mut config = config;`
  (which is noisy and the shadowing form may not work since `config` is immutable after the
  first binding and the second uses a move).

The omission appears unintentional — `let` and `let mut` are symmetric everywhere else.

## Decision needed from author

1. Does `let mut (a, b) = ...` make all bindings mutable, or only the first-level binding?
2. Is selective per-binding `mut` inside a pattern (`let (mut a, b) = ...`) supported? If so,
   update the PATTERN grammar to allow `[ "mut" ] IDENT` in binding positions.
3. Add `[ "mut" ]` to `LET_ELSE_STATEMENT` in `docs/syntax.md` (if the omission is unintentional).


---

## F-037 ✓ RESOLVED

### `if` without `else` type constraint unspecified; or-pattern binding consistency in `if let` undefined

**Decisions:**
1. `if` without `else` has type `()`. The then-block must produce `()` — a non-unit then-block
   without an `else` is a type error even in statement position (no silent discard).
2. `else if let` is valid (the grammar already supported it via `IF_EXPR` recursion);
   an example was added to `docs/design.md` § `if let` and `let...else`.
3. Or-pattern binding consistency: every alternative must bind the same set of names with
   the same types — mismatched names or types are a compile error. `syntax.md` §5.6 already
   stated this; `docs/design.md` now has the rule and error examples in the `if let` section.

Changes: `docs/design.md` § Conditionals — new "`if` without `else`" paragraph; § `if let`
and `let...else` — new `else if let` example and or-pattern consistency note.

## Finding 1 — `if` without `else` as an expression

`docs/design.md` says `if` is an expression that "returns a value." The grammar in `docs/syntax.md`:

```
IF_EXPR = "if" EXPR BLOCK [ "else" ( IF_EXPR | BLOCK ) ]
```

The `else` branch is optional. But the spec never says what the type of the expression is when
`else` is absent. Concrete programs that are unspecified:

```kara
// Used as a statement — almost certainly OK; block produces ()
if cond { side_effect(); }

// Used as an expression — what is the type?
let x: i32 = if cond { 5 };         // ERROR? (no else, type mismatch)
let y = if cond { 5 };               // What type does y have? i32? ()? ill-typed?

// Both branches present — clear
let max = if a > b { a } else { b };  // type is the LUB of branches ✓
```

The expected rule (standard in typed languages): "an `if` without `else` is only well-typed if
the then-block produces `()` (unit). In a value-expression context, `if` without `else` is a
type error." The spec doesn't state this.

If the block is:
```kara
if cond { x + 1 }   // block produces i32, no else
```
...is this always a type error, or only a type error when used in expression position?
When used as a statement, is it silently discarded?

## Finding 2 — `else if let` is supported but undocumented in design.md

The grammar allows `else` followed by another `IF_EXPR`:
```
IF_EXPR = ... [ "else" ( IF_EXPR | BLOCK ) ]
```
`IF_EXPR` includes the `"if" "let"` form, so `else if let` is grammatically valid. The
syntax.md examples show `else if other_condition` (plain `if`). But `design.md` § Conditionals
never mentions `else if let`:

```kara
if let Some(a) = try_a() {
    use(a);
} else if let Some(b) = try_b() {   // is this valid? spec never says
    use(b);
} else {
    fallback();
}
```

Almost certainly valid (since the grammar supports it), but `design.md` is silent on it.

## Finding 3 — Or-patterns in `if let` with mismatched bindings

The spec shows:
```kara
if let Left(x) | Right(x) = val {
    use(x);
}
```
Both alternatives bind the same name `x`. But the spec never states whether all alternatives in
an or-pattern **must** bind the same set of names with the same types (as Rust requires):

```kara
// Mismatched names — is this a compile error?
if let A(x) | B(y) = val {
    // which is in scope? x? y? both? neither?
    use(x);
}

// Mismatched types for the same name — compile error?
if let A(x: i32) | B(x: i64) = val {
    use(x);   // x: ??
}
```

Without a rule, the user doesn't know whether `x` and `y` are independently bound (and the
compiler would have to synthesize a union type) or whether this is always an error.

## Decision needed from author

1. State explicitly: "An `if` without an `else` branch has type `()`. The then-block must
   produce `()`; if it produces a non-unit value, the expression is a type error. In statement
   position, a missing-else `if` with a non-unit block is still a type error — the value is not
   silently discarded."
2. Add an `else if let` example to design.md § Conditionals (since the grammar already supports it).
3. State the or-pattern binding consistency rule: "All alternatives in an or-pattern must bind
   the same set of names with the same types. An or-pattern with mismatched names or types is a
   compile error." Add this to the Conditionals section and the pattern grammar in syntax.md.


---

## F-038 ✓ RESOLVED

### `break expr` on non-`loop` forms undefined; conditional `break` loop type unspecified

**Decisions:**
1. `break expr` with a non-`()` value inside `while` or `for` is a compile error. `while`
   and `for` always have type `()`. Plain `break` / `break ()` is valid in any loop.
2. `loop` type is the LUB of all reachable `break` value types. Conditional `break` still
   contributes its type — no implicit `Option` wrapper. Mismatched break types are a compile
   error. No reachable `break` → type `Never`.
3. `for x in collection` requires `Iterable`. Types implementing only `IntoIterator` require
   explicit `.into_iter()`. A stdlib blanket `impl[I: Iterator] Iterable for I` makes
   `for x in col.into_iter()` work (see F-039 for full blanket-impl treatment).

Changes: `docs/design.md` § Loops — replaced vague "only meaningful with `loop`" sentence
with explicit compile-error rule, full `loop` type inference rules with examples, and
clarified `for` desugaring requirement with `Iterable` vs `IntoIterator` guidance.

## Finding 1 — `break expr` on `while` and `for` loops

`docs/design.md` § Loops states:

> `break expr` provides the loop's return value (**only meaningful with `loop`**).

But the spec says "only meaningful" — not "only valid" or "a compile error." This leaves three
questions unanswered:

**Is `break expr` in a `while` or `for` loop a compile error?**

```kara
let x = while cond {       // for loops too
    break 5;               // compile error? warning? value silently discarded?
};
```

**If it is not an error — what type does `while`/`for` evaluate to?**

```kara
let x = while cond { break 5; };
// x: i32? ()? ill-typed?
```

**What type does `break label expr` have when the named loop is `while`/`for`?**

```kara
outer: while cond {
    inner: for item in items {
        break outer 42;    // targeting a while loop — error? meaningless?
    }
}
```

The spec specifies that `loop { break v }` produces the type of `v`, but is silent on whether
`while`/`for` are expressions at all (other than through the `loop` → `break` path).

## Finding 2 — Conditional `break` and loop type inference

```kara
let x = loop {
    if cond {
        break 5;      // conditionally reachable break
    }
    // may loop forever if cond is never true
};
// What is the type of x?
```

The spec says: "A `loop` with no reachable `break` has type `Never`." But here there IS a
reachable `break` — it's just guarded by a condition. The spec doesn't say:

1. Whether `x` has type `i32` (the break value type), `Never`, or `Option[i32]`.
2. Whether having *some* but not *all* code paths breaking with a value changes the loop's type.
3. What happens if two breaks produce values of different types:
   ```kara
   let x = loop {
       if cond_a { break 5i32; }
       if cond_b { break "hello"; }   // type mismatch? LUB?
   };
   ```

## Finding 3 — `for` desugaring and `IntoIterator` vs `.iter()` contract

The spec says `for pattern in collection` desugars to "`.iter()` followed by repeated `.next()`
calls — the collection is borrowed, never consumed." But:

1. What trait must `collection` implement? The spec mentions `IntoIterator` in the Iterator
   section, but the desugaring here says `.iter()` (a concrete method call), not `into_iter()`.
   These are different in Rust — `.iter()` borrows and returns `&Item`; `into_iter()` may
   consume. Which is it for Kāra?
2. If the desugaring is `.iter()`, does `for x in some_custom_type` require a `.iter()` method
   or an `IntoIterator` implementation?
3. What is the error if the collection has no `.iter()` method and no `IntoIterator` impl?

## Decision needed from author

1. State whether `break expr` in `while`/`for` is a compile error. If so, add the diagnostic.
   If not, state the type of the loop expression and whether the break value is discarded.
2. Define loop type inference for conditional breaks: when a `loop` has a `break v` on some
   paths, does the loop's type become the type of `v` (requiring all `break`s to agree), or does
   the compiler require at least one unconditional `break`?
3. Clarify the `for` loop desugaring: does the collection need a `.iter()` method, an
   `IntoIterator` impl, or a specific trait? State this in the Iterator Traits section and
   cross-reference from the Loops section.


---

## F-039 ✓ RESOLVED

### `for` loop requires `Iterable` (.iter()); relationship to `IntoIterator` undefined

**Decisions:**
1. `for x in collection` requires `collection: Iterable`. A missing `Iterable` impl is a
   compile error that names the missing trait and suggests `.into_iter()` if `IntoIterator`
   is found.
2. Blanket impl: `impl[I: Iterator] Iterable for I` — every `Iterator` is also `Iterable`
   (iter() returns self). This makes `for x in col.into_iter()` work without a separate
   loop-entry trait for consuming iterators.
3. `IntoIterator`'s role is **explicit consuming iteration** — not a `for`-loop trait.
   Types that can only be iterated once implement `IntoIterator` but not `Iterable`;
   users call `.into_iter()` explicitly.

Changes: `docs/design.md` § Iterator Traits — formal blanket impl declaration, `IntoIterator`
role statement, and consuming-vs-borrowing examples. (The `for` desugaring clarification
and `Iterable` requirement were also added in F-038.)

## Finding

`docs/design.md` § Iterator Traits defines three traits:

```kara
trait Iterable       { fn iter(ref self) -> impl Iterator[...] }
trait Iterator       { fn next(mut ref self) -> Option[...] with _ }
trait IntoIterator   { fn into_iter(self) -> impl Iterator[...] }
```

And the `for` desugaring:

```kara
let _it = collection.iter()   // borrows collection
while let Some(x) = _it.next() { ... }
```

The spec says "Two built-in traits power `for` loops" (Iterable + Iterator). `IntoIterator` is
listed but excluded from the `for` desugaring. This creates three gaps.

### Gap 1 — What does `for x in collection` require?

If `for` calls `.iter()`, then the collection must implement `Iterable`. But:

```kara
struct MyStream { ... }

impl IntoIterator for MyStream {      // consuming-iterate impl
    type Item = Event
    fn into_iter(self) -> impl Iterator[Item = Event] { ... }
}

for event in MyStream.open() {       // ERROR? MyStream implements IntoIterator, not Iterable
    process(event);
}
```

`MyStream` cannot implement `Iterable` (it can only be consumed once — it has no `ref self`
`.iter()` method). Is `for x in my_stream` a compile error? If so, users must write:

```kara
for event in MyStream.open().into_iter() {   // into_iter() returns impl Iterator
    process(event);                          // but does Iterator implement Iterable?
}
```

### Gap 2 — Does `Iterator` implement `Iterable`? Is there a blanket impl?

`for x in my_vec.into_iter()` — `into_iter()` returns `impl Iterator`. For this to work with
`for`, the returned iterator must implement `Iterable` (so that `.iter()` can be called on it).

The spec doesn't say whether there is a blanket impl `impl[I: Iterator] Iterable for I` that
makes every `Iterator` also an `Iterable` (where `iter()` returns `self`). Without this blanket
impl, `for x in some_iterator` is a compile error.

In Rust, `IntoIterator` is the `for`-loop trait, and `impl<I: Iterator> IntoIterator for I`
provides the blanket impl. Kāra's three-trait design requires an equivalent.

### Gap 3 — Explicit consuming `for` loop

The spec says to call `.into_iter()` explicitly to consume a collection. But the explicit form:

```kara
for x in my_vec.into_iter() {   // does for work here?
    consume(x);
}
```

...requires `impl Iterator[...]` (the return type of `into_iter`) to be usable in a `for` loop.
This only works if either:
- `Iterator` auto-implements `Iterable` (blanket impl), OR
- `for` accepts both `Iterable` and `Iterator` directly

The spec doesn't specify either.

## Decision needed from author

1. State whether the `for` desugaring accepts `Iterable` only, `Iterator` only, or both (via
   separate grammar branches or a unified trait bound).
2. If `IntoIterator` is not a `for`-loop trait, clarify its role (explicit consuming iteration
   via method call, not `for` syntax).
3. State whether there is a blanket `impl[I: Iterator] Iterable for I` that makes every
   `Iterator` usable in a `for` loop.
4. Add an example of consuming iteration:
   ```kara
   for x in my_vec.into_iter() { ... }  // valid if Iterator: Iterable
   ```


---

## F-040 ✓ RESOLVED

### Derive dependency chain example is wrong; Serializer trait has no effect annotations

**Decisions:**
1. Example fixed to `#[derive(PartialEq, Eq, Hash, Display)]`. Derive auto-resolves
   dependency order — listing `#[derive(Hash)]` alone is also valid; the compiler
   auto-derives `PartialEq → Eq → Hash`. `#[derive(Copy)]` auto-derives `Clone`.
   Dependency chains documented explicitly.
2. `#[derive(Copy)]` without `Clone` is never a compile error — compiler fills in `Clone`.
3. All `Serializer` and `Deserializer` trait methods now carry `with _` so implementations
   may declare I/O, network, allocation effects. `Serialize::serialize` and
   `Deserialize::deserialize` also carry `with _`.
4. `#[serde(default = expr)]` accepts any compile-time constant expression (same rules as
   module-level `let` bindings). Runtime expressions are a compile error.

Changes: `docs/design.md` § Derive — fixed example, dependency-chain table, auto-derive rule;
§ Serializer/Deserializer traits — `with _` on every method; §serde attributes — `default`
expression rule.

## Finding 1 — Example derives `Eq` and `Hash` without `PartialEq`

`docs/design.md` § Derive:

```kara
#[derive(Eq, Hash, Display)]
struct Point { x: i64, y: i64 }
// Compiler generates Eq (field-by-field ==), Hash (field-by-field hash), Display (formatted string)
```

But the derivable traits list says:

> `Eq` (requires `PartialEq` — marker, adds no method body)
> `Hash` (requires `Eq` — reflects the consistency contract)

The example derives `Eq` without `PartialEq`. By the dependency rule, this should be a compile
error. Possible readings:

**Option A — The example is wrong.** Should be `#[derive(PartialEq, Eq, Hash, Display)]`.

**Option B — Deriving `Eq` auto-derives `PartialEq`.** The compiler satisfies the dependency
chain automatically: `#[derive(Eq)]` silently also generates `PartialEq`. This is a convenience
the spec hasn't stated.

**Option C — Kāra's `Eq` combines both Rust's `PartialEq` and `Eq` into a single trait.**
The "requires `PartialEq`" note means "requires the programmer to have ensured reflexivity" — not
that a separate `PartialEq` derive is needed. `Eq` IS the full equality trait in Kāra. This
would make the example correct, but the spec's statement "`Eq` requires `PartialEq`" is
misleading.

The ordering of `Eq` before `Hash` also raises a question: does the order in `#[derive(...)]`
matter? Must dependencies precede dependents in the list, or does the compiler sort them?

## Finding 2 — `Copy` and `Clone` derive rule

The spec says "`Copy` (requires all fields `Copy`; must derive `Clone` alongside)." This says
`Clone` must be in the derive list alongside `Copy`. But the spec doesn't say:

1. Is `#[derive(Copy)]` without `Clone` a compile error?
2. Does the compiler auto-derive `Clone` when `Copy` is requested?
3. What is the diagnostic if `Clone` is missing?

The Rust behavior (compile error: "the `Copy` trait requires that `Clone` is implemented") is
the likely intent, but should be stated explicitly.

## Finding 3 — Serializer and Deserializer traits have no effect annotations

```kara
trait Serializer {
    fn serialize_bool(mut ref self, v: bool)
    fn serialize_string(mut ref self, v: ref String)
    // ... all methods return () with no effect annotation
}
```

All `Serializer` and `Deserializer` methods return `()` with no effect annotation. But a
typical serializer writes to a `Write` destination (file, socket, string buffer). Any
`Serializer` that does I/O would have `writes(FileSystem)` or similar effects.

If traits have no effect annotations on their methods, then any implementation must also be pure
(no effects declared beyond what the trait allows). This means a file-writing serializer cannot
implement `Serializer` — it would violate the trait's (empty) effect ceiling.

The fix is likely to add `with _` to all `Serializer`/`Deserializer` methods (allowing
implementations to carry any effects):

```kara
trait Serializer {
    fn serialize_bool(mut ref self, v: bool) with _
    fn serialize_string(mut ref self, v: ref String) with _
    ...
}
```

But this is a significant design choice. Without `with _`, serializer implementations are
forced to buffer internally and flush via a separate method — the effects are attributed to the
flush, not the per-field serialize calls. With `with _`, the effects propagate naturally.

## Finding 4 — `#[serde(default = expr)]` — only literals shown

The `#[serde(default = 8080)]` example uses a literal. The spec doesn't say whether arbitrary
compile-time expressions are allowed, or whether `default` takes only literals. If the default
must be a constant, how is it specified for complex defaults?

```kara
#[serde(default = Vec.new())]   // valid? or only literals?
#[serde(default = i64::MAX)]     // constant reference — valid?
```

## Decision needed from author

1. Clarify the derive dependency chain: does `#[derive(Eq)]` without `PartialEq` auto-derive
   `PartialEq`, error, or is Kāra's `Eq` a combined trait? Fix the example accordingly.
2. State whether `#[derive(Copy)]` without `Clone` is a compile error or auto-derives `Clone`.
   Add the error diagnostic.
3. Add `with _` to `Serializer`/`Deserializer` methods (or explain how I/O serializers work
   with the current pure trait interface).
4. Clarify whether `#[serde(default = ...)]` accepts arbitrary constant expressions or only
   literals.


---

## F-041 ✓ RESOLVED

### Orphan rule does not address blanket `impl[T: Bound] ForeignTrait for T`

**Decision:** A blanket impl `impl[T: Bounds...] Trait for T` is permitted iff the crate
defines `Trait` (the implemented trait). Owning a bound (`MyConstraint`) is not sufficient.
Generic `T` is not a concrete type any crate owns — only trait ownership matters.

Patterns:
- `impl[T: SomeTrait] MyTrait for T` — MyTrait is yours → allowed
- `impl[T: Foreign1 + Foreign2] Foreign3 for T` — Foreign3 not yours → rejected
- `impl[T: MyConstraint] ForeignTrait for T` — ForeignTrait not yours → rejected
- Stdlib: `impl[I: Iterator] Iterable for I` — Iterable is stdlib → allowed

Change: `docs/design.md` § Orphan Rules — new "Blanket impls over generic parameters"
paragraph with the rule, all three pattern examples (allowed and rejected), and stdlib
examples.

## Finding

`docs/design.md` § Orphan Rules states:

> A crate may only write `impl Trait for Type` if it defines **either** `Trait` **or** `Type` (or both).

And: "Applies to all `impl` forms."

But a common pattern is a blanket impl where neither trait nor type "belongs" to the author:

```kara
// Crate "my-utils" wants every Iterable type to implement its trait
impl[T: Iterable] Printable for T { ... }
// my-utils defines Printable; Iterable is foreign (std); T is generic

// Is this allowed? T is not a concrete type — it's a generic parameter.
// "my-utils" doesn't define T; but nobody does (it's a variable, not a type).
```

The rule as written ("define either Trait or Type") handles the two-concrete-type case. It
does not address blanket impls over generic parameters.

## The three patterns that need clarification

**Pattern 1 — Own the trait, generic T:** `impl[T: SomeTrait] MyTrait for T`
- `MyTrait` is yours, `T` is generic. Should be allowed: you own the trait.
- Rust allows this.

**Pattern 2 — Own nothing, constrain both:** `impl[T: ForeignTrait1 + ForeignTrait2] ForeignTrait3 for T`
- Nothing is yours. This is an orphan and should be rejected.
- But the spec's current rule ("own either Trait or Type") doesn't cover the `T` case.

**Pattern 3 — Own the constraint, not the trait or base:**
`impl[T: MyConstraint] ForeignTrait for T`
- `MyConstraint` is yours, but neither `ForeignTrait` nor `T` is.
- Rust disallows this (you must own the trait or the type, not just a bound).
- Kāra's rule doesn't address it.

## Stdlib implications

The standard library requires blanket impls for ergonomics:
```kara
// Into is the blanket impl of From
impl[T, U: From[T]] Into[T] for U { ... }   // both Into and U are stdlib — allowed
impl[I: Iterator] Iterable for I { ... }    // both in stdlib — allowed
```

User code that tries analogous blanket impls (owning the trait) should also be allowed:
```kara
// user-defined serialization adapter for all Displayable types
impl[T: Display] MySerializer.Encodable for T { ... }  // MySerializer.Encodable is yours
```

## Decision needed from author

State explicitly: "The orphan rule for blanket impls is: a blanket `impl[T: Bounds...] Trait for T`
is permitted iff the crate defines `Trait` (or at least one of the listed bounds). It is
rejected if the crate defines none of them." Add an example showing allowed and rejected blanket
impls.


---

## F-042 ✓ RESOLVED

### `as` keyword has dual semantics: numeric cast vs refinement assertion

**Decision:** `x as T` uses single-dispatch disambiguation based on `T`:
- `T` is a primitive numeric type → numeric cast (bitwise, no check, no effects)
- `T` is a refinement type → refinement assertion; **source must match the base type exactly** (compile error otherwise)
- For cross-type + refinement, two explicit casts are required: `(x as i32) as Special`

This keeps `as` unambiguous — one semantic per use. Adversarial case `i64 as Special` (where
`Special = i32 where ...`) is a compile error; the programmer must narrow numerically first.

`docs/design.md` § Refinement Types: added "`as` disambiguation rule" paragraph with rule table and examples.
`docs/syntax.md` §5.19 Cast Expressions: rewrote with three-way disambiguation table (numeric / refinement assertion / raw pointer).

## Finding

`docs/design.md` § Refinement Types defines two uses of `as`:

**Use 1 — Numeric cast (existing, pre-refinement):**
```kara
let n: i32 = x as i32;   // narrowing/widening cast, no runtime check, may silently truncate
```

**Use 2 — Refinement assertion (new in this section):**
```kara
let n: NonZero = x as NonZero;   // runtime check inserted, panics on failure, propagates `panics`
```

The spec shows both forms using the `as` keyword but describes them as having fundamentally
different semantics:

| | Numeric cast | Refinement assertion |
|---|---|---|
| Runtime check | No (bitwise conversion) | Yes (predicate check) |
| On failure | Silent truncation or wrap | Panic (propagates `panics` effect) |
| Effect | None | `panics` |

## The disambiguation problem

The compiler must decide which semantics apply when it sees `x as T`. The rule is presumably:
- If `T` is a numeric primitive type (`i8`..`u128`, `f32`, `f64`): numeric cast
- If `T` is a refinement type: assertion

But the spec never states this disambiguation rule. Adversarial cases:

```kara
type Special = i32 where self > 0

let x: i64 = 100;
let y = x as Special;   // What happens here?
// Option A: numeric cast from i64 to i32 (and THEN refinement assert?) — two steps?
// Option B: compile error — can only cast to base type, not refined type, with numeric cast
// Option C: refinement assert, no numeric conversion
```

`x: i64` being cast to `Special = i32 where self > 0` — this involves both a numeric narrowing
(`i64 → i32`) AND a refinement assertion (`self > 0`). Do both happen? In which order? What
effects does this carry (`panics` only for the assertion, or also for the narrowing)?

Another case:
```kara
type Percent = i32 where self >= 0 && self <= 100
let n: f64 = 50.5;
let p = n as Percent;   // f64 → i32 narrowing + predicate check? two casts?
```

## Decision needed from author

1. Define the disambiguation rule: when is `x as T` a numeric cast and when is it a refinement
   assertion?
2. If `T` is a refinement type, does `as` also perform a numeric conversion if the base type
   differs from `x`'s type? Or is the source type required to match the base type?
3. If both conversions are needed, state the ordering (numeric conversion first, then predicate
   check) and the combined effect signature.
4. Add an explicit section documenting the `as` keyword's full semantics (numeric cast vs
   refinement assertion vs cross-refinement).


---

## F-043 ✓ RESOLVED

### Refinement constraint language grammar not fully specified

**Decision:** The constraint language is any *pure expression* over `self` and compile-time
constants — not a special-purpose whitelist. `REFINEMENT_PRED = PURE_EXPR` where `PURE_EXPR`
covers arithmetic/bitwise/comparison operators, `&&`/`||`/`!`, struct field access (`self.lo`),
zero-argument method calls on `self` with no effect annotations (`self.len()`, `self.is_empty()`,
`self.is_ascii()`, `self.is_sorted()`, etc.), and literals/module-level consts. Methods with
arguments or functions not on `self` are disallowed.

The constraint language and static elision are separate: any pure expression is *expressible*;
whether the runtime check is omitted is governed only by the two elision rules (const-eval and
type-identity). `self.is_ascii()` is a *valid* constraint — the spec's prior example was
misleading; it is not a counter-example.

`docs/design.md` § Refinement Types: replaced vague "Allowed constraints" prose with formal
`REFINEMENT_PRED` / `PURE_EXPR` grammar and explicit allow/deny lists.
`docs/syntax.md` §3.12: replaced `REFINE_EXPR = EXPR` with the full `REFINEMENT_PRED` / `PURE_EXPR`
production; updated `DISTINCT_TYPE` to match.

## Finding

`docs/design.md` § Refinement Types says:

> **Allowed constraints:** Numeric comparisons (`>`, `<`, `>=`, `<=`, `==`, `!=`) and
> `self.len()` on collections. Boolean operators (`&&`, `||`, `!`) to combine predicates.
> Arbitrary method calls (e.g., `self.is_ascii()`) are **not** allowed.

This is a description, not a grammar. The boundary between allowed and disallowed predicates
is defined only by examples:
- `self != 0` — allowed (numeric comparison)
- `self >= 0.0 && self <= 100.0` — allowed (numeric + boolean)
- `self.len() > 0` — allowed (special-cased collection method)
- `self.is_ascii()` — NOT allowed (arbitrary method call)

Several questions remain:

### What exactly is allowed?

**Is `self.len()` the only allowed method call, or are there others?**

```kara
type NonEmpty[T] = Vec[T] where self.len() > 0    // shown as valid
type Ordered[T] = Vec[T] where self.is_sorted()   // is_sorted allowed?
type AsciiStr = String where self.is_ascii()      // is_ascii — spec says NOT allowed
type ValidUtf8 = String where self.is_empty()     // is_empty — allowed?
```

**Are chained method calls allowed?**

```kara
type Valid = Vec[i32] where self.len() > 0 && self.len() <= 100
// two uses of self.len() — allowed?
```

**Can you compare self against other constants?**

```kara
type SmallVec[T] = Vec[T] where self.len() < MAX_SIZE   // MAX_SIZE is a const — allowed?
type InRange = i32 where self > MIN && self < MAX        // const references — allowed?
```

**Is arithmetic on self allowed?**

```kara
type EvenNumber = i32 where self % 2 == 0   // modulo in constraint — allowed?
type PowerOfTwo = i32 where self & (self - 1) == 0   // bitwise in constraint — allowed?
```

**Can the constraint reference field names for structs?**

```kara
type ValidRange = Struct { lo: i32, hi: i32 } where self.lo < self.hi
// field access in constraint — allowed?
```

### Why `self.len()` is special but `self.is_ascii()` is not

The spec says the constraint language avoids "predicates the compiler can evaluate without
executing user code." But `self.len()` IS user code (well, stdlib code). The real distinction
is probably: `self.len()` is a pure, no-allocation, no-effect, always-terminates method on
built-in collection types that the compiler can reason about. `self.is_ascii()` is a more
complex predicate that may traverse the string.

But this distinction is not formally stated. The spec's reasoning ("no SMT solver, no
interval arithmetic") applies to the ELISION — whether the compiler can prove a constraint
without a runtime check. The LANGUAGE of constraints is separate from whether they can be
elided.

## Decision needed from author

1. Define a formal grammar for the refinement constraint language:
   ```
   REFINEMENT_PRED = EXPR COMPARISON_OP EXPR
                   | EXPR "." "len()" COMPARISON_OP INT_LITERAL
                   | REFINEMENT_PRED ("&&" | "||") REFINEMENT_PRED
                   | "!" REFINEMENT_PRED
                   | "(" REFINEMENT_PRED ")"
   ```
   ...or whatever the actual grammar is.

2. State explicitly whether:
   - Const references (module-level `let` or compile-time constants) are allowed on the right-hand side
   - `self % n == 0` style arithmetic is allowed in constraints
   - Method calls beyond `self.len()` are allowed, and if so, which ones (whitelist vs rule)
   - Field access (`self.field > 0`) on struct refinements is allowed

3. Clarify the distinction between "allowed in the constraint grammar" and "can be elided at
   compile time" — these are separate questions, and the spec conflates them.


---

## F-044 ✓ RESOLVED

### `distinct type T = Base where predicate` construction semantics undefined

**Decision:**
1. **`T(value)` always checks the predicate** — const-eval literal → compile-time check; runtime
   value → runtime assertion propagating `panics`. No "raw wrap without checking" path.
2. **`T.try_from(value)` is auto-generated**, returning `Result[T, RefinementError]` — the
   recoverable path.
3. **Two elision rules apply unchanged** at binding sites (`let p: ValidPort = 80` uses rule 1).
   `let p: ValidPort = n` where `n: u16` is a compile error; use constructor or `try_from`.
4. **`.raw()` returns `Base`** (the raw base type), stripping both the `distinct` wrapper and
   the predicate.

`docs/design.md` § Distinct Types: added "Construction semantics for `distinct type T = Base
where predicate`" paragraph with four numbered rules and a code example.

## Finding

`docs/design.md` § Distinct Types shows the combined form:

```kara
distinct type ValidPort = u16 where self >= 1 && self <= 65535
```

But the construction semantics are never specified. The section defines construction for plain distinct types as:

> **Wrap:** `UserId(42)` — constructor syntax

For refined-but-not-distinct types, the construction is:

> **Construction:** Dynamic values enter refined types via `try_from`, which returns `Result`

For the combined form, neither rule is stated. The questions are:

### 1 — Does `ValidPort(80)` check the predicate?

```kara
let p = ValidPort(80);    // constructor syntax — does it check 1 <= 80 <= 65535?
let q = ValidPort(70000); // out of range — compile error? runtime panic? Result?
```

Three plausible behaviors:
- **A**: `ValidPort(x)` always applies the predicate. If `x` is a literal, compile-time check (compile error on failure). If runtime, panics on failure (propagates `panics`). No `Result` — constructor is "asserting."
- **B**: `ValidPort(x)` never checks the predicate — the constructor is a raw wrap, just like plain distinct types. Users must use `ValidPort.try_from(x)?` explicitly for the check.
- **C**: `ValidPort(x)` is a type error — distinct+refinement types require explicit `try_from` for ALL construction.

### 2 — Does `ValidPort.try_from(x)` still work?

For plain refinement types, `TryFrom` is auto-generated by the compiler. For distinct+refinement
types, is `TryFrom` also auto-generated? If so, what is the `TryFrom[u16]` → `ValidPort` impl?

### 3 — Interaction with the two elision rules

The refinement type section defines two elision rules for narrowing:
1. Const-evaluable initializer → compile-time check
2. Type-identity narrowing → no check

For a combined `distinct type`, rule 1 applies to `let p: ValidPort = 80` (literal). But rule 2
("type is exactly the target refined type") must now account for the `distinct` wrapper — is a
`ValidPort` exactly the same as a `ValidPort` (yes, trivially), but is a `u16` exactly the same
as a `ValidPort`? No — there's both the `distinct` wrapper and the predicate.

So `let p: ValidPort = 80;` (without explicit constructor) — what happens? Does this use the
constructor syntax, or does the binding initialization use the refinement rules?

### 4 — `.raw()` on a combined type

For plain distinct types, `.raw()` unwraps to the base type. For `distinct type ValidPort = u16
where self >= 1 && self <= 65535`, does `.raw()` return `u16` (stripping both the distinct
wrapper and the predicate), or does it return `ValidPort` stripped of the `distinct`-ness but
retaining the predicate?

## Decision needed from author

State explicitly for `distinct type T = Base where predicate`:
1. Whether `T(value)` constructor syntax checks the predicate (and if so, what happens on failure).
2. Whether `T.try_from(value)` is still available and auto-generated.
3. Whether the two elision rules from refinement types apply at `let x: T = literal` binding sites.
4. What `.raw()` returns (base type `Base`, or `Base where predicate`).


---

## F-045 ✓ RESOLVED

### `assert` undefined; property test shrinking unspecified; snapshot file location missing

**Decision:**
1. **Assert family** — all four are test-prelude compiler builtins (not available in production code):
   - `assert(cond: bool) with panics` — captures source expression text in failure message
   - `assert_eq[T: Eq + Debug](a: T, b: T) with panics` — "expected X, got Y" message
   - `assert_ne[T: Eq + Debug](a: T, b: T) with panics` — "expected not-equal" message
   - `assert_snapshot[T: Display](expr: ref T) with panics` — diff vs saved snapshot
   `assert_ne` added to the global compiler-builtins list; `assert_snapshot` / `Arbitrary` / `Shrink` injected into `_test.kara` files only.

2. **Shrinking** — `Arbitrary` and `Shrink` are separate traits; `#[derive(Arbitrary)]` auto-derives both. Manual `Arbitrary` impls must provide `Shrink` (or use `NoShrink`). Default strategy: integers toward zero, collections remove tail elements, structs shrink fields independently. `Shrink` is test-only.

3. **Snapshot location** — `tests/snapshots/<module_path>/<test_name>.snap` relative to crate root; plain text (`Display` output); keyed on fully qualified test function name; renaming creates a new path (orphan `.snap` flagged as stale). Files are committed to source control.

`docs/design.md` § Testing: added "Test assertion functions" table; added `Shrink` trait definition; added "Snapshot file location and identity" paragraph. Compiler-builtins table and prelude list updated with `assert_ne`.

## Finding 1 — `assert` is used but never defined

Tests throughout `docs/design.md` use `assert(condition)` and `assert_snapshot(expr)`, but
neither is defined anywhere in the spec:

```kara
fn test_add() {
    assert(add(1, 2) == 3);    // assert defined where?
}
```

Missing from the spec:
1. Is `assert` a built-in, a stdlib function, or a test-only prelude item?
2. What is its effect signature? Presumably `panics` (it aborts the test on failure).
3. What message does it produce on failure? Just "assertion failed"? Does it capture the
   expression text for a better message (like Rust's `assert_eq!`)?
4. Is `assert_eq(a, b)` also provided (with a better "expected X, got Y" message)? Not shown.
5. Is `assert` available in production code, or only in test files?
6. Is `assert_snapshot(expr)` a stdlib function? What trait must `expr` implement (`Display`?
   `Debug`? `Serialize`)?

## Finding 2 — Property test shrinking unspecified

The spec says property tests "shrink the input to the minimal failing case" but never defines:

1. Is there a `Shrink` trait that custom types must implement?
2. Does `#[derive(Arbitrary)]` also generate a shrinking implementation?
3. If a custom type implements `Arbitrary` manually, does it also need to implement `Shrink`
   manually?
4. What is the default shrinking strategy for derived types (remove elements, halve values,
   step toward zero)?

Without shrinking infrastructure defined, the property test framework is incompletely specified.

## Finding 3 — Snapshot file location and format

The spec says: "First run saves the output to a file. Subsequent runs compare against the
saved snapshot." But:
1. Where is the file stored? `tests/snapshots/`? Same directory as the test file?
2. What is the file format? Plain text? JSON? The test name as the filename?
3. How is the snapshot identified if the test function moves or is renamed?
4. What type does `assert_snapshot` accept? Must it implement `Display`? `Debug`? `Serialize`?

## Decision needed from author

1. Add `assert`, `assert_eq`, `assert_ne`, and `assert_snapshot` to the standard library surface
   or test prelude, with their signatures, effects, and failure messages.
2. Define the `Shrink` trait (or state that `Arbitrary` includes shrinking behavior).
3. Specify snapshot file location convention (e.g., `tests/snapshots/<module>/<test_name>.snap`).


---

## F-046 ✓ RESOLVED

### `Display.to_string(self)` takes owned receiver; f-string `{expr}` would consume non-Copy values

**Decision: Reading A — spec typo.** `Display.to_string` takes `ref self` (borrows), matching
`Debug.fmt_debug(ref self)` and making f-strings non-destructive. The owned-receiver form would
make every `{name}` in an f-string consume `name`, which is clearly wrong for non-Copy types.

`docs/design.md` § String Interpolation: updated `to_string(self)` → `to_string(ref self)` and
added an explicit note that the borrow allows the same value to appear in multiple `{}` slots.

## Finding

`docs/design.md` § String Interpolation defines:

> `Display` provides a `to_string(self) -> String` method.

And the f-string desugaring:

> The compiler desugars `{expr}` to `expr.to_string()` (method call via UFCS).

If `to_string` takes `self` by value (owned), then `{expr}` in an f-string *consumes* `expr`.
This is a problem for non-Copy types:

```kara
let name = "Alice".to_string();      // name: String (not Copy)
let greeting = f"Hello, {name}!";    // {name} → name.to_string() → consumes name
println(name);                        // COMPILE ERROR: name was moved into f-string
```

And using the same value twice:
```kara
let msg = f"{name} said: {name}";    // ERROR: name moved at first {name}, not available at second
```

In contrast, `Debug` is defined as:

```kara
trait Debug {
    fn fmt_debug(ref self) -> String
}
```

`Debug.fmt_debug` takes `ref self` (borrows). The asymmetry between `Display.to_string(self)`
and `Debug.fmt_debug(ref self)` is suspicious.

## The two plausible readings

**Reading A — `to_string` should take `ref self`.** The spec has a typo; `Display` should be:
```kara
trait Display {
    fn to_string(ref self) -> String
}
```
This matches `Debug`, matches idiomatic use (you can use the same value in multiple format
slots), and matches Rust's `Display` which borrows the value.

**Reading B — `to_string` takes `self` and the f-string desugaring auto-refs.** The f-string
desugaring calls `(&expr).to_string()` (an autoref), making the call a ref-self invocation even
though the trait declares `self`. The trait itself is conceptually "take ownership or borrow
— implementors choose." But Kāra's method dispatch does perform auto-ref (Step 2 of the method
resolution algorithm). So `name.to_string()` when `Display` declares `to_string(self)` might
auto-ref: the resolver finds `to_string` on `ref String` (via auto-ref), not on `String`
directly. This is confusing but technically workable.

**Reading C — `to_string(self)` is intentional for Copy types, and non-Copy types must use `ref self` explicitly in their impl.** But this would mean two versions of `to_string` (owned and ref), which conflicts with a single trait.

## Decision needed from author

1. State whether `Display.to_string` takes `self` (owned) or `ref self` (borrow).
2. If `self` (owned): clarify that f-string desugaring auto-refs the expression so non-Copy types are not consumed.
3. If `ref self`: fix the trait definition in the spec.
4. Either way, align `Debug.fmt_debug(ref self)` and `Display.to_string(? self)` to the same convention.


---

## F-047 ✓ RESOLVED

### Generic specialization without a call — `sort[i64]` — has no grammar production

**Decision: Option 2** — remove `sort[i64]` from the spec; use a typed closure instead.
`x.sort()` covers the common case (receiver type pins `T`). For higher-order passing without
a typed binding, write `|v: Vec[i64]| sort(v)` — equally readable, no parser complexity.
No `GENERIC_SPEC_EXPR` production is added.

`docs/design.md` § First-Class Functions: removed `sort[i64]` example; replaced with typed
closure form; added note that method-call syntax (`.sort()`) is the idiomatic path.

**Section:** First-Class Functions (line 2901)
**Tags:** syntax-gap

## Finding

The spec shows:

```kara
let g = sort[i64];   // explicit specialization when inference can't resolve
```

The current call-expression grammar is:

```
CALL_EXPR = EXPR [ "[" TYPE_LIST "]" ] "(" [ ARG_LIST ] ")"
```

The `"(" [ ARG_LIST ] ")"` suffix is required — not optional. `sort[i64]` without `()` therefore
does not parse as any current expression form. There is no `GENERIC_SPEC_EXPR` production that
yields a monomorphised function *value* without simultaneously calling it.

This is a real gap: the feature is promised, but the grammar doesn't support it.

## Decision needed from author

Choose one:

1. Add a new production: `GENERIC_SPEC_EXPR = EXPR "[" TYPE_LIST "]"` (no call suffix). Distinguish
   from index expressions (`list[0]`) by context: if all items inside `[...]` are types, it's a
   specialization; if any is an expression, it's an index. Spell out the disambiguation rule.
2. Keep the grammar as-is and require a wrapper closure when inference fails. Simpler grammar,
   slightly more verbose call sites. Remove the `sort[i64]` example or add a note that it is
   aspirational syntax.


---

## F-048 ✓ RESOLVED

### Effect clause in function/closure *type* expressions has no grammar production

**Decision:** Already resolved in `docs/syntax.md`. `FUNCTION_TYPE` at §6.3 is:
```
FUNCTION_TYPE = "Fn" "(" [ TYPE_LIST ] ")" [ "->" TYPE ] [ "with" EFFECT_SPEC ]
```
`EFFECT_SPEC` (defined at §7.1) covers `_`, space-separated effect terms, and named groups.
`FUNCTION_TYPE` is a full `TYPE` variant — valid in all type positions including generics,
struct fields, and `let` bindings. No further action needed.

**Section:** First-Class Functions (line 2883), Closures (line 2910)
**Tags:** syntax-gap

## Finding

The spec routinely uses effect-annotated function type expressions:

```kara
let f: Fn(User) -> () with writes(UserDB) = save;
let f: Fn(User) -> () with reads(UserDB) writes(AuditLog) = |u| { ... }
fn process(logger: Fn(String) -> () with _ = |_| ())
```

No grammar production for `FN_TYPE` is defined in the spec or `docs/syntax.md`. The effect
system section defines the `with` clause for *function declarations* but not for *type
expressions* that appear in `let` bindings, parameter types, or struct fields.

Unanswered questions:

1. Is `Fn(T) -> U with E1 E2` a single type with two effects, or a parse error?
2. Can the effect clause appear in any type position (e.g., `Vec[Fn(T) -> U with E]`)?
3. What is the exact grammar token for an effect in a type?

## Decision needed from author

Add a `FN_TYPE` grammar production to `docs/syntax.md`:

```
FN_TYPE = "Fn" "(" [ PARAM_TYPE_LIST ] ")" "->" TYPE [ "with" EFFECT_LIST ]
EFFECT_LIST = EFFECT+
EFFECT = VERB "(" RESOURCE ")" | VERB | "_"
```

Confirm whether `FN_TYPE` is valid in all type positions or only at declaration level.


---

## F-049 ✓ RESOLVED

### `String` implements `Add` with `rhs: ref String` but the trait requires `rhs: Self`

**Decision: Option 1** — formalized as a general conformance rule: non-receiver parameter modes
(`ref`, `mut ref`) are not part of trait conformance — only the underlying type is checked.
The receiver mode (`self` / `ref self` / `mut ref self`) still must match exactly. Auto-ref
at call sites handles the mismatch transparently. `String.add(self, rhs: ref String)` is
conforming because `ref String` has the same underlying type as `Self = String`.

`docs/design.md` § Operator Traits: added "Trait conformance and parameter modes" paragraph
after the canonical trait definitions. Removed "deviates" qualifier from the `String` Add
impl description, replaced with a back-reference to the conformance rule.

**Section:** Operator Traits — Stdlib impls (line 3307)
**Tags:** contradiction

## Finding

The canonical `Add` trait definition:

```kara
trait Add { fn add(self, rhs: Self) -> Self }
```

The spec then states for `String`:

> `Add` on `String` — `fn add(self, rhs: ref String) -> String`. **This deviates from the
> homogeneous `fn add(self, rhs: Self) -> Self` signature shown in the trait definition.**

A trait impl must match the trait's declared method signature. An impl that changes a parameter
from `Self` to `ref Self` is not conforming. The spec acknowledges the deviation but does not
explain how it is mechanically possible.

Three interpretations, each with problems:

1. Mode annotations (`ref`, `mut ref`) are part of the signature — the impl is non-conforming.
2. `String.add` is a built-in, not a trait impl — breaks "every operator is trait-dispatched."
3. The trait has an associated `Rhs` type — but the spec explicitly rejects that for v1.

## Decision needed from author

Choose one:

1. Change the `Add` trait to allow `rhs: ref Self` as an alternative and document the rule.
2. Change `String + &String` to `String + String` (both owned) — accept the copy cost.
3. Introduce a `Rhs` type parameter on `Add` with `type Rhs = Self` as the default, allowing
   `impl Add for String { type Rhs = ref String; ... }`.


---

## F-050 ✓ RESOLVED

### `IndexMut` for range indexing: spec says returns `mut Slice[T]` but trait returns `mut ref Self.Output`

**Decision:** Prose was wrong. With `type Output = Slice[T]` the trait signature yields
`mut ref Slice[T]` — three prose occurrences updated to match. The assignment desugar
`collection[a..b] = other` → `*IndexMut.index_mut(mut ref collection, a..b) = other`;
dereferencing `mut ref Slice[T]` and assigning `Slice[T]` performs element-wise copy into
the backing buffer (panics if lengths differ).

`docs/design.md`: updated "Range indexing" paragraph (§ Slices) and "Slice indexing" paragraph
(§ Subscript Trait) to say `mut ref Slice[T]` consistently and to document the element-copy
semantics of the assignment desugar.

**Section:** Subscript Trait (line 3204)
**Tags:** contradiction

## Finding

The `IndexMut` trait:

```kara
trait IndexMut[Idx] {
    type Output
    fn index_mut(mut ref self, idx: Idx) -> mut ref Self.Output
}
```

The spec says:

> Mutable range indexing (`impl IndexMut[Range[i64]]`) returns `mut Slice[T]`.

With `type Output = Slice[T]`, the trait signature yields `mut ref Slice[T]` — not `mut Slice[T]`.
`mut ref Slice[T]` (a mutable reference to a slice) and `mut Slice[T]` (an owned mutable slice)
are different types, requiring different desugaring for `collection[a..b] = other`.

The prose uses "returns `mut Slice[T]`" in multiple places, consistently omitting `ref`.

## Decision needed from author

Confirm that range `IndexMut` has `type Output = Slice[T]` → returns `mut ref Slice[T]`,
and update all prose references from "returns `mut Slice[T]`" to "returns `mut ref Slice[T]`"
(three occurrences in Slices + Subscript sections). Or explain why range indexing returns an
owned slice and update the trait signature accordingly.


---

