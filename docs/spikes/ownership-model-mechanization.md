# Spike: Mechanize the ownership/drop model — stop the drop-soundness whack-a-mole

**Status:** COMPLETE (core) — **Slices 1–4 DELIVERED** (2026-07-07/08). Slice 4's original "codegen *consumes* the oracle" goal is **retired in favour of verified independence** (see the Decision at the end of Slice 4) — literal consumption would make the verification tautological and give up more than it buys. The oracle is sound on conditional moves (branch merge) and models spawn/`par` captures, so the differential **checks 100% of the corpus at 0 divergences**. Every *tractable, in-scope* edge is closed; the residuals are not deferred work but genuine boundaries: **stored/heap-env closure escape** (blocked on a separate ownership-checker false positive — tracked as [B-2026-07-08-2](../bug-ledger.md) — not a model gap), the **NLL last-use floor** (a deliberate v1 boundary — the model is a ceiling by design), **match-payload place-equivalence** (documented, sound-by-under-approximation, low value), and **oracle coverage beyond the heap-core subset** (generics/traits — a tool-growth effort, its own scope). Independent of, and parallelizable with, the LLJIT productionization spike (different bug axis).
**Decision date:** 2026-07-06. **Owner call:** worth doing; start with the measurement slice (a fuzzer), *not* the proof.

---

## Slice 1 — DELIVERED (2026-07-07): the drop-soundness fuzzer

**What landed.** `src/bin/drop_fuzz.rs` (a `--features llvm` bin) + `scripts/drop-fuzz.sh` (one-command driver). It generates well-typed heap-core Kāra programs, compiles each with the exact AOT path `karac build` ships, links under **ASan + LSan**, runs it, and lets the **sanitizer be the judge** — no model required, exactly as the slice specified. Touches **no compiler code** (drives the `karac` library the same way `tests/memory_sanitizer.rs` does), so zero risk, as promised.

- **Two build surfaces**, both run per program: **seq** (`concurrency = None`, auto-par dormant) and **autopar** (`concurrency = Some(analysis)`, the default-`karac build` posture). A finding on either is a finding ([[auto-par-is-third-ab-surface]]).
- **Generator** covers the heap core: `String`, `Vec[String]`, `Vec[Vec[String]]`, `Vec[(i64,String)]`, `Vec[Payload]`, a heap-bearing `struct Payload`, `Option[String]`, a recursive boxed `shared enum Tree`, and `Map[String,i64]` / `Set[String]`. Shapes exercised: move-into-aggregate (`push`), owned vs `.iter()` borrow for-loops, tuple-heap-component push, struct destructure (obligation split), `Option` match, owned-param pass + return-move (`echo_vec`), **index-store** overwrite (`v[i] = …`), nested-Vec, Map/Set **key adoption** (`insert` of owned String keys), **`ref` / `mut ref` borrow-forwarding** (`peek(s)` retains, `grow(mut v)` mutates in place), **`par {}`** shared-heap capture, and **`spawn`/`TaskGroup`/`join`** cross-task capture.
  - *Deliberately excluded:* **heap-env closures** (`fn make(k) -> Fn(i64)->i64 { |x| x+k }` in a `Vec[Fn]`). They trip a *known ownership-checker false positive* ("closure ref-capture escapes by return"), now tracked as **[B-2026-07-08-2](../bug-ledger.md)** (`surface: ownership`, open) — the `[[ownership-checker-open-false-positives]]` class this spike explicitly scopes out — so the valid-program gate rejects them. They re-enter the generator once that FP is closed. Repro + root cause (Copy-scalar capture defaults to `ref`, escape check has no by-copy exemption; `src/ownership/closure_escape.rs`) are in the ledger entry.
- **Gotchas honored** (all from this doc's Gotchas section): ≥40-byte payloads (short-String LSan blindness), every value read into a `println`'d `acc` accumulator (DCE + reachable-leak escape), body wrapped in a `while` loop (double-free shows on the 2nd free), and a **valid-program gate** — a program is only *run* if it parses, typechecks, and passes the ownership checker cleanly, so a finding implicates the *lowering*, never buggy generated source.
- **Shrinker**: line-based delta-debug reduces a failing program to a minimal, kata-sized repro (verified: a 20-statement program → the 3-line `Vec[String]`-pushed-into-`Vec[Vec[String]]` double-free core).
- **Report**: measured drop-bug rate + a bucketed, per-surface corpus of shrunk `.kara` repros (`docs/spikes/drop-fuzz-corpus/report.md`).

**Measured drop-bug rate on current HEAD: 0 over ~2500 valid (program, surface) executions** across multiple runs (500 + 400 programs on the initial core, plus 350 on the widened Map/Set + ref-forwarding core, each × 2 surfaces). The known classes in the covered heap-core are **closed** on HEAD — an honest, meaningful measurement, not a vacuous pass (see next).

> **Scope correction (2026-07-31).** That 0 is a statement about *implicitly*-dropped
> heap, and only that. The corpus contained no `impl Drop` at all, so the number said
> nothing about user destructors — and the sanitizers could not have told us if it had:
> a Drop body that runs twice, or never, while the heap is still freed correctly leaves
> ASan and LSan completely silent. Over the same period the ledger accumulated six
> user-`impl Drop` bugs, none of them found here; **0 of 812 ledger entries name this
> fuzzer as their source.** The grammar has since gained a `Tracked` type with a
> token-printing `impl Drop`, a drop-log oracle asserting per-tag construct/drop
> balance, displacement transforms, and the interpreter as a third surface. On its
> first 6-program run the widened corpus rediscovered open bug B-2026-07-30-11 from
> four independent directions. Read the original 0 as "the implicit-drop heap core is
> clean", which is what it measured, not "drop insertion is clean".

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

Three alignment rules make the differential sound — each pinned down by a false-positive it eliminated (792 → 392 → 111 → 0 divergences as they went in). A fourth (fixed-array locals, B-2026-08-23-2) arrived later and is neutral on this corpus:
  1. **Oracle on the *surface* tree** (before `lower`), matching the oracle's model + unit tests; otherwise `lower`'s desugared temporaries (`iv2`, `v5`) get scheduled but never match codegen.
  2. **Local drops only, not parameters** — codegen frees a bare `String`/`Vec`/`Map` param **caller-side** (caller-retains), whereas the oracle models the owned param as callee-owned. Both free exactly once, across the call boundary; a per-callee compare would false-positive.
  3. **Skip the §7 closure/cross-task capture edge** — the oracle walks a closure with Read role (conservative, never moves the captured parent), so it keeps a `spawn`/`par`-captured heap value Owned and schedules a drop codegen elides (the task frees it). Documented model-conservatism, not a codegen leak; those programs (68% of the corpus, from the `spawn`/`par` transforms) are excluded and **counted**, not silently dropped.

**A zero here is a measurement, not a property — and it has gone red since (B-2026-08-23-5).** The corpus sat at 94 divergences (186 at `--count 400`) against this doc and the module header both still reading 0, because the standing gate that would have caught it could not reach the class: no case in `tests/drop_differential.rs` declared an `impl Drop`, and an `impl Drop` is what routes a binding through `fire_due_user_drops` (NLL live-range-end firing) instead of the scope-exit drain — where one arm emitted the drop without recording it. The other 6 were the oracle reading an enum-variant constructor's argument rather than moving it. Both fixed, corpus back to 0 at both sizes, Drop-bearing cases added to the curated gate. The lesson generalizes past this bug: **re-run the corpus rather than citing a number from this file**, and when a curated gate and the corpus disagree, believe the corpus.

Only the **missing-drop (leak)** direction is checked. The extra-drop (double-free) direction is *not* emit-time observable: codegen routinely neutralizes a moved-out value's drop with a runtime null/cap guard while keeping the cleanup action, so at emit time a guarded no-op is indistinguishable from a real free — the ASan/LSan run (Slice 1) stays the double-free authority. **Non-vacuity proof** (mirrors Slice 1's reverted fault-injection): with `KARAC_DROPOBS_SILENCE=1` the recorder no-ops and the differential reports the *entire* schedule as missing (137/137 over 22 programs); off, 0. Green-off + red-on proves the gate observes real drops, not vacuously.

**The structural fix — RESOLVED to verified independence (see Decision below).** The original plan was to refactor drop-insertion to *consume* the oracle's facts instead of re-deriving them locally ("impossible by construction"). Increments 1–3 built the apparatus for that; scoping the final step then established that literal consumption is the wrong target and the independent-and-cross-checked architecture is the better end state. The increments below are the delivered work; the **Decision** at the end records why consumption is retired.

- **Increment 1 — DELIVERED 2026-07-08: the inline self-check (`KARAC_ORACLE_DROP_CHECK`).** `compile_program` now *runs the ownership oracle on its own (lowered) input tree* and, at module end, verifies codegen's emitted drop set (via `drop_obs`) covers the oracle's per-function schedule — the same missing-drop check the external differential does, but inline, on whatever real program is compiled. This is the foundation the consumption step builds on: codegen now **holds the oracle's facts**. Key enabler, validated first: the oracle run on the *lowered* tree agrees with codegen's emitted drops just as the surface-tree run does (0 divergences on the corpus, identical `drops_checked`), so **no surface tree needs threading into codegen** — it analyzes the tree it already has. That lowered-tree agreement is now **locked as a standing test** (`tests/drop_differential.rs::lowered_oracle_agrees_with_codegen`, via `differential_check_on(_, OracleTree::Lowered)`) — if lowering ever introduced a droppable temporary the two disagree on, the self-check's no-plumbing design would need revisiting, and this goes red first. Off by default (one env probe, zero behavior change), warn-only (never fails a build), and it yields to an external arming so it never perturbs the fuzzer/differential. Params excluded (caller-retains); §7 captures may warn benignly.
- **Increment 2 — DELIVERED 2026-07-08: oracle control-flow soundness (branch-state merge), the prerequisite consumption exposed.** The first *consumption* attempt (route move-into-container drop suppression through the oracle) was **aborted at design time** by the verify-first discipline: probing found the oracle **unsound on conditional moves** — `if cond { v.push(s); }` marked `s` `Moved` with *no branch merge*, so it scheduled no drop for `s` even though the else-path must free it. Consuming *that* oracle would make codegen elide `s`'s drop → leak on `!cond`, and **neither guard would catch it**: the differential goes tautological the moment codegen consumes the oracle, and the fuzzer generates no conditional-move shapes. So consumption is unsafe until the oracle is sound. Fixed the root cause instead: `analyze` now snapshots outer binding states around every branch (`if`/`if let`/`match`/`while`/`loop`/`for`) and merges them — a place stays `Moved` (drop-elidable) only if `Moved` on **every** path; any disagreement collapses to `Owned`, so the drop is scheduled and codegen's runtime cap/null guard makes the over-scheduled conditional drop correct (under-scheduling would leak). Three regression tests pin it (conditional-move keeps the source scheduled; both-branch move disarms it; one-arm match keeps it). All guards hold: differential still 0, oracle self-check 0 violations over ~5k drops, ASan clean, 16 oracle + 12 gate tests green. This is oracle-only — codegen untouched — so it carries no drop-path regression risk while removing the blocker.
- **Increment 3 — DELIVERED 2026-07-08: conditional-move fuzzer coverage (the consumption guard).** The generator now emits the conditional-move shape the straight-line corpus lacked — `conditional_move_str_into_vec` → `if round % 2 == 0 { v.push(s); }`, so `s` is moved on half the loop iterations and left owned on the rest. This is the **runtime (ASan/LSan) counterpart to the oracle's branch merge**, and the guard the eventual consumption needs (once codegen consumes the oracle, the differential can't guard that decision — it compares codegen *to* the oracle — so ASan/LSan is the remaining independent check). Runtime confirms the merge fix: these programs are ownership-valid, **ASan/LSan-clean** (codegen frees `s` exactly on the not-moved path — no leak — and not on the moved path — no double-free), and the inline self-check reports OK (the oracle now schedules `s`, codegen covers it). ~5% of generated programs carry the shape (thousands of conditional-path executions per run). Generator + fuzzer only — no codegen change.
- **Edge close (2026-07-08): the capture edge is closed — differential coverage 32% → 100%.** The largest §7 open edge (closure captures) previously forced the differential to *skip* every `spawn`/`par` program (~68% of the corpus). Both forms are now modelled: a **`spawn`** capture demotes the captured heap binding to `Borrowed` (auto-promoted shared/RC — no scope-drop obligation, later reads/captures valid), matching codegen's RC/join free; a **`par {}`** block captures `shared struct` values whose scope-exit `RcDec` *is* the drop the oracle already schedules, so they agree with no special handling. The differential now checks **1000/1000 programs, 0 skipped, 0 divergences** — the capture skip is removed entirely. Oracle-only change (no codegen risk); 17 oracle + 13 gate tests, oracle self-check 0 violations over ~4.6k drops, ASan clean, fmt + clippy clean. This tightens the verified-independence apparatus below without touching the independence itself. Remaining §7 items are now only the *stored/heap-env* closure escape procedure (blocked on an ownership-checker FP, not exercised by the fuzzer), the NLL floor (a deliberate v1 boundary), and the match-payload place-equivalence (documented, low-value).

### Decision (2026-07-08): the end state is **verified independence**, not literal consumption

Scoping the actual consumption — codegen deriving its drop decisions *from* the oracle instead of independently — surfaced three findings that, together, say literal consumption is the **wrong target**. The apparatus built above (increments 1–3) is the better end state, and this spike's original "codegen reads the oracle" framing should be retired in its favour.

1. **Consumption destroys the only thing that makes drop-soundness verifiable.** Every guard here — the `drop_fuzz --differential` gate, the inline `KARAC_ORACLE_DROP_CHECK` self-check — works *because codegen and the oracle derive drops independently*; comparing two independent derivations is what detects a divergence. The moment codegen derives its drops *from* the oracle, the comparison is `X == X`: identically 0, guarding nothing. This is not hypothetical — that independence is exactly what caught the oracle's own conditional-move unsoundness in increment 2. Had codegen already been consuming the oracle, that model bug would have become a silent **codegen leak with no detector**. On a drop path, a *detector* of divergence is worth more than a *preventer*, because the preventer can only enforce agreement with a model that is itself sometimes wrong.

2. **Codegen's runtime guards are strictly more precise than the oracle's static schedule.** Codegen suppresses a moved value's drop with a runtime cap/null guard that decides *per execution path* (free iff not-moved-on-this-path). The oracle's schedule is a static ceiling (§3.5) and, by the increment-2 merge rule, deliberately *over-*schedules conditional moves and relies on that same runtime guard for correctness. So the oracle cannot replace the guard; even under "consumption," codegen must keep it. Consumption could therefore only remove *static registration for unconditional moves* — a thin sliver, not the `suppress_*` / cap-zeroing scatter the original slice imagined eliminating (most of which handles conditional/contextual cases that need the runtime guard regardless).

3. **The bug class consumption would prevent is already empirically absent.** The differential reports **0 divergences** across the entire corpus and the standing gate — codegen does not drop what the (now-sound) oracle says is moved. Consumption would trade a working detector for a preventer of a bug that is not occurring, and forfeit the detector's ability to catch *future oracle* bugs in the process.

**Therefore the delivered architecture is the end state:** an independent, conditional-move-sound oracle; codegen deriving drops independently with more-precise runtime guards; and **three independent nets** continuously cross-checking them — the differential (missing-drop / leak), the inline self-check (any real program compiled with the env flag), and the ASan/LSan fuzzer (double-free + leak, now conditional-move-aware). "Impossible by construction" reads stronger than "caught by construction," but for a path where the two derivations must stay independent to be checkable at all, independence + detection beats consumption + tautology.

**Escape hatch, if the "by construction" property is ever specifically wanted:** the one safe, non-regressing slice is *unconditional-move static elision* — skip registering a drop the oracle proves `Moved` on all paths — gated by the ASan/LSan fuzzer. It buys the property for that sliver at the cost of the differential's guard coverage on it; recorded here as available but **not recommended**.

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

Slice 1 ✅ (2026-07-07): a one-command fuzzer wired to the LSan gate, producing a measured drop-bug rate + a shrunk corpus, that independently rediscovers ≥2 known classes (leak + double-free, via reverted fault-injection — see the *Slice 1 — DELIVERED* section). Slices 2–4 ✅ (2026-07-08): a single written ownership judgment that explains every corpus bug as a stated-rule violation ✅; an executable oracle ✅ (now sound on conditional moves). The original "codegen drop-insertion consuming the oracle's facts" criterion is **superseded** — see the Slice 4 *Decision*: consuming the oracle would make the very verification tautological, so the delivered end state is *verified independence* (independent oracle + codegen, cross-checked by the differential, the inline self-check, and the fuzzer). The slice-1 fuzzer is green as the standing gate on macOS arm64 + Linux/LSan, plus the `tests/drop_differential.rs` gate in the CI llvm tier.

## Open question (owner sign-off)

Sequencing vs LLJIT and vs the flagship diagnostic-fix work ([[diagnostic-fix-invariant-audit]], `docs/diagnostic-fix-audit.md`) — all three are hardening axes competing for the same attention. This spike is the only one fully independent of the others (touches neither the interpreter nor the diagnostic surface), so it can run in parallel. Slice 1 is cheap and pure-measurement — a low-commitment way to size the problem before committing to slices 2–4.

---

## Re-measurement 2026-09-10: the corpus was green because it could not build the shapes

This spike closed as COMPLETE (core) on 2026-07-08 with the differential at
**0 divergences**. Two months on, the drop-bug class it targets is still the
open queue: of **42 open ledger rows, ~30 (71%) are drop-placement/ownership**,
the family totals **953 rows of which 52% are follow-ups to an earlier row**,
and follow-up chains run **20 deep** — the deepest is one week of 2026-08-28 →
09-04, every link fixed, every fix spawning the next spelling. Codegen carries
**93** `disarm_* / suppress_* / retract_*` functions, several of them the same
rule duplicated per syntactic context (`disarm_moved_bare_tuple_elem_bodies`
and `..._for_block`; four variants of `disarm_tuple_elem_bodies_at`).

A green gate and an unshrinking bug class cannot both be health signals. The
cause is coverage, and it is the **second** time — `drop_differential.rs`
already records the corpus sitting at 94 against a doc reading 0 because "no
curated shape declared an `impl Drop`". Three distinct blindness mechanisms,
each measured:

1. **Generator vocabulary.** `Array` appeared *nowhere* in `drop_fuzz`'s type
   enum while five open rows were filed against `Array` element drops in one
   week (B-2026-09-09-24, -09-10-7, -8, -26, -27). Hand-fed the nested shape,
   the differential reports it correctly — `Divergence { function: "main",
   place: "aa" }` — so the tooling was never blind, the corpus just could not
   build one. **Fixed here:** four `Array` producers added. The differential
   went **0 → 30 divergences** (116 programs, 581 drops) and the ASan/drop-log
   run found **60 valid repros** (38 the `Option[Array[Tracked,2]]` of -27, 22
   the `Array[Array[Tracked,2],2]` of -8/-26). The two *correct* Array shapes
   added alongside them produce zero findings — no false alarms.

2. **Oracle scope.** A `match` over an owned heap local takes the compared
   schedule from 1 to 0 — including `Some(_)`, which binds nothing. `match` is
   how Kāra consumes every `Option`/`Result`/enum, so the 0-divergence record
   is substantially vacuous over the family it would be most valuable on. This
   is the residual this doc calls "match-payload place-equivalence …
   low value"; the measurement says it is where the bugs are.
   Filed **B-2026-09-10-31**.

3. **Validity decided at the wrong boundary, in both directions.** A program
   that typechecks but fails `compile_to_ir` returns `DiffOutcome::Invalid` and
   is silently uncounted — so the gate excludes exactly the shapes codegen is
   worst at, and codegen-refusal is its own open row class
   (**B-2026-09-10-30**). Mirror image on the fuzzer side: the shrinker deletes
   a `let` and saves a program that no longer compiles as a repro, because "a
   constructed `Drop` body never ran" is vacuously true of a program that never
   runs — the empty program is the shrink predicate's fixed point, 2 of 62 in
   one run (**B-2026-09-10-33**).

**What this does not argue.** Nothing here reopens the 2026-07-08 Decision that
the end state is *verified independence* rather than codegen literally
consuming the oracle; that argument (consumption makes the verification
tautological) is untouched. The finding is narrower and cheaper to act on: the
verification is only as good as its corpus, and the corpus needs widening
before the 0 means what it is read to mean. Extending the generator touches no
compiler code — the Slice 1 property — so it stays the low-risk half.

**Next widenings, in open-row order:** generic by-value payloads
(B-2026-09-10-22), nested tuples inside arm bindings (-21), `shared`/`par` enum
payloads (-11, -20). Each is a `Ty` variant, a producer, a sink arm and a
prelude helper — the pattern the `Array` block now demonstrates.

## Widening 2026-09-11: user and generic enums — and the first bug the corpus found on its own

The `Array` widening above closed one vocabulary hole and immediately exposed
the next one. Two oracle fixes landed against **user** and **generic** enums
(B-2026-09-10-31's match-payload projection, B-2026-09-11-1's
instantiation-keyed heap-ness) and **neither was fuzzable**: the generator's
entire enum vocabulary was `Option`, `Result` and the `shared enum Tree`, so a
user enum's arm and a generic's type ARGUMENT never appeared in a generated
program. Both fixes shipped validated by unit tests and hand-fed differential
cases while the corpus reported its usual 0 — silence, not confirmation, which
is the same reading error this section was written to stop.

**Four `Ty` variants close it**, following the `Array` block's pattern:
`Parcel` (a user enum carrying all three arm shapes the projection must tell
apart — a multi-payload variant that splits positionally, a scalar payload that
must schedule nothing, and a true unit variant), `Slot[String]` and
`Slot[Tracked]` (a user generic enum at a heap type argument, the second one
drop-log observable), and `Wrap[Tracked]` (the generic struct twin).

Half the generic-enum sinks **borrow** rather than match, and that half is the
one that matters: a matched scrutinee is moved, so its drop belongs to the arm
bindings, while a peeked one is still live at scope exit and is scheduled only
if `Slot[String]` is read as heap through its type argument. Matching alone
would have left the instantiation path as unfuzzed as it was before.

**The coverage is now real and measurable.** 77 of 396 oracle-scheduled drops
over 60 programs (19%) sit on the new shapes, against 0 before; 40 of 60
programs build at least one. The differential stays at **0 divergences** — over
185 programs / 1313 drops at seed base 1 and 287 / 2103 at base 2000 — but it
is now a 0 that had something to check.

**And the sanitizer half found a bug the corpus could not previously reach**
(B-2026-09-11-3): a user-declared **generic** enum leaks its payload whenever
the value reaches scope exit. The type discriminators are what make it a report
rather than a puzzle — in one identical surrounding program:

    Slot[String]   (user generic enum)     70 B x 40 rounds LEAKED
    StrSlot        (the same, monomorphic) clean
    Option[String] (the built-in generic)  clean
    Wrap[String]   (user generic STRUCT)   clean
    plain String                           clean

A generic enum erases its payload area to the width of the bare parameter `T`
— one word — so a three-word `String` monomorph is heap-boxed, and the box drop
reclaimed the envelope while nothing owned the buffer inside it. That is the
same declaration-vs-instantiation axis B-2026-09-11-1 fixed in the oracle, one
layer over in codegen: the erasure is what causes the BOXING.

**The CALL-SHAPE half of that row was filed wrong, and the correction belongs
here because it is a lesson about this harness rather than about that bug.** It
reported that `ref` with no match in the callee was clean while `ref` + match
leaked, and concluded that the match on a borrowed generic enum disarms the
caller's drop. The leak had been measured under `KARAC_OPT_LEVEL=0`; those
controls had not. At the default `-O2` LLVM deletes an allocation nothing
observes, so both "clean" cells were reporting the optimizer. At `-O0` they leak
identically and the match is not the axis at all — the two cells that really are
clean (an owned callee, a match at the call site) are clean because the payload
is MOVED OUT and the arm binding frees it. The row's whole "context sensitivity"
section went the same way: the neighbouring locals it named as required were
making the allocation observable, not selecting a codegen path.

**A control measured at a different optimization level from its subject is not a
control.** CLAUDE.md already says an `-O2`-only zero is evidence of nothing; what
this adds is that the rule binds the CONTROLS just as hard as the measurement,
and that a probe harness which sweeps cells should pin the opt level once for the
whole sweep rather than per cell. The fixture that closed the row hit the same
trap one more time — literal-seeded and `len()`-only, it folded away at `-O2` and
passed against the unfixed compiler — and needed an opaque seed, a byte-level
read, and an allocation floor before it measured anything.

**Following that row's remainder produced three more defects, and the shape of
how they were found is the point.** B-2026-09-11-4 asked for four payload shapes
(a tuple, an `Array`, an `Option[String]`, a user generic struct) that still
leaked inside a generic enum's box. Rather than fix the resolver and re-measure
the one cell the row quoted, the sweep was widened to a **4 x 4 matrix** — each
payload shape crossed with four call shapes (never read, matched at the call
site, through an owned callee, through a `ref` callee) — plus a `String` row and
a plain-user-struct row as controls, every cell at `KARAC_OPT_LEVEL=0` with one
PASS/FAIL line of its own. The row had recorded four leaking cells. The matrix
found **ten**, and three things the row could not have said:

* an `Option[String]` payload matched out was ALREADY clean, which made it the
  cell that would have turned into a double free had the retraction not fired —
  the row's own stated hazard, and the only shape where it had teeth;
* six move-out cells leak for a reason the resolver cannot reach (the arm
  binding owns nothing), which is a second hole, not the same one;
* one cell was not a leak at all but a **miscompile** — a tuple payload read
  through a `ref` parameter returns garbage, and its all-scalar `(i64, i64)`
  variant allocates nothing whatsoever, so no amount of leak-chasing would have
  surfaced it.

**The generalization: a row's own cell list is a hypothesis, not a test plan.**
The matrix cost one generator and one runner script and it found more than twice
what the row described, including a defect in a different class. It also caught
the fix's own risk — a body-count control across four user-`Drop` payload shapes,
run before and after, is what turned "this does not move a `Drop` body" from an
argument about which emitter is memory-only into four measured pairs. That
control then found the third defect on its own: the body counts were WRONG in
both directions across the three backends, on every shape, before the fix
touched anything.

**A CLASS emerged from following those remainders, and naming it is worth more
than any one of the fixes.** Five separate backend sites have now been found
losing a TUPLE, every one for the same structural reason: the site matches on
`TypeKind::Path` (or looks the type up by NAME) and a tuple has no path spelling
and no name to look up, so it falls to an `_ => <nothing>` tail. In order of
discovery, none of them looked for:

| site | what it lost |
|---|---|
| `enum_boxed_payload_interior_drop`'s first guard | a boxed tuple payload's interior (B-2026-09-11-4) |
| `declared_mismatches_word`'s `llvm_type_for_name` | a tuple binding read as the payload WORD (B-2026-09-12-4) |
| …and the two guards beside it (`ok_padded_primitive`, `ok_single_word`) | the same, by two different routes |
| `enum_drop_kind_for_type_expr` | an enum's tuple payload, never freed (B-2026-09-12-8) |
| `type_expr_word_aligned`'s narrow-element reject | `(bool, String)`, still open (B-2026-09-12-10) |

Each was met rather than sought, which is the inefficiency. The pattern is now
attested well enough to grep for directly — every `match … kind { Path(..) => …,
_ => … }` over a payload, field or binding type in the backend is a candidate —
and doing that sweep once is probably worth more than chasing the next instance.
The same question should be asked of `Array`, whose two spellings (`TypeKind::Array`
from a literal's inferred type, `Path(["Array"], …)` from an annotation) make it
the *other* type a name-keyed guard mishandles, in its own way: a guard keyed on
the kind alone silently misses every ANNOTATED array, which B-2026-09-06-49
recorded and `array_elem_and_len` exists to prevent.

**A second transferable rule came out of the same work, about fixes rather than
searches.** B-2026-09-12-8's drop, added alone, turned its leak into a double
free the moment the enum was passed by value — and *every leak cell still read
clean*. A leak-only sweep would have shipped it. Any fix that gives something a
new owner needs a cell where the value is COPIED, not just cells where it dies;
the by-value-parameter cell is the cheapest one that exercises the copy/drop
symmetry these classifiers keep warning about.

Note what the **differential** says about that program: nothing. It reports 0,
correctly — codegen does emit a cleanup action for the binding, so the emitted
set covers the schedule. The differential checks that a drop is SCHEDULED AND
EMITTED, not that the emitted one frees the right memory; the sanitizer is the
oracle for that half, which is why Slice 1 keeps both.

**Two defects in the fuzzer itself fell out of chasing that leak**, and both are
the same failure mode as the coverage holes above — a gate that looks green
because it is not looking. `Runner::run` called `karac::resolve` only to feed
the typechecker and never checked `resolved.errors`, so a program with an
undefined name passed the validity gate: the name types as unit, the program
runs, its `Drop` bodies go missing with the deleted binding, and the drop log
scores that as the finding reproducing. That is how the shrinker came to save
non-compiling repros (B-2026-09-10-33) and how one reached the interpreter and
tripped `tuple index on Value::Unit` fifty times in a single run. Separately, a
shrink candidate that PANICS still scored `memory-leak`, because LSan reports
whatever was live when the program aborted — so the corpus's one leak repro was
a program that died on its second statement, filed under the signature of a
real leak. Both are fixed; the same corpus position now shrinks to the
generic-enum leak instead of to nothing.

The discipline that keeps catching these is the one the Gotchas section already
states for generated programs — *a finding must implicate the lowering, never
buggy generated source* — applied to the fuzzer's own inputs. The corollary
this round adds: a saved repro is not evidence until something has re-run it.
`drop_fuzz --verify <file>` does that now, and it is what turned "62 of 62
compile" into the more useful "60 of 62 reproduce their claimed signature".
