# Phase 12: Self-Hosting

> **The v1 pivot.** Rewrite `karac` in Kāra. Executes AFTER the Phase 8 floor + Phase 9 (done) + Phase 10 (mostly done), and BEFORE Phase 11 — real order **`8 → 9 → 10 → 12 → 11`** (numeric order ≠ execution order). Design record + rationale in [`../roadmap.md` § Phase 12](../roadmap.md#phase-12-self-hosting); the *why* — self-hosting is the credibility proof for a solo/AI-built language — is the driver. This tracker holds the action items: pre-pivot blockers, port sequencing, the bootstrap procedure, and the triage of what self-hosting does / doesn't exercise.

## Strategy

- **Port, don't redesign.** The Rust `karac` is the spec, the differential oracle (diff Rust-output vs Kāra-output on the same inputs), and a near-line-for-line translation source. Most of the port is mechanical.
- **The Rust compiler is now a means, not the product** — the bootstrap seed + oracle. Invest in it only to keep it (a) a trustworthy oracle and (b) complete enough (Phase 8 floor) to compile the port. **Stop adding NEW compiler features to Rust after the pivot** — they would have to be re-implemented in Kāra anyway; the pivot's "write once" savings apply only to *unbuilt* features.
- **The bar is "production dev platform," not "passes the fixpoint."** Phase 11 and residual Phase 10 GPU get built ON the self-hosted compiler, so it must be pleasant to do real feature work in (usable diagnostics, fast iteration, complete coverage).
- **Self-hosting is a bug-finder, but only for the compiler-shaped subset** of the language (enums, pattern matching, recursion, strings, Vec/Map/Set, Option/Result/`?`, FFI). It is near-blind to floats/numerics, the concurrency runtime, networking, and Tensor — keep katas/demos as the bug-finder for those (see SEPARATE-TRACK below).

## Pre-pivot blockers (FIX-BEFORE)

These corrupt the stage-1 build or the differential oracle — land them before leaning on the port. (Triaged 2026-06-10; full reasoning at the bottom of this file.)

### Cluster 1 — memory-safety in collections-of-heap-elements ⚠

The compiler is built on `Vec[(Span, Token)]`, `Map[String, Symbol]`, etc. These shallow-copy heap-containing compound elements → use-after-free / double-free. The output is value-correct but ASAN-dirty — the worst case for a self-hosted compiler: it appears to work while corrupting memory, then crashes non-deterministically on a large build (like building itself). One root family (trivially-copyable misclassification + non-recursive deep-copy).

- [ ] ⚠ **`Vec[(i64, String)]` clone UAF** — `karac_string_clone` reads a freed buffer; a tuple/struct Vec element containing a heap field is shallow-bitwise-moved on push-grow. → [`bugs.md` B-2026-06-10-5](bugs.md).
- [ ] ⚠ **`Map.insert` / `Set.insert` owned-param UAF** — stores the alias while the caller retains the free. → [`phase-7-codegen.md`](phase-7-codegen.md) § *Owned-param retention follow-up: Map.insert / Set.insert*.
- [ ] ⚠ **Map/Set deep-copy doesn't recurse heap elements** — `Vec[Map[K,V]]` consumed at a retaining site flat-copies the outer buffer and aliases the map handles. → [`phase-7-codegen.md`](phase-7-codegen.md) § *Owned-param retention follow-up: deep copy doesn't recurse through Map/Set elements*.
- [ ] ⚠ **`Option[String]` / `Option[Vec[…]]` dropped-without-destructuring leaks its inline payload** — the compiler holds `Option[Token]` / `Option[String]` pervasively (lexer/parser return them), and every undestructured drop leaks. → [`bugs.md` B-2026-06-10-6](bugs.md) (filed by the concurrent session 2026-06-10).

### Cluster 2 — floor prerequisites the port needs (Phase 8)

The port can't be written cleanly without these. The rest of the Phase 8 floor can finish in parallel.

- [ ] **FFI surface for the LLVM-C binding** — `#[repr(C)]`, callback passing, String marshaling, error-code mapping, raw-pointer deref/method, `CString`. The self-hosted codegen calls LLVM through `extern "C"`. → [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md) § FFI + the [LLVM-C FFI spike](../spikes/self-hosting-llvm-c-ffi.md).
- [ ] **Native-library link directive (`kara.toml [link]`)** — `karac` has no way to link an external native library; the AOT link line (`codegen/driver.rs:432`) is hardcoded `cc <obj> <runtime.a> -lm -lpthread -ldl`. The codegen leg can't link `libLLVM-18` without a `[link]` table (`libs`, `search-paths` from `llvm-config --libdir`) appended to the linker invocation. Surfaced + designed by the spike's sub-q 2 (Linking). → [LLVM-C FFI spike](../spikes/self-hosting-llvm-c-ffi.md) § Prerequisites.
- [ ] **`From` / `TryFrom` + `Error` trait** — `?`-operator error conversion across error types; used everywhere in the compiler. → [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md) (G25 Standard Error trait; Conversion traits).
- [ ] **Effect-polymorphic `Iterator.next()`** — loops over data. → [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md).
- [ ] **`Map.new()` / `Set.new()` initialisers + plain `insert` / `get` / `contains`** — core symbol-table / intern-table construction (module-scope `Map.new()` is the actual gap; local `Map.new()` + plain ops already work). The *fallible* `Map.try_insert` / `Set.try_insert` is **NOT** a prereq — the compiler aborts on OOM like rustc. → [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md).

## Lexer stdlib-floor slice (LANDED 2026-06-10)

The byte-indexed lexer port needs `u8`/`char`/`String` primitives the stdlib lacked.
Landed across typechecker + interpreter + codegen (each with unit + E2E tests), all
gates green:

- [x] **`String.substring(start, end)`** — two-arg byte-range slice (`token_text`, radix-prefix strip). Preserves the one-arg contract (negative/past-end → empty).
- [x] **`u8.is_ascii_digit / is_ascii_alphabetic / is_ascii_hexdigit`** — value-receiver predicates on the bytes from `String.bytes()` (codegen = inline range checks, no extern). `is_alpha` composes as `b.is_ascii_alphabetic() or b == b'_'`.
- [x] **`i64.from_str_radix(s, radix)`** — `Option[i64]`; hex/bin/oct literals. New `karac_runtime_parse_i64_radix` extern.
- [x] **`f64.parse(s)`** — `Option[f64]`; float literals. New `karac_runtime_parse_f64` extern; properly typed in the typechecker (floats need it — see below).
- [x] **`fix(codegen)` enum/tuple FLOAT payload binding** — pre-existing bug surfaced by `f64.parse`: enum float payloads (`Option[f64]`, the lexer's `Token::Float(f64,…)`) were bound/printed as raw i64 bits. `record_pattern_binding_surface_types` had no `Type::Float` arm → payload word never bitcast back to the float, binding type-tracked as i64. Fixed in typechecker (record `f64`/`f32`) + codegen `reconstruct_payload_value` (bitcast, single-word + tuple-element paths).

(P7 `char.try_from(u32)` + `char` predicates deferred to the non-ASCII slice 2.)

## Self-hosting-surfaced blockers (FIX-BEFORE the lexer can run)

The lexer's natural shape — `struct SpannedToken { token: Token, span: Span }` lexed by
`mut ref self` methods — surfaced two pre-existing compiler bugs. Both reproduce on clean
`main`; neither is caused by the floor slice. The lexer (and all of self-hosting: every AST
node nests enums in structs) is blocked until #1 is fixed.

- [ ] ⚠ **#1 (CODEGEN, bootstrap-critical) — struct fields of a USER enum collapse to one `i64`, losing the payload.** Root cause: `compile_program` (`src/codegen.rs:~5059/5068`) runs `declare_structs` **before** `declare_enums`, so when a struct field references a user enum it isn't in `enum_layouts` yet and `llvm_type_for_name` falls through to `i64` (`src/codegen/types_lowering.rs:1212`). (Seeded `Option`/`Result` are exempt — seeded at 5051, which is why `Option` fields work.) Minimal repro: `enum Token { Ident(String), Eof } struct S { token: Token, span: i64 } fn f(s: S){ match s.token { Ident(name) => …, Eof => … } }` → IR lays `S` as `{ i64, i64 }`; codegen fails `Undefined variable 'name'` (tag silently reads 0, payload binding skipped). Interp is correct. **Fix direction:** two-pass declaration — (a) register struct *metadata* (field TypeExprs/names) for all structs, (b) build enum layouts (make `payload_word_count_for_type_expr` recurse via `struct_field_type_exprs` (AST) instead of `llvm_type_word_count(struct_types)` at `declarations.rs:3162` so it doesn't need struct LLVM types), (c) build struct LLVM types (now `enum_layouts` is populated). High blast radius (every struct/enum layout) — needs the full `--features llvm` + memory_sanitizer suite. → cross-ref [`phase-7-codegen.md`](phase-7-codegen.md).
- [ ] **#2 (INTERP) — `mut ref self` method mutations don't persist.** A `mut ref self` method's writes to `self` are lost on return (direct field writes work; method-call writes don't). Repro: `struct C{n:i64} impl C{ fn inc(mut ref self){ self.n = self.n+1 } } let mut c=C{n:0}; c.inc(); // c.n still 0`. Codegen is correct. Pre-existing on `main`. Not on the lexer's critical path (the port runs under AOT, not interp), but it makes `karac run` unsound for any self-mutating method. → cross-ref [`phase-4-interpreter.md`](phase-4-interpreter.md).
- [ ] **#3 (CODEGEN, minor) — f-string over a match-arm payload binding yields empty/garbage.** `match e { A(name) => f"…{name}…" }` compiles but prints empty (f-string span-collision family, B-2026-06-09-1). Workaround in the port: `push_str` + `to_string` instead of f-strings around match-bound vars. Low priority.

## Port sequencing

Start at lexer → parser (need only the well-tested core; can begin before the full floor lands). Each stage is differential-tested against the Rust `karac` on the same inputs — including the compiler's own source.

- [~] **Lexer in Kāra** — tokens + spans; diff token stream vs Rust lexer. **IN PROGRESS** (2026-06-10). Scan model decided: **byte-indexed** (index a `Vec[u8]` from `source.bytes()`, mirror `src/lexer.rs` near-line-for-line) — chosen over char-iterator so `(offset,length)` spans reproduce the Rust lexer bit-for-bit (exact differential oracle), the port stays mechanical, and lookahead stays O(1). Skeleton + scaffold live in `selfhost/` (root; NOT `bootstrap/`, which is the staging procedure). Harness pivoted from `karac run` (interp) to **`karac build` (AOT)** — the interpreter can't run the lexer (blocker #2 below) and AOT is the real bootstrap oracle anyway. **Blocked on two pre-existing compiler bugs surfaced by the port** (see § Self-hosting-surfaced blockers).
- [ ] **Parser in Kāra** — AST + error recovery; diff AST + diagnostics.
- [ ] **Resolver in Kāra** — name resolution, scopes, visibility.
- [ ] **TypeChecker in Kāra** — inference, generics, trait bounds, exhaustiveness.
- [ ] **EffectChecker in Kāra** — inference (private) + verification (public) + conflict detection.
- [ ] **OwnershipChecker in Kāra** — param-mode inference, move/borrow checking, RC fallback.
- [ ] **Codegen in Kāra** — LLVM-C via `extern "C"` FFI (the big leg; gated on the [LLVM-C FFI spike](../spikes/self-hosting-llvm-c-ffi.md) + the Cluster-2 FFI floor surface).
- [ ] **CLI / driver in Kāra** — `karac build` / `check` over the above.
- [ ] **Bootstrap** — see below.

## Bootstrap procedure (3-stage)

- [ ] **Stage-1:** Rust `karac` compiles the Kāra compiler source → stage-1 binary (correct, but ~2× slow — the codegen IR-quality pass is a Phase 11 item not yet in either compiler).
- [ ] **Stage-2:** stage-1 compiles the same source → stage-2.
- [ ] **Stage-3 fixpoint:** stage-2 compiles the same source → stage-3; **stage-2 and stage-3 must be byte-identical.** Ship stage-2.
- [ ] **Differential gate in CI:** over a corpus of `.kara` programs, Rust-`karac` output == Kāra-`karac` output (the oracle). Lock before declaring self-hosted.

Speed note: the codegen IR-quality pass (Phase 11, compiler-internal) is written in Kāra *after* the pivot and recovers the self-hosted compiler's own speed via this same staging — see [`../roadmap.md` § Codegen Optimization](../roadmap.md#codegen-optimization-ir-quality-pass).

**Done when:** `karac build <kara-compiler-src>` produces a binary that compiles Kāra programs with output identical to the Rust `karac`, and the 3-stage bootstrap reaches a byte-identical fixpoint. From here the Rust `karac` is frozen as the bootstrap seed and all new compiler work lands in Kāra.

## Triage of the existing backlog (2026-06-10)

What self-hosting does / does not force. The full reasoning is the session that produced this tracker; recorded here so the pivot decision is auditable.

**DEFER-TO-BACKWARDS** — the port surfaces these with a real reproducer; never unsafe; fix on demand:
- Returned-borrow residue: guarded / payload-binding `match` arms, method-call chains in return position ([`bugs.md` B-2026-06-07-5](bugs.md)) — clean `UnsupportedForm` today.
- Move-overwrite inner-element *leak* ([`phase-7-codegen.md`](phase-7-codegen.md)) + owned-temp scrutinee-subframe *leak* ([`../spikes/general-owned-temp-tracking.md`](../spikes/general-owned-temp-tracking.md) slice 4) — memory *pressure*, not corruption; fix early only if the port balloons.
- interp / codegen drop-order parity — irrelevant: the oracle is AOT-vs-AOT, the port never runs the interpreter.
- string-format specifiers, `dyn` / trait objects, `Arena` / `Interner`, separate-compilation — the port can start **monolithic** with `Map`-based interning; add when it asks.
- Phase 9 adversarial soundness corpus — **the port IS the corpus** (better than synthetic).
- Phase 5 diagnostic-quality gaps — fix as the port annoys you.

**SEPARATE-TRACK** — the compiler never exercises these; self-hosting will NOT find their bugs (keep katas/demos):
- All Phase 6 (concurrency runtime, par / spawn / channels, scheduler, Windows-IOCP; [`../spikes/network-async-coroutine-transform.md`](../spikes/network-async-coroutine-transform.md), [`../spikes/windows-iocp-eventloop.md`](../spikes/windows-iocp-eventloop.md)).
- Phase 11 numerics / Tensor / data / security / embedded / IR-pass.
- Phase 10 GPU / WASM / embedded-TLS.
- interp perf-drift ([`bugs.md` B-2026-06-09-2](bugs.md), AOT unaffected); TaskGroup-by-value IR (B-2026-06-07-2, resolved-by-rejection); [`../spikes/independence-noalias-ilp.md`](../spikes/independence-noalias-ilp.md) (perf).

**Already landed (de-risks the pivot):** enum-payload boxing ([`../spikes/oversized-enum-payload.md`](../spikes/oversized-enum-payload.md) — a real silent miscompile, fixed) and pattern-arm unbound-field drop ([`../spikes/pattern-arm-unbound-field-drop.md`](../spikes/pattern-arm-unbound-field-drop.md)) — the enum / pattern-matching paths the compiler hammers hardest are correct.
