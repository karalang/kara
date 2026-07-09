# Caller-retains parameter model — making the consume/escape decision explicit

**Status:** ✅ **COMPLETE (2026-07-04).** All phases delivered; both target bugs and every
residual split off from them are fixed, and the once-`#[ignore]`d leak trackers are
un-ignored and green on the authoritative Linux-LSan gate. The consumption classifier this
spike scoped now ships as `src/codegen/consume_class.rs` and was **lifted into the
consolidated ownership judgment** as the load-bearing rule §4 (`Escape` / `NonConsuming`) of
[`ownership-drop-judgment.md`](ownership-drop-judgment.md); the corpus-wide parity guarantee
Phase 0 wanted is delivered in a stronger form by the executable oracle + drop-differential of
[`ownership-model-mechanization.md`](ownership-model-mechanization.md) (S3/S4). This file is
retained as the design reference (linked from `consume_class.rs` and the bug ledger). See
**Outcome** below for the plan-vs-reality map. The rest of the document is the original
pre-implementation design, kept verbatim.

## Outcome — what shipped (and where the plan bent)

| Plan | Reality | Evidence |
|---|---|---|
| Phase 0 — `classify_use_site` + shadow `debug_assert!` at every suppressor, ship inert | Classifier shipped as `consume_class.rs` (`binding_only_borrowed` / `classify_binding_in_expr`), **conservative-by-default** (any unknown/transferring position ⇒ `Consumed`). Because a wrong answer can only *keep* today's suppress behavior — never drop a needed suppression, so never a double-free — it was safe to wire it to **gate** the B-31 site directly in Phase 1; no separate inert shadow ship was needed. The broader "parity at *every* suppressor" goal was met in a **stronger** form by the sibling spike: an executable ownership oracle (`src/ownership_oracle.rs`) implementing the same `Escape`/`NonConsuming` `classify`, plus a codegen drop-differential (`src/drop_differential.rs`, `tests/drop_differential.rs`) that checks **100% of the corpus at 0 divergences** — a whole-corpus check that subsumes per-suppressor debug-asserts. | `src/codegen/consume_class.rs`; `ownership-drop-judgment.md` §4; `ownership-model-mechanization.md` S3/S4 |
| Phase 1 — gate `suppress_inline_option_agg_payload_cleanup` on `Escape` (B-2026-07-03-31) | Delivered. `arm_only_borrows_option_agg_payload` / `block_only_borrows_option_agg_payload` (`control_flow_match.rs`) route the classifier over the arm body + guard for `match` / if-let / while-let; `let else` stays unconditional (its bindings escape the analyzable scope). | fix `80229526`; test `asan_b31_option_agg_payload_borrow_only_no_leak` |
| Phase 2 — `field_copy_supported` admits `Option[shared]` + rc-inc/rc-dec legs + escape rc-transfer (B-2026-07-03-28) | Delivered, but **without** the originally-scoped element-deep piece (b): the pinned consume path self-balances. `field_copy_supported` admits `Option[shared]`; `deep_copy_option_inline_payload_in_place` rc-INCs the inline box on `Some`; `track_struct_var` registers a combined value-drop + shared-field rc-dec for any shared-owning struct. | fix `7f727aaa`; tests `asan_b28_option_shared_and_direct_shared_struct_drop_no_leak`, un-ignored `asan_attr_node_list_drop_consume_and_plain` (240 B/6 → 0) |
| — (residual, not in the original plan) | **B-2026-07-04-7** — the escape/return sibling of B-31: three coupled halves (copy-depth == drop-depth), no model rewrite. `field_copy_supported`'s Option arm admits a non-shared struct/enum payload; `deep_copy_option_struct_enum_payload_in_place` is the box-aware copy peer of `emit_option_drop_fn`; the destructure-leaf zeros the source's Option tag on move-out. | fix `e56cc298`; test `asan_b04_7_option_heap_enum_struct_field_drop_no_leak` |
| — (residual) | **B-2026-07-04-9(a,b)** — the rc-balance / element-deep residuals. (b): the fresh-temp struct-arg gate registers the combined drop for a shared-owning struct even when not copy-supported (`f22b58c4`). (a): `deep_copy_vec_aggregate_elements_in_place` + a `vec_elem_agg_drop_for_type_expr` destructure-leaf drain, landed together (copy-depth == drop-depth). **The original "needs caller-retains model completion" diagnosis was wrong** — it was a contained copy/drop depth asymmetry, not the for-loop-consume suppressor / a missing `binding_only_borrowed` migration. | fixes `f22b58c4` (b), `273c9397` (a); test `asan_b04_9a_vec_struct_field_entrycopy_wholedrop_no_double_free` |
| — (residual) | **B-2026-07-04-17** — pre-existing for-loop-element-move-to-new-owner double-free (independent of the Option class). | fix `278e1a91` |

**Two honest deviations from the plan.** (1) The classifier is wired to gate **only** the B-31
suppressor family — the rest of "the current suppressor scatter" below was *not* migrated to it;
the residuals were closed by copy-depth-vs-drop-depth fixes in `param_own.rs` / `synth_drop.rs`,
not by routing every suppressor through the classifier. The corpus-parity ambition instead
became the oracle/differential. (2) Phase 2's element-deep entry-copy "piece (b)" was found
**not** needed for the port shape (the consume path self-balances) and shipped only later, as
B-04-9(a), for the whole-drop case — see that entry for the false-start it corrected.

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
