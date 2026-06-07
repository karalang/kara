# Design spike — general owned-temp tracking (codegen)

**Status:** scoping, 2026-06-06. **Not yet started.** This doc scopes the work;
no code has landed. It is the designated *unblocker* for the phase-6 line-489
remainder (scrutinee-temp drop scope) and the phase-6 line-497 tail-expr leak
carve-out — both of those are blocked on the gap described here.

**Doc footprint** (update these together — see memory `maintain-scope-doc-index`):

- this file — the scope + slice plan (entry point)
- `docs/design.md` § *Temporary Lifetime Rules* (lines ~2547–2586) — the
  authoritative spec; **already written**, this spike only implements it
- `docs/design.md` § *Scrutinee temporary scope* (~2496–2516) — the line-489
  consumer
- `docs/design.md` § *Drop ordering within a branch* / *Tail-expression
  temporary scope* (v60 item 28, ~9775–9823) — the line-497 consumer
- `docs/implementation_checklist/phase-6-runtime.md` line 489 (scrutinee scope)
  and line 497 (tail-expr temp leak carve-out) — both reference this gap

---

## 1. Problem

Every value produced by an expression that is **not** bound to a named slot is a
*temporary*. design.md § *Temporary Lifetime Rules* already pins, per position,
exactly when each temporary's heap storage must be dropped (full table in §3
below). Codegen does **not** implement that table generally: it tracks drops for
**named `let` bindings** (via the `track_*_var` family → `CleanupAction` on the
`scope_cleanup_actions` stack) plus exactly **two** narrow special cases for
unnamed temps. Every other heap-owning temporary **leaks**.

This is the single prerequisite that blocks finishing line-489: the scrutinee
of `if let` / `while let` / `let…else` is a temporary, and there is no machinery
to scope-and-drop it. It is the same gap behind the line-497 tail-expr carve-out
("an *untracked* tail temp does not drop at all — a leak gap, not an ordering
gap"). Building this once makes both fall out cheaply.

The spec is done; this is purely a codegen implementation gap.

## 2. Current state — what is and isn't tracked

### Tracked today

- **Named `let` bindings** — `track_vec_var`, `track_map_var`, `track_rc_var`,
  `track_struct_var`, `track_enum_var`, `track_user_drop_var`, `track_file_var`,
  `track_soa_groups`, the cluster/elision variants (all in
  `src/codegen/runtime.rs` ~492–1078). Each pushes a `CleanupAction`
  (`src/codegen/state.rs`) onto the top `scope_cleanup_actions` frame; the frame
  drains LIFO at scope exit via `emit_scope_cleanup` (runtime.rs:1100) /
  `drain_top_frame_with_emit` (1235).

### The only two unnamed-temp cleanups that exist

1. **`ref T` rvalue-arg materialization** — `call_dispatch.rs:847–859`. A fresh
   rvalue passed to a `ref T` param is stored into a `ref_rvalue_arg{i}` entry
   alloca; **iff** it is Vec/String-shaped it calls `track_vec_var(temp, None)`.
   Drops at **function scope exit**, not at the statement `;`. Passes
   `elem_ty: None`, so nested-heap elements of that temp leak. Maps/RC args are
   **not** covered.
2. **Discarded `RequestBuilder`** — `free_discarded_request_builder_temp`
   (`stmts.rs:2195`, called from `StmtKind::Expr`). Immediate `http_builder_free`
   for an abandoned `c.request(url).header(...)` chain. HTTP-builder only.

### Where heap temps leak today (the work surface)

| # | Site | Today |
|---|---|---|
| a | Expression statement `make_vec();` (`stmts.rs:2189` `StmtKind::Expr`) | Only RequestBuilder freed; Vec/Map/RC **leak** |
| b | Method-chain intermediates `a.b().c()` | every intermediate temp **leaks** |
| c | Scrutinee of `if let`/`while let`/`let…else` | **leaks** on every path (the line-489 gap) |
| d | By-value call args that are fresh temps | Vec via `ref_rvalue_arg`; Map/RC **leak** |
| e | Operands of binary/index/other operators (`arr[make_vec().len()]`) | **leak** |

`match` scrutinees are intentionally *not* in scope — they live across all arms
by design (design.md § *Temporary Lifetime Rules*, match row) and are handled by
the existing match lowering.

### Key mechanics to reuse

- **`create_entry_alloca(fn, name, ty)`** (`src/codegen.rs` ~5695) — the temp
  slot allocator. Existing synth-name conventions: `ref_rvalue_arg{i}`,
  `__indexed_elem_{n}`, `loop.result`, `clone.dst`. A general temp can mint
  `__tmp{n}` the same way.
- **`emit_free_vec_buffer_if_owned`** (runtime.rs:781) — emits an **immediate**
  (not queued) `cap>0`-guarded outer-buffer free. This is the right primitive
  for "drop at the `;`" / "drop before the non-matching arm" — a *point* drop,
  not a scope-exit drop. (It is outer-buffer-only — no recursive element walk —
  so a general path that needs element recursion must instead push a scoped
  `FreeVecBuffer{ elem_ty: Some(..) }` and drain it.)
- **Scope frames** — `control_flow.rs` already pushes/pops
  `scope_cleanup_actions` frames around if-let (160/173), while-let (722/734),
  blocks (669/687), and for-loops (`control_flow_for.rs:595/615`). The scrutinee
  drop just needs its own sub-frame around the scrutinee eval.

## 3. The spec (already authoritative — do not redesign)

design.md § *Temporary Lifetime Rules* (~2547–2586). Canonical rule:
**"temporaries drop at the end of the smallest enclosing eager-evaluation
context."** Per-position ceiling (NLL may shorten, never extend):

| Position | Drop point |
|---|---|
| Statement-position expr (`expr;`) | at the `;` |
| Tail expression of a block | after tail evaluates, **before** block locals drop |
| Scrutinee of `if let`/`while let`/`let-else` | before the non-matching arm (miss); through the matching arm body (hit); per-iteration for `while let` |
| `match` scrutinee | across all arms (lives to match exit) |
| Function arg / operator operand | at the end of the enclosing statement |
| Binding-extension exception | a `let r = <borrows temp>` **extends** the temp to `r`'s live range |

Drop *ordering* among co-expiring temps/locals/defers is the separate LIFO
program-order rule (§ *Drop ordering within a branch*); this spike only decides
*when each temp's range ends* and emits the drop — it slots into the existing
LIFO drain.

## 4. Proposed design

A single chokepoint: **`materialize_owned_temp(value, ty) -> slot`** in
`src/codegen/runtime.rs`, which (1) mints a `__tmp{n}` entry alloca, (2) stores
the value, (3) if the value is heap-owning (Vec/String / Map handle / RC box /
user-Drop / enum-with-drop — reuse the type-classification the `track_*_var`
helpers already use), pushes the matching `CleanupAction` onto the **current**
`scope_cleanup_actions` frame with the correct `elem_ty: Some(..)` (closing the
`ref_rvalue_arg`'s `None` nested-leak), (4) returns the slot.

The drop point is then determined by *which frame is current* when
`materialize_owned_temp` runs — which is the existing scope-frame machinery:

- **Statement-position temp** — wrap `StmtKind::Expr` (and the discard arm) in a
  one-shot temp frame: push frame → compile expr through
  `materialize_owned_temp` → `drain_top_frame_with_emit`. Drops at `;`.
  Subsumes `free_discarded_request_builder_temp` (becomes a `CleanupAction`
  variant or stays as the immediate special-case, decided in slice 1).
- **Scrutinee temp** — push a dedicated scrutinee sub-frame inside
  `compile_if_let`/`compile_while_let`/`compile_let_else` *around the scrutinee
  eval only*; drain it on the miss edge *before* branching to the else/exit, and
  on the hit edge at matching-arm-body exit (per-iteration for while-let). This
  is line-489 slice 3, now a thin consumer.
- **Tail-expr temp** — already lands on the block frame in program order after
  the lets (line-497 says ordering holds "by construction"); routing the tail
  temp through `materialize_owned_temp` gives it a `CleanupAction` so it actually
  *drops* (closing the leak) while keeping the LIFO order that's already correct.
- **Arg / operand temps** — route fresh-temp args/operands through
  `materialize_owned_temp` against the enclosing statement frame. Generalizes
  and replaces the Vec-only `ref_rvalue_arg` path (now covers Map/RC + recursive
  elem drop).

**Binding-extension exception:** when the temp is borrowed into a `let r`, do
*not* materialize-and-drop at the inner point — defer to `r`'s binding drop.
Detected the same way the existing `let`-extension is (the typechecker/ownership
already classifies borrow-into-temp; codegen consults that classification rather
than re-deriving it).

**NLL shortening** is out of scope for v1 — we emit at the position ceiling. The
spec explicitly allows the ceiling as a correct (if conservative) drop point;
last-use shortening is a later optimization, not a correctness requirement.

## 5. Slice plan (bounded, ASAN-gated)

Each slice is independently landable, gated by `cargo fmt --all -- --check`,
`cargo clippy --all --all-targets --features llvm -D warnings`, the non-llvm
suite, and the relevant `--features llvm` suites. New leak/UAF coverage goes in
`tests/memory_sanitizer.rs` (Linux `detect_leaks=1` is the leak oracle; the
existing `asan_ref_arg_*` / `asan_tail_expr_*` family is the model).

1. **Chokepoint + statement-position temps.** Add `materialize_owned_temp` +
   the type-classification reuse; wrap `StmtKind::Expr` in a one-shot temp frame.
   Fold `free_discarded_request_builder_temp` into it (or leave as the immediate
   special-case and document why). Tests: `asan_expr_stmt_discarded_vec_freed`,
   `_map_freed`, `_rc_freed`. *This slice alone fixes the most common leak (a).*
2. **Generalize call-arg / operand temps.** Replace the Vec-only
   `ref_rvalue_arg` path with `materialize_owned_temp` (Map/RC + `elem_ty:
   Some`); cover operator operands. Tests: `asan_ref_arg_map_freed`,
   `asan_ref_arg_nested_vec_elem_freed` (the `None`→`Some` nested-leak fix),
   `asan_operand_temp_freed`.
3. **Method-chain intermediates.** Route chain receivers/intermediates through
   the chokepoint against the statement frame. Tests:
   `asan_method_chain_intermediate_vec_freed`.
4. **Scrutinee sub-frame (= line-489 slice 3).** Dedicated scrutinee frame in
   if-let/while-let/let-else; drain on miss-before-else, hit-at-arm-exit,
   per-iteration. Tests: `asan_if_let_scrutinee_temp_freed_on_miss` +
   `_on_hit_at_arm_exit`, `asan_while_let_scrutinee_temp_freed_per_iteration`,
   `asan_let_else_scrutinee_temp_freed_before_else`. **Interpreter parity:** the
   tree-walk interpreter is Arc-refcounted so it does not leak, but add matching
   `tests/interpreter.rs` drop-observation tests once slice 6 lands a Drop type.
5. **Tail-expr temp drop (= closes line-497 carve-out).** Route block tail
   temps through the chokepoint. Verify the existing LIFO order is preserved
   (`test_ir_*` ordering assertion) and the leak closes
   (`asan_tail_expr_temp_freed`).
6. **Drop-order *observation* (= line-489 slice 4).** Add a user-`Drop` type (a
   minimal instrumented destructor type, or the `MutexGuard` shape if cheap) so
   the canonical `mu.lock().get(k)` drop-*order* tests from the spec test plan
   can run. Tests: the design.md § *Scrutinee temporary scope* example —
   guard `Drop` fires before the else arm; before preceding `let` bindings in
   tail position. This is the slice that turns slices 4–5 from "ASAN-clean" into
   "provably correct *order*".

Slices 1–3 are the general unblocker and stand on their own. Slices 4–6 are the
line-489/497 payoff and depend on 1–3.

## 6. Risks

- **Double-free against named bindings.** A temp that is *moved into* a `let`
  must not also be temp-dropped. Mitigation: materialize only when the value is
  genuinely discarded/intermediate; the move-into-let path already suppresses
  source cleanup (`suppress_source_vec_cleanup_for_arg` family) — reuse that
  suppression, don't invent a parallel one. ASAN double-free is the gate.
- **Coroutine frames.** Across an A2 coroutine suspend, temp drops must land on
  the per-park destroy edge like locals do (`emit_coro_destroy_edge_drops`
  snapshots `scope_cleanup_actions`). Because temps go on the same stack, this
  is automatic — but add a `coro_e2e.rs` test for a heap temp live across a park
  (mirrors `coroutine_heap_local_across_park`). This is why temps must be
  *queued `CleanupAction`s on the scope stack*, not ad-hoc immediate frees,
  wherever they can cross a suspend.
- **Ordering regressions.** Tail-expr-before-locals is currently correct "by
  construction"; routing tail temps through the chokepoint must preserve it.
  IR-level ordering test guards this.

## 7. What this unblocks

- **phase-6 line 489** scrutinee-temp drop scope (slice 4 here = its slice 3;
  slice 6 here = its slice 4 / MutexGuard observation).
- **phase-6 line 497** tail-expr temp leak carve-out (slice 5 here).
- General correctness: closes leak categories (a)–(e) above, the most common of
  which (discarded expression statements producing a heap value) is reachable by
  ordinary user code today.
