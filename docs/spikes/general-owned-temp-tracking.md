# Design spike — general owned-temp tracking (codegen)

> **[self-hosting: defer-to-backwards → [phase-12](../implementation_checklist/phase-12-self-hosting.md#triage-of-the-existing-backlog-2026-06-10)]** — the open **slice 4** (scrutinee sub-frame) is a *leak* in pattern-match scrutinee temps. The compiler pattern-matches constantly, so it's relevant — but it's memory pressure, not corruption; the port surfaces it. Fix early only if the self-hosted compiler balloons.

**Status:** **slices 1–3 + 5 landed 2026-06-06/07**. Slice 1: chokepoint +
statement-position discard (Vec/String). Slice 2 part A: the lowering-pass
`owned_temp_drops` hint table + Map/Set-handle and shared-struct RC-box discard +
Vec element-type closing (nested-heap leak). Slice 2 part B: the `ref_rvalue_arg`
call-arg path migrated onto the owned-temp classification (Vec element closing +
Map-handle cleanup). Slice 3: method-chain receiver temps for the Vec/String
`len`/`is_empty` fast path (the canonical `make_vec().len()` leak). Slice 5: the
tail-expr temp leak — a fresh owned temp in the tail of a *discarded* block
(`{ make() }` / `let _ = { make() };`) routed through the chokepoint via the new
`discarded_owned_temp_tail` block-peel helper (closes the phase-6 line-497
carve-out). Slice 3b (element-type-aware `get`/`first`/`contains` on temp
receivers) **attempted 2026-06-07 and reverted** — blocked on a
`MethodCall.span == receiver.span` collision that needs a typechecker-recorded
receiver-element-type table (see slice 3b note below). **Slice 4 (scrutinee
sub-frame) is NOT cleanly landable on the chokepoint** (finding 2026-06-07,
IR-probed): every realizable `if let`/`while let`/`let…else` scrutinee is an
`Option`/`Result`/enum carrying heap in a *payload*, so dropping it needs
recursive payload drop — the **same machinery as [the pattern-arm unbound-field
drop](pattern-arm-unbound-field-drop.md) (B)**, not chokepoint routing — and the
guard-style scrutinee additionally needs slice 3b. Sequence slice 4 *after* B (or
fold the wholesale-drop case into B's mechanism); it is not a bounded
chokepoint slice. Slice 6 not started. This doc scopes the work and is the
designated *unblocker* for the phase-6 line-489 remainder (scrutinee-temp drop
scope) and the phase-6 line-497 tail-expr leak carve-out — the latter now closed
by slice 5.

**Scope decision (2026-06-06):** build the `materialize_owned_temp` chokepoint +
slice 1 for its standalone architectural value (it stops the special-case
accretion noted in §2), then reassess before committing to the full line-489
chain. Slice 1 is the low-risk entry point: a *discarded* statement value has no
binding, so the double-free-vs-move-into-`let` risk (§6) does not arise.

**Key finding that reshapes slices 2–6:** codegen does **not** receive the full
`expr_types: HashMap<SpanKey, Type>` map from the typechecker — only *derived
hint sets* (`string_typed_exprs`, `method_callee_types`, `user_ord_typed_exprs`,
…), per the codegen-containment rule in CLAUDE.md (analysis phases communicate
via plain-data hint records, not the type map). Consequence: **Vec/String temps
are detectable from the LLVM value type** (`llvm_ty_is_vec_struct` — Vec and
String share `{ptr,len,cap}`), but **Map handles and RC boxes are not** (both are
plain pointers/handles, indistinguishable by LLVM type). So full generality
(Map/RC/user-Drop temps) requires a **new lowering-pass hint table**. **Landed
form (slice 2):** `owned_temp_drops: HashMap<(usize, usize), TypeExpr>` — the
surface `TypeExpr` of each heap-owning temp expression, derived in lowering from
`tc.expr_types` via `TypeChecker::type_to_type_expr` (filtered to
Vec/VecDeque/String/Map/Set/shared). `TypeExpr` (not a bespoke `TempDropKind`
enum) because the existing codegen helpers already consume `TypeExpr`:
`materialize_owned_temp` recovers the Vec element type via `extract_vec_elem_type`
(closing the nested leak), the Map key/val classification via
`map_temp_cleanup_parts` (`shared_heap_type_for_type_expr` + `llvm_ty_is_vec_struct`,
mirroring the let-site), and the RC heap layout via `shared_types`. This reuses the
TypeExpr-valued hint-table precedent (`pattern_binding_inner_types`,
`method_unwrap_inner_types`), so no new derivation machinery was needed.

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
- `docs/spikes/pattern-arm-unbound-field-drop.md` — the separate "move-out
  partial drop" track surfaced while scoping slice 4 (see slice 4 below)

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

1. **Chokepoint + statement-position temps. — DONE 2026-06-06.**
   `materialize_owned_temp` (`src/codegen/runtime.rs`) mints an `__owned_tmp`
   entry alloca, stores the value, and queues a `FreeVecBuffer` on the current
   frame **iff** the value is `llvm_ty_is_vec_struct` (Vec/String). The
   `StmtKind::Expr` arm (`src/codegen/stmts.rs`) wraps the discard in a one-shot
   frame (`push` → compile → `materialize_owned_temp` → `drain_top_frame_with_emit`)
   gated by `expr_yields_fresh_owned_temp` (Call/MethodCall only — excludes
   place expressions, so no double-free against a binding; `ref`-returns are
   `ptr`-typed and rejected by the vec-struct check). `free_discarded_request_builder_temp`
   left as its own immediate special-case (different runtime free fn,
   shape-detected — folding it in buys nothing). **Map/RC discard deferred to
   the hint table (see status note) — not LLVM-type-detectable.**
   **macOS test note:** macOS ASAN has no LeakSanitizer, so the *leak* direction
   can't be caught at runtime here; the leak-closure is gated by an
   **archive-independent IR test** (`test_ir_discarded_vec_temp_emits_free` +
   negative `test_ir_discarded_unit_call_no_owned_temp` in `tests/codegen.rs`,
   asserting the `__owned_tmp` slot + `cleanup.free` drain), and the ASAN tests
   (`asan_discarded_vec_temp_freed_no_double_free`,
   `asan_discarded_string_temp_coexists_with_bound_string`) gate the *double-free*
   direction (which does fault on macOS) and, on Linux CI, the leak too. Gates
   green: fmt, clippy `--all --all-targets --features llvm`, codegen (1240),
   memory_sanitizer (97), non-llvm suite.
2. **Part A — lowering-pass hint table + Map/RC/elem discard. — DONE 2026-06-06.**
   Added `owned_temp_drops: HashMap<(usize, usize), TypeExpr>` to `Program`
   (`src/ast.rs`), populated in `src/lowering.rs` from `tc.expr_types` via
   `TypeChecker::type_to_type_expr` (filtered to Vec/VecDeque/String/Map/Set/
   shared), wired onto codegen state (`src/codegen.rs`, incl. the
   `compile_stdlib_program` swap-all set). `materialize_owned_temp`
   (`src/codegen/runtime.rs`) now takes the producing expr's `span_key` instead
   of an `elem_ty` arg and dispatches three ways: **Vec/String** (LLVM-type
   detectable; elem type recovered from the table via `extract_vec_elem_type`,
   closing slice 1's `None` nested-leak), **Map/Set** (`map_temp_cleanup_parts`
   derives the K/V Vec/shared classification from the `TypeExpr`, → `track_map_var`
   → `FreeMapHandle`), **shared-struct RC box** (`shared_types` heap layout →
   `track_rc_var` → `rc_dec`). The discard caller (`src/codegen/stmts.rs`) passes
   the span. **Adjacent fix:** the explicit-`return m;` path
   (`src/codegen/exprs.rs`) never had the Map tail-return suppression the
   tail-*expression* path carries, so a callee returning a map via `return m;`
   freed the handle *and* returned it → double-free under AOT (latent; no prior
   AOT test returned a user-fn map). Added `suppress_map_cleanup_for_tail_identifier`
   to the explicit-return Identifier arm. **Tests:** IR —
   `test_ir_discarded_map_temp_emits_free` (`__owned_tmp` + `karac_map_free`),
   `test_ir_discarded_nested_vec_elem_freed` (`cleanup.drop.cond` recursive
   element free proves elem_ty flowed). ASAN — `asan_discarded_map_temp_freed`,
   `asan_discarded_nested_vec_string_temp_freed`, `asan_discarded_rc_temp_freed`,
   `asan_returned_map_explicit_return_no_double_free` (the return-suppression
   regression). All gates green.
   **Part B — call-arg temps. — DONE 2026-06-07.** Migrated the Vec-only
   `ref_rvalue_arg` path (`call_dispatch.rs`) onto the owned-temp
   classification via a new `queue_ref_rvalue_arg_cleanup` helper (sharing
   `map_temp_cleanup_parts`, now `pub(super)`): Vec/String temps recover their
   element type from `owned_temp_drops` (closing the `track_vec_var(temp, None)`
   nested-leak for `Vec[String]` / `Vec[Vec[T]]`), and fresh `Map`/`Set` handles
   passed to a `ref Map` param are freed via `FreeMapHandle` (leaked entirely
   before). RC-box rvalue args (`ref shared T`) deferred — the `ref shared T`
   argument ABI needs separate handling; the prior code didn't cover them
   either, so it's not a regression. **Tests:** 2 IR
   (`test_ir_ref_arg_nested_vec_elem_freed` → `cleanup.drop.cond`,
   `test_ir_ref_arg_map_emits_free` → `karac_map_free`) + 2 ASAN
   (`asan_ref_arg_nested_vec_elem_freed`, `asan_ref_arg_map_freed`). The Vec
   element-type extraction reuses `extract_vec_elem_type`; detection stays by
   LLVM value type so a missing hint entry degrades to slice-1 behavior (outer
   freed, inner leaks) — never a double-free.
   **Operand temps (deferred → slice 3).** Operator operands that are fresh
   heap temps overlap the method-chain receiver-temp surface (`make_vec().len()`
   is a chain receiver). Folded into slice 3 rather than split across two slices.
3. **Method-chain receiver temps (Vec/String `len`/`is_empty`). — DONE 2026-06-07.**
   The canonical `make_vec().len()` shape: the non-identifier `len`/`is_empty`
   fast path in `method_call.rs` extracted the length from the receiver struct
   value and discarded it, orphaning `data`. Now, when the receiver is a
   *fresh-owned* temp (`expr_yields_fresh_owned_temp` — Call/MethodCall, which
   excludes place-expression receivers like `h.items.len()` that would
   double-free against the binding's cleanup), the receiver value is routed
   through `materialize_owned_temp` → `FreeVecBuffer` (element type from
   `owned_temp_drops`). `len`/`is_empty` borrow `self` read-only, so the caller
   owns the temp. Drops at the enclosing frame's exit (the position ceiling;
   NLL shortening is out of scope per §4). **Tests:** 2 IR
   (`test_ir_method_chain_receiver_temp_freed` → `__owned_tmp` + `cleanup.free`;
   `test_ir_method_chain_field_receiver_no_owned_temp` → the place-receiver
   negative) + 2 ASAN (`asan_method_chain_intermediate_vec_freed`,
   `asan_method_chain_field_receiver_no_double_free`).
   **Slice 3b (a) — Vec/VecDeque scalar-element read methods on fresh temps.
   — DONE 2026-06-29.** `make_vec().get(i)` / `.first()` / `.last()` /
   `.get_unchecked(i)` / `.contains(x)` on a non-identifier `Vec[T]`/`VecDeque[T]`
   receiver now compile (they hard-errored — "no handler for method 'get' on
   non-identifier receiver"). The fix path scoped above landed exactly as
   designed: a dedicated typechecker table `temp_recv_elem_types`
   (`SpanKey` → element `TypeExpr`) populated in `infer_method_call` where the
   receiver type is known, forwarded through `lowering.rs` to `Program` and onto
   codegen state (sibling to `method_unwrap_inner_types`; same collision dodge —
   `owned_temp_drops[span]` holds the method-result `Option[T]`, not the
   receiver's `Vec[T]`). Codegen's `try_compile_freshtemp_vec_read_method`
   (`method_call.rs`) materializes the receiver into a `__vrecv_tmp` slot,
   registers the element type (`vec_elem_types` + `var_elem_type_exprs`), drop-
   tracks the fresh temp via `track_vec_var` → `FreeVecBuffer` at the enclosing
   frame's exit (gated on `expr_yields_fresh_owned_temp`; the read methods borrow
   `self`, so the caller owns the temp), then re-dispatches through the
   identifier-keyed `compile_vec_method`. **Scoped to SCALAR elements** (the
   typechecker only records `Int`/`UInt`/`Float`/`Bool`/`Char`): a scalar element
   has no destructor, so the `Option[ref T]` result (B-2026-06-07-5 — `get`
   returns `Option[ref T]` even for scalars) registers no second free and the
   single receiver-buffer `FreeVecBuffer` is the complete, double-free-free drop.
   A **heap element** (`Vec[String]`) returns `Option[ref String]` aliasing the
   buffer this frees, and its no-double-free relies on the borrow-binding
   cleanup-suppression (`scrutinee_is_borrow_call`) firing for a *temp* receiver
   — unverified, so it's the next 3b sub-slice. Native-oracle parity: codegen
   output byte-identical to the interpreter across i64/f64/bool elements + OOB/
   empty→None; auto-par A/B identical. **Tests:** 2 IR
   (`test_ir_freshtemp_vec_get_emits_owned_temp_free` → `__vrecv_tmp` +
   `cleanup.free`; `test_ir_freshtemp_vec_get_field_receiver_no_owned_temp` → the
   place-receiver negative, proving the `Call`/`MethodCall`-only typechecker gate)
   + 2 ASAN (`asan_freshtemp_vec_get_no_double_free` — looped fresh-temp get;
   `asan_freshtemp_vec_first_last_contains_no_double_free`).
   **Slice 3b-heap — `Vec[String]` borrow-returning read methods on fresh temps.
   — DONE 2026-06-29.** `make_strvec().get(i)` / `.first()` / `.last()` on a
   fresh-temp `Vec[String]` receiver now compile (they hard-errored — the 3b-a
   gate recorded scalar elements only). The "unverified" borrow-suppression
   concern flagged above **resolved favorably with no codegen-helper change**:
   `scrutinee_is_borrow_call` keys off the *method* (`get`/`first`/`last`), not
   the receiver shape, so the `Some(s)` arm binding's `ref String` is already
   suppressed from independent drop for a temp receiver exactly as for a named
   one. The fix is therefore typechecker-only: the `temp_recv_elem_types` gate
   also records the element type when it is an owned `String` (which resolves to
   `Type::Str` here — *not* `Type::Named { "String" }`) for `get`/`first`/`last`.
   The recorded `str` `TypeExpr` lowers to `vec_struct_type`, so the existing
   `FreeVecBuffer` **vec-struct recursion** (`llvm_ty_is_vec_struct`) per-element
   frees each element String buffer (`cleanup.drop.inner.free`) before the outer
   buffer — the same drop the named-binding `Vec[String].get` path already emits,
   reached verbatim through `compile_vec_method`. So each per-element buffer is
   freed exactly once at frame exit while the borrow reads it: no double-free
   (macOS ASAN) and no leak (Linux LSan, run locally via `scripts/lsan-local.sh`
   — `Compiling karac` confirmed, zero LeakSanitizer reports). **`contains` on a
   fresh-temp `Vec[String]` also landed** (same date, same one-line gate
   addition): it returns `bool`, so *no* borrow escapes and there is no
   suppression obligation at all — the receiver temp just takes the same
   per-element `FreeVecBuffer` recursion, and the compared arg is borrowed (a
   static literal in the tests), not consumed. A *fresh-owned* `contains` arg
   (`contains(make_str())`) is the separate 3b-c operand-temp leak, still out of
   scope. `get_unchecked` (bare `ref String` via a let-binding suppression path
   — `is_borrow_returning_call_expr` — that doesn't fire for builtin methods,
   plus it needs an `unsafe` block) stays scalar-only as a distinct follow-on.
   **Tests:** 2 IR (`test_ir_freshtemp_vec_string_get_emits_per_element_drop` and
   `…_contains_emits_per_element_drop` → `__vrecv_tmp` **and**
   `cleanup.drop.inner.free`, the per-element String free the scalar case never
   emits) + 3 ASAN (`asan_freshtemp_vec_string_get_no_double_free` — looped get,
   ≥36-byte elements so an LSan-reachable-short-string false-pass can't mask a
   regression; `…_first_last_no_double_free`; `…_contains_no_double_free`).
   LSan-gate caveat re-confirmed (`lsan-docker-stale-karac-after-rebase`): the
   first run reused a stale shared-volume karac+test-binary (`running 2 tests`,
   no `Compiling karac`); `cargo clean -p karac` in the volume forced the real
   `running 3 tests` / 3-passed run.
   **Slice 3e — nested `Vec[Vec[scalar]]` elements. — DONE 2026-06-29.**
   `make_grid().get(i)` / `.first()` / `.last()` on a fresh-temp `Vec[Vec[i64]]`
   (matrices/grids/adjacency lists) now compile. Another **one-spot gate lift**:
   the gate records a `Vec[scalar]` / `VecDeque[scalar]` element for
   `get`/`first`/`last`. The element is a `vec_struct_type`, so the same
   `FreeVecBuffer` vec-struct recursion (`cleanup.drop.inner.free`) that frees
   `Vec[String]`'s per-element buffers frees each inner row's data buffer (the
   documented one-level `Vec[Vec[T]]` path), and `get` returns
   `Option[ref Vec[i64]]` — a borrow `scrutinee_is_borrow_call` suppresses, so
   each inner buffer is freed exactly once. **Inner must be SCALAR**: a
   `Vec[Vec[String]]` would leak the innermost String buffers (two-level nesting
   exceeds the one-level recursion) — excluded. `contains`/`get_unchecked` stay
   out for nested Vec. Codegen output matched the interpreter oracle (`20`).
   Verified: no double-free (macOS ASAN) and no leak (Linux LSan — `Compiling
   karac` confirmed, zero reports). **Tests:** 1 IR
   (`test_ir_freshtemp_vec_nested_get_emits_per_element_drop` → `__vrecv_tmp`
   **and** `cleanup.drop.inner.free`) + 1 ASAN
   (`asan_freshtemp_vec_nested_get_no_double_free` — looped, multi-element rows).
   **Slice 3f — user-STRUCT elements via `elem_agg_drop` threading. — DONE
   2026-06-29.** `make_recs().get(i)` / `.first()` / `.last()` on a fresh-temp
   `Vec[Rec]` where `Rec` has a `String`/`Vec`/`shared` field now compile. This
   is the **first slice that adds mechanism rather than lifting a gate**: a struct
   element's heap fields aren't reached by the inline vec-struct recursion (which
   only frees elements that are *themselves* Vec/String), so the helper now
   threads the synthesized per-element `__karac_drop_struct_<S>` — codegen's
   existing `vec_elem_agg_drop_for_type_expr` (which also handles transitive
   `shared` fields) → `track_vec_of_aggs_var`, emitted as the `cleanup.adrop`
   loop. The helper tries the agg-drop first and falls back to `track_vec_var`, so
   scalar/String/nested-POD-Vec elements (not in `struct_types`) keep their
   existing path unchanged. The typechecker gate records a `Vec[<user struct>]`
   element (checked via `self.env.structs`) for `get`/`first`/`last`; `get`
   returns `Option[ref Rec]`, a borrow `scrutinee_is_borrow_call` suppresses, so
   each element's heap fields are freed once at frame exit while the borrow reads
   them (verified reading both the scalar `r.n` and the String `r.name` through
   the borrow). Codegen output matched the interpreter oracle (`2`). Verified: no
   double-free (macOS ASAN) and no leak (Linux LSan — `Compiling karac` confirmed,
   zero reports). **Tests:** 1 IR
   (`test_ir_freshtemp_vec_struct_get_emits_agg_drop` → `__vrecv_tmp` **and**
   `cleanup.adrop.body`, the per-element struct-drop loop the scalar/String cases
   never emit) + 2 ASAN (`asan_freshtemp_vec_struct_get_no_double_free`;
   `asan_freshtemp_vec_struct_string_field_read_no_double_free` — the borrow reads
   the very String the agg drop frees).

   **Slice 3g — user-ENUM elements (gate lift over 3f). — DONE 2026-06-29.**
   `make_toks().get(i)` / `.first()` / `.last()` on a fresh-temp `Vec[Tok]` where
   `Tok` is a user enum with a heap-bearing variant (`Word { s: String }`) now
   compile. This is a **pure typechecker gate lift** — zero codegen change: 3f's
   `vec_elem_agg_drop_for_type_expr` already routes a non-shared enum to
   `emit_enum_drop_switch` (synthesizing `__karac_drop_<Enum>`) and a `shared
   enum` to a per-element rc-dec, so the helper's existing try-agg-then-fallback
   threads the enum drop into the `FreeVecBuffer` identically to the struct case.
   The gate adds `is_user_enum` (checked via `self.env.enums`) alongside
   `is_user_struct` for `get`/`first`/`last`; `get` returns `Option[ref Tok]`, a
   borrow `scrutinee_is_borrow_call` suppresses, so each element's variant payload
   is freed once at frame exit while the borrow reads it (verified matching the
   borrow on its variant and reading the payload String through it, `s.len()`).
   Before the lift `Vec[Enum].get` on a temp hard-errored ("no handler for method
   'get' on non-identifier receiver"). Codegen output matched the interpreter
   oracle (`53`/`20`/`53`). Verified: no double-free (macOS ASAN) and no leak
   (Linux LSan). **Tests:** 1 IR (`test_ir_freshtemp_vec_enum_get_emits_agg_drop`
   → `__vrecv_tmp` **and** `cleanup.adrop.body` + `__karac_drop_Tok`) + 1 ASAN
   (`asan_freshtemp_vec_enum_get_no_double_free` — loops, ≥36-byte payloads,
   reads the payload String through the borrow).

   **Slice 3h — `for x in make_vec().iter()` / `.into_iter()`. — DONE
   2026-06-30.** A fresh-temp Vec iterated in a for-loop. This was a **silent
   miscompile, not a hard error**: the for-loop dispatch (`control_flow_for.rs`)
   peels a transparent `.iter()`/`.into_iter()` and recurses on the receiver, but
   `MethodCall.span == receiver.span`, so at the collided span `expr_types` holds
   the method result `Iterator[T]` (clobbering the receiver's `Vec[T]`). The
   `owned_temp_drops` table (built in lowering from `expr_types`, droppable set
   `{Vec,VecDeque,String,Map,Set}`) therefore had **no entry**, so
   `try_compile_for_vec_value` returned `None` and the loop body was skipped —
   `make_v().iter()` summed to `0` where the interpreter gave `105`. Fix in two
   parts: (1) the fresh-temp Vec gate now records the **element type** span-keyed
   in `temp_recv_elem_types` for `iter`/`into_iter` across every supported element
   shape (scalar/String/POD-Vec/user struct/user enum) — the gate already derived
   the element from the receiver `Vec[T]`, so it runs before the `iter` return
   path; (2) `try_compile_for_vec_value` falls back to `temp_recv_elem_types` when
   `owned_temp_drops` misses, reconstructing `Vec[elem]` via the new
   `vec_type_expr_from_element` so the existing materialize-into-`__for_vec_`-local
   → iterate → scope-exit-drop path runs unchanged. The element-drop threading
   (`var_elem_type_exprs` → `track_vec_var` / `track_vec_of_aggs_var` /
   `track_vec_of_maps_var`) is the same the read-method slices use, so heap
   elements free once at scope exit. `.into_iter()` rides the same path. Verified
   scalar/String/struct/enum all match the interpreter (`60`/`102`/`58`,
   `104`/`107`); no double-free (macOS ASAN), no leak (Linux LSan). **Tests:** 1
   IR (`test_ir_freshtemp_vec_iter_emits_materialize_and_drop` → `__for_vec_`
   **and** `cleanup.drop.inner.free`, proving the materialize fired, not the silent
   skip) + 2 ASAN (`asan_freshtemp_vec_iter_string_no_double_free`;
   `asan_freshtemp_vec_into_iter_struct_no_double_free` — reads the heap field
   through the bound element).

   **Slice 3i — `for (k,v) in make_map()` / `for x in make_set()` (+ `.iter()`).
   — DONE 2026-06-30.** The Map/Set sibling of 3h — same silent-skip-to-0
   miscompile. TWO entry shapes: the **bare** form `for (k, v) in make_map()`
   reached the for-loop `_ =>` arm with the receiver's `Map[K,V]` already in
   `owned_temp_drops` (Map/Set are in the droppable set) but
   `try_compile_for_vec_value` returns None for a non-Vec, so the body was
   skipped; the **`.iter()`** form peels `.iter()`/recurses, hits the collided
   span (`Iterator[(K,V)]` clobbers `Map[K,V]` in `expr_types`), so
   `owned_temp_drops` misses entirely. Fix: (1) new codegen
   `try_compile_for_mapset_value` — the Map/Set twin of the Vec helper:
   materialize the handle into a `__for_mapset_` synth local,
   `register_var_from_type_expr` (map_key_types/map_val_types or set_elem_types),
   queue the per-entry-heap-aware `FreeMapHandle` cleanup
   (`map_temp_cleanup_parts` → `track_map_var`), then drive
   `compile_for_map_var` / `compile_for_set_var` via the recursed Identifier; it
   reads `owned_temp_drops` (bare) **or** falls back to `temp_recv_mapset_types`
   (`.iter()`). Wired in the `_ =>` arm after the Vec attempt. (2) the fresh-temp
   Map/Set gate (slice 3d) now records `temp_recv_mapset_types` for `iter` too
   (Map: `get`/`contains_key`/`iter`; Set: `contains`/`iter`), same scalar/String
   K/V constraint (the `FreeMapHandle` per-entry drop only frees scalar/String).
   Verified scalar Map, String-key Map, Set[String], both bare and `.iter()`, all
   match the interpreter (`130`/`33`/`107`); no double-free (macOS ASAN), no leak
   (Linux LSan). **Tests:** 1 IR
   (`test_ir_freshtemp_map_iter_emits_materialize_and_handle_free` →
   `__for_mapset_` **and** `karac_map_free_with_drop_vec`) + 2 ASAN
   (`asan_freshtemp_map_iter_string_key_no_double_free`;
   `asan_freshtemp_set_bare_string_no_double_free` — covers both the `.iter()` and
   bare entry shapes).

   **Slice 3j — user `impl`-block methods on fresh-temp struct receivers. —
   DONE 2026-06-30.** `make_thing().method()` on a non-shared user struct
   hard-errored ("no handler for method '…' on non-identifier receiver") — a
   silent hard error, not a miscompile. The identifier-keyed user-impl dispatch
   resolves the receiver type via `inferred_receiver_type`, which reads
   `var_type_names` and so returns `None` for a Call/MethodCall receiver, dropping
   through to the diagnostic even though the `Type.method` function exists. Fix: a
   new codegen `try_compile_freshtemp_user_method` (sibling of
   `try_compile_nonident_collection_method`) — recover the struct type from the
   typechecker's `Type.method` callee key (`dispatch_key`), materialize the
   receiver into a `__urecv_tmp` synth local, register it under that struct name,
   and re-dispatch by recursing into `compile_method_call` with a synth Identifier
   receiver (which hits the user-impl arm *before* reaching the helper again — no
   infinite recursion). **Drop handling: track UNCONDITIONALLY** (for a
   fresh-owned receiver), mirroring the `let`-binding path in `stmts.rs`, which
   always `track_struct_var`s a struct local — `track_user_drop_var` when the type
   has an `impl Drop`, else `track_struct_var` → `__karac_drop_struct_<S>`. This
   holds for BOTH self modes: a `ref self` method obviously leaves the caller
   owning the temp, and — the non-obvious part LSan caught — an owned `self` method
   also does NOT drop `self` (the user-impl dispatch passes the receiver by shallow
   value copy and emits no receiver drop), so the caller's temp is still the sole
   owner. The first cut gated tracking on `self_is_ref`; macOS ASAN passed
   (double-free-only) but the Linux LSan gate flagged the owned-`self` field `Vec`
   leaking once per call — a textbook "don't conclude no-leak from a green Mac ASAN
   run". Only a NON-fresh-owned receiver (a borrow-returning call) skips tracking.
   Verified ref-self (reading a field String via `self.items.get(0)`) and
   owned-self, both matching the interpreter (`102`/`152`); no double-free (macOS
   ASAN), no leak (Linux LSan — including the owned-self case the first cut leaked). **Tests:** 2
   IR (`test_ir_freshtemp_user_method_ref_self_materializes_and_drops`;
   `test_ir_freshtemp_user_method_owned_self_materializes_and_drops` — both assert
   `__urecv_tmp` **and** `__karac_drop_struct_Counter`) + 2 ASAN (ref-self and
   owned-self, looped). **NOTE — pre-existing bugs surfaced while
   probing (NOT temp-specific, unfixed here):** `for s in self.field.iter()`
   inside a method iterates 0 (even for a `let`-bound struct — the `self.field`
   for-loop shape isn't recognised); `self.field[i].method()` errors
   ("indexed-receiver … requires the indexed container to be a named variable").
   Both reproduce on clean `main` via a plain `let c = make(); c.method()` and are
   independent method-body-lowering gaps. **Still open (temp surface):**
   `Map.keys()`/`.values()` on a temp (a different binding shape,
   not a for-loop peel even for named maps); heap K/V (`Map[String, Vec[T]]`) on
   temps; deeper-nested `Vec[Vec[String]]` (two-level heap leaks the innermost
   even for named bindings — an upstream recursion limit); `get_unchecked` on
   `Vec[String]`; and (the `vector_method_receivers` model — receiver `(T, N)`
   recorded at the collided span — remains a second copy-able precedent for any
   future receiver-type table).
   **Slice 3k — user `impl`-block methods on fresh-temp ENUM and SHARED
   struct/enum receivers. — DONE 2026-06-30.** The follow-on to 3j: `make().m()`
   where the return type is a value enum, a `shared struct`, or a `shared enum`.
   All three hard-errored identically to 3j ("no handler for method '…' on
   non-identifier receiver") while the interpreter and the `let`-bound codegen
   forms worked. **Two parts.** (1) The **shared** cases had a deeper upstream
   miss: a shared receiver's type is `Type::Shared(name)`, and
   `method_callee_type_name` (typechecker, `types.rs`) had **no `Type::Shared`
   arm** — so `method_callee_types` never recorded the `Type.method` key for any
   shared-receiver call, program-wide, and codegen's `dispatch_key` was `None`.
   The let-bound path never noticed because it dispatches via
   `inferred_receiver_type`/`var_type_names`, not the callee key. Added the arm
   (`Type::Shared(name) => Some(name)`), which now records shared method-call
   sites like any named one (a strict addition — the only other caller of that
   helper is numeric-receiver-gated). Value enums already recorded their key
   (`Type::Named`), which is why enum receivers reached the codegen helper but
   still bailed on its struct-only gate. (2) Extended
   `try_compile_freshtemp_user_method`'s gate from "non-shared struct only" to
   accept value enum / shared struct / shared enum, and routed each to the drop
   its `let`-binding site uses: shared struct/enum (and `par`) →
   `track_rc_var(synth, ptr, heap_type)` (heap type from `shared_types`) = one
   scope-exit `RcDec` running the recursive `__karac_rc_drop_<T>`; value enum →
   `track_enum_var` (no-op for scalar payloads, `__karac_drop_<Enum>` switch for
   heap-bearing variants); non-shared struct → unchanged. Same UNCONDITIONAL
   fresh-owned tracking as 3j: the method borrows or shallow-copies `self` and
   emits no receiver drop, so the caller's temp is the sole owner (one dec / one
   drop frees exactly once — net-zero on the RC count for the shared case, so it
   drives rc→0). Verified enum `75`, shared struct `137`, heap-bearing enum `55`,
   shared `Vec[String]`-field struct `2`, shared enum `51` — all matching the
   interpreter under `run` and `build`; macOS ASAN clean (no double-free), Linux
   LSan clean (no leak — the RC-dec correctness the shared path hinges on).
   **Tests:** 3 IR (`test_ir_freshtemp_enum_method_materializes_and_drops` →
   `__urecv_tmp` + `__karac_drop_Msg`; `…_shared_struct_method…` → `__urecv_tmp`
   + `__karac_rc_drop_Bag`; `…_shared_enum_method…` → `__urecv_tmp` +
   `__karac_rc_drop_Expr`) + 3 ASAN (heap-payload enum, shared `Vec[String]`
   field, shared enum — each looped). **Still open (temp surface):** unchanged
   from 3j's list minus this slice (heap K/V Maps on temps, `Vec[Vec[String]]`,
   `get_unchecked` on `Vec[String]`).
   **Slice 3l — `make_map().keys()` / `.values()` on a fresh-temp Map. — DONE
   2026-06-30.** `.keys()`/`.values()` materialize a fresh `Vec[K]`/`Vec[V]`, but
   the MAP receiver is itself a fresh owned temp — `make_map().keys()`
   hard-errored ("no handler for method 'keys' on non-identifier receiver") in
   both `let`-bind and for-loop forms while the interpreter and the named-map form
   worked. Pure typechecker gate lift: the fresh-temp Map/Set side-table
   (`temp_recv_mapset_types`, slice 3d/3i) recorded only `get`/`contains_key`/
   `iter`, so codegen's `try_compile_freshtemp_mapset_read_method` — which already
   materializes the handle into `__mrecv_tmp`, drop-tracks it via `track_map_var`,
   and re-dispatches through `compile_map_method` (which handles keys/values/
   entries) — bailed on the missing side-table entry. Added `keys`/`values` to the
   Map arm's method match (same scalar/String K/V constraint). The returned Vec is
   owned by the enclosing binding / for-loop like any collection-method result;
   only the Map RECEIVER temp needed the side-table. **No codegen change** — the
   re-dispatch machinery and `compile_map_keys_values_entries` (which CLONES each
   scalar/String element into the result Vec, so freeing the handle afterward
   never dangles the returned Vec) were already in place. Verified scalar `2`/`300`,
   String-key `104`, String-value `2` — all matching the interpreter under `run`
   and `build`; macOS ASAN clean, Linux LSan clean (the two-owner shape — handle
   per-entry drop + cloned-element result Vec — frees each String exactly once).
   (`entries()` followed in slice 3m below.) **Tests:** 2 IR
   (`test_ir_freshtemp_map_keys_emits_materialize_and_handle_free` → `__mrecv_tmp`
   + `karac_map_free`; `…_map_string_keys…` → `__mrecv_tmp` +
   `karac_map_free_with_drop_vec`) + 3 ASAN (scalar keys+values, String-key keys,
   String-value values — each looped).
   **Slice 3m — `make_map().entries()` on a fresh-temp Map. — DONE 2026-06-30.**
   The keys/values sibling (slice 3l). `.entries()` materializes a fresh
   `Vec[(K,V)]`; `make_map().entries()` hard-errored identically ("no handler for
   method 'entries' on non-identifier receiver") in both `let`-bind and for-loop
   forms. The 3l note flagged entries as needing "tuple-element drop threading for
   String K/V" — that turned out to be a non-issue: the returned `Vec[(K,V)]`'s
   tuple-element drop is the SAME machinery the NAMED-map path already uses
   (`let es: Vec[(i64,String)] = m.entries()` is a live codegen test), and the
   fresh-temp case reuses the identical result-Vec handling — only the Map
   RECEIVER temp is new, and its handle drop (`track_map_var`) is method-agnostic.
   So this was the same one-line typechecker gate lift as 3l: add `entries` to the
   fresh-temp Map arm's method match (same scalar/String K/V constraint). **No
   codegen change.** Verified scalar `2`/`303`, String-key `137`, String-value
   `105` — all matching the interpreter under `run` and `build`; macOS ASAN clean,
   Linux LSan clean (handle per-entry drop + cloned-pair result Vec free each
   String once). **Tests:** 2 IR
   (`test_ir_freshtemp_map_entries_emits_materialize_and_handle_free` →
   `__mrecv_tmp` + `karac_map_free`; `…_string_value_entries…` → `__mrecv_tmp` +
   `karac_map_free_with_drop_vec`) + 3 ASAN (scalar, String-key, String-value —
   each looped). **Still open (temp surface):** heap K/V (`Map[String, Vec[T]]`)
   on temps — value/element not scalar-or-String, so the fresh-temp Map gate still
   excludes it; deeper-nested `Vec[Vec[String]]`; `get_unchecked` on `Vec[String]`.
   **Slice 3b-c — operator-operand temps. — DONE 2026-06-29.** `make_str() + "x"`
   leaked the fresh `make_str()` operand. Confirmed the spike's diagnosis: a
   String `+` (and `==`/`<`/… comparison) desugars in `lowering.rs`
   `rewrite_binary` to a `String.add`/`eq`/`lt`/… **assoc call** (because
   `primitive_type_name(Type::Str) == "String"`), so it never reaches the
   `ExprKind::Binary` codegen arm — the fix had to land at the lowered call site,
   `compile_assoc_call`'s primitive-binop arm (`assoc_call.rs`), where the
   operand exprs *and* their compiled values are both in hand. There,
   `compile_string_binop` reads each operand's `{ptr,len}` (concat copies into a
   fresh result buffer; a comparison scans) but takes no ownership, so a
   fresh-owned operand orphaned its buffer once per evaluation. The fix reuses
   the existing `free_fresh_owned_str_arg` (the Set/Map fresh-string-arg helper)
   on each operand after the binop computes its result — no new mechanism. It
   self-gates to fresh-owned shapes (`Call`/`MethodCall`, fresh `String[a..b]`
   slice) with a `cap > 0` backstop, so a named binding / rodata literal / borrow
   operand is never (double-)freed. **Chained concat falls out for free**:
   `make_str() + "a" + "b"` lowers to `String.add(String.add(make_str(), "a"),
   "b")`, and the inner `String.add` is itself a fresh-temp `Call` that
   `expr_yields_fresh_owned_temp` already recognizes — so the intermediate is
   freed by the outer call's operand-free, no `Binary`-special-casing needed.
   Verified: no double-free (macOS ASAN) and no leak (Linux LSan — `Compiling
   karac` confirmed, 3/3, zero reports). **Tests:** 2 IR
   (`test_ir_operand_temp_string_concat_emits_free` → `freearg.free`;
   `test_ir_string_concat_literals_no_operand_free` → the static-literal negative,
   no operand free) + 3 ASAN (`asan_operand_temp_string_concat_freed`,
   `…_chained_concat_freed`, `…_named_binding_not_double_freed` — the
   named-operand double-free guard). **Pitfall recorded:** the first attempt put
   the free in the `ExprKind::Binary` codegen arm and the IR test caught it as
   dead (string `+` is a Call by codegen time); the macOS ASAN test had passed
   *vacuously* on the leak it couldn't see — the IR `freearg.free` assertion is
   what flagged the misplacement.
   **Slice 3d — `Map`/`Set` fresh-temp receivers. — DONE 2026-06-29.**
   `make_map().get(k)` / `.contains_key(k)` and `make_set().contains(x)` on a
   fresh-temp receiver now compile (they hard-errored — "no handler for method
   'get' on non-identifier receiver"). The Map/Set handle is a plain `ptr` (no
   `{ptr,len,cap}` struct to detect, unlike Vec), so a dedicated typechecker
   table `temp_recv_mapset_types` (sibling to `temp_recv_elem_types`) records the
   receiver's **whole** `Map[K,V]` / `Set[T]` `TypeExpr` — `compile_map_method`
   needs K+V, and the handle's `FreeMapHandle` drop is classified from the full
   type (a single element type doesn't suffice). A *separate* table from the Vec
   one so a `Vec[Map[..]]` element type can never be mistaken for a Map
   *receiver*. Codegen's `try_compile_freshtemp_mapset_read_method`
   (`method_call.rs`) materializes the handle into a `__mrecv_tmp` slot,
   registers `map_key_types`/`map_val_types` (or `set_elem_types`) for the synth
   name, drop-tracks the handle via `track_map_var` (classified by the existing
   `map_temp_cleanup_parts`, gated on `expr_yields_fresh_owned_temp`), then
   re-dispatches through the identifier-keyed `compile_map_method` /
   `compile_set_method`. **Scoped to SCALAR K/V/elem**: `Map.get` returns
   `Option[ref V]` (the borrow suppressed by the receiver-shape-agnostic
   `scrutinee_is_borrow_call`, which covers `get`), and a scalar V owns no nested
   heap, so the single `FreeMapHandle` (`karac_map_free` — no per-entry drop) is
   the complete drop; `contains_key`/`contains` return `bool` (no borrow).
   Codegen output matched the interpreter oracle (`200`/`true`/`false`). Verified:
   no double-free (macOS ASAN) and no leak (Linux LSan — `Compiling karac`
   confirmed, zero reports). **Tests:** 2 IR
   (`test_ir_freshtemp_map_get_emits_handle_free`,
   `test_ir_freshtemp_set_contains_emits_handle_free` → `__mrecv_tmp` **and**
   `karac_map_free`) + 2 ASAN (`asan_freshtemp_map_get_no_double_free` — looped
   get; `asan_freshtemp_map_contains_key_set_contains_no_double_free`).
   **Slice 3d-heap — `String` K/V (and Set elem). — DONE 2026-06-29.**
   `make_map().get(k)` on `Map[String, i64]` (heap key), `Map[i64, String]` (heap
   value), `Map[String, String]`, and `Set[String].contains` now compile. Like
   the `Vec[String]` slice this was a **one-spot typechecker gate lift** with no
   codegen-helper change: the gate now records the receiver type when K/V/elem is
   scalar *or* owned `String` (`Type::Str`). Everything downstream already
   composed — `map_temp_cleanup_parts` classifies `key_is_vec`/`val_is_vec` from
   the `TypeExpr`, so the handle drop takes the per-entry variant
   `karac_map_free_with_drop_vec` (frees each entry's key/value String buffer
   before the handle); `compile_map_method` resolves the String LLVM type for the
   lookup; and `Map[_, String].get`'s `Option[ref String]` value borrow is
   suppressed from independent drop by the receiver-shape-agnostic
   `scrutinee_is_borrow_call` — the same single-free shape `Vec[String]`
   established, so each entry String is freed exactly once at frame exit while the
   borrow reads it. Codegen output matched the interpreter oracle
   (`22`/`true`/`<value-string>`). Verified: no double-free (macOS ASAN) and no
   leak (Linux LSan — `Compiling karac` confirmed, zero reports). **Tests:** 1 IR
   (`test_ir_freshtemp_map_string_value_get_emits_drop_free` → `__mrecv_tmp`
   **and** `karac_map_free_with_drop_vec`, the per-entry drop the scalar case
   never emits) + 2 ASAN (`asan_freshtemp_map_string_key_no_double_free`;
   `asan_freshtemp_map_string_value_no_double_free` — the heap-value `Option[ref
   String]` borrow, the riskiest double-free case). **Still open under 3d:** other
   heap K/V (`Vec[T]`, user struct/enum, nested `Map` — need element-drop
   threading the helper doesn't carry); `Map.values`/`keys`/`iter` on a temp.
   None of 3b/3b-heap/3b-c/3d blocks slices 4–6 (the scrutinee/tail/drop-order
   payoff needs the receiver-temp mechanism this slice establishes, which
   `materialize_owned_temp` now provides).
4. **Scrutinee sub-frame (= line-489 slice 3). — fresh-temp enum wholesale +
   partial drop LANDED via B (2026-06-07) for if-let/match/let-else; while-let +
   nesting + non-enum scrutinees remain.** Dedicated scrutinee frame in
   if-let/while-let/let-else; drain on miss-before-else, hit-at-arm-exit,
   per-iteration. **Gate to the wholesale-drop case** — a scrutinee whose
   bindings are all borrows / non-heap (nothing moved out of the temp), so the
   whole temp drops as one unit and partial-drop is never needed. **Update
   (2026-06-07):** the B track ([`pattern-arm-unbound-field-drop.md`](pattern-arm-unbound-field-drop.md))
   landed `materialize_freshtemp_enum_scrutinee` + `track_enum_var` for fresh-temp
   *enum* scrutinees in **all four** pattern-matching constructs
   (if-let/match/let-else/while-let) — which already delivers BOTH the
   wholesale-drop on the miss edge (no suppression → the enum drop walk frees the
   whole temp) AND the move-out-aware partial drop on the hit edge. So slice 4's
   wholesale case is **done for all four constructs over enum scrutinees**. What's
   left under this slice: deep-nested enum scrutinees (needs `EnumDropKind`
   recursion — a core enum-drop enhancement, not chokepoint work), plain-struct
   destructure temps, and the guard-style borrow-returning-method scrutinee
   (still gated on 3b — see below).
   **Why this is not a bounded chokepoint slice:** an IR probe on `main` showed
   that every *realizable* scrutinee is an `Option`/`Result`/enum carrying heap
   in a *payload* (`if let Holder.Empty = make()` where `make() -> Holder` has a
   `Full(Vec[i64])` variant leaks the Vec on the else edge; `Option[Vec[i64]]`
   likewise). Those are **not chokepoint-routable** — `materialize_owned_temp`
   handles only top-level Vec/String/Map/Set/RC, and the scrutinee flows as an
   SSA aggregate (`owned_tmp=0` in the probe). Dropping it needs **recursive
   payload drop of an enum/struct value**, which is exactly the mechanism in
   [`pattern-arm-unbound-field-drop.md`](pattern-arm-unbound-field-drop.md) (B):
   B frees the *unbound* heap fields on the hit path; slice-4 wholesale frees
   *all* fields on the miss/exit path. Both need "given an owned aggregate, free
   its heap-bearing fields (optionally minus the bound ones)." **So sequence
   slice 4 after B** (or fold the wholesale case into B's recursive-drop
   helper) — it is not separable from B as originally hoped. The *moved-out*
   case is B itself — an IR-proven leak (`if let Full(_, n) = make()` leaks the
   unbound `Vec` field). **Additional prerequisite (slice 3b):** the guard-style
   scrutinee (`mu.lock().get(k)`) needs borrow-returning receiver methods to
   dispatch on *temp* receivers — deferred 3b work — before a scrutinee temp
   that lives through the arm even compiles. Tests:
   `asan_if_let_scrutinee_temp_freed_on_miss` + `_on_hit_at_arm_exit`,
   `asan_while_let_scrutinee_temp_freed_per_iteration`,
   `asan_let_else_scrutinee_temp_freed_before_else`. **Interpreter parity:** the
   tree-walk interpreter is Arc-refcounted so it does not leak, but add matching
   `tests/interpreter.rs` drop-observation tests once slice 6 lands a Drop type.
5. **Tail-expr temp drop (= closes line-497 carve-out). — DONE.** A fresh owned
   temp produced in the tail of a *discarded* block (`{ make() }` in statement
   position, or `let _ = { make() };`) is the block's return value: the block's
   own frame (`compile_block_with_frame`) drops only its block-local lets, so
   the escaping tail temp was never freed (IR-probed on `main`: `{ make() }`
   emitted `%call = call @make()` with no `free`). **What landed:** a new
   `discarded_owned_temp_tail` helper (`src/codegen/stmts.rs`) peels
   *single-tail* block wrappers (`Block` / `Seq` / `Unsafe` / `LabeledBlock`)
   down to the tail `Expr` a discarded value originates from, returning it iff
   it yields a fresh owned temp. Two discard sites consume it: (a) the
   `StmtKind::Expr` discard arm now sees through block wrappers (the
   `compile_expr` value IS the block's tail value; the chokepoint is keyed on
   the *tail* expr's span so element/Map/RC hint-table lookups resolve), and
   (b) a new early `StmtKind::Let { Wildcard, .. }` arm routes `let _ = …`
   discards through the same path (`pending_map_insert_old_dec` is set above the
   `match` and consumed inside `compile_expr`, so the `let _ = m.insert(k,v)`
   displaced-value dec is untouched; the chokepoint no-ops on the returned
   `Option`). **Branching tails excluded** (`if`/`match` in discard position):
   an aliasing place-expr branch would double-free, so they stay a safe leak
   for a later slice (pinned by `test_ir_discarded_branching_tail_temp_not_tracked`).
   **Drop *order* (tail temp vs the block's own locals) is a slice-6 concern** —
   the tail temp drops in the discard arm's one-shot frame *after* the block
   frame drained its locals, the reverse of the spec's single-frame LIFO; that
   is unobservable without a user-`Drop` type (slice 6) and leak/UAF-clean
   either way. Tests: `test_ir_discarded_block_tail_temp_freed`,
   `test_ir_let_wildcard_block_tail_temp_freed`,
   `test_ir_discarded_branching_tail_temp_not_tracked` (codegen.rs);
   `asan_discarded_block_tail_temp_freed` (Vec[String] nested-elem free),
   `asan_let_wildcard_block_tail_temp_freed` (Map handle via wildcard-let),
   `asan_discarded_block_tail_temp_with_block_local_no_double_free`
   (block-local + tail temp each freed once). Gates green: fmt, clippy
   `--all --all-targets --features llvm`, codegen + memory_sanitizer
   (`--features llvm`), full non-llvm suite.
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
