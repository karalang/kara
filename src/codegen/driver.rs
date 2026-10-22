//! Legacy codegen's half of the driver: the optimization pipeline (with the
//! coroutine lowering), AddressSanitizer instrumentation and the env-flag
//! readers for debug metadata, auto-par, contracts and error traces. The
//! target-machine setup and the linker moved to `crate::backend::driver`
//! (B-2026-10-22-24), which the MIR backend uses without legacy codegen; they
//! are re-exported here so `super::driver::*` paths keep resolving.

pub(crate) use crate::backend::driver::*;

use inkwell::module::Module;
use inkwell::targets::TargetMachine;

/// Run LLVM mid-end optimization passes on the constructed module.
///
/// karac builds raw IR with locals as alloca'd stack slots (the natural
/// shape produced by a non-SSA-aware codegen). Without this pass run,
/// `mem2reg` + downstream optimizations never fire and tight integer
/// kernels (e.g., the Parallax bench's `busy_loop`) emit ~12 instructions
/// per iter through a stack-spill chain instead of the 4-instruction
/// register-only inner loop LLVM-O2 produces. See
/// `docs/investigations/parallax_perf.md § Findings, 2026-05-10` for the
/// before/after disasm + bench numbers that motivated wiring this up.
///
/// Pass string is the LLVM new-pass-manager pipeline alias `default<O…>` —
/// the same pipeline `clang -O…` runs. `KARAC_OPT_LEVEL=0` short-circuits
/// the pass run entirely (debugging fallback for "did the optimizer eat
/// my IR construct?"). Other levels run the matching `default<O…>`
/// pipeline; size tiers (`s`, `z`) currently fold into `-O2` (see
/// `read_opt_level_env` table).
pub(super) fn apply_optimization_passes(
    module: &Module<'_>,
    target_machine: &TargetMachine,
    binsearch_reopt: bool,
) -> Result<(), String> {
    // Coroutine lowering is a CORRECTNESS pass, not an optimization: a
    // function marked `presplitcoroutine` (the A2 network-async transform —
    // see `docs/spikes/network-async-coroutine-transform.md`) is NOT a
    // valid runnable function until CoroSplit rewrites it into the
    // ramp/resume/destroy clones. So the coro pipeline must run at EVERY
    // opt level, including `-O0` / `KARAC_OPT_LEVEL=0` — otherwise a debug
    // build would silently emit un-split coroutines (a no-op task, exactly
    // the bug C class). For non-coroutine modules (everything today) this is
    // a pure no-op: the coro passes only touch `presplitcoroutine` funcs.
    {
        let coro_opts = inkwell::passes::PassBuilderOptions::create();
        // SAFETY: `module` is a live inkwell `Module` for the whole call.
        let coro_passes = unsafe { super::coro::coro_lowering_pipeline(module.as_mut_ptr()) };
        module
            .run_passes(coro_passes, target_machine, coro_opts)
            .map_err(|e| {
                format!(
                    "LLVM coroutine lowering passes failed: {}. \
                     This is a correctness pass (CoroSplit) — it cannot be \
                     skipped for `presplitcoroutine` functions.",
                    e.to_string()
                )
            })?;
    }

    let level = read_opt_level_env();
    if level != "0" {
        let pipeline = match level {
            "1" => "default<O1>",
            "3" => "default<O3>",
            _ => "default<O2>",
        };
        let options = inkwell::passes::PassBuilderOptions::create();
        module
            .run_passes(pipeline, target_machine, options)
            .map_err(|e| {
                format!(
                    "LLVM optimization pass `{}` failed: {}. \
                     Workaround: set KARAC_OPT_LEVEL=0 to skip the pass run.",
                    pipeline,
                    e.to_string()
                )
            })?;

        // Binary-search midpoint BCE (control_flow_bce.rs § midpoint): the
        // `assume(lo <= mid < hi)` facts only fold the `nums[mid]` bounds
        // check once they are co-resident with it post-inline, which the
        // single pipeline above doesn't reach (callee-optimize-then-inline
        // phase ordering). A second pipeline run completes the fold. Gated on
        // emission, so non-binary-search modules never pay it; `default<O1>`
        // suffices (verified) and is cheaper than re-running the full level.
        if binsearch_reopt {
            let opts2 = inkwell::passes::PassBuilderOptions::create();
            module
                .run_passes("default<O1>", target_machine, opts2)
                .map_err(|e| format!("LLVM binary-search re-optimization pass failed: {e}."))?;
        }

        // B-2026-08-30-32 — the f16 rounding guard runs LAST, after every pass
        // that could have undone it. See the function's own doc-comment for why
        // this one is placed after optimization where its bf16 sibling
        // (`verify_no_native_bf16_ops`) runs before it.
        if crate::target::active_target_is_wasm() {
            verify_no_native_f16_arith(module)?;
        }
    }

    // B-2026-09-07-40 — AddressSanitizer INSTRUMENTATION, opt-in and last.
    // Runs at every optimization level, `-O0` included: the whole point of the
    // knob is the `-O0` leg, where an allocation the optimizer would have
    // folded away is still real. Placed after the pipeline because that is
    // where clang places it (`registerOptimizerLastEPCallback`), so an
    // instrumented `-O2` build checks the accesses that actually survived
    // optimization rather than the ones codegen happened to emit.
    apply_address_sanitizer(module, target_machine)?;
    Ok(())
}

/// Instrument the module with **AddressSanitizer's access checks** when
/// `KARAC_SANITIZE_ADDRESS` is set (B-2026-09-07-40). Off by default: an
/// instrumented build is a different binary, materially bigger and slower, and
/// nothing but a sanitizer leg wants one.
///
/// WHY THIS EXISTS AT ALL. `tests/memory_sanitizer.rs` links its programs with
/// `-fsanitize=address`, and that flag at the LINK step buys only the ASAN
/// **runtime** — the allocator interposition, which is what catches leaks,
/// double frees and invalid frees. ASAN's memory-ACCESS checking is a compiler
/// pass, so an object nobody instrumented carries no shadow-memory checks and a
/// program that reads or writes freed memory runs clean. Measured on
/// B-2026-09-07-33's callee, which writes three zero words into an envelope it
/// just freed: ASAN reported a leak and nothing else, while valgrind reported
/// three `Invalid write of size 8`. So the suite named `memory_sanitizer` was
/// blind to the entire use-after-free and buffer-overflow class, and every row
/// in the ledger whose evidence is an invalid read or write was unpinnable.
///
/// TWO HALVES, BOTH REQUIRED. Running the `asan` pass is not enough on its own:
/// `AddressSanitizer::instrumentFunction` returns early for any function
/// without the `sanitize_address` attribute, which clang stamps on everything
/// it compiles under `-fsanitize=address` and karac stamps nowhere. Measured
/// directly on a two-function module under `opt -passes=asan`: the attributed
/// function got its `__asan_report_load8`, the bare one was returned untouched.
/// Stamping happens here, over `module.get_functions()`, rather than at the ~395
/// `add_function` call sites — one central place that cannot be forgotten by
/// the next site to be added.
///
/// Declarations are skipped (`count_basic_blocks() == 0`): the attribute is
/// meaningless without a body, and the runtime archive this module links
/// against is not instrumented either. That mix is fine and is the normal ASAN
/// arrangement — uninstrumented code simply is not access-checked, while the
/// allocator interposition it shares still sees every `malloc`/`free`.
pub(super) fn apply_address_sanitizer(
    module: &Module<'_>,
    target_machine: &TargetMachine,
) -> Result<(), String> {
    if !read_sanitize_address_env() {
        return Ok(());
    }

    let kind_id = inkwell::attributes::Attribute::get_named_enum_kind_id("sanitize_address");
    if kind_id == 0 {
        return Err(
            "KARAC_SANITIZE_ADDRESS is set, but this LLVM build does not know the \
                    `sanitize_address` function attribute, so the `asan` pass would return \
                    every function untouched and the instrumentation would be silently absent."
                .to_string(),
        );
    }
    let attr = module.get_context().create_enum_attribute(kind_id, 0);

    let mut bodies = 0usize;
    for func in module.get_functions() {
        if func.count_basic_blocks() > 0 {
            func.add_attribute(inkwell::attributes::AttributeLoc::Function, attr);
            bodies += 1;
        }
    }
    if bodies == 0 {
        // A declarations-only module (nothing to check). Running the pass would
        // still emit the module ctor and the `__asan_*` declarations, which is
        // pure cost.
        return Ok(());
    }

    let opts = inkwell::passes::PassBuilderOptions::create();
    module
        .run_passes("asan", target_machine, opts)
        .map_err(|e| {
            format!(
                "LLVM AddressSanitizer pass failed: {}. Unset KARAC_SANITIZE_ADDRESS to \
                 compile without instrumentation.",
                e.to_string()
            )
        })
}

/// Read the `KARAC_SANITIZE_ADDRESS` env var: absent or `0` leaves the module
/// uninstrumented (the default for every build), any other value turns
/// [`apply_address_sanitizer`] on.
///
/// Deliberately UNCACHED, unlike [`read_opt_level_env`]. That one is cached so
/// repeated `karac build` calls inside one process agree on a level; here the
/// value is read once per compile and the knob is set for a whole leg, so a
/// cache would buy nothing and would make the test that toggles it lie.
pub(super) fn read_sanitize_address_env() -> bool {
    !matches!(
        std::env::var("KARAC_SANITIZE_ADDRESS").as_deref(),
        Err(_) | Ok("0") | Ok("")
    )
}

/// Reject any native `half` ARITHMETIC instruction surviving into the backend
/// on a target that would promote it to `f32` and never round back
/// (B-2026-08-30-32). wasm32 only — every other target LLVM 18 supports
/// legalizes `half` with `SoftPromoteHalf`, which rounds after each operation.
///
/// WHY THIS RUNS AFTER OPTIMIZATION, where the bf16 guard runs before it.
/// `verify_no_native_bf16_ops` checks what CODEGEN EMITTED, and that is the
/// right question for bf16: the ops it forbids are unselectable, so LLVM has
/// no transform that could introduce one. f16 is the opposite case. The
/// widened form codegen emits here is *semantically equal* to a native `half`
/// op, so InstCombine is entitled to shrink it back — and at `default<O2>` it
/// does, for all five arithmetic operators, which is precisely why
/// `emit_f32_to_f16_opaque` rounds through a call instead of an `fptrunc`. A
/// check that ran before the pipeline would therefore pass on IR the pipeline
/// then breaks. Running it here asks the only question that matters: is the
/// module the BACKEND receives free of unrounded f16 arithmetic?
///
/// So this guard covers two failure modes, not one: a codegen site that
/// forgets to widen, and a future LLVM that learns to see through the call.
/// The second is not hypothetical — it is one `TargetLibraryInfo` entry away.
///
/// TWO PASSES, because an f16 op reaches the backend in two spellings: a
/// plain `fmul half`, and an intrinsic like `llvm.sqrt.f16` that an opcode
/// sweep does not see. bf16 was immune to the second by accident —
/// `verify_no_native_bf16_ops` matches intrinsics by name and every
/// `llvm.*.bf16` contains "bf16" — while nothing matched `llvm.sqrt.f16`.
///
/// Comparisons and `fneg` are deliberately absent from the opcode list. They
/// are exact: `fneg` flips a sign bit and a compare produces an `i1`, so
/// neither can carry an unrepresentable value, and the wasm backend normalizes
/// each operand through f16 before comparing it (measured).
fn verify_no_native_f16_arith(module: &Module<'_>) -> Result<(), String> {
    use inkwell::types::BasicTypeEnum;
    use inkwell::values::InstructionOpcode as Op;

    let ctx = module.get_context();
    let f16_t: BasicTypeEnum<'_> = ctx.f16_type().into();
    let is_f16 = |t: BasicTypeEnum<'_>| {
        t == f16_t || (t.is_vector_type() && t.into_vector_type().get_element_type() == f16_t)
    };

    // PASS 1 — INEXACT f16 INTRINSICS, found by NAME at the module level.
    //
    // An intrinsic is a native f16 op wearing a call's clothing, and it is the
    // half that is easiest to miss: `llvm.sqrt.f16` looks like a call, not
    // like an `fmul`, so an opcode sweep walks straight past it. It cost
    // `65504f16.sqrt()` its correct answer (255.875) in favour of the
    // unrounded f32 255.93748474… on wasm.
    //
    // ALLOW-LIST, not a deny-list, so a newly-emitted inexact intrinsic fails
    // closed rather than slipping through. The permitted ones return one of
    // their operands or move a sign bit, so they cannot produce a value
    // outside the f16 grid no matter how the backend legalizes them.
    // Declarations are read by NAME only — never by touching a call's
    // operands, which is what keeps this safe on coroutine intrinsics (the
    // hazard `verify_no_native_bf16_ops`'s first pass documents).
    const EXACT_AT_ANY_WIDTH: &[&str] = &[
        ".fabs.",
        ".minnum.",
        ".maxnum.",
        ".minimum.",
        ".maximum.",
        ".copysign.",
        ".fmin.",
        ".fmax.",
    ];
    let mut f = module.get_first_function();
    while let Some(fv) = f {
        let name = fv.get_name().to_string_lossy().into_owned();
        if name.starts_with("llvm.")
            && name.contains("f16")
            && !name.contains("bf16")
            && !EXACT_AT_ANY_WIDTH.iter().any(|ex| name.contains(ex))
        {
            return Err(format!(
                "internal error: the inexact f16 intrinsic `{name}` survived \
                 into the wasm backend. LLVM 18 legalizes wasm32 `half` with \
                 `PromoteFloat`, which computes at `f32` and does not round \
                 back, so its result would not be representable as an `f16` \
                 (`65504f16.sqrt()` yields 255.93748474… where the f16 answer \
                 is 255.875). Route the call through \
                 `build_float_intrinsic_bf16_safe`, which widens the operands \
                 and rounds the result. If the intrinsic really is exact at \
                 every width — it returns one of its operands, or only moves a \
                 sign bit — add it to `EXACT_AT_ANY_WIDTH` instead."
            ));
        }
        f = fv.get_next_function();
    }

    // PASS 2 — plain arithmetic. These carry only float operands and a float
    // result, so the type API is safe on them; `Call` is deliberately not in
    // the opcode list (pass 1 covers it).
    let mut f = module.get_first_function();
    while let Some(fv) = f {
        for bb in fv.get_basic_blocks() {
            let mut i = bb.get_first_instruction();
            while let Some(inst) = i {
                if matches!(
                    inst.get_opcode(),
                    Op::FAdd | Op::FSub | Op::FMul | Op::FDiv | Op::FRem
                ) && BasicTypeEnum::try_from(inst.get_type()).is_ok_and(is_f16)
                {
                    return Err(format!(
                        "internal error: a native `half` {:?} survived into the \
                         wasm backend in `{}`. LLVM 18 legalizes wasm32 `half` \
                         with `PromoteFloat`, which computes in `f32` and does \
                         NOT round the result back, so the value would not be \
                         representable as an `f16` (`65504f16 * 3f16` yields a \
                         finite 196512). f16 arithmetic must be widened to \
                         `f32` and rounded through `emit_f32_to_f16_opaque` — \
                         see `f16_arith_needs_explicit_rounding`. If codegen \
                         does emit the widened form, the optimizer shrank it \
                         back: the rounding call is what blocks that fold, so \
                         check it was not replaced by an `fptrunc`.",
                        inst.get_opcode(),
                        fv.get_name().to_string_lossy(),
                    ));
                }
                i = inst.get_next_instruction();
            }
        }
        f = fv.get_next_function();
    }
    Ok(())
}

thread_local! {
    /// Per-thread pin for the runtime-debug-metadata gate, consulted by
    /// [`read_runtime_debug_metadata_env`] ahead of the env var. Set through
    /// [`pin_runtime_debug_metadata`]; `None` means "read the env var".
    static RUNTIME_DEBUG_METADATA_PIN: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Pin the runtime-debug-metadata gate for every compile on THIS THREAD until
/// the returned guard drops — the race-free counterpart to setting
/// `KARAC_RUNTIME_DEBUG_METADATA`, in the same spirit as
/// [`crate::codegen::compile_to_ir_with_contracts_stripped`] and
/// [`crate::codegen::compile_to_ir_with_debug_info`].
///
/// B-2026-08-20-26: `KARAC_RUNTIME_DEBUG_METADATA` is read afresh at every
/// `Codegen::new`, and the env is PROCESS-global. The three sibling gates
/// (`KARAC_STRIP_CONTRACTS`, `KARAC_DEBUG_INFO`, `KARAC_STRIP_ERROR_TRACE`)
/// each grew an explicit override precisely so a caller that needs one value
/// does not impose it on everything else compiling at that moment; this gate
/// was the one left without. Its tests set and unset the process env instead,
/// under a mutex shared only with their own peers — so every OTHER test
/// compiling concurrently in the same binary saw the gate flip underneath it,
/// and the codegen suite's determinism test
/// (`test_ir_shared_variant_name_resolves_to_scrutinee_enum_deterministically`)
/// failed ~6 of 8 full-suite runs on one line of IR:
/// `@KARAC_SPAWN_SITES_ENABLED`. Per-thread rather than per-call because the
/// gate has to reach three compile lanes (IR, object, JIT) whose entry points
/// already carry 6–9 positional parameters.
///
/// Nested pins restore the enclosing value on drop, so a guard can be taken
/// around a block that itself pins.
pub fn pin_runtime_debug_metadata(enabled: bool) -> RuntimeDebugMetadataPin {
    let prior = RUNTIME_DEBUG_METADATA_PIN.with(|c| c.replace(Some(enabled)));
    RuntimeDebugMetadataPin { prior }
}

/// RAII restore token from [`pin_runtime_debug_metadata`].
#[must_use = "the pin lifts as soon as the guard drops"]
pub struct RuntimeDebugMetadataPin {
    prior: Option<bool>,
}

impl Drop for RuntimeDebugMetadataPin {
    fn drop(&mut self) {
        let prior = self.prior;
        RUNTIME_DEBUG_METADATA_PIN.with(|c| c.set(prior));
    }
}

/// Decide whether `KARAC_SPAWN_SITES` (and friends) emit populated. Slice 3
/// of the Debugger Contract; see `Codegen::runtime_debug_metadata_enabled`
/// for the field doc and `phase-8-stdlib-floor.md` § "Auto-Concurrency
/// Codegen — Debugger Contract slice 3" for the spec.
///
/// A thread-local [`pin_runtime_debug_metadata`] wins when set; otherwise the
/// `KARAC_RUNTIME_DEBUG_METADATA` env var decides:
///
/// - `Ok("0")` → `false` (gate explicitly off).
/// - `Ok(_)`   → `true` (any other value, including empty).
/// - `Err(_)`  → `true` (dev default; profile-aware defaults land in
///   Phase 8.5 Track 2).
pub(super) fn read_runtime_debug_metadata_env() -> bool {
    if let Some(pinned) = RUNTIME_DEBUG_METADATA_PIN.with(|c| c.get()) {
        return pinned;
    }
    !matches!(std::env::var("KARAC_RUNTIME_DEBUG_METADATA"), Ok(v) if v == "0")
}

/// Read the `KARAC_AUTO_PAR` env var to decide whether
/// `compile_function_body` dispatches non-trivial parallel groups to
/// `emit_par_run` (auto-par codegen) or falls back to plain sequential
/// `compile_block`. Slice 6 of the Auto-Concurrency Codegen track;
/// mirrors slice 3's `read_runtime_debug_metadata_env` shape exactly.
/// See `phase-8-stdlib-floor.md` § "Auto-Concurrency Codegen —
/// Parallax-lite Workload" for the spec.
///
/// - `Ok("0")` → `false` (gate explicitly off — sequential codegen).
/// - `Ok(_)`   → `true` (any other value, including empty).
/// - `Err(_)`  → `false` (default OFF since the 2026-10-06 v2 redesign; see
///   `par_cost::auto_par_enabled`).
///
/// Returns `true` iff auto-par dispatch is enabled. The
/// `Codegen::auto_par_disabled` field is `!return_value` so the
/// compile-time check reads naturally as `if self.conc.auto_par_disabled`.
pub(super) fn read_auto_par_env() -> bool {
    // Delegates so codegen and `karac query concurrency` cannot disagree about
    // whether auto-par is on at all (B-2026-08-05-13).
    crate::par_cost::auto_par_enabled()
}

/// Read the `KARAC_STRIP_CONTRACTS` env var to decide whether contract
/// machinery (`requires` / `ensures` / `old(...)` capture / struct & impl
/// `invariant` checks) is emitted into AOT binaries. design.md § Contracts:
/// contracts are "checked at runtime in debug builds, stripped in release" —
/// so a release build elides every contract assert, paying zero runtime cost
/// (including the `old(...)` pre-state clone). This is the codegen trigger
/// for that release behavior; a future `karac build --release` CLI flag sets
/// the same env var (mirrors how `KARAC_OPT_LEVEL` / `KARAC_AUTO_PAR` are
/// env-driven build knobs read fresh at `Codegen` construction).
///
/// - `Ok("1")` / `Ok("true")` → `true` (release — strip all contracts).
/// - anything else (incl. unset) → `false` (debug default — contracts active).
///
/// Returns `true` iff contracts should be stripped. Read fresh per `Codegen`
/// (no `OnceLock` cache) so a process compiling many programs can vary it,
/// and stored in `Codegen::strip_contracts`.
pub(super) fn read_strip_contracts_env() -> bool {
    matches!(std::env::var("KARAC_STRIP_CONTRACTS"), Ok(v) if v == "1" || v == "true")
}

/// Release trigger for the `?`-error-return-trace instrumentation (`KARAC_STRIP
/// _ERROR_TRACE`). When set, codegen emits no `karac_error_trace_push` at `?`
/// failure sites and no `karac_error_trace_clear` on the success path — the
/// trace is a debug-only diagnostic (mirrors `strip_contracts`), so a release
/// build pays zero `?`-site instrumentation cost. `karac build --release` forces
/// this on alongside contract stripping; the env var is the per-construction
/// knob the `release` flag and `Codegen::new` default compose with (OR).
///
/// - `Ok("1")` / `Ok("true")` → `true` (strip the trace).
/// - anything else (incl. unset) → `false` (debug default — trace active).
pub(super) fn read_strip_error_trace_env() -> bool {
    matches!(std::env::var("KARAC_STRIP_ERROR_TRACE"), Ok(v) if v == "1" || v == "true")
}
