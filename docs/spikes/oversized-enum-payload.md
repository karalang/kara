# Design spike — oversized enum payload (codegen)

**Status:** **Boxing LANDED 2026-06-07** — native support for payloads wider
than a seeded enum's fixed area, via the **box-oversized** representation chosen
in §4. Two slices:
- **Pack + unpack** (commit `873be65c`): `coerce_to_payload_words` heap-boxes a
  value wider than the area (box pointer in word 0) instead of erroring; the
  unpack readers (`reconstruct_payload_value`, the `.unwrap()` helper in
  `calls.rs`) recompute the same `llvm_type_word_count(T) > area` predicate and
  load `T` back from the box.
- **Drop** (this commit): a `BoxedEnumDrop` cleanup action frees the box (and
  runs the inner struct drop first when `T` owns heap) at scope exit, queued by
  `track_boxed_enum_var` at the **annotated** `let o: Option[T]` / `Result[T,E]`
  site.

The fail-loud `E_ENUM_PAYLOAD_OVERSIZED` error is **removed** — the capability it
guarded now works.

**Remaining follow-ups (box-free coverage):**
- **Fresh-temp scrutinee box-free** — `Vec[Wide].pop()` / `Map[K,Wide].get()` →
  `match` produces a boxed `Option` with no named binding, so v1's let-site drop
  doesn't free it (the box leaks; invisible on macOS — no LeakSanitizer). Tie-in:
  re-widen `asan_soa_pop_remove_no_leak_or_uaf` back to 4-field `Entity` once this
  lands (it was narrowed to 3-field as the B#1 stop-gap workaround).
- **Move-OUT of a boxed payload** — `match boxed_opt { Some(h) => … }` where `h`
  owns heap copies the inner pointers; freeing both `h` and the box double-frees.
  v1 ASAN coverage isolates the move-IN suppression (no `match`); the move-out
  interaction needs the `suppress_destructured_enum_payload_cleanup` keying.
- **Untyped-let inference** (`let o = make_opt()` with no annotation) and
  **`Result.Err` boxing** beyond the annotated-Ok/Err cases.

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
narrowed `asan_soa_pop_remove_no_leak_or_uaf` to a 3-word struct still stands —
re-widen it once the fresh-temp-scrutinee box-free (§1) lands.

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
optimization); `Result` boxes per-variant at its 5-word area. The move-out
suppression interaction is the remaining open item (§1 follow-ups).

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
- `src/codegen/types_lowering.rs` `boxed_enum_payload_variants` — let-site predicate
- `tests/codegen.rs` `test_{e2e,ir}_boxed_*` — round-trip + box-free regressions
- `tests/memory_sanitizer.rs` `asan_boxed_option_*` — box-free double-free gate;
  `asan_soa_pop_remove_no_leak_or_uaf` — narrowed to fit (re-widen after the
  fresh-temp-scrutinee box-free, §1)
