# Spike: Small-String Optimization (SSO) for the runtime `String`

**Status:** 🟢 **Slice 1 LANDED (layout + accessors + free-gate hardening, proven
no-op); follow-up gate hardening landed 2026-07-11 — EVERY String buffer-free/realloc
gate is now inline-safe (`SGT`), proven no-op (full suite + 590 ASAN/LSan + 2137
codegen E2E green).** Slice 2 (inline construction — *the win*) is the next dedicated
session; its precise handoff is in the staged plan below (step 1's gate half is now
done — only the memcpy-source half + steps 2–4 + the construction flip remain, all
coordinated + only-testable-with-construction). This doc is the campaign's living
handoff: layout decision (now settled), staged slice plan, the tag-aware-accessor work
list, and the verification matrix. Scoped 2026-06-12; Slice 1 landed 2026-07-09.

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
  - **Remaining sweep (the next session's work):** 108 `struct_gep(vec_ty, _, 1)`,
    57 `extract_value(_, 1)`, 69 `extract_value(_, 0)` — 234 single-field reads, each
    needing String/Vec classification. Then concat / `to_string` / `push_str` result
    as construction sites, then the default flip + the profile gate.
  - Gates at this commit: fmt + both clippy legs green; `--features llvm` **SSO=off**
    16,001 passed (red: the load-flaky `signalling_karac_run_does_not_orphan_the_jit_runner`
    and `gpu_e2e`'s missing optional archive); **SSO=on** 16,739 passed, `gpu_e2e` the
    only red binary.

- **Slice 3 — sweep + runtime/FFI decode.** Remaining raw sites; runtime decode
  (`println`/file/http/tls/json); thread the Kāra type to keep `Vec` branch-free for perf.
  Gate: corpus re-bench.
- **Slice 4 (optional, "go further").** Pair with the lexer source-slices (below) to get
  the hot path to Rust *zero*-copy; small-string fast paths in concat/compare.

## Verification matrix

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
