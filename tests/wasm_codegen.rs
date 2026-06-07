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
}
