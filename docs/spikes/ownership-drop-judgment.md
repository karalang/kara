# The Kāra ownership & drop judgment (consolidated)

**Status:** DRAFT — Slice 2 of [`ownership-model-mechanization.md`](ownership-model-mechanization.md). This is the single written artifact the spike calls for: *for every place at every program point, who owns it and when it is freed.* It consolidates the rules that today live implicitly, scattered across `src/ownership.rs`, the drop-insertion codegen, and three partial specs. It is the **model of record**; Slice 3 implements it as an executable oracle, Slice 4 makes codegen read the oracle's answers instead of re-deriving them.

**What it consolidates (the fragments this replaces as the single source):**
- `docs/design.md` § *Temporary Lifetime Rules* (~2551) — *when* an unnamed temporary drops.
- `docs/design.md` § *Drop ordering within a branch* (~9786) — the LIFO ordering among co-expiring drops.
- `docs/design.md` § *Feature 4: Tiered Ownership* (~7806) — the owned → `ref` → RC tiers.
- [`caller-retains-param-model.md`](caller-retains-param-model.md) — the **consumption classifier** (`Escape` vs `NonConsuming`), the predicate whose absence generates the double-free ⇄ leak asymmetry.
- [`general-owned-temp-tracking.md`](general-owned-temp-tracking.md) — the per-position temp materialization/drop mechanics.

That these existed as *separate* specs, each covering one region, is itself the evidence the spike names: the model was emergent-but-unwritten. This document is the join.

> **Scope.** This is the *runtime drop discipline* (what codegen must emit), not the ownership *checker's* diagnostics. `karac build`/`run` tolerate ownership-checker errors by design (only `karac check` gates), so a program can pass `build` and still double-free — that runtime discipline is the target. The checker's separate false-positive class is [`ownership-checker-open-false-positives`], out of scope here.

---

## 1. Domain — places and their state

A **place** is a root local binding plus a (possibly empty) path of **projections**: field access (`.name`), tuple index (`.0`), collection index (`[i]`), or deref. `a`, `a.name`, `a.items[0]`, `t.1.field` are all places rooted at the local `a` / `t`.

At every program point, each place is in exactly one **ownership state**:

| State | Meaning | Free obligation |
|---|---|---|
| **Owned** | The place holds the sole, live claim to a heap allocation. | **Yes** — must be freed exactly once, at its live-range end. |
| **Borrowed** | The place aliases an allocation owned elsewhere (`ref` / `mut ref` / a borrowing read). | No — the owner frees it. |
| **Moved** | The obligation has been transferred *out* of this place to a consumer. | No — and the place must not be read. |
| **Dead** | Uninitialized, or its live range has ended (already dropped). | No — must not be read. |

The lattice is flat: a place is in one state; transitions (below) move it between states. A non-heap (POD) place — `i64`, `bool` — is never Owned in the free-obligation sense; it carries no obligation and the rules below are vacuous for it. "Owned place" throughout means *owned **and** heap-bearing* (String, Vec, Map, Set, Slice-of-heap, a struct/enum/tuple with any heap field, an RC box, or a user-`Drop` type).

**Aggregates split.** An Owned aggregate (struct/tuple/enum-payload/collection) is one place with sub-places for its heap fields/elements. Its obligation is the *set* of its heap sub-places' obligations. Moving a field *out* of the aggregate splits the obligation (§3.4): the moved field's obligation leaves with the consumer; the aggregate retains the rest.

---

## 2. The single invariant

Everything below exists to maintain this one property. It **is** freed-exactly-once + no-use-after-free, restated over places:

> **At every program point:**
> 1. the set of pending free-obligations **equals** the set of Owned (heap-bearing) places — no more, no fewer;
> 2. **no place carries two free-obligations** (no two distinct Owned places alias one allocation);
> 3. **no Moved or Dead place is read** (or projected, or re-dropped);
> 4. every Owned place's obligation is **discharged exactly once**, at its live-range end (§3.5), and the user `Drop` body — if any — runs there, exactly once.

Clause 1 failing *low* (an Owned place with no obligation) is a **leak**. Clause 1 failing *high*, or clause 2, is a **double-free**. Clause 3 is a **use-after-free**. Clause 4 failing is a **drop-elision** (missing user destructor) or a double-drop. The asymmetry of stakes — misclassify a move as non-escape → leak (recoverable, LSan-caught); misclassify a non-escape as escape → double-free (corruption, ASan-caught) — is why the consumption classifier (§4) is the load-bearing rule, not a heuristic.

---

## 3. Transitions (the rules)

### 3.1 Creation — Dead → Owned

Evaluating a heap-producing expression into a place makes it **Owned**: a literal (`"…".to_string()`, `Vec[…]`, a struct/tuple/enum literal), a `Vec.new()`/`Map.new()`, a call/method returning an owned value, an owned-in owned-out helper (`echo_vec(v)`), an arithmetic/format expression producing a String. The new place's obligation is armed at creation.

An **unnamed** result (not bound to a `let`) is a **temporary** — Owned, but with a live range fixed by position (§3.5's temp table), not by a binding's scope.

### 3.2 Move — Owned(source) → Moved(source), Owned(dest)

A move transfers the obligation from source to destination. **Whether an operation is a move is decided by the consumption classifier (§4), not by syntax.** On a move:
- the source becomes **Moved** — its drop is **disarmed** (the source must never free the allocation again); and
- the destination becomes **Owned** — it is now the sole freer.

If the source is *caller-retained* (still Owned after the operation because the operation only *copies* — see `NonConsuming` in §4), then it is **not** a move: no disarm, and the consumer gets its own allocation (a defensive/deep copy, or an RC-inc for a shared payload). **Copy-depth must equal drop-depth**: an entry-copy that clones N heap levels obliges N rc-decs / frees on the copy's drop, and *zero* change to the source's obligation.

### 3.3 Borrow — Owned(source) → Borrowed(alias), source stays Owned

`ref T` / `mut ref T` params, and borrowing reads (`.len()`, a field read that yields a reference, a method returning a borrow), create a **Borrowed** alias. The source stays **Owned** and keeps its obligation; the borrow carries none. Call-site rule: `ref` is never written at a call site; `mut ref` requires the `mut` marker on arguments whose place-root is a fresh owned binding/temporary (design.md Feature 4 Part 1½). A borrow must not outlive its source (the borrow-escape rule; §7).

### 3.4 Projection & obligation splitting

Reading *through* a projection without moving (`acc + a.name.len()`, `p.0`) is a borrowing read — no state change. **Moving a heap sub-place *out* of an aggregate splits the obligation:**
- `let s = a.name;` / `f(a.name)` / `return a.name;` (field move-out), `let x = t.0;` (tuple-element move-out), `let w = v[i];` (index move-out of a heap element), `match m.get(k) { … }` binding that moves the payload out — each transfers *that sub-place's* obligation to the consumer and marks the sub-place **Moved**;
- the aggregate `a` / `t` / `v` stays **Owned** for its *remaining* heap sub-places, and its drop must **skip** the moved-out sub-place (zero its cap / null its slot / suppress that field's rc-dec).

Splitting is where clause 2 is most often violated: if the aggregate's drop still frees a sub-place the consumer now owns, both free it → double-free. This single rule subsumes the entire "suppressor scatter" (`suppress_source_vec_cleanup_for_arg_ex`'s eight handlers, `suppress_inline_option_agg_payload_cleanup`, the tuple/struct/enum move-source zeroers).

### 3.5 Live-range end — the drop point (Owned → Dead)

An Owned place's obligation is discharged at its live-range end, computed as:

**Named bindings** (`let`): live range ends at the **last use** (non-lexical), *ceilinged* at the enclosing scope's exit. Drop fires there; a binding whose last use is mid-scope drops at that use and does **not** appear in the end-of-scope stack (design.md § Drop ordering, rule via RC Dataflow NLL).

**Unnamed temporaries**: live range ends by **position** (design.md § Temporary Lifetime Rules — the authoritative ceiling; NLL may shorten, never extend):

| Position | Live-range end |
|---|---|
| Statement-position expr (`expr;`) | at the `;` |
| Block tail expression | after the tail evaluates, **before** the block's locals drop |
| Scrutinee of `if let`/`while let`/`let-else` | before the non-matching arm (miss); through the matching arm body (hit); per-iteration for `while let` |
| `match` scrutinee | across all arms, drops at match exit |
| match-arm guard | at the end of the guard, before the arm body |
| call/operator/index argument | after the call/operator/index completes (end of enclosing statement) |
| struct/tuple/array literal field | after the literal is fully constructed (then owned by the literal) |
| `return expr;` value | at function return, after defer/errdefer |
| `for` iterator value | across the whole loop, drops at loop exit |

**Binding-extension exception:** `let r = <expr that borrows a temp>` extends the temp's live range to `r`'s — the temp is not dropped at the inner position (design.md § Temporary Lifetime Rules).

**Ordering among co-expiring drops** (design.md § Drop ordering within a branch): a single **LIFO stack ordered by program-order of introduction**, unifying destructors (incl. RC dec), `defer`, and `errdefer`. Reverse declaration order for locals; **tail-expression temporaries pop before block locals** (they are later in program order). Struct fields drop in reverse declaration order. This ordering is orthogonal to *when* each range ends (the table above) — the table sets the set of drops at a point; the LIFO sequences them.

### 3.6 Tier interaction (owned → `ref` → RC)

- **Owned** (default): the rules above; a single freer.
- **`ref` / `mut ref`**: Borrowed — never a freer (§3.3). A `ref`-param's slot holds a pointer into the caller's frame; zeroing/​freeing through it is a bug (corrupts the caller).
- **RC (`shared struct`/`shared enum`, RC-fallback)**: the "obligation" is a reference-count **dec**, not a free; the allocation frees when the count hits zero. Move-out transfers a count (suppress source dec, or the destination already holds a counted ref); copy/retain does an **inc**. Copy-depth == drop-depth applies to the inc/dec legs identically: a copy-supported shared-owning aggregate that is *copied* must inc, and *its* drop must dec; one that is *moved* must transfer (no net inc). The Option[shared] rc-balance bug (B-2026-07-03-28) is exactly this rule applied to an inline-Option payload.

---

## 4. The consumption classifier — the load-bearing predicate

The one predicate the scattered code re-derives per-shape, stated once. For a place used at a site:

```
classify(place, site) -> { Escape, NonConsuming }
```

**`Escape`** — ownership leaves the frame; treat as a **move** (§3.2: disarm source, transfer/rc-transfer):
- the function tail / `return` (or a place rooted at the returned value);
- storage into a place that **outlives the current statement** — a container mutator (`push`/`insert`/`push_back`), a struct/enum/tuple/**collection literal** field, an index-store or field-store into an outliving place;
- capture by an **escaping** closure or a `spawn`/`par` task body (the cross-task capture case — see §7).

**`NonConsuming`** — the source retains ownership; **do not** disarm its drop (copy/rc-inc if the consumer needs its own):
- an argument to a **user function/method whose parameter is owned** — the callee entry-deep-copies owned aggregates / defensively copies owned Vec/String, then frees *its* copy; the caller keeps the original;
- an argument to a **`ref`/`mut ref`** parameter — a borrow;
- a **borrowing read** (`.len()`, a field read, a method returning a borrow).

The distinction the syntactic heuristics blur: **a user-fn owned param is entry-copied (NonConsuming); a builtin container mutator / aggregate literal retains (Escape).** Both look like "value flows into a call/constructor." The classifier keys on the *callee's retention behavior* (param mode; builtin-retains-ness — both already in the ownership-pass signatures and builtin dispatch tables), not the site shape. This predicate is the core of Slice 3's oracle; every §3.4 split and §3.2 move consults it.

---

## 5. Completeness test — every historic drop bug is a stated-rule violation

The spike's acceptance bar for the model: *every bug in the corpus must be a violation of a **stated** rule; if one isn't, the rules have a hole.* Below, the ledger's 39 class-tagged memory-safety bugs (plus named untagged ones) are each attributed to the invariant clause (§2) and rule (§3/§4) they violate. No bug requires a rule not stated here — the model is **closed** over the historic corpus.

### 5.1 double-free (clause 2 / clause 1-high) — a moved-out sub-place's obligation not disarmed (§3.4 split), or an aliased second owner (§3.2)

| Bug | Shape | Rule violated |
|---|---|---|
| B-2026-06-14-8 | bind heap value out of a tuple element (`let inr = h.ps.0`) | §3.4 split — source sub-place not marked Moved / not skipped by aggregate drop |
| B-2026-06-14-11 | `let w = v[i]` for a heap Vec element (cap>0) | §3.4 split — index move-out; element and `w` both freed |
| B-2026-06-14-12 | reading a heap enum/struct Vec element | §3.4 split — same, element type is enum/struct |
| B-2026-07-04-1 | `Vec[f"…"]` literal element (f-string temp) | §3.2 — temp moved into literal (Escape) but source drop not disarmed |
| B-2026-07-04-3 | for-loop element var moved into a tuple `(i, x)` pushed to a Vec | §3.2/§4 — `x` aliases the iterated buffer (NonConsuming borrow), tuple push is Escape ⇒ must copy, not alias |
| B-2026-07-04-5 | `.collect()` over a fresh-temp source | §3.2 — collect consumes (Escape) the temp source; alias not copied |
| B-2026-07-04-17, B-2026-07-05-2, B-2026-07-07-1 | `Vec[struct]` / `Vec[enum]` / `Vec[String]` element **moved out** | §3.4 split — element move-out double-frees against the Vec's element drop |
| B-2026-06-13-19, B-2026-06-13-20 | Map handle in a struct field / tuple-in-struct | §3.6 RC/handle — Maps are caller-retains; a second freer added |
| B-2026-07-01-12 | `Map.get` payload moved out of a **borrow**-bound match arm | §3.3+§3.4 — payload is Borrowed (map still owns); moving it out double-frees against the map's stored value |

### 5.2 leak (clause 1-low) — an Owned place with no discharged obligation

| Bug | Shape | Rule violated |
|---|---|---|
| B-2026-07-03-30 | non-shared struct **field** `Vec[String]`/`Vec[Map]`/`Vec[Vec]` | §2.1 — each element is Owned; struct drop didn't recurse into the field's elements |
| B-2026-06-14-25, B-2026-06-14-26 | Map/Set returned by value and bound / in a bare tuple | §3.5 — the returned handle is Owned at the binding; no drop emitted |
| B-2026-06-19-2, B-2026-06-17-2/3 | heap value moved into a `spawn`/`tg.spawn` closure | §3.5 in the task frame — the moved-in capture is Owned by the task; task exit didn't drop it |
| B-2026-07-02-2, B-2026-07-03-27/28/31/33, B-2026-07-04-7 | `Option[heap]` payloads as fields / destructured / returned | §2.1 + §3.4 — the Some-payload is Owned; undestructured/escape drop missing (the caller-retains classifier's exact target) |
| B-2026-06-12-6, B-2026-06-14-1/2/21/31/32/33 | entry-copy / tuple-var-from-call / body-local heap in a for-loop / shared-box walker | §3.5 + §3.6 — an Owned temp/local/field with no drop, or copy-depth < drop-depth (the copy under-recursed) |
| B-2026-06-29-1 | fresh `Vec[String]` arg to a dispatched method | §3.5 — statement-position temp arg not materialized-and-dropped |

### 5.3 drop-elision (clause 4) — user `Drop` not run at the owner's live-range end

| Bug | Shape | Rule violated |
|---|---|---|
| B-2026-07-01-6 | enum-variant Drop-typed **temp** passed directly as a call arg | §3.5 temp table — arg temp's range ends after the call; its user `Drop` skipped |
| B-2026-07-01-7 | fn-returned Drop-typed temp passed as a call arg | §3.5 — same, source is a call result |
| B-2026-07-01-8 | interpreter never runs user `Drop` for value enums | §2.4 — the obligation includes the user `Drop` body; not run |

### 5.4 soundness (clause 3 / §7 borrow-escape) — a borrow outlives its owner

| Bug | Shape | Rule violated |
|---|---|---|
| B-2026-06-22-2 | an **escaping** capturing closure (dangling stack environment) | §7 borrow-escape — the closure's captured borrow outlives the frame; capture must be by-move (Escape) or the env heap-allocated |
| B-2026-06-22-4 | calling a closure stored in a struct field returns 0 | §3.3 — the stored closure's environment place mis-resolved (a read of a mis-owned place) |

**No hole found.** Every class-tagged bug, and every untagged heap-shape miscompile the spike named (for-loop-element-escape, boxed-`Option` move-out, cross-task capture, index-store, Map/Set key adoption), is a violation of a clause in §2 via a rule in §3–§4. The one *near*-hole worth flagging: the closure soundness bugs (§5.4) sit at the borrow-escape boundary (§7), which this judgment states as a rule but does **not** yet fully mechanize (closures are excluded from the Slice-1 fuzzer for the same reason — a live ownership-checker FP). That is the model's known open edge, not a missing rule.

---

## 6. Sanity checks (the spike's required one-line consequences)

The spike requires the model independently explain two named bugs as **one-line consequences**:

- **for-loop-element-escape (B-2026-07-04-3).** In `for x in v.iter() { a.push((i, x)); }`, `x` is a **Borrowed** alias of `v`'s buffer (§3.3, `.iter()`). `a.push((i, x))` is an **Escape** of a tuple containing `x` (§4). Escaping a Borrowed place is not a move of an Owned place — so the push must **copy** `x`'s allocation, not alias it (§3.2 NonConsuming-source ⇒ defensive copy). Aliasing gives two Owned places over one buffer (clause 2) → double-free. *One line: an Escape whose payload is a Borrowed alias must deep-copy.*

- **boxed-`Option` move-out (B-2026-07-03-31 / B-2026-06-14-8).** `let A { value } = a; match value { Some(v) => f(v) }` moves the `Some` payload out of the aggregate (§3.4 split): `value`/`a.value` becomes **Moved**, its obligation transfers to `v`. But `f(v)` where `f` takes an **owned** param is **NonConsuming** (§4) — the callee entry-copies, `v` stays Owned and must drop. Disarming `v`'s drop (mistaking the call for an Escape) orphans the inner String → leak. *One line: field move-out arms the destination's drop; a NonConsuming consumer must not disarm it.*

Both fall out of §3.4 + §4 with no new machinery — the evidence the model reaches the right shapes.

---

## 7. Open edges — what the judgment states but does not yet fully pin

- **Borrow-escape / closures. — MECHANIZED for the corpus 2026-07-08.** The oracle now models the *drop-schedule* consequence of both capture forms the fuzzer produces, so the differential **checks 100% of generated programs (0 skipped, 0 divergences)** — up from ~32%: (a) a **`spawn`-closure** capture demotes the captured heap binding to `Borrowed` (it escapes as an auto-promoted shared/RC reference; the RC/join owns the free, not the scope), with later reads / additional captures valid (no false use-after-move on the multi-`spawn` shape); (b) a **`par {}`** block captures `shared struct` values whose scope-exit `RcDec` *is* the drop the oracle already schedules — so those need no special handling and simply agree. Verified by `ownership_oracle::tests::spawn_captured_vec_is_not_scheduled_and_no_uam` and `tests/drop_differential.rs::{spawn_capture_is_checked_clean, par_block_shared_capture_is_checked_clean}`. **Still open (not exercised by the fuzzer):** the general borrow-*escape decision procedure* for **stored / heap-env** closures — a closure that outlives its captures and is invoked later, vs one whose captures are by-move. The oracle's current rule is the conservative "any closure capture is a shared borrow," which is exactly right for the `spawn`/`par` escapes the corpus contains but is not a full escape analysis; heap-env closures are additionally gated out of the fuzzer by a live ownership-checker false positive, so this edge is blocked on that FP, not on the model.
- **NLL shortening (the floor).** §3.5 emits at the position/scope **ceiling**; last-use shortening (the floor) is a correctness-preserving optimization, explicitly out of scope for the oracle's v1 (design.md § Temporary Lifetime Rules composition-with-NLL). The oracle computes the ceiling; matching codegen's ceiling is the differential test, not the floor.
- **Cross-task capture rc-transfer.** §4's Escape includes `spawn`/`par` capture, and §3.6 states the rc-transfer rule, but the *auto-promotion* decision (when a captured heap value becomes a shared/RC reference vs a by-move transfer) is an auto-par-surface detail the oracle must consult from the concurrency analysis, not re-derive.
- **Match-arm payload heap-ness / place attribution.** For `match o { Some(x) => … }` on an owned `Option[String]` (or any enum arm binding a heap payload), the oracle moves the scrutinee `o` and introduces the payload binding `x` as **non-heap** — it does not infer the arm-payload's type (`ownership_oracle::bind_match_pattern_inner`), so it schedules **no** drop for that shape. This is *sound-by-under-approximation* here only because codegen frees the inline payload via `o`'s slot, not a distinct `x` slot: the free exists, just attributed to a different place. Inferring the payload type to schedule `x`'s drop would not improve the Slice-4 differential — it would swap a zero-schedule for a **place-attribution** mismatch (oracle "drop x" vs codegen "free o's payload"), the same cross-boundary accounting difference as owned params (differential rule 2). Pinning this needs a *place-equivalence* notion (o's payload ≡ x) shared by model and codegen, not just payload-type inference. Surfaced by `tests/drop_differential.rs::option_string_match_is_clean`.

---

## 8. How Slices 3–4 consume this

- **Slice 3 (oracle). — DELIVERED 2026-07-07: [`src/ownership_oracle.rs`](../../src/ownership_oracle.rs).** The judgment is now executable: a standalone pass (no codegen / `inkwell` dependency) that implements `classify` (§4) and the per-place-per-point state computation (§1–§3), producing for each function a **drop schedule** (§3.5, LIFO) and the **invariant violations** (§2). It is unit-tested against the rules and against both §6 sanity-check shapes (asserting the model certifies the source valid and schedules exactly the drops codegen historically got wrong). Wired into the fuzzer (`drop_fuzz --oracle-only`, or always-on alongside the ASan path): over **2000** generated programs it scheduled **~19.8k** drops with **0** invariant violations — the model and the generator agree corpus-wide. The v1 oracle covers the fuzzer's heap-core subset (see the module's `## Scope`); two edges are conservative exactly where §7 says the judgment is open (closure/cross-task captures treated as borrows; NLL shortening not modelled — drops at the ceiling). The remaining piece — running the oracle **differentially against codegen's actual drops** — needs the codegen observability hook, which is Slice 4.
  - Original slice text: *Implement `classify` (§4) and a per-place-per-point state computation (§1–§3) as a standalone pass. It answers, for every place at every point: state (§1), and — for each program point — the exact set of drops to fire (§3.5) and which sub-places are split-suppressed (§3.4). The fuzzer then runs **differentially**: model says "drop here / this is Moved"; check codegen did the same. A divergence is now attributable to the lowering, not the model.*
- **Slice 4 (codegen reads the oracle). — DOWN-PAYMENT DELIVERED 2026-07-07: the differential.** The observability half is done: `src/codegen/drop_obs.rs` (a read-only, off-by-default recorder) taps the single cleanup-drain funnel (`emit_cleanup_action_at`) and `drop_fuzz --differential` diffs the oracle's per-function schedule (this §3.5 ceiling) against codegen's *actual* emitted drops. Over the corpus: **330 programs, 2199 scheduled drops, 0 divergences** — codegen's emitted drop set covers the model's schedule on every checked function, i.e. the §3.5 ceilings match (the §7.NLL floor is deliberately not compared). The comparison checks the **missing-drop (leak)** direction only — the extra-drop direction is not emit-time observable (codegen guards moved-out drops at runtime while keeping the action), so the ASan/LSan run stays the double-free authority. It excludes parameter drops (codegen frees bare `String`/`Vec` params caller-side vs the model's callee-owns — freed once, across the boundary) and the §7 closure/capture edge (skipped + counted). Non-vacuity is proved by a permanent silence knob (`KARAC_DROPOBS_SILENCE=1` → the whole schedule reports missing). **What remains** is the structural fix: replace the suppressor scatter (§3.4's list) and the drop-point re-derivation with reads of the oracle's facts, so "checker thinks it's Moved, codegen still drops it" becomes impossible by construction — one computed set of facts, both surfaces consult it. The differential is the net that refactor lands behind: it proves agreement today and will catch any drift.

This document is the contract between the two: the oracle *is* this judgment made executable; codegen *consumes* the oracle. A future disagreement is resolved by reading §2 — the invariant is the arbiter.
