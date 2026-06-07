# Design spike — pattern-arm unbound heap-field drop (codegen)

**Status:** **identified 2026-06-07, not started.** A real, IR-proven leak found
while scoping slice 4 of [`general-owned-temp-tracking.md`](general-owned-temp-tracking.md).
Distinct mechanism from that spike (move-out-aware *partial* drop inside
pattern-bind codegen, not chokepoint routing of a whole temp), so it gets its own
scope doc and is sequenced as its own slice. See that spike's slice-4 discussion
for why the two are separable.

**Doc footprint** (update these together — see memory `maintain-scope-doc-index`):

- this file — the scope + design (entry point)
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
