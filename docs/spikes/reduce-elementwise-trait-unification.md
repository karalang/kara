# Design spike — trait-dispatched Reduce / ElementwiseMap / ElementwiseOrd unification

**Status:** 🟡 **S0–S5 + S6-pre COMPLETE (S0–S1 2026-06-30 `bcaff37d`, `73af27b0`,
`7adcc380`, `29b55062`; S2–S5 2026-07-01, S3 `b0a40963`+`eb21e300`, S4 `2ff34611`;
S6-pre probe matrix 2026-07-02 — see §3.3, which also surfaced + fixed
B-2026-07-02-10..13); S6a–S6c open.** Unifies the three copy-pasted
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

- **S6a** — declare the three traits in stdlib; Tensor/Column/Slice
  `#[compiler_builtin]`-impl them (routing to kernels). Compiler-internal
  dispatch end-to-end first.
- **S6b** — default method bodies + generic `fold` + `where`; enable a *user*
  `impl Reduce[T] for MyType` to monomorphize.
- **S6c** — `ElementwiseMap` / `ElementwiseOrd` user impls; blanket `Vec[T]` impls.

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
