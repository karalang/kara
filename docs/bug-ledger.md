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
| `source` | `kata:<num>` · `selfhost:<comp>` · `dogfood:<name>` · `internal` | who/what surfaced it |
| `surface` | codegen · typecheck · interp · ownership · effect · lexer · parser · runtime · resolver · cli · autopar · other | which compiler phase the defect was in |
| `class` | `unported` · `shared-logic` · `port-mistake` · `""` | port-triage taxonomy (phase-12 §); empty unless it's a self-hosting port bug |
| `severity` | high · med · low | `high` = soundness / miscompile / bootstrap-critical |
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
## Current state

_Generated from `bug-ledger.jsonl` by `scripts/bug-curve.py` — **74 surfaced · 1 open · 73 fixed** (2026-05-20 → 2026-06-14). Do not edit this block by hand; edit the ledger and regenerate._

### Open (1)

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-06-09-2 | 2026-06-09 | interp | low | Residual interp instruction-count drift on map-heavy workloads (333B->378B), interp-lane only | phase-4-interpreter.md |

### Fixed (73)

<details><summary>73 fixed — the regression test is the durable artifact; prose lives in each owning phase tracker</summary>

| id | surface | sev | title | fix |
|---|---|---|---|---|
| B-2026-05-20-1 | interp | med | Vec.pop synonym not dispatched in interpreter (only pop_back/pop_front handled) | 7ebb8dd |
| B-2026-05-25-1 | codegen | med | No String.push(char)/push_str builder primitive — only O(n^2) f-string self-append | 7ef42b9 |
| B-2026-06-07-1 | typecheck | high | TaskGroup escape rejection — ScopeLocal structural enforcement gap (escape could outlive frame) | — |
| B-2026-06-07-2 | codegen | med | Struct-returned-by-value ABI fall-through mis-sizes annotation-driven slot (TaskGroup/TaskHandle) | — |
| B-2026-06-07-3 | codegen | high | Borrow-mode File pattern bind registered its own close — double-close of the source fd | — |
| B-2026-06-07-4 | interp | med | Map-heavy interpreter dispatch regression — backpressure clone in method dispatch hot path | — |
| B-2026-06-07-5 | codegen | high | Returned borrows (-> ref T) v1 COMPLETE: tiers+Option[ref T], false-pass fixed, docs honest; StringSlice=v2 | — |
| B-2026-06-08-1 | codegen | high | Narrow-int binop computed at operand's true LLVM width, wrapping/truncating the wider operand | — |
| B-2026-06-09-1 | lexer | low | F-string interpolation extractor not string-aware — brace/escaped-quote inside literal miscounts | — |
| B-2026-06-10-1 | codegen | med | Vec.contains / String.contains codegen lowering (linear element scan) | — |
| B-2026-06-10-2 | codegen | high | Moving a heap field out of a by-value struct param shallow-shared the buffer — double-free | — |
| B-2026-06-10-3 | codegen | high | VecDeque/String with_capacity + match-bound VecDeque payload: missing codegen arm crashed | — |
| B-2026-06-10-4 | codegen | high | Borrow-returning call forwarded into ref param double-freed source heap (first(pick(v))) | — |
| B-2026-06-10-5 | codegen | high | Vec[(i64,String)] clone UAF — armed f-string acc freed source String after push | 63440878 |
| B-2026-06-10-6 | codegen | high | Option/Result inline-heap payload leaked when dropped undestructured (Result/Map/non-Call RHS) | 9995e88b |
| B-2026-06-10-8 | codegen | med | RC-fallback let-bound tuple/struct drop leak — box free at rc==0 didn't recurse into heap fields | 669db992 |
| B-2026-06-11-1 | codegen | med | Array[u8,N].as_ptr()/.as_mut_ptr() had no codegen handler (feeds CStr.from_ptr) | 943d9d0d |
| B-2026-06-11-2 | codegen | high | Block expression in value position freed its tail heap value — empty/dangling result under AOT | — |
| B-2026-06-11-3 | codegen | high | Unsafe raw-pointer deref *p returned address instead of loading; *p=val store side too | e08d9277 |
| B-2026-06-11-4 | codegen | med | By-value aggregate (tuple/literal/nested struct) leaked heap fields on drop | — |
| B-2026-06-11-5 | codegen | med | Direct block-construct call argument leaked its temp (ownerless after tail-cleanup suppression) | — |
| B-2026-06-11-6 | codegen | med | Struct field through a tuple element (t.1.name) compiled to i64 0 placeholder under AOT | — |
| B-2026-06-11-7 | lexer | med | Chained tuple index t.1.1 failed to parse — lexer ate 1.1 as a float | — |
| B-2026-06-11-8 | runtime | med | Vec-using compute binary re-anchored heavy std-IO runtime cluster (alloc_or_panic stderr OOM) | — |
| B-2026-06-11-10 | codegen | med | unwrap_or(default) on Option/Result unimplemented across typecheck+interp+codegen | — |
| B-2026-06-12-1 | codegen | med | wasm32 alloc wrappers need i64 shims — direct i64 call traps signature mismatch on i32 size_t | — |
| B-2026-06-12-2 | other | med | CI test-coverage follow-on — Linux-LSan leak gate (13 leaks) now closed and required | — |
| B-2026-06-12-3 | codegen | med | Method call on tuple-destructure-bound String/Vec/Slice failed codegen (unregistered dispatch) | — |
| B-2026-06-12-4 | interp | med | Ill-typed binop/unary (String*Int, -String) under karac run panicked unreachable! in interpreter | — |
| B-2026-06-12-5 | codegen | med | push_str of a fresh-owned String range-slice temp (src[a..b]) leaked once per call | — |
| B-2026-06-12-6 | codegen | med | Entry-copy of enum-field struct from fresh-temp ctor arg leaks (#22): inline struct-literal arg with an enum leaf, callee consumes it via match, no caller drop — track_inline_owned_aggregate_arg gated on enum-blind aggregate_has_heap_field; added source-level copy-supported gate | 9b161ee0 |
| B-2026-06-12-7 | autopar | med | Auto-par reduction: for _ wildcard loop var failed to lower (fell back to sequential) | f0f456b9 |
| B-2026-06-12-8 | ownership | high | Struct ref/mut ref non-receiver method arg passed by value + spurious RC-promotion (segfault) | — |
| B-2026-06-12-9 | codegen | high | ? inside main() -> Result[..] miscompiles — ret {i64,i64} vs i32 entry-point signature | e5be4553 |
| B-2026-06-12-10 | codegen | med | Self-host lexer per-iteration leak — inline enum-ctor temp call arg missing caller-side drop | ecfa867a |
| B-2026-06-12-11 | typecheck | med | push_str/contains/starts_with rejected a borrowed `ref String` arg under build | 522bec1c |
| B-2026-06-12-12 | codegen | med | chained .bytes().len() failed in codegen (slice-header method-chain receiver) | 240389ff |
| B-2026-06-12-13 | codegen | high | push_str(substring(..)) leaked the fresh-owned temp unbounded (token-text surface) | 5ebdc96c |
| B-2026-06-12-14 | codegen | med | No String.repeat(n) primitive — the cur*count repeat op had no builtin | bb10c5ce |
| B-2026-06-13-1 | codegen | high | RC-fallback binding at ref/mut ref arg site — get_data_ptr returned box slot, read/wrote rc header | — |
| B-2026-06-13-2 | runtime | med | Lean sort speed regression — common-case 8/16-byte fast path restored (2.14x jump, low-card deferred) | 8ad33528 |
| B-2026-06-13-3 | cli | low | RC-fallback perf note dropped from default check text output (only json/LSP showed it) | 0862d529 |
| B-2026-06-13-4 | effect | med | allocates(Heap) declarability inconsistency — substrate effect wrongly listed as must-declare | — |
| B-2026-06-13-5 | codegen | med | Tuple-destructure leaf cleanup — let (a,b)=pair() leaves got no scope-exit free (leak) | — |
| B-2026-06-13-6 | autopar | med | Tuple-destructure binding escaping auto-par group got no slot — Undefined variable codegen abort | — |
| B-2026-06-13-7 | codegen | high | Unqualified struct-variant pattern (A { n }) didn't bind fields — Undefined variable | — |
| B-2026-06-13-8 | codegen | high | Shared enum struct-variant: built inline aggregate not RC box + match fields stayed unbound | — |
| B-2026-06-13-9 | codegen | med | Unannotated let a=E.A{..} registered variant not enum in var_type_names — method dispatch missed | — |
| B-2026-06-13-10 | other | high | Recursive shared enum with recursive-variant-first overflowed compiler stack in exhaustiveness | — |
| B-2026-06-13-11 | codegen | med | Recursive shared enum boxes not recursively rc-dec'd — child boxes leaked | — |
| B-2026-06-13-12 | typecheck | med | Unqualified struct-variant construction (Variant {..}) rejected as not-a-struct | — |
| B-2026-06-13-13 | codegen | med | Enum drop into nested-struct payload (in-place 16449077 + moved-out 129d6edc); Vec[heap-element] payload is a deferred outer-buffer-only design position (phase-7.2) | 129d6edc |
| B-2026-06-13-14 | codegen | high | Narrow-int arith branch poisons if/match/if-let phi merge — returned const-0 placeholder | 32ad0c84 |
| B-2026-06-13-15 | cli | med | karac run downgrades hard type errors (E_INT_AS_CHAR class) to warnings + runs with placeholder — silent wrong output, exit 0 | b59eb070 |
| B-2026-06-13-16 | codegen | med | String.split on wasm trapped signature_mismatch — FFI size params usize (i32 on wasm32) vs codegen i64; retyped u64 (same class as B-2026-06-12-1, own symbol) | 5f660971 |
| B-2026-06-13-17 | codegen | med | String collection method (split/contains) on a non-identifier receiver fell through identifier-keyed dispatch — materialize synth local + route to compile_vec_method (Vec-element-typed residual still open) | d4832861 |
| B-2026-06-13-18 | autopar | high | auto-par parallelized console-output stmts (no resource effect); workers raced on stdout, reordered output | 48145ad4 |
| B-2026-06-13-19 | codegen | high | Map field drop hardcoded karac_map_free_with_drop_vec(handle,1,1) — for an occupied scalar Map[i64,i64] the runtime read offset-16 of an 8-byte key as a bogus cap and freed the key VALUE as a pointer (corruption); now compute (drop_key,drop_val) from K/V types via map_drop_flags. Hit both emit_tuple_elem_drops AND the regular struct-field MapOrSet drop (plain struct S{m:Map[i64,i64]} crashed on main) | c3d120e9 |
| B-2026-06-13-20 | codegen | high | Map leaf in a tuple inside a struct field double-freed — Maps are caller-retains (origin FreeMapHandle frees) but #21's NestedTuple struct drop added a second freer; fix transfers the handle to the tuple owner at construction (Part B) + TypeExpr tuple-var drop (Part A) + null-on-move-into-struct (Part C1). Original tracker hypothesis (by-value-consume double-free) DISPROVEN: by-value Map param is caller-retains | c3d120e9 |
| B-2026-06-14-1 | codegen | med | synthesize_tuple_drop_fn_te memoization key (type_expr_sig) used only the base path segment, so Map[i64,i64] (flags (0,0)) and Map[String,i64] (flags (1,0)) shared one drop fn keyed Map_i64; whichever map shape was synthesized first dropped the other's heap (scalar-first leaked a later Map[String,_]'s keys; String-first ran drop_key=1 over a scalar map, the B-2026-06-13-19 garbage-free class). Fix: fold generic args into the sig. Pre-existing #23-era bug, surfaced by #24 making the call-RHS tuple path reachable | 1410a427 |
| B-2026-06-14-2 | codegen | med | phase-12 #24: a let-bound tuple VAR sourced from a CALL (let p = ret_tuple(i), RHS a Call not a tuple literal, no annotation) whose only heap is an enum/Map/Set leaf leaked 1/iter - track_tuple_var's LLVM walk is enum/Map-blind and tuple_binding_elem_tes missed the call-result source. Fix recovers element TypeExprs from the callee's return type (fn_return_type_exprs). Method-call RHS deferred (leaks, never double-frees) | 1410a427 |
| B-2026-06-14-3 | cli | low | whole-program 'karac query effects'/'query concurrency' looked each fn up by the call-graph node key, but call_graph::render_target_type keyed impl methods by the rendered receiver (Box[T].m via render_type_expr) while effectchecker+concurrency key by the bare base name (Box.m via segments.last()). Keys agreed for plain receivers but diverged for GENERIC ones, so an impl[T] Box[T] method reported empty effects under 'query effects' and was dropped entirely from 'query concurrency' (silent data loss). Fix: render_target_type extracts the bare base name, matching NodeInfo's documented join contract; also fixes Type.assoc() edges to generic types. Surfaced by the Cartographer dogfood | 34bcd728 |
| B-2026-06-14-4 | codegen | high | phase-12 #25: reading a struct field through a <struct>.tuplefield.0.<field> chain (h.ps.0.n / match h.ps.0.tok) mis-compiled - type_name_of_expr's TupleIndex arm resolved only Identifier-rooted tuples, so field_index_for found no element struct type and compile_field_access returned the i64 0 placeholder (scalar read 0; enum-field match scrutinee never resolved -> no String dispatch for arm binding s -> no-handler build-fail). Fix: resolve the element struct type via place_chain_tuple_tes for non-Identifier-rooted tuples. Original 'arm-binding dispatch registrar' hypothesis disproven - typechecker typed s fine, the codegen READ was the bug | cf476a7b |
| B-2026-06-14-6 | codegen | high | phase-12 #26: a method on a Map/Set tuple element (h.m.0.len()) read a GARBAGE handle. Map/Set lower to an opaque ptr handle whose runtime methods resolve via a NAMED slot (compile_map_method -> get_data_ptr), so the dispatch sites gate on object.kind==Identifier; a tuple-index Map receiver fell through to a generic path. Fix: try_compile_tuple_index_receiver_method GEPs the element handle slot (field_chain_place_ptr) and re-dispatches via a synth identifier (shared compile_method_via_synth_elem_ptr, factored out of the FieldAccess peer). Gated to Map/Set (Vec/scalar work via value extraction). Synth aliases h's handle slot - reads borrow, in-place insert mutates, h is sole freer. Original framing (element-0 GEP load) disproven | 8a8619cf |
| B-2026-06-14-8 | codegen | high | phase-12 #27: binding a heap-bearing value OUT of a tuple element double-freed at scope exit. let inr = h.ps.0 (heap struct moved out) and let tk = h.ps.0.tok (enum field moved out through the tuple element) each registered the binding's drop (via the lowering type annotation) but did NOT suppress the SOURCE, so both the binding's drop and the owning h's NestedTuple tuple drop freed the same buffer. Fix: (1) call suppress_tuple_index_move_source in the struct-binding path (zero_tuple_elem_cap_at routes a struct element through zero_struct_move_caps); (2) new suppress_place_field_enum_move_source for <tupleindex>.field via place-chain GEP + zero_enum_payload_caps. Part (a) type-registration was already done by the annotation - only source-suppress (b) was missing | 7110d21f |
| B-2026-06-14-9 | codegen | med | phase-12 #28: a Map/Set bound to a LOCAL from a place source (let mm = s.m / let mm = h.m.0) build-failed 'no handler for method len on variable mm' - the let-stmt's unannotated-RHS fallback had Vec/VecDeque/String arms but none for Map/Set, so mm got var_type_names but never the K/V dispatch side-tables. Fix: a Map/Set arm registers them via register_var_from_type_expr keyed off the typechecker's pattern_binding_inner_types. Dispatch-only (no FreeMapHandle) - track_map_var is gated on a fresh-handle RHS, so mm stays a caller-retains alias and the owner is the sole freer (no double-free). Annotated form already worked. Surfaced fixing #26 | 253b7335 |
| B-2026-06-14-10 | other | low | phase-12 #13: no Unicode char classifier - Kara shipped only u8.is_ascii_* byte predicates. Added char.is_alphabetic/is_numeric/is_alphanumeric/is_whitespace end-to-end: typecheck (char receiver -> Bool, rejected on non-char), interp (Value::Char arm via Rust char methods), codegen (karac_runtime_char_is_* externs - Unicode tables can't be inlined; char lowers to i32, extern returns i8), runtime (char::from_u32(cp).is_some_and). Verified interp==codegen incl Unicode (Greek alpha U+03B1 is_alphabetic, Devanagari digit U+096B is_numeric). Unblocks the lexer multi-codepoint non-ASCII recovery (#29) | 173ff36b |
| B-2026-06-14-11 | codegen | high | `let w = v[i]` for a heap-owned Vec[String]/Vec[Vec] (cap>0) element double-freed at scope exit: compile_index returns a SHALLOW element struct sharing the buffer, so both w's drop and v's element-drop free it. Codegen-only (karac check/run fine, only build SIGABRTs); masked for literal cap-0 elements (#171). Vec-index sibling of the move-out family (B-2026-06-14-8). Fix: deep-clone at the let bind (clone_owned_vec_index_element, matching interp's clone — v[i] stays valid), via the per-type clone fn. Scoped to String/Vec; enum/struct Vec elements -> B-2026-06-14-12 | 8555f44a |
| B-2026-06-14-12 | codegen | high | Reading a heap-bearing ENUM or STRUCT Vec element (Vec[E] where E.Tag(String), or Vec[struct{name:String}]) shallow-aliases the container buffer — same root as B-2026-06-14-11 but enum/struct elements fell through clone_owned_vec_index_element's vec-struct gate AND emit_clone_fn_for_type_expr shallow-cloned user enum/struct (no #[derive(Clone)] analog). Codegen-only (interp clones, build SIGABRTs/ASAN double-free). Fix has 3 parts: (1) emit_struct_clone_fn + emit_enum_clone_fn synthesize deep clones (struct mirrors emit_tuple_clone_fn; enum mirrors emit_enum_drop_switch's tag-switch + per-variant word-region field clone), wired into emit_clone_fn_for_type_expr's Path branch; (2) broadened clone_owned_vec_index_element's gate (dropped the llvm_ty_is_vec_struct restriction) so enum/struct/tuple `let w = v[i]` route to the new clones; (3) clone the scrut at the `match v[i] {V(s)=>}` compile site (so the arm extractvalue reads the clone) + materialize_freshtemp_enum_scrutinee now drop-tracks a heap Vec-index scrutinee (expr_is_heap_vec_index) so a no-bind arm frees the clone. ASAN regression: asan_let_bound_vec_enum_struct_element_no_double_free | — |
| B-2026-06-14-13 | ownership | high | A `for x in xs` loop binding sharing a name with an earlier same-function `let x` was conflated by the ownership RC analysis: the formal RC predicate paired the prior binding's Consume (e.g. `push(handle)`) with the loop body's use of the loop var (`handle.join()`) as dominance-incomparable across the loop boundary and inserted a spurious RC fallback on the shared name; codegen (which keys RC by demangled name) then RC-boxed the binding and mis-lowered the plain `{i64}` loop element as an Rc pointer -> SEGV (native, exit 139) / TaskHandle.join deadlock (wasm-threads). Root cause: cfg.rs's ExprKind::For arm ignored the loop pattern (no scoping, no Define), so the loop var shared the outer binding's identity. Fix: scope the for-loop binding to a per-loop @forN rename frame (like match arms' @armN) + record a Define for it (src/cfg.rs push_for_rename_frame). demangle_binding strips @forN for rc_values/diagnostics. Surfaced + fixed building Fathom; regression tests/codegen.rs::e2e_for_loop_binding_name_collision_no_false_rc | 5f32eb18 |
| B-2026-06-14-14 | codegen | high | TaskHandle[T].join() returned a NON-scalar T as garbage + trapped. recover_task_handle_join_return_ty hardcoded i64 (documented slice-4 limitation), so a spawn returning Vec/String/struct had join read i64-shaped bytes off the runtime result buffer instead of the {ptr,len,cap} header - .len() was garbage and the program SIGABRT'd (native exit 133) / 'unreachable' (wasm-threads). Scalar joins were fine; the spawn write side was already correctly sized. Fix: typechecker records each join's T in a new span-keyed task_join_return_types table (intercept in infer_method_call, mirrors the spawn/channel_elem_types pattern), forwarded ast->lowering->codegen; recover_task_handle_join_return_ty now looks it up + lowers via llvm_type_for_type_expr (i64 fallback only for unrecorded sites e.g. discarded-handle accept loops). Verified native + wasm-threads return Vec[u8] row-bands intact. Surfaced building Fathom (parallel Mandelbrot, examples/fathom) | 4363d6c1 |
| B-2026-06-14-15 | codegen | high | Numeric (int/float) f-string interpolation traps on ALL wasm targets: println(f"{x}") for x:i64/f64 (any to_string-via-snprintf path) aborts with 'unreachable' because wasm-ld emits a trapping `signature_mismatch:snprintf` stub. Root cause: codegen declares int snprintf(char*, size_t n, const char*, ...) with i64 for `size_t n` (src/codegen.rs ~2582, hardcoded i64_type) - correct on 64-bit native but WRONG on wasm32 where size_t is i32 (pointer width), so the declared signature can't reconcile with wasi-libc's snprintf and wasm-ld replaces the call with a trap. Confirmed sequential wasm_browser AND --features wasm-threads; affects wasm_wasi too. String-only f-strings (f"{name}") + plain println work (no snprintf). PRE-EXISTING, not introduced by the Fathom slice - surfaced debugging a Fathom test program with f-string println. FIXED: declare + call snprintf with a pointer-width size_t (i32 on wasm32 / i64 on 64-bit, via the target usize/pointer-int type, not hardcoded i64_type) and pass the 64-byte buf size with matching width at the call sites (src/codegen/runtime.rs ~4958 fst path + control_flow.rs compile_print). Regression: a wasm E2E asserting println(f"{42}") prints 42. | 1158d525 |
| B-2026-06-14-16 | codegen | high | #[derive(Display)] on a baked-stdlib enum (IoError, VarError) renders correctly in the interpreter but degraded in AOT: a compiled main() -> Result[(), IoError] returning Err printed 'Error: 0', f"{io_err}"/println(io_err) printed a placeholder, and a payload-variant match couldn't bind its payload ('Undefined variable m'). Root cause was deeper than the tracker's 'imprecise seeded tags' hypothesis: IoError was NEVER seeded - no codegen layout at all (not in seed_builtin_enum_layouts, not in the user program_snapshot which excludes baked stdlib). That single gap caused all three symptoms - construction fell to an i64 0 placeholder, the payload match couldn't bind, and emit_enum_display_fn had an empty variant set. Companion: the bare variant 'Other'/'PermissionDenied' collide across IoError/Utf8Error/TcpError/TlsError, so qualified construction+match picked a wrong tag by HashMap order. FIXED: (1) seed the IoError layout in src/codegen/declarations.rs::seed_builtin_enum_layouts (mirrors the Utf8Error Other(String) template - 4-word struct, tags NotFound=0..Other=6, Other drops VecOrString); (2) emit_enum_display_fn falls back to crate::prelude::STDLIB_PROGRAMS for variant names+kinds when the enum isn't in program_snapshot; (3) new baked_display_enum_names set (populated once from STDLIB_PROGRAMS via extract_derived_traits) re-admits Display-deriving baked enums in expr_user_enum_name_any despite their seeded status so the f-string/println dispatch reaches the generic Display path; (4) honor the qualified Enum.Variant path in construction (try_compile_enum_variant gains enum_name_override, passed by the qualified compile_assoc_call constructor; refinement Ok/Err pin Result) and in match (compile_pattern_condition Binding/TupleVariant/Struct arms prefer variant_pattern_enum_and_tag). Latent until phase-8 entry-point Slice C's C2 made baked error enums Display-eligible. Regression: tests/codegen.rs::e2e_baked_stdlib_enum_display_and_qualified_match. Surfaced by the entry-point Slice C recon (phase-8-stdlib-floor.md line 675). | 134cb8b9 |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
