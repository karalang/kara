# Bug ledger — the standard

`docs/bug-ledger.jsonl` is the **single, committed, machine-countable record of
every bug (or missing primitive) surfaced in `karac`.** It exists so one
question — *"are we still finding bugs, and where?"* — becomes a number you can
watch flatten, sliced by surface (codegen/ownership/…) and by source
(kata/selfhost/dogfood/internal). Flattening of the kata + dogfood slices is a
v1 launch gate; you cannot see it without consistent capture.

Before this ledger, bug records were scattered across phase trackers (by B-ID),
test comments, commit messages, and kata READMEs (by bare commit SHA), with the
`B-YYYY-MM-DD-N` convention followed only ~1 in 4 times — so the corpus was
**not countable**. This file is the fix: prose stays in the trackers/READMEs;
they just **reference the B-ID**, and the ledger is the index.

**One tracker, not two (2026-06-14).** There used to be a second, gitignored
`bugs.md` "active triage" scratchpad holding open-bug prose. Two hand-maintained
lists drift — they did (an open bug lived in one and not the other) — so it was
**retired**. `bug-ledger.jsonl` is now the *sole* source of truth you hand-edit;
open-bug prose lives in the owning phase tracker (rule 2); and the **`## Current
state` section at the bottom of this file is generated** from the ledger (Open
bugs in full, Fixed collapsed) so you never have to read raw JSON. Never edit
that block by hand — edit the ledger and run `--inject` (see Tooling).

## The rule (lightweight, enforced)

1. **Every bug surfaced → one appended JSONL line**, keyed by a `B-YYYY-MM-DD-N`
   ID (the day it was surfaced; `N` = that day's sequence). Minting a B-ID is the
   first step of triaging any bug — the same moment it lands in a tracker.
2. **The detailed prose lives in its owning phase tracker** (or kata README);
   the ledger row carries only the countable fields + a `tracker` pointer.
3. **A kata that surfaces a bug cites its B-ID(s)** in its README (not a bare
   SHA). `selfhost`/`dogfood` bugs cite the B-ID in their spike/tracker entry.
4. `scripts/bug-lint.sh` enforces 1–3 (B-ID format + uniqueness, enum ranges,
   and the cross-repo kata↔ledger link). Run it in CI.

## Schema (one JSON object per line, fields in this order)

| field | values | notes |
|---|---|---|
| `id` | `B-YYYY-MM-DD-N` | primary key, unique |
| `date` | `YYYY-MM-DD` | surfaced date = the curve's x-axis |
| `source` | `family[:slug]` — family one of kata · kata-gap · kata-gap-audit · selfhost · dogfood · probe · spike · internal · followup · test-infra · example (e.g. `kata:42`, `selfhost:typechecker`, `followup:B-…`) | who/what surfaced it; free-text provenance goes in `detail` as a SOURCE NOTE |
| `surface` | codegen · typecheck · interp · ownership · effect · lexer · parser · runtime · resolver · cli · autopar · other — or a `+`-joined compound (`typecheck+codegen`) | which compiler phase(s) the defect was in; parenthetical detail goes in `detail`, not here |
| `class` | miscompile · double-free · use-after-free · leak · crash · codegen-gap · missing-feature · false-positive · soundness · run-vs-build · diagnostics · perf · other | failure mode — ONE primary class per bug (canonicalized 2026-07-17; the old free-text/port-triage values were migrated); nuance goes in `detail` |
| `severity` | high · medium · low | `high` = soundness / miscompile / bootstrap-critical |
| `status` | open · fixed | |
| `fix` | commit SHA · `""` | the landing commit |
| `title` | one line, ≤110 chars | |
| `tracker` | `<file>#anchor` or `kata:<n>-README` | where the prose lives |

## Tooling

```bash
python3 scripts/bug-curve.py                 # markdown report → stdout
python3 scripts/bug-curve.py --svg docs/bug-curve.svg   # + cumulative-curve SVG
python3 scripts/bug-curve.py --inject docs/bug-ledger.md  # refresh the generated
                                                          # "Current state" block
KARA_KATAS_DIR=../kara-katas ./scripts/bug-lint.sh      # integrity gate (CI)
```

**After any ledger edit, run** `--inject docs/bug-ledger.md` (refreshes the Open/
Fixed view) **and** `--svg docs/bug-curve.svg` (refreshes the curve), then
`bug-lint.sh`. The generated block can't drift from the ledger because it's never
hand-written.

## Reading the curve honestly

The historical rows are a **best-effort backfill** (2026-05 → 2026-06-13), and
the early slope reflects **when consistent record-keeping started, not the true
bug rate** — the `B-ID` convention only began ~2026-06-07, and the late-June
spike is the self-hosting + shared-enum-drop push, not a regression. The ledger
becomes a *true* signal **going forward**, where every bug is one append at
triage time. That's the whole reason for the standard: without it you can't
distinguish "bugs flattening" from "we stopped writing them down."

## Known backfill debt (does not block the curve)

- **`class` is empty** on all rows — the port-triage taxonomy was applied
  unreliably by the initial extraction, so it was blanked. Fill per-bug from the
  owning phase-12 triage when touched.
- **34 rows lack a `fix` SHA** — the trackers recorded the fix in prose, not a
  greppable SHA. `bug-lint.sh` warns (not errors) on these; backfill from
  `git log` opportunistically.
- **Pre-convention SHA-only bugs** (e.g. some early kata gaps) may still be
  uncaptured. Add them when found; don't trust the May/early-June counts as
  complete.

<!-- BUG-LEDGER:GENERATED:BEGIN -->

### By class

| class | total | open |
|---|---|---|
| miscompile | 345 | 7 |
| run-vs-build | 292 | 15 |
| leak | 251 | 7 |
| missing-feature | 192 | 0 |
| double-free | 162 | 1 |
| codegen-gap | 159 | 1 |
| diagnostics | 115 | 0 |
| false-positive | 102 | 0 |
| soundness | 95 | 3 |
| perf | 87 | 0 |
| other | 69 | 2 |
| crash | 67 | 0 |
| use-after-free | 26 | 0 |

### By surface

| surface | total | open |
|---|---|---|
| codegen | 1349 | 34 |
| interp | 313 | 15 |
| typecheck | 285 | 0 |
| ownership | 72 | 1 |
| other | 70 | 0 |
| cli | 70 | 2 |
| autopar | 55 | 0 |
| parser | 42 | 0 |
| runtime | 33 | 0 |
| effect | 29 | 0 |
| resolver | 28 | 0 |
| lexer | 8 | 0 |
## Current state

_Generated from `bug-ledger.jsonl` by `scripts/bug-curve.py` — **1962 surfaced · 36 open · 1894 fixed · 11 wontfix · 3 relocated** (2026-05-20 → 2026-09-01). Do not edit this block by hand; edit the ledger and regenerate._

### Open (36)

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-08-30-17 | 2026-08-30 | interp+codegen | low | AN `if let` MISS DROPS THE SCRUTINEE TEMPORARY *AFTER* THE `else` ARM ON BOTH BACKENDS, WHERE design.md SAYS BEFORE IT -- `els dE` vs the specified `dE els`; the two backends AGREE, so no A/B gate can see it | — |
| B-2026-08-30-18 | 2026-08-30 | codegen | medium | A struct literal built DIRECTLY at a `return` site, whose field is a `Vec` of `Drop` elements, runs the element's `Drop` body ONCE TOO MANY on every compiled backend -- `mid dR14 v14 dR14 post` against the interpreter's `mid v14 dR14 post`. The two working spellings both route the value through a named binding first (`let bx = ...; return bx`) or omit `return` entirely (the bare tail), so the return-position literal is the whole trigger; a non-container `Drop` field is correct in all three spellings | — |
| B-2026-08-30-23 | 2026-08-30 | interp+codegen | medium | THE FREE-FUNCTION SPELLING OF A CONDITIONAL RETURN LOSES THE DYING PARAMETER'S `Drop` BODY on all three backends -- `fn pick(a: R, k: bool) -> R { if k { return R { id: 98 }; } a }` at `k = true` never runs `a`'s body, while the METHOD and ASSOCIATED spellings of the same function do; the free-fn arm is the last one still gated on the union-over-return-sites predicate | none |
| B-2026-08-30-53 | 2026-08-30 | codegen | medium | The SAME `match` arm as B-2026-08-29-58 with the arm NOT TAKEN loses the assigned-into local's OWN initializer `Drop` body on both compiled backends -- `out` still holds the value it was declared with, nobody else owns it, and only the interpreter runs it. The mirror image of -58, which ran one too many on the taken path | — |
| B-2026-08-30-54 | 2026-08-30 | interp+codegen | medium | The FIELD-target spelling `h.f = r` of B-2026-08-29-58 is wrong on BOTH backends in different directions: the interpreter runs the payload's `Drop` body twice, and codegen disarms the field PERMANENTLY so a later fresh value assigned to `h.f` never runs its body at all. Neither side is an oracle, which is why -58's fix stops at a bare-identifier target | — |
| B-2026-08-31-3 | 2026-08-31 | interp+codegen | medium | A `match v[i]` ARM THAT MOVES ITS PAYLOAD INTO A BY-VALUE CALL RUNS THE PAYLOAD'S `Drop` BODY TWICE ON THE COMPILED BACKENDS AND ONCE IN THE INTERPRETER -- `got 3 dR3 dE dR3` vs `got 3 dE dR3`; the consuming call is the variable, not the index | — |
| B-2026-08-31-4 | 2026-08-31 | interp+codegen | medium | A LOCAL WHOSE LAST USE IS A `ref` ARGUMENT TO A CALLEE RETURNING A HEAP VALUE IS DROPPED AT SCOPE EXIT INSTEAD OF ITS LIVE-RANGE END ON BOTH COMPILED BACKENDS, AGAINST design.md line 866 -- `str end dE` vs interp `dE str end`; an `i64` return is clean | — |
| B-2026-08-31-6 | 2026-08-31 | codegen | medium | AN AUTO-PAR OUTLINED REGION SWALLOWS THE NLL DROP POINTS OF THE STATEMENTS IT SPANS -- `fire_due_user_drops` early-returns on the terminated insert block for exactly those indices, so a binding whose live-range end falls inside the region never fires there; the visible symptom today is that a NEVER-READ shadowed binding's `Drop` order is wrong in one direction without outlining and the other direction with it | none |
| B-2026-08-31-7 | 2026-08-31 | interp+codegen | medium | A TUPLE-PATTERN REBIND OVER AN OWNED TUPLE PARAM RUNS THE ELEMENT'S `Drop` BODY TWICE ON BOTH COMPILED BACKENDS AND ONCE UNDER `--interp` -- `b1 dR1 dR1` vs `b1 dR1`; the interpreter is the correct column, and the same arm WITHOUT the rebind agrees at one on every surface | — |
| B-2026-08-31-28 | 2026-08-31 | interp+codegen | medium | A BARE `match` ARM HANDING OUT A HEAP-CARRYING `Option` PAYLOAD RUNS NO `Drop` BODY ON EITHER COMPILED BACKEND -- `let _ = match o { Some(r) => r, None => .. };` where `Option[R]` and `R` carries a `String` prints `dR1` under `--interp` and nothing under `karac run` / `karac build`; the BRACED spelling of the same arm, the same bare arm over a USER enum, and the same bare arm over an `Option` whose payload carries no heap are all correct at one body on all three. | — |
| B-2026-08-31-31 | 2026-08-31 | codegen | medium | WHEN THE DESTRUCTURED STRUCT HAS ITS OWN `impl Drop`, ITS WRAPPER STILL RUNS THE MOVED-OUT FIELD'S BODY ON THE HUSK -- the carrier is `OwnWrapper`, which B-2026-08-31-26's mask cannot reach | — |
| B-2026-08-31-38 | 2026-08-31 | codegen | medium | `if let Some(H { r, .. }) = o` OVER AN `Option`-WRAPPED STRUCT PAYLOAD RUNS NO `Drop` BODY AT ALL ON EITHER COMPILED BACKEND -- the `match` spelling of the same pattern is correct, so it is the `if let` leg alone | — |
| B-2026-08-31-39 | 2026-08-31 | codegen | medium | AN `Option[T]` INSIDE A GENERIC FN REACHES THE DISPLAY GATE WITH `T` UNSUBSTITUTED, so an aggregate instantiation is declined or ICEs where the interpreter renders it -- `T = Vec[i64]` PANICS the compiler, `T = Array`/`Slice` refuse naming `T`, and DESTRUCTURING (the obvious workaround) is a SILENT miscompile printing `1` or nothing | — |
| B-2026-08-31-43 | 2026-08-31 | interp+codegen | medium | A `self`-ROOTED PROJECTION SCRUTINEE IS UNMASKED AT EVERY DEPTH, SO A MATERIALIZING ARM'S PAYLOAD `Drop` BODY RUNS TWICE -- `match self.e { E.A(r) => { let m = r; return m.id; } .. }` inside a `mut ref self` method prints `dR1` twice on all three backends, and the two-hop `self.s.e` doubles identically; the same code with the receiver bound to a LOCAL first runs one body | — |
| B-2026-08-31-46 | 2026-08-31 | interp+codegen | medium | A FRESH-TEMP ARG ESCAPING INSIDE A RETURNED `Option.Some(r)` RUNS ITS `Drop` BODY TWICE ON THE COMPILED BACKENDS -- `let _ = k.f(mk(4), true);` over `fn f(ref self, r: R, keep: bool) -> Option[R] { if keep { return Option.Some(r); } return Option.None; }` prints the body twice under `karac run` / `karac build` and once under `--interp`; the NAMED-binding and FREE-FN spellings of the same shape print twice on BOTH backends | — |
| B-2026-08-31-50 | 2026-08-31 | interp+codegen | medium | AN ENUM-CONSTRUCTOR MIXED WRAP LOSES ITS SLOT MASK ACROSS A WHOLE-VALUE REBIND, AND CODEGEN CANNOT INHERIT IT BECAUSE IT STORES NOTHING PER VARIABLE -- `let w = W2.Two(r, mk(2)); let w2 = w;` prints `dR1 dR2 dR1` where `dR2 dR1` is due, on all three backends; the STRUCT and TUPLE spellings of the same rebind were fixed by B-2026-08-29-44 and this one could not be, because `enum_ctor_param_view_payload_slots` derives the masked slots from the ctor EXPRESSION at the `let` and keeps no per-var record for a rebind to copy | — |
| B-2026-09-01-1 | 2026-09-01 | codegen | low | A SELF-ASSIGNMENT WHOSE RHS IS AN `if`/`match` LEAKS THE OVERWRITTEN VALUE -- `e = if c { pass(e) } else { pass(e) }` loses a block (12 allocs / 11 frees at -O0), the same shape B-2026-08-29-51 fixed for blocks and measured UNTOUCHED by it; MIXED arms leak identically, and like its sibling the leak is clean at -O2 | — |
| B-2026-09-01-3 | 2026-09-01 | interp+codegen | medium | THE TUPLE SPELLING OF B-2026-08-29-47 STILL DOUBLES A PARAM VIEW'S `Drop` BODY -- `let t = (r, 5); let x = t.0;` prints `dR1 dR1` where one is due, agreed by all four surfaces, because the per-field param-view record that fix added has no tuple peer | none |
| B-2026-09-01-5 | 2026-09-01 | codegen | low | A DISCARDED BRANCH LITERAL WHOSE FIELD IS A PROJECTION OFF A NAMED LOCAL STILL STRANDS 38 B -- `P { a: t.a, b: 1 }` is the half of B-2026-08-29-32's guard that B-2026-08-31-44 could NOT admit, because the aggregate-literal move takeover does not extend to named locals and admitting it double-frees in a loop | — |
| B-2026-09-01-9 | 2026-09-01 | codegen | low | A BRANCH MIXING AN F-STRING ARM WITH A STRING-LITERAL ARM LEAKS THE F-STRING -- `if c { f"i{n}" } else { "lit" }.contains("i")` strands the 2 B buffer that the all-f-string spelling now frees, because the fail-closed predicate requires EVERY tail to mint and a rodata literal mints nothing; admitting it is a runtime no-op at the one free site that was read, but weakens the quantifier that keeps an aliased-place arm out at seven | — |
| B-2026-09-01-16 | 2026-09-01 | codegen | medium | A STRUCT WHOSE FIELD IS PASSED BY VALUE FROM INSIDE AN INTERPOLATED-STRING ARGUMENT HAS ITS `Drop` DEFERRED TO SCOPE EXIT ON ALL THREE COMPILED SURFACES -- `println(f"field={readf(h.r)}")` is interp `field=43 drop 40 end` vs compiled `field=43 end drop 40`, against design.md line 866; hoisting the call to its own `let` makes all four agree, and the callee returns `i64`, which is B-2026-08-31-4's own passing CONTROL | — |
| B-2026-09-01-17 | 2026-09-01 | interp+codegen | low | THE PROJECTED SPELLING OF B-2026-08-31-35 STILL RUNS THE LOCAL'S `Drop` BODY TWICE -- `let _ = if c { W { r: t.r, b: 1 } } else { .. };` over a local `W` doubles on all three backends because the aggregate-literal source walker resolves a bare NAME and not a field projection, so the disarm e49a85f wired up never names `t` | — |
| B-2026-09-01-23 | 2026-09-01 | codegen | low | THE BRANCH ARM-OWNER SLOT IS ONE PER CONSTRUCT AND RESET EACH PASS, so a branch inside a loop whose owner frame lives OUTSIDE the loop frees only the LAST pass's escaping value -- `while i < 3 { let k = if i > 0 { mkA(n) } else { t }.contains("aaa"); }` strands `iterations - 1` of them (42 B in 2 blocks at 3 iterations, 72 B in 4 at 5); the same branch with the sibling binding declared INSIDE the loop body is clean, which isolates the frame CHOICE rather than the slot as the cause | — |
| B-2026-09-01-27 | 2026-09-01 | interp | low | A FRESH-TEMP owned argument DESTRUCTURED inside a method runs the right Drop bodies in the WRONG ORDER -- the payload's body fires before the enum shell's under `--interp` and after it on both compiled backends, so the counts agree and the sequence does not | — |
| B-2026-09-01-26 | 2026-09-01 | codegen | medium | A `match` OVER A FRESH-TEMP ENUM IN A NESTED EXPRESSION POSITION LEAKS THE HEAP PAYLOAD ITS ARM MOVED OUT -- `println(f"out[{match mkVe(9) { Ve.A(s) => { s } .. }}]")` loses 15 B at BOTH opt levels while the let-bound spelling of the same match is clean; both enum flavours, and clean for a scalar payload | — |
| B-2026-09-01-29 | 2026-09-01 | codegen | medium | AN `Option` WHOSE PAYLOAD OWNS HEAP LEAKS THAT HEAP WHENEVER THE VALUE IS PASSED AS A FUNCTION ARGUMENT -- under BOTH parameter modes, by value AND `ref`, generic AND non-generic; the `let`-bound spelling of the identical value is clean | — |
| B-2026-09-01-30 | 2026-09-01 | codegen | medium | A BARE-`T` BY-VALUE GENERIC PARAM'S OWNERSHIP CONVENTION DEPENDS ON THE COMPILER NOT KNOWING ITS ELEMENT TYPE, so a better-informed monomorph strands the caller's temporary (leak) and the obvious caller-side compensation double-frees a match-arm return | — |
| B-2026-09-01-32 | 2026-09-01 | interp+codegen | medium | The UNQUALIFIED enum struct-variant literal `Hold { .. }` loses BOTH `Drop` bodies on ALL FOUR surfaces, while the unqualified TUPLE-variant spelling `A(r)` is correct on all four | — |
| B-2026-09-01-33 | 2026-09-01 | codegen | medium | A PRODUCER-FN argument loses the enum payload's `Drop` body on BOTH compiled backends -- `eat(mk(5))` runs `dSv` against the interpreter's `dSv dR5`, for the tuple-variant spelling equally; routing the same value through a NAMED LOCAL is correct everywhere | — |
| B-2026-09-01-34 | 2026-09-01 | codegen | medium | A DISCARDED enum STRUCT-VARIANT literal in a TWO-TAIL `if` or a `match` arm emits NO `Drop` body at all on the compiled backends -- not even the enum's OWN -- while the same literal as a direct producer, a block tail, or a no-`else` `if` tail runs both; the tuple-variant spelling is uniform across all five | — |
| B-2026-09-01-35 | 2026-09-01 | codegen | high | A FRESH-TEMP `Option`/`Result` ARGUMENT IS DOUBLE-FREED when the callee pushes it into a LOCAL it then RETURNS INSIDE AN AGGREGATE -- neither escape guard sees that route, and it blocks B-2026-09-01-29's leak fix | — |
| B-2026-09-01-36 | 2026-09-01 | cli+codegen | medium | A REPL CELL BINDING WHOSE OWN TYPE IS `shared` CANNOT JOIN THE JIT SNAPSHOT TIER: admitted, `s.n` reads the POINTER'S OWN BITS instead of the field (two different garbage values from the same expression where `--interp` prints `3`), because `register_var_from_type_expr` -- the registrar the snapshot replay path defers to -- re-registers a shared name as an INLINE struct; the OWNERSHIP half is fine (retracting the queued `RcDec` HOLDS the reference), and `shared` as a COMPONENT (`struct W { s: S }`, `Vec[S]`) round-trips correctly | — |
| B-2026-09-01-37 | 2026-09-01 | cli | medium | A STRUCT LITERAL USED AS A CALL ARGUMENT, ALONE IN A REPL CELL, SILENTLY PRODUCES NO OUTPUT on BOTH backends -- `println(f"{take(H { n: 1 })}")` prints nothing with no diagnostic, and the `v.push(H { .. });` spelling additionally poisons the session so the next cell reports `undefined name 'v'` for a binding accepted two cells earlier; the same code in a `.kara` file works on both backends, and the same statements in ONE cell work | — |
| B-2026-09-01-38 | 2026-09-01 | ownership+codegen | high | design.md's "partial moves out of a struct field are rejected if the struct has a `Drop` impl" is UNIMPLEMENTED, and the consequence is that a user's `drop` body RUNS ON THE ZEROED FIELD -- `match h { H { r, .. } }` prints `dR[d:3] dH dR[:0]` on both compiled backends | — |
| B-2026-09-01-39 | 2026-09-01 | interp+codegen | medium | A LIVE LOCAL HANDED OUT OF A DISCARDED BRANCH LOSES ITS PAYLOAD'S `Drop` BODY, and the `if` and `match` spellings disagree in OPPOSITE directions on the two backends -- `let _ = if c { E.A(mk(8)) } else { e };` with the `e` arm taken is interp `dE` / compiled `dE dR5`, while the `match` spelling of the same thing is interp `dE dR5` / compiled `dE` | — |
| B-2026-09-01-40 | 2026-09-01 | codegen | high | A `let`-BOUND SCALAR FIELD READ OFF A STRUCT WITH ITS OWN `impl Drop` RUNS A DROP-BEARING SIBLING FIELD'S BODY AN EXTRA TIME ON EVERY COMPILED SURFACE -- `let h = H { r: R { .. }, n: 4 }; let q = h.n;` is `dH dR3` under `--interp` and `dR3 dH dR3` on jit/build/no-auto-par, with the field fully LIVE in both bodies; the same read spelled INLINE (`println(f"{h.n}")`) agrees | — |

### Relocated (3)

<details><summary>3 relocated — real and still scheduled, but with no action item today and a concrete external trigger, so the work now lives on a canonical tracker. Not wontfix (that is 'measured to a standstill'); follow the tracker column.</summary>

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-08-23-11 | 2026-08-23 | typecheck | medium | `Type::Function` carries NO EFFECT ROW, so design.md § First-Class Functions' `let f = save;  // f: Fn(User) -> () with writes(UserDB)` is not representable and a function value's effects cannot propagate through its TYPE. NARROWED 7e7972b: the IMPRECISION this caused -- a function value that is bound and never called still demanding the enclosing function declare its effects -- is FIXED, in the effect checker and without the type row (bind-an-alias, attribute-at-every-other-mention). What remains is representability only, and the prescription in the original title does not work as written: the typechecker cannot populate an effect row, because `effectcheck` runs after `typecheck` AND consumes its output, so inferred effects at typecheck time would need a cycle. Read the detail before picking this up. | implementation_checklist/phase-5-diagnostics.md#fn-type-effect-row-deferred |
| B-2026-08-26-13 | 2026-08-26 | other | medium | `String.split` HAS NO BORROWING FORM: it returns `Vec[String]`, so tokenizing costs one heap allocation and one byte copy PER FIELD, and there is no slice-returning variant to reach for. Measured at 0.67 s where C's in-place split is 0.03 s and Rust's `Vec[&str]` is 0.18 s -- but Kara BEATS Rust's same-semantics `Vec[String]` version (1.17 s) by 1.7x, so the implementation is fine and the API's return shape is the entire gap. | docs/roadmap.md § Phase 7.3 stdlib — the `StringSlice` item (zero-copy parsing/splitting), expanded in 5858a19 to carry the measured state, the v1/v2 blocker, and the open cursor-vs-split decision |
| B-2026-08-31-33 | 2026-08-31 | interp | medium | THE INTERPRETER SKIPS THE PARTIALLY-MOVED RESIDUE'S OWN `Drop` BODY WHEN AN ARM DESTRUCTURES AN `Option`-WRAPPED PAYLOAD -- `Some(H { r, .. })` prints `dR[a]` where both compiled backends print `dH dR[a]` | B-2026-09-01-38 |

</details>

### Wontfix (11)

<details><summary>11 wontfix — real and reproduced, measured to a standstill, no action left. Titles are kept in full: they carry the measurements that closed the question, so read one before reopening its subject.</summary>

| id | date | surface | sev | title |
|---|---|---|---|---|
| B-2026-08-10-20 | 2026-08-10 | codegen | low | ACCEPTED COST, not a pending fix: `Vec[(i64,i64)].sort_by` on SHUFFLED-UNIFORM input is ~1.30x Rust's `sort_by` (karac ~11.1 ms vs driftsort 8.2-8.9, 150k pairs, this host). Residual of B-2026-08-10-19. FIVE DIRECTIONS WERE MEASURED AGAINST IT AND NONE CLOSES IT -- merge-kernel tweaks, RUN tuning, the bounds-check hoist, a 3-way quicksort run-builder and a 2-way one; do not reopen any of them without new information. The last and most promising, a stable 2-way branchless quicksort run-builder, was BUILT IN FULL, verified correct (98/98 pattern x size, element-type coverage across AOT/LLJIT/interp) and measured: random 11.11 -> 11.73 ms (0.95x, i.e. SLOWER) with instructions 74.05M -> 82.18M (+11%); sawtooth 2.82 -> 3.22 (0.87x). A sweep of the configuration space (span 512/2048/8192/16384/65536, base 16/32/64) found NO setting that beats main on random -- the best, span 16384 base 64, reaches 11.17 vs main's 11.11, i.e. parity. Not merged; the implementation was reverted. This is the cost of the algorithm karac has. The one direction that DID pay, few-unique, is split out as its own open row B-2026-08-11-10. Full write-up in docs/spikes/sort-algorithm-gap.md. |
| B-2026-08-11-28 | 2026-08-11 | codegen | low | RESIDUAL of B-2026-08-10-9, split out per the live-remainder rule: on SHUFFLED-UNIFORM input the mono `sort_by` is still ~1.6x Rust's driftsort. 50a50e8 replaced the fixed-32-run merge sort with a natural-run merge sort, which was the right fix and moved the ORDERED patterns enormously (sorted and reverse went from 39-54x behind to roughly 2x AHEAD -- measured here at 0.47x and 0.52x). It deliberately did not move the shuffled case, and the closing note records that in its own numbers: `random 14.91 -> 14.60 ms (UNCHANGED)`, because shuffled input has ~2-element natural runs so the RUN padding reproduces the old run length and the old pass count. MEASURED FRESH (hyperfine, 10 runs each, clone subtracted via an identical kernel minus the sort call; 25 rounds x 150k (i64,i64) pairs, x86 container): kara pure sort 260.6 ms vs rust 159.0 ms = 1.64x. Same kernel by pattern: shuffled 1.51x total / 1.64x pure, sorted 0.47x, reverse 0.52x. SEVERITY LOW deliberately -- shuffled-uniform is the regime where an adaptive sort has least to exploit, driftsort is a strong baseline, and 1.6x on the hardest pattern while beating it 2x on ordered input is a defensible place to sit. Filed so the remainder is visible in the work queue rather than only inside a closed row's prose, NOT as a claim that it must be closed. CAVEAT: single host, x86_64 shared container, not the canonical Apple-silicon bench host. NEXT STEP if picked up: compare the emitted merge inner loop against driftsort's on shuffled input; the run-detection phase is already known not to help there, so any remaining gap is in the merge itself. |
| B-2026-08-15-31 | 2026-08-15 | codegen | medium | MECHANISM PINNED (was: not pinned). Kata #246's 1.57x deficit to `clang -O3` on arm64 is ENTIRELY LOOP UNROLLING, and karac is at `clang -O2` PARITY. Dynamic counts: kara 527.4M instrs / 66.2M cycles, clang -O3 370.7M / 42.0M, clang -O2 500.5M / 74.4M, clang -O3 -fno-unroll-loops 500.6M / 63.7M. So kara vs equal-treatment C is 1.05x on instructions and 1.06x on wall time, and kara is 11% AHEAD of clang -O2 on cycles. karac's own full-unroll hint machinery is live and working but declines this loop, and THREE ways of making it fire were then built and measured -- hint the converging guard (LLVM refuses and warns), canonicalise to a counted loop + full unroll (works, #246 -> 1.08x of clang -O3, but unbounded: kata #189's million-iteration reverse took `karac build` from 0.6s to killed at 300s), and canonicalise + bounded partial unroll (safe, but a corpus NET REGRESSION -- 10 rows better, 11 worse). WONTFIX: the blocker is that a converging loop's trip count has NO COMPILE-TIME BOUND, and until that changes every unroll strategy either misfires or over-applies. |
| B-2026-08-15-33 | 2026-08-15 | codegen | medium | `karac build` IS NOT REPRODUCIBLE: the SAME karac binary, on the SAME source, emits DIFFERENT executables across runs. Three consecutive builds of kata #253's min_meeting_rooms.kara gave two distinct sha256s (c909a866 / 76932c29 / c909a866); #252, #16 and #18 were stable across the same test, so it is program-dependent rather than universal. Sinks are unaffected -- the binaries are equivalent, not wrong -- so this is a build-reproducibility defect, not a miscompile. UPSTREAM: reproduces in stock `opt` 18.1.8 on a fixed .ll, so karac is not in the loop; the varying thing is the pass manager's FUNCTION VISIT ORDER (slot 513 names a different function per run), first diverging among the 66 identical `__karac_panic_site_*` cold leaves. |
| B-2026-08-16-9 | 2026-08-16 | codegen | low | Shuffled `sort_by` is ~1.26x driftsort after the fused single pass — the win is IPC, not work; tree at its two-pass floor and levels at the median-of-3 limit |
| B-2026-08-16-11 | 2026-08-16 | codegen | medium | The default integer overflow check is emitted inside the loop, blocking auto-vectorisation of EVERY integer reduction |
| B-2026-08-17-40 | 2026-08-17 | codegen | medium | Kata 236's 14.5% kāra-only slowdown is CODE PLACEMENT, not a codegen regression — hot code byte-identical, moved 152 bytes; instructions flat to 0.002% |
| B-2026-08-21-3 | 2026-08-21 | codegen | low | MEASURED NEGATIVE RESULT -- hoisting the ASCII test to the top of the `String.push` codegen arm removes 34% of the program's retired instructions and makes it 10% SLOWER; the string-build inner loop is latency-bound, not instruction-bound, so instruction-count reasoning does not predict its wall time |
| B-2026-08-21-52 | 2026-08-22 | effect | low | CHANNEL EFFECT RESOURCES HAVE NO PER-VALUE IDENTITY -- every channel collapses to the single `Channel` resource. The conflict-analysis motivation this row was filed on has since been REFUTED by measurement (the producer/consumer case already worked; two-producer serialization was a verb-lattice defect, fixed separately) -- what remains is a narrow spec-fidelity gap against design.md:6049, where only the NON-communication verbs on distinct channels over-serialize |
| B-2026-08-24-18 | 2026-08-24 | codegen | low | `dbg(x)` REFUSES for a HEADERLESS or WEAK-HEADERED `shared` type -- the last two shapes in the shared family the compiled `Debug` renderers do not cover. MEASURED UNREACHABLE and closed without an implementation: `dbg(x)` hands `x` to a call, which is exactly the escape the headerless purity gate demotes on, so a type can be headerless or `dbg`-able but never both (16 probe programs, both arms, controls green). `has_weak_header` is false for every type today. The guard is KEPT as insurance -- making `dbg` a borrowing intrinsic would stop it demoting and turn the arm into a silent one-word field-offset miscompile -- and the exclusion is now pinned by tests instead of assumed. |
| B-2026-08-29-62 | 2026-08-29 | codegen | low | Spelling a binary-search midpoint `lo + ((hi - lo) >> 1)` instead of `lo + (hi - lo) / 2` costs 1.67x on current main: LLVM's X86CmovConversion rewrites the branchless loop back into a branch, and only `/ 2`'s signed sign-correction lengthens the dependency chain enough to stop it -- so the "slower" spelling wins by accident. NOT statically decidable: forcing the branchless form is 1.86x FASTER on this kata and 2.02x SLOWER on kata #275, and flipping only the input data from random to a ramp reverses the sign within this one program |

</details>

### Fixed (1894)

<details><summary>1894 fixed — compact index (one-line titles; full write-up + cross-refs live in `bug-ledger.jsonl`, grep by id). The regression test is the durable artifact.</summary>

| id | surface | sev | title | fix |
|---|---|---|---|---|
| B-2026-05-20-1 | interp | high | Vec.pop synonym not dispatched in interpreter (only pop_back/pop_front handled) | 7ebb8dd |
| B-2026-05-25-1 | codegen | high | No String.push(char)/push_str builder primitive — only O(n^2) f-string self-append | 7ef42b9 |
| B-2026-06-07-1 | typecheck | high | TaskGroup escape rejection — ScopeLocal structural enforcement gap (escape could outlive frame) | — |
| B-2026-06-07-2 | codegen | high | Struct-returned-by-value ABI fall-through mis-sizes annotation-driven slot (TaskGroup/TaskHandle) | — |
| B-2026-06-07-3 | codegen | high | Borrow-mode File pattern bind registered its own close — double-close of the source fd | — |
| B-2026-06-07-4 | interp | high | Map-heavy interpreter dispatch regression — backpressure clone in method dispatch hot path | — |
| B-2026-06-07-5 | codegen | high | Returned borrows (-> ref T) v1 COMPLETE: tiers+Option[ref T], false-pass fixed, docs honest; StringSlice=v2 | — |
| B-2026-06-08-1 | codegen | high | Narrow-int binop computed at operand's true LLVM width, wrapping/truncating the wider operand | — |
| B-2026-06-09-1 | lexer | high | F-string interpolation extractor not string-aware — brace/escaped-quote inside literal miscounts | — |
| B-2026-06-09-2 | interp | high | Interp map-heavy drift RESOLVED: bench 333B->89.9B (3.7x via B-07-4 borrow fix); small-N +13.5% won't-chase | — |
| B-2026-06-10-1 | codegen | high | Vec.contains / String.contains codegen lowering (linear element scan) | — |
| B-2026-06-10-2 | codegen | high | Moving a heap field out of a by-value struct param shallow-shared the buffer — double-free | — |
| B-2026-06-10-3 | codegen | high | VecDeque/String with_capacity + match-bound VecDeque payload: missing codegen arm crashed | — |
| B-2026-06-10-4 | codegen | high | Borrow-returning call forwarded into ref param double-freed source heap (first(pick(v))) | — |
| B-2026-06-10-5 | codegen | high | Vec[(i64,String)] clone UAF — armed f-string acc freed source String after push | 63440878 |
| B-2026-06-10-6 | codegen | high | Option/Result inline-heap payload leaked when dropped undestructured (Result/Map/non-Call RHS) | 9995e88b |
| B-2026-06-10-8 | codegen | high | RC-fallback let-bound tuple/struct drop leak — box free at rc==0 didn't recurse into heap fields | 669db992 |
| B-2026-06-11-1 | codegen | high | Array[u8,N].as_ptr()/.as_mut_ptr() had no codegen handler (feeds CStr.from_ptr) | 943d9d0d |
| B-2026-06-11-2 | codegen | high | Block expression in value position freed its tail heap value — empty/dangling result under AOT | — |
| B-2026-06-11-3 | codegen | high | Unsafe raw-pointer deref *p returned address instead of loading; *p=val store side too | e08d9277 |
| B-2026-06-11-4 | codegen | high | By-value aggregate (tuple/literal/nested struct) leaked heap fields on drop | — |
| B-2026-06-11-5 | codegen | high | Direct block-construct call argument leaked its temp (ownerless after tail-cleanup suppression) | — |
| B-2026-06-11-6 | codegen | high | Struct field through a tuple element (t.1.name) compiled to i64 0 placeholder under AOT | — |
| B-2026-06-11-7 | lexer | high | Chained tuple index t.1.1 failed to parse — lexer ate 1.1 as a float | — |
| B-2026-06-11-8 | runtime | high | Vec-using compute binary re-anchored heavy std-IO runtime cluster (alloc_or_panic stderr OOM) | — |
| B-2026-06-11-10 | codegen | high | unwrap_or(default) on Option/Result unimplemented across typecheck+interp+codegen | — |
| B-2026-06-12-1 | codegen | high | wasm32 alloc wrappers need i64 shims — direct i64 call traps signature mismatch on i32 size_t | — |
| B-2026-06-12-2 | other | high | CI test-coverage follow-on — Linux-LSan leak gate (13 leaks) now closed and required | — |
| B-2026-06-12-3 | codegen | high | Method call on tuple-destructure-bound String/Vec/Slice failed codegen (unregistered dispatch) | — |
| B-2026-06-12-4 | interp | high | Ill-typed binop/unary (String*Int, -String) under karac run panicked unreachable! in interpreter | — |
| B-2026-06-12-5 | codegen | high | push_str of a fresh-owned String range-slice temp (src[a..b]) leaked once per call | — |
| B-2026-06-12-6 | codegen | high | Entry-copy of enum-field struct from fresh-temp ctor arg leaks (#22): inline struct-literal arg with an enum leaf, callee consumes it via match, no c… | 9b161ee0 |
| B-2026-06-12-7 | autopar | high | Auto-par reduction: for _ wildcard loop var failed to lower (fell back to sequential) | f0f456b9 |
| B-2026-06-12-8 | ownership | high | Struct ref/mut ref non-receiver method arg passed by value + spurious RC-promotion (segfault) | — |
| B-2026-06-12-9 | codegen | high | ? inside main() -> Result[..] miscompiles — ret {i64,i64} vs i32 entry-point signature | e5be4553 |
| B-2026-06-12-10 | codegen | high | Self-host lexer per-iteration leak — inline enum-ctor temp call arg missing caller-side drop | ecfa867a |
| B-2026-06-12-11 | typecheck | high | push_str/contains/starts_with rejected a borrowed `ref String` arg under build | 522bec1c |
| B-2026-06-12-12 | codegen | high | chained .bytes().len() failed in codegen (slice-header method-chain receiver) | 240389ff |
| B-2026-06-12-13 | codegen | high | push_str(substring(..)) leaked the fresh-owned temp unbounded (token-text surface) | 5ebdc96c |
| B-2026-06-12-14 | codegen | high | No String.repeat(n) primitive — the cur*count repeat op had no builtin | bb10c5ce |
| B-2026-06-13-1 | codegen | high | RC-fallback binding at ref/mut ref arg site — get_data_ptr returned box slot, read/wrote rc header | — |
| B-2026-06-13-2 | runtime | high | Lean sort speed regression — common-case 8/16-byte fast path restored (2.14x jump, low-card deferred) | 8ad33528 |
| B-2026-06-13-3 | cli | high | RC-fallback perf note dropped from default check text output (only json/LSP showed it) | 0862d529 |
| B-2026-06-13-4 | effect | high | allocates(Heap) declarability inconsistency — substrate effect wrongly listed as must-declare | — |
| B-2026-06-13-5 | codegen | high | Tuple-destructure leaf cleanup — let (a,b)=pair() leaves got no scope-exit free (leak) | — |
| B-2026-06-13-6 | autopar | high | Tuple-destructure binding escaping auto-par group got no slot — Undefined variable codegen abort | — |
| B-2026-06-13-7 | codegen | high | Unqualified struct-variant pattern (A { n }) didn't bind fields — Undefined variable | — |
| B-2026-06-13-8 | codegen | high | Shared enum struct-variant: built inline aggregate not RC box + match fields stayed unbound | — |
| B-2026-06-13-9 | codegen | high | Unannotated let a=E.A{..} registered variant not enum in var_type_names — method dispatch missed | — |
| B-2026-06-13-10 | other | high | Recursive shared enum with recursive-variant-first overflowed compiler stack in exhaustiveness | — |
| B-2026-06-13-11 | codegen | high | Recursive shared enum boxes not recursively rc-dec'd — child boxes leaked | — |
| B-2026-06-13-12 | typecheck | high | Unqualified struct-variant construction (Variant {..}) rejected as not-a-struct | — |
| B-2026-06-13-13 | codegen | high | Enum drop into nested-struct payload (in-place 16449077 + moved-out 129d6edc); Vec[heap-element] payload is a deferred outer-buffer-only design posit… | 129d6edc |
| B-2026-06-13-14 | codegen | high | Narrow-int arith branch poisons if/match/if-let phi merge — returned const-0 placeholder | 32ad0c84 |
| B-2026-06-13-15 | cli | high | karac run downgrades hard type errors (E_INT_AS_CHAR class) to warnings + runs with placeholder — silent wrong output, exit 0 | b59eb070 |
| B-2026-06-13-16 | codegen | high | String.split on wasm trapped signature_mismatch — FFI size params usize (i32 on wasm32) vs codegen i64; retyped u64 (same class as B-2026-06-12-1, ow… | 5f660971 |
| B-2026-06-13-17 | codegen | high | String collection method (split/contains) on a non-identifier receiver fell through identifier-keyed dispatch — materialize synth local + route to co… | d4832861 |
| B-2026-06-13-18 | autopar | high | auto-par parallelized console-output stmts (no resource effect); workers raced on stdout, reordered output | 48145ad4 |
| B-2026-06-13-19 | codegen | high | Map field drop hardcoded karac_map_free_with_drop_vec(handle,1,1) — for an occupied scalar Map[i64,i64] the runtime read offset-16 of an 8-byte key a… | c3d120e9 |
| B-2026-06-13-20 | codegen | high | Map leaf in a tuple inside a struct field double-freed — Maps are caller-retains (origin FreeMapHandle frees) but #21's NestedTuple struct drop added… | c3d120e9 |
| B-2026-06-14-1 | codegen | high | synthesize_tuple_drop_fn_te memoization key (type_expr_sig) used only the base path segment, so Map[i64,i64] (flags (0,0)) and Map[String,i64] (flags… | 1410a427 |
| B-2026-06-14-2 | codegen | high | phase-12 #24: a let-bound tuple VAR sourced from a CALL (let p = ret_tuple(i), RHS a Call not a tuple literal, no annotation) whose only heap is an e… | 1410a427 |
| B-2026-06-14-3 | cli | high | whole-program 'karac query effects'/'query concurrency' looked each fn up by the call-graph node key, but call_graph::render_target_type keyed impl m… | 34bcd728 |
| B-2026-06-14-4 | codegen | high | phase-12 #25: reading a struct field through a <struct>.tuplefield.0.<field> chain (h.ps.0.n / match h.ps.0.tok) mis-compiled - type_name_of_expr's T… | cf476a7b |
| B-2026-06-14-6 | codegen | high | phase-12 #26: a method on a Map/Set tuple element (h.m.0.len()) read a GARBAGE handle | 8a8619cf |
| B-2026-06-14-8 | codegen | high | phase-12 #27: binding a heap-bearing value OUT of a tuple element double-freed at scope exit | 7110d21f |
| B-2026-06-14-9 | codegen | high | phase-12 #28: a Map/Set bound to a LOCAL from a place source (let mm = s.m / let mm = h.m.0) build-failed 'no handler for method len on variable mm'… | 253b7335 |
| B-2026-06-14-10 | other | high | phase-12 #13: no Unicode char classifier - Kara shipped only u8.is_ascii_* byte predicates | 173ff36b |
| B-2026-06-14-11 | codegen | high | `let w = v[i]` for a heap-owned Vec[String]/Vec[Vec] (cap>0) element double-freed at scope exit: compile_index returns a SHALLOW element struct shari… | 8555f44a |
| B-2026-06-14-12 | codegen | high | Reading a heap-bearing ENUM or STRUCT Vec element (Vec[E] where E.Tag(String), or Vec[struct{name:String}]) shallow-aliases the container buffer — sa… | — |
| B-2026-06-14-13 | ownership | high | A `for x in xs` loop binding sharing a name with an earlier same-function `let x` was conflated by the ownership RC analysis: the formal RC predicate… | 5f32eb18 |
| B-2026-06-14-14 | codegen | high | TaskHandle[T].join() returned a NON-scalar T as garbage + trapped | 4363d6c1 |
| B-2026-06-14-15 | codegen | high | Numeric (int/float) f-string interpolation traps on ALL wasm targets: println(f"{x}") for x:i64/f64 (any to_string-via-snprintf path) aborts with 'un… | 1158d525 |
| B-2026-06-14-16 | codegen | high | #[derive(Display)] on a baked-stdlib enum (IoError, VarError) renders correctly in the interpreter but degraded in AOT: a compiled main() -> Result[(… | 134cb8b9 |
| B-2026-06-14-17 | codegen | high | wasm_browser --features wasm-threads: ANY parallel program (TaskGroup.spawn/par{}) DEADLOCKS in a real browser - the canvas/output never updates | 1d73edbb |
| B-2026-06-14-19 | codegen | high | StringSlice v1: slice/find + source-pinned by-value -> StringSlice (first_word); split-views stay v2 | — |
| B-2026-06-14-18 | typecheck | high | ref/mut-ref String stdlib methods degraded to Type::Error (find/slice unrouted); unwrap_or fell through | — |
| B-2026-06-14-20 | codegen | high | write_console panic-free (try_with + realloc buffers) restores lean binary floor; @fwrite test->write_console | — |
| B-2026-06-14-21 | codegen | high | A body-local owned heap `let` (Vec/String/Map/Set/Slice/array element) declared inside a for-over-COLLECTION loop leaks every iteration but the last | 9a7920c6 |
| B-2026-06-14-22 | codegen | high | wasm-threads browser builds: the WASI-preview1 polyfill's fd_write and random_get (emitted in src/wasm_glue.rs GLUE_STATIC_BODY) pass a SharedArrayBu… | 69c49ec0 |
| B-2026-06-14-23 | codegen | high | Vec/String.with_capacity miscompiled on wasm32 with an i32 count (.len()-derived) — i32*i64 byte-size multiply + i32-into-i64 cap field/alloc param e… | a55f17c1 |
| B-2026-06-14-24 | runtime | high | karac-runtime clippy-red under `cargo clippy --all --all-targets -- -D warnings`: `clashing_extern_declarations` on `realloc` | 34ce3f4d |
| B-2026-06-14-25 | codegen | high | Map/Set returned BY VALUE from a call and bound (`let m2 = make_map()`) leaks the handle on Linux LSan (silent on macOS — no LeakSanitizer there) | ae9aa79d |
| B-2026-06-14-26 | codegen | high | Bare tuple over a `Map.new()`-created var (`let t = (d, i)`) leaks the Map leaf on Linux LSan | ae9aa79d |
| B-2026-06-14-27 | ownership | high | A Copy binding destructured from a tuple whose sibling is a move type was misclassified as a move, so `karac check` rejected valid code with a spurio… | 56847674 |
| B-2026-06-14-28 | codegen | high | Parser pre-port recursive-heap gate (struct-wrapped AST shape: shared enum Expr { Add(BinOp) } + struct BinOp { left: Expr, right: Expr }) | 0890627c |
| B-2026-06-14-29 | codegen | high | PRE-EXISTING (latent on main, NOT introduced by B-2026-06-14-28; reproduces on the DIRECT-payload shape `shared enum Expr { Num(i64), Add(Expr,Expr)… | 0890627c |
| B-2026-06-14-30 | codegen | high | A zero-length fixed-array binding `let a: Array[T, 0] = []` passed to a `Slice[T]` parameter hard-failed AOT codegen (`karac build`) while `karac run… | 36e9d82f |
| B-2026-06-14-31 | codegen | high | B-2026-06-14-28 residual: the shared-enum box-drop walker leaked a Vec[shared] struct-field and a move-out edge; macOS false-green hid it (the Linux-… | 0252ec77 |
| B-2026-06-14-32 | codegen | high | tests/memory_sanitizer.rs::asan_inline_index_fn_returned_vec_string_no_leak leaks 6 bytes (1 obj from karac_string_clone) under Linux LSan on BOTH ar… | b08d984f |
| B-2026-06-14-33 | codegen | high | tests/memory_sanitizer.rs::asan_vec_extend_from_slice_self_alias_rejects FAILS under Linux LSan on BOTH arm64 and x86_64: the test expects emit_panic… | b08d984f |
| B-2026-06-14-34 | codegen | high | B-2026-06-14-31 shared-enum drop fix regressed self-host lexer compilation: emit_nested_struct_shared_rc_decs (the shared-enum-payload RC-dec walker,… | 8ffe58c4 |
| B-2026-06-15-1 | codegen | high | REGRESSION (surfaced by the full-corpus quiet-pass sweep; introduced by 0890627c / B-2026-06-14-28's CORRECT Vec[shared]-element dec, which UNMASKED… | 0f78fc4f |
| B-2026-06-15-2 | codegen | high | REGRESSION (surfaced by the full-corpus quiet-pass sweep; introduced by 1a401c7b "auto-par ordered output"): routing EVERY console write through the… | ebce9d99 |
| B-2026-06-15-3 | codegen | high | REGRESSION (surfaced by the fixed-compiler re-bench's compile-elapsed lane): the auto-par reduction env captured a fixed-size [N x T] array BY VALUE… | c3050fc8 |
| B-2026-06-15-4 | resolver | high | Cross-module enum payload-variant patterns/constructors failed name resolution | 2b2c9acf |
| B-2026-06-15-5 | typecheck | high | An imported user type did not shadow a same-named baked-prelude type, though a LOCAL definition did | 2b2c9acf |
| B-2026-06-15-6 | parser | high | Qualified struct construction `module.Type { . | 5867dfe6 |
| B-2026-06-15-7 | typecheck | high | An imported struct's field TYPES were re-lowered in the IMPORTER's namespace, so a field whose type the consumer did not import by name resolved to a… | 5867dfe6 |
| B-2026-06-16-1 | codegen | high | Canonical binary search `while lo < hi { let mid = lo + (hi-lo)/2; nums[mid] }` kept its `nums[mid]` bounds check under -O2, leaving kata #34 1.58x b… | 9b36be5d |
| B-2026-06-17-1 | codegen | high | Indexing a `ref Array[T,N]` parameter fails codegen with 'Index operator applied to non-array type' | 2cdbbac4 |
| B-2026-06-17-2 | runtime | high | Unbounded ~100 B/connection memory leak in the spawn/structured-concurrency model under a long-lived accept loop (the CANONICAL server shape: loop {… | 849030b6 |
| B-2026-06-17-3 | runtime | high | Residual of B-2026-06-17-2: a discarded FREE `spawn(\|\| handle(conn))` whose closure tail is a coroutine-compiled blocking handler (the ws_echo_freesp… | 69a03439 |
| B-2026-06-17-4 | typecheck | high | Passing a `mut ref T` binding to a `ref T` parameter is a `karac run` warning (the interpreter accepts it) but a `karac build` HARD ERROR: typecheck… | 3ab709a2 |
| B-2026-06-17-5 | codegen | high | `mut ref T` params already carry LLVM `noalias` (emit_param_alias_attrs), but that rested on an exclusive-borrow guarantee NOT enforced at call sites… | 1e0fe5ea |
| B-2026-06-17-6 | ownership | high | Exclusive-borrow rule not enforced at call sites: `f(mut v, mut v)` (two simultaneous `mut ref` exclusive borrows of the SAME binding) and `f(mut v,… | 1e0fe5ea |
| B-2026-06-17-7 | codegen | high | SOLVED (root cause identified + fixed): kata:37's solver ran ~1.34x behind Rust, and the earlier recharacterization left the cause 'unidentified' | 1de4eb1e |
| B-2026-06-17-8 | runtime | high | WASM `spawn`/`TaskGroup` builds fail at link: `undefined symbol: karac_runtime_task_detach` | ab920b23 |
| B-2026-06-17-9 | codegen | high | A Sender/Receiver moved into a non-blocking (coroutine) free `spawn` (or `tg.spawn`) was dropped by the spawn WRAPPER at ramp-return time instead of… | 691117f6 |
| B-2026-06-18-1 | codegen | high | `s.chars().collect()` into a `Vec[char]` failed codegen ('no handler for method collect on non-identifier receiver'), though it always RAN fine under… | e272ed42 |
| B-2026-06-18-2 | typecheck | high | `for c in <String>` mistyped the loop variable as `String` (owned) / `ref String` (borrowed) instead of `char` -- a typechecker/codegen MISMATCH, sin… | a658f238 |
| B-2026-06-18-3 | typecheck | high | `s.char_at(i)` (O(n) Unicode-aware i-th char -> Option[char]) and `s.char_count()` (O(n) scalar count -> i64) on String are UNIMPLEMENTED end-to-end:… | 96cd5015 |
| B-2026-06-18-4 | codegen | high | `char.is_uppercase()` / `char.is_lowercase()` have no codegen handler ('no handler for method is_uppercase on variable c'), while the sibling char-cl… | 45ecfdcb |
| B-2026-06-18-5 | codegen | high | `s.chars()` as a STANDALONE value (bound to a variable, e.g | 0ccea67e |
| B-2026-06-18-6 | codegen | high | `String.push(char)` lowered to a `karac_string_encode_char` CALL + a variable-length `build_memcpy` -- which LLVM lowers to a libc `memmove` call eve… | 1bb11108 |
| B-2026-06-18-7 | codegen | high | `for c in s.chars()` called `karac_string_decode_char` PER CHARACTER to decode one UTF-8 scalar | 89760340 |
| B-2026-06-18-8 | codegen | high | A heap value (String/Vec[T]) moved into a tg.spawn/spawn closure INSIDE A LOOP was freed twice (use-after-free / double-free): the spawn-capture move… | 279928ff |
| B-2026-06-18-9 | codegen | high | `<expr>.clone()` on a `ref`/`mut ref` (BORROWED) collection receiver mis-built into a shallow ALIAS sharing the source buffer, while the interpreter… | c0432862 |
| B-2026-06-18-10 | codegen | high | A `Vec.sort_by(\|a, b\| a.cmp(b))` whose enclosing function got AUTO-PARALLELIZED failed LLVM module verification ('Referring to an argument in another… | c0432862 |
| B-2026-06-18-12 | codegen | high | `Vec.from_slice(arr[a..b])` / `try_from_slice` with a RANGE-slice source failed codegen ('Vec.from_slice: nested-index source `arr[i]` requires outer… | e721b217 |
| B-2026-06-18-11 | codegen | high | FIXED: `String.from_utf8(v: Vec[u8]) -> Result[String, Utf8Error]` was interpreter-only; the `Result.Ok(line)` String binding failed CODEGEN with 'Un… | 410de0ab |
| B-2026-06-19-1 | codegen | high | An `Array[T, N]` passed to a `ref Slice[T]` parameter mis-built (segfault / vec-index-out-of-bounds) while the interpreter ran correctly -- a run/bui… | 6d207a7e |
| B-2026-06-19-2 | codegen | high | A heap value (String/Vec[T]) moved into a spawn/tg.spawn closure was LEAKED once per spawn when the closure body only BORROWED it | 9a5e1c39 |
| B-2026-06-19-3 | codegen | high | The self-hosted parser leaked its returned AST node: render_expr(parse_expr(src)) on input "1" leaked 80 bytes (the `shared enum Expr` box, allocated… | — |
| B-2026-06-19-4 | codegen | high | A `match` arm that binds a `shared enum` struct-variant payload as a WHOLE struct (Int(n) / Ident(n), then reads n.suffix / n.name) does not drop the… | 8a78ee6d |
| B-2026-06-19-5 | codegen | high | A computed scalar pushed/stored into a sub-word-element collection (Vec[u8] / Vec[bool] / Vec[u16] / Vec[u32], and the slice index-store) was stored… | 66a489ef |
| B-2026-06-19-6 | codegen | high | A read-only `let r = v[i]` binding of a HEAP-owning element out of a `Vec[Vec[T]]` (or any Vec whose element type is non-trivially-copyable) ALWAYS d… | — |
| B-2026-06-19-7 | codegen | high | Index-assignment to a heap-owning element of a nested collection — `out[j] = nb` where `out: Vec[Vec[i64]]` and `nb: Vec[i64]` — SIGTRAPs (exit 133)… | — |
| B-2026-06-19-8 | codegen | high | Vec.filled(n, val) with a heap-backed element type bit-copied the SAME fill aggregate into all N slots, so every slot aliased one backing buffer: the… | — |
| B-2026-06-19-9 | interp+codegen | high | Structural `==`/`!=` on a `shared struct` (design.md § Equality Semantics) was unhandled in both backends despite a correct `#[derive(Eq, PartialEq)]` | — |
| B-2026-06-19-10 | typecheck+interp+codegen | high | `{checked,saturating,overflowing}_{add,sub,mul}` — documented integer methods (design.md § Arithmetic Overflow, the table at ~2146: checked_*→Option[… | — |
| B-2026-06-19-11 | codegen | high | A heap value (Vec[T] / String) captured READ-ONLY by MULTIPLE sibling TaskGroup.spawn tasks while the parent still owns it — the canonical parallel-s… | — |
| B-2026-06-19-12 | typecheck+interp+codegen | high | Two width-dependent integer scalar method families were unrecognized (`no method 'pow'/'count_ones' on type ...`), surfaced by the cross-kata math-bi… | — |
| B-2026-06-19-13 | typecheck+interp | high | `char.to_digit(radix) -> Option[u32]` (Rust's char::to_digit) was unrecognized (Tier-A A2) | 4e4b57de |
| B-2026-06-19-14 | codegen | high | SoA `layout` blocks did not cross function boundaries — passing a SoA-laid-out Vec[E] to another function miscompiled | 74bbbef7 |
| B-2026-06-20-1 | codegen | high | Passing a bare named `fn` as a first-class `Fn(...)` value miscompiles | 79f1de14 |
| B-2026-06-20-2 | typecheck+interp+codegen | high | Four allocating String->String methods were unrecognized (Tier-B B1): `trim()` / `to_lowercase()` / `to_uppercase()` (no-arg -> String) and `replace(… | — |
| B-2026-06-20-3 | codegen | high | `Vec.binary_search(x)` / `Slice.binary_search(x) -> Option[i64]` were typecheck- and interpreter-complete but had NO codegen — `karac build` failed l… | — |
| B-2026-06-20-4 | codegen | high | String `==`/`!=` codegen memcmp'd `l_len` bytes from BOTH operand pointers UNCONDITIONALLY (compile_string_binop, BinOp::Eq\|NotEq in src/codegen/expr… | 90db12cb |
| B-2026-06-20-5 | typecheck+interp+codegen | high | No ordered key->value map existed (Tier-B B3) — only `SortedSet` (ordered set) and `Map` (insertion-order hash map) | — |
| B-2026-06-20-7 | codegen | high | Field-level SoA index-store `vec[i].field = expr` is dropped for index >= 1 -- the per-group destination address is not strided by the element index,… | 38fb0b57 |
| B-2026-06-20-8 | interp+codegen | high | Tier-D D1: `Map.entry(k).or_insert(d)` write-through (the `mut ref V` contract, design.md § Entry[K,V]) was broken in BOTH backends — the flagship co… | 2f0a7de1 |
| B-2026-06-20-9 | codegen | high | Map-key NO-ADOPT ownership residual (the broader gap B-2026-06-20-8 deferred): the fresh-temp-only key free missed every non-fresh-temp owned key on… | c7b72bd4 |
| B-2026-06-20-10 | runtime+codegen | high | Present-key Map.remove / Set.remove of a HEAP key leaks the bucket's STORED key buffer (and the bool karac_map_remove variant leaks the stored value… | c7b72bd4 |
| B-2026-06-20-11 | codegen | high | Two codegen gaps surfaced by the bespoke word-frequency kata's `keys().sort()` ordered-report idiom over a `Map[String,_]` | d9c05582 |
| B-2026-06-20-12 | codegen | high | Set INCOMING-element NO-ADOPT leak: Set.remove(x) / Set.contains(x) / Set.insert(x) of a HEAP element (Set[String], Set[Vec[T]]) leaked the incoming… | efeb9dbf |
| B-2026-06-20-13 | codegen | high | Heap `for`-loop element BORROW consumed by a retaining sink double-freed in codegen (A/B mismatch on the flagship counter idiom) | 7b93ed59 |
| B-2026-06-20-14 | codegen | high | Three PRE-EXISTING leaks the Linux-LSan gate (scripts/lsan-local.sh) flags but the macOS post-landing ASAN run misses (Apple clang has no LeakSanitiz… | 862f5a1e |
| B-2026-06-20-15 | typecheck+codegen | high | Set[Vec[T]] (and Map[Vec[T], _]) did not deduplicate equal-CONTENTS vecs -- two equal vecs inserted as distinct elements (len()==2 instead of 1), an… | ddc625ad |
| B-2026-06-20-16 | autopar | high | Auto-par (statement-level) RACED a map-mutating loop against a later read of the same map under the DEFAULT `karac build` — a silent wrong-answer/cra… | 1ee2b64d |
| B-2026-06-20-18 | codegen+resolver | high | SoA layout elements with String/Vec heap fields leaked across push/store/cleanup (front-end rejected them entirely) | b83e11fc |
| B-2026-06-21-1 | codegen | high | First-class fn value bound to a LOCAL first miscompiled — sibling of B-2026-06-20-1 (which only closed the direct bare-fn-name argument case) | fcc2c925 |
| B-2026-06-21-2 | codegen | high | First-class fn values in return / struct-field / Vec[Fn] positions miscompiled — the remaining non-arg, non-let positions after B-2026-06-20-1/-06-21… | 98731b72 |
| B-2026-06-21-3 | codegen | high | Un-annotated first-class fn-value extraction from a struct field / Vec element miscompiled — the last residual of B-2026-06-21-2 | 7010bc86 |
| B-2026-06-22-1 | typecheck | high | Map.new()/SortedMap.new() did not back-propagate K/V from insert/get, so an un-annotated map field/binding failed `karac check` though it ran fine | 387c9346 |
| B-2026-06-22-2 | codegen | high | An ESCAPING capturing closure silently miscompiles (dangling stack environment) — soundness hole surfaced finishing first-class fn values (B-2026-06-… | be2ef68e |
| B-2026-06-22-3 | cli | high | `--bindings component` (the wasm_wasi default) emitted NON-reproducible component bytes — three builds of identical source produced three DIFFERENT S… | — |
| B-2026-06-22-4 | codegen | high | Calling a closure stored in a struct field silently returns 0 under `karac build` — `(h.f)(arg)` miscompiles (codegen-only; `karac run` is CORRECT) | 4feed3b1 |
| B-2026-06-29-1 | codegen | high | DataFrame.select fresh Vec[String] arg leaked its heap buffer (48 bytes) — `select` is dispatched by try_compile_dataframe_method, which returns earl… | 5679f78b |
| B-2026-06-29-2 | typecheck | high | Scalar integer-indexing a BORROWED nested collection inferred Type::Error, so a `let row = m[i]` binding (where `m: ref Vec[Vec[i64]]` / `mut ref Sli… | a6c92f5b |
| B-2026-06-30-1 | codegen | high | Printing a `char` that crosses a CALL-RETURN SSA boundary formatted the i32 scalar as its integer codepoint under `karac build` (LLVM codegen) instea… | 292f5e13 |
| B-2026-06-30-2 | typecheck | high | `s[i]` (scalar integer index) on a `String` inferred Type::Error SILENTLY in infer_expr's ExprKind::Index final `match &obj_ty` (the `_ => Type::Erro… | f82feef2 |
| B-2026-06-30-3 | typecheck | high | Reassigning a non-`mut` field of a `shared`/`par struct` was unchecked at compile time — `karac build` silently accepted the write (exit 0) while `ka… | a0f77cbb |
| B-2026-06-30-4 | typecheck | high | Iterating a shared-borrowed `Vec` of a Copy scalar bound the element `ref i64`, not auto-deref'd in arithmetic — `for x in row` over a `ref Vec[i64]`… | a0f77cbb |
| B-2026-06-30-5 | typecheck | high | Atomic[T] op arity diverged between `karac run` and `karac build`: the implicit-ordering form (`c.count.fetch_add(1)` / `c.count.load()`) ran fine un… | 12c2346f |
| B-2026-06-30-6 | typecheck | high | Iterating a `mut ref Vec` of a Copy scalar bound the element `mut ref i64`, not auto-deref'd in arithmetic — the mutable-borrow sibling of B-2026-06-… | cc002702 |
| B-2026-06-30-7 | codegen | high | A for-loop over a struct FIELD's Vec accessed through `self` inside an impl method (`for s in self.items.iter()`) silently iterated ZERO times under… | 948a758b |
| B-2026-06-30-8 | interp | high | `TaskGroup.new()` / `tg.spawn(closure)` / `handle.join()` / `tg.cancel()` and the free `spawn(closure)` compiled and ran under `karac build` (codegen… | 8f2c8d16 |
| B-2026-06-30-9 | typecheck | high | Arithmetic on a `mut ref`/`ref` numeric SCALAR did not auto-deref, so `x = x + 1i64` where `x: mut ref i64` diverged: `karac check`/`build` HARD-erro… | a678da68 |
| B-2026-06-30-10 | codegen | high | Mutating a `mut ref` numeric-SCALAR parameter via an identifier target (`x = x + 1` / `x += 1`) did NOT propagate to the caller in built binaries — a… | 6f00795b |
| B-2026-06-30-11 | typecheck | high | A user program that redefined ONE always-injected stdlib type (`struct Response`, exported by `runtime/stdlib/http.kara`) lost the WHOLE module: `Ser… | 18e1b6ca |
| B-2026-06-30-12 | codegen | high | `String.sorted()` — characters sorted ascending into a fresh String, the canonical anagram key — was interpreter-only: `karac build` HARD-errored `co… | a00a2f58 |
| B-2026-06-30-13 | typecheck | high | `String.cmp(other) -> Ordering` — the method form of the `<`/`>` operators — was rejected at typecheck (`error[typecheck]: no method 'cmp' on type 'S… | 1cf09cb6 |
| B-2026-06-30-14 | interp | high | SILENT interpreter miscompile: `match x.cmp(y) { Less => .., Equal => .., Greater => . | 1cf09cb6 |
| B-2026-06-30-15 | codegen | high | `Vec.sort()` on a non-integer / non-String element type (e.g | 463fa826 |
| B-2026-07-01-1 | codegen | high | Tensor element-wise negation `-t` lowered to `0.0 - x` (fsub with a zero LHS) instead of a true IEEE `fneg`, so a `0.0` element negated to `+0.0` und… | eb21e300 |
| B-2026-07-01-2 | codegen | high | Column element-wise negation `-c` on an `i64` column lowered to a bare wrapping `ineg` (build_int_neg), so a slot holding `i64::MIN` silently wrapped… | eb21e300 |
| B-2026-07-01-3 | interp | high | Interpreter narrow-int width laxity for Column/Tensor element-wise ops: the interpreter stores every integer element as Value::Int(i64) and evaluates… | 1078e747 |
| B-2026-07-01-4 | interp | high | karac test panicked the interpreter ("internal error: entered unreachable code: variable not found; should be caught by resolver", src/interpreter/ev… | c6aa55c6 |
| B-2026-07-01-5 | resolver | high | Test-companion merge tripped E0101 'already defined in this scope' when the _test.kara file re-declared an import its production sibling already has… | c6aa55c6 |
| B-2026-07-01-6 | codegen | high | Enum-variant Drop-typed temporary passed directly as a call argument never ran its user drop body — the Call arm of track_inline_owned_aggregate_arg… | d1e56715 |
| B-2026-07-01-7 | codegen | high | Fn-call-RETURNED Drop-typed temporary passed directly as a call argument skips the user drop body for BOTH structs and enums — consume_g(make_guard()… | dcd819d8 |
| B-2026-07-01-8 | interp | high | Interpreter never runs user impl Drop for value enums in ANY position — let-bound, inline temp arg, or scope exit (probe enumdrop.kara 2026-07-01: ka… | d6f2e665 |
| B-2026-07-01-9 | codegen | high | SILENT miscompile: Stats.* over an integer slice bit-reinterpreted the i64 buffer as f64 under `karac build` — `Stats.sum(vec![3, 1, 2])` printed den… | 327a11e0 |
| B-2026-07-01-10 | other | high | Stats.* arguments MOVE the slice: `let v: Vec[f64] = ...; Stats.sum(v); Stats.mean(v)` is rejected by the ownership checker ('value v moved here, use… | 1fd799b7 |
| B-2026-07-01-11 | typecheck | high | *const Foo / *mut Foo for an opaque foreign type (unsafe extern "C" { type Foo; }) incorrectly fired E_OPAQUE_TYPE_REQUIRES_INDIRECTION — the canonic… | dd04325d |
| B-2026-07-01-12 | codegen | high | Map.get payload MOVED OUT of the borrow-bound match arm double-frees against the map's stored value — `let s = match m.get(k) { Some(x) => x, None =>… | be3ddf6e |
| B-2026-07-02-1 | codegen | high | Struct-pattern destructure of Option/Result payloads (`Some(Holder { name, id })`) was wholly UNIMPLEMENTED in codegen while the interpreter handled… | 51d562bb |
| B-2026-07-02-2 | codegen | high | Option/Result STRUCT payloads as container elements leaked in both widths: Vec[Option[Holder]] (payload wider than Option's 3-word inline area → heap… | d84e8de4 |
| B-2026-07-02-3 | codegen | high | Drop-surface tail bundle (slice 3v, four legs, all probe-red): (1) VecDeque never admitted by the recursive drop/clone gates despite sharing Vec's li… | 2f01f848 |
| B-2026-07-02-4 | autopar | high | Index reads and for-loops over heap-element vecs (Vec[Vec[String]], Vec[Vec[Vec[i64]]]) under-free — three sort-INDEPENDENT minimal repros (2026-07-0… | 20258bd4 |
| B-2026-07-02-5 | interp | high | Cross-branch reads inside an explicit `par { }` block (a statement branch reading a sibling branch's let-binding, e.g | fccdd4ab |
| B-2026-07-02-6 | codegen | high | Narrow-element collection LITERALS packed i64/f64 behind a narrow-typed context at EVERY sink: the literal compilers derived the element LLVM type fr… | 1078e747 |
| B-2026-07-02-7 | typecheck | high | Out-of-range integer literals are silently admitted at narrow int contexts: `let x: i8 = 200` typechecks and both surfaces print 200 (the permissive… | d984ddfc |
| B-2026-07-02-8 | autopar | high | SILENT data corruption in DEFAULT auto-par builds: `Vec.sort()`/`sort_by`/`sort_by_key`/`reverse`/`pop`/`remove` were invisible to the auto-paralleli… | d5e1165d |
| B-2026-07-02-9 | codegen | high | `String.cmp(other)` on a NON-identifier receiver — a string LITERAL (`"abd".cmp("abc")`) or an INDEX into a Vec[String] (`v[0].cmp(v[1])`) — typechec… | 3c8cd55b |
| B-2026-07-02-20 | ownership | high | Explicit closure capture-mode prefix (`own`/`ref`/`mut ref`, design.md Rule 2½) was IGNORED by the closure-escape check: `own \|x\| x + k` returned fro… | e092e041 |
| B-2026-07-02-21 | ownership | high | `println(s); println(s)` (or any later use of a printed heap value: `println(c); println(c.len())`) was rejected by `karac check` as a use-after-move… | e092e041 |
| B-2026-07-02-22 | ownership | high | `for x in v { . | e092e041 |
| B-2026-07-02-23 | ownership | high | Comparison operators CONSUME their operands under `karac check`: `if a == b {..} if a == c {..}` on String flags `a` as moved at the first comparison… | 3a6fe756 |
| B-2026-07-02-24 | ownership | high | Binding a NAMED FUNCTION as a value treats the fn item as affine: `let g = doubler; let h = doubler;` flags `doubler` moved at the first let and use-… | 2605e610 |
| B-2026-07-02-25 | ownership | high | A field- or tuple-path USE consumes the WHOLE root binding: `Add(b) => eval(b.left) + eval(b.right)` flags `b` moved-whole at the first field-path ca… | 5426bbd1 |
| B-2026-07-02-26 | ownership | high | `with_provider[Metric](p, \|\| ..); println(p.n)` is rejected — the provider VALUE arg is classified as consumed, so the post-pop use of `p` is a use-a… | 5426bbd1 |
| B-2026-07-02-27 | codegen | high | A `ref Column[i64]` parameter SILENTLY produces no output under codegen: `fn fst(c: ref Column[i64], i: i64) -> i64 { match c[i] { Some(v) => v, None… | 21a3a7f1 |
| B-2026-07-02-28 | codegen | high | `ref Slice[i64]` + `unsafe { xs.get_unchecked(i) }` MISCOMPILES: the AOT binary prints the slice header words instead of elements (probed refslice_pr… | 765fa108 |
| B-2026-07-02-10 | interp | high | Struct method named like a seq builtin (first/last/get_unchecked) silently returned Unit — try_eval_seq_method receiver-shape arms swallowed non-seq… | 0ad802a7 |
| B-2026-07-02-11 | codegen | high | Generic-mono param surface unimplemented: a Fn(...)-typed param was never in closure_fn_types so f(x) fell to the unknown-callee const-0 placeholder… | d72e0923 |
| B-2026-07-02-12 | codegen | high | Un-annotated closure param vs declared-Fn ABI mismatch: params fell back to i64 (pending_closure_param_hints was never set anywhere), so \|a\| f"{a}!"… | d72e0923 |
| B-2026-07-02-13 | codegen | high | let-annotation elem hint leaked into call-argument collection literals: pending_let_elem_type covers the whole RHS (needed by Vec.with_capacity lower… | d72e0923 |
| B-2026-07-02-29 | interp | high | Browser playground 100% broken: run_on_interp_thread (the Windows fat-stack fix) lifted EVERY interpreter run onto a fresh 16MB spawn_scoped thread;… | 0624f0fe |
| B-2026-07-02-30 | interp | high | Second universal playground trap + the program-triggered wasm trap class: Interpreter::new seeded xorshift via SystemTime::now(), which panics 'time… | 0624f0fe |
| B-2026-07-02-31 | codegen | high | Explicit `par { }` blocks whose branches read an OUTER (pre-par) binding, or whose branch RHS contains a nested block expression, fail codegen at the… | 24b1c9f4 |
| B-2026-07-02-32 | resolver | high | Every use of the fully-wired #[profile(P1, ...)] attribute emitted a bogus error[E_UNKNOWN_ATTRIBUTE] alongside the real profile diagnostics: 'profil… | 713f5988 |
| B-2026-07-02-33 | interp | high | Interpreter had no ExprKind::OffsetOf arm: karac run panicked 'not yet implemented: unhandled expr' on any offset_of[T](field.path) while karac build… | 84fb2a6b |
| B-2026-07-02-34 | interp | high | Sibling gap found probing B-2026-07-02-33: size_of[T]()/align_of[T]() had no interpreter intercept — the Call(Index(Ident,T)) parse shape fell throug… | 84fb2a6b |
| B-2026-07-02-35 | ownership | high | A read-only owned bare-`T` param reused across call sites is flagged use-after-move by `karac check` (`fn peek(s: Span) -> i64 { s.off }` then `let x… | 6e64f902 |
| B-2026-07-02-36 | ownership | high | Same-scope shadowing re-`let` shared the CFG binding identity of the binding it shadows: after `let x = "..."; let y = x; let x = 99;`, reading the N… | 479ff4bf |
| B-2026-07-02-37 | cli | high | REPL sessions deterministically bricked by re-binding a persistent let: `let x = 99;` after `let x = "...";` pruned the earlier binder's replay slice… | e132165d |
| B-2026-07-02-38 | codegen | high | Residual of B-2026-07-02-31: an explicit `par { }` branch whose `let` RHS is a METHOD CALL (`let x = base.abs()`) or an INDEX (`let x = v[0]`) still… | 1fecbb0f |
| B-2026-07-02-39 | codegen | high | Generic monos leaked every non-tensor name-keyed var side-table across nested compiles AND same-LLVM-shape handle instantiations shared one mono | a2c33051 |
| B-2026-07-02-40 | typecheck | high | Bound trait-args never substituted in method dispatch through a generic bound: fn grand[C: MyReduce[i64]](c: ref C) -> i64 { c.total() } failed check… | ffa06155 |
| B-2026-07-02-41 | codegen | high | Vec[T]-param generic monos never bind T — two element-type instantiations SHARE one mono and the second silently returns garbage under `karac build`:… | 438a0b06 |
| B-2026-07-02-42 | typecheck | high | FIXED (ee1fd78e): a PARAMETERIZED bound never compared its trait ARGS against the matched impl | ee1fd78e |
| B-2026-07-03-1 | codegen | high | f64.to_bits()/to_bits32() and the inverse i64.bits_as_f64()/bits_as_f32() had an interpreter + typechecker implementation but NO codegen arm, so a pr… | 0ceda7ab |
| B-2026-07-03-2 | codegen | high | A method declared `-> Self` was broken end-to-end under `karac build` for EVERY form (inherent method, trait impl, static constructor); `karac run` m… | f6e35b1c |
| B-2026-07-03-3 | codegen | high | PRE-EXISTING codegen miscompile (independent of Self; surfaced while fixing B-2026-07-03-2): a chained `expr.method().field` where the method returns… | 839beaea |
| B-2026-07-03-4 | autopar | high | FIXED (db020ee6, side effect of B-2026-07-03-11): Auto-par-only miscompile newly reachable after B-2026-07-03-2 enabled `-> Self` builds: a STATIC as… | db020ee6 |
| B-2026-07-03-5 | typecheck | high | User-defined trait impls on PRIMITIVE integer types are unsupported end-to-end despite being an intended feature (design.md shows `impl Pod for u8`,… | ae7c9525 |
| B-2026-07-03-6 | interp | high | SILENT DATA LOSS / no-op: the interpreter's `value_compare` (src/interpreter/helpers.rs) has NO arm for `Value::Struct` or `Value::EnumVariant`, so t… | 1b1a843b |
| B-2026-07-03-7 | codegen | high | Codegen side of the derived-Ord-struct ordering surface is unimplemented but fails LOUD (not silent, unlike its interp sibling B-2026-07-03-6): `Vec[… | ba67416e |
| B-2026-07-03-8 | typecheck | high | Trait DEFAULT METHODS are not inherited/dispatched onto implementing types: a method with a default body in the trait cannot be called on an implemen… | 6d488e58 |
| B-2026-07-03-9 | codegen | high | FIXED (dda5d5de): A generic `Slice[T]` by-value param called with a Vec arg fails codegen module verification: `fn gfirst[T](s: Slice[T]) -> T { s[0]… | dda5d5de |
| B-2026-07-03-10 | typecheck | high | Follow-on to B-2026-07-03-8: trait default methods on GENERIC traits are not inherited onto implementors | 2ddfa564 |
| B-2026-07-03-11 | codegen | high | FIXED (db020ee6): PRE-EXISTING (surfaced while verifying B-2026-07-03-8; confirmed still open after the S6b TypeExpr-level mono work of B-2026-07-02-… | db020ee6 |
| B-2026-07-03-12 | interp | high | Follow-on to B-2026-07-03-6: interpreter struct/enum ordering in value_compare is by ALPHABETICAL field/variant name, not derived-`Ord` DECLARATION o… | 9bc8e762 |
| B-2026-07-03-13 | interp | high | Interpreter: a `mut ref` SCALAR parameter FORWARDED unmarked into a nested/recursive call did not propagate the callee's mutation back to the caller… | 13cbc81e |
| B-2026-07-03-14 | autopar | high | Auto-par: the reduction recognizer accepted a `+` reduction whose per-iteration delta RECURSES into the enclosing function (`if legal { total = total… | 13cbc81e |
| B-2026-07-03-15 | codegen | high | A GENERIC impl/trait METHOD called on a CONCRETE receiver is not codegen-monomorphized: `fn apply[A](ref self, init: A, f: Fn(A, i64) -> A) -> A { f(… | cb6919c3 |
| B-2026-07-03-16 | codegen | high | SILENT MISCOMPILE: immediate field access on the result of a call that returns an aggregate (struct) — `f().field` — reads 0/default under `karac bui… | 839beaea |
| B-2026-07-03-17 | codegen | high | FIXED (db020ee6): a GENERIC function with a NON-generic NARROW return type mis-lowered under karac build (surfaced while probing B-2026-07-03-11; rep… | db020ee6 |
| B-2026-07-03-18 | typecheck | high | S6b-4a operator-on-bounded-T: `a OP b` on a type parameter bounded by the stdlib operator trait for that operator (`+`->Add, `-`->Sub, `*`->Mul, `/`-… | 5c761a6e |
| B-2026-07-03-19 | typecheck | high | S6b-4: a user `impl Reduce[T] for MyType` could not inherit a DEFAULT method from the BAKED stdlib `Reduce[T]` trait | c0a83c33 |
| B-2026-07-03-20 | typecheck | high | FIXED (a0206b1f): a BOUND on a generic impl's own type param (`impl[T: Sub] Pair[T] { .. | a0206b1f |
| B-2026-07-03-21 | autopar | high | A narrow-width (u8/u16/u32) local whose RHS is a generic call loses its UNSIGNED-ness when the binding lands in an auto-par PAR GROUP: `fn vfirst[T](… | 9f986336 |
| B-2026-07-03-22 | codegen | high | A generic `-> T` return whose T is bound from a Slice/container ELEMENT is not resolved to its concrete type at the f-string format site: `fn gsum[T]… | b5432340 |
| B-2026-07-03-23 | codegen | high | FIXED (four layers, 7ab01d78 typecheck + 08ee105d/168e2e37 codegen): a generic struct with an inline type-param field (`struct Box[T]{v:T}`, even unb… | 08ee105d |
| B-2026-07-03-24 | interp | high | Follow-on to B-2026-07-03-5: a generic BOUND over a PRIMITIVE trait impl (`fn f[T: Dbl](x: T) -> T { x.dbl() }` called with a u8/i32/f64) now TYPECHE… | d15c8372 |
| B-2026-07-03-25 | codegen | high | `.iter().map(f).collect()` into a `Vec` FAILS under `karac build` while working under `karac run` — a run/build divergence on a book-documented idiom… | 009fd479 |
| B-2026-07-03-26 | ownership | high | Matching a non-Copy field (or indexed-element field) of a BORROWED `mut ref self` receiver was treated as consuming `self`, so a later whole-`self` u… | b4dd3ba8 |
| B-2026-07-03-27 | codegen | high | An `Option[E]` field where E is a PLAIN (non-`shared`) user enum with a heap payload, dropped undestructured (via a `Some(_)` wildcard match or plain… | 14d4391e |
| B-2026-07-03-28 | codegen | high | RESIDUAL of Facet A after B-2026-07-03-33: an `Option` struct FIELD still leaks its Some-payload on plain/consumed drop when the payload is NOT the i… | 7f727aaa |
| B-2026-07-03-29 | codegen | high | `<iter>.collect()` under `karac build` rejected (loudly) adaptor chains beyond the `map`/`filter` subset landed in B-2026-07-03-25 (009fd479) | 76be2de2 |
| B-2026-07-03-30 | codegen | high | A non-shared struct FIELD typed Vec[String] / Vec[Map] / Vec[Set] / Vec[Vec[..]] leaks each element's own heap (String char buffer, Map/Set buckets,… | 58e19c9b |
| B-2026-07-03-31 | codegen | high | An `Option[<user enum/struct>]` field DESTRUCTURED into a local (`let A { value } = a`) and then MOVE-matched (`match value { Some(v) => f(v) \| v.fie… | 80229526 |
| B-2026-07-03-32 | codegen | high | SILENT wrong-output / crash under `karac build` (correct under `karac run` and `KARAC_AUTO_PAR=0`) when an owned `Column` / `DataFrame` / `Tensor` is… | 52a454c3 |
| B-2026-07-03-33 | codegen | high | Facet A step 1 (FIXED): a struct FIELD typed `Option[String]` / `Option[Vec[..]]` (inline `{ptr,len,cap}` payload) leaked its `Some` payload when the… | 25f48c25 |
| B-2026-07-03-35 | codegen | high | A Tensor declared with a NARROW numeric element type (i8/i16/i32, u8/u16/u32, f32) and built via `Tensor.from([...])` stores its elements at 8 bytes… | 98ae6e12 |
| B-2026-07-03-34 | codegen | high | SILENT seq-vs-auto-par divergence that CRASHES: a valid program panics `vec index out of bounds` under the DEFAULT `karac build` (auto-par on) while… | 99617752 |
| B-2026-07-04-1 | codegen | high | A `Vec[…]` literal (`compile_vec_prefix_literal`) whose element is an f-string (`Vec[f"…"]`) or an identifier String/Vec (`let a=…; Vec[a]`) DOUBLE-F… | 3c96c8e9 |
| B-2026-07-04-2 | codegen | high | `<iter>.collect()` under `karac build` residual after map/filter (B-2026-07-03-25) + stateful-passthrough (B-2026-07-03-29) | — |
| B-2026-07-04-3 | codegen | high | An inline tuple `(…, x)` whose heap component `x` is a FOR-LOOP element variable, moved into a `Vec` (`for x in w.iter() { v.push((i, x)) }`), DOUBLE… | 210eb93b |
| B-2026-07-04-4 | codegen | high | A NON-terminal heap `enumerate` in `<iter>.collect()` whose `(i64, <heap>)` tuple flows downstream miscompiled (garbage/crash) | 39a1ca46 |
| B-2026-07-04-5 | codegen | high | A `<iter>.collect()` chain whose SOURCE is a FRESH-TEMP call result (`mk().iter().enumerate().collect()`, not a named local) read and dropped the sou… | 748684f6 |
| B-2026-07-04-6 | codegen | high | The natural rolling-DP inner scan `dp[c] = dp[c] + dp[c - 1]` over a `Vec[i64]` (kata #62 Unique Paths) ran ~3.1x slower than clang -O3 / rustc -O (3… | b5d18320 |
| B-2026-07-04-7 | codegen | high | The ESCAPE-side sibling of B-2026-07-03-31 (which fixed the BORROW side): RETURNING (or otherwise genuinely moving OUT) a `Some(v)`-bound payload fro… | e56cc298 |
| B-2026-07-04-8 | interp | high | The tree-walk interpreter has NO u64 model: `Value::Int(i64)` is signedness-blind, so every u64 value ≥ 2^63 is handled as its negative i64 two's-com… | c7d503a2 |
| B-2026-07-04-9 | codegen | high | Two PRE-EXISTING residuals of the caller-retains shared-drop model, surfaced while landing B-2026-07-03-28 Phase 2 (both repro on main BEFORE and aft… | 273c9397 |
| B-2026-07-04-10 | codegen | high | Closed the three follow-ups noted when the rolling-DP length-pin bounds-check elision first landed (B-2026-07-04-6), generalising which counted-fill… | 81746fff |
| B-2026-07-04-11 | typecheck+codegen | high | `infer_binary`'s numeric arithmetic arm required an EXACT type match only for int-int operand pairs (`both_ints`); ANY pair with a float operand fell… | 444e6cb0 |
| B-2026-07-04-12 | interp | high | A float + unsuffixed-integer-LITERAL arithmetic expression (`let z = a + 1` where `a: f64`) diverges: `karac check` PASSES and `karac build` is CORRE… | b8e3d3ab |
| B-2026-07-04-13 | codegen | high | SHADOW SOUNDNESS HOLE in the rolling-DP length-pin bounds-check elision (B-2026-07-04-6/10), found while auditing the nested-block follow-up | 6ca6fe44 |
| B-2026-07-04-14 | codegen | high | Closed the last length-pin follow-up: NESTED-BLOCK fills | 6ca6fe44 |
| B-2026-07-04-15 | typecheck | high | S6c-12 slice 4 edge: a GENERIC container impl's trait DEFAULT method fails to resolve at the call site on the SECOND element monomorphization | c495dda3 |
| B-2026-07-04-16 | codegen | high | S6c-12 slice 4: a GENERIC TENSOR impl's `T + T` operator lowers as an INTEGER add for a non-i64 element mono under `karac build` | 735c5717 |
| B-2026-07-04-17 | codegen | high | PRE-EXISTING (repros on main today for a plain `struct { s: String }`, INDEPENDENT of B-2026-07-04-7): iterating an owned `Vec[<heap struct>]` by val… | 278e1a91 |
| B-2026-07-05-1 | codegen | high | Residual of B-2026-07-04-4: a heap `enumerate` whose `(i64, <heap>)` tuple is whole-COPIED downstream (`let X = <tuple>`) still gates to the loud dis… | — |
| B-2026-07-05-2 | codegen | high | Residual of B-2026-07-04-17 (struct case fixed 278e1a91): a BARE `Vec[<user enum>]` element MOVED whole to a new owner (`for a in items { let x = a }… | 81ad98c |
| B-2026-07-06-1 | codegen | high | `Column.from_vec(<temporary Vec[String]>)` is not supported under `karac build` — a fresh (non-`let`-bound) `Vec[String]` argument to `Column.from_ve… | a5e27243 |
| B-2026-07-06-2 | codegen | high | Bound-generic trait method dispatch over a USER-TYPE implementor fails under `karac build` (works under `karac run`) | 4f3e5747 |
| B-2026-07-06-3 | resolver | high | Resolver 'did-you-mean' suggestions are never machine-applicable: the resolver computes the exact correct name (`suggest_const_name`, fuzzy-match can… | 830831f |
| B-2026-07-06-4 | cli | high | `karac fix` silently drops the ownership `fix_diff` multi-edit migration it already computed: OwnershipErrorKind::ConcurrentSharedStruct / Concurrent… | 0f21b4b |
| B-2026-07-06-5 | codegen | high | Blanket Vec[T] surface-trait impls (`impl Reduce[i64] for Vec[i64]` etc., the spike's remaining 'blanket Vec[T] impls' S6c item): DESIGN PROVEN acros… | — |
| B-2026-07-07-1 | codegen | high | PRE-EXISTING sibling of B-2026-07-04-17 / B-2026-07-05-2, previously UNTRACKED: a `Vec[String]` / `Vec[Vec[T]]` for-loop element moved WHOLE into a l… | 81ad98c |
| B-2026-07-07-2 | codegen | high | Follow-on to B-2026-07-04-8 (interpreter u64 model, fixed 45eb926): under `karac build`, `Column[u64]`/`Tensor[u64]` `sorted`/`argsort`/`argmin`/`arg… | 7e5ef5f |
| B-2026-07-07-3 | resolver | high | E0107 UndefinedLabel `did you mean` suggestion is prose-only (not machine-applicable): as of B-2026-07-06-3 (830831f) a misspelled `break`/`continue`… | 911db54 |
| B-2026-07-07-4 | codegen | high | Borrow-returning fn (`fn f(u: ref String) -> ref String { u }`, and the `ref Vec` analog) writes out of bounds and segfaults under -O2/LLJIT | fddfb9a |
| B-2026-07-07-5 | codegen | high | Always-JIT execution lane produced EMPTY output for every non-trivial program on Linux/ELF | 199098e |
| B-2026-07-07-6 | codegen | high | REPL cross-type rebind crashes the JIT runner on Linux (1 test: repl_jit_cross_type_rebind_uses_new_value) | 8ab9e79 |
| B-2026-07-08-1 | typecheck | high | Typechecker accepted a return-position `impl Trait` with MULTIPLE distinct concrete witnesses — a run-vs-build divergence | def4648 |
| B-2026-07-08-2 | ownership | high | Ownership-checker FALSE POSITIVE: a closure that captures a Copy scalar (e.g | 59ddabb |
| B-2026-07-08-3 | codegen | high | PERF codegen-gap (no correctness impact): karac fails to strength-reduce the loop-invariant-base linear expression in kata #63's obstacle predicate,… | ac06b64b |
| B-2026-07-08-4 | codegen | high | [FIXED] Raw-pointer instance methods (`.offset` / `.read` / `.write`) are spec'd (design.md § raw pointers, L3319/L3337) but unimplemented in codegen… | cfbf0e7 |
| B-2026-07-08-5 | codegen | high | SILENT WRONG OUTPUT (exit 0): under codegen (both `karac build` AOT and the Slice-6b `KARAC_RUN_JIT=1` JIT path), a `Map` insertion performed inside… | FIXED (LLJIT Slice 6c prereq) |
| B-2026-07-08-6 | codegen | high | FIXED (both legs) | 776169c |
| B-2026-07-08-7 | codegen | low | PERF codegen-gap (no correctness impact): karac's `Vec.new()` + push-loop fill does not lower to a single sized zeroed allocation the way rust's `vec… | this commit (characterization — no code change; Linux glibc… |
| B-2026-07-08-8 | codegen | high | CORRECTED ROOT CAUSE + kata fixed | this commit |
| B-2026-07-08-9 | codegen | high | CODEGEN GAP (blocks Slice 6c): built-in `Option[T]` / `Result[T,E]` values have NO Display support under codegen — neither in an f-string (`f"{opt}"`… | this commit |
| B-2026-07-08-10 | codegen | high | SILENT WRONG OUTPUT (exit 0): `Vec.filled(n, val)` (and the `Vec[val; n]` repeat literal, which shares `build_vec_filled`) mis-sizes the buffer for a… | fa51734 |
| B-2026-07-08-11 | ownership | high | Ownership-checker FALSE POSITIVE: a method call on a GENERIC-type receiver dispatched through a single trait bound (`a.cmp(b)` where `a: T`, `T: Ord`… | this commit |
| B-2026-07-08-12 | codegen | high | CODEGEN GAP (blocks Slice 6c): a struct with a `Map` (or `Set`) field constructed inside an associated `new()` constructor emits INVALID IR — `insert… | this commit |
| B-2026-07-08-13 | parser | high | Using a reserved keyword as an identifier yields a CRYPTIC parser error that names the internal Debug token instead of saying the name is reserved | d25b777 |
| B-2026-07-08-14 | interp | high | INTERP BUG (run-vs-build): mutating a `Map`/`Set` through a STRUCT FIELD does not persist under the tree-walk interpreter, while codegen (AOT + JIT)… | 15ebcb0 |
| B-2026-07-08-15 | codegen | high | A #[derive(...)]-generated method works under the INTERPRETER (`karac run --interp`) but FAILS codegen (`karac build` AND, since Slice 6c, JIT-defaul… | this commit |
| B-2026-07-08-16 | codegen | high | CODEGEN GAP (blocks Slice 6c): destructuring a TUPLE whose element is a SHARED struct (pointer-repr) out of an `Option` emits INVALID IR — `insertval… | this commit |
| B-2026-07-08-17 | codegen | high | CODEGEN GAP (blocks Slice 6c): `<map>.values().collect()` / `.keys().collect()` / `.entries().collect()` fails codegen with `no handler for method 'c… | this commit |
| B-2026-07-08-18 | codegen+interp | low | DESIGN QUESTION (run-vs-build divergence, blocks no current example): displaying an `Option[T]`/`Result[T,E]` whose payload T is a COMPOUND type (a u… | this commit |
| B-2026-07-08-19 | cli | high | BUILD FOOTGUN (silent truncated binary): `karac build path/to/src/main.kara` where that file belongs to a `kara.toml` PACKAGE (has sibling modules it… | this commit |
| B-2026-07-08-20 | resolver+cli | high | RUN-vs-BUILD divergence: `karac build` (PROJECT mode) fails to resolve a cross-module ASSOCIATED FUNCTION that `karac run` resolves fine | this commit |
| B-2026-07-08-21 | codegen | high | SHARED RC-DROP freed scalar Map/Set keys as pointers | this commit |
| B-2026-07-08-22 | codegen | high | SHARED-ENUM match-binding that MOVES a heap payload out double-frees it | f06e1cf |
| B-2026-07-08-23 | parser | high | PARSER: a SINGLE-FIELD shorthand struct literal `P { a }` in a value position was misparsed as `P` followed by a block `{ a }`, producing a spurious… | this commit |
| B-2026-07-08-24 | codegen | low | PERF codegen-gap (no correctness impact): kāra's stock `default<O2>` pass pipeline leaves LLVM RUNTIME loop unrolling OFF, so small counted loops wit… | 05c01077 |
| B-2026-07-08-25 | codegen | medium | Generic `T.default()` under a `T: Default` bound does NOT monomorphize in codegen (it runs correctly in the interpreter) | this commit (took fix direction (a): route generic `T.defau… |
| B-2026-07-09-1 | codegen | medium | A method call on an INDEXED FIELD-ACCESS receiver — `self.names[i].bytes()` — works under the INTERPRETER but FAILS codegen with 'indexed-receiver me… | this commit |
| B-2026-07-09-2 | codegen | high | AArch64 (Apple silicon) ABI divergence: a `#[repr(C)]` struct passed BY VALUE across the C export boundary is mislowered on arm64, silently returning… | 991d3e2 |
| B-2026-07-09-3 | cli | high | The interactive JIT-default `karac repl` SILENTLY DROPPED all cell stdout: a `println(1 + 1);` cell printed nothing to the terminal (while `karac rep… | this commit |
| B-2026-07-09-4 | cli | medium | A REPL cell that PANICS under the JIT loses ALL of its own output — both the text it printed before the fault AND the panic message itself | this commit |
| B-2026-07-09-5 | runtime | low | `#[derive(Message)]` on a BARE ENUM (`#[derive(Message)] enum Role { Guest, Member, Admin }` + `r.encode()` / `Role.decode(..)`) failed with confusin… | this commit |
| B-2026-07-09-6 | codegen | high | Matching a BORROWED `Option[struct]` (`ref` / `mut ref` parameter) and reading a field of the `Some(n)` payload silently returned 0 — a wrong-answer… | this commit |
| B-2026-07-09-7 | typecheck | medium | Silent unchecked implicit integer conversions at binding boundaries (let-annotation, function argument, function return) — inconsistent with the stri… | this commit |
| B-2026-07-09-8 | codegen | medium | Windows x64 `#[repr(C)]` struct-by-value ABI is unhandled — the raw-struct lowering does not match the Microsoft x64 calling convention, a latent sil… | 4c90993d |
| B-2026-07-09-9 | resolver | low | The diverging primitive `panic(msg)` is recognized by the typechecker (diverging list, typechecker/exprs.rs:752 & expr_method_call.rs:41) and by the… | this commit |
| B-2026-07-09-10 | codegen | low | `Result::unwrap_err()` / `Result::expect_err()` (the Err-extracting, Ok-panicking variants) have NO codegen dispatcher arm: `karac build` fails with… | this commit |
| B-2026-07-09-11 | codegen | high | Niche-optimized `Option[shared T]` value stored into a CONVENTIONAL 4-word field slot crashed codegen — and the crash was INVISIBLE because all three… | 706a71e |
| B-2026-07-09-12 | codegen | high | Self-hosted parser BUILDS (after B-2026-07-09-11) but SEGFAULTS / heap-corrupts at RUNTIME on control-flow expressions | this commit |
| B-2026-07-09-13 | codegen | high | [root cause corrected — see fix] A struct-KEYED `Map[S, String]` (S a `#[derive(Hash, Eq, PartialEq)]` struct) double-frees a String VALUE under code… | this commit |
| B-2026-07-09-14 | autopar | high | PERF-REGRESSION (auto-par cost model, NO correctness impact): the default `karac build` (auto-par ON) fans out a ~70us-spawn parallel group in a hot… | this commit |
| B-2026-07-09-15 | codegen | medium | CODEGEN-GAP (no correctness impact — interpreter worked, `karac build` rejected cleanly): `Map.try_insert` / `Set.try_insert` were interpreter-only,… | this commit |
| B-2026-07-09-16 | codegen | medium | CODEGEN-GAP (no correctness impact — interpreter worked, `karac build` rejected): `SortedSet[T]` was entirely interpreter-only under `karac build` —… | this commit |
| B-2026-07-09-17 | codegen | medium | CODEGEN-GAP (no correctness impact — interpreter worked, `karac build` rejected cleanly at `SortedMap.new`): `SortedMap[K, V]` was interpreter-only u… | this commit |
| B-2026-07-09-18 | codegen | high | Generic (monomorphized) fn with an IMPLICIT TAIL bare `f"…"` double-frees the returned String under codegen (interpreter correct) | this commit |
| B-2026-07-09-19 | codegen | high | Returning a heap FIELD through a BORROWED receiver (`fn name(ref self) -> String { self.n }`, or a `ref` param) double-frees under codegen; the inter… | this commit |
| B-2026-07-09-20 | codegen | medium | The `?` operator does not support MULTI-WORD error types in codegen — a `Result[T, E]` where E is (or contains) a String / Vec / multi-field struct | this commit |
| B-2026-07-10-1 | codegen | high | SILENT WRONG-VALUE miscompile (NOT a crash / UAF — distinct from B-2026-07-09-12): the self-hosted parser's block STATEMENT expressions read back as… | this commit |
| B-2026-07-10-2 | codegen | medium | `Option`/`Result` `unwrap` / `expect` / `unwrap_err` / `expect_err` on a LET-BOUND receiver with a HEAP payload double-frees | this commit |
| B-2026-07-10-3 | codegen | low | Memory leak: `match … { Err(e) => println(e.heapfield) }` where `e` is a struct bound out of an enum/Result payload and the heap field is passed DIRE… | this commit |
| B-2026-07-10-4 | codegen | high | Self-hosted ITEM/TYPE parser crashes at runtime (heap corruption) — DISTINCT from the B-2026-07-09-12 auto-par bug | 1b5f543 |
| B-2026-07-10-5 | codegen | low | kāra is the SLOWEST of five compiled mirrors on kata #76 minimum-window-substring (M5 AND x86 container) — a two-pointer sliding-window bounds-check-… | 2fa6513 |
| B-2026-07-10-6 | codegen | low | RUN-vs-BUILD divergence: `karac run` (JIT-default) cannot execute a `gpu.dispatch` program | 05d72ed |
| B-2026-07-10-7 | codegen | high | Reading a field from a LITERAL-constructed SoA binding segfaults under codegen | e86942e |
| B-2026-07-10-8 | codegen | high | A function whose value is a top-level `if`-expression with FLOAT type miscompiled: `fn relu(x: f32) -> f32 { if x > 0.0 { x } else { 0.0 } }` failed… | 514ee12c |
| B-2026-07-11-1 | ownership | low | `Vector[T, N]` (a fixed-size SIMD value) was not classified Copy, so a bare rebind `let e1 = ux;` where `ux: Vector[f64, 2]` was treated as a MOVE an… | this commit |
| B-2026-07-11-2 | codegen | high | Indexing a Vec[T]/Slice[T]/Array[T,N] (read OR write) with a NARROWER-than-i64 integer index — e.g | this commit |
| B-2026-07-11-40 | codegen | low | Turbofish-inferred raw-pointer binding loses its pointee in codegen: `let p = ptr.null[u8](); unsafe { p.read() }` fails with 'no handler for method… | 8c4a32f |
| B-2026-07-11-3 | resolver+typecheck+interp+codegen | high | An explicit `par { }` block's top-level `let` bindings did not ESCAPE the block: `par { let a = fa(); let b = fb(); } /* use a, b */` failed with `er… | ed07aed |
| B-2026-07-11-4 | typecheck | medium | `spawn(\|\| work())` — a closure-literal thunk with NO LHS annotation — failed type inference with `cannot infer type parameter 'T'; add a type annotat… | 8be6c95 |
| B-2026-07-11-5 | codegen | high | Matching a `ref`-scrutinee enum and using a non-i64-word payload (String / bool / narrow int / float) miscompiles: the via-ptr fast path binds the le… | this commit |
| B-2026-07-11-6 | codegen | high | A `Vec[struct]` bound as an enum payload loses its element TypeExpr, so `for e in entries { e.field }` binds the element without a struct type and th… | this commit |
| B-2026-07-11-7 | codegen | high | `?` on a `Result[<concrete user enum>, E]` truncates the multi-word Ok payload to its first word: `let v = pv()?` where `pv() -> Result[Json, String]… | this commit |
| B-2026-07-11-9 | other | low | RC-fallback false-positive on a loop accumulator moved out via an early `return`: an accumulator built in a loop and returned from a branch inside th… | 687df6c |
| B-2026-07-11-10 | typecheck | low | Pushing an empty `Vec.new()` into a `Vec[Vec[i64]]` does not infer the inner element type from the receiver/method signature — `out.push(Vec.new())`… | this commit |
| B-2026-07-11-11 | codegen | medium | `.push()` (and other Vec/String methods) unsupported on a non-identifier receiver — a nested place expr like `self.scopes[i].names.push(x)` / `o.inne… | fc50a89 |
| B-2026-07-11-12 | codegen | high | SILENT: `match ref_struct.field { Some(g) => .. | af0cc9d |
| B-2026-07-11-13 | typecheck+codegen+interp | low | `String.chars()` random-access/length ergonomics (the gap-1 half of B-2026-07-11-9, which was marked fixed for gap 2 — the rc-fallback — only) | this commit |
| B-2026-07-11-14 | codegen | medium | LEAK: a user method on a FRESH-TEMP shared-struct receiver (`make().count()`) leaks the RC box + heap fields under LSan — a regression on `main`, the… | fbf97df |
| B-2026-07-11-15 | codegen | medium | LEAK: `with_capacity(n)` with a runtime `n == 0` orphaned a 1-byte heap buffer | ae65a04 |
| B-2026-07-11-16 | codegen | low | Module-level `let` binding with a COMPUTED / cross-referencing initializer (e.g | this commit — computed module-binding initializers routed t… |
| B-2026-07-11-17 | codegen | medium | `fold(init, \|acc, x\| body)` on a fused iterator chain had no codegen terminal | f0dcd3d |
| B-2026-07-11-18 | codegen | high | SILENT MISCOMPILE: `for x in <iter-chain-with-map/filter>` iterates ZERO times in codegen (interpreter iterates correctly) — wrong answer, no error. | fb41490 |
| B-2026-07-11-19 | typecheck+codegen | low | FIXED (last gap c964b29) | c964b29 |
| B-2026-07-11-20 | codegen | low | GPU helper-call gathering (reachable_helpers, GPU-LBM-5) misses #[gpu] helper calls inside Index / let-RHS / MethodCall / Cast positions, so a valid… | gpu_wgsl::reachable_helpers: add Index/MethodCall/Cast arms… |
| B-2026-07-11-21 | codegen | high | SILENT: reusing an owned `Option[shared struct]` value across two by-value consuming calls whose callee CLONES the matched subtree double-frees under… | 43e1354 |
| B-2026-07-11-22 | codegen | high | DOUBLE-FREE: `for it in vec { match it { Variant(payload) => .. | 8f4453e8 |
| B-2026-07-11-23 | interp+codegen | medium | `mut ref` closure capture (mutation of a captured mutable local) is unimplemented: a closure that writes a captured name mutates a SNAPSHOT, not the… | d123c06 |
| B-2026-07-11-24 | codegen | high | Passing a `Vec[Vec[Option[shared]]]` ELEMENT by value to a consuming (cloning) callee corrupts the heap when the outer Vec GROWS in a loop — silent w… | d2bb92e |
| B-2026-07-11-25 | codegen | high | SILENT MISCOMPILE: a GENERIC struct's ASSOCIATED function returning a struct (`impl[T] W[T] { fn make(x: T) -> W[T] { W{v:x} } }` called `W.make(7)`)… | 6b3fcac |
| B-2026-07-11-26 | codegen+interp | medium | A fresh-temp ENUM scrutinee whose type has a user `impl Drop` SILENTLY SKIPPED that Drop in `if let` / `while let` / `let…else` / `match` — the user-… | this commit |
| B-2026-07-11-27 | codegen | high | A gpu.dispatch result bound/assigned to a SoA `layout` variable SIGSEGVs: compile_gpu_dispatch_soa returns a standard AoS Vec {ptr,len,cap}, but a la… | codegen/exprs.rs + stmts.rs: bind/assign a gpu.dispatch res… |
| B-2026-07-11-28 | codegen | high | Two monomorph void-return miscompiles: (a) a generic VOID fn whose body TAIL is a statement-position `if`/`while` emitted `ret i64 0` in a void LLVM… | 9d17820 |
| B-2026-07-11-29 | codegen | high | Vec[Vec[Option[shared]]] deep-clone + consume + grow: force-cloned inner Vec's scope-exit drop LEAKS retained element handles, and at larger sizes sp… | 106efc1 |
| B-2026-07-11-30 | ownership | low | FIXED (84061d3) | 84061d3 |
| B-2026-07-11-31 | codegen | high | A generic struct instance method mis-inferred its type param `T` (mangled `$i64`, defaulted) when `T` appeared ONLY nested inside a container field (… | 93b095b |
| B-2026-07-11-32 | codegen | high | DOUBLE-FREE: an index-based element swap of a NON-COPY `Vec` element (`let t = v[i]; v[i] = v[j]; v[j] = t;` over `Vec[String]`) aliases the heap buf… | 1e81849 |
| B-2026-07-11-33 | codegen | medium | Vec[Option[shared]] element drop leaked the shared payloads (buffer-only cleanup) — kata-23 merge-k-lists | 6eb7df42 |
| B-2026-07-11-34 | typecheck+interp+codegen | low | Adaptor chaining over `stdin.lines()` (`for x in stdin.lines().map(\|r\| r)` / `.filter(p)`) TYPECHECKS but silently iterates ZERO times under both `ka… | this commit |
| B-2026-07-11-35 | codegen | high | A GENERIC container over a NON-COPY element (`Heap[String]`, `H[T]{xs:Vec[T]}`) was broken across several DIRECT-field-access legs | a663328 |
| B-2026-07-11-37 | codegen | high | Passing an `Option[String]` moved out of a RECURSIVE shared-enum node BY VALUE to a `mut ref self` method double-frees the payload under codegen (JIT… | The method-call by-value argument path (`compile_method_cal… |
| B-2026-07-11-38 | codegen | low | `fs.read_lines(path) -> Result[Vec[String], IoError]` is INTERPRETER-ONLY at v1 — no codegen | this commit |
| B-2026-07-11-39 | codegen | high | Dropping a recursive `shared enum` whose variant payload struct holds an `Option[String]` (or `Option[<inline-heap>]`) field LEAKS the `Some` payload… | Root cause: the recursive `shared enum` rc-drop destructor… |
| B-2026-07-12-1 | codegen | high | Passing a struct FIELD (`self.names`) BY REF to a FREE function double-frees the field's Vec under codegen (AOT `free(): double free detected in tcac… | 844e3b9 |
| B-2026-07-12-2 | codegen | low | `OnceLock[T]`/`OnceCell[T]` `set`/`get` codegen supports only a HEAP-FREE element `T` (scalar or small all-scalar struct, <=3 words) at v1; a heap-ow… | c4d61cb |
| B-2026-07-12-3 | codegen | high | Assignment through a `mut ref Option[shared]` parameter does not write back to the caller on codegen (interpreter correct) — silent wrong result, SIG… | 89fd514 |
| B-2026-07-12-4 | codegen | medium | Pushing a FIELD-READ `Option[shared]` (`stack.push(n.left)`) onto a `Vec[Option[shared]]` and dropping the Vec with residual elements LEAKS the pushe… | 744ca1c |
| B-2026-07-12-5 | autopar+codegen | high | The auto-parallelizer LOST a `mut ref self` method's mutation (silent wrong answer) when the call was nested in an f-string interpolation (`println(f… | 0d399837 |
| B-2026-07-12-6 | typecheck | medium | Inside a GENERIC method (`impl[T] Box[T]`), the result of `self.items.pop()` on a `Vec[T]` FIELD is INFERRED as `Option[Option[T]]` (one extra Option… | 237e27b |
| B-2026-07-12-31 | codegen | medium | A match ARM inside a very large recursive match-method that declares ~4+ heap-typed (Vec) locals corrupts memory (segfault / double-free / spurious v… | 588265f |
| B-2026-07-12-7 | codegen | medium | A volatile read through `ptr.const(param.field)` does NOT observe a prior volatile write through `ptr.mut(param.field)` to the SAME field, IN THE SAM… | e5041f2a |
| B-2026-07-12-8 | codegen | medium | A GENERIC (monomorphized) function `fn f[T](..) -> i64` whose body TAIL is a bare `loop { . | 49c8c64 |
| B-2026-07-12-9 | codegen | high | A `match` ARM GUARD (`p if cond => ..`) is SILENTLY IGNORED under codegen (`karac build` / JIT): the arm fires whenever its PATTERN matches, regardle… | 92656ed |
| B-2026-07-12-10 | typecheck | low | FIXED (3fd34c0) for the arithmetic-body case | 3fd34c0 |
| B-2026-07-12-11 | typecheck+interp+codegen | medium | `Option[T].map(f)` and `Result[T,E].map(f)` TYPECHECK CLEAN (`karac check` -> `All checks passed`) but are UNIMPLEMENTED in BOTH the interpreter AND… | ba52795 |
| B-2026-07-12-12 | codegen | medium | A CLOSURE whose body is itself a CLOSURE (a closure returning a closure / currying) fails codegen: the OUTER closure's return type is lowered as the… | d29b6b0 |
| B-2026-07-12-13 | codegen | high | A `match` on a TUPLE scrutinee is not DISCRIMINATED under codegen (`karac build` / JIT): the FIRST tuple-pattern arm always fires regardless of the t… | be39032 |
| B-2026-07-12-14 | typecheck+codegen | medium | Explicit `e.to_string()` on a `#[derive(Display)]` enum with PAYLOAD variants (e.g | 7521a16 |
| B-2026-07-12-15 | codegen | low | A bare `self.to_string()` / `f"{self}"` inside an impl method failed under codegen (`no handler for method 'to_string' on non-identifier receiver`) w… | 71929b2 |
| B-2026-07-12-16 | codegen | high | Two coupled generic-monomorphization codegen bugs, both blocking `VolatileCell[i32]` | 21b52c4 |
| B-2026-07-12-17 | codegen | medium | FIXED (7c8d383) | 7c8d383 |
| B-2026-07-12-18 | codegen | high | FIXED (acbaf96) | acbaf96 |
| B-2026-07-12-19 | typecheck | medium | A bounded generic method (`impl[T: Copy] Cell[T] { fn read(...) }`) was WRONGLY REJECTED on a PRIMITIVE instantiation (`Cell[i32]`) — "`i32` does not… | a9220b0 |
| B-2026-07-12-20 | typecheck | low | FIXED (86fd7c5) | 86fd7c5 |
| B-2026-07-12-21 | codegen | medium | Extracting an `Option[shared]` element from a `Vec[Option[shared]]` via `v[i]` and re-storing it (e.g | ee17020 |
| B-2026-07-12-22 | runtime | high | `karac run` (JIT / LLJIT) fails with 'Symbols not found: [karac_realloc_or_panic]' on ANY program that GROWS a `Vec` or `String` past its initial cap… | 73bc02a3 |
| B-2026-07-12-23 | codegen | medium | A fresh-temp `match <call-returning-Option[shared]> { Some(n) => <use n> }` LEAKS the node once per match in some shapes (32 B/iter over a loop) | d9dd2ee |
| B-2026-07-12-24 | codegen | medium | A `let d: Result[shared T, E] = f()` (or `= v[i]`, or `match`) LEAKS the payload node once per binding (32 B/iter over a loop) | 550b22c |
| B-2026-07-12-25 | codegen | medium | A match arm that binds a `shared enum` payload and passes it BY VALUE into a RECURSIVE self-call leaks the whole RC chain (every node); the same payl… | c51f5e8 |
| B-2026-07-12-26 | codegen | medium | `AlreadySetError[T]` (the `OnceLock`/`OnceCell` `set` error type, a BAKED stdlib struct) is never registered in codegen because baked stdlib structs… | ef97f7a |
| B-2026-07-12-27 | codegen | high | Moving a heap field OUT of a concretely-instantiated GENERIC user-struct payload bound from an `Option`/`Result` (or user enum) match arm SILENTLY MI… | 65d37fef |
| B-2026-07-12-28 | codegen | low | A match arm that RETURNS the bound generic-struct payload directly as the function's return value fails LLVM module verification: fn pick() -> Wrap[S… | 8c327a4 |
| B-2026-07-12-29 | codegen | medium | ARM64-ONLY leak: `work[i] = <place>` on `Vec[Option[shared]]`/`Vec[shared]` (e.g | 63d618e |
| B-2026-07-12-30 | codegen | medium | Reassigning a `Vec[shared]` / `Vec[Option[shared]]` local variable (`current = next`) LEAKS the shared elements of the OVERWRITTEN old Vec (x86-visib… | 924cd05 |
| B-2026-07-13-1 | codegen | high | An owned Vec/String PARAM returned from an `if`/`match`/nested-block BRANCH TAIL double-frees under codegen (AOT/JIT `free(): double free detected in… | 78a9a4a |
| B-2026-07-13-2 | codegen | high | A bare generic param `x: T` bound to a builtin COLLECTION (String/Vec/VecDeque) misses its owned-param return DEEP-COPY in the monomorph body under t… | d20a515 |
| B-2026-07-13-3 | codegen | medium | A GENERIC function whose body is a `match` expression evaluating to a HEAP type `T` (String/Vec), monomorphized, lowers the match VALUE to the i64 co… | 0e7face |
| B-2026-07-13-4 | interp+codegen | low | Calling a method directly on an enum UNIT-VARIANT LITERAL receiver — `Dir.North.code()` — fails on BOTH surfaces (the typechecker ACCEPTS it, but nei… | 7bd38e2 |
| B-2026-07-13-5 | codegen+typecheck | medium | Three composable Tensor limitations block idiomatic numerical-stdlib .kara over generic-dim tensors: (A) a tensor reduction/transform on a NON-IDENTI… | 45c07cb |
| B-2026-07-13-6 | codegen | high | A `let` that SHADOWS an outer variable inside ANY nested scope (plain block, `if`/`else` block, `while` body, `for` body, `match` arm, nested block)… | 07fe865 |
| B-2026-07-13-7 | codegen | high | Reading the row sub-tensors returned by `Tensor.iter_axis(n)` double-frees under JIT/native (`free(): double free detected in tcache 2`); the interpr… | 0de5fc8 |
| B-2026-07-13-8 | codegen | high | `String.from(<String>)` returned the source's `{ptr,len,cap}` aggregate UNCHANGED (an ALIAS of its heap buffer) instead of an owned copy | 257059b |
| B-2026-07-13-9 | codegen | high | A generic `shared`/`par` struct with a BARE type-parameter field (`shared struct Box[T] { mut v: T }`) instantiated at a HEAP type (`Box[String]`, `B… | 07678e8 |
| B-2026-07-13-10 | codegen | high | A chained field read/store through a Vec that is itself a FIELD of a shared struct (`root.kids[i].val`) returned the const-0 placeholder on the READ… | 26bb968 |
| B-2026-07-13-11 | codegen | high | A `shared struct` (or `par struct`) with a `Vec[shared T]` FIELD leaked every element's RC box on drop | 26bb968 |
| B-2026-07-13-12 | codegen | high | A `shared struct` (or `par struct`) with a `Map[K, shared V]` (or `shared K`) FIELD leaked the shared K/V boxes on drop | 5070066 |
| B-2026-07-13-13 | codegen | high | A `shared enum` with a `Vec[shared T]` PAYLOAD (`shared enum Tree { Branch(Vec[Node]) }`) leaked the element boxes when the enum was dropped with its… | 5070066 |
| B-2026-07-13-14 | codegen | high | Matching a `shared enum` and binding a `Vec[shared T]` PAYLOAD out (`match t { Branch(xs) => … }`) leaks the shared elements | 05c1383 |
| B-2026-07-13-15 | codegen | high | A `shared enum` with a `Map[K, shared V]` (or shared K) PAYLOAD (`shared enum Store { Full(Map[i64, Node]) }`) leaked the shared K/V boxes when dropp… | 9f59f09 |
| B-2026-07-13-16 | codegen | high | Sending an OWNED heap payload (`Vec`/`String`) through a `Channel` and then `recv`-ing it DOUBLE-FREED on native/JIT | eb8db8d |
| B-2026-07-13-17 | codegen | medium | A heap payload (`Vec`/`String`/…) SENT on a `Channel` but never RECEIVED leaks its buffer when the channel is dropped | 768cda1 |
| B-2026-07-13-18 | codegen | high | `iter().fold(init, \|acc, x\| ...)` with a HEAP accumulator (`String`; a Vec accumulator hits a separate type-inference wall) double-frees the accumula… | 763e244 |
| B-2026-07-13-19 | codegen | medium | The `?` operator applied to a `Result[Option[T], E]` (an Option NESTED inside the Result's Ok) loses the Option's payload type/value: the extracted `… | 75dd66c |
| B-2026-07-13-20 | codegen | high | A closure with a BLOCK body whose tail is a local built by a collection/String constructor (`\|\| { let mut v = Vec.new(); v.push(1); v }`) had its ret… | be02fd6 |
| B-2026-07-13-21 | codegen | high | `infer_closure_return_type` had further gaps for heap-typed closure tails, each declaring the closure fn `-> i64` against a heap body → LLVM verifier… | 1a621fe |
| B-2026-07-14-1 | codegen | high | DOUBLE-FREE: a heap payload MOVED out of a for-loop-over-owned-Vec element (via a match arm, into a function-call argument) is freed twice — once by… | this commit |
| B-2026-07-14-2 | lexer+other | medium | SPEC CONTRADICTION on bare `f16`/`bf16`: the Rust seed lexer (src/lexer.rs, keyword_or_ident, explicit comment ~L1373) treats bare `f16`/`bf16` as OR… | this commit |
| B-2026-07-14-3 | codegen | high | ARM64 LEAK: `let a = m.get(k).unwrap()` where m is `Map[_, shared T]` DOUBLE rc-inc's the fetched node | this commit |
| B-2026-07-14-4 | typecheck+ownership | low | A recursive value enum whose variant carries the SAME nested/recursive type in TWO payload fields (`enum Expr { Num(i64), Add(Expr, Expr) }`) emitted… | d831ebe |
| B-2026-07-14-5 | typecheck | medium | The typechecker SILENTLY ACCEPTED any method name on `Option[T]` / `Result[T,E]`, poisoning the call to `Type::Error` (universally assignable) so it… | 1af84da |
| B-2026-07-14-6 | runtime+interp+codegen | low | Several standard `Option`/`Result` combinators are UNIMPLEMENTED across the stack: `map_err`, `map_or`, `map_or_else`, `take`, `err`, `ok` (Result),… | a900ba9 |
| B-2026-07-14-7 | codegen | medium | Two for-loop-over-iterator-adaptor shapes SILENTLY SKIPPED the loop body in codegen (ran zero iterations, produced a wrong answer with no error), whi… | 77703ea |
| B-2026-07-14-8 | codegen | low | Proper codegen lowering for the FULL family of iterator for-loop adaptors (enumerate single-var, zip, skip/take/chain, step_by-on-iterator, flat_map,… | ff89fcf |
| B-2026-07-14-9 | codegen+interp | medium | `for x in xs.iter_mut()` (mutable iteration) is an explicitly-DEFERRED feature (typechecker/lowering.rs: 'the future .iter_mut()') that the typecheck… | 00fc91a |
| B-2026-07-14-10 | interp+codegen | low | Implement `for x in xs.iter_mut()` end-to-end: (interp) add an `iter_mut` dispatch arm that yields a mutable reference to each element so `*x = …` wr… | a9ea16e |
| B-2026-07-14-11 | codegen | low | `Vec[Vec[T]].get(i).unwrap()` loses the inner `Vec[T]` type in codegen: a subsequent `.len()`/`.get()`/index on the result fails LOUD with `no handle… | 0910b5f |
| B-2026-07-14-12 | codegen | medium | A GENERIC function that consumes an owned HEAP param MORE THAN ONCE leaks exactly one heap buffer (the original param), in the AOT/native binary (int… | 6a51fe9 |
| B-2026-07-14-13 | codegen | high | A slice pattern (`[]`, `[a, b]`, `[first, .., last]`) matched on a `ref Vec[T]` / `ref Slice[T]` PARAMETER silently mis-dispatched in codegen — the w… | f85ddf8 |
| B-2026-07-14-14 | typecheck | low | Two related slice-pattern typechecker ergonomics gaps, both CONSISTENT across interp/JIT/native (so not miscompiles — just friction) | ffa33c3 |
| B-2026-07-14-15 | codegen | high | CRASH: `let r = m.get(k).unwrap()` on a `Map[K, V]` whose VALUE is a NON-shared heap type (`Vec`, `String`) DOUBLE-FREES under JIT and native (`free(… | 51618aa |
| B-2026-07-14-16 | codegen+other | high | CRASH: `let x = v.get(i).unwrap()` / `v.first().unwrap()` on a `Vec[T]` with a SCALAR element (`i64`) reads invalid memory under JIT/native (`Invalid… | 290bfa1 |
| B-2026-07-14-17 | codegen | high | `Vec.clear()` (and the in-place mutators `fill` / `swap` / `swap_remove`) were INVISIBLE to the auto-parallelizer's write-dependency gate: the effect… | — |
| B-2026-07-14-18 | typecheck+interp+codegen | low | `Tensor.matmul(other)` and `Tensor.transpose()` are PHANTOM methods: the typechecker ACCEPTS them (returns a Tensor type, so `a.matmul(b).sum()` type… | b359b50 |
| B-2026-07-14-19 | codegen | low | The `Regex` feature works in the interpreter (oracle) but is ENTIRELY UNWIRED in codegen — `karac build`/JIT fail LOUD on every Regex operation while… | 670a775 |
| B-2026-07-14-20 | interp+codegen | low | Lazy iterator-adaptor closures (`filter`/`map`/`take_while`/`skip_while`/… predicates) DIVERGE between backends when the loop body mutates a variable… | 54e18bc |
| B-2026-07-14-21 | codegen | medium | A for-loop over a `map`/`filter` iterator chain that the fused-chain peel REJECTS — a destructuring closure param (`for x in ps.iter().map(\|(a, b)\| a… | 93d954f |
| B-2026-07-14-22 | interp | medium | The interpreter's for-loop EAGERLY DRAINED a `Value::Iterator` iterable into a Vec before running the body (eval_expr.rs `ExprKind::For`), with two c… | 01dae34 |
| B-2026-07-15-1 | other | medium | Reassigning a shared-struct local (`node = popped`) with a value bound out of `Vec[shared].pop()` does NOT release the local's OLD value (x86-visible… | a384971 |
| B-2026-07-15-2 | codegen | medium | A `Vec[shared]` local with EXACTLY ONE pushed element that is never read after the push leaks that element at scope-exit drop (two+ elements, or any… | a384971 |
| B-2026-07-15-3 | typecheck | low | No clean way to read a `mut ref i64` into a plain `i64` for indexing/arg-passing: `let ci: i64 = cur;`, passing `cur` to an `i64` param, and `cur as… | 42a9f2c |
| B-2026-07-15-4 | autopar | medium | Auto-par parallelizes an independent recursive divide-and-conquer (build left subtree, build right subtree) with NO minimum-granularity cutoff, spawn… | 67cc2db |
| B-2026-07-15-5 | codegen | high | Nested enum-variant patterns over BOXED payloads miscompile: `Option.Some(Option.Some(x))` silently takes the wrong arm, `Wrap.W(Option.Some(x))` bin… | e05c93d |
| B-2026-07-15-6 | codegen | medium | A bare-`T`-annotated local in a monomorphized generic fn (`let mut best: T = items[0]` with T→String) never registers as a tracked heap binding, so r… | a9f3f7f |
| B-2026-07-15-7 | interp | low | Interpreter emits a spurious cascading second diagnostic after a runtime error inside a compound expression: `min / -1 + 0` reports the integer-overf… | b3be90d |
| B-2026-07-15-8 | codegen | high | A closure that captures and calls another closure (`let base = \|x\| x+1; let composed = \|x\| base(x)*10`) returns 0 in codegen (interp: 50) — the captu… | cb46eed |
| B-2026-07-15-9 | codegen | low | A nested indirect closure call whose inner call yields a fresh heap value consumed by the outer call (`\|s\| wrap(wrap(s))`) leaks the intermediate Str… | 7db3655 |
| B-2026-07-15-10 | other | medium | zip adaptor surface gaps in codegen: `zip().map(f).collect()` (a map over the zipped tuples) and single-binding `for pair in zip { pair.0 }` both lou… | 465d54b |
| B-2026-07-15-11 | codegen | medium | A monomorphized generic struct whose field IS a bare type param (`struct Box[T] { value: T }`, used as `Box[String]` / `Box[Vec[..]]`) never frees th… | 5843cb9 |
| B-2026-07-15-12 | codegen | medium | An f-string that embeds a String-returning call/method/slice DIRECTLY (`f"...{obj.describe()}"`, `f"...{greet(x)}"`, `f"...{s[a..b]}"`) leaks the fre… | 0e4875d |
| B-2026-07-15-13 | codegen+other | high | A closure that mutates a captured collection through a mutating METHOD (`acc.push(x)`, `buf.push_str(s)`, `m.insert(k,v)`) does not write through to… | 25f43a1 |
| B-2026-07-15-14 | ownership | medium | `karac check` rejects calling a String/Map-mutating closure more than once (`append("a"); append("b")` → `value 'append' moved here, used again`) whi… | e69d078 |
| B-2026-07-15-15 | codegen | high | `Option.ok_or(e)` panics the compiler (ExtractOutOfRange at pattern_binding.rs) — it builds the result value in the OPTION layout {tag,w0..w2} (4 fie… | b287a24 |
| B-2026-07-15-16 | typecheck | low | Closure-param inference is missing for direct collection/Result methods that take a closure (`Vec.retain(\|x\| …)`, `Result.and_then(\|x\| …)`) — the par… | c4820b1 |
| B-2026-07-15-17 | codegen | low | A closure whose tail expression is an Option-returning method call miscompiles: `let put = \|k, v\| m.insert(k, v)` (Map.insert -> Option[V]) fails cod… | 286636f |
| B-2026-07-15-18 | codegen | high | A `<generic-struct>.field.method()` receiver reads the WRONG field for a multi-heap-field generic struct: `Pair[Vec,Vec].second.len()` returns the fi… | bf4b002 |
| B-2026-07-15-19 | codegen | low | `Vec[T]/VecDeque[T].retain(\|x\| pred)` has no AOT codegen lowering — typechecks and runs correctly in the interpreter (B-2026-07-15-16 added both), bu… | 93c8c0d |
| B-2026-07-15-20 | codegen | medium | A generic-struct field-receiver method loud-bails for a ref-param receiver (`p: ref Pair[Vec,Vec]` → `p.second.len()`) or an indexed receiver (`v[i].… | 355d5da |
| B-2026-07-15-21 | codegen | low | Read-only `Option[shared]` tree traversal ran ~1.8x behind equal-safety Rust — the residual was per-node REFCOUNT TRAFFIC (the balanced retain/releas… | a8d47f2 |
| B-2026-07-15-22 | codegen | high | `let bound = o.inner` moving a struct-typed heap-bearing field out of an owned struct double-frees the inner Vec buffer at scope exit — both `bound`'… | 8a02f75 |
| B-2026-07-15-23 | codegen | high | Moving a struct with a Map/Set handle field or an enum-with-heap-payload field double-frees the handle/enum buffer at scope exit (SIGSEGV for Map/Set… | 50b2945 |
| B-2026-07-15-24 | codegen | low | `zero_struct_move_caps` GEPs a Map/Set handle field at the BASE struct-layout offset; when a PRECEDING field is a bare generic param mono'd to a wide… | 7a2bfce |
| B-2026-07-15-25 | codegen | high | Reassigning a struct's heap-owning field (`o.f = x`) never drops the OLD field value: leaks it for a fresh RHS (`h.v = [9,8,7]` strands the old Vec b… | c588e30 |
| B-2026-07-15-26 | other | high | An INLINE `map.get(k).unwrap()` whose value is heap-owning (`Map[i64, String]` / `Map[i64, Vec[..]]`) double-frees: `println(m.get(2).unwrap())` and… | e465acb |
| B-2026-07-15-27 | other | low | Inline-indexing a temporary Vec (`m.get(k).unwrap()[i]`, and likely any `<method-chain>[i]`) loud-bails 'Index operator applied to non-array type'; b… | 354b4df |
| B-2026-07-16-1 | other | high | `<struct-field-map>.get(k).unwrap()` of a heap value (a `Map[_, String]`/`Map[_, Vec]` FIELD) double-frees the value buffer, both bound (`let v = h.m… | 3a44455 |
| B-2026-07-16-2 | runtime | high | Three recent runtime-symbol surfaces missing from the JIT keep-list — critical_section acquire/release (b6ea37a1), the tracing span/exporter octet, a… | e1972851 |
| B-2026-07-16-3 | codegen | high | #[par_unordered] collect combine helper used get_store_size for its byte-offset GEPs and its size-keyed symbol, but Vec push/index GEPs stride by ele… | 70fd850a |
| B-2026-07-16-4 | cli | medium | lljit_prototype::lljit_gdb_registration_listener_registers_dwarf_module fails on macOS (M5 Pro, Darwin 25.5): after installing + materializing a DWAR… | 8579cdac |
| B-2026-07-16-6 | autopar+other | high | Auto-par reduction lowers a loop whose body carries plain `shared` RC traffic into a multi-threaded worker — racing non-atomic rc-inc/rc-dec across w… | b057501 |
| B-2026-07-16-5 | codegen | high | Storing a `ref String`/`ref Vec` borrow into a value position — `Some(s)` with `s: ref String`, or passing a declared `ref String` struct field to a… | 46924c3 |
| B-2026-07-16-7 | ownership | high | rc-elide conditions 1-4 do not constrain where a payload PROJECTION flows: an elided fn passing `n.parent` (any projection) to a mutating callee can… | 2639536 |
| B-2026-07-16-8 | codegen | medium | LLJIT (`karac run` / KARAC_TEST_JIT) produces EMPTY OUTPUT for programs using regex, alloc_zeroed, string methods (trim/case/strip/replace/sorted/spl… | 902a163f |
| B-2026-07-16-9 | codegen | high | An `Option[shared]` bound from an `if`/`if let`/`match`/block EXPRESSION, then passed by value MORE THAN ONCE, is a use-after-free: the `let`-path `s… | 3137755 |
| B-2026-07-16-10 | codegen | medium | FIXED — User `defer` blocks execute FIFO-inline (at declaration point, in declaration order) instead of LIFO-at-scope-exit when the enclosing functio… | 07f4e09 |
| B-2026-07-16-11 | codegen | low | A `Vec` built by `Vec.new()` + a counted `push`-loop reallocs ~log(n) times (growth-doubling) where the trip count is statically derivable — auto-pre… | dae4e309 |
| B-2026-07-16-12 | ownership | medium | FIXED — Builtin collection LOOKUP methods (Map.get/remove/contains_key, Set.contains/remove, Vec.contains, String.contains) consumed their key/value… | 8f32f01 |
| B-2026-07-16-13 | other | low | `m[key]` (Map/SortedMap index operator) only accepts integer keys — a non-integer key (`m["x"]` on `Map[String,i64]`) is rejected 'index must be an i… | c585377 |
| B-2026-07-16-14 | typecheck+interp+other | medium | `karac check` accepts iterator-reduction / string-collection methods DIRECTLY on a Vec (`v.sum()`, `v.max()`, `v.min()`, `v.product()`, `v.join(sep)`… | 5090b76 |
| B-2026-07-16-15 | codegen | high | Seq-tabulate (dae4e309) miscompiled counted push loops whose body ALSO writes the while-loop's control state: `while c < n { out.push(c); if c == 3 {… | b4f86484 |
| B-2026-07-16-16 | codegen | high | tests/selfhost_codegen.rs (selfhost_codegen_matches_seed_run) is RED on main: the self-hosted emitter compiles and runs, but executing its emitted IR… | — |
| B-2026-07-16-17 | other | low | The loop-bound pre-sizing pass fired only on a STRAIGHT-LINE single push per iteration; a body whose sole fill is a balanced `if COND { v.push(a) } e… | 53f5c09 |
| B-2026-07-16-18 | codegen | high | FIXED — Reassigning a heap-owning STRUCT variable (`a = b`) double-frees: the Assign arm never suppressed the moved source `b`'s StructDrop, so both… | b837786 |
| B-2026-07-16-19 | autopar+codegen | high | A function returning `Option[String]` built from a MOVED Vec element (`let words = s.split(" "); if words.len()>0 { Some(words[0]) } else { None }`)… | d9cd7a2 |
| B-2026-07-16-20 | other+interp | medium | A `.to_string()` chained as the receiver of another method (`s.to_string().to_uppercase()`, `s.trim().to_string()…`) build-failed with 'Vec/String me… | c043d03 |
| B-2026-07-16-21 | codegen | medium | A heap-String-returning method used as the RECEIVER of another method (`s.to_uppercase().to_lowercase()`, `e.to_uppercase().split(",")`, `c.trim().to… | c043d03 |
| B-2026-07-16-22 | codegen | medium | `Option[String].unwrap_or(default)` / `Result[String,E].unwrap_or(default)` leaks a fresh heap-String default once per call when the receiver is data… | 598765b |
| B-2026-07-16-23 | codegen | medium | `unwrap_or(<non-Call heap default>)` mismanaged the eager default's ownership | ed0b9db |
| B-2026-07-16-24 | codegen | medium | `String.replace(from, to)` never freed its fresh-owned String ARGUMENTS — a fresh-temp arg (`s.replace(a.to_string(), b.to_string())`) leaks once per… | 7be908c |
| B-2026-07-17-2 | codegen | high | shared-ownership-matrix frontier REGRESSION: `forwarding_chain/ResultOk` + `forwarding_chain/ResultErr` went Clean → Leak with RC-elision ON (`KARAC_… | 3830213 |
| B-2026-07-17-3 | codegen | high | An owned `self` receiver method returned/moved with a non-empty heap (Vec/String) field DOUBLE-FREES the field buffer | 13eda85 |
| B-2026-07-17-4 | codegen | high | `let a = r.unwrap_or("x").len();` on a let-bound `Option[String]` double-frees SEQUENTIALLY (no auto-par): unwrap_or's present branch reconstitutes t… | d9cd7a2 |
| B-2026-07-17-5 | cli | low | wasm link fails with cryptic `undefined symbol: __wasm_first_page_end` (from rustup's self-contained wasi libc.a dlmalloc.c.obj) when the PATH `wasm-… | 6e88245 |
| B-2026-07-17-1 | codegen | low | In-place single-row DP `while k >= 1 { row[k] = row[k] + row[k-1]; k = k-1 }` (Pascal #119, and the general rolling-DP shape) keeps a per-iteration b… | 6474a73 |
| B-2026-07-17-6 | typecheck+interp | medium | `match <non-Option scalar/String> { Some(v) => …, None => … }` (Option-variant patterns on a scrutinee that is NOT an Option) PASSES `karac check` bu… | b71fdd3 |
| B-2026-07-17-7 | codegen | high | `Vec[Tensor]` element ownership was never wired through codegen — a `Vec[Tensor]` (tensor-valued-autograd `Tape` grads/values columns) leaked every e… | 87443ed |
| B-2026-07-17-8 | codegen | medium | Par-tabulate install of a pre-seeded accumulator takes the combine APPEND arm — a serial total×elem memcpy on the parent thread per dispatch while ev… | 7b7ba41a |
| B-2026-07-17-9 | codegen | medium | Routing Vec/String frees through an unattributed karac_free_buf declaration turned every cleanup drain into a clobber-everything opaque call — LLVM k… | 7b7ba41a |
| B-2026-07-17-10 | runtime | medium | The buffer-cache's first cut used OnceLock/Mutex/env::var_os/eprint_fmt inside the force-kept karac_alloc_or_panic/karac_free_buf closure — ONE reach… | 7b7ba41a |
| B-2026-07-17-11 | codegen | medium | Iterator.reduce over FLOAT elements returns the None arm under karac build (interp correct): `[1.5, 2.5, 0.5].iter().reduce(\|a, x\| if x > a { x } els… | 75e248d |
| B-2026-07-17-12 | typecheck | medium | Unknown methods on non-exhaustive prelude types (Vec/String/Map/Set/...) silently type as Type::Error, which unifies with ANYTHING: `v.some_typo()` p… | dc094bc |
| B-2026-07-17-13 | codegen | low | A narrow-UNSIGNED value carried through an Option payload prints SIGNED: `Option[u8]` holding 200u8, unwrapped and printed, shows -56 (200 as i8) und… | 8d21349 |
| B-2026-07-17-15 | codegen+autopar | high | Two annotated opaque-handle-new bindings (`let t: Interner = Interner.new()` / `let a: Arena[T] = Arena.new()`) in the same fn are auto-parallelized… | e01b609 |
| B-2026-07-17-16 | ownership | low | Spurious RC fallback on the natural 'build a nested Vec row-by-row' shape: `while … { let mut row = Vec.new(); while … { row.push(x) } outer.push(row… | 4938f2c |
| B-2026-07-17-17 | codegen | medium | A tensor VARIABLE reassignment (`w = w - g` / `w = w + d`, where `w: mut Tensor`) never freed the displaced old block — one `[rank][dims][data]` leak… | fbb824f |
| B-2026-07-17-18 | typecheck | low | Unknown methods on the Type::Named numerical prelude types `Tensor` and `DataFrame` silently typed as Type::Error (same check/execution hole B-2026-0… | aee4a66 |
| B-2026-07-17-19 | typecheck+codegen | low | Unknown methods on a fixed-size `Array[T, N]` silently type as Type::Error and pass `karac check`, then run on no backend — the same check/execution… | 75c85023 |
| B-2026-07-17-20 | codegen | high | Copying a Vec field out of a MATCH-BOUND enum payload borrowed from a ref-Vec element double-frees under AOT: `for it in items { match it { Fu(f) =>… | 3140c6d |
| B-2026-07-17-21 | codegen | high | AOT MISCOMPILE (use-after-free): a loop containing `let x = Some(shared T); vec.push(x)` frees the LAST pushed element, so reading vec[N-1] after the… | 62ca0962 |
| B-2026-07-18-2 | codegen | high | CONTEXT-DEPENDENT AOT memory corruption in the selfhost codegen generator once the Slice-12 struct machinery (a ~21-Vec-field Emitter struct + Struct… | 8c5cc150 |
| B-2026-07-18-1 | codegen | medium | KARAC_AUTO_PAR=0 (auto_par_disabled) did NOT disable the parallel REDUCE lowering — only the parallel-group dispatch | 4d6efad |
| B-2026-07-18-3 | codegen | medium | Consuming a BOXED `Option` payload whose type is a heap-containing tuple (e.g | eab5026 |
| B-2026-07-18-4 | codegen | medium | A STRUCT-VARIANT enum payload's Vec field bound DIRECTLY in a match arm over a borrowed ref-Vec element, then moved into a local (`enum It { Fu { par… | 8c9fb2b |
| B-2026-07-18-5 | interp | low | gpu.upload / gpu.download ICE the interpreter with `unreachable!("variable 'gpu' not found")` — no interpreter arm exists for the resident-buffer API… | fcdf5202 |
| B-2026-07-18-6 | codegen | medium | PRE-EXISTING on main (not from the B-2026-07-18-2 fix — reproduced with it stashed): tests/http_client_codegen.rs test_ir_http_error_drop_frees_messa… | fcdf520 |
| B-2026-07-18-7 | codegen | high | Plain struct-variable REASSIGNMENT (`let mut p = P { x: 1, y: 2 }; p = P { x: 10, y: 20 }`) now emits a reference to `karac_runtime_gpu_free_soa`, so… | 13f9c2a |
| B-2026-07-18-8 | codegen | medium | String built one byte at a time (`out.push_str(s[d..d+1])` in a loop) emits ~1.15-1.56x more instructions than equal-safety Rust: push_str's per-appe… | 07678e8 |
| B-2026-07-18-9 | codegen | high | An ASSOCIATED-fn call (`Type.method(...)`) passing a FRESH-TEMP value to a `ref`/`mut ref` param passed the temp BY VALUE instead of spilling it to a… | 1e5e01e3 |
| B-2026-07-18-10 | codegen | high | `Tensor.{from,zeros,ones,full}` in an ARGUMENT position laid its data out at the literal's DEFAULT element width (f64 for `-1.0`, i64 for `1`) rather… | 7bf8bf54 |
| B-2026-07-18-11 | typecheck+codegen | medium | `OnceLock[T].get().unwrap_or(<value>)` fails the LLVM verifier and `.unwrap()` faults at run time (interp fine) — the `Option[ref T]` payload from a… | 162a13f |
| B-2026-07-18-12 | interp | medium | `Stats.*` on a `Slice[T]` value (`Stats.mean(v.as_slice())`, the declared `ref Slice[f64]` param's canonical form) read ZERO elements in the tree-wal… | 3198a408 |
| B-2026-07-18-13 | codegen | high | [RESOLVED — re-measured at parity] Kata #415 add_strings recorded 13.4x equal-safety Rust at filing (89.1B vs 6.25B instrs) | 90fe2ad |
| B-2026-07-18-14 | codegen+interp | low | Interpolating a WHOLE TUPLE value in an f-string / `println` (`f"{t}"` where `t: (i64, i64)` or `(i64, String)`) passes `karac check` and renders `(3… | f882072 |
| B-2026-07-18-15 | codegen | high | String accumulator built by `push(char)` in a counted loop was mis-lowered through the Vec TABULATE reduction path, overrunning the byte buffer by 3… | 2c472f18 |
| B-2026-07-18-16 | codegen | medium | `<int>.parse(s)` / `<int>.from_str_radix(s, r)` / `f64.parse(s)` never freed their fresh-owned String ARGUMENT — a fresh-temp arg (`i64.parse("42".to… | 9ef97b4 |
| B-2026-07-18-17 | codegen | low | [FIXED] The typechecker did NOT propagate a method parameter's / struct field's (substituted) expected type into the ARGUMENT's own inference, so a t… | 60b7150 |
| B-2026-07-18-20 | codegen+interp | high | The whole `std.encoding` surface (`Base64`/`Hex` encode/decode, `Url` encode/decode) SILENTLY MISCOMPILED under `karac build` / `karac run` (JIT) | cb49f4f |
| B-2026-07-18-21 | codegen | medium | Chained `arena.get(r).field` on a struct-element `Arena[T]` silently reads the `i64 0` placeholder instead of the stored field (AOT/JIT; interp corre… | 5c3f61f |
| B-2026-07-18-22 | runtime | low | Both GPU dispatch entrypoints (karac_runtime_gpu_dispatch + karac_runtime_gpu_dispatch_resident) called `std::slice::from_raw_parts(uniform_ptrs, n_u… | b06159d |
| B-2026-07-18-23 | typecheck | medium | Field access of ANY field on a type with no named fields — a primitive (`i64`/`f64`/`bool`/`char`), `String`, or other non-struct/non-union — was SIL… | dae02ea |
| B-2026-07-18-24 | codegen | low | Displaying an `Option[ref T]` with a SCALAR payload — the type of `Vec.first()` / `.get(i)` / `.last()` (the borrow-typed accessor, B-2026-07-14-11)… | 24471c6 |
| B-2026-07-18-25 | resolver | medium | A std-rooted `import` naming a nonexistent baked-stdlib module (`import std.math`, `import std.completelybogus`, `import std.foo.{Bar}`) was SILENTLY… | d7a5e52 |
| B-2026-07-18-26 | codegen+interp | low | The 2-arg `assert(cond, "msg")` form — accepted by the typechecker, run by the interpreter, and emitted by the COMPILER itself for tensor shape-check… | 8c2cfc4 |
| B-2026-07-18-27 | effect | medium | Assigning a captured LOCAL `let mut` binding from inside a `par { }` branch is NOT caught by the concurrency-write checker, and produces DIVERGENT ru… | 3136b0f |
| B-2026-07-18-28 | codegen | high | SILENT MISCOMPILE of the design-recommended Atomic-in-`par` escape hatch: a `par { }` block with 2+ branches that mutate a captured `Atomic[T]` write… | 3136b0f |
| B-2026-07-18-29 | codegen | high | REBUILDING or RE-WRAPPING a match-bound shared-enum payload node double-frees under AOT (interp correct): both `MethodCall(MethodCallExpr { object, m… | ea5a844 |
| B-2026-07-18-30 | codegen | medium | A `ref Atomic[T]` FUNCTION PARAMETER does not mutate the caller's atomic under codegen — `fn bump(c: ref Atomic[i64]) { c.fetch_add(1, SeqCst); }` ca… | 6057527 |
| B-2026-07-18-31 | typecheck | high | A GENERIC function returning `Option[T]` from a Vec accessor (`v.first()` / `v.last()`), monomorphized with a HEAP `T` (`String`), DOUBLE-FREES under… | 056cbb3 |
| B-2026-07-18-32 | codegen | medium | A GENERIC function body that RECONSTRUCTS a struct with heap fields, monomorphized with a HEAP `T` (`String`), emits INVALID LLVM IR (interp correct) | 62c5330 |
| B-2026-07-18-33 | typecheck+codegen | medium | `Option/Result.map` over a HEAP payload (String/Vec) now works under codegen, unblocked by fixing a chained-method span collision | 58a45ea |
| B-2026-07-18-34 | codegen | high | A fresh owned/heap ARGUMENT temp passed to a predicate call inside a `while` CONDITION leaked one allocation PER ITERATION (unbounded) under AOT/JIT… | 7bcbd47 |
| B-2026-07-18-35 | typecheck+interp | low | `SortedMap` lacked the `.entry()` API that `Map` has, so idiomatic ORDERED aggregation (`m.entry(k).and_modify(\|c\| c += 1).or_insert(1)`, `m.entry(k)… | 83ec9a5 |
| B-2026-07-18-36 | codegen | medium | A CHAINED width-sensitive integer intrinsic (`x.leading_zeros().leading_zeros()`, also `rotate_left`/`count_ones` chains) miscompiled under codegen w… | a71708d |
| B-2026-07-18-37 | codegen | high | A by-value-`self` method returning a HEAP field directly as its tail — `fn get(self) -> String { self.v }`, the canonical owned accessor — DOUBLE-FRE… | dbf8994 |
| B-2026-07-18-38 | codegen | medium | An owned HEAP param (or local) moved into a VEC/ARRAY LITERAL that is returned/bound DOUBLE-FREES under AOT (interp correct): `fn dup(x: String) -> V… | 2d60d38 |
| B-2026-07-18-39 | typecheck+codegen | high | An iterator chain whose SOURCE is a TEMPORARY Vec (a `vec![…]` literal or a call result, NOT a `let`-bound variable) SILENTLY miscompiled to 0/empty… | 8f70020 |
| B-2026-07-18-40 | codegen | medium | Displaying `Option[ref String]` — the borrow-typed result of `Vec[String].get(i)` / `.first()` / `.last()` — failed under codegen with the deferred s… | 6c76b81 |
| B-2026-07-18-41 | typecheck+interp+codegen | low | `Iterator.rev()` was unimplemented — `v.iter().rev()` rejected with `no method 'rev' on type 'Iterator'` in both backends | 9dcf1b8 |
| B-2026-07-18-42 | codegen | high | A closure that CAPTURES a whole heap String/Vec and RETURNS it double-frees under AOT/JIT (interp correct): `fn f(x: String) -> String { let g = \|\| x… | f96d2f2 |
| B-2026-07-18-43 | codegen | medium | A closure that captures a Vec and RETURNS it, then the result is INDEXED, fails codegen with `Index operator applied to non-array type` (interp corre… | aa407f2 |
| B-2026-07-18-44 | codegen | high | A GENERIC struct's owned by-value param/self whose HEAP FIELD is returned (moved out) double-frees under AOT/JIT (interp correct): `fn take[T](b: Box… | 3ea24dd |
| B-2026-07-18-45 | codegen | medium | A generic struct whose type param is bound to a WHOLE Vec (`Box[Vec[i64]]`) and whose field is returned (moved out) double-frees under AOT/JIT (inter… | 06f51f7 |
| B-2026-07-18-46 | codegen | medium | A closure that captures a whole heap-bearing STRUCT (a struct with a String/Vec field) and RETURNS it miscompiles under AOT/JIT (interp correct): `st… | e543745 |
| B-2026-07-18-47 | codegen | high | An enum METHOD with owned `self` that MATCHES its heap payload double-frees under AOT/JIT (interp correct): `impl E { fn take(self) -> String { match… | 6b23dcb |
| B-2026-07-18-48 | codegen | medium | A USER method whose name collides with a builtin Vec/String method (`get`/`take`/`unwrap`/…), called on a NON-IDENTIFIER receiver (a struct/enum LITE… | d402c8f |
| B-2026-07-18-49 | interp | medium | A user method literally named `unwrap` on a user enum/struct is mis-resolved to the builtin Option/Result `unwrap` by the INTERPRETER (prints the val… | 4c9c450 |
| B-2026-07-18-52 | codegen | high | Whole-`Vec[String]` variable reassignment (`cur = nxt`) freed only the OLD Vec's outer {ptr,len,cap} buffer and stranded every element String — the B… | 98e72be |
| B-2026-07-18-50 | typecheck | medium | A GENERIC struct literal whose field type WRAPS the type param in a container (`items: Vec[T]`, `v: Option[T]`) did NOT infer `T` from a concrete ini… | c5c13a7 |
| B-2026-07-18-51 | typecheck | medium | A GENERIC struct literal that binds the SAME type param from CONFLICTING field values was silently ACCEPTED — `Two[T] { a: 1, b: "s".to_string() }` (… | c5c13a7 |
| B-2026-07-19-1 | typecheck+interp+codegen | low | `Vec[T].dedup()` was unimplemented — `v.dedup()` rejected `no method 'dedup' on type 'Vec'` in both backends (`dedup` was already in ast.rs's mutatin… | f5156fb |
| B-2026-07-19-2 | typecheck+interp+codegen | low | `Iterator.position(pred) -> Option[i64]` was unimplemented (rejected `no method 'position' on type 'Iterator'`) | 6448f0d |
| B-2026-07-19-3 | ownership | medium | `karac check` reports a hard `error[ownership]: value 'v' moved here, used again here` for a reused OWNED heap value (`f(v); f(v)` where `v: Vec`/`St… | 98be8da |
| B-2026-07-19-4 | typecheck+interp+codegen | low | `Iterator.find(pred) -> Option[T]` was unimplemented (rejected `no method 'find' on type 'Iterator'`) | 2a48965 |
| B-2026-07-19-5 | typecheck+interp | low | String-receiver `"42".parse()` (the Rust-familiar sugar) was rejected (`no method 'parse' on type 'String'`) — only the type-receiver `i64.parse(s) -… | 89366dd |
| B-2026-07-19-6 | codegen | high | A field store of an `Option[shared]` into a Vec-INDEXED shared-struct element (identifier root — `v[i].next = Some(v[j])`, `nodes[i].field = X`) had… | 6f8f441 |
| B-2026-07-19-7 | typecheck+interp+codegen | low | `Iterator.last() -> Option[T]` and `Iterator.nth(n) -> Option[T]` were unimplemented (rejected `no method 'last'/'nth' on type 'Iterator'`) | 77cd44c |
| B-2026-07-19-8 | typecheck+codegen | medium | `weak T` struct fields are DECLARATION-ONLY: the modifier parses, type-lowers to `Type::Weak`, and satisfies the ownership cycle checker (`struct Chi… | e119392 |
| B-2026-07-19-9 | typecheck+interp+codegen | low | `Vec[T].split_off(i) -> Vec[T]` was unimplemented | 293b6b5 |
| B-2026-07-19-10 | typecheck+interp+codegen | low | `String.replacen(from, to, n) -> String` was unimplemented (only `replace` existed) | 9a21d56 |
| B-2026-07-19-11 | codegen | low | `Iterator.rev()` codegen residual (B-2026-07-18-41) — a BARE range base `(a..b).rev()` / `(a..=b).rev()` was loud-deferred to `--interp` | 20bcdc5 |
| B-2026-07-19-12 | typecheck+interp+codegen | low | `Iterator.flatten()` was unimplemented — `xs.iter().flatten()` rejected with `no method 'flatten' on type 'Iterator'` | 0425a45 |
| B-2026-07-19-13 | codegen | medium | Indexed-shared-struct field READ (`nodes[i].field`) hardcoded heap offset `idx + 1` instead of routing through `shared_gep_layout`, so it mis-read an… | 8f606de |
| B-2026-07-19-14 | typecheck+interp+codegen | medium | Iterator predicate/Option-adaptor cluster UNIMPLEMENTED: `filter_map`, `find_map`, `partition` are rejected `no method '<name>' on type 'Iterator'` i… | 242c07c |
| B-2026-07-19-15 | codegen | low | `Vec[T].sorted()` (immutable sort returning a NEW Vec) is UNIMPLEMENTED in codegen for every element type — it falls to the generic 'Vec/String metho… | c6848c4 |
| B-2026-07-19-16 | codegen | low | A DISCARDED `Map.remove(k)` result over a `Map[K, shared V]` LEAKS the removed value's RC | 47f7940 |
| B-2026-07-20-1 | typecheck+codegen | high | Iterating a `Vec` that lives in a TUPLE element (`t.0.iter()`, `t.0.iter().fold(..)`) silently iterated ZERO times under codegen (JIT + native) while… | eeee817 |
| B-2026-07-20-2 | codegen | medium | Indexing a `Vec` that lives in a TUPLE element (`t.0[i]`) fails codegen LOUD — `error: codegen failed: Index operator applied to non-array type` (JIT… | 8efca21 |
| B-2026-07-20-3 | interp+codegen | high | Index-STORE into a `Vec` that lives in a TUPLE element (`t.0[i] = v`) is DROPPED by the tree-walk interpreter (SILENT — the store is a no-op: `let mu… | b7d2bc8 |
| B-2026-07-20-4 | codegen | low | Calling a method on an indexed element of a tuple-element `Vec` (`t.0[i].len()`) fails codegen LOUD — "codegen: indexed-receiver method 'len' require… | 8cfa72a |
| B-2026-07-20-5 | codegen | low | `Iterator.partition()` codegen lowered only a trivially-copyable element and loud-deferred a HEAP element (String/Vec) to `--interp` (the documented… | ba6751f |
| B-2026-07-20-6 | ownership | low | `karac check` reports a false `error[ownership]: value 'row' moved here, used again here` for an iter_axis ROW-VIEW reused across two CHAINED `row.zi… | 62d148c |
| B-2026-07-20-7 | codegen | high | `Map[K, struct-with-heap-field].get().unwrap()` DOUBLE-FREES under codegen (JIT + AOT-O2/O0); interp correct | bd2bb92 |
| B-2026-07-20-8 | codegen | low | `Vec[T].sorted_by(cmp: Fn(T,T)->Ordering)` / `String.sorted_by` (immutable CUSTOM-COMPARATOR sort returning a new Vec/String) is UNIMPLEMENTED in cod… | a87b290 |
| B-2026-07-20-9 | codegen | high | `Vec[struct-with-heap-field].get(i)/.first()/.last().unwrap()` field read is FLAKY under codegen (JIT + AOT): `let a = v.get(0).unwrap(); print(a.nam… | b7b72eb |
| B-2026-07-20-10 | codegen | high | Every WASM program that frees a heap buffer traps at runtime (`unreachable` via a `signature_mismatch:karac_free_buf` stub) | a25a2a1 |
| B-2026-07-20-11 | codegen | medium | `karac build` fails LLVM module verification with `Invalid bitcast: bitcast float %elem to i64` for a fused f32 map-reduce whose base is an `iter_axi… | 5f5897b |
| B-2026-07-20-12 | codegen | medium | An f16 inside a COMPOUND enum payload (`Option[(f16, f16)]`) builds and runs but produces a WRONG VALUE under `karac build`: `match o { Some(p) => pr… | 35d340f |
| B-2026-07-20-13 | codegen | medium | wasm-threads has NO path for a request-driven EXPORTED fn to use the worker pool: `instantiate()` exports run on the caller's (browser main) thread b… | 7517adb |
| B-2026-07-20-14 | codegen | medium | Auto-par return-slot rebind LOSES an unannotated struct binding's type identity: `let tx = make_taps(..)` (struct-of-Vecs, no annotation) fanned out… | 29a4a97 |
| B-2026-07-21-1 | interp+codegen | medium | INTERP BUG (run-vs-build): user `impl Drop` destructors run at LAST USE in FIFO (declaration) order under the tree-walk interpreter, while codegen (J… | 7c4bf90 |
| B-2026-07-21-2 | codegen | low | `<chain>.next()` — e.g | 5a9536b |
| B-2026-07-21-3 | codegen | medium | Construction-heavy `Vector[f64,2]` patterns are a ~4.7x PESSIMIZATION on wasm: rewriting Prism's Lanczos tap loop from 4 scalar f64 accumulators to t… | 6951017 |
| B-2026-07-21-5 | codegen | high | AOT double-free: Vec[struct-with-enum-field] element bind -> ref-param call -> match on the enum field -> concat consumes the String payload binding | 94cf1c4 |
| B-2026-07-21-6 | codegen | high | JIT-only miscompile: a match-bound String payload of an enum FIELD reached through a `ref` struct param prints EMPTY when an arm CONSUMES it via conc… | 94cf1c4 |
| B-2026-07-21-7 | codegen | high | Struct-PATTERN destructure of a struct-typed FIELD reached through a `ref` param double-frees when a binding escapes: `match h.inner { Pt { s, x } =>… | 06ea22a |
| B-2026-07-21-8 | codegen | high | if-let over an enum FIELD through a `ref` param with a CONSUMING binding double-frees: `if let Ident(name) = st.tok { return "i:".to_string() + name;… | 9b2c2c8c |
| B-2026-07-21-9 | codegen | high | match on an Option[String] FIELD through a `ref` param with a consuming Some arm double-frees: `match h.opt { Some(s) => return "o:".to_string() + s,… | 7dc17d5b |
| B-2026-07-21-10 | codegen | medium | match on a TUPLE-typed FIELD through a `ref` param with a consuming binding double-frees: `match h.pair { (s, x) => return "t:".to_string() + s + … }… | 8c47c73 |
| B-2026-07-21-11 | codegen | high | whole-field `let` move of a struct-typed FIELD through a `ref` param double-frees when the copy's heap is consumed: `let p = h.inner; return "l:" + p… | d00951ad |
| B-2026-07-21-12 | codegen | low | Vec[String] field .first() consuming read through a `ref` param LEAKS the element copy at O0: `match h.items.first() { Some(s) => return "f:" + s, …… | acd2ba3 |
| B-2026-07-21-13 | codegen | high | `vec_field.push(nodes[j])` — pushing a BARE `shared struct` element read from another `Vec[shared]` (an aliasing indexed read, source still owns it)… | a4a66c5 |
| B-2026-07-21-14 | codegen | medium | match on a Result[String, i64] FIELD through a `ref` param with a consuming Ok arm double-frees under AOT (O0 and O2): `match h.res { Ok(s) => return… | 3ea6b06 |
| B-2026-07-21-15 | codegen | medium | a struct with a `Result[String, i64]`-class field leaks the live half's heap payload at the owning struct's scope-exit drop — `let a = Holder { res:… | ecf05ca |
| B-2026-07-21-16 | codegen | high | match/if-let/let-else DIRECTLY over an OWNED struct's `Option[String]` field with a payload binding double-frees under AOT — `match a.opt { Some(s) =… | f9be2c7 |
| B-2026-07-21-17 | interp | low | A runtime error raised inside a spliced gated-stdlib wrapper body reports the USER file's path with the SPLICE-COMPOSITE line/col — `import std.lazy.… | 5d5361f |
| B-2026-07-21-18 | codegen | medium | `if let Some(n) = v.pop()` / `while let` / `let…else` over a `Vec[shared T]` (or `VecDeque`) LEAKS the popped node — every element popped through a N… | 587dc94 |
| B-2026-07-21-20 | ownership | low | Spurious E0500 UseAfterMove on a weak-to-weak field splice `nodes[i].next = nodes[prev].next` where both sides index the SAME `Vec[shared]` and `next… | e1ddb43 |
| B-2026-07-21-21 | codegen | high | Materializing a `weak`-field read into a strong `Option[shared]` local and then storing it back into another `weak` field DOUBLE-FREES / use-after-fr… | e1ddb43 |
| B-2026-07-22-1 | codegen | high | test_e2e_f16_bf16_enum_payload_pack_unpack CRASHES on macOS arm64 (empty stdout vs expected '4\n1\n3.5\n1.75\n6\n') while Linux arm64 AND x86 are gre… | 50e329e |
| B-2026-07-22-2 | codegen | medium | asan_closure_captures_heap_struct_returned_clean FAILS on the arm64 LSan CI leg (memory-sanitizer-arm64) while the x86 LSan leg is green — an arm64-O… | ad55064 |
| B-2026-07-22-3 | codegen | medium | Mixed-name method chains hanging off an associated constructor failed codegen with 'no handler for method on non-identifier receiver' — `Command.new(… | same commit as the std.process codegen slice |
| B-2026-07-22-4 | interp | low | interp: `as f16` / `as bf16` casts don't round to storage precision — narrowing float casts are identity in the tree-walk interpreter, diverging from… | 6a734aa |
| B-2026-07-22-5 | runtime | low | runtime test test_try_wait_kill_reap asserts Unix signal-kill encoding (-2) unconditionally — red on every Windows CI leg (actual 2) | 07bda94 |
| B-2026-07-22-6 | codegen | medium | `s[a..b].to_string()` / `.clone()` — a `.to_string()`/`.clone()` METHOD CALL directly on a String SLICE fails codegen with "indexed-receiver method '… | 9014477 |
| B-2026-07-22-7 | codegen | high | Index-assigning an UNTYPED float literal to an f32 Tensor element silently stores nothing under AOT: `let mut a: Tensor[f32,[2]] = Tensor.zeros(vec![… | 7495530 |
| B-2026-07-22-8 | codegen | medium | Reassigning a `mut String` STRUCT FIELD leaks the OLD buffer when the field's current value was set in a PRIOR function call | 1358437 |
| B-2026-07-22-9 | codegen | high | AOT double-free: a Vec-payload enum variant (Node.Nums(Vec[i64])) COEXISTING with a String-payload variant (Ident(String)), where a String-payload TE… | bc0e99d |
| B-2026-07-22-10 | typecheck | medium | An unknown associated function on a scalar primitive type — e.g | — |
| B-2026-07-22-11 | codegen | high | Total-order float wrappers (F32/F64 shipped; F16/Bf16 planned) silently MISCOMPILE in codegen: `a > b`/`a == b` on a wrapper return const-0 (every co… | cd78554 |
| B-2026-07-22-12 | codegen | medium | Overwriting an existing key on a `Map[K, String]` / `Map[K, Vec[…]]` (and the parallel `Map.remove`) leaks the DISPLACED / removed old value's heap b… | abe0236 |
| B-2026-07-22-13 | ownership | low | Spurious `rc-fallback` (perf false-positive) for a `match`/`if let` binding that is CONSUMED EXACTLY ONCE, when the match sits inside a loop: `while… | 042f848 |
| B-2026-07-22-14 | autopar | medium | Auto-par false-negative REGRESSION from B-2026-07-22-9: the new producer-side move-hazard guard de-parallelizes a heap-owning producer whose binding… | 7a73b9d |
| B-2026-07-23-1 | codegen | medium | An early `return` out of a `for (k, v) in map` / `for x in set` loop leaks the `karac_map_iter_new` iterator handle | 1dea868 |
| B-2026-07-23-2 | codegen | medium | F32/F64 total-order wrapper: `.value` field access on a match-arm binding extracted from a USER-ENUM payload falls through to the const-0 tail -> mal… | 054b1be |
| B-2026-07-23-3 | codegen | medium | A `Map`/`Set` value bound out of a USER-ENUM variant payload loses its container type for codegen method dispatch: `match v { Table(m) => m.len() }`… | 054b1be |
| B-2026-07-23-4 | codegen | high | Matching a fresh-temp `Result[_, _]` (a direct function-call return) whose extracted payload is a STRUCT with a heap field (String/Vec), and reading… | 45a501a |
| B-2026-07-23-5 | typecheck+codegen | medium | A generic fn whose return type PERMUTES the struct's type params — `fn swap[A,B](p: Pair[A,B]) -> Pair[B,A] { Pair { first: p.second, second: p.first… | a0f12e9 |
| B-2026-07-23-6 | codegen | high | SILENT MISCOMPILE in the selfhost codegen PORT (selfhost/src/codegen.kara emitter, NOT the seed): OR-PATTERNS `A \| B \| C =>` matched only the FIRST a… | 990ba16 |
| B-2026-07-23-7 | codegen | high | SILENT MISCOMPILE in the selfhost codegen PORT (codegen.kara emitter): MATCH GUARDS `Pat if <cond> =>` were IGNORED entirely — the emitter always too… | 73b3e69 |
| B-2026-07-23-8 | codegen | high | SILENT MISCOMPILE in the selfhost codegen PORT (codegen.kara emitter): `loop {}`, `break`, and `continue` were UNHANDLED — a `loop { … break }` emitt… | fa25a34 |
| B-2026-07-23-9 | codegen | high | SILENT MISCOMPILE in the selfhost codegen PORT (codegen.kara emitter): `for x in <vec>` element iteration was UNHANDLED — the loop emitted nothing/0… | 57ea46e |
| B-2026-07-23-10 | codegen | medium | CODEGEN-GAP in the selfhost codegen PORT (codegen.kara emitter): `i64.to_f64()` was unimplemented — the method returned the i64 receiver unchanged (k… | e2aaaa9 |
| B-2026-07-23-11 | codegen | medium | A user enum carrying a `Map`/`Set`(-family) payload leaks the handle at scope-exit drop: the enum drop walker has no Map/Set arm, so the whole kv-tab… | a609803 |
| B-2026-07-23-12 | interp | low | `mut ref` enum-payload `Map` mutation does not write through in the interpreter (interp keeps the pre-mutation size) while codegen writes through cor… | 54b76cb |
| B-2026-07-23-13 | codegen | high | SEED codegen double-free: `if let Variant(t) = e` over an OWNED-VARIABLE user-enum scrutinee with a heap (String/Vec) payload re-freed the payload | 8b1b47b6 |
| B-2026-07-23-14 | codegen | medium | Returning a `Map`/`Set`(-family) value moved OUT of an enum payload fails codegen module verification: `fn unwrap(v: V) -> Map[K,V] { match v { Table… | 178a193 |
| B-2026-07-23-15 | parser | high | SELFHOST PARSER (parser.kara, Phase-12 port): the `expr as TYPE` cast operator was UNHANDLED in `parse_expr_bp` (only import-alias `as` existed) | 3afd613 |
| B-2026-07-23-16 | codegen | high | A PLAIN struct pattern whose field sub-pattern is an enum-variant pattern — `match it { Item { shape: Shape.Circle(r), . | df1b68c |
| B-2026-07-23-17 | resolver | medium | An undefined name inside a `requires`/`ensures` contract expression passes `karac check` and ICEs at runtime ('variable … not found … should be caugh… | e2fd788 |
| B-2026-07-23-18 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): UNARY NEGATION `-x` was a silent NO-OP | c979710 |
| B-2026-07-23-19 | parser | high | A parenthesized single pattern `(P)` is parsed as a 1-element tuple `Tuple([P])` instead of a grouping, so `x @ (1 \| 2 \| 3)` and top-level `(1 \| 2)`… | 71cf9bff |
| B-2026-07-23-20 | autopar | high | Auto-par miscompiles a reduction loop whose body passes a LOOP-INVARIANT shared `mut ref` scratch buffer to a helper | fa53107 |
| B-2026-07-23-21 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): the BITWISE and SHIFT operators `& \| ^ << >>` were ALL emitted as `add` | 8a5fec2 |
| B-2026-07-23-22 | parser | medium | SELFHOST PARSER (parser.kara, Phase-12 port): COMPOUND ASSIGNMENT `x += v` / `-=` / `*=` / `/=` / `%=` is SILENTLY DROPPED — a no-op with no diagnost… | ff05918 |
| B-2026-07-23-23 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): INDEXED ASSIGNMENT `v[i] = value` was SILENTLY DROPPED | 53f2553 |
| B-2026-07-23-24 | codegen | medium | SELFHOST EMITTER (codegen.kara, Phase-12 port): `Vec[bool]` has NO distinct element kind — it is conflated with `Vec[i64]` (kind 3), so the i1/i64 la… | 0498e79 |
| B-2026-07-23-25 | autopar | medium | Auto-par over-parallelizes a fine-grained inner loop, making the DEFAULT `karac build` catastrophically slow (~1000x) while output stays CORRECT | c702d61 |
| B-2026-07-23-26 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): statement-position `v.pop()` was a NO-OP — a non-push statement method call fell through to `emit_met… | 21a6eed |
| B-2026-07-23-27 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): statement-position `s.push_str(arg)` was a NO-OP — it fell through to `emit_method_value`, which does… | bd6af74 |
| B-2026-07-23-28 | codegen | high | Calling a closure bound as a `for`-loop element returns 0 under `karac build`/JIT (interp correct) | efc5d46 |
| B-2026-07-23-29 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): a FIELD-receiver Vec method call — `self.<vec>.push(x)` / `self.<vec>.pop()` — was a NO-OP | b686bdb |
| B-2026-07-24-1 | codegen | medium | Compiler-driven inline-hint pass runs on the LOWERED AST but its node-count thresholds are calibrated for RAW-source sizes, so a small loop-hot helpe… | 20d33f7 |
| B-2026-07-24-2 | codegen | medium | `for k in map.keys()` (and `.values()`/`.entries()`) eagerly materializes the whole key/value set into a fresh heap `Vec` on every evaluation, so a `… | bef6bbc |
| B-2026-07-24-3 | typecheck+interp | medium | `u8 as char` was rejected at typecheck with E_INT_AS_CHAR even though it is the ONE infallible integer→char cast — every byte (0..=255) is a valid Un… | 6b0c98a |
| B-2026-07-25-1 | codegen | high | USE-AFTER-FREE under codegen: a RECURSIVE function taking an OWNED `String` parameter whose argument is an ELEMENT of a `Vec[String]` obtained from a… | — |
| B-2026-07-25-2 | typecheck | low | Match-arm type unification does not bind an unsolved type variable inside a generic: `match m.get(k) { Some(d) => d, None => Vec.new() }` is rejected… | 9226dff |
| B-2026-07-25-4 | codegen | medium | `for k in m.keys()` (and `.values()`/`.entries()`) STILL eagerly materializes an owned `Vec` per evaluation when either map half is a HEAP type (`Map… | 15dcfac |
| B-2026-07-25-5 | codegen | medium | Indexed-receiver method call on a MAP element (`m[k].push(x)`) type-checks and runs correctly under the tree-walk interpreter but is REJECTED by code… | 4416d33 |
| B-2026-07-26-1 | codegen | medium | Overflow check on a BOUNDED loop-accumulator (`cnt = cnt + 1` guarded by an if, inside a counted loop) is not elided, and because a checked add can t… | 762865a |
| B-2026-07-26-2 | codegen+runtime | medium | Map/Set probes called `eq_fn` on EVERY occupied bucket they walked past, because the status byte carried no hash information — one cold key dereferen… | 58412d9 |
| B-2026-07-27-1 | codegen | high | SEED codegen double-free: `return <struct>.<vecfield>[i];` — returning a heap (String) element of a STRUCT-FIELD Vec directly as a `return` STATEMENT… | 42d8e96 |
| B-2026-07-27-2 | codegen | medium | Struct-FIELD `Map[_, Vec[String]]` leaks the inner Vec's element buffers at drop | 44b606e |
| B-2026-07-27-3 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): a BOOL-valued `if` or `match` in VALUE position emitted INVALID IR | 14a65d4 |
| B-2026-07-27-4 | codegen+cli | medium | No working per-function / per-line PROFILING attribution for an AOT kara binary | f569aac |
| B-2026-07-27-5 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): value-position `if` and `match` used a HARDCODED `alloca i64` result slot, so a STRING / struct / enu… | 304e86c |
| B-2026-07-27-6 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): a heap-bearing payload inside a `shared enum` could not always be released | dd14643 |
| B-2026-07-27-7 | codegen | medium | `for ch in <str>.chars()` over a COMPILE-TIME-CONSTANT string runs ~21.7x the instructions of the identical Rust source, because the ASCII/multibyte… | de5da39 |
| B-2026-07-27-8 | codegen | high | `continue` inside `for ch in <s>.chars()` is an INFINITE LOOP in compiled code on every backend (JIT and AOT, -O0 and -O2), because the general chars… | 6f1c706 |
| B-2026-07-27-9 | ownership | medium | A non-`mut` `let` binding can be REASSIGNED and MUTATED IN PLACE with no diagnostic — `let s: String = "abc"; s = "xyz";` and `let s: String = "abc";… | 6a855bdc |
| B-2026-07-27-10 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): a DESTRUCTURING `let S { a, b } = s;` bound NOTHING — the Let handler matched only a plain BindingPat… | 7bd2de1 |
| B-2026-07-27-11 | codegen | high | SELFHOST EMITTER (codegen.kara, Phase-12 port): `.clone()` is UNIMPLEMENTED and silently lowers to the i64 default `0` — `let b = a.clone()` on a Str… | d7707de |
| B-2026-07-27-12 | codegen | medium | SELFHOST EMITTER (codegen.kara, Phase-12 port): a `to_string()` result produced INSIDE a match arm and returned from a fn leaks — the caller never fr… | 416245cc |
| B-2026-07-27-13 | cli | medium | `karac explain` REJECTED the diagnostic code the compiler itself emits: every structured diagnostic carries `"code": "E0200"`, but `karac explain E02… | e211e3d |
| B-2026-07-27-14 | cli | medium | FOUR diagnostic codes are minted by TWO DIFFERENT PHASES for unrelated errors: E0222 is both resolve `PrivateItemAccess` and typecheck `RefutablePatt… | 074377c |
| B-2026-07-27-15 | codegen | high | SELFHOST EMITTER: a heap-owning payload bound by a `match` arm is bound as a BORROW, so returning or consuming it hands out a buffer the scrutinee's… | 416245cc |
| B-2026-07-28-1 | other | medium | stale (not missing) runtime archive soft-skipped every E2E link failure — ~960 codegen assertions silently voided, suite reported green | 1d4ddd9 |
| B-2026-07-28-2 | codegen | medium | `for ch in <runtime-string>.chars()` cannot take the branch-free stride-1 loop, because that lowering requires a compile-time all-ASCII proof (B-2026… | d73e5fb |
| B-2026-07-28-3 | codegen | high | A plain (non-`shared`) struct that reaches ITSELF through a `Vec` field OVERFLOWS THE COMPILER'S STACK during codegen — every adjacency-list graph (`… | cb38efe |
| B-2026-07-28-4 | codegen | high | THREE of the five `examples/tangle` programs - the flagship ownership-soundness dogfood corpus - do not match the expected output their own README do… | edde4a7 |
| B-2026-07-28-5 | codegen | high | SELFHOST EMITTER (codegen.kara): `Option[<shared enum>]` has no kind of its own — `kind_of_ty` special-cases only `Option[String]` and collapses ever… | 4afbc56 |
| B-2026-07-28-6 | codegen | high | Assigning through a `shared struct` field of a plain struct (`h.cell.value = v`) writes into the FIELD SLOT instead of through the RC handle, so the… | 85fc58c |
| B-2026-07-28-7 | interp | high | INTERPRETER: an assignment to the scrutinee inside a match arm is SILENTLY REVERTED — `match cur { Some(n) => { cur = n.next } }` leaves `cur` at its… | 7e1729f |
| B-2026-07-28-8 | codegen | high | CODEGEN: storing into a PLAIN (value-type) struct's `Option[shared T]` field drops the old value BEFORE retaining the new one, so an aliasing RHS (`h… | fa8ae31 |
| B-2026-07-28-9 | codegen | high | CODEGEN: a whole-struct MOVE does not null a `shared struct` / `shared enum` HANDLE field, so the moved-out source's drop rc-dec's a second time for… | fab2ba3 |
| B-2026-07-28-10 | interp | medium | INTERP: `Column.from_arrow_ipc` (and the Column constructors generally) ignore the binding's declared element type, so `let c: Column[i64] = Column.f… | b72fab5 |
| B-2026-07-28-11 | codegen | high | CODEGEN: `Vec[<struct carrying a shared handle>].clone()` does not retain the cloned elements' handles, so the clone's drop and the original's drop b… | a4711ca |
| B-2026-07-28-12 | codegen | medium | CODEGEN: `println(<Vec expression>)` renders EMPTY (or a stray tab) when the operand is not an identifier — `println(vec![9i64, 8i64])`, `println(t.s… | — |
| B-2026-07-28-13 | autopar | high | explicit `par {}` over 18 arms that all READ one shared graph crashes nondeterministically (~0.8%): silent exit 133 (SIGTRAP) and 139 (SIGSEGV), empt… | c1eeed5 |
| B-2026-07-28-14 | codegen | medium | SELFHOST EMITTER (codegen.kara): `Option[<plain struct>]` has no representation — `kind_of_ty` collapses it to `Option$i`, whose payload slot is an i… | 46acf5b |
| B-2026-07-28-15 | codegen | high | SELFHOST EMITTER (codegen.kara): no IMPORT resolution — a type named by an `import` hits `kind_of_ty`'s i64 fallback SILENTLY, so an imported struct… | 04964fa |
| B-2026-07-28-16 | codegen | high | SEED CODEGEN: an owned struct's `Option`/`Result` field passed BY VALUE to a parameter that owns it was not treated as a move — the callee freed the… | 6dc5854 |
| B-2026-07-28-17 | codegen | high | SEED CODEGEN: passing an `Option[<heap>]` field of a struct bound out of a `shared enum` payload to an owning parameter double-frees — `match a { V(n… | 09ffe5a |
| B-2026-07-29-1 | codegen | medium | SELFHOST EMITTER (codegen.kara): `==` with a BOOL operand emits an i64 compare against an i1 value — `icmp eq i64 %t, false` — which LLVM rejects | 5e7224b |
| B-2026-07-29-2 | codegen | high | SELFHOST EMITTER (codegen.kara): a match-expression RESULT slot is allocated `i64` in `Parser.collect_leading_doc_comments` while an arm yields a Str… | 5e7224b |
| B-2026-07-29-3 | resolver | high | A `_test.kara` companion that imports the module it tests is reported as a circular module dependency (`E0223`, bogus `thing → thing`), so the idioma… | 93d14ee |
| B-2026-07-29-4 | codegen | medium | SELFHOST EMITTER (codegen.kara): `panic` / `todo` / `unreachable` were lowered as USER FUNCTION CALLS | 5e7224b |
| B-2026-07-29-5 | codegen | high | SELFHOST EMITTER (codegen.kara): an UN-ANNOTATED `let v = Vec.new()` defaults to Vec[i64], so pushing a String stores a 16-byte `{ptr,i64}` into an 8… | 5e7224b |
| B-2026-07-29-6 | codegen | medium | SELFHOST EMITTER (codegen.kara): a value-position `match` over a HEAP-PAYLOAD enum leaks the payload the arm binds | 72f7a0e |
| B-2026-07-29-7 | codegen | high | CODEGEN: `Vector[T, N]` bindings and arithmetic do not survive the STDLIB compilation unit — identical code compiles and runs correctly in a user mod… | fa94dc4 |
| B-2026-07-29-9 | typecheck | high | A trait bound whose trait is IMPORTED resolves no methods: `S: Doer` on an imported `Doer` reports no "unknown trait", yet every call through the bou… | 155f7b3 |
| B-2026-07-29-10 | typecheck | medium | An ALIASED imported trait bound is rejected at bound satisfaction: `import doer.Doer as D` + `fn go[T: D](...)` reports "trait bound `T: D` is not sa… | fa94dc4 |
| B-2026-07-29-11 | other | medium | docs/design.md — the AUTHORITATIVE spec — showed the abandoned Rust-style `Display` in three code blocks, including a full `fn fmt(ref self, f: mut r… | 3e21d84 |
| B-2026-07-29-12 | codegen | medium | CODEGEN DIAGNOSTIC: direct use of a `-> ref T` method result is a KNOWN deferral with a precise error ('borrow-returning method call `.m(...)` must b… | fa94dc4 |
| B-2026-07-29-13 | codegen | low | PERF: `Tensor.iter_axis(n)` materialized every sub-tensor as a COPY | d2076a6 |
| B-2026-07-29-24 | codegen | low | The BOUND spelling `let rows = t.iter_axis(0); for row in rows` does not fuse — only the direct `for row in t.iter_axis(0)` does — so ordinary user c… | 2abfcdc |
| B-2026-07-29-14 | typecheck+codegen | medium | AOT `karac build` flattens the module tree and DROPS the import declarations, which erases every ALIAS binding with them — `import m.{T as A}` leaves… | e9ba3cc |
| B-2026-07-29-22 | typecheck | medium | An importing module's per-module env carries NO impl entries from the module it imports from, so a trait bound on an imported type is rejected whenev… | 1027a61 |
| B-2026-07-29-23 | typecheck+codegen | low | An aliased FUNCTION import (`import m.{mk as build}`) still fails the flattened AOT build with `resolve: undefined name` | 1027a61 |
| B-2026-07-29-15 | codegen | medium | A method on a borrow-returning call result (`h.label().len()` where `label() -> ref String`) had no leak-free lowering, because the materialize-the-b… | aa7dc81 |
| B-2026-07-29-16 | ownership | high | OWNERSHIP FALSE POSITIVE in the single-file `karac check` path: a `mut` binding initialized from a FREE FUNCTION and then passed to an IMPORTED `ref`… | 3f325b4 |
| B-2026-07-29-17 | resolver | medium | RESOLVER HOLE: `effect resource R: SomeTrait;` accepts a trait name that is not defined ANYWHERE — no error, all checks pass | 9ddb52e |
| B-2026-07-29-18 | interp | medium | `env.args()` reported the HOSTING PROCESS's argv rather than the program's, differently on each executor: `karac run --interp prog.kara` gave [karac,… | 8db4602 |
| B-2026-07-29-19 | other | medium | docs/design.md — the AUTHORITATIVE spec — writes `ref` AT CALL SITES in 9 code blocks, a form the parser rejects outright ('`ref` is not written at c… | d8fd573 |
| B-2026-07-29-20 | codegen+interp | high | REPL cross-cell value snapshot was taken at each binding's INITIALIZER, so EVERY in-cell mutation was lost crossing to the next cell — on both lanes… | 8172bae |
| B-2026-07-29-21 | codegen | high | Every call to a `-> ref String` accessor leaks one `karac_string_clone` block under AOT | b74a9a7 |
| B-2026-07-29-25 | typecheck | high | An IMPORTED type alias is not expanded to its underlying type: `import types.Row;` where `pub type Row = Map[String, i64]` leaves `Row` nominal, so `… | 0a23cf8 |
| B-2026-07-29-26 | typecheck | medium | A GENERIC REFINEMENT alias is opaque to its base's operations: `type NonEmpty[T] = Vec[T] where self.len() > 0;` then `for r in rows` binds `r` to `N… | 5445e85 |
| B-2026-07-29-27 | typecheck | high | `#[derive(Clone)]` synthesizes NO callable `.clone()` method: `s.clone()` on a derived struct or enum fails with `no method 'clone' on type 'S'`, whi… | 91c43cfa |
| B-2026-07-29-28 | ownership+codegen | medium | A call's PARAMETER MODES change with syntactic position: a bare (owning) param is CONSUMED in statement position but only BORROWED inside an f-string… | c8bce5d3 |
| B-2026-07-29-29 | autopar+cli | medium | `karac query concurrency` reports statement-level parallel groups but NOT loop-reduction fan-out, so the Tier-2 decision the entire kata parallel lan… | 6936f1ab |
| B-2026-07-29-30 | autopar | low | `#[par_unordered]` asserts a behaviour that does not occur: it is the required opt-in for Collect loop fan-out, but BOTH Collect paths preserve itera… | 05b1fe66 |
| B-2026-07-29-31 | typecheck | medium | `.clone()` is callable ONLY on the built-in heap/RC types | 91c43cfa |
| B-2026-07-29-32 | codegen | medium | A FRESH-TEMP (non-`let`-bound) `Vec` argument passed to a `Slice[T]` parameter fails LLVM MODULE VERIFICATION under `karac build`, while the tree-wal… | 0c3fea05 |
| B-2026-07-29-33 | autopar+cli | low | `query concurrency`'s `loop_reductions` reports the ANALYSIS decision, not codegen's final verdict: a recognized `parallel_fanout` loop can still be… | 795fb62c |
| B-2026-07-29-34 | codegen+cli | medium | REGRESSION SURFACE: `test_build_debug_info_keeps_symbols_and_dwarf` — the test pinning B-2026-07-27-4 (profiling attribution, marked FIXED at f569aac… | 795fb62c |
| B-2026-07-29-35 | codegen | high | SILENT WRONG OUTPUT (run-vs-build): a GENERIC `fn f[T](s: Slice[T]) -> T` returning a HEAP element yields an EMPTY/garbage value under `karac build`… | 5f2cb6ad |
| B-2026-07-29-36 | ownership | low | The use-after-move suggestion advised `.clone()` for EVERY type: 'clone 'x' at the move site (`x.clone()`), declare the callee parameter `ref` if it… | 697e6de8 |
| B-2026-07-29-38 | ownership+interp+codegen | high | PREMATURE DROP on move-out: a binding moved into an aggregate literal is dropped AT THE MOVE POINT, because the shared NLL last-use analysis is a pur… | 1a536d29 |
| B-2026-07-29-39 | ownership+interp+codegen | high | An aggregate NEVER runs its fields' user `impl Drop` when it dies: a struct holding a Drop-implementing value drops nothing at scope exit, so any res… | 10c1763a |
| B-2026-07-30-1 | autopar | medium | Auto-par's cross-task-safety gate is TYPE-based, so it declines a reduction whose body allocates an iteration-LOCAL shared value (kata #23, #86 par l… | 05b1fe66 |
| B-2026-07-30-2 | codegen+runtime | medium | Vec.sort_by calls the comparator through a function pointer, so sort-bound katas track C's qsort instead of Rust's monomorphized sort (~2x) | 0139427 |
| B-2026-07-30-3 | codegen | high | CRASH (run-vs-build): an `Array[T, N]` argument passed to a GENERIC `ref Slice[T]` parameter builds and then aborts — SIGTRAP (133) with literal elem… | f0395667 |
| B-2026-07-30-4 | codegen | medium | #3629 bfs_sieve trails C on the sequential lane — the buffer cache's park withholds a large chunk and glibc's SUBSEQUENT SMALL allocations degrade 2.… | 507a446 |
| B-2026-07-30-5 | codegen | medium | VecDeque.pop_front is O(n) (memmove per pop), making a DEEP queue drain O(n^2) — fixed by a head-index lowering for eligible locals (767x on a depth-… | 60733f7 |
| B-2026-07-30-6 | codegen | high | CRASH (run-vs-build): an `Array[T, N]` REF PARAM forwarded to a `Slice[T]` parameter segfaults under `karac build` while the interpreter is correct | f0395667 |
| B-2026-07-30-7 | codegen | high | A PLAIN type alias whose base is not `i64`-shaped, used as a parameter or return type, lowers to the `i64` unknown-name fall-through in codegen: `typ… | b528fa2 |
| B-2026-07-30-8 | autopar | low | Auto-par's early-exit gate declines a reduction for a `break`/`continue` that targets a NESTED loop and therefore cannot exit the reduction body at a… | 3993c2e |
| B-2026-07-30-9 | codegen | low | `println` is not line-atomic across tasks: it emits TWO separate `write_console` calls (payload, then the newline), so two spawned tasks printing con… | 36a7fa5 |
| B-2026-07-30-10 | typecheck+codegen | low | `Result[T, E].clone()` is still rejected at typecheck (`no method 'clone' on type 'Result'`) after B-2026-07-29-31 gave `Option[T]` a callable one —… | 4b5f811 |
| B-2026-07-30-11 | interp+codegen | high | A user `impl Drop` body ran only for a value reachable from a direct binding through STRUCT FIELDS | 8de98fe |
| B-2026-07-30-12 | codegen | medium | A by-value owned-struct arg that the callee RETURNS mishandled its caller temp TWO ways: a fn-call arg (`pass(mk())`) leaked the orphaned buffer, and… | 3ee768a |
| B-2026-07-30-13 | typecheck | low | An arithmetic mismatch whose wrong operand is an Option/Result was reported as an integer/floating-point mix, naming a float type that appears nowher… | 3981fef |
| B-2026-07-30-14 | typecheck | high | A MAP LITERAL typed as `HashMap<K, V>` — a name Kāra source cannot write — so it matched no annotation: `let m: Map[String, i64] = Map["x": 1];` fail… | af9a99a9 |
| B-2026-07-30-15 | codegen+interp | high | `Json` has no integer variant — every i64 serializes through f64, so whole numbers emit `1.0` (which Go's encoding/json REFUSES into an int field) an… | 78a0623 |
| B-2026-07-30-16 | typecheck+codegen+interp | high | `File.sync_all()` / `sync_data()` — the durability API design.md:9150 mandates — TYPECHECKS CLEAN but is unimplemented in codegen AND the interpreter… | 38808e0 |
| B-2026-07-30-17 | typecheck | medium | Map LOOKUP (`get` / `get_or` / `contains_key` / `remove`) rejected a borrowed key: `m.get(key)` where `key: ref String` failed with `expected 'String… | 097f0a4 |
| B-2026-07-30-18 | codegen | high | SILENT WRONG OUTPUT + OUT-OF-BOUNDS READ (run-vs-build): a `ref Slice[T]` / `ref Vec[T]` PARAM forwarded to a BY-VALUE `Slice[T]` parameter is read a… | ccf4053a |
| B-2026-07-31-1 | typecheck | high | NO METHOD-NAME CHECKING on baked-stdlib handle types — `f.totally_bogus_method_xyz()` on a `File`, `TcpStream`, `Mutex`, `Regex`, `Pool`, … typecheck… | 103fbe4 |
| B-2026-07-31-2 | typecheck | medium | `Ok(x)` / `Err(x)` / `Some(x)` did not push the expected PAYLOAD type inward, so a type-inferred constructor in the payload never resolved: `fn f() -… | dcdc9e1 |
| B-2026-07-31-3 | typecheck | high | CONTRADICTORY RULES: E_ENUM_NESTED_ENUM_PAYLOAD said to mark a nested inner enum `shared`, while E_NOT_CROSS_TASK said a `shared enum` cannot cross a… | 895d4da |
| B-2026-07-31-4 | interp | high | INTERPRETER LOSES PROVIDER MUTATIONS: a `mut ref self` provider method called inside `with_provider` has no visible effect — two `bump()` calls read… | 2216c86 |
| B-2026-07-31-5 | codegen | low | A value enum's own `impl Drop` body fired at SCOPE EXIT under codegen but at the binding's NLL live-range end under the interpreter — `mid\|DS` vs `DS… | 09459b2 |
| B-2026-07-31-6 | codegen | medium | A `Result[T, E]` whose payload is HEAP-BOXED got the INLINE payload cleanup registered ON TOP of the correct box drop, so the inline overlay read the… | d5f45d6 |
| B-2026-07-31-7 | codegen | medium | An UNANNOTATED `let` of an `Option`/`Result` whose payload is heap-BOXED registered no cleanup at all and leaked the whole box, payload heap included… | d5f45d6 |
| B-2026-07-31-8 | codegen | high | An INVERTED loop range (`for y in 5..3`) makes auto-par's `iter_total` negative, and the descriptor field is `u64` — so a loop that runs ZERO iterati… | db59de2 |
| B-2026-07-31-9 | codegen | medium | The `providers { R => v } in { body }` BLOCK form emits NOTHING under codegen/JIT — even a read-only `R.get()` — while the interpreter runs it correc… | 74d2b387 |
| B-2026-07-31-10 | codegen | medium | `body_is_memory_bound` classifies a 7-tap vector CONVOLUTION as memory-bound — it keys on "has an index read and no substantial CALL" and cannot see… | f068f0e |
| B-2026-07-31-11 | codegen | medium | An early `return` out of a provider body — `with_provider`'s closure OR the `providers { } in { }` block — cannot be compiled: the `karac_provider_po… | 9cfd715 |
| B-2026-07-31-12 | codegen | medium | `Json.parse` of a STRING / ARRAY / OBJECT leaks the lifted Kāra-side tree's heap payloads under codegen — one tree per parse, even when the payload i… | 66ec99f |
| B-2026-07-31-13 | autopar+cli | medium | `codegen/reduce.rs` open-coded its own copy of the fan-out gate sequence instead of calling `par_cost::fanout_verdict` — the exact drift `par_cost` w… | 269d9ce |
| B-2026-07-31-14 | autopar+cli | medium | `karac query concurrency` reports `fanned_out: true` / `cost_gate: "fanout"` for a reduction with a FLOAT accumulator, which codegen always refuses t… | 7375ecf |
| B-2026-07-31-15 | interp+typecheck | medium | `break` out of an enclosing loop from inside a `with_provider` closure body makes the interpreter return UNIT from an i64 function — the Break contro… | c18c457 |
| B-2026-07-31-16 | codegen | low | `return` inside a `with_provider` CLOSURE body is closure-scoped per the typechecker and interpreter (`let x = with_provider[R](v, \|\| { return 8; });… | 7cd69b0 |
| B-2026-07-31-17 | codegen | low | A let-bound value expression that TERMINATES (`let x = { return 5; }; tail`) fails module verification under codegen — the let-site emits its store a… | e92c16b |
| B-2026-07-31-18 | typecheck | low | The typechecker types a `return E` inside a `with_provider(p, \|\| { .. | 64c4df5 |
| B-2026-07-31-19 | codegen | medium | `?` inside a `with_provider` CLOSURE body lowers as a FN-LEVEL early return of the Err, but per design.md (body is `Fn() -> T`) and the interpreter i… | 7190a77 |
| B-2026-07-31-20 | codegen | medium | codegen never registers heap-type metadata (string_vars / vec_elem_types / var_type_names) for a `let` binding whose RHS is a with_provider call, so… | a710d6c |
| B-2026-07-31-21 | runtime | high | Map/Set capacity grows linearly with TOTAL removals, not live size -- a sliding-window map leaks memory without bound (297 MB where Rust holds 2.4 MB) | 75a3a92 |
| B-2026-07-31-22 | interp+codegen | high | A whole-value MOVE of a binding carrying a container-bodies walk (enum payload / Vec element / tuple element) left the source's __karac_dropelems_* a… | ef85e85 |
| B-2026-07-31-23 | typecheck | high | B-2026-07-31-1 PRONG 3 — the four HTTP hardcoded arms (`Client` / `RequestBuilder` / `Response` / `HttpError`) still accepted any far-from-anything m… | 5e7c2e9 |
| B-2026-07-31-24 | interp | high | `Client.request(m, url)` and the ENTIRE `RequestBuilder` chain (`header`/`body`/`timeout`/`send`) had no interpreter dispatch arm — typechecks clean,… | 5e7c2e9 |
| B-2026-07-31-25 | interp | medium | Interpreter captured ONLY `content-type` off an HTTP response, so `Response.header(name)` answered `None` for every other header and `Response.header… | 5e7c2e9 |
| B-2026-07-31-26 | codegen | low | STALE CODEGEN-GAP DIAGNOSTICS: 15 codegen error/doc sites say a deferred construct "works under `karac run`" — false since `karac run` became JIT-by-… | 9d70e52 |
| B-2026-07-31-27 | codegen | medium | A whole-map rebind (`let m5 = m4;`) leaks the ENTIRE map — handle, bucket arrays, and every stored value's heap — on every execution of the shape. | 193956f |
| B-2026-07-31-28 | ownership | medium | RC-fallback analysis is not path-sensitive across a branch JOIN: a value consumed exactly once on each of two MUTUALLY EXCLUSIVE `if`/`else` (or `mat… | 35723fc9 |
| B-2026-07-31-29 | ownership+cli | high | `karac build` and `karac run` DO NOT GATE on ownership errors: a use-after-move that `karac check` reports as `error[ownership]` (exit 1) still compi… | ef7e209 |
| B-2026-07-31-30 | codegen | high | `for x in <module-level Vec/Map/Set/String>` compiled to a zero-iteration loop (run-vs-build divergence) | e0fdc44 |
| B-2026-07-31-31 | parser+typecheck | medium | Mechanical diagnostics that name their own replacement carry no `karac fix` payload (`!` -> `not`, String -> StringSlice) | 49dc24c |
| B-2026-07-31-32 | resolver+parser | medium | E_MODULE_BINDING_NAMING suggests renaming a single-uppercase-letter binding to itself | 41cb78a |
| B-2026-07-31-33 | resolver+cli | medium | `karac fix` renames a module binding's DECLARATION only, breaking every use site | 0c1c599 |
| B-2026-07-31-34 | codegen+runtime | low | 17 codegen-declared runtime symbols are absent from `__preserve_no_mangle_symbols` — each needs a per-symbol strip-risk verdict (async/net/TLS famili… | (no code change — verified not at risk) |
| B-2026-07-31-35 | autopar+codegen | high | An eligible head-index deque in a function auto-par splits fails the BUILD: the __par_branch_* compile inherits the outer function's name-keyed head… | 6ed7439 |
| B-2026-07-31-36 | cli | medium | `karac build -o <path>` / `--out <path>` was PARSED BUT SILENTLY IGNORED for executable builds — the artifact always landed at `<stem>` in the CWD | — |
| B-2026-07-31-37 | interp+codegen | high | `a = b` on a struct binding runs b's user `impl Drop` body TWICE on every backend — and on a heap-bearing type codegen's second run reads the MOVED-F… | 1276227 |
| B-2026-07-31-38 | codegen | medium | A binding moved into a variant constructor and then REASSIGNED never drops again on codegen: the ctor move retracts its UserDrop action permanently a… | 1276227 |
| B-2026-07-31-39 | codegen | medium | Reassigning a struct binding never frees the OLD value's field heap — `let mut a = S{..String..}; a = S{..};` orphans the first String on every execu… | 1276227 |
| B-2026-07-31-40 | autopar+codegen | medium | A Map introduced INSIDE an auto-par parallel group leaks whole (handle + buckets) at branch write-back — 344 bytes definitely lost per execution of t… | 06cbc7a |
| B-2026-07-31-41 | autopar+codegen | medium | User Drop timing diverges in auto-par lanes: under the DEFAULT build a displaced struct binding's body fires with the wrong id/time (drop 6 at the as… | dcb9b7a |
| B-2026-07-31-42 | autopar | medium | The auto-par cost model has no notion of memory ACCESS PATTERN, only of statement count | f56f958 |
| B-2026-07-31-43 | codegen | low | A chained method call on a fresh container-returning builtin receiver leaks the receiver temp: `let k = Env.args().len();` leaks the args Vec[String]… | 53975c0 |
| B-2026-07-31-44 | other | medium | 554 `--features llvm`-gated tests across 19 targets never run in CI — including par_codegen (220) and drop_differential (13) | 9013b32 |
| B-2026-07-31-45 | interp+codegen | medium | let-else with a moved Drop-bearing enum payload: the interpreter runs the payload's Drop body TWICE (once BEFORE the binding is even used) while AOT… | 2a8f4a7 |
| B-2026-08-01-1 | codegen | medium | Chained get/first().unwrap() then len() on a fresh-temp Vec[Vec[scalar]] receiver double-frees the borrowed row: let n = mk_rows().first().unwrap().l… | 54ac148 |
| B-2026-08-01-2 | interp+codegen | medium | wildcard-let user-enum discard fires the Drop payload body under karac run but not karac build (+ own-Drop-enum and erased-generic sibling divergence… | 6915b47 |
| B-2026-08-01-3 | codegen | medium | enum reassign never frees the displaced payload's interior heap — Full(Res{name: String}) leaks the old String on every assignment (DCE-masked until… | fff43cb |
| B-2026-08-01-4 | codegen | medium | fresh Drop-bearing call-arg temp in let position fires its body at scope exit (owned param) or never (ref param) under karac build; the interpreter f… | d88686c |
| B-2026-08-01-5 | interp+codegen | medium | fresh method-RECEIVER Drop temps: interp never fires their bodies, karac build fires them at scope exit — and owned-self passthrough chains DOUBLE-fi… | 79f99b0 |
| B-2026-08-01-6 | interp+codegen | low | match on a borrowed `ref self` enum receiver binding the Drop payload fires the payload body under karac run only (arm channel treats the borrowed vi… | 1881798 |
| B-2026-08-01-7 | interp+codegen | low | owned-self method matching its payload out double-fires the payload Drop body — on BOTH backends (receiver binding's walk + the arm channel each fire… | a598112 |
| B-2026-08-01-8 | interp+codegen | medium | mixed fresh+place tuple discard (`let _ = (r, 20);`) fires the moved struct's Drop body over a ZEROED slot under karac build and leaks the moved heap… | 9ab346d |
| B-2026-08-01-9 | codegen | medium | reassigning a struct/enum binding from a call that mentions it (`e = pass(e)`, `s = mk(s.id + 1)`) orphans the old value's heap under karac build — t… | 6d95ba7 |
| B-2026-08-01-10 | codegen | medium | bare user-enum ctor statement (`Box2.Full(Res { . | fe58e5b |
| B-2026-08-01-11 | codegen | medium | discarded no-own-Drop struct temp with a Drop-bearing heap field (`mk_h();`, `let _ = mk_h();`) fires the field body but leaks the field's heap on bo… | fe58e5b |
| B-2026-08-01-12 | interp | medium | struct destructure of an owned param (`let Holder { r } = h;` in the callee) fires the bound field's Drop body a second time under karac run only | b916307 |
| B-2026-08-01-13 | interp+codegen | medium | owned enum ARG Drop-body ownership incoherent: fresh ctor arg silent when the callee drops it whole, DOUBLE body (both backends) when the callee matc… | 8de98fe |
| B-2026-08-01-14 | codegen | medium | fresh enum-ctor arg to a passthrough callee (`pass2(E2.B(..))`) orphans the ORIGINAL payload buffer — the callee entry-copies and returns the copy | 8de98fe |
| B-2026-08-01-15 | interp+codegen | medium | move-indirected param destructure (`let h2 = h; let Holder { r } = h2;` inside the callee) fires a STALE cap-zeroed body under karac build and a doub… | 309c651 |
| B-2026-08-01-16 | interp+codegen | medium | Assign-based param rebind (`h2 = h;` onto a pre-declared local) double-fires the Drop body on both backends and silently skips the displaced value's… | e0fc863 |
| B-2026-08-01-17 | interp+codegen | low | SortedSet[DropT] element Drop bodies drain in INSERTION order under karac build, sorted order under the interpreter (plain Set diverges too) | 96ad40a |
| B-2026-08-01-18 | codegen | medium | Set/SortedSet element with a String field leaks the field buffer at scope exit under karac build (element blob freed, fields never walked) | eaf6c2a |
| B-2026-08-01-19 | interp+codegen | medium | storing an owned param into a local container FIELD (`o.h = h;`) fires the caller-retained value's Drop body TWICE — container walk + caller NLL, in… | 99254ea |
| B-2026-08-01-20 | interp+codegen | low | displaced-value Drop bodies at FIELD-assign targets are silent on both backends (`o.h = <new>;` frees the old field's heap without running its bodies) | 622946f |
| B-2026-08-01-21 | codegen | medium | index-assign over a struct element with heap fields (`v[i] = Res { . | 6aa0669 |
| B-2026-08-01-22 | interp+codegen | medium | struct-FIELD Vec[DropT] elements: Drop bodies never fire (owner death AND index-assign displacement, both backends); the displaced element's field bu… | bc4315f |
| B-2026-08-01-23 | interp+codegen | low | container-in-container element Drop bodies are silent on both backends (Vec[Vec[DropT]] inner elements, Map[K, Vec[DropT]] value-Vec elements) — memo… | 20ba0e72 |
| B-2026-08-01-24 | codegen | high | Moving elements out of a BY-VALUE `Vec[S]` parameter double-frees each element's heap field: `for h in headers { out.push(h); }` frees the same Strin… | 7225d0a |
| B-2026-08-01-25 | parser | medium | `&&` / `\|\|` diagnostics are emitted 2-3x per occurrence and carry NO `replacement`, so `karac fix` cannot apply a one-token substitution the message… | 28438e8 |
| B-2026-08-01-26 | typecheck+codegen | medium | Closure that move-captures an outer Vec (`let mut v = outer` in the body) and is called TWICE: both `karac run` (JIT) and `karac build` (AOT) print g… | 8ab3b9a |
| B-2026-08-01-27 | ownership+codegen | medium | Closure body that MOVES a captured Vec (`let mut v = outer; v.push(x); v.len()`) called twice: interpreter prints 3 (env alias — mutations accumulate… | b876cd3 |
| B-2026-08-01-28 | codegen | high | The B-2026-08-01-24 for-loop element double-free through the remaining consume arms: `m.insert(k, h)`, `set.insert(p)`, and `names.push(h.name)` over… | 31cd03e |
| B-2026-08-01-29 | codegen | low | Duplicate-key inserts of a for-loop STRUCT element orphan the staged deep copy: `set.insert(p)` with an equal element already present, and `m.insert(… | 09c2721 |
| B-2026-08-01-30 | codegen+interp | medium | Displacement residuals probed live (the b173/b174 recorded residuals): a DEEP-chain field assign `o.h.r = Res{..}` fires no displaced Drop body on EI… | e23c570 |
| B-2026-08-01-31 | codegen | high | Deep-chain field move-out (`let x = o.h.r` / `let s = o.h.name`) never cap-zeroes the source: the moved-out binding AND the root's StructDrop both fr… | e23c570 |
| B-2026-08-01-32 | codegen | low | Vec.filled's calloc fast path only recognises a SCALAR constant zero, so Vec.filled(n, Vec.new()) store-loops an all-zero aggregate instead of calloc… | 0872d65 |
| B-2026-08-01-33 | ownership+autopar | high | `shared struct` was excluded from parallelism on BOTH surfaces — explicit `par {}` hard-errored (E_CONCURRENT_SHARED_STRUCT) and auto-par declined vi… | e061ab29 |
| B-2026-08-01-34 | codegen | medium | Generic-monomorph field moved out of a deep chain (`let g = o.h.b` where `b: Boxy[String]`) still double-frees — the B-2026-08-01-31 suppressor decli… | f82c0db |
| B-2026-08-01-35 | codegen | high | Field store through a FIELD-ROOTED indexed container (`o.hs[i].field = x` where `hs: Vec[P]` is itself a struct field) is SILENTLY DROPPED under kara… | 38d2d23 |
| B-2026-08-01-36 | parser+resolver | medium | A module-level `let` whose name starts with a LOWERCASE letter is not recognized as a module binding at all — it falls through to a top-level stateme… | 0584af0 |
| B-2026-08-02-1 | other | medium | The Mend scorer counts a CORRECT `karac fix` as having "broken the build" whenever it unmasks errors a earlier-phase failure was hiding | dd65d4f |
| B-2026-08-02-4 | resolver+effect | low | FFI lint suggests `allocates(Heap)` but neither the lint nor E0100 mentions the required `effect resource Heap;` declaration — following the compiler… | 8930667 |
| B-2026-08-02-2 | typecheck | low | No implicit `*mut T` -> `*const T` weakening at call sites — every consume-direction binding that passes a malloc'd (or otherwise mut) buffer to a `*… | a31d7be |
| B-2026-08-02-3 | interp | low | Interpreter refuses raw-pointer FFI (`CString.as_ptr` under `karac run --interp`) via a raw Rust panic! + backtrace instead of a structured diagnosti… | 03c8d6a |
| B-2026-08-02-5 | typecheck+interp+codegen | medium | Tuple-element assignment targets are accepted by every checking phase but unimplemented on both backends: `t.0 = v` and `o.t.0 = v` ICE the interpret… | 762a7f8 |
| B-2026-08-02-6 | resolver+interp | high | A BUILTIN function name in ANY non-call position — `spawn;`, `let f = println;`, `spawn { … }` — passes `karac check` and then panics the interpreter… | e6bf8b5 |
| B-2026-08-02-7 | codegen+runtime | high | Declaring a user struct named `Response` WITH A HEAP FIELD makes every `Client.get` double-free the response body: codegen applies the USER type's dr… | 5dd0264c |
| B-2026-08-02-8 | codegen | medium | Tuple-element COMPOUND assignment (`t.0 += 5`) is silently dropped under karac build — prints the stale value with no diagnostic — while the interpre… | a92ecae |
| B-2026-08-02-9 | typecheck | medium | Pointee-changing and const->mut raw pointer casts compile OUTSIDE unsafe blocks — the spec-mandated unsafe gate on `*T as *U` is not enforced (`let w… | a31d7be |
| B-2026-08-02-10 | typecheck+codegen | medium | Methods on tuple-element receivers (`t.0.push(x)`, `t.0.len()`) loud-bail under karac build while the interpreter runs them — the last tuple-place ga… | 7b1122c |
| B-2026-08-02-11 | typecheck | low | Tuple literals do not thread the EXPECTED element types into their elements: `let t: (Vec[i64], i64) = (Vec.new(), 3)` and `Sw { t: (Vec.new(), 3) }`… | 723336d |
| B-2026-08-02-12 | codegen | high | Vec.filled(n, Map.new()) segfaults under AOT (Map handle elements not cloned per slot) | dce2015 |
| B-2026-08-02-13 | typecheck+codegen | high | STDLIB AND USER TYPES SHARE ONE FLAT NAMESPACE, so any user struct that shadows a prelude type name silently takes over that type's codegen identity | — |
| B-2026-08-02-14 | codegen+interp | medium | Drop-carrying field of a GENERIC-mono parent struct: body silent at owner death (both backends) + Vec-element String buffer leaks under AOT | 6ff8614 |
| B-2026-08-02-15 | codegen+interp | high | indexed field store with a NON-PURE index (`v[f()].field = x`): codegen silently dropped the store AND the index's side effect; the interpreter appli… | d52e9ff7 |
| B-2026-08-02-16 | codegen | medium | Vec[T]-of-Drop field of a GENERIC parent: element Drop bodies fire under karac run but stay silent under AOT | 8f3b456 |
| B-2026-08-02-17 | codegen | medium | Map[K, T] field of a GENERIC parent: the value's heap is never freed at owner death under AOT | db367df |
| B-2026-08-02-18 | codegen+interp | low | user Drop bodies silent for TUPLE-held and Map-VALUE-held Drop values in struct fields, on BOTH backends (parity holds, memory clean) | d6e9beb |
| B-2026-08-02-19 | codegen | medium | generic parent's tuple field never frees the mono element heap: `(T, i64)` classifies no-heap at the declared TE | d6e9beb |
| B-2026-08-02-20 | codegen+interp | medium | container moved into a struct-literal FIELD keeps the source binding's element/value bodies walk armed -- interp-only early fires (Set/SortedMap) and… | 61e68c5 |
| B-2026-08-02-21 | codegen | medium | struct-field Set[DropStruct] leaks element heap; struct-field SortedMap[K, DropStruct] leaks the WHOLE handle tree at owner death | 7cf6db5 |
| B-2026-08-02-22 | codegen+interp | high | Vec[(T, ...)] with a heap/Drop-bearing TUPLE element: no element memory drop and no element bodies walk — heap leaks and the Drop body never fires (b… | 98b3898 |
| B-2026-08-02-23 | codegen+interp | medium | aggregate-literal source disarm has depth-1 / same-frame reach: a NESTED literal or a CALLEE-built literal fires the moved source's element bodies tw… | LEG 2 FIXED (b208): the callee-built returned literal turne… |
| B-2026-08-02-24 | interp | medium | interpreter misses a Map VALUE struct's Vec-field element bodies at owner death; AOT fires them | 0a422d8 |
| B-2026-08-02-25 | codegen | high | MATCH-ARM LEG ONLY (displacement leg fixed in 21a1fb6): consuming an Option[Drop] payload via a match/if-let arm binding on a NAMED binding with a bo… | 542d7d7 |
| B-2026-08-02-26 | codegen | high | a TUPLE binding whose element is a container of Drop values (`let t = (xs, 9)`, xs: Vec[Res]) runs no Drop body and leaks the elements under AOT whil… | Three edits, one per gap |
| B-2026-08-02-27 | codegen+interp | medium | an own-Drop source moved into a let-RHS TUPLE keeps its own body armed: fires twice on both backends, and under AOT the first fire reads the moved-fr… | Codegen: the TUPLE arm of `disarm_container_bodies_move_sou… |
| B-2026-08-02-28 | codegen+interp | medium | a call result consumed directly as another call's argument (`use_it(mk(xs))`) leaks the inner temp's heap under AOT and fires its Drop body at a diff… | LEAK LEG FIXED (b210) |
| B-2026-08-03-1 | codegen+interp | high | an Option/Result payload never runs its user Drop body in ANY nested position — struct field, Vec element, Map value, tuple element — on BOTH backend… | Root cause confirmed as predicted -- a wiring gap, not miss… |
| B-2026-08-03-2 | codegen+interp | high | a container element destroyed WITHOUT being bound never runs its Drop body — clear, truncate, discarded remove/swap_remove/pop, and whole-container r… | CLASS 3 FIXED (b213), row stays OPEN for classes 1 and 2 |
| B-2026-08-03-3 | codegen+interp | high | a tuple-held Option[Struct] / Result[Struct, E] payload is never freed in ANY position, and a `let x = t.N` move-out fires the source element's Drop… | c775e70 |
| B-2026-08-03-4 | codegen | medium | mis-shaped handler `Response` panics the HTTP serve shim instead of diagnosing | 7068f8c |
| B-2026-08-03-5 | codegen | high | Displacing an Option[T] binding emits a SPURIOUS extra user Drop body over a stale slot, printing a garbage field value — pre-existing, independent o… | 284c432 |
| B-2026-08-03-6 | interp | medium | `match t.N { Ok(v) => . | src/interpreter/pattern_match.rs (the TupleIndex arms of `d… |
| B-2026-08-03-7 | codegen+interp | medium | a struct field holding a tuple, and a Map value holding a tuple, run NO Drop body for the tuple's elements on EITHER backend | 08c12068 |
| B-2026-08-03-8 | codegen+interp | medium | `let x = h.f` moving an Option / Result / Vec FIELD out of a struct never disarms the field's Drop machinery — SEGV for an Option[Struct] field, a do… | 8407085 |
| B-2026-08-03-9 | typecheck | medium | the canonical `Map[K, Vec[V]]` grouping idiom taught by the corpus is QUADRATIC, but the language already has the O(k) answer -- `Map.entry(k).or_ins… | d166203 |
| B-2026-08-03-10 | codegen | medium | an Option field whose payload is INLINE (a struct narrow enough to fit the 3-word payload area) fires its Drop body THREE times under AOT when the ow… | 257d666 |
| B-2026-08-03-11 | codegen | medium | a struct field holding a MIXED `Result[<Drop struct>, String]` -- one half a direct String/Vec, the other a struct/enum -- is admitted by NEITHER Res… | 0567cea |
| B-2026-08-03-12 | codegen | low | `coroutine_preserves_active_span_across_suspend` (tests/coro_e2e.rs) is INTERMITTENT -- the post-resume log line came back unstamped (`[info] after-r… | 7f9e8656 |
| B-2026-08-04-1 | codegen | high | FRESH-TEMP twin of B-2026-08-02-25's match-arm leg: a heap-BOXED Option/Result payload bound out of a `match mk() { Some(r) => . | c89192f |
| B-2026-08-04-2 | codegen | high | A heap-BOXED Option payload bound by a consuming match arm and then MOVED -- into a struct literal, or out as the match's tail value -- double-frees… | 8f8696d |
| B-2026-08-04-3 | codegen | medium | A FRESH-TEMP boxed Option/Result scrutinee matched by a WILDCARD payload arm (`match mk() { Some(_) => . | bad9a7a |
| B-2026-08-04-4 | interp | medium | INTERPRETER: a Drop-body move record is keyed by BINDING NAME and outlives its block, so a later same-named binding that was never moved is treated a… | 168c887 |
| B-2026-08-04-5 | codegen | high | ICE: destructuring a heap-BOXED Option/Result payload with a STRUCT sub-pattern (`match o { Some(Full { name, buf }) => . | 9908f6a |
| B-2026-08-04-6 | codegen | medium | A FRESH-TEMP boxed Option/Result scrutinee destructured by a PARTIAL struct sub-pattern (`match mk() { Some(Full { name, buf: _ }) => . | 7ecdec0 |
| B-2026-08-04-7 | cli | high | EVERY project-mode `karac build` fails immediately: it opens the PACKAGE NAME as a source path (`error: cannot read 'solo'`), so a manifest-driven bu… | 4c52d82 |
| B-2026-08-04-8 | codegen | medium | Bounds-check elimination fails for the CONVERGING two-pointer loop `while lo <= hi { v[base+lo] .. | e94e6bd9 |
| B-2026-08-04-9 | codegen | high | `?` on an Option/Result whose payload is heap-BOXED unwraps the BOX POINTER as if it were the payload's first word -- the value comes out empty or ga… | 60087fc |
| B-2026-08-04-10 | codegen | medium | `let <StructPattern> = <expr>?;` -- a struct destructure written DIRECTLY on a `?` whose payload is heap-boxed -- leaks the payload's heap fields; ro… | a82b4ff |
| B-2026-08-04-11 | codegen | high | `match <fresh Result temp> { Err(e) => . | 563eb8b |
| B-2026-08-04-12 | codegen | high | `?` PROPAGATING an Err whose payload is a struct wider than THREE words silently drops every word past the third -- the Err mirror of B-2026-08-04-9'… | e6a2eca |
| B-2026-08-04-13 | codegen | high | The descending-loop bounds-check skip (B-2026-07-17-1) reads the facts it rests on with `stmt_writes_ident`, which sees only TOP-LEVEL assignment tar… | 484c2c9 |
| B-2026-08-04-14 | interp | medium | The interpreter silently DROPPED an out-of-range index-assign: `v[100] = 7` on a 2-element Vec produced no error, no growth, and no store — while AOT… | a4ca760 |
| B-2026-08-04-15 | autopar+codegen | high | AUTO-PAR SILENTLY DROPS STORES through a tuple-element receiver: `t.0.push(x)` recorded NO write in the dependency walk, so two pushes to the same Ve… | 567d5aa |
| B-2026-08-04-16 | codegen+ownership | high | Moving a Vec OUT of a tuple element, mutating it, and moving it BACK (`let mut e = t.0; e.push(x); t.0 = e;`) aborts with `free(): double free detect… | e986284 |
| B-2026-08-04-17 | other | medium | memory-fixture authoring hazard: a payload whose content is compile-time constant, or that is read only through `.len()`, is a DEAD allocation LLVM d… | 49e126a |
| B-2026-08-04-18 | ownership | low | Moving a heap value OUT of an aggregate element and then assigning it BACK (`let mut e = t.0; e.push(x); t.0 = e;`) warns `value 't' moved here, used… | c312c8a2 |
| B-2026-08-04-19 | codegen | high | Double-free (masked at -O2, hard at -O0/JIT): an owned struct/enum binding moved by an ASSIGNMENT — `o.h = h` into a heap-owning user-struct field, o… | 06bf3145 |
| B-2026-08-04-20 | codegen | medium | The `KARAC_TEST_JIT=1` codegen parity leg did not set KARAC_PROGRAM_ARGS, so `env.args().len()` returned 2 (the `karac_jit_runner` argv `[runner, <ir… | 39b0c294 |
| B-2026-08-04-21 | cli | medium | `karac fmt` silently deleted every declaration modifier it had no printer for (`unsafe fn`, `comptime fn`, `comptime` param prefix) | c35df3a |
| B-2026-08-05-1 | codegen | high | Passing a TUPLE ELEMENT to a `ref` parameter (`peek(t.0)` where `fn peek(v: ref Vec[i64])`) double-frees the element buffer under AOT; the struct-fie… | 99d27f7 |
| B-2026-08-05-2 | codegen | high | A `mut ref` parameter given a TUPLE ELEMENT (`bump(mut t.0)`) does not mutate the element under AOT -- the program then PANICS reading the index the… | 99d27f7 |
| B-2026-08-05-3 | codegen | medium | `Option[(Vec[T], ...)]` leaks the tuple payload's heap element when the Some arm binds and reads it -- 32 bytes definitely lost; the struct payload t… | f0aadd9 |
| B-2026-08-05-4 | runtime | high | PERF-REGRESSION introduced by B-2026-07-31-21's fix (75a3a928): a remove-heavy Map runs 1.76x slower because the same-width compacting rehash re-fire… | 73237002 |
| B-2026-08-05-5 | codegen+runtime | medium | ARM64 perf regression ATTRIBUTED to 58412d9f (7-bit hash tag in the map bucket control byte); FIX SHAPE REOPENED, was wrongly recorded as settled | c4e6d76e |
| B-2026-08-05-14 | codegen | medium | tests/selfhost_codegen.rs (selfhost_codegen_matches_seed_run) is RED on macOS/arm64 for EVERY corpus entry: the self-hosted emitter hardcodes a Linux… | 06a3f683 |
| B-2026-08-05-6 | codegen | medium | Bounds checks survive in a CALLEE that walks a caller-owned buffer at a caller-chosen offset — `fn f(v: ref Vec[u8], base: i64, len: i64) { while lo… | c87f488 |
| B-2026-08-05-7 | codegen | high | ~23 heap-ownership shapes emit a DOUBLE FREE; the `ok_or` String Err payload case is CONFIRMED to abort on a DEFAULT -O2 `karac build` as soon as the… | ef9c1b1 |
| B-2026-08-05-8 | codegen | medium | `s.contains(other)` on a String bound out of a `Result[String, E]` Ok arm fails to COMPILE -- "Binary op Eq: right operand has non-comparable type {… | b7a0a1d |
| B-2026-08-05-9 | codegen | high | `unwrap_or` with a FRESH F-STRING default leaks on a DEFAULT -O2 build -- 133 leaked allocations in an existing fixture -- while the byte-identical p… | c5dcb1f7 |
| B-2026-08-05-10 | codegen | high | A `ref`-borrowed `shared` handle captured into a `par` branch reads as ZERO under codegen — silent wrong answer, interpreter disagrees | 93b1a81 |
| B-2026-08-05-11 | interp | medium | `File.read` / `BufReader.read` reject a fixed `Array[u8, N]` buffer that AOT accepts — the blessed `let mut buf: Array[u8, N]; f.read(mut buf)` idiom… | 1caca04 |
| B-2026-08-05-12 | parser | low | the `ref` at a call site diagnostic tells the author to remove one token but carries no machine-applicable replacement, so `karac fix` leaves it | 9b17779 |
| B-2026-08-05-13 | autopar | medium | `karac query concurrency` reports `fanned_out: true` for a disjoint-write loop that runs SINGLE-THREADED when the accumulator is a `mut ref` parameter | 286afea |
| B-2026-08-05-15 | codegen | medium | taking a free function as a VALUE (`let f = g;`) and calling it through the binding fails to build when any parameter is a `ref Vec[T]` -- the indire… | 72f9f49 |
| B-2026-08-05-16 | codegen | medium | NONDETERMINISTIC SEGV at -O0: a bare variant-name pattern whose name is shared by two enums resolves against the UNORDERED enum_layouts map, so per-p… | e3a086e6 |
| B-2026-08-05-17 | cli | medium | `karac build` does not enforce EFFECT errors that `karac check` reports — a program `check` rejects with 1 error builds and runs; type errors ARE enf… | 0bfde1c |
| B-2026-08-05-18 | typecheck+effect | medium | a RESOURCE-LESS effect verb (`panics` / `blocks` / `suspends`) is silently dropped wherever an effect LIST is converted to an effect SET — so a `Fn(.… | 69d630b |
| B-2026-08-05-19 | typecheck+codegen | high | generic args are NOT invariant across numeric element types: `Vec[i64]` is silently accepted where `Vec[u16]` is declared, and AOT then reinterprets… | 80d7a37 |
| B-2026-08-05-20 | codegen | high | A whole-value binding-to-binding move of a BOXED-payload `Option` (`let b2 = body;`) double-frees at -O0 -- deterministic, no destructure involved, a… | e7023333 |
| B-2026-08-05-21 | codegen | medium | The INTEGER-OVERFLOW check on an index add `v[base + i]` is still emitted after BCE has PROVEN `0 <= base + i < v.len()` -- a fact that already entai… | 72f9fd7d |
| B-2026-08-05-22 | codegen | high | A fresh-temp aggregate ARGUMENT whose heap lives only behind `Option` fields registered no caller-side cleanup and leaked one payload per call -- 749… | this commit |
| B-2026-08-05-23 | other | medium | the JIT/selfhost oracles report a module that NEVER RAN as an output mismatch: run_ir discarded karac_jit_runner's stderr, so an unresolved external… | 9e25bfaa |
| B-2026-08-05-24 | cli | medium | `main` is RED: tests/cli.rs::wasm_browser_rich_exports_marshal_e2e fails a typecheck since 80d7a37c (B-2026-08-05-19, generic args invariant across n… | 8d6d1b92 |
| B-2026-08-05-25 | typecheck | low | A constant integer EXPRESSION payload does not adopt its expected type in an enum constructor: `Result.Err(0 - 1)` into `Result[i32, i32]` is rejecte… | 895ea26 |
| B-2026-08-05-26 | typecheck | medium | tensor arithmetic infers an f64 element for an f32 operand pair: `let p: Tensor[f32, [D]] = a * k` with `a: ref Tensor[f32, [D]]` and `k: f32` types… | a64e931 |
| B-2026-08-05-27 | codegen | high | The surface-concat RECEIVER gap is only closed for the len-family: `("p:".to_string() + s).starts_with(..)` still leaks the concat on a DEFAULT -O2 b… | this commit |
| B-2026-08-05-28 | codegen | medium | a String-to-String xform does not compile when its RESULT is itself a method RECEIVER — `("p:".to_string() + s).to_uppercase().len()` fails with "no… | 7f067c28 |
| B-2026-08-05-29 | typecheck+cli | medium | a single-target `karac check` silently omits `#[target(T)]`-gated bodies: they are stripped before any pass, so `check` prints "All checks passed" an… | e865f233 |
| B-2026-08-05-30 | other | medium | the wasm E2E tests skip on a SUCCESSFUL build: `wasm_build_skip_reason` matches the string `wasm-tools not found`, which the browser-bindings path em… | c5005e9 |
| B-2026-08-05-31 | interp+codegen | medium | the interpreter computes `Tensor[f32]` elements in f64 while AOT uses a packed f32 buffer, so an f32 tensor gives DIFFERENT ANSWERS on the two backen… | 2bfece1 |
| B-2026-08-05-32 | codegen | high | A struct with a DIRECT `shared` field, bound to a LOCAL and passed BY VALUE, never rc-decs the box -- it leaks on a DEFAULT -O2 build (288 B / 8 allo… | 17b58f4 |
| B-2026-08-05-33 | codegen | high | LAW, not one fixture: a by-value aggregate param that is CALLER-RETAINS is owned by nobody and leaks once per call on a DEFAULT -O2 build | 13a6f9ed |
| B-2026-08-05-34 | codegen | medium | PERF-REGRESSION, RESOLVED BY MEASUREMENT AND LARGELY NOT A DEFECT: the corpus figure is real but is dominated by e4047440 (AOT integer overflow/div-z… | e4047440 |
| B-2026-08-05-35 | other | medium | the ASAN harness SKIPS on a CODEGEN failure ('setup failed -- skipping'), so a memory_sanitizer test written for a shape that does not yet compile re… | 1557d40 |
| B-2026-08-05-37 | codegen+interp | high | a `mut ref` PARAMETER given a PLACE argument silently DISCARDS the callee's write on every backend — `bump(mut g.val)` / `bump(mut t.0)` / `bump(mut… | 1bf6175 |
| B-2026-08-05-39 | codegen | medium | a `mut ref` AGGREGATE parameter's whole-value REASSIGNMENT stored past its slot — `x = mk()` on a `mut ref String` wrote 24 bytes into the 8-byte all… | 559a8cc |
| B-2026-08-05-40 | codegen | medium | a `Slice[T]` / `mut Slice[T]` parameter fed from a PLACE (`f(g.a)`, `f(g.q.a)`, `f(t.0)`, `f(vv[0])`) did not COMPILE — the Vec's 3-word `{ptr,len,ca… | 0949f9f |
| B-2026-08-05-41 | typecheck+codegen | medium | a `shared struct` field reached through a `mut ref` ARGUMENT bypasses the immutable-field write gate that rejects the assignment spelling, and the wr… | de799c3 |
| B-2026-08-06-1 | codegen | medium | a generic wrapper's bare `T` field bound to a MAP leaks its whole handle tree: `fn sink(b: Box[Map[i64, String]])` loses 25,830 B / 40 blocks on a DE… | 5c43517 |
| B-2026-08-06-2 | codegen | high | TWO defects on the by-value generic-struct param path, and THIS ROW'S OWN 'clean' CONTROL WAS THE WORSE ONE: (A) the CONCRETE spelling `fn take(b: Bo… | 933b859 |
| B-2026-08-06-4 | typecheck+codegen | medium | a `shared struct`'s Vec field passed to a `mut Slice[T]` parameter does not COMPILE (LLVM module verification hard-fails) even when the field is decl… | 28ceec6b |
| B-2026-08-06-5 | codegen | high | A cast TO `char` inside an f-string hole is DROPPED by both compiled backends -- `println(f"{b as char}")` prints the integer codepoint (98) where th… | 335540c9 |
| B-2026-08-06-6 | codegen | high | a `Map`/`Set` field moved out INDIVIDUALLY left the source handle live, so the owner's struct drop freed storage the destination still owned — `fn ta… | 26b2176 |
| B-2026-08-06-7 | codegen+interp | high | shift by >= the bit width is UNDEFINED BEHAVIOUR in AOT output — one `let` variable prints two different values in the same run and different values… | ee551b8 |
| B-2026-08-06-8 | codegen | low | a generic wrapper's bare `T` field bound to a SHARED struct leaks 2,560 B / 80 blocks at -O0 (clean at -O2): `Box[T]` at `T = Node` where `Node` is a… | 0928227 |
| B-2026-08-06-9 | codegen | medium | TWO shapes where a heap-BOXED enum payload loses its owner AT A CALL BOUNDARY: (A) [FIXED] a NAMED `Option` binding passed by value has its let-site… | 9370f723 |
| B-2026-08-06-10 | codegen | medium | a callee arm that MOVES A FIELD OUT of a boxed `Option[Struct]` param orphans the box the caller's struct drop owns: `fn f(h: Option[H]) { match h {… | e6a0a5a1 |
| B-2026-08-06-11 | codegen | high | an owned boxed-payload enum param that ESCAPES -- returned, or forwarded to another by-value param -- is freed by the callee anyway: `fn id(o: Opt[St… | 05005aae |
| B-2026-08-06-12 | codegen | high | a GENERIC struct LITERAL used directly as a METHOD RECEIVER cannot be built: `Box { v: <String> }.take()` passes `karac check`, runs correctly under… | 2a985b08 |
| B-2026-08-06-13 | lexer+parser | low | `i64::MIN` cannot be WRITTEN as a literal in any spelling -- `-9223372036854775808i64` is a parse error (`Invalid integer literal`), because a negati… | <this commit> |
| B-2026-08-06-14 | codegen | high | a `shared` field RETURNED out of a BY-VALUE struct param is a use-after-free on a DEFAULT -O2 build: `fn giveback(b: Holder) -> Node { return b.v; }`… | 257630a |
| B-2026-08-06-15 | codegen | medium | a `shared` handle escaping a VALUE-POSITION BLOCK is never rc-dec'd by its consumer binding: `let x = { let b = Box { . | e954652 |
| B-2026-08-06-16 | typecheck | low | the upper half of u64 is unwritable as a literal -- `18446744073709551615u64` (and any magnitude above i64::MAX, in any radix) is a parse error, beca… | <this commit> |
| B-2026-08-06-17 | typecheck+codegen | medium | `ref CStr` as an `unsafe extern "C"` PARAMETER type is accepted by the typechecker and then dies at codegen with a raw LLVM module-verification error… | adaa9e34 |
| B-2026-08-06-18 | codegen | high | a u64 ARITHMETIC RESULT above i64::MAX renders SIGNED under both compiled backends but unsigned under the interpreter -- `println(u64.MAX - 1u64)` pr… | ac3041ce |
| B-2026-08-06-19 | codegen | medium | a chained FIELD ACCESS on a generic method's RETURN cannot be built: `w.take().f` fails `karac build` with "cannot resolve field 'f' on this receiver… | FIXED (src/codegen/expr_ops.rs: new `generic_method_return_… |
| B-2026-08-06-20 | codegen | medium | two instantiations of ONE generic struct reached through a MIX of literal and named receivers collide on the unmangled `@Type.method` symbol, whose s… | 2fbf970d |
| B-2026-08-06-21 | codegen | high | a boxed `Option`/`Result` binding passed by value to a PASSTHROUGH callee is freed TWICE at -O0: `fn id(o: Option[Option[i64]]) -> Option[Option[i64]… | fe7fea77 |
| B-2026-08-06-22 | codegen | medium | a DOUBLY-nested generic chain `outer.take().take().f` at `Box[Box[Wide]]` cannot be built: type resolution SUCCEEDS (the field index resolves to Some… | cfb149aa |
| B-2026-08-06-23 | codegen | medium | a generic struct LITERAL in RECEIVER position whose field initializer type cannot be NAMED lowers at the ERASED `{i64}` layout: `Box { v: f * 2.0 }.t… | FIXED (src/codegen/call_dispatch.rs: new `scalar_type_name_… |
| B-2026-08-06-24 | interp | medium | EVERY `extern` call under `--interp` reports an "internal .. | 3607c241 |
| B-2026-08-06-25 | codegen | medium | the generic-impl MONOMORPH NAME mangles only the type argument's HEAD, so `Box[Box[Wide]]` and `Box[Box[Box[Wide]]]` (and `Box[Box[i64]]`, `Box[Box[S… | FIXED (src/codegen/mono.rs: new `append_nested_instantiatio… |
| B-2026-08-06-26 | codegen | medium | a boxed `Result` passed through a passthrough callee and matched with a payload-BINDING arm still double-frees at -O0: `Result[Wide, i64]` where `Wid… | d1aa4477 |
| B-2026-08-06-27 | codegen | high | an INLINE heap `Option[String]` payload passed by value to a passthrough callee is freed TWICE at the DEFAULT -O2 as well as -O0, when the payload is… | edee55d9 |
| B-2026-08-06-28 | codegen | high | a DISCARDED passthrough call result double-frees its argument's payload at the DEFAULT -O2: `let d: Option[String] = Some(mk()); idopt(d);` aborts wi… | FIXED (src/codegen/runtime.rs: new `discarded_temp_aliases_… |
| B-2026-08-06-29 | codegen | high | a BOUND-then-CONSUMED passthrough result double-frees at the DEFAULT -O2: `let x = idopt(d); peek(x);` aborts with `free(): double free detected in t… | FIXED (src/codegen/control_flow_match.rs: new `moved_arg_ow… |
| B-2026-08-06-30 | codegen | medium | a SELF-RECURSIVE reduction was scored at ~15,000,000 units per iteration (64^3, the depth cap unrolling the same body and compounding the nested-loop… | a23262c |
| B-2026-08-06-31 | codegen | medium | the STRUCT-payload sibling of B-2026-08-06-9 leg A: a boxed struct `Option` payload lost its owner across a by-value call, in both the FRESH-TEMP for… | efc9b308 |
| B-2026-08-06-32 | codegen | medium | a heap-BOXED `Option` payload nested inside a `Result`'s INLINE payload area has no owner on any side -- 32 B per construction at -O0 | c6eaeea |
| B-2026-08-06-33 | codegen | medium | SETTLED, lane A's DIRECTION confirmed: on x86_64 the map hash-tag compare PAYS for primitive keys, so B-2026-08-05-5's `!self.target_is_aarch64` cond… | e7a5ebc |
| B-2026-08-06-34 | other | medium | MAIN IS NOT RED and the four cells are NOT fixed: the ownership-matrix ratchet reported four `Leak`->`Clean` flips because LeakSanitizer was INERT (t… | 84529948 |
| B-2026-08-07-1 | codegen | high | a BOXED-payload enum binding that is RETURNED is freed by its own frame -- the caller reads and frees the same box: double free + use-after-free at B… | ae74c7f |
| B-2026-08-07-2 | codegen | medium | the remaining owner sites for a box nested in a `Result`'s INLINE payload area -- ALL SIX RESOLVED | b55a849 |
| B-2026-08-07-7 | codegen | medium | a struct field of type `Option[Option[String]]` DOUBLE-FREED the String at BOTH opt levels when a match arm bound it out -- FIXED (c25c949) by disarm… | c25c949 |
| B-2026-08-07-9 | codegen | high | a match ARM that binds a struct field's whole boxed payload out and destructures it in a SECOND match double-frees the interior at both opt levels; t… | ba64b21 |
| B-2026-08-07-6 | codegen | low | the DIRECT sibling of the nested-box chain: `Option[Option[Option[i64]]]` with no `Result` wrapper leaks its inner envelope -- `BoxedEnumDrop` frees… | 28e17d9d |
| B-2026-08-07-4 | codegen | medium | reassigning a `let mut` binding whose enum payload is heap-BOXED leaks the OVERWRITTEN value's box -- the store site frees nothing; 32 B per overwrit… | da29013 |
| B-2026-08-07-3 | codegen | high | `<Result/Option binding>.map(f)` whose ABSENT branch (`Err`/`None`) carries a heap payload double-frees when the map result is CONSUMED or DISCARDED:… | 94ead2dc |
| B-2026-08-07-5 | codegen | medium | `b = c` between two boxed-payload enum bindings leaves BOTH slots holding one box and both armed -- glibc double free at -O0 | a635ffe |
| B-2026-08-07-8 | typecheck | low | `self` cannot be passed to a BORROW parameter from any receiver mode — `byref(self)` from `ref self` is `expected 'ref Inner', found 'Inner'`, and so… | aa3b394 |
| B-2026-08-07-10 | codegen | medium | PLACEMENT SENSITIVITY CONFIRMED AND PRICED ON arm64, BUT THIS ROW'S OWN 1.09x DOES NOT REPRODUCE: vanilla builds of b84477dd and 36a7fa5a are 1.0000… | c538a878 |
| B-2026-08-07-11 | codegen | medium | the envelope chain is owned only at the LET site: passing a boxed-chain enum by value to an owned param leaks 320 B/10, and moving it into a struct l… | d5cfa3c8 |
| B-2026-08-07-12 | codegen | medium | a fresh-temp struct literal passed BY VALUE leaked the heap inside an `Option`/`Result` field at BOTH opt levels, and the callee's entry copy of a BO… | 00660c3 |
| B-2026-08-07-13 | other | low | docs/bug-ledger.jsonl has no pinned JSON encoding, so writers flip it between raw UTF-8 and \uXXXX escapes and each flip rewrites all ~1000 rows | 9636a9b6 |
| B-2026-08-07-14 | codegen | medium | RESOLVED, NOT A DEFECT: the i64 OVERFLOW CHECK's real cost is LOST AUTO-VECTORIZATION -- the trap branch is a second loop exit, so kara drops from AV… | No karac change, and none is available -- this row closes b… |
| B-2026-08-07-15 | codegen | high | a struct that is NOT copy-supported (any `Map`/`Set` field) passed as a FRESH TEMP by value is dropped by BOTH frames -- 480 valgrind errors, 175 inv… | db16f10 |
| B-2026-08-07-16 | codegen | low | the x86 hash-tag probe spills 2 of the caller's registers per key-probe (4 memory ops) because `ctrl` puts the loop one value over x86-64's 15 GPRs —… | 1d0fe05d |
| B-2026-08-07-17 | codegen | high | B-2026-08-07-15's own-by-transfer gate exempted EVERY generic struct, and a generic struct at a CONCRETE param (`fn take(x: Mix[String])` over `Mix[T… | 0cf32de |
| B-2026-08-07-18 | codegen | high | naming a generic fn's type param differently from the struct's silently miscompiles: `fn f[U](x: Mix[U])` is 30 valgrind errors / 10 invalid frees /… | 592216a0 |
| B-2026-08-07-19 | codegen | medium | the `Result` twin of B-2026-08-07-12 leg 1: a `Result[Map[K,V], E]` STRUCT FIELD leaks its whole handle tree, 720 B / 10 iterations on a DEFAULT -O2… | 31755b38 |
| B-2026-08-07-20 | codegen | medium | a SHARED-owning struct never frees its `Option[Map]`/`Option[Set]` field -- 720 B / 10 iterations at BOTH opt levels for a plain `let`, no call in th… | 1ee40f33 |
| B-2026-08-07-21 | codegen | low | CLOSED AS PRICED, PREVALENCE ZERO: elementwise checked arithmetic does lose auto-vectorization (4.22x measured in kara) and, unlike the reduction cas… | No code change — closed as priced, on the prevalence number… |
| B-2026-08-07-22 | interp | high | a `par {}` block inside a `while` loop HANGS under `--interp` (the DEFAULT for `karac check`-adjacent workflows and the Mend oracle) while the identi… | b64a8b47 |
| B-2026-08-07-23 | ownership | high | a `frozen` handle has no legal place to be STORED, so no iterative traversal can use one: a local `VecDeque[Node]` worklist refuses `queue.push_back(… | 63caea3e |
| B-2026-08-07-24 | codegen | medium | 64-BYTE BASIC-BLOCK ALIGNMENT is a MEASURED 10.0% on kata:170 -- its aligned placement DISTRIBUTION entirely dominates the unaligned one, worst align… | DECLINED, PRICED, no code change -- and closed rather than… |
| B-2026-08-07-25 | other | medium | A BENCHED KATA'S HEADLINE NUMBER CAN CARRY A 1.31x PLACEMENT RANGE BEHIND IT AND THE CORPUS HAS NEVER BEEN CHECKED FOR IT: kata:170's recorded figure… | f152a64 |
| B-2026-08-07-26 | ownership | high | a frozen-element container declared INSIDE a closure was still admitted -- `try_admit_container_method` had no `in_closure` gate, so an escaping clos… | 6673e162 |
| B-2026-08-07-27 | other | medium | kata #133's par lane compiles and runs after B-2026-08-01-33, but restoring it is the whole bench pipeline -- the .kara's "DOES NOT COMPILE" header,… | 2603167 |
| B-2026-08-08-1 | ownership | medium | the `par` capture gate is keyed on binding NAMES, so two branches that each declare their own local `let n = <shared>` read as ONE binding reachable… | 8c26ad4a |
| B-2026-08-08-2 | typecheck+ownership | high | this row's PREMISE WAS WRONG -- kata #133 was never blocked on `Map[K, frozen V]` (its `visited` map holds the CLONES, which are mutated and can neve… | 502a598f |
| B-2026-08-08-3 | codegen | medium | a `par` branch whose body is a BLOCK EXPRESSION containing a `Map` fails codegen with `Undefined variable '<outer destructured name>'`, while `karac… | da9ae409 |
| B-2026-08-08-4 | ownership+typecheck | high | a CONTAINER-mediated strong cycle in a `shared struct` (`mut ns: Vec[N]`, `mut next: Option[N]`) is accepted and leaks the whole graph -- design.md s… | 68d0b86b |
| B-2026-08-08-13 | codegen | high | a `Vec[weak N]` push SILENTLY CORRUPTED the target's first field — `w.push(a)` changed `a.v` from 41 to 42, because the weak-target scan never saw th… | f869989f |
| B-2026-08-08-14 | interp | medium | the interpreter has no weak CONTAINER element, so a `Vec[weak T]` read-back reports `non-exhaustive match .. | f0c47394 |
| B-2026-08-08-5 | typecheck+codegen | high | the `weak` downgrade store coercion reaches a direct FIELD store but not a container-ELEMENT store, so `Vec[weak N]` cannot be built -- which makes d… | 1bb5328a |
| B-2026-08-08-6 | codegen | medium | a caller-retains struct PARAM whose callee moves a promoted `Option`/`Result` field out has two owners | 9b4abaf0 |
| B-2026-08-08-7 | other | medium | six ASAN fixtures were passing VACUOUSLY — their programs fail typecheck and `karac build` would refuse them, but the harness only ever asserted the… | 7c7de323 |
| B-2026-08-08-8 | typecheck | low | expected-return seeding reaches only PATH callees, and its argument checking only collection literals — a plain generic free fn still rejects a conte… | a2ce6b79 |
| B-2026-08-08-9 | typecheck | high | a generic slot bound by the EXPECTATION skipped the narrowing check — `let x: u8 = id(big)` with `big: i64 = 5000000000` typechecked and printed 5000… | a2ce6b79 |
| B-2026-08-08-10 | codegen | medium | a generic struct with a `Vec[T]` FIELD returned by value from a generic function fails codegen — `ret { { ptr, i64, i64 } } %field` against a `ptr` r… | f773c317 |
| B-2026-08-08-11 | typecheck | low | a type error about a `frozen` parameter names its type `ref T` -- the surface keyword and the diagnostic disagree, because `frozen T` lowers to `Ref(… | 4f32b1e1 |
| B-2026-08-08-15 | autopar+codegen | high | an RC-bearing `shared struct` published as an auto-par return slot is never adopted by the joining scope -- it LEAKS when the branch suppresses its r… | 62619a88 |
| B-2026-08-08-16 | other | medium | the ASAN memory suite compiles every fixture with AUTO-PAR DISABLED, so ~1000 leak/UAF fixtures cover sequential codegen only -- the hole that hid B-… | 74bf4856 |
| B-2026-08-08-23 | autopar+codegen | medium | an auto-par branch containing a `while let` never captured the names its scrutinee reads — `refs_in_expr` had no `WhileLet` arm, so `karac build` ref… | 74bf4856 |
| B-2026-08-08-17 | autopar+codegen | high | a closure's write through a captured `String` is SILENTLY LOST when the analyzer parallelizes the enclosing function -- `karac build` and `karac run`… | 10659bf4 |
| B-2026-08-08-18 | autopar+codegen | medium | a `Column` arithmetic chain passed to a two-arg fn emits a malformed call under auto-par -- LLVM module verification rejects `call i64 @fst(i64 %m8,… | 31208e3a |
| B-2026-08-08-19 | autopar+codegen | medium | a user method on a `shared struct` loses its dispatcher under auto-par -- `codegen: no handler for method 'total' on variable 'b' (method dispatch fe… | ce1b8703 |
| B-2026-08-08-20 | codegen | high | `Vec[String].first()` / `.last()` CONSUMED AS A VALUE double-frees under EVERY codegen backend -- `println(v.first().unwrap())` on a two-element `Vec… | PENDING |
| B-2026-08-08-21 | codegen | low | `Option/Result.map` with an UN-ANNOTATED closure returning a String/Vec is refused by `karac build` -- a loud, actionable bail ("annotate the closure… | 8521c30e |
| B-2026-08-08-22 | codegen | low | a closure whose body IS a String value (bare literal or `+` concat) is declared with a POINTER return, so the `{ptr,len,cap}` it yields fails LLVM ve… | 51ceecab |
| B-2026-08-08-24 | typecheck+codegen | high | an OWNED closure-param annotation over a BORROWED payload (`out.first().map(\|x: String\| ...)`) was silently accepted and miscompiled -- empty String,… | cdd74e13 |
| B-2026-08-08-25 | codegen | medium | matching a payload out of a live `Option[String]` / `Result[String, _]` binding leaves the BINDING DANGLING, so any later read is garbage or aborts t… | d530c033 |
| B-2026-08-08-26 | other | medium | `tests/cli.rs` is the dark target B-2026-07-31-44 missed, and it was RED the whole time -- 35 of its 43 `#[cfg(feature = "llvm")]` tests run in NO CI… | f68004a |
| B-2026-08-08-27 | other | low | the dark-llvm-target audit has never been RE-RUN as a check -- B-2026-07-31-44 swept 19 targets by hand and B-2026-08-08-26 found the 20th the same w… | 5b60bd77 |
| B-2026-08-08-28 | codegen | high | a weak ELEMENT read through a struct FIELD (`a.ns[0]` on `mut ns: Vec[weak N]`) skips the balancing acquire and over-releases -- SIGSEGV under JIT an… | 02f8a0c |
| B-2026-08-08-29 | typecheck+codegen | medium | `Map[K, weak V]` is ACCEPTED and lowers the value as a STRONG ref that nothing releases, so writing `weak` LEAKS where the strong `Map[K, V]` twin is… | dc31715 |
| B-2026-08-08-30 | codegen | high | mapping a BORROWED SCALAR payload — `Vec[i64].first().map(\|x\| x + 1)` — was TWO defects, and the reported panic was the lucky one: the closure's `ref… | e524f62 |
| B-2026-08-09-1 | codegen | medium | a `Map[K, V]` field of a SHARED struct has no general per-value drop-fn channel, so a V that needs a RECURSIVE drop leaks -- the non-shared struct fi… | ec446e0 |
| B-2026-08-09-2 | typecheck+codegen | medium | `Map[K, weak V]` is now store-only: the read is NOT an upgrade, so `m.get(k)` yields `Option[weak V]` and the `Some` binding rejects every field acce… | 68bebfd3 |
| B-2026-08-09-3 | codegen | medium | a `shared struct` binding's Drop body fires at LEXICAL SCOPE EXIT under codegen but at LIVE-RANGE END under `--interp` -- design.md mandates live-ran… | e9e3807 |
| B-2026-08-09-4 | codegen | medium | `let r = <Option/Result>.map(f)` over a HEAP payload leaks the result's payload once per evaluation — `map_passthrough_armed_source` claimed EVERY `.… | 5698333 |
| B-2026-08-09-5 | codegen | high | an indirect closure call lowered the return type from the SURFACE `Fn(..)` type while the emitted body used its own, so a borrowed-String mapper's 3-… | 37d0992a |
| B-2026-08-09-6 | typecheck+codegen | high | `Result[T, E].map(f)` never learns `E`, so a HEAP `Err` payload is mishandled on the pass-through branch: `Result[i64, String]` DOUBLE-FREES and abor… | cfd574e3 |
| B-2026-08-09-7 | codegen | medium | chaining any two Result-returning combinators without an intervening `let` fails in codegen -- `r.map(f).map(g)` panics (`ExtractOutOfRange`) under a… | bf5737a0 |
| B-2026-08-09-8 | codegen | high | a bare REBIND of an inline-Option-payload local (`let p = o`) followed by TWO reads of `p` double-frees the payload -- the caller-retains classifier… | 0137be0 |
| B-2026-08-09-9 | codegen | medium | a user enum with a `Vec[String]` payload consumed by a match arm over a LIVE local still empties the source -- the enum deep-copy is OUTER-buffer onl… | 7d9ecce1 |
| B-2026-08-09-10 | interp | medium | `--interp` SKIPS the Drop body of a struct payload bound out of an owned enum PARAM, where both compiled backends run it -- the param-shaped sibling… | 128b746 |
| B-2026-08-09-11 | codegen | medium | the `if let` spelling of a CONSUMING arm over a live user-enum local still empties the source -- the match site got a live-local clone leg, the if-le… | 43cc3cb |
| B-2026-08-09-12 | codegen | high | the `<refparam>.field` ref-chain ENUM clone leg shares the outer-only payload duplicator, so a `Vec[String]` payload behind a `ref` param aliases its… | 9a15e07c |
| B-2026-08-09-13 | codegen | medium | a `Vec[heap-element]` enum payload leaks EVERY element -- `__karac_drop_E` frees the outer buffer only, the documented `EnumDropKind::VecOrString` v1… | d52a1312 |
| B-2026-08-09-14 | codegen | high | a CONSUMING `while let` arm over a plain enum local whose source is DEAD after the loop double-frees -- the `match` and `if let` spellings of the sam… | a2e1f42 |
| B-2026-08-09-15 | codegen | medium | codegen runs a `Drop` body TWICE when a match arm RETURNS a Drop-carrying payload out of an owned enum param -- once in the callee, once at the calle… | bd5c2c2 |
| B-2026-08-09-16 | codegen | high | a `let` that aliases a match-arm payload bound off an owned enum PARAM (`let k = r; return k;`) double-frees the payload's String -- the let-move sup… | 1b9901f |
| B-2026-08-09-17 | codegen | high | a `File` MOVED out of its binding (into a `Vec`, a struct, or a return) is still closed by the origin binding at scope exit, so the new owner holds f… | fdc874b |
| B-2026-08-09-18 | interp | low | Interpreter ICEs (`internal error: entered unreachable code`) on a METHOD CALL whose RECEIVER faulted, instead of reporting the receiver's runtime er… | bb46a68d |
| B-2026-08-09-19 | interp | low | SIBLING OF B-2026-08-09-18, NOT CLOSED BY bb46a68d: a faulted operand still ICEs the interpreter in THREE more positions, all with the same shape (`u… | 512f59a |
| B-2026-08-09-20 | codegen | medium | a `File` moved into a `Vec` or a struct is never closed -- the container has no element/field drop for the handle, so the fd leaks until process exit… | cc48dcca |
| B-2026-08-09-21 | codegen | medium | A NESTED index whose base is a STRUCT FIELD (`h.data[i][j]`) is rejected by codegen -- `codegen: nested indexed read requires the outer container to… | 4f3d6921 |
| B-2026-08-10-1 | codegen | medium | a NESTED indexed store that overwrites a heap element (`d[i][j] = <String>`) leaks the old value -- the single-index store frees it, the nested one d… | 1b6ed41 |
| B-2026-08-10-2 | typecheck | low | the `already a mut-ref; drop the `mut` marker` diagnostic tells the author to delete one token but carries no machine-applicable replacement, so `kar… | 93444f5 |
| B-2026-08-10-3 | typecheck+interp+codegen | medium | `File` has no `seek` on the Kāra surface even though the runtime entry point `karac_runtime_file_seek` is already implemented and exported — so any r… | 9f1e3c6f |
| B-2026-08-10-4 | typecheck+interp+codegen | medium | `split_at_mut` is fully specified in design.md but implemented nowhere, so there is NO way to obtain a mutable sub-view of a buffer — `buf[n..]` yiel… | 04bcd16 |
| B-2026-08-10-5 | typecheck+codegen | medium | index-assign through a TUPLE FIELD whose type is a Slice is rejected by codegen (`p.0[0] = x`) — the same spelling works when the field is a Vec, and… | a51ef80 |
| B-2026-08-10-9 | codegen | high | `Vec.sort_by`'s mono path was a NON-ADAPTIVE fixed-32-run bottom-up merge sort: it did ceil(log2(n/32)) = 13 full passes over 2.4 MB for 150k element… | 50a50e8 |
| B-2026-08-10-13 | codegen | medium | Inside a `sort_by` COMPARATOR CLOSURE, a closure parameter supports only TUPLE-FIELD access; any METHOD CALL or INDEX on it falls through codegen's m… | b90027e |
| B-2026-08-10-16 | codegen | medium | An explicit `return` inside a `sort_by` COMPARATOR CLOSURE emits an LLVM module-verification failure: `Module verification failed: "Function return t… | 568e6ff |
| B-2026-08-10-17 | typecheck | medium | a `return` NESTED inside a closure body (in an `if` / loop) is typechecked against `()` instead of the closure's return type, so an early return from… | 819af61 |
| B-2026-08-10-18 | codegen | medium | an explicit `return` inside an ITERATOR ADAPTOR closure (`map`/`filter`/`any`/`all`/`retain`) fails codegen with `Terminator found in the middle of a… | c0c8842 |
| B-2026-08-10-19 | codegen | medium | `Vec[(i64,i64)].sort_by` on SHUFFLED-UNIFORM input is ~1.66x Rust's `sort_by` (karac 14.82 ms vs driftsort 8.93 ms, 150k pairs, this host, both progr… | 31485cd |
| B-2026-08-10-21 | codegen | medium | the `UseAfterMove` defensive copy that `cli.rs` promises DOES NOT EXIST for any heap type on the binding-to-binding move path -- `karac check` exits… | bb663f1d |
| B-2026-08-11-2 | typecheck | medium | `char` and `bool` receivers skip method-existence checking entirely, so ANY method name passes `karac check` and unifies with ANY return type -- the… | c2be671 |
| B-2026-08-11-3 | codegen | medium | a generic struct's method whose parameter is the TYPE PARAMETER leaks a fresh TEMPORARY argument's buffer -- `s.push([1i64,2i64])` on a `Stack[T]` le… | 3dd2e4b |
| B-2026-08-11-4 | typecheck | low | `v.cast()` on ANY primitive receiver passes `karac check` and then fails at run time -- `cast` sits on the PRIMITIVE_VALUE_METHODS exemption but is n… | 0aca4db |
| B-2026-08-11-5 | parser | high | EVERY parse-phase diagnostic raised inside an f-string interpolation hole is DISCARDED | 3baa5a4 |
| B-2026-08-11-6 | typecheck | high | A bare TYPE NAME in call position -- `i64(42)`, `F64(1.5)`, `bool(1)`, `String("hi")`, `Vec(1)`, or a named-field user struct `P(1)` -- is accepted b… | fc9c3fb |
| B-2026-08-11-7 | typecheck | high | `Vec[f64].sort()` bypasses the `Ord` gate that design.md § Float semantics REQUIRES -- the spec's own verbatim counter-example compiles | e51d3e7 |
| B-2026-08-11-15 | typecheck+codegen | medium | `Vec[f64].max()` / `.min()` return an ORDER-DEPENDENT element instead of erroring, and there is currently no working remedy to point users at -- whic… | 3d0064f |
| B-2026-08-11-8 | typecheck | medium | `F64.from(x)` / `F32.from(x)` -- the total-order wrapper constructor that design.md AND the compiler's own `T: Ord` diagnostic both name as THE fix f… | d8ddc56 |
| B-2026-08-11-13 | codegen+interp | medium | `F64`/`F32` ordering and equality depend on the SIGN BIT of a NaN, which is not stable across backends or optimization levels -- so `Vec[F64].sort()`… | 284d44a4 |
| B-2026-08-11-14 | other | low | design.md § Float semantics specifies the `F32`/`F64` total order as `-Infinity < .. | 284d44a4 |
| B-2026-08-11-9 | typecheck | low | the seven comparison-op names are exempted from method-existence checking by NAME rather than by whether the receiver carries the baked impl, so `f.c… | 22ba601 |
| B-2026-08-11-10 | codegen | low | `Vec[(i64,i64)].sort_by` on FEW-UNIQUE input (150k pairs over 8 distinct keys) is 5.93 ms / 48.8M instructions vs driftsort's 1.81 ms / 14.2M on this… | 3d77cc60 |
| B-2026-08-11-1 | codegen+typecheck | high | a `Vec[char]` INDEX used directly as a method receiver (`cs[0].to_string()`) loses its `char` type and dispatches to the INTEGER method, so codegen s… | 7372313 |
| B-2026-08-11-11 | typecheck | medium | TWO defects at the tuple receiver, filed together and NOT one bug: (a) tuple-index projection through a `ref` is rejected while the identical struct-… | 3abdda1 |
| B-2026-08-11-12 | codegen | high | a borrow-returning accessor's payload handed through `unwrap_or` (`m.get(k).unwrap_or(d)`, `v.get(i).unwrap_or(d)`) is an ALIAS of the container's st… | bf43b275 |
| B-2026-08-11-16 | autopar+codegen | high | Auto-par (ON BY DEFAULT) silently DROPS every LITERAL-step accumulator in a `while` loop that also contains a NON-literal-step one: `while i < n { b… | 7fe2b6dd |
| B-2026-08-11-17 | interp | high | The INTERPRETER's `sort_by_key` with a float key returns a COMPLETELY UNSORTED sequence when a NaN is present, while both compiled backends sort corr… | c6c34c3 |
| B-2026-08-11-18 | codegen | medium | Chained field access on an Option-unwrap TEMPORARY fails in codegen while working in the interpreter: `get().unwrap().x` where `get() -> Option[P]` e… | e23bf9e |
| B-2026-08-11-19 | interp+codegen | medium | The DIRECT-on-Vec iterator terminals (`v.max()` / `v.min()`, desugared to the `.iter()` chain) are POSITION-SENSITIVE: they compile in statement/argu… | cb3d9015 |
| B-2026-08-11-20 | typecheck+codegen | low | `f64.to_bits()` is declared `-> u64` by the typechecker but its value renders as a SIGNED i64 on all three backends: `(-1.0).to_bits()` prints -46161… | 1597660 |
| B-2026-08-11-21 | codegen+interp | high | EVERY un-annotated `let` holding an unsigned value prints SIGNED under both compiled backends while the interpreter prints it correctly -- `let a = 1… | cacd93e1 |
| B-2026-08-11-22 | codegen | medium | CHAINING ONTO A SCALAR'S `.to_string()` BREAKS CODEGEN TWO WAYS, both check-green and both interpreter-correct: `n.to_string().to_string()` PANICS th… | 13b4814d |
| B-2026-08-11-23 | codegen | low | `Vec[T].sorted_by_key(f)` PASSES `karac check` and then fails at build: "Vec/String method 'sorted_by_key' is not yet supported in codegen" | a243f2a |
| B-2026-08-11-24 | codegen | high | A String EQUALITY comparison between an unbound TEMPORARY and a `ref String` PARAMETER leaks the temporary, every evaluation | 7239101 |
| B-2026-08-11-25 | codegen | high | DOUBLE FREE on both compiled backends: a heap field of a struct held as a Vec ELEMENT, read back by ASSIGNMENT to an existing binding (`out = stats[0… | 970dadfd |
| B-2026-08-11-26 | other | medium | the codegen suite's JIT lane — the ONE lane whose stated job is run==build parity — fed codegen `ownership: None` while its AOT twin fed `Some(&owner… | 883fcbe1 |
| B-2026-08-11-27 | autopar+codegen | high | NONDETERMINISTIC SILENT WRONG ANSWER on the DEFAULT `karac build`: a 13-line program that overwrites a `Vec[Tensor]` element through a `shared struct… | 7dfc17c |
| B-2026-08-11-29 | codegen | high | SEGV (exit 139) on a DEFAULT `karac build`: a struct with a `Map`/`Set` field, bound out of a `Result`'s `Ok(..)` match arm and then passed BY VALUE… | 8e1f96dc |
| B-2026-08-11-30 | codegen | medium | A by-value `Option`/`Result` parameter that the callee never DESTRUCTURES is dropped by NO frame, so the entire `Ok`/`Some` payload leaks: the caller… | 19d0e9a |
| B-2026-08-11-31 | other | medium | `tests/par_codegen.rs` -- the ONLY lane that threads `ConcurrencyAnalysis` into codegen -- had no JIT leg at all, so the DEFAULT `karac build` config… | 43face1 |
| B-2026-08-11-32 | codegen | high | a widening cast on an unsigned `Vec` element read through a struct FIELD sign-extends instead of zero-extending -- `h.px[0] as f64` on a `Vec[u16]` h… | 023bc9a |
| B-2026-08-11-33 | codegen | medium | A `#[derive(Eq)]` STRUCT temporary carrying a HEAP field, compared against a `ref` param, leaks that field every evaluation: `mk(hay) == other` with… | 23e5d66 |
| B-2026-08-11-34 | other | medium | The E2E harnesses guard `parse` errors and then DISCARD `resolve` and `typecheck` errors, while `karac build` stops on either -- so the suite silentl… | 9410488b |
| B-2026-08-11-35 | cli | high | `karac fix` DESTROYS SOURCE, silently and while reporting success, when the machine-applicable diagnostic sits INSIDE AN F-STRING INTERPOLATION | 35d7fec |
| B-2026-08-12-2 | codegen | medium | A `Map` field of an `Ok` payload leaks ~72 B per call when the `Result` is LET-BOUND before the match; matching the producing call INLINE is clean, a… | c081b16 |
| B-2026-08-12-3 | other | medium | `tests/codegen.rs`'s E2E harness ran `resolve` and `typecheck` only to feed `lower` and DISCARDED their errors, so the suite stayed green on 40 progr… | e0c6bab |
| B-2026-08-12-4 | codegen | high | `asan_vec_element_field_move_by_assignment_no_double_free` is red on main with a LeakSanitizer leak, and NONDETERMINISTICALLY so: green in the full p… | 3e8d8fa9 |
| B-2026-08-12-5 | codegen | high | SILENT WRONG ANSWER (run-vs-build): `#[derive(Eq)]` equality over a struct with a `Vec[String]` field reports NOT EQUAL in both compiled backends for… | a827a7f |
| B-2026-08-12-6 | other | medium | 103 of the 109 codegen-invoking test harnesses in `tests/` resolved the RAW parse tree, skipping some or all of the three AST rewrites `karac build`… | b3a1061 |
| B-2026-08-12-1 | codegen | high | Passing a by-value `Option`/`Result` argument TWICE makes every call after the first read the WRONG VARIANT -- `Err`/`None` for a value that is still… | c24343b |
| B-2026-08-12-7 | typecheck | medium | A union or `#[derive(Copy)]` struct with a RAW POINTER field is rejected as not-`Copy` by the one rule whose own suggestion is "hold it behind a raw… | 9410488b |
| B-2026-08-12-8 | typecheck | medium | Four methods that CODEGEN FULLY IMPLEMENTS are rejected by the typechecker, so `karac build` refuses programs the backend demonstrably compiles and r… | 9f4fc14 |
| B-2026-08-12-9 | typecheck+codegen | low | The `f64` sort/max/min emitters are UNREACHABLE from any valid program: the F64 total-order rule rejects `Vec[f64].sorted()` / `.max()` / `.min()` at… | 2768ad1 |
| B-2026-08-12-10 | typecheck | low | Implicit narrowing to a refinement type works for an INTEGER literal but not for a FLOAT or STRING literal, and the diagnostic then states something… | 4ecd716 |
| B-2026-08-12-11 | codegen | medium | Codegen's TWO type-lowering entry points kept two hand-maintained lists of built-in handle types and DISAGREED: `llvm_type_for_type_expr` answered `p… | 9b2f5ad |
| B-2026-08-12-12 | codegen | low | SIX type names still lower to the silent `i64` default with no LLVM layout of their own, across 16 of the 2906 `tests/codegen.rs` programs: `Unit` (6… | 75fbfc0 |
| B-2026-08-12-13 | codegen+ownership | low | Assigning TWICE from the same already-moved-out place (`cur = box[0].s;` … `cur = box[0].s;`) leaks that buffer: the first assignment cap-zeroes the… | eef6e980 |
| B-2026-08-12-14 | codegen | medium | Reading a field off a `Json.parse` error is a RUN-VS-BUILD split: `karac run --interp` prints `e.line` / `e.column` / `e.message` correctly, while `k… | 8268903 |
| B-2026-08-12-15 | codegen | high | A boxed `Option` FIELD envelope inside an inline STRUCT enum payload (`Result[W, i64]` over `struct W { o: Option[Option[i64]] }`) has no owner in AN… | c0320d7 |
| B-2026-08-12-16 | codegen | low | `Json.parse`'s error message LEAKS: codegen copies the runtime's diagnostic into a Kara String but pins that String's `cap` to 0, so the scope-exit f… | ec58cb0 |
| B-2026-08-12-17 | codegen | medium | A boxed `Option` FIELD envelope inside an inline STRUCT enum payload still leaks 32 B per call when the by-value ARGUMENT is not a fresh construction… | 5015d5d |
| B-2026-08-12-18 | codegen | low | The INTERIOR of a boxed `Option` field envelope owned by the fresh-temp argument spill has no owner: `cls(Result.Ok(S { o: Option.Some(Option.Some(f"… | 1e3aef1 |
| B-2026-08-12-19 | codegen | low | The CALLEE's ENTRY COPY of a `Result[S, i64]` whose struct payload has a boxed `Option` field leaks WHOLE -- box and interior both -- when the callee… | 44eaf8e |
| B-2026-08-12-20 | effect | high | A write to a captured local from inside `par { }` is silently DROPPED when it is routed through a `mut ref` parameter (`par { bump(mut v); … }`) or a… | d37c097 |
| B-2026-08-12-21 | parser | medium | An assignment written as a bare `match` arm body (`Some(q) => total = total + q,`) produced THREE errors, two of them fictional — including a bogus '… | 9abcf48 |
| B-2026-08-12-22 | codegen | high | DOUBLE FREE on both compiled backends: index-assigning a WHOLE struct element read out of the same Vec (`let b = ps[1]; ps[0] = b;`, element = struct… | af43027 |
| B-2026-08-12-23 | ownership | medium | `E_CONCURRENT_PLAIN_STRUCT` fires on BUILTIN containers (`Vec` / `Map` / `Set`) and then prescribes a migration the user cannot perform — 'rename `st… | 9d7db97 |
| B-2026-08-12-24 | codegen | medium | Inside a GENERIC IMPL, `let`-binding a `T`-typed struct FIELD and then calling a trait method on that local (`let a = self.v; a.describe()`) fails th… | 2282be2 |
| B-2026-08-12-25 | typecheck | low | `char` has no `to_lowercase` / `to_uppercase` / `is_digit`, though it has `to_ascii_lowercase`, `is_alphabetic`, `is_numeric`, `is_alphanumeric` and… | ef73c4d |
| B-2026-08-12-26 | codegen | medium | ELEMENT-TO-ELEMENT index assign LEAKS one buffer per assignment: `ps[0] = ps[1]` over `Vec[Pair]` with `struct Pair { word: String, n: i64 }` loses 4… | 4da8bc7 |
| B-2026-08-12-27 | codegen | high | A heap FIELD read out of a Vec element (`ps[0].word`) is a SHALLOW ALIAS of the container's buffer on both compiled backends | d19d0d6 |
| B-2026-08-12-28 | codegen | low | A chained indexed field READ (`a[i][j].field`) failed `karac build` with the generic self-accusing 'cannot resolve field .. | 63b0bd19 |
| B-2026-08-12-29 | typecheck | low | The `s[i]`-on-String rejection prescribes `s.char_at(i)` as the substitute, but `char_at` returns `Option[char]` — so the suggested replacement does… | a092d138 |
| B-2026-08-12-30 | parser | medium | GENERIC PARAMETERS, TRAIT BOUNDS and WHERE-CLAUSES are absent from the span walker ENTIRELY -- not a missing field but a missing subtree | 3bf70a6 |
| B-2026-08-12-31 | codegen | medium | The displaced element still LEAKS when an index-assign's RHS mentions the container through a CALL: `ps[0] = mk(ps[0].n + k)` over `Vec[Pair]` with `… | 19dbf4a |
| B-2026-08-12-33 | codegen | medium | The displaced element still LEAKS when an index-assign's RHS passes container HEAP into a call: `ps[0] = passthru(ps[0])` and `ps[0] = takes(ps[0].wo… | 0eec7ad |
| B-2026-08-13-1 | ownership | low | The ownership checker treats an OWNED String passed as the ARGUMENT of a read-only String method as a MOVE, and does so INCONSISTENTLY across the thr… | 1299fd3 |
| B-2026-08-12-32 | typecheck | medium | A user trait `impl` on `String` or `Slice[T]` is ACCEPTED but never found at the call site: `impl Zero for String { . | ad9fb73 |
| B-2026-08-13-2 | codegen | medium | `.to_string()` on a NON-IDENTIFIER scalar receiver is check-green and interp-green but dies under BOTH compiled backends the moment it is used as a r… | 97a2a69 |
| B-2026-08-13-3 | codegen | high | Passing a Vec ELEMENT whose struct has a NESTED STRUCT field to a call and assigning the result back to that same slot DOUBLE-FREES on both compiled… | 5e6b6f0 |
| B-2026-08-13-4 | codegen | high | Binding or consuming a NESTED heap field read off a Vec ELEMENT double-frees: `let w = ds[0].inner.word` over `Vec[Deep]` with `struct Deep { inner:… | 15284a9 |
| B-2026-08-12-34 | typecheck+codegen | medium | A user trait `impl` on `Slice[T]` / `Map` / `Set` is still not callable, and `Map`'s is check-green with BOTH compiled backends dead | ec1d19c |
| B-2026-08-13-5 | codegen | high | A heap field read off a SLICE element double-frees at ANY depth: `fn take(xs: Slice[Pair]) -> String { xs[0].word }` aborts with `free(): double free… | f005e16 |
| B-2026-08-13-6 | codegen | high | A heap field read through a SHARED-struct hop off a Vec element double-frees: `shared struct Inner { word: String }` inside `struct Holder { inner: I… | 71629ef |
| B-2026-08-13-7 | typecheck+interp+codegen | medium | A user trait `impl` on `Slice[T]` is not callable at all -- `no method` at check, so `T: Zero` over a slice cannot be written | b0ef9888 |
| B-2026-08-13-8 | interp+codegen | high | Two impls of one trait on the same type head with different type arguments collapse to ONE dispatch target, and the backends pick OPPOSITE ones -- `-… | e9ab6470 |
| B-2026-08-13-9 | codegen | medium | A trait-BOUND call over a user-impl'd builtin container (`fn show[T: Zero](x: ref T)` with a `Map`/`Set`/`Slice`) is check-green and interp-green wit… | 4cf8f38 |
| B-2026-08-13-10 | codegen | medium | kara is 1.58x behind EQUAL-SAFETY rustc and 1.49x behind clang on the min/argmin/second-min reduction of kata #265's O(n*k) DP, because LLVM if-conve… | 3ea8310 |
| B-2026-08-13-11 | codegen | high | DOUBLE FREE on both compiled backends when a heap payload makes an ENUM-MEDIATED ROUND TRIP through a Vec: read out of a Vec element into a returned… | 3e62426 |
| B-2026-08-13-12 | cli | low | The DELIBERATE codegen deferral of chained field receivers (`e.doc.lines.len()`, FR4) is invisible to `karac check`: check reports `All checks passed… | 30ea825 |
| B-2026-08-13-13 | parser | low | A parse diagnostic PRESCRIBES CODE THAT DOES NOT PARSE: the bare-assignment-in-a-match-arm error says "wrap it in braces: `pattern => { place = value… | 0fa42ca |
| B-2026-08-13-14 | interp+codegen | high | Binding a NESTED STRUCT field (`let mut t = b.a;`) EMPTIES the source under both compiled backends while the interpreter treats it as an ALIAS: codeg… | 7994156 |
| B-2026-08-13-15 | typecheck+codegen | high | SILENT WRONG ANSWER: `Set[i64].contains(x)` called with a NARROWER integer (a `u8` from `String.bytes()`) returns false for an element that IS presen… | 675494c |
| B-2026-08-13-16 | interp | medium | The INTERPRETER aliases a struct binding: `let mut t = a; t.lines.push(x)` is visible through `a`, where JIT and AOT both copy | 6cda7ae |
| B-2026-08-13-17 | codegen | medium | An ANNOTATED tuple binding ignores its annotation: `let t: (i64, i64) = (b, d)` with `b: u8` / `d: u32` lays the aggregate out from the element VALUE… | 35fb7aec |
| B-2026-08-13-18 | codegen | medium | An implicit int-to-FLOAT widening emits INVALID IR and fails module verification at three boundaries -- a struct-literal field, a field assignment, a… | aea7671 |
| B-2026-08-13-19 | codegen | medium | Binding a struct out of a TUPLE ELEMENT (`let r = t.0;`) EMPTIES the source under both compiled backends: a later read of `t.0` sees a zero-length Ve… | 73f7285 |
| B-2026-08-13-20 | codegen | high | SEGFAULT with no diagnostic: moving a HEAP-ELEMENT Vec out of a tuple (`let r = t.0;` where the element is `Vec[String]`) crashes both compiled backe… | fc1887a |
| B-2026-08-13-21 | codegen | medium | A tuple literal in RETURN / ARGUMENT / STRUCT-FIELD position is still laid out from its element values, so a narrow element under a wider declared ty… | cf30e6ba |
| B-2026-08-13-22 | codegen | high | An ANNOTATED ARRAY literal ignores its annotation the way B-2026-08-13-17's tuple did: `let a: Array[i64, 2] = [v, v]` with `v: u8 = 200` lays the ag… | 6b74cb11 |
| B-2026-08-14-1 | typecheck+codegen | high | FIVE SITES ACCEPT AN IMPLICIT NARROWING the language says it refuses — Vec.push, Vec.contains, Set.insert/contains, Set.remove and the annotated tupl… | 1edb623 |
| B-2026-08-14-2 | interp | high | INTERPRETER-SIDE: the implicit int-to-float widening is not performed at all, so an int source bound to a float destination stays an Int | ebe60bf |
| B-2026-08-14-3 | codegen | high | GENERICS AT UNSIGNED NARROW WIDTHS RETURN SIGN-EXTENDED GARBAGE on both compiled backends: `fn idg[T](x: T) -> T` called as `idg(200u8)` yields -56,… | 6f6256c |
| B-2026-08-14-4 | codegen | high | A STRUCT WITH UNSIGNED NARROW FIELDS IS CORRUPTED BY A Vec ROUND-TRIP: reading `v[0].a` where `a: u8 = 200` yields -56 on both compiled backends, whi… | 6f6256c |
| B-2026-08-14-5 | codegen | medium | A FIELD READ ON AN ARRAY-INDEXED STRUCT IS A HARD CODEGEN GAP: `arr[0].a` where `arr: Array[Plain, 1]` fails to build with "codegen: cannot resolve f… | 33ae956 |
| B-2026-08-14-6 | typecheck+codegen | medium | The int-to-float implicit widening is performed at NEITHER surface for a CONTAINER element: the interpreter stores an Int in a `Vec[f64]`, and the co… | 780a1d6 |
| B-2026-08-14-7 | interp | medium | The interpreter performs f32 ARITHMETIC at f64 precision, so an `f32` result diverges from both compiled backends wherever the true f32 result would… | 108f01b4 |
| B-2026-08-14-8 | codegen | medium | `Slice[T].contains` typechecks and interprets but has NO codegen: `karac check` passes, `karac run --interp` answers correctly, `karac build` dies wi… | e803ba7 |
| B-2026-08-14-9 | codegen | medium | EIGHT more `Slice[T]` methods typecheck and interpret but have no codegen: the mutators `fill`/`reverse`/`sort`/`sort_by_key`/`swap` and the view-pro… | 8339a6d |
| B-2026-08-14-10 | typecheck | high | NONDETERMINISTIC TYPECHECKING: a user enum declaring a variant named `None`, `Some` or `Ok` makes `karac check` return a COIN FLIP on identical input… | 9cea4a0 |
| B-2026-08-14-11 | typecheck+codegen | medium | An UNSUFFIXED float literal at a narrow-float annotation is never narrowed, on EITHER surface: `let a: f32 = 0.1` holds the f64 0.1 where `let b: f32… | 039d204 |
| B-2026-08-14-12 | typecheck | medium | An implicit float NARROWING between two declared types is accepted with no diagnostic and no rounding on either surface — `let c: f64 = 0.1; let d: f… | 6b891bf |
| B-2026-08-14-13 | typecheck | medium | Mixed-width float arithmetic between two TYPED operands is accepted and takes the LEFT operand's width, so `a * b` is `f32` and `b * a` is `f64` for… | d9fb605 |
| B-2026-08-14-14 | typecheck+codegen | medium | A TENSOR-scalar operation never checks its scalar operand against the element type: `Tensor[f16] * some_f64` silently narrows the f64 to `f16`, and `… | d6dc6d8 |
| B-2026-08-14-15 | codegen | high | Two leaks that share one polarity: a NESTED CONTAINER read into a `let` BINDING and then consumed leaks, while the identical read consumed INLINE is… | 1282910 |
| B-2026-08-14-16 | codegen | high | THE AUTO-PAR TABULATE REWRITE STORES A COMPUTED SUB-WORD ELEMENT AT 8 BYTES: a `while` loop whose body is `v.push(<computed u8>)` is rewritten to a h… | fc266fa |
| B-2026-08-14-17 | codegen | medium | Indexing a tensor-valued TEMPORARY fails to build while `--interp` runs it: `(t * 2)[0]`, `(t + t)[0]`, `Tensor.from([1.0, 2.0])[0]` and `(0.0 - t)[0… | e734d98 |
| B-2026-08-14-18 | codegen | high | Reassigning a `mut` CONTAINER field (`Vec` / `Map` / `Set`) of a `shared struct` never frees the OLD container -- the whole thing leaks, scaling with… | b0bc91d |
| B-2026-08-14-19 | interp+codegen | medium | `String.substring` AT A NON-CODEPOINT BOUNDARY DIVERGES BETWEEN `karac run` AND `karac build`: the interpreter replaces each invalid byte with U+FFFD… | 0d8a18d |
| B-2026-08-14-20 | typecheck | low | THE `String -> bytes -> String` ROUND TRIP DOES NOT TYPECHECK: `String.from_utf8(s.bytes())` fails with "expected 'Vec<u8>', found 'Slice[u8]'", and… | ebc20d37 |
| B-2026-08-14-21 | codegen | high | `s += x` ON A `mut ref String` PARAMETER IS SILENTLY DROPPED IN COMPILED CODE: the callee's append never reaches the caller's binding | e6605e9 |
| B-2026-08-14-22 | codegen | high | `s += x` ON A LOCAL `String` LEAKS THE ENTIRE PREVIOUS BUFFER ON EVERY APPEND: building a 160 KB string with 20,000 appends peaks at 1.5 GB of RSS —… | 69abc03 |
| B-2026-08-14-23 | codegen | medium | `s = s + x` NEVER REUSES THE LEFT BUFFER, so building a string by repeated append is 21x slower than `s.push_str(x)` and 17x slower than the same spe… | 645bc75 |
| B-2026-08-14-24 | autopar | medium | a disjoint-write loop nested inside an `if` (or a bare block) is INVISIBLE to the auto-parallelisation analysis -- it is not declined with a reason,… | 7360ad8 |
| B-2026-08-14-25 | codegen | medium | A `String` field of a `shared struct` NEVER releases the buffer its reassignment displaces — one leaked buffer per assignment on every layout, not (a… | 04e550b |
| B-2026-08-14-26 | codegen | high | A field assignment through a CHAINED shared parent (`outer.inner.field = v`, both `shared struct`) is SILENTLY DROPPED under `karac build` while `--i… | 38e15acd |
| B-2026-08-14-27 | codegen | high | BINDING AN INNER `Vec` ELEMENT OF A `ref Vec[Vec[i64]]` TO A LOCAL DOUBLE-FREES ITS BUFFER: `let first = m[0i64];` copies the row header without a re… | 0dc28cf4 |
| B-2026-08-14-28 | codegen | high | A `shared struct` LINKED LIST BUILT THROUGH A DUMMY-HEAD CURSOR IS READ AFTER FREE: `leetcode/1-100/2-add-two-numbers/iterative.kara` SEGFAULTS under… | be32e463 |
| B-2026-08-14-29 | typecheck | medium | COMPOUND ASSIGNMENT IS NOT OPERAND-TYPE-CHECKED AT ALL: `s += 1i64` on a String, `n += "a"` on an i64, and `p += 1i64` on a struct all report "All ch… | 0fea7d1 |
| B-2026-08-14-30 | codegen | high | Interpolating a DECODED `repeated string` protobuf field SEGFAULTS under `karac build` — `println(f"{rt.members}")` on a `Team.decode(...)` result di… | 823ab4b |
| B-2026-08-14-31 | codegen | medium | Printing a whole `Map` or `Set` STRUCT FIELD emits a raw pointer address under `karac build` — `println(f"{b.m}")` gives `{aaaaaaaaaaaa: 1}` under `-… | 1062214 |
| B-2026-08-14-32 | codegen | high | an index read into a heap-owning Vec, used as the value of an `if`/`match` ARM, is freed by both the binding and the container -- `let w = if c { v[i… | 97c63ac |
| B-2026-08-14-33 | autopar | medium | `query concurrency` reports `fanned_out: false, cost_gate: "unknown"` for a disjoint-write loop nested in an `if` or a block that DOES fan out -- the… | Both loop lookups behind the report -- find_loop_by_span (d… |
| B-2026-08-14-34 | resolver+ownership | medium | DIAGNOSTIC OUTPUT IS NONDETERMINISTIC RUN-TO-RUN on the same binary and the same input, in two independent places: a resolver "did you mean" suggesti… | a5cb578 |
| B-2026-08-14-35 | codegen | medium | `SortedMap` and `SortedSet` print with the WRONG PREFIX under `karac build` on every spelling, bound variable included: `SortedMap{kk: 1}` renders as… | 097987b |
| B-2026-08-14-36 | codegen | medium | A `Map` or `Set` TEMPORARY that is printed and not bound leaks its whole handle — `println(f"{mk()}")` in a 40-print loop strands 21440 bytes over 12… | 320a86b |
| B-2026-08-14-37 | ownership | medium | AN IMMUTABLE `Slice[T]` FORMAL REPORTS ITS OWNED `Vec[T]` ARGUMENT AS MOVED: `fn take(xs: Slice[u8])` called as `take(v)` warns `value 'v' moved here… | 60c94174 |
| B-2026-08-14-38 | codegen | low | INDEXING THE RESULT OF A Vec-RETURNING METHOD CALL loud-bails `Index operator applied to non-array type` under `karac build` while `--interp` runs it… | FIXED |
| B-2026-08-15-1 | codegen | medium | An UNANNOTATED `let m = <call returning SortedMap/SortedSet>` registers nothing in codegen: `f"{m}"` prints the control pointer, `println(m)` prints… | 8668bbf2 |
| B-2026-08-15-2 | codegen+ownership | high | `s.push_str(s)` ON A HEAP STRING THAT MUST GROW IS A USE-AFTER-FREE: the grow reallocs the destination and the copy then reads through the now-stale… | 074fa446 |
| B-2026-08-15-3 | codegen | low | LOOP-BOUND PRE-SIZING RECOGNIZES THE FILL ONLY AS A `push`/`push_str` METHOD CALL, so an accumulator filled by `s = s + x` or `s += x` starts at capa… | e8d1093 |
| B-2026-08-15-4 | autopar | low | `cost_gate: "unknown"` conflates "the loop lookup failed" (a compiler bug) with "found it, but the iterable is not a shapeable range" (a legitimate l… | 3893197 |
| B-2026-08-15-5 | codegen | medium | shared-struct String field reassignment never frees the displaced buffer (hidden at default opt by LICM; -O0 leg caught it) | 04e550b0 |
| B-2026-08-15-6 | codegen | medium | THE DEEP-CLONED HEAP ELEMENT OF AN INLINE-TEMP-VEC INDEX LEAKS WHEN THE INDEX IS AN F-STRING INTERPOLATION OPERAND but not when it is a direct print… | 3d190f71 |
| B-2026-08-15-7 | codegen | medium | A FRESH-OWNED `Vec` TEMPORARY PASSED BY VALUE TO A GENERIC FN'S OWNED PARAM IS NEVER DROPPED — `take(nums.clone())` strands one buffer per call when… | 2623cde0 |
| B-2026-08-15-8 | autopar | low | A DISJOINT-WRITE ENTRY HAS NOWHERE TO PUT ITS COST-DECLINE PROSE: its `reason` field is the disjointness proof's, so `cost_gate` names the declining… | ac23f22 |
| B-2026-08-15-9 | codegen | medium | A FRESH-OWNED `Vec` TEMPORARY PASSED TO A BARE-`T` GENERIC PARAM THAT THE CALLEE RETURNS is never dropped -- `pick(x.clone(), y.clone())` on `fn pick… | 57282b03 |
| B-2026-08-15-10 | codegen | high | A `UseAfterMove` WHOSE MOVE IS A CALL ARGUMENT GETS NO DEFENSIVE COPY: `uam_defensive_copy` covers three POSITIONS (a `let` RHS and the two struct-li… | 74e558f |
| B-2026-08-15-11 | typecheck | high | EXHAUSTIVENESS IS NOT CHECKED when the `match` scrutinee is a `ref` PARAMETER -- `fn f(m: ref M) -> i64 { match m { A => 1 } }` over a three-variant… | 526d672 |
| B-2026-08-15-12 | codegen | medium | `codegen failed: Undefined variable 'm'` on a program `karac check` accepts and the interpreter runs correctly: an enum-variant pattern binds a `shar… | 454af75 |
| B-2026-08-15-13 | codegen | medium | A `Map`/`Set` LOCAL IN A FUNCTION WITH A `Vec[Struct]` PARAM makes `let e = entries[0]; e.field` FAIL TO BUILD — `codegen: cannot resolve field '...'… | 3c6ac77c |
| B-2026-08-15-14 | codegen | medium | AN INLINE CONTAINER TEMPORARY IN ARGUMENT POSITION FREES ITS BUFFER WITHOUT RELEASING ITS ELEMENTS — `agg(ns.clone())` over a `Vec[shared Node]` stra… | fd7254d |
| B-2026-08-15-15 | codegen | high | A SLICE BINDING IS REGISTERED FOR A VALUE-TYPE STRUCT DROP OF ITS ELEMENT TYPE — `let s = es[0..2]` over a `Vec[Entry]` emits `__karac_drop_struct_En… | 409c699 |
| B-2026-08-15-16 | codegen | high | A RANGE-SLICE BINDING crossing an AUTO-PAR JOIN returns the WRONG LENGTH, silently, in the DEFAULT build — `let s = nums[0..2]; return s.len()` yield… | f0832187 |
| B-2026-08-15-17 | codegen | medium | `karac build` PANICS -- `alias attribute 'noalias' emitted on a non-pointer param lowering` -- for a user STRUCT shadowing an owned-ptr handle name p… | 454af75 |
| B-2026-08-15-18 | other | medium | PLAIN `cargo test` FAILED TO COMPILE on main: `tests/codegen.rs`'s `presize_reservation` module lost its `#[cfg(feature = "llvm")]` gate, so the whol… | 454af75 |
| B-2026-08-15-19 | autopar | medium | `#[par_order_free]` IS SILENTLY IGNORED when the analyzer does not classify the loop as a collect: no fan-out, no diagnostic, and `karac query concur… | b5af2c1 |
| B-2026-08-15-20 | codegen | low | THE COMPILED BACKENDS REPORT AN ARITHMETIC TRAP AT AN INCIDENTAL COLUMN — whatever subexpression happened to compile last inside the right operand —… | 731c5e9 |
| B-2026-08-15-21 | codegen | high | A FIELD ASSIGNMENT THROUGH A `mut Slice[Struct]` PARAMETER IS SILENTLY DROPPED under codegen — `fn bump(s: mut Slice[P]) { s[0].x = s[0].x + 1; }` le… | 9b284cb |
| B-2026-08-15-22 | typecheck | medium | A function returning a REUSABLE closure over a heap value is rejected whenever the body passes that capture BY VALUE -- which every stdlib `contains`… | 1f801ce8 |
| B-2026-08-15-23 | autopar+codegen | high | A `#[par_order_free]` COLLECT-TABULATE LOOP DISPATCHES FOUR WORKERS AND GIVES ALL THE WORK TO ONE: the order-free dynamic chunker sized chunks `.max(… | c04bc65 |
| B-2026-08-15-24 | codegen | low | A TUPLE-element field store through a `mut Slice[(A, B)]` parameter is not lowered — `fn bump(s: mut Slice[(i64, i64)]) { s[0].0 = 99; }` fails the b… | 3e6face |
| B-2026-08-15-25 | codegen | medium | A CLOSURE WHOSE BODY IS A BOOL-RETURNING BUILTIN PREDICATE fails LLVM module verification — `\|s\| s.starts_with("ab")` emits the closure's return as `… | f1474ffe |
| B-2026-08-15-26 | typecheck | low | THE `OnceFnIntoFnSlot` HELP TEXT PRESCRIBES A CLONE ROUTE THAT DOES NOT COMPILE — split out of B-2026-08-15-22 as its fix shape (3) | 80f24f2 |
| B-2026-08-15-27 | ownership | low | CALLING AN `Fn(ref T)` CLOSURE TWICE WITH THE SAME ARGUMENT warns "value moved here, used again here" — the closure's parameter is a BORROW, so the c… | 8242fae |
| B-2026-08-15-28 | codegen | high | A `ref`-DECLARED CLOSURE PARAM OF A HANDLE-BACKED BUILTIN IS DOUBLE-DEREFERENCED — `\|m\| m.len()` over a two-entry `Map` printed 529 under `karac buil… | f1474ffe |
| B-2026-08-15-29 | ownership | low | The B-2026-08-15-27 false positive SURVIVES when the closure arrives as a PARAMETER rather than a local — `fn run(f: Fn(ref String) -> bool)` calling… | 55ae608 |
| B-2026-08-15-30 | codegen | medium | The shuffled-uniform `sort_by` residual closed as `wontfix` in B-2026-08-11-28 at ~1.6x Rust is MATERIALLY LARGER on the canonical Apple-silicon host… | 93ea7a86 |
| B-2026-08-15-32 | ownership | low | An `OnceFn()` PARAMETER called twice is NOT reported — `fn run(g: OnceFn()) { g(); g(); }` passes clean, while the identical local (`let g = \|\| apply… | eec3aa5 |
| B-2026-08-16-1 | autopar+codegen | high | AUTO-PAR FORKS TWO INDEPENDENT `let`s AND THEN CODEGEN CANNOT SEE THEIR BINDINGS: two locals initialized from calls returning a heap collection, gath… | ROOT CAUSE: `refs_in_expr` (src/codegen/closures.rs) — the… |
| B-2026-08-16-2 | other | medium | `shared_ownership_matrix` CANNOT PASS ON macOS: 84529948 added an LSAN_OPTIONS env without the macOS guard ASAN_OPTIONS already had, so every cell ab… | e953aae0 |
| B-2026-08-16-3 | codegen | low | Shuffled `sort_by` is still ~1.80x driftsort after B-2026-08-15-30 routed it to the partition; the one measured lead is the scatter kernel's shape | 012645a5 |
| B-2026-08-16-4 | parser | high | Parser had NO recursion-depth limit: ~210 nested `(` crashed `karac check` with a stack-overflow abort (exit 134) instead of a diagnostic | ea042102 |
| B-2026-08-16-5 | runtime | medium | `env_set`/`env_var` fed the canonical empty Kāra String's null pointer to `slice::from_raw_parts` — library UB (benign today, a Miri/hardening trap) | a26de4bc |
| B-2026-08-16-6 | typecheck | high | A function whose body produces NO VALUE passes `karac check` despite a declared non-Unit return type: `fn f() -> String { }` is accepted, the interpr… | 7320d68 |
| B-2026-08-16-7 | codegen | high | A struct FIELD moved into a binding (`let cur = e.doc;`) and then passed as a `mut ref` ARGUMENT (`apply(mut e.doc, ..)`) loses the field's heap cont… | c049126 |
| B-2026-08-16-8 | other | medium | memory_sanitizer's four link-skip sites bypassed `link_or_skip` — the B-2026-07-28-1 stale-archive panic never protected the ASan/LSan suite | 220070be |
| B-2026-08-16-10 | other | medium | CI never built the regex/arrow opt-in archives, so the 8 Regex / Arrow-IPC codegen E2E tests (and memory_sanitizer's regex fixtures) green-skipped in… | c92476aa |
| B-2026-08-16-12 | cli | medium | The `chained_field_receiver` lint that closed B-2026-08-13-12 only walks SOME expression positions: it catches `let n = e.doc.lines.len();` and an `i… | 2683735 |
| B-2026-08-16-13 | cli | low | The ESCAPING-CLOSURE deferral (`E_ESCAPING_CLOSURE_NOT_YET`, epic B-2026-06-22-2) is invisible to `karac check` -- storing a returned capturing closu… | 0431984 |
| B-2026-08-16-14 | other | medium | A 31-bit LCG shifted by 16 gives 15-bit keys, so `% 1000000` never fires — 'shuffled-uniform' benchmark data has 32768 distinct keys | No compiler change — the defect is entirely in kara-katas b… |
| B-2026-08-17-1 | effect | medium | collect_calls_in_expr name-only method fallback scans all method_bodies keys per call site, allocating a format! probe per key — O(call sites x impl… | 3d466756 |
| B-2026-08-17-2 | cli | low | The `map_value_clone_reinsert` advisory lint shares the partial-walk idiom B-2026-08-16-12 removed from its sibling: its traversal carries `_ => {}`… | 281a257 |
| B-2026-08-17-3 | cli | medium | Text-mode `karac check` silently DROPS the whole `TypeCheckResult::warnings` channel: every `type_lint_warning` lint (`deprecated`, `unstable_api`, `… | 281a257 |
| B-2026-08-17-4 | other | low | design.md line 4299 states a rule the spec CONTRADICTS 1,500 lines later: "omitting `allocates` from a public function's declaration is the commitmen… | 2e53d69 |
| B-2026-08-17-5 | typecheck | medium | A user enum variant whose name collides with a scope-0 prelude type cannot be constructed BARE -- correct by design -- but the diagnostic advises `Sp… | ae4a01b5 |
| B-2026-08-17-6 | other | low | `tests/par_atomic_promotion.rs`'s two tests race on the process-global `KARAC_PAR_ATOMIC_PROMOTION` each one set-then-removes: 1/1500 runs at --test-… | 3f1570f0 |
| B-2026-08-17-7 | typecheck | low | DESIGN CALL, not a defect report: should a bare `Span(...)` resolve to the USER's enum variant when the colliding prelude name has no bare-callable f… | a46916b |
| B-2026-08-17-8 | effect | low | `#[no_effect(allocates(Heap))]` is spec vocabulary with NO implementation — design.md prescribes it in three passages (5792's purposes list, the corr… | f0ee064c |
| B-2026-08-17-9 | other | low | `scripts/asan-o0-leg.sh` calls every new -O0 failure a real leak -- "ASAN is reporting on memory the program actually touched" -- but under KARAC_REQ… | 394fc56d |
| B-2026-08-17-10 | typecheck+interp+codegen | medium | INDEXING AN ITERATOR TYPECHECKS, THEN EVERY BACKEND IMPROVISES DIFFERENTLY: `karac check` passes `w.chars()[0]` and `v.iter()[0]`; the interpreter di… | b1871c96 |
| B-2026-08-17-11 | typecheck+cli | medium | E0200'S SUGGESTED REPAIR IS THE UNSAFE ONE: `cannot mix 'u8' and 'i64' .. | BOTH HALVES SHIPPED, in the order the row required: the dir… |
| B-2026-08-17-12 | ownership | high | MULTIPLE READERS IN A `par {}` BLOCK ARE REJECTED -- design.md's edge case #1, written out verbatim and labelled "ALLOWED (multiple readers)", fails… | 29c92dc |
| B-2026-08-17-13 | ownership | high | READING AN UNINITIALIZED BINDING IS NOT REJECTED: `let x: i64; println(f"{x}")` passes `karac check`, the interpreter prints `()` -- the UNIT value i… | d0e204b |
| B-2026-08-17-14 | autopar+codegen | medium | A `#[par_order_free]` COLLECT LOOP REPORTS `fanned_out: true` AND RUNS AT 101% CPU — the B-2026-08-15-23 SIGNATURE AGAIN, on a compiler that already… | 342bc96 |
| B-2026-08-17-15 | typecheck | low | `type_display` renders a generic `Type::Named` with ANGLE brackets -- `Option<i64>`, `Vec<i64>` -- while every sibling arm in the same function uses… | FIXED, and the row's 'cosmetics-plus' framing turned out to… |
| B-2026-08-17-16 | ownership | medium | `let mc = m.as_slice_mut().to_vec();` keeps the mut-Slice borrow of `m` alive for `mc`'s WHOLE lifetime, so any later `ref` read near `m` mis-fires C… | FIXED with a copy-out gate at the binding propagation, plus… |
| B-2026-08-17-17 | ownership | low | `let x: i64; loop { x = 1; break; } println(x);` is REJECTED with use-of-uninitialized, but design.md's DA table lists the shape as OK — an infinite… | FIXED with the break-dominance analysis the row called for |
| B-2026-08-17-18 | codegen | low | Deferred initialization of NON-SCALAR locals (`let s: String; if c { s = .. | f52a7f9 |
| B-2026-08-17-19 | typecheck | medium | Default parameter values are INERT at every call site -- omitting a defaulted argument is an arity error, so the whole feature is unusable | Call-site default-parameter fill, as a pre-resolve AST rewr… |
| B-2026-08-17-20 | typecheck | low | The default-parameter-value const validator rejects FOUR of the forms design.md explicitly lists as allowed, including the spec's own example literal | 3aebeb9b |
| B-2026-08-17-21 | parser+typecheck | medium | `UPPERCASE_BINDING.field.method()` is misresolved as a type path -- "type 'Point' is not callable" -- so a module-level struct constant's field can n… | Fixed in three places, because the row's false positive was… |
| B-2026-08-17-22 | typecheck | medium | A string literal NESTED inside a module-level struct / tuple / array initializer types as `String` instead of `StringSlice`, and since `String` is fo… | 8c2be9b |
| B-2026-08-17-23 | codegen | medium | A destructuring pattern in FUNCTION-parameter position is never lowered by codegen -- `karac check` passes clean, `--interp` runs correctly, and BOTH… | 31044ad |
| B-2026-08-17-24 | codegen | low | The STRUCT-pattern sibling of B-2026-07-14-21's tuple fix: a struct destructuring pattern as an iterator-adaptor closure parameter still bails out of… | 2f15c3f |
| B-2026-08-17-25 | codegen | high | The pipe operator `\|>` produces a SILENT WRONG ANSWER under both compiled backends -- every pipe expression evaluates to 0 -- and a String-typed pipe… | 3e9a6287 |
| B-2026-08-17-26 | typecheck | low | A closure literal as the pipe RHS is rejected -- "right-hand side of pipe must be a function name or function call" -- even though design.md prescrib… | df626614 |
| B-2026-08-17-27 | codegen+interp | high | The `??` operator is wrong on BOTH backends, in three different ways, with `karac check` clean throughout | 3e9a6287 |
| B-2026-08-17-28 | codegen+interp | high | Optional chaining `?.` ICEs the interpreter at one shape and silently returns the wrong answer at another, and is not lowered by codegen at all -- `k… | b76912ac |
| B-2026-08-17-29 | codegen | high | A range bound to a `let` and then iterated compiles to a ZERO-ITERATION loop -- silent wrong answer under both compiled backends, `karac check` clean… | 139562c |
| B-2026-08-17-30 | typecheck+cli | low | E0218 TELLS YOU THE EXACT TEXT TO INSERT AND `karac fix` STILL DECLINES IT: the call-site `mut`-marker diagnostic ends with ``Write `mut <expr>`.`` b… | 94cc17e3 |
| B-2026-08-17-31 | other | medium | 77 of design.md's code-block declarations omit the trailing `;` the grammar REQUIRES -- 51 of 63 bodyless trait declarations, plus 26 of 31 `type` al… | e5699b17 |
| B-2026-08-17-32 | typecheck+interp | medium | Calling a PLAIN REFINEMENT TYPE as a constructor -- `ValidPort(80)` where `type ValidPort = u16 where self >= 1 and self <= 65535;` -- is ACCEPTED by… | feb79b2 |
| B-2026-08-17-33 | typecheck | medium | Derive dependency auto-resolution is implemented for the `Ord` chain but NOT for `Copy` or `Hash`, and design.md states the behaviour unconditionally | Close the derive set over its dependencies at the single po… |
| B-2026-08-17-34 | codegen | medium | `#[derive(Display)]` on an enum works under `--interp` and is REFUSED by both compiled backends when the interpolated expression is an enum-variant P… | Two missing arms in codegen's Display operand recognition,… |
| B-2026-08-17-35 | other | low | Two design.md sections state things the language does not have: § derive(Display) uses an enum-variant syntax that does not parse, and § Subscript Tr… | 56ee5c1 |
| B-2026-08-17-36 | typecheck | medium | `collect()` can only ever produce `Vec[T]` -- every other `FromIterator` target design.md promises is rejected at typecheck | 21c402f |
| B-2026-08-17-37 | cli | medium | The `must_use` lint NEVER RUNS under `karac check` or `karac build` -- it fires only under `karac run`, so the entire surface is invisible to `karac… | 777f4e7 |
| B-2026-08-17-38 | typecheck | medium | `TreeMap[K, V]` -- a standard collection design.md documents 13 times, with its own method table, and prescribes TWICE as the remedy for Map/Set's un… | f49ed9fc |
| B-2026-08-17-39 | typecheck | low | Five iterator-adaptor diagnostics print types with Rust's `{:?}` Debug formatter, leaking internal AST structs into user-facing messages -- and one o… | fbc42214 |
| B-2026-08-17-41 | typecheck | high | A QUALIFIED payload-free variant pattern (`Dir.North`) lowers to a WILDCARD in the exhaustiveness engine, so it silently covers the whole scrutinee:… | One-line lowering fix in `exhaustive::lower_pattern`'s `Bin… |
| B-2026-08-17-42 | codegen | medium | DUPLICATE OF B-2026-08-17-29 -- A RANGE bound to a variable has no codegen | 139562c |
| B-2026-08-17-43 | codegen | high | DUPLICATE OF B-2026-08-17-28 -- `?.` OPTIONAL CHAINING has no codegen and produced a SILENT WRONG ANSWER | b76912ac |
| B-2026-08-17-44 | typecheck | medium | `self` inside `impl Trait for Slice[i64]` types as `Named { name: "Slice", args: [] }` -- the ELEMENT TYPE IS LOST -- so the body can barely do anyth… | 26b320c2 |
| B-2026-08-17-45 | codegen | high | Moving an `Option`-TYPED FIELD out of a matched struct payload DOUBLE FREES under both compiled backends, on a program `karac check` passes clean and… | 0dfeb3d |
| B-2026-08-18-1 | cli | medium | `karac build` renders NO warning-level diagnostic on a successful build -- the suppression is GLOBAL to the build path, not a `must_use` wiring quirk | ea53727a |
| B-2026-08-18-2 | cli | medium | Four sibling lints -- `undocumented_unsafe` / `unsafe_op_in_unsafe_fn`, `missing_must_use`, `missing_track_caller`, `ffi_float_eq` -- are invoked ONL… | f8f10ac |
| B-2026-08-18-3 | interp+codegen | medium | Indexing by a `let`-BOUND range crashes the interpreter with an internal `unreachable!()`, and does not compile under either compiled backend | a587e325 |
| B-2026-08-18-4 | codegen | high | The user-`Drop` spelling of B-2026-08-17-45 still DOUBLE FREES: moving an `Option`-typed field out of a matched struct payload aborts under both comp… | 3966efa |
| B-2026-08-18-5 | codegen | high | EVERY SYNTHESIZED predicate compared an UNSIGNED operand with the SIGNED comparison predicate, so a valid value was rejected at run time on both comp… | dc42b14 |
| B-2026-08-18-6 | typecheck | medium | The `PartialEq` derive's FIELD validator is STRICTER than `Eq`'s, which is backwards: `#[derive(Eq)] struct A { v: Vec[i64] }` is accepted and its `=… | One missing match arm in `type_supports_partial_eq`: `Vec[T… |
| B-2026-08-18-7 | parser | medium | Every `?.` node copies its object's span VERBATIM, so nested chains are indistinguishable to any span-keyed side table | e644132 |
| B-2026-08-18-8 | codegen | medium | A `match` whose SCRUTINEE is itself a `match` yielding a boxed `Option` payload LEAKS the box -- 32 bytes per evaluation, unbounded in a loop | f7621d7 |
| B-2026-08-18-9 | typecheck+codegen | medium | `question_ok_payload_types`' PRODUCER and CONSUMER do not key off the same node, and the `?` operator's span cannot be fixed until they do | 19d7c7f |
| B-2026-08-18-10 | codegen | medium | A `match` whose SCRUTINEE is a nested `match` with a BARE-IDENTIFIER arm leaks the boxed payload -- `match (match i % 3 { 0 => { o } _ => { None } })… | 7f982b0 |
| B-2026-08-18-11 | codegen | high | An OWNED `self` receiver on a builtin-container impl head is not lowered, and the two shapes measured fail in the two worst ways: `for x in self` ove… | 4a484fb0 |
| B-2026-08-18-12 | codegen | medium | `self.len()` inside an `impl Trait for Map[K, V]` or `impl Trait for Set[T]` body has NO codegen dispatcher -- "no handler for method 'len' on non-id… | 925b2db0 |
| B-2026-08-18-13 | typecheck | medium | A method declared by `impl Trait for Array[i64, 3]` CANNOT BE CALLED: the call site reports "no method 'g' on type 'Array'" even though the impl bloc… | b9d50b4d |
| B-2026-08-18-14 | codegen | medium | A RANGE SUBSCRIPT used directly as a METHOD RECEIVER -- `v[0..3].first_or(-1)` -- has no codegen: "no handler for expression kind Range" | 6a80d37 |
| B-2026-08-18-15 | codegen | high | Binding a container element's BOXED `Option` field double-frees the payload envelope: `let c = v[i].opt;` aborts with `free(): double free detected i… | Narrowed the let-site box registration: an initializer that… |
| B-2026-08-18-16 | cli | medium | `missing_must_use` and `missing_track_caller` render BAKED-STDLIB findings against the USER's filename with the stdlib item's own span, producing dia… | bbbea03 |
| B-2026-08-18-17 | cli | low | The `must_use` lint's JSON entry reuses diagnostic code `E0250`, which is ALREADY the typechecker's `ModuleBindingEffectfulInit` -- so a `must_use` e… | f32eae6d |
| B-2026-08-18-18 | typecheck | low | `collect()` reaches a non-`Vec` `FromIterator` target only through an ANNOTATED `let` | 8eae11c |
| B-2026-08-18-19 | cli | medium | PROJECT-mode `karac build` still renders no warning-level diagnostic | 90112fc6 |
| B-2026-08-18-20 | cli | low | `karac run` is inconsistent with ITSELF about warnings: it renders `must_use` but not `deprecated` | d2ed766c |
| B-2026-08-18-21 | parser+codegen | medium | `Index` and `MethodCall` nodes still copy their object's span, so a whole postfix CHAIN collapses onto its innermost receiver's SpanKey and the last… | 4a22ddc4 |
| B-2026-08-18-22 | codegen | medium | A STRING range subscript used as a METHOD RECEIVER reaches codegen only for `to_string` / `clone`: `s[0..5].len()` fails the build with "indexed-rece… | b3d0c97 |
| B-2026-08-18-23 | cli | medium | `cargo clippy --all --all-targets -- -D warnings` is RED on main: `collect_warning_diagnostics_json` and `render_text_warning_diagnostics` in src/cli… | 81b1fe4f |
| B-2026-08-18-24 | parser+codegen | medium | `MethodCall` nodes still copy their object's span, so a method CHAIN collapses onto its innermost receiver's SpanKey | 849096a2 |
| B-2026-08-18-25 | typecheck | low | The `deprecated` diagnostic's MESSAGE TEXT begins with its own severity prefix, so every renderer that adds one prints it twice: "warning[deprecated]… | 0ad5c51b |
| B-2026-08-18-26 | codegen | high | A READ METHOD called directly on a returned `Set`/`Map` temp reads GARBAGE under `karac build` while `--interp` is correct: `mk_set().len()` gives 0… | c347908 |
| B-2026-08-18-27 | typecheck | low | `collect()` in ARGUMENT position still fixes the chain to `Vec`: `f(<chain>.collect())` against a non-`Vec` parameter reports "expected 'Set[i64]', f… | b55a046 |
| B-2026-08-18-28 | cli | low | NO lint diagnostic code is registered in `karac explain`'s CODE_TABLE, so `karac explain E0278` (must_use) and `karac explain E0259` (the four compil… | 4d071c5 |
| B-2026-08-18-29 | other | low | 28 of design.md's code blocks still fail to parse on STATEMENT terminators -- a `let` whose `;` is missing (13 blocks), bare expression statements, `… | 56f6bfa |
| B-2026-08-18-31 | parser+codegen | low | `FieldAccess` nodes still copy their object's span -- a FIFTH postfix arm with the identical defect the four-row family (`?.`, `?`, `Index`, `MethodC… | 399c7a68 |
| B-2026-08-18-32 | ownership+codegen+interp | low | THE OWNERSHIP CHECKER AND BOTH BACKENDS DISAGREE ABOUT WHETHER `String + ` CONSUMES ITS LEFT OPERAND | 982ce8c |
| B-2026-08-18-33 | parser | low | SEVEN parser arms still copy their LHS's span -- `Cast`, `TupleIndex`, `Call`, `Path`, `Pipe`, `NilCoalesce`, `Range` -- and the postfix-span family… | e654c22f |
| B-2026-08-18-34 | interp+codegen | high | `map.entry(k).or_insert(default).push(v)` FAILS ON ALL THREE BACKENDS when the map is a struct FIELD, while `karac check` accepts it: interpreter say… | 4e49246 |
| B-2026-08-18-35 | codegen | high | Multi-shot host producers (`every`, `animation_frames`) never unref their node timers, so a wasm program HANGS after completing its work — and the fo… | 502d6279 |
| B-2026-08-18-36 | codegen | medium | A NESTED INDEXED READ through a struct field -- `h.buckets[3][1]` where `buckets: Map[i64, Vec[i64]]` -- fails codegen with "nested indexed read requ… | ffb3f76 |
| B-2026-08-18-37 | interp | medium | A NESTED SUBSCRIPT whose INNER index FAULTS panicked the interpreter with an internal `unreachable!()` -- `m[missing_key][0]` printed "internal error… | c80c62f |
| B-2026-08-18-38 | other | medium | The ENTIRE wasm E2E surface in `tests/cli.rs` passed VACUOUSLY on any machine whose default rustup toolchain is not the pinned one: 36 tests took the… | c3f5f3f |
| B-2026-08-18-39 | autopar+codegen | high | `?.` ON TWO LET-BOUND CALL RESULTS MISCOMPILES UNDER AUTO-PAR, RETURNING MEMORY THAT VARIES BETWEEN RUNS | f3fb56b |
| B-2026-08-18-40 | typecheck+codegen | low | A `#[gpu]` KERNEL BODY COULD NOT LOOP, BIND A LOCAL, OR BRANCH ON A VALUE — it had to be a SINGLE EXPRESSION | 7e373aa9 |
| B-2026-08-18-41 | parser | low | TWO `effect resource` declaration forms design.md specifies normatively do not parse: a GENERIC trait bound (`effect resource C: Provider[Request];`… | 641d6df |
| B-2026-08-18-42 | other | low | EIGHT occurrences across SEVEN lines in design.md's kara blocks use Rust MACRO syntax (`name!(...)`), which Kara does not have -- the parser rejects… | 9b6e1b1 |
| B-2026-08-18-43 | parser | medium | `Expected Semicolon` -- the most common parse error in the whole corpus -- carries NO machine-applicable `replacement`, so `karac fix` reports it and… | ce0ae03 |
| B-2026-08-18-44 | ownership | medium | A SLICE PATTERN'S BINDINGS were dropped by `cfg::pattern_bindings`, so the ownership CFG recorded no `Define` for them and never called `note_local_i… | b55a046 |
| B-2026-08-18-45 | ownership | medium | EVERY `collect()` into a non-`Vec` target emitted a `perf[rc-fallback]` note naming a SYNTHESIZED binding the user never wrote -- "RC fallback insert… | b55a046 |
| B-2026-08-18-46 | parser | medium | `karac fix` CORRUPTED a Rust macro call: `println!("hi");` was rewritten to `printlnnot ("hi");` | ce0ae03 |
| B-2026-08-18-47 | resolver+cli | medium | `karac fix` AUTO-APPLIES a `did you mean` rename to a SEMANTICALLY UNRELATED function: an undefined `format("{}", 1)` is rewritten to `forget("{}", 1… | a1a0ca0 |
| B-2026-08-18-48 | codegen | medium | A HEAP-BOXED enum payload that is MOVED into a by-value call is freed by nobody | 67e8feb |
| B-2026-08-18-49 | typecheck+codegen | low | STATEMENT-FORM `if` WAS NOT SUPPORTED AT ALL in a `#[gpu]` kernel body — not merely `if` branches containing locals | df6538ed |
| B-2026-08-18-50 | typecheck | low | A PARAMETERIZED RESOURCE'S PARTITION KEY IS NEVER TYPECHECKED against its declared type | ce92539 |
| B-2026-08-18-51 | other | medium | `tests/gpu_e2e.rs` FAILS THE WHOLE SUITE on any machine that has not built the OPTIONAL `libkarac_runtime_gpu.a` — six tests panic with a link error… | 066b695 |
| B-2026-08-19-1 | codegen+runtime | high | A `#[gpu]` kernel SILENTLY DROPS Kara's checked-integer-arithmetic semantics: the same expression that TRAPS under `--interp` (and on CPU) WRAPS on t… | 73aeb057 |
| B-2026-08-19-2 | interp | low | `karac run --interp prog \| head` dumps a Rust panic + backtrace on SIGPIPE; the JIT and the AOT binary exit silently | 17af4e8 |
| B-2026-08-19-3 | parser+codegen | low | MULTI-BOUND `effect resource` declarations do not parse: `effect resource UserDB: DatabaseProvider + HealthCheckable;` -> "Expected Semicolon, found… | c151f7b |
| B-2026-08-19-4 | typecheck | medium | `with_provider[R](p, ...)` NEVER CHECKS that `p` implements the resource's declared provider trait | f472db3 |
| B-2026-08-19-5 | other | low | The codegen E2E harness (`tests/codegen.rs::run_program_capturing_inner`) runs resolve + typecheck but NOT the effect checker, so a test can pin beha… | cde0a48 |
| B-2026-08-19-6 | typecheck+interp+codegen | medium | `i128` / `u128` are CARRIED AS 64-BIT and wrap there silently: `let a: i128 = 9223372036854775807; a.wrapping_add(1)` prints `-9223372036854775808` i… | b62cd33 |
| B-2026-08-19-7 | runtime+cli | low | `karac run` (JIT) IGNORES A CLOSED READER ENTIRELY: with `\| head -2` it runs the whole program to completion, discarding every failed write, then exi… | 15a11aa |
| B-2026-08-19-8 | lexer+typecheck+interp+codegen | medium | IMPLEMENT 128-bit integers for real: `i128` / `u128` are specified normatively in design.md as v1 primitives (all four overflow method families, the… | 7561b5d |
| B-2026-08-19-9 | other | medium | `tests/gpu_e2e.rs` PINNED `KARAC_GPU_BACKEND=cpu`, so all fourteen execution fixtures SKIPPED ON macOS — the project's primary dev machine — while re… | 7c862b7d |
| B-2026-08-19-10 | typecheck+codegen+runtime | medium | A GPU REDUCTION (N inputs -> 1 result) CANNOT BE WRITTEN AT ALL | 63870ee0 |
| B-2026-08-19-11 | typecheck | low | design.md's `with_provider` signature demanded `Send + Sync`, a guarantee Kara had already DECIDED NOT TO SHIP under that name -- four stale spec lin… | f4340ba |
| B-2026-08-19-13 | typecheck+codegen+runtime | medium | GPU reductions COMPLETE, no gaps: the SAME ELEVEN ops over `Vec[f32]`, `Vec[i32]` and `Vec[u32]` (sum / prod / min / max / mean / dot / argmin / argm… | d3cadc9f |
| B-2026-08-19-14 | typecheck | high | NO `shared struct` OR `par struct` COULD SATISFY ANY TRAIT BOUND ANYWHERE -- `impl_table_key` had no `Type::Shared` arm, so `type_satisfies_bound` an… | f4340ba |
| B-2026-08-19-15 | typecheck | medium | the `providers { R => p } in { . | f4340ba |
| B-2026-08-19-16 | interp | high | `First.A` EVALUATED TO A `Second` VALUE under `karac run` whenever two enums shared a unit-variant name -- user enums registered their unit variants… | c88b51b |
| B-2026-08-19-17 | typecheck+interp | medium | A BARE AMBIGUOUS UNIT VARIANT PICKED A DIFFERENT ENUM IN EACH BACKEND -- `karac build` printed First's answer and `karac run` printed Second's, with… | a94038e |
| B-2026-08-19-18 | cli+codegen | high | `karac run` FAILED on every GPU reduction program while `karac build` answered correctly — `JIT session error: Symbols not found: [ karac_runtime_gpu… | ba484b53 |
| B-2026-08-19-19 | codegen | medium | A 128-bit scalar cannot be carried in an ENUM PAYLOAD: an Option/Result payload word is 64 bits and `i128`/`u128` needs two, which the pack/unpack ma… | 6716341 |
| B-2026-08-19-20 | interp+codegen | medium | `Stats.*` empty-input refusals presented DIFFERENTLY on the two legs -- the interpreter raw-`panic!`ed (Rust backtrace, exit 101) where `karac build`… | 9298b98 |
| B-2026-08-19-21 | typecheck | medium | Set/SortedSet `contains` and `remove`, and both `binary_search` arms, REJECTED a borrowed needle — `s.contains(w)` for a `w: ref String` failed with… | Named the rule once as `peel_probe_ref` in `src/typechecker… |
| B-2026-08-19-23 | lexer+parser+interp | medium | The UPPER HALF of `u128` (values above `i128::MAX`) is unreachable end-to-end: the interpreter's value carrier is a signed `i128`, so such a value is… | d0b8bdf |
| B-2026-08-19-24 | typecheck | medium | TYPE-DIRECTED BARE UNIT-VARIANT RESOLUTION: when the context names an enum, a bare variant name now means THAT enum's variant (`let x: Second = A;`,… | ff300ce |
| B-2026-08-19-25 | interp+codegen | medium | `Stats.sum` / `Stats.prod` over i64 raw-`panic!`ed on integer overflow under `karac run` (Rust backtrace, exit 101) where `karac build` trapped clean… | 1833be5 |
| B-2026-08-19-26 | codegen | high | SILENT MISCOMPILE on plain 64-bit code: a shift took its WIDTH and its SIGNEDNESS from the shift AMOUNT rather than from the value | bb3eeb7 |
| B-2026-08-19-27 | interp | medium | The interpreter renders an UNSIGNED value with its SIGNED reading whenever it is nested in a container or an enum payload: `println(o)` on an `Option… | 4c8a6683 |
| B-2026-08-19-28 | codegen | medium | `println(v)` on a `Vec[Option[T]]` PANICS the compiler — `emit_display_fn_for_type: type_name 'T' not yet supported` — unless an earlier bare `Option… | c72187e |
| B-2026-08-19-29 | parser | low | `usize` was not an integer suffix ANYWHERE — absent from the lexer's suffix table and from `IntSuffix` itself — so `42usize` lexed as `42` plus a str… | c7cf8c0 |
| B-2026-08-19-30 | codegen | medium | A BARE `println(e)` on a user-defined GENERIC enum value PANICS the compiler — `emit_display_fn_for_type: type_name 'T' not yet supported` | db463e5 |
| B-2026-08-20-1 | parser | low | An upper-half unsigned literal could not be a MATCH PATTERN for any width — `parser/exprs.rs` has an unsigned band that wraps the magnitude onto the… | 08ab8a7 |
| B-2026-08-20-2 | codegen | high | GPU float NaN guards written as `!(x == x)` are DELETED on Metal: MSL compiles with fast-math by default, which lets the compiler assume no NaN exist… | 9aed2e2c |
| B-2026-08-20-3 | autopar | high | AUTO-PAR (on by default) turns a `parallel_reduction` whose body does `chars().collect()` into a 2.55x PESSIMIZATION — 326 ms sequential becomes 831… | df7bef9 |
| B-2026-08-20-4 | parser+typecheck+codegen+interp | medium | `LiteralPattern::Integer` holds an `i64`, so NO 128-bit literal can be a match pattern — `match n { 170141183460469231731687303715884105727i128 => . | b4072bc |
| B-2026-08-20-5 | codegen | high | `karac run` (the DEFAULT executor) DIED ON ANY SIGNED 128-BIT MULTIPLY on macOS — `JIT session error: Symbols not found: [ ___muloti4 ]`, so nothing… | 02f1b87d |
| B-2026-08-20-7 | parser | high | A NEGATIVE integer literal cannot be a match pattern at ANY width: `match n { -5 => . | 3cf5ff9 |
| B-2026-08-20-6 | typecheck | medium | A `u64` / `usize` match whose arms COVER THE WHOLE DOMAIN by two ranges is wrongly rejected as non-exhaustive: `match n { 0u64..=9223372036854775807u… | 6b4d2b2 |
| B-2026-08-20-8 | autopar | medium | `karac build --concurrency-report` prints the ANALYSIS label with no lowering verdict, so it claims `parallel_reduction` for a loop its own sibling q… | c0874f8 |
| B-2026-08-20-9 | runtime+autopar | medium | Auto-par worker threads contend on ONE glibc malloc arena: any fan-out body that allocates per iteration pays a shared lock the sequential build neve… | FIXED IN THE RUNTIME, NOT THE COST MODEL |
| B-2026-08-20-10 | typecheck | high | LITERAL PATTERNS ARE NEVER TYPECHECKED — `PatternKind::Literal` is an explicit "deferred" arm | b585059 |
| B-2026-08-20-11 | codegen | high | SILENT RUN-VS-BUILD MISCOMPILE: codegen does not compare an enum payload's LITERAL pattern at all — it matches on the tag alone | e66b71d |
| B-2026-08-20-12 | typecheck | low | A FLOAT literal pattern makes the following `_` arm report `warning[unreachable_arm]`: `match f { 1.5 => .., _ => . | `src/exhaustive.rs` lowered every float literal pattern to… |
| B-2026-08-20-13 | typecheck | medium | A FLOAT literal is accepted at an INTEGER-annotated binding and the annotation is simply ignored: `let n: i64 = 1.5; println(n)` compiles and prints… | Root cause is in `src/typechecker/types.rs`: the numeric-co… |
| B-2026-08-20-14 | autopar | high | A `parallel_reduction` is REPORTED but never dispatched when the loop's induction variable is a REUSED binding (`i = 0` on an `i` declared earlier) r… | f620315 |
| B-2026-08-20-15 | autopar+codegen | high | AUTO-PAR MISCOMPILE: after a fanned-out `while k < end { ...; k = k + 1; }` reduction, the parent's counter is left at its INIT value instead of `end… | d361c11 |
| B-2026-08-20-16 | cli | high | `karac check <file>` and `karac run <file>` SILENTLY TRUNCATE a package member to a single file, producing both FALSE REJECTIONS and FALSE ACCEPTANCE… | b8e8a70 |
| B-2026-08-20-17 | resolver | medium | MODULE-BINDING imports are unimplemented on every surface: `import db.connection;` and `import db;` bind nothing | f725b44 |
| B-2026-08-20-18 | parser | medium | WILDCARD and NESTED-GROUP imports do not lex or parse, though design.md says both ship in v1 | ae0b21b |
| B-2026-08-20-19 | cli | low | E0223 (circular module dependency) omits the FILES on the cycle from its TEXT render, though `--output=json` has always carried `cycle_files` | 1f3d565 |
| B-2026-08-20-20 | cli+resolver | medium | `karac run <file>` rejects an ALIASED import of a sibling module -- `import db.connection.Pool as P;` then `P { size: 2 }` fails with `error[resolve]… | 4f37cc1 |
| B-2026-08-20-21 | interp | medium | `Tensor[f32].matmul` accumulates in f64 under `karac run --interp` and in f32 under `karac build`, so a long enough contraction gives DIFFERENT ANSWE… | 2f94bb1b |
| B-2026-08-20-22 | interp | medium | `Tensor.from` under a `Tensor[f32, ...]` annotation narrows a bare float LITERAL but not a NEGATED one or a computed one, so `Tensor.from([-0.1])` ho… | 209a6607 |
| B-2026-08-20-23 | ownership | low | every whole-buffer `gpu.*` reduction CONSUMES its buffer, so calling two of them on the same `Vec` warns `value moved here, used again here` -- but t… | 06f0cca |
| B-2026-08-20-24 | cli | high | Two modules of one package may not declare the same top-level name: `karac check` ACCEPTS the program and BOTH execution paths reject it | 36efad6 |
| B-2026-08-20-25 | other | low | design.md's `E0226 ConflictingPlatformModule` (§ Platform-specific modules) names a condition that CANNOT ARISE, so the fix is to strike it rather th… | c0e217e |
| B-2026-08-20-26 | codegen | medium | A TEST BINARY MUTATING THE PROCESS ENV, not a miscompile: `test_ir_shared_variant_name_resolves_to_scrutinee_enum_deterministically` failed ~6 of 8 F… | 3220697 |
| B-2026-08-20-27 | interp+codegen | high | an INTEGER `Tensor.matmul` never overflow-checks: `karac build` silently WRAPS and `karac run --interp` returns a value outside the element type -- w… | beb4c419 |
| B-2026-08-20-28 | resolver | high | A module binding's MEMBER ACCESS claims the bare member name in the importing module, so anything else answering to that name either collides with it… | c50c3fa |
| B-2026-08-20-29 | cli | low | THE NATIVE OS PLATFORMS ARE NOT SELECTABLE, so a `_macos` / `_windows` / `_linux` module can only be checked or built on that host, and no command ve… | 5079dbc |
| B-2026-08-20-30 | cli | low | `karac explain E0223` and `E0227` REFUSE two codes users actually see, with a message that claims their family IS covered: "`karac explain` covers th… | b01cd21 |
| B-2026-08-20-31 | cli | low | THE EFFECT (`E04xx`), OWNERSHIP (`E05xx`) AND PROVIDER-ESCAPE (`E0600`) BANDS ARE UNCATALOGUED, so `karac explain E0400` / `E0500` / `E0600` refuse 2… | fb1f3ab |
| B-2026-08-20-32 | interp | high | The TREE-WALK INTERPRETER's `.clone()` on a NESTED `Vec[Vec[T]]` is SHALLOW -- the inner Vecs are shared, so mutating the clone mutates the original;… | c6641a8 |
| B-2026-08-20-33 | codegen | medium | A THREE-level index assignment (`a[i][j][k] = v` on a plain local `Vec[Vec[Vec[T]]]`) is rejected by codegen with `Index assignment target must be a… | 5b6c33b |
| B-2026-08-20-34 | runtime | medium | STACK EXHAUSTION in an AOT binary is a BARE SIGSEGV with EMPTY stderr -- no message, no hint, exit 139; Rust prints `thread 'main' has overflowed its… | 1e91166 |
| B-2026-08-20-35 | codegen | high | A NESTED INDEX WHOSE OUTER CONTAINER IS A MAP: the READ (`m[k][i]` on a `Map[K, Vec[V]]`) is rejected `outer is not a Vec/Slice/Array`, and the WRITE… | 16354f1 |
| B-2026-08-20-36 | cli | medium | EIGHT of the 28 registered lints have NO EMIT SITE anywhere in `src/` outside the registry, so they can never fire -- and all eight are registered `d… | 6c07671 |
| B-2026-08-20-37 | lexer | medium | BYTE-STRING literals `b"..."` are lexed as RESERVED, though design.md § Byte and Byte-String Literals specifies them as shipped with a type rule, thr… | 550bb1f |
| B-2026-08-20-38 | typecheck | medium | A MUTABLE SUB-RANGE slice cannot be formed at a call site -- design.md § Slices' own second example is rejected | 1464a6f |
| B-2026-08-20-39 | typecheck | medium | `v.last(n)` -- the END-RELATIVE ACCESS design.md prescribes as THE replacement for negative indexing -- takes no argument: `v.last(1)` is rejected wi… | 537a237 |
| B-2026-08-20-40 | codegen | medium | `s.bytes()[i]` -- the byte-access form design.md documents for protocol and binary-format parsing -- is check-clean and interp-correct but has NO COD… | bc2a768 |
| B-2026-08-20-41 | typecheck+interp+codegen | medium | `String.normalize` and the `NFC` constant DO NOT EXIST, so the remedy design.md gives for its own Unicode-equality hazard is unwritable | 410d6f5 |
| B-2026-08-21-2 | cli | medium | SIX OF THE EIGHT REGISTERED-BUT-UNWIRED LINTS ARE NOW WIRED (`redundant_suffix`, `module_mut_binding`, `mutual_recursion_note`, `pure_loop_in_par`, `… | 17ff5c0 |
| B-2026-08-21-4 | codegen | high | A free function DECLARED to return `Slice[T]` hands back a GARBAGE header in codegen while `--interp` is correct, and MOST uses of it are SILENT | b538966 |
| B-2026-08-21-5 | codegen | medium | `compile_cstr_method` PANICS on a `ref CStr` PARAMETER receiver instead of emitting a diagnostic: `fn look(c: ref CStr) -> i64 { c.len() }` aborts ka… | 5b895aa |
| B-2026-08-21-6 | codegen+interp | medium | `Map`/`Set` DO NOT USE THE SPEC'S DEFAULT HASHER: design.md mandates `SipHash13BuildHasher` seeded from a per-process random source, codegen emits Fx… | 8c3c8d6 |
| B-2026-08-21-7 | ownership | medium | A call-site `mut` MARKER BYPASSES THE `let mut` REQUIREMENT, so a binding declared without `mut` can be mutated through any mutating parameter while… | cefc213 |
| B-2026-08-21-8 | interp | medium | THE INTERPRETER'S `Map`/`Set` ARE ASSOCIATION LISTS, so every `get`/`insert`/`contains` is a LINEAR SCAN and any map-keyed program is QUADRATIC under… | 04ed765 |
| B-2026-08-21-9 | parser | medium | design.md WRITES SYNTAX THE PARSER HAS NO PRODUCTION FOR, in two places syntax.md also disagrees with: the inline associated-type binding `Trait[Asso… | 18e39e0e |
| B-2026-08-21-10 | typecheck | medium | FOUR STDLIB ENTRY POINTS design.md DOCUMENTS DO NOT EXIST -- `Vec.from_fn` (in the Vec method table, with a worked example), `Vec.is_sorted` (used in… | 5fee766 |
| B-2026-08-21-11 | other | low | design.md EXAMPLES ARE WRITTEN IN SYNTAX design.md AND syntax.md THEMSELVES FORBID: `mu.lock()` and `let group = ...` use hard keywords as identifier… | FIXED in design.md |
| B-2026-08-21-12 | parser+typecheck | medium | FOUR FORMS syntax.md OR design.md DEFINES THAT THE FRONT END REJECTS -- `let...else` (a named production, LET_ELSE_STATEMENT), struct functional upda… | e386fc4 |
| B-2026-08-21-13 | codegen | high | EVERY `gpu.*` REDUCTION SILENTLY DROPPED every element past 4,194,240 | 42fee64f |
| B-2026-08-21-14 | typecheck+cli | medium | THE NEWLY-WIRED `redundant_suffix` LINT SHIPS WARN-BY-DEFAULT WITH NO MACHINE-APPLICABLE FIX, so `karac fix` reports `no fixable diagnostics` on a fi… | c6ac12f |
| B-2026-08-21-15 | cli | medium | The MULTI-FILE build gate failed the build on ANY effect-checker entry, including NOTE-severity ones: `run_multi_file_codegen` tested `!e.errors.is_e… | 2a12881 |
| B-2026-08-21-16 | resolver | high | A SPEC-LEGAL PROGRAM FAILED TO COMPILE: `layout_unassigned_fields` was emitted as a hard `ResolveError`, so any `layout` block not naming every field… | fa8a940 |
| B-2026-08-21-17 | lexer | medium | THE SELF-HOSTED KARA LEXER CANNOT LEX `b"..."`, so it now disagrees with the Rust lexer on any input containing one: `selfhost/src/lexer.kara` still… | 3e84931 |
| B-2026-08-21-18 | codegen+typecheck | low | STRUCT FUNCTIONAL UPDATE `P { x: 1, ..base }` IS STILL UNIMPLEMENTED -- it now says so clearly instead of dropping the base, but the form syntax.md's… | 1d634c6 |
| B-2026-08-21-19 | codegen | high | RUN/BUILD DIVERGENCE from two independently-green commits meeting: `let a = b"abc"; a.is_sorted();` printed `true` under `--interp` and FAILED TO COM… | ca74f17 |
| B-2026-08-21-20 | typecheck | high | `main` IS RED: `selfhost_typechecker_matches_rust_typechecker` fails at 5fee766 on a clean checkout, independent of any local work | dda94ff |
| B-2026-08-21-21 | codegen | high | CODEGEN SILENTLY DROPS `requires` / `ensures` CONTRACTS ON A GENERIC FUNCTION -- the monomorphic sibling of the same contract fires on all three surf… | bc18fcb |
| B-2026-08-21-22 | codegen | high | `Vec.filled(n, f"...")` DOUBLE-FREES UNDER BOTH COMPILED BACKENDS -- the f-string accumulator is moved into the buffer and ALSO freed at scope exit;… | bcdb9cb |
| B-2026-08-21-23 | codegen | high | A FIXED ARRAY PASSED TO A **METHOD**'s `ref Slice[T]` PARAMETER READS A GARBAGE LENGTH -- `bytes.len()` answers 3 under the interpreter, -1 under AOT… | 08f57a7 |
| B-2026-08-21-24 | codegen | medium | AN ARRAY **LITERAL** TEMPORARY IN A `ref Slice[T]` PARAMETER FAILS LLVM MODULE VERIFICATION -- `f([1u8, 2u8, 3u8])` builds nothing while the by-value… | c5e549fc |
| B-2026-08-21-25 | codegen | medium | A METHOD CALL ON A FIXED-ARRAY **TEMPORARY** HAS NO CODEGEN LOWERING -- `n.to_ne_bytes().len()` build-fails with the dispatcher's own "this is a code… | a496b8df |
| B-2026-08-21-26 | typecheck | medium | `TryFrom[intN]` FOR A C-LIKE `#[repr(intN)]` ENUM DOES NOT EXIST -- design.md § Enum Discriminant Runtime Surface commits to auto-generating it and s… | 5e323df1 |
| B-2026-08-21-27 | typecheck | medium | self-host typechecker diverges from Rust on a struct-update expression missing a field | dda94ff |
| B-2026-08-21-28 | other | medium | THE design.md CONFORMANCE SUITE IS BLIND TO ITS HIGHEST-YIELD CLASS: 21 of its 71 baseline blocks are SIGNATURE CATALOGUES that never compile, so the… | c320ae5 |
| B-2026-08-21-29 | typecheck | medium | THREE DOCUMENTED SURFACES ARE NOT REACHABLE AS WRITTEN: every spelling of channel construction design.md uses (`Channel.new[T]()`, `Channel.bounded`)… | 02dce7a |
| B-2026-08-21-30 | codegen | high | A `mut Slice[T]` **METHOD** PARAMETER FED A `mut`-MARKED ARRAY LOSES THE WRITE -- the callee's `b[0] = 9u8` never reaches the caller's array under JI… | 563fdfb |
| B-2026-08-21-31 | resolver+typecheck | medium | `isize` IS NOT A TYPE -- design.md names it a v1 numeric primitive in four normative passages and writes it into six signatures, and three of the com… | 50deb0a |
| B-2026-08-21-32 | resolver+effect | medium | EFFECT RESOURCES CANNOT BE ROOTED AT A VALUE, so design.md's whole channel effect model is unwritable: `with sends(tx)` on a channel PARAMETER is `'t… | 6efbaa0 |
| B-2026-08-21-33 | other | low | the seven self-host oracles ignore `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` on the one link failure that still soft-skips, and their skip comment wrongly cl… | 910a2af9 |
| B-2026-08-21-38 | interp+codegen | high | THE INTERPRETER LOSES A `mut ref` SCALAR WRITE THROUGH A **METHOD** ARGUMENT -- `h.bump(mut n)` leaves `n` at its old value under `--interp` while JI… | c2d4c83 |
| B-2026-08-21-39 | codegen | medium | AN **ASSOCIATED FUNCTION** (no `self`) FAILS MODULE VERIFICATION ON AN ARRAY ARGUMENT TO A SLICE PARAMETER -- `H.f(mut bm)` passes the raw `[3 x i8]`… | c5e549fc |
| B-2026-08-21-40 | codegen | medium | A `mut ref String` ARGUMENT ON A **FIELD** PLACE LEAKS THE FIELD'S ORIGINAL BUFFER WHEN THE CALLEE REASSIGNS IT -- `free_app(mut b.name)` with a body… | e6d0dbe |
| B-2026-08-21-41 | codegen | high | ITERATING A FIXED-ARRAY **TEMPORARY** SILENTLY YIELDS ZERO ELEMENTS under `karac build` -- `n.to_ne_bytes().iter().count()` is 0 (interp: 2) and `for… | 521ca182 |
| B-2026-08-21-42 | codegen | medium | NO `self.<method>()` DISPATCHER ON AN `impl .. | 6cfd52f5 |
| B-2026-08-21-43 | codegen | low | A USER IMPL ON A **NON-SCALAR** `Array` HEAD IS UNCALLABLE ON A TEMPORARY -- `mk().tag()` for `impl Tag for Array[String, 2]` still loud-fails after… | 9d53994 |
| B-2026-08-21-44 | codegen | low | `as_slice()` ON A FIXED-ARRAY **TEMPORARY** HAS NO CODEGEN LOWERING -- `n.to_ne_bytes().as_slice().len()` fails dispatch while `--interp` answers, th… | 9b079fb0 |
| B-2026-08-21-45 | typecheck+codegen | low | SIX SHIPPED USER-FACING DIAGNOSTICS (eight messages) CARRIED RUNS OF 18-34 SPACES mid-sentence, because rustfmt INTERMITTENTLY rejoins a `\`-continue… | ae306218 |
| B-2026-08-21-46 | codegen | low | `enumerate` OVER A BRACKETED LITERAL bailed to the adaptor backstop while `--interp` answered -- and this row's premise was WRONG: the literal is not… | e92355bf |
| B-2026-08-21-48 | codegen | medium | AN **UN-ANNOTATED** SLICE BINDING CANNOT CALL A USER METHOD ON THE `Slice` HEAD -- `let s = v[0..2]; s.f()` fails with "no handler for method 'f' on… | e66ce9f |
| B-2026-08-21-49 | codegen | high | EVERY REDUCE/SCAN SHADER WROTE ITS PER-WORKGROUP SLOT UNBOUNDED, so an overshoot workgroup stored past a tightly sized output buffer -- Metal CLAMPS… | FIXED by bounding the write in the SHADER, which is the onl… |
| B-2026-08-21-50 | codegen | high | A C-LIKE ENUM BOUND OUT OF `Ok(...)` MATCHES THE WRONG VARIANT UNDER CODEGEN -- `Ok(UsbClass.Hid)` selects the FIRST variant, and passing the binding… | c00a1308 |
| B-2026-08-21-51 | typecheck | medium | A GENERIC ENUM'S STRUCT-SHAPED VARIANT DOES NOT BIND ITS TYPE PARAMETER: constructing one infers the BARE head (`MyErr`, not `MyErr[u8]`) and a patte… | c9f5115d |
| B-2026-08-22-1 | codegen | high | `MemoryOrdering` REACHED CODEGEN WITH NO ENUM LAYOUT, so every match arm compiled to a BINDING pattern instead of a tag test and the first arm always… | 8fb7cec7 |
| B-2026-08-21-47 | codegen | medium | A `shared struct` FIELD SOURCE LEAKS A DIFFERENT BUFFER THAN THE PLAIN-STRUCT ONE DID -- `last = s.name` off a `shared struct` leaks the POST-append… | e6d0dbe |
| B-2026-08-22-2 | interp | high | A BARE UNIT-VARIANT PATTERN OF A BAKED-STDLIB ENUM MAKES THE **INTERPRETER** MATCH THE FIRST ARM ALWAYS -- `match e { NotFound => .., PermissionDenie… | 28bd239 |
| B-2026-08-22-3 | typecheck | high | AN ASSOCIATED-TYPE-EQUALITY BOUND IS NEVER DISCHARGED AT CALL SITES -- `where I.Item = i64` is validated at the DECLARATION and then ignored, so a ca… | 69be64c |
| B-2026-08-22-4 | parser | medium | AN INLINE ASSOCIATED-TYPE BINDING IS STILL REJECTED IN `impl Trait` TYPE POSITION -- `fn keys(ref self) -> impl Iterator[Item = ref K]` (design.md's… | 02e73282 |
| B-2026-08-22-5 | parser | medium | AN EFFECT CLAUSE ON AN IMPL HEADER IS CORRECTLY ABSENT FROM THE GRAMMAR -- but the parser rejected it with a generic `Expected LeftBrace, found With`… | 70168ff |
| B-2026-08-22-6 | typecheck | low | The `Hasher` and `BuildHasher` TRAITS are not baked, so a user cannot write their own hasher: `Map[K, V, H]` accepts only the two compiler-known sele… | 840f95ef |
| B-2026-08-22-7 | codegen | low | `f16_software_emulated` IS THE ONE STARTER-SET LINT WHOSE TRIGGER DEPENDS ON THE TARGET -- and the blocker is NOT the placement this row was filed on… | 289bc8a3 |
| B-2026-08-22-8 | cli | low | `implicit_clone` HAS NO TRIGGER IN THE SPEC AND CANNOT BE DE-REGISTERED WITHOUT A design.md EDIT -- the one starter-set lint whose resolution is a SP… | 85ce532 |
| B-2026-08-22-9 | typecheck | high | NO WHERE-CLAUSE BOUND OF ANY CLASS WAS DISCHARGED ON A METHOD CALL -- the method path passed `None` for the callee's where clause, so `h.take(NotMark… | 69be64c |
| B-2026-08-22-10 | typecheck | high | NO GENERIC BOUND OF ANY CLASS IS DISCHARGED ON A STATIC/ASSOCIATED-FUNCTION CALL -- `Type.assoc_fn(args)` never reaches the call-site engine at all,… | 5514b6f |
| B-2026-08-21-53 | typecheck+parser | medium | CALL-SITE TYPE APPLICATION `f[T](args)` IS IMPLEMENTED ONLY FOR THE `ptr.*` BUILTINS -- `Vec.new[i64]()`, `Channel.new[i64]()` and a user generic `id… | f666fe7 |
| B-2026-08-21-54 | other | medium | `test_shortener_example_end_to_end` ASSERTS A JSON KEY ORDER and is RED on main since the FxHash change (8c3c8d6) reordered Map iteration -- and it i… | 69be64c |
| B-2026-08-22-11 | cli | medium | `karac fmt` DELETED AN INLINE ASSOCIATED-TYPE BINDING FROM EVERY TRAIT BOUND -- `I: Src[Item = i64]` was rewritten to `I: Src[]`, a weaker contract t… | Every trait-bound render site now goes through one `format_… |
| B-2026-08-22-12 | codegen | medium | A METHOD CALL THROUGH A RETURN-POSITION `impl Trait` VALUE HAS NO CODEGEN DISPATCHER -- `make().get()` is check-green and interpreter-green and fails… | a3ee407e |
| B-2026-08-22-13 | typecheck | high | A STATIC/ASSOCIATED CALL BORROWED AN UNRELATED GLOBAL FUNCTION'S WHERE CLAUSE WHEN THE NAMES COLLIDED -- `H.take(x)` was checked against the stdlib's… | 5514b6f |
| B-2026-08-22-14 | codegen+effect | low | AN EXISTENTIAL DECLARING POLYMORPHIC EFFECT VARIABLES (`-> impl Emit with F`) IS THE ONE RETURN-POSITION `impl Trait` SHAPE STILL WITHOUT A BUILD --… | 2dd4f972 |
| B-2026-08-22-15 | typecheck | medium | A `-> Self` TRAIT METHOD CALLED ON AN EXISTENTIAL RECEIVER LOSES THE TRAIT -- `make().bumped()` types as a BARE type parameter named after the trait,… | c4603ac |
| B-2026-08-22-16 | typecheck | medium | `Channel.bounded(cap)` EXISTED IN NO SPELLING -- implemented across runtime, typechecker, interpreter and codegen, returning the same `(Sender[T], Re… | 555495c9 |
| B-2026-08-22-17 | typecheck | high | THE BLESSED EXPLICIT SPELLING DID NOT WORK FOR BUILTIN TYPES -- `Vec[i64].new()` / `Map[K, V].new()` / `Channel[T].new()` all reported "no method 'ne… | 6f02f211 |
| B-2026-08-22-18 | codegen | high | MOVING A HEAP ELEMENT OUT OF AN OWNED `Array[T, N]` PARAMETER DOUBLE-FREES IT -- `fn take_first(a: Array[String, 2]) -> String { return a[0]; }` free… | 1b75e99 |
| B-2026-08-22-19 | autopar | medium | TWO SENDS ON A CHANNEL SPURIOUSLY SERIALIZE -- `ConcurrencyChecker::two_effects_conflict` codes `(Sends,Sends)` and `(Receives,Receives)` as a CONFLI… | 8fcfb64d |
| B-2026-08-22-20 | effect | medium | THE BUILTIN CHANNEL METHODS CARRY NO `sends`/`receives` EFFECT -- `Sender.send` is seeded `allocates(Heap)` and nothing else, and the ONLY `EffectVer… | e576a8c |
| B-2026-08-22-21 | typecheck | low | `Sender.try_send` AND `Receiver.recv_blocking` ARE SPEC'D BUT DO NOT EXIST -- design.md:6070 declares both with effects, and each is "no method '<nam… | bde5935 |
| B-2026-08-22-22 | interp | low | FLAKY TEST -- `a_map_hashes_through_the_hasher_it_was_built_with` asserts that SipHash13 and Fx produce DIFFERENT iteration orders over six keys, but… | d80540b6 |
| B-2026-08-22-23 | codegen | low | A STRUCT-WITH-HEAP CHANNEL PAYLOAD LEAKS ITS OWN `String`/`Vec` FIELDS ON `try_send`'S REJECT PATH -- the `SendError.Full(v)` binding frees a String… | 0e5dca3 |
| B-2026-08-22-24 | interp+runtime | low | THE INTERPRETER CANNOT MODEL RECEIVER LIVENESS, SO `SendError.Closed` AND `send`'S NO-RECEIVER PANIC ARE BOTH UNREACHABLE -- the compiled runtime has… | d6fe527 |
| B-2026-08-22-25 | typecheck+cli | low | `karac fix`'s `redundant_suffix` DELETION SPAN IS ONE CHARACTER SHORT ON THE UNDERSCORE-SEPARATED SUFFIX FORM: `0_i64` is rewritten to `0_`, leaving… | 098daf6 |
| B-2026-08-22-26 | codegen | high | A collection literal passed to a `ref Slice[T]` parameter through a METHOD CALL fails LLVM module verification -- `coerce_to_slice` builds the `{ptr,… | 840f95ef |
| B-2026-08-22-27 | codegen+runtime | high | A COMPILED `Map[i64, V, H]` under any NON-DEFAULT hasher stops growing after one resize and silently drops every key past it: `len` still counts them… | bab8cf2 |
| B-2026-08-22-28 | typecheck | medium | An associated-type bound declared on a STDLIB-BAKED trait is never discharged at an impl site: `trait_assoc_decls` finds the trait declaration only b… | d0ced1a |
| B-2026-08-22-29 | codegen+runtime | medium | `FxBuildHasher` WAS 13.7x SLOWER than the default on `String` keys because an Fx digest's low bits -- the ones karac's table takes its bucket index f… | d7cc9cfd |
| B-2026-08-22-30 | typecheck | medium | Three `f16_software_emulated` lint tests fail on EVERY aarch64 macOS machine on a clean `main`: the tests assert the emulation warning fires, while `… | 6cf7be8e |
| B-2026-08-23-1 | codegen | medium | Owned Array[T, N] with heap elements leaks its element buffers at -O0 -- no scope-exit element drop was ever registered (the -O0 remainder B-2026-08-… | 6c3957cf |
| B-2026-08-23-2 | ownership | medium | Ownership oracle does not model fixed-array (Array[T,N]) ownership, so the drop-differential is blind to the array-element-drop class and the planned… | 6406ef1 |
| B-2026-08-23-3 | typecheck | medium | `f16_software_emulated` asked ONE capability question for TWO widths, so `bf16` arithmetic warned nowhere on a native-`f16` CPU (`apple-m1`, `sapphir… | 6cf7be8e |
| B-2026-08-23-4 | codegen | low | Codegen owns a LOCAL fixed array element-wise but a PARAM fixed array whole, so the drop-differential cannot gate the array class by place name and n… | 7102243 |
| B-2026-08-23-5 | ownership+codegen | medium | The drop_fuzz oracle-codegen differential corpus reports 94 divergences at the default size (186 at --count 400), against a module doc that states th… | f0a55a6 |
| B-2026-08-23-6 | codegen | high | ANY non-`main` function declared with an EXPLICIT `-> ()` return type miscompiles into UNBOUNDED RECURSION on both compiled backends | 550eeef |
| B-2026-08-23-7 | effect | high | A declared effect is LOST when a function is reached through a LET BINDING or a CONTAINER ELEMENT, so a public function's effect declaration -- "guar… | 67e0e1f |
| B-2026-08-23-8 | effect | medium | BUILT-IN PRIMITIVE RESOURCE effects are never inferred -- stdout, stdin, stderr, env and the filesystem contribute NOTHING to any effect set, so desi… | 72b91bb |
| B-2026-08-23-9 | interp | medium | The INTERPRETER SKIPS an `errdefer` block when the function's `Err` comes from its TAIL EXPRESSION, while both compiled backends run it | 9c95c06 |
| B-2026-08-23-10 | parser | low | UNIT STRUCTS (`struct Name;`) do not parse, so design.md § Typestate via Phantom Type Parameters' state markers cannot be written as the spec writes… | 54548e0 |
| B-2026-08-23-12 | effect | medium | A `let` binding's `Fn(...)` TYPE ANNOTATION is never checked against the assigned function's effects, so `let f: Fn(i64) -> i64 = save;` -- a slot th… | 4f67f9d1 |
| B-2026-08-23-13 | effect | low | The mutual-recursion NOTE now fires for two functions that merely REFERENCE each other as VALUES, calling them a "mutual recursion group" though neit… | 249f165 |
| B-2026-08-23-14 | codegen | high | `eprintln` output is SILENTLY DROPPED under `karac run` (JIT) and `karac build` (AOT) -- a compiled program loses every stderr write | 5786ab6 |
| B-2026-08-23-15 | interp | medium | The INTERPRETER does not order `eprintln` output across `par {}` branches, so stderr from a parallel region interleaves NONDETERMINISTICALLY -- 4 dis… | a5dc4691 |
| B-2026-08-23-16 | typecheck+codegen | high | `dbg(x)` HAS NO CODEGEN LOWERING AT ALL, so under JIT and AOT it returns CONSTANT 0 instead of its argument -- design.md calls it "an identity functi… | 107bea6 |
| B-2026-08-23-17 | codegen+interp | medium | A PANIC MESSAGE goes to STDOUT under JIT and AOT but to STDERR under `--interp`, and design.md § Entry Point (line 6423) says stderr for all of them | c12de8f |
| B-2026-08-23-18 | codegen | medium | `dbg(x)` still has NO CODEGEN LOWERING, so a program containing it cannot be compiled at all -- `karac build` and `karac run` now REFUSE it with an a… | a112513 |
| B-2026-08-23-19 | codegen | high | `errdefer(e) { .. | 4aaef4d |
| B-2026-08-23-20 | codegen | medium | `rebuild_value_from_payload_words` takes exactly THREE payload words, so an Option/Result payload held INLINE in four or more words reconstructs its… | 16312b9 |
| B-2026-08-23-21 | interp | medium | The INTERPRETER's entry-point error line renders `E` through Rust's `Display for Value` instead of the Kāra `Display` impl, so `main() -> Result[(),… | 67c2a3a |
| B-2026-08-24-1 | effect | medium | The `Fn(..)` EFFECT SLOT IS STILL UNCHECKED AT FOUR NON-`let` POSITIONS after B-2026-08-23-12 wired the binding annotation | 536c495 |
| B-2026-08-24-2 | codegen | low | `dbg(x)` where `x` is a `shared struct` / `shared enum` REFUSES to compile: "codegen: `dbg` at L:C cannot render a value of type `Sh` yet — the compi… | 6dc7d9d |
| B-2026-08-24-3 | parser | low | A STRUCT LITERAL WITH EXPLICIT GENERIC ARGUMENTS (`Connection[Disconnected] { socket: .. | 8dd822e |
| B-2026-08-24-4 | interp | high | A RUNTIME ERROR inside a `par {}` branch is swallowed WHOLE by the interpreter -- no message on stderr, exit code 0, and the statements after the joi… | de0fee9 |
| B-2026-08-24-5 | codegen | high | `asan_local_fixed_array_returned_out_is_not_dropped_twice` DOUBLE-FREES at KARAC_OPT_LEVEL=0, so `scripts/asan-o0-leg.sh` is RED on main and reports… | 5a585d8 |
| B-2026-08-24-6 | codegen | low | `dbg(x)` where `x` is a `shared ENUM` still REFUSES to compile -- "codegen: `dbg` at L:C cannot render a value of type `Shape` yet" -- while `karac r… | 7d0a582 |
| B-2026-08-24-7 | interp | medium | A RUNTIME ERROR inside `return <expr>` is OVERWRITTEN by the return's own control flow, so the interpreter KEEPS EXECUTING after the fault and hands… | b964f37 |
| B-2026-08-24-8 | effect | low | A BARE `Fn(..)` SLOT IS IMPRACTICAL BECAUSE `panics` IS INFERRED FROM ORDINARY INDEXING and is not a transparent verb, so a closure as plain as `\|w,… | 40c6c13 |
| B-2026-08-24-9 | codegen | low | `dbg(x)` where `x` is a SELF-REFERENTIAL `shared enum` (`shared enum T2 { Node(i64, T2), Leaf(i64) }`) REFUSES to compile | 0ee3e15 |
| B-2026-08-24-10 | parser+typecheck+codegen | high | A TAIL-POSITION `loop` DROPS ITS BREAK VALUE and the declared return type is NEVER CHECKED, so one program behaves THREE DIFFERENT WAYS: `fn pick() -… | 787c355 |
| B-2026-08-24-11 | effect | low | The `Fn(..)` EFFECT SLOT INSIDE A CONTAINER is never checked, so an effectful function placed in a pure-declared element slot is accepted in silence:… | 2193636 |
| B-2026-08-24-12 | effect | medium | THE COMPILER'S OWN SUGGESTED FIX IS UNFOLLOWABLE for a public function that RETURNS an `Fn(..)` value | 05a5108 |
| B-2026-08-24-13 | codegen | medium | A NON-SCALAR `break` VALUE (String / Vec / struct) cannot travel through the compiled backends' break-value slot: `fn pick() -> String { loop { break… | 758bf19 |
| B-2026-08-24-14 | other | medium | `karac fmt` EMITS UNPARSEABLE SOURCE for any `Fn(..)` / `OnceFn(..)` type annotation, and silently drops two declarations doing it | 05a5108 |
| B-2026-08-24-15 | codegen | high | `Vec.insert` / `Vec.remove` / `Vec.swap_remove` HAVE NO CODEGEN BOUNDS CHECK: an out-of-range index CORRUPTS THE HEAP (insert/remove) or SILENTLY RET… | 114a9d2 |
| B-2026-08-24-16 | effect | medium | A declared `Fn(..)` EFFECT SLOT is checked only when the value arrives in the DECLARING statement, so the same annotation gives two different answers… | 776680a |
| B-2026-08-24-17 | effect | low | A declared `Fn(..)` element slot on a receiver that is NOT a plain named binding is still unchecked: `self.items.push(save)` carries the same slot as… | a9d540b |
| B-2026-08-24-19 | codegen | low | A `Map` / `Set` BREAK VALUE could not leave a loop on the compiled backends: `loop { ...; let mut m = Map.new(); ...; break m }` FAILED AT MODULE VER… | d9b5ea0 |
| B-2026-08-24-20 | effect | low | A METHOD-CHAIN receiver's declared `Fn(..)` element slot is still unchecked: `hand_out().push(save)` carries the same slot as a bound `v.push(save)`,… | ba5b136 |
| B-2026-08-24-21 | codegen | low | A `shared` STRUCT OR ENUM BREAK VALUE cannot leave a loop on the compiled backends, and the OBVIOUS FIX MAKES IT WORSE: `loop { ...; let n = Node { v… | 54fcfd3 |
| B-2026-08-24-22 | ownership | medium | `karac check` IS NOT DETERMINISTIC: the same binary on the same unchanged input emits a DIFFERENT rc-fallback diagnostic run to run | 9628ede |
| B-2026-08-24-23 | effect | low | The `--output=json` `mutual_recursion_groups` field still reports a VALUE-ONLY cycle as a mutual recursion group, the same false claim B-2026-08-23-1… | 7000780 |
| B-2026-08-24-24 | typecheck | low | `File.open` takes an owned `String`, so a read-only path predicate cannot borrow one and must clone | 0382211 |
| B-2026-08-25-1 | codegen | low | An RVALUE `break` value that owns heap is still refused: `break Node { v: 11 }` (shared), `break Map.new()`, and `break f"x"` in a labeled-block TAIL… | 0892679 |
| B-2026-08-25-2 | codegen | high | `std.cli` IS NOT AOT-COMPILABLE: every pure-Kāra INSTANCE method on its types (`Arg.required`, `Parser.about`, .. | d7bd8ac9 |
| B-2026-08-25-3 | codegen | medium | `codegen_tests::vec_mutation_methods_bounds_check_out_of_range_index` IS FLAKY, and it fails by showing exactly the PRE-FIX symptom of the high-sever… | e647b5a |
| B-2026-08-25-4 | typecheck | medium | An ASSOCIATED fn inside a BOUNDED generic impl saw the impl's OWN bound as unsatisfied on a local receiver | b4042fa |
| B-2026-08-25-5 | codegen | high | A generic method calling a MUTATING sibling generic method in a loop HANGS under codegen (interp ok) | 8e93189 |
| B-2026-08-25-6 | codegen | low | A BRANCHING `break` carrier that owns heap is still refused: `break if c { Node { v: 4 } } else { Node { v: 5 } }` fails module verification while th… | e01dce4 |
| B-2026-08-25-7 | codegen | high | A generic method that REBINDS its owned receiver to a local (`let mut h = self`) and drains through a sibling `mut ref self` method returns EMPTY ele… | cbc545f |
| B-2026-08-25-8 | codegen | high | A MUTUAL type cycle through a `Vec` CRASHES THE COMPILER: `struct Inner { owner: Outer }` + `struct Outer { kids: Vec[Inner] }` passes `karac check`… | 39b159e |
| B-2026-08-25-9 | codegen | medium | A TEMPORARY passed as the path argument to a `#[compiler_builtin]` fs entry point is never freed: ~47 bytes leak per call, under BOTH the owned and `… | e7a99bb |
| B-2026-08-25-10 | codegen | high | A generic impl method that transfers heap-owning content OUT of an owned `self` (`fn into_vec(self) -> Vec[T] { self.xs }`, or a drain through `pop`)… | c91d018 |
| B-2026-08-25-11 | codegen | medium | A monomorph's per-element drop for a `Vec[T]` field is emitted as an EMPTY STUB (`karac_drop_Vec`, `ret void`) when the impl's `T` is recorded head-o… | 6789e4a |
| B-2026-08-25-12 | codegen | high | `match` on a `Result` whose Ok payload is a struct with THREE OR MORE `Vec` fields SEGFAULTS the compiled binary while the interpreter is correct | 443a902 |
| B-2026-08-25-13 | codegen | high | `lower_stdlib_source` DISCARDS the typecheck errors it computes for every baked stdlib module, so a baked module can ship code `karac check` rejects… | 0440daa |
| B-2026-08-25-14 | codegen | medium | An owned `self` aggregate param's OWN scope-exit drop is the UNMANGLED base drop (`__karac_drop_struct_Heap`, outer-only), not the monomorph's, so a… | 6df0bfb |
| B-2026-08-25-15 | codegen+runtime | high | Returning a heap-bearing FIELD out of a BORROWED match payload (`return pv.value` where `pv` is bound from `.get()` on a `ref self` receiver) MOVES t… | 55f16cb |
| B-2026-08-25-16 | codegen | medium | An aggregate TEMPORARY used as the receiver of an owned-`self` method is dropped with the UNMANGLED outer-only drop (`__karac_drop_struct_Heap`), so… | a9856f1 |
| B-2026-08-25-17 | codegen | medium | A DISCARDED method-call result that owns heap is never freed: `h.xs.pop();` as a bare expression statement leaks the popped element's buffer (8 bytes… | c8753cd |
| B-2026-08-25-18 | codegen | medium | Discarded heap-owning pop in a generic FREE FUNCTION leaks the container's elements at -O0 -- B-2026-08-25-17's free-fn precedence guard was not actu… | Fixed in two paired sites |
| B-2026-08-25-19 | typecheck | medium | The ENTIRE `Box` / `Rc` / `Arc` smart-pointer family is UNDEFINED -- all six of `Box.new`, `Box.try_new`, `Rc.new`, `Rc.try_new`, `Arc.new`, `Arc.try… | 803d173 |
| B-2026-08-25-20 | typecheck | medium | Three of the fallible-allocation `try_*` methods design.md names by name do not exist, and `AllocError`'s `Display` is Debug formatting rather than t… | 2242d1c |
| B-2026-08-25-21 | typecheck | low | `m.try_insert(k, v)?;` as a statement warns `discarded 'Option' value`, though the displaced-value must-use exemption exists precisely for that shape… | 0fcc886 |
| B-2026-08-25-22 | typecheck | medium | The STABLE-HASH module does not exist, so the escape design.md prescribes for every use case where `Hash` is explicitly the wrong tool is unavailable | 44851f9 |
| B-2026-08-25-23 | cli | low | `karac explain --concept=` supports exactly ONE concept, `closures`, and it is not one of the pages design.md names | 7472e06 |
| B-2026-08-25-24 | resolver | low | The unassigned-layout-fields warning renders as `warning[resolve]` instead of `warning[layout_unassigned_fields]`, so the suppression design.md presc… | 733c0f2 |
| B-2026-08-25-25 | codegen | high | An ASSOCIATED fn in a GENERIC impl that binds a struct LITERAL to a local and calls a MUTATING sibling through it dispatches to the UNMANGLED base pr… | 8d32a0d |
| B-2026-08-25-26 | typecheck | low | An un-inferrable `T` from an EMPTY array literal is reported as a BOUND failure naming `<error>` as the type, pointed at a later method call rather t… | 40a54f6 |
| B-2026-08-25-27 | codegen | high | SIX `let`-RHS forms bound a generic-struct value with NO recorded instantiation, so the sibling call dispatched to the unmangled base prototype and S… | 14c6d32 |
| B-2026-08-25-28 | codegen | high | A method call whose receiver is an unnamed TEMPORARY inside a generic impl dispatches to the unmangled base prototype and SEGFAULTS at a heap-carryin… | 5fa76a7 |
| B-2026-08-25-29 | other | medium | TWO CLAIMS in design.md § Operator Traits contradicted the implementation, and BOTH TURNED OUT TO BE SPEC DEFECTS rather than the missing features th… | 297ef4f |
| B-2026-08-25-30 | typecheck | medium | OPERATOR-FAILURE DIAGNOSTICS DO NOT SPEAK THE TRAIT LANGUAGE design.md § Operator Traits mandates, and two of them NAME A TRAIT THE USER DOES NOT NEED | 3964ede |
| B-2026-08-25-31 | typecheck | low | The un-inferrable-type-argument diagnostic points at the first USE, not at the empty literal that caused it, because `Type::Error` carries no provena… | a54d41d |
| B-2026-08-25-32 | other | medium | `PriorityQueue` HAS NO `peek`: there is NO non-destructive way to read the root, so the archetypal two-heap median use case cannot read a queue's hea… | e2673b8 |
| B-2026-08-25-33 | codegen | high | `?` in `main()` SEGFAULTS under `karac run` (the default JIT executor) whenever main's error type is an enum with an INLINE SCALAR payload -- `AllocE… | 7d20106 |
| B-2026-08-25-34 | codegen | medium | `AllocError`'s `Display` renders the Debug form (`OutOfMemory { requested_bytes: 4096 }`) into the user-facing `Error: {e}` entry-point line, and the… | 7553a08 |
| B-2026-08-25-35 | typecheck+codegen | high | `PriorityQueue[T]` IS UNINSTANTIABLE FOR EVERY USER TYPE, because the OPERATOR path and the TRAIT-BOUND path recognize DISJOINT sets of `Ord` impleme… | 8af03a8 |
| B-2026-08-26-1 | other | low | design.md § Operator Traits § Notably absent prescribes `vec.concat(other)` as one of the two redirects for `vec1 + vec2`, but `Vec.concat()` in this… | 5964c16 |
| B-2026-08-26-2 | typecheck | medium | TWO MORE FACILITIES design.md § `Hash` and `Hasher`'s stability paragraph NAMES AS SHIPPED DO NOT EXIST, after B-2026-08-25-22 built the third | 7da2bfe |
| B-2026-08-26-3 | typecheck+codegen | high | `LazyLock[T]` IS A CHECK-ONLY PHANTOM: `let TABLE: LazyLock[i64] = LazyLock.new(\|\| 40 + 2);` passes `karac check` cleanly and then fails on EVERY exe… | 3988b47 |
| B-2026-08-26-4 | other | low | design.md's CANONICAL `?`-operator example calls `input.parse_i64()?` and names `ParseError` -- NEITHER EXISTS: the real API is `i64.parse(s) -> Opti… | 5a7e985 |
| B-2026-08-26-5 | codegen | medium | `?` in `main() -> Result[(), E]` reconstructed `E` from only its first THREE payload words, so any error held inline in four or more words rendered g… | 06880a4 |
| B-2026-08-26-6 | interp+codegen | high | `~` COMPLEMENTS AT THE CARRIER WIDTH, NOT THE DECLARED WIDTH, so every narrow UNSIGNED integer gets a wrong value and the two backends disagree on wh… | 297ef4f |
| B-2026-08-26-7 | codegen | high | A NARROW LOCAL INITIALIZED FROM AN UNSUFFIXED LITERAL IS ALLOCATED AT i64 IN CODEGEN, so every width-sensitive operation on it runs at 64 bits and ca… | 08410c2 |
| B-2026-08-26-8 | interp | medium | INTERPRETER `Vector[T, N]` LANE ARITHMETIC IGNORES THE LANE WIDTH, so a narrow-lane vector computes on the i128 carrier and diverges from codegen, wh… | ff403a3 |
| B-2026-08-26-9 | codegen | high | `PriorityQueue.push` RUNS THE DROP GLUE FOR ITS BY-VALUE PARAMETER ON A VALUE IT HAS ALREADY MOVED into the backing `Vec`, so an element type with `i… | d9c520e |
| B-2026-08-26-10 | typecheck+codegen | medium | `Map` HASHING NEVER CALLS A HAND-WRITTEN `impl Hash`: with the derive absent the key is REJECTED, and with `#[derive(Hash)]` present alongside the im… | 095087d |
| B-2026-08-26-11 | other | medium | design.md's `String` METHOD TABLE names the char-append method `push_char`, which DOES NOT EXIST -- the working method is `push(c: char)`, and the ta… | 0564ee2 |
| B-2026-08-26-12 | codegen | high | HEAP CORRUPTION ON BOTH COMPILED BACKENDS: an `if`-EXPRESSION whose arms yield an `Option[shared]` binding, passed BY VALUE to a function inside a lo… | 3454927 |
| B-2026-08-26-14 | codegen | high | A `par` BRANCH WHOSE BODY CALLS A VALUE-PRESERVING SCALAR METHOD (`abs` / `sqrt` / a `float_math` transcendental) ON A NARROW-INTEGER RECEIVER FAILS… | 08410c2 |
| B-2026-08-26-15 | typecheck | low | `LazyLock.new`'s closure-capture restriction is UNENFORCED: design.md says the closure "may only capture other module-level compile-time bindings; ca… | bd68c8a |
| B-2026-08-26-16 | effect | medium | UNDECLARED EFFECTS ESCAPE THROUGH `LazyLock.get()`: design.md says the effect system attributes first-access initialization to the CALLING function,… | b77511f |
| B-2026-08-26-17 | codegen | high | A MODULE-SCOPE `Atomic[T]` failed BOTH compiled backends with `codegen: Atomic receiver 'X' has no slot` while `--interp` was correct -- and module s… | f79cefd |
| B-2026-08-26-18 | codegen | medium | A `PriorityQueue[T]` whose element T is a STRUCT CARRYING A HEAP FIELD leaks that field's buffer -- 31 bytes in 8 allocations on the three-push fixtu… | 54c5421 |
| B-2026-08-26-19 | typecheck | low | A `ref T`-returning method is accepted in a plain value position but REJECTED as a closure tail against `Fn() -> T`: `\|\| cell.get_or_init(\|\| 7)` fail… | fa5e169 |
| B-2026-08-26-20 | codegen | medium | A `char` returned through a CLOSURE prints as its integer code point under both compiled backends while the interpreter prints the character: `let c:… | d3b3053 |
| B-2026-08-26-21 | codegen+interp | medium | A RELOCATING element store (`b.xs[i] = b.xs[j]`, then `b.xs[j] = t`) runs the displaced element's user `Drop` BODY, even though the value is being mo… | 6a73afd |
| B-2026-08-26-22 | typecheck+codegen | medium | The fallible-allocation table's REMAINING gaps, after B-2026-08-25-20 landed the Vec capacity family: `try_resize` / `try_append` have panicking base… | 4c3da28 |
| B-2026-08-26-23 | codegen | medium | `Ordering`'s PREDICATE METHODS (`is_lt` / `is_le` / `is_gt` / `is_ge` / `is_eq`) fail to BUILD whenever codegen cannot name the receiver's type -- a… | 4e75708 |
| B-2026-08-26-24 | codegen+interp | medium | A COMPARISON OPERATOR INSIDE A GENERIC BODY never dispatches to the element's user `impl Ord`: operator lowering resolves only CONCRETE operand types… | 191dc02 |
| B-2026-08-26-25 | typecheck | medium | ORDERING OPERATORS ON A `shared struct` ARE ADMITTED BY THE TYPECHECKER AND IMPLEMENTED BY NEITHER BACKEND: `a < b` on a shared struct passes `karac… | 47a6810 |
| B-2026-08-26-26 | effect+codegen | medium | RUN-VS-BUILD ON EVERY FALLIBLE `try_*` COMPANION CONSUMED WITH `?`: the auto-parallelizer hoists the call into a worker function, which returns void,… | aae805f |
| B-2026-08-26-27 | codegen | medium | `Vec.try_from_iter` CANNOT BE REGISTERED UNTIL `collect` HAS A FALLIBLE LOWERING: its base `Vec.from_iter` lowers to `iter.collect()`, whose codegen… | c5da7e8 |
| B-2026-08-26-28 | codegen | medium | A NESTED `Result` AS `main`'s ERROR TYPE LOSES BOTH ITS TAG AND ITS PAYLOAD on every compiled backend: `main() -> Result[(), Result[A, B]]` returning… | c9db880 |
| B-2026-08-26-29 | interp+codegen | medium | A MANUAL `impl Display` IS IGNORED AS SOON AS THE VALUE IS NESTED: an enum with a hand-written `Display` renders through its impl at top level (`f"{e… | 59d6568 |
| B-2026-08-26-30 | codegen | medium | SWEEP THE REMAINING ENTRY-HOISTED-ALLOCA / CONDITIONAL-STORE / SCOPE-TRACKED SITES | 225e233c |
| B-2026-08-26-31 | codegen | medium | A local MOVED into a container's struct element slot (`b.xs[j] = t`) kept its own `UserDrop` registration, so its `impl Drop` body ran a SECOND time… | 0af8b28 |
| B-2026-08-26-32 | codegen | medium | `Map.get` WITH AN INLINE TEMPORARY STRUCT KEY THAT OWNS HEAP LEAKS THE TEMPORARY -- one allocation per lookup | b46cb47 |
| B-2026-08-26-33 | codegen | medium | A `#[derive(Display)]` STRUCT WITH A PAYLOAD-BEARING ENUM FIELD IS REFUSED BY `karac build` -- and the refusal is INVERTED with respect to difficulty… | 7b5f733 |
| B-2026-08-26-34 | codegen | medium | `Vec.swap` HAD NO CODEGEN ARM: `karac build` hard-errored "Vec/String method 'swap' is not yet supported in codegen" on a program `karac run` execute… | 4d75c59 |
| B-2026-08-26-35 | interp | medium | `Vec.swap` with an out-of-range index SILENTLY DID NOTHING in the interpreter: `v.swap(0, 99)` on a two-element Vec left the vector untouched and exi… | 4d75c59 |
| B-2026-08-26-36 | parser | high | `ref` IN EXPRESSION POSITION IS SPECIFIED BUT UNIMPLEMENTED: design.md shows `let r = ref some_function();` (§ Binding-extension exception) and `let… | 0eb4e2f |
| B-2026-08-26-37 | typecheck | low | The index-move rule covers only a `let` initializer and an assignment RHS; OTHER value positions still read a non-`Copy` element by value and silentl… | 6a73afd |
| B-2026-08-26-38 | codegen | medium | A `StringSlice` BOUND BY A MATCH PATTERN LOST EVERY METHOD BUT `to_string` under `karac build` -- `match head(s) { Some(v) => v.len() }` over an `Opt… | d5056cd |
| B-2026-08-26-39 | codegen | medium | THE `Error return trace` SECTION IS NONDETERMINISTIC ON THE COMPILED BACKENDS: the same AOT binary, same input, same pinned `KARAC_HASH_SEED`, emits… | 7c10fdf |
| B-2026-08-26-40 | typecheck | low | A WRONG-ARITY CALL TO AN ASSOCIATED FUNCTION IS REPORTED AS IF THE FUNCTION DID NOT EXIST: `String.with_capacity()` and `String.with_capacity(1i64, 2… | 9d530ad |
| B-2026-08-26-41 | codegen+interp | medium | A USER `impl Drop` ON A MAP **KEY** TYPE NEVER FIRES, while the same impl on the VALUE type does | 34c3f54 |
| B-2026-08-27-1 | typecheck+interp+codegen | high | TWO `impl From[X] for T` FOR THE SAME TARGET SILENTLY RUN THE WRONG ONE: `?` and `.into()` resolve the conversion by target name only (`T.from`), so… | 0dffbc0 |
| B-2026-08-27-2 | codegen+interp | medium | `Map.remove(k)` DESTROYS AN ENTRY'S KEY IN PLACE AND RUNS NO `Drop` BODY FOR IT | eca034b |
| B-2026-08-27-3 | codegen | medium | `Map.remove` LEAKS A STRUCT KEY'S HEAP FIELDS -- one allocation per removal | 0170c65 |
| B-2026-08-27-4 | codegen | high | A `shared` STRUCT USED AS A MAP/SET KEY IS MATCHED BY POINTER IDENTITY ON THE COMPILED BACKENDS AND STRUCTURALLY BY THE INTERPRETER | 393424d |
| B-2026-08-27-5 | codegen | medium | `==` ON A `shared struct` WITH A NICHE-ENCODED `Option[shared T]` FIELD COMPARES RAW POINTERS, so two structurally-equal values answer `false` on the… | 41b233c |
| B-2026-08-27-6 | codegen | medium | A PLAIN (non-shared) ENUM USED AS A MAP/SET KEY WITH A HEAP-BEARING PAYLOAD COMPARES ITS PAYLOAD WORDS INSTEAD OF RECURSING, so `Map[S, V]` where `en… | a35c3f5 |
| B-2026-08-27-7 | codegen+interp | medium | CONTAINER ELEMENT `Drop` BODIES FIRE IN A DIFFERENT ORDER ON THE TWO BACKENDS: the interpreter walks INSERTION order (deterministic across seeds and… | 478fef6 |
| B-2026-08-27-8 | codegen | low | NLL DROP PLACEMENT IS DISPATCHED ON AN LLVM SYMBOL-NAME PREFIX | a74acc3 |
| B-2026-08-27-9 | codegen | medium | A FLOAT FIELD OF A `shared struct` IS COMPARED BY BITS, NOT BY IEEE SEMANTICS, so `==` is wrong in BOTH directions on the compiled backends: `0.0` vs… | cac21aa |
| B-2026-08-27-10 | typecheck+codegen | high | `Vec[T] == Vec[T]` IS ACCEPTED BY `karac check` AND `karac build`, EXECUTED BY BOTH COMPILED BACKENDS, AND REJECTED AT RUNTIME BY THE INTERPRETER --… | a7512aa |
| B-2026-08-27-11 | codegen | high | A USER STRUCT NAMED `F64` / `F32` / `F16` / `Bf16` SILENTLY INHERITS THE PRELUDE TOTAL-ORDER WRAPPER'S COMPARISON, which compares ONE float field and… | 374a8a3 |
| B-2026-08-27-12 | codegen | medium | THE MAP/SET ENUM KEY COMPARATOR DECLINES TWO PAYLOAD SHAPES THAT `compile_enum_eq` ALREADY HANDLES ON THE `==` SIDE, so `Map[Option[String], V]` and… | 51d68e9 |
| B-2026-08-27-13 | codegen | medium | `Vec.contains` COMPARES AN ENUM ELEMENT'S RAW PAYLOAD WORDS, reaching NEITHER of the compiler's two structural enum comparators, so `vec![E.A(f"yy")]… | 6aead97 |
| B-2026-08-27-14 | typecheck | high | THE IMPL TABLE IS NAME-KEYED, so a user type that shadows a prelude name SILENTLY INHERITS THE STDLIB TYPE'S TRAIT IMPLS | e17ecdf |
| B-2026-08-27-15 | typecheck | low | THE PRELUDE-SHADOW LINT DOES NOT EXIST | e17ecdf |
| B-2026-08-27-16 | codegen | high | `==` ON A GENERIC ENUM WHOSE ARGUMENT OVERFLOWS ITS ERASED PAYLOAD ALLOTMENT PANICS THE COMPILER, so `Box1[String] == Box1[String]` for `enum Box1[T]… | 752e0b9 |
| B-2026-08-27-17 | codegen | high | `assert_eq` / `assert_ne` COMPARE AN ENUM'S PAYLOAD WORDS INSTEAD OF ITS CONTENTS, so a Kara test asserting two structurally-equal enums with heap pa… | 752e0b9 |
| B-2026-08-27-18 | codegen | high | `==` ON A USER STRUCT WITH EXACTLY THREE FIELDS PANICS THE COMPILER, so `struct S3 { a: i64, b: i64, c: i64 }` and `S3 { . | 9b8d435 |
| B-2026-08-27-19 | codegen | medium | AN ENUM WHOSE PAYLOAD STRUCT IS NOT WORD-ALIGNED IS TREATED AS HAVING NO HEAP PAYLOAD, so `==` on it silently compares payload WORDS: `enum Holder {… | d6cb5222 |
| B-2026-08-27-20 | codegen | high | A NESTED index store whose RHS is a named local double-frees under codegen (`d[0][1] = x`), while the same store from a temporary (`d[0][1] = f"zz"`)… | cd032829 |
| B-2026-08-27-21 | codegen | high | A `ref` element binding did not dispatch methods like the value it borrows, in two independent ways: `.clone()` on an element read THROUGH one (`let… | 367edc1 |
| B-2026-08-27-22 | codegen | high | `.clone()` on an element of an SoA-laid-out container returns the wrong data under codegen — element 5 reads back the cold group at indices 1 and 2 (… | 56985ec7 |
| B-2026-08-27-23 | typecheck | high | A tuple (`(i64, String)`) and `Option[T]` have NO `.clone()`, so the index-move rejection outlaws reading such an element by value with no replacemen… | 94711002536753 |
| B-2026-08-27-24 | typecheck+interp+codegen | medium | `Slice[T] == Slice[T]` IS ACCEPTED BY `karac check`, DIES IN THE INTERPRETER WITH A DIAGNOSTIC THAT BLAMES THE TYPECHECKER, AND IS REFUSED BY CODEGEN… | 30420faf |
| B-2026-08-27-25 | typecheck+codegen | medium | `Array[T, N] == Array[T, N]` IS ACCEPTED BY `karac check`, ANSWERS CORRECTLY UNDER THE INTERPRETER, AND FAILS TO COMPILE: codegen reports "left opera… | 30420faf |
| B-2026-08-27-26 | codegen | medium | `free_fresh_owned_str_arg` FREES A FRESH TEMPORARY OPERAND'S BUFFER BUT NOT ITS ELEMENTS, so a `Vec[String]` temporary compared with `==` leaks every… | e57bc89 |
| B-2026-08-27-27 | effect+cli | high | `karac check` ON A PROJECT DOES NOT RUN THE MULTI-FILE EFFECT CHECK THAT `karac build` RUNS, so a project prints "All checks passed." and then fails… | dc939a8 |
| B-2026-08-27-28 | codegen | medium | `Vec.contains(<fresh Vec temporary>)` LEAKS THE NEEDLE ENTIRELY -- buffer and elements -- because `free_fresh_owned_str_arg` does not fire at that ca… | 4af0901 |
| B-2026-08-27-29 | codegen | high | A STRUCT `.clone()` passed DIRECTLY as a call argument was never freed — `take(a.clone())` leaked its temporary once per call, unbounded in a loop, w… | 9f3f4fe |
| B-2026-08-27-30 | codegen | medium | A fresh `Array[String, N]` TEMPORARY compared with `==` leaks every element -- 26 bytes in 4 allocations under LSan, the same signature B-2026-08-27-… | c1c1f35 |
| B-2026-08-27-31 | codegen | medium | An INDEXED `Array[String, N]` temporary leaks its elements -- `mk(4)[0]` loses 13 bytes in 2 allocations -- a different position from B-2026-08-27-30… | 9c06c13 |
| B-2026-08-27-32 | codegen | medium | A struct FIELD of type `Array[T, N]` with heap elements is never dropped: `struct Holder { a: Array[String, 2] }` leaks 13 bytes in 2 allocations wit… | 9c06c13 |
| B-2026-08-27-33 | typecheck | medium | A `T: Ord` bound on a GENERIC IMPL rejects a tuple that the IDENTICAL bound on a free fn accepts: `impl[T: Ord] W[T]` refuses `W[(i64, i64)]` with "`… | 01d6784 |
| B-2026-08-27-34 | codegen | high | 3454927 (B-2026-08-26-12) EMITS THE BRANCH-LEAF RETAIN ONCE PER BINDING, NOT ONCE PER USE, so the THIRD consumption of an `Option[shared]` selected b… | c56e598 |
| B-2026-08-27-35 | codegen | medium | An array LITERAL temporary leaks ALL of its elements -- indexed (`Array[f"a{n}", f"b{n}"][0]`, 7 bytes in 2 allocations) and as an `==` operand (16 b… | 5313f90 |
| B-2026-08-27-36 | codegen | high | A MATCH-ARM payload binding inside a generic impl recorded the PRE-monomorphization `Bag[T]` into the name-keyed instantiation table, poisoning the F… | ce72041 |
| B-2026-08-27-37 | codegen | high | Destructuring a GENERIC struct out of a TUPLE PARAM and consuming its heap-carrying field DOUBLE-FREES under JIT and AOT while the interpreter is cor… | bd5dbc2 |
| B-2026-08-27-38 | interp+codegen | medium | Tuple comparison operators NEVER LOWERED on either backend, while `karac check` accepted them: `let a = (1, 2); let b = (1, 3); a < b` passed the typ… | 01d6784 |
| B-2026-08-27-39 | codegen | medium | A THREE-element tuple `==` PANICS THE COMPILER: `(1, 2, 3) == (1, 2, 4)` lowers to `{i64, i64, i64}`, structurally the same LLVM object as the `{ptr,… | 01d6784 |
| B-2026-08-27-40 | codegen | high | A TUPLE TYPE ARGUMENT does not survive the monomorphization substitution channel, which is keyed by type NAME | 3cfbd1e |
| B-2026-08-27-41 | typecheck+interp+codegen | medium | `.cmp()` on a tuple has no dispatch on ANY surface -- the typechecker refuses `(1, 2).cmp((1, 3))` outright, and both backends fall through their met… | 31cf904 |
| B-2026-08-27-42 | interp+codegen | low | `Array[T, N]` ORDERING operators never lower, while `karac check` accepts them: `a < b` on two `Array[i64, 2]` passes the typechecker and then fails… | b356e808 |
| B-2026-08-27-43 | codegen | high | AN ARM-LOCAL BINDING HANDED OUT OF A BRANCH LEAF LOSES ITS OWNER, so the FIRST consumption of the selected `Option[shared]` reads through a freed box | 9f62ac6 |
| B-2026-08-27-44 | codegen | medium | A heap-bearing TUPLE argument whose value ESCAPES the caller's frame LEAKS the caller's original buffer, because the callee entry-copies but the call… | 4efd5ab |
| B-2026-08-27-45 | interp+codegen | low | `Slice[T]` ORDERING operators never lower, while `karac check` accepts them -- the last member of the tuple/array family: `a < b` on two `Slice[i64]`… | 8cfae471 |
| B-2026-08-27-46 | interp | high | `.cmp()` WITH AN ENUM-VARIANT LITERAL AS THE RECEIVER answers a CONSTANT under the interpreter, independent of both operands -- `E.A.cmp(E.B)`, `E.B.… | 87210fe |
| B-2026-08-27-47 | codegen | medium | CODEGEN'S `.cmp()` RECEIVER SURFACE IS NARROWER THAN THE TYPECHECKER ADMITS: a struct LITERAL receiver (`P { . | fa6d35a |
| B-2026-08-27-48 | interp | medium | The INTERPRETER fires a user `Drop` body TWICE for a struct DESTRUCTURED out of a TUPLE PARAM, where the compiled backends fire it once: `fn take(p:… | b0dac13 |
| B-2026-08-27-49 | codegen | medium | A FIELD READ ON A BLOCK-EXPRESSION RECEIVER never lowers: `{ let x = make(); x }.n` fails `karac build` with "codegen: cannot resolve field 'n' on th… | 152f30f |
| B-2026-08-27-50 | codegen | high | A GENERIC FN'S CONTAINER PARAM BOUND TO A FIELD-ACCESS ARGUMENT loses its ELEMENT TYPE, because every resolver that recovers an element keys on the a… | 4e5bdc0 |
| B-2026-08-27-51 | codegen | medium | `Vec[Option[T]].sort()` IS CHECK-GREEN, SORTS CORRECTLY UNDER `--interp`, AND REFUSES TO BUILD -- `Vec.sort()`'s codegen element-type dispatch has it… | 12b88f9 |
| B-2026-08-27-52 | codegen | high | A READ-ONLY `ref` CONTAINER PARAM OF A GENERIC CALLEE, GIVEN A STRUCT-FIELD ARGUMENT, DOUBLE-FREES the field's buffer: the mono argument path compute… | 4e5bdc0 |
| B-2026-08-27-53 | codegen | medium | A `Slice[T]` PARAM GIVEN A STRUCT-FIELD `Vec` ARGUMENT from a generic-impl method fails MODULE VERIFICATION: the Vec-to-Slice coercion declines on a… | 6bbb01a |
| B-2026-08-28-1 | codegen | high | BOTH COMPILED BACKENDS RUN NO USER `Drop` BODY AT ALL for a struct DESTRUCTURED out of a tuple that is NOT a param -- a tuple LOCAL or a CALL RESULT:… | 383a723 |
| B-2026-08-28-2 | interp+codegen | medium | A user `Drop` body runs TWICE FOR ONE OBJECT on ALL THREE BACKENDS when a struct destructured out of a tuple param is RETURNED: `fn take(p: (R, i64))… | d5d7421 |
| B-2026-08-28-3 | codegen | medium | A STRUCT FIELD READ THROUGH A TUPLE ELEMENT never lowers when the tuple came from a CALL: `let q = make(); q.0.id` (and `make().0.id`) fail `karac bu… | 0f058f1 |
| B-2026-08-28-4 | interp+codegen | medium | A tuple-param destructure inside a CLOSURE is wrong on BOTH backends and in OPPOSITE directions: `let f = \|p: (R, i64)\| { let (r, n) = p; r.id + n };… | c4d53de |
| B-2026-08-28-5 | codegen | high | EVERY `bool` COMPARISON INVERTS IN COMPILED CODE: `false < true` answers FALSE and `Vec[bool].sort()` sorts DESCENDING on jit/build/auto-par, while t… | 45d2e1b |
| B-2026-08-28-6 | codegen | medium | `Vec.binary_search` ACCEPTS EVERY ELEMENT TYPE `sort` DOES BUT LOWERS ONLY INTEGER AND String -- tuples, nested Vec, Array, derived-Ord structs/enums… | 09913ea |
| B-2026-08-28-7 | codegen | medium | TWO MORE RECEIVER SHAPES the block-receiver fix does not reach, both still `karac check`-clean and interpreter-correct but failing `karac build` with… | dac56eb |
| B-2026-08-28-8 | codegen | medium | A NESTED TUPLE PATTERN never runs its inner leaf's user `Drop` body when the source is a tuple LOCAL: `let p = ((R { id: 41 }, 2), 1); let ((r, m), n… | 919f31e |
| B-2026-08-28-9 | codegen | medium | An ENUM leaf destructured out of a tuple LOCAL never runs its live variant's payload `Drop` body: `enum E { A(R), B } let p = (E.A(R { id: 41 }), 1);… | 919f31e |
| B-2026-08-28-10 | codegen | medium | A STRUCT-PATTERN destructure leaf's user `Drop` body is wrong in TWO OPPOSITE DIRECTIONS depending on the source: from a LOCAL it runs TOO EARLY (`dr… | f723dde |
| B-2026-08-28-11 | codegen | medium | A GENERIC struct leaf destructured out of a tuple never runs its Drop-bearing field's body on ANY source path -- local, call result and tuple literal… | 821d5ab |
| B-2026-08-28-12 | interp+codegen | medium | A tuple-element WILDCARD drops the element ZERO TIMES ON ALL THREE BACKENDS: `let (_, n) = p;` where element 0 declares `impl Drop` runs the body und… | 429977d |
| B-2026-08-28-13 | codegen | high | MOVING A `Vec` OUT OF A FIELD OF A BORROWED BINDING inside a GENERIC fn or impl DOUBLE-FREES the buffer under both compiled backends: `impl[T] Bag[T]… | e58aaef |
| B-2026-08-28-14 | codegen | medium | A `String` / `Vec` FIELD READ ON A SHARED-STRUCT TEMPORARY RECEIVER LEAKS THE BUFFER: `make().tag` where `make() -> shared Node` prints correctly and… | e17e361 |
| B-2026-08-28-15 | codegen | high | PROJECTING A HEAP-CARRYING STRUCT ELEMENT OUT OF A TUPLE PARAM AND RETURNING IT DOUBLE-FREES: `fn take(p: (R, i64)) -> R { p.0 }` where `struct R { i… | 0944166 |
| B-2026-08-28-16 | interp+codegen | medium | The B-2026-08-28-2 double `Drop` body SURVIVES when the tuple argument comes from a LOCAL instead of a literal: `let q = (R { id: 41 }, 1); let x = t… | 44ebce6 |
| B-2026-08-28-17 | interp+codegen | medium | THE STRUCT TWIN of B-2026-08-28-2: returning a field extracted from an owned STRUCT param runs its user `Drop` body TWICE on all three backends -- `s… | f697522 |
| B-2026-08-28-18 | codegen | medium | A tuple param returned WHOLE and then destructured AT THE CALL SITE loses its element's user `Drop` body on both compiled backends: `fn take(p: (R, i… | 0f058f1 |
| B-2026-08-28-19 | interp+codegen | low | The caller-side fresh-temp argument `Drop` walk fires AT THE CALL in the interpreter and at SCOPE EXIT in both compiled backends, so a tuple-literal… | 8888f9f |
| B-2026-08-28-20 | codegen | low | A BARE-`shared` FIELD READ OFF A SHARED TEMPORARY RECEIVER LEAKS THE INNER OBJECT'S PAYLOAD: `make_outer(12).inner.tag` prints `t12` on all three bac… | 9cc5fb1 |
| B-2026-08-28-21 | interp+codegen | medium | A PARENT STRUCT THAT DECLARES ITS OWN `Drop` still double-runs a returned field's user `Drop` body: with `impl Drop for W` added, `fn take(w: W) -> R… | 56e6493 |
| B-2026-08-28-22 | interp+codegen | medium | ALL THREE interprocedural escape predicates UNION over a callee's return sites, so a BRANCHY callee loses the user `Drop` body of whichever value act… | 7c0569a |
| B-2026-08-28-23 | interp+codegen | low | A NESTED PROJECTION out of an owned struct param still double-runs the field's user `Drop` body: `fn take(w: W) -> R { w.inner.r }` prints `drop 41`… | 4d37d4e |
| B-2026-08-28-24 | codegen | high | A TUPLE ELEMENT PROJECTED OUT OF A CONTAINER ELEMENT (`v[0].0`) IS NEVER CLONED AND DOUBLE-FREES ON BOTH COMPILED BACKENDS, AT EVERY POSITION INCLUDI… | 0723acf |
| B-2026-08-28-25 | codegen | high | A HEAP FIELD READ THROUGH A DEEPER PLACE ROOTED AT A `ref` BINDING AND RETURNED DIRECTLY IS NEVER CLONED AND DOUBLE-FREES: `fn peek(w: ref W) -> Stri… | d4da426 |
| B-2026-08-28-26 | codegen | medium | A TUPLE-TYPED BINDING LEAF loses its inner element's user `Drop` body when the source is a tuple LOCAL: `let p = ((R { id: 41 }, 2), 1); let (inner,… | 72b6f1c |
| B-2026-08-28-27 | codegen | medium | A FRESH TUPLE TEMP IS NEVER DROPPED, so every heap element it owns leaks: `println(twoheap(1).1)` where `twoheap -> (Person, String)` loses the whole… | 1aeaddc |
| B-2026-08-28-28 | codegen | medium | A GENERIC callee's tuple element cannot be named, so a struct field read through it still fails `karac build`: `fn firstof[T](x: T) -> (T, i64)` then… | 4d7ffa6 |
| B-2026-08-28-29 | codegen | medium | A destructure of a FRESH STRUCT source loses user `Drop` bodies on BOTH compiled backends while the interpreter runs one: `let W { r, n } = mk()` and… | 05fefaa |
| B-2026-08-28-30 | interp+codegen | medium | THE B-2026-08-28-12 WILDCARD DISCARD DISAGREES BETWEEN BACKENDS IN TWO OPPOSITE-DIRECTION SHAPES that its fix (429977d) did not cover: an ENUM elemen… | 34b1691 |
| B-2026-08-28-31 | interp+codegen | medium | AN ENUM WITH ITS OWN `impl Drop`, DISCARDED BY A WILDCARD DESTRUCTURE LEAF, RUNS NO BODY COMPILED while `karac run --interp` runs the enum's OWN body… | d283401 |
| B-2026-08-28-32 | codegen | low | A CONTAINER-ELEMENT HEAP READ CONSUMED BY AN UNBOUND TEMPORARY LEAKS: `println(Box2 { w: p[0].word }.w)`, `println((p[1].word, 9).0)`, and printing a… | 7b175c6 |
| B-2026-08-28-33 | codegen | low | A CLONED CONTAINER-ELEMENT STRUCT PASSED DIRECTLY AS AN OWNED ARGUMENT LEAKS 3 BYTES: `takes_r(w[1].0)` where `w: Vec[(R, i64)]`, because an owned ag… | 54ca269 |
| B-2026-08-28-34 | codegen | medium | A FIELD READ THROUGH A TUPLE ELEMENT OF A CONTAINER ELEMENT never lowers: `v[0].0.id` where `v: Vec[(R, i64)]` fails `karac build` with "codegen: can… | f6741d1 |
| B-2026-08-28-35 | codegen | high | A STRUCT-VALUED FIELD READ OFF A CONTAINER ELEMENT IS NEVER CLONED AND DOUBLE-FREES: `let e = q[1].r` where `q: Vec[Q]`, `struct Q { r: R, n: i64 }`… | a0005b8 |
| B-2026-08-28-36 | codegen | medium | A CONTAINER-ELEMENT TUPLE-INDEX READ WHOSE LEAF IS A WHOLE HEAP-CARRYING STRUCT LEAKS ITS CLONE when no destination adopts it: after B-2026-08-28-24… | 54ca269 |
| B-2026-08-28-37 | codegen | high | A TUPLE-INDEX LEAF read through a BORROWED place double-frees: `fn peek(t: ref Wt) -> String { return t.pair.0; }` where `struct Wt { pair: (String,… | 221cf4c |
| B-2026-08-28-38 | codegen | medium | A CLOSURE'S BY-VALUE STRUCT PARAM RUNS NO USER `Drop` BODY ON EITHER COMPILED BACKEND while the interpreter runs one: `let f = \|r: R\| { r.id };` and… | 7b30536 |
| B-2026-08-28-39 | interp | medium | A WHOLE-PATTERN WILDCARD DISCARD OF AN OWN-`Drop` ENUM OMITS THE PAYLOAD BODY IN THE INTERPRETER ONLY: `let _ = E.A(R { id: 41 });` prints `drop E` u… | 4997b9f |
| B-2026-08-28-40 | interp+codegen | medium | A WILDCARD DESTRUCTURE LEAF RUNS ONE `Drop` BODY WHERE THE SAME VALUE BOUND RUNS TWO, so a discarded aggregate's INNER object never gets its body: `l… | 6673474 |
| B-2026-08-28-41 | interp+codegen | medium | A PAYLOADLESS VARIANT OF AN OWN-`Drop` ENUM, DISCARDED BY `let _ =`, RUNS ZERO BODIES ON ALL THREE BACKENDS: `let _ = E.B;` where `impl Drop for E` p… | b75a1f0 |
| B-2026-08-28-42 | codegen | high | A HEAP FIELD READ OFF A FIELD-ROOTED CONTAINER IS NEVER CLONED AND DOUBLE-FREES: `h.xs[0].name` where `struct H { xs: Vec[R] }` and `struct R { id: i… | ed405c3 |
| B-2026-08-28-43 | interp+codegen | medium | TWO MORE SPELLINGS OF A DISCARDED PAYLOADLESS OWN-`Drop` ENUM VARIANT STILL RUN NO BODY: `let _ = B;` (the BARE spelling) is interp 1 / compiled 0, a… | d5dbdcb |
| B-2026-08-28-44 | codegen | low | TWO SHAPES OF THE B-2026-08-28-32 FAMILY STILL LEAK: a container-element heap read consumed by an unbound TUPLE LITERAL (`println((p[1].word, 9).0)`)… | 6ca7ed6 |
| B-2026-08-28-45 | interp+codegen | medium | A FRESH STRUCT LITERAL DESTRUCTURED IN THE SAME STATEMENT LOSES ITS WILDCARD FIELD'S `Drop` BODY on both compiled backends: `let W { e: _, n } = W {… | 05fefaa |
| B-2026-08-28-46 | interp+codegen | medium | A STRUCT HOLDING A UNIT VARIANT OF AN OWN-`Drop` ENUM DIVERGES IN THE INTERPRETER-SILENT DIRECTION when it is never destructured: `let w = W { e: E.B… | 9727cdb |
| B-2026-08-28-47 | interp+codegen | medium | A TUPLE HOLDING A UNIT VARIANT OF AN OWN-`Drop` ENUM RUNS NO BODY ON ANY BACKEND when it is never destructured: `let p = (E.B, 1); println(p.1)` is 0… | 9727cdb |
| B-2026-08-28-48 | interp | low | A DISCARDED PAYLOAD-BEARING ENUM CTOR IN BARE STATEMENT POSITION LOSES ITS PAYLOAD'S `Drop` BODY IN THE INTERPRETER: `E.A(R { id: 41 });` is interp 1… | 4190c0a |
| B-2026-08-28-49 | codegen | low | TWO RESIDUALS OF B-2026-08-28-20, both pre-existing and measured identical before and after that row's fix | 5885ba5 |
| B-2026-08-28-50 | codegen | medium | AN UNBOUND DESTRUCTURE FIELD WHOSE TYPE HAS NO `Drop` LEAKS ON A STRUCT-LITERAL SOURCE: `let W { r: _, n } = W { r: R { . | bf7c226 |
| B-2026-08-28-51 | interp+codegen | high | A CONDITIONALLY-MOVED LOCAL RETURNED FROM A BRANCH TAIL RUNS ITS `Drop` BODY TWICE AND IS THEN USED AFTER THE DROP, on all four surfaces: `fn take(k:… | 74223ee |
| B-2026-08-28-52 | interp+codegen | medium | ONE PROGRAM, WRONG IN OPPOSITE DIRECTIONS ON THE TWO BACKENDS: a local moved out by an explicit `return r` inside a branch (`fn take(k: bool) -> R {… | f94c75c |
| B-2026-08-28-53 | interp+codegen | low | A DISCARDED own-`Drop` parent temp runs its FIELD's `Drop` body BEFORE its own on the two compiled backends and AFTER it in the interpreter: `take(W… | 43e93bc |
| B-2026-08-28-54 | interp+codegen | medium | AN ENUM WITH NO OWN `Drop` BUT A DROP-BEARING PAYLOAD RUNS NO BODY ON ANY BACKEND when held as a struct field or tuple element: `enum E2 { A(R), B }`… | 0e872dd |
| B-2026-08-28-55 | interp+codegen | medium | A `Vec` ELEMENT OF AN OWN-`Drop` ENUM RUNS NO BODY ON ANY BACKEND: `let mut v = Vec.new(); v.push(E.B)` is 0/0/0 while the TUPLE element of the same… | 0ad7a39 |
| B-2026-08-28-56 | codegen | medium | A TUPLE-TYPED DESTRUCTURE LEAF WHOSE ELEMENT TYPE HAS NO `Drop` GETS NO MEMORY OWNER, on either source: `let (inner, n) = ((R { . | 3e4a63b |
| B-2026-08-28-57 | interp+codegen | medium | AN `Array[E, N]` OF AN OWN-`Drop` ENUM DIVERGES TWICE OVER: the interpreter runs both bodies IMMEDIATELY and the compiled backends run them at SCOPE… | e615cce |
| B-2026-08-28-58 | interp+codegen | medium | AN `Option[E]` HOLDING AN OWN-`Drop` ENUM RUNS NO BODY ON ANY BACKEND -- `let o: Option[E] = Some(E.B)` is 0/0/0 and the payload variant loses `dR` t… | 033eafc |
| B-2026-08-28-59 | codegen | medium | A NAMED TUPLE-DESTRUCTURE LEAF OF ENUM TYPE LOSES THE ENUM'S OWN `Drop` BODY ON BOTH COMPILED BACKENDS while the interpreter runs it: `let (gv, gn) =… | bde7f05 |
| B-2026-08-28-60 | codegen | low | AN UNDESTRUCTURED TUPLE LOSES THE INNER HEAP OF A NESTED STRUCT ELEMENT: `let p = ((R { tags: mkv(3) }, 2), 1); println(p.1)` where `R` declares no `… | 6c0a296 |
| B-2026-08-28-61 | codegen | medium | A GENERIC CALLEE'S tuple element still loses its Drop-bearing field's body when destructured at the call site: `fn src[T](x: T) -> (Box2[T], i64)` wi… | dc01eb1 |
| B-2026-08-28-62 | interp+codegen | medium | A BY-VALUE PARAM THAT ESCAPES THROUGH A CALL in return position runs its `Drop` body TWICE on all three backends: `fn outer(y: R) -> (BoxR, i64) { re… | 5ce2a57 |
| B-2026-08-28-63 | interp+codegen | medium | A CONSUMING `match` / `if let` ARM THAT BINDS AN ENUM PAYLOAD OUT RUNS NO `Drop` BODY FOR IT ON ANY BACKEND, while the same arm binding a STRUCT payl… | f862656 |
| B-2026-08-28-64 | codegen | medium | `Option[K]` NEVER FREES A HEAP-CARRYING NESTED-STRUCT PAYLOAD while `Result[K, i64]` DOES: the plain `let` site calls `track_inline_result_payload_va… | 77729f2f |
| B-2026-08-28-65 | codegen | medium | A `return <local>` NESTED IN A BRANCH LOSES THE LOCAL'S `Drop` BODY ON THE PATH THAT NEVER TAKES THE RETURN, on all three COMPILED backends while the… | f94c75c |
| B-2026-08-28-66 | codegen | high | A CONSUMING ARM THAT HANDS ITS BOUND STRUCT PAYLOAD ON RUNS THAT PAYLOAD'S `Drop` BODY TWICE ON BOTH COMPILED BACKENDS -- `let k = match o { Some(r)… | 0e6f200 |
| B-2026-08-28-67 | interp+codegen | low | A `match` OVER A BARE USER ENUM RUNS THE PAYLOAD'S `Drop` BODY AND THE ENUM'S OWN IN OPPOSITE ORDERS ON THE TWO BACKENDS -- `match e { E.A(r) => . | dcf4891 |
| B-2026-08-28-68 | codegen | medium | A `Result` WITH ONE SIDE BOXED LOSES THE OTHER SIDE'S INLINE PAYLOAD WALK: `track_inline_result_payload_var`'s name-keyed `boxed_enum_payload_vars` e… | c886f1e |
| B-2026-08-28-69 | interp+codegen | medium | A DISCARDED `match` WHOSE ARM YIELDS ITS BOUND PAYLOAD RUNS NO `Drop` BODY IN THE INTERPRETER while both compiled backends run exactly one -- `match… | 16a8791 |
| B-2026-08-28-70 | interp+codegen | medium | A CONDITIONALLY-RETURNED owned param of a METHOD loses its `Drop` body IN THE INTERPRETER while all three compiled backends run it -- and the callee-… | 277621a |
| B-2026-08-28-71 | codegen | medium | A GENERIC callee's monomorph param slot does not hold what a bodies-only user-`Drop` walker expects: registering B-2026-08-28-22's conditionally-retu… | 907b378 |
| B-2026-08-28-72 | codegen | medium | A `match` THAT IS A FUNCTION'S RETURNED VALUE RUNS THE YIELDED PAYLOAD'S `Drop` BODY TWICE ON BOTH COMPILED BACKENDS -- `fn take(o: Option[R]) -> R {… | 5746b82 |
| B-2026-08-28-73 | codegen | high | REGRESSION from 77729f2 (NOT f862656, as first filed): A CONSUMING ARM THAT HANDS ITS BOUND BOXED ENUM PAYLOAD OUT AS THE MATCH'S VALUE DOUBLE-FREES… | 1caf601 |
| B-2026-08-28-74 | codegen | medium | THREE REAL LEAKS THE DEFAULT LSan CONFIGURATION HIDES: a `shared` ENUM broken out of a `loop` (32 B x 3 fixtures, `pick`) and a tensor ref-return (72… | 0ba1f22 |
| B-2026-08-28-75 | codegen | medium | REASSIGNING AN `Option`/`Result` BINDING WHOSE BOXED PAYLOAD ARRIVED BY A WHOLE-VALUE MOVE FROM ANOTHER BINDING ORPHANS THE PAYLOAD BOX ON EVERY STOR… | d5c4606 |
| B-2026-08-29-1 | codegen | medium | A BARE `String` / `Vec` PAYLOAD INSIDE AN `Option` OR `Result` NEVER HAS ITS BUFFER FREED WHEN THE BINDING IS REASSIGNED -- `let mut vv: Option[Strin… | 98bddee |
| B-2026-08-29-2 | codegen | medium | A DOUBLY-NESTED `Option` OVER A HEAP PAYLOAD LEAKS THAT PAYLOAD ON PLAIN SCOPE EXIT -- `let vv: Option[Option[String]] = Some(Some(s));` and nothing… | c876e64 |
| B-2026-08-29-3 | codegen | medium | A GENERIC method that UNCONDITIONALLY returns its owned param still runs the param's `Drop` body TWICE on all three compiled backends against once in… | 7f5d6f9 |
| B-2026-08-29-4 | codegen | high | A METHOD that hands an inline `Option`/`Result` argument back DOUBLE-FREES the payload on all three compiled backends (glibc abort, valgrind `Invalid… | 8d8d1b4 |
| B-2026-08-29-5 | codegen | medium | A DISCARDED BRANCH CONSTRUCT STRANDS WHATEVER ITS TAKEN ARM HANDS OUT -- `if let Some(s) = vv { s };` in statement position leaks the whole 38 B buff… | a86e5d1 |
| B-2026-08-29-6 | codegen | high | A passthrough call used DIRECTLY AS A MATCH SCRUTINEE (`match take(s) { . | d1db93d |
| B-2026-08-29-7 | codegen | high | An `Option` MATCH ARM THAT REBINDS ITS HEAP PAYLOAD TO A LOCAL AND RETURNS IT IS A DOUBLE FREE ON BOTH COMPILED BACKENDS -- `match b { Some(r) => { l… | 905d73f |
| B-2026-08-29-8 | codegen | medium | A MATCH ARM THAT REBINDS ITS PAYLOAD TO A LOCAL AND YIELDS IT AS THE ARM'S BLOCK TAIL RUNS THE `Drop` BODY TWICE ON BOTH COMPILED BACKENDS -- `Box2.F… | 077a6bd |
| B-2026-08-29-9 | interp | high | REGRESSION (277621a): A METHOD that RETURNS a payload bound out of its owned enum / `Option` param now runs that payload's `Drop` BODY TWICE IN THE I… | 1167db7 |
| B-2026-08-29-10 | interp+codegen | medium | A METHOD whose owned `Option[T]` param has its payload bound out and NOT returned MISSES the payload's `Drop` body under CODEGEN -- interp `drop 7`/`… | 28ab271 |
| B-2026-08-29-11 | interp | medium | The interpreter's method frames SHARE one moved-out name space, so one method's legitimately-marked param suppresses an unrelated LATER method's iden… | b04cf10 |
| B-2026-08-29-12 | codegen | medium | A `Tensor` FIELD OF A STRUCT IS NEVER FREED -- 72 B per struct, with no ref return, no method and no read of the field anywhere; the parent row's "te… | 4b0a560 |
| B-2026-08-29-13 | codegen | medium | A `match` ARM THAT YIELDS A SHARED CHILD INTO A BINDING LEAKS THAT CHILD'S BOX -- `let keep = match t { Bin(l, r) => l };` over a recursive `shared e… | 9e337a9 |
| B-2026-08-29-14 | codegen | medium | A METHOD that returns its owned param with `return r;` (rather than as a BLOCK TAIL) runs the param's `Drop` body TWICE under codegen for a FRESH-TEM… | 7df22de |
| B-2026-08-29-15 | interp+codegen | medium | A NAMED struct binding passed by value to a function that returns it runs the `Drop` body TWICE on all four surfaces -- for a FREE FUNCTION as well a… | baf2338 |
| B-2026-08-29-16 | interp+codegen | medium | A GENERIC method that returns its owned param CONDITIONALLY is wrong in BOTH directions and diverges both ways -- param dies: interp 0 bodies / compi… | 907b378 |
| B-2026-08-29-17 | interp+codegen | medium | ALL THREE BACKENDS run a match-arm payload's `Drop` body TWICE when the arm REBINDS it to a local that does NOT escape -- `Box2.Full(r) => { let m =… | 8bc9955 |
| B-2026-08-29-18 | codegen | medium | THE OUTER-`Result` SPELLING OF THE NESTED-ENVELOPE LEAK: `Result[Option[Wide], i64]` and `Result[Option[String], i64]` still strand the innermost pay… | c794214 |
| B-2026-08-29-19 | interp+codegen | medium | ALL THREE BACKENDS run a `Drop` body TWICE when a match-arm payload is rebound, RE-WRAPPED into a fresh enum and matched again -- `Full(r) => { let m… | a865ed6 |
| B-2026-08-29-20 | interp+codegen | medium | THE `let _ = match …` SPELLING OF A DISCARDED MATCH RUNS NO `Drop` BODY IN THREE CELLS while its call twin (`let _ = mk();`) is correct on both backe… | e88b7bf |
| B-2026-08-29-21 | codegen | medium | A NON-GENERIC method whose owned param is returned CONDITIONALLY, with BOTH exits spelled as `return` statements, runs the param's `Drop` body TWICE… | 64e3e90 |
| B-2026-08-29-22 | runtime | high | EVERY COMPILED macOS BINARY THAT RECORDS AN ERROR-RETURN-TRACE FRAME ABORTS AT EXIT -- SIGABRT / exit 134 and a raw Rust panic (`use of std::thread::… | 7a8107c2 |
| B-2026-08-29-23 | codegen | medium | EVERY SCALAR `bf16` OPERATION ABORTED ISel ON arm64 AND wasm32 -- `LLVM ERROR: Cannot select`, exit 134, no program output | 7ba9c46 |
| B-2026-08-29-24 | interp+codegen | medium | A param VIEW wrapped into a MIXED-payload enum, or into a STRUCT / TUPLE / VEC / `Some`, still runs its `Drop` body TWICE -- `let w = W2.Two(r, R { i… | 2ce3507 |
| B-2026-08-29-25 | interp+codegen | medium | A DISCARDED `if` RUNS NO `Drop` BODY IN EITHER STATEMENT FORM -- `if c { R { . | fd4e80f |
| B-2026-08-29-27 | codegen | medium | A VALUE-POSITION BLOCK OR BRANCH WHOSE TAIL MINTS A FRESH OWNED TEMP LEAKS IT WHEN THE CONSUMER IS A METHOD RECEIVER OR A CALL ARGUMENT -- `if c { mk… | 707912b |
| B-2026-08-29-28 | interp+codegen | low | A FRESH-TEMP ENUM SCRUTINEE'S OWN `Drop` BODY RUNS AT THE MATCH ON THE INTERPRETER AND AT THE END OF THE ENCLOSING SCOPE ON BOTH COMPILED BACKENDS --… | 501b70d |
| B-2026-08-29-29 | interp+codegen | medium | A PROJECTION-PLACE ENUM SCRUTINEE RUNS ITS PAYLOAD'S `Drop` BODY TWICE ON BOTH COMPILED BACKENDS, THE SECOND TIME ON THE ZEROED SLOT -- `match s.e {… | af86dfd |
| B-2026-08-29-30 | interp+codegen | medium | A DISCARDED `if` WITH NO `else` RUNS NO `Drop` BODY AND LEAKS -- `let _ = if n == 1 { R { . | 4a8397f3 |
| B-2026-08-29-31 | interp+codegen | medium | THE `let _ =` SPELLING OF A DISCARDED BRANCH STILL STRANDS WHATEVER ITS ARM HANDS OUT -- B-2026-08-29-5 fixed the BARE-STATEMENT form, and the wildca… | 5d6a9a7e |
| B-2026-08-29-32 | codegen | medium | A DISCARDED BRANCH OR MATCH WHOSE ARMS ARE LITERALS OF A STRUCT WITH HEAP FIELDS BUT NO `impl Drop` LEAKS THEM -- `if c { P { a: payload(), b: 1 } }… | 79458f1c |
| B-2026-08-29-33 | interp+codegen | medium | A MATERIALIZING ARM OVER A PROJECTION-PLACE ENUM SCRUTINEE RUNS THE PAYLOAD'S `Drop` BODY TWICE ON EVERY BACKEND -- `match s.e { E.A(r) => { let m =… | 9f3e2cb |
| B-2026-08-29-34 | codegen | medium | A `Vector[bf16, N]` LANE OP ABORTS ISel ON arm64 AND wasm32 -- the vector legalizer SCALARIZES `<4 x bfloat> fadd` back into the unselectable scalar… | e860b90 |
| B-2026-08-29-35 | codegen | high | THE THREE `let`-FORM LEGS NEVER RAN THE PROJECTION-PLACE PAYLOAD SUPPRESSOR, so `if let E.A(r) = s.e { let m = r; . | 9f3e2cb |
| B-2026-08-29-36 | codegen | low | A TWO-HOP PROJECTION (`match w.s.e { . | 4843ffe7 |
| B-2026-08-29-37 | codegen | medium | A `ref self` METHOD WHOSE MATCH MOVES `self.e`'s PAYLOAD OUT RUNS THE ENUM'S OWN `Drop` BODY TWICE ON BOTH COMPILED BACKENDS -- `dR8 dE n8 dE dR8` vs… | fddafe3 |
| B-2026-08-29-38 | codegen | medium | A method's FRESH-TEMP argument whose value is handed back out runs its `Drop` body TWICE on both compiled backends against once in the interpreter --… | fc379153 |
| B-2026-08-29-39 | ownership | medium | A BINDING INITIALIZED TO `None` IS CLASSIFIED AS NON-RC FOR THE REST OF ITS LIFE, so reusing an `Option[shared T]` after passing it by value is repor… | c793ad0 |
| B-2026-08-29-42 | codegen | high | A REDUCED-PRECISION RECEIVER (`f16` / `bf16`) CALLED THE DOUBLE-PRECISION libm SYMBOL and got silent garbage -- `x.tan()` returned `x`, `x.hypot(y)`… | e860b90 |
| B-2026-08-29-40 | interp | medium | The INTERPRETER's `Vector[bf16, N]` unary math never rounds lanes back to the element width -- it computes every lane in f64 and keeps the excess pre… | 5f226ed |
| B-2026-08-29-41 | interp | low | The INTERPRETER computes `to_degrees` / `to_radians` / `cosh` at f64 and returns an f64-precision value whatever the receiver's width, so all three d… | 7c552c7 |
| B-2026-08-29-43 | interp+codegen | medium | A MIXED STRUCT LITERAL over a struct with its OWN `impl Drop` still runs a param view's `Drop` body TWICE -- `let s = Sd3 { a: r, b: R { id: 2 } }` p… | 131957b3 |
| B-2026-08-29-44 | interp+codegen | medium | A WHOLE-VALUE REBIND AFTER A MIXED WRAP RE-ARMS THE WALK THE MASK JUST WITHHELD -- `let w = W2.Two(r, R { id: 2 }); let w2 = w;` prints `dR1 dR2 dR1`… | 6bf48fbc |
| B-2026-08-29-45 | interp+codegen | medium | A BINDING MOVED INTO A `Vec` LITERAL RUNS ITS `Drop` BODY TWICE -- both `let v = [r]` (param) and `let m = R { . | aa33534 |
| B-2026-08-29-46 | interp+codegen | medium | TWO OWNED PARAMS' `Drop` BODIES RUN IN OPPOSITE ORDER ON THE TWO BACKENDS -- `fn take(r: R, q: R) -> i64 { 7 }` called with two fresh temps prints `d… | 33f8781 |
| B-2026-08-29-47 | interp+codegen | medium | MOVING A PARAM VIEW BACK OUT OF THE STRUCT IT WAS JUST WRAPPED INTO DOUBLES ITS `Drop` BODY -- `let s = S { r: r }; let x = s.r;` prints `dR1 dR1` wh… | eec415a4 |
| B-2026-08-29-48 | interp+codegen | medium | An arm that ASSIGNS the payload to an outer binding and returns THAT later runs the payload's `Drop` body TWICE -- on all four surfaces, so no A/B ga… | d135d02 |
| B-2026-08-29-49 | interp+codegen | medium | An argument the callee STORES into an outliving place (`fn take(sink: mut ref Vec[Res], r: Res) { sink.push(r); }`) runs the value's `Drop` body ONE… | 0343db57 |
| B-2026-08-29-50 | interp+codegen | medium | A param moved into a RETURNED AGGREGATE (`fn wrapf(r: Res) -> Hh { Hh { r: r } }`), and a CONDITIONALLY-returned param on BOTH its paths, still run t… | 7b23317 |
| B-2026-08-29-51 | codegen | low | A self-assignment whose RHS is a BLOCK ending in a passthrough call leaks the argument's heap -- `e = { let z = 1; pass(e) };` loses the `String` (13… | 2378582 |
| B-2026-08-29-52 | codegen | high | Printing a `Vector[T, N]` gives THREE DIFFERENT ANSWERS -- the interpreter renders the lanes, the JIT prints a raw POINTER (the same one for two diff… | 60465148 |
| B-2026-08-29-53 | codegen | medium | Compiled `Vector[f16, N].tanh()` returns NaN for x above ~5.5 -- the exp-derived formula computes e^(2x), which overflows f16's exponent range | 2e5490d |
| B-2026-08-29-54 | codegen | high | A STATIC (associated) FUNCTION'S FRESH-TEMP ARGUMENTS RUN NO `Drop` BODY AT ALL ON THE COMPILED BACKENDS -- `impl H { fn s2(a: R, b: R) -> i64 { 7 }… | 5962bb9 |
| B-2026-08-29-55 | codegen | medium | ARGUMENT TEMPORARIES ARE HELD TO THE END OF THE STATEMENT ON THE COMPILED BACKENDS INSTEAD OF DYING WHEN THE CALL RETURNS -- `take(R { id: 1 }, R { i… | 4b73d1e2 |
| B-2026-08-29-56 | codegen | high | A HEAP-CARRYING `Option` BOUND TO A LOCAL AND RETURNED IS FREED TWICE WHEN THE CALLER UNWRAPS IT -- `fn collect() -> Option[String] { let buf = Some(… | bc1c37c |
| B-2026-08-29-57 | interp | medium | `return out` as a block's FINAL EXPRESSION -- no trailing semicolon -- makes the INTERPRETER run the returned local's `Drop` body TWICE, once at call… | fc450fe |
| B-2026-08-29-58 | interp | medium | A `match` arm that ASSIGNS the param's payload to an outer local the callee does NOT return runs the payload's `Drop` body ONE TIME TOO MANY on the i… | e7c7d44 |
| B-2026-08-29-60 | interp+codegen | low | `asinh` / `acosh` / `atanh` diverge between `run` and `build` at f64 as well as f32 -- Rust std implements the inverse hyperbolics as formulas rather… | cc4f0c6 |
| B-2026-08-29-61 | codegen | medium | ONE AOT BINARY RETURNS TWO ANSWERS FOR THE SAME VALUE: `karac build` constant-folds a compile-time-known f32 `cosh`/`sinh`/`log10` in double precisio… | d95d6f7 |
| B-2026-08-29-63 | codegen | medium | Passing an own-heap struct BY VALUE deep-copies its heap fields at every call, even though the argument is MOVED -- measured at exactly N*8 extra byt… | 8b0ad9e |
| B-2026-08-29-64 | ownership | low | design.md's REPL section states use-after-move is a blocking `error[E0382]` with "strictness identical to compiled code"; the compiler emits code E05… | 2f8aa27 |
| B-2026-08-29-65 | interp+codegen | medium | A PARAM returned by a TAIL `return r` -- no trailing semicolon -- runs its `Drop` body TWICE on EVERY backend, while the same function written `retur… | fc450fe |
| B-2026-08-29-66 | codegen | high | A struct WRAPPING a `Vec` of `Drop` elements, built in a callee and returned, runs the element's `Drop` body on FREED memory in the compiled backends… | fa0f33fe |
| B-2026-08-29-67 | codegen | medium | A `Vec[T]` of `Drop` elements returned from a callee runs the element's `Drop` body BEFORE the caller's first use of the vector on the compiled backe… | fa0f33fe |
| B-2026-08-30-1 | cli+codegen | high | The DEFAULT (JIT) REPL lane silently discards every cross-cell mutation -- `let mut n: i64 = 0;` then `n = 5;` reads back 0, a String reads back EMPT… | 80bb5ec6 |
| B-2026-08-30-2 | codegen | medium | A VALUE-POSITION BLOCK OR BRANCH WHOSE TAIL HANDS OUT A BINDING RATHER THAN MINTING A TEMP LEAKS IT, AND CANNOT BE FIXED AT THE CONSUMER -- `{ loc }.… | c935db1 |
| B-2026-08-30-3 | codegen | low | A VALUE-POSITION BRANCH WHOSE ARM TAIL IS AN F-STRING LEAKS THE ACCUMULATOR -- `if c { f"x{n}" } else { f"y{n}" }.contains("x")` and the call-argumen… | a0619a88 |
| B-2026-08-30-4 | interp+codegen | low | `cbrt` can now be admitted to the float-math table -- the libm-shim mechanism added by B-2026-08-29-60 removes the exact `run == build` obstacle that… | f8045545 |
| B-2026-08-30-5 | codegen | medium | `to_degrees` / `to_radians` on an `f16` receiver disagree between the interpreter and every compiled surface on ~25% of inputs -- codegen rounds 180/… | 42e103d |
| B-2026-08-30-6 | codegen | high | A bisection over NEGATIVE bounds miscompiles: `(lo + hi) / 2` truncates toward zero, so the midpoint recognizer's `assume(mid < hi)` is `assume(false… | 7a10c4e |
| B-2026-08-30-7 | cli+codegen | medium | The JIT REPL's B.5.3d PASS-THROUGH silently REVERTS every mutation to a `let mut` binding whose type is not snapshot-eligible, and re-runs its RHS (s… | d825ba93 |
| B-2026-08-30-8 | codegen | high | A CONSUMED-THEN-REASSIGNED-THEN-RETURNED `Option` LOCAL IS FREED TWICE -- a `match` arm that binds the payload out disarms the binding's scope-exit f… | 10018b3 |
| B-2026-08-30-9 | interp | medium | The INTERPRETER renders an unsigned `Vector` lane above `i64::MAX` as a SIGNED reinterpretation -- `Vector[u64, 2]` holding `u64::MAX` prints `Vector… | 69708f4 |
| B-2026-08-30-10 | ownership | medium | A READ-ONLY `match` ARM IS REPORTED AS A MOVE OF ITS SCRUTINEE, so an `Option[String]` read once and then returned draws `value 'b' moved here, used… | ff99609 |
| B-2026-08-30-11 | codegen | medium | THE MINTING ARM OF A MIXED BRANCH HAS NO OWNER EITHER -- `if c { mkA(n) } else { t }.contains("aaa")` strands the 27 B `mkA` temp on the `c` path whi… | 1c0b4d5f |
| B-2026-08-30-12 | codegen | medium | A VALUE-POSITION BLOCK WHOSE TAIL NAMES A BINDING IT DECLARED ITSELF LEAKS IT IN A READ-ONLY CONSUMER -- `{ let t = mkB(n); t }.contains("bbb")` stra… | cdb844d4 |
| B-2026-08-30-13 | codegen | low | AN ASSIGNMENT WHOSE RHS IS A VALUE-POSITION BLOCK DOES NOT FREE THE VALUE IT OVERWRITES AT -O0 -- `let mut s = mkA(n); s = { t };` strands 15 B while… | bd00968b |
| B-2026-08-30-14 | interp | high | THE INTERPRETER SILENTLY DISCARDS `return` / `break` / `continue` INSIDE A MATCH ARM WHEN THE SCRUTINEE IS A FRESH TEMP WHOSE ENUM HAS ITS OWN `impl… | facec84 |
| B-2026-08-30-15 | codegen | medium | A FRESH-TEMP STRUCT SCRUTINEE'S OWN `Drop` BODY NEVER RUNS AT ALL ON EITHER COMPILED BACKEND -- `match mkS() { S { r: r } => . | ed13ce30 |
| B-2026-08-30-16 | codegen | low | A FRESH-TEMP `shared enum` SCRUTINEE'S OWN `Drop` BODY STILL RUNS AT THE ENCLOSING SCOPE'S EXIT -- `match mkSe() { . | 1de3bd88 |
| B-2026-08-30-19 | codegen | high | THE STRUCT-PAYLOAD RESIDUAL OF B-2026-08-08-25: a read-only `match` arm over an `Option[S]` / `Result[S, E]` whose STRUCT payload carries heap still… | 4f02313 |
| B-2026-08-30-20 | interp+codegen | medium | AN ARGUMENT PRODUCED BY AN ASSOCIATED CALL HAS NO OWNER ON ANY BACKEND -- `s1(H.mkr(1))` where `H.mkr` is an `impl` block's associated fn runs NO `Dr… | f1bb1102 |
| B-2026-08-30-21 | codegen | medium | AN ASSOCIATED CALL RETURNING AN ALL-SCALAR STRUCT RECORDS NO TYPE FOR ITS RESULT: a field read off it is a HARD `karac build` failure (`cannot resolv… | f1bb1102 |
| B-2026-08-30-22 | interp | medium | THE INTERPRETER RUNS AN EXTRA `Drop` BODY FOR AN ASSOCIATED-FN PASSTHROUGH ARGUMENT -- `let x = H.id(R { . | 491dd0e5 |
| B-2026-08-30-24 | codegen | medium | NO `f16` PROGRAM LINKS FOR A WASM TARGET: LLVM lowers `half` to the `__extendhfsf2` / `__truncsfhf2` compiler-rt builtins and the wasm link line has… | 240919c |
| B-2026-08-30-25 | typecheck | medium | THE TYPECHECKER ACCEPTS ANY METHOD NAME ON AN `f16` / `bf16` RECEIVER -- `karac check` prints "All checks passed." on `let s: String = a.completely_b… | d4cda14 |
| B-2026-08-30-26 | codegen | medium | An integer <-> `bf16` conversion is refused by BOTH compiled backends in every direction (`internal error: codegen emitted a native bfloat SIToFP/UIT… | 532549e |
| B-2026-08-30-27 | codegen | low | A loop-invariant DIRECT-LIBM float method (`cosh`/`sinh`/`tan`/`asin`/...) is not hoisted out of a loop, while the INTRINSIC-family ones (`log10`/`ex… | 9b5fce2 |
| B-2026-08-30-28 | interp+codegen | medium | A CONDITIONAL store of an owned param into a `mut ref` param -- `if c { sink.push(r); }` -- LOSES the payload's `Drop` body on the path where the sto… | fd205752 |
| B-2026-08-30-29 | effect | high | THE EFFECT CHECKER DOES NOT ENFORCE design.md's "Drop bodies must not panic" RULE -- an `impl Drop` whose body calls `panic()`, indexes out of bounds… | 198b899 |
| B-2026-08-30-30 | resolver+interp | medium | `process.exit(code)` WORKS ON BOTH COMPILED BACKENDS AND RAISES AN INTERNAL "this is a compiler bug" RUNTIME ERROR ON THE INTERPRETER -- the interpre… | 198b899 |
| B-2026-08-30-31 | codegen | high | AUTO-PAR RUNS `process.exit(code)` BEFORE THE `println`s THAT PRECEDE IT WHENEVER ANY STATEMENT FOLLOWS THE EXIT, silently discarding ALL prior stdou… | 198b899 |
| B-2026-08-30-32 | codegen | medium | LLVM-18 PROMOTES f16 ARITHMETIC AND MATH METHODS TO f32 ON wasm32 AND NEVER ROUNDS THE RESULT BACK, so a wasm module holds values the type cannot rep… | c1a9f3e |
| B-2026-08-30-33 | interp+codegen | medium | A by-value param with TWO exits -- conditionally STORED into a `mut ref` place and also RETURNED on another path -- runs NO `Drop` body on the path w… | f77f6a4b |
| B-2026-08-30-34 | interp | medium | A `u64` value >= 2^63 converts to EVERY float type as its negative two's-complement under the interpreter (`u64::MAX as f64` prints -1) while all thr… | 74f6989 |
| B-2026-08-30-35 | effect | high | AN EXPRESSION INSIDE AN f-STRING INTERPOLATION HOLE CONTRIBUTES NO EFFECTS TO INFERENCE -- `println(f"{touch()}")` infers [writes(Stdout)] where the… | ae33af9 |
| B-2026-08-30-36 | typecheck | medium | `f16` AND `bf16` IMPLEMENT NONE OF THE SIX ARITHMETIC OPERATOR TRAITS (`Add`/`Sub`/`Mul`/`Div`/`Rem`/`Neg`), so neither can satisfy a generic `T: Add… | ea9d378 |
| B-2026-08-30-37 | effect | medium | TWO MORE EFFECT WALKERS TREAT AN f-STRING AS A LEAF, and `ae33af9` fixed only the third -- `modbind_synth::walk_expr` loses a module-binding read wri… | 7366410 |
| B-2026-08-30-38 | interp+codegen | medium | AN ARGUMENT THAT IS A CONTROL-FLOW OR BLOCK EXPRESSION LOSES ITS `Drop` BODY ENTIRELY -- `one(if c { mk(1) } else { mk(2) })`, the `match` spelling a… | 0d34440b |
| B-2026-08-30-39 | codegen | medium | `dbg` OF A `Vector`, AND ANY CONTAINER-NESTED `Vector` IN AN F-STRING, PANIC THE COMPILER -- `emit_display_fn_for_type: type_name 'Vector_u64_2' not… | 57327e8 |
| B-2026-08-30-40 | typecheck | low | THE `f16.` / `bf16.` ASSOCIATED-CALL NAMESPACE IS UNREACHABLE: `f16.add(a, b)`, `f16.from(x)` and `f16.parse(s)` are all rejected with "'f16' is a ty… | d64f6e4 |
| B-2026-08-30-41 | interp+codegen | medium | A `T: PartialOrd` BOUND MAKES `a < b` UNRUNNABLE ON BOTH BACKENDS, FOR EVERY TYPE — the tree-walk aborts with "method 'partial_cmp' not found" and `k… | dc6f56f |
| B-2026-08-30-42 | codegen | medium | A `bf16` PARAMETER ON A FUNCTION BOUNDARY LLVM DOES NOT INLINE ABORTS THE WASM BUILD WITH `LLVM ERROR: Cannot select: bf16_to_fp` — a hard process ab… | cd4e081 |
| B-2026-08-30-43 | interp | medium | A METHOD ON A GENERIC `impl` COMPUTES AT f64 UNDER `--interp` AT EVERY NARROW FLOAT WIDTH — `impl[T: Add + Mul] Pair[T] { fn combine(...) -> T }` at… | 8245dc9 |
| B-2026-08-30-44 | interp | medium | AN UNSIGNED VALUE PRINTS AS `-1` FROM INSIDE A GENERIC BODY UNDER `--interp` — `fn show[T](x: T) -> String { f"{x}" }` renders `u64::MAX` and `u128::… | d1ec5de |
| B-2026-08-30-45 | codegen | medium | `i128` AND `u128` SHARE ONE MONOMORPH SYMBOL, so a generic instantiated at both computes the second at the first's signedness — `show[T](x) { f"{x}"… | da21408 |
| B-2026-08-30-46 | interp | medium | `dbg()` OF A `Set` / `SortedSet` / `SortedMap` RENDERS A `u64` ELEMENT AT OR ABOVE 2^63 AS ITS NEGATIVE under `--interp`, while `f"{s}"` on the SAME… | 0d25859 |
| B-2026-08-30-47 | codegen | high | A READ-ONLY `match` ARM OVER AN `Option`/`Result` WHOSE PAYLOAD IS A HEAP-OWNING NON-STRUCT STILL TAKES THE PAYLOAD -- six shapes, three failure mode… | 29fd6f1 |
| B-2026-08-30-48 | interp | medium | AN INTEGER REACHING A FLOAT SLOT THROUGH AN AGGREGATE OR A VARIANT PAYLOAD IS STILL CONVERTED WRONG UNDER `--interp` -- a tuple / `Array` literal ele… | 42112cb |
| B-2026-08-30-49 | codegen | high | BOTH COMPILED BACKENDS PRODUCE `0` FOR AN INTEGER COERCED INTO A FLOAT-TYPED `if`/`else` OR `match` BRANCH, FOR ANY VALUE AND AT BOTH f32 AND f64 --… | b554da2 |
| B-2026-08-30-50 | interp+codegen | medium | A MIXED CONTROL-FLOW ARGUMENT LOSES THE FRESH BRANCH'S `Drop` BODY when that branch is the one taken -- `one(if false { k } else { mk(31) })` runs `k… | 76677be1 |
| B-2026-08-30-51 | interp | high | INTERPRETER: A SHADOWED BINDING'S `Drop` BODY NEVER RUNS AND THE SHADOWING VALUE'S RUNS TWICE -- `let t = mk(1); let t = mk(2);` prints `dR2 dR2` und… | 07dc9e76 |
| B-2026-08-30-52 | codegen | high | A READ-ONLY ARM THAT DESTRUCTURES OR NESTS OVER AN `Option`/`Result` PAYLOAD TOOK ITS HEAP -- FULLY FIXED | 022f07c |
| B-2026-08-30-55 | interp | medium | A METHOD frame does not participate in the owned-ENUM-param ownership protocol: a fresh-temp enum argument runs ZERO `Drop` bodies on the interpreter… | 3a09311 |
| B-2026-08-30-56 | codegen | high | BOTH COMPILED BACKENDS BITCAST AN INTEGER INTO AN `Option` / `Result` PAYLOAD AND AN ENUM STRUCT-VARIANT FLOAT FIELD, so `let o: Option[f64] = Some(m… | b9500f80 |
| B-2026-08-31-1 | interp+codegen | medium | A MATCH ARM THAT MOVES AN OWNED STRUCT PARAM'S ENUM PAYLOAD INTO A FRESH LOCAL RUNS THE PAYLOAD'S `Drop` BODY TWICE UNDER `--interp` AND ONCE ON BOTH… | e59a31b |
| B-2026-08-30-57 | codegen | medium | CODEGEN MISHANDLES A SHADOWED BINDING'S `Drop` BODY in two shapes -- a BLOCK-LOCAL shadowed binding (`one({ let t = mk(90); let t = mk(91); t })`) lo… | dd229de4 |
| B-2026-08-31-5 | codegen | medium | REGRESSION from 07dc9e76 -- a NEVER-READ shadowed binding's `Drop` body moves to FUNCTION EXIT on the compiled backends when any later `Vec` binding… | 9eeb04a2 |
| B-2026-08-31-8 | interp | medium | A STRUCT-VARIANT PATTERN OVER AN OWNED ENUM PARAM LOSES BOTH `Drop` BODIES UNDER `--interp` -- `b3` against `b3 dSv dR3` on all three compiled surfac… | 95fbe26 |
| B-2026-08-31-9 | codegen | medium | A STRUCT FIELD OF `Vector[T, N]` TYPE HAS NO DERIVED-Display ARM UNDER `karac build`, AND THE REFUSAL DUMPS A RUST `{:?}` OF THE FIELD'S TypeExpr --… | 86c9957 |
| B-2026-08-31-10 | codegen | medium | AN `Option` WHOSE PAYLOAD LOWERS TO A MULTI-WORD STRUCT CANNOT BE INTERPOLATED IN AN F-STRING UNDER `karac build`, AND THE ERROR MISDESCRIBES ITS OWN… | da529e8 |
| B-2026-08-31-11 | codegen | medium | A GENERIC CALLING ANOTHER GENERIC LOSES THE UNSIGNED READING ON BOTH COMPILED BACKENDS — `wrap[T](x) { show(x) }` prints `u64::MAX` as `-1` from JIT… | d87ae4f |
| B-2026-08-31-12 | interp+codegen | medium | A `u64` FIELD READ INSIDE A GENERIC `impl` PRINTS AS `-1` ON ALL THREE BACKENDS — `impl[T] Box[T] { fn get(ref self) -> String { f"{self.v}" } }` at… | 17279ac |
| B-2026-08-31-13 | typecheck | low | THE VALUE-RECEIVER SPELLING `x.partial_cmp(y)` IS REJECTED ON A CONCRETE RECEIVER WITH "expects 2 argument(s), found 1" WHILE THE IDENTICALLY-REGISTE… | 7bd0b04 |
| B-2026-08-31-14 | codegen | high | A BORROW-MODE PAYLOAD BINDING NAME STAYED REGISTERED FOR EVERY LATER MATCH IN THE SAME FUNCTION -- `borrowed_agg_payload_struct_vars` is keyed by BIN… | c64cbfd |
| B-2026-08-31-15 | typecheck | medium | A COMPARISON METHOD ON A PRIMITIVE RECEIVER IS SILENTLY POISONED, NOT CHECKED — `let s: String = n.cmp(m)` type-checks and PRINTS `Less` into the Str… | 26e3508 |
| B-2026-08-31-16 | codegen | low | THE DERIVED-Display REFUSAL FOR AN UNSUPPORTED FIELD TYPE DUMPS A RUST `{:?}` OF THE FIELD'S TypeExpr — spans and byte offsets included — INTO USER-F… | 64194650 |
| B-2026-08-31-18 | codegen | high | A `Vector[T, N]` OR `Array[T, N]` ENUM PAYLOAD RECONSTRUCTS AS GARBAGE UNDER CODEGEN -- silently, in `match` as well as in Display, so a bound payloa… | 527bdf8 |
| B-2026-08-31-19 | codegen | medium | `Array[T, N]` HAS NO Display UNDER CODEGEN AT ANY DEPTH, AND THE QUIET HALF IS THE PLAIN ONE -- nested (a `Vec` element, a struct field, a tuple fiel… | c48c0fb |
| B-2026-08-31-17 | codegen | medium | AN `Option`/`Result` CALL RESULT INTERPOLATED IN AN F-STRING LEAKS ITS PAYLOAD ONCE PER EVALUATION -- `f"{mk(n)}"` where `mk` returns `Option[String]… | 4a58370 |
| B-2026-08-31-20 | interp | medium | THE INTERPRETER DOES NOT NARROW A FLOAT LITERAL TO THE ANNOTATED PAYLOAD WIDTH INSIDE `Option.Some(...)` -- `let q: Option[f16] = Option.Some(0.1)` p… | f632ce30 |
| B-2026-08-31-21 | interp+codegen | medium | A DISCARDED `shared` STRUCT LITERAL RUNS NO `Drop` BODY AND LEAKS ITS RC BOX ON ALL THREE BACKENDS -- `let _ = S { . | 964832d |
| B-2026-08-31-22 | interp+codegen | medium | A DISCARDED `if`/`else` WHOSE ARMS ARE OWN-`Drop` ENUM CTORS RUNS ONE BODY ON THE INTERPRETER AND NONE ON EITHER COMPILED BACKEND -- `let _ = if c {… | a139cc1 |
| B-2026-08-31-23 | codegen | high | A NESTED `match` ARM THAT MOVES A HEAP LEAF OUT OF A WHOLE-BOUND *BOXED* `Option` PAYLOAD DOUBLE FREES -- the TRANSFER-path twin of B-2026-08-30-52 (… | f6059cf |
| B-2026-08-31-24 | codegen | high | EVERY ACCESS TO A `Vec[Vector[T, N]]` ELEMENT BUFFER IS EMITTED AT THE VECTOR'S NATURAL ALIGNMENT AGAINST `malloc` MEMORY, SO IT FAULTS ON `vmovaps`… | 33f43c4 |
| B-2026-08-31-25 | codegen | medium | `Slice[T]` HAS NO Display UNDER CODEGEN AT ANY DEPTH -- `f"{s}"` on a slice is a hard build error where the interpreter prints `[1, 2]`, and it is th… | dc3b493 |
| B-2026-08-31-26 | codegen | medium | BOTH COMPILED BACKENDS RUN A STRUCT FIELD'S `Drop` BODY TWICE WHEN A `match` ARM DESTRUCTURES A BARE STRUCT SCRUTINEE AND MOVES THE FIELD OUT -- the… | 6673f7d |
| B-2026-08-31-27 | interp | medium | THE INTERPRETER NEVER RUNS A FIELD'S `Drop` BODY WHEN A SINGLE-LEVEL `match` ARM DESTRUCTURES AN `Option`-WRAPPED PAYLOAD AND MOVES THE FIELD OUT --… | 6673f7d |
| B-2026-08-31-29 | codegen | medium | `let g = ref arr[i]` ON AN `Array[Vector[T, N], M]` PANICS THE COMPILER -- the ref binding is loaded as the LANE type (`i64`) instead of the vector,… | 73e6e86 |
| B-2026-08-31-30 | codegen | high | THE `if let` / `let .. | ac28d68 |
| B-2026-08-31-32 | interp | medium | THE INTERPRETER RUNS NO `Drop` BODY AT ALL FOR A FRESH-TEMP STRUCT SCRUTINEE THAT AN ARM DESTRUCTURES -- `match H { . | 86e8d7a |
| B-2026-08-31-34 | interp+codegen | high | A STRUCT LITERAL WHOSE HEAP FIELD IS A PROJECTION OFF A FRESH TEMP DOUBLE-FREES ON AOT WHILE THE INTERPRETER IS CLEAN -- `let w = V { v: mkv(1).v, b:… | 2b3b966 |
| B-2026-08-31-35 | interp+codegen | medium | A DISCARDED BRANCH WHOSE ARM LITERAL CONSUMES A LOCAL RUNS THAT LOCAL'S `Drop` BODY TWICE ON ALL THREE BACKENDS -- `let t = mkd(7); let _ = if n == 0… | e49a85f |
| B-2026-08-31-36 | interp | medium | THE INTERPRETER BINDS THE WHOLE CONTAINER, NOT THE ELEMENT, FOR `let g = ref c[i]` WHEN `c` IS A `Slice` OR A `Map` -- codegen and the typechecker bo… | 8ba5e95f |
| B-2026-08-31-37 | codegen | low | A `Map` BASE FOR `let g = ref m[k]` DECLINES WITH THE INTERNAL STRING `unreachable: Ref handled in compile_expr` -- no span, and the state is plainly… | 15f91338 |
| B-2026-08-31-41 | ownership | high | A `Slice[T]` THAT BORROWS A LOCAL `Vec` ESCAPES INTO A RETURN VALUE WITH NO DIAGNOSTIC -- `karac check` says "All checks passed" on `fn f() -> Slice[… | c5b5f6a |
| B-2026-08-31-42 | codegen | medium | AN AGGREGATE CONTAINING A `bf16` FIELD ABORTS THE WASM BUILD WITH `Cannot select: fp_to_bf16` WHEN IT CROSSES A NON-INLINED BOUNDARY BY VALUE -- a pa… | b1be5b9 |
| B-2026-08-31-44 | codegen | low | B-2026-08-29-32'S FRESHNESS GUARD IS NOW OVER-CONSERVATIVE AND LEAKS 38 B PER EVALUATION IN TWO SHAPES THE UPSTREAM ALIASING FIX HAS SINCE MADE SAFE… | 1b5c026 |
| B-2026-08-31-45 | ownership | low | A SEMICOLON-LESS `return e` IN TAIL POSITION IS REPORTED AS AN UNSUPPORTED BORROW-RETURN FORM -- `fn f(x: ref S) -> ref S { return x }` is rejected w… | c5b5f6a |
| B-2026-08-31-47 | interp | medium | THE INTERPRETER LOSES A `Drop` BODY THAT BOTH COMPILED BACKENDS RUN, IN TWO METHOD-ARG SHAPES -- a fresh enum temp whose payload an arm BINDS but doe… | 30e0e5d |
| B-2026-08-31-48 | codegen | high | TWO INSTANTIATIONS OF ONE GENERIC FN AT DIFFERENT `Array`/`Slice`/`Vector` TYPE ARGS COLLIDE ON ONE MONO SYMBOL AND FAIL MODULE VERIFICATION -- `fn i… | 007c279 |
| B-2026-08-31-49 | codegen | high | A GENERIC `Result[T, E]` RENDERS WITH THE `Option` VARIANT TABLE WHEN A GENERIC `Option[T]` DISPLAY IS EMITTED FIRST -- `Ok(7)` prints `Some(7)` and… | e5fd34f |
| B-2026-09-01-2 | interp | medium | THE INTERPRETER LOSES A MIXED WRAP'S FRESH FIELD BODY WHEN THE VIEW FIELD IS MOVED OUT -- `let s = S3 { a: r, b: mk(2) }; let x = s.a;` prints `dR1`… | 8a3f0a8 |
| B-2026-09-01-4 | codegen | medium | READING A NON-`Copy` FIELD OUT OF A `ref` PARAM MINTS AN IMPLICIT DEEP COPY -- silently allocating and running a user `Drop` body the source never wr… | e207be3 |
| B-2026-09-01-6 | codegen | medium | CODEGEN DECLINES A NESTED `ref v[0][1]` WITH THE INTERNAL STRING `unreachable: Ref handled in compile_expr` WHILE THE INTERPRETER READS THE ELEMENT C… | b7674909 |
| B-2026-09-01-7 | interp | medium | A BARE `if` WITH NO `else` IN STATEMENT POSITION, WHOSE ARM LITERAL CONSUMES A LIVE LOCAL, RUNS THAT LOCAL'S `Drop` BODY TWICE UNDER `--interp` AND O… | e49a85f |
| B-2026-09-01-8 | codegen | low | CODEGEN'S ERROR CHANNEL CARRIES NO SPAN, SO EVERY `codegen failed: ...` DIAGNOSTIC IS SOURCE-LESS -- ~135 messages render as a bare sentence with no… | 71bb567 |
| B-2026-09-01-10 | codegen | medium | THE -O0 ASAN LEG IS RED ON `main` WITH THREE UNOWNED FAILURES AND AN EMPTY QUARANTINE LIST -- `scripts/asan-o0-leg.sh` reports 1339 passed / 3 failed… | fcb6696 |
| B-2026-09-01-11 | interp+codegen | medium | THE NON-CONSTRUCTOR ARMS OF A DISCARDED BRANCH STILL DISAGREE IN THREE SHAPES -- a CALL arm registers nothing on either compiled backend where the di… | ee15b92 |
| B-2026-09-01-12 | interp+codegen | medium | A SUFFIXED FLOAT LITERAL WHOSE SUFFIX CONTRADICTS THE DESTINATION WIDTH SPLITS THE BACKENDS -- `let d: Option[f32] = Option.Some(0.1f64)` is `Some(0.… | 605208d9 |
| B-2026-09-01-13 | codegen+interp | medium | THE QUALIFIED `Type.method` CALLEE SPELLING IS RESOLVED DIFFERENTLY FROM THE BARE ONE IN AT LEAST THREE PLACES, AND NOBODY HAS SWEPT FOR THE REST --… | 43a0e728 |
| B-2026-09-01-14 | cli | medium | THE DEFAULT `cargo test` LEG WAS RED ON `main` FOR TWO HOURS BECAUSE A CODEGEN-DIAGNOSTIC ASSERTION IS NOT GATED ON THE llvm FEATURE -- `ref_binding_… | Added `#[cfg(feature = "llvm")]`, the gate the other 98 `ka… |
| B-2026-09-01-15 | codegen | medium | A NESTED `ref c[i][j]` STILL DECLINES ON FOUR ROOTS THE Vec-ROOTED LOWERING DOES NOT MODEL -- `Array[Vec[T], N]`, `Vec[Array[T, N]]`, a struct FIELD… | 2143f047 |
| B-2026-09-01-18 | interp | medium | A DISCARDED AGGREGATE LITERAL BEHIND A BLOCK WRAPPER, OR WRITTEN BARE IN STATEMENT POSITION, RUNS ITS CONSUMED LOCAL'S `Drop` BODY TWICE ON THE INTER… | 0033588 |
| B-2026-09-01-19 | typecheck+interp+codegen | medium | AN OUT-OF-RANGE INTEGER LITERAL WITH A WIDER SUFFIX REACHES A GENERIC PAYLOAD SLOT UNCHECKED, SO THE BACKENDS DISAGREE AND ONE SHAPE FLIPS SIGN -- `O… | f366e42e |
| B-2026-09-01-20 | typecheck | medium | DEFAULT ARGUMENTS ARE FILLED FOR FREE FUNCTIONS ONLY -- `H.f(1)` and `h.g(1)` both fail with `expected 2 argument(s), found 1` for a parameter that h… | fc67e1f |
| B-2026-09-01-21 | codegen | medium | A DISCARDED STRUCT LITERAL MIXING A LIVE LOCAL SOURCE WITH A MINTED SIBLING LOSES THE MINTED FIELD'S `Drop` BODY ON BOTH COMPILED BACKENDS -- `S2 { r… | e3b9e67 |
| B-2026-09-01-22 | codegen | medium | A DISCARDED STRUCT LITERAL BEHIND **TWO** BLOCK WRAPPERS RUNS ITS CONSUMED LOCAL'S `Drop` BODY TWICE ON BOTH COMPILED BACKENDS -- `{ { S { r: t, k: 1… | 1fcdb47 |
| B-2026-09-01-24 | codegen | medium | A DISCARDED ALL-MINTED STRUCT LITERAL WITH ONE FIELD THAT IS A SCALAR FIELD READ OF A LIVE LOCAL LEAKS EVERY MINTED OBJECT ON BOTH COMPILED BACKENDS… | 7ef82d3 |
| B-2026-09-01-25 | codegen | medium | THE MEMORY HALF OF B-2026-09-01-21: a DISCARDED aggregate whose fields/elements MIX a live-local source with anything else runs every `Drop` BODY onc… | f909f9a |
| B-2026-09-01-28 | interp | medium | `if let` AND `while let` OVER AN `Option` WITH A STRUCT PAYLOAD LOSE THE PAYLOAD'S `Drop` BODY, where the `match` spelling of the identical program r… | 213f847 |
| B-2026-09-01-31 | interp | medium | A DISCARDED enum STRUCT-VARIANT literal loses its PAYLOAD's `Drop` body under `--interp` -- `let _ = Sv.Hold { inner: R { . | edaca15 |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
