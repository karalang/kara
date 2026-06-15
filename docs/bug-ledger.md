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

_Generated from `bug-ledger.jsonl` by `scripts/bug-curve.py` — **84 surfaced · 0 open · 84 fixed** (2026-05-20 → 2026-06-14). Do not edit this block by hand; edit the ledger and regenerate._

### Open (0)

_None — the ledger is fully drained._

### Fixed (84)

<details><summary>84 fixed — the regression test is the durable artifact; prose lives in each owning phase tracker</summary>

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
| B-2026-06-09-2 | interp | low | Interp map-heavy drift RESOLVED: bench 333B->89.9B (3.7x via B-07-4 borrow fix); small-N +13.5% won't-chase | — |
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
| B-2026-06-14-17 | codegen | high | wasm_browser --features wasm-threads: ANY parallel program (TaskGroup.spawn/par{}) DEADLOCKS in a real browser - the canvas/output never updates. Node (worker_threads) works, so it passed E2E and looked shipped. Root cause is in the emitted threaded glue (src/wasm_glue.rs GLUE_STATIC_BODY): wasi.thread-spawn created the pthread Web Worker FROM INSIDE the calling worker (`new Worker(new URL('#kara-thread-worker', import.meta.url))` in karaThreadWorkerMain), and the caller then immediately blocks in join()/recv() on memory.atomic.wait32. In browsers a nested dedicated worker's module script is fetched on its CREATING agent's event loop; a blocked creator never turns its loop, so the sibling's script never loads (observed in headless Chrome via CDP: 91 worker targets created with url='', none ever ran their module top-level). Every spawned task sits unclaimed, join() blocks forever, and the scheduler keeps spawning workers chasing an idle one -> hard deadlock. The glue's own comment claimed 'the classic PROXY_TO_PTHREAD model' but only proxied the PROGRAM to a primary worker, NOT thread CREATION. FIXED by completing the model (Emscripten-style): route every worker's thread-spawn to the always-live MAIN thread via a shared spawn-request ring (SPAWN_CTL SAB + requestMainThreadSpawn producer + startSpawnService main-thread drain that calls spawnKaraWorkerSync); thread-spawn still allocates+returns the tid synchronously (wasi-threads ABI) and only defers Worker construction. node keeps spawning siblings directly (nodeWorkerThreads branch) - no such constraint there. Verified end-to-end in headless Chrome: examples/fathom now renders the Mandelbrot at ~120fps across 18 cores (132859/153600 non-black px, 127 colors). Surfaced building the Fathom dogfood demo (it just showed a black screen). NOT browser-CI-testable without a headless browser harness (node path can't reproduce it). Regression: src/wasm_glue.rs unit test threaded_glue_renders_pick_constants_and_machinery asserts the spawn-proxy machinery is emitted. | 1d73edbb |
| B-2026-06-14-19 | codegen | med | StringSlice v1: slice/find + source-pinned by-value -> StringSlice (first_word); split-views stay v2 | — |
| B-2026-06-14-18 | typecheck | med | ref/mut-ref String stdlib methods degraded to Type::Error (find/slice unrouted); unwrap_or fell through | — |
| B-2026-06-14-20 | codegen | med | write_console panic-free (try_with + realloc buffers) restores lean binary floor; @fwrite test->write_console | — |
| B-2026-06-14-21 | codegen | high | A body-local owned heap `let` (Vec/String/Map/Set/Slice/array element) declared inside a for-over-COLLECTION loop leaks every iteration but the last. Only compile_for_range[_with_step] pushed a per-iteration scope-cleanup frame; the eight collection for-variants (compile_for_vec_var/slice_var/map_var/set_var/array_var/array_values/string_chars_inner/string_bytes_inner in src/codegen/control_flow_for.rs) called compile_block(body) with NO frame, so each body-local binding registered its FreeVecBuffer/drop in the enclosing FUNCTION frame and only the FINAL iteration's value was freed at the function tail - N-1 iterations' worth leaked (confirmed via `leaks --atExit`: 49999/50000). Surfaced as a hard browser OOM in the Fathom dogfood demo: render_frame's `for handle in handles { let chunk = handle.join(); for byte in chunk {...} }` leaked the joined Vec[u8] (~76KB) every frame, exhausting the wasm32 1GB linear-memory ceiling after ~550 frames (__karac_alloc_or_panic64 -> std::process::abort -> unreachable). NOT join-specific and NOT wasm-specific (native RSS climbed 175->461MB on the repro; wasm just hits a hard ceiling); for-over-range was already correct, which is why only collection loops leaked. FIXED: a shared compile_loop_body_with_cleanup(body, continue_bb) helper that pushes a per-iteration frame, compiles the body, and on normal fall-through drain_top_frame_with_emit()s before the back-edge branch; a body terminator (break/continue/return) pops without emitting (the loop_stack cleanup_depth walk already drained it). Wired into all eight collection variants (array_values inlines the drain since it is straight-line-unrolled, string_chars_inner inlines to preserve its byte_offset store). Regression: tests/memory_sanitizer.rs::asan_for_over_collection_body_local_no_leak (Linux-CI LSan is the leak gate; mac checks no double-free/UAF from the added cleanup). Verified: leaks 0 (was 49999); Fathom soaks 1750 frames @120fps no OOM (was ~550); memory_sanitizer 246/0. | 9a7920c6 |
| B-2026-06-14-22 | codegen | high | wasm-threads browser builds: the WASI-preview1 polyfill's fd_write and random_get (emitted in src/wasm_glue.rs GLUE_STATIC_BODY) pass a SharedArrayBuffer-backed Uint8Array view straight to TextDecoder.decode / crypto.getRandomValues, which BOTH reject shared-backed views ('The provided ArrayBufferView value must not be shared.'). So ANY stdout/stderr write from a threaded browser program - a print, a panic, or the alloc-error abort path - throws a TypeError inside the polyfill, turning a benign diagnostic into a fatal worker error (it masked the real OOM message of B-2026-06-14-21 in the Fathom dogfood until fixed). node (non-shared buffer + fs.writeSync path) is unaffected, so the node E2E never caught it. FIXED: fd_write decodes a .slice() copy (fresh non-shared ArrayBuffer); random_get fills a non-shared scratch Uint8Array via crypto.getRandomValues then .set()s it into linear memory. Verified end-to-end in headless Chrome (Fathom prints/aborts with no 'must not be shared' TypeError). Regression: emit-level needles in wasm_glue::threaded_glue_renders_pick_constants_and_machinery. FOLLOW-UP: 69c49ec0 MISSED the exported readString() helper - the single string-decode funnel used by the threaded main-thread host-service ctx.readString (a host fn taking a string arg, line ~1395) and the rich string-export lift (karaLift case 4) - which has the identical shared-view bug (latent: Plume/Fathom take no string host-fn args so it never fired there; node's lenient TextDecoder hid it). Fixed by .slice()-copying inside readString too, covering all 3 call sites at once; regression needle added to the same wasm_glue test. | 69c49ec0 |
| B-2026-06-14-23 | codegen | med | Vec/String.with_capacity miscompiled on wasm32 with an i32 count (.len()-derived) — i32*i64 byte-size multiply + i32-into-i64 cap field/alloc param emit invalid IR; latent until loop-bound pre-sizing injected with_capacity(<i32 bound>) | a55f17c1 |
| B-2026-06-14-24 | runtime | low | karac-runtime clippy-red under `cargo clippy --all --all-targets -- -D warnings`: `clashing_extern_declarations` on `realloc`. runtime/src/lib.rs's OutputCapture allocator hooks (added by aabc01c8, B-2026-06-14-20 panic-free write_console) declared `extern "C" fn realloc(ptr: *mut c_void, size) -> *mut c_void` while runtime/src/alloc.rs already declares `fn realloc(ptr: *mut u8, size) -> *mut u8` for the SAME libc symbol — two mismatched decls in one crate trip the lint. NOT cfg(test)-gated, so it also breaks CI's `cargo clippy --all -- -D warnings`. Exactly the '--all-targets not --tests/--all' coverage class CLAUDE.md flags. FIXED: align lib.rs's decl to `*mut u8` (matching alloc.rs — same byte allocator) and drop the now-redundant `as *mut c_void`/`as *mut u8` casts at the two call sites; `free` keeps `*mut c_void` (alloc.rs doesn't declare it, so no clash). Surfaced running the events.wheel slice's clippy gate. | 34ce3f4d |
| B-2026-06-14-25 | codegen | med | Map/Set returned BY VALUE from a call and bound (`let m2 = make_map()`) leaks the handle on Linux LSan (silent on macOS — no LeakSanitizer there). The binding registered its dispatch side-tables (`m2.len()` compiles) but the let-path fresh-handle gate that queues `FreeMapHandle` only matched `clone`/`union`/`intersection`/`difference` MethodCalls, not a plain `Call`. The callee suppresses its own free on the move-out `return m;`, so with no caller-side track the handle leaks. FIXED: extend the gate to an owned-returning `Call`, excluding borrow-returning callees (`fn_ref_return_inner`, e.g. `ref Map` accessors). Latent since the call-sourced-Map-bind+method feature landed this window; authored macOS-only so LSan never saw it. Reddened CI memory-sanitizer (asan_returned_map_explicit_return_no_double_free). | ae9aa79d |
| B-2026-06-14-26 | codegen | med | Bare tuple over a `Map.new()`-created var (`let t = (d, i)`) leaks the Map leaf on Linux LSan. The #23 Part A tuple drop never registered because `compile_map_new_stmt`/`compile_set_new_stmt` never recorded the binding's surface type name, so `type_name_of(d)` returned None, `infer_arg_elem_te` produced an empty `Path`, and `type_expr_has_drop_heap` was false — the tuple drop was skipped. The place-source path (`let mm = s.m`) already records "Map" via `record_var_type_name`. FIXED: mirror that recording in the two `*_new_stmt` builders. The tuple-var-moved-into-struct case (`pair`→`Hi{m:pair}`) is balanced by the existing Part C1 move-out suppression (verified no double-free under macOS ASAN). Reddened CI memory-sanitizer (asan_struct_tuple_map_leaf_no_double_free). | ae9aa79d |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
