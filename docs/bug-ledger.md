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
| miscompile | 239 | 0 |
| leak | 172 | 0 |
| double-free | 124 | 1 |
| codegen-gap | 105 | 0 |
| run-vs-build | 104 | 1 |
| missing-feature | 93 | 1 |
| perf | 64 | 0 |
| false-positive | 61 | 0 |
| diagnostics | 52 | 0 |
| crash | 44 | 0 |
| soundness | 42 | 0 |
| other | 30 | 0 |
| use-after-free | 18 | 0 |

### By surface

| surface | total | open |
|---|---|---|
| codegen | 827 | 2 |
| typecheck | 159 | 1 |
| interp | 138 | 0 |
| ownership | 47 | 0 |
| other | 41 | 0 |
| autopar | 40 | 0 |
| cli | 29 | 0 |
| runtime | 21 | 0 |
| resolver | 18 | 0 |
| parser | 15 | 0 |
| effect | 5 | 0 |
| lexer | 4 | 0 |
## Current state

_Generated from `bug-ledger.jsonl` by `scripts/bug-curve.py` — **1148 surfaced · 3 open · 1133 fixed · 2 wontfix** (2026-05-20 → 2026-08-13). Do not edit this block by hand; edit the ledger and regenerate._

### Open (3)

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-08-12-32 | 2026-08-13 | typecheck | medium | A user trait `impl` on `String` or `Slice[T]` is ACCEPTED but never found at the call site: `impl Zero for String { .. }` then `s.describe()` fails with `no method 'describe' on type 'String'`. The same impl on `i64`, `f64`, `bool` or even `Vec[i64]` resolves fine, so this is a per-builtin hole in method resolution rather than a blanket 'no user traits on builtins' rule. | method resolution for a USER trait impl whose target is a builtin type -- the `impl Zero for String` declaration is accepted, but a `String` receiver never finds the method |
| B-2026-08-13-2 | 2026-08-13 | codegen | medium | `.to_string()` on a NON-IDENTIFIER scalar receiver is check-green and interp-green but dies under BOTH compiled backends the moment it is used as a receiver for a further method: `'x'.to_string().to_uppercase()`, `7.to_string().len()`, `true.to_string().to_uppercase()`, `(7 + 1).to_string().len()`, `3.5.to_string().len()` -- and one shape, `'x'.to_string().to_string()`, is an outright codegen ICE (`Found IntValue ... but expected the StructValue variant`) | `compile_method_call` / `try_compile_nonident_collection_method` in src/codegen/method_call.rs -- the non-identifier-receiver dispatch for `to_string` |
| B-2026-08-13-4 | 2026-08-13 | codegen | high | Binding or consuming a NESTED heap field read off a Vec ELEMENT double-frees: `let w = ds[0].inner.word` over `Vec[Deep]` with `struct Deep { inner: Pair, .. }` aborts with `free(): double free detected in tcache 2` on both compiled backends where the interpreter prints the string — no call, no index-assign, nothing else in the program | `clone_vec_elem_heap_field_read` (src/codegen/collections.rs) — it handles the ONE-level `ps[0].word` and has no nested sibling |

### Wontfix (2)

<details><summary>2 wontfix — real and reproduced, measured to a standstill, no action left. Titles are kept in full: they carry the measurements that closed the question, so read one before reopening its subject.</summary>

| id | date | surface | sev | title |
|---|---|---|---|---|
| B-2026-08-10-20 | 2026-08-10 | codegen | low | ACCEPTED COST, not a pending fix: `Vec[(i64,i64)].sort_by` on SHUFFLED-UNIFORM input is ~1.30x Rust's `sort_by` (karac ~11.1 ms vs driftsort 8.2-8.9, 150k pairs, this host). Residual of B-2026-08-10-19. FIVE DIRECTIONS WERE MEASURED AGAINST IT AND NONE CLOSES IT -- merge-kernel tweaks, RUN tuning, the bounds-check hoist, a 3-way quicksort run-builder and a 2-way one; do not reopen any of them without new information. The last and most promising, a stable 2-way branchless quicksort run-builder, was BUILT IN FULL, verified correct (98/98 pattern x size, element-type coverage across AOT/LLJIT/interp) and measured: random 11.11 -> 11.73 ms (0.95x, i.e. SLOWER) with instructions 74.05M -> 82.18M (+11%); sawtooth 2.82 -> 3.22 (0.87x). A sweep of the configuration space (span 512/2048/8192/16384/65536, base 16/32/64) found NO setting that beats main on random -- the best, span 16384 base 64, reaches 11.17 vs main's 11.11, i.e. parity. Not merged; the implementation was reverted. This is the cost of the algorithm karac has. The one direction that DID pay, few-unique, is split out as its own open row B-2026-08-11-10. Full write-up in docs/spikes/sort-algorithm-gap.md. |
| B-2026-08-11-28 | 2026-08-11 | codegen | low | RESIDUAL of B-2026-08-10-9, split out per the live-remainder rule: on SHUFFLED-UNIFORM input the mono `sort_by` is still ~1.6x Rust's driftsort. 50a50e8 replaced the fixed-32-run merge sort with a natural-run merge sort, which was the right fix and moved the ORDERED patterns enormously (sorted and reverse went from 39-54x behind to roughly 2x AHEAD -- measured here at 0.47x and 0.52x). It deliberately did not move the shuffled case, and the closing note records that in its own numbers: `random 14.91 -> 14.60 ms (UNCHANGED)`, because shuffled input has ~2-element natural runs so the RUN padding reproduces the old run length and the old pass count. MEASURED FRESH (hyperfine, 10 runs each, clone subtracted via an identical kernel minus the sort call; 25 rounds x 150k (i64,i64) pairs, x86 container): kara pure sort 260.6 ms vs rust 159.0 ms = 1.64x. Same kernel by pattern: shuffled 1.51x total / 1.64x pure, sorted 0.47x, reverse 0.52x. SEVERITY LOW deliberately -- shuffled-uniform is the regime where an adaptive sort has least to exploit, driftsort is a strong baseline, and 1.6x on the hardest pattern while beating it 2x on ordered input is a defensible place to sit. Filed so the remainder is visible in the work queue rather than only inside a closed row's prose, NOT as a claim that it must be closed. CAVEAT: single host, x86_64 shared container, not the canonical Apple-silicon bench host. NEXT STEP if picked up: compare the emitted merge inner loop against driftsort's on shuffled input; the run-detection phase is already known not to help there, so any remaining gap is in the merge itself. |

</details>

### Fixed (1133)

<details><summary>1133 fixed — compact index (one-line titles; full write-up + cross-refs live in `bug-ledger.jsonl`, grep by id). The regression test is the durable artifact.</summary>

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
| B-2026-06-19-4 | codegen | high | A `match` arm that binds a `shared enum` struct-variant payload as a WHOLE struct (Int(n) / Ident(n), then reads n.suffix / n.name) does not drop the… | 8a78ee6d (phase-12 #41) — implemented ledger candidate #2: the shared-enum box rc-drop now WALKS a plain-struct payload that owns a String/Vec/heap field (emit_shared_enum_field_drop + field_is_walkable now gate on `|| type_expr_has_drop_heap(te)`, and emit_nested_struct_shared_rc_decs gained a direct-String cap-guarded free under owns_buffer_free). The double-free tension is resolved by the binding side: bind_pattern_values deliberately does NOT track_struct_var a whole-struct binding `n` for a shared-enum scrutinee (pattern_binding.rs `!pattern_binding_scrutinee_is_shared_enum`), so the box rc-drop is the SOLE owner of name's buffer — exactly one free, no leak and no double-free, regardless of whether the arm reads or consumes `n`. Verified LSan-clean for BOTH the recursive consuming case (asan_shared_enum_recursive_struct_payload_string_freed_no_leak) and the ledger's exact read-only single-level repro (asan_shared_enum_struct_variant_whole_binding_readonly_no_leak, added here). |
| B-2026-06-19-5 | codegen | high | A computed scalar pushed/stored into a sub-word-element collection (Vec[u8] / Vec[bool] / Vec[u16] / Vec[u32], and the slice index-store) was stored… | 66a489ef |
| B-2026-06-19-6 | codegen | high | A read-only `let r = v[i]` binding of a HEAP-owning element out of a `Vec[Vec[T]]` (or any Vec whose element type is non-trivially-copyable) ALWAYS d… | — |
| B-2026-06-19-7 | codegen | high | Index-assignment to a heap-owning element of a nested collection — `out[j] = nb` where `out: Vec[Vec[i64]]` and `nb: Vec[i64]` — SIGTRAPs (exit 133)… | — |
| B-2026-06-19-8 | codegen | high | Vec.filled(n, val) with a heap-backed element type bit-copied the SAME fill aggregate into all N slots, so every slot aliased one backing buffer: the… | — |
| B-2026-06-19-9 | interp+codegen | high | Structural `==`/`!=` on a `shared struct` (design.md § Equality Semantics) was unhandled in both backends despite a correct `#[derive(Eq, PartialEq)]` | — |
| B-2026-06-19-10 | typecheck+interp+codegen | high | `{checked,saturating,overflowing}_{add,sub,mul}` — documented integer methods (design.md § Arithmetic Overflow, the table at ~2146: checked_*→Option[… | — |
| B-2026-06-19-11 | codegen | high | A heap value (Vec[T] / String) captured READ-ONLY by MULTIPLE sibling TaskGroup.spawn tasks while the parent still owns it — the canonical parallel-s… | — |
| B-2026-06-19-12 | typecheck+interp+codegen | high | Two width-dependent integer scalar method families were unrecognized (`no method 'pow'/'count_ones' on type ...`), surfaced by the cross-kata math-bi… | — |
| B-2026-06-19-13 | typecheck+interp | high | `char.to_digit(radix) -> Option[u32]` (Rust's char::to_digit) was unrecognized (Tier-A A2) | 4e4b57de |
| B-2026-06-19-14 | codegen | high | SoA `layout` blocks did not cross function boundaries — passing a SoA-laid-out Vec[E] to another function miscompiled | By-value SoA params DONE in 74bbbef7 (slice 1): a bare Vec[E] param whose name matches a layout block now lowers to the 4-field SoA struct in declare_function (via soa_value_param_layout) with a compile_function prologue that spills the moved-in struct to a directly-GEP'd slot; caller-retains ownership (no callee free, no double-free), no call-site marshalling. Tests: tests/codegen.rs::test_e2e_soa_by_value_param_read_across_function + tests/memory_sanitizer.rs::asan_soa_by_value_param_caller_retains_no_leak_or_double_free (LSan-clean). Forward arg-layout mono DONE in 9beaa258 (slice 2): a by-value SoA Vec[E] crosses a call boundary regardless of param name, routed to an on-demand layout monomorph keyed on the caller's argument layout (fn_asts + ensure_layout_mono_generated). SoA RETURN values DONE in 72298356 (slice 3): backward inference keys the return layout off the receiving `let recv = f()` binding; declare_mono_function lowers the return type to the SoA struct and compile_mono_function seeds the returned local(s) SoA, with suppress_soa_cleanup_for_tail_identifier moving the buffers out to the caller (no double-free/leak). Tests: tests/codegen.rs::test_e2e_soa_return_value_caller_different_name + ::test_e2e_soa_return_two_layouts_through_one_builder_distinct_monos + tests/memory_sanitizer.rs::asan_soa_return_value_caller_owns_no_leak_or_double_free. Multi-buffer / differing-name (BORROW forms) DONE in 6aa7afd9/238e1388 (slice 4): forward layout-flow inference extended to ref Vec[E] / mut ref Vec[E] params (param_is_layout_carrying peels one ref/mut-ref), so multiple SoA buffers of one element type flow through shared by-ref helpers regardless of param name, each a distinct monomorph (total$data_soa_grid vs total$data_soa_coll). A borrow param keeps its pointer ABI (active_param_soa_layout guarded to by-value only); compile_mono_function's prologue registers a SoA borrow param in ref_params so the access paths deref the slot once (without it: pointer bytes read as the SoA struct -> garbage len -> SIGTRAP); ref_params is now save/restored around both mono entry points. Tests: tests/codegen.rs::test_e2e_soa_by_ref_param_caller_different_name + ::test_e2e_soa_two_buffers_through_one_ref_helper_distinct_monos + ::test_e2e_soa_mut_ref_push_across_function + tests/memory_sanitizer.rs::asan_soa_mut_ref_fill_borrow_no_leak_or_double_free (Linux-LSan-clean). Origin-only soa_layouts cutover DONE in db584a09/8ad88b24 (slice 5): the access-path trigger now reads a per-binding LayoutId value carrier (binding_layouts, seeded at the binding site by seed_binding_site_layout) instead of re-deriving SoA-ness from the binding name against soa_layouts at each use; active_layout_id consults layout_subst then binding_layouts with no soa_layouts fallback, so soa_layouts is origin-only (catalogue + LayoutId->shape + binding-site name-match). The redundant name-keyed by-value param ABI (soa_value_param_layout) is retired: the base symbol lowers every Vec[E] param AoS and a SoA arg routes to a mono regardless of param name, fixing the footgun where a base param merely sharing a name with a layout block lowered SoA on the name alone (an AoS arg then marshalled a 3-field struct into a 4-field slot). Cross-group disjointness audited: the borrow checker is layout-agnostic (codegen containment), groups partition fields, so cross-group borrows are already-disjoint field places -- no new fact needed. Tests: tests/codegen.rs::test_e2e_soa_layout_named_param_base_is_aos_mono_is_soa + tests/memory_sanitizer.rs::asan_soa_layout_named_param_base_aos_and_mono_soa_no_leak. Slice 6 (Slipstream full-SoA PROOF) DONE in b9138d30: examples/slipstream/src/sim.kara's carried LBM grid (plus per-substep coll/next) is a layout block split into two cache groups; the per-band chunks stay AoS (they cross the generic TaskHandle[Vec[LbmNode]] join). The native oracle's milestone framebuffer checksums are byte-identical AoS<->SoA (1582897806 / 793640938 / 680974524) and the browser flagship runs on SoA in real headless Chrome (verify_browser.mjs PASS). The proof surfaced and fixed FIVE more cross-function gaps (all in the compiler, no demo-side workarounds): (1) with_capacity SoA constructor -- the presize pass rewrites a counted-loop-filled Vec.new() into Vec.with_capacity(n) (init_grid/fan_collide shape) and the SoA let path matched only Vec.new, so the rewritten binding kept the AoS slot under an SoA layout; is_vec_with_capacity_call routes it to compile_soa_new. (2) Returned-local base-symbol clash -- a builder whose returned local is named after a layout block (init_grid's grid) name-matched its body SoA while the AoS-return base symbol returned the 3-field Vec; soa_return_locals suppresses the origin name-match for a returned local (its layout is the return mono's). (3) SoA reassignment grid = substep(grid,..) -- the carried-grid per-frame double-buffer had no backward-mono path on the assignment arm (only the let arm); compile_soa_assign_from_call parks the return layout, frees the OLD groups (by-value param is caller-retains), stores the new SoA header (Linux-LSan clean, no double-free/leak). (4) Tail-CALL SoA-return propagation -- a SoA-returning fn whose body ENDS IN a layout-returning call (substep's fan_stream(coll,..)) returned AoS while its signature was SoA; compile_tail_final_expr flows the return layout to the tail call. (5) SoA across a coroutine suspend (browser render loop's grid carried across frames.recv()) -- collect soa_layouts + pre-populate fn_asts BEFORE the state-machine emission (the poll-fn body's SoA-return inference reads both), size an SoA persisted local's state-struct field as the 4-field struct, and type an SoA binding's par-block/auto-par return slot SoA (infer_let_binding_llvm_type). Tests: tests/codegen.rs::test_e2e_soa_{counted_loop_fill_returned,returned_local_name_matches_layout,reassign_carried_buffer,tail_call_soa_return} + tests/memory_sanitizer.rs::asan_soa_reassign_carried_buffer_no_leak_or_double_free. Gates: codegen E2E 1696/0, par_codegen 34/0, coro_e2e 122/0, Linux-LSan soa 12/0. FOLLOW-ONS (separate features, never miscompile -- degrade to AoS or unbuilt): branch-leaf/multi-return SoA returns degrade to AoS (spike §8); the whole-element SoA index-store grid[i] = E{..} is unbuilt even single-function (push + mut-ref field write-back + field-level vec[i].field= via B-2026-06-20-7 all work; the whole-element scatter is the gap). The field-level SoA index-store blocker is FIXED (B-2026-06-20-7). See docs/spikes/per-layout-monomorphization.md. FOLLOW-ONS (2026-06-20, commits c74752f6 + 3ccab359): (1) whole-element SoA index-store grid[i]=E{...} — compile_soa_index_store scatters the RHS element's fields into each group buffer at [i] (the push-at-len decomposition, bounds-checked, no growth; deref a ref/mut-ref param slot for cross-function scatter); pre-fix it fell into compile_vec_index_store and wrote whole AoS elements over one group's narrower stride (silent heap-buffer-overflow). 3 codegen E2E (single-fn, mut-ref cross-fn, cold-group) + 1 ASAN overflow guard. POD elements only (the whole SoA subsystem assumes POD; heap-field elements stay an orthogonal gap). (2) branch-leaf / multi-return SoA returns — soa_return_local_names now recursively collects EVERY bare-identifier return site (each explicit return <id>; in any branch/loop/nested block except closures, plus every tail leaf of a branch-bearing tail if c {a} else {b}), so each return value lowers SoA against the patched signature; pre-fix multi-site returns were a hard LLVM 'return type does not match' verify failure (NOT the silent AoS degrade the spike assumed). Early-return move-out uses a branch-safe runtime cap=0 sentinel (neutralize_moved_soa_groups_slot), not the tail path's compile-time FreeSoaGroups frame removal — the early-return frame is shared with the fall-through path where the local is not returned and must still be freed (branch-buried-move footgun). 3 codegen E2E + 2 ASAN/LSan (early-return fall-through, branch-leaf tails — both paths). Native Slipstream oracle byte-identical post-change (1582897806/793640938/680974524); full codegen 1713, par 34, coro 122, SoA LSan 15 all green. |
| B-2026-06-20-1 | codegen | high | Passing a bare named `fn` as a first-class `Fn(...)` value miscompiles | 79f1de14 |
| B-2026-06-20-2 | typecheck+interp+codegen | high | Four allocating String->String methods were unrecognized (Tier-B B1): `trim()` / `to_lowercase()` / `to_uppercase()` (no-arg -> String) and `replace(… | — |
| B-2026-06-20-3 | codegen | high | `Vec.binary_search(x)` / `Slice.binary_search(x) -> Option[i64]` were typecheck- and interpreter-complete but had NO codegen — `karac build` failed l… | — |
| B-2026-06-20-4 | codegen | high | String `==`/`!=` codegen memcmp'd `l_len` bytes from BOTH operand pointers UNCONDITIONALLY (compile_string_binop, BinOp::Eq\|NotEq in src/codegen/expr… | 90db12cb |
| B-2026-06-20-5 | typecheck+interp+codegen | high | No ordered key->value map existed (Tier-B B3) — only `SortedSet` (ordered set) and `Map` (insertion-order hash map) | — |
| B-2026-06-20-7 | codegen | high | Field-level SoA index-store `vec[i].field = expr` is dropped for index >= 1 -- the per-group destination address is not strided by the element index,… | FIXED in 38fb0b57 via a new compile_soa_field_store (collections.rs): locate the field's owning group + its within-group position, then address group_buf[index] by THAT group's sub-struct stride and GEP the field slot (the store-side mirror of compile_soa_index_read's group addressing — same ref-param deref, same bounds check, same coerce_to_struct_field_ty). compile_field_store (expr_ops.rs) routes an indexed SoA object here FIRST (gated on active_soa_layout), ahead of the indexed-shared / nested-plain-struct branches that caused the bug. Compound assignment `bodies[i].field += expr` composes for free (read via compile_soa_index_read, store via the new path). Tests: tests/codegen.rs::test_e2e_soa_field_index_store_strided (cross-group write+read, 164 — a dropped index-1 store gives 161.5) + tests/memory_sanitizer.rs::asan_soa_field_index_store_no_overflow (field scatter across both groups, 20x loop — the old OOB store trips ASAN). ROOT CAUSE: compile_field_store had no SoA branch, so `bodies[i].x = expr` fell into the nested-plain-struct path which treats the SoA struct as a contiguous AoS element — it read the SoA header's field-0 pointer as the data base and strided by the FULL element size. Index 0 coincidentally hit group-0 slot 0; index >= 1 wrote PAST the group buffer (a silent heap overflow), so the store was dropped. Repro (single function, no call boundary): struct Body { x: f64, vx: f64, health: f64 }; layout bodies: Vec[Body] { group pos { x } group vel { vx } group hp { health } }; push two elements, then `bodies[0].x = 7.0; bodies[1].x = 9.0; println(bodies[0].x + bodies[1].x)`. Codegen (SoA) prints 17; the AoS interpreter oracle prints 16. The field-level SoA index-store writes element index 0 correctly but the store at index 1 lands at neither slot 1 nor slot 0 (read-back of bodies[0].x is still 7, bodies[1].x is still its original 10) -- the per-group destination address is computed wrong for index >= 1 (likely a missing index*elem_stride term / wrong group base, mirroring nothing like compile_soa_index_read's correct read addressing), so non-zero-index field scatters are effectively dropped. Pure cross-group READS (bodies[i].x + bodies[i].health) are correct (covered by existing tests). PRE-EXISTING: the base compiler at 6296b9ae miscompiles it byte-identically, so it is NOT introduced by per-layout-mono slice 5 (db584a09, which only swapped the layout value carrier). Independent of the borrow checker (a codegen address-arithmetic fault, not an aliasing-fact gap; surfaced while auditing cross-group disjointness for the e.position += e.velocity idiom). Same family as the unbuilt whole-element index-store grid[i] = E{..} (still open). UNBLOCKS B-2026-06-19-14 slice 6 (Slipstream's LBM kernel scatters field updates by index). |
| B-2026-06-20-8 | interp+codegen | high | Tier-D D1: `Map.entry(k).or_insert(d)` write-through (the `mut ref V` contract, design.md § Entry[K,V]) was broken in BOTH backends — the flagship co… | 2f0a7de1 45089125 200b689c |
| B-2026-06-20-9 | codegen | high | Map-key NO-ADOPT ownership residual (the broader gap B-2026-06-20-8 deferred): the fresh-temp-only key free missed every non-fresh-temp owned key on… | c7b72bd4 |
| B-2026-06-20-10 | runtime+codegen | high | Present-key Map.remove / Set.remove of a HEAP key leaks the bucket's STORED key buffer (and the bool karac_map_remove variant leaks the stored value… | Completes the map-key-ownership class B-2026-06-20-9 (c7b72bd4) started — that fix handled the key ARGUMENT (incoming, no-adopt); this handles the key/value already STORED in the bucket. A present-key remove of a HEAP key tombstones the bucket, but karac_map_free_with_drop_vec only walks OCCUPIED slots, so the tombstoned {ptr,len,cap} String/Vec key buffer is orphaned at map drop (Linux-LSan-only; macOS ASAN misses reachable leaks). FIXED via a drop-flag ABI on the runtime remove entry points + codegen threading the key's heap-ness through. RUNTIME (runtime/src/map.rs): karac_map_remove_old(map,key,out_old_val) gained drop_key: i32 — on a present-key tombstone it frees the bucket's STORED key when drop_key!=0 (heap {ptr,len,cap}) and cap>0; it NEVER frees the value, which is MOVED OUT to the caller via out_old_val (the returned Some(old) owns it — freeing it would double-free). The bool karac_map_remove(map,key) gained drop_key: i32, drop_val: i32 and frees BOTH stored halves (it discards both; the presence boolean carries no payload) — kept correct for the exported ABI even though codegen does not wire it (Map.remove/Set.remove lower to remove_old). Extracted the cap>0-guarded {ptr,len,cap} free into KaracMap::free_heap_field + free_stored_key/free_stored_val helpers and routed karac_map_free_with_drop_vec's live-slot walk through them (behavior-preserving dedup). CODEGEN: bumped the karac_map_remove_old declaration to 4 params (src/codegen.rs) and passed drop_key=llvm_ty_is_vec_struct(key_ty) at BOTH call sites — Map.remove (src/codegen/maps.rs) and Set.remove (src/codegen/collections.rs; elem_ty is the set's key, Set lowers to Map[T,()] so there is no value half). The Map.remove arm's existing incoming-key free (free_fresh_owned_str_arg, B-2026-06-20-9) is unchanged and complementary: it releases the key ARGUMENT on the no-adopt path; the new drop_key releases the STORED key the bucket adopted at insert. NO interpreter change (GC-by-Value-clone; A/B byte-identical). This MODIFIES an existing symbol's signature, so the prebuilt staticlib archives were REBUILT in the worktree (lean->min->full), not copied. TESTS: 2 LSan regression tests in tests/memory_sanitizer.rs (>=36-byte keys per the LSan-reachability rule) — asan_map_remove_present_heap_key_no_leak (Map[String,i64].remove(present)->Some; stored String key freed) and asan_map_remove_present_heap_key_and_vec_value_no_leak (Map[String,Vec[i64]].remove(present)->Some(vec): stored String key freed AND Vec value moved out + freed exactly once by the match-arm, NOT double-freed). NEGATIVE CONTROL: disabling the remove_old key free makes both tests FAIL under the Linux LSan gate (44 / 42 bytes leaked, exactly 1 allocation each = the stored key buffer; the value-test's single-alloc leak confirms the moved-out Vec is not leaked). GATES (worktree): fmt clean; clippy --all --all-targets --features llvm -D warnings clean; runtime unit tests 261; codegen 1700; macOS memory_sanitizer 319; Linux LSan gate 316 passed / 3 failed where the 3 are the PRE-EXISTING non-map leaks documented in B-2026-06-20-9 (asan_owned_struct_option_shared_field_captured_from_builder_no_uaf, asan_single_field_struct_option_payload_sizing_no_bad_access, asan_string_eq_mismatched_len_no_overread_in_indexed_payload_match — confirmed failing on clean main, unrelated to maps); all 39 map LSan tests green incl. the 2 new ones. KNOWN OPEN (distinct, out of scope): Set.remove of a heap element still leaks the INCOMING element — the B-2026-06-20-9 incoming-key class was never applied to Set (collections.rs has no free_fresh_owned_str_arg); flagged for a follow-up slice. |
| B-2026-06-20-11 | codegen | high | Two codegen gaps surfaced by the bespoke word-frequency kata's `keys().sort()` ordered-report idiom over a `Map[String,_]` | d9c05582 |
| B-2026-06-20-12 | codegen | high | Set INCOMING-element NO-ADOPT leak: Set.remove(x) / Set.contains(x) / Set.insert(x) of a HEAP element (Set[String], Set[Vec[T]]) leaked the incoming… | efeb9dbf. Completes the Set side of the map/set key-ownership class: B-2026-06-20-9 (c7b72bd4) fixed the INCOMING key for Map's no-adopt paths but never applied it to Set (collections.rs had ZERO free_fresh_owned_str_arg calls), and B-2026-06-20-10 (a1b59c5e) fixed only the Set STORED element on a present-key remove. This handles the remaining gap — the INCOMING element ARGUMENT on Set's no-adopt paths. Set lowers to Map[T, ()], so the three element-taking arms in src/codegen/collections.rs call karac_map_remove_old / karac_map_contains / karac_map_insert_old; each leaked the incoming element buffer (Linux-LSan-only; macOS ASAN misses reachable leaks). (1) Set.remove and (2) Set.contains are lookup-only and never retain the incoming element — added free_fresh_owned_str_arg(&args[0].value, elem_val) after the runtime call (mirrors Map.remove / Map.contains_key from B-2026-06-20-9; no-ops on a moved binding via its own un-suppressed source free, on a borrowed cap==0 view, and on a non-Vec-String element). Set.remove's incoming free is a DISTINCT buffer from the STORED element freed by the existing drop_key flag (B-2026-06-20-10): the present-key remove test leaks exactly ONE buffer pre-fix (the incoming), confirming drop_key already frees the stored one. (3) Set.insert lowers to karac_map_insert_old, which ADOPTS the element only on the VACANT insert; on the EXISTS (duplicate) path it keeps the bucket's existing element and never adopts the incoming one, while the insert arm's consume-site dance (suppress_fstr_acc_if_moved_out + maybe_defensive_copy_param_arg + suppress_source_vec_cleanup_for_arg) already either suppressed a moved source's scope-exit free or made a private defensive copy of an owned-param element — so the incoming {ptr,len,cap} buffer is orphaned. Fix: free it on the EXISTS branch ONLY (mirror Map.insert's exists-path free), via a conditional-branch diamond guarded by the `existed` bool calling free_str_vec_buffer_if_heap(elem_val); on the vacant branch the bucket adopted the buffer, so a free there would double-free. A vec-struct compile-time gate (llvm_ty_is_vec_struct(elem_ty)) restricts the diamond to heap (Set[String]/Set[Vec[T]]) elements; the cap>0 runtime guard inside free_str_vec_buffer_if_heap no-ops on a borrowed view / rodata literal. Audited ALL Set.* arms: len/is_empty/clear take no element; union/intersection/difference take another SET HANDLE (borrowed, not an owned element buffer) — only insert/contains/remove take an owned element. Reuses the existing free_fresh_owned_str_arg / free_str_vec_buffer_if_heap helpers (runtime.rs) — no duplicated cap>0 {ptr,len,cap} free logic. NO runtime/ABI change, NO archive rebuild, NO interpreter change (interpreter is GC-by-Value-clone; A/B output byte-identical). TESTS: 4 LSan regression tests in tests/memory_sanitizer.rs (>=36-byte elements per the LSan-reachability rule, since LSan misses short still-reachable String/Vec buffers) — asan_set_remove_present_fresh_temp_element_no_leak (Set[String].remove(present): STORED element freed by drop_key + INCOMING fresh temp freed by this fix, two distinct buffers), asan_set_contains_present_fresh_temp_element_no_leak (Set[String].contains(present) fresh temp), asan_set_insert_moved_binding_duplicate_element_no_leak (Set[String].insert dup moved binding -> exists-branch free), asan_set_remove_absent_fresh_temp_vec_element_no_leak (Set[Vec[i64]].remove(absent) fresh-temp Vec from a helper fn, >=48-byte data buffer; uses an EXPLICIT MISS because Set[Vec] does not dedupe equal-contents vecs — a pre-existing Set/Vec hash-eq gap, out of scope — so the lookup-only incoming free is isolated without depending on content equality). NEGATIVE CONTROL: stashing only collections.rs (fix reverted, tests kept) makes all 4 new tests FAIL under the Linux LSan gate with EXACTLY ONE leaked allocation each (39 / 40 / 64 / 39 bytes = the incoming element buffer; the present-remove test's single 39-byte leak confirms the stored element is NOT leaked); restoring the fix makes all 4 pass. GATES (worktree): fmt clean; clippy --all --all-targets --features llvm -D warnings clean; macOS codegen 1707; macOS memory_sanitizer 325 passed / 1 ignored (incl. the 4 new); Linux LSan gate `set` filter 15 passed / 0 failed (incl. the 4 new). The 3 PRE-EXISTING non-map/non-set LSan leaks documented in B-2026-06-20-9/-10 (asan_owned_struct_option_shared_field_captured_from_builder_no_uaf, asan_single_field_struct_option_payload_sizing_no_bad_access, asan_string_eq_mismatched_len_no_overread_in_indexed_payload_match) are unaffected (not in the `set` filter; macOS ASAN does not surface them). Completes the map/set key-ownership class B-2026-06-20-9 / B-2026-06-20-10 started. |
| B-2026-06-20-13 | codegen | high | Heap `for`-loop element BORROW consumed by a retaining sink double-freed in codegen (A/B mismatch on the flagship counter idiom) | 7b93ed59 |
| B-2026-06-20-14 | codegen | high | Three PRE-EXISTING leaks the Linux-LSan gate (scripts/lsan-local.sh) flags but the macOS post-landing ASAN run misses (Apple clang has no LeakSanitiz… | 862f5a1e |
| B-2026-06-20-15 | typecheck+codegen | high | Set[Vec[T]] (and Map[Vec[T], _]) did not deduplicate equal-CONTENTS vecs -- two equal vecs inserted as distinct elements (len()==2 instead of 1), an… | ddc625ad. TWO Vec-specific root causes, both fixed. (1) TYPECHECKER (src/typechecker/derives.rs): the built-in `Vec` is registered in env.structs with NO derived traits, so `type_supports_hash`/`type_supports_eq` fell through to the generic `Type::Named` lookup and reported `Vec` as un-Hash/un-Eq -- a HARD ERROR in the codegen (`karac build`) path (so `Set[Vec[T]]` did not even compile) but only a non-fatal WARNING in the interpreter (`karac run`) path (interpreter is GC-by-Value-clone and dedupes via structural `Value::Array` eq, so it printed the right answer). Fix: a dedicated `Type::Named { name: "Vec", args } if args.len()==1` arm in BOTH predicates, placed BEFORE the generic Named struct/enum lookup, that recurses into the element (`Vec[T]` is Hash/Eq by content iff `T` is) -- mirrors the existing `Type::Array`/`Type::Slice`/`Type::Vector` recursion and `type_supports_display`'s explicit `"Vec"` arm. `Set[Vec[f64]]` stays correctly REJECTED (f64 is not Eq -- IEEE-754 NaN!=NaN). (2) CODEGEN (src/codegen/synth.rs): even past that gate, a `Vec[T]` element/key routed through the `emit_hash_fn_for_type`/`emit_eq_fn_for_type` byte-loop FALLBACK, which hashes/compares the {ptr,len,cap} HEADER (pointer identity), so two equal-contents vecs land in different buckets and never collapse. (String elements always deduped via the `type_name=="String"` byte-walk arm, masking the gap.) Fix: a `Vec` dispatch arm in `emit_hash_fn_for_type_expr`/`emit_eq_fn_for_type_expr` (before the user-struct arm) that, for a `TypeKind::Path` head "Vec" with a `GenericArg::Type` arg, calls the new `emit_hash_fn_for_vec`/`emit_eq_fn_for_vec`; absent the arg it falls back to the header byte-loop. `emit_hash_fn_for_vec` emits `karac_hash_Vec_<elem>(*const Vec)->i64`: loads {data,len} from the {ptr,len,cap} header, seeds the FxHash state with `len` (length is part of the digest, matching Rust's `Hash for [T]`; rotate_left(0,5)=0 collapses mix(0,len) to len*SEED), then folds each per-element hash (`emit_hash_fn_for_type_expr(elem)`, recurses for Vec[String]/Vec[Vec[_]]) via the same rotate-5/xor/mul tail-mix the tuple/struct combiners use. `emit_eq_fn_for_vec` emits `karac_eq_Vec_<elem>(*const Vec,*const Vec)->i1`: compare lengths, then each element via the per-element eq fn, short-circuit false on first mismatch. CRITICAL naming: keyed on the element-aware `karac_hash_Vec_<elem>`/`karac_eq_Vec_<elem>` (via `display_mangle_te`) NOT the shallow `mangled_type_name` "Vec" -- otherwise distinct element types would share one now-content-dependent body; the dispatcher's early cache check (keyed on shallow "Vec") simply misses for Vec and the real dedup happens inside the new fns. Empty vecs: len 0 -> no element loop, hash 0, eq true (data ptr loaded but never dereferenced -> no NULL deref). `cap` is NOT read (a Vec's value identity is len+contents, not capacity). NO runtime/ABI change, NO archive rebuild, NO interpreter change (A/B byte-identical). Fixes `Map[Vec[T], _]` keys and Vec-field-bearing struct/tuple keys for free (all route through the same dispatchers). Vec-ONLY on BOTH sides (not VecDeque): the codegen content path is keyed on a Path head "Vec", so admitting VecDeque in the typechecker without a matching codegen path would re-create the A/B divergence one layer down -- keeping both sides Vec-only stays in lockstep. The fix ALSO opens a previously-UNREACHABLE ownership path: once two equal-contents vecs collapse, the second insert takes `karac_map_insert_old`'s EXISTS branch, whose incoming-element free (B-2026-06-20-12) now actually fires -- covered by a new LSan regression. TESTS: 3 codegen E2E (tests/codegen.rs: test_e2e_set_vec_dedup_by_content [dedup + distinct-stay-separate + contains/remove + same-prefix-diff-length non-equality], test_e2e_set_string_dedup_companion, test_e2e_map_vec_key_dedup_by_content); 2 typechecker (tests/typechecker.rs: test_set_and_map_accept_vec_element_when_inner_hash_eq, test_set_rejects_vec_element_when_inner_not_hash_eq); 1 LSan (tests/memory_sanitizer.rs: asan_set_vec_duplicate_element_dedup_no_leak_no_double_free, >=48-byte buffer per the LSan-reachability rule). Stale-comment fixup on asan_set_remove_absent_fresh_temp_vec_element_no_leak (its "Set[Vec] does not dedupe" parenthetical). GATES (worktree): fmt clean; clippy --all --all-targets --features llvm -D warnings clean; typechecker 1741; codegen 1710 (incl. 3 new); macOS memory_sanitizer 329 passed/1 ignored (incl. new); Linux LSan gate `set` filter 16 passed/0 failed (incl. new dedup test). Determinism verified across 6 codegen rebuilds (no HashMap-order nondeterminism). Closes the SEPARATE Set/Vec hash-eq gap noted while writing B-2026-06-20-12 (efeb9dbf). |
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
| B-2026-07-08-5 | codegen | high | SILENT WRONG OUTPUT (exit 0): under codegen (both `karac build` AOT and the Slice-6b `KARAC_RUN_JIT=1` JIT path), a `Map` insertion performed inside… | FIXED (LLJIT Slice 6c prereq). ROOT CAUSE: `compile_for` (src/codegen/control_flow_for.rs) peeled `.iter()`/`.into_iter()`/`.chars()`/`.bytes()`/`.step_by()` off the for-loop iterable but had NO arm for `.enumerate()`, so `for (i,v) in xs.iter().enumerate()` landed on the dispatcher's silent `_ =>` skip-body arm — the loop body was never emitted, so EVERY outer-variable mutation inside an enumerate loop was lost under codegen (not Map-specific: a plain `sum = sum + v` accumulator also stayed 0). NARROWED via: plain `.iter()` loop persists (len=3), enumerate loop does not (len=0); a plain-i64 `sum`/`count` accumulator in an enumerate loop reads 0 vs the interpreter's 60/5. FIX: added an `.enumerate()` arm to `compile_for` that recognises a 2-tuple pattern over an indexable receiver (`for_receiver_is_indexable`: Vec/Slice/array var), stashes the index sub-pattern in a new `Codegen.enumerate_index_pattern` field, and recurses on the inner `.iter()` receiver with the ELEMENT sub-pattern. The underlying `compile_for_{vec,slice,array}_var` loops already carry the storage index as their induction variable `cur` — which IS the enumerate index — so each now calls `bind_enumerate_index(cur)` right after binding the element, which `take()`s the stashed pattern (so nested loops don't inherit it) and binds it. Codegen-only: the tree-walk interpreter was already correct and is untouched. VERIFIED: examples/leetcode/two_sum.kara now prints `Found: nums[0]+nums[1]=9` under interp, AOT, and JIT (was `No solution` under AOT/JIT); full codegen suite 2086/2086, memory_sanitizer 557/0, new regression test `e2e_enumerate_loop_body_and_outer_mutations_execute` in tests/codegen.rs. COVERAGE: `.iter().enumerate()` over a Vec/Slice/array VARIABLE and over a struct FIELD (`obj.field.iter().enumerate()`, via the field-iter synth path) are both fixed. Still falling through to the prior skip (unchanged, rarer, follow-on): enumerate over array-LITERALS, index receivers, and Map/Set/String receivers. |
| B-2026-07-08-6 | codegen | high | FIXED (both legs) | 776169c (non-generic) + generic/mono leg (this commit) |
| B-2026-07-08-7 | codegen | low | PERF codegen-gap (no correctness impact): karac's `Vec.new()` + push-loop fill does not lower to a single sized zeroed allocation the way rust's `vec… | this commit (characterization — no code change; Linux glibc shows karac already wins, macOS-only platform-allocator artifact, allocator surgery unwarranted) |
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
| B-2026-07-08-25 | codegen | medium | Generic `T.default()` under a `T: Default` bound does NOT monomorphize in codegen (it runs correctly in the interpreter) | this commit (took fix direction (a): route generic `T.default()` in a mono to the concrete type). Three-part wiring: (1) `type_satisfies_bound` (src/typechecker/exprs.rs) gained a `Default` arm delegating to `type_supports_default`, so the `T: Default` bound discharges for `#[derive(Default)]`/primitive concrete args instead of falling through to the empty impl-table and rejecting every arg; (2) `compile_assoc_call` (src/codegen/assoc_call.rs) resolves `type_name` through `type_subst_names` at entry so `T.default()` keys on the concrete `S.default` (derived inherent fn) inside the mono, plus a `primitive_default_value` helper emits the correctly-typed zero for i64/f64/bool/char/String (the old i64-0 fallthrough zeroed only 8 of String's 24 bytes — a double-free); (3) the interpreter's `fn_param_mut_ref_flags` + a `primitive_default_value` intercept (src/interpreter.rs, eval_call.rs) close the interpreter legs for baked-stdlib `mut ref` write-back forwarding and primitive `T.default()`. Also fixed a latent ASAN-harness gap: `run_under_asan` skipped `desugar_program`, so any derive-dependent program miscompiled under it (tests/memory_sanitizer.rs). std.mem `take` now ships as a real generic source body. Tests: interpreter test_std_mem_take, codegen e2e_std_mem_take_codegen, asan_std_mem_take_heap_no_leak_no_double_free, typechecker test_default_bound_{satisfied_by_derive_and_primitive,rejected_for_non_default_type}. |
| B-2026-07-09-1 | codegen | medium | A method call on an INDEXED FIELD-ACCESS receiver — `self.names[i].bytes()` — works under the INTERPRETER but FAILS codegen with 'indexed-receiver me… | this commit |
| B-2026-07-09-2 | codegen | high | AArch64 (Apple silicon) ABI divergence: a `#[repr(C)]` struct passed BY VALUE across the C export boundary is mislowered on arm64, silently returning… | 991d3e2 (Slice 1 arm64 params) + fa18029 (Slice 2 arm64 returns) + 6a6294f (Slice 3a arm64 >16B indirect params) + c3a6820 (Slice 3b arm64 >16B sret returns) + bc6a78c (Slice 3c x86-64 >16B byval/sret) |
| B-2026-07-09-3 | cli | high | The interactive JIT-default `karac repl` SILENTLY DROPPED all cell stdout: a `println(1 + 1);` cell printed nothing to the terminal (while `karac rep… | this commit |
| B-2026-07-09-4 | cli | medium | A REPL cell that PANICS under the JIT loses ALL of its own output — both the text it printed before the fault AND the panic message itself | this commit |
| B-2026-07-09-5 | runtime | low | `#[derive(Message)]` on a BARE ENUM (`#[derive(Message)] enum Role { Guest, Member, Admin }` + `r.encode()` / `Role.decode(..)`) failed with confusin… | this commit |
| B-2026-07-09-6 | codegen | high | Matching a BORROWED `Option[struct]` (`ref` / `mut ref` parameter) and reading a field of the `Some(n)` payload silently returned 0 — a wrong-answer… | this commit |
| B-2026-07-09-7 | typecheck | medium | Silent unchecked implicit integer conversions at binding boundaries (let-annotation, function argument, function return) — inconsistent with the stri… | this commit |
| B-2026-07-09-8 | codegen | medium | Windows x64 `#[repr(C)]` struct-by-value ABI is unhandled — the raw-struct lowering does not match the Microsoft x64 calling convention, a latent sil… | 4c90993d (classifier + signature-match tests + failed Stage 4 Windows CI job attempt) + <this commit> (drop the infeasible Windows execution leg; Linux forced-arch signature-match is the correctness gate) |
| B-2026-07-09-9 | resolver | low | The diverging primitive `panic(msg)` is recognized by the typechecker (diverging list, typechecker/exprs.rs:752 & expr_method_call.rs:41) and by the… | this commit |
| B-2026-07-09-10 | codegen | low | `Result::unwrap_err()` / `Result::expect_err()` (the Err-extracting, Ok-panicking variants) have NO codegen dispatcher arm: `karac build` fails with… | this commit |
| B-2026-07-09-11 | codegen | high | Niche-optimized `Option[shared T]` value stored into a CONVENTIONAL 4-word field slot crashed codegen — and the crash was INVISIBLE because all three… | 706a71e |
| B-2026-07-09-12 | codegen | high | Self-hosted parser BUILDS (after B-2026-07-09-11) but SEGFAULTS / heap-corrupts at RUNTIME on control-flow expressions | this commit |
| B-2026-07-09-13 | codegen | high | [root cause corrected — see fix] A struct-KEYED `Map[S, String]` (S a `#[derive(Hash, Eq, PartialEq)]` struct) double-frees a String VALUE under code… | this commit. ROOT CAUSE was NOT struct-key-specific (the original entry misattributed it): the trigger is a `let` binding between a borrow-returning collection accessor and the consuming match — `let g = coll.get(k); match g { Some(v) => <move/drop v> }`. `Map.get` returns `Option[V]` whose payload ALIASES the bucket's stored value (and Vec/Slice/Array `.get`/`.first`/`.last` return `Option[ref T]` aliasing element storage); the codegen borrow-alias protection (`scrutinee_is_borrow_call` + `clone_escaping_borrow_payload_binding`) only recognized a DIRECT `match coll.get(k)` method-call scrutinee. Through the intermediate `let g`, the scrutinee is an IDENTIFIER, so the protection was skipped: an arm that moved/dropped the payload freed the aliased buffer a second time against the collection's own value/element drop (`karac_map_free_with_drop_vec`). The struct-key repro in the original entry just happened to trip glibc's tcache double-free detector (ASan/valgrind confirmed the free came from `main`'s payload drop, not the hash path — which is correct); the identical i64-key `let`-form double-freed too, and Vec.get `let`-form was the same class. FIX: a new `borrow_accessor_let_payload: HashMap<var, Option-te>` records a `let` binding whose RHS is a borrow-returning stdlib-collection accessor (Map/SortedMap `.get`; Vec/Slice/Array/VecDeque `.get`/`.first`/`.last`) at the let-site (src/codegen/stmts.rs). `scrutinee_is_borrowed_binding` re-admits such an identifier scrutinee into the borrow protection, and `borrow_get_payload_clone_te` recovers the payload type for it — so the Map payload clones on escape exactly as in the direct form and the ref-typed Vec payload self-gates to alias-only. Cleared per-function alongside `for_loop_borrow_vars`. Tests: codegen test_ir_map_get_letbound_moveout_emits_payload_clone; memory_sanitizer asan_letbound_map_get_moveout_no_double_free (bare-String value + struct KEY + if-let sibling, looped). Full suites green (codegen 2112, memory_sanitizer 569, interpreter 1138, par_codegen 154); read-only map.get stays zero-cost (no clone). RESIDUAL (known limitation): the fix covers the common `let g = coll.get(k); match g`/`if let` idiom, but the alias model is still fragile through OTHER indirections — passing `g` to a function, returning it, or chained rebinds (`let h = g; match h`) — because the borrow property is only propagated one hop. These are rarer (the ergonomic path is to match at the call site or `.clone()`). The principled long-term fix is to RETYPE `Map.get -> Option[ref V]` (like `Vec.get`) so the typechecker REJECTS escapes and forces an explicit `.clone()` to own — aligning the type with the codegen reality (the payload IS a borrow) and retiring the whole clone-on-escape patch family. That is an API/semantics decision (touches typechecker + interpreter + all Map.get usage), deferred to a design discussion rather than a bug-fix slice. |
| B-2026-07-09-14 | autopar | high | PERF-REGRESSION (auto-par cost model, NO correctness impact): the default `karac build` (auto-par ON) fans out a ~70us-spawn parallel group in a hot… | this commit |
| B-2026-07-09-15 | codegen | medium | CODEGEN-GAP (no correctness impact — interpreter worked, `karac build` rejected cleanly): `Map.try_insert` / `Set.try_insert` were interpreter-only,… | this commit |
| B-2026-07-09-16 | codegen | medium | CODEGEN-GAP (no correctness impact — interpreter worked, `karac build` rejected): `SortedSet[T]` was entirely interpreter-only under `karac build` —… | this commit |
| B-2026-07-09-17 | codegen | medium | CODEGEN-GAP (no correctness impact — interpreter worked, `karac build` rejected cleanly at `SortedMap.new`): `SortedMap[K, V]` was interpreter-only u… | this commit |
| B-2026-07-09-18 | codegen | high | Generic (monomorphized) fn with an IMPLICIT TAIL bare `f"…"` double-frees the returned String under codegen (interpreter correct) | this commit. A generic (monomorphized) fn whose IMPLICIT TAIL expression is a bare `f"…"` double-freed the returned String under codegen (`karac run`/`build`); the interpreter was correct. `compile_mono_function` (src/codegen/mono.rs) lacked the InterpolatedStringLit-tail cap-suppression block that `compile_function` (src/codegen/functions.rs) has: when a function's final expression is a bare f-string, the loaded {data,len,cap} is the return value, but the f-string accumulator's queued `FreeVecBuffer` frees `data` between the return-value load and `ret` (a use-after-free the caller then double-frees against its own binding). `suppress_cleanup_for_tail_return` only covers Identifier-tail moves, and the f-string acc is staged in `last_fstr_acc` only during `compile_expr`, so the mono needed the same post-compile `zero_vec_alloca_cap(last_fstr_acc.take())` guard. ISOLATION: only the generic + implicit-tail + f-string combination breaks — non-generic tail f-string, generic `let s = f".."; s`, generic `return f".."`, generic tail `.to_string()`/Vec-literal all already worked (they route through suppression or a non-tail path); even a no-interpolation tail f-string in a generic fn double-freed (so it is NOT Display- or param-interpolation-specific — it is the mono tail-f-string ownership transfer). Surfaced while verifying the Display trait surface (which is otherwise complete: user `impl Display`, `#[derive(Display)]`, the non-Display f-string typecheck rejection, and generic `T: Display` dispatch all work in both backends) via `fn describe[T: Display](x: T) -> String { f"item is {x}" }`. FIX: add the guarded `last_fstr_acc.take()` → `zero_vec_alloca_cap` block to `compile_mono_function` right after `suppress_cleanup_for_tail_return`, mirroring `compile_function`. Valgrind-confirmed the pre-fix free came from the caller's read-after-free of the returned buffer; ASAN-clean post-fix. Tests: codegen e2e_generic_tail_fstring_return_codegen; memory_sanitizer asan_generic_tail_fstring_no_double_free (Display struct + primitive + String + no-interp tail, looped). Full suites green (codegen 2122, memory_sanitizer 581, interpreter 1139). |
| B-2026-07-09-19 | codegen | high | Returning a heap FIELD through a BORROWED receiver (`fn name(ref self) -> String { self.n }`, or a `ref` param) double-frees under codegen; the inter… | this commit. `maybe_defensive_copy_param_arg` (src/codegen/runtime.rs) — the return-value hook run on the tail/return expr in `compile_function` — gained a `FieldAccess` arm: when the receiver is a borrow (`SelfValue`/identifier in `self.ref_params`) and the accessed field is a non-shared heap-owning type (`te_owns_heap_below_buffer` && not `shared_heap_type_for_type_expr`), the loaded field value is deep-CLONED via `emit_clone_fn_for_type_expr(field_te)` (alloca src+dst, call, load) so the returned value owns an independent buffer. The receiver's struct name comes from `inferred_receiver_type(object)` (which resolves `self` too), the field TypeExpr from `struct_field_names` + `struct_field_type_exprs`. Gated on `ref_params` membership so an OWNED receiver's field move-out (handled by zeroing the source cap in `suppress_source_vec_cleanup_for_arg_ex`, which requires the receiver to be dropped by this frame) is untouched — the two cases are mutually exclusive. Shared (RC) fields are left to the refcount machinery. ISOLATION: the receiver is used AFTER the call in the tests (`x.name()` then `x.n`), proving a move would be wrong and a clone is required; covers `ref self` / `mut ref self`, String and Vec[i64] fields. Tests: codegen e2e_ref_self_heap_field_return_codegen; memory_sanitizer asan_ref_self_field_return_no_double_free (looped, leak+double-free clean under LSan/ASan). Suites green: codegen 2127, memory_sanitizer 584, interpreter 1141. |
| B-2026-07-09-20 | codegen | medium | The `?` operator does not support MULTI-WORD error types in codegen — a `Result[T, E]` where E is (or contains) a String / Vec / multi-field struct | this commit. The `?` error path now round-trips MULTI-WORD error payloads. `rebuild_value_from_payload_words` (src/codegen/calls.rs) generic-struct arm advances a word cursor field-by-field, giving each field its own window of words (1 scalar / 2 Slice / 3 String-Vec, recursively for nested structs) via the new `payload_words_for_type` helper — so `AppError { msg: String }` claims all three words for its String field instead of insertvalue'ing a lone i64 into a 3-word slot. `compile_question` (src/codegen/exprs.rs) now (a) extracts ALL inner-error words gated on the INNER value's LLVM width, (b) reconstructs the `From` source arg at the param's true type via `rebuild_value_from_payload_words` (a String param gets its full {ptr,len,cap}), (c) packs the converted target error back into the OUTER Err slot via `coerce_to_payload_words` across all outer payload words, and (d) the `main() -> Result[(), E]` exit path uses the converted value / reconstructs E from all words. Verified value-correct AND valgrind-clean across: same-error String, cross-error String->struct{String}, i64->struct{i64}, same-error Vec[i64], cross-error String->struct{i64,String} (4-word), and `main() -> Result[(),String]` exit. Full --features llvm suite green (codegen 2127, memory_sanitizer 584, par_codegen 155, cli 516). |
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
| B-2026-07-11-16 | codegen | low | Module-level `let` binding with a COMPUTED / cross-referencing initializer (e.g | this commit — computed module-binding initializers routed through __karac_static_init (declare zero placeholder global + compile_expr the initializer in the prologue, storing before main). Annotated computed/cross-referencing inits (let DOUBLED: i64 = COUNT * 2) now run==build. Follow-up (same day): UN-annotated computed inits (let DOUBLED = COUNT * 2) also closed — lowering threads the typechecker's inferred value-expr type into a new Program.module_binding_types side-table (keyed by binding name), and declare_module_bindings sizes the placeholder global from it when there is no `: TYPE`. The typechecker stays the single source of truth for the type (codegen never re-infers). Both i64 chains and an i32 binding are covered by test_e2e_modbind_computed_unannotated_initializer + interpreter parity. Module-binding run==build parity is now COMPLETE (literal, composite, let mut, annotated-computed, un-annotated-computed). |
| B-2026-07-11-17 | codegen | medium | `fold(init, \|acc, x\| body)` on a fused iterator chain had no codegen terminal | f0dcd3d |
| B-2026-07-11-18 | codegen | high | SILENT MISCOMPILE: `for x in <iter-chain-with-map/filter>` iterates ZERO times in codegen (interpreter iterates correctly) — wrong answer, no error. | fb41490 |
| B-2026-07-11-19 | typecheck+codegen | low | FIXED (last gap c964b29) | c964b29 |
| B-2026-07-11-20 | codegen | low | GPU helper-call gathering (reachable_helpers, GPU-LBM-5) misses #[gpu] helper calls inside Index / let-RHS / MethodCall / Cast positions, so a valid… | gpu_wgsl::reachable_helpers: add Index/MethodCall/Cast arms to calls_in and a StmtKind::Let arm to calls_in_block so #[gpu] helper calls in those AST positions are gathered into the reachable-helper set. |
| B-2026-07-11-21 | codegen | high | SILENT: reusing an owned `Option[shared struct]` value across two by-value consuming calls whose callee CLONES the matched subtree double-frees under… | 43e1354 |
| B-2026-07-11-22 | codegen | high | DOUBLE-FREE: `for it in vec { match it { Variant(payload) => .. | 8f4453e8 |
| B-2026-07-11-23 | interp+codegen | medium | `mut ref` closure capture (mutation of a captured mutable local) is unimplemented: a closure that writes a captured name mutates a SNAPSHOT, not the… | d123c06 |
| B-2026-07-11-24 | codegen | high | Passing a `Vec[Vec[Option[shared]]]` ELEMENT by value to a consuming (cloning) callee corrupts the heap when the outer Vec GROWS in a loop — silent w… | d2bb92e |
| B-2026-07-11-25 | codegen | high | SILENT MISCOMPILE: a GENERIC struct's ASSOCIATED function returning a struct (`impl[T] W[T] { fn make(x: T) -> W[T] { W{v:x} } }` called `W.make(7)`)… | 6b3fcac |
| B-2026-07-11-26 | codegen+interp | medium | A fresh-temp ENUM scrutinee whose type has a user `impl Drop` SILENTLY SKIPPED that Drop in `if let` / `while let` / `let…else` / `match` — the user-… | this commit |
| B-2026-07-11-27 | codegen | high | A gpu.dispatch result bound/assigned to a SoA `layout` variable SIGSEGVs: compile_gpu_dispatch_soa returns a standard AoS Vec {ptr,len,cap}, but a la… | codegen/exprs.rs + stmts.rs: bind/assign a gpu.dispatch result into a SoA `layout` variable via AoS->SoA scatter (compile_soa_let_from_gpu_dispatch / compile_soa_assign_from_gpu_dispatch, sharing soa_scatter_aos_into + the factored soa_push_value), instead of storing the AoS {ptr,len,cap} header raw into the multi-group SoA slot. |
| B-2026-07-11-28 | codegen | high | Two monomorph void-return miscompiles: (a) a generic VOID fn whose body TAIL is a statement-position `if`/`while` emitted `ret i64 0` in a void LLVM… | 9d17820 |
| B-2026-07-11-29 | codegen | high | Vec[Vec[Option[shared]]] deep-clone + consume + grow: force-cloned inner Vec's scope-exit drop LEAKS retained element handles, and at larger sizes sp… | 106efc1 |
| B-2026-07-11-30 | ownership | low | FIXED (84061d3) | 84061d3 |
| B-2026-07-11-31 | codegen | high | A generic struct instance method mis-inferred its type param `T` (mangled `$i64`, defaulted) when `T` appeared ONLY nested inside a container field (… | 93b095b |
| B-2026-07-11-32 | codegen | high | DOUBLE-FREE: an index-based element swap of a NON-COPY `Vec` element (`let t = v[i]; v[i] = v[j]; v[j] = t;` over `Vec[String]`) aliases the heap buf… | 1e81849 |
| B-2026-07-11-33 | codegen | medium | Vec[Option[shared]] element drop leaked the shared payloads (buffer-only cleanup) — kata-23 merge-k-lists | 6eb7df42 |
| B-2026-07-11-34 | typecheck+interp+codegen | low | Adaptor chaining over `stdin.lines()` (`for x in stdin.lines().map(\|r\| r)` / `.filter(p)`) TYPECHECKS but silently iterates ZERO times under both `ka… | this commit |
| B-2026-07-11-35 | codegen | high | A GENERIC container over a NON-COPY element (`Heap[String]`, `H[T]{xs:Vec[T]}`) was broken across several DIRECT-field-access legs | a663328 (read leg) + d3a72bb (return-field-element leg) + 783cf63 (PUSH leg) + 757f16b (RETURN-OWNED-`T`-PARAM leg: mono tail-return deep-copy of an owned-vecstr param, mirroring the non-generic path + element-aware mono-mangle disambiguation for builtin-collection whole-type-params). ALL FOUR LEGS FIXED. |
| B-2026-07-11-37 | codegen | high | Passing an `Option[String]` moved out of a RECURSIVE shared-enum node BY VALUE to a `mut ref self` method double-frees the payload under codegen (JIT… | The method-call by-value argument path (`compile_method_call`, src/codegen/method_call.rs) now calls `suppress_inline_option_result_binding_move(&a.value)` for an owned by-value arg — the same caller-slot-nulling the free-fn path (`compile_call`, call_dispatch.rs:1535) already applied but the method path omitted. A moved inline-heap `Option[String]` / `Result` / boxed-enum binding now zeroes its caller slot, so the caller's scope-exit `FreeInlineOptionPayload` no longer re-frees the payload the callee's match arm already dropped. Gated OUT of a return-passthrough via `find_function_ast(qualified)` + `fn_returns_param(pidx)` (the method's `self` is param 0, so source arg `i` maps to declared param `i+1`); by-ref args never reach the by-value tail (every `is_ref` arm `continue`s earlier), so no borrow gate is needed, and the helper self-guards on the inline/boxed payload sets so shared `Option[shared T]` and untracked args are untouched. Regression: tests/memory_sanitizer.rs::asan_option_heap_moved_from_recursive_shared_enum_into_mut_ref_self_method_no_double_free (LeakSanitizer OFF via the new `assert_no_double_free_leaks_allowed` helper — this shape ALSO carries the separate, pre-existing leak B-2026-07-11-39, so a full-leak gate would mask the double-free). |
| B-2026-07-11-38 | codegen | low | `fs.read_lines(path) -> Result[Vec[String], IoError]` is INTERPRETER-ONLY at v1 — no codegen | this commit |
| B-2026-07-11-39 | codegen | high | Dropping a recursive `shared enum` whose variant payload struct holds an `Option[String]` (or `Option[<inline-heap>]`) field LEAKS the `Some` payload… | Root cause: the recursive `shared enum` rc-drop destructor (`emit_shared_enum_rc_drop_fn`, synth_drop.rs) decides which variants to drop via `field_is_walkable`, whose struct-payload arm gates on `struct_owns_shared_field || type_expr_has_drop_heap`. `type_expr_has_drop_heap` has a DELIBERATE `Option | Result => false` blind spot (its copy-side callers depend on it), so a payload struct whose only heap is an `Option[String]` / `Option[Vec[T]]` field read as heapless -> the whole variant was judged non-walkable, got no drop block, and its boxed payload + Some payload leaked. The recursive variant (`Wrap(WrapNode{inner:Expr})`) is required only because it makes the enum need the generated destructor at all (a single-variant enum returns None and the payload is reclaimed via the struct's own value-drop, which DOES handle Option[String] via `FieldDrop::OptionInline`). Fix (3 coordinated sites in synth_drop.rs, mirroring B-2026-07-11-33): (1) `field_is_walkable` struct arm and (2) `emit_shared_enum_field_drop` struct arm both OR in the Option-aware companion predicate `te_owns_option_heap_payload` so the variant is walked; (3) `emit_nested_struct_shared_rc_decs_ex` (the payload-struct field walker) gains an `Option[<inline-heap>]` arm (sibling of its existing `Option[shared]` and plain-`String` arms) that frees the Some payload via `emit_option_drop_fn(payload)`, gated on `owns_buffer_free` (RC-box path only; the value-drop path's `__karac_drop_struct_<S>` already frees it). Covers Option[String] and Option[Vec[T]]. The sibling `shared struct` + Option[String] shape was probed and is already leak-clean (no change needed there). Regression: tests/memory_sanitizer.rs::asan_recursive_shared_enum_option_heap_payload_field_no_leak (full LSan). Also upgraded the B-2026-07-11-37 regression from its LeakSanitizer-off gate back to the full-leak `assert_clean_asan_run` (that shape shared this leak; now fully clean). |
| B-2026-07-12-1 | codegen | high | Passing a struct FIELD (`self.names`) BY REF to a FREE function double-frees the field's Vec under codegen (AOT `free(): double free detected in tcac… | 844e3b9 |
| B-2026-07-12-2 | codegen | low | `OnceLock[T]`/`OnceCell[T]` `set`/`get` codegen supports only a HEAP-FREE element `T` (scalar or small all-scalar struct, <=3 words) at v1; a heap-ow… | c4d61cb |
| B-2026-07-12-3 | codegen | high | Assignment through a `mut ref Option[shared]` parameter does not write back to the caller on codegen (interpreter correct) — silent wrong result, SIG… | 89fd514 |
| B-2026-07-12-4 | codegen | medium | Pushing a FIELD-READ `Option[shared]` (`stack.push(n.left)`) onto a `Vec[Option[shared]]` and dropping the Vec with residual elements LEAKS the pushe… | 744ca1c,fba468e |
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
| B-2026-07-13-3 | codegen | medium | A GENERIC function whose body is a `match` expression evaluating to a HEAP type `T` (String/Vec), monomorphized, lowers the match VALUE to the i64 co… | 0e7face,0d680d7 |
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
| B-2026-07-15-21 | codegen | low | Read-only `Option[shared]` tree traversal ran ~1.8x behind equal-safety Rust — the residual was per-node REFCOUNT TRAFFIC (the balanced retain/releas… | a8d47f2 (Part A: RC-elision default-ON) + 36748bb (Part B: Some-binding elision + TCO) + Part C (borrow-forward relaxation of condition 1, folds in the wrapper/helper shape #110) |
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
| B-2026-07-16-13 | other | low | `m[key]` (Map/SortedMap index operator) only accepts integer keys — a non-integer key (`m["x"]` on `Map[String,i64]`) is rejected 'index must be an i… | c585377 — the index-expression typecheck had no Map/SortedMap arm, so a non-integer key fell to the generic integer-or-range gate; an integer key slipped through but returned Type::Error (no Map arm in the element match), check-passing while the interpreter unreachable!'d and only codegen worked. BOTH backends already had native Map-index support the typecheck gate was blocking — codegen's compile_map_index (read, hashes any K) + compile_index_store (write, insert/overwrite); the interpreter had neither. Fix = a typecheck arm (types m[k] -> V for read AND assignment target, checks the key against K with the B-2026-07-16-12 ref-key relaxation) + the interpreter's two missing arms (eval_expr Index read: (Value::Map|SortedMap, key) scan/OrdValue lookup, missing key = clear runtime error since m[k] panics where m.get(k) returns None; set_index store: insert-or-overwrite before the Int-index gate). No desugar, no codegen change. Verified read+write, String/int/SortedMap keys, interp/JIT/native parity, valgrind clean, missing-key panics both backends. Tests: test_map_index_read_and_write_string_and_int_keys, test_map_index_read_missing_key_panics; the existing codegen test_e2e_map_index_* tests are now backed by a real check gate. |
| B-2026-07-16-14 | typecheck+interp+other | medium | `karac check` accepts iterator-reduction / string-collection methods DIRECTLY on a Vec (`v.sum()`, `v.max()`, `v.min()`, `v.product()`, `v.join(sep)`… | 5090b76 — all six shapes now RUN with real types across interp/JIT/AOT. max/min join the iterator terminal surface (Option[T], numeric-or-String, reduce-shaped typing + span-keyed elem recording); direct terminals on iterable receivers route through infer_iterator_method and lowering canonicalizes them to the .iter() chain (receiver identified via method_callee_types — expr_types cannot answer receiver questions, MethodCall.span == receiver.span); join/concat are Vec[String]/VecDeque methods lowered through a new karac_string_join runtime entry (read-only element walk, vector keeps ownership) with interp seq arms (positional separator: ["", "x"].join("|") == "|x"). Codegen max/min reuse the reduce lowering via a synthesized comparison closure; FLOAT elements bail loud to --interp pending B-2026-07-17-11 (pre-existing reduce float-accumulator bug this desugar would inherit as a silent wrong answer). Pin: asan_direct_vec_iterator_terminals_and_string_join_no_leak. NOTE the root cause was WIDER than filed — see B-2026-07-17-12 (unknown methods on non-exhaustive prelude types silently type as Type::Error and unify with anything; v.some_typo() and let x: bool = v.sum() both passed check pre-fix). |
| B-2026-07-16-15 | codegen | high | Seq-tabulate (dae4e309) miscompiled counted push loops whose body ALSO writes the while-loop's control state: `while c < n { out.push(c); if c == 3 {… | b4f86484 |
| B-2026-07-16-16 | codegen | high | tests/selfhost_codegen.rs (selfhost_codegen_matches_seed_run) is RED on main: the self-hosted emitter compiles and runs, but executing its emitted IR… | — |
| B-2026-07-16-17 | other | low | The loop-bound pre-sizing pass fired only on a STRAIGHT-LINE single push per iteration; a body whose sole fill is a balanced `if COND { v.push(a) } e… | 53f5c09 |
| B-2026-07-16-18 | codegen | high | FIXED — Reassigning a heap-owning STRUCT variable (`a = b`) double-frees: the Assign arm never suppressed the moved source `b`'s StructDrop, so both… | b837786 |
| B-2026-07-16-19 | autopar+codegen | high | A function returning `Option[String]` built from a MOVED Vec element (`let words = s.split(" "); if words.len()>0 { Some(words[0]) } else { None }`)… | d9cd7a2 — three coordinated changes: (1) analyzer move-hazard gate (concurrency.rs) keeps statements that CONSUME an owned-heap capture (match/if-let payload move, owned call arg, Option/Result combinator receiver, bare-RHS alias/aggregate move) out of par groups — the hazard is stmt-vs-parent-scope-exit, invisible to the stmt-vs-stmt conflict graph; (2) the par-branch publish loop now tag-sentinel-suppresses FreeInlineOptionPayload / FreeInlineOptionMapPayload / FreeInlineResultPayload for published slots (pre-fix the worker freed the payload right after copying it into the returns struct); (3) the parent slot-rebind loop re-registers the payload free against its fresh alloca (RHS-span -> enum_inst_type_exprs, mirroring the sequential let path) so suppression (2) does not trade the double-free for a per-slot leak. |
| B-2026-07-16-20 | other+interp | medium | A `.to_string()` chained as the receiver of another method (`s.to_string().to_uppercase()`, `s.trim().to_string()…`) build-failed with 'Vec/String me… | c043d03 |
| B-2026-07-16-21 | codegen | medium | A heap-String-returning method used as the RECEIVER of another method (`s.to_uppercase().to_lowercase()`, `e.to_uppercase().split(",")`, `c.trim().to… | c043d03 |
| B-2026-07-16-22 | codegen | medium | `Option[String].unwrap_or(default)` / `Result[String,E].unwrap_or(default)` leaks a fresh heap-String default once per call when the receiver is data… | 598765b |
| B-2026-07-16-23 | codegen | medium | `unwrap_or(<non-Call heap default>)` mismanaged the eager default's ownership | ed0b9db |
| B-2026-07-16-24 | codegen | medium | `String.replace(from, to)` never freed its fresh-owned String ARGUMENTS — a fresh-temp arg (`s.replace(a.to_string(), b.to_string())`) leaks once per… | 7be908c |
| B-2026-07-17-2 | codegen | high | shared-ownership-matrix frontier REGRESSION: `forwarding_chain/ResultOk` + `forwarding_chain/ResultErr` went Clean → Leak with RC-elision ON (`KARAC_… | 3830213 — Result-carried payload exclusion at rc-elide classification (src/rc_elide.rs condition-1 filter): a param whose declared type wraps a shared handle in Result (at any nesting) is never admitted to the elidable set. Root cause confirmed as the e39db64 borrow-forward relaxation admitting the `eat2(r)` bare forward while `eat` itself stayed un-elided (forwarding escapes condition 2): `eat` compiled the forward as a MOVE (own release suppressed) and `eat2` skipped its elided release — nobody freed the Node. The pair-elision is edge-local (skip call-site retain + skip callee release = net zero) and is only balanced when the edge carries that pair: Option[shared]/bare-shared args follow the caller-retains convention (both halves exist), Result[shared] args follow the MOVE convention (no retain twin — the B-2026-07-12-24 scope-exit-dec residual), so Result edges have no pair to skip. Re-admitting Result is gated on B-2026-07-12-24 giving Result[shared] locals a real scope-exit release. Verified: matrix repro leak gone (valgrind 0 definitely lost, interp parity, elidable set empty for the chain), the Option twin of the SAME chain still elides and is valgrind-clean, treesum win intact at 26.8% elide-on vs off (0.364s vs 0.462s, within the 17-32% band), shared_ownership_matrix frontier restored (FLOWS.expected untouched per the filing's instruction). Unit pin: rc_elide::tests::result_carried_param_never_elides. |
| B-2026-07-17-3 | codegen | high | An owned `self` receiver method returned/moved with a non-empty heap (Vec/String) field DOUBLE-FREES the field buffer | 13eda85 (direct-return leg, sibling session) + 2df786d (rebind leg) — both legs were the same missing-SelfValue gate at different call sites of the owned-struct move-out suppression. Direct return (`fn ident(self) -> T {{ self }}`): suppress_source_vec_cleanup_for_arg_ex's var_name match gained a SelfValue -> 'self' arm (gated to an inline-struct self slot so ref self never GEPs through a borrow pointer). Rebind (`let mut b = self; ...; b` — the builder/fluent shape): the Let-arm's move-source resolution in stmts.rs likewise resolves SelfValue to 'self', routing it into the same helper (and into suppress_user_drop_for_var for user-Drop structs) exactly like a plain owned-struct Identifier move. NOT a deeper deep-copy interaction after all — the deep-copy at entry was correct; only the suppression routing missed SelfValue. Verified: builder chain, String-field rebind, user-Drop rebind — interp/JIT/native parity, valgrind clean (drop count exactly matches the two deep-copied instances); memory_sanitizer + codegen suites green. Pins: asan_owned_self_direct_return_no_double_free (13eda85), asan_owned_self_rebind_builder_chain_no_double_free (this fix). |
| B-2026-07-17-4 | codegen | high | `let a = r.unwrap_or("x").len();` on a let-bound `Option[String]` double-frees SEQUENTIALLY (no auto-par): unwrap_or's present branch reconstitutes t… | d9cd7a2 — calls.rs unwrap_or arm now calls suppress_inline_option_result_binding_move(object) at the merge point, the exact suppression unwrap/expect gained in B-2026-07-10-2 (same no-op cases: fresh-temp receiver, non-heap payload; the absent path zeroes a payload-less slot harmlessly). Pin: asan_sequential_unwrap_or_on_named_option_binding_no_double_free. |
| B-2026-07-17-5 | cli | low | wasm link fails with cryptic `undefined symbol: __wasm_first_page_end` (from rustup's self-contained wasi libc.a dlmalloc.c.obj) when the PATH `wasm-… | 6e88245 |
| B-2026-07-17-1 | codegen | low | In-place single-row DP `while k >= 1 { row[k] = row[k] + row[k-1]; k = k-1 }` (Pascal #119, and the general rolling-DP shape) keeps a per-iteration b… | 6474a73 |
| B-2026-07-17-6 | typecheck+interp | medium | `match <non-Option scalar/String> { Some(v) => …, None => … }` (Option-variant patterns on a scrutinee that is NOT an Option) PASSES `karac check` bu… | b71fdd3 |
| B-2026-07-17-7 | codegen | high | `Vec[Tensor]` element ownership was never wired through codegen — a `Vec[Tensor]` (tensor-valued-autograd `Tape` grads/values columns) leaked every e… | 87443ed |
| B-2026-07-17-8 | codegen | medium | Par-tabulate install of a pre-seeded accumulator takes the combine APPEND arm — a serial total×elem memcpy on the parent thread per dispatch while ev… | 7b7ba41a |
| B-2026-07-17-9 | codegen | medium | Routing Vec/String frees through an unattributed karac_free_buf declaration turned every cleanup drain into a clobber-everything opaque call — LLVM k… | 7b7ba41a |
| B-2026-07-17-10 | runtime | medium | The buffer-cache's first cut used OnceLock/Mutex/env::var_os/eprint_fmt inside the force-kept karac_alloc_or_panic/karac_free_buf closure — ONE reach… | 7b7ba41a |
| B-2026-07-17-11 | codegen | medium | Iterator.reduce over FLOAT elements returns the None arm under karac build (interp correct): `[1.5, 2.5, 0.5].iter().reduce(\|a, x\| if x > a { x } els… | 75e248d — the reduce lowering (try_compile_iter_chain_reduce) synthesizes an Option[<elem>] accumulator folded via a match and compiles that AST WITHOUT a re-typecheck pass, so the synthesized Some(<acc>) payload binding had no pattern_binding_types entry and codegen's payload reconstruction fell to the raw-i64 default: a float acc read the payload word via `sitofp i64 -> double` (the f64 bit pattern reinterpreted as an integer VALUE = garbage, so the fold never landed Some and reduce returned the None arm), and a narrow u8/i32 acc skipped truncation. Root cause was WIDER than filed — it hit narrow ints too, not just floats. Fix: give the synthesized Some(acc) binding a unique synthetic span (usize::MAX - uid, distinct per reduce) and register the element's surface name in pattern_binding_types there, so the existing float-bitcast / int-truncate reconstruction arms fire exactly as for a typechecked match. Also un-gated the direct v.max()/v.min() float fast path (B-2026-07-16-14 had bailed floats to --interp pending this) — elem_is_int became elem_is_scalar (adds f32/f64). Verified float/narrow-int reduce + direct float max/min interp/JIT/native parity, valgrind clean. Tests: test_e2e_iter_chain_reduce_float_and_narrow_int_payload (codegen) + a float leg on the B-16-14 asan pin. RESIDUAL (out of scope, filed B-2026-07-17-13): narrow-UNSIGNED reduce now yields the correct bit pattern but still PRINTS signed ([200u8].max() -> -56) — the pre-existing Option-through-unsigned-print gap (B-2026-07-03-21 class), reproduces with a hand-written match over Option[u8], untouched here. |
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
| B-2026-07-18-13 | codegen | high | [RESOLVED — re-measured at parity] Kata #415 add_strings recorded 13.4x equal-safety Rust at filing (89.1B vs 6.25B instrs) | 90fe2ad (attributed, not bisected — B-2026-07-18-15, the String push/push_str accumulator mis-lowered through the Vec TABULATE path; same window + same shape as this kata's string-building loops) |
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
| B-2026-07-18-33 | typecheck+codegen | medium | `Option/Result.map` over a HEAP payload (String/Vec) now works under codegen, unblocked by fixing a chained-method span collision | 58a45ea,4b941dc |
| B-2026-07-18-34 | codegen | high | A fresh owned/heap ARGUMENT temp passed to a predicate call inside a `while` CONDITION leaked one allocation PER ITERATION (unbounded) under AOT/JIT… | 7bcbd47 |
| B-2026-07-18-35 | typecheck+interp | low | `SortedMap` lacked the `.entry()` API that `Map` has, so idiomatic ORDERED aggregation (`m.entry(k).and_modify(\|c\| c += 1).or_insert(1)`, `m.entry(k)… | 83ec9a5 |
| B-2026-07-18-36 | codegen | medium | A CHAINED width-sensitive integer intrinsic (`x.leading_zeros().leading_zeros()`, also `rotate_left`/`count_ones` chains) miscompiled under codegen w… | a71708d |
| B-2026-07-18-37 | codegen | high | A by-value-`self` method returning a HEAP field directly as its tail — `fn get(self) -> String { self.v }`, the canonical owned accessor — DOUBLE-FRE… | dbf8994 |
| B-2026-07-18-38 | codegen | medium | An owned HEAP param (or local) moved into a VEC/ARRAY LITERAL that is returned/bound DOUBLE-FREES under AOT (interp correct): `fn dup(x: String) -> V… | 2d60d38 |
| B-2026-07-18-39 | typecheck+codegen | high | An iterator chain whose SOURCE is a TEMPORARY Vec (a `vec![…]` literal or a call result, NOT a `let`-bound variable) SILENTLY miscompiled to 0/empty… | 8f70020 |
| B-2026-07-18-40 | codegen | medium | Displaying `Option[ref String]` — the borrow-typed result of `Vec[String].get(i)` / `.first()` / `.last()` — failed under codegen with the deferred s… | 6c76b81 |
| B-2026-07-18-41 | typecheck+interp+codegen | low | `Iterator.rev()` was unimplemented — `v.iter().rev()` rejected with `no method 'rev' on type 'Iterator'` in both backends | 9dcf1b8,c2a0f23 |
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
| B-2026-07-19-8 | typecheck+codegen | medium | `weak T` struct fields are DECLARATION-ONLY: the modifier parses, type-lowers to `Type::Weak`, and satisfies the ownership cycle checker (`struct Chi… | e119392 (typecheck + store/read/drop codegen, slices 2-4) + 8f606de (indexed receivers + read-drift fix, slice 5); runtime + layout groundwork edd99f9 (slice 1a) / 85080f1 (slice 1b) |
| B-2026-07-19-9 | typecheck+interp+codegen | low | `Vec[T].split_off(i) -> Vec[T]` was unimplemented | 293b6b5 |
| B-2026-07-19-10 | typecheck+interp+codegen | low | `String.replacen(from, to, n) -> String` was unimplemented (only `replace` existed) | 9a21d56 |
| B-2026-07-19-11 | codegen | low | `Iterator.rev()` codegen residual (B-2026-07-18-41) — a BARE range base `(a..b).rev()` / `(a..=b).rev()` was loud-deferred to `--interp` | 20bcdc5 |
| B-2026-07-19-12 | typecheck+interp+codegen | low | `Iterator.flatten()` was unimplemented — `xs.iter().flatten()` rejected with `no method 'flatten' on type 'Iterator'` | 0425a45,1f1e879,ffd6384 |
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
| B-2026-07-22-12 | codegen | medium | Overwriting an existing key on a `Map[K, String]` / `Map[K, Vec[…]]` (and the parallel `Map.remove`) leaks the DISPLACED / removed old value's heap b… | abe0236, af564fa |
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
| B-2026-07-29-13 | codegen | low | PERF: `Tensor.iter_axis(n)` materialized every sub-tensor as a COPY | d2076a6,64ab0ee |
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
| B-2026-07-29-25 | typecheck | high | An IMPORTED type alias is not expanded to its underlying type: `import types.Row;` where `pub type Row = Map[String, i64]` leaves `Row` nominal, so `… | 0a23cf8+6de8b3b |
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
| B-2026-07-31-10 | codegen | medium | `body_is_memory_bound` classifies a 7-tap vector CONVOLUTION as memory-bound — it keys on "has an index read and no substantial CALL" and cannot see… | f068f0e (indexed-write path) + 269d9ce (reduction path) |
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
| B-2026-07-31-26 | codegen | low | STALE CODEGEN-GAP DIAGNOSTICS: 15 codegen error/doc sites say a deferred construct "works under `karac run`" — false since `karac run` became JIT-by-… | 9d70e52,a6f5df7 |
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
| B-2026-08-01-33 | ownership+autopar | high | `shared struct` was excluded from parallelism on BOTH surfaces — explicit `par {}` hard-errored (E_CONCURRENT_SHARED_STRUCT) and auto-par declined vi… | e061ab29. `frozen` adopted into the authoritative spec as design.md § Feature 4 Part 5c ('Frozen Handles'), which is the remainder this row's own last section named. The feature itself shipped across earlier commits (surface, escape check E0511, freeze-site check E0512, RC suppression via the borrow lowering, `par` + auto-par admission, projection stickiness, per-instance freeze, frozen-element containers) and the motivating kata is published with a measured par lane. Every rule in the new section was re-measured on the current compiler before being written, which corrected two of this row's own claims: the freeze site requires a unique source only for a `mut`-bearing type, and the frozen-element container is an ordinary `Vec[S]` local with no `frozen` in its spelling. The spec's normative example is pinned end to end by `design_md_part_5c_frozen_example_compiles_and_runs` with a keyword-stripped control that must still be refused by the capture gate. |
| B-2026-08-01-34 | codegen | medium | Generic-monomorph field moved out of a deep chain (`let g = o.h.b` where `b: Boxy[String]`) still double-frees — the B-2026-08-01-31 suppressor decli… | f82c0db |
| B-2026-08-01-35 | codegen | high | Field store through a FIELD-ROOTED indexed container (`o.hs[i].field = x` where `hs: Vec[P]` is itself a struct field) is SILENTLY DROPPED under kara… | 38d2d23 |
| B-2026-08-01-36 | parser+resolver | medium | A module-level `let` whose name starts with a LOWERCASE letter is not recognized as a module binding at all — it falls through to a top-level stateme… | FIXED 0584af0. The open question in the filing — how to disambiguate script mode from a module binding — resolves cleanly against design.md § Script mode, which makes script mode and an explicit `fn main()` MUTUALLY EXCLUSIVE (a file with both is already an error). So in a file with explicit main, a top-level `let` cannot be a script statement; it can only be an incorrectly-named module binding. src/parser.rs now reclassifies those into Item::ModuleBinding BEFORE the ambiguity check, so the resolver's existing E_MODULE_BINDING_NAMING fires — exactly the 'widen the recognizer and let the good diagnostic do its job' shape the filing proposed.

END-TO-END RESULT (verified, not inferred): `let mut my_count: i64 = 0;` + two use sites now reports E_MODULE_BINDING_NAMING suggesting `MY_COUNT`, and `karac fix` rewrites the declaration AND both use sites, after which the file re-checks clean. The misdirecting ambiguity error is gone for this shape.

SCOPED DELIBERATELY: only plain-name `let` statements reclassify, and only in a root source file that has an explicit `main`. Expression statements, assignments and tuple destructures are not module-binding shaped, so they stay script statements and keep the ambiguity error — which is the correct diagnostic for them, and is regression-tested in both directions. A file with no explicit `main` is untouched, so script mode still parses `let x = 1;` as a genuine script statement.

Tests: 5 in tests/parser.rs (recognition of a lowercase binding, single-letter `c` on the far side of the old boundary, script mode untouched, expression-statement and tuple-destructure shapes still reporting ambiguity) + 1 in tests/resolver.rs asserting the naming diagnostic fires with the rename and the misdirecting error is absent. |
| B-2026-08-02-1 | other | medium | The Mend scorer counts a CORRECT `karac fix` as having "broken the build" whenever it unmasks errors a earlier-phase failure was hiding | FIXED dd65d4f. Took the filing's suggested shape (phase-aware rather than code-set) and found TWO phase-blind predicates, not one — the filing flagged `fix_introduced_new_error` and suspected `fixes_resolved`; the suspicion was right and the second is worse. `fixes_resolved` was `max(0, len(before) - len(after))`, so the reported case scored ZERO resolved (1 diagnostic before, 2 after): the correct fix was counted AGAINST fix_precision_pct, not merely omitted from it.

New rules: a code is charged as a regression only when it appears at or BEFORE the phase that was blocking the build (so a parse fix yielding typecheck errors is progress, while a parse fix yielding a DIFFERENT parse error is still caught); resolved is a per-code multiset difference, since newly-unmasked codes only ADD to the after set and cannot mask a code that really went away. Unknown phase names sort LAST, so a phase the harness has not learned yet reads as newly-unmasked rather than as a regression — the conservative direction for a metric whose failure mode is over-reporting.

MEASURED, old -> new:
  batch_20260801T235833 (layered): 6 resolved / 75.0% / 1 'broke the build'
                                -> 7 resolved / 87.5% / 0
  batch_20260731T212520 (single-mistake): 5 / 100.0% / 0 — BYTE-IDENTICAL under both.
That second line is the filing's claim restated as a measurement: the bias fires only on multi-phase programs, so the metric looked fine until the fixtures got good.

Tests: examples/mend/harness/test_mend_score.py (new — the harness had none). Most of it guards the OPPOSITE direction from this bug, since the danger in relaxing a metric is making it always-pass: a different parse error at the same phase, a new earlier-phase error, and a same-phase newcomer alongside legitimately-unmasked ones are all still charged. Verified non-vacuous by stubbing the detector to always return 'no regression' — exactly those three fail. Wired into the CI lint job (pure python, no build). |
| B-2026-08-02-4 | resolver+effect | low | FFI lint suggests `allocates(Heap)` but neither the lint nor E0100 mentions the required `effect resource Heap;` declaration — following the compiler… | 8930667 |
| B-2026-08-02-2 | typecheck | low | No implicit `*mut T` -> `*const T` weakening at call sites — every consume-direction binding that passes a malloc'd (or otherwise mut) buffer to a `*… | a31d7be |
| B-2026-08-02-3 | interp | low | Interpreter refuses raw-pointer FFI (`CString.as_ptr` under `karac run --interp`) via a raw Rust panic! + backtrace instead of a structured diagnosti… | 03c8d6a |
| B-2026-08-02-5 | typecheck+interp+codegen | medium | Tuple-element assignment targets are accepted by every checking phase but unimplemented on both backends: `t.0 = v` and `o.t.0 = v` ICE the interpret… | 762a7f8 |
| B-2026-08-02-6 | resolver+interp | high | A BUILTIN function name in ANY non-call position — `spawn;`, `let f = println;`, `spawn { … }` — passes `karac check` and then panics the interpreter… | FIXED e6bf8b5. Resolve rejects every non-call reference to a builtin function (`error_builtin_not_a_value`, src/resolver.rs), naming the correct form — `spawn` gets its closure form spelled out, the rest get `name(…)`. Two details the implementation turns on: (1) keyed on the resolved SYMBOL id (SymbolTable::prelude_fn_ids), not the name, so a local shadowing a builtin stays legal; (2) keyed on the callee's ROOT IDENTIFIER SPAN, not "callee is an Identifier" — a generic-instantiated call parses as Call{callee: Index{object: Identifier, index: ..}}, so the first cut rejected every `with_provider[Clock](..)` (caught by tests/cli.rs's provider-escape cases), while a blanket subtree exemption would have wrongly admitted a builtin as the INDEX (`f[println](..)`). The suggested-fix shape in the filing was right; these two are what it took to make it correct.

Also converted the `unreachable!` at eval_expr.rs:170 into a structured runtime error, as the filing asked: the hole that reached it is closed, but the arm stays reachable from any future resolver hole of the same kind and a panic there violates the never-panic rule regardless.

USER functions remain first-class values (examples/shortener's `Server.serve(addr, handle)` depends on it) — the asymmetry is deliberate, since making builtins first-class would be a feature with a codegen half, not a bug fix.

Tests: 7 in tests/resolver.rs — the three repro shapes (bare `spawn;`, `spawn { .. }`, `let f = println;`), the `spawn` message naming the closure form, and three negatives (call position, generic-callee call, user fn as a value, builtin shadowed by a local). |
| B-2026-08-02-7 | codegen+runtime | high | Declaring a user struct named `Response` WITH A HEAP FIELD makes every `Client.get` double-free the response body: codegen applies the USER type's dr… | 5dd0264c |
| B-2026-08-02-8 | codegen | medium | Tuple-element COMPOUND assignment (`t.0 += 5`) is silently dropped under karac build — prints the stale value with no diagnostic — while the interpre… | a92ecae |
| B-2026-08-02-9 | typecheck | medium | Pointee-changing and const->mut raw pointer casts compile OUTSIDE unsafe blocks — the spec-mandated unsafe gate on `*T as *U` is not enforced (`let w… | a31d7be |
| B-2026-08-02-10 | typecheck+codegen | medium | Methods on tuple-element receivers (`t.0.push(x)`, `t.0.len()`) loud-bail under karac build while the interpreter runs them — the last tuple-place ga… | 7b1122c |
| B-2026-08-02-11 | typecheck | low | Tuple literals do not thread the EXPECTED element types into their elements: `let t: (Vec[i64], i64) = (Vec.new(), 3)` and `Sw { t: (Vec.new(), 3) }`… | 723336d |
| B-2026-08-02-12 | codegen | high | Vec.filled(n, Map.new()) segfaults under AOT (Map handle elements not cloned per slot) | dce2015 |
| B-2026-08-02-13 | typecheck+codegen | high | STDLIB AND USER TYPES SHARE ONE FLAT NAMESPACE, so any user struct that shadows a prelude type name silently takes over that type's codegen identity | — |
| B-2026-08-02-14 | codegen+interp | medium | Drop-carrying field of a GENERIC-mono parent struct: body silent at owner death (both backends) + Vec-element String buffer leaks under AOT | 6ff8614 |
| B-2026-08-02-15 | codegen+interp | high | indexed field store with a NON-PURE index (`v[f()].field = x`): codegen silently dropped the store AND the index's side effect; the interpreter appli… | d52e9ff7+leg2 |
| B-2026-08-02-16 | codegen | medium | Vec[T]-of-Drop field of a GENERIC parent: element Drop bodies fire under karac run but stay silent under AOT | 8f3b456 |
| B-2026-08-02-17 | codegen | medium | Map[K, T] field of a GENERIC parent: the value's heap is never freed at owner death under AOT | db367df |
| B-2026-08-02-18 | codegen+interp | low | user Drop bodies silent for TUPLE-held and Map-VALUE-held Drop values in struct fields, on BOTH backends (parity holds, memory clean) | d6e9beb |
| B-2026-08-02-19 | codegen | medium | generic parent's tuple field never frees the mono element heap: `(T, i64)` classifies no-heap at the declared TE | d6e9beb |
| B-2026-08-02-20 | codegen+interp | medium | container moved into a struct-literal FIELD keeps the source binding's element/value bodies walk armed -- interp-only early fires (Set/SortedMap) and… | 61e68c5 |
| B-2026-08-02-21 | codegen | medium | struct-field Set[DropStruct] leaks element heap; struct-field SortedMap[K, DropStruct] leaks the WHOLE handle tree at owner death | 7cf6db5 |
| B-2026-08-02-22 | codegen+interp | high | Vec[(T, ...)] with a heap/Drop-bearing TUPLE element: no element memory drop and no element bodies walk — heap leaks and the Drop body never fires (b… | 98b3898 |
| B-2026-08-02-23 | codegen+interp | medium | aggregate-literal source disarm has depth-1 / same-frame reach: a NESTED literal or a CALLEE-built literal fires the moved source's element bodies tw… |  LEG 2 FIXED (b208): the callee-built returned literal turned out to be TWO separate defects, neither of them the per-frame tail-return rule the leg-2 note predicted. Probing the shape (b205/b206/b209, 20 programs) relocated the duplicate fire: it is the CALLER, not the callee. Under the caller-drops-the-owned-arg convention the caller fires the arg binding's body at the call -- correct when the value dies inside the callee (`fn consume(v) -> i64 { v.len() }` fires exactly once, and still does), a DUPLICATE when the callee hands that value back out. Two gaps made that happen. (a) `call_arg_flows_into_return` was consulted only by the memory and Option/Result channels; the `UserDrop` bodies channel never consulted it, so even the minimal `fn passthru(v: Vec[Res]) -> Vec[Res] { v }` over `let ys = passthru(xs);` double-fired -- the bare-identifier passthrough that `fn_returns_param` ALREADY recognized. The arg loop now retracts the arg binding's walk when the position flows into the return. (b) `fn_returns_param` recognized only a BARE-identifier return site, so `Holder { xs: v, tag: 9 }` was not a return site at all and (a) never engaged for the shape this row was filed for. Its `expr_is_ident` now also descends StructLiteral/Tuple return expressions -- the param crosses the frame boundary inside the aggregate exactly as it does bare. Because that predicate is the SHARED ast helper both backends gate on, one edit fixed codegen and interp together. The retraction is deliberately the CONTAINER-ONLY disarm, not the strong whole-value form: `karac_drop_<T>` is body + fields + MEMORY under one name, and an own-`Drop` struct param is entry-copied by the callee, so the strong form orphaned the caller's copy (measured: `fn mk(r: Res) -> Bx { Bx { r: r } }` went vg-clean -> 3-byte definite leak). `arg_is_entry_copied_heap_struct` cannot gate it -- that helper matches only literal and call args, and the shape here is a bare identifier. A `suppress_cleanup_for_tail_return` aggregate-literal arm was added too and does carry its own weight: it is what fixes a LOCAL source moved into a returned literal (probe b205_c), which was AOT-only-wrong. Interp twin: `record_passthrough_arg_moves`, routed through `record_container_move_source_name` so it records container/field walks and leaves an own-`Drop` binding armed, matching codegen leg-for-leg. RESIDUALS, both duplicate-body only and both vg-clean: (i) an own-`Drop` struct param returned inside a literal still fires twice (b205_b) -- the callee entry-copies it, so two real values exist and the two bodies match the two frees; (ii) a TRANSITIVE passthrough `fn outer(v) -> Holder { mk(v) }` (b206_i) fires twice, because recognizing it needs a program-level fixpoint over callees rather than the single-Function walk `fn_returns_param` performs. Pins (all STASH-PROVEN FAILED pre-fix): e2e + interp twin passthrough_arg_and_returned_literal_single_fire (bare passthrough + returned literal + a non-returning `consume` control that must keep its single fire), and asan passthrough_arg_returned_no_double_free for the leak side of the container-only choice. |
| B-2026-08-02-24 | interp | medium | interpreter misses a Map VALUE struct's Vec-field element bodies at owner death; AOT fires them | 0a422d8 |
| B-2026-08-02-25 | codegen | high | MATCH-ARM LEG ONLY (displacement leg fixed in 21a1fb6): consuming an Option[Drop] payload via a match/if-let arm binding on a NAMED binding with a bo… | 542d7d7 (src/codegen.rs, src/codegen/control_flow.rs, src/codegen/control_flow_match.rs, src/codegen/pattern_binding.rs, src/codegen/runtime.rs; pins codegen.rs::e2e_named_optres_boxed_payload_arm_runs_user_drop_body -- stash-proven RED, missing D1/D2/D3/W4 -- its interpreter ORACLE twin interpreter.rs::test_named_optres_boxed_payload_arm_runs_user_drop_body with a byte-identical expectation, and memory_sanitizer.rs::asan_boxed_optres_payload_arm_body_runs_against_the_box, the guard for the mutating-body double-free the first attempt caused) |
| B-2026-08-02-26 | codegen | high | a TUPLE binding whose element is a container of Drop values (`let t = (xs, 9)`, xs: Vec[Res]) runs no Drop body and leaks the elements under AOT whil… | Three edits, one per gap. (1) `emit_tuple_elem_user_drop_bodies_fn` now selects targets via a new TypeExpr-keyed gate `elem_te_runs_user_drop` (the codegen twin of interp's `field_te_runs_user_drop`, assembled from the same three head extractors that widened `type_runs_user_drop` for struct fields) and dispatches each element through a new `emit_tuple_elem_bodies_at` carrying the same four legs the struct-FIELD walker has -- direct struct, Vec/VecDeque element, Map/Set value, nested tuple -- so a tuple element and a struct field of the same type now run the same bodies. Its memoization key moved from the element HEAD NAME to the full mangled TypeExpr, because `(Vec[Res], i64)` and `(Res, i64)` both keyed `0_Res` and the second shape would have reused the first's walker; a direct struct element still mangles to its bare name, so existing symbol names are unchanged. (2) `emit_tuple_elem_drops`' Vec arm diverts to the recursive `karac_drop_Vec_<E>` (drain live elements, then free the buffer) when the Vec's element owns heap, and the let-site prefers the TypeExpr path over the LLVM-type path when a new `tuple_elem_needs_deep_drop` predicate says an element would otherwise be freed shallowly. String and `Vec[primitive]` keep the shallow path, where the buffer free alone is exact. (3) `tuple_binding_elem_tes` refines each literal element through a new `refined_tuple_literal_elem_te`, which rebuilds `Vec[E]` for a collection BINDING from `var_type_names` + `var_elem_type_exprs` and takes a free-function CALL's declared return type verbatim, falling back to the old head-name inference for anything else -- kept local to the tuple let-site rather than changing `infer_arg_elem_te`, which has many other callers. Pins: e2e + interp twin tuple_binding_container_element_drop (all three element sources: named binding, fresh call, annotated) and asan tuple_binding_container_element_heap_freed. The e2e and asan pins were STASH-PROVEN FAILED pre-fix; the interp twin PASSES pre-fix and is deliberately kept, since the interpreter's value-driven tuple walk never lost the element type and the twin pins the parity the fix restored. |
| B-2026-08-02-27 | codegen+interp | medium | an own-Drop source moved into a let-RHS TUPLE keeps its own body armed: fires twice on both backends, and under AOT the first fire reads the moved-fr… | Codegen: the TUPLE arm of `disarm_container_bodies_move_sources` now uses the strong `suppress_user_drop_for_var`, matching its consuming-arg sibling. Interp: the tuple arm of `record_container_bodies_move_sources` routes through the existing `record_container_move_sources_in_aggregate_arg`, the consuming-arg helper that already put own-`Drop` sources on `moved_out_user_drop_bindings`; the two positions move the value identically, so they now record identically. CORRECTION ON THE RECORD: the first attempt promoted the STRUCT-literal arm alongside the tuple arm, on the reasoning that it would be a no-op because the let-site's `struct_lit_sources` block already retracts the same names. That reasoning covered only the BINDING path. On the WILDCARD path (`let _ = W { r: r0 }`) `struct_lit_sources` never runs, and unlike the tuple position there is no struct-literal discard walker to take over, so the promotion silenced r0's body outright -- caught by `e2e_wildcard_let_discard_place_shapes_single_fire`, the existing pin for exactly that position, which went RED in the full suite. The struct-literal arm therefore stays on the container-only form on BOTH backends, and the asymmetry is deliberate: the tuple arm is safe only because its wildcard position has an owner (`track_discarded_tuple_elem_bodies`) and the struct one does not. GENERALIZABLE: when promoting a disarm to the strong whole-value form, check every POSITION that reaches the same call site -- binding, wildcard discard, arg -- not just the one in the repro; a disarm is only safe where some other channel is provably the new owner. Pins (both STASH-PROVEN FAILED pre-fix): e2e + interp twin tuple_literal_own_drop_source_disarm, each carrying the inline-element control alongside the named-source case so a future change cannot fix the duplicate by silencing both. |
| B-2026-08-02-28 | codegen+interp | medium | a call result consumed directly as another call's argument (`use_it(mk(xs))`) leaks the inner temp's heap under AOT and fires its Drop body at a diff… | LEAK LEG FIXED (b210). The SHAPE-2 fn-call arm of `track_inline_owned_aggregate_arg` registered the field-BODIES walk and then RETURNED, omitting the memory drop that its struct-LITERAL sibling registers for the identical value -- the two arms differ only in how the temp was produced (a call vs an inline literal), which is not a reason to own it differently. That is why the body printed normally while nothing freed the buffer: a leak that produces CORRECT OUTPUT, so neither parity diffing nor the fire-count oracle could see it; only valgrind did. The arm now mirrors the sibling exactly: the same `needs_memory_drop` gate (LLVM-visible heap field, or a copy-supported struct with a drop-heap field TypeExpr), `track_struct_var` pushed FIRST and the bodies walk second, since the frame drains LIFO and the body must run before the fields it reads are freed. Sound for the same reason the sibling is: the callee entry-copies a copy-supported struct -- `arg_is_entry_copied_heap_struct` already resolves exactly this Call shape through `fn_return_type_names` -- so the caller temp is an INDEPENDENT buffer and freeing it cannot touch the callee's copy. Fixing the order also settled the timing divergence for the bound-result shape (`let n = use_it(mk(xs));`), which now matches `karac run` on both the body position and the value. RESIDUAL (timing only, no leak, tracked here): when the consuming call is NESTED INSIDE another call -- `println(use_it(mk(xs)))` -- AOT fires the temp's body at STATEMENT end (after the enclosing println emits) while the interpreter fires it when the inner call completes (before). Both are self-consistent notions of the temp's death and both are leak-free; reconciling them means changing one backend's temp-drop timing convention wholesale, which is a much larger change than this leak fix and is deliberately not attempted here. Statement-end is codegen's established convention for temps (B-2026-08-01-5). Pins: e2e + interp twin nested_call_temp_owned_arg_drop (bound result, bare-statement discard, and an inner call taking no argument -- the three shapes where both backends agree) and asan nested_call_temp_owned_arg_freed for the leak itself. The e2e and asan pins were STASH-PROVEN FAILED pre-fix; the interp twin passes pre-fix and is kept to pin the order codegen now has to reproduce. |
| B-2026-08-03-1 | codegen+interp | high | an Option/Result payload never runs its user Drop body in ANY nested position — struct field, Vec element, Map value, tuple element — on BOTH backend… | Root cause confirmed as predicted -- a wiring gap, not missing machinery: the tag-guarded payload walker `emit_optres_payload_user_drop_bodies_fn` already existed for the direct-binding position and simply was not reachable from any nested one. Option/Result needed adding at THREE layers, and finding all three is the substance of the fix: (1) the REACHABILITY gates -- a new `optres_payload_heads` extractor (the Option/Result sibling of `vec_field_elem_head` / `map_or_set_field_val_head` / `tuple_field_elem_heads`, and the first to return MULTIPLE heads, since either of Result's arms can be live and carry a Drop) wired into `type_runs_user_drop`'s field walk and into `elem_te_runs_user_drop`; (2) the field-INDEX selector `user_drop_field_indices_mono`, whose omission meant widening the gate alone still left the field out of the walk set; (3) the per-position DISPATCHERS -- an Option/Result arm in `emit_user_drop_field_bodies_fn`, in `emit_tuple_elem_bodies_at`, in `emit_nested_vec_elem_bodies_fn` (striding by the Option LLVM type via the shared `emit_vec_elem_walker_loop`), and in `emit_map_val_user_drop_bodies_fn`. A fourth gap was DECLARED-NAME ERASURE again: `Option.Some(x)` in a tuple literal is a ctor Call naming no function, so `refined_tuple_literal_elem_te` fell through to head-name inference and the element read as bare `Option`; it now rebuilds `Option[P]` from the ctor's own argument. INTERPRETER (needed independently, and mid-fix AOT briefly fired while interp did not -- a divergence that would have been worse than the original both-silent bug): the `field_te_runs_user_drop` gate learned to check a RANGE of generic args rather than one index; `field_value_carries_user_drop` gained an Option/Result payload arm; and FOUR separate walks needed the arm because this side splits them differently than codegen does -- struct field, Vec element, the field-level map walk AND the binding-level map walk, plus the binding-level map REGISTRATION gate (`record_map_val_bodies_te`), which is the gate-before-walk shape B-2026-08-02-24 already taught: the walk was correct and simply never armed. All routed through `run_discarded_value_user_drops`, the value-driven recursion this side already used for these built-ins. UNMASKED, NOT INTRODUCED -- filed as B-2026-08-03-3: two positions (a `Result[Res, i64]` struct field, and a tuple-held `Option[Res]`) go vg-clean -> vg=99 with this fix. Verified latent rather than regressed with `valgrind --trace-malloc=yes` on the stashed build: the pre-fix binaries allocated the payload buffer ZERO times, LLVM having elided a heap value nothing ever read. Making the body read `self.name` is what brings the allocation to life and exposes a free that was always missing. Same discriminator, same conclusion as B-2026-08-02-19. The full suite is green (6867/0) with those leaks present, so nothing existing depended on the silence. Pins (all three STASH-PROVEN FAILED pre-fix): e2e + interp twin optres_payload_bodies_in_nested_positions, covering all four nested positions plus a `None` control that must stay silent; asan optres_payload_nested_positions_clean, deliberately scoped to the three positions whose memory was already correct, so it guards the new walks without baking in the B-2026-08-03-3 leaks. |
| B-2026-08-03-2 | codegen+interp | high | a container element destroyed WITHOUT being bound never runs its Drop body — clear, truncate, discarded remove/swap_remove/pop, and whole-container r… | CLASS 3 FIXED (b213), row stays OPEN for classes 1 and 2. The interpreter's bare-statement discard arm (`StmtKind::Expr`'s MethodCall branch) only ever admitted USER methods, while the `let _ = v.pop();` form went through `discard_rhs_produces_owned_value`, whose method list already covers the builtin container removals -- so the two DISCARD STATEMENT SHAPES disagreed with each other, and against codegen, which fires for both. The bare arm now runs the same walker on the same method list. DELIBERATELY GATED to an Option/Result-shaped result, which is exactly codegen's current reach: its discard registrar is the optres payload walker and the only receiver it resolves a type for is Map-shaped, so `v.pop()` and `m.remove(k)` are covered there while `v.remove(i)` and `v.swap_remove(i)` (bare `T`) are not. The first attempt used the unrestricted list and FLIPPED those two from both-silent to interpreter-fires/AOT-silent -- strictly worse, since AOT also leaks them. Caught by re-running the probe matrix before committing; the gate is what keeps this a pure divergence-removal. GENERALIZABLE: when closing a run-vs-build split by moving one backend, check every shape the change admits, not just the reported one -- a fix that widens a gate can convert a shared gap into a divergence, which is the worse failure mode. STILL OPEN: class 1 (clear / truncate / whole-container reassign -- body silent, memory clean, both backends) and class 2 (discarded `v.remove(i)` / `v.swap_remove(i)` -- body silent AND leaking, both backends). Class 2 needs codegen's discard registrar to resolve a Vec receiver's element type and to own the removed element's memory, which is the substantive remaining work. Pin: interp `bare_statement_container_removal_discard_fires` (STASH-PROVEN FAILED pre-fix), carrying the bound-removal control alongside the two fixed shapes. CLEAR FIXED (b213b), covering the `clear()` half of class 1 on both backends. Both arms turned out to do all the MEMORY work and none of the body work, which is exactly why the shapes were vg-clean and silent: codegen's `Vec.clear` went straight to `emit_vec_drop_fn` (drain + free) and its `Map.clear` to the shared rc-dec walks plus the per-value drop fn, while the interpreter's arms called Rust's `Vec::clear` / replaced the table with an empty one -- operations that reclaim storage and know nothing about Kara destructors. Each now runs the SAME element/value bodies walker the binding-death path uses, BEFORE the memory work on the codegen side (the walker frees nothing, so it cannot disturb the drop, and running it first is what lets a body read the fields it prints) and after the container is emptied on the interpreter side (so no read guard is live while a body runs, in case a body touches the container). Pins (both STASH-PROVEN FAILED pre-fix): e2e + interp twin container_clear_runs_element_drop_bodies, carrying a two-element Vec (order matters), a Map, and a clear-then-reuse control that checks the buffer is still usable and the replacement element still fires at scope exit. CLASS 1 REMAINDER, still open: `truncate(n)` (the REMOVED tail is silent while the survivors still fire, so a naive whole-container walker would be wrong here -- truncate needs a RANGED walk over [n, len), which is why it was not folded into this change) and whole-container reassign `v = w` (the displaced old container's elements). Class 2 (discarded Vec remove/swap_remove: silent AND leaking) is untouched and remains the substantive work. CLASS 2 FIXED (b214). A discarded `v.remove(i);` / `v.swap_remove(i);` hands the element back BY VALUE with nothing to receive it, and the discard battery could not tell what it was holding: its method arm resolves a return type through `fn_return_type_names`, keyed `Type.method`, and BUILTINS have no entry there (they are not declared impl methods). So the registration was skipped entirely -- no body, no free. The arm now falls back to the receiver's recorded element TypeExpr (`var_elem_type_exprs`), the same side-table the container's own drop machinery keys on, for a Vec/VecDeque receiver and the two by-value removals. `pop` is deliberately NOT in that fallback: it returns `Option[T]`, which the optres arm of the same battery already owns. Once codegen could own the element, the interpreter's bare-statement gate was widened to match (and `swap_remove` added to both its method lists, having been absent from each). SEQUENCING WAS THE POINT: codegen first, interpreter second. The class-3 pass had already shown that widening the interpreter alone converts a shared gap into a divergence -- worse, here, because AOT was also leaking -- so the interpreter arm stayed narrow until the backend that ships could own the value. Verified by re-running the full removal matrix after EACH half, not just at the end. Pins (all three STASH-PROVEN FAILED pre-fix): e2e + interp twin discarded_vec_removal_fires_and_frees, carrying the always-correct BOUND control and a two-element `survivor` case that checks only the REMOVED element fires early while the one left behind still fires at scope exit; asan discarded_vec_removal_no_leak for the leak half. CLASS 1 REMAINDER, the only part of this row still open: `truncate(n)` (needs a RANGED walk over [n, len) -- its survivors correctly fire already, so the whole-container walker used for `clear` would double-fire them) and whole-container reassign `v = w` (the displaced old container's elements). CLASS 1 COMPLETED (b215) — the row is now CLOSED, all three classes fixed. The two remaining positions had the same memory-yes/bodies-no split as `clear`, but needed different walks, and getting that distinction right is the substance: `truncate(n)` required a RANGED walk. Its existing loop already stepped [n, len) freeing each removed element, and it ran no body -- so the removed tail lost its destructors while the SURVIVORS still fired at binding death, a fire count that looks plausible and is wrong. The whole-container walker used for `clear` is NOT usable here: it would re-fire the survivors. The per-slot bodies dispatcher now runs inside that same [n, len) loop, ahead of each free. Whole-container reassign `v = w` DOES displace every old element, so there the whole-container walker is correct; it now runs before the reassign path's per-element release and buffer free. The interpreter needed the mirror-image edits: a tail snapshot in `truncate`, and a `Value::Array` arm in the displaced-value match, which previously handled only Struct and EnumVariant. REFACTOR: `emit_tuple_elem_bodies_at` was renamed `emit_slot_drop_bodies_at` and made pub(super). It was written for tuple elements and named for them, but its contract -- run the bodies reachable from ONE SLOT of a given type -- was never tuple-specific, and truncate needed exactly that per-slot dispatch. Pins (both STASH-PROVEN FAILED pre-fix): e2e + interp twin truncate_and_reassign_run_displaced_drop_bodies, whose truncate case is what pins the RANGE -- the removed tail fires at the truncate, the survivor fires at binding death, neither twice -- plus a truncate(0) case and the reassign case. |
| B-2026-08-03-3 | codegen+interp | high | a tuple-held Option[Struct] / Result[Struct, E] payload is never freed in ANY position, and a `let x = t.N` move-out fires the source element's Drop… | Leg A + the move-out double-fire: c775e70 (src/codegen/synth_drop.rs, src/codegen/control_flow_match.rs, src/codegen/runtime.rs, src/codegen/stmts.rs, src/codegen/call_dispatch.rs, src/codegen.rs, src/codegen/functions.rs, src/interpreter.rs, src/interpreter/eval_stmt.rs; pins test_e2e_tuple_held_optres_payload_freed_and_dropped, test_e2e_tuple_elem_move_out_single_body_fire, test_tuple_held_optres_payload_bodies_fire, test_tuple_elem_move_out_single_body_fire, asan_tuple_held_optres_payload_freed). Leg B: 8cfe5be (src/codegen/control_flow.rs, src/codegen/control_flow_match.rs, src/codegen/param_own.rs, src/codegen/stmts.rs, src/codegen/synth_drop.rs; pins test_e2e_result_struct_payload_field_freed_and_single_body (stash-proven RED -- pre-fix the local-match position prints a spurious `drop 5 drop 5` and the param-match position fires twice), asan_result_struct_payload_field_freed (stash-proven RED with a heap-use-after-free), and the interpreter ORACLE twin test_result_struct_payload_field_freed_and_single_body, which is GREEN under the stash by construction -- the interpreter was correct on all seven positions throughout, which is what makes it the oracle). |
| B-2026-08-03-4 | codegen | medium | mis-shaped handler `Response` panics the HTTP serve shim instead of diagnosing | FIXED 7068f8c. `require_http_handler_response_shape` (src/codegen/http.rs) validates the handler's LLVM return type — struct, >=2 fields, field 1 itself a struct (the String aggregate) — at all four `emit_http_handler_shim` call sites in src/codegen/assoc_call.rs, returning a structured diagnostic that states the required shape instead of letting the shim's `build_extract_value(.., 1, ..).unwrap()` panic. Regression test `misshaped_handler_response_is_a_diagnostic_not_a_panic` in tests/http_client_codegen.rs asserts both the diagnostic text and the absence of a panic; examples/shortener still builds, confirming the legitimate user-declared-`Response` pattern is undisturbed. |
| B-2026-08-03-5 | codegen | high | Displacing an Option[T] binding emits a SPURIOUS extra user Drop body over a stale slot, printing a garbage field value — pre-existing, independent o… | 284c432 |
| B-2026-08-03-6 | interp | medium | `match t.N { Ok(v) => . | src/interpreter/pattern_match.rs (the TupleIndex arms of `disarm_moved_out_enum_payload` and `scrutinee_expr_is_consuming`); pin interpreter.rs::test_tuple_elem_match_scrutinee_body_fires_at_arm_end, stash-proven RED. Suite 12891/0; 22-shape probe matrix unchanged elsewhere. |
| B-2026-08-03-7 | codegen+interp | medium | a struct field holding a tuple, and a Map value holding a tuple, run NO Drop body for the tuple's elements on EITHER backend | 08c12068 (src/codegen/synth_drop.rs, src/interpreter/pattern_match.rs, src/interpreter/eval_stmt.rs; pins test_e2e_struct_field_and_map_value_tuple_run_element_drop, test_struct_field_and_map_value_tuple_run_element_drop, asan_struct_field_tuple_optres_payload_freed -- all three stash-proven RED). 22-shape probe matrix all SAME across backends and vg=0 except the rows this does not claim; suite 12891/0. |
| B-2026-08-03-8 | codegen+interp | medium | `let x = h.f` moving an Option / Result / Vec FIELD out of a struct never disarms the field's Drop machinery — SEGV for an Option[Struct] field, a do… | Memory half: src/codegen/param_own.rs (`suppress_struct_field_move_into_literal`'s Option/Result arm), pin asan_option_struct_field_move_out_no_double_free. Bodies half: 8407085 (src/codegen/runtime.rs, src/codegen/synth_drop.rs, src/codegen/control_flow_match.rs, src/codegen/stmts.rs, src/codegen.rs, src/codegen/functions.rs, src/interpreter.rs, src/interpreter/eval_stmt.rs; pins test_e2e_struct_field_move_out_single_body_fire, test_struct_field_move_out_single_body_fire). All three pins stash-proven RED; suite 12896/0. |
| B-2026-08-03-9 | typecheck | medium | the canonical `Map[K, Vec[V]]` grouping idiom taught by the corpus is QUADRATIC, but the language already has the O(k) answer -- `Map.entry(k).or_ins… | d166203 |
| B-2026-08-03-10 | codegen | medium | an Option field whose payload is INLINE (a struct narrow enough to fit the 3-word payload area) fires its Drop body THREE times under AOT when the ow… | 257d666 (src/codegen/clone_drop.rs; pin test_e2e_inline_option_payload_field_body_fires_once, stash-proven RED -- pre-fix AOT prints `drop 1` twice and `drop 2` three times where the interpreter prints each once) |
| B-2026-08-03-11 | codegen | medium | a struct field holding a MIXED `Result[<Drop struct>, String]` -- one half a direct String/Vec, the other a struct/enum -- is admitted by NEITHER Res… | 0567cea (src/codegen/control_flow_match.rs, src/codegen/param_own.rs; pins asan_mixed_halves_result_struct_field_freed -- the leak oracle, stash-proven RED with 123 bytes leaked in 6 allocations under LSan -- and the companion E2E test_e2e_mixed_halves_result_struct_field_freed, which is green under the stash by construction and guards the six consuming positions against the double-free/double-fire that arming a free invites) |
| B-2026-08-03-12 | codegen | low | `coroutine_preserves_active_span_across_suspend` (tests/coro_e2e.rs) is INTERMITTENT -- the post-resume log line came back unstamped (`[info] after-r… | 7f9e8656 |
| B-2026-08-04-1 | codegen | high | FRESH-TEMP twin of B-2026-08-02-25's match-arm leg: a heap-BOXED Option/Result payload bound out of a `match mk() { Some(r) => . | c89192f (src/codegen/control_flow.rs, src/codegen/control_flow_match.rs; pins codegen.rs::e2e_freshtemp_boxed_optres_payload_arm_body_runs_against_the_box and memory_sanitizer.rs::asan_freshtemp_boxed_optres_payload_arm_body_runs_against_the_box, both stash-proven RED -- the E2E aborts before flushing any output (`left: ""`) and the ASan pin reports a double-free at exit 23) |
| B-2026-08-04-2 | codegen | high | A heap-BOXED Option payload bound by a consuming match arm and then MOVED -- into a struct literal, or out as the match's tail value -- double-frees… | 8f8696d (src/codegen.rs, src/codegen/control_flow.rs, src/codegen/control_flow_match.rs, src/codegen/exprs.rs, src/codegen/functions.rs, src/codegen/mono.rs, src/codegen/pattern_binding.rs, src/codegen/runtime.rs, src/codegen/shadow.rs, src/codegen/stmts.rs; pins codegen.rs::e2e_boxed_optres_payload_view_move_transfers_box_interior (stash-proven RED -- aborts before flushing any output) and memory_sanitizer.rs::asan_boxed_optres_payload_view_move_has_one_owner, both carrying the by-value-arg and no-move controls that the first cut got wrong) |
| B-2026-08-04-3 | codegen | medium | A FRESH-TEMP boxed Option/Result scrutinee matched by a WILDCARD payload arm (`match mk() { Some(_) => . | bad9a7a (src/codegen/control_flow.rs, src/codegen/control_flow_match.rs; pins memory_sanitizer.rs::asan_wildcard_boxed_optres_payload_frees_the_struct_interior, stash-proven RED under LeakSanitizer, covering the `Option` wildcard, the `Result` Err-side wildcard, and a whole-binding control) |
| B-2026-08-04-4 | interp | medium | INTERPRETER: a Drop-body move record is keyed by BINDING NAME and outlives its block, so a later same-named binding that was never moved is treated a… | 168c887 |
| B-2026-08-04-5 | codegen | high | ICE: destructuring a heap-BOXED Option/Result payload with a STRUCT sub-pattern (`match o { Some(Full { name, buf }) => . | 9908f6a (src/codegen/control_flow_match.rs; pins codegen.rs::e2e_boxed_optres_payload_struct_destructure_deboxes and ::e2e_struct_pattern_wins_over_a_same_named_enum_variant, both stash-proven RED with the exact `ExtractOutOfRange` ICE, their interpreter oracles interpreter.rs::test_boxed_optres_payload_struct_destructure_deboxes and ::test_struct_pattern_wins_over_a_same_named_enum_variant, and memory_sanitizer.rs::asan_boxed_optres_payload_struct_destructure_owns_the_interior_once for the ownership side the debox made reachable) |
| B-2026-08-04-6 | codegen | medium | A FRESH-TEMP boxed Option/Result scrutinee destructured by a PARTIAL struct sub-pattern (`match mk() { Some(Full { name, buf: _ }) => . | 7ecdec0 (src/codegen/control_flow_match.rs, src/codegen/control_flow.rs; pins memory_sanitizer.rs::asan_freshtemp_boxed_payload_partial_destructure_owns_every_field, stash-proven RED under LeakSanitizer at 19,200 bytes, covering `field: _`, `..`, the String half, the `Result` Err side, and an all-fields-bound control for the double-free direction) |
| B-2026-08-04-7 | cli | high | EVERY project-mode `karac build` fails immediately: it opens the PACKAGE NAME as a source path (`error: cannot read 'solo'`), so a manifest-driven bu… | 4c52d82 (src/cli.rs; no new test -- the 36 pre-existing `tests/cli.rs` cases were the RED signal and are the pin: 524 passed / 36 failed before, 560 passed / 0 failed after) |
| B-2026-08-04-8 | codegen | medium | Bounds-check elimination fails for the CONVERGING two-pointer loop `while lo <= hi { v[base+lo] .. | e94e6bd9 (slice 1, intra-function converging+base skip) + 55f3d4a3 (slice 2, length pin survives passing the Vec to a free function). Residual callee-side gap split to B-2026-08-05-6. |
| B-2026-08-04-9 | codegen | high | `?` on an Option/Result whose payload is heap-BOXED unwraps the BOX POINTER as if it were the payload's first word -- the value comes out empty or ga… | 60087fc (src/codegen/exprs.rs, src/codegen/control_flow_match.rs for the helper's visibility; pins codegen.rs::e2e_question_reconstructs_wide_and_boxed_ok_payloads, its interpreter oracle interpreter.rs::test_question_reconstructs_wide_and_boxed_ok_payloads, and memory_sanitizer.rs::asan_question_deboxed_ok_payload_frees_the_box -- the first two stash-proven RED on the exact garbage output, the third at 12,800 bytes under LeakSanitizer) |
| B-2026-08-04-10 | codegen | medium | `let <StructPattern> = <expr>?;` -- a struct destructure written DIRECTLY on a `?` whose payload is heap-boxed -- leaks the payload's heap fields; ro… | a82b4ff (src/codegen/stmts.rs; pins the extended memory_sanitizer.rs::asan_question_deboxed_ok_payload_frees_the_box, stash-proven RED against this commit's change alone at 6,400 bytes under LeakSanitizer) |
| B-2026-08-04-11 | codegen | high | `match <fresh Result temp> { Err(e) => . | 563eb8b (src/codegen/control_flow_match.rs; leg (a) was 375dd77). Pins codegen.rs::e2e_freshtemp_result_arm_binding_a_struct_payload_owns_it_once, memory_sanitizer.rs::asan_freshtemp_result_arm_binding_a_struct_payload_owns_it_once, interpreter.rs::test_freshtemp_result_arm_binding_a_struct_payload_owns_it_once. All three are stash-proven RED (abort, exit 134) and carry the wildcard / named-scrutinee / recover-CONSUME immunities as controls. cargo test --features llvm: 12968 passed, 0 failed. |
| B-2026-08-04-12 | codegen | high | `?` PROPAGATING an Err whose payload is a struct wider than THREE words silently drops every word past the third -- the Err mirror of B-2026-08-04-9'… | e6a2eca (src/codegen/exprs.rs; pins codegen.rs::e2e_question_propagates_a_wide_inline_err_payload_whole, stash-proven RED on the exact wrong field value, with a BOXED 6-word control and a no-`?` control) |
| B-2026-08-04-13 | codegen | high | The descending-loop bounds-check skip (B-2026-07-17-1) reads the facts it rests on with `stmt_writes_ident`, which sees only TOP-LEVEL assignment tar… | 484c2c9 |
| B-2026-08-04-14 | interp | medium | The interpreter silently DROPPED an out-of-range index-assign: `v[100] = 7` on a 2-element Vec produced no error, no growth, and no store — while AOT… | a4ca760 |
| B-2026-08-04-15 | autopar+codegen | high | AUTO-PAR SILENTLY DROPS STORES through a tuple-element receiver: `t.0.push(x)` recorded NO write in the dependency walk, so two pushes to the same Ve… | 567d5aa |
| B-2026-08-04-16 | codegen+ownership | high | Moving a Vec OUT of a tuple element, mutating it, and moving it BACK (`let mut e = t.0; e.push(x); t.0 = e;`) aborts with `free(): double free detect… | e986284 (src/codegen/stmts.rs tuple-assign arm; src/codegen/expr_ops.rs tuple_index_elem_type_expr). Pins codegen.rs::e2e_named_source_moved_into_a_tuple_element_is_disarmed, memory_sanitizer.rs::asan_named_source_moved_into_a_tuple_element_is_disarmed (floored at 500 allocations against ~1.8k), interpreter.rs::test_named_source_moved_into_a_tuple_element_is_disarmed. Both codegen tests are stash-proven RED against the unfixed compiler (the E2E aborts to empty output, the ASAN case reports a memory error); the interpreter twin passes both ways, as the oracle should. Verified across 16 probe shapes -- both element positions, Vec / String / Vec[String] elements, both-Vec tuples, fresh-temp vs named sources, and the struct-field control -- all clean on both backends with matching alloc/free counts. cargo test --features llvm: 12974 passed, 0 failed. |
| B-2026-08-04-17 | other | medium | memory-fixture authoring hazard: a payload whose content is compile-time constant, or that is read only through `.len()`, is a DEAD allocation LLVM d… | PARTIAL. 49e126a adds the allocation floor (assert_clean_asan_run_min_allocs). dee8b8a adds the KARAC_ASAN_ALLOC_AUDIT=1 corpus sweep and de-vacuums the Vec-grow fixture. 2467458 de-vacuums eight more and makes the floored variant report to the audit. Zero-allocation fixtures 102 -> 93. OPEN for the remainder, whose discard-shaped subset needs a different technique than the other 93 -- see the refinement in detail. 42ababe de-vacuums plain_alias_struct_fields_no_leak. STILL OPEN for the remaining 73, now mechanically identified by the -O0/-O2 differential; see detail. 11e93060 adds the `concurrency` group that the promotion was actually blocked on -- the leg is green (961/0 at c00ea51a) but the CI job was being starved of runners, not failing. STILL OPEN for the branch-protection flip itself. CLOSED 2026-08-07: `Memory sanitizer -O0 (unoptimized allocations)` promoted to a required check on `main` after three consecutive green CI completions (c7fa03c7, b1d8385d, c5239b0d). Bypass deliberately left ALLOWED -- see detail: a required check cannot gate a direct push, so the flag is a formality while this repo commits straight to `main`, and the ratchet in scripts/asan-o0-leg.sh is the actual mechanism. |
| B-2026-08-04-18 | ownership | low | Moving a heap value OUT of an aggregate element and then assigning it BACK (`let mut e = t.0; e.push(x); t.0 = e;`) warns `value 't' moved here, used… | c312c8a2 |
| B-2026-08-04-19 | codegen | high | Double-free (masked at -O2, hard at -O0/JIT): an owned struct/enum binding moved by an ASSIGNMENT — `o.h = h` into a heap-owning user-struct field, o… | 06bf3145 |
| B-2026-08-04-20 | codegen | medium | The `KARAC_TEST_JIT=1` codegen parity leg did not set KARAC_PROGRAM_ARGS, so `env.args().len()` returned 2 (the `karac_jit_runner` argv `[runner, <ir… | 39b0c294 |
| B-2026-08-04-21 | cli | medium | `karac fmt` silently deleted every declaration modifier it had no printer for (`unsafe fn`, `comptime fn`, `comptime` param prefix) | c35df3a |
| B-2026-08-05-1 | codegen | high | Passing a TUPLE ELEMENT to a `ref` parameter (`peek(t.0)` where `fn peek(v: ref Vec[i64])`) double-frees the element buffer under AOT; the struct-fie… | 99d27f7 (src/codegen/call_dispatch.rs — a TupleIndex arm in the ref-argument path, sibling to B-2026-07-12-1's FieldAccess arm). ONE arm closes both rows. Pins codegen.rs::e2e_tuple_element_borrowed_in_place_for_ref_params, memory_sanitizer.rs::asan_tuple_element_borrowed_in_place_for_ref_params (floored at 300 against ~800 allocations), interpreter.rs::test_tuple_element_borrowed_in_place_for_ref_params. Both codegen tests are stash-proven RED against the unfixed compiler (E2E panics with `vec index out of bounds`, ASAN reports a memory error); the interpreter twin passes both ways. cargo test --features llvm: 12980 passed, 0 failed. |
| B-2026-08-05-2 | codegen | high | A `mut ref` parameter given a TUPLE ELEMENT (`bump(mut t.0)`) does not mutate the element under AOT -- the program then PANICS reading the index the… | 99d27f7 (src/codegen/call_dispatch.rs — a TupleIndex arm in the ref-argument path, sibling to B-2026-07-12-1's FieldAccess arm). ONE arm closes both rows. Pins codegen.rs::e2e_tuple_element_borrowed_in_place_for_ref_params, memory_sanitizer.rs::asan_tuple_element_borrowed_in_place_for_ref_params (floored at 300 against ~800 allocations), interpreter.rs::test_tuple_element_borrowed_in_place_for_ref_params. Both codegen tests are stash-proven RED against the unfixed compiler (E2E panics with `vec index out of bounds`, ASAN reports a memory error); the interpreter twin passes both ways. cargo test --features llvm: 12980 passed, 0 failed. |
| B-2026-08-05-3 | codegen | medium | `Option[(Vec[T], ...)]` leaks the tuple payload's heap element when the Some arm binds and reads it -- 32 bytes definitely lost; the struct payload t… | f0aadd9 |
| B-2026-08-05-4 | runtime | high | PERF-REGRESSION introduced by B-2026-07-31-21's fix (75a3a928): a remove-heavy Map runs 1.76x slower because the same-width compacting rehash re-fire… | 73237002 + 45398dd9 |
| B-2026-08-05-5 | codegen+runtime | medium | ARM64 perf regression ATTRIBUTED to 58412d9f (7-bit hash tag in the map bucket control byte); FIX SHAPE REOPENED, was wrongly recorded as settled | c4e6d76e (the tag compare); the placement half is B-2026-08-07-10 |
| B-2026-08-05-14 | codegen | medium | tests/selfhost_codegen.rs (selfhost_codegen_matches_seed_run) is RED on macOS/arm64 for EVERY corpus entry: the self-hosted emitter hardcodes a Linux… | 06a3f683 |
| B-2026-08-05-6 | codegen | medium | Bounds checks survive in a CALLEE that walks a caller-owned buffer at a caller-chosen offset — `fn f(v: ref Vec[u8], base: i64, len: i64) { while lo… | c87f488 |
| B-2026-08-05-7 | codegen | high | ~23 heap-ownership shapes emit a DOUBLE FREE; the `ok_or` String Err payload case is CONFIRMED to abort on a DEFAULT -O2 `karac build` as soon as the… | ef9c1b1 + aaadaac + 33f1ca0 + 0965dbc + 103c518 + 0830bbc (leak A) + f380a8f (leak B) — row STAYS OPEN for the remaining 10, all leaks + 5d48b3c + b359f437 (tensor fresh-temp owned arg) + 70c15e83 (boxed Option/Result payload box, both sites) |
| B-2026-08-05-8 | codegen | medium | `s.contains(other)` on a String bound out of a `Result[String, E]` Ok arm fails to COMPILE -- "Binary op Eq: right operand has non-comparable type {… | b7a0a1d |
| B-2026-08-05-9 | codegen | high | `unwrap_or` with a FRESH F-STRING default leaks on a DEFAULT -O2 build -- 133 leaked allocations in an existing fixture -- while the byte-identical p… | c5dcb1f7 |
| B-2026-08-05-10 | codegen | high | A `ref`-borrowed `shared` handle captured into a `par` branch reads as ZERO under codegen — silent wrong answer, interpreter disagrees | 93b1a81 |
| B-2026-08-05-11 | interp | medium | `File.read` / `BufReader.read` reject a fixed `Array[u8, N]` buffer that AOT accepts — the blessed `let mut buf: Array[u8, N]; f.read(mut buf)` idiom… | FIXED 1caca04. `File.read` and `BufReader.read` in the interpreter now match `Value::Array` alongside `Value::Slice`, taking the whole array as the buffer window (start 0, len = array len). This mirrors the deliberate permissiveness `File.write` already had. AOT was already correct and is untouched. |
| B-2026-08-05-12 | parser | low | the `ref` at a call site diagnostic tells the author to remove one token but carries no machine-applicable replacement, so `karac fix` leaves it | 9b17779 |
| B-2026-08-05-13 | autopar | medium | `karac query concurrency` reports `fanned_out: true` for a disjoint-write loop that runs SINGLE-THREADED when the accumulator is a `mut ref` parameter | 286afea |
| B-2026-08-05-15 | codegen | medium | taking a free function as a VALUE (`let f = g;`) and calling it through the binding fails to build when any parameter is a `ref Vec[T]` -- the indire… | 72f9f49 |
| B-2026-08-05-16 | codegen | medium | NONDETERMINISTIC SEGV at -O0: a bare variant-name pattern whose name is shared by two enums resolves against the UNORDERED enum_layouts map, so per-p… | e3a086e6 |
| B-2026-08-05-17 | cli | medium | `karac build` does not enforce EFFECT errors that `karac check` reports — a program `check` rejects with 1 error builds and runs; type errors ARE enf… | 0bfde1c |
| B-2026-08-05-18 | typecheck+effect | medium | a RESOURCE-LESS effect verb (`panics` / `blocks` / `suspends`) is silently dropped wherever an effect LIST is converted to an effect SET — so a `Fn(.… | 69d630b |
| B-2026-08-05-19 | typecheck+codegen | high | generic args are NOT invariant across numeric element types: `Vec[i64]` is silently accepted where `Vec[u16]` is declared, and AOT then reinterprets… | 80d7a37 (layout rule + literal adoption) + e7a04ac (expected-type seeding; user generics now covered) — `Tensor` still excluded pending B-2026-08-05-26 |
| B-2026-08-05-20 | codegen | high | A whole-value binding-to-binding move of a BOXED-payload `Option` (`let b2 = body;`) double-frees at -O0 -- deterministic, no destructure involved, a… | e7023333 |
| B-2026-08-05-21 | codegen | medium | The INTEGER-OVERFLOW check on an index add `v[base + i]` is still emitted after BCE has PROVEN `0 <= base + i < v.len()` -- a fact that already entai… | 72f9fd7d |
| B-2026-08-05-22 | codegen | high | A fresh-temp aggregate ARGUMENT whose heap lives only behind `Option` fields registered no caller-side cleanup and leaked one payload per call -- 749… | this commit |
| B-2026-08-05-23 | other | medium | the JIT/selfhost oracles report a module that NEVER RAN as an output mismatch: run_ir discarded karac_jit_runner's stderr, so an unresolved external… | 9e25bfaa |
| B-2026-08-05-24 | cli | medium | `main` is RED: tests/cli.rs::wasm_browser_rich_exports_marshal_e2e fails a typecheck since 80d7a37c (B-2026-08-05-19, generic args invariant across n… | 8d6d1b92 |
| B-2026-08-05-25 | typecheck | low | A constant integer EXPRESSION payload does not adopt its expected type in an enum constructor: `Result.Err(0 - 1)` into `Result[i32, i32]` is rejecte… | 895ea26 |
| B-2026-08-05-26 | typecheck | medium | tensor arithmetic infers an f64 element for an f32 operand pair: `let p: Tensor[f32, [D]] = a * k` with `a: ref Tensor[f32, [D]]` and `k: f32` types… | a64e931 |
| B-2026-08-05-27 | codegen | high | The surface-concat RECEIVER gap is only closed for the len-family: `("p:".to_string() + s).starts_with(..)` still leaks the concat on a DEFAULT -O2 b… | this commit |
| B-2026-08-05-28 | codegen | medium | a String-to-String xform does not compile when its RESULT is itself a method RECEIVER — `("p:".to_string() + s).to_uppercase().len()` fails with "no… | 7f067c28 |
| B-2026-08-05-29 | typecheck+cli | medium | a single-target `karac check` silently omits `#[target(T)]`-gated bodies: they are stripped before any pass, so `check` prints "All checks passed" an… | e865f233 + 192947f + aa38d78 |
| B-2026-08-05-30 | other | medium | the wasm E2E tests skip on a SUCCESSFUL build: `wasm_build_skip_reason` matches the string `wasm-tools not found`, which the browser-bindings path em… | c5005e9 |
| B-2026-08-05-31 | interp+codegen | medium | the interpreter computes `Tensor[f32]` elements in f64 while AOT uses a packed f32 buffer, so an f32 tensor gives DIFFERENT ANSWERS on the two backen… | 2bfece1 |
| B-2026-08-05-32 | codegen | high | A struct with a DIRECT `shared` field, bound to a LOCAL and passed BY VALUE, never rc-decs the box -- it leaks on a DEFAULT -O2 build (288 B / 8 allo… | 17b58f4 |
| B-2026-08-05-33 | codegen | high | LAW, not one fixture: a by-value aggregate param that is CALLER-RETAINS is owned by nobody and leaks once per call on a DEFAULT -O2 build | 13a6f9ed fixed (a) by registering the by-value param drop at its DECLARED instantiation (enum_inst_var_types) instead of by bare name. 126180fb fixed (b); 17b58f4 fixed (c). |
| B-2026-08-05-34 | codegen | medium | PERF-REGRESSION, RESOLVED BY MEASUREMENT AND LARGELY NOT A DEFECT: the corpus figure is real but is dominated by e4047440 (AOT integer overflow/div-z… | No karac change, and none is wanted: both causes are correctness work kept as-is -- e4047440 (integer overflow safety) and 2406bab8 (panic-free sort). The surviving optimization headroom is B-2026-08-07-14 (kara's overflow check costs 1.309x of its own unchecked baseline vs rustc's 1.131x). The corpus-join guards are kara-katas 60c76c3 (workload/sink) and 9844c02 (measurement window). |
| B-2026-08-05-35 | other | medium | the ASAN harness SKIPS on a CODEGEN failure ('setup failed -- skipping'), so a memory_sanitizer test written for a shape that does not yet compile re… | 1557d40 |
| B-2026-08-05-37 | codegen+interp | high | a `mut ref` PARAMETER given a PLACE argument silently DISCARDS the callee's write on every backend — `bump(mut g.val)` / `bump(mut t.0)` / `bump(mut… | 1bf6175 (src/codegen.rs `fn_param_mut_ref`; src/codegen/call_dispatch.rs `mut_ref_place_arg_ptr` + the direct-call arm; src/codegen/mono.rs the generic-mono arm; src/codegen/functions.rs + mono.rs registration; src/interpreter/eval_call.rs the CICO write-back; src/interpreter.rs `place_is_writeback_safe`). Pins e2e_mut_ref_place_argument_writes_back, test_mut_ref_place_argument_writes_back, test_mut_ref_place_writeback_skips_impure_subscript. |
| B-2026-08-05-39 | codegen | medium | a `mut ref` AGGREGATE parameter's whole-value REASSIGNMENT stored past its slot — `x = mk()` on a `mut ref String` wrote 24 bytes into the 8-byte all… | 559a8cc (src/codegen/stmts.rs — the aggregate arm beside the scalar `mut ref` assign-through, plus `reclaim_displaced_ref_param_pointee`). Pins e2e_mut_ref_aggregate_param_reassignment_writes_through, test_mut_ref_aggregate_param_reassignment_replaces_pointee, asan_mut_ref_aggregate_param_reassignment_no_leak. |
| B-2026-08-05-40 | codegen | medium | a `Slice[T]` / `mut Slice[T]` parameter fed from a PLACE (`f(g.a)`, `f(g.q.a)`, `f(t.0)`, `f(vv[0])`) did not COMPILE — the Vec's 3-word `{ptr,len,ca… | 0949f9f (src/codegen/expr_ops.rs — the place arm in `coerce_to_slice`; src/codegen/call_dispatch.rs — the ref-slot guard). Pins e2e_slice_param_from_a_place_argument, test_slice_param_from_a_place_argument, asan_slice_param_from_a_place_argument_no_leak. |
| B-2026-08-05-41 | typecheck+codegen | medium | a `shared struct` field reached through a `mut ref` ARGUMENT bypasses the immutable-field write gate that rejects the assignment spelling, and the wr… | de799c3 (src/typechecker/fields.rs the `SharedFieldNotMut` gate's `mut ref` argument context; src/codegen/call_dispatch.rs `shared_mut_ref_place_arg_ptr` + `is_pure_field_chain`, replacing `mut_ref_place_arg_ptr`'s explicit shared bail). Pins e2e_mut_ref_place_argument_shared_receiver_writes_back, test_mut_ref_place_argument_shared_receiver_writes_back, asan_shared_field_mut_ref_arg_string_no_leak, and four typechecker gate tests. |
| B-2026-08-06-1 | codegen | medium | a generic wrapper's bare `T` field bound to a MAP leaks its whole handle tree: `fn sink(b: Box[Map[i64, String]])` loses 25,830 B / 40 blocks on a DE… | 5c43517 (src/codegen/synth_drop.rs — the Map/Set arm in the subst-driven bare-generic-param reclassification loop; src/codegen/call_dispatch.rs — bare-param head resolution in `zero_struct_move_caps_mono`, plus the new `zero_struct_field_move_cap_inst` threading the source binding's recorded instantiation from `suppress_source_vec_cleanup_for_arg_ex`'s FieldAccess arm). Pins e2e_bare_generic_param_map_field_is_freed_and_neutralized, asan_bare_generic_param_map_field_is_freed_and_neutralized, test_bare_generic_param_map_field_transfers_the_handle. |
| B-2026-08-06-2 | codegen | high | TWO defects on the by-value generic-struct param path, and THIS ROW'S OWN 'clean' CONTROL WAS THE WORSE ONE: (A) the CONCRETE spelling `fn take(b: Bo… | 933b859 (defect A, the concrete-param double free) + eefe88a (defect B, the generic-param leak) |
| B-2026-08-06-4 | typecheck+codegen | medium | a `shared struct`'s Vec field passed to a `mut Slice[T]` parameter does not COMPILE (LLVM module verification hard-fails) even when the field is decl… | 28ceec6b |
| B-2026-08-06-5 | codegen | high | A cast TO `char` inside an f-string hole is DROPPED by both compiled backends -- `println(f"{b as char}")` prints the integer codepoint (98) where th… | 335540c9 |
| B-2026-08-06-6 | codegen | high | a `Map`/`Set` field moved out INDIVIDUALLY left the source handle live, so the owner's struct drop freed storage the destination still owned — `fn ta… | 26b2176 (src/codegen/call_dispatch.rs — the Map/Set arm in `zero_struct_field_move_cap`, plus the Sorted variants in `zero_struct_move_caps_mono`'s existing arm). Pins e2e_map_field_move_out_neutralizes_the_source, test_map_field_move_out_transfers_the_handle, asan_map_field_move_out_neutralizes_the_source. |
| B-2026-08-06-7 | codegen+interp | high | shift by >= the bit width is UNDEFINED BEHAVIOUR in AOT output — one `let` variable prints two different values in the same run and different values… | ee551b8 (shift legs) + <this commit> (negation leg) |
| B-2026-08-06-8 | codegen | low | a generic wrapper's bare `T` field bound to a SHARED struct leaks 2,560 B / 80 blocks at -O0 (clean at -O2): `Box[T]` at `T = Node` where `Node` is a… | 0928227 (src/codegen/param_own.rs — `struct_owns_shared_field_subst`, the gate; src/codegen/synth_drop.rs — `emit_nested_struct_shared_rc_decs_ex_mono`, the walker; src/codegen/runtime.rs — `emit_vec_elem_struct_with_shared_drop_fn_mono` plus the `track_struct_var_inst` call site; src/codegen/call_dispatch.rs — the `shared` arm in `zero_struct_field_move_cap_impl`; src/codegen/stmts.rs — the FieldAccess arm in `suppress_block_tail_cleanup`). Pins asan_bare_generic_param_shared_field_is_rc_dec_and_neutralized, e2e_shared_field_moved_out_of_a_value_block_survives_the_frame_drain, test_bare_generic_param_shared_field_transfers_the_handle. |
| B-2026-08-06-9 | codegen | medium | TWO shapes where a heap-BOXED enum payload loses its owner AT A CALL BOUNDARY: (A) [FIXED] a NAMED `Option` binding passed by value has its let-site… | 9370f723 (leg B: fresh-temp enum arg registrar asks `enum_drop_switch_does_work`); de8881f8 (leg A: the callee owns a boxed non-struct Option payload) |
| B-2026-08-06-10 | codegen | medium | a callee arm that MOVES A FIELD OUT of a boxed `Option[Struct]` param orphans the box the caller's struct drop owns: `fn f(h: Option[H]) { match h {… | e6a0a5a1 |
| B-2026-08-06-11 | codegen | high | an owned boxed-payload enum param that ESCAPES -- returned, or forwarded to another by-value param -- is freed by the callee anyway: `fn id(o: Opt[St… | 05005aae |
| B-2026-08-06-12 | codegen | high | a GENERIC struct LITERAL used directly as a METHOD RECEIVER cannot be built: `Box { v: <String> }.take()` passes `karac check`, runs correctly under… | 2a985b08 |
| B-2026-08-06-13 | lexer+parser | low | `i64::MIN` cannot be WRITTEN as a literal in any spelling -- `-9223372036854775808i64` is a parse error (`Invalid integer literal`), because a negati… | <this commit> |
| B-2026-08-06-14 | codegen | high | a `shared` field RETURNED out of a BY-VALUE struct param is a use-after-free on a DEFAULT -O2 build: `fn giveback(b: Holder) -> Node { return b.v; }`… | 257630a (src/codegen/call_dispatch.rs — the new `share_direct_shared_field_ref_for_return`, which gates on the caller-retains regime using the SAME name-only predicate the param_own.rs arm gates on so the two cannot drift; src/codegen/exprs.rs — the explicit-`return` hook; src/codegen/stmts.rs — the tail hook, on the early-return side of `compile_tail_final_expr`'s `tail_inner` gate). Pins e2e_shared_field_returned_from_a_caller_retains_param_keeps_its_ref and test_shared_field_returned_from_a_param_transfers_the_handle. |
| B-2026-08-06-15 | codegen | medium | a `shared` handle escaping a VALUE-POSITION BLOCK is never rc-dec'd by its consumer binding: `let x = { let b = Box { . | e954652 (src/codegen/stmts.rs — `suppress_block_tail_cleanup`'s FieldAccess arm records the transfer via the new `tail_field_is_direct_shared`, and the let-site receive-inc consumes it; src/codegen.rs — the `block_tail_shared_transfer` channel). Pins asan_shared_field_escaping_a_value_block_transfers_exactly_one_ref and test_shared_field_escaping_a_value_block_transfers_the_handle. |
| B-2026-08-06-16 | typecheck | low | the upper half of u64 is unwritable as a literal -- `18446744073709551615u64` (and any magnitude above i64::MAX, in any radix) is a parse error, beca… | <this commit> |
| B-2026-08-06-17 | typecheck+codegen | medium | `ref CStr` as an `unsafe extern "C"` PARAMETER type is accepted by the typechecker and then dies at codegen with a raw LLVM module-verification error… | adaa9e34 |
| B-2026-08-06-18 | codegen | high | a u64 ARITHMETIC RESULT above i64::MAX renders SIGNED under both compiled backends but unsigned under the interpreter -- `println(u64.MAX - 1u64)` pr… | ac3041ce |
| B-2026-08-06-19 | codegen | medium | a chained FIELD ACCESS on a generic method's RETURN cannot be built: `w.take().f` fails `karac build` with "cannot resolve field 'f' on this receiver… | FIXED (src/codegen/expr_ops.rs: new `generic_method_return_inst` substitutes the RECEIVER's instantiation into the method's declared return TypeExpr, recovered from the AST the declare loop keeps in `generic_fns`; consulted from `enum_inst_type_of_expr`'s new MethodCall arm — placed BEFORE the span fallback — and from `type_name_of_expr`'s MethodCall tail). Pins tests/codegen.rs `e2e_chained_field_access_on_generic_method_return`, stash-proven RED. |
| B-2026-08-06-20 | codegen | medium | two instantiations of ONE generic struct reached through a MIX of literal and named receivers collide on the unmangled `@Type.method` symbol, whose s… | 2fbf970d |
| B-2026-08-06-21 | codegen | high | a boxed `Option`/`Result` binding passed by value to a PASSTHROUGH callee is freed TWICE at -O0: `fn id(o: Option[Option[i64]]) -> Option[Option[i64]… | fe7fea77 |
| B-2026-08-06-22 | codegen | medium | a DOUBLY-nested generic chain `outer.take().take().f` at `Box[Box[Wide]]` cannot be built: type resolution SUCCEEDS (the field index resolves to Some… | cfb149aa |
| B-2026-08-06-23 | codegen | medium | a generic struct LITERAL in RECEIVER position whose field initializer type cannot be NAMED lowers at the ERASED `{i64}` layout: `Box { v: f * 2.0 }.t… | FIXED (src/codegen/call_dispatch.rs: new `scalar_type_name_of_expr` + `is_scalar_type_name`, chained onto `infer_arg_elem_te`'s namer). Pins tests/codegen.rs `e2e_generic_receiver_literal_with_arithmetic_field_initializer`, stash-proven RED. |
| B-2026-08-06-24 | interp | medium | EVERY `extern` call under `--interp` reports an "internal .. | 3607c241 |
| B-2026-08-06-25 | codegen | medium | the generic-impl MONOMORPH NAME mangles only the type argument's HEAD, so `Box[Box[Wide]]` and `Box[Box[Box[Wide]]]` (and `Box[Box[i64]]`, `Box[Box[S… | FIXED (src/codegen/mono.rs: new `append_nested_instantiation_mangle` appends `$<param>_gi_<token>` when a type argument is ITSELF a generic instantiation of a USER struct/enum, deriving the concrete args from the RECEIVER's recorded instantiation). Pins tests/codegen.rs `e2e_two_nested_generic_instantiations_get_distinct_monos`, stash-proven RED. |
| B-2026-08-06-26 | codegen | medium | a boxed `Result` passed through a passthrough callee and matched with a payload-BINDING arm still double-frees at -O0: `Result[Wide, i64]` where `Wid… | d1aa4477 |
| B-2026-08-06-27 | codegen | high | an INLINE heap `Option[String]` payload passed by value to a passthrough callee is freed TWICE at the DEFAULT -O2 as well as -O0, when the payload is… | edee55d9 |
| B-2026-08-06-28 | codegen | high | a DISCARDED passthrough call result double-frees its argument's payload at the DEFAULT -O2: `let d: Option[String] = Some(mk()); idopt(d);` aborts wi… | FIXED (src/codegen/runtime.rs: new `discarded_temp_aliases_armed_source` declines to register a discarded temp's free when the call is a passthrough of an armed source, applied to the inline-Option, inline-Result and boxed-Option registrars; src/codegen/control_flow_match.rs: `call_passthrough_armed_any_source` widens B-2026-08-06-27's detector to the boxed armed set WITHOUT changing that row's let-site gate). Pins tests/memory_sanitizer.rs `asan_discarded_passthrough_result_does_not_free_source_payload`, stash-proven RED. |
| B-2026-08-06-29 | codegen | high | a BOUND-then-CONSUMED passthrough result double-frees at the DEFAULT -O2: `let x = idopt(d); peek(x);` aborts with `free(): double free detected in t… | FIXED (src/codegen/control_flow_match.rs: new `moved_arg_owner_name` resolves a moved-arg name through `passthrough_owner_alias` before the consume-suppressors look it up, so the suppression lands on the binding that actually owns the payload). Pins tests/memory_sanitizer.rs `asan_bound_passthrough_result_consumed_by_callee_frees_once`, stash-proven RED with a real ASAN error. |
| B-2026-08-06-30 | codegen | medium | a SELF-RECURSIVE reduction was scored at ~15,000,000 units per iteration (64^3, the depth cap unrolling the same body and compounding the nested-loop… | a23262c |
| B-2026-08-06-31 | codegen | medium | the STRUCT-payload sibling of B-2026-08-06-9 leg A: a boxed struct `Option` payload lost its owner across a by-value call, in both the FRESH-TEMP for… | efc9b308 (fresh-temp half); 85334b0f (retracts the first named-binding attempt); 72840c86 (named half + the let-destructure mirror, both sites) |
| B-2026-08-06-32 | codegen | medium | a heap-BOXED `Option` payload nested inside a `Result`'s INLINE payload area has no owner on any side -- 32 B per construction at -O0 | c6eaeea |
| B-2026-08-06-33 | codegen | medium | SETTLED, lane A's DIRECTION confirmed: on x86_64 the map hash-tag compare PAYS for primitive keys, so B-2026-08-05-5's `!self.target_is_aarch64` cond… | e7a5ebc — SETTLED BY MEASUREMENT 2026-08-07, no behaviour change: the shipped x86 default already was the faster arm. That commit carries the EVIDENCE, in src/codegen/mono.rs::map_tag_compare's doc comment. |
| B-2026-08-06-34 | other | medium | MAIN IS NOT RED and the four cells are NOT fixed: the ownership-matrix ratchet reported four `Leak`->`Clean` flips because LeakSanitizer was INERT (t… | 84529948 |
| B-2026-08-07-1 | codegen | high | a BOXED-payload enum binding that is RETURNED is freed by its own frame -- the caller reads and frees the same box: double free + use-after-free at B… | ae74c7f |
| B-2026-08-07-2 | codegen | medium | the remaining owner sites for a box nested in a `Result`'s INLINE payload area -- ALL SIX RESOLVED | b55a849,d1aa710,3176865 |
| B-2026-08-07-7 | codegen | medium | a struct field of type `Option[Option[String]]` DOUBLE-FREED the String at BOTH opt levels when a match arm bound it out -- FIXED (c25c949) by disarm… | c25c949,5f47dc6 |
| B-2026-08-07-9 | codegen | high | a match ARM that binds a struct field's whole boxed payload out and destructures it in a SECOND match double-frees the interior at both opt levels; t… | ba64b21 |
| B-2026-08-07-6 | codegen | low | the DIRECT sibling of the nested-box chain: `Option[Option[Option[i64]]]` with no `Result` wrapper leaks its inner envelope -- `BoxedEnumDrop` frees… | 28e17d9d |
| B-2026-08-07-4 | codegen | medium | reassigning a `let mut` binding whose enum payload is heap-BOXED leaks the OVERWRITTEN value's box -- the store site frees nothing; 32 B per overwrit… | da29013 |
| B-2026-08-07-3 | codegen | high | `<Result/Option binding>.map(f)` whose ABSENT branch (`Err`/`None`) carries a heap payload double-frees when the map result is CONSUMED or DISCARDED:… | 94ead2dc |
| B-2026-08-07-5 | codegen | medium | `b = c` between two boxed-payload enum bindings leaves BOTH slots holding one box and both armed -- glibc double free at -O0 | a635ffe |
| B-2026-08-07-8 | typecheck | low | `self` cannot be passed to a BORROW parameter from any receiver mode — `byref(self)` from `ref self` is `expected 'ref Inner', found 'Inner'`, and so… | aa3b394 |
| B-2026-08-07-10 | codegen | medium | PLACEMENT SENSITIVITY CONFIRMED AND PRICED ON arm64, BUT THIS ROW'S OWN 1.09x DOES NOT REPRODUCE: vanilla builds of b84477dd and 36a7fa5a are 1.0000… | No default change, and none is warranted from one kata -- the row closes by measurement. Two levers shipped as the instruments in c538a878: `KARAC_TEXT_PAD=<bytes>` (a non-eliminable filler ahead of `main`, so placement can be VARIED rather than quantised) and `KARAC_LLVM_ARGS="<flags>"` (clang's `-mllvm`, which is the only way to reach the block-alignment cl::opt family from codegen). Both are behaviour-neutral by test, and the `KARAC_LLVM_ARGS` test asserts the flag REACHES THE BACKEND rather than that the build survived it -- a silently-forwarding-nothing lever would make every measurement a measurement of the default. Adopting block alignment as a default is B-2026-08-07-24. |
| B-2026-08-07-11 | codegen | medium | the envelope chain is owned only at the LET site: passing a boxed-chain enum by value to an owned param leaks 320 B/10, and moving it into a struct l… | PARTIAL. d5cfa3c8 fixes leg (a), the owned param (320 B / 10 -> clean). Legs (b) struct-literal move and (c) `Vec` push STAY OPEN and are re-diagnosed in detail: they are NOT chain gaps -- a single-box `Option[Option[i64]]` leaks 320 B through either -- but a destination-ownership defect, so the chain must not be fixed there before the owner is. 49a12153 fixes leg (b), the struct-field chain, by giving `emit_option_drop_fn`'s boxed branch the recursion it lacked (1,280 B / 40 -> clean). It became a pure chain gap only after 31768650 gave the outermost envelope a field owner. STILL OPEN for leg (c), the `Vec` push -- which is NOT a chain gap, the single-box case leaks too -- and for a newly measured residual, `struct Hs { b: Option[Option[Option[String]]] }`, which takes the classifier's other branch and is unchanged by this. f5df577f fixes leg (c), the `Vec` push, by giving the element arm the same `option_payload_boxed_envelope_only` admission the struct field got in 31768650. SEVERITY RAISED low -> medium: leg (c) leaks at the DEFAULT -O2 (5,120 B + 3,840 B indirectly), which this row twice asserted was impossible for the family — that claim was carried forward from legs (a)/(b) rather than measured. All three filed legs are now closed; the row stays open only for the `struct Hs { b: Option[Option[Option[String]]] }` residual. 61c69e0d fixes the last residual — the MATCH-ARM parked envelope (`own_boxed_option_field_envelope_at`), a fourth site neither leg reached — by giving it the same chain. ROW CLOSED: all three filed legs plus the residual. |
| B-2026-08-07-12 | codegen | medium | a fresh-temp struct literal passed BY VALUE leaked the heap inside an `Option`/`Result` field at BOTH opt levels, and the callee's entry copy of a BO… | 00660c3,e0d88f2, and leg 1 5ab6e8e8 (synth_drop.rs promotion gate + payload_droppable; control_flow_match.rs both legs of place_optres_field_move_info_ex; runtime.rs option_payload_map_or_set_drop_ok). The Result twin is B-2026-08-07-19, not this row. |
| B-2026-08-07-13 | other | low | docs/bug-ledger.jsonl has no pinned JSON encoding, so writers flip it between raw UTF-8 and \uXXXX escapes and each flip rewrites all ~1000 rows | 9636a9b6 |
| B-2026-08-07-14 | codegen | medium | RESOLVED, NOT A DEFECT: the i64 OVERFLOW CHECK's real cost is LOST AUTO-VECTORIZATION -- the trap branch is a second loop exit, so kara drops from AV… | No karac change, and none is available -- this row closes by measurement and decision, not by a commit. All four directions are now closed. (1) A cheaper instruction sequence: karac emits the SAME `llvm.s{add,sub,mul}.with.overflow` intrinsic rustc does, so there is nothing to diff at the IR level. (2) Trap-block placement: already outlined `cold` + `noinline` + `noreturn`, ending in `unreachable`. (3) Range analysis: BUILT AND REJECTED 2026-07-03 (`docs/spikes/overflow-check-elision.md`), with the rejection promoted into design.md § Arithmetic Overflow as a standing decision. (4) The vectorizable checked reduction this row proposed: ILLEGAL -- checked add is non-associative, so partitioning a reduction into lanes deletes traps the language promises. Counterexample runs at HEAD: over [2^62, 2^62, -2^62, -2^62] the sequential form panics with integer overflow (exit 1) and the 2-lane form prints 0 (exit 0). Branchless reformulations do not rescue a reduction either (0 ymm for all three under clang 18 -O3 -mavx2, against a wrapping control at 8), because the overflow flag still depends on the sequential prefix. design.md already prices the residual as "the gap IS the guarantee, not a defect" and names `wrapping_*` plus the deferred scoped `#[wrapping]` region as the sanctioned levers. The elementwise sibling shape, which IS legal and DOES vectorize, is filed separately as B-2026-08-07-21. |
| B-2026-08-07-15 | codegen | high | a struct that is NOT copy-supported (any `Map`/`Set` field) passed as a FRESH TEMP by value is dropped by BOTH frames -- 480 valgrind errors, 175 inv… | db16f10 |
| B-2026-08-07-16 | codegen | low | the x86 hash-tag probe spills 2 of the caller's registers per key-probe (4 memory ops) because `ctrl` puts the loop one value over x86-64's 15 GPRs —… | 1d0fe05d lands NO speedup: the `KARAC_MAP_PROBE` A/B lever and the three LOOKUP probe sites folded into one `emit_lookup_probe_cursor` helper (default `Bounded` proven byte-identical to the pre-refactor compiler on all three katas), plus a comment on `runtime/src/map.rs`'s growth guard naming the termination invariant the off-default forms depend on. The row closes because every lever it named is now priced: cachegrind says -25% / -32% instructions on kata:170, the clock says -3% at best and +11% on kata:217. Two instruments, opposite signs, second time in this family after B-2026-08-05-5. |
| B-2026-08-07-17 | codegen | high | B-2026-08-07-15's own-by-transfer gate exempted EVERY generic struct, and a generic struct at a CONCRETE param (`fn take(x: Mix[String])` over `Mix[T… | 0cf32de |
| B-2026-08-07-18 | codegen | high | naming a generic fn's type param differently from the struct's silently miscompiles: `fn f[U](x: Mix[U])` is 30 valgrind errors / 10 invalid frees /… | 592216a0. Half 1 (callee): `compile_mono_function` hoists `concrete_generic_struct_inst(param.ty)` above the ownership call and passes it to `make_aggregate_param_callee_owned_inst`, so the own-by-transfer drop binds the STRUCT's params positionally instead of being synthesized against the erased layout. `inst` reaches only `track_struct_var_inst`, never an arm-selection predicate, so no ownership arm moves — which is what the rejected scoped-subst overlay got wrong. Half 2 (caller): `compile_generic_call` asks `struct_param_owned_by_transfer` per IDENTIFIER argument under the callee's substitution and retracts via `move_declined_copy_struct_arg`; that lockstep existed on the concrete path only, and half 1 alone turned its absence into double frees in C/D/Pair/Trip. 32/32 matrix cells clean (8 shapes x 2 spellings x 2 opt levels). Fixture `asan_diverging_type_param_name_generic_struct_param`, stash-proven red. |
| B-2026-08-07-19 | codegen | medium | the `Result` twin of B-2026-08-07-12 leg 1: a `Result[Map[K,V], E]` STRUCT FIELD leaks its whole handle tree, 720 B / 10 iterations on a DEFAULT -O2… | 31755b38. New scoped `result_field_map_or_set_half_ok` (runtime.rs) admitting a `Map`/`Set` handle half, routed straight to `emit_result_drop_fn` from the promotion loop's Result arm, and threaded through the three paired move sites (`place_optres_field_move_info_ex`'s Result branch + two destructure-leaf registrations in stmts.rs). Neither trap predicate was widened. The pairing rule fired exactly as this row predicted: classifier alone turned four clean shapes into 470 valgrind errors. 18/18 matrix cells clean at both opt levels; fixture `asan_result_map_half_struct_field_freed_and_move_paired`, stash-proven red. |
| B-2026-08-07-20 | codegen | medium | a SHARED-owning struct never frees its `Option[Map]`/`Option[Set]` field -- 720 B / 10 iterations at BOTH opt levels for a plain `let`, no call in th… | 1ee40f33. Third disjunct `shared_owning_struct_sole_field_owner` at the `emit_struct_drop_synthesis_impl` promotion gate — CALLER-RETAINS as a third way to have one owner, beside copy-support (duplication) and own-by-transfer (transfer) — paired in lockstep with three sites that must agree with it: the for-loop strict-shared `Option` ban in `field_copy_supported` (B-2026-07-18-2, whose stated premise this change inverts), `finish_owned_struct_destructure`'s `transferable`, and a new `Option[Map]`/`Option[Set]` leaf arm in `track_owned_destructure_field_cleanup`. Scoped by `struct_used_as_bare_by_value_param` — whole-program and type-keyed, matching the per-TYPE synthesis — because the frame-boundary half (B-2026-08-08-6) needs a callee-body predicate. Also closes phase-12 #43; its `#[ignore]`d reproducer is un-ignored. Fixture `asan_shared_owning_struct_option_map_field_freed`, stash-proven red at 4,320 B / 60 blocks at both opt levels, plus the scope guard `asan_shared_owning_struct_option_map_by_value_param_single_owner`. |
| B-2026-08-07-21 | codegen | low | CLOSED AS PRICED, PREVALENCE ZERO: elementwise checked arithmetic does lose auto-vectorization (4.22x measured in kara) and, unlike the reduction cas… | No code change — closed as priced, on the prevalence number this row itself said should decide it. Scanned 230 local `.kara` files (stdlib, examples, example projects, selfhost) for an indexed-destination assignment with trapping arithmetic: 121 raw hits, 100 of them in one generated compile-speed benchmark, 21 in real code, and ZERO surviving hand-verification against the row's own conditions — each disqualified independently (a call in the RHS in sha256/sigv4, a real loop-carried dependence in coin_change, a data-dependent scatter index in grade_histogram, division-plus-calls in embeddings, and a five-iteration trip count in the one structurally-qualifying case, slice_basics). Two findings recorded on the way: (a) this row's blocker (1), site attribution, is WRONG to call fatal — it dissolves under a single-checked-op restriction, because `emit_panic` takes a static string and one candidate site means the deferred trap reports an identical message and location, and the single-op body is the vectorizing shape anyway; (b) `src/codegen/accum_overflow.rs` (B-2026-07-26-1) is the repo's one successful attack on this class and it took ELISION with a trip-count proof, paying none of the four observability blockers, for a 7.9x win — a route unavailable here because the operand is an unbounded heap value, which is exactly the residual `docs/spikes/overflow-check-elision.md` declined to build a prover for. Reopen criterion: run the same scan over the kata corpus (absent from a cloud clone) and reopen if more than a couple of loops survive. |
| B-2026-08-07-22 | interp | high | a `par {}` block inside a `while` loop HANGS under `--interp` (the DEFAULT for `karac check`-adjacent workflows and the Mend oracle) while the identi… | b64a8b47 — `eval_par_block` now pushes a scope in each branch interpreter after seeding the environment snapshot, so the join merges only the bindings the branch INTRODUCED instead of the whole flattened enclosing environment. The merged snapshot was shadowing every outer variable in the parent's current scope, which swallowed any assignment made after the par block inside a nested block. |
| B-2026-08-07-23 | ownership | high | a `frozen` handle has no legal place to be STORED, so no iterative traversal can use one: a local `VecDeque[Node]` worklist refuses `queue.push_back(… | 63caea3e |
| B-2026-08-07-24 | codegen | medium | 64-BYTE BASIC-BLOCK ALIGNMENT is a MEASURED 10.0% on kata:170 -- its aligned placement DISTRIBUTION entirely dominates the unaligned one, worst align… | DECLINED, PRICED, no code change -- and closed rather than parked, because all three shipping paths are settled noes rather than deferrals. (a) Carrying an LLVM patch for the Apple subtargets' `PrefLoopLogAlignment` is disqualified by DISTRIBUTION, not effort: karac builds against whatever LLVM the user has, so a fork breaks `cargo install karac --features llvm`, which is a launch gate traded for one kata. (b) Upstreaming needs multi-host, multi-workload data and would reach users only at LLVM 21+; the measurement here is a decent seed for it and nothing more. (c) Shipping the blanket flag as a documented opt-in is WORSE than doing nothing: `-align-all-nofallthru-blocks` is an internal cl::opt in MachineBlockPlacement.cpp with no stability guarantee, and `KARAC_LLVM_ARGS` silently ignores an unknown flag (verified), so an LLVM upgrade that drops it would revert the optimization with zero signal -- acceptable for a measurement lever, a trap for a user-facing recipe. REOPEN CONDITION, and it is a real one: if a Kara workload that actually ships -- not a kata -- is found placement-pathological the way kata:170 is, the 10% becomes worth an LLVM dependency and (b) gets its justification. `KARAC_TEXT_PAD` makes that detectable on any program in minutes, so the condition is testable rather than aspirational. |
| B-2026-08-07-25 | other | medium | A BENCHED KATA'S HEADLINE NUMBER CAN CARRY A 1.31x PLACEMENT RANGE BEHIND IT AND THE CORPUS HAS NEVER BEEN CHECKED FOR IT: kata:170's recorded figure… | No compiler change -- the row was a reporting risk, not a defect. Discharged in kara-katas f152a64 (the screen + the spread/margin join), ba7872d (interleaving + per-kata control, which corrected the screen's first corpus run) and 22baac1 (the 258-pair corpus measurement, the 13 README caveats, and BENCHMARKS.md's new 'Code placement (arm64)' section). Keeping it current needs no new machinery: re-run `placement-spread.py --all` then `stamp-placement-caveat.py`, both idempotent, whenever benchmark numbers are refreshed for publication. Deliberately NOT wired into every bench run -- it adds minutes per kata to measure something that moves only when emitted code size moves. |
| B-2026-08-07-26 | ownership | high | a frozen-element container declared INSIDE a closure was still admitted -- `try_admit_container_method` had no `in_closure` gate, so an escaping clos… | 6673e162 |
| B-2026-08-07-27 | other | medium | kata #133's par lane compiles and runs after B-2026-08-01-33, but restoring it is the whole bench pipeline -- the .kara's "DOES NOT COMPILE" header,… | kara-katas 2603167 |
| B-2026-08-08-1 | ownership | medium | the `par` capture gate is keyed on binding NAMES, so two branches that each declare their own local `let n = <shared>` read as ONE binding reachable… | 8c26ad4a |
| B-2026-08-08-2 | typecheck+ownership | high | this row's PREMISE WAS WRONG -- kata #133 was never blocked on `Map[K, frozen V]` (its `visited` map holds the CLONES, which are mutated and can neve… | 502a598f |
| B-2026-08-08-3 | codegen | medium | a `par` branch whose body is a BLOCK EXPRESSION containing a `Map` fails codegen with `Undefined variable '<outer destructured name>'`, while `karac… | da9ae409. Two holes in the par-branch return-slot type inference (`src/codegen/stmts.rs`): `infer_block_tail_llvm_type` now honours an inner `let`'s type ANNOTATION (it consulted only the RHS, while its own caller checks the annotation first), and `infer_expr_llvm_type`'s `MethodCall` arm types a builtin container/string `len()` from `method_callee_types` — the typechecker's own span-keyed record of the receiver — restricted to methods with one possible return type across every receiver. Neither the `Map` nor the slot-ownership transfer this row named is involved; Vec/String/Set reproduce identically, and the reported name is the BRANCH binding, not the outer destructured one. 18 variants build and agree with the interpreter; valgrind clean at both opt levels. Fixtures `par_branch_block_body_ending_in_container_len` and `par_branch_block_body_binding_distinct_from_destructured_name`, both stash-proven red. |
| B-2026-08-08-4 | ownership+typecheck | high | a CONTAINER-mediated strong cycle in a `shared struct` (`mut ns: Vec[N]`, `mut next: Option[N]`) is accepted and leaks the whole graph -- design.md s… | 68d0b86b |
| B-2026-08-08-13 | codegen | high | a `Vec[weak N]` push SILENTLY CORRUPTED the target's first field — `w.push(a)` changed `a.v` from 41 to 42, because the weak-target scan never saw th… | f869989f |
| B-2026-08-08-14 | interp | medium | the interpreter has no weak CONTAINER element, so a `Vec[weak T]` read-back reports `non-exhaustive match .. | f0c47394 |
| B-2026-08-08-5 | typecheck+codegen | high | the `weak` downgrade store coercion reaches a direct FIELD store but not a container-ELEMENT store, so `Vec[weak N]` cannot be built -- which makes d… | 1bb5328a |
| B-2026-08-08-6 | codegen | medium | a caller-retains struct PARAM whose callee moves a promoted `Option`/`Result` field out has two owners | 9b4abaf0 |
| B-2026-08-08-7 | other | medium | six ASAN fixtures were passing VACUOUSLY — their programs fail typecheck and `karac build` would refuse them, but the harness only ever asserted the… | 7c7de323 |
| B-2026-08-08-8 | typecheck | low | expected-return seeding reaches only PATH callees, and its argument checking only collection literals — a plain generic free fn still rejects a conte… | a2ce6b79 |
| B-2026-08-08-9 | typecheck | high | a generic slot bound by the EXPECTATION skipped the narrowing check — `let x: u8 = id(big)` with `big: i64 = 5000000000` typechecked and printed 5000… | a2ce6b79 |
| B-2026-08-08-10 | codegen | medium | a generic struct with a `Vec[T]` FIELD returned by value from a generic function fails codegen — `ret { { ptr, i64, i64 } } %field` against a `ptr` r… | f773c317. `llvm_type_for_type_expr` (`src/codegen/types_lowering.rs`) now lowers a user-declared struct through its own (mono) struct type when its name collides with a prelude type, ahead of the hard-coded `name == "Column"` / `"DataFrame"` / `"Tensor"` / `"Interner"` handle arms that keyed on the NAME alone and returned a bare `ptr`. Guarded on `user_shadowed_prelude_types`, the declaration pass's existing record, which excludes `stdlib_origin` items. NOT the generic-struct-return gap this row describes: a non-generic `struct Column { data: Vec[i64] }` fails identically pre-fix, and renaming the struct is a one-token fix. The complement of `reject_shadowed_prelude_types` (B-2026-08-02-13), which still refuses built-in machinery over a user value — all 17 of its tests pass unchanged, as do the 159 built-in Column/DataFrame/Tensor/Interner/Arrow codegen tests. Fixture `user_struct_shadowing_a_prelude_type_name_keeps_its_own_layout`, stash-proven red. |
| B-2026-08-08-11 | typecheck | low | a type error about a `frozen` parameter names its type `ref T` -- the surface keyword and the diagnostic disagree, because `frozen T` lowers to `Ref(… | 4f32b1e1 |
| B-2026-08-08-15 | autopar+codegen | high | an RC-bearing `shared struct` published as an auto-par return slot is never adopted by the joining scope -- it LEAKS when the branch suppresses its r… | 62619a88 |
| B-2026-08-08-16 | other | medium | the ASAN memory suite compiles every fixture with AUTO-PAR DISABLED, so ~1000 leak/UAF fixtures cover sequential codegen only -- the hole that hid B-… | 74bf4856 |
| B-2026-08-08-23 | autopar+codegen | medium | an auto-par branch containing a `while let` never captured the names its scrutinee reads — `refs_in_expr` had no `WhileLet` arm, so `karac build` ref… | 74bf4856 |
| B-2026-08-08-17 | autopar+codegen | high | a closure's write through a captured `String` is SILENTLY LOST when the analyzer parallelizes the enclosing function -- `karac build` and `karac run`… | 10659bf4 |
| B-2026-08-08-18 | autopar+codegen | medium | a `Column` arithmetic chain passed to a two-arg fn emits a malformed call under auto-par -- LLVM module verification rejects `call i64 @fst(i64 %m8,… | 31208e3a |
| B-2026-08-08-19 | autopar+codegen | medium | a user method on a `shared struct` loses its dispatcher under auto-par -- `codegen: no handler for method 'total' on variable 'b' (method dispatch fe… | ce1b8703 |
| B-2026-08-08-20 | codegen | high | `Vec[String].first()` / `.last()` CONSUMED AS A VALUE double-frees under EVERY codegen backend -- `println(v.first().unwrap())` on a two-element `Vec… | PENDING |
| B-2026-08-08-21 | codegen | low | `Option/Result.map` with an UN-ANNOTATED closure returning a String/Vec is refused by `karac build` -- a loud, actionable bail ("annotate the closure… | 8521c30e |
| B-2026-08-08-22 | codegen | low | a closure whose body IS a String value (bare literal or `+` concat) is declared with a POINTER return, so the `{ptr,len,cap}` it yields fails LLVM ve… | 51ceecab. `infer_closure_return_type` types a `StringLit` body and a `+` chain whose either side is a String as the `{ptr,len,cap}` String struct rather than a bare pointer, so the `ret` matches the declared signature. The `closure_body_is_bare_string_value` gate in the `map` heap-payload path is deleted along with its now-dead helper: it existed only to reject these bodies with advice (annotate the parameter) that measurement showed does not help — `|s: String| "fixed"` failed identically. Pinned by tests/codegen.rs::test_e2e_option_map_string_value_bodies_compile_and_run (9 shapes, each compared against the `--interp` oracle), STASH-PROVEN red pre-fix. Residual, filed separately as B-2026-08-08-24 and pinned by an `#[ignore]`d test: the annotated concat `|s: String| s + "!"` now BUILDS but returns an empty String — a pre-existing silent miscompile this row did not introduce and does not fix. |
| B-2026-08-08-24 | typecheck+codegen | high | an OWNED closure-param annotation over a BORROWED payload (`out.first().map(\|x: String\| ...)`) was silently accepted and miscompiled -- empty String,… | cdd74e13. `src/typechecker/exprs.rs` no longer combines an explicit closure-param annotation with the combinator seed via `.or_else`: when BOTH exist they are compared first, and a `ref`/`mut ref` seed under a non-ref annotation is a TypeMismatch carrying a machine-applicable fix-it that rewrites the annotation to the borrow spelling (`check_closure_param_annotation_against_seed`). Verified end-to-end: `karac fix` turns `|x: String|` into `|x: ref String|` unattended and the result runs correctly on BOTH backends; `--output=json` carries code E0200 / class TYPE_MISMATCH / expected `ref String` / got `String` plus the fix-it span. Placed at the shared consumption point so it covers `map` and every `infer_closure_ret` sibling. PINS: typechecker `owned_closure_param_annotation_over_borrowed_payload_is_rejected` (STASH-PROVEN red pre-fix) + `borrowed_payload_accepts_the_correct_closure_param_spellings`, and e2e `test_e2e_option_map_over_borrowed_payload_correct_spellings`. Gates: full --features llvm suite GREEN post-rebase (13293 passed / 0 failed / 102 targets), clippy --all-targets, fmt, ledger lint 1042/0. NOTE the row as filed did not reproduce -- see the detail; its `#[ignore]`d pin is un-ignored and renamed to `test_e2e_option_map_owned_payload_annotated_concat_is_correct` as the control documenting that. |
| B-2026-08-08-25 | codegen | medium | matching a payload out of a live `Option[String]` / `Result[String, _]` binding leaves the BINDING DANGLING, so any later read is garbage or aborts t… | PARTIAL -- d530c033 fixes the BORROW-ONLY half: a read-only `match` / `if let` / `while let` over a live local owning an inline Option/Result `{ptr,len,cap}` payload is now classified as a BORROW (`scrutinee_is_readonly_inline_optres_local`), so the arm binding registers no free and the source keeps its own -- no clone, one free, source stays valid. Pins: e2e `test_e2e_match_out_of_option_string_leaves_source_usable` (STASH-PROVEN red pre-fix: second read printed `+\r`) and asan `asan_match_out_of_live_option_string_no_use_after_free`. Full --features llvm suite GREEN (13295/0, 102 targets). STILL OPEN, pinned `#[ignore]`d as `test_e2e_map_twice_over_live_option_string`: the CONSUMING half (`.map`, where the payload really is moved into the mapper) and the USER-ENUM leg (general `suppress_destructured_enum_payload_cleanup` channel). See detail for both candidate designs and the trap that `deep_copy_enum_heap_payload_in_place` no-ops on the erased Option layout. SECOND PARTIAL -- 502a598f closes LEG 1 for a BORROWING mapper: the escape guard counted the arm pattern `None` as a payload binding (the parser returns a unit variant in pattern position as `Binding("None")`), so any arm body MENTIONING `None` vetoed the read-only classification. Fixes `.map` twice, `.map` then a plain match, and the hand-written `match o { Some(v) => Some(v.len()), None => None }` -- which no combinator was ever needed to reproduce. Pins: e2e `test_e2e_map_twice_over_live_option_string` (un-ignored, stash-proven red) and asan `asan_map_over_live_option_string_leaves_source_owning_its_buffer` (clean at -O0 too). STILL OPEN, pinned `#[ignore]`d as `test_e2e_consuming_map_over_live_optres_still_open`: a CONSUMING mapper (`|s| s`), `Result.map`, and the user-enum channel. See detail. THIRD PARTIAL -- f616d635 closes LEG 1's CONSUMING half: `clone_escaping_live_local_option`, the bare-local sibling of `clone_escaping_borrowed_ref_chain_option` (B-2026-07-21-9), deep-clones the Option so the consuming arm owns an independent buffer while the live source keeps its own -- gated on `scrutinee_read_after_match` so a DEAD source keeps today's zero-cost transfer. This was the row's last memory-unsafety, so class drops use-after-free -> miscompile and severity high -> medium. Pins: e2e `test_e2e_consuming_match_over_a_live_option_local_keeps_the_source` and asan `asan_consuming_match_over_a_live_option_local_frees_each_buffer_once`, both stash-proven red. STILL OPEN: leg 2 (`Result.map`) and leg 3 (the user-enum channel), both valgrind-clean wrong output. See detail for the two arguments this row made AGAINST a clone, both of which were wrong. FIXED by df4f33fc, which closes LEGS 2 AND 3 -- both halves of each. Leg 2 read-only: a SCALAR `Err` co-arm disqualified `scrutinee_is_readonly_inline_optres_local` for the whole match (the `map` in the title is not involved; the read-only spelling fails identically, and `Err(_)` / a heap `Err` were always correct). Leg 2 consuming: `clone_escaping_live_local_result`, the twin of leg 1's Option clone -- the heap `.map` synthesizes a match whose scrutinee IS the receiver, so it was leg 1's shape one membership test away. Leg 3: `scrutinee_is_readonly_owned_enum_local` (the missing user-enum caller-retains classifier, read-only half) plus `clone_escaping_live_local_enum` (consuming half). Identifier scrutinees only -- owned `self` is excluded because its scope-exit drop is the documented residual and the arm is the sole owner there. See detail for the two framings this row carried that measurement corrected. Follow-up 35629603 closes the `if let` / `while let` read-only spelling of leg 3, which the match-site fix did not reach (`scrutinee_is_readonly_owned_enum_local_block`). The CONSUMING half at that site remains, filed as B-2026-08-09-11. |
| B-2026-08-08-26 | other | medium | `tests/cli.rs` is the dark target B-2026-07-31-44 missed, and it was RED the whole time -- 35 of its 43 `#[cfg(feature = "llvm")]` tests run in NO CI… | f68004a |
| B-2026-08-08-27 | other | low | the dark-llvm-target audit has never been RE-RUN as a check -- B-2026-07-31-44 swept 19 targets by hand and B-2026-08-08-26 found the 20th the same w… | 5b60bd77 |
| B-2026-08-08-28 | codegen | high | a weak ELEMENT read through a struct FIELD (`a.ns[0]` on `mut ns: Vec[weak N]`) skips the balancing acquire and over-releases -- SIGSEGV under JIT an… | 02f8a0c |
| B-2026-08-08-29 | typecheck+codegen | medium | `Map[K, weak V]` is ACCEPTED and lowers the value as a STRONG ref that nothing releases, so writing `weak` LEAKS where the strong `Map[K, V]` twin is… | dc31715 |
| B-2026-08-08-30 | codegen | high | mapping a BORROWED SCALAR payload — `Vec[i64].first().map(\|x\| x + 1)` — was TWO defects, and the reported panic was the lucky one: the closure's `ref… | e524f62 (both legs: the closure return-type inference over a borrow param, and the leaked param borrow mark) |
| B-2026-08-09-1 | codegen | medium | a `Map[K, V]` field of a SHARED struct has no general per-value drop-fn channel, so a V that needs a RECURSIVE drop leaks -- the non-shared struct fi… | ec446e0 |
| B-2026-08-09-2 | typecheck+codegen | medium | `Map[K, weak V]` is now store-only: the read is NOT an upgrade, so `m.get(k)` yields `Option[weak V]` and the `Some` binding rejects every field acce… | 68bebfd3 |
| B-2026-08-09-3 | codegen | medium | a `shared struct` binding's Drop body fires at LEXICAL SCOPE EXIT under codegen but at LIVE-RANGE END under `--interp` -- design.md mandates live-ran… | e9e3807 |
| B-2026-08-09-4 | codegen | medium | `let r = <Option/Result>.map(f)` over a HEAP payload leaks the result's payload once per evaluation — `map_passthrough_armed_source` claimed EVERY `.… | 5698333 |
| B-2026-08-09-5 | codegen | high | an indirect closure call lowered the return type from the SURFACE `Fn(..)` type while the emitted body used its own, so a borrowed-String mapper's 3-… | 37d0992a |
| B-2026-08-09-6 | typecheck+codegen | high | `Result[T, E].map(f)` never learns `E`, so a HEAP `Err` payload is mishandled on the pass-through branch: `Result[i64, String]` DOUBLE-FREES and abor… | cfd574e3 |
| B-2026-08-09-7 | codegen | medium | chaining any two Result-returning combinators without an intervening `let` fails in codegen -- `r.map(f).map(g)` panics (`ExtractOutOfRange`) under a… | bf5737a0 |
| B-2026-08-09-8 | codegen | high | a bare REBIND of an inline-Option-payload local (`let p = o`) followed by TWO reads of `p` double-frees the payload -- the caller-retains classifier… | 0137be0 |
| B-2026-08-09-9 | codegen | medium | a user enum with a `Vec[String]` payload consumed by a match arm over a LIVE local still empties the source -- the enum deep-copy is OUTER-buffer onl… | FIXED by 7d9ecce1. `deep_copy_enum_heap_payload_with_elements_in_place` -- the element-deep sibling of the enum payload duplicator -- plus `enum_payload_clone_is_faithful`, the widened successor of the gate that shipped with legs 2/3. The outer-only copy's stated premise ("mirrors the enum drop's outer-only free") was half true and the missing half WAS the bug: the drop's `VecOrString` arm frees only the outer buffer, but it is not the whole owner -- ELEMENTS ride the separate per-binding container-elem-bodies channel, so the copy was one level shallower than the thing it had to be independent of. Depth is opt-in per call site, not the default: unconditional depth leaked 1990 bytes / 300 allocations in `asan_match_bound_struct_variant_vec_field_reborrow_no_double_free`, because most callers' copies are dropped by the outer-only `EnumDrop` and the deep elements would have no owner. |
| B-2026-08-09-10 | interp | medium | `--interp` SKIPS the Drop body of a struct payload bound out of an owned enum PARAM, where both compiled backends run it -- the param-shaped sibling… | 128b746 |
| B-2026-08-09-11 | codegen | medium | the `if let` spelling of a CONSUMING arm over a live user-enum local still empties the source -- the match site got a live-local clone leg, the if-le… | 43cc3cb |
| B-2026-08-09-12 | codegen | high | the `<refparam>.field` ref-chain ENUM clone leg shares the outer-only payload duplicator, so a `Vec[String]` payload behind a `ref` param aliases its… | FIXED by 9a15e07c. `clone_escaping_borrowed_ref_chain_enum` now uses `deep_copy_enum_heap_payload_with_elements_in_place` (added by B-2026-08-09-9), so the clone's `Vec[String]` element buffers are its own and a consuming arm no longer frees strings the caller still holds. Gating the leg off the shape was never available -- that reintroduces B-2026-07-21-5/-6 -- so the element-deep copy was the only route. Pins: e2e `test_e2e_ref_chain_enum_vec_payload_clone_is_element_deep` and asan `asan_ref_chain_enum_vec_payload_clone_is_element_deep`, both stash-proven red, the ASAN one with an explicit heap-use-after-free. |
| B-2026-08-09-13 | codegen | medium | a `Vec[heap-element]` enum payload leaks EVERY element -- `__karac_drop_E` frees the outer buffer only, the documented `EnumDropKind::VecOrString` v1… | FIXED by d52a1312 -- `emit_enum_drop_switch`'s `VecOrString` arm drains a `Vec[heap-element]` payload's elements (inside the `cap > 0` guard, so a consuming arm that already owns them is untouched), and `deep_copy_enum_heap_payload_in_place` becomes element-deep for every caller so copy-depth still equals drop-depth. Both drains route through one new `vec_element_drain_fn`, shared with the struct-field arm that had drifted ahead of it. Pinned by `asan_readonly_match_over_enum_vec_payload_frees_each_element_once`. |
| B-2026-08-09-14 | codegen | high | a CONSUMING `while let` arm over a plain enum local whose source is DEAD after the loop double-frees -- the `match` and `if let` spellings of the sam… | a2e1f42 |
| B-2026-08-09-15 | codegen | medium | codegen runs a `Drop` body TWICE when a match arm RETURNS a Drop-carrying payload out of an owned enum param -- once in the callee, once at the calle… | bd5c2c2 |
| B-2026-08-09-16 | codegen | high | a `let` that aliases a match-arm payload bound off an owned enum PARAM (`let k = r; return k;`) double-frees the payload's String -- the let-move sup… | FIXED by 1b9901f. `let k: Res = r;` where the source's type has a user `impl
Drop` now retracts the source's MEMORY action as well as its body:
`suppress_struct_cleanup_for_tail_identifier` beside the existing
`suppress_user_drop_for_var`, in the let-stmt move-suppression's `has_user_drop`
arm (stmts.rs). Pins: e2e `test_e2e_returned_enum_param_payload_drop_fires_once_-
still_open` (alias case, and its `Drop` body now READS the payload string) plus
the method spelling restored to `test_e2e_returned_enum_arg_payload_method_-
spelling_fires_once`, and asan `asan_let_aliased_enum_param_payload_frees_once`,
stash-proven red with an explicit heap-use-after-free. |
| B-2026-08-09-17 | codegen | high | a `File` MOVED out of its binding (into a `Vec`, a struct, or a return) is still closed by the origin binding at scope exit, so the new owner holds f… | fdc874b |
| B-2026-08-09-18 | interp | low | Interpreter ICEs (`internal error: entered unreachable code`) on a METHOD CALL whose RECEIVER faulted, instead of reporting the receiver's runtime er… | FIXED by bb46a68d -- `eval_method_call` short-circuits on `pending_cf` immediately after evaluating the receiver, so a faulted receiver propagates its own runtime error instead of being dispatched on as a `Unit` poison. Placed at the single receiver-eval site rather than in the arm that reported it: the receiver assertions are per-method (`len`, `chars`, ... each have their own; `is_empty` has none), so one check covers every method on every builtin receiver. Pinned by `test_faulted_method_receiver_reports_the_fault_not_an_ice` plus a healthy-receiver over-fire control. |
| B-2026-08-09-19 | interp | low | SIBLING OF B-2026-08-09-18, NOT CLOSED BY bb46a68d: a faulted operand still ICEs the interpreter in THREE more positions, all with the same shape (`u… | 512f59a |
| B-2026-08-09-20 | codegen | medium | a `File` moved into a `Vec` or a struct is never closed -- the container has no element/field drop for the handle, so the fd leaks until process exit… | FIXED by cc48dcca -- two arms, one per container kind: a `File` element arm in `vec_elem_agg_drop_for_type_expr` (new `emit_file_slot_close_fn`, threaded as `elem_agg_drop` so every `track_vec_of_aggs_var` site picks it up) and a `FieldDrop::FileHandleClose` in the struct drop glue; `te_recursive_drop_fully_supported` admits `File` so `Vec[Vec[File]]` leaves the one-level fast path. Both null the slot after closing, because `karac_runtime_file_close` is not idempotent, and both defer to a user type that shadows the name `File`. Also fixes two shapes the row did not record -- a handle read back out of a container into a fresh binding (`let g = hs[0]` / `let g = h.f`) leaked identically. Pinned by four ASAN fixtures including an over-fire control. The row's unverified `DropChannelEnd` sibling risk was probed and does NOT reproduce. |
| B-2026-08-09-21 | codegen | medium | A NESTED index whose base is a STRUCT FIELD (`h.data[i][j]`) is rejected by codegen -- `codegen: nested indexed read requires the outer container to… | FIXED by 4f3d6921, BOTH halves. READ: `compile_nested_index_read` gained a struct-FIELD arm -- `nested_index_field_base_elem` resolves the element `TypeExpr` from the field's DECLARED type and the container pointer via `lower_field_access_ptr`, then rejoins the existing synth-identifier tail (factored out as `finish_nested_index_read`). The name-keyed lowering was split into a by-POINTER core, `lower_indexed_elem_ptr_vec_at`, so the field base reuses the identical bounds check and GEP. WRITE: `compile_index_store` gained a matching arm that normalises the field to a synth identifier and recurses, so the existing named-outer nested store handles it unchanged. Pin: e2e `test_e2e_nested_index_rooted_at_struct_field` (7 cases), stash-proven red. |
| B-2026-08-10-1 | codegen | medium | a NESTED indexed store that overwrites a heap element (`d[i][j] = <String>`) leaks the old value -- the single-index store frees it, the nested one d… | 1b6ed41 |
| B-2026-08-10-2 | typecheck | low | the `already a mut-ref; drop the `mut` marker` diagnostic tells the author to delete one token but carries no machine-applicable replacement, so `kar… | 93444f5 + 9c0c8655 -- widened past the reported shape: labeled arguments get the edit instead of being excluded from it (`CallArg::mut_marker_span` replaces the span-subtraction, so the exclusion 93444f5 pinned is removed rather than kept), the sibling "`mut` marker is not legal here" arm gains the same deletion, and the sweep this row asked for found and fixed the two remaining members of the family (`#[profile(name: x)]` prefix, `#[non_exhaustive]` on a union). Family fully enumerated at six. |
| B-2026-08-10-3 | typecheck+interp+codegen | medium | `File` has no `seek` on the Kāra surface even though the runtime entry point `karac_runtime_file_seek` is already implemented and exported — so any r… | FIXED by 9f1e3c6f — the surface half, across all three backends. `runtime/stdlib/io.kara` gains `File.seek(ref self, whence: SeekFrom, offset: i64) -> Result[i64, IoError] with reads(FileSystem)` plus the payload-free `enum SeekFrom { Start, Current, End }`; `SeekFrom` joins the prelude type list; the interpreter gets a `seek` arm over the `Arc<Mutex<File>>`; codegen declares the extern, truncates the SeekFrom tag to the ABI's u8, and unpacks the io-result as `FileOkKind::ByteCount`. The runtime needed NO change — `karac_runtime_file_seek` was already written, exported, and on the `__preserve_no_mangle_symbols` keep-list, which is exactly the outcome its early-export comment predicted. Pins: interp `test_file_seek_positions_and_reads` + `test_seek_from_variants_discriminate`, e2e `test_e2e_file_seek_positions_and_reads` + `test_e2e_seek_from_prelude_enum_discriminates`; all stash-proven red. |
| B-2026-08-10-4 | typecheck+interp+codegen | medium | `split_at_mut` is fully specified in design.md but implemented nowhere, so there is NO way to obtain a mutable sub-view of a buffer — `buf[n..]` yiel… | 04bcd16 |
| B-2026-08-10-5 | typecheck+codegen | medium | index-assign through a TUPLE FIELD whose type is a Slice is rejected by codegen (`p.0[0] = x`) — the same spelling works when the field is a Vec, and… | a51ef80 |
| B-2026-08-10-9 | codegen | high | `Vec.sort_by`'s mono path was a NON-ADAPTIVE fixed-32-run bottom-up merge sort: it did ceil(log2(n/32)) = 13 full passes over 2.4 MB for 150k element… | 50a50e8 |
| B-2026-08-10-13 | codegen | medium | Inside a `sort_by` COMPARATOR CLOSURE, a closure parameter supports only TUPLE-FIELD access; any METHOD CALL or INDEX on it falls through codegen's m… | b90027e |
| B-2026-08-10-16 | codegen | medium | An explicit `return` inside a `sort_by` COMPARATOR CLOSURE emits an LLVM module-verification failure: `Module verification failed: "Function return t… | 568e6ff |
| B-2026-08-10-17 | typecheck | medium | a `return` NESTED inside a closure body (in an `if` / loop) is typechecked against `()` instead of the closure's return type, so an early return from… | 819af61 |
| B-2026-08-10-18 | codegen | medium | an explicit `return` inside an ITERATOR ADAPTOR closure (`map`/`filter`/`any`/`all`/`retain`) fails codegen with `Terminator found in the middle of a… | c0c8842 |
| B-2026-08-10-19 | codegen | medium | `Vec[(i64,i64)].sort_by` on SHUFFLED-UNIFORM input is ~1.66x Rust's `sort_by` (karac 14.82 ms vs driftsort 8.93 ms, 150k pairs, this host, both progr… | 31485cd |
| B-2026-08-10-21 | codegen | medium | the `UseAfterMove` defensive copy that `cli.rs` promises DOES NOT EXIST for any heap type on the binding-to-binding move path -- `karac check` exits… | FIXED by bb663f1d for the `{ptr,len,cap}` family (`String` / `Vec` / `VecDeque`); `Map`, `Set` and user structs remain, pinned as an `#[ignore]`d test and described below. THE GATE CAME FIRST, and had to: `tests/common/mod.rs`'s `assert_ownership_clean` refused every program with ownership errors, so no fixture could exercise this mechanism at all. It now mirrors `cli.rs`'s `is_fatal_ownership_kind`, admitting the two ADVISORY kinds production is documented to accept. The fix itself is two halves that must move together: `OwnershipCheckResult::use_after_move_consume_sites` ships the flagged consume spans (derived from the diagnostics, so warning and copy can never disagree); codegen's identifier load deep-copies at those spans (`uam_defensive_copy`), and the single disarm funnel `suppress_source_vec_cleanup_for_arg_ex` skips the source disarm — but keys on `uam_copied_sites` (a copy ACTUALLY happened), not on the flagged span. Pins: e2e `test_e2e_use_after_move_defensive_copy_vecstr_family` and asan `asan_use_after_move_defensive_copy_frees_each_buffer_once`, both stash-proven red (the ASAN one with an explicit heap-use-after-free). Follow-up 1bfac4da closes the STRUCT-LITERAL consume site (`H { name: s }`) in both `compile_struct_init` branches; the function-arg and container-push consume shapes measured already clean. The remaining gap is the TYPE axis (Map/Set/struct-value moves). Follow-up aafb6b69 closes the TYPE axis -- `Map`/`Set` via `emit_map_clone_fn` and user structs via field recursion -- so the row is fully fixed for every family except `shared struct`, which needs no copy (RC-managed). The `#[ignore]`d deferral test is now live as `test_e2e_use_after_move_defensive_copy_map_set_struct`. |
| B-2026-08-11-2 | typecheck | medium | `char` and `bool` receivers skip method-existence checking entirely, so ANY method name passes `karac check` and unifies with ANY return type -- the… | FIXED by c2be671. `char` and `bool` receivers now route through the
SCALAR-PRIMITIVE arm in `src/typechecker/expr_method_call.rs` -- the same arm
`i64`/`u32`/`f64`/`u8` have used since B-2026-07-03-5. One `matches!` gains
`Type::Bool | Type::Char`, which buys both halves at once: a method with no impl
candidate is rejected `no method 'X' on type 'char'` (`NoMethodFound`) instead of
poisoning to `Type::Error`, and a method that HAS one dispatches through the impl
table and gets a real return type. `char` numeric-conversion typos (`to_i64` /
`as_i64` / `to_int` / `to_u32` / `as_u32` / `code_point` / `ord`) and `is_digit`
carry a hint naming the spelling that exists. Pins: five tests in
`tests/typechecker.rs` -- the bogus-name matrix, the four-annotation unification
case, the full real char/bool surface, user impls on both (dispatch AND return
type), and the hint. |
| B-2026-08-11-3 | codegen | medium | a generic struct's method whose parameter is the TYPE PARAMETER leaks a fresh TEMPORARY argument's buffer -- `s.push([1i64,2i64])` on a `Stack[T]` le… | FIXED by 3dd2e4b, in `src/codegen/mono.rs`.

`compile_generic_call` now materializes a fresh heap Vec/String temp into the
caller's scope when the callee's param at that index is written as a bare type
param AND the callee's declared return type does not mention that type param
(the new `generic_param_is_bare_type_param` + `type_expr_mentions`). That is
the third of the three legs B-2026-07-11-35's comment names — the deep-copy
registration and per-monomorph struct-drop synthesis shipped; caller-side
fresh-owned-temp cleanup never did.

The return-type guard is load-bearing, not defensive. Without it the fix aborts
`e2e_return_owned_generic_param_no_double_free` and
`test_e2e_generic_forward_owned_string_param_no_double_free` with a real
`free(): double free detected in tcache 2`: `fn echo[T](x: T) -> T { x }` is
also a bare type param, and the caller's binding for the RESULT already owns
the buffer. The test is syntactic rather than a body walk because
`fn_returns_param` answers false for a forwarding tail — `pick[T](a: T, b: T)
-> T { id(a) }` and `nest[T](x: T) -> T { id(id(x)) }` both slip past it and
both were measured aborting. An unrecognized return shape answers "mentions
it", so the conservative direction is a leak, never a double free.

Regression test: `asan_generic_struct_method_bare_type_param_temp_arg_no_leak`
(`tests/memory_sanitizer.rs`), 50 iterations so LSan sees accumulation, pushing
a temporary and a named binding side by side — the named one is the
double-free control. Verified to FAIL pre-fix. Full `--features llvm` suite
green (102 targets, 13399 passed, 0 failed). |
| B-2026-08-11-4 | typecheck | low | `v.cast()` on ANY primitive receiver passes `karac check` and then fails at run time -- `cast` sits on the PRIMITIVE_VALUE_METHODS exemption but is n… | FIXED by 0aca4db. `"cast"` dropped from `PRIMITIVE_VALUE_METHODS`
(`src/typechecker/expr_method_call.rs`); no replacement needed, because the
normal path already does the right thing in both directions. Pins: three tests
in `tests/typechecker.rs` -- `v.cast()` rejected on i64/f64/char/bool/u8, a user
`impl i64 { fn cast }` / `impl char { fn cast }` reachable AND its return type
enforced, and a regression guard that the seven comparison ops stay exempt on
every receiver that carries the baked impl. |
| B-2026-08-11-5 | parser | high | EVERY parse-phase diagnostic raised inside an f-string interpolation hole is DISCARDED | FIXED by 3baa5a4. Three parts at the single site the row identified
(`src/parser/exprs.rs`, the `InterpolatedStringLit` arm): rebase and append the
nested parse's `errors` onto the enclosing parser; rebase and merge its
`fix_edits`; and drop the `Text` fallback, which existed only to hide the
error the first part now surfaces. A hole that neither parses nor produces a
diagnostic emits `could not parse interpolation` rather than vanishing.

The rebase is factored out of `shift_expr_spans` as
`span_visitor::shift_interp_span`, so the expression, the diagnostics and the
edits share one implementation and cannot drift; it also clamps the offset at
0, which matters only on the new path (an ERROR can point into the synthetic
prologue, where the old arithmetic wrapped to a huge `usize`).

Pins: four tests in `tests/parser.rs` -- the five swallowed shapes reporting,
the span landing inside the hole rather than on the wrapper, Leg B enforced
with its edit surviving both in-hole and outside, and a guard that well-formed
holes (nested f-strings and format specs included) still report nothing.

FOLLOW-UP COVERAGE (same day, separate session that had reached the same fix
independently and rebased onto this one rather than duplicating it). Four pins
added on top, all stash-proven red against the pre-fix parser, each covering
something the original four did not:

  - `tests/parser.rs::interp_hole_that_parses_as_a_statement_is_still_an_error`
    -- the `nested_errors == 0` branch. `f"{let q = 1}"` parses cleanly inside
    the wrapper as a STATEMENT, so the `find_map` for a `StmtKind::Expr` comes
    back empty AND there is no error to forward; that branch is the one place
    where dropping the `Text` fallback is not by itself enough, and it was
    written but unpinned. Also covers the empty hole `f"{}"`.

  - Three in `tests/cli.rs` for the MEND-LOOP surface the row's own
    "MEND-LOOP CONSEQUENCE" paragraph is about, which parser-level pins cannot
    reach: `check` exits NON-ZERO with the diagnostic rebased onto the right
    line/column; `--output=json` no longer emits `"diagnostics":[]`; and
    `karac fix` end-to-end actually rewrites an in-hole `takes(ref xs)` to
    `takes(xs)` and the repaired file then checks clean. The last one is the
    only test that exercises the diagnostic-span-keyed `fix_edits` merge
    through the real `cmd_fix` path -- the half of the fix that fails silently
    (correct edit, orphaned key, zero fixes applied) rather than loudly. |
| B-2026-08-11-6 | typecheck | high | A bare TYPE NAME in call position -- `i64(42)`, `F64(1.5)`, `bool(1)`, `String("hi")`, `Vec(1)`, or a named-field user struct `P(1)` -- is accepted b… | FIXED by fc9c3fb. The rejection lives on the bare-identifier arm of
`infer_expr` (`src/typechecker/exprs.rs`), with a per-family remedy built by
`type_name_in_value_position_message` (`src/typechecker/expr_ops.rs`). It fires
only when every real resolution has failed, so the legal bare-name forms --
enum variants, distinct types, the comptime `Type` pseudovalue -- are
untouched. Pins: four tests in `tests/typechecker.rs` (the eight-shape
rejection matrix plus the value-position leg, the per-family remedies, every
legal form as a guard, and the two collateral paths). |
| B-2026-08-11-7 | typecheck | high | `Vec[f64].sort()` bypasses the `Ord` gate that design.md § Float semantics REQUIRES -- the spec's own verbatim counter-example compiles | e51d3e7 (gate on Vec/Slice `sort`, Vec `sorted`, Vec/Slice `binary_search`, via a new `require_ord_element` in typechecker/derives.rs. `max`/`min` split to B-2026-08-11-15; `contains` deliberately left allowed — see detail.) |
| B-2026-08-11-15 | typecheck+codegen | medium | `Vec[f64].max()` / `.min()` return an ORDER-DEPENDENT element instead of erroring, and there is currently no working remedy to point users at -- whic… | FIXED by 3d0064f, in three parts, in the order the row prescribed:
codegen first, then the typecheck widening, then the gate.

1. `src/codegen/method_call.rs` — the total-order wrappers are admitted to the
   iterator `max`/`min` arm's element test and to `try_compile_iter_chain_reduce`.
   The reduce gate keys on `is_trivially_copyable_te`, a PRIMITIVE-name list
   shared by 36 call sites, so it was NOT widened: a new
   `is_total_float_wrapper_te` (`src/codegen/assoc_call.rs`) is OR'd in at the
   one site that needs it. A `{ float }` wrapper is trivially copyable in the
   sense that gate cares about (no heap payload to double-free in the synthetic
   `Some(acc) => Some(f(acc, x))` match), but that is not the question the other
   35 callers ask, and answering it for them would have been a silent change to
   every one of those sites.

2. `src/typechecker/stdlib_iter.rs` — the numeric-or-String element test admits
   `F32`/`F64`/`F16`/`Bf16`.

3. The gate itself, plus a per-method harm clause in `require_ord_element`
   (`src/typechecker/derives.rs`): quoting `sort()`'s "leaves the sequence
   completely unsorted" under `max`/`min` would describe a symptom those methods
   do not have.

WHY IT IS LANDABLE NOW AND WAS NOT IN -7. -7 withdrew this gate because the
remedy dead-ended; the missing precondition was not only codegen but
B-2026-08-11-13. The wrapper's total order is IEEE totalOrder, which is
SIGN-SENSITIVE, and nothing at source level chooses a NaN's sign — measured
before -13 landed, `Vec[F64].sorted()` on `[NaN, 3.0, 1.0]` gave `NaN,1,3` under
the interpreter and `1,3,NaN` under both compiled backends, because interp
produced 0xFFF8... and compiled produced 0x7FF8... (confirmed via `to_bits()`).
Gating on top of that would have swapped a silent wrong answer for a NEW
run-vs-build gap. -13's construction-side canonicalization removed it, and
`max`/`min` inherit the fix.

MEASURED, interp / JIT / AOT, all three identical: `[3.0, 1.0, 2.0]` as
`Vec[F64]` gives max 3 / min 1; `[NaN, 3.0, 1.0]` AND `[3.0, NaN, 1.0]` both
give max NaN / min 1 — position-independent, which is the property the wrapper
buys and the raw `f64` path could not provide. On raw `f64` the same three
inputs gave 3 / NaN / 3, i.e. the answer moved with the NaN.

ONE CLAIM IN THIS ROW WAS WRONG, and one thing I first recorded as a
correction to it was itself wrong -- both settled by measurement:

  * The row's predicted codegen failure ("no handler for method 'max' on
    non-identifier receiver") never occurs. Admitting the wrapper at typecheck
    produces a LOUD, correct deferral from `Iterator.reduce()` instead, because
    the reduce lowering gates on trivially-copyable elements. That is the gate
    this fix opens, and it is a different site from the one the row names.

  * The row's `Vec[f64].max()` spelling is CORRECT and I briefly recorded that
    it was not. Mid-investigation I concluded "`Vec.max()` does not exist on
    either backend" from a single probe that failed on both. It does exist:
    `let fs: Vec[f64] = [1.5, 2.5, 0.5]; println(fs.max().unwrap())` answers
    2.5 on interpreter, JIT and AOT alike (verified with the gate temporarily
    disabled), and a shipped ASAN fixture has been compiling that exact form
    all along. What my probe actually hit is a SEPARATE gap, filed as its own
    row: the direct-Vec terminal desugar is POSITION-SENSITIVE, so
    `println("max=" + fs.max().unwrap().to_string())` fails on both backends
    with an error that blames `max` itself. Generalizing from one expression
    shape's misleading message is what produced the wrong correction; the fix
    was to vary the shape rather than trust the diagnostic's noun.

The `Stats.max(v)` divergence the row folded in is NOT fixed here and is not the
remedy the diagnostic names. Re-measured on all three backends it is also
sharper than recorded: interp 3, JIT NaN, AOT 3 — the JIT is the outlier, not
"interpreter vs compiled". The row speculated the fix might be shared with
max/min ("one total-order float reduce that both surfaces call"); it is not —
this fix routes through the iterator reduce path and does not touch `Stats` —
so per the row's own instruction it is split out rather than closed here.

Tests: `test_float_iter_max_min_are_gated_and_wrapper_is_accepted`
(tests/typechecker.rs) asserts gate AND remedy together, since a gate whose
remedy stops compiling is a regression a rejection-only test reports green on;
`test_e2e_iter_max_min_over_total_order_wrapper` (tests/codegen.rs) pins the
NaN-first / NaN-middle agreement.

KNOWN REMAINDER, deliberately not worked around: chained
`.max().unwrap().value` still fails in codegen ("cannot resolve field 'value'
on this receiver"). It is PRE-EXISTING and unrelated — it reproduces with a
plain `struct P { x: i64 }` and an `Option[P]`, no iterator or wrapper involved
— so it is filed separately rather than papered over. The remedy is fully
usable via a `let` binding or a `match`, both verified on all three backends,
and the E2E test spells it the `let` way for exactly this reason. |
| B-2026-08-11-8 | typecheck | medium | `F64.from(x)` / `F32.from(x)` -- the total-order wrapper constructor that design.md AND the compiler's own `T: Ord` diagnostic both name as THE fix f… | d8ddc56 (three parts: an ordinary Kāra `impl <W> { fn from }` in each of runtime/stdlib/{f32,f64,f16,bf16}.kara makes the call typecheck; a codegen intercept in assoc_call.rs BUILDS the wrapper struct, because codegen does not route path-calls to baked stdlib impls and the call was otherwise reaching the unknown-callee tail as a const i64 0; and type_name_of_expr in expr_ops.rs types the call-result temp so a direct `F64.from(x).value` chain resolves its field. Display is a fourth part: one seam in build_struct_display_parts covering both println and f-strings.) |
| B-2026-08-11-13 | codegen+interp | medium | `F64`/`F32` ordering and equality depend on the SIGN BIT of a NaN, which is not stable across backends or optimization levels -- so `Vec[F64].sort()`… | FIXED by 284d44a4, by canonicalizing NaN at CONSTRUCTION in all three backends --
the row's own preferred shape ("doing it at construction is the stronger
invariant"), chosen over normalizing in the comparison key.

1. Codegen: `canonicalize_wrapper_nan` (src/codegen/assoc_call.rs) emits
   `select(fcmp uno x, x, <canonical quiet NaN>, x)` and runs inside
   `compile_total_order_wrapper_from`.
2. Codegen: `F64 { value: x }` previously fell through the GENERIC struct-init
   path and stored raw bits. It is now intercepted at the top of
   `compile_struct_init` and routed through that same
   `compile_total_order_wrapper_from`, so the literal and `.from` are ONE
   lowering rather than two that can drift apart.
3. Interpreter: `canonical_wrapper_f64` / `_f32` (src/interpreter/helpers.rs)
   applied at both producers -- the four `X.from` arms in eval_call.rs and the
   struct-literal arm in interpreter.rs.

The canonical value is spelled as an explicit bit pattern on both sides
(0x7FF8000000000000 / 0x7FC00000, plus the 16-bit siblings) rather than
`f64::NAN`, so the interpreter and the compiled backends are provably the same
bits instead of the same by convention.

WHY CONSTRUCTION AND NOT THE COMPARISON KEY. The key-side fix is narrower per
site but has MORE sites -- cmp, eq, the two sort-key paths in vec_method.rs, and
the Map/Set hash -- and a site that forgot would fail SILENTLY: `eq` calls two
keys equal while `hash` sends them to different buckets. Construction gives one
invariant ("no wrapper ever holds a non-canonical NaN") that every consumer
inherits, at two sites per backend. The producer set is closed and small, which
is what makes that tractable: there is no `F64.NAN` constant (it does not
exist -- see B-2026-08-11-14) and the wrapper has no arithmetic, so `X.from` and
the struct literal are the ONLY ways to make one.

`-0.0` is deliberately NOT normalized. Unlike a NaN's sign it is a distinct,
well-defined point of the total order that all three backends already agreed on,
with a pin (`test_e2e_total_order_float_wrappers`) asserting it.

WHAT THE ROW UNDER-STATED. It reported a 3-way split; measured here it is 4-way,
and the extra leg is the sharpest evidence: the default `-O2` AOT build and
`KARAC_OPT_LEVEL=0` disagree on the SAME source, because -O2 inlines the call and
constant-folds the division while -O0 leaves a real one. Two further findings the
row did not have, both worse than a split:
  - `Vec[F64].sort()` under the JIT returned NaN at BOTH ENDS (`NaN -1 1 NaN`).
    That IS correctly sorted under raw totalOrder -- -NaN precedes -Infinity,
    +NaN follows +Infinity -- and completely unreadable, since both print `NaN`.
  - A two-NaN `Map[F64, i64]` had a different LEN per backend (interp 1, JIT 2,
    AOT -O2 1, AOT -O0 2), so a collection silently gained or lost an entry
    depending on how the program was run.

MEASUREMENT TRAP WORTH RECORDING. The first codegen pin PASSED pre-fix. At -O2
LLVM folds both the constant NaN and the nominally-runtime one to the same
positive NaN, so AOT agreed with the correct answer by luck, and the bug is only
visible where a NaN genuinely survives to run time. The pin now sources its zero
from `env.args().len()`, which the optimizer cannot see through; that made it red
at every opt level. A codegen E2E test that exercises constant-foldable
arithmetic is testing the folder, not the backend.

Pins: twin tests asserting the SAME string in tests/interpreter.rs
(`test_total_order_wrapper_nan_canonicalized_interp_parity`) and tests/codegen.rs
(`test_e2e_total_order_wrapper_nan_canonicalized`) -- the pairing IS the
contract, run == build. Both stash-proven red. |
| B-2026-08-11-14 | other | low | design.md § Float semantics specifies the `F32`/`F64` total order as `-Infinity < .. | FIXED by 284d44a4, in the same commit as B-2026-08-11-13 and for the reason this row
gave: once NaN is canonicalized, the bullet can be stated CORRECTLY as a whole
instead of patched in place. Doing this row alone was not actually possible
honestly -- writing today's raw-totalOrder NaN behaviour into the spec would have
documented a run-vs-build divergence as intended semantics.

The `F32`/`F64` bullet list in docs/design.md § Float semantics now reads:
  - Ordering `-Infinity < ... < -0.0 < 0.0 < ... < +Infinity < NaN`, with `-0.0`
    and `0.0` called out as DISTINCT adjacent values and `-0.0 == 0.0` stated
    explicitly as `false`, contrasted against the `f64` primitive where it is
    `true`. That is the correction this row asked for.
  - Equality is bit-equality, with the reason attached: it is what makes `Hash`
    sound, since equal keys have identical bits and cannot hash apart.
  - NaN is ONE value, canonicalized at construction -- now true of the code as
    well as the spec.
Plus a paragraph on why canonicalizing is not cosmetic, since a reader who
assumes raw totalOrder would reasonably expect the sign to matter.

THE CODE BLOCK WAS ALSO WRONG, in three ways this row did not list, each found by
RUNNING it rather than reading it:
  - It used `#` for comments. Kāra's comment is `//`; `#` begins an attribute, so
    every commented line was a parse error. This was the ONLY `#`-commented kara
    block in design.md, against 600 `//` comments elsewhere.
  - `F64.NAN` does not exist -- no associated constants are implemented on the
    wrapper (`no associated function 'NAN' on type 'F64'`). Rewritten as
    `F64.from(f64.NAN)`.
  - `scores.map(F64.from)` is rejected: iterator adaptors require an explicit
    `.iter()`. Rewritten as `scores.iter().map(F64.from).collect()` -- which is
    also verbatim what B-2026-08-11-7's new `Ord`-gate diagnostic tells users to
    write, so the spec and the compiler's own advice now agree.

Every line of the replacement block was executed on interp, JIT and AOT before
being written down, including the two claims stated as compile errors
(`Map[f64, String]` and `Vec[f64].sort()`). |
| B-2026-08-11-9 | typecheck | low | the seven comparison-op names are exempted from method-existence checking by NAME rather than by whether the receiver carries the baked impl, so `f.c… | FIXED by 22ba601. The exemption is now keyed on whether the receiver
carries the baked impl instead of on the method name alone: one
`let exempt_comparison_ops = !matches!(&receiver_for_lookup, Type::Float(_));`
guarding both `PRIMITIVE_VALUE_METHODS.contains(&method)` sites in
`src/typechecker/expr_method_call.rs`. Floats have no baked candidate, so no new
code is needed -- `find_methods_with_args` comes back empty and the existing
error branch produces `no method 'cmp' on type 'f64'`, naming the float width.
Pins: four tests in `tests/typechecker.rs` -- all seven names on f32 and f64
rejected, the operators and the real float methods left working, a user
`impl f64 { fn cmp }` reachable with its return type enforced, and a
cross-check that the direct-method surface now agrees with the `T: Ord` bound
surface. |
| B-2026-08-11-10 | codegen | low | `Vec[(i64,i64)].sort_by` on FEW-UNIQUE input (150k pairs over 8 distinct keys) is 5.93 ms / 48.8M instructions vs driftsort's 1.81 ms / 14.2M on this… | FIXED — the full-array stable partition of § Direction 7 was budget-checked,
built, and measured. On the pattern this row filed as 3.3x behind driftsort,
karac now runs FEWER INSTRUCTIONS THAN DRIFTSORT and lands at wall-clock
parity. Fixed in 3d77cc60; emitter in `emit_sort_by_mono` (src/codegen/vec_method.rs); write-up in
docs/spikes/sort-algorithm-gap.md § Direction 7.

Before/after on the same container, baseline reproducing this row's own
recorded checksum (48.8M / 22.5M) exactly:

    pattern         instructions           driftsort   was            now
    few-unique      48.8M -> 13.2M  3.70x     14.2M    3.44x behind   1.08x AHEAD
    sawtooth        22.5M -> 23.4M  0.96x     31.6M    1.40x ahead    1.35x ahead
    random          72.3M -> 72.1M  1.00x     48.2M    1.50x behind   unchanged
    nearly-sorted   31.5M -> 32.9M  0.96x
    sorted           1.0M ->  1.0M  1.00x
    reverse          1.8M ->  1.8M  1.00x

Wall clock on the target, same session and slope method: 4.760 -> 1.742 ms
(2.73x), against driftsort's 1.514 ms best / 1.764 ms bulk average. Wall clock
on the other five rows is noise -- it swings several percent in BOTH directions
against IDENTICAL instruction counts, and sorted/reverse sit at 0.03-0.4 ms,
under the slope method's floor. Argue from the instruction column.

THE 4% ON SAWTOOTH AND NEARLY-SORTED IS REAL AND IS NOT WHAT IT LOOKS LIKE, and
three candidate explanations were each measured and killed. Not the probe:
sorted and reverse call it on every sort and did not move off 1.0M / 1.8M, which
bounds it under 0.05M. Not the partition running: neither pattern passes the
gate. Not the call-graph change: the obvious suspect was that `qpart` calling
back into the merge gives the merge a second caller, but DELETING that call (a
deliberately incorrect build, valid to measure because neither pattern ever
reaches `qpart`) made both WORSE, 24.4M and 34.4M. What is left is codegen
jitter -- perturbing the module around a 60-block function moves these two
patterns a few percent either way, and the shipped arrangement is the better end
of the measured range. Also measured and rejected: hoisting the probe into its
own function changed nothing (13.2/23.4/72.1/32.9 identical), so it is kept out
of line on structure, not on evidence of a win.

THE DESIGN, and the two things that made it work where six earlier directions
did not. (1) It occupies the OTHER END OF THE RECURSION from the bounded
run-builder that was built and reverted for B-2026-08-10-20: partition from n
DOWN to a span and merge below, rather than merge above a span and partition
below. So it removes the TOP passes, and every partition runs on a large range
rather than paying a fixed per-range cost near its base size. (2) It STOPS
EARLY, which no merge can -- a range whose every element ties with the pivot is
sorted and stable and is finished. Instrumented on few-unique: 8 all-equal exits
for 8 distinct keys, and NOT ONE merge pass over the array.

GATE PLACEMENT WAS MOST OF THE WORK, because ungated this is 11-13x WORSE on
sorted and reverse. Both full-pass placements lose: a counting pass BEFORE phase
1 costs ~1.3M, which is 42% of sorted's entire budget; AFTER phase 1 it is free
for sorted/reverse but costs few-unique 19.4M -> 33.9M, because phase 1's RUN=32
insertion padding spends ~14.5M on an input whose natural runs are ~2 long and
the partition then discards it -- no better than the 34.25M run-builder this row
already rejected. So the entry gate is O(1): 512 samples estimating BOTH
cardinality (ties with a random pivot) and existing orderedness (ordered
adjacent pairs). Both are needed -- cardinality alone would partition an input
that is ALREADY SORTED over few keys. The in-recursion gate is exact and free:
`neq = nle - nlt` is already computed by the partition's own counting pass, so
each range re-decides for nothing and a mixed input partitions the part that
pays and merges the part that does not.

FOUR KERNEL DETAILS, NONE OPTIONAL. (a) The pivot must be RANDOMISED; a
fixed-position median-of-3 is degenerate on periodic input -- on the sawtooth
lo/lo+len/2/hi-1 hold 0, 0, 999, the median is 0, and 0 is the range MINIMUM, so
each level peels only the copies of the minimum: measured 2328.7M instructions,
~100x the merge. (b) ONE counting pass tallies both `< pivot` and `<= pivot`,
which picks the split predicate without a second pass and folds the old t=1
retry into it. (c) Count-then-scatter, two cursors over one in-order walk --
the whole stability argument. (d) `allow_part` on the merge sort's signature:
without it an abandoned range is re-probed, accepted by the sampling estimate
that the exact tie count just rejected, and handed straight back -- same range,
same pivot, forever. A depth limit of 64 keeps the worst case O(n log n) after
introducing a randomised pivot.

GATE THRESHOLD VALIDATED AGAINST A MEASURED CROSSOVER, not tuned to the
benchmark. With the partition forced on, so the crossover is a property of the
algorithms: distinct keys 2/8/32/64/128/256/512 -> partition 7.6/22.4/30.7/36.8/
60.6/64.8/67.0M against merge 38.7/49.7/53.0/52.0/51.0/50.5/50.5M, i.e. the
crossover is between 64 and 128. The arithmetic predicts d < 68 (a partition
level costs two passes against the merge's one, so it wins while 2*log2(d) <
log2(n/RUN) = 12.2). GATE = 64 sits just on the winning side of both.

VERIFICATION. A 756-case sweep -- 6 patterns x 14 sizes x 9 cardinalities,
sizes straddling the probe floor (4095/4096/4097) and cardinalities straddling
the gate (63/64/65/129) -- asserting sorted AND stable, with the ORACLE ITSELF
VALIDATED by poisoning it (two deliberately unsorted cases produce 27,500 FAIL
lines, so a silent pass means something). Element types i64, all-int struct and
heap String. Full `cargo test --features llvm` green; ASAN clean at the default
level and on the `-O0` leg (1032 passed, quarantine list matched exactly).
`should_use_mono_vec_sort_by_for` admits only i64 and all-int structs, so no
heap-owning element ever reaches this path -- the memory-safety class here is
bounds, not ownership. Pins: `test_e2e_vec_sort_by_partition_path_low_cardinality`
(tests/codegen.rs, strict assert_eq so a stale archive fails loudly) and
`asan_vec_sort_by_partition_path_in_bounds` (tests/memory_sanitizer.rs), both
checking sortedness, stability, and the all-equal early exit -- that branch
returns a range declaring it sorted WITHOUT writing it, so only stability can
catch it having moved anything.

WHAT THIS DOES NOT CLOSE: B-2026-08-10-20, the shuffled-uniform 1.50x, which
remains `wontfix`. Random is untouched here (72.3M -> 72.1M) by design -- the
gate rejects it after one 512-sample probe. |
| B-2026-08-11-1 | codegen+typecheck | high | a `Vec[char]` INDEX used directly as a method receiver (`cs[0].to_string()`) loses its `char` type and dispatches to the INTEGER method, so codegen s… | 7372313 (three root causes: the `char` name in register_var_from_type_expr, the Array element-TypeExpr fallback at the indexed-receiver site, and VecDeque in the typechecker's scalar index arm) |
| B-2026-08-11-11 | typecheck | medium | TWO defects at the tuple receiver, filed together and NOT one bug: (a) tuple-index projection through a `ref` is rejected while the identical struct-… | FIXED by 3abdda1, in two independent places, because the row's "one root
cause" guess was wrong (see the correction below).

1. `src/typechecker/exprs.rs`, the `TupleIndex` arm now peels `Ref`/`MutRef`
   before matching `Type::Tuple`, exactly as `infer_field_access` already did
   for struct receivers. Projection yields the element's BY-VALUE type -- a read
   through a borrow, not a reborrow -- so nothing downstream learns a new shape,
   and the out-of-bounds arm still fires through the peel.

2. `src/typechecker/expr_method_call.rs`, a tuple receiver reaching method
   dispatch is now rejected `no method 'X' on type '(i64, i64)'`, and the
   `to_string` intercept gained a `Type::Tuple` arm gated on every element being
   Display -- typing the one method tuples actually have is what lets the
   rejection land without taking it down too.

Pins: three tests in `tests/typechecker.rs` -- projection through a ref
(including nested, plus the surviving bounds check), method-existence checking
on both by-value and ref tuples (plus all three poison-unification spellings),
and `to_string` surviving AND being typed `String`. |
| B-2026-08-11-12 | codegen | high | a borrow-returning accessor's payload handed through `unwrap_or` (`m.get(k).unwrap_or(d)`, `v.get(i).unwrap_or(d)`) is an ALIAS of the container's st… | FIXED by bf43b275. `unwrap_or`'s PRESENT path now deep-clones the payload when the receiver is a borrow-returning accessor, so the result the caller owns is not the container's stored buffer. Reuses the match path's own shape gate -- split out of `borrow_get_payload_clone_te` as `borrow_payload_clone_te_gate` -- so the two paths agree on which payloads are safe to clone and how deep. Pins: e2e `test_e2e_borrow_accessor_unwrap_or_clones_heap_payload` (6 cases incl. the scalar and absent-key controls) and asan `asan_borrow_accessor_unwrap_or_clones_heap_payload`, both stash-proven red -- the ASAN one with an explicit double-free. |
| B-2026-08-11-16 | autopar+codegen | high | Auto-par (ON BY DEFAULT) silently DROPS every LITERAL-step accumulator in a `while` loop that also contains a NON-literal-step one: `while i < n { b… | FIXED by 7fe2b6dd — one condition in `classify_loop_body`
(src/concurrency.rs). The induction-step skip was tied to a SHAPE when it
needed to be tied to a NAME.

    // before
    if induction_step_via_assign(value, &name) {
        // i = i + const_lit -- loop-counter step; ignored.
    } else { ...reduction... }

    // after
    if induction_step_via_assign(value, &name) && induction_var == Some(name.as_str())
    { ... }

THE COMMENT ABOVE THAT LINE ALREADY STATED THE INTENT CORRECTLY -- "an explicit
`while`-loop counter would be tagged as the reduction accumulator" -- so the
defect was that the test never checked the write was against the loop's OWN
counter. `x = x + <literal>` is the counter's shape, but it is also any
literal-step accumulator's shape, so `a = a + 1` was swallowed by a branch
written for `i = i + 1`: neither classified as a reduction nor rejected, just
dropped on the floor. The fan-out lowering then rebinds only the accumulator and
the loop variable per worker and captures everything else, so `a`'s writes went
into per-worker copies and the parent kept its pre-loop value.

A new helper `loop_induction_var(expr)` names the counter -- the `for` pattern
binding, or the variable in a `while k < end` condition via
`par_cost::parse_lt_condition`, which is the SAME matcher the cost model and the
fan-out lowering use, so the analysis agrees with the shape codegen will
actually lower. The same one-name rule was applied to the `+=` arm, which had
its own copy of the skip.

WITH THE NAME CHECK, A NON-COUNTER LITERAL-STEP WRITE BECOMES A REDUCTION
CANDIDATE -- which is what it is, `+` with a constant delta -- and the existing
"two distinct accumulators decline the loop" rule three lines below then rejects
the mixed loop by itself. No new rejection logic was needed; the bug was that
these writes never reached that rule.

CORROBORATION THAT THIS WAS THE ODD ONE OUT, not a new restriction: the SAME
statement wrapped in a conditional (`if cond { a = a + 1; }`) was always
classified as a reduction by `conditional_acc_update_shape`, with no
induction-shape exclusion at all. The bare form was the only spelling that
vanished.

TWO BEHAVIOUR CHANGES, both measured, neither a correctness risk:
  - A loop whose ONLY accumulator is literal-step (`while i < n { a = a + 1;
    i = i + 1; }`) is now a recognized reduction where before it was invisible.
    That is a legitimate reduction (identity 0, associative, commutative) and
    the codegen cost gates still decide whether to lower it. Verified correct.
  - A `while` whose counter cannot be named -- e.g. `i <= n`, which
    `parse_lt_condition` does not match -- now treats its counter as a reduction
    candidate, so a loop with a real accumulator alongside it DECLINES instead
    of lowering. That is perf-only and fail-safe, and `while k < hi` is the
    shape the reduction lowering supports anyway. Verified the answer stays
    correct.

PINS. Three in tests/concurrency.rs (analysis level): the mixed loop must
decline, the `+=` spelling must decline, and the ordinary single-accumulator
loop must STILL be recognized -- that last one is the guard against fixing this
by breaking auto-par generally. One in tests/par_codegen.rs (value level):
asserts `4 6 3`, and pre-fix produces `1 0 3`.

THE VALUE PIN HAD TO MOVE, and the reason generalises. It was written first in
tests/codegen.rs, where it PASSED PRE-FIX and so asserted nothing: that
harness's `run_program` compiles without `concurrency_analyze`, so auto-par
never fires there and the sequential fallback happens to give the right answer.
tests/par_codegen.rs has its own `run_program` that threads the analysis into
codegen -- its doc comment says exactly this -- and there the pin fails pre-fix
with `left: Some("1 0 3\n")`. AN AUTO-PAR E2E ASSERTION PLACED IN
tests/codegen.rs IS VACUOUS; it must go in tests/par_codegen.rs. Checked by
stashing the fix and re-running, which is the only reason the vacuous placement
was caught.

VERIFICATION. The 12-line repro and every variant from the row's boundary map
now give the right answer, as does the original 648-case harness this was found
in (`combinations=648`, was 0). Full `cargo test --features llvm` green; ASAN
clean at the default level and on the `-O0` leg. Also re-ran the sort sweep the
harness was built for: 648 combinations, 0 failures. |
| B-2026-08-11-17 | interp | high | The INTERPRETER's `sort_by_key` with a float key returns a COMPLETELY UNSORTED sequence when a NaN is present, while both compiled backends sort corr… | c6c34c3 (two independent causes in one expression: the interpreter's `value_compare` float arm went from `partial_cmp(..).unwrap_or(Equal)` to `total_cmp`, which fixes the incoherence; and BOTH that arm and the runtime's `karac_float_cmp` now canonicalize NaN first, which fixes the provenance split `total_cmp` alone left behind.) |
| B-2026-08-11-18 | codegen | medium | Chained field access on an Option-unwrap TEMPORARY fails in codegen while working in the interpreter: `get().unwrap().x` where `get() -> Option[P]` e… | FIXED by e23bf9e, in `src/codegen/expr_ops.rs`.

`type_name_of_expr`'s `MethodCall` arm gains the UNWRAPPING siblings, and a new
`unwrap_receiver_inst` resolves the receiver's wrapper instantiation with
generic args intact.

ROOT CAUSE, and it is a deliberate exclusion rather than an oversight.
B-2026-08-09-7 taught this same arm the wrapper-PRESERVING combinators
(`map`/`map_err`/`and_then`/`or_else`/`filter`), which return the receiver's own
type, and its comment explicitly lists the unwrapping siblings as NOT covered
because they return the PAYLOAD -- for which `recv_ty` ("Option") is the wrong
answer. That reasoning was right. What was missing is the payload's own name,
which needs the receiver's generic ARGS, and `fn_return_type_names` keeps only
the bare head segment. The full-TypeExpr table (`fn_return_type_exprs`) already
existed for exactly this reason -- `declare_function`'s comment names the
`Option[T]` / `Result[T, E]` generic arg it recovers -- it was simply never
consulted from this path.

Two parts:

1. `unwrap_payload_idx` -- which generic arg the method returns. Arg 0 for
   `unwrap` / `expect` / `unwrap_or` / `unwrap_or_else` (`T` in both
   `Option[T]` and `Result[T, E]`); arg 1 for `unwrap_err` (`E`, `Result`-only
   -- `Option` has no error arm). `map_or` / `map_or_else` are excluded because
   they return the CLOSURE's result, which the receiver's instantiation does
   not name at all. The lookup sits after the `fn_return_type_names` probe, so
   a user type named `Option` with its own `unwrap` still wins.

2. `unwrap_receiver_inst` -- the receiver's instantiation. A CALL receiver
   resolves through the callee's declared return TypeExpr, and that probe runs
   FIRST, before `enum_inst_type_of_expr`, whose last resort is a span lookup:
   the parser gives a `MethodCall` its RECEIVER's span, the collision
   B-2026-08-06-19, -12 and B-2026-08-05-28 all trace to, so consulting it
   first on a call receiver risks a neighbouring record instead of a clean
   miss. An IDENTIFIER receiver still goes through `enum_inst_var_types`.

THE ROW'S CHARACTERIZATION WAS TOO BROAD and the correction matters, because it
is what localizes the bug. The row said "the gap is specifically the un-bound
temporary receiver". Measured, it is not about temporaries at all:
`plain().x` (a struct-returning fn) and `P { x: 11 }.x` (a struct literal) are
both un-bound temporaries and both always compiled, and
`get().unwrap().double()` -- a METHOD call on the very same receiver -- also
always worked, because method dispatch resolves through a different path. Only
FIELD access on an unwrap-family result failed. All three are carried in the
test as live controls.

TWO THINGS THE ADVERSARIAL PASS CHANGED, both found by trying to break the fix
rather than by confirming it:

  * `unwrap_err` was excluded in the first version, on the reasoning that its
    payload sits at a different index. That was SAFE -- it failed loudly rather
    than resolving wrongly -- but it left the same gap one method over, so the
    index was made method-dependent instead. This is the case worth being
    careful about: answering arg 0 for `unwrap_err` would read `T`'s field
    layout out of an `E` value, a miscompile rather than a failed lookup.
    `Result[P, Q]` with `Err(Q { y: 99 })` reading 99 is the assertion that
    pins it.
  * The nested `Option[Option[P]]` chain failed, because the intermediate
    payload is itself a wrapper and the first gate admitted only structs.
    `unwrap_receiver_inst` now recurses through a chained unwrap, and the gate
    admits `enum_layouts` too. The recursion lives in the instantiation
    resolver rather than the name lookup on purpose: the next link needs the
    full `Option[P]`, and a head name would drop the `[P]`.

MEASURED on interpreter / JIT / AOT, all identical: single unwrap 6, `expect`
6, `unwrap_or` 6, `Result` Ok 10, `unwrap_err` 100, nested chain 43, identifier
receiver 4, plus the three controls. Before the fix the six chained forms
failed `karac build` while the interpreter ran every one of them.

Test: `e2e_chained_field_access_on_option_result_unwrap` (tests/codegen.rs),
on the default codegen leg because the divergence is run-vs-build. Full
`--features llvm` suite green (102 targets, 13431 passed); fmt and clippy
--all-targets clean. |
| B-2026-08-11-19 | interp+codegen | medium | The DIRECT-on-Vec iterator terminals (`v.max()` / `v.min()`, desugared to the `.iter()` chain) are POSITION-SENSITIVE: they compile in statement/argu… | FIXED by cb3d9015 — the desugar gate was reading a side table through a key that CANNOT
distinguish the calls in a chain.

THE ROW'S PREMISE WAS WRONG, and the correction is the useful part. This is not
about string concatenation and not about "position". `let s =
xs.max().unwrap().to_string();` fails with no concat anywhere, while
`println(xs.max().unwrap() + 1)` — a binary operand, the shape the row blamed —
works fine. THE TRIGGER IS ONE EXTRA METHOD CALL CHAINED ONTO THE RESULT.
Measured across seven positions: bare statement, `let`-bound then concatenated,
arithmetic operand, f-string interpolation, struct-literal field and call
argument all work; every spelling that chains a further call onto the terminal
failed.

MECHANISM, PROVEN RATHER THAN INFERRED. Dumping `method_callee_types` for the
working and failing programs shows ONE entry, at the SAME key:

    println(xs.max().unwrap())               SpanKey(48, 2) -> Vec.max
    let s = xs.max().unwrap().to_string()    SpanKey(48, 2) -> i64.to_string

The parser sets a MethodCall's span equal to its RECEIVER's span, so every call
in a chain hashes to one key and the last write wins. `src/lowering.rs` gated
the `.iter()` insertion on that entry's head being "Vec"/"VecDeque"; with
`.to_string()` chained the entry read `i64.to_string`, the head test failed, no
`.iter()` was inserted, and the backends met a raw `Vec.max()` they do not
implement. Hence the misleading nouns this row was filed for: interp "method
'max' not found on type 'Vec'", codegen "Vec/String method 'max' is not yet
supported" — both blaming `max`, which was never the problem.

THE FIX IS TO STOP RE-DERIVING THE DECISION. `infer_method_call`'s Vec/VecDeque
narrowing arm is the one place that actually decides a call is a direct
iterable-collection terminal, so it now records the site itself, in a dedicated
`TypeCheckResult::direct_iter_terminals` set, and the lowering asks that instead
of reconstructing the answer from a receiver-type NAME. The two sides can no
longer disagree about what the narrowing means.

The new table is keyed by `SpanKey::for_method_call` — the CLOSING-PAREN span,
a leaf no outer expression aliases. Both halves of that were already in the
codebase and simply unused here: `SpanKey::for_method_call` is documented as the
key "that disambiguates chained calls", and `infer_method_call`'s own
`args_close_span` parameter carries a doc comment describing precisely this
aliasing ("`span` ... the parser sets equal to the receiver's span, so ... any
outer chained `MethodCall` clobbers `expr_types[span]`"). The infrastructure was
there; this side table just wasn't using it.

DELIBERATELY NOT RE-KEYED GLOBALLY. `method_callee_types` has 6 insert sites and
7 readers (monomorphization, codegen/method_call, codegen/lazyframe x2,
unsafe_lint, must_use_lint x2), any of which may depend on today's
last-write-wins behaviour. Re-keying it is a strictly larger change than this
row, and the collision remains latent for those consumers.

VERIFIED, both backends: all four terminals (`sum` / `product` / `max` / `min`)
on `Vec` and on `VecDeque`, chained; and the narrowing is intact —
`SortedSet.max()` / `.min()` keep their own surfaces and are NOT rewritten
(pinned separately, since moving the decision into the typechecker could have
widened it).

TWO THINGS THIS DOES NOT FIX, both pre-existing and both confirmed independent
of the desugar:
  - `Vec[String]` iterator terminals are unsupported in codegen. The error now
    names `iter` rather than `max`, which still reads oddly for someone who
    wrote neither — but the USER-WRITTEN explicit `v.iter().max()` produces the
    identical error, which is what proves the desugar is not the cause.
  - Chaining onto a SCALAR's `.to_string()` breaks codegen in two ways, both
    interp-correct: `n.to_string().len()` is a clean error ("no handler for
    method 'to_string'") for i64 / f64 / bool, and `n.to_string().to_string()`
    PANICS the compiler at src/codegen/method_call.rs:2683 ("Found IntValue but
    expected the StructValue variant") — it loads the i64 and treats it as a
    String struct. Same chained-call span-aliasing family as this row, different
    table. Filed separately.
    (Measured twice: a first pass reported these as silent wrong ANSWERS —
    printing a constant 3, and once printing a string from an unrelated
    program. Both were artifacts of a probe script that fell through to a STALE
    binary when the build failed without the word "error" on stderr. Checking
    the exit code instead showed rc=101, a panic. Worth recording because the
    false reading was the more alarming one and would have gone into the ledger
    as a miscompile.) |
| B-2026-08-11-20 | typecheck+codegen | low | `f64.to_bits()` is declared `-> u64` by the typechecker but its value renders as a SIGNED i64 on all three backends: `(-1.0).to_bits()` prints -46161… | FIXED by 1597660. `expr_is_unsigned_int` (`src/codegen/expr_ops.rs`) gains a
`to_bits` / `to_bits32` arm returning `true` unconditionally, alongside the
existing `abs_diff` one -- the two methods whose result signedness is fixed by
the METHOD rather than the receiver. Recursing on the receiver (what the
Self-returning methods below it do) is wrong here precisely because the
receiver is a float, so it answered `false` and codegen emitted `%lld`.

Pin: `e2e_to_bits_renders_unsigned` in `tests/codegen.rs` -- negative, negative
non-power-of-two, a positive control, `to_bits32`, and the bare `println` path
alongside the f-string one. |
| B-2026-08-11-21 | codegen+interp | high | EVERY un-annotated `let` holding an unsigned value prints SIGNED under both compiled backends while the interpreter prints it correctly -- `let a = 1… | FIXED by cacd93e1, both legs, and they turned out to be the SAME defect seen from two sides:
a type recorded against a span that something else overwrites.

LEG 1 (codegen, the inferred `let`) — fixed UPSTREAM of the classifier, which is
what the row asked for. Codegen's un-annotated `let` path already ends in
`record_var_type_name` for whatever surface it finds in `pattern_binding_types`;
the gap was that the TYPECHECKER never wrote an entry there for a scalar. It
records Tuple, Map/Set, Option/Result and generic user structs, and primitives
fell straight through. One arm in `bind_pattern_types`
(`src/typechecker/patterns.rs`) recording `Type::Int(_) | Type::UInt(_)` via the
existing `method_callee_type_name` therefore fixes every consumer of
`var_type_names` at once, rather than patching the `%llu`/`%lld` classifier
alone -- so the narrow-int widening and coercion paths the row flagged are
covered by the same entry. Signed sizes are recorded alongside unsigned on
purpose: the classifier now answers "not unsigned" from a PRESENT entry instead
of from an absent one, so no future consumer can mistake "unrecorded" for
"signed".

LEG 2 (interpreter, `u64.to_string()`) — the interpreter ALREADY had an unsigned
arm; it was asking the wrong span. `span_type_is_unsigned64(&object.span)` reads
`expr_types`, and the parser sets a MethodCall's span equal to its RECEIVER's,
so by the time the interpreter runs that entry holds the CALL's result. Measured
directly rather than inferred: for `hi.to_string()`,
`expr_types[SpanKey(66, 2)] = Str`. That is also exactly why `f"{hi}"` was
correct all along -- its interpolated expression is the bare identifier, whose
span nothing aliases -- which is the asymmetry the row found so surprising.

The typechecker now stashes an integer receiver's type at the CLOSING-PAREN span
for `to_string`, and the interpreter reads there first, falling back to the
receiver span for unaliased shapes. That hatch is not new: `args_close_span`
exists for precisely this, its doc comment on both `infer_method_call` and
`eval_method_call` describes this clobber, and `pow` plus the bit intrinsics
already read through it via `int_width_at`. `to_string` simply never opted in.

VERIFIED interp == JIT == AOT, character for character, on all five producing
shapes from the row (suffixed literal, fn return, binop, `to_bits`,
`reverse_bits`) plus `to_string` on both a bound and an annotated u64, `usize`,
and the two controls that were already correct and had to stay so: an annotated
binding, and interpolating the producing expression directly. A SIGNED binding
is the control in the other direction -- a fix that simply rendered every
`Value::Int` unsigned would turn -1 into 2^64-1, and both pins assert it stays
-1.

TWO CONSEQUENCES THE SUITE SURFACED, both worth recording.

  1. TWO EXISTING TESTS WERE PINNING THE BUG AS EXPECTED OUTPUT.
     `test_codegen_primitive_const_u64_max_bit_pattern_preserved` and
     `..._usize_max` asserted "-1" for `let x = u64.MAX; println(x)`, with a
     comment calling the signed rendering "a separate concern; the constant
     value is correctly emitted". It was not separate -- it was this bug, and
     `let x = u64.MAX` is exactly the un-annotated shape leg 1 is about. Both
     now assert 18446744073709551615. This is how the row's "invisible for over
     a month" happened: the wrong answer had a green test defending it. Both
     were also on the tolerant `if let Some(out)` form, which asserts NOTHING
     when the runtime archive is missing, so they were tightened to strict
     `assert_eq!` while being corrected.

  2. THE COROUTINE STATE-STRUCT LAYOUT WANTS THE ABSENCE, uniquely.
     `state_struct_layout_primitive_typed_bindings_have_none_type_name`
     documents a real contract -- codegen falls through to its primitive-sizing
     path on an ABSENT entry, so a present "i64" would send it down the
     named-type path instead. Recording scalar names broke that assertion, and
     the assertion was RIGHT: this was a genuine risk of miscompiled coroutine
     state, not a stale expectation. Every other consumer of the map wants the
     scalar name, so the narrowing was put at that one reader (a filter in
     `record_entry`, src/cli.rs) rather than at the recording site.

FOURTH INSTANCE OF ONE ROOT CAUSE, which is now the thing worth carrying
forward. B-2026-08-11-19 (the direct-Vec iterator desugar), B-2026-08-11-22
(both `to_string` routing gates), and now this row's leg 2 are all the same
mechanism: a side table keyed by `SpanKey::from_span` on a method call, whose
span the parser aliases to the receiver's, so a chain -- or merely a call
wrapping an identifier -- collapses to one key and the last write wins. Each has
been fixed by giving that ONE consumer a different signal: a dedicated
collision-free table (-19), the receiver's own static type (-22), the
closing-paren stash (here). None re-keyed `method_callee_types` or `expr_types`,
so the collision is still latent for every other consumer. The recurrence rate
suggests the next one is a matter of when. |
| B-2026-08-11-22 | codegen | medium | CHAINING ONTO A SCALAR'S `.to_string()` BREAKS CODEGEN TWO WAYS, both check-green and both interpreter-correct: `n.to_string().to_string()` PANICS th… | FIXED by 13b4814d — and the row's HYPOTHESIS WAS RIGHT: it is the same span-aliasing
mechanism as B-2026-08-11-19, in the two gates that route a `to_string` call.

CONFIRMED BY INSTRUMENTATION, not by reading. Printing the gate's inputs for
`n.to_string().to_string()`:

    outer link  obj=MethodCall  dispatch_key=Some("String.to_string")  string_like=true
    inner link  obj=Identifier  dispatch_key=Some("String.to_string")  string_like=false

The inner link is reading the OUTER's key. The parser sets a MethodCall's span
equal to its receiver's, so both links share one `method_callee_types` entry and
the last write wins. There IS already a chained-call collision guard where
`dispatch_key` is computed -- it separates links by requiring the key's method
segment to match this call's method -- but it cannot help here, because BOTH
links are named `to_string`. That is the specific reason this shape slipped
through a guard written for exactly this class.

TWO GATES WERE READING THAT KEY, which is why the row had two arms:

  1. The String-copy path (`method_call.rs`) tested the key's type segment for
     "String"/"StringSlice". The inner link passed it with an `i64` receiver,
     compiled `n` to an IntValue, and unwrapped it as a struct: the compiler
     panic.
  2. `recv_is_scalar_primitive` tested the same key for a scalar type name. With
     the key shadowed to "String.to_string" it declined too, so once the panic
     was vetoed the call fell through to the catch-all as "no handler for method
     'to_string'". Fixing only the first arm converts a panic into a wrong
     error, which is why both had to move.

BOTH NOW CONSULT `type_name_of_expr(object)`, which is keyed by the receiver
EXPRESSION and so cannot be shadowed. It separates the two links exactly -- the
inner reports Some("i64"), the outer None -- so nothing that previously worked
changes. It is also a static lookup, which preserves the property the scalar
gate was written for: the receiver is NOT pre-compiled, so a side-effecting
receiver is still evaluated once. The veto is applied ONLY to the span-keyed
half of the String test; `expr_is_string_like` reads the receiver expression, so
it is left alone.

SECOND LEG, found while building the ASAN fixture and fixed here because leaving
it would have made the surface MORE irregular, not less: `x.clone()` as a
receiver in a chain had no resolvable type, so the same shadowed key broke
`s.clone().to_string().len()` (String receiver) and `n.clone().to_string()
.len()` (scalar). `clone` preserves its receiver's type, so `type_name_of_expr`
now resolves it by recursion, and `expr_is_string_like` does the same. Recursion
rather than adding `clone` to the neighbouring string-like NAME list, because
that list deliberately excludes `sorted`/`replace`/`repeat` for naming Vec
methods too -- a blanket `clone` entry would misroute `v.clone().to_string()` on
a Vec into the String-copy path. `v.clone().len()` is pinned as that control.

VERIFIED build == interp on i64 / f64 / bool / char, for `.to_string()
.to_string()` and `.to_string().len()`, and for identifier, struct-field and
call-result receivers. Controls that must keep their existing routing all hold:
String and literal receivers, `trim()` / `to_uppercase()` / `clone()` chains,
and `Vec.clone()`.

ASAN, which the value pins cannot cover: the String-copy path both mallocs a
fresh buffer and frees the intermediate receiver when it is a fresh owned
String, and this fix changes WHICH calls reach it -- so a routing fix with the
ownership wrong is a per-iteration leak or a double free. The new fixture loops
both sides of the veto (a scalar receiver that must now allocate its own String,
and a String receiver chained through a builtin that must still free the
intermediate) and is clean at the default level and on the -O0 leg.

STILL LATENT, and worth stating plainly since this is now the third row in this
family: `method_callee_types` is still keyed by `SpanKey::from_span`, so a chain
still collapses to one entry for its 6 writers and 7 readers. B-2026-08-11-19
fixed one consumer by giving it a collision-free side table; this row fixed two
more by having them consult the receiver's own type instead. Neither re-keyed
the map. A consumer that needs the key and cannot use either workaround will hit
this again. |
| B-2026-08-11-23 | codegen | low | `Vec[T].sorted_by_key(f)` PASSES `karac check` and then fails at build: "Vec/String method 'sorted_by_key' is not yet supported in codegen" | a243f2a (folded into the existing `sorted_by` arm in src/codegen/vec_method.rs rather than copied — the two are one desugar with a different inner method) |
| B-2026-08-11-24 | codegen | high | A String EQUALITY comparison between an unbound TEMPORARY and a `ref String` PARAMETER leaks the temporary, every evaluation | 7239101 (one arm in src/codegen/exprs.rs: extend the surface-`Binary` fresh-owned-operand free from `Add` to `Eq`/`NotEq`, gated on the OPERANDS being String-shaped since a comparison yields i1) |
| B-2026-08-11-25 | codegen | high | DOUBLE FREE on both compiled backends: a heap field of a struct held as a Vec ELEMENT, read back by ASSIGNMENT to an existing binding (`out = stats[0… | FIXED by 970dadfd — the Assign arm never called the field-move-out suppressors the Let arm
has called since B-2026-08-03-8 / B-2026-08-01-31. Three lines of dispatch, not
new machinery.

WHAT THE DIAGNOSIS TURNED ON: the row's own control table already said the
`let` form of the identical read is clean, INCLUDING through a Vec index. So
`suppress_place_field_struct_move_source` — the deeper-place suppressor that
resolves the owner through `field_chain_place_ptr` / `place_chain_type_name` —
already handled `stats[0].region` exactly. It had simply never been reachable
from an assignment: grepping its call sites (plus
`suppress_struct_field_move_into_literal` and `disarm_struct_field_move_bodies`)
finds the Let arm and struct-literal field init, and nothing else. The
ASSIGNMENT path compiled the RHS and stored it while leaving the field live in
its owner, so the owner's drop freed the buffer the target now owned.

That is why the row's three necessary conditions are what they are, and each is
now explained rather than just observed: the read must be an ASSIGNMENT (the
`let` arm calls the suppressors), the struct must be reached through a Vec INDEX
(a plain binding or `self` takes the SHALLOW suppressor, which the Let arm also
calls but which a deeper place never reaches), and the field must be heap-owning
(there is nothing to double-free otherwise).

The fix mirrors the Let arm's block verbatim — same three calls, same gates
(`owned_struct_params` sources are deep-copied instead, shallow forms take the
dedicated suppressor, deeper places take the place-chain sibling) — so the two
arms cannot drift apart again. Gated on the assignment TARGET being a
heap-shaped local, which is the Let arm's `vec_elem_types` gate.

ORDERING IS LOAD-BEARING and is commented at the site: the calls go AFTER
`compile_expr(value)`, because they cap-zero the SOURCE in place. Emitting them
first hands the target a cap of 0 and converts the double free into a leak —
the same bug wearing different clothes, and one the value pins cannot see.

VERIFIED on the row's full eight-row table: the four aborting shapes (String
field, `Vec[i64]` field, an unrelated earlier read of the same field, a source
built by `+` instead of `push_str`) and the four over-fire controls that were
already clean and that a broad fix would regress (the `let` form, a plain struct
binding, a tuple element, a `Vec[String]` with no struct). All eight now agree
with the interpreter. The row's real-world shape — a max-by-revenue scan
assigning `best_region = stats[j].region` in a loop — runs correctly too.

ASAN IS THE GATE THAT DISTINGUISHES A FIX FROM A RELABELLING, since the repair
is precisely "stop the owner freeing". The new fixture loops 40 iterations so a
per-iteration leak accumulates, and covers String fields, a `Vec[i64]` field and
a `+`-built source. Clean at the default level and on the `-O0` leg; pre-fix it
reports `AddressSanitizer: attempting double-free`. The value pin pre-fix
returns `Some("")` — the abort beats the stdout flush, which is exactly the
"appears to produce NO output" failure mode the row warns sends debugging in the
wrong direction.

FIXTURE HAZARD, carried forward from the row because it is easy to re-introduce:
every payload in both pins is built at RUN TIME (`String.new()` + `push_str`, or
`a + b`). A `String` LITERAL field is static, so the second free lands on a
non-heap pointer and the fixture passes while the bug is present. |
| B-2026-08-11-26 | other | medium | the codegen suite's JIT lane — the ONE lane whose stated job is run==build parity — fed codegen `ownership: None` while its AOT twin fed `Some(&owner… | FIXED by 883fcbe1 — `jit_dispatch` now takes the `OwnershipCheckResult` the harness ALREADY computes a few lines above the dispatch, and forwards it to `compile_to_ir_with_options`, so the JIT leg's codegen arguments match its AOT twin's `compile_to_object_with_options(..., Some(&ownership), None, ...)` argument for argument.

`concurrency` stays `None` on BOTH legs, deliberately, and that is parity rather than a second half-fix: `tests/codegen.rs` is the SEQUENTIAL lane by design and `tests/par_codegen.rs` is the auto-par one (its own comments name the split — "`tests/codegen.rs`'s harness compiles without `concurrency_analyze`"). The bar for this lane is its AOT twin, not cli.rs.

VERIFIED both legs, full suite, on the same tree:
    KARAC_TEST_JIT=1 cargo test --features llvm --test codegen  →  2902 passed, 0 failed
                     cargo test --features llvm --test codegen  →  2902 passed, 0 failed
Before the fix the JIT leg was 2900 passed / 2 failed with the AOT leg at 2902 / 0 — and the 2900 that passed anyway are why this sat undetected: only a test whose outcome DEPENDS on an ownership hint can see the starvation, and until B-2026-08-10-21 landed its two pins, the suite had none. |
| B-2026-08-11-27 | autopar+codegen | high | NONDETERMINISTIC SILENT WRONG ANSWER on the DEFAULT `karac build`: a 13-line program that overwrites a `Vec[Tensor]` element through a `shared struct… | FIXED by 7dfc17c. `collect_expr_inner_writes`'s `Call` arm
(`src/concurrency.rs`) now records a write on an argument whose parameter is a
SHARED type AND whose callee body writes that parameter, alongside the existing
`mut`-marker and `MutRef` gates. Two helpers: `type_is_shared` (against the
`shared_type_names` set the checker already builds) and `callee_writes_param`
(walks the callee body with the existing `collect_block_inner_writes`, which
covers both the `s.f[i] = …` assignment spelling and the `s.f.push(…)` method
spelling; re-entry guarded through the closure walk's name stack).

Pins: two tests in `tests/concurrency.rs` -- the mutation case, stash-proven red
against the pre-fix analysis, and the read-only case as the over-serialization
guard. Both use a plain `Vec[i64]` shared struct. |
| B-2026-08-11-29 | codegen | high | SEGV (exit 139) on a DEFAULT `karac build`: a struct with a `Map`/`Set` field, bound out of a `Result`'s `Ok(..)` match arm and then passed BY VALUE… | FIXED by 8e1f96dc — the row's hypothesis was the right family but the wrong member. It is
not a missing Map/Set half on a promotion arm; it is the WHOLE-BINDING sibling
of `wrapper_arm_moves_heap_field_to_free_fn`, which had only ever detected a
moved FIELD.

WHAT ACTUALLY HAPPENS, read off the IR and confirmed by valgrind rather than
inferred. The Ok arm binds the struct and passes it BY VALUE, so the callee owns
it and ends with `__karac_drop_struct_S`, which frees the Map. The arm's source
payload drop stayed armed, so `main`'s `respl.ok` block called
`karac_drop_Map_String_i64` on the same handle: valgrind reports an invalid read
inside `karac_map_free_with_drop_vec` over a block already freed by `main`, from
a `karac_map_new` allocation. Not a double free at two call sites — a
use-after-free with both ends inside `main`.

Two false leads are worth recording because each looked decisive:

  - `take()` appeared to free nothing. Grepping its IR for `drop_Map`/
    `karac_map_free` returns zero hits — the callee's free is spelled
    `__karac_drop_struct_S`, one level up. The by-value param IS callee-owned.
  - `main` appeared to hold only ONE Map drop, which argued against a double
    free at all. It does hold one; the other free is the callee's, inlined.

WHY THE EXISTING GATE MISSED IT. The arm-suppression condition is
`!(borrow_only && wrapper) || heap_field_moved_to_free_fn || binding_owns_
payload`. For `Ok(s) => take(s)`: `wrapper` is true; `borrow_only` is
SPURIOUSLY true, because `consume_class` scores a free-fn argument as
entry-copied (the exact blind spot the `heap_field` override was added for);
`heap_field_moved_to_free_fn` is false because the arm passes `s`, not `s.a`;
and `binding_owns_payload` is false because `field_copy_supported` excludes
`Map`/`Set`, so the struct is not entry-copy supported. Nothing fired.

That last clause is also the whole explanation for the row's sharpest control:
a `Vec[String]` field IS entry-copy supported, so `binding_owns_payload` is
already true and suppression already fires. Verified in the IR before touching
anything — the Vec variant emits `respl.suppress.at.*` on BOTH arms (20
occurrences), the Map variant only on the Err arm (10). The new condition is
therefore additive on the shape that already worked, which is what keeps the
control clean rather than merely appearing to.

THE FIX: `wrapper_arm_moves_whole_binding_to_free_fn`, gated to a non-shared
user struct binding, plus `consume_class::bindings_passed_whole_to_free_fn_arg`
(bare identifier arguments only — `take(s.a)` is the field case and
`take(s.a.len())` reads rather than moves). Added as one more override on the
same condition, so both move shapes are handled at one site.

VERIFIED on the row's seven-row table plus an Err-arm variant: both crashing
shapes (Map, Set) now agree with the interpreter, and all five over-fire
controls stay clean — `Option` instead of `Result`, no wrapper, a bare Map
payload, inline use, and the `Vec` field. Under valgrind with `--leak-check=full`
the fixed Map, Set and Vec binaries report 0 errors and no leaks, which is the
direction that matters here: the repair is "stop the SOURCE freeing", so
over-suppressing would leak instead of crash.

TEST-INFRA NOTE, filed separately: a single program combining all seven shapes
fails MODULE VERIFICATION inside `tests/codegen.rs`'s harness ("Function return
type does not match operand type of return inst") while `karac build` and
`karac run --interp` both compile and run that same program correctly, with and
without this fix. The pin is therefore written as one small program per shape.
That divergence is not this bug and is not caused by it, but it means the E2E
harness can reject a program the compiler accepts. |
| B-2026-08-11-30 | codegen | medium | A by-value `Option`/`Result` parameter that the callee never DESTRUCTURES is dropped by NO frame, so the entire `Ok`/`Some` payload leaks: the caller… | FIXED by 19d0e9a.

Own by transfer for the two built-in enums, mirroring B-2026-08-05-33's struct
arm one type-class over. `compile_function` (src/codegen/functions.rs) now
registers the inline Option / Result / Option-Map payload drops on a by-value
`Option`/`Result` param's slot, gated on the param NEVER ESCAPING
(`nonescaping_param_names`). No entry copy: the caller has already transferred,
which the emitted IR shows directly as a whole-slot `store zeroinitializer` into
the source binding, so there is no original left to protect.

That gate keeps exactly one owner in each direction. A param that reaches a
return hands ownership back, and the caller's arg-site zeroing is itself
suppressed for that shape (`!call_arg_flows_into_return`), so the caller kept
its drop and the callee must not add one. A param used only as a match
scrutinee is consumed in place, and the standard local move-suppression retracts
this registration exactly as it already does for the entry-copied user-enum
params that share the path.

Verified over the row's own -O0 matrix under valgrind, 200 iterations: all seven
leaking shapes go to zero (String and Vec payloads, fresh-temp and let-bound
arguments, Option and Result, struct and bare-container payloads) and all
thirteen controls stay clean -- discard, inline-match, let-match, arg-match,
bare-struct, bare-Vec, user-enum, let-unused and double-pass.

The two repros this row landed are un-ignored as regression pins, and
`asan_by_value_optres_arg_passed_twice_no_double_free` pins the safety property
the copy-free form rests on: the second pass of the same binding arrives all
zeros, so every `cap > 0` guard skips instead of double-freeing. It reads the
payload in the callee so a drop that fired twice would surface as a
use-after-free rather than passing quietly.

NOT fixed by this, and deliberately so: B-2026-08-12-1, the wrong-variant read
on that second pass. It is the caller-side half of the same split and is
unchanged here. |
| B-2026-08-11-31 | other | medium | `tests/par_codegen.rs` -- the ONLY lane that threads `ConcurrencyAnalysis` into codegen -- had no JIT leg at all, so the DEFAULT `karac build` config… | FIXED by 43face1. `jit_dispatch_par` in `tests/par_codegen.rs` compiles each test program to IR with both analyses threaded and executes it through the sibling `karac_jit_runner`, gated on `KARAC_TEST_JIT=1`; two CI steps schedule it on x86 and arm64. Verified by negative control (a corrupted expectation fails the JIT leg), and hardened to panic rather than soft-skip so it cannot silently become vacuous. |
| B-2026-08-11-32 | codegen | high | a widening cast on an unsigned `Vec` element read through a struct FIELD sign-extends instead of zero-extending -- `h.px[0] as f64` on a `Vec[u16]` h… | 023bc9a |
| B-2026-08-11-33 | codegen | medium | A `#[derive(Eq)]` STRUCT temporary carrying a HEAP field, compared against a `ref` param, leaks that field every evaluation: `mk(hay) == other` with… | 23e5d66 (new `track_fresh_struct_temp_operand` in src/codegen/expr_ops.rs, called from the `Eq`/`NotEq` arm in exprs.rs beside the B-2026-08-11-24 String free: materialize the fresh temp into a slot and register its struct drop, threading the generic instantiation) |
| B-2026-08-11-34 | other | medium | The E2E harnesses guard `parse` errors and then DISCARD `resolve` and `typecheck` errors, while `karac build` stops on either -- so the suite silentl… | FIXED by 9410488b, on top of the gate e0c6bab landed -- but first, the correction
this row needed and the earlier pass could not make without the program:

THE ROW'S PREMISE WAS BACKWARDS, and the error was mine, not the harness's.

It claimed the E2E harness REJECTS source `karac build` accepts. It does not. It
ACCEPTS source `karac build` REJECTS -- the same divergence pointing the other
way, and the worse direction, because it manufactures false coverage rather than
false failure. e0c6bab's investigation reached the right fix while noting the
premise still needed re-measuring "on the same source -- which again needs the
program". The program was recoverable from the session transcript; here it is,
with the answer.

HOW THE WRONG PREMISE GOT WRITTEN. The original combined program used struct
names `SM`/`SS`/`SV`, which trip the Const-class naming rule. I renamed them with
a pattern-list script (`struct SM ` -> `struct Sm `, `Result[SM, E]` -> ..., seven
patterns) that did NOT cover the bare return-type position, so `fn mk_bare() -> SM`
survived while `struct Sm` was declared -- leaving an UNDEFINED TYPE. When I then
went to compare against the CLI, I saved the FULLY-renamed program to disk and
built THAT. So "the CLI compiles it, the harness rejects it" compared two
different programs. The comparison, not the compiler, was the divergence.

THE REAL DEFECT, minimised to four lines:

    struct Sm { a: Map[String, i64] }
    fn mk_bare() -> SM { let mut m: Map[String, i64] = Map.new(); m.insert("k", 1); return Sm { a: m }; }
    fn tm(s: Sm) -> i64 { return s.a.len(); }
    fn main() { println(f"{tm(mk_bare())}"); }

`karac check` / `karac build`: `error[resolve]: undefined type 'SM'`.
`tests/codegen.rs` `run_program`: byte-identical to this row's reported symptom --
"Module verification failed: Function return type does not match operand type of
return inst! ret { ptr } %field / i64 ... %call = call i64 @mk_bare()". Codegen
defaulted the unresolved `SM` to `i64`, declared `mk_bare` as returning `i64`, and
emitted a `{ ptr }` return into it. The verifier's complaint is the symptom; the
undefined type is the cause, and its one-line diagnostic was discarded three
phases earlier.

WHAT THIS COMMIT ADDS ON TOP OF e0c6bab. That commit gated one harness and
grandfathered 39 of the 40 programs it found. This one:

  * WIDENS the gate from 1 call site to all 24 that already call
    `assert_ownership_clean` -- across tests/{codegen,par_codegen,
    memory_sanitizer,coro_e2e,disjoint_differential,http_server}.rs. That call is
    already the marker for "this harness feeds codegen production-shaped input",
    so the two gates belong at the same places. Fallout beyond codegen.rs was one
    test in par_codegen.rs and none elsewhere.
  * FIXES 32 of the 39 grandfathered programs rather than parking them: missing
    `mut` call-site markers (9), single-letter module bindings that are Type-class
    where module `let` needs Const-class (5), `pub fn` returning a private type
    (5), implicit narrowing coercions needing an explicit `as` (5), missing
    `#[derive(Eq)]` (2), refinement narrowing needing the explicit form (2), plus
    `*mut Unit` (an undefined type), unqualified `Some`/`None` resolving to
    builtin `Option` instead of the program's own enum, an unbounded `T` used
    arithmetically, a non-`mut` shared field written through, and a `*mut u8`
    passed where `*const u8` was required.
  * Leaves 6 grandfathered, each with a row: B-2026-08-12-8 (four methods codegen
    implements and typecheck rejects -- those tests were the only thing proving
    the emitters work), B-2026-08-12-9 (the `f64` `sorted` emitter, unreachable
    from valid source under the F64 total-order rule), and one deliberate negative
    test whose whole point is a codegen-layer rejection.

It also turned up a REAL COMPILER BUG: B-2026-08-12-7 -- a union or
`#[derive(Copy)]` struct with a raw-pointer field rejected as not-`Copy` by the
rule whose own suggestion is to use a raw pointer. Fixed here with a pin.
`test_e2e_{size,align}_of_epoll_data_style_union` had been green for months
purely because their typecheck errors were discarded. B-2026-08-12-10 (refinement
literals narrowing for integers but not floats or strings, with a diagnostic that
asserts something false) came from the same sweep and is left open.

THE OTHER HALF OF THIS ROW'S CLASS IS ALSO CLOSED. e0c6bab measured that 46 of
the 52 codegen-invoking harnesses in tests/codegen.rs never ran `desugar_program`
and 47 never ran `expand_gated_stdlib_imports`, so they resolved a different
program than `karac build` does -- this row's divergence on 46 sibling harnesses.
b3a1061 fixed it by giving cli.rs and every harness one shared
`prepare_for_resolve`. Between that, the gate, and this commit, the harness-vs-CLI
gap this row is about is closed on both the pass-list and the error-checking axis.

METHOD NOTE, since this is the second time in two days that a comparison rather
than a compiler produced the finding: when a program "behaves differently" across
two lanes, diff the exact bytes fed to each before believing the lanes differ.
Saving a normalised copy for one side and the original for the other is enough to
invent a divergence out of nothing -- and it survived a `git stash`-based
with/without check, because both halves of that check ran on the same wrong file. |
| B-2026-08-11-35 | cli | high | `karac fix` DESTROYS SOURCE, silently and while reporting success, when the machine-applicable diagnostic sits INSIDE AN F-STRING INTERPOLATION | FIXED by 35d7fec, on both the reported trigger and the class under it.

ROOT CAUSE, exactly as the row's FIX DIRECTION guessed. An interpolation hole
is re-parsed standalone inside a synthetic `fn __interp__() { ... }` wrapper,
and `span_visitor::shift_expr_spans` rebases its spans to absolute file
coordinates afterwards. `CallArg::mut_marker_span` -- added later
(B-2026-08-10-2) and the field the `drop the mut marker` edit is built from --
was NOT in the walker, so it alone kept the wrapper's coordinates while
`arg.span` and `arg.value.span` moved. `emit_marker_deletion` then computed
`end = arg.value.span.offset` (absolute, ~96) minus a marker offset of ~19
(wrapper-relative), producing a 77-byte deletion starting at byte 19. That is
why the diagnostic and its caret were both correct while the edit was not, and
why the row's ASCII / em-dash probes all came back clean: it was never a byte
arithmetic problem.

THE AUDIT THE ROW ASKED FOR ("worth auditing every machine-applicable fix for
the same relative-span assumption") was run mechanically rather than by
inspection: for every `pub <field>: Span | Option<Span>` in `src/ast/*.rs`,
check the field name appears in BOTH halves of `span_visitor.rs`. It found
SEVEN unvisited fields, all auxiliary token spans added after the walker was
written: `CallArg::mut_marker_span`, `StructDef::struct_keyword_span`,
`StructDef::kind_keyword_span`, `StructField::mut_keyword_span`,
`TraitMethod::self_span`, `EffectResourceDecl::provider_trait_span`,
`GenericParam::variance_span`.

Six live inside structs the walker already descends into and are one line each;
all six are fixed here. That matters beyond f-strings: `module.rs` rebases every
span through the SAME walker to give multi-module projects unique offsets, and
its own comment already warns that "a span this walk MISSES stays at its
file-local offset and can still collide". Three of the six --
`struct_keyword_span`, `kind_keyword_span`, `mut_keyword_span` -- are precisely
what `ownership/concurrent_shared.rs` builds the `par struct` migration's
multi-edit `fix_diff` from, so the same corruption was reachable in project mode
with no f-string in sight.

The seventh is not a missing line. Generic parameters, trait bounds and
where-clauses are absent from the walker in their entirety (zero references to
`generic_params`, `bounds`, `where_clause`, `supertraits`; `TraitBound` and
`WhereClause` are never named). That is a subtree needing a new traversal in
both halves, so it is split out as B-2026-08-12-30 rather than ridden in here.

AND A NET UNDER THE WHOLE CLASS, which is the part that generalises. Every
machine-applicable edit is an (offset, length, replacement) triple and nothing
checked that the offset still meant what the diagnostic meant -- the existing
out-of-bounds check passed here, because the bogus range sat comfortably inside
the file. `cmd_fix` now re-parses the rewrite and REFUSES TO WRITE when it has
more parse errors than the input. "No worse" rather than "clean", deliberately:
the `has_parse_errors` branch applies recovery edits to a file that does not
parse, and each pass is meant to reduce the count, not reach zero in one go.
This is the row's "refusing to write when a computed span does not re-lex to the
token the diagnostic named", implemented as a whole-file property instead of a
per-token one -- it covers fix producers that do not exist yet.

VERIFIED, both layers independently. With the span fix in place the repro
rewrites to `f(xs)` and `karac check` passes. With ONLY the span fix reverted,
the guard alone blocks the edit, exits 1, and leaves the file byte-identical to
the original -- so neither layer is load-bearing alone.

PINNED by `fix_inside_an_fstring_interpolation_edits_only_the_marker` on BOTH
the unlabeled form and the labeled one (`f(xs: mut xs)` is the shape
`mut_marker_span` exists for at all, since `CallArg::span` starts at the label).
It asserts the WHOLE resulting file rather than the edited line, because the
failure mode is collateral deletion elsewhere, and then re-runs `karac check` to
confirm the deletion actually resolves the diagnostic that prescribed it.

Suite green at 13491 passed / 0 failed across 116 targets; clippy and fmt clean. |
| B-2026-08-12-2 | codegen | medium | A `Map` field of an `Ok` payload leaks ~72 B per call when the `Result` is LET-BOUND before the match; matching the producing call INLINE is clean, a… | FIXED by c081b16.

The match arm's struct-payload binding is now admitted when the struct's only
non-duplicable heap is a `Map`/`Set` HANDLE, so it registers the
`track_struct_var` that frees it.

WHY IT WAS EXCLUDED, and why the exclusion was one condition too wide. The
`is_inline_optres_struct_payload` arm (pattern_binding.rs, B-2026-07-10-3) exists
for exactly this class -- "the struct's inner heap was owned by NOBODY and
leaked" -- but gates on `aggregate_param_copy_supported_struct`, which is FALSE
for a Map/Set field because the entry copy cannot duplicate a side-table handle.
Copy-supported and drop-supported come apart here: the synthesized struct drop
DOES free a Map field, which is why the same struct let-bound directly
(`let s = mk();` over `fn mk() -> S`) was already leak-clean. The arm needs the
FREE, not the copy, so the new predicate `struct_heap_copyable_or_handle` admits
a direct handle field and nothing else -- a nested struct still has to be fully
copy-supported, so the widening is exactly the measured case.

Nobody else owned it: the consuming arm zeroes the scrutinee's payload words
unconditionally (`respl.suppress.w1..w3`, read off the emitted IR for this
shape), so the `Result`'s own payload drop finds a null handle and frees
nothing. valgrind: 14,400 B direct plus 105,600 B indirect over 200 iterations,
the whole map from `karac_map_new`.

THREE CONDITIONS NARROW IT, and each was forced by a test rather than chosen:

  * the arm must only BORROW the binding. An arm that moves it on
    (`Ok(s) => take(s)`) hands ownership to the move machinery, whose
    source-zeroing clears Vec/String CAPS and not a side-table handle -- so a
    drop registered here is never retracted and the handle is freed twice. That
    is B-2026-08-11-29's SEGV, and its pin
    (`test_e2e_result_struct_wrapper_moved_to_callee_and_its_controls`) caught
    the over-fire immediately. Needed a new per-ARM flag,
    `pattern_binding_arm_only_borrows`; every neighbouring flag is per-match,
    but this is a property of the arm body.
  * the scrutinee must not be a FRESH OWNING TEMP (`match mk() { .. }`). That
    temp keeps its own payload cleanup, so the shape was already clean and a
    drop here is a second owner -- caught by the same pin's `inline_use` control.
  * the scrutinee must not be an OWNED PARAM, which is the case the surrounding
    comment's use-after-free warning is about: a non-copy-supported payload
    leaves an owned param on caller-retains, so freeing the binding there would
    turn a status-quo leak into a UAF on `sink(e); use(e)`.

VERIFIED at KARAC_OPT_LEVEL=0 under valgrind over 200 iterations: the row's
repro and every shape in its matrix are clean -- `Map` and `Set` fields,
`Option` and `Result` wrappers -- and the controls that were already clean stay
clean (bare `Result[Map,E]` with no struct, the struct with no enum wrapper, the
inline match, and the let-bound-unused form). `#[ignore]` lifted on
`asan_let_bound_result_map_field_leaks_on_match`. |
| B-2026-08-12-3 | other | medium | `tests/codegen.rs`'s E2E harness ran `resolve` and `typecheck` only to feed `lower` and DISCARDED their errors, so the suite stayed green on 40 progr… | FIXED by e0c6bab. `common::assert_check_clean` panics when an E2E test program has a resolve or typecheck error, with the 40 measured offenders grandfathered by name and diagnostic in `CHECK_GATE_GRANDFATHERED`. Verified by negative control; suite green at 2905/0. |
| B-2026-08-12-4 | codegen | high | `asan_vec_element_field_move_by_assignment_no_double_free` is red on main with a LeakSanitizer leak, and NONDETERMINISTICALLY so: green in the full p… | FIXED by 3e8d8fa9 -- and the row's nondeterminism has a mundane explanation that
is worth more than the fix: the leak is DETERMINISTIC, and LeakSanitizer was the
unreliable part.

WHAT IT ACTUALLY IS. `cur = stats[j].region` cap-zeroes the source so the
element's owner stops freeing it (B-2026-08-11-25's fix) -- which leaves whatever
`cur` held BEFORE with no owner at all. `trigger_eager_free` (src/codegen/stmts.rs,
the Assign arm) frees the target's displaced buffer and classifies every other
transferring RHS -- a moved alias, a fresh ref, the `mk().s` fresh-temp staging, a
bare `v[i]` -- but had NO arm for a field moved out of a deeper place. A
`FieldAccess` matches none of them, so the displaced buffer was orphaned once per
execution of the assignment.

So the row's reading was right in substance: the assignment-path cap-zeroing did
trade the double free for a leak, in exactly the mode the fixture's comment
predicts. What it could not see is that the leak arrives through the DISPLACED
TARGET rather than the moved source, which is why it is invisible to the value
pins and why the repair is an added free rather than a narrowed suppression.

WHY IT LOOKED NONDETERMINISTIC. The fixture's max-by-revenue loop overwrites
`best_region` only when the running max improves -- twice across its 40 elements
-- so exactly ONE buffer is displaced per run. One leaked pointer left behind in a
stale stack slot reads to LSan as still-reachable, and whether that slot survives
depends on what ran before it in the process. Under valgrind, which reports
`definitely lost` from the same tree on every run, it is 8 bytes in 1 block,
every time. That also explains the direction of the flake the row found puzzling
(green in the full parallel suite, red standalone): it is not scheduling, it is
stack residue.

It did not reproduce for me at all -- 15 standalone runs green, the full suite
green, KARAC_PAR_WORKERS 1/2/4/8/16 all green, and green on the row's own tree
(9789448) at three runs. `cannot reproduce` would have been the wrong conclusion:
valgrind found it immediately on the first try.

BISECTED to the leg rather than guessed: of the fixture's three assignments, the
`out = cat[0].region` and `got = vs[0].xs` legs are clean and only the loop leaks,
and the leak scales exactly with the overwrite count (6 iterations -> 5 blocks, 11
-> 10). Two controls stay clean and bound the shape: `cur = <fresh String>` and
`cur = s.region` where `s` is a plain local. It is the deeper-place branch only.

THE ALIAS GUARD, which is the part worth reading. The naive fix -- add the arm --
is WORSE than the bug. This arm is the only one whose RHS can alias the slot it
overwrites: after the first `cur = box[0].s` the source is cap-zeroed INTO `cur`,
so a second execution of the same assignment reads a place that now aliases `cur`
itself, and freeing "the old value" frees the buffer about to be stored back.
Measured with the arm unguarded: correct content on the first read, garbage on
the second, two invalid reads under valgrind. `zero_vec_slot_header_if_aliases`
compares the two pointers and zeroes len/cap when they match, so the cap-gated
free and the len-gated element walks all no-op -- one icmp and two selects on a
path that already loads the header.

PINS, and why the existing one could not do this job. The sibling ASAN fixture
leaks one block, which is what LSan misses -- keeping it and adding a line would
have re-created the same blind spot.
`asan_place_field_move_assign_overwrite_no_leak` overwrites 200 times, past any
reachability accident: 3088 bytes in 193 allocations pre-fix, clean post-fix.
`test_e2e_repeated_place_field_move_assign_reads_correctly` pins the guard with a
CONTENT read, because `.len()` comes from the header and reads correctly off a
dangling pointer -- a length-only pin would have seen nothing.

Corrected while here, as the row asked: the sibling fixture's stale
B-2026-08-11-33 cross-reference (an id collision from 2026-08-11) now reads -25,
and its "the loop is here to make such a leak accumulate" comment now says
plainly that it does not, and points at the fixture that does.

REMAINDER, split out as B-2026-08-12-13 rather than buried here: assigning twice
from the same already-moved place still leaks that one buffer, because both
source and target end up cap-zeroed and neither frees it. That is unchanged from
before this fix (it leaked the same 8 bytes then), and strictly better than the
pre-B-25 double free -- but it is a live defect, and `karac check` accepts the
program. |
| B-2026-08-12-5 | codegen | high | SILENT WRONG ANSWER (run-vs-build): `#[derive(Eq)]` equality over a struct with a `Vec[String]` field reports NOT EQUAL in both compiled backends for… | a827a7f (route the `==` operator for a Vec-carrying struct to the existing TYPE-directed `emit_eq_fn_for_struct` via a new `try_compile_struct_eq_typed` in src/codegen/expr_ops.rs, gated by `struct_has_vec_field_deep`; the shape-directed field walk stays for every other struct) |
| B-2026-08-12-6 | other | medium | 103 of the 109 codegen-invoking test harnesses in `tests/` resolved the RAW parse tree, skipping some or all of the three AST rewrites `karac build`… | FIXED by b3a1061. `karac::prepare_for_resolve` owns the parse->resolve rewrite sequence; `cli.rs` and all 109 codegen-invoking test harnesses call it, so no driver can run a subset. Pinned by `desugar_dependent_constructs_reach_codegen_through_the_harness` (multi-assign, trait default method, `impl Trait` param), verified by negative control. |
| B-2026-08-12-1 | codegen | high | Passing a by-value `Option`/`Result` argument TWICE makes every call after the first read the WRONG VARIANT -- `Err`/`None` for a value that is still… | FIXED by c24343b.

The callee now ENTRY-COPIES a by-value `Option`/`Result` param, so the caller
keeps its own value and nothing is zeroed: every pass reads the true variant.
Four parts, all consulting one predicate so the frames cannot drift.

  * `optres_param_entry_copied_te` (param_own.rs) -- the single predicate. It
    delegates to `field_copy_supported`'s `Option`/`Result` arms (which already
    vet BOTH halves of a `Result`; the drop frees whichever is live, so a copy
    that skipped the `Err` half would double-free every error path while every
    `Ok`-only test passed), and then EXCLUDES shared and boxed payloads. As a
    struct FIELD those two are legitimately copyable -- an rc-INC, a fresh
    envelope -- but at a param boundary they already have owners (the rc
    machinery; `boxed_enum_payload_vars` / `boxed_struct_payload_vars`) with
    their own caller-side retraction rules, and an entry copy is a second,
    unsynchronised answer to the same question. Measured: admitting them fails
    20 memory_sanitizer fixtures across the shared-Option, boxed-Option and
    boxed-enum-chain families.
  * `deep_copy_optres_param_in_place` (param_own.rs) -- dispatches exactly as
    `deep_copy_one_aggregate_field` does. No new copy emitters were needed.
  * Caller side (call_dispatch.rs AND method_call.rs) -- both sites that emit
    the arg-site whole-slot zero now skip it on the same predicate, and both
    register a cleanup for a FRESH-TEMP argument via `track_optres_arg_temp`.
    Gating only the free-fn site left a method's caller zeroing a slot the
    callee no longer took over.
  * REGISTER BEFORE COPY (functions.rs) -- the ordering is load-bearing, and
    finding out why is what unblocked this row after the first attempt failed.

THE ORDERING BUG, which is the whole story of the failed first attempt. Both
inline-payload trackers zero-init the slot in the entry block when they believe
they are registering from a NESTED scope, so a `let` whose store may never
execute (loop body skipped, branch not taken) cannot leave the cleanup arm
reading `undef` as a tag. Their test for "nested" is `insert block != entry
block`. Emitting the entry copy FIRST splits the entry block and leaves the
builder in the copy's merge block, so the trackers concluded they were nested
and planted a whole-slot `zeroinitializer` before entry's terminator -- ahead of
both the copy's own payload reads and the body's first read of the param. Every
matching callee then returned the `Err`/`None` arm on the FIRST call.

A param slot is never the shape that defensive zero is for: it is initialised
unconditionally by the incoming argument store. Registering while the builder is
still at entry says exactly that, and needs no new flag or gate.

THE FRESH-TEMP HALF. Once the callee copies, a temp argument has no owner at
all -- the callee frees its copy and the original leaks, which is
B-2026-08-11-30's leak reappearing through the other door. `track_optres_arg_temp`
spills the value to an entry alloca and registers the same inline-payload
cleanups a `let` of it would get, so the free carries the drop machinery's own
`cap > 0` / tag guards and an `Err`/`None` temp is a no-op rather than a wild
free. Identifier arguments are excluded: their let-site registration already
owns the value and now fires, so owning one here would be the double free the
zero used to prevent.

VERIFIED. Values correct on AOT -O0, AOT -O2, JIT and interpreter for `Result`
and `Option`, with a scalar half and with a user-enum half. The full
B-2026-08-11-30 -O0 valgrind matrix stays clean in both directions: all seven
previously-leaking shapes at zero, and all fourteen controls (arg-match,
let-match, discard, inline-match, bare-struct, bare-Vec, user-enum, let-unused,
double-pass and double-pass-with-match) clean. B-2026-08-11-30's own repros stay
green, so the leak fix is preserved rather than traded away.

The row's `#[ignore]` is lifted: `e2e_repeated_by_value_optres_arg_reads_wrong_variant`
(tests/codegen.rs) is now a passing regression pin.

REMAINING SCOPE, recorded rather than papered over. `take(Some(x))` where `x` is
a live local is excluded from the temp registration by the freshness gate above,
so that spelling keeps B-2026-08-11-30's leak. Excluding it is what stops the
self-host double free, and a leak is the safe direction; closing it needs the
caller to know whether the ctor's payload move already retracted `x`'s own
cleanup, which is a separate question from this row's. |
| B-2026-08-12-7 | typecheck | medium | A union or `#[derive(Copy)]` struct with a RAW POINTER field is rejected as not-`Copy` by the one rule whose own suggestion is "hold it behind a raw… | FIXED by 9410488b -- added a `Type::Pointer { .. } => true` arm to `is_type_copy` (src/typechecker/derives.rs). Pinned by `test_e2e_union_and_struct_raw_pointer_fields_are_copy` in tests/codegen.rs, which covers both pointer spellings in a union AND a `#[derive(Copy, Clone)]` struct holding a `*mut u8`; verified to fail pre-fix with the E_UNION_FIELD_NOT_COPY pair and pass post-fix. |
| B-2026-08-12-8 | typecheck | medium | Four methods that CODEGEN FULLY IMPLEMENTS are rejected by the typechecker, so `karac build` refuses programs the backend demonstrably compiles and r… | 9f4fc14 (three registrations — Set.clear in stdlib_map.rs plus a MISSING interpreter arm in method_call_set.rs, range at both reduce dispatch sites in expr_method_call.rs, iter_mut in expr_method_call.rs yielding `mut ref T` — and one test correction, since `.collect()` on a bare Vec is a deliberate rejection rather than a gap; all four removed from CHECK_GATE_GRANDFATHERED) |
| B-2026-08-12-9 | typecheck+codegen | low | The `f64` sort/max/min emitters are UNREACHABLE from any valid program: the F64 total-order rule rejects `Vec[f64].sorted()` / `.max()` / `.min()` at… | 2768ad1 (premise refuted, not a design call: `Body::FloatScalar` in emit_cmp_fn_for_type_expr goes from the ORDERED IEEE predicates to the total-order key, with NaN canonicalized first; the grandfathered test's float leg is rewritten through a generic and removed from CHECK_GATE_GRANDFATHERED) |
| B-2026-08-12-10 | typecheck | low | Implicit narrowing to a refinement type works for an INTEGER literal but not for a FLOAT or STRING literal, and the diagnostic then states something… | FIXED by 4ecd716.

Both halves, and the float half turned out to be three missing pieces rather
than one.

PROBLEM 1 -- float narrowing. `eval_const_expr_with_chain` (typechecker/items.rs)
had arms for `Integer`, `Bool`, `CharLit` and `ByteLit` but none for `Float`, so
a float literal fell to `NonConstShape` and lost an elision that was always
decidable. Adding the arm alone was not enough:

  * `apply_comparison` (typechecker/const_eval.rs) had no `F32`/`F64` pair, so
    the folded value could not be compared against the predicate's literal. Uses
    `partial_cmp` and reports a NaN operand as INCOMPARABLE rather than silently
    answering `false`, which keeps a NaN-bearing predicate on the runtime path
    instead of admitting or rejecting it on a made-up answer.
  * `apply_unary`'s `Neg` arm covered only integers, so a NEGATIVE float literal
    still did not fold. That was the row's sharpest symptom once found: `-3`
    against the `i64` twin already reported the accurate
    E_REFINEMENT_PREDICATE_VIOLATION while `-2.5` against the `f64` one reported
    "not a compile-time constant".
  * `infer_operand_target_ty` named a type only for SUFFIXED integer literals,
    so `0.5f32 < 1.0` folded `F32` against `F64` and read as incomparable. It
    now does the same for suffixed float literals, and reaches through a unary
    minus -- which the integer arm never needed, because a negative integer
    literal arrives already folded.

`f16`/`bf16` are deliberately left out: `ConstValue` has no half-precision
variant, and widening to `f32` would fold predicates at a precision the runtime
does not use. They keep today's behaviour.

The whole float addition is ADDITIVE: the evaluator never produced `F32`/`F64`
before, so no existing reduction changes -- it only stops failing on a shape it
could always have folded.

PROBLEM 2 -- the false message. The rejection used one fixed sentence, "the
value is not a compile-time constant", which is simply untrue of a string
literal. The reason is now chosen from what was actually established, so the
diagnostic never asserts something the user can see is false:

    value reduced, predicate did not     -> "the value is a compile-time constant
                                             but its `where` predicate could not
                                             be evaluated at build time"
    written as a LITERAL, did not reduce -> "the value is a literal, but the
                                             build-time predicate evaluator does
                                             not yet support `String` constants"
    anything else                        -> the original wording, which is
                                             accurate for a genuine runtime value

REMAINDER, not fixed and recorded rather than implied: STRING literals still do
not const-elide. That needs a `ConstValue::Str` variant plus const evaluation of
`self.len()`, and `ConstValue` is shared with const generics, the interpreter and
codegen -- a materially larger change than this row's papercut, and one whose
blast radius is the const-generic parameter surface. The existing boundary test
`refinement_elision_string_literal_needs_explicit_construction` still pins the
rejection; what changed there is only that the message is now true.

PINS: `refinement_elision_float_literal_is_const_elided` (both directions --
admitted values elide, violating ones report PREDICATE_VIOLATION rather than the
narrowing rejection, which is what shows the value was folded rather than waved
through; covers `f64`, suffixed `f32`, and the negated literal) and
`refinement_elision_rejection_message_is_not_false` (asserts the string case no
longer claims a literal is not a constant, and that a genuine runtime value
still gets the original accurate wording). |
| B-2026-08-12-11 | codegen | medium | Codegen's TWO type-lowering entry points kept two hand-maintained lists of built-in handle types and DISAGREED: `llvm_type_for_type_expr` answered `p… | FIXED by 9b2f5ad. `builtin_opaque_ptr_handle` is the single table both entry points consult, so the same type cannot lower to `ptr` through one and `i64` through the other. `KARAC_STRICT_TYPE_LOWERING=1` reports any remaining unknown name at the point of the decision, filtering generic parameters (user program + baked stdlib) so it is quiet on `hello world` and fires on 16 of 2906 E2E programs. |
| B-2026-08-12-12 | codegen | low | SIX type names still lower to the silent `i64` default with no LLVM layout of their own, across 16 of the 2906 `tests/codegen.rs` programs: `Unit` (6… | FIXED by 75fbfc0. Three of the six resolved, three measured and left alone.

REAL BUG -- `Expr`. `declare_enums` asks for a shared enum's own LLVM layout
while computing that same enum's drop kind: `Expr.Blk(Block)` where `struct
Block { tail: Option[Expr] }` walks to "is the Option payload boxed?" -> "how
wide is `Expr`?", and at that moment `Expr` is not in `shared_types` yet, so the
answer came from the unknown-name `i64` default. Right only by coincidence -- a
shared handle and an `i64` are both one word -- which is also why NO behavioural
test can distinguish the two, and why the guard had to be the lever rather than
an output assertion. Fixed with a name-only `shared_type_names` set collected
before any layout pass and consulted alongside `shared_types`; being shared is a
name-level property, so a name set built first is sufficient -- the same
argument `build_struct_types` already makes for its own `shared_struct_names`
pre-pass, which exists for the identical forward-reference reason.

LEVER PRECISION -- `E`. Not a type at all: the generic parameter of the baked
`Result[T, E]` declaration, reaching the default on any program that calls a
Result combinator. The generic-name filter scanned the USAGE-GATED stdlib set,
which excludes the prelude-baked declarations; it now scans every baked program.
Confirmed benign first: `and_then` over a `Result[i64, String]` builds and
matches the interpreter on both arms, and `map_err` over a String `E` is refused
by codegen outright, so the `E` width never decided anything.

KNOWN TYPE READING AS UNKNOWN -- `Unit`. Written as a name throughout the baked
stdlib (`-> Unit`, `Result[Unit, IoError]`) with no declaration anywhere. Given
an explicit arm returning the same `i64` the `TypeKind::Tuple(&[])` arm already
produces, so no layout changes -- it stops a KNOWN type reading as an unknown
one, which is what the lever needs to stay honest.

NOT DEFECTS. `ParBlockInfo` / `TaskInfo` are a deliberate v1 placeholder that
`stmts.rs` documents in situ -- "using `i64` as a placeholder element type keeps
Vec dispatch working for the v1 contract surface (`.len()` / `.is_empty()`
ignore element type). Field access is a v1.x follow-up that requires registering
the baked struct types." The audit found the documented deferral, not a bug.

`JsonError`'s width query is likewise benign: it comes from
`nested_boxed_enum_payload_variants` off the `let r = Json.parse(..)` binding,
and nothing materialises the payload by layout because the only operation that
would -- field access -- is refused first. Opaque propagation of a parse error
was verified to match the interpreter on both arms. But that refusal is itself a
real run-vs-build gap, so it is SPLIT OUT rather than buried here.

PINNED by a subprocess CLI test (`strict_type_lowering_is_quiet_on_a_recursive_
shared_enum`) -- the lever is an env var and `cargo test` shares one process, so
an in-process setting would leak into whatever else is compiling concurrently.
Verified non-vacuous: dropping the `shared_type_names` consult alone makes it
fail with ``type name `Expr```.

MEASURED: the corpus audit goes from 16 firing tests over 6 names to 4 over 3,
and the 4 that remain are the two documented placeholders plus the split-out
gap. Full suite green at 13467 passed / 0 failed across 116 targets; clippy and
fmt clean. |
| B-2026-08-12-13 | codegen+ownership | low | Assigning TWICE from the same already-moved-out place (`cur = box[0].s;` … `cur = box[0].s;`) leaks that buffer: the first assignment cap-zeroes the… | FIXED by eef6e980.

Suppressing the free was necessary but not sufficient. B-2026-08-12-4's alias
guard stopped the displaced-value free from reclaiming the buffer about to be
stored back -- without it the second read returns garbage -- but it neutralized
the slot header and then let the store put the SOURCE's header over it, and the
source had been cap-zeroed by the first assignment. So both source and target
ended up carrying `cap == 0` and neither freed the buffer at scope exit: the read
was correct and the ownership was gone.

`keep_aliased_slot_ownership` (src/codegen/runtime.rs) now does both halves --
neutralize the slot so every free below no-ops, AND carry the target's own `cap`
across into the value being stored, so the target stays the single owner it was
before the aliasing assignment. One `icmp` and three `select`s, on a path that
already loads the header. A non-struct incoming value (no `{ptr,len,cap}` to
compare) is returned untouched.

MEASURED on the row's own program, same tree, under valgrind: `definitely lost:
8 bytes in 1 blocks` before, clean after, with the printed content correct in
both. Ten shapes re-checked clean and correct afterwards, including 100 repeated
reads of one element, a `Vec[i64]` field re-read three times, and an interleaved
distinct/repeated sequence through one target.

THE PIN READS TWO ELEMENTS' WORTH, NOT ONE ELEMENT TWICE, and that distinction is
the whole reason this row existed separately. The leak is one buffer per element
that gets RE-READ, so re-reading a single element 100 times still leaks exactly
one block -- and one leaked block can sit in a stale stack slot and read to LSan
as still-reachable, which is precisely how B-2026-08-12-4 stayed invisible under
ASAN while valgrind reported it every run.
`asan_repeated_place_field_move_assign_keeps_one_owner` reads 150 elements twice
each: 2192 bytes in 137 allocations pre-fix, clean post-fix.

`test_e2e_repeated_place_field_move_assign_reads_correctly` gained two legs: a
`Vec[i64]` field re-read three times, where a wrong restored capacity would show
up in the ELEMENTS rather than the header, and an interleaved
distinct/repeated sequence, where a missed free leaks and an unguarded free reads
back garbage -- the two failure modes on either side of this guard.

WHAT THIS DOES NOT DO, deliberately: `karac check` still accepts the program.
Reading a place that has been moved out of is a use-after-move, and the ownership
checker is silent on it -- the row named that as the second of two plausible
fixes and it is the one still open. This commit took the codegen half because
`UseAfterMove` is advisory by design (non-fatal, `.clone()`-suggesting, compiles),
so the backend has to produce something sound for the shape either way; a
diagnostic would warn but would not stop the leak. The diagnostic remains worth
adding on its own merits. |
| B-2026-08-12-14 | codegen | medium | Reading a field off a `Json.parse` error is a RUN-VS-BUILD split: `karac run --interp` prints `e.line` / `e.column` / `e.message` correctly, while `k… | FIXED by 8268903. `seed_builtin_struct_types` now seeds `JsonError`'s layout,
field names and field type names -- exactly the way `Response` and `HttpError`,
the two other baked stdlib structs whose values codegen hand-rolls, are already
seeded. With a layout to GEP, the Err binding's fields resolve and the compiled
binary matches the interpreter.

THE INTEGER FIELDS ARE SEEDED `i64`, NOT THE DECLARED `u32`, and that is
load-bearing rather than sloppy: `json.rs`'s `Build Result.Err(JsonError { line,
column, message })` packs `line` / `column` into the widened Result's `w0` /
`w1` as FULL WORDS (`zext` from the runtime's u32). The seeded layout has to
describe what is actually built, not what the source declares -- seeding `i32`
would put the field GEPs half a word out of step with the packing. The same
as-built-not-as-declared rule the `Response` seed already follows for its hidden
`headers` field.

VERIFIED against the `--interp` oracle across the three positions that resolve a
field differently, all byte-identical: a bare read (`println(e.line)`), an
f-string interpolation (`f"col={e.column}"`), and the whole struct passed BY
VALUE to a fn that reads two fields (`describe(e)`). The Ok arm and a second
successful parse in the same program are unaffected.

PINNED by `e2e_json_parse_error_fields_are_readable`, which covers all three
positions plus the `message` String -- exercised by LENGTH rather than content,
so the pin does not encode serde_json's wording and will not churn on a crate
bump. Verified non-vacuous: disabling the seed alone makes it fail with the
original `cannot resolve field 'line' ... this is a compiler gap`.

THE MESSAGE LEAK IS SPLIT OUT, NOT FIXED, and is now measured rather than
assumed: `json.rs` pins the message String's `cap` to 0 so the scope-exit free
is a no-op, and LeakSanitizer reports 39 bytes in 1 allocation on a parse-error
program. This commit is the PRECONDITION for fixing it -- a synthesized drop now
exists to fire, which is why the cap could not have been wired before -- but the
Err payload is passed by value, destructured and `?`-propagated, and each is a
place a real cap could turn one owner into two. That needs its own ASAN matrix.
Filed as B-2026-08-12-16. It is also why this commit adds no ASAN fixture for
the shape: one would be RED on the Linux `memory-sanitizer` job today.

Suite green at 13471 passed / 0 failed across 116 targets; clippy and fmt clean. |
| B-2026-08-12-15 | codegen | high | A boxed `Option` FIELD envelope inside an inline STRUCT enum payload (`Result[W, i64]` over `struct W { o: Option[Option[i64]] }`) has no owner in AN… | FIXED by c0320d7. The quarantine entry added one commit earlier is removed and
the -O0 known-failures list is EMPTY again; the fixture is clean under valgrind
at -O0 and returns 20440.

THE ROW'S CAUSE WAS WRONG, and that is why four fixes failed before this one.
It blamed `c24343b3` and the by-value call, so all four attempts were aimed at
the argument-passing machinery. One probe refutes both:

    let r: Result[W, i64] = Result.Ok(W { o: Option.Some(Option.Some(n)) });

leaks 32 B per construction with NO CALL ANYWHERE IN THE PROGRAM. Measured at
-O0 with valgrind, 1,280 B in 40 blocks over 40 iterations -- byte-identical to
the call form the row named. The entry copy had only removed an ACCIDENTAL
owner (the callee's arm, which used to free the CALLER's envelope) from one of
the forms; the gap itself is in the type walk and predates c24343b3.

WHY NEITHER EXISTING PREDICATE COULD NAME THE BOX. `W` is 4 LLVM words and fits
`Result`'s 5-word area, so nothing boxes at the outer level and
`boxed_enum_payload_variants` reports nothing. The inline payload is a STRUCT
rather than an `Option`/`Result`, and `nested_boxed_enum_payload_variants`
reaches its inner walk only through `boxed_enum_payload_variants(arg)` -- which
matches on the enum NAME -- so it reports nothing either. No let-site
registration had ever covered the shape.

THE FIX IS AN ENUMERATION, NOT A MECHANISM. `struct_payload_boxed_field_variants`
is the third sibling of those two: for an inline user-struct payload it walks
the struct's fields and returns each boxing `Option`/`Result` field's
coordinates. It needed no new cleanup action -- `NestedBoxedEnumDrop` was
already parameterized on `inner_tag_field`, and a struct payload only shifts
that index by the words of the fields ahead of the box
(`coerce_to_payload_words` flattens word-by-word in LLVM field order). Three
guards keep the offset arithmetic honest rather than assumed: a `shared struct`
payload (a 1-word RC pointer, never flattened), a GENERIC struct (whose declared
field types measure ERASED), and a per-field word sum that must equal the
struct's own LLVM width.

THREE OWNING SITES, and a fix at any one alone is wrong at the other two.
Each was measured into existence by the failure of the previous state:

  1. THE LET SITE (`stmts.rs`) -- covers every form that binds the value.
     Alone, it turns the leak into a glibc `double free detected in tcache 2`
     on `let r = ...; match r { Ok(w) => ... }`, because the arm's
     `__karac_drop_struct_W` already frees the box.
  2. THE CONSUMING ARM (`suppress_struct_field_boxed_payload_arm_bind`) --
     hands the box from the scrutinee to the arm by zeroing the box WORD in the
     scrutinee's slot. Gated on `pattern_consumes_field`, because `Ok(_)`
     registers no `StructDrop` and zeroing there would orphan the box. Zeroes
     the word rather than the tag for the reason
     `suppress_nested_boxed_drop_for_var` gives: `Result`'s `Ok` is tag 0, so a
     tag-zero leaves the outer guard passing.
  3. THE FRESH-TEMP ARGUMENT SPILL (`track_optres_arg_temp`) -- the one form
     with no binding to hang a drop on.

Plus one RETRACTION EXCEPTION: the arg-site `suppress_nested_boxed_drop_for_var`
must NOT fire for this population, because the callee deliberately registers
nothing for it (the `functions.rs` loop stays narrow -- widening it there was
measured as a double free on all three of the temp-argument, bound-argument and
matched-after-call forms, reconfirming the row's rejected attempt 3). Tracked
by `struct_field_boxed_payload_vars`, which plays exactly the role
`boxed_struct_payload_vars` plays for the direct-box family one line up in the
same loop, and for the same asymmetry: a STRUCT payload has a callee-side
move-out mirror and a bare enum payload does not.

This is also what makes the row's rejected attempt 1 (gate that retraction on
`callee_entry_copies`) legible in hindsight: it was a byte-identical no-op
because the caller had no registration to retract. It is load-bearing only once
the let site arms one.

THE TEMP SPILL NEEDED ITS OWN FRESHNESS PREDICATE, and this is the subtlest
part. `optres_arg_is_unowned_temp` guards a `{ptr,len,cap}` PAYLOAD buffer,
which a ctor can wrap without minting (`Ok(x)` hands us `x`'s buffer) -- hence
its narrowness, paid for by a self-host double free (the row's rejected attempt
2). An ENVELOPE is different: it is minted by the construction itself and can
never be named by the source program, so the only way an argument carries a
LIVE one is by reading it out of a place that already owns it.
`optres_arg_mints_field_envelope` therefore asks PER FIELD, and only of the
fields that actually box, whether the initializer is a construction. An
expression-kind safe-list is the wrong shape here: the measured argument is
`cls(Result.Ok(W { o: Option.Some(Option.Some(n + i)) }))`, whose `n` and `i`
are IDENTIFIERS -- refused by any place-based rule, and unable to carry a box.

MEASURED, per shape, baseline vs fixed, valgrind at -O0 over 40 iterations:

  the fixture                       2,560 B / 80  ->  CLEAN
  bare let, NO CALL in the program  1,280 B / 40  ->  CLEAN
  a leading scalar before the box   1,280 B / 40  ->  CLEAN
  `takw(r); takw(r)` twice over     1,280 B / 40  ->  CLEAN
  passthrough `takw(idw(b))`        1,280 B / 40  ->  CLEAN
  String interior sibling           1,440 B       ->    160 B
  ctor wrapping a LIVE binding      1,280 B / 40  ->  unchanged
  `takw(mkw(n))`                    1,280 B / 40  ->  unchanged

No shape regressed and none became corruption. The three that did not reach
zero are live remainders with their own causes and are filed separately rather
than buried here. |
| B-2026-08-12-16 | codegen | low | `Json.parse`'s error message LEAKS: codegen copies the runtime's diagnostic into a Kara String but pins that String's `cap` to 0, so the scope-exit f… | FIXED by ec58cb0. The message no longer leaks: LeakSanitizer reports a
clean run on the four-position matrix that was RED at 400 allocations.

THE ROW'S FIX SHAPE WAS HALF THE FIX, and the missing half is the more
interesting one. Wiring the real `cap` into w4 (`msg_len_word`, which is exact
in all three incoming paths -- the alloc path mallocs precisely `msg_len_i64`
bytes, and the null and empty paths both arrive with ptr == 0 AND len == 0, so
the guarded free stays a no-op there) took the matrix from 400 leaked
allocations to 200. It did NOT fix the two commonest shapes.

MEASURED, per position, after the cap alone:
  1. bare arm binding      `Err(e) => n + e.line`          -- STILL LEAKING
  2. passed BY VALUE       `Err(e) => describe(e)`         -- clean
  3. field read then value `let p = e.message.len(); ...`  -- clean
  4. `?`-propagated        across a frame boundary         -- STILL LEAKING
The two that passed did so for a reason that has nothing to do with this row:
the CALLEE's own param drop owned the payload. Nothing in the caller ever did.

ROOT CAUSE OF THE REMAINDER: `bind_pattern_values`'s
`is_inline_optres_struct_payload` gate -- the B-2026-07-10-3 machinery that
gives a `Some(e)`/`Err(e)` struct payload its scope-exit `StructDrop` -- was
returning false for `JsonError`, so the arm binding got NO drop registered at
all. Both of its admission tests (`aggregate_param_copy_supported_struct` and
`struct_heap_copyable_or_handle`) read `struct_field_type_exprs`, and both bail
to `false` the moment that map has no entry for the name. B-2026-08-12-14
seeded `struct_types`, `struct_field_names` and `struct_field_type_names` --
enough to make the field READABLE, which is all that row was about -- but not
the TypeExprs the ownership gates consult. So there was a `cap > 0`-guarded
drop synthesized and no binding registered to fire it.

Seeding `struct_field_type_exprs` for `JsonError` (`i64`, `i64`, `String`,
matching the as-built layout choice the sibling row documents) closes it. The
`String` entry is the load-bearing one. The seed sits inside the same
`!struct_types.contains_key` guard as the rest and runs before
`declare_structs`, which unconditionally overwrites all three maps -- so a user
program declaring its own `struct JsonError` still overrides it, exactly as it
already overrides `Response`.

DIRECTION THE FIX OPENS, and why the matrix is four positions rather than a
smoke test: before this, every read of `e.message` was trivially safe because
the buffer was never freed at all. Now it is genuinely freed, so each position
where the payload could acquire a second owner is a potential double-free or
use-after-free rather than a leak. ASAN catches that direction as loudly as
LSan catches the leak, and all four are green -- including the by-value and
`?`-propagation legs, which are the two that cross a frame boundary.

PINNED by two tests. `asan_json_parse_error_message_freed_once`
(tests/memory_sanitizer.rs) is the memory gate: the four positions in a
100-iteration loop, verified non-vacuous in stages -- 400 leaked allocations
before any change, 200 with the cap alone, 0 with both halves.
`e2e_json_parse_error_fields_are_readable` (tests/codegen.rs) gains the
correctness half: `describe` now reports the message's LENGTH and the caller
compares it against the length it read before handing the struct over, so a
mis-scoped free shows up as wrong output and not only as a sanitizer trip.
Still length rather than content, so neither pin encodes serde_json's wording.

VERIFIED byte-identical across all three backends (`--interp`, LLJIT, AOT) on a
program that prints the message, both integer fields, and the message through a
by-value callee, plus a `?`-propagated error and a successful parse in the same
run. Suite green at 116 targets, 0 failures; fmt and clippy clean.

NOTE for the reader coming from B-2026-08-12-14: that row's closing paragraph
says registering the field "changes no free behaviour; it only makes the field
readable". That was accurate for what it shipped -- the cap was still pinned to
0 -- but the registration it added was also the precondition for this fix, and
the comment it left at the seed site has been rewritten accordingly. |
| B-2026-08-12-17 | codegen | medium | A boxed `Option` FIELD envelope inside an inline STRUCT enum payload still leaks 32 B per call when the by-value ARGUMENT is not a fresh construction… | FIXED by 5015d5d. Both shapes are clean at -O0 under valgrind and under
LeakSanitizer; `asan_struct_payload_boxed_field_envelope_owned_in_arg_position`
pins them (3,840 B in 120 allocations against the pre-fix compiler).

ONE ROOT CAUSE, NOT TWO, and the row filed it as two. The bound spellings are
what collapse them together -- each leaking form has a `let`-bound twin that
was already clean:

    let r = Result.Ok(w);   clean        takw(Result.Ok(w));   1,280 B / 40
    let m = mkw(n);         clean        takw(mkw(n));         1,280 B / 40

So neither the ctor move nor the callee's escaping return is at fault, and the
row's two proposed fix sketches -- a move-suppression peer for the first, a
discard-position owner for the second -- were both aimed one step off. Both are
handled the moment a binding exists to own the result. What was missing is the
ARGUMENT-POSITION owner, and one predicate refused both.

WHAT CHANGED. `optres_arg_mints_field_envelope` admitted only constructions,
because a place read can alias an envelope its owner still frees. It now admits
two more shapes, each with a discriminator rather than a loosening:

  * A plain call to a known function, when `nested_boxed_owner_source_of`
    reports no live owner. That resolver already walks passthrough chains and
    the alias map to a fixpoint, and it is exactly the question being asked --
    `takw(idw(mkw(n)))` forwards a TEMP (nobody owns it, admit) while
    `takw(idw(b))` forwards a BINDING (its let site owns it, refuse). Both are
    in the fixture, because the second is the shape a careless widening turns
    into a double free.
  * An identifier NESTED INSIDE a construction, when it is not itself an armed
    envelope owner (checked against all three registration families). Split
    into its own entry point, `envelope_operand_is_unowned`, because the
    question genuinely differs by depth: at top level `take(r)` names the enum
    value and its let site owns the envelope, while one level in `Ok(w)` names
    the STRUCT being moved into a fresh envelope and the move disarms it.

MEASURED, baseline vs fixed, valgrind at -O0 over 40 iterations:

  takw(Result.Ok(w)) for a live w    1,280 B / 40  ->  CLEAN
  takw(mkw(n))                       1,280 B / 40  ->  CLEAN
  takw(idw(mkw(n))) chained          1,280 B / 40  ->  CLEAN
  takw(idw(b)) for a bound b            CLEAN      ->  CLEAN   (control)
  let r = Result.Ok(w); match r         CLEAN      ->  CLEAN   (control)

VERIFIED AGAINST THE CHECK THIS ROW WARNED ABOUT. The row records that widening
the sibling PAYLOAD predicate aborted the self-host parser with `double free
detected in tcache 2` while the whole -O0 valgrind matrix and 1044
memory_sanitizer fixtures stayed green -- i.e. the local evidence was not
sufficient then and is not sufficient now. All 8 self-host oracle tests pass
(lexer, parser, parser-items, parser-types, resolver, resolver-program,
typechecker, codegen-vs-seed), and drop-fuzz reports 0 memory-safety findings
and 0 invariant violations over 200 generated programs / 558 valid executions /
2,271 scheduled drops. Full suite 13,476 passed / 0 failed; -O0 leg green with
an empty quarantine list.

NOT COVERED: the String INTERIOR of a temp-argument envelope, which is
B-2026-08-12-18 and unaffected by this -- the probe carrying it still reports
its 160 B, unchanged. |
| B-2026-08-12-18 | codegen | low | The INTERIOR of a boxed `Option` field envelope owned by the fresh-temp argument spill has no owner: `cls(Result.Ok(S { o: Option.Some(Option.Some(f"… | FIXED by 1e3aef1. `NestedBoxedEnumDrop` gained an optional interior
free, wired for the STRUCT-FIELD population only. Verified across a
six-position matrix: 640 B in 160 allocations before, 0 after, at BOTH opt
levels.

THE ROW'S SCOPE WAS TOO NARROW, and that is the substantive correction. It
placed the leak at the fresh-temp argument, reasoning that "the argument is a
fresh TEMP, so there is no arm and no binding anywhere in the caller". The
mechanism is right; the extent is not. Measured per position at the default
-O2, against the pre-fix compiler:

  let-bound, NEVER MATCHED (no call in the program) -- LEAKED
  let-bound, `Result.Ok(_)` wildcard arm            -- LEAKED
  let-bound, passed BY VALUE to a callee            -- LEAKED
  fresh temp argument (the filed shape)             -- LEAKED
  let-bound, arm binds the STRUCT (`Ok(s) => 1`)    -- clean
  let-bound, arm binds the String out               -- clean

So the rule is not "a temp has no arm" but "nothing bound anything". A
let-bound value with a wildcard arm, and one never matched at all, are the same
hole reached without a call. The row is also filed as an -O0/valgrind finding;
it reproduces at -O2, because the interior IS heap and does not fold the way
the all-scalar envelopes of the parent row do.

THE INTERIOR IS ALLOCATED EXACTLY ONCE in every position -- all six spellings
report 48 allocations for 40 iterations -- so nothing deep-copies it and
exactly one owner is correct. That measurement is what ruled out the
alternative fix of giving the callee one.

FIX. `CleanupAction::NestedBoxedEnumDrop` takes an `inner_payload_free`
descriptor, `Some` only when the box holds an `Option` whose payload is a
direct `{ptr,len,cap}` heap value and the envelope chain is EMPTY (so this box
is the innermost). The emit reuses `emit_free_inline_payload_overlay` verbatim
with the BOX as the slot -- the box holds a flattened `{tag, ptr, len, cap}`,
i.e. exactly `Option`'s own LLVM shape -- so the `cap > 0` guard and the
recursive element walk come along unchanged.

NO RETRACTION WAS NEEDED, which is what makes this contained. The row proposed
mirroring `retract_boxed_tuple_inner_drop_for_arm`'s conditional downgrade. It
turns out the existing handoff already does it: an arm that binds the struct
out hands the box over by ZEROING the box word in the scrutinee's slot
(`suppress_struct_field_boxed_payload_arm_bind`), so this action's null guard
skips the whole free -- interior included -- and `__karac_drop_struct_S` does
all of it. The two clean positions above are in the fixture as the DOUBLE-FREE
guard for exactly that, and stay green.

RESTRICTED TO THE STRUCT-FIELD POPULATION, and the restriction is load-bearing
rather than caution. The sibling population -- where the inline payload IS the
`Option`/`Result`, `Result[Option[Option[String]], i64]` -- passes `None`,
because there an arm CAN name the interior directly (`Ok(Some(Some(t)))`) and
owns it. `stmts.rs` records that an interior drop was implemented for that
population and measured WRONG: a glibc double free at both opt levels. The
struct-field case is not subject to it because reaching that interior from a
pattern means binding the struct, which triggers the handoff above.

PINNED by `asan_struct_field_boxed_interior_owned_without_arm`, all six
positions in one 40-iteration program, non-vacuous at both levels (640 B / 160
allocations pre-fix, 0 post-fix, identical at -O0 and -O2). It carries a
`String` interior deliberately: an all-scalar envelope folds away at -O2 and a
clean run over an allocation that never happened proves nothing, which is the
trap the parent row's fixture documents hitting twice.

ONE POSITION REMAINS AND IS SPLIT OUT AS B-2026-08-12-19, not folded in: a
fresh-temp argument whose CALLEE's arm binds nothing leaks the callee's ENTRY
COPY -- box and interior both malloc'd in the callee. Caller-side is clean, so
it only shows at -O0. Bracketed by measurement rather than asserted: pre-fix
-O0 is 1,551 B / 120 allocations, post-fix 1,391 B / 80, and the 160 B / 40
difference is exactly the caller-side Strings this row closed. Its fixture is
separate so the -O0 quarantine does not blanket the six positions that are
fixed.

Suite green at 116 targets, 0 failures; the -O0 leg matches the quarantine list
exactly; clippy and fmt clean. |
| B-2026-08-12-19 | codegen | low | The CALLEE's ENTRY COPY of a `Result[S, i64]` whose struct payload has a boxed `Option` field leaks WHOLE -- box and interior both -- when the callee… | FIXED by 44eaf8e. The struct-FIELD boxed population is now registered
CALLEE-side, and the `-O0` quarantine list this row was added to is empty
again — the ratchet's "a listed fixture that starts PASSING fails the leg too"
direction is what flagged the line, on the same day it went on.

THE OWNERSHIP AXIS IS THE CALLEE'S BODY, not the argument form the parent row
was filed against. Measured per callee at -O0, 40 calls each, before the fix:

  `Result.Ok(_)`, binds nothing              -- LEAKED 1,391 B / 80 allocations
  never matches the param at all             -- LEAKED 1,391 B / 80 allocations
  `Result.Ok(s) => 2`, binds the struct      -- clean
  binds the struct and reads the String      -- clean

So `functions.rs`'s stated reason for declining this population -- "that box
already HAS a callee-side owner: the arm that binds `W` out runs
`__karac_drop_struct_W`" -- is true of an arm that BINDS and false of every
other body.

THE COUNT IS WHAT MAKES THE FIX SAFE, and it is the measurement the row asked
for. The same program reports 168 allocations for 40 iterations -- FOUR per
call, two boxes and two Strings. The entry copy deep-copies through the box, so
the callee's copy is a genuinely separate allocation from the caller's value:
freeing it here cannot reach the caller's, and the caller-owns contract the
argument site enforces (the `struct_field_boxed_payload_vars` exception to
`suppress_nested_boxed_drop_for_var`) is untouched.

FIX. The param registers the same `struct_payload_boxed_field_variants`
population the let site does, gated on `optres_param_entry_copied_te` -- the
predicate the entry copy itself is emitted under. Without a copy the slot holds
the CALLER's box and freeing it would be a double free, so the gate asks
whether the copy happened rather than inferring it from the shape.

THE DOUBLE FREE THE OLD COMMENT RECORDS IS REAL, and is handled rather than
risked. Registering here was tried before and aborted with a glibc `double free
detected in tcache 2` on the temp-argument, bound-argument and
matched-after-call forms. The missing piece was one line: the param must JOIN
`struct_field_boxed_payload_vars`. That set is what
`suppress_struct_field_boxed_payload_arm_bind` keys on, so joining it lets the
EXISTING arm-bind handoff zero the box word and disarm this registration for
exactly the bodies whose arm already owns the copy. Same disarm the let site
has always relied on, and the same "no new retraction needed" shape as
B-2026-08-12-18 -- both rows turned out to need a registration, not a
retraction.

PINNED by `asan_struct_field_boxed_interior_nonbinding_callee`, rewritten from
the single quarantined shape into a four-callee matrix. The two BINDING callees
are in it as the double-free guard, not as filler: they are the shapes the
earlier attempt aborted on. Non-vacuous, measured at -O0 against the pre-fix
compiler: 2,782 B in 160 allocations, which is the two leaking legs at two
allocations each over 40 iterations. It PASSES at -O2 pre-fix, so the
`scripts/asan-o0-leg.sh` run is the gate and the fixture's doc says so --
at -O2 the copy is dead and LLVM deletes it.

Suite green at 116 targets, 0 failures; the -O0 leg green with an EMPTY
quarantine list; clippy and fmt clean. |
| B-2026-08-12-20 | effect | high | A write to a captured local from inside `par { }` is silently DROPPED when it is routed through a `mut ref` parameter (`par { bump(mut v); … }`) or a… | FIXED by d37c097. `check_captured_local_par_writes` now unions THREE collectors over the par block instead of one: `collect_assigned_roots_block` (unchanged), the existing `collect_mut_method_receiver_roots_block` (the curated `is_mutating_collection_method` set, already used by closure capture-mode inference), and a new par-local `collect_mut_arg_roots_block` that records the place root of every CALL-SITE `mut` MARKER. All three feed the existing diagnostic unchanged — the message was already exactly right, it just never fired.

The `mut` marker is an exact signal here and needs no type information: design.md Feature 4 Part 1½ REQUIRES the marker on precisely the arguments whose place root is a fresh owned binding, which is what a `let mut` local is, while an argument already rooted at an in-scope `mut ref` binding forwards unmarked — and such a binding is not a `let mut` local, so it was never in the flagged set. The new collector is deliberately par-local rather than a widening of `collect_assigned_roots_*`, which `index_disjoint.rs` and the closure-capture analysis also consume.

FALSE-POSITIVE EVIDENCE, the load-bearing part of this fix, since the check now rejects strictly more: the sanctioned escapes still pass (`Atomic` local via `fetch_add`, `par struct` field), branch-LOCAL mutation still passes, read-only capture still passes, and mutation OUTSIDE the par of a binding merely read inside it still passes. Swept the whole corpus for new rejections: 0 of the repo's examples and 0 of the 744 `.kara` files in kara-katas newly rejected.

Three regression tests in tests/effectchecker.rs: the `mut ref` param route, the mutating-method route, and a read-only guard (`len()` + an unmarked argument) pinning that the widening did not start flagging reads. |
| B-2026-08-12-21 | parser | medium | An assignment written as a bare `match` arm body (`Some(q) => total = total + q,`) produced THREE errors, two of them fictional — including a bogus '… | FIXED by 9abcf48. `parse_match_expr` now checks for an assignment operator (`=` or any compound form) after a non-block arm body and, when it finds one, calls the new `recover_assignment_arm_body`: it reports 'assignment is a statement, not an expression, so it cannot be a bare `match` arm body — wrap it in braces: `pattern => { place = value }`' anchored at the operator, then CONSUMES the assignment and builds the arm body the author meant — a unit-typed block holding the assignment statement, i.e. the braced form. A trailing `;` is eaten if present.

Recovering into a well-formed tree (rather than only improving the message) is what removes the cascade: parsing resumes at the next arm, the `match` completes, nothing resynchronizes at top level, and the two fictional errors disappear. One error, at the right token, naming the fix.

NO machine-applicable `fix_diff` is attached: the edit is a brace on each side of the body, and the parser holds tokens but not source text, so it cannot build the single (offset, length, replacement) that the `fix_edits` side-channel takes. Wiring that would mean either two edits per diagnostic or source access in the parser; both are larger than this fix and neither is needed for the message to be correct. |
| B-2026-08-12-22 | codegen | high | DOUBLE FREE on both compiled backends: index-assigning a WHOLE struct element read out of the same Vec (`let b = ps[1]; ps[0] = b;`, element = struct… | FIXED by af43027. The index-assign store now disarms a NAMED struct
source, the AoS peer of the SoA arm that already sat directly below it. The
filed repro, the swap, and every source form now agree with the interpreter on
all three backends.

THE BOUNDARY TABLE WAS WRONG IN THREE ROWS, and the cause is worth recording
because it is a trap any probe of this class can fall into: STRING LITERALS
HAVE `cap` 0. The second free is therefore a guarded no-op, so a shape that
double-frees looks clean whenever the heap field was initialised from a
literal. Re-measured with f-strings, one variant at a time:

  named binding from an element read   `let b = ps[1]; ps[0] = b;`   ABORTS (as filed)
  named binding from a struct LITERAL  `let p = Pair{f"..."}; ps[0] = p;`  ABORTS  (filed: OK)
  named binding from a CALL result     `let c = mk(k); ps[0] = c;`   ABORTS  (not in the filing)
  field-ROOTED container               `h.xs[0] = b;`                ABORTS  (not in the filing)
  heap field is a `Vec`, not a String   `Bag { xs: Vec[i64] }`        ABORTS  (filed as OK for a scalar struct -- true, but a Vec field is not scalar; an EMPTY Vec also reads cap 0 and hides it)
  field-wise rebuild                   `ps[0] = Pair{ word: ps[1].word, .. }`  ABORTS  (filed: OK)
  element-to-element                   `ps[0] = ps[1];`              no double free (filed: not covered)
  `Vec[String]` element copy           `let b = xs[1]; xs[0] = b;`   no double free (as filed)

So the trigger is NOT "the RHS is a whole-element copy read out of the same
Vec". It is: the container's element type is a struct with a LIVE heap field,
and the RHS is a NAMED binding -- whatever that binding was initialised from.
The provenance never mattered; the filing's read-vs-literal distinction was an
artifact of which probes happened to use literals.

MECHANISM, confirmed rather than inferred. The element READ is already
deep-cloned (`clone_owned_vec_index_element`) -- verified directly: after
`let b = ps[1]`, mutating `b.word` leaves `ps[1].word` untouched, identically
on both backends. So `b` owns its own buffer, and the bug is entirely at the
STORE: `compile_index_store` moves `b`'s field pointers into the element slot
while `b`'s `StructDrop` stays armed, and the container's element drain and `b`
then free the same buffer.

WHY THE EXISTING GATE MISSED IT: `target_owns_heap_vec_elem` asks whether the
ELEMENT is a `{ptr,len,cap}`. A struct whose FIELD is one is not, which is
exactly why `Vec[String]` was safe and `Vec[<struct with a String>]` was not --
the one boundary row the filing got right, and the row that localises the gate.

FIX. The AoS peer of the SoA `zero_struct_move_caps` arm immediately below the
same store: resolve the element struct through `vec_index_elem_type_expr`
(which handles the bare-identifier and field-rooted container spellings alike)
and zero the named source's heap-field caps. Skipped for a `ref` param, for
shared structs, and for slice/map targets, which do not own their elements.
No-op for an all-scalar element struct, and idempotent against the SoA arm for
a struct both reach.

PINNED by two tests. `asan_index_assign_named_struct_source_freed_once` carries
the source matrix -- element read, literal, call result, field-rooted
container, `Vec`-typed heap field, and the swap -- and is non-vacuous: against
the pre-fix compiler ASAN reports `attempting double-free`, not merely a leak.
`e2e_index_assign_named_struct_source_swap` is the observable half, checked
against the interpreter oracle. EVERY string in both is an f-string, for the
`cap`-0 reason above; a literal-valued fixture would pass without the fix.

THE SWAP IS SPELLED WITH TWO TEMPS in the fixture, and that is a deferral
rather than a simplification. The terser one-temp swap contains
`qs[0] = qs[1]`, which is clean of the double free but LEAKS one buffer per
assignment -- pre-existing, measured identically on both sides of this fix
(400 B / 40 allocations either way), and filed as B-2026-08-12-26 with a
written, `#[ignore]`d fixture. The two-temp spelling double-freed before this
fix too, so it still gates the shape that matters.

ONE RESIDUAL, SPLIT OUT AS B-2026-08-12-27: a struct LITERAL whose field is
read out of an element's heap field (`let q = Pair { word: ps[0].word, n: 9 };`)
still double-frees, with NO index-assign anywhere in the program. A whole-
element read is cloned; a FIELD read of that element is not. Distinct site,
high severity, filed with the isolating repro.

Suite green at 116 targets, 0 failures; the -O0 leg green with an empty
quarantine list; clippy and fmt clean. |
| B-2026-08-12-23 | ownership | medium | `E_CONCURRENT_PLAIN_STRUCT` fires on BUILTIN containers (`Vec` / `Map` / `Set`) and then prescribes a migration the user cannot perform — 'rename `st… | FIXED by 9d7db97, for the half of this row that was a defect. The row filed two
things; one was real and is fixed, the other's PREMISE IS REFUTED and the
evidence is recorded below rather than acted on.

(1) THE HELP TEXT -- REAL, AND WORSE THAN THE ROW SAYS. A builtin now gets
advice its author can act on. The old text was not merely imprecise: measuring
what an author would actually try next, the two obvious escapes are ALSO
unavailable, so the diagnostic was a dead end rather than a badly-worded
signpost.

    rename `struct Vec` to `par struct Vec`   -- no such declaration exists
    wrap it in `Arc[Vec[i64]]`                -- `undefined type 'Arc'`
    clone per branch: `f(v.clone())`          -- STILL REJECTED

The `Arc` finding matters for anyone extending this: `Arc` sits in this pass's
own cross-task-safe exemption list (`classify_binding_type`), so it reads like
the sanctioned answer, but the surface language does not resolve the type yet.
Offering it would have swapped one impossible instruction for another. The
in-branch clone is the subtler trap -- reading `v` in order to clone it IS the
second-branch use the error is about -- and the new text warns about it by name.

The three routes the new text names were chosen by MEASUREMENT and are pinned
by a test that compiles each one (`test_concurrent_plain_struct_builtin_
suggested_routes_compile`), so the advice cannot rot into a claim again:
hoisting per-branch copies before the `par` block, building disjoint values up
front, or sharing through `Mutex[T]`.

Gated on whether the type has a declaration in THIS program -- the same
condition that already makes `fix_diff` empty for a builtin, which is why the
machine-applicable fix was never wrong here, only the prose that advertised it.
A user struct's diagnostic is byte-identical to before.

(2) THE "INCONSISTENT GATE" -- PREMISE REFUTED, no change made. The row reads
the Vec/Map/Set-vs-String split as "an artifact of what the baked stdlib
happens to register in `struct_info` rather than a decision". It is a decision,
and design.md states it in the same paragraph the row cites for the rule
itself (line 9599):

    "Primitives and other non-struct cross-task-safe values are freely read
     across branches; only non-`par` struct/enum-typed bindings are gated."

The split is exactly struct-typed vs not, verified in the baked source rather
than inferred from behaviour:

    runtime/stdlib/vec.kara:21   struct Vec[=T] { }
    runtime/stdlib/map.kara:10   struct Map[=K, =V] { }
    runtime/stdlib/set.kara:7    struct Set[=T] { }
    String                       no struct declaration anywhere in runtime/stdlib/

So `Vec`/`Map`/`Set` ARE struct-typed bindings and `String` is not, and the
observed behaviour is what the spec prescribes. Changing it would be the
borrow-mode-aware refinement design.md explicitly defers ("remains the target
model but is not v1"), not a bug fix -- and the row itself opens by saying it is
not a request to relax the rule.

THE ROW'S SAFETY WORRY IS ALSO ALREADY HANDLED, which is worth recording since
it was the stated reason a fix looked hard. It warns that exempting the
collections "would ALSO accept concurrent MUTATION of them across branches,
which is a real race". Measured: it would not. Concurrent mutation is rejected
by the EFFECT checker, not by this ownership gate, for both populations --

    Vec mutated from two branches      error[effect] "cannot be written from inside par"
    String mutated from two branches   error[effect] "cannot be written from inside par"

-- which is exactly why `String` has never needed the ownership gate to be safe.

ONE HYPOTHESIS OF MINE WAS ALSO WRONG and is recorded so it is not re-chased: an
owned `String` passed into two par branches is accepted, which looked like a
use-after-move the gate was silently missing. It is not par-specific -- the same
double move in straight-line code reports the identical ownership WARNING and
`karac check` passes. Use-after-move is a warning language-wide; nothing about
`par` changes it. |
| B-2026-08-12-24 | codegen | medium | Inside a GENERIC IMPL, `let`-binding a `T`-typed struct FIELD and then calling a trait method on that local (`let a = self.v; a.describe()`) fails th… | FIXED by 2282be2. `bind_pattern_types` and its match-arm sibling
`record_pattern_binding_surface_types` now record a `Type::TypeParam(name)`
binding, exactly as they already recorded `Type::Named { name }`.

THE SURFACE IS TYPECHECK, NOT CODEGEN, and the row's mechanism guess is
refuted rather than refined. It reads "the monomorph appears to lose the
local's type"; the monomorph never had it. Instrumented at both ends on the
minimal repro and on the boundary row that WORKS:

  let a = p;       (T-typed PARAM)  -> Type::Named { name: "T" }  -> recorded
  let a = self.v;  (T-typed FIELD)  -> Type::TypeParam("T")       -> NOTHING

At the codegen let site the failing binding printed `pbt=None` with the
monomorph subst `{"T": "i64"}` sitting right there, fully populated. Codegen
was not losing anything -- it was never told. A generic parameter reaches the
binding recorder under TWO SPELLINGS depending on where it came from, and only
one of them had an arm.

That is also what explains the boundary the filing found so strange. All three
of its working variants either avoid a binding entirely (`self.v.describe()`)
or bind something that infers `Named` (a param, in a free fn or a generic
impl); only a FIELD read produces `TypeParam`.

FIX: one arm in `bind_pattern_types`, recording the param NAME. That is not a new
behaviour but the existing one made reachable -- codegen's
`record_var_type_name` already resolves a recorded name through the
monomorph's `type_subst_names` (`T` -> `i64`), which is precisely how the
working `Named { name: "T" }` spelling has always worked. Outside a mono the
subst map is empty and the name stays put, same as today.

THE MATCH-ARM SIBLING LOOKS LIKE THE SAME HOLE AND IS NOT, which is the one
thing worth carrying forward from this fix. `record_pattern_binding_surface_
types` mirrors `bind_pattern_types` arm for arm and is likewise missing
`TypeParam`, so mirroring the record there is the obvious next edit. It was
made, and the suite refused it: `test_e2e_generic_enum_{heap,struct_heap,vec}_
payload_match_return` all fail with `Module verification failed: Function
return type does not match operand type of return inst! ret i64 0 / { ptr, i64,
i64 }`. In a match arm the recorded name drives PAYLOAD WORD COUNT, and `"T"`
reads as a one-word named type where the live payload is a three-word
`{ptr,len,cap}`. The `let` site has no payload reconstruction, which is exactly
why the same record is safe there and not here.

The match-arm shape needs no fix regardless: `match self.get(i) { Ok(v) =>
v.describe() }` dispatches correctly with only the `let` arm in place, verified
on the same probe. The reverted attempt is recorded in situ at the `let` arm so
the next reader does not re-make it.

VERIFIED across `karac check`, `--interp`, LLJIT and AOT, all agreeing, on the
filed repro and on the ORIGINAL probe's shape (`let a = self.get(x)?;`) that
the row noted as "the same shape one level further out".

PINNED by `e2e_generic_impl_local_bound_from_t_field_dispatches`, which covers
the field read, the `?`-propagated form and the match arm, at TWO monomorphs --
a scalar (`i64`) and a user struct (`P`). Two instantiations matter here rather
than being thoroughness for its own sake: recording the param NAME is only
correct if the subst resolves it per mono, and a single instantiation would
pass against a hardcoded answer. Non-vacuous: against the pre-fix compiler it
fails with the row's exact message, `no handler for method 'describe' on
variable 'a'`.

ONE ADJACENT GAP FOUND AND FILED AS B-2026-08-12-32, not fixed here: a user
trait impl on `String` or `Slice[T]` is ACCEPTED at the declaration but never
found at the call site, while the same impl on `i64` / `f64` / `bool` / even
`Vec[i64]` resolves. It surfaced because `impl Zero for String` was the natural
second monomorph for this test; the test uses a user struct instead. Confirmed
pre-existing by measuring both sides of this fix.

Suite green at 117 targets, 0 failures; clippy and fmt clean. |
| B-2026-08-12-25 | typecheck | low | `char` has no `to_lowercase` / `to_uppercase` / `is_digit`, though it has `to_ascii_lowercase`, `is_alphabetic`, `is_numeric`, `is_alphanumeric` and… | FIXED by ef73c4d. All three names exist on `char` now, on all three backends,
byte-identical: `to_lowercase` / `to_uppercase` -> char, `is_digit(radix)` ->
bool.

THE SPEC DECISION THE ROW FLAGGED, decided rather than deferred. A scalar can
case-fold to SEVERAL scalars, which is why Rust returns an iterator and a
`char -> char` signature cannot express it. Kāra returns `char` and applies the
full mapping only when it yields exactly one scalar, leaving `self` unchanged
when it expands -- the rule Go's `unicode.ToLower` and Java's
`Character.toLowerCase(char)` already use, so it is a familiar contract rather
than a novel one.

WHAT THAT GIVES UP, COUNTED RATHER THAN GUESSED (whole codespace, surrogates
excluded): exactly 102 scalars expand under uppercase -- `ß`, `ŉ`, `ǰ`, the
Greek iota-subscript block, the `ﬁ`-family ligatures -- and exactly ONE under
lowercase, `İ` U+0130, whose full lowercase is `i` + a combining dot. Every
other scalar in Unicode folds 1:1 and is exact. The count is in the runtime
extern's doc comment so the next reader does not have to re-derive it.

FULL MAPPING IS NOT LOST, which is what makes the collapse a routing choice
rather than a missing capability: `String.to_uppercase()` is already the
full-Unicode String->String transform, so `c.to_string().to_uppercase()` renders
`SS`. Both forms are pinned side by side in the tests.

`is_digit(radix)` MIRRORS RUST AND SHARES `to_digit`'s ARM in all three phases
-- same receiver gate, same u32 radix rule, same 2..=36 trap, and the trap
message names the method that was called. Sharing is deliberate: two arms would
drift on the radix, and the row's sibling method is exactly where a writer looks
next. The discoverability half is preserved by the ARITY diagnostic -- a bare
`c.is_digit()` now says "expects 1 argument, got 0: write `is_digit(10)` for
decimal, or `is_numeric()` for the Unicode predicate", which replaces
B-2026-08-11-2's `no method 'is_digit'` hint (that hint is deleted; the method
is no longer missing).

AND THE PART THAT WAS NOT ABOUT `char` AT ALL, found while wiring it and worth
more than the feature. `to_lowercase`/`to_uppercase` were String-ONLY before
this, and FOUR codegen sites keyed String-ness on the method NAME alone. One of
them says so in a comment: "Restricted to methods that exist ONLY on String, so
a Vec receiver can never take this path ... A non-String receiver here would
already have failed the typechecker, which is what makes the name sufficient."
Putting the two names on `char` falsified that premise everywhere at once.

  - `expr_is_string_like` (expr_ops.rs) was the LIVE one, and the trigger is the
    row's own line: `c.to_lowercase().to_string()` matched `to_string` with a
    "string-like" receiver, entered the String-copy path, and unwrapped an i32
    codepoint as a `{ptr,len,cap}` struct. Verified pre-fix by the identical
    shape one link along (see the separate row filed below), which panics with
    `Found IntValue ... but expected the StructValue variant` at that exact
    site.
  - `try_compile_nonident_collection_method`'s name signal and
    `closure_body_produces_heap_string` (calls.rs) are the same premise in two
    more places; both are now receiver-gated.
  - `expr_is_char` gains the two names so a folded char still RENDERS as a
    glyph -- without it `println(c.to_lowercase())` prints the codepoint, the
    recurring gap that function's comments already record four times.
  - The fifth site, `free_str_vec_buffer_if_heap`, was checked and deliberately
    left alone: it discriminates on the LLVM type (`llvm_ty_is_vec_struct`), not
    the name, so an i32 result no-ops there already. Type-guarded sites were
    never at risk; name-guarded ones all were.

VERIFICATION. Interp / JIT / AOT / `KARAC_AUTO_PAR=0` AOT all byte-identical on
the probe, and the interp and codegen test twins assert the same bytes: the
expanding cases (`ß`, `ﬁ`, `İ`), the `.to_string()` chain, the loop form the row
was filed from, and String receivers on both ends to prove the String->String
transforms were not captured by the new char arm. Typechecker pins cover typing
in BOTH directions (a `String` annotation on the char fold is rejected; a `char`
annotation on the String transform is rejected), the arity hint, the radix-type
error, and that the hint set did not widen. Full suite green at 13503 passed / 0
failed; clippy native + both wasm targets, and fmt, clean. |
| B-2026-08-12-26 | codegen | medium | ELEMENT-TO-ELEMENT index assign LEAKS one buffer per assignment: `ps[0] = ps[1]` over `Vec[Pair]` with `struct Pair { word: String, n: i64 }` loses 4… | FIXED by af532fb. `asan_index_assign_elem_to_elem_no_leak` is flipped from
`#[ignore]` to live and extended; it pins at 1355 B in 160 allocations against
the pre-fix compiler, on the DEFAULT leg (a `Vec` keeps the allocation alive, so
unlike the boxed-envelope family this is not an -O0-only shape).

THE ROW'S SUGGESTED MECHANISM WAS WRONG, and its own advice is what found that
out -- "worth reading the emitted IR for the count of clone calls at this site
before assuming". There is exactly ONE clone, so neither "the clone TEMP created
for the read, freed by nobody" nor "a second clone made and discarded" is the
leak. The whole statement is one clone and one store:

    vidx.ok:  call @karac_clone_struct_Pair(ps[1] -> %vidx.elem.clone)
    v.st.ok:  store %vidx.elem.cloned -> ps[0]        ; and no free
    cleanup.adrop.body: drop each live element        ; scope exit only

Slot 0's old buffer is overwritten and never freed. THE LEAK IS THE DISPLACED
OCCUPANT -- the LHS's previous contents, not anything about the read. Confirmed
by size: `ps[0] = ps[1]` loses 10 B per assign (slot 0 held `alpha..`) and
`ps[1] = ps[0]` loses 8 B (slot 1 held `beta..`), i.e. it tracks the LHS.

`emit_displaced_index_elem_drop` exists to free exactly that and DECLINED. Its
"an RHS mentioning the container declines (uncertain => silent)" guard matched
`ps[1]`. The guard is right in general -- dropping the displaced element before
the RHS is read would free a buffer the RHS still needs -- but it is wrong at
this site, because the emitter runs after `clone_owned_vec_index_element` has
already deep-copied the RHS, and the value in hand no longer aliases anything.

THE SELF-ASSIGN IS THE PROOF. `ps[0] = ps[0]` leaked identically, 400 B / 40.
That is the case the aliasing guard exists for, and it is precisely the case
where the clone makes the drop safe -- so the guard was declining on a hazard
that had already been eliminated. Any account of this bug that does not explain
the self-assign is incomplete, which is what ruled out the clone-temp theory
before the IR did.

The guard is now skipped exactly when the RHS was deep-cloned. Whether it was is
asked by COMPARING THE VALUE rather than by re-deriving the clone's five
admission conditions: `clone_owned_vec_index_element` returns its argument
unchanged on every decline path and a fresh `load` on the one clone path, so
identity IS the answer and there is a single source of truth. A duplicated
predicate would drift the first time either gate is tuned, and the failure mode
of drift here is a double free, not a leak.

MEASURED at -O0 with valgrind, 40 iterations, baseline -> fixed:

    ps[0] = ps[1]                      400 B / 40  ->  CLEAN
    ps[0] = ps[0]   (self-assign)      400 B / 40  ->  CLEAN
    one-temp swap  t / qs[0] / qs[1]   400 B / 40  ->  CLEAN
    h.xs[0] = h.xs[1]  (field-rooted)  400 B / 40  ->  CLEAN
    ps[0] = ps[j]  (variable index)    400 B / 40  ->  CLEAN
    two-temp swap                         CLEAN    ->  CLEAN   (control)
    Vec[Vec[i64]]  vv[0] = vv[1]          CLEAN    ->  CLEAN   (control)

The one-temp swap is the row's own motivation and is now clean; both swap
spellings agree on their output (502), and the E2E test pins the VALUES because
this fix is a free-before-store on a slot the RHS was just read from -- a fix
that freed the wrong side would print garbage rather than merely leak.

VERIFIED BEYOND THE LOCAL MATRIX, since this touches the drop machinery: full
suite 13,492 passed / 0 failed, -O0 leg green with an empty quarantine list, and
drop-fuzz 0 memory-safety findings / 0 invariant violations over 200 generated
programs (558 valid executions, 2,271 scheduled drops).

NOT COVERED, filed separately: an RHS that mentions the container through a CALL
(`ps[0] = mk(ps[0].n + k)`) still declines and still leaks 400 B / 40, unchanged
by this fix. It needs a different argument -- the call's result is fresh, but
nothing here proves the call did not stash an alias -- so the mention guard
keeps declining and the leak stays the safe direction. |
| B-2026-08-12-27 | codegen | high | A heap FIELD read out of a Vec element (`ps[0].word`) is a SHALLOW ALIAS of the container's buffer on both compiled backends | FIXED by d19d0d6. A heap FIELD read out of a Vec element is now deep
CLONED at the read, so the destination and the container own separate buffers.
All eight double-freeing destinations, the silent use-after-free, and both
previously-clean control shapes now agree with the interpreter; the two
fixtures this row landed RED are flipped live.

THE READ IS A COPY, and that is what forced the shape of the fix. `karac check`
accepts reading `ps[0].word` after binding it and the interpreter still has the
value, so aliasing was wrong on both counts. The WHOLE-element read already
cloned (`clone_owned_vec_index_element`); this is the field-read sibling it
never had.

WHAT WAS REJECTED, and why it is worth recording: mirroring the `let` site's
source cap-zeroing at the other seven destinations silences all eight aborts in
a few lines. It is wrong. That suppression IS a move, and the checker says the
read is a copy -- so it would have traded eight double frees for eight silent
use-after-frees of exactly the kind the `let` site already had.

FOUR PIECES, all small:

  * `clone_vec_elem_heap_field_read` (collections.rs) -- deep-clones the read.
    Gated to a plain element index into a NAMED owning Vec, a non-shared user
    struct element, a `{ptr,len,cap}` field, and a value whose LLVM type really
    is that struct (the same defensive width check the sibling makes).
  * the clone gets its OWN scope cleanup, because a non-consuming read must not
    leak -- `ps[0].word.len()` and `ps[0].word + "!"` are common and were clean
    before.
  * `vec_elem_field_clone_slots`, span -> clone alloca, so a CONSUMING
    destination takes the clone over by zeroing its `cap`. The takeover lives
    in `suppress_source_vec_cleanup_for_arg_ex`, which all ~87 consuming sites
    already funnel through -- one edit instead of eight, and it stays `&self`
    because zeroing a cap is a builder store rather than a queue edit. The map
    is span-keyed because the clone is anonymous; there is no binding to name.
  * `suppress_place_field_struct_move_source` now DECLINES an index-rooted
    chain. That call was the whole reason the `let` shape looked clean while
    the other seven aborted. Struct-rooted chains (`o.h.name`,
    B-2026-08-01-31) keep the move -- they have no container element behind
    them and nothing clones them.

THE FRESH-TEMP CHANNEL WAS NOT NEEDED, which is the reason this landed as a
contained change rather than the broad one the row predicted. The row's plan
was to route the clone through `expr_yields_fresh_owned_temp` so consumers free
it; that predicate has 62 consumers and flipping it changes what every one of
them frees. Registering the clone's own cleanup and letting the existing
consuming funnel disarm it gets the same ownership answer and touches one
funnel. The row's estimate is corrected accordingly.

VERIFIED against the interpreter oracle on all fifteen probe programs from the
reshaping pass -- the eight owning destinations, the two shapes that were
already clean (by-value argument, plain `let`), the two non-consuming reads
(`.len()`, concatenation), the mutation case, and two controls. Every one
byte-identical across `--interp` and `karac build`.

PINNED by the two fixtures this row filed RED, now live:
`asan_vec_elem_heap_field_read_freed_once` (all eight destinations in one
program, plus a re-read of the container's own field afterwards -- which is the
half that would catch a disown-the-source "fix") and
`e2e_vec_elem_field_read_is_a_copy` (the silent half; it asserts `a1X\na1` and
got `a1X\n<garbage>` before).

Full ASAN suite green at BOTH opt levels -- 1054 passed at -O2 and at -O0, the
-O0 leg with an empty quarantine list. The leak direction is covered by that:
an unclaimed clone would be an LSan orphan, and a clone claimed twice a double
free. Suite green at 116 targets, 0 failures; clippy and fmt clean. |
| B-2026-08-12-28 | codegen | low | A chained indexed field READ (`a[i][j].field`) failed `karac build` with the generic self-accusing 'cannot resolve field .. | 63b0bd19 |
| B-2026-08-12-29 | typecheck | low | The `s[i]`-on-String rejection prescribes `s.char_at(i)` as the substitute, but `char_at` returns `Option[char]` — so the suggested replacement does… | a092d138 |
| B-2026-08-12-30 | parser | medium | GENERIC PARAMETERS, TRAIT BOUNDS and WHERE-CLAUSES are absent from the span walker ENTIRELY -- not a missing field but a missing subtree | FIXED by 3bf70a6. `visit_generic_params` / `visit_generic_params_mut`,
`visit_trait_bound` / `_mut` and `visit_where_clause` / `_mut` are threaded
through every generics-carrying item arm in BOTH halves of the walker, and the
audit that found the row is now a test rather than a note.

THIRTEEN CARRIERS, enumerated from the AST rather than from the row: Function,
StructDef, EnumDef, TraitDef, TraitAliasDef, MarkerTraitDef, AssocTypeDecl,
TraitMethod, ImplBlock, AssocTypeBinding, EffectResourceDecl, TypeAliasDef,
DistinctTypeDef. The row named eight with a trailing "..."; the two that would
have been easy to miss are `AssocTypeBinding` (the `type Item = T;` inside an
`impl`, which carries its own generics) and `DistinctTypeDef`. Two of the
thirteen -- `TypeAliasDef` and `DistinctTypeDef` -- have `generic_params` but
NO `where_clause` field, which the compiler caught rather than a reviewer.

The traversals also cover what hangs off the generics rather than just the
declared spans: bound generic ARGS (`T: Other[i64]`), const-param types, shape
literals and their dims, and all four `WhereConstraint` shapes -- including
`ConstPredicate`'s expression and `ProjectionBound`'s projection type, each of
which carries spans of its own.

THE AUDIT IS NOW THREE TESTS in `tests/span_visitor_coverage.rs`, all three
verified non-vacuous by running them against the pre-fix walker:

  * `every_ast_span_field_is_visited_by_both_walker_halves` -- the mechanical
    check that found all seven original findings, kept as the row asks. Pre-fix
    it reports `variance_span (read-only: MISSING, mut: MISSING)`.
  * `generics_bounds_and_where_clauses_are_reachable_from_both_halves` -- the
    SUBTREE check, which the field check structurally cannot express: only one
    of these spans (`variance_span`) has a distinctive field name, so six of
    the seven missing spans were invisible to a per-field audit. Pre-fix it
    reports all seven needles missing.
  * `mut_walk_shifts_every_span_the_read_walk_can_see` -- the behavioural one,
    and the only one that proves the property `module.rs` actually depends on.
    It shifts every span through the mut walk and asserts nothing reachable
    from the READ-ONLY walk was left at its file-local offset. Pre-fix it
    FAILS.

THE ASYMMETRY IS THE REAL FAILURE MODE, which is why the third test is phrased
that way rather than as "the mut walk visits N spans". A field in one half and
not the other is worse than a field in neither: the read-only walk is what
attributes a span to a module, so a span it can see but the mut walk cannot
move is attributed at an offset that has already shifted underneath it. That is
`module.rs`'s own warning -- "a span this walk MISSES stays at its file-local
offset and can still collide" -- made executable.

The source-text audits are a NAME check and say so in situ: they prove a field
is mentioned in each half, not that it is mentioned correctly. They are a floor
against the omission class; the behavioural test is what covers correctness.

NOTHING MISCOMPILED BEFORE THIS and nothing does now -- the row was explicit
that no machine-applicable fix is currently built from a generic-parameter
span, so the two consumers that would corrupt had nothing to corrupt with. What
changes is that the latent case is closed rather than mitigated: the moment a
variance / bound / where-clause diagnostic grows a fix-it, it no longer
inherits B-2026-08-11-35's failure mode. Adding spans to the walk only WIDENS
`module.rs`'s `max_end`, so the change is additive by construction.

Suite green at 117 targets, 0 failures; clippy and fmt clean. |
| B-2026-08-12-31 | codegen | medium | The displaced element still LEAKS when an index-assign's RHS mentions the container through a CALL: `ps[0] = mk(ps[0].n + k)` over `Vec[Pair]` with `… | FIXED by 19dbf4a. `asan_index_assign_scalar_reaching_call_rhs_frees_displaced`
pins it at 800 B in 80 allocations against the pre-fix compiler.

SAME DISPLACED OCCUPANT AS B-2026-08-12-26, declined by a different arm of the
same guard. -26 relaxed `emit_displaced_index_elem_drop`'s mention guard for an
RHS that had already been deep-cloned; a CALL gets no such proof, so it kept
declining and the LHS's old buffer kept leaking.

WHICH OF THE ROW'S TWO CANDIDATE FIXES WAS RIGHT: (a), and (b) was based on a
misreading of my own. The row proposed "sink the displaced drop below the RHS
evaluation" as an ordering fix. It is already below it -- `compile_expr(value)`
runs at the top of the Assign arm and the displaced drop near the bottom -- so
there was no ordering to fix. Re-ordering could not have helped either: if the
RHS value aliases the old buffer, freeing it after the store still leaves a
dangling header. The hazard is ALIASING, not sequence, and only (a) addresses it.

WHAT THE GUARD IS ACTUALLY ASKING is whether the already-computed RHS value
points INTO the buffer about to be freed. A textual mention of the container is
a proxy for that, and a coarse one: `ps[0] = mk(ps[0].n + k)` names `ps` solely
to read an `i64` out of it, and an `i64` cannot carry a buffer anywhere.

`expr_cannot_carry_container_heap` answers by REACHABILITY rather than by
trusting the callee, which is what lets it stay sound with no escape analysis:

  * an expression that never names the container cannot carry its heap;
  * a call whose every argument is safe cannot either, because the callee is
    handed no pointer into the container to hand back;
  * a scalar FIELD read of an element (`ps[i].n`) is admitted by resolving the
    field's own type through the element type -- a `String` field is not;
  * everything unenumerated is UNSAFE, so a false negative is the status-quo
    leak and a false positive would be a double free.

MEASURED at -O0 with valgrind, 40 iterations, baseline -> fixed:

  ps[0] = mk(ps[0].n + k)             400 B / 40  ->  CLEAN
  rs[1] = mk(rs[0].n * 2 + rs[1].n)   400 B / 40  ->  CLEAN
  ps[0] = mk(k + i)   (no mention)       CLEAN    ->  CLEAN   (control)
  ps[0] = passthru(ps[0])             400 B / 40  ->  unchanged, still declines
  ps[0] = takes(ps[0].word)           400 B / 40  ->  unchanged, still declines

The `rs` leg is the one that would catch a sloppy version: its RHS reads scalars
from BOTH elements, including the one being overwritten, so a predicate that
only checked the assigned index would admit it for the wrong reason.

VERIFIED beyond the local matrix, since this is drop machinery: -O0 leg green
with an empty quarantine list, and drop-fuzz 0 memory-safety findings / 0
invariant violations over 200 generated programs (558 valid executions, 2,271
scheduled drops). Re-measured after rebasing onto B-2026-08-12-27's fix
(`clone a heap field read out of a Vec element`), which lands in adjacent
machinery -- the whole matrix is unchanged by the combination.

NOT COVERED, filed separately: the two shapes whose ARGUMENT genuinely carries
heap out of the container -- `passthru(ps[0])` (the element itself) and
`takes(ps[0].word)` (a heap field). Both still leak 400 B / 40, unchanged by
this and by -26. They need the callee's escape behaviour, which nothing at this
site establishes. |
| B-2026-08-12-33 | codegen | medium | The displaced element still LEAKS when an index-assign's RHS passes container HEAP into a call: `ps[0] = passthru(ps[0])` and `ps[0] = takes(ps[0].wo… | FIXED by 0eec7ad. Both shapes are clean under valgrind (200 B / 40 blocks each ->
0) with output unchanged on all four surfaces, and the ASAN/LSan pin fails
without the code change.

NEITHER FIX TRUSTS THE CALLEE, which is what the row said the shapes would
need. Both establish the same thing the reachability arm establishes -- that
the callee was never handed a pointer into the container -- by finding the
copy that already happens.

  - `takes(ps[0].word)`. The row's own WORTH CHECKING FIRST was the answer:
    B-2026-08-12-27 deep-clones a heap FIELD read off a Vec element AT THE
    READ, so the argument is an independent buffer and the element keeps its
    own. The row measured "the leak is unchanged after rebasing onto that
    commit" and concluded the clone might not cover the call-argument
    position. It does. The leak was unchanged because the clone's existence
    was never the blocker -- the GUARD could not see it. It now asks, and the
    same clone that was there all along clears the shape.
  - `passthru(ps[0])`. No caller-side clone exists, but an owned bare-`Path`
    aggregate param is callee-owned by ENTRY COPY
    (`make_aggregate_param_callee_owned_inst`): the callee duplicates the heap
    fields into its own frame before the body runs, so what it can hand back
    is its copy. The displaced occupant therefore still has exactly one owner,
    the container, and freeing it is the missing free rather than a second
    one.

ASKED OF THE EMITTED CODE, NOT RE-DERIVED. The field clone's admission test is
eight conditions deep; restating it at the guard would drift from the real one
the first time either is tuned, with a double free as the failure mode -- the
lesson B-2026-08-12-26 already wrote down when it chose value identity over a
second predicate. So the clone now RECORDS its span when it fires and the guard
reads the record. The entry-copy arm calls
`aggregate_param_copy_supported_struct` -- the same function the callee's entry
copy is gated on, not a copy of it.

THE RECORD IS ORDERED, NOT JUST KEYED, and that is a real distinction rather
than tidiness: a span-keyed map answers "was this span ever cloned in this
module", which is a different claim once one source expression compiles twice
(two monomorphs of one generic body) and the clone's type-driven gates decide
differently in each. The stale hit would authorize freeing a buffer the second
context still aliases. The guard marks the log's length before the RHS is
compiled and looks only past that mark, so the evidence is the statement's own.

OWN-BY-TRANSFER IS EXCLUDED ON PURPOSE, and it is the sharp edge of the
entry-copy arm. B-2026-08-05-33's arm makes a param callee-owned by handing it
the ORIGINAL buffers with no copy at all; its scope-exit drop then frees
exactly what this free would free. Asking "is the param callee-owned" would
admit it and produce a double free -- which is why the gate asks for the COPY
arm specifically. A `Map`-bearing element (`Reg { by: Map[String, i64], .. }`)
is the measured instance and stays declining, verified clean.

A SECOND, NARROWER GATE ON TOP, from a measurement rather than a hunch: the
element's fields must be scalars or a DIRECT buffer (`String`, `Vec[scalar]`,
`Vec[String]`). A NESTED STRUCT field passes copy-support, but the whole shape
it would admit -- `ds[0] = bump(ds[0])` over `Deep { inner: Pair, .. }` --
already DOUBLE-FREES on both compiled backends BEFORE this change, on a single
assignment, while `ds[0] = mk(ds[0].tag + 1)` and `ds[0] = ds[1]` over the same
element are clean. Filed as its own row. Admitting the shape would mean
reasoning from a property the target program demonstrably does not have, so it
is excluded until that row is fixed and it can be re-measured. Verified
byte-identical to pre-change behaviour with the narrowing in place.

STILL DECLINING, each a leak rather than corruption, each verified unchanged:
a `ref`-param callee (nothing copies anywhere -- 29 B / 5 blocks, before and
after), and a FIELD-ROOTED container (`h.xs[0] = passthru(h.xs[0])`), where
both the field-read clone and the entry-copy arm want an `Identifier` root.

MEASURED ACROSS SURFACES, not just under the sanitizer: interp / JIT / AOT /
`KARAC_AUTO_PAR=0` AOT byte-identical on every probe, with valgrind
before/after on each -- 39 B / 15 blocks -> 0 on the mixed-shape program,
192 B / 5 -> 0 on the `Vec[String]`-element one. Pins: an ASAN twin over all
four call shapes including the `Vec[String]` element (asserted to FAIL without
the code change, so it is not vacuous), a codegen value test that the round
trip through the callee and back into its own slot preserves the string, and
an interpreter twin fixing the oracle those values are checked against. |
| B-2026-08-13-1 | ownership | low | The ownership checker treats an OWNED String passed as the ARGUMENT of a read-only String method as a MOVE, and does so INCONSISTENTLY across the thr… | FIXED by 1299fd3. All seven read-only String methods now classify their argument
as a BORROW, so the row's one-line repro is clean and prints `abc|abc`
identically on both backends.

THE ROW'S FIX DIRECTION WAS RIGHT and its diagnosis was exact: the typechecker
had already been widened to accept a borrow in these positions and says so in
`is_str_like`'s doc, while the ownership phase still classified the owned
argument as a consume. The mechanism is one table --
`collect_method_param_modes` (src/ownership.rs) seeds builtin methods that have
no syntactic signature, and `String.contains` was the only String method ever
added to it. That is the whole of the inconsistency the row measured: `contains`
was clean because it was on the list, `starts_with` and `push_str` were not.

WIDER THAN THE ROW MEASURED. It named three methods (`push_str`, `contains`,
`starts_with`, from `is_str_like`'s doc). Probing the neighbouring surface one
method at a time found the same false positive on FOUR more -- `ends_with`,
`find`, `split` and `replace` -- all fixed here. `replace` takes the mode vector
`[Ref, Ref]` rather than `[Ref]`: both of its arguments are scanned into a
freshly built result and neither is stored.

ONE ENTRY IN THE ROW'S NEIGHBOURHOOD IS A GHOST. `String.insert_str` appears in
ownership.rs's RECEIVER-mode table, but there is no such method: the typechecker
rejects `b.insert_str(0, t)` with "no method 'insert_str' on type 'String'". It
is deliberately NOT given an argument mode here -- doing so would invent a
signature for a method that does not exist. Recorded because my first probe of
it reported a move and looked like an eighth case; the "move" text was the
ownership warning printed alongside the typecheck error, not a finding.

THE CRITERION IS BYTES-NOT-HEADER, and it was verified rather than assumed. Each
of these copies or scans the argument's UTF-8 bytes and never stores its
`{ptr,len,cap}` header, so the caller keeps the buffer and the callee frees
nothing. A probe reusing one owned `String` as the argument of all six others
and then reading it again is valgrind-clean at -O0, before AND after the change,
and agrees with the interpreter. That "before" measurement is the load-bearing
one: it proves the change is a pure re-classification with no memory behaviour
riding on it, which is what the row predicted ("NOT A CORRECTNESS BUG").

`Vec.push` and `Map.insert` are the negative control and stay OWN -- they store
the argument itself, so reusing it is a real use-after-move. A test pins that
they still report, because if the byte-copying criterion is ever applied to a
method that keeps the header, the next symptom is a double free rather than a
diagnostic regression.

THE COST THE ROW DESCRIBED IS GONE. Its kata #257 shape -- an owned `Frame`
popped from a worklist whose `prefix` is read twice as a `push_str` argument,
with no `ref` binding available to hand -- was written around an
`extend(prefix: ref String, v: i64)` helper that existed solely to turn two
owned reads into two borrows. Written directly, without the helper, it now
passes `karac check` silently, runs valgrind-clean and gives the right answer. |
| B-2026-08-13-3 | codegen | high | Passing a Vec ELEMENT whose struct has a NESTED STRUCT field to a call and assigning the result back to that same slot DOUBLE-FREES on both compiled… | FIXED by 5e6b6f0. The abort is gone on both compiled backends, output matches the
interpreter, and the ASAN pin fails without the code change.

THE ROW'S PREMISE WAS WRONG, and correcting it is most of the finding. It was
filed as an index-assign bug -- "passing a Vec ELEMENT to a call and assigning
the result back to that same slot" -- because that is the shape the adversarial
probe happened to use. Bisecting it took the container, the index-assign and the
call-argument position away one at a time, and the abort survived all three:

    struct Pair { word: String, n: i64 }
    struct Deep { inner: Pair, tag: i64 }
    fn take(d: Deep) -> String { d.inner.word }     // <- the whole bug
    fn main() { let d = Deep { .. }; println(take(d)); }

No `Vec`, no index-assign, no element. The real subject is a NESTED heap field
moved out of an OWNED BY-VALUE aggregate param.

WHY IT DOUBLE-FREES. An owned bare-`Path` aggregate param is callee-owned: it is
entry-copied and its scope-exit struct drop frees its heap. Moving a heap field
out of it therefore has to zero that field's `cap` in the source, or the drop
frees the buffer the caller was just handed. That cap-zeroing exists and is
shared by all ~87 consume sites -- but it resolved the owner by matching the
RECEIVER against an `Identifier`/`SelfValue`. `d.word` matches. `d.inner.word`
does not: its receiver is itself a place expression, so the arm fell through and
nothing was zeroed. The one-level twin has always worked, which is what makes
this a gap in REACH rather than a disagreement about ownership -- and it is now
pinned beside the nested form so a refactor cannot fix one and lose the other.

THE FIX WALKS THE CHAIN to its root and GEPs down the same path the drop walks,
then hands the innermost struct to the EXISTING helper -- so the zeroing rule
itself is unchanged and still lives in one place. Guarded for the property the
one-level arm is guarded for, not by analogy: the root slot must hold its struct
INLINE (a `ref Struct` param's slot is a pointer into the CALLER's frame, and
GEP-ing a `cap` off it writes past the alloca -- the B-2026-07-07-4 class), every
hop must be a non-shared user struct held inline in its parent, and each hop's
LLVM field type must match the registered struct type (inside a monomorph a
bare-`T` field is erased in the base layout, so a mismatch means the offsets
cannot be trusted). Every decline is the status-quo double free, never a new
failure mode.

WHAT THE BISECTION ALSO SHOWED, and what the fixtures had to be rebuilt around:
the abort is invisible unless the moved-out String is actually PRINTED.
`let r = take(d); println(r.len());` is clean, and so is the same call inside a
`while` loop accumulating lengths -- the first fixture written for this row was
green against the UNFIXED compiler for exactly that reason. A pin for this class
has to consume the value as a string, and this one is asserted to fail without
the fix.

MEASURED across all four consuming positions the suppression serves, each
reaching it by a different route: bare tail RETURN, struct LITERAL field, call
ARGUMENT, and a `let` binding (the last was already clean -- its own site
handles the chain -- so it is the control saying the fix did not double up on a
working position). Two hops deep (`o.mid.inner.word`) as well as one, since the
walk is a loop. Interp / JIT / AOT / `KARAC_AUTO_PAR=0` byte-identical.

AND IT RETIRED THE GATE IT WAS BLOCKING. B-2026-08-12-33's entry-copy arm
shipped with a narrower gate (`elem_fields_are_scalar_or_direct_heap`) that
excluded elements with a nested struct field, because this abort made it
impossible to reason from the entry-copy property in that shape. With the abort
fixed, the shape re-measures clean and the gate is REMOVED rather than left as
scar tissue: `ds[0] = bump(ds[0])` over `Deep { inner: Pair, tag: i64 }` leaked
60 B in 20 blocks with the gate and is fully clean without it, which is its own
ASAN pin. That is the widening the row asked for, done on measurement.

ONE MORE BUG FELL OUT and is filed separately rather than folded in: binding a
nested heap field off a Vec ELEMENT (`let w = ds[0].inner.word`) double-frees on
its own, with no call and no index-assign -- B-2026-08-12-27 clones the
one-level `ps[0].word` and has no nested sibling. It reproduces identically
before this change, so the two are independent; the index-assign fixture here
reads only scalars back out of the element to keep from asserting that bug in
this row's name. |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
