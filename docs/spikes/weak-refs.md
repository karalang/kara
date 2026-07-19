# Spike: `weak T` reference implementation

**Status:** design (2026-07-19). Tracks the fix for [`B-2026-07-19-8`](../bug-ledger.md)
— `weak T` fields are declaration-only today (they parse, lower to `Type::Weak`,
and satisfy the ownership cycle checker, but there is no way to construct, assign,
or read a weak value; no runtime, no codegen). This spike is the design and
slicing to make `weak` a working, memory-safe feature.

## Why

Reference counting cannot collect cycles. design.md § Cycles gives `weak` as the
escape hatch: a `weak` back-edge does not contribute to the strong count, so a
graph with back-pointers (linked list with `random`, tree with `parent`, graph
adjacency) can be freed. Concretely this unblocks kata **#138 (Copy List with
Random Pointer)**, held on an RC-cycle leak ([`B-2026-07-19-6`](../bug-ledger.md)
residual), and the whole cyclic-shared-struct class.

Target semantics (design.md § Cycles, Swift-like): a `weak T` field yields
`Option[T]` on access — `Some(v)` while a strong reference still exists, `None`
once the last strong reference is dropped. No dangling reads.

## Design: two-count control block (Rust `Rc`/`Weak`)

The only memory-safe way to answer "is the target still alive?" without a tracing
GC is the split-count control block: the box outlives the payload.

```
RC box today:      { i64 strong, <fields…> }
RC box with weak:  { i64 strong, i64 weak, <fields…> }   // weak >= 1 while any strong OR weak ref exists
```

Lifecycle (non-atomic; the `par`/Arc path mirrors it atomically):

| op | action |
|---|---|
| **retain (strong)** | `strong += 1` |
| **release (strong)** | `strong -= 1`; at `0`: run the recursive **payload drop** (free owned heap fields), then `weak -= 1` (the implicit weak the strong set holds); if `weak == 0` free the box |
| **downgrade** (`&strong → weak`) | `weak += 1` |
| **weak drop** | `weak -= 1`; if `weak == 0 && strong == 0` free the box |
| **upgrade** (`weak → Option[T]`) | if `strong > 0` { `strong += 1`; `Some(box)` } else `None` |

Invariant: the strong set collectively holds **one** weak count (à la Rust), so the
box is freed exactly when both counts reach zero. The payload's heap fields are
dropped at `strong == 0` (deterministic destruction — the design's promise), but
the 16-byte control header lingers until the last weak ref goes, so `upgrade` can
always safely read `strong`.

## Blast radius (the hard part)

The box layout change shifts every shared-struct field from base 1 to base 2:

- **`shared_gep_layout`** (`codegen.rs`) is the one funnel — base `1 → 2` (headed),
  and the payload-only sub-struct skips two words. Everything that routes through
  it updates for free.
- **Field stores** — there are THREE branches in `compile_field_store`
  (bare-identifier, indexed-shared, nested); [`B-2026-07-19-6`](../bug-ledger.md)
  just proved they drift. All must go through `shared_gep_layout` (post-`-6` they
  do). **Audit these first.**
- **Field reads** (`compile_field_access`), **constructor stores**, **drop synth**
  (`emit_struct_drop_synthesis`), pattern binding — every site that GEPs a shared
  payload by base offset.
- **`headerless` types** (Phase-C2b niche: a single-`Option[Self]`-link node with
  NO refcount word) are the sharp corner. A headerless type has no strong count at
  all, so it cannot host a weak count either. **Decision: a shared struct that is
  the target of any `weak` field (whole-program) is force-**headed** (opt out of
  the niche), so it has the `{strong, weak}` header. Compute this in the same
  whole-program pass that decides `headerless_types`.

## Syntax & typing

- **Field decl:** `mut random: weak Node` (already parses → `TypeKind::Weak`).
- **Downgrade (store):** implicit at a `weak`-field store — `node.random = Some(other)`
  or `node.random = other` where the field is `weak Node` and the RHS is `Node` /
  `Option[Node]`. No new expression syntax; the typechecker coerces `T`/`Option[T]`
  → `weak T` at the field-store / constructor site (mirrors how `mut` fields are
  handled). Alternatively a `.downgrade()` method — but implicit is closer to
  design.md and to Swift.
- **Access (upgrade):** `node.random` on a `weak T` field has type `Option[T]`.
  This is the load-bearing typing change: a `weak` field READ is an upgrade.
- **Null/empty:** a fresh `weak` field with no target reads `None`. Construction
  needs an empty-weak literal — reuse `None` typed at `weak T` (the constructor
  coerces `None: Option[T]` → an empty weak slot).

## Runtime primitives (`runtime/src/` — new symbols)

- `karac_weak_downgrade(box) -> box` — `weak += 1`, return the same pointer.
- `karac_weak_drop(box)` — `weak -= 1`; free box if `strong == 0 && weak == 0`.
- `karac_weak_upgrade(box) -> box|null` — if `strong > 0` `strong += 1` return box
  else null. Codegen wraps the null/non-null as `Option[T]`.
- The strong **release** changes: on `strong == 0`, after payload drop, do the
  `weak -= 1` + conditional box-free (today it frees the box directly). This is a
  change to the existing `emit_rc_dec` / `karac_rc_release` shape, gated on the
  type being headed-with-weak.

## Slicing (each slice LSan-green before the next)

1. **Layout + lifecycle groundwork.** Whole-program "is weak-targeted" set →
   force-headed + `{strong, weak}` box for those types. `shared_gep_layout` base
   bump for weak-boxed types. Strong release → payload-drop-then-weak-dec. NO weak
   ops yet — pure refactor; the entire existing shared-struct + memory_sanitizer
   suite must stay green (this proves the layout change is inert for non-weak use).
2. **Typechecker.** `weak`-field store coercion (`T`/`Option[T]` → `weak T`);
   `weak`-field read type = `Option[T]`; empty-`None` construction. Diagnostics for
   misuse. (No codegen yet → guard behind "weak store/read unimplemented" hard
   error so there's no check-vs-build gap — the [`B-2026-07-19-3`](../bug-ledger.md)
   lesson: never let check pass what build can't do.)
3. **Codegen store (downgrade).** `weak`-field store emits `karac_weak_downgrade`
   (no strong retain) + the field store; `weak`-field drop emits `karac_weak_drop`.
4. **Codegen read (upgrade).** `weak`-field read emits `karac_weak_upgrade` →
   `Option[T]` (Some/None on non-null/null). Wire the recursive drop synth to call
   `karac_weak_drop` for weak fields, never the strong recursive drop.
5. **Kata #138.** `random: weak Node`; deep-copy is now leak-free. Cross-check the
   acyclic + cyclic cases LSan-clean; land the kata.

## Test plan

- Unit (runtime): the four primitives' count arithmetic + free timing.
- ASAN/LSan (the authoritative gate): downgrade-then-outlive (upgrade `Some`),
  downgrade-then-target-freed (upgrade `None`, no UAF), a real cycle (`a.next=b;
  b.weak_prev=a`) freed clean, and the #138 forest. Both x86 and arm64 (the
  [`B-2026-07-12-29`](../bug-ledger.md) arm64-only-leak lesson).
- Regression: the full existing shared-struct suite unchanged after slice 1.

## Risk

Slice 1 is the risk concentrate — a pervasive layout change to the RC memory
model. It is deliberately inert (no behavior change) so it can be validated purely
by the existing LSan suite before any weak semantics ride on top. Every later
slice adds one isolable capability with its own LSan gate. Do NOT collapse slices.
