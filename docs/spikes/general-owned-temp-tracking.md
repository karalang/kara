# Design spike — general owned-temp tracking (codegen)

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
   **Deferred to a follow-up (slice 3b):** (a) element-type-aware Vec/String/Map
   receiver methods on temps (`get`/`contains`/`iter`, `Map.get` etc.) — these
   hard-error today (no non-identifier handler) and dispatch via the synth-alloca
   redispatch the `RequestBuilder` chain uses (`compile_vec_method` on a
   registered synth name). **Blocker found 2026-06-07 (attempted, reverted):**
   `compile_vec_method` needs the receiver's *element* LLVM type to shape the
   `Option[T]` payload of `get`/`first`/`last`, but it cannot be recovered for a
   temp receiver — the parser sets `MethodCall.span == receiver.span`, so
   `make_vec()` (receiver) and `.get(0)` (outer) collide at one `expr_types`
   key, and `owned_temp_drops[span]` holds the *method-result* type
   (`Option[i64]`), not the receiver's `Vec[i64]`. (LLVM-type detection gives
   only the type-erased `{ptr,len,cap}`, no element.) **Fix path:** a dedicated
   typechecker-recorded table mapping the method-call span → the receiver's
   element `TypeExpr`, populated in `infer_method_call` (where the receiver type
   is known) and forwarded through lowering — exactly the pattern
   `method_callee_types` uses to dodge the same span-collision race. (A second
   live instance landed 2026-06-07 with the WASM SIMD-128 slice:
   `vector_method_receivers` — receiver `(T, N)` recorded at the collided
   span in `infer_method_call` *and* at `Vector` lane-read Index spans, folded
   into `unsigned_vector_exprs` in lowering.rs for SIMD reduce/print
   signedness — copy either model.) That makes
   3b a small cross-phase slice (typechecker + lowering + the codegen
   redispatch), not codegen-only. Until it lands these methods on temps stay a
   hard error (fail-loud, no silent leak);
   (b) user-`impl` methods on fresh-temp receivers (also unsupported today);
   (c) **operator-operand temps** (`make_str() + "x"`) — the lowered
   `String.add(make_str(), "x")` passes the fresh operand as an *owned* arg to
   an inline intrinsic that copies but never frees it, so closing that leak is
   an owned-arg-to-intrinsic concern distinct from the borrow-receiver shape
   handled here (`asan_operand_temp_freed` lands with it). Each is its own
   bounded, ASAN-gated slice; none blocks slices 4–6 (the scrutinee/tail/drop-
   order payoff needs the receiver-temp mechanism this slice establishes, which
   `materialize_owned_temp` now provides).
4. **Scrutinee sub-frame (= line-489 slice 3). — BLOCKED on B's recursive
   payload-drop machinery (finding 2026-06-07, IR-probed).** Dedicated scrutinee
   frame in if-let/while-let/let-else; drain on miss-before-else,
   hit-at-arm-exit, per-iteration. **Gate to the wholesale-drop case** — a
   scrutinee whose bindings are all borrows / non-heap (nothing moved out of the
   temp), so the whole temp drops as one unit and partial-drop is never needed.
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
