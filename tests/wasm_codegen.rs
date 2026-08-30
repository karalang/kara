//! WASM-target IR pins (phase-10 "WASM concurrency lowering —
//! sequential default" + the "`--features wasm-threads` opt-in"'s
//! threaded pass).
//!
//! **Why a dedicated test binary.** `target::set_active_target` is a
//! process-global (one artifact per invocation); flipping it inside a
//! shared test binary would race every parallel codegen test that
//! assumes the native default (the same hazard `wasm_wasi_host_fn_e2e`'s
//! doc-comment records — its import-entry assertions live in a CLI
//! subprocess for this exact reason). This binary sets `wasm_wasi` from
//! every test, so intra-binary parallelism is safe — all writers store
//! the same value. **Do not add native-target IR tests to this file.**
//! The threaded-pass pins are safe in this same binary because the
//! threaded-pass selection is parameter-passed (a `Codegen` setter via
//! `compile_to_ir_wasm_threaded`), never another process-global.

#[cfg(feature = "llvm")]
mod wasm_codegen_tests {
    use karac::codegen::{compile_to_ir_wasm_threaded, compile_to_ir_with_options};

    /// Pin this process to the wasm_wasi target, then run the same
    /// pipeline shape as `par_codegen.rs::ir_for_with_concurrency`
    /// (resolve → typecheck → lower → effectcheck → concurrency_analyze
    /// → codegen) and return the emitted IR.
    fn wasm_ir_for_with_concurrency(src: &str) -> String {
        karac::target::set_active_target("wasm_wasi").expect("wasm_wasi is a valid v1 target");
        let mut parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        karac::prepare_for_resolve(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        compile_to_ir_with_options(&parsed.program, None, Some(&analysis), None, None)
            .expect("codegen failed")
    }

    /// Same pipeline shape as [`wasm_ir_for_with_concurrency`], emitted
    /// through the **threaded pass** (`compile_to_ir_wasm_threaded` —
    /// the second pass of a `--features wasm-threads` dual-artifact
    /// build). Still stores the same `wasm_wasi` value to the
    /// process-global active target as every other test in this binary
    /// (the threaded/sequential split is the parameter-passed setter,
    /// not the target name — the CLI's browser-only flag scoping is a
    /// CLI-layer rule, orthogonal to the IR shape pinned here).
    fn wasm_threaded_ir_for_with_concurrency(src: &str) -> String {
        karac::target::set_active_target("wasm_wasi").expect("wasm_wasi is a valid v1 target");
        let mut parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        compile_to_ir_wasm_threaded(&parsed.program, None, Some(&analysis))
            .expect("threaded codegen failed")
    }

    /// The auto-par fixture that emits exactly one `karac_par_run`
    /// fan-out on native (`par_codegen.rs::
    /// test_auto_par_three_independent_reads_emits_par_run`) must emit
    /// NONE on a wasm target: auto-par fan-out is pure overhead on a
    /// single-threaded target, so `Codegen::auto_par_disabled` is forced
    /// on and the statements compile sequentially.
    #[test]
    fn wasm_target_skips_auto_par_fan_out() {
        let ir = wasm_ir_for_with_concurrency(
            r#"
effect resource Net;
effect resource Disk;
effect resource Db;

fn fetch_net() -> i64 reads(Net) { 1 }
fn fetch_disk() -> i64 reads(Disk) { 2 }
fn fetch_db() -> i64 reads(Db) { 3 }

fn main() {
    let _ = fetch_net();
    let _ = fetch_disk();
    let _ = fetch_db();
}
"#,
        );
        assert_eq!(
            ir.matches("call void @karac_par_run").count(),
            0,
            "wasm targets must not emit auto-par dispatch; IR:\n{ir}"
        );
        assert!(
            !ir.contains("__par_branch_"),
            "no branch fns may be synthesized for an auto-par group on wasm; IR:\n{ir}"
        );
        // The statements still compile — sequentially, in the plain
        // `compile_block` path.
        for callee in ["@fetch_net", "@fetch_disk", "@fetch_db"] {
            assert!(
                ir.contains(&format!("call i64 {callee}")),
                "sequential call to {callee} missing; IR:\n{ir}"
            );
        }
    }

    /// Explicit `par {{}}` is NOT gated: it still lowers through
    /// `karac_par_run` on wasm so the block's cancellation/result-slot
    /// semantics are preserved — the *runtime* archive's sequential
    /// `karac_par_run` body (`seq_par_run`) supplies the in-order
    /// execution on this target.
    #[test]
    fn wasm_target_keeps_explicit_par_block_lowering() {
        let ir = wasm_ir_for_with_concurrency(
            r#"
fn main() {
    par {
        println(100);
        println(200);
    }
}
"#,
        );
        assert_eq!(
            ir.matches("call void @karac_par_run").count(),
            1,
            "explicit par {{}} must still dispatch through karac_par_run on wasm; IR:\n{ir}"
        );
        assert!(
            ir.contains("__par_branch_0_0") && ir.contains("__par_branch_0_1"),
            "explicit par branch fns must be synthesized on wasm; IR:\n{ir}"
        );
    }

    /// The threaded pass of a `--features wasm-threads` build re-enables
    /// auto-par (phase-10 wasm-threads entry): the SAME fixture that
    /// must emit zero fan-outs on sequential wasm (the first test above)
    /// must emit exactly one `karac_par_run` + its synthesized branch
    /// fns through `compile_to_ir_wasm_threaded` — the threaded module
    /// has a real worker pool, so the fan-out pays off there. Also pins
    /// the threaded module's triple (the wasip1-threads machine is what
    /// makes the emitted object carry `+atomics` — without it wasm-ld
    /// rejects the `--shared-memory` link).
    #[test]
    fn wasm_threaded_pass_emits_auto_par_fan_out() {
        let ir = wasm_threaded_ir_for_with_concurrency(
            r#"
effect resource Net;
effect resource Disk;
effect resource Db;

fn fetch_net() -> i64 reads(Net) { 1 }
fn fetch_disk() -> i64 reads(Disk) { 2 }
fn fetch_db() -> i64 reads(Db) { 3 }

fn main() {
    let _ = fetch_net();
    let _ = fetch_disk();
    let _ = fetch_db();
}
"#,
        );
        assert_eq!(
            ir.matches("call void @karac_par_run").count(),
            1,
            "the threaded pass must re-enable the auto-par fan-out; IR:\n{ir}"
        );
        assert!(
            ir.contains("__par_branch_"),
            "auto-par branch fns must be synthesized on the threaded pass; IR:\n{ir}"
        );
        assert!(
            ir.contains("target triple = \"wasm32-wasip1-threads\""),
            "the threaded pass must emit for the wasip1-threads triple; IR:\n{ir}"
        );
    }

    /// f16 ARITHMETIC must be widened to `f32` and rounded back through an
    /// explicit `__truncsfhf2` call on wasm — never left as a native `half`
    /// binop for the backend to legalize (B-2026-08-30-32).
    ///
    /// LLVM 18 legalizes wasm32 `half` with `PromoteFloat`, which computes in
    /// `f32` and does not round the result back, so a native `fmul half`
    /// leaves a value the type cannot hold: `65504f16 * 3f16` came out as a
    /// finite 196512 where the largest finite f16 is 65504 and the answer must
    /// be `inf`.
    ///
    /// WHY THE ROUNDING IS A CALL AND NOT AN `fptrunc`, which is what makes
    /// this pin worth having: an `fptrunc (fadd (fpext a) (fpext b))` is
    /// semantically identical to `fadd half`, and InstCombine shrinks it back
    /// to exactly that whenever the wide type is exact for the narrow one —
    /// which `f32` is for `f16` (24 significand bits ≥ 2·11 + 2). Measured at
    /// `default<O2>`, the pipeline karac runs: all five of add/sub/mul/div/rem
    /// fold. So a fix that emitted the obvious spelling would be undone before
    /// the backend saw it, and would still pass a value test written against
    /// an un-optimized module. The call is what survives, and this test pins
    /// the call rather than the widening for that reason.
    #[test]
    fn wasm_f16_arithmetic_rounds_through_an_unfoldable_call() {
        let ir = wasm_ir_for_with_concurrency(
            r#"
fn main() {
    let n = env.args().len() as i64;
    let one: f32 = n as f32;
    let a: f16 = (one * 65504.0f32) as f16;
    let b: f16 = (one * 3.0f32) as f16;
    println(f"{a + b} {a - b} {a * b} {a / b}");
}
"#,
        );
        // No native `half` arithmetic may reach the backend.
        for native in ["fadd half", "fsub half", "fmul half", "fdiv half"] {
            assert!(
                !ir.contains(native),
                "`{native}` must not survive on wasm — LLVM promotes it to f32 \
                 and never rounds back; IR:\n{ir}"
            );
        }
        // Each of the four ops rounds through its own call.
        assert_eq!(
            ir.matches("call i32 @__truncsfhf2").count(),
            4,
            "each f16 arithmetic op must round through `__truncsfhf2`; IR:\n{ir}"
        );
        // The arithmetic itself happens at f32.
        assert!(
            ir.contains("fadd float") && ir.contains("fdiv float"),
            "f16 arithmetic must be computed at f32 on wasm; IR:\n{ir}"
        );
    }

    /// The MATH INTRINSICS need the same widening, and are the half an opcode
    /// sweep walks past: `llvm.sqrt.f16` looks like a call, not like an
    /// `fmul`, so it survived the first pass of this fix. Measured,
    /// `65504f16.sqrt()` returned the unrounded f32 255.93748474… on wasm
    /// where both other backends give 255.875, and 9 of 11 printed lines
    /// across the fourteen f16 math methods differed from native.
    ///
    /// The two that AGREED are exactly the exact-at-any-width families —
    /// `floor`/`ceil`/`round`/`trunc` and `copysign` — which is the
    /// prediction the guard's allow-list rests on, so it is pinned here.
    #[test]
    fn wasm_f16_math_intrinsics_are_widened_and_rounded() {
        let ir = wasm_ir_for_with_concurrency(
            r#"
fn main() {
    let n = env.args().len() as i64;
    let one: f32 = n as f32;
    let a: f16 = (one * 65504.0f32) as f16;
    let b: f16 = (one * 3.7f32) as f16;
    println(f"{a.sqrt()} {a.exp()} {a.ln()} {b.floor()} {b.copysign(a)}");
}
"#,
        );
        // No INEXACT f16 intrinsic may reach the backend.
        for native in [
            "llvm.sqrt.f16",
            "llvm.exp.f16",
            "llvm.log.f16",
            "llvm.floor.f16",
            "llvm.copysign.f16",
        ] {
            assert!(
                !ir.contains(native),
                "`{native}` must not survive on wasm — LLVM promotes it to f32 \
                 and never rounds back; IR:\n{ir}"
            );
        }
        // They are called at f32 instead, and each rounds back through the
        // same unfoldable call the arithmetic path uses.
        assert!(
            ir.contains("llvm.sqrt.f32") && ir.contains("llvm.exp.f32"),
            "f16 math must be computed at f32 on wasm; IR:\n{ir}"
        );
        assert_eq!(
            ir.matches("call i32 @__truncsfhf2").count(),
            5,
            "one rounding call per math method; IR:\n{ir}"
        );
    }

    /// The `<N x half>` lane form needs the same treatment for the same
    /// reason: the vector legalizer scalarizes the lane op into the scalar
    /// `half` nodes whose legalization is the lossy one, so `Vector[f16, 4]`
    /// multiplication carried the identical unrounded result.
    ///
    /// Only the ROUNDING is per-lane — the arithmetic stays a `<4 x float>`
    /// vector op, which wasm does have — so the pin checks both halves.
    #[test]
    fn wasm_f16_vector_lane_arithmetic_rounds_per_lane() {
        let ir = wasm_ir_for_with_concurrency(
            r#"
fn main() {
    let n = env.args().len() as i64;
    let one: f32 = n as f32;
    let a: f16 = (one * 65504.0f32) as f16;
    let b: f16 = (one * 3.0f32) as f16;
    let u: Vector[f16, 4] = Vector[f16, 4](a, b, a, b);
    let v: Vector[f16, 4] = Vector[f16, 4](b, b, b, b);
    let w = u * v;
    println(f"{w[0]}");
}
"#,
        );
        assert!(
            !ir.contains("fmul <4 x half>"),
            "a native `<4 x half>` lane op must not survive on wasm; IR:\n{ir}"
        );
        assert!(
            ir.contains("fmul <4 x float>"),
            "the lane arithmetic itself must stay a vector op at f32; IR:\n{ir}"
        );
        assert_eq!(
            ir.matches("call i32 @__truncsfhf2").count(),
            4,
            "one rounding call per lane of the 4-lane result; IR:\n{ir}"
        );
    }
}
