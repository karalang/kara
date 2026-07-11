# Design: Headerless RC-elision for in-place link-permuting reshapers

**Status:** IN PROGRESS on branch `worktree-headerless-reshaper` — analysis half partially wired, gated OFF behind `KARAC_HEADERLESS_RESHAPER`. Coverage arm validated; caller-adoption blocker isolated (see §9). NOT on `main`.
**Home once approved:** `docs/implementation_checklist/phase-7-codegen.md` (the design record for the `src/ownership/elision.rs` phases)
**Surfaced by:** kata #92 (Reverse Linked List II) M5 rebench + profiling, 2026-07-11

---

## 1. Motivation & evidence

On kata #92, kāra retires **1.17× the instructions of C at 0.88× its IPC** (1.33× cycles). Profiling ruled out refcount churn (`rc_ops:0` in the hot path), borrow-flag checks (none emitted), the allocator (plain `malloc`/`free`, same as C), and `Option` tags (niche/null-ptr optimized). The gap is dominated by **node width**: kāra allocates **24 B/node** (`malloc #0x18` — `{rc:i64, val, next}`) vs C's **16 B** (`#0x10` — `{val, next}`), on **35.6 M allocations/run**.

The 8-byte header is an rc word the ownership pass **proves it never mutates in the hot path**. kāra already has the machinery to drop it — Phase D "headerless cluster" elision (`emit_headerless_alloc`) — and it **does** fire for pure build/fold/drop:

> **Empirical check (confirmed):** an append-only `build → fold → drop` program (kata #92 minus the reversal) compiles to **16 B nodes** (`mov w0, #0x10`). Add the reversal back and the same `ListNode` reverts to **24 B**. The reversal is the *sole* disqualifier.

Closing this generalizes across the linked-list family (#82/#83/#86/#92) and any borrow-and-permute list algorithm.

## 2. Why the reversal disqualifies (root cause)

For a type `T` to go program-wide headerless, **every** function touching `T` must fit a recognized-safe shape (`fn_covers_member`, `elision.rs:2890`). The existing categories:

| Category | Shape | Recognizer |
|---|---|---|
| **build** | fresh append: `let n = T{…,link:None}; cur.link = Some(n); cur = n` | `recognize_b2` / `triple_at` (exactly **one** store site) |
| **borrow-read** | read-only borrowed `T` param | borrowed family |
| **adopt** | consume a builder's fresh return | adopted-root |

`reverse_between` fits **none** and violates on three axes simultaneously:

1. **The splice** does **three** displacing link-stores (`cur.next = nxt.next; nxt.next = prev.next; prev.next = Some(nxt)`). `recognize_b2` Rule 1 allows exactly one. Semantically, mid-splice a node is transiently **refcount-2** — the reason headerless (no count word) is unsound *in general*.
2. **It returns its input** (permuted), not a fresh allocation — no "in-place transform" category exists.
3. **The `dummy` node** (`let dummy = ListNode{val:0, next:head}`) is a fresh alloc that is **freed while its `.next` chain is returned** — a "free one node, not its chain" shape `FreeClusterWalk` doesn't model.

So this is **not** relaxing a store-count check. It needs a **new coverage category**.

## 3. Proposal: the `reshaper` coverage category

A **reshaper** is a function that *owns* a headerless-`T` list (consuming param — kāra bare `Option[T]` is owned), permutes its link fields via recognized moves, and returns a node of the (permuted) same chain. It **allocates no `T`** (except a recognized dummy) and **frees no `T`** (except the dummy).

### 3.1 Recognized moves (default-deny — anything else poisons)

- **cursor-walk** (read-advance, no store):
  `match cur.link { Some(n) => { cur = n } None => {} }` and `while … { cursor-walk }`.
- **splice-triple** (head-insertion rotation), where `nxt` was bound `= cur.link`:
  ```
  cur.link  = nxt.link      // s0
  nxt.link  = prev.link     // s1
  prev.link = Some(nxt)     // s2
  ```
  Recognized as an atomic unit by a new `splice_triple_at` (sibling of `triple_at`).
- **dummy-detach-and-return**:
  `let dummy = T{…, link: <owned-head>}` … `return dummy.link` — `dummy` is a sentinel whose chain is the returned list.

### 3.2 Integration points

- New `ClusterKind::Reshaper` + an `ElidedCluster { reshaper: true, … }` flag.
- `fn_covers_member`: a `T`-consuming-and-returning fn is *covered* iff it is a recognized reshaper (all stores/allocs/frees fit §3.1) — a fourth arm alongside literal/return/param/builder rules.
- `compute_headerless_types`: a reshaper contributes its member type to `headerless_types` like a builder does.
- Caller side (adopted-root, Phase C1c): the reshaper's **return** must be adoptable as a unique owner exactly like a fresh-return builder's; its **consumed input** must be move-in (not used after the call). Verify the adopted-root path accepts a reshaper return, not only a builder return.

## 4. Soundness argument (the crux to validate)

**Headerless contract.** A headerless `T` has no rc word; codegen emits **no inc/dec anywhere**; the only deallocation is a `FreeClusterWalk` at the owning binding's scope-exit that walks the link chain from the root and frees each node **once**. Correctness requires, at every free point:

> **(WF)** the structure reachable from the freed root is a finite acyclic chain via `link`, each node reachable exactly once, with **no external alias** to any node.

**Claim.** A reshaper preserves (WF) end-to-end, so the caller's single free-walk stays correct.

Proof obligations the recognizer enforces syntactically:

- **(R1) No `T` allocation** except the recognized dummy ⇒ returned node-set = input node-set (a permutation neither creates nor destroys nodes).
- **(R2) No `T` free** in the body except the dummy ⇒ the reshaper never frees the borrowed/owned nodes; the caller's free-walk does.
- **(R3) Every link-store is a recognized move.** The splice-triple is a **bijective edge rotation** (`prev→cur→nxt→X` ⇒ `prev→nxt→cur→X`): no node created/destroyed, chain acyclic and singly-reachable **at triple-end**. The cursor-walk stores nothing. No other store shape is admitted.
- **(R4) The return is a node of the permuted chain**, adopted by the caller as unique owner; the reshaper's input binding does **not** free at scope-exit (move-out, mirroring the borrowed-family exit-dec skip).
- **(R5) Dummy** is freed as a **single node** (not a chain-walk — its `link` points into the returned chain, which must survive).

**Why the transient refcount-2 is benign under headerless.** It exists only *between* splice stores s1 and s2. Headerless emits **no `RcDec` anywhere in the reshaper**, so nothing is freed mid-permutation. The only frees are the dummy (single node, R5) and the caller's post-return walk — both over (WF) structures. The transient double-reachability never intersects a free. ∎ (informal — the differential+sanitizer harness in §6 is the real proof.)

**Ownership note strengthening the argument:** in kata #92, `main` moves `list` into `reverse_between` and doesn't use it after, so the reshaper is the **exclusive owner** during the transform (refcount trivially 1, modulo the transient splice) and hands ownership out via `return`. No borrow/alias concerns — cleaner than the borrowed-read family.

## 5. Codegen impact

Mostly analysis-side. Once `ListNode` is in `headerless_types`:

- Allocation routes through `emit_headerless_alloc` (16 B) — existing.
- Member-field GEPs use the headerless twin at field-base 0 (`shared_gep_layout`) — existing.
- Splice stores are plain pointer overwrites — already how they'd lower once there are no rc ops to emit/skip.
- **New:** the dummy single-node free (R5) — a `FreeSharedElided`-style unconditional null-guarded `free(dummy)` that must **not** recurse into `dummy.link`. Confirm an existing cleanup variant fits or add one.

## 6. Verification plan (silent-miscompile class — non-negotiable)

1. **Differential headerless-on/off oracle:** every linked-list-family kata (#82/#83/#86/#92) + focused unit programs must produce **byte-identical** output with the reshaper elision on vs off (`KARAC_*` toggle or a build flag), across `karac run` / `run --interp` / `build` / default-auto-par.
2. **Sanitizers:** the same set under **LSan (Linux CI)** for leaks and **ASan** for double-free/UAF — via `scripts/lsan-local.sh` (macOS has no LSan). A leak = a store that orphaned; a double-free = a permutation the recognizer wrongly admitted.
3. **Discriminating oracle:** a program where a *buggy* recognizer (admitting a non-permutation store) would double-free or leak, proving the gate actually rejects it.
4. **Corpus regression:** re-run the full bench corpus — the change touches `headerless_types` reconciliation, so verify no other type's classification shifts.
5. **Node-width assertion:** kata #92 nodes drop to `malloc #0x10`; re-bench and confirm the instruction/IPC gap to C narrows as predicted.

## 7. Risks & open questions

- **External alias / escape.** (WF) forbids external aliases. The recognizer must poison on *any* use of a `T` binding outside the recognized moves/reads (store into non-`T` location, capture, `par`/`send`). Inherit the existing `poison_all` default-deny.
- **Niche link required.** The free-walk's null-check needs `Option[T]` niche shape — already gated; the reshaper must keep that gate.
- **Adopted-root acceptance.** The Phase C1c adopted path currently expects a fresh-return builder; must be extended to accept a reshaper return. Risk: subtle interaction with `var_option_shared_heap` registration (adopted roots are never reassigned — verify a reshaper return holds that).
- **Dummy generality.** The dummy shape recognizer must be tight — a mis-recognized "dummy" that's actually a live list node would leak or free wrong. Start with the exact `T{…, link: <owned-head>}` … `return dummy.link` shape only.
- **Scope creep.** Only the splice-triple + cursor-walk + dummy are in v1 of this category. Three-pointer reversal (kata #92 variant) uses a different move set and is explicitly **out** of this slice (it currently triggers an RC-fallback note — separate story).

## 8. Phasing

1. **P0 — recognizer + soundness gate (analysis only), default-OFF.** Add `splice_triple_at`, the reshaper category, `fn_covers_member`/`compute_headerless_types` wiring. Behind an off-by-default flag. Land with the differential+sanitizer harness green on the family. *No perf change yet — this is the correctness core.*
2. **P1 — flip default-ON** once the harness + corpus regression are green across #82/#83/#86/#92 and the discriminating oracle rejects the bad shape.
3. **P2 — measure.** Re-bench #92 (+ family): confirm 24 B→16 B, narrowed C gap, no regression elsewhere. Update the kata READMEs' perf-ceiling notes.

**Estimate:** multi-day; the recognizer is ~a day, the soundness harness + adopted-root integration + corpus re-verification is the bulk. Highest-risk file: `src/ownership/elision.rs` (recognizer soundness) and the adopted-root reconciliation.

---

## 9. Progress & findings (2026-07-11, branch `worktree-headerless-reshaper`)

All changes are in `src/ownership/elision.rs`, **gated OFF** behind `KARAC_HEADERLESS_RESHAPER` (a `reshaper_enabled()` helper). With the flag off, behavior is byte-for-byte unchanged — full non-codegen test suite green (2000+ tests), kata #92 unchanged (24 B nodes, sink 147795689), fmt + clippy `--all --all-targets -D warnings` clean.

**Done (validated empirically):**
- **Coverage arm** — `recognize_reshaper(f, t)` (LOOSE de-risking version: owned `Option[t]` param + `Option[t]` return + `<ident>.<link>` RootLink final expr) wired into `fn_covers_member`; returns `(true, true)` for a recognized reshaper. Confirmed: `reverse_between` now reports `covered=true`. `build`/`fold` already covered.
- **Builder-summary registration** — `reshaper_member(f)` extracts `(T, link_idx)` from the `Option[T]` return; recognized reshapers are inserted into `builder_summaries` so callers can adopt their result (mirrors a fresh-return builder).
- **Empirical proof the mechanism is right:** an *append-only* build→fold→drop program (kata #92 minus the reversal) already goes headerless (16 B, `malloc #0x10`), so the ONLY disqualifier is the reversal — confirmed by A/B.

**BLOCKER — caller adoption of a reshaper result (the §7 risk, now precise):**
`compute_headerless_types` still disqualifies `ListNode` because **`main` reports `covered=false`**. Debug counts: `main: builder_sites=2 adopted=1`. `main` calls both `build` and `reverse_between` (2 builder sites), but only `list` (from `build`) becomes an adopted cluster — **`let r = reverse_between(list, …)` is NOT adopted**. Root cause: `fn_adopted_clusters` / `collect_adoption_candidates` do not recognize a reshaper call, specifically one that **consumes another T root (`list`) as an owned arg**. The existing sanctioned-arg channel only covers *borrowed* positions; a reshaper consumes its input (owned) and returns a new chain — a new arg channel the adoption verifier needs.

**Remaining work (in order):**
1. **Extend `fn_adopted_clusters`/`collect_adoption_candidates`** so `let r = reshaper(root, …)` is an adopted root, and the consumed `root` arg is a recognized owned-consume position (not a poison). This is the hard, soundness-critical core.
2. **Tighten `recognize_reshaper`** from the loose boundary check to a full default-deny body walk (splice-triple via a new `splice_triple_at`, cursor-walk, dummy-with-owned-link literal, no other alloc/free/escape). MUST land before any thought of flipping the flag.
3. **Verify codegen** end-to-end once `main` is covered — this is still UNTESTED (blocked on 1). Confirm 24 B→16 B, correct splice lowering, the `dummy` single-node free (likely already handled by `ReturnedChain::RootLink` — see §3/§4), and the consumed-root arg.
4. **Differential + LSan/ASan harness** across #82/#83/#86/#92 + a discriminating oracle that rejects a non-permutation shape. Land default-OFF only when green; then flip.

**Debug aids left in (gated, inert):** `KARAC_RESHAPER_DEBUG=1` (with the flag on) prints per-fn `touches/covered` and the `builder_sites/adopted/lits/ret_t/t_params/of_t` counts in `compute_headerless_types` / the builder-call rule — the tool used to isolate the blocker above.
