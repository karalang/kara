# Design spike — oversized enum payload (codegen)

**Status:** **Boxing LANDED 2026-06-07** — native support for payloads wider
than a seeded enum's fixed area, via the **box-oversized** representation chosen
in §4. Two slices:
- **Pack + unpack** (commit `873be65c`): `coerce_to_payload_words` heap-boxes a
  value wider than the area (box pointer in word 0) instead of erroring; the
  unpack readers (`reconstruct_payload_value`, the `.unwrap()` helper in
  `calls.rs`) recompute the same `llvm_type_word_count(T) > area` predicate and
  load `T` back from the box.
- **Drop** (commit `873be65c` sibling): a `BoxedEnumDrop` cleanup action frees
  the box (and runs the inner struct drop first when `T` owns heap) at scope
  exit, queued by `track_boxed_enum_var` at the annotated `let o: Option[T]` /
  `Result[T,E]` site.
- **Box-free coverage** (2026-06-07): the box drop now also fires for
  fresh-temp scrutinees (`match v.pop() { … }`, §1, all four pattern
  constructs), the move-OUT-of-box interaction (§2, box-only free), untyped
  lets (`let o = make_opt()`, §3), and `Result.Err` payloads (§4). See the
  **Box-free coverage** follow-ups below for the per-slice detail and the two
  narrow deferred cases (unbound heap-owning payload; method-call-RHS untyped
  let — both leak-only, never double-free).

The fail-loud `E_ENUM_PAYLOAD_OVERSIZED` error is **removed** — the capability it
guarded now works.

**Remaining follow-ups (box-free coverage):**
- **Fresh-temp scrutinee box-free + move-OUT — LANDED 2026-06-07.**
  `Vec[Wide].pop()` / `Map[K,Wide].get()` → `match` / `if let` / `while let` /
  `let … else` produces a boxed `Option`/`Result` with no named binding, so v1's
  let-site drop didn't free it (the box leaked; invisible on macOS — no
  LeakSanitizer). Fixed by `track_freshtemp_boxed_enum_scrutinee`
  (`src/codegen/control_flow_match.rs`), called at the same hook point as the
  user-enum `materialize_freshtemp_enum_scrutinee` in all four constructs (and
  mutually exclusive with it — seeded `Option`/`Result` carry all-`None` drop
  kinds, so the user-enum path no-ops on them). It recovers `T`'s width from the
  bound payload sub-pattern (`pattern_payload_word_count > area`, the unbox
  mirror of `reconstruct_payload_value`), materializes the enum struct into an
  alloca, and queues a `BoxedEnumDrop` for the box. **Box-only free**
  (`inner_struct_name = None`): the bound payload owns `T`'s inner heap and frees
  it via its own binding cleanup, so this resolves the move-OUT double-free
  (`match boxed_opt { Some(h) => … }` where `h` owns heap) by construction — the
  box drop never touches `T`. Tie-in done: `asan_soa_pop_remove_no_leak_or_uaf`
  re-widened to 4-field `Entity` (reads the 4th word `label` back through the box
  + box-free clean). Tests: `test_ir_freshtemp_boxed_option_{match,iflet}_frees_box`
  (leak gate — `boxdrop` block presence) and
  `asan_freshtemp_boxed_option_match_move_out_no_double_free` (move-out
  double-free gate, `H` owns a `Vec`). **Narrow remaining (deferred):** an
  *unbound* heap-owning boxed payload (`Some(_)` where `T` owns heap) leaks its
  inner heap — a wildcard pattern doesn't carry `T`'s width, so detection needs
  the scrutinee's static type; never a double-free, no regression. A
  `Result`-loop terminating on a boxed `Err` `while let` miss is likewise rare
  and deferred (an `Option` loop terminates on `None`, which carries no box).
- **Untyped-let inference + `Result.Err` boxing — LANDED 2026-06-07.** An
  *untyped* `let o = make_opt()` (no annotation) had no `TypeExpr` for the
  let-site box drop to read `T` from, so the box leaked. `declare_function` now
  records each function's full return `TypeExpr` in `fn_return_type_exprs`
  (`fn_return_type_names` kept only the bare segment, dropping the generic arg);
  the let-site recovers it via `untyped_let_boxed_enum_te` when the RHS is a
  direct call to a known free function, and runs the same
  `boxed_enum_payload_variants` + `track_boxed_enum_var` path as the annotated
  let. Because `boxed_enum_payload_variants` already checks **both** `Ok` and
  `Err` (areas 3/5), this also closes the **`Result.Err` boxing** gap for both
  untyped lets (here) and fresh-temp scrutinees (the §1 helper checks every arm
  variant, `Err` included). Tests: `test_ir_untyped_let_boxed_option_frees_box`,
  `test_ir_untyped_let_boxed_result_err_frees_box` (leak gates — both fail
  without the fix), `test_e2e_untyped_let_boxed_option_reads_correct_value`
  (round-trip), `asan_untyped_let_boxed_option_inner_heap_no_double_free`
  (inner-heap balance). **Narrow remaining (deferred):** a *method-call* RHS
  (`let o = v.pop()`) — its return type isn't in `fn_return_type_exprs`;
  recovering it needs synthesizing `Option[elem]` from the receiver's
  `var_elem_type_exprs`. Leaks the box, never double-frees. (Note: the common
  `match v.pop() { … }` shape — pop's result consumed directly, not let-bound —
  is already covered by the §1 fresh-temp scrutinee path.)

A real, IR-proven **silent miscompile** found 2026-06-07 while scoping the
"deep nesting" follow-up of
[`pattern-arm-unbound-field-drop.md`](pattern-arm-unbound-field-drop.md). That
spike's premise — "nested heap payloads leak; extend `EnumDropKind` +
`emit_enum_drop_switch` to recurse and free them" — was **wrong**: the value is
*truncated before any drop question arises*, so there is nothing sound to recurse
into. The leak the spike noted is a downstream symptom of the truncation (the
inner `Vec.cap` reads back as `0`, which is why no free was emitted).

## 1. The bug

Seeded builtin enums have a **fixed payload area**: `Option` = **3** i64 words,
`Result` = **5** (`src/codegen/declarations.rs`, `seed_builtin_enum_layouts`).
User enums size their own area to `max(variant_totals)` and never self-truncate.
But `Option`/`Result` are generic and type-erased to i64 words — the *one* LLVM
`Option` struct (`{ i64 tag, i64 w0, i64 w1, i64 w2 }`) is shared by every
`Option[T]` in the program. When `T` needs more words than the area, the packing
chokepoint `coerce_to_payload_words` silently **truncated** the overflow words
(now a hard error).

This is not exotic. It is reachable by entirely ordinary code, because the
collection ops route their element through `Option`:

- `Vec[T].pop()` / `pop_front()` / `remove()` → `Option[T]`
- `Map[K, T].get()` / `.remove()` → `Option[T]`
- any `fn -> Option[Wide]` / `Option[(a, b, c, d)]` literal

**IR-proven repro (2026-06-07):**

```kara
struct Entity { x: i64, y: i64, hp: i64, label: i64 }   // 4 words
fn main() {
    let mut v: Vec[Entity] = Vec.new();
    v.push(Entity { x: 1, y: 2, hp: 3, label: 5000 });
    match v.pop() {                 // Vec.pop() -> Option[Entity], 4 words into 3
        Some(e) => println(e.label),
        None => println(-1),
    }
}
```

`e.label` (the 4th word) printed **`8271692032`** — garbage from an adjacent
slot, not even a clean zero. The popped value is silently corrupt.

The original spike's `if let Some(Full(_, n)) = make_opt()` (where
`make_opt() -> Option[Holder]`, `Holder.Full(Vec[i64], i64)` = 5 words) is the
same bug: `n` printed `0` instead of `42`, and the `Vec.cap` was zeroed.

## 2. Blast radius (measured 2026-06-07)

Instrumented `coerce_to_payload_words` to log every pack where source words
exceed the destination area, then ran the full codegen + memory_sanitizer suite
(~1411 tests): **exactly one** truncation signature fired —
`asan_soa_pop_remove_no_leak_or_uaf`'s 4-word `Entity` into Option's 3 words.
That test *passed* only because it read `e.x` (word 0, survives), never the
truncated field. Examples with self-referential `Option[ValueEnum]`
(`merge_sorted_lists.kara`) are dead (pre-existing resolve errors);
`Option[shared T]` (e.g. `max_depth_binary_tree.kara`'s `TreeNode`) is a 1-word
RC pointer and fits. So real exposure is "any `Option`/`Result` over a
struct/tuple wider than the area" — common in principle, sparse in the current
corpus because most code uses small structs or `shared` for big aggregates.

## 3. History — fail-loud stop-gap (superseded)

Before boxing, `coerce_to_payload_words` returned `Err(E_ENUM_PAYLOAD_OVERSIZED)`
when the decomposed word count exceeded `num_words`, instead of truncating — a
hard compile error beat garbage (memory `prefer-failloud-over-silent-miscompile`)
while the real representation was designed. The boxing slices **removed** that
error; the capability it guarded now works. The B#1 corpus workaround that
narrowed `asan_soa_pop_remove_no_leak_or_uaf` to a 3-word struct has been
**reverted 2026-06-07** — the fresh-temp-scrutinee box-free (§1) landed, so the
test is back to its 4-word `Entity` and reads the boxed 4th word.

## 4. The chosen representation — box-oversized (LANDED)

| approach | common case (`Option[i64]`/`Option[Vec]`) | wide case | new drop work | reach |
|---|---|---|---|---|
| widen-global (seed area to program-wide max payload width) | **bloats** to widest | works | none | every Option grows; threads width through the hardcoded `coerce_to_payload_words(_, 3/5)` sites |
| **box-oversized** ✅ (heap-box `T` > area, pointer in w0) | unchanged (small) | works | box free + inner drop | localized to pack/unpack/drop of the wide case |
| per-`T` Option monomorphization (distinct `Option_T` LLVM types) | unchanged | works | per-type drop | codegen-wide rewrite (Option is one seeded type today) |

**Boxing was chosen.** Widen-global taxes the hot common case (`Option[i64]`
from `Map.get` in a loop) to fix the rare wide one — wrong default for a
performance-oriented language. Per-`T` monomorphization is cleanest but a
codegen-wide change. Boxing keeps the common path byte-identical and confines the
heap indirection to the wide case.

**The coherence invariant that makes it sound:** the box decision is a pure
function of the static type — `llvm_type_word_count(T) > area` — recomputed
identically at pack (`coerce_to_payload_words`, from the value's LLVM type),
unpack (`reconstruct_payload_value` / the `.unwrap()` helper, from the
typechecker-recorded `T`), and drop (`track_boxed_enum_var`, from the declared
`Option[T]`). The typechecker assigns the same `Option[T]` at a value's
definition and every use, so the sites stay in lockstep with no runtime
"am I boxed" flag. Where a consumer site genuinely cannot recover `T`, it must
**not** read word 0 as inline (the `.unwrap()` path keeps its early-return
fail-loud for that case).

As §4's recommendation foresaw, boxing **re-converged with the original "deep
nesting" drop-recursion**: dropping a boxed `Option[H]` is "run
`__karac_drop_struct_H` (free the inner `Vec`), then free the box" — now on a
*sound* heap `H` instead of a truncated inline one (`BoxedEnumDrop`,
`runtime.rs`).

**Resolved design questions** (the §4 open list): box only when *strictly* wider
than the area (fitting payloads stay byte-identical); keep the seeded
`{tag, w0, …}` layout with the box pointer in `w0` (no niche — a later
optimization); `Result` boxes per-variant at its 5-word area. The move-out suppression
interaction is **resolved** (the fresh-temp box-only free, §1 LANDED) — the
remaining open items are untyped-let inference and `Result.Err` boxing.

## 5. Doc footprint (update these together — see memory `maintain-scope-doc-index`)

- this file — the representation + coherence invariant (entry point)
- `docs/spikes/pattern-arm-unbound-field-drop.md` — "deep nesting" follow-up
  corrected to point here (representation miscompile, not a drop gap)
- `docs/spikes/general-owned-temp-tracking.md` — slice-4 note references this
- `src/codegen/call_dispatch.rs` `coerce_to_payload_words` — pack/box + the
  authoritative doc comment
- `src/codegen/control_flow_match.rs` `reconstruct_payload_value` — unpack/unbox
- `src/codegen/calls.rs` (`.unwrap()`/`.expect()` helper) — unwrap/unbox
- `src/codegen/{state,runtime}.rs` — `BoxedEnumDrop` action + `track_boxed_enum_var`
- `src/codegen/types_lowering.rs` `boxed_enum_payload_variants` — let-site
  predicate; `untyped_let_boxed_enum_te` — §3 untyped-let RHS type recovery
- `src/codegen/control_flow_match.rs` `track_freshtemp_boxed_enum_scrutinee` +
  `src/codegen/control_flow.rs` (if-let/while-let/let-else hooks) — §1 fresh-temp
  scrutinee box-free
- `src/codegen.rs` `fn_return_type_exprs` + `src/codegen/functions.rs`
  (`declare_function` registration) — §3 untyped-let return-type source
- `src/codegen/stmts.rs` let-site boxed block — annotated + §3 untyped box drop
- `tests/codegen.rs` `test_{e2e,ir}_boxed_*`, `test_ir_freshtemp_boxed_option_*`,
  `test_{ir,e2e}_untyped_let_boxed_*` — round-trip + box-free regressions
- `tests/memory_sanitizer.rs` `asan_boxed_option_*` /
  `asan_freshtemp_boxed_option_*` / `asan_untyped_let_boxed_option_*` — box-free
  double-free gates; `asan_soa_pop_remove_no_leak_or_uaf` — re-widened to 4-field
  `Entity` (§1 landed)
