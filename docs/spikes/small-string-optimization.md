# Spike: Small-String Optimization (SSO) for the runtime `String`

**Status:** 🟡 **Slice 1 landed** (layout + accessors + free-gate hardening). Its
follow-up claimed every String buffer-free/realloc gate was inline-safe —
**Slice 2 measured that claim FALSE**: 16 gates were still unsigned `UGT`, and 14
were fixed in `3833ff8`.

**Slice 2 inline construction is LIVE behind `KARAC_SSO=1`, default OFF.** It
works and it is correct on every surface probed. Both construction sites are
inline — `s[a..b]` and `String.substring`.

**THE MOTIVATING WORKLOAD NOW WINS 14.8%, AND THE FIX IS LANDED. 2026-09-12.**
Slice 2's own exit gate — re-profile the self-hosted lexer — had never been run;
every payoff figure below it was synthetic. Run on
441 KiB of real Kāra it first showed SSO **4% SLOWER**, and attributing that
regression to a single function exposed the cause: one `cap > 0` ownership gate
in `emit_vecstr_defensive_copy` still read UNSIGNED, so every inline String was
deep-copied back onto the heap at each by-value consuming call — re-spending the
`malloc` construction had just avoided. Flipping it to `SGT` measures:

| 441 KiB × 200 passes | allocations | instructions | wall, best-of-7 |
|---|---|---|---|
| `KARAC_SSO=0` | 16,292,811 | 9,190,806,670 | 1484 ms |
| `KARAC_SSO=1` before | 11,232,411 | 9,507,462,371 | 1524 ms |
| **`KARAC_SSO=1` after** | **5,128,411 (−69%)** | **7,173,722,614 (−21.9%)** | **1265 ms (14.8% faster)** |

That gate survived Slice 2's sweep because `rustfmt` puts the predicate and its
`cap` operand on different lines, so no line-oriented grep could match it.

**THE GATE WAS MASKING A CLASS OF LATENT BUGS, AND THAT IS THE OTHER HALF OF THE
STORY.** By de-inlining every String at every by-value consuming call, the
unsigned gate kept inline descriptors out of most of the compiler — every site
downstream of it had been silently exempted from supporting them. Flipping it
reddened **eight binaries** on the first attempt. The de-masking then converged
in **two** fixes, not the open-ended list it looked like:

1. an enum-payload store made through the read-only accessor (`param_own.rs`),
   which wrote a spill-slot address into a payload word; and
2. the mutation chokepoint's Vec guard (`vec_method.rs`), which tested
   `!vec_elem_types.contains_key(v)` on the false belief that the table names
   `Vec` receivers — it holds Strings too, so `push_str` on a pattern-bound
   String skipped promotion entirely.

All seven selfhost differentials pass at `KARAC_SSO=1` with all three changes in. See "SLICE 2'S OWN GATE
FINALLY RAN" below for the profile, the four hypotheses it falsified first, the
latent bug the flip exposed, and the near-miss where a stale `karac` nearly got
the whole thing reverted as a no-op.

**The synthetic transient/retained table below is now STALE and must not be
quoted.** Every number in it was taken with that unsigned gate live, so all of
them understate SSO — the retained rows most of all, since a retained String is
precisely what gets deep-copied at a consuming call. Re-run before citing.

This doc is the campaign's living handoff: layout decision (settled), staged slice plan, the tag-aware
accessor work list, and the verification matrix. Scoped 2026-06-12; Slice 1 landed
2026-07-09; Slice 2 construction 2026-09-11; Slice 3's FFI boundary opened
2026-09-12.

**The default flip has a second blocker as of 2026-09-12, and it is not about
speed.** `UseAfterMove` is advisory *because `cli.rs` promises the binary is
memory-safe anyway*; at `KARAC_SSO=1` a moved-from short String reads empty in
one measured shape and **SIGSEGVs** in another, so the flip would falsify that
promise. Programs with no ownership diagnostic are unaffected. The cause is not
yet traced — see "A NEW BLOCKER FOR THE DEFAULT FLIP" under the Slice 3 entry,
which records what was measured and what is still only a hypothesis.

**CI now runs one `KARAC_SSO=1` fixture** —
`tests/cli.rs::test_sso_inline_string_survives_the_env_ffi_boundary`, added with
the Slice 3 entry below. Before it, no COMPILED Kāra program in the suite ever
emitted an inline descriptor — the runtime's own unit tests build them through
`new_inline`, but nothing exercised codegen's construction path — so every SSO
regression to date reached `main` through a green gate set and was caught only
by a human remembering to run the second leg by hand.
One fixture is not coverage; it is the first one, and the pattern it establishes
(shell out to `karac` so the env var is per-fixture, and build the String with
`substring` so it is actually inline) is what the rest of the surface should
copy.

**Layout decision — SETTLED (Slice 1):** Option A, **inline flag = sign bit (bit 63) of
`cap`**. Three states discriminated by `cap` read as `i64`: static-heap (`cap == 0`),
owned-heap (`cap > 0`), inline (`cap < 0`). Encoding the flag as the sign bit is
load-bearing — it collapses the buffer-free decision to the single signed compare
`cap > 0`, which is a *provable no-op today* (no code has ever produced a `cap` with
bit 63 set; a real capacity never approaches 2^63) yet is forward-correct for inline.
Full folly-`fbstring`-style 23-byte inline overlay: bytes `0..=22` hold data, byte 23
(`cap`'s MSB) holds `flag | length`. The **executable contract** lives in
`runtime/src/sso.rs` (exhaustively unit-tested; the single source of truth codegen
mirrors); the codegen tag helpers are in `src/codegen/sso.rs`.

## Why this campaign

Profiling the self-hosted lexer ([`selfhost-lexer-profile.md`](selfhost-lexer-profile.md))
found per-token **allocation** is the #1 remaining codegen-perf cost. After the
string-literal `match` dispatch lever shipped (commit `5adf2e90`, 111.7 B → 66.9 B
instructions, Rust gap **4.58× → 2.74×**), `malloc`/`free` is now the **#1 self-time
leaf**. The dominant source: per-token `substring` (`selfhost/src/main.kara:1239`,
`:1260`, …) returns an **owned `String` copy**. Most lexemes — identifiers, keywords,
short tokens — are **short** (< ~23 bytes), so they fit inline.

**SSO is the corpus-wide lever.** Inline short strings directly in the `{ptr,len,cap}`
struct → **no `malloc` when `len ≤ N`**. Unlike a lexer source rewrite (which only fixes
the lexer), SSO makes *every* short-string allocation in *every* Kāra program disappear —
the principled "natural `substring` code stays fast" fix that matches the project's
fix-the-compiler-not-the-workload rule. It is the lever to **close the gap and go
further** (the user's framing, 2026-06-12: "close it anyway or go further — only question
is now or later").

**Why later, not now (the real reason).** SSO has the **largest blast radius of any
change in the String subsystem**. It re-lays-out the struct that *every* String op
assumes; a subtle layout miscompile is *silent data corruption* — exactly the failure
class the guardmalloc/LSan discipline exists to catch. It deserves a fresh full context
window and deliberate staging, not a long-session bolt-on. "Later" is cheap **because
this doc preserves the warm context.**

## The central constraint (settle the layout around this)

`String`, `str`, `Vec`, and `VecDeque` **all share one LLVM struct** —
`vec_struct_type()` = `{ ptr: *u8, i64 len, i64 cap }` (24 bytes), defined at
`src/codegen/types_lowering.rs:337`. Confirmed shared at e.g.
`types_lowering.rs:1239`, `declarations.rs:4318`, `control_flow_match.rs:1654`.

**SSO must not change `Vec` semantics.** Therefore: **encode SSO *within* the existing
24-byte struct via a tag — do not split `String` into its own type.** A uniform
tag-aware data-ptr accessor is then *correctness-safe for `Vec` too*: `Vec` never sets
the inline tag, so the accessor always takes its heap path for a `Vec` and behaves
identically to today. (Threading the Kāra-level type to keep `Vec` on the branch-free
raw path is a *perf* refinement, not a correctness requirement — see Slice 3.)

### Layout decision — DECIDED (Slice 1): Option A, flag = sign bit of `cap`

- **Option A — in-struct tag (CHOSEN).** Reuse the 24 bytes. Inline form stores up
  to 23 bytes of data overlapping the `ptr`/`len`/`cap` words (folly `fbstring` style),
  with **bit 63 of `cap` (the sign bit)** as the inline flag. `Vec` leaves the flag clear.
  Minimal type churn — String stays `vec_struct_type` everywhere.
  - *Why the sign bit of `cap` and not the low bit of `ptr` or a bit of `len`:* the low
    bit of `ptr` is unsafe (a `.rodata` string-literal buffer is not guaranteed ≥2-byte
    aligned), and overlapping `len` would break the invariant that a `cap`-only signed
    compare distinguishes all three drop states. The sign bit of `cap` gives one predicate
    — `is_owned_heap ⇔ (i64)cap > 0` — that is simultaneously (a) a no-op today, (b)
    correct for inline (`cap < 0` skips free), (c) correct for static (`cap == 0` skips),
    and (d) identical to the old `UGT` gate for `Vec` (whose cap is a non-negative count).
- **Option B — split `String` into a distinct LLVM type.** Cleaner semantics but enormous
  churn: String currently *is* `vec_struct_type` across ~15 files, the by-value ABI, the
  recursive-drop type-identity checks (`llvm_ty_is_vec_struct`), and dispatch. **Rejected.**

### Hazard: the `cap == 0` "static literal, don't free" convention

Today `cap == 0` marks a String whose buffer is static `.rodata` (string literals;
`StringLit` at `exprs.rs:60`; the dispatch literal-pattern builds `cap_zero` at
`control_flow_match.rs`). Drop frees only `cap > 0`. SSO adds a **third state**, so the
encoding must distinguish all three cleanly:

| state | meaning | drop action |
|---|---|---|
| static-heap (`cap == 0`, flag clear) | literal, buffer in `.rodata` | none |
| owned-heap (`cap > 0`, flag clear) | malloc'd buffer | `free(ptr)` |
| **inline (flag set)** | bytes live in the struct | none (no buffer) |

## Work list — the tag-aware-accessor surface

Raw field-0 data-ptr reads (`extract_value(_, 0)` / `build_struct_gep(vec_ty, _, 0)`) and
field-1/2 len/cap reads, spread across ~15 codegen files. Counts (field-0 reads, `grep`
2026-06-12) as a scale guide — **not all are String; many are `Vec`** (uniform accessor
is safe for both):

```
vec_method.rs   79    runtime.rs      23    clone_drop.rs   22
http.rs         15    method_call.rs  13    expr_ops.rs     13
assoc_call.rs   11    reduce.rs        9    collections.rs   8
control_flow_for 7    tcp/synth_display/file 6    tls 5    exprs 4
```

Find the full set with:
`grep -rn "extract_value.*, 0\|extract_value.*, 1\|build_struct_gep(vec_ty" src/codegen/`.

**Must-not-miss sites:**
- The **string-match dispatch tree** just shipped (`control_flow_match.rs`,
  `emit_string_dispatch` / `emit_len_bucket` / `emit_byte_group`) reads
  `extract_value(sv, 0/1)` raw for ptr/len → route through the accessor.
- **Drop/clone** (`clone_drop.rs`, 22 sites) keyed on `cap > 0` → become tag-aware
  (inline ⇒ no free; inline clone ⇒ struct copy, no buffer alloc).
- **`Vec.push` inline grow** (`vec_method.rs:690`) — Vec path, must stay raw/unaffected;
  good canary that the accessor is a true no-op for Vec.
- **Runtime/FFI by-value ABI** (`runtime/src/*`, plus codegen `runtime.rs`, `file.rs`,
  `http.rs`, `tcp.rs`, `tls.rs`, `json.rs`): any runtime fn receiving a String by value
  (`println`, file write, http body, …) must also decode the tag. **Runtime-side change,
  not just codegen.**

## Staged slice plan

Each slice is independently shippable and gated on the full String + ASAN suite; the
perf payoff lands in Slice 2.

- **Slice 0** — *this spike.* Scope + layout-decision criteria. ✅ **DONE.**
- **Slice 1 — layout + accessors (no behavior change).** ✅ **DONE (2026-07-09).**
  Layout settled (Option A, sign bit of `cap`). Shipped:
  - `runtime/src/sso.rs` — the executable encoding contract on `RuntimeKaracString`
    (`is_inline` / `is_static` / `is_owned_heap` / `byte_len` / `data_ptr` / `as_bytes` /
    `new_inline` + `INLINE_CAPACITY = 23`), exhaustively unit-tested (all three states,
    every inline length 0..=23, boundary rejection, layout-pin). This is the single
    source of truth codegen mirrors, and the FFI-decode path Slice 3 will call.
  - `src/codegen/sso.rs` — codegen tag helpers `sso_string_is_owned_heap` (SGT, wired)
    and `sso_string_is_inline` (SLT, ready for Slice 2).
  - **Free-gate hardening:** the six `{ptr,len,cap}` buffer-free gates (`emit_string_drop_fn`,
    `emit_vec_drop_fn` in `clone_drop.rs`; the overwrite-free, enum-payload-free, and live
    `FreeVecBuffer` gates in `runtime.rs`; the enum `VecOrString` payload drop in
    `synth_drop.rs`) now route through `sso_string_is_owned_heap` (`UGT`→`SGT`). Proven
    no-op: full suite + 562 ASAN cases + 2103 codegen E2E + 153 par_codegen all green,
    zero perf delta.
  - **Follow-up gate hardening — GATE HALF landed 2026-07-11 (proven no-op).** The
    remaining String-buffer `was_heap` gates now route through
    `sso_string_is_owned_heap` (`UGT`→`SGT`): the three from-slice builders
    (`tss`/`efs`/`tefs` in `vec_method.rs`) **and** `emit_string_buffer_grow` (the
    `String.push`/`push_str` realloc-or-fresh gate, which the original list missed —
    an inline string must never be `realloc`'d, it must take the fresh-malloc path).
    So **every** String buffer-free/realloc gate is now inline-safe. Verified full
    suite + 590 ASAN/LSan + 2137 codegen E2E + 158 par_codegen green, zero perf delta
    (a real `cap` never has bit 63 set, so `SGT`≡`UGT`). **STILL DEFERRED to the flip
    (the coordinated, only-testable-with-construction half):** each of these gates' memcpy
    *source* (`data`, the raw field-0 load in the grow/fresh path) must become the
    tag-aware `string_data_ptr` — that is coupled to inline construction and can't be a
    no-op. `FreeSoaGroups` (`runtime.rs`) is **NOT** in scope (its `cap` is a SoA group
    count, never a String descriptor). Grow gates comparing `new_len`/`doubled` to `cap`
    are unrelated and must stay `UGT`.
- **Slice 2 — inline construction (the win).** `substring`, runtime-built `StringLit`,
  concat, `to_string`, `push_str` result → build **inline** when `len ≤ 23`. Concrete
  checklist for the fresh session:
  1. ~~Convert the remaining `was_heap` gates to `sso_string_is_owned_heap`~~ —
     **GATE HALF DONE 2026-07-11** (proven no-op; `tss`/`efs`/`tefs` + the missed
     `emit_string_buffer_grow`, so every String buffer gate is inline-safe).
     **Remaining:** fix each gate's memcpy *source* — the raw field-0 `data` load in
     the grow/fresh path (`vec_method.rs` `emit_string_buffer_grow` line ~133; the
     `tss`/`efs`/`tefs` builders' post-grow memcpy) — to `string_data_ptr` (step 2's
     accessor), so an inline source is copied from the struct-self pointer. This is
     the coupled, only-testable-with-construction half.
  2. **Tag-aware `string_data_ptr` / `string_len`** in codegen (mirror `runtime/src/sso.rs`)
     — **SSA FORM + THE DISPATCH TREE DONE (2026-09-11); the slot form and the rest of the
     sweep remain.** The whole SSO surface is now behind **`KARAC_SSO` (default OFF)**, which
     is what makes the remaining sweep landable incrementally: with the gate off each
     accessor emits *literally the same inkwell calls with the same value names* it replaced,
     so an intermediate commit is **byte-identical IR** rather than merely "the branch is
     never taken at runtime". That distinction is load-bearing — routing the string-`match`
     dispatch tree through a live `load`+`icmp`+`select` would put a cost on the hottest
     String read in the corpus (the `5adf2e90` lever) for every commit between here and the
     flip, with no malloc saving yet to pay for it. `KARAC_SSO=1` turns the whole thing on,
     which is how the sweep is tested ahead of the default flip.
     Shipped: `sso_string_parts_from_value` (the SSA form — spills to an **entry-block**
     alloca so the inline self-pointer has an address; entry placement is load-bearing twice
     over, the address is stable for the whole function and a call site inside a loop
     allocates once rather than per iteration), `sso_select_len` (decodes the inline length
     from `cap`'s high byte with a **logical** shift — a sign-extending one smears the flag
     bit into the length) and `sso_load_cap`. Step 4's dispatch tree
     (`emit_string_dispatch`) is routed through it.
     **The sharp edge the rest of the sweep must respect:** the returned pointer is valid
     only for an *immediate* read (a memcpy source, a comparison, a runtime call that
     copies). An inline descriptor is self-referential, so storing that pointer into
     anything outliving the frame dangles as soon as the value is copied elsewhere.
     Verified: a string-`match` program built at `KARAC_SSO=0` and `=1` produces **identical
     output** but **different binaries** — the gate is real (the on-path is not dead code)
     and behaviour is unchanged (no inline strings exist yet, so the tag select always
     yields the heap value). Full `--features llvm` suite green **both ways**: 16,739 tests
     across 109 binaries, `codegen` 3772/0 and `memory_sanitizer` 1589/0, with `gpu_e2e` the
     only red binary (the sanctioned missing-optional-archive state).
     Original note, still describing the remainder:
     a *slot* form (GEP field-0 address for inline, load field-0 for heap) is clean; a
     *value* (SSA) form must **spill to an alloca** to take the inline self-pointer — this
     is the main new complexity. Sweep the field-0 (data-ptr, ~224 sites) and field-1
     (len, ~204 sites) reads *on Strings* onto these (many are `Vec` — the accessor is a
     safe no-op there, but threading the Kāra type to keep `Vec` branch-free is Slice 3).
  3. ~~**Clone becomes tag-aware:** inline source ⇒ struct copy, no malloc~~ —
     **DONE (2026-09-11, proven no-op).** Both String-clone entry points now branch on
     the tag *first*, before any field read: the runtime `karac_string_clone`
     (`runtime/src/clone.rs`) and codegen's fallible `emit_string_try_clone_fn`
     (`clone_drop.rs`). An inline source is copied as a verbatim 24-byte descriptor —
     the bytes travel inside it and the destination's self-pointer re-derives from its
     own address — so nothing is allocated. `clone.rs` dropped its private duplicate of
     the `{ptr,len,cap}` layout and now aliases `RuntimeKaracString`, which is what
     carries the `sso.rs` accessors; a second accessor-less copy of the layout is how
     the two halves would drift. The two `EQ cap, 0` sites in `emit_vec_clone_fn` /
     `emit_vec_try_clone_fn` are **Vec** clones and stay as they are (a Vec never sets
     the flag); the String path never reached them. Regression tests
     (`clone_of_inline_source_is_a_struct_copy`,
     `inline_clone_data_ptr_follows_the_destination`) feed `karac_string_clone` an
     inline source built by the reference encoder `new_inline`, which is what makes the
     runtime half testable *before* construction exists — verified to FAIL with the new
     branch disabled, so they discriminate. The codegen half has no such handle and is
     only exercisable once construction lands.
  4. ~~Route the **string-match dispatch tree** through `string_data_ptr` + `string_len`~~
     — **DONE (2026-09-11)** as part of step 2: `emit_string_dispatch`'s two raw
     `extract_value(sv, 0/1)` reads are now the single `sso_string_parts_from_value` call.
     `emit_len_bucket` / `emit_byte_group` take the decoded ptr+len as arguments from it, so
     they needed no change of their own.
  Gate: **re-profile the self-host lexer** (instruction count + `malloc` leaf share must
  drop), full ASAN + **Linux/LSan** (SSO touches every free path — authoritative leak
  gate).
  **RAN 2026-09-12. Fails as the tree stands; passes with a change that is not
  yet landable.** First run: the `malloc` share dropped as predicted (31% fewer
  allocation calls) but the instruction count went **UP** 3.4% and wall time
  4.0% — a conjunction with only one half passing. Attributing that failure found
  an unswept unsigned `cap > 0` gate; flipping it passes both halves
  (allocations **−69%**, instructions **−21.9%**, wall **14.8% faster**) and
  reddens eight binaries, because that gate was masking latent inline bugs. See
  "SLICE 2'S OWN GATE FINALLY RAN" below.
- **Slice 2 progress — INLINE CONSTRUCTION IS LIVE behind `KARAC_SSO=1` (2026-09-11).**
  `s[a..b]` now builds an inline String when the slice fits the 23-byte overlay,
  allocating nothing. Default remains OFF; the read sweep is ~1/3 done.
  - **The encoder lives in the RUNTIME, not codegen** — a deliberate departure from
    this doc's original sketch. `karac_string_slice_into(data, len, start, end, out)`
    validates through the same `slice_validate` as `karac_string_slice` and writes a
    complete descriptor; codegen calls it and loads the 24 bytes back, owning no
    encoding at all. Two reasons: (a) the allocating path does bounds AND UTF-8
    char-boundary checking, both fatal, and an inline fast path in codegen that
    skipped them would have been a silent soundness regression; (b) `new_inline` is
    already the exhaustively-tested source of truth, and re-emitting the byte-packing
    as IR would be a second implementation free to drift — a codegen↔runtime layout
    mismatch is exactly the silent corruption this campaign's staging exists to
    prevent. It is not a new call either: the allocating path already made one. The
    win is removing the *malloc*.
  - **MUTATION PROMOTES rather than going tag-aware** (`sso_deinline_in_place`). A
    mutating op reads `len`/`cap` raw and repeatedly — growth test, destination
    offset, aliasing rebase, post-copy length store. The failure was QUIET, not loud:
    `needs_grow` is `UGT(new_len, cap)`, an inline `cap` is negative, so as unsigned
    it is enormous, the grow is SKIPPED, and the copy lands past the end of the
    descriptor. Promoting once at the head leaves every read downstream byte-for-byte
    the pre-SSO code. A short mutated string loses its inline win, which is the right
    trade — the corpus's short strings are overwhelmingly *read* — and it is what
    folly's `fbstring` does.
  - **SLICE 1'S "EVERY String buffer-free gate is now inline-safe" CLAIM WAS WRONG.**
    16 `is_heap` gates were still `UGT(cap, 0)`; an inline `cap < 0` reads as an
    enormous unsigned, so each would have freed the DESCRIPTOR'S OWN ADDRESS. 14 are
    now `SGT` (the 2 SoA group-count gates stay, per this doc's own exclusion). Five
    IR-assertion tests in `tests/codegen.rs` pinned the old `icmp ugt` text and were
    updated, with a note at the assertions so nobody "restores" `ugt`. **Consequence
    for the campaign's staging rule:** this commit is therefore NOT byte-identical IR
    at SSO=off — it is *semantically* identical with 14 predicates flipped.
  - **A latent UB in Slice 1 was found and fixed:** `RuntimeKaracString::as_bytes`
    called `from_raw_parts(null, 0)` for the canonical empty String `{null, 0, 0}`,
    which is UB at length zero and aborts under the debug precondition check. It had
    only test callers, so it would have become live the moment Slice 3 wired it into
    the FFI decode.
  - **The sweep was PROBE-DRIVEN, and that is the transferable lesson.** Each
    `KARAC_SSO=1` crash named exactly one unswept site — far more reliable than
    eyeballing ~430 candidates. It is also how the *class* boundary was found: a
    mechanical rewrite of 119 ptr+len PAIRS missed `String.len()`, which is a
    **len-only** read, and `f_str_slice` silently returned 0 instead of 3
    (`par_codegen::test_e2e_autopar_joined_range_slice_binding`).
  - **Remaining sweep (superseded by the round-2 entry below — read that first):**
    108 `struct_gep(vec_ty, _, 1)`, 57 `extract_value(_, 1)`, 69
    `extract_value(_, 0)`. Round 2 re-counted these with a balanced-paren scanner
    and classified them: of 88 field-1 READ sites on the shared struct, most are
    `Vec`-only methods (`sort`, `pop`, `retain`, …) that `String` does not have, so
    the real String-relevant remainder is far smaller than the raw count suggests.
  - Gates at this commit: fmt + both clippy legs green; `--features llvm` **SSO=off**
    16,001 passed (red: the load-flaky `signalling_karac_run_does_not_orphan_the_jit_runner`
    and `gpu_e2e`'s missing optional archive); **SSO=on** 16,739 passed, `gpu_e2e` the
    only red binary.

- **Slice 2 round 2 — correctness advanced, and the first PAYOFF MEASUREMENT is
  NEGATIVE (2026-09-11).** The probe now agrees byte-for-byte at both settings
  across three receiver forms. Then the first end-to-end measurement was taken,
  and it says the campaign **as currently built is a net wall-time LOSS on the
  shape it was designed for.** That is the headline; the rest is why.

  - **Routed this round**, each String-only so `Vec` pays nothing:
    `karac_hash_String` / `karac_eq_String` (Map/Set keys — the
    highest-consequence pair, since a wrong digest puts equal keys in different
    buckets where they never meet to be compared), the nested-String display leaf
    in `synth_display` (`Vec[String]` elements, struct fields — distinct from the
    two top-level print arms), `for c in s`, `Secret.ct_eq`, 11 receiver
    `(ptr, len)` pairs in `vec_method.rs`, and the len-family SSA arm in
    `method_call.rs`.
  - **A LENGTH NEEDS NO SPILL.** `cap` is field 2 of the aggregate already in
    hand, so a tag-aware length is a shift, a mask and a select over registers.
    Only a DATA POINTER needs the descriptor's address. This splits the remaining
    sweep by COST, not only by correctness: the 74 `extract_value(_, 1)` sites are
    cheap, the 94 `extract_value(_, 0)` ones are not.
  - **`emit_string_try_clone_fn` was deliberately left alone** — it already
    dispatches on the `cap` tag. A mechanical rewrite would have "routed" it into
    nonsense. Classifying each candidate beat transforming all of them, the
    counter-lesson to the previous round's 119-site mass rewrite.

  ### THE RECEIVER'S FORM SELECTS THE CODEGEN PATH

  The most transferable finding here. `src[0..5].is_empty()` dispatches through
  B-2026-08-18-22's borrowed scalar-reader arm and produces a `{ptr, len, cap = 0}`
  view — **never an inline descriptor**, so it cannot exercise the tag in either
  direction. `let t = src[0..5]; t.is_empty()` reaches the len-family SSA arm in
  `method_call.rs` instead, and THAT one was broken: it answered `true` for a 3-
  and a 5-byte inline String, `false` for 9 and 19 — exactly tracking whether byte
  8 happened to be occupied, because an inline `len` field spans bytes 8..=15 and
  `new_inline` zero-fills before copying.

  It survived a green 28-surface probe, and then survived a first fix aimed at the
  `"is_empty"` arm in `vec_method.rs` — the wrong site. **Testing one receiver form
  certifies one path.** The probe now runs every surface in three: subscript in
  receiver position, `let`-bound owned, and passed across a function boundary.

  Two other probe "findings" this round were the PROBE's bugs, recorded so nobody
  re-derives them: a binding reused across `Vec.push` and `Map.insert` (both take
  ownership) read as an SSO divergence, and `s[a..b].substring(..)` failing codegen
  at BOTH settings, which is B-2026-08-18-22's deliberately deferred half. Neither
  was filed. **Rule: a probe result is evidence about the probe until the probe is
  proven to reach the code it targets.**

  ### The payoff measurement — three workloads, and the answer flips

  > **STALE as of 2026-09-12 — do not quote these numbers.** Every row below was
  > measured with the unsigned `dcopy.owned` gate live, which deep-copied each
  > inline String back onto the heap at every by-value consuming call. All of
  > them understate SSO, the retained rows most of all. Re-run before citing.
  > The mechanism, and what it was worth on the self-hosted lexer, are under
  > "THE LOSS WAS ONE MISSED GATE" below.

  AOT, archives matching `3833ff8`, best-of-N wall time (best-of, not mean: the
  container is noisy and the minimum is the stabler statistic):

  | workload | allocations 0 → 1 | `SSO=0` | `SSO=1` | |
  |---|---|---|---|---|
  | **transient**, 3-byte literal — slice a token, compare, discard, 1M iters | 1,000,009 → 9 | 22 ms | 31 ms | **−41%** |
  | **transient**, 16-byte literal — as above, but both legs call `bcmp` | 1,000,009 → 9 | 21 ms | 25 ms | **−19%** |
  | **retained** — slice a token, push into a `Vec[String]`, keep, 200k iters | 200,012 → 12 | 16 ms | **10 ms** | **+37%** |

  **The premise holds, but only for RETAINED strings.** A short-lived
  `malloc`/`free` pair is a glibc tcache hit — far cheaper than "allocation is the
  #1 self-time leaf" suggests when the buffer is freed in the same loop iteration
  it was made in. Retain the strings so tcache cannot recycle them and the picture
  inverts: 200k allocations become 12, and the workload runs **37% faster**.

  **CORRECTION (2026-09-12).** This entry originally continued: "That matters for
  this campaign specifically, because the self-hosted lexer *keeps* its token
  texts — it does not slice-and-discard." That was read off `lexer.kara`, never
  measured, and it is wrong in the half that matters — see "THE LEXER IS NOT THE
  'RETAINED' SHAPE" below. The lexer is a mix, and the allocation saving and the
  read-path cost do not split along the same line.

  The two transient rows isolate WHY that leg loses. Their only difference is the
  compared literal's length, chosen so the 16-byte row emits `call bcmp@plt` on
  BOTH legs and the 3-byte row does not:

  * **~5 ms of the 9 ms is lost compare folding.** At SSO=0 the 3-byte compare
    folds to `movzwl`/`xor`/`movzbl`/`xor` — four instructions, no call. At SSO=1
    the pointer is a `cmovs` between the heap pointer and the descriptor's own
    address, so LLVM can no longer see where the bytes are and calls `bcmp`.
  * **~4 ms is residual read-path overhead** that survives even when both legs
    call `bcmp`. **A million removed allocations do not pay for it.** So fixing
    the folding alone would NOT make the transient leg a win — it would take it
    from −41% to about −19%.

  The disassembly on the losing leg also showed a second, separate inefficiency:

  1. **The tag-select makes the data pointer OPAQUE to LLVM.** At SSO=0 the 3-byte
     compare folds to `movzwl`/`xor`/`movzbl`/`xor` — four instructions, no call.
     At SSO=1 the pointer is a `cmovs` between the heap pointer and the
     descriptor's own address, so LLVM can no longer see where the bytes are and
     emits `call bcmp@plt`. An inlined 4-instruction compare became a PLT call; at
     ~25 cycles over 1M iterations that is the right order of magnitude for the
     whole 9 ms. (Not yet isolated — the `lexlong` variant, where both legs call
     `bcmp`, is built and unmeasured.)
  2. **The descriptor makes a pointless round trip.** `compile_string_slice` has
     `karac_string_slice_into` write the descriptor to an alloca, then LOADS it to
     an SSA aggregate; the consumer's `sso_string_parts_from_value` STORES it
     straight back to a second alloca. Six memory ops to return to where the callee
     already put it.

  (That round trip was then fixed and measured to be worthless — see below.)

  ### The landed construction site MISSES the motivating workload

  The self-hosted lexer's per-token copy is `self.src.substring(a, b)`
  (`selfhost/src/lexer.kara:275`, `:513`, `:651`, `:986`, …) — the **method**, not
  the `s[a..b]` subscript. `substring` mallocs inline in codegen (its own clamp +
  `malloc` + `memcpy`), a separate construction site entirely. Measured: the same
  benchmark written with `.substring()` allocates **1,000,046 times at
  `KARAC_SSO=1`**. So the commit that "turned on inline construction" does not
  touch the profile that motivated this campaign.

  ### Consequence for the staged plan

  **Step 5 ("flip the default and prove the payoff") cannot succeed as stated** and
  should not be attempted until the read path stops costing more than the
  allocation it saves.

  **ELIMINATING THE ROUND TRIP WAS TRIED AND IS WORTHLESS — do not redo it.**
  Implemented as provenance read off the IR (if the value is a `load`, reuse the
  pointer it loaded from; bail on any intervening `store`/`call` or a load from
  another block). It worked: the three redundant `mov …,0x18(%rsp)` stores
  disappear from the loop. It bought **nothing** — best-of-7 wall time 31 ms → 32
  ms, and retired instructions went the WRONG way, **162,335,574 → 168,335,584**.
  Deterministic counter, so that is not noise. The change was reverted rather than
  landed: a soundness-critical IR scan for no measured benefit is a bad trade.

  A plausible mechanism, offered as a HYPOTHESIS and not measured: reusing the
  variable's own slot takes the address of `t`, which can block mem2reg from
  promoting it, where the dedicated spill alloca kept the address-taking confined
  to a slot nothing else used. If someone revisits this, test that first.

  **Correction to an earlier draft of this entry**, which claimed the regression
  was "to a first approximation entirely" the opaque pointer. The 16-byte-literal
  row measures that: it is about **half**, and the other half is read-path
  overhead a million removed allocations still fail to cover. Fixing the folding
  is worth doing and is not sufficient.
  1. **Branch rather than select where the pointer feeds a length-known compare**,
     so each arm has a concrete pointer LLVM can still fold. Worth ~5 ms of the
     9 ms on the transient leg; leaves it still ~19% slower.
     **SUPERSEDED 2026-09-12 — do not start here.** On the self-hosted lexer the
     compare-folding effect is real but accounts for **0.9%** of the regression
     (`__memcmp_avx2_movbe`: present at `KARAC_SSO=1`, absent at `=0`,
     2,863,800 instructions of a +316,655,701 total). This lever was sized on a
     benchmark built to isolate it; those proportions do not survive a program
     whose String reads are diffuse.
  2. ~~**Make `substring` a construction site**~~ — **DONE**, see below.
  3. Only then re-measure, and only then consider the default.

  ### `String.substring` is now a construction site (2026-09-11)

  The site the profile actually uses. Same two-shape split as `s[a..b]`:

  > **STALE as of 2026-09-12 — same reason as the table above.** Measured with
  > the unsigned `dcopy.owned` gate live; both rows understate SSO. Re-run
  > before citing.

  | shape | allocations 0 → 1 | `SSO=0` | `SSO=1` | |
  |---|---|---|---|---|
  | transient — substring, compare, discard | 1,000,047 → **47** | 9 ms | 10 ms | −11% |
  | **retained** — substring, push into a `Vec[String]` | 200,012 → **12** | 14 ms | **9 ms** | **+36%** |

  The retained row reproduces the `s[a..b]` benchmark's +37% almost exactly, which
  is the consistency worth having: the win is a property of the SHAPE, not of one
  benchmark.

  **The entrypoint is deliberately narrow, and the reason is the buffer contract.**
  The obvious design — one runtime constructor shared with
  `karac_string_slice_into` — dies on inspection: `String.substring` allocates
  exactly `n` bytes through `karac_alloc_or_panic` with no NUL, while
  `karac_string_slice` allocates `n + 1` through Rust's allocator and
  NUL-terminates. Sharing would force one site's contract onto the other and
  silently change which allocator a buffer came from, which the free path has to
  agree with. So `karac_string_try_inline_into(src, n, out) -> i8` owns only the
  inline ENCODING: it writes `out` and answers 1, or answers 0 and leaves `out`
  alone so the caller keeps its own heap arm. **Returning the verdict rather than
  the threshold is what keeps `INLINE_CAPACITY` out of codegen** — codegen
  branches on the answer and never learns the number.

  **A construction fast path is most dangerous where it makes validation
  skippable.** `substring` does a UTF-8 boundary check (B-2026-08-14-19: an
  unchecked cut inside a codepoint put invalid UTF-8 on stdout and made `karac
  run` and `karac build` disagree about a substring's LENGTH). The inline branch
  sits after that check and takes the same `start`/`end` — measured, not asserted:
  `"日本語".substring(0, 2)` faults identically at both settings and under
  `--interp`.

  ### Turning it on broke the self-hosted compiler, and that is the method working

  The first `KARAC_SSO=1` gate after this change went red on **six** binaries —
  all four self-host tests, the ASAN suite, and the known coroutine flake. Every
  string literal's text came back EMPTY:

      Kāra: "0 7 1 1 STR"      Rust: "0 7 1 1 STR hello"
      Kāra: (str  @0:4)        Rust: (str hi @0:4)

  `push_str`'s **argument** was read with raw `extract_value(src_val, 0/1)`. For
  an inline argument, field 0 is the first eight CONTENT bytes reinterpreted as a
  pointer and field 1 is content bytes 8..=15 — zero for anything under 8 bytes —
  so it appended nothing, silently. The lexer's string-literal path is
  `value.push_str(self.src.substring(run_start, self.current))`, so making
  `substring` inline lit a fuse that had been sitting there since the read sweep.

  **The site scanner's blind spot is why it was missed**, and it is worth naming
  because it is not a reasoning error. The scanner matched
  `build_extract_value(NAME, 0` with `NAME` as `[\w.]+`, which silently skips
  `build_extract_value(src_val.into_struct_value(), 0` — the parens break the
  match. Rewritten to accept an arbitrary aggregate expression, it found exactly
  one more String site of the same shape (`try_push_str`). Two scanner gaps have
  now each hidden a real site; a regex over call syntax is a lower bound, never a
  census.

  **PER-ARM PROMOTION LEAKS BY CONSTRUCTION — it is now a chokepoint.** The
  deeper find: `try_push_str` mutates its receiver and had NO
  `sso_deinline_in_place`. The previous round added promotion to `push_str`,
  `push` and `reserve` one arm at a time and simply missed it, and nothing failed
  until a new construction site made inline values common. Promotion now happens
  once in `compile_vec_method`, keyed off an over-broad list of mutating method
  names: a false positive costs one predictable compare, a false negative costs
  silent corruption. A `Vec` guard keeps that compare off the hot `Vec` paths
  while erring toward promoting whenever the receiver is not positively known to
  be a `Vec` — which is what covers a `ref String` parameter, filtered out of the
  String-typed tables by the `Ref(Str)` gap B-2026-08-18-22 hit.

  After the fix, both legs: **109 binaries, 16,801 passed, zero red.**

  Gates at this commit: fmt OK, both clippy legs green, 109 binaries per leg.
  SSO=off 16,028 passed, 2 red (`signalling_karac_run_does_not_orphan_the_jit_runner`
  = B-2026-09-09-1, and `coroutine_ws_over_tls_concurrent_handlers_all_execute`);
  SSO=on 16,801 passed, zero red. Both reds are on the leg this work cannot affect.

- **Slice 3 — sweep + runtime/FFI decode.** Remaining raw sites; runtime decode
  (`println`/file/http/tls/json); thread the Kāra type to keep `Vec` branch-free for perf.
  Gate: corpus re-bench.

- **Slice 3 progress — the FFI boundary: one gap MEASURED, one REASONED, and
  two different fix shapes (2026-09-12).** A String crossing into a runtime
  extern is the same failure class as the `push_str` argument bug that broke
  the self-hosted compiler, and it fails the same way — silently — because
  field 0 of an inline descriptor is a plausible-looking pointer built from the
  string's own first eight bytes.

  ### The gap that was measured: `extract_string_ptr_len`

  `tls.rs::extract_string_ptr_len` — a helper whose own doc comment says it
  extracts `{ptr, len}` from "a Kāra `String` struct value" — was still the two
  raw `extract_value`s, and it has **nine callers**: TLS listener bind
  (cert / key / addr), TLS client connect (addr / server-name / roots-PEM),
  `env.set` (name, value) and `env.var` (name). `env.set` is the one that is
  cheap to probe, and it is loud:

  ```
  let name = src.substring(0, 18);        // 18 bytes -> inline
  env.set(name, src.substring(19, 28));

  KARAC_SSO=0  ->  short-val
  KARAC_SSO=1  ->  memory allocation of 5715719208697159504 bytes failed
                   ... karac_runtime_env_set (runtime/src/lib.rs:1059)
  ```

  That byte count is not noise, it is **the name's own bytes 8..=15, plus one**.
  `5715719208697159504 - 1` is `0x4F5250564E455F4F`, whose little-endian bytes
  are `"O_ENVPRO"` — exactly `"KARAC_SSO_ENVPROBE"[8..16]`. The `+1` is
  `CString::new` reserving the NUL, which is worth spelling out: the first
  arithmetic on a corrupt length can hide the identity of the corruption, and
  the whole reason this decodes at all is that an inline `len` field *is*
  content.

  Routing the helper through `sso_string_parts_from_value` fixes all nine sites at once and
  is byte-identical IR at `KARAC_SSO=0` — the accessor's off-path is exactly the
  two extracts it replaced, with the same value names. Verified after the fix:
  all five lanes agree on `short-val` — `--interp`, JIT at `0` and `1`, AOT at
  `0` and `1`.

  ### The gap that is NOT routable: `cabi.rs::emit_string_return_area`

  Worth its own entry because **the obvious fix is the wrong one.** This is the
  wasm Component-Model export trampoline, and it *stores* the String's pointer
  into a module-level return area that the component lifter reads **after the
  trampoline has returned**. The accessor's contract is "valid for an immediate
  read only" — an inline descriptor's data pointer is the descriptor's own
  address — so routing it here would trade a garbage pointer for a **dangling**
  one, which is strictly harder to debug. The fix is `sso_deinline_in_place`:
  promote to a heap buffer before reading, restoring exactly the invariant the
  function's contract already assumed ("the string bytes already live in the
  guest's linear memory").

  So the FFI sweep has **two fix shapes**, and the question that picks between
  them is not "is this a String?" but **"does the pointer outlive the frame?"**
  Immediate read ⇒ route. Stored anywhere ⇒ promote.

  **This one is REASONED, NOT MEASURED**, and it says so at the site. A wasm
  component E2E needs `wasm-tools` and the two wasm runtime archives, and the
  container had neither. It is guarded by `sso_on()`, so it is a literal no-op
  on the default leg — but an unverified fix is not a verified one, and the next
  session with a wasm toolchain should run it before believing it.

  ### The remaining raw-site count is NOT a remaining-risk count

  The correction to the previous round's framing, and the transferable half of
  this entry. A balanced-paren scan still reports 92 `extract_value(_, 0)`, 72
  `extract_value(_, 1)` and 245 `build_struct_gep(vec_ty, _, 0)` sites raw,
  which reads as a large outstanding surface. Re-reading them says otherwise:
  `expr_ops.rs`'s field-0 sites are enum tags and overflow-check pairs,
  `synth_display.rs`'s are enum tags and float wrappers, `closures.rs` and
  `par_blocks.rs`'s are closure fat pointers, `tls.rs`'s others are
  `{fd, config}` listener handles, and `method_call_ffi.rs`'s are `CStr` /
  `CString` `{ptr, len}` receivers — a different two-field struct that never
  carries the tag. They share the `{ptr,len,cap}` *syntax*, not its semantics.

  Risk concentrates where a syntax scan does not look: in the **helpers that
  return a `(data_ptr, len)` pair**. Enumerating those found the straggler in
  one grep, where two rounds of site-by-site triage had walked past it.

  **The census that works is on the SIGNATURE, not the name and not the call
  syntax** — `fn … -> (PointerValue<'ctx>, IntValue<'ctx>)`. It depends on no
  naming discipline, and it is a small closed set. Run over `src/codegen`, it
  returns thirteen, of which six touch a String:

  | helper | file | state |
  |---|---|---|
  | `sso_string_parts_from_value` | `sso.rs` | the accessor itself |
  | `str_data_len` | `method_call.rs` | routed |
  | `load_string_data_len` | `vec_method.rs` | routed (`to_uppercase`, `join`, `replace`, `strip_*`, …) |
  | `regex_pattern_data_len` | `method_call.rs` | routed, both the flattened and nested `Regex` shapes |
  | `df_string_parts` | `dataframe.rs` | routed (returns a `Result`, so the signature scan misses it — see below) |
  | **`extract_string_ptr_len`** | **`tls.rs`** | **was raw — 9 callers** |

  The straggler survived because of *where it lives*: nobody auditing String
  behaviour greps `tls.rs`, and its `env.set` / `env.var` callers sit in
  `method_call_ffi.rs`, three files from anything named String. A name-keyed
  grep does find it — but it also misses `load_string_data_len`'s siblings and
  depends on whoever named them, whereas the signature is structural.

  And note the signature scan's own blind spot, recorded so the next person
  does not trust it further than it goes: `df_string_parts` returns
  `Result<(PointerValue, IntValue), String>` and does not match. That is the
  third scanner gap this campaign has hit. **Every mechanical census here has
  been a lower bound**; the value of this one is that it is a *different* lower
  bound from the `extract_value` scan, and the two together left one site
  standing rather than none.

  A useful negative from the same audit: of the three sites the previous round's
  handoff listed as suspected FFI gaps, one — `method_call_ffi.rs:129` — is a
  **false positive** (it is `CString.as_bytes`, not a String), one is the
  measured `tls.rs` gap, and one is the `cabi.rs` site needing the other fix
  shape entirely. One of three was actionable as listed. A suspected-site list
  is a starting point for reading, never a work list to apply.

  ### The first `KARAC_SSO=1` fixture in the tree

  `tests/cli.rs::test_sso_inline_string_survives_the_env_ffi_boundary`. Until
  now **no compiled Kāra program in CI ever emitted an inline descriptor** (the
  runtime unit tests build them via `new_inline`, which is why the *clone*
  half was testable ahead of construction — but nothing drove codegen's
  construction path) — so the entire SSO surface was covered by a two-leg gate
  cycle a human had to remember to run
  with the variable set, which is how both this bug and the `push_str` one
  reached `main`. It has to shell out: `KARAC_SSO` is read once per process
  through a `OnceLock` at codegen time and `tests/codegen.rs` compiles
  in-process, so a fixture there cannot set it per-test and the first reader
  would win regardless. Same constraint, and the same placement, as
  `KARAC_OPT_LEVEL` in
  `test_rc_promoted_param_move_suppression_is_sound_at_o0_and_on_the_jit`.

  It also pins the thing that made the *existing* `env.set` / `env.var` E2E
  fixtures useless here: **a string literal is static (`cap == 0`) and never
  inline**, so `env.set("NAME", "val")` cannot exercise the tag in either
  direction — which is why those fixtures stayed green through the whole
  regression. This one builds its name and value with `substring`, a real
  construction site. Same lesson as the receiver-form finding one round earlier:
  a test that does not reach the code path certifies nothing.

  Two details keep it from going quietly vacuous. Its AOT half skips when the
  runtime archive is absent, like every other build-and-run fixture here — so
  under `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` (which CI's archive-building jobs set)
  it asserts that **both** AOT legs actually built and ran, rather than
  reporting green on two skips. And it was verified to FAIL with the fix backed
  out, so it discriminates.

  ### A NEW BLOCKER FOR THE DEFAULT FLIP: moved-from Strings read WILD, not stale

  Found while bisecting a probe divergence, and it is the most consequential
  thing in this entry — because it is not a footgun the user was warned about,
  it is a **documented guarantee that SSO breaks**. `UseAfterMove` is advisory
  by deliberate design, and `src/cli.rs` states the reason in as many words:

  > `UseAfterMove` — codegen **defensive-copies the reuse, so the binary is
  > memory-safe**; the diagnostic carries a machine-applicable `.clone()` fix
  > precisely because the program compiles and runs. Keeping it non-fatal for
  > `build` is deliberate.

  So `karac check` prints `warning[ownership]` and then `All checks passed.`,
  and the compiler promises the resulting binary is memory-safe. This compiles,
  runs, and is covered by that promise:

  ```
  let a = base.substring(0, 10);
  m.insert(a, 7);          // takes ownership
  println(a);              // warning[ownership], but permitted
  ```

  **What was measured** (and this is the part to trust):

  | program | `--interp` | `KARAC_SSO=0` | `KARAC_SSO=1` |
  |---|---|---|---|
  | one 10-byte source, `Map.insert` then read | `abcdefghij` | `abcdefghij` | *empty*, exit 0 |
  | 10-byte **and** 40-byte sources, both moved then read | both correct | both correct | **SIGSEGV, exit 139** |

  The 40-byte string is the control: over the 23-byte inline capacity, so it
  stays heap on both legs and reads back fine. Its presence is what turns the
  failure from a wrong answer into a crash — which is both why it is in the
  reproducer and why the inline path, not `Map`, is the thing implicated.

  **The mechanism is a HYPOTHESIS, and it does not yet fit both rows.** Move
  suppression disarms a moved-from source by zeroing `cap` — see
  `zero_struct_move_caps`'s contract, "each Vec/String field's `cap` is
  zeroed", with the `_mono` sibling saying `cap`/`len`. For a heap String that
  leaves `{ptr intact, len intact, cap = 0}`, which is exactly the
  static-literal state, so the moved-from read is benign and returns the old
  contents — consistent with both non-SSO columns. For an **inline** String
  `cap` is not spare: it carries the inline flag AND the length, so zeroing it
  should leave `{ptr = content bytes 0..=7, len = content bytes 8..=15}`, a low
  bogus pointer with a large length. That predicts a crash in row 1, and row 1
  prints *empty* instead — which fits `len` being zeroed too, and then does not
  explain row 2's crash.

  So two of the three facts are explained and one is not. **Trace the actual
  disarm site for a bare local String before writing the fix** — those cited
  helpers walk struct FIELDS, and whichever site handles a plain `let` binding
  was not read. A fix aimed at the wrong site would pass this reproducer for
  the wrong reason, which is the failure mode this campaign has already hit
  once (the `is_empty` fix that landed in `vec_method.rs` when the defect was
  in `method_call.rs`).

  So SSO does not merely change what a moved-from read returns. It converts
  *reads stale data* into *reads wild memory* on a program the compiler
  explicitly undertakes to keep memory-safe. Four things follow:

  - **Memory safety of programs with no ownership diagnostic is not affected**,
    and that is worth stating precisely so nobody over-reads this. An inline
    descriptor owns no buffer, and `cap = 0` makes its drop a no-op exactly as
    before — no leak, no double free. The defect is confined to reading a
    moved-from value.
  - **It is a hard flip blocker, and the reason is a contract rather than
    taste.** The `UseAfterMove` guarantee is not "we warned you"; it is "the
    binary is memory-safe". Flipping SSO on by default would falsify that
    sentence for every short String in the corpus, and the sentence would still
    be sitting in `cli.rs` saying otherwise. Either the disarm becomes
    tag-aware or that documented guarantee has to be withdrawn first — and
    withdrawing it means making `UseAfterMove` fatal, which is a language
    decision, not a codegen one.
  - **Not filed as a ledger row, deliberately.** It is reachable only at
    `KARAC_SSO=1`, an experimental gate that is off by default and has never
    shipped on, and this doc is the campaign's canonical tracker — the same
    reason the `push_str` and free-gate defects were recorded here rather than
    in `bug-ledger.jsonl`. It becomes a row the moment a flip is attempted.
  - **The likely fix, once the site is traced.** Disarm an inline source by
    writing the canonical empty descriptor `{null, 0, 0}` rather than clearing
    `cap` (and `len`) in place — a defined `""` for the moved-from read, which
    is also the honest answer, since the value really did move away. Offered as
    a direction, not a patch: the disarm is a family of sites
    (`zero_struct_move_caps`, `zero_struct_move_caps_mono`,
    `zero_enum_payload_caps`, plus the bare-local site above), it lives in the
    move machinery rather than the String subsystem, and the unexplained row 2
    may mean something else is wrong as well. It is not done here deliberately
    — sizing it honestly beats bolting a guess onto an FFI commit.

  The reproducer is the snippet above, doubled — the two rows of the table.
  It lives here rather than in `examples/` on purpose: it segfaults on one leg,
  so it belongs in no corpus that gets run, and it carries an ownership
  diagnostic that every corpus fixture is supposed to be free of.

  ### RUN `karac check` ON THE PROBE BEFORE BELIEVING A DIVERGENCE

  The operational upgrade to last round's "a probe result is evidence about the
  probe until the probe is proven to reach the code it targets." That rule was
  already written down here, and this round produced the **same class of false
  alarm anyway**: a combined probe segfaulted at `KARAC_SSO=1` and ran clean at
  `=0`, which reads exactly like a fresh codegen bug. Every one of its eleven
  surfaces passed in isolation; the divergence came from a binding reused after
  `Map.insert` had taken ownership — the same ownership-reuse mistake the
  previous round recorded, in a new costume.

  What makes this actionable rather than just another warning is that **the
  compiler had already said so**: `karac check` on the probe prints
  `warning[ownership]: value 'a' moved here, used again here`. The check was
  free and was not run. So the pre-flight is now concrete —

  > `karac check <probe>.kara` must be clean of `warning[ownership]` before any
  > `KARAC_SSO=0` vs `=1` divergence is treated as a compiler finding.

  — and the same command is what distinguishes the two outcomes: the clean
  version of that probe (`ffiprobe.kara`, every ownership-taking use given its
  own fresh slice) is byte-identical at both settings across all eleven
  surfaces, which is a real *negative* result for `println`, f-string
  interpolation, `to_uppercase` / `to_lowercase` / `trim`, `len` /
  `char_count`, `contains` / `starts_with`, `i64.parse`, `Map` insert+get,
  `Vec[String]` display, and `+` concatenation.

  ### Still open on this boundary

  - **The borrowed-view lifetime — AUDITED 2026-09-12, safe by enumeration.**
    `karac_string_slice_borrow` returns a pointer *into* its source, and when
    the source is inline that source is the accessor's entry-block spill slot,
    so the view is only valid until the next accessor call reuses that slot.
    The consumer set is small and closed, and every member is an immediate
    read:

    | consumer | what it does with the view |
    |---|---|
    | `calls.rs:172` | B-2026-08-18-22's scalar-reader receiver arm (`len`, `contains`, `starts_with`, …) — reads, returns a scalar |
    | `maps.rs` ×5 | `get` / `contains_key` / `remove` / entry lookups — hashes and compares the bytes |
    | `vec_method.rs:5340` | `push_str`'s ARGUMENT — memcpy source |

    So it is correct today, and correct by enumeration rather than by
    construction: nothing stops a future consumer from storing the view. The
    check when adding one is "does this read the bytes before the next
    `sso_string_parts_from_value` call in the same function?" — and the same
    scan that found the enum-payload store (accessor result reaching a
    `build_store` / `store_enum_word` / `build_insert_value`) is the mechanical
    version of that question.
  - **The wasm return area** fix is reasoned, not measured (above).
  - **Unprobed surfaces**, each of which takes a `(ptr, len)` pair from a String
    and is one `substring`-built argument away from being probed exactly as
    `env.set` was: `serve_https` / `serve_ws_tls`, the HTTP *client* builder
    path, `json`, `interner`, `Regex`, `String.normalize`.

  Gates at this commit: fmt OK; clippy GREEN on both legs; `--features llvm`,
  109 binaries per leg. **`KARAC_SSO=0`: 16,805 passed, ZERO red.**
  `KARAC_SSO=1`: 16,770 passed, one red — `coro_e2e`'s
  `coroutine_ws_over_tls_concurrent_handlers_all_execute`, which is
  B-2026-09-12-1 and not this work (the SSO-off leg of the same tree is clean,
  and that row's two prior reds are on opposite settings of the gate). That red
  was worth more than the green: the row had asked for the assertion's
  `left:`/`right:` counts to be preserved on the next occurrence, and this one
  came back **15/16 — one handler wedged, the server did come up**, which
  settles the question the row was written to separate. The row is updated with
  it and stays open.

- **SLICE 2'S OWN GATE FINALLY RAN, AND THE MOTIVATING WORKLOAD LOSES
  (2026-09-12).** Slice 2's exit criterion was "re-profile the self-host lexer
  (instruction count + `malloc` leaf share must drop)". It had never been run.
  Every payoff number in this doc up to here is synthetic. Run now, on the
  workload the whole campaign was scoped around, SSO is a **4% REGRESSION**.

  **Setup**, following [`selfhost-lexer-profile.md`](selfhost-lexer-profile.md)'s
  method: a snapshot of `selfhost/src/{span,token,lexer}.kara` (never the live
  tree) plus a lex-in-a-loop driver, 441 KiB of real Kāra (the compiler's own
  sources + `examples/`), 200 passes, built sequential (`KARAC_AUTO_PAR=0`).
  Token output is **identical on both legs** (10,639,400) and the binaries
  differ, so the gate is real and correctness holds. Linux x86-64 container —
  **do not compare these absolute numbers against that doc's macOS/M5 figures**;
  only the two legs here are comparable to each other.

  | 441 KiB × 200 passes | `KARAC_SSO=0` | `KARAC_SSO=1` | |
  |---|---|---|---|
  | wall, best-of-7 | **1450 ms** | 1508 ms | **−4.0%** |
  | instructions retired (callgrind) | 9,190,806,670 | 9,507,462,371 | **−3.4%** |
  | heap allocations (valgrind) | 16,292,811 | 11,232,411 | **−31%** |
  | bytes allocated | 2,231,969,346 | 2,210,121,746 | −1.0% |

  Wall and instruction deltas agree in sign and size, so this is **extra work
  executed**, not a cache or branch-prediction artifact. The two allocation rows
  are the campaign's problem in one line: SSO removes **31% of allocation CALLS
  but only 1% of allocated BYTES**, because the strings it captures average
  **4.4 bytes**. Those are exactly the allocations glibc's tcache already serves
  almost free.

  ### Where the instructions went

  | bucket | delta | note |
  |---|---|---|
  | libc allocator (`malloc`/`free`/`_int_*`/consolidate) | **−629,573,607** | the win — 124 instructions per removed allocation |
  | `karac_free_buf` | −85,990,200 | fewer buffers to release |
  | `karac_string_try_inline_into` | +192,436,000 | 38 instructions/call over 5,060,400 calls |
  | Kāra code (`lex_all` + its static locals) | **+929,630,600** | the tag-aware read path |
  | `__memcmp_avx2_movbe` | +2,863,800 | **absent entirely at `KARAC_SSO=0`** |
  | net | **+316,655,701** | |

  The new runtime entrypoint **pays for itself comfortably** — 38 instructions
  to avoid a 124-instruction malloc/free pair. The campaign is not losing on its
  construction site. It is losing on the **read** side: +930 M instructions of
  tag-select spread through the generated Kāra code, against −716 M of allocator
  and free-path work removed.

  ### THE `bcmp` STORY IS TRUE AND IRRELEVANT — a correction to this doc

  Slice 2 round 2 decomposed a 9 ms synthetic regression as "**~5 ms is lost
  compare folding**: the tag-select makes the data pointer opaque, so an inlined
  4-instruction compare becomes `call bcmp@plt`", and filed **branch-not-select**
  as lever #1, "worth ~5 ms of the 9 ms".

  On the real lexer that mechanism is **confirmed in kind and negligible in
  size**. `__memcmp_avx2_movbe` appears at `KARAC_SSO=1` and is *completely
  absent* at `KARAC_SSO=0` — exactly the predicted effect, and a clean natural
  experiment for it — at **2,863,800 instructions: 0.9% of the regression.**

  So branch-not-select is not the lever here. It was sized on a microbenchmark
  built to isolate it (a 3-byte literal compare in a tight loop), and that
  benchmark's proportions do not survive contact with a program whose String
  reads are diffuse. **The cost is not concentrated in one foldable compare; it
  is one select on every String read, everywhere.** A lever that fixes the
  compare sites recovers ~1% of this.

  ### THE LEXER IS NOT THE "RETAINED" SHAPE — a second correction

  This doc claimed "the self-hosted lexer **keeps** its token texts — it does not
  slice-and-discard", and used that to argue the campaign's motivating workload
  sits on the winning side of the split. **That was read off the source, not
  measured, and it is wrong in the half that matters.** The hot path is:

  ```
  let text = self.src.substring(self.start, self.current);
  let token = keyword_or_ident(text);      // takes the String BY VALUE
  ```

  `keyword_or_ident` is a `match text { "fn" => Token.Fn, … }` over **87 arms**.
  A keyword returns a payload-free variant and the String is **dropped** — the
  transient shape. An identifier returns `Token.Identifier(text)` — retained. In
  the 441 KiB input: 41,866 identifier-shaped lexemes per pass, **19.6%
  keywords**, mean length 4.4 bytes, and **100% under the 23-byte capacity**.

  The trap is that the two effects do not split along the same line. The
  allocation saving follows the 20/80 keyword/identifier split; the read-path
  cost falls on **all 41,866**, because every one of them goes through the
  `match` dispatch regardless of which way it resolves. Reading the source tells
  you the first split and hides the second.


  ### Reproducing it

  The harness is small enough to keep here rather than in the tree, and it must
  not be run against the live `selfhost/` worktree (that doc's rule, and another
  session edits it):

  ```bash
  mkdir -p lexprof/src && cd lexprof
  printf '[package]\nname = "lexprof"\nversion = "0.1.0"\nauthors = []\nedition = "2026"\n\n[dependencies]\n' > kara.toml
  cp <kara>/selfhost/src/{span,token,lexer}.kara src/
  cat <kara>/selfhost/src/*.kara <kara>/examples/*.kara | head -c 451584 > input.kara
  # src/main.kara:
  #   import lexer.lex_all;
  #   fn main() with panics reads(FileSystem) {
  #       match fs.read_to_string("input.kara") {
  #           Ok(src) => {
  #               let mut total: i64 = 0;
  #               let mut i: i64 = 0;
  #               while i < 200 { let toks = lex_all(src.clone()); total = total + toks.len(); i = i + 1; }
  #               println(total);
  #           }
  #           Err(_) => { println("READ FAILED"); }
  #       }
  #   }
  for m in 0 1; do KARAC_SSO=$m KARAC_AUTO_PAR=0 karac build && mv lexprof lexprof_sso$m; done
  valgrind --tool=memcheck   ./lexprof_sso$m   # "total heap usage: N allocs"
  valgrind --tool=callgrind  ./lexprof_sso$m   # "I refs:"
  callgrind_annotate --threshold=97 callgrind.out.<pid>
  ```

  Both valgrind counters are deterministic, so they can be trusted from a single
  run and under load; only the wall-time row needs best-of-N.

  **The token total is NOT a correctness check, and treating it as one cost a
  session.** This harness sums `toks.len()`, so it proves the two binaries lexed
  the same NUMBER of tokens and says nothing about their CONTENT. A miscompile
  that drops String payload content keeps the count identical — and drops
  allocations, which reads as a spectacular win. That exact thing happened on
  2026-09-12: a −69%/−21.5% result was recorded from a compiler that was
  rendering `IDENT ab` as `IDENT x\u{fffd}`.

  Two rules, both cheap:

  1. **Gate every perf number on `KARAC_SSO=1 cargo test --features llvm --test
     selfhost_lexer` being GREEN for the exact `karac` that built the benchmark.**
     That differential compares against the Rust lexer token-for-token, which is
     the correctness oracle this harness does not have. Perf numbers from a
     compiler that fails it are meaningless.
  2. **`cmp` the benchmark binaries across any compiler change.** A change that
     does nothing and a change you did not compile look identical from the
     outside, and only one of them is worth reverting — see the near-miss under
     "A near-miss worth keeping".


  ### THE LOSS WAS ONE MISSED GATE — WORTH 14.8%, AND NOW LANDED

  **Everything above this heading is the measurement that found the bug — it is
  correct and worth reading, and its conclusion is superseded.** The profile did
  its job: by attributing the regression to a single function it exposed an
  unswept `cap > 0` gate, and flipping it turns SSO from a 2.6% loss into a
  **14.8% win** on the campaign's motivating workload.

  **The flip is NOT on `main`.** It reddened eight binaries — all seven selfhost
  differentials plus the coroutine flake — because the unsigned gate was masking
  latent inline-descriptor bugs elsewhere. One is fixed below; a SIGSEGV in the
  self-hosted item parser is still open. The numbers here are what the flip is
  WORTH, not what the tree currently does.

  Same harness, same 441 KiB input, same 200 passes, all three verified against
  a `karac` whose selfhost differential passes:

  | | heap allocations | instructions retired | wall, best-of-7 |
  |---|---|---|---|
  | `KARAC_SSO=0` | 16,292,811 | 9,190,806,670 | 1484 ms |
  | `KARAC_SSO=1`, before | 11,232,411 (−31%) | 9,507,462,371 (**+3.4%**) | 1524 ms (**−2.6%**) |
  | `KARAC_SSO=1`, after | **5,128,411 (−69%)** | **7,173,722,614 (−21.9%)** | **1265 ms (14.8% faster)** |

  ### How the profile found it

  DWARF-attributed callgrind named one function: **`Lexer.make_spanned`, 348.6 M
  → 776.6 M instructions**, 46% of the entire Kāra-side regression. Disassembly
  showed why — **187 → 591 instructions, and its `malloc`/`memcpy` call sites
  went 5 → 13.** SSO was *adding* allocation sites to the hottest per-token
  function, which is the opposite of the whole design.

  `make_spanned(token: Token)` takes an owned aggregate by value, so it hits
  `emit_vecstr_defensive_copy` — the deep copy that gives a retaining callee its
  own buffer. Its ownership gate was:

  ```rust
  inkwell::IntPredicate::UGT, cap, i64_t.const_int(0, false), "dcopy.owned",
  ```

  An inline `cap` is negative, so read **unsigned** it is enormous and the gate is
  always true: every inline String was deep-copied onto the heap, re-spending
  exactly the `malloc` construction had just avoided. Construction removed the
  allocation and this handed it straight back, one function later.

  `SGT` sends an inline source down the pass-through arm, where the phi already
  returns the header verbatim. That is correct for the same reason the `cap == 0`
  literal case is — an inline String owns no buffer, so there is no alias to
  defend against, and the descriptor re-derives its data pointer from whatever
  address it lands at. `Vec` never sets the flag, so `SGT` ≡ `UGT` there.

  ### Why it survived every previous sweep

  **The predicate and its `cap` operand are on different lines.** Slice 2's sweep
  found 16 unsigned gates and flipped 14 with line-oriented greps; this one is
  formatted across five lines by `rustfmt`, so no `grep 'UGT.*cap'` could ever
  have matched it. A multi-line-aware census — balance the parens of each
  `build_int_compare(…)` call, then test its operands — finds it immediately, and
  reports it as **the last one in the tree**.

  That is the fourth scanner blind spot this campaign has hit, and the pattern is
  now unmistakable: *every* mechanical census here has been a lower bound, and
  each gap was a formatting accident rather than a reasoning error — a closing
  paren on the wrong line, an aggregate expression instead of an identifier, a
  `Result`-wrapped return type, and now a multi-line argument list. **Write the
  census against the syntax tree, not against lines.**

  Note also what this was NOT. Unlike the 14 gates Slice 2 flipped, this was never
  a soundness bug: the copy path handles an inline source correctly, because
  `sso_string_parts_from_value` hands it the right bytes. It was a pure
  performance bug — and a total one on that path, which is why a predicate nobody
  could see was worth 2.3 billion instructions.


  ### THE FLIP EXPOSED LATENT BUGS — the first is the doc's own rule, broken

  The gate flip alone is **not** the fix. Landing it by itself reddened eight
  binaries: all seven selfhost differentials plus the known coroutine flake, with
  the lexer rendering `IDENT ab` as `IDENT x\u{fffd}` — right length, wrong bytes.

  **This is the entry's real finding.** The unsigned gate de-inlined every String
  at every by-value consuming call, so inline descriptors never reached most of
  the compiler. It was load-bearing by accident — not for correctness of its own
  path, but as a *filter* keeping a whole representation out of code that had
  never been audited for it. Flipping it is therefore not a one-line perf fix; it
  is a de-masking exercise whose size is unknown until it is run. **It ran, and
  it converged in two defects** — see "THE DE-MASKING RAN" below for both and
  for the gate results. The second announced itself as:

  > `selfhost_parser_items` — **SIGSEGV** in the item-parser binary at
  > `KARAC_SSO=1` with the gate flipped, after the payload-store fix below made
  > the lexer, parser and codegen differentials pass.

  and turned out to be the mutation chokepoint's inverted Vec guard.

  The cause sits one layer up, in the enum-payload arm of the by-value param deep
  copy (`param_own.rs`). It reconstructs a `{ptr,len,cap}` from the enum's payload
  words, runs the defensive copy, and then writes the result back — and it wrote
  it back through **`sso_string_parts_from_value`**:

  ```rust
  let (cd, cl) = self.sso_string_parts_from_value(copied, "p14e.c");   // WRONG here
  let cc = build_extract_value(copied, 2, ...);                        // raw cap
  store_enum_word(data_idx, ptr_to_int(cd)); store_enum_word(len_idx, cl); …
  ```

  That is a **store**, not a read. For an inline `copied` the accessor returns the
  address of an entry-block SPILL SLOT, so the payload ends up holding a `cap`
  that still carries the inline tag next to a `ptr` into a frame that dies. A
  reader then trusts `cap`, recomputes the data pointer from the payload's own
  address, and reads whatever is there.

  **This is exactly the rule this doc has carried since the accessor landed** —
  *"the returned pointer is valid only for an immediate read; storing it into
  anything outliving the frame dangles"* — violated in the tree, by the read
  sweep, at a site that looks like a ptr+len read and is not one. It was latent
  only because the unsigned gate below it de-inlined every source first, so
  `copied` was always heap and the accessor was a no-op. The moment the gate went
  signed, the latent bug became the live one.

  The fix is to round-trip the three words verbatim — a descriptor is complete in
  every state, and the payload should hold it as-is.

  **A validated scan says this was the only such site.** The check is: for each
  accessor call, do its result names reach a `build_store` / `store_enum_word` /
  `build_ptr_to_int` within the next 25 lines? Run against `HEAD` it finds
  `param_own.rs:3728`; run against the fixed tree it finds nothing. **Validating
  the scanner against the known instance before trusting its zero is the step
  that makes a null result mean anything** — four scanner blind spots in this
  campaign say an unvalidated census is a guess.

  ### The first measurement of the fix was taken on the CORRUPT build

  Recorded because the reasoning that caught it was nearly right and the
  conclusion nearly wrong. The −69% allocations were first measured from the
  gate-flip-only compiler — the one corrupting payloads — which is grounds for
  suspecting the win was an artifact: a miscompile that drops String content
  would also drop allocations.

  Re-measured after the payload fix: allocations **identical** (5,128,411) and
  instructions slightly **better** (7,211,445,814 → 7,173,722,614). The suspicion
  was wrong — the corruption changed which bytes were stored, not how many
  allocations happened — but it was the right suspicion to have, and checking it
  cost one re-run. **A perf number from a compiler that fails its differential is
  not evidence, even when it turns out to be right.**

  Gates on the landed state (the payload fix, WITHOUT the gate flip): fmt OK;
  clippy GREEN on both legs; `--features llvm` 109 binaries per leg, **both
  `KARAC_SSO=0` and `=1` at 16,809 passed with ZERO red binaries** — which also
  confirms the payload fix is the no-op it should be while the gate stays
  unsigned.


  ### THE DE-MASKING RAN, AND IT WAS TWO FIXES — not an open-ended list

  The flip's first attempt reddened eight binaries and the honest report was
  "unknown number of defects behind this." Run properly — flip the gate, run the
  selfhost differentials at `KARAC_SSO=1`, fix what breaks, repeat — it converged
  in **two** rounds. All seven differentials green — and the full cycle with all
  three changes in reports **`SSO=0` 16,820 passed / zero red** and **`SSO=1`
  16,785 passed** with only B-2026-09-12-1's coroutine flake, plus the ASAN
  `-O0` ratchet leg green at BOTH SSO settings (1593 passed, quarantine list
  matched exactly). That last leg is the one that matters here: the flip moves
  values from a copy arm to a pass-through arm, and `-O2` deletes the unobserved
  allocation that a stranded buffer would show up as.

      selfhost_codegen        ok      selfhost_parser_types   ok
      selfhost_lexer          ok      selfhost_resolver       ok
      selfhost_parser         ok      selfhost_typechecker    ok
      selfhost_parser_items   ok

  **All three defects share one root, and it is worth naming as a class.** Every
  one is a guard or an accessor written while inline descriptors could not reach
  it, so its assumption about the value it was handed was never tested:

  | # | site | what was wrong |
  |---|---|---|
  | 1 | `param_own.rs` enum-payload store | used the read-only accessor to STORE a descriptor, writing a spill-slot address into a payload word |
  | 2 | `vec_method.rs` mutation chokepoint | its "is this a Vec?" guard tested `!vec_elem_types.contains_key(v)`, but that table holds Strings too |
  | 3 | *(none — 1 and 2 were the whole list)* | |

  ### THE SIDE-TABLE MISREADING, because it happened twice

  Defect 2 is the instructive one. The guard read:

  ```rust
  && !self.var_types.vec_elem_types.contains_key(var_name)   // "not a Vec" — WRONG
  ```

  `vec_elem_types` maps **any** `{ptr,len,cap}`-shaped local to its ELEMENT type.
  A pattern-bound or let-bound `String` is in it with element `i8` —
  `pattern_binding.rs`'s own comment calls it "the side table METHOD DISPATCH
  reads to pick the String-shaped arm." So the guard skipped promotion for
  exactly the Strings that needed it, and this doc's own description of it
  ("erring toward promoting whenever the receiver is not positively known to be a
  `Vec`") was backwards.

  Measured: the self-hosted item parser SIGSEGV'd in
  `collect_leading_doc_comments` on

  ```kara
  Some(prev) => { let mut joined = prev; joined.push_str("\n"); … }
  ```

  — an **invalid write of size 1** at address `0x656e6f20656e696c`, which is the
  little-endian ASCII of `"line one"`: the string's own content bytes used as a
  destination pointer. Valgrind names the function and decodes the address in one
  step, which is why it found in minutes what reading could not.

  **The guard is now gone rather than corrected.** Both Vec guards in this
  campaign were wrong in the same unsafe direction, and there is no reliable
  POSITIVE Vec signal to test (`var_type_names` carries struct names, not
  container names; `string_vars` would reintroduce the same failure the moment an
  entry is missing). So the mutating set promotes unconditionally:
  `sso_deinline_in_place` is a not-taken branch for a `Vec` — its `cap` is a
  count, never negative — and compiles out entirely with SSO off. The cost on
  `Vec.push` is one load, one compare and one not-taken branch. **Re-introduce a
  guard only off a positive Vec signal and only with a measurement showing that
  cost matters**; never off the absence of a String signal.

  ### What the masking cost, as a lesson

  A gate nobody could grep for was not just hiding 2.3 billion instructions of
  performance. It was **holding a whole value representation out of the
  compiler**, and every site downstream of it had been silently exempted from
  supporting inline Strings. That is why "flip one predicate" was the wrong
  mental model and "de-mask, then fix what surfaces" was the right one — and why
  the selfhost differentials, not the unit suite, were the instrument: they are
  the only tests that run a real 20,000-line Kāra program end to end and compare
  it against an independent implementation.

  ### Four mechanisms were falsified before this one was found

  Recorded because the ratio is the lesson. Each was plausible, each was derived
  by reading code or disassembly, and each was wrong or negligible:

  | hypothesis | predicted | measured |
  |---|---|---|
  | lost compare folding (`bcmp`) | "~5 of the 9 ms" | **0.9%** of the regression |
  | the descriptor round trip | fewer redundant stores | **negative** — instructions went UP |
  | the `Vec` len select | the diffuse cost | **42 instructions** out of 9.5 B |
  | `try_inline_into` call overhead | 61% of the regression | net **−612 M** — construction was always winning |

  Only end-to-end counters held up. The profile was worth more than all four
  readings of the source that preceded it.

  ### A near-miss worth keeping

  The first measurement of this fix reported allocations and instructions
  **exactly** unchanged — 9,507,462,371 to the single instruction — and was one
  command away from being written up as "the flip is a no-op, reverting." It was
  measuring a `karac` built two minutes earlier, from before the flip: the
  measurement was fired off the binary's mtime changing rather than off the build
  task's completion.

  An exact match to nine significant figures is not a null result, it is a
  fingerprint — a genuine no-op still perturbs layout and inlining. The cheap
  guard is `cmp` on the two artifacts: **a change that does nothing and a change
  you did not compile look identical from the outside**, and only one of them is
  worth reverting. Same shape as CLAUDE.md's stale-archive and `git archive`
  bisect traps — a result that fails to move when the input demonstrably did.

  ### What this means for the plan

  1. **The PERF objection IS ANSWERED, and the de-masking is done.** SSO is
     −21.9% instructions and 14.8% faster on the self-hosted lexer with 69% of
     its allocations removed, so Slice 2's gate ("instruction count + `malloc`
     leaf share must drop") passes on both halves for the first time. The gate
     flip and both of the latent bugs it un-masked are landed and gated.
  2. **The remaining blocker is CORRECTNESS and it is now the ONLY one**: the
     move-suppression disarm (see "A NEW BLOCKER FOR THE DEFAULT FLIP" above),
     which falsifies a documented `UseAfterMove` guarantee. Its first step is
     still *trace the disarm site*, not write the fix.
     **Measured 2026-09-12, after the de-masking: it is UNCHANGED.** Both
     reproducers behave exactly as before (`uam` reads empty at `KARAC_SSO=1`,
     `mv3` still SIGSEGVs, exit 139), so the two are independent defects rather
     than one cause — which is a useful negative, because the family resemblance
     ("inline descriptors reaching code written before they existed") made it
     reasonable to expect the de-masking to carry it away. It did not.
  3. **Re-measure the synthetic shapes before quoting them again.** The
     transient/retained table above was taken with the unsigned `dcopy` gate
     live, so every one of those numbers understates SSO — the retained rows most
     of all, since a retained String is exactly what gets deep-copied at a
     consuming call. The −11%/−19%/−41% transient losses may also have shrunk.
     **Nothing in that table should be quoted until it is re-run.**
  4. **Branch-not-select stays a footnote** — measured at 0.9% of the regression
     it was filed against.
  5. **The read-path pessimism above was wrong, and the reason is worth keeping.**
     "The cost is diffuse, one select on every String read" was an inference from
     a per-function delta, not a measurement of the selects themselves. The
     diffuse cost was real but small; one concentrated gate was 2.3 billion
     instructions. **Attribute to a line before concluding a cost is structural.**
- **Slice 4 (optional, "go further").** Pair with the lexer source-slices (below) to get
  the hot path to Rust *zero*-copy; small-string fast paths in concat/compare.

## Verification matrix

- **The whole `--features llvm` suite at `KARAC_SSO=0` AND `=1`** — the two-leg
  gate cycle. This is the only thing that exercises inline descriptors broadly,
  and it is MANUAL: nothing schedules it, so it happens when whoever is holding
  the campaign remembers. Every SSO regression so far landed through a green
  single-leg gate set.
- `tests/cli.rs::test_sso_inline_string_survives_the_env_ffi_boundary` — the one
  `KARAC_SSO=1` fixture that runs unprompted. It is `#[cfg(feature = "llvm")]`,
  so it rides the `--features llvm` leg — which is the leg that has codegen at
  all — and it covers exactly one shape: a String crossing into a runtime
  extern. Growing this list is how the manual cycle above stops being
  load-bearing.
- `tests/codegen.rs` String suite (E2E) + the new dispatch tests.
- `tests/memory_sanitizer.rs` ASAN on macOS (UAF/double-free) **and** the Linux/LSan CI
  `memory-sanitizer` job (leaks — *the* gate, since SSO rewrites the free path; macOS
  cannot see leaks).
- `leaks --atExit` guardmalloc at **both O0 and O2** (codegen leaks and double-frees hide
  oppositely under optimization — `reference_macos_leak_detection_methodology`).
- Re-profile the self-host lexer (instruction-count gate) + corpus re-bench before any
  published number.

## The complementary, separately-owned win (record — do NOT do here)

The self-host *number* specifically also closes by rewriting the lexer to **classify on
borrowed slices** (`s[a..b]`, clone only when an identifier is actually stored) —
`selfhost/src/main.kara:1239`, `:1260`, `:696/:703/:720`, the string/char-scan sites, etc.
**The string-match dispatch tree already works zero-copy on a slice** (it reads ptr+len,
which a slice has), so there is **no compiler blocker** — this is the
[`project_lexer_string_scan_shape`] lesson applied inside the lexer. SSO (no-malloc) and
slices (zero-copy) are complementary: SSO helps the whole corpus; slices get this one hot
path fully to Rust. This file is **selfhost-session-owned source** — filed here for that
session, intentionally not edited from a compiler-side worktree (the
two-sessions-one-file hazard).

## Cross-references

- [`selfhost-lexer-profile.md`](selfhost-lexer-profile.md) — the profile that motivates
  this (allocation = #1 leaf post-dispatch).
- String-match dispatch lever — commit `5adf2e90`; shares the accessor surface (its
  dispatch tree must route through the tag-aware accessor in Slice 1/3).
- `roadmap.md` § Codegen Optimization — the allocation-reduction entry points here.
- `reference_macos_leak_detection_methodology`, `project_self_hosting_v1_credibility`.
