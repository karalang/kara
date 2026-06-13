# Weave — Data Pipeline with Verifiable Invariants

Dogfooding project from [`docs/dogfooding.md`](../../docs/dogfooding.md) § Weave
(Tier 2). It proves what no other example in the corpus exercises *together*:
**refinement types + contracts + effect inference**, on a real multi-stage CSV
ETL — the "correctness by construction" story.

## What it proves

A four-stage pipeline over an embedded CSV. Untyped text enters at the top; by
the time data reaches the later stages every invariant is a *type*, so those
stages never re-validate:

| Stage | Signature shape | Kāra feature exercised |
|---|---|---|
| `parse_row`  | `String -> Result[ValidatedRow, ParseError]` `with panics` `ensures …` | refinement `try_from` narrowing; `ensures` postcondition; inferred-effect declaration |
| `enrich_row` | `ValidatedRow -> EnrichedRow` `with reads(CurrencyDB)` `ensures result.qty == old(row.qty)` | effect resource + `with_provider` injection; `old(...)` pre-state capture on a consumed receiver |
| `aggregate`  | `NonEmpty[EnrichedRow] -> Summary` `requires …` `ensures …` | generic refinement as a precondition *type*; `requires`/`ensures` |
| `Summary`    | struct with `invariant self.row_count >= 0` | struct invariant checked at construction / pub-method exit |

Refinement types in play: `BoundedText` (length-bounded String), `PositivePrice`
(`f64 where self > 0.0`), `PositiveQty` (`i64 where self > 0`), and the generic
`NonEmpty[T]`.

## Run it

```
karac run examples/weave/src/main.kara
```

Expected output (3 valid rows enriched at 1 EUR = 1.08 USD; 3 rejected, one per
failure class):

```
  ok    alice@example.com  usd=13.5  x3
  ok    bob@example.com  usd=4.32  x10
  ok    carol@example.com  usd=10.789200000000001  x1
  skip  email is empty
  skip  email 'bad-no-at' has no '@'
  skip  price -2 is not > 0
summary: 3 rows, usd_total=94.48920000000001
```

Runs via the tree-walk interpreter (`karac run`). `String.split` now also has a
codegen path (GAP-W2); a full `karac build` of Weave additionally depends on
unrelated codegen fixes for struct-variant-enum `impl Display` (others' area).

## Dogfooding findings

Building Weave surfaced seven findings (the dogfood's load-bearing job, per
`docs/dogfooding.md` § "The dogfooding purpose is load-bearing"). Full detail in
the header of [`src/main.kara`](src/main.kara); summary:

| ID | Finding | Status |
|---|---|---|
| GAP-W1 | The canonical `ValidEmail = String where self.contains("@")` is **inexpressible** in v1 — the refinement constraint language admits only pure *zero-arg* methods; `contains` takes an argument. Structural "@"-check moved into `parse_row`'s body. | By design; roster sketch corrected |
| GAP-W2 | `String.split` was unimplemented (`word_count.kara` assumed it). | **Fixed** — interpreter + typechecker + codegen (codegen via the `karac_runtime_string_split` out-param helper, native) |
| GAP-W3 | `karac run <file>` is single-file only — it never loads sibling `src/*.kara` modules into the interpreter, so a multi-module layout fails at runtime despite resolving + typechecking. | Tracked; example kept single-file |
| GAP-W4 | User types could not `impl Display` in v1 (operator traits were stdlib-only). | **Fixed** — gate lifted; `ParseError` now has a real `impl Display { fn to_string }` and `f"{err}"` dispatches to it (interpreter + codegen). Surfaced + fixed a pre-existing pattern-binding shadowing bug |
| GAP-W5 | The roster sketch labels `parse_row` "pure", but effect inference shows it carries `panics` (from indexing). Honest signature: `with panics`. | By design; roster note added |
| GAP-W6 | The missing-effect diagnostic suggested an **un-parseable** fix (`Add: allocates(Heap), panics()` — comma-separated + empty-parens + undeclarable `Heap`), and `allocates(Heap)` was a three-way knot (required-when-undeclared vs default-permitted vs unwritable). | **Fixed** — fix-it now emits a valid `with` clause, and the substrate `allocates(Heap)` is exempt from the must-declare set per design.md § Effect Substrate (an allocating pub fn needs no `with` clause) |

GAP-W2/W4/W6 fixes ship with regression tests:
`tests/interpreter.rs::{test_string_split_interpreter,
test_user_impl_display_dispatches_through_to_string,
test_tuple_variant_binding_shadows_unit_variant_local}`,
`tests/codegen.rs::e2e_user_impl_display_dispatches_to_to_string`,
`tests/resolver.rs::test_user_impl_display_for_struct_and_enum_allowed`,
`tests/effectchecker.rs::{test_missing_effect_fixit_*,
test_pub_fn_allocating_only_needs_no_declaration,
test_*_companion_infers_allocates_but_does_not_require_declaration}`.
