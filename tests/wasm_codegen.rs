//! WASM-target IR pins (phase-10 "WASM concurrency lowering —
//! sequential default").
//!
//! **Why a dedicated test binary.** `target::set_active_target` is a
//! process-global (one artifact per invocation); flipping it inside a
//! shared test binary would race every parallel codegen test that
//! assumes the native default (the same hazard `wasm_wasi_host_fn_e2e`'s
//! doc-comment records — its import-entry assertions live in a CLI
//! subprocess for this exact reason). This binary sets `wasm_wasi` from
//! every test, so intra-binary parallelism is safe — all writers store
//! the same value. **Do not add native-target IR tests to this file.**

#[cfg(feature = "llvm")]
mod wasm_codegen_tests {
    use karac::codegen::compile_to_ir_with_options;

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
}
