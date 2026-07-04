# Caller-retains parameter model — making the consume/escape decision explicit

**Status:** design / scope (not yet implemented). Foundation for B-2026-07-03-28 and
B-2026-07-03-31, both `#[ignore]`d leak-only bugs blocked on this model.

## Summary

Codegen frees every owned heap allocation via the scope-drop of its owning binding,
*unless* that binding escapes the frame — in which case the source drop is disarmed and
ownership transfers. Under this compiler's convention, **passing a value to a user
function is not an escape**: the callee entry-deep-copies owned aggregate params
(`make_aggregate_param_callee_owned` → `deep_copy_struct_heap_fields_in_place`,
`param_own.rs`) or defensively copies owned `Vec`/`String` at its own consume sites
(`owned_vecstr_params` + `maybe_defensive_copy_param_arg`, `runtime.rs`). The callee frees
its copy; the caller keeps and frees the original.

The bug class exists because **there is no single predicate that answers "is this use-site
an escape (ownership transfer) or a non-consuming copy/borrow?"** Today that answer is
re-derived by a scatter of per-shape suppressors, each with local heuristics. Two of them
get it wrong on specific payload shapes, and both fixes need the same missing predicate.

## The invariant

> Every owned heap allocation is freed exactly once — by the scope-drop of the binding
> that owns it — unless that binding *genuinely escapes the frame*, in which case ownership
> transfers and the source's drop is disarmed (with a defensive copy or rc transfer where
> the source is caller-retained).

## The two bugs are one blocker

### B-2026-07-03-31 (leak on destructure-then-match-move)

```kara
enum Val { Nothing, Ident(String) }
struct A { value: Option[Val] }
fn ident_len(v: Val) -> i64 { match v { Val.Ident(s) => s.len(), Val.Nothing => 0 } }
fn use_a(a: A) -> i64 { let A { value } = a; match value { Some(v) => ident_len(v), None => 0 } }
```

`suppress_inline_option_agg_payload_cleanup` (`control_flow_match.rs`) fires whenever the
scrutinee is in `inline_option_agg_payload_vars`, the pattern is `Some(_)`, and the arm
"consumes the field." It tag-zeroes the source to disarm its scope drop — **correct only if
`v` escapes**. But `ident_len(v)` is a call to an owned param → entry-copied → *non-consuming*.
Disarming orphans `v`'s inner `String` → ~40 B/move leak. The suppressor answers "does the
arm consume?" with a shape heuristic instead of an escape check.

### B-2026-07-03-28 (Option[shared] / nested Vec[struct] rc-balance)

`field_copy_supported` (`param_own.rs:311–316`) accepts `Option[String]`/`Option[Vec]` but
returns `false` for `Option[shared]`, so a struct carrying it bails from *both* entry-copy
(no rc-inc) *and* drop synthesis (no rc-dec) → leak. Making it copy-supported (2026-07-03
attempt, patch `b28-residual-attempt.patch`) went ASAN-green but **LSan regressed**
(240 B/6 → 714 B/18): the deep entry-copy adds an rc-inc that the **move/consume path never
rc-decs** (source drop suppressed on move-out). That regression is exactly a missing
escape/consume decision — on a *move* (escape) the rc must transfer; on a *copy*
(non-consuming) it must not.

### Why one predicate

Misclassify an escape as non-escape → **leak** (LSan-catchable). Misclassify a non-escape
as escape → **double-free** (ASAN-catchable). Both bugs need the same primitive, and the
asymmetric stakes are why this is an ownership-model change, not a contained patch.

## The model: a use-site consumption classifier

```
classify_use_site(binding, use_context) -> Consumption   // { Escape, NonConsuming }
```

**Escape** (ownership leaves the frame; disarm source drop, transfer/rc-inc/defensive-copy):
- the function tail / `return` expr (or a place rooted at the returned binding); or
- an element/field of an aggregate or collection that *retains* it beyond the current
  statement — a **container mutator** (`push`/`insert`/`push_back`), a struct/enum/tuple/
  **collection literal** field, an index-store or field-store into an outliving place; or
- captured by an **escaping** closure (already computed by
  `reject_escaping_capturing_closure`, `closures.rs:128–329`).

**NonConsuming** (source retains; do not disarm its drop):
- an argument to a **user function/method whose param is owned** (callee entry-copies) or a
  **borrow** (`ref`/`mut ref`); or
- a borrowing read (field read, `.len()`, a method returning a borrow — several already
  recognized by `scrutinee_is_borrow_call`).

The distinction the current code blurs: **a user-fn owned param is entry-copied
(NonConsuming); a builtin container mutator / aggregate literal retains (Escape).** Both look
like "value flows into a call/constructor," which is why the syntactic per-site heuristics get
it wrong. The classifier keys on the callee's *retention behavior*, not the site shape. The
inputs it needs (param mode; builtin-retains-ness) already exist in the ownership-pass
signatures and the builtin dispatch tables.

## Delivery plan

### Phase 0 — foundation as a pure refactor (no behavior change)

Implement `classify_use_site` and route it in **shadow mode**: at each existing suppressor,
compute the classifier verdict and `debug_assert!` it agrees with the current heuristic. Run
the full codegen + ASAN + LSan suites. This proves the predicate reproduces today's behavior
everywhere **before flipping any decision** — the primary guard against the double-free edge.
Phase 0 ships inert.

### Phase 1 — B-31 (leak-only, contained)

Flip `suppress_inline_option_agg_payload_cleanup` (and its `boxed_enum_payload_vars` sibling
`suppress_boxed_payload_struct_destructure`) to suppress **only** on `Escape`. A
`Some(v) => f(v)` / `v.field` arm classifies NonConsuming → drop stays armed → leak closed.
Regression: the B-31 repro under ASAN + LSan.

### Phase 2 — B-28 (the rc-balance)

1. Extend `field_copy_supported` to accept `Option[shared]` (`param_own.rs:311–316`).
2. Add the rc-inc leg to `deep_copy_option_inline_payload_in_place` for a shared payload, and
   the rc-dec leg to struct-drop for `Option[shared]`, mirroring the direct-`shared` arm in
   `emit_nested_struct_shared_rc_decs_ex` (`synth_drop.rs`). Uphold **copy-depth == drop-depth**
   (`param_own.rs:385–407`) for the nested `Vec[struct]` case.
3. Route the move/consume path through the classifier: when a copy-supported shared-owning
   struct *escapes* (move-out), transfer the rc (suppress source rc-dec, or rc-inc destination)
   so copy-supported + consumed-via-move stays balanced — the hole the 2026-07-03 attempt hit.
   Regression: un-ignore `asan_attr_node_list_drop_consume_and_plain` (`memory_sanitizer.rs`),
   expect `240 B/6 → 0`.

## Concrete change sites

| Phase | File:symbol | Change |
|---|---|---|
| 0 | new `classify_use_site` (near the suppressors) | predicate + shadow `debug_assert!` at each suppressor |
| 1 | `control_flow_match.rs` `suppress_inline_option_agg_payload_cleanup` | gate on `Escape` |
| 1 | `control_flow_match.rs` `suppress_boxed_payload_struct_destructure` | gate on `Escape` |
| 2 | `param_own.rs:311–316` `field_copy_supported` | accept `Option[shared]` |
| 2 | `param_own.rs` `deep_copy_option_inline_payload_in_place` | add rc-inc for shared payload |
| 2 | `synth_drop.rs` `emit_nested_struct_shared_rc_decs_ex` | `Option[shared]` rc-dec for copy-supported structs |
| 2 | `call_dispatch.rs:3607` (shared move-out) | rc-transfer via classifier on escape |

## The current suppressor scatter (what the classifier unifies)

- `suppress_source_vec_cleanup_for_arg_ex` — 8 handlers (`call_dispatch.rs:3469–3661`):
  tuple-index, field-access, Vec/String ident, Tensor, Column, DataFrame, Map/Set, shared.
- `suppress_map_cleanup_for_tail_identifier` (`call_dispatch.rs:3082`) — scans all frames,
  drops the matching `FreeMapHandle`.
- `suppress_fstr_acc_if_moved_out` (`exprs.rs:2140`) — disarms the f-string accumulator.
- `suppress_inline_option_agg_payload_cleanup` (`control_flow_match.rs`) — inline Option
  agg payload; the B-31 site.
- `maybe_defensive_copy_param_arg` (`runtime.rs:2814`) — the copy-and-retain route, consults
  `owned_vecstr_params` + `for_loop_borrow_vars`.

State tables in play: `owned_vecstr_params`, `for_loop_borrow_vars`, `owned_struct_params`,
`inline_option_agg_payload_vars`, `boxed_enum_payload_vars`, `shared_types`, `var_type_names`,
`vec_elem_types`.

## Risk register

- **Double-free (misclassify → Escape).** Highest risk. Phase-0 parity assertions + macOS
  ASAN suite on every phase.
- **Residual leak (misclassify → NonConsuming).** Linux LSan gate (`scripts/lsan-local.sh`;
  confirm `Compiling karac` present + `passed+filtered == total`, per the stale-karac trap).
- **run==build parity.** Codegen-only leaks; `karac run` unaffected — but A/B every regression.
- **Blast radius.** The classifier touches every consume site; Phase-0 shadow mode is what
  makes that safe. Do not skip it.

## Recommendation

A real multi-file ownership project (Phase 0 is the bulk of the risk-reduction; Phases 1–2 are
then small and guarded), not a "take the next bug" slice. Both targets are leak-only, med
severity, `#[ignore]`d — nothing miscompiles or double-frees today, so it is discretionary.
Worth doing to make the ownership model explicit (it retires a *class* of payload-shape bugs,
not just these two). Recommended entry point: build Phase 0 and prove parity, then stop for
review before flipping any behavior.
