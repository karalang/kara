# Design spike — oversized enum payload (codegen)

**Status:** **Fail-loud stop-gap LANDED 2026-06-07** (`E_ENUM_PAYLOAD_OVERSIZED`
at `coerce_to_payload_words`, `src/codegen/call_dispatch.rs`). The real fix
(native support for payloads wider than a seeded enum's fixed area) is **not
started** — it is a deliberate representation/design decision (box vs widen vs
per-`T` monomorphization), scoped below for later scheduling. Recommendation:
**box oversized payloads** (rationale in §4).

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

## 3. Stop-gap (landed)

`coerce_to_payload_words` now returns `Err(E_ENUM_PAYLOAD_OVERSIZED)` when the
decomposed value's word count exceeds `num_words`, instead of truncating. A hard
compile error beats garbage (memory `prefer-failloud-over-silent-miscompile`):
the "capability" it removes never actually worked. Nested *enum* payloads are
still rejected one level earlier by the typechecker's `E_ENUM_NESTED_ENUM_PAYLOAD`,
so this guard's live surface is oversized **struct / tuple** payloads.

- Diagnostic + guard: `src/codegen/call_dispatch.rs` `coerce_to_payload_words`.
- Negative test: `test_codegen_rejects_oversized_option_payload` (tests/codegen.rs).
- Corpus fix: `asan_soa_pop_remove_no_leak_or_uaf` narrowed to a 3-word struct
  (its `cold { label }` group made the popped `Entity` 4 words). Cold-group
  *layout* coverage is unaffected — it lives in tests/codegen.rs.

## 4. The real fix — design fork (not started)

| approach | common case (`Option[i64]`/`Option[Vec]`) | wide case | new drop work | reach |
|---|---|---|---|---|
| **widen-global** (seed area to program-wide max payload width) | **bloats** to widest | works | none | every Option grows; threads width through the hardcoded `coerce_to_payload_words(_, 3/5)` sites |
| **box-oversized** (heap-box `T` > area, pointer in w0) | unchanged (small) | works | **box free + inner drop** | localized to pack/unpack/drop of the wide case |
| **per-`T` Option monomorphization** (distinct `Option_T` LLVM types) | unchanged | works | per-type drop | codegen-wide rewrite (Option is one seeded type today) |

**Recommendation: box-oversized.** Widen-global taxes the hot common case
(`Option[i64]` from `Map.get` in a loop) to fix the rare wide one — wrong default
for a performance-oriented language. Per-`T` monomorphization is cleanest but is
a codegen-wide change. Boxing keeps the common path small and, crucially,
**re-converges with the original "deep nesting" drop-recursion**: dropping a
boxed `Option[Holder]` is "call `__karac_drop_Holder` (free the inner `Vec`),
then free the box" — exactly the `EnumDropKind` recursion the sibling spike
imagined, but now on a *sound* heap `Holder` instead of a truncated inline one.
So that work is not wasted; it just needs this representation first. Cost: one
heap alloc per wide `Some(...)` + the box-free drop obligation (slots into the
existing `EnumDrop` / synth-drop path).

Open questions for the box slice: niche the boxed case to 1 word (null = None)?
interaction with move-out suppression (B) when a pattern binds a field out of a
boxed payload; `Result` (5-word area) boxing threshold; whether to box at the
seeded-area boundary or only when strictly larger.

## 5. Doc footprint (update these together — see memory `maintain-scope-doc-index`)

- this file — the scope + design fork (entry point)
- `docs/spikes/pattern-arm-unbound-field-drop.md` — "deep nesting" follow-up
  corrected to point here (representation miscompile, not a drop gap)
- `docs/spikes/general-owned-temp-tracking.md` — slice-4 note references this
- `src/codegen/call_dispatch.rs` `coerce_to_payload_words` — the guard + the
  authoritative doc comment
- `tests/codegen.rs` `test_codegen_rejects_oversized_option_payload` — the
  fail-loud regression
- `tests/memory_sanitizer.rs` `asan_soa_pop_remove_no_leak_or_uaf` — narrowed to fit
