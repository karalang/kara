# Design spike — trait-dispatched Reduce / ElementwiseMap / ElementwiseOrd unification

**Status:** 🟡 **S0–S5 + S6-pre + S6a COMPLETE (S0–S1 2026-06-30 `bcaff37d`,
`73af27b0`, `7adcc380`, `29b55062`; S2–S5 2026-07-01, S3 `b0a40963`+`eb21e300`,
S4 `2ff34611`; S6-pre probe matrix 2026-07-02 — see §3.3, which also surfaced +
fixed B-2026-07-02-10..13; S6a 2026-07-02 — see §3.4, which surfaced + fixed
the ref-handle-param deref bug B-2026-07-02-27, the mono side-table leak /
handle-instantiation collision, and the bound trait-arg substitution gap);
S6b–S6c open.** Unifies the three copy-pasted
reduce/element-wise/ordering implementations (Tensor, Column, `Stats.*`) behind
one internal kernel, then layers **user-extensible** surface traits on top. **S0
(interpreter twin + shared vocabulary):**
[`src/reduce_kernel.rs`](../../src/reduce_kernel.rs) holds the plain-data
`ReduceOp` vocabulary + the interpreter f64 math (`reduce_f64`,
`quantile_linear_sorted`); `Stats.*` and `Column`'s f64 reductions funnel
through it, and the byte-identical `tensor_minmax_reduce`/`column_minmax` +
`value_to_f64`/`val_f64` duplicates collapsed into `interpreter::helpers`. **S1
(codegen emitters, [`src/codegen/kernel.rs`](../../src/codegen/kernel.rs)):**
`ContainerAccess` (dense buffer + optional Arrow `bitmap`) with `emit_reduce_
fold` + `emit_reduce_minmax` and their `_gated` (validity) variants. All three
surfaces' `sum`/`prod`/`mean`/`min`/`max` funnel through them — Stats + Tensor
(dense), Column (validity-gated, folds valid slots + guards all-null). The old
~120-line `emit_scalar_reduce_loop` was deleted and Column shed ~150 lines.
Seeds and empty policy stay per-surface at the call sites. **S2 (f64-accumulator
family):** `emit_sum_f64_and_count` (dense-or-gated overflow-safe `Σ x as f64` +
count) and `emit_variance_from` (`mean = sum/count` → `Σ(x−mean)²` → Bessel-
adjusted divide, `bessel` knob) now back Column `mean`/`var`/`std` (sample, ÷ n−1)
and Stats `variance`/`stddev` (population, ÷ n); `column_sum_f64_and_count` and
Stats' hand-rolled variance loop are deleted, elements widen through the shared
`column_elem_to_f64`. **S3 (element-wise map family):** `emit_elementwise_map` —
one dense-or-gated map skeleton (SQL null propagation: dst bit = AND of operand
bits, compute only in the valid branch) parameterized on the second operand
(`MapOther`: access / broadcast scalar / none) and the per-element op
(`MapKernelOp::Binop` via `compile_binop_typed`, `::Neg` = IEEE `fneg` / checked
int `0−x`); Tensor `⊕`/`-t` and Column `⊕`/comparisons/`-c` route through it
(Column's three hand-rolled loops deleted), plus the interpreter twin
`map_binop_slots` (all four `eval_*_binop` paths + shared `broadcast_pair`).
S3 probing surfaced and fixed two pre-existing run-vs-build neg divergences
(B-2026-07-01-1 tensor `-0.0`, B-2026-07-01-2 column `i64::MIN` wrap) and
open-ledgered the interpreter narrow-int width-laxity class (B-2026-07-01-3).
**S4 (ordering family):** `emit_sort_scratch` — ONE insertion-sort skeleton
keyed by `SortKey` (`Value` f64 sort / `IndexInto` stable argsort) — behind
`column_sort_f64_inplace` (now an adapter serving Stats `sort`/`median`/
`percentile`, Column `median`/`quantile` via `column_sorted_valid_f64`, and
the `DataFrame.describe` quartiles) plus `Stats.argsort`'s keyed index sort;
and `emit_reduce_argminmax` (first-occurrence compare-select, float+int
predicates) behind `Stats.argmin`/`argmax` (`Option` wrap stays at the call
site). ~400 lines of duplicated sort IR-builder code deleted. Note: the
"lands Column `median`/`quantile` codegen" bonus predicted below was already
delivered by an earlier slice (`column_sorted_valid_f64` predates S4); S4
instead retired that function's inline duplicate of the sort.
**S5 (non-f64 element axis):** `Stats.*` accepts `Slice[i64]`/`Vec[i64]` —
the typechecker's `infer_stats_call` intercept types the surface from the
argument's element (`sum`/`prod` → i64 CHECKED folds, `min`/`max` →
`Option[i64]`, `sort` → `Vec[i64]`, exact-i64 ordering above 2⁵³; float
statistics promote) and records the kind in a new `stats_elem_types`
side-table (typechecker → lowering → Program → codegen); the interpreter's
int-mode reads the static ARG type from `expr_types` (empty `Vec[i64]` gets
the INTEGER identities) and funnels through `reduce_i64`; the codegen paths
instantiate the shared emitters at i64 (`SortKey::IntValue`/`IndexIntoInt`).
S5 also FIXED the pre-existing silent miscompile where integer slices
bit-reinterpreted as f64 under `karac build` (B-2026-07-01 fixed-entry), made
narrower numeric elements a hard error (blocked on the interp width-laxity
class B-2026-07-01-3), and open-ledgered the Stats-args-move stdlib-signature
gap. **The kernel (S0–S5) is complete.**
Refactor byte-identical — codegen run-vs-build oracle 1945/0, par_codegen
127/0, interpreter 1056/0. Two layers, bottom-up: the
internal kernel (slices S0–S5) is the load-bearing refactor and is fully covered
by a byte-identical native oracle;
the surface traits (S6) sit on top — builtins *override* the generic default
methods with the fast kernel, user types get the fold-based defaults
monomorphized. Closes the two open `std.stats` long-tail items (**non-f64
element types** and **trait-dispatched Reduce/ElementwiseOrd unification**) from
[phase-11-stdlib-longtail.md](../implementation_checklist/phase-11-stdlib-longtail.md).
This file is the architecture of record; update its `Status:`
line (and the `docs/spikes/README.md` row) as slices land.

Cross-refs: [design.md](../design.md), the arm64 ASan aggregate-load fix baked
into `Stats.*` codegen ([`src/codegen/stats.rs`](../../src/codegen/stats.rs)),
and the codegen surfaces in
[phase-7-codegen.md](../implementation_checklist/phase-7-codegen.md).

---

## 1. The problem

Three families implement the *same* reduction over numeric container data, each
hardcoded to its own container shape and element assumptions:

| Family | Reduce loop | Element access | Elem types | Var form | Empty policy | min/max return |
|---|---|---|---|---|---|---|
| **Tensor** | `emit_scalar_reduce_loop` ([`tensor.rs:2843`](../../src/codegen/tensor.rs)) | flat contiguous data ptr | all numeric | — | trap | bare `T` |
| **Column** | per-method loops ([`column.rs:2168`](../../src/codegen/column.rs)) | Arrow bitmap-gated + valid-count | numeric | sample (÷n−1) | trap / ≥2 | bare `T` |
| **Stats** | `stats_fold`/`stats_minmax`/… ([`stats.rs`](../../src/codegen/stats.rs)) | `Slice[f64]` via spill-alloca scalar-GEP | **f64 only** | population (÷n) | −0.0/1.0/None/trap | `Option[f64]` |

Plus element-wise maps (Tensor binop/neg, Column binop/neg with
null-propagation; Stats has none) and ordering ops (min/max/median/percentile/
argmin/argmax/sort/argsort, each re-implementing a scratch sort + comparator).

The **only** things that genuinely differ across the three are: (a) how you read
`(len, element[i], is_valid[i])`, (b) the element kind, and (c) per-surface
semantic knobs (Bessel correction, empty policy, result wrapping). Everything
else is copy-paste. Each interpreter twin (`eval_stats_fn`,
`eval_tensor_reduce`, `eval_column_reduce`) duplicates the same split.

---

## 2. Layer 1 — internal kernel (S0–S5)

### 2.1 Descriptors (`src/codegen/kernel.rs`, new + interpreter twin)

- **`ContainerAccess`** — the one axis that differs across surfaces:
  - `FlatContiguous{ data, len }` — Tensor.
  - `ArrowNullable{ data, bitmap, len }` — Column; yields `is_valid[i]`.
  - `SlicePtr{ data, len }` — Stats/Slice. **Constructed via the spill-alloca
    scalar-GEP pattern so the arm64-Linux ASan aggregate-load-→-null bug fix
    (see `stats.rs` header comment) is inherited, not re-derived per call site.**
- **`ElemKind`** — LLVM type + signed/unsigned/float. Drives seed, accumulator
  type, and comparison predicate (`OGT`/`OLT` vs `SGT`/`SLT` vs `UGT`/`ULT`).
  **This is the axis that unlocks non-f64.**
- **`ReduceOp` / `MapOp` / `OrdOp`** — the operation plus per-surface knobs:
  `Var{ bessel: bool }`, `EmptyPolicy` (`Trap` / `Identity(-0.0|1.0)` / `None` /
  `RequireN(2)`), `ResultWrap` (`Bare` / `Option`).

### 2.2 Emitters

- `emit_reduce(access, kind, op) -> value` — one fold loop; seed / empty-guard /
  validity-gate / post-process (mean division, Bessel) all parameterized.
- `emit_elementwise_map(access, kind, op, other?) -> container` — unary + binary;
  null-propagation delegated to `access`.
- `emit_ord_op(access, kind, op)` + shared `emit_sort_scratch(access, kind)` —
  one comparator-parameterized scratch sort backing median/percentile/argmin/
  argmax/sorted/argsort and min/max ordering.
- Interpreter twin: single `reduce_over` / `map_over` / `ord_over` so all three
  eval paths funnel through one implementation.

### 2.3 Slices

| Slice | Scope | Notable |
|---|---|---|
| **S0** ✅ | Descriptors + interpreter twin. **Zero behavior change.** *(landed `bcaff37d`)* | Proved byte-identical: interpreter 1046/0, codegen E2E+oracle 1921/0. `ReduceOp` vocabulary + `reduce_f64` in `src/reduce_kernel.rs`; `Stats.*`/`Column` f64 reductions + shared min-max/`value_as_f64` funneled through it. |
| **S1** ✅ | Route Tensor `emit_scalar_reduce_loop`, Column sum/minmax, Stats fold/minmax/mean → `emit_reduce`. Preserve exact seeds, empty policy, return shape **per surface**. | **S1a (`73af27b0`):** `ContainerAccess` + `emit_reduce_fold`; Stats + Tensor `sum`/`prod`/`mean`. **S1b (`7adcc380`):** `emit_reduce_minmax`; Tensor + Stats `min`/`max`, axis-sum rerouted, `emit_scalar_reduce_loop` deleted. **S1c (`29b55062`):** `bitmap` axis + `*_gated` variants; Column `sum`/`min`/`max` migrated (oracle 1937/0, par 127/0). Column `mean` → S2. |
| **S2** ✅ | Fold the f64-accumulator family — Column `mean`/`var`/`std` (÷n−1) + Stats `variance`/`stddev` (÷n) — into a shared f64-sum-and-count emitter with a Bessel knob. | **Landed 2026-07-01.** `emit_sum_f64_and_count` (dense-or-gated `Σ x as f64` + count) + `emit_variance_from` (`mean` → `Σ(x−mean)²` → `count − (bessel?1:0)` divide) in `kernel.rs`; Column `mean`/`var`/`std` + Stats `variance`/`stddev` migrated, `column_sum_f64_and_count` + Stats' variance loop deleted, elements widen via shared `column_elem_to_f64`. Numbers unchanged — oracle **1943/0**, par 127/0. |
| **S3** ✅ | Unify ElementwiseMap: Tensor binop/neg + Column binop/neg (null-prop via access). Stats has none. | **Landed 2026-07-01 (`b0a40963` refactor + `eb21e300` neg fix).** `emit_elementwise_map` (`MapOther` second-operand axis, `MapKernelOp` op axis, gated = AND-of-bitmaps → dst bitmap + zero placeholder); Tensor `emit_tensor_binop_loop` now a thin adapter, Column's 3 loops deleted; interpreter twin `map_binop_slots` + `broadcast_pair` behind all four `eval_*_binop` paths. Probing **fixed 2 pre-existing neg divergences** — tensor `-0.0` (fsub→fneg, B-2026-07-01-1) and column `i64::MIN` silent wrap (ineg→checked `0−x`, B-2026-07-01-2) — and open-ledgered interp narrow-int width laxity (B-2026-07-01-3). Oracle 1945/0, par 127/0, interp 1056/0. |
| **S4** ✅ | Unify ElementwiseOrd + `emit_sort_scratch`; route Stats median/percentile/argmin/argmax/sort/argsort + Tensor/Column min/max ordering. | **Landed 2026-07-01 (`2ff34611`).** `emit_sort_scratch` (`SortKey::Value` / `::IndexInto` stable argsort) + `emit_reduce_argminmax` in `kernel.rs`; `column_sort_f64_inplace` → adapter, `column_sorted_valid_f64`'s inline sort + `stats_argsort`'s keyed sort + `stats_argminmax`'s loop deleted (~400 lines). Tensor/Column min/max ordering was already S1b/c; the predicted Column `median`/`quantile` bonus had already landed pre-S4 (`column_sorted_valid_f64`) — S4 retired its duplicate sort instead. Oracle 1945/0, par 127/0. |
| **S5** ✅ | Non-f64 element kinds for Stats (`Slice[i64]`/`f32`/…). Thread `ElemKind` from typechecker binding annotation. | **Landed 2026-07-01.** Scoped to **i64** (+f64): f32/narrow ints stay hard errors until the interpreter evaluates them width-faithfully (B-2026-07-01-3) — pre-S5 they silently bit-reinterpreted to garbage under `karac build` (the fixed high-severity miscompile). Return rules as designed: `sum`/`prod`→T (checked folds), `min`/`max`→`Option[T]`, `sort`→`Vec[T]`, float stats→f64. `infer_stats_call` + `stats_elem_types` plumbing + interpreter `reduce_i64` + codegen `SortKey::IntValue`/`IndexIntoInt`. Exact above 2⁵³ both surfaces. |

Each S1–S5 keeps a **native byte-identical oracle** (Slipstream-style) per
touched surface, and A/B across `run` / `KARAC_AUTO_PAR=0` / default auto-par.

---

## 3. Layer 2 — user-extensible surface traits (S6, gated sub-epic)

### 3.1 Target trait shapes (design sketch)

```kara
trait Reduce[T] {
    fn fold[A](ref self, init: A, f: fn(A, T) -> A) -> A;                    // the primitive
    fn sum(ref self) -> T where T: Add + Zero { self.fold(T::zero(), |a, x| a + x) }   // default
    fn product(ref self) -> T where T: Mul + One { ... }                    // default, overridable
    fn min(ref self) -> Option[T] where T: Ord { ... }
    fn max(ref self) -> Option[T] where T: Ord { ... }
}
trait ElementwiseMap[T] {
    fn map(ref self, f: fn(T) -> T) -> Self;                                // same-elem-type first cut
    fn zip_with(ref self, other: ref Self, f: fn(T, T) -> T) -> Self;
}
trait ElementwiseOrd[T: Ord] {
    fn argmin(ref self) -> Option[i64];
    fn argmax(ref self) -> Option[i64];
    fn sorted(ref self) -> Vec[T];
    fn argsort(ref self) -> Vec[i64];
}
```

### 3.2 Dispatch story (fits the existing static-mono model — no vtables)

- Typecheck resolves `x.sum()` → `Reduce::sum` for `x`'s type.
- If the resolved impl method is `#[compiler_builtin]` (Tensor/Column/Slice) →
  **codegen intercepts → kernel** (S0–S5). Fast path, unchanged behavior.
- Else (user type) → **monomorphize the default/user body** — the generic
  fold-based path. Slower but correct; consistent with Kāra's per-concrete-type
  monomorphization (no runtime trait-object ABI).

### 3.3 Prerequisite spikes (gate S6 — resolve before committing)

User-extensibility roughly doubles the surface-trait cost because it needs
language features the current trait system may lack. **S6-pre** must
confirm/build each:

1. **Default trait-method bodies** — stdlib traits today (`Ord`, `Add`) are
   signature-only (`;`); default bodies are likely a *new* feature.
2. **Generic methods inside traits** (`fold[A]`) + **`where` on trait methods**.
3. **`fn`-value params through monomorphized generic calls** — closures work
   (heap-env epic CLOSED), but must verify they thread through generic
   trait-method monomorphization.
4. **Blanket / over-container impls** (`impl[T] Reduce[T] for Vec[T]`) — Vec
   Hash+Eq are *implicitly admitted*, not real impls; real blanket impls may be
   new.
5. **Element-type-changing `map` deferred** — `Tensor[i64].map(fn(i64)->f64)`
   needs HKT-ish associated-type constructors. First cut restricts `map` to
   same element type (`fn(T)->T`); flag the limitation.

#### S6-pre findings (probed 2026-07-02, both `run` and `build` surfaces)

| # | Feature | `run` | `build` | Verdict |
|---|---|---|---|---|
| 1 | Default body as fallback (impl omits the method) | ✗ typecheck "no method" | ✗ same | **Missing feature.** Bodies PARSE (`TraitMethod.body: Option<Block>`) but no phase falls back to them. The S6b work item. |
| 1b | Impl *overrides* a default body | ✓ | ✓ | Works. |
| 2a | Trait-level generic `T` as method return (`impl Wrap[i64]`) | ✓ (after B-2026-07-02-10) | ✓ | Works. The `run` failure was NOT trait-related — builtin-name shadowing (`first` swallowed by the seq arm), fixed this slice. |
| 2b | Generic method inside a trait (`twice[A]`) | ✓ | ✗ loud ("no handler for method") | Codegen gap — impl-method monomorphization doesn't exist. S6b work item. |
| 2c | `where T: Display` on a trait method | ✓ | ✓ | Works. (`where Self: Sized` does NOT parse — SelfType rejected in where clauses; not needed by the S6 design.) |
| 3 | `Fn`-value params through generic monos | ✓ | ✓ (after B-2026-07-02-11) | **Was a silent miscompile** (`apply(20, |v| v*2+2)` returned 0); fixed this slice along with the whole Vec-param-in-mono surface (B-2026-07-02-11) and un-annotated closure param ABI (B-2026-07-02-12). |
| 4a | Blanket impl over a user generic struct (`impl[T] Total for Holder[T]`) | ✓ | ✓ | Works — pleasant surprise. |
| 4b | Blanket impl over builtin containers (`impl[T] Total for Vec[T]`) | ✗ runtime "type 'unknown'" | ✗ loud codegen reject | Missing feature (S6c). `karac check` ADMITS it — a check-passes/run-fails admission gap. |
| 5 | `T.zero()` assoc fn via a bound (`fn make[T: Zeroish]() -> T`) | ✓ | ✓ | Works — the `T::zero()` default-body pattern in the design sketch is viable today. |

**Net assessment:** the trait system is much further along than feared. The
real S6 feature work reduces to (i) default-body fallback dispatch (all three
phases), (ii) codegen monomorphization of impl/trait methods with their own
generic params, (iii) trait impls over builtin containers. `where` on methods,
blanket impls over user types, and assoc-fns-via-bounds already work. Four
pre-existing compiler bugs surfaced (and were fixed) by the probing —
B-2026-07-02-10..13, see the ledger.

### 3.4 S6 slices (after S6-pre)

- **S6a** ✅ **(landed 2026-07-02)** — the three traits are declared in the
  baked stdlib (`runtime/stdlib/reduce.kara` / `elementwise_map.kara` /
  `elementwise_ord.kara`, prelude-visible) and `Reduce[T]` is
  `#[compiler_builtin]`-implemented by `Column[T]` and `Tensor[T, ...S]` —
  bound-generic dispatch (`fn spread[C: Reduce[i64]](c: ref C)`) works
  end-to-end on `run` **and** `build` for both implementors, routing to the
  S0–S5 kernels; concrete-receiver dispatch is byte-unchanged (the impl
  bodies never run). **Shape divergences from the §3.1 sketch, on purpose:**
  `min`/`max` return `T` and trap on empty (the established Column/Tensor
  policy — invariant #1); `fold`/`product`/Option-forms wait for S6b
  (default bodies + generic trait methods). `ElementwiseMap`/`ElementwiseOrd`
  are **declaration-only**: no builtin has closure-taking `map`/`zip_with`
  or method-form `argmin`/`argsort` yet, and `Slice` is not a nominal impl
  target (the Vec 4b wall) — both are S6c. **Compiler work S6a forced:**
  (i) `ref Column/Tensor/DataFrame` params read their control pointer one
  deref short — B-2026-07-02-27, fixed via `get_data_ptr` in the three
  `*_ptr_for_var` helpers; (ii) generic monos leaked every non-tensor
  name-keyed var side-table across nested compiles (B-2026-07-02-11
  fallout; `SavedVarSideTables` now swaps all 17) and same-LLVM-shape
  handle instantiations (`Column[i64]` vs `Tensor[i64,[4]]`, both `ptr`)
  shared one mangled mono — `mono_handle_param_infos` +
  `collect_mono_handle_params` thread the arg spans'
  `column_typed_exprs`/`tensor_typed_exprs` records into a mangle axis
  (`$c_col_i64` / `$c_ten_i64_4`) and the mono prologue's registration;
  (iii) the typechecker never substituted a bound's trait args
  (`C: Reduce[i64]` typed `c.sum()` as raw `T`) —
  `trait_bound_arg_subs` + `dispatch_trait_assoc_fn`'s `trait_subs`
  param fix it. **Known residuals (deliberate):** `Vec[T]`-param monos
  still never bind `T` (two elem-type instantiations SHARE one mono —
  silent wrong values under `build`, probed `p8`; open-ledgered, S6b
  prerequisite since trait-method monomorphization needs TypeExpr-level
  substitution anyway); bound-arg satisfaction never compares trait args —
  `Column[f64]` where `C: Reduce[i64]` PASSES `karac check`, runs with
  silently wrong types, and only dies at codegen module verification
  (probed `p10`; open-ledgered, fix belongs with S6b's impl-matching);
  DataFrame values through bare-generic bounds don't register (no
  `dataframe_typed_exprs` table; loud fall-through).
- **S6b** — default method bodies + generic `fold` + `where`; enable a *user*
  `impl Reduce[T] for MyType` to monomorphize.
  - **S6b-1** ✅ **(landed 2026-07-03)** — the TypeExpr-level mono type-args
    prereq (the `Vec[T]` elem-collision, B-2026-07-02-41). `unify_types`
    gained an owned-to-`ref` coercion arm so a `ref Vec[T]` slot solves its
    element param from an owned `Vec[i64]` arg; the resolved per-call subst
    threads to codegen via `Program.call_type_subs` (resolved through the
    active `type_subst`, so nested calls flatten), plus a codegen-local
    container-element fallback (`vec_/slice_/set_/map_*` element tables) for
    the nested `T -> T` case the typechecker drops. Two element-type
    instantiations of `f[T](v: ref Vec[T])` are now distinct monos on both
    surfaces. Probing found one new open gap: a generic **by-value**
    `Slice[T]` param + Vec arg misses the Vec→Slice header coercion
    (ledgered; the non-generic and `mut Slice[T]` forms already coerce).
  - **S6b-2** ✅ **(landed 2026-07-03)** — default-body **fallback dispatch**.
    A pre-resolve desugar pass (`synthesize_trait_default_methods`,
    `src/desugar.rs`) copies each non-overridden trait default body into
    every `impl Tr for T` block, so all phases see it as an ordinary
    hand-written impl method (the one form already end-to-end). Two legs:
    **(a)** non-generic traits (B-2026-07-03-8, `6d488e58`); **(b)** generic
    traits (B-2026-07-03-10, this slice) — the trait's declared params zip
    positionally against `impl Tr[Args]`'s type-args and
    `substitute_trait_params_in_function` rewrites every trait-param mention
    in the copy's param/return types, `where` clause, own generic-param
    bounds, and body type-expressions (`T`-typed locals, casts, `T.assoc()`
    paths, `Fn(A, T)` closure types) to the concrete arg; a method's own
    generic params (`fold[A]`) shadow same-named trait params and are left
    untouched. Two distinct concrete args (`Chooser[i64]` / `Chooser[f64]`)
    of one generic trait now inherit distinct concrete defaults on both
    surfaces. Probing surfaced **two pre-existing, orthogonal** open gaps
    (ledgered, not introduced here): a generic **impl-method** codegen
    monomorphization gap — `o.apply[A](..)` on a concrete receiver runs but
    `build`-fails "no handler for method" (= S6b-3 / S6-pre finding 2b), and
    a broad `f().field` miscompile — immediate field access on any
    aggregate-returning call result reads 0 under `build` (bind the result
    first to work around).
  - **S6b-3** ✅ **(landed 2026-07-03)** — generic trait/impl **method**
    codegen monomorphization (B-2026-07-03-15). The declaration pass
    (`src/codegen.rs`) now registers a generic impl method into `generic_fns`
    keyed `Type.method` (via `make_impl_method_function`, which prepends
    `self` as an ordinary ref/owned param 0) instead of skipping it, and
    `compile_method_call` (`src/codegen/method_call.rs`) — after every
    builtin + the non-generic-method arm decline — routes such a call through
    `compile_generic_call` with the receiver prepended as arg 0, so the whole
    free-fn mono pipeline (infer type-args from the arg value types, mangle a
    per-instantiation symbol, declare+compile the mono, ref/owned arg ABI)
    applies unchanged. `self`'s concrete type contributes no type-param; the
    method's own params (`A`) bind from the by-value args. Covers scalar
    (`wrap[A]`, distinct i64/f64 monos), closure-param (`apply[A]` — the
    `fold` shape), explicit trait-impl (`dup[A]`), `self`-receiver, and
    fresh-temp receiver (`make_x().wrap(1)` — the fresh-temp materialization
    path's gate now also fires for generic methods, re-entering with the
    now-Identifier synth local). Residual: a **mut-self** generic method on a
    `self`/non-identifier receiver passes a copy (read-only self is correct —
    the Reduce case). The
    *real* stdlib `Reduce` fold-based defaults remain additionally gated on
    operator-on-bounded-`T` (closed by S6b-4a below) and the `f().field`
    miscompile (B-2026-07-03-16, fixed on main by `839beaea` as a dup of
    B-2026-07-03-3).
  - **S6b-4a** ✅ **(landed 2026-07-03)** — operator-on-bounded-`T`
    (B-2026-07-03-18). `a OP b` on a type parameter bounded by the stdlib
    operator trait for that operator (`+`→`Add`, `-`→`Sub`, `*`→`Mul`,
    `/`→`Div`, `%`→`Rem`, unary `-`→`Neg`) is now admitted with result type
    `T`, mirroring the existing `T: Numeric` arm (`infer_binary` /
    `check_unary` in `src/typechecker/expr_ops.rs`). Before the fix it
    hard-errored under `karac build` ("arithmetic operator requires numeric
    type, found 'T'") and only warned-then-ran under `karac run` — the
    run/build divergence blocking the fold defaults. Pure **typecheck
    admission**: user operator-trait impls are forbidden (resolver:
    stdlib-only), so every instantiation of such a `T` is a primitive numeric
    / `String` (`Add`) / distinct-numeric that codegen already lowers
    post-monomorphization (verified: the `T: Numeric` arm already built+ran).
    Two spellings handled — the operand is `Type::TypeParam` when a param
    (free-fn bound) but a bare `Type::Named { "T" }` when a `-> T` method
    result or `let x: T` local inside a **generic-trait default body** (the
    Named-vs-TypeParam trap); `type_param_has_trait_bound` consults
    `enclosing_bounds` (the authoritative in-scope type-param set) to accept
    both. The wrong-trait (`-` under `T: Add`) and unbounded-`T` cases stay
    rejected. Tests: typechecker `operator_on_operator_trait_bounded_type_
    param` + `operator_on_wrong_or_missing_trait_bound_rejected`, codegen e2e
    `e2e_operator_on_operator_trait_bounded_type_param` (i64/f64 distinct
    monos, `String` concat, unary `Neg`, generic-trait default `+`/`*`).
  - **S6b-4b** ✅ **(landed 2026-07-03)** — baked stdlib trait defaults
    reachable by the splice pass (B-2026-07-03-19). `synthesize_trait_default_
    methods` (`src/desugar.rs`) now collects default-bodied methods from
    `crate::prelude::STDLIB_PROGRAMS` as well as the user program (via the
    extracted `collect_trait_defaults_from_items`, user-first so a same-named
    user trait shadows), and clears `stdlib_origin` on each spliced copy so it
    is compiled as ordinary user code (the never-checked baked impl bodies are
    otherwise skipped). `Reduce[T]` gains its first DEFAULT method — `fn
    range(ref self) -> T { self.max() - self.min() }` — so a **concrete** user
    `impl Reduce[i64]/[f64] for MyType` inherits `.range()` without a body; the
    concrete `T` substitutes in, lowering `self.max() - self.min()` as native
    `T - T` (A/B-verified run == KARAC_AUTO_PAR=0 == build, i64→6 / f64→7.5).
    Only `Reduce.range` has a stdlib default body, so the change touches only
    user `Reduce` impls. Test: codegen e2e `test_e2e_stdlib_reduce_default_
    method_inherited_by_user_impl`. **Residual:** `fold`/`product`/`Option`-forms
    still need a `fold[A]` primitive on the trait (would change the
    required-method set the builtin impls satisfy) — a further slice.
  - **S6b-4d** ✅ **(landed 2026-07-03)** — builtin `Column`/`Tensor` inherit
    the `Reduce.range` default. The splice only rewrites user-program impls
    (their `#[compiler_builtin]` impls live in `STDLIB_PROGRAMS`, and the
    receiver is type-erased so a generic default-dispatch would hit the "type
    'unknown'" wall — that GENERAL mechanism stays S6c), so `range` is added
    directly to the Column/Tensor reduction dispatch (interp + codegen, all 4
    sites) as `max - min` reusing the existing min/max kernels — trapping on an
    empty / all-null input exactly as `min`/`max` do. `c.range()` / `t.range()`
    now work on run == build for i64 and f64. Test: codegen e2e
    `test_e2e_builtin_column_tensor_range`, interpreter
    `builtin_column_tensor_range_default_method`.
  - **S6b-4c** ✅ **(landed 2026-07-03)** — bounded-generic-impl method
    resolution (B-2026-07-03-20). A bound on a generic impl's OWN type param
    (`impl[T: Sub] Pair[T]`) made `self.m()` inside the impl — and an external
    `p.m()` — unresolvable under `build` ("no method 'm' on type 'Pair'"; ran
    fine). `impl_bounds_discharge` (`src/typechecker/env.rs`) dropped the
    candidate whenever it couldn't PROVE the impl's bound: `self`'s type is the
    bare target name (no args, so the bound had no arg to substitute), and a
    receiver typed `Pair[T]` substitutes to a `Type::TypeParam` (not in the
    impl table). Both are UNDECIDABLE, not FALSE — the fix makes discharge
    permissive for a missing/type-variable substitution (the concrete
    instantiation checks the bound), for both the inline-bound and where-clause
    arms. This DIRECTLY unblocks **generic** user impls of `Reduce`
    (`impl[T: Sub] Reduce[T] for Pair[T]` now inherits + resolves the spliced
    `range` default — i64 **and now f64** on run == build). The NON-i64 arm
    was a **separate, pre-existing, general** miscompile (B-2026-07-03-23) —
    fixed in the same session (below). Tests: typechecker
    `bounded_generic_impl_methods_resolve`, codegen e2e
    `test_e2e_bounded_generic_impl_method_call` (i64).
  - **B-2026-07-03-23** ✅ **(landed 2026-07-03)** — generic-struct element
    monomorphization. A generic struct with an inline type-param field
    (`Box[T] { v: T }`, even unbounded / no impl) lost its `[f64]` arg, so
    codegen defaulted the element to i64 and read non-i64 fields as i64
    (silent garbage under `build`, correct under `run`; a by-value method
    hard-crashed the build). Fixed in four layers: (1) the typechecker infers
    a struct literal's generic args (`Box{v:2.5}` → `Box[f64]`,
    `infer_struct_literal`); (2) field access substitutes the receiver's args
    into the field type (`infer_field_access`); (3) codegen builds a
    per-instantiation LLVM struct type (`Box[f64]` → `{double}`) at
    construction / field access / store / the function ABI (`mono_struct_type`
    wired into `llvm_type_for_type_expr`); (4) methods on a generic struct
    monomorphize by the receiver's instantiation (register into `generic_fns`,
    dispatch through `compile_generic_call` binding the impl's `T` from the
    receiver's recorded instantiation). This is what makes the **f64** arm of
    generic `Reduce` impls (`Pair[f64].range()`) work. Covers f64 / i64 /
    String elements, field store, two-field arithmetic, function ABI, `Vec` of
    a generic struct, and ref-self / by-value-self / bounded-impl methods.
    Tests: codegen e2e `test_e2e_generic_struct_field_monomorphizes_by_element`
    + `test_e2e_generic_struct_method_monomorphizes_by_receiver`.
  - **S6c-1 (`Column.fold`)** ✅ **(landed 2026-07-03)** — the general
    left-fold primitive on `Column[T]` that the fixed reductions
    (`sum`/`prod`/`min`/`max`) specialize: `col.fold(0, |a, x| a + x)` is
    `sum`, `col.fold(0, |a, x| if x > 2 { a + 1 } else { a })` counts, etc.
    Threads `init: A` through `f(acc, elem)` over the valid slots (nulls
    skipped, in order); an empty / all-null column returns `init` unchanged
    (the fold identity — NO empty trap, unlike `sum`/`min`/`max`). Declared
    `#[compiler_builtin] fn fold[A](ref self, init: A, f: Fn(A, T) -> A) -> A`
    in the inherent `impl[T] Column[T]`; typed by a `fold` intercept
    (`expr_method_call.rs`) that binds `A` from `init` and the element `T`
    from the receiver, then `check_expr`s the closure against `Fn(A, T) -> A`
    (closure-param pushdown, mirroring `Iterator.fold`) — baked generic
    dispatch can't bind `A` from an argument. Interp threads the accumulator
    through `invoke_function_value` (any `A`/`T`); codegen
    (`compile_column_fold`) **inlines** the closure body into an in-place
    reduction loop (compiled in the current fn with `acc`/`elem` bound as
    locals, shadowed outer bindings saved/restored) — sidestepping the
    closure-value ABI, captures resolving through the enclosing scope. First
    native (`karac build`) cut is POD-only and inline-literal-only: a
    closure-valued local / named fn, a heap element (`Column[String]`), or a
    heap / aggregate accumulator (`String`/`Vec`) is rejected **loudly**
    (each works under `karac run`) — never a silent miscompile. A/B verified
    run == KARAC_AUTO_PAR=0 == build. Tests: codegen e2e `test_e2e_column_fold`
    {`,_with_nulls_skips_them`,`_rejects_noninline_and_heap_accumulator`};
    interpreter `column_fold_reduction`; typechecker
    `test_column_fold_result_type_is_accumulator` +
    `test_column_fold_wrong_arity_rejected`. **Residuals (follow-ons):**
    the non-inline-closure + heap-`A`/heap-`T` native paths; and `fold` on the
    `Reduce` *trait* (bound-generic dispatch — needs the primitive declared
    trait-side + a matching `Tensor` impl).
  - **S6c-1b (`Tensor.fold`)** ✅ **(landed 2026-07-03)** — parity with
    `Column.fold`, completing the primitive across both handle-backed builtin
    reducers. Same shape, with two divergences: a tensor has NO null concept,
    so **every** element folds (a 2-D tensor folds all cells in C order — no
    bitmap gate), and an empty tensor returns `init` (the loop just doesn't
    run). The typechecker `fold` intercept is generalized to extract the
    element from either `Column[T]` (sole arg) or `Tensor[T, ...S]` (leading
    arg), error messages naming the container. Codegen `compile_tensor_fold`
    mirrors `compile_column_fold` minus the validity gate; same POD-only +
    inline-literal first-cut boundaries (loud reject). Tests: codegen e2e
    `test_e2e_tensor_fold`{`,_rejects_noninline_and_heap_accumulator`};
    interpreter `tensor_fold_reduction`; typechecker
    `test_tensor_fold_result_type_is_accumulator` +
    `test_tensor_fold_wrong_arity_rejected`.
  - **S6c-2 (`Column.map` / `Tensor.map`)** ✅ **(landed 2026-07-03)** — the
    `ElementwiseMap` trait's `map(|x| ...) -> Self` closure surface on both
    handle-backed containers, the element-wise-map sibling of the fold
    primitive. **Shared kernel:** a `MapKernelOp::Closure { params, body }`
    variant of `emit_elementwise_map` binds the single closure param to each
    element and inlines the body at the compute point (save/restore shadowed
    outer bindings, captures through the enclosing scope) — reusing the
    existing loop + validity-bitmap + null-skip machinery, so `Column.map`
    preserves nulls for free (the result carries the source validity bitmap).
    Column codegen `compile_column_map` (`column_alloc` + gated map), Tensor
    codegen `compile_tensor_map` (`tensor_alloc_runtime` + copy dims + dense
    map); same POD-only + inline-literal first cut as fold (closure-valued
    local / `Column[String]` rejected loudly, each works under `karac run`).
    The typechecker `map` intercept types `Column[T]`/`Tensor[T,...S]`
    `.map(Fn(T)->T)` → `Self` (same-element-type first cut), and because the
    result IS `Self` the fresh container auto-registers as a column/tensor
    binding — its `expr_types` entry flows into
    `column_typed_exprs`/`tensor_typed_exprs` and the let-binding gets
    scope-exit cleanup with no new plumbing (LSan-verified via
    `asan_column_tensor_map_freed_no_leak`). Interpreter map is
    type-agnostic (nulls preserved for Column). A/B verified run ==
    KARAC_AUTO_PAR=0 == build. Tests: codegen e2e `test_e2e_column_map`
    {`,_preserves_nulls`,`_rejects_noninline_and_string`} + `test_e2e_tensor_map`
    {`,_rejects_noninline`}; interpreter `column_map_reduction` /
    `tensor_map_reduction`; typechecker `test_column_map_result_type_is_self` /
    `test_tensor_map_result_type_is_self` (+ wrong-arity). **Residuals
    (follow-ons):** element-type-changing `map` (`Fn(T)->U`, needs the
    associated-type constructor — §3.3 item 5); the non-inline /
    heap-element native paths; and `map` on the `ElementwiseMap` *trait*
    (bound-generic dispatch).
  - **S6c-2b (`Column.zip_with` / `Tensor.zip_with`)** ✅ **(landed
    2026-07-03)** — `ElementwiseMap`'s BINARY form `zip_with(other: ref Self,
    f: Fn(T, T) -> T) -> Self`: element-wise combine of two same-shape
    containers through the closure. Extends the just-landed map kernel with a
    `(MapKernelOp::Closure, MapOther::Access)` arm binding TWO closure params
    (this element + the other container's element at the same index) — reusing
    the gated-map loop, so Column ANDs the two validity bitmaps (a null on
    either side → null result, closure not called there) and Tensor runs a
    runtime shape-equality guard (no bitmap). Codegen `compile_column_zip_with`
    (`column_operand` for the other operand + `column_alloc`) /
    `compile_tensor_zip_with` (`compile_expr` + `emit_tensor_shape_eq_guard` +
    `tensor_alloc_runtime`); same POD + inline-literal first cut (non-inline
    closure / heap element rejected loudly). Declared in the inherent
    Column/Tensor impls with `other: ref Self` — the `ref` makes the ownership
    checker treat the operand as a BORROW (READ, not consumed), so an operand
    may be reused after `zip_with` (without the decl the arg defaulted to a
    consume → spurious use-after-move; `map`'s single-`self` shape never hit
    it). Typechecker `zip_with` intercept checks `other` assignable to `Self`
    and the closure `Fn(T,T)->T`; returns `Self`. A/B verified run ==
    KARAC_AUTO_PAR=0 == build. Tests: codegen e2e
    `test_e2e_{column,tensor}_zip_with`{`,_propagates_nulls`,`_rejects_noninline`};
    interpreter `column_tensor_zip_with_reduction`; typechecker
    `test_column_zip_with_returns_self_and_checks_other` +
    `test_tensor_zip_with_wrong_arity_rejected`. **Residuals:** the non-inline /
    heap-element native paths; `zip_with` on the *trait* (bound-generic).
- **S6c-3** ✅ **(landed)** — `ElementwiseOrd` builtin method surfaces
  `argmin()` / `argmax()` on `Column[T]` and `Tensor[T, ...S]` → `Option[i64]`
  (regardless of `T`): the index of the **first** minimum / maximum, `None` on
  an empty / all-null receiver. Column reports the **original** slot index over
  the valid slots (nulls skipped in the compare — `Series.idxmin` semantics);
  Tensor the flat C-order index over all elements. Codegen adds a
  validity-gated `emit_reduce_argminmax_gated` (`(seeded, best)` → the caller
  wraps `Some`/`None` on `seeded`) — the gated sibling of the dense
  `emit_reduce_argminmax` (reused as-is for Tensor) and `emit_reduce_minmax_gated`;
  both wrap via the shared `build_option_some_via_phis`. Typed by an
  `argmin`/`argmax` intercept (like `map`/`fold`) with 0-arg + numeric-element
  diagnostics. `run` == `KARAC_AUTO_PAR=0` == `build` across ties (first
  occurrence), f64 elements, null-skipping, all-null/empty `None`, and inline
  `match` on the call result. Tests: codegen `test_e2e_{column,tensor}_argmin_argmax`;
  interpreter `column_tensor_argmin_argmax_reduction`; typechecker
  `test_column_tensor_argmin_argmax_result_type_is_option_i64` (+ wrong-arity /
  non-numeric); memory_sanitizer `asan_column_tensor_argmin_freed_no_leak`
  (owned-temp receiver free, LSan-clean).
- **S6c-4** ✅ **(landed)** — `ElementwiseOrd` `sorted()` → `Vec[T]` /
  `argsort()` → `Vec[i64]` on `Column[T]` and `Tensor[T, ...S]`. `Column`
  operates on the **valid** slots (nulls dropped, so the `sorted`/`argsort`
  result length is the valid count; `argsort` reports the ORIGINAL slot
  indices, `Series.argsort` semantics); `Tensor` over all elements in flat
  C-order. Ties are **stable** (first occurrence / ascending index order — the
  scratch sort's strict `>`). Codegen reuses the `Stats.sort`/`argsort`
  scratch-sort machinery (`emit_sort_scratch` + `stats_build_vec`, now
  `pub(super)`): `Tensor` calls `stats_sort`/`stats_argsort` on the dense
  C-order buffer directly; `Column` adds a `column_compact_valid` that gathers
  the valid values (or their original indices, for argsort) into a fresh
  8-byte buffer, then sorts (`argsort` keys on `data[idx]` via `IndexInto`).
  **First cut: i64/f64 elements under `build`** — the shared scratch sort
  moves 8-byte f64/i64 keys and compares int keys as *signed*, so a narrower /
  unsigned-64 / f32 element is rejected LOUDLY (each works under `karac run`;
  the interpreter is width-agnostic). Results are typed by the extended
  ordering intercept (`sorted` → `Vec[T]`, `argsort` → `Vec[i64]`). `run` ==
  `KARAC_AUTO_PAR=0` == `build` across ties, nulls (valid-only + original-slot
  argsort), f64, 2-D tensors, and the empty column. Tests: codegen
  `test_e2e_{column,tensor}_sorted_argsort` + `_narrow_width_rejected_loudly`;
  interpreter `column_tensor_sorted_argsort_reduction`; typechecker
  `test_column_tensor_sorted_argsort_result_types` (+ wrong-arity);
  memory_sanitizer `asan_column_tensor_sorted_argsort_freed_no_leak`.
  **Surfaced a pre-existing codegen bug** — B-2026-07-03-31: a fresh `Vec`
  temp returned by ANY builtin method (`iter_valid`/`sorted`/`Stats.sort`/…)
  and passed BY VALUE directly as a function argument corrupts the heap when
  the callee builds strings (silent wrong output / SIGBUS under `build`,
  correct under `run`; reproduces on main with pure `iter_valid`). Not this
  slice's logic — open-ledgered + spun off; the shipped tests use the
  `let`-bound `Stats.sort` idiom that sidesteps it.
- **S6c** — remaining: `ElementwiseOrd` user impls + `sorted`/`argsort` on
  non-i64/f64 element widths under `build` (interp already handles all);
  `ElementwiseMap` `zip_with`; blanket `Vec[T]` impls; user trait-impl methods
  over builtin containers (probed: interp "type 'unknown'", codegen loud
  fall-through).

---

## 4. Cross-cutting invariants (bake in, don't discover mid-slice)

1. **No number changes** — population vs sample variance, and all empty
   policies, stay op-level, never a global rule (breaks the oracle otherwise).
2. **Owned-temp free** (B-2026-06-29-1): unified paths keep
   `materialize_owned_temp` and `PrefixCollectionLiteral` in the gate.
3. **Type-erased Column** carries no elem tag in the pointer — non-f64
   `ElemKind` threads from the typechecker binding annotation.
4. Per slice: own worktree (`EnterWorktree`), commit proactively, LSan gate
   (`scripts/lsan-local.sh`), ff-merge to main + remove worktree, record any
   surfaced compiler bugs in [`docs/bug-ledger.jsonl`](../bug-ledger.jsonl).

---

## 5. Sequencing & risk

S0–S5 are low-risk, independently shippable, and each is oracle-verifiable — the
committed spine. **S6-pre is the real unknown**: if default-method-bodies +
generic trait methods turn out large, S6 splits off as a follow-on epic while
S0–S5 have already delivered the dedup and non-f64 stats.
