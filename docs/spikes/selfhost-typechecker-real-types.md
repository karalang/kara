# Scoping: a real type representation for the self-hosted TypeChecker

**Status:** proposal / not started. **Author context:** written after the
coarse-category TypeChecker port reached its ceiling (Phase 12, 21 slices, 12
error kinds — see `docs/implementation_checklist/phase-12-self-hosting.md`).

## 1. Why this exists

`selfhost/src/typechecker.kara` today infers a **coarse i64 category** per
expression — `0 NUM · 1 BOOL · 2 CHAR · 3 STR · 4 UNKNOWN`, plus
`STRUCT_BASE(1000)+idx` and `ENUM_BASE(100000)+idx` for nominal identity. That
scheme deliberately collapses detail (every int width and both floats are one
`NUM`), which is exactly what let 21 slices land byte-identical against the seed
with near-zero false-positive risk. It is **conservatively sound**: it only
fires when both sides land in a known category, so it never rejects a correct
program.

It has now hit a hard wall. Every remaining `TypeErrorKind` the seed emits is
blocked by a *structural* limit of the category scheme, not by effort:

| Blocked check | Why the coarse scheme can't do it |
|---|---|
| arithmetic/bitwise int-vs-float mixing (`i64 + f64`, `i64 + u8`) | one `NUM` category can't distinguish widths or the int/float domain |
| `InvalidCast` | the selfhost AST has no cast node (`as` unrepresented) |
| `InvalidTupleIndex` (`t.5` on a 2-tuple) | no tuple-arity tracking |
| `NotAStruct` | parse/cross-module gated |
| **generics** (`Vec[T]`, `fn sort[T: Ord]`, instantiation) | no type variables, no substitution, no unification |
| **trait bounds** (`T: Ord`, method resolution by bound) | no trait/impl environment, no bound satisfaction |

Unlocking these is not another slice on the category scheme — it requires
**replacing the i64 category with a real recursive type representation** plus a
substitution + unification engine. That is a distinct sub-project with its own
architecture, risks, and verification story. This doc scopes it.

## 2. The target (what the seed has)

`src/typechecker/types.rs::Type` is a rich recursive enum (~1.7 kLOC of type
machinery alone; the surrounding inference in `exprs.rs`/`expr_call.rs`/
`expr_ops.rs` is ~8 kLOC). The load-bearing variants for generics:

- `Int(IntSize) · UInt(UIntSize) · Float(FloatSize) · Bool · Char · Str · Unit · Never`
- `Tuple(Vec<Type>) · Array{element,size} · Slice{element,mutable}`
- `Named { name, args: Vec<Type> }` — the generic instantiation form (`Vec[i64]`)
- `Function { params, return_type }`
- `Ref · MutRef · Rc · Arc · Pointer{…}`
- **`TypeParam(String)`** — a named generic parameter `T`
- **`TypeVar(TypeVarId)`** — a unification metavariable

Generic inference is: at a call to `fn f[T](x: T) -> T`, mint a fresh `TypeVar`
per type param, `substitute_type_params` the signature, `check_expr` each
argument against its param slot, `unify_types` the actual into the slot to bind
the var, then `resolve_type_vars` the result. `env.fresh_type_var()`,
`unify_types`, `substitute_type_params`, `resolve_type_vars`, `types_compatible`
are the five core primitives.

## 3. The representation in Kāra

A recursive type must be a **`shared enum`**, exactly like `Expr` / `Pattern` /
`TypeExpr` already are (RC handles for child edges). Proposed minimal core:

```kara
pub shared enum Ty {
    Prim(i64),             // 0..7: i-widths collapse? NO — see §3.1
    NamedTy(NamedType),    // { name: String, args: Vec[Ty] }
    TupleTy(Vec[Ty]),
    FnTy(FnType),          // { params: Vec[Ty], ret: Ty }
    RefTy(Ty),             // ref / mut ref (carry a bool)
    ParamTy(String),       // TypeParam
    VarTy(i64),            // TypeVar metavariable
    ErrTy(SpanNode),       // recovery
}
```

### 3.1 Primitive granularity — the first real decision

The whole point is to stop collapsing numerics. `Prim` must distinguish at
least `{ i8..i128, u8..u128, usize, f32, f64, bool, char, str, unit, never }`
(≈16 discriminants) so `i64 + f64` and `i64 + u8` can be rejected. This is the
minimum that unblocks the arithmetic int/float/width checks.

### 3.2 No HashMap → flat Vec substitution

The substitution map is `Vec[i64]` (var id → index) paralleled by `Vec[Ty]`
(binding), or a single `Vec[TySubst { id, ty }]` scanned linearly — the same
flat-Vec-plus-scan pattern the current env, fn table, and struct table already
use. `next_var: i64` mints ids.

## 4. The load-bearing risk: unification under Kāra's ownership model

This is the crux, and it is where the effort could stall. `unify(a: Ty, b: Ty)`
**recursively walks two shared-enum trees while mutating a substitution table** —
precisely the shape that produced every codegen footgun this port already hit:

- **B-2026-07-11-37** — an `Option[payload]` moved by value into a `mut ref
  self` method double-frees; must match `Option` inline.
- **B-2026-07-12-1** — passing a `self.<Vec field>` by ref to a free fn
  double-frees; must scan via a `ref self` method.
- **B-2026-07-12-31** — ~4+ heap-typed (`Vec`) locals in one arm of a large
  match corrupt the frame; must extract to a dedicated method.
- The established idiom that a match-bound `ref` to a nested node reads its
  `Vec` fields as **empty** under codegen, forcing by-value destructuring.

Unification is a `mut ref self` recursion over two `shared enum Ty` values that
reads nested `Vec[Ty]` args, allocates fresh vars, and pushes to a substitution
Vec — it will stress all of these simultaneously. **Expect to discover and file
2–4 new codegen bugs here**, each needing a minimal repro and a workaround (or a
codegen fix). Budget for that; it is not optional polish.

Mitigation: build unification as small, single-responsibility `ref self`/`mut
ref self` methods (never free fns taking `self.field`), keep ≤3 heap locals per
method (extract aggressively), and destructure `Ty` by value at every step.

## 5. Verification story

The category-scheme oracle rendered `(kind @off:len)` and diffed against
`karac::typecheck`. That still works for **error parity** and must stay green
throughout — the 12 existing kinds must not regress. Two additions:

1. **A type-render oracle.** Add `render_ty(t: Ty) -> String` mirroring the
   seed's `type_display`, and a differential test that infers a type for a
   curated expression corpus and diffs the rendered type against the seed's
   inferred type for the same expression. This is how substitution/unification
   correctness gets pinned before any new error kind is wired.
2. **The reality-check stays the safety gate.** Run the ported checker over the
   8 real modules; it must stay `(ok)` (zero false positives) at every step, as
   it has for all 21 slices. Generics inference over real generic code
   (`Vec[T]`, `Option[T]`, the AST's own `shared enum` payloads) is the real
   test of the engine.

Caveat inherited from the port: single-file seed-parity is **not** a valid
typechecker gate (the seed emits cross-module artifacts for imported types), so
the curated oracle + reality-check remain the parity vehicle, exactly as now.

## 6. Proposed phases

Each phase is independently landable, oracle-verified, and reality-check-clean —
the same discipline as the 21 category slices.

- **Phase A — `Ty` representation + `render_ty` + `ty_of_type` (TypeExpr→Ty).**
  Introduce the `shared enum Ty`, the primitive granularity of §3.1, and a
  render matching `type_display`. Rewrite `cat_of_type` as `ty_of_type`. No
  inference change yet — a scaffolding phase. *Verify:* render oracle over a
  type-annotation corpus.

- **Phase B — re-express the 12 existing checks on `Ty`.** Swap the i64
  category for `Ty` throughout the expr walk, keeping every existing error kind
  byte-identical. `cats_compatible` becomes `types_compatible(a, b)` (structural,
  no vars yet). This is the risky mechanical rewrite; the existing 202-entry
  oracle is the safety net — it must stay 100% green. *Payoff so far: none new;
  this is the migration cost.*

- **Phase C — numeric granularity checks.** With real int/float/width types,
  land the arithmetic/bitwise int-vs-float/width mixing errors that §1 lists as
  blocked (`i64 + f64`, `i64 + u8`). First genuinely-new coverage. *Verify:*
  new corpus entries + reality-check.

- **Phase D — substitution + unification engine.** `fresh_var`, the flat-Vec
  substitution, `unify(a, b) -> bool` with occurs-check, `resolve(t)` (walk
  vars to their bindings). No user-facing check yet — a pure engine. **This is
  the high-risk phase (see §4).** *Verify:* a unit-style oracle that unifies
  hand-built `Ty` pairs and asserts the resulting substitution/resolution
  (drive it from a small `.kara` harness, diff rendered results).

- **Phase E — generic instantiation at call sites.** Record fn generic params;
  at a call, mint fresh vars, substitute the signature, check args against
  slots, unify, resolve the return. Unblocks `fn sort[T](…)`, `Vec.new()`,
  `Option[T]` construction typing. *Verify:* generic-call corpus + reality-check
  over the AST modules (which are dense with `Vec[T]` / `Option[T]`).

- **Phase F — trait/impl environment + bound satisfaction.** Collect
  `trait`/`impl` items, check `T: Bound` at instantiation, resolve methods by
  bound. Largest and last; may itself split into sub-slices (declared bounds →
  satisfaction → method dispatch). *Verify:* trait-bound corpus + reality-check.

## 7. Cost, and the honest alternative

- **Rough size.** Phases A–B are a rewrite of the current 1.5 kLOC type layer
  plus a render (~+0.5 kLOC). D is small in LOC but high in debugging risk
  (§4). E–F are the bulk of new logic (~1–2 kLOC) and mirror the seed's
  heaviest inference files. Total is a **multi-week, ~10-slice** effort, larger
  than everything in the category port combined, and gated on surviving the
  unification codegen risk.

- **The alternative is legitimate.** The coarse checker is already
  conservatively sound and covers the entire non-generic surface with 12 error
  kinds, byte-identical. If the near-term Phase 12 goal is *a working
  self-hosted front-end that agrees with the seed on real code*, the category
  scheme already delivers that, and effort may be better spent on the **codegen
  port** (the next pipeline stage) than on generics inference. Generics matter
  for *rejecting more wrong programs*, not for compiling the compiler's own
  (already-checked) source.

- **Recommendation.** Do not start this until (a) generics-level rejection is an
  explicit goal, and (b) there is appetite for the unification codegen-bug hunt.
  If green-lit, **Phase D is the go/no-go milestone**: if unification can't be
  made codegen-clean in a reasonable bug budget, stop there — Phases A–C still
  deliver the numeric-mixing checks standalone and are worth landing on their
  own.
