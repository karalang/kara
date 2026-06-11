# Spike: LLVM-C FFI binding for self-hosted codegen

**Status:** OPEN — design question, not started. Blocks the **codegen leg** of [Phase 12 Self-Hosting](../implementation_checklist/phase-12-self-hosting.md#port-sequencing) and informs the Phase 8 FFI floor surface ([phase-12 § Pre-pivot blockers, Cluster 2](../implementation_checklist/phase-12-self-hosting.md#pre-pivot-blockers-fix-before)).

## Question

The Rust `karac` emits LLVM IR via `inkwell` (safe Rust bindings over the LLVM C++ API). The self-hosted (Kāra) `karac` cannot use `inkwell` — it must call LLVM itself. **How does Kāra codegen bind to LLVM?**

## Constraint that mostly answers it

Kāra FFI is `extern "C"` (Phase 7, ✅). LLVM ships a stable **C API** (`llvm-c/*.h`) — the natural and only sane target. So the binding is: Kāra `extern "C"` declarations over `libLLVM`'s C API. The codegen-containment invariant carries over — only the Kāra codegen module touches these externs, exactly as only `src/codegen.rs` touches `inkwell` today.

## Open sub-questions (resolve before the codegen port)

- **Surface scope.** Which slice of the LLVM-C API does codegen actually use? Enumerate it directly from the Rust codegen's `inkwell` call sites (module / builder / type / value construction, target machine, pass/verifier, object-file emit) — that inventory IS the exact set of `extern "C"` signatures to declare. Anything `inkwell` wraps that the Rust codegen never calls, the Kāra binding can skip.
- **Linking.** System shared `libLLVM` vs a vendored static lib? Version pinning (Rust side is **LLVM 18** — the Kāra binding must match, or IR/ABI drift). How does `karac build` of the Kāra compiler locate + link it (a `kara.toml` link directive? an `extern "C"` link-name + search-path story)?
- **Handle / ownership modeling.** LLVM-C uses opaque pointers (`LLVMModuleRef`, `LLVMBuilderRef`, `LLVMValueRef`, …). Model each as a Kāra **opaque-handle newtype** (single primitive-field struct — the `host fn` boundary shape from Phase 10, which allows a `Drop`-based release without requiring `Copy`). Decide per handle: owned-with-`Drop` (must `LLVMDispose*`) vs borrowed/non-owning.
- **String marshaling.** LLVM-C takes/returns C strings (and `LLVMDisposeMessage` for error strings). Depends on the Phase 8 *FFI — String marshaling* + `CString` items. Cross-ref Cluster 2.
- **Error / diagnostic mapping.** Verifier failures, target-init errors, `LLVMVerifyModule` out-params → Kāra `Result` via the Phase 8 *FFI — Error code mapping* item. Preserves the "every phase emits structured diagnostics" invariant on the codegen leg.
- **Bootstrapping risk.** The binding must link + run under BOTH the Rust `karac` (stage-1 build of the Kāra compiler) and the self-hosted `karac` (stage-2+). The `extern "C"` + link path must be identical across stages — verify before the fixpoint, not after.

## Prerequisites (Phase 8 floor)

`#[repr(C)]`, callback passing, String marshaling, error-code mapping, raw-pointer deref/method, `CString` — see [phase-12 § Pre-pivot blockers, Cluster 2](../implementation_checklist/phase-12-self-hosting.md#pre-pivot-blockers-fix-before).

## Definition of done (this spike)

A decision record covering: chosen linking strategy + version-pin; the enumerated LLVM-C surface (from the `inkwell` call-site inventory); the handle-ownership model (which handles `Drop`); and a **minimal proof** — a Kāra program that `extern "C"`-calls LLVM-C to build → verify → emit a trivial module to an object file, linked and run. That proof is the seed of the Kāra codegen module.
