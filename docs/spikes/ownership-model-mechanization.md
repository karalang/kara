# Spike: Mechanize the ownership/drop model — stop the drop-soundness whack-a-mole

**Status:** OPEN — **Slices 1–3 DELIVERED**, **Slice 2 judgment DRAFTED**, **Slice 4 DOWN-PAYMENT DELIVERED** (the read-only oracle↔codegen differential; 2026-07-07); Slice 4's structural refactor (codegen *consuming* the oracle) open. Independent of, and parallelizable with, the LLJIT productionization spike (different bug axis).
**Decision date:** 2026-07-06. **Owner call:** worth doing; start with the measurement slice (a fuzzer), *not* the proof.

---

## Slice 1 — DELIVERED (2026-07-07): the drop-soundness fuzzer

**What landed.** `src/bin/drop_fuzz.rs` (a `--features llvm` bin) + `scripts/drop-fuzz.sh` (one-command driver). It generates well-typed heap-core Kāra programs, compiles each with the exact AOT path `karac build` ships, links under **ASan + LSan**, runs it, and lets the **sanitizer be the judge** — no model required, exactly as the slice specified. Touches **no compiler code** (drives the `karac` library the same way `tests/memory_sanitizer.rs` does), so zero risk, as promised.

- **Two build surfaces**, both run per program: **seq** (`concurrency = None`, auto-par dormant) and **autopar** (`concurrency = Some(analysis)`, the default-`karac build` posture). A finding on either is a finding ([[auto-par-is-third-ab-surface]]).
- **Generator** covers the heap core: `String`, `Vec[String]`, `Vec[Vec[String]]`, `Vec[(i64,String)]`, `Vec[Payload]`, a heap-bearing `struct Payload`, `Option[String]`, a recursive boxed `shared enum Tree`, and `Map[String,i64]` / `Set[String]`. Shapes exercised: move-into-aggregate (`push`), owned vs `.iter()` borrow for-loops, tuple-heap-component push, struct destructure (obligation split), `Option` match, owned-param pass + return-move (`echo_vec`), **index-store** overwrite (`v[i] = …`), nested-Vec, Map/Set **key adoption** (`insert` of owned String keys), **`ref` / `mut ref` borrow-forwarding** (`peek(s)` retains, `grow(mut v)` mutates in place), **`par {}`** shared-heap capture, and **`spawn`/`TaskGroup`/`join`** cross-task capture.
  - *Deliberately excluded:* **heap-env closures** (`fn make(k) -> Fn(i64)->i64 { |x| x+k }` in a `Vec[Fn]`). They trip a *known ownership-checker false positive* ("closure ref-capture escapes by return") — the separate `[[ownership-checker-open-false-positives]]` class this spike explicitly scopes out — so the valid-program gate rejects them. They re-enter the generator once that FP is closed.
- **Gotchas honored** (all from this doc's Gotchas section): ≥40-byte payloads (short-String LSan blindness), every value read into a `println`'d `acc` accumulator (DCE + reachable-leak escape), body wrapped in a `while` loop (double-free shows on the 2nd free), and a **valid-program gate** — a program is only *run* if it parses, typechecks, and passes the ownership checker cleanly, so a finding implicates the *lowering*, never buggy generated source.
- **Shrinker**: line-based delta-debug reduces a failing program to a minimal, kata-sized repro (verified: a 20-statement program → the 3-line `Vec[String]`-pushed-into-`Vec[Vec[String]]` double-free core).
- **Report**: measured drop-bug rate + a bucketed, per-surface corpus of shrunk `.kara` repros (`docs/spikes/drop-fuzz-corpus/report.md`).

**Measured drop-bug rate on current HEAD: 0 over ~2500 valid (program, surface) executions** across multiple runs (500 + 400 programs on the initial core, plus 350 on the widened Map/Set + ref-forwarding core, each × 2 surfaces). The known classes in the covered heap-core are **closed** on HEAD — an honest, meaningful measurement, not a vacuous pass (see next).

**Validation — the fuzzer rediscovers ≥2 known classes (acceptance criterion met).** Because HEAD is hardened, "green" was proven non-vacuous by **fault injection** (mutation-testing the detector): two temporary, env-gated, default-dormant knobs were added to codegen, the fuzzer was run, then the knobs were **fully reverted** (not committed — the committed slice-1 artifact touches no compiler code):
- `DROPFUZZ_INJECT_LEAK` — skip the scope-cleanup drain (`emit_scope_cleanup_from`). Fuzzer flagged **`memory-leak`** on both `seq` and `autopar`.
- `DROPFUZZ_INJECT_DOUBLE_FREE` — disable move-source suppression (`suppress_source_vec_cleanup_for_arg_ex`) so caller and callee both free a moved value. Fuzzer flagged **`double-free`** (+ downstream `segv`) on both surfaces, and the shrinker minimized it to the 3-line repro above.

The exact injection diffs are recorded in `docs/spikes/drop-fuzz-corpus/README.md` so the validation is reproducible on demand. This establishes the detector + generator + harness pipeline catches the two headline classes (leak, double-free) the ledger is full of; the 0% on HEAD is therefore a real "these shapes are clean," not a blind spot.

**Value even if Slices 2–4 never happen:** a one-command, seed-reproducible drop-bug hunter that outpaces katas and can be pointed at any future codegen change as a standing gate. Widening the generator (Map/Set keys, `Slice[T]`, closures capturing heap, deeper nesting, layout blocks) is the cheapest next increment and stays pure-measurement.

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

**Slice 2 — write the rules (the spec, informed by the corpus). — DRAFTED 2026-07-07: [`ownership-drop-judgment.md`](ownership-drop-judgment.md).**
The consolidated judgment now exists as one doc: the place-state lattice (Owned/Borrowed/Moved/Dead), the single invariant (freed-exactly-once + no-UAF, stated over places), the transitions (creation, move, borrow, projection/obligation-split, drop-point, tier interaction), the load-bearing **consumption classifier** (`Escape` vs `NonConsuming`, lifted from `caller-retains-param-model.md`), and the design.md temporary-lifetime + drop-ordering rules folded in. The **completeness test passes**: all 39 class-tagged ledger bugs (+ the named untagged ones) are attributed to a stated-rule violation, and the two required sanity checks (for-loop-element-escape, boxed-`Option` move-out) fall out as one-line consequences. Known open edge: borrow-escape for closures is *stated* but not yet mechanized (entangled with the ownership-checker FP that also gates closures out of the Slice-1 fuzzer) — the first thing Slice 3 should add. Original slice text retained below for reference.

Consolidate the scattered fragments into one ownership judgment as a doc. For every **place** (root local + path of field/index projections) at every program point, its state:
- **Owned** — sole obligation to free.  **Borrowed** — aliases an owner, no obligation.  **Moved** — obligation transferred out; must not be read, must not be dropped.  **Dead** — uninit.

Plus the transitions (move: Owned→Moved at source, Owned at dest; `&x`: Borrowed; **field-move-out *splits* an aggregate's obligation**; scope-exit drops every still-Owned place once) and the one invariant to hold: *at every point, free-obligations == Owned places; no place carries two obligations; no Moved place is read.* That invariant **is** freed-exactly-once + no-UAF. **Completeness test:** every bug in the slice-1 corpus must be a violation of a *stated* rule; if one isn't, the rules have a hole. (Sanity check the model reaches the right shapes — it must independently explain, e.g., the for-loop-element-escape and boxed-`Option` move-out bugs as one-line consequences.)

**Slice 3 — make the rules executable (the oracle). — DELIVERED 2026-07-07: [`src/ownership_oracle.rs`](../../src/ownership_oracle.rs) (+ `--oracle-only` in the fuzzer).**
The judgment is now an executable standalone pass (no codegen/`inkwell` dependency): it computes per-place-per-point state (Owned/Borrowed/Moved/Dead), the **consumption classifier** (Escape vs NonConsuming), a per-function **drop schedule** (LIFO), and the **invariant violations**. Unit-tested (13 cases) against the rules and both required sanity shapes — the tests assert the model certifies the historic-bug source *valid* and schedules exactly the drops codegen got wrong (the reference). Wired into the fuzzer as a corpus-wide model self-check: **2000 generated programs, ~19.8k scheduled drops, 0 invariant violations** — model and generator agree. v1 covers the fuzzer's heap-core subset; the two §7 open edges (closure/cross-task captures, NLL shortening) are conservatively handled. **What remains for the "differential vs codegen" is Slice 4's observability hook** — comparing the oracle's schedule to codegen's actual emitted drops. Original slice text retained below.

Implement the judgment as a standalone pass computing per-place-per-point ownership state. Now the fuzzer runs *differentially*: model says "drop here / this is Moved"; check codegen did the same. Divergences are the remaining bugs, now attributable to the *lowering* (not the model).

**Slice 4 — codegen reads the oracle (the structural fix). — DOWN-PAYMENT DELIVERED 2026-07-07: the read-only oracle↔codegen differential.**
The bounded, zero-risk half of Slice 4 — the piece Slice 3 flagged as remaining ("differential vs codegen") — now exists:

- **`src/codegen/drop_obs.rs`** — a thread-local, off-by-default recorder. Armed only by the differential harness (`begin`/`take`); the production `karac` / test / REPL path pays one relaxed `is_some` check per emitted cleanup and is otherwise untouched.
- **The single-seam tap** in `codegen/runtime.rs`'s `emit_cleanup_action_at` — the *sole funnel* every actually-emitted drop passes through (all three drains — normal `drain_top_frame_with_emit`, function-exit `emit_scope_cleanup_from`, error-path `emit_scope_cleanup_for_error_path` — route through it). It records `(function, place)` for each compiler-internal heap drop. Place names are recovered by reverse-mapping the action's alloca through codegen's `variables` (name→slot) table, so Map/Set handles and pattern temporaries resolve to their real binding names, not the slot's LLVM name. **Purely observational** (`&self`, no IR mutation) — arming it cannot change emission.
- **`src/drop_differential.rs`** (lib, `--features llvm`) — the differential core as a reusable API: `differential_check(src) -> DiffOutcome` (`Checked { drops_checked, divergences } | CaptureEdge | Invalid`). It compiles each program in-process (`compile_to_ir`, seq surface) with the recorder armed and diffs the oracle's per-function drop schedule against codegen's emitted set. Reports a **missing drop** (oracle scheduled it, codegen emitted no cleanup → a leak) localized to `(function, place)`.
- **`drop_fuzz --differential`** — runs `differential_check` over the generated corpus. Result: **330 programs checked, 2199 scheduled drops, 0 divergences** (670 skipped as the §7 capture edge, below). `--explain S` prints one seed's per-function drop sets for triage.
- **`tests/drop_differential.rs`** (`--features llvm`) — the **standing regression gate**: 11 canonical heap-core shapes (owned String, move-into-vec, struct/Map/Set locals, destructure, nested Vec, borrow-param, the Option-match boundary, the capture edge, an ownership-error case), each asserting codegen covers the oracle's schedule. This is the net the structural refactor lands behind — a codegen change that drops a scheduled place on the wrong path turns one of these red. Runs in the CI llvm tier; needs no archives / `cc` (pure lowering, nothing linked or run).

Three alignment rules make the differential sound — each pinned down by a false-positive it eliminated (792 → 392 → 111 → 0 divergences as they went in):
  1. **Oracle on the *surface* tree** (before `lower`), matching the oracle's model + unit tests; otherwise `lower`'s desugared temporaries (`iv2`, `v5`) get scheduled but never match codegen.
  2. **Local drops only, not parameters** — codegen frees a bare `String`/`Vec`/`Map` param **caller-side** (caller-retains), whereas the oracle models the owned param as callee-owned. Both free exactly once, across the call boundary; a per-callee compare would false-positive.
  3. **Skip the §7 closure/cross-task capture edge** — the oracle walks a closure with Read role (conservative, never moves the captured parent), so it keeps a `spawn`/`par`-captured heap value Owned and schedules a drop codegen elides (the task frees it). Documented model-conservatism, not a codegen leak; those programs (68% of the corpus, from the `spawn`/`par` transforms) are excluded and **counted**, not silently dropped.

Only the **missing-drop (leak)** direction is checked. The extra-drop (double-free) direction is *not* emit-time observable: codegen routinely neutralizes a moved-out value's drop with a runtime null/cap guard while keeping the cleanup action, so at emit time a guarded no-op is indistinguishable from a real free — the ASan/LSan run (Slice 1) stays the double-free authority. **Non-vacuity proof** (mirrors Slice 1's reverted fault-injection): with `KARAC_DROPOBS_SILENCE=1` the recorder no-ops and the differential reports the *entire* schedule as missing (137/137 over 22 programs); off, 0. Green-off + red-on proves the gate observes real drops, not vacuously.

**The structural fix — in progress.** Refactor drop-insertion to *consume* the oracle's facts instead of re-deriving them locally — this is where "checker thinks it's moved, codegen still drops it" becomes **impossible by construction**. The differential is the safety net it lands behind: it already proves the two agree today, so it catches any drift.

- **Increment 1 — DELIVERED 2026-07-08: the inline self-check (`KARAC_ORACLE_DROP_CHECK`).** `compile_program` now *runs the ownership oracle on its own (lowered) input tree* and, at module end, verifies codegen's emitted drop set (via `drop_obs`) covers the oracle's per-function schedule — the same missing-drop check the external differential does, but inline, on whatever real program is compiled. This is the foundation the consumption step builds on: codegen now **holds the oracle's facts**. Key enabler, validated first: the oracle run on the *lowered* tree agrees with codegen's emitted drops just as the surface-tree run does (0 divergences on the corpus, identical `drops_checked`), so **no surface tree needs threading into codegen** — it analyzes the tree it already has. That lowered-tree agreement is now **locked as a standing test** (`tests/drop_differential.rs::lowered_oracle_agrees_with_codegen`, via `differential_check_on(_, OracleTree::Lowered)`) — if lowering ever introduced a droppable temporary the two disagree on, the self-check's no-plumbing design would need revisiting, and this goes red first. Off by default (one env probe, zero behavior change), warn-only (never fails a build), and it yields to an external arming so it never perturbs the fuzzer/differential. Params excluded (caller-retains); §7 captures may warn benignly.
- **Increment 2 — DELIVERED 2026-07-08: oracle control-flow soundness (branch-state merge), the prerequisite consumption exposed.** The first *consumption* attempt (route move-into-container drop suppression through the oracle) was **aborted at design time** by the verify-first discipline: probing found the oracle **unsound on conditional moves** — `if cond { v.push(s); }` marked `s` `Moved` with *no branch merge*, so it scheduled no drop for `s` even though the else-path must free it. Consuming *that* oracle would make codegen elide `s`'s drop → leak on `!cond`, and **neither guard would catch it**: the differential goes tautological the moment codegen consumes the oracle, and the fuzzer generates no conditional-move shapes. So consumption is unsafe until the oracle is sound. Fixed the root cause instead: `analyze` now snapshots outer binding states around every branch (`if`/`if let`/`match`/`while`/`loop`/`for`) and merges them — a place stays `Moved` (drop-elidable) only if `Moved` on **every** path; any disagreement collapses to `Owned`, so the drop is scheduled and codegen's runtime cap/null guard makes the over-scheduled conditional drop correct (under-scheduling would leak). Three regression tests pin it (conditional-move keeps the source scheduled; both-branch move disarms it; one-arm match keeps it). All guards hold: differential still 0, oracle self-check 0 violations over ~5k drops, ASan clean, 16 oracle + 12 gate tests green. This is oracle-only — codegen untouched — so it carries no drop-path regression risk while removing the blocker.
- **Next — consumption, now unblocked.** With the oracle sound on conditional moves, replacing the scattered suppressor re-derivation (`call_dispatch.rs` `suppress_*`, cap-zeroing) with reads of the schedule is now *safe by construction* — the oracle only says `Moved` when a place is gone on every path. Caveat carried forward: once codegen consumes the oracle the differential can no longer guard that decision (it compares codegen *to* the oracle), so each consumption increment must be gated by the **ASan/LSan fuzzer** (runtime truth) — ideally after widening the generator to emit conditional-move shapes, which the corpus currently lacks.

Original slice text retained below.

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

Slice 1 ✅ (2026-07-07): a one-command fuzzer wired to the LSan gate, producing a measured drop-bug rate + a shrunk corpus, that independently rediscovers ≥2 known classes (leak + double-free, via reverted fault-injection — see the *Slice 1 — DELIVERED* section). Slices 2–4: a single written ownership judgment that explains every corpus bug as a stated-rule violation; an executable oracle; codegen drop-insertion consuming the oracle's facts; the slice-1 fuzzer green as the standing gate on macOS arm64 + Linux/LSan.

## Open question (owner sign-off)

Sequencing vs LLJIT and vs the flagship diagnostic-fix work ([[diagnostic-fix-invariant-audit]], `docs/diagnostic-fix-audit.md`) — all three are hardening axes competing for the same attention. This spike is the only one fully independent of the others (touches neither the interpreter nor the diagnostic surface), so it can run in parallel. Slice 1 is cheap and pure-measurement — a low-commitment way to size the problem before committing to slices 2–4.
