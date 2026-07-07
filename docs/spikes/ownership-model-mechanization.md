# Spike: Mechanize the ownership/drop model — stop the drop-soundness whack-a-mole

**Status:** OPEN — proposed epic. Independent of, and parallelizable with, the LLJIT productionization spike (different bug axis).
**Decision date:** 2026-07-06. **Owner call:** worth doing; start with the measurement slice (a fuzzer), *not* the proof.

---

## Decision & rationale

The drop-soundness bug history is whack-a-mole: copy-depth == drop-depth, by-value struct field move-out double-free, boxed-`Option` move-out UAF, for-loop element escape double-free, cross-task shared-heap capture double-free, index-store of heap Vec elem double-free, Map/Set key no-adopt leak, narrow-elem store stride. **Each was found by a kata, diagnosed in codegen, patched at that one site — then the same *class* reappeared in a new heap shape a few weeks later.** That pattern is the signature of an **unspecified invariant**: there is no single artifact that states "for every place at every program point, who owns it and when it is freed." The rules live implicitly, scattered across `src/ownership.rs` (the static checker) and the drop-insertion logic in codegen, as accumulated habits. Every new heap shape (a boxed enum, an SoA field, a loop alias, a `spawn` capture) is a fresh case the habits never covered, discovered at runtime as a double-free.

By `design.md` § Specification Layers, "every owned heap value is freed exactly once, and no place is read after its owner drops" is **guaranteed-semantics** — part of the program's meaning, identical on every surface. An unspecified guaranteed-semantics rule is the *same category error* as the `value_compare`/ordering divergences the LLJIT spike targets: a rule the language guarantees but no single artifact defines.

**The fix:** write the ownership/borrow model down as a checkable spec *first*, prove (or exhaustively fuzz) the drop discipline against it, then make codegen **read its answers** instead of re-deriving them. The bug class doesn't vanish — it **relocates**. Today a kata failure is ambiguous (is the *model* wrong, or the *lowering*?), which is exactly why patching one site leaves the class alive. Once the model is the single source of truth, a failure can only be a lowering mistranslating a known-correct spec — bounded, local, diagnosable.

### Ground truth — how big is this really

- **39 ledger entries (14% of 285) are class-tagged memory-safety bugs:** `double-free` (12), `leak` (21), `drop-elision` (3), `soundness` (3).
- **51% of all ledger titles touch the free / drop / leak / ownership theme** once the heap-shape miscompiles in the untagged and `miscompile` (47) buckets are counted.
- This is the **largest single bug *class*** in the ledger — larger than the run-vs-build split (23%) the LLJIT spike targets, and on a **different axis**: most of these reproduce under `karac build` alone (and some only under default auto-par — [[auto-par-is-third-ab-surface]]), so LLJIT does **not** touch them.

### Relationship to LLJIT productionization

Orthogonal and complementary — the two structural cures for the two biggest recurring taxonomies:

| Spike | Kills | Reproduces on |
|---|---|---|
| LLJIT productionization | run-vs-build tax — two impls of one semantics (23%) | interp vs build |
| **This spike** | drop-soundness whack-a-mole — double-free / UAF / leak (largest single class) | `build` alone **+ auto-par** — LLJIT does not touch it |

This spike touches no interpreter code and can run in parallel with whoever picks up LLJIT.

**Scope note — this is about the codegen *drop-insertion*, not the ownership *checker's* false positives.** The static checker (`src/ownership.rs`) and the runtime drop discipline (codegen) are two consumers of the same unwritten model; the checker's FP class is separately closed ([[ownership-checker-open-false-positives]]). Recall `karac build`/`run` **tolerate** ownership-checker errors by design — only `karac check` gates ([[e2e-ownership-gate-allowlist]]) — so a program can pass `build` and still double-free at runtime. The target here is that runtime discipline.

---

## Current state — what already exists to build on

- **A malloc interposer** for verifying alloc/free gaps ([[vec-of-vec-append-double-alloc]], `index-store-of-heap-vec-elem`) — proven technique, ad-hoc today.
- **The LSan docker gate** — `scripts/lsan-local.sh` runs the Linux ASAN + LeakSanitizer leak gate on macOS via colima/docker ([[lsan-gate-via-colima-docker]]). This is the ground-truth detector the fuzzer will drive.
- **`kara-katas`** — the manual bug-finders that discover these one at a time today. The fuzzer generalizes and automates exactly what the katas do by hand.
- **Partial, scattered specs already exist** — `docs/spikes/caller-retains-param-model.md` and `docs/spikes/general-owned-temp-tracking.md` each pin down *fragments* of the drop discipline (param copy-depth, owned-temp tracking). That they exist as separate spikes, each covering one region, **is itself the evidence** the model is emergent-but-unwritten. Slice 2 consolidates them into one spec.

There is **no** unified written ownership judgment, **no** executable oracle both surfaces consult, and **no** exhaustive fuzzer — drop bugs are found by whichever kata happens to hit the shape.

---

## Ordered slices (risk climbs gradually; slice 1 touches no compiler code)

**Slice 1 — the drop-soundness fuzzer (measure first, build nothing).**
A harness that hunts the bugs katas find today, but exhaustively.
- **Generator:** emits small (≤ ~20-node) well-typed Kāra programs over the heap core — owned heap values (String, Vec, Box, structs/enums with heap fields), moves (by-value pass, return, store-into-aggregate), borrows (ref params, index reads), projections (field/index), and the stressful containers (for-loops over collections, `Option`/enum payloads, `spawn`/`par` captures).
- **Ground-truth detector:** compile each with `karac build`, run under ASan + LSan (`scripts/lsan-local.sh`). ASan catches double-free / UAF at runtime; LSan catches leaks. **No model required — the sanitizer is the judge.** Also compile each under **default auto-par** (the third surface) since some drop bugs diverge only there.
- **Shrinker:** delete-node / simplify-subtree until a failing program stops failing → each bug becomes a minimal, kata-sized repro.
- **Report:** a measured drop-bug rate + a bucketed corpus of minimal repros.
- *Value even if the rest is never done:* a drop-bug hunter that outpaces katas. Touches no compiler code → zero risk.

**Slice 2 — write the rules (the spec, informed by the corpus).**
Consolidate the scattered fragments into one ownership judgment as a doc. For every **place** (root local + path of field/index projections) at every program point, its state:
- **Owned** — sole obligation to free.  **Borrowed** — aliases an owner, no obligation.  **Moved** — obligation transferred out; must not be read, must not be dropped.  **Dead** — uninit.

Plus the transitions (move: Owned→Moved at source, Owned at dest; `&x`: Borrowed; **field-move-out *splits* an aggregate's obligation**; scope-exit drops every still-Owned place once) and the one invariant to hold: *at every point, free-obligations == Owned places; no place carries two obligations; no Moved place is read.* That invariant **is** freed-exactly-once + no-UAF. **Completeness test:** every bug in the slice-1 corpus must be a violation of a *stated* rule; if one isn't, the rules have a hole. (Sanity check the model reaches the right shapes — it must independently explain, e.g., the for-loop-element-escape and boxed-`Option` move-out bugs as one-line consequences.)

**Slice 3 — make the rules executable (the oracle).**
Implement the judgment as a standalone pass computing per-place-per-point ownership state. Now the fuzzer runs *differentially*: model says "drop here / this is Moved"; check codegen did the same. Divergences are the remaining bugs, now attributable to the *lowering* (not the model).

**Slice 4 — codegen reads the oracle (the structural fix).**
Refactor drop-insertion to consume the oracle's facts instead of re-deriving them locally. This is where "checker thinks it's moved, codegen still drops it" becomes **impossible by construction** — one computed set of facts, both surfaces consult it. Land behind the slice-1 fuzzer as the permanent gate.

**Depth of mechanization is a slice-2/3 decision, not committed up front.** Lightweight (a written judgment + executable oracle + property-based fuzzing) captures most of the value without maintenance rot. A proof assistant (Coq/Lean, RustBelt-style) is the heavyweight option — highest assurance, highest maintenance (a proof that rots is worse than none). **Recommendation: do NOT reach for a proof assistant now** — the lightweight path is the right first target; revisit only if the core proves stable and the assurance is wanted.

---

## Gotchas — do not rediscover these

- **LSan misses *reachable* leaks (short-String).** Generated data payloads must be **≥ 36 bytes** or a real leak reads as clean ([[lsan-reachability-short-string-leaks]]).
- **The LSan docker target volume is SHARED across worktrees** → a stale `karac` can be reused after a rebase. Assert `passed + filtered == TOTAL` and rebuild before trusting a run ([[lsan-docker-stale-karac-after-rebase]]).
- **DCE masks non-escaping leaks.** A leak on a value the optimizer proves dead is silently dropped — the fuzzer must make generated values *escape* (print / return / store) so the leak is observable ([[struct-drop-depth-invariant-and-option-blocker]]).
- **One known ASan-arm64 false lead:** 24-byte aggregate load + `extractvalue` mis-lowers field 0 to NULL *only* under arm64-Linux ASan ([[asan-arm64-aggregate-load-extractvalue-null]]). It's a real codegen quirk but ASan-arm64-specific — don't misfile it as a generic drop bug; cross-check against non-ASan build.
- **The E2E suite flakes under concurrent load** — re-run a red fuzzer batch *alone* before trusting it ([[e2e-suite-flakes-under-concurrent-load]]).
- **Corpus → katas, no workarounds.** Every shrunk repro becomes a permanent kata; never route a generator around a shape that crashes — that shape *is* the find ([[katas-are-bug-finders-no-workarounds]]).

## Acceptance criteria

Slice 1: a one-command fuzzer wired to the LSan gate, producing a measured drop-bug rate + a shrunk corpus, that independently rediscovers ≥2 known classes. Slices 2–4: a single written ownership judgment that explains every corpus bug as a stated-rule violation; an executable oracle; codegen drop-insertion consuming the oracle's facts; the slice-1 fuzzer green as the standing gate on macOS arm64 + Linux/LSan.

## Open question (owner sign-off)

Sequencing vs LLJIT and vs the flagship diagnostic-fix work ([[diagnostic-fix-invariant-audit]], `docs/diagnostic-fix-audit.md`) — all three are hardening axes competing for the same attention. This spike is the only one fully independent of the others (touches neither the interpreter nor the diagnostic surface), so it can run in parallel. Slice 1 is cheap and pure-measurement — a low-commitment way to size the problem before committing to slices 2–4.
