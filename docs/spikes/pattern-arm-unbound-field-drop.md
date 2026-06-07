# Design spike — pattern-arm unbound heap-field drop (codegen)

**Status:** **DONE for all four pattern-matching constructs over enum-variant
patterns (if-let / match / let-else / while-let), 2026-06-07.** Out-of-mechanism
remainders carved to separate follow-ups — see "Remaining" below. Note the
"deep nesting" follow-up was **reclassified 2026-06-07** from a drop gap to a
representation-level silent miscompile (oversized enum payload), now tracked in
[`oversized-enum-payload.md`](oversized-enum-payload.md) and fixed loud. A
real, IR-proven leak found while scoping slice 4 of
[`general-owned-temp-tracking.md`](general-owned-temp-tracking.md).

**Implementation finding (reshaped the fix — simpler than §4's "new per-field
free codegen"):** an ownership-loaded IR probe showed the leak is *only* for a
**fresh-temp** enum scrutinee (`if let Full(_, n) = make()`). A **bound-variable**
scrutinee (`let x = make(); if let Full(_, n) = x`) already frees the unbound
field — its `let`-site `track_enum_var` registered an `EnumDrop` (`__karac_drop_<E>`)
that frees all heap payload words, and `match` additionally calls
`suppress_destructured_enum_payload_cleanup` to zero the cap of *moved-into-binding*
fields so they aren't double-freed. The fresh temp simply had **no source
`EnumDrop`**. So the fix reuses the existing machinery rather than writing new
per-field free codegen: `materialize_freshtemp_enum_scrutinee`
(`src/codegen/control_flow_match.rs`) stores the fresh-temp enum value into an
alloca and `track_enum_var`s it; the existing suppression (refactored into a
`…_at(slot_ptr, enum_name, pattern)` core) then zeroes the moved-in fields'
caps. **Net result is move-out-aware partial drop** — unbound heap fields freed
by the drop walk, moved-in fields freed once by their binding, and on a **miss
edge** (no suppression) the whole temp drops wholesale (which also closes the
fresh-temp half of the sibling spike's slice-4 "scrutinee wholesale drop" for
if-let/match/let-else). The earlier no-ownership probe falsely showed a
bound-variable double-free; that was a `compile_to_ir(None, None)` artifact — the
real (ownership-loaded) build is clean, confirmed by the passing ASAN corpus.

**Landed (2026-06-07):** if-let (`compile_if_let`), match (`compile_match`),
let-else (`compile_let_else`), while-let (`compile_while_let`) — fresh-temp enum
`TupleVariant` / `Struct` / unit-variant scrutinees. Gated to fresh
`Call`/`MethodCall` scrutinees (`expr_yields_fresh_owned_temp`); a place
scrutinee keeps its own `EnumDrop` and is untouched (negative-test pinned). The
first three share one enclosing-frame `EnumDrop`; `while let` is the
per-iteration outlier — its materialize+`track_enum_var` register in the *body*
frame (drains each iteration; the entry alloca is overwritten by the next
iteration's scrutinee before reuse). Tests: `test_ir_iflet_freshtemp_enum_*` /
`test_ir_match_freshtemp_enum_*` / `test_ir_letelse_freshtemp_enum_*` /
`test_ir_whilelet_freshtemp_enum_unbound_field_freed` /
`test_ir_iflet_place_scrutinee_not_materialized` (codegen.rs, the macOS-reliable
gate); `asan_iflet_freshtemp_enum_{bound_field_no_double_free,unbound_field_clean,
miss_wholesale_clean}`, `asan_match_freshtemp_enum_unbound_field_clean`,
`asan_letelse_freshtemp_enum_bound_field_no_double_free`,
`asan_whilelet_freshtemp_enum_{unbound_field_clean,bound_field_no_double_free}`
(memory_sanitizer.rs).

**Remaining — out of B's mechanism, each its own follow-up (still leak, not
miscompile; pre-existing, this fix doesn't widen them):**
- **Deep nesting** (`if let Some(Full(_, n)) = make_opt_holder()`) — **PREMISE
  CORRECTED 2026-06-07: this is NOT a drop gap. It is a representation-level
  silent miscompile, tracked in
  [`oversized-enum-payload.md`](oversized-enum-payload.md).** The earlier
  framing ("extend `EnumDropKind` + `emit_enum_drop_switch` to recurse and free
  the nested `Holder`'s `Vec`") was wrong. An ownership-loaded probe of the
  actual repro showed `Option[Holder]` *truncates* the inner enum: `Holder.Full`
  needs 4 payload words but `Option`'s area is 3, so the high words (`Vec.cap`
  and `n`) are packed as `0`. `n` prints `0` not `42`; the zeroed `Vec.cap` is
  precisely *why* no free was emitted (the leak was a downstream symptom). There
  is nothing sound to recurse into — the value is destroyed before drop. The fix
  is a sound representation first (recommend boxing); the `EnumDropKind`
  recursion then becomes the box's drop walk. The truncation is now a hard
  error (`E_ENUM_PAYLOAD_OVERSIZED`), so this case fails loud instead of
  miscompiling.
- **`while let` heap-bearing *miss* variant** — **LANDED 2026-06-07.** The final
  non-matching scrutinee (evaluated in the header, never entering the per-iteration
  body) was dropped on the floor at loop exit; common drains end on a heap-free
  `None`/`Empty`, but a heap-bearing non-matching variant (e.g.
  `enum Item { Go(Vec), Stop(Vec) }`, matching `Go`, terminating on `Stop`)
  leaked. Fix: `compile_while_let` now routes the false edge through a dedicated
  `whilelet.miss` block, and `drop_freshtemp_enum_scrutinee_on_miss`
  (`src/codegen/control_flow_match.rs`) wholesale-drops the final fresh-temp enum
  there via `__karac_drop_<E>` (no cap-suppression — a miss binds nothing out).
  Same fresh-temp / non-shared / has-heap gate as
  `materialize_freshtemp_enum_scrutinee`, so a place scrutinee (owned elsewhere)
  is untouched (would otherwise double-free). Tests:
  `test_ir_whilelet_miss_variant_freed` /
  `test_ir_whilelet_place_scrutinee_miss_not_dropped` (codegen.rs, the
  macOS-reliable leak gate); `asan_whilelet_miss_variant_no_double_free`
  (memory_sanitizer.rs, the double-free gate).
- **Plain-struct destructure** (`let Point { items: _, count } = make()`) —
  fresh-temp *struct* (non-enum) temp with an unbound heap field leaks
  (IR-probed `mat=0`; `materialize_freshtemp_enum_scrutinee` is enum-only). A
  struct analogue would need `track_struct_var` + a struct-field-offset
  suppression. **Blocked behind a separate codegen gap anyway:** the *bound*-field
  variant (`let Point { items, count } = make(); items.len()`) fails today with
  "no handler for method 'len' on variable 'items'" — struct-destructure field
  bindings aren't fully wired for method dispatch, so the construct is
  half-supported independent of this leak.

Distinct mechanism from the sibling spike (move-out-aware *partial* drop reusing
`EnumDrop` + cap-suppression, not chokepoint routing of a whole temp), so it gets
its own scope doc. See that spike's slice-4 discussion for why the two are
separable.

**Doc footprint** (update these together — see memory `maintain-scope-doc-index`):

- this file — the scope + design (entry point)
- `docs/spikes/oversized-enum-payload.md` — the reclassified "deep nesting"
  follow-up (representation miscompile, not a drop gap)
- `docs/spikes/general-owned-temp-tracking.md` — slice-4 note cross-references
  this as the separate "move-out partial drop" track
- `docs/implementation_checklist/phase-6-runtime.md` line 489 (if-let / while-let
  / let-else) — the control-flow surface this lands in
- `docs/design.md` § *Temporary Lifetime Rules* / *Drop ordering within a branch* —
  the authoritative spec for when unbound payloads must drop (already written)

---

## 1. Problem

When a pattern matches a **fresh-owned** value (an enum or struct *temporary*,
e.g. a function return) and the matching arm **does not bind every heap-bearing
field**, the unbound heap fields are never freed — they leak on the hit path.
Symmetrically, on the miss path the whole matched temp (if heap-bearing) must
drop before the else/exit.

This is *not* the owned-temp-tracking problem (routing a whole fresh temp through
`materialize_owned_temp` and dropping it as one unit). It is **move-out-aware
partial drop**: the pattern moves *some* fields into bindings (whose own cleanup
frees them) and leaves *others* unbound, and codegen must free exactly the
unbound heap fields — never the moved-out ones (that would double-free against
the binding's cleanup).

## 2. Repro (IR-proven, 2026-06-07)

```kara
enum Holder { Full(Vec[i64], i64), Empty }

fn make() -> Holder {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return Holder.Full(v, 42_i64);
}

fn main() {
    if let Full(_, n) = make() {   // binds `n` (i64); the Vec field is `_`
        println(n);
    }
}
```

Emitted `main` IR (abridged) — the enum payload is `{tag,i64,i64,i64,i64}` with
the Vec occupying payload words 1–3 (`{ptr,len,cap}`) and the i64 `n` at word 4:

```llvm
iflet.then:
  %payload  = extractvalue ... %call, 1   ; Vec.ptr  — never freed
  %payload1 = extractvalue ... %call, 2   ; Vec.len
  %payload2 = extractvalue ... %call, 3   ; Vec.cap
  %payload3 = extractvalue ... %call, 4   ; n (i64) — the only bound field
  store i64 %payload3, ptr %n
  ...
  br label %iflet.merge                    ; no free of the Vec buffer
```

No `free` / cleanup is emitted for the Vec. Leaks one buffer per hit. (macOS ASAN
has no LeakSanitizer, so this passes vacuously there — the leak surfaces on Linux
`detect_leaks=1` or via the IR absence of a free; a double-free in the *fix*
would fault on macOS.)

**Scope of the bug — verify each surface:** the repro is `if let`. The same
extract-bound-fields-discard-the-rest shape is used by `while let`, `let…else`,
and plain `match` arms — all are likely affected and must be checked/covered.
Plain `let` destructuring of a temp (`let Full(_, n) = make();`, where legal) is
the same shape.

## 3. Distinct from owned-temp tracking — why its own slice

| | owned-temp tracking (slices 1–3 of the sibling spike) | this spike |
|---|---|---|
| unit of drop | the whole fresh temp, dropped as one | individual *unbound* fields of a matched temp |
| mechanism | `materialize_owned_temp` → `track_*_var` → scope-frame drain | per-field free in pattern-bind codegen |
| key risk | place-expr receiver double-free (gated by `expr_yields_fresh_owned_temp`) | double-freeing a field the pattern **moved into a binding** |
| needs | LLVM value type / `owned_temp_drops` hint | per-variant field types + bound-vs-unbound field analysis |

Folding this into a slice-4 scrutinee frame would entangle clean chokepoint
routing with a subtle move-out-avoidance analysis in one commit. Keep separate.

## 4. Design constraints

- **Move-out-aware.** Free a payload field iff it is heap-bearing AND not bound
  to a by-value (moving) binding. A field bound by `_` or omitted → free it. A
  field bound to `x` (owned move) → the binding's cleanup owns it; do **not**
  free here. A field bound by `ref`/`mut ref` → borrow, the temp still owns it →
  free here (the borrow doesn't outlive the arm — interacts with the spec's
  binding-extension exception, design.md § Temporary Lifetime Rules).
- **Only for fresh-owned scrutinees.** A scrutinee that is a *place expression*
  (an existing binding `x`, a field `a.b`) is owned by something else; partial
  drop here would double-free. Gate to fresh temps — reuse the
  `expr_yields_fresh_owned_temp` discipline from the sibling spike.
- **Per-variant field types.** Codegen needs each enum variant's field type list
  to know which payload words are heap (`{ptr,len,cap}` Vec/String, Map handle,
  RC box, nested struct/enum with drop). The `track_enum_var` / `EnumDrop`
  machinery already knows how to drop an enum's heap payload *wholesale*; this
  slice needs the *selective* version.
- **Drop edges** (mirror the sibling spike's slice-4 table): hit → at matching
  arm-body exit (the unbound fields live through the arm in case bound `ref`s
  alias siblings — conservative: drop at arm exit); miss → before the else/exit
  branch; `while let` → per iteration; `match` → per arm.
- **Ordering.** Slots into the existing LIFO program-order drop drain; does not
  redefine ordering, only *when each unbound field's range ends*.

## 5. Open questions

- Does plain `match` already drop unbound heap payloads (it may have separate
  handling that if-let lacks)? Confirm with the same IR probe per construct
  before assuming uniform absence.
- Struct patterns (`Holder { items: _, n }`) vs enum-variant patterns — same
  analysis, different bind codegen path; cover both.
- Nested patterns (`Some(Full(_, n))`) — the partial-drop analysis must recurse.
  v1 may scope to one level and defer nesting (note the limitation loudly, per
  memory `prefer-failloud-over-silent-miscompile`).

## 6. What this unblocks / relates to

- A correctness fix reachable by ordinary user code today (`if let Variant(_, x)
  = make()` leaks) — higher severity than the sibling spike's guard-style
  scrutinee case, which needs borrow-returning receiver methods on temps to even
  compile.
- Independent of the owned-temp spike's A-track (slices 3b/4/5/6) but **edits the
  same `control_flow.rs` / pattern-bind codegen**, so serialize — do not run in a
  parallel worktree.
