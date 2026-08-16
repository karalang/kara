# Spike: state decomposition — `Codegen` (439 fields) and `infer_method_call` (5.9k lines)

**Status:** 🚧 **IN PROGRESS — Phase 1 COMPLETE (`infer_method_call` 5,873 → 694 lines, −88%, 13 slices); Phase 2 fourteen clusters landed (`Codegen` 439 → 222 fields, −49%). Remaining: cluster 15 `VarTables` only — the sanctioned residual, or the last extraction.** This doc is the live coordination point for the refactor: the cluster map below is measured (not guessed), and the status table at the bottom is updated as slices land. **Other agents: read § Coordination protocol before editing `src/codegen.rs` or `src/typechecker/expr_method_call.rs`.**

*Origin: project-review item 6 ("schedule state-level decomposition of Codegen — not as a launch item, but before any MLIR/backend-swap ambition is taken seriously; same treatment for `infer_method_call`"). Owner decided 2026-08-15 to start now rather than defer, on the reasoning in § Why now.*

## The problem

Two god objects, both fully working and fully tested, both growing:

| Target | Size | Trend |
|---|---|---|
| `struct Codegen<'ctx>` (`src/codegen.rs:1113`) | **439 fields**, declaration spans 3,565 lines | 424 at review (~3 weeks ago) → 439 |
| `fn infer_method_call` (`src/typechecker/expr_method_call.rs:1011`) | **5,873-line body** in a 9,075-line file | ~3.6k at review → 5.9k |

Neither is a bug. Both are debt against three specific ambitions:

1. **Backend swap (MLIR).** [`design.md § Codegen architecture`](../design.md#codegen-architecture) commits to codegen *containment* — no `inkwell` type escapes `src/codegen.rs` — and calls a substrate swap "a contained surgery on one module." That boundary is real and enforced. But **inside** it there is no structure: 439 fields in one struct, every helper taking `&mut self`, 76 sibling `impl Codegen` modules all reaching into the same flat namespace. The surgery is contained to one module, and that module is a single organism — it cannot be ported subsystem by subsystem, because there are no subsystems.
2. **Self-hosting.** `selfhost/` must re-express this code in Kāra. Porting ten cohesive sub-structs is tractable; porting 439 tangled fields is not.
3. **Multi-agent bug fixing (the near-term payoff).** Most stdlib/typecheck fixes land somewhere in `expr_method_call.rs`'s 9k lines; most codegen fixes land in `codegen.rs`'s 12k. Two agents fixing unrelated bugs collide in one file today. Splitting by subsystem makes those diffs disjoint.

## Why now, and why not big-bang

**Why now:** the safety net is unusually strong, so the standard "large refactors are risky" prior is weaker here than almost anywhere. A decomposition slice is behaviour-neutral by construction, and this repo can *prove* neutrality the same day: ~3,200 codegen E2E tests asserting exact program output, `run` == `build` parity as a tested invariant, the ASAN/LSan fixture suite at two optimization levels on x86 **and** arm64, and the self-host differential oracle. Meanwhile the cost compounds — both targets grew measurably in three weeks, and every week adds more code written against the tangled shape.

**Why not big-bang:** a long-lived refactor branch over `codegen.rs` would be rebase hell and would *cause* the conflicts this is meant to remove — `main` in this repo routinely advances from sibling sessions mid-task. So:

> **Every slice is pure code motion, compiles green, passes the full `--features llvm` suite, and lands on `main` the same day. Nothing stays in flight overnight. No slice mixes a behaviour change with motion.**

## Measurement — the headline finding

Per-field usage was measured across all 77 codegen files (`self.<field>` occurrences; all 76 `src/codegen/*.rs` modules are sibling `impl Codegen` blocks, so this is the true subsystem-access map):

| Fields used in… | Count | Share |
|---|---|---|
| exactly 1 file | 143 | 33% |
| 2–3 files | 178 | 41% |
| 4–9 files | 86 | 20% |
| ≥10 files | 31 | 7% |
| 0 files (dead) | 1 | — |

**73% of the struct (321 of 439 fields) is touched by at most three files.** That is the finding that makes this tractable: the great majority of `Codegen` is not shared state at all — it is *private state of one subsystem, parked in a global struct*. Only 31 fields are genuinely broadly-shared, and 5 of those (`context`, `builder`, `module`, `current_fn`, `free_fn`) are the legitimate LLVM substrate that *should* stay common.

The zero-read field was `provider_lookup_result_ty`: computed and stored by the constructor but **never read back** through `self`. Re-verified and **deleted** in the cluster-3 slice (the constructor still builds the local, which the lookup fn's `fn_type` needs).

## Cluster map (measured, proposed)

Clusters are ordered by extraction risk, lowest first. "Files" = number of distinct codegen files touching any field in the cluster.

| # | Cluster | Fields | Files | Notes / risk |
|---|---|---|---|---|
| 1 | **`RuntimeFns`** — cached `FunctionValue` declarations (`karac_*_fn`, `malloc_fn`, `printf_fn`, …) | 67 | 37 | **Lowest risk in the whole refactor.** Pure declare-once lookup cache; no subsystem semantics. Broad file count but uniformly `self.x_fn` reads. Ideal first slice: 15% of the struct at near-zero risk. |
| 2 | **`TargetAbi`** — sret/niche/arm64-coercion/headerless layout | 18 | 8 | Tight, self-contained. |
| 3 | **`Provider`** — provider vtables, resource ids/traits | 7 | 3 | Nearly a private module already. |
| 4 | **`Tracing`** — spans, panic-site counters, error-trace strip | 5 | 5 | Small, isolated. |
| 5 | **`Display`** — baked `Display` type tables | 6 | 3 | Small, isolated. |
| 6 | **`BceOverflow`** — bounds-check elision, binsearch guards, overflow-check elision | 14 | 10 | Self-contained analysis caches. |
| 7 | **`Contracts`** — refinement/distinct/secret/invariant state | 12 | 12 | Self-contained. |
| 8 | **`GpuAccel`** — gpu buffers, tensor/column/dataframe infos, SoA layouts | 14 | 16 | Self-contained; touches the phase-10/11 surfaces. |
| 9 | **`MapSet`** — map/set/deque element + key type tables | 21 | 18 | Cohesive but wide. |
| 10 | **`Mono`** — type/const/layout substitutions, generic fn asts | 15 | 17 | Cohesive; interacts with type tables. |
| 11 | **`Concurrency`** — coro ctx, par counters, spawn sites, state machines, hot-swap | 24 | 17 | Cohesive; auto-par surface. |
| 12 | **`PatternMatch`** — the 12 `pattern_binding_*` fields + variant/branch state | 18 | 20 | Cohesive; `control_flow_match.rs` is the home. |
| 13 | **`DropRc`** — drop-fn caches, RC fallback, scope cleanup actions | 21 | 30 | ⚠️ **Highest-traffic cluster for bug fixes** — see Coordination protocol. |
| 14 | **`FnCtx`** — the current-function frame (`current_fn_*`, param/return modes, `loop_stack`, return retargets) | ~35 | wide | Conceptually the cleanest win (a real "frame" object) but touched everywhere. Do late. |
| 15 | **`VarTables`** — per-variable side tables (`variables`, `var_type_names`, `vec_elem_types`, inline/boxed payload vars, …) | ~90 | wide | **The hard tail.** Highest breadth (`variables` alone: 38 files). Do last, or consciously leave as the residual common state. |
| — | **`LlvmCore`** — `context`, `builder`, `module`, `current_fn`, `target_data`, debug info, symbol tables | 21 | 64 | **Does not move.** This is the legitimate shared substrate. |

Clusters 1–13 are ~228 fields (52% of the struct) at low-to-moderate risk. Clusters 14–15 are the genuinely hard 30%, and it is a legitimate outcome to stop after 13 and leave `VarTables`/`FnCtx` as the residual — that alone would take `Codegen` from 439 fields to ~145 and give every subsystem a named home.

Raw per-field data: regenerate with the script in § Reproducing the measurement.

## `infer_method_call` — structure and plan

The function is **not** a deep tangle; it is a **sequential chain of ~75 independent top-level guard blocks**, each of the shape "if the receiver type and method name match this family, handle it and return." Measured: 75 blocks opening at the function's top indentation level, and only **13 shared local bindings** across the whole 5,873-line body (`obj_ty`, `receiver_for_lookup`, `obj_ty_for_named`, `vec_elem_for_dispatch`, `str_routes_to_user_impl`, …), most computed late.

That is close to the best possible case for extraction. The plan:

1. Introduce a small `MethodCallCtx` struct holding the shared locals (built incrementally — early blocks need almost none).
2. Extract each guard block into `fn try_<family>_method(&mut self, ctx: &mut MethodCallCtx, …) -> Option<Result<Type>>` in a per-family module: `method_vec.rs`, `method_string.rs`, `method_map_set.rs`, `method_numeric.rs`, `method_iter.rs`, `method_tensor_column.rs`, `method_atomic.rs`, `method_gpu.rs`, `method_tuple.rs`, `method_user_impl.rs`.
3. The parent becomes a readable chain: `if let Some(r) = self.try_vec_method(&mut ctx, …) { return r; }`.

**Order is load-bearing** — the chain is a first-match-wins sequence, so extraction must preserve block order exactly. Each slice extracts one family and asserts no behaviour change via the full suite.

## Slice sequencing

### Phase 1 — `infer_method_call` (start now, runs parallel to bug fixing)

Lives in the **typechecker**; the open bugs are all **codegen**-surface, so the files are disjoint. One family per slice, ~10 slices.

### Phase 2 — `Codegen` clusters (queued behind the three open codegen bugs)

Clusters 1 → 13 in the order listed, one per slice. Start with `RuntimeFns` (67 fields, near-zero risk) to validate the pattern end-to-end before touching anything semantic.

### Per-slice verification bar (non-negotiable)

```bash
cargo fmt --all -- --check                          # hard gate, peer to clippy
cargo clippy --all --all-targets -- -D warnings
cargo test --features llvm                          # codegen E2E + memory_sanitizer
```

Plus: the slice diff must be **pure motion** (no logic edits, no opportunistic cleanups), and the commit message must say which cluster/family moved.

## Coordination protocol (for concurrent agents)

The refactor **always yields to bug fixes**; a fix never waits on the refactor.

1. **Before starting a Phase-2 slice**, check `grep '"status": "open"' docs/bug-ledger.jsonl` and recent `main` commits. If an open or in-flight bug touches the target cluster's fields, **pick a different cluster**.
2. **Live conflicts as of 2026-08-16 (evening):** none — the open-bug queue is empty. B-2026-08-15-6 / -7 (the RC/drop leaks that blocked cluster 13 `DropRc`) closed earlier; **B-2026-08-16-1** (the auto-par binding-visibility bug that blocked clusters 11 and 15) is **fixed in `d3afbdd8`** — the defect was a missing `PrefixCollectionLiteral` arm in `refs_in_expr`, not the binding tables themselves, but the deferral was still right: the debugging session *did* spend its time reading `refs`/slot flow, exactly the surface a rename would have churned. The one row still open, B-2026-08-15-30, is the arm64 `sort_by` re-measurement — it needs the M5 host and blocks no cluster (cluster 6 landed under it; see item 2's rule above). Clusters 11 `Concurrency` and 15 `VarTables` are open for the taking; re-check `grep '"status": "open"'` before starting either.

   **B-2026-08-15-30 no longer blocks cluster 6**, and the reasoning generalizes: it is a *perf re-measurement* (is the `sort_by` residual on Apple silicon materially bigger than the x86 number its `wontfix` was argued from?), not a defect in the BCE code. A cluster extraction is pure field regrouping — same fields, same borrow granularity, identical emitted IR — so it cannot move a benchmark by a single nanosecond, and a re-measure lands on `bce_state.rs` just as cleanly as it would have on `codegen.rs`. **The rule to apply is not "is a bug open near these fields" but "would the extraction disturb work someone is actually doing in them."** A correctness bug in the cluster's own logic blocks; a perf question answered by hyperfine does not.
3. **A slice is in flight for hours, not days.** If you find `codegen.rs` mid-migration and need to fix a bug in it, just fix the bug — the refactor rebases onto you, not the other way round.
4. **Update the status table below** when a slice lands, so the next session (or agent) knows what has moved.

## Reproducing the measurement

```python
# per-field usage across all codegen files
import re, os, collections
lines = open('src/codegen.rs').read().splitlines()
start = next(i for i,l in enumerate(lines) if l.startswith('pub(super) struct Codegen'))
end   = next(i for i in range(start+1, len(lines)) if lines[i] == '}')
fields = [m.group(1) for i in range(start+1, end)
          if (l := lines[i]).startswith('    ') and not l.startswith('     ')
          and (m := re.match(r'^(?:pub(?:\([a-z_]+\))?\s+)?([a-z_][a-z_0-9]*)\s*:', l.strip()))]
files = ['src/codegen.rs'] + sorted('src/codegen/'+f for f in os.listdir('src/codegen') if f.endswith('.rs'))
usage = {f: collections.Counter() for f in fields}
for fp in files:
    t = open(fp).read()
    for f in fields:
        n = len(re.findall(r'\bself\.'+f+r'\b', t))
        if n: usage[f][os.path.basename(fp)[:-3]] += n
```

## Status

| Phase | Slice | State | Landed |
|---|---|---|---|
| 1 | slice 1 — **scalar primitives** (`method_numeric.rs`): `abs`/`signum`/`sqrt`, the `float_math` table, bit-width converters + bit intrinsics, wrapping/saturating/checked/overflowing, `div_euclid`/`rem_euclid`, `pow`, `min`/`max`/`clamp`, `abs_diff`, rotates, `char` surface | **landed** — 768 lines moved; 5,873 → 5,132 | 2026-08-15 |
| 1 | slice 2 — **Vec/VecDeque mutation** (`method_vec_mutation.rs`): `push`, `insert`, `extend`/`extend_from_slice`, `pop`/`pop_back`/`pop_front`, `remove`/`swap_remove`, `get_unchecked`, `push_back`/`push_front` | **landed** — 470 lines moved; 5,132 → 4,668 | 2026-08-16 |
| 1 | slice 3 — **Option/Result combinators** (`method_optres_combinator.rs`): closure-free (`ok`/`err`/`or`/`and`/`ok_or`/`flatten`/`take`/`get_or_insert`) + closure-taking (`unwrap_or_else`/`map_or`/`map_or_else`/`map_err`/`and_then`/`or_else`/`filter`) | **landed** — 386 lines moved; 4,668 → 4,291 | 2026-08-16 |
| 1 | slice 4 — **Column/DataFrame analytics** (`method_column_analytics.rs`): `zip_with`, `argmin`/`argmax`/`sorted`/`argsort`, the `Column` scalar reductions, the `DataFrame` surface | **landed** — 359 lines moved; 4,291 → 3,942 | 2026-08-16 |
| 1 | slice 5 — **Column element-wise** (`method_column_elementwise.rs`): `iter`/`iter_valid`/`fillna`/`dropna`, `fold`, `map` (+ Option/Result and Tensor broadcast forms) | **landed** — 361 lines moved; 3,942 → 3,591 | 2026-08-16 |
| 1 | slice 6 — **user-`impl` dispatch tail** (`method_user_impl.rs`): impl-table lookup, candidate collection, bound gating, specialization, and the `no method 'X' on T` diagnostics | **landed** — 639 lines moved; 3,591 → 2,957 | 2026-08-16 |
| 1 | slice 7 — **identifier-receiver dispatch** (`method_identifier_receiver.rs`): the `ptr` provenance module, other prelude module paths, and type-receiver associated calls (`T.method(..)`, incl. `From` disambiguation) | **landed** — 428 lines moved; 2,957 → 2,537 | 2026-08-16 |
| 1 | slice 8 — **iterator / aggregation / atomics** (`method_iterator_agg.rs`): `iter`/`iter_mut`/`into_iter`, collection `clone`, `sum`/`product`/`max`/`min`, `join`/`concat`, the line iterators, and the atomic `load`/`store`/`compare_exchange`/`fetch_*` family | **landed** — 397 lines moved; 2,537 → 2,156 | 2026-08-16 |
| 1 | slice 9 — **sequence mutation + slice views** (`method_sequence_mutation.rs`): the comparator sorts, `retain`, `dedup`, `split_off`; plus `as_slice`/`as_slice_mut` and `as_ptr`/`as_mut_ptr` as a second entry point | **landed** — 332 lines moved; 2,156 → 1,838 | 2026-08-16 |
| 1 | slice 10 — **path + raw-pointer receivers** (`method_identifier_receiver.rs` 2nd entry point, `method_pointer.rs`): concrete-type UFCS `TypeName[Args].method(..)`, and the raw-pointer instance-method surface | **landed** — 309 lines moved; 1,838 → 1,542 | 2026-08-16 |
| 1 | slice 11 — **nominal-receiver tail** (`method_nominal_tail.rs`): distinct-type `.raw()` + no-deref rule, `cmp`, `to_string` (String / `Display` struct / all-unit `Display` enum), tuple receivers — the last built-in guards before user-`impl` dispatch | **landed** — 206 lines moved; 1,542 → 1,346 | 2026-08-16 |
| 1 | slice 12 — **fresh-temp receiver recording** (`method_temp_receiver.rs`) + **`Vector[T, N]` SIMD** (`method_simd.rs`): the span-keyed `temp_recv_*` side tables codegen reads to reconstruct a temporary receiver's shape, and the portable-SIMD instance surface | **landed** — 536 lines moved; 1,346 → 828 | 2026-08-16 |
| 1 | slice 13 — **named containers** (`method_mapset.rs`): `Map`/`Entry`/`SortedMap`/`SortedSet`/`Set` (type params thread through return types), plus the `Regex`, `CStr`/`CString` and HTTP named types | **landed** — 142 lines moved; 828 → 694 | 2026-08-16 |
| 1 | **remaining residual** — the receiver-normalization preamble (`obj_ty` → `receiver_for_lookup` / `obj_ty_for_named` / `vec_elem_for_dispatch`), 14 extracted-family call sites, and ~10 small blocks of 9–27 lines each (`spawn`/`join`, refinement, slice, `to_string`, fallible-alloc). No further clean family boundary — the residual is the function's own logic. | judgement call: stop here | — |
| 2 | cluster 1 **`RuntimeFns`** (`src/codegen/runtime_fns.rs`) | **landed** — 66 fields moved (not 67: `static_init_fn` is a *synthesized* fn, not a declare-once cache, so it stays); 338 call sites across 38 files rewritten to `self.runtime_fns.*`; `Codegen` 439 → 374 fields | 2026-08-16 |
| 2 | cluster 2 **`TargetAbi`** (`src/codegen/target_abi.rs`) | **landed** — 14 fields (target predicates, `#[repr(C)]` param/return adaptations, niche ABI, headerless layout); the three `current_fn_*` ABI fields correctly deferred to cluster 14 `FnCtx`; `Codegen` 374 → 361 fields | 2026-08-16 |
| 2 | cluster 3 **`Provider`** (`src/codegen/provider_state.rs`) | **landed** — 6 fields moved + the dead `provider_lookup_result_ty` deleted; `Codegen` 361 → 355 fields | 2026-08-16 |
| 2 | cluster 5 **`Display`** (`src/codegen/display.rs`) | **landed** — 8 fields (per-site payload types for Option/Result, tuple, Vec, Map, Set; baked-Display enums; emitted-fn cache) | 2026-08-16 |
| 2 | cluster 4 **`Tracing`** (`src/codegen/tracing.rs`) | **landed** — 4 fields (`current_span`, `panic_site_counter`, `strip_error_trace`, `runtime_panic_prefix_needed`); `Codegen` 355 → 345 fields | 2026-08-16 |
| 2 | cluster 7 **`Contracts`** (`src/codegen/contract_state.rs`) | **landed** — 12 fields (refinement/distinct bases + predicates, the per-function contract frame, `strip_contracts`, secret types) | 2026-08-16 |
| 2 | cluster 8 **`GpuAccel`** (`src/codegen/accel.rs`) | **landed** — 14 fields (SoA layouts + drop fns, GPU WGSL/buffers, tensor and column/DataFrame infos); `Codegen` 345 → 321 fields | 2026-08-16 |
| 2 | cluster 6 **`BceOverflow`** (`src/codegen/bce_state.rs`) | **landed** — 12 fields (the BCE fact stack + `len_alias`, the length-pin / descending / converging / interprocedural skip tables, the binsearch guard stack + assume flag, and the two overflow-elision armings). Taken while B-2026-08-15-30 is still open: that bug is a *perf re-measurement* (is the `sort_by` residual on arm64 bigger than the x86 number its `wontfix` was argued from?), and a field regrouping emits identical IR, so it cannot move a benchmark. 46 access sites — the smallest cluster in either phase; no `'ctx` (every field is plain analysis data). `Codegen` 255 → 244 fields | 2026-08-16 |
| 2 | cluster 9 **`MapSet`** (`src/codegen/mapset.rs`) | **landed** — 16 fields (per-variable Map K/V, Set elem, VecDeque head slots, temp-recv map/set types, map-lowering flags) | 2026-08-16 |
| 2 | cluster 10 **`Mono`** (`src/codegen/mono_state.rs`) | **landed** — 9 fields (generic fn ASTs, emitted-mono set, and the active type/const/layout substitution); `Codegen` 321 → 298 fields | 2026-08-16 |
| 2 | cluster 12 **`PatternMatch`** (`src/codegen/pattern_state.rs`) | **landed** — 17 fields (scrutinee classification flags, Option/Result payload slot+area, per-binding type/borrow-mode tables, variant payload bindings); `Codegen` 298 → 282 fields | 2026-08-16 |
| 2 | cluster 11 **`Concurrency`** (`src/codegen/conc_state.rs`) | **landed** — 23 fields (coro context + park/spawn plumbing, the network state-machine tables, spawn-site registry + detach flags, channel/task element types, the auto-par branch machinery — capture modes, analyzer decisions, branch struct/counter, cancel pointer — and the hot-swap tables). Taken the same evening B-2026-08-16-1 closed (`d3afbdd8`); the deferral was honored while the debugging session was live in these tables. `program_snapshot` and the two `atomic_*` sets deliberately stay behind: the first's consumer is `mono.rs`, the others are the RC-promotion surface, not concurrency lowering. 177 access sites; `Codegen` 244 → 222 fields (−23 moved, +1 for the `conc` handle, as with every cluster) | 2026-08-16 |
| 2 | cluster 13 **`DropRc`** (`src/codegen/drop_rc.rs`) | **landed** — 17 fields (scope cleanup stack, per-type drop/clone fn caches, RC/Arc fallback analysis + heap types, `rc_elide_ref_params`, owned-temp drops); unblocked once B-2026-08-15-6/-7 closed; 382 access sites — the widest slice yet; `Codegen` 282 → 266 fields | 2026-08-16 |
| 2 | cluster 14 **`FnCtx`** (`src/codegen/fn_ctx.rs`) | **landed** — 12 fields (fn name + param names, the whole return shape incl. the two ABI return values `target_abi` deferred here, `loop_stack`, caller loc, heap-closure spans). `current_fn` itself stays on `Codegen` per the LlvmCore call — 524 sites, and it is the substrate handle, not frame metadata; `Codegen` 266 → 255 fields | 2026-08-16 |
| 2 | cluster 15 `VarTables` — sub-slice A **`VarTypes`** (`src/codegen/var_types.rs`) | **landed** — 24 fields: the per-variable TYPE-shape tables (surface type names, Vec/Slice/Array/tuple element types in both LLVM and `TypeExpr` form, Option/Result payload instantiations, String/CStr/handle classifications, `binding_layouts`, and the three `pending_let_*` staging cells). 797 field-access sites — the widest slice in either phase. One new footgun found and handled: `tuple_var_elem_tes` is BOTH a field and a method (`call_dispatch.rs`), and the blanket rewrite caught the 3 method-call sites; disambiguated by the trailing `(` and restored. `variables` itself stays on `Codegen` — it is the frame's core, not a side table. `Codegen` 222 → 199 fields | 2026-08-16 |
| 2 | cluster 15 `VarTables` — sub-slice B **`ClosureState`** (`src/codegen/closure_state.rs`) | **landed** — 17 fields: closure fat-pointer fn types, the whole heap-env ownership surface (owner locals, the five `fns_returning_heap_env*` sets, the per-function owner maps `reject_heap_env_misuse` walks), currying, capture paths, the name counter, and the two `pending_closure_*` staging cells. 169 access sites; `Codegen` 199 → 183 fields | 2026-08-16 |
| 2 | cluster 15 `VarTables` — sub-slice C **`PayloadVars`** (`src/codegen/payload_vars.rs`) | **landed** — 18 fields: the inline Option/Result payload-var sets (+ map/agg variants), the boxed payload families (enum/struct/nested/struct-field), view vars (shared-enum, boxed-optres), `deboxed_payload_box_ptrs`, the passthrough-owner alias chain (plain/boxed/nested-boxed), plain-alias bases + generic params, `param_view_locals`. 147 access sites; `Codegen` 183 → 166 fields | 2026-08-16 |
| 2 | cluster 15 `VarTables` — sub-slice D **`FnSig`** (`src/codegen/fn_sig.rs`) | **landed** — 15 fields, keyed by function name: `fn_asts`, the param-mode tables (`ref`/`mut ref`/tensor/slice-elem), the return-shape tables (names, `TypeExpr`s, ref-return inner, Option-of-shared), `extern_link_names`, `callee_effectful`, `heuristic_inline_hints`, `fn_body_ident_mention_offsets`, `track_caller_fns`, `declare_only_fns`. 115 access sites; `Codegen` 166 → 152 fields | 2026-08-16 |
| 2 | cluster 15 `VarTables` — sub-slice E **`TypeDecls`** (`src/codegen/type_decls.rs`) | **landed** — 19 fields, keyed by TYPE name: struct/shared/union LLVM types + field name/type tables + generic params, enum layouts + unit variants + the per-instantiation pair, `Ord`-orderability, moved-field drop bodies, prelude shadowing. **1,299 access sites — the largest slice of the whole spike.** Two rewrite footguns handled: `struct_field_type_exprs` is also a method (3 call sites restored, same as `tuple_var_elem_tes`), and `synth_drop.rs` binds the receiver as `slf` in a closure — a third receiver spelling the rewriter didn't know (3 sites; parity re-verified over `self|cg|slf`). `Codegen` 152 → 134 fields | 2026-08-16 |
| 2 | cluster 15 `VarTables` — remainder | in progress — next: rc-elision fn-keyed set (7 fields, 17 sites, dry-run clean), then `variables` + residual singles | — |
