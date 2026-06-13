# Spike: Small-String Optimization (SSO) for the runtime `String`

**Status:** 🟡 **SCOPED — campaign teed up, NOT started.** Designed for a *fresh,
dedicated session* (highest blast radius of any String-subsystem change; see "Why
later" below). This doc is the cold-start handoff: layout decision, staged slice plan,
the tag-aware-accessor work list, and the verification matrix. Scoped 2026-06-12.

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

### Layout decision (settle in Slice 1)

- **Option A — in-struct tag (recommended).** Reuse the 24 bytes. Inline form stores up
  to ~23 bytes of data overlapping the `ptr`/`len`/`cap` words (libc++/folly style), with
  one bit (a high bit of `cap` or `len`, or the low bit of `ptr` which is always
  pointer-aligned ⇒ 0 for heap) as the inline flag. `Vec` leaves the flag clear. Minimal
  type churn — String stays `vec_struct_type` everywhere.
- **Option B — split `String` into a distinct LLVM type.** Cleaner semantics but enormous
  churn: String currently *is* `vec_struct_type` across ~15 files, the by-value ABI, the
  recursive-drop type-identity checks (`llvm_ty_is_vec_struct`), and dispatch. **Rejected
  unless A proves unworkable.**

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

- **Slice 0** — *this spike.* Scope + layout-decision criteria.
- **Slice 1 — layout + accessors (no behavior change).** Pick the layout (Option A);
  implement tag-aware `string_data_ptr()` / `string_len()` / `string_is_inline()` in
  codegen and the runtime; prove them no-op-correct for `Vec`. Route the construction +
  read sites to the accessors but **keep every string heap-allocated (tag never set)**.
  Gate: full suite green, **zero perf delta** (pure plumbing).
- **Slice 2 — inline construction (the win).** `substring`, runtime-built `StringLit`,
  concat, `to_string`, `push_str` result → build **inline** when `len ≤ N`. Drop/clone
  become tag-aware. Gate: **re-profile the self-host lexer** (instruction count + `malloc`
  leaf share must drop), full ASAN + **Linux/LSan** (SSO touches every free path —
  authoritative leak gate).
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
