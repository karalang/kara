# Spike: LLVM-C FFI binding for self-hosted codegen

**Status:** OPEN — design question, not started. Blocks the **codegen leg** of [Phase 12 Self-Hosting](../implementation_checklist/phase-12-self-hosting.md#port-sequencing) and informs the Phase 8 FFI floor surface ([phase-12 § Pre-pivot blockers, Cluster 2](../implementation_checklist/phase-12-self-hosting.md#pre-pivot-blockers-fix-before)).

## Question

The Rust `karac` emits LLVM IR via `inkwell` (safe Rust bindings over the LLVM C++ API). The self-hosted (Kāra) `karac` cannot use `inkwell` — it must call LLVM itself. **How does Kāra codegen bind to LLVM?**

## Constraint that mostly answers it

Kāra FFI is `extern "C"` (Phase 7, ✅). LLVM ships a stable **C API** (`llvm-c/*.h`) — the natural and only sane target. So the binding is: Kāra `extern "C"` declarations over `libLLVM`'s C API. The codegen-containment invariant carries over — only the Kāra codegen module touches these externs, exactly as only `src/codegen.rs` touches `inkwell` today.

## Open sub-questions (resolve before the codegen port)

- **Surface scope.** ✅ RESOLVED — see [LLVM-C surface inventory](self-hosting-llvm-c-surface.md). Codegen uses **~120 distinct llvm-c functions** out of the hundreds `inkwell` wraps. Key finding: ~1,900 of the apparent "inkwell calls" are typed-value coercions (`into_int_value`, etc.) that map to **zero** llvm-c functions — they vanish under llvm-c's single untyped `LLVMValueRef`, which also simplifies the handle model (sub-q 3). LLVM-18 opaque-pointer `2`-variants (`LLVMBuildLoad2`/`Call2`/`GEP2`) are the hard version pin.
- **Linking.** ✅ RESOLVED — **dynamic-link the system `libLLVM`, version-pinned to LLVM 18.1, identical to the Rust stage.** Rationale and mechanism below.

  **Decision: dynamic, not vendored-static.** The Rust `karac` already pins **LLVM 18.1** and links `prefer-dynamic` (`inkwell` `llvm18-1-prefer-dynamic` + `llvm-sys` `prefer-dynamic`, `Cargo.toml:90,96`) — and the `Cargo.toml:55` comment states the explicit reason: a *single* LLVM copy shared between inkwell and llvm-sys. The self-hosted binding must extend that same single-copy discipline: stage-1 (Rust `karac` building the Kāra compiler) and stage-2+ (self-hosted) link the **same** `libLLVM-18.dylib`/`.so`, or IR/ABI drifts between stages (this is also what sub-q *Bootstrapping risk* requires). Vendored-static is rejected: a static `libLLVM` is huge (against the AOT pipeline's small-binary discipline — `driver.rs` goes to lengths for `-dead_strip`/lean-archive), and a shared `.dylib` is the simplest way to *guarantee* the cross-stage single-copy invariant.

  **Locating it:** reuse `llvm-sys`'s existing discovery — `LLVM_SYS_181_PREFIX` env var, else `llvm-config --libdir` on `PATH`. The bootstrap script exports one prefix; both stages read it, so both resolve the identical library. (Same probe family `driver.rs:263` already uses for `wasm-ld` under Homebrew `llvm@18`.)

  **Linking it — the gap (new prerequisite, see below):** the AOT link line in `codegen/driver.rs:432` is hardcoded `cc <obj> <runtime.a> -lm -lpthread -ldl` plus dead-strip; `extra_cc_args` is internal (sanitizer flags only). There is **no** way for a `.kara` program or `kara.toml` to add `-L`/`-l` for an external native library — Kāra's linker-control attributes (`design.md` § Linker Control Attributes) name *symbols*, not *libraries*. So the Kāra-karac link needs `-L<llvm-libdir> -lLLVM-18` injected, which requires a **new native-library link directive**. Chosen shape: a **`kara.toml` `[link]` directive** (`libs = ["LLVM-18"]`, `search-paths` from `llvm-config --libdir`) appended to the `cc` line — *not* an extern-block attribute. Reason: the libdir is environment-specific (resolved by `llvm-config` at build time), which is a project/build concern; the `unsafe extern {}` block says *what symbols*, the manifest says *where the library lives* — mirroring how `llvm-sys` itself splits source FFI from build-time `llvm-config`.

  **Runtime consequence (noted, not a blocker):** dynamic libLLVM means the self-hosted `karac` carries a runtime `.dylib`/`.so` dependency on `libLLVM-18` — it is *not* a standalone static binary. This is already true of the Rust `karac` (`prefer-dynamic`), so it is not a regression; acceptable for a compiler (developers have LLVM installed). Document in the self-hosted build README.
- **Handle / ownership modeling.** ✅ RESOLVED. Two-category model, both categories already buildable on shipped mechanisms.

  **Representation.** Each LLVM-C opaque pointer is `typedef struct LLVMOpaqueX *LLVMXRef;`. Model the pointee as a Kāra opaque foreign type (`unsafe extern "C" { type LLVMOpaqueModule; }`, design.md § Opaque Foreign Types — shipped) and the handle as a **single-field newtype** over `*mut LLVMOpaqueModule` — the Phase-10 `host fn` opaque-handle shape (phase-10-targets.md:26, landed 2026-06-05), where `Copy` is *not* required so a handle can carry `Drop`-based release.

  **Category A — owned-with-`Drop`** (a matching `LLVMDispose*`; modeled non-`Copy` → move-only → exactly one dispose, double-free-proof by design.md:8808's `Drop`/`Copy` mutual exclusion). `impl Drop` calls the disposer:

  | handle | disposer | note |
  |---|---|---|
  | Context | LLVMContextDispose | the arena root — **disposed last** |
  | Module | LLVMDisposeModule | |
  | Builder | LLVMDisposeBuilder | |
  | TargetMachine | LLVMDisposeTargetMachine | |
  | TargetData (from `LLVMCreateTargetDataLayout`) | LLVMDisposeTargetData | distinct from the borrowed `LLVMGetModuleDataLayout` view |
  | MemoryBuffer | LLVMDisposeMemoryBuffer | object-emit buffer |
  | DIBuilder | LLVMDisposeDIBuilder | **must `LLVMDIBuilderFinalize` before dispose** |
  | PassBuilderOptions (for `LLVMRunPasses`) | LLVMDisposePassBuilderOptions | |
  | error message (`char*` out-param) | LLVMDisposeMessage | feeds sub-q 4/5; the dispose lives here |

  **Category B — borrowed / non-owning** (NO disposer — arena-owned by the Context/Module; modeled as plain `Copy` value handles with **no `Drop`**). Giving any of these a `Drop` would double-free when the Context tears down the whole arena:

  - `LLVMValueRef` (functions, instructions, constants, globals, params), `LLVMTypeRef` (all types), `LLVMBasicBlockRef`, `LLVMAttributeRef` — Context/Module arena.
  - `LLVMTargetRef` (`from_triple`) — static/global.
  - Borrowed views: `LLVMGetModuleContext`, `LLVMGetModuleDataLayout` — non-owning aliases of an already-owned handle.

  The `Drop`/`Copy` exclusion is load-bearing here: category B is `Copy` (freely passed by value, the ~1,900 ex-coercion sites), category A is non-`Copy` — the compiler structurally forbids an arena handle from accidentally acquiring both a bitwise copy *and* a destructor.

  **Drop-order invariant.** The Context must be the **last** Category-A handle disposed (it owns every arena handle; disposing it invalidates all `LLVMValueRef`/`LLVMTypeRef`/`LLVMBasicBlockRef`). DIBuilder must `Finalize` before its own dispose. Arrange the owning codegen struct's field/drop order so Context drops last; TargetMachine / TargetData / MemoryBuffer are Context-independent and may drop in any order.

  **The one inkwell safety property the binding loses (honest cost).** inkwell encodes "a borrowed `LLVMValueRef` must not outlive its `Context`" with a Rust lifetime parameter (`Module<'ctx>`, `BasicValueEnum<'ctx>`). Kāra opaque handles are scalars with **no lifetime parameter**, so the compiler will *not* catch a borrowed handle outliving its Context — a use-after-free the type system can't see. Why it's acceptable: codegen is single-pass with **one** Context created first and dropped last; no borrowed handle is stored past the Context's scope, so the hazard is structurally absent. It becomes a manual binding-author invariant rather than a checked one — the genuine cost of dropping from inkwell's lifetime-checked wrapper to raw llvm-c. Document it at the top of the Kāra codegen module.
- **String marshaling.** ✅ RESOLVED. Three directions, all on shipped `CStr`/`CString`/`String` surface (design.md § C-String Literals) bar one small floor refinement.

  **Inbound (Kāra → LLVM `const char*`).**
  - *Static* names (fixed triples, a few fixed symbol names) → `c"..."` literal `ref CStr`, `.as_ptr() -> *const u8`. Zero-cost (rodata, no copy/NUL append — design.md:4613).
  - *Dynamic* names (the common case: program-built symbol names for `LLVMAddFunction` / `LLVMGetNamedFunction` / the `Name` arg of every `LLVMBuildXxx`) → `String.to_cstring() -> Result[CString, NulError]`, then `.as_ptr()` (design.md:4642–4644). The `NulError` is effectively unreachable for Kāra identifiers (no interior NUL) but the `Result` must be threaded — a minor error-map touchpoint that feeds sub-q 5.
  - *Explicit ptr+len* APIs (`LLVMConstStringInContext(C, str, len, …)`, `LLVMCreateStringAttribute(C, K, KLen, V, VLen)`) → pass `(s.as_ptr(), s.len())` directly; no NUL, no `CString` allocation.

  **Outbound (LLVM runtime-owned `char*` → Kāra `String`).** LLVM returns heap `char*` the *caller* must free. Read into an owned Kāra `String`, **then** call the Category-A disposer (sub-q 3 table). Sources: `LLVMPrintModuleToString`, `LLVMGetDefaultTargetTriple`, and the verifier/emit error out-params — all freed with `LLVMDisposeMessage`; the `LLVMErrorRef` path (sub-q 5) frees with `LLVMDisposeErrorMessage`. Read path: `unsafe { CStr.from_ptr(p) }.to_string() -> Result[String, Utf8Error]`. **Floor refinement (folded into the Phase-8 *String marshaling* prerequisite, not a new blocker):** design.md describes the behavior of a runtime-constructed `CStr` (O(N) `len` walking to the NUL — design.md:4638) but never *names* the unsafe constructor. Add `CStr.from_ptr(*const u8) -> ref CStr` (or `String.from_c_ptr`) to that item. Fallback if it slips: the read is hand-rollable today from shipped primitives (manual strlen over `ptr.const` reads + `String.from_raw_parts(ptr, len)`), so it does not *block* — it is the ergonomic/correct spelling, not a gate.

  **Object bytes (binary, not a string).** The in-memory object path (`LLVMGetBufferStart`/`LLVMGetBufferSize`, the 49 `as_slice` sites) returns raw bytes that may contain NULs → marshal as `Slice[u8]` / `String.from_raw_parts`, **never** the C-string path. The minimal proof can sidestep this entirely by using `LLVMTargetMachineEmitToFile` (takes a `const char* Filename`, writes the object directly), so byte-buffer marshaling is only needed for an in-memory/JIT object pipeline.

- **Error / diagnostic mapping.** ✅ RESOLVED. LLVM-C has **two** error idioms; both map to Kāra `Result[T, CodegenError]`, preserving the "every phase emits structured diagnostics, never panic" invariant on the codegen leg.

  1. **Legacy `LLVMBool` return + `char** OutMessage` out-param** — `LLVMVerifyModule`, `LLVMTargetMachineEmitToFile`, `LLVMGetTargetFromTriple`. Declare the out-param as `*mut *const u8`; from Kāra, `let mut msg: *const u8 = ptr.null()` and pass `ptr.mut(msg)` (shipped safe construction — no floor gap). On the failure return (these return `1` = failure), read `msg` via the outbound path above, `LLVMDisposeMessage`, return `Err`. On success the out-param is null/untouched → `Ok`.
     - **Load-bearing detail:** `LLVMVerifyModule` takes a `LLVMVerifierFailureAction` — the Kāra binding **must** pass `LLVMReturnStatusAction`, never `LLVMAbortProcessAction` / `LLVMPrintMessageAction`, or LLVM calls `abort()`/`exit()` and never returns control to Kāra. This single enum choice is what keeps a verifier failure a structured `Result` instead of a process kill — the codegen-leg equivalent of the "never just panic" rule.
  2. **Newer `LLVMErrorRef` return** — `LLVMRunPasses` (new pass-manager C API). Non-null = error. `LLVMGetErrorMessage(err) -> char*` (consumes the error; the string is freed with `LLVMDisposeErrorMessage`). Read into `String`, dispose, return `Err`.

  **Diagnostic class.** These are **not** user-source-spanned errors — verifier/emit failures mean codegen produced invalid IR, i.e. a compiler-internal invariant violation. Map them to an **ICE-class** diagnostic ("codegen produced invalid IR: <llvm message>"), the same status a verifier failure has in the Rust `karac` today — not a `Span`-carrying user diagnostic. The exception is target-init / unknown-triple errors (`LLVMGetTargetFromTriple`), which are **environment-class**: no source span, but user-actionable ("unknown target triple '<x>'"). This keeps the "structured diagnostics" invariant honest without inventing fake source spans for backend-internal failures.
- **Error / diagnostic mapping.** Verifier failures, target-init errors, `LLVMVerifyModule` out-params → Kāra `Result` via the Phase 8 *FFI — Error code mapping* item. Preserves the "every phase emits structured diagnostics" invariant on the codegen leg.
- **Bootstrapping risk.** ✅ RESOLVED — synthesis of sub-q 2/3/4/5 against the [3-stage bootstrap](../implementation_checklist/phase-12-self-hosting.md#bootstrap-fixpoint) (phase-12:51–54). Two cross-stage invariants, one verification protocol, one honest limit on what the fixpoint proves.

  **The stages, and where the binding sits in each:**
  - *Stage-1* — Rust `karac` compiles the Kāra-written compiler source → `karac₁`. The Rust karac must parse/typecheck/codegen the Kāra source's ~120 LLVM-C `unsafe extern "C"` blocks (opaque foreign types, raw-ptr params, `ptr.mut` out-params), and `karac₁` must link `libLLVM-18` via the new `kara.toml [link]` directive.
  - *Stage-2* — `karac₁` compiles the same source → `karac₂`. Now the *self-hosted* codegen module emits the LLVM-C calls.
  - *Stage-3 fixpoint* — `karac₂` compiles the source → `karac₃`; `karac₂` and `karac₃` must be byte-identical. Ship `karac₂`.

  **Invariant 1 — identical `libLLVM-18` across stages.** Stage-1's Rust karac resolves libLLVM via `inkwell`/`llvm-sys` (`LLVM_SYS_181_PREFIX`); `karac₁`/`karac₂` resolve it via the `[link]` directive's `search-paths`. If those resolve *different* libLLVM copies (different patch version, Homebrew vs vendored), the IR `karac₁` emits can diverge from what the Rust stage validates against → fixpoint divergence or ABI drift. **Mitigation (already the sub-q 2 decision):** one `LLVM_SYS_181_PREFIX` exported by the bootstrap script is the single source of truth — the Rust build reads it directly, and the `[link]` `search-paths` come from `llvm-config --libdir` of *that same prefix*. One LLVM location, all stages.

  **Invariant 2 — identical link path across stages.** Stage-1's `cc` line (`<karac.o> <runtime.a> -lm -lpthread -ldl -L<llvm-libdir> -lLLVM-18`) and stage-2's (emitted by `karac₁`'s own ported driver) must match. This holds *by construction* — `karac₁`'s driver is a faithful port of `codegen/driver.rs` reading the same `kara.toml [link]` — **provided the `[link]` directive is itself ported faithfully.** That makes the `[link]` blocker a *both-compilers* dependency, not just a Rust-side one: it must exist identically in the Rust karac (to build stage-1) and in the Kāra karac (to build stage-2). Highest-risk surface generally, because the FFI floor + `[link]` directive are the **newest** code — the most likely site of stage-1/stage-2 disagreement. Mitigation: differential-test them like every other phase (phase-12:37,54); the LLVM-C codegen module is its own best differential-test input.

  **Verification protocol — "before the fixpoint, not after," made precise.** A byte-identical fixpoint (`karac₂ == karac₃`) proves **self-consistency, not correctness**: a stable FFI/link bug present in *both* stage-1 and stage-2 survives the fixpoint (`karac₂` still equals `karac₃`) while silently miscompiling real programs. This is exactly why phase-12:9 sets the bar at "production dev platform, not passes the fixpoint," and why the real correctness gate is the **differential oracle** (phase-12:54 — Rust-`karac` output == Kāra-`karac` output over a `.kara` corpus), not the fixpoint. Concrete protocol for the codegen leg:
  1. Run the **minimal proof** (this spike's DoD) green under the **stage-0 Rust karac** *and* under the **stage-2 self-hosted karac** — same `extern "C"` source, same `[link]` resolution, same emitted object.
  2. Gate the codegen leg on the **differential oracle**, not the fixpoint alone.
  3. Only then lean on the 3-stage fixpoint as the final self-consistency check.

  No new prerequisite — the 3-stage bootstrap, differential gate, and Cluster-2 floor already exist; this fixes the binding's *place* in them and the order of trust.

## Prerequisites (Phase 8 floor)

`#[repr(C)]`, callback passing, String marshaling, error-code mapping, raw-pointer deref/method, `CString` — see [phase-12 § Pre-pivot blockers, Cluster 2](../implementation_checklist/phase-12-self-hosting.md#pre-pivot-blockers-fix-before).

- [x] **`CStr.from_ptr(*const u8) -> ref CStr` unsafe constructor.** ✅ LANDED 2026-06-11. *Surfaced by sub-q 4 (String marshaling).* The inbound half of the outbound read path (LLVM runtime-owned `char*` → Kāra `String`, then dispose): `unsafe { CStr.from_ptr(p) }.to_string()`. Lowers to libc `strlen` + the `{ptr, len}` aggregate a `c"..."` literal lowers to (codegen `assoc_call.rs`); typechecked as an associated constructor (`expr_call.rs`, validates `*const u8`, returns `ref CStr`); `unsafe`-gated by the `unsafe_op_in_unsafe_fn` registry seed (`unsafe_lint.rs`, alongside `ptr.from_exposed`); interpreter-rejected (no raw-pointer representation under `karac run`). 6 tests across the three layers. **NOTE:** the *outbound* `CStr.to_string()` half it feeds is still unbuilt — see the proof's gate; `from_ptr` alone does not complete the read path.
- [x] **Native-library link directive (`kara.toml [link]`).** ✅ LANDED 2026-06-11. *New prerequisite surfaced by sub-q 2 (Linking).* `karac` had no way to link an external native library: the AOT link line was hardcoded `cc <obj> <runtime.a> -lm -lpthread -ldl`, `extra_cc_args` was internal, and Kāra's linker-control attributes name symbols not libraries. Now a `kara.toml` `[link]` table (`libs = [...]`, `search-paths = [...]`) appends `-L<path>` / `-l<name>` after the runtime archive — exactly the `-L<llvm-libdir> -lLLVM-18` the codegen leg needs. Implementation across `manifest.rs` (parse + validate), `target.rs` (`OnceLock` process-wide config, the `set_target_cpu_override` pattern), `codegen/driver.rs` (`link_executable_impl` appends the flags; absent table → byte-identical link line), `cli.rs` (resolved from the manifest in both build paths; manifest-only, no CLI/env tier since the libdir is environment-resolved). 7 manifest unit tests + 2 link E2E (a missing lib's name reaching the linker error proves `-l` injection; a real `zlibVersion` resolving only with `[link] libs=["z"]` proves the resolve path). The shape matches this section's decision exactly: the `unsafe extern {}` block says *what symbols*, the manifest says *where the library lives*. ↔ cross-ref [phase-12 § Pre-pivot blockers, Cluster 2](../implementation_checklist/phase-12-self-hosting.md#pre-pivot-blockers-fix-before).

## Definition of done (this spike)

A decision record covering: chosen linking strategy + version-pin; the enumerated LLVM-C surface (from the `inkwell` call-site inventory); the handle-ownership model (which handles `Drop`); and a **minimal proof** — a Kāra program that `extern "C"`-calls LLVM-C to build → verify → emit a trivial module to an object file, linked and run. That proof is the seed of the Kāra codegen module.

**Status:** decision record ✅ COMPLETE — all of sub-q 1–6 above resolved ([surface inventory](self-hosting-llvm-c-surface.md), linking, handles, marshaling, errors, bootstrapping). **Proof: handle chain RUNS against real libLLVM** — five prerequisites have landed and a `LLVMContextCreate` → `LLVMModuleCreateWithNameInContext(name, ctx)` → `LLVMContextDispose(ctx)` program now builds, links `libLLVM-18`, and runs (exit 0). This is the first time Kāra drives real libLLVM through this binding. What landed (2026-06-11):

- ✅ **`kara.toml [link]` directive** (libLLVM-18 linkable).
- ✅ **`CStr.from_ptr`** (inbound `char*`→`CStr`).
- ✅ **`#[link_name]` honored on `unsafe extern` imports** — binds the PascalCase LLVM-C API to legal snake_case Kāra names.
- ✅ **Auto-par "Undefined variable" bug** — the capture-set collector didn't recurse into `unsafe {}`; a general-purpose correctness fix.
- ✅ **`*mut T` raw pointers are `Copy`** — a `*mut` handle passed to many FFI calls no longer fires a use-after-move (Rust parity; `is_copy_type` `Type::Pointer` arm). This is what made the handle chain run.

Remaining gates to the **full** proof (build → verify → emit-object → link → exit 42):

1. **`CStr.to_string() -> Result[String, Utf8Error]`** — the outbound `char*`→`String` half of the read path (`read_and_dispose`). No codegen lowering, no runtime UTF-8 validator to reuse. The remaining half of the phase-8 *CString conversions* item. Needed for the proof to **compile** (used only on error paths, never on the `exit=42` success path — so a proof written to report errors *without* `to_string` could run sooner).
2. **Proof-spec rewrite** — semicolon-free statements that don't parse + PascalCase extern names → snake_case + `#[link_name]` + terminators. Mechanical.

The spike closes when the (rewritten) proof runs green (`exit=42`) under both bootstrap stages. The hard FFI/ownership gates are now cleared; what remains is one stdlib method + a mechanical rewrite.
