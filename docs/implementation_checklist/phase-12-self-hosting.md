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

- [x] ⚠ **`Vec[(i64, String)]` clone UAF — DONE 2026-06-10** (`63440878`). Root cause was NOT a shallow clone (the clone fn already deep-clones): pushing an inline-f-string tuple left the f-string accumulator's `FreeVecBuffer` armed, freeing the source String right after the push → the Vec element dangled (UAF on read/clone), and the Vec scope-exit drain didn't recurse into the tuple element's String (leak). Fixed by per-element acc suppression at tuple construction + one-level recursive drain into tuple/struct heap fields. push/read, `.clone()`, `.try_clone()`, scope-drop all single-free clean. → [`bugs.md` B-2026-06-10-5](bugs.md). Spun-off non-blocker: B-2026-06-10-8 (RC-fallback let-bound-tuple drop leak — a leak, not UAF).
- [ ] ⚠ **`Map.insert` / `Set.insert` owned-param UAF** — stores the alias while the caller retains the free. → [`phase-7-codegen.md`](phase-7-codegen.md) § *Owned-param retention follow-up: Map.insert / Set.insert*.
- [ ] ⚠ **Map/Set deep-copy doesn't recurse heap elements** — `Vec[Map[K,V]]` consumed at a retaining site flat-copies the outer buffer and aliases the map handles. → [`phase-7-codegen.md`](phase-7-codegen.md) § *Owned-param retention follow-up: deep copy doesn't recurse through Map/Set elements*.
- [ ] ⚠ **`Option[String]` / `Option[Vec[…]]` dropped-without-destructuring leaks its inline payload** — the compiler holds `Option[Token]` / `Option[String]` pervasively (lexer/parser return them), and every undestructured drop leaks. → [`bugs.md` B-2026-06-10-6](bugs.md) (filed by the concurrent session 2026-06-10).

### Cluster 2 — floor prerequisites the port needs (Phase 8)

The port can't be written cleanly without these. The rest of the Phase 8 floor can finish in parallel.

- [ ] **FFI surface for the LLVM-C binding** — `#[repr(C)]`, callback passing, String marshaling, error-code mapping, raw-pointer deref/method, `CString`. The self-hosted codegen calls LLVM through `extern "C"`. → [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md) § FFI + the [LLVM-C FFI spike](../spikes/self-hosting-llvm-c-ffi.md).
- [x] **Native-library link directive (`kara.toml [link]`)** *(2026-06-11)* — `karac` had no way to link an external native library; the AOT link line was hardcoded `cc <obj> <runtime.a> -lm -lpthread -ldl`. Now a `[link]` table (`libs = [...]`, `search-paths = [...]`) appends `-L<path>` / `-l<name>` to the native `cc` invocation, so the codegen leg can link `libLLVM-18`. Landed across four layers: `manifest.rs` (`parse_link_table` → `Manifest.link_libs` / `link_search_paths`, array-of-non-empty-strings validation, unknown-key soft-warn); `target.rs` (`NativeLinkConfig` + `set_native_link_config` / `native_link_config`, `OnceLock` first-set-wins, the `set_target_cpu_override` pattern); `codegen/driver.rs` `link_executable_impl` (appends `-L` then `-l` after the runtime archive, no-op when unset → link line byte-identical to before); `cli.rs` (`apply_native_link_config` wired into both single-file and project build paths, manifest-only — no CLI/env tier, since a libdir is a project fact). Tests: 7 manifest unit tests + 2 link E2E (negative: a missing lib's name reaches the linker error → proves `-l` injection; positive: `zlibVersion` resolves *only* with `[link] libs=["z"]`, fails without → contrast attributes resolution to the directive). Surfaced + designed by the spike's sub-q 2 (Linking). → [LLVM-C FFI spike](../spikes/self-hosting-llvm-c-ffi.md) § Prerequisites.
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
node nests enums in structs) was blocked until #1 — **now fixed**.

- [x] ⚠ **#1 (CODEGEN, bootstrap-critical) — struct fields of a USER enum collapse to one `i64` — DONE 2026-06-10** (worktree `user-enum-struct-field-layout`). Root cause confirmed exactly as diagnosed: `compile_program` ran `declare_structs` **before** `declare_enums`, so a struct field referencing a user enum wasn't in `enum_layouts` yet and `llvm_type_for_name` fell through to `i64` (`types_lowering.rs`), losing the payload word — codegen then failed `Undefined variable 'name'` at the match arm (interp was correct). **Fix (the diagnosed two-pass declaration):** split `declare_structs` into (a) `register_struct_metadata` (AST field side tables — `struct_field_type_{names,exprs}`, `struct_field_names` — no LLVM types), then `declare_enums` (b) whose `payload_word_count_for_type_expr` struct branch now recurses through `struct_field_type_exprs` (AST) instead of `llvm_type_word_count(struct_types)`, so enum sizing no longer needs struct LLVM types, then (c) `build_struct_types` (LLVM types, `enum_layouts` now populated). Same ordering applied to the stdlib declaration path (`declare_stdlib_program`). Verified both directions (enum-in-struct AND struct-in-enum payload), wide multi-word payloads, and a scalar field *after* the enum field (correct read proves exact payload sizing). High blast radius cleared: full `--features llvm` suite + memory_sanitizer green. **Regression tests:** `tests/codegen.rs::test_e2e_user_enum_field_in_struct` (value-correctness) + `tests/memory_sanitizer.rs::asan_user_enum_field_in_struct_heap_payload` (heap String payload, no UAF/double-free). → cross-ref [`phase-7-codegen.md`](phase-7-codegen.md).
- [ ] **#2 (INTERP) — `mut ref self` method mutations don't persist.** A `mut ref self` method's writes to `self` are lost on return (direct field writes work; method-call writes don't). Repro: `struct C{n:i64} impl C{ fn inc(mut ref self){ self.n = self.n+1 } } let mut c=C{n:0}; c.inc(); // c.n still 0`. Codegen is correct. Pre-existing on `main`. Not on the lexer's critical path (the port runs under AOT, not interp), but it makes `karac run` unsound for any self-mutating method. → cross-ref [`phase-4-interpreter.md`](phase-4-interpreter.md).
- [ ] **#3 (CODEGEN, minor) — f-string over a match-arm payload binding yields empty/garbage.** `match e { A(name) => f"…{name}…" }` compiles but prints empty (f-string span-collision family, B-2026-06-09-1). Workaround in the port: `push_str` + `to_string` instead of f-strings around match-bound vars. Low priority.

Building `selfhost/src/main.kara` with the AOT path (post-#1) surfaced a **chain** of further blockers, each masking the next (codegen aborts on the first failing body, so #5/#6 only became visible once #4 was fixed). Numbered #4+ in surface order:

- [x] **#4 (CODEGEN, bootstrap-critical) — index a `Vec`/`Slice`/`Array` field through `self` (`self.bytes[self.current]`) — DONE 2026-06-11** (worktree `selfhost-lexer`, commit `af139057`). Died with "Index operator applied to non-array type". The field-access-rooted index arm resolves the receiver via `lower_field_access_ptr`, whose receiver `match` handles `Identifier`/`Index` but deliberately leaves `SelfValue` at `Ok(None)` — that fall-through is load-bearing for the method-receiver path (`self.count.fetch_add(...)` atomic dispatch must NOT take the generic field-receiver path). So `self.field[i]` fell to the generic tail and compiled the field to a `{ptr,len,cap}` VALUE. Fix: normalise a `SelfValue` inner to a synthetic `Identifier("self")` (self is registered under the name "self" in every per-binding registry) **only in `compile_index`'s FieldAccess arm** — the shared helper, and thus the atomic/method path, is byte-identical. Regression test `tests/codegen.rs::test_e2e_index_vec_field_through_self`; full `--features llvm` codegen suite green (1496), atomic-field-method tests re-verified.
- [ ] **#5 (CODEGEN, bootstrap-critical) — `substring` (and other String methods) on a non-identifier receiver.** `self.src.substring(self.start, self.current)` dies with "no handler for method 'substring' on non-identifier receiver (method dispatch fell through)". The lexer slices the source by byte offsets via `self.src.substring(...)` — a method on a field-access (`self.src`) receiver. Same family as the `.first()`-on-non-identifier-receiver gap noted in [`bugs.md` B-2026-06-07-5](bugs.md). Fix: a dispatcher arm in `compile_method_call` for String methods on field-access receivers (bind the field to a temp / resolve the field ptr, then dispatch — mirror the index-path FieldAccess normalisation in #4). Surfaced 2026-06-11; sole blocker visible after #4.
- [ ] **#6 (CODEGEN/DESIGN, bootstrap-critical) — user `struct Span` collides with the always-injected stdlib `std.tracing` `Span`.** The lexer names its span type `struct Span { line, column, offset, length }` — the single most natural name, and one the Rust `karac` uses throughout (`token.rs`). But `runtime/stdlib/tracing.kara` defines `struct Span { name, span_id, parent_id, fields: Vec[SpanField] }`, and `src/prelude.rs` lists `Span` (+ `LogEvent`/`SpanField`/`Log`/`StdoutExporter`/…) as an **always-injected** prelude name (`STDLIB_SOURCES`, not gated). Codegen's `struct_types` is a flat name-keyed map; `declare_stdlib_program(tracing)` runs unconditionally (even when tracing is unused) and overwrites `struct_types["Span"]` with the tracing layout, so `compile_struct_init("Span", …)` for the user literal builds against the WRONG type → `Invalid InsertValueInst operands` / `Function return type does not match` at module verification. Confirmed via minimal repro (a `Span { line:i64,… }` constructed in a method returning `Span` is enough). **Masked behind #5** today (codegen aborts on `substring` before reaching verification). **A naive "user shadows stdlib name" skip is unsafe** — tracing's own method bodies are compiled unconditionally against the shared `struct_types`, so skipping the user-colliding name would break tracing's codegen unless tracing is also not compiled. **DESIGN DECISION PENDING** (foundational — every self-hosting stage names types like `Span`/`Token`/`Type`, so this gates all of self-hosting, not just the lexer). Candidate fixes: **(B)** gate `std.tracing` (+ peers `process`/`pool`/`tensor`) behind explicit `import` (move to `GATED_STDLIB_SOURCES`, drop from the prelude name lists) — smallest, uses the existing mechanism, but removes ambient `Log.info()`/`Span` from default scope (migrate existing users); **(C)** dead-stdlib elimination — declare/compile an always-injected stdlib module only if the program references it (general, collision-proof for all squatted names, + binary-size/compile-time win, no user-facing import change; most work). Naive **(A)** user-shadows-name collapses toward (C) in practice because of the shared `struct_types` issue above. Surfaced + diagnosed 2026-06-11.

## Port sequencing

Start at lexer → parser (need only the well-tested core; can begin before the full floor lands). Each stage is differential-tested against the Rust `karac` on the same inputs — including the compiler's own source.

- [~] **Lexer in Kāra** — tokens + spans; diff token stream vs Rust lexer. **IN PROGRESS** (2026-06-10). Scan model decided: **byte-indexed** (index a `Vec[u8]` from `source.bytes()`, mirror `src/lexer.rs` near-line-for-line) — chosen over char-iterator so `(offset,length)` spans reproduce the Rust lexer bit-for-bit (exact differential oracle), the port stays mechanical, and lookahead stays O(1). Skeleton + scaffold live in `selfhost/` (root; NOT `bootstrap/`, which is the staging procedure). Harness pivoted from `karac run` (interp) to **`karac build` (AOT)** — the interpreter can't run the lexer (blocker #2 below) and AOT is the real bootstrap oracle anyway. **Bootstrap-critical blocker #1 (enum-in-struct-field) FIXED 2026-06-10**; **#4 (index-`Vec`-field-through-`self`) FIXED 2026-06-11** (`af139057`). Building the skeleton surfaces the blocker chain in order — still BLOCKED on **#5** (`substring` on a field receiver, mechanical) and **#6** (`Span` name-collision with stdlib tracing, a design decision gating all self-hosting). (#2 is interp-only, off the AOT critical path; #3 is the minor f-string-over-match-binding cosmetic, has a `push_str` workaround.)
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
