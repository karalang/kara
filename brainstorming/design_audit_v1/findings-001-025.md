# Design Audit Findings: F-001 – F-025

Each finding has an anchor matching its number (e.g. `#f-001`).

---

## F-001

### Layout block constraints misclassified as Implementation Freedom

## Finding

`docs/design.md` Specification Layers § Implementation Freedom lists:

> Layout transforms selected under a `layout` block (SoA vs AoS, field grouping, padding). The
> programmer opts in to layout control when they write a layout block; **the compiler must respect
> the block's grouping and ordering constraints**. Within those constraints (e.g., padding,
> alignment within a group), the compiler retains optimization freedom.

The phrase "the compiler must respect" is a guarantee, not a freedom. Mandatory behavior belongs
under Guaranteed semantics. The bullet conflates two distinct things:

- **(a)** Whether the compiler respects grouping/ordering constraints at all → **Guaranteed semantics**
- **(b)** What the compiler does within those constraints (padding, alignment) → **Implementation freedom**

As written, a conforming compiler could technically ignore layout block constraints entirely and
still claim compliance, since the bullet sits under Implementation Freedom.

## Options

**Option A — Split the bullet.**
Move the constraint-compliance half to Guaranteed semantics:
> Guaranteed: A `layout` block's grouping and field-ordering constraints are always honored.

Keep under Implementation Freedom:
> Within those constraints, the compiler retains freedom over padding, alignment, and
> sub-group layout choices.

**Option B — Reword the Implementation Freedom bullet to be unambiguous.**
Keep it in one place but reword to make clear the freedom only applies *within* the constraints,
not to the constraints themselves. Add an explicit note: "Non-compliance with grouping/ordering
constraints is a compiler bug, not a freedom."

**Option C — Leave as-is with an editorial note.**
Accept that "must respect" within an Implementation Freedom bullet is informal shorthand and
tooling / conformance tests will catch violations. Lower formal correctness for lower churn.

## Recommendation

Option A is cleanest — the Guaranteed semantics list is the load-bearing place for compiler
conformance, and adding "layout block constraints are honored" there is a clear, unambiguous
commitment. Option B risks the same misread if the wording is still under Implementation Freedom.

## Decision

**Option A.** Split the bullet. Added to Guaranteed semantics: "`layout` block constraints are honored — grouping and field-ordering constraints are always respected by a conforming compiler; non-compliance is a compiler bug." Implementation Freedom bullet rewritten to cover only within-constraint freedom (padding, alignment, sub-group arrangement), with a cross-reference back to the Guaranteed bullet. `spec-updated`


---

## F-003

### "Observable ordering within a single effect resource" is underspecified

## Finding

`docs/design.md` Seed Classification § Guaranteed semantics states:

> Observable ordering within a single effect resource. Two writes to the same resource in source
> order are observed in source order. (Cross-resource ordering is not guaranteed — see
> Implementation freedom.)

Three sub-questions the spec leaves unanswered:

### 1. Per resource instance or per resource type?

If I have two distinct `Log` resource instances — say `access_log` and `error_log` — are writes to
each ordered independently (per instance), or are they globally ordered across all instances of
type `Log` (per type)?

```kara
// Are these two sequences independently ordered, or globally ordered?
fn process(access_log: mut ref Log, error_log: mut ref Log) with writes(Log) {
    access_log.write("req start")
    error_log.write("processing")
    access_log.write("req end")
}
```

### 2. Are reads covered, or only writes?

The spec says "two writes... are observed in source order." What about a read after a write to the
same resource? Is this ordered?

```kara
fn check(db: mut ref DB) with reads(DB), writes(DB) {
    db.write(record)
    let v = db.read(key)   // guaranteed to see the write above?
}
```

### 3. What does "observed" mean?

At the language/program level? At the hardware level? For ordinary resources this distinction
doesn't matter, but for MMIO (Volatile Memory Access, covered later in the spec) it matters
enormously — writes that appear ordered at the language level may be reordered by the CPU or
compiler unless a memory barrier is inserted.

## Options

**On scope (per instance vs per type):**
- Per instance is the natural choice (a resource instance is the unit of effect analysis). State
  it explicitly.

**On reads:**
- Write-then-read to the same resource is almost certainly intended to be ordered (otherwise the
  read might not see the write). Extend the guarantee to cover reads.
- Alternative: state "reads observe all prior writes in source order" as a separate bullet.

**On "observed":**
- Add a note: "ordering is at the language/operational level; the compiler must not reorder
  effects on a single resource instance across sequence points, but hardware memory barriers are
  the responsibility of the runtime/backend for MMIO."

## Decision

**Closed — spec updated.** All three sub-questions resolved:

1. **Scope: per instance.** "Same resource" means the same resource instance — two distinct
   `Log` instances are independently ordered, not globally ordered across all instances of
   type `Log`. The resource instance is the natural unit of effect analysis, and the spec text
   already implied this with "same resource."

2. **Reads covered.** The guarantee extends to any two accesses (reads or writes), not writes
   only. A read after a write to the same instance must observe that write; leaving reads
   unordered would make the guarantee useless for any read-after-write pattern.

3. **Language/operational level.** The compiler must not reorder accesses to a single resource
   instance across sequence points. Hardware memory-barrier insertion (for MMIO and similar
   special-purpose hardware resources) is the backend's responsibility and is addressed
   separately in the Volatile Memory Access section; programs using those resources already
   operate under the `volatile` qualifier rules that handle physical ordering.

`docs/design.md` line 119 updated accordingly. `spec-updated`


---

## F-004

### RC flavor change (Rc → Arc) is classified as Reported Behavior but has semantic implications

## Finding

`docs/design.md` Seed Classification § Reported behavior states:

> A later compiler version is free to pick differently (e.g. switching `Rc` → `Arc` when it proves
> a value crosses a thread boundary), so `representation` is reported behavior, not guaranteed.

`Rc` and `Arc` are not semantically interchangeable:

- `Rc<T>` does not implement `Send` or `Sync`. A value cannot cross a thread boundary.
- `Arc<T>` implements `Send` (when `T: Send`) and `Sync` (when `T: Sync`).

This means a compiler switch from `Rc` to `Arc` is a **semantic change**, not just a reporting
change:

```kara
shared struct Config { ... }

fn main() with sends(Thread) {
    let cfg = Config.new()
    // With Rc backing: compile error — Config is not Send
    // With Arc backing: compiles — Config is Send
    spawn(|| use_config(cfg))
}
```

If the compiler switches `Rc` → `Arc` across versions, programs that previously failed to compile
now compile (or vice versa: Arc → Rc breaks programs that were sending shared values). Either
direction is a breaking change in program meaning, which is the definition of Guaranteed semantics
territory, not Reported behavior.

The spec's example ("switching Rc → Arc when it proves a value crosses a thread boundary") is
actually a case where the compiler is *responding to a semantic requirement* — it must use Arc
because the program sends the value. That's not an arbitrary flavor choice; it's a necessity.

## Options

**Option A — Guarantee thread-safety-driven RC flavor selection.**
Add to Guaranteed semantics: "A `shared` value that crosses a thread boundary is always backed by
a thread-safe RC flavor (equivalent to `Arc`). A `shared` value that never crosses a thread
boundary may be backed by a non-thread-safe flavor (`Rc`)." The compiler's choice is then
deterministic from the program's thread-crossing behavior, making the flavor observable in
the guaranteed sense.

**Option B — Restrict the freedom to flavor-neutral changes.**
Clarify that "free to pick differently" only applies to choices that are semantically invisible
(e.g., which specific `Arc`-like implementation, not Rc vs Arc). A switch that changes the Send
behavior of a type is not a free choice.

**Option C — Expose thread-safety as a declared property of `shared struct`.**
Let the programmer annotate `shared struct Foo: Send` or `shared struct Foo` (not Send). The
compiler enforces the annotation; the RC flavor is then a consequence, not a free choice.

## Decision

**Closed — Option A, spec updated.** The RC flavor selection rule is now Guaranteed semantics:
a value proven to cross a thread boundary is always `Arc`; one proven not to cross may be `Rc`;
cannot-prove defaults to `Arc` for RC-fallback values (and is a compile error for `shared struct`).
The rule is stable across compiler versions; what may change is which specific values the
thread-crossing analysis proves — promotions from `Rc` to `Arc` are never breaking, since `Arc:
Send` is strictly wider than `Rc: !Send`. The Reported behavior bullet is narrowed to describe
the specific flavor chosen for a given binding, not arbitrary compiler freedom.

`docs/design.md` Guaranteed semantics and Reported behavior sections updated. `spec-updated`


---

## F-005

### Broadening inferred private effects can silently break public function verification on compiler upgrade

## Finding

`docs/design.md` Seed Classification § Reported behavior states:

> The inferred effect set of a *private* function... the compiler is free to **tighten or broaden**
> the inferred set as the inference algorithm improves.

And § Guaranteed semantics states:

> The effect set of a **public** function is part of its signature.

These two interact badly. Consider:

```kara
// Library v1, compiler v1: priv_helper inferred as writes(Log)
fn priv_helper(log: mut ref Log) { log.write("x") }

pub fn process(log: mut ref Log) with writes(Log) {
    priv_helper(log)   // declared writes(Log) covers inferred writes(Log) ✓
}
```

Now the library author upgrades to compiler v2, which "broadens" the inferred effect set of
`priv_helper` to `writes(Log), reads(DB)` (hypothetically, due to a changed inference algorithm).

```kara
// Same source, compiler v2: priv_helper now inferred as writes(Log), reads(DB)
pub fn process(log: mut ref Log) with writes(Log) {
    priv_helper(log)   // declared writes(Log) does NOT cover reads(DB) → compile error
}
```

The library's source has not changed. The library's public API has not changed. But upgrading the
compiler breaks the build. This violates the spirit of the guaranteed semantics for public
functions — the author cannot maintain a stable public API without also controlling compiler
versions.

## The "broadening" case is the problem

Tightening (inferring *fewer* effects) is safe — if the compiler decides `priv_helper` only does
`writes(Log)` instead of `writes(Log), reads(DB)`, public callers' declarations remain valid.

Broadening (inferring *more* effects) is the dangerous direction. It can break public function
verification without any source change.

## Options

**Option A — Disallow broadening; only allow tightening.**
Change "tighten or broaden" to "tighten (or leave unchanged)." The inference algorithm may only
become more precise over time (fewer inferred effects), never less precise (more inferred effects).
This is the conservative, safe direction.

**Option B — Separate the freedom claim by direction.**
State explicitly: "The compiler is free to infer a *subset* of the effects it previously inferred
(tightening). A compiler version must not infer *additional* effects not inferred by any prior
conforming version for the same source (no broadening)." This makes the monotonicity requirement
formal.

**Option C — Require explicit effect declarations on any private function called by a public function.**
If a private function is in the "effect cone" of a public function, it must have an explicit
effect declaration (which is guaranteed). The inference freedom only applies to private functions
not transitively called by public functions. This is strict but removes the loophole entirely.

**Option D — Treat broadening as a deprecation-worthy compiler behavior.**
Keep the freedom to broaden but require the compiler to emit a warning when a compiler upgrade
would broaden a private effect set that affects a public function's verification. The author
must explicitly opt in to the new broader inference.

## Decision

**Closed — Refined Option D (edition-gated broadening), spec updated.**

Within an edition, inference is monotonically non-increasing (tighten only — Option A's invariant
applies within a release cycle). Across an edition boundary, broadening is allowed but treated as
a breaking change: itemized in the edition release notes, surfaced by `karac explain`, and
enforced as warning-then-error that must be resolved before the new edition compiles.

Rationale: under the assumption that LLMs write most code, the upgrade economics change — a
well-documented edition changelog is LLM-automatable, so the repair cost of a broadening is low.
More importantly, a missed effect in private inference means the public function's declared effect
set is silently incorrect (callers get a false guarantee). Edition-gating makes the correction
predictable and opt-in at the upgrade boundary rather than invisible forever.

`docs/design.md` Reported behavior bullet for private function inference updated. `spec-updated`


---

## F-006

### "Memory-safe" in Starting Assumption 5 is never formally defined

## Finding

`docs/design.md` § Starting Assumptions states:

> **Safe by default, unsafe by opt-in** — the language is **memory-safe** and effect-checked at
> the language level.

"Memory-safe" is used as a load-bearing term — it motivates the design of the ownership system,
the unsafe escape hatch, and the embedded/ISR model — but it is never defined anywhere in the
spec. Memory safety has multiple distinct components that are not all guaranteed by the same
mechanisms:

| Property | Meaning |
|----------|---------|
| Spatial safety | No out-of-bounds reads or writes |
| Temporal safety | No use-after-free, no dangling pointers |
| Type safety | No type confusion (reading a value as the wrong type) |
| Data race freedom | No simultaneous mutable + read access from multiple threads |
| Integer safety | No overflow with undefined behavior (distinct from panicking) |

Rust's memory safety guarantee is well-defined: "no undefined behavior in safe code," which
covers spatial, temporal, and type safety via the borrow checker and bounds checking. Data races
are prevented by `Send`/`Sync`. Integer overflow is defined behavior (panic or wrap, depending on
build mode).

Kāra's guarantee is not stated. This matters because:

1. **Embedded targets** often disable bounds checking for performance. Does "memory-safe" still
   hold on `embedded` profiles? If not, is there a profile-level annotation that says so?

2. **The ownership system** (Feature 4) provides temporal safety. But if the compiler falls back
   to RC, does temporal safety still hold? (Yes, RC prevents dangling, but the spec doesn't
   connect these explicitly.)

3. **Data races**: Kāra has auto-concurrency (Feature 5) and effect analysis. Is data race freedom
   a consequence of effect analysis, and if so, is it formally guaranteed?

## Example — embedded profile + bounds check

```kara
#[profile(embedded)]
fn read_register(buf: Slice[u8], idx: u32) -> u8 {
    buf[idx]   // is this bounds-checked? what happens if idx is out of range?
}
```

The spec doesn't say whether `embedded` profile disables bounds checking or whether the
`memory-safe` guarantee still applies.

## Options

**Option A — Add a formal definition of memory safety to Starting Assumptions.**
Define the four properties (spatial, temporal, type, race freedom) and state which are guaranteed
in safe code on all targets, and which may be weakened by profiles or `unsafe`.

**Option B — Add a forward reference to the ownership section.**
Keep the assumption brief but add: "See Feature 4 (Tiered Ownership) for the formal memory
safety model, and § Unsafe Escape Hatch for the boundary where the guarantee ends."

**Option C — Clarify the profile impact on memory safety explicitly.**
Add a note to § Project Profiles stating which safety properties are weakened by each profile
(e.g., `embedded` profile may disable bounds-checking panics in favor of `unsafe` index
operations; this is an explicit safety tradeoff the profile author takes on).

## Decision

**Closed — Options A + C, spec updated.**

Starting Assumption 5 now formally defines five memory-safety properties via a table:
temporal safety, type safety, data race freedom (all universal — hold on every profile in safe
code), spatial safety (holds everywhere; `embedded`/`isr` may opt out of bounds-checking panics
explicitly via `bounds_checks = false`), and integer overflow (defined behavior on all profiles —
never UB; behavior is panic by default, wrapping on `embedded` by default).

Data race freedom is noted as structurally enforced by the effect conflict system and RC→Arc
promotion — stronger than Rust's `Send`/`Sync` opt-in model.

§ Project Profiles gains a "Memory safety properties under restricted profiles" paragraph
stating that `embedded`/`isr` may only weaken spatial safety and integer overflow behavior,
always explicitly and never silently. Temporal safety, type safety, and data race freedom are
not weakened by any built-in profile.

`docs/design.md` Starting Assumptions §5 and § Project Profiles updated. `spec-updated`


---

## F-007

### "Public function" in Guaranteed semantics: does visibility context matter?

## Finding

`docs/design.md` Seed Classification § Guaranteed semantics states:

> The effect set of a **public** function is part of its signature. A caller that type-checks
> against a declared effect set will continue to type-check across compiler versions.

The spec uses "public" without defining what it means in this context. Consider:

```kara
// Case 1: pub fn in a pub module — externally reachable
pub mod network {
    pub fn connect(addr: Str) with sends(Network) { ... }
}

// Case 2: pub fn in a private module — not externally reachable
mod internal {
    pub fn helper() with writes(Log) { ... }
}

// Case 3: pub(crate) fn — reachable within crate only
pub(crate) fn shared_util() with reads(DB) { ... }
```

The guarantee is about cross-version stability for callers. External callers can only reach
Case 1. Cases 2 and 3 are not in the public API.

- **Case 2**: `pub fn` inside a private module is technically `pub` at the item level, but
  unreachable from outside the crate. If the author changes its effects, no external caller breaks.
  Does the guaranteed-semantics promise still apply?

- **Case 3**: `pub(crate)` creates an intra-crate API. Does it warrant the same stability
  guarantee as fully-public functions, or is it treated like private functions (reported behavior)?

The distinction has real consequences: if `pub(crate)` functions are guaranteed, refactoring
internal crate structure (narrowing or widening `pub(crate)` effect sets) becomes a versioned
breaking change even though no external caller exists.

## The intent

The intent is almost certainly: "public" means "reachable by external callers who cannot see your
source" — i.e., functions in the crate's public API surface. A `pub fn` in a private module and
a `pub(crate) fn` are not in that surface.

## Options

**Option A — Define "public" as "pub and reachable from outside the crate."**
Add a clarifying sentence: "For this guarantee, 'public' means the function is `pub` and
accessible via a fully-public module path from outside the crate. `pub(crate)`, `pub(super)`, and
`pub` items in private modules are not covered."

**Option B — Extend the guarantee to `pub(crate)` as an intra-crate API.**
Treat `pub(crate)` as a crate-internal API with its own stability guarantee — breaking a
`pub(crate)` effect set is a compile error within the crate. This is stricter but aligns with
large codebases that use `pub(crate)` as an internal contract.

**Option C — Make the guarantee visibility-level parameterized.**
"The effect set of a function is guaranteed for callers at any visibility level that can see the
declaration." A `pub` function's effects are guaranteed to all callers; a `pub(crate)` function's
effects are guaranteed to intra-crate callers; a private function with explicit effects is
guaranteed to callers in the same module.

## Decision

**Closed — Won't Fix (false premise). Spec clarified.**

The finding was written against Rust's visibility model. Kāra has no `mod` keyword, no
`pub(crate)`, and no `pub(super)`. Modules are files; the module tree is the directory tree.
Kāra's three-tier visibility model is: `pub` (external API, crosses the package boundary),
default/no-keyword (project-internal), and `private` (same directory only).

Cases 2 and 3 from the finding do not exist in Kāra — there is no such thing as a `pub fn`
inside a private module block, and `pub(crate)` is not a valid qualifier.

`pub` is the only visibility level that reaches external callers, so "public function" is
unambiguous. The Guaranteed semantics bullet now states this explicitly, anchoring "public" to
Kāra's `pub` keyword and noting the absence of Rust-style visibility sub-qualifiers.

`docs/design.md` Guaranteed semantics first bullet updated. `spec-updated`


---

## F-008

### Single uppercase letters cannot be Type-class but are used as generic type parameters throughout the spec

## Finding

`docs/design.md` defines Type class as:

> First alphabetic character is uppercase, **and at least one subsequent alphabetic character is
> lowercase.**

`docs/syntax.md` TYPE_IDENT comment reinforces this:

> Must contain at least one LOWER_ALPHA.

By this definition, single uppercase letters — `T`, `A`, `B`, `K`, `V`, `E`, `R`, `S` — are
**not** Type-class. They have no subsequent alphabetic character at all, let alone a lowercase one.
By the Const-class rule ("all alphabetic characters uppercase"), they are Const-class.

But CN-1 says:

> Every `struct`, `enum`, `trait`, `type`, `distinct`, **generic type parameter**, and
> `effect resource` declaration introduces a Type-class identifier.

And the spec uses single uppercase letters as generic type parameters throughout:

```kara
trait Iterator[T] { ... }       // T is Const-class by the definition
fn sort[T: Ord](items: ...)     // T is Const-class by the definition
struct HashMap[K, V] { ... }    // K, V are Const-class by the definition
```

These are direct contradictions. Either the definition is wrong, or the examples are wrong, or
there is an unstated exception.

## Root cause

The design.md definition was written with multi-character names in mind. Single uppercase letters
are a standard and universal convention for generic type parameters (Rust, Haskell, Java, Swift,
C++, etc.) and the spec examples clearly rely on them.

## Options

**Option A — Extend the Type-class definition to include single uppercase letters.**
Add to the definition: "OR the identifier is a single uppercase alphabetic character." This
explicitly carves out single-letter type parameter names.

```
Type class: first alphabetic character is uppercase AND (at least one subsequent alphabetic
character is lowercase OR the identifier is exactly one alphabetic character).
```

Corresponding syntax.md change to TYPE_IDENT:
```
TYPE_IDENT = UPPER_ALPHA ( LOWER_ALPHA { ALPHA | DIGIT | "_" } { ALPHA | DIGIT | "_" }
           | /* empty — single letter */ )
```
(Or more readably, document the carve-out in the comment.)

**Option B — Allow single uppercase letters as both Type and Const class depending on position.**
`T` is accepted in any position that requires TYPE_IDENT (generic parameter, type alias, etc.)
and in any position that requires CONST_IDENT (module-level constant). The class is position-
determined for single uppercase letters. Ambiguity in expressions is resolved by scope.

This is flexible but weakens the "one-glance" property — you can't tell `T`'s class without
knowing context.

**Option C — Require multi-letter names for all Type-class identifiers; deprecate single-letter generics.**
Accept the consequence: `fn sort[Item: Ord](...)` not `fn sort[T: Ord](...)`. Single uppercase
letters are CONST_IDENT and can only be used as constants.

This is a breaking change from universal convention and makes Kāra distinctly hostile to
experienced programmers. Not recommended.

**Option D — Redefine Type class as "starts with uppercase" (drop the lowercase requirement).**
Make TYPE_IDENT simply `UPPER_ALPHA { ALPHA | DIGIT | "_" }` without any constraint on subsequent
characters. Const class becomes a subset of Type class by position (only allowed at module scope
for constant bindings). The disambiguation between `T` and `TC` and `TextColor` is positional,
not lexical.

This is the simplest grammar change but means the lexer can no longer classify by pattern alone.

## Impact on path disambiguation

Option A is the most conservative and preserves the one-glance invariant for multi-letter names
while allowing the universal `T`/`K`/`V` convention. It should be the default recommendation
unless Option D is chosen as part of a broader reconsideration of the case-class system.

## Decision

**Closed — Option A, spec updated.**

The Type-class pattern in the identifier table now reads: "first alphabetic character is
uppercase, and (at least one subsequent alphabetic character is lowercase, OR the identifier is
exactly one alphabetic character)." A new CN-7 rule makes the single-letter carve-out explicit
and explains why it causes no reader confusion in practice (module-scope constants are always
multi-character). The old CN-7 (FFI exception) is renumbered CN-8.

`docs/syntax.md` TYPE_IDENT comment updated to note the CN-7 carve-out for single-character
identifiers.

`docs/design.md` identifier table and CN-6/CN-7 rules updated. `docs/syntax.md` TYPE_IDENT
comment updated. `spec-updated`


---

## F-009

### Path disambiguation rationale contradicts its own example

## Finding

`docs/design.md` § Rationale states:

> The compiler decides at lex time which is which by looking at the first segment's case class:
> **Type- or Const-class starts a path expression; Value-class starts a value expression.**

But the example given immediately before this claim:

> `std.fs.read_to_string` vs `user.name.trim()`

`std` is a module name. Module names are Value-class (CN-2: "module filenames... introduce a
Value-class identifier"). So `std.fs.read_to_string` starts with a Value-class segment — which
the disambiguation rule says must be a value expression (field access), not a path.

The rule as stated cannot correctly disambiguate:

```kara
let std = some_value
std.fs.read_to_string(path)   // field access on local `std`? or module path?
```

Both readings are Value-class first segments. The compiler cannot resolve this at lex time using
case class alone — it requires scope lookup to know whether `std` refers to a local binding or the
module `std`.

## Additional adversarial examples

```kara
// Value-class name that is both a local variable and a module
let db = Db.new()
db.connect()           // field/method on local `db`? or path into module `db`?

// Const-class path into a module — does this work?
let MAX = 100          // Const-class binding
MAX.something          // Is this a path? `MAX` is Const-class → path rule applies
                       // But MAX is a value, not a module. Confusing.
```

## Root cause

The disambiguation claim is too strong. The actual rule is probably:

- If the first segment is Type-class, it is unambiguously a type path (types are never local
  value bindings).
- If the first segment is Const-class, it could be a path OR a constant access — resolved by
  scope.
- If the first segment is Value-class, it could be field access on a local variable OR a module
  path — resolved by scope.

"At lex time" is incorrect for Value-class and Const-class segments. Only Type-class provides
true lex-time disambiguation.

## Options

**Option A — Correct the rationale to reflect reality.**
Revise the claim to: "A Type-class first segment unambiguously starts a type path, resolved at
lex time. Value-class and Const-class first segments are resolved by name lookup during the
resolver phase: if the name refers to a module, it's a path; if it refers to a local binding or
constant, it's a value expression."

**Option B — Require module paths to use an explicit sigil or keyword.**
E.g., `::std.fs.read_to_string` for absolute paths, similar to Rust's `::` for crate-root paths.
This restores true lex-time disambiguation by making path vs field access syntactically distinct
for Value-class names.

**Option C — Require modules to be accessed via Const-class or Type-class names.**
Rename modules to start with uppercase or all-caps. This is a significant constraint on the
module system but makes disambiguation purely lexical. Conflicts with CN-2 which already
establishes module names as Value-class.

## Decision

**Closed — Option A, spec updated.**

The "at lex time" claim was an overclaim. The rationale now correctly describes three cases:

- Type-class first segment: always a type path, lex-time (no scope lookup needed — types are
  never local bindings).
- Const-class first segment: never a module path (module names are Value-class by CN-2), so
  always a constant/binding — also effectively lex-time by elimination.
- Value-class first segment: resolver-phase lookup. If the name resolves to a module → path;
  if it resolves to a local binding → field/method access. Both parse identically; the resolver
  distinguishes them.

The bad example (`std.fs.read_to_string` vs `user.name.trim()` — both Value-class first
segments, illustrating nothing about lex-time disambiguation) is replaced with a Type-class
example (`Vec[i32].default()` vs `user.name.trim()`) that actually demonstrates the guarantee.

`docs/design.md` § Rationale, point 1 rewritten. `spec-updated`


---

## F-011

### TYPE_IDENT grammar in syntax.md is broader than its comment and design.md's definition

## Finding

`docs/syntax.md` defines:

```
TYPE_IDENT  = UPPER_ALPHA { ALPHA | DIGIT | "_" }
              // Must contain at least one LOWER_ALPHA.
```

The grammar `UPPER_ALPHA { ALPHA | DIGIT | "_" }` allows:
- Zero subsequent characters → single uppercase letter (`T`, `A`)
- All-uppercase subsequent characters → `TC`, `HTTP`, `MAX2`

But the comment says "Must contain at least one LOWER_ALPHA." The grammar does not enforce this.
`ALPHA` includes both `UPPER_ALPHA` and `LOWER_ALPHA`, so `HTTP` (all uppercase) satisfies the
grammar but violates the comment.

This means the grammar is the wrong tool for enforcing the case class invariant — it is
over-permissive. The compiler must add a post-lex check to validate the lowercase requirement.

## Consequence

Identifiers like `HTTP`, `TC`, `DB` all match TYPE_IDENT grammatically but should be CONST_IDENT
per design.md. A parser that uses the grammar as-is would misclassify them.

This is related to F-008 (single uppercase letters): `T` matches TYPE_IDENT grammatically AND
CONST_IDENT grammatically. Without an external disambiguation rule, the parser does not know
which class `T` belongs to.

## Correct TYPE_IDENT grammar (if the "at least one lowercase" rule is kept)

```
TYPE_IDENT = UPPER_ALPHA { ALPHA | DIGIT | "_" }*<contains-lower>
```

This cannot be expressed cleanly in standard BNF. Options:
1. Express it in two productions:
   ```
   TYPE_IDENT = UPPER_ALPHA { UPPER_ALPHA | DIGIT | "_" } LOWER_ALPHA { ALPHA | DIGIT | "_" }
              | UPPER_ALPHA { ALPHA | DIGIT | "_" } LOWER_ALPHA { UPPER_ALPHA | DIGIT | "_" }
   ```
   (Unwieldy — requires stating the lowercase can appear at any position.)

2. Define it as a post-lex check:
   ```
   TYPE_IDENT = UPPER_ALPHA { ALPHA | DIGIT | "_" }
                where the identifier contains at least one LOWER_ALPHA character
   ```

3. Accept that the grammar is over-permissive and document the disambiguation algorithm
   separately (resolve ambiguity between TYPE_IDENT and CONST_IDENT by checking for lowercase).

## Relationship to CONST_IDENT

CONST_IDENT = `UPPER_ALPHA { UPPER_ALPHA | DIGIT | "_" }` is a subset of TYPE_IDENT grammatically.
An all-uppercase identifier starting with uppercase matches BOTH. The spec resolves this by
saying all-uppercase → Const class; has-lowercase → Type class. But the grammar doesn't express
this disjoint requirement — it must be enforced outside the grammar.

## Options

**Option A — Add a post-lex note to syntax.md.**
Keep the grammar as-is (over-permissive) and add a note: "The three class grammars overlap for
all-uppercase identifiers starting with uppercase. Disambiguation rule: if all alphabetic
characters are uppercase → CONST_IDENT; if at least one subsequent alphabetic character is
lowercase → TYPE_IDENT; if first alphabetic character is lowercase or identifier begins with `_`
→ VALUE_IDENT. This classification is applied post-lex and is not expressed in the grammar
productions."

**Option B — Rewrite the grammar to be precise (Option A from F-008).**
Add a carve-out for single uppercase letters and express the "contains lowercase" requirement
through grammar restructuring (verbose but unambiguous).

**Option C — Simplify: define a single IDENT production and classify post-lex.**
Remove TYPE_IDENT, CONST_IDENT, VALUE_IDENT as grammar terminals. Use IDENT everywhere in
productions, with a semantic rule that classifies after lexing. Grammar is simpler; the
classification algorithm lives in one place.

## Decision

**Closed — Option A, spec updated.**

A post-lex classification algorithm is now documented in `docs/syntax.md` immediately after
the three grammar productions. The grammar remains over-permissive (intentionally — the
constraint cannot be expressed cleanly in BNF); the normative rule is the four-step algorithm:

1. First char lowercase or `_` → VALUE_IDENT
2. First char uppercase, exactly one alphabetic char (CN-7) → TYPE_IDENT
3. First char uppercase, all remaining alphabetic chars uppercase → CONST_IDENT
4. First char uppercase, at least one subsequent lowercase → TYPE_IDENT

Steps 2–4 are disjoint and cover all uppercase-starting cases exactly. The note explains that
grammar productions are intentionally over-permissive and that the algorithm is the normative
classification. Coordinated with F-008 (CN-7 single-letter carve-out is step 2 above).

`docs/syntax.md` post-lex classification block added. `spec-updated`


---

## F-014

### Least Upper Bound (LUB) algorithm referenced but not defined

## Finding

`docs/design.md` § Type Inference references LUB in several places:

> In synthesis mode, branches synthesize independently and the result is their least upper bound
> — a compile error if no LUB exists (Kāra has no `Any` type and does not widen arbitrary pairs).

The spec never defines what LUB means for Kāra's type system. This matters in several cases:

### Case 1 — Generic types with partially-inferred parameters

```kara
fn f(cond: bool) {
    let x = if cond { Ok(42) } else { Err("oops") }
    // Ok(42)  synthesizes as Result[i64, _]  (error type unknown)
    // Err("oops") synthesizes as Result[_, String] (ok type unknown)
    // LUB is Result[i64, String] — but is this defined?
}
```

Without a check-mode expected type, does the compiler join the two partial `Result` types into
`Result[i64, String]`? Or is this a "no LUB" error? This is the common idiom in error-handling
code. Rust handles it via constraint solving across both branches simultaneously — Kāra's
"local, not global" inference constraint makes the answer unclear.

### Case 2 — Refinement types

```kara
type Positive = i64 where self > 0
type NonNegative = i64 where self >= 0

fn f(cond: bool, a: Positive, b: NonNegative) -> ??? {
    if cond { a } else { b }
    // Positive <: i64, NonNegative <: i64
    // LUB(Positive, NonNegative) = ?
    // Both are subtypes of i64 — is the LUB i64? Or NonNegative (wider refinement)?
}
```

Is the LUB of two refinement types their common base type? Or the union of their predicates? Or
the widest refinement that covers both?

### Case 3 — Enum variants in match

```kara
enum Shape { Circle(f64), Square(f64), Triangle(f64, f64) }

let area = match shape {
    Circle(r)        => 3.14 * r * r,
    Square(s)        => s * s,
    Triangle(b, h)   => 0.5 * b * h,
}
// All arms synthesize f64 — LUB is f64. Fine.

// But:
let desc = match shape {
    Circle(_)   => "round",
    Square(_)   => 42,     // &str vs i64 — no LUB, compile error
    Triangle(..) => true,
}
```

The last case should be a compile error. The spec says "a compile error if no LUB exists." But
what error message? What does the compiler report: "no common type for `&str`, `i64`, `bool`"?

## What needs to be defined

1. **LUB for nominal types**: `LUB(A, B)` where `A` and `B` are the same generic type with
   different parameters — is it the generic type with LUB parameters? Only if the type is
   covariant in those parameters (which for nominal types it is not — they're invariant). So
   `LUB(Vec[i64], Vec[String])` = no LUB, compile error.

2. **LUB for refinement types**: Is `LUB(Positive, NonNegative) = i64` (base type)? Or
   `NonNegative` (looser predicate covers both)?

3. **LUB for partial types** (one branch synthesizes `Result[i64, _]`, other `Result[_, String]`):
   Does the compiler attempt to unify the unknowns across branches, or is each branch
   independently synthesized with no cross-branch propagation ("local, not global")?

4. **Error message format** when no LUB exists.

## Decision

**Closed — LUB algorithm defined in spec.**

A five-rule LUB algorithm is now documented in § Type Inference under *LUB algorithm*:

1. Identical types → that type.
2. Same generic constructor, some parameters unknown → unify unknowns across branches within
   that constructor only (bounded cross-branch propagation; not global constraint solving).
   Handles the canonical `Ok(x)` / `Err(e)` idiom without annotation.
3. Same generic constructor, all parameters known but differing → no LUB (nominal types are
   invariant). Compile error; programmer must annotate to enter check mode.
4. Refinement types sharing a base → base type. No predicate reasoning (conservative; avoids
   requiring a theorem prover in v1).
5. All other cases → no LUB, compile error listing all diverging branch types with a suggestion
   to add a return-type annotation.

Match arms use the same algorithm pairwise left-to-right.

`docs/design.md` *LUB algorithm* subsection added to § Type Inference. `spec-updated`


---

## F-015

### `?` operator desugaring and implicit `From::from` effects not addressed

## Finding

The `?` operator on `Result[T, OtherError]` in a function returning `Result[U, MyError]`
desugars to roughly:

```kara
match expr {
    Ok(v)  => v,
    Err(e) => return Err(MyError.from(e)),   // implicit From::from call
}
```

The `MyError.from(e)` call is implicit — the programmer didn't write it. But the conversion
traits section states:

> The four stdlib conversion traits (`From`, `Into`, `TryFrom`, `TryInto`) declare their methods
> `with _` so that impls may carry effects.

This means `MyError.from(e)` can have arbitrary effects. For example:

```kara
impl From[DbError] for AppError {
    fn from(e: DbError) -> AppError with writes(Log) {
        log.write(f"converting DbError: {e}")
        AppError.Database(e)
    }
}

fn process() -> Result[(), AppError] {
    let conn = db.connect()?   // implicit: AppError.from(DbError) with writes(Log)
    Ok(())
}
```

`process()` calls `db.connect()?` which implicitly calls `AppError.from(DbError)` which has
`writes(Log)`. Does `process()`'s inferred effect set include `writes(Log)`? Or does the
implicit `from` call in the `?` desugaring escape effect tracking?

The spec says effects come from calls in the function body. The `from` call is not visible in
the source — it's compiler-inserted. If the compiler does not track it, `process()` would have
an incorrect (incomplete) effect set. If it does track it, the spec should say so explicitly.

## Adversarial example — pure function secretly effectful

```kara
impl From[ParseError] for ApiError {
    fn from(e: ParseError) -> ApiError with sends(Metrics) {
        metrics.increment("parse_errors")
        ApiError.Parse(e)
    }
}

// This function looks pure but actually sends metrics via `?`
fn parse_input(s: String) -> Result[Data, ApiError] {
    let data = Data.parse(s)?   // implicit from() with sends(Metrics)
    Ok(data)
}
```

If `?`-inserted `from` effects are not tracked, `parse_input` has an invisible `sends(Metrics)`
effect that effect analysis misses entirely. A caller that forbids `sends(Metrics)` would
erroneously accept `parse_input`.

## Options

**Option A — Track effects of the implicit `from` call in `?`.**
Specify that `expr?` in a function returning `Result[T, E2]` from a `Result[T, E1]` context
contributes the effects of `E2::from(E1)` to the enclosing function's inferred effect set.
This is correct but requires the compiler to look up `from`'s effects at every `?` site.

**Option B — Require explicit `from` calls if the conversion has effects.**
`?` desugaring is only allowed when `From::from` for the relevant types is effect-free (i.e.,
has no effects beyond the default). If the `from` impl has effects, the programmer must write
the conversion explicitly to make the effects visible in source.

**Option C — Require `From` impls used via `?` to be effect-free.**
Ban effects on `From` impls that are used for `?` propagation. This is restrictive but makes
`?` semantics simple and effect-transparent.

## Decision

**Closed — Option A, spec updated.**

The `?` desugaring is now formally specified in § Error Handling with explicit effect-tracking
semantics: the compiler-inserted `E2.from(e)` call contributes the `From` impl's declared
effects to the enclosing function's inferred effect set, exactly as if the programmer had
written the call explicitly. Two edge cases are pinned:

- `E1 == E2`: no `from` call inserted, no additional effects.
- No `From[E1] for E2` impl: type error; effect question does not arise.

The rule is grounded in soundness: the `with _` declaration on `From::from` already exposes
impl effects; `?` desugaring must propagate them or the effect system has an invisible hole.

`docs/design.md` `?` desugaring and effect tracking bullet added to § Error Handling. `spec-updated`


---

## F-016

### `errdefer(e)` in functions returning `Option[T]` is undefined

## Finding

`docs/design.md` § Error Handling states:

> `errdefer(e)` with an error binding — The binding `e` has the type of the function's `Err`
> variant.

And:

> Because a panic is not an `Err` value of that type, `errdefer(e)` blocks are **skipped during
> panic unwind**.

Both descriptions assume the function returns `Result[T, E]` — there is an `Err` variant with
a type. But `?` also works in functions returning `Option[T]`:

> In a function returning `Option[T]`, `expr?` propagates `None`.

`Option[T]` has no `Err` variant. `None` carries no value. So what happens if the programmer
writes `errdefer(e)` in a function returning `Option[T]`?

```kara
fn find_user(id: u64) -> Option[User] {
    let record = db_records.get(id)?    // propagates None
    errdefer(e) {                       // ERROR? What type is `e`?
        log_failure(e)
    }
    Some(parse_record(record))
}
```

Three possible behaviors the spec must pick:

1. **Compile error** — `errdefer(e)` is only valid in functions returning `Result`. This is the
   cleanest and most consistent with the description.

2. **`e` binds `()` (unit)** — `None` has no payload, so `e` is `()`. The errdefer block runs
   on `?`-propagated `None`, with `e = ()`. The block runs but `e` is useless.

3. **Silently treat as parameterless `errdefer`** — if the function returns `Option`, `errdefer(e)`
   is treated as `errdefer` (drop the binding). This is lenient but could mask programmer mistakes.

## Related question — `errdefer` (parameterless) in `Option`-returning functions

The parameterless form `errdefer { ... }` runs on "non-success exit." In a `Result`-returning
function, non-success = `Err`. In an `Option`-returning function, non-success = `None`. Does
parameterless `errdefer` run when `?` propagates `None`? The spec says:

> `errdefer expr` — evaluates `expr` only when the enclosing scope exits via an error path
> (a `?` that propagates `Err`, or an explicit `return Err(...)`).

"or a `?` that propagates `Err`" — not `None`. So by the current wording, `errdefer` does NOT
run when `?` propagates `None`. Is this intentional? A programmer using `Option` may still want
cleanup-on-failure behavior.

## Decision

**Closed — spec updated.**

1. **`errdefer(e)` in Option-returning functions: compile error.** The binding requires an
   error value; `None` carries none. Diagnostic: `errdefer with a binding requires Result
   return type; use errdefer { ... } for cleanup on None-propagation`.

2. **Parameterless `errdefer` fires on `None`-propagation.** "Non-success exit" now formally
   covers: `?` propagating `Err` (Result functions), `?` propagating `None` (Option functions),
   explicit `return Err(...)`/`return None`, and panic. Success is `Ok(...)`, `Some(...)`, or
   `()` return only. Three locations updated in § Error Handling: the `errdefer expr` bullet
   definition, the description after the `errdefer(e)` example, and the first Rules bullet.

`docs/design.md` three locations in § Error Handling updated. `spec-updated`


---

## F-017

### Prelude uses wildcard import internally but wildcard imports are blocked for user code

## Finding

`docs/design.md` § Module System states:

> **Wildcard imports** (`import path.*;`) and **nested grouping** (`import a.{b.{c, d}, e};`) are
> not available in v1.

But the same section also states:

> **Prelude.** The compiler prepends a synthetic `import std.prelude.*;` at each module's top at
> parse time — users never write it explicitly.

The prelude is injected as `import std.prelude.*;` — a wildcard import — but that syntax is
unavailable to users. Two interpretations:

**Interpretation A — The wildcard syntax is supported in the grammar but blocked for user code.**
The parser and resolver handle `*`, but a user-written `import foo.*;` is rejected with
`E0xxx WildcardImportNotAvailable`. The prelude injection bypasses this restriction by being
compiler-inserted (not user-written). This is internally consistent but exposes a two-tier import
system that is not documented.

**Interpretation B — The prelude uses an entirely different internal mechanism.**
`import std.prelude.*;` in the spec description is just a conceptual shorthand for "all prelude
items are injected." The compiler doesn't actually process a wildcard import statement — it
injects the specific prelude items directly into the scope table without going through the import
machinery. In this case the grammar truly never supports `*`, and the spec's `import std.prelude.*`
notation is informal.

## Why this matters

- If Interpretation A: the grammar supports `*` but it's user-blocked. A future version that
  enables wildcard imports for users is a purely additive change (remove the restriction). The
  spec should document this.

- If Interpretation B: the spec's notation is misleading. The prelude description should say
  "the compiler automatically places all prelude items in scope, equivalent to individually
  importing each item" rather than using the `*` syntax.

The current wording conflates a concrete syntax (`import std.prelude.*;`) with a conceptual
description, and does so in a section that just said wildcard imports don't exist.

## Decision

**Closed — both wildcard imports and nested grouping promoted to v1.**

Neither was as complex as originally assumed. Both are now in v1:

- **Wildcard imports** (`import path.*;`): bring all `pub` items from the named module into
  scope. Precedence rules: explicit import beats wildcard; wildcard vs wildcard collision is an
  error at the use site (not import site), `E0226 AmbiguousWildcardImport`; prelude has lowest
  priority and is shadowed by any user import.

- **Nested grouping** (`import a.{b.{c, d}, e}`): pure syntactic sugar — expands to flat
  imports before the resolver runs. Zero new resolution behavior.

The prelude description is updated: `import std.prelude.*;` is now real wildcard import syntax
processed through the normal import machinery at lowest precedence — not a synthetic/informal
notation.

`docs/design.md` § Module System wildcard/nested bullet replaced with full spec; prelude
description updated. `docs/syntax.md` IMPORT_TAIL and IMPORT_ITEM productions updated with
wildcard and nested forms; examples added. `spec-updated`


---

## F-019

### `edition` field is present but its semantic implications are undefined

## Finding

`docs/design.md` § Package System states:

> `[package].edition` (optional — validated if present, defaulted otherwise)

And the manifest template always emits `edition = "2026"` with the comment:

> `edition` is written explicitly (not defaulted) so users see the concept on day one and are
> not silently upgraded if the default changes.

The spec never defines:
1. What values are valid for `edition`.
2. What a compiler does differently based on the edition value.
3. What the "default" edition is (when the field is absent).
4. How editions interact with dependencies that may declare different editions.

## Why this matters

Edition is a forward-compatibility mechanism — the entire point is that a future edition can
change language semantics without breaking code written for an earlier edition. If the semantics
of each edition are not defined, the mechanism is a placeholder with no load-bearing function.

Concrete questions:

- Is `"2026"` the only valid value in v1? If so, what does "validated if present" mean — just
  checking it's a known string?
- When a future `"2027"` edition lands, what language changes does it gate? The spec doesn't
  define the edition boundary process.
- If a library is edition `"2026"` and a binary is edition `"2027"`, do they interoperate? Can
  a `"2027"` compiler consume a `"2026"` library? Rust's answer: yes, editions are a per-crate
  property and the compiler handles mixed editions.
- What is the "default" edition when the field is absent? The current year? The earliest edition?

## Adversarial example

```toml
# What does this do differently than edition = "2026"?
[package]
name = "mylib"
edition = "2027"   # future edition — accepted? rejected? ignored in v1?
```

## Options

**Option A — Define `edition` as purely forward-compatible bookkeeping in v1.**
State explicitly: "In v1, the only valid edition is `2026`. Future editions may change language
behavior; the process for defining editions is deferred. A manifest with `edition = "2026"` is
semantically identical to one without the field. The field is present to future-proof the
manifest format and ensure users are edition-aware from day one."

**Option B — Define the edition boundary process now.**
Specify: what triggers a new edition (breaking changes), how editions are numbered, and what the
compiler does when it encounters an unknown edition value. At minimum: unknown edition = compile
error with suggestion to upgrade the compiler.

## Decision

**Closed — Option A + structural rules from Option B, spec updated.**

Option A alone was insufficient because F-005 already made editions load-bearing (inference
broadening at edition boundaries). The spec now defines:

1. Valid values in v1: `"2026"` only. Unknown value = compile error with upgrade suggestion.
2. Default when absent: `"2026"` (earliest edition — never silently upgrades).
3. Semantic effect in v1: `edition = "2026"` is identical to absent — no behavior difference.
4. Per-crate property: workspaces may mix editions; newer compiler always handles older editions;
   libraries do not impose their edition on consumers.
5. What a new edition gates: breaking language changes requiring source modification. One
   concrete example already in spec (F-005: inference broadening). Future editions add to this
   list at ship time; the edition declaration process is deferred until a second edition is ready.

`docs/design.md` **Edition semantics** subsection added to § Package System. `spec-updated`


---

## F-020

### Workspace-level dependency declaration syntax is not shown in the spec

## Finding

`docs/design.md` § Package System states:

> Workspace-level `[dependencies]` declarations are inherited by members that opt in:
> `http = { workspace = true }` in a member's `kara.toml` uses the version from the workspace
> root.

But the workspace manifest example only shows:

```toml
[workspace]
members = ["core", "cli", "web"]
```

There is no example of how workspace-level dependencies are declared in the root manifest.
A member writes `http = { workspace = true }` — but where does the workspace root declare the
`http` version that this refers to?

## What's missing

The spec never shows the root manifest's syntax for shared dependencies. In Cargo, this is
`[workspace.dependencies]`:

```toml
[workspace]
members = ["core", "cli", "web"]

[workspace.dependencies]
http = "1.2"
json = "0.8"
```

Kāra may use the same pattern, a different key, or an entirely different mechanism. Without the
syntax, the `workspace = true` opt-in is dangling — it references something that has no defined
declaration site.

## Adversarial example

```toml
# workspace root kara.toml — how do I declare shared http version?
[workspace]
members = ["core", "cli"]

# ??? — what goes here?
```

```toml
# member core/kara.toml
[dependencies]
http = { workspace = true }   # references... what, exactly?
```

## Decision

**Closed — spec updated.**

Workspace-level dependency declarations use `[workspace.dependencies]` at the root manifest
(same pattern as Cargo — familiar, no reason to differ). A complete root + member example pair
is now in the spec.

Missing declaration: `workspace = true` in a member for a dep not in `[workspace.dependencies]`
is a compile error: `dependency 'http' uses workspace = true but 'http' is not declared in
[workspace.dependencies]`. Members may still declare their own version constraints for deps not
in `[workspace.dependencies]` — `workspace = true` is opt-in per dependency.

`docs/design.md` § Package System Workspaces paragraph updated with `[workspace.dependencies]`
syntax, full root + member example, and error case. `spec-updated`


---

## F-021

### When is the package name used as a module path prefix?

## Finding

`docs/design.md` § Package System states:

> **Invariant: directory name = package name = root module name**

And § Module System states:

> All types, functions, and effect resources are namespaced by their module path.
> `myapp.db.UserDB` and `otherlib.db.UserDB` are distinct resources.

And:

> Items in `main.kara` / `lib.kara` are hoisted to the crate root — `fn start()` in `main.kara`
> is reachable from other files as `start`, not `main.start`.

The spec never says when or whether the package name appears as the leading segment in a path.
This creates ambiguity in three cases:

### Case 1 — Intra-package paths

Inside `myproject`, does `db/connection.kara` define:
- `db.connection.Connection` (bare module path, no package prefix), or
- `myproject.db.connection.Connection` (package-prefixed)?

The hoisting rule says `main.kara` items are `start`, not `main.start`. By analogy, `db.kara`
items should be `db.something`, not `myproject.db.something`. But the spec never states this
explicitly for non-root files.

### Case 2 — Cross-package imports

When package `webserver` depends on package `http`, how does it import `http`'s Connection type?

```kara
import http.client.Connection    // bare package name as first segment?
import http.Connection           // package name, then hoisted lib.kara items?
```

The spec says imports are "always absolute from the crate root — there is no `self.` / `super.`
/ relative form." But "crate root" is ambiguous: the current package's root, or the global root
of all packages? If the latter, how are package names injected as root-level module segments?

### Case 3 — Effect resource disambiguation

> `myapp.db.UserDB` and `otherlib.db.UserDB` are distinct resources — the compiler treats them
> as non-conflicting.

This example uses the package name as the first path segment. But nowhere in the spec is it
stated that the package name is the mandatory root prefix for cross-package paths.

## The core question

Is `import db.connection.Connection` inside `myproject` a reference to:
- The `db` module within `myproject` (intra-package, package name is implicit), OR
- A top-level package named `db` (cross-package, no prefix needed)?

If both are valid and a package named `db` exists as a dependency, there is a namespace
collision that the spec does not address.

## Decision

**Closed — spec updated.**

All three questions answered in a new "Package name in paths" block in § Module System:

1. **Intra-package paths omit the package name.** `db/connection.kara` → `db.connection.Connection`. "Absolute from the crate root" means the current package's root. Consistent with hoisting rule.

2. **Cross-package paths use the dependency's package name as the first segment.** `import http.client.Connection`. The resolver identifies cross-package access by checking whether the first segment matches a declared dependency in `kara.toml`.

3. **Collision: intra-package wins; external must be aliased.** If a local module and a dependency share a name, the local module wins. The dependency gets an `alias` key in `kara.toml` (`db = { version = "1.0", alias = "db_ext" }`); without an alias it is shadowed and inaccessible.

4. **Canonical identity includes the package name** (for effect conflict analysis and diagnostics) — the compiler prepends it internally; user code within a package omits it.

The Imports section opening sentence is updated to replace the ambiguous "crate root" with the precise two-case description.

`docs/design.md` "Package name in paths" block added to § Module System; Imports sentence updated. `spec-updated`


---

## F-023 ✓ RESOLVED

### Public functions writing to module-level `let mut` cannot declare effects: synthetic resource is not namable

## Finding

`docs/design.md` § Module-Level Bindings states:

> Every module-level `let mut BINDING` implicitly declares a project-internal
> `effect resource BINDING_resource;`... The synthetic resource is not exportable and
> not namable in user code — it exists only for conflict analysis. A function that mutates
> `BINDING` has `writes(BINDING_resource)` in its inferred effect set... public functions
> must declare the effect explicitly per the usual `public_effects = "declared"` rule.

These two requirements are directly in conflict. A public function must declare its effects
explicitly, but the effect it needs to declare (`writes(BINDING_resource)`) uses a resource
that cannot be named in user code.

## Adversarial example

```kara
let mut COUNTER: i64 = 0

// How does this public function declare the write effect?
pub fn increment() with writes(???) {   // COUNTER_resource is not namable
    COUNTER += 1
}
```

Under the current spec:
- `writes(COUNTER_resource)` — rejected, resource not namable in user code
- `with writes(???)` — no valid spelling exists
- No `with` clause — the body has `writes(COUNTER_resource)` inferred, which exceeds
  the empty declared set → verification fails

So a public function that mutates a module-level `let mut` cannot compile under strict
`public_effects = "declared"` mode. This makes module-level `let mut` bindings essentially
inaccessible from public APIs.

## Is this intentional?

Possibly. The spec strongly discourages `let mut` in `lib` and `app` profiles (warning and
compile error respectively). It might be intentional that public mutation of global state is
unrepresentable in the effect system — forcing authors to use `Mutex[T]`, `Atomic[T]`, or
the context-struct pattern instead.

But the spec does not say this is intentional. It states the two requirements without noting
the conflict.

## Options

**Option A — Acknowledge the conflict and make it intentional policy.**
Add a note: "A `pub fn` that directly mutates a module-level `let mut` cannot satisfy
`public_effects = 'declared'` because the synthetic resource is not namable. This is by
design: direct public mutation of global state is not representable in the effect system —
use `Atomic[T]`, `Mutex[T]`, or a context-struct pattern instead."

**Option B — Make the synthetic resource namable via a scoped path.**
Allow `writes(module.BINDING)` or `writes(self::BINDING)` as the user-facing spelling.
The compiler maps this to the synthetic resource internally. The resource is still not
importable, but can be referenced within the declaring module and its children.

**Option C — Exempt module-level `let mut` writes from `public_effects = "declared"`.**
Treat writes to named module-level bindings as implicitly declared when the function body
contains such a write. The declaration is inferred from the binding's name, not the resource.
This is a special-case relaxation of the declared-effects rule.

## Decision

**Closed — Option A, spec updated.**

The conflict is intentional design policy. A `pub fn` that directly mutates a module-level
`let mut` binding cannot satisfy `public_effects = "declared"` because the synthetic resource
is not namable — and this is correct by design. Direct public mutation of global state without
a named synchronisation primitive is not representable in the full effect-declaration discipline.

Two paths forward for authors:

1. **Wrap in a named concurrency primitive.** Replace `let mut COUNTER: i64 = 0` with
   `let COUNTER: Atomic[i64] = Atomic.new(0)`. The `pub fn increment()` then declares
   `with writes(Atomic[i64])` (or similar named resource) — fully representable.

2. **Use `public_effects = "inferred"` at the project level.** This is the correct escape
   hatch for `embedded`-profile device-driver crates that expose raw MMIO or DMA writes via
   `pub fn`. Under `"inferred"` mode the compiler infers and displays the synthetic-resource
   effect without requiring an explicit declaration. Conflict analysis is unaffected — callers
   still observe conflicts between two functions that both write the same binding.

Option B (make the synthetic resource namable as `writes(module.BINDING)`) was considered and
rejected: it would require Const-class identifiers to be valid in effect position with
module-scoped visibility rules — real language complexity for a rare case that the existing
`public_effects = "inferred"` escape hatch already handles. Option C (exempt from declared
mode) silently weakens effect discipline and was rejected outright.

`docs/design.md` "Effect attribution" paragraph split into two; second paragraph documents
the `pub fn` restriction and the two escape paths. `spec-updated`


---

## F-024 ✓ RESOLVED

### `String` at module scope: string literal initializer conflicts with the heap-allocation ban

## Finding

`docs/design.md` § Module-Level Bindings lists as ✓ allowed:

```kara
pub let APP_NAME: String = "karac"
```

But the forbidden initializers include:

> Anything requiring runtime heap allocation.

`String` is a growable, heap-allocated string type (it appears alongside `Vec` in the prelude
and carries `allocates(Heap)` for dynamic operations). If `"karac"` is stored as a `String`,
it requires heap allocation — which is forbidden. Either:

1. `String` at module scope is **not** heap-allocated for literal initializers (it's stored in
   read-only binary data, like Rust's `&'static str`), or
2. The example is wrong and the type should be `StringSlice` (a borrowed/static string), or
3. There is a special carve-out for string literals that the spec doesn't state.

## Related question — `StringSlice` vs `String` for static data

The spec defines both `String` and `StringSlice` in the prelude. For:

```kara
pub let GREETING: String = "hello"
pub let GREETING2: StringSlice = "hello"
```

If both are valid, what is the difference at module scope? If `String` is heap-allocated and
`StringSlice` is a view into static data, then only `GREETING2` should be allowed at module
scope. But the spec shows `GREETING` (with `: String`) as the example.

## Adversarial example

```kara
// Is this allowed? String is heap-allocated.
pub let HOST: String = "localhost"

// Is this the correct form for a static string?
pub let HOST2: StringSlice = "localhost"

// What if you need to concatenate at compile time?
pub let URL: String = "http://" + "localhost"   // compile-time concatenation?
```

The last case is especially interesting: is `+` on string literals a compile-time constant
expression? The spec doesn't say.

## Options

**Option A — Specify that string literals at module scope are zero-copy static data.**
State: "A string literal used as a module-level binding initializer is stored in the binary's
read-only data segment. The type may be `String` (treated as a static string with no heap
involvement) or `StringSlice`. In either case, no `allocates(Heap)` effect is produced."

**Option B — Restrict module-scope string literals to `StringSlice`.**
State: "String literals at module scope must have type `StringSlice` (or a type alias thereof).
`String` requires heap allocation and is not permitted as a module-level binding type."

**Option C — Change the example to use `StringSlice`.**
The simplest fix: replace `pub let APP_NAME: String = "karac"` with
`pub let APP_NAME: StringSlice = "karac"` in the spec example and document that
`String` at module scope requires heap allocation and is therefore forbidden.

## Decision

**Closed — Options B + C combined, spec updated.**

`String` is heap-allocated and is forbidden at module scope — the heap-allocation ban in the
"Forbidden initializers" list already covers it; the example was simply wrong. The correct type
for a static string at module scope is `StringSlice`: a zero-copy pointer into the binary's
read-only data segment, no runtime allocation.

Three specific changes made to `docs/design.md` § Module-Level Bindings:

1. **Allowed initializers bullet** updated: string literals are explicitly called out as having
   type `StringSlice` at module scope, with a note that the function-body default of `String`
   does not apply.

2. **Forbidden initializers bullet** updated: added an explicit sentence that `String` is
   heap-allocated and therefore forbidden; `StringSlice` is the correct type.

3. **Example code block** corrected: `pub let APP_NAME: String = "karac"` →
   `pub let APP_NAME: StringSlice = "karac"`, and a `pub let HOST: String = "localhost"`
   error case added to make the rule concrete.

4. **"String literals at module scope" note** added after the code block, covering: static
   storage in the binary, the `StringSlice` type choice, why the `String` default does not
   apply, and that compile-time string concatenation is deferred to `const fn` (not in v1).

The `StringSlice`-is-borrowed-from-static-data question is the same as Rust's `&'static str` —
no lifetime annotation is needed at module scope because module-level bindings have no owner
that could be freed.

`docs/design.md` § Module-Level Bindings updated. `spec-updated`


---

## F-025 ✓ RESOLVED

### `par {}` concurrency rule for `let mut`: direct mutation only or transitive through call graph?

## Finding

`docs/design.md` § Module-Level Bindings states:

> Mutating a module-level `let mut` binding from inside a `par { }` region or from a
> `spawn()`-ed task is a **compile error** unless the binding's type is an explicit
> concurrency primitive.

"Mutating... from inside" is ambiguous: does it mean:

**(a) Direct mutation only** — only a bare assignment or `+=` directly in the `par {}` body:
```kara
par {
    COUNTER += 1   // caught — direct write in par body
    COUNTER += 2
}
```

**(b) Transitive through the call graph** — any call to a function that has `writes(COUNTER_resource)` in its inferred effect set, even indirectly:
```kara
fn increment() { COUNTER += 1 }   // writes(COUNTER_resource) inferred

par {
    increment()   // caught? or not?
    increment()
}
```

## Why this matters

The synthetic per-binding resource (`COUNTER_resource`) propagates through effect inference
like any other resource effect. The existing conflict analysis for `par {}` already serializes
tasks that have conflicting effects. Per the spec, two `writes(COUNTER_resource)` effects in
sibling `par {}` branches always conflict.

If the conflict-based serialization is what catches direct mutation, then transitive mutation
(through a function call) should also be caught — because the same effect propagates. In that
case, interpretation (b) is what the conflict analysis already does, and the "compile error"
is just the usual conflict-upgrade-to-error rule applied to this specific resource.

But if the rule is (a) only, the compiler must special-case direct writes to module-level
`let mut` in `par {}` bodies, separately from the general effect-conflict rule.

## The inconsistency

The spec says the concurrency rule is "a static rule backed by the synthetic-resource effect:
because `writes(BINDING_resource)` conflicts with itself across branches, the existing conflict
analysis would already serialize the two writes — but serializing within a `par {}` region is
almost never what the programmer meant, so the compiler upgrades it to an error."

This wording implies interpretation (b) — the rule applies wherever `writes(BINDING_resource)`
appears, which includes transitive calls. But "mutating... from inside" reads like (a).

## Adversarial cases

```kara
let mut COUNTER: i64 = 0

fn bump(n: i64) { COUNTER += n }           // writes(COUNTER_resource)
fn read_counter() -> i64 { COUNTER }        // reads(COUNTER_resource)

// Case 1: two write calls — should be error either way
par {
    bump(1)
    bump(2)
}

// Case 2: read + write — should conflict (reader-writer conflict)
par {
    bump(1)
    let _ = read_counter()
}

// Case 3: write in one branch, read in another — same conflict?
par {
    bump(1)
    println(read_counter())
}
```

Under effect-conflict analysis, cases 1, 2, and 3 all involve conflicting effects across
`par {}` branches and should all be errors (or at least serializations). Is that correct?

## Decision

**Closed — interpretation (b), spec updated.**

The `par {}` concurrency rule is interpretation (b): transitive through the call graph. The
spec's own rationale already implied this ("backed by the synthetic-resource effect") — the
"mutating from inside" phrasing was the only imprecision. It is now replaced with effect-set
language.

All three adversarial cases are errors:

- **Case 1** (`bump` + `bump`): both branches carry `writes(COUNTER_resource)` transitively →
  `writes` + `writes` conflict → error.
- **Case 2** (`bump` + `read_counter`): `writes(COUNTER_resource)` vs `reads(COUNTER_resource)`
  → reader–writer conflict → error.
- **Case 3** (`bump` + `println(read_counter())`): same reader–writer conflict, even with the
  read buried inside a call chain → error.

Two branches that both only carry `reads(BINDING_resource)` remain a non-conflict (`reads` +
`reads`) and are not an error — the rule only fires when at least one branch writes.

The updated spec wording:
- "any `par { }` branch or `spawn()`-ed task whose *transitive* effect set contains
  `writes(BINDING_resource)`" (replacing "mutating from inside")
- Explicit mention that calling a function that carries the effect is caught identically to
  an inline assignment
- Added the reader–writer case explicitly: a branch with `reads` + a sibling branch with
  `writes` on the same binding is also an error
- Kept the `reads` + `reads` non-conflict clarification

`docs/design.md` Concurrency rule paragraph rewritten. `spec-updated`


---

